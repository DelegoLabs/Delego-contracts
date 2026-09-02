//! Delego Permissions Contract
//! Spending limits, delegated authority, and time-locked allowance decrements
//!
//! # Error code allocation
//!
//! Numeric error codes are `u32` values surfaced over the bridge, so every
//! contract must own a disjoint numeric block. The current allocation is:
//!
//! | Contract          | Range      |
//! |-------------------|------------|
//! | `EscrowError`     | 1000-1999  |
//! | `PermissionError` | 2000-2999  |
//! | `ReputationError` | 3000-3999  |
//! | `DelegationError` | 4000-4999  |
//! | `MarketplaceError` | 5000-5999 |
//!
//! `PermissionError` keeps its historical status-code style by adding the
//! allocation base (`2000`) to each legacy value (e.g. `ParentNotFound`
//! moves from `404` to `2404`). The unit tests below enforce that every
//! `PermissionError` discriminant is inside the contract's range and that
//! the documented ranges are pairwise disjoint.

// Contract crates compile as no_std for release and wasm builds, but keep std
// enabled during testing so dev-dependencies and test assertions operate normally.
// This exact conditional form must be consistent across all workspace contract crates.
#![cfg_attr(not(test), no_std)]
#![allow(clippy::too_many_arguments)]
#![warn(missing_docs)]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, xdr::ToXdr, Address, BytesN,
    Env, Symbol, Vec,
};

const _PERM: Symbol = symbol_short!("PERM");
const _PENDING_DEC: Symbol = symbol_short!("PEND_DEC");

/// Contract name and semver for backend compatibility checks.
/// Soroban Symbol only allows [a-zA-Z0-9_], so hyphens/dots are replaced with underscores.
pub const CONTRACT_NAME: &str = "delego_perms";
pub const CONTRACT_SEMVER: &str = "0_1_0";

/// Maximum number of merchant addresses allowed in a permission's whitelist.
/// Prevents overly large merchant lists from increasing storage and execution
/// costs unexpectedly.
pub const MAX_MERCHANTS_PER_PERMISSION: u32 = 25;

/// Maximum number of entries retained in a single (owner, delegate) pair's
/// audit log. Once exceeded, the oldest entry is dropped on each append so
/// long-lived permissions don't accrue unbounded storage.
pub const MAX_AUDIT_ENTRIES: u32 = 200;
/// Upper bound on the velocity limit's minimum-spend-interval, in ledgers.
/// At ~5s per ledger this is roughly one year; anything above this would
/// effectively disable spending forever with no clear signal, so it is
/// rejected outright.
pub const MAX_VELOCITY_INTERVAL: u32 = 6_307_200;
/// Default allowance-decrease timelock in seconds (24 hours).
pub const DEFAULT_DECREASE_TIMELOCK_SECS: u64 = 86_400;
/// Maximum configurable allowance-decrease timelock (30 days).
pub const MAX_DECREASE_TIMELOCK_SECS: u64 = 2_592_000;
pub const MAX_SWEEP_BATCH: u32 = 50;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
/// Errors returned by permission operations.
#[allow(missing_docs)]
pub enum PermissionError {
    /// No permission record found for this owner/delegate pair
    PermissionNotFound = 2302,
    NotFound = 2001,
    /// Permission has expired
    Expired = 2002,
    /// Amount exceeds per-transaction limit
    ExceedsPerTxLimit = 2003,
    /// Amount exceeds remaining total allowance
    ExceedsTotalLimit = 2004,
    /// Merchant is not in the allowed merchants list
    MerchantNotAllowed = 2005,
    /// Caller is not authorized (not the owner)
    Unauthorized = 2006,
    /// Invalid parameter (zero limit, etc.)
    InvalidParam = 2007,
    /// Permission is currently paused
    PermissionPaused = 2008,
    /// Permission is already paused
    AlreadyPaused = 2009,
    /// Permission is already active
    AlreadyActive = 2010,
    /// New grants are globally paused by admin
    GrantsPaused = 2011,
    /// No relayer signing key registered for this delegate
    RelayerKeyNotSet = 2012,
    /// Relayer-submitted nonce does not match the delegate's expected next nonce
    InvalidNonce = 2013,
    /// Relayer-submitted signature has expired
    SignatureExpired = 2014,
    /// A live permission already exists; use `re_grant` to replace it explicitly (issue #51)
    AlreadyGranted = 2015,
    /// Owner and delegate cannot be the same address
    SelfDelegationNotAllowed = 2401,
    /// Fewer valid owner signatures were provided than the configured threshold
    InsufficientSignatures = 2402,
    /// Metadata schema is not in the approved schema registry
    UnknownSchema = 2403,
    /// Referenced parent permission was not found
    ParentNotFound = 2404,
    /// Child limits exceed what the parent permission can back
    ExceedsParentLimit = 2405,
    /// Spend rejected because the velocity (min interval) limit has not elapsed
    VelocityLimitExceeded = 2406,
    /// sweep_inactive called before admin has configured an inactivity threshold
    InactivityThresholdNotSet = 2407,
    /// A pending allowance decrease already exists for this delegation
    PendingDecreaseExists = 2408,
    /// Time-lock on pending allowance decrease has not elapsed yet
    TimeLockActive = 2409,
    /// Decrease would drop the allowance limit below what has already been spent
    LimitBelowSpent = 2410,
    /// A multi-owner spend accumulation would overflow or exceed the
    /// permission's total allowance
    ExceedsAllowance = 2411,
    /// A grant's `expires_at_ledger = ledger_sequence + ttl_ledgers`
    /// computation would overflow `u32`, so no valid expiry ledger can be
    /// represented. Returned instead of an arithmetic overflow panic.
    InvalidExpiry = 2412,
    /// Admin-gated call made before `set_admin` has ever been called
    NotInitialized = 2500,
}

#[cfg(test)]
mod error_code_tests {
    use super::PermissionError;

    const ERROR_CODE_RANGES: &[(&str, u32, u32)] = &[
        ("EscrowError", 1000, 1999),
        ("PermissionError", 2000, 2999),
        ("ReputationError", 3000, 3999),
        ("DelegationError", 4000, 4999),
        ("MarketplaceError", 5000, 5999),
    ];

    const PERMISSION_ERROR_CODES: &[u32] = &[
        PermissionError::PermissionNotFound as u32,
        PermissionError::NotFound as u32,
        PermissionError::Expired as u32,
        PermissionError::ExceedsPerTxLimit as u32,
        PermissionError::ExceedsTotalLimit as u32,
        PermissionError::MerchantNotAllowed as u32,
        PermissionError::Unauthorized as u32,
        PermissionError::InvalidParam as u32,
        PermissionError::PermissionPaused as u32,
        PermissionError::AlreadyPaused as u32,
        PermissionError::AlreadyActive as u32,
        PermissionError::GrantsPaused as u32,
        PermissionError::RelayerKeyNotSet as u32,
        PermissionError::InvalidNonce as u32,
        PermissionError::SignatureExpired as u32,
        PermissionError::AlreadyGranted as u32,
        PermissionError::SelfDelegationNotAllowed as u32,
        PermissionError::InsufficientSignatures as u32,
        PermissionError::UnknownSchema as u32,
        PermissionError::ParentNotFound as u32,
        PermissionError::ExceedsParentLimit as u32,
        PermissionError::VelocityLimitExceeded as u32,
        PermissionError::InactivityThresholdNotSet as u32,
        PermissionError::PendingDecreaseExists as u32,
        PermissionError::TimeLockActive as u32,
        PermissionError::LimitBelowSpent as u32,
        PermissionError::ExceedsAllowance as u32,
        PermissionError::NotInitialized as u32,
    ];

    #[test]
    fn permission_error_codes_are_unique_and_in_reserved_range() {
        let permission_range = ERROR_CODE_RANGES
            .iter()
            .find(|entry| entry.0 == "PermissionError")
            .expect("PermissionError range must be declared");
        let (start, end) = (permission_range.1, permission_range.2);

        for (i, code) in PERMISSION_ERROR_CODES.iter().enumerate() {
            assert!(
                (start..=end).contains(code),
                "PermissionError code {} is outside the allocated range {}-{}",
                code,
                start,
                end
            );
            assert!(
                !PERMISSION_ERROR_CODES[..i].contains(code),
                "duplicate PermissionError code {}",
                code
            );
        }
    }

    #[test]
    fn error_code_ranges_are_disjoint() {
        for (i, &(name_i, start_i, end_i)) in ERROR_CODE_RANGES.iter().enumerate() {
            for &(name_j, start_j, end_j) in ERROR_CODE_RANGES.iter().skip(i + 1) {
                assert!(
                    end_i < start_j || end_j < start_i,
                    "error code ranges overlap: {} ({}-{}) and {} ({}-{})",
                    name_i,
                    start_i,
                    end_i,
                    name_j,
                    start_j,
                    end_j
                );
            }
        }
    }
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
/// The current state of a permission.
#[allow(missing_docs)]
pub enum PermissionStatus {
    Active,
    Paused,
    Revoked,
    Expired,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
/// A record describing a delegated permission from an owner to a delegate.
#[allow(missing_docs)]
pub struct PermissionRecord {
    pub owner: Address,
    pub delegate: Address,
    pub limit_total: i128,
    pub spent: i128,
    pub limit_per_tx: i128,
    pub allowed_merchants: Vec<Address>,
    pub status: PermissionStatus,
    pub expires_at_ledger: u32,
    pub created_at: u64,
    /// Owner half of the parent permission's `(owner, delegate)` key, for
    /// permissions created via `grant_child`. `None` for top-level grants.
    pub parent_owner: Option<Address>,
    /// Delegate half of the parent permission's `(owner, delegate)` key,
    /// for permissions created via `grant_child`. `None` for top-level
    /// grants. Together with `parent_owner`, this forms the reference the
    /// issue describes as `parent_permission`.
    pub parent_delegate: Option<Address>,
}

/// A delegation permission jointly controlled by multiple owners (issue #326).
///
/// Spends require signatures from at least `threshold` of `owners`. Keyed in
/// storage by `(owners[0], delegate)` — see `DataKey::MultiPermission`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub struct MultiOwnerPermission {
    pub owners: Vec<Address>,
    pub threshold: u32,
    pub delegate: Address,
    pub limit_total: i128,
    pub spent: i128,
    pub limit_per_tx: i128,
    pub allowed_merchants: Vec<Address>,
    pub status: PermissionStatus,
    pub expires_at_ledger: u32,
    pub created_at: u64,
}

#[contracttype]
#[derive(Clone, Debug)]
/// Emitted when a multi-owner permission is granted.
#[allow(missing_docs)]
pub struct MultiOwnerGrantedEvent {
    pub primary_owner: Address,
    pub delegate: Address,
    pub owner_count: u32,
    pub threshold: u32,
    pub total_limit: i128,
}

/// Emitted after a multi-owner delegated spend is successfully recorded (issue #326).
#[contracttype]
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct MultiOwnerSpendEvent {
    pub primary_owner: Address,
    pub delegate: Address,
    pub merchant: Address,
    pub amount: i128,
    pub remaining: i128,
    pub signer_count: u32,
}

/// Emitted when an admin registers a new approved metadata schema (issue #328).
#[contracttype]
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct SchemaRegisteredEvent {
    pub admin: Address,
    pub schema: Symbol,
}

/// Lightweight config for multi-merchant whitelisting and allowance tracking.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub struct PermissionConfig {
    pub merchants: Vec<Address>,
    pub allowance: i128,
}

