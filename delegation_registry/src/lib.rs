// Contract crates compile as no_std for release and wasm builds, but keep std
// enabled during testing so dev-dependencies and test assertions operate normally.
// This exact conditional form must be consistent across all workspace contract crates.
#![cfg_attr(not(test), no_std)]

#![no_std]
#![warn(missing_docs)]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env,
    Symbol, Vec,
};

/// Persistent entries are bumped when they approach expiry and kept alive
/// for roughly 30 days, matching the repository's persistent-storage policy.
const PERSISTENT_BUMP_THRESHOLD: u32 = 17_280;
const PERSISTENT_BUMP_AMOUNT: u32 = 518_400;

/// Represents the lifecycle status of a delegation.
/// Contract version information for deployment scripts and runtime compatibility checks.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractVersion {
    pub name: Symbol,
    pub semver: Symbol,
}
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DelegationStatus {
    /// Delegation is pending activation.
    Pending,
    /// Delegation is active.
    Active,
    /// Delegation is paused.
    Paused,
    /// Delegation was revoked.
    Revoked,
    /// Delegation has expired.
    Expired,
}

/// A record representing a single delegation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationRecord {
    /// Unique identifier for the delegation.
    pub id: u64,
    /// Address of the delegation owner.
    pub owner: Address,
    /// Identifier of the authorized agent.
    pub agent_id: BytesN<32>,
    /// Contract address for which permissions are delegated.
    pub permissions_contract: Address,
    /// Current lifecycle status.
    pub status: DelegationStatus,
    /// Human-readable label for the delegation.
    pub label: Symbol,
    /// Ledger timestamp when the delegation was created.
    pub created_at: u64,
    pub updated_at: u64,
    /// Ledger sequence at which the delegation expires.
    pub expires_at_ledger: u32,
    /// Version number used for history/rollback.
    pub version: u32,
}

/// A point-in-time snapshot of a delegation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationSnapshot {
    /// Snapshot version.
    pub version: u32,
    /// Ledger sequence at which the snapshot was taken.
    pub snapshot_ledger: u32,
    /// The delegation record at this version.
    pub record: DelegationRecord,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationPage {
    pub items: Vec<DelegationRecord>,
    pub total: u32,
    pub next_offset: Option<u32>,
}

// ── Events ────────────────────────────────────────────────────────────────────

/// Emitted when a new delegation is created.
#[contracttype]
#[derive(Clone, Debug)]
pub struct DelegationCreatedEvent {
    /// Unique delegation identifier.
    pub delegation_id: u64,
    /// Address of the delegation owner.
    pub owner: Address,
    /// Identifier of the associated agent.
    pub agent: BytesN<32>,
    /// Ledger timestamp of the event.
    pub timestamp: u64,
}

/// Emitted when a delegation is paused.
#[contracttype]
#[derive(Clone, Debug)]
pub struct DelegationPausedEvent {
    /// Unique delegation identifier.
    pub delegation_id: u64,
    /// Address of the delegation owner.
    pub owner: Address,
    /// Identifier of the associated agent.
    pub agent: BytesN<32>,
    /// Ledger timestamp of the event.
    pub timestamp: u64,
}

/// Emitted when a delegation is resumed.
#[contracttype]
#[derive(Clone, Debug)]
pub struct DelegationResumedEvent {
    /// Unique delegation identifier.
    pub delegation_id: u64,
    /// Address of the delegation owner.
    pub owner: Address,
    /// Identifier of the associated agent.
    pub agent: BytesN<32>,
    /// Ledger timestamp of the event.
    pub timestamp: u64,
}

/// Emitted when a delegation is revoked.
#[contracttype]
#[derive(Clone, Debug)]
pub struct DelegationRevokedEvent {
    /// Unique delegation identifier.
    pub delegation_id: u64,
    /// Address of the delegation owner.
    pub owner: Address,
    /// Identifier of the associated agent.
    pub agent: BytesN<32>,
    /// Ledger timestamp of the event.
    pub timestamp: u64,
}

/// Emitted when a delegation transitions to Expired status.
#[contracttype]
#[derive(Clone, Debug)]
pub struct DelegationExpiredEvent {
    /// Unique delegation identifier.
    pub delegation_id: u64,
    /// Address of the delegation owner.
    pub owner: Address,
    /// Identifier of the associated agent.
    pub agent: BytesN<32>,
    /// Ledger timestamp of the event.
    pub timestamp: u64,
}

