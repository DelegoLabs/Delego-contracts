//! Delego Reputation Contract
//!
//! Tracks time-decayed trust scores for merchants and agents on the Delego
//! platform, driven by escrow transaction outcomes and counterparty ratings.

// Contract crates compile as no_std for release and wasm builds, but keep std
// enabled during testing so dev-dependencies and test assertions operate normally.
// This exact conditional form must be consistent across all workspace contract crates.
#![cfg_attr(not(test), no_std)]
#![no_std]
#![warn(missing_docs)]
// Several entry points mirror escrow/permissions call shapes and exceed
// clippy's default 7-argument limit; restructuring them would break the
// published ABI these contracts are reviewed against.
#![allow(clippy::too_many_arguments)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env, String,
    Symbol, Vec,
};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationScore {
    pub entity: Address,
    /// 0-10000 basis points (0.00% to 100.00%). Masked to `0` by
    /// [`ReputationContract::get_reputation`] until `total_transactions`
    /// reaches `ReputationConfig::min_transactions_threshold`.
    pub score: u32,
    pub total_transactions: u64,
    pub successful_transactions: u64,
    pub disputed_transactions: u64,
    /// 0-10000 basis points of a 5-star scale, time-decayed like `score`.
    pub avg_rating: u32,
    pub last_updated: u64,
}