#[contracttype]
#[derive(Clone, Debug)]
/// Emitted when a permission is granted.
#[allow(missing_docs)]
pub struct PermissionGrantedEvent {
    pub owner: Address,
    pub delegate: Address,
    pub per_tx_limit: i128,
    pub total_limit: i128,
    /// Amount the previous record for `(owner, delegate)` had already spent
    /// before this (re-)grant. `0` for a first grant with no prior record.
    /// Lets consumers see that a re-grant reset spend accounting (issue #51).
    pub previous_spent: i128,
    /// Change in remaining allowance caused by this (re-)grant, i.e. the new
    /// total limit minus the previous remaining allowance. Positive when more
    /// spending power was added, negative when it was reduced (issue #51).
    pub remaining_delta: i128,
    pub expires_at_ledger: u32,
    pub merchant_count: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
/// Emitted when a permission is revoked.
#[allow(missing_docs)]
pub struct PermissionRevokedEvent {
    pub owner: Address,
    pub delegate: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
/// Emitted when a permission is transferred to a new delegate.
#[allow(missing_docs)]
pub struct PermissionTransferredEvent {
    pub owner: Address,
    pub old_delegate: Address,
    pub new_delegate: Address,
    pub remaining_allowance: i128,
}

/// Emitted after a delegated spend is successfully recorded (issue #99).
#[contracttype]
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct PermissionSpendEvent {
    pub owner: Address,
    pub delegate: Address,
    pub merchant: Address,
    pub amount: i128,
    pub remaining: i128,
}

/// Canonical payload a delegate signs off-chain to authorize a gasless spend
/// submitted on their behalf by a relayer (issue #334). Serialized via
/// [`soroban_sdk::xdr::ToXdr`] to produce the exact bytes that are ed25519-signed
/// and later re-derived and verified inside `execute_spend_via_relayer`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub struct RelayedSpendMessage {
    pub owner: Address,
    pub delegate: Address,
    pub merchant: Address,
    pub amount: i128,
    pub nonce: u64,
    pub expiration_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
/// Emitted when a permission's merchant whitelist changes.
#[allow(missing_docs)]
pub struct MerchantWhitelistChangedEvent {
    pub owner: Address,
    pub delegate: Address,
    pub merchant_count: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
/// A pending allowance decrease waiting for its time-lock to expire.
#[allow(missing_docs)]
pub struct PendingAllowanceDecrement {
    pub amount: i128,
    pub execution_time: u64,
}

/// Typed allowance breakdown returned by `get_allowance_detail` (issue #98).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub struct RemainingAllowance {
    pub limit: i128,
    pub spent: i128,
    pub remaining: i128,
    pub expires_at_ledger: u32,
}

/// Contract identity returned by `version` (issue #103).
#[contracttype]
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct ContractVersion {
    pub name: Symbol,
    pub semver: Symbol,
}

/// Stored when a permission is paused; cleared on resume (issue #105).
#[contracttype]
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct PauseMetadata {
    pub paused_by: Address,
    pub reason_code: Symbol,
    pub paused_at_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
/// Emitted when a permission is paused.
#[allow(missing_docs)]
pub struct PermissionPausedEvent {
    pub owner: Address,
    pub delegate: Address,
    pub paused_by: Address,
    pub reason_code: Symbol,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct PermissionResumedEvent {
    pub owner: Address,
    pub delegate: Address,
    pub resumed_by: Address,
}

/// Global pause state for new permission grants (issue #186).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionPauseState {
    pub grants_paused: bool,
    pub updated_at_ledger: u32,
}

/// Emitted when the global grant pause state changes (issue #186).
#[contracttype]
#[derive(Clone, Debug)]
pub struct GrantPauseChangedEvent {
    pub grants_paused: bool,
    pub changed_by: Address,
    pub ledger: u32,
}

/// Emitted when an allowance decrease is successfully applied (issue #189).
#[contracttype]
#[derive(Clone, Debug)]
pub struct AllowanceDecreasedEvent {
    pub owner: Address,
    pub delegate: Address,
    pub old_limit: i128,
    pub new_limit: i128,
}

/// Emitted when an allowance increase is successfully applied.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AllowanceIncreasedEvent {
    pub owner: Address,
    pub delegate: Address,
    pub old_limit: i128,
    pub new_limit: i128,
}

/// Emitted when the admin configures a new spend velocity limit (#324).
#[contracttype]
#[derive(Clone, Debug)]
pub struct VelocityLimitSetEvent {
    pub previous: Option<u32>,
    pub current: u32,
    pub set_by: Address,
}

/// Emitted when the expiry of a permission is updated via `update_expiry` (issue #102).
#[contracttype]
#[derive(Clone, Debug)]
pub struct PermissionExpiryUpdatedEvent {
    pub owner: Address,
    pub delegate: Address,
    pub old_expiry: u32,
    pub new_expiry: u32,
}

/// Emitted by `set_relayer_key` whenever a delegate's relayer signing key
/// is registered or rotated, so a key swap (e.g. from a compromised
/// delegate) leaves an on-chain trace.
#[contracttype]
#[derive(Clone, Debug)]
pub struct RelayerKeyChangedEvent {
    pub delegate: Address,
    pub old_key: Option<BytesN<32>>,
    pub new_key: BytesN<32>,
}

/// Emitted by `renew_permission` when a renewal's requested extension would
/// overflow `u32` and is instead capped at `u32::MAX`, so callers get an
/// explicit signal rather than a silently saturated expiry.
#[contracttype]
#[derive(Clone, Debug)]
pub struct PermissionExpiryCappedEvent {
    pub owner: Address,
    pub delegate: Address,
    pub capped_at: u32,
}

/// Emitted by `propose_admin` when the current admin proposes a successor
/// as part of the two-step admin transfer.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AdminProposedEvent {
    pub current_admin: Address,
    pub new_admin: Address,
}

/// Emitted by `accept_admin` once the proposed successor accepts the role.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AdminAcceptedEvent {
    pub previous_admin: Address,
    pub new_admin: Address,
}

/// A single entry in the on-chain audit log for a (owner, delegate) pair.
/// Stored as a `Vec<AuditLogEntry>` under `DataKey::AuditLog(owner, delegate)`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditLogEntry {
    pub action: Symbol,
    pub actor: Address,
    pub timestamp: u64,
}

/// Compact read-only status view for a single delegation (issue #100).
///
/// `active`    – true only when the delegate can currently spend.
/// `reason`    – short code describing the state:
///               `"active"`, `"revoked"`, `"expired"`, `"exhausted"`, `"paused"`,
///               `"not_found"`.
/// `remaining` – remaining allowance (0 when not active).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegateStatusView {
    pub active: bool,
    pub reason: Symbol,
    pub remaining: i128,
}

/// Status view that surfaces `PermissionStatus` explicitly rather than only
/// a derived `active`/`reason` pair, so callers can distinguish e.g.
/// "revoked" from "no budget left" without relying on `reason` string
/// matching.
///
/// `remaining` is `0` for every non-`Active` state (matching
/// `DelegateStatusView`'s existing convention) — it does not reflect the
/// permission's underlying allowance once it is no longer spendable.
/// `expires_at_ledger` is always the record's stored expiry regardless of
/// status, or `0` when no permission record exists for this pair.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegateStatusV2 {
    pub owner: Address,
    pub delegate: Address,
    pub status: PermissionStatus,
    pub remaining: i128,
    pub expires_at_ledger: u32,
}

///
/// `allowed`       – true when all validation rules would pass.
/// `reason`        – short code describing the outcome:
///                   `"ok"`, `"not_found"`, `"expired"`, `"paused"`,
///                   `"unauthorized"`, `"per_tx_limit"`, `"total_limit"`,
///                   `"bad_merchant"`.
/// `remaining_after` – allowance that would be left if the spend were
///                   executed (equals current remaining when `allowed`
///                   is false).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpendPreview {
    pub allowed: bool,
    pub reason: Symbol,
    pub remaining_after: i128,
}

/// Compact receipt returned after a successful grant (issue #180).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionReceipt {
    pub owner: Address,
    pub delegate: Address,
    pub limit: i128,
    pub expires_at_ledger: u32,
    pub active: bool,
}

/// Optional metadata linking on-chain policy to off-chain descriptions (issue #181).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionMetadata {
    pub policy_hash: BytesN<32>,
    pub schema: Symbol,
}

/// On-chain spend analytics for a single (owner, delegate) delegation,
/// updated on every successful spend (issue #336).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionUsageStats {
    pub total_spends: u64,
    pub total_spent: i128,
    pub average_spend: i128,
    pub largest_spend: i128,
    pub first_spend_ledger: u32,
    pub last_spend_ledger: u32,
}

/// Non-truncating usage telemetry view (issue: add spend-count and
/// BPS-denominated average to usage stats).
///
/// `average_spent_bps` is `total_spent * 10_000 / spend_count`, i.e. the
/// average spend amount expressed in basis points of a single unit, which
/// preserves precision that plain integer-division of `average_spend`
/// would otherwise round away for small or skewed spend counts. Both this
/// and `average_spend` use truncating integer division; callers needing
/// the exact fractional remainder should derive it from `total_spent` and
/// `spend_count` directly.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageStatsView {
    pub spend_count: u64,
    pub total_spent: i128,
    pub average_spent_bps: u64,
    pub last_spend_ledger: Option<u32>,
}

/// Tracks the total spent amount and most recent spend ledger for audit and freshness checks.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionUsage {
    pub spent: i128,
    pub last_spend_ledger: Option<u32>,
}

/// Read-only view of the merchant restriction configured under a delegation
/// permission. `None` when the delegation pair has no permission record or
/// the whitelist is empty.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantRestriction {
    pub owner: Address,
    pub delegate: Address,
    pub merchant: Option<Address>,
}

/// Full merchant whitelist for a delegation pair, bounded by
/// `MAX_MERCHANTS_PER_PERMISSION`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantRestrictionView {
    pub merchants: Vec<Address>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildPermission {
    pub delegate: Address,
    pub limit_total: i128,
    pub limit_per_tx: i128,
    pub created_at: u64,
}

#[contracttype]
pub enum DataKey {
    Permission(Address, Address),
    PendingDecrement(Address, Address),
    PauseMetadata(Address, Address),
    Admin,
    PendingAdmin,
    GrantPauseState,
    Metadata(Address, Address),
    /// Instance-level flag: when true, grant() allows owner == delegate.
    AllowSelfDelegation,
    /// Delegate's registered ed25519 public key used to verify relayed spends.
    RelayerKey(Address),
    /// Next expected nonce for a (owner, delegate) pair's relayed spends.
    RelayerNonce(Address, Address),
    /// On-chain usage analytics for a (owner, delegate) pair.
    UsageStats(Address, Address),
    /// Multi-owner permission, keyed by (owners[0], delegate).
    MultiPermission(Address, Address),
    /// Instance-level list of approved `PermissionMetadata.schema` identifiers.
    SchemaRegistry,
    /// List of child delegates granted under a (owner, delegate) pair via `grant_child`.
    Children(Address, Address),
    /// Instance-level inactivity threshold in seconds used by `sweep_inactive`.
    InactivityThreshold,
    /// Instance-level minimum number of ledgers between successive spends (velocity limit).
    MinSpendInterval,
    /// Instance-level delay in seconds before a scheduled allowance decrease can execute.
    DecreaseTimelockSecs,
    /// Last ledger on which a spend was executed for a (owner, delegate) pair.
    LastSpendLedger(Address, Address),
    /// Append-only audit log for a (owner, delegate) pair.
    AuditLog(Address, Address),
    /// Index of delegate addresses granted by a given owner.
    UserPermissions(Address),
}

#[contract]
pub struct PermissionsContract;

// The `#[contractimpl]` macro generates client/wrapper functions that mirror
// the ABI entry-point signatures above; they cannot be annotated individually
// from user code, so the allow lives on the impl block for those generated
// wrappers only. User-defined functions carry their own scoped allows.
#[allow(clippy::too_many_arguments)]
#[contractimpl]
impl PermissionsContract {
    /// Records a (owner, delegate) delegation as a **first grant**.
    ///
    /// A plain `grant` refuses to silently overwrite a live permission
    /// (issue #51): if an Active/Paused record already exists for this
    /// `(owner, delegate)` pair, it returns [`PermissionError::AlreadyGranted`]
    /// instead of resetting its spend accounting. Callers that deliberately
    /// want to replace an existing delegation must use [`Self::re_grant`].
    pub fn grant(
        env: Env,
        owner: Address,
        delegate: Address,
        limit_total: i128,
        limit_per_tx: i128,
        allowed_merchants: Vec<Address>,
        ttl_ledgers: u32,
    ) -> Result<(), PermissionError> {
        Self::grant_impl(
            env,
            owner,
            delegate,
            limit_total,
            limit_per_tx,
            allowed_merchants,
            ttl_ledgers,
            false,
        )
    }

    /// Explicitly replaces an existing delegation's terms (issue #51).
    ///
    /// Unlike [`Self::grant`], this is permitted on a live (Active/Paused)
    /// permission. The emitted [`PermissionGrantedEvent`] carries the previous
    /// record's `spent` (as `previous_spent`) and the resulting change in
    /// remaining allowance (as `remaining_delta`), so spend accounting is
    /// never erased without a distinguishable signal.
    ///
    /// Fails with [`PermissionError::PermissionNotFound`] if no permission
    /// exists yet for `(owner, delegate)` — use `grant` for a first grant.
    pub fn re_grant(
        env: Env,
        owner: Address,
        delegate: Address,
        limit_total: i128,
        limit_per_tx: i128,
        allowed_merchants: Vec<Address>,
        ttl_ledgers: u32,
    ) -> Result<(), PermissionError> {
        Self::grant_impl(
            env,
            owner,
            delegate,
            limit_total,
            limit_per_tx,
            allowed_merchants,
            ttl_ledgers,
            true,
        )
    }

