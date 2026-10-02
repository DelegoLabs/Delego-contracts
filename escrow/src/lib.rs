//! Delego Escrow Contract
//!
//! Holds funds in escrow until order fulfillment is confirmed.
//!
//! # Event topic schema
//!
//! Entity-scoped lifecycle events are published as `(escrow, <action>,
//! escrow_id)` so off-chain indexers and Soroban RPC subscriptions can filter
//! by escrow directly from the topics, without deserializing the event body
//! (issue #142). The id is also retained in the event data. The topic id type
//! matches the event's own id field: it is the `u64` `escrow_id` for every
//! action except `metadata` and `cancelled`, which carry the `BytesN<32>`
//! order id. Contract-wide events that have no single escrow to route by
//! (`upgraded`, `paused`, `feedist`, `pl_fund`, `pl_wdrw`, and the `admin`
//! transfer events) keep the two-topic `(escrow|admin, <action>)` form.
//!
//! # Topic index
//!
//! | Topic 1 | Topic 2 | Topic 3 | Event payload |
//! |---------|---------|---------|---------------|
//! | `escrow` | `upg_prop` | — | `ContractUpgradeProposedEvent` |
//! | `escrow` | `upg_appr` | — | `ContractUpgradeApprovedEvent` |
//! | `escrow` | `upg_cncl` | — | `ContractUpgradeCancelledEvent` |
//! | `escrow` | `upgraded` | — | `ContractUpgradedEvent` |
//! | `escrow` | `vote` | `u64` escrow_id | `DisputeVotedEvent` |
//! | `escrow` | `resolved` | `u64` escrow_id | `EscrowResolvedEvent` |
//! | `escrow` | `tmo_ext` | `u64` escrow_id | `TimeoutExtendedEvent` / `EscrowTimeoutExtendedEvent` |
//! | `escrow` | `bounty` | `u64` escrow_id | `KeeperBountyPaidEvent` |
//! | `escrow` | `fee_sched` | — | `ConfigChangeScheduledEvent` |
//! | `escrow` | `dispsplit` | `u64` escrow_id | `DisputeResolvedEvent` |
//! # Cancellation protection (issue #355)
//!
//! A seller may only cancel an escrow that is still `Created`, but a created
//! order is not yet funded: without a guard a seller could get a `cancel` in
//! front of a buyer's pending `fund`/`deposit` and invalidate an order the
//! buyer is already committed to. `cancel` therefore refuses a unilateral
//! cancellation until a protection window — snapshotted into the escrow at
//! creation and re-anchored by `accept_order` — has elapsed, and the only
//! early ways out are the buyer's own `agree_cancel` or the escrow timeout.
//! Once the buyer's deposit lands the escrow is `Funded` and `cancel` always
//! fails with `AlreadyFunded`. `get_cancel_eligibility` answers the same
//! question read-only, with a `reason` symbol, so a client never has to guess
//! at transaction ordering.

// Contract crates compile as no_std for release and wasm builds, but keep std
// enabled during testing so dev-dependencies and test assertions operate normally.
// This exact conditional form must be consistent across all workspace contract crates.
#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Bytes, BytesN, Env,
    IntoVal, InvokeError, Map, Symbol, Vec,
    contract, contractimpl, contracttype, symbol_short, Address, Bytes, BytesN, Env, InvokeError,
    Map, Symbol, Vec,
};
use delego_interfaces::LendingPoolClient;

// Formal verification specifications for escrow lifecycle invariants
pub mod invariants;

mod admin_actions;
#[cfg(test)]
mod admin_actions_test;
pub use admin_actions::{
    AdminAction, PendingAdminAction, QueuedAdminAction, ADMIN_ACTION_DELAY_LEDGERS,
    ADMIN_ACTION_DELAY_SECONDS,
};

/// Lifecycle state of an escrow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EscrowStatus {
    /// Escrow has been created but not yet funded.
    Created,
    /// Escrow has been funded by the buyer.
    Funded,
    /// Escrow is in buyer inspection period after delivery confirmation.
    Inspection,
    /// Funds have been released to the seller.
    Released,
    /// Funds have been refunded to the buyer.
    Refunded,
    /// Escrow is disputed and awaiting resolution.
    Disputed,
    /// Escrow has been cancelled by an authorized party.
    Cancelled,
    /// Initial ruling issued in two-tiered dispute — 48-hour appeal window open (issue #354).
    InitialRuling,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArbiterStakingRecord {
    pub arbiter: Address,
    pub staked_amount: i128,
    pub assigned_disputes_count: u32,
    pub is_slashed: bool,
}

/// Instance-level lock held while a release calls external contracts.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReentrancyGuard {
    Unlocked,
    Locked,
}

/// Terminal states an escrow can reach after it is no longer active.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EscrowTerminalState {
    /// Funds were released to the seller.
    Released,
    /// Funds were refunded to the buyer.
    Refunded,
    /// Escrow was cancelled.
    Cancelled,
}

impl EscrowTerminalState {
    /// Returns the terminal state corresponding to the given status, if the status is terminal.
    pub fn from_status(status: &EscrowStatus) -> Option<Self> {
        match status {
            EscrowStatus::Released => Some(EscrowTerminalState::Released),
            EscrowStatus::Refunded => Some(EscrowTerminalState::Refunded),
            EscrowStatus::Cancelled => Some(EscrowTerminalState::Cancelled),
            _ => None,
        }
    }
}

/// Optional yield accrual configuration for an escrow (issue #331).
///
/// References an external lending contract that funds are notionally
/// deposited with while escrowed, and the annual rate at which yield
/// accrues for as long as the escrow remains held.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldConfig {
    /// Lending contract the escrowed funds notionally earn yield through.
    pub lending_contract: Address,
    /// Annual yield rate in basis points (e.g., 500 = 5% APR).
    pub apr_bps: u32,
}

/// Bitflag indicating the escrow has been funded by the buyer.
pub const ESCROW_FLAG_FUNDED: u32 = 1 << 0;
/// Bitflag indicating the escrow is currently disputed.
pub const ESCROW_FLAG_DISPUTED: u32 = 1 << 1;
/// Bitflag indicating the escrow dispute has been appealed.
pub const ESCROW_FLAG_APPEALED: u32 = 1 << 2;
/// Bitflag indicating the escrow has been inspected.
pub const ESCROW_FLAG_INSPECTED: u32 = 1 << 3;
/// Bitflag indicating yield accrual is enabled for the escrow.
pub const ESCROW_FLAG_YIELD_ON: u32 = 1 << 4;

/// Packed lifecycle flags for an escrow, stored as a single `u32` bitmask.
///
/// Consolidating the former boolean fields (`is_funded`, `is_disputed`,
/// `is_appealed`, `is_inspected`) into one integer reduces the serialized
/// size of `EscrowRecord` in persistent storage.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EscrowFlags(pub u32);

impl EscrowFlags {
    /// Returns `true` if the given flag bit is set.
    pub fn has_flag(&self, flag: u32) -> bool {
        (self.0 & flag) != 0
    }

    /// Sets the given flag bit and returns the updated flags.
    pub fn set_flag(&mut self, flag: u32) {
        self.0 |= flag;
    }

    /// Clears the given flag bit and returns the updated flags.
    pub fn clear_flag(&mut self, flag: u32) {
        self.0 &= !flag;
    }
}

/// Basis-point ceiling on `CrossCurrencySwapConfig::max_slippage_bps` (50%).
/// Configs above this are rejected outright as misconfigured rather than
/// merely risky (issue #318).
pub const MAX_SWAP_SLIPPAGE_BPS: u32 = 5_000;

/// Optional cross-currency settlement configuration for an escrow (issue
/// #318). When set on an escrow, `release_with_swap` atomically swaps the
/// seller-bound remainder from `deposit_token` (which must match the
/// escrow's own `record.token`) into `payout_token` through
/// `router_contract` at release time, instead of paying the seller directly
/// in the buyer's deposit token.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrossCurrencySwapConfig {
    /// Token the buyer funded the escrow with; must equal `record.token`.
    pub deposit_token: Address,
    /// Token the seller is paid out in after the atomic swap.
    pub payout_token: Address,
    /// Maximum slippage, in basis points, tolerated between the caller's
    /// expected payout quote and the router's actual output before the
    /// release reverts.
    pub max_slippage_bps: u32,
    /// Soroban DEX/AMM router contract used to execute the swap. Must
    /// implement `swap_exact_tokens_for_tokens(env, from_token: Address,
    /// to_token: Address, amount_in: i128, min_amount_out: i128, to: Address)
    /// -> i128`, delivering `to_token` directly to `to` and returning the
    /// actual amount delivered.
    pub router_contract: Address,
}

/// Optional yield split distribution configuration for an escrow (issue #360).
///
/// When accrued yield is distributed upon release or refund, this config
/// defines how the yield is split between the buyer and seller as basis points.
/// Both parties benefit fairly based on their capital commitment and risk.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldSplitConfig {
    /// Percentage of accrued yield allocated to the seller in basis points (e.g., 5000 = 50%).
    /// The remaining yield (10_000 - seller_yield_share_bps) is allocated to the buyer.
    pub seller_yield_share_bps: u32,
}

/// Buyer inspection period configuration for an escrow (issue #356).
///
/// After delivery confirmation, the buyer has a configurable inspection window
/// to verify the goods/assets for defects before funds are finalized to the seller.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectionPeriodConfig {
    /// Inspection duration in ledgers (e.g., 72 hours = ~51,840 ledgers at 5s/ledger).
    pub inspection_duration_ledgers: u32,
    /// Ledger sequence at which delivery was confirmed.
    pub delivery_confirmed_ledger: u32,
    /// Ledger sequence at which funds will auto-release if no dispute is filed.
    pub auto_release_ledger: u32,
/// Order acceptance and cancel-protection state for a single escrow
/// (issue #355).
///
/// An escrow is created *before* the buyer commits funds, which leaves a
/// window in which a seller that watches the mempool can front-run a pending
/// `fund`/`deposit` with a `cancel` for the very same escrow. The state below
/// is snapshotted at creation (and refreshed by `accept_order`) so the
/// contract can answer one deterministic question — "may this seller cancel
/// unilaterally right now?" — from stored data alone.
///
/// The window is snapshotted per escrow rather than read from a live config on
/// every call, so an admin config change can never retroactively shorten (or
/// lengthen) the protection of an order that is already live.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderAcceptanceState {
    /// Whether the seller has acknowledged the order via `accept_order`.
    pub seller_accepted: bool,
    /// Ledger sequence at which the seller accepted the order, or `0` while
    /// the order has not been accepted yet.
    pub accepted_at_ledger: u32,
    /// Minimum number of ledgers that must elapse after creation (and after a
    /// seller acceptance) before the seller may cancel unilaterally.
    pub cancel_lockout_ledgers: u32,
}

/// Full on-chain record for a single escrow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowRecord {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Buyer's address.
    pub buyer: Address,
    /// Seller's address.
    pub seller: Address,
    /// Token contract address for the escrowed asset.
    pub token: Address,
    /// Total amount of tokens escrowed.
    pub amount: i128,
    /// Amount released to seller so far.
    pub released_amount: i128,
    /// Amount refunded to buyer so far.
    pub refunded_amount: i128,
    /// Current lifecycle state of the escrow.
    pub status: EscrowStatus,
    /// Packed lifecycle flags (funded, disputed, appealed, inspected, yield).
    pub flags: EscrowFlags,
    /// Off-chain order ID this escrow is associated with.
    pub order_id: BytesN<32>,
    /// Ledger timestamp when the escrow was created.
    pub created_at: u64,
    /// Ledger timestamp when the escrow was last updated.
    pub updated_at: u64,
    /// Ledger sequence at which the escrow can be refunded or disputed.
    pub timeout_ledger: u32,
    /// Token id of the NFT proof-of-purchase receipt minted for the buyer
    /// upon release, if any (issue #320). `None` until a full release
    /// successfully mints one (or when no receipt minter is configured, or
    /// minting failed — minting failures never block fund settlement).
    pub receipt_token_id: Option<u64>,
}

/// Token-unit threshold above which escrow releases require finance approval.
pub const DUAL_CONTROL_THRESHOLD: i128 = 10_000;

/// A single tier in a merchant's commission schedule.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommissionTier {
    /// Minimum settled volume (in token units) required for this tier.
    pub min_settled_volume: i128,
    /// Commission rate in basis points applied at this tier.
    pub commission_rate_bps: u32,
}

/// Accumulated settled volume and active commission tier for a merchant.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantVolumeRecord {
    /// Total volume settled through escrow releases for this merchant.
    pub total_settled_volume: i128,
    /// Commission rate in basis points currently applied to this merchant.
    pub active_tier_bps: u32,
    /// Ledger sequence at which the record was last updated.
    pub last_updated_ledger: u32,
}

/// Emitted when a merchant's active commission tier changes.
#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantTierUpdatedEvent {
    /// Merchant whose tier changed.
    pub merchant: Address,
    /// Newly active commission rate in basis points.
    pub active_tier_bps: u32,
    /// Total settled volume at the time of the update.
    pub total_settled_volume: i128,
}

/// Basis-point gas-fee compensation paid to the oracle relayer that submits a
/// valid signed delivery proof via `verify_delivery_and_release` (issue
/// #317). Delivery oracles front the transaction fee for that call out of
/// pocket; this rebate reimburses them directly from the escrowed deposit
/// before the remaining balance is split between the platform fee and the
/// seller, so the buyer's total deposit still fully accounts for the
/// release.
pub const ORACLE_REBATE_BPS: u32 = 10; // 0.10% of the released amount

/// 48 hours in ledgers at ~5 seconds per ledger (17280 ledgers ≈ 48 hours).
/// Used for the appeal window in two-tiered dispute resolution (issue #354).
pub const APPEAL_WINDOW_LEDGERS: u32 = 17_280;
/// Maximum number of line items a multi-item escrow may register (issue #363).
pub const MAX_SUB_ORDER_ITEMS: u32 = 64;
/// Ledger window within which multi-oracle attestations must be submitted
/// (issue #352), measured from the first recorded vote.
pub const MULTI_ORACLE_CONSENSUS_WINDOW_LEDGERS: u32 = 17_280; // ~1 day of ledgers

/// Maximum number of authorized oracles in a multi-oracle configuration (issue #352).
pub const MAX_ORACLES: u32 = 32;

/// Finance approval state for a high-value escrow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DualControlConfig {
    /// Address authorized to provide the secondary approval.
    pub secondary_approver: Address,
    /// Whether the secondary approver has authorized release.
    pub is_secondary_approved: bool,
    /// Ledger timestamp of the secondary approval, or zero before approval.
    pub secondary_approved_at: u64,
}

/// Secondary-approver expiration for a high-value dual-control escrow (#336).
///
/// Stored per escrow by `set_dual_control_timeout`. The deadline is expressed
/// as an absolute ledger sequence (like `EscrowRecord::timeout_ledger`) so the
/// check stays deterministic and cannot be pushed back by a relayer. Once
/// `current_ledger >= approver_deadline_ledger` and the secondary approver has
/// not signed, `handle_dual_control_timeout` applies `fallback_action` so the
/// order can no longer be held hostage by an unresponsive finance approver.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DualControlTimeout {
    /// Ledger sequence at which the secondary approval window closes.
    pub approver_deadline_ledger: u32,
    /// State applied when the deadline passes unapproved (`Disputed` or `Refunded`).
    pub fallback_action: EscrowStatus,
}

impl DualControlTimeout {
    /// Returns `true` when `fallback_action` is one of the two settlement
    /// states a fallback may produce.
    ///
    /// `Released` is deliberately rejected: the timeout may only divert funds
    /// away from the seller (dispute or refund), never authorize a release that
    /// the secondary approver never signed off on.
    pub fn is_valid_fallback(action: &EscrowStatus) -> bool {
        matches!(action, EscrowStatus::Disputed | EscrowStatus::Refunded)
    }
}

/// Outcome of a partial release.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartialReleaseResult {
    /// Amount released to the seller.
    pub released: i128,
    /// Amount still held in escrow.
    pub remaining: i128,
    /// Whether the escrow was fully released by this operation.
    pub fully_released: bool,
}

/// Outcome of a partial refund.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartialRefundResult {
    /// Amount refunded to the buyer.
    pub refunded: i128,
    /// Amount still held in escrow.
    pub remaining: i128,
    /// Whether the escrow was fully refunded by this operation.
    pub fully_refunded: bool,
}

/// Legacy condition metadata retained for storage compatibility.
///
/// A boolean response from `oracle_contract` is no longer sufficient to
/// authorize release; use `SignedDeliveryProof` and
/// `verify_delivery_and_release` instead.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseCondition {
    pub condition_type: Symbol,
    pub oracle_contract: Address,
}

/// Delivery attestation signed by the configured Ed25519 delivery oracle.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedDeliveryProof {
    pub escrow_id: u64,
    pub carrier_code: Symbol,
    pub tracking_hash: BytesN<32>,
    pub delivery_timestamp: u64,
    pub oracle_pubkey: BytesN<32>,
    pub signature: BytesN<64>,
}

/// Buyer/seller authorization envelope for an absolute timeout extension.
///
/// The signatures are included in the canonical authorization arguments. The
/// contract additionally calls `require_auth_for_args` for both participants,
/// so Soroban verifies each participant's cryptographic signature over the
/// complete voucher and a voucher cannot be replayed with changed fields.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeoutExtensionVoucher {
    pub escrow_id: u64,
    pub new_timeout_ledger: u32,
    pub nonce: u64,
    pub buyer_signature: BytesN<64>,
    pub seller_signature: BytesN<64>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
struct SignedDeliveryPayload {
    escrow_id: u64,
    carrier_code: Symbol,
    tracking_hash: BytesN<32>,
    delivery_timestamp: u64,
}

/// Emitted when a new escrow is created.
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowCreatedEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Buyer's address.
    pub buyer: Address,
    /// Seller's address.
    pub seller: Address,
    /// Token contract address for the escrowed asset.
    pub token: Address,
    /// Total amount of tokens escrowed.
    pub amount: i128,
    /// Off-chain order ID.
    pub order_id: BytesN<32>,
    /// Ledger sequence at which the escrow can be refunded or disputed.
    pub timeout_ledger: u32,
}

/// Emitted when escrow creation includes an off-chain order metadata hash.
///
/// `escrow_id` is the 32-byte order id so indexers can join contract events
/// to off-chain order records (same correlation key as `ReleaseEligibility`).
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowMetadataEvent {
    pub escrow_id: BytesN<32>,
    pub order_hash: BytesN<32>,
    pub schema: Symbol,
}

/// Emitted when an escrow is cancelled.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowCancelledEvent {
    /// The escrow ID (32-byte order ID).
    pub escrow_id: BytesN<32>,
    /// Address that cancelled the escrow.
    pub cancelled_by: Address,
    /// Symbolic reason for cancellation.
    pub reason: Symbol,
}

/// Emitted when a seller accepts a created order (issue #355).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowOrderAcceptedEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Seller that accepted the order.
    pub seller: Address,
    /// Ledger sequence at which the seller accepted the order.
    pub accepted_at_ledger: u32,
    /// First ledger at which the seller may cancel this escrow unilaterally.
    pub cancel_allowed_ledger: u32,
}

/// Emitted when a buyer agrees to a cancellation (issue #355).
///
/// The recorded agreement is what lets the seller cancel inside the
/// front-running protection window; it is consumed by the cancelling call.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowCancelAgreedEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Buyer that agreed to the cancellation.
    pub buyer: Address,
    /// Ledger sequence at which the agreement was recorded.
    pub agreed_at_ledger: u32,
}

/// Emitted when funds are released to the seller.
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowReleasedEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Seller's address.
    pub seller: Address,
    /// Amount released to the seller.
    pub amount: i128,
    /// Address that triggered the release.
    pub released_by: Address,
}

/// Emitted when an oracle relayer is reimbursed for the transaction cost of
/// submitting a signed delivery proof via `verify_delivery_and_release`
/// (issue #317).
#[contracttype]
#[derive(Clone, Debug)]
pub struct OracleRebateDisbursedEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Oracle relayer address that received the rebate.
    pub oracle: Address,
    /// Amount of `token` transferred to the oracle as compensation.
    pub rebate_amount: i128,
    /// Token the rebate was paid in (the escrow's deposit token).
    pub token: Address,
}

/// Emitted when an escrow's cross-currency swap settlement is configured or
/// updated via `set_cross_currency_swap_config` (issue #318).
#[contracttype]
#[derive(Clone, Debug)]
pub struct CrossCurrencySwapConfiguredEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Token the buyer funded the escrow with.
    pub deposit_token: Address,
    /// Token the seller will be paid out in.
    pub payout_token: Address,
    /// Maximum tolerated slippage, in basis points.
    pub max_slippage_bps: u32,
    /// Router contract that will execute the swap.
    pub router_contract: Address,
}

/// Emitted when `release_with_swap` completes an atomic cross-currency
/// release (issue #318).
#[contracttype]
#[derive(Clone, Debug)]
pub struct CrossCurrencySwapReleasedEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Seller's address.
    pub seller: Address,
    /// Amount of the escrow's deposit token swapped away.
    pub deposit_amount: i128,
    /// Token the seller was actually paid out in.
    pub payout_token: Address,
    /// Actual amount of `payout_token` delivered to the seller.
    pub payout_amount: i128,
    /// Address that triggered the release.
    pub released_by: Address,
/// Maximum number of milestones a single milestone escrow may carry. Keeps
/// schedule validation and storage bounded.
pub const MAX_MILESTONES: u32 = 20;

/// One tranche of a staggered-release escrow (e.g. deposit, dispatch, delivery).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Milestone {
    /// Identifier unique within the escrow's schedule.
    pub milestone_id: u32,
    /// Principal disbursed (before platform fee) when this milestone is released.
    pub amount: i128,
    /// Short human-readable label, e.g. `deposit` or `dispatch`.
    pub description: Symbol,
    /// Whether this milestone has been paid out.
    pub is_completed: bool,
    /// Ledger timestamp of the payout, or zero while pending.
    pub completed_at: u64,
}

/// Milestone schedule attached to an escrow created via `create_milestone_escrow`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MilestoneEscrowConfig {
    /// Ordered milestone schedule; amounts sum to the escrowed principal.
    pub milestones: Vec<Milestone>,
}

/// Emitted on each milestone payout.
#[contracttype]
#[derive(Clone, Debug)]
pub struct MilestoneReleasedEvent {
    /// Escrow the milestone belongs to.
    pub escrow_id: u64,
    /// Milestone that was released.
    pub milestone_id: u32,
    /// Principal released for this milestone (before platform fee).
    pub amount: i128,
    /// Seller receiving the payout.
    pub seller: Address,
    /// Principal still held in escrow after this payout.
    pub remaining: i128,
}

/// Emitted when an admin registers (or updates) an approved metadata schema.
#[contracttype]
#[derive(Clone, Debug)]
pub struct SchemaRegisteredEvent {
    /// Schema identifier, as referenced by `EscrowMetadata.schema`.
    pub schema: Symbol,
    /// Hash of the off-chain schema definition document.
    pub schema_definition_uri: BytesN<32>,
    /// Admin that registered the schema.
    pub registered_by: Address,
}

/// Emitted alongside the release event when a fully-released escrow had a
/// `YieldConfig` set, reporting the yield accrued over its holding period
/// (issue #331).
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowYieldAccruedEvent {
    pub escrow_id: u64,
    pub seller: Address,
    pub yield_amount: i128,
    pub held_seconds: u64,
}

/// Emitted when a fully-released escrow with yield split distribution completes
/// and accrued yield is split between buyer and seller (issue #360).
///
/// This event replaces `EscrowYieldAccruedEvent` when a `YieldSplitConfig` is
/// configured, providing transparent reporting of how yield is allocated to
/// both parties based on their capital commitment and holding period.
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowYieldSplitAccruedEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Seller's address (receives seller_yield).
    pub seller: Address,
    /// Buyer's address (receives buyer_yield).
    pub buyer: Address,
    /// Total yield accrued over the holding period.
    pub total_yield: i128,
    /// Yield amount allocated to the seller based on configured split.
    pub seller_yield: i128,
    /// Yield amount allocated to the buyer based on configured split.
    pub buyer_yield: i128,
    /// Seconds the escrow was held, used to calculate yield.
    pub held_seconds: u64,
}

/// Emitted when funds are refunded to the buyer.
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowRefundedEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Buyer's address.
    pub buyer: Address,
    /// Amount refunded to the buyer.
    pub amount: i128,
    /// Amount remaining in escrow after this refund.
    pub remaining: i128,
    /// Address that triggered the refund.
    pub refunded_by: Address,
}

/// Emitted when a buyer reclaims escrowed funds immediately because the
/// seller has been banned by marketplace governance (fraud restitution).
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowFraudRestitutionRefundedEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Buyer receiving the refund.
    pub buyer: Address,
    /// Banned seller whose escrow is being unwound.
    pub seller: Address,
    /// Amount refunded to the buyer.
    pub amount: i128,
    /// Marketplace contract that reported the ban.
    pub marketplace_contract: Address,
}

/// Emitted when an admin configures a secondary-approver deadline.
#[contracttype]
#[derive(Clone, Debug)]
pub struct DualControlTimeoutSetEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Ledger sequence at which the secondary approval window closes.
    pub approver_deadline_ledger: u32,
    /// State applied when the deadline passes unapproved.
    pub fallback_action: EscrowStatus,
}

/// Emitted when an expired secondary-approver deadline triggers the fallback.
#[contracttype]
#[derive(Clone, Debug)]
pub struct DualControlTimeoutFallbackEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Ledger sequence at which the deadline was observed as reached.
    pub approver_deadline_ledger: u32,
    /// State applied by this fallback.
    pub fallback_action: EscrowStatus,
    /// Amount refunded to the buyer by a `Refunded` fallback (zero otherwise).
    pub refunded_amount: i128,
    /// Address that triggered the fallback.
    pub executed_by: Address,
}

/// Emitted when a release condition is attached to an escrow.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ReleaseConditionSetEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Symbolic condition type forwarded to the oracle.
    pub condition_type: Symbol,
    /// Oracle contract that evaluates the condition.
    pub oracle_contract: Address,
}

/// Emitted when an escrow is disputed.
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowDisputedEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Address that initiated the dispute.
    pub disputed_by: Address,
}

/// Emitted when a dispute is resolved.
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowResolvedEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Whether the resolution releases funds to the seller.
    pub release_to_seller: bool,
    /// Address that resolved the dispute.
    pub resolved_by: Address,
}

/// Merkle proof for a delivery event committed by a published root.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerkleDeliveryProof {
    /// Daily Merkle root to verify against.
    pub root: BytesN<32>,
    /// SHA-256 hash of the escrow's `order_id`.
    pub leaf: BytesN<32>,
    /// Sibling hashes from the leaf to the root.
    pub proof: Vec<BytesN<32>>,
    /// Zero-based position of the leaf in the tree.
    pub index: u32,
}

/// Amounts and mediator recipient for a split dispute settlement.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeResolutionAward {
    /// Amount returned to the buyer.
    pub buyer_amount: i128,
    /// Amount paid to the seller.
    pub seller_amount: i128,
    /// Mediator's fee.
    pub mediator_fee: i128,
    /// Recipient of the mediator fee.
    pub mediator_address: Address,
}

/// A single registered line item of a multi-item escrow (issue #363).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubOrderItem {
    /// Caller-chosen identifier for the line item.
    pub item_id: Symbol,
    /// Portion of the escrowed principal held for this item.
    pub amount: i128,
}

/// Itemized resolution for one line item of a disputed escrow (issue #363).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubOrderItemResolution {
    /// Identifier of the line item being resolved.
    pub item_id: Symbol,
    /// Amount returned to the buyer for this item.
    pub refund_amount: i128,
    /// Amount released to the seller for this item.
    pub release_amount: i128,
    /// Set `true` once the item has been resolved on-chain.
    pub is_resolved: bool,
}

/// Emitted when a single sub-order line item is resolved (issue #363).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubItemResolvedEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Identifier of the line item that was resolved.
    pub item_id: Symbol,
    /// Amount refunded to the buyer for this item.
    pub refund_amount: i128,
    /// Amount released to the seller for this item.
    pub release_amount: i128,
    /// Escrow balance still held after this resolution.
    pub remaining: i128,
    /// Admin that resolved the item.
    pub resolved_by: Address,
/// k-of-n oracle consensus configuration for an escrow (issue #352).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiOracleConfig {
    /// Number of affirmative attestations required to release the escrow.
    pub required_oracle_count: u32,
    /// Authorized oracle addresses.
    pub oracle_addresses: Vec<Address>,
    /// Condition identifier the oracles attest to.
    pub condition_symbol: Symbol,
}

/// A single oracle's recorded attestation (issue #352).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleVoteRecord {
    /// Oracle that cast the attestation.
    pub oracle: Address,
    /// Whether the oracle attested that the condition is met.
    pub condition_met: bool,
    /// Ledger sequence at which the attestation was recorded.
    pub voted_at_ledger: u32,
}

/// Emitted after a multi-oracle attestation is recorded (issue #352).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleConsensusEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Oracle that cast the attestation.
    pub oracle: Address,
    /// Whether the oracle attested that the condition is met.
    pub condition_met: bool,
    /// Number of distinct affirmative attestations recorded so far.
    pub affirmative_votes: u32,
    /// Number of affirmative attestations required to release.
    pub required_oracle_count: u32,
    /// Whether this attestation triggered the release.
    pub released: bool,
}

/// Emitted after a disputed escrow is paid to the buyer, seller, and mediator.
#[contracttype]
#[derive(Clone, Debug)]
pub struct DisputeResolvedEvent {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Amount returned to the buyer.
    pub buyer_amount: i128,
    /// Amount paid to the seller.
    pub seller_amount: i128,
    /// Mediator's fee.
    pub mediator_fee: i128,
    /// Recipient of the mediator's fee.
    pub mediator_address: Address,
    /// Admin that resolved the dispute.
    pub resolved_by: Address,
}

/// Emitted on each arbiter vote during dispute resolution (#32).
///
/// Reports live tallies so indexers and UIs can track quorum progress
/// on-chain without replaying storage diffs.
#[contracttype]
#[derive(Clone, Debug)]
pub struct DisputeVotedEvent {
    pub escrow_id: u64,
    pub arbiter: Address,
    pub release_to_seller: bool,
    pub votes_for: u32,
    pub threshold: u32,
}

/// Emitted when escrowed funds are split among multiple recipients (#321).
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowSplitReleasedEvent {
    pub escrow_id: u64,
    pub recipient_count: u32,
    pub total_released: i128,
    pub fee_charged: i128,
    pub released_by: Address,
}

/// Affiliate referral configuration for a merchant (issue #affiliate).
///
/// When a merchant is onboarded via an affiliate referrer, a share of the
/// platform commission fee is routed directly to the referrer's address on
/// each settled order.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AffiliateConfig {
    /// Address that receives the referral share of platform fees.
    pub referrer_address: Address,
    /// Referral share of the platform fee in basis points (e.g. 2000 = 20%).
    pub referral_share_bps: u32,
    /// Ledger sequence at which the affiliate configuration expires.
    pub expires_at_ledger: u32,
}

/// Emitted when an escrow's timeout ledger is extended (#323).
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowTimeoutExtendedEvent {
    pub escrow_id: u64,
    pub old_timeout_ledger: u32,
    pub new_timeout_ledger: u32,
    pub extended_by: Address,
}

/// Emitted when an admin transfer is proposed.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AdminProposedEvent {
    /// Current admin address.
    pub current_admin: Address,
    /// Proposed new admin address.
    pub new_admin: Address,
}

/// Emitted when a proposed admin accepts the transfer.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AdminAcceptedEvent {
    /// New admin address that accepted.
    pub new_admin: Address,
}

/// Emitted when a proposed admin transfer is cancelled.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AdminTransferCancelledEvent {
    /// Current admin address who cancelled the transfer.
    pub current_admin: Address,
}

/// Emitted when the contract's pause state changes.
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowPauseChangedEvent {
    /// Whether the contract is now paused.
    pub paused: bool,
    /// Admin address that triggered the change.
    pub admin: Address,
    /// Ledger sequence number of the change.
    pub ledger: u32,
}

/// Pause state for the contract's create and deposit operations.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowPauseState {
    /// Whether new escrow creation is paused.
    pub create_paused: bool,
    /// Address that last updated the pause state.
    pub updated_by: Address,
    /// Ledger sequence of the last update.
    pub updated_at_ledger: u32,
    /// Ledger sequence at which the pause expires, if any.
    pub expires_at_ledger: Option<u32>,
}

/// Emitted when the contract is upgraded to new wasm code (issue #325).
#[contracttype]
#[derive(Clone, Debug)]
pub struct ContractUpgradedEvent {
    pub admin: Address,
    pub previous_semver: Symbol,
    pub new_wasm_hash: BytesN<32>,
}

/// Pending multi-sig contract upgrade (issue #292).
///
/// Created by `propose_upgrade`, co-signed via `approve_upgrade`, and applied
/// by `upgrade` once `approvals` reaches the configured threshold and the
/// `UPGRADE_TIMELOCK_SECS` timelock measured from `proposed_at` has elapsed.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradeProposal {
    pub new_wasm_hash: BytesN<32>,
    pub proposed_at: u64,
    pub approvals: Vec<Address>,
    pub executed: bool,
}

/// Pending emergency rescue proposal for an escrow stranded by a broken token contract.
///
/// Created by an admin and requires multi-sig approval plus a 14-day timelock
/// before funds can be rescued via `emergency_rescue_stalled_escrow`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyRescueProposal {
    /// The escrow ID being rescued.
    pub escrow_id: u64,
    /// Destination address for recovered funds.
    pub recovery_destination: Address,
    /// Ledger timestamp when the proposal was created.
    pub proposed_at: u64,
    /// Multi-sig approvals from admin co-signers.
    pub approvals: Vec<Address>,
    /// Whether the rescue has been executed.
    pub executed: bool,
}

/// Emitted when an admin proposes a contract upgrade (issue #292).
#[contracttype]
#[derive(Clone, Debug)]
pub struct ContractUpgradeProposedEvent {
    pub proposer: Address,
    pub new_wasm_hash: BytesN<32>,
    pub proposed_at: u64,
    pub executable_at: u64,
    pub threshold: u32,
}

/// Emitted when an admin approves the pending upgrade proposal.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ContractUpgradeApprovedEvent {
    pub approver: Address,
    pub new_wasm_hash: BytesN<32>,
    pub approval_count: u32,
    pub threshold: u32,
}

/// Emitted when an admin cancels the pending upgrade proposal.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ContractUpgradeCancelledEvent {
    pub cancelled_by: Address,
    pub new_wasm_hash: BytesN<32>,
}

/// Emitted when the multi-treasury fee distribution is updated (issue #327).
#[contracttype]
#[derive(Clone, Debug)]
pub struct FeeDistributionSetEvent {
    pub admin: Address,
    pub treasury_count: u32,
    pub total_bps: u32,
}

/// Emitted when the admin adds or removes a token from the escrow
/// allowlist (issue #283), so off-chain indexers can track which tokens are
/// currently safe to use as escrow collateral without polling `list_tokens`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenAllowlistUpdatedEvent {
    pub admin: Address,
    pub token: Address,
    /// `true` when the token was added to the allowlist, `false` when removed.
    pub allowed: bool,
/// Emitted when an escrow is rescued via emergency intervention due to a broken external token contract.
/// This event indicates that governance has recovered stranded funds through the timelock-protected
/// emergency rescue mechanism.
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowEmergencyRescuedEvent {
    /// Unique identifier for the rescued escrow.
    pub escrow_id: u64,
    /// Original token contract that failed or became defunct.
    pub original_token: Address,
    /// Amount recovered from the stranded escrow.
    pub amount_recovered: i128,
    /// Address where recovered funds were sent.
    pub recovery_destination: Address,
    /// Admin(s) who authorized the emergency rescue.
    pub authorized_by: Address,
    /// Ledger timestamp when the rescue was executed.
    pub rescued_at: u64,
}

/// Optional metadata hash stored on escrow creation for off-chain order verification.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowMetadata {
    /// Hash of off-chain order details (e.g., order JSON)
    pub order_hash: BytesN<32>,
    /// Schema identifier for the off-chain data (e.g., "order_v1")
    pub schema: Symbol,
}

/// Payload passed to the external receipt-minting contract on full release
/// (issue #320), matching the schema requested for the NFT proof-of-purchase
/// receipt.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PurchaseReceiptData {
    pub order_id: BytesN<32>,
    pub buyer: Address,
    pub seller: Address,
    pub amount: i128,
    pub completed_at: u64,
    /// Item SKU for the purchase. This contract does not otherwise track a
    /// SKU per escrow, so this is populated from the escrow's registered
    /// metadata schema symbol (`EscrowMetadataSchema`) when one was set at
    /// creation, or a generic placeholder symbol when it was not.
    pub item_sku: Symbol,
}

/// Emitted when a purchase receipt is successfully minted for a fully
/// released escrow (issue #320).
#[contracttype]
#[derive(Clone, Debug)]
pub struct PurchaseReceiptMintedEvent {
    pub escrow_id: u64,
    pub token_id: u64,
    pub buyer: Address,
}

/// Maps an escrow to its held token.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowTokenView {
    /// Unique identifier for the escrow.
    pub escrow_id: u64,
    /// Token contract address.
    pub token: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeConfig {
    /// Fee in basis points (e.g., 250 = 2.5%)
    pub fee_bps: u32,
    /// Address that receives the fee
    pub treasury: Address,
}

/// Complete escrow configuration including admin and fee parameters.
/// Used by `constructor` to atomically initialize the contract at deploy time
/// Used by `__constructor` to atomically initialize the contract at deploy time
/// without requiring post-deployment initialization calls that could be front-run.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowConfig {
    pub admin: Address,
    pub fee_bps: u32,
    pub treasury: Address,
    pub min_amount: i128,
    pub max_amount: i128,
}

/// One treasury's share of the release fee, used by [`FeeConfig`]'s
/// multi-treasury successor configured via `set_fee_distribution`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreasuryShare {
    /// Address that receives this share of the fee
    pub treasury: Address,
    /// Share in basis points (e.g., 250 = 2.5%)
    pub bps: u32,
}

/// Net seller payout plus the platform fee deducted for a release amount,
/// as computed by `compute_payout` (issue #27).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleasePayout {
    /// Amount the seller actually receives after the fee is deducted.
    pub seller_net: i128,
    /// Total fee charged across all treasuries.
    pub fee: i128,
    /// Primary treasury that receives the fee (from `FeeConfig`).
    pub treasury: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowAmountLimits {
    pub min_amount: i128,
    pub max_amount: i128,
}

/// One order's parameters for `batch_deposit` (issue #317). The buyer is
/// supplied once for the whole batch (see `batch_deposit`), not per order.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchDepositParams {
    pub seller: Address,
    pub token: Address,
    pub amount: i128,
    pub order_id: BytesN<32>,
    pub timeout_ledgers: u32,
    pub order_hash: Option<BytesN<32>>,
    pub schema: Option<Symbol>,
}

/// One item for an atomic batch of funded escrows.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchEscrowItem {
    /// Seller receiving the funds after release.
    pub seller: Address,
    /// Whitelisted token to escrow.
    pub token: Address,
    /// Positive amount to escrow.
    pub amount: i128,
    /// Unique order identifier.
    pub order_id: BytesN<32>,
    /// Absolute ledger sequence when the escrow timeout expires.
    pub timeout_ledger: u32,
}

/// One item for an atomic batch deposit that funds an existing escrow
/// (issue #317). The buyer and token are supplied once for the whole batch.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchDepositItem {
    /// Escrow to fund.
    pub escrow_id: u64,
    /// Positive amount to deposit into the escrow.
    pub amount: i128,
}

/// Aggregate result returned by `batch_deposit`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchDepositResult {
    /// Number of escrows funded by the batch.
    pub funded_count: u32,
    /// Sum of all item amounts transferred from the buyer.
    pub total_deposited: i128,
}

/// One escrow's release request for `batch_release` (issue #317).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchReleaseParams {
    pub escrow_id: u64,
    pub release_amount: i128,
}

/// One escrow's refund request for `batch_refund` (issue #317).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchRefundParams {
    pub escrow_id: u64,
    pub refund_amount: i128,
}

/// Shared liquidity reserve for a single token, used to instantly settle
/// funded escrows without waiting on the ordinary buyer/admin release flow
/// (issue #335).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiquidityPool {
    pub token: Address,
    pub balance: i128,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct PoolFundedEvent {
    pub token: Address,
    pub funder: Address,
    pub amount: i128,
    pub new_balance: i128,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct PoolWithdrawnEvent {
    pub token: Address,
    pub admin: Address,
    pub amount: i128,
    pub new_balance: i128,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct PoolSettledEvent {
    pub escrow_id: u64,
    pub token: Address,
    pub seller: Address,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuorumConfig {
    pub arbiters: soroban_sdk::Vec<Address>,
    pub threshold: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeVote {
    pub arbiter: Address,
    pub release_to_seller: bool,
}

/// A single arbiter's vote to extend an escrow's refund timeout, cast via
/// `extend_timeout_via_quorum` (issue #333).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeoutExtensionVote {
    pub arbiter: Address,
    pub extension_ledgers: u32,
    pub voted_at: u64,
}

/// Emitted once quorum is reached and `timeout_ledger` is extended.
#[contracttype]
#[derive(Clone, Debug)]
pub struct TimeoutExtendedEvent {
    pub escrow_id: u64,
    pub previous_timeout_ledger: u32,
    pub new_timeout_ledger: u32,
    pub extension_ledgers: u32,
}

/// Emitted when a keeper bumps TTL and receives a bounty.
#[contracttype]
#[derive(Clone, Debug)]
pub struct KeeperBountyPaidEvent {
    pub escrow_id: u64,
    pub keeper: Address,
    pub bounty_amount: i128,
    pub new_timeout_ledger: u32,
}

/// Emitted when escrow enters inspection period after delivery confirmation.
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowInspectionStartedEvent {
    pub escrow_id: u64,
    pub delivery_confirmed_ledger: u32,
    pub inspection_duration_ledgers: u32,
    pub auto_release_ledger: u32,
}

/// Emitted when buyer releases funds after passing inspection.
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowInspectionPassedEvent {
    pub escrow_id: u64,
    pub released_by: Address,
}

/// Emitted when seller/keeper claims funds after inspection auto-release window.
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowInspectionAutoReleasedEvent {
    pub escrow_id: u64,
    pub released_by: Address,
}

/// Emitted when a scheduled config change becomes effective.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ConfigChangeScheduledEvent {
    pub fee_bps: u32,
    pub treasury: Address,
    pub effective_ledger: u32,
    pub scheduled_at_ledger: u32,
    pub scheduled_by: Address,
}

/// Represents a scheduled fee configuration change.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledFeeUpdate {
    pub fee_bps: u32,
    pub treasury: Address,
    pub effective_ledger: u32,
    pub scheduled_at_ledger: u32,
}

/// On-chain shipment proof record for timeout resolution.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShipmentProof {
    /// Escrow ID this proof belongs to.
    pub escrow_id: u64,
    /// Carrier/shipping provider identifier.
    pub carrier: Symbol,
    /// Tracking number hash.
    pub tracking_hash: BytesN<32>,
    /// Timestamp when shipment was made.
    pub shipped_at: u64,
    /// Block timestamp when proof was recorded.
    pub recorded_at: u64,
}

/// Contract version information for deployment scripts and runtime compatibility checks.
///
/// # When to bump
/// - **Patch** (third digit): Bug fixes, internal refactors, gas optimizations — no
///   observable contract-behaviour change to callers.
/// - **Minor** (second digit): New read-only getters, new events, new optional
///   parameters — backward-compatible additions.
/// - **Major** (first digit): Breaking changes — removed functions, changed
///   function signatures, altered storage layout, modified event shapes.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractVersion {
    pub name: Symbol,
    pub semver: Symbol,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminView {
    pub admin: Address,
    pub pending_admin: Option<Address>,
}

/// Emitted when a terminal escrow's persistent storage is reclaimed by
/// `archive_terminal_escrow` (issue #331).
///
/// Published *before* the auxiliary entries are removed so an indexer replaying
/// the ledger sees what was purged even though the state behind the event is
/// gone by the end of the transaction.
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowArchivedEvent {
    /// Unique identifier of the archived escrow.
    pub escrow_id: u64,
    /// Terminal state the escrow had settled into.
    pub terminal_state: EscrowTerminalState,
    /// Ledger timestamp of the escrow's last update (its terminal transition).
    pub terminal_at: u64,
    /// Ledger timestamp at which the sweep ran.
    pub archived_at: u64,
    /// Number of persistent entries removed by this sweep.
    pub cleared_entries: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeVotesPrunedEvent {
    pub pruned_count: u32,
    pub pruned_by: Address,
}

/// Status of a dispute appeal in the two-tiered resolution system.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppealStatus {
    /// Initial ruling issued — 48-hour appeal window open.
    PendingAppealWindow,
    /// Appeal filed and bond deposited — awaiting council review.
    Appealed,
    /// Dispute finalized — either uncontested after deadline or appeal resolved.
    Finalized,
}

/// Record tracking a dispute appeal's state in the two-tiered resolution system.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeAppealRecord {
    /// The party who won the initial ruling.
    pub initial_winner: Address,
    /// The party who filed the appeal.
    pub appealed_by: Address,
    /// Amount in stroops locked as appeal bond.
    pub appeal_bond_amount: i128,
    /// Ledger sequence after which the appeal window closes.
    /// If no appeal is filed before this ledger, ruling is auto-finalized.
    pub appeal_deadline_ledger: u32,
    /// Current status of the appeal.
    pub status: AppealStatus,
}

// ── Cross-currency path payments (issue #337) ───────────────────────────────

/// Maximum number of hops accepted in a single cross-currency route (issue #337).
///
/// Longer routes are rejected with [`EscrowError::InvalidPathRoute`] so the
/// per-leg validation loop and the router invocation stay bounded regardless of
/// what a caller submits.
pub const MAX_PATH_LEGS: u32 = 5;

/// Slippage tolerance committed on-chain for a cross-currency path payment
/// (issue #337).
///
/// `min_output_amount` is the smallest amount of the route's destination token
/// that may be delivered to the seller and `max_input_amount` the largest
/// amount of the escrowed token the route may consume. Both are compared
/// against the amounts the DEX router *actually* reports — never against the
/// amounts it was asked for — so a sandwich attacker who moves the pool price
/// between submission and inclusion cannot force a settlement outside this
/// window: the whole invocation is reverted instead.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlippageBounds {
    /// Smallest acceptable amount of the route's destination token.
    pub min_output_amount: i128,
    /// Largest acceptable amount of the escrowed token the route may spend.
    pub max_input_amount: i128,
}

/// One hop of a cross-currency route: a single pool conversion (issue #337).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathLeg {
    /// Liquidity pool contract that services this hop.
    pub pool: Address,
    /// Token paid into this hop.
    pub token_in: Address,
    /// Token received from this hop.
    pub token_out: Address,
}

/// An ordered cross-currency route submitted to a DEX router (issue #337).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathRoute {
    /// Ordered hops. The first leg consumes the escrowed token and the last leg
    /// pays out the destination token; every leg's `token_out` must equal the
    /// next leg's `token_in`.
    pub legs: Vec<PathLeg>,
}

/// Amounts a DEX router reports after executing a route (issue #337).
///
/// The router is untrusted: `execute_path_payment` re-checks both amounts
/// against the committed [`SlippageBounds`] before any token moves on-chain.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathExecutionResult {
    /// Amount of the escrowed token the route consumed.
    pub input_amount: i128,
    /// Amount of the destination token the route delivered.
    pub output_amount: i128,
}

/// Emitted when the slippage window for an escrow is committed or tightened
/// (issue #337).
#[contracttype]
#[derive(Clone, Debug)]
pub struct SlippageBoundsSetEvent {
    pub escrow_id: u64,
    pub min_output_amount: i128,
    pub max_input_amount: i128,
    pub set_by: Address,
}

/// Emitted when a cross-currency path payment settles inside its committed
/// slippage window (issue #337).
#[contracttype]
#[derive(Clone, Debug)]
pub struct PathPaymentSettledEvent {
    pub escrow_id: u64,
    pub router: Address,
    pub seller: Address,
    pub leg_count: u32,
    pub input_amount: i128,
    pub output_amount: i128,
    pub min_output_amount: i128,
    pub max_input_amount: i128,
    pub settled_by: Address,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Escrow(u64),
    /// Per-escrow finance approval configuration.
    DualControlConfig(u64),
    /// Per-escrow secondary-approver expiration (issue #336).
    DualControlTimeout(u64),
    LastEscrowId,
    PendingAdmin,
    AdminList,
    FeeConfig,
    AmountLimits,
    QuorumConfig,
    DisputeVotes(u64),
    TimeoutExtensionVotes(u64),
    AllowedToken(Address),
    AllowedTokenAt(u32),
    AllowedTokenCount,
    PauseState,
    /// Order metadata hash half, persisted independently (issue #39).
    EscrowMetadataHash(u64),
    /// Order metadata schema half, persisted independently (issue #39).
    EscrowMetadataSchema(u64),
    LiquidityPool(Address),
    /// Set to `true` the first time the contract is upgraded via `upgrade`.
    MigrationFlag,
    /// Optional multi-treasury fee split configured via `set_fee_distribution`.
    FeeDistribution,
    /// Optional yield configuration for an escrow.
    EscrowYieldConfig(u64),
    /// Optional yield split configuration for an escrow (issue #360).
    /// Defines how accrued yield is distributed between buyer and seller.
    EscrowYieldSplitConfig(u64),
    /// Release condition for an escrow.
    ReleaseCondition(u64),
    /// Ed25519 public key authorized to sign delivery proofs.
    OraclePublicKey,
    /// Marketplace contract used to check whether escrow sellers may trade.
    MerchantRegistry,
    /// External contract invoked to mint an NFT proof-of-purchase receipt
    /// on full release (issue #320). Optional — when unset, release
    /// settlement proceeds exactly as before with no minting attempted.
    ReceiptMinterContract,
    /// Authorized merchant categories for spend validation (MCC codes).
    AuthorizedCategories,
    /// Admin flag: when `true`, buyer-originated releases on the escrow must
    /// pass `get_release_eligibility` (issue #48).
    RequireReleaseCondition(u64),
    /// Append-only list of all escrow IDs, used for paginated enumeration (issue #49).
    EscrowIds,
    /// Number of escrow IDs in a buyer's index.
    BuyerEscrowCount(Address),
    /// Per-buyer escrow ID at a zero-based index.
    BuyerEscrowAt(Address, u32),
    /// Last ledger when bump_ttl_with_bounty was called (rate limiting).
    LastBumpLedger(u64),
    /// Scheduled fee update pending activation.
    ScheduledFeeUpdate,
    /// Shipment proof for an escrow (required for seller claim on timeout).
    ShipmentProof(u64),
    /// Current multi-sig upgrade proposal (issue #292).
    UpgradeProposal,
    /// M-of-N admin approvals required to execute an upgrade (issue #292).
    UpgradeThreshold,
    /// Optional cross-currency swap settlement configuration for an escrow
    /// (issue #318).
    CrossCurrencySwapConfig(u64),
    /// Instance-storage re-entrancy lock held while an entry point is executing
    /// an external call. A re-entrant invocation observes `true` and is
    /// rejected with [`EscrowError::ReentrancyDetected`] (issue #334).
    ReentrancyGuard,
    /// Two-tiered dispute appeal record, keyed by escrow_id (issue #354).
    DisputeAppeal(u64),
    /// Address of the appeals council authorized to finalize appeals (issue #354).
    AppealsCouncil,
    ArbiterStake(Address),
    DisputeDeadline(u64),
    /// Slippage window committed for an escrow's cross-currency path payment
    /// (issue #337).
    PathSlippageBounds(u64),
    /// Merchant's settled transaction volume, accumulated on each successful
    /// escrow release (issue #328).
    MerchantSettledVolume(Address),
    /// Highest fee tier the merchant has achieved so far (issue #328).
    MerchantFeeTier(Address),
    /// Admin-configured volume fee tier table (issue #328).
    FeeTiers,
    /// Published daily delivery Merkle root for a UTC date
    /// (epoch day as `u64`).
    MerkleRoot(u64),
    /// Buyer inspection period configuration for an escrow (issue #356).
    InspectionPeriodConfig(u64),
    /// Emergency rescue proposal for a stranded escrow (issue #365).
    EmergencyRescueProposal(u64),
    /// Admin-approved metadata schema → hash of its definition document.
    RegisteredSchema(Symbol),
    /// Milestone schedule for escrows created via `create_milestone_escrow`.
    MilestoneConfig(u64),
    /// Prevents release entry points from being re-entered during token calls.
    ReleaseGuard,
    /// Per-escrow order acceptance + cancel lockout snapshot (issue #355).
    OrderAcceptance(u64),
    /// First ledger at which a unilateral seller cancel is permitted for an
    /// escrow (issue #355). `u32::MAX` while the escrow is un-cancellable.
    CancelLockoutLedger(u64),
    /// Buyer agreement that lets the seller cancel inside the protection
    /// window (issue #355).
    CancelBuyerAgreement(u64),
    /// Highest accepted mutual timeout-extension nonce for an escrow.
    TimeoutExtensionNonce(u64),
    /// Contract-wide default cancel lockout window, in ledgers (issue #355).
    CancelLockoutLedgers,
    /// Minimum release fee floor, in the token's smallest unit, configurable by
    /// the admin (issue #362).
    MinFeeStroops,
}

// `export = false` suppresses the generated `contractspecv0` entry for this
// enum only. The Soroban XDR spec caps `ScSpecUdtErrorEnumV0.cases` at 50
// entries, and this enum carries more variants than that (the codes are frozen
// and cannot be pruned to fit). Dropping the spec entry keeps the crate
// compiling and leaves the runtime ABI untouched: every variant still converts
// to and from `soroban_sdk::Error` through the `TryFrom`/`From` impls generated
// below, with the same numeric codes. The registry table in this attribute is
// the authoritative error catalogue for tooling.
#[contracterror(export = false)]
    /// Published delivery Merkle root for a UTC epoch day.
    MerkleRoot(u64),
}

    /// Published delivery Merkle root for a UTC epoch day.
    MerkleRoot(u64),
    /// Registered line items of a multi-item escrow (issue #363).
    SubOrderItems(u64),
    /// Persisted resolution record for a single line item (issue #363).
    SubItemResolution(u64, Symbol),
}

    /// Published delivery Merkle root for a UTC epoch day.
    MerkleRoot(u64),
    /// Multi-oracle consensus configuration for an escrow (issue #352).
    MultiOracleConfig(u64),
    /// Recorded oracle attestations for an escrow (issue #352).
    EscrowOracleVotes(u64),
}

// NOTE: `EscrowError` intentionally does not use the `#[contracterror]` derive.
// The Soroban XDR spec for a contract error enum is capped at 50 cases
// (`VecM<ScSpecUdtErrorEnumCaseV0, 50>`), but this ABI carries more than that.
// The equivalent conversions are implemented by hand below (without emitting
// the on-chain spec entry) so every historic code stays representable.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
    /// Daily delivery Merkle tree root for batch delivery verification.
    MerkleRoot(u64),
}

// Canonical ABI numbering for `EscrowError`.
//
// Error codes are part of the contract ABI. They are frozen for the 0.x
// deployment line; do not renumber, remove, or reuse existing codes. The
// declaration order below is not meaningful — the `#[repr(u32)]` values are.
//
// # Registry
//
// `First version` records the first contract version in which a code is
// known to be present. Legacy variants are marked `≤0.2.0` because they
// predate this audit; exact pre-0.2.0 introduction releases are not
// tracked.
//
// | Code | Variant | First version |
// |------|---------|---------------|
// | 1 | AlreadyInitialized | ≤0.2.0 |
// | 2 | NotFound | ≤0.2.0 |
// | 3 | Unauthorized | ≤0.2.0 |
// | 4 | AlreadyReleased | ≤0.2.0 |
// | 5 | AlreadyRefunded | ≤0.2.0 |
// | 6 | InvalidStatus | ≤0.2.0 |
// | 7 | TimeoutNotReached | ≤0.2.0 |
// | 8 | NotDisputed | ≤0.2.0 |
// | 9 | InvalidAmount | ≤0.2.0 |
// | 10 | TokenNotWhitelisted | ≤0.2.0 |
// | 11 | InsufficientEscrowBalance | ≤0.2.0 |
// | 12 | ZeroAmount | ≤0.2.0 |
// | 13 | NoPendingTransfer | ≤0.2.0 |
// | 14 | InvalidPendingAdmin | ≤0.2.0 |
// | 15 | AdminAlreadyExists | ≤0.2.0 |
// | 16 | InvalidFeeBps | ≤0.2.0 |
// | 17 | AmountBelowMin | ≤0.2.0 |
// | 18 | AmountAboveMax | ≤0.2.0 |
// | 19 | InvalidLimits | ≤0.2.0 |
// | 20 | NotAnArbiter | ≤0.2.0 |
// | 21 | AlreadyVoted | ≤0.2.0 |
// | 22 | InvalidQuorum | ≤0.2.0 |
// | 23 | QuorumNotReached | ≤0.2.0 |
// | 24 | QuorumConfigNotSet | ≤0.2.0 |
// | 25 | ConflictingQuorum | ≤0.2.0 |
// | 26 | CreationPaused | ≤0.2.0 |
// | 27 | AlreadyCancelled | ≤0.2.0 |
// | 28 | AlreadyFunded | ≤0.2.0 |
// | 29 | InvalidExtension | ≤0.2.0 |
// | 30 | PoolNotFound | ≤0.2.0 |
// | 31 | InsufficientPoolBalance | ≤0.2.0 |
// | 32 | InvalidAddress | ≤0.2.0 |
// | 33 | InvalidEscrowParticipants | ≤0.2.0 |
// | 36 | ReleaseConditionNotSet | ≤0.2.0 |
// | 37 | OracleCallFailed | ≤0.2.0 |
// | 38 | ConditionNotMet | ≤0.2.0 |
// | 39 | InvalidYieldConfig | ≤0.2.0 |
// | 40 | AmountLimitsNotSet | ≤0.2.0 |
// | 41 | FeeConfigNotSet | ≤0.2.0 |
// | 43 | MerchantNotTrading | New |
// | 44 | MerchantStatusCheckFailed | New |
// | 45 | SignedProofRequired | New |
// | 46 | InvalidSignedDeliveryProof | New |
// | 47 | OraclePublicKeyNotSet | New |
// | 201 | InvalidReleaseRecipient | ≤0.2.0 |
// | 400 | MetadataNotSet | next major |
// | 401 | InvalidMetadata | next major |
// | 402 | BumpRateLimitExceeded | next major |
// | 403 | BumpThresholdNotMet | next major |
// | 404 | ShipmentProofNotFound | next major |
// | 405 | FeeUpdateNotEffective | next major |
// | 406 | FeeNoticeWindowNotMet | next major |
// | 407 | InvalidMerkleProof | next major |
// | 408 | MerkleRootAlreadyPublished | next major |
// | 409 | BatchLimitExceeded | next major |
// | 410 | InvalidDisputeAward | next major |
// | 411 | InvalidSwapConfig | next major |
// | 412 | SwapConfigNotSet | next major |
// | 413 | SlippageExceeded | next major |
// | 414 | SwapRouterCallFailed | next major |
// | 415 | ReentrancyDetected | next major |
// | 416 | AppealWindowExpired | next major |
// | 417 | AppealAlreadyFiled | next major |
// | 418 | NoAppealFound | next major |
// | 419 | NotAppealsCouncil | next major |
// | 420 | DisputeNotInitialRuling | next major |
// | 421 | AppealBondInsufficient | next major |
// | 422 | ArbiterStakedAmountZero | next major |
// | 423 | DisputeNotExpired | next major |
// | 424 | ArbiterNotAssigned | next major |
// | 425 | SlippageBoundsNotSet | next major |
// | 426 | InvalidSlippageBounds | next major |
// | 427 | SlippageBoundsTooLoose | next major |
// | 428 | InvalidPathRoute | next major |
// | 429 | PathExecutionFailed | next major |
// | 430 | InvalidTier | next major |
// | 431 | TierLimitExceeded | next major |
// | 432 | TierNotFound | next major |
// | 433 | UpgradeProposalExists | next major |
// | 434 | UpgradeProposalNotFound | next major |
// | 435 | UpgradeHashMismatch | next major |
// | 436 | UpgradeTimelockActive | next major |
// | 437 | MerchantCategoryNotAllowed | next major |
// | 438 | ApproverDeadlineExpired | next major |
// | 439 | ApproverDeadlineNotReached | next major |
// | 440 | InvalidFallbackAction | next major |
// | 441 | DualControlTimeoutNotConfigured | next major |
// | 442 | DualControlAlreadyApproved | next major |
// | 443 | ArchivalRetentionNotElapsed | next major |
// | 444 | NotInInspection | next major |
// | 445 | InspectionExpired | next major |
// | 446 | InspectionConfigNotSet | next major |
// | 447 | InspectionAutoReleaseNotReady | next major |
// | 448+ | Reserved for new variants | next major |
// | 407 | UpgradeProposalExists | next major |
// | 408 | UpgradeProposalNotFound | next major |
// | 409 | UpgradeTimelockActive | next major |
// | 410 | UpgradeHashMismatch | next major |
// | 411 | EmergencyRescueProposalNotFound | next major |
// | 412 | EmergencyRescueTimelockNotElapsed | next major |
// | 413 | EmergencyRescueThresholdNotMet | next major |
// | 414 | EmergencyRescueAlreadyExecuted | next major |
// | 415 | EscrowNotEligibleForRescue | next major |
// | 416+ | Reserved for new variants | next major |
// | 411 | InvalidMilestoneSchedule | next major |
// | 412 | MilestoneNotFound | next major |
// | 413 | MilestoneAlreadyReleased | next major |
// | 414 | MilestoneReleaseRequired | next major |
// | 415+ | Reserved for new variants | next major |
// | 411 | GuardianNotSet | next major |
// | 412 | GuardianAlreadySet | next major |
// | 413 | AdminActionNotFound | next major |
// | 414 | AdminActionLocked | next major |
// | 415 | AdminActionVetoed | next major |
// | 416 | AdminActionAlreadyQueued | next major |
// | 417 | AdminActionOverflow | next major |
// | 418+ | Reserved for new variants | next major |
// | 411 | MathOverflow | next major |
// | 412 | ReentrancyDetected | next major |
// | 411 | CancelLockoutActive | next major |
// | 412 | InvalidCancelLockout | next major |
// | 411 | MathOverflow | next major |
// | 412 | InvalidMinFee | next major |
// | 413+ | Reserved for new variants | next major |
//
// # Allocating new variants
//
// New variants MUST use codes in the reserved contiguous range starting at
// 400. Do not fill historical gaps or reuse codes from the registry above.
// | 400+ | Reserved for new variants | next major |
// # Cross-contract allocation
// The contract error enums (`EscrowError`, `PermissionError`,
// `ReputationError`, `DelegationError`, `MarketplaceError`) share a single
// numeric ABI space when errors surface over a bridge.  Each contract owns
// a disjoint range; the table below is the canonical allocation and is
// checked by `error_code_allocation_tests`.
// | Contract | Error enum | Allocated range |
// |----------|------------|-----------------|
// | escrow | `EscrowError` | 400..=999 |
// | permission | `PermissionError` | 1_000..=1_999 |
// | reputation | `ReputationError` | 2_000..=2_999 |
// | delegation | `DelegationError` | 3_000..=3_999 |
// | marketplace | `MarketplaceError` | 4_000..=4_999 |
// The 0.x codes in the registry above are frozen legacy codes; they predate
// this table. New `EscrowError` variants MUST use `400..=999` (or the 1.0
// renumbered range) and MUST NOT use another contract's range.
// New variants MUST use codes in the escrow allocation (`400..=999`) and
// MUST NOT use another contract's range. Do not fill historical gaps or
// reuse codes from the registry above.
//
// # Renumber plan
//
// The next `ContractVersion` major bump (1.0.0) is the planned breaking
// release for renumbering `EscrowError` contiguously from 1 to N, removing
// gaps and sorting declaration order by code. Until that release, the codes
// in the registry above are stable.
// release for renumbering `EscrowError` contiguously inside the escrow
// allocation range (`400..=999`), removing gaps and sorting declaration
// order by code. Until that release, the codes in the registry above are
// stable.
//
// `export = false` (on the attribute above) skips the XDR contract-spec entry
// for this enum. The spec format caps error enums at 50 cases and this enum
// has grown past that, so generating the spec panics at compile time. Error
// codes themselves are unchanged and still surface to callers; only the
// self-describing spec metadata for the error enum is omitted.
/// Canonical ABI error codes for the escrow contract; see the registry above.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum EscrowError {
    /// Contract already initialized
    AlreadyInitialized = 1,
    /// Escrow record not found
    NotFound = 2,
    /// Caller is not authorized for this operation
    Unauthorized = 3,
    /// Escrow has already been released
    AlreadyReleased = 4,
    /// Escrow has already been refunded
    AlreadyRefunded = 5,
    /// Escrow is not in Funded status
    InvalidStatus = 6,
    /// Refund timeout has not been reached
    TimeoutNotReached = 7,
    /// Escrow is not in Disputed status
    NotDisputed = 8,
    /// Invalid amount (zero or negative)
    InvalidAmount = 9,
    /// Address is zero or invalid
    InvalidAddress = 32,
    /// Buyer and seller must be distinct
    InvalidEscrowParticipants = 33,
    /// Token is not approved for escrow
    TokenNotWhitelisted = 10,
    /// Release amount exceeds balance
    InsufficientEscrowBalance = 11,
    /// Release amount is zero
    ZeroAmount = 12,
    /// No pending admin transfer exists
    NoPendingTransfer = 13,
    /// Caller is not the pending admin
    InvalidPendingAdmin = 14,
    /// Admin already exists
    AdminAlreadyExists = 15,
    /// Fee BPS exceeds maximum
    InvalidFeeBps = 16,
    /// Amount is below the minimum allowed
    AmountBelowMin = 17,
    /// Amount is above the maximum allowed
    AmountAboveMax = 18,
    /// Invalid limits (min <= 0 or max < min)
    InvalidLimits = 19,
    /// Not an authorized arbiter
    NotAnArbiter = 20,
    /// Arbiter has already voted
    AlreadyVoted = 21,
    /// Invalid quorum threshold
    InvalidQuorum = 22,
    /// Quorum not yet reached
    QuorumNotReached = 23,
    /// Quorum config not set
    QuorumConfigNotSet = 24,
    /// Conflicting quorum outcomes
    ConflictingQuorum = 25,
    /// Release recipient mismatch
    InvalidReleaseRecipient = 201,
    /// Escrow creation is paused
    CreationPaused = 26,
    /// Escrow has already been cancelled
    AlreadyCancelled = 27,
    /// Escrow has already been funded
    AlreadyFunded = 28,
    /// Extension length must be > 0
    InvalidExtension = 29,
    /// No liquidity pool for token
    PoolNotFound = 30,
    /// Pool balance is insufficient
    InsufficientPoolBalance = 31,
    /// Release condition not set
    ReleaseConditionNotSet = 36,
    /// Oracle call failed
    OracleCallFailed = 37,
    /// Oracle condition was not met
    ConditionNotMet = 38,
    /// Invalid yield configuration
    InvalidYieldConfig = 39,
    /// Amount limits not configured
    AmountLimitsNotSet = 40,
    /// Fee config not set
    FeeConfigNotSet = 41,
    /// Merchant is suspended or closed
    MerchantNotTrading = 43,
    /// Merchant status check failed
    MerchantStatusCheckFailed = 44,
    /// Delivery proof required
    SignedProofRequired = 45,
    /// Invalid signed delivery proof
    InvalidSignedDeliveryProof = 46,
    /// Oracle public key not set
    OraclePublicKeyNotSet = 47,
    /// No metadata stored at creation
    MetadataNotSet = 400,
    /// Invalid metadata payload
    InvalidMetadata = 401,
    /// Invalid Merkle proof
    InvalidMerkleProof = 407,
    /// Invalid dispute award
    InvalidDisputeAward = 410,
    /// Cross-currency swap configuration is missing required fields, reuses
    /// the escrow's deposit token as the payout token, or sets a slippage
    /// bound above `MAX_SWAP_SLIPPAGE_BPS` (issue #318).
    InvalidSwapConfig = 411,
    /// `release_with_swap` was called on an escrow with no
    /// `CrossCurrencySwapConfig` set via `set_cross_currency_swap_config`.
    SwapConfigNotSet = 412,
    /// The router returned (or would have returned) less than the
    /// slippage-bounded minimum acceptable payout amount.
    SlippageExceeded = 413,
    /// The configured swap router contract call failed or trapped.
    SwapRouterCallFailed = 414,
    /// A re-entrant invocation was attempted while an external call was in
    /// flight (issue #334).
    ReentrancyDetected = 415,
    /// Appeal filing window has expired (issue #354).
    AppealWindowExpired = 416,
    /// An appeal has already been filed for this dispute (issue #354).
    AppealAlreadyFiled = 417,
    /// No appeal found for this escrow (issue #354).
    NoAppealFound = 418,
    /// Caller is not the appeals council (issue #354).
    NotAppealsCouncil = 419,
    /// Escrow dispute is not in InitialRuling status (issue #354).
    DisputeNotInitialRuling = 420,
    /// Appeal bond amount is below minimum required (issue #354).
    AppealBondInsufficient = 421,
    /// Arbiter staked amount must be greater than zero
    ArbiterStakedAmountZero = 422,
    /// Dispute has not yet expired
    DisputeNotExpired = 423,
    /// Arbiter is not assigned to this dispute
    ArbiterNotAssigned = 424,
    /// No slippage window has been committed for this escrow's path payment
    /// (issue #337).
    SlippageBoundsNotSet = 425,
    /// A slippage bound is zero or negative (issue #337).
    InvalidSlippageBounds = 426,
    /// A committed slippage window may only be tightened, never widened
    /// (issue #337).
    SlippageBoundsTooLoose = 427,
    /// The submitted cross-currency route is empty, too long, self-converting,
    /// or does not chain its tokens from the escrowed token to a different
    /// destination token (issue #337).
    InvalidPathRoute = 428,
    /// The DEX router call failed or returned an unusable result (issue #337).
    PathExecutionFailed = 429,
    /// External oracle invocation failed (panic, revert, or paused oracle).
    OracleInvocationFailed = 1019,
    /// A volume fee tier is malformed (fee_bps exceeds the 10% cap)
    InvalidTier = 430,
    /// The volume fee tier table is full; no further tiers can be added
    TierLimitExceeded = 431,
    /// No volume fee tier is configured for the given merchant
    TierNotFound = 432,
    /// A pending multi-sig upgrade proposal already exists
    UpgradeProposalExists = 433,
    /// No pending multi-sig upgrade proposal exists
    UpgradeProposalNotFound = 434,
    /// The wasm hash of a pending upgrade proposal does not match
    UpgradeHashMismatch = 435,
    /// The upgrade timelock has not elapsed
    UpgradeTimelockActive = 436,
    /// Merchant's category is not in the authorized set
    MerchantCategoryNotAllowed = 437,
    /// The secondary-approver deadline passed before the approval was recorded.
    ApproverDeadlineExpired = 438,
    /// The secondary-approver deadline has not been reached yet.
    ApproverDeadlineNotReached = 439,
    /// Dual-control fallback action must be `Disputed` or `Refunded`.
    InvalidFallbackAction = 440,
    /// No secondary-approver deadline is configured for this escrow.
    DualControlTimeoutNotConfigured = 441,
    /// The secondary approver already signed, so no fallback is due.
    DualControlAlreadyApproved = 442,
    /// A terminal escrow has not been settled for long enough to archive.
    ArchivalRetentionNotElapsed = 443,
    /// Escrow is not in Inspection status for this operation.
    NotInInspection = 444,
    /// Inspection period has already expired; cannot file dispute.
    InspectionExpired = 445,
    /// Inspection configuration not set for this escrow.
    InspectionConfigNotSet = 446,
    /// Inspection auto-release ledger has not been reached yet.
    InspectionAutoReleaseNotReady = 447,
    /// Voucher nonce was already consumed.
    TimeoutExtensionNonceUsed = 448,
    /// Voucher signatures did not identify both escrow participants.
    InvalidTimeoutExtensionVoucher = 449,
}

/// Runs `f` under a re-entrancy lock and returns its result unchanged.
///
/// Soroban discards every storage write made during an invocation once the
/// top-level entry point returns `Err`, so an external call that fails only
/// leaves escrow storage pristine if the error is propagated rather than
/// swallowed. This helper enforces both halves of that guarantee (issue #334):
///
/// * it takes an instance-storage re-entrancy lock before calling `f`, so a
///   downstream contract invoked from inside `f` cannot call back into the
///   escrow and act on state that has only been half-applied;
/// * it returns `f`'s `Result` unchanged, so a failure propagates out of the
///   entry point and the host rolls the partial writes back.
///
/// The lock is always released before returning. A nested call while the lock
/// is held fails fast with [`EscrowError::ReentrancyDetected`].
pub fn execute_atomic_operation<F, T>(env: &Env, f: F) -> Result<T, EscrowError>
where
    F: FnOnce() -> Result<T, EscrowError>,
{
    let storage = env.storage().instance();
    let locked: bool = storage.get(&DataKey::ReentrancyGuard).unwrap_or(false);
    if locked {
        return Err(EscrowError::ReentrancyDetected);
    }
    storage.set(&DataKey::ReentrancyGuard, &true);

    let result = f();

    // Release the lock. When `result` is `Err`, this write is rolled back along
    // with the rest of the invocation; when it is `Ok`, the contract is left
    // re-usable by the next call.
    storage.remove(&DataKey::ReentrancyGuard);
    result
    /// An unexecuted upgrade proposal is already pending
    UpgradeProposalExists = 411,
    /// No pending upgrade proposal exists
    UpgradeProposalNotFound = 412,
    /// The upgrade timelock has not elapsed yet
    UpgradeTimelockActive = 413,
    /// The supplied wasm hash does not match the pending proposal
    UpgradeHashMismatch = 414,
    /// Checked arithmetic overflowed while updating escrow disbursements
    MathOverflow = 415,
    /// Cumulative releases and refunds would exceed the escrowed principal
    ExceedsTotalEscrowAmount = 416,
    /// Seller merchant category is not in the authorized category list
    MerchantCategoryNotAllowed = 417,
    /// A sub-order line item has already been resolved
    SubItemAlreadyResolved = 418,
}

/// Manual, spec-free equivalent of the impls generated by `#[contracterror]`.
///
/// See the note above [`EscrowError`]: the on-chain error spec is limited to 50
/// cases, so the conversions are written out here and the spec entry is omitted.
macro_rules! impl_escrow_error_try_from {
    ($($code:literal => $variant:ident),* $(,)?) => {
        impl TryFrom<soroban_sdk::Error> for EscrowError {
            type Error = soroban_sdk::Error;
            #[inline(always)]
            fn try_from(error: soroban_sdk::Error) -> Result<Self, Self::Error> {
                if error.is_type(soroban_sdk::xdr::ScErrorType::Contract) {
                    let discriminant = error.get_code();
                    Ok(match discriminant {
                        $( $code => Self::$variant, )*
                        _ => return Err(error),
                    })
                } else {
                    Err(error)
                }
            }
        }

        impl TryFrom<soroban_sdk::InvokeError> for EscrowError {
            type Error = soroban_sdk::InvokeError;
            #[inline(always)]
            fn try_from(error: soroban_sdk::InvokeError) -> Result<Self, Self::Error> {
                match error {
                    soroban_sdk::InvokeError::Abort => Err(error),
                    soroban_sdk::InvokeError::Contract(discriminant) => Ok(match discriminant {
                        $( $code => Self::$variant, )*
                        _ => return Err(error),
                    }),
                }
            }
        }
    };
}

impl_escrow_error_try_from! {
    1 => AlreadyInitialized,
    2 => NotFound,
    3 => Unauthorized,
    4 => AlreadyReleased,
    5 => AlreadyRefunded,
    6 => InvalidStatus,
    7 => TimeoutNotReached,
    8 => NotDisputed,
    9 => InvalidAmount,
    32 => InvalidAddress,
    33 => InvalidEscrowParticipants,
    10 => TokenNotWhitelisted,
    11 => InsufficientEscrowBalance,
    12 => ZeroAmount,
    13 => NoPendingTransfer,
    14 => InvalidPendingAdmin,
    15 => AdminAlreadyExists,
    16 => InvalidFeeBps,
    17 => AmountBelowMin,
    18 => AmountAboveMax,
    19 => InvalidLimits,
    20 => NotAnArbiter,
    21 => AlreadyVoted,
    22 => InvalidQuorum,
    23 => QuorumNotReached,
    24 => QuorumConfigNotSet,
    25 => ConflictingQuorum,
    201 => InvalidReleaseRecipient,
    26 => CreationPaused,
    27 => AlreadyCancelled,
    28 => AlreadyFunded,
    29 => InvalidExtension,
    30 => PoolNotFound,
    31 => InsufficientPoolBalance,
    36 => ReleaseConditionNotSet,
    37 => OracleCallFailed,
    38 => ConditionNotMet,
    39 => InvalidYieldConfig,
    40 => AmountLimitsNotSet,
    41 => FeeConfigNotSet,
    43 => MerchantNotTrading,
    44 => MerchantStatusCheckFailed,
    45 => SignedProofRequired,
    46 => InvalidSignedDeliveryProof,
    47 => OraclePublicKeyNotSet,
    48 => DualControlNotConfigured,
    49 => SecondaryApprovalRequired,
    42 => MaxTreasuriesExceeded,
    400 => MetadataNotSet,
    401 => InvalidMetadata,
    402 => BumpRateLimitExceeded,
    403 => BumpThresholdNotMet,
    404 => ShipmentProofNotFound,
    405 => FeeUpdateNotEffective,
    406 => FeeNoticeWindowNotMet,
    407 => InvalidMerkleProof,
    408 => MerkleRootAlreadyPublished,
    409 => BatchLimitExceeded,
    410 => InvalidDisputeAward,
    411 => UpgradeProposalExists,
    412 => UpgradeProposalNotFound,
    413 => UpgradeTimelockActive,
    414 => UpgradeHashMismatch,
    415 => MathOverflow,
    416 => ExceedsTotalEscrowAmount,
    417 => MerchantCategoryNotAllowed,
    418 => SubItemAlreadyResolved,
}

impl TryFrom<&soroban_sdk::Error> for EscrowError {
    type Error = soroban_sdk::Error;
    #[inline(always)]
    fn try_from(error: &soroban_sdk::Error) -> Result<Self, Self::Error> {
        <_ as TryFrom<soroban_sdk::Error>>::try_from(*error)
    }
}

impl From<EscrowError> for soroban_sdk::Error {
    #[inline(always)]
    fn from(val: EscrowError) -> soroban_sdk::Error {
        <_ as From<&EscrowError>>::from(&val)
    }
}

impl From<&EscrowError> for soroban_sdk::Error {
    #[inline(always)]
    fn from(val: &EscrowError) -> soroban_sdk::Error {
        soroban_sdk::Error::from_contract_error(*val as u32)
    }
}

impl TryFrom<&soroban_sdk::InvokeError> for EscrowError {
    type Error = soroban_sdk::InvokeError;
    #[inline(always)]
    fn try_from(error: &soroban_sdk::InvokeError) -> Result<Self, Self::Error> {
        <_ as TryFrom<soroban_sdk::InvokeError>>::try_from(*error)
    }
}

impl From<EscrowError> for soroban_sdk::InvokeError {
    #[inline(always)]
    fn from(val: EscrowError) -> soroban_sdk::InvokeError {
        <_ as From<&EscrowError>>::from(&val)
    }
}

impl From<&EscrowError> for soroban_sdk::InvokeError {
    #[inline(always)]
    fn from(val: &EscrowError) -> soroban_sdk::InvokeError {
        soroban_sdk::InvokeError::Contract(*val as u32)
    }
}

impl soroban_sdk::TryFromVal<soroban_sdk::Env, soroban_sdk::Val> for EscrowError {
    type Error = soroban_sdk::ConversionError;
    #[inline(always)]
    fn try_from_val(env: &soroban_sdk::Env, val: &soroban_sdk::Val) -> Result<Self, Self::Error> {
        use soroban_sdk::TryIntoVal;
        let error: soroban_sdk::Error = val.try_into_val(env)?;
        error.try_into().map_err(|_| soroban_sdk::ConversionError)
    }
}

impl soroban_sdk::TryFromVal<soroban_sdk::Env, EscrowError> for soroban_sdk::Val {
    type Error = soroban_sdk::ConversionError;
    #[inline(always)]
    fn try_from_val(_env: &soroban_sdk::Env, val: &EscrowError) -> Result<Self, Self::Error> {
        let error: soroban_sdk::Error = val.into();
        Ok(error.into())
    }
    /// Emergency rescue proposal not found for the given escrow.
    EmergencyRescueProposalNotFound = 411,
    /// Emergency rescue timelock period has not yet elapsed.
    EmergencyRescueTimelockNotElapsed = 412,
    /// Multi-sig approval threshold not reached for emergency rescue.
    EmergencyRescueThresholdNotMet = 413,
    /// Emergency rescue has already been executed for this escrow.
    EmergencyRescueAlreadyExecuted = 414,
    /// Escrow is not eligible for emergency rescue (must be in Funded or Disputed status).
    EscrowNotEligibleForRescue = 415,
}

/// Errors returned by the multi-oracle consensus path (issue #352).
///
/// Codes follow the issue specification (1051+). Because this is a separate
/// contract-error enum it does not consume the escrow `EscrowError` code space.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MultiOracleError {
    /// Not enough distinct affirmative attestations to release the escrow.
    ThresholdNotMet = 1051,
    /// The oracle has already attested for this escrow.
    DuplicateOracleVote = 1052,
    /// The caller is not an authorized oracle for this escrow.
    UnauthorizedOracle = 1053,
    /// The consensus window has elapsed; no further attestations are accepted.
    ConsensusWindowExpired = 1054,
    /// The escrow could not be found.
    EscrowNotFound = 1055,
    /// No multi-oracle configuration has been set for this escrow.
    ConfigNotSet = 1056,
    /// The escrow is not in a state that allows consensus release.
    InvalidStatus = 1057,
    /// The release triggered by consensus failed.
    ReleaseFailed = 1058,
    /// Milestone schedule is empty, too long, has duplicate ids, non-positive
    /// amounts, pre-completed entries, or an overflowing total.
    InvalidMilestoneSchedule = 411,
    /// Escrow has no milestone with the requested id (or is not a milestone escrow).
    MilestoneNotFound = 412,
    /// Milestone has already been paid out.
    MilestoneAlreadyReleased = 413,
    /// Milestone escrows must be paid out through `release_milestone`.
    MilestoneReleaseRequired = 414,
    /// Configure a guardian before proposing critical changes.
    GuardianNotSet = 411,
    /// Guardian bootstrap may only run once.
    GuardianAlreadySet = 412,
    /// No live proposal matches the requested action.
    AdminActionNotFound = 413,
    /// Both review thresholds must elapse before execution.
    AdminActionLocked = 414,
    /// The security guardian vetoed this proposal.
    AdminActionVetoed = 415,
    /// An identical proposal is already pending.
    AdminActionAlreadyQueued = 416,
    /// Proposal ID or review deadline would overflow.
    AdminActionOverflow = 417,
    /// A fee or yield calculation exceeded the supported integer range.
    MathOverflow = 411,
    /// A release entry point was re-entered while an external call was active.
    ReentrancyDetected = 412,
    /// Net tokens received after transfer are less than the requested deposit
    /// amount. Occurs with fee-on-transfer or deflationary tokens where the
    /// balance delta is smaller than the nominal amount passed to transfer().
    DepositUnderfunded = 411,
    /// Arithmetic overflow when computing the deposit balance delta.
    MathOverflow = 412,
    /// A unilateral seller cancel is blocked by the front-running protection
    /// window: the buyer has not agreed to the cancellation and the escrow's
    /// timeout has not been reached (issue #355).
    CancelLockoutActive = 411,
    /// Requested cancel lockout window is above `MAX_CANCEL_LOCKOUT_LEDGERS`
    /// (issue #355).
    InvalidCancelLockout = 412,
    /// A fee calculation overflowed `i128` (issue #362).
    MathOverflow = 411,
    /// The requested minimum fee floor is negative or above
    /// [`MAX_MIN_FEE_STROOPS`] (issue #362).
    InvalidMinFee = 412,
}

/// Compact receipt returned to buyers after escrow creation via `get_receipt`.
///
/// Fields are a purposeful subset of `EscrowRecord` — callers that need the
/// full record (amount, token, timeout, …) should use `get_escrow` instead.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowReceipt {
    /// Numeric escrow identifier (matches the value returned by `deposit`).
    pub escrow_id: u64,
    /// Address of the buyer who funded the escrow.
    pub buyer: Address,
    /// Address of the seller (merchant) who will receive the released funds.
    pub seller: Address,
    /// Merchant-facing order reference embedded at deposit time.
    pub order_id: BytesN<32>,
    /// Current lifecycle status of the escrow.
    pub status: EscrowStatus,
}

/// Merchant-facing receipt for dashboards and settlement checks (issue #171).
///
/// `escrow_id` is the 32-byte order id (same correlation key as
/// [`ReleaseEligibility`]). `release_eligible` is computed read-only from
/// the current status and timeout without mutating state.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantEscrowReceipt {
    pub escrow_id: BytesN<32>,
    pub merchant: Address,
    pub buyer: Address,
    pub status: EscrowStatus,
    pub release_eligible: bool,
}

/// Refund eligibility result returned by `get_refund_eligibility` (issue #173).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundEligibility {
    pub escrow_id: u64,
    pub eligible: bool,
    pub reason: Symbol,
}

/// One rung of the volume-based platform fee ladder (issue #328).
///
/// A merchant whose trailing 30-day settled volume is at or above
/// `min_volume` pays `fee_bps` instead of the base `FeeConfig::fee_bps`.
/// Tiers are ordered by `min_volume`; the highest tier whose threshold is
/// met wins.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeTier {
    /// Minimum trailing 30-day settled volume (inclusive) for this tier.
    pub min_volume: i128,
    /// Platform fee in basis points charged at this tier (e.g., 200 = 2%).
    pub fee_bps: u32,
}

/// Emitted when a merchant's settled volume first reaches a higher fee tier
/// (issue #328). Lets off-chain services react to the discount without
/// replaying release events.
#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantVolumeTierUpdatedEvent {
    /// Merchant (seller) that achieved the new tier.
    pub merchant: Address,
    /// New tier's minimum-volume threshold.
    pub min_volume: i128,
    /// New tier's fee in basis points.
    pub fee_bps: u32,
    /// Merchant's total settled volume at the time of the upgrade.
    pub total_settled_volume: i128,
}

/// Release eligibility result returned by `get_release_eligibility`.
///
/// `escrow_id` mirrors the escrow record's 32-byte order id so settlement
/// workers can correlate the read-only response with their backend job.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseEligibility {
    pub escrow_id: BytesN<32>,
    pub eligible: bool,
    pub reason: Symbol,
}

/// Cancellation eligibility result returned by `get_cancel_eligibility`
/// (issue #355).
///
/// The answer is derived purely from stored state plus the current ledger, so
/// a client can predict `cancel`'s outcome before submitting a transaction and
/// cannot be tricked into a different result by transaction ordering. `reason`
/// is one of: `ok`, `notfound`, `notseller`, `funded`, `cancelled`,
/// `badstate`, `timeout` (timeout reached — cancellation is allowed), `agreed`
/// (buyer agreed — cancellation is allowed) or `lockout` (protection window is
/// still running — the seller must wait or obtain the buyer's agreement).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancelEligibility {
    pub escrow_id: u64,
    pub eligible: bool,
    pub reason: Symbol,
}

/// Read-only timeout metadata for a single escrow (issue #88).
///
/// Returned by [`EscrowContract::get_timeout_view`].  All fields are
/// derived from stored state and the current ledger sequence — the getter
/// never mutates contract storage.
///
/// # Fields
/// - `escrow_id`      — Numeric escrow identifier (matches `EscrowRecord.escrow_id`).
/// - `timeout_ledger` — Ledger sequence at which the buyer-refund timeout expires.
/// - `current_ledger` — Ledger sequence at the time this getter was invoked.
/// - `refundable`     — `true` when `current_ledger >= timeout_ledger` **and** the
///                      escrow is still in `Funded` status (buyer may refund).
///                      `false` for terminal states (Released / Refunded) or when
///                      the timeout has not yet been reached.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowTimeoutView {
    pub escrow_id: BytesN<32>,
    pub timeout_ledger: u32,
    pub current_ledger: u32,
    pub refundable: bool,
}

/// Complete read-only state snapshot for an escrow, returned by
/// `get_escrow_snapshot` (issue #329) so off-chain indexers can audit an
/// escrow's full state in a single call instead of replaying its event
/// history.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowSnapshot {
    pub record: EscrowRecord,
    pub fee_config: FeeConfig,
    pub current_ledger: u32,
    pub timed_out: bool,
    pub release_eligible: bool,
}

/// Monotonic yield snapshot returned by [`EscrowContract::get_accrued_yield`]
/// (issue #34).
///
/// All inputs needed for display are included so polling UIs cannot observe
/// decreasing yield.  `snapshot_ledger` anchors the read to a block-stable
/// point; `held_seconds` is computed from `created_at` to that ledger's
/// timestamp, making the result deterministic within a single ledger close.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldView {
    pub escrow_id: u64,
    pub principal: i128,
    pub apy_bps: u32,
    pub held_seconds: u64,
    pub accrued: i128,
    pub snapshot_ledger: u32,
}

/// Compact read-only escrow summary for API/indexer consumers (issue #90).
///
/// Returned by [`EscrowContract::get_escrow_summary`]. Contains all display
/// fields needed by the backend event indexer in a single call. The response
/// shape is identical for both terminal and non-terminal escrows — callers
/// do not need to branch on status to decode the result.
///
/// # Fields
/// - `escrow_id` — 32-byte order identifier (same correlation key used by
///   [`ReleaseEligibility`] and [`MerchantEscrowReceipt`]).
/// - `buyer`     — Address of the buyer who funded the escrow.
/// - `merchant`  — Address of the seller/merchant who receives released funds.
/// - `amount`    — Total escrowed token amount.
/// - `status`    — Current lifecycle status of the escrow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowSummary {
    pub escrow_id: BytesN<32>,
    pub buyer: Address,
    pub merchant: Address,
    pub amount: i128,
    pub status: EscrowStatus,
}

/// Paginated result returned by `list_escrows` and `list_escrows_by_buyer`
/// (issue #49). `items` contains up to `limit` escrow records starting at
/// `offset`. `total` is the total number of escrows in the queried index.
/// `next_offset` is `Some(offset + items.len())` when more records follow,
/// or `None` when the last page has been reached.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowListPage {
    pub items: soroban_sdk::Vec<EscrowRecord>,
    pub total: u32,
    pub next_offset: Option<u32>,
}

/// Maximum number of escrows returned in a single page from `list_escrows`
/// or `list_escrows_by_buyer`. Callers requesting a larger `limit` will
/// silently receive at most this many records.
pub const MAX_PAGE_LIMIT: u32 = 50;

/// Seconds in a 365-day year, used to prorate `YieldConfig::apr_bps` down to
/// the actual holding period of an escrow.
const SECONDS_PER_YEAR: i128 = 31_536_000;

/// Calculate a basis-point amount without allowing intermediate overflow.
fn calculate_fee_and_yield(amount: i128, bps: u32) -> Result<i128, EscrowError> {
    amount
        .checked_mul(bps as i128)
        .and_then(|product| product.checked_div(10_000))
        .ok_or(EscrowError::MathOverflow)
}

fn calculate_yield(amount: i128, bps: u32, held_seconds: u64) -> Result<i128, EscrowError> {
    let denominator = 10_000i128
        .checked_mul(SECONDS_PER_YEAR)
        .ok_or(EscrowError::MathOverflow)?;
    amount
        .checked_mul(bps as i128)
        .and_then(|product| product.checked_mul(held_seconds as i128))
        .and_then(|product| product.checked_div(denominator))
        .ok_or(EscrowError::MathOverflow)
}

/// Maximum number of treasury rows accepted by `set_fee_distribution`.
/// Keeps the fee-splitting loop bounded and prevents unbounded config growth.
const MAX_TREASURIES: u32 = 10;

/// Persistent TTL bump parameters (mirrors `marketplace`/`reputation`): any
/// entry whose remaining TTL is below the threshold is extended out to ~30
/// days of ledgers. Escrow records are long-lived by design — an open escrow
/// must not be evicted while funds are still locked.
const PERSISTENT_BUMP_THRESHOLD: u32 = 17_280; // ~1 day of ledgers (5s/ledger)
const PERSISTENT_BUMP_AMOUNT: u32 = 518_400; // ~30 days of ledgers

/// Minimum ledgers between bump_ttl_with_bounty calls to prevent bounty draining.
const BUMP_RATE_LIMIT_LEDGERS: u32 = 100;
/// TTL threshold (in ledgers) from expiration to qualify for bounty payout.
const BUMP_BOUNTY_THRESHOLD_LEDGERS: u32 = 17_280; // ~1 day
/// Keeper bounty amount instroked (small amount to incentivize maintenance).
const KEEPER_BOUNTY_AMOUNT: i128 = 1_000_000; // 1 XLM equivalent (assuming 7 decimal tokens)
/// Delay between an upgrade proposal and its earliest execution (48 hours).
pub const UPGRADE_TIMELOCK_SECS: u64 = 172_800;
/// Minimum (and default) number of admin approvals required to upgrade, so a
/// single compromised key can never replace contract code on its own.
pub const MIN_UPGRADE_THRESHOLD: u32 = 2;
/// Number of ledgers a terminal escrow's persistent entries are retained after
/// the escrow settles, before `archive_terminal_escrow` may reclaim them
/// (issue #331). 518_400 ledgers at the network's nominal 5s close time is
/// ~30 days — long enough for every dispute window, refund claim, and
/// reconciliation job to have read the record, while still bounding the rent a
/// dead escrow can pin down.
pub const ARCHIVAL_RETENTION_LEDGERS: u32 = 518_400;
/// Nominal seconds between ledger closes, used to convert
/// [`ARCHIVAL_RETENTION_LEDGERS`] into a wall-clock retention window.
const SECONDS_PER_LEDGER: u64 = 5;

/// Mandatory window after `timeout_ledger` during which a seller/admin
/// timeout-based claim (`refund`/`partial_refund` called by the seller with a
/// recorded shipment proof) is blocked, guaranteeing the buyer a window to
/// raise a dispute without being front-run the instant the timeout is
/// reached (issue #284). ~24 hours at a 5s average ledger close time.
pub const DISPUTE_GRACE_PERIOD_LEDGERS: u32 = 17_280;

/// Maximum number of entries in the volume fee tier table (issue #328).
/// Keeps the tier lookup bounded; tiers must fit in a single ledger entry.
pub const MAX_FEE_TIERS: u32 = 5;

/// Maximum fee any single volume fee tier may charge, matching the 10% cap
/// enforced for `FeeConfig::fee_bps` and `TreasuryShare::bps` (issue #328).
pub const MAX_TIER_FEE_BPS: u32 = 1000;

/// Length of the trailing window, in seconds, over which a merchant's
/// settled volume is measured for fee tiering (issue #328).
pub const VOLUME_WINDOW_SECS: u64 = 2_592_000; // 30 days

/// Whether the volume fee tier table can be mutated after initialization.
/// Tier updates are admin-only; the flag exists so the future "locked"
/// lifecycle has a storage slot to write (issue #328).
pub const TIER_LOCK_UNSET: u32 = 0;
/// Delay between an emergency rescue proposal and its earliest execution (14 days).
pub const EMERGENCY_RESCUE_TIMELOCK_SECS: u64 = 1_209_600;

/// Default front-running protection window for seller cancellation, in ledgers
/// (issue #355).
///
/// A contract cannot observe the mempool, so the only deterministic way to
/// stop a seller from cancelling an order a buyer is about to fund is to make
/// the freshly created (or freshly accepted) order non-cancellable for a
/// minimum number of ledgers. At Stellar's ~5 s ledger time, 10 ledgers is
/// roughly one minute — long enough for a `fund`/`deposit` transaction that is
/// already in flight to land first, short enough that a genuinely stalled order
/// can still be cleaned up by its seller or by mutual agreement.
pub const DEFAULT_CANCEL_LOCKOUT_LEDGERS: u32 = 10;

/// Upper bound for the admin-configurable cancel protection window
/// (issue #355). Keeps a mis-configuration from parking escrows in an
/// effectively permanent `Created` state.
pub const MAX_CANCEL_LOCKOUT_LEDGERS: u32 = 1_000;

/// Sentinel "first ledger at which cancellation is allowed" used when the
/// protection state for an escrow cannot be read. The guard fails closed:
/// a seller must obtain the buyer's agreement (or wait for the timeout)
/// instead of cancelling on a state the contract cannot verify.
const CANCEL_LOCKOUT_NEVER: u32 = u32::MAX;

/// Why a unilateral seller `cancel` cannot proceed right now (issue #355).
///
/// The variants map one-to-one onto the errors `cancel` returns and onto the
/// reason symbols reported by `get_cancel_eligibility`, so the read-only view
/// and the state-changing call can never disagree.
enum CancelBlock {
    /// The escrow is already funded, so the buyer's deposit is on-chain.
    Funded,
    /// The escrow is already cancelled.
    Cancelled,
    /// The escrow is in a state that is not cancellable at all.
    InvalidStatus,
    /// The protection window is still running and no mutual agreement or
    /// timeout applies.
    Lockout,
}

impl CancelBlock {
    /// The typed error `cancel` returns for this block reason.
    fn into_error(self) -> EscrowError {
        match self {
            CancelBlock::Funded => EscrowError::AlreadyFunded,
            CancelBlock::Cancelled => EscrowError::AlreadyCancelled,
            CancelBlock::InvalidStatus => EscrowError::InvalidStatus,
            CancelBlock::Lockout => EscrowError::CancelLockoutActive,
        }
    }

    /// The reason symbol reported by `get_cancel_eligibility`.
    fn reason(&self) -> Symbol {
        match self {
            CancelBlock::Funded => symbol_short!("funded"),
            CancelBlock::Cancelled => symbol_short!("cancelled"),
            CancelBlock::InvalidStatus => symbol_short!("badstate"),
            CancelBlock::Lockout => symbol_short!("lockout"),
        }
    }
}

/// Snapshot of the cancellation guard for a single escrow (issue #355).
struct CancelGuard {
    /// Acceptance + lockout configuration snapshotted for the escrow.
    acceptance: OrderAcceptanceState,
    /// First ledger at which a unilateral cancel is permitted.
    allowed_ledger: u32,
    /// Whether the buyer has agreed to a cancellation.
    buyer_agreed: bool,
}

/// Denominator for every basis-points fee computation in this contract.
pub const BPS_DENOMINATOR: i128 = 10_000;

/// Default minimum release fee, in the token's smallest unit (stroop), used
/// when no admin override is configured (issue #362).
///
/// One stroop is the smallest fee the contract can charge, and it is the
/// smallest floor that still makes the fee non-zero: integer division
/// truncates `amount * fee_bps / 10_000` to `0` for every `amount` below
/// `10_000 / fee_bps` stroops, so without a floor a seller can fragment a large
/// payment into dust escrows and pay no platform fee at all. The floor is always
/// clamped to the amount being charged, so a settlement is never over-charged
/// either, and it is never applied when the platform charges no fee at all
/// (`fee_bps == 0`).
pub const DEFAULT_MIN_FEE_STROOPS: i128 = 1;

/// Upper bound for the admin-configurable minimum fee floor (issue #362).
///
/// The floor is clamped to the released amount, so this is purely a guard rail
/// against a mis-configuration that would swallow most of a small settlement.
/// `0` is allowed and restores the pure pro-rata behaviour.
pub const MAX_MIN_FEE_STROOPS: i128 = 1_000_000;

/// Applies the minimum fee floor to an already computed fee (issue #362).
///
/// Returns `min_fee_stroops.min(amount)` when the pro-rata fee came out below
/// the floor, so the fee is never zero and never larger than the amount it is
/// deducted from. The floor is skipped for non-positive amounts and for a `0`
/// bps configuration, which is an explicit "the platform charges no fee"
/// setting rather than a fee that truncation erased.
fn apply_fee_floor(amount: i128, fee_bps: u32, fee: i128, min_fee_stroops: i128) -> i128 {
    if fee_bps > 0 && amount > 0 && fee < min_fee_stroops {
        min_fee_stroops.min(amount)
    } else {
        fee
    }
}

/// Overflow-free `amount * bps / BPS_DENOMINATOR`.
///
/// Splits the amount into whole and fractional basis-point units so the
/// intermediate products cannot exceed `i128`, which keeps large escrows
/// feeable instead of overflowing into [`EscrowError::MathOverflow`].
fn bps_fee(amount: i128, bps: u32) -> i128 {
    (amount / BPS_DENOMINATOR) * bps as i128
        + ((amount % BPS_DENOMINATOR) * bps as i128) / BPS_DENOMINATOR
}

/// Computes the release fee for `amount` at `fee_bps`, floored at
/// `min_fee_stroops` (issue #362).
///
/// Without the floor, `(amount * fee_bps) / 10_000` truncates to zero for every
/// `amount` below `10_000 / fee_bps` stroops, so an attacker could split a
/// single payment into arbitrarily many dust escrows and evade the platform fee
/// entirely. With the floor, every release pays at least `min_fee_stroops`
/// (clamped to `amount`), which makes fragmentation strictly more expensive than
/// settling in one go. A `fee_bps` of `0` is charged as `0` regardless of the
/// floor.
///
/// # Errors
/// Returns [`EscrowError::MathOverflow`] when `amount * fee_bps` does not fit
/// in an `i128`.
pub fn calculate_fee_with_minimum(
    amount: i128,
    fee_bps: u32,
    min_fee_stroops: i128,
) -> Result<i128, EscrowError> {
    let calculated = amount
        .checked_mul(fee_bps as i128)
        .ok_or(EscrowError::MathOverflow)?
        / BPS_DENOMINATOR;
    Ok(apply_fee_floor(
        amount,
        fee_bps,
        calculated,
        min_fee_stroops,
    ))
}

fn check_not_terminal(record: &EscrowRecord) -> Result<(), EscrowError> {
    match record.status {
        EscrowStatus::Released => Err(EscrowError::AlreadyReleased),
        EscrowStatus::Refunded => Err(EscrowError::AlreadyRefunded),
        EscrowStatus::Cancelled => Err(EscrowError::AlreadyCancelled),
        _ => Ok(()),
    }
}

/// Returns `true` once a seller/admin timeout-based claim is allowed to
/// proceed: the escrow must not be under an active dispute, and the mandatory
/// post-timeout dispute grace period must have fully elapsed. Used to close
/// the front-running window described in issue #284, where a seller could
/// otherwise claim the instant `current_ledger == timeout_ledger`, racing a
/// buyer dispute that has not yet landed.
pub fn can_seller_claim(current_ledger: u32, timeout_ledger: u32, is_disputed: bool) -> bool {
    !is_disputed && current_ledger >= timeout_ledger.saturating_add(DISPUTE_GRACE_PERIOD_LEDGERS)
}

const ZERO_ACCOUNT_STRKEY: &str = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";
const ZERO_CONTRACT_STRKEY: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";

fn is_zero_address(env: &Env, address: &Address) -> bool {
    let zero_account = Address::from_str(env, ZERO_ACCOUNT_STRKEY);
    let zero_contract = Address::from_str(env, ZERO_CONTRACT_STRKEY);
    address == &zero_account || address == &zero_contract
}

/// Reads the admin-configured volume fee tier table, ordered ascending by
/// `min_volume` (issue #328). Empty when no tiers are configured, in which
/// case the base `FeeConfig::fee_bps` applies to every merchant.
fn get_fee_tiers(env: &Env) -> Vec<FeeTier> {
    env.storage()
        .instance()
        .get(&DataKey::FeeTiers)
        .unwrap_or_else(|| Vec::new(env))
}

/// Returns the highest tier whose `min_volume` is met by `volume`, or the
/// base fee basis points from `FeeConfig` when no tier applies (issue #328).
fn effective_fee_bps(env: &Env, merchant: &Address) -> Result<u32, EscrowError> {
    let tiers = get_fee_tiers(env);
    if tiers.is_empty() {
        let fee_config: FeeConfig = env
            .storage()
            .instance()
            .get(&DataKey::FeeConfig)
            .ok_or(EscrowError::FeeConfigNotSet)?;
        return Ok(fee_config.fee_bps);
    }

    let volume: i128 = env
        .storage()
        .persistent()
        .get(&DataKey::MerchantSettledVolume(merchant.clone()))
        .unwrap_or(0);

    let mut best_bps: Option<u32> = None;
    for tier in tiers.iter() {
        if volume >= tier.min_volume {
            best_bps = Some(tier.fee_bps);
        }
    }

    match best_bps {
        Some(bps) => Ok(bps),
        None => {
            // Below every configured tier threshold: charge the base fee.
            let fee_config: FeeConfig = env
                .storage()
                .instance()
                .get(&DataKey::FeeConfig)
                .ok_or(EscrowError::FeeConfigNotSet)?;
            Ok(fee_config.fee_bps)
        }
    }
}

/// Accumulates `amount` into the merchant's settled volume and evaluates the
/// volume fee tier ladder. Emits [`MerchantVolumeTierUpdatedEvent`] the first
/// time the merchant's volume reaches a strictly higher tier than the one
/// currently recorded (issue #328). Called on every successful escrow
/// release; must never fail the release itself.
fn update_merchant_volume_and_tier(env: &Env, merchant: &Address, amount: i128) {
    let volume_key = DataKey::MerchantSettledVolume(merchant.clone());
    let volume: i128 = env.storage().persistent().get(&volume_key).unwrap_or(0);
    let new_volume = volume.saturating_add(amount);
    env.storage().persistent().set(&volume_key, &new_volume);

    let tiers = get_fee_tiers(env);
    if tiers.is_empty() {
        return;
    }

    let tier_key = DataKey::MerchantFeeTier(merchant.clone());
    let current_min_volume: Option<i128> = env.storage().persistent().get(&tier_key);

    // Highest tier whose threshold the new volume meets. Tiers are stored
    // ascending by min_volume, so the last match wins.
    let mut best: Option<FeeTier> = None;
    for tier in tiers.iter() {
        if new_volume >= tier.min_volume {
            best = Some(tier);
        }
    }

    if let Some(tier) = best {
        let achieved_higher = match current_min_volume {
            None => true,
            Some(min_volume) => tier.min_volume > min_volume,
        };
        if achieved_higher {
            env.storage().persistent().set(&tier_key, &tier.min_volume);
            env.events().publish(
                (
                    symbol_short!("escrow"),
                    symbol_short!("tier_up"),
                    merchant.clone(),
                ),
                MerchantVolumeTierUpdatedEvent {
                    merchant: merchant.clone(),
                    min_volume: tier.min_volume,
                    fee_bps: tier.fee_bps,
                    total_settled_volume: new_volume,
                },
            );
        }
    }
/// Verify the actual net token delta received by the contract after a deposit
/// transfer.
///
/// Fee-on-transfer and deflationary tokens deduct a fee during `transfer()`,
/// so the balance increase can be less than the nominal `expected_amount`.
/// This function reads the post-transfer balance, computes the delta relative
/// to `balance_before`, and returns the real amount received — or aborts with
/// a typed error when the deposit is underfunded.
fn verify_received_deposit_delta(
    env: &Env,
    token_client: &soroban_sdk::token::Client,
    contract_address: &Address,
    balance_before: i128,
    expected_amount: i128,
) -> Result<i128, EscrowError> {
    let balance_after = token_client.balance(contract_address);
    let net_received = balance_after
        .checked_sub(balance_before)
        .ok_or(EscrowError::MathOverflow)?;
    if net_received < expected_amount {
        return Err(EscrowError::DepositUnderfunded);
    }
    Ok(net_received)
}

#[contract]
pub struct EscrowContract;

// The `#[contractimpl]` macro generates client/wrapper functions that mirror
// the ABI entry-point signatures above; they cannot be annotated individually
// from user code, so the allow lives on the impl block for those generated
// wrappers only. User-defined functions carry their own scoped allows.
#[allow(clippy::too_many_arguments)]
#[contractimpl]
impl EscrowContract {
    fn enter_release(env: &Env) -> Result<(), EscrowError> {
        let guard: ReentrancyGuard = env
            .storage()
            .instance()
            .get(&DataKey::ReleaseGuard)
            .unwrap_or(ReentrancyGuard::Unlocked);
        if guard == ReentrancyGuard::Locked {
            return Err(EscrowError::ReentrancyDetected);
        }
        env.storage()
            .instance()
            .set(&DataKey::ReleaseGuard, &ReentrancyGuard::Locked);
        Ok(())
    }

    fn exit_release(env: &Env) {
        env.storage()
            .instance()
            .set(&DataKey::ReleaseGuard, &ReentrancyGuard::Unlocked);
    }

    /// Soroban constructor: initialize the escrow contract at deployment time with
    /// atomic admin + config setup. The host invokes this exactly once, so a later
    /// post-deploy `initialize` call cannot front-run a real deployer.
    pub fn __constructor(env: Env, config: EscrowConfig) -> Result<(), EscrowError> {
        Self::constructor(env, config)
    }

    /// Compatibility helper for tests and explicit deploy flows that call the
    /// function by name rather than via Soroban's auto-constructor path.
    pub fn constructor(env: Env, config: EscrowConfig) -> Result<(), EscrowError> {
        // Validate configuration
        if config.fee_bps > 1000 {
            return Err(EscrowError::InvalidFeeBps);
        }
        if config.min_amount <= 0 || config.max_amount < config.min_amount {
            return Err(EscrowError::InvalidLimits);
        }
        if is_zero_address(&env, &config.treasury) {
            return Err(EscrowError::InvalidAddress);
        }

        // Atomically store admin and configuration at deploy time
        env.storage().instance().set(&DataKey::Admin, &config.admin);
        env.storage().instance().set(&DataKey::LastEscrowId, &0u64);
        env.storage().instance().set(
            &DataKey::FeeConfig,
            &FeeConfig {
                fee_bps: config.fee_bps,
                treasury: config.treasury.clone(),
            },
        );
        env.storage().instance().set(
            &DataKey::AmountLimits,
            &EscrowAmountLimits {
                min_amount: config.min_amount,
                max_amount: config.max_amount,
            },
        );

        Ok(())
    }

    /// Initialize the escrow contract with the admin, fee config, and amount limits.
    ///
    /// # Deprecation Note
    /// For new deployments, prefer [`constructor`] which is called atomically at deploy time
    /// For new deployments, prefer [`__constructor`] which is called atomically at deploy time
    /// and cannot be front-run. This function exists for backward compatibility with legacy
    /// contracts deployed before the constructor pattern was available.
    pub fn initialize(
        env: Env,
        admin: Address,
        fee_bps: u32,
        treasury: Address,
        min_amount: i128,
        max_amount: i128,
    ) -> Result<bool, EscrowError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(EscrowError::AlreadyInitialized);
        }
        if fee_bps > 1000 {
            return Err(EscrowError::InvalidFeeBps);
        }
        if min_amount <= 0 || max_amount < min_amount {
            return Err(EscrowError::InvalidLimits);
        }
        if is_zero_address(&env, &treasury) {
            return Err(EscrowError::InvalidAddress);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::LastEscrowId, &0u64);
        env.storage()
            .instance()
            .set(&DataKey::FeeConfig, &FeeConfig { fee_bps, treasury });
        env.storage().instance().set(
            &DataKey::AmountLimits,
            &EscrowAmountLimits {
                min_amount,
                max_amount,
            },
        );
        // Keep the contract instance alive from deployment.
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
        Ok(true)
    }

    /// Return the contract name and semantic version.
    /// Callable without authentication — safe for off-chain tooling.
    pub fn version(_env: Env) -> ContractVersion {
        ContractVersion {
            name: symbol_short!("escrow"),
            semver: symbol_short!("0_2_0"),
        }
    }

    /// Read-only version check for backend services to call before
    /// interacting with the contract (issue #325). Equivalent to `version`.
    pub fn check_version(env: Env) -> ContractVersion {
        Self::version(env)
    }

    /// Return the active primary admin and any pending primary admin transfer.
    /// Callable without authentication for backend health checks and deployment
    /// verification.
    ///
    /// # Errors
    /// Returns [`EscrowError::NotFound`] when the contract has not been initialized.
    pub fn get_admin(env: Env) -> Result<AdminView, EscrowError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(EscrowError::NotFound)?;
        let pending_admin: Option<Address> = env.storage().instance().get(&DataKey::PendingAdmin);

        Ok(AdminView {
            admin,
            pending_admin,
        })
    }

    /// Set the number of admin approvals (M) required to execute an upgrade.
    /// Primary-admin only.
    ///
    /// The signer set (N) is the primary admin plus all co-admins.
    /// `threshold` must be at least [`MIN_UPGRADE_THRESHOLD`] and no greater
    /// than the current signer count. Changing the threshold while a proposal
    /// is pending is rejected so the bar cannot be lowered mid-flight.
    pub fn set_upgrade_threshold(
        env: Env,
        admin: Address,
        threshold: u32,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        let primary_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(EscrowError::NotFound)?;
        if admin != primary_admin {
            return Err(EscrowError::Unauthorized);
        }
        if threshold < MIN_UPGRADE_THRESHOLD || threshold > Self::upgrade_signer_count(&env) {
            return Err(EscrowError::InvalidQuorum);
        }
        if Self::pending_upgrade_proposal(&env).is_some() {
            return Err(EscrowError::AlreadyInitialized);
        }
        env.storage()
            .instance()
            .set(&DataKey::UpgradeThreshold, &threshold);
        Ok(true)
    }

    /// Number of admin approvals required to execute an upgrade.
    /// Defaults to [`MIN_UPGRADE_THRESHOLD`] when never configured.
    pub fn get_upgrade_threshold(env: Env) -> u32 {
        Self::upgrade_threshold(&env)
    }

    /// Return the current (pending or last executed) upgrade proposal, if any.
    pub fn get_upgrade_proposal(env: Env) -> Option<UpgradeProposal> {
        env.storage().instance().get(&DataKey::UpgradeProposal)
    }

    /// Propose upgrading the contract to `new_wasm_hash`. Admin or co-admin.
    ///
    /// Starts the [`UPGRADE_TIMELOCK_SECS`] timelock and counts as the
    /// proposer's own approval. Only one unexecuted proposal may exist at a
    /// time; cancel it first to propose a different hash.
    pub fn propose_upgrade(
        env: Env,
        proposer: Address,
        new_wasm_hash: BytesN<32>,
    ) -> Result<bool, EscrowError> {
        proposer.require_auth();
        if !Self::is_admin(env.clone(), proposer.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        if Self::pending_upgrade_proposal(&env).is_some() {
            return Err(EscrowError::AlreadyInitialized);
        }

        let proposed_at = env.ledger().timestamp();
        let mut approvals = Vec::new(&env);
        approvals.push_back(proposer.clone());
        let proposal = UpgradeProposal {
            new_wasm_hash: new_wasm_hash.clone(),
            proposed_at,
            approvals,
            executed: false,
        };
        env.storage()
            .instance()
            .set(&DataKey::UpgradeProposal, &proposal);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("upg_prop")),
            ContractUpgradeProposedEvent {
                proposer,
                new_wasm_hash,
                proposed_at,
                executable_at: proposed_at.saturating_add(UPGRADE_TIMELOCK_SECS),
                threshold: Self::upgrade_threshold(&env),
            },
        );
        Ok(true)
    }

    /// Approve the pending upgrade proposal. Admin or co-admin.
    ///
    /// `new_wasm_hash` must match the pending proposal so an approval can
    /// never be applied to a proposal that was swapped out after review.
    pub fn approve_upgrade(
        env: Env,
        approver: Address,
        new_wasm_hash: BytesN<32>,
    ) -> Result<bool, EscrowError> {
        approver.require_auth();
        if !Self::is_admin(env.clone(), approver.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        let mut proposal =
            Self::pending_upgrade_proposal(&env).ok_or(EscrowError::NotFound)?;
        if proposal.new_wasm_hash != new_wasm_hash {
            return Err(EscrowError::InvalidAddress);
        }
        if proposal.approvals.contains(&approver) {
            return Err(EscrowError::AlreadyVoted);
        }
        proposal.approvals.push_back(approver.clone());
        env.storage()
            .instance()
            .set(&DataKey::UpgradeProposal, &proposal);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("upg_appr")),
            ContractUpgradeApprovedEvent {
                approver,
                new_wasm_hash,
                approval_count: Self::valid_upgrade_approvals(&env, &proposal.approvals),
                threshold: Self::upgrade_threshold(&env),
            },
        );
        Ok(true)
    }

    /// Cancel the pending upgrade proposal. Any admin or co-admin may veto.
    pub fn cancel_upgrade(env: Env, caller: Address) -> Result<bool, EscrowError> {
        caller.require_auth();
        if !Self::is_admin(env.clone(), caller.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        let proposal =
            Self::pending_upgrade_proposal(&env).ok_or(EscrowError::NotFound)?;
        env.storage().instance().remove(&DataKey::UpgradeProposal);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("upg_cncl")),
            ContractUpgradeCancelledEvent {
                cancelled_by: caller,
                new_wasm_hash: proposal.new_wasm_hash,
            },
        );
        Ok(true)
    }

    /// Execute the pending multi-sig upgrade to `new_wasm_hash`. Admin or
    /// co-admin (issue #292).
    ///
    /// Requires a pending proposal for the same hash, at least
    /// `get_upgrade_threshold()` approvals from addresses that are *still*
    /// admins, and that [`UPGRADE_TIMELOCK_SECS`] has elapsed since the
    /// proposal was submitted. A single key can therefore never replace the
    /// contract code unilaterally.
    ///
    /// Persistent storage (escrows, admin list, fee config, …) is keyed by
    /// contract instance and is unaffected by an upgrade — only the
    /// executable code changes, so existing escrows remain functional
    /// afterwards. Sets the migration flag and emits
    /// [`ContractUpgradedEvent`] so backend services can detect the version
    /// change.
    pub fn upgrade(
        env: Env,
        admin: Address,
        new_wasm_hash: BytesN<32>,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin.clone()) {
            return Err(EscrowError::Unauthorized);
        }

        let mut proposal =
            Self::pending_upgrade_proposal(&env).ok_or(EscrowError::NotFound)?;
        if proposal.new_wasm_hash != new_wasm_hash {
            return Err(EscrowError::InvalidAddress);
        }
        if env.ledger().timestamp() < proposal.proposed_at.saturating_add(UPGRADE_TIMELOCK_SECS) {
            return Err(EscrowError::TimeoutNotReached);
        }
        if Self::valid_upgrade_approvals(&env, &proposal.approvals) < Self::upgrade_threshold(&env)
        {
            return Err(EscrowError::QuorumNotReached);
        }

        proposal.executed = true;
        env.storage()
            .instance()
            .set(&DataKey::UpgradeProposal, &proposal);

        let previous_semver = Self::version(env.clone()).semver;

        env.storage().instance().set(&DataKey::MigrationFlag, &true);
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("upgraded")),
            ContractUpgradedEvent {
                admin,
                previous_semver,
                new_wasm_hash: new_wasm_hash.clone(),
            },
        );

        env.deployer().update_current_contract_wasm(new_wasm_hash);

        Ok(true)
    }

    /// Returns true once the contract has been upgraded at least once via `upgrade`.
    pub fn is_migrated(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::MigrationFlag)
            .unwrap_or(false)
    }

    /// Set the escrow amount limits. Admin-only.
    pub fn set_limits(
        env: Env,
        admin: Address,
        min_amount: i128,
        max_amount: i128,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        if min_amount <= 0 || max_amount < min_amount {
            return Err(EscrowError::InvalidLimits);
        }
        env.storage().instance().set(
            &DataKey::AmountLimits,
            &EscrowAmountLimits {
                min_amount,
                max_amount,
            },
        );
        Ok(true)
    }

    /// Get the current escrow amount limits.
    ///
    /// # Errors
    /// Returns [`EscrowError::AmountLimitsNotSet`] when the contract has not
    /// been initialized with amount limits yet.
    pub fn get_limits(env: Env) -> Result<EscrowAmountLimits, EscrowError> {
        env.storage()
            .instance()
            .get(&DataKey::AmountLimits)
            .ok_or(EscrowError::AmountLimitsNotSet)
    }

    /// Configure the finance approver for a high-value escrow. Admin-only.
    pub fn set_dual_control_config(
        env: Env,
        admin: Address,
        escrow_id: u64,
        secondary_approver: Address,
    ) -> Result<(), EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin) {
            return Err(EscrowError::Unauthorized);
        }

        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;
        if record.amount <= DUAL_CONTROL_THRESHOLD {
            return Err(EscrowError::InvalidAmount);
        }

        env.storage().persistent().set(
            &DataKey::DualControlConfig(escrow_id),
            &DualControlConfig {
                secondary_approver,
                is_secondary_approved: false,
                secondary_approved_at: 0,
            },
        );
        Ok(())
    }

    /// Configure a secondary-approver deadline for a dual-control escrow.
    /// Admin-only. Issue #336.
    ///
    /// `timeout_ledgers` is measured from the current ledger sequence and must
    /// be greater than zero, so the deadline is always in the future. Once the
    /// deadline passes without a recorded secondary approval,
    /// [`Self::handle_dual_control_timeout`] applies `fallback_action`.
    ///
    /// # Errors
    /// - [`EscrowError::NotFound`] when the escrow does not exist.
    /// - [`EscrowError::DualControlNotConfigured`] when no secondary approver
    ///   is configured for the escrow.
    /// - [`EscrowError::InvalidExtension`] when `timeout_ledgers` is zero.
    /// - [`EscrowError::InvalidFallbackAction`] when `fallback_action` is not
    ///   `Disputed` or `Refunded`.
    pub fn set_dual_control_timeout(
        env: Env,
        admin: Address,
        escrow_id: u64,
        timeout_ledgers: u32,
        fallback_action: EscrowStatus,
    ) -> Result<DualControlTimeout, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin) {
            return Err(EscrowError::Unauthorized);
        }

        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Escrow(escrow_id))
            .ok_or(EscrowError::NotFound)?;
        check_not_terminal(&record)?;
        if record.status != EscrowStatus::Funded {
            return Err(EscrowError::InvalidStatus);
        }

        // A deadline without a secondary approver could never be satisfied.
        if !env
            .storage()
            .persistent()
            .has(&DataKey::DualControlConfig(escrow_id))
        {
            return Err(EscrowError::DualControlNotConfigured);
        }

        if timeout_ledgers == 0 {
            return Err(EscrowError::InvalidExtension);
        }
        if !DualControlTimeout::is_valid_fallback(&fallback_action) {
            return Err(EscrowError::InvalidFallbackAction);
        }

        let timeout = DualControlTimeout {
            approver_deadline_ledger: env.ledger().sequence().saturating_add(timeout_ledgers),
            fallback_action: fallback_action.clone(),
        };
        env.storage()
            .persistent()
            .set(&DataKey::DualControlTimeout(escrow_id), &timeout);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("dctmo"), escrow_id),
            DualControlTimeoutSetEvent {
                escrow_id,
                approver_deadline_ledger: timeout.approver_deadline_ledger,
                fallback_action,
            },
        );

        Ok(timeout)
    }

    /// Read-only getter for the secondary-approver deadline of an escrow.
    pub fn get_dual_control_timeout(
        env: Env,
        escrow_id: u64,
    ) -> Result<DualControlTimeout, EscrowError> {
        env.storage()
            .persistent()
            .get(&DataKey::DualControlTimeout(escrow_id))
            .ok_or(EscrowError::DualControlTimeoutNotConfigured)
    }

    /// Read-only check for whether the secondary-approver window has closed.
    ///
    /// Returns `false` when no deadline is configured, so callers must gate on
    /// [`Self::get_dual_control_timeout`] (or the `NotConfigured` error) rather
    /// than treat a `false` answer as "not yet expired".
    pub fn is_approver_deadline_passed(env: Env, escrow_id: u64) -> bool {
        let timeout: Option<DualControlTimeout> = env
            .storage()
            .persistent()
            .get(&DataKey::DualControlTimeout(escrow_id));
        match timeout {
            Some(cfg) => env.ledger().sequence() >= cfg.approver_deadline_ledger,
            None => false,
        }
    }

    /// Record explicit finance authorization for a configured high-value escrow.
    ///
    /// # Errors
    /// Returns [`EscrowError::ApproverDeadlineExpired`] when a deadline
    /// configured by `set_dual_control_timeout` has already passed: a late
    /// signature must not resurrect an order whose fallback is due, and the
    /// approver is expected to take the dispute/refund path instead.
    pub fn approve_release(
        env: Env,
        escrow_id: u64,
        secondary_approver: Address,
    ) -> Result<bool, EscrowError> {
        let config_key = DataKey::DualControlConfig(escrow_id);
        let mut config: DualControlConfig = env
            .storage()
            .persistent()
            .get(&config_key)
            .ok_or(EscrowError::Unauthorized)?;
        if config.secondary_approver != secondary_approver {
            return Err(EscrowError::Unauthorized);
        }
        secondary_approver.require_auth();

        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Escrow(escrow_id))
            .ok_or(EscrowError::NotFound)?;
        if record.amount <= DUAL_CONTROL_THRESHOLD || record.status != EscrowStatus::Funded {
            return Err(EscrowError::InvalidStatus);
        }

        if Self::is_approver_deadline_passed(env.clone(), escrow_id) {
            return Err(EscrowError::ApproverDeadlineExpired);
        }

        config.is_secondary_approved = true;
        config.secondary_approved_at = env.ledger().timestamp();
        env.storage().persistent().set(&config_key, &config);
        Ok(true)
    }

    /// Invoke an external oracle contract's `evaluate` entry point, catching
    /// any cross-contract failure (panic, revert, or paused oracle) and
    /// mapping it to [`EscrowError::OracleInvocationFailed`] so the caller
    /// can retry or open a dispute instead of being trapped.
    ///
    /// Returns `Ok(true)` when the oracle reports the condition is met,
    /// `Ok(false)` when the oracle reports it is not met, and
    /// `Err(EscrowError::OracleInvocationFailed)` when the oracle call
    /// itself fails for any reason.
    pub fn safe_oracle_call(
        env: &Env,
        oracle: &Address,
        condition: Symbol,
    ) -> Result<bool, EscrowError> {
        let result = env.try_invoke_contract::<bool, InvokeError>(
            oracle,
            &Symbol::new(env, "evaluate"),
            soroban_sdk::vec![env, condition.into_val(env)],
        );
        match result {
            Ok(Ok(met)) => Ok(met),
            Ok(Err(_)) | Err(_) => Err(EscrowError::OracleInvocationFailed),
        }
    }
    /// Apply the configured fallback once the secondary-approver deadline has
    /// passed without an approval. Issue #336.
    ///
    /// Permissionless: the outcome is fully determined by stored state, so any
    /// address (typically a keeper or either escrow party) may trigger it.
    /// A `Refunded` fallback pays the buyer the full remaining balance and
    /// bypasses the escrow `timeout_ledger` — the approver deadline is the
    /// governing clock here. A `Disputed` fallback only moves the escrow to
    /// `Disputed`, leaving settlement to the arbiter quorum.
    ///
    /// # Errors
    /// - [`EscrowError::NotFound`] when the escrow does not exist.
    /// - [`EscrowError::DualControlTimeoutNotConfigured`] when no deadline is set.
    /// - [`EscrowError::InvalidStatus`] when the escrow is not `Funded`.
    /// - [`EscrowError::DualControlAlreadyApproved`] when the approver already signed.
    /// - [`EscrowError::ApproverDeadlineNotReached`] before the deadline.
    /// - [`EscrowError::InvalidFallbackAction`] for a corrupt fallback action.
    pub fn handle_dual_control_timeout(
        env: Env,
        escrow_id: u64,
        caller: Address,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();

        let timeout: DualControlTimeout = env
            .storage()
            .persistent()
            .get(&DataKey::DualControlTimeout(escrow_id))
            .ok_or(EscrowError::DualControlTimeoutNotConfigured)?;
        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;
        check_not_terminal(&record)?;
        if record.status != EscrowStatus::Funded {
            return Err(EscrowError::InvalidStatus);
        }

        let config: DualControlConfig = env
            .storage()
            .persistent()
            .get(&DataKey::DualControlConfig(escrow_id))
            .ok_or(EscrowError::DualControlNotConfigured)?;
        if config.is_secondary_approved {
            return Err(EscrowError::DualControlAlreadyApproved);
        }
        if env.ledger().sequence() < timeout.approver_deadline_ledger {
            return Err(EscrowError::ApproverDeadlineNotReached);
        }
        if !DualControlTimeout::is_valid_fallback(&timeout.fallback_action) {
            return Err(EscrowError::InvalidFallbackAction);
        }

        let remaining = record.amount - record.released_amount - record.refunded_amount;
        let refunded_amount = match timeout.fallback_action {
            EscrowStatus::Refunded => {
                let token_client = soroban_sdk::token::Client::new(&env, &record.token);
                token_client.transfer(&env.current_contract_address(), &record.buyer, &remaining);
                record.refunded_amount += remaining;
                record.status = EscrowStatus::Refunded;
                record.updated_at = env.ledger().timestamp();
                env.storage().persistent().set(&key, &record);

                env.events().publish(
                    (
                        symbol_short!("escrow"),
                        symbol_short!("refunded"),
                        escrow_id,
                    ),
                    EscrowRefundedEvent {
                        escrow_id,
                        buyer: record.buyer.clone(),
                        amount: remaining,
                        remaining: 0,
                        refunded_by: caller.clone(),
                    },
                );
                remaining
            }
            EscrowStatus::Disputed => {
                record.status = EscrowStatus::Disputed;
                record.updated_at = env.ledger().timestamp();
                env.storage().persistent().set(&key, &record);

                env.events().publish(
                    (
                        symbol_short!("escrow"),
                        symbol_short!("disputed"),
                        escrow_id,
                    ),
                    EscrowDisputedEvent {
                        escrow_id,
                        disputed_by: caller.clone(),
                    },
                );
                0
            }
            _ => return Err(EscrowError::InvalidFallbackAction),
        };

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("dcfb"), escrow_id),
            DualControlTimeoutFallbackEvent {
                escrow_id,
                approver_deadline_ledger: timeout.approver_deadline_ledger,
                fallback_action: timeout.fallback_action,
                refunded_amount,
                executed_by: caller,
            },
        );

        Ok(true)
    }

    /// Set the quorum configuration for dispute resolution. Admin-only.
    pub fn set_quorum_config(
        env: Env,
        admin: Address,
        arbiters: soroban_sdk::Vec<Address>,
        threshold: u32,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        if threshold == 0 || threshold > arbiters.len() {
            return Err(EscrowError::InvalidQuorum);
        }
        // Check for duplicate arbiters
        let mut unique_arbiters = soroban_sdk::Vec::new(&env);
        for arbiter in arbiters.iter() {
            if unique_arbiters.contains(&arbiter) {
                return Err(EscrowError::InvalidQuorum);
            }
            unique_arbiters.push_back(arbiter);
        }
        let quorum_config = QuorumConfig {
            arbiters: unique_arbiters,
            threshold,
        };
        env.storage()
            .instance()
            .set(&DataKey::QuorumConfig, &quorum_config);
        Ok(true)
    }

    /// Get the current quorum configuration.
    pub fn get_quorum_config(env: Env) -> Result<QuorumConfig, EscrowError> {
        env.storage()
            .instance()
            .get(&DataKey::QuorumConfig)
            .ok_or(EscrowError::QuorumConfigNotSet)
    }

    /// Vote on a disputed escrow. Only authorized arbiters.
    pub fn vote_dispute(
        env: Env,
        escrow_id: u64,
        arbiter: Address,
        release_to_seller: bool,
    ) -> Result<bool, EscrowError> {
        arbiter.require_auth();

        let quorum_config: QuorumConfig = env
            .storage()
            .instance()
            .get(&DataKey::QuorumConfig)
            .ok_or(EscrowError::QuorumConfigNotSet)?;
        if !quorum_config.arbiters.contains(&arbiter) {
            return Err(EscrowError::NotAnArbiter);
        }

        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };
        if record.status != EscrowStatus::Disputed {
            return Err(EscrowError::NotDisputed);
        }

        let votes_key = DataKey::DisputeVotes(escrow_id);
        let mut votes: soroban_sdk::Vec<DisputeVote> = env
            .storage()
            .persistent()
            .get(&votes_key)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env));

        if votes.iter().any(|vote| vote.arbiter == arbiter) {
            return Err(EscrowError::AlreadyVoted);
        }

        votes.push_back(DisputeVote {
            arbiter: arbiter.clone(),
            release_to_seller,
        });
        env.storage().persistent().set(&votes_key, &votes);

        // Compute live tallies so the event reflects the state *after* this vote.
        let votes_for = votes.iter().filter(|v| v.release_to_seller).count() as u32;

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("vote"), escrow_id),
            DisputeVotedEvent {
                escrow_id,
                arbiter,
                release_to_seller,
                votes_for,
                threshold: quorum_config.threshold,
            },
        );

        Ok(true)
    }

    /// Get votes for a disputed escrow.
    pub fn get_dispute_votes(env: Env, escrow_id: u64) -> soroban_sdk::Vec<DisputeVote> {
        let votes_key = DataKey::DisputeVotes(escrow_id);
        env.storage()
            .persistent()
            .get(&votes_key)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env))
    }

    /// Resolve a disputed escrow via quorum.
    pub fn resolve_dispute_quorum(
        env: Env,
        escrow_id: u64,
        caller: Address,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();
        Self::enter_release(&env)?;

        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };
        if record.status != EscrowStatus::Disputed {
            return Err(EscrowError::NotDisputed);
        }

        let quorum_config: QuorumConfig = env
            .storage()
            .instance()
            .get(&DataKey::QuorumConfig)
            .ok_or(EscrowError::QuorumConfigNotSet)?;
        let votes_key = DataKey::DisputeVotes(escrow_id);
        let votes: soroban_sdk::Vec<DisputeVote> = env
            .storage()
            .persistent()
            .get(&votes_key)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env));

        // Only count votes from current arbiters
        let seller_votes = votes
            .iter()
            .filter(|v| v.release_to_seller && quorum_config.arbiters.contains(&v.arbiter))
            .count() as u32;
        let buyer_votes = votes
            .iter()
            .filter(|v| !v.release_to_seller && quorum_config.arbiters.contains(&v.arbiter))
            .count() as u32;

        // Handle conflicting quorum outcomes explicitly
        let release_to_seller =
            if seller_votes >= quorum_config.threshold && buyer_votes >= quorum_config.threshold {
                return Err(EscrowError::ConflictingQuorum);
            } else if seller_votes >= quorum_config.threshold {
                true
            } else if buyer_votes >= quorum_config.threshold {
                false
            } else {
                return Err(EscrowError::QuorumNotReached);
            };

        let token_client = soroban_sdk::token::Client::new(&env, &record.token);
        if release_to_seller {
            let payout = Self::compute_payout(&env, &record.seller, record.amount)?;
            Self::distribute_fee(&env, &token_client, payout.fee)?;
            token_client.transfer(
                &env.current_contract_address(),
                &record.seller,
                &payout.seller_net,
            );
            update_merchant_volume_and_tier(&env, &record.seller, record.amount);
            record.status = EscrowStatus::Released;
            Self::try_mint_purchase_receipt(&env, &mut record);
        let payout = if release_to_seller {
            Some(Self::compute_payout(&env, record.amount)?)
        } else {
            None
        };
        record.status = if release_to_seller {
            EscrowStatus::Released
        } else {
            EscrowStatus::Refunded
        };
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);
        env.storage().persistent().remove(&votes_key);
        env.storage()
            .persistent()
            .remove(&DataKey::TimeoutExtensionVotes(escrow_id));

        if release_to_seller {
            let payout = payout.unwrap();
            Self::distribute_fee(&env, &token_client, payout.fee)?;
            token_client.transfer(
                &env.current_contract_address(),
                &record.seller,
                &payout.seller_net,
            );
        } else {
            token_client.transfer(
                &env.current_contract_address(),
                &record.buyer,
                &record.amount,
            );
        }

        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("resolved"),
                escrow_id,
            ),
            EscrowResolvedEvent {
                escrow_id,
                release_to_seller,
                resolved_by: caller,
            },
        );

        Self::exit_release(&env);
        Ok(true)
    }

    /// Vote to extend the refund timeout of an escrow via arbiter quorum.
    ///
    /// Any arbiter configured in [`QuorumConfig`] may cast one vote per
    /// escrow proposing an `extension_ledgers` amount. Once at least
    /// `threshold` arbiters have voted for the *same* `extension_ledgers`
    /// value, the escrow's `timeout_ledger` is pushed back by that amount,
    /// a [`TimeoutExtendedEvent`] is published, and the vote log is cleared
    /// so arbiters can vote again for a future extension.
    ///
    /// Requires the escrow to not be in a terminal state (Released,
    /// Refunded, Cancelled). Each arbiter may vote only once per round.
    pub fn extend_timeout_via_quorum(
        env: Env,
        escrow_id: u64,
        arbiter: Address,
        extension_ledgers: u32,
    ) -> Result<bool, EscrowError> {
        arbiter.require_auth();

        if extension_ledgers == 0 {
            return Err(EscrowError::InvalidExtension);
        }

        let quorum_config: QuorumConfig = env
            .storage()
            .instance()
            .get(&DataKey::QuorumConfig)
            .ok_or(EscrowError::QuorumConfigNotSet)?;
        if !quorum_config.arbiters.contains(&arbiter) {
            return Err(EscrowError::NotAnArbiter);
        }

        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };
        check_not_terminal(&record)?;

        let votes_key = DataKey::TimeoutExtensionVotes(escrow_id);
        let mut votes: soroban_sdk::Vec<TimeoutExtensionVote> = env
            .storage()
            .persistent()
            .get(&votes_key)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env));

        if votes.iter().any(|vote| vote.arbiter == arbiter) {
            return Err(EscrowError::AlreadyVoted);
        }

        votes.push_back(TimeoutExtensionVote {
            arbiter,
            extension_ledgers,
            voted_at: env.ledger().timestamp(),
        });

        let matching_votes = votes
            .iter()
            .filter(|vote| vote.extension_ledgers == extension_ledgers)
            .count() as u32;

        if matching_votes < quorum_config.threshold {
            env.storage().persistent().set(&votes_key, &votes);
            return Ok(false);
        }

        let previous_timeout_ledger = record.timeout_ledger;
        let new_timeout_ledger = previous_timeout_ledger.saturating_add(extension_ledgers);
        record.timeout_ledger = new_timeout_ledger;
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);
        env.storage().persistent().remove(&votes_key);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("tmo_ext"), escrow_id),
            TimeoutExtendedEvent {
                escrow_id,
                previous_timeout_ledger,
                new_timeout_ledger,
                extension_ledgers,
            },
        );

        Ok(true)
    }

    /// Get the recorded timeout-extension votes for an escrow's current round.
    pub fn get_timeout_extension_votes(
        env: Env,
        escrow_id: u64,
    ) -> soroban_sdk::Vec<TimeoutExtensionVote> {
        let votes_key = DataKey::TimeoutExtensionVotes(escrow_id);
        env.storage()
            .persistent()
            .get(&votes_key)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env))
    }

    pub fn register_arbiter_with_stake(
        env: Env,
        arbiter: Address,
        stake_amount: i128,
    ) -> Result<bool, EscrowError> {
        arbiter.require_auth();
        if stake_amount <= 0 {
            return Err(EscrowError::ArbiterStakedAmountZero);
        }
        
        let record = ArbiterStakingRecord {
            arbiter: arbiter.clone(),
            staked_amount: stake_amount,
            assigned_disputes_count: 0,
            is_slashed: false,
        };
        env.storage().persistent().set(&DataKey::ArbiterStake(arbiter), &record);
        Ok(true)
    }

    pub fn slash_delinquent_arbiter(
        env: Env,
        escrow_id: u64,
        caller: Address,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();
        let escrow_key = DataKey::Escrow(escrow_id);
        let escrow: EscrowRecord = match env.storage().persistent().get(&escrow_key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };
        
        let deadline: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::DisputeDeadline(escrow_id))
            .ok_or(EscrowError::NotFound)?;
            
        if env.ledger().timestamp() <= deadline {
            return Err(EscrowError::DisputeNotExpired);
        }

        let quorum_config: QuorumConfig = env
            .storage()
            .instance()
            .get(&DataKey::QuorumConfig)
            .ok_or(EscrowError::QuorumConfigNotSet)?;

        let votes_key = DataKey::DisputeVotes(escrow_id);
        let votes: soroban_sdk::Vec<DisputeVote> = env
            .storage()
            .persistent()
            .get(&votes_key)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env));

        let token_client = soroban_sdk::token::Client::new(&env, &escrow.token);
        
        let mut slashed_any = false;
        
        for arbiter in quorum_config.arbiters.iter() {
            let has_voted = votes.iter().any(|v| v.arbiter == arbiter);
            if !has_voted {
                let stake_key = DataKey::ArbiterStake(arbiter.clone());
                if let Some(mut arbiter_record) = env.storage().persistent().get::<_, ArbiterStakingRecord>(&stake_key) {
                    if !arbiter_record.is_slashed && arbiter_record.staked_amount > 0 {
                        arbiter_record.is_slashed = true;
                        let penalty = arbiter_record.staked_amount / 2;
                        if penalty > 0 {
                            token_client.transfer(
                                &env.current_contract_address(),
                                &escrow.buyer,
                                &penalty,
                            );
                            token_client.transfer(
                                &env.current_contract_address(),
                                &escrow.seller,
                                &penalty,
                            );
                        }
                        arbiter_record.staked_amount = 0;
                        env.storage().persistent().set(&stake_key, &arbiter_record);
                        slashed_any = true;
                    }
                }
            }
        }
        
        if !slashed_any {
            return Err(EscrowError::NotFound);
        }
        
        Ok(true)
    }

    /// Update the fee percentage. Admin-only.
    pub fn update_fee(env: Env, admin: Address, new_fee_bps: u32) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        if new_fee_bps > 1000 {
            return Err(EscrowError::InvalidFeeBps);
        }
        admin_actions::consume(&env, AdminAction::Fee(new_fee_bps))?;
        let mut fee_config: FeeConfig = Self::get_fee_config(env.clone())?;
        fee_config.fee_bps = new_fee_bps;
        env.storage()
            .instance()
            .set(&DataKey::FeeConfig, &fee_config);
        Ok(true)
    }

    /// Configure the minimum release fee floor, in the token's smallest unit
    /// (stroop). Admin-only (issue #362).
    ///
    /// Integer division truncates `amount * fee_bps / 10_000` to zero for every
    /// `amount` below `10_000 / fee_bps` stroops, so without a floor a seller
    /// can fragment a large payment into dust escrows and pay no platform fee
    /// at all. The floor is applied to the total fee of every payout path
    /// (release, partial release, split release and dispute resolution) and is
    /// always clamped to the released amount, so it can never over-charge a
    /// settlement. `0` restores the pure pro-rata behaviour. A platform fee of
    /// `0` bps is never floored: it is an explicit "no platform fee" setting
    /// rather than a fee that truncation erased.
    ///
    /// # Errors
    /// Returns [`EscrowError::Unauthorized`] when the caller is not an admin,
    /// and [`EscrowError::InvalidMinFee`] when `min_fee_stroops` is negative or
    /// above [`MAX_MIN_FEE_STROOPS`].
    pub fn set_min_fee_stroops(
        env: Env,
        admin: Address,
        min_fee_stroops: i128,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin) {
            return Err(EscrowError::Unauthorized);
        }
        if !(0..=MAX_MIN_FEE_STROOPS).contains(&min_fee_stroops) {
            return Err(EscrowError::InvalidMinFee);
        }
        env.storage()
            .instance()
            .set(&DataKey::MinFeeStroops, &min_fee_stroops);
        Ok(true)
    }

    /// Current minimum release fee floor, in stroops (issue #362).
    ///
    /// Returns [`DEFAULT_MIN_FEE_STROOPS`] when no admin override is configured.
    pub fn get_min_fee_stroops(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::MinFeeStroops)
            .unwrap_or(DEFAULT_MIN_FEE_STROOPS)
    }

    /// Get the current fee configuration.
    ///
    /// # Errors
    /// Returns [`EscrowError::FeeConfigNotSet`] when the contract has not
    /// been initialized with fee configuration yet.
    pub fn get_fee_config(env: Env) -> Result<FeeConfig, EscrowError> {
        env.storage()
            .instance()
            .get(&DataKey::FeeConfig)
            .ok_or(EscrowError::FeeConfigNotSet)
    }

    /// Schedule a fee update to take effect after the minimum notice window.
    ///
    /// Requires admin authentication and enforces a minimum notice period
    /// of 24 hours and 17,280 ledgers, subject to guardian veto.
    /// Emits an `admin/queued` event with the proposal ID; execute that ID
    /// explicitly with `execute_admin_action`. Getters never apply changes.
    pub fn schedule_fee_update(
        env: Env,
        admin: Address,
        new_fee_bps: u32,
        new_treasury: Address,
    ) -> Result<bool, EscrowError> {
        Self::queue_admin_action(
            env,
            admin,
            AdminAction::FeeConfig(new_fee_bps, new_treasury),
        )?;
        Ok(true)
    }

    /// Get any pending scheduled fee update.
    pub fn get_scheduled_fee_update(env: Env) -> Option<ScheduledFeeUpdate> {
        env.storage().instance().get(&DataKey::ScheduledFeeUpdate)
    }

    /// Extend an escrow's TTL and pay a small keeper bounty if within threshold.
    ///
    /// This function allows keepers to extend escrows that are close to expiration
    /// (within `BUMP_BOUNTY_THRESHOLD_LEDGERS` of their timeout). If the escrow
    /// is within this threshold, a small bounty is paid to the keeper address.
    ///
    /// Rate limiting is enforced to prevent bounty draining - a keeper cannot
    /// call this function more frequently than `BUMP_RATE_LIMIT_LEDGERS`.
    ///
    /// # Arguments
    /// * `escrow_id` - The ID of the escrow to bump
    /// * `keeper` - Address that receives the bounty and called the function
    ///
    /// # Returns
    /// * `true` if TTL was extended and bounty paid
    /// * `false` if TTL was extended but escrow was not within bounty threshold
    ///
    /// # Errors
    /// Returns [`EscrowError::BumpRateLimitExceeded`] if called too soon after last bump.
    /// Returns [`EscrowError::NotFound`] if escrow does not exist.
    /// Returns [`EscrowError::InvalidStatus`] if escrow is in a terminal state.
    pub fn bump_ttl_with_bounty(
        env: Env,
        escrow_id: u64,
        keeper: Address,
    ) -> Result<bool, EscrowError> {
        keeper.require_auth();

        // Check rate limiting
        let current_ledger = env.ledger().sequence();
        let last_bump: u32 = env
            .storage()
            .instance()
            .get(&DataKey::LastBumpLedger(escrow_id))
            .unwrap_or(0);
        if current_ledger.saturating_sub(last_bump) < BUMP_RATE_LIMIT_LEDGERS {
            return Err(EscrowError::TimeoutNotReached);
        }

        // Get and validate escrow
        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };
        check_not_terminal(&record)?;

        if record.status != EscrowStatus::Funded {
            return Err(EscrowError::InvalidStatus);
        }

        // Calculate ledgers until timeout
        let ledgers_until_timeout = record.timeout_ledger.saturating_sub(current_ledger);

        // Extend TTL by the bump amount (30 days)
        let extension = PERSISTENT_BUMP_AMOUNT;
        let old_timeout = record.timeout_ledger;
        record.timeout_ledger = record.timeout_ledger.saturating_add(extension);
        record.updated_at = env.ledger().timestamp();

        // Update storage
        env.storage().persistent().set(&key, &record);
        env.storage()
            .instance()
            .set(&DataKey::LastBumpLedger(escrow_id), &current_ledger);

        // Extend TTL of the escrow record
        let storage = env.storage().persistent();
        storage.extend_ttl(&key, PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
        // Keep the cancellation guard snapshot (issue #355) on the same
        // schedule as the record it describes.
        Self::bump_cancel_guard_ttl(&env, escrow_id);

        // Check if within bounty threshold and pay bounty
        let bounty_paid = if ledgers_until_timeout <= BUMP_BOUNTY_THRESHOLD_LEDGERS
            && ledgers_until_timeout > 0
        {
            let remaining = record.amount - record.released_amount - record.refunded_amount;
            if remaining >= KEEPER_BOUNTY_AMOUNT {
                let token_client = soroban_sdk::token::Client::new(&env, &record.token);
                token_client.transfer(
                    &env.current_contract_address(),
                    &keeper,
                    &KEEPER_BOUNTY_AMOUNT,
                );

                env.events().publish(
                    (symbol_short!("escrow"), symbol_short!("bounty"), escrow_id),
                    KeeperBountyPaidEvent {
                        escrow_id,
                        keeper: keeper.clone(),
                        bounty_amount: KEEPER_BOUNTY_AMOUNT,
                        new_timeout_ledger: record.timeout_ledger,
                    },
                );
                true
            } else {
                false
            }
        } else {
            false
        };

        // Emit timeout extended event
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("tmo_ext"), escrow_id),
            EscrowTimeoutExtendedEvent {
                escrow_id,
                old_timeout_ledger: old_timeout,
                new_timeout_ledger: record.timeout_ledger,
                extended_by: keeper,
            },
        );

        Ok(bounty_paid)
    }

    /// Record an on-chain shipment proof for an escrow.
    ///
    /// This proof is required for sellers to claim funds on timeout.
    /// Without a recorded proof, only the buyer can claim a refund on timeout.
    pub fn record_shipment_proof(
        env: Env,
        escrow_id: u64,
        caller: Address,
        carrier: Symbol,
        tracking_hash: BytesN<32>,
        shipped_at: u64,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();

        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };

        // Only seller can record shipment proof
        if caller != record.seller {
            return Err(EscrowError::Unauthorized);
        }

        // Validate shipment timestamp is reasonable
        if shipped_at < record.created_at || shipped_at > env.ledger().timestamp() {
            return Err(EscrowError::InvalidSignedDeliveryProof);
        }

        let proof = ShipmentProof {
            escrow_id,
            carrier,
            tracking_hash,
            shipped_at,
            recorded_at: env.ledger().timestamp(),
        };
        env.storage()
            .persistent()
            .set(&DataKey::ShipmentProof(escrow_id), &proof);

        // Extend TTL for the proof
        let storage = env.storage().persistent();
        storage.extend_ttl(
            &DataKey::ShipmentProof(escrow_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        Ok(true)
    }

    /// Get shipment proof for an escrow.
    pub fn get_shipment_proof(env: Env, escrow_id: u64) -> Result<ShipmentProof, EscrowError> {
        env.storage()
            .persistent()
            .get(&DataKey::ShipmentProof(escrow_id))
            .ok_or(EscrowError::NotFound)
    }

    /// Check if an escrow has a valid shipment proof.
    pub fn has_shipment_proof(env: Env, escrow_id: u64) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::ShipmentProof(escrow_id))
    }

    /// Configure the release fee to be split across multiple treasuries. Admin-only.
    ///
    /// The combined `bps` across all shares must not exceed 1000 (10%). Once
    /// set, `resolve_dispute` and `resolve_dispute_quorum` pay each treasury
    /// its configured share instead of the single treasury in `FeeConfig`.
    /// Passing an empty vector reverts to the single-treasury `FeeConfig`.
    pub fn set_fee_distribution(
        env: Env,
        admin: Address,
        shares: soroban_sdk::Vec<TreasuryShare>,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin.clone()) {
            return Err(EscrowError::Unauthorized);
        }

        if shares.len() > MAX_TREASURIES {
            return Err(EscrowError::InvalidLimits);
        }

        let mut total_bps: u32 = 0;
        for share in shares.iter() {
            if is_zero_address(&env, &share.treasury) {
                return Err(EscrowError::InvalidAddress);
            }
            if share.bps == 0 {
                return Err(EscrowError::InvalidFeeBps);
            }
            total_bps = total_bps
                .checked_add(share.bps)
                .ok_or(EscrowError::InvalidFeeBps)?;
        }
        if total_bps > 1000 {
            return Err(EscrowError::InvalidFeeBps);
        }

        admin_actions::consume(&env, AdminAction::FeeDistribution(shares.clone()))?;
        env.storage()
            .instance()
            .set(&DataKey::FeeDistribution, &shares);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("feedist")),
            FeeDistributionSetEvent {
                admin,
                treasury_count: shares.len(),
                total_bps,
            },
        );

        Ok(true)
    }

    /// Configure the volume-based fee tier ladder (issue #328). Admin-only.
    ///
    /// Replaces the entire tier table. Tiers are validated then stored
    /// ascending by `min_volume`; the highest tier whose threshold a
    /// merchant's settled volume meets is the tier the merchant pays.
    /// Passing an empty vector removes all tiers, reverting every merchant to
    /// the base `FeeConfig::fee_bps`. Each tier's `fee_bps` must be within
    /// the 1000 bps (10%) cap, and thresholds must be strictly increasing.
    /// Emits [`ConfigChangeScheduledEvent`]-style topics under
    /// `(escrow, tierset)` with a [`FeeDistributionSetEvent`]-shaped summary.
    pub fn set_fee_tiers(
        env: Env,
        admin: Address,
        tiers: Vec<FeeTier>,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        if tiers.len() > MAX_FEE_TIERS {
            return Err(EscrowError::TierLimitExceeded);
        }

        let mut sorted: Vec<FeeTier> = Vec::new(&env);
        let mut prev_min_volume: Option<i128> = None;
        for tier in tiers.iter() {
            if tier.fee_bps == 0 || tier.fee_bps > MAX_TIER_FEE_BPS {
                return Err(EscrowError::InvalidTier);
            }
            if tier.min_volume <= 0 {
                return Err(EscrowError::InvalidTier);
            }
            if let Some(prev) = prev_min_volume {
                if tier.min_volume <= prev {
                    return Err(EscrowError::InvalidTier);
                }
            }
            prev_min_volume = Some(tier.min_volume);
            sorted.push_back(tier);
        }

        let count = sorted.len();
        env.storage().instance().set(&DataKey::FeeTiers, &sorted);

        // Reuse the `(escrow, feedist)` topic family so indexers can subscribe
        // to fee-table changes in one subscription; the data shape carries the
        // tier count and the top tier's fee for monitoring.
        let top_bps = if count == 0 {
            0
        } else {
            sorted.get_unchecked(count - 1).fee_bps
        };
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("tierset")),
            FeeDistributionSetEvent {
                admin,
                treasury_count: count,
                total_bps: top_bps,
            },
        );

        Ok(true)
    }

    /// Get the configured volume fee tier table, ascending by `min_volume`
    /// (issue #328). Empty when unset (base `FeeConfig::fee_bps` applies).
    pub fn get_fee_tiers(env: Env) -> Vec<FeeTier> {
        get_fee_tiers(&env)
    }

    /// Remove one volume fee tier by its `min_volume` threshold (issue #328).
    /// Admin-only. No-op success when the tier does not exist.
    pub fn remove_fee_tier(
        env: Env,
        admin: Address,
        min_volume: i128,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin.clone()) {
            return Err(EscrowError::Unauthorized);
        }

        let tiers = get_fee_tiers(&env);
        let mut updated: Vec<FeeTier> = Vec::new(&env);
        let mut found = false;
        for tier in tiers.iter() {
            if tier.min_volume == min_volume {
                found = true;
            } else {
                updated.push_back(tier);
            }
        }
        if !found {
            return Err(EscrowError::TierNotFound);
        }

        let count = updated.len();
        env.storage().instance().set(&DataKey::FeeTiers, &updated);
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("tierset")),
            FeeDistributionSetEvent {
                admin,
                treasury_count: count,
                total_bps: if count == 0 {
                    0
                } else {
                    updated.get_unchecked(count - 1).fee_bps
                },
            },
        );
        Ok(true)
    }

    /// Merchant's lifetime settled volume accumulated across successful
    /// escrow releases (issue #328).
    pub fn get_merchant_settled_volume(env: Env, merchant: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::MerchantSettledVolume(merchant))
            .unwrap_or(0)
    }

    /// The highest fee tier the merchant has achieved so far (issue #328),
    /// or `None` while the merchant sits below every configured threshold.
    pub fn get_merchant_tier(env: Env, merchant: Address) -> Option<FeeTier> {
        let min_volume: Option<i128> = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantFeeTier(merchant));
        let min_volume = min_volume?;
        get_fee_tiers(&env)
            .iter()
            .find(|tier| tier.min_volume == min_volume)
    }

    /// Fee in basis points the merchant would pay on a release right now
    /// (issue #328): the applicable volume tier, or the base `FeeConfig`
    /// fee when no tier applies.
    pub fn get_effective_fee_bps(env: Env, merchant: Address) -> Result<u32, EscrowError> {
        effective_fee_bps(&env, &merchant)
    }

    /// Get the current multi-treasury fee distribution. Empty when unset
    /// (i.e. the single-treasury `FeeConfig` is in effect).
    pub fn get_fee_distribution(env: Env) -> soroban_sdk::Vec<TreasuryShare> {
        env.storage()
            .instance()
            .get(&DataKey::FeeDistribution)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env))
    }

    /// Computes and transfers the release fee out of `amount`, splitting it
    /// across the configured multi-treasury distribution when one is set,
    /// falling back to the single-treasury `FeeConfig` otherwise. Returns
    /// the total fee amount deducted.
    fn distribute_fee(
        env: &Env,
        token_client: &soroban_sdk::token::Client,
        total_fee: i128,
    ) -> Result<(), EscrowError> {
        if total_fee == 0 {
            return Ok(());
        }

        let shares: soroban_sdk::Vec<TreasuryShare> = env
            .storage()
            .instance()
            .get(&DataKey::FeeDistribution)
            .unwrap_or_else(|| soroban_sdk::Vec::new(env));

        if shares.is_empty() {
            let fee_config: FeeConfig = Self::get_fee_config(env.clone())?;
            token_client.transfer(
                &env.current_contract_address(),
                &fee_config.treasury,
                &total_fee,
            );
        } else {
            let mut total_bps: i128 = 0;
            for share in shares.iter() {
                total_bps = total_bps
                    .checked_add(share.bps as i128)
                    .ok_or(EscrowError::MathOverflow)?;
            }

            let mut distributed: i128 = 0;
            let last_idx = shares.len() - 1;

            for (i, share) in shares.iter().enumerate() {
                if i as u32 == last_idx {
                    let remaining = total_fee - distributed;
                    if remaining > 0 {
                        token_client.transfer(
                            &env.current_contract_address(),
                            &share.treasury,
                            &remaining,
                        );
                    }
                } else {
                    let bps = share.bps as i128;
                    let fee = total_fee
                        .checked_mul(bps)
                        .and_then(|product| product.checked_div(total_bps))
                        .ok_or(EscrowError::MathOverflow)?;
                    if fee > 0 {
                        token_client.transfer(
                            &env.current_contract_address(),
                            &share.treasury,
                            &fee,
                        );
                        distributed = distributed
                            .checked_add(fee)
                            .ok_or(EscrowError::MathOverflow)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Computes the total release fee (in tokens) for `amount`, splitting it
    /// across the configured multi-treasury distribution when one is set,
    /// falling back to the single-treasury `FeeConfig` otherwise. Never
    /// transfers tokens and never panics on a missing config: returns
    /// `FeeConfigNotSet` when no config exists.
    ///
    /// When no multi-treasury split is configured, the basis points come from
    /// the merchant's volume fee tier when one applies (issue #328), falling
    /// back to the base `FeeConfig::fee_bps`.
    fn compute_fee_amount(
        env: &Env,
        merchant: &Address,
        amount: i128,
    ) -> Result<i128, EscrowError> {
    /// The total is floored at the configured minimum fee (issue #362) so that
    /// integer truncation can never make the fee of a small release zero.
    fn compute_fee_amount(env: &Env, amount: i128) -> Result<i128, EscrowError> {
        let min_fee = Self::get_min_fee_stroops(env.clone());
        let shares: soroban_sdk::Vec<TreasuryShare> = env
            .storage()
            .instance()
            .get(&DataKey::FeeDistribution)
            .unwrap_or_else(|| soroban_sdk::Vec::new(env));

        let (pro_rata_fee, total_bps): (i128, u32) = if !shares.is_empty() {
            let mut total_fee: i128 = 0;
            let mut total_bps: u32 = 0;
            for share in shares.iter() {
                let fee = calculate_fee_and_yield(amount, share.bps)?;
                total_fee = total_fee
                    .checked_add(fee)
                    .ok_or(EscrowError::MathOverflow)?;
                total_fee += bps_fee(amount, share.bps);
                total_bps += share.bps;
            }
            (total_fee, total_bps)
        } else {
            let fee_bps = effective_fee_bps(env, merchant)? as i128;
            Ok((amount / 10_000i128) * fee_bps + ((amount % 10_000i128) * fee_bps) / 10_000i128)
            let fee_config: FeeConfig = Self::get_fee_config(env.clone())?;
            calculate_fee_and_yield(amount, fee_config.fee_bps)
        }
            let fee_config: FeeConfig = Self::get_fee_config(env.clone())?;
            (bps_fee(amount, fee_config.fee_bps), fee_config.fee_bps)
        };

        Ok(apply_fee_floor(amount, total_bps, pro_rata_fee, min_fee))
    }

    /// Computes the net seller payout and platform fee for `amount` (issue #27).
    /// Pure calculation — see `distribute_fee` for the transfer side.
    fn compute_payout(
        env: &Env,
        merchant: &Address,
        amount: i128,
    ) -> Result<ReleasePayout, EscrowError> {
        let fee = Self::compute_fee_amount(env, merchant, amount)?;
        let fee_config: FeeConfig = Self::get_fee_config(env.clone())?;
        Ok(ReleasePayout {
            seller_net: amount.checked_sub(fee).ok_or(EscrowError::MathOverflow)?,
            fee,
            treasury: fee_config.treasury,
        })
    }

    /// Add a token to the escrow whitelist. Admin-only.
    pub fn add_token(
        env: Env,
        admin: Address,
        token_address: Address,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        admin_actions::consume(&env, AdminAction::AddToken(token_address.clone()))?;

        if Self::is_token_allowed(env.clone(), token_address.clone()) {
            return Ok(true);
        }

        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::AllowedTokenCount)
            .unwrap_or(0);

        env.storage()
            .instance()
            .set(&DataKey::AllowedToken(token_address.clone()), &count);
        env.storage()
            .instance()
            .set(&DataKey::AllowedTokenAt(count), &token_address);
        env.storage()
            .instance()
            .set(&DataKey::AllowedTokenCount, &(count + 1));

        // Admin token allowlist changes are logged so off-chain indexers can
        // track which tokens are safe for escrow use (issue #283).
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("tok_add")),
            TokenAllowlistUpdatedEvent {
                admin,
                token: token_address,
                allowed: true,
            },
        );

        Ok(true)
    }

    /// Remove a token from the escrow whitelist. Admin-only.
    pub fn remove_token(
        env: Env,
        admin: Address,
        token_address: Address,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        admin_actions::consume(&env, AdminAction::RemoveToken(token_address.clone()))?;

        let index_opt: Option<u32> = env
            .storage()
            .instance()
            .get(&DataKey::AllowedToken(token_address.clone()));

        if let Some(idx) = index_opt {
            let count: u32 = env
                .storage()
                .instance()
                .get(&DataKey::AllowedTokenCount)
                .unwrap_or(0);

            if count > 0 {
                let last_idx = count - 1;
                if idx != last_idx {
                    // Swap with the last element
                    let last_token: Address = env
                        .storage()
                        .instance()
                        .get(&DataKey::AllowedTokenAt(last_idx))
                        .unwrap();
                    env.storage()
                        .instance()
                        .set(&DataKey::AllowedTokenAt(idx), &last_token);
                    env.storage()
                        .instance()
                        .set(&DataKey::AllowedToken(last_token), &idx);
                }

                // Remove the target token from mappings
                env.storage()
                    .instance()
                    .remove(&DataKey::AllowedToken(token_address.clone()));
                env.storage()
                    .instance()
                    .remove(&DataKey::AllowedTokenAt(last_idx));
                env.storage()
                    .instance()
                    .set(&DataKey::AllowedTokenCount, &last_idx);

                // Admin token allowlist changes are logged so off-chain
                // indexers can track which tokens are safe for escrow use
                // (issue #283).
                env.events().publish(
                    (symbol_short!("escrow"), symbol_short!("tok_rem")),
                    TokenAllowlistUpdatedEvent {
                        admin,
                        token: token_address,
                        allowed: false,
                    },
                );
            }
        }

        Ok(true)
    }

    /// Returns true when the token is approved for escrow deposits.
    pub fn is_token_allowed(env: Env, token_address: Address) -> bool {
        env.storage()
            .instance()
            .has(&DataKey::AllowedToken(token_address))
    }

    /// List all tokens currently approved for escrow deposits.
    pub fn list_tokens(env: Env) -> soroban_sdk::Vec<Address> {
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::AllowedTokenCount)
            .unwrap_or(0);

        Self::list_tokens_paginated(env, 0, count)
    }

    /// List tokens currently approved for escrow deposits with pagination.
    pub fn list_tokens_paginated(env: Env, offset: u32, limit: u32) -> soroban_sdk::Vec<Address> {
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::AllowedTokenCount)
            .unwrap_or(0);

        let mut tokens = soroban_sdk::Vec::new(&env);
        let end = count.min(offset.saturating_add(limit));

        for i in offset..end {
            if let Some(token) = env.storage().instance().get(&DataKey::AllowedTokenAt(i)) {
                tokens.push_back(token);
            }
        }
        tokens
    }

    /// Fund the shared liquidity pool for a token so it can back instant
    /// settlements via `settle_from_pool`. Any account may contribute
    /// liquidity for a whitelisted token.
    pub fn fund_pool(
        env: Env,
        funder: Address,
        token: Address,
        amount: i128,
    ) -> Result<i128, EscrowError> {
        funder.require_auth();

        if !Self::is_token_allowed(env.clone(), token.clone()) {
            return Err(EscrowError::TokenNotWhitelisted);
        }
        if amount <= 0 {
            return Err(EscrowError::InvalidAmount);
        }

        let token_client = soroban_sdk::token::Client::new(&env, &token);
        token_client.transfer(&funder, &env.current_contract_address(), &amount);

        let pool_key = DataKey::LiquidityPool(token.clone());
        let mut pool: LiquidityPool =
            env.storage()
                .instance()
                .get(&pool_key)
                .unwrap_or(LiquidityPool {
                    token: token.clone(),
                    balance: 0,
                });
        pool.balance += amount;
        env.storage().instance().set(&pool_key, &pool);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("pl_fund")),
            PoolFundedEvent {
                token,
                funder,
                amount,
                new_balance: pool.balance,
            },
        );

        Ok(pool.balance)
    }

    /// Withdraw liquidity from a token's pool. Admin-only.
    pub fn withdraw_from_pool(
        env: Env,
        admin: Address,
        token: Address,
        amount: i128,
    ) -> Result<i128, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        if amount <= 0 {
            return Err(EscrowError::InvalidAmount);
        }

        let pool_key = DataKey::LiquidityPool(token.clone());
        let mut pool: LiquidityPool = env
            .storage()
            .instance()
            .get(&pool_key)
            .ok_or(EscrowError::PoolNotFound)?;

        if amount > pool.balance {
            return Err(EscrowError::InsufficientPoolBalance);
        }

        let token_client = soroban_sdk::token::Client::new(&env, &token);
        token_client.transfer(&env.current_contract_address(), &admin, &amount);

        pool.balance -= amount;
        env.storage().instance().set(&pool_key, &pool);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("pl_wdrw")),
            PoolWithdrawnEvent {
                token,
                admin,
                amount,
                new_balance: pool.balance,
            },
        );

        Ok(pool.balance)
    }

    /// Instantly settle a funded escrow by paying the seller out of the
    /// shared liquidity pool for that escrow's token, instead of going
    /// through the ordinary buyer/admin-triggered `release` flow. Admin-only.
    ///
    /// The settled amount is debited from the pool's tracked balance so that
    /// `pool.balance` always reflects real, currently unencumbered liquidity
    /// and a later `withdraw_from_pool` cannot over-commit the same tokens.
    pub fn settle_from_pool(
        env: Env,
        escrow_id: u64,
        caller: Address,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();
        if !Self::is_admin(env.clone(), caller.clone()) {
            return Err(EscrowError::Unauthorized);
        }

        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };

        if record.status != EscrowStatus::Funded {
            return Err(EscrowError::InvalidStatus);
        }

        let remaining = record.amount - record.released_amount - record.refunded_amount;
        if remaining <= 0 {
            return Err(EscrowError::ZeroAmount);
        }

        let pool_key = DataKey::LiquidityPool(record.token.clone());
        let mut pool: LiquidityPool = env
            .storage()
            .instance()
            .get(&pool_key)
            .ok_or(EscrowError::PoolNotFound)?;
        if pool.balance < remaining {
            return Err(EscrowError::InsufficientPoolBalance);
        }

        Self::assert_value_conservation_invariant(&record, remaining, 0)?;

        let token_client = soroban_sdk::token::Client::new(&env, &record.token);
        token_client.transfer(&env.current_contract_address(), &record.seller, &remaining);

        pool.balance = pool
            .balance
            .checked_sub(remaining)
            .ok_or(EscrowError::InsufficientPoolBalance)?;
        env.storage().instance().set(&pool_key, &pool);

        record.released_amount = record
            .released_amount
            .checked_add(remaining)
            .ok_or(EscrowError::MathOverflow)?;
        record.status = EscrowStatus::Released;
        Self::try_mint_purchase_receipt(&env, &mut record);
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);
        update_merchant_volume_and_tier(&env, &record.seller, remaining);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("pl_stl"), escrow_id),
            PoolSettledEvent {
                escrow_id,
                token: record.token.clone(),
                seller: record.seller.clone(),
                amount: remaining,
            },
        );

        Ok(true)
    }

    /// Read-only getter for a token's liquidity pool balance.
    ///
    /// # Errors
    /// Returns [`EscrowError::PoolNotFound`] when no pool has ever been funded
    /// for the given token, so callers can distinguish an unfunded pool from a
    /// funded one that is currently empty.
    pub fn get_liquidity_pool(env: Env, token: Address) -> Result<LiquidityPool, EscrowError> {
        env.storage()
            .instance()
            .get(&DataKey::LiquidityPool(token))
            .ok_or(EscrowError::PoolNotFound)
    }

    /// Commit the slippage window a cross-currency path payment for `escrow_id`
    /// must settle inside (issue #337).
    ///
    /// The buyer (or an admin) fixes the tolerance *before* a route is
    /// executed, so the party that executes the swap can never choose how much
    /// slippage is tolerated. Once a window is committed it may only be
    /// **tightened** — `min_output_amount` may rise and `max_input_amount` may
    /// fall — and any attempt to widen it is rejected with
    /// [`EscrowError::SlippageBoundsTooLoose`]. That is what makes the limit
    /// dynamic over the life of an escrow: a user can lock in a tight window
    /// before broadcasting a route and tighten it further at any point before
    /// settlement, but a front-runner cannot relax it to let a worse fill
    /// through.
    ///
    /// # Errors
    /// - [`EscrowError::NotFound`] — unknown escrow.
    /// - [`EscrowError::Unauthorized`] — caller is neither the buyer nor an admin.
    /// - [`EscrowError::InvalidStatus`] — escrow is not `Created` or `Funded`.
    /// - [`EscrowError::InvalidSlippageBounds`] — either bound is zero or negative.
    /// - [`EscrowError::SlippageBoundsTooLoose`] — the new window is wider than
    ///   the committed one.
    pub fn set_slippage_bounds(
        env: Env,
        escrow_id: u64,
        caller: Address,
        bounds: SlippageBounds,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Escrow(escrow_id))
            .ok_or(EscrowError::NotFound)?;

        if caller != record.buyer && !Self::is_admin(env.clone(), caller.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        check_not_terminal(&record)?;
        if record.status != EscrowStatus::Created && record.status != EscrowStatus::Funded {
            return Err(EscrowError::InvalidStatus);
        }
        if bounds.min_output_amount <= 0 || bounds.max_input_amount <= 0 {
            return Err(EscrowError::InvalidSlippageBounds);
        }

        let bounds_key = DataKey::PathSlippageBounds(escrow_id);
        let committed: Option<SlippageBounds> = env.storage().persistent().get(&bounds_key);
        if let Some(previous) = &committed {
            if bounds.min_output_amount < previous.min_output_amount
                || bounds.max_input_amount > previous.max_input_amount
            {
                return Err(EscrowError::SlippageBoundsTooLoose);
            }
        }

        env.storage().persistent().set(&bounds_key, &bounds);
        env.storage().persistent().extend_ttl(
            &bounds_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("slipset"), escrow_id),
            SlippageBoundsSetEvent {
                escrow_id,
                min_output_amount: bounds.min_output_amount,
                max_input_amount: bounds.max_input_amount,
                set_by: caller,
            },
        );

        Ok(true)
    }

    /// Read-only getter for an escrow's committed slippage window.
    ///
    /// Returns `None` while no window has been committed, so callers can
    /// distinguish "never set" from a committed window of `0` (which is
    /// rejected at set time and can never be stored). Never mutates state.
    pub fn get_slippage_bounds(env: Env, escrow_id: u64) -> Option<SlippageBounds> {
        let bounds: Option<SlippageBounds> = env
            .storage()
            .persistent()
            .get(&DataKey::PathSlippageBounds(escrow_id));
        if bounds.is_some() {
            env.storage().persistent().extend_ttl(
                &DataKey::PathSlippageBounds(escrow_id),
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );
        }
        bounds
    }

    /// Read-only slippage check for off-chain quoting and monitoring (issue #337).
    ///
    /// Returns `true` only when `escrow_id` has a committed window that the
    /// supplied amounts satisfy (`output_amount >= min_output_amount` and
    /// `input_amount <= max_input_amount`) *and* the escrow still holds
    /// `input_amount` of its escrowed token. Never mutates state.
    pub fn is_within_slippage_bounds(
        env: Env,
        escrow_id: u64,
        input_amount: i128,
        output_amount: i128,
    ) -> bool {
        let bounds: Option<SlippageBounds> = env
            .storage()
            .persistent()
            .get(&DataKey::PathSlippageBounds(escrow_id));
        let bounds = match bounds {
            Some(b) => b,
            None => return false,
        };
        let record: Option<EscrowRecord> =
            env.storage().persistent().get(&DataKey::Escrow(escrow_id));
        let record = match record {
            Some(r) => r,
            None => return false,
        };
        let remaining = record.amount - record.released_amount - record.refunded_amount;
        remaining >= input_amount
            && output_amount >= bounds.min_output_amount
            && input_amount <= bounds.max_input_amount
    }

    /// Settle a funded escrow in a different currency by executing a DEX route
    /// under the slippage window committed by `set_slippage_bounds`
    /// (issue #337).
    ///
    /// # Flow
    /// 1. The escrow must be `Funded` and the caller must be its buyer or an
    ///    admin. The route must be a well-formed cross-currency chain: every
    ///    token whitelisted, no hop converting a token into itself, the first
    ///    leg consuming the escrowed token, each leg feeding the next, and the
    ///    last leg paying out a token other than the escrowed one.
    /// 2. `router.execute_path(route, bounds, <this contract>)` is invoked. The
    ///    router delivers the destination token to this contract and reports
    ///    the amounts it moved.
    /// 3. The **reported** amounts are checked against the committed
    ///    [`SlippageBounds`]. A route that consumed more than
    ///    `max_input_amount` or delivered less than `min_output_amount` reverts
    ///    the whole invocation — the router's delivery included — with
    ///    [`EscrowError::SlippageExceeded`]. This is the anti-sandwich guard:
    ///    the guard reads the realised fill, never the requested amount.
    /// 4. On success the escrow pays the router `input_amount` net of the
    ///    platform fee, forwards `output_amount` of the destination token to
    ///    the seller, and books the spent input against `released_amount`.
    ///
    /// The router may consume any amount up to `max_input_amount`; the
    /// contract — not the executor — decides what is acceptable.
    ///
    /// # Errors
    /// - [`EscrowError::NotFound`], [`EscrowError::Unauthorized`],
    ///   [`EscrowError::InvalidStatus`], [`EscrowError::InvalidAddress`].
    /// - [`EscrowError::SlippageBoundsNotSet`] — no window was committed.
    /// - [`EscrowError::InvalidPathRoute`] / [`EscrowError::TokenNotWhitelisted`].
    /// - [`EscrowError::PathExecutionFailed`] — the router call failed or its
    ///   result is not usable.
    /// - [`EscrowError::InsufficientEscrowBalance`] — the route consumed more
    ///   than the escrow still holds.
    /// - [`EscrowError::SlippageExceeded`] — the realised fill is outside the
    ///   committed window.
    pub fn execute_path_payment(
        env: Env,
        escrow_id: u64,
        settled_by: Address,
        router: Address,
        route: PathRoute,
    ) -> Result<PathExecutionResult, EscrowError> {
        settled_by.require_auth();
        if is_zero_address(&env, &router) {
            return Err(EscrowError::InvalidAddress);
        }

        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;
        if settled_by != record.buyer && !Self::is_admin(env.clone(), settled_by.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        check_not_terminal(&record)?;
        if record.status != EscrowStatus::Funded {
            return Err(EscrowError::InvalidStatus);
        }

        let bounds: SlippageBounds = env
            .storage()
            .persistent()
            .get(&DataKey::PathSlippageBounds(escrow_id))
            .ok_or(EscrowError::SlippageBoundsNotSet)?;
        let destination_token = Self::validate_path_route(&env, &record, &route)?;

        let remaining = record.amount - record.released_amount - record.refunded_amount;
        if remaining <= 0 {
            return Err(EscrowError::ZeroAmount);
        }

        let executed = env.try_invoke_contract::<PathExecutionResult, InvokeError>(
            &router,
            &Symbol::new(&env, "execute_path"),
            soroban_sdk::vec![
                &env,
                route.into_val(&env),
                bounds.into_val(&env),
                env.current_contract_address().to_val(),
            ],
        );
        let result = match executed {
            Ok(Ok(result)) => result,
            Ok(Err(_)) | Err(_) => return Err(EscrowError::PathExecutionFailed),
        };

        if result.input_amount <= 0 || result.output_amount <= 0 {
            return Err(EscrowError::PathExecutionFailed);
        }
        if result.output_amount < bounds.min_output_amount
            || result.input_amount > bounds.max_input_amount
        {
            return Err(EscrowError::SlippageExceeded);
        }
        if result.input_amount > remaining {
            return Err(EscrowError::InsufficientEscrowBalance);
        }

        let token_client = soroban_sdk::token::Client::new(&env, &record.token);
        let payout = Self::compute_payout(&env, result.input_amount)?;
        Self::distribute_fee(&env, &token_client, payout.fee)?;
        token_client.transfer(&env.current_contract_address(), &router, &payout.seller_net);
        soroban_sdk::token::Client::new(&env, &destination_token).transfer(
            &env.current_contract_address(),
            &record.seller,
            &result.output_amount,
        );

        record.released_amount = record
            .released_amount
            .checked_add(result.input_amount)
            .ok_or(EscrowError::InsufficientEscrowBalance)?;
        let new_remaining = record.amount - record.released_amount - record.refunded_amount;
        let fully_released = new_remaining == 0;
        if fully_released {
            record.status = EscrowStatus::Released;
        }
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("pathset"), escrow_id),
            PathPaymentSettledEvent {
                escrow_id,
                router,
                seller: record.seller.clone(),
                leg_count: route.legs.len(),
                input_amount: result.input_amount,
                output_amount: result.output_amount,
                min_output_amount: bounds.min_output_amount,
                max_input_amount: bounds.max_input_amount,
                settled_by,
            },
        );

        Ok(result)
    }

    /// Validate a cross-currency route against its escrow and return the
    /// destination token the route must pay out (issue #337).
    ///
    /// Rejects empty and over-long routes, zero-address pools or tokens, hops
    /// that convert a token into itself, tokens that are not whitelisted for
    /// escrow, a route that does not start from the escrowed token, a broken
    /// token chain between hops, and a route that ends back on the escrowed
    /// token (which would not be a cross-currency conversion at all).
    fn validate_path_route(
        env: &Env,
        record: &EscrowRecord,
        route: &PathRoute,
    ) -> Result<Address, EscrowError> {
        let leg_count = route.legs.len();
        if leg_count == 0 || leg_count > MAX_PATH_LEGS {
            return Err(EscrowError::InvalidPathRoute);
        }
        let first = match route.legs.get(0) {
            Some(leg) => leg,
            None => return Err(EscrowError::InvalidPathRoute),
        };
        if first.token_in != record.token {
            return Err(EscrowError::InvalidPathRoute);
        }

        let mut expected_token_in: Option<Address> = None;
        for leg in route.legs.iter() {
            if is_zero_address(env, &leg.pool)
                || is_zero_address(env, &leg.token_in)
                || is_zero_address(env, &leg.token_out)
            {
                return Err(EscrowError::InvalidAddress);
            }
            if leg.token_in == leg.token_out {
                return Err(EscrowError::InvalidPathRoute);
            }
            if let Some(expected) = &expected_token_in {
                if &leg.token_in != expected {
                    return Err(EscrowError::InvalidPathRoute);
                }
            }
            if !Self::is_token_allowed(env.clone(), leg.token_in.clone())
                || !Self::is_token_allowed(env.clone(), leg.token_out.clone())
            {
                return Err(EscrowError::TokenNotWhitelisted);
            }
            expected_token_in = Some(leg.token_out.clone());
        }

        let destination = expected_token_in.ok_or(EscrowError::InvalidPathRoute)?;
        if destination == record.token {
            return Err(EscrowError::InvalidPathRoute);
        }
        Ok(destination)
    }

    /// Publish a delivery Merkle root for a UTC date.
    ///
    /// Only the primary admin or a co-admin may publish roots. Dates are
    /// represented as Unix epoch days. A stored root cannot be replaced;
    /// reads and proof submissions refresh its persistent-storage TTL.
    pub fn publish_merkle_root(
        env: Env,
        caller: Address,
        date: u64,
        root: BytesN<32>,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();
        if !Self::is_admin(env.clone(), caller) {
            return Err(EscrowError::Unauthorized);
        }

        let key = DataKey::MerkleRoot(date);
        if env.storage().persistent().has(&key) {
            return Err(EscrowError::AlreadyInitialized);
        }
        env.storage().persistent().set(&key, &root);
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("merkroot"), date),
            root,
        );
        Ok(true)
    }

    /// Return the published delivery root for a UTC epoch date, if present.
    pub fn get_merkle_root(env: Env, date: u64) -> Option<BytesN<32>> {
        let key = DataKey::MerkleRoot(date);
        let root = env.storage().persistent().get(&key);
        if root.is_some() {
            env.storage().persistent().extend_ttl(
                &key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );
        }
        root
    }

    /// Verify a Merkle path against the root carried in `proof`.
    ///
    /// Leaves and internal nodes use SHA-256. Sibling ordering is determined
    /// by the corresponding bit of `index`; paths are limited to 32 levels.
    pub fn verify_merkle_proof(env: Env, proof: MerkleDeliveryProof) -> bool {
        Self::verify_merkle_path(&env, &proof)
    }

    /// Release a funded escrow after proving its order is included in the
    /// published delivery root for `date`.
    ///
    /// The leaf convention is `SHA-256(order_id)`. The buyer must authorize
    /// the call; a proof for a different order or an unpublished root fails.
    pub fn release_with_merkle_proof(
        env: Env,
        escrow_id: u64,
        buyer: Address,
        date: u64,
        proof: MerkleDeliveryProof,
    ) -> Result<bool, EscrowError> {
        buyer.require_auth();
        Self::enter_release(&env)?;
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;
        if buyer != record.buyer {
            return Err(EscrowError::Unauthorized);
        }
        Self::validate_release_status(&record)?;
        Self::ensure_not_milestone_escrow(&env, escrow_id)?;

        let published_root: Option<BytesN<32>> =
            env.storage().persistent().get(&DataKey::MerkleRoot(date));
        let published_root = published_root.ok_or(EscrowError::InvalidMerkleProof)?;
        env.storage().persistent().extend_ttl(
            &DataKey::MerkleRoot(date),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        let expected_leaf: BytesN<32> = env
            .crypto()
            .sha256(&Bytes::from_array(&env, &record.order_id.to_array()))
            .into();
        if proof.root != published_root
            || proof.leaf != expected_leaf
            || !Self::verify_merkle_path(&env, &proof)
        {
            return Err(EscrowError::InvalidMerkleProof);
        }

        Self::execute_release(
            &env,
            escrow_id,
            &key,
            record.clone(),
            buyer,
            record.amount - record.released_amount - record.refunded_amount,
            0,
        )?;
        Self::exit_release(&env);
        Ok(true)
    }

    fn verify_merkle_path(env: &Env, proof: &MerkleDeliveryProof) -> bool {
        if proof.proof.len() > u32::BITS {
            return false;
        }

        let mut hash = proof.leaf.clone();
        let mut index = proof.index;
        for sibling in proof.proof.iter() {
            let (left, right) = if index & 1 == 0 {
                (hash, sibling)
            } else {
                (sibling, hash)
            };
            let mut node = Bytes::new(env);
            node.append(&Bytes::from_array(env, &left.to_array()));
            node.append(&Bytes::from_array(env, &right.to_array()));
            hash = env.crypto().sha256(&node).into();
            index >>= 1;
        }

        index == 0 && hash == proof.root
    }

    /// Create an escrow in unfunded `Created` status.
    ///
    /// Optional metadata parameters (order_hash and schema) can be provided to store
    /// a hash of off-chain order details for later verification.
    ///
    /// # Errors
    /// Returns [`EscrowError::InvalidAddress`] when buyer, seller, token, or
    /// treasury are the zero address, and
    /// [`EscrowError::InvalidEscrowParticipants`] when buyer and seller are
    /// the same address.
    // Reason: Soroban ABI entry point — 9 args is part of the published
    // on-chain signature and cannot be restructured without a breaking change.
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        env: Env,
        buyer: Address,
        seller: Address,
        token: Address,
        amount: i128,
        order_id: BytesN<32>,
        timeout_ledgers: u32,
        order_hash: Option<BytesN<32>>,
        schema: Option<Symbol>,
    ) -> Result<u64, EscrowError> {
        if is_zero_address(&env, &buyer)
            || is_zero_address(&env, &seller)
            || is_zero_address(&env, &token)
        {
            return Err(EscrowError::InvalidAddress);
        }
        if buyer == seller {
            return Err(EscrowError::InvalidEscrowParticipants);
        }

        buyer.require_auth();
        Self::create_internal(
            env,
            buyer,
            seller,
            token,
            amount,
            order_id,
            timeout_ledgers,
            order_hash,
            schema,
        )
    }

    /// Shared `create` logic used by both `create` and `batch_deposit`.
    /// Callers are responsible for their own validation and
    /// `buyer.require_auth()` — batch callers authorize the buyer once for
    /// the whole batch instead of once per order, since Soroban's auth
    /// tracker only matches one invocation of `require_auth` per address per
    /// top-level call.
    // Reason: mirrors the `create` ABI signature so batch callers stay uniform.
    #[allow(clippy::too_many_arguments)]
    fn create_internal(
        env: Env,
        buyer: Address,
        seller: Address,
        token: Address,
        amount: i128,
        order_id: BytesN<32>,
        timeout_ledgers: u32,
        order_hash: Option<BytesN<32>>,
        schema: Option<Symbol>,
    ) -> Result<u64, EscrowError> {
        if let Some(pause_state) = env
            .storage()
            .instance()
            .get::<DataKey, EscrowPauseState>(&DataKey::PauseState)
        {
            if pause_state.create_paused {
                return Err(EscrowError::CreationPaused);
            }
        }

        if let Some(registry) = env
            .storage()
            .instance()
            .get::<_, Address>(&DataKey::MerchantRegistry)
        {
            let args = soroban_sdk::vec![&env, seller.to_val()];
            let status = env.try_invoke_contract::<bool, InvokeError>(
                &registry,
                &Symbol::new(&env, "is_merchant_trading"),
                args,
            );
            match status {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) => return Err(EscrowError::MerchantNotTrading),
                _ => return Err(EscrowError::MerchantStatusCheckFailed),
            }
        }

        if !Self::is_token_allowed(env.clone(), token.clone()) {
            return Err(EscrowError::TokenNotWhitelisted);
        }

        if amount <= 0 {
            return Err(EscrowError::InvalidAmount);
        }
        let limits: EscrowAmountLimits = Self::get_limits(env.clone())?;
        if amount < limits.min_amount {
            return Err(EscrowError::AmountBelowMin);
        }
        if amount > limits.max_amount {
            return Err(EscrowError::AmountAboveMax);
        }

        // Metadata must be supplied fully (both order_hash and schema) or not
        // at all. A half-set entry would otherwise be persisted with the set
        // half and stale/absent other half, silently dropping metadata — reject
        // it loudly with a typed error instead (issue #38). This shared path is
        // used by `create`, `deposit`, and every `batch_deposit` entry, so all
        // three behave identically.
        if order_hash.is_some() != schema.is_some() {
            return Err(EscrowError::InvalidMetadata);
        }

        let mut last_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::LastEscrowId)
            .unwrap_or(0);
        last_id += 1;
        env.storage()
            .instance()
            .set(&DataKey::LastEscrowId, &last_id);

        let timeout_ledger = timeout_ledgers
            .checked_add(env.ledger().sequence())
            .ok_or(EscrowError::InvalidExtension)?;
        let record = EscrowRecord {
            escrow_id: last_id,
            buyer: buyer.clone(),
            seller: seller.clone(),
            token: token.clone(),
            amount,
            released_amount: 0,
            refunded_amount: 0,
            status: EscrowStatus::Created,
            order_id: order_id.clone(),
            created_at: env.ledger().timestamp(),
            updated_at: env.ledger().timestamp(),
            timeout_ledger,
            receipt_token_id: None,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Escrow(last_id), &record);

        // Seed the cancellation guard for the new order (issue #355). The
        // escrow is only `Created` at this point, so a seller that front-runs
        // a pending buyer deposit with `cancel` is exactly the case this state
        // exists for: the window runs from creation until
        // `created_sequence + cancel_lockout_ledgers`.
        Self::init_cancel_guard(&env, last_id);

        // Maintain global escrow ID index for list_escrows (issue #49).
        let mut all_ids: soroban_sdk::Vec<u64> = env
            .storage()
            .instance()
            .get(&DataKey::EscrowIds)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env));
        all_ids.push_back(last_id);
        env.storage().instance().set(&DataKey::EscrowIds, &all_ids);

        // Keep each buyer index entry separate so no persistent value grows
        // with the buyer's lifetime escrow count.
        let buyer_count_key = DataKey::BuyerEscrowCount(buyer.clone());
        let buyer_count: u32 = env
            .storage()
            .persistent()
            .get(&buyer_count_key)
            .unwrap_or(0);
        let next_buyer_count = buyer_count
            .checked_add(1)
            .ok_or(EscrowError::InvalidExtension)?;
        env.storage().persistent().set(
            &DataKey::BuyerEscrowAt(buyer.clone(), buyer_count),
            &last_id,
        );
        env.storage()
            .persistent()
            .set(&buyer_count_key, &next_buyer_count);

        // Persist each metadata half independently so a later call can supply
        // the missing one (issue #181). Both halves are only ever stored
        // together here: `order_hash`/`schema` are validated to be
        // all-or-nothing, so a half-set entry can never silently drop metadata
        // (issue #38). Borrow here; the combined match below moves the
        // originals into the event.
        if let Some(hash) = &order_hash {
            env.storage()
                .persistent()
                .set(&DataKey::EscrowMetadataHash(last_id), hash);
        }
        if let Some(sch) = &schema {
            env.storage()
                .persistent()
                .set(&DataKey::EscrowMetadataSchema(last_id), sch);
        }

        // The metadata event is only emitted once both halves are present. The
        // order id is carried as a topic so indexers can filter by escrow
        // without deserializing the event body (issue #142).
        if let (Some(hash), Some(sch)) = (order_hash, schema) {
            env.events().publish(
                (
                    symbol_short!("escrow"),
                    symbol_short!("metadata"),
                    order_id.clone(),
                ),
                EscrowMetadataEvent {
                    escrow_id: order_id.clone(),
                    order_hash: hash,
                    schema: sch,
                },
            );
        }

        // A long-lived, open escrow must not be evicted while it is still
        // being read: bump the TTL of the record, its buyer index, and the
        // contract instance (mirrors marketplace/reputation).
        let storage = env.storage().persistent();
        storage.extend_ttl(
            &DataKey::Escrow(last_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        storage.extend_ttl(
            &buyer_count_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("created"), last_id),
            EscrowCreatedEvent {
                escrow_id: last_id,
                buyer: record.buyer.clone(),
                seller: record.seller.clone(),
                token: record.token.clone(),
                amount: record.amount,
                order_id,
                timeout_ledger,
            },
        );

        Ok(last_id)
    }

    /// Fund an existing escrow that is in `Created` status.
    pub fn fund(env: Env, escrow_id: u64, buyer: Address) -> Result<bool, EscrowError> {
        buyer.require_auth();

        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };

        if buyer != record.buyer {
            return Err(EscrowError::Unauthorized);
        }

        if record.status == EscrowStatus::Funded {
            return Err(EscrowError::AlreadyFunded);
        }

        if record.status == EscrowStatus::Cancelled {
            return Err(EscrowError::AlreadyCancelled);
        }

        if record.status != EscrowStatus::Created {
            return Err(EscrowError::InvalidStatus);
        }

        let token_client = soroban_sdk::token::Client::new(&env, &record.token);
        let balance_before = token_client.balance(&env.current_contract_address());
        token_client.transfer(&buyer, &env.current_contract_address(), &record.amount);
        let net_received = verify_received_deposit_delta(
            &env,
            &token_client,
            &env.current_contract_address(),
            balance_before,
            record.amount,
        )?;

        record.amount = net_received;
        record.status = EscrowStatus::Funded;
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);

        Ok(true)
    }

    /// Cancel an escrow that has been created but not yet funded.
    /// Only the merchant (seller) may call.
    ///
    /// A seller may not cancel unilaterally while the order is still inside
    /// its front-running protection window (issue #355). An escrow is created
    /// before the buyer commits funds, so without the window a seller that
    /// watches the mempool could front-run a pending `fund`/`deposit` with a
    /// `cancel` for the same order and take the order (and the inventory it
    /// reserves) out from under the buyer. A contract cannot observe the
    /// mempool, so the window is the deterministic approximation: for the
    /// first `cancel_lockout_ledgers` ledgers after creation — and after a
    /// seller `accept_order` — a unilateral cancel is rejected with
    /// [`EscrowError::CancelLockoutActive`].
    ///
    /// Cancellation inside the window requires one of the two escape hatches
    /// this design keeps on purpose:
    /// * explicit mutual agreement — the buyer records consent with
    ///   `agree_cancel`, after which the seller may cancel immediately, or
    /// * expiry — the escrow's own timeout has been reached, which is the
    ///   seller's existing right to clean up a stalled order.
    ///
    /// Once the buyer's deposit is on-chain the escrow is `Funded` and the
    /// existing [`EscrowError::AlreadyFunded`] guard rejects the cancel, so a
    /// submitted deposit can never be unwound by the seller.
    ///
    /// # Errors
    /// Returns [`EscrowError::NotFound`] when no escrow exists,
    /// [`EscrowError::Unauthorized`] when the caller is not the seller,
    /// [`EscrowError::AlreadyFunded`]/[`EscrowError::AlreadyCancelled`]/
    /// [`EscrowError::InvalidStatus`] for non-`Created` escrows, and
    /// [`EscrowError::CancelLockoutActive`] while the protection window runs
    /// and neither agreement nor timeout applies.
    pub fn cancel(
        env: Env,
        escrow_id: u64,
        caller: Address,
        reason: Symbol,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();

        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };

        if caller != record.seller {
            return Err(EscrowError::Unauthorized);
        }

        // Single source of truth for "may this seller cancel right now?" —
        // the same evaluation backs `get_cancel_eligibility` (issue #355).
        let guard = Self::cancel_guard(&env, escrow_id);
        if let Some(block) = Self::cancel_block(&env, &record, &guard) {
            return Err(block.into_error());
        }

        record.status = EscrowStatus::Cancelled;
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);

        // The buyer's agreement authorized exactly this cancellation; it must
        // not survive it and authorize another one later.
        Self::clear_cancel_agreement(&env, escrow_id);

        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("cancelled"),
                record.order_id.clone(),
            ),
            EscrowCancelledEvent {
                escrow_id: record.order_id.clone(),
                cancelled_by: caller,
                reason,
            },
        );

        Ok(true)
    }

    /// Record that the seller accepts a created order (issue #355).
    ///
    /// Acceptance is what a buyer's `fund`/`deposit` is normally waiting on,
    /// so it re-anchors the front-running protection window: the seller gets
    /// no shorter a window than the one that started at creation, and the
    /// buyer is guaranteed a full `cancel_lockout_ledgers` window in which to
    /// fund the order it just accepted. The window can therefore only ever be
    /// extended, never shortened, by accepting.
    ///
    /// Accepting is idempotent with respect to the recorded ledger only in the
    /// sense that a second call simply re-anchors the window to the current
    /// ledger; the buyer is never left with less time than the configured
    /// window, and the escrow's timeout still bounds how long a `Created`
    /// escrow can be held. An escrow with no readable snapshot is left failing
    /// closed instead of being given a window.
    ///
    /// # Errors
    /// Returns [`EscrowError::NotFound`] when no escrow exists,
    /// [`EscrowError::Unauthorized`] when the caller is not the seller, and
    /// [`EscrowError::InvalidStatus`] when the escrow is not `Created`.
    pub fn accept_order(env: Env, escrow_id: u64, seller: Address) -> Result<bool, EscrowError> {
        seller.require_auth();

        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Escrow(escrow_id))
            .ok_or(EscrowError::NotFound)?;

        if seller != record.seller {
            return Err(EscrowError::Unauthorized);
        }

        if record.status != EscrowStatus::Created {
            return Err(EscrowError::InvalidStatus);
        }

        let now = env.ledger().sequence();
        let mut guard = Self::cancel_guard(&env, escrow_id);
        // An escrow whose window cannot be read fails closed
        // (`CANCEL_LOCKOUT_NEVER`); accepting must never turn that into a
        // cancellable window, so the deadline is only re-anchored when the
        // escrow actually carries a snapshot.
        let has_snapshot = env
            .storage()
            .persistent()
            .has(&DataKey::CancelLockoutLedger(escrow_id));
        if has_snapshot {
            guard.allowed_ledger = now.saturating_add(guard.acceptance.cancel_lockout_ledgers);
        }
        let cancel_allowed_ledger = guard.allowed_ledger;

        guard.acceptance.seller_accepted = true;
        guard.acceptance.accepted_at_ledger = now;

        env.storage()
            .persistent()
            .set(&DataKey::OrderAcceptance(escrow_id), &guard.acceptance);
        if has_snapshot {
            env.storage().persistent().set(
                &DataKey::CancelLockoutLedger(escrow_id),
                &cancel_allowed_ledger,
            );
        }
        Self::bump_cancel_guard_ttl(&env, escrow_id);

        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("accepted"),
                escrow_id,
            ),
            EscrowOrderAcceptedEvent {
                escrow_id,
                seller,
                accepted_at_ledger: now,
                cancel_allowed_ledger,
            },
        );

        Ok(true)
    }

    /// Record the buyer's agreement to cancel an unfunded escrow
    /// (issue #355).
    ///
    /// This is the "explicit mutual agreement" half of the cancellation
    /// guard: while the protection window is running, the buyer can waive it
    /// on-chain, and the seller's next `cancel` succeeds. The agreement is
    /// consumed by the cancelling call and is rejected for any escrow that is
    /// not in `Created` status.
    ///
    /// # Errors
    /// Returns [`EscrowError::NotFound`] when no escrow exists,
    /// [`EscrowError::Unauthorized`] when the caller is not the escrow's
    /// buyer, and [`EscrowError::InvalidStatus`] when the escrow is not
    /// `Created`.
    pub fn agree_cancel(env: Env, escrow_id: u64, buyer: Address) -> Result<bool, EscrowError> {
        buyer.require_auth();

        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Escrow(escrow_id))
            .ok_or(EscrowError::NotFound)?;

        if buyer != record.buyer {
            return Err(EscrowError::Unauthorized);
        }

        if record.status != EscrowStatus::Created {
            return Err(EscrowError::InvalidStatus);
        }

        env.storage()
            .persistent()
            .set(&DataKey::CancelBuyerAgreement(escrow_id), &true);
        Self::bump_cancel_guard_ttl(&env, escrow_id);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("agreed"), escrow_id),
            EscrowCancelAgreedEvent {
                escrow_id,
                buyer,
                agreed_at_ledger: env.ledger().sequence(),
            },
        );

        Ok(true)
    }

    /// Read-only view of the order acceptance and cancel-protection state
    /// (issue #355).
    ///
    /// Escrows created before this state existed (or whose snapshot has been
    /// evicted) report a freshly derived state with `seller_accepted: false`
    /// and the contract-wide default window; `get_cancel_eligibility` still
    /// fails closed for those, so a missing snapshot can never widen a
    /// seller's cancellation rights.
    ///
    /// # Errors
    /// Returns [`EscrowError::NotFound`] when no escrow exists for `escrow_id`.
    pub fn get_order_acceptance(
        env: Env,
        escrow_id: u64,
    ) -> Result<OrderAcceptanceState, EscrowError> {
        let key = DataKey::Escrow(escrow_id);
        if !env.storage().persistent().has(&key) {
            return Err(EscrowError::NotFound);
        }
        let guard = Self::cancel_guard(&env, escrow_id);
        Self::bump_cancel_guard_ttl(&env, escrow_id);
        Ok(guard.acceptance)
    }

    /// Deterministic, read-only answer to "can `caller` cancel this escrow
    /// right now?" (issue #355).
    ///
    /// The answer is computed from the same guard `cancel` enforces, using
    /// only stored state and `env.ledger()`. Callers can therefore predict the
    /// outcome of a `cancel` transaction instead of guessing at mempool
    /// ordering, and a seller cannot obtain a different answer by racing a
    /// deposit. `reason` is `ok` when the cancel may proceed; see
    /// [`CancelEligibility`] for the full symbol list.
    pub fn get_cancel_eligibility(env: Env, escrow_id: u64, caller: Address) -> CancelEligibility {
        let record: EscrowRecord = match env.storage().persistent().get(&DataKey::Escrow(escrow_id))
        {
            Some(rec) => rec,
            None => {
                return CancelEligibility {
                    escrow_id,
                    eligible: false,
                    reason: symbol_short!("notfound"),
                };
            }
        };

        let (eligible, reason) = if caller != record.seller {
            (false, symbol_short!("notseller"))
        } else {
            let guard = Self::cancel_guard(&env, escrow_id);
            match Self::cancel_block(&env, &record, &guard) {
                Some(block) => (false, block.reason()),
                // `cancel_block` cleared the escrow for one of three reasons;
                // name the one that applied so a client knows which escape
                // hatch it is relying on.
                None if guard.buyer_agreed => (true, symbol_short!("agreed")),
                None if env.ledger().sequence() >= record.timeout_ledger => {
                    (true, symbol_short!("timeout"))
                }
                None => (true, symbol_short!("ok")),
            }
        };

        CancelEligibility {
            escrow_id,
            eligible,
            reason,
        }
    }

    /// Configure the default cancellation protection window for new escrows,
    /// in ledgers. Admin-only (issue #355).
    ///
    /// The value is snapshotted into each escrow at creation (and on
    /// acceptance), so changing it never retroactively alters the protection
    /// of an order that is already live. `0` disables the window entirely and
    /// is only appropriate for deployments that settle cancellation off-chain.
    ///
    /// # Errors
    /// Returns [`EscrowError::Unauthorized`] when the caller is not an admin
    /// and [`EscrowError::InvalidCancelLockout`] when `ledgers` exceeds
    /// [`MAX_CANCEL_LOCKOUT_LEDGERS`].
    pub fn set_cancel_lockout(env: Env, admin: Address, ledgers: u32) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin) {
            return Err(EscrowError::Unauthorized);
        }
        if ledgers > MAX_CANCEL_LOCKOUT_LEDGERS {
            return Err(EscrowError::InvalidCancelLockout);
        }
        env.storage()
            .instance()
            .set(&DataKey::CancelLockoutLedgers, &ledgers);
        Ok(true)
    }

    /// Current default cancellation protection window, in ledgers (issue #355).
    ///
    /// Returns [`DEFAULT_CANCEL_LOCKOUT_LEDGERS`] when no admin override is
    /// configured.
    pub fn get_cancel_lockout(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::CancelLockoutLedgers)
            .unwrap_or(DEFAULT_CANCEL_LOCKOUT_LEDGERS)
    }

    /// Seed the cancellation guard for a newly created escrow (issue #355).
    fn init_cancel_guard(env: &Env, escrow_id: u64) {
        let lockout_ledgers = Self::get_cancel_lockout(env.clone());
        let acceptance = OrderAcceptanceState {
            seller_accepted: false,
            accepted_at_ledger: 0,
            cancel_lockout_ledgers: lockout_ledgers,
        };
        let allowed_ledger = env.ledger().sequence().saturating_add(lockout_ledgers);

        env.storage()
            .persistent()
            .set(&DataKey::OrderAcceptance(escrow_id), &acceptance);
        env.storage()
            .persistent()
            .set(&DataKey::CancelLockoutLedger(escrow_id), &allowed_ledger);
        Self::bump_cancel_guard_ttl(env, escrow_id);
    }

    /// Read the cancellation guard for an escrow (issue #355).
    ///
    /// Fails closed: an escrow whose protection state cannot be read gets
    /// `CANCEL_LOCKOUT_NEVER`, which only the buyer's agreement or the escrow
    /// timeout can clear.
    fn cancel_guard(env: &Env, escrow_id: u64) -> CancelGuard {
        let acceptance: Option<OrderAcceptanceState> = env
            .storage()
            .persistent()
            .get(&DataKey::OrderAcceptance(escrow_id));
        let allowed_ledger: Option<u32> = env
            .storage()
            .persistent()
            .get(&DataKey::CancelLockoutLedger(escrow_id));
        let buyer_agreed: bool = env
            .storage()
            .persistent()
            .get(&DataKey::CancelBuyerAgreement(escrow_id))
            .unwrap_or(false);

        CancelGuard {
            acceptance: acceptance.unwrap_or(OrderAcceptanceState {
                seller_accepted: false,
                accepted_at_ledger: 0,
                cancel_lockout_ledgers: Self::get_cancel_lockout(env.clone()),
            }),
            allowed_ledger: allowed_ledger.unwrap_or(CANCEL_LOCKOUT_NEVER),
            buyer_agreed,
        }
    }

    /// Evaluate the cancellation guard for a seller cancel (issue #355).
    ///
    /// A unilateral cancel is permitted only when the escrow is `Created` and
    /// at least one of the following holds:
    /// * the buyer's deposit is on-chain — rejected, the escrow is no longer
    ///   `Created` and the buyer's funds stay locked;
    /// * the buyer agreed to the cancellation (`agree_cancel`);
    /// * the escrow's timeout has been reached; or
    /// * the protection window has fully elapsed.
    fn cancel_block(env: &Env, record: &EscrowRecord, guard: &CancelGuard) -> Option<CancelBlock> {
        if record.status == EscrowStatus::Funded {
            return Some(CancelBlock::Funded);
        }
        if record.status == EscrowStatus::Cancelled {
            return Some(CancelBlock::Cancelled);
        }
        if record.status != EscrowStatus::Created {
            return Some(CancelBlock::InvalidStatus);
        }
        if guard.buyer_agreed || env.ledger().sequence() >= record.timeout_ledger {
            return None;
        }
        if env.ledger().sequence() >= guard.allowed_ledger {
            return None;
        }
        Some(CancelBlock::Lockout)
    }

    /// Consume a recorded buyer agreement so it can only authorize one cancel.
    fn clear_cancel_agreement(env: &Env, escrow_id: u64) {
        env.storage()
            .persistent()
            .set(&DataKey::CancelBuyerAgreement(escrow_id), &false);
    }

    /// Keep the guard state alive for as long as the escrow record itself.
    ///
    /// Mirrors the TTL handling of [`EscrowRecord`]: the snapshot and the
    /// deadline are bumped with the same thresholds. Keys that are not stored
    /// are skipped — `extend_ttl` on a missing key is a host error, and the
    /// buyer agreement is only written when a buyer actually agrees.
    fn bump_cancel_guard_ttl(env: &Env, escrow_id: u64) {
        let storage = env.storage().persistent();
        let keys = [
            DataKey::OrderAcceptance(escrow_id),
            DataKey::CancelLockoutLedger(escrow_id),
            DataKey::CancelBuyerAgreement(escrow_id),
        ];
        for key in keys.iter() {
            if storage.has(key) {
                storage.extend_ttl(key, PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
            }
        }
    }

    /// Deposit funds into escrow for an order.
    /// Combined convenience call: creates an escrow and immediately funds it.
    // Reason: Soroban ABI entry point — 9 args is part of the published
    // on-chain signature and cannot be restructured without a breaking change.
    #[allow(clippy::too_many_arguments)]
    pub fn deposit(
        env: Env,
        buyer: Address,
        seller: Address,
        token: Address,
        amount: i128,
        order_id: BytesN<32>,
        timeout_ledgers: u32,
        order_hash: Option<BytesN<32>>,
        schema: Option<Symbol>,
    ) -> Result<u64, EscrowError> {
        if is_zero_address(&env, &buyer)
            || is_zero_address(&env, &seller)
            || is_zero_address(&env, &token)
        {
            return Err(EscrowError::InvalidAddress);
        }
        if buyer == seller {
            return Err(EscrowError::InvalidEscrowParticipants);
        }

        buyer.require_auth();
        Self::deposit_internal(
            env,
            buyer,
            seller,
            token,
            amount,
            order_id,
            timeout_ledgers,
            order_hash,
            schema,
        )
    }

    /// Shared `deposit` logic used by both `deposit` and `batch_deposit`.
    /// Callers are responsible for their own validation and
    /// `buyer.require_auth()`.
    // Reason: mirrors the `deposit` ABI signature so batch callers stay uniform.
    #[allow(clippy::too_many_arguments)]
    fn deposit_internal(
        env: Env,
        buyer: Address,
        seller: Address,
        token: Address,
        amount: i128,
        order_id: BytesN<32>,
        timeout_ledgers: u32,
        order_hash: Option<BytesN<32>>,
        schema: Option<Symbol>,
    ) -> Result<u64, EscrowError> {
        let escrow_id = Self::create_internal(
            env.clone(),
            buyer.clone(),
            seller,
            token.clone(),
            amount,
            order_id,
            timeout_ledgers,
            order_hash,
            schema,
        )?;

        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;

        let token_client = soroban_sdk::token::Client::new(&env, &token);
        let balance_before = token_client.balance(&env.current_contract_address());
        token_client.transfer(&buyer, &env.current_contract_address(), &amount);
        let net_received = verify_received_deposit_delta(
            &env,
            &token_client,
            &env.current_contract_address(),
            balance_before,
            amount,
        )?;

        record.amount = net_received;
        record.status = EscrowStatus::Funded;
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        // The buyer's deposit is now on-chain, so the escrow is no longer
        // cancellable by the seller; keep the guard snapshot (issue #355) in
        // step with the record it protects.
        Self::bump_cancel_guard_ttl(&env, escrow_id);
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);

        Ok(escrow_id)
    }

    /// Deposit multiple orders for a single buyer into escrow in one call
    /// (issue #317). Reduces the per-order transaction overhead of calling
    /// `deposit` separately for each order.
    ///
    /// The buyer authorizes the whole batch once; each order may specify a
    /// different seller, token, and amount. Because a Soroban contract
    /// invocation is atomic, an error on any entry aborts the whole call —
    /// every escrow created earlier in the batch is rolled back along with
    /// it, so callers never observe a partial batch. Each successfully
    /// created escrow still emits its own `EscrowCreatedEvent` (and
    /// `EscrowMetadataEvent` when metadata is supplied), exactly as a
    /// standalone `deposit` call would.
    pub fn batch_deposit(
        env: Env,
        buyer: Address,
        orders: Vec<BatchDepositParams>,
    ) -> Result<Vec<u64>, EscrowError> {
        if is_zero_address(&env, &buyer) {
            return Err(EscrowError::InvalidAddress);
        }
        buyer.require_auth();

        let mut escrow_ids = Vec::new(&env);
        for order in orders.iter() {
            if is_zero_address(&env, &order.seller) || is_zero_address(&env, &order.token) {
                return Err(EscrowError::InvalidAddress);
            }
            if buyer == order.seller {
                return Err(EscrowError::InvalidEscrowParticipants);
            }
            let escrow_id = Self::deposit_internal(
                env.clone(),
                buyer.clone(),
                order.seller,
                order.token,
                order.amount,
                order.order_id,
                order.timeout_ledgers,
                order.order_hash,
                order.schema,
            )?;
            escrow_ids.push_back(escrow_id);
        }
        Ok(escrow_ids)
    }

    /// Atomically create and fund multiple escrows for one buyer.
    ///
    /// The buyer approves this contract to spend the aggregate amount for
    /// each token. Each token is pulled once with `transfer_from`, reducing
    /// repeated transfer overhead. Any invalid item or failed allowance
    /// reverts the complete batch. Timeouts in `items` are absolute ledger
    /// sequence numbers.
    pub fn batch_create_escrows(
        env: Env,
        buyer: Address,
        items: Vec<BatchEscrowItem>,
    ) -> Result<Vec<u64>, EscrowError> {
        if is_zero_address(&env, &buyer) {
            return Err(EscrowError::InvalidAddress);
        }
        if items.len() > MAX_PAGE_LIMIT {
            return Err(EscrowError::InvalidLimits);
        }
        buyer.require_auth();

        let current_ledger = env.ledger().sequence();
        let mut token_totals: Map<Address, i128> = Map::new(&env);
        for item in items.iter() {
            if is_zero_address(&env, &item.seller) || is_zero_address(&env, &item.token) {
                return Err(EscrowError::InvalidAddress);
            }
            if buyer == item.seller {
                return Err(EscrowError::InvalidEscrowParticipants);
            }
            item.timeout_ledger
                .checked_sub(current_ledger)
                .ok_or(EscrowError::InvalidExtension)?;
            if item.amount <= 0 {
                return Err(EscrowError::InvalidAmount);
            }
            let aggregate = token_totals.get(item.token.clone()).unwrap_or(0i128);
            token_totals.set(
                item.token,
                aggregate
                    .checked_add(item.amount)
                    .ok_or(EscrowError::InvalidAmount)?,
            );
        }

        let mut escrow_ids = Vec::new(&env);
        for item in items.iter() {
            let timeout_ledgers = item
                .timeout_ledger
                .checked_sub(current_ledger)
                .ok_or(EscrowError::InvalidExtension)?;
            let escrow_id = Self::create_internal(
                env.clone(),
                buyer.clone(),
                item.seller,
                item.token,
                item.amount,
                item.order_id,
                timeout_ledgers,
                None,
                None,
            )?;
            let key = DataKey::Escrow(escrow_id);
            let mut record: EscrowRecord = env
                .storage()
                .persistent()
                .get(&key)
                .ok_or(EscrowError::NotFound)?;
            record.status = EscrowStatus::Funded;
            record.updated_at = env.ledger().timestamp();
            env.storage().persistent().set(&key, &record);
            env.storage().persistent().extend_ttl(
                &key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );
            escrow_ids.push_back(escrow_id);
        }

        let contract_address = env.current_contract_address();
        for (token, amount) in token_totals.iter() {
            let token_client = soroban_sdk::token::Client::new(&env, &token);
            token_client.transfer_from(&contract_address, &buyer, &contract_address, &amount);
        }
        Ok(escrow_ids)
    }

    /// Release funds for multiple escrows in a single call (issue #317).
    ///
    /// Each entry is released exactly as `partial_release` would (buyer or
    /// admin only), in order. An error on any entry aborts and reverts the
    /// entire batch. Each release still emits its own `EscrowReleasedEvent`.
    pub fn batch_release(
        env: Env,
        caller: Address,
        releases: Vec<BatchReleaseParams>,
    ) -> Result<Vec<PartialReleaseResult>, EscrowError> {
        caller.require_auth();
        Self::enter_release(&env)?;

        let mut results = Vec::new(&env);
        for item in releases.iter() {
            let result = Self::partial_release_internal(
                env.clone(),
                item.escrow_id,
                caller.clone(),
                item.release_amount,
            )?;
            results.push_back(result);
        }
        Self::exit_release(&env);
        Ok(results)
    }

    /// Refund funds for multiple escrows in a single call (issue #317).
    ///
    /// Each entry is refunded exactly as `partial_refund` would (seller/admin
    /// any time, buyer after timeout), in order. An error on any entry aborts
    /// and reverts the entire batch. Each refund still emits its own
    /// `EscrowRefundedEvent`.
    pub fn batch_refund(
        env: Env,
        caller: Address,
        refunds: Vec<BatchRefundParams>,
    ) -> Result<Vec<PartialRefundResult>, EscrowError> {
        caller.require_auth();

        let mut results = Vec::new(&env);
        for item in refunds.iter() {
            let result = Self::partial_refund_internal(
                env.clone(),
                item.escrow_id,
                caller.clone(),
                item.refund_amount,
            )?;
            results.push_back(result);
        }
        Ok(results)
    }

    /// Release a partial amount to the seller.
    /// `release_amount` must not exceed the balance after prior releases and refunds.
    /// If release_amount equals the remaining balance, set status to Released.
    /// The platform fee is deducted from `release_amount` (issue #27).
    pub fn partial_release(
        env: Env,
        escrow_id: u64,
        caller: Address,
        release_amount: i128,
    ) -> Result<PartialReleaseResult, EscrowError> {
        caller.require_auth();
        Self::enter_release(&env)?;
        let result = Self::partial_release_internal(env.clone(), escrow_id, caller, release_amount);
        Self::exit_release(&env);
        result
    }

    /// Shared `partial_release` logic used by both `partial_release` and
    /// `batch_release`. Callers are responsible for their own
    /// `caller.require_auth()`.
    fn partial_release_internal(
        env: Env,
        escrow_id: u64,
        caller: Address,
        release_amount: i128,
    ) -> Result<PartialReleaseResult, EscrowError> {
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };

        if caller != record.buyer && !Self::is_admin(env.clone(), caller.clone()) {
            return Err(EscrowError::Unauthorized);
        }

        check_not_terminal(&record)?;

        Self::ensure_not_milestone_escrow(&env, escrow_id)?;

        if record.status != EscrowStatus::Funded {
            return Err(EscrowError::InvalidStatus);
        }
        Self::validate_release_status(&record)?;

        if record.amount > DUAL_CONTROL_THRESHOLD {
            return Err(EscrowError::SignedProofRequired);
        }

        // When the admin has required it, buyer-originated releases must pass
        // the release-eligibility gate (issue #48). Admins are not gated.
        if caller == record.buyer
            && env
                .storage()
                .persistent()
                .get(&DataKey::RequireReleaseCondition(escrow_id))
                .unwrap_or(false)
            && Self::release_block_reason(env.clone(), &record).is_some()
        {
            return Err(EscrowError::ConditionNotMet);
        }

        Self::execute_release(&env, escrow_id, &key, record, caller, release_amount, 0)
    }

    /// Shared release logic used by `partial_release`, `verify_delivery_and_release`,
    /// and `release_with_merkle_proof`. Callers are responsible for their own
    /// Enforce the conservation-of-value invariant for an escrow (issue #358).
    ///
    /// Verifies that the escrow's cumulative `released_amount + refunded_amount`,
    /// after applying `additional_release` and `additional_refund`, can never
    /// exceed the escrowed principal `escrow.amount`. All arithmetic is checked
    /// so a corrupted counter surfaces as [`EscrowError::MathOverflow`] instead
    /// of silently wrapping; a disbursement overrun returns
    /// [`EscrowError::ExceedsTotalEscrowAmount`].
    fn assert_value_conservation_invariant(
        escrow: &EscrowRecord,
        additional_release: i128,
        additional_refund: i128,
    ) -> Result<(), EscrowError> {
        let new_released = escrow
            .released_amount
            .checked_add(additional_release)
            .ok_or(EscrowError::MathOverflow)?;
        let new_refunded = escrow
            .refunded_amount
            .checked_add(additional_refund)
            .ok_or(EscrowError::MathOverflow)?;
        let total_disbursed = new_released
            .checked_add(new_refunded)
            .ok_or(EscrowError::MathOverflow)?;
        if total_disbursed > escrow.amount {
            return Err(EscrowError::ExceedsTotalEscrowAmount);
        }
        Ok(())
    }

    /// Enforce the conservation-of-value invariant for an escrow.
    ///
    /// Verifies that the escrow's cumulative `released_amount + refunded_amount`,
    /// after applying `additional_release` and `additional_refund`, can never
    /// exceed the escrowed principal `escrow.amount`. All arithmetic is checked
    /// so a corrupted counter surfaces as [`EscrowError::MathOverflow`] instead
    /// of silently wrapping; a disbursement overrun returns
    /// [`EscrowError::ExceedsTotalEscrowAmount`].
    fn assert_value_conservation_invariant(
        escrow: &EscrowRecord,
        additional_release: i128,
        additional_refund: i128,
    ) -> Result<(), EscrowError> {
        let new_released = escrow
            .released_amount
            .checked_add(additional_release)
            .ok_or(EscrowError::MathOverflow)?;
        let new_refunded = escrow
            .refunded_amount
            .checked_add(additional_refund)
            .ok_or(EscrowError::MathOverflow)?;
        let total_disbursed = new_released
            .checked_add(new_refunded)
            .ok_or(EscrowError::MathOverflow)?;
        if total_disbursed > escrow.amount {
            return Err(EscrowError::ExceedsTotalEscrowAmount);
        }
        Ok(())
    }

    /// Enforce the conservation-of-value invariant for an escrow.
    ///
    /// Verifies that the escrow's cumulative `released_amount + refunded_amount`,
    /// after applying `additional_release` and `additional_refund`, can never
    /// exceed the escrowed principal `escrow.amount`. All arithmetic is checked
    /// so a corrupted counter surfaces as [`EscrowError::MathOverflow`] instead
    /// of silently wrapping; a disbursement overrun returns
    /// [`EscrowError::ExceedsTotalEscrowAmount`].
    fn assert_value_conservation_invariant(
        escrow: &EscrowRecord,
        additional_release: i128,
        additional_refund: i128,
    ) -> Result<(), EscrowError> {
        let new_released = escrow
            .released_amount
            .checked_add(additional_release)
            .ok_or(EscrowError::MathOverflow)?;
        let new_refunded = escrow
            .refunded_amount
            .checked_add(additional_refund)
            .ok_or(EscrowError::MathOverflow)?;
        let total_disbursed = new_released
            .checked_add(new_refunded)
            .ok_or(EscrowError::MathOverflow)?;
        if total_disbursed > escrow.amount {
            return Err(EscrowError::ExceedsTotalEscrowAmount);
        }
        Ok(())
    }

    /// Shared release logic used by both `partial_release` and
    /// `evaluate_and_release`. Callers are responsible for their own
    /// authorization checks before invoking this.
    ///
    /// The platform fee (per `FeeConfig` or the multi-treasury
    /// `FeeDistribution`) is deducted from `release_amount` and transferred to
    /// the treasury(ies); the seller receives the remainder (issue #27).
    /// `released_amount` tracks the full escrow-amount released, not the net
    /// seller payout.
    ///
    /// `pre_deducted` is an amount already transferred out of the escrow's
    /// balance by the caller before this function runs (currently only the
    /// oracle relayer rebate paid in `verify_delivery_and_release`, issue
    /// #317). It is folded into `released_amount`/`remaining` bookkeeping so
    /// the escrow still reaches zero remaining and transitions to `Released`,
    /// but it is excluded from the fee/seller-payout computation, which is
    /// still based only on `release_amount`.
    fn execute_release(
        env: &Env,
        escrow_id: u64,
        key: &DataKey,
        record: EscrowRecord,
        caller: Address,
        release_amount: i128,
    ) -> Result<PartialReleaseResult, EscrowError> {
        // The token transfers below are external calls: run the whole release
        // under the re-entrancy lock so a malicious token cannot call back into
        // the escrow before the record write is committed, and so a failed
        // transfer propagates and leaves storage pristine (issue #334).
        execute_atomic_operation(env, || {
            Self::execute_release_inner(env, escrow_id, key, record, caller, release_amount)
        })
    }

    /// Unguarded release body. Callers must go through [`Self::execute_release`]
    /// so the re-entrancy lock is held across the token transfers (issue #334).
    fn execute_release_inner(
        env: &Env,
        escrow_id: u64,
        key: &DataKey,
        mut record: EscrowRecord,
        caller: Address,
        release_amount: i128,
        pre_deducted: i128,
    ) -> Result<PartialReleaseResult, EscrowError> {
        if release_amount <= 0 {
            return Err(EscrowError::ZeroAmount);
        }

        let remaining = record.amount - record.released_amount - record.refunded_amount;
        let total_drawn = release_amount + pre_deducted;
        if total_drawn > remaining {
        let remaining = record
            .amount
            .checked_sub(record.released_amount)
            .and_then(|balance| balance.checked_sub(record.refunded_amount))
            .ok_or(EscrowError::MathOverflow)?;
        if release_amount > remaining {
            return Err(EscrowError::InsufficientEscrowBalance);
        }

        // Defence in depth (issue #358): even if the single-operation capacity
        // check above were bypassed, cumulative disbursements must never exceed
        // the escrowed principal.
        // Defence in depth: even if the single-operation capacity check above
        // were bypassed, cumulative disbursements must never exceed the
        // escrowed principal.
        Self::assert_value_conservation_invariant(&record, release_amount, 0)?;

        let token_client = soroban_sdk::token::Client::new(env, &record.token);

        let payout = Self::compute_payout(env, &record.seller, release_amount)?;
        Self::distribute_fee(env, &token_client, payout.fee)?;
        token_client.transfer(
            &env.current_contract_address(),
            &record.seller,
            &payout.seller_net,
        );

        record.released_amount += total_drawn;
        // Accumulate the merchant's settled volume and re-evaluate the fee
        // tier ladder (issue #328). Succeeds on every successful release.
        update_merchant_volume_and_tier(env, &record.seller, release_amount);
        let payout = Self::compute_payout(env, release_amount)?;
        record.released_amount += release_amount;
        let new_remaining = record.amount - record.released_amount - record.refunded_amount;
        record.released_amount = record
            .released_amount
            .checked_add(release_amount)
            .ok_or(EscrowError::MathOverflow)?;
        let total_disbursed = record
            .released_amount
            .checked_add(record.refunded_amount)
            .ok_or(EscrowError::MathOverflow)?;
        let new_remaining = record
            .amount
            .checked_sub(total_disbursed)
            .ok_or(EscrowError::MathOverflow)?;
        // Terminal state is reached exactly when the remaining balance hits
        // zero, i.e. `released + refunded == amount`.
        let fully_released = new_remaining == 0;
        if fully_released {
            record.status = EscrowStatus::Released;
            Self::try_mint_purchase_receipt(env, &mut record);
        }

        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(key, &record);

        Self::distribute_fee(env, &token_client, payout.fee)?;
        token_client.transfer(
            &env.current_contract_address(),
            &record.seller,
            &payout.seller_net,
        );

        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("released"),
                escrow_id,
            ),
            EscrowReleasedEvent {
                escrow_id,
                seller: record.seller.clone(),
                amount: release_amount,
                released_by: caller,
            },
        );

        if fully_released {
            let yield_config: Option<YieldConfig> = env
                .storage()
                .persistent()
                .get(&DataKey::EscrowYieldConfig(escrow_id));
            if let Some(cfg) = &yield_config {
                let (yield_amount, held_seconds) = Self::compute_yield(&record, Some(cfg), env);
                
                // Check if yield split configuration exists (issue #360)
                let split_config: Option<YieldSplitConfig> = env
                    .storage()
                    .persistent()
                    .get(&DataKey::EscrowYieldSplitConfig(escrow_id));
                
                match split_config {
                    Some(split) => {
                        // Split yield between buyer and seller according to configured basis points
                        let seller_yield = (yield_amount * split.seller_yield_share_bps as i128) / 10_000i128;
                        let buyer_yield = yield_amount - seller_yield;
                        
                        // Transfer yields to both parties atomically
                        if seller_yield > 0 {
                            token_client.transfer(
                                &env.current_contract_address(),
                                &record.seller,
                                &seller_yield,
                            );
                        }
                        if buyer_yield > 0 {
                            token_client.transfer(
                                &env.current_contract_address(),
                                &record.buyer,
                                &buyer_yield,
                            );
                        }
                        
                        // Emit the new split yield event (issue #360)
                        env.events().publish(
                            (symbol_short!("escrow"), symbol_short!("yield"), escrow_id),
                            EscrowYieldSplitAccruedEvent {
                                escrow_id,
                                seller: record.seller.clone(),
                                buyer: record.buyer.clone(),
                                total_yield: yield_amount,
                                seller_yield,
                                buyer_yield,
                                held_seconds,
                            },
                        );
                    }
                    None => {
                        // Backward compatibility: if no split config, seller gets 100% of yield
                        if yield_amount > 0 {
                            token_client.transfer(
                                &env.current_contract_address(),
                                &record.seller,
                                &yield_amount,
                            );
                        }
                        
                        // Emit the legacy yield event for backward compatibility
                        env.events().publish(
                            (symbol_short!("escrow"), symbol_short!("yield"), escrow_id),
                            EscrowYieldAccruedEvent {
                                escrow_id,
                                seller: record.seller.clone(),
                                yield_amount,
                                held_seconds,
                            },
                        );
                    }
                }
                let (yield_amount, held_seconds) = Self::compute_yield(&record, Some(cfg), env)?;
                env.events().publish(
                    (symbol_short!("escrow"), symbol_short!("yield"), escrow_id),
                    EscrowYieldAccruedEvent {
                        escrow_id,
                        seller: record.seller.clone(),
                        yield_amount,
                        held_seconds,
                    },
                );
            }
        }

        Ok(PartialReleaseResult {
            released: release_amount,
            remaining: new_remaining,
            fully_released,
        })
    }

    /// Create and fund an escrow whose principal is paid out in staggered
    /// milestones (e.g. 30% deposit, 40% dispatch, 30% delivery).
    ///
    /// The escrowed principal is the sum of the milestone amounts, so the
    /// schedule always equals the funded balance exactly. Milestones must be
    /// submitted pending (`is_completed == false`, `completed_at == 0`) with
    /// unique ids and positive amounts. Funds are pulled from the buyer
    /// immediately, as with `deposit`.
    ///
    /// # Errors
    /// - [`EscrowError::InvalidMilestoneSchedule`] if the schedule is invalid.
    /// - Any error `deposit` returns (limits, whitelist, pause, ...).
    // Reason: mirrors the `deposit` ABI signature plus the schedule.
    #[allow(clippy::too_many_arguments)]
    pub fn create_milestone_escrow(
        env: Env,
        buyer: Address,
        seller: Address,
        token: Address,
        order_id: BytesN<32>,
        timeout_ledgers: u32,
        milestones: Vec<Milestone>,
    ) -> Result<u64, EscrowError> {
        if is_zero_address(&env, &buyer)
            || is_zero_address(&env, &seller)
            || is_zero_address(&env, &token)
        {
            return Err(EscrowError::InvalidAddress);
        }
        if buyer == seller {
            return Err(EscrowError::InvalidEscrowParticipants);
        }
        let total = Self::validate_milestones(&milestones)?;

        buyer.require_auth();
        let escrow_id = Self::deposit_internal(
            env.clone(),
            buyer,
            seller,
            token,
            total,
            order_id,
            timeout_ledgers,
            None,
            None,
        )?;

        let key = DataKey::MilestoneConfig(escrow_id);
        env.storage()
            .persistent()
            .set(&key, &MilestoneEscrowConfig { milestones });
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        Ok(escrow_id)
    }

    /// Pay out a single milestone to the seller. Only the buyer may call.
    ///
    /// The platform fee is deducted from the milestone amount exactly as in
    /// `partial_release`. Once every milestone is paid the escrow's remaining
    /// balance is zero and it transitions to the terminal `Released` state.
    ///
    /// # Errors
    /// - [`EscrowError::NotFound`] if the escrow does not exist.
    /// - [`EscrowError::MilestoneNotFound`] if the escrow has no such milestone.
    /// - [`EscrowError::MilestoneAlreadyReleased`] if it was already paid.
    /// - [`EscrowError::AlreadyReleased`] / [`EscrowError::AlreadyRefunded`] /
    ///   [`EscrowError::InvalidStatus`] if the escrow is not `Funded`.
    /// - [`EscrowError::InsufficientEscrowBalance`] if a partial refund has
    ///   left too little principal to cover the milestone.
    pub fn release_milestone(
        env: Env,
        escrow_id: u64,
        milestone_id: u32,
    ) -> Result<PartialReleaseResult, EscrowError> {
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;
        record.buyer.require_auth();

        check_not_terminal(&record)?;
        Self::validate_release_status(&record)?;

        if record.amount > DUAL_CONTROL_THRESHOLD {
            return Err(EscrowError::SignedProofRequired);
        }
        if env
            .storage()
            .persistent()
            .get(&DataKey::RequireReleaseCondition(escrow_id))
            .unwrap_or(false)
            && Self::release_block_reason(env.clone(), &record).is_some()
        {
            return Err(EscrowError::ConditionNotMet);
        }

        let config_key = DataKey::MilestoneConfig(escrow_id);
        let mut config: MilestoneEscrowConfig = env
            .storage()
            .persistent()
            .get(&config_key)
            .ok_or(EscrowError::MilestoneNotFound)?;
        let index = config
            .milestones
            .iter()
            .position(|m| m.milestone_id == milestone_id)
            .ok_or(EscrowError::MilestoneNotFound)? as u32;
        let mut milestone = config.milestones.get_unchecked(index);
        if milestone.is_completed {
            return Err(EscrowError::MilestoneAlreadyReleased);
        }

        let seller = record.seller.clone();
        let caller = record.buyer.clone();
        let result =
            Self::execute_release(&env, escrow_id, &key, record, caller, milestone.amount)?;

        milestone.is_completed = true;
        milestone.completed_at = env.ledger().timestamp();
        let amount = milestone.amount;
        config.milestones.set(index, milestone);
        env.storage().persistent().set(&config_key, &config);
        env.storage().persistent().extend_ttl(
            &config_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("milestone"),
                escrow_id,
            ),
            MilestoneReleasedEvent {
                escrow_id,
                milestone_id,
                amount,
                seller,
                remaining: result.remaining,
            },
        );

        Ok(result)
    }

    /// Returns the milestone schedule for a milestone escrow.
    pub fn get_milestones(env: Env, escrow_id: u64) -> Result<MilestoneEscrowConfig, EscrowError> {
        env.storage()
            .persistent()
            .get(&DataKey::MilestoneConfig(escrow_id))
            .ok_or(EscrowError::MilestoneNotFound)
    }

    /// Validates a milestone schedule and returns its total principal.
    fn validate_milestones(milestones: &Vec<Milestone>) -> Result<i128, EscrowError> {
        if milestones.is_empty() || milestones.len() > MAX_MILESTONES {
            return Err(EscrowError::InvalidMilestoneSchedule);
        }
        let mut total: i128 = 0;
        for (i, m) in milestones.iter().enumerate() {
            if m.amount <= 0 || m.is_completed || m.completed_at != 0 {
                return Err(EscrowError::InvalidMilestoneSchedule);
            }
            // Bounded by MAX_MILESTONES, so the quadratic scan stays cheap.
            if milestones
                .iter()
                .skip(i + 1)
                .any(|other| other.milestone_id == m.milestone_id)
            {
                return Err(EscrowError::InvalidMilestoneSchedule);
            }
            total = total
                .checked_add(m.amount)
                .ok_or(EscrowError::InvalidMilestoneSchedule)?;
        }
        Ok(total)
    }

    /// Milestone escrows may only be paid through `release_milestone`;
    /// arbitrary-amount payouts would desync the schedule.
    fn ensure_not_milestone_escrow(env: &Env, escrow_id: u64) -> Result<(), EscrowError> {
        if env
            .storage()
            .persistent()
            .has(&DataKey::MilestoneConfig(escrow_id))
        {
            return Err(EscrowError::MilestoneReleaseRequired);
        }
        Ok(())
    }

    /// Release escrowed funds to the seller. Only the buyer or admin may call.
    /// The platform fee is deducted from the released amount (issue #27).
    pub fn release(
        env: Env,
        escrow_id: u64,
        caller: Address,
        recipient: Address,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();
        Self::enter_release(&env)?;
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };

        if recipient != record.seller {
            return Err(EscrowError::InvalidReleaseRecipient);
        }

        // Validate seller's merchant category if authorization is configured
        Self::validate_seller_category(&env, &record.seller)?;

        let remaining = record.amount - record.released_amount - record.refunded_amount;
        Self::partial_release_internal(env.clone(), escrow_id, caller, remaining)?;
        Self::exit_release(&env);
        Ok(true)
    }

    /// Refund escrowed funds to the buyer.
    /// Seller or admin may refund at any time; the buyer may refund after timeout.
    /// Refunds the full remaining (unreleased, unrefunded) balance.
    pub fn refund(env: Env, escrow_id: u64, caller: Address) -> Result<bool, EscrowError> {
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };

        let remaining = record.amount - record.released_amount - record.refunded_amount;
        Self::partial_refund(env, escrow_id, caller, remaining)?;
        Ok(true)
    }

    /// Refund a partial amount to the buyer.
    /// `refund_amount` must be <= (record.amount - record.released_amount - record.refunded_amount).
    /// Status remains `Funded` unless the refund exhausts the remaining balance, in
    /// which case status becomes `Refunded`.
    /// Seller or admin may refund at any time; the buyer may refund after timeout.
    pub fn partial_refund(
        env: Env,
        escrow_id: u64,
        caller: Address,
        refund_amount: i128,
    ) -> Result<PartialRefundResult, EscrowError> {
        caller.require_auth();
        Self::partial_refund_internal(env, escrow_id, caller, refund_amount)
    }

    /// Shared `partial_refund` logic used by both `partial_refund` and
    /// `batch_refund`. Callers are responsible for their own
    /// `caller.require_auth()`.
    fn partial_refund_internal(
        env: Env,
        escrow_id: u64,
        caller: Address,
        refund_amount: i128,
    ) -> Result<PartialRefundResult, EscrowError> {
        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };

        check_not_terminal(&record)?;

        if record.status != EscrowStatus::Funded && record.status != EscrowStatus::Inspection {
            return Err(EscrowError::InvalidStatus);
        }

        let timeout_reached = env.ledger().sequence() >= record.timeout_ledger;

        // Check for shipment proof - required for seller to claim on timeout
        let has_proof = env
            .storage()
            .persistent()
            .has(&DataKey::ShipmentProof(escrow_id));

        // During inspection, only buyer can initiate refund
        if record.status == EscrowStatus::Inspection {
            if caller != record.buyer {
                return Err(EscrowError::Unauthorized);
            }
            // Buyer can always refund during inspection
        } else if caller == record.seller || Self::is_admin(env.clone(), caller.clone()) {
            // Seller or admin: allowed at any time before timeout. Once the
            // timeout is reached, a mandatory dispute grace period must fully
            // elapse before a timeout-based claim can proceed, and the escrow
            // must not be under an active dispute — this guarantees the buyer
            // a window to raise a dispute without being front-run by a seller
            // racing to claim the instant `timeout_ledger` is reached (issue
            // #284). Absence of shipment proof still blocks the seller/admin
            // from ever claiming on timeout; the buyer retains exclusive
            // refund rights in that case.
            let is_disputed = record.status == EscrowStatus::Disputed;
            if timeout_reached
                && (!has_proof
                    || !can_seller_claim(env.ledger().sequence(), record.timeout_ledger, is_disputed))
            {
                return Err(EscrowError::TimeoutNotReached);
            }
        } else if caller == record.buyer {
            // Buyer: can always refund after timeout, even without shipment proof
            if !timeout_reached {
                return Err(EscrowError::TimeoutNotReached);
            }
        } else {
            return Err(EscrowError::Unauthorized);
        }

        if refund_amount <= 0 {
            return Err(EscrowError::ZeroAmount);
        }

        let remaining = record
            .amount
            .checked_sub(record.released_amount)
            .and_then(|balance| balance.checked_sub(record.refunded_amount))
            .ok_or(EscrowError::MathOverflow)?;
        if refund_amount > remaining {
            return Err(EscrowError::InsufficientEscrowBalance);
        }

        // Defence in depth (issue #358): cumulative releases and refunds must
        // never exceed the escrowed principal.
        // Defence in depth: cumulative releases and refunds must never exceed
        // the escrowed principal.
        Self::assert_value_conservation_invariant(&record, 0, refund_amount)?;

        let token_client = soroban_sdk::token::Client::new(&env, &record.token);
        token_client.transfer(
            &env.current_contract_address(),
            &record.buyer,
            &refund_amount,
        );

        record.refunded_amount = record
            .refunded_amount
            .checked_add(refund_amount)
            .ok_or(EscrowError::MathOverflow)?;
        let total_disbursed = record
            .released_amount
            .checked_add(record.refunded_amount)
            .ok_or(EscrowError::MathOverflow)?;
        let new_remaining = record
            .amount
            .checked_sub(total_disbursed)
            .ok_or(EscrowError::MathOverflow)?;
        // Terminal state is reached exactly when the remaining balance hits
        // zero, i.e. `released + refunded == amount`.
        let fully_refunded = new_remaining == 0;
        if fully_refunded {
            record.status = EscrowStatus::Refunded;
        }

        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);

        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("refunded"),
                escrow_id,
            ),
            EscrowRefundedEvent {
                escrow_id,
                buyer: record.buyer.clone(),
                amount: refund_amount,
                remaining: new_remaining,
                refunded_by: caller,
            },
        );

        // On full refund, distribute accrued yield to buyer if yield config exists (issue #360)
        if fully_refunded {
            let yield_config: Option<YieldConfig> = env
                .storage()
                .persistent()
                .get(&DataKey::EscrowYieldConfig(escrow_id));
            if let Some(cfg) = &yield_config {
                let (yield_amount, held_seconds) = Self::compute_yield(&record, Some(cfg), &env);
                
                // Check if yield split configuration exists (issue #360)
                let split_config: Option<YieldSplitConfig> = env
                    .storage()
                    .persistent()
                    .get(&DataKey::EscrowYieldSplitConfig(escrow_id));
                
                match split_config {
                    Some(split) => {
                        // Calculate buyer's share of yield (inverse of seller's share)
                        let seller_yield = (yield_amount * split.seller_yield_share_bps as i128) / 10_000i128;
                        let buyer_yield = yield_amount - seller_yield;
                        
                        // Transfer buyer's yield share
                        if buyer_yield > 0 {
                            token_client.transfer(
                                &env.current_contract_address(),
                                &record.buyer,
                                &buyer_yield,
                            );
                        }
                        
                        // Emit the split yield event for refund completion (issue #360)
                        env.events().publish(
                            (symbol_short!("escrow"), symbol_short!("yield"), escrow_id),
                            EscrowYieldSplitAccruedEvent {
                                escrow_id,
                                seller: record.seller.clone(),
                                buyer: record.buyer.clone(),
                                total_yield: yield_amount,
                                seller_yield,
                                buyer_yield,
                                held_seconds,
                            },
                        );
                    }
                    None => {
                        // Backward compatibility: on refund without split config,
                        // buyer gets 100% of yield (fair allocation for refund scenario)
                        if yield_amount > 0 {
                            token_client.transfer(
                                &env.current_contract_address(),
                                &record.buyer,
                                &yield_amount,
                            );
                        }
                        
                        // Emit the legacy yield event
                        env.events().publish(
                            (symbol_short!("escrow"), symbol_short!("yield"), escrow_id),
                            EscrowYieldAccruedEvent {
                                escrow_id,
                                seller: record.seller.clone(),
                                yield_amount,
                                held_seconds,
                            },
                        );
                    }
                }
            }
        }

        Ok(PartialRefundResult {
            refunded: refund_amount,
            remaining: new_remaining,
            fully_refunded,
        })
    }

    /// Configure a conditional release for an escrow, gated on an external oracle
    /// contract (issue #339). Callable by the buyer, seller, or admin while the
    /// escrow is not in a terminal state.
    ///
    /// `oracle_contract` must implement `resolve(condition_type: Symbol) -> bool`.
    /// `evaluate_and_release` calls this function and releases funds to the
    /// seller only when it returns `true`.
    pub fn set_release_condition(
        env: Env,
        caller: Address,
        escrow_id: u64,
        condition_type: Symbol,
        oracle_contract: Address,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();

        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };

        if caller != record.buyer
            && caller != record.seller
            && !Self::is_admin(env.clone(), caller.clone())
        {
            return Err(EscrowError::Unauthorized);
        }

        check_not_terminal(&record)?;

        env.storage().persistent().set(
            &DataKey::ReleaseCondition(escrow_id),
            &ReleaseCondition {
                condition_type: condition_type.clone(),
                oracle_contract: oracle_contract.clone(),
            },
        );

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("condset"), escrow_id),
            ReleaseConditionSetEvent {
                escrow_id,
                condition_type,
                oracle_contract,
            },
        );

        Ok(true)
    }

    /// Read-only getter for the release condition configured on an escrow.
    pub fn get_release_condition(
        env: Env,
        escrow_id: u64,
    ) -> Result<ReleaseCondition, EscrowError> {
        env.storage()
            .persistent()
            .get(&DataKey::ReleaseCondition(escrow_id))
            .ok_or(EscrowError::ReleaseConditionNotSet)
    }

    /// Deprecated insecure bool-oracle release path. It remains in the ABI for
    /// compatibility but never releases funds; use
    /// `verify_delivery_and_release` with an oracle signature instead.
    pub fn evaluate_and_release(
        _env: Env,
        _escrow_id: u64,
        caller: Address,
    ) -> Result<PartialReleaseResult, EscrowError> {
        caller.require_auth();
        Err(EscrowError::SignedProofRequired)
    }

    /// Verify an oracle-signed delivery attestation and release the escrow's
    /// full remaining balance to its seller.
    ///
    /// The signed payload consists of the XDR encoding of `escrow_id`,
    /// `carrier_code`, `tracking_hash`, and `delivery_timestamp`. The proof's
    /// public key must equal the admin-configured key, and the delivery time
    /// must fall between escrow creation and the current ledger timestamp.
    pub fn verify_delivery_and_release(
        env: Env,
        escrow_id: u64,
        caller: Address,
        proof: SignedDeliveryProof,
    ) -> Result<PartialReleaseResult, EscrowError> {
        caller.require_auth();
        Self::enter_release(&env)?;

        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };
        check_not_terminal(&record)?;
        if record.status != EscrowStatus::Funded {
            return Err(EscrowError::InvalidStatus);
        }

        if record.amount > DUAL_CONTROL_THRESHOLD {
            let config: DualControlConfig = env
                .storage()
                .persistent()
                .get(&DataKey::DualControlConfig(escrow_id))
                .ok_or(EscrowError::Unauthorized)?;
            if !config.is_secondary_approved {
                return Err(EscrowError::Unauthorized);
            }
        }

        let configured_key: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::OraclePublicKey)
            .ok_or(EscrowError::OraclePublicKeyNotSet)?;
        if proof.escrow_id != escrow_id
            || proof.oracle_pubkey != configured_key
            || proof.delivery_timestamp < record.created_at
            || proof.delivery_timestamp > env.ledger().timestamp()
        {
            return Err(EscrowError::InvalidSignedDeliveryProof);
        }

        use soroban_sdk::xdr::ToXdr;
        let payload = SignedDeliveryPayload {
            escrow_id: proof.escrow_id,
            carrier_code: proof.carrier_code,
            tracking_hash: proof.tracking_hash,
            delivery_timestamp: proof.delivery_timestamp,
        }
        .to_xdr(&env);
        env.crypto()
            .ed25519_verify(&configured_key, &payload, &proof.signature);

        // Get or use default inspection duration
        let inspection_config: Option<InspectionPeriodConfig> = env
            .storage()
            .persistent()
            .get(&DataKey::InspectionPeriodConfig(escrow_id));

        let inspection_duration = inspection_config
            .map(|cfg| cfg.inspection_duration_ledgers)
            .unwrap_or(51_840); // Default: 72 hours at 5s/ledger

        let delivery_confirmed_ledger = env.ledger().sequence();
        let auto_release_ledger = delivery_confirmed_ledger + inspection_duration;

        // Store inspection configuration
        let inspection_cfg = InspectionPeriodConfig {
            inspection_duration_ledgers: inspection_duration,
            delivery_confirmed_ledger,
            auto_release_ledger,
        };
        env.storage()
            .persistent()
            .set(&DataKey::InspectionPeriodConfig(escrow_id), &inspection_cfg);

        // Transition to Inspection status
        let mut record = record;
        record.status = EscrowStatus::Inspection;
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);

        // Extend TTL for persistent storage
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
        env.storage().persistent().extend_ttl(
            &DataKey::InspectionPeriodConfig(escrow_id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Emit inspection started event
        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("insp_start"),
                escrow_id,
            ),
            EscrowInspectionStartedEvent {
                escrow_id,
                delivery_confirmed_ledger,
                inspection_duration_ledgers: inspection_duration,
                auto_release_ledger,
            },
        );

        Ok(PartialReleaseResult {
            released: 0,
            remaining: record.amount - record.released_amount - record.refunded_amount,
            fully_released: false,
        })
    }

    /// Computes and transfers the oracle relayer gas-fee rebate (issue #317)
    /// out of the escrow's own token balance, reimbursing the caller that
    /// submitted the signed delivery proof for the transaction cost it fronted.
    /// Returns the rebate amount actually transferred (zero when `amount` is
    /// too small for `ORACLE_REBATE_BPS` to round to a positive amount, in
    /// which case no transfer is made).
    fn disburse_oracle_rebate(
        env: &Env,
        token_client: &soroban_sdk::token::Client,
        oracle: &Address,
        amount: i128,
    ) -> i128 {
        if amount <= 0 {
            return 0;
        }
        let rebate = (amount * ORACLE_REBATE_BPS as i128) / 10_000i128;
        if rebate > 0 {
            token_client.transfer(&env.current_contract_address(), oracle, &rebate);
        }
        rebate
    }

    /// Configure (or update) cross-currency settlement for an escrow (issue
    /// #318), letting a buyer fund in `deposit_token` while the seller is
    /// paid out in a different `payout_token` via `release_with_swap`.
    ///
    /// Only the escrow's buyer or an admin may call. `config.deposit_token`
    /// must equal the escrow's own `record.token`, `config.payout_token` must
    /// differ from it (otherwise there is nothing to swap), and
    /// `config.max_slippage_bps` must be nonzero and no greater than
    /// `MAX_SWAP_SLIPPAGE_BPS`.
    pub fn set_cross_currency_swap_config(
        env: Env,
        caller: Address,
        escrow_id: u64,
        config: CrossCurrencySwapConfig,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();

        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;

        if caller != record.buyer && !Self::is_admin(env.clone(), caller.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        check_not_terminal(&record)?;

        if is_zero_address(&env, &config.router_contract)
            || is_zero_address(&env, &config.payout_token)
        {
            return Err(EscrowError::InvalidSwapConfig);
        }
        if config.deposit_token != record.token {
            return Err(EscrowError::InvalidSwapConfig);
        }
        if config.payout_token == config.deposit_token {
            return Err(EscrowError::InvalidSwapConfig);
        }
        if config.max_slippage_bps == 0 || config.max_slippage_bps > MAX_SWAP_SLIPPAGE_BPS {
            return Err(EscrowError::InvalidSwapConfig);
        }

        let swap_key = DataKey::CrossCurrencySwapConfig(escrow_id);
        env.storage().persistent().set(&swap_key, &config);
        env.storage().persistent().extend_ttl(
            &swap_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("swapcfg"), escrow_id),
            CrossCurrencySwapConfiguredEvent {
                escrow_id,
                deposit_token: config.deposit_token,
                payout_token: config.payout_token,
                max_slippage_bps: config.max_slippage_bps,
                router_contract: config.router_contract,
            },
        );

        Ok(true)
    }

    /// Read-only getter for an escrow's cross-currency swap configuration, if
    /// any (issue #318).
    pub fn get_cross_currency_swap_config(
        env: Env,
        escrow_id: u64,
    ) -> Option<CrossCurrencySwapConfig> {
        env.storage()
            .persistent()
            .get(&DataKey::CrossCurrencySwapConfig(escrow_id))
    }

    /// Release an escrow's full remaining balance to its seller, atomically
    /// swapping it from the escrow's deposit token into the seller's payout
    /// token through the router configured via
    /// `set_cross_currency_swap_config` (issue #318).
    ///
    /// The platform fee is computed and collected in the deposit token first
    /// (matching `release`/`partial_release`, issue #27); only the
    /// seller-bound net remainder is swapped. `expected_payout_amount` is the
    /// caller's off-chain price quote for that swap; the router's actual
    /// output must be at least `expected_payout_amount` reduced by the
    /// escrow's configured `max_slippage_bps`, or the whole release reverts
    /// (`EscrowError::SlippageExceeded`), protecting the seller's payout from
    /// excessive slippage. The buyer's deposit is never at risk beyond the
    /// amount already escrowed, since the swap input is capped at the
    /// escrow's own remaining balance.
    pub fn release_with_swap(
        env: Env,
        escrow_id: u64,
        caller: Address,
        recipient: Address,
        expected_payout_amount: i128,
    ) -> Result<i128, EscrowError> {
        caller.require_auth();

        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;

        if recipient != record.seller {
            return Err(EscrowError::InvalidReleaseRecipient);
        }
        if caller != record.buyer && !Self::is_admin(env.clone(), caller.clone()) {
            return Err(EscrowError::Unauthorized);
        }

        Self::validate_seller_category(&env, &record.seller)?;
        check_not_terminal(&record)?;
        Self::validate_release_status(&record)?;

        if expected_payout_amount <= 0 {
            return Err(EscrowError::InvalidAmount);
        }

        if record.amount > DUAL_CONTROL_THRESHOLD {
            let dual_control: DualControlConfig = env
                .storage()
                .persistent()
                .get(&DataKey::DualControlConfig(escrow_id))
                .ok_or(EscrowError::DualControlNotConfigured)?;
            if !dual_control.is_secondary_approved {
                return Err(EscrowError::SecondaryApprovalRequired);
            }
        }

        let swap_config: CrossCurrencySwapConfig = env
            .storage()
            .persistent()
            .get(&DataKey::CrossCurrencySwapConfig(escrow_id))
            .ok_or(EscrowError::SwapConfigNotSet)?;
        if swap_config.deposit_token != record.token {
            return Err(EscrowError::InvalidSwapConfig);
        }

        Self::ensure_not_milestone_escrow(&env, escrow_id)?;
        let remaining = record.amount - record.released_amount - record.refunded_amount;
        if remaining <= 0 {
            return Err(EscrowError::ZeroAmount);
        }

        let token_client = soroban_sdk::token::Client::new(&env, &record.token);
        let payout = Self::compute_payout(&env, remaining)?;
        Self::distribute_fee(&env, &token_client, payout.fee)?;

        // Floor the router's acceptable output at the caller's quoted price
        // minus the escrow's configured slippage tolerance, so a bad quote or
        // a manipulated pool cannot shortchange the seller beyond what the
        // escrow was configured to tolerate.
        let min_amount_out = payout.seller_net.min(
            expected_payout_amount
                - (expected_payout_amount * swap_config.max_slippage_bps as i128) / 10_000i128,
        );

        // Hand the seller-bound remainder to the router, then invoke it to
        // perform the swap and deliver the payout token directly to the
        // seller. If the router call fails or traps, this whole invocation
        // returns an error and the Soroban host reverts every state change
        // made during it, including the transfer just made here — so no
        // funds can be stranded in the router.
        token_client.transfer(
            &env.current_contract_address(),
            &swap_config.router_contract,
            &payout.seller_net,
        );

        let args = soroban_sdk::vec![
            &env,
            record.token.to_val(),
            swap_config.payout_token.to_val(),
            payout.seller_net.into_val(&env),
            min_amount_out.into_val(&env),
            record.seller.to_val(),
        ];
        let swap_result = env.try_invoke_contract::<i128, InvokeError>(
            &swap_config.router_contract,
            &Symbol::new(&env, "swap_exact_tokens_for_tokens"),
            args,
        );

        let amount_out = match swap_result {
            Ok(Ok(out)) if out >= min_amount_out => out,
            Ok(Ok(_)) => return Err(EscrowError::SlippageExceeded),
            _ => return Err(EscrowError::SwapRouterCallFailed),
        };

        record.released_amount += remaining;
        record.status = EscrowStatus::Released;
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("swaprel"), escrow_id),
            CrossCurrencySwapReleasedEvent {
                escrow_id,
                seller: record.seller.clone(),
                deposit_amount: remaining,
                payout_token: swap_config.payout_token.clone(),
                payout_amount: amount_out,
                released_by: caller,
            },
        );

        Ok(amount_out)
    }

    /// Configure k-of-n oracle consensus for an escrow (issue #352).
    ///
    /// Buyer, seller, or admin may configure while the escrow is not terminal.
    /// `required_oracle_count` must be between 1 and the number of oracles;
    /// oracle addresses must be non-empty, unique, non-zero, and bounded by
    /// [`MAX_ORACLES`]. Configuring resets any previously recorded votes.
    pub fn set_multi_oracle_config(
        env: Env,
        caller: Address,
        escrow_id: u64,
        config: MultiOracleConfig,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();

        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Escrow(escrow_id))
            .ok_or(EscrowError::NotFound)?;
        if caller != record.buyer
            && caller != record.seller
            && !Self::is_admin(env.clone(), caller.clone())
        {
            return Err(EscrowError::Unauthorized);
        }
        check_not_terminal(&record)?;

        let oracle_count = config.oracle_addresses.len();
        if oracle_count == 0
            || oracle_count > MAX_ORACLES
            || config.required_oracle_count == 0
            || config.required_oracle_count > oracle_count
        {
            return Err(EscrowError::InvalidQuorum);
        }
        for i in 0..oracle_count {
            let oracle = config
                .oracle_addresses
                .get(i)
                .ok_or(EscrowError::InvalidAddress)?;
            if is_zero_address(&env, &oracle) {
                return Err(EscrowError::InvalidAddress);
            }
            for j in (i + 1)..oracle_count {
                let other = config
                    .oracle_addresses
                    .get(j)
                    .ok_or(EscrowError::InvalidAddress)?;
                if other == oracle {
                    return Err(EscrowError::InvalidQuorum);
                }
            }
        }

        let config_key = DataKey::MultiOracleConfig(escrow_id);
        let votes_key = DataKey::EscrowOracleVotes(escrow_id);
        env.storage().persistent().set(&config_key, &config);
        env.storage().persistent().extend_ttl(
            &config_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        env.storage()
            .persistent()
            .set(&votes_key, &soroban_sdk::Vec::<OracleVoteRecord>::new(&env));
        env.storage().persistent().extend_ttl(
            &votes_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("oraset"), escrow_id),
            config.required_oracle_count,
        );
        Ok(true)
    }

    /// Read-only getter for an escrow's multi-oracle configuration (issue #352).
    pub fn get_multi_oracle_config(env: Env, escrow_id: u64) -> Option<MultiOracleConfig> {
        env.storage()
            .persistent()
            .get(&DataKey::MultiOracleConfig(escrow_id))
    }

    /// Read-only getter for the attestations recorded on an escrow (issue #352).
    pub fn get_oracle_votes(env: Env, escrow_id: u64) -> soroban_sdk::Vec<OracleVoteRecord> {
        env.storage()
            .persistent()
            .get(&DataKey::EscrowOracleVotes(escrow_id))
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env))
    }

    /// Submit a multi-oracle attestation for an escrow (issue #352).
    ///
    /// The caller must be one of the escrow's authorized oracles and may attest
    /// at most once. Attestations must arrive within
    /// [`MULTI_ORACLE_CONSENSUS_WINDOW_LEDGERS`] of the first vote. Once
    /// `required_oracle_count` distinct affirmative attestations are recorded
    /// the escrow is released to the seller immediately.
    ///
    /// Returns `true` when this attestation triggered the release, `false` when
    /// it was recorded and consensus is still pending.
    pub fn submit_oracle_attestation(
        env: Env,
        escrow_id: u64,
        caller: Address,
        condition_met: bool,
    ) -> Result<bool, MultiOracleError> {
        caller.require_auth();

        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(MultiOracleError::EscrowNotFound)?;
        let config: MultiOracleConfig = env
            .storage()
            .persistent()
            .get(&DataKey::MultiOracleConfig(escrow_id))
            .ok_or(MultiOracleError::ConfigNotSet)?;
        if record.status != EscrowStatus::Funded {
            return Err(MultiOracleError::InvalidStatus);
        }
        if !config.oracle_addresses.contains(&caller) {
            return Err(MultiOracleError::UnauthorizedOracle);
        }

        let mut votes: soroban_sdk::Vec<OracleVoteRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::EscrowOracleVotes(escrow_id))
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env));

        for i in 0..votes.len() {
            if let Some(existing) = votes.get(i) {
                if existing.oracle == caller {
                    return Err(MultiOracleError::DuplicateOracleVote);
                }
            }
        }

        let current_ledger = env.ledger().sequence();
        if let Some(first) = votes.get(0) {
            let expiry = first
                .voted_at_ledger
                .saturating_add(MULTI_ORACLE_CONSENSUS_WINDOW_LEDGERS);
            if current_ledger > expiry {
                return Err(MultiOracleError::ConsensusWindowExpired);
            }
        }

        votes.push_back(OracleVoteRecord {
            oracle: caller.clone(),
            condition_met,
            voted_at_ledger: current_ledger,
        });

        let mut affirmative: u32 = 0;
        for i in 0..votes.len() {
            if let Some(recorded) = votes.get(i) {
                if recorded.condition_met {
                    affirmative += 1;
                }
            }
        }

        let mut released = false;
        if affirmative >= config.required_oracle_count {
            let remaining = record
                .amount
                .checked_sub(record.released_amount)
                .and_then(|balance| balance.checked_sub(record.refunded_amount))
                .ok_or(MultiOracleError::ReleaseFailed)?;
            if remaining <= 0 {
                return Err(MultiOracleError::InvalidStatus);
            }
            Self::execute_release(&env, escrow_id, &key, record, caller.clone(), remaining)
                .map_err(|_| MultiOracleError::ReleaseFailed)?;
            released = true;
        }

        let votes_key = DataKey::EscrowOracleVotes(escrow_id);
        env.storage().persistent().set(&votes_key, &votes);
        env.storage().persistent().extend_ttl(
            &votes_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("oracst"), escrow_id),
            OracleConsensusEvent {
                escrow_id,
                oracle: caller,
                condition_met,
                affirmative_votes: affirmative,
                required_oracle_count: config.required_oracle_count,
                released,
            },
        );

        Ok(released)
    }

    /// Check whether an escrow has reached its multi-oracle threshold (issue #352).
    ///
    /// Returns `Ok(true)` once enough distinct affirmative attestations are
    /// recorded, otherwise `Err(MultiOracleError::ThresholdNotMet)`.
    pub fn check_oracle_consensus(env: Env, escrow_id: u64) -> Result<bool, MultiOracleError> {
        let config: MultiOracleConfig = env
            .storage()
            .persistent()
            .get(&DataKey::MultiOracleConfig(escrow_id))
            .ok_or(MultiOracleError::ConfigNotSet)?;
        let votes: soroban_sdk::Vec<OracleVoteRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::EscrowOracleVotes(escrow_id))
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env));
        let mut affirmative: u32 = 0;
        for i in 0..votes.len() {
            if let Some(recorded) = votes.get(i) {
                if recorded.condition_met {
                    affirmative += 1;
                }
            }
        }
        if affirmative >= config.required_oracle_count {
            Ok(true)
        } else {
            Err(MultiOracleError::ThresholdNotMet)
        }
        let result = Self::execute_release(&env, escrow_id, &key, record, caller, remaining);
        Self::exit_release(&env);
        result
    }

    /// Mark the escrow as disputed. Only the buyer or seller may call.
    pub fn dispute(env: Env, escrow_id: u64, caller: Address) -> Result<bool, EscrowError> {
        caller.require_auth();

        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };

        if caller != record.buyer && caller != record.seller {
            return Err(EscrowError::Unauthorized);
        }

        // Allow dispute from Funded or Inspection status
        if record.status != EscrowStatus::Funded && record.status != EscrowStatus::Inspection {
            return Err(EscrowError::InvalidStatus);
        }

        // If in Inspection, check that we haven't exceeded the inspection window
        if record.status == EscrowStatus::Inspection {
            let inspection_config: InspectionPeriodConfig = env
                .storage()
                .persistent()
                .get(&DataKey::InspectionPeriodConfig(escrow_id))
                .ok_or(EscrowError::InspectionConfigNotSet)?;
            if env.ledger().sequence() >= inspection_config.auto_release_ledger {
                return Err(EscrowError::InspectionExpired);
            }
        }

        record.status = EscrowStatus::Disputed;
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);

        // Disputed escrows can remain open far longer than a normal
        // settlement while resolution is pending; bump the persistent TTL so
        // the record is not archived out from under an active dispute
        // (issue #282).
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);

        // 7 days statutory resolution window (604800 seconds)
        let deadline = env.ledger().timestamp() + 604800;
        env.storage().persistent().set(&DataKey::DisputeDeadline(escrow_id), &deadline);

        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("disputed"),
                escrow_id,
            ),
            EscrowDisputedEvent {
                escrow_id,
                disputed_by: caller,
            },
        );

        Ok(true)
    }

    /// Permissionless keeper entrypoint: extend the persistent-storage TTL of
    /// an active (non-terminal) escrow so it cannot be archived out from
    /// under its funds while resolution — including a pending dispute — is
    /// still in progress (issue #282). Unlike `bump_ttl_with_bounty`, this
    /// does not adjust `timeout_ledger`, pay a bounty, or rate-limit calls,
    /// and it is available for `Disputed` escrows as well as `Funded` ones;
    /// it exists purely as an always-available storage-eviction backstop
    /// that anyone (a keeper bot, the buyer, or the seller) can call for
    /// free.
    ///
    /// # Errors
    /// Returns [`EscrowError::NotFound`] if the escrow does not exist, or one
    /// of the "already terminal" errors if it has already reached
    /// `Released`, `Refunded`, or `Cancelled`.
    pub fn extend_escrow_ttl(env: Env, escrow_id: u64) -> Result<bool, EscrowError> {
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };

        check_not_terminal(&record)?;

        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);

        Ok(true)
    }

    /// Resolve a disputed escrow with an initial ruling. Only the admin may call.
    ///
    /// # Two-Tiered Resolution (issue #354)
    ///
    /// Instead of immediately releasing funds, this function transitions the escrow
    /// to `InitialRuling` status and opens a 48-hour appeal window. Funds remain
    /// escrowed until the ruling is finalized by either:
    /// - `finalize_uncontested_ruling` (called after appeal window expires with no appeal)
    /// - `finalize_dispute_appeal` (called by appeals council to finalize an appeal)
    ///
    /// The `release_to_seller` parameter determines who would win if the ruling stands:
    /// - `true`: seller wins (receives funds minus fee)
    /// - `false`: buyer wins (receives full refund)
    pub fn resolve_dispute(
        env: Env,
        escrow_id: u64,
        caller: Address,
        release_to_seller: bool,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();
        Self::enter_release(&env)?;

        if !Self::is_admin(env.clone(), caller.clone()) {
            return Err(EscrowError::Unauthorized);
        }

        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };

        if record.status != EscrowStatus::Disputed {
            return Err(EscrowError::NotDisputed);
        }

        // Determine the initial winner based on the ruling direction
        let initial_winner = if release_to_seller {
            record.seller.clone()
        } else {
            record.buyer.clone()
        };

        // Compute the minimum appeal bond (10% of escrow amount, minimum 100 stroops)
        let appeal_bond = record.amount / 10;
        let appeal_bond = appeal_bond.max(100);

        // Create the appeal record with the 48-hour window
        let appeal_record = DisputeAppealRecord {
            initial_winner: initial_winner.clone(),
            appealed_by: initial_winner.clone(), // placeholder, will be overwritten on actual appeal
            appeal_bond_amount: appeal_bond,
            appeal_deadline_ledger: env.ledger().sequence() + APPEAL_WINDOW_LEDGERS,
            status: AppealStatus::PendingAppealWindow,
        };

        // Store the appeal record
        env.storage().persistent().set(
            &DataKey::DisputeAppeal(escrow_id),
            &appeal_record,
        );

        // Transition escrow to InitialRuling (funds remain held)
        record.status = EscrowStatus::InitialRuling;
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);

        // Emit event: initial ruling issued, appeal window open
        let token_client = soroban_sdk::token::Client::new(&env, &record.token);
        let payout = if release_to_seller {
            Some(Self::compute_payout(&env, record.amount)?)
        } else {
            None
        };
        record.status = if release_to_seller {
            EscrowStatus::Released
        } else {
            EscrowStatus::Refunded
        };
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);

        if release_to_seller {
            let payout = payout.unwrap();
            Self::distribute_fee(&env, &token_client, payout.fee)?;
            token_client.transfer(
                &env.current_contract_address(),
                &record.seller,
                &payout.seller_net,
            );
        } else {
            token_client.transfer(
                &env.current_contract_address(),
                &record.buyer,
                &record.amount,
            );
        }

        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("initial_r"),
                escrow_id,
            ),
            (
                initial_winner.clone(),
                appeal_record.appeal_deadline_ledger,
            ),
        );

        Self::exit_release(&env);
        Ok(true)
    }

    /// Resolve a disputed escrow with an exact buyer, seller, and mediator split.
    ///
    /// Only the admin may call. The award amounts must all be non-negative
    /// and sum exactly to the escrow amount. This resolution bypasses the
    /// ordinary release fee because the mediator fee is explicit in the award.
    pub fn resolve_dispute_split(
        env: Env,
        escrow_id: u64,
        caller: Address,
        award: DisputeResolutionAward,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();
        if !Self::is_admin(env.clone(), caller.clone()) {
            return Err(EscrowError::Unauthorized);
        }

        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;
        if record.status != EscrowStatus::Disputed {
            return Err(EscrowError::NotDisputed);
        }
        if record.released_amount != 0 || record.refunded_amount != 0 {
            return Err(EscrowError::InvalidDisputeAward);
        }
        if award.buyer_amount < 0 || award.seller_amount < 0 || award.mediator_fee < 0 {
            return Err(EscrowError::InvalidDisputeAward);
        }
        let total_award = award
            .buyer_amount
            .checked_add(award.seller_amount)
            .and_then(|total| total.checked_add(award.mediator_fee))
            .ok_or(EscrowError::InvalidDisputeAward)?;
        if total_award != record.amount
            || (award.mediator_fee > 0 && is_zero_address(&env, &award.mediator_address))
        {
            return Err(EscrowError::InvalidDisputeAward);
        }
        let seller_and_mediator = award
            .seller_amount
            .checked_add(award.mediator_fee)
            .ok_or(EscrowError::InvalidDisputeAward)?;

        let token_client = soroban_sdk::token::Client::new(&env, &record.token);
        let contract_address = env.current_contract_address();
        if award.buyer_amount > 0 {
            token_client.transfer(&contract_address, &record.buyer, &award.buyer_amount);
        }
        if award.seller_amount > 0 {
            token_client.transfer(&contract_address, &record.seller, &award.seller_amount);
        }
        if award.mediator_fee > 0 {
            token_client.transfer(
                &contract_address,
                &award.mediator_address,
                &award.mediator_fee,
            );
        }

        record.refunded_amount = award.buyer_amount;
        record.released_amount = seller_and_mediator;
        record.status = if seller_and_mediator == 0 {
            EscrowStatus::Refunded
        } else {
            EscrowStatus::Released
        };
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);

        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("dispsplit"),
                escrow_id,
            ),
            DisputeResolvedEvent {
                escrow_id,
                buyer_amount: award.buyer_amount,
                seller_amount: award.seller_amount,
                mediator_fee: award.mediator_fee,
                mediator_address: award.mediator_address,
                resolved_by: caller,
            },
        );
        Ok(true)
    }

    /// Files an appeal against an initial dispute ruling (issue #354).
    ///
    /// The losing party must deposit the appeal bond within the 48-hour window
    /// to escalate the dispute to the appeals council for review.
    ///
    /// # Bond mechanics
    ///
    /// - Bond amount is defined in the DisputeAppealRecord created during `resolve_dispute`
    /// - Bond is transferred from appealer to the contract for escrow
    /// - If appeal fails (council upholds initial ruling): bond is slashed to the initial winner
    /// - If appeal succeeds (council reverses): bond is returned to the appealer
    ///
    /// # Errors
    ///
    /// * `DisputeNotInitialRuling` if escrow is not in InitialRuling status
    /// * `AppealWindowExpired` if deadline_ledger has passed
    /// * `AppealAlreadyFiled` if an appeal already exists for this dispute
    /// * `AppealBondInsufficient` if the caller has insufficient token balance
    pub fn file_dispute_appeal(
        env: Env,
        escrow_id: u64,
        caller: Address,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();

        // Load escrow — must be in InitialRuling status
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;

        if record.status != EscrowStatus::InitialRuling {
            return Err(EscrowError::DisputeNotInitialRuling);
        }

        // Load appeal record
        let appeal_key = DataKey::DisputeAppeal(escrow_id);
        let mut appeal: DisputeAppealRecord = env
            .storage()
            .persistent()
            .get(&appeal_key)
            .ok_or(EscrowError::NoAppealFound)?;

        // Check that appeal window is still open
        if env.ledger().sequence() > appeal.appeal_deadline_ledger {
            return Err(EscrowError::AppealWindowExpired);
        }

        // Check that an appeal hasn't already been filed
        if appeal.status != AppealStatus::PendingAppealWindow {
            return Err(EscrowError::AppealAlreadyFiled);
        }

        // Determine the losing party (whoever is not the initial winner)
        let is_caller_winner = caller == appeal.initial_winner;
        if is_caller_winner {
            return Err(EscrowError::Unauthorized); // Winner can't file appeal
        }

        // Transfer bond from caller to contract
        let token_client = soroban_sdk::token::Client::new(&env, &record.token);
        token_client.transfer(
            &caller,
            &env.current_contract_address(),
            &appeal.appeal_bond_amount,
        );

        // Record appeal
        appeal.appealed_by = caller.clone();
        appeal.status = AppealStatus::Appealed;
        env.storage().persistent().set(&appeal_key, &appeal);

        // Emit event
        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("appealed"),
                escrow_id,
            ),
            (caller, appeal.appeal_bond_amount),
        );

        Ok(true)
    }

    /// Finalizes an appeal and enforces the final ruling (issue #354).
    ///
    /// Called by the appeals council only. Resolves the appeal with a final winner
    /// and handles bond slashing:
    /// - If appeal fails (final_winner == initial_winner): bond is slashed to initial winner
    /// - If appeal succeeds (final_winner != initial_winner): bond is returned to appealer
    ///
    /// After finalization, escrow funds are released according to the final ruling
    /// and the escrow transitions to terminal state (Released or Refunded).
    ///
    /// # Errors
    ///
    /// * `NotAppealsCouncil` if caller is not the configured appeals council
    /// * `NoAppealFound` if no appeal record exists for this escrow
    /// * `NotFound` if escrow record doesn't exist
    pub fn finalize_dispute_appeal(
        env: Env,
        escrow_id: u64,
        caller: Address,
        final_winner: Address,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();

        // Verify caller is the appeals council
        let council_key = DataKey::AppealsCouncil;
        let council: Address = env
            .storage()
            .instance()
            .get(&council_key)
            .ok_or(EscrowError::NotAppealsCouncil)?;

        if caller != council {
            return Err(EscrowError::NotAppealsCouncil);
        }

        // Load appeal — must be in Appealed status
        let appeal_key = DataKey::DisputeAppeal(escrow_id);
        let mut appeal: DisputeAppealRecord = env
            .storage()
            .persistent()
            .get(&appeal_key)
            .ok_or(EscrowError::NoAppealFound)?;

        if appeal.status != AppealStatus::Appealed {
            return Err(EscrowError::NoAppealFound);
        }

        // Load escrow
        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;

        // Determine if appeal succeeded (final winner is not the initial winner)
        let appeal_succeeded = final_winner != appeal.initial_winner;

        // Handle bond slashing logic
        let token_client = soroban_sdk::token::Client::new(&env, &record.token);
        let contract_address = env.current_contract_address();

        if appeal_succeeded {
            // Appeal succeeded — return bond to appealer
            token_client.transfer(
                &contract_address,
                &appeal.appealed_by,
                &appeal.appeal_bond_amount,
            );
        } else {
            // Appeal failed — slash bond to initial winner
            token_client.transfer(
                &contract_address,
                &appeal.initial_winner,
                &appeal.appeal_bond_amount,
            );
        }

        // Release escrow funds to final winner
        let payout = Self::compute_payout(&env, record.amount)?;
        
        if final_winner == record.seller {
            // Seller wins: release to seller minus fee
            Self::distribute_fee(&env, &token_client, payout.fee)?;
            token_client.transfer(
                &contract_address,
                &final_winner,
                &payout.seller_net,
            );
            record.status = EscrowStatus::Released;
        } else {
            // Buyer wins: refund to buyer
            token_client.transfer(
                &contract_address,
                &final_winner,
                &record.amount,
            );
            record.status = EscrowStatus::Refunded;
        }

        // Finalize
        appeal.status = AppealStatus::Finalized;
        record.updated_at = env.ledger().timestamp();

        env.storage().persistent().set(&appeal_key, &appeal);
        env.storage().persistent().set(&key, &record);

        // Emit event
        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("fin_appea"),
                escrow_id,
            ),
            (final_winner, appeal_succeeded),
        );

        Ok(true)
    }

    /// Auto-finalizes an uncontested ruling after the appeal window expires (issue #354).
    ///
    /// Can be called by anyone after the appeal deadline has passed without
    /// an appeal being filed. Releases escrow funds to the initial winner
    /// (since the ruling went uncontested).
    ///
    /// # Errors
    ///
    /// * `NoAppealFound` if no appeal record exists
    /// * `AppealAlreadyFiled` if an appeal has already been filed
    /// * `AppealWindowExpired` if the appeal window is still open (use correct condition)
    pub fn finalize_uncontested_ruling(env: Env, escrow_id: u64) -> Result<bool, EscrowError> {
        // Load appeal record
        let appeal_key = DataKey::DisputeAppeal(escrow_id);
        let appeal: DisputeAppealRecord = env
            .storage()
            .persistent()
            .get(&appeal_key)
            .ok_or(EscrowError::NoAppealFound)?;

        // Must be in PendingAppealWindow status (no appeal filed yet)
        if appeal.status != AppealStatus::PendingAppealWindow {
            return Err(EscrowError::AppealAlreadyFiled);
        }

        // Must be past the deadline
        if env.ledger().sequence() <= appeal.appeal_deadline_ledger {
            return Err(EscrowError::AppealWindowExpired);
        }

        // Load escrow
        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;

        // Release to initial winner (ruling stands uncontested)
        let token_client = soroban_sdk::token::Client::new(&env, &record.token);
        let payout = Self::compute_payout(&env, record.amount)?;

        if appeal.initial_winner == record.seller {
            // Seller wins: release to seller minus fee
            Self::distribute_fee(&env, &token_client, payout.fee)?;
            token_client.transfer(
                &env.current_contract_address(),
                &appeal.initial_winner,
                &payout.seller_net,
            );
            record.status = EscrowStatus::Released;
        } else {
            // Buyer wins: refund to buyer
            token_client.transfer(
                &env.current_contract_address(),
                &appeal.initial_winner,
                &record.amount,
            );
            record.status = EscrowStatus::Refunded;
        }

        // Finalize
        let mut final_appeal = appeal;
        final_appeal.status = AppealStatus::Finalized;
        record.updated_at = env.ledger().timestamp();

        env.storage().persistent().set(&appeal_key, &final_appeal);
        env.storage().persistent().set(&key, &record);

        // Emit event
        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("uncontst"),
                escrow_id,
            ),
            appeal.initial_winner,
        );

        Ok(true)
    }

    /// Set configurable inspection duration for an escrow. Admin-only.
    /// Overrides the default 72-hour inspection window.
    pub fn set_inspection_config(
        env: Env,
        admin: Address,
        escrow_id: u64,
        inspection_duration_ledgers: u32,
    ) -> Result<(), EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin) {
            return Err(EscrowError::Unauthorized);
        }

        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;

        // Only allow setting inspection config before delivery is confirmed
        if record.status != EscrowStatus::Funded && record.status != EscrowStatus::Created {
            return Err(EscrowError::InvalidStatus);
        }

        let current_ledger = env.ledger().sequence();
        let auto_release_ledger = current_ledger + inspection_duration_ledgers;

        let inspection_cfg = InspectionPeriodConfig {
            inspection_duration_ledgers,
            delivery_confirmed_ledger: current_ledger,
            auto_release_ledger,
        };
        env.storage()
            .persistent()
            .set(&DataKey::InspectionPeriodConfig(escrow_id), &inspection_cfg);

        Ok(())
    }

    /// Buyer releases funds after passing inspection. Only the buyer may call.
    /// Transitions from Inspection to Released status.
    pub fn release_on_inspection_passed(
        env: Env,
        escrow_id: u64,
        buyer: Address,
    ) -> Result<bool, EscrowError> {
        buyer.require_auth();

        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;

        if record.status != EscrowStatus::Inspection {
            return Err(EscrowError::NotInInspection);
        }

        if buyer != record.buyer {
            return Err(EscrowError::Unauthorized);
        }

        // Get inspection config to verify we're still within window
        let inspection_config: InspectionPeriodConfig = env
            .storage()
            .persistent()
            .get(&DataKey::InspectionPeriodConfig(escrow_id))
            .ok_or(EscrowError::InspectionConfigNotSet)?;

        if env.ledger().sequence() >= inspection_config.auto_release_ledger {
            return Err(EscrowError::InspectionExpired);
        }

        let remaining = record.amount - record.released_amount - record.refunded_amount;
        if remaining <= 0 {
            return Err(EscrowError::ZeroAmount);
        }

        let token_client = soroban_sdk::token::Client::new(&env, &record.token);
        let payout = Self::compute_payout(&env, remaining)?;
        Self::distribute_fee(&env, &token_client, payout.fee)?;
        token_client.transfer(
            &env.current_contract_address(),
            &record.seller,
            &payout.seller_net,
        );

        record.released_amount += remaining;
        record.status = EscrowStatus::Released;
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);

        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("insp_pass"),
                escrow_id,
            ),
            EscrowInspectionPassedEvent {
                escrow_id,
                released_by: buyer,
            },
        );

        Ok(true)
    }

    /// Seller or keeper claims funds after inspection auto-release window expires.
    /// Transitions from Inspection to Released status without requiring buyer action.
    pub fn claim_inspection_auto_release(
        env: Env,
        escrow_id: u64,
        caller: Address,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();
    /// Register the line items that make up a multi-item escrow (issue #363).
    ///
    /// Buyer or admin only, while the escrow is not terminal and no funds have
    /// been disbursed yet. Item amounts must be positive, unique by `item_id`,
    /// and sum exactly to the escrowed principal. Registering items lets a
    /// dispute be settled one line at a time so uncontested funds are never
    /// locked up behind the whole order.
    pub fn set_sub_order_items(
        env: Env,
        escrow_id: u64,
        caller: Address,
        items: soroban_sdk::Vec<SubOrderItem>,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();

        if items.is_empty() || items.len() > MAX_SUB_ORDER_ITEMS {
            return Err(EscrowError::InvalidAmount);
        }

        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Escrow(escrow_id))
            .ok_or(EscrowError::NotFound)?;
        if caller != record.buyer && !Self::is_admin(env.clone(), caller.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        check_not_terminal(&record)?;
        if record.released_amount != 0 || record.refunded_amount != 0 {
            return Err(EscrowError::InvalidStatus);
        }

        let mut total: i128 = 0;
        for i in 0..items.len() {
            let item = items.get(i).ok_or(EscrowError::InvalidAmount)?;
            if item.amount <= 0 {
                return Err(EscrowError::InvalidAmount);
            }
            total = total
                .checked_add(item.amount)
                .ok_or(EscrowError::MathOverflow)?;

            for j in (i + 1)..items.len() {
                let other = items.get(j).ok_or(EscrowError::InvalidAmount)?;
                if other.item_id == item.item_id {
                    return Err(EscrowError::InvalidAmount);
                }
            }
        }
        if total != record.amount {
            return Err(EscrowError::InvalidAmount);
        }

        env.storage()
            .persistent()
            .set(&DataKey::SubOrderItems(escrow_id), &items);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("subitems"), escrow_id),
            items.len(),
        );
        Ok(true)
    }

    /// Return the registered line items of a multi-item escrow (issue #363).
    pub fn get_sub_order_items(env: Env, escrow_id: u64) -> soroban_sdk::Vec<SubOrderItem> {
        env.storage()
            .persistent()
            .get(&DataKey::SubOrderItems(escrow_id))
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env))
    }

    /// Return the persisted resolution for one line item, if any (issue #363).
    pub fn get_sub_item_resolution(
        env: Env,
        escrow_id: u64,
        item_id: Symbol,
    ) -> Option<SubOrderItemResolution> {
        env.storage()
            .persistent()
            .get(&DataKey::SubItemResolution(escrow_id, item_id))
    }

    /// Resolve a single line item of a disputed multi-item escrow (issue #363).
    ///
    /// Admin only. Immediately releases the uncontested `release_amount` to the
    /// seller and refunds the disputed `refund_amount` to the buyer, so the rest
    /// of the order does not have to wait for the whole dispute to be settled.
    /// The two amounts must be non-negative and sum exactly to the registered
    /// amount for `item_id`; each item may be resolved at most once. When the
    /// cumulative disbursements reach the escrowed principal the escrow moves to
    /// a terminal state.
    pub fn resolve_sub_item_dispute(
        env: Env,
        escrow_id: u64,
        caller: Address,
        resolution: SubOrderItemResolution,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();
        if !Self::is_admin(env.clone(), caller.clone()) {
            return Err(EscrowError::Unauthorized);
        }

        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;

        if record.status != EscrowStatus::Inspection {
            return Err(EscrowError::NotInInspection);
        }

        // Only seller, admin, or any party can call (keeper bounty incentive)
        if caller != record.seller && !Self::is_admin(env.clone(), caller.clone()) {
            // Allow any caller for bounty incentive, but validate seller later
            // For now, we allow any caller to trigger auto-release for keeper bounty
        }

        let inspection_config: InspectionPeriodConfig = env
            .storage()
            .persistent()
            .get(&DataKey::InspectionPeriodConfig(escrow_id))
            .ok_or(EscrowError::InspectionConfigNotSet)?;

        let current_ledger = env.ledger().sequence();
        if current_ledger < inspection_config.auto_release_ledger {
            return Err(EscrowError::InspectionAutoReleaseNotReady);
        }

        let remaining = record.amount - record.released_amount - record.refunded_amount;
        if remaining <= 0 {
            return Err(EscrowError::ZeroAmount);
        }

        let token_client = soroban_sdk::token::Client::new(&env, &record.token);
        let payout = Self::compute_payout(&env, remaining)?;
        Self::distribute_fee(&env, &token_client, payout.fee)?;
        token_client.transfer(
            &env.current_contract_address(),
            &record.seller,
            &payout.seller_net,
        );

        record.released_amount += remaining;
        record.status = EscrowStatus::Released;
        if record.status != EscrowStatus::Disputed {
            return Err(EscrowError::NotDisputed);
        }

        let items: soroban_sdk::Vec<SubOrderItem> = env
            .storage()
            .persistent()
            .get(&DataKey::SubOrderItems(escrow_id))
            .ok_or(EscrowError::MetadataNotSet)?;
        let item = Self::find_sub_order_item(&items, &resolution.item_id)
            .ok_or(EscrowError::NotFound)?;

        let resolution_key = DataKey::SubItemResolution(escrow_id, resolution.item_id.clone());
        if env.storage().persistent().has(&resolution_key) {
            return Err(EscrowError::SubItemAlreadyResolved);
        }

        if resolution.refund_amount < 0 || resolution.release_amount < 0 {
            return Err(EscrowError::InvalidDisputeAward);
        }
        let item_total = resolution
            .refund_amount
            .checked_add(resolution.release_amount)
            .ok_or(EscrowError::MathOverflow)?;
        if item_total != item.amount {
            return Err(EscrowError::InvalidDisputeAward);
        }

        // Cumulative disbursements may never exceed the escrowed principal.
        Self::assert_value_conservation_invariant(
            &record,
            resolution.release_amount,
            resolution.refund_amount,
        )?;

        let token_client = soroban_sdk::token::Client::new(&env, &record.token);
        let contract_address = env.current_contract_address();
        if resolution.refund_amount > 0 {
            token_client.transfer(&contract_address, &record.buyer, &resolution.refund_amount);
        }
        if resolution.release_amount > 0 {
            token_client.transfer(&contract_address, &record.seller, &resolution.release_amount);
        }

        record.refunded_amount = record
            .refunded_amount
            .checked_add(resolution.refund_amount)
            .ok_or(EscrowError::MathOverflow)?;
        record.released_amount = record
            .released_amount
            .checked_add(resolution.release_amount)
            .ok_or(EscrowError::MathOverflow)?;
        let total_disbursed = record
            .released_amount
            .checked_add(record.refunded_amount)
            .ok_or(EscrowError::MathOverflow)?;
        let new_remaining = record
            .amount
            .checked_sub(total_disbursed)
            .ok_or(EscrowError::MathOverflow)?;
        if new_remaining == 0 {
            record.status = if record.released_amount == 0 {
                EscrowStatus::Refunded
            } else {
                EscrowStatus::Released
            };
        }

        let stored = SubOrderItemResolution {
            item_id: resolution.item_id.clone(),
            refund_amount: resolution.refund_amount,
            release_amount: resolution.release_amount,
            is_resolved: true,
        };
        env.storage().persistent().set(&resolution_key, &stored);

        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);

        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("insp_auto"),
                escrow_id,
            ),
            EscrowInspectionAutoReleasedEvent {
                escrow_id,
                released_by: caller,
            },
        );

        Ok(true)
    }

            (symbol_short!("escrow"), symbol_short!("subres"), escrow_id),
            SubItemResolvedEvent {
                escrow_id,
                item_id: stored.item_id.clone(),
                refund_amount: stored.refund_amount,
                release_amount: stored.release_amount,
                remaining: new_remaining,
                resolved_by: caller,
            },
        );
        Ok(true)
    }

    /// Look up a registered line item by id.
    fn find_sub_order_item(
        items: &soroban_sdk::Vec<SubOrderItem>,
        item_id: &Symbol,
    ) -> Option<SubOrderItem> {
        for i in 0..items.len() {
            let item = items.get(i)?;
            if &item.item_id == item_id {
                return Some(item);
            }
        }
        None
    }

    /// Read-only helper for settlement workers to determine whether release can proceed.
    ///
    /// Reason symbols (≤9 chars for `symbol_short!` compat):
    ///   `ok`       — escrow is funded and not timed out
    ///   `notfound` — escrow record does not exist
    ///   `released` — already released (terminal)
    ///   `refunded` — already refunded (terminal)
    ///   `disputed` — escrow is under dispute
    ///   `timeout`  — refund timeout has been reached
    pub fn get_release_eligibility(env: Env, escrow_id: u64) -> ReleaseEligibility {
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => {
                return ReleaseEligibility {
                    escrow_id: BytesN::from_array(&env, &[0u8; 32]),
                    eligible: false,
                    reason: symbol_short!("notfound"),
                };
            }
        };

        let reason = match Self::release_block_reason(env, &record) {
            Some(reason) => reason,
            None => symbol_short!("ok"),
        };

        ReleaseEligibility {
            escrow_id: record.order_id,
            eligible: reason == symbol_short!("ok"),
            reason,
        }
    }

    /// Read-only getter for escrow state.
    ///
    /// # Errors
    /// Returns [`EscrowError::NotFound`] when no escrow exists for `escrow_id`.
    pub fn get_escrow(env: Env, escrow_id: u64) -> Result<EscrowRecord, EscrowError> {
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;
        // Reads extend the TTL so a long-lived, open escrow is not evicted
        // while it is still being read (mirrors marketplace `get_merchant`).
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
        Ok(record)
    }

    /// Read-only buyer-facing receipt for an escrow.
    ///
    /// Returns a compact [`EscrowReceipt`] containing the identifiers and
    /// current status that a backend service can forward to the buyer after
    /// escrow creation or as a status check.  The full record (amount, token,
    /// timeout, …) is available via [`get_escrow`].
    ///
    /// # Errors
    /// Returns [`EscrowError::NotFound`] when no escrow exists for `escrow_id`.
    pub fn get_receipt(env: Env, escrow_id: u64) -> Result<EscrowReceipt, EscrowError> {
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;
        Ok(EscrowReceipt {
            escrow_id: record.escrow_id,
            buyer: record.buyer,
            seller: record.seller,
            order_id: record.order_id,
            status: record.status,
        })
    }

    /// Read-only merchant-facing receipt for dashboards and settlement checks.
    ///
    /// Returns a [`MerchantEscrowReceipt`] with the order id as `escrow_id`,
    /// the seller as `merchant`, and a computed `release_eligible` flag that
    /// does not mutate contract state.
    ///
    /// # Errors
    /// Returns [`EscrowError::NotFound`] when no escrow exists for `escrow_id`.
    pub fn get_merchant_receipt(
        env: Env,
        escrow_id: u64,
    ) -> Result<MerchantEscrowReceipt, EscrowError> {
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;
        let release_eligible = Self::release_block_reason(env, &record).is_none();
        Ok(MerchantEscrowReceipt {
            escrow_id: record.order_id,
            merchant: record.seller,
            buyer: record.buyer,
            status: record.status,
            release_eligible,
        })
    }

    /// Read-only complete state snapshot of an escrow (issue #329).
    ///
    /// Aggregates the full escrow record, the fee configuration applied to
    /// it, and computed fields (current timeout status and release
    /// eligibility) in a single call, so off-chain indexers and auditors
    /// don't have to replay contract events to reconstruct current state.
    /// Never mutates storage.
    ///
    /// # Errors
    /// Returns [`EscrowError::NotFound`] when no escrow exists for `escrow_id`.
    pub fn get_escrow_snapshot(env: Env, escrow_id: u64) -> Result<EscrowSnapshot, EscrowError> {
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;

        let fee_config: FeeConfig = Self::get_fee_config(env.clone())?;
        let current_ledger = env.ledger().sequence();
        let timed_out =
            record.status == EscrowStatus::Funded && current_ledger >= record.timeout_ledger;
        let release_eligible = Self::release_block_reason(env.clone(), &record).is_none();

        Ok(EscrowSnapshot {
            record,
            fee_config,
            current_ledger,
            timed_out,
            release_eligible,
        })
    }

    /// Configure optional yield accrual for an escrow (issue #331). Admin-only.
    ///
    /// `apr_bps` is the annual yield rate in basis points (max 10000 = 100%).
    /// Yield is prorated by the actual time the escrow is held and reported
    /// via [`EscrowYieldAccruedEvent`] when the escrow is fully released.
    ///
    /// # Errors
    /// - [`EscrowError::Unauthorized`] if `admin` is not an escrow admin.
    /// - [`EscrowError::InvalidYieldConfig`] if `apr_bps` exceeds 10000.
    /// - [`EscrowError::NotFound`] if `escrow_id` does not exist.
    /// - Any terminal-state error from [`check_not_terminal`] once the
    ///   escrow has already been released, refunded, or cancelled.
    pub fn set_yield_config(
        env: Env,
        admin: Address,
        escrow_id: u64,
        lending_contract: Address,
        apr_bps: u32,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        if apr_bps > 10_000 {
            return Err(EscrowError::InvalidYieldConfig);
        }

        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;
        check_not_terminal(&record)?;

        env.storage().persistent().set(
            &DataKey::EscrowYieldConfig(escrow_id),
            &YieldConfig {
                lending_contract,
                apr_bps,
            },
        );

        Ok(true)
    }

    /// Configure yield split distribution between buyer and seller for an escrow (issue #360).
    ///
    /// Allows admin to specify how accrued yield is distributed upon full release or refund.
    /// The seller receives `seller_yield_share_bps` basis points of the total yield, and the
    /// buyer receives the remaining (`10_000 - seller_yield_share_bps`) basis points.
    /// Useful for long-term escrows or dispute scenarios where both parties should benefit
    /// from the accrued interest based on their capital commitment.
    ///
    /// The escrow must not be in a terminal state (Released, Refunded, or Cancelled).
    /// Calling this function is optional — if no `YieldSplitConfig` is set, escrows default
    /// to awarding 100% of yield to the seller (backward compatible with issue #331).
    ///
    /// # Arguments
    /// - `admin` — Address of the caller (must be primary admin or co-admin).
    /// - `escrow_id` — Numeric identifier of the escrow to configure.
    /// - `seller_yield_share_bps` — Percentage of yield for the seller (0-10_000 basis points).
    ///
    /// # Errors
    /// - [`EscrowError::Unauthorized`] if `caller` is neither the primary admin nor a co-admin.
    /// - [`EscrowError::InvalidYieldConfig`] if `seller_yield_share_bps` exceeds 10_000.
    /// - [`EscrowError::NotFound`] if the escrow does not exist.
    /// - [`EscrowError::AlreadyReleased`] if the escrow has already been released.
    /// - [`EscrowError::AlreadyRefunded`] if the escrow has already been refunded.
    /// - [`EscrowError::AlreadyCancelled`] if the escrow has already been cancelled.
    pub fn set_yield_split_config(
        env: Env,
        admin: Address,
        escrow_id: u64,
        seller_yield_share_bps: u32,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        if seller_yield_share_bps > 10_000 {
            return Err(EscrowError::InvalidYieldConfig);
        }

        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;
        check_not_terminal(&record)?;

        env.storage().persistent().set(
            &DataKey::EscrowYieldSplitConfig(escrow_id),
            &YieldSplitConfig {
                seller_yield_share_bps,
            },
        );

        Ok(true)
    }

    /// Monotonic yield snapshot for an escrow (issue #34).
    ///
    /// Returns a [`YieldView`] containing every input needed to compute
    /// display-relevant yield numbers.  `snapshot_ledger` anchors the read
    /// to the current ledger, `held_seconds` is derived from
    /// `created_at` to the snapshot ledger's close time, and `accrued` is
    /// the yield at that frozen point.  Two calls within the same ledger
    /// always return identical results; calls in different ledgers produce
    /// monotonically increasing `held_seconds` and `accrued`.
    ///
    /// When a `YieldConfig` is set, `accrued` is delegated to the configured
    /// external lending pool through the shared interface (issue #326),
    /// falling back to the internal APR estimate if the pool is unreachable,
    /// paused, or reports a non-positive figure.
    ///
    /// Returns zero yield fields when no `YieldConfig` is set.
    /// Never mutates storage.
    ///
    /// # Errors
    /// Returns [`EscrowError::NotFound`] when no escrow exists for `escrow_id`.
    pub fn get_accrued_yield(env: Env, escrow_id: u64) -> Result<YieldView, EscrowError> {
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;

        let yield_config: Option<YieldConfig> = env
            .storage()
            .persistent()
            .get(&DataKey::EscrowYieldConfig(escrow_id));
        let apy_bps = yield_config.as_ref().map(|c| c.apr_bps).unwrap_or(0);
        let snapshot_ledger = env.ledger().sequence();
        let (accrued, held_seconds) = match yield_config.as_ref() {
            Some(cfg) => Self::accrued_yield(&record, cfg, &env),
            None => (0, 0),
        };
        let (accrued, held_seconds) = Self::compute_yield(&record, yield_config.as_ref(), &env)?;

        let remaining = record.amount - record.released_amount - record.refunded_amount;

        Ok(YieldView {
            escrow_id,
            principal: remaining,
            apy_bps,
            held_seconds,
            accrued,
            snapshot_ledger,
        })
    }

    /// Read-only compact escrow summary for API/indexer consumers (issue #90).
    ///
    /// Returns all display fields needed by the backend event indexer in a
    /// single call. The response is stable for both terminal and non-terminal
    /// escrows. Never mutates storage.
    ///
    /// # Errors
    /// Returns [`EscrowError::NotFound`] when no escrow exists for `escrow_id`.
    pub fn get_escrow_summary(env: Env, escrow_id: u64) -> Result<EscrowSummary, EscrowError> {
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;

        Ok(EscrowSummary {
            escrow_id: record.order_id,
            buyer: record.buyer,
            merchant: record.seller,
            amount: record.amount,
            status: record.status,
        })
    }

    /// Propose a new primary admin. Must be called by current primary admin.
    pub fn propose_admin(
        env: Env,
        current_admin: Address,
        new_admin: Address,
    ) -> Result<bool, EscrowError> {
        current_admin.require_auth();
        let primary_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(EscrowError::NotFound)?;
        if current_admin != primary_admin {
            return Err(EscrowError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);
        env.events().publish(
            (symbol_short!("admin"), symbol_short!("proposed")),
            AdminProposedEvent {
                current_admin,
                new_admin,
            },
        );
        Ok(true)
    }

    /// Accept the primary admin role. Must be called by the proposed new admin.
    pub fn accept_admin(env: Env, new_admin: Address) -> Result<bool, EscrowError> {
        new_admin.require_auth();
        let pending_admin: Address = match env.storage().instance().get(&DataKey::PendingAdmin) {
            Some(addr) => addr,
            None => return Err(EscrowError::NoPendingTransfer),
        };
        if new_admin != pending_admin {
            return Err(EscrowError::InvalidPendingAdmin);
        }

        let mut admin_list: soroban_sdk::Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AdminList)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env));
        if let Some(index) = admin_list.first_index_of(&new_admin) {
            admin_list.remove(index);
            env.storage()
                .instance()
                .set(&DataKey::AdminList, &admin_list);
        }

        env.storage().instance().set(&DataKey::Admin, &new_admin);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.events().publish(
            (symbol_short!("admin"), symbol_short!("accepted")),
            AdminAcceptedEvent { new_admin },
        );
        Ok(true)
    }

    /// Cancel a pending admin transfer. Must be called by current primary admin.
    pub fn cancel_admin_transfer(env: Env, current_admin: Address) -> Result<bool, EscrowError> {
        current_admin.require_auth();
        let primary_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(EscrowError::NotFound)?;
        if current_admin != primary_admin {
            return Err(EscrowError::Unauthorized);
        }
        if !env.storage().instance().has(&DataKey::PendingAdmin) {
            return Err(EscrowError::NoPendingTransfer);
        }
        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.events().publish(
            (symbol_short!("admin"), symbol_short!("cancelled")),
            AdminTransferCancelledEvent { current_admin },
        );
        Ok(true)
    }

    /// Add a co-admin. Must be called by the primary admin.
    pub fn add_co_admin(
        env: Env,
        admin: Address,
        new_co_admin: Address,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        let primary_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(EscrowError::NotFound)?;
        if admin != primary_admin {
            return Err(EscrowError::Unauthorized);
        }
        if new_co_admin == primary_admin {
            return Err(EscrowError::AdminAlreadyExists);
        }
        let mut admin_list: soroban_sdk::Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AdminList)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env));
        if admin_list.contains(&new_co_admin) {
            return Err(EscrowError::AdminAlreadyExists);
        }
        admin_list.push_back(new_co_admin);
        env.storage()
            .instance()
            .set(&DataKey::AdminList, &admin_list);
        Ok(true)
    }

    /// Prune dispute and timeout votes for settled/terminal escrows (`Released`, `Refunded`, `Cancelled`, `ResolvedSeller`, `ResolvedBuyer`).
    ///
    /// Callable by admin in bounded batches (`escrow_ids.len() <= MAX_PAGE_LIMIT`).
    /// Returns the number of escrows whose auxiliary dispute data was pruned from persistent storage.
    pub fn prune_dispute_votes(
        env: Env,
        admin: Address,
        escrow_ids: soroban_sdk::Vec<u64>,
    ) -> Result<u32, EscrowError> {
        admin.require_auth();
        let primary_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(EscrowError::NotFound)?;
        if admin != primary_admin {
            return Err(EscrowError::Unauthorized);
        }

        if escrow_ids.len() > MAX_PAGE_LIMIT {
            return Err(EscrowError::InvalidLimits);
        }

        let mut pruned_count: u32 = 0;

        for id in escrow_ids.iter() {
            let key = DataKey::Escrow(id);
            if let Some(record) = env.storage().persistent().get::<_, EscrowRecord>(&key) {
                let is_terminal = EscrowTerminalState::from_status(&record.status).is_some();

                if is_terminal {
                    let votes_key = DataKey::DisputeVotes(id);
                    let ext_key = DataKey::TimeoutExtensionVotes(id);
                    let mut had_data = false;
                    if env.storage().persistent().has(&votes_key) {
                        env.storage().persistent().remove(&votes_key);
                        had_data = true;
                    }
                    if env.storage().persistent().has(&ext_key) {
                        env.storage().persistent().remove(&ext_key);
                        had_data = true;
                    }
                    if had_data {
                        pruned_count += 1;
                    }
                }
            }
        }

        if pruned_count > 0 {
            env.events().publish(
                (symbol_short!("escrow"), symbol_short!("pruned")),
                DisputeVotesPrunedEvent {
                    pruned_count,
                    pruned_by: admin,
                },
            );
        }

        Ok(pruned_count)
    }

    /// Reclaim the persistent storage held by a settled escrow (issue #331).
    ///
    /// Once an escrow reaches a terminal state (`Released`, `Refunded`,
    /// `Cancelled`) it can never move again, so its storage is dead weight
    /// that still pins rent. This sweep deletes the escrow record together with
    /// every per-escrow auxiliary entry — dispute/timeout vote maps, metadata
    /// halves, shipment proof, release condition, dual-control config, yield
    /// config, release-condition gate, and the keeper bump marker — after the
    /// [`ARCHIVAL_RETENTION_LEDGERS`] retention window has elapsed since the
    /// escrow's terminal transition.
    ///
    /// The window is measured from `record.updated_at`, which is frozen at the
    /// terminal transition: every mutating path calls `check_not_terminal`
    /// (including `bump_ttl_with_bounty`), so no later call can push the
    /// timestamp forward and extend an escrow's life.
    ///
    /// # Safety
    ///
    /// Only terminal escrows past the retention window are eligible, so the
    /// sweep can never delete an active or disputed escrow. It is additionally
    /// guarded by a settled-balance check: a `Released`/`Refunded` escrow whose
    /// balance is not fully drained is rejected rather than archived. Because
    /// the only state it can touch is already-dead state, the call takes no
    /// `require_auth` — it is safe for any keeper (or automated rent sweeper)
    /// to submit, and a third party gains nothing by triggering it early.
    ///
    /// # Indexes
    ///
    /// The shared `EscrowIds` and `BuyerEscrowAt` indexes are deliberately left
    /// untouched. `list_escrows`/`list_escrows_by_buyer` already skip ids whose
    /// record is gone, and rewriting a shared, append-only vector per escrow
    /// would cost more rent than the sweep reclaims. An archived escrow is
    /// therefore reported as [`EscrowError::NotFound`] by every getter.
    ///
    /// # Errors
    /// - [`EscrowError::NotFound`] if `escrow_id` has no record (or was already
    ///   archived by an earlier sweep).
    /// - [`EscrowError::InvalidStatus`] if the escrow is not terminal
    ///   (`Created`, `Funded`, or `Disputed`), or a payout-terminal escrow
    ///   still has an undrained balance.
    /// - [`EscrowError::ArchivalRetentionNotElapsed`] if the retention window
    ///   has not yet passed since the terminal transition.
    pub fn archive_terminal_escrow(env: Env, escrow_id: u64) -> Result<(), EscrowError> {
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;

        // `EscrowTerminalState::from_status` is the single source of truth for
        // what counts as terminal, so archiving can never disagree with
        // `check_not_terminal` about which states are still live.
        let terminal_state =
            EscrowTerminalState::from_status(&record.status).ok_or(EscrowError::InvalidStatus)?;

        // Defensive invariant: a payout-terminal escrow must have fully drained
        // its balance. `Cancelled` is only reachable from `Created` (never
        // funded), so it is exempt — there is nothing to have stranded.
        if terminal_state != EscrowTerminalState::Cancelled
            && record.amount - record.released_amount - record.refunded_amount != 0
        {
            return Err(EscrowError::InvalidStatus);
        }

        let archived_at = env.ledger().timestamp();
        let retention_secs = ARCHIVAL_RETENTION_LEDGERS as u64 * SECONDS_PER_LEDGER;
        if archived_at.saturating_sub(record.updated_at) < retention_secs {
            return Err(EscrowError::ArchivalRetentionNotElapsed);
        }

        // Snapshot which entries are actually present first, so the event can
        // report the reclaim count before any of the state is dropped. A plain
        // array is used rather than a host `Vec` — the key set is fixed, and
        // building it on the stack avoids the storage round-trip and the clone
        // that materializing `DataKey` values into a `Vec` would cost.
        let aux_keys = [
            DataKey::DisputeVotes(escrow_id),
            DataKey::TimeoutExtensionVotes(escrow_id),
            DataKey::EscrowMetadataHash(escrow_id),
            DataKey::EscrowMetadataSchema(escrow_id),
            DataKey::ShipmentProof(escrow_id),
            DataKey::ReleaseCondition(escrow_id),
            DataKey::DualControlConfig(escrow_id),
            DataKey::EscrowYieldConfig(escrow_id),
            DataKey::RequireReleaseCondition(escrow_id),
            DataKey::LastBumpLedger(escrow_id),
        ];
        let storage = env.storage().persistent();
        let mut cleared_entries: u32 = 0;
        for aux_key in aux_keys.iter() {
            if storage.has(aux_key) {
                cleared_entries += 1;
            }
        }
        cleared_entries += 1; // the escrow record itself

        // Emitted *before* the removals: once the sweep returns, the state this
        // event describes no longer exists, so the record of what was purged
        // has to be in the log ahead of the deletion.
        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("archived"),
                escrow_id,
            ),
            EscrowArchivedEvent {
                escrow_id,
                terminal_state,
                terminal_at: record.updated_at,
                archived_at,
                cleared_entries,
            },
        );

        for aux_key in aux_keys.iter() {
            storage.remove(aux_key);
        }
        storage.remove(&key);

        Ok(())
    }

    /// Remove a co-admin. Must be called by the primary admin.
    pub fn remove_co_admin(
        env: Env,
        admin: Address,
        co_admin: Address,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        let primary_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(EscrowError::NotFound)?;
        if admin != primary_admin {
            return Err(EscrowError::Unauthorized);
        }
        let mut admin_list: soroban_sdk::Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AdminList)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env));
        let index = match admin_list.first_index_of(&co_admin) {
            Some(idx) => idx,
            None => return Err(EscrowError::NotFound),
        };
        admin_list.remove(index);
        env.storage()
            .instance()
            .set(&DataKey::AdminList, &admin_list);
        Ok(true)
    }

    /// Configure the marketplace registry used to reject suspended merchant
    /// sellers when creating new escrows. Admin-only.
    pub fn set_merchant_registry(
        env: Env,
        admin: Address,
        registry: Address,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin) {
            return Err(EscrowError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&DataKey::MerchantRegistry, &registry);
        Ok(true)
    }

    /// Set the appeals council address for two-tiered dispute resolution (issue #354).
    ///
    /// The appeals council is the only address authorized to call `finalize_dispute_appeal`
    /// to resolve appeals. This enables a decentralized governance model where multiple
    /// arbiters can review dispute appeals and overturn initial rulings.
    ///
    /// Admin-only.
    pub fn set_appeals_council(
        env: Env,
        admin: Address,
        council: Address,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin) {
            return Err(EscrowError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&DataKey::AppealsCouncil, &council);
        Ok(true)
    }

    /// Configure the external contract invoked to mint an NFT
    /// proof-of-purchase receipt when an escrow is fully released
    /// (issue #320). Admin-only. Passing the zero address is rejected;
    /// there is no supported way to *unset* it once configured other than
    /// pointing it at a no-op contract, matching how other optional
    /// cross-contract integrations (e.g. `set_merchant_registry`) work in
    /// this contract.
    pub fn set_receipt_minter_contract(
        env: Env,
        admin: Address,
        minter: Address,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin) {
            return Err(EscrowError::Unauthorized);
        }
        if is_zero_address(&env, &minter) {
            return Err(EscrowError::InvalidAddress);
        }
        env.storage()
            .instance()
            .set(&DataKey::ReceiptMinterContract, &minter);
        Ok(true)
    }

    /// Returns the NFT receipt token id minted for `escrow_id`, if any
    /// (issue #320). `Ok(None)` covers both "not released yet" and "no
    /// receipt minter configured" / "minting failed" — none of these are
    /// error conditions since minting is best-effort by design.
    pub fn get_purchase_receipt_token_id(
        env: Env,
        escrow_id: u64,
    ) -> Result<Option<u64>, EscrowError> {
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Escrow(escrow_id))
            .ok_or(EscrowError::NotFound)?;
        Ok(record.receipt_token_id)
    }

    /// Best-effort call to the configured receipt-minting contract on full
    /// release (issue #320). Mutates `record.receipt_token_id` in place on
    /// success; on any failure (no minter configured, the cross-contract
    /// call trapping, or it returning an error) this silently leaves
    /// `receipt_token_id` as `None` and does not propagate an error —
    /// minting failures must never block fund settlement, which by this
    /// point has already happened.
    ///
    /// The external contract is expected to expose a
    /// `mint_receipt(receipt: PurchaseReceiptData) -> u64` entry point that
    /// mints (and transfers to `receipt.buyer`) a soulbound receipt token
    /// and returns its token id.
    fn try_mint_purchase_receipt(env: &Env, record: &mut EscrowRecord) {
        let minter: Address = match env
            .storage()
            .instance()
            .get(&DataKey::ReceiptMinterContract)
        {
            Some(addr) => addr,
            None => return,
        };

        let item_sku: Symbol = env
            .storage()
            .persistent()
            .get(&DataKey::EscrowMetadataSchema(record.escrow_id))
            .unwrap_or_else(|| symbol_short!("generic"));

        let receipt = PurchaseReceiptData {
            order_id: record.order_id.clone(),
            buyer: record.buyer.clone(),
            seller: record.seller.clone(),
            amount: record.amount,
            completed_at: env.ledger().timestamp(),
            item_sku,
        };

        let args = soroban_sdk::vec![env, receipt.into_val(env)];
        let result = env.try_invoke_contract::<u64, InvokeError>(
            &minter,
            &Symbol::new(env, "mint_receipt"),
            args,
        );

        if let Ok(Ok(token_id)) = result {
            record.receipt_token_id = Some(token_id);
            env.events().publish(
                (symbol_short!("escrow"), symbol_short!("receipt")),
                PurchaseReceiptMintedEvent {
                    escrow_id: record.escrow_id,
                    token_id,
                    buyer: record.buyer.clone(),
                },
            );
        }
        // Any other outcome (no such contract, it trapped, it returned an
        // application error, or a decode failure) is swallowed on purpose:
        // fund settlement must not be blocked by a receipt-minting failure.
    }
    /// Set authorized merchant categories (MCC codes) for spend validation.
    ///
    /// When set, the escrow contract will validate that the seller's merchant
    /// category matches one of these authorized categories during spend operations.
    /// This enforces merchant category restrictions as configured by the platform.
    ///
    /// Admin-only. Setting an empty list disables category validation.
    pub fn set_authorized_categories(
        env: Env,
        admin: Address,
        categories: soroban_sdk::Vec<Symbol>,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin) {
            return Err(EscrowError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&DataKey::AuthorizedCategories, &categories);
        Ok(true)
    }

    /// Get the currently authorized merchant categories.
    pub fn get_authorized_categories(env: Env) -> soroban_sdk::Vec<Symbol> {
        env.storage()
            .instance()
            .get(&DataKey::AuthorizedCategories)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env))
    }

    /// Validate that a seller's merchant category is authorized for spending.
    ///
    /// This function checks the marketplace registry to verify the seller's
    /// category matches one of the authorized categories configured on the escrow.
    /// Returns `Ok(())` on success, or `Err(EscrowError::MerchantCategoryNotAllowed)` on failure.
    fn validate_seller_category(env: &Env, seller: &Address) -> Result<(), EscrowError> {
        // Get authorized categories
        let authorized_categories = Self::get_authorized_categories(env.clone());

        // If no categories configured, skip validation
        if authorized_categories.is_empty() {
            return Ok(());
        }

        // Get marketplace registry
        let registry: Address = match env
            .storage()
            .instance()
            .get::<_, Address>(&DataKey::MerchantRegistry)
        {
            Some(r) => r,
            None => return Ok(()), // No registry, skip validation
        };

        // Try calling the marketplace to validate merchant category by seller address
        let result = env.try_invoke_contract::<u64, EscrowError>(
            &registry,
            &Symbol::new(env, "get_merchant_id_by_owner"),
            soroban_sdk::vec![env, seller.into_val(env)],
        );

        let merchant_id = match result {
            Ok(Ok(id)) => id,
            _ => return Ok(()), // If we can't find merchant, allow (fail open)
        };

        // Now validate the category
        use soroban_sdk::IntoVal;
        let args = soroban_sdk::vec![
            env,
            merchant_id.into_val(env),
            authorized_categories.into_val(env)
        ];
        let args = soroban_sdk::vec![env, merchant_id.into_val(env), authorized_categories.to_val()];
        let validation_result = env.try_invoke_contract::<(), EscrowError>(
            &registry,
            &Symbol::new(&env, "validate_merchant_category"),
            args,
        );

        match validation_result {
            Ok(Ok(())) => Ok(()),
            _ => Err(EscrowError::Unauthorized),
        }
    }

    /// Register (or update) an approved metadata schema. Admin or co-admin only.
    ///
    /// Gating this on admin auth keeps arbitrary callers from polluting the
    /// schema catalog with deceptive entries. `schema_definition_uri` is the
    /// hash of the off-chain schema definition document.
    ///
    /// # Errors
    /// - [`EscrowError::Unauthorized`] if `admin` is not an admin.
    pub fn register_schema(
        env: Env,
        admin: Address,
        schema: Symbol,
        schema_definition_uri: BytesN<32>,
    ) -> Result<(), EscrowError> {
        Self::require_admin(&env, &admin)?;

        let key = DataKey::RegisteredSchema(schema.clone());
        env.storage().persistent().set(&key, &schema_definition_uri);
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("schemreg")),
            SchemaRegisteredEvent {
                schema,
                schema_definition_uri,
                registered_by: admin,
            },
        );
        Ok(())
    }

    /// Returns the definition hash of a registered schema, if any.
    pub fn get_schema_definition(env: Env, schema: Symbol) -> Option<BytesN<32>> {
        env.storage()
            .persistent()
            .get(&DataKey::RegisteredSchema(schema))
    }

    /// Requires `admin`'s auth and that it is the primary admin or a co-admin.
    fn require_admin(env: &Env, admin: &Address) -> Result<(), EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        Ok(())
    }

    /// Configure the Ed25519 public key authorized to sign delivery proofs.
    /// Admin-only; changing this key immediately changes which proofs are valid.
    pub fn set_oracle_public_key(
        env: Env,
        admin: Address,
        public_key: BytesN<32>,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin) {
            return Err(EscrowError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&DataKey::OraclePublicKey, &public_key);
        Ok(true)
    }

    /// Set or clear the admin pause flag for new escrow creation. Admin-only.
    pub fn set_create_paused(env: Env, admin: Address, paused: bool) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        let pause_state = EscrowPauseState {
            create_paused: paused,
            updated_by: admin.clone(),
            updated_at_ledger: env.ledger().sequence(),
            expires_at_ledger: None,
        };
        env.storage()
            .instance()
            .set(&DataKey::PauseState, &pause_state);
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("paused")),
            EscrowPauseChangedEvent {
                paused,
                admin,
                ledger: env.ledger().sequence(),
            },
        );
        Ok(true)
    }

    /// Set an emergency pause with automatic expiry after specified duration in ledgers.
    /// Admin-only. The pause auto-expires after `duration_ledgers` ledgers.
    pub fn set_emergency_pause(
        env: Env,
        admin: Address,
        paused: bool,
        duration_ledgers: u32,
    ) -> Result<bool, EscrowError> {
        admin.require_auth();
        if !Self::is_admin(env.clone(), admin.clone()) {
            return Err(EscrowError::Unauthorized);
        }
        let current_ledger = env.ledger().sequence();
        let expires_at = if paused && duration_ledgers > 0 {
            Some(current_ledger.saturating_add(duration_ledgers))
        } else {
            None
        };
        let pause_state = EscrowPauseState {
            create_paused: paused,
            updated_by: admin.clone(),
            updated_at_ledger: current_ledger,
            expires_at_ledger: expires_at,
        };
        env.storage()
            .instance()
            .set(&DataKey::PauseState, &pause_state);
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("paused")),
            EscrowPauseChangedEvent {
                paused,
                admin,
                ledger: current_ledger,
            },
        );
        Ok(true)
    }

    /// Get the current escrow creation pause state.
    /// Returns false if pause has expired.
    pub fn get_create_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get::<DataKey, EscrowPauseState>(&DataKey::PauseState)
            .map(|s| {
                if let Some(expires_at) = s.expires_at_ledger {
                    s.create_paused && env.ledger().sequence() < expires_at
                } else {
                    s.create_paused
                }
            })
            .unwrap_or(false)
    }

    /// Get the token address associated with an escrow.
    pub fn get_token(env: Env, escrow_id: u64) -> Result<EscrowTokenView, EscrowError> {
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;
        Ok(EscrowTokenView {
            escrow_id,
            token: record.token,
        })
    }

    /// Get the optional metadata for an escrow.
    ///
    /// Returns the metadata if it was provided during escrow creation.
    /// Returns [`EscrowError::NotFound`] when no escrow exists for
    /// `escrow_id`, or [`EscrowError::MetadataNotSet`] when the escrow
    /// exists but no metadata was stored.
    pub fn get_escrow_metadata(env: Env, escrow_id: u64) -> Result<EscrowMetadata, EscrowError> {
        let escrow: EscrowRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Escrow(escrow_id))
            .ok_or(EscrowError::NotFound)?;
        let _ = escrow;

        let order_hash: Option<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&DataKey::EscrowMetadataHash(escrow_id));
        let schema: Option<Symbol> = env
            .storage()
            .persistent()
            .get(&DataKey::EscrowMetadataSchema(escrow_id));

        let order_hash = order_hash.ok_or(EscrowError::MetadataNotSet)?;
        let schema = schema.ok_or(EscrowError::MetadataNotSet)?;
        Ok(EscrowMetadata { order_hash, schema })
    }

    /// Fill in (or overwrite) the order-hash half of an escrow's metadata
    /// after creation (issue #39). Buyer or admin only. Useful when an escrow
    /// was created with only a schema, or with no metadata at all.
    pub fn set_escrow_metadata_hash(
        env: Env,
        escrow_id: u64,
        caller: Address,
        order_hash: BytesN<32>,
    ) -> Result<(), EscrowError> {
        caller.require_auth();

        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Escrow(escrow_id))
            .ok_or(EscrowError::NotFound)?;
        if caller != record.buyer && !Self::is_admin(env.clone(), caller.clone()) {
            return Err(EscrowError::Unauthorized);
        }

        env.storage()
            .persistent()
            .set(&DataKey::EscrowMetadataHash(escrow_id), &order_hash);
        Ok(())
    }

    /// Fill in (or overwrite) the schema half of an escrow's metadata after
    /// creation (issue #39). Buyer or admin only. Useful when an escrow was
    /// created with only a hash, or with no metadata at all.
    pub fn set_escrow_metadata_schema(
        env: Env,
        escrow_id: u64,
        caller: Address,
        schema: Symbol,
    ) -> Result<(), EscrowError> {
        caller.require_auth();

        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Escrow(escrow_id))
            .ok_or(EscrowError::NotFound)?;
        if caller != record.buyer && !Self::is_admin(env.clone(), caller.clone()) {
            return Err(EscrowError::Unauthorized);
        }

        env.storage()
            .persistent()
            .set(&DataKey::EscrowMetadataSchema(escrow_id), &schema);
        Ok(())
    }

    /// Verifies that `raw_json_bytes` — the canonical off-chain order
    /// payload — hashes (SHA-256) to exactly `expected_hash` (issue #321).
    ///
    /// This is a pure, standalone digest check with no storage access, so it
    /// can validate a payload against *any* hash (not only one already
    /// stored for an escrow) — e.g. before calling `create`/`deposit`, to
    /// confirm the `order_hash` about to be submitted matches the payload a
    /// client just canonicalized.
    ///
    /// A stable client-side digest requires a canonical, deterministic JSON
    /// encoding (fixed key order, no incidental whitespace) before hashing;
    /// see [`Self::verify_escrow_order_metadata`] for the on-chain
    /// counterpart that reads an escrow's *stored* `order_hash`.
    ///
    /// ```text
    /// // Sample off-chain (TypeScript) client SDK code computing the same
    /// // digest, so a buyer's canonical order JSON is guaranteed
    /// // bit-for-bit identical to what `verify_order_metadata_digest`
    /// // recomputes on-chain:
    /// //
    /// //   import { createHash } from "node:crypto";
    /// //
    /// //   // `order` fields MUST be serialized in a fixed, sorted key
    /// //   // order with no extra whitespace — any deviation changes the
    /// //   // digest.
    /// //   function canonicalOrderJson(order: Record<string, unknown>): Buffer {
    /// //     const sortedKeys = Object.keys(order).sort();
    /// //     const canonical: Record<string, unknown> = {};
    /// //     for (const key of sortedKeys) canonical[key] = order[key];
    /// //     return Buffer.from(JSON.stringify(canonical), "utf-8");
    /// //   }
    /// //
    /// //   function computeOrderHash(order: Record<string, unknown>): Buffer {
    /// //     return createHash("sha256").update(canonicalOrderJson(order)).digest();
    /// //   }
    /// //
    /// //   // `computeOrderHash(order)` is the exact 32 bytes to pass as
    /// //   // `order_hash` to `create`/`deposit`, and as `expected_hash` to
    /// //   // `verify_order_metadata_digest`.
    /// ```
    pub fn verify_order_metadata_digest(
        env: Env,
        raw_json_bytes: Bytes,
        expected_hash: BytesN<32>,
    ) -> bool {
        let computed: BytesN<32> = env.crypto().sha256(&raw_json_bytes).into();
        computed == expected_hash
    }

    /// Verifies `raw_json_bytes` against the `order_hash` already stored for
    /// `escrow_id` (issue #321), so a relying party can confirm the
    /// off-chain order payload it holds is exactly the one the buyer
    /// committed to at deposit time — an unvalidated `order_hash` alone
    /// cannot be tampered with post-deposit, but nothing previously checked
    /// that a *given* payload actually produces it.
    ///
    /// # Errors
    /// Returns [`EscrowError::NotFound`] when no escrow exists for
    /// `escrow_id`, or [`EscrowError::MetadataNotSet`] when the escrow
    /// exists but has no stored `order_hash`.
    pub fn verify_escrow_order_metadata(
        env: Env,
        escrow_id: u64,
        raw_json_bytes: Bytes,
    ) -> Result<bool, EscrowError> {
        if !env.storage().persistent().has(&DataKey::Escrow(escrow_id)) {
            return Err(EscrowError::NotFound);
        }
        let expected_hash: BytesN<32> = env
            .storage()
            .persistent()
            .get(&DataKey::EscrowMetadataHash(escrow_id))
            .ok_or(EscrowError::MetadataNotSet)?;

        Ok(Self::verify_order_metadata_digest(
            env,
            raw_json_bytes,
            expected_hash,
        ))
    }

    /// Returns true if the address is the primary admin or a co-admin.
    /// Read-only check: returns whether the given caller is eligible to refund
    /// the specified escrow, and a machine-readable reason symbol (issue #173).
    ///
    /// Reason symbols (≤7 chars for `symbol_short!` compat):
    ///   `ok`       — caller may refund right now
    ///   `notfund`  — escrow not found
    ///   `released` — already released (terminal)
    ///   `refunded` — already refunded (terminal)
    ///   `disputed` — escrow is under dispute
    ///   `noauth`   — caller is not buyer/seller/admin
    ///   `timeout`  — buyer must wait for timeout
    pub fn get_refund_eligibility(env: Env, escrow_id: u64, caller: Address) -> RefundEligibility {
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => {
                return RefundEligibility {
                    escrow_id,
                    eligible: false,
                    reason: symbol_short!("notfund"),
                };
            }
        };

        // Terminal states
        if record.status == EscrowStatus::Released {
            return RefundEligibility {
                escrow_id,
                eligible: false,
                reason: symbol_short!("released"),
            };
        }
        if record.status == EscrowStatus::Refunded {
            return RefundEligibility {
                escrow_id,
                eligible: false,
                reason: symbol_short!("refunded"),
            };
        }
        if record.status == EscrowStatus::Cancelled {
            return RefundEligibility {
                escrow_id,
                eligible: false,
                reason: symbol_short!("cancelled"),
            };
        }
        if record.status == EscrowStatus::Created {
            return RefundEligibility {
                escrow_id,
                eligible: false,
                reason: symbol_short!("unfunded"),
            };
        }
        if record.status == EscrowStatus::Disputed {
            return RefundEligibility {
                escrow_id,
                eligible: false,
                reason: symbol_short!("disputed"),
            };
        }

        // Must be Funded at this point
        let is_seller = caller == record.seller;
        let is_buyer = caller == record.buyer;
        let is_admin = Self::is_admin(env.clone(), caller.clone());

        if is_seller || is_admin {
            return RefundEligibility {
                escrow_id,
                eligible: true,
                reason: symbol_short!("ok"),
            };
        }

        if is_buyer {
            let timeout_reached = env.ledger().sequence() >= record.timeout_ledger;
            if timeout_reached {
                return RefundEligibility {
                    escrow_id,
                    eligible: true,
                    reason: symbol_short!("ok"),
                };
            } else {
                return RefundEligibility {
                    escrow_id,
                    eligible: false,
                    reason: symbol_short!("timeout"),
                };
            }
        }

        RefundEligibility {
            escrow_id,
            eligible: false,
            reason: symbol_short!("noauth"),
        }
    }

    /// Read-only timeout metadata for a single escrow (issue #88).
    ///
    /// Returns the timeout ledger, the current ledger, and whether the buyer
    /// is currently eligible to trigger a refund based purely on the timeout.
    /// The getter does **not** mutate any contract state — safe to call at any
    /// time without auth.
    ///
    /// `refundable` is `true` only when the escrow is still `Funded` **and**
    /// `current_ledger >= timeout_ledger`.  Terminal states (`Released`,
    /// `Refunded`) and disputed escrows always return `refundable: false`.
    ///
    /// # Errors
    /// Returns [`EscrowError::NotFound`] when no escrow exists for `escrow_id`.
    pub fn get_timeout_view(env: Env, escrow_id: u64) -> Result<EscrowTimeoutView, EscrowError> {
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;

        let current_ledger = env.ledger().sequence();
        let timeout_ledger = record.timeout_ledger;

        // Only a Funded escrow can become refundable via timeout.
        // Released, Refunded, and Disputed states are not refundable here.
        let refundable = record.status == EscrowStatus::Funded && current_ledger >= timeout_ledger;

        Ok(EscrowTimeoutView {
            escrow_id: record.order_id,
            timeout_ledger,
            current_ledger,
            refundable,
        })
    }

    fn validate_release_status(record: &EscrowRecord) -> Result<(), EscrowError> {
        if record.status == EscrowStatus::Released {
            return Err(EscrowError::AlreadyReleased);
        }

        if record.status != EscrowStatus::Funded {
            return Err(EscrowError::InvalidStatus);
        }

        Ok(())
    }

    /// Yield accrued for an escrow, preferring the configured external
    /// lending pool's own accounting when available (issue #326).
    ///
    /// When a `YieldConfig` is set, the escrow drives its `lending_contract`
    /// through the shared [`LendingPoolClient`] and trusts the pool-reported
    /// `get_accrued_yield`, which reflects the actual on-pool position rather
    /// than the simple APR formula.  If the pool is unreachable, paused, or
    /// reports a non-positive figure, it falls back to the internal
    /// [`Self::compute_yield`] estimate so the escrow keeps returning honest
    /// numbers while the external protocol is degraded.  `held_seconds`
    /// remains ledger-derived so the [`YieldView`] stays monotonic.
    ///
    /// Never mutates storage.
    fn accrued_yield(
        record: &EscrowRecord,
        yield_config: &YieldConfig,
        env: &Env,
    ) -> (i128, u64) {
        let (fallback, held_seconds) = Self::compute_yield(record, Some(yield_config), env);
        let pool_yield = match LendingPoolClient::new(env, &yield_config.lending_contract)
            .try_get_accrued_yield(&env.current_contract_address())
        {
            Ok(Ok(pool_report)) if pool_report > 0 => Some(pool_report),
            _ => None,
        };
        (pool_yield.unwrap_or(fallback), held_seconds)
    }

    /// Computes yield accrued on an escrow's remaining principal for the time
    /// it has been held, based on the given `YieldConfig` APR.  The principal
    /// is `record.amount - record.released_amount - record.refunded_amount`
    /// so partially settled escrows report honest yield (issue #33). Returns
    /// `(yield_amount, held_seconds)`; `(0, 0)` when no yield config is set.
    fn compute_yield(
        record: &EscrowRecord,
        yield_config: Option<&YieldConfig>,
        env: &Env,
    ) -> Result<(i128, u64), EscrowError> {
        match yield_config {
            Some(cfg) => {
                let held_seconds = env.ledger().timestamp().saturating_sub(record.created_at);
                let remaining = record.amount - record.released_amount - record.refunded_amount;
                let yield_amount = (remaining * cfg.apr_bps as i128 * held_seconds as i128)
                    / (10_000i128 * SECONDS_PER_YEAR);
                (yield_amount, held_seconds)
                let remaining =
                    record.amount
                        .checked_sub(record.released_amount)
                        .and_then(|amount| amount.checked_sub(record.refunded_amount))
                        .ok_or(EscrowError::MathOverflow)?;
                let yield_amount = calculate_yield(remaining, cfg.apr_bps, held_seconds)?;
                Ok((yield_amount, held_seconds))
            }
            None => Ok((0, 0)),
        }
    }

    fn release_block_reason(env: Env, record: &EscrowRecord) -> Option<Symbol> {
        match record.status {
            EscrowStatus::Funded => {
                if env.ledger().sequence() >= record.timeout_ledger {
                    Some(symbol_short!("timeout"))
                } else {
                    None
                }
            }
            EscrowStatus::Inspection => Some(symbol_short!("inspect")),
            EscrowStatus::Created => Some(symbol_short!("unfunded")),
            EscrowStatus::Released => Some(symbol_short!("released")),
            EscrowStatus::Refunded => Some(symbol_short!("refunded")),
            EscrowStatus::Disputed => Some(symbol_short!("disputed")),
            EscrowStatus::Cancelled => Some(symbol_short!("cancelled")),
        }
    }

    /// Release escrowed funds split among multiple recipients (#321).
    ///
    /// `shares` is a list of `(recipient, amount)` pairs. The sum of all
    /// amounts must not exceed the remaining escrow balance. A platform fee
    /// is deducted from each individual share before transfer.
    /// Only the buyer or an admin may call this.
    pub fn split_release(
        env: Env,
        escrow_id: u64,
        caller: Address,
        shares: Vec<(Address, i128)>,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();
        Self::enter_release(&env)?;

        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };

        if caller != record.buyer && !Self::is_admin(env.clone(), caller.clone()) {
            return Err(EscrowError::Unauthorized);
        }

        check_not_terminal(&record)?;
        Self::ensure_not_milestone_escrow(&env, escrow_id)?;

        if record.status != EscrowStatus::Funded {
            return Err(EscrowError::InvalidStatus);
        }

        // Validate total split amount
        let remaining = record.amount - record.released_amount - record.refunded_amount;
        let mut total: i128 = 0;
        for (_, amount) in shares.iter() {
            if amount <= 0 {
                return Err(EscrowError::InvalidAmount);
            }
            total = total
                .checked_add(amount)
                .ok_or(EscrowError::MathOverflow)?;
        }
        if total > remaining {
            return Err(EscrowError::InsufficientEscrowBalance);
        }
        Self::assert_value_conservation_invariant(&record, total, 0)?;

        let token_client = soroban_sdk::token::Client::new(&env, &record.token);

        let mut total_fee: i128 = 0;
        let mut total_released: i128 = 0;
        let mut transfers = Vec::new(&env);

        for (recipient, amount) in shares.iter() {
            let fee = Self::compute_fee_amount(&env, &record.seller, amount)?;
            let net = amount - fee;

            token_client.transfer(&env.current_contract_address(), &recipient, &net);

            total_fee = total_fee
                .checked_add(fee)
                .ok_or(EscrowError::MathOverflow)?;
            total_released = total_released
                .checked_add(amount)
                .ok_or(EscrowError::MathOverflow)?;
            transfers.push_back((recipient, net));
            total_fee += fee;
            total_released += amount;
        }

        record.released_amount += total_released;
        update_merchant_volume_and_tier(&env, &record.seller, total_released);
        let new_remaining = record.amount - record.released_amount - record.refunded_amount;
        record.released_amount = record
            .released_amount
            .checked_add(total_released)
            .ok_or(EscrowError::MathOverflow)?;
        let total_disbursed = record
            .released_amount
            .checked_add(record.refunded_amount)
            .ok_or(EscrowError::MathOverflow)?;
        let new_remaining = record
            .amount
            .checked_sub(total_disbursed)
            .ok_or(EscrowError::MathOverflow)?;
        if new_remaining == 0 {
            record.status = EscrowStatus::Released;
            Self::try_mint_purchase_receipt(&env, &mut record);
        }
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);

        for (recipient, net) in transfers.iter() {
            token_client.transfer(&env.current_contract_address(), &recipient, &net);
        }
        Self::distribute_fee(&env, &token_client, total_fee)?;

        // #45: A split release that exhausts the escrow balance is a terminal
        // payout path, so it reports the yield accrued over the holding period
        // just like the other terminal payout paths.
        if new_remaining == 0 {
            let yield_config: Option<YieldConfig> = env
                .storage()
                .persistent()
                .get(&DataKey::EscrowYieldConfig(escrow_id));
            if let Some(cfg) = &yield_config {
                let (yield_amount, held_seconds) = Self::compute_yield(&record, Some(cfg), &env)?;
                env.events().publish(
                    (symbol_short!("escrow"), symbol_short!("yield"), escrow_id),
                    EscrowYieldAccruedEvent {
                        escrow_id,
                        seller: record.seller.clone(),
                        yield_amount,
                        held_seconds,
                    },
                );
            }
        }

        env.events().publish(
            (
                symbol_short!("escrow"),
                symbol_short!("splitrel"),
                escrow_id,
            ),
            EscrowSplitReleasedEvent {
                escrow_id,
                recipient_count: shares.len(),
                total_released,
                fee_charged: total_fee,
                released_by: caller,
            },
        );

        Ok(true)
    }

    /// Extend the timeout ledger of a `Funded` escrow (#323).
    ///
    /// Requires mutual authentication from both buyer and seller (both must
    /// sign the transaction), OR unilateral authorization from an admin.
    /// `new_timeout_ledger` must be strictly greater than the current value.
    pub fn extend_timeout(
        env: Env,
        escrow_id: u64,
        caller: Address,
        new_timeout_ledger: u32,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();

        let key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = match env.storage().persistent().get(&key) {
            Some(rec) => rec,
            None => return Err(EscrowError::NotFound),
        };

        if record.status != EscrowStatus::Funded {
            return Err(EscrowError::InvalidStatus);
        }

        if new_timeout_ledger <= record.timeout_ledger {
            return Err(EscrowError::InvalidExtension);
        }

        // Authorization: admin can do it alone; otherwise both buyer AND seller must sign.
        if !Self::is_admin(env.clone(), caller.clone()) {
            // Caller must be either buyer or seller, and both must authenticate.
            if caller != record.buyer && caller != record.seller {
                return Err(EscrowError::Unauthorized);
            }
            record.buyer.require_auth();
            record.seller.require_auth();
        }

        let old_timeout = record.timeout_ledger;
        record.timeout_ledger = new_timeout_ledger;
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("exttime"), escrow_id),
            EscrowTimeoutExtendedEvent {
                escrow_id,
                old_timeout_ledger: old_timeout,
                new_timeout_ledger,
                extended_by: caller,
            },
        );

        Ok(true)
    }

    /// Extend a funded escrow timeout using a mutually signed voucher (#357).
    ///
    /// The buyer and seller each authenticate the exact voucher arguments.
    /// This keeps the extension gas-efficient for a relayer while ensuring
    /// that changing the escrow, deadline, nonce, or either signature causes
    /// authorization to fail. Nonces make a valid voucher single-use.
    pub fn extend_timeout_with_mutual_signatures(
        env: Env,
        voucher: TimeoutExtensionVoucher,
    ) -> Result<bool, EscrowError> {
        let key = DataKey::Escrow(voucher.escrow_id);
        let mut record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;
        if record.status != EscrowStatus::Funded {
            return Err(EscrowError::InvalidStatus);
        }
        if voucher.new_timeout_ledger <= record.timeout_ledger {
            return Err(EscrowError::InvalidExtension);
        }
        let previous_nonce: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::TimeoutExtensionNonce(voucher.escrow_id))
            .unwrap_or(0);
        if voucher.nonce <= previous_nonce {
            return Err(EscrowError::TimeoutExtensionNonceUsed);
        }

        let auth_args = (
            voucher.escrow_id,
            voucher.new_timeout_ledger,
            voucher.nonce,
            voucher.buyer_signature.clone(),
            voucher.seller_signature.clone(),
        );
        record.buyer.require_auth_for_args(auth_args.clone());
        record.seller.require_auth_for_args(auth_args);

        let old_timeout = record.timeout_ledger;
        record.timeout_ledger = voucher.new_timeout_ledger;
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);
        env.storage()
            .persistent()
            .set(&DataKey::TimeoutExtensionNonce(voucher.escrow_id), &voucher.nonce);
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("exttime"), voucher.escrow_id),
            EscrowTimeoutExtendedEvent {
                escrow_id: voucher.escrow_id,
                old_timeout_ledger: old_timeout,
                new_timeout_ledger: voucher.new_timeout_ledger,
                extended_by: record.buyer,
            },
        );
        Ok(true)
    }

    /// Paginated enumeration of all escrows (issue #49).
    ///
    /// Returns up to `min(limit, MAX_PAGE_LIMIT)` [`EscrowRecord`]s starting
    /// at zero-based `offset`. The global escrow ID list is maintained by
    /// `create_internal`, so records are returned in creation order.
    ///
    /// `page.total`       — total number of escrows ever created.
    /// `page.next_offset` — `Some(next)` when another page follows; `None` on
    ///                      the last page.
    pub fn list_escrows(env: Env, offset: u32, limit: u32) -> EscrowListPage {
        let all_ids: soroban_sdk::Vec<u64> = env
            .storage()
            .instance()
            .get(&DataKey::EscrowIds)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env));

        let total = all_ids.len();
        let capped_limit = limit.min(MAX_PAGE_LIMIT);
        let start = offset.min(total);
        let end = (start + capped_limit).min(total);

        let mut items = soroban_sdk::Vec::new(&env);
        for i in start..end {
            let escrow_id = all_ids.get(i).unwrap();
            if let Some(record) = env
                .storage()
                .persistent()
                .get::<DataKey, EscrowRecord>(&DataKey::Escrow(escrow_id))
            {
                items.push_back(record);
            }
        }

        let count = end - start;
        let next_offset = if end < total {
            Some(start + count)
        } else {
            None
        };

        EscrowListPage {
            items,
            total,
            next_offset,
        }
    }

    /// Paginated enumeration of escrows for a specific buyer (issue #49).
    ///
    /// Returns up to `min(limit, MAX_PAGE_LIMIT)` [`EscrowRecord`]s belonging
    /// to `buyer`, starting at zero-based `offset` within that buyer's index.
    /// The per-buyer index is maintained by `create_internal` in creation order.
    ///
    /// `page.total`       — total number of escrows created by this buyer.
    /// `page.next_offset` — `Some(next)` when another page follows; `None` on
    ///                      the last page.
    pub fn list_escrows_by_buyer(
        env: Env,
        buyer: Address,
        offset: u32,
        limit: u32,
    ) -> EscrowListPage {
        let total: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::BuyerEscrowCount(buyer.clone()))
            .unwrap_or(0);
        let capped_limit = limit.min(MAX_PAGE_LIMIT);
        let start = offset.min(total);
        let end = start.saturating_add(capped_limit).min(total);

        let mut items = soroban_sdk::Vec::new(&env);
        for i in start..end {
            let escrow_id: u64 = env
                .storage()
                .persistent()
                .get(&DataKey::BuyerEscrowAt(buyer.clone(), i))
                .unwrap();
            if let Some(record) = env
                .storage()
                .persistent()
                .get::<DataKey, EscrowRecord>(&DataKey::Escrow(escrow_id))
            {
                items.push_back(record);
            }
        }

        let count = end - start;
        let next_offset = if end < total {
            Some(start + count)
        } else {
            None
        };

        EscrowListPage {
            items,
            total,
            next_offset,
        }
    }

    pub fn is_admin(env: Env, address: Address) -> bool {
        let primary_admin: Address = match env.storage().instance().get(&DataKey::Admin) {
            Some(addr) => addr,
            None => return false,
        };
        if address == primary_admin {
            return true;
        }
        let admin_list: soroban_sdk::Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AdminList)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env));
        admin_list.contains(&address)
    }

    /// Configured upgrade threshold, defaulting to [`MIN_UPGRADE_THRESHOLD`].
    fn upgrade_threshold(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::UpgradeThreshold)
            .unwrap_or(MIN_UPGRADE_THRESHOLD)
    }

    /// Size of the upgrade signer set: the primary admin plus co-admins.
    fn upgrade_signer_count(env: &Env) -> u32 {
        let co_admins: soroban_sdk::Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AdminList)
            .unwrap_or_else(|| soroban_sdk::Vec::new(env));
        let has_primary = env.storage().instance().has(&DataKey::Admin);
        co_admins.len() + u32::from(has_primary)
    }

    /// The stored upgrade proposal, if it has not been executed yet.
    fn pending_upgrade_proposal(env: &Env) -> Option<UpgradeProposal> {
        env.storage()
            .instance()
            .get::<DataKey, UpgradeProposal>(&DataKey::UpgradeProposal)
            .filter(|p| !p.executed)
    }

    /// Count approvals from addresses that are still admins, so approvals
    /// from since-removed co-admins do not count toward the threshold.
    fn valid_upgrade_approvals(env: &Env, approvals: &Vec<Address>) -> u32 {
        approvals
            .iter()
            .filter(|a| Self::is_admin(env.clone(), a.clone()))
            .count() as u32
    }

    // ── Ticket 1: clear_release_condition ────────────────────────────────────

    /// Remove the release condition for an escrow. Admin or co-admin only.
    ///
    /// Useful when the configured oracle becomes unavailable or the condition
    /// is no longer required. Blocked on escrows that have already reached a
    /// terminal state (Released or Refunded) — those escrows are already
    /// settled so clearing the condition would have no effect and signals a
    /// caller logic error.
    ///
    /// # Errors
    /// - [`EscrowError::Unauthorized`] if `caller` is neither the primary
    ///   admin nor a co-admin.
    /// - [`EscrowError::NotFound`] if `escrow_id` does not exist.
    /// - [`EscrowError::AlreadyReleased`] if the escrow has been released.
    /// - [`EscrowError::AlreadyRefunded`] if the escrow has been refunded.
    /// - [`EscrowError::AlreadyCancelled`] if the escrow has been cancelled.
    pub fn clear_release_condition(
        env: Env,
        caller: Address,
        escrow_id: u64,
    ) -> Result<(), EscrowError> {
        caller.require_auth();
        if !Self::is_admin(env.clone(), caller) {
            return Err(EscrowError::Unauthorized);
        }
        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;
        check_not_terminal(&record)?;
        env.storage()
            .persistent()
            .remove(&DataKey::ReleaseCondition(escrow_id));
        Ok(())
    }

    /// Enable or disable the release-condition gate for buyer-originated
    /// releases on an escrow (issue #48). Admin-only.
    ///
    /// When `require` is `true`, buyer-originated `partial_release`/`release`
    /// on this escrow are blocked unless `get_release_eligibility` returns
    /// eligible. The default is `false` for backward compatibility.
    pub fn set_require_release_condition(
        env: Env,
        caller: Address,
        escrow_id: u64,
        require: bool,
    ) -> Result<bool, EscrowError> {
        caller.require_auth();
        if !Self::is_admin(env.clone(), caller) {
            return Err(EscrowError::Unauthorized);
        }

        let key = DataKey::Escrow(escrow_id);
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(EscrowError::NotFound)?;
        check_not_terminal(&record)?;

        env.storage()
            .persistent()
            .set(&DataKey::RequireReleaseCondition(escrow_id), &require);
        Ok(true)
    }

    /// Read-only getter for whether buyer-originated releases on an escrow are
    /// gated on the release condition. Defaults to `false` when unset.
    pub fn get_require_release_condition(env: Env, escrow_id: u64) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::RequireReleaseCondition(escrow_id))
            .unwrap_or(false)
    }

    // ── Ticket 2: get_yield_config ────────────────────────────────────────────

    /// Read-only getter for the yield configuration of an escrow.
    ///
    /// Returns `None` when no yield config has been set via `set_yield_config`.
    /// Never mutates storage.
    pub fn get_yield_config(env: Env, escrow_id: u64) -> Option<YieldConfig> {
        env.storage()
            .persistent()
            .get(&DataKey::EscrowYieldConfig(escrow_id))
    }

    /// Retrieve the yield split configuration for an escrow (issue #360).
    ///
    /// Returns the configured yield distribution between buyer and seller.
    /// If no `YieldSplitConfig` has been set for the escrow, returns `None`.
    /// Never mutates storage (read-only getter).
    ///
    /// # Arguments
    /// - `escrow_id` — Numeric identifier of the escrow.
    ///
    /// # Returns
    /// - `Some(config)` if a `YieldSplitConfig` has been configured via `set_yield_split_config`.
    /// - `None` if no split configuration exists for this escrow (default behavior: seller gets 100%).
    pub fn get_yield_split_config(env: Env, escrow_id: u64) -> Option<YieldSplitConfig> {
        env.storage()
            .persistent()
            .get(&DataKey::EscrowYieldSplitConfig(escrow_id))
    }

    // ── Ticket 3: get_co_admins / get_pending_admin ───────────────────────────

    /// Read-only getter for the list of co-admins.
    ///
    /// Returns an empty `Vec` when no co-admins have been added — never
    /// panics on missing state. Never mutates storage.
    pub fn get_co_admins(env: Env) -> soroban_sdk::Vec<Address> {
        env.storage()
            .instance()
            .get(&DataKey::AdminList)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env))
    }

    /// Read-only getter for the pending (proposed) new primary admin.
    ///
    /// Returns `None` when no admin transfer is in progress. Never mutates
    /// storage.
    pub fn get_pending_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::PendingAdmin)
    }

    /// Propose an emergency rescue for an escrow stranded by a broken or paused token contract.
    /// 
    /// Only admins can propose rescues. The proposal triggers a 14-day timelock and requires
    /// multi-sig approval from co-admins before funds can be recovered.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment
    /// * `escrow_id` - The ID of the escrow to rescue
    /// * `recovery_destination` - Address where recovered funds should be sent
    /// * `proposer` - Admin address proposing the rescue (must be an admin or co-admin)
    ///
    /// # Returns
    ///
    /// A `Result` that contains `true` if the proposal was created successfully, or
    /// an `EscrowError` if validation failed.
    ///
    /// # Errors
    ///
    /// * `Unauthorized` - if caller is not an admin
    /// * `NotFound` - if the escrow does not exist
    /// * `EscrowNotEligibleForRescue` - if escrow is in terminal or invalid state
    /// * `InvalidAddress` - if recovery_destination is zero/invalid
    pub fn propose_emergency_rescue(
        env: Env,
        escrow_id: u64,
        recovery_destination: Address,
        proposer: Address,
    ) -> Result<bool, EscrowError> {
        proposer.require_auth();
        if !Self::is_admin(env.clone(), proposer.clone()) {
            return Err(EscrowError::Unauthorized);
        }

        // Validate recovery destination
        if is_zero_address(&env, &recovery_destination) {
            return Err(EscrowError::InvalidAddress);
        }

        // Get the escrow record
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Escrow(escrow_id))
            .ok_or(EscrowError::NotFound)?;

        // Only Funded or Disputed escrows can be rescued
        match record.status {
            EscrowStatus::Funded | EscrowStatus::Disputed => {}
            _ => return Err(EscrowError::EscrowNotEligibleForRescue),
        }

        // Create the proposal
        let proposal = EmergencyRescueProposal {
            escrow_id,
            recovery_destination: recovery_destination.clone(),
            proposed_at: env.ledger().timestamp(),
            approvals: {
                let mut vec = soroban_sdk::Vec::new(&env);
                vec.push_back(proposer.clone());
                vec
            },
            executed: false,
        };

        env.storage().persistent().set(
            &DataKey::EmergencyRescueProposal(escrow_id),
            &proposal,
        );

        Ok(true)
    }

    /// Approve a pending emergency rescue proposal.
    ///
    /// Co-admins can approve rescue proposals. Once enough approvals are collected
    /// (meeting the upgrade threshold) and the 14-day timelock elapses, the rescue
    /// can be executed.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment
    /// * `escrow_id` - The ID of the escrow with a pending rescue proposal
    /// * `approver` - Co-admin address approving the rescue
    ///
    /// # Returns
    ///
    /// A `Result` containing the current approval count if successful.
    ///
    /// # Errors
    ///
    /// * `Unauthorized` - if caller is not an admin
    /// * `EmergencyRescueProposalNotFound` - if no proposal exists for this escrow
    /// * `EmergencyRescueAlreadyExecuted` - if the rescue has already been executed
    pub fn approve_emergency_rescue(
        env: Env,
        escrow_id: u64,
        approver: Address,
    ) -> Result<u32, EscrowError> {
        approver.require_auth();
        if !Self::is_admin(env.clone(), approver.clone()) {
            return Err(EscrowError::Unauthorized);
        }

        let proposal_key = DataKey::EmergencyRescueProposal(escrow_id);
        let mut proposal: EmergencyRescueProposal = env
            .storage()
            .persistent()
            .get(&proposal_key)
            .ok_or(EscrowError::EmergencyRescueProposalNotFound)?;

        if proposal.executed {
            return Err(EscrowError::EmergencyRescueAlreadyExecuted);
        }

        // Add approval if not already approved by this admin
        if !proposal.approvals.contains(&approver) {
            proposal.approvals.push_back(approver);
        }

        env.storage().persistent().set(&proposal_key, &proposal);
        Ok(proposal.approvals.len())
    }

    /// Execute an emergency rescue for a stranded escrow.
    ///
    /// Rescues funds from an escrow whose underlying token contract is broken or defunct.
    /// Requires:
    /// 1. A valid rescue proposal to exist
    /// 2. The 14-day timelock to have elapsed since proposal
    /// 3. Multi-sig approval threshold to be met
    /// 4. The escrow to still be in Funded or Disputed status
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment
    /// * `escrow_id` - The ID of the escrow to rescue
    /// * `caller` - Admin authorizing the rescue execution
    ///
    /// # Returns
    ///
    /// A `Result` containing `true` if the rescue was executed successfully.
    ///
    /// # Errors
    ///
    /// * `Unauthorized` - if caller is not an admin
    /// * `NotFound` - if the escrow does not exist
    /// * `EmergencyRescueProposalNotFound` - if no proposal exists
    /// * `EmergencyRescueTimelockNotElapsed` - if 14 days haven't passed
    /// * `EmergencyRescueThresholdNotMet` - if approval count is below threshold
    /// * `EmergencyRescueAlreadyExecuted` - if already executed
    /// * `EscrowNotEligibleForRescue` - if escrow is no longer eligible (terminal state)
    pub fn emergency_rescue_stalled_escrow(
        env: Env,
        escrow_id: u64,
        recovery_destination: Address,
        caller: Address,
    ) -> Result<(), EscrowError> {
        caller.require_auth();
        if !Self::is_admin(env.clone(), caller.clone()) {
            return Err(EscrowError::Unauthorized);
        }

        // Get the escrow
        let escrow_key = DataKey::Escrow(escrow_id);
        let mut record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&escrow_key)
            .ok_or(EscrowError::NotFound)?;

        // Check escrow is still eligible for rescue
        match record.status {
            EscrowStatus::Funded | EscrowStatus::Disputed => {}
            _ => return Err(EscrowError::EscrowNotEligibleForRescue),
        }

        // Get the proposal
        let proposal_key = DataKey::EmergencyRescueProposal(escrow_id);
        let mut proposal: EmergencyRescueProposal = env
            .storage()
            .persistent()
            .get(&proposal_key)
            .ok_or(EscrowError::EmergencyRescueProposalNotFound)?;

        if proposal.executed {
            return Err(EscrowError::EmergencyRescueAlreadyExecuted);
        }

        // Check timelock has elapsed (14 days = 1_209_600 seconds)
        let current_timestamp = env.ledger().timestamp();
        if current_timestamp < proposal.proposed_at + EMERGENCY_RESCUE_TIMELOCK_SECS {
            return Err(EscrowError::EmergencyRescueTimelockNotElapsed);
        }

        // Check multi-sig threshold is met
        let threshold = Self::upgrade_threshold(&env);
        if proposal.approvals.len() < threshold {
            return Err(EscrowError::EmergencyRescueThresholdNotMet);
        }

        // Verify recovery destination matches proposal
        if recovery_destination != proposal.recovery_destination {
            return Err(EscrowError::InvalidAddress);
        }

        // Calculate amount to recover
        let amount_to_recover = record.amount - record.released_amount - record.refunded_amount;

        // Mark as executed before attempting token transfer (prevent reentrancy)
        proposal.executed = true;
        env.storage().persistent().set(&proposal_key, &proposal);

        // Update escrow status to Refunded (to prevent further operations)
        record.status = EscrowStatus::Refunded;
        record.refunded_amount = record.amount;
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&escrow_key, &record);

        // Transfer funds directly to recovery destination
        let token_client = soroban_sdk::token::Client::new(&env, &record.token);
        token_client.transfer(
            &env.current_contract_address(),
            &recovery_destination,
            &amount_to_recover,
        );

        // Emit the rescue event
        env.events().publish(
            ("escrow", "emergency_rescued", escrow_id),
            EscrowEmergencyRescuedEvent {
                escrow_id,
                original_token: record.token,
                amount_recovered: amount_to_recover,
                recovery_destination,
                authorized_by: caller,
                rescued_at: env.ledger().timestamp(),
            },
        );

        Ok(())
    }

    /// Get the current emergency rescue proposal for an escrow, if it exists.
    ///
    /// Returns `None` if no proposal has been created yet.
    pub fn get_emergency_rescue_proposal(
        env: Env,
        escrow_id: u64,
    ) -> Option<EmergencyRescueProposal> {
        env.storage()
            .persistent()
            .get(&DataKey::EmergencyRescueProposal(escrow_id))
    }
}

/// Cancellation protection for created (unfunded) orders — issue #355.
///
/// A buyer broadcasts `deposit` for an accepted order while a seller
/// broadcasts `cancel` for the very same escrow. On a chain without mempool
/// visibility the contract cannot know which of the two lands first, so it
/// makes the unilateral `cancel` *lose* whenever it would land first: for a
/// window of ledgers after creation (and after a seller acceptance) the
/// seller may only cancel with the buyer's agreement on-chain or once the
/// escrow's own timeout has been reached. Once the buyer's deposit is
/// on-chain the escrow is `Funded` and the pre-existing `AlreadyFunded`
/// guard keeps the funds locked.
#[cfg(test)]
mod cancel_guard_tests {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Events, Ledger as _},
        token::StellarAssetClient,
        TryIntoVal,
    };

    const DEPOSIT: i128 = 1_000;
    const TIMEOUT: u32 = 10_000;
    const REASON: Symbol = symbol_short!("cancel");
    const FAR_FUTURE: u32 = 100_000;

    struct Setup<'a> {
        client: EscrowContractClient<'a>,
        contract: Address,
        admin: Address,
        buyer: Address,
        seller: Address,
        token: Address,
    }

    fn setup(env: &Env) -> Setup<'_> {
        env.mock_all_auths();
        let admin = Address::generate(env);
        let buyer = Address::generate(env);
        let seller = Address::generate(env);
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(env))
            .address();
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 250,
            treasury: Address::generate(env),
            min_amount: 100,
            max_amount: 1_000_000,
        };
        let contract = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(env, &contract);
        StellarAssetClient::new(env, &token).mint(&buyer, &10_000);
        client.add_token(&admin, &token);
        Setup {
            client,
            contract,
            admin,
            buyer,
            seller,
            token,
        }
    }

    fn order_id(env: &Env, byte: u8) -> BytesN<32> {
        BytesN::from_array(env, &[byte; 32])
    }

    /// Create an order without funding it — the only state a seller is ever
    /// allowed to cancel.
    fn create(env: &Env, s: &Setup, byte: u8) -> u64 {
        s.client.create(
            &s.buyer,
            &s.seller,
            &s.token,
            &DEPOSIT,
            &order_id(env, byte),
            &TIMEOUT,
            &None,
            &None,
        )
    }

    /// The buyer's funding submission — the transaction whose mempool entry a
    /// seller would try to get ahead of.
    fn fund(s: &Setup, escrow_id: u64) {
        assert!(s.client.fund(&escrow_id, &s.buyer));
        assert_eq!(s.client.get_escrow(&escrow_id).status, EscrowStatus::Funded);
    }

    fn reason_of(s: &Setup, escrow_id: u64) -> Symbol {
        s.client
            .get_cancel_eligibility(&escrow_id, &s.seller)
            .reason
    }

    fn eligible(s: &Setup, escrow_id: u64) -> bool {
        s.client
            .get_cancel_eligibility(&escrow_id, &s.seller)
            .eligible
    }

    fn has_event(env: &Env, action: &str) -> bool {
        env.events().all().iter().any(|(_, topics, data)| {
            if topics.len() != 3 {
                return false;
            }
            let topic: Symbol = topics.get(1).unwrap().try_into_val(env).unwrap();
            topic == Symbol::new(env, action) && !data.is_void()
        })
    }

    #[test]
    fn creation_snapshots_the_protection_window() {
        let env = Env::default();
        let s = setup(&env);
        let escrow_id = create(&env, &s, 1);

        let acceptance = s.client.get_order_acceptance(&escrow_id);
        assert!(!acceptance.seller_accepted);
        assert_eq!(acceptance.accepted_at_ledger, 0);
        assert_eq!(
            acceptance.cancel_lockout_ledgers,
            DEFAULT_CANCEL_LOCKOUT_LEDGERS
        );

        let eligibility = s.client.get_cancel_eligibility(&escrow_id, &s.seller);
        assert_eq!(eligibility.escrow_id, escrow_id);
        assert!(!eligibility.eligible);
        assert_eq!(eligibility.reason, symbol_short!("lockout"));
    }

    #[test]
    fn a_seller_cancel_inside_the_window_is_rejected() {
        let env = Env::default();
        let s = setup(&env);
        let escrow_id = create(&env, &s, 2);

        assert_eq!(
            s.client.try_cancel(&escrow_id, &s.seller, &REASON),
            Err(Ok(EscrowError::CancelLockoutActive))
        );
        // The rejection leaves no trace: the order is still open, so the
        // buyer can still fund it instead of losing it to a losing race.
        assert_eq!(
            s.client.get_escrow(&escrow_id).status,
            EscrowStatus::Created
        );
        assert_eq!(
            s.client
                .get_cancel_eligibility(&escrow_id, &s.seller)
                .reason,
            symbol_short!("lockout")
        );
    }

    #[test]
    fn a_rejected_cancel_does_not_block_the_buyer_deposit() {
        let env = Env::default();
        let s = setup(&env);
        let escrow_id = create(&env, &s, 27);

        // The seller's front-running attempt is rejected...
        assert_eq!(
            s.client.try_cancel(&escrow_id, &s.seller, &REASON),
            Err(Ok(EscrowError::CancelLockoutActive))
        );
        // ...and the buyer's own funding submission — the thing the guard
        // exists to protect — still lands.
        fund(&s, escrow_id);

        // Only the escrow's buyer may fund it.
        assert_eq!(
            s.client.try_fund(&escrow_id, &s.seller),
            Err(Ok(EscrowError::Unauthorized))
        );
    }

    #[test]
    fn only_the_seller_may_cancel() {
        let env = Env::default();
        let s = setup(&env);
        let escrow_id = create(&env, &s, 3);
        env.ledger()
            .set_sequence_number(DEFAULT_CANCEL_LOCKOUT_LEDGERS);

        // The buyer cannot cancel the seller's order even after the window
        // elapsed — that is what makes the buyer's on-chain agreement
        // meaningful instead of redundant.
        assert_eq!(
            s.client.try_cancel(&escrow_id, &s.buyer, &REASON),
            Err(Ok(EscrowError::Unauthorized))
        );
        assert_eq!(
            s.client
                .try_cancel(&escrow_id, &Address::generate(&env), &REASON),
            Err(Ok(EscrowError::Unauthorized))
        );
        assert_eq!(
            s.client.get_cancel_eligibility(&escrow_id, &s.buyer).reason,
            symbol_short!("notseller")
        );
    }

    #[test]
    fn the_window_expires_at_the_configured_ledger() {
        let env = Env::default();
        let s = setup(&env);
        let escrow_id = create(&env, &s, 4);

        // One ledger short of the deadline the window is still running.
        env.ledger()
            .set_sequence_number(DEFAULT_CANCEL_LOCKOUT_LEDGERS - 1);
        assert_eq!(
            s.client.try_cancel(&escrow_id, &s.seller, &REASON),
            Err(Ok(EscrowError::CancelLockoutActive))
        );

        // The first unprotected ledger is cancellable again.
        env.ledger()
            .set_sequence_number(DEFAULT_CANCEL_LOCKOUT_LEDGERS);
        assert_eq!(reason_of(&s, escrow_id), symbol_short!("ok"));
        assert!(s.client.cancel(&escrow_id, &s.seller, &REASON));
        assert_eq!(
            s.client.get_escrow(&escrow_id).status,
            EscrowStatus::Cancelled
        );
        assert_eq!(
            s.client.try_cancel(&escrow_id, &s.seller, &REASON),
            Err(Ok(EscrowError::AlreadyCancelled))
        );
        assert_eq!(reason_of(&s, escrow_id), symbol_short!("cancelled"));
    }

    #[test]
    fn the_buyer_can_waive_the_window() {
        let env = Env::default();
        let s = setup(&env);
        let escrow_id = create(&env, &s, 5);

        assert!(s.client.agree_cancel(&escrow_id, &s.buyer));
        assert!(has_event(&env, "agreed"));
        assert!(eligible(&s, escrow_id));
        assert_eq!(reason_of(&s, escrow_id), symbol_short!("agreed"));

        // Inside the nominal window, but explicitly agreed to.
        assert!(s.client.cancel(&escrow_id, &s.seller, &REASON));
        assert_eq!(
            s.client.get_escrow(&escrow_id).status,
            EscrowStatus::Cancelled
        );
    }

    #[test]
    fn only_the_buyer_may_agree_to_a_cancellation() {
        let env = Env::default();
        let s = setup(&env);
        let escrow_id = create(&env, &s, 6);

        assert_eq!(
            s.client.try_agree_cancel(&escrow_id, &s.seller),
            Err(Ok(EscrowError::Unauthorized))
        );
        assert_eq!(
            s.client
                .try_agree_cancel(&escrow_id, &Address::generate(&env)),
            Err(Ok(EscrowError::Unauthorized))
        );
        // A seller cannot manufacture the buyer's consent, so the window
        // still applies.
        assert_eq!(reason_of(&s, escrow_id), symbol_short!("lockout"));
    }

    #[test]
    fn agreement_is_only_valid_while_the_order_is_unfunded() {
        let env = Env::default();
        let s = setup(&env);
        let funded = create(&env, &s, 7);
        fund(&s, funded);

        assert_eq!(
            s.client.try_agree_cancel(&funded, &s.buyer),
            Err(Ok(EscrowError::InvalidStatus))
        );
    }

    #[test]
    fn a_deposit_cannot_be_front_run_by_a_seller_cancel() {
        let env = Env::default();
        let s = setup(&env);
        let escrow_id = create(&env, &s, 8);

        // Legitimate order: the buyer funds the order it accepted.
        fund(&s, escrow_id);

        // Attack: the seller's cancel is submitted in the very first ledger,
        // where it would have won the race before this fix. It must lose
        // regardless of submission order, and the funds stay in escrow.
        assert_eq!(
            s.client.try_cancel(&escrow_id, &s.seller, &REASON),
            Err(Ok(EscrowError::AlreadyFunded))
        );
        assert_eq!(reason_of(&s, escrow_id), symbol_short!("funded"));
        let record = s.client.get_escrow(&escrow_id);
        assert_eq!(record.status, EscrowStatus::Funded);
        assert_eq!(record.amount, DEPOSIT);
    }

    #[test]
    fn a_settled_escrow_can_never_be_cancelled() {
        let env = Env::default();
        let s = setup(&env);
        let escrow_id = create(&env, &s, 9);
        s.client.accept_order(&escrow_id, &s.seller);
        fund(&s, escrow_id);
        env.ledger().set_sequence_number(FAR_FUTURE);

        assert_eq!(
            s.client.try_cancel(&escrow_id, &s.seller, &REASON),
            Err(Ok(EscrowError::AlreadyFunded))
        );
    }

    #[test]
    fn a_losing_cancel_cannot_be_retried_after_the_window() {
        let env = Env::default();
        let s = setup(&env);
        let escrow_id = create(&env, &s, 10);

        // The seller tries to front-run and is blocked...
        assert_eq!(
            s.client.try_cancel(&escrow_id, &s.seller, &REASON),
            Err(Ok(EscrowError::CancelLockoutActive))
        );

        // ...and the window is short enough that a seller who was never
        // front-running keeps its ability to clean up a stalled order.
        env.ledger()
            .set_sequence_number(DEFAULT_CANCEL_LOCKOUT_LEDGERS);
        assert!(s.client.cancel(&escrow_id, &s.seller, &REASON));
    }

    #[test]
    fn acceptance_is_seller_only() {
        let env = Env::default();
        let s = setup(&env);
        let escrow_id = create(&env, &s, 11);

        assert!(s.client.accept_order(&escrow_id, &s.seller));
        assert!(has_event(&env, "accepted"));
        let acceptance = s.client.get_order_acceptance(&escrow_id);
        assert!(acceptance.seller_accepted);
        assert_eq!(acceptance.accepted_at_ledger, env.ledger().sequence());

        assert_eq!(
            s.client.try_accept_order(&escrow_id, &s.buyer),
            Err(Ok(EscrowError::Unauthorized))
        );
        assert_eq!(
            s.client
                .try_accept_order(&escrow_id, &Address::generate(&env)),
            Err(Ok(EscrowError::Unauthorized))
        );
    }

    #[test]
    fn acceptance_re_anchors_the_protection_window() {
        let env = Env::default();
        let s = setup(&env);
        let escrow_id = create(&env, &s, 12);

        // The seller accepts the order some ledgers after it was created; the
        // buyer is then guaranteed a full window to fund the order it just
        // saw accepted.
        env.ledger().set_sequence_number(4);
        s.client.accept_order(&escrow_id, &s.seller);

        let now = env.ledger().sequence();
        assert_eq!(
            s.client
                .get_cancel_eligibility(&escrow_id, &s.seller)
                .reason,
            symbol_short!("lockout")
        );
        assert_eq!(
            s.client.try_cancel(&escrow_id, &s.seller, &REASON),
            Err(Ok(EscrowError::CancelLockoutActive))
        );

        // Four ledgers past the original deadline, but inside the re-anchored
        // one, the buyer's deposit is still safe.
        env.ledger()
            .set_sequence_number(now + DEFAULT_CANCEL_LOCKOUT_LEDGERS - 1);
        assert_eq!(
            s.client.try_cancel(&escrow_id, &s.seller, &REASON),
            Err(Ok(EscrowError::CancelLockoutActive))
        );

        env.ledger()
            .set_sequence_number(now + DEFAULT_CANCEL_LOCKOUT_LEDGERS);
        assert!(s.client.cancel(&escrow_id, &s.seller, &REASON));
    }

    #[test]
    fn acceptance_is_only_valid_while_the_order_is_open() {
        let env = Env::default();
        let s = setup(&env);
        let cancelled = create(&env, &s, 13);
        let funded = create(&env, &s, 14);
        env.ledger()
            .set_sequence_number(DEFAULT_CANCEL_LOCKOUT_LEDGERS);
        s.client.cancel(&cancelled, &s.seller, &REASON);
        fund(&s, funded);

        assert_eq!(
            s.client.try_accept_order(&cancelled, &s.seller),
            Err(Ok(EscrowError::InvalidStatus))
        );
        assert_eq!(
            s.client.try_accept_order(&funded, &s.seller),
            Err(Ok(EscrowError::InvalidStatus))
        );
    }

    #[test]
    fn the_order_timeout_waives_the_window() {
        let env = Env::default();
        let s = setup(&env);
        let escrow_id = create(&env, &s, 15);
        let timeout_ledger = s.client.get_escrow(&escrow_id).timeout_ledger;

        // The timeout is reached long before the window would have expired.
        env.ledger().set_sequence_number(timeout_ledger);
        assert!(eligible(&s, escrow_id));
        assert_eq!(reason_of(&s, escrow_id), symbol_short!("timeout"));
        assert!(s.client.cancel(&escrow_id, &s.seller, &REASON));
    }

    #[test]
    fn the_admin_configures_the_window() {
        let env = Env::default();
        let s = setup(&env);
        let escrow_id = create(&env, &s, 16);

        assert_eq!(
            s.client.get_cancel_lockout(),
            DEFAULT_CANCEL_LOCKOUT_LEDGERS
        );
        assert_eq!(
            s.client
                .get_order_acceptance(&escrow_id)
                .cancel_lockout_ledgers,
            DEFAULT_CANCEL_LOCKOUT_LEDGERS
        );

        s.client.set_cancel_lockout(&s.admin, &7);
        assert_eq!(s.client.get_cancel_lockout(), 7);
        // An order that is already live keeps the window it was created with.
        assert_eq!(
            s.client
                .get_order_acceptance(&escrow_id)
                .cancel_lockout_ledgers,
            DEFAULT_CANCEL_LOCKOUT_LEDGERS
        );

        s.client
            .set_cancel_lockout(&s.admin, &MAX_CANCEL_LOCKOUT_LEDGERS);
        assert_eq!(s.client.get_cancel_lockout(), MAX_CANCEL_LOCKOUT_LEDGERS);

        s.client.set_cancel_lockout(&s.admin, &0);
        assert_eq!(s.client.get_cancel_lockout(), 0);

        assert_eq!(
            s.client.try_set_cancel_lockout(&s.seller, &5),
            Err(Ok(EscrowError::Unauthorized))
        );
        assert_eq!(
            s.client.try_set_cancel_lockout(&s.buyer, &5),
            Err(Ok(EscrowError::Unauthorized))
        );
        assert_eq!(
            s.client
                .try_set_cancel_lockout(&s.admin, &(MAX_CANCEL_LOCKOUT_LEDGERS + 1)),
            Err(Ok(EscrowError::InvalidCancelLockout))
        );
        // Rejected values must not be persisted.
        assert_eq!(s.client.get_cancel_lockout(), 0);
    }

    #[test]
    fn the_window_is_snapshotted_per_escrow() {
        let env = Env::default();
        let s = setup(&env);
        let early = create(&env, &s, 17);
        s.client
            .set_cancel_lockout(&s.admin, &MAX_CANCEL_LOCKOUT_LEDGERS);
        let late = create(&env, &s, 18);
        s.client.set_cancel_lockout(&s.admin, &3);

        // A later configuration change must not retroactively alter the
        // protection of orders that are already live.
        assert_eq!(
            s.client.get_order_acceptance(&early).cancel_lockout_ledgers,
            DEFAULT_CANCEL_LOCKOUT_LEDGERS
        );
        assert_eq!(
            s.client.get_order_acceptance(&late).cancel_lockout_ledgers,
            MAX_CANCEL_LOCKOUT_LEDGERS
        );

        env.ledger()
            .set_sequence_number(DEFAULT_CANCEL_LOCKOUT_LEDGERS);
        assert!(s.client.cancel(&early, &s.seller, &REASON));
        assert_eq!(
            s.client.try_cancel(&late, &s.seller, &REASON),
            Err(Ok(EscrowError::CancelLockoutActive))
        );
    }

    #[test]
    fn a_zero_window_leaves_cancels_unilateral() {
        let env = Env::default();
        let s = setup(&env);
        s.client.set_cancel_lockout(&s.admin, &0);
        let escrow_id = create(&env, &s, 19);

        assert!(eligible(&s, escrow_id));
        assert_eq!(reason_of(&s, escrow_id), symbol_short!("ok"));
        assert!(s.client.cancel(&escrow_id, &s.seller, &REASON));
    }

    #[test]
    fn the_guard_never_agrees_with_itself() {
        let env = Env::default();
        let s = setup(&env);
        let fresh = create(&env, &s, 20);
        let funded = create(&env, &s, 21);
        let agreed = create(&env, &s, 22);
        fund(&s, funded);
        s.client.agree_cancel(&agreed, &s.buyer);

        // The read-only view and the state-changing call must agree on every
        // outcome an order can be in while the seller is watching it.
        for (escrow_id, can_cancel) in [(fresh, false), (funded, false), (agreed, true)] {
            let eligibility = s.client.get_cancel_eligibility(&escrow_id, &s.seller);
            assert_eq!(eligibility.escrow_id, escrow_id);
            assert_eq!(
                eligibility.eligible, can_cancel,
                "eligibility for {escrow_id}"
            );
            let result = s.client.try_cancel(&escrow_id, &s.seller, &REASON);
            assert_eq!(result.is_ok(), can_cancel, "cancel for {escrow_id}");
        }

        // Once the window elapses the same order is cancellable, and the view
        // follows it into the terminal state.
        env.ledger()
            .set_sequence_number(DEFAULT_CANCEL_LOCKOUT_LEDGERS);
        assert!(s.client.cancel(&fresh, &s.seller, &REASON));
        let eligibility = s.client.get_cancel_eligibility(&fresh, &s.seller);
        assert!(!eligibility.eligible);
        assert_eq!(eligibility.reason, symbol_short!("cancelled"));
    }

    #[test]
    fn a_missing_snapshot_fails_closed() {
        let env = Env::default();
        let s = setup(&env);
        let escrow_id = create(&env, &s, 24);

        // Simulate a legacy escrow (or an evicted snapshot) whose protection
        // state cannot be read.
        env.as_contract(&s.contract, || {
            env.storage()
                .persistent()
                .remove(&DataKey::OrderAcceptance(escrow_id));
            env.storage()
                .persistent()
                .remove(&DataKey::CancelLockoutLedger(escrow_id));
        });

        assert_eq!(reason_of(&s, escrow_id), symbol_short!("lockout"));
        assert_eq!(
            s.client.try_cancel(&escrow_id, &s.seller, &REASON),
            Err(Ok(EscrowError::CancelLockoutActive))
        );

        // The buyer's agreement is still honoured, so a missing snapshot can
        // never trap the escrow.
        s.client.agree_cancel(&escrow_id, &s.buyer);
        assert!(s.client.cancel(&escrow_id, &s.seller, &REASON));
    }

    #[test]
    fn accepting_cannot_open_a_missing_snapshot() {
        let env = Env::default();
        let s = setup(&env);
        let escrow_id = create(&env, &s, 26);
        env.as_contract(&s.contract, || {
            env.storage()
                .persistent()
                .remove(&DataKey::CancelLockoutLedger(escrow_id));
        });

        // Acceptance records the acknowledgement but must not hand the seller a
        // window the escrow never had.
        assert!(s.client.accept_order(&escrow_id, &s.seller));
        assert!(s.client.get_order_acceptance(&escrow_id).seller_accepted);
        env.ledger()
            .set_sequence_number(DEFAULT_CANCEL_LOCKOUT_LEDGERS);
        assert_eq!(reason_of(&s, escrow_id), symbol_short!("lockout"));
        assert_eq!(
            s.client.try_cancel(&escrow_id, &s.seller, &REASON),
            Err(Ok(EscrowError::CancelLockoutActive))
        );
    }

    #[test]
    fn unknown_escrows_are_rejected() {
        let env = Env::default();
        let s = setup(&env);
        let missing = 9_999u64;

        assert_eq!(
            s.client.try_accept_order(&missing, &s.seller),
            Err(Ok(EscrowError::NotFound))
        );
        assert_eq!(
            s.client.try_agree_cancel(&missing, &s.buyer),
            Err(Ok(EscrowError::NotFound))
        );
        assert_eq!(
            s.client.try_get_order_acceptance(&missing),
            Err(Ok(EscrowError::NotFound))
        );
        assert_eq!(
            s.client.try_cancel(&missing, &s.seller, &REASON),
            Err(Ok(EscrowError::NotFound))
        );
        let eligibility = s.client.get_cancel_eligibility(&missing, &s.seller);
        assert!(!eligibility.eligible);
        assert_eq!(eligibility.reason, symbol_short!("notfound"));
    }
}

#[cfg(test)]
mod escrow_feature_tests {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Events},
        token::StellarAssetClient,
        TryIntoVal,
    };

    fn setup(
        env: &Env,
    ) -> (
        EscrowContractClient<'_>,
        Address,
        Address,
        Address,
        Address,
        Address,
    ) {
        soroban_sdk::testutils::Ledger::with_mut(&env.ledger(), |l| {
            l.min_persistent_entry_ttl = PERSISTENT_BUMP_AMOUNT;
        });
        env.mock_all_auths();
        let admin = Address::generate(env);
        let treasury = Address::generate(env);
        let buyer = Address::generate(env);
        let seller = Address::generate(env);
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(env))
            .address();
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 250,
            treasury,
            min_amount: 100,
            max_amount: 1_000_000,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(env, &contract_id);
        StellarAssetClient::new(env, &token).mint(&buyer, &10_000);
        admin_actions_test::approve(&env, &client, &admin, AdminAction::AddToken(token.clone()));
        (client, admin, buyer, seller, token, contract_id)
    }

    fn order_leaf(env: &Env, order_id: &BytesN<32>) -> BytesN<32> {
        env.crypto()
            .sha256(&Bytes::from_array(env, &order_id.to_array()))
            .into()
    }

    #[test]
    fn published_root_releases_only_the_committed_order() {
        let env = Env::default();
        let (client, admin, buyer, seller, token, _) = setup(&env);
        let order_id = BytesN::from_array(&env, &[91; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000, &order_id, &1_000, &None, &None,
        );
        let date = 20u64;
        let leaf = order_leaf(&env, &order_id);
        client.publish_merkle_root(&admin, &date, &leaf);
        let proof = MerkleDeliveryProof {
            root: leaf.clone(),
            leaf,
            proof: Vec::new(&env),
            index: 0,
        };

        assert_eq!(
            client.try_publish_merkle_root(&admin, &date, &proof.root),
            Err(Ok(EscrowError::AlreadyInitialized))
        );
        assert!(client.release_with_merkle_proof(&escrow_id, &buyer, &date, &proof));
        assert_eq!(client.get_escrow(&escrow_id).status, EscrowStatus::Released);
    }

    #[test]
    fn merkle_path_uses_index_to_order_siblings() {
        let env = Env::default();
        let left = BytesN::from_array(&env, &[1; 32]);
        let right = BytesN::from_array(&env, &[2; 32]);
        let mut node = Bytes::new(&env);
        node.append(&Bytes::from_array(&env, &left.to_array()));
        node.append(&Bytes::from_array(&env, &right.to_array()));
        let root: BytesN<32> = env.crypto().sha256(&node).into();
        let proof = MerkleDeliveryProof {
            root,
            leaf: right,
            proof: Vec::from_array(&env, [left]),
            index: 1,
        };

        assert!(EscrowContract::verify_merkle_path(&env, &proof));
        let mut invalid = proof;
        invalid.index = 0;
        assert!(!EscrowContract::verify_merkle_path(&env, &invalid));
    }

    #[test]
    fn merkle_release_rejects_a_leaf_for_another_order() {
        let env = Env::default();
        let (client, admin, buyer, seller, token, _) = setup(&env);
        let order_id = BytesN::from_array(&env, &[11; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000, &order_id, &1_000, &None, &None,
        );
        let date = 21u64;
        let committed = order_leaf(&env, &order_id);
        client.publish_merkle_root(&admin, &date, &committed);
        let proof = MerkleDeliveryProof {
            root: committed,
            leaf: BytesN::from_array(&env, &[12; 32]),
            proof: Vec::new(&env),
            index: 0,
        };

        assert_eq!(
            client.try_release_with_merkle_proof(&escrow_id, &buyer, &date, &proof),
            Err(Ok(EscrowError::InvalidMerkleProof))
        );
        assert_eq!(client.get_escrow(&escrow_id).status, EscrowStatus::Funded);
    }

    #[test]
    fn merkle_release_settles_only_the_unrefunded_balance() {
        let env = Env::default();
        let (client, admin, buyer, seller, token, contract_id) = setup(&env);
        let token_client = soroban_sdk::token::Client::new(&env, &token);
        let order_id = BytesN::from_array(&env, &[13; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000, &order_id, &1_000, &None, &None,
        );
        client.partial_refund(&escrow_id, &seller, &200);
        let date = 22u64;
        let leaf = order_leaf(&env, &order_id);
        client.publish_merkle_root(&admin, &date, &leaf);
        let proof = MerkleDeliveryProof {
            root: leaf.clone(),
            leaf,
            proof: Vec::new(&env),
            index: 0,
        };

        assert!(client.release_with_merkle_proof(&escrow_id, &buyer, &date, &proof));
        let record = client.get_escrow(&escrow_id);
        assert_eq!(record.released_amount, 800);
        assert_eq!(record.refunded_amount, 200);
        assert_eq!(record.status, EscrowStatus::Released);
        assert_eq!(token_client.balance(&contract_id), 0);
    }

    #[test]
    fn batch_escrows_pull_aggregated_token_allowance_once() {
        let env = Env::default();
        let (client, _admin, buyer, seller, token, contract_id) = setup(&env);
        let token_client = soroban_sdk::token::Client::new(&env, &token);
        let timeout = env.ledger().sequence() + 500;
        token_client.approve(&buyer, &contract_id, &2_500, &(timeout + 100));
        let items = Vec::from_array(
            &env,
            [
                BatchEscrowItem {
                    seller: seller.clone(),
                    token: token.clone(),
                    amount: 1_000,
                    order_id: BytesN::from_array(&env, &[31; 32]),
                    timeout_ledger: timeout,
                },
                BatchEscrowItem {
                    seller,
                    token: token.clone(),
                    amount: 1_500,
                    order_id: BytesN::from_array(&env, &[32; 32]),
                    timeout_ledger: timeout,
                },
            ],
        );

        let ids = client.batch_create_escrows(&buyer, &items);
        assert_eq!(ids.len(), 2);
        assert_eq!(token_client.balance(&buyer), 7_500);
        assert_eq!(token_client.balance(&contract_id), 2_500);
        assert_eq!(
            client.get_escrow(&ids.get(0).unwrap()).timeout_ledger,
            timeout
        );
        assert_eq!(
            client.get_escrow(&ids.get(1).unwrap()).status,
            EscrowStatus::Funded
        );
    }

    #[test]
    fn batch_escrow_allowance_failure_reverts_all_items() {
        let env = Env::default();
        let (client, admin, buyer, seller, token, contract_id) = setup(&env);
        let token_admin = Address::generate(&env);
        let second_token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let second_admin = soroban_sdk::token::StellarAssetClient::new(&env, &second_token);
        second_admin.mint(&buyer, &10_000);
        admin_actions_test::approve(
            &env,
            &client,
            &admin,
            AdminAction::AddToken(second_token.clone()),
        );

        let token_client = soroban_sdk::token::Client::new(&env, &token);
        let second_client = soroban_sdk::token::Client::new(&env, &second_token);
        let timeout = env.ledger().sequence() + 500;
        token_client.approve(&buyer, &contract_id, &1_000, &(timeout + 100));
        let items = Vec::from_array(
            &env,
            [
                BatchEscrowItem {
                    seller: seller.clone(),
                    token: token.clone(),
                    amount: 1_000,
                    order_id: BytesN::from_array(&env, &[41; 32]),
                    timeout_ledger: timeout,
                },
                BatchEscrowItem {
                    seller,
                    token: second_token.clone(),
                    amount: 1_000,
                    order_id: BytesN::from_array(&env, &[42; 32]),
                    timeout_ledger: timeout,
                },
            ],
        );

        assert!(client.try_batch_create_escrows(&buyer, &items).is_err());
        assert!(client.try_get_escrow(&1).is_err());
        assert_eq!(token_client.balance(&buyer), 10_000);
        assert_eq!(second_client.balance(&buyer), 10_000);
        assert_eq!(token_client.balance(&contract_id), 0);
        assert_eq!(second_client.balance(&contract_id), 0);
    }

    #[test]
    fn split_dispute_pays_all_award_recipients_and_emits_event() {
        let env = Env::default();
        let (client, admin, buyer, seller, token, contract_id) = setup(&env);
        let mediator = Address::generate(&env);
        let token_client = soroban_sdk::token::Client::new(&env, &token);
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &1_000,
            &BytesN::from_array(&env, &[51; 32]),
            &1_000,
            &None,
            &None,
        );
        client.dispute(&escrow_id, &buyer);

        let award = DisputeResolutionAward {
            buyer_amount: 700,
            seller_amount: 250,
            mediator_fee: 50,
            mediator_address: mediator.clone(),
        };
        assert!(client.resolve_dispute_split(&escrow_id, &admin, &award));

        let events = env.events().all();
        let mut found_event = false;
        for (event_contract, topics, data) in events.iter() {
            if event_contract != contract_id || topics.len() != 3 {
                continue;
            }
            let topic: Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if topic == symbol_short!("dispsplit") {
                let event: DisputeResolvedEvent = data.try_into_val(&env).unwrap();
                assert_eq!(event.escrow_id, escrow_id);
                assert_eq!(event.buyer_amount, 700);
                assert_eq!(event.seller_amount, 250);
                assert_eq!(event.mediator_fee, 50);
                assert_eq!(event.mediator_address, mediator);
                found_event = true;
            }
        }
        assert!(found_event);

        assert_eq!(token_client.balance(&buyer), 9_700);
        assert_eq!(token_client.balance(&seller), 250);
        assert_eq!(token_client.balance(&mediator), 50);
        assert_eq!(token_client.balance(&contract_id), 0);
        let record = client.get_escrow(&escrow_id);
        assert_eq!(record.status, EscrowStatus::Released);
        assert_eq!(record.released_amount, 300);
        assert_eq!(record.refunded_amount, 700);
    }

    #[test]
    fn split_dispute_rejects_awards_that_do_not_match_escrow_amount() {
        let env = Env::default();
        let (client, admin, buyer, seller, token, contract_id) = setup(&env);
        let token_client = soroban_sdk::token::Client::new(&env, &token);
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &1_000,
            &BytesN::from_array(&env, &[52; 32]),
            &1_000,
            &None,
            &None,
        );
        client.dispute(&escrow_id, &buyer);
        let award = DisputeResolutionAward {
            buyer_amount: 700,
            seller_amount: 250,
            mediator_fee: 49,
            mediator_address: Address::generate(&env),
        };

        assert_eq!(
            client.try_resolve_dispute_split(&escrow_id, &admin, &award),
            Err(Ok(EscrowError::InvalidDisputeAward))
        );
        assert_eq!(client.get_escrow(&escrow_id).status, EscrowStatus::Disputed);
        assert_eq!(token_client.balance(&contract_id), 1_000);
    }
}

#[cfg(test)]
mod fee_distribution_tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn setup(env: &Env) -> (EscrowContractClient<'_>, Address, Address) {
        soroban_sdk::testutils::Ledger::with_mut(&env.ledger(), |l| {
            l.min_persistent_entry_ttl = PERSISTENT_BUMP_AMOUNT;
        });
        let admin = Address::generate(env);
        let treasury = Address::generate(env);
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 250u32,
            treasury: treasury.clone(),
            min_amount: 100i128,
            max_amount: 1_000_000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(env, &contract_id);
        env.mock_all_auths();
        (client, admin, contract_id)
    }

    #[test]
    fn rejects_zero_address_treasury() {
        let env = Env::default();
        let (client, admin, _contract_id) = setup(&env);
        let shares = soroban_sdk::vec![
            &env,
            TreasuryShare {
                treasury: Address::from_str(&env, ZERO_ACCOUNT_STRKEY),
                bps: 100,
            }
        ];

        let res = client.try_set_fee_distribution(&admin, &shares);
        assert_eq!(res, Err(Ok(EscrowError::InvalidAddress)));
    }

    #[test]
    fn rejects_zero_bps_share() {
        let env = Env::default();
        let (client, admin, _contract_id) = setup(&env);
        let treasury = Address::generate(&env);
        let shares = soroban_sdk::vec![&env, TreasuryShare { treasury, bps: 0 }];

        let res = client.try_set_fee_distribution(&admin, &shares);
        assert_eq!(res, Err(Ok(EscrowError::InvalidFeeBps)));
    }

    #[test]
    fn rejects_too_many_treasuries() {
        let env = Env::default();
        let (client, admin, _contract_id) = setup(&env);
        let mut shares = Vec::new(&env);
        for _ in 0..=MAX_TREASURIES {
            shares.push_back(TreasuryShare {
                treasury: Address::generate(&env),
                bps: 1,
            });
        }

        let res = client.try_set_fee_distribution(&admin, &shares);
        assert_eq!(res, Err(Ok(EscrowError::InvalidLimits)));
    }

    #[test]
    fn accepts_multi_treasury_distribution() {
        let env = Env::default();
        let (client, admin, _contract_id) = setup(&env);
        let treasury1 = Address::generate(&env);
        let treasury2 = Address::generate(&env);
        let shares = soroban_sdk::vec![
            &env,
            TreasuryShare {
                treasury: treasury1.clone(),
                bps: 300,
            },
            TreasuryShare {
                treasury: treasury2.clone(),
                bps: 200,
            },
        ];

        admin_actions_test::approve(
            &env,
            &client,
            &admin,
            AdminAction::FeeDistribution(shares.clone()),
        );
        assert_eq!(client.get_fee_distribution(), shares);
    }
}

// ── Issue #328: dynamic platform fee tiering by merchant volume ────────────
#[cfg(test)]
mod fee_tier_tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Events};
    use soroban_sdk::token::{StellarAssetClient, TokenClient};
    use soroban_sdk::TryIntoVal;

    fn setup(
        env: &Env,
    ) -> (
        EscrowContractClient<'_>,
        Address,
        Address,
        Address,
        Address,
        Address,
    ) {
        env.mock_all_auths();
        let admin = Address::generate(env);
        let treasury = Address::generate(env);
        let buyer = Address::generate(env);
        let seller = Address::generate(env);
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(env))
            .address();
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 250,
            treasury,
            min_amount: 100,
            max_amount: 1_000_000,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(env, &contract_id);
        StellarAssetClient::new(env, &token).mint(&buyer, &1_000_000);
        client.add_token(&admin, &token);
        (client, admin, buyer, seller, token, contract_id)
    }

    fn tier(min_volume: i128, fee_bps: u32) -> FeeTier {
        FeeTier {
            min_volume,
            fee_bps,
        }
    }

    fn deposit_and_release(
        env: &Env,
        client: &EscrowContractClient<'_>,
        buyer: &Address,
        seller: &Address,
        token: &Address,
        amount: i128,
        seed: u8,
    ) -> u64 {
        let order_id = BytesN::from_array(env, &[seed; 32]);
        let escrow_id = client.deposit(
            buyer, seller, token, &amount, &order_id, &1_000, &None, &None,
        );
        client.release(&escrow_id, buyer, seller);
        escrow_id
    }

    fn count_tier_up_events(env: &Env, contract_id: &Address, seller: &Address) -> u32 {
        let mut count = 0u32;
        for event in env.events().all().iter() {
            let (c_id, topics, _value) = event;
            if c_id != *contract_id || topics.len() != 3 {
                continue;
            }
            let t0: Symbol = topics.get(0).unwrap().try_into_val(env).unwrap();
            let t1: Symbol = topics.get(1).unwrap().try_into_val(env).unwrap();
            if t0 == symbol_short!("escrow") && t1 == symbol_short!("tier_up") {
                let merchant: Address = topics.get(2).unwrap().try_into_val(env).unwrap();
                if merchant == *seller {
                    count += 1;
                }
            }
        }
        count
    }

    #[test]
    fn base_fee_applies_when_no_tiers_configured() {
        let env = Env::default();
        let (client, admin, buyer, seller, token, _) = setup(&env);
        let _ = admin;
        let token_client = TokenClient::new(&env, &token);

        deposit_and_release(&env, &client, &buyer, &seller, &token, 1_000, 1);

        // 250 bps of 1000 = 25 fee; seller nets 975.
        assert_eq!(token_client.balance(&seller), 975);
        assert_eq!(client.get_merchant_settled_volume(&seller), 1_000);
        assert_eq!(client.get_effective_fee_bps(&seller), 250);
    }

    #[test]
    fn merchant_below_first_threshold_pays_base_fee() {
        let env = Env::default();
        let (client, admin, buyer, seller, token, _) = setup(&env);
        client.set_fee_tiers(&admin, &soroban_sdk::vec![&env, tier(5_000, 100)]);

        assert_eq!(client.get_effective_fee_bps(&seller), 250);
        assert_eq!(client.get_merchant_tier(&seller), None);
    }

    #[test]
    fn tier_discount_applies_after_crossing_threshold() {
        let env = Env::default();
        let (client, admin, buyer, seller, token, contract_id) = setup(&env);
        let token_client = TokenClient::new(&env, &token);
        client.set_fee_tiers(&admin, &soroban_sdk::vec![&env, tier(2_000, 100)]);

        // Release 1: fee charged at base 250 bps (tier evaluated before update).
        deposit_and_release(&env, &client, &buyer, &seller, &token, 1_000, 1);
        assert_eq!(token_client.balance(&seller), 975);
        assert_eq!(client.get_merchant_tier(&seller), None);

        // Release 2: fee still at base (volume was 1000 < 2000 at evaluation);
        // volume crosses 2000 after the update, so the tier event fires. It is
        // visible only in this invocation's event buffer.
        let order_id = BytesN::from_array(&env, &[2u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000, &order_id, &1_000, &None, &None,
        );
        client.release(&escrow_id, &buyer, &seller);
        // Read events before any further contract invocation: the event
        // buffer is cleared at the start of each invocation.
        assert_eq!(count_tier_up_events(&env, &contract_id, &seller), 1);
        assert_eq!(token_client.balance(&seller), 1_950);
        assert_eq!(client.get_merchant_tier(&seller), Some(tier(2_000, 100)));

        // Release 3: merchant now pays the discounted 100 bps; no further
        // tier event (already at the highest tier reached).
        deposit_and_release(&env, &client, &buyer, &seller, &token, 1_000, 3);
        assert_eq!(token_client.balance(&seller), 2_940); // +990
        assert_eq!(client.get_effective_fee_bps(&seller), 100);
        assert_eq!(count_tier_up_events(&env, &contract_id, &seller), 0);
    }

    #[test]
    fn partial_releases_accumulate_volume_and_fire_tier_event_once() {
        let env = Env::default();
        let (client, admin, buyer, seller, token, contract_id) = setup(&env);
        client.set_fee_tiers(&admin, &soroban_sdk::vec![&env, tier(1_500, 100)]);

        let order_id = BytesN::from_array(&env, &[9u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &2_000, &order_id, &1_000, &None, &None,
        );

        client.partial_release(&escrow_id, &buyer, &500);
        assert_eq!(client.get_merchant_settled_volume(&seller), 500);
        assert_eq!(client.get_merchant_tier(&seller), None);

        // Crosses the 1500 threshold on this release's post-update. Read the
        // event buffer before any further contract invocation clears it.
        client.partial_release(&escrow_id, &buyer, &1_100);
        assert_eq!(count_tier_up_events(&env, &contract_id, &seller), 1);
        assert_eq!(client.get_merchant_settled_volume(&seller), 1_600);
        assert_eq!(client.get_merchant_tier(&seller), Some(tier(1_500, 100)));

        // Finishing the escrow adds volume but fires no new tier event.
        client.partial_release(&escrow_id, &buyer, &400);
        assert_eq!(count_tier_up_events(&env, &contract_id, &seller), 0);
        assert_eq!(client.get_merchant_settled_volume(&seller), 2_000);
    }

    #[test]
    fn tier_event_carries_merchant_tier_and_volume() {
        let env = Env::default();
        let (client, admin, buyer, seller, token, contract_id) = setup(&env);
        client.set_fee_tiers(
            &admin,
            &soroban_sdk::vec![&env, tier(1_000, 200), tier(3_000, 100),],
        );

        deposit_and_release(&env, &client, &buyer, &seller, &token, 1_500, 1);

        let mut found: Option<MerchantVolumeTierUpdatedEvent> = None;
        for event in env.events().all().iter() {
            let (c_id, topics, value) = event;
            if c_id != contract_id || topics.len() != 3 {
                continue;
            }
            let t0: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == symbol_short!("escrow") && t1 == symbol_short!("tier_up") {
                found = Some(value.try_into_val(&env).unwrap());
            }
        }
        let evt = found.expect("tier_up event must be emitted");
        assert_eq!(evt.merchant, seller);
        assert_eq!(evt.min_volume, 1_000);
        assert_eq!(evt.fee_bps, 200);
        assert_eq!(evt.total_settled_volume, 1_500);
    }

    #[test]
    fn merchants_track_volume_independently() {
        let env = Env::default();
        let (client, admin, buyer, seller, token, _) = setup(&env);
        let other_seller = Address::generate(&env);
        client.set_fee_tiers(&admin, &soroban_sdk::vec![&env, tier(2_000, 100)]);

        deposit_and_release(&env, &client, &buyer, &seller, &token, 2_000, 1);
        deposit_and_release(&env, &client, &buyer, &other_seller, &token, 500, 2);

        assert_eq!(client.get_merchant_settled_volume(&seller), 2_000);
        assert_eq!(client.get_effective_fee_bps(&seller), 100);
        assert_eq!(client.get_merchant_settled_volume(&other_seller), 500);
        assert_eq!(client.get_effective_fee_bps(&other_seller), 250);
    }

    #[test]
    fn set_fee_tiers_validates_and_stores_sorted_input() {
        let env = Env::default();
        let (client, admin, _buyer, _seller, _token, _) = setup(&env);

        // Non-admin rejected.
        let intruder = Address::generate(&env);
        assert_eq!(
            client.try_set_fee_tiers(&intruder, &soroban_sdk::vec![&env, tier(1_000, 100)]),
            Err(Ok(EscrowError::Unauthorized))
        );

        // Zero bps rejected.
        assert_eq!(
            client.try_set_fee_tiers(&admin, &soroban_sdk::vec![&env, tier(1_000, 0)]),
            Err(Ok(EscrowError::InvalidTier))
        );

        // Above the 10% cap rejected.
        assert_eq!(
            client.try_set_fee_tiers(&admin, &soroban_sdk::vec![&env, tier(1_000, 1_001)]),
            Err(Ok(EscrowError::InvalidTier))
        );

        // Non-positive threshold rejected.
        assert_eq!(
            client.try_set_fee_tiers(&admin, &soroban_sdk::vec![&env, tier(0, 100)]),
            Err(Ok(EscrowError::InvalidTier))
        );

        // Duplicate/out-of-order thresholds rejected.
        assert_eq!(
            client.try_set_fee_tiers(
                &admin,
                &soroban_sdk::vec![&env, tier(1_000, 200), tier(1_000, 100)]
            ),
            Err(Ok(EscrowError::InvalidTier))
        );

        // More than MAX_FEE_TIERS rejected.
        let mut too_many = Vec::new(&env);
        for i in 0..=MAX_FEE_TIERS {
            too_many.push_back(tier(1_000 + 1_000 * i as i128, 100));
        }
        assert_eq!(
            client.try_set_fee_tiers(&admin, &too_many),
            Err(Ok(EscrowError::TierLimitExceeded))
        );

        // Valid configuration round-trips.
        let tiers = soroban_sdk::vec![&env, tier(1_000, 200), tier(3_000, 100)];
        assert!(client.set_fee_tiers(&admin, &tiers.clone()));
        assert_eq!(client.get_fee_tiers(), tiers);
    }

    #[test]
    fn remove_fee_tier_updates_effective_fee() {
        let env = Env::default();
        let (client, admin, buyer, seller, token, _) = setup(&env);
        client.set_fee_tiers(
            &admin,
            &soroban_sdk::vec![&env, tier(1_000, 200), tier(3_000, 100),],
        );

        deposit_and_release(&env, &client, &buyer, &seller, &token, 1_500, 1);
        assert_eq!(client.get_effective_fee_bps(&seller), 200);

        // Non-admin rejected; unknown tier rejected.
        assert_eq!(
            client.try_remove_fee_tier(&Address::generate(&env), &1_000),
            Err(Ok(EscrowError::Unauthorized))
        );
        assert_eq!(
            client.try_remove_fee_tier(&admin, &7_000),
            Err(Ok(EscrowError::TierNotFound))
        );

        assert!(client.remove_fee_tier(&admin, &1_000));
        assert_eq!(client.get_fee_tiers().len(), 1);

        // Merchant no longer meets the only remaining tier: base fee returns.
        assert_eq!(client.get_effective_fee_bps(&seller), 250);
    }
}

#[cfg(test)]
mod batch_flow_tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn batch_release_missing_escrow_returns_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 250u32,
            treasury: treasury.clone(),
            min_amount: 100i128,
            max_amount: 1_000_000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(&env, &contract_id);

        let caller = Address::generate(&env);
        let releases = soroban_sdk::vec![
            &env,
            BatchReleaseParams {
                escrow_id: 999,
                release_amount: 1,
            }
        ];

        assert_eq!(
            client.try_batch_release(&caller, &releases),
            Err(Ok(EscrowError::NotFound))
        );
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn setup(env: &Env) -> (EscrowContractClient<'_>, Address, Address) {
        soroban_sdk::testutils::Ledger::with_mut(&env.ledger(), |l| {
            l.min_persistent_entry_ttl = PERSISTENT_BUMP_AMOUNT;
        });
        let admin = Address::generate(env);
        let treasury = Address::generate(env);
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 250u32,
            treasury: treasury.clone(),
            min_amount: 100i128,
            max_amount: 1_000_000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(env, &contract_id);
        (client, admin, contract_id)
    }

    fn setup_with_token(env: &Env) -> (EscrowContractClient<'_>, Address, Address, Address) {
        let (client, admin, contract_id) = setup(env);
        env.mock_all_auths();
        let token_admin = Address::generate(env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        admin_actions_test::approve(&env, &client, &admin, AdminAction::AddToken(token.clone()));
        (client, admin, contract_id, token)
    }

    #[test]
    fn get_escrow_metadata_absent_escrow_returns_not_found() {
        let env = Env::default();
        let (client, _admin, _contract_id) = setup(&env);
        let result = client.try_get_escrow_metadata(&999u64);
        assert_eq!(result, Err(Ok(EscrowError::NotFound)));
    }

    #[test]
    fn get_escrow_metadata_existing_without_metadata_returns_metadata_not_set() {
        let env = Env::default();
        let (client, _admin, _contract_id, token) = setup_with_token(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let order_id = BytesN::from_array(&env, &[0u8; 32]);
        let no_hash: Option<BytesN<32>> = None;
        let no_schema: Option<Symbol> = None;
        let escrow_id = client.create(
            &buyer, &seller, &token, &100i128, &order_id, &1000u32, &no_hash, &no_schema,
        );
        let result = client.try_get_escrow_metadata(&escrow_id);
        assert_eq!(result, Err(Ok(EscrowError::MetadataNotSet)));
    }

    #[test]
    fn get_escrow_metadata_existing_with_metadata_returns_metadata() {
        let env = Env::default();
        let (client, _admin, _contract_id, token) = setup_with_token(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let order_id = BytesN::from_array(&env, &[7u8; 32]);
        let order_hash = BytesN::from_array(&env, &[1u8; 32]);
        let schema = Symbol::new(&env, "order_v1");
        let escrow_id = client.create(
            &buyer,
            &seller,
            &token,
            &100i128,
            &order_id,
            &1000u32,
            &Some(order_hash.clone()),
            &Some(schema.clone()),
        );
        let metadata = client.get_escrow_metadata(&escrow_id);
        assert_eq!(metadata.order_hash, order_hash);
        assert_eq!(metadata.schema, schema);
    }

    /// Issue #38: a batch entry with only one of order_hash/schema set must be
    /// rejected with a typed error, never silently persisted with stale
    /// metadata. Covers all four Option combinations.
    #[test]
    fn batch_deposit_rejects_half_set_metadata() {
        let env = Env::default();
        let (client, admin, _contract_id, token) = setup_with_token(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &1_000_000i128);

        let order_hash = BytesN::from_array(&env, &[38u8; 32]);
        let schema = Symbol::new(&env, "order_v1");

        // (Both None) — valid: no metadata, and the batch succeeds.
        let mut none_none = soroban_sdk::Vec::new(&env);
        none_none.push_back(BatchDepositParams {
            seller: seller.clone(),
            token: token.clone(),
            amount: 100i128,
            order_id: BytesN::from_array(&env, &[1u8; 32]),
            timeout_ledgers: 1000u32,
            order_hash: None,
            schema: None,
        });
        assert_eq!(client.batch_deposit(&buyer, &none_none).len(), 1);

        // (Both Some) — valid: full metadata stored.
        let mut some_some = soroban_sdk::Vec::new(&env);
        some_some.push_back(BatchDepositParams {
            seller: seller.clone(),
            token: token.clone(),
            amount: 100i128,
            order_id: BytesN::from_array(&env, &[2u8; 32]),
            timeout_ledgers: 1000u32,
            order_hash: Some(order_hash.clone()),
            schema: Some(schema.clone()),
        });
        assert_eq!(client.batch_deposit(&buyer, &some_some).len(), 1);

        // (Some hash only) — rejected with InvalidMetadata.
        let mut hash_only = soroban_sdk::Vec::new(&env);
        hash_only.push_back(BatchDepositParams {
            seller: seller.clone(),
            token: token.clone(),
            amount: 100i128,
            order_id: BytesN::from_array(&env, &[3u8; 32]),
            timeout_ledgers: 1000u32,
            order_hash: Some(order_hash.clone()),
            schema: None,
        });
        assert_eq!(
            client.try_batch_deposit(&buyer, &hash_only),
            Err(Ok(EscrowError::InvalidMetadata))
        );

        // (Schema only) — rejected with InvalidMetadata.
        let mut schema_only = soroban_sdk::Vec::new(&env);
        schema_only.push_back(BatchDepositParams {
            seller: seller.clone(),
            token: token.clone(),
            amount: 100i128,
            order_id: BytesN::from_array(&env, &[4u8; 32]),
            timeout_ledgers: 1000u32,
            order_hash: None,
            schema: Some(schema.clone()),
        });
        assert_eq!(
            client.try_batch_deposit(&buyer, &schema_only),
            Err(Ok(EscrowError::InvalidMetadata))
        );

        // Admin role is exercised only to keep the client bound; unused here.
        let _ = admin;
    }
}

#[cfg(test)]
mod path_slippage_tests {
    use super::*;
    use soroban_sdk::{
        symbol_short,
        testutils::{Address as _, Events},
        token::{StellarAssetClient, TokenClient},
        TryFromVal, TryIntoVal,
    };

    /// Mock DEX router used to drive the path-payment tests (issue #337).
    ///
    /// The mock prices every hop from a per-pool rate held on-chain, so a test
    /// can simulate a sandwich by moving the rate *after* the slippage window
    /// was committed. It spends exactly the committed `max_input_amount` — the
    /// ceiling the escrow authorises — and delivers the destination token to
    /// the caller (the escrow) before reporting the amounts it moved.
    mod mock_dex_router {
        use super::*;
        use soroban_sdk::{contract, contractimpl, contracttype, token::TokenClient};

        /// Storage keys for the mock router's manipulable pool state.
        #[contracttype]
        #[derive(Clone)]
        pub enum MockRouterKey {
            /// Destination units quoted per 10_000 input units by a pool.
            PoolRate(Address),
            /// Multiplier applied to the input the router reports it spent.
            InputSkew,
            /// Multiplier applied to the output the router actually delivers.
            OutputFactor,
        }

        #[contract]
        pub struct MockDexRouter;

        #[contractimpl]
        impl MockDexRouter {
            /// Quotes `rate_bps` destination units per 10_000 input units.
            pub fn set_pool_rate(env: Env, pool: Address, rate_bps: i128) {
                env.storage()
                    .persistent()
                    .set(&MockRouterKey::PoolRate(pool), &rate_bps);
            }

            /// Makes the router report an input larger than the one it
            /// committed to consuming, the way a hostile or misconfigured
            /// router would.
            pub fn set_input_skew(env: Env, skew_bps: i128) {
                env.storage()
                    .persistent()
                    .set(&MockRouterKey::InputSkew, &skew_bps);
            }

            /// Scales the output actually delivered, independently of the
            /// output the router reports. A factor below 10_000 under-delivers.
            pub fn set_output_factor(env: Env, factor_bps: i128) {
                env.storage()
                    .persistent()
                    .set(&MockRouterKey::OutputFactor, &factor_bps);
            }

            pub fn execute_path(
                env: Env,
                route: PathRoute,
                bounds: SlippageBounds,
                output_recipient: Address,
            ) -> PathExecutionResult {
                let ceiling = bounds.max_input_amount;
                let mut quoted = ceiling;
                let mut destination: Option<Address> = None;
                for leg in route.legs.iter() {
                    let rate: i128 = env
                        .storage()
                        .persistent()
                        .get(&MockRouterKey::PoolRate(leg.pool))
                        .unwrap_or(10_000);
                    quoted = quoted * rate / 10_000;
                    destination = Some(leg.token_out.clone());
                }

                let skew: i128 = env
                    .storage()
                    .persistent()
                    .get(&MockRouterKey::InputSkew)
                    .unwrap_or(10_000);
                let factor: i128 = env
                    .storage()
                    .persistent()
                    .get(&MockRouterKey::OutputFactor)
                    .unwrap_or(10_000);
                let delivered = quoted * factor / 10_000;

                if let Some(token) = destination {
                    if delivered > 0 {
                        TokenClient::new(&env, &token).transfer(
                            &env.current_contract_address(),
                            &output_recipient,
                            &delivered,
                        );
                    }
                }

                PathExecutionResult {
                    input_amount: ceiling * skew / 10_000,
                    output_amount: quoted,
                }
            }
        }
    }

    /// Router whose `execute_path` returns a value the escrow cannot decode.
    mod malformed_router {
        use super::*;
        use soroban_sdk::{contract, contractimpl};

        #[contract]
        pub struct MalformedRouter;

        #[contractimpl]
        impl MalformedRouter {
            pub fn execute_path(
                _env: Env,
                _route: PathRoute,
                _bounds: SlippageBounds,
                _output_recipient: Address,
            ) -> i128 {
                1
            }
        }
    }

    use malformed_router::MalformedRouter;
    use mock_dex_router::{MockDexRouter, MockDexRouterClient};

    struct PathFixture<'a> {
        client: EscrowContractClient<'a>,
        router_client: MockDexRouterClient<'a>,
        admin: Address,
        treasury: Address,
        buyer: Address,
        seller: Address,
        source: Address,
        middle: Address,
        dest: Address,
        contract_id: Address,
        router: Address,
        pool: Address,
        second_pool: Address,
        escrow_id: u64,
    }

    impl<'a> PathFixture<'a> {
        fn source_balance(&self, env: &Env, holder: &Address) -> i128 {
            TokenClient::new(env, &self.source).balance(holder)
        }

        fn dest_balance(&self, env: &Env, holder: &Address) -> i128 {
            TokenClient::new(env, &self.dest).balance(holder)
        }

        /// A `source -> dest` route quoted at 1:1.
        fn direct_route(&self, env: &Env) -> PathRoute {
            PathRoute {
                legs: Vec::from_array(
                    env,
                    [PathLeg {
                        pool: self.pool.clone(),
                        token_in: self.source.clone(),
                        token_out: self.dest.clone(),
                    }],
                ),
            }
        }

        /// A `source -> middle -> dest` route quoted at 1:1 on both hops.
        fn multi_hop_route(&self, env: &Env) -> PathRoute {
            PathRoute {
                legs: Vec::from_array(
                    env,
                    [
                        PathLeg {
                            pool: self.pool.clone(),
                            token_in: self.source.clone(),
                            token_out: self.middle.clone(),
                        },
                        PathLeg {
                            pool: self.second_pool.clone(),
                            token_in: self.middle.clone(),
                            token_out: self.dest.clone(),
                        },
                    ],
                ),
            }
        }
    }

    fn setup<'a>(env: &'a Env) -> PathFixture<'a> {
mod value_conservation_tests {
mod sub_order_resolution_tests {
    use super::*;
    use soroban_sdk::{
        testutils::Address as _,
        token::{Client as TokenClient, StellarAssetClient},
    };

    fn setup(env: &Env) -> (EscrowContractClient<'_>, Address, Address, Address, Address) {
mod multi_oracle_tests {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        token::{Client as TokenClient, StellarAssetClient},
    };

    fn base(
        env: &Env,
    ) -> (
        EscrowContractClient<'_>,
        Address,
        Address,
        Address,
        Address,
        u64,
        [Address; 3],
    ) {
        env.mock_all_auths();
        let admin = Address::generate(env);
        let treasury = Address::generate(env);
        let buyer = Address::generate(env);
        let seller = Address::generate(env);
        let asset_admin = Address::generate(env);
        let source = env
            .register_stellar_asset_contract_v2(asset_admin.clone())
            .address();
        let middle = env
            .register_stellar_asset_contract_v2(asset_admin.clone())
            .address();
        let dest = env
            .register_stellar_asset_contract_v2(asset_admin)
            .address();

        let contract_id = env.register(
            EscrowContract,
            (EscrowConfig {
                admin: admin.clone(),
                fee_bps: 250,
                treasury: treasury.clone(),
                min_amount: 100,
                max_amount: 1_000_000,
            },),
        );
        let client = EscrowContractClient::new(env, &contract_id);
        for token in [&source, &middle, &dest] {
            client.add_token(&admin, token);
        }
        StellarAssetClient::new(env, &source).mint(&buyer, &10_000);

        let router = env.register(MockDexRouter, ());
        let router_client = MockDexRouterClient::new(env, &router);
        // The mock pays the destination currency out of balances it already holds.
        StellarAssetClient::new(env, &dest).mint(&router, &1_000_000);
        StellarAssetClient::new(env, &middle).mint(&router, &1_000_000);

        let pool = Address::generate(env);
        router_client.set_pool_rate(&pool, &10_000);
        let second_pool = Address::generate(env);
        router_client.set_pool_rate(&second_pool, &10_000);

        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &source,
            &1_000,
            &BytesN::from_array(env, &[71; 32]),
            &1_000,
            &None,
            &None,
        );

        PathFixture {
            client,
            router_client,
            admin,
            treasury,
            buyer,
            seller,
            source,
            middle,
            dest,
            contract_id,
            router,
            pool,
            second_pool,
            escrow_id,
        }
    }

    fn find_event<T>(env: &Env, contract_id: &Address, action: Symbol) -> Option<T>
    where
        T: TryFromVal<Env, soroban_sdk::Val>,
    {
        for (event_contract, topics, data) in env.events().all().iter() {
            if event_contract != *contract_id || topics.len() != 3 {
                continue;
            }
            let topic: Symbol = topics.get(1).unwrap().try_into_val(env).unwrap();
            if topic == action {
                return T::try_from_val(env, &data).ok();
            }
        }
        None
    }

    fn commit_bounds(f: &PathFixture<'_>, min_output_amount: i128, max_input_amount: i128) {
        assert!(f.client.set_slippage_bounds(
            &f.escrow_id,
            &f.buyer,
            &SlippageBounds {
                min_output_amount,
                max_input_amount,
            },
        ));
    }

    #[test]
    fn set_slippage_bounds_rejects_an_unknown_escrow() {
        let env = Env::default();
        let f = setup(&env);

        assert_eq!(
            f.client.try_set_slippage_bounds(
                &999,
                &f.buyer,
                &SlippageBounds {
                    min_output_amount: 950,
                    max_input_amount: 1_000,
                },
            ),
            Err(Ok(EscrowError::NotFound))
        );
    }

    #[test]
    fn set_slippage_bounds_rejects_callers_who_are_neither_buyer_nor_admin() {
        let env = Env::default();
        let f = setup(&env);
        let stranger = Address::generate(&env);
        let bounds = SlippageBounds {
            min_output_amount: 950,
            max_input_amount: 1_000,
        };

        assert_eq!(
            f.client
                .try_set_slippage_bounds(&f.escrow_id, &stranger, &bounds),
            Err(Ok(EscrowError::Unauthorized))
        );
        assert_eq!(f.client.get_slippage_bounds(&f.escrow_id), None);
    }

    #[test]
    fn set_slippage_bounds_rejects_non_positive_bounds() {
        let env = Env::default();
        let f = setup(&env);

        for bounds in [
            SlippageBounds {
                min_output_amount: 0,
                max_input_amount: 1_000,
            },
            SlippageBounds {
                min_output_amount: 950,
                max_input_amount: 0,
            },
            SlippageBounds {
                min_output_amount: -1,
                max_input_amount: 1_000,
            },
            SlippageBounds {
                min_output_amount: 950,
                max_input_amount: -1,
            },
        ] {
            assert_eq!(
                f.client
                    .try_set_slippage_bounds(&f.escrow_id, &f.buyer, &bounds),
                Err(Ok(EscrowError::InvalidSlippageBounds))
            );
        }
        assert_eq!(f.client.get_slippage_bounds(&f.escrow_id), None);
    }

    #[test]
    fn set_slippage_bounds_rejects_a_disputed_escrow() {
        let env = Env::default();
        let f = setup(&env);
        f.client.dispute(&f.escrow_id, &f.buyer);

        assert_eq!(
            f.client.try_set_slippage_bounds(
                &f.escrow_id,
                &f.buyer,
                &SlippageBounds {
                    min_output_amount: 950,
                    max_input_amount: 1_000,
                },
            ),
            Err(Ok(EscrowError::InvalidStatus))
        );
    }

    /// A committed window is the user's floor: it can be tightened at any time
    /// before settlement, but never widened — otherwise a front-runner could
    /// loosen the tolerance to let a worse fill through.
    #[test]
    fn committed_bounds_may_be_tightened_but_never_widened() {
        let env = Env::default();
        let f = setup(&env);
        commit_bounds(&f, 950, 1_000);

        // Tightening (a higher minimum output and a lower ceiling) is allowed.
        commit_bounds(&f, 990, 900);
        assert_eq!(
            f.client.get_slippage_bounds(&f.escrow_id),
            Some(SlippageBounds {
                min_output_amount: 990,
                max_input_amount: 900,
            })
        );

        for looser in [
            SlippageBounds {
                min_output_amount: 989,
                max_input_amount: 900,
            },
            SlippageBounds {
                min_output_amount: 990,
                max_input_amount: 901,
            },
            SlippageBounds {
                min_output_amount: 1,
                max_input_amount: 10_000,
            },
        ] {
            assert_eq!(
                f.client
                    .try_set_slippage_bounds(&f.escrow_id, &f.buyer, &looser),
                Err(Ok(EscrowError::SlippageBoundsTooLoose))
            );
        }
        assert_eq!(
            f.client.get_slippage_bounds(&f.escrow_id),
            Some(SlippageBounds {
                min_output_amount: 990,
                max_input_amount: 900,
            })
        );
    }

    #[test]
    fn set_slippage_bounds_publishes_the_committed_window() {
        let env = Env::default();
        let f = setup(&env);
        commit_bounds(&f, 950, 1_000);

        let event: SlippageBoundsSetEvent =
            find_event(&env, &f.contract_id, symbol_short!("slipset")).unwrap();
        assert_eq!(event.escrow_id, f.escrow_id);
        assert_eq!(event.min_output_amount, 950);
        assert_eq!(event.max_input_amount, 1_000);
        assert_eq!(event.set_by, f.buyer);
    }

    #[test]
    fn the_slippage_view_reports_whether_a_quoted_fill_would_settle() {
        let env = Env::default();
        let f = setup(&env);

        assert!(!f
            .client
            .is_within_slippage_bounds(&f.escrow_id, &1_000, &1_000));
        commit_bounds(&f, 950, 1_000);

        assert!(f
            .client
            .is_within_slippage_bounds(&f.escrow_id, &1_000, &1_000));
        // Boundary: the floor and the ceiling themselves are acceptable.
        assert!(f
            .client
            .is_within_slippage_bounds(&f.escrow_id, &1_000, &950));
        assert!(f.client.is_within_slippage_bounds(&f.escrow_id, &400, &950));
        // Below the floor, above the ceiling, or beyond the escrow balance.
        assert!(!f
            .client
            .is_within_slippage_bounds(&f.escrow_id, &1_000, &949));
        assert!(!f
            .client
            .is_within_slippage_bounds(&f.escrow_id, &1_001, &1_000));
        assert!(!f
            .client
            .is_within_slippage_bounds(&f.escrow_id, &1_001, &1_001));
    }

    #[test]
    fn path_payment_requires_a_committed_slippage_window() {
        let env = Env::default();
        let f = setup(&env);
        let route = f.direct_route(&env);

        assert_eq!(
            f.client
                .try_execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &route),
            Err(Ok(EscrowError::SlippageBoundsNotSet))
        );
    }

    #[test]
    fn path_payment_settles_the_seller_in_the_destination_currency() {
        let env = Env::default();
        let f = setup(&env);
        commit_bounds(&f, 950, 1_000);
        let route = f.direct_route(&env);

        let result = f
            .client
            .execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &route);
        assert_eq!(
            result,
            PathExecutionResult {
                input_amount: 1_000,
                output_amount: 1_000,
            }
        );

        // Read the event before any further contract call: the test host only
        // exposes the events of the invocation that is currently in scope.
        let event: PathPaymentSettledEvent =
            find_event(&env, &f.contract_id, symbol_short!("pathset")).unwrap();
        assert_eq!(event.escrow_id, f.escrow_id);
        assert_eq!(event.router, f.router);
        assert_eq!(event.seller, f.seller);
        assert_eq!(event.leg_count, 1);
        assert_eq!(event.input_amount, 1_000);
        assert_eq!(event.output_amount, 1_000);
        assert_eq!(event.min_output_amount, 950);
        assert_eq!(event.max_input_amount, 1_000);
        assert_eq!(event.settled_by, f.buyer);

        // The seller is paid in the destination currency, the router in the
        // escrowed token net of the 250 bps platform fee, and the escrow keeps
        // nothing.
        assert_eq!(f.dest_balance(&env, &f.seller), 1_000);
        assert_eq!(f.source_balance(&env, &f.treasury), 25);
        assert_eq!(f.source_balance(&env, &f.router), 975);
        assert_eq!(f.source_balance(&env, &f.contract_id), 0);

        let record = f.client.get_escrow(&f.escrow_id);
        assert_eq!(record.released_amount, 1_000);
        assert_eq!(record.refunded_amount, 0);
        assert_eq!(record.status, EscrowStatus::Released);
    }

    #[test]
    fn a_multi_hop_route_settles_through_an_intermediate_currency() {
        let env = Env::default();
        let f = setup(&env);
        commit_bounds(&f, 950, 1_000);
        let route = f.multi_hop_route(&env);

        let result = f
            .client
            .execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &route);

        assert_eq!(result.output_amount, 1_000);

        // Read the event before any further contract call: the test host only
        // exposes the events of the invocation that is currently in scope.
        let event: PathPaymentSettledEvent =
            find_event(&env, &f.contract_id, symbol_short!("pathset")).unwrap();
        assert_eq!(event.leg_count, 2);
        assert_eq!(event.input_amount, 1_000);
        assert_eq!(event.output_amount, 1_000);

        assert_eq!(f.dest_balance(&env, &f.seller), 1_000);
        assert_eq!(
            f.client.get_escrow(&f.escrow_id).status,
            EscrowStatus::Released
        );
    }

    /// Simulated sandwich attack (issue #337).
    ///
    /// The buyer commits a 5% window and broadcasts a route. A searcher
    /// front-runs it with a large buy that pushes the pool price down 10%, so
    /// the victim's back-run fill delivers 900 units instead of 1_000. Because
    /// the contract checks the *realised* fill and not the requested amount,
    /// the invocation is reverted: the router is never paid, the seller
    /// receives nothing, and the escrow stays fully funded for a retry or a
    /// refund once the pool recovers.
    #[test]
    fn a_simulated_sandwich_attack_cannot_settle_outside_the_window() {
        let env = Env::default();
        let f = setup(&env);
        commit_bounds(&f, 950, 1_000);
        let route = f.direct_route(&env);

        // The searcher's front-run moves the pool price down by 10%.
        f.router_client.set_pool_rate(&f.pool, &9_000);

        assert_eq!(
            f.client
                .try_execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &route),
            Err(Ok(EscrowError::SlippageExceeded))
        );

        // No settlement event was published, and nothing moved: the router is
        // unpaid, the seller is unpaid, and the escrow still holds the full
        // amount. The event is read first because the test host only exposes
        // the events of the invocation currently in scope.
        assert!(find_event::<PathPaymentSettledEvent>(
            &env,
            &f.contract_id,
            symbol_short!("pathset")
        )
        .is_none());
        assert_eq!(f.source_balance(&env, &f.router), 0);
        assert_eq!(f.dest_balance(&env, &f.seller), 0);
        assert_eq!(f.source_balance(&env, &f.contract_id), 1_000);
        let record = f.client.get_escrow(&f.escrow_id);
        assert_eq!(record.status, EscrowStatus::Funded);
        assert_eq!(record.released_amount, 0);

        // Once the pool recovers the same committed window settles normally.
        f.router_client.set_pool_rate(&f.pool, &10_000);
        assert!(
            f.client
                .execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &route)
                .output_amount
                > 0
        );

        // Sanity-check the negative assertion above: the same lookup does find
        // the event once the payment actually settles.
        let event: PathPaymentSettledEvent =
            find_event(&env, &f.contract_id, symbol_short!("pathset")).unwrap();
        assert_eq!(event.output_amount, 1_000);
        assert_eq!(f.dest_balance(&env, &f.seller), 1_000);
    }

    /// The dynamic part of the limit: a window tightened after the attacker
    /// started moving the price rejects a move the original window allowed.
    #[test]
    fn a_tightened_window_rejects_a_move_the_wider_window_allowed() {
        let env = Env::default();
        let f = setup(&env);
        let route = f.direct_route(&env);

        commit_bounds(&f, 950, 1_000);
        f.router_client.set_pool_rate(&f.pool, &9_600);
        assert!(f
            .client
            .try_execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &route)
            .is_ok());

        // Same adverse move, but the second escrow committed a tighter floor.
        let second_id = f.client.deposit(
            &f.buyer,
            &f.seller,
            &f.source,
            &1_000,
            &BytesN::from_array(&env, &[72; 32]),
            &1_000,
            &None,
            &None,
        );
        assert!(f.client.set_slippage_bounds(
            &second_id,
            &f.buyer,
            &SlippageBounds {
                min_output_amount: 980,
                max_input_amount: 1_000,
            },
        ));
        assert_eq!(
            f.client
                .try_execute_path_payment(&second_id, &f.buyer, &f.router, &route),
            Err(Ok(EscrowError::SlippageExceeded))
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(env))
            .address();
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0,
            treasury,
            min_amount: 1,
            max_amount: 1_000_000_000,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(env, &contract_id);
        StellarAssetClient::new(env, &token).mint(&buyer, &1_000_000);
        client.add_token(&admin, &token);
        (client, admin, buyer, seller, token)
    }

    fn sample_record(env: &Env, amount: i128, released: i128, refunded: i128) -> EscrowRecord {
        EscrowRecord {
            escrow_id: 1,
            buyer: Address::generate(env),
            seller: Address::generate(env),
            token: Address::generate(env),
            amount,
            released_amount: released,
            refunded_amount: refunded,
            status: EscrowStatus::Funded,
            order_id: BytesN::from_array(env, &[7; 32]),
            created_at: 0,
            updated_at: 0,
            timeout_ledger: 0,
        }
    }

    #[test]
    fn invariant_rejects_cumulative_overrun() {
        let env = Env::default();
        let record = sample_record(&env, 1_000, 600, 300);
        assert_eq!(
            EscrowContract::assert_value_conservation_invariant(&record, 200, 0),
            Err(EscrowError::ExceedsTotalEscrowAmount)
        );
        assert_eq!(
            EscrowContract::assert_value_conservation_invariant(&record, 0, 101),
            Err(EscrowError::ExceedsTotalEscrowAmount)
        );
    }

    #[test]
    fn invariant_allows_exact_disbursement() {
        let env = Env::default();
        let record = sample_record(&env, 1_000, 600, 400);
        assert_eq!(
            EscrowContract::assert_value_conservation_invariant(&record, 0, 0),
            Ok(())
        );
    }

    #[test]
    fn invariant_reports_overflow() {
        let env = Env::default();
        let record = sample_record(&env, i128::MAX, i128::MAX, 0);
        assert_eq!(
            EscrowContract::assert_value_conservation_invariant(&record, 1, 0),
            Err(EscrowError::MathOverflow)
        );
        assert_eq!(
            EscrowContract::assert_value_conservation_invariant(&record, 0, 1),
            Err(EscrowError::MathOverflow)
        );
    }

    #[test]
    fn micro_releases_transition_to_released_exactly_at_zero() {
        let env = Env::default();
        let (client, _admin, buyer, seller, token) = setup(&env);
        let order_id = BytesN::from_array(&env, &[21; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000, &order_id, &1_000, &None, &None,
        );

        // Uneven fractional micro-releases that sum to one Stroop short of the
        // principal. The escrow must remain non-terminal until the last chunk.
        for chunk in [1i128, 2, 3, 94, 100, 300, 499] {
            assert_eq!(client.partial_release(&escrow_id, &buyer, &chunk).released, chunk);
        }
        let record = client.get_escrow(&escrow_id);
        assert_eq!(record.released_amount, 999);
        assert_eq!(record.status, EscrowStatus::Funded);

        assert!(client.partial_release(&escrow_id, &buyer, &1).fully_released);
        let record = client.get_escrow(&escrow_id);
        assert_eq!(record.released_amount, 1_000);
        assert_eq!(record.refunded_amount, 0);
        assert_eq!(record.status, EscrowStatus::Released);

        // Terminal state: nothing remains and no further release is accepted.
        assert_eq!(TokenClient::new(&env, &token).balance(&client.address), 0);
        assert_eq!(
            client.try_partial_release(&escrow_id, &buyer, &1),
            Err(Ok(EscrowError::AlreadyReleased))
    fn sample_items(env: &Env) -> soroban_sdk::Vec<SubOrderItem> {
        soroban_sdk::Vec::from_array(
            env,
            [
                SubOrderItem {
                    item_id: Symbol::new(env, "book"),
                    amount: 20,
                },
                SubOrderItem {
                    item_id: Symbol::new(env, "laptop"),
                    amount: 980,
                },
            ],
        )
    }

    fn disputed_escrow(
        env: &Env,
    ) -> (EscrowContractClient<'_>, Address, Address, Address, Address, u64) {
        let (client, admin, buyer, seller, token) = setup(env);
        let order_id = BytesN::from_array(env, &[41; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000, &order_id, &1_000, &None, &None,
        );
        client.set_sub_order_items(&escrow_id, &buyer, &sample_items(env));
        client.dispute(&escrow_id, &buyer);
        (client, admin, buyer, seller, token, escrow_id)
    }

    #[test]
    fn registering_items_requires_principal_match() {
        let env = Env::default();
        let (client, _admin, buyer, seller, token) = setup(&env);
        let order_id = BytesN::from_array(&env, &[42; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000, &order_id, &1_000, &None, &None,
        );
        let bad = soroban_sdk::Vec::from_array(
            &env,
            [SubOrderItem {
                item_id: Symbol::new(&env, "only"),
                amount: 999,
            }],
        );
        assert_eq!(
            client.try_set_sub_order_items(&escrow_id, &buyer, &bad),
            Err(Ok(EscrowError::InvalidAmount))
        );
    }

    #[test]
    fn contested_refund_and_uncontested_release_are_independent() {
        let env = Env::default();
        let (client, admin, buyer, _seller, token, escrow_id) = disputed_escrow(&env);
        let token_client = TokenClient::new(&env, &token);

        // Only the contested "book" is refunded to the buyer right away; the
        // uncontested laptop does not have to wait for the whole dispute.
        assert!(client.resolve_sub_item_dispute(
            &escrow_id,
            &admin,
            &SubOrderItemResolution {
                item_id: Symbol::new(&env, "book"),
                refund_amount: 20,
                release_amount: 0,
                is_resolved: true,
            },
        ));
        let record = client.get_escrow(&escrow_id);
        assert_eq!(record.refunded_amount, 20);
        assert_eq!(record.released_amount, 0);
        assert_eq!(record.status, EscrowStatus::Disputed);
        assert_eq!(token_client.balance(&buyer), 1_000_000 - 1_000 + 20);

        // The uncontested laptop then releases to the seller; the escrow is
        // terminal and the contract holds nothing.
        assert!(client.resolve_sub_item_dispute(
            &escrow_id,
            &admin,
            &SubOrderItemResolution {
                item_id: Symbol::new(&env, "laptop"),
                refund_amount: 0,
                release_amount: 980,
                is_resolved: true,
            },
        ));
        let record = client.get_escrow(&escrow_id);
        assert_eq!(record.refunded_amount, 20);
        assert_eq!(record.released_amount, 980);
        assert_eq!(record.refunded_amount + record.released_amount, record.amount);
        assert_eq!(record.status, EscrowStatus::Released);
        assert_eq!(token_client.balance(&client.address), 0);
    }

    #[test]
    fn an_item_cannot_be_resolved_twice() {
        let env = Env::default();
        let (client, admin, _buyer, _seller, _token, escrow_id) = disputed_escrow(&env);
        let resolution = SubOrderItemResolution {
            item_id: Symbol::new(&env, "book"),
            refund_amount: 20,
            release_amount: 0,
            is_resolved: true,
        };
        assert!(client.resolve_sub_item_dispute(&escrow_id, &admin, &resolution));
        assert_eq!(
            client.try_resolve_sub_item_dispute(&escrow_id, &admin, &resolution),
            Err(Ok(EscrowError::SubItemAlreadyResolved))
        let order_id = BytesN::from_array(env, &[51; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000, &order_id, &1_000, &None, &None,
        );
        let oracles = [
            Address::generate(env),
            Address::generate(env),
            Address::generate(env),
        ];
        (client, admin, buyer, seller, token, escrow_id, oracles)
    }

    fn configured(
        env: &Env,
    ) -> (
        EscrowContractClient<'_>,
        Address,
        Address,
        Address,
        Address,
        u64,
        [Address; 3],
    ) {
        let (client, admin, buyer, seller, token, escrow_id, oracles) = base(env);
        let config = MultiOracleConfig {
            required_oracle_count: 2,
            oracle_addresses: soroban_sdk::Vec::from_array(
                env,
                [oracles[0].clone(), oracles[1].clone(), oracles[2].clone()],
            ),
            condition_symbol: Symbol::new(env, "delivered"),
        };
        client.set_multi_oracle_config(&admin, &escrow_id, &config);
        (client, admin, buyer, seller, token, escrow_id, oracles)
    }

    #[test]
    fn two_of_three_consensus_releases_escrow() {
        let env = Env::default();
        let (client, _admin, _buyer, seller, token, escrow_id, oracles) = configured(&env);
        let token_client = TokenClient::new(&env, &token);

        // First affirmative vote is recorded but does not release.
        assert!(!client.submit_oracle_attestation(&escrow_id, &oracles[0], &true));
        assert_eq!(client.get_escrow(&escrow_id).status, EscrowStatus::Funded);
        assert_eq!(
            client.try_check_oracle_consensus(&escrow_id),
            Err(Ok(MultiOracleError::ThresholdNotMet))
        );

        // Second affirmative vote crosses the 2-of-3 threshold and auto-releases.
        assert!(client.submit_oracle_attestation(&escrow_id, &oracles[1], &true));
        let record = client.get_escrow(&escrow_id);
        assert_eq!(record.status, EscrowStatus::Released);
        assert_eq!(record.released_amount, 1_000);
        assert_eq!(token_client.balance(&seller), 1_000);
        assert_eq!(token_client.balance(&client.address), 0);

        // Once terminal, further attestations are rejected.
        assert_eq!(
            client.try_submit_oracle_attestation(&escrow_id, &oracles[2], &true),
            Err(Ok(MultiOracleError::InvalidStatus))
        );
    }

    #[test]
    fn path_payment_reverts_when_the_router_consumes_more_than_the_ceiling() {
        let env = Env::default();
        let f = setup(&env);
        commit_bounds(&f, 950, 1_000);
        let route = f.direct_route(&env);

        // The router reports spending 5% more than it was authorised to.
        f.router_client.set_input_skew(&10_500);

        assert_eq!(
            f.client
                .try_execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &route),
            Err(Ok(EscrowError::SlippageExceeded))
        );
        assert_eq!(f.source_balance(&env, &f.router), 0);
        assert_eq!(f.source_balance(&env, &f.contract_id), 1_000);
    }

    #[test]
    fn path_payment_reverts_when_the_route_spends_more_than_the_escrow_holds() {
        let env = Env::default();
        let f = setup(&env);
        // A prior partial refund shrinks the balance the route may draw on.
        f.client.partial_refund(&f.escrow_id, &f.seller, &200);
        commit_bounds(&f, 900, 1_000);
        let route = f.direct_route(&env);

        assert_eq!(
            f.client
                .try_execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &route),
            Err(Ok(EscrowError::InsufficientEscrowBalance))
        );
        assert_eq!(f.source_balance(&env, &f.contract_id), 800);
    }

    #[test]
    fn a_partial_path_payment_leaves_the_remainder_refundable() {
        let env = Env::default();
        let f = setup(&env);
        commit_bounds(&f, 350, 400);
        let route = f.direct_route(&env);

        f.client
            .execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &route);

        let record = f.client.get_escrow(&f.escrow_id);
        assert_eq!(record.released_amount, 400);
        assert_eq!(record.status, EscrowStatus::Funded);
        assert_eq!(f.dest_balance(&env, &f.seller), 400);
        assert_eq!(f.source_balance(&env, &f.contract_id), 600);

        assert!(f.client.refund(&f.escrow_id, &f.seller));
        assert_eq!(f.source_balance(&env, &f.buyer), 9_600);
        assert_eq!(
            f.client.get_escrow(&f.escrow_id).status,
            EscrowStatus::Refunded
    fn resolution_amounts_must_match_the_registered_item() {
        let env = Env::default();
        let (client, admin, _buyer, _seller, _token, escrow_id) = disputed_escrow(&env);
        assert_eq!(
            client.try_resolve_sub_item_dispute(
                &escrow_id,
                &admin,
                &SubOrderItemResolution {
                    item_id: Symbol::new(&env, "book"),
                    refund_amount: 19,
                    release_amount: 0,
                    is_resolved: true,
                },
            ),
            Err(Ok(EscrowError::InvalidDisputeAward))
        );
    }

    #[test]
    fn a_refunded_escrow_cannot_settle_a_path_payment() {
        let env = Env::default();
        let f = setup(&env);
        commit_bounds(&f, 950, 1_000);
        let route = f.direct_route(&env);
        f.client.refund(&f.escrow_id, &f.seller);

        assert_eq!(
            f.client
                .try_execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &route),
    fn release_and_refund_share_the_principal_without_overrun() {
        let env = Env::default();
        let (client, _admin, buyer, seller, token) = setup(&env);
        let order_id = BytesN::from_array(&env, &[22; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000, &order_id, &1_000, &None, &None,
        );

        assert_eq!(client.partial_release(&escrow_id, &buyer, &333).released, 333);
        assert_eq!(client.partial_release(&escrow_id, &buyer, &333).released, 333);
        assert_eq!(client.partial_refund(&escrow_id, &seller, &334).refunded, 334);

        let record = client.get_escrow(&escrow_id);
        assert_eq!(record.released_amount + record.refunded_amount, record.amount);
        assert_eq!(record.status, EscrowStatus::Refunded);
        assert_eq!(TokenClient::new(&env, &token).balance(&client.address), 0);
        assert_eq!(
            client.try_partial_release(&escrow_id, &buyer, &1),
            Err(Ok(EscrowError::AlreadyRefunded))
        );
    }

    #[test]
    fn path_payment_rejects_callers_who_are_neither_buyer_nor_admin() {
        let env = Env::default();
        let f = setup(&env);
        commit_bounds(&f, 950, 1_000);
        let route = f.direct_route(&env);
        let stranger = Address::generate(&env);

        assert_eq!(
            f.client
                .try_execute_path_payment(&f.escrow_id, &stranger, &f.router, &route),
            Err(Ok(EscrowError::Unauthorized))
        );

        // An admin may settle on the buyer's behalf.
        assert!(
            f.client
                .execute_path_payment(&f.escrow_id, &f.admin, &f.router, &route)
                .output_amount
                > 0
    fn duplicate_and_unauthorized_votes_are_rejected() {
        let env = Env::default();
        let (client, _admin, _buyer, _seller, _token, escrow_id, oracles) = configured(&env);
        let stranger = Address::generate(&env);

        assert!(!client.submit_oracle_attestation(&escrow_id, &oracles[0], &true));
        assert_eq!(
            client.try_submit_oracle_attestation(&escrow_id, &oracles[0], &true),
            Err(Ok(MultiOracleError::DuplicateOracleVote))
        );
        assert_eq!(
            client.try_submit_oracle_attestation(&escrow_id, &stranger, &true),
            Err(Ok(MultiOracleError::UnauthorizedOracle))
        );
    }

    #[test]
    fn path_payment_rejects_an_unknown_escrow_and_a_zero_router() {
        let env = Env::default();
        let f = setup(&env);
        commit_bounds(&f, 950, 1_000);
        let route = f.direct_route(&env);
        let zero_contract = Address::from_str(&env, ZERO_CONTRACT_STRKEY);

        assert_eq!(
            f.client
                .try_execute_path_payment(&999, &f.buyer, &f.router, &route),
            Err(Ok(EscrowError::NotFound))
        );
        assert_eq!(
            f.client
                .try_execute_path_payment(&f.escrow_id, &f.buyer, &zero_contract, &route),
            Err(Ok(EscrowError::InvalidAddress))
    fn attestations_after_consensus_window_are_rejected() {
        let env = Env::default();
        let (client, _admin, _buyer, _seller, _token, escrow_id, oracles) = configured(&env);

        env.ledger().set_sequence_number(1_000);
        assert!(!client.submit_oracle_attestation(&escrow_id, &oracles[0], &true));

        env.ledger()
            .set_sequence_number(1_000 + MULTI_ORACLE_CONSENSUS_WINDOW_LEDGERS + 1);
        assert_eq!(
            client.try_submit_oracle_attestation(&escrow_id, &oracles[1], &true),
            Err(Ok(MultiOracleError::ConsensusWindowExpired))
        );
    }

    #[test]
    fn path_payment_rejects_routes_that_do_not_convert_the_escrowed_token() {
        let env = Env::default();
        let f = setup(&env);
        commit_bounds(&f, 950, 1_000);

        // Starts from the wrong token.
        let wrong_start = PathRoute {
            legs: Vec::from_array(
                &env,
                [PathLeg {
                    pool: f.pool.clone(),
                    token_in: f.dest.clone(),
                    token_out: f.source.clone(),
                }],
            ),
        };
        assert_eq!(
            f.client
                .try_execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &wrong_start),
            Err(Ok(EscrowError::InvalidPathRoute))
        );

        // Converts a token into itself.
        let self_converting = PathRoute {
            legs: Vec::from_array(
                &env,
                [PathLeg {
                    pool: f.pool.clone(),
                    token_in: f.source.clone(),
                    token_out: f.source.clone(),
                }],
            ),
        };
        assert_eq!(
            f.client
                .try_execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &self_converting),
            Err(Ok(EscrowError::InvalidPathRoute))
        );

        // Pays out the escrowed token, so it is not a cross-currency payment.
        let round_trip = PathRoute {
            legs: Vec::from_array(
                &env,
                [
                    PathLeg {
                        pool: f.pool.clone(),
                        token_in: f.source.clone(),
                        token_out: f.middle.clone(),
                    },
                    PathLeg {
                        pool: f.pool.clone(),
                        token_in: f.middle.clone(),
                        token_out: f.source.clone(),
                    },
                ],
            ),
        };
        assert_eq!(
            f.client
                .try_execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &round_trip),
            Err(Ok(EscrowError::InvalidPathRoute))
        );
    }

    #[test]
    fn path_payment_rejects_a_route_whose_hops_do_not_chain() {
        let env = Env::default();
        let f = setup(&env);
        commit_bounds(&f, 950, 1_000);

        // The second hop pays out a different token than the first hop produces.
        let broken = PathRoute {
            legs: Vec::from_array(
                &env,
                [
                    PathLeg {
                        pool: f.pool.clone(),
                        token_in: f.source.clone(),
                        token_out: f.middle.clone(),
                    },
                    PathLeg {
                        pool: f.pool.clone(),
                        token_in: f.source.clone(),
                        token_out: f.dest.clone(),
                    },
                ],
            ),
        };
        assert_eq!(
            f.client
                .try_execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &broken),
            Err(Ok(EscrowError::InvalidPathRoute))
        );
    }

    #[test]
    fn path_payment_rejects_empty_and_oversized_routes() {
        let env = Env::default();
        let f = setup(&env);
        commit_bounds(&f, 950, 1_000);

        let empty = PathRoute {
            legs: Vec::new(&env),
        };
        assert_eq!(
            f.client
                .try_execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &empty),
            Err(Ok(EscrowError::InvalidPathRoute))
        );

        // The hop limit is checked first, so an over-long route is rejected
        // before any of its legs are inspected.
        let mut legs: Vec<PathLeg> = Vec::new(&env);
        for i in 0..=MAX_PATH_LEGS {
            let hop = if i % 2 == 0 {
                PathLeg {
                    pool: f.pool.clone(),
                    token_in: f.source.clone(),
                    token_out: f.middle.clone(),
                }
            } else {
                PathLeg {
                    pool: f.pool.clone(),
                    token_in: f.middle.clone(),
                    token_out: f.source.clone(),
                }
            };
            legs.push_back(hop);
        }
        assert_eq!(
            f.client.try_execute_path_payment(
                &f.escrow_id,
                &f.buyer,
                &f.router,
                &PathRoute { legs },
            ),
            Err(Ok(EscrowError::InvalidPathRoute))
        );
    }

    #[test]
    fn path_payment_rejects_route_tokens_that_are_not_whitelisted() {
        let env = Env::default();
        let f = setup(&env);
        commit_bounds(&f, 950, 1_000);
        let unlisted = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        let route = PathRoute {
            legs: Vec::from_array(
                &env,
                [PathLeg {
                    pool: f.pool.clone(),
                    token_in: f.source.clone(),
                    token_out: unlisted,
                }],
            ),
        };

        assert_eq!(
            f.client
                .try_execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &route),
            Err(Ok(EscrowError::TokenNotWhitelisted))
        );
    }

    #[test]
    fn path_payment_rejects_a_router_that_returns_an_unusable_result() {
        let env = Env::default();
        let f = setup(&env);
        commit_bounds(&f, 950, 1_000);
        let route = f.direct_route(&env);
        let broken_router = env.register(MalformedRouter, ());

        assert_eq!(
            f.client
                .try_execute_path_payment(&f.escrow_id, &f.buyer, &broken_router, &route),
            Err(Ok(EscrowError::PathExecutionFailed))
        );
        assert_eq!(f.source_balance(&env, &f.contract_id), 1_000);
    }

    #[test]
    fn a_router_that_does_not_deliver_is_never_paid() {
        let env = Env::default();
        let f = setup(&env);
        commit_bounds(&f, 950, 1_000);
        let route = f.direct_route(&env);

        // The router reports a full output but delivers nothing.
        f.router_client.set_output_factor(&0);

        assert!(f
            .client
            .try_execute_path_payment(&f.escrow_id, &f.buyer, &f.router, &route)
            .is_err());
        assert_eq!(f.source_balance(&env, &f.router), 0);
        assert_eq!(f.source_balance(&env, &f.treasury), 0);
        assert_eq!(f.dest_balance(&env, &f.seller), 0);
        assert_eq!(f.source_balance(&env, &f.contract_id), 1_000);
        assert_eq!(
            f.client.get_escrow(&f.escrow_id).status,
            EscrowStatus::Funded
        );
    fn release_after_partial_refund_cannot_exceed_remaining() {
        let env = Env::default();
        let (client, _admin, buyer, seller, token) = setup(&env);
        let order_id = BytesN::from_array(&env, &[23; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000, &order_id, &1_000, &None, &None,
        );

        assert_eq!(client.partial_refund(&escrow_id, &seller, &400).refunded, 400);
        assert_eq!(
            client.try_partial_release(&escrow_id, &buyer, &601),
            Err(Ok(EscrowError::InsufficientEscrowBalance))
        );

        let record = client.get_escrow(&escrow_id);
        assert_eq!(record.released_amount, 0);
        assert_eq!(record.refunded_amount, 400);
        assert_eq!(record.status, EscrowStatus::Funded);
    fn only_admin_can_resolve_items() {
        let env = Env::default();
        let (client, _admin, buyer, _seller, _token, escrow_id) = disputed_escrow(&env);
        assert_eq!(
            client.try_resolve_sub_item_dispute(
                &escrow_id,
                &buyer,
                &SubOrderItemResolution {
                    item_id: Symbol::new(&env, "book"),
                    refund_amount: 20,
                    release_amount: 0,
                    is_resolved: true,
                },
            ),
            Err(Ok(EscrowError::Unauthorized))
    fn invalid_configurations_are_rejected() {
        let env = Env::default();
        let (client, admin, _buyer, _seller, _token, escrow_id, oracles) = base(&env);

        let too_many = MultiOracleConfig {
            required_oracle_count: 4,
            oracle_addresses: soroban_sdk::Vec::from_array(
                &env,
                [oracles[0].clone(), oracles[1].clone(), oracles[2].clone()],
            ),
            condition_symbol: Symbol::new(&env, "delivered"),
        };
        assert_eq!(
            client.try_set_multi_oracle_config(&admin, &escrow_id, &too_many),
            Err(Ok(EscrowError::InvalidQuorum))
        );

        let duplicates = MultiOracleConfig {
            required_oracle_count: 2,
            oracle_addresses: soroban_sdk::Vec::from_array(
                &env,
                [oracles[0].clone(), oracles[0].clone()],
            ),
            condition_symbol: Symbol::new(&env, "delivered"),
        };
        assert_eq!(
            client.try_set_multi_oracle_config(&admin, &escrow_id, &duplicates),
            Err(Ok(EscrowError::InvalidQuorum))
        );
    }
}

#[cfg(all(test, feature = "full_suite"))]
mod integration_tests;
#[cfg(all(test, feature = "full_suite"))]
mod test;

#[cfg(test)]
mod dual_control_timeout_tests {
    use super::*;
    use ed25519_dalek::Signer;
    use soroban_sdk::{
        testutils::{Address as _, Events, Ledger},
        token::StellarAssetClient,
        xdr::ToXdr,
        TryIntoVal,
    };

    /// Amount above `DUAL_CONTROL_THRESHOLD`, so dual control applies.
    const HIGH_VALUE: i128 = 20_000;
    /// Refund window long enough that the approver deadline, not the escrow
    /// timeout, is the clock under test.
    const ESCROW_TIMEOUT: u32 = 10_000;

    struct Fx<'a> {
        env: Env,
        client: EscrowContractClient<'a>,
        admin: Address,
        buyer: Address,
        seller: Address,
        finance: Address,
        token: Address,
        contract_id: Address,
    }

    fn setup(env: &Env) -> Fx<'_> {
        env.mock_all_auths();
        let admin = Address::generate(env);
        let treasury = Address::generate(env);
        let buyer = Address::generate(env);
        let seller = Address::generate(env);
        let finance = Address::generate(env);
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(env))
            .address();
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 250,
            treasury,
            min_amount: 100,
            max_amount: 1_000_000,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(env, &contract_id);
        StellarAssetClient::new(env, &token).mint(&buyer, &100_000);
        client.add_token(&admin, &token);
        Fx {
            env: env.clone(),
            client,
            admin,
            buyer,
            seller,
            finance,
            token,
            contract_id,
        }
    }

    impl Fx<'_> {
        /// Fund a high-value escrow with dual control and a secondary-approver
        /// deadline of `timeout_ledgers`.
        fn funded_with_timeout(&self, timeout_ledgers: u32, fallback: EscrowStatus) -> u64 {
            let order_id = BytesN::from_array(&self.env, &[7u8; 32]);
            let escrow_id = self.client.deposit(
                &self.buyer,
                &self.seller,
                &self.token,
                &HIGH_VALUE,
                &order_id,
                &ESCROW_TIMEOUT,
                &None,
                &None,
            );
            self.client
                .set_dual_control_config(&self.admin, &escrow_id, &self.finance);
            self.client.set_dual_control_timeout(
                &self.admin,
                &escrow_id,
                &timeout_ledgers,
                &fallback,
            );
            escrow_id
        }

        fn balance(&self, address: &Address) -> i128 {
            self.env.as_contract(&self.contract_id, || {
                soroban_sdk::token::Client::new(&self.env, &self.token).balance(address)
            })
        }

        /// Oracle-signed delivery attestation for `escrow_id`, as required by
        /// `verify_delivery_and_release`.
        fn delivery_proof(&self, escrow_id: u64) -> SignedDeliveryProof {
            let key = ed25519_dalek::SigningKey::from_bytes(&[31u8; 32]);
            let oracle_pubkey = BytesN::from_array(&self.env, &key.verifying_key().to_bytes());
            self.client
                .set_oracle_public_key(&self.admin, &oracle_pubkey);
            let delivery_timestamp = self.env.ledger().timestamp();
            let tracking_hash = BytesN::from_array(&self.env, &[93u8; 32]);
            let payload = SignedDeliveryPayload {
                escrow_id,
                carrier_code: symbol_short!("ups"),
                tracking_hash: tracking_hash.clone(),
                delivery_timestamp,
            }
            .to_xdr(&self.env);
            let mut payload_bytes = vec![0u8; payload.len() as usize];
            payload.copy_into_slice(&mut payload_bytes);
            SignedDeliveryProof {
                escrow_id,
                carrier_code: symbol_short!("ups"),
                tracking_hash,
                delivery_timestamp,
                oracle_pubkey,
                signature: BytesN::from_array(&self.env, &key.sign(&payload_bytes).to_bytes()),
            }
        }
    }

    #[test]
    fn approval_before_the_deadline_authorizes_the_release() {
        let env = Env::default();
        let fx = setup(&env);
        let escrow_id = fx.funded_with_timeout(500, EscrowStatus::Refunded);
        let proof = fx.delivery_proof(escrow_id);

        // A second deadline is not reached yet, so the release stays blocked.
        assert_eq!(
            fx.client
                .try_verify_delivery_and_release(&escrow_id, &fx.buyer, &proof),
            Err(Ok(EscrowError::SecondaryApprovalRequired))
        );

        assert!(fx.client.approve_release(&escrow_id, &fx.finance));

        let result = fx
            .client
            .verify_delivery_and_release(&escrow_id, &fx.buyer, &proof);
        assert!(result.fully_released);
        assert_eq!(
            fx.client.get_escrow(&escrow_id).status,
            EscrowStatus::Released
        );
    }

    #[test]
    fn approval_after_the_deadline_is_rejected() {
        let env = Env::default();
        let fx = setup(&env);
        let escrow_id = fx.funded_with_timeout(100, EscrowStatus::Refunded);
        fx.env.ledger().set_sequence_number(100);

        assert!(fx.client.is_approver_deadline_passed(&escrow_id));
        assert_eq!(
            fx.client.try_approve_release(&escrow_id, &fx.finance),
            Err(Ok(EscrowError::ApproverDeadlineExpired))
        );
        // The late signature must not have been recorded.
        assert_eq!(
            fx.client.try_verify_delivery_and_release(
                &escrow_id,
                &fx.buyer,
                &fx.delivery_proof(escrow_id)
            ),
            Err(Ok(EscrowError::SecondaryApprovalRequired))
        );
        assert_eq!(
            fx.client.get_escrow(&escrow_id).status,
            EscrowStatus::Funded
        );
    }

    #[test]
    fn refund_fallback_returns_the_balance_to_the_buyer() {
        let env = Env::default();
        let fx = setup(&env);
        let escrow_id = fx.funded_with_timeout(100, EscrowStatus::Refunded);
        let keeper = Address::generate(&env);

        // Still inside the approval window: the fallback is premature.
        assert_eq!(
            fx.client
                .try_handle_dual_control_timeout(&escrow_id, &keeper),
            Err(Ok(EscrowError::ApproverDeadlineNotReached))
        );

        // The escrow's own refund timeout has not elapsed either, so the buyer
        // could not have self-served here.
        fx.env.ledger().set_sequence_number(100);
        assert!(fx.client.handle_dual_control_timeout(&escrow_id, &keeper));

        // The buyer is made whole without waiting for the escrow timeout.
        assert_eq!(fx.balance(&fx.buyer), 100_000);
        assert_eq!(fx.balance(&fx.contract_id), 0);
        let record = fx.client.get_escrow(&escrow_id);
        assert_eq!(record.status, EscrowStatus::Refunded);
        assert_eq!(record.refunded_amount, HIGH_VALUE);
        assert_eq!(record.released_amount, 0);
    }

    #[test]
    fn dispute_fallback_escalates_instead_of_moving_funds() {
        let env = Env::default();
        let fx = setup(&env);
        let escrow_id = fx.funded_with_timeout(100, EscrowStatus::Disputed);
        fx.env.ledger().set_sequence_number(100);

        assert!(fx
            .client
            .handle_dual_control_timeout(&escrow_id, &fx.seller));

        let record = fx.client.get_escrow(&escrow_id);
        assert_eq!(record.status, EscrowStatus::Disputed);
        assert_eq!(record.refunded_amount, 0);
        assert_eq!(fx.balance(&fx.contract_id), HIGH_VALUE);
        assert_eq!(fx.balance(&fx.buyer), 100_000 - HIGH_VALUE);

        // A second pass has nothing left to do.
        assert_eq!(
            fx.client
                .try_handle_dual_control_timeout(&escrow_id, &fx.seller),
            Err(Ok(EscrowError::InvalidStatus))
        );
    }

    #[test]
    fn refund_fallback_settles_the_remaining_balance_after_a_partial_refund() {
        let env = Env::default();
        let fx = setup(&env);
        let escrow_id = fx.funded_with_timeout(100, EscrowStatus::Refunded);
        // An admin settlement before the deadline leaves a partial balance.
        fx.client.partial_refund(&escrow_id, &fx.admin, &5_000);
        assert_eq!(fx.balance(&fx.contract_id), HIGH_VALUE - 5_000);

        fx.env.ledger().set_sequence_number(100);
        assert!(fx.client.handle_dual_control_timeout(&escrow_id, &fx.buyer));

        let record = fx.client.get_escrow(&escrow_id);
        assert_eq!(record.status, EscrowStatus::Refunded);
        assert_eq!(record.released_amount, 0);
        assert_eq!(record.refunded_amount, HIGH_VALUE);
        assert_eq!(fx.balance(&fx.contract_id), 0);
    }

    #[test]
    fn fallback_is_refused_once_the_approver_has_signed() {
        let env = Env::default();
        let fx = setup(&env);
        let escrow_id = fx.funded_with_timeout(100, EscrowStatus::Refunded);
        assert!(fx.client.approve_release(&escrow_id, &fx.finance));

        fx.env.ledger().set_sequence_number(100);
        assert_eq!(
            fx.client
                .try_handle_dual_control_timeout(&escrow_id, &fx.buyer),
            Err(Ok(EscrowError::DualControlAlreadyApproved))
        );
        assert_eq!(
            fx.client.get_escrow(&escrow_id).status,
            EscrowStatus::Funded
        );
    }

    #[test]
    fn fallback_reports_a_missing_deadline() {
        let env = Env::default();
        let fx = setup(&env);
        let order_id = BytesN::from_array(&env, &[17u8; 32]);
        let escrow_id = fx.client.deposit(
            &fx.buyer,
            &fx.seller,
            &fx.token,
            &HIGH_VALUE,
            &order_id,
            &ESCROW_TIMEOUT,
            &None,
            &None,
        );

        assert_eq!(
            fx.client.try_get_dual_control_timeout(&escrow_id),
            Err(Ok(EscrowError::DualControlTimeoutNotConfigured))
        );
        assert!(!fx.client.is_approver_deadline_passed(&escrow_id));
        assert_eq!(
            fx.client
                .try_handle_dual_control_timeout(&escrow_id, &fx.buyer),
            Err(Ok(EscrowError::DualControlTimeoutNotConfigured))
        );
        // Without a deadline, the approval window never closes — but the
        // escrow still needs a configured secondary approver.
        fx.client
            .set_dual_control_config(&fx.admin, &escrow_id, &fx.finance);
        assert!(fx.client.approve_release(&escrow_id, &fx.finance));
    }

    #[test]
    fn deadline_configuration_is_validated() {
        let env = Env::default();
        let fx = setup(&env);
        let order_id = BytesN::from_array(&env, &[23u8; 32]);
        let unconfigured = fx.client.deposit(
            &fx.buyer,
            &fx.seller,
            &fx.token,
            &HIGH_VALUE,
            &order_id,
            &ESCROW_TIMEOUT,
            &None,
            &None,
        );
        let escrow_id = fx.funded_with_timeout(100, EscrowStatus::Refunded);
        let stranger = Address::generate(&env);

        assert_eq!(
            fx.client.try_set_dual_control_timeout(
                &stranger,
                &escrow_id,
                &100,
                &EscrowStatus::Refunded
            ),
            Err(Ok(EscrowError::Unauthorized))
        );
        assert_eq!(
            fx.client.try_set_dual_control_timeout(
                &fx.admin,
                &unconfigured,
                &100,
                &EscrowStatus::Refunded
            ),
            Err(Ok(EscrowError::DualControlNotConfigured))
        );
        assert_eq!(
            fx.client.try_set_dual_control_timeout(
                &fx.admin,
                &escrow_id,
                &0,
                &EscrowStatus::Refunded
            ),
            Err(Ok(EscrowError::InvalidExtension))
        );
        // A fallback may only divert funds away from the seller.
        for action in [
            EscrowStatus::Created,
            EscrowStatus::Released,
            EscrowStatus::Cancelled,
        ] {
            assert_eq!(
                fx.client
                    .try_set_dual_control_timeout(&fx.admin, &escrow_id, &100, &action),
                Err(Ok(EscrowError::InvalidFallbackAction))
            );
        }

        // The rejected attempts left the original deadline untouched.
        let stored = fx.client.get_dual_control_timeout(&escrow_id);
        assert_eq!(stored.fallback_action, EscrowStatus::Refunded);
        assert_eq!(stored.approver_deadline_ledger, 100);
    }

    #[test]
    fn only_high_value_escrows_take_a_dual_control_deadline() {
        let env = Env::default();
        let fx = setup(&env);
        let order_id = BytesN::from_array(&env, &[29u8; 32]);
        let escrow_id = fx.client.deposit(
            &fx.buyer,
            &fx.seller,
            &fx.token,
            &1_000,
            &order_id,
            &ESCROW_TIMEOUT,
            &None,
            &None,
        );

        assert_eq!(
            fx.client
                .try_set_dual_control_config(&fx.admin, &escrow_id, &fx.finance),
            Err(Ok(EscrowError::InvalidAmount))
        );
        assert_eq!(
            fx.client.try_set_dual_control_timeout(
                &fx.admin,
                &escrow_id,
                &100,
                &EscrowStatus::Refunded
            ),
            Err(Ok(EscrowError::DualControlNotConfigured))
        );
    }

    #[test]
    fn a_later_deadline_can_replace_an_earlier_one() {
        let env = Env::default();
        let fx = setup(&env);
        let escrow_id = fx.funded_with_timeout(100, EscrowStatus::Refunded);

        fx.env.ledger().set_sequence_number(50);
        let stored = fx.client.set_dual_control_timeout(
            &fx.admin,
            &escrow_id,
            &100,
            &EscrowStatus::Disputed,
        );
        assert_eq!(stored.approver_deadline_ledger, 150);
        assert_eq!(stored.fallback_action, EscrowStatus::Disputed);

        // The original deadline is gone, so the order stays approvable.
        assert!(!fx.client.is_approver_deadline_passed(&escrow_id));
        assert!(fx.client.approve_release(&escrow_id, &fx.finance));
    }

    #[test]
    fn fallback_publishes_its_own_event_alongside_the_settlement_one() {
        let env = Env::default();
        let fx = setup(&env);
        let escrow_id = fx.funded_with_timeout(100, EscrowStatus::Refunded);
        let keeper = Address::generate(&env);
        fx.env.ledger().set_sequence_number(100);

        assert!(fx.client.handle_dual_control_timeout(&escrow_id, &keeper));

        let mut fallback_event = None;
        let mut refund_event = None;
        for (event_contract, topics, data) in env.events().all().iter() {
            if event_contract != fx.contract_id || topics.len() != 3 {
                continue;
            }
            let topic: Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if topic == symbol_short!("dcfb") {
                let event: DualControlTimeoutFallbackEvent = data.try_into_val(&env).unwrap();
                fallback_event = Some(event);
            } else if topic == symbol_short!("refunded") {
                let event: EscrowRefundedEvent = data.try_into_val(&env).unwrap();
                refund_event = Some(event);
            }
        }

        let fallback_event = fallback_event.expect("fallback event not published");
        assert_eq!(fallback_event.escrow_id, escrow_id);
        assert_eq!(fallback_event.approver_deadline_ledger, 100);
        assert_eq!(fallback_event.fallback_action, EscrowStatus::Refunded);
        assert_eq!(fallback_event.refunded_amount, HIGH_VALUE);
        assert_eq!(fallback_event.executed_by, keeper);

        let refund_event = refund_event.expect("refund event not published");
        assert_eq!(refund_event.amount, HIGH_VALUE);
        assert_eq!(refund_event.remaining, 0);
        assert_eq!(refund_event.refunded_by, keeper);
    }

    #[test]
    fn deadline_setting_publishes_a_configuration_event() {
        let env = Env::default();
        let fx = setup(&env);
        let order_id = BytesN::from_array(&env, &[37u8; 32]);
        let escrow_id = fx.client.deposit(
            &fx.buyer,
            &fx.seller,
            &fx.token,
            &HIGH_VALUE,
            &order_id,
            &ESCROW_TIMEOUT,
            &None,
            &None,
        );
        fx.client
            .set_dual_control_config(&fx.admin, &escrow_id, &fx.finance);
        fx.client
            .set_dual_control_timeout(&fx.admin, &escrow_id, &100, &EscrowStatus::Disputed);

        let mut found = false;
        for (event_contract, topics, data) in env.events().all().iter() {
            if event_contract != fx.contract_id || topics.len() != 3 {
                continue;
            }
            let topic: Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if topic == symbol_short!("dctmo") {
                let event: DualControlTimeoutSetEvent = data.try_into_val(&env).unwrap();
                assert_eq!(event.escrow_id, escrow_id);
                assert_eq!(event.approver_deadline_ledger, 100);
                assert_eq!(event.fallback_action, EscrowStatus::Disputed);
                found = true;
            }
        }
        assert!(found);
    }
}

#[cfg(test)]
mod quorum_cleanup_tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn dispute_votes_removed_after_quorum_resolution() {
        let env = Env::default();
        soroban_sdk::testutils::Ledger::with_mut(&env.ledger(), |l| {
            l.min_persistent_entry_ttl = PERSISTENT_BUMP_AMOUNT;
        });
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let arbiter1 = Address::generate(&env);
        let arbiter2 = Address::generate(&env);
        let treasury = Address::generate(&env);

        let token = env.register_stellar_asset_contract(admin.clone());
        let token_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_client.mint(&buyer, &1000i128);

        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0u32,
            treasury: treasury.clone(),
            min_amount: 1i128,
            max_amount: 1000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(&env, &contract_id);
        admin_actions_test::approve(&env, &client, &admin, AdminAction::AddToken(token.clone()));

        let arbiters = soroban_sdk::vec![&env, arbiter1.clone(), arbiter2.clone()];
        client.set_quorum_config(&admin, &arbiters, &2u32);

        let order_id = BytesN::from_array(&env, &[0u8; 32]);
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &1000i128,
            &order_id,
            &1000u32,
            &None::<BytesN<32>>,
            &None::<Symbol>,
        );

        client.dispute(&escrow_id, &buyer);

        client.vote_dispute(&escrow_id, &arbiter1, &true);
        client.vote_dispute(&escrow_id, &arbiter2, &true);

        let votes_key = DataKey::DisputeVotes(escrow_id);
        assert!(env.as_contract(&contract_id, || {
            env.storage().persistent().has(&votes_key)
        }));

        client.resolve_dispute_quorum(&escrow_id, &arbiter1);

        assert!(!env.as_contract(&contract_id, || {
            env.storage().persistent().has(&votes_key)
        }));
    }
}

#[cfg(test)]
mod reentrancy_tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn register_escrow(env: &Env) -> Address {
        let config = EscrowConfig {
            admin: Address::generate(env),
            fee_bps: 0u32,
            treasury: Address::generate(env),
            min_amount: 1i128,
            max_amount: 1_000_000i128,
        };
        env.register(EscrowContract, (config,))
    }

    #[test]
    fn atomic_operation_returns_value_and_releases_guard() {
        let env = Env::default();
        let contract_id = register_escrow(&env);

        let outcome = env.as_contract(&contract_id, || {
            assert_eq!(
                execute_atomic_operation(&env, || Ok::<u32, EscrowError>(42)),
                Ok(42)
            );
            // The lock must have been released, otherwise this second call
            // would fail with ReentrancyDetected.
            execute_atomic_operation(&env, || Ok::<u32, EscrowError>(7))
        });

        assert_eq!(outcome, Ok(7));
    }

    #[test]
    fn atomic_operation_propagates_error_and_releases_guard() {
        let env = Env::default();
        let contract_id = register_escrow(&env);

        let outcome = env.as_contract(&contract_id, || {
            let failed = execute_atomic_operation(&env, || {
                Err::<u32, EscrowError>(EscrowError::ZeroAmount)
            });
            assert_eq!(failed, Err(EscrowError::ZeroAmount));
            // A failed operation must not leave the lock held.
            execute_atomic_operation(&env, || Ok::<u32, EscrowError>(1))
        });

        assert_eq!(outcome, Ok(1));
    }

    #[test]
    fn reentrant_atomic_operation_is_rejected() {
        let env = Env::default();
        let contract_id = register_escrow(&env);

        let outcome = env.as_contract(&contract_id, || {
            execute_atomic_operation(&env, || {
                // Simulates a downstream contract calling back into escrow while
                // an external call is in flight.
                execute_atomic_operation(&env, || Ok::<u32, EscrowError>(1))
            })
        });

        assert_eq!(outcome, Err(EscrowError::ReentrancyDetected));
    }

    #[test]
    fn release_path_is_reentrancy_guarded() {
        // `execute_release` runs its token transfers through
        // `execute_atomic_operation`. A release attempted while an external
        // call already holds the lock must fail fast (and therefore never
        // reach the transfers), leaving the escrow record untouched.
        let env = Env::default();
        let contract_id = register_escrow(&env);

        let outcome = env.as_contract(&contract_id, || {
            execute_atomic_operation(&env, || {
                EscrowContract::execute_release(
                    &env,
                    1u64,
                    &DataKey::Escrow(1u64),
                    EscrowRecord {
                        escrow_id: 1u64,
                        buyer: Address::generate(&env),
                        seller: Address::generate(&env),
                        token: Address::generate(&env),
                        amount: 100i128,
                        released_amount: 0i128,
                        refunded_amount: 0i128,
                        status: EscrowStatus::Funded,
                        order_id: BytesN::from_array(&env, &[0u8; 32]),
                        created_at: 0u64,
                        updated_at: 0u64,
                        timeout_ledger: 0u32,
                    },
                    Address::generate(&env),
                    10i128,
                )
            })
        });

        assert_eq!(outcome, Err(EscrowError::ReentrancyDetected));
    }
}

#[cfg(test)]
mod archival_tests {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Events, Ledger},
        TryIntoVal,
    };

    /// Wall-clock retention enforced by `archive_terminal_escrow`.
    const RETENTION_SECS: u64 = ARCHIVAL_RETENTION_LEDGERS as u64 * SECONDS_PER_LEDGER;

    struct Fixture<'a> {
        env: Env,
        client: EscrowContractClient<'a>,
        contract_id: Address,
        admin: Address,
        buyer: Address,
        seller: Address,
        token: Address,
    }

    /// Registers a whitelisted escrow contract with zero fees so a settlement
    /// drains the balance exactly, keeping the archive-time invariant check
    /// honest in these tests.
    fn setup(env: &Env) -> Fixture<'_> {
        env.mock_all_auths();
        let admin = Address::generate(env);
        let buyer = Address::generate(env);
        let seller = Address::generate(env);
        let treasury = Address::generate(env);
        let token = env.register_stellar_asset_contract(admin.clone());
        let token_client = soroban_sdk::token::StellarAssetClient::new(env, &token);
        token_client.mint(&buyer, &10_000i128);

        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0u32,
            treasury,
            min_amount: 1i128,
            max_amount: 1_000_000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(env, &contract_id);
        client.add_token(&admin, &token);

        Fixture {
            env: env.clone(),
            client,
            contract_id,
            admin,
            buyer,
            seller,
            token,
        }
    }

    impl Fixture<'_> {
        fn deposit(&self, seed: u8) -> u64 {
            self.client.deposit(
                &self.buyer,
                &self.seller,
                &self.token,
                &1_000i128,
                &BytesN::from_array(&self.env, &[seed; 32]),
                &1_000u32,
                &None::<BytesN<32>>,
                &None::<Symbol>,
            )
        }

        /// Creates without funding, leaving the escrow in `Created` — the only
        /// state `cancel` accepts, and the way to reach a `Cancelled` terminal
        /// state that never held funds.
        fn create(&self, seed: u8) -> u64 {
            self.client.create(
                &self.buyer,
                &self.seller,
                &self.token,
                &1_000i128,
                &BytesN::from_array(&self.env, &[seed; 32]),
                &1_000u32,
                &None::<BytesN<32>>,
                &None::<Symbol>,
            )
        }

        /// Every per-escrow entry `archive_terminal_escrow` is responsible for.
        /// An explicit `std::vec` path is needed because the crate-level `Vec`
        /// is the host-backed `soroban_sdk::Vec`.
        fn aux_keys(&self, escrow_id: u64) -> std::vec::Vec<DataKey> {
            std::vec![
                DataKey::DisputeVotes(escrow_id),
                DataKey::TimeoutExtensionVotes(escrow_id),
                DataKey::EscrowMetadataHash(escrow_id),
                DataKey::EscrowMetadataSchema(escrow_id),
                DataKey::ShipmentProof(escrow_id),
                DataKey::ReleaseCondition(escrow_id),
                DataKey::DualControlConfig(escrow_id),
                DataKey::EscrowYieldConfig(escrow_id),
                DataKey::RequireReleaseCondition(escrow_id),
                DataKey::LastBumpLedger(escrow_id),
            ]
        }

        fn persisted(&self, key: &DataKey) -> bool {
            self.env.as_contract(&self.contract_id, || {
                self.env.storage().persistent().has(key)
            })
        }

        /// Grows the ledger clock past the retention window.
        fn advance_past_retention(&self) {
            let target = self.env.ledger().timestamp() + RETENTION_SECS;
            self.env.ledger().with_mut(|li| li.timestamp = target);
        }
    }

    #[test]
    fn released_escrow_is_reclaimed_after_retention() {
        let env = Env::default();
        let f = setup(&env);
        let escrow_id = f.deposit(1);
        let seller = f.seller.clone();
        f.client.release(&escrow_id, &f.buyer, &seller);

        // Aux entries exist before the sweep so the reclaim is observable.
        let aux_keys = f.aux_keys(escrow_id);
        assert!(f.persisted(&DataKey::Escrow(escrow_id)));

        f.advance_past_retention();
        f.client.archive_terminal_escrow(&escrow_id);

        assert!(!f.persisted(&DataKey::Escrow(escrow_id)));
        for key in aux_keys {
            assert!(!f.persisted(&key), "auxiliary entry survived the sweep");
        }
        // Every getter now reports the escrow as gone.
        assert_eq!(
            f.client.try_get_escrow(&escrow_id),
            Err(Ok(EscrowError::NotFound))
        );
    }

    #[test]
    fn archived_event_is_published_before_removal() {
        let env = Env::default();
        let f = setup(&env);
        let escrow_id = f.deposit(2);
        let seller = f.seller.clone();
        f.client.release(&escrow_id, &f.buyer, &seller);

        let terminal_at = f.client.get_escrow(&escrow_id).updated_at;
        f.advance_past_retention();
        let archived_at = f.env.ledger().timestamp();
        f.client.archive_terminal_escrow(&escrow_id);

        let events = f.env.events().all();
        let archived = events.last().expect("the sweep emitted an event");
        assert_eq!(archived.0, f.contract_id);
        assert_eq!(archived.1.len(), 3);

        let action: Symbol = archived.1.get(1).unwrap().try_into_val(&f.env).unwrap();
        assert_eq!(action, symbol_short!("archived"));
        let topic_id: u64 = archived.1.get(2).unwrap().try_into_val(&f.env).unwrap();
        assert_eq!(topic_id, escrow_id);

        let event: EscrowArchivedEvent = archived.2.try_into_val(&f.env).unwrap();
        assert_eq!(event.escrow_id, escrow_id);
        assert_eq!(event.terminal_state, EscrowTerminalState::Released);
        assert_eq!(event.terminal_at, terminal_at);
        assert_eq!(event.archived_at, archived_at);
        // Only the escrow record itself is stored for this escrow — `deposit`
        // wrote no metadata halves, so nothing else is reclaimed.
        assert_eq!(event.cleared_entries, 1);

        // The event is the last thing the sweep leaves behind: the state it
        // describes is already gone by the time the log is read back.
        assert!(!f.persisted(&DataKey::Escrow(escrow_id)));
    }

    #[test]
    fn active_escrow_is_protected_from_archival() {
        let env = Env::default();
        let f = setup(&env);
        let escrow_id = f.deposit(3);

        // Retention is irrelevant while funds are still locked.
        f.advance_past_retention();
        f.advance_past_retention();

        assert_eq!(
            f.client.try_archive_terminal_escrow(&escrow_id),
            Err(Ok(EscrowError::InvalidStatus))
        );
        assert!(f.persisted(&DataKey::Escrow(escrow_id)));
    }

    #[test]
    fn disputed_escrow_is_protected_from_archival() {
        let env = Env::default();
        let f = setup(&env);
        let escrow_id = f.deposit(4);
        f.client.dispute(&escrow_id, &f.buyer);

        f.advance_past_retention();
        f.advance_past_retention();

        assert_eq!(
            f.client.try_archive_terminal_escrow(&escrow_id),
            Err(Ok(EscrowError::InvalidStatus))
        );
        assert!(f.persisted(&DataKey::Escrow(escrow_id)));
    }

    #[test]
    fn retention_window_blocks_premature_archival() {
        let env = Env::default();
        let f = setup(&env);
        let escrow_id = f.deposit(5);
        let seller = f.seller.clone();
        f.client.release(&escrow_id, &f.buyer, &seller);

        // One second short of the window.
        let just_short = f.env.ledger().timestamp() + RETENTION_SECS - 1;
        f.env.ledger().with_mut(|li| li.timestamp = just_short);
        assert_eq!(
            f.client.try_archive_terminal_escrow(&escrow_id),
            Err(Ok(EscrowError::ArchivalRetentionNotElapsed))
        );
        assert!(f.persisted(&DataKey::Escrow(escrow_id)));

        // Exactly at the window the escrow becomes archivable.
        f.env.ledger().with_mut(|li| li.timestamp += 1);
        assert!(f.client.try_archive_terminal_escrow(&escrow_id).is_ok());
    }

    #[test]
    fn retention_window_is_measured_from_the_terminal_transition() {
        let env = Env::default();
        let f = setup(&env);
        let escrow_id = f.deposit(6);

        // Age the escrow well past the window *before* it settles; the sweep
        // must still hold, because retention runs from the terminal transition
        // and not from creation.
        f.advance_past_retention();
        f.advance_past_retention();
        let settled_at = f.env.ledger().timestamp();
        let seller = f.seller.clone();
        f.client.release(&escrow_id, &f.buyer, &seller);

        assert_eq!(
            f.client.try_archive_terminal_escrow(&escrow_id),
            Err(Ok(EscrowError::ArchivalRetentionNotElapsed))
        );

        f.env
            .ledger()
            .with_mut(|li| li.timestamp = settled_at + RETENTION_SECS);
        assert!(f.client.try_archive_terminal_escrow(&escrow_id).is_ok());
    }

    #[test]
    fn cancelled_escrow_is_archivable() {
        let env = Env::default();
        let f = setup(&env);
        // Never funded, so the settled-balance invariant does not apply — the
        // sweep must still reclaim it.
        let escrow_id = f.create(7);
        let seller = f.seller.clone();
        f.client
            .cancel(&escrow_id, &seller, &symbol_short!("expired"));

        f.advance_past_retention();
        f.client.archive_terminal_escrow(&escrow_id);
        assert!(!f.persisted(&DataKey::Escrow(escrow_id)));
    }

    #[test]
    fn refunded_escrow_is_archivable() {
        let env = Env::default();
        let f = setup(&env);
        let escrow_id = f.deposit(8);
        f.client.refund(&escrow_id, &f.seller);

        f.advance_past_retention();
        f.client.archive_terminal_escrow(&escrow_id);
        assert!(!f.persisted(&DataKey::Escrow(escrow_id)));
    }

    #[test]
    fn unknown_escrow_is_reported_as_not_found() {
        let env = Env::default();
        let f = setup(&env);
        f.advance_past_retention();
        assert_eq!(
            f.client.try_archive_terminal_escrow(&999u64),
            Err(Ok(EscrowError::NotFound))
        );
    }

    #[test]
    fn repeated_sweeps_are_rejected_once_the_record_is_gone() {
        let env = Env::default();
        let f = setup(&env);
        let escrow_id = f.deposit(9);
        let seller = f.seller.clone();
        f.client.release(&escrow_id, &f.buyer, &seller);

        f.advance_past_retention();
        f.client.archive_terminal_escrow(&escrow_id);
        assert_eq!(
            f.client.try_archive_terminal_escrow(&escrow_id),
            Err(Ok(EscrowError::NotFound))
        );
    }

    #[test]
    fn sweeping_an_escrow_leaves_its_neighbour_untouched() {
        let env = Env::default();
        let f = setup(&env);
        let archived_id = f.deposit(10);
        let kept_id = f.deposit(11);
        let seller = f.seller.clone();
        f.client.release(&archived_id, &f.buyer, &seller);
        f.client.release(&kept_id, &f.buyer, &seller);

        f.advance_past_retention();
        f.client.archive_terminal_escrow(&archived_id);

        assert!(!f.persisted(&DataKey::Escrow(archived_id)));
        assert!(f.persisted(&DataKey::Escrow(kept_id)));
    }

    #[test]
    fn retention_ledgers_is_thirty_days() {
        // 518_400 ledgers at the nominal 5s close time.
        assert_eq!(ARCHIVAL_RETENTION_LEDGERS, 518_400);
        assert_eq!(RETENTION_SECS, 30 * 24 * 60 * 60);
    }
}

#[cfg(test)]
mod error_code_allocation_tests {
    use super::*;
    const ALLOCATED_RANGES: [(u32, u32); 5] = [
        (400, 999),
        (1_000, 1_999),
        (2_000, 2_999),
        (3_000, 3_999),
        (4_000, 4_999),
    ];
    fn escrow_error_codes() -> [u32; 61] {
    fn escrow_error_codes() -> [u32; 50] {
    fn escrow_error_codes() -> [u32; 52] {
    fn escrow_error_codes() -> [u32; 46] {
    fn escrow_error_codes() -> [u32; 47] {
        [
            EscrowError::AlreadyInitialized as u32,
            EscrowError::NotFound as u32,
            EscrowError::Unauthorized as u32,
            EscrowError::AlreadyReleased as u32,
            EscrowError::AlreadyRefunded as u32,
            EscrowError::InvalidStatus as u32,
            EscrowError::TimeoutNotReached as u32,
            EscrowError::NotDisputed as u32,
            EscrowError::InvalidAmount as u32,
            EscrowError::TokenNotWhitelisted as u32,
            EscrowError::InsufficientEscrowBalance as u32,
            EscrowError::ZeroAmount as u32,
            EscrowError::NoPendingTransfer as u32,
            EscrowError::InvalidPendingAdmin as u32,
            EscrowError::AdminAlreadyExists as u32,
            EscrowError::InvalidFeeBps as u32,
            EscrowError::AmountBelowMin as u32,
            EscrowError::AmountAboveMax as u32,
            EscrowError::InvalidLimits as u32,
            EscrowError::NotAnArbiter as u32,
            EscrowError::AlreadyVoted as u32,
            EscrowError::InvalidQuorum as u32,
            EscrowError::QuorumNotReached as u32,
            EscrowError::QuorumConfigNotSet as u32,
            EscrowError::ConflictingQuorum as u32,
            EscrowError::CreationPaused as u32,
            EscrowError::AlreadyCancelled as u32,
            EscrowError::AlreadyFunded as u32,
            EscrowError::InvalidExtension as u32,
            EscrowError::PoolNotFound as u32,
            EscrowError::InsufficientPoolBalance as u32,
            EscrowError::InvalidAddress as u32,
            EscrowError::InvalidEscrowParticipants as u32,
            EscrowError::ReleaseConditionNotSet as u32,
            EscrowError::OracleCallFailed as u32,
            EscrowError::ConditionNotMet as u32,
            EscrowError::InvalidYieldConfig as u32,
            EscrowError::AmountLimitsNotSet as u32,
            EscrowError::FeeConfigNotSet as u32,
            EscrowError::InvalidReleaseRecipient as u32,
            EscrowError::MerchantNotTrading as u32,
            EscrowError::MerchantStatusCheckFailed as u32,
            EscrowError::SignedProofRequired as u32,
            EscrowError::InvalidSignedDeliveryProof as u32,
            EscrowError::OraclePublicKeyNotSet as u32,
            EscrowError::SlippageBoundsNotSet as u32,
            EscrowError::SlippageExceeded as u32,
            EscrowError::InvalidSlippageBounds as u32,
            EscrowError::SlippageBoundsTooLoose as u32,
            EscrowError::InvalidPathRoute as u32,
            EscrowError::PathExecutionFailed as u32,
            EscrowError::ApproverDeadlineExpired as u32,
            EscrowError::ApproverDeadlineNotReached as u32,
            EscrowError::InvalidFallbackAction as u32,
            EscrowError::DualControlTimeoutNotConfigured as u32,
            EscrowError::DualControlAlreadyApproved as u32,
            EscrowError::ArchivalRetentionNotElapsed as u32,
            EscrowError::NotInInspection as u32,
            EscrowError::InspectionExpired as u32,
            EscrowError::InspectionConfigNotSet as u32,
            EscrowError::InspectionAutoReleaseNotReady as u32,
            EscrowError::EmergencyRescueProposalNotFound as u32,
            EscrowError::EmergencyRescueTimelockNotElapsed as u32,
            EscrowError::EmergencyRescueThresholdNotMet as u32,
            EscrowError::EmergencyRescueAlreadyExecuted as u32,
            EscrowError::EscrowNotEligibleForRescue as u32,
            EscrowError::GuardianNotSet as u32,
            EscrowError::GuardianAlreadySet as u32,
            EscrowError::AdminActionNotFound as u32,
            EscrowError::AdminActionLocked as u32,
            EscrowError::AdminActionVetoed as u32,
            EscrowError::AdminActionAlreadyQueued as u32,
            EscrowError::AdminActionOverflow as u32,
            EscrowError::CancelLockoutActive as u32,
            EscrowError::InvalidCancelLockout as u32,
            EscrowError::MathOverflow as u32,
            EscrowError::InvalidMinFee as u32,
        ]
    }

    /// ===== Emergency Rescue Tests =====

    #[test]
    fn propose_emergency_rescue_requires_admin() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let not_admin = Address::generate(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let recovery_destination = Address::generate(&env);
        let treasury = Address::generate(&env);

        let token = env.register_stellar_asset_contract(admin.clone());
        let token_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_client.mint(&buyer, &1000i128);

        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0u32,
            treasury: treasury.clone(),
            min_amount: 1i128,
            max_amount: 1000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(&env, &contract_id);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[0u8; 32]);
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &100i128,
            &order_id,
            &1000u32,
            &None::<BytesN<32>>,
            &None::<Symbol>,
        );

        // Non-admin cannot propose rescue
        let result = client.try_propose_emergency_rescue(
            &escrow_id,
            &recovery_destination,
            &not_admin,
        );
        assert_eq!(result, Err(Ok(EscrowError::Unauthorized)));
    }

    #[test]
    fn propose_emergency_rescue_rejects_invalid_destination() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let treasury = Address::generate(&env);

        let token = env.register_stellar_asset_contract(admin.clone());
        let token_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_client.mint(&buyer, &1000i128);

        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0u32,
            treasury: treasury.clone(),
            min_amount: 1i128,
            max_amount: 1000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(&env, &contract_id);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[0u8; 32]);
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &100i128,
            &order_id,
            &1000u32,
            &None::<BytesN<32>>,
            &None::<Symbol>,
        );

        // Zero address should be rejected
        let zero_addr = Address::from_str(&env, "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF");
        let result = client.try_propose_emergency_rescue(
            &escrow_id,
            &zero_addr,
            &admin,
        );
        assert_eq!(result, Err(Ok(EscrowError::InvalidAddress)));
    }

    #[test]
    fn propose_emergency_rescue_only_for_funded_or_disputed() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let recovery_destination = Address::generate(&env);
        let treasury = Address::generate(&env);

        let token = env.register_stellar_asset_contract(admin.clone());
        let token_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_client.mint(&buyer, &1000i128);

        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0u32,
            treasury: treasury.clone(),
            min_amount: 1i128,
            max_amount: 1000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(&env, &contract_id);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[0u8; 32]);
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &100i128,
            &order_id,
            &1000u32,
            &None::<BytesN<32>>,
            &None::<Symbol>,
        );

        // Release the escrow to make it terminal
        client.release(&escrow_id, &seller);

        // Cannot propose rescue for released escrow
        let result = client.try_propose_emergency_rescue(
            &escrow_id,
            &recovery_destination,
            &admin,
        );
        assert_eq!(result, Err(Ok(EscrowError::EscrowNotEligibleForRescue)));
    }

    #[test]
    fn emergency_rescue_requires_timelock_elapsed() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let co_admin = Address::generate(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let recovery_destination = Address::generate(&env);
        let treasury = Address::generate(&env);

        let token = env.register_stellar_asset_contract(admin.clone());
        let token_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_client.mint(&buyer, &1000i128);

        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0u32,
            treasury: treasury.clone(),
            min_amount: 1i128,
            max_amount: 1000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(&env, &contract_id);
        client.add_token(&admin, &token);
        client.add_co_admin(&admin, &co_admin);

        let order_id = BytesN::from_array(&env, &[0u8; 32]);
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &100i128,
            &order_id,
            &1000u32,
            &None::<BytesN<32>>,
            &None::<Symbol>,
        );

        // Create proposal
        client.propose_emergency_rescue(&escrow_id, &recovery_destination, &admin);

        // Approve
        client.approve_emergency_rescue(&escrow_id, &co_admin);

        // Try to execute before timelock - should fail
        let result = client.try_emergency_rescue_stalled_escrow(
            &escrow_id,
            &recovery_destination,
            &admin,
        );
        assert_eq!(result, Err(Ok(EscrowError::EmergencyRescueTimelockNotElapsed)));
    }

    #[test]
    fn emergency_rescue_requires_threshold_approvals() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let recovery_destination = Address::generate(&env);
        let treasury = Address::generate(&env);

        let token = env.register_stellar_asset_contract(admin.clone());
        let token_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_client.mint(&buyer, &1000i128);

        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0u32,
            treasury: treasury.clone(),
            min_amount: 1i128,
            max_amount: 1000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(&env, &contract_id);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[0u8; 32]);
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &100i128,
            &order_id,
            &1000u32,
            &None::<BytesN<32>>,
            &None::<Symbol>,
        );

        // Create proposal (proposer counts as first approval)
        client.propose_emergency_rescue(&escrow_id, &recovery_destination, &admin);

        // With default threshold of 2, we need at least 2 approvals
        // Skip timelock for this test by directly advancing ledger time
        env.ledger()
            .set_timestamp(env.ledger().timestamp() + EMERGENCY_RESCUE_TIMELOCK_SECS + 1);

        // Try with only proposer approval - should fail
        let result = client.try_emergency_rescue_stalled_escrow(
            &escrow_id,
            &recovery_destination,
            &admin,
        );
        assert_eq!(result, Err(Ok(EscrowError::EmergencyRescueThresholdNotMet)));
    }

    #[test]
    fn emergency_rescue_executes_successfully_with_valid_proposal() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let co_admin = Address::generate(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let recovery_destination = Address::generate(&env);
        let treasury = Address::generate(&env);

        let token = env.register_stellar_asset_contract(admin.clone());
        let token_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_client.mint(&buyer, &1000i128);

        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0u32,
            treasury: treasury.clone(),
            min_amount: 1i128,
            max_amount: 1000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(&env, &contract_id);
        client.add_token(&admin, &token);
        client.add_co_admin(&admin, &co_admin);

        let order_id = BytesN::from_array(&env, &[0u8; 32]);
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &100i128,
            &order_id,
            &1000u32,
            &None::<BytesN<32>>,
            &None::<Symbol>,
        );

        // Propose and approve
        client.propose_emergency_rescue(&escrow_id, &recovery_destination, &admin);
        client.approve_emergency_rescue(&escrow_id, &co_admin);

        // Skip the timelock
        env.ledger()
            .set_timestamp(env.ledger().timestamp() + EMERGENCY_RESCUE_TIMELOCK_SECS + 1);

        // Execute rescue
        let result = client.try_emergency_rescue_stalled_escrow(
            &escrow_id,
            &recovery_destination,
            &admin,
        );
        assert!(result.is_ok());

        // Verify escrow is now refunded
        let record = client.get_escrow(&escrow_id);
        assert_eq!(record.status, EscrowStatus::Refunded);
        assert_eq!(record.refunded_amount, 100i128);

        // Verify proposal is marked as executed
        let proposal = client.get_emergency_rescue_proposal(&escrow_id);
        assert!(proposal.is_some());
        assert!(proposal.unwrap().executed);
    }

    #[test]
    fn emergency_rescue_prevents_double_execution() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let co_admin = Address::generate(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let recovery_destination = Address::generate(&env);
        let treasury = Address::generate(&env);

        let token = env.register_stellar_asset_contract(admin.clone());
        let token_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_client.mint(&buyer, &1000i128);

        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0u32,
            treasury: treasury.clone(),
            min_amount: 1i128,
            max_amount: 1000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(&env, &contract_id);
        client.add_token(&admin, &token);
        client.add_co_admin(&admin, &co_admin);

        let order_id = BytesN::from_array(&env, &[0u8; 32]);
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &100i128,
            &order_id,
            &1000u32,
            &None::<BytesN<32>>,
            &None::<Symbol>,
        );

        // Propose and approve
        client.propose_emergency_rescue(&escrow_id, &recovery_destination, &admin);
        client.approve_emergency_rescue(&escrow_id, &co_admin);

        // Skip timelock
        env.ledger()
            .set_timestamp(env.ledger().timestamp() + EMERGENCY_RESCUE_TIMELOCK_SECS + 1);

        // First execution succeeds
        assert!(client
            .try_emergency_rescue_stalled_escrow(&escrow_id, &recovery_destination, &admin)
            .is_ok());

        // Second execution fails
        let result = client.try_emergency_rescue_stalled_escrow(
            &escrow_id,
            &recovery_destination,
            &admin,
        );
        assert_eq!(result, Err(Ok(EscrowError::EmergencyRescueAlreadyExecuted)));
    }

    #[test]
    fn get_emergency_rescue_proposal_returns_none_when_not_exists() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let treasury = Address::generate(&env);

        let token = env.register_stellar_asset_contract(admin.clone());
        let token_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_client.mint(&buyer, &1000i128);

        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0u32,
            treasury: treasury.clone(),
            min_amount: 1i128,
            max_amount: 1000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(&env, &contract_id);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[0u8; 32]);
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &100i128,
            &order_id,
            &1000u32,
            &None::<BytesN<32>>,
            &None::<Symbol>,
        );

        // No proposal exists yet
        let proposal = client.get_emergency_rescue_proposal(&escrow_id);
        assert!(proposal.is_none());
    }

    #[test]
    fn approve_emergency_rescue_requires_admin() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let not_admin = Address::generate(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let recovery_destination = Address::generate(&env);
        let treasury = Address::generate(&env);

        let token = env.register_stellar_asset_contract(admin.clone());
        let token_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_client.mint(&buyer, &1000i128);

        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0u32,
            treasury: treasury.clone(),
            min_amount: 1i128,
            max_amount: 1000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(&env, &contract_id);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[0u8; 32]);
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &100i128,
            &order_id,
            &1000u32,
            &None::<BytesN<32>>,
            &None::<Symbol>,
        );

        // Create proposal
        client.propose_emergency_rescue(&escrow_id, &recovery_destination, &admin);

        // Non-admin cannot approve
        let result = client.try_approve_emergency_rescue(&escrow_id, &not_admin);
        assert_eq!(result, Err(Ok(EscrowError::Unauthorized)));
            EscrowError::MathOverflow as u32,
            EscrowError::ReentrancyDetected as u32,
        ]
    }

    #[test]
    fn fee_calculation_checks_maximum_i128_boundary() {
        assert_eq!(
            calculate_fee_and_yield(i128::MAX, 0),
            Ok(0),
        );
        assert_eq!(
            calculate_fee_and_yield(i128::MAX / 10_000, 10_000),
            Ok(i128::MAX / 10_000),
        );
        assert_eq!(
            calculate_fee_and_yield(i128::MAX, 10_000),
            Err(EscrowError::MathOverflow),
        );
    }

    #[test]
    fn yield_calculation_checks_maximum_i128_boundary() {
        assert_eq!(calculate_yield(i128::MAX, 0, u64::MAX), Ok(0));
        assert_eq!(
            calculate_yield(i128::MAX, 1, u64::MAX),
            Err(EscrowError::MathOverflow),
        );
    }

    #[test]
    fn math_overflow_uses_next_escrow_error_code() {
        assert_eq!(EscrowError::MathOverflow as u32, 411);
    }

    #[test]
    fn escrow_error_codes_are_unique() {
        let mut codes = escrow_error_codes();
        codes.sort_unstable();
        for pair in codes.windows(2) {
            assert_ne!(pair[0], pair[1], "duplicate EscrowError code: {}", pair[0]);
        }
    }
    #[test]
    fn cross_contract_ranges_are_disjoint() {
        let mut ranges = ALLOCATED_RANGES;
        ranges.sort_unstable();
        for pair in ranges.windows(2) {
            assert!(
                pair[0].1 < pair[1].0,
                "overlapping error-code ranges: {}..={} and {}..={}",
                pair[0].0,
                pair[0].1,
                pair[1].0,
                pair[1].1
            );
        }
    }
    #[test]
    fn escrow_error_codes_avoid_other_contract_ranges() {
        for &code in &escrow_error_codes() {
            if code >= 400 {
                assert!(
                    code <= ALLOCATED_RANGES[0].1,
                    "EscrowError code {} is outside the escrow allocation",
                    code
                );
                for &(lo, hi) in &ALLOCATED_RANGES[1..] {
                    assert!(
                        !(lo..=hi).contains(&code),
                        "EscrowError code {} collides with another contract's range {}..={}",
                        code,
                        lo,
                        hi
                    );
                }
            }
        }
    }
}

// ─── Issue #321: SHA-256 order metadata digest verification ──────────────
//
// Added as a fresh, self-contained `#[cfg(test)]` module (like
// `quorum_cleanup_tests` / `error_code_allocation_tests` above) rather than
// in `escrow/src/test.rs`: that file (and `integration_tests.rs`) is
// pre-existing, gated behind the non-default `full_suite` feature, and its
// own top-of-file comment in `escrow/Cargo.toml` states it is "under active
// repair" and "does not currently compile" — confirmed while reading the
// file for this change (e.g. `test_initialize_rejects_zero_treasury` around
// line 245 is missing its own `#[test]`/signature and is accidentally
// nested inside the previous test function's body, referencing that
// function's locals). That breakage is pre-existing, unrelated to issues
// #315/#320/#321/#323, and out of scope for this change, so it was left
// untouched; new tests were placed here instead so they actually compile
// and run under a plain `cargo test`.
#[cfg(test)]
mod order_metadata_digest_tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn setup(env: &Env) -> (EscrowContractClient<'_>, Address) {
        env.mock_all_auths();
        let admin = Address::generate(env);
        let treasury = Address::generate(env);
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0u32,
            treasury,
            min_amount: 1i128,
            max_amount: 1_000_000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(env, &contract_id);
        (client, admin)
    }

    #[test]
    fn digest_matches_the_exact_canonical_payload() {
        let env = Env::default();
        let (client, _admin) = setup(&env);
        let payload = Bytes::from_array(&env, b"{\"item\":\"widget\",\"qty\":1}");
        let expected: BytesN<32> = env.crypto().sha256(&payload).into();

        assert!(client.verify_order_metadata_digest(&payload, &expected));
    }

    #[test]
    fn digest_rejects_a_tampered_payload() {
        let env = Env::default();
        let (client, _admin) = setup(&env);
        let original = Bytes::from_array(&env, b"{\"item\":\"widget\",\"qty\":1}");
        let tampered = Bytes::from_array(&env, b"{\"item\":\"widget\",\"qty\":9}");
        let expected: BytesN<32> = env.crypto().sha256(&original).into();

        assert!(!client.verify_order_metadata_digest(&tampered, &expected));
    }

    #[test]
    fn verify_escrow_order_metadata_checks_against_the_stored_hash() {
        let env = Env::default();
        let (client, admin) = setup(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token = env.register_stellar_asset_contract(admin.clone());
        soroban_sdk::token::StellarAssetClient::new(&env, &token).mint(&buyer, &1000i128);
        client.add_token(&admin, &token);

        let payload = Bytes::from_array(&env, b"{\"order\":\"abc123\"}");
        let order_hash: BytesN<32> = env.crypto().sha256(&payload).into();
        let schema = symbol_short!("order_v1");
        let order_id = BytesN::from_array(&env, &[5u8; 32]);

        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &100i128,
            &order_id,
            &1000u32,
            &Some(order_hash),
            &Some(schema),
        );

        assert!(client.verify_escrow_order_metadata(&escrow_id, &payload));

        let tampered = Bytes::from_array(&env, b"{\"order\":\"tampered\"}");
        assert!(!client.verify_escrow_order_metadata(&escrow_id, &tampered));
    }

    #[test]
    fn verify_escrow_order_metadata_not_found() {
        let env = Env::default();
        let (client, _admin) = setup(&env);
        let bogus_payload = Bytes::from_array(&env, b"{}");
        assert_eq!(
            client.try_verify_escrow_order_metadata(&999u64, &bogus_payload),
            Err(Ok(EscrowError::NotFound))
        );
    }

    #[test]
    fn verify_escrow_order_metadata_metadata_not_set() {
        let env = Env::default();
        let (client, admin) = setup(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token = env.register_stellar_asset_contract(admin.clone());
        soroban_sdk::token::StellarAssetClient::new(&env, &token).mint(&buyer, &1000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[6u8; 32]);
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &100i128,
            &order_id,
            &1000u32,
            &None::<BytesN<32>>,
            &None::<Symbol>,
        );

        let payload = Bytes::from_array(&env, b"{}");
        assert_eq!(
            client.try_verify_escrow_order_metadata(&escrow_id, &payload),
            Err(Ok(EscrowError::MetadataNotSet))
        );
    }
}

// ─── Issue #320: NFT proof-of-purchase receipt minting on release ────────
#[cfg(test)]
mod purchase_receipt_tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    /// Minimal external contract standing in for a real NFT/receipt-minting
    /// contract, so this suite can exercise the escrow contract's real
    /// cross-contract call path without depending on one existing anywhere
    /// else in this workspace. Mints monotonically increasing token ids.
    #[contract]
    struct MockReceiptMinter;

    #[contractimpl]
    impl MockReceiptMinter {
        pub fn mint_receipt(env: Env, receipt: PurchaseReceiptData) -> u64 {
            let next_id: u64 = env
                .storage()
                .instance()
                .get(&symbol_short!("next_id"))
                .unwrap_or(1);
            env.storage()
                .instance()
                .set(&symbol_short!("next_id"), &(next_id + 1));
            env.storage()
                .persistent()
                .set(&symbol_short!("lastrcpt"), &receipt);
            next_id
        }
    }

    struct Fixture {
        client_id: Address,
        admin: Address,
        buyer: Address,
        seller: Address,
        token: Address,
    }

    fn setup(env: &Env) -> Fixture {
        env.mock_all_auths();
        let admin = Address::generate(env);
        let buyer = Address::generate(env);
        let seller = Address::generate(env);
        let treasury = Address::generate(env);
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0u32,
            treasury,
            min_amount: 1i128,
            max_amount: 1_000_000i128,
        };
        let client_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(env, &client_id);

        let token = env.register_stellar_asset_contract(admin.clone());
        soroban_sdk::token::StellarAssetClient::new(env, &token).mint(&buyer, &10_000i128);
        client.add_token(&admin, &token);

        Fixture {
            client_id,
            admin,
            buyer,
            seller,
            token,
        }
    }

    fn deposit_escrow(env: &Env, client: &EscrowContractClient, fx: &Fixture, seed: u8) -> u64 {
        let order_id = BytesN::from_array(env, &[seed; 32]);
        client.deposit(
            &fx.buyer,
            &fx.seller,
            &fx.token,
            &1000i128,
            &order_id,
            &1000u32,
            &None::<BytesN<32>>,
            &None::<Symbol>,
        )
    }

    #[test]
    fn full_release_mints_a_receipt_when_a_minter_is_configured() {
        let env = Env::default();
        let fx = setup(&env);
        let client = EscrowContractClient::new(&env, &fx.client_id);

        let minter_id = env.register(MockReceiptMinter, ());
        client.set_receipt_minter_contract(&fx.admin, &minter_id);

        let escrow_id = deposit_escrow(&env, &client, &fx, 1);
        // Buyer-initiated release of the full remaining amount (<= the
        // dual-control threshold, so no signed delivery proof is required).
        client.release(&escrow_id, &fx.buyer, &fx.seller);

        let escrow = client.get_escrow(&escrow_id);
        assert_eq!(escrow.status, EscrowStatus::Released);
        assert!(
            escrow.receipt_token_id.is_some(),
            "a fully released escrow with a configured minter must record a receipt token id"
        );

        let token_id = client.get_purchase_receipt_token_id(&escrow_id);
        assert_eq!(token_id, escrow.receipt_token_id);
    }

    #[test]
    fn minting_failure_never_blocks_fund_settlement() {
        let env = Env::default();
        let fx = setup(&env);
        let client = EscrowContractClient::new(&env, &fx.client_id);

        // Point the minter at an address with no deployed contract at all —
        // the cross-contract call must fail cleanly (caught by
        // `try_invoke_contract`), not trap the whole transaction.
        let bogus_minter = Address::generate(&env);
        client.set_receipt_minter_contract(&fx.admin, &bogus_minter);

        let escrow_id = deposit_escrow(&env, &client, &fx, 2);
        let released = client.release(&escrow_id, &fx.buyer, &fx.seller);
        assert!(released, "release must still succeed when minting fails");

        let escrow = client.get_escrow(&escrow_id);
        assert_eq!(escrow.status, EscrowStatus::Released);
        assert_eq!(
            escrow.receipt_token_id, None,
            "a failed mint must leave receipt_token_id unset rather than erroring"
#[cfg(test)]
mod min_fee_floor_tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    const FEE_BPS: u32 = 250;

    struct Fixture<'a> {
        client: EscrowContractClient<'a>,
        admin: Address,
        treasury: Address,
        buyer: Address,
        seller: Address,
        token: Address,
        token_client: soroban_sdk::token::TokenClient<'a>,
    }

    /// Deploys the contract with `fee_bps` and funds `buyer` with
    /// `buyer_funds` of a whitelisted test token.
    fn setup<'a>(env: &'a Env, fee_bps: u32, buyer_funds: i128) -> Fixture<'a> {
        env.mock_all_auths();
        let admin = Address::generate(env);
        let treasury = Address::generate(env);
        let buyer = Address::generate(env);
        let seller = Address::generate(env);
        let token = env
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        soroban_sdk::token::StellarAssetClient::new(env, &token).mint(&buyer, &buyer_funds);
        let token_client = soroban_sdk::token::TokenClient::new(env, &token);

        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps,
            treasury: treasury.clone(),
            min_amount: 1i128,
            max_amount: 1_000_000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(env, &contract_id);
        client.add_token(&admin, &token);

        Fixture {
            client,
            admin,
            treasury,
            buyer,
            seller,
            token,
            token_client,
        }
    }

    impl Fixture<'_> {
        fn balance(&self, who: &Address) -> i128 {
            self.token_client.balance(who)
        }

        /// Funds a fresh escrow of `amount` and returns its id.
        fn fund(&self, env: &Env, amount: i128, seed: u8) -> u64 {
            let order_id = BytesN::from_array(env, &[seed; 32]);
            self.client.deposit(
                &self.buyer,
                &self.seller,
                &self.token,
                &amount,
                &order_id,
                &1000u32,
                &None::<BytesN<32>>,
                &None::<Symbol>,
            )
        }
    }

    #[test]
    fn micro_amount_pays_the_floor_instead_of_zero() {
        // 10 * 250 / 10_000 truncates to 0, which is what the attack relied on.
        assert_eq!(calculate_fee_with_minimum(10, FEE_BPS, 0), Ok(0));
        assert_eq!(
            calculate_fee_with_minimum(10, FEE_BPS, DEFAULT_MIN_FEE_STROOPS),
            Ok(1)
        );
    }

    #[test]
    fn no_receipt_is_attempted_when_no_minter_is_configured() {
        let env = Env::default();
        let fx = setup(&env);
        let client = EscrowContractClient::new(&env, &fx.client_id);

        let escrow_id = deposit_escrow(&env, &client, &fx, 3);
        client.release(&escrow_id, &fx.buyer, &fx.seller);

        let escrow = client.get_escrow(&escrow_id);
        assert_eq!(escrow.status, EscrowStatus::Released);
        assert_eq!(escrow.receipt_token_id, None);
    }

    #[test]
    fn get_purchase_receipt_token_id_not_found() {
        let env = Env::default();
        let fx = setup(&env);
        let client = EscrowContractClient::new(&env, &fx.client_id);

        assert_eq!(
            client.try_get_purchase_receipt_token_id(&999u64),
            Err(Ok(EscrowError::NotFound))
    fn pro_rata_fee_above_the_floor_is_unchanged() {
        assert_eq!(
            calculate_fee_with_minimum(1_000_000, FEE_BPS, DEFAULT_MIN_FEE_STROOPS),
            Ok(25_000)
        );
        assert_eq!(
            calculate_fee_with_minimum(1_000_000, FEE_BPS, 0),
            Ok(25_000)
        );
    }

    #[test]
    fn set_receipt_minter_contract_requires_admin() {
        let env = Env::default();
        let fx = setup(&env);
        let client = EscrowContractClient::new(&env, &fx.client_id);

        let not_admin = Address::generate(&env);
        let minter = Address::generate(&env);
        assert_eq!(
            client.try_set_receipt_minter_contract(&not_admin, &minter),
            Err(Ok(EscrowError::Unauthorized))
        );
    }

    #[test]
    fn set_receipt_minter_contract_rejects_zero_address() {
        let env = Env::default();
        let fx = setup(&env);
        let client = EscrowContractClient::new(&env, &fx.client_id);

        let zero = Address::from_str(&env, ZERO_CONTRACT_STRKEY);
        assert_eq!(
            client.try_set_receipt_minter_contract(&fx.admin, &zero),
            Err(Ok(EscrowError::InvalidAddress))
        );
    }
}

#[cfg(test)]
mod lending_pool_delegation_tests {
    use super::*;
    use delego_interfaces::{MockLendingPool, MockLendingPoolClient};
    use soroban_sdk::testutils::{Address as _, Ledger};
    use soroban_sdk::{token, Address, BytesN, Env};

    fn setup_pool_escrow(env: &Env) -> (EscrowContractClient<'_>, Address, Address) {
        let admin = Address::generate(env);
        let treasury = Address::generate(env);
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0u32,
            treasury: treasury.clone(),
            min_amount: 1i128,
            max_amount: 100_000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(env, &contract_id);
        (client, admin, contract_id)
    }

    fn funded_escrow(
        env: &Env,
        client: &EscrowContractClient<'_>,
        admin: &Address,
    ) -> (u64, Address) {
        let buyer = Address::generate(env);
        let seller = Address::generate(env);
        let token_admin = Address::generate(env);
        let token = env.register_stellar_asset_contract(token_admin);
        let token_client = token::StellarAssetClient::new(env, &token);
        token_client.mint(&buyer, &10_000i128);
        client.add_token(admin, &token);

        let order_id = BytesN::from_array(env, &[90u8; 32]);
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &1_000i128,
            &order_id,
            &1_000u32,
            &None::<BytesN<32>>,
            &None::<soroban_sdk::Symbol>,
        );
        (escrow_id, token)
    }

    /// When a pool is configured and reports yield, `get_accrued_yield` must
    /// return the pool-reported figure rather than the internal APR estimate.
    #[test]
    fn get_accrued_yield_delegates_to_pool() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, contract_id) = setup_pool_escrow(&env);
        let (escrow_id, token) = funded_escrow(&env, &client, &admin);

        let pool_id = env.register(MockLendingPool, ());
        let pool = MockLendingPoolClient::new(&env, &pool_id);
        client.set_yield_config(&admin, &escrow_id, &pool_id, &500u32);

        // Seed the escrow's notional pool position at creation time, accruing
        // at a deliberately different rate (100% APR) than the escrow config.
        pool.set_position(&contract_id, &token, &1_000i128);
        pool.set_yield_rate(&10_000u32);

        // Advance one year: internal estimate = 1000 * 500bps = 50, while the
        // pool reports 1000 * 10000bps = 1000.
        env.ledger().with_mut(|li| {
            li.sequence_number = 1000;
            li.timestamp = 31_536_000;
        });

        let view = client.get_accrued_yield(&escrow_id);
        assert_eq!(view.accrued, 1_000i128, "pool-reported yield must win");
        assert_eq!(view.apy_bps, 500, "apy_bps still reflects escrow config");
        assert_eq!(view.held_seconds, 31_536_000);
    }

    /// A paused (unreachable) pool must not break yield reads: the escrow
    /// falls back to the internal APR estimate.
    #[test]
    fn get_accrued_yield_falls_back_when_pool_paused() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, contract_id) = setup_pool_escrow(&env);
        let (escrow_id, token) = funded_escrow(&env, &client, &admin);

        let pool_id = env.register(MockLendingPool, ());
        let pool = MockLendingPoolClient::new(&env, &pool_id);
        client.set_yield_config(&admin, &escrow_id, &pool_id, &500u32);

        pool.set_position(&contract_id, &token, &1_000i128);
        pool.set_yield_rate(&10_000u32);
        env.ledger().with_mut(|li| {
            li.sequence_number = 1000;
            li.timestamp = 31_536_000;
        });

        // Pool reachable: delegated figure wins.
        let before = client.get_accrued_yield(&escrow_id);
        assert_eq!(before.accrued, 1_000i128);

        // Pause the pool: reads must fall back to the internal 5% estimate (50).
        pool.set_paused(&true);
        let after = client.get_accrued_yield(&escrow_id);
        assert_eq!(after.accrued, 50i128, "fallback must kick in when pool is paused");
    }

    /// A pool with no position for this escrow reports zero; the escrow treats
    /// zero as non-authoritative and falls back to its internal estimate.
    #[test]
    fn get_accrued_yield_falls_back_when_pool_reports_zero() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_pool_escrow(&env);
        let (escrow_id, _token) = funded_escrow(&env, &client, &admin);

        let pool_id = env.register(MockLendingPool, ());
        client.set_yield_config(&admin, &escrow_id, &pool_id, &500u32);

        env.ledger().with_mut(|li| {
            li.sequence_number = 1000;
            li.timestamp = 31_536_000;
        });

        // No position seeded: pool returns 0, escrow must use its own estimate.
        let view = client.get_accrued_yield(&escrow_id);
        assert_eq!(view.accrued, 50i128, "zero pool report must fall back internally");
    }

    /// An unreachable lending address (no contract) must not break reads: the
    /// escrow falls back to the internal APR estimate.
    #[test]
    fn get_accrued_yield_falls_back_when_pool_unreachable() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_pool_escrow(&env);
        let (escrow_id, _token) = funded_escrow(&env, &client, &admin);

        let lending = Address::generate(&env);
        client.set_yield_config(&admin, &escrow_id, &lending, &500u32);

        env.ledger().with_mut(|li| {
            li.sequence_number = 1000;
            li.timestamp = 31_536_000;
        });

        let view = client.get_accrued_yield(&escrow_id);
        assert_eq!(view.accrued, 50i128, "unreachable pool must fall back internally");
    fn a_zero_fee_configuration_is_never_floored() {
        // A 0 bps platform fee is an explicit "charge nothing" setting, not a
        // fee that truncation erased, so the floor must not invent a charge.
        assert_eq!(
            calculate_fee_with_minimum(10, 0, DEFAULT_MIN_FEE_STROOPS),
            Ok(0)
        );
        assert_eq!(
            calculate_fee_with_minimum(10, 0, MAX_MIN_FEE_STROOPS),
            Ok(0)
        );
        assert_eq!(
            calculate_fee_with_minimum(1_000_000, 0, MAX_MIN_FEE_STROOPS),
            Ok(0)
        );
    }

    #[test]
    fn floor_lifts_a_truncated_but_non_zero_fee() {
        // 100 * 250 / 10_000 == 2, below a 50 stroop floor.
        assert_eq!(calculate_fee_with_minimum(100, FEE_BPS, 50), Ok(50));
        assert_eq!(calculate_fee_with_minimum(100, FEE_BPS, 2), Ok(2));
    }

    #[test]
    fn floor_is_clamped_to_the_amount() {
        // A 5 stroop escrow can never be charged more than 5 stroops.
        assert_eq!(calculate_fee_with_minimum(5, FEE_BPS, 1_000), Ok(5));
        assert_eq!(calculate_fee_with_minimum(5, FEE_BPS, 5), Ok(5));
    }

    #[test]
    fn zero_amount_is_never_floored() {
        assert_eq!(calculate_fee_with_minimum(0, FEE_BPS, 1_000), Ok(0));
    }

    #[test]
    fn fragmenting_a_payment_cannot_reduce_the_fee() {
        // One 10_000 stroop payment settles for 250.
        let single = calculate_fee_with_minimum(10_000, FEE_BPS, DEFAULT_MIN_FEE_STROOPS).unwrap();
        // The same value split into 1_000 dust escrows truncates to zero per
        // escrow without the floor; with the floor every leg pays 1.
        let fragmented: i128 = (0..1_000)
            .map(|_| calculate_fee_with_minimum(10, FEE_BPS, DEFAULT_MIN_FEE_STROOPS).unwrap())
            .sum();
        assert_eq!(single, 250);
        assert_eq!(fragmented, 1_000);
        assert!(fragmented > single);
    }

    #[test]
    fn fee_never_exceeds_the_amount_it_is_deducted_from() {
        for amount in 1..500i128 {
            for min_fee in [0i128, 1, 7, 250, 10_000] {
                let fee = calculate_fee_with_minimum(amount, FEE_BPS, min_fee).unwrap();
                assert!(fee >= 0, "negative fee for amount {amount}");
                assert!(fee <= amount, "fee {fee} exceeds amount {amount}");
            }
        }
    }

    #[test]
    fn overflow_is_reported_instead_of_wrapping() {
        assert_eq!(
            calculate_fee_with_minimum(i128::MAX, 1000, DEFAULT_MIN_FEE_STROOPS),
            Err(EscrowError::MathOverflow)
        );
    }

    #[test]
    fn overflow_free_helper_matches_the_checked_multiplication() {
        for amount in [1i128, 39, 40, 41, 9_999, 10_000, 12_345_678] {
            for bps in [0u32, 1, 25, 250, 1000] {
                assert_eq!(bps_fee(amount, bps), amount * bps as i128 / 10_000);
            }
        }
    }

    #[test]
    fn default_floor_is_one_stroop() {
        let env = Env::default();
        let f = setup(&env, FEE_BPS, 10);
        assert_eq!(f.client.get_min_fee_stroops(), DEFAULT_MIN_FEE_STROOPS);
    }

    #[test]
    fn admin_can_configure_the_floor_including_zero() {
        let env = Env::default();
        let f = setup(&env, FEE_BPS, 10);

        assert!(f.client.set_min_fee_stroops(&f.admin, &250));
        assert_eq!(f.client.get_min_fee_stroops(), 250);

        assert!(f.client.set_min_fee_stroops(&f.admin, &MAX_MIN_FEE_STROOPS));
        assert_eq!(f.client.get_min_fee_stroops(), MAX_MIN_FEE_STROOPS);

        // Opting out of the floor restores the pre-#362 pro-rata behaviour.
        assert!(f.client.set_min_fee_stroops(&f.admin, &0));
        assert_eq!(f.client.get_min_fee_stroops(), 0);
    }

    #[test]
    fn floor_config_rejects_non_admin_and_out_of_range_values() {
        let env = Env::default();
        let f = setup(&env, FEE_BPS, 10);
        let stranger = Address::generate(&env);

        assert_eq!(
            f.client.try_set_min_fee_stroops(&stranger, &100),
            Err(Ok(EscrowError::Unauthorized))
        );
        assert_eq!(
            f.client.try_set_min_fee_stroops(&f.admin, &-1),
            Err(Ok(EscrowError::InvalidMinFee))
        );
        assert_eq!(
            f.client
                .try_set_min_fee_stroops(&f.admin, &(MAX_MIN_FEE_STROOPS + 1)),
            Err(Ok(EscrowError::InvalidMinFee))
        );
        assert_eq!(f.client.get_min_fee_stroops(), DEFAULT_MIN_FEE_STROOPS);
    }

    #[test]
    fn micro_release_pays_the_floor_to_the_treasury() {
        let env = Env::default();
        let f = setup(&env, FEE_BPS, 10);

        let escrow_id = f.fund(&env, 10, 1);
        f.client.release(&escrow_id, &f.buyer, &f.seller);

        // Without the floor the treasury would receive 0 and the seller 10.
        assert_eq!(f.balance(&f.treasury), 1);
        assert_eq!(f.balance(&f.seller), 9);
    }

    #[test]
    fn micro_release_is_fee_free_once_the_floor_is_disabled() {
        let env = Env::default();
        let f = setup(&env, FEE_BPS, 10);
        f.client.set_min_fee_stroops(&f.admin, &0);

        let escrow_id = f.fund(&env, 10, 1);
        f.client.release(&escrow_id, &f.buyer, &f.seller);

        assert_eq!(f.balance(&f.treasury), 0);
        assert_eq!(f.balance(&f.seller), 10);
    }

    #[test]
    fn configured_floor_overrides_the_pro_rata_fee() {
        let env = Env::default();
        let f = setup(&env, FEE_BPS, 100);
        // 100 * 250 / 10_000 == 2 pro rata, floored to 50.
        f.client.set_min_fee_stroops(&f.admin, &50);

        let escrow_id = f.fund(&env, 100, 1);
        f.client.release(&escrow_id, &f.buyer, &f.seller);

        assert_eq!(f.balance(&f.treasury), 50);
        assert_eq!(f.balance(&f.seller), 50);
    }

    #[test]
    fn fragmented_releases_are_feeed_more_than_one_settlement() {
        let env = Env::default();
        // 200 for the fragmented sweep + 200 for the single settlement.
        let f = setup(&env, FEE_BPS, 400);

        // 20 dust escrows of 10 stroops: 1 stroop fee per escrow.
        for seed in 0..20u8 {
            let escrow_id = f.fund(&env, 10, seed);
            f.client.release(&escrow_id, &f.buyer, &f.seller);
        }
        let fragmented_fee = f.balance(&f.treasury);

        // The same 200 stroops settled in a single escrow costs 5.
        let single_id = f.fund(&env, 200, 200);
        f.client.release(&single_id, &f.buyer, &f.seller);
        let single_fee = f.balance(&f.treasury) - fragmented_fee;

        assert_eq!(fragmented_fee, 20);
        assert_eq!(single_fee, 5);
        assert!(fragmented_fee > single_fee);
    }

    #[test]
    fn multi_treasury_distribution_also_pays_the_floor() {
        let env = Env::default();
        let f = setup(&env, FEE_BPS, 10);
        let treasury_a = Address::generate(&env);
        let treasury_b = Address::generate(&env);
        let shares = soroban_sdk::vec![
            &env,
            TreasuryShare {
                treasury: treasury_a.clone(),
                bps: 150,
            },
            TreasuryShare {
                treasury: treasury_b.clone(),
                bps: 150,
            },
        ];
        f.client.set_fee_distribution(&f.admin, &shares);

        let escrow_id = f.fund(&env, 10, 1);
        f.client.release(&escrow_id, &f.buyer, &f.seller);

        // The 1 stroop floor is distributed in full, not rounded away.
        assert_eq!(f.balance(&treasury_a) + f.balance(&treasury_b), 1);
        assert_eq!(f.balance(&f.seller), 9);
    }

    #[test]
    fn dispute_resolution_pays_the_floor_on_the_full_balance() {
        let env = Env::default();
        let f = setup(&env, FEE_BPS, 10);

        let escrow_id = f.fund(&env, 10, 1);
        f.client.dispute(&escrow_id, &f.buyer);
        f.client.resolve_dispute(&escrow_id, &f.admin, &true);

        assert_eq!(f.balance(&f.treasury), 1);
        assert_eq!(f.balance(&f.seller), 9);
    }

    #[test]
    fn floor_cannot_exceed_the_escrow_balance() {
        let env = Env::default();
        let f = setup(&env, FEE_BPS, 5);
        f.client.set_min_fee_stroops(&f.admin, &1_000);

        let escrow_id = f.fund(&env, 5, 1);
        f.client.release(&escrow_id, &f.buyer, &f.seller);

        assert_eq!(f.balance(&f.treasury), 5);
        assert_eq!(f.balance(&f.seller), 0);
    }

    #[test]
    fn partial_releases_charge_the_floor_per_leg() {
        let env = Env::default();
        let f = setup(&env, FEE_BPS, 30);

        let escrow_id = f.fund(&env, 30, 1);
        f.client.partial_release(&escrow_id, &f.buyer, &10);
        f.client.partial_release(&escrow_id, &f.buyer, &20);

        // 10 * 250 / 10_000 == 0 -> floored to 1; 20 * 250 / 10_000 == 0 -> 1.
        assert_eq!(f.balance(&f.treasury), 2);
        assert_eq!(f.balance(&f.seller), 28);
    }
}
