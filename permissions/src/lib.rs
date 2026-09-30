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

/// Computes the canonical, version-bound domain separator for this contract
/// deployment.
///
/// The separator binds signed meta-transaction payloads to both the concrete
/// contract address and the contract's semver string, so a signature produced
/// against one deployment/version cannot be replayed against another after a
/// WASM upgrade (issue: strict domain separator hash invalidation).
///
/// Layout: `sha256( contract_address_xdr || "PERM_V2" || semver_xdr )`.
pub fn compute_versioned_domain_separator(env: &Env) -> BytesN<32> {
    let mut payload = soroban_sdk::Bytes::new(env);
    payload.append(&env.current_contract_address().to_xdr(env));
    payload.append(&symbol_short!("PERM_V2").to_xdr(env));
    payload.append(&Symbol::new(env, CONTRACT_SEMVER).to_xdr(env));
    env.crypto().sha256(&payload).into()
}

/// Builds the exact byte string a delegate signs for a relayed spend: the
/// versioned domain separator followed by the XDR-encoded message. Binding
/// the separator into the signed payload is what makes signatures strictly
/// version-scoped.
fn relayed_spend_signing_payload(env: &Env, message: &RelayedSpendMessage) -> soroban_sdk::Bytes {
    let mut payload = soroban_sdk::Bytes::new(env);
    payload.append(&compute_versioned_domain_separator(env).into());
    payload.append(&message.clone().to_xdr(env));
    payload
}

/// Maximum number of merchant addresses allowed in a permission's whitelist.
/// Prevents overly large merchant lists from increasing storage and execution
/// costs unexpectedly.
pub const MAX_MERCHANTS_PER_PERMISSION: u32 = 25;

/// Maximum number of entrypoint symbols allowed in a single permission's
/// `ScopedPermissionConfig` (issue #369). Mirrors
/// [`MAX_MERCHANTS_PER_PERMISSION`]: a scope is a deliberately short list of
/// pre-authorized function signatures, and the bound keeps the per-spend
/// membership scan's cost predictable.
pub const MAX_FUNCTIONS_PER_PERMISSION: u32 = 10;

/// Maximum number of entries retained in a single (owner, delegate) pair's
/// audit log. Once exceeded, the oldest entry is dropped on each append so
/// long-lived permissions don't accrue unbounded storage.
pub const MAX_AUDIT_ENTRIES: u32 = 200;
/// Maximum number of audit entries returned by one page query.
pub const MAX_AUDIT_PAGE_SIZE: u32 = 20;
/// Upper bound on the velocity limit's minimum-spend-interval, in ledgers.
/// At ~5s per ledger this is roughly one year; anything above this would
/// effectively disable spending forever with no clear signal, so it is
/// rejected outright.
pub const MAX_VELOCITY_INTERVAL: u32 = 6_307_200;
/// Upper bound on the optional wall-clock velocity floor, in seconds (one
/// year), mirroring `MAX_VELOCITY_INTERVAL` for the timestamp dimension.
pub const MAX_VELOCITY_INTERVAL_SECS: u64 = 31_536_000;
/// Upper bound on the rolling spend window, in ledgers (~one year), mirroring
/// `MAX_VELOCITY_INTERVAL` (issue #368).
pub const MAX_ROLLING_WINDOW_LEDGERS: u32 = 6_307_200;
/// Default allowance-decrease timelock in seconds (24 hours).
pub const DEFAULT_DECREASE_TIMELOCK_SECS: u64 = 86_400;
/// Maximum configurable allowance-decrease timelock (30 days).
pub const MAX_DECREASE_TIMELOCK_SECS: u64 = 2_592_000;
pub const MAX_SWEEP_BATCH_SIZE: u32 = 50;
pub const MAX_SWEEP_BATCH: u32 = MAX_SWEEP_BATCH_SIZE;
/// Maximum relayer fee in basis points (100 bps = 1.00% maximum tip, issue #370).
pub const MAX_RELAYER_FEE_BPS: u32 = 100;
/// Maximum absolute relayer fee in stroops (10_000_000 stroops = 1 XLM, issue #370).
pub const MAX_ABSOLUTE_RELAYER_STROOPS: i128 = 10_000_000;
/// Number of ledgers a permission must be expired before it can be pruned by a keeper (issue #374).
pub const PRUNE_EXPIRATION_THRESHOLD_LEDGERS: u32 = 100_000;
/// Maximum depth of a parent-delegation hierarchy. A child permission's
/// `depth_level` must be strictly less than this value to be created.
pub const MAX_HIERARCHY_DEPTH: u32 = 3;

/// Default grace period (in seconds) granted to merchants verified under an
/// older verification policy when the required attestation count increases.
pub const DEFAULT_VERIFICATION_GRACE_PERIOD_SECS: u64 = 2_592_000;
/// Maximum configurable verification grace period (30 days).
pub const MAX_VERIFICATION_GRACE_PERIOD_SECS: u64 = 2_592_000;

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
    /// Nonce cancellation targets a nonce that was already consumed or
    /// would overflow the nonce counter (issue #297)
    NonceAlreadyUsed = 2413,
    /// Spend attempted before the grant's `not_before_ledger` activation ledger
    GrantNotYetActive = 2414,
    /// Spend attempted outside the delegation's configured business-hour /
    /// day-of-week `TimeWindowRestriction` (issue #315)
    OutsideAuthorizedWindow = 2415,
    /// Relayer-submitted signature was created for an earlier execution epoch
    StaleEpoch = 2416,
    /// The invoked contract entrypoint is outside the delegation's
    /// `ScopedPermissionConfig`: either the caller named a target contract
    /// that is not the scoped one, named a function that is not in
    /// `allowed_function_symbols`, or (for a scoped grant) did not state the
    /// invocation at all. Scoping fails closed (issue #369).
    UnauthorizedFunction = 2417,
    /// Admin-gated call made before `set_admin` has ever been called
    NotInitialized = 2500,
    /// Allowance sweep targeted a delegation that is still live
    DelegationNotExpired = 2418,
    /// A child permission grant would exceed `MAX_HIERARCHY_DEPTH`
    MaxHierarchyDepthExceeded = 2419,
    /// Merchant's verification is below the currently required policy
    /// threshold and the grace period has elapsed.
    VerificationBelowPolicy = 2420,
    /// Merchant's verification is below the currently required policy
    /// threshold but still within the grace period.
    VerificationGracePeriod = 2421,
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
        PermissionError::InvalidExpiry as u32,
        PermissionError::NonceAlreadyUsed as u32,
        PermissionError::GrantNotYetActive as u32,
        PermissionError::OutsideAuthorizedWindow as u32,
        PermissionError::StaleEpoch as u32,
        PermissionError::UnauthorizedFunction as u32,
        PermissionError::NotInitialized as u32,
        PermissionError::DelegationNotExpired as u32,
        PermissionError::MaxHierarchyDepthExceeded as u32,
        PermissionError::VerificationBelowPolicy as u32,
        PermissionError::VerificationGracePeriod as u32,
    ];

    #[test]
    fn permission_error_codes_are_unique_and_in_reserved_range() {
        assert_eq!(PERMISSION_ERROR_CODES.len(), 34);
        assert_eq!(PERMISSION_ERROR_CODES.len(), 30);
        assert_eq!(PERMISSION_ERROR_CODES.len(), 31);
        assert_eq!(PERMISSION_ERROR_CODES.len(), 30);

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

/// Validates that a relayer fee does not exceed both the absolute cap (1 XLM)
/// and proportional cap (1% / 100 BPS of the spend amount), preventing rogue
/// relayers from draining a delegator's allowance via excessive tip manipulation (issue #370).
///
/// Returns `Ok(())` if the fee is within safety boundaries, or
/// `Err(PermissionError::InvalidParam)` if `relayer_fee` is negative,
/// exceeds `MAX_ABSOLUTE_RELAYER_STROOPS`, or exceeds `MAX_RELAYER_FEE_BPS`
/// proportion of `spend_amount`.
pub fn validate_relayer_fee(spend_amount: i128, relayer_fee: i128) -> Result<(), PermissionError> {
    if relayer_fee < 0 || relayer_fee > MAX_ABSOLUTE_RELAYER_STROOPS {
        return Err(PermissionError::InvalidParam);
    }
    let max_proportional = (spend_amount * MAX_RELAYER_FEE_BPS as i128) / 10_000;
    if relayer_fee > max_proportional {
        return Err(PermissionError::InvalidParam);
    }
    Ok(())
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
/// Ledger-bounded activation window for a permission grant.
///
/// A spend is only permitted while
/// `not_before_ledger <= current_ledger <= not_after_ledger`.
#[allow(missing_docs)]
pub struct ActiveWindow {
    pub not_before_ledger: u32,
    pub not_after_ledger: u32,
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
    /// Ledger at which this grant becomes spendable. `0` means immediately
    /// active (the pre-existing behaviour).
    pub not_before_ledger: u32,
    /// Ledger after which this grant is no longer spendable.
    pub not_after_ledger: u32,
    /// Owner half of the parent permission's `(owner, delegate)` key, for
    /// permissions created via `grant_child`. `None` for top-level grants.
    pub parent_owner: Option<Address>,
    /// Delegate half of the parent permission's `(owner, delegate)` key,
    /// for permissions created via `grant_child`. `None` for top-level
    /// grants. Together with `parent_owner`, this forms the reference the
    /// issue describes as `parent_permission`.
    pub parent_delegate: Option<Address>,
}
/// Metadata describing a permission's position in a delegation hierarchy.
///
/// `root_owner` is the top-level owner of the chain, `parent_permission_id`
/// is reserved for callers that key permissions by id (currently `None`),
/// and `depth_level` is the number of parent hops above this permission
/// (top-level grants have `depth_level == 0`).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HierarchyMetadata {
    pub root_owner: Address,
    pub parent_permission_id: Option<u64>,
    pub depth_level: u32,
}


#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
/// Version of the execution context for relayed spends on a permission.
pub struct EpochConfig {
    pub current_epoch: u32,
    pub epoch_started_ledger: u32,
}

/// Restricts a delegated permission to a single target contract and an
/// explicit allowlist of contract entrypoints (issue #369).
///
/// Amounts and merchant addresses alone do not describe *what* a delegate is
/// allowed to do. This config lets an owner say "this agent may only ever call
/// `fund` on this one escrow contract" and have the permission contract refuse
/// every other entrypoint, including ones the target contract exposes but the
/// owner never intended to hand over.
///
/// A scope is attached to a `(owner, delegate)` pair and lives in its own
/// storage slot (`DataKey::PermissionScope`) rather than inside
/// [`PermissionRecord`]. That keeps `PermissionRecord`'s serialized shape
/// untouched — permissions granted before this feature keep loading — and
/// mirrors how [`MerchantAllowlist`] (issue #296) stores the optional
/// seller-specific allowlist beside the record. The absence of a scope means
/// the delegation is unscoped and behaves exactly as it did before.
///
/// Semantics are enforced by `can_spend_scoped` / `execute_spend_scoped`:
/// - `target_contract` must equal the contract the invocation names.
/// - `invoked_function` must appear in `allowed_function_symbols`.
/// - Both must be supplied; a scoped grant cannot be spent through the
///   unscoped entrypoints, so the check can never be skipped by omitting it.
///
/// `allowed_function_symbols` is bounded by `MAX_FUNCTIONS_PER_PERMISSION` and
/// must be non-empty and duplicate-free.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopedPermissionConfig {
    pub target_contract: Address,
    pub allowed_function_symbols: Vec<Symbol>,
}

/// Emitted when a delegation's function scope is set, replaced, or cleared
/// (issue #369). `target_contract` is `None` when a scope is cleared, since a
/// cleared scope names no target contract.
#[contracttype]
#[derive(Clone, Debug)]
pub struct PermissionScopeUpdatedEvent {
    pub owner: Address,
    pub delegate: Address,
    pub target_contract: Option<Address>,
    pub function_count: u32,
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

/// Emitted when an expired delegation's token allowance is reset to zero.
#[contracttype]
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct AllowanceReclaimedEvent {
    pub owner: Address,
    pub delegate: Address,
    pub token: Address,
    /// Allowance that was outstanding before the reset.
    pub reclaimed_amount: i128,
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
    pub epoch: u32,
}

/// Canonical payload a delegate signs off-chain to authorize a gasless spend
/// on a specific channel, submitted by a relayer (issue #367). Includes
/// `channel_id` to bind the signature to a specific nonce lane, preventing
/// cross-channel replay.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub struct ChannelRelayedSpendMessage {
    pub owner: Address,
    pub delegate: Address,
    pub merchant: Address,
    pub amount: i128,
    pub channel_id: u32,
    pub nonce: u64,
    pub expiration_ledger: u32,
    pub epoch: u32,
}

/// Signature bundle for a channel-based relayed spend (issue #367).
/// Carries the `channel_id` so the on-chain verifier can look up the correct
/// nonce lane, the `nonce` for replay protection within that lane, and the
/// ed25519 `signature` over the [`ChannelRelayedSpendMessage`].
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub struct ChannelSpendSignature {
    pub channel_id: u32,
    pub nonce: u64,
    pub signature: BytesN<64>,
}

