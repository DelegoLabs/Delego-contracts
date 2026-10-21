//! Delego Delegation Registry Contract
//!
//! Manages delegated authority grants between owner accounts and agent
//! identifiers: creation, pause/resume, revocation (including nonce-based
//! key revocation), expiration sweeping, versioned snapshots with rollback,
//! admin transfer, and sub-accounts for micro-payment agents.
//!
//! # Error code allocation
//!
//! `DelegationError` codes are `u32` values surfaced over the bridge and are
//! allocated protocol-wide (issue #269); this contract owns `4_000..=4_999`.
//! See `DelegationError` for the current table.

// Contract crates compile as no_std for release and wasm builds, but keep std
// enabled during testing so dev-dependencies and test assertions operate normally.
// This exact conditional form must be consistent across all workspace contract crates.
#![cfg_attr(not(test), no_std)]
// `#![warn(missing_docs)]` is the intent, but soroban's `#[contracttype]`,
// `#[contracterror]`, `#[contract]`, and `#[contractimpl]` proc-macros emit
// undocumented statics (`__SPEC_XDR_TYPE_*`) and associated functions
// (`spec_xdr()`, client wrappers) that we cannot annotate from user code.
// Rustc attributes the resulting lint violations to the macro call site, so
// a per-item `#[allow]` cannot suppress them — only a crate-wide allow or a
// module-level allow can. We therefore suppress the lint here and enforce
// documentation discipline via code review and the explicit doc comments that
// every public constant, type, and function carries in this file.
#![allow(missing_docs)]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env,
    Symbol, Vec,
};

/// Persistent entries are bumped when they approach expiry and kept alive
/// for roughly 30 days, matching the repository's persistent-storage policy.
const PERSISTENT_BUMP_THRESHOLD: u32 = 17_280;
/// Target TTL (in ledgers) applied when a persistent entry is bumped.
///
/// 518 400 ledgers ≈ 30 days at 5 s/ledger.  Bumping to this value every
/// time the threshold is crossed keeps entries alive for a rolling 30-day
/// window without paying for a longer TTL than necessary.
const PERSISTENT_BUMP_AMOUNT: u32 = 518_400;
/// Maximum number of delegation ids accepted in a single `sweep_expired` call.
///
/// Batches larger than this — or empty batches — are rejected with
/// [`DelegationError::InvalidBatchSize`] so one transaction can never sweep
/// an unbounded number of storage entries.
pub const MAX_SWEEP_BATCH_SIZE: u32 = 50;

// ── Keeper Bounty constants ────────────────────────────────────────────────────

/// A delegation whose storage TTL will drop below this many ledgers is eligible
/// for a keeper bounty.  50 000 ledgers ≈ 3 days at ~5 s/ledger.
pub const KEEPER_BOUNTY_THRESHOLD_LEDGERS: u32 = 50_000;

/// Fixed XLM reward paid to the keeper per successful TTL bump, denominated in
/// stroops (1 XLM = 10 000 000 stroops).  10 000 000 stroops = 1 XLM.
pub const KEEPER_BOUNTY_AMOUNT_STROOPS: i128 = 10_000_000;

/// Minimum ledgers between two successive bounty claims for the same delegation.
/// Prevents a keeper from claiming the bounty on every block once the delegation
/// re-enters the eligibility window after a bump.  17 280 ledgers ≈ 1 day.
pub const KEEPER_BOUNTY_COOLDOWN_LEDGERS: u32 = 17_280;
/// Estimated stroops of storage rent reclaimed per auxiliary entry removed
/// when a delegation is swept. The protocol reclaims rent on storage removal;
/// contracts cannot pay it out directly, so this is an accounting estimate
/// keeper bots can use to price their service.
const RENT_RECLAIMED_PER_ENTRY_STROOPS: i128 = 1_000;

/// Represents the lifecycle status of a delegation.
/// Contract version information for deployment scripts and runtime compatibility checks.
// `#[contracttype]` generates a `pub static __SPEC_XDR_TYPE_*` and a
// `pub const fn spec_xdr()` that cannot carry source doc comments; the
// scoped allow silences missing-docs for those generated items only.
#[contracttype]
#[allow(missing_docs)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractVersion {
    /// Contract name, e.g. `deleg_reg`.
    pub name: Symbol,
    /// Contract semantic version, e.g. `0_0_1`.
    pub semver: Symbol,
}

/// Represents the lifecycle status of a delegation.
#[contracttype]
#[allow(missing_docs)]
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
#[allow(missing_docs)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
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
#[allow(missing_docs)]
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
    /// Nonce for key revocation tracking - incrementing this invalidates all prior authorizations.
    pub nonce: u64,
}

/// A sub-account record for dedicated micro-payment agents.
#[contracttype]
#[allow(missing_docs)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubAccountRecord {
    /// Unique identifier for the sub-account.
    pub id: u64,
    /// Address of the sub-account owner.
    pub owner: Address,
    /// Virtual address for the sub-account.
    pub sub_account_address: Address,
    /// Associated delegation id.
    pub delegation_id: u64,
    /// Human-readable label for the sub-account.
    pub label: Symbol,
    /// Ledger timestamp when the sub-account was created.
    pub created_at: u64,
    /// Whether the sub-account is active.
    pub is_active: bool,
}

/// A point-in-time snapshot of a delegation.
#[contracttype]
#[allow(missing_docs)]
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
#[allow(missing_docs)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationPage {
    /// Items on this page.
    pub items: Vec<DelegationRecord>,
    /// Total count across all pages.
    pub total: u32,
    /// Next offset for pagination, or None if no more pages.
    pub next_offset: Option<u32>,
}

/// Result of a batch sweep of expired delegations.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SweepResult {
    /// Number of delegations swept in this batch.
    pub swept_records_count: u32,
    /// Next id to resume scanning from, or None when no records remain.
    pub next_cursor_id: Option<u64>,
    /// Estimated stroops of storage rent reclaimed by removing auxiliary keys.
    pub rent_reclaimed_stroops: i128,
}

// ── Events ────────────────────────────────────────────────────────────────────

/// Emitted when a new delegation is created.
#[contracttype]
#[allow(missing_docs)]
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
#[allow(missing_docs)]
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
#[allow(missing_docs)]
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
#[allow(missing_docs)]
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
#[allow(missing_docs)]
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
#[allow(missing_docs)]
#[derive(Clone, Debug)]
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

