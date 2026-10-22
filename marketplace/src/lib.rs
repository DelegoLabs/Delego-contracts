//! Delego Marketplace Contract (Merchant Registry & Discovery)
//!
//! Maintains a trusted on-chain registry of merchants with multi-verifier verification,
//! paginated category and name discovery, commission configuration, metadata cooldown lock,
//! status lifecycle controls, and reputation score snapshot integration.

// Contract crates compile as no_std for release and wasm builds, but keep std
// enabled during testing so dev-dependencies and test assertions operate normally.
// This exact conditional form must be consistent across all workspace contract crates.
#![cfg_attr(not(test), no_std)]
#![allow(clippy::too_many_arguments)]
#![warn(missing_docs)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env,
    InvokeError, String, Symbol, Vec,
};

/// Storefront metadata storage and update support.
pub mod storefront;
pub mod oracle;
pub use oracle::*;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[contracttype]
#[repr(u32)]
pub enum MerchantStatus {
    Registered = 0, // Created, not yet verified
    Verified = 1,   // Passed verification
    Suspended = 2,  // Temporarily disabled (admin action / review)
    Closed = 3,     // Permanently removed
    All = 4,        // Filter sentinel for cursor discovery (matches every status)
    Banned = 5,     // Permanently barred from trading
    /// Storefront stopped taking new orders while escrows opened before the
    /// transition are still being fulfilled. A merchant may only reach
    /// `Closed` once every one of those escrows has settled.
    Inactive = 6,
    /// Temporary maintenance mode - storefront pauses new orders but remains visible.
    /// Auto-releases after the scheduled maintenance window expires.
    Maintenance = 7,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct Merchant {
    pub id: u64,
    pub owner: Option<Address>,
    pub name: String,
    pub description: String,
    pub category: Symbol,
    pub image_url: String,
    pub commission_rate_bps: u32,
    pub metadata: Option<String>,
    /// Off-chain metadata URI (IPFS CIDv1 or HTTPS). Capped at
    /// [`MAX_METADATA_URI_LEN`] bytes; `None` when not supplied at
    /// registration time.
    pub metadata_uri: Option<String>,
    pub status: MerchantStatus,
    pub verified: bool,
    pub created_at: u64,
    pub updated_at: u64,
    pub reputation: Option<Address>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct MerchantView {
    pub id: u64,
    pub name: String,
    pub category: Symbol,
    pub commission_rate_bps: u32,
    pub verified: bool,
    pub status: MerchantStatus,
    pub reputation_score: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub enum ReputationResolution {
    NotConfigured,
    Available(u32, u64),
    CallFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct MerchantStats {
    pub total: u64,
    pub active: u64,
    pub suspended: u64,
    pub closed: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct DiscoveryPage {
    pub items: Vec<MerchantView>,
    pub total: u32,
    pub next_offset: Option<u32>,
    pub next_cursor: Option<u64>,
}

/// Verification tier of a merchant, derived from its verification state and
/// lifecycle status.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[contracttype]
#[repr(u32)]
pub enum MerchantTier {
    /// Not verified (or no longer in good standing).
    Unverified = 0,
    /// Verified and in good standing.
    Verified = 1,
    /// Verified and suspended (temporarily out of good standing).
    Suspended = 2,
    /// Permanently closed or banned.
    Terminated = 3,
}

/// Consolidated operational snapshot of a merchant, returned by a single
/// read call so external dApps do not need to stitch together several RPCs.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct MerchantOperationalView {
    pub status: MerchantStatus,
    pub tier: MerchantTier,
    pub verified: bool,
    pub effective_fee_bps: u32,
    pub bond_balance: i128,
    pub dispute_count: u32,
}

/// Routing configuration for marketplace commission disbursements.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct TreasurySplitConfig {
    pub dao_treasury: Address,
    pub dao_bps: u32,
    pub arbiter_pool: Address,
    pub arbiter_bps: u32,
    pub insurance_pool: Address,
    pub insurance_bps: u32,
}

/// Liquidation reserve locked against a high-value order until maturity.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct LiquidationReserve {
    pub merchant: Address,
    pub order_id: u64,
    pub reserve_amount: i128,
    pub unlock_ledger: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct MerchantCursor {
    pub after_id: u64,
    pub status: MerchantStatus,
    pub limit: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct NameRelease {
    pub name: String,
    pub released_at: u64,
    pub previous_merchant: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct MerchantViewDetailed {
    pub view: MerchantView,
    pub reputation: ReputationResolution,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct ArchivedMerchant {
    pub id: u64,
    pub closed_at: u64,
    pub last_view: MerchantView,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct CategoryEntry {
    pub key: Symbol,
    pub normalized: Symbol,
    pub display: String,
    pub added_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct CategoryProposal {
    pub proposed_symbol: Symbol,
    pub proposed_by: Address,
    pub vote_count: u32,
    pub voting_deadline: u64,
    pub is_approved: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct RegisterParams {
    pub name: String,
    pub description: String,
    pub category: Symbol,
    pub image_url: String,
    pub metadata: Option<String>,
    /// Optional URI pointing to off-chain merchant metadata (IPFS CID or HTTPS
    /// URL). Stored as a `soroban_sdk::String` and capped at
    /// [`MAX_METADATA_URI_LEN`] bytes to support both IPFS base58/base32 CIDv1
    /// strings and HTTPS URIs without an arbitrary 32-byte truncation.
    pub metadata_uri: Option<String>,
    pub required_verifications: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AffiliateConfig {
    pub referrer_address: Address,
    pub referral_share_bps: u32,
    pub expires_at_ledger: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct VerificationPolicy {
    pub required: u32,
    pub max_verifications: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct Verifier {
    pub address: Address,
    pub label: Symbol,
    pub registered_at: u64,
}

/// One page of the verifier set, returned by [`MarketplaceContract::get_verifiers_page`].
///
/// `total` is the total number of registered verifiers (the size of the whole
/// set, not of `items`), and `next_offset` is the offset to pass to the next
/// call to continue pagination, or `None` when the last page has been reached.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct VerifierPage {
    pub items: Vec<Verifier>,
    pub total: u32,
    pub next_offset: Option<u32>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractVersion {
    pub name: Symbol,
    pub semver: Symbol,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MerchantValidationError {
    EmptyName,
    EmptyDescription,
    WhitespaceOnly,
}

impl From<MerchantValidationError> for MarketplaceError {
    fn from(e: MerchantValidationError) -> Self {
        match e {
            MerchantValidationError::EmptyName
            | MerchantValidationError::EmptyDescription
            | MerchantValidationError::WhitespaceOnly => MarketplaceError::InvalidParam,
        }
    }
}

/// Cross-contract error code allocation.
///
/// To keep bridge error mapping unambiguous, each contract owns a disjoint
/// numeric range. The Marketplace contract must stay within its range:
/// | Contract            | Error code range |
/// |---------------------|------------------|
/// | `PermissionError`   | 1..=999          |
/// | `EscrowError`       | 1000..=1999      |
/// | `ReputationError`   | 2000..=2999      |
/// | `DelegationError`   | 3000..=3999      |
/// | `MarketplaceError`  | 4000..=4999      |
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MarketplaceError {
    AlreadyInitialized = 5001,
    NotInitialized = 5002,
    Unauthorized = 5003,
    MerchantNotFound = 5004,
    AlreadyVerified = 5005,
    InvalidCommissionBps = 5006,
    DuplicateMerchantName = 5007,
    MerchantFrozen = 5008,
    MerchantClosed = 5009,
    VerifierAlreadyExists = 5010,
    VerifierNotFound = 5011,
    InsufficientVerifications = 5012,
    MetadataLockActive = 5013,
    InvalidCategory = 5014,
    InvalidParam = 5015,
    NoPendingAdmin = 5016,
    VerificationCountOverflow = 5017,
    DuplicateMerchantOwner = 5018,
    /// Merchant has been permanently banned from trading.
    MerchantBanned = 5019,
    /// Merchant category does not match authorized categories for this operation
    MerchantCategoryNotAllowed = 5020,
    /// `metadata_uri` field exceeds [`MAX_METADATA_URI_LEN`] bytes.
    MetadataUriTooLong = 4021,
    /// Storefront metadata contains an invalid IPFS CID.
    InvalidIpfsCid = 5021,
    MetadataUriTooLong = 5021,
    /// Insufficient registration deposit provided
    InsufficientDeposit = 4022,
    /// Cannot refund deposit while merchant has open escrows
    HasOpenEscrows = 4023,
    /// Deposit already refunded or does not exist
    DepositNotFound = 4024,
    /// Merchant must be in Suspended status to file an appeal.
    MerchantNotSuspended = 4030,
    /// A pending (Pending) appeal bond already exists for this merchant.
    AppealAlreadyPending = 4031,
    /// Bond amount must be greater than zero.
    ZeroBondAmount = 4032,
    /// No appeal bond found for this merchant.
    AppealNotFound = 4025,
    /// Appeal has already been resolved and cannot be resolved again.
    AppealAlreadyResolved = 4026,
    /// The treasury address for slashed bonds has not been configured.
    TreasuryNotSet = 4027,
    /// The token used for appeal bonds has not been configured.
    AppealBondTokenNotSet = 4028,
    /// The merchant storefront is inactive (winding down / deactivated).
    MerchantInactive = 4029,
    /// Category proposal not found.
    CategoryProposalNotFound = 4033,
    /// Category proposal expired.
    CategoryProposalExpired = 4034,
    /// Category already voted.
    CategoryAlreadyVoted = 4035,
    /// Merchant not verified for this operation.
    MerchantNotVerified = 4036,
    /// Active orders pending while attempting closure.
    ActiveOrdersPending = 4037,
    /// SKU inventory not found.
    SkuNotFound = 4038,
    /// A maintenance window is already active for this merchant.
    MaintenanceActive = 4039,
    /// Invalid maintenance window configuration.
    InvalidMaintenanceWindow = 4040,
    /// Insufficient stock available for the requested quantity.
    InsufficientStock = 4041,
    /// Invalid SKU ID format or parameters.
    InvalidSku = 4042,
    /// Stock reservation failed due to concurrent modifications.
    StockReservationFailed = 4043,
    /// The verifier set has reached [`MAX_VERIFIERS`]; remove one before adding another.
    VerifierLimitReached = 5022,
}

// ---------------------------------------------------------------------------
// Appeal bond types
// ---------------------------------------------------------------------------

/// Resolution state of a merchant suspension appeal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppealResolution {
    /// Appeal has been filed and is awaiting governance ruling.
    Pending,
    /// Governance upheld the appeal; merchant is reinstated and bond refunded.
    UpheldAndReinstated,
    /// Governance rejected the appeal as frivolous; bond is slashed to treasury.
    RejectedAndSlashed,
}

/// On-chain record of a merchant's suspension appeal and its staked bond.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantAppealBond {
    /// ID of the merchant filing the appeal.
    pub merchant_id: u64,
    /// Amount of tokens posted as a bond.
    pub bond_amount: i128,
    /// Token contract address for the bond asset.
    pub bond_token: Address,
    /// Ledger sequence at which the appeal was filed.
    pub appealed_at_ledger: u32,
    /// Current resolution status of this appeal.
    pub resolution_status: AppealResolution,
}

// ---------------------------------------------------------------------------
// Appeal bond events
// ---------------------------------------------------------------------------

/// Emitted when a merchant files a suspension appeal by posting a bond.
#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantAppealFiledEvent {
    /// ID of the appealing merchant.
    pub merchant_id: u64,
    /// Bond amount locked in the contract.
    pub bond_amount: i128,
    /// Token used for the bond.
    pub bond_token: Address,
    /// Ledger sequence at which the appeal was recorded.
    pub appealed_at_ledger: u32,
}

/// Emitted when governance resolves a merchant appeal (either outcome).
#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantAppealResolvedEvent {
    /// ID of the merchant whose appeal was resolved.
    pub merchant_id: u64,
    /// Bond amount that was either refunded or slashed.
    pub bond_amount: i128,
    /// Final resolution: [`AppealResolution::UpheldAndReinstated`] or
    /// [`AppealResolution::RejectedAndSlashed`].
    pub resolution: AppealResolution,
    /// Address that received the bond (merchant owner on reinstatement,
    /// treasury on slash).
    pub recipient: Address,
    CategoryProposalNotFound = 4022,
    CategoryProposalExpired = 4023,
    CategoryAlreadyVoted = 4024,
    MerchantNotVerified = 4025,
    MetadataUriTooLong = 5021,
    /// A closure was requested while the merchant still has escrows that have
    /// not been released, refunded or cancelled.
    ActiveOrdersPending = 4022,
    /// The merchant storefront is [`MerchantStatus::Inactive`]: it no longer
    /// accepts profile, verification, category or commission changes.
    MerchantInactive = 4023,
    /// SKU inventory not found for the specified merchant and SKU ID.
    SkuNotFound = 4024,
    /// Insufficient stock available for the requested quantity.
    InsufficientStock = 4025,
    /// Invalid SKU ID format or parameters.
    InvalidSku = 4026,
    /// Maintenance window is currently active for this merchant.
    MaintenanceActive = 4027,
    /// Invalid maintenance window configuration.
    InvalidMaintenanceWindow = 4028,
    /// Stock reservation failed due to concurrent modifications.
    StockReservationFailed = 4029,
    /// Treasury split configuration has not been set.
    TreasurySplitNotConfigured = 4030,
    /// Treasury split basis points do not sum to exactly 10,000.
    InvalidTreasurySplit = 4031,
    /// Liquidation reserve not found for the given order.
    ReserveNotFound = 4032,
    /// Liquidation reserve is still locked until its maturity ledger.
    ReserveLocked = 4033,
    /// Liquidation reserve has already been withdrawn or liquidated.
    ReserveAlreadyReleased = 4034,
    /// Price timestamp is older than the allowed staleness threshold or from the future.
    StaleOraclePrice = 5030,
}

// --- Events ---
//
// All events follow the 3-topic Soroban convention `(contract, action, entity_id)`
// so off-chain indexers and Soroban RPC subscriptions can filter by entity from
// the topics alone, without deserializing the event body (issue #142).
//
// Topic 0: `symbol_short!("mkplc")` — contract namespace.
// Topic 1: `Symbol` action name (e.g. `symbol_short!("created")`).
// Topic 2: Entity ID — `u64` merchant id, or `Address` for non-merchant-scoped
//          events (verifiers, admin).
//
// The entity id is also retained in the event data payload for convenience.
//
// Event schema reference:
// | Event                              | Topic 1     | Topic 2 (entity) |
// |------------------------------------|-------------|------------------|
// | MerchantRegisteredEvent            | created     | u64 merchant_id  |
// | MerchantVerifiedEvent              | verified    | u64 merchant_id  |
// | MerchantProfileUpdatedEvent        | updated     | u64 merchant_id  |
// | MerchantCategoryChangedEvent       | category    | u64 merchant_id  |
// | MerchantMetadataUpdatedEvent       | metadata    | u64 merchant_id  |
// | MerchantCommissionSetEvent         | commission  | u64 merchant_id  |
// | MerchantSuspendedEvent             | suspended   | u64 merchant_id  |
// | MerchantBannedEvent                | banned      | u64 merchant_id  |
// | MerchantUnsuspendedEvent           | unsuspend   | u64 merchant_id  |
// | MerchantClosedEvent                | closed      | u64 merchant_id  |
// | MerchantVerificationRevokedEvent   | revoked     | u64 merchant_id  |
// | MerchantReputationSetEvent         | reputation  | u64 merchant_id  |
// | VerifierAddedEvent                 | verifier    | Address verifier |
// | VerifierRemovedEvent               | unverifier  | Address verifier |
// | AdminProposedEvent                 | adminprop   | Address new_admin|
// | AdminAcceptedEvent                 | adminacc    | Address new_admin|
// | CategoryAddedEvent                 | catadd      | Symbol key       |
// | CategoryRemovedEvent               | catrm       | Symbol key       |
// | MetadataCooldownSetEvent           | cooldown    | Address set_by   |
// | MerchantPrunedEvent                | pruned      | Address pruned_by|

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantRegisteredEvent {
    pub merchant_id: u64,
    pub owner: Address,
    pub name: String,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantVerifiedEvent {
    pub merchant_id: u64,
    pub verifier: Address,
    pub verified: bool,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantProfileUpdatedEvent {
    pub merchant_id: u64,
    pub updated_fields: Symbol,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CategoryChange {
    pub merchant_id: u64,
    pub from: Symbol,
    pub to: Symbol,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantCategoryChangedEvent {
    pub merchant_id: u64,
    pub from: Symbol,
    pub to: Symbol,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantMetadataUpdatedEvent {
    pub merchant_id: u64,
    pub new_metadata: Option<String>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CooldownConfig {
    pub value_seconds: u64,
    pub min_seconds: u64,
    pub max_seconds: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataCooldownSetEvent {
    pub previous: Option<u64>,
    pub current: u64,
    pub set_by: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantCommissionSetEvent {
    pub merchant_id: u64,
    pub commission_bps: u32,
    pub set_by: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantSuspendedEvent {
    pub merchant_id: u64,
    pub suspended_by: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantBannedEvent {
    pub merchant_id: u64,
    pub banned_by: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantUnsuspendedEvent {
    pub merchant_id: u64,
    pub unsuspended_by: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantClosedEvent {
    pub merchant_id: u64,
    pub closed_by: Address,
    pub reason: Symbol,
    pub name: String,
    pub category: Symbol,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantDepositRefundedEvent {
    pub merchant_id: u64,
    pub owner: Address,
    pub amount: i128,
    pub token: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantInactiveEvent {
    pub merchant_id: u64,
    pub inactive_by: Address,
    pub pending_escrows: u32,
}

/// Emitted when a merchant storefront enters maintenance mode.
#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantMaintenanceStartedEvent {
    pub merchant_id: u64,
    pub started_at: u64,
    pub scheduled_end: u64,
    pub reason: Option<String>,
}

/// Emitted when a merchant storefront exits maintenance mode (auto or manual).
#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantMaintenanceEndedEvent {
    pub merchant_id: u64,
    pub ended_at: u64,
    pub was_automatic: bool,
}

/// Emitted when SKU inventory is created or initialized.
#[contracttype]
#[derive(Clone, Debug)]
pub struct SkuInventoryCreatedEvent {
    pub merchant_id: u64,
    pub sku_id: String,
    pub initial_stock: u64,
}

/// Emitted when SKU inventory stock is decremented atomically.
#[contracttype]
#[derive(Clone, Debug)]
pub struct SkuStockDecrementedEvent {
    pub merchant_id: u64,
    pub sku_id: String,
    pub quantity: u64,
    pub remaining_stock: u64,
}

/// Emitted when SKU inventory stock is replenished.
#[contracttype]
#[derive(Clone, Debug)]
pub struct SkuStockReplenishedEvent {
    pub merchant_id: u64,
    pub sku_id: String,
    pub quantity: u64,
    pub new_stock: u64,
}

/// Emitted when SKU stock is reserved for a pending order.
#[contracttype]
#[derive(Clone, Debug)]
pub struct SkuStockReservedEvent {
    pub merchant_id: u64,
    pub sku_id: String,
    pub quantity: u64,
    pub reserved_total: u64,
}

/// Emitted when reserved SKU stock is released back to available.
#[contracttype]
#[derive(Clone, Debug)]
pub struct SkuStockReleasedEvent {
    pub merchant_id: u64,
    pub sku_id: String,
    pub quantity: u64,
    pub available_stock: u64,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct VerifierAddedEvent {
    pub verifier: Address,
    pub label: Symbol,
    pub added_by: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct VerifierRemovedEvent {
    pub verifier: Address,
    pub removed_by: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantVerificationRevokedEvent {
    pub merchant_id: u64,
    pub revoked_by: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminProposedEvent {
    pub current_admin: Address,
    pub new_admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminAcceptedEvent {
    pub previous_admin: Address,
    pub new_admin: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantReputationSetEvent {
    pub merchant_id: u64,
    pub reputation: Option<Address>,
    pub set_by: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct CategoryAddedEvent {
    pub key: Symbol,
    pub normalized: Symbol,
    pub added_by: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct CategoryRemovedEvent {
    pub key: Symbol,
    pub removed_by: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantPrunedEvent {
    pub pruned_count: u32,
    pub pruned_by: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommissionTier {
    pub min_settled_volume: i128,
    pub commission_rate_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantVolumeRecord {
    pub total_settled_volume: i128,
    pub active_tier_bps: u32,
    pub last_updated_ledger: u32,
}

/// Verification badge tier for a merchant.
///
/// Merchants advance through tiers based on cumulative settled sales volume,
/// dispute rate, and bond collateral. The tier is recomputed on every
/// volume/dispute update and transitions emit [`MerchantTierUpdatedEvent`].
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MerchantTier {
    Unverified = 0,
    Bronze = 1,
    Silver = 2,
    Gold = 3,
    Platinum = 4,
}

/// Per-merchant tier state: current tier plus the inputs used to derive it.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantTierState {
    pub tier: MerchantTier,
    /// Cumulative settled sales volume in stroops.
    pub cumulative_volume: i128,
    /// Disputes observed in the current window.
    pub disputes: u32,
    /// Total settled orders observed in the current window.
    pub total_orders: u32,
    /// Ledger timestamp of the last evaluation.
    pub last_evaluated: u64,
}

/// Emitted whenever a merchant's verification tier changes.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantTierUpdatedEvent {
    pub merchant_id: u64,
    pub previous_tier: MerchantTier,
    pub new_tier: MerchantTier,
    pub cumulative_volume: i128,
    pub dispute_rate_bps: u32,
}

// Tier qualification thresholds. Volume is in stroops (1 XLM = 10_000_000).
// Dispute rate is expressed in basis points (100 bps = 1%).
pub const TIER_BRONZE_MIN_VOLUME: i128 = 100_000_000; // 10 XLM
pub const TIER_SILVER_MIN_VOLUME: i128 = 1_000_000_000; // 100 XLM
pub const TIER_GOLD_MIN_VOLUME: i128 = 100_000_000_000; // 10,000 XLM
pub const TIER_PLATINUM_MIN_VOLUME: i128 = 1_000_000_000_000; // 100,000 XLM
pub const TIER_MAX_DISPUTE_RATE_BPS: u32 = 100; // < 1% dispute rate

/// Snapshot stored when a merchant is permanently closed.
/// Maintenance mode configuration for a merchant storefront.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaintenanceWindow {
    /// Merchant ID under maintenance.
    pub merchant_id: u64,
    /// Unix timestamp when maintenance mode was activated.
    pub started_at: u64,
    /// Unix timestamp when maintenance mode will automatically end.
    pub scheduled_end: u64,
    /// Optional reason for maintenance (e.g., "System upgrade").
    pub reason: Option<String>,
    /// Previous status before entering maintenance mode.
    pub previous_status: MerchantStatus,
}

/// SKU (Stock Keeping Unit) inventory record with atomic decrement support.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkuInventory {
    /// Unique SKU identifier (merchant-scoped).
    pub sku_id: String,
    /// Merchant ID that owns this SKU.
    pub merchant_id: u64,
    /// Current available stock count.
    pub available_stock: u64,
    /// Total stock ever added (for analytics).
    pub total_added: u64,
    /// Total stock sold/decremented (for analytics).
    pub total_sold: u64,
    /// Reserved stock (pending orders not yet confirmed).
    pub reserved_stock: u64,
    /// Timestamp of last stock update.
    pub last_updated: u64,
    /// Whether this SKU is currently active for sale.
    pub is_active: bool,
}

// --- Storage Keys ---

#[contracttype]
pub enum DataKey {
    Admin,
    PendingAdmin,
    NextMerchantId,
    MerchantStats,
    Merchant(u64),
    MerchantOwner(Address),
    RiskOracle,
    MerchantName(String),
    FreedName(String),
    ArchivedMerchant(u64),
    MerchantArchivedAt(u64),
    VerifiedCount(u64),
    /// Number of escrows opened for this merchant that have not settled yet.
    /// Closure is refused while this is non-zero.
    ActiveEscrowCount(u64),
    Verifiers,
    MerchantIds,
    CategoryIndex(Symbol),
    MetadataCooldown,
    MetadataCooldownConfig,
    MerchantVerifier(u64, Address),
    MerchantVerifierList(u64),
    VerificationPolicy(u64),
    CategoryProposal(Symbol),
    CategoryVote(Symbol, Address, u64),
    LastMetadataUpdate(u64),
    GlobalReputationContract,
    Categories,
    CommissionTiers,
    MerchantVolume(u64),
    MerchantDeposit(u64),
    DepositToken(u64),
    TotalMerchantsRegistered,
    /// Stores the [`MerchantAppealBond`] for a given merchant id.
    AppealBond(u64),
    /// Address to receive slashed appeal bonds (the customer restitution pool).
    AppealTreasury,
    /// Token contract used for merchant suspension appeal bonds.
    AppealBondToken,
    /// Records the ledger timestamp at which a merchant's verification policy
    /// was raised, so a grace period can be applied before enforcement.
    PolicyRaisedAt(u64),
    /// Records the previous `required` value before a policy increase.
    PolicyPreviousRequired(u64),
    PendingPayoutChange(u64),
    PayoutAddress(u64),
    /// Maintenance window configuration for a merchant.
    MaintenanceWindow(u64),
    /// SKU inventory record: (merchant_id, sku_id).
    SkuInventory(u64, String),
    /// List of all SKU IDs for a merchant.
    MerchantSkuList(u64),
    /// Storefront metadata record for a merchant.
    StorefrontMetadata(u64),
}

/// Mirror of `ReputationScore` from `delego-reputation` for cross-contract deserialization.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalReputationScore {
    pub entity: Address,
    pub score: u32,
    pub total_transactions: u64,
    pub successful_transactions: u64,
    pub disputed_transactions: u64,
    pub avg_rating: u32,
    pub last_updated: u64,
}

/// Tier-to-fee-discount mapping for staked merchants.
///
/// `tier` is derived from a merchant's reputation score:
/// - 1 (Bronze):   score 0–249
/// - 2 (Silver):   score 250–499
/// - 3 (Gold):     score 500–749
/// - 4 (Platinum): score ≥ 750
///
/// `discount_bps` is the percentage reduction applied to the platform fee,
/// expressed in basis points (e.g. 1000 = 10% discount).
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TierFeeDiscount {
    /// 1 = Bronze, 2 = Silver, 3 = Gold, 4 = Platinum
    pub tier: u32,
    /// Discount on the platform fee, in basis points (e.g. 1000 = 10%).
    pub discount_bps: u32,
}

const MAX_COMMISSION_BPS: u32 = 10_000;
const MAX_REQUIRED_VERIFICATIONS: u32 = 50;
const DEFAULT_METADATA_COOLDOWN_SECS: u64 = 86_400; // 24 hours
const MIN_METADATA_COOLDOWN_SECS: u64 = 60;
const MAX_METADATA_COOLDOWN_SECS: u64 = 30 * 24 * 60 * 60;
const MAX_PAGE_LIMIT: u32 = 50;
const CATEGORY_VOTE_THRESHOLD: u32 = 3;
const CATEGORY_VOTING_PERIOD_SECS: u64 = 7 * 24 * 60 * 60;
const PERSISTENT_BUMP_THRESHOLD: u32 = 17_280; // ~1 day of ledgers (5s/ledger)
const PERSISTENT_BUMP_AMOUNT: u32 = 518_400; // ~30 days of ledgers
pub const PAYOUT_CHANGE_COOLDOWN_LEDGERS: u32 = 17_280; // ~24 hours

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingPayoutChange {
    pub merchant_id: u64,
    pub proposed_payout_address: Address,
    pub effective_at_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantPayoutChangeScheduledEvent {
    pub merchant_id: u64,
    pub proposed_payout_address: Address,
    pub effective_at_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantPayoutChangeCancelledEvent {
    pub merchant_id: u64,
    pub cancelled_by: Address,
}

/// Base registration deposit in stroops (5 XLM = 50,000,000 stroops)
pub const BASE_REGISTRATION_DEPOSIT_STROOPS: i128 = 50_000_000;

/// Compute registration deposit based on total merchants registered
/// Currently returns a fixed base deposit, but can be extended to dynamic curves
pub fn compute_registration_deposit(total_merchants_registered: u64) -> i128 {
    // For now, use fixed deposit. Can be made dynamic later:
    // e.g., BASE_REGISTRATION_DEPOSIT_STROOPS + (total_merchants_registered as i128 * scaling_factor)
    let _ = total_merchants_registered; // Allow for future dynamic logic
    BASE_REGISTRATION_DEPOSIT_STROOPS
}
/// Grace period (in seconds) granted to pre-existing merchants after a
/// verification policy threshold increase, during which they retain their
/// verified status while acquiring the additional attestations.
pub const VERIFICATION_POLICY_GRACE_PERIOD_SECS: u64 = 30 * 24 * 60 * 60;

pub(crate) fn normalize_symbol(env: &Env, sym: &Symbol) -> Symbol {
    use soroban_sdk::xdr::ToXdr;
    let xdr = sym.clone().to_xdr(env);
    let xdr_len = xdr.len() as usize;
    // XDR encoding of ScVal::Symbol:
    // 4-byte tag (ScValType::Symbol = 10) + 4-byte big-endian length + characters + 0..3 padding bytes
    if xdr_len >= 8 {
        let mut xdr_bytes = [0u8; 48];
        let read_len = xdr_len.min(48);
        xdr.copy_into_slice(&mut xdr_bytes[..read_len]);
        let sym_len =
            u32::from_be_bytes([xdr_bytes[4], xdr_bytes[5], xdr_bytes[6], xdr_bytes[7]]) as usize;
        if sym_len <= 32 && 8 + sym_len <= read_len {
            let mut lower_buf = [0u8; 32];
            for i in 0..sym_len {
                lower_buf[i] = xdr_bytes[8 + i].to_ascii_lowercase();
            }
            if let Ok(lower_str) = core::str::from_utf8(&lower_buf[..sym_len]) {
                return Symbol::new(env, lower_str);
            }
        }
    }
    sym.clone()
}
// --- Merchant profile field bounds ---
//
// Caps on `RegisterParams` (and the corresponding `update_merchant_profile`)
// string fields. Enforced *before* any bytes are copied off the host string,
// so a caller cannot force unbounded storage growth or unbounded gas by
// submitting an arbitrarily large `name`/`description`/`image_url`/`metadata`.
pub const MAX_NAME_LEN: u32 = 64;
pub const MAX_DESCRIPTION_LEN: u32 = 512;
pub const MAX_IMAGE_URL_LEN: u32 = 256;
pub const MAX_METADATA_LEN: u32 = 1024;
/// Maximum byte length for a `metadata_uri` value.
///
/// IPFS CIDv1 base58/base32 strings are at most ~60 bytes; HTTPS URIs
/// pointing to off-chain metadata rarely exceed 128 bytes in practice.
/// Keeping this cap tight prevents unbounded storage growth while still
/// accommodating real-world IPFS and HTTPS URIs.
pub const MAX_METADATA_URI_LEN: u32 = 128;

/// Maximum number of verifiers that can be registered at once.
///
/// Bounds both instance storage growth driven by [`MarketplaceContract::add_verifier`]
/// and the read cost of [`MarketplaceContract::get_verifiers`]. Callers that need
/// to read the set in bounded chunks should use
/// [`MarketplaceContract::get_verifiers_page`] instead of fetching the whole vector.
pub const MAX_VERIFIERS: u32 = 50;

// Largest of the caps above; used to size the stack buffer that string
// normalization copies host bytes into. `#![no_std]` without the `alloc`
// feature means we cannot heap-allocate a buffer sized to the actual string,
// so we bound the buffer at the biggest field cap and always bounds-check
// the raw string length against the field-specific cap before copying.
const MAX_FIELD_BUF_LEN: usize = MAX_METADATA_LEN as usize;

/// Read-only closure guard shared by `close_merchant` and the bond-release
/// path.
///
/// `active_escrow_count` is the number the caller observed, while the value
/// persisted under [`DataKey::ActiveEscrowCount`] is the registry's own record
/// for `merchant_id`. The guard fails when either is non-zero, and also when
/// the reported count is *lower* than the recorded one, so a stale or
/// optimistic reading can never let a merchant slip past the grace period
/// while escrows are still open.
pub fn can_close_merchant(
    env: &Env,
    merchant_id: u64,
    active_escrow_count: u32,
) -> Result<(), MarketplaceError> {
    if active_escrow_count > 0 {
        return Err(MarketplaceError::ActiveOrdersPending);
    }
    let recorded: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::ActiveEscrowCount(merchant_id))
        .unwrap_or(0u32);
    if active_escrow_count < recorded {
        return Err(MarketplaceError::ActiveOrdersPending);
    }
    Ok(())
}

#[contract]
pub struct MarketplaceContract;

#[contractimpl]
impl MarketplaceContract {
    // --- Initialization ---

    pub fn __constructor(env: Env, admin: Address) -> Result<(), MarketplaceError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(MarketplaceError::AlreadyInitialized);
        }
        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &Option::<Address>::None);
        env.storage()
            .instance()
            .set(&DataKey::NextMerchantId, &1u64);
        env.storage().instance().set(
            &DataKey::MerchantStats,
            &MerchantStats {
                total: 0,
                active: 0,
                suspended: 0,
                closed: 0,
            },
        );
        env.storage()
            .instance()
            .set(&DataKey::Verifiers, &Vec::<Verifier>::new(&env));
        env.storage()
            .instance()
            .set(&DataKey::Categories, &Vec::<CategoryEntry>::new(&env));
        env.storage()
            .instance()
            .set(&DataKey::MetadataCooldown, &DEFAULT_METADATA_COOLDOWN_SECS);
        env.storage().instance().set(
            &DataKey::MetadataCooldownConfig,
            &CooldownConfig {
                value_seconds: DEFAULT_METADATA_COOLDOWN_SECS,
                min_seconds: MIN_METADATA_COOLDOWN_SECS,
                max_seconds: MAX_METADATA_COOLDOWN_SECS,
            },
        );
        env.storage()
            .instance()
            .set(&DataKey::TotalMerchantsRegistered, &0u64);

        Ok(())
    }

    // --- Archived Merchant ---

    pub fn get_archived_merchant(env: Env, id: u64) -> Option<ArchivedMerchant> {
        env.storage()
            .persistent()
            .get(&DataKey::ArchivedMerchant(id))
    }

    fn validate_merchant_input(
        name: &String,
        description: &String,
    ) -> Result<(), MerchantValidationError> {
        if name.len() == 0 {
            return Err(MerchantValidationError::EmptyName);
        }
        // Soroban String has no .trim(); check that the string contains at
        // least one non-whitespace byte by copying into a stack buffer.
        let name_len = name.len() as usize;
        if name_len > 0 && name_len <= MAX_FIELD_BUF_LEN {
            let mut buf = [0u8; MAX_FIELD_BUF_LEN];
            name.copy_into_slice(&mut buf[..name_len]);
            if buf[..name_len].iter().all(|b| b.is_ascii_whitespace()) {
                return Err(MerchantValidationError::WhitespaceOnly);
            }
        }
        if description.len() == 0 {
            return Err(MerchantValidationError::EmptyDescription);
        }
        let desc_len = description.len() as usize;
        if desc_len > 0 && desc_len <= MAX_FIELD_BUF_LEN {
            let mut buf = [0u8; MAX_FIELD_BUF_LEN];
            description.copy_into_slice(&mut buf[..desc_len]);
            if buf[..desc_len].iter().all(|b| b.is_ascii_whitespace()) {
                return Err(MerchantValidationError::WhitespaceOnly);
            }
        }
        Ok(())
    }

    // --- Merchant Lifecycle ---

    pub fn register_merchant(
        env: Env,
        merchant: Address,
        params: RegisterParams,
        deposit_token: Address,
        deposit_amount: i128,
    ) -> Result<u64, MarketplaceError> {
        merchant.require_auth();

        // Get total merchants registered for deposit calculation
        let total_registered: u64 = env
            .storage()
            .instance()
            .get(&DataKey::TotalMerchantsRegistered)
            .unwrap_or(0);

        let required_deposit = compute_registration_deposit(total_registered);

        // Validate deposit amount
        if deposit_amount < required_deposit {
            return Err(MarketplaceError::InsufficientDeposit);
        }

        // Transfer deposit from merchant to contract
        use soroban_sdk::token;
        let token_client = token::Client::new(&env, &deposit_token);
        token_client.transfer(&merchant, &env.current_contract_address(), &deposit_amount);

        if let Some(existing_id) = env
            .storage()
            .persistent()
            .get::<_, u64>(&DataKey::MerchantOwner(merchant.clone()))
        {
            let existing: Merchant = env
                .storage()
                .persistent()
                .get(&DataKey::Merchant(existing_id))
                .ok_or(MarketplaceError::MerchantNotFound)?;
            if existing.status != MerchantStatus::Closed {
                return Err(MarketplaceError::DuplicateMerchantOwner);
            }
        }

        Self::validate_merchant_input(&params.name, &params.description)?;
        let params = Self::validate_and_normalize(&env, &params)?;

        let name_key = DataKey::MerchantName(params.name.clone());
        if env.storage().persistent().has(&name_key) {
            return Err(MarketplaceError::DuplicateMerchantName);
        }

        // If the name was previously freed, clear the FreedName entry so it can be reused.
        let freed_key = DataKey::FreedName(params.name.clone());
        if env.storage().persistent().has(&freed_key) {
            env.storage().persistent().remove(&freed_key);
        }

        let normalized_category = normalize_symbol(&env, &params.category);

        // Allowlist gating: When non-empty, only allowlisted categories are accepted
        let categories = Self::get_categories(env.clone());
        if categories.len() != 0 {
            let mut allowed = false;
            for c in categories.iter() {
                if c.normalized == normalized_category {
                    allowed = true;
                    break;
                }
            }
            if !allowed {
                return Err(MarketplaceError::InvalidCategory);
            }
        }

        let now = env.ledger().timestamp();
        let next_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextMerchantId)
            .ok_or(MarketplaceError::NotInitialized)?;

        let required_verifications = if params.required_verifications == 0 {
            1
        } else {
            params.required_verifications
        };

        // Determine current verifier capacity and enforce bounds
        let verifiers_len: u32 = Self::get_verifiers(env.clone()).len();
        if required_verifications > MAX_REQUIRED_VERIFICATIONS {
            return Err(MarketplaceError::InvalidParam);
        }
        if verifiers_len > 0 && required_verifications > verifiers_len {
            return Err(MarketplaceError::InvalidParam);
        }

        let new_merchant = Merchant {
            id: next_id,
            owner: Some(merchant.clone()),
            name: params.name.clone(),
            description: params.description,
            category: normalized_category.clone(),
            image_url: params.image_url,
            commission_rate_bps: 0,
            metadata: params.metadata.clone(),
            metadata_uri: params.metadata_uri.clone(),
            status: MerchantStatus::Registered,
            verified: false,
            created_at: now,
            updated_at: now,
            reputation: None,
        };

        // Persist merchant record and indices
        env.storage()
            .persistent()
            .set(&DataKey::Merchant(next_id), &new_merchant);
        env.storage()
            .persistent()
            .set(&DataKey::MerchantOwner(merchant.clone()), &next_id);
        env.storage().persistent().set(&name_key, &next_id);
        let policy = VerificationPolicy {
            required: required_verifications,
            max_verifications: verifiers_len.min(MAX_REQUIRED_VERIFICATIONS),
        };
        env.storage()
            .persistent()
            .set(&DataKey::VerificationPolicy(next_id), &policy);
        env.storage()
            .persistent()
            .set(&DataKey::VerifiedCount(next_id), &0u32);
        env.storage().persistent().set(
            &DataKey::MerchantVerifierList(next_id),
            &Vec::<Address>::new(&env),
        );
        env.storage()
            .persistent()
            .set(&DataKey::LastMetadataUpdate(next_id), &now);

        // Store deposit information
        env.storage()
            .persistent()
            .set(&DataKey::MerchantDeposit(next_id), &deposit_amount);
        env.storage()
            .persistent()
            .set(&DataKey::DepositToken(next_id), &deposit_token);

        // Append to full merchant ids index (persistent storage)
        let mut merchant_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantIds)
            .unwrap_or_else(|| Vec::new(&env));
        merchant_ids.push_back(next_id);
        env.storage()
            .persistent()
            .set(&DataKey::MerchantIds, &merchant_ids);

        let mut stats = Self::get_merchant_stats(env.clone());
        stats.total = stats.total.saturating_add(1);
        stats.active = stats.active.saturating_add(1);
        env.storage()
            .instance()
            .set(&DataKey::MerchantStats, &stats);

        // Append to category index
        let cat_key = DataKey::CategoryIndex(normalized_category.clone());
        let mut cat_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&cat_key)
            .unwrap_or_else(|| Vec::new(&env));
        cat_ids.push_back(next_id);
        env.storage().persistent().set(&cat_key, &cat_ids);

        // Extend TTL for all persistent entries created
        let storage = env.storage().persistent();
        storage.extend_ttl(
            &DataKey::Merchant(next_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        storage.extend_ttl(&name_key, PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
        storage.extend_ttl(
            &DataKey::VerificationPolicy(next_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        storage.extend_ttl(
            &DataKey::VerifiedCount(next_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        storage.extend_ttl(
            &DataKey::MerchantVerifierList(next_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        storage.extend_ttl(
            &DataKey::LastMetadataUpdate(next_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        storage.extend_ttl(
            &DataKey::MerchantIds,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        storage.extend_ttl(&cat_key, PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);

        // Increment monotonic counter
        let incremented = next_id
            .checked_add(1)
            .ok_or(MarketplaceError::InvalidParam)?;
        env.storage()
            .instance()
            .set(&DataKey::NextMerchantId, &incremented);

        // Increment total merchants registered counter
        let new_total_registered = total_registered
            .checked_add(1)
            .ok_or(MarketplaceError::InvalidParam)?;
        env.storage()
            .instance()
            .set(&DataKey::TotalMerchantsRegistered, &new_total_registered);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("reg"), next_id),
            MerchantRegisteredEvent {
                merchant_id: next_id,
                owner: merchant,
                name: params.name,
            },
        );

        Ok(next_id)
    }

    pub fn is_name_available(env: Env, name: String) -> bool {
        let live_key = DataKey::MerchantName(name);
        !env.storage().persistent().has(&live_key)
    }

    pub fn update_merchant_profile(
        env: Env,
        merchant_id: u64,
        caller: Address,
        name: String,
        description: String,
        image_url: String,
        new_category: Option<Symbol>,
    ) -> Result<(), MarketplaceError> {
        caller.require_auth();

        Self::validate_merchant_input(&name, &description)?;
        let name = Self::normalize_bounded_string(&env, &name, MAX_NAME_LEN)?;
        if name.len() == 0 {
            return Err(MarketplaceError::InvalidParam);
        }
        let description = Self::normalize_bounded_string(&env, &description, MAX_DESCRIPTION_LEN)?;
        let image_url = Self::normalize_bounded_string(&env, &image_url, MAX_IMAGE_URL_LEN)?;

        // Validate new_category when provided: must be a valid (non-default) Symbol.
        // Soroban Symbol values are always non-empty when created via symbol_short! or
        // Symbol::new, so no additional validation is needed here beyond type safety.

        let mut merchant = Self::get_merchant(env.clone(), merchant_id)?;
        Self::check_not_frozen_or_closed(&merchant)?;

        if merchant.owner != Some(caller) {
            return Err(MarketplaceError::Unauthorized);
        }

        if merchant.name != name {
            let new_name_key = DataKey::MerchantName(name.clone());
            if env.storage().persistent().has(&new_name_key) {
                return Err(MarketplaceError::DuplicateMerchantName);
            }
            env.storage()
                .persistent()
                .remove(&DataKey::MerchantName(merchant.name.clone()));
            env.storage().persistent().set(&new_name_key, &merchant_id);
            merchant.name = name;
        }

        merchant.description = description;
        merchant.image_url = image_url;

        // Re-index CategoryIndex when category changes.
        if let Some(to_cat) = new_category {
            let from_cat = merchant.category.clone();
            if from_cat != to_cat {
                // Remove merchant_id from old category index (filter-rebuild shrinks the Vec).
                let old_cat_key = DataKey::CategoryIndex(from_cat.clone());
                let old_ids: Vec<u64> = env
                    .storage()
                    .persistent()
                    .get(&old_cat_key)
                    .unwrap_or_else(|| Vec::new(&env));
                let mut new_old_ids: Vec<u64> = Vec::new(&env);
                for id in old_ids.iter() {
                    if id != merchant_id {
                        new_old_ids.push_back(id);
                    }
                }
                env.storage().persistent().set(&old_cat_key, &new_old_ids);

                // Append merchant_id to new category index.
                let new_cat_key = DataKey::CategoryIndex(to_cat.clone());
                let mut new_ids: Vec<u64> = env
                    .storage()
                    .persistent()
                    .get(&new_cat_key)
                    .unwrap_or_else(|| Vec::new(&env));
                new_ids.push_back(merchant_id);
                env.storage().persistent().set(&new_cat_key, &new_ids);

                // Extend TTL for both category index keys.
                let storage = env.storage().persistent();
                storage.extend_ttl(
                    &old_cat_key,
                    PERSISTENT_BUMP_THRESHOLD,
                    PERSISTENT_BUMP_AMOUNT,
                );
                storage.extend_ttl(
                    &new_cat_key,
                    PERSISTENT_BUMP_THRESHOLD,
                    PERSISTENT_BUMP_AMOUNT,
                );

                // Update the merchant's stored category.
                merchant.category = to_cat.clone();

                // Emit category-changed event.
                env.events().publish(
                    (symbol_short!("mkplc"), symbol_short!("cat_chg")),
                    MerchantCategoryChangedEvent {
                        merchant_id,
                        from: from_cat,
                        to: to_cat,
                    },
                );
            }
        }

        merchant.updated_at = env.ledger().timestamp();

        env.storage()
            .persistent()
            .set(&DataKey::Merchant(merchant_id), &merchant);

        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("profile"),
                merchant_id,
            ),
            MerchantProfileUpdatedEvent {
                merchant_id,
                updated_fields: symbol_short!("profile"),
            },
        );

        Ok(())
    }

    pub fn update_metadata(
        env: Env,
        merchant_id: u64,
        caller: Address,
        new_metadata: Option<String>,
    ) -> Result<(), MarketplaceError> {
        caller.require_auth();

        let mut merchant = Self::get_merchant(env.clone(), merchant_id)?;
        Self::check_not_frozen_or_closed(&merchant)?;

        let admin = Self::get_admin(env.clone())?;
        let is_admin = caller == admin;
        let is_owner = merchant.owner == Some(caller.clone());

        if !is_admin && !is_owner {
            return Err(MarketplaceError::Unauthorized);
        }

        // Change-detection: Compare incoming metadata against stored value
        if merchant.metadata == new_metadata {
            // No change detected; skip write and event emission
            return Ok(());
        }

        let now = env.ledger().timestamp();

        if !is_admin {
            let cooldown = Self::get_metadata_cooldown(env.clone());
            let last_update: u64 = env
                .storage()
                .persistent()
                .get(&DataKey::LastMetadataUpdate(merchant_id))
                .unwrap_or(0);
            if cooldown > 0 && now < last_update.saturating_add(cooldown) {
                return Err(MarketplaceError::MetadataLockActive);
            }
        }

        merchant.metadata = new_metadata.clone();
        merchant.updated_at = now;

        env.storage()
            .persistent()
            .set(&DataKey::Merchant(merchant_id), &merchant);
        env.storage()
            .persistent()
            .set(&DataKey::LastMetadataUpdate(merchant_id), &now);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("meta"), merchant_id),
            MerchantMetadataUpdatedEvent {
                merchant_id,
                new_metadata,
            },
        );

        Ok(())
    }

    pub fn set_storefront_metadata(
        env: Env,
        merchant_id: u64,
        caller: Address,
        ipfs_cid: String,
        content_hash: BytesN<32>,
    ) -> Result<(), MarketplaceError> {
        storefront::set_storefront_metadata(env, merchant_id, caller, ipfs_cid, content_hash)
    }

    // --- Category Management ---

    pub fn propose_category(
        env: Env,
        merchant: Address,
        symbol: Symbol,
    ) -> Result<(), MarketplaceError> {
        merchant.require_auth();
        Self::get_verified_merchant_id(env.clone(), &merchant)?;

        let normalized = normalize_symbol(&env, &symbol);
        if false {
            return Err(MarketplaceError::InvalidCategory);
        }

        for category in Self::get_categories(env.clone()).iter() {
            if category.normalized == normalized || category.key == symbol {
                return Err(MarketplaceError::InvalidCategory);
            }
        }

        let now = env.ledger().timestamp();
        let key = DataKey::CategoryProposal(normalized.clone());
        if let Some(existing) = env.storage().instance().get::<_, CategoryProposal>(&key) {
            if existing.is_approved || now < existing.voting_deadline {
                return Err(MarketplaceError::InvalidCategory);
            }
        }

        let proposal = CategoryProposal {
            proposed_symbol: symbol,
            proposed_by: merchant,
            vote_count: 0,
            voting_deadline: now.saturating_add(CATEGORY_VOTING_PERIOD_SECS),
            is_approved: false,
        };
        env.storage().instance().set(&key, &proposal);
        Ok(())
    }

    pub fn vote_category(
        env: Env,
        merchant: Address,
        symbol: Symbol,
    ) -> Result<(), MarketplaceError> {
        merchant.require_auth();
        Self::get_verified_merchant_id(env.clone(), &merchant)?;
        let normalized = normalize_symbol(&env, &symbol);
        let key = DataKey::CategoryProposal(normalized.clone());
        let mut proposal: CategoryProposal = env
            .storage()
            .instance()
            .get(&key)
            .ok_or(MarketplaceError::CategoryProposalNotFound)?;

        if proposal.is_approved {
            return Err(MarketplaceError::InvalidCategory);
        }
        if env.ledger().timestamp() >= proposal.voting_deadline {
            return Err(MarketplaceError::CategoryProposalExpired);
        }

        let vote_key = DataKey::CategoryVote(
            normalized.clone(),
            merchant.clone(),
            proposal.voting_deadline,
        );
        if env.storage().persistent().has(&vote_key) {
            return Err(MarketplaceError::CategoryAlreadyVoted);
        }

        proposal.vote_count = proposal
            .vote_count
            .checked_add(1)
            .ok_or(MarketplaceError::InvalidParam)?;
        env.storage().persistent().set(&vote_key, &true);

        if proposal.vote_count >= CATEGORY_VOTE_THRESHOLD {
            let categories = Self::get_categories(env.clone());
            for category in categories.iter() {
                if category.normalized == normalized || category.key == proposal.proposed_symbol {
                    return Err(MarketplaceError::InvalidCategory);
                }
            }

            let mut categories = categories;
            categories.push_back(CategoryEntry {
                key: proposal.proposed_symbol.clone(),
                normalized: normalized.clone(),
                display: String::from_str(&env, "Community approved category"),
                added_at: env.ledger().timestamp(),
            });
            env.storage()
                .instance()
                .set(&DataKey::Categories, &categories);
            proposal.is_approved = true;
            env.events().publish(
                (symbol_short!("mkplc"), symbol_short!("cat_add")),
                CategoryAddedEvent {
                    key: proposal.proposed_symbol.clone(),
                    normalized,
                    added_by: proposal.proposed_by.clone(),
                },
            );
        }

        env.storage().instance().set(&key, &proposal);
        Ok(())
    }

    pub fn get_category_proposal(env: Env, symbol: Symbol) -> Option<CategoryProposal> {
        let normalized = normalize_symbol(&env, &symbol);
        env.storage()
            .instance()
            .get(&DataKey::CategoryProposal(normalized))
    }

    pub fn add_category(
        env: Env,
        admin: Address,
        category: CategoryEntry,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        let current_admin = Self::get_admin(env.clone())?;
        if admin != current_admin {
            return Err(MarketplaceError::Unauthorized);
        }

        let normalized = normalize_symbol(&env, &category.key);
        let mut categories = Self::get_categories(env.clone());
        for c in categories.iter() {
            if c.normalized == normalized || c.key == category.key {
                return Err(MarketplaceError::InvalidCategory);
            }
        }

        let now = env.ledger().timestamp();
        let entry = CategoryEntry {
            key: category.key.clone(),
            normalized: normalized.clone(),
            display: category.display,
            added_at: if category.added_at == 0 {
                now
            } else {
                category.added_at
            },
        };

        categories.push_back(entry);
        env.storage()
            .instance()
            .set(&DataKey::Categories, &categories);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("cat_add")),
            CategoryAddedEvent {
                key: category.key,
                normalized,
                added_by: admin,
            },
        );

        Ok(())
    }

    pub fn remove_category(
        env: Env,
        admin: Address,
        category: Symbol,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        let current_admin = Self::get_admin(env.clone())?;
        if admin != current_admin {
            return Err(MarketplaceError::Unauthorized);
        }

        let normalized = normalize_symbol(&env, &category);
        let categories = Self::get_categories(env.clone());
        let mut new_categories = Vec::new(&env);
        let mut found = false;

        for c in categories.iter() {
            if c.key == category || c.normalized == normalized {
                found = true;
            } else {
                new_categories.push_back(c);
            }
        }

        if !found {
            return Err(MarketplaceError::InvalidCategory);
        }

        env.storage()
            .instance()
            .set(&DataKey::Categories, &new_categories);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("cat_rem")),
            CategoryRemovedEvent {
                key: category,
                removed_by: admin,
            },
        );

        Ok(())
    }

    pub fn get_categories(env: Env) -> Vec<CategoryEntry> {
        env.storage()
            .instance()
            .get(&DataKey::Categories)
            .unwrap_or_else(|| Vec::new(&env))
    }

    fn get_verified_merchant_id(env: Env, merchant: &Address) -> Result<u64, MarketplaceError> {
        let merchant_id: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantOwner(merchant.clone()))
            .ok_or(MarketplaceError::MerchantNotFound)?;
        let record: Merchant = env
            .storage()
            .persistent()
            .get(&DataKey::Merchant(merchant_id))
            .ok_or(MarketplaceError::MerchantNotFound)?;
        if !record.verified || record.status != MerchantStatus::Verified {
            return Err(MarketplaceError::MerchantNotVerified);
        }
        Ok(merchant_id)
    }

    // --- Verification ---

    pub fn add_verifier(
        env: Env,
        admin: Address,
        verifier: Verifier,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        let current_admin = Self::get_admin(env.clone())?;
        if admin != current_admin {
            return Err(MarketplaceError::Unauthorized);
        }

        let mut verifiers = Self::get_verifiers(env.clone());
        for v in verifiers.iter() {
            if v.address == verifier.address {
                return Err(MarketplaceError::VerifierAlreadyExists);
            }
        }

        let verifier_count: u32 = verifiers.len();
        if verifier_count >= MAX_VERIFIERS {
            return Err(MarketplaceError::VerifierLimitReached);
        }

        verifiers.push_back(verifier.clone());
        env.storage()
            .instance()
            .set(&DataKey::Verifiers, &verifiers);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("v_add")),
            VerifierAddedEvent {
                verifier: verifier.address,
                label: verifier.label,
                added_by: admin,
            },
        );

        Ok(())
    }

    pub fn remove_verifier(
        env: Env,
        admin: Address,
        verifier: Address,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        let current_admin = Self::get_admin(env.clone())?;
        if admin != current_admin {
            return Err(MarketplaceError::Unauthorized);
        }

        let verifiers = Self::get_verifiers(env.clone());
        let mut new_verifiers = Vec::new(&env);
        let mut found = false;

        for v in verifiers.iter() {
            if v.address == verifier {
                found = true;
            } else {
                new_verifiers.push_back(v);
            }
        }

        if !found {
            return Err(MarketplaceError::VerifierNotFound);
        }

        // Ensure removal won't invalidate any existing merchant verification policies
        let new_verifiers_len: u32 = new_verifiers.len();
        let merchant_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantIds)
            .unwrap_or_else(|| Vec::new(&env));
        for id in merchant_ids.iter() {
            Self::bump_merchant_state(&env, id);
            let policy: Option<VerificationPolicy> = env
                .storage()
                .persistent()
                .get(&DataKey::VerificationPolicy(id));
            if let Some(p) = policy {
                if p.required > new_verifiers_len {
                    return Err(MarketplaceError::InvalidParam);
                }
            }
        }

        env.storage()
            .instance()
            .set(&DataKey::Verifiers, &new_verifiers);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("v_rem")),
            VerifierRemovedEvent {
                verifier,
                removed_by: admin,
            },
        );

        Ok(())
    }

    // --- Verification Policy Revalidation ---

    /// Returns the number of verifications currently collected for a merchant.
    fn get_merchant_verifications_count(env: &Env, merchant_id: u64) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::VerifiedCount(merchant_id))
            .unwrap_or(0)
    }

    /// Dynamic verification check: a merchant is verified iff the number of
    /// collected verifications meets or exceeds the policy's required count.
    /// This replaces reliance on the static `Merchant.verified` boolean.
    pub fn recheck_merchant_verification(
        env: Env,
        merchant_id: u64,
        policy: VerificationPolicy,
    ) -> bool {
        let current_verifications = Self::get_merchant_verifications_count(&env, merchant_id);
        current_verifications >= policy.required
    }

    /// Re-evaluates a merchant's verification status against the current
    /// policy. If the policy's `required` count exceeds the merchant's
    /// collected verifications, the merchant is downgraded from `Verified`
    /// to `Registered` — unless it is still within the 30-day grace period
    /// that begins when the policy threshold was raised.
    ///
    /// Returns `true` when the merchant is (still) considered verified under
    /// the current policy, `false` otherwise.
    pub fn revalidate_merchant_status(
        env: Env,
        merchant_id: u64,
    ) -> Result<bool, MarketplaceError> {
        let mut merchant = Self::get_merchant(env.clone(), merchant_id)?;
        Self::check_not_frozen_or_closed(&merchant)?;

        let policy: VerificationPolicy = env
            .storage()
            .persistent()
            .get(&DataKey::VerificationPolicy(merchant_id))
            .unwrap_or(VerificationPolicy {
                required: 1,
                max_verifications: MAX_REQUIRED_VERIFICATIONS,
            });

        let meets_policy =
            Self::recheck_merchant_verification(env.clone(), merchant_id, policy.clone());

        if meets_policy {
            if !merchant.verified {
                merchant.verified = true;
                merchant.status = MerchantStatus::Verified;
                merchant.updated_at = env.ledger().timestamp();
                env.storage()
                    .persistent()
                    .set(&DataKey::Merchant(merchant_id), &merchant);
            }
            return Ok(true);
        }

        // Merchant does not meet the current policy. Check grace period.
        let raised_at: Option<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::PolicyRaisedAt(merchant_id));

        let now = env.ledger().timestamp();
        let within_grace = match raised_at {
            Some(ts) => now < ts.saturating_add(VERIFICATION_POLICY_GRACE_PERIOD_SECS),
            None => false,
        };

        if within_grace {
            // Keep verified status during grace period.
            return Ok(merchant.verified);
        }

        // Grace period expired (or never set): downgrade.
        if merchant.verified || merchant.status == MerchantStatus::Verified {
            merchant.verified = false;
            if merchant.status == MerchantStatus::Verified {
                merchant.status = MerchantStatus::Registered;
            }
            merchant.updated_at = now;
            env.storage()
                .persistent()
                .set(&DataKey::Merchant(merchant_id), &merchant);
        }

        Ok(false)
    }

    /// Admin-only: raise the required verification count for a merchant's
    /// policy. Records the timestamp so the 30-day grace period applies to
    /// pre-existing merchants that were verified under the old policy.
    pub fn set_verification_policy(
        env: Env,
        admin: Address,
        merchant_id: u64,
        required: u32,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        if admin != Self::get_admin(env.clone())? {
            return Err(MarketplaceError::Unauthorized);
        }
        if required > MAX_REQUIRED_VERIFICATIONS {
            return Err(MarketplaceError::InvalidParam);
        }

        let verifiers_len: u32 = Self::get_verifiers(env.clone()).len();
        if verifiers_len > 0 && required > verifiers_len {
            return Err(MarketplaceError::InvalidParam);
        }

        let mut policy: VerificationPolicy = env
            .storage()
            .persistent()
            .get(&DataKey::VerificationPolicy(merchant_id))
            .unwrap_or(VerificationPolicy {
                required: 1,
                max_verifications: verifiers_len.min(MAX_REQUIRED_VERIFICATIONS),
            });

        let previous_required = policy.required;
        if required > previous_required {
            // Threshold increase: start grace period for pre-existing merchants.
            let now = env.ledger().timestamp();
            env.storage()
                .persistent()
                .set(&DataKey::PolicyRaisedAt(merchant_id), &now);
            env.storage().persistent().set(
                &DataKey::PolicyPreviousRequired(merchant_id),
                &previous_required,
            );
            env.storage().persistent().extend_ttl(
                &DataKey::PolicyRaisedAt(merchant_id),
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );
        }

        policy.required = required;
        if policy.max_verifications < required {
            policy.max_verifications = required;
        }
        env.storage()
            .persistent()
            .set(&DataKey::VerificationPolicy(merchant_id), &policy);
        env.storage().persistent().extend_ttl(
            &DataKey::VerificationPolicy(merchant_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        Ok(())
    }

    pub fn verify_merchant(
        env: Env,
        merchant_id: u64,
        verifier: Address,
    ) -> Result<(), MarketplaceError> {
        verifier.require_auth();

        let verifiers = Self::get_verifiers(env.clone());
        let mut is_registered_verifier = false;
        for v in verifiers.iter() {
            if v.address == verifier {
                is_registered_verifier = true;
                break;
            }
        }

        if !is_registered_verifier {
            return Err(MarketplaceError::Unauthorized);
        }

        let mut merchant = Self::get_merchant(env.clone(), merchant_id)?;
        Self::check_not_frozen_or_closed(&merchant)?;

        if merchant.verified {
            return Err(MarketplaceError::AlreadyVerified);
        }

        let verifier_key = DataKey::MerchantVerifier(merchant_id, verifier.clone());
        if env.storage().persistent().has(&verifier_key) {
            return Err(MarketplaceError::AlreadyVerified);
        }

        env.storage().persistent().set(&verifier_key, &true);

        let mut verifier_list: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantVerifierList(merchant_id))
            .unwrap_or_else(|| Vec::new(&env));
        verifier_list.push_back(verifier.clone());
        env.storage()
            .persistent()
            .set(&DataKey::MerchantVerifierList(merchant_id), &verifier_list);

        let current_count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::VerifiedCount(merchant_id))
            .unwrap_or(0);
        let new_count = current_count
            .checked_add(1)
            .ok_or(MarketplaceError::VerificationCountOverflow)?;
        env.storage()
            .persistent()
            .set(&DataKey::VerifiedCount(merchant_id), &new_count);

        let required: u32 = {
            let policy: Option<VerificationPolicy> = env
                .storage()
                .persistent()
                .get(&DataKey::VerificationPolicy(merchant_id));
            policy.map(|p| p.required).unwrap_or(1)
        };

        if new_count >= required {
            merchant.verified = true;
            merchant.status = MerchantStatus::Verified;
        }
        merchant.updated_at = env.ledger().timestamp();

        env.storage()
            .persistent()
            .set(&DataKey::Merchant(merchant_id), &merchant);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("verif"), merchant_id),
            MerchantVerifiedEvent {
                merchant_id,
                verifier,
                verified: merchant.verified,
            },
        );

        Ok(())
    }

    pub fn revoke_verification(
        env: Env,
        admin: Address,
        merchant_id: u64,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        let current_admin = Self::get_admin(env.clone())?;
        if admin != current_admin {
            return Err(MarketplaceError::Unauthorized);
        }

        let mut merchant = Self::get_merchant(env.clone(), merchant_id)?;
        Self::check_not_frozen_or_closed(&merchant)?;

        merchant.verified = false;
        if merchant.status == MerchantStatus::Verified {
            merchant.status = MerchantStatus::Registered;
        }
        merchant.updated_at = env.ledger().timestamp();

        // Clear collected verifications
        env.storage()
            .persistent()
            .set(&DataKey::VerifiedCount(merchant_id), &0u32);

        let verifier_list: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantVerifierList(merchant_id))
            .unwrap_or_else(|| Vec::new(&env));
        for v in verifier_list.iter() {
            env.storage()
                .persistent()
                .remove(&DataKey::MerchantVerifier(merchant_id, v));
        }
        env.storage().persistent().set(
            &DataKey::MerchantVerifierList(merchant_id),
            &Vec::<Address>::new(&env),
        );

        env.storage()
            .persistent()
            .set(&DataKey::Merchant(merchant_id), &merchant);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("v_rev"), merchant_id),
            MerchantVerificationRevokedEvent {
                merchant_id,
                revoked_by: admin,
            },
        );

        Ok(())
    }

    // --- Discovery & Query ---

    pub fn get_merchant_stats(env: Env) -> MerchantStats {
        env.storage()
            .instance()
            .get(&DataKey::MerchantStats)
            .unwrap_or(MerchantStats {
                total: 0,
                active: 0,
                suspended: 0,
                closed: 0,
            })
    }

    pub fn get_merchant(env: Env, merchant_id: u64) -> Result<Merchant, MarketplaceError> {
        let merchant: Merchant = env
            .storage()
            .persistent()
            .get(&DataKey::Merchant(merchant_id))
            .ok_or(MarketplaceError::MerchantNotFound)?;

        Self::bump_merchant_state(&env, merchant_id);

        Ok(merchant)
    }

    /// Validate that a merchant's category matches one of the authorized categories.
    ///
    /// This is used by the escrow contract to enforce merchant category restrictions
    /// during spend operations. The merchant must be active and verified.
    ///
    /// # Arguments
    /// * `merchant_id` - The ID of the merchant to validate
    /// * `authorized_categories` - List of category symbols that are allowed
    ///
    /// # Returns
    /// * `Ok(())` if the merchant's category is in the authorized list
    /// * `Err(MarketplaceError::MerchantCategoryNotAllowed)` if category doesn't match
    /// * `Err(MarketplaceError::MerchantNotFound)` if merchant doesn't exist
    /// * `Err(MarketplaceError::MerchantFrozen)` if merchant is suspended
    /// * `Err(MarketplaceError::MerchantClosed)` if merchant is closed
    /// * `Err(MarketplaceError::MerchantBanned)` if merchant is banned
    /// * `Err(MarketplaceError::MerchantInactive)` if the storefront is winding down
    pub fn validate_merchant_category(
        env: Env,
        merchant_id: u64,
        authorized_categories: soroban_sdk::Vec<Symbol>,
    ) -> Result<(), MarketplaceError> {
        let merchant = Self::get_merchant(env.clone(), merchant_id)?;

        // Check merchant status - must be active (Verified or Registered)
        match merchant.status {
            MerchantStatus::Verified | MerchantStatus::Registered => {
            }
            MerchantStatus::Suspended => return Err(MarketplaceError::MerchantFrozen),
            MerchantStatus::Closed => return Err(MarketplaceError::MerchantClosed),
            MerchantStatus::Banned => return Err(MarketplaceError::MerchantBanned),
            MerchantStatus::Inactive => return Err(MarketplaceError::MerchantInactive),
            MerchantStatus::All => {
                // This is a sentinel value, should never reach here
                return Err(MarketplaceError::InvalidParam);
            }
            MerchantStatus::Maintenance => return Err(MarketplaceError::MaintenanceActive),
        }

        // Check if merchant's category is in the authorized list
        let mut authorized = false;
        for cat in authorized_categories.iter() {
            if cat == merchant.category || normalize_symbol(&env, &cat) == merchant.category {
                authorized = true;
                break;
            }
        }

        if !authorized {
            return Err(MarketplaceError::MerchantCategoryNotAllowed);
        }

        Ok(())
    }

    /// Get merchant category by ID (convenience function for cross-contract calls).
    pub fn get_merchant_category(env: Env, merchant_id: u64) -> Result<Symbol, MarketplaceError> {
        let merchant = Self::get_merchant(env, merchant_id)?;
        Ok(merchant.category)
    }

    /// Get merchant ID by owner address.
    ///
    /// This is used by the escrow contract to validate merchant categories.
    /// Returns the merchant ID if found, or 0 if no merchant is registered for this owner.
    pub fn get_merchant_id_by_owner(env: Env, owner: Address) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::MerchantOwner(owner))
            .unwrap_or(0)
    }

    /// Validate a merchant's category against authorized categories.
    ///
    /// Used by escrow contract for spend validation.
    /// Returns Ok(()) if valid, or error if not allowed.
    pub fn validate_cat_by_seller(
        env: Env,
        seller: Address,
        authorized_categories: soroban_sdk::Vec<Symbol>,
    ) -> Result<(), MarketplaceError> {
        // First get the merchant ID by owner
        let merchant_id = Self::get_merchant_id_by_owner(env.clone(), seller);

        // If no merchant found, allow (non-marketplace seller)
        if merchant_id == 0 {
            return Ok(());
        }

        // Now validate the category
        Self::validate_merchant_category(env, merchant_id, authorized_categories)
    }

    pub fn get_merchant_view_detailed(
        env: Env,
        merchant_id: u64,
    ) -> Result<MerchantViewDetailed, MarketplaceError> {
        let merchant = Self::get_merchant(env.clone(), merchant_id)?;

        let reputation_contract = merchant.reputation.clone().or_else(|| {
            env.storage()
                .instance()
                .get(&DataKey::GlobalReputationContract)
        });

        let reputation = if let Some(rep_addr) = reputation_contract {
            if let Some(owner) = merchant.owner.clone() {
                let args = soroban_sdk::vec![&env, owner.to_val()];
                let call_result = env.try_invoke_contract::<ExternalReputationScore, InvokeError>(
                    &rep_addr,
                    &Symbol::new(&env, "get_reputation"),
                    args,
                );
                match call_result {
                    Ok(Ok(rep)) => ReputationResolution::Available(rep.score, rep.last_updated),
                    _ => ReputationResolution::CallFailed,
                }
            } else {
                ReputationResolution::NotConfigured
            }
        } else {
            ReputationResolution::NotConfigured
        };

        let reputation_score = match &reputation {
            ReputationResolution::Available(score, _) => Some(*score),
            _ => None,
        };

        let view = MerchantView {
            id: merchant.id,
            name: merchant.name,
            category: merchant.category,
            commission_rate_bps: merchant.commission_rate_bps,
            verified: merchant.verified,
            status: merchant.status,
            reputation_score,
        };

        Ok(MerchantViewDetailed { view, reputation })
    }

    fn bump_merchant_state(env: &Env, id: u64) {
        let storage = env.storage().persistent();
        let merchant: Merchant = match storage.get(&DataKey::Merchant(id)) {
            Some(merchant) => merchant,
            None => return,
        };
        let keys = [
            DataKey::Merchant(id),
            DataKey::MerchantName(merchant.name.clone()),
            DataKey::VerifiedCount(id),
            DataKey::VerificationPolicy(id),
            DataKey::MerchantVerifierList(id),
            DataKey::LastMetadataUpdate(id),
        ];
        for key in keys {
            if storage.has(&key) {
                storage.extend_ttl(&key, PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
            }
        }
    }

    // (get_merchant_view_detailed body above; this duplicate removed)

    pub fn get_merchant_view(env: Env, merchant_id: u64) -> Result<MerchantView, MarketplaceError> {
        let detailed = Self::get_merchant_view_detailed(env, merchant_id)?;
        Ok(detailed.view)
    }

    pub fn get_merchant_operational_view(
        env: Env,
        merchant_id: u64,
    ) -> Result<MerchantOperationalView, MarketplaceError> {
        let merchant = Self::get_merchant(env.clone(), merchant_id)?;

        let effective = merchant.verified && merchant.status == MerchantStatus::Verified;
        Ok(MerchantOperationalView {
            id: merchant.id,
            name: merchant.name,
            status: merchant.status,
            verified: merchant.verified,
            effective,
        })
    }

    pub fn get_merchants(
        env: Env,
        offset: u32,
        limit: u32,
    ) -> Result<DiscoveryPage, MarketplaceError> {
        // Return all merchants regardless of status (backward compatible)
        Self::get_merchants_filtered(env, offset, limit, None)
    }

    pub fn get_merchants_by_status(
        env: Env,
        status: MerchantStatus,
        offset: u32,
        limit: u32,
    ) -> Result<DiscoveryPage, MarketplaceError> {
        // Filter merchants by specific status
        Self::get_merchants_filtered(env, offset, limit, Some(status))
    }

    fn get_merchants_filtered(
        env: Env,
        offset: u32,
        limit: u32,
        status_filter: Option<MerchantStatus>,
    ) -> Result<DiscoveryPage, MarketplaceError> {
        let limit = limit.min(MAX_PAGE_LIMIT);

        // Probe id-scoped `DataKey::Merchant(u64)` entries directly instead of
        // deserializing the whole `MerchantIds` index (issue #120). Merchant ids
        // are assigned monotonically from `NextMerchantId`, so the id space is
        // contiguous and each id in `1..NextMerchantId` maps to a record.
        let next_merchant_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextMerchantId)
            .unwrap_or(1);

        // Collect ids that pass the status filter.
        let mut filtered_ids = Vec::new(&env);
        let mut id = 1u64;
        while id < next_merchant_id {
            if let Ok(merchant) = Self::get_merchant(env.clone(), id) {
                let include = match status_filter {
                    Some(filter) => merchant.status == filter,
                    None => true,
                };
                if include {
                    filtered_ids.push_back(id);
                }
            }
            id += 1;
        }

        let total = filtered_ids.len();
        if offset >= total || limit == 0 {
            return Ok(DiscoveryPage {
                items: Vec::new(&env),
                total: total as u32,
                next_offset: None,
                next_cursor: None,
            });
        }

        let end = offset.saturating_add(limit).min(total);
        let mut items = Vec::new(&env);
        let mut i = offset;
        while i < end {
            let mid = filtered_ids.get(i).unwrap();
            let view = Self::get_merchant_view(env.clone(), mid)?;
            items.push_back(view);
            i += 1;
        }

        let next_offset = if end < total { Some(end) } else { None };
        let next_cursor = if end < total {
            Some(filtered_ids.get(end.saturating_sub(1)).unwrap())
        } else {
            None
        };

        Ok(DiscoveryPage {
            items,
            total: total as u32,
            next_offset,
            next_cursor,
        })
    }

    pub fn get_merchants_by_category(
        env: Env,
        category: Symbol,
        offset: u32,
        limit: u32,
    ) -> Result<DiscoveryPage, MarketplaceError> {
        // Return all merchants in category regardless of status (backward compatible)
        Self::get_merchants_by_category_filtered(env, category, offset, limit, None)
    }

    pub fn get_merchants_by_category_status(
        env: Env,
        category: Symbol,
        status: MerchantStatus,
        offset: u32,
        limit: u32,
    ) -> Result<DiscoveryPage, MarketplaceError> {
        // Filter merchants by category and specific status
        Self::get_merchants_by_category_filtered(env, category, offset, limit, Some(status))
    }

    fn get_merchants_by_category_filtered(
        env: Env,
        category: Symbol,
        offset: u32,
        limit: u32,
        status_filter: Option<MerchantStatus>,
    ) -> Result<DiscoveryPage, MarketplaceError> {
        let limit = limit.min(MAX_PAGE_LIMIT);
        let normalized = normalize_symbol(&env, &category);
        let mut cat_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::CategoryIndex(normalized.clone()))
            .unwrap_or_else(|| Vec::new(&env));

        // Backward compatibility: If no merchants found under normalized key,
        // and raw category differs from normalized, check raw category index.
        if cat_ids.len() == 0 && normalized != category {
            if let Some(legacy_ids) = env
                .storage()
                .persistent()
                .get(&DataKey::CategoryIndex(category))
            {
                cat_ids = legacy_ids;
            }
        }

        // Filter merchants by status if provided
        let mut filtered_ids = Vec::new(&env);
        for id in cat_ids.iter() {
            if let Ok(merchant) = Self::get_merchant(env.clone(), id) {
                if let Some(status) = status_filter {
                    if merchant.status == status {
                        filtered_ids.push_back(id);
                    }
                } else {
                    // No filter: include all
                    filtered_ids.push_back(id);
                }
            }
        }

        let total = filtered_ids.len();
        if offset >= total || limit == 0 {
            return Ok(DiscoveryPage {
                items: Vec::new(&env),
                total,
                next_offset: None,
                next_cursor: None,
            });
        }

        let end = offset.saturating_add(limit).min(total);
        let mut items = Vec::new(&env);
        let mut i = offset;
        while i < end {
            let id = filtered_ids.get(i).unwrap();
            let view = Self::get_merchant_view(env.clone(), id)?;
            items.push_back(view);
            i += 1;
        }

        let next_offset = if end < total { Some(end) } else { None };
        // Expose the last returned id so callers can pivot to cursor pagination.
        let next_cursor = if end < total {
            Some(filtered_ids.get(end.saturating_sub(1)).unwrap())
        } else {
            None
        };

        Ok(DiscoveryPage {
            items,
            total,
            next_offset,
            next_cursor,
        })
    }

    pub fn get_merchants_cursor(
        env: Env,
        cursor: MerchantCursor,
    ) -> Result<DiscoveryPage, MarketplaceError> {
        let limit = cursor.limit.min(MAX_PAGE_LIMIT);
        let status_filter = cursor.status;

        // Iterate id-scoped `DataKey::Merchant(u64)` entries from `after_id`
        // without deserializing the whole `MerchantIds` index (issue #120). The
        // loop reads at most `limit` merchant views, so cost is bounded by the
        // page size (plus cheap key probes for skipped ids), not by the registry
        // size, and deep pages never pay for the full index.
        let next_merchant_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextMerchantId)
            .unwrap_or(1);

        let mut items = Vec::new(&env);
        let mut last_id = cursor.after_id;
        let mut id = cursor.after_id.saturating_add(1);

        while items.len() < limit && id < next_merchant_id {
            if env.storage().persistent().has(&DataKey::Merchant(id)) {
                let merchant = Self::get_merchant(env.clone(), id)?;
                let matches =
                    status_filter == MerchantStatus::All || merchant.status == status_filter;
                if matches {
                    let view = Self::get_merchant_view(env.clone(), id)?;
                    items.push_back(view);
                    last_id = id;
                }
            }
            id += 1;
        }

        let next_cursor = if items.len() != 0 && id < next_merchant_id {
            Some(last_id)
        } else {
            None
        };

        Ok(DiscoveryPage {
            items,
            total: 0,
            next_offset: None,
            next_cursor,
        })
    }

    pub fn get_merchants_by_category_cursor(
        env: Env,
        category: Symbol,
        after_id: u64,
        limit: u32,
    ) -> Result<DiscoveryPage, MarketplaceError> {
        let limit = limit.min(MAX_PAGE_LIMIT);

        // Category membership is already scoped by `CategoryIndex(category)`, so
        // page that id list from `after_id` without scanning the global registry.
        let cat_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::CategoryIndex(category))
            .unwrap_or_else(|| Vec::new(&env));

        let mut items = Vec::new(&env);
        let mut last_id = after_id;
        let n = cat_ids.len();

        // Skip ids already seen by the caller (after_id is exclusive).
        let mut i = 0u32;
        while i < n && cat_ids.get(i).unwrap() <= after_id {
            i += 1;
        }

        while i < n && items.len() < limit {
            let id = cat_ids.get(i).unwrap();
            let view = Self::get_merchant_view(env.clone(), id)?;
            items.push_back(view);
            last_id = id;
            i += 1;
        }

        let next_cursor = if i < n { Some(last_id) } else { None };

        Ok(DiscoveryPage {
            items,
            total: 0,
            next_offset: None,
            next_cursor,
        })
    }

    // --- Commission ---

    pub fn set_merchant_commission(
        env: Env,
        merchant_id: u64,
        caller: Address,
        commission_bps: u32,
    ) -> Result<(), MarketplaceError> {
        caller.require_auth();

        if commission_bps > MAX_COMMISSION_BPS {
            return Err(MarketplaceError::InvalidCommissionBps);
        }

        let mut merchant = Self::get_merchant(env.clone(), merchant_id)?;
        Self::check_not_frozen_or_closed(&merchant)?;

        let admin = Self::get_admin(env.clone())?;
        let is_admin = caller == admin;
        let is_owner = merchant.owner == Some(caller.clone());

        if !is_admin && !is_owner {
            return Err(MarketplaceError::Unauthorized);
        }

        merchant.commission_rate_bps = commission_bps;
        merchant.updated_at = env.ledger().timestamp();

        env.storage()
            .persistent()
            .set(&DataKey::Merchant(merchant_id), &merchant);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("comm"), merchant_id),
            MerchantCommissionSetEvent {
                merchant_id,
                commission_bps,
                set_by: caller,
            },
        );

        Ok(())
    }

    pub fn get_commission(env: Env, merchant_id: u64) -> Result<u32, MarketplaceError> {
        let merchant = Self::get_merchant(env, merchant_id)?;
        Ok(merchant.commission_rate_bps)
    }

    // --- Operational view (issue #561) ---

    /// Returns a consolidated operational snapshot of `merchant_id` in a
    /// single read call: lifecycle status, verification tier, effective fee
    /// rate, active bond balance and dispute count.
    ///
    /// Returns [`MarketplaceError::MerchantNotFound`] for unregistered ids.
    pub fn get_merchant_operational_view(
        env: Env,
        merchant_id: u64,
    ) -> Result<MerchantOperationalView, MarketplaceError> {
        let merchant = Self::get_merchant(env.clone(), merchant_id)?;

        let tier = match merchant.status {
            MerchantStatus::Closed | MerchantStatus::Banned => MerchantTier::Terminated,
            MerchantStatus::Suspended => MerchantTier::Suspended,
            _ if merchant.verified => MerchantTier::Verified,
            _ => MerchantTier::Unverified,
        };

        let effective_fee_bps = merchant.commission_rate_bps.min(MAX_COMMISSION_BPS);

        let bond_balance: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantBond(merchant_id))
            .unwrap_or(0i128);

        let dispute_count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantDisputeCount(merchant_id))
            .unwrap_or(0u32);

        Ok(MerchantOperationalView {
            status: merchant.status,
            tier,
            verified: merchant.verified,
            effective_fee_bps,
            bond_balance,
            dispute_count,
        })
    }

    // --- Treasury split routing (issue #562) ---

    /// Stores the treasury split configuration. Admin only.
    pub fn set_treasury_split_config(
        env: Env,
        admin: Address,
        config: TreasurySplitConfig,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        if admin != Self::get_admin(env.clone())? {
            return Err(MarketplaceError::Unauthorized);
        }
        if config
            .dao_bps
            .checked_add(config.arbiter_bps)
            .and_then(|v| v.checked_add(config.insurance_bps))
            != Some(10_000)
        {
            return Err(MarketplaceError::InvalidTreasurySplit);
        }
        env.storage()
            .instance()
            .set(&DataKey::TreasurySplit, &config);
        Ok(())
    }

    /// Returns the configured treasury split, if any.
    pub fn get_treasury_split_config(env: Env) -> Option<TreasurySplitConfig> {
        env.storage().instance().get(&DataKey::TreasurySplit)
    }

    /// Routes `amount` of `token` held by the contract across the configured
    /// DAO / arbiter / insurance pools. The rounding remainder is assigned to
    /// the insurance pool so no tokens are left behind.
    pub fn route_fee_disbursements(
        env: Env,
        token: Address,
        amount: i128,
    ) -> Result<(), MarketplaceError> {
        if amount <= 0 {
            return Err(MarketplaceError::InvalidParam);
        }
        let config: TreasurySplitConfig = env
            .storage()
            .instance()
            .get(&DataKey::TreasurySplit)
            .ok_or(MarketplaceError::TreasurySplitNotConfigured)?;

        if config
            .dao_bps
            .checked_add(config.arbiter_bps)
            .and_then(|v| v.checked_add(config.insurance_bps))
            != Some(10_000)
        {
            return Err(MarketplaceError::InvalidTreasurySplit);
        }

        let dao_share = amount
            .checked_mul(config.dao_bps as i128)
            .ok_or(MarketplaceError::InvalidParam)?
            / 10_000;
        let arbiter_share = amount
            .checked_mul(config.arbiter_bps as i128)
            .ok_or(MarketplaceError::InvalidParam)?
            / 10_000;
        // Remainder (including rounding dust) goes to the insurance pool.
        let insurance_share = amount - dao_share - arbiter_share;

        let client = soroban_sdk::token::Client::new(&env, &token);
        let from = env.current_contract_address();
        if dao_share > 0 {
            client.transfer(&from, &config.dao_treasury, &dao_share);
        }
        if arbiter_share > 0 {
            client.transfer(&from, &config.arbiter_pool, &arbiter_share);
        }
        if insurance_share > 0 {
            client.transfer(&from, &config.insurance_pool, &insurance_share);
        }

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("fee_route")),
            (token, amount, dao_share, arbiter_share, insurance_share),
        );

