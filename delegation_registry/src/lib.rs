// Contract crates compile as no_std for release and wasm builds, but keep std
// enabled during testing so dev-dependencies and test assertions operate normally.
// This exact conditional form must be consistent across all workspace contract crates.
#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env,
    Symbol, Vec,
};

/// Persistent entries are bumped when they approach expiry and kept alive
/// for roughly 30 days, matching the repository's persistent-storage policy.
const PERSISTENT_BUMP_THRESHOLD: u32 = 17_280;
const PERSISTENT_BUMP_AMOUNT: u32 = 518_400;
pub const MAX_SWEEP_BATCH_SIZE: u32 = 50;

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

/// Capability bit: the delegate may move funds through the permissions contract.
pub const PERM_FLAG_SPEND: u32 = 1 << 0;
/// Capability bit: the delegate may claim refunds.
pub const PERM_FLAG_REFUND: u32 = 1 << 1;
/// Capability bit: the delegate may open/participate in disputes.
pub const PERM_FLAG_DISPUTE: u32 = 1 << 2;
/// Capability bit: the delegate may re-delegate its authority onwards.
pub const PERM_FLAG_DELEGATE: u32 = 1 << 3;

/// Every capability bit this contract defines. Bits outside this mask are
/// reserved and rejected by the mutating entry points.
pub const PERM_ALL_FLAGS: u32 =
    PERM_FLAG_SPEND | PERM_FLAG_REFUND | PERM_FLAG_DISPUTE | PERM_FLAG_DELEGATE;

/// A delegation's capability set packed into a single `u32` bitmask (issue #322).
///
/// The four capabilities a delegation can carry — spend, refund, dispute and
/// delegate — used to be modelled as four independent booleans. Each boolean
/// carries its own type discriminant and length prefix in the XDR encoding of
/// a [`DelegationRecord`], so a delegation paid for four times the flag
/// overhead it needed. Packing them into one `u32` replaces four encoded
/// fields with a single one and turns a permission check into one load plus a
/// bitwise test instead of four loads and four branches.
///
/// Bits are additive: setting a bit never clears another, and unknown bits
/// (outside [`PERM_ALL_FLAGS`]) are rejected so future capabilities can be
/// added without silently widening an existing delegation's scope.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub struct PermissionBitmask(pub u32);

impl PermissionBitmask {
    /// A bitmask with every currently defined capability enabled.
    pub const fn all() -> Self {
        Self(PERM_ALL_FLAGS)
    }

    /// A bitmask with no capability enabled.
    pub const fn none() -> Self {
        Self(0)
    }

    /// Returns the raw bit pattern.
    pub const fn bits(&self) -> u32 {
        self.0
    }

    /// Returns `true` when every capability in `flags` is set.
    ///
    /// `flags` may combine several bits; all of them must be present.
    pub const fn has_flag(&self, flags: u32) -> bool {
        self.0 & flags == flags
    }

    /// Returns `self` with every capability in `flags` set.
    pub const fn set_flag(&self, flags: u32) -> Self {
        Self(self.0 | flags)
    }

    /// Returns `self` with every capability in `flags` cleared.
    pub const fn clear_flag(&self, flags: u32) -> Self {
        Self(self.0 & !flags)
    }