    /// Shared implementation for `grant` / `re_grant`.
    ///
    /// `re_grant` opts the caller into replacing an existing live permission
    /// and reports the previous spent / remaining delta on the event so spend
    /// accounting is never silently discarded.
    fn grant_impl(
        env: Env,
        owner: Address,
        delegate: Address,
        limit_total: i128,
        limit_per_tx: i128,
        allowed_merchants: Vec<Address>,
        ttl_ledgers: u32,
        re_grant: bool,
    ) -> Result<(), PermissionError> {
        owner.require_auth();

        // Issue #186: block new grants when globally paused
        if let Some(state) = env
            .storage()
            .instance()
            .get::<DataKey, PermissionPauseState>(&DataKey::GrantPauseState)
        {
            if state.grants_paused {
                return Err(PermissionError::GrantsPaused);
            }
        }

        // Reject self-delegation unless the contract config explicitly allows it (issue #182).
        let allow_self: bool = env
            .storage()
            .instance()
            .get(&DataKey::AllowSelfDelegation)
            .unwrap_or(false);
        if !allow_self && owner == delegate {
            return Err(PermissionError::SelfDelegationNotAllowed);
        }

        // Reject nonsensical limits: per-tx must be positive and the total
        // allowance must be at least one full per-tx spend.
        if limit_per_tx <= 0 || limit_total < limit_per_tx {
            return Err(PermissionError::InvalidParam);
        }

        // Validate merchant whitelist bounds and uniqueness.
        Self::validate_merchant_list(&env, &allowed_merchants)?;

        // Issue #51: distinguish a first grant from a re-grant. A plain grant
        // must not silently overwrite a live delegation and reset its spend
        // accounting; only an explicit re-grant replaces it, and it reports
        // the previous spent amount and the remaining-allowance delta.
        let key = DataKey::Permission(owner.clone(), delegate.clone());
        let existing: Option<PermissionRecord> = env.storage().persistent().get(&key);
        let (previous_spent, old_remaining) = match &existing {
            Some(r) => (r.spent, r.limit_total - r.spent),
            None => (0, 0),
        };
        match &existing {
            Some(r)
                if !re_grant
                    && matches!(
                        r.status,
                        PermissionStatus::Active | PermissionStatus::Paused
                    ) =>
            {
                return Err(PermissionError::AlreadyGranted);
            }
            None if re_grant => {
                return Err(PermissionError::PermissionNotFound);
            }
            _ => {}
        }

        let expires_at_ledger = env.ledger().sequence() + ttl_ledgers;
        let expires_at_ledger = Self::grant_expiry_ledger(&env, ttl_ledgers)?;

        let user_perms_key = DataKey::UserPermissions(owner.clone());
        let mut delegates: Vec<Address> = env
            .storage()
            .persistent()
            .get(&user_perms_key)
            .unwrap_or(Vec::new(&env));
        if !delegates.contains(&delegate) {
            delegates.push_back(delegate.clone());
            env.storage().persistent().set(&user_perms_key, &delegates);
        }

        let user_perms_key = DataKey::UserPermissions(owner.clone());
        let mut delegates: Vec<Address> = env
            .storage()
            .persistent()
            .get(&user_perms_key)
            .unwrap_or(Vec::new(&env));
        if !delegates.contains(&delegate) {
            delegates.push_back(delegate.clone());
            env.storage().persistent().set(&user_perms_key, &delegates);
        }

        let user_perms_key = DataKey::UserPermissions(owner.clone());
        let mut delegates: Vec<Address> = env
            .storage()
            .persistent()
            .get(&user_perms_key)
            .unwrap_or(Vec::new(&env));
        if !delegates.contains(&delegate) {
            delegates.push_back(delegate.clone());
            env.storage().persistent().set(&user_perms_key, &delegates);
        }

        let record = PermissionRecord {
            owner: owner.clone(),
            delegate: delegate.clone(),
            limit_total,
            spent: 0,
            limit_per_tx,
            allowed_merchants: allowed_merchants.clone(),
            status: PermissionStatus::Active,
            expires_at_ledger,
            created_at: env.ledger().timestamp(),
            parent_owner: None,
            parent_delegate: None,
        };

        env.storage().persistent().set(&key, &record);

        // Change in usable allowance caused by this (re-)grant. For a first
        // grant `old_remaining` is 0 so this equals the new total limit; for a
        // re-grant it reflects how much more (or less) the delegate can spend
        // than before.
        let remaining_delta = limit_total - old_remaining;

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("granted")),
            PermissionGrantedEvent {
                owner: owner.clone(),
                delegate: delegate.clone(),
                per_tx_limit: limit_per_tx,
                total_limit: limit_total,
                previous_spent,
                remaining_delta,
                expires_at_ledger,
                merchant_count: allowed_merchants.len(),
            },
        );

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("merc_list")),
            MerchantWhitelistChangedEvent {
                owner: owner.clone(),
                delegate: delegate.clone(),
                merchant_count: allowed_merchants.len(),
            },
        );

        let action = if re_grant {
            symbol_short!("regranted")
        } else {
            symbol_short!("granted")
        };
        Self::append_audit_log(&env, &owner, &delegate, owner.clone(), action);

        Ok(())
    }

    /// Grants a child permission under an existing permission, for agent
    /// hierarchies where a delegate needs to sub-delegate part of its own
    /// allowance to another agent (issue #332).
    ///
    /// Must be called by `parent_delegate` — the delegate of the parent
    /// permission `(parent_owner, parent_delegate)` — acting as the
    /// "owner" of the new child permission. The child is bounded by the
    /// parent: its `limit_total` cannot exceed the parent's remaining
    /// allowance, its `limit_per_tx` cannot exceed the parent's per-tx
    /// limit, and its expiry is clamped to the parent's expiry.
    ///
    /// # Errors
    /// - [`PermissionError::ParentNotFound`] if the parent permission
    ///   doesn't exist.
    /// - [`PermissionError::PermissionPaused`] / [`PermissionError::Expired`]
    ///   if the parent isn't currently active.
    /// - [`PermissionError::ExceedsParentLimit`] if the requested child
    ///   limits exceed what the parent can back.
    pub fn grant_child(
        env: Env,
        parent_owner: Address,
        parent_delegate: Address,
        child_delegate: Address,
        limit_total: i128,
        limit_per_tx: i128,
        allowed_merchants: Vec<Address>,
        ttl_ledgers: u32,
    ) -> Result<(), PermissionError> {
        parent_delegate.require_auth();

        if let Some(state) = env
            .storage()
            .instance()
            .get::<DataKey, PermissionPauseState>(&DataKey::GrantPauseState)
        {
            if state.grants_paused {
                return Err(PermissionError::GrantsPaused);
            }
        }

        let allow_self: bool = env
            .storage()
            .instance()
            .get(&DataKey::AllowSelfDelegation)
            .unwrap_or(false);
        if !allow_self && parent_delegate == child_delegate {
            return Err(PermissionError::SelfDelegationNotAllowed);
        }

        if limit_per_tx <= 0 || limit_total < limit_per_tx {
            return Err(PermissionError::InvalidParam);
        }

        // Validate merchant whitelist bounds and uniqueness.
        Self::validate_merchant_list(&env, &allowed_merchants)?;

        let parent_key = DataKey::Permission(parent_owner.clone(), parent_delegate.clone());
        let parent_record: PermissionRecord = env
            .storage()
            .persistent()
            .get(&parent_key)
            .ok_or(PermissionError::ParentNotFound)?;

        if parent_record.status != PermissionStatus::Active {
            return Err(PermissionError::PermissionPaused);
        }
        if env.ledger().sequence() >= parent_record.expires_at_ledger {
            return Err(PermissionError::Expired);
        }

        let parent_remaining = parent_record.limit_total - parent_record.spent;
        if limit_total > parent_remaining || limit_per_tx > parent_record.limit_per_tx {
            return Err(PermissionError::ExceedsParentLimit);
        }

        let requested_expiry = Self::grant_expiry_ledger(&env, ttl_ledgers)?;
        let expires_at_ledger = requested_expiry.min(parent_record.expires_at_ledger);

        let record = PermissionRecord {
            owner: parent_delegate.clone(),
            delegate: child_delegate.clone(),
            limit_total,
            spent: 0,
            limit_per_tx,
            allowed_merchants: allowed_merchants.clone(),
            status: PermissionStatus::Active,
            expires_at_ledger,
            created_at: env.ledger().timestamp(),
            parent_owner: Some(parent_owner.clone()),
            parent_delegate: Some(parent_delegate.clone()),
        };

        let child_key = DataKey::Permission(parent_delegate.clone(), child_delegate.clone());
        env.storage().persistent().set(&child_key, &record);

        let children_key = DataKey::Children(parent_owner, parent_delegate.clone());
        let mut children: Vec<Address> = env
            .storage()
            .persistent()
            .get(&children_key)
            .unwrap_or_else(|| Vec::new(&env));
        if !children.contains(&child_delegate) {
            children.push_back(child_delegate.clone());
            env.storage().persistent().set(&children_key, &children);
        }

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("granted")),
            PermissionGrantedEvent {
                owner: parent_delegate,
                delegate: child_delegate,
                per_tx_limit: limit_per_tx,
                total_limit: limit_total,
                previous_spent: 0,
                remaining_delta: limit_total,
                expires_at_ledger,
                merchant_count: allowed_merchants.len(),
            },
        );

        Ok(())
    }

    pub fn revoke(env: Env, owner: Address, delegate: Address) -> Result<(), PermissionError> {
        owner.require_auth();

        let key = DataKey::Permission(owner.clone(), delegate.clone());
        if let Some(mut record) = env
            .storage()
            .persistent()
            .get::<DataKey, PermissionRecord>(&key)
        {
            let user_perms_key = DataKey::UserPermissions(owner.clone());
            let mut delegates: Vec<Address> = env
                .storage()
                .persistent()
                .get(&user_perms_key)
                .unwrap_or(Vec::new(&env));
            if let Some(index) = delegates.first_index_of(&delegate) {
                delegates.remove(index);
                env.storage().persistent().set(&user_perms_key, &delegates);
            }

            record.status = PermissionStatus::Revoked;
            env.storage().persistent().set(&key, &record);
            env.storage()
                .persistent()
                .remove(&DataKey::PendingDecrement(owner.clone(), delegate.clone()));

            env.events().publish(
                (symbol_short!("perm"), symbol_short!("revoked")),
                PermissionRevokedEvent {
                    owner: owner.clone(),
                    delegate: delegate.clone(),
                },
            );

            // Cascade: revoking a permission also revokes every child
            // permission granted under it (issue #332).
            Self::revoke_children(&env, &owner, &delegate);

            Ok(())
        } else {
            Err(PermissionError::PermissionNotFound)
        }
    }

    /// Transfer a permission from one delegate to another, preserving spending limits and history.
    ///
    /// Atomically:
    /// 1. Verifies the owner authorizes the transfer
    /// 2. Checks that the old permission exists and is not revoked
    /// 3. Creates a new permission with the same limits/spending/merchants
    /// 4. Revokes the old permission
    /// 5. Emits PermissionTransferredEvent with remaining allowance
    ///
    /// The new permission starts fresh with the same configuration but preserves
    /// the spent amount and remaining allowance from the old permission.
    pub fn transfer_permission(
        env: Env,
        owner: Address,
        old_delegate: Address,
        new_delegate: Address,
    ) -> Result<(), PermissionError> {
        owner.require_auth();

        // Prevent self-transfer
        if old_delegate == new_delegate {
            return Err(PermissionError::InvalidParam);
        }

        // Check self-delegation is allowed if new delegate is owner
        let allow_self: bool = env
            .storage()
            .instance()
            .get(&DataKey::AllowSelfDelegation)
            .unwrap_or(false);
        if !allow_self && owner == new_delegate {
            return Err(PermissionError::SelfDelegationNotAllowed);
        }

        // Retrieve the old permission
        let old_key = DataKey::Permission(owner.clone(), old_delegate.clone());
        let old_record: PermissionRecord = env
            .storage()
            .persistent()
            .get(&old_key)
            .ok_or(PermissionError::PermissionNotFound)?;

        // Reject if already revoked
        if old_record.status == PermissionStatus::Revoked {
            return Err(PermissionError::Unauthorized);
        }

        // Calculate remaining allowance
        let remaining_allowance = old_record.limit_total - old_record.spent;

        // Create new permission with same configuration but fresh expiry
        // Preserve the spent counter to maintain history
        let new_record = PermissionRecord {
            owner: owner.clone(),
            delegate: new_delegate.clone(),
            limit_total: old_record.limit_total,
            spent: old_record.spent,
            limit_per_tx: old_record.limit_per_tx,
            allowed_merchants: old_record.allowed_merchants.clone(),
            status: PermissionStatus::Active,
            expires_at_ledger: old_record.expires_at_ledger,
            created_at: env.ledger().timestamp(),
            parent_owner: old_record.parent_owner.clone(),
            parent_delegate: old_record.parent_delegate.clone(),
        };

        let new_key = DataKey::Permission(owner.clone(), new_delegate.clone());

        // Ensure new permission doesn't already exist
        if env.storage().persistent().has(&new_key) {
            return Err(PermissionError::InvalidParam);
        }

        let user_perms_key = DataKey::UserPermissions(owner.clone());
        let mut delegates: Vec<Address> = env
            .storage()
            .persistent()
            .get(&user_perms_key)
            .unwrap_or(Vec::new(&env));

        if let Some(index) = delegates.first_index_of(&old_delegate) {
            delegates.remove(index);
        }

        if !delegates.contains(&new_delegate) {
            delegates.push_back(new_delegate.clone());
        }
        env.storage().persistent().set(&user_perms_key, &delegates);

        // Store the new permission
        env.storage().persistent().set(&new_key, &new_record);

        // Revoke the old permission
        let mut revoked_record = old_record;
        revoked_record.status = PermissionStatus::Revoked;
        env.storage().persistent().set(&old_key, &revoked_record);
        env.storage()
            .persistent()
            .remove(&DataKey::PendingDecrement(
                owner.clone(),
                old_delegate.clone(),
            ));

        // Emit transfer event
        env.events().publish(
            (symbol_short!("perm"), symbol_short!("transf")),
            PermissionTransferredEvent {
                owner: owner.clone(),
                old_delegate,
                new_delegate: new_delegate.clone(),
                remaining_allowance,
            },
        );

        Self::append_audit_log(
            &env,
            &owner,
            &new_delegate,
            owner.clone(),
            symbol_short!("transf"),
        );

        Ok(())
    }

    /// Renew a permission by extending its TTL without disruption.
    /// Owner extends the expiry ledger by the specified additional ledgers.
    /// Fails if permission is revoked or expired.
    pub fn renew_permission(
        env: Env,
        owner: Address,
        delegate: Address,
        additional_ledgers: u32,
    ) -> Result<(), PermissionError> {
        owner.require_auth();

        let key = DataKey::Permission(owner.clone(), delegate.clone());
        if let Some(mut record) = env
            .storage()
            .persistent()
            .get::<DataKey, PermissionRecord>(&key)
        {
            // Reject if already revoked
            if record.status == PermissionStatus::Revoked {
                return Err(PermissionError::Unauthorized);
            }

            // Reject if already expired
            if env.ledger().sequence() >= record.expires_at_ledger {
                return Err(PermissionError::Expired);
            }

            // Store old expiry for audit log
            let old_expires = record.expires_at_ledger;

            // Extend TTL, explicitly signalling rather than silently
            // saturating if the extension would overflow u32.
            record.expires_at_ledger = match old_expires.checked_add(additional_ledgers) {
                Some(new_expiry) => new_expiry,
                None => {
                    env.events().publish(
                        (symbol_short!("perm"), symbol_short!("exp_cap")),
                        PermissionExpiryCappedEvent {
                            owner: owner.clone(),
                            delegate: delegate.clone(),
                            capped_at: u32::MAX,
                        },
                    );
                    u32::MAX
                }
            };

            // Persist the updated record (spent counter preserved)
            env.storage().persistent().set(&key, &record);

            // Publish renewal event
            env.events().publish(
                (symbol_short!("perm"), symbol_short!("renewed")),
                (
                    owner.clone(),
                    delegate.clone(),
                    old_expires,
                    record.expires_at_ledger,
                ),
            );

            Self::append_audit_log(
                &env,
                &owner,
                &delegate,
                owner.clone(),
                symbol_short!("renewed"),
            );

            Ok(())
        } else {
            Err(PermissionError::PermissionNotFound)
        }
    }

    /// Directly sets a new absolute expiry ledger for a permission (issue #102).
    ///
    /// Unlike `renew_permission` (which adds ledgers), this sets the expiry to an
    /// exact ledger sequence number. Useful for setting a precise deadline rather
    /// than extending relatively.
    ///
    /// # Errors
    /// - [`PermissionError::PermissionNotFound`] if no permission exists for `(owner, delegate)`
    /// - [`PermissionError::Unauthorized`] if permission is already revoked
    /// - [`PermissionError::InvalidParam`] if `new_expiry` is not greater than the current ledger
    pub fn update_expiry(
        env: Env,
        owner: Address,
        delegate: Address,
        new_expiry: u32,
    ) -> Result<(), PermissionError> {
        owner.require_auth();

        // Validate: new_expiry must be strictly in the future
        if new_expiry <= env.ledger().sequence() {
            return Err(PermissionError::InvalidParam);
        }

        let key = DataKey::Permission(owner.clone(), delegate.clone());
        let mut record: PermissionRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(PermissionError::PermissionNotFound)?;

        // Cannot update a revoked permission
        if record.status == PermissionStatus::Revoked {
            return Err(PermissionError::Unauthorized);
        }

        let old_expiry = record.expires_at_ledger;
        record.expires_at_ledger = new_expiry;
        env.storage().persistent().set(&key, &record);

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("exp_upd")),
            PermissionExpiryUpdatedEvent {
                owner: owner.clone(),
                delegate: delegate.clone(),
                old_expiry,
                new_expiry,
            },
        );

        Self::append_audit_log(
            &env,
            &owner,
            &delegate,
            owner.clone(),
            symbol_short!("exp_upd"),
        );

        Ok(())
    }

    /// Requires that `caller` is authorized and matches the stored admin.
    /// Returns `PermissionError::NotInitialized` (never panics) if
    /// `set_admin` has not yet been called, and `Unauthorized` if `caller`
    /// is not the stored admin.
    fn require_admin(env: &Env, caller: &Address) -> Result<Address, PermissionError> {
        caller.require_auth();
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(PermissionError::NotInitialized)?;
        if *caller != stored_admin {
            return Err(PermissionError::Unauthorized);
        }
        Ok(stored_admin)
    }

    /// Computes an absolute expiry ledger as `current_sequence + ttl_ledgers`,
    /// returning [`PermissionError::InvalidExpiry`] instead of overflow-panicking
    /// when `ttl_ledgers` is large enough to push the sum past `u32::MAX`.
    fn grant_expiry_ledger(env: &Env, ttl_ledgers: u32) -> Result<u32, PermissionError> {
        ttl_ledgers
            .checked_add(env.ledger().sequence())
            .ok_or(PermissionError::InvalidExpiry)
    }

    /// Validates the merchant whitelist:
    /// - Must not exceed `MAX_MERCHANTS_PER_PERMISSION` entries.
    /// - Must not contain duplicate addresses.
    fn validate_merchant_list(env: &Env, merchants: &Vec<Address>) -> Result<(), PermissionError> {
        if merchants.len() > MAX_MERCHANTS_PER_PERMISSION {
            return Err(PermissionError::InvalidParam);
        }
        let mut seen: Vec<Address> = Vec::new(env);
        for m in merchants.iter() {
            if seen.contains(&m) {
                return Err(PermissionError::InvalidParam);
            }
            seen.push_back(m);
        }
        Ok(())
    }

    /// Recursively revokes every child permission granted under
    /// `(owner, delegate)` via `grant_child`.
    fn revoke_children(env: &Env, owner: &Address, delegate: &Address) {
        let children_key = DataKey::Children(owner.clone(), delegate.clone());
        let children: Vec<Address> = env
            .storage()
            .persistent()
            .get(&children_key)
            .unwrap_or_else(|| Vec::new(env));

        for child_delegate in children.iter() {
            let child_key = DataKey::Permission(delegate.clone(), child_delegate.clone());
            if let Some(mut child_record) = env
                .storage()
                .persistent()
                .get::<DataKey, PermissionRecord>(&child_key)
            {
                if child_record.status != PermissionStatus::Revoked {
                    child_record.status = PermissionStatus::Revoked;
                    env.storage().persistent().set(&child_key, &child_record);
                    env.events().publish(
                        (symbol_short!("perm"), symbol_short!("revoked")),
                        PermissionRevokedEvent {
                            owner: delegate.clone(),
                            delegate: child_delegate.clone(),
                        },
                    );
                }
                Self::revoke_children(env, delegate, &child_delegate);
            }
        }
    }

    /// Walks the `parent_owner`/`parent_delegate` chain of a permission
    /// record, returning the minimum `expires_at_ledger` across the record
    /// itself and every ancestor. A child permission is only ever
    /// spendable up to the earliest expiry in its lineage, so a parent
    /// that expires naturally (by ledger, without an explicit `revoke`)
    /// also caps every descendant's effective liveness — closing the gap
    /// where only explicit revocation cascaded but natural expiry did not.
    ///
    /// Bounded to 32 hops to guard against a pathological/cyclic parent
    /// chain; real chains are expected to be a handful of levels deep.
    fn effective_expiry(env: &Env, record: &PermissionRecord) -> u32 {
        let mut min_expiry = record.expires_at_ledger;
        let mut next_parent = match (record.parent_owner.clone(), record.parent_delegate.clone()) {
            (Some(p_owner), Some(p_delegate)) => Some((p_owner, p_delegate)),
            _ => None,
        };

        let mut hops = 0u32;
        while let Some((p_owner, p_delegate)) = next_parent {
            if hops >= 32 {
                break;
            }
            hops += 1;

            let parent_key = DataKey::Permission(p_owner, p_delegate);
            let parent_record: PermissionRecord = match env.storage().persistent().get(&parent_key)
            {
                Some(r) => r,
                None => break,
            };

            if parent_record.expires_at_ledger < min_expiry {
                min_expiry = parent_record.expires_at_ledger;
            }

            next_parent = match (
                parent_record.parent_owner.clone(),
                parent_record.parent_delegate.clone(),
            ) {
                (Some(p_owner), Some(p_delegate)) => Some((p_owner, p_delegate)),
                _ => None,
            };
        }

        min_expiry
    }

    pub fn can_spend(
        env: Env,
        owner: Address,
        delegate: Address,
        amount: i128,
        merchant: Address,
    ) -> Result<(), PermissionError> {
        let key = DataKey::Permission(owner.clone(), delegate.clone());
        let record: PermissionRecord = match env.storage().persistent().get(&key) {
            Some(r) => r,
            None => return Err(PermissionError::PermissionNotFound),
        };

        match record.status {
            PermissionStatus::Active => {}
            PermissionStatus::Paused => return Err(PermissionError::PermissionPaused),
            PermissionStatus::Expired => return Err(PermissionError::Expired),
            PermissionStatus::Revoked => return Err(PermissionError::Unauthorized),
        }

        // Bound liveness by the full ancestor chain: a parent that expired
        // naturally (by ledger, without an explicit revoke) also expires
        // this child, even though this record's own `expires_at_ledger`
        // hasn't been reached yet.
        if env.ledger().sequence() >= Self::effective_expiry(&env, &record) {
            return Err(PermissionError::Expired);
        }

        if amount > record.limit_per_tx {
            return Err(PermissionError::ExceedsPerTxLimit);
        }

        let remaining = record.limit_total - record.spent;
        if amount > remaining {
            return Err(PermissionError::ExceedsTotalLimit);
        }

        if !record.allowed_merchants.is_empty() {
            let mut allowed = false;
            for m in record.allowed_merchants.iter() {
                if m == merchant {
                    allowed = true;
                    break;
                }
            }
            if !allowed {
                return Err(PermissionError::MerchantNotAllowed);
            }
        }

        Ok(())
    }

    pub fn execute_spend(
        env: Env,
        owner: Address,
        delegate: Address,
        amount: i128,
        merchant: Address,
    ) -> Result<(), PermissionError> {
        delegate.require_auth();

        // Propagate the precise reason (expired, over-limit, wrong merchant, …)
        // to the caller instead of panicking with an opaque string.
        Self::can_spend(
            env.clone(),
            owner.clone(),
            delegate.clone(),
            amount,
            merchant.clone(),
        )?;

        // #54: Velocity check — reject if min_spend_interval has not yet elapsed
        // since the last recorded spend ledger for this (owner, delegate) pair.
        Self::check_velocity(&env, &owner, &delegate)?;

        let _velocity_key = DataKey::LastSpendLedger(owner.clone(), delegate.clone());

        let velocity_key = DataKey::LastSpendLedger(owner.clone(), delegate.clone());
        let remaining = Self::apply_spend(&env, &owner, &delegate, amount)?;

        // Emit after successful spend only (issue #99).
        env.events().publish(
            (symbol_short!("perm"), symbol_short!("spent")),
            PermissionSpendEvent {
                owner,
                delegate,
                merchant,
                amount,
                remaining,
            },
        );

        Ok(())
    }

    /// Shared helper that applies a validated spend to the child permission
    /// record and then walks the parent chain, decrementing each ancestor's
    /// allowance by the same `amount` (issue #55 / #332).
    ///
    /// Callers **must** have already run all validation (`can_spend`,
    /// velocity checks, nonce checks, …) before calling this function.
    /// `apply_spend` only mutates storage — it does **not** re-validate.
    ///
    /// Returns the remaining allowance on the child permission after the spend.
    fn apply_spend(
        env: &Env,
        owner: &Address,
        delegate: &Address,
        amount: i128,
    ) -> Result<i128, PermissionError> {
        let key = DataKey::Permission(owner.clone(), delegate.clone());
        let mut record: PermissionRecord = env.storage().persistent().get(&key).unwrap();

        record.spent += amount;
        let remaining = record.limit_total - record.spent;

        // Capture the parent link before writing the updated record so we can
        // walk the chain without holding an immutable borrow.
        let mut next_parent = match (record.parent_owner.clone(), record.parent_delegate.clone()) {
            (Some(p_owner), Some(p_delegate)) => Some((p_owner, p_delegate)),
            _ => None,
        };
        env.storage().persistent().set(&key, &record);

        Self::record_spend_stats(env, owner, delegate, amount);

        // Record the current ledger for velocity tracking.
        env.storage().persistent().set(
            &DataKey::LastSpendLedger(owner.clone(), delegate.clone()),
            &env.ledger().sequence(),
        );

        // Walk the parent chain, deducting the same amount from each ancestor's
        // allowance so a child's spend is also reflected against the allowance
        // it was carved out of (issue #332). This path is now shared between
        // execute_spend and execute_spend_via_relayer (issue #55).
        while let Some((p_owner, p_delegate)) = next_parent {
            let parent_key = DataKey::Permission(p_owner, p_delegate);
            let mut parent_record: PermissionRecord = env
                .storage()
                .persistent()
                .get(&parent_key)
                .ok_or(PermissionError::ParentNotFound)?;

            let parent_remaining = parent_record.limit_total - parent_record.spent;
            if amount > parent_remaining {
                return Err(PermissionError::ExceedsParentLimit);
            }

            parent_record.spent += amount;
            next_parent = match (
                parent_record.parent_owner.clone(),
                parent_record.parent_delegate.clone(),
            ) {
                (Some(pp_owner), Some(pp_delegate)) => Some((pp_owner, pp_delegate)),
                _ => None,
            };
            env.storage().persistent().set(&parent_key, &parent_record);
        }

        Ok(remaining)
    }

    /// Rejects a spend when the configured velocity limit (`MinSpendInterval`)
    /// has not yet elapsed since the last recorded spend ledger for this
    /// (owner, delegate) pair (issue #54). Called from both `execute_spend`
    /// and `execute_spend_via_relayer` before the new spend is recorded, so
    /// direct and relayed spends share the same throttle. No-op when no
    /// interval has been configured or no prior spend exists.
    fn check_velocity(
        env: &Env,
        owner: &Address,
        delegate: &Address,
    ) -> Result<(), PermissionError> {
        let velocity_key = DataKey::LastSpendLedger(owner.clone(), delegate.clone());
        if let Some(last_ledger) = env
            .storage()
            .persistent()
            .get::<DataKey, u32>(&velocity_key)
        {
            if let Some(min_interval) = env
                .storage()
                .instance()
                .get::<DataKey, u32>(&DataKey::MinSpendInterval)
            {
                let current = env.ledger().sequence();
                if current < last_ledger + min_interval {
                    return Err(PermissionError::VelocityLimitExceeded);
                }
            }
        }
        Ok(())
    }

    /// Register (or rotate) the ed25519 public key used to verify this
    /// delegate's signed messages in `execute_spend_via_relayer`. Must be
    /// called by the delegate directly (a real, gas-paying transaction) —
    /// after this one-time setup, subsequent spends can be relayed gaslessly.
    pub fn set_relayer_key(
        env: Env,
        delegate: Address,
        public_key: BytesN<32>,
    ) -> Result<(), PermissionError> {
        delegate.require_auth();

        let old_key: Option<BytesN<32>> = env
            .storage()
            .instance()
            .get(&DataKey::RelayerKey(delegate.clone()));

        env.storage()
            .instance()
            .set(&DataKey::RelayerKey(delegate.clone()), &public_key);

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("relaykey")),
            RelayerKeyChangedEvent {
                delegate: delegate.clone(),
                old_key,
                new_key: public_key,
            },
        );

        // Relayer keys aren't scoped to an (owner, delegate) pair, so the
        // audit trail is keyed by (delegate, delegate) rather than a real
        // owner relationship.
        Self::append_audit_log(
            &env,
            &delegate,
            &delegate,
            delegate.clone(),
            symbol_short!("relaykey"),
        );

        Ok(())
    }

    /// Returns the delegate's registered relayer signing key, if any.
    pub fn get_relayer_key(env: Env, delegate: Address) -> Option<BytesN<32>> {
        env.storage().instance().get(&DataKey::RelayerKey(delegate))
    }

    /// Returns the next nonce a relayed spend for this (owner, delegate) pair
    /// must use.
    pub fn get_relayer_nonce(env: Env, owner: Address, delegate: Address) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::RelayerNonce(owner, delegate))
            .unwrap_or(0)
    }

    /// Execute a spend on the delegate's behalf from a relayer, without
    /// requiring the delegate to submit (or pay fees for) the transaction
    /// themselves.
    ///
    /// The delegate authorizes the spend by signing a [`RelayedSpendMessage`]
    /// off-chain with the key registered via `set_relayer_key`; any relayer
    /// can then submit that message and signature here. The signature is
    /// verified with Soroban's ed25519 crypto primitive against the
    /// delegate's registered public key, the `nonce` must match the
    /// delegate's next expected nonce (preventing replay), and
    /// `expiration_ledger` must not yet have been reached.
    // Reason: Soroban ABI entry point — signature is part of the published
    // on-chain ABI and cannot be restructured without a breaking change.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_spend_via_relayer(
        env: Env,
        relayer: Address,
        owner: Address,
        delegate: Address,
        amount: i128,
        merchant: Address,
        nonce: u64,
        expiration_ledger: u32,
        signature: BytesN<64>,
    ) -> Result<(), PermissionError> {
        relayer.require_auth();

        if env.ledger().sequence() >= expiration_ledger {
            return Err(PermissionError::SignatureExpired);
        }

        let nonce_key = DataKey::RelayerNonce(owner.clone(), delegate.clone());
        let expected_nonce: u64 = env.storage().persistent().get(&nonce_key).unwrap_or(0);
        if nonce != expected_nonce {
            return Err(PermissionError::InvalidNonce);
        }

        let public_key: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::RelayerKey(delegate.clone()))
            .ok_or(PermissionError::RelayerKeyNotSet)?;

        let message = RelayedSpendMessage {
            owner: owner.clone(),
            delegate: delegate.clone(),
            merchant: merchant.clone(),
            amount,
            nonce,
            expiration_ledger,
        };
        let message_bytes = message.to_xdr(&env);
        env.crypto()
            .ed25519_verify(&public_key, &message_bytes, &signature);

        // Signature verified — apply the same validation rules as a direct
        // execute_spend before mutating state.
        Self::can_spend(
            env.clone(),
            owner.clone(),
            delegate.clone(),
            amount,
            merchant.clone(),
        )?;

        // #54: Velocity check — reject if min_spend_interval has not yet elapsed
        // since the last recorded spend ledger for this (owner, delegate) pair.
        // Shared with execute_spend so direct and relayed spends share the
        // same throttle (issue #179).
        // Velocity check for relayed spend path
        Self::check_velocity(&env, &owner, &delegate)?;

        // Advance the nonce before mutating spend state so a replay attempt
        // within the same ledger is rejected even if apply_spend panics.
        env.storage().persistent().set(&nonce_key, &(nonce + 1));

        // apply_spend increments the child record, walks the full parent chain,
        // updates usage stats, and records the last spend ledger — identical to
        // the direct execute_spend path (issue #55).
        let remaining = Self::apply_spend(&env, &owner, &delegate, amount)?;

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("relayed")),
            PermissionSpendEvent {
                owner,
                delegate,
                merchant,
                amount,
                remaining,
            },
        );

        Ok(())
    }

    /// Grant a delegation jointly controlled by multiple owners (issue #326).
    ///
    /// `owners` must be non-empty and contain no duplicates. `threshold` is
    /// the minimum number of owner signatures required to authorize a spend
    /// (1 <= threshold <= owners.len()). The caller must be one of `owners`.
    /// Stored keyed by `(owners[0], delegate)`.
    // Reason: Soroban ABI entry point — signature is part of the published
    // on-chain ABI and cannot be restructured without a breaking change.
    #[allow(clippy::too_many_arguments)]
    pub fn grant_multi_owner(
        env: Env,
        caller: Address,
        owners: Vec<Address>,
        delegate: Address,
        limit_total: i128,
        limit_per_tx: i128,
        allowed_merchants: Vec<Address>,
        ttl_ledgers: u32,
        threshold: u32,
    ) -> Result<(), PermissionError> {
        caller.require_auth();

        if owners.is_empty() || !owners.contains(&caller) {
            return Err(PermissionError::Unauthorized);
        }
        if threshold == 0 || threshold > owners.len() {
            return Err(PermissionError::InvalidParam);
        }

        // Reject self-delegation unless explicitly allowed, mirroring the
        // guard already applied on `grant` and `grant_child`.
        let allow_self: bool = env
            .storage()
            .instance()
            .get(&DataKey::AllowSelfDelegation)
            .unwrap_or(false);
        if !allow_self && owners.contains(&delegate) {
            return Err(PermissionError::SelfDelegationNotAllowed);
        }
        if limit_per_tx <= 0 || limit_total < limit_per_tx {
            return Err(PermissionError::InvalidParam);
        }

        // Validate merchant whitelist bounds and uniqueness.
        Self::validate_merchant_list(&env, &allowed_merchants)?;

        let mut unique_owners: Vec<Address> = Vec::new(&env);
        for owner in owners.iter() {
            if unique_owners.contains(&owner) {
                return Err(PermissionError::InvalidParam);
            }
            unique_owners.push_back(owner);
        }

        let primary_owner = unique_owners.get(0).unwrap();
        let expires_at_ledger = Self::grant_expiry_ledger(&env, ttl_ledgers)?;

        let record = MultiOwnerPermission {
            owners: unique_owners.clone(),
            threshold,
            delegate: delegate.clone(),
            limit_total,
            spent: 0,
            limit_per_tx,
            allowed_merchants,
            status: PermissionStatus::Active,
            expires_at_ledger,
            created_at: env.ledger().timestamp(),
        };

        env.storage().persistent().set(
            &DataKey::MultiPermission(primary_owner.clone(), delegate.clone()),
            &record,
        );

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("mgrant")),
            MultiOwnerGrantedEvent {
                primary_owner,
                delegate,
                owner_count: unique_owners.len(),
                threshold,
                total_limit: limit_total,
            },
        );

        Ok(())
    }

    /// Dry-run validation of a multi-owner spend, identical to `can_spend`
    /// but requiring `threshold`-of-`owners` valid signers instead of a
    /// single delegate-authorized owner (issue #326).
    pub fn can_spend_multi(
        env: Env,
        primary_owner: Address,
        delegate: Address,
        signers: Vec<Address>,
        amount: i128,
        merchant: Address,
    ) -> Result<(), PermissionError> {
        let key = DataKey::MultiPermission(primary_owner, delegate);
        let record: MultiOwnerPermission = match env.storage().persistent().get(&key) {
            Some(r) => r,
            None => return Err(PermissionError::PermissionNotFound),
        };

        match record.status {
            PermissionStatus::Active => {}
            PermissionStatus::Paused => return Err(PermissionError::PermissionPaused),
            PermissionStatus::Expired => return Err(PermissionError::Expired),
            PermissionStatus::Revoked => return Err(PermissionError::Unauthorized),
        }

        if env.ledger().sequence() >= record.expires_at_ledger {
            return Err(PermissionError::Expired);
        }

        let mut counted: Vec<Address> = Vec::new(&env);
        let mut valid_signers: u32 = 0;
        for signer in signers.iter() {
            if record.owners.contains(&signer) && !counted.contains(&signer) {
                counted.push_back(signer);
                valid_signers += 1;
            }
        }
        if valid_signers < record.threshold {
            return Err(PermissionError::InsufficientSignatures);
        }

        if amount > record.limit_per_tx {
            return Err(PermissionError::ExceedsPerTxLimit);
        }

        let remaining = record.limit_total - record.spent;
        if amount > remaining {
            return Err(PermissionError::ExceedsTotalLimit);
        }

        if !record.allowed_merchants.is_empty() {
            let mut allowed = false;
            for m in record.allowed_merchants.iter() {
                if m == merchant {
                    allowed = true;
                    break;
                }
            }
            if !allowed {
                return Err(PermissionError::MerchantNotAllowed);
            }
        }

        Ok(())
    }

    /// Execute a multi-owner delegated spend. Requires the delegate's
    /// authorization plus authorization from each address in `signers`; at
    /// least `threshold` of `signers` must be registered owners (issue #326).
    pub fn execute_spend_multi(
        env: Env,
        primary_owner: Address,
        delegate: Address,
        signers: Vec<Address>,
        amount: i128,
        merchant: Address,
    ) -> Result<(), PermissionError> {
        delegate.require_auth();

        // Deduplicate signers before requiring auth or counting them toward
        // quorum: a duplicate-padded list must not waste auth frames, and
        // must not be able to satisfy the threshold with fewer real
        // signers than intended.
        let mut unique_signers: Vec<Address> = Vec::new(&env);
        for signer in signers.iter() {
            if !unique_signers.contains(&signer) {
                unique_signers.push_back(signer);
            }
        }
        for signer in unique_signers.iter() {
            signer.require_auth();
        }

        Self::can_spend_multi(
            env.clone(),
            primary_owner.clone(),
            delegate.clone(),
            unique_signers.clone(),
            amount,
            merchant.clone(),
        )?;

        let key = DataKey::MultiPermission(primary_owner.clone(), delegate.clone());
        let mut record: MultiOwnerPermission = env.storage().persistent().get(&key).unwrap();

        record.spent = record
            .spent
            .checked_add(amount)
            .filter(|spent| *spent <= record.limit_total)
            .ok_or(PermissionError::ExceedsAllowance)?;
        env.storage().persistent().set(&key, &record);

        let remaining = record.limit_total - record.spent;

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("mspent")),
            MultiOwnerSpendEvent {
                primary_owner,
                delegate,
                merchant,
                amount,
                remaining,
                signer_count: unique_signers.len(),
            },
        );

        Ok(())
    }

    /// Read-only getter for a multi-owner permission record.
    pub fn get_multi_permission(
        env: Env,
        primary_owner: Address,
        delegate: Address,
    ) -> Result<MultiOwnerPermission, PermissionError> {
        env.storage()
            .persistent()
            .get(&DataKey::MultiPermission(primary_owner, delegate))
            .ok_or(PermissionError::PermissionNotFound)
    }

    /// Dry-run a spend and report whether it would succeed, without mutating state.
    ///
    /// Reuses the identical validation sequence from `can_spend`.  Because this
    /// function never writes to storage it is safe to call at any time without
    /// requiring delegate auth — the caller supplies no secret; they only learn
    /// whether a *particular amount / merchant* would pass.
    ///
    /// # Returns
    /// A [`SpendPreview`] with:
    /// - `allowed = true` when every rule passes.
    /// - `reason` — a short [`Symbol`] code:
    ///   `"ok"`, `"not_found"`, `"expired"`, `"paused"`,
    ///   `"unauthorized"`, `"per_tx_limit"`, `"total_limit"`, `"bad_merchant"`.
    /// - `remaining_after` — how much allowance would be left *after* the
    ///   spend if it were executed; when `allowed = false` this equals the
    ///   current remaining (i.e. the spend is not subtracted).
    pub fn preview_spend(
        env: Env,
        owner: Address,
        delegate: Address,
        amount: i128,
        merchant: Address,
    ) -> SpendPreview {
        // Compute current remaining before we know whether the call will pass.
        let current_remaining: i128 = env
            .storage()
            .persistent()
            .get::<DataKey, PermissionRecord>(&DataKey::Permission(owner.clone(), delegate.clone()))
            .map(|r| r.limit_total - r.spent)
            .unwrap_or(0);

        match Self::can_spend(env.clone(), owner, delegate, amount, merchant) {
            Ok(()) => SpendPreview {
                allowed: true,
                reason: Symbol::new(&env, "ok"),
                remaining_after: current_remaining - amount,
            },
            Err(e) => {
                let reason = match e {
                    PermissionError::PermissionNotFound => Symbol::new(&env, "not_found"),
                    PermissionError::Expired => Symbol::new(&env, "expired"),
                    PermissionError::PermissionPaused => Symbol::new(&env, "paused"),
                    PermissionError::Unauthorized => Symbol::new(&env, "unauthorized"),
                    PermissionError::ExceedsPerTxLimit => Symbol::new(&env, "per_tx_limit"),
                    PermissionError::ExceedsTotalLimit => Symbol::new(&env, "total_limit"),
                    PermissionError::MerchantNotAllowed => Symbol::new(&env, "bad_merchant"),
                    // Remaining variants cannot be returned by can_spend but
                    // exhaustively handled to satisfy the compiler.
                    _ => Symbol::new(&env, "unauthorized"),
                };
                SpendPreview {
                    allowed: false,
                    reason,
                    remaining_after: current_remaining,
                }
            }
        }
    }

    pub fn get_permissions_by_owner(env: Env, owner: Address) -> Vec<PermissionRecord> {
        let delegates: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::UserPermissions(owner.clone()))
            .unwrap_or(Vec::new(&env));

        let mut records = Vec::new(&env);
        for delegate in delegates.iter() {
            if let Some(record) = env
                .storage()
                .persistent()
                .get(&DataKey::Permission(owner.clone(), delegate))
            {
                records.push_back(record);
            }
        }
        records
    }

    pub fn get_permission(env: Env, owner: Address, delegate: Address) -> PermissionRecord {
        let key = DataKey::Permission(owner, delegate);
        env.storage().persistent().get(&key).unwrap()
    }

    pub fn get_permission(env: Env, owner: Address, delegate: Address) -> PermissionRecord {
        let key = DataKey::Permission(owner, delegate);
        env.storage().persistent().get(&key).ok_or(PermissionError::PermissionNotFound)
    }

    pub fn get_remaining_allowance(env: Env, owner: Address, delegate: Address) -> Result<i128, PermissionError> {
        let key = DataKey::Permission(owner, delegate);
        let record: PermissionRecord = env.storage().persistent().get(&key).ok_or(PermissionError::PermissionNotFound)?;
        Ok(record.limit_total - record.spent)
    }

    /// Typed allowance getter: returns limit, spent, remaining (clamped ≥ 0),
    /// and expiry. Returns PermissionError::PermissionNotFound for unknown pairs (issue #98).
    pub fn get_allowance_detail(
        env: Env,
        owner: Address,
        delegate: Address,
    ) -> Result<RemainingAllowance, PermissionError> {
        let key = DataKey::Permission(owner, delegate);
        let record: PermissionRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(PermissionError::PermissionNotFound)?;

        let raw = record.limit_total - record.spent;
        let remaining = if raw < 0 { 0 } else { raw };

        Ok(RemainingAllowance {
            limit: record.limit_total,
            spent: record.spent,
            remaining,
            expires_at_ledger: record.expires_at_ledger,
        })
    }

    /// Increase the total allowance for an existing permission grant.
    ///
    /// Emits [`AllowanceIncreasedEvent`] only when `amount > 0` and the limit
    /// actually rises. A zero `amount` is a no-op: storage is untouched and no
    /// event is published.
    pub fn increase_allowance(
        env: Env,
        owner: Address,
        delegate: Address,
        amount: i128,
    ) -> Result<(), PermissionError> {
        owner.require_auth();

        if amount == 0 {
            return Ok(());
        }
        if amount < 0 {
            return Err(PermissionError::InvalidParam);
        }

        let perm_key = DataKey::Permission(owner.clone(), delegate.clone());
        let mut record: PermissionRecord = env
            .storage()
            .persistent()
            .get(&perm_key)
            .ok_or(PermissionError::PermissionNotFound)?;

        let old_limit = record.limit_total;
        let new_limit = old_limit
            .checked_add(amount)
            .ok_or(PermissionError::InvalidParam)?;

        record.limit_total = new_limit;
        env.storage().persistent().set(&perm_key, &record);

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("allowinc")),
            AllowanceIncreasedEvent {
                owner,
                delegate,
                old_limit,
                new_limit,
            },
        );

        Ok(())
    }

    pub fn decrease_allowance(
        env: Env,
        owner: Address,
        delegate: Address,
        amount: i128,
    ) -> Result<(), PermissionError> {
        owner.require_auth();

        if amount <= 0 {
            return Err(PermissionError::InvalidParam);
        }

        let perm_key = DataKey::Permission(owner.clone(), delegate.clone());
        let record: PermissionRecord = env.storage().persistent().get(&perm_key).unwrap();

        let pend_key = DataKey::PendingDecrement(owner.clone(), delegate.clone());
        if env.storage().persistent().has(&pend_key) {
            return Err(PermissionError::PendingDecreaseExists);
        }

        if record.limit_total - amount < record.spent {
            return Err(PermissionError::LimitBelowSpent);
        }

        let execution_time = env.ledger().timestamp() + 86400;
        let execution_time = env.ledger().timestamp() + Self::get_decrease_timelock_secs(env.clone());

        let pending = PendingAllowanceDecrement {
            amount,
            execution_time,
        };

        env.storage().persistent().set(&pend_key, &pending);

        Ok(())
    }

    pub fn execute_decrease_allowance(
        env: Env,
        owner: Address,
        delegate: Address,
    ) -> Result<(), PermissionError> {
        owner.require_auth();

        let pend_key = DataKey::PendingDecrement(owner.clone(), delegate.clone());
        let pending: PendingAllowanceDecrement = env.storage().persistent().get(&pend_key).unwrap();

        if pending.amount <= 0 {
            return Err(PermissionError::InvalidParam);
        }

        if env.ledger().timestamp() < pending.execution_time {
            return Err(PermissionError::TimeLockActive);
        }

        let perm_key = DataKey::Permission(owner.clone(), delegate.clone());
        let mut record: PermissionRecord = env.storage().persistent().get(&perm_key).unwrap();

        let previous_limit = record.limit_total;
        let new_limit = record.limit_total - pending.amount;
        if new_limit < record.spent {
            return Err(PermissionError::LimitBelowSpent);
        }

        record.limit_total = new_limit;
        env.storage().persistent().set(&perm_key, &record);
        env.storage().persistent().remove(&pend_key);

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("allowdec")),
            AllowanceDecreasedEvent {
                owner,
                delegate,
                old_limit: previous_limit,
                new_limit,
            },
        );

        Ok(())
    }

    pub fn pause(env: Env, owner: Address, delegate: Address) -> Result<(), PermissionError> {
        owner.require_auth();

        let perm_key = DataKey::Permission(owner.clone(), delegate.clone());
        let mut record: PermissionRecord = match env.storage().persistent().get(&perm_key) {
            Some(r) => r,
            None => return Err(PermissionError::PermissionNotFound),
        };

        if record.status != PermissionStatus::Active {
            return Err(PermissionError::AlreadyPaused);
        }

        record.status = PermissionStatus::Paused;
        env.storage().persistent().set(&perm_key, &record);

        let reason_code = symbol_short!("none");
        env.storage().persistent().set(
            &DataKey::PauseMetadata(owner.clone(), delegate.clone()),
            &PauseMetadata {
                paused_by: owner.clone(),
                reason_code: reason_code.clone(),
                paused_at_ledger: env.ledger().sequence(),
            },
        );

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("paused")),
            PermissionPausedEvent {
                owner: owner.clone(),
                delegate: delegate.clone(),
                paused_by: owner.clone(),
                reason_code,
            },
        );

        Self::append_audit_log(
            &env,
            &owner,
            &delegate,
            owner.clone(),
            symbol_short!("paused"),
        );

        Ok(())
    }

    pub fn resume(env: Env, owner: Address, delegate: Address) -> Result<(), PermissionError> {
        owner.require_auth();

        let perm_key = DataKey::Permission(owner.clone(), delegate.clone());
        let mut record: PermissionRecord = match env.storage().persistent().get(&perm_key) {
            Some(r) => r,
            None => return Err(PermissionError::PermissionNotFound),
        };

        if record.status == PermissionStatus::Active {
            return Err(PermissionError::AlreadyActive);
        }

        record.status = PermissionStatus::Active;
        env.storage().persistent().set(&perm_key, &record);
        env.storage()
            .persistent()
            .remove(&DataKey::PauseMetadata(owner.clone(), delegate.clone()));

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("resumed")),
            PermissionResumedEvent {
                owner: owner.clone(),
                delegate: delegate.clone(),
                resumed_by: owner.clone(),
            },
        );

        Self::append_audit_log(
            &env,
            &owner,
            &delegate,
            owner.clone(),
            symbol_short!("resumed"),
        );

        Ok(())
    }

    /// Returns the stored pause metadata for `(owner, delegate)`.
    ///
    /// # Errors
    /// [`PermissionError::PermissionNotFound`] if no pause metadata is stored
    /// for the pair.
    pub fn get_pause_metadata(
        env: Env,
        owner: Address,
        delegate: Address,
    ) -> Result<PauseMetadata, PermissionError> {
        env.storage()
            .persistent()
            .get(&DataKey::PauseMetadata(owner, delegate))
            .ok_or(PermissionError::PermissionNotFound)
    }

    pub fn set_admin(env: Env, admin: Address) {
        admin.require_auth();
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("Admin already set");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
    }

    /// Propose a new admin as part of a two-step admin transfer.
    /// Only the current admin can propose. Stores the proposed address
    /// until `accept_admin` is called by that address.
    pub fn propose_admin(
        env: Env,
        caller: Address,
        new_admin: Address,
    ) -> Result<(), PermissionError> {
        caller.require_auth();
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(PermissionError::NotFound)?;
        if caller != stored_admin {
            return Err(PermissionError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("adm_prop")),
            AdminProposedEvent {
                current_admin: caller,
                new_admin,
            },
        );

        Ok(())
    }

    /// Accept a previously proposed admin role.
    /// Only the address stored by `propose_admin` may call this.
    pub fn accept_admin(env: Env, caller: Address) -> Result<(), PermissionError> {
        caller.require_auth();
        let pending: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .ok_or(PermissionError::NotFound)?;
        if caller != pending {
            return Err(PermissionError::Unauthorized);
        }
        let previous_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(PermissionError::NotFound)?;
        env.storage().instance().set(&DataKey::Admin, &caller);
        env.storage().instance().remove(&DataKey::PendingAdmin);

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("adm_acc")),
            AdminAcceptedEvent {
                previous_admin,
                new_admin: caller,
            },
        );

        Ok(())
    }

    /// Pause new grant creation. Admin-only.
    pub fn pause_grants(env: Env, admin: Address) -> Result<(), PermissionError> {
        Self::require_admin(&env, &admin)?;

        let state = PermissionPauseState {
            grants_paused: true,
            updated_at_ledger: env.ledger().sequence(),
        };
        env.storage()
            .instance()
            .set(&DataKey::GrantPauseState, &state);

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("gpaused")),
            GrantPauseChangedEvent {
                grants_paused: true,
                changed_by: admin,
                ledger: state.updated_at_ledger,
            },
        );

        Ok(())
    }

    /// Unpause new grant creation. Admin-only.
    pub fn unpause_grants(env: Env, admin: Address) -> Result<(), PermissionError> {
        Self::require_admin(&env, &admin)?;

        let state = PermissionPauseState {
            grants_paused: false,
            updated_at_ledger: env.ledger().sequence(),
        };
        env.storage()
            .instance()
            .set(&DataKey::GrantPauseState, &state);

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("gpaused")),
            GrantPauseChangedEvent {
                grants_paused: false,
                changed_by: admin,
                ledger: state.updated_at_ledger,
            },
        );

        Ok(())
    }

    /// Read the current grant pause state.
    pub fn get_grant_pause_state(env: Env) -> PermissionPauseState {
        env.storage()
            .instance()
            .get(&DataKey::GrantPauseState)
            .unwrap_or(PermissionPauseState {
                grants_paused: false,
                updated_at_ledger: 0,
            })
    }

    /// Configure the inactivity threshold (in seconds) used by `sweep_inactive`.
    /// Admin-only (issue #338).
    pub fn set_inactivity_threshold(
        env: Env,
        admin: Address,
        threshold_seconds: u64,
    ) -> Result<(), PermissionError> {
        Self::require_admin(&env, &admin)?;
        if threshold_seconds == 0 {
            return Err(PermissionError::InvalidParam);
        }
        env.storage()
            .instance()
            .set(&DataKey::InactivityThreshold, &threshold_seconds);
        Ok(())
    }

    /// Read the currently configured inactivity threshold (seconds). Returns
    /// `0` when the admin has not configured one yet.
    pub fn get_inactivity_threshold(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::InactivityThreshold)
            .unwrap_or(0)
    }

    /// Auto-revoke a permission that has never been spent against and has sat
    /// idle past the configured inactivity threshold (issue #338).
    ///
    /// Reclaims on-chain storage from grants that were created but never
    /// used. Callable by anyone — the eligibility rules (zero spend, elapsed
    /// threshold, still `Active`) are the sole gate, not caller identity.
    ///
    /// Returns `Ok(true)` when the permission was revoked, `Ok(false)` when
    /// it exists but is not (yet) eligible (has spend, isn't `Active`, or the
    /// threshold hasn't elapsed). Returns `Err(PermissionNotFound)` when no permission
    /// exists for the pair, and `Err(InactivityThresholdNotSet)` when the
    /// admin has not configured a threshold.
    pub fn sweep_inactive(
        env: Env,
        owner: Address,
        delegate: Address,
        caller: Address,
    ) -> Result<bool, PermissionError> {
        caller.require_auth();

        let key = DataKey::Permission(owner.clone(), delegate.clone());
        let mut record: PermissionRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(PermissionError::PermissionNotFound)?;

        if record.status != PermissionStatus::Active || record.spent != 0 {
            return Ok(false);
        }

        let threshold = Self::get_inactivity_threshold(env.clone());
        if threshold == 0 {
            return Err(PermissionError::InactivityThresholdNotSet);
        }

        let inactive_since = record.created_at + threshold;
        if env.ledger().timestamp() < inactive_since {
            return Ok(false);
        }

        record.status = PermissionStatus::Revoked;
        env.storage().persistent().set(&key, &record);
        env.storage()
            .persistent()
            .remove(&DataKey::PendingDecrement(owner.clone(), delegate.clone()));

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("autorevk")),
            PermissionRevokedEvent {
                owner: owner.clone(),
                delegate: delegate.clone(),
            },
        );

        Self::append_audit_log(&env, &owner, &delegate, caller, symbol_short!("autorevk"));

        Ok(true)
    }

    /// Transitions a permission whose TTL has elapsed from `Active` to
    /// `Expired`, making the `PermissionStatus::Expired` variant reachable
    /// in stored state rather than only computed on the fly. `is_active`,
    /// `can_spend`, and `get_delegate_status` already treat an
    /// on-the-fly-expired `Active` record as not spendable, so this is a
    /// bookkeeping transition rather than a behavior change for those
    /// reads — it lets `get_permission`/`get_multi_permission` callers see
    /// `Expired` in the stored `status` field without recomputing it
    /// themselves. Returns `Ok(true)` if a transition occurred, `Ok(false)`
    /// if the record was not eligible (already non-`Active`, or not yet
    /// past its expiry).
    pub fn sweep_expired(
        env: Env,
        owner: Address,
        delegate: Address,
        caller: Address,
    ) -> Result<bool, PermissionError> {
        caller.require_auth();

        let key = DataKey::Permission(owner.clone(), delegate.clone());
        let mut record: PermissionRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(PermissionError::PermissionNotFound)?;

        if record.status != PermissionStatus::Active {
            return Ok(false);
        }
        if env.ledger().sequence() < record.expires_at_ledger {
            return Ok(false);
        }

        record.status = PermissionStatus::Expired;
        env.storage().persistent().set(&key, &record);

        Self::append_audit_log(&env, &owner, &delegate, caller, symbol_short!("expired"));

        Ok(true)
    }

    /// Bounded batch version of [`Self::sweep_expired`].
    ///
    /// Callable by anyone. Up to `MAX_SWEEP_BATCH` (50) `(owner, delegate)` pairs can be provided.
    /// Iterates through the pairs, updating eligible records to `PermissionStatus::Expired`.
    /// Returns the number of transitioned records.
    pub fn sweep_expired_batch(
        env: Env,
        pairs: Vec<(Address, Address)>,
        caller: Address,
    ) -> Result<u32, PermissionError> {
        caller.require_auth();

        if pairs.len() > MAX_SWEEP_BATCH {
            return Err(PermissionError::InvalidParam);
        }

        let mut transitioned: u32 = 0;
        for (owner, delegate) in pairs.iter() {
            let key = DataKey::Permission(owner.clone(), delegate.clone());
            if let Some(mut record) = env.storage().persistent().get::<_, PermissionRecord>(&key) {
                if record.status == PermissionStatus::Active
                    && env.ledger().sequence() >= record.expires_at_ledger
                {
                    record.status = PermissionStatus::Expired;
                    env.storage().persistent().set(&key, &record);
                    Self::append_audit_log(
                        &env,
                        &owner,
                        &delegate,
                        caller.clone(),
                        symbol_short!("expired"),
                    );
                    transitioned += 1;
                }
            }
        }

        Ok(transitioned)
    }

    /// Bounded batch version of [`Self::sweep_inactive`].
    ///
    /// Callable by anyone. Up to `MAX_SWEEP_BATCH` (50) `(owner, delegate)` pairs can be provided.
    /// Iterates through the pairs, revoking inactive permissions that have sat idle past the inactivity threshold.
    /// Returns the number of transitioned records.
    pub fn sweep_inactive_batch(
        env: Env,
        pairs: Vec<(Address, Address)>,
        caller: Address,
    ) -> Result<u32, PermissionError> {
        caller.require_auth();

        if pairs.len() > MAX_SWEEP_BATCH {
            return Err(PermissionError::InvalidParam);
        }

        let threshold = Self::get_inactivity_threshold(env.clone());
        if threshold == 0 {
            return Err(PermissionError::InactivityThresholdNotSet);
        }

        let current_time = env.ledger().timestamp();
        let mut transitioned: u32 = 0;

        for (owner, delegate) in pairs.iter() {
            let key = DataKey::Permission(owner.clone(), delegate.clone());
            if let Some(mut record) = env.storage().persistent().get::<_, PermissionRecord>(&key) {
                if record.status == PermissionStatus::Active
                    && record.spent == 0
                    && current_time >= record.created_at + threshold
                {
                    record.status = PermissionStatus::Revoked;
                    env.storage().persistent().set(&key, &record);
                    env.storage()
                        .persistent()
                        .remove(&DataKey::PendingDecrement(owner.clone(), delegate.clone()));

                    env.events().publish(
                        (symbol_short!("perm"), symbol_short!("autorevk")),
                        PermissionRevokedEvent {
                            owner: owner.clone(),
                            delegate: delegate.clone(),
                        },
                    );

                    Self::append_audit_log(
                        &env,
                        &owner,
                        &delegate,
                        caller.clone(),
                        symbol_short!("autorevk"),
                    );

                    transitioned += 1;
                }
            }
        }

        Ok(transitioned)
    }

    /// Configure the minimum number of ledgers that must elapse between successive
    /// spends for any delegation pair (#324). Admin-only.
    ///
    /// Set `interval` to `0` to disable velocity limiting.
    /// Configure the delay before a scheduled allowance decrease can execute.
    /// Admin-only; values are bounded to prevent disabling the security delay.
    pub fn set_decrease_timelock_secs(
        env: Env,
        admin: Address,
        secs: u64,
    ) -> Result<(), PermissionError> {
        Self::require_admin(&env, &admin)?;

        if secs == 0 || secs > MAX_DECREASE_TIMELOCK_SECS {
            return Err(PermissionError::InvalidParam);
        }

        env.storage()
            .instance()
            .set(&DataKey::DecreaseTimelockSecs, &secs);
        Ok(())
    }

    /// Returns the configured allowance-decrease timelock in seconds.
    pub fn get_decrease_timelock_secs(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::DecreaseTimelockSecs)
            .unwrap_or(DEFAULT_DECREASE_TIMELOCK_SECS)
    }

    pub fn set_velocity_limit(
        env: Env,
        admin: Address,
        interval: u32,
    ) -> Result<(), PermissionError> {
        Self::require_admin(&env, &admin)?;

        if interval > MAX_VELOCITY_INTERVAL {
            return Err(PermissionError::InvalidParam);
        }

        let previous: Option<u32> = env.storage().instance().get(&DataKey::MinSpendInterval);

        env.storage()
            .instance()
            .set(&DataKey::MinSpendInterval, &interval);

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("velset")),
            VelocityLimitSetEvent {
                previous,
                current: interval,
                set_by: admin,
            },
        );

        Ok(())
    }

    /// Returns the currently configured minimum spend interval (in ledgers).
    /// Returns `0` when no velocity limit has been set.
    pub fn get_velocity_limit(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::MinSpendInterval)
            .unwrap_or(0)
    }

    /// Returns contract name and semantic version for deployment verification (issue #103).
    pub fn version(env: Env) -> ContractVersion {
        ContractVersion {
            name: Symbol::new(&env, CONTRACT_NAME),
            semver: Symbol::new(&env, CONTRACT_SEMVER),
        }
    }

    /// Allow or forbid self-delegation globally. Admin-only (issue #182).
    pub fn set_allow_self_delegation(
        env: Env,
        admin: Address,
        allow: bool,
    ) -> Result<(), PermissionError> {
        Self::require_admin(&env, &admin)?;
        env.storage()
            .instance()
            .set(&DataKey::AllowSelfDelegation, &allow);
        Ok(())
    }

    /// Register an approved `PermissionMetadata.schema` identifier. Admin-only (issue #328).
    pub fn register_schema(
        env: Env,
        admin: Address,
        schema: Symbol,
    ) -> Result<(), PermissionError> {
        Self::require_admin(&env, &admin)?;

        let mut registry: Vec<Symbol> = env
            .storage()
            .instance()
            .get(&DataKey::SchemaRegistry)
            .unwrap_or_else(|| Vec::new(&env));
        if !registry.contains(&schema) {
            registry.push_back(schema.clone());
            env.storage()
                .instance()
                .set(&DataKey::SchemaRegistry, &registry);
        }

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("schemreg")),
            SchemaRegisteredEvent { admin, schema },
        );

        Ok(())
    }

    /// Returns the list of approved metadata schema identifiers (issue #328).
    pub fn get_registered_schemas(env: Env) -> Vec<Symbol> {
        env.storage()
            .instance()
            .get(&DataKey::SchemaRegistry)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Grants a permission and stores optional metadata hash (issue #181).
    ///
    /// Grants a permission and stores optional metadata hash (issue #181).
    ///
    /// When `metadata` is provided, its `schema` must already be registered
    /// via `register_schema` — unregistered schemas are rejected with
    /// `PermissionError::UnknownSchema` and no grant is recorded (issue #328).
    ///
    /// This is a **first grant**: like [`Self::grant`], it rejects a live
    /// (Active/Paused) existing permission with `PermissionError::AlreadyGranted`
    /// (issue #51). Use [`Self::re_grant_with_metadata`] to replace one.
    pub fn grant_with_metadata(
        env: Env,
        owner: Address,
        delegate: Address,
        limit_total: i128,
        limit_per_tx: i128,
        allowed_merchants: Vec<Address>,
        ttl_ledgers: u32,
        metadata: Option<PermissionMetadata>,
    ) -> Result<(), PermissionError> {
        Self::validate_metadata_schema(&env, &metadata)?;
        Self::grant_impl(
            env.clone(),
            owner.clone(),
            delegate.clone(),
            limit_total,
            limit_per_tx,
            allowed_merchants,
            ttl_ledgers,
            false,
        )?;
        Self::store_metadata(&env, &owner, &delegate, metadata);
        Ok(())
    }

    /// Explicitly replaces an existing permission while storing optional
    /// metadata (issue #51). Semantics mirror [`Self::re_grant`] combined with
    /// the metadata handling of [`Self::grant_with_metadata`]: permitted on a
    /// live permission, fails with `PermissionError::PermissionNotFound` if no
    /// record exists, and the granted event reports the previous spent amount.
    pub fn re_grant_with_metadata(
        env: Env,
        owner: Address,
        delegate: Address,
        limit_total: i128,
        limit_per_tx: i128,
        allowed_merchants: Vec<Address>,
        ttl_ledgers: u32,
        metadata: Option<PermissionMetadata>,
    ) -> Result<(), PermissionError> {
        Self::validate_metadata_schema(&env, &metadata)?;
        Self::grant_impl(
            env.clone(),
            owner.clone(),
            delegate.clone(),
            limit_total,
            limit_per_tx,
            allowed_merchants,
            ttl_ledgers,
            true,
        )?;
        Self::store_metadata(&env, &owner, &delegate, metadata);
        Ok(())
    }

    /// Rejects metadata whose `schema` is not in the approved registry (issue #328).
    fn validate_metadata_schema(
        env: &Env,
        metadata: &Option<PermissionMetadata>,
    ) -> Result<(), PermissionError> {
        if let Some(ref m) = metadata {
            let registry: Vec<Symbol> = env
                .storage()
                .instance()
                .get(&DataKey::SchemaRegistry)
                .unwrap_or_else(|| Vec::new(env));
            if !registry.contains(&m.schema) {
                return Err(PermissionError::UnknownSchema);
            }
        }
        Ok(())
    }

    /// Stores provided metadata, or clears any stale metadata from a previous
    /// grant so `get_metadata` never returns a hash that belongs to an older
    /// policy.
    fn store_metadata(
        env: &Env,
        owner: &Address,
        delegate: &Address,
        metadata: Option<PermissionMetadata>,
    ) {
        let meta_key = DataKey::Metadata(owner.clone(), delegate.clone());
        match metadata {
            Some(m) => env.storage().persistent().set(&meta_key, &m),
            None => {
                if env.storage().persistent().has(&meta_key) {
                    env.storage().persistent().remove(&meta_key);
                }
            }
        }
    }

    /// Returns optional metadata for a permission grant (issue #181).
    pub fn get_metadata(env: Env, owner: Address, delegate: Address) -> Option<PermissionMetadata> {
        env.storage()
            .persistent()
            .get(&DataKey::Metadata(owner, delegate))
    }

    /// Returns the merchant restriction configured under the spending
    /// permission for the given delegation pair, or `None` when no
    /// permission exists or the whitelist is empty.
    ///
    /// This is a read-only getter; it does not mutate allowance counters
    /// or TTLs.
    pub fn get_merchant_restriction(
        env: Env,
        owner: Address,
        delegate: Address,
    ) -> Option<MerchantRestriction> {
        let key = DataKey::Permission(owner.clone(), delegate.clone());
        let record: PermissionRecord = env.storage().persistent().get(&key)?;

        let merchant = record.allowed_merchants.get(0);

        Some(MerchantRestriction {
            owner,
            delegate,
            merchant,
        })
    }

    /// Returns the full whitelisted-merchant list for a delegation pair,
    /// bounded by `MAX_MERCHANTS_PER_PERMISSION`. Returns `None` when no
    /// permission record exists; an empty `Vec` when the record exists but
    /// has no merchant restriction configured.
    pub fn get_merchant_restrictions(
        env: Env,
        owner: Address,
        delegate: Address,
    ) -> Option<MerchantRestrictionView> {
        let key = DataKey::Permission(owner, delegate);
        let record: PermissionRecord = env.storage().persistent().get(&key)?;
        Some(MerchantRestrictionView {
            merchants: record.allowed_merchants,
        })
    }

    /// Returns a compact receipt for an existing permission grant (issue #180).
    /// Includes active status derived from stored state and current ledger.
    pub fn get_receipt(
        env: Env,
        owner: Address,
        delegate: Address,
    ) -> Result<PermissionReceipt, PermissionError> {
        let key = DataKey::Permission(owner.clone(), delegate.clone());
        let record: PermissionRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(PermissionError::PermissionNotFound)?;

        let active = matches!(record.status, PermissionStatus::Active)
            && env.ledger().sequence() < record.expires_at_ledger;

        Ok(PermissionReceipt {
            owner,
            delegate,
            limit: record.limit_total,
            expires_at_ledger: record.expires_at_ledger,
            active,
        })
    }

    /// Returns on-chain usage analytics for a (owner, delegate) delegation.
    /// A pair with no recorded spends yet returns all-zero stats.
    pub fn get_usage_stats(env: Env, owner: Address, delegate: Address) -> PermissionUsageStats {
        env.storage()
            .persistent()
            .get(&DataKey::UsageStats(owner, delegate))
            .unwrap_or(PermissionUsageStats {
                total_spends: 0,
                total_spent: 0,
                average_spend: 0,
                largest_spend: 0,
                first_spend_ledger: 0,
                last_spend_ledger: 0,
            })
    }

    /// Returns non-truncating usage telemetry for a (owner, delegate)
    /// delegation, including a BPS-denominated average spend that avoids
    /// the coarse rounding plain integer division produces on
    /// `PermissionUsageStats::average_spend`. A pair with no recorded
    /// spends returns zeroed stats with `last_spend_ledger: None`.
    pub fn get_usage_stats_bps(env: Env, owner: Address, delegate: Address) -> UsageStatsView {
        let stats = Self::get_usage_stats(env, owner, delegate);

        let average_spent_bps = if stats.total_spends == 0 {
            0
        } else {
            let total_spent_u = if stats.total_spent < 0 {
                0i128
            } else {
                stats.total_spent
            };
            total_spent_u
                .saturating_mul(10_000)
                .checked_div(stats.total_spends as i128)
                .unwrap_or(0) as u64
        };

        UsageStatsView {
            spend_count: stats.total_spends,
            total_spent: stats.total_spent,
            average_spent_bps,
            last_spend_ledger: if stats.total_spends == 0 {
                None
            } else {
                Some(stats.last_spend_ledger)
            },
        }
    }

    /// Returns the total spent amount and the ledger sequence of the most
    /// recent delegated spend for a (owner, delegate) pair.
    pub fn get_permission_usage(env: Env, owner: Address, delegate: Address) -> PermissionUsage {
        let key = DataKey::Permission(owner.clone(), delegate.clone());
        let spent = if let Some(record) = env
            .storage()
            .persistent()
            .get::<DataKey, PermissionRecord>(&key)
        {
            record.spent
        } else {
            0
        };

        let last_spend_ledger = env
            .storage()
            .persistent()
            .get::<DataKey, u32>(&DataKey::LastSpendLedger(owner, delegate));

        PermissionUsage {
            spent,
            last_spend_ledger,
        }
    }

    /// Returns a compact status view for a delegate: whether they can currently
    /// spend, why not if blocked, and how much allowance remains (issue #100).
    ///
    /// This is a **pure read-only** getter — it never mutates renewal counters,
    /// spend counters, or any other state.
    ///
    /// # Reason codes
    /// | `reason`     | meaning                                              |
    /// |--------------|------------------------------------------------------|
    /// | `"active"`   | delegate can spend right now                         |
    /// | `"not_found"`| no permission record exists for this pair            |
    /// | `"revoked"`  | permission was explicitly revoked                    |
    /// | `"expired"`  | permission TTL has elapsed                           |
    /// | `"exhausted"`| remaining allowance is zero or negative              |
    /// | `"paused"`   | permission is temporarily paused                    |
    pub fn get_delegate_status(env: Env, owner: Address, delegate: Address) -> DelegateStatusView {
        let key = DataKey::Permission(owner, delegate);
        let record: PermissionRecord = match env.storage().persistent().get(&key) {
            Some(r) => r,
            None => {
                return DelegateStatusView {
                    active: false,
                    reason: Symbol::new(&env, "not_found"),
                    remaining: 0,
                }
            }
        };

        let remaining = {
            let raw = record.limit_total - record.spent;
            if raw < 0 {
                0
            } else {
                raw
            }
        };

        // Check status field first (handles Revoked and Paused).
        match record.status {
            PermissionStatus::Revoked => {
                return DelegateStatusView {
                    active: false,
                    reason: Symbol::new(&env, "revoked"),
                    remaining: 0,
                }
            }
            PermissionStatus::Paused => {
                return DelegateStatusView {
                    active: false,
                    reason: Symbol::new(&env, "paused"),
                    remaining,
                }
            }
            PermissionStatus::Active | PermissionStatus::Expired => {}
        }

        // Check ledger-based expiry.
        if env.ledger().sequence() >= record.expires_at_ledger {
            return DelegateStatusView {
                active: false,
                reason: Symbol::new(&env, "expired"),
                remaining,
            };
        }

        // Check allowance exhaustion.
        if remaining == 0 {
            return DelegateStatusView {
                active: false,
                reason: Symbol::new(&env, "exhausted"),
                remaining: 0,
            };
        }

        DelegateStatusView {
            active: true,
            reason: Symbol::new(&env, "active"),
            remaining,
        }
    }

    /// Like `get_delegate_status`, but surfaces `PermissionStatus` directly
    /// instead of a derived `active`/`reason` pair — so a `Revoked`
    /// permission is distinguishable from an `Expired` one without string
    /// matching on `reason`.
    pub fn get_delegate_status_v2(
        env: Env,
        owner: Address,
        delegate: Address,
    ) -> Option<DelegateStatusV2> {
        let key = DataKey::Permission(owner.clone(), delegate.clone());
        let record: PermissionRecord = env.storage().persistent().get(&key)?;

        let is_expired = env.ledger().sequence() >= record.expires_at_ledger;
        let effective_status = if is_expired && record.status == PermissionStatus::Active {
            PermissionStatus::Expired
        } else {
            record.status.clone()
        };

        let remaining = if effective_status == PermissionStatus::Active {
            let raw = record.limit_total - record.spent;
            if raw < 0 {
                0
            } else {
                raw
            }
        } else {
            0
        };

        Some(DelegateStatusV2 {
            owner,
            delegate,
            status: effective_status,
            remaining,
            expires_at_ledger: record.expires_at_ledger,
        })
    }

    /// Quick-check whether a permission is currently active (exists, has
    /// `Active` status, and has not expired).
    pub fn is_active(env: Env, owner: Address, delegate: Address) -> bool {
        let key = DataKey::Permission(owner, delegate);
        let record: PermissionRecord = match env.storage().persistent().get(&key) {
            Some(r) => r,
            None => return false,
        };
        if record.status != PermissionStatus::Active {
            return false;
        }
        env.ledger().sequence() < Self::effective_expiry(&env, &record)
    }

    /// Returns the audit log for a (owner, delegate) pair, or an empty vec
    /// when no actions have been recorded yet.
    pub fn get_audit_log(env: Env, owner: Address, delegate: Address) -> Vec<AuditLogEntry> {
        env.storage()
            .persistent()
            .get(&DataKey::AuditLog(owner, delegate))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Appends an `AuditLogEntry` to the persistent log for `(owner, delegate)`.
    /// Called internally after any state-changing operation.
    fn append_audit_log(
        env: &Env,
        owner: &Address,
        delegate: &Address,
        actor: Address,
        action: Symbol,
    ) {
        let key = DataKey::AuditLog(owner.clone(), delegate.clone());
        let mut log: Vec<AuditLogEntry> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(env));

        log.push_back(AuditLogEntry {
            action,
            actor,
            timestamp: env.ledger().timestamp(),
        });
        Self::retain_audit(&mut log, MAX_AUDIT_ENTRIES);

        env.storage().persistent().set(&key, &log);
    }

    /// Drops the oldest entries from `log` until its length is at most
    /// `cap`, keeping per-permission audit storage flat regardless of how
    /// many state-changing actions the pair accumulates over its lifetime.
    fn retain_audit(log: &mut Vec<AuditLogEntry>, cap: u32) {
        while log.len() > cap {
            log.remove(0);
        }
    }

    /// spend. Called from both `execute_spend` and `execute_spend_via_relayer`
    /// so relayed spends are reflected in the same analytics.
    fn record_spend_stats(env: &Env, owner: &Address, delegate: &Address, amount: i128) {
        let key = DataKey::UsageStats(owner.clone(), delegate.clone());
        let ledger = env.ledger().sequence();

        let mut stats: PermissionUsageStats =
            env.storage()
                .persistent()
                .get(&key)
                .unwrap_or(PermissionUsageStats {
                    total_spends: 0,
                    total_spent: 0,
                    average_spend: 0,
                    largest_spend: 0,
                    first_spend_ledger: ledger,
                    last_spend_ledger: ledger,
                });

        if stats.total_spends == 0 {
            stats.first_spend_ledger = ledger;
        }
        stats.total_spends += 1;
        stats.total_spent += amount;
        stats.average_spend = stats.total_spent / stats.total_spends as i128;
        stats.last_spend_ledger = ledger;
        if amount > stats.largest_spend {
            stats.largest_spend = amount;
        }

        env.storage().persistent().set(&key, &stats);
    }
}

#[cfg(test)]
mod integration_tests;
#[cfg(test)]
mod test;

#[cfg(test)]
mod absent_key_tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn absent_pair() -> (Env, Address, Address) {
        let env = Env::default();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        (env, owner, delegate)
    }

    #[test]
    fn get_permission_absent_returns_not_found() {
        let (env, owner, delegate) = absent_pair();
        assert_eq!(
            PermissionsContract::get_permission(env, owner, delegate),
            Err(PermissionError::PermissionNotFound)
        );
    }

    #[test]
    fn get_remaining_allowance_absent_returns_not_found() {
        let (env, owner, delegate) = absent_pair();
        assert_eq!(
            PermissionsContract::get_remaining_allowance(env, owner, delegate),
            Err(PermissionError::PermissionNotFound)
        );
    }

    #[test]
    fn get_pause_metadata_absent_returns_not_found() {
        let (env, owner, delegate) = absent_pair();
        assert_eq!(
            PermissionsContract::get_pause_metadata(env, owner, delegate),
            Err(PermissionError::PermissionNotFound)
        );
    }
}