        Ok(())
    }

    // --- Liquidation reserve (issue #563) ---

    /// Locks a 10% liquidation reserve for a high-value settlement.
    ///
    /// `settlement_amount` is the gross amount owed to the merchant; the
    /// reserve is deducted from it and held by the contract until
    /// `unlock_ledger`. Returns the reserve amount (0 when the order is below
    /// [`HIGH_VALUE_THRESHOLD`]).
    pub fn lock_reserve(
        env: Env,
        merchant: Address,
        order_id: u64,
        settlement_amount: i128,
    ) -> Result<i128, MarketplaceError> {
        if settlement_amount <= 0 {
            return Err(MarketplaceError::InvalidParam);
        }
        if settlement_amount < HIGH_VALUE_THRESHOLD {
            return Ok(0);
        }
        let reserve_amount = settlement_amount
            .checked_mul(RESERVE_BPS as i128)
            .ok_or(MarketplaceError::InvalidParam)?
            / 10_000;
        if reserve_amount <= 0 {
            return Ok(0);
        }
        let unlock_ledger = env.ledger().sequence() + RESERVE_LOCK_LEDGERS;
        let reserve = LiquidationReserve {
            merchant: merchant.clone(),
            order_id,
            reserve_amount,
            unlock_ledger,
        };
        env.storage()
            .persistent()
            .set(&DataKey::LiquidationReserve(order_id), &reserve);
        env.storage().persistent().extend_ttl(
            &DataKey::LiquidationReserve(order_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("res_lock"), order_id),
            (merchant, reserve_amount, unlock_ledger),
        );

        Ok(reserve_amount)
    }

    /// Returns the liquidation reserve for `order_id`, if any.
    pub fn get_liquidation_reserve(env: Env, order_id: u64) -> Option<LiquidationReserve> {
        env.storage()
            .persistent()
            .get(&DataKey::LiquidationReserve(order_id))
    }

    /// Releases a matured reserve back to the merchant. Rejected before
    /// `unlock_ledger`.
    pub fn withdraw_reserve(
        env: Env,
        merchant: Address,
        order_id: u64,
        token: Address,
    ) -> Result<i128, MarketplaceError> {
        merchant.require_auth();
        let reserve: LiquidationReserve = env
            .storage()
            .persistent()
            .get(&DataKey::LiquidationReserve(order_id))
            .ok_or(MarketplaceError::ReserveNotFound)?;
        if reserve.merchant != merchant {
            return Err(MarketplaceError::Unauthorized);
        }
        if env.ledger().sequence() < reserve.unlock_ledger {
            return Err(MarketplaceError::ReserveLocked);
        }

        env.storage()
            .persistent()
            .remove(&DataKey::LiquidationReserve(order_id));

        let client = soroban_sdk::token::Client::new(&env, &token);
        client.transfer(
            &env.current_contract_address(),
            &merchant,
            &reserve.reserve_amount,
        );

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("res_wd"), order_id),
            (merchant, reserve.reserve_amount),
        );

        Ok(reserve.reserve_amount)
    }

    /// Arbiter-only: liquidates a reserve to refund the buyer in a dispute.
    pub fn liquidate_reserve(
        env: Env,
        arbiter: Address,
        order_id: u64,
        buyer: Address,
        token: Address,
    ) -> Result<i128, MarketplaceError> {
        arbiter.require_auth();
        if !Self::is_admin_or_risk_oracle(&env, &arbiter)? {
            return Err(MarketplaceError::Unauthorized);
        }
        let reserve: LiquidationReserve = env
            .storage()
            .persistent()
            .get(&DataKey::LiquidationReserve(order_id))
            .ok_or(MarketplaceError::ReserveNotFound)?;

        env.storage()
            .persistent()
            .remove(&DataKey::LiquidationReserve(order_id));

        let client = soroban_sdk::token::Client::new(&env, &token);
        client.transfer(
            &env.current_contract_address(),
            &buyer,
            &reserve.reserve_amount,
        );

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("res_liq"), order_id),
            (buyer, reserve.reserve_amount),
        );

        Ok(reserve.reserve_amount)
    }

    // --- Moderation / Security ---

    pub fn suspend_merchant(
        env: Env,
        admin: Address,
        merchant_id: u64,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        if !Self::is_admin_or_risk_oracle(&env, &admin)? {
            return Err(MarketplaceError::Unauthorized);
        }

        let mut merchant = Self::get_merchant(env.clone(), merchant_id)?;
        if matches!(merchant.status, MerchantStatus::Closed) {
            return Err(MarketplaceError::MerchantClosed);
        }
        if matches!(merchant.status, MerchantStatus::Banned) {
            return Err(MarketplaceError::MerchantBanned);
        }

        if merchant.status == MerchantStatus::Inactive {
            // Already barred from new orders; keep the flag so an enforcement
            // action cannot hand the storefront back before the pending
            // escrows settle.
            return Ok(());
        }

        let prev_status = merchant.status;
        if prev_status != MerchantStatus::Suspended {
            let mut stats = Self::get_merchant_stats(env.clone());
            stats.active = stats.active.saturating_sub(1);
            stats.suspended = stats.suspended.saturating_add(1);
            env.storage()
                .instance()
                .set(&DataKey::MerchantStats, &stats);
        }

        merchant.status = MerchantStatus::Suspended;
        merchant.updated_at = env.ledger().timestamp();

        if let Some(owner) = merchant.owner.clone() {
            env.storage()
                .persistent()
                .set(&DataKey::MerchantOwner(owner), &merchant_id);
        }
        env.storage()
            .persistent()
            .set(&DataKey::Merchant(merchant_id), &merchant);

        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("suspend"),
                merchant_id,
            ),
            MerchantSuspendedEvent {
                merchant_id,
                suspended_by: admin,
            },
        );

        Ok(())
    }

    pub fn unsuspend_merchant(
        env: Env,
        admin: Address,
        merchant_id: u64,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        let current_admin = Self::get_admin(env.clone())?;
        if admin != current_admin {
            return Err(MarketplaceError::Unauthorized);
        }

        let mut merchant = Self::get_merchant(env.clone(), merchant_id)?;
        if matches!(merchant.status, MerchantStatus::Closed) {
            return Err(MarketplaceError::MerchantClosed);
        }
        if matches!(merchant.status, MerchantStatus::Banned) {
            return Err(MarketplaceError::MerchantBanned);
        }
        if matches!(merchant.status, MerchantStatus::Inactive) {
            return Err(MarketplaceError::MerchantInactive);
        }

        let prev_status = merchant.status;
        if prev_status == MerchantStatus::Suspended {
            let mut stats = Self::get_merchant_stats(env.clone());
            stats.suspended = stats.suspended.saturating_sub(1);
            stats.active = stats.active.saturating_add(1);
            env.storage()
                .instance()
                .set(&DataKey::MerchantStats, &stats);
        }

        merchant.status = if merchant.verified {
            MerchantStatus::Verified
        } else {
            MerchantStatus::Registered
        };
        merchant.updated_at = env.ledger().timestamp();

        if let Some(owner) = merchant.owner.clone() {
            env.storage()
                .persistent()
                .set(&DataKey::MerchantOwner(owner), &merchant_id);
        }
        env.storage()
            .persistent()
            .set(&DataKey::Merchant(merchant_id), &merchant);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("unsusp"), merchant_id),
            MerchantUnsuspendedEvent {
                merchant_id,
                unsuspended_by: admin,
            },
        );

        Ok(())
    }

    /// Permanently ban a merchant from trading. The admin or configured risk
    /// oracle may trigger this emergency action; bans cannot be reversed by
    /// `unsuspend_merchant`.
    pub fn ban_merchant(
        env: Env,
        caller: Address,
        merchant_id: u64,
    ) -> Result<(), MarketplaceError> {
        caller.require_auth();
        if !Self::is_admin_or_risk_oracle(&env, &caller)? {
            return Err(MarketplaceError::Unauthorized);
        }
        let mut merchant = Self::get_merchant(env.clone(), merchant_id)?;
        if merchant.status == MerchantStatus::Closed {
            return Err(MarketplaceError::MerchantClosed);
        }
        if merchant.status != MerchantStatus::Banned {
            let mut stats = Self::get_merchant_stats(env.clone());
            if merchant.status == MerchantStatus::Suspended {
                stats.suspended = stats.suspended.saturating_sub(1);
            } else {
                if merchant.status != MerchantStatus::Inactive {
                    stats.active = stats.active.saturating_sub(1);
                }
                stats.suspended = stats.suspended.saturating_add(1);
            }
            env.storage()
                .instance()
                .set(&DataKey::MerchantStats, &stats);
        }
        merchant.status = MerchantStatus::Banned;
        merchant.updated_at = env.ledger().timestamp();
        if let Some(owner) = merchant.owner.clone() {
            env.storage()
                .persistent()
                .set(&DataKey::MerchantOwner(owner), &merchant_id);
        }
        env.storage()
            .persistent()
            .set(&DataKey::Merchant(merchant_id), &merchant);
        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("ban"), merchant_id),
            MerchantBannedEvent {
                merchant_id,
                banned_by: caller,
            },
        );
        Ok(())
    }

    // --- Storefront closure grace period ---
    //
    // A merchant that still has unsettled escrows may not be closed: the
    // escrow funds belong to buyers, and letting the seller walk away from an
    // open escrow would strand them. Instead the merchant first moves to
    // `MerchantStatus::Inactive`, which stops new orders from being created
    // while leaving the already-funded escrows free to settle. Only once the
    // pending count reaches zero can the registry retire the storefront.

    /// Escrows opened for `merchant_id` that have not been released, refunded
    /// or cancelled yet. Zero for merchants that never opened one.
    pub fn get_active_escrow_count(env: Env, merchant_id: u64) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::ActiveEscrowCount(merchant_id))
            .unwrap_or(0u32)
    }

    /// Records that a new escrow was opened against `merchant_id`.
    ///
    /// Callable by the admin or the configured risk oracle, which are the
    /// addresses the escrow side reports through. The counter saturates so a
    /// misbehaving reporter cannot wrap it back to zero and unlock closure.
    pub fn report_escrow_opened(
        env: Env,
        reporter: Address,
        merchant_id: u64,
    ) -> Result<(), MarketplaceError> {
        reporter.require_auth();
        if !Self::is_admin_or_risk_oracle(&env, &reporter)? {
            return Err(MarketplaceError::Unauthorized);
        }
        Self::get_merchant(env.clone(), merchant_id)?;

        let current = Self::get_active_escrow_count(env.clone(), merchant_id);
        let updated = current.saturating_add(1);
        env.storage()
            .persistent()
            .set(&DataKey::ActiveEscrowCount(merchant_id), &updated);
        Ok(())
    }

    /// Records that an escrow against `merchant_id` settled, releasing the
    /// closure grace period once the last one clears.
    ///
    /// The counter is floored at zero so a duplicate settlement report cannot
    /// wrap it around into `u32::MAX` and deadlock the merchant forever.
    pub fn report_escrow_settled(
        env: Env,
        reporter: Address,
        merchant_id: u64,
    ) -> Result<(), MarketplaceError> {
        reporter.require_auth();
        if !Self::is_admin_or_risk_oracle(&env, &reporter)? {
            return Err(MarketplaceError::Unauthorized);
        }
        Self::get_merchant(env.clone(), merchant_id)?;

        let current = Self::get_active_escrow_count(env.clone(), merchant_id);
        let updated = current.saturating_sub(1);
        env.storage()
            .persistent()
            .set(&DataKey::ActiveEscrowCount(merchant_id), &updated);
        Ok(())
    }

    /// Overwrites the recorded pending count for `merchant_id`.
    ///
    /// Admin-only escape hatch for reconciling against escrow state that was
    /// migrated or restored out of band; it still cannot make the count lower
    /// than reality without the admin asserting so.
    pub fn set_active_escrow_count(
        env: Env,
        admin: Address,
        merchant_id: u64,
        count: u32,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        if admin != Self::get_admin(env.clone())? {
            return Err(MarketplaceError::Unauthorized);
        }
        Self::get_merchant(env.clone(), merchant_id)?;
        env.storage()
            .persistent()
            .set(&DataKey::ActiveEscrowCount(merchant_id), &count);
        Ok(())
    }

    /// Closes the storefront to new orders without retiring the registry
    /// entry. Allowed while escrows are still pending, because the whole point
    /// of the grace period is to let those escrows finish.
    ///
    /// Callable by the merchant owner or the admin. Idempotent: a merchant
    /// that is already `Inactive` is left untouched. A merchant that is
    /// already `Closed` cannot be deactivated.
    pub fn deactivate_merchant(
        env: Env,
        caller: Address,
        merchant_id: u64,
    ) -> Result<(), MarketplaceError> {
        caller.require_auth();
        let mut merchant = Self::get_merchant(env.clone(), merchant_id)?;
        let is_admin = caller == Self::get_admin(env.clone())?;
        if !is_admin && merchant.owner.as_ref() != Some(&caller) {
            return Err(MarketplaceError::Unauthorized);
        }

        if merchant.status == MerchantStatus::Inactive {
            return Ok(());
        }
        if matches!(
            merchant.status,
            MerchantStatus::Closed | MerchantStatus::Banned
        ) {
            return Err(if merchant.status == MerchantStatus::Closed {
                MarketplaceError::MerchantClosed
            } else {
                MarketplaceError::MerchantBanned
            });
        }

        let prev_status = merchant.status;
        if prev_status != MerchantStatus::Suspended {
            let mut stats = Self::get_merchant_stats(env.clone());
            stats.active = stats.active.saturating_sub(1);
            env.storage()
                .instance()
                .set(&DataKey::MerchantStats, &stats);
        } else {
            let mut stats = Self::get_merchant_stats(env.clone());
            stats.suspended = stats.suspended.saturating_sub(1);
            env.storage()
                .instance()
                .set(&DataKey::MerchantStats, &stats);
        }

        let pending_escrows = Self::get_active_escrow_count(env.clone(), merchant_id);
        merchant.status = MerchantStatus::Inactive;
        merchant.updated_at = env.ledger().timestamp();
        if let Some(owner) = merchant.owner.clone() {
            env.storage()
                .persistent()
                .set(&DataKey::MerchantOwner(owner), &merchant_id);
        }
        env.storage()
            .persistent()
            .set(&DataKey::Merchant(merchant_id), &merchant);

        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("inactive"),
                merchant_id,
            ),
            MerchantInactiveEvent {
                merchant_id,
                inactive_by: caller,
                pending_escrows,
            },
        );

        Ok(())
    }

    pub fn close_merchant(
        env: Env,
        admin: Address,
        merchant_id: u64,
        reason: Symbol,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        let current_admin = Self::get_admin(env.clone())?;
        if admin != current_admin {
            return Err(MarketplaceError::Unauthorized);
        }

        let mut merchant = Self::get_merchant(env.clone(), merchant_id)?;
        let prev_status = merchant.status;
        if prev_status != MerchantStatus::Closed {
            // Refuse to strand buyers whose escrows are still open; the
            // merchant has to settle them (or go Inactive and settle them
            // later) before the registry entry can be retired.
            let pending = Self::get_active_escrow_count(env.clone(), merchant_id);
            can_close_merchant(&env, merchant_id, pending)?;

            let mut stats = Self::get_merchant_stats(env.clone());
            match prev_status {
                MerchantStatus::Suspended | MerchantStatus::Banned => {
                    stats.suspended = stats.suspended.saturating_sub(1);
                }
                MerchantStatus::Registered | MerchantStatus::Verified => {
                    stats.active = stats.active.saturating_sub(1);
                }
                // `Inactive` already left the `active` bucket, and `Closed`
                // needs no bucket adjustment.
                MerchantStatus::Inactive
                | MerchantStatus::Closed
                | MerchantStatus::Maintenance
                | MerchantStatus::All => {}
            }
            stats.closed = stats.closed.saturating_add(1);
            env.storage()
                .instance()
                .set(&DataKey::MerchantStats, &stats);
        }

        merchant.status = MerchantStatus::Closed;
        if let Some(owner) = merchant.owner.clone() {
            env.storage()
                .persistent()
                .set(&DataKey::MerchantOwner(owner), &merchant_id);
        }

        // Remove from global merchant index (rebuild without merchant_id).
        let merchant_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantIds)
            .unwrap_or_else(|| Vec::new(&env));
        let mut new_ids: Vec<u64> = Vec::new(&env);
        for mid in merchant_ids.iter() {
            if mid != merchant_id {
                new_ids.push_back(mid);
            }
        }
        env.storage()
            .persistent()
            .set(&DataKey::MerchantIds, &new_ids);

        // Prune from category index
        let cat_key = DataKey::CategoryIndex(merchant.category.clone());
        let cat_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&cat_key)
            .unwrap_or_else(|| Vec::new(&env));
        let mut new_cat_ids: Vec<u64> = Vec::new(&env);
        for cid in cat_ids.iter() {
            if cid != merchant_id {
                new_cat_ids.push_back(cid);
            }
        }
        env.storage().persistent().set(&cat_key, &new_cat_ids);

        // Archive the merchant snapshot
        let closed_at = env.ledger().timestamp();
        let archived = ArchivedMerchant {
            id: merchant.id,
            closed_at,
            last_view: MerchantView {
                id: merchant.id,
                name: merchant.name.clone(),
                category: merchant.category.clone(),
                commission_rate_bps: merchant.commission_rate_bps,
                verified: merchant.verified,
                status: merchant.status,
                reputation_score: None,
            },
        };
        env.storage()
            .persistent()
            .set(&DataKey::ArchivedMerchant(merchant_id), &archived);
        env.storage()
            .persistent()
            .set(&DataKey::MerchantArchivedAt(merchant_id), &closed_at);
        merchant.updated_at = env.ledger().timestamp();

        env.storage()
            .persistent()
            .set(&DataKey::Merchant(merchant_id), &merchant);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("closed"), merchant_id),
            MerchantClosedEvent {
                merchant_id,
                closed_by: admin,
                reason,
                name: merchant.name.clone(),
                category: merchant.category,
            },
        );

        Ok(())
    }

    /// Voluntary merchant deregistration with deposit refund
    ///
    /// Allows merchant owner to close their store voluntarily and receive
    /// their registration deposit back if no open escrows exist.
    ///
    /// # Arguments
    /// * `merchant_id` - The ID of the merchant to deregister
    /// * `owner` - The merchant owner address
    ///
    /// # Returns
    /// * `Ok(())` if deregistration and refund succeed
    /// * `Err(MarketplaceError::HasOpenEscrows)` if merchant has active escrows
    /// * `Err(MarketplaceError::DepositNotFound)` if no deposit exists
    /// * `Err(MarketplaceError::Unauthorized)` if caller is not the owner
    pub fn deregister_merchant(
        env: Env,
        merchant_id: u64,
        owner: Address,
    ) -> Result<(), MarketplaceError> {
        owner.require_auth();

        let mut merchant = Self::get_merchant(env.clone(), merchant_id)?;

        // Verify ownership
        if merchant.owner != Some(owner.clone()) {
            return Err(MarketplaceError::Unauthorized);
        }

        // Check for open escrows (this would need to be integrated with escrow contract)
        // For now, we assume no open escrows if status is not Closed
        // In production, this should query the escrow contract

        // Retrieve deposit information
        let deposit_amount: Option<i128> = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantDeposit(merchant_id));
        let deposit_token: Option<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::DepositToken(merchant_id));

        if deposit_amount.is_none() || deposit_token.is_none() {
            return Err(MarketplaceError::DepositNotFound);
        }

        let deposit = deposit_amount.unwrap();
        let token = deposit_token.unwrap();

        // Update merchant status to Closed
        let prev_status = merchant.status;
        if prev_status != MerchantStatus::Closed {
            let mut stats = Self::get_merchant_stats(env.clone());
            match prev_status {
                MerchantStatus::Suspended | MerchantStatus::Banned => {
                    stats.suspended = stats.suspended.saturating_sub(1);
                }
                MerchantStatus::Registered | MerchantStatus::Verified => {
                    stats.active = stats.active.saturating_sub(1);
                }
                MerchantStatus::Closed => {}
                MerchantStatus::Inactive => {}
                MerchantStatus::Maintenance => {}
                MerchantStatus::All => {}
            }
            stats.closed = stats.closed.saturating_add(1);
            env.storage()
                .instance()
                .set(&DataKey::MerchantStats, &stats);
        }

        merchant.status = MerchantStatus::Closed;
        merchant.updated_at = env.ledger().timestamp();

        // Prune from global merchant index
        let mut merchant_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantIds)
            .unwrap_or_else(|| Vec::new(&env));
        if let Some(pos) = merchant_ids.iter().position(|x| x == merchant_id) {
            merchant_ids.remove(pos as u32);
            env.storage()
                .persistent()
                .set(&DataKey::MerchantIds, &merchant_ids);
        }

        // Prune from category index
        let cat_key = DataKey::CategoryIndex(merchant.category.clone());
        let mut cat_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&cat_key)
            .unwrap_or_else(|| Vec::new(&env));
        if let Some(pos) = cat_ids.iter().position(|x| x == merchant_id) {
            cat_ids.remove(pos as u32);
            env.storage().persistent().set(&cat_key, &cat_ids);
        }

        // Archive the merchant
        let closed_at = env.ledger().timestamp();
        let archived = ArchivedMerchant {
            id: merchant.id,
            closed_at,
            last_view: MerchantView {
                id: merchant.id,
                name: merchant.name.clone(),
                category: merchant.category.clone(),
                commission_rate_bps: merchant.commission_rate_bps,
                verified: merchant.verified,
                status: merchant.status,
                reputation_score: None,
            },
        };
        env.storage()
            .persistent()
            .set(&DataKey::ArchivedMerchant(merchant_id), &archived);
        env.storage()
            .persistent()
            .set(&DataKey::MerchantArchivedAt(merchant_id), &closed_at);

        // Save updated merchant
        env.storage()
            .persistent()
            .set(&DataKey::Merchant(merchant_id), &merchant);

        // Refund deposit to owner
        use soroban_sdk::token;
        let token_client = token::Client::new(&env, &token);
        token_client.transfer(&env.current_contract_address(), &owner, &deposit);

        // Clear deposit storage
        env.storage()
            .persistent()
            .remove(&DataKey::MerchantDeposit(merchant_id));
        env.storage()
            .persistent()
            .remove(&DataKey::DepositToken(merchant_id));

        // Emit events
        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("closed"), merchant_id),
            MerchantClosedEvent {
                merchant_id,
                closed_by: owner.clone(),
                reason: symbol_short!("volunt"),
                name: merchant.name.clone(),
                category: merchant.category,
            },
        );

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("refund"), merchant_id),
            MerchantDepositRefundedEvent {
                merchant_id,
                owner,
                amount: deposit,
                token,
            },
        );

        Ok(())
    }

    /// Prunes closed merchants from `MerchantIds` and `CategoryIndex` (state maintenance).
    ///
    /// Callable by admin in bounded batches (`merchant_ids.len() <= MAX_PAGE_LIMIT`).
    /// Returns the number of closed merchants pruned from active indices.
    pub fn prune_closed_merchants(
        env: Env,
        admin: Address,
        merchant_ids: Vec<u64>,
    ) -> Result<u32, MarketplaceError> {
        admin.require_auth();
        let current_admin = Self::get_admin(env.clone())?;
        if admin != current_admin {
            return Err(MarketplaceError::Unauthorized);
        }
        if merchant_ids.len() > MAX_PAGE_LIMIT {
            return Err(MarketplaceError::InvalidParam);
        }

        let mut all_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantIds)
            .unwrap_or_else(|| Vec::new(&env));
        let mut pruned_count: u32 = 0;

        for id in merchant_ids.iter() {
            if let Ok(merchant) = Self::get_merchant(env.clone(), id) {
                if merchant.status == MerchantStatus::Closed {
                    let mut modified = false;
                    if let Some(pos) = all_ids.iter().position(|x| x == id) {
                        all_ids.remove(pos as u32);
                        modified = true;
                    }
                    let cat_key = DataKey::CategoryIndex(merchant.category.clone());
                    if let Some(mut cat_ids) =
                        env.storage().persistent().get::<_, Vec<u64>>(&cat_key)
                    {
                        if let Some(pos) = cat_ids.iter().position(|x| x == id) {
                            cat_ids.remove(pos as u32);
                            env.storage().persistent().set(&cat_key, &cat_ids);
                        }
                    }
                    if modified {
                        pruned_count += 1;
                    }
                }
            }
        }

        if pruned_count > 0 {
            env.storage()
                .persistent()
                .set(&DataKey::MerchantIds, &all_ids);
            env.events().publish(
                (symbol_short!("mkplc"), symbol_short!("pruned")),
                MerchantPrunedEvent {
                    pruned_count,
                    pruned_by: admin,
                },
            );
        }

        Ok(pruned_count)
    }

    // --- Reputation Pairing Config ---

    pub fn set_merchant_reputation(
        env: Env,
        admin: Address,
        merchant_id: u64,
        reputation: Option<Address>,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        let current_admin = Self::get_admin(env.clone())?;
        if admin != current_admin {
            return Err(MarketplaceError::Unauthorized);
        }

        let mut merchant = Self::get_merchant(env.clone(), merchant_id)?;
        Self::check_not_frozen_or_closed(&merchant)?;

        merchant.reputation = reputation.clone();
        merchant.updated_at = env.ledger().timestamp();

        env.storage()
            .persistent()
            .set(&DataKey::Merchant(merchant_id), &merchant);

        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("rep_set"),
                merchant_id,
            ),
            MerchantReputationSetEvent {
                merchant_id,
                reputation,
                set_by: admin,
            },
        );

        Ok(())
    }

    pub fn set_reputation_contract(
        env: Env,
        admin: Address,
        reputation: Address,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        let current_admin = Self::get_admin(env.clone())?;
        if admin != current_admin {
            return Err(MarketplaceError::Unauthorized);
        }

        env.storage()
            .instance()
            .set(&DataKey::GlobalReputationContract, &reputation);

        Ok(())
    }

    // --- Admin / Config ---

    pub fn propose_admin(
        env: Env,
        current_admin: Address,
        new_admin: Address,
    ) -> Result<bool, MarketplaceError> {
        current_admin.require_auth();
        let admin = Self::get_admin(env.clone())?;
        if current_admin != admin {
            return Err(MarketplaceError::Unauthorized);
        }

        if new_admin == current_admin {
            return Ok(false);
        }

        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &Some(new_admin.clone()));

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("adm_prop")),
            AdminProposedEvent {
                current_admin,
                new_admin,
            },
        );

        Ok(true)
    }

    pub fn accept_admin(env: Env, caller: Address) -> Result<(), MarketplaceError> {
        caller.require_auth();

        let pending: Option<Address> = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .unwrap_or(None);

        // No proposal exists: distinct error so callers can tell this apart
        // from "not the proposed successor" (Unauthorized).
        if pending.is_none() {
            return Err(MarketplaceError::NoPendingAdmin);
        }

        if pending != Some(caller.clone()) {
            return Err(MarketplaceError::Unauthorized);
        }

        let previous_admin = Self::get_admin(env.clone())?;

        env.storage().instance().set(&DataKey::Admin, &caller);
        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &Option::<Address>::None);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("adm_acc")),
            AdminAcceptedEvent {
                previous_admin,
                new_admin: caller,
            },
        );

        Ok(())
    }

    pub fn set_metadata_cooldown(
        env: Env,
        admin: Address,
        cooldown_seconds: u64,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        let current_admin = Self::get_admin(env.clone())?;
        if admin != current_admin {
            return Err(MarketplaceError::Unauthorized);
        }

        let previous = Self::get_metadata_cooldown(env.clone());
        let current =
            cooldown_seconds.clamp(MIN_METADATA_COOLDOWN_SECS, MAX_METADATA_COOLDOWN_SECS);
        if previous == current {
            return Ok(());
        }

        env.storage().instance().set(
            &DataKey::MetadataCooldownConfig,
            &CooldownConfig {
                value_seconds: current,
                min_seconds: MIN_METADATA_COOLDOWN_SECS,
                max_seconds: MAX_METADATA_COOLDOWN_SECS,
            },
        );
        // Keep the original key populated for deployments upgraded from the
        // pre-config format and older readers.
        env.storage()
            .instance()
            .set(&DataKey::MetadataCooldown, &current);
        env.events().publish(
            (symbol_short!("mkplc"), Symbol::new(&env, "cooldown_set")),
            MetadataCooldownSetEvent {
                previous: Some(previous),
                current,
                set_by: admin,
            },
        );

        Ok(())
    }

    pub fn get_metadata_cooldown(env: Env) -> u64 {
        env.storage()
            .instance()
            .get::<_, CooldownConfig>(&DataKey::MetadataCooldownConfig)
            .map(|config| config.value_seconds)
            .or_else(|| env.storage().instance().get(&DataKey::MetadataCooldown))
            .unwrap_or(DEFAULT_METADATA_COOLDOWN_SECS)
    }

    pub fn get_admin(env: Env) -> Result<Address, MarketplaceError> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(MarketplaceError::NotInitialized)
    }

    /// Configure or clear the risk oracle authorized to freeze merchant
    /// trading. Only the platform admin may change this address.
    pub fn set_risk_oracle(
        env: Env,
        admin: Address,
        risk_oracle: Option<Address>,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        if admin != Self::get_admin(env.clone())? {
            return Err(MarketplaceError::Unauthorized);
        }
        match risk_oracle {
            Some(oracle) => env.storage().instance().set(&DataKey::RiskOracle, &oracle),
            None => env.storage().instance().remove(&DataKey::RiskOracle),
        }
        Ok(())
    }

    /// Returns whether a seller may trade. Addresses not registered as
    /// merchant owners remain eligible for non-marketplace escrow orders.
    pub fn is_merchant_trading(env: Env, seller: Address) -> bool {
        let Some(merchant_id) = env
            .storage()
            .persistent()
            .get::<_, u64>(&DataKey::MerchantOwner(seller))
        else {
            return true;
        };
        let Some(merchant) = env
            .storage()
            .persistent()
            .get::<_, Merchant>(&DataKey::Merchant(merchant_id))
        else {
            return false;
        };
        !matches!(
            merchant.status,
            MerchantStatus::Suspended
                | MerchantStatus::Banned
                | MerchantStatus::Closed
                | MerchantStatus::Inactive
        )
    }

    /// Returns the merchant status for a given seller address, or `None` if
    /// the address is not registered as a merchant owner.
    ///
    /// Used by the escrow contract to determine whether a seller has been
    /// administratively suspended or banned by governance, which triggers the
    /// selective buyer withdrawal (fraud restitution) path.
    pub fn get_merchant_status_by_owner(env: Env, seller: Address) -> Option<MerchantStatus> {
        let merchant_id: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantOwner(seller))?;
        let merchant: Merchant = env
            .storage()
            .persistent()
            .get(&DataKey::Merchant(merchant_id))?;
        Some(merchant.status)
    }

    /// Returns `true` when the seller is a registered merchant whose status is
    /// `Suspended` or `Banned`.
    ///
    /// The escrow contract calls this to allow buyers to bypass the remaining
    /// timeout ledger and claim an immediate 100% refund of unreleased escrow
    /// funds when the merchant has been administratively suspended or banned
    /// for confirmed fraudulent activity. Addresses that are not registered
    /// merchants return `false`, so honest active merchants and non-marketplace
    /// sellers are unaffected.
    pub fn is_merchant_suspended_or_banned(env: Env, seller: Address) -> bool {
        match Self::get_merchant_status_by_owner(env, seller) {
            Some(MerchantStatus::Suspended) | Some(MerchantStatus::Banned) => true,
            _ => false,
        }
    }

    /// Returns every registered verifier.
    ///
    /// Prefer [`Self::get_verifiers_page`] for bounded reads: this accessor
    /// materialises the whole set (up to [`MAX_VERIFIERS`] entries) on every
    /// call.
    pub fn get_verifiers(env: Env) -> Vec<Verifier> {
        env.storage()
            .instance()
            .get(&DataKey::Verifiers)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Returns one page of the verifier set.
    ///
    /// `offset` is the zero-based index to start from (clamped to the set
    /// size) and `limit` is the maximum number of entries to return. The
    /// returned `VerifierPage` carries the total set size so callers can
    /// render page counts without a second call, and `next_offset` to pass
    /// back into this function to fetch the following page (`None` once the
    /// end of the set has been reached). A `limit` of zero returns an empty
    /// page. Because the set itself is capped at [`MAX_VERIFIERS`], the read
    /// cost of a page is bounded regardless of the caller-supplied `limit`.
    pub fn get_verifiers_page(env: Env, offset: u32, limit: u32) -> VerifierPage {
        let verifiers = Self::get_verifiers(env.clone());
        let total: u32 = verifiers.len();

        let start = offset.min(total);
        let end = if limit == 0 {
            start
        } else {
            start.saturating_add(limit).min(total)
        };

        let mut items = Vec::new(&env);
        let mut i = start;
        while i < end {
            // In-bounded: i < end <= total == verifiers.len().
            items.push_back(verifiers.get(i).unwrap());
            i += 1;
        }

        let next_offset = if end < total { Some(end) } else { None };
        VerifierPage {
            items,
            total,
            next_offset,
        }
    }

    // ---------------------------------------------------------------------------
    // Appeal bond — admin treasury config
    // ---------------------------------------------------------------------------

    /// Configure the address that receives slashed appeal bonds.
    ///
    /// Only the platform admin may call this. The treasury address is required
    /// before any appeal can be rejected and slashed; calling
    /// [`resolve_merchant_appeal`] with a rejection ruling will fail with
    /// [`MarketplaceError::TreasuryNotSet`] if no treasury has been configured.
    pub fn set_appeal_treasury(
        env: Env,
        admin: Address,
        treasury: Address,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        if admin != Self::get_admin(env.clone())? {
            return Err(MarketplaceError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&DataKey::AppealTreasury, &treasury);
        Ok(())
    }

    /// Return the currently configured appeal-bond treasury, if any.
    pub fn get_appeal_treasury(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::AppealTreasury)
    }

    /// Configure the token contract used for all new appeal bonds.
    ///
    /// Only the platform admin may call this. Existing appeals retain the
    /// token address recorded when they were filed.
    pub fn set_appeal_bond_token(
        env: Env,
        admin: Address,
        bond_token: Address,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        if admin != Self::get_admin(env.clone())? {
            return Err(MarketplaceError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&DataKey::AppealBondToken, &bond_token);
        Ok(())
    }

    /// Return the currently configured appeal-bond token, if any.
    pub fn get_appeal_bond_token(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::AppealBondToken)
    }

    // ---------------------------------------------------------------------------
    // Appeal bond — file
    // ---------------------------------------------------------------------------

    /// File a suspension appeal by locking a refundable staking bond.
    ///
    /// The calling address must be the registered owner of `merchant_id`, and the
    /// merchant must currently be in [`MerchantStatus::Suspended`] status.
    /// Exactly one pending appeal per merchant is allowed at a time.
    ///
    /// The configured bond token is pulled from the merchant owner into the
    /// contract using the SAC `transfer` authorisation flow. The owner must
    /// authorize the transfer when this transaction is submitted.
    ///
    /// # Arguments
    /// * `env`          — execution environment.
    /// * `merchant_id`  — ID of the suspended merchant filing the appeal.
    /// * `bond_amount`  — number of token units to lock (must be > 0).
    ///
    /// # Errors
    /// * [`MarketplaceError::MerchantNotFound`]    — no such merchant.
    /// * [`MarketplaceError::Unauthorized`]        — caller is not the merchant owner.
    /// * [`MarketplaceError::MerchantNotSuspended`]— merchant is not suspended.
    /// * [`MarketplaceError::AppealAlreadyPending`]— a pending appeal already exists.
    /// * [`MarketplaceError::ZeroBondAmount`]      — `bond_amount` is not positive.
    /// * [`MarketplaceError::AppealBondTokenNotSet`]— no bond token is configured.
    pub fn file_merchant_suspension_appeal(
        env: Env,
        merchant_id: u64,
        bond_amount: i128,
    ) -> Result<(), MarketplaceError> {
        // Resolve owner and require their authorisation.
        let merchant = Self::get_merchant(env.clone(), merchant_id)?;
        let owner = merchant
            .owner
            .clone()
            .ok_or(MarketplaceError::Unauthorized)?;
        owner.require_auth();

        // Only the merchant's own owner may file an appeal.
        if merchant.owner != Some(owner.clone()) {
            return Err(MarketplaceError::Unauthorized);
        }

        // Merchant must be suspended to be eligible for appeal.
        if merchant.status != MerchantStatus::Suspended {
            return Err(MarketplaceError::MerchantNotSuspended);
        }

        // Reject zero / negative bond amounts.
        if bond_amount <= 0 {
            return Err(MarketplaceError::ZeroBondAmount);
        }

        let bond_token: Address = env
            .storage()
            .instance()
            .get(&DataKey::AppealBondToken)
            .ok_or(MarketplaceError::AppealBondTokenNotSet)?;

        // Only one pending appeal per merchant.
        if let Some(existing) = env
            .storage()
            .persistent()
            .get::<_, MerchantAppealBond>(&DataKey::AppealBond(merchant_id))
        {
            if existing.resolution_status == AppealResolution::Pending {
                return Err(MarketplaceError::AppealAlreadyPending);
            }
        }

        // Pull bond tokens from the owner into this contract.
        let token_client = soroban_sdk::token::Client::new(&env, &bond_token);
        token_client.transfer(&owner, &env.current_contract_address(), &bond_amount);

        let appealed_at_ledger = env.ledger().sequence();
        let bond = MerchantAppealBond {
            merchant_id,
            bond_amount,
            bond_token: bond_token.clone(),
            appealed_at_ledger,
            resolution_status: AppealResolution::Pending,
        };

        env.storage()
            .persistent()
            .set(&DataKey::AppealBond(merchant_id), &bond);
        env.storage().persistent().extend_ttl(
            &DataKey::AppealBond(merchant_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("ap_filed"),
                merchant_id,
            ),
            MerchantAppealFiledEvent {
                merchant_id,
                bond_amount,
                bond_token,
                appealed_at_ledger,
            },
        );

        Ok(())
    }

    // ---------------------------------------------------------------------------
    // Appeal bond — resolve
    // ---------------------------------------------------------------------------

    /// Resolve a pending merchant suspension appeal.
    ///
    /// Only the platform admin (or, in a future upgrade, a governance contract)
    /// may call this function.
    ///
    /// **`UpheldAndReinstated`**: the bond is refunded to the merchant owner
    /// and the merchant's status is restored to its pre-suspension value
    /// (`Verified` if previously verified, `Registered` otherwise).
    ///
    /// **`RejectedAndSlashed`**: the bond is transferred to the address
    /// configured via [`set_appeal_treasury`]. The merchant remains suspended.
    ///
    /// # Arguments
    /// * `env`         — execution environment.
    /// * `admin`       — must be the current platform admin.
    /// * `merchant_id` — ID of the merchant whose appeal is being resolved.
    /// * `resolution`  — ruling: [`AppealResolution::UpheldAndReinstated`] or
    ///                   [`AppealResolution::RejectedAndSlashed`]. Passing
    ///                   [`AppealResolution::Pending`] is rejected.
    ///
    /// # Errors
    /// * [`MarketplaceError::Unauthorized`]       — caller is not admin.
    /// * [`MarketplaceError::AppealNotFound`]     — no appeal bond on record.
    /// * [`MarketplaceError::AppealAlreadyResolved`]— bond already resolved.
    /// * [`MarketplaceError::InvalidParam`]       — `resolution` is `Pending`.
    /// * [`MarketplaceError::TreasuryNotSet`]     — slash requested but no treasury configured.
    pub fn resolve_merchant_appeal(
        env: Env,
        admin: Address,
        merchant_id: u64,
        resolution: AppealResolution,
    ) -> Result<(), MarketplaceError> {
        admin.require_auth();
        if admin != Self::get_admin(env.clone())? {
            return Err(MarketplaceError::Unauthorized);
        }

        // Pending is not a valid resolution to pass in.
        if resolution == AppealResolution::Pending {
            return Err(MarketplaceError::InvalidParam);
        }

        let mut bond: MerchantAppealBond = env
            .storage()
            .persistent()
            .get(&DataKey::AppealBond(merchant_id))
            .ok_or(MarketplaceError::AppealNotFound)?;

        if bond.resolution_status != AppealResolution::Pending {
            return Err(MarketplaceError::AppealAlreadyResolved);
        }

        let token_client = soroban_sdk::token::Client::new(&env, &bond.bond_token);
        let contract_addr = env.current_contract_address();

        let recipient = match resolution {
            AppealResolution::UpheldAndReinstated => {
                // Reinstate the merchant.
                let mut merchant = Self::get_merchant(env.clone(), merchant_id)?;
                if merchant.status == MerchantStatus::Suspended {
                    let mut stats = Self::get_merchant_stats(env.clone());
                    stats.suspended = stats.suspended.saturating_sub(1);
                    stats.active = stats.active.saturating_add(1);
                    env.storage()
                        .instance()
                        .set(&DataKey::MerchantStats, &stats);

                    merchant.status = if merchant.verified {
                        MerchantStatus::Verified
                    } else {
                        MerchantStatus::Registered
                    };
                    merchant.updated_at = env.ledger().timestamp();
                    env.storage()
                        .persistent()
                        .set(&DataKey::Merchant(merchant_id), &merchant);
                }

                // Refund bond to the merchant owner.
                let owner = merchant.owner.ok_or(MarketplaceError::Unauthorized)?;
                token_client.transfer(&contract_addr, &owner, &bond.bond_amount);
                owner
            }
            AppealResolution::RejectedAndSlashed => {
                // Treasury must be configured before a slash can proceed.
                let treasury: Address = env
                    .storage()
                    .instance()
                    .get(&DataKey::AppealTreasury)
                    .ok_or(MarketplaceError::TreasuryNotSet)?;

                // Slash bond to the customer restitution pool (treasury).
                token_client.transfer(&contract_addr, &treasury, &bond.bond_amount);
                treasury
            }
            // Already handled by the Pending guard above; unreachable in practice.
            AppealResolution::Pending => return Err(MarketplaceError::InvalidParam),
        };

        bond.resolution_status = resolution.clone();
        env.storage()
            .persistent()
            .set(&DataKey::AppealBond(merchant_id), &bond);
        env.storage().persistent().extend_ttl(
            &DataKey::AppealBond(merchant_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("ap_resv"),
                merchant_id,
            ),
            MerchantAppealResolvedEvent {
                merchant_id,
                bond_amount: bond.bond_amount,
                resolution,
                recipient,
            },
        );

        Ok(())
    }

    /// Retrieve the appeal bond record for a merchant, if one exists.
    pub fn get_appeal_bond(env: Env, merchant_id: u64) -> Option<MerchantAppealBond> {
        env.storage()
            .persistent()
            .get(&DataKey::AppealBond(merchant_id))
    }

    // ---------------------------------------------------------------------------

    pub fn version(_env: Env) -> ContractVersion {
        ContractVersion {
            name: symbol_short!("market"),
            semver: symbol_short!("0_0_1"),
        }
    }

    /// Get the current required registration deposit amount
    pub fn get_registration_deposit(env: Env) -> i128 {
        let total_registered: u64 = env
            .storage()
            .instance()
            .get(&DataKey::TotalMerchantsRegistered)
            .unwrap_or(0);
        compute_registration_deposit(total_registered)
    }

    /// Get deposit information for a merchant
    pub fn get_merchant_deposit(env: Env, merchant_id: u64) -> Option<(i128, Address)> {
        let deposit_amount: Option<i128> = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantDeposit(merchant_id));
        let deposit_token: Option<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::DepositToken(merchant_id));

        if let (Some(amount), Some(token)) = (deposit_amount, deposit_token) {
            Some((amount, token))
        } else {
            None
        }
    }

    // --- Helper validation methods ---

    fn check_not_frozen_or_closed(merchant: &Merchant) -> Result<(), MarketplaceError> {
        match merchant.status {
            MerchantStatus::Suspended => Err(MarketplaceError::MerchantFrozen),
            MerchantStatus::Banned => Err(MarketplaceError::MerchantBanned),
            MerchantStatus::Closed => Err(MarketplaceError::MerchantClosed),
            MerchantStatus::Maintenance => Err(MarketplaceError::MerchantInactive),
            // A storefront being wound down takes no new profile edits.
            MerchantStatus::Inactive => Err(MarketplaceError::MerchantInactive),
            _ => Ok(()),
        }
    }

    fn is_admin_or_risk_oracle(env: &Env, caller: &Address) -> Result<bool, MarketplaceError> {
        if caller == &Self::get_admin(env.clone())? {
            return Ok(true);
        }
        Ok(env
            .storage()
            .instance()
            .get::<_, Address>(&DataKey::RiskOracle)
            .as_ref()
            == Some(caller))
    }

    /// Trims leading/trailing ASCII whitespace from `s` and enforces that its
    /// (pre-trim) byte length does not exceed `max_len`.
    ///
    /// The length check happens *before* any bytes are copied out of the
    /// host string, so an oversized string is rejected in constant work
    /// rather than first being paid for byte-by-byte.
    ///
    /// Trimming only ever strips single-byte ASCII whitespace characters
    /// (space, tab, CR, LF, form feed, see `u8::is_ascii_whitespace`), which
    /// can never appear as a continuation byte of a multi-byte UTF-8
    /// sequence, so the remaining slice is always valid UTF-8.
    fn normalize_bounded_string(
        env: &Env,
        s: &String,
        max_len: u32,
    ) -> Result<String, MarketplaceError> {
        let raw_len = s.len();
        if raw_len > max_len {
            return Err(MarketplaceError::InvalidParam);
        }

        let mut buf = [0u8; MAX_FIELD_BUF_LEN];
        let n = raw_len as usize;
        s.copy_into_slice(&mut buf[..n]);

        let mut start = 0usize;
        let mut end = n;
        while start < end && buf[start].is_ascii_whitespace() {
            start += 1;
        }
        while end > start && buf[end - 1].is_ascii_whitespace() {
            end -= 1;
        }

        Ok(String::from_bytes(env, &buf[start..end]))
    }

    /// Normalizes and bounds-checks every string field on `RegisterParams`,
    /// used by both `register_merchant` and, field-by-field, by
    /// `update_merchant_profile`. Whitespace is trimmed to a canonical form
    /// before persistence, and every field is rejected with
    /// `MarketplaceError::InvalidParam` if it exceeds its configured cap.
    /// `name` is additionally required to be non-empty after trimming.
    fn validate_and_normalize(
        env: &Env,
        p: &RegisterParams,
    ) -> Result<RegisterParams, MarketplaceError> {
        let name = Self::normalize_bounded_string(env, &p.name, MAX_NAME_LEN)?;
        if name.len() == 0 {
            return Err(MarketplaceError::InvalidParam);
        }
        let description = Self::normalize_bounded_string(env, &p.description, MAX_DESCRIPTION_LEN)?;
        let image_url = Self::normalize_bounded_string(env, &p.image_url, MAX_IMAGE_URL_LEN)?;
        let metadata = match &p.metadata {
            Some(m) => Some(Self::normalize_bounded_string(env, m, MAX_METADATA_LEN)?),
            None => None,
        };

        // Validate metadata_uri length. We store it verbatim (no whitespace
        // trimming — URIs must not be altered) but reject anything that
        // exceeds MAX_METADATA_URI_LEN bytes to prevent storage bloat.
        let metadata_uri = match &p.metadata_uri {
            Some(uri) => {
                if uri.len() > MAX_METADATA_URI_LEN {
                    return Err(MarketplaceError::MetadataUriTooLong);
                }
                Some(uri.clone())
            }
            None => None,
        };

        Ok(RegisterParams {
            name,
            description,
            category: p.category.clone(),
            image_url,
            metadata,
            metadata_uri,
            required_verifications: p.required_verifications,
        })
    }
    // -------------------------------------------------------------------------
    // Maintenance Mode Functions
    // -------------------------------------------------------------------------

    /// Enter maintenance mode for a merchant storefront.
    ///
    /// Temporarily pauses the storefront from accepting new orders while keeping
    /// it visible. The storefront will automatically exit maintenance when the
    /// scheduled end time is reached.
    ///
    /// # Arguments
    /// * `caller` - Must be merchant owner or admin
    /// * `merchant_id` - The merchant to put in maintenance mode
    /// * `duration_seconds` - How long maintenance should last
    /// * `reason` - Optional description of why maintenance is needed
    ///
    /// # Returns
    /// * `Ok(())` on success
    /// * `Err(MarketplaceError::Unauthorized)` if caller is not owner or admin
    /// * `Err(MarketplaceError::MerchantClosed)` if merchant is closed
    /// * `Err(MarketplaceError::MaintenanceActive)` if already in maintenance
    pub fn enter_maintenance_mode(
        env: Env,
        caller: Address,
        merchant_id: u64,
        duration_seconds: u64,
        reason: Option<String>,
    ) -> Result<(), MarketplaceError> {
        caller.require_auth();

        let mut merchant = Self::get_merchant(env.clone(), merchant_id)?;

        // Verify caller is owner or admin
        let is_admin = caller == Self::get_admin(env.clone())?;
        let is_owner = merchant.owner.as_ref() == Some(&caller);
        if !is_admin && !is_owner {
            return Err(MarketplaceError::Unauthorized);
        }

        // Can't enter maintenance if closed or banned
        if matches!(
            merchant.status,
            MerchantStatus::Closed | MerchantStatus::Banned
        ) {
            return Err(MarketplaceError::MerchantClosed);
        }

        // Check if already in maintenance
        if merchant.status == MerchantStatus::Maintenance {
            return Err(MarketplaceError::MaintenanceActive);
        }

        // Validate duration (min 1 minute, max 30 days)
        if duration_seconds < 60 || duration_seconds > 30 * 24 * 60 * 60 {
            return Err(MarketplaceError::InvalidMaintenanceWindow);
        }

        let now = env.ledger().timestamp();
        let scheduled_end = now.saturating_add(duration_seconds);

        // Create maintenance window record
        let maintenance = MaintenanceWindow {
            merchant_id,
            started_at: now,
            scheduled_end,
            reason: reason.clone(),
            previous_status: merchant.status,
        };

        // Store maintenance window
        env.storage()
            .persistent()
            .set(&DataKey::MaintenanceWindow(merchant_id), &maintenance);
        env.storage().persistent().extend_ttl(
            &DataKey::MaintenanceWindow(merchant_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Update merchant status
        merchant.status = MerchantStatus::Maintenance;
        merchant.updated_at = now;
        env.storage()
            .persistent()
            .set(&DataKey::Merchant(merchant_id), &merchant);

        // Emit event
        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("maint_on"),
                merchant_id,
            ),
            MerchantMaintenanceStartedEvent {
                merchant_id,
                started_at: now,
                scheduled_end,
                reason,
            },
        );

        Ok(())
    }

    /// Exit maintenance mode for a merchant storefront.
    ///
    /// Can be called manually by the owner/admin or automatically when checking
    /// if maintenance has expired.
    ///
    /// # Arguments
    /// * `caller` - Must be merchant owner or admin (or contract itself for auto-exit)
    /// * `merchant_id` - The merchant to exit from maintenance
    ///
    /// # Returns
    /// * `Ok(())` on success
    /// * `Err(MarketplaceError::Unauthorized)` if caller is not authorized
    /// * `Err(MarketplaceError::MerchantClosed)` if merchant is closed
    pub fn exit_maintenance_mode(
        env: Env,
        caller: Address,
        merchant_id: u64,
    ) -> Result<(), MarketplaceError> {
        caller.require_auth();

        let mut merchant = Self::get_merchant(env.clone(), merchant_id)?;

        // Verify caller is owner or admin
        let is_admin = caller == Self::get_admin(env.clone())?;
        let is_owner = merchant.owner.as_ref() == Some(&caller);
        if !is_admin && !is_owner {
            return Err(MarketplaceError::Unauthorized);
        }

        // Must be in maintenance mode
        if merchant.status != MerchantStatus::Maintenance {
            return Ok(());
        }

        // Get maintenance window
        let maintenance: MaintenanceWindow = env
            .storage()
            .persistent()
            .get(&DataKey::MaintenanceWindow(merchant_id))
            .ok_or(MarketplaceError::InvalidMaintenanceWindow)?;

        let now = env.ledger().timestamp();
        let was_automatic = now >= maintenance.scheduled_end;

        // Restore previous status
        merchant.status = maintenance.previous_status;
        merchant.updated_at = now;
        env.storage()
            .persistent()
            .set(&DataKey::Merchant(merchant_id), &merchant);

        // Clean up maintenance window
        env.storage()
            .persistent()
            .remove(&DataKey::MaintenanceWindow(merchant_id));

        // Emit event
        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("maint_of"),
                merchant_id,
            ),
            MerchantMaintenanceEndedEvent {
                merchant_id,
                ended_at: now,
                was_automatic,
            },
        );

        Ok(())
    }

    /// Check if a merchant's maintenance window has expired and auto-release if so.
    ///
    /// This can be called by anyone (e.g., buyers trying to place orders) to
    /// automatically transition merchants out of maintenance when the window expires.
    ///
    /// # Arguments
    /// * `merchant_id` - The merchant to check
    ///
    /// # Returns
    /// * `true` if merchant is currently in maintenance mode
    /// * `false` if maintenance has expired or merchant is not in maintenance
    pub fn check_maintenance_status(env: Env, merchant_id: u64) -> bool {
        let Ok(merchant) = Self::get_merchant(env.clone(), merchant_id) else {
            return false;
        };

        if merchant.status != MerchantStatus::Maintenance {
            return false;
        }

        let Some(maintenance) = env
            .storage()
            .persistent()
            .get::<_, MaintenanceWindow>(&DataKey::MaintenanceWindow(merchant_id))
        else {
            return false;
        };

        let now = env.ledger().timestamp();

        // If maintenance window expired, auto-release
        if now >= maintenance.scheduled_end {
            // Auto-exit by having contract call itself
            let _ = Self::auto_exit_maintenance(&env, merchant_id, maintenance);
            return false;
        }

        true
    }

    /// Internal helper to auto-exit maintenance without requiring auth.
    fn auto_exit_maintenance(
        env: &Env,
        merchant_id: u64,
        maintenance: MaintenanceWindow,
    ) -> Result<(), MarketplaceError> {
        let mut merchant: Merchant = env
            .storage()
            .persistent()
            .get(&DataKey::Merchant(merchant_id))
            .ok_or(MarketplaceError::MerchantNotFound)?;

        let now = env.ledger().timestamp();

        // Restore previous status
        merchant.status = maintenance.previous_status;
        merchant.updated_at = now;
        env.storage()
            .persistent()
            .set(&DataKey::Merchant(merchant_id), &merchant);

        // Clean up maintenance window
        env.storage()
            .persistent()
            .remove(&DataKey::MaintenanceWindow(merchant_id));

        // Emit event
        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("maint_of"),
                merchant_id,
            ),
            MerchantMaintenanceEndedEvent {
                merchant_id,
                ended_at: now,
                was_automatic: true,
            },
        );

        Ok(())
    }

    /// Get maintenance window info for a merchant.
    pub fn get_maintenance_window(env: Env, merchant_id: u64) -> Option<MaintenanceWindow> {
        env.storage()
            .persistent()
            .get(&DataKey::MaintenanceWindow(merchant_id))
    }

    // -------------------------------------------------------------------------
    // SKU Inventory Functions
    // -------------------------------------------------------------------------

    /// Create or initialize SKU inventory for a merchant.
    ///
    /// # Arguments
    /// * `merchant_owner` - Must be the merchant owner
    /// * `merchant_id` - The merchant this SKU belongs to
    /// * `sku_id` - Unique identifier for the SKU (merchant-scoped)
    /// * `initial_stock` - Starting inventory quantity
    ///
    /// # Returns
    /// * `Ok(())` on success
    /// * `Err(MarketplaceError::Unauthorized)` if caller is not merchant owner
    /// * `Err(MarketplaceError::InvalidSku)` if SKU ID is invalid
    pub fn create_sku_inventory(
        env: Env,
        merchant_owner: Address,
        merchant_id: u64,
        sku_id: String,
        initial_stock: u64,
    ) -> Result<(), MarketplaceError> {
        merchant_owner.require_auth();

        // Verify merchant exists and caller is owner
        let merchant = Self::get_merchant(env.clone(), merchant_id)?;
        if merchant.owner.as_ref() != Some(&merchant_owner) {
            return Err(MarketplaceError::Unauthorized);
        }

        // Validate SKU ID
        if sku_id.len() == 0 || sku_id.len() > 64 {
            return Err(MarketplaceError::InvalidSku);
        }

        // Check if SKU already exists
        let sku_key = DataKey::SkuInventory(merchant_id, sku_id.clone());
        if env.storage().persistent().has(&sku_key) {
            return Err(MarketplaceError::InvalidSku);
        }

        let now = env.ledger().timestamp();

        // Create SKU inventory record
        let inventory = SkuInventory {
            sku_id: sku_id.clone(),
            merchant_id,
            available_stock: initial_stock,
            total_added: initial_stock,
            total_sold: 0,
            reserved_stock: 0,
            last_updated: now,
            is_active: true,
        };

        // Store SKU inventory
        env.storage().persistent().set(&sku_key, &inventory);
        env.storage().persistent().extend_ttl(
            &sku_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Update merchant's SKU list
        let mut sku_list: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantSkuList(merchant_id))
            .unwrap_or_else(|| Vec::new(&env));
        sku_list.push_back(sku_id.clone());
        env.storage()
            .persistent()
            .set(&DataKey::MerchantSkuList(merchant_id), &sku_list);
        env.storage().persistent().extend_ttl(
            &DataKey::MerchantSkuList(merchant_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Emit event
        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("sku_new"),
                merchant_id,
            ),
            SkuInventoryCreatedEvent {
                merchant_id,
                sku_id,
                initial_stock,
            },
        );

        Ok(())
    }

    /// Atomically decrement SKU stock for a purchase.
    ///
    /// This provides atomic decrement to prevent overselling. The operation
    /// fails if insufficient stock is available.
    ///
    /// # Arguments
    /// * `merchant_id` - The merchant
    /// * `sku_id` - The SKU to decrement
    /// * `quantity` - Amount to decrement
    ///
    /// # Returns
    /// * `Ok(remaining_stock)` - The stock remaining after decrement
    /// * `Err(MarketplaceError::SkuNotFound)` if SKU doesn't exist
    /// * `Err(MarketplaceError::InsufficientStock)` if not enough stock
    pub fn decrement_sku_stock(
        env: Env,
        merchant_id: u64,
        sku_id: String,
        quantity: u64,
    ) -> Result<u64, MarketplaceError> {
        let sku_key = DataKey::SkuInventory(merchant_id, sku_id.clone());

        let mut inventory: SkuInventory = env
            .storage()
            .persistent()
            .get(&sku_key)
            .ok_or(MarketplaceError::SkuNotFound)?;

        // Check if SKU is active
        if !inventory.is_active {
            return Err(MarketplaceError::InvalidSku);
        }

        // Check sufficient stock (atomic check)
        if inventory.available_stock < quantity {
            return Err(MarketplaceError::InsufficientStock);
        }

        // Perform atomic decrement
        inventory.available_stock = inventory.available_stock.saturating_sub(quantity);
        inventory.total_sold = inventory.total_sold.saturating_add(quantity);
        inventory.last_updated = env.ledger().timestamp();

        // Save updated inventory
        env.storage().persistent().set(&sku_key, &inventory);
        env.storage().persistent().extend_ttl(
            &sku_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Emit event
        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("sku_dec"),
                merchant_id,
            ),
            SkuStockDecrementedEvent {
                merchant_id,
                sku_id,
                quantity,
                remaining_stock: inventory.available_stock,
            },
        );

        Ok(inventory.available_stock)
    }

    /// Replenish SKU stock.
    ///
    /// # Arguments
    /// * `merchant_owner` - Must be merchant owner
    /// * `merchant_id` - The merchant
    /// * `sku_id` - The SKU to replenish
    /// * `quantity` - Amount to add
    ///
    /// # Returns
    /// * `Ok(new_stock)` - The new total stock
    /// * `Err` on authorization or lookup failures
    pub fn replenish_sku_stock(
        env: Env,
        merchant_owner: Address,
        merchant_id: u64,
        sku_id: String,
        quantity: u64,
    ) -> Result<u64, MarketplaceError> {
        merchant_owner.require_auth();

        // Verify merchant exists and caller is owner
        let merchant = Self::get_merchant(env.clone(), merchant_id)?;
        if merchant.owner.as_ref() != Some(&merchant_owner) {
            return Err(MarketplaceError::Unauthorized);
        }

        let sku_key = DataKey::SkuInventory(merchant_id, sku_id.clone());

        let mut inventory: SkuInventory = env
            .storage()
            .persistent()
            .get(&sku_key)
            .ok_or(MarketplaceError::SkuNotFound)?;

        // Add stock
        inventory.available_stock = inventory.available_stock.saturating_add(quantity);
        inventory.total_added = inventory.total_added.saturating_add(quantity);
        inventory.last_updated = env.ledger().timestamp();

        // Save updated inventory
        env.storage().persistent().set(&sku_key, &inventory);
        env.storage().persistent().extend_ttl(
            &sku_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Emit event
        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("sku_rep"),
                merchant_id,
            ),
            SkuStockReplenishedEvent {
                merchant_id,
                sku_id,
                quantity,
                new_stock: inventory.available_stock,
            },
        );

        Ok(inventory.available_stock)
    }

    /// Reserve SKU stock for a pending order.
    ///
    /// Moves stock from available to reserved, preventing other orders from
    /// claiming it while the current order is being processed.
    ///
    /// # Arguments
    /// * `merchant_id` - The merchant
    /// * `sku_id` - The SKU to reserve
    /// * `quantity` - Amount to reserve
    ///
    /// # Returns
    /// * `Ok(())` on success
    /// * `Err(MarketplaceError::InsufficientStock)` if not enough available
    pub fn reserve_sku_stock(
        env: Env,
        merchant_id: u64,
        sku_id: String,
        quantity: u64,
    ) -> Result<(), MarketplaceError> {
        let sku_key = DataKey::SkuInventory(merchant_id, sku_id.clone());

        let mut inventory: SkuInventory = env
            .storage()
            .persistent()
            .get(&sku_key)
            .ok_or(MarketplaceError::SkuNotFound)?;

        // Check sufficient available stock
        if inventory.available_stock < quantity {
            return Err(MarketplaceError::InsufficientStock);
        }

        // Move from available to reserved (atomic)
        inventory.available_stock = inventory.available_stock.saturating_sub(quantity);
        inventory.reserved_stock = inventory.reserved_stock.saturating_add(quantity);
        inventory.last_updated = env.ledger().timestamp();

        // Save updated inventory
        env.storage().persistent().set(&sku_key, &inventory);
        env.storage().persistent().extend_ttl(
            &sku_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Emit event
        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("sku_rsv"),
                merchant_id,
            ),
            SkuStockReservedEvent {
                merchant_id,
                sku_id,
                quantity,
                reserved_total: inventory.reserved_stock,
            },
        );

        Ok(())
    }

    /// Release reserved SKU stock back to available.
    ///
    /// Used when an order is cancelled or fails.
    ///
    /// # Arguments
    /// * `merchant_id` - The merchant
    /// * `sku_id` - The SKU to release
    /// * `quantity` - Amount to release
    ///
    /// # Returns
    /// * `Ok(())` on success
    /// * `Err` on lookup failures
    pub fn release_sku_stock(
        env: Env,
        merchant_id: u64,
        sku_id: String,
        quantity: u64,
    ) -> Result<(), MarketplaceError> {
        let sku_key = DataKey::SkuInventory(merchant_id, sku_id.clone());

        let mut inventory: SkuInventory = env
            .storage()
            .persistent()
            .get(&sku_key)
            .ok_or(MarketplaceError::SkuNotFound)?;

        // Move from reserved back to available
        inventory.reserved_stock = inventory.reserved_stock.saturating_sub(quantity);
        inventory.available_stock = inventory.available_stock.saturating_add(quantity);
        inventory.last_updated = env.ledger().timestamp();

        // Save updated inventory
        env.storage().persistent().set(&sku_key, &inventory);
        env.storage().persistent().extend_ttl(
            &sku_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Emit event
        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("sku_rel"),
                merchant_id,
            ),
            SkuStockReleasedEvent {
                merchant_id,
                sku_id,
                quantity,
                available_stock: inventory.available_stock,
            },
        );

        Ok(())
    }

    /// Confirm a reserved stock sale (moves from reserved to sold).
    ///
    /// # Arguments
    /// * `merchant_id` - The merchant
    /// * `sku_id` - The SKU
    /// * `quantity` - Amount to confirm as sold
    ///
    /// # Returns
    /// * `Ok(())` on success
    pub fn confirm_sku_sale(
        env: Env,
        merchant_id: u64,
        sku_id: String,
        quantity: u64,
    ) -> Result<(), MarketplaceError> {
        let sku_key = DataKey::SkuInventory(merchant_id, sku_id.clone());

        let mut inventory: SkuInventory = env
            .storage()
            .persistent()
            .get(&sku_key)
            .ok_or(MarketplaceError::SkuNotFound)?;

        // Decrement reserved stock and increment sold
        inventory.reserved_stock = inventory.reserved_stock.saturating_sub(quantity);
        inventory.total_sold = inventory.total_sold.saturating_add(quantity);
        inventory.last_updated = env.ledger().timestamp();

        // Save updated inventory
        env.storage().persistent().set(&sku_key, &inventory);
        env.storage().persistent().extend_ttl(
            &sku_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        Ok(())
    }

    /// Get SKU inventory information.
    pub fn get_sku_inventory(
        env: Env,
        merchant_id: u64,
        sku_id: String,
    ) -> Result<SkuInventory, MarketplaceError> {
        let sku_key = DataKey::SkuInventory(merchant_id, sku_id);
        env.storage()
            .persistent()
            .get(&sku_key)
            .ok_or(MarketplaceError::SkuNotFound)
    }

    /// Get all SKU IDs for a merchant.
    pub fn get_merchant_sku_list(env: Env, merchant_id: u64) -> Vec<String> {
        env.storage()
            .persistent()
            .get(&DataKey::MerchantSkuList(merchant_id))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Toggle SKU active status.
    ///
    /// # Arguments
    /// * `merchant_owner` - Must be merchant owner
    /// * `merchant_id` - The merchant
    /// * `sku_id` - The SKU
    /// * `is_active` - New active status
    pub fn set_sku_active_status(
        env: Env,
        merchant_owner: Address,
        merchant_id: u64,
        sku_id: String,
        is_active: bool,
    ) -> Result<(), MarketplaceError> {
        merchant_owner.require_auth();

        // Verify merchant exists and caller is owner
        let merchant = Self::get_merchant(env.clone(), merchant_id)?;
        if merchant.owner.as_ref() != Some(&merchant_owner) {
            return Err(MarketplaceError::Unauthorized);
        }

        let sku_key = DataKey::SkuInventory(merchant_id, sku_id);

        let mut inventory: SkuInventory = env
            .storage()
            .persistent()
            .get(&sku_key)
            .ok_or(MarketplaceError::SkuNotFound)?;

        inventory.is_active = is_active;
        inventory.last_updated = env.ledger().timestamp();

        env.storage().persistent().set(&sku_key, &inventory);
        env.storage().persistent().extend_ttl(
            &sku_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        Ok(())
    }
}

#[cfg(test)]
mod overflow_tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn verify_merchant_count_overflow_returns_error() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let contract_id = env.register(MarketplaceContract, (admin.clone(),));
        let client = MarketplaceContractClient::new(&env, &contract_id);

        let owner = Address::generate(&env);
        let params = RegisterParams {
            name: String::from_str(&env, "overflow-merchant"),
            description: String::from_str(&env, "desc"),
            category: symbol_short!("cate"),
            image_url: String::from_str(&env, "https://example.com/image.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        };

        let merchant_id =
            crate::testutil::register_merchant_with_deposit(&env, &client, &owner, &params);

        env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .set(&DataKey::VerifiedCount(merchant_id), &u32::MAX);
        });

        let verifier = Address::generate(&env);
        let verifier_struct = Verifier {
            address: verifier.clone(),
            label: symbol_short!("v"),
            registered_at: env.ledger().timestamp(),
        };
        client.add_verifier(&admin, &verifier_struct);

        assert_eq!(
            client.try_verify_merchant(&merchant_id, &verifier),
            Err(Ok(MarketplaceError::VerificationCountOverflow))
        );
    }

    #[test]
    fn verification_count_overflow_error_payload() {
        assert_eq!(MarketplaceError::VerificationCountOverflow as u32, 5017);
    }
}