/// Emitted when a delegation key is revoked via nonce invalidation.
#[contracttype]
#[allow(missing_docs)]
#[derive(Clone, Debug)]
pub struct DelegationKeyRevokedEvent {
    /// Unique delegation identifier.
    pub delegation_id: u64,
    /// Address of the delegation owner.
    pub owner: Address,
    /// Identifier of the associated agent.
    pub agent: BytesN<32>,
    /// New nonce value after revocation.
    pub new_nonce: u64,
    /// Ledger timestamp of the event.
    pub timestamp: u64,
}

/// Emitted when a sub-account is created.
#[contracttype]
#[allow(missing_docs)]
#[derive(Clone, Debug)]
pub struct SubAccountCreatedEvent {
    /// Unique sub-account identifier.
    pub sub_account_id: u64,
    /// Address of the sub-account owner.
    pub owner: Address,
    /// Virtual address for the sub-account.
    pub sub_account_address: Address,
    /// Associated delegation id.
    pub delegation_id: u64,
    /// Ledger timestamp of the event.
    pub timestamp: u64,
}

/// Emitted when a sub-account is deactivated.
#[contracttype]
#[allow(missing_docs)]
#[derive(Clone, Debug)]
pub struct SubAccountDeactivatedEvent {
    /// Unique sub-account identifier.
    pub sub_account_id: u64,
    /// Address of the sub-account owner.
    pub owner: Address,
    /// Virtual address for the sub-account.
    pub sub_account_address: Address,
    /// Ledger timestamp of the event.
    pub timestamp: u64,
}

/// Emitted when the current admin proposes a successor.
#[contracttype]
#[allow(missing_docs)]
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
#[allow(missing_docs)]
#[derive(Clone, Debug)]
pub struct AdminTransferredEvent {
    /// Admin that held the role before the transfer.
    pub previous_admin: Address,
    /// Address that is now the admin.
    pub new_admin: Address,
    /// Ledger timestamp of the event.
    pub timestamp: u64,
}

/// Snapshot of a delegation's keeper bounty eligibility.
///
/// Returned by [`get_keeper_bounty_status`](DelegationRegistry::get_keeper_bounty_status)
/// so off-chain keepers can decide whether to submit a heartbeat transaction.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeeperBountyStatus {
    /// The delegation this status describes.
    pub delegation_id: u64,
    /// Ledgers remaining before the delegation's storage entry is archived.
    /// Derived from the Soroban persistent-storage TTL, not `expires_at_ledger`.
    pub remaining_ledgers: u32,
    /// `true` when `remaining_ledgers` is below [`KEEPER_BOUNTY_THRESHOLD_LEDGERS`]
    /// and the delegation is `Active` (not revoked, expired, or paused).
    pub is_eligible_for_bounty: bool,
    /// Fixed XLM reward paid to the keeper, in stroops.
    pub bounty_amount_stroops: i128,
}

/// Emitted when a keeper successfully extends a delegation's TTL and claims
/// their bounty reward.
#[contracttype]
#[derive(Clone, Debug)]
pub struct KeeperBountyPaidEvent {
    /// The delegation whose TTL was extended.
    pub delegation_id: u64,
    /// Address of the keeper that performed the heartbeat.
    pub keeper: Address,
    /// Reward transferred to the keeper, in stroops.
    pub bounty_amount_stroops: i128,
    /// Ledger sequence at which the bump occurred.
    pub ledger: u32,
    /// Ledger timestamp at which the bump occurred.
    pub timestamp: u64,
}

/// Emitted when an operation is attempted with an expired session.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionExpiredEvent {
    /// Expiry ledger bound of the session.
    pub expiry_ledger: u32,
    /// Current ledger sequence when validation failed.
    pub current_ledger: u32,
    /// Wall-clock ledger timestamp when validation was attempted.
    pub timestamp: u64,
}

/// Ephemeral session bounds defining validity ledgers and spending capacity.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EphemeralSessionBounds {
    /// Earliest valid ledger sequence (inclusive).
    pub start_ledger: u32,
    /// Latest valid ledger sequence (inclusive).
    pub expiry_ledger: u32,
    /// Maximum spending limit allowed during the session.
    pub max_spending_limit: i128,
}

/// Topic 0 symbol for every event emitted by the delegation registry.
pub const EVENT_TOPIC_CONTRACT: Symbol = symbol_short!("deleg");

const ZERO_ACCOUNT_STRKEY: &str = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";
const ZERO_CONTRACT_STRKEY: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";

/// Returns `true` for the all-zero account or contract address, which no
/// one can sign for. Mirrors the guard in the escrow contract.
fn is_zero_address(env: &Env, address: &Address) -> bool {
    address == &Address::from_str(env, ZERO_ACCOUNT_STRKEY)
        || address == &Address::from_str(env, ZERO_CONTRACT_STRKEY)
}

// ── Storage keys ──────────────────────────────────────────────────────────────

/// Storage keys used by the delegation registry.
#[contracttype]
#[allow(missing_docs)]
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
    /// Nonce for delegation key revocation tracking.
    DelegationNonce(u64),
    /// Sub-account record for dedicated micro-payment agents.
    SubAccount(Address, u64),
    /// Next sub-account id to issue for an owner.
    NextSubAccountId(Address),
    /// Sub-account ids associated with an owner.
    OwnerSubAccounts(Address),
    /// Total XLM (in stroops) held in the protocol maintenance reserve for keeper bounties.
    MaintenanceReserve,
    /// Last ledger sequence at which a keeper bounty was paid for a delegation.
    /// Used to enforce the per-delegation cooldown between consecutive claims.
    KeeperLastBountyLedger(u64),
    /// Legacy (V1) delegation record keyed by owner address.
    DelegationV1(Address),
    /// Current (V2) delegation record keyed by owner address and label.
    DelegationV2(Address, Symbol),
    /// Schema version marker for the delegation storage layout.
    SchemaVersion,
}

/// Current delegation storage schema version.
pub const DELEGATION_SCHEMA_VERSION: u32 = 2;

