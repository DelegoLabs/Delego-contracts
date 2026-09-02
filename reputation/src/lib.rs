//! Delego Reputation Contract
//!
//! Tracks on-chain reputation scores for participants (merchants and agents)
//! in the Delego protocol.
//!
//! # Error range
//! `ReputationError` discriminants occupy `4000..=4999` (per the workspace-wide
//! error-code allocation table in `docs/architecture/contracts.md`).
//!
//! # Event topic schema
//! Entity-scoped events: `(reptn, <action>, entity)`.
//! Contract-wide events (no single entity): `(reptn, <action>)`.

// Contract crates compile as no_std for release and wasm builds, but keep std
// enabled during testing so dev-dependencies and test assertions operate normally.
// This exact conditional form must be consistent across all workspace contract crates.
#![cfg_attr(not(test), no_std)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env, Vec,
};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Maximum number of historical transaction records retained per entity.
const SCORE_WINDOW: u32 = 200;
/// Maximum ledger-budget for a single `prune_entity_history` call.
const MAX_PRUNE_BATCH: u32 = 50;
/// Minimum allowed rating (inclusive).
const MIN_RATING: u32 = 1;
/// Maximum allowed rating (inclusive).
const MAX_RATING: u32 = 5;
/// Approximate ledgers per day (5-second average ledger time).
const LEDGERS_PER_DAY: u32 = 17_280;
/// TTL extension for persistent entries (~30 days).
const PERSISTENT_TTL_LEDGERS: u32 = LEDGERS_PER_DAY * 30;

// ─── Types ────────────────────────────────────────────────────────────────────

/// A single recorded transaction with its rating.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionRecord {
    /// Amount of the transaction (informational; not used in scoring).
    pub amount: i128,
    /// Rating given for this transaction (1–5).
    pub rating: u32,
    /// Ledger timestamp when this record was created.
    pub recorded_at: u64,
}

/// Aggregated reputation score for an entity.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationScore {
    /// Entity this score belongs to.
    pub entity: Address,
    /// Sum of all ratings within the scoring window.
    pub total_rating: u64,
    /// Number of transactions in the scoring window.
    pub transaction_count: u64,
    /// Integer average rating (0 if no transactions).
    pub average_rating: u32,
    /// Ledger timestamp of the last transaction.
    pub last_updated: u64,
}

// ─── Events ───────────────────────────────────────────────────────────────────

/// Emitted when a transaction is recorded for an entity.
#[contracttype]
#[derive(Clone, Debug)]
pub struct TransactionRecordedEvent {
    pub entity: Address,
    pub amount: i128,
    pub rating: u32,
    pub new_average: u32,
    pub transaction_count: u64,
    pub timestamp: u64,
}

/// Emitted when entity history is pruned.
#[contracttype]
#[derive(Clone, Debug)]
pub struct HistoryPrunedEvent {
    pub entity: Address,
    pub records_removed: u32,
    pub records_remaining: u32,
    pub timestamp: u64,
}

/// Emitted when the admin transfer is proposed.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AdminProposedEvent {
    pub current_admin: Address,
    pub proposed_admin: Address,
}

/// Emitted when the admin transfer is accepted.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AdminAcceptedEvent {
    pub new_admin: Address,
}

// ─── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    /// Instance: current admin.
    Admin,
    /// Instance: pending admin (two-step handover).
    PendingAdmin,
    /// Persistent per entity: transaction history Vec<TransactionRecord>.
    EntityHistory(Address),
    /// Persistent per entity: aggregated ReputationScore.
    EntityScore(Address),
}

// ─── Errors ───────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ReputationError {
    /// Contract has already been initialised.
    AlreadyInitialized = 4000,
    /// Contract has not been initialised yet.
    NotInitialized = 4001,
    /// The supplied rating is outside 1–5.
    InvalidRating = 4002,
    /// The supplied amount is not positive.
    InvalidAmount = 4003,
    /// No reputation record exists for the entity.
    NotFound = 4004,
    /// Caller is not the admin.
    Unauthorized = 4005,
    /// `max_records` exceeds the prune batch cap.
    BatchTooLarge = 4006,
    /// No pending admin proposal to accept.
    NoPendingAdmin = 4007,
    /// Caller is not the proposed admin.
    NotPendingAdmin = 4008,
}