#[cfg(test)]
mod error_code_uniqueness_tests {
    use super::*;

    const PERMISSION_ERROR_RANGE: (u32, u32) = (2000, 2999);
    const ESCROW_ERROR_RANGE: (u32, u32) = (1000, 1999);
    const REPUTATION_ERROR_RANGE: (u32, u32) = (3000, 3999);
    const DELEGATION_ERROR_RANGE: (u32, u32) = (4000, 4999);
    const MARKETPLACE_ERROR_RANGE: (u32, u32) = (5000, 5999);

    fn marketplace_error_codes() -> [u32; 22] {
        [
            MarketplaceError::AlreadyInitialized as u32,
            MarketplaceError::NotInitialized as u32,
            MarketplaceError::Unauthorized as u32,
            MarketplaceError::MerchantNotFound as u32,
            MarketplaceError::AlreadyVerified as u32,
            MarketplaceError::InvalidCommissionBps as u32,
            MarketplaceError::DuplicateMerchantName as u32,
            MarketplaceError::MerchantFrozen as u32,
            MarketplaceError::MerchantClosed as u32,
            MarketplaceError::VerifierAlreadyExists as u32,
            MarketplaceError::VerifierNotFound as u32,
            MarketplaceError::InsufficientVerifications as u32,
            MarketplaceError::MetadataLockActive as u32,
            MarketplaceError::InvalidCategory as u32,
            MarketplaceError::InvalidParam as u32,
            MarketplaceError::NoPendingAdmin as u32,
            MarketplaceError::VerificationCountOverflow as u32,
            MarketplaceError::DuplicateMerchantOwner as u32,
            MarketplaceError::MerchantBanned as u32,
            MarketplaceError::MerchantCategoryNotAllowed as u32,
            MarketplaceError::MetadataUriTooLong as u32,
            MarketplaceError::VerifierLimitReached as u32,
        ]
    }