    /// Returns `true` when no capability is set.
    pub const fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// Returns `true` when `flag` is a non-zero combination of known
    /// capability bits.
    ///
    /// Guards the mutating entry points against a zero mask (a no-op write that
    /// still costs a storage bump) and against reserved bits that this contract
    /// does not define.
    pub const fn is_valid_flag(flag: u32) -> bool {
        flag != 0 && flag & PERM_ALL_FLAGS == flag
    }
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
    /// Ledger timestamp of the last mutation to this delegation.
    pub updated_at: u64,
    /// Ledger sequence at which the delegation expires.
    pub expires_at_ledger: u32,
    /// Version number used for history/rollback.
    pub version: u32,
    /// Capabilities granted to the delegate, packed into one word (issue #322).
    pub permissions: PermissionBitmask,
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

/// A paginated page of delegation records.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationPage {
    /// Items on this page.
    pub items: Vec<DelegationRecord>,
    /// Total count across all pages.
    pub total: u32,
    /// Next offset for pagination, or None if no more pages.
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

/// Emitted when a delegation's capability bitmask changes (issue #322).
#[contracttype]
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct PermissionFlagsChangedEvent {
    /// Unique delegation identifier.
    pub delegation_id: u64,
    /// Address of the delegation owner.
    pub owner: Address,
    /// Identifier of the associated agent.
    pub agent: BytesN<32>,
    /// The delegation's full capability set after the change.
    pub permissions: PermissionBitmask,
    /// Ledger timestamp of the event.
    pub timestamp: u64,
}

/// Emitted when the current admin proposes a successor.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AdminProposedEvent {
    /// Admin that made the proposal.
    pub current_admin: Address,
    /// Address proposed to take over as admin.
    pub proposed_admin: Address,
    /// Ledger timestamp of the event.
    pub timestamp: u64,
}

/// Emitted when a proposed admin accepts the role and the transfer completes.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AdminTransferredEvent {
    /// Admin that held the role before the transfer.
    pub previous_admin: Address,
    /// Address that is now the admin.
    pub new_admin: Address,
    /// Ledger timestamp of the event.
    pub timestamp: u64,
}

/// Topic 0 symbol for every event emitted by the delegation registry.
pub const EVENT_TOPIC_CONTRACT: Symbol = symbol_short!("deleg");

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
    /// Snapshot for a delegation, stored separately by delegation id and version.
    Snapshot(u64, u32),
    /// Delegation ids associated with an owner.
    UserDelegations(Address),
    /// Current version for a delegation.
    DelegationVersion(u64),
    /// Version history for a delegation.
    DelegationHistory(u64),
    /// Snapshot key schema version used to lazily migrate legacy histories.
    SnapshotSchemaVersion(u64),
    /// Admin address proposed to take over, pending acceptance.
    ProposedAdmin,
}