// ─── Contract ─────────────────────────────────────────────────────────────────

#[contract]
pub struct ReputationContract;

#[contractimpl]
impl ReputationContract {
    // ── Initialisation ────────────────────────────────────────────────────────

    /// Initialise the contract with an admin address.
    pub fn initialize(env: Env, admin: Address) -> Result<(), ReputationError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(ReputationError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        Ok(())
    }

    // ── Core scoring ──────────────────────────────────────────────────────────

    /// Record a transaction and rating for `entity`.
    ///
    /// `rating` must be in [1, 5]. `amount` must be > 0.
    /// The history ring buffer is capped at `SCORE_WINDOW` records; the
    /// oldest entry is dropped once the window is full.
    pub fn record_transaction(
        env: Env,
        entity: Address,
        amount: i128,
        rating: u32,
    ) -> Result<(), ReputationError> {
        if !env.storage().instance().has(&DataKey::Admin) {
            return Err(ReputationError::NotInitialized);
        }
        if !(MIN_RATING..=MAX_RATING).contains(&rating) {
            return Err(ReputationError::InvalidRating);
        }
        if amount <= 0 {
            return Err(ReputationError::InvalidAmount);
        }

        let now = env.ledger().timestamp();

        let record = TransactionRecord {
            amount,
            rating,
            recorded_at: now,
        };

        // Load existing history.
        let history_key = DataKey::EntityHistory(entity.clone());
        let mut history: Vec<TransactionRecord> = env
            .storage()
            .persistent()
            .get(&history_key)
            .unwrap_or(Vec::new(&env));

        // Evict oldest entry if the window is full.
        while history.len() >= SCORE_WINDOW {
            history.remove(0);
        }
        history.push_back(record);

        env.storage().persistent().set(&history_key, &history);
        env.storage().persistent().extend_ttl(
            &history_key,
            PERSISTENT_TTL_LEDGERS,
            PERSISTENT_TTL_LEDGERS,
        );

        // Recompute aggregate score.
        let score = Self::compute_score(&env, &entity, &history, now);
        let new_average = score.average_rating;
        let transaction_count = score.transaction_count;
        let score_key = DataKey::EntityScore(entity.clone());
        env.storage().persistent().set(&score_key, &score);
        env.storage().persistent().extend_ttl(
            &score_key,
            PERSISTENT_TTL_LEDGERS,
            PERSISTENT_TTL_LEDGERS,
        );

        env.events().publish(
            (
                symbol_short!("reptn"),
                symbol_short!("tx_rec"),
                entity.clone(),
            ),
            TransactionRecordedEvent {
                entity,
                amount,
                rating,
                new_average,
                transaction_count,
                timestamp: now,
            },
        );

        Ok(())
    }

    /// Get the reputation score for `entity`.
    pub fn get_reputation(env: Env, entity: Address) -> Result<ReputationScore, ReputationError> {
        env.storage()
            .persistent()
            .get(&DataKey::EntityScore(entity))
            .ok_or(ReputationError::NotFound)
    }

    /// Get the raw transaction history for `entity`.
    pub fn get_history(env: Env, entity: Address) -> Vec<TransactionRecord> {
        env.storage()
            .persistent()
            .get(&DataKey::EntityHistory(entity))
            .unwrap_or(Vec::new(&env))
    }

    // ── Maintenance ───────────────────────────────────────────────────────────