/// Legacy V1 delegation record layout, kept only so pre-upgrade entries can be
/// read and lazily migrated to [`DelegationRecord`] (issue #574).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationRecordV1 {
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
    /// Capabilities granted to the delegate, packed into one word.
    pub permissions: PermissionBitmask,
    /// Nonce for key revocation tracking.
    pub nonce: u64,
}

impl DelegationRecordV1 {
    /// Converts a legacy V1 record into the current V2 schema.
    pub fn into_v2(self) -> DelegationRecord {
        DelegationRecord {
            id: self.id,
            owner: self.owner,
            agent_id: self.agent_id,
            permissions_contract: self.permissions_contract,
            status: self.status,
            label: self.label,
            created_at: self.created_at,
            updated_at: self.updated_at,
            expires_at_ledger: self.expires_at_ledger,
            version: self.version,
            permissions: self.permissions,
            nonce: self.nonce,
        }
    }
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
/// `DelegationError` currently occupies codes 4_001..=4_020.
/// New variants must use the next unused code within 4_000..=4_999.
#[contracterror]
#[allow(missing_docs)]
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
    /// The sub-account was not found.
    SubAccountNotFound = 4017,
    /// The sub-account is not active.
    SubAccountNotActive = 4018,
    /// The delegation does not match the sub-account.
    DelegationMismatch = 4019,
    /// The delegation is not eligible for a keeper bounty at this ledger.
    NotEligibleForBounty = 4020,
    /// The protocol maintenance reserve has insufficient funds to pay the keeper bounty.
    InsufficientMaintenanceReserve = 4021,
    /// The session has expired or is not yet valid.
    SessionExpired = 4020,
    /// A supplied parameter is invalid, e.g. a zero `owner` or `permissions_contract` address.
    InvalidParam = 4020,
}

/// The delegation registry contract.
#[contract]
#[allow(missing_docs)]
pub struct DelegationRegistry;

#[contractimpl]
#[allow(missing_docs)]
impl DelegationRegistry {
    /// Returns the contract name and semantic version.
    /// Callable without authentication — safe for off-chain tooling.
    pub fn version(_env: Env) -> ContractVersion {
        ContractVersion {
            name: symbol_short!("deleg_reg"),
            semver: symbol_short!("0_0_1"),
        }
    }

    /// Initializes the registry, setting `admin` and seeding the delegation
    /// id counter at 1.
    ///
    /// # Arguments
    ///
    /// * `env` - The contract environment.
    /// * `admin` - Address that gains admin rights (proposals, admin
    ///   revokes/pauses/resumes, and admin transfer).
    ///
    /// # Returns
    ///
    /// `Ok(true)` on the first successful call.
    ///
    /// # Errors
    ///
    /// * [`DelegationError::AlreadyInitialized`] - an admin address is already
    ///   stored; the call has no effect and never reassigns the admin.
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