    #[test]
    fn marketplace_error_codes_are_unique() {
        let codes = marketplace_error_codes();
        for (i, code) in codes.iter().enumerate() {
            for other in codes.iter().skip(i + 1) {
                assert!(
                    code != other,
                    "duplicate error code {} in MarketplaceError",
                    code
                );
            }
        }
    }

    #[test]
    fn marketplace_error_codes_are_in_allocated_range() {
        for code in marketplace_error_codes() {
            assert!(
                (MARKETPLACE_ERROR_RANGE.0..=MARKETPLACE_ERROR_RANGE.1).contains(&code),
                "MarketplaceError code {} outside allocated range",
                code
            );
        }
    }

    #[test]
    fn marketplace_error_codes_do_not_collide_with_other_contracts() {
        let other_ranges = [
            PERMISSION_ERROR_RANGE,
            ESCROW_ERROR_RANGE,
            REPUTATION_ERROR_RANGE,
            DELEGATION_ERROR_RANGE,
        ];
        for code in marketplace_error_codes() {
            for &(start, end) in &other_ranges {
                assert!(
                    !(start..=end).contains(&code),
                    "MarketplaceError code {} collides with reserved range {}-{}",
                    code,
                    start,
                    end
                );
            }
        }
    }
}

#[cfg(test)]
mod storefront_closure_tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::{symbol_short, Address, Env, String};

    struct Ctx {
        env: Env,
        admin: Address,
        owner: Address,
        contract_id: Address,
        client: MarketplaceContractClient<'static>,
    }

    fn setup() -> Ctx {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let owner = Address::generate(&env);
        let contract_id = env.register(MarketplaceContract, (admin.clone(),));
        let client = MarketplaceContractClient::new(&env, &contract_id);
        Ctx {
            env,
            admin,
            owner,
            contract_id,
            client,
        }
    }

    fn register(ctx: &Ctx, name: &str) -> u64 {
        crate::testutil::register_merchant_with_deposit(
            &ctx.env,
            &ctx.client,
            &ctx.owner,
            &RegisterParams {
                name: String::from_str(&ctx.env, name),
                description: String::from_str(&ctx.env, "desc"),
                category: symbol_short!("tech"),
                image_url: String::from_str(&ctx.env, "img.png"),
                metadata: None,
                metadata_uri: None,
                required_verifications: 1,
            },
        )
    }

    #[test]
    fn new_merchant_starts_with_no_pending_escrows() {
        let ctx = setup();
        let id = register(&ctx, "Fresh Store");
        assert_eq!(ctx.client.get_active_escrow_count(&id), 0);
    }

    #[test]
    fn closure_is_refused_while_escrows_are_open() {
        let ctx = setup();
        let id = register(&ctx, "Busy Store");

        ctx.client.report_escrow_opened(&ctx.admin, &id);
        ctx.client.report_escrow_opened(&ctx.admin, &id);
        assert_eq!(ctx.client.get_active_escrow_count(&id), 2);

        let err = ctx
            .client
            .try_close_merchant(&ctx.admin, &id, &symbol_short!("voluntary"))
            .unwrap_err()
            .unwrap();
        assert_eq!(err, MarketplaceError::ActiveOrdersPending);

        // The failed attempt must not have changed any state.
        let merchant = ctx.client.get_merchant(&id);
        assert_eq!(merchant.status, MerchantStatus::Registered);
        assert_eq!(ctx.client.get_active_escrow_count(&id), 2);
    }

    #[test]
    fn closure_succeeds_once_the_last_escrow_settles() {
        let ctx = setup();
        let id = register(&ctx, "Settling Store");

        ctx.client.report_escrow_opened(&ctx.admin, &id);
        ctx.client.report_escrow_settled(&ctx.admin, &id);
        assert_eq!(ctx.client.get_active_escrow_count(&id), 0);

        ctx.client
            .close_merchant(&ctx.admin, &id, &symbol_short!("voluntary"));
        assert_eq!(ctx.client.get_merchant(&id).status, MerchantStatus::Closed);
    }

    #[test]
    fn disputed_escrow_also_blocks_closure() {
        let ctx = setup();
        let id = register(&ctx, "Disputed Store");
        ctx.client.report_escrow_opened(&ctx.admin, &id);

        let err = ctx
            .client
            .try_close_merchant(&ctx.admin, &id, &symbol_short!("dispute"))
            .unwrap_err()
            .unwrap();
        assert_eq!(err, MarketplaceError::ActiveOrdersPending);
        assert_eq!(
            ctx.client.get_merchant(&id).status,
            MerchantStatus::Registered
        );
    }

    #[test]
    fn merchant_can_go_inactive_while_escrows_are_open() {
        let ctx = setup();
        let id = register(&ctx, "Winding Down Store");
        ctx.client.report_escrow_opened(&ctx.admin, &id);

        ctx.client.deactivate_merchant(&ctx.owner, &id);
        let merchant = ctx.client.get_merchant(&id);
        assert_eq!(merchant.status, MerchantStatus::Inactive);
        assert!(!merchant.verified);

        // Still blocked from retiring the registry entry.
        let err = ctx
            .client
            .try_close_merchant(&ctx.admin, &id, &symbol_short!("voluntary"))
            .unwrap_err()
            .unwrap();
        assert_eq!(err, MarketplaceError::ActiveOrdersPending);
    }

    #[test]
    fn inactive_storefront_takes_no_new_orders() {
        let ctx = setup();
        let id = register(&ctx, "Paused Store");
        assert!(ctx.client.is_merchant_trading(&ctx.owner));

        ctx.client.deactivate_merchant(&ctx.owner, &id);
        assert!(!ctx.client.is_merchant_trading(&ctx.owner));

        let view = ctx.client.get_merchant_operational_view(&id);
        assert_eq!(view.status, MerchantStatus::Inactive);
        assert!(!view.effective);
    }

    #[test]
    fn inactive_storefront_rejects_profile_edits() {
        let ctx = setup();
        let id = register(&ctx, "Paused Store");
        ctx.client.deactivate_merchant(&ctx.owner, &id);

        let err = ctx
            .client
            .try_update_merchant_profile(
                &id,
                &ctx.owner,
                &String::from_str(&ctx.env, "Paused Store"),
                &String::from_str(&ctx.env, "new desc"),
                &String::from_str(&ctx.env, "img.png"),
                &None,
            )
            .unwrap_err()
            .unwrap();
        assert_eq!(err, MarketplaceError::MerchantInactive);
    }

    #[test]
    fn deactivation_is_idempotent_and_owner_or_admin_only() {
        let ctx = setup();
        let id = register(&ctx, "Twice Paused");
        let stranger = Address::generate(&ctx.env);

        assert_eq!(
            ctx.client
                .try_deactivate_merchant(&stranger, &id)
                .unwrap_err()
                .unwrap(),
            MarketplaceError::Unauthorized
        );

        ctx.client.deactivate_merchant(&ctx.owner, &id);
        // Second call is a no-op rather than an error.
        ctx.client.deactivate_merchant(&ctx.owner, &id);
        ctx.client.deactivate_merchant(&ctx.admin, &id);
        assert_eq!(
            ctx.client.get_merchant(&id).status,
            MerchantStatus::Inactive
        );
    }

    #[test]
    fn closed_merchant_cannot_be_deactivated() {
        let ctx = setup();
        let id = register(&ctx, "Already Gone");
        ctx.client
            .close_merchant(&ctx.admin, &id, &symbol_short!("voluntary"));

        assert_eq!(
            ctx.client
                .try_deactivate_merchant(&ctx.owner, &id)
                .unwrap_err()
                .unwrap(),
            MarketplaceError::MerchantClosed
        );
    }

    #[test]
    fn full_grace_period_lifecycle() {
        let ctx = setup();
        let id = register(&ctx, "Lifecycle Store");

        // Two buyers check out before the merchant winds the storefront down.
        ctx.client.report_escrow_opened(&ctx.admin, &id);
        ctx.client.report_escrow_opened(&ctx.admin, &id);

        ctx.client.deactivate_merchant(&ctx.owner, &id);
        assert!(!ctx.client.is_merchant_trading(&ctx.owner));
        assert_eq!(
            ctx.client
                .try_close_merchant(&ctx.admin, &id, &symbol_short!("voluntary"))
                .unwrap_err()
                .unwrap(),
            MarketplaceError::ActiveOrdersPending
        );

        // First escrow settles: still one outstanding.
        ctx.client.report_escrow_settled(&ctx.admin, &id);
        assert_eq!(
            ctx.client
                .try_close_merchant(&ctx.admin, &id, &symbol_short!("voluntary"))
                .unwrap_err()
                .unwrap(),
            MarketplaceError::ActiveOrdersPending
        );

        // Second escrow settles: the storefront may finally be retired.
        ctx.client.report_escrow_settled(&ctx.admin, &id);
        ctx.client
            .close_merchant(&ctx.admin, &id, &symbol_short!("voluntary"));

        let merchant = ctx.client.get_merchant(&id);
        assert_eq!(merchant.status, MerchantStatus::Closed);
        assert_eq!(ctx.client.get_active_escrow_count(&id), 0);
        assert!(!ctx.client.is_merchant_trading(&ctx.owner));
    }

    #[test]
    fn escrow_counters_floor_at_zero_and_reject_unauthorized_reporters() {
        let ctx = setup();
        let id = register(&ctx, "Noisy Reporter Store");
        let stranger = Address::generate(&ctx.env);

        assert_eq!(
            ctx.client
                .try_report_escrow_opened(&stranger, &id)
                .unwrap_err()
                .unwrap(),
            MarketplaceError::Unauthorized
        );
        assert_eq!(
            ctx.client
                .try_report_escrow_settled(&stranger, &id)
                .unwrap_err()
                .unwrap(),
            MarketplaceError::Unauthorized
        );

        // Settlement without a matching open must not wrap the counter.
        ctx.client.report_escrow_settled(&ctx.admin, &id);
        assert_eq!(ctx.client.get_active_escrow_count(&id), 0);
    }

    #[test]
    fn admin_can_reconcile_the_pending_count() {
        let ctx = setup();
        let id = register(&ctx, "Migrated Store");

        ctx.client.set_active_escrow_count(&ctx.admin, &id, &3);
        assert_eq!(ctx.client.get_active_escrow_count(&id), 3);
        assert_eq!(
            ctx.client
                .try_close_merchant(&ctx.admin, &id, &symbol_short!("voluntary"))
                .unwrap_err()
                .unwrap(),
            MarketplaceError::ActiveOrdersPending
        );

        ctx.client.set_active_escrow_count(&ctx.admin, &id, &0);
        ctx.client
            .close_merchant(&ctx.admin, &id, &symbol_short!("voluntary"));
    }

    #[test]
    fn can_close_merchant_rejects_under_reported_counts() {
        let ctx = setup();
        let id = register(&ctx, "Optimistic Reporter Store");
        ctx.client.set_active_escrow_count(&ctx.admin, &id, &2);

        // The reported figure must never be able to undercut the registry's
        // own record of what is still open, and a figure above zero closes the
        // gate regardless.
        assert_eq!(
            ctx.env
                .as_contract(&ctx.contract_id, || can_close_merchant(&ctx.env, id, 0)),
            Err(MarketplaceError::ActiveOrdersPending)
        );
        assert_eq!(
            ctx.env
                .as_contract(&ctx.contract_id, || can_close_merchant(&ctx.env, id, 1)),
            Err(MarketplaceError::ActiveOrdersPending)
        );
        assert_eq!(
            ctx.env
                .as_contract(&ctx.contract_id, || can_close_merchant(&ctx.env, id, 2)),
            Err(MarketplaceError::ActiveOrdersPending)
        );

        // Once the registry agrees that nothing is pending, the gate opens.
        ctx.client.set_active_escrow_count(&ctx.admin, &id, &0);
        assert_eq!(
            ctx.env
                .as_contract(&ctx.contract_id, || can_close_merchant(&ctx.env, id, 0)),
            Ok(())
        );
    }

    #[test]
    fn closure_stats_accounting_survives_the_inactive_hop() {
        let ctx = setup();
        let id = register(&ctx, "Counted Store");
        assert_eq!(ctx.client.get_merchant_stats().active, 1);

        ctx.client.report_escrow_opened(&ctx.admin, &id);
        ctx.client.deactivate_merchant(&ctx.owner, &id);
        let stats = ctx.client.get_merchant_stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.closed, 0);

        ctx.client.report_escrow_settled(&ctx.admin, &id);
        ctx.client
            .close_merchant(&ctx.admin, &id, &symbol_short!("voluntary"));
        let stats = ctx.client.get_merchant_stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.closed, 1);
    }

    #[test]
    fn suspending_an_inactive_merchant_keeps_the_grace_period() {
        let ctx = setup();
        let id = register(&ctx, "Suspend After Pause");
        ctx.client.deactivate_merchant(&ctx.owner, &id);

        ctx.client.suspend_merchant(&ctx.admin, &id);

        // The suspension is recorded as a no-op: `Inactive` already blocks
        // trading, and overwriting it would let `unsuspend_merchant` revive
        // the storefront while escrows are still open.
        assert_eq!(
            ctx.client.get_merchant(&id).status,
            MerchantStatus::Inactive
        );
        let stats = ctx.client.get_merchant_stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.suspended, 0);
    }

    #[test]
    fn unsuspend_does_not_revive_an_inactive_storefront() {
        let ctx = setup();
        let id = register(&ctx, "Paused Then Suspended");
        ctx.client.deactivate_merchant(&ctx.owner, &id);
        ctx.client.suspend_merchant(&ctx.admin, &id);

        assert_eq!(
            ctx.client
                .try_unsuspend_merchant(&ctx.admin, &id)
                .unwrap_err()
                .unwrap(),
            MarketplaceError::MerchantInactive
        );
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    /// Deploys a fresh SAC, mints the required registration deposit to
    /// `merchant`, and returns `(token_address, deposit_amount)` ready to be
    /// forwarded to [`MarketplaceContract::register_merchant`].
    pub(crate) fn funded_registration_deposit(env: &Env, merchant: &Address) -> (Address, i128) {
        let token_admin = Address::generate(env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin.clone())
            .address();
        let token = soroban_sdk::token::StellarAssetClient::new(env, &token_id);
        let deposit = compute_registration_deposit(0);
        token.mint(merchant, &deposit);
        (token_id, deposit)
    }

    /// `register_merchant` with a freshly deployed deposit token satisfying
    /// the required registration deposit.
    pub(crate) fn register_merchant_with_deposit(
        env: &Env,
        client: &MarketplaceContractClient,
        merchant: &Address,
        params: &RegisterParams,
    ) -> u64 {
        let (token, deposit) = funded_registration_deposit(env, merchant);
        client.register_merchant(merchant, params, &token, &deposit)
    }

    /// Fallible variant of [`register_merchant_with_deposit`] for tests that
    /// assert on registration failures.
    pub(crate) fn try_register_merchant_with_deposit(
        env: &Env,
        client: &MarketplaceContractClient,
        merchant: &Address,
        params: &RegisterParams,
    ) -> Result<Result<u64, soroban_sdk::Error>, Result<MarketplaceError, soroban_sdk::InvokeError>>
    {
        let (token, deposit) = funded_registration_deposit(env, merchant);
        client.try_register_merchant(merchant, params, &token, &deposit)
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod merchant_operational_view_tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::{Address, Env, String, Symbol};

    fn seed_merchant(
        env: &Env,
        contract_id: &Address,
        id: u64,
        status: MerchantStatus,
        verified: bool,
    ) {
        let merchant = Merchant {
            id,
            owner: None,
            name: String::from_str(env, "Matrix Merchant"),
            description: String::from_str(env, ""),
            category: Symbol::new(env, "general"),
            image_url: String::from_str(env, ""),
            commission_rate_bps: 0,
            metadata: None,
            metadata_uri: None,
            status,
            verified,
            created_at: 0,
            updated_at: 0,
            reputation: None,
        };
        env.as_contract(contract_id, || {
            env.storage()
                .persistent()
                .set(&DataKey::Merchant(id), &merchant);
            env.storage()
                .persistent()
                .set(&DataKey::MerchantName(merchant.name.clone()), &id);
        });
    }

    #[test]
    fn operational_view_tier_matrix() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let contract_id = env.register(MarketplaceContract, (admin.clone(),));
        let client = MarketplaceContractClient::new(&env, &contract_id);

        let cases = [
            (MerchantStatus::Registered, false, MerchantTier::Unverified),
            (MerchantStatus::Registered, true, MerchantTier::Verified),
            (MerchantStatus::Verified, false, MerchantTier::Unverified),
            (MerchantStatus::Verified, true, MerchantTier::Verified),
            (MerchantStatus::Suspended, false, MerchantTier::Suspended),
            (MerchantStatus::Suspended, true, MerchantTier::Suspended),
            (MerchantStatus::Closed, false, MerchantTier::Terminated),
            (MerchantStatus::Closed, true, MerchantTier::Terminated),
            (MerchantStatus::Banned, true, MerchantTier::Terminated),
        ];

        for (i, (status, verified, tier)) in cases.iter().enumerate() {
            let id = i as u64 + 1;
            seed_merchant(&env, &contract_id, id, *status, *verified);
            let view = client.get_merchant_operational_view(&id);
            assert_eq!(view.status, *status);
            assert_eq!(view.verified, *verified);
            assert_eq!(view.tier, *tier);
            assert_eq!(view.bond_balance, 0);
            assert_eq!(view.dispute_count, 0);
        }
    }

    #[test]
    fn operational_view_unregistered_returns_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let contract_id = env.register(MarketplaceContract, (admin.clone(),));
        let client = MarketplaceContractClient::new(&env, &contract_id);

        let res = client.try_get_merchant_operational_view(&999);
        assert_eq!(res, Err(Ok(MarketplaceError::MerchantNotFound)));
    }

    #[test]
    fn operational_view_reports_fee_and_bond() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let contract_id = env.register(MarketplaceContract, (admin.clone(),));
        let client = MarketplaceContractClient::new(&env, &contract_id);

        let owner = Address::generate(&env);
        let params = RegisterParams {
            name: String::from_str(&env, "Fee Merchant"),
            description: String::from_str(&env, "desc"),
            category: Symbol::new(&env, "general"),
            image_url: String::from_str(&env, "https://example.com/img.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 0,
        };
        let id = client.register_merchant(&owner, &params);
        client.set_merchant_commission(&id, &admin, &250);

        env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .set(&DataKey::MerchantBond(id), &1_000i128);
            env.storage()
                .persistent()
                .set(&DataKey::MerchantDisputeCount(id), &3u32);
        });

        let view = client.get_merchant_operational_view(&id);
        assert_eq!(view.effective_fee_bps, 250);
        assert_eq!(view.bond_balance, 1_000);
        assert_eq!(view.dispute_count, 3);
    }

    #[test]
    fn suspend_keeps_verified_flag_but_not_tier() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let contract_id = env.register(MarketplaceContract, (admin.clone(),));
        let client = MarketplaceContractClient::new(&env, &contract_id);

        let verifier = Verifier {
            address: Address::generate(&env),
            label: Symbol::new(&env, "gov"),
            registered_at: 0,
        };
        client.add_verifier(&admin, &verifier);

        let owner = Address::generate(&env);
        let params = RegisterParams {
            name: String::from_str(&env, "Verified Merchant"),
            description: String::from_str(&env, "desc"),
            category: Symbol::new(&env, "general"),
            image_url: String::from_str(&env, "https://example.com/img.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        };
        let id = crate::testutil::register_merchant_with_deposit(&env, &client, &owner, &params);
        client.verify_merchant(&id, &verifier.address);
        client.suspend_merchant(&admin, &id);

        let view = client.get_merchant_operational_view(&id);
        assert_eq!(view.status, MerchantStatus::Suspended);
        assert!(view.verified);
        assert_eq!(view.tier, MerchantTier::Suspended);
    }
}