    /// Admin-only: trim `entity`'s transaction history to at most `max_records`
    /// entries, removing from the oldest end.
    ///
    /// `max_records` must be ≤ `SCORE_WINDOW` (200) and ≤ `MAX_PRUNE_BATCH` (50)
    /// per prune call to bound gas consumption.
    pub fn prune_entity_history(
        env: Env,
        admin: Address,
        entity: Address,
        max_records: u32,
    ) -> Result<u32, ReputationError> {
        Self::require_admin(&env, &admin)?;

        if max_records > MAX_PRUNE_BATCH {
            return Err(ReputationError::BatchTooLarge);
        }

        let history_key = DataKey::EntityHistory(entity.clone());
        let mut history: Vec<TransactionRecord> = env
            .storage()
            .persistent()
            .get(&history_key)
            .unwrap_or(Vec::new(&env));

        let original_len = history.len();
        let mut removed = 0u32;

        // Remove from the oldest (front) while over the cap.
        while history.len() > max_records {
            history.remove(0);
            removed += 1;
        }

        if removed > 0 {
            env.storage().persistent().set(&history_key, &history);
            env.storage().persistent().extend_ttl(
                &history_key,
                PERSISTENT_TTL_LEDGERS,
                PERSISTENT_TTL_LEDGERS,
            );

            // Recompute score after prune.
            let now = env.ledger().timestamp();
            let score = Self::compute_score(&env, &entity, &history, now);
            let score_key = DataKey::EntityScore(entity.clone());
            env.storage().persistent().set(&score_key, &score);
            env.storage().persistent().extend_ttl(
                &score_key,
                PERSISTENT_TTL_LEDGERS,
                PERSISTENT_TTL_LEDGERS,
            );

            env.events().publish(
                (symbol_short!("reptn"), symbol_short!("pruned")),
                HistoryPrunedEvent {
                    entity,
                    records_removed: removed,
                    records_remaining: original_len - removed,
                    timestamp: now,
                },
            );
        }

        Ok(removed)
    }

    // ── Admin management (two-step) ───────────────────────────────────────────

    /// Current admin proposes a new admin. Returns `true` if a new proposal was
    /// set, `false` if the proposal was unchanged.
    pub fn propose_admin(
        env: Env,
        current_admin: Address,
        new_admin: Address,
    ) -> Result<bool, ReputationError> {
        Self::require_admin(&env, &current_admin)?;

        let existing: Option<Address> = env.storage().instance().get(&DataKey::PendingAdmin);
        if existing.as_ref() == Some(&new_admin) {
            return Ok(false);
        }

        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);

        env.events().publish(
            (symbol_short!("reptn"), symbol_short!("adm_prop")),
            AdminProposedEvent {
                current_admin,
                proposed_admin: new_admin,
            },
        );

        Ok(true)
    }

    /// Proposed admin accepts the transfer, completing it atomically.
    pub fn accept_admin(env: Env, caller: Address) -> Result<(), ReputationError> {
        caller.require_auth();

        let pending: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .ok_or(ReputationError::NoPendingAdmin)?;

        if pending != caller {
            return Err(ReputationError::NotPendingAdmin);
        }

        env.storage().instance().set(&DataKey::Admin, &pending);
        env.storage().instance().remove(&DataKey::PendingAdmin);

        env.events().publish(
            (symbol_short!("reptn"), symbol_short!("adm_acc")),
            AdminAcceptedEvent { new_admin: caller },
        );

        Ok(())
    }

    /// Returns the current admin address.
    pub fn get_admin(env: Env) -> Result<Address, ReputationError> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(ReputationError::NotInitialized)
    }

    // ── Version ───────────────────────────────────────────────────────────────

    /// Returns the contract version number.
    pub fn version(_env: Env) -> u32 {
        1
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn require_admin(env: &Env, caller: &Address) -> Result<(), ReputationError> {
        caller.require_auth();
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(ReputationError::NotInitialized)?;
        if &admin != caller {
            return Err(ReputationError::Unauthorized);
        }
        Ok(())
    }

    fn compute_score(
        _env: &Env,
        entity: &Address,
        history: &Vec<TransactionRecord>,
        now: u64,
    ) -> ReputationScore {
        let mut total_rating: u64 = 0;
        let transaction_count = history.len() as u64;

        for record in history.iter() {
            total_rating += record.rating as u64;
        }

        let average_rating = if transaction_count > 0 {
            (total_rating / transaction_count) as u32
        } else {
            0
        };

        ReputationScore {
            entity: entity.clone(),
            total_rating,
            transaction_count,
            average_rating,
            last_updated: now,
        }
    }
}

#[cfg(test)]
mod test;