/// A persisted record of a single escrow transaction.
///
/// The `amount` field is informational-only and does not affect reputation
/// scoring. Scores are computed based solely on `outcome` and time decay,
/// not on transaction value. This design choice ensures that dust transactions
/// and high-value transactions are weighted equally in reputation calculations.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScoreDecomposition {
    pub entity: Address,
    pub base_score_bps: i128,
    pub penalty_bps: i128,
    pub total_transactions: u64,
    pub final_score: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionRecord {
    pub escrow_id: u64,
    pub entity: Address,
    pub counterparty: Address,
    /// Transaction amount in the smallest denomination of the token.
    /// This field is persisted for historical record-keeping but does not
    /// influence reputation scoring calculations.
    pub amount: i128,
    pub outcome: TransactionOutcome,
    /// 0-10000, set once by `rate_entity`.
    pub rating: Option<u32>,
    pub recorded_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransactionOutcome {
    Released,
    Refunded,
    Disputed,
    ResolvedSeller,
    ResolvedBuyer,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Flag {
    pub reporter: Address,
    pub entity: Address,
    pub reason: Symbol,
    pub details: Option<String>,
    pub flagged_at: u64,
    pub resolved: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationConfig {
    pub decay_window_seconds: u64,
    /// Minimum number of lifetime transactions (window-independent) an
    /// entity must accumulate before [`ReputationContract::get_reputation`]
    /// stops masking its `score`/`avg_rating` to `0`. The masking gate
    /// compares against the exact lifetime `total_transactions` counter —
    /// **not** the `SCORE_WINDOW` sample that feeds the score recompute — so
    /// it is always satisfiable regardless of the window. A valid threshold
    /// must not exceed `SCORE_WINDOW`, however: anything larger would describe
    /// a gate that never unmasks within the scoring-relevant window the
    /// contract's behavior is documented against, so `validate_config`
    /// rejects it with [`ReputationError::InvalidParam`].
    pub min_transactions_threshold: u64,
    pub dispute_penalty_bps: u32,
    pub freeze_threshold_flags: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct ContractVersion {
    pub name: Symbol,
    pub semver: Symbol,
}

/// # Cross-contract error-code allocation
///
/// Soroban error codes surface as raw `u32` values over a bridge, so each
/// contract must keep its numeric error space disjoint. Every contract error
/// enum uses a 16-bit contract prefix plus a contract-local code:
///
/// | Contract | Error enum | Base |
/// |----------|------------|------------|
/// | escrow | `EscrowError` | `0x0001_0000` |
/// | permissions | `PermissionError` | `0x0002_0000` |
/// | reputation | `ReputationError` | `0x0003_0000` |
/// | delegation_registry | `DelegationError` | `0x0004_0000` |
/// | marketplace | `MarketplaceError` | `0x0005_0000` |
///
/// A numeric code is `base + local_code`; the high 16 bits identify the
/// originating contract and the low 16 bits identify the variant inside that
/// contract. Keep this table in sync with the contract sources and keep the
/// allocation tests green.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ReputationError {
    /// Reserved for API/ABI compatibility with issue #18's error contract.
    /// Unreachable in normal operation: initialization now happens via
    /// `__constructor` (see [`ReputationContract::__constructor`]), which
    /// the host guarantees can run at most once, atomically with
    /// deployment — there is no second call for this to guard against.
    AlreadyInitialized = 0x0003_0001,
    NotInitialized = 0x0003_0002,
    Unauthorized = 0x0003_0003,
    EntityNotFound = 0x0003_0004,
    /// Same escrow_id already rated.
    DuplicateRating = 0x0003_0005,
    /// Rating out of range.
    InvalidRating = 0x0003_0006,
    EntityFrozen = 0x0003_0007,
    /// Same reporter already flagged.
    AlreadyFlagged = 0x0003_0008,
    /// Invalid input parameter.
    InvalidParam = 0x0003_0009,
    /// No active (unresolved) flag from reporter.
    NoActiveFlag = 0x0003_000A,
    /// Reporter did not flag the entity.
    NotFlagReporter = 0x0003_000B,
}

#[cfg(test)]
mod error_code_allocation {
    use super::*;
    const CONTRACT_SPACES: &[(&str, u32)] = &[
        ("EscrowError", 0x0001_0000),
        ("PermissionError", 0x0002_0000),
        ("ReputationError", 0x0003_0000),
        ("DelegationError", 0x0004_0000),
        ("MarketplaceError", 0x0005_0000),
    ];
    #[test]
    fn contract_spaces_are_disjoint() {
        for (i, &(_, base_a)) in CONTRACT_SPACES.iter().enumerate() {
            for &(_, base_b) in CONTRACT_SPACES.iter().skip(i + 1) {
                assert_ne!(base_a, base_b, "contract error-code spaces must be disjoint");
            }
        }
    }
    #[test]
    fn reputation_error_codes_are_unique_and_in_allocated_space() {
        let mut codes = [
            ReputationError::AlreadyInitialized as u32,
            ReputationError::NotInitialized as u32,
            ReputationError::Unauthorized as u32,
            ReputationError::EntityNotFound as u32,
            ReputationError::DuplicateRating as u32,
            ReputationError::InvalidRating as u32,
            ReputationError::EntityFrozen as u32,
            ReputationError::AlreadyFlagged as u32,
            ReputationError::InvalidParam as u32,
            ReputationError::NoActiveFlag as u32,
            ReputationError::NotFlagReporter as u32,
        ];
        for code in codes {
            assert!(
                (0x0003_0001..=0x0003_ffff).contains(&code),
                "ReputationError code {code:#x} escaped its allocated contract space"
            );
        }
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 11, "ReputationError codes must be unique");
    }
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct TransactionRecordedEvent {
    pub escrow_id: u64,
    pub entity: Address,
    pub outcome: TransactionOutcome,
    pub new_score: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct EntityRatedEvent {
    pub rater: Address,
    pub entity: Address,
    pub rating: u32,
    pub escrow_id: u64,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct EntityFlaggedEvent {
    pub reporter: Address,
    pub entity: Address,
    pub reason: Symbol,
    pub flag_count: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct EntityFrozenEvent {
    pub entity: Address,
    pub frozen_by: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct EntityUnfrozenEvent {
    pub entity: Address,
    pub unfrozen_by: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct AdminProposedEvent {
    pub current_admin: Address,
    pub new_admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminAcceptedEvent {
    pub new_admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntityHistoryPrunedEvent {
    pub entity: Address,
    pub pruned_count: u32,
    pub pruned_by: Address,
pub struct ScoreAccumulator {
    pub decay_window_seconds: u64,
    pub weighted_value_sum: i128,
    pub weight_sum: i128,
    pub rating_weighted_sum: i128,
    pub rating_weight_sum: i128,
    pub disputed_recent: i128,
}

#[contracttype]
pub enum DataKey {
    Admin,
    PendingAdmin,
    Config,
    Reputation(Address),
    TransactionHistory(Address),
    TransactionRecord(u64),
    Flags(Address),
    FrozenStatus(Address),
    RatedEscrows(Address),
    /// `true` once `.1` has appeared in a recorded transaction with `.0`.
    /// Stored in both directions so the relationship reads symmetrically while
    /// still supporting a directed lookup when needed.
    Transacted(Address, Address),
    /// Incremental weighted sums used by `record_transaction`'s hot path.
    ScoreAccumulator(Address),
}

/// Maximum basis points value (100.00%), used both for ratings/scores and
/// for the recency-weight scale in [`recency_weight_bps`].
const BPS_SCALE: i128 = 10_000;

/// The maximum number of full half-lives after which the recency weight is
/// treated as zero.  With `BPS_SCALE = 10_000`, the right-shift
/// `BPS_SCALE >> full_halvings` yields 1 at `full_halvings = 13`
/// (2^13 = 8 192 < 10 000) and 0 at `full_halvings = 14`
/// (2^14 = 16 384 > 10 000).  Setting `MAX_HALVINGS = 13` therefore makes
/// the early-exit guard reachable *and* precise: it fires exactly when the
/// shift-based computation would produce a non-zero base for the last time.
///
/// Invariant (enforced by the `test_max_halvings_invariant` unit test):
///   `BPS_SCALE >> MAX_HALVINGS != 0`
const MAX_HALVINGS: u64 = 13;

/// Caps how many of an entity's most recent transactions feed the
/// time-decayed score/avg_rating computation in [`ReputationContract::recompute_score`],
/// so `record_transaction` and `rate_entity` stay bounded-cost regardless of
/// how large an entity's lifetime history grows.
const SCORE_WINDOW: u32 = 200;

/// Persistent entries are bumped when they approach expiry and kept alive
/// for roughly 30 days, matching the repository's persistent-storage policy.
const PERSISTENT_BUMP_THRESHOLD: u32 = 17_280;
const PERSISTENT_BUMP_AMOUNT: u32 = 518_400;

/// Maps a transaction outcome to its contribution toward `score`, in basis
/// points, per the reputation score formula.
fn outcome_value_bps(outcome: &TransactionOutcome) -> i128 {
    match outcome {
        TransactionOutcome::Released => 10_000,
        TransactionOutcome::Refunded => 2_000,
        TransactionOutcome::Disputed => 0,
        TransactionOutcome::ResolvedSeller => 8_000,
        TransactionOutcome::ResolvedBuyer => 2_000,
    }
}

/// Time-decayed recency weight in basis points: `10000 * 2^(-elapsed /
/// decay_window)`, i.e. weight halves every `decay_window_secs` (the
/// formula's half-life). WASM contracts cannot use floating point, so the
/// exponential is evaluated as an exact halving for each full half-life
/// elapsed, with a linear interpolation between consecutive halvings for the
/// remainder — a deterministic fixed-point approximation of `e^(-lambda *
/// t)` accurate to a few percent, which is sufficient for reputation
/// weighting.
fn recency_weight_bps(elapsed_secs: u64, decay_window_secs: u64) -> i128 {
    if decay_window_secs == 0 {
        return BPS_SCALE;
    }
    let full_halvings = elapsed_secs / decay_window_secs;
    if full_halvings >= MAX_HALVINGS {
        return 0;
    }
    let remainder_secs = elapsed_secs % decay_window_secs;
    let base = BPS_SCALE >> full_halvings;
    if base == 0 {
        return 0;
    }
    let numerator = base * (remainder_secs as i128);
    let denominator = 2 * (decay_window_secs as i128);
    let decrement = numerator / denominator;
    (base - decrement).max(0)
}

#[contract]
pub struct ReputationContract;

#[contractimpl]
impl ReputationContract {
    // --- Initialization ---

    /// Sets the admin and config. This is a Soroban *constructor*: the host
    /// invokes it exactly once, atomically with contract deployment (see
    /// `env.register(ReputationContract, (admin, config))`), and rejects any
    /// later attempt to call it directly.
    ///
    /// A plain post-deploy `initialize(...)` function — as used elsewhere in
    /// this workspace (`escrow`, `permissions`, `delegation_registry`) —
    /// leaves a window between deployment and initialization where anyone
    /// can call it first and self-authorize as `admin`, since `admin` is
    /// itself just a caller-supplied parameter and `require_auth()` on it
    /// only proves the caller controls *some* address, not that they're the
    /// intended deployer. Making initialization part of deployment itself
    /// closes that window entirely for this contract.
    pub fn __constructor(
        env: Env,
        admin: Address,
        config: ReputationConfig,
    ) -> Result<(), ReputationError> {
        Self::validate_config(&config)?;

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Config, &config);
        // Keep the contract instance alive from deployment.
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
        Ok(())
    }

    pub fn version(env: Env) -> ContractVersion {
        ContractVersion {
            name: symbol_short!("reput"),
            semver: Symbol::new(&env, env!("CARGO_PKG_VERSION")),
        }
    }

    // --- Core Recording ---

    /// Record a transaction outcome for `entity`. Called by the authorized
    /// backend/admin address (see the integration section of issue #18) once
    /// an escrow reaches `Released`, `Refunded`, `Disputed`, `ResolvedSeller`
    /// or `ResolvedBuyer`.
    ///
    /// Calling this again with an `escrow_id` already on file updates that
    /// record in place (e.g. `Disputed` followed later by `ResolvedSeller`
    /// for the same escrow) rather than appending a duplicate — the escrow's
    /// lifecycle can legitimately call this more than once, but it should
    /// only ever count once toward `total_transactions`.
    pub fn record_transaction(
        env: Env,
        caller: Address,
        escrow_id: u64,
        entity: Address,
        counterparty: Address,
        amount: i128,
        outcome: TransactionOutcome,
    ) -> Result<(), ReputationError> {
        caller.require_auth();
        let admin = Self::require_admin(&env)?;
        if caller != admin {
            return Err(ReputationError::Unauthorized);
        }

        let record_key = DataKey::TransactionRecord(escrow_id);
        let existing: Option<TransactionRecord> = env.storage().persistent().get(&record_key);
        if let Some(prior) = &existing {
            if prior.entity != entity {
                return Err(ReputationError::InvalidParam);
            }
        } else if Self::is_frozen(env.clone(), entity.clone()) {
            // Only reject brand-new escrows for a frozen entity — a
            // lifecycle update to an escrow already on file (e.g. `Disputed`
            // followed later by `ResolvedSeller`) must still be allowed to
            // land, otherwise a dispute recorded before a freeze could never
            // resolve and its penalty would outlive the freeze.
            return Err(ReputationError::EntityFrozen);
        }

        let record = TransactionRecord {
            escrow_id,
            entity: entity.clone(),
            counterparty,
            amount,
            outcome: outcome.clone(),
            rating: existing.as_ref().and_then(|r| r.rating),
            recorded_at: env.ledger().timestamp(),
        };
        env.storage().persistent().set(&record_key, &record);
        env.storage().persistent().extend_ttl(
            &record_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        // The contract instance must stay alive alongside its records.
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);

        let history_len_before = if let Some(prior) = &existing {
            Self::apply_outcome_change_counts(&env, &entity, &prior.outcome, &outcome);
            None
        } else {
            let hist_key = DataKey::TransactionHistory(entity.clone());
            let mut history: Vec<u64> = env
                .storage()
                .persistent()
                .get(&hist_key)
                .unwrap_or_else(|| Vec::new(&env));
            let len_before = history.len();
            history.push_back(escrow_id);
            env.storage().persistent().set(&hist_key, &history);
            env.storage().persistent().extend_ttl(
                &hist_key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );

            // Record the symmetric counterpart relationship in both directions so
            // a transaction between A and B reads as transacted for both A->B and
            // B->A when callers need a bidirectional relationship check.
            env.storage().persistent().set(
                &DataKey::Transacted(entity.clone(), record.counterparty.clone()),
                &true,
            );
            env.storage().persistent().extend_ttl(
                &DataKey::Transacted(entity.clone(), record.counterparty.clone()),
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );
            env.storage().persistent().set(
                &DataKey::Transacted(record.counterparty.clone(), entity.clone()),
                &true,
            );
            env.storage().persistent().extend_ttl(
                &DataKey::Transacted(record.counterparty.clone(), entity.clone()),
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );
            Self::apply_new_transaction_counts(&env, &entity, &outcome);
            Some(len_before)
        };

        // New escrow records slide the window incrementally; in-place
        // lifecycle updates fall back to the full recompute path.
        let score = match history_len_before {
            Some(len_before) => Self::apply_incremental_score_update(&env, &entity, len_before)?,
            None => Self::recompute_score(&env, &entity)?,
        };

        env.events().publish(
            (symbol_short!("reput"), symbol_short!("tx_rec")),
            TransactionRecordedEvent {
                escrow_id,
                entity,
                outcome,
                new_score: score.score,
            },
        );

        Ok(())
    }

    /// Rate the entity on the other side of a completed escrow. `rater` must
    /// be the `counterparty` recorded for `escrow_id`, and each escrow may
    /// be rated at most once (sybil resistance).
    pub fn rate_entity(
        env: Env,
        rater: Address,
        escrow_id: u64,
        entity: Address,
        rating: u32,
    ) -> Result<(), ReputationError> {
        rater.require_auth();
        if rating as i128 > BPS_SCALE {
            return Err(ReputationError::InvalidRating);
        }
        if Self::is_frozen(env.clone(), entity.clone()) {
            return Err(ReputationError::EntityFrozen);
        }

        let record_key = DataKey::TransactionRecord(escrow_id);
        let mut record: TransactionRecord = env
            .storage()
            .persistent()
            .get(&record_key)
            .ok_or(ReputationError::EntityNotFound)?;
        if record.entity != entity || record.counterparty != rater {
            return Err(ReputationError::Unauthorized);
        }
        if matches!(record.outcome, TransactionOutcome::Disputed) {
            return Err(ReputationError::InvalidParam);
        }

        let rated_key = DataKey::RatedEscrows(rater.clone());
        let mut rated: Vec<u64> = env
            .storage()
            .persistent()
            .get(&rated_key)
            .unwrap_or_else(|| Vec::new(&env));
        if rated.contains(escrow_id) {
            return Err(ReputationError::DuplicateRating);
        }
        rated.push_back(escrow_id);
        env.storage().persistent().set(&rated_key, &rated);

        record.rating = Some(rating);
        env.storage().persistent().set(&record_key, &record);

        Self::recompute_score(&env, &entity)?;

        env.events().publish(
            (symbol_short!("reput"), symbol_short!("rated")),
            EntityRatedEvent {
                rater,
                entity,
                rating,
                escrow_id,
            },
        );

        Ok(())
    }

    // --- Read-Only Views ---

    /// Returns `entity`'s reputation. `score` and `avg_rating` are masked to
    /// `0` while `total_transactions` is below
    /// `ReputationConfig::min_transactions_threshold`, per the score's
    /// public-visibility rule.
    pub fn get_reputation(env: Env, entity: Address) -> Result<ReputationScore, ReputationError> {
        let config = Self::get_config(env.clone())?;
        let mut record: ReputationScore = env
            .storage()
            .persistent()
            .get(&DataKey::Reputation(entity.clone()))
            .ok_or(ReputationError::EntityNotFound)?;

        Self::bump_entity(&env, &entity);

        if record.total_transactions < config.min_transactions_threshold {
            record.score = 0;
            record.avg_rating = 0;
        }
        Ok(record)
    }

    /// Returns the raw base/penalty breakdown behind `entity`'s current
    /// score, including the clamped final score that `get_reputation`
    /// reports.
    pub fn get_score_decomposition(
        env: Env,
        entity: Address,
    ) -> Result<ScoreDecomposition, ReputationError> {
        let rep = Self::load_or_default_reputation(&env, &entity);
        let (decomposition, _, _) =
            Self::compute_score_components(&env, &entity, rep.total_transactions)?;
        Ok(decomposition)
    }

    pub fn get_reputation_breakdown(
        env: Env,
        entity: Address,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<TransactionRecord>, ReputationError> {
        Self::bump_entity(&env, &entity);

        let history: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::TransactionHistory(entity))
            .unwrap_or_else(|| Vec::new(&env));

        let mut result = Vec::new(&env);
        let end = offset.saturating_add(limit).min(history.len());
        let mut i = offset;
        while i < end {
            let escrow_id = history.get(i).unwrap();
            if let Some(record) = env
                .storage()
                .persistent()
                .get::<DataKey, TransactionRecord>(&DataKey::TransactionRecord(escrow_id))
            {
                result.push_back(record);
            }
            i += 1;
        }
        Ok(result)
    }

    pub fn get_flags(
        env: Env,
        entity: Address,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<Flag>, ReputationError> {
        Self::bump_entity(&env, &entity);

        let flags: Vec<Flag> = env
            .storage()
            .persistent()
            .get(&DataKey::Flags(entity))
            .unwrap_or_else(|| Vec::new(&env));

        let mut result = Vec::new(&env);
        let end = offset.saturating_add(limit).min(flags.len());
        let mut i = offset;
        while i < end {
            result.push_back(flags.get(i).unwrap());
            i += 1;
        }
        Ok(result)
    }

    pub fn is_frozen(env: Env, entity: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::FrozenStatus(entity))
            .unwrap_or(false)
    }

    /// Returns whether two entities have a transaction relationship.
    ///
    /// When `directed` is `false`, the check is symmetric: it returns true if
    /// either direction has been recorded. When `directed` is `true`, it checks
    /// only the exact `(entity_a, entity_b)` direction.
    pub fn has_relation(env: Env, entity_a: Address, entity_b: Address, directed: bool) -> bool {
        let forward = env
            .storage()
            .persistent()
            .get(&DataKey::Transacted(entity_a.clone(), entity_b.clone()))
            .unwrap_or(false);
        if directed {
            return forward;
        }
        forward
            || env
                .storage()
                .persistent()
                .get(&DataKey::Transacted(entity_b, entity_a))
                .unwrap_or(false)
    }

    pub fn get_config(env: Env) -> Result<ReputationConfig, ReputationError> {
        env.storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(ReputationError::NotInitialized)
    }

    // --- Flagging ---

    /// Report `entity` for fraud or dispute-worthy behavior. Reporting is
    /// gated to the admin or an address that has actually transacted with
    /// `entity` (i.e. appears as `counterparty` on one of its recorded
    /// transactions) — otherwise anyone could mint free addresses and
    /// auto-freeze an arbitrary entity by reaching `freeze_threshold_flags`
    /// with throwaway reporters. A reporter may have at most one active
    /// (unresolved) flag per entity. Once the entity's active flag count
    /// reaches `ReputationConfig::freeze_threshold_flags`, it is auto-frozen.
    pub fn flag_entity(
        env: Env,
        reporter: Address,
        entity: Address,
        reason: Symbol,
        details: Option<String>,
    ) -> Result<(), ReputationError> {
        reporter.require_auth();
        let config = Self::get_config(env.clone())?;
        let admin = Self::require_admin(&env)?;
        if reporter != admin && !Self::has_transacted_with(&env, &entity, &reporter) {
            return Err(ReputationError::Unauthorized);
        }

        let key = DataKey::Flags(entity.clone());
        let mut flags: Vec<Flag> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));

        if flags.iter().any(|f| f.reporter == reporter && !f.resolved) {
            return Err(ReputationError::AlreadyFlagged);
        }

        flags.push_back(Flag {
            reporter: reporter.clone(),
            entity: entity.clone(),
            reason: reason.clone(),
            details,
            flagged_at: env.ledger().timestamp(),
            resolved: false,
        });
        env.storage().persistent().set(&key, &flags);

        let active_count = flags.iter().filter(|f| !f.resolved).count() as u32;

        env.events().publish(
            (symbol_short!("reput"), symbol_short!("flagged")),
            EntityFlaggedEvent {
                reporter,
                entity: entity.clone(),
                reason,
                flag_count: active_count,
            },
        );

        if active_count >= config.freeze_threshold_flags
            && !Self::is_frozen(env.clone(), entity.clone())
        {
            env.storage()
                .persistent()
                .set(&DataKey::FrozenStatus(entity.clone()), &true);
            env.events().publish(
                (symbol_short!("reput"), symbol_short!("frozen")),
                EntityFrozenEvent {
                    entity,
                    frozen_by: env.current_contract_address(),
                },
            );
        }

        Ok(())
    }

    /// Mark `reporter`'s flag against `entity` resolved. Admin-only. Does
    /// not automatically unfreeze — see [`Self::unfreeze_entity`].
    pub fn resolve_flag(
        env: Env,
        admin: Address,
        reporter: Address,
        entity: Address,
    ) -> Result<(), ReputationError> {
        admin.require_auth();
        Self::require_caller_is_admin(&env, &admin)?;

        let key = DataKey::Flags(entity.clone());
        let mut flags: Vec<Flag> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));

        // Check if entity has any flags at all
        if flags.is_empty() {
            return Err(ReputationError::EntityNotFound);
        }

        // Check if reporter has any flags (active or resolved) for this entity
        let has_any_flag = flags.iter().any(|f| f.reporter == reporter);
        if !has_any_flag {
            return Err(ReputationError::NotFlagReporter);
            return Err(ReputationError::NoActiveFlag);
        // Error taxonomy (see ReputationError):
        // - entity never seen on-chain (no reputation record, no relation) → EntityNotFound
        // - entity known, but there is no flag to resolve for it → NoActiveFlag
        // - entity has flags, but none from this reporter → NotFlagReporter
        // - reporter's flag exists but is already resolved → NoActiveFlag
            let entity_known = env
                .storage()
                .persistent()
                .has(&DataKey::Reputation(entity.clone()))
                || Self::has_transacted_with(&env, &entity, &reporter);
            return Err(if entity_known {
                ReputationError::NoActiveFlag
            } else {
                ReputationError::EntityNotFound
            });
        }

        let idx = flags
            .iter()
            .position(|f| f.reporter == reporter && !f.resolved)
            .ok_or(ReputationError::NoActiveFlag)?;
        // Distinguish between "no flags exist at all" and "flags exist but
        // none are from this reporter" so callers get a precise error.
        let idx = if flags.is_empty() {
            return Err(ReputationError::NoActiveFlag);
        } else {
            flags
                .iter()
                .position(|f| f.reporter == reporter && !f.resolved)
                .ok_or(ReputationError::NotFlagReporter)?
        };
            .position(|f| f.reporter == reporter)
            .ok_or(ReputationError::NotFlagReporter)?;
            .position(|f| f.reporter == reporter && !f.resolved);
        let Some(idx) = idx else {
            // Distinguish why there is no active flag to clear for `reporter`
            // so off-chain tooling can react appropriately.
            if !flags.iter().any(|f| !f.resolved) || flags.iter().any(|f| f.reporter == reporter) {
                // Nothing active on the entity at all, or `reporter`'s own
                // flags are all already resolved.
                return Err(ReputationError::NoActiveFlag);
            }
            // Some other reporter's flag is active; `reporter` has never
            // flagged this entity.
            return Err(ReputationError::NotFlagReporter);
        let mut flag = flags.get(idx as u32).unwrap();
        if flag.resolved {
            return Err(ReputationError::NoActiveFlag);
        }
        flag.resolved = true;
        flags.set(idx as u32, flag);
        env.storage().persistent().set(&key, &flags);

        Ok(())
    }

    // --- Admin ---

    pub fn freeze_entity(env: Env, admin: Address, entity: Address) -> Result<(), ReputationError> {
        admin.require_auth();
        Self::require_caller_is_admin(&env, &admin)?;

        env.storage()
            .persistent()
            .set(&DataKey::FrozenStatus(entity.clone()), &true);
        env.events().publish(
            (symbol_short!("reput"), symbol_short!("frozen")),
            EntityFrozenEvent {
                entity,
                frozen_by: admin,
            },
        );
        Ok(())
    }

    pub fn unfreeze_entity(
        env: Env,
        admin: Address,
        entity: Address,
    ) -> Result<(), ReputationError> {
        admin.require_auth();
        Self::require_caller_is_admin(&env, &admin)?;

        env.storage()
            .persistent()
            .set(&DataKey::FrozenStatus(entity.clone()), &false);
        env.events().publish(
            (symbol_short!("reput"), symbol_short!("unfrozn")),
            EntityUnfrozenEvent {
                entity,
                unfrozen_by: admin,
            },
        );
        Ok(())
    }

    /// Prune an entity's transaction history records that are outside the scoring window (`SCORE_WINDOW` = 200).
    ///
    /// Callable by admin for state maintenance / cold-storage hygiene. Bounded by `max_records_to_prune` (capped at 50).
    /// Returns the number of pruned records.
    pub fn prune_entity_history(
        env: Env,
        admin: Address,
        entity: Address,
        max_records_to_prune: u32,
    ) -> Result<u32, ReputationError> {
        admin.require_auth();
        Self::require_caller_is_admin(&env, &admin)?;

        if max_records_to_prune == 0 {
            return Ok(0);
        }
        let cap = max_records_to_prune.min(50);

        let hist_key = DataKey::TransactionHistory(entity.clone());
        let history: Vec<u64> = env
            .storage()
            .persistent()
            .get(&hist_key)
            .unwrap_or_else(|| Vec::new(&env));

        if history.len() <= SCORE_WINDOW {
            return Ok(0);
        }

        let excess = (history.len() - SCORE_WINDOW).min(cap);
        let mut pruned_count: u32 = 0;

        let mut new_history = Vec::new(&env);
        for (i, id) in history.iter().enumerate() {
            if (i as u32) < excess {
                let record_key = DataKey::TransactionRecord(id);
                env.storage().persistent().remove(&record_key);
                pruned_count += 1;
            } else {
                new_history.push_back(id);
            }
        }

        env.storage().persistent().set(&hist_key, &new_history);

        if pruned_count > 0 {
            env.events().publish(
                (symbol_short!("reput"), symbol_short!("pruned")),
                EntityHistoryPrunedEvent {
                    entity,
                    pruned_count,
                    pruned_by: admin,
                },
            );
        }

        Ok(pruned_count)
    }

    pub fn update_config(
        env: Env,
        admin: Address,
        config: ReputationConfig,
    ) -> Result<(), ReputationError> {
        admin.require_auth();
        Self::require_caller_is_admin(&env, &admin)?;
        Self::validate_config(&config)?;

        env.storage().instance().set(&DataKey::Config, &config);
        Ok(())
    }

    /// Propose a new admin. Must be called by the current admin.
    pub fn propose_admin(
        env: Env,
        current_admin: Address,
        new_admin: Address,
    ) -> Result<(), ReputationError> {
        current_admin.require_auth();
        Self::require_caller_is_admin(&env, &current_admin)?;

        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);
        env.events().publish(
            (symbol_short!("reput"), soroban_sdk::Symbol::new(&env, "admin_prop")),
            AdminProposedEvent {
                current_admin,
                new_admin,
            },
        );
        Ok(())
    }

    /// Accept a proposed admin transfer. Must be called by the pending admin.
    pub fn accept_admin(env: Env, caller: Address) -> Result<(), ReputationError> {
        caller.require_auth();
        let pending: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .ok_or(ReputationError::Unauthorized)?;
        if caller != pending {
            return Err(ReputationError::Unauthorized);
        }

        env.storage().instance().set(&DataKey::Admin, &caller);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.events().publish(
            (symbol_short!("reput"), symbol_short!("admin_acc")),
            AdminAcceptedEvent { new_admin: caller },
        );
        Ok(())
    }

    // --- Internal helpers ---

    fn require_admin(env: &Env) -> Result<Address, ReputationError> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(ReputationError::NotInitialized)
    }

    fn require_caller_is_admin(env: &Env, caller: &Address) -> Result<(), ReputationError> {
        let admin = Self::require_admin(env)?;
        if *caller != admin {
            return Err(ReputationError::Unauthorized);
        }
        Ok(())
    }

    fn validate_config(config: &ReputationConfig) -> Result<(), ReputationError> {
        if config.decay_window_seconds == 0 {
            return Err(ReputationError::InvalidParam);
        }
        if config.dispute_penalty_bps as i128 > BPS_SCALE {
            return Err(ReputationError::InvalidParam);
        }
        if config.freeze_threshold_flags == 0 {
            return Err(ReputationError::InvalidParam);
        }
        // The masking gate compares lifetime `total_transactions` (see
        // [`Self::get_reputation`]), so a threshold above `SCORE_WINDOW` can
        // only ever unmask after the recompute window has already slid past
        // the score-relevant records — the gate then silently samples a
        // stale subset instead of unlocking as documented. Reject it.
        if config.min_transactions_threshold > SCORE_WINDOW as u64 {
            return Err(ReputationError::InvalidParam);
        }
        Ok(())
    }

    /// Returns `true` if `counterparty` has appeared on at least one of
    /// `entity`'s recorded transactions. Used to gate [`Self::flag_entity`]
    /// so only genuine counterparties (or the admin) can report an entity.
    ///
    /// This is an O(1) lookup against `DataKey::Transacted`, written once
    /// per new escrow in `record_transaction` — not a scan over
    /// `TransactionHistory`, which would make `flag_entity`'s cost grow with
    /// the entity's lifetime transaction count (the same unbounded-growth
    /// problem `SCORE_WINDOW` guards against in `recompute_score`).
    fn has_transacted_with(env: &Env, entity: &Address, counterparty: &Address) -> bool {
        Self::has_relation(env.clone(), entity.clone(), counterparty.clone(), false)
    }

    /// Refreshes the persistent storage TTL on `entity`'s score record,
    /// top-`SCORE_WINDOW` transaction history records, and flags.
    fn bump_entity(env: &Env, entity: &Address) {
        let rep_key = DataKey::Reputation(entity.clone());
        if env.storage().persistent().has(&rep_key) {
            env.storage().persistent().extend_ttl(
                &rep_key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );
        }

        let hist_key = DataKey::TransactionHistory(entity.clone());
        if env.storage().persistent().has(&hist_key) {
            env.storage().persistent().extend_ttl(
                &hist_key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );
            if let Some(history) = env
                .storage()
                .persistent()
                .get::<_, Vec<u64>>(&hist_key)
            {
                let len = history.len();
                let start = len.saturating_sub(SCORE_WINDOW);
                let mut i = start;
                while i < len {
                    let escrow_id = history.get(i).unwrap();
                    let rec_key = DataKey::TransactionRecord(escrow_id);
                    if env.storage().persistent().has(&rec_key) {
                        env.storage().persistent().extend_ttl(
                            &rec_key,
                            PERSISTENT_BUMP_THRESHOLD,
                            PERSISTENT_BUMP_AMOUNT,
                        );
                    }
                    i += 1;
                }
            }
        }

        let flags_key = DataKey::Flags(entity.clone());
        if env.storage().persistent().has(&flags_key) {
            env.storage().persistent().extend_ttl(
                &flags_key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );
        }

        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
    }

    fn load_or_default_reputation(env: &Env, entity: &Address) -> ReputationScore {
        env.storage()
            .persistent()
            .get(&DataKey::Reputation(entity.clone()))
            .unwrap_or(ReputationScore {
                entity: entity.clone(),
                score: 0,
                total_transactions: 0,
                successful_transactions: 0,
                disputed_transactions: 0,
                avg_rating: 0,
                last_updated: 0,
            })
    }

    /// `true` for the outcomes that count toward `successful_transactions`.
    fn is_successful_outcome(outcome: &TransactionOutcome) -> bool {
        matches!(
            outcome,
            TransactionOutcome::Released | TransactionOutcome::ResolvedSeller
        )
    }

    /// Increments `entity`'s lifetime counters for a brand-new escrow.
    /// Called once per `escrow_id`, not on lifecycle updates — see
    /// [`Self::apply_outcome_change_counts`] for those.
    fn apply_new_transaction_counts(env: &Env, entity: &Address, outcome: &TransactionOutcome) {
        let mut rep = Self::load_or_default_reputation(env, entity);
        rep.total_transactions += 1;
        if Self::is_successful_outcome(outcome) {
            rep.successful_transactions += 1;
        }
        if matches!(outcome, TransactionOutcome::Disputed) {
            rep.disputed_transactions += 1;
        }
        env.storage()
            .persistent()
            .set(&DataKey::Reputation(entity.clone()), &rep);
        env.storage().persistent().extend_ttl(
            &DataKey::Reputation(entity.clone()),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
    }

    /// Adjusts `entity`'s lifetime counters when an already-recorded escrow's
    /// outcome changes (e.g. `Disputed` -> `ResolvedSeller`), without
    /// touching `total_transactions`.
    fn apply_outcome_change_counts(
        env: &Env,
        entity: &Address,
        prior: &TransactionOutcome,
        new: &TransactionOutcome,
    ) {
        let mut rep = Self::load_or_default_reputation(env, entity);
        if Self::is_successful_outcome(prior) {
            rep.successful_transactions = rep.successful_transactions.saturating_sub(1);
        }
        if matches!(prior, TransactionOutcome::Disputed) {
            rep.disputed_transactions = rep.disputed_transactions.saturating_sub(1);
        }
        if Self::is_successful_outcome(new) {
            rep.successful_transactions += 1;
        }
        if matches!(new, TransactionOutcome::Disputed) {
            rep.disputed_transactions += 1;
        }
        env.storage()
            .persistent()
            .set(&DataKey::Reputation(entity.clone()), &rep);
        env.storage().persistent().extend_ttl(
            &DataKey::Reputation(entity.clone()),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
    }

    /// Applies the newest record's contribution to the incremental accumulator.
    fn add_record_contribution(
        accumulator: &mut ScoreAccumulator,
        config: &ReputationConfig,
        record: &TransactionRecord,
        now: u64,
    ) {
        let elapsed = now.saturating_sub(record.recorded_at);
        let weight = recency_weight_bps(elapsed, config.decay_window_seconds);
        let value = outcome_value_bps(&record.outcome);
        accumulator.weighted_value_sum += weight * value;
        accumulator.weight_sum += weight;
        if matches!(record.outcome, TransactionOutcome::Disputed) && weight > 0 {
            accumulator.disputed_recent += 1;
        }
        if let Some(rating) = record.rating {
            accumulator.rating_weighted_sum += weight * (rating as i128);
            accumulator.rating_weight_sum += weight;
        }
    }

    /// Removes an evicted record's contribution from the incremental accumulator.
    fn remove_record_contribution(
        accumulator: &mut ScoreAccumulator,
        config: &ReputationConfig,
        record: &TransactionRecord,
        now: u64,
    ) {
        let elapsed = now.saturating_sub(record.recorded_at);
        let weight = recency_weight_bps(elapsed, config.decay_window_seconds);
        let value = outcome_value_bps(&record.outcome);
        accumulator.weighted_value_sum -= weight * value;
        accumulator.weight_sum -= weight;
        if matches!(record.outcome, TransactionOutcome::Disputed) && weight > 0 {
            accumulator.disputed_recent -= 1;
        }
        if let Some(rating) = record.rating {
            accumulator.rating_weighted_sum -= weight * (rating as i128);
            accumulator.rating_weight_sum -= weight;
        }
    }

    /// Incrementally updates `score`/`avg_rating` after a new transaction has
    /// been appended to `TransactionHistory`. Falls back to a full
    /// recomputation whenever the accumulator is unavailable or stale, a
    /// record is missing, or the history was not appended as expected.
    fn apply_incremental_score_update(
        env: &Env,
        entity: &Address,
        history_len_before: u32,
    ) -> Result<ReputationScore, ReputationError> {
        let config = Self::get_config(env.clone())?;
        let now = env.ledger().timestamp();

        let history: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::TransactionHistory(entity.clone()))
            .unwrap_or_else(|| Vec::new(env));
        let len_after = history.len();
        if len_after != history_len_before.saturating_add(1) {
            return Self::recompute_score(env, entity);
        }

        let mut accumulator: ScoreAccumulator = match env
            .storage()
            .persistent()
            .get(&DataKey::ScoreAccumulator(entity.clone()))
        {
            Some(acc) if acc.decay_window_seconds == config.decay_window_seconds => acc,
            None if history_len_before == 0 => ScoreAccumulator {
                decay_window_seconds: config.decay_window_seconds,
                weighted_value_sum: 0,
                weight_sum: 0,
                rating_weighted_sum: 0,
                rating_weight_sum: 0,
                disputed_recent: 0,
            },
            _ => return Self::recompute_score(env, entity),
        };

        let newest_idx = len_after.saturating_sub(1);
        let newest_id = match history.get(newest_idx) {
            Some(id) => id,
            None => return Self::recompute_score(env, entity),
        };
        let newest_record: Option<TransactionRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::TransactionRecord(newest_id));
        match newest_record {
            Some(record) => {
                Self::add_record_contribution(&mut accumulator, &config, &record, now);
            }
            None => return Self::recompute_score(env, entity),
        };

        if len_after > SCORE_WINDOW {
            let evicted_idx = len_after.saturating_sub(SCORE_WINDOW).saturating_sub(1);
            let evicted_id = match history.get(evicted_idx) {
                Some(id) => id,
                None => return Self::recompute_score(env, entity),
            };
            let evicted_record: Option<TransactionRecord> = env
                .storage()
                .persistent()
                .get(&DataKey::TransactionRecord(evicted_id));
            match evicted_record {
                Some(record) => {
                    Self::remove_record_contribution(&mut accumulator, &config, &record, now);
                }
                None => return Self::recompute_score(env, entity),
            };
        }

        let mut rep = Self::load_or_default_reputation(env, entity);
        let base_score = if accumulator.weight_sum > 0 {
            accumulator.weighted_value_sum / accumulator.weight_sum
        } else {
            0
        };
        let penalty = accumulator.disputed_recent * (config.dispute_penalty_bps as i128);
        rep.score = (base_score - penalty).clamp(0, BPS_SCALE) as u32;
        rep.avg_rating = if accumulator.rating_weight_sum > 0 {
            (accumulator.rating_weighted_sum / accumulator.rating_weight_sum).clamp(0, BPS_SCALE)
                as u32
        } else {
            0
        };
        rep.last_updated = now;

        env.storage()
            .persistent()
            .set(&DataKey::Reputation(entity.clone()), &rep);
        env.storage().persistent().extend_ttl(
            &DataKey::Reputation(entity.clone()),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        env.storage()
            .persistent()
            .set(&DataKey::ScoreAccumulator(entity.clone()), &accumulator);
        env.storage().persistent().extend_ttl(
            &DataKey::ScoreAccumulator(entity.clone()),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        Ok(rep)
    }

    /// Computes the raw score decomposition and the time-decayed average
    /// rating in a single pass over the recency window. Does not persist;
    /// `recompute_score` uses the returned values to update and emit the
    /// breakdown, while `get_score_decomposition` returns them directly.
    fn compute_score_components(
        env: &Env,
        entity: &Address,
        total_transactions: u64,
    ) -> Result<(ScoreDecomposition, u32, u64), ReputationError> {
        let config = Self::get_config(env.clone())?;

        let history: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::TransactionHistory(entity.clone()))
            .unwrap_or_else(|| Vec::new(env));
        let now = env.ledger().timestamp();
        let len = history.len();
        let start = len.saturating_sub(SCORE_WINDOW);

        let mut weighted_value_sum: i128 = 0;
        let mut weight_sum: i128 = 0;
        let mut rating_weighted_sum: i128 = 0;
        let mut rating_weight_sum: i128 = 0;
        let mut disputed_recent: i128 = 0;

        let mut i = start;
        while i < len {
            let escrow_id = history.get(i).unwrap();
            i += 1;

            // A persistent entry can expire its TTL and be archived
            // independently of `TransactionHistory`; treat a missing record
            // as no longer relevant to the score rather than failing the
            // whole recomputation.
            let record: Option<TransactionRecord> = env
                .storage()
                .persistent()
                .get(&DataKey::TransactionRecord(escrow_id));
            let record = match record {
                Some(record) => record,
                None => continue,
            };

            let elapsed = now.saturating_sub(record.recorded_at);
            let weight = recency_weight_bps(elapsed, config.decay_window_seconds);
            let value = outcome_value_bps(&record.outcome);
            weighted_value_sum += weight * value;
            weight_sum += weight;

            if matches!(record.outcome, TransactionOutcome::Disputed) && weight > 0 {
                disputed_recent += 1;
            }

            if let Some(rating) = record.rating {
                rating_weighted_sum += weight * (rating as i128);
                rating_weight_sum += weight;
            }
        }

        let accumulator = ScoreAccumulator {
            decay_window_seconds: config.decay_window_seconds,
            weighted_value_sum,
            weight_sum,
            rating_weighted_sum,
            rating_weight_sum,
            disputed_recent,
        };

        let base_score = if weight_sum > 0 {
            weighted_value_sum / weight_sum
        } else {
            0
        };
        let penalty = disputed_recent * (config.dispute_penalty_bps as i128);
        let avg_rating = if rating_weight_sum > 0 {
            (rating_weighted_sum / rating_weight_sum).clamp(0, BPS_SCALE) as u32
        } else {
            0
        };

        Ok((
            ScoreDecomposition {
                entity: entity.clone(),
                base_score_bps: base_score,
                penalty_bps: penalty,
                total_transactions,
                final_score: (base_score - penalty).clamp(0, BPS_SCALE) as u32,
            },
            avg_rating,
            now,
        ))
    }

    /// Recomputes and persists `entity`'s `score`/`avg_rating`/
    /// `last_updated`, per the score formula:
    ///
    /// ```text
    /// score = sum(recency_weight(r) * outcome_value(r)) / sum(recency_weight(r))
    /// ```
    ///
    /// with an additional flat penalty of `dispute_penalty_bps` subtracted
    /// per still-relevant (non-fully-decayed) `Disputed` record.
    /// `avg_rating` is computed the same way over records carrying a rating.
    ///
    /// Only the most recent `SCORE_WINDOW` records feed this computation, so
    /// `record_transaction` and `rate_entity` stay bounded-cost regardless of
    /// how large an entity's lifetime history grows; records older than that
    /// already carry a recency weight close to zero for any realistic
    /// `decay_window_seconds`, so excluding them from the average has
    /// negligible effect. `total_transactions` / `successful_transactions` /
    /// `disputed_transactions` are exact lifetime counts maintained
    /// separately and incrementally — see [`Self::apply_new_transaction_counts`]
    /// and [`Self::apply_outcome_change_counts`] — so they are left as-is here.
    fn recompute_score(env: &Env, entity: &Address) -> Result<ReputationScore, ReputationError> {
        let mut rep = Self::load_or_default_reputation(env, entity);
        let (decomposition, avg_rating, now) =
            Self::compute_score_components(env, entity, rep.total_transactions)?;
        rep.score = decomposition.final_score;
        rep.avg_rating = avg_rating;
        rep.last_updated = now;

        env.storage()
            .persistent()
            .set(&DataKey::Reputation(entity.clone()), &rep);
        env.storage().persistent().extend_ttl(
            &DataKey::Reputation(entity.clone()),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        env.storage()
            .persistent()
            .set(&DataKey::ScoreAccumulator(entity.clone()), &accumulator);
        env.storage().persistent().extend_ttl(
            &DataKey::ScoreAccumulator(entity.clone()),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        env.events().publish(
            (symbol_short!("reput"), symbol_short!("score_dec")),
            decomposition,
        Ok(rep)
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod config_parity_test {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn get_config_matches_constructor_config() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let config = ReputationConfig {
            decay_window_seconds: 86_400,
            min_transactions_threshold: 5,
            dispute_penalty_bps: 250,
            freeze_threshold_flags: 3,
        };

        let contract_id = env.register(ReputationContract, (admin, config.clone()));

        let stored: ReputationConfig = env.invoke_contract(
            &contract_id,
            &Symbol::new(&env, "get_config"),
            soroban_sdk::vec![&env],
        );

        assert_eq!(stored, config);
    }
}