/// Owner-controlled, seller-specific allowlist enforced on every spend for a
/// sensitive `(owner, delegate)` delegation (issue #296).
///
/// When `is_enabled` is true, spends (direct and relayed) are only permitted
/// to merchants in `merchants`; an enabled allowlist with no entries blocks
/// all spends (fail-closed). Bounded by `MAX_MERCHANTS_PER_PERMISSION`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub struct MerchantAllowlist {
    pub is_enabled: bool,
    pub merchants: Vec<Address>,
}

#[contracttype]
#[derive(Clone, Debug)]
/// Emitted when a delegation's seller allowlist is set or updated (issue #296).
#[allow(missing_docs)]
pub struct MerchantAllowlistUpdatedEvent {
    pub owner: Address,
    pub delegate: Address,
    pub is_enabled: bool,
    pub merchant_count: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
/// Emitted when an owner cancels stalled relayer nonces (issue #297).
#[allow(missing_docs)]
pub struct NonceCancelledEvent {
    pub owner: Address,
    pub delegate: Address,
    /// Highest nonce invalidated by this call.
    pub cancelled_nonce: u64,
    /// Next nonce a relayed spend must use after the cancellation.
    pub next_nonce: u64,
}

/// Emitted when an owner bulk-invalidates a range of relayer nonces for a
/// `(owner, delegate)` pair due to a suspected key compromise (issue #335).
#[contracttype]
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct NonceBatchInvalidatedEvent {
    pub owner: Address,
    pub delegate: Address,
    /// Highest nonce that was invalidated by this call (`up_to_nonce`).
    pub up_to_nonce: u64,
    /// New expected nonce — the first nonce a relayed spend must use after
    /// the invalidation (`up_to_nonce + 1`).
    pub next_nonce: u64,
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
#[derive(Clone, Debug, Eq, PartialEq)]
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

/// Emitted when the admin configures the wall-clock velocity floor (#290).
#[contracttype]
#[derive(Clone, Debug)]
pub struct VelocityLimitSecsSetEvent {
    pub previous: Option<u64>,
    pub current: u64,
    pub set_by: Address,
}

/// Velocity state for a single (owner, delegate) pair, returned by
/// `get_velocity_state` (issue #290).
///
/// `min_interval_ledgers` / `last_spend_ledger` drive the authoritative,
/// drift-free check against `env.ledger().sequence()`. The timestamp fields
/// are an optional secondary floor that can only make the limiter stricter.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VelocityLimit {
    pub min_interval_ledgers: u32,
    pub last_spend_ledger: u32,
    pub min_interval_secs: u64,
    pub last_spend_timestamp: u64,
}

/// Business-hour / day-of-week spend restriction for a single (owner,
/// delegate) delegation (issue #315).
///
/// `start_hour_utc`/`end_hour_utc` are hours-of-day in `[0, 24)`. A spend's
/// hour (derived from `env.ledger().timestamp()`) must fall in
/// `[start_hour_utc, end_hour_utc)` when `start_hour_utc < end_hour_utc`, or
/// in `[start_hour_utc, 24) ∪ [0, end_hour_utc)` when `start_hour_utc >
/// end_hour_utc` (an overnight window, e.g. 22 -> 6). Equal start/end hours
/// disable the hour-of-day check (any hour is allowed) so the restriction
/// can be day-of-week-only.
///
/// `allowed_days_bitmap` is a 7-bit mask, bit 0 = Monday through bit 6 =
/// Sunday (bits 7-31 must be zero). A day whose bit is unset is fully
/// blocked regardless of hour.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub struct TimeWindowRestriction {
    pub start_hour_utc: u32,
    pub end_hour_utc: u32,
    pub allowed_days_bitmap: u32,
}

/// Emitted when a delegation's business-hour restriction is set or cleared
/// (issue #315).
#[contracttype]
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct TimeWindowRestrictionSetEvent {
    pub owner: Address,
    pub delegate: Address,
    pub start_hour_utc: u32,
    pub end_hour_utc: u32,
    pub allowed_days_bitmap: u32,
}

/// Rolling-window spend cap for a single (owner, delegate) pair (issue #368).
///
/// Complements the per-transaction limit: even a delegate that stays under
/// `limit_per_tx` on every call cannot move more than `max_spend_in_window`
/// within any `window_ledgers`-long sliding window.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RollingWindowLimit {
    /// Length of the sliding window, in ledgers.
    pub window_ledgers: u32,
    /// Maximum cumulative spend allowed within one window.
    pub max_spend_in_window: i128,
    /// Cumulative spend recorded so far in the current window.
    pub current_window_spend: i128,
    /// Ledger at which the current window started.
    pub window_start_ledger: u32,
}

/// Emitted when the admin configures the rolling-window velocity cap (#368).
#[contracttype]
#[derive(Clone, Debug)]
pub struct RollingWindowSetEvent {
    pub previous_window_ledgers: u32,
    pub previous_max_spend: i128,
    pub window_ledgers: u32,
    pub max_spend_in_window: i128,
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

/// Emitted when an expired permission record is pruned from persistent storage (issue #374).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub struct PermissionPrunedEvent {
    pub owner: Address,
    pub delegate: Address,
    pub keeper: Address,
    pub pruned_at_ledger: u32,
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
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditLogEntry {
    pub action: Symbol,
    pub actor: Address,
    pub timestamp: u64,
}

/// One bounded page from a permission's retained audit trail.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditTrailPage {
    pub entries: Vec<AuditLogEntry>,
    pub total_entries: u32,
    pub next_cursor: Option<u32>,
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

/// On-chain verification policy describing how many attestations a merchant
/// must hold to be considered verified.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub struct VerificationPolicy {
    pub required: u32,
}

/// Per-merchant verification state. `verified_at` records the timestamp at
/// which the merchant last satisfied the policy in force at that time, so a
/// later policy increase can grant a fair grace period rather than instantly
/// invalidating pre-existing merchants.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub struct MerchantVerification {
    pub verifications: u32,
    pub verified_at: u64,
    pub required_at_verification: u32,
}