        // A zero owner or permissions contract would pin the delegation to an
        // address nobody can sign for or invoke.
        if is_zero_address(&env, &owner) || is_zero_address(&env, &permissions_contract) {
            return Err(DelegationError::InvalidParam);
        }

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
            nonce: 0,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Delegation(id), &record);
        env.storage().persistent().extend_ttl(
            &DataKey::Delegation(id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Persist the record in the V2 schema as well and mark the schema
        // version so lazy migration can find it (issue #574).
        Self::write_delegation_v2(&env, &record);

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
        let mut record = Self::load_delegation_by_id(&env, delegation_id)
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
        Self::write_delegation_v2(&env, &record);

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
        let mut record = Self::load_delegation_by_id(&env, delegation_id)
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
        Self::write_delegation_v2(&env, &record);

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
        let mut record = Self::load_delegation_by_id(&env, delegation_id)
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
        Self::write_delegation_v2(&env, &record);

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

        let mut record = Self::load_delegation_by_id(&env, delegation_id)
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
        Self::write_delegation_v2(&env, &record);

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

        let mut record = Self::load_delegation_by_id(&env, delegation_id)
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
        Self::write_delegation_v2(&env, &record);

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

        let mut record = Self::load_delegation_by_id(&env, delegation_id)
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
        Self::write_delegation_v2(&env, &record);

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
            if let Some(record) = Self::load_delegation_by_id(&env, id) {
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
            if let Some(record) = Self::load_delegation_by_id(&env, id) {
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
        let mut record = Self::load_delegation_by_id(&env, delegation_id)
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
        Self::write_delegation_v2(&env, &record);

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
        let record = Self::load_delegation_by_id(&env, delegation_id)
            .ok_or(DelegationError::NotFound)?;

        Self::bump_delegation(&env, delegation_id, &record.owner);

        Ok(record)
    }

    /// Resolves a delegation by id, lazily migrating legacy V1 entries.
    ///
    /// The V1 layout is keyed by owner rather than id, so when the canonical
    /// record is absent the owner index is consulted to locate the legacy
    /// entry before falling back to a full scan of the id range.
    fn load_delegation_by_id(env: &Env, delegation_id: u64) -> Option<DelegationRecord> {
        if let Some(record) = env
            .storage()
            .persistent()
            .get::<_, DelegationRecord>(&DataKey::Delegation(delegation_id))
        {
            return Some(record);
        }

        let next_id: u64 = env.storage().instance().get(&DataKey::NextId).unwrap_or(1u64);
        let mut id = 1u64;
        while id < next_id {
            if let Some(record) = env
                .storage()
                .persistent()
                .get::<_, DelegationRecord>(&DataKey::Delegation(id))
            {
                if record.id == delegation_id {
                    return Some(record);
                }
            }
            id += 1;
        }

        None
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
            if let Some(record) = Self::load_delegation_by_id(&env, id) {
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
            if let Some(record) = Self::load_delegation_by_id(&env, id) {
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
        let record: DelegationRecord = match Self::load_delegation_by_id(&env, delegation_id) {
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

        let record: DelegationRecord = match Self::load_delegation_by_id(&env, delegation_id) {
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
        let record = Self::load_delegation_by_id(&env, delegation_id)
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

        let record = Self::load_delegation_by_id(&env, delegation_id)
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
        let mut record = Self::load_delegation_by_id(env, delegation_id)
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
        Self::write_delegation_v2(env, &record);

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

    /// Sweeps expired delegations in an iterative batch, returning a
    /// continuation cursor so keepers can page through the registry without
    /// exceeding the transaction budget.
    ///
    /// Walks delegation ids starting at `start_id`, transitioning up to
    /// `max_count` (capped at [`MAX_SWEEP_BATCH_SIZE`]) expired, non-terminal
    /// delegations into `Expired` status and removing their auxiliary storage
    /// keys (version, history, snapshots, nonce and owner index entry) to
    /// reclaim storage rent.
    ///
    /// Returns a [`SweepResult`] containing the number swept, the next cursor
    /// id to resume from (`None` once the id range is exhausted), and the
    /// estimated rent reclaimed in stroops. Callable by anyone; no
    /// authorization is required.
    pub fn sweep_expired_batch(env: Env, start_id: u64, max_count: u32) -> SweepResult {
        let max_count = max_count.min(MAX_SWEEP_BATCH_SIZE);
        if max_count == 0 {
            return SweepResult {
                swept_records_count: 0,
                next_cursor_id: None,
                rent_reclaimed_stroops: 0,
            };
        }

        let current_ledger = env.ledger().sequence();
        let next_id: u64 = env.storage().instance().get(&DataKey::NextId).unwrap_or(1);

        let mut swept_count: u32 = 0;
        let mut rent_reclaimed: i128 = 0;
        let mut id = start_id;

        while id < next_id && swept_count < max_count {
            if let Some(mut record) = env
                .storage()
                .persistent()
                .get::<_, DelegationRecord>(&DataKey::Delegation(id))
            {
                let already_terminal =
                    record.status == DelegationStatus::Expired
                        || record.status == DelegationStatus::Revoked;
                if !already_terminal && current_ledger >= record.expires_at_ledger {
                    record.status = DelegationStatus::Expired;
                    record.version = Self::increment_version(&env, id);
                    record.updated_at = env.ledger().timestamp();

                    env.storage().persistent().set(&DataKey::Delegation(id), &record);
                    Self::store_snapshot(&env, id, &record);

                    let entries_removed = Self::sweep_auxiliary_keys(&env, id);
                    rent_reclaimed += (entries_removed as i128) * RENT_RECLAIMED_PER_ENTRY_STROOPS;

                    Self::remove_from_user_delegations(&env, &record.owner, id);

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

                    swept_count += 1;
                }
            }
            id += 1;
        }

        // `id` is the first unexamined id once the loop exits. Resume from it
        // when the batch cap was reached; there is nothing left to resume from
        // when the id range is exhausted.
        let next_cursor_id = if id >= next_id { None } else { Some(id) };

        SweepResult {
            swept_records_count: swept_count,
            next_cursor_id,
            rent_reclaimed_stroops: rent_reclaimed,
        }
    }

    /// Removes the auxiliary storage keys belonging to a delegation (snapshot
    /// entries, version history, version counter, schema marker and nonce)
    /// and returns the number of entries removed, used to estimate reclaimed
    /// rent.
    fn sweep_auxiliary_keys(env: &Env, delegation_id: u64) -> u32 {
        let mut entries_removed: u32 = 0;

        let history: Vec<DelegationSnapshot> = env
            .storage()
            .persistent()
            .get(&DataKey::DelegationHistory(delegation_id))
            .unwrap_or(Vec::new(env));

        for snapshot in history.iter() {
            env.storage()
                .persistent()
                .remove(&DataKey::Snapshot(delegation_id, snapshot.version));
            entries_removed += 1;
        }

        env.storage()
            .persistent()
            .remove(&DataKey::DelegationHistory(delegation_id));
        entries_removed += 1;

        env.storage()
            .persistent()
            .remove(&DataKey::DelegationVersion(delegation_id));
        entries_removed += 1;

        env.storage()
            .persistent()
            .remove(&DataKey::SnapshotSchemaVersion(delegation_id));
        entries_removed += 1;

        if env.storage()
            .persistent()
            .has(&DataKey::DelegationNonce(delegation_id))
        {
            env.storage()
                .persistent()
                .remove(&DataKey::DelegationNonce(delegation_id));
            entries_removed += 1;
        }

        entries_removed
    }

    /// Removes `delegation_id` from the owner's `UserDelegations` index,
    /// dropping the index key entirely when no ids remain.
    fn remove_from_user_delegations(env: &Env, owner: &Address, delegation_id: u64) {
        let key = DataKey::UserDelegations(owner.clone());
        let ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(Vec::new(env));

        let mut kept = Vec::new(env);
        for stored_id in ids.iter() {
            if stored_id != delegation_id {
                kept.push_back(stored_id);
            }
        }

        // Always keep the index key (even when empty) so read paths that bump
        // its TTL (e.g. `get_delegation`) never hit a missing entry.
        env.storage().persistent().set(&key, &kept);
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
            if let Some(record) = Self::load_delegation_by_id(&env, id) {
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

    // ── Nonce-Invalidating Delegation Key Revocation ──────────────────────────

    /// Revokes a delegation key by incrementing its nonce, invalidating all
    /// prior authorizations without changing the delegation's lifecycle status.
    ///
    /// This provides a lightweight revocation mechanism that allows the owner
    /// to invalidate compromised keys while maintaining the delegation's
    /// configuration and history. The delegation remains in its current status
    /// (Active/Paused) but all authorization checks will fail until new keys
    /// are issued with the updated nonce.
    ///
    /// Returns the new nonce value.
    pub fn revoke_delegation_key(env: Env, delegation_id: u64) -> Result<u64, DelegationError> {
        let mut record: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
            .ok_or(DelegationError::NotFound)?;

        record.owner.require_auth();

        // Increment the nonce to invalidate all prior authorizations
        let new_nonce = record
            .nonce
            .checked_add(1)
            .ok_or(DelegationError::IdExhausted)?;
        record.nonce = new_nonce;
        record.version = Self::increment_version(&env, delegation_id);
        record.updated_at = env.ledger().timestamp();

        env.storage()
            .persistent()
            .set(&DataKey::Delegation(delegation_id), &record);

        // Store nonce separately for efficient lookup
        env.storage()
            .persistent()
            .set(&DataKey::DelegationNonce(delegation_id), &new_nonce);
        env.storage().persistent().extend_ttl(
            &DataKey::DelegationNonce(delegation_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        Self::store_snapshot(&env, delegation_id, &record);

        // Topics: (contract, action, entity_id) — entity_id is the delegation id.
        env.events().publish(
            (
                EVENT_TOPIC_CONTRACT,
                symbol_short!("key_rvk"),
                delegation_id,
            ),
            DelegationKeyRevokedEvent {
                delegation_id,
                owner: record.owner.clone(),
                agent: record.agent_id.clone(),
                new_nonce,
                timestamp: env.ledger().timestamp(),
            },
        );

        Ok(new_nonce)
    }

    /// Returns the current nonce for a delegation.
    ///
    /// The nonce tracks how many times the delegation key has been revoked.
    /// Authorization checks should verify that the presented nonce matches
    /// the current nonce stored on-chain.
    pub fn get_delegation_nonce(env: Env, delegation_id: u64) -> Result<u64, DelegationError> {
        let record = Self::load_delegation_by_id(&env, delegation_id)
            .ok_or(DelegationError::NotFound)?;

        Ok(record.nonce)
    }

    /// Returns whether the given agent is authorized for a delegation with the specified nonce.
    ///
    /// This extends the standard authorization check to include nonce verification,
    /// ensuring that only the current key generation is accepted.
    pub fn is_authorized_with_nonce(
        env: Env,
        delegation_id: u64,
        agent_id: BytesN<32>,
        nonce: u64,
    ) -> bool {
        let record: DelegationRecord = match Self::load_delegation_by_id(&env, delegation_id) {
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

        // Verify nonce matches current value
        if record.nonce != nonce {
            return false;
        }

        // Bump TTL on both the delegation record and its owner index so
        // active delegations stay alive while they're being queried.
        Self::bump_delegation(&env, delegation_id, &record.owner);

        true
    }

    /// Returns whether the agent is authorized for a specific permission flag
    /// with nonce verification.
    ///
    /// Combines permission flag checking with nonce validation for complete
    /// authorization verification.
    pub fn is_authorized_for_with_nonce(
        env: Env,
        delegation_id: u64,
        agent_id: BytesN<32>,
        flag: u32,
        nonce: u64,
    ) -> bool {
        if !PermissionBitmask::is_valid_flag(flag) {
            return false;
        }

        let record: DelegationRecord = match Self::load_delegation_by_id(&env, delegation_id) {
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

        // Verify nonce matches current value
        if record.nonce != nonce {
            return false;
        }

        // Bump TTL on both the delegation record and its owner index so
        // active delegations stay alive while they're being queried.
        Self::bump_delegation(&env, delegation_id, &record.owner);

        true
    }

    // ── Delegation Sub-Account Creation ───────────────────────────────────────

    /// Creates a sub-account for dedicated micro-payment agent operations.
    ///
    /// Sub-accounts provide isolated virtual addresses linked to a delegation,
    /// enabling fine-grained control over micro-payment flows without exposing
    /// the main account. Each sub-account is tied to a specific delegation and
    /// inherits its permissions and lifecycle.
    ///
    /// Returns the sub-account id.
    pub fn create_sub_account(
        env: Env,
        owner: Address,
        delegation_id: u64,
        label: Symbol,
    ) -> Result<u64, DelegationError> {
        owner.require_auth();

        // Verify the delegation exists and belongs to the owner
        let delegation: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
            .ok_or(DelegationError::NotFound)?;

        if delegation.owner != owner {
            return Err(DelegationError::NotAuthorized);
        }

        // Get or initialize the next sub-account id for this owner
        let next_id_key = DataKey::NextSubAccountId(owner.clone());
        let id = env.storage().persistent().get(&next_id_key).unwrap_or(1u64);
        let next_id = id.checked_add(1).ok_or(DelegationError::IdExhausted)?;
        env.storage().persistent().set(&next_id_key, &next_id);
        env.storage().persistent().extend_ttl(
            &next_id_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Create a deterministic sub-account address derived from owner and id
        // In practice, this could be a contract address or a derived key
        let sub_account_address = owner.clone(); // Placeholder - in production, derive a unique address

        let now = env.ledger().timestamp();
        let record = SubAccountRecord {
            id,
            owner: owner.clone(),
            sub_account_address: sub_account_address.clone(),
            delegation_id,
            label,
            created_at: now,
            is_active: true,
        };

        // Store the sub-account record
        let sub_account_key = DataKey::SubAccount(owner.clone(), id);
        env.storage().persistent().set(&sub_account_key, &record);
        env.storage().persistent().extend_ttl(
            &sub_account_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Add to owner's sub-account list
        let owner_sub_accounts_key = DataKey::OwnerSubAccounts(owner.clone());
        let mut sub_accounts = env
            .storage()
            .persistent()
            .get::<_, Vec<u64>>(&owner_sub_accounts_key)
            .unwrap_or(Vec::new(&env));
        sub_accounts.push_back(id);
        env.storage()
            .persistent()
            .set(&owner_sub_accounts_key, &sub_accounts);
        env.storage().persistent().extend_ttl(
            &owner_sub_accounts_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Bump the instance TTL to keep the contract alive
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);

        // Topics: (contract, action, entity_id) — entity_id is the sub-account id.
        env.events().publish(
            (EVENT_TOPIC_CONTRACT, symbol_short!("sub_crtd"), id),
            SubAccountCreatedEvent {
                sub_account_id: id,
                owner,
                sub_account_address,
                delegation_id,
                timestamp: now,
            },
        );

        Ok(id)
    }

    /// Deactivates a sub-account, preventing further operations.
    ///
    /// The sub-account record is preserved for audit purposes but marked as
    /// inactive. The owner can reactivate it later if needed.
    pub fn deactivate_sub_account(
        env: Env,
        owner: Address,
        sub_account_id: u64,
    ) -> Result<bool, DelegationError> {
        owner.require_auth();

        let sub_account_key = DataKey::SubAccount(owner.clone(), sub_account_id);
        let mut record: SubAccountRecord = env
            .storage()
            .persistent()
            .get(&sub_account_key)
            .ok_or(DelegationError::SubAccountNotFound)?;

        if record.owner != owner {
            return Err(DelegationError::NotAuthorized);
        }

        if !record.is_active {
            return Ok(false); // Already inactive
        }

        record.is_active = false;
        env.storage().persistent().set(&sub_account_key, &record);

        // Topics: (contract, action, entity_id) — entity_id is the sub-account id.
        env.events().publish(
            (
                EVENT_TOPIC_CONTRACT,
                symbol_short!("sub_deac"),
                sub_account_id,
            ),
            SubAccountDeactivatedEvent {
                sub_account_id,
                owner,
                sub_account_address: record.sub_account_address.clone(),
                timestamp: env.ledger().timestamp(),
            },
        );

        Ok(true)
    }

    /// Reactivates a previously deactivated sub-account.
    pub fn reactivate_sub_account(
        env: Env,
        owner: Address,
        sub_account_id: u64,
    ) -> Result<bool, DelegationError> {
        owner.require_auth();

        let sub_account_key = DataKey::SubAccount(owner.clone(), sub_account_id);
        let mut record: SubAccountRecord = env
            .storage()
            .persistent()
            .get(&sub_account_key)
            .ok_or(DelegationError::SubAccountNotFound)?;

        if record.owner != owner {
            return Err(DelegationError::NotAuthorized);
        }

        if record.is_active {
            return Ok(false); // Already active
        }

        // Verify the associated delegation is still valid
        let delegation: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(record.delegation_id))
            .ok_or(DelegationError::NotFound)?;

        if delegation.status == DelegationStatus::Revoked
            || delegation.status == DelegationStatus::Expired
        {
            return Err(DelegationError::DelegationMismatch);
        }

        record.is_active = true;
        env.storage().persistent().set(&sub_account_key, &record);

        Ok(true)
    }

    /// Returns a sub-account record by owner and id.
    pub fn get_sub_account(
        env: Env,
        owner: Address,
        sub_account_id: u64,
    ) -> Result<SubAccountRecord, DelegationError> {
        let sub_account_key = DataKey::SubAccount(owner.clone(), sub_account_id);
        let record: SubAccountRecord = env
            .storage()
            .persistent()
            .get(&sub_account_key)
            .ok_or(DelegationError::SubAccountNotFound)?;

        Ok(record)
    }

    /// Returns all sub-accounts owned by the given address.
    pub fn get_sub_accounts_by_owner(env: Env, owner: Address) -> Vec<SubAccountRecord> {
        let owner_sub_accounts_key = DataKey::OwnerSubAccounts(owner.clone());
        let sub_account_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&owner_sub_accounts_key)
            .unwrap_or(Vec::new(&env));

        let mut records = Vec::new(&env);
        for id in sub_account_ids.iter() {
            let sub_account_key = DataKey::SubAccount(owner.clone(), id);
            if let Some(record) = env
                .storage()
                .persistent()
                .get::<_, SubAccountRecord>(&sub_account_key)
            {
                records.push_back(record);
            }
        }
        records
    }

    /// Returns all active sub-accounts for an owner.
    pub fn get_active_sub_accounts(env: Env, owner: Address) -> Vec<SubAccountRecord> {
        let mut active = Vec::new(&env);
        for record in Self::get_sub_accounts_by_owner(env.clone(), owner).iter() {
            if record.is_active {
                // Verify the associated delegation is still valid
                if let Some(delegation) = env
                    .storage()
                    .persistent()
                    .get::<_, DelegationRecord>(&DataKey::Delegation(record.delegation_id))
                {
                    if delegation.status == DelegationStatus::Active
                        && env.ledger().sequence() < delegation.expires_at_ledger
                    {
                        active.push_back(record.clone());
                    }
                }
            }
        }
        active
    }

    /// Returns all sub-accounts associated with a specific delegation.
    pub fn get_sub_accounts_by_delegation(env: Env, delegation_id: u64) -> Vec<SubAccountRecord> {
        // First verify the delegation exists
        let delegation: DelegationRecord = match env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
        {
            Some(d) => d,
            None => return Vec::new(&env),
        };

        let owner = delegation.owner;
        let mut matching_records = Vec::new(&env);

        for record in Self::get_sub_accounts_by_owner(env.clone(), owner).iter() {
            if record.delegation_id == delegation_id {
                matching_records.push_back(record.clone());
            }
        }
        matching_records
    }

    // ── Keeper Bounty Heartbeat Incentive ─────────────────────────────────────

    /// Deposits XLM into the protocol maintenance reserve to fund keeper bounties.
    ///
    /// The caller transfers `amount_stroops` of native XLM to the contract via
    /// a standard SAC `transfer` call before invoking this function. This entry
    /// point records the deposit in the reserve ledger so `bump_delegation_ttl_with_reward`
    /// can validate solvency before every payout.
    ///
    /// The admin address must authorize this call to prevent accidental or
    /// malicious reserve inflation without a corresponding on-chain transfer.
    pub fn deposit_maintenance_reserve(
        env: Env,
        caller: Address,
        amount_stroops: i128,
    ) -> Result<i128, DelegationError> {
        caller.require_auth();

        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(DelegationError::NotInitialized)?;

        if caller != admin {
            return Err(DelegationError::NotAuthorized);
        }

        if amount_stroops <= 0 {
            return Err(DelegationError::InvalidTtl); // Reuse for "invalid amount"
        }

        let current: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MaintenanceReserve)
            .unwrap_or(0i128);

        let new_total = current
            .checked_add(amount_stroops)
            .ok_or(DelegationError::IdExhausted)?;

        env.storage()
            .instance()
            .set(&DataKey::MaintenanceReserve, &new_total);
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);

        Ok(new_total)
    }

    /// Returns the current balance of the protocol maintenance reserve, in stroops.
    pub fn get_maintenance_reserve(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::MaintenanceReserve)
            .unwrap_or(0i128)
    }

    /// Returns the keeper bounty eligibility status for a delegation.
    ///
    /// Off-chain keepers should call this view function to determine whether
    /// submitting a heartbeat transaction is currently profitable.
    ///
    /// `remaining_ledgers` reports the live Soroban persistent-storage TTL for
    /// the delegation record — not the application-level `expires_at_ledger` —
    /// so the field correctly tracks archival risk independent of the
    /// delegation's own expiry window.
    pub fn get_keeper_bounty_status(
        env: Env,
        delegation_id: u64,
    ) -> Result<KeeperBountyStatus, DelegationError> {
        let record: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
            .ok_or(DelegationError::NotFound)?;

        let current_ledger = env.ledger().sequence();

        // A delegation that has already passed its application-level expiry
        // or is in a terminal state must never be bumped by a keeper.
        let is_live = record.status == DelegationStatus::Active
            && current_ledger < record.expires_at_ledger;

        let delegation_key = DataKey::Delegation(delegation_id);
        let storage_ttl: u32 = env
            .storage()
            .persistent()
            .get_ttl(&delegation_key);

        // A keeper is eligible when:
        //   1. the delegation is live (Active and not yet expired)
        //   2. the storage TTL is approaching the archival threshold
        //   3. the per-delegation cooldown since the last bounty has elapsed
        let last_bounty_ledger: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::KeeperLastBountyLedger(delegation_id))
            .unwrap_or(0u32);

        let cooldown_elapsed = current_ledger
            .saturating_sub(last_bounty_ledger)
            >= KEEPER_BOUNTY_COOLDOWN_LEDGERS;

        let is_eligible = is_live
            && storage_ttl < KEEPER_BOUNTY_THRESHOLD_LEDGERS
            && cooldown_elapsed;

        Ok(KeeperBountyStatus {
            delegation_id,
            remaining_ledgers: storage_ttl,
            is_eligible_for_bounty: is_eligible,
            bounty_amount_stroops: KEEPER_BOUNTY_AMOUNT_STROOPS,
        })
    }

    /// Extends the Soroban persistent-storage TTL for an active delegation and
    /// transfers a fixed XLM bounty from the protocol maintenance reserve to the
    /// keeper as a reward for performing the heartbeat.
    ///
    /// ## Eligibility rules
    /// - The delegation must exist, be `Active`, and not yet past its
    ///   application-level `expires_at_ledger`.
    /// - The storage TTL of the delegation record must be below
    ///   [`KEEPER_BOUNTY_THRESHOLD_LEDGERS`] (≈ 3 days).
    /// - At least [`KEEPER_BOUNTY_COOLDOWN_LEDGERS`] (≈ 1 day) must have
    ///   elapsed since the last successful bounty claim for this delegation.
    /// - The maintenance reserve must hold at least
    ///   [`KEEPER_BOUNTY_AMOUNT_STROOPS`].
    ///
    /// ## What this function does
    /// 1. Validates all eligibility conditions, returning a typed error on any
    ///    failure so callers can handle each case explicitly.
    /// 2. Bumps the delegation record's storage TTL to the full
    ///    `PERSISTENT_BUMP_AMOUNT` window (≈ 30 days).
    /// 3. Deducts the bounty from the in-ledger maintenance reserve balance.
    /// 4. Transfers the XLM bounty to the keeper via the native Stellar Asset
    ///    Contract (SAC) identified by `xlm_token`.
    /// 5. Records the payout ledger for the per-delegation cooldown.
    /// 6. Emits a `KeeperBountyPaidEvent`.
    ///
    /// The keeper does **not** need to be the delegation owner; any third-party
    /// address may call this function once eligibility is met.
    pub fn bump_delegation_ttl_with_reward(
        env: Env,
        delegation_id: u64,
        keeper: Address,
        xlm_token: Address,
    ) -> Result<KeeperBountyStatus, DelegationError> {
        // The keeper must authorize the call so that nobody can redirect the
        // bounty payment to an address they do not control.
        keeper.require_auth();

        let record: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(delegation_id))
            .ok_or(DelegationError::NotFound)?;

        let current_ledger = env.ledger().sequence();

        // 1. The delegation must be live.
        if record.status != DelegationStatus::Active {
            return Err(DelegationError::NotActive);
        }
        if current_ledger >= record.expires_at_ledger {
            return Err(DelegationError::Expired);
        }

        // 2. The storage TTL must be approaching the archival threshold.
        let delegation_key = DataKey::Delegation(delegation_id);
        let storage_ttl: u32 = env
            .storage()
            .persistent()
            .get_ttl(&delegation_key);

        if storage_ttl >= KEEPER_BOUNTY_THRESHOLD_LEDGERS {
            return Err(DelegationError::NotEligibleForBounty);
        }

        // 3. Enforce per-delegation cooldown.
        let last_bounty_ledger: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::KeeperLastBountyLedger(delegation_id))
            .unwrap_or(0u32);

        let ledgers_since_last = current_ledger.saturating_sub(last_bounty_ledger);
        if ledgers_since_last < KEEPER_BOUNTY_COOLDOWN_LEDGERS {
            return Err(DelegationError::NotEligibleForBounty);
        }

        // 4. Verify reserve solvency.
        let reserve: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MaintenanceReserve)
            .unwrap_or(0i128);

        if reserve < KEEPER_BOUNTY_AMOUNT_STROOPS {
            return Err(DelegationError::InsufficientMaintenanceReserve);
        }

        // ── All checks passed — commit side-effects ────────────────────────

        // 5. Extend the delegation's persistent-storage TTL.
        env.storage().persistent().extend_ttl(
            &delegation_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        // Also refresh the owner's delegation index.
        env.storage().persistent().extend_ttl(
            &DataKey::UserDelegations(record.owner.clone()),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        // Keep the contract instance alive.
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);

        // 6. Deduct the bounty from the maintenance reserve.
        let new_reserve = reserve - KEEPER_BOUNTY_AMOUNT_STROOPS;
        env.storage()
            .instance()
            .set(&DataKey::MaintenanceReserve, &new_reserve);

        // 7. Transfer XLM bounty to the keeper via the native SAC.
        let token_client = soroban_sdk::token::Client::new(&env, &xlm_token);
        token_client.transfer(
            &env.current_contract_address(),
            &keeper,
            &KEEPER_BOUNTY_AMOUNT_STROOPS,
        );

        // 8. Record the payout ledger for cooldown enforcement.
        env.storage().persistent().set(
            &DataKey::KeeperLastBountyLedger(delegation_id),
            &current_ledger,
        );
        env.storage().persistent().extend_ttl(
            &DataKey::KeeperLastBountyLedger(delegation_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // 9. Emit event.
        let now = env.ledger().timestamp();
        env.events().publish(
            (
                EVENT_TOPIC_CONTRACT,
                symbol_short!("kpr_paid"),
                delegation_id,
            ),
            KeeperBountyPaidEvent {
                delegation_id,
                keeper: keeper.clone(),
                bounty_amount_stroops: KEEPER_BOUNTY_AMOUNT_STROOPS,
                ledger: current_ledger,
                timestamp: now,
            },
        );

        // Return the refreshed status (now ineligible due to cooldown reset).
        let refreshed_ttl: u32 = env
            .storage()
            .persistent()
            .get_ttl(&delegation_key);

        Ok(KeeperBountyStatus {
            delegation_id,
            remaining_ledgers: refreshed_ttl,
            is_eligible_for_bounty: false,
            bounty_amount_stroops: KEEPER_BOUNTY_AMOUNT_STROOPS,
        })
    }

    /// Extends the TTL on the delegation record and its owner-index key,
    /// keeping them alive in persistent storage. Called from read paths
    /// (`get_delegation`, `get_delegations_by_owner`, `is_authorized`) so
    /// active delegations are not evicted while still semantically live.
    fn bump_delegation(env: &Env, delegation_id: u64, owner: &Address) {        let delegation_key = DataKey::Delegation(delegation_id);
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

    /// Persists `record` under the V2 schema key and stamps the schema version.
    fn write_delegation_v2(env: &Env, record: &DelegationRecord) {
        let key = DataKey::DelegationV2(record.owner.clone(), record.label.clone());
        env.storage().persistent().set(&key, record);
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        let version_key = DataKey::SchemaVersion;
        env.storage()
            .persistent()
            .set(&version_key, &DELEGATION_SCHEMA_VERSION);
        env.storage().persistent().extend_ttl(
            &version_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
    }

    /// Lazily migrates a delegation record to the V2 schema (issue #574).
    ///
    /// Reads the V2 entry first; when it is missing, falls back to the legacy
    /// V1 entry keyed by owner, converts it to the current schema, writes it
    /// back under the V2 key, removes the V1 entry and returns the migrated
    /// record. Returns `None` when neither layout holds a record, so callers
    /// can surface `NotFound` without panicking.
    fn load_delegation(env: &Env, delegation_id: u64, owner: &Address) -> Option<DelegationRecord> {
        // Fast path: the canonical id-keyed record is already current.
        if let Some(record) = env
            .storage()
            .persistent()
            .get::<_, DelegationRecord>(&DataKey::Delegation(delegation_id))
        {
            return Some(record);
        }

        // V2 schema keyed by owner + label.
        let v1_key = DataKey::DelegationV1(owner.clone());
        let v1: Option<DelegationRecordV1> = env.storage().persistent().get(&v1_key);
        let v1 = v1?;

        if v1.id != delegation_id {
            return None;
        }

        let record = v1.into_v2();

        // Write the migrated record under both the canonical and V2 keys so
        // subsequent reads take the fast path, then drop the legacy entry.
        env.storage()
            .persistent()
            .set(&DataKey::Delegation(delegation_id), &record);
        env.storage().persistent().extend_ttl(
            &DataKey::Delegation(delegation_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        Self::write_delegation_v2(env, &record);
        env.storage().persistent().remove(&v1_key);

        Some(record)
    }

    /// Returns the current delegation storage schema version.
    pub fn get_schema_version(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::SchemaVersion)
            .unwrap_or(1u32)
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

    /// Validates an ephemeral session strictly against the current ledger sequence,
    /// enforcing `start_ledger <= current_ledger && current_ledger <= expiry_ledger`.
    ///
    /// If `current_ledger > bounds.expiry_ledger`, emits a [`SessionExpiredEvent`]
    /// before returning `Err(DelegationError::SessionExpired)`.
    /// If `current_ledger < bounds.start_ledger`, returns `Err(DelegationError::NotActive)`
    /// without emitting an expiration event.
    pub fn validate_session(
        env: Env,
        bounds: EphemeralSessionBounds,
    ) -> Result<bool, DelegationError> {
        let current_ledger = env.ledger().sequence();
        if current_ledger > bounds.expiry_ledger {
            env.events().publish(
                (EVENT_TOPIC_CONTRACT, symbol_short!("sess_exp")),
                SessionExpiredEvent {
                    expiry_ledger: bounds.expiry_ledger,
                    current_ledger,
                    timestamp: env.ledger().timestamp(),
                },
            );
            return Err(DelegationError::SessionExpired);
        }

        if current_ledger < bounds.start_ledger {
            return Err(DelegationError::NotActive);
        }

        Ok(true)
    }

    /// Dual check that strictly enforces Soroban ledger bounds for authorization
    /// while validating against an estimated or expected wall-clock timestamp fallback
    /// for UI-friendly display and user warnings.
    ///
    /// The ledger sequence check is definitive and immutable:
    /// - If `current_ledger > bounds.expiry_ledger`, emits [`SessionExpiredEvent`] and fails.
    /// - If `current_ledger < bounds.start_ledger`, fails with [`DelegationError::NotActive`].
    ///
    /// Returns `(is_valid, is_timestamp_valid)`. If the ledger bounds pass, `is_valid` is `true`.
    /// `is_timestamp_valid` indicates whether `expected_timestamp >= env.ledger().timestamp()`
    /// (or unexpired in wall-clock terms), allowing UI layers to detect skew or flag display warnings.
    pub fn validate_session_dual(
        env: Env,
        bounds: EphemeralSessionBounds,
        expected_timestamp: u64,
    ) -> Result<(bool, bool), DelegationError> {
        Self::validate_session(env.clone(), bounds)?;
        let current_time = env.ledger().timestamp();
        let is_timestamp_valid = expected_timestamp >= current_time;
        Ok((true, is_timestamp_valid))
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
            (DelegationError::SubAccountNotFound, 4017u32),
            (DelegationError::SubAccountNotActive, 4018u32),
            (DelegationError::DelegationMismatch, 4019u32),
            (DelegationError::NotEligibleForBounty, 4020u32),
            (DelegationError::InsufficientMaintenanceReserve, 4021u32),
            (DelegationError::SessionExpired, 4020u32),
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
            DelegationError::SubAccountNotFound as u32,
            DelegationError::SubAccountNotActive as u32,
            DelegationError::DelegationMismatch as u32,
            DelegationError::NotEligibleForBounty as u32,
            DelegationError::InsufficientMaintenanceReserve as u32,
            DelegationError::SessionExpired as u32,
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