/// Errors for delegation registry operations.
/// # Error code allocation
///
/// Error codes are surfaced over bridges and must be unique across contracts.
/// The following ranges are allocated protocol-wide and must not overlap (issue #269):
/// | Contract            | Reserved codes  |
/// | ------------------- | --------------- |
/// | `EscrowError`       | 1_000..=1_999   |
/// | `PermissionError`   | 2_000..=2_999   |
/// | `ReputationError`   | 3_000..=3_999   |
/// | `DelegationError`   | 4_000..=4_999   |
/// | `MarketplaceError`  | 5_000..=5_999   |
/// `DelegationError` currently occupies codes 4_001..=4_016.
/// New variants must use the next unused code within 4_000..=4_999.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum DelegationError {
    /// The delegation was not found.
    NotFound = 4001,
    /// The delegation is not active.
    NotActive = 4002,
    /// The delegation is not paused.
    NotPaused = 4003,
    /// The delegation has expired.
    Expired = 4004,
    /// The registry has already been initialized.
    AlreadyInitialized = 4005,
    /// The provided version is invalid.
    InvalidVersion = 4006,
    /// The target version is not lower than the current version.
    VersionNotLower = 4007,
    /// The requested snapshot was not found.
    SnapshotNotFound = 4008,
    /// The provided agent id is invalid.
    InvalidAgentId = 4009,
    /// No more delegation ids are available.
    IdExhausted = 4010,
    /// No admin transfer has been proposed.
    NoPendingAdmin = 4011,
    /// The registry has not been initialized yet.
    NotInitialized = 4012,
    /// The provided TTL is invalid (must be greater than 0).
    InvalidTtl = 4013,
    /// The caller is not authorized to perform admin operations.
    NotAuthorized = 4014,
    /// The supplied permission flag is not a single known capability bit.
    InvalidPermissionFlag = 4015,
    /// The sweep batch is empty or exceeds the maximum supported size.
    InvalidBatchSize = 4016,
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

    /// Proposes `new_admin` as the successor to the current admin.
    ///
    /// Only the current admin can propose: the stored admin address is loaded
    /// from instance storage and has to authorize the call, so nobody else can
    /// nominate a successor. The proposal stays pending until the proposed
    /// address calls `accept_admin`, so proposing an address the contract does
    /// not control transfers nothing on its own.
    pub fn propose_admin(env: Env, new_admin: Address) -> Result<bool, DelegationError> {
        let current_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(DelegationError::NotInitialized)?;

        current_admin.require_auth();

        env.storage()
            .instance()
            .set(&DataKey::ProposedAdmin, &new_admin);

        // Topics: (contract, action, entity_id) — entity_id is the proposed
        // admin address so indexers can filter admin proposals per address.
        env.events().publish(
            (
                EVENT_TOPIC_CONTRACT,
                symbol_short!("adm_prop"),
                new_admin.clone(),
            ),
            AdminProposedEvent {
                current_admin,
                proposed_admin: new_admin,
                timestamp: env.ledger().timestamp(),
            },
        );

        Ok(true)
    }

    /// Accepts a pending admin proposal, completing the two-step transfer.
    ///
    /// Only the proposed address can accept — it must authorize the call — and
    /// the pending proposal is cleared once the transfer lands. Returns the
    /// address that is now the admin.
    pub fn accept_admin(env: Env) -> Result<Address, DelegationError> {
        let proposed_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::ProposedAdmin)
            .ok_or(DelegationError::NoPendingAdmin)?;

        proposed_admin.require_auth();

        let previous_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(DelegationError::NotInitialized)?;

        env.storage()
            .instance()
            .set(&DataKey::Admin, &proposed_admin);
        env.storage().instance().remove(&DataKey::ProposedAdmin);

        // Topics: (contract, action, entity_id) — entity_id is the new admin.
        env.events().publish(
            (
                EVENT_TOPIC_CONTRACT,
                symbol_short!("adm_xfer"),
                proposed_admin.clone(),
            ),
            AdminTransferredEvent {
                previous_admin,
                new_admin: proposed_admin.clone(),
                timestamp: env.ledger().timestamp(),
            },
        );

        Ok(proposed_admin)
    }

    /// Creates a new delegation and returns its id.
    ///
    /// The delegation is created with every capability in
    /// [`PermissionBitmask::all()`] enabled. Use
    /// [`create_scoped_delegation`](Self::create_scoped_delegation)
    /// to grant a narrower set.
    pub fn create_delegation(
        env: Env,
        owner: Address,
        agent_id: BytesN<32>,
        permissions_contract: Address,
        label: Symbol,
        ttl_ledgers: u32,
    ) -> Result<u64, DelegationError> {
        Self::create_delegation_inner(
            env,
            owner,
            agent_id,
            permissions_contract,
            label,
            ttl_ledgers,
            PermissionBitmask::all(),
        )
    }

    /// Creates a new delegation whose delegate only holds `permissions`.
    ///
    /// Identical to [`create_delegation`](Self::create_delegation) except that
    /// the capability bitmask is supplied by the caller, so an owner can grant
    /// a single action (e.g. spend-only) instead of every capability. The mask
    /// is stored as one `u32` inside the delegation record rather than as
    /// separate boolean fields (issue #322).
    ///
    /// Returns [`DelegationError::InvalidPermissionFlag`] when the mask
    /// contains a reserved bit, since accepting one would let a delegation
    /// carry capabilities this contract cannot interpret.
    pub fn create_scoped_delegation(
        env: Env,
        owner: Address,
        agent_id: BytesN<32>,
        permissions_contract: Address,
        label: Symbol,
        ttl_ledgers: u32,
        permissions: PermissionBitmask,
    ) -> Result<u64, DelegationError> {
        if permissions.0 & !PERM_ALL_FLAGS != 0 {
            return Err(DelegationError::InvalidPermissionFlag);
        }

        Self::create_delegation_inner(
            env,
            owner,
            agent_id,
            permissions_contract,
            label,
            ttl_ledgers,
            permissions,
        )
    }

    fn create_delegation_inner(
        env: Env,
        owner: Address,
        agent_id: BytesN<32>,
        permissions_contract: Address,
        label: Symbol,
        ttl_ledgers: u32,
        permissions: PermissionBitmask,
    ) -> Result<u64, DelegationError> {
        owner.require_auth();

        if ttl_ledgers == 0 {
            return Err(DelegationError::InvalidTtl);
        }

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

        let expires_at_ledger = env
            .ledger()
            .sequence()
            .checked_add(ttl_ledgers)
            .ok_or(DelegationError::InvalidTtl)?;
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
            permissions,
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

        Self::store_snapshot(&env, id, &record);

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

        // Topics: (contract, action, entity_id) — entity_id is the delegation id.
        env.events().publish(
            (EVENT_TOPIC_CONTRACT, symbol_short!("created"), id),
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

        // Topics: (contract, action, entity_id) — entity_id is the delegation id.
        env.events().publish(
            (EVENT_TOPIC_CONTRACT, symbol_short!("paused"), delegation_id),
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
    ///
    /// The expiry check runs before any state is written: when the delegation has
    /// passed its `expires_at_ledger` the call returns `DelegationError::Expired`
    /// and the stored record is left exactly as it was (still `Paused`).
    /// Persisting the `Paused -> Expired` transition is `sweep_expired`'s job.
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

        // Fail fast: no version bump, no snapshot and no event when the call errors.
        if env.ledger().sequence() >= record.expires_at_ledger {
            return Err(DelegationError::Expired);
        }

        record.status = DelegationStatus::Active;
        record.version = Self::increment_version(&env, delegation_id);
        record.updated_at = env.ledger().timestamp();

        env.storage()
            .persistent()
            .set(&DataKey::Delegation(delegation_id), &record);

        Self::store_snapshot(&env, delegation_id, &record);

        // Topics: (contract, action, entity_id) — entity_id is the delegation id.
        env.events().publish(
            (
                EVENT_TOPIC_CONTRACT,
                symbol_short!("resumed"),
                delegation_id,
            ),
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

        // Topics: (contract, action, entity_id) — entity_id is the delegation id.
        env.events().publish(
            (
                EVENT_TOPIC_CONTRACT,
                symbol_short!("revoked"),
                delegation_id,
            ),
            DelegationRevokedEvent {
                delegation_id,
                owner: record.owner.clone(),
                agent: record.agent_id.clone(),
                timestamp: env.ledger().timestamp(),
            },
        );

        Ok(true)
    }

    /// Admin force-revokes a delegation regardless of owner consent.
    /// This is an emergency function for compromised delegations.
    ///
    /// Returns `Ok(true)` if the delegation transitioned to `Revoked`.
    /// Returns `Ok(false)` if the delegation was already `Revoked` (idempotent no-op).
    pub fn admin_revoke(
        env: Env,
        caller: Address,
        delegation_id: u64,
    ) -> Result<bool, DelegationError> {
        caller.require_auth();

        let admin = env
            .storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::Admin)
            .expect("Admin not set");

        if caller != admin {
            return Err(DelegationError::NotAuthorized);
        }

        let mut record: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
            .ok_or(DelegationError::NotFound)?;

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

        // Topics: (contract, action, entity_id) — entity_id is the delegation id.
        env.events().publish(
            (
                EVENT_TOPIC_CONTRACT,
                symbol_short!("revoked"),
                delegation_id,
            ),
            DelegationRevokedEvent {
                delegation_id,
                owner: record.owner.clone(),
                agent: record.agent_id.clone(),
                timestamp: env.ledger().timestamp(),
            },
        );

        Ok(true)
    }

    /// Admin force-pauses a delegation regardless of owner action (issue
    /// #285). Emergency counterpart to `pause_delegation`, mirroring
    /// `admin_revoke`'s authorization pattern: `caller` must require auth
    /// and must equal the configured admin, independent of `record.owner`.
    ///
    /// Returns `Ok(true)` if the delegation transitioned to `Paused`.
    /// Returns `Err(DelegationError::NotActive)` if it was not `Active`
    /// (mirrors `pause_delegation`'s own precondition).
    pub fn admin_pause_delegation(
        env: Env,
        caller: Address,
        delegation_id: u64,
    ) -> Result<bool, DelegationError> {
        caller.require_auth();

        let admin = env
            .storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::Admin)
            .expect("Admin not set");

        if caller != admin {
            return Err(DelegationError::NotAuthorized);
        }

        let mut record: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
            .ok_or(DelegationError::NotFound)?;

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

    /// Admin resumes a delegation it (or `pause_delegation`'s owner path)
    /// previously paused (issue #285). Emergency counterpart to
    /// `resume_delegation`, so an admin-initiated pause is not
    /// unrecoverable when the owner is unavailable. Same expiry precondition
    /// as `resume_delegation`: the stored record is left untouched (still
    /// `Paused`) if the delegation's expiry has already passed.
    pub fn admin_resume_delegation(
        env: Env,
        caller: Address,
        delegation_id: u64,
    ) -> Result<bool, DelegationError> {
        caller.require_auth();

        let admin = env
            .storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::Admin)
            .expect("Admin not set");

        if caller != admin {
            return Err(DelegationError::NotAuthorized);
        }

        let mut record: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
            .ok_or(DelegationError::NotFound)?;

        if record.status != DelegationStatus::Paused {
            return Err(DelegationError::NotPaused);
        }

        if env.ledger().sequence() >= record.expires_at_ledger {
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

    const MAX_PAGE_LIMIT: u32 = 100;

    /// Returns a page of the delegations owned by `owner`.
    ///
    /// `offset` is clamped to the number of delegations the owner has and
    /// `limit` is clamped to MAX_PAGE_LIMIT. `next_offset` is `None` once the
    /// final page has been returned.
    pub fn get_delegations_by_owner_paged(
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
    }

    /// Returns a page of the version history for `delegation_id`.
    ///
    /// `offset` is clamped to the history length and `limit` is clamped to
    /// MAX_PAGE_LIMIT; `next_offset` is `None` on the final page.
    pub fn get_delegation_history_paged(
        env: Env,
        delegation_id: u64,
        offset: u32,
        limit: u32,
    ) -> DelegationPage {
        let history: Vec<DelegationSnapshot> = env
            .storage()
            .persistent()
            .get(&DataKey::DelegationHistory(delegation_id))
            .unwrap_or(Vec::new(&env));

        let total = history.len() as u32;
        let limit = limit.min(Self::MAX_PAGE_LIMIT);
        let offset = offset.min(total);

        let mut items = Vec::new(&env);
        let start = offset;
        let end = offset.saturating_add(limit).min(total);
        let mut i = start;
        while i < end {
            let snapshot = history.get(i).unwrap();
            items.push_back(snapshot.record);
            i += 1;
        }

        let next_offset = if end < total { Some(end) } else { None };
        DelegationPage {
            items,
            total,
            next_offset,
        }
    }

    /// Returns a page of every delegation currently in `Expired` status.
    ///
    /// Walks the `1..NextId` id range, collects records whose stored status is
    /// `Expired`, then pages the result. `next_offset` is `None` on the final
    /// page.
    pub fn get_expired_delegations_paged(env: Env, offset: u32, limit: u32) -> DelegationPage {
        let current_ledger = env.ledger().sequence();
        let next_id: u64 = env.storage().instance().get(&DataKey::NextId).unwrap_or(1);
        let mut expired = Vec::new(&env);
        let mut id = 1u64;
        while id < next_id {
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
            id += 1;
        }

        let total = expired.len() as u32;
        let limit = limit.min(Self::MAX_PAGE_LIMIT);
        let offset = offset.min(total);

        let mut items = Vec::new(&env);
        let start = offset;
        let end = offset.saturating_add(limit).min(total);
        let mut i = start;
        while i < end {
            items.push_back(expired.get(i).unwrap());
            i += 1;
        }

        let next_offset = if end < total { Some(end) } else { None };
        DelegationPage {
            items,
            total,
            next_offset,
        }
    }

    /// Rolls a delegation back to a previous version.
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

        Self::migrate_snapshot_keys(&env, delegation_id);
        let snapshot: DelegationSnapshot = env
            .storage()
            .persistent()
            .get(&DataKey::Snapshot(delegation_id, target_version))
            .ok_or(DelegationError::SnapshotNotFound)?;

        if snapshot.record.permissions_contract != record.permissions_contract {
            return Err(DelegationError::InvalidVersion);
        }

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
            // Topics: (contract, action, entity_id) — entity_id is the delegation id.
            env.events().publish(
                (
                    EVENT_TOPIC_CONTRACT,
                    symbol_short!("expired"),
                    delegation_id,
                ),
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

    /// Returns all delegations associated with the given agent identifier.
    ///
    /// Iterates through all issued delegations and collects records matching
    /// `agent_id`.
    pub fn get_delegations_by_agent(env: Env, agent_id: BytesN<32>) -> Vec<DelegationRecord> {
        let next_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextId)
            .unwrap_or(1u64);

        let mut records = Vec::new(&env);
        for id in 1..next_id {
            if let Some(record) = env
                .storage()
                .persistent()
                .get::<_, DelegationRecord>(&DataKey::Delegation(id))
            {
                if record.agent_id == agent_id {
                    Self::bump_delegation(&env, id, &record.owner);
                    records.push_back(record);
                }
            }
        }
        records
    }

    /// Returns every delegation owned by `owner` whose stored status is
    /// `Active` and whose `expires_at_ledger` has not yet been reached.
    ///
    /// Paused, revoked, and expired delegations are excluded, and so are
    /// still-`Active` records whose expiry has already passed but which have
    /// not been swept yet. The result therefore matches the set of
    /// delegations that `is_authorized` treats as live, so clients can
    /// enumerate usable delegations without fetching and filtering every
    /// record themselves.
    pub fn get_active_delegations(env: Env, owner: Address) -> Vec<DelegationRecord> {
        let current_ledger = env.ledger().sequence();
        let mut active = Vec::new(&env);
        for record in Self::get_delegations_by_owner(env, owner).iter() {
            if record.status == DelegationStatus::Active
                && current_ledger < record.expires_at_ledger
            {
                active.push_back(record.clone());
            }
        }
        active
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

    /// Returns whether the agent is authorized for `flag` on a delegation.
    ///
    /// Same liveness and identity checks as [`is_authorized`](Self::is_authorized)
    /// — the delegation must be `Active`, unexpired, and registered to
    /// `agent_id` — plus a single bitwise test against the delegation's packed
    /// [`PermissionBitmask`]. Because the four capabilities share one word,
    /// answering this costs one comparison instead of the four boolean loads
    /// and branches an unpacked layout would need (issue #322).
    ///
    /// Returns `false` (never panics) when the delegation does not exist or
    /// `flag` is not a known capability bit, so callers can fail closed.
    pub fn is_authorized_for(
        env: Env,
        delegation_id: u64,
        agent_id: BytesN<32>,
        flag: u32,
    ) -> bool {
        if !PermissionBitmask::is_valid_flag(flag) {
            return false;
        }

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

        if !record.permissions.has_flag(flag) {
            return false;
        }

        // Bump TTL on both the delegation record and its owner index so
        // active delegations stay alive while they're being queried.
        Self::bump_delegation(&env, delegation_id, &record.owner);

        true
    }

    /// Returns a delegation's capability bitmask (issue #322).
    pub fn get_permissions(
        env: Env,
        delegation_id: u64,
    ) -> Result<PermissionBitmask, DelegationError> {
        let record: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
            .ok_or(DelegationError::NotFound)?;

        Ok(record.permissions)
    }

    /// Returns whether a delegation grants `flag` to its delegate.
    ///
    /// Unlike [`is_authorized_for`](Self::is_authorized_for) this ignores
    /// lifecycle state and expiry: it answers purely "does this delegation
    /// carry this capability", which is what scope-inspection UIs need.
    pub fn has_permission(
        env: Env,
        delegation_id: u64,
        flag: u32,
    ) -> Result<bool, DelegationError> {
        if !PermissionBitmask::is_valid_flag(flag) {
            return Err(DelegationError::InvalidPermissionFlag);
        }

        let record: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
            .ok_or(DelegationError::NotFound)?;

        Ok(record.permissions.has_flag(flag))
    }

    /// Sets `flag` on a delegation, requiring the owner to authorize.
    ///
    /// Other capabilities are left untouched. The delegation's version, history
    /// snapshot and `updated_at` are advanced so a scope change is visible in
    /// the audit trail and is reachable by `rollback_delegation`.
    ///
    /// Returns `Ok(true)` when the flag was newly set, `Ok(false)` when it was
    /// already set (no write, no version bump).
    pub fn set_permission_flag(
        env: Env,
        delegation_id: u64,
        flag: u32,
    ) -> Result<bool, DelegationError> {
        if !PermissionBitmask::is_valid_flag(flag) {
            return Err(DelegationError::InvalidPermissionFlag);
        }

        Self::update_permissions(&env, delegation_id, |mask| mask.set_flag(flag))
    }

    /// Clears `flag` on a delegation, requiring the owner to authorize.
    ///
    /// Mirrors [`set_permission_flag`](Self::set_permission_flag): the
    /// delegation's version, history snapshot and `updated_at` advance only
    /// when the flag actually changed.
    ///
    /// Returns `Ok(true)` when the flag was cleared, `Ok(false)` when it was
    /// already clear.
    pub fn clear_permission_flag(
        env: Env,
        delegation_id: u64,
        flag: u32,
    ) -> Result<bool, DelegationError> {
        if !PermissionBitmask::is_valid_flag(flag) {
            return Err(DelegationError::InvalidPermissionFlag);
        }

        Self::update_permissions(&env, delegation_id, |mask| mask.clear_flag(flag))
    }

    /// Applies `mutate` to a delegation's capability bitmask on behalf of its
    /// owner, skipping the write (and the version bump) when nothing changes.
    fn update_permissions(
        env: &Env,
        delegation_id: u64,
        mutate: impl FnOnce(PermissionBitmask) -> PermissionBitmask,
    ) -> Result<bool, DelegationError> {
        let mut record: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
            .ok_or(DelegationError::NotFound)?;

        record.owner.require_auth();

        let updated = mutate(record.permissions);
        if updated == record.permissions {
            return Ok(false);
        }

        record.permissions = updated;
        record.version = Self::increment_version(env, delegation_id);
        record.updated_at = env.ledger().timestamp();

        env.storage()
            .persistent()
            .set(&DataKey::Delegation(delegation_id), &record);

        Self::store_snapshot(env, delegation_id, &record);

        // Topics: (contract, action, entity_id) — entity_id is the delegation id.
        env.events().publish(
            (
                EVENT_TOPIC_CONTRACT,
                symbol_short!("perm_chg"),
                delegation_id,
            ),
            PermissionFlagsChangedEvent {
                delegation_id,
                owner: record.owner.clone(),
                agent: record.agent_id.clone(),
                permissions: record.permissions,
                timestamp: record.updated_at,
            },
        );

        Ok(true)
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
    pub fn sweep_expired(env: Env, delegation_ids: Vec<u64>) -> Result<Vec<u64>, DelegationError> {
        if delegation_ids.is_empty() || delegation_ids.len() > MAX_SWEEP_BATCH_SIZE {
            return Err(DelegationError::InvalidBatchSize);
        }

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

                    // Topics: (contract, action, entity_id) — entity_id is the delegation id.
                    env.events().publish(
                        (EVENT_TOPIC_CONTRACT, symbol_short!("expired"), id),
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

        Ok(swept)
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

    fn migrate_snapshot_keys(env: &Env, delegation_id: u64) {
        let schema_version: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::SnapshotSchemaVersion(delegation_id))
            .unwrap_or(0);
        if schema_version >= 1 {
            return;
        }

        let history: Vec<DelegationSnapshot> = env
            .storage()
            .persistent()
            .get(&DataKey::DelegationHistory(delegation_id))
            .unwrap_or(Vec::new(env));
        for snapshot in history.iter() {
            let key = DataKey::Snapshot(delegation_id, snapshot.version);
            if !env.storage().persistent().has(&key) {
                env.storage().persistent().set(&key, &snapshot);
            }
            env.storage().persistent().extend_ttl(
                &key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );
        }

        let version_key = DataKey::SnapshotSchemaVersion(delegation_id);
        env.storage().persistent().set(&version_key, &1u32);
        env.storage().persistent().extend_ttl(
            &version_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
    }

    fn store_snapshot(env: &Env, delegation_id: u64, record: &DelegationRecord) {
        Self::migrate_snapshot_keys(env, delegation_id);
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
        let snapshot_key = DataKey::Snapshot(delegation_id, version);
        env.storage().persistent().set(&snapshot_key, &snapshot);
        env.storage().persistent().extend_ttl(
            &snapshot_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        let mut history: Vec<DelegationSnapshot> = env
            .storage()
            .persistent()
            .get(&DataKey::DelegationHistory(delegation_id))
            .unwrap_or(Vec::new(&env));
        history.push_back(snapshot);
        env.storage()
            .persistent()
            .set(&DataKey::DelegationHistory(delegation_id), &history);
        env.storage().persistent().extend_ttl(
            &DataKey::DelegationHistory(delegation_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
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
            (DelegationError::NotFound, 4001u32),
            (DelegationError::NotActive, 4002u32),
            (DelegationError::NotPaused, 4003u32),
            (DelegationError::Expired, 4004u32),
            (DelegationError::AlreadyInitialized, 4005u32),
            (DelegationError::InvalidVersion, 4006u32),
            (DelegationError::VersionNotLower, 4007u32),
            (DelegationError::SnapshotNotFound, 4008u32),
            (DelegationError::InvalidAgentId, 4009u32),
            (DelegationError::IdExhausted, 4010u32),
            (DelegationError::NoPendingAdmin, 4011u32),
            (DelegationError::NotInitialized, 4012u32),
            (DelegationError::InvalidTtl, 4013u32),
            (DelegationError::NotAuthorized, 4014u32),
            (DelegationError::InvalidPermissionFlag, 4015u32),
            (DelegationError::InvalidBatchSize, 4016u32),
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
            DelegationError::IdExhausted as u32,
            DelegationError::NoPendingAdmin as u32,
            DelegationError::NotInitialized as u32,
            DelegationError::InvalidTtl as u32,
            DelegationError::NotAuthorized as u32,
            DelegationError::InvalidPermissionFlag as u32,
            DelegationError::InvalidBatchSize as u32,
        ];
        seen.sort_unstable();
        for pair in seen.windows(2) {
            assert_ne!(pair[0], pair[1], "duplicate DelegationError code");
        }
        for code in seen {
            assert!(
                (4000..=4999).contains(&code),
                "DelegationError code {code} is outside the reserved range"
            );
        }
    }
}