// ── Storage keys ──────────────────────────────────────────────────────────────

/// Storage keys used by the delegation registry.
#[contracttype]
pub enum DataKey {
    /// Address of the contract admin.
    Admin,
    /// Next delegation id to issue.
    NextId,
    /// Delegation record stored by id.
    Delegation(u64),
    /// Delegation ids associated with an owner.
    UserDelegations(Address),
    /// Current version for a delegation.
    DelegationVersion(u64),
    /// Version history for a delegation.
    DelegationHistory(u64),
}

/// Errors for delegation registry operations.
/// # Error code allocation
///
/// Error codes are surfaced over bridges and must be unique across contracts.
/// The following ranges are allocated protocol-wide and must not overlap:
/// | Contract | Reserved codes |
/// | --- | --- |
/// | `EscrowError` | 1..=100 |
/// | `PermissionError` | 101..=200 |
/// | `ReputationError` | 201..=300 |
/// | `DelegationError` | 301..=400 |
/// | `MarketplaceError` | 401..=500 |
/// `DelegationError` currently occupies the first nine codes in its range.
/// New variants must use the next unused code within 301..=400.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum DelegationError {
    /// The delegation was not found.
    NotFound = 301,
    /// The delegation is not active.
    NotActive = 302,
    /// The delegation is not paused.
    NotPaused = 303,
    /// The delegation has expired.
    Expired = 304,
    /// The registry has already been initialized.
    AlreadyInitialized = 305,
    /// The provided version is invalid.
    InvalidVersion = 306,
    /// The target version is not lower than the current version.
    VersionNotLower = 307,
    /// The requested snapshot was not found.
    SnapshotNotFound = 308,
    /// The provided agent id is invalid.
    InvalidAgentId = 309,
    /// No more delegation ids are available.
    IdExhausted = 310,
}

/// The delegation registry contract.
#[contract]
pub struct DelegationRegistry;

#[contractimpl]
impl DelegationRegistry {
    /// Initializes the registry with the admin address.
    /// Return the contract name and semantic version.
    /// Callable without authentication — safe for off-chain tooling.
    pub fn version(_env: Env) -> ContractVersion {
        ContractVersion {
            name: symbol_short!("deleg_reg"),
            semver: symbol_short!("0_0_1"),
        }
    }