/// Emitted when a merchant's verification status is re-evaluated against the
/// currently active policy.
#[contracttype]
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct MerchantRevalidatedEvent {
    pub merchant_id: u64,
    pub verifications: u32,
    pub required: u32,
    pub compliant: bool,
    pub grace_deadline: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpendExecutionResult {
    pub remaining_allowance: i128,
    pub new_spend_ledger: u32,
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
    /// Execution epoch for a (owner, delegate) pair's relayed spends.
    ExecutionEpoch(Address, Address),
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
    /// Instance-level minimum number of seconds between successive spends,
    /// checked in addition to `MinSpendInterval` (issue #290).
    MinSpendIntervalSecs,
    /// Ledger timestamp of the last spend for a (owner, delegate) pair.
    LastSpendTimestamp(Address, Address),
    /// Instance-level rolling-window velocity configuration (issue #368).
    RollingWindowConfig,
    /// Per-pair rolling-window spend state (issue #368).
    RollingWindowState(Address, Address),
    /// Legacy serialized audit log retained for lazy migration.
    AuditLog(Address, Address),
    /// Physical ring-buffer index of a retained audit entry.
    AuditLogAt(Address, Address, u32),
    /// Oldest physical ring-buffer slot for a pair's audit log.
    AuditLogStart(Address, Address),
    /// Number of entries retained for a pair, capped at `MAX_AUDIT_ENTRIES`.
    AuditLogCount(Address, Address),
    /// Index of delegate addresses granted by a given owner.
    UserPermissions(Address),
    /// Seller-specific allowlist for a sensitive (owner, delegate) delegation.
    MerchantAllowlist(Address, Address),
    /// Business-hour / day-of-week spend restriction for a (owner, delegate)
    /// delegation (issue #315).
    TimeWindowRestriction(Address, Address),
    /// Function scope restricting a (owner, delegate) delegation to specific
    /// contract entrypoints (issue #369). Absent means unscoped.
    PermissionScope(Address, Address),
    /// Index of owner addresses that have granted permissions to a given delegate.
    DelegatePermissions(Address),
    /// Delegation-hierarchy metadata for a (owner, delegate) permission.
    Hierarchy(Address, Address),
    /// Next expected nonce for a (owner, delegate, channel_id) triple's relayed spends.
    /// Enables parallel multi-channel nonce lanes (issue #367).
    ChannelNonce(Address, Address, u32),
    /// Instance-level active verification policy.
    VerificationPolicy,
    /// Per-merchant verification state, keyed by merchant id.
    MerchantVerification(u64),
    /// Instance-level grace period (seconds) granted to pre-existing
    /// merchants when the required verification count increases.
    VerificationGracePeriodSecs,
}

/// Computes `current + ttl` as an absolute expiry ledger.
///
/// Unchecked `u32` addition would wrap for large TTLs (e.g. `u32::MAX`),
/// producing an expiry in the past and an instantly-expired grant. Overflow
/// is instead reported as [`PermissionError::InvalidExpiry`] (2412).
pub fn compute_expiry_ledger(current: u32, ttl: u32) -> Result<u32, PermissionError> {
    current
        .checked_add(ttl)
        .ok_or(PermissionError::InvalidExpiry)
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
            None,
        )
    }

    /// Returns the number of attestations currently recorded for `merchant_id`.
    fn get_merchant_verifications_count(env: &Env, merchant_id: u64) -> u32 {
        env.storage()
            .persistent()
            .get::<DataKey, MerchantVerification>(&DataKey::MerchantVerification(merchant_id))
            .map(|v| v.verifications)
            .unwrap_or(0)
    }

    /// Returns the currently active verification policy, defaulting to a
    /// zero-attestation policy when none has been configured.
    fn get_active_verification_policy(env: &Env) -> VerificationPolicy {
        env.storage()
            .instance()
            .get(&DataKey::VerificationPolicy)
            .unwrap_or(VerificationPolicy { required: 0 })
    }

    /// Returns the configured grace period (seconds) for pre-existing
    /// merchants when the required verification count increases.
    fn get_verif_grace_secs(env: &Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::VerificationGracePeriodSecs)
            .unwrap_or(DEFAULT_VERIFICATION_GRACE_PERIOD_SECS)
    }

    /// Dynamically evaluates whether `merchant_id` satisfies `policy`.
    ///
    /// Unlike a static boolean flag, this reads the merchant's current
    /// attestation count on every call, so a policy increase immediately
    /// affects the result for merchants verified under an older policy.
    pub fn recheck_merchant_verification(
        env: &Env,
        merchant_id: u64,
        policy: VerificationPolicy,
    ) -> bool {
        let current_verifications = Self::get_merchant_verifications_count(env, merchant_id);
        current_verifications >= policy.required
    }

    /// Sets the active verification policy. Admin-only.
    ///
    /// When the required attestation count increases, merchants already
    /// verified under the previous policy are not immediately invalidated;
    /// instead they receive a grace period (see
    /// `set_verif_grace_secs`) during which they may acquire
    /// the additional attestations.
    pub fn set_verification_policy(
        env: Env,
        admin: Address,
        required: u32,
    ) -> Result<(), PermissionError> {
        Self::require_admin(&env, &admin)?;
        env.storage()
            .instance()
            .set(&DataKey::VerificationPolicy, &VerificationPolicy { required });
        Ok(())
    }

    /// Returns the currently active verification policy.
    pub fn get_verification_policy(env: Env) -> VerificationPolicy {
        Self::get_active_verification_policy(&env)
    }

    /// Configures the grace period (seconds) granted to pre-existing
    /// merchants when the required verification count increases. Admin-only.
    /// Values are bounded to `MAX_VERIFICATION_GRACE_PERIOD_SECS`.
    pub fn set_verif_grace_secs(
        env: Env,
        admin: Address,
        secs: u64,
    ) -> Result<(), PermissionError> {
        Self::require_admin(&env, &admin)?;
        if secs > MAX_VERIFICATION_GRACE_PERIOD_SECS {
            return Err(PermissionError::InvalidParam);
        }
        env.storage()
            .instance()
            .set(&DataKey::VerificationGracePeriodSecs, &secs);
        Ok(())
    }

    /// Records (or updates) the attestation count for a merchant. Admin-only.
    ///
    /// When the merchant meets the currently active policy, `verified_at` is
    /// stamped with the current ledger timestamp so a later policy increase
    /// can compute a fair grace deadline from the moment of verification.
    pub fn set_merchant_verifications(
        env: Env,
        admin: Address,
        merchant_id: u64,
        verifications: u32,
    ) -> Result<(), PermissionError> {
        Self::require_admin(&env, &admin)?;

        let policy = Self::get_active_verification_policy(&env);
        let now = env.ledger().timestamp();
        let existing: Option<MerchantVerification> = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantVerification(merchant_id));

        let verified_at = if verifications >= policy.required {
            now
        } else {
            existing.map(|v| v.verified_at).unwrap_or(0)
        };

        env.storage().persistent().set(
            &DataKey::MerchantVerification(merchant_id),
            &MerchantVerification {
                verifications,
                verified_at,
                required_at_verification: policy.required,
            },
        );

        Ok(())
    }

    /// Returns the stored verification state for a merchant, if any.
    pub fn get_merchant_verification(
        env: Env,
        merchant_id: u64,
    ) -> Option<MerchantVerification> {
        env.storage()
            .persistent()
            .get(&DataKey::MerchantVerification(merchant_id))
    }

    /// Re-evaluates a merchant's verification status against the currently
    /// active policy and returns whether the merchant is compliant.
    ///
    /// Merchants that were verified under an older, lower-threshold policy
    /// are not immediately invalidated when the required count increases.
    /// Instead, they are granted a grace period (measured from the moment
    /// they were last verified) during which they may acquire the additional
    /// attestations. Once the grace period elapses without the merchant
    /// meeting the new threshold, this returns `false` and the merchant is
    /// considered non-compliant.
    ///
    /// # Errors
    /// - [`PermissionError::VerificationGracePeriod`] when the merchant is
    ///   below the required threshold but still within the grace period.
    /// - [`PermissionError::VerificationBelowPolicy`] when the merchant is
    ///   below the required threshold and the grace period has elapsed.
    pub fn revalidate_merchant_status(
        env: Env,
        merchant_id: u64,
    ) -> Result<bool, PermissionError> {
        let policy = Self::get_active_verification_policy(&env);
        let verifications = Self::get_merchant_verifications_count(&env, merchant_id);

        if verifications >= policy.required {
            return Ok(true);
        }

        // Below the current policy threshold. Determine whether the merchant
        // is still within the grace period granted for pre-existing
        // verifications.
        let grace_secs = Self::get_verif_grace_secs(&env);
        let now = env.ledger().timestamp();

        let verified_at = env
            .storage()
            .persistent()
            .get::<DataKey, MerchantVerification>(&DataKey::MerchantVerification(merchant_id))
            .map(|v| v.verified_at)
            .unwrap_or(0);

        let grace_deadline = verified_at.saturating_add(grace_secs);

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("mrevalid")),
            MerchantRevalidatedEvent {
                merchant_id,
                verifications,
                required: policy.required,
                compliant: false,
                grace_deadline,
            },
        );

        if verified_at > 0 && now < grace_deadline {
            Err(PermissionError::VerificationGracePeriod)
        } else {
            Err(PermissionError::VerificationBelowPolicy)
        }
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
            None,
        )
    }

    /// Records a first grant whose authority is restricted to `scope`
    /// (issue #369).
    ///
    /// Behaves exactly like [`Self::grant`] except that the resulting record
    /// carries a [`ScopedPermissionConfig`]: the delegate can only ever
    /// execute the listed entrypoints on `scope.target_contract`, and must
    /// route every spend through [`Self::execute_spend_scoped`] so the
    /// invocation is actually checked.
    ///
    /// Fails with [`PermissionError::InvalidParam`] if `scope` is malformed
    /// (empty or duplicate function symbols, or more than
    /// `MAX_FUNCTIONS_PER_PERMISSION` of them).
    // Reason: Soroban ABI entry point — signature is part of the published
    // on-chain ABI and cannot be restructured without a breaking change.
    #[allow(clippy::too_many_arguments)]
    pub fn grant_scoped(
        env: Env,
        owner: Address,
        delegate: Address,
        limit_total: i128,
        limit_per_tx: i128,
        allowed_merchants: Vec<Address>,
        ttl_ledgers: u32,
        scope: ScopedPermissionConfig,
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
            Some(scope),
        )
    }

    /// Explicitly replaces an existing delegation's terms with a new
    /// [`ScopedPermissionConfig`] (issue #369).
    ///
    /// The mirror of [`Self::grant_scoped`] for a live permission: same
    /// `AlreadyGranted` / `PermissionNotFound` semantics as
    /// [`Self::re_grant`], and the scope is replaced wholesale rather than
    /// merged, so an owner can narrow (or drop, via [`Self::re_grant`]) a
    /// delegate's authority in one call.
    // Reason: Soroban ABI entry point — signature is part of the published
    // on-chain ABI and cannot be restructured without a breaking change.
    #[allow(clippy::too_many_arguments)]
    pub fn re_grant_scoped(
        env: Env,
        owner: Address,
        delegate: Address,
        limit_total: i128,
        limit_per_tx: i128,
        allowed_merchants: Vec<Address>,
        ttl_ledgers: u32,
        scope: ScopedPermissionConfig,
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
            Some(scope),
        )
    }

    /// Shared implementation for `grant` / `re_grant` and their scoped variants.
    ///
    /// `re_grant` opts the caller into replacing an existing live permission
    /// and reports the previous spent / remaining delta on the event so spend
    /// accounting is never silently discarded. `scope` is stored verbatim on
    /// the new record; a `None` scope therefore clears any scope a previous
    /// grant carried, mirroring how `store_metadata` drops stale metadata.
    #[allow(clippy::too_many_arguments)]
    fn grant_impl(
        env: Env,
        owner: Address,
        delegate: Address,
        limit_total: i128,
        limit_per_tx: i128,
        allowed_merchants: Vec<Address>,
        ttl_ledgers: u32,
        re_grant: bool,
        scope: Option<ScopedPermissionConfig>,
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

        // Issue #369: reject a malformed function scope before any state is
        // touched, so a grant never lands with an unenforceable scope.
        if let Some(ref scope) = scope {
            Self::validate_scope_config(&env, scope)?;
        }

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
            not_before_ledger: 0,
            not_after_ledger: 0,
            parent_owner: None,
            parent_delegate: None,
        };

        env.storage().persistent().set(&key, &record);
        let epoch_key = DataKey::ExecutionEpoch(owner.clone(), delegate.clone());
        let epoch_config = env
            .storage()
            .persistent()
            .get(&epoch_key)
            .unwrap_or(EpochConfig {
                current_epoch: 0,
                epoch_started_ledger: env.ledger().sequence(),
            });
        env.storage().persistent().set(
            &epoch_key,
            &epoch_config,
        );

        // Issue #369: a `None` scope clears any scope a previous grant
        // carried, so `re_grant` can never leave a stale, unenforced scope
        // behind. `re_grant_with_metadata` passes the live scope back in, so
        // comparing against the previous value keeps the scope event to
        // genuine changes instead of re-announcing an unchanged scope.
        let previous_scope: Option<ScopedPermissionConfig> =
            Self::load_scope(&env, &owner, &delegate);
        let scope_changed = previous_scope != scope;
        Self::store_scope(&env, &owner, &delegate, scope.clone());

        Self::add_to_address_index(
            &env,
            &DataKey::DelegatePermissions(delegate.clone()),
            &owner,
        );

        // Change in usable allowance caused by this (re-)grant. For a first
        // grant `old_remaining` is 0 so this equals the new total limit; for a
        // re-grant it reflects how much more (or less) the delegate can spend
        // than before.
        let remaining_delta = limit_total - old_remaining;

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("granted"), delegate.clone()),
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
            (symbol_short!("perm"), symbol_short!("merc_list"), delegate.clone()),
            MerchantWhitelistChangedEvent {
                owner: owner.clone(),
                delegate: delegate.clone(),
                merchant_count: allowed_merchants.len(),
            },
        );

        // Issue #369: surface the granted function scope as its own event so an
        // indexer can tell a scoped grant from an unscoped one without reading
        // the scope back out of storage. Suppressed when the scope is carried
        // over unchanged (`re_grant_with_metadata`).
        if scope_changed {
            if let Some(ref scope) = scope {
                env.events().publish(
                    (symbol_short!("perm"), symbol_short!("scope")),
                    PermissionScopeUpdatedEvent {
                        owner: owner.clone(),
                        delegate: delegate.clone(),
                        target_contract: Some(scope.target_contract.clone()),
                        function_count: scope.allowed_function_symbols.len(),
                    },
                );
            }
        }

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
    /// A child never carries its own [`ScopedPermissionConfig`] (issue #369):
    /// function scoping is inherited from the parent chain at spend time, so a
    /// delegate cannot widen the authority its owner granted by handing an
    /// unscoped sub-delegation to a downstream agent.
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
        // Enforce the maximum delegation hierarchy depth. A child's depth is
        // one greater than its parent's; reject before any state is written
        // so a chain can never exceed `MAX_HIERARCHY_DEPTH` levels.
        let parent_depth = Self::hierarchy_depth(
            &env,
            &parent_owner,
            &parent_delegate,
        );
        let child_depth = parent_depth
            .checked_add(1)
            .ok_or(PermissionError::MaxHierarchyDepthExceeded)?;
        if child_depth >= MAX_HIERARCHY_DEPTH {
            return Err(PermissionError::MaxHierarchyDepthExceeded);
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
            not_before_ledger: 0,
            not_after_ledger: 0,
            parent_owner: Some(parent_owner.clone()),
            parent_delegate: Some(parent_delegate.clone()),
        };

        let child_key = DataKey::Permission(parent_delegate.clone(), child_delegate.clone());
        env.storage().persistent().set(&child_key, &record);
        env.storage().persistent().set(
            &DataKey::ExecutionEpoch(parent_delegate.clone(), child_delegate.clone()),
            &EpochConfig {
                current_epoch: 0,
                epoch_started_ledger: env.ledger().sequence(),
            },
        );
        Self::add_to_address_index(
            &env,
            &DataKey::DelegatePermissions(child_delegate.clone()),
            &parent_delegate,
        );
        // Record the child's hierarchy metadata so descendants can compute
        // their own depth without re-walking the whole parent chain.
        let root_owner = Self::hierarchy_root_owner(&env, &parent_owner, &parent_delegate);
        env.storage().persistent().set(
            &DataKey::Hierarchy(parent_delegate.clone(), child_delegate.clone()),
            &HierarchyMetadata {
                root_owner,
                parent_permission_id: None,
                depth_level: child_depth,
            },
        );


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
            (symbol_short!("perm"), symbol_short!("granted"), child_delegate.clone()),
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
            Self::remove_from_address_index(
                &env,
                &DataKey::DelegatePermissions(delegate.clone()),
                &owner,
            );

            record.status = PermissionStatus::Revoked;
            env.storage().persistent().set(&key, &record);
            env.storage()
                .persistent()
                .remove(&DataKey::PendingDecrement(owner.clone(), delegate.clone()));

            env.events().publish(
                (symbol_short!("perm"), symbol_short!("revoked"), delegate.clone()),
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
    /// Returns the `depth_level` recorded for the permission keyed by
    /// `(owner, delegate)`, or `0` when no hierarchy metadata exists (i.e.
    /// the permission is a top-level grant).
    fn hierarchy_depth(env: &Env, owner: &Address, delegate: &Address) -> u32 {
        env.storage()
            .persistent()
            .get::<DataKey, HierarchyMetadata>(&DataKey::Hierarchy(
                owner.clone(),
                delegate.clone(),
            ))
            .map(|m| m.depth_level)
            .unwrap_or(0)
    }

    /// Returns the `root_owner` for the permission keyed by `(owner, delegate)`,
    /// falling back to `owner` itself when no hierarchy metadata exists.
    fn hierarchy_root_owner(env: &Env, owner: &Address, delegate: &Address) -> Address {
        env.storage()
            .persistent()
            .get::<DataKey, HierarchyMetadata>(&DataKey::Hierarchy(
                owner.clone(),
                delegate.clone(),
            ))
            .map(|m| m.root_owner)
            .unwrap_or_else(|| owner.clone())
    }

    /// Returns the recorded hierarchy metadata for a permission, if any.
    pub fn get_hierarchy_metadata(
        env: Env,
        owner: Address,
        delegate: Address,
    ) -> Option<HierarchyMetadata> {
        env.storage()
            .persistent()
            .get(&DataKey::Hierarchy(owner, delegate))
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
            not_before_ledger: old_record.not_before_ledger,
            not_after_ledger: old_record.not_after_ledger,
            parent_owner: old_record.parent_owner.clone(),
            parent_delegate: old_record.parent_delegate.clone(),
        };

        let new_key = DataKey::Permission(owner.clone(), new_delegate.clone());

        // Ensure new permission doesn't already exist
        if env.storage().persistent().has(&new_key) {
            return Err(PermissionError::InvalidParam);
        }

        // Issue #369: a transfer hands the same authority to a new delegate,
        // so the function scope has to travel with it. Forgetting it here
        // would silently escalate a scoped grant into an unscoped one.
        let inherited_scope: Option<ScopedPermissionConfig> =
            Self::load_scope(&env, &owner, &old_delegate);
        Self::store_scope(&env, &owner, &new_delegate, inherited_scope.clone());

        if let Some(ref scope) = inherited_scope {
            env.events().publish(
                (symbol_short!("perm"), symbol_short!("scope")),
                PermissionScopeUpdatedEvent {
                    owner: owner.clone(),
                    delegate: new_delegate.clone(),
                    target_contract: Some(scope.target_contract.clone()),
                    function_count: scope.allowed_function_symbols.len(),
                },
            );
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
        Self::remove_from_address_index(
            &env,
            &DataKey::DelegatePermissions(old_delegate.clone()),
            &owner,
        );
        Self::add_to_address_index(
            &env,
            &DataKey::DelegatePermissions(new_delegate.clone()),
            &owner,
        );

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
            (symbol_short!("perm"), symbol_short!("transf"), new_delegate.clone()),
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
                        (symbol_short!("perm"), symbol_short!("exp_cap"), delegate.clone()),
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
                (symbol_short!("perm"), symbol_short!("renewed"), delegate.clone()),
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
            (symbol_short!("perm"), symbol_short!("exp_upd"), delegate.clone()),
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
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(PermissionError::NotInitialized)?;
        caller.require_auth();
        if *caller != stored_admin {
            return Err(PermissionError::Unauthorized);
        }
        Ok(stored_admin)
    }

    /// Computes an absolute expiry ledger as `current_sequence + ttl_ledgers`,
    /// returning [`PermissionError::InvalidExpiry`] instead of overflow-panicking
    /// when `ttl_ledgers` is large enough to push the sum past `u32::MAX`.
    fn grant_expiry_ledger(env: &Env, ttl_ledgers: u32) -> Result<u32, PermissionError> {
        compute_expiry_ledger(env.ledger().sequence(), ttl_ledgers)
    }

    fn add_to_address_index(env: &Env, key: &DataKey, address: &Address) {
        let mut addresses: Vec<Address> = env
            .storage()
            .persistent()
            .get(key)
            .unwrap_or_else(|| Vec::new(env));
        if !addresses.contains(address) {
            addresses.push_back(address.clone());
            env.storage().persistent().set(key, &addresses);
        }
    }

    fn remove_from_address_index(env: &Env, key: &DataKey, address: &Address) {
        let mut addresses: Vec<Address> = env
            .storage()
            .persistent()
            .get(key)
            .unwrap_or_else(|| Vec::new(env));
        if let Some(index) = addresses.first_index_of(address) {
            addresses.remove(index);
            env.storage().persistent().set(key, &addresses);
        }
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

    /// Validates a function scope (issue #369):
    /// - `allowed_function_symbols` must be non-empty, since an empty list is
    ///   indistinguishable from "no scope" to a reader and would leave the
    ///   record fail-closed with no way to ever spend.
    /// - Must not exceed `MAX_FUNCTIONS_PER_PERMISSION` entries.
    /// - Must not contain duplicate symbols.
    fn validate_scope_config(
        env: &Env,
        scope: &ScopedPermissionConfig,
    ) -> Result<(), PermissionError> {
        let functions = &scope.allowed_function_symbols;
        if functions.is_empty() || functions.len() > MAX_FUNCTIONS_PER_PERMISSION {
            return Err(PermissionError::InvalidParam);
        }
        let mut seen: Vec<Symbol> = Vec::new(env);
        for f in functions.iter() {
            if seen.contains(&f) {
                return Err(PermissionError::InvalidParam);
            }
            seen.push_back(f);
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
                        (symbol_short!("perm"), symbol_short!("revoked"), child_delegate.clone()),
                        PermissionRevokedEvent {
                            owner: delegate.clone(),
                            delegate: child_delegate.clone(),
                        },
                    );
                }
                Self::remove_from_address_index(
                    env,
                    &DataKey::DelegatePermissions(child_delegate.clone()),
                    delegate,
                );
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

    /// Validates the child permission's remaining limit, and then walks the parent
    /// chain validating that each ancestor also has sufficient remaining allowance.
    fn validate_chain(
        env: &Env,
        record: &PermissionRecord,
        amount: i128,
    ) -> Result<(), PermissionError> {
        let remaining = record.limit_total - record.spent;
        if amount > remaining {
            return Err(PermissionError::ExceedsTotalLimit);
        }

        let mut next_parent = match (record.parent_owner.clone(), record.parent_delegate.clone()) {
            (Some(p_owner), Some(p_delegate)) => Some((p_owner, p_delegate)),
            _ => None,
        };

        while let Some((p_owner, p_delegate)) = next_parent {
            let parent_key = DataKey::Permission(p_owner, p_delegate);
            let parent_record: PermissionRecord = env
                .storage()
                .persistent()
                .get(&parent_key)
                .ok_or(PermissionError::ParentNotFound)?;

            let parent_remaining = parent_record.limit_total - parent_record.spent;
            if amount > parent_remaining {
                return Err(PermissionError::ExceedsParentLimit);
            }

            next_parent = match (
                parent_record.parent_owner.clone(),
                parent_record.parent_delegate.clone(),
            ) {
                (Some(pp_owner), Some(pp_delegate)) => Some((pp_owner, pp_delegate)),
                _ => None,
            };
        }

        Ok(())
    }

    pub fn can_spend(
        env: Env,
        owner: Address,
        delegate: Address,
        amount: i128,
        merchant: Address,
    ) -> Result<(), PermissionError> {
        // No invocation is stated here, so a scoped grant fails closed in
        // `check_function_scope` rather than slipping through unchecked
        // (issue #369).
        Self::can_spend_scoped(env, owner, delegate, amount, merchant, None, None)
    }

    /// [`Self::can_spend`] extended with the invocation being authorized
    /// (issue #369).
    ///
    /// When the `(owner, delegate)` record carries a [`ScopedPermissionConfig`],
    /// `target_contract` and `invoked_function` must both be supplied and
    /// must both be authorized; otherwise the spend is rejected with
    /// [`PermissionError::UnauthorizedFunction`]. For an unscoped grant the
    /// extra arguments are ignored and the result is identical to
    /// [`Self::can_spend`].
    ///
    /// Scoping is evaluated *before* the limit checks so an unauthorized
    /// entrypoint reports the authorization failure rather than an incidental
    /// amount/merchant error, and so an unauthorized entrypoint is rejected
    /// even when it would otherwise have failed for another reason.
    // Reason: Soroban ABI entry point — signature is part of the published
    // on-chain ABI and cannot be restructured without a breaking change.
    #[allow(clippy::too_many_arguments)]
    pub fn can_spend_scoped(
        env: Env,
        owner: Address,
        delegate: Address,
        amount: i128,
        merchant: Address,
        target_contract: Option<Address>,
        invoked_function: Option<Symbol>,
    ) -> Result<(), PermissionError> {
        let key = DataKey::Permission(owner.clone(), delegate.clone());
        let record: PermissionRecord = match env.storage().persistent().get(&key) {
            Some(r) => r,
            None => return Err(PermissionError::PermissionNotFound),
        };

        // Issue #369: enforce the function scope (and every ancestor's scope)
        // before any amount is committed.
        Self::check_function_scope(
            &env,
            &owner,
            &delegate,
            &record,
            target_contract.as_ref(),
            &invoked_function,
        )?;

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

        Self::check_active_window(&env, &record)?;

        if amount > record.limit_per_tx {
            return Err(PermissionError::ExceedsPerTxLimit);
        }

        Self::validate_chain(&env, &record, amount)?;

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

        Self::check_merchant_allowlist(&env, &owner, &delegate, &merchant)?;

        // Issue #315: reject spends outside the configured business-hour /
        // day-of-week window, if one is set for this delegation.
        Self::check_time_window(&env, &owner, &delegate)?;

        Ok(())
    }

    /// Enforces a permission's calendar activation window (issue: calendar
    /// bounded spending grants). Rejects spends before `not_before_ledger`
    /// with `GrantNotYetActive` and after `not_after_ledger` with `Expired`.
    /// A `not_before_ledger`/`not_after_ledger` of `0` means "unbounded".
    fn check_active_window(
        env: &Env,
        record: &PermissionRecord,
    ) -> Result<(), PermissionError> {
        let current = env.ledger().sequence();
        if record.not_before_ledger != 0 && current < record.not_before_ledger {
            return Err(PermissionError::GrantNotYetActive);
        }
        if record.not_after_ledger != 0 && current > record.not_after_ledger {
            return Err(PermissionError::Expired);
        }
        Ok(())
    }

    /// Enforces the function scope of `(owner, delegate)` and of every
    /// ancestor it was granted under (issue #369).
    ///
    /// The chain walk is what closes lateral privilege escalation: a
    /// `grant_child` sub-delegation inherits its parent's scope, so a delegate
    /// holding `only escrow.fund` cannot hand a downstream agent a broader
    /// (or absent) scope by leaving the child unscoped. A missing ancestor
    /// ends the walk — `validate_chain` already reports a genuinely missing
    /// parent separately, and an unverifiable ancestor must not widen the
    /// effective scope.
    ///
    /// Every scope in the chain must authorize the same invocation, so the
    /// effective scope is the intersection of the lineage rather than the
    /// narrowest-looking single link.
    #[allow(clippy::too_many_arguments)]
    fn check_function_scope(
        env: &Env,
        owner: &Address,
        delegate: &Address,
        record: &PermissionRecord,
        target_contract: Option<&Address>,
        invoked_function: &Option<Symbol>,
    ) -> Result<(), PermissionError> {
        Self::enforce_single_scope(
            &Self::load_scope(env, owner, delegate),
            target_contract,
            invoked_function,
        )?;

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

            let parent_key = DataKey::Permission(p_owner.clone(), p_delegate.clone());
            let parent_record: PermissionRecord = match env.storage().persistent().get(&parent_key)
            {
                Some(r) => r,
                None => break,
            };

            Self::enforce_single_scope(
                &Self::load_scope(env, &p_owner, &p_delegate),
                target_contract,
                invoked_function,
            )?;

            next_parent = match (
                parent_record.parent_owner.clone(),
                parent_record.parent_delegate.clone(),
            ) {
                (Some(p_owner), Some(p_delegate)) => Some((p_owner, p_delegate)),
                _ => None,
            };
        }

        Ok(())
    }

    /// Applies a single pair's scope to one invocation. An absent scope is
    /// vacuously satisfied; a present one requires the invocation to name the
    /// scoped contract and one of its allowed function symbols.
    fn enforce_single_scope(
        scope: &Option<ScopedPermissionConfig>,
        target_contract: Option<&Address>,
        invoked_function: &Option<Symbol>,
    ) -> Result<(), PermissionError> {
        let scope = match scope {
            None => return Ok(()),
            Some(scope) => scope,
        };

        // Fail closed: a scoped grant is only spendable through an entrypoint
        // that names the contract and the function being invoked.
        let target = target_contract.ok_or(PermissionError::UnauthorizedFunction)?;
        let invoked = invoked_function
            .clone()
            .ok_or(PermissionError::UnauthorizedFunction)?;

        if *target != scope.target_contract {
            return Err(PermissionError::UnauthorizedFunction);
        }
        if !scope.allowed_function_symbols.contains(&invoked) {
            return Err(PermissionError::UnauthorizedFunction);
        }
        Ok(())
    }

    /// Rejects a spend to `merchant` when the `(owner, delegate)` pair has an
    /// enabled seller allowlist that does not contain it (issue #296). Runs
    /// from `can_spend`, so direct and relayed spends are both covered.
    fn check_merchant_allowlist(
        env: &Env,
        owner: &Address,
        delegate: &Address,
        merchant: &Address,
    ) -> Result<(), PermissionError> {
        if let Some(allowlist) = env
            .storage()
            .persistent()
            .get::<DataKey, MerchantAllowlist>(&DataKey::MerchantAllowlist(
                owner.clone(),
                delegate.clone(),
            ))
        {
            if allowlist.is_enabled && !allowlist.merchants.contains(merchant) {
                return Err(PermissionError::MerchantNotAllowed);
            }
        }
        Ok(())
    }

    /// Sets the seller-specific allowlist for a sensitive `(owner, delegate)`
    /// delegation (issue #296). Must be authorized by the owner, and a
    /// permission must already exist for the pair.
    ///
    /// While `is_enabled` is true, spends to any merchant not in `merchants`
    /// fail with [`PermissionError::MerchantNotAllowed`]. `merchants` must
    /// contain no duplicates and at most `MAX_MERCHANTS_PER_PERMISSION`
    /// entries, otherwise [`PermissionError::InvalidParam`] is returned.
    pub fn set_merchant_allowlist(
        env: Env,
        owner: Address,
        delegate: Address,
        is_enabled: bool,
        merchants: Vec<Address>,
    ) -> Result<(), PermissionError> {
        owner.require_auth();

        if !env
            .storage()
            .persistent()
            .has(&DataKey::Permission(owner.clone(), delegate.clone()))
        {
            return Err(PermissionError::PermissionNotFound);
        }

        Self::validate_merchant_list(&env, &merchants)?;

        let merchant_count = merchants.len();
        env.storage().persistent().set(
            &DataKey::MerchantAllowlist(owner.clone(), delegate.clone()),
            &MerchantAllowlist {
                is_enabled,
                merchants,
            },
        );

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("allowlst"), delegate.clone()),
            MerchantAllowlistUpdatedEvent {
                owner: owner.clone(),
                delegate: delegate.clone(),
                is_enabled,
                merchant_count,
            },
        );

        Self::append_audit_log(
            &env,
            &owner,
            &delegate,
            owner.clone(),
            symbol_short!("allowlst"),
        );

        Ok(())
    }

    /// Returns the seller allowlist configured for `(owner, delegate)`, if any.
    pub fn get_merchant_allowlist(
        env: Env,
        owner: Address,
        delegate: Address,
    ) -> Option<MerchantAllowlist> {
        env.storage()
            .persistent()
            .get(&DataKey::MerchantAllowlist(owner, delegate))
    }

    /// Configures (or replaces) the business-hour / day-of-week restriction
    /// for a `(owner, delegate)` delegation (issue #315). Owner-authorized;
    /// a permission must already exist for the pair.
    ///
    /// `start_hour_utc` and `end_hour_utc` must each be `< 24`.
    /// `allowed_days_bitmap` must only use bits 0-6 (Mon-Sun); any of bits
    /// 7-31 set is rejected as [`PermissionError::InvalidParam`].
    pub fn set_time_window_restriction(
        env: Env,
        owner: Address,
        delegate: Address,
        start_hour_utc: u32,
        end_hour_utc: u32,
        allowed_days_bitmap: u32,
    ) -> Result<(), PermissionError> {
        owner.require_auth();

        if !env
            .storage()
            .persistent()
            .has(&DataKey::Permission(owner.clone(), delegate.clone()))
        {
            return Err(PermissionError::PermissionNotFound);
        }

        if start_hour_utc >= 24 || end_hour_utc >= 24 || (allowed_days_bitmap & !0x7F) != 0 {
            return Err(PermissionError::InvalidParam);
        }

        env.storage().persistent().set(
            &DataKey::TimeWindowRestriction(owner.clone(), delegate.clone()),
            &TimeWindowRestriction {
                start_hour_utc,
                end_hour_utc,
                allowed_days_bitmap,
            },
        );

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("timewin")),
            TimeWindowRestrictionSetEvent {
                owner: owner.clone(),
                delegate: delegate.clone(),
                start_hour_utc,
                end_hour_utc,
                allowed_days_bitmap,
            },
        );

        Self::append_audit_log(
            &env,
            &owner,
            &delegate,
            owner.clone(),
            symbol_short!("timewin"),
        );

        Ok(())
    }

    /// Sets, replaces, or clears the function scope of an existing delegation
    /// (issue #369).
    ///
    /// Owner-authorized, mirroring [`Self::set_merchant_allowlist`]: the scope
    /// is a narrowing of the delegate's authority, so only the owner may set
    /// it. Passing `None` clears the scope and returns the delegation to
    /// unscoped behaviour, which is how an owner widens authority again after
    /// a period of tight scoping — a deliberate, observable act that emits
    /// [`PermissionScopeUpdatedEvent`] with a zero `function_count`.
    ///
    /// # Errors
    /// - [`PermissionError::PermissionNotFound`] if no permission exists for
    ///   the pair.
    /// - [`PermissionError::InvalidParam`] if `scope` is malformed (see
    ///   `MAX_FUNCTIONS_PER_PERMISSION`).
    pub fn set_permission_scope(
        env: Env,
        owner: Address,
        delegate: Address,
        scope: Option<ScopedPermissionConfig>,
    ) -> Result<(), PermissionError> {
        owner.require_auth();

        let key = DataKey::Permission(owner.clone(), delegate.clone());
        if !env.storage().persistent().has(&key) {
            return Err(PermissionError::PermissionNotFound);
        }

        if let Some(ref s) = scope {
            Self::validate_scope_config(&env, s)?;
        }

        Self::store_scope(&env, &owner, &delegate, scope.clone());

        let (target_contract, function_count) = match &scope {
            Some(s) => (
                Some(s.target_contract.clone()),
                s.allowed_function_symbols.len(),
            ),
            // A cleared scope names no target contract.
            None => (None, 0),
        };

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("scope")),
            PermissionScopeUpdatedEvent {
                owner: owner.clone(),
                delegate: delegate.clone(),
                target_contract,
                function_count,
            },
        );

        Self::append_audit_log(
            &env,
            &owner,
            &delegate,
            owner.clone(),
            symbol_short!("scope"),
        );

        Ok(())
    }

    /// Removes any business-hour restriction configured for `(owner,
    /// delegate)` (issue #315). A no-op (not an error) when none was set.
    pub fn clear_time_window_restriction(
        env: Env,
        owner: Address,
        delegate: Address,
    ) -> Result<(), PermissionError> {
        owner.require_auth();
        env.storage()
            .persistent()
            .remove(&DataKey::TimeWindowRestriction(owner, delegate));
        Ok(())
    }

    /// Returns the business-hour restriction configured for `(owner,
    /// delegate)`, if any (issue #315).
    pub fn get_time_window_restriction(
        env: Env,
        owner: Address,
        delegate: Address,
    ) -> Option<TimeWindowRestriction> {
        env.storage()
            .persistent()
            .get(&DataKey::TimeWindowRestriction(owner, delegate))
    }

    /// Rejects a spend made outside the delegation's configured business-hour
    /// window (issue #315). No-op when no restriction is configured.
    ///
    /// The current UTC hour-of-day and Mon-Sun weekday are derived directly
    /// from `env.ledger().timestamp()` (seconds since the Unix epoch, which
    /// was a Thursday): `weekday = (days_since_epoch + 3) % 7` maps
    /// Monday -> 0 ... Sunday -> 6, and `hour = (timestamp % 86400) / 3600`.
    fn check_time_window(
        env: &Env,
        owner: &Address,
        delegate: &Address,
    ) -> Result<(), PermissionError> {
        let restriction: TimeWindowRestriction = match env
            .storage()
            .persistent()
            .get(&DataKey::TimeWindowRestriction(owner.clone(), delegate.clone()))
        {
            Some(r) => r,
            None => return Ok(()),
        };

        let timestamp = env.ledger().timestamp();
        let days_since_epoch = timestamp / 86_400;
        let seconds_of_day = timestamp % 86_400;
        let weekday = ((days_since_epoch + 3) % 7) as u32;
        let hour = (seconds_of_day / 3_600) as u32;

        let day_bit = 1u32 << weekday;
        if restriction.allowed_days_bitmap & day_bit == 0 {
            return Err(PermissionError::OutsideAuthorizedWindow);
        }

        if restriction.start_hour_utc != restriction.end_hour_utc {
            let in_window = if restriction.start_hour_utc < restriction.end_hour_utc {
                hour >= restriction.start_hour_utc && hour < restriction.end_hour_utc
            } else {
                // Overnight window wrapping past midnight (e.g. 22 -> 6).
                hour >= restriction.start_hour_utc || hour < restriction.end_hour_utc
            };
            if !in_window {
                return Err(PermissionError::OutsideAuthorizedWindow);
            }
        }

        Ok(())
    }

    /// Returns the [`ScopedPermissionConfig`] attached to `(owner, delegate)`,
    /// or `None` when the delegation is unscoped or unknown (issue #369).
    pub fn get_permission_scope(
        env: Env,
        owner: Address,
        delegate: Address,
    ) -> Option<ScopedPermissionConfig> {
        Self::load_scope(&env, &owner, &delegate)
    }

    pub fn execute_spend(
        env: Env,
        owner: Address,
        delegate: Address,
        amount: i128,
        merchant: Address,
    ) -> Result<(), PermissionError> {
        delegate.require_auth();

        Self::execute_spend_impl(env, owner, delegate, amount, merchant, None, None)
    }

    /// Executes a spend on behalf of a permission restricted to specific
    /// contract entrypoints (issue #369).
    ///
    /// Identical to [`Self::execute_spend`] except that it states *what* is
    /// being invoked. For a grant carrying a [`ScopedPermissionConfig`] this
    /// is the only entrypoint that can succeed: `target_contract` must equal
    /// the scoped contract and `invoked_function` must be one of
    /// `allowed_function_symbols`, otherwise the spend is rejected with
    /// [`PermissionError::UnauthorizedFunction`] and no allowance moves. For
    /// an unscoped grant the two extra arguments are simply ignored.
    // Reason: Soroban ABI entry point — signature is part of the published
    // on-chain ABI and cannot be restructured without a breaking change.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_spend_scoped(
        env: Env,
        owner: Address,
        delegate: Address,
        amount: i128,
        merchant: Address,
        target_contract: Option<Address>,
        invoked_function: Option<Symbol>,
    ) -> Result<(), PermissionError> {
        delegate.require_auth();

        Self::execute_spend_impl(
            env,
            owner,
            delegate,
            amount,
            merchant,
            target_contract,
            invoked_function,
        )
    }

    /// Shared implementation for [`Self::execute_spend`] and
    /// [`Self::execute_spend_scoped`]. `target_contract` / `invoked_function`
    /// are forwarded to `can_spend_scoped` so both entrypoints run the exact
    /// same authorization, velocity and accounting sequence.
    #[allow(clippy::too_many_arguments)]
    fn execute_spend_impl(
        env: Env,
        owner: Address,
        delegate: Address,
        amount: i128,
        merchant: Address,
        target_contract: Option<Address>,
        invoked_function: Option<Symbol>,
    ) -> Result<(), PermissionError> {
        // Propagate the precise reason (expired, over-limit, wrong merchant,
        // unauthorized function, …) to the caller instead of panicking with
        // an opaque string.
        Self::can_spend_scoped(
            env.clone(),
            owner.clone(),
            delegate.clone(),
            amount,
            merchant.clone(),
            target_contract,
            invoked_function,
        )?;

        // #54: Velocity check — reject if min_spend_interval has not yet elapsed
        // since the last recorded spend ledger for this (owner, delegate) pair.
        Self::check_velocity(&env, &owner, &delegate)?;

        // #368: Rolling-window cap — reject if this spend would push the
        // pair's cumulative spend within the current window past the cap.
        Self::check_rolling_window(&env, &owner, &delegate, amount)?;

        let result = Self::apply_spend(&env, &owner, &delegate, amount)?;

        // Emit after successful spend only (issue #99).
        env.events().publish(
            (symbol_short!("perm"), symbol_short!("spent"), delegate.clone()),
            PermissionSpendEvent {
                owner,
                delegate,
                merchant,
                amount,
                remaining: result.remaining_allowance,
            },
        );

        Ok(())
    }

    /// Shared helper that applies a validated spend to the child permission
    /// record and then walks the parent chain, decrementing each ancestor's
    /// allowance by the same `amount` (issue #55 / #332).
    ///
    /// Callers must run all policy checks first. This helper independently
    /// rechecks the current allowance before mutating storage.
    ///
    /// Rechecks and persists the spend atomically against the latest stored record.
    fn apply_spend(
        env: &Env,
        owner: &Address,
        delegate: &Address,
        amount: i128,
    ) -> Result<SpendExecutionResult, PermissionError> {
        let key = DataKey::Permission(owner.clone(), delegate.clone());
        let mut record: PermissionRecord = env.storage().persistent().get(&key).unwrap();

        if amount <= 0 {
            return Err(PermissionError::InvalidParam);
        }
        let new_spent = record
            .spent
            .checked_add(amount)
            .ok_or(PermissionError::ExceedsTotalLimit)?;
        if new_spent > record.limit_total {
            return Err(PermissionError::ExceedsTotalLimit);
        }
        record.spent = new_spent;
        let remaining = record.limit_total - record.spent;
        let new_spend_ledger = env.ledger().sequence();

        // Capture the parent link before writing the updated record so we can
        // walk the chain without holding an immutable borrow.
        let mut next_parent = match (record.parent_owner.clone(), record.parent_delegate.clone()) {
            (Some(p_owner), Some(p_delegate)) => Some((p_owner, p_delegate)),
            _ => None,
        };
        env.storage().persistent().set(&key, &record);

        Self::record_spend_stats(env, owner, delegate, amount);

        // Record the current ledger sequence and timestamp for velocity tracking.
        env.storage().persistent().set(
            &DataKey::LastSpendLedger(owner.clone(), delegate.clone()),
            &new_spend_ledger,
        );
        env.storage().persistent().set(
            &DataKey::LastSpendTimestamp(owner.clone(), delegate.clone()),
            &env.ledger().timestamp(),
        );

        // #368: Accrue this spend into the pair's rolling-window accumulator.
        Self::record_rolling_window_spend(env, owner, delegate, amount)?;

        // Walk the parent chain, deducting the same amount from each ancestor's
        // allowance so a child's spend is also reflected against the allowance
        // it was carved out of (issue #332). This path is now shared between
        // execute_spend and execute_spend_via_relayer (issue #55).
        while let Some((p_owner, p_delegate)) = next_parent {
            let parent_key = DataKey::Permission(p_owner, p_delegate);
            let mut parent_record: PermissionRecord =
                env.storage().persistent().get(&parent_key).unwrap();

            let parent_spent = parent_record
                .spent
                .checked_add(amount)
                .ok_or(PermissionError::ExceedsTotalLimit)?;
            if parent_spent > parent_record.limit_total {
                return Err(PermissionError::ExceedsTotalLimit);
            }
            parent_record.spent = parent_spent;
            next_parent = match (
                parent_record.parent_owner.clone(),
                parent_record.parent_delegate.clone(),
            ) {
                (Some(pp_owner), Some(pp_delegate)) => Some((pp_owner, pp_delegate)),
                _ => None,
            };
            env.storage().persistent().set(&parent_key, &parent_record);
        }

        Ok(SpendExecutionResult {
            remaining_allowance: remaining,
            new_spend_ledger,
        })
    }

    /// Rejects a spend when the configured velocity limit has not yet elapsed
    /// since the last recorded spend for this (owner, delegate) pair (issues
    /// #54, #290). Called from both `execute_spend` and
    /// `execute_spend_via_relayer` before the new spend is recorded, so direct
    /// and relayed spends share the same throttle.
    ///
    /// The primary check is against `env.ledger().sequence()`, which is
    /// deterministic and immune to validator clock drift: a spend is rejected
    /// while `current_ledger < last_spend_ledger + min_interval_ledgers`. When
    /// a wall-clock floor (`MinSpendIntervalSecs`) is also configured, the
    /// spend must additionally satisfy
    /// `now >= last_spend_timestamp + min_interval_secs`. The timestamp check
    /// can only tighten the limiter, never loosen it, so drift cannot be used
    /// to bypass the ledger interval. No-op when no interval is configured or
    /// no prior spend exists.
    fn check_velocity(
        env: &Env,
        owner: &Address,
        delegate: &Address,
    ) -> Result<(), PermissionError> {
        let min_interval_ledgers: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MinSpendInterval)
            .unwrap_or(0);
        if min_interval_ledgers > 0 {
            if let Some(last_ledger) = env
                .storage()
                .persistent()
                .get::<DataKey, u32>(&DataKey::LastSpendLedger(owner.clone(), delegate.clone()))
            {
                // Saturate so a last_ledger near u32::MAX keeps the pair
                // throttled instead of wrapping around and unlocking it.
                let next_allowed = last_ledger.saturating_add(min_interval_ledgers);
                if env.ledger().sequence() < next_allowed {
                    return Err(PermissionError::VelocityLimitExceeded);
                }
            }
        }

        let min_interval_secs: u64 = env
            .storage()
            .instance()
            .get(&DataKey::MinSpendIntervalSecs)
            .unwrap_or(0);
        if min_interval_secs > 0 {
            // Pairs whose last spend predates timestamp tracking have no
            // recorded timestamp; they are governed by the ledger check alone.
            if let Some(last_ts) =
                env.storage()
                    .persistent()
                    .get::<DataKey, u64>(&DataKey::LastSpendTimestamp(
                        owner.clone(),
                        delegate.clone(),
                    ))
            {
                let next_allowed = last_ts.saturating_add(min_interval_secs);
                if env.ledger().timestamp() < next_allowed {
                    return Err(PermissionError::VelocityLimitExceeded);
                }
            }
        }

        Ok(())
    }

    /// Load the instance-level rolling-window configuration, if any.
    fn rolling_window_config(env: &Env) -> Option<RollingWindowLimit> {
        env.storage()
            .instance()
            .get(&DataKey::RollingWindowConfig)
    }

    /// Load (and lazily roll over) the rolling-window state for a pair.
    ///
    /// A window that has fully elapsed is reset to start at the current ledger
    /// with an accumulator of zero, so the cap applies to a true sliding window
    /// rather than a fixed bucket (issue #368).
    fn load_rolling_window_state(
        env: &Env,
        owner: &Address,
        delegate: &Address,
    ) -> RollingWindowLimit {
        let config = Self::rolling_window_config(env).unwrap_or(RollingWindowLimit {
            window_ledgers: 0,
            max_spend_in_window: 0,
            current_window_spend: 0,
            window_start_ledger: 0,
        });
        let current_ledger = env.ledger().sequence();
        let mut state: RollingWindowLimit = env
            .storage()
            .persistent()
            .get(&DataKey::RollingWindowState(owner.clone(), delegate.clone()))
            .unwrap_or(RollingWindowLimit {
                window_ledgers: config.window_ledgers,
                max_spend_in_window: config.max_spend_in_window,
                current_window_spend: 0,
                window_start_ledger: current_ledger,
            });
        state.window_ledgers = config.window_ledgers;
        state.max_spend_in_window = config.max_spend_in_window;
        if state.window_ledgers > 0
            && current_ledger >= state.window_start_ledger.saturating_add(state.window_ledgers)
        {
            state.current_window_spend = 0;
            state.window_start_ledger = current_ledger;
        }
        state
    }

    /// Reject a spend that would push the pair's cumulative spend within the
    /// current rolling window past `max_spend_in_window` (issue #368).
    ///
    /// No-op when the cap is disabled (`max_spend_in_window == 0`) or the
    /// window length is zero. Called before any state mutation so a rejected
    /// spend leaves the accumulator untouched.
    fn check_rolling_window(
        env: &Env,
        owner: &Address,
        delegate: &Address,
        amount: i128,
    ) -> Result<(), PermissionError> {
        let config = match Self::rolling_window_config(env) {
            Some(config) if config.window_ledgers > 0 && config.max_spend_in_window > 0 => config,
            _ => return Ok(()),
        };
        if amount <= 0 {
            return Ok(());
        }
        let state = Self::load_rolling_window_state(env, owner, delegate);
        let projected = state
            .current_window_spend
            .checked_add(amount)
            .ok_or(PermissionError::VelocityLimitExceeded)?;
        if projected > config.max_spend_in_window {
            return Err(PermissionError::VelocityLimitExceeded);
        }
        Ok(())
    }

    /// Accrue a successful spend into the pair's rolling-window accumulator,
    /// rolling the window over first if it has elapsed (issue #368).
    fn record_rolling_window_spend(
        env: &Env,
        owner: &Address,
        delegate: &Address,
        amount: i128,
    ) -> Result<(), PermissionError> {
        if Self::rolling_window_config(env).is_none() {
            return Ok(());
        }
        let mut state = Self::load_rolling_window_state(env, owner, delegate);
        if state.window_ledgers == 0 {
            return Ok(());
        }
        state.current_window_spend = state
            .current_window_spend
            .checked_add(amount)
            .ok_or(PermissionError::VelocityLimitExceeded)?;
        env.storage().persistent().set(
            &DataKey::RollingWindowState(owner.clone(), delegate.clone()),
            &state,
        );
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
            (symbol_short!("perm"), symbol_short!("relaykey"), delegate.clone()),
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

    /// Returns the next nonce a channel-based relayed spend for this
    /// (owner, delegate, channel_id) triple must use (issue #367).
    /// Channels are independent lanes (0..=255), enabling parallel spends.
    pub fn get_channel_nonce(
        env: Env,
        owner: Address,
        delegate: Address,
        channel_id: u32,
    ) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::ChannelNonce(owner, delegate, channel_id))
            .unwrap_or(0)
    }

    /// Returns the current execution epoch for relayed spends.
    pub fn get_execution_epoch(env: Env, owner: Address, delegate: Address) -> EpochConfig {
        env.storage()
            .persistent()
            .get(&DataKey::ExecutionEpoch(owner, delegate))
            .unwrap_or(EpochConfig {
                current_epoch: 0,
                epoch_started_ledger: 0,
            })
    }

    /// Invalidates a stalled relayer nonce (and every nonce below it) for the
    /// `(owner, delegate)` pair so later signed spends are no longer blocked
    /// behind a dropped or censored submission (issue #297).
    ///
    /// Must be authorized by the owner. `nonce` must be at least the current
    /// expected nonce; the expected nonce is advanced to `nonce + 1`, so any
    /// outstanding signed message carrying a cancelled nonce is rejected with
    /// [`PermissionError::InvalidNonce`]. The delegation itself is untouched.
    ///
    /// # Errors
    /// - [`PermissionError::PermissionNotFound`] if no permission exists.
    /// - [`PermissionError::NonceAlreadyUsed`] if `nonce` was already consumed
    ///   or cancelled, or if advancing past it would overflow.
    pub fn cancel_nonce(
        env: Env,
        owner: Address,
        delegate: Address,
        nonce: u64,
    ) -> Result<(), PermissionError> {
        owner.require_auth();

        if !env
            .storage()
            .persistent()
            .has(&DataKey::Permission(owner.clone(), delegate.clone()))
        {
            return Err(PermissionError::PermissionNotFound);
        }

        let nonce_key = DataKey::RelayerNonce(owner.clone(), delegate.clone());
        let expected_nonce: u64 = env.storage().persistent().get(&nonce_key).unwrap_or(0);
        if nonce < expected_nonce {
            return Err(PermissionError::NonceAlreadyUsed);
        }
        let next_nonce = nonce
            .checked_add(1)
            .ok_or(PermissionError::NonceAlreadyUsed)?;

        env.storage().persistent().set(&nonce_key, &next_nonce);

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("nonce_cxl"), delegate.clone()),
            NonceCancelledEvent {
                owner: owner.clone(),
                delegate: delegate.clone(),
                cancelled_nonce: nonce,
                next_nonce,
            },
        );

        Self::append_audit_log(
            &env,
            &owner,
            &delegate,
            owner.clone(),
            symbol_short!("nonce_cxl"),
        );

        Ok(())
    }

    /// Bulk-invalidate all relayer nonces up to and including `up_to_nonce`
    /// for an `(owner, delegate)` pair whose agent private key is suspected
    /// of being compromised (issue #335).
    ///
    /// Must be authorized by the owner. The stored expected nonce is advanced
    /// to `up_to_nonce + 1`, so every signed message carrying any nonce
    /// ≤ `up_to_nonce` is permanently rejected with
    /// [`PermissionError::InvalidNonce`]. The delegation itself is untouched —
    /// the owner can continue granting fresh relayer nonces for new spends.
    ///
    /// This is the batch companion to [`Self::cancel_nonce`]: `cancel_nonce`
    /// targets a single stalled nonce, while `invalidate_nonce_range` is
    /// designed for the key-compromise recovery path where potentially many
    /// signed messages need to be voided in one call.
    ///
    /// # Errors
    /// - [`PermissionError::PermissionNotFound`] if no permission exists for
    ///   `(owner, delegate)`.
    /// - [`PermissionError::NonceAlreadyUsed`] if `up_to_nonce` is already
    ///   below the current expected nonce (all those nonces are already
    ///   consumed/cancelled), or if advancing past `up_to_nonce` would
    ///   overflow the nonce counter.
    pub fn invalidate_nonce_range(
        env: Env,
        owner: Address,
        delegate: Address,
        up_to_nonce: u64,
    ) -> Result<(), PermissionError> {
        owner.require_auth();

        if !env
            .storage()
            .persistent()
            .has(&DataKey::Permission(owner.clone(), delegate.clone()))
        {
            return Err(PermissionError::PermissionNotFound);
        }

        let nonce_key = DataKey::RelayerNonce(owner.clone(), delegate.clone());
        let current_nonce: u64 = env.storage().persistent().get(&nonce_key).unwrap_or(0);

        // Reject if up_to_nonce is already below what's expected — all nonces
        // in that range have already been consumed or cancelled.
        if up_to_nonce < current_nonce {
            return Err(PermissionError::NonceAlreadyUsed);
        }

        let next_nonce = up_to_nonce
            .checked_add(1)
            .ok_or(PermissionError::NonceAlreadyUsed)?;

        env.storage().persistent().set(&nonce_key, &next_nonce);

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("nonce_inv")),
            NonceBatchInvalidatedEvent {
                owner: owner.clone(),
                delegate: delegate.clone(),
                up_to_nonce,
                next_nonce,
            },
        );

        Self::append_audit_log(
            &env,
            &owner,
            &delegate,
            owner.clone(),
            symbol_short!("nonce_inv"),
        );

        Ok(())
    }

    /// Execute a spend on the delegate's behalf from a relayer, without
    /// requiring the delegate to submit (or pay fees for) the transaction
    /// themselves.
    ///
    /// The delegate authorizes the spend by signing a [`RelayedSpendMessage`]
    /// off-chain with the key registered via `set_relayer_key`; any relayer
    /// can then submit that message and signature here. The signature is
    /// verified with Soroban's ed25519 crypto primitive against the
    /// delegate's registered public key, the `nonce` and `epoch` must match
    /// the current execution context (preventing replay), and
    /// `expiration_ledger` must not yet have been reached.
    ///
    /// The signed payload names no contract entrypoint, so this path is only
    /// usable by unscoped delegations: for a permission carrying a
    /// [`ScopedPermissionConfig`] (issue #369) the shared validation rejects
    /// the relayed spend with [`PermissionError::UnauthorizedFunction`],
    /// because there is no way for the signature to attest *which* function
    /// the relayer is invoking on the delegate's behalf. Relayed agents on a
    /// scoped grant must therefore submit [`Self::execute_spend_scoped`]
    /// themselves. Scoping fails closed here rather than being skipped.
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
        epoch: u32,
        signature: BytesN<64>,
    ) -> Result<(), PermissionError> {
        relayer.require_auth();

        if env.ledger().sequence() >= expiration_ledger {
            return Err(PermissionError::SignatureExpired);
        }

        let epoch_config =
            Self::get_execution_epoch(env.clone(), owner.clone(), delegate.clone());
        if epoch != epoch_config.current_epoch {
            return Err(PermissionError::StaleEpoch);
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
            epoch,
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

        // #368: Rolling-window cap, shared with the direct spend path.
        Self::check_rolling_window(&env, &owner, &delegate, amount)?;

        // Advance the nonce before mutating spend state so a replay attempt
        // within the same ledger is rejected even if apply_spend panics.
        let next_nonce = nonce.checked_add(1).ok_or(PermissionError::InvalidNonce)?;
        env.storage().persistent().set(&nonce_key, &next_nonce);

        // apply_spend increments the child record, walks the full parent chain,
        // updates usage stats, and records the last spend ledger — identical to
        // the direct execute_spend path (issue #55).
        let result = Self::apply_spend(&env, &owner, &delegate, amount)?;

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("relayed"), delegate.clone()),
            PermissionSpendEvent {
                owner,
                delegate,
                merchant,
                amount,
                remaining: result.remaining_allowance,
            },
        );

        Ok(())
    }

    /// Execute a spend on the delegate's behalf from a relayer, using a
    /// specific channel nonce lane (issue #367).
    ///
    /// This is the multi-channel analogue of [`Self::execute_spend_via_relayer`].
    /// The delegate authorizes the spend by signing a
    /// [`ChannelRelayedSpendMessage`] off-chain with the key registered via
    /// `set_relayer_key`; any relayer can then submit that message and
    /// signature here. The signature is verified against the delegate's
    /// registered public key. The `channel_id` (0..=255) selects an independent
    /// nonce lane, so concurrent spends on different channels never block each
    /// other. The `nonce` must match the current expected nonce for that
    /// channel, and `expiration_ledger` must not yet have been reached.
    ///
    /// The signed payload names no contract entrypoint, so this path is only
    /// usable by unscoped delegations: for a permission carrying a
    /// [`ScopedPermissionConfig`] (issue #369) the shared validation rejects
    /// the relayed spend with [`PermissionError::UnauthorizedFunction`],
    /// because there is no way for the signature to attest *which* function
    /// the relayer is invoking on the delegate's behalf. Relayed agents on a
    /// scoped grant must therefore submit [`Self::execute_spend_scoped`]
    /// themselves. Scoping fails closed here rather than being skipped.
    // Reason: Soroban ABI entry point — signature is part of the published
    // on-chain ABI and cannot be restructured without a breaking change.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_spend_via_channel(
        env: Env,
        relayer: Address,
        owner: Address,
        delegate: Address,
        amount: i128,
        merchant: Address,
        channel_sig: ChannelSpendSignature,
        expiration_ledger: u32,
        epoch: u32,
    ) -> Result<(), PermissionError> {
        relayer.require_auth();

        if env.ledger().sequence() >= expiration_ledger {
            return Err(PermissionError::SignatureExpired);
        }

        let epoch_config =
            Self::get_execution_epoch(env.clone(), owner.clone(), delegate.clone());
        if epoch != epoch_config.current_epoch {
            return Err(PermissionError::StaleEpoch);
        }

        // Validate channel_id range (0..=255)
        if channel_sig.channel_id > 255 {
            return Err(PermissionError::InvalidParam);
        }

        let nonce_key = DataKey::ChannelNonce(owner.clone(), delegate.clone(), channel_sig.channel_id);
        let expected_nonce: u64 = env.storage().persistent().get(&nonce_key).unwrap_or(0);
        if channel_sig.nonce != expected_nonce {
            return Err(PermissionError::InvalidNonce);
        }

        let public_key: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::RelayerKey(delegate.clone()))
            .ok_or(PermissionError::RelayerKeyNotSet)?;

        let message = ChannelRelayedSpendMessage {
            owner: owner.clone(),
            delegate: delegate.clone(),
            merchant: merchant.clone(),
            amount,
            channel_id: channel_sig.channel_id,
            nonce: channel_sig.nonce,
            expiration_ledger,
            epoch,
        };
        let message_bytes = message.to_xdr(&env);
        env.crypto()
            .ed25519_verify(&public_key, &message_bytes, &channel_sig.signature);

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
        Self::check_velocity(&env, &owner, &delegate)?;

        // #368: Rolling-window cap, shared with the direct spend path.
        Self::check_rolling_window(&env, &owner, &delegate, amount)?;

        // Advance the nonce before mutating spend state so a replay attempt
        // within the same ledger is rejected even if apply_spend panics.
        let next_nonce = channel_sig.nonce.checked_add(1).ok_or(PermissionError::InvalidNonce)?;
        env.storage().persistent().set(&nonce_key, &next_nonce);

        // apply_spend increments the child record, walks the full parent chain,
        // updates usage stats, and records the last spend ledger — identical to
        // the direct execute_spend path (issue #55).
        let result = Self::apply_spend(&env, &owner, &delegate, amount)?;

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("chn_spnd"), delegate.clone()),
            PermissionSpendEvent {
                owner,
                delegate,
                merchant,
                amount,
                remaining: result.remaining_allowance,
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

        // #368: Rolling-window cap applies to multi-owner spends too.
        Self::check_rolling_window(&env, &primary_owner, &delegate, amount)?;

        let key = DataKey::MultiPermission(primary_owner.clone(), delegate.clone());
        let mut record: MultiOwnerPermission = env.storage().persistent().get(&key).unwrap();

        record.spent = record
            .spent
            .checked_add(amount)
            .filter(|spent| *spent <= record.limit_total)
            .ok_or(PermissionError::ExceedsAllowance)?;
        env.storage().persistent().set(&key, &record);

        Self::record_rolling_window_spend(&env, &primary_owner, &delegate, amount)?;

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
                    PermissionError::OutsideAuthorizedWindow => {
                        Symbol::new(&env, "outside_win")
                    }
                    PermissionError::UnauthorizedFunction => Symbol::new(&env, "bad_function"),
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

    /// Returns stored permissions granted to `delegate` by every indexed owner.
    pub fn get_permissions_by_delegate(env: Env, delegate: Address) -> Vec<PermissionRecord> {
        let owners: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::DelegatePermissions(delegate.clone()))
            .unwrap_or_else(|| Vec::new(&env));

        let mut records = Vec::new(&env);
        for owner in owners.iter() {
            if let Some(record) = env
                .storage()
                .persistent()
                .get(&DataKey::Permission(owner, delegate.clone()))
            {
                records.push_back(record);
            }
        }
        records
    }

    /// Resets the owner's SAC allowance for `delegate` on `token` to zero once
    /// the delegation has expired, so a lapsed delegation cannot keep pulling
    /// funds through a stale `approve`.
    ///
    /// Open to any caller at zero cost: there is no caller parameter and no
    /// reward, and the call only ever lowers an allowance. Expiry uses the
    /// effective (lineage-capped) expiry, so a child is sweepable once any
    /// ancestor has lapsed. A zero allowance is a no-op with no event.
    ///
    /// Note: the SAC's `approve` itself calls `owner.require_auth()`, so the
    /// submitting transaction must carry an owner auth entry for this
    /// invocation (e.g. a pre-signed sweep handed to a keeper).
    ///
    /// # Errors
    /// - [`PermissionError::PermissionNotFound`] if no delegation exists.
    /// - [`PermissionError::DelegationNotExpired`] if it is still live.
    pub fn sweep_expired_allowance(
        env: Env,
        owner: Address,
        delegate: Address,
        token: Address,
    ) -> Result<(), PermissionError> {
        let record: PermissionRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Permission(owner.clone(), delegate.clone()))
            .ok_or(PermissionError::PermissionNotFound)?;

        let current_ledger = env.ledger().sequence();
        if current_ledger < Self::effective_expiry(&env, &record) {
            return Err(PermissionError::DelegationNotExpired);
        }

        let token_client = soroban_sdk::token::Client::new(&env, &token);
        let reclaimed_amount = token_client.allowance(&owner, &delegate);
        if reclaimed_amount == 0 {
            return Ok(());
        }
        token_client.approve(&owner, &delegate, &0, &current_ledger);

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("allow_rcl")),
            AllowanceReclaimedEvent {
                owner,
                delegate,
                token,
                reclaimed_amount,
            },
        );
        Ok(())
    }



    pub fn get_remaining_allowance(
        env: Env,
        owner: Address,
        delegate: Address,
    ) -> Result<i128, PermissionError> {
        let key = DataKey::Permission(owner, delegate);
        let record: PermissionRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(PermissionError::PermissionNotFound)?;
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
        let execution_time =
            env.ledger().timestamp() + Self::get_decrease_timelock_secs(env.clone());
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

    fn bump_execution_epoch(
        env: &Env,
        owner: &Address,
        delegate: &Address,
    ) -> Result<(), PermissionError> {
        let current = Self::get_execution_epoch(env.clone(), owner.clone(), delegate.clone());
        let next_epoch = current
            .current_epoch
            .checked_add(1)
            .ok_or(PermissionError::InvalidParam)?;
        let updated = EpochConfig {
            current_epoch: next_epoch,
            epoch_started_ledger: env.ledger().sequence(),
        };
        env.storage().persistent().set(
            &DataKey::ExecutionEpoch(owner.clone(), delegate.clone()),
            &updated,
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

        Self::bump_execution_epoch(&env, &owner, &delegate)?;
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

        Self::bump_execution_epoch(&env, &owner, &delegate)?;
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
        Self::remove_from_address_index(
            &env,
            &DataKey::DelegatePermissions(delegate.clone()),
            &owner,
        );
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

        if pairs.is_empty() || pairs.len() > MAX_SWEEP_BATCH_SIZE {
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

    /// Prunes a permission record that has been expired for more than
    /// `PRUNE_EXPIRATION_THRESHOLD_LEDGERS` (100,000 ledgers) from persistent storage,
    /// reclaiming contract rent and eliminating blockchain state bloat (issue #374).
    ///
    /// Callable by any keeper bot with authentication.
    /// Returns `Ok(true)` if the permission was pruned, or `Ok(false)` if the
    /// permission is active or has not yet reached the expiration threshold.
    /// Returns `Err(PermissionError::PermissionNotFound)` if the permission does not exist.
    pub fn prune_expired_permission(
        env: Env,
        owner: Address,
        delegate: Address,
        keeper: Address,
    ) -> Result<bool, PermissionError> {
        keeper.require_auth();

        let key = DataKey::Permission(owner.clone(), delegate.clone());
        let record: PermissionRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(PermissionError::PermissionNotFound)?;

        let current_ledger = env.ledger().sequence();
        if current_ledger.saturating_sub(record.expires_at_ledger) <= PRUNE_EXPIRATION_THRESHOLD_LEDGERS {
            return Ok(false);
        }

        // Delete primary permission record from persistent storage
        env.storage().persistent().remove(&key);

        // Clean up secondary persistent storage entries associated with this permission
        env.storage()
            .persistent()
            .remove(&DataKey::PendingDecrement(owner.clone(), delegate.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::PauseMetadata(owner.clone(), delegate.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::Metadata(owner.clone(), delegate.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::MerchantAllowlist(owner.clone(), delegate.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::RelayerNonce(owner.clone(), delegate.clone()));
        // Clean up all channel nonce lanes (0..=255)
        for channel_id in 0u32..=255 {
            env.storage()
                .persistent()
                .remove(&DataKey::ChannelNonce(owner.clone(), delegate.clone(), channel_id));
        }
        env.storage()
            .persistent()
            .remove(&DataKey::LastSpendLedger(owner.clone(), delegate.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::LastSpendTimestamp(owner.clone(), delegate.clone()));

        // Remove delegate from owner's user permissions index
        let user_perms_key = DataKey::UserPermissions(owner.clone());
        let mut delegates: Vec<Address> = env
            .storage()
            .persistent()
            .get(&user_perms_key)
            .unwrap_or(Vec::new(&env));
        if let Some(index) = delegates.first_index_of(&delegate) {
            delegates.remove(index);
            if delegates.is_empty() {
                env.storage().persistent().remove(&user_perms_key);
            } else {
                env.storage().persistent().set(&user_perms_key, &delegates);
            }
        }

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("pruned")),
            PermissionPrunedEvent {
                owner,
                delegate,
                keeper,
                pruned_at_ledger: current_ledger,
            },
        );

        Ok(true)
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

        if pairs.is_empty() || pairs.len() > MAX_SWEEP_BATCH_SIZE {
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
                    Self::remove_from_address_index(
                        &env,
                        &DataKey::DelegatePermissions(delegate.clone()),
                        &owner,
                    );
                    env.storage()
                        .persistent()
                        .remove(&DataKey::PendingDecrement(owner.clone(), delegate.clone()));

                    env.events().publish(
                        (symbol_short!("perm"), symbol_short!("autorevk"), delegate.clone()),
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

    /// Configure the minimum number of ledgers that must elapse between successive
    /// spends for any delegation pair (#324). Admin-only.
    ///
    /// This is the authoritative velocity check and is evaluated against
    /// `env.ledger().sequence()` (#290). Set `interval` to `0` to disable
    /// ledger-based velocity limiting.
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
            (symbol_short!("perm"), symbol_short!("velset"), admin.clone()),
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

    /// Configure an optional wall-clock floor, in seconds, between successive
    /// spends for any delegation pair (#290). Admin-only.
    ///
    /// Evaluated against `env.ledger().timestamp()` in addition to the ledger
    /// sequence interval set by `set_velocity_limit`; a spend must satisfy
    /// both. Set `secs` to `0` to disable the timestamp floor.
    pub fn set_velocity_limit_secs(
        env: Env,
        admin: Address,
        secs: u64,
    ) -> Result<(), PermissionError> {
        Self::require_admin(&env, &admin)?;

        if secs > MAX_VELOCITY_INTERVAL_SECS {
            return Err(PermissionError::InvalidParam);
        }

        let previous: Option<u64> = env.storage().instance().get(&DataKey::MinSpendIntervalSecs);

        env.storage()
            .instance()
            .set(&DataKey::MinSpendIntervalSecs, &secs);

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("velsecset"), admin.clone()),
            VelocityLimitSecsSetEvent {
                previous,
                current: secs,
                set_by: admin,
            },
        );

        Ok(())
    }

    /// Returns the configured wall-clock velocity floor in seconds, or `0`
    /// when none has been set.
    pub fn get_velocity_limit_secs(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::MinSpendIntervalSecs)
            .unwrap_or(0)
    }

    /// Returns the velocity state for an (owner, delegate) pair (#290).
    ///
    /// `last_spend_ledger` / `last_spend_timestamp` are `0` when the pair has
    /// never spent (or spent before timestamp tracking was introduced).
    pub fn get_velocity_state(env: Env, owner: Address, delegate: Address) -> VelocityLimit {
        VelocityLimit {
            min_interval_ledgers: Self::get_velocity_limit(env.clone()),
            last_spend_ledger: env
                .storage()
                .persistent()
                .get(&DataKey::LastSpendLedger(owner.clone(), delegate.clone()))
                .unwrap_or(0),
            min_interval_secs: Self::get_velocity_limit_secs(env.clone()),
            last_spend_timestamp: env
                .storage()
                .persistent()
                .get(&DataKey::LastSpendTimestamp(owner, delegate))
                .unwrap_or(0),
        }
    }

    /// Configure the rolling-window velocity cap (issue #368). Admin-only.
    ///
    /// Even a delegate that keeps every transaction under `limit_per_tx` cannot
    /// move more than `max_spend_in_window` within any `window_ledgers`-long
    /// sliding window. Set `max_spend_in_window` to `0` to disable the cap; a
    /// non-zero cap requires a non-zero `window_ledgers`.
    pub fn set_rolling_window_limit(
        env: Env,
        admin: Address,
        window_ledgers: u32,
        max_spend_in_window: i128,
    ) -> Result<(), PermissionError> {
        Self::require_admin(&env, &admin)?;

        if max_spend_in_window < 0
            || window_ledgers > MAX_ROLLING_WINDOW_LEDGERS
            || (max_spend_in_window > 0 && window_ledgers == 0)
        {
            return Err(PermissionError::InvalidParam);
        }

        let previous = Self::rolling_window_config(&env);
        let config = RollingWindowLimit {
            window_ledgers,
            max_spend_in_window,
            current_window_spend: 0,
            window_start_ledger: 0,
        };
        env.storage()
            .instance()
            .set(&DataKey::RollingWindowConfig, &config);

        env.events().publish(
            (symbol_short!("perm"), symbol_short!("rwset")),
            RollingWindowSetEvent {
                previous_window_ledgers: previous
                    .as_ref()
                    .map(|p| p.window_ledgers)
                    .unwrap_or(0),
                previous_max_spend: previous
                    .as_ref()
                    .map(|p| p.max_spend_in_window)
                    .unwrap_or(0),
                window_ledgers,
                max_spend_in_window,
                set_by: admin,
            },
        );

        Ok(())
    }

    /// Returns the configured rolling-window cap, or a zeroed value when unset.
    pub fn get_rolling_window_limit(env: Env) -> RollingWindowLimit {
        Self::rolling_window_config(&env).unwrap_or(RollingWindowLimit {
            window_ledgers: 0,
            max_spend_in_window: 0,
            current_window_spend: 0,
            window_start_ledger: 0,
        })
    }

    /// Returns the current rolling-window state for an (owner, delegate) pair
    /// after applying any pending window roll-over (issue #368).
    pub fn get_rolling_window_state(
        env: Env,
        owner: Address,
        delegate: Address,
    ) -> RollingWindowLimit {
        Self::load_rolling_window_state(&env, &owner, &delegate)
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
            (symbol_short!("perm"), symbol_short!("schemreg"), admin.clone()),
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
            None,
        )?;
        Self::store_metadata(&env, &owner, &delegate, metadata);
        Ok(())
    }

    /// Explicitly replaces an existing permission while storing optional
    /// metadata (issue #51). Semantics mirror [`Self::re_grant`] combined with
    /// the metadata handling of [`Self::grant_with_metadata`]: permitted on a
    /// live permission, fails with `PermissionError::PermissionNotFound` if no
    /// record exists, and the granted event reports the previous spent amount.
    ///
    /// Unlike [`Self::re_grant`], this entrypoint does **not** clear an
    /// existing [`ScopedPermissionConfig`] (issue #369). It exists to change
    /// limits/metadata, so dropping the function scope here would silently
    /// widen the delegate's authority as a side effect of a limit bump. The
    /// scope is preserved verbatim; an owner who wants to drop it uses
    /// [`Self::re_grant`] (clears) or [`Self::set_permission_scope`] with
    /// `None` (clears without touching limits).
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

        // Issue #369: read the live scope before `grant_impl` overwrites the
        // record, then hand it straight back so the narrowest-restriction
        // path is the default on the metadata entrypoint.
        let preserved_scope: Option<ScopedPermissionConfig> =
            Self::load_scope(&env, &owner, &delegate);

        Self::grant_impl(
            env.clone(),
            owner.clone(),
            delegate.clone(),
            limit_total,
            limit_per_tx,
            allowed_merchants,
            ttl_ledgers,
            true,
            preserved_scope,
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

    /// Writes a `(owner, delegate)` pair's function scope, or removes the slot
    /// entirely when `scope` is `None` (issue #369). Removal — rather than
    /// storing an empty config — keeps an unscoped delegation byte-identical
    /// to one that was never scoped.
    fn store_scope(
        env: &Env,
        owner: &Address,
        delegate: &Address,
        scope: Option<ScopedPermissionConfig>,
    ) {
        let scope_key = DataKey::PermissionScope(owner.clone(), delegate.clone());
        match scope {
            Some(s) => env.storage().persistent().set(&scope_key, &s),
            None => {
                if env.storage().persistent().has(&scope_key) {
                    env.storage().persistent().remove(&scope_key);
                }
            }
        }
    }

    /// Reads a `(owner, delegate)` pair's function scope, or `None` when the
    /// delegation is unscoped or unknown (issue #369).
    fn load_scope(
        env: &Env,
        owner: &Address,
        delegate: &Address,
    ) -> Option<ScopedPermissionConfig> {
        env.storage()
            .persistent()
            .get(&DataKey::PermissionScope(owner.clone(), delegate.clone()))
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

    /// Returns one page of the retained audit log for a (owner, delegate) pair.
    /// The cursor is a zero-based logical offset and each page contains at most
    /// `MAX_AUDIT_PAGE_SIZE` entries.
    pub fn get_audit_log_page(
        env: Env,
        owner: Address,
        delegate: Address,
        cursor: Option<u32>,
    ) -> AuditTrailPage {
        let storage = env.storage().persistent();
        let count_key = DataKey::AuditLogCount(owner.clone(), delegate.clone());
        let start = cursor.unwrap_or(0);
        let mut entries = Vec::new(&env);

        if let Some(total_entries) = storage.get::<_, u32>(&count_key) {
            let first = start.min(total_entries);
            let end = first.saturating_add(MAX_AUDIT_PAGE_SIZE).min(total_entries);
            let oldest_slot: u32 = storage
                .get(&DataKey::AuditLogStart(owner.clone(), delegate.clone()))
                .unwrap_or(0);
            for logical_index in first..end {
                let physical_index = (oldest_slot + logical_index) % MAX_AUDIT_ENTRIES;
                if let Some(entry) = storage.get(&DataKey::AuditLogAt(
                    owner.clone(),
                    delegate.clone(),
                    physical_index,
                )) {
                    entries.push_back(entry);
                }
            }
            return AuditTrailPage {
                entries,
                total_entries,
                next_cursor: if end < total_entries { Some(end) } else { None },
            };
        }

        // Read pre-indexed logs during the storage migration window. New
        // writes migrate this bounded legacy vector into indexed entries.
        let legacy: Vec<AuditLogEntry> = storage
            .get(&DataKey::AuditLog(owner, delegate))
            .unwrap_or_else(|| Vec::new(&env));
        let total_entries = legacy.len();
        let first = start.min(total_entries);
        let end = first.saturating_add(MAX_AUDIT_PAGE_SIZE).min(total_entries);
        for i in first..end {
            if let Some(entry) = legacy.get(i) {
                entries.push_back(entry);
            }
        }
        AuditTrailPage {
            entries,
            total_entries,
            next_cursor: if end < total_entries { Some(end) } else { None },
        }
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
        let storage = env.storage().persistent();
        let count_key = DataKey::AuditLogCount(owner.clone(), delegate.clone());
        let start_key = DataKey::AuditLogStart(owner.clone(), delegate.clone());
        let mut start: u32 = storage.get(&start_key).unwrap_or(0);
        let mut count: u32 = match storage.get(&count_key) {
            Some(count) => count,
            None => {
                let legacy: Vec<AuditLogEntry> = storage
                    .get(&DataKey::AuditLog(owner.clone(), delegate.clone()))
                    .unwrap_or_else(|| Vec::new(env));
                let legacy_count = legacy.len().min(MAX_AUDIT_ENTRIES);
                for i in 0..legacy_count {
                    if let Some(entry) = legacy.get(i) {
                        storage.set(
                            &DataKey::AuditLogAt(owner.clone(), delegate.clone(), i),
                            &entry,
                        );
                    }
                }
                storage.remove(&DataKey::AuditLog(owner.clone(), delegate.clone()));
                legacy_count
            }
        };
        let slot = if count < MAX_AUDIT_ENTRIES {
            let slot = (start + count) % MAX_AUDIT_ENTRIES;
            count += 1;
            slot
        } else {
            let slot = start;
            start = (start + 1) % MAX_AUDIT_ENTRIES;
            slot
        };
        storage.set(
            &DataKey::AuditLogAt(owner.clone(), delegate.clone(), slot),
            &AuditLogEntry {
                action,
                actor,
                timestamp: env.ledger().timestamp(),
            },
        );
        storage.set(&count_key, &count);
        storage.set(&start_key, &start);
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
mod fuzz_tests;

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
}

#[cfg(test)]
mod audit_log_page_tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger};

    #[test]
    fn audit_log_pages_are_bounded_and_keep_the_latest_entries() {
        let env = Env::default();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        env.as_contract(&contract_id, || {
            for timestamp in 0..205u64 {
                env.ledger().set_timestamp(timestamp);
                PermissionsContract::append_audit_log(
                    &env,
                    &owner,
                    &delegate,
                    owner.clone(),
                    symbol_short!("grant"),
                );
            }
        });

        let first = client.get_audit_log_page(&owner, &delegate, &None);
        assert_eq!(first.total_entries, MAX_AUDIT_ENTRIES);
        assert_eq!(first.entries.len(), MAX_AUDIT_PAGE_SIZE);
        assert_eq!(first.entries.get(0).unwrap().timestamp, 5);
        assert_eq!(first.entries.get(19).unwrap().timestamp, 24);
        assert_eq!(first.next_cursor, Some(MAX_AUDIT_PAGE_SIZE));

        let last = client.get_audit_log_page(&owner, &delegate, &Some(180));
        assert_eq!(last.entries.len(), MAX_AUDIT_PAGE_SIZE);
        assert_eq!(last.entries.get(0).unwrap().timestamp, 185);
        assert_eq!(last.entries.get(19).unwrap().timestamp, 204);
        assert_eq!(last.next_cursor, None);
    }

    #[test]
    fn first_audit_write_migrates_legacy_vector_to_indexed_storage() {
        let env = Env::default();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);
        let legacy = soroban_sdk::vec![
            &env,
            AuditLogEntry {
                action: symbol_short!("grant"),
                actor: owner.clone(),
                timestamp: 1,
            },
            AuditLogEntry {
                action: symbol_short!("revoke"),
                actor: owner.clone(),
                timestamp: 2,
            },
        ];

        env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .set(&DataKey::AuditLog(owner.clone(), delegate.clone()), &legacy);
            PermissionsContract::append_audit_log(
                &env,
                &owner,
                &delegate,
                owner.clone(),
                symbol_short!("renew"),
            );
            assert!(!env
                .storage()
                .persistent()
                .has(&DataKey::AuditLog(owner.clone(), delegate.clone())));
        });

        let page = client.get_audit_log_page(&owner, &delegate, &None);
        assert_eq!(page.total_entries, 3);
        assert_eq!(page.entries.len(), 3);
        assert_eq!(
            page.entries.get(2).unwrap().timestamp,
            env.ledger().timestamp()
        );
    }
}