    pub fn initialize(env: Env, admin: Address) -> Result<bool, DelegationError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(DelegationError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::NextId, &1u64);
        Ok(true)
    }

    /// Returns the configured admin address.
    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::Admin)
            .expect("Admin not set")
    }

    /// Creates a new delegation and returns its id.
    pub fn create_delegation(
        env: Env,
        owner: Address,
        agent_id: BytesN<32>,
        permissions_contract: Address,
        label: Symbol,
        ttl_ledgers: u32,
    ) -> Result<u64, DelegationError> {
        owner.require_auth();

        // Reject the all-zero sentinel agent id so authorization records
        // can never be seeded with a dead id, keeping is_authorized
        // failing closed.
        if agent_id == BytesN::from_array(&env, &[0u8; 32]) {
            return Err(DelegationError::InvalidAgentId);
        }

        let id = env
            .storage()
            .instance()
            .get(&DataKey::NextId)
            .unwrap_or(1u64);
        let next_id = id.checked_add(1).ok_or(DelegationError::IdExhausted)?;
        env.storage().instance().set(&DataKey::NextId, &next_id);

        let expires_at_ledger = env.ledger().sequence() + ttl_ledgers;
        let now = env.ledger().timestamp();

        let record = DelegationRecord {
            id,
            owner: owner.clone(),
            agent_id: agent_id.clone(),
            permissions_contract,
            status: DelegationStatus::Active,
            label,
            created_at: now,
            updated_at: now,
            expires_at_ledger,
            version: 1,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Delegation(id), &record);
        env.storage().persistent().extend_ttl(
            &DataKey::Delegation(id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Initialize version tracking
        env.storage()
            .persistent()
            .set(&DataKey::DelegationVersion(id), &1u32);

        // Store snapshot for version 1
        let snapshot = DelegationSnapshot {
            version: 1,
            snapshot_ledger: env.ledger().sequence(),
            record: record.clone(),
        };

        let mut history = env
            .storage()
            .persistent()
            .get::<_, Vec<DelegationSnapshot>>(&DataKey::DelegationHistory(id))
            .unwrap_or(Vec::new(&env));

        history.push_back(snapshot);
        env.storage()
            .persistent()
            .set(&DataKey::DelegationHistory(id), &history);
        env.storage().persistent().extend_ttl(
            &DataKey::DelegationHistory(id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        let mut user_dels = env
            .storage()
            .persistent()
            .get::<_, Vec<u64>>(&DataKey::UserDelegations(owner.clone()))
            .unwrap_or(Vec::new(&env));

        user_dels.push_back(id);
        env.storage()
            .persistent()
            .set(&DataKey::UserDelegations(owner.clone()), &user_dels);
        env.storage().persistent().extend_ttl(
            &DataKey::UserDelegations(owner.clone()),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Bump the instance TTL to keep the contract alive.
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);

        env.events().publish(
            (symbol_short!("deleg"), symbol_short!("created")),
            DelegationCreatedEvent {
                delegation_id: id,
                owner,
                agent: agent_id,
                timestamp: now,
            },
        );

        Ok(id)
    }

    /// Pauses an active delegation.
    pub fn pause_delegation(env: Env, delegation_id: u64) -> Result<bool, DelegationError> {
        let mut record: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
            .ok_or(DelegationError::NotFound)?;

        record.owner.require_auth();

        if record.status != DelegationStatus::Active {
            return Err(DelegationError::NotActive);
        }

        record.status = DelegationStatus::Paused;
        record.version = Self::increment_version(&env, delegation_id);
        record.updated_at = env.ledger().timestamp();

        env.storage()
            .persistent()
            .set(&DataKey::Delegation(delegation_id), &record);

        Self::store_snapshot(&env, delegation_id, &record);

        env.events().publish(
            (symbol_short!("deleg"), symbol_short!("paused")),
            DelegationPausedEvent {
                delegation_id,
                owner: record.owner.clone(),
                agent: record.agent_id.clone(),
                timestamp: env.ledger().timestamp(),
            },
        );

        Ok(true)
    }

    /// Resumes a paused delegation.
    pub fn resume_delegation(env: Env, delegation_id: u64) -> Result<bool, DelegationError> {
        let mut record: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
            .ok_or(DelegationError::NotFound)?;

        record.owner.require_auth();

        if record.status != DelegationStatus::Paused {
            return Err(DelegationError::NotPaused);
        }

        if env.ledger().sequence() >= record.expires_at_ledger {
            record.status = DelegationStatus::Expired;
            record.version = Self::increment_version(&env, delegation_id);
            record.updated_at = env.ledger().timestamp();
            env.storage()
                .persistent()
                .set(&DataKey::Delegation(delegation_id), &record);
            Self::store_snapshot(&env, delegation_id, &record);

            env.events().publish(
                (symbol_short!("deleg"), symbol_short!("expired")),
                DelegationExpiredEvent {
                    delegation_id,
                    owner: record.owner.clone(),
                    agent: record.agent_id.clone(),
                    timestamp: env.ledger().timestamp(),
                },
            );

            return Err(DelegationError::Expired);
        }

        record.status = DelegationStatus::Active;
        record.version = Self::increment_version(&env, delegation_id);
        record.updated_at = env.ledger().timestamp();

        env.storage()
            .persistent()
            .set(&DataKey::Delegation(delegation_id), &record);

        Self::store_snapshot(&env, delegation_id, &record);

        env.events().publish(
            (symbol_short!("deleg"), symbol_short!("resumed")),
            DelegationResumedEvent {
                delegation_id,
                owner: record.owner.clone(),
                agent: record.agent_id.clone(),
                timestamp: env.ledger().timestamp(),
            },
        );

        Ok(true)
    }

    /// Revokes an active or paused delegation.
    ///
    /// Returns `Ok(true)` if the delegation transitioned to `Revoked`.
    /// Returns `Ok(false)` if the delegation was already `Revoked` (idempotent no-op).
    /// Revokes a delegation.
    pub fn revoke_delegation(env: Env, delegation_id: u64) -> Result<bool, DelegationError> {
        let mut record: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
            .ok_or(DelegationError::NotFound)?;

        record.owner.require_auth();

        if record.status == DelegationStatus::Revoked {
            return Ok(false);
        }

        record.status = DelegationStatus::Revoked;
        record.version = Self::increment_version(&env, delegation_id);
        record.updated_at = env.ledger().timestamp();

        env.storage()
            .persistent()
            .set(&DataKey::Delegation(delegation_id), &record);

        Self::store_snapshot(&env, delegation_id, &record);

        env.events().publish(
            (symbol_short!("deleg"), symbol_short!("revoked")),
            DelegationRevokedEvent {
                delegation_id,
                owner: record.owner.clone(),
                agent: record.agent_id.clone(),
                timestamp: env.ledger().timestamp(),
            },
        );

        Ok(true)
    }

    /// Rolls a delegation back to a previous version.
    const MAX_PAGE_LIMIT: u32 = 100;

    pub fn get_delegations_by_owner_paginated(
        env: Env,
        owner: Address,
        offset: u32,
        limit: u32,
    ) -> DelegationPage {
        let user_dels: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::UserDelegations(owner))
            .unwrap_or(Vec::new(&env));
        let total = user_dels.len() as u32;
        let limit = limit.min(Self::MAX_PAGE_LIMIT);
        let offset = offset.min(total);
        let mut items = Vec::new(&env);
        let start = offset;
        let end = offset.saturating_add(limit).min(total);
        let mut i = start;
        while i < end {
            let id = user_dels.get(i).unwrap();
            if let Some(record) = env
                .storage()
                .persistent()
                .get::<_, DelegationRecord>(&DataKey::Delegation(id))
            {
                items.push_back(record);
            }
            i += 1;
        }
        let next_offset = if end < total { Some(end) } else { None };
        DelegationPage {
            items,
            total,
            next_offset,
    }
    pub fn get_delegation_history_paginated(
        delegation_id: u64,
        let history: Vec<DelegationSnapshot> = env
            .get(&DataKey::DelegationHistory(delegation_id))
        let total = history.len() as u32;
            let snapshot = history.get(i).unwrap();
            items.push_back(snapshot.record);
    pub fn get_expired_delegations_paginated(
        let next_id: u64 = env
            .instance()
            .get(&DataKey::NextId)
            .unwrap_or(1);
        let mut expired = Vec::new(&env);
        let mut id = 1u64;
        while id < next_id {
                if record.status == DelegationStatus::Expired {
                    expired.push_back(record);
                }
            id += 1;
        let total = expired.len() as u32;
            items.push_back(expired.get(i).unwrap());
    pub fn rollback_delegation(
        env: Env,
        delegation_id: u64,
        target_version: u32,
    ) -> Result<bool, DelegationError> {
        let mut record: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
            .ok_or(DelegationError::NotFound)?;

        record.owner.require_auth();

        if target_version < 1 {
            return Err(DelegationError::InvalidVersion);
        }

        let current_version: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::DelegationVersion(delegation_id))
            .unwrap_or(1);

        if target_version >= current_version {
            return Err(DelegationError::VersionNotLower);
        }

        let history: Vec<DelegationSnapshot> = env
            .storage()
            .persistent()
            .get(&DataKey::DelegationHistory(delegation_id))
            .unwrap_or(Vec::new(&env));

        let mut target_snapshot: Option<DelegationSnapshot> = None;
        for snapshot in history.iter() {
            if snapshot.version == target_version {
                target_snapshot = Some(snapshot);
                break;
            }
        }

        let snapshot = target_snapshot.ok_or(DelegationError::SnapshotNotFound)?;

        if snapshot.record.permissions_contract != record.permissions_contract {
            return Err(DelegationError::InvalidVersion);
        // Reject rollback to a snapshot whose delegation was already expired —
        // reviving an expired delegation via rollback must never be allowed.
        if snapshot.record.status == DelegationStatus::Expired {
            return Err(DelegationError::Expired);
        }

        record = snapshot.record;

        // Re-validate snapshot liveness on rollback: if the restored
        // snapshot's expiry has already passed, mark it Expired instead of
        // reviving a dead delegation across its original expiry.
        let expired_on_restore = env.ledger().sequence() >= record.expires_at_ledger;
        if expired_on_restore {
            record.status = DelegationStatus::Expired;
        }

        record.version = Self::increment_version(&env, delegation_id);
        record.updated_at = env.ledger().timestamp();

        env.storage()
            .persistent()
            .set(&DataKey::Delegation(delegation_id), &record);

        Self::store_snapshot(&env, delegation_id, &record);

        if expired_on_restore {
            env.events().publish(
                (symbol_short!("deleg"), symbol_short!("expired")),
                DelegationExpiredEvent {
                    delegation_id,
                    owner: record.owner.clone(),
                    agent: record.agent_id.clone(),
                    timestamp: env.ledger().timestamp(),
                },
            );
        }

        Ok(true)
    }

    /// Returns a delegation record by id.
    pub fn get_delegation(
        env: Env,
        delegation_id: u64,
    ) -> Result<DelegationRecord, DelegationError> {
        let record: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
            .ok_or(DelegationError::NotFound)?;

        Self::bump_delegation(&env, delegation_id, &record.owner);

        Ok(record)
    }

    /// Returns the current version for a delegation.
    pub fn get_delegation_version(env: Env, delegation_id: u64) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::DelegationVersion(delegation_id))
            .unwrap_or(1)
    }

    /// Returns the full version history for a delegation.
    pub fn get_delegation_history(env: Env, delegation_id: u64) -> Vec<DelegationSnapshot> {
        env.storage()
            .persistent()
            .get(&DataKey::DelegationHistory(delegation_id))
            .unwrap_or(Vec::new(&env))
    }

    /// Returns all delegations owned by the given address.
    pub fn get_delegations_by_owner(env: Env, owner: Address) -> Vec<DelegationRecord> {
        let user_dels_key = DataKey::UserDelegations(owner.clone());
        let user_dels: Vec<u64> = env
            .storage()
            .persistent()
            .get(&user_dels_key)
            .unwrap_or(Vec::new(&env));

        // Bump the index key so it stays alive alongside the records.
        if env.storage().persistent().has(&user_dels_key) {
            env.storage().persistent().extend_ttl(
                &user_dels_key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );
        }

        let mut records = Vec::new(&env);
        for id in user_dels.iter() {
            if let Some(record) = env
                .storage()
                .persistent()
                .get::<_, DelegationRecord>(&DataKey::Delegation(id))
            {
                Self::bump_delegation(&env, id, &record.owner);
                records.push_back(record);
            }
        }
        records
    }

    /// Returns whether the given agent is authorized for a delegation.
    pub fn is_authorized(env: Env, delegation_id: u64, agent_id: BytesN<32>) -> bool {
        let record: DelegationRecord = match env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
        {
            Some(r) => r,
            None => return false,
        };

        if record.status != DelegationStatus::Active {
            return false;
        }

        if env.ledger().sequence() >= record.expires_at_ledger {
            return false;
        }

        if record.agent_id != agent_id {
            return false;
        }

        // Bump TTL on both the delegation record and its owner index so
        // active delegations stay alive while they're being queried.
        Self::bump_delegation(&env, delegation_id, &record.owner);

        true
    }

    /// Sweeps a caller-supplied batch of delegation ids, transitioning any
    /// that have passed their `expires_at_ledger` into `Expired` status.
    ///
    /// Callable by anyone: it only advances delegations that have already
    /// expired according to on-chain state, so there is nothing to
    /// authorize. Ids that don't exist, aren't yet expired, or are already
    /// `Expired`/`Revoked` are silently skipped, making repeated sweeps of
    /// the same batch safe and gas-efficient.
    ///
    /// Returns the ids that were actually swept.
    pub fn sweep_expired(env: Env, delegation_ids: Vec<u64>) -> Vec<u64> {
        let current_ledger = env.ledger().sequence();
        let mut swept = Vec::new(&env);

        for id in delegation_ids.iter() {
            let key = DataKey::Delegation(id);
            if let Some(mut record) = env.storage().persistent().get::<_, DelegationRecord>(&key) {
                let already_terminal = record.status == DelegationStatus::Expired
                    || record.status == DelegationStatus::Revoked;
                if !already_terminal && current_ledger >= record.expires_at_ledger {
                    record.status = DelegationStatus::Expired;
                    record.version = Self::increment_version(&env, id);
                    record.updated_at = env.ledger().timestamp();
                    env.storage().persistent().set(&key, &record);

                    Self::store_snapshot(&env, id, &record);

                    env.events().publish(
                        (symbol_short!("deleg"), symbol_short!("expired")),
                        DelegationExpiredEvent {
                            delegation_id: id,
                            owner: record.owner.clone(),
                            agent: record.agent_id.clone(),
                            timestamp: env.ledger().timestamp(),
                        },
                    );

                    swept.push_back(id);
                }
            }
        }

        swept
    }

    /// Returns all delegations owned by `owner` that are currently expired.
    ///
    /// A delegation is considered expired here when the current ledger has
    /// passed `expires_at_ledger`, regardless of whether `sweep_expired` has
    /// already updated its stored status — this lets callers discover sweep
    /// candidates as well as already-swept delegations in one call.
    pub fn get_expired_delegations(env: Env, owner: Address) -> Vec<DelegationRecord> {
        let current_ledger = env.ledger().sequence();
        let user_dels = env
            .storage()
            .persistent()
            .get::<_, Vec<u64>>(&DataKey::UserDelegations(owner))
            .unwrap_or(Vec::new(&env));

        let mut expired = Vec::new(&env);
        for id in user_dels.iter() {
            if let Some(record) = env
                .storage()
                .persistent()
                .get::<_, DelegationRecord>(&DataKey::Delegation(id))
            {
                let is_expired = record.status == DelegationStatus::Expired
                    || (record.status != DelegationStatus::Revoked
                        && current_ledger >= record.expires_at_ledger);
                if is_expired {
                    expired.push_back(record);
                }
            }
        }
        expired
    }
    /// Extends the TTL on the delegation record and its owner-index key,
    /// keeping them alive in persistent storage. Called from read paths
    /// (`get_delegation`, `get_delegations_by_owner`, `is_authorized`) so
    /// active delegations are not evicted while still semantically live.
    fn bump_delegation(env: &Env, delegation_id: u64, owner: &Address) {
        let delegation_key = DataKey::Delegation(delegation_id);
        env.storage().persistent().extend_ttl(
            &delegation_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        let user_dels_key = DataKey::UserDelegations(owner.clone());
        env.storage().persistent().extend_ttl(
            &user_dels_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
    }

    fn increment_version(env: &Env, delegation_id: u64) -> u32 {
        let version: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::DelegationVersion(delegation_id))
            .unwrap_or(1);
        let new_version = version + 1;
        env.storage()
            .persistent()
            .set(&DataKey::DelegationVersion(delegation_id), &new_version);
        new_version
    }

    fn store_snapshot(env: &Env, delegation_id: u64, record: &DelegationRecord) {
        let version: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::DelegationVersion(delegation_id))
            .unwrap_or(1);
        let snapshot = DelegationSnapshot {
            version,
            snapshot_ledger: env.ledger().sequence(),
            record: record.clone(),
        };
        let mut history: Vec<DelegationSnapshot> = env
            .storage()
            .persistent()
            .get(&DataKey::DelegationHistory(delegation_id))
            .unwrap_or(Vec::new(env));
        history.push_back(snapshot);
        env.storage()
            .persistent()
            .set(&DataKey::DelegationHistory(delegation_id), &history);
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod error_code_uniqueness_tests {
    use super::DelegationError;

    #[test]
    fn delegation_error_codes_are_unique_and_allocated() {
        let codes = [
            (DelegationError::NotFound, 301u32),
            (DelegationError::NotActive, 302u32),
            (DelegationError::NotPaused, 303u32),
            (DelegationError::Expired, 304u32),
            (DelegationError::AlreadyInitialized, 305u32),
            (DelegationError::InvalidVersion, 306u32),
            (DelegationError::VersionNotLower, 307u32),
            (DelegationError::SnapshotNotFound, 308u32),
            (DelegationError::InvalidAgentId, 309u32),
        ];

        for (variant, expected) in codes {
            assert_eq!(variant as u32, expected, "numeric code changed");
        }

        let mut seen = [
            DelegationError::NotFound as u32,
            DelegationError::NotActive as u32,
            DelegationError::NotPaused as u32,
            DelegationError::Expired as u32,
            DelegationError::AlreadyInitialized as u32,
            DelegationError::InvalidVersion as u32,
            DelegationError::VersionNotLower as u32,
            DelegationError::SnapshotNotFound as u32,
            DelegationError::InvalidAgentId as u32,
        ];
        seen.sort_unstable();
        for pair in seen.windows(2) {
            assert_ne!(pair[0], pair[1], "duplicate DelegationError code");
        }
        for code in seen {
            assert!(
                (301..=400).contains(&code),
                "DelegationError code {} is outside the reserved range",
                code
            );
        }
    }
}
