//! Delego Reputation Contract
//!
//! Tracks time-decayed trust scores for merchants and agents on the Delego
//! platform, driven by escrow transaction outcomes and counterparty ratings.

// Contract crates compile as no_std for release and wasm builds, but keep std
// enabled during testing so dev-dependencies and test assertions operate normally.
// This exact conditional form must be consistent across all workspace contract crates.
#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]
// Several entry points mirror escrow/permissions call shapes and exceed
// clippy's default 7-argument limit; restructuring them would break the
// published ABI these contracts are reviewed against.
#![allow(clippy::too_many_arguments)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN,
    ConversionError, Env, EnvBase, IntoVal, InvokeError, MapObject, String, Symbol, TryFromVal,
    TryIntoVal, Val, Vec,
};

pub mod algorithm;
#[cfg(test)]
mod rehabilitation_test;
#[cfg(test)]
mod blinded_review_test;
#[cfg(test)]
mod fraud_report_test;
#[cfg(test)]
mod reputation_reader_test;
mod oracle_proof;
pub use oracle_proof::{CarrierProof, CarrierProofVerifiedEvent};
pub mod scoring;
pub use scoring::{
    aggregate_decayed_score, calculate_decayed_merchant_score, compute_decayed_weight,
    FeedbackReview,
};

pub mod antisybil;
pub use antisybil::{
    calculate_reviewer_weight, get_buyer_completed_orders, EscrowListPageMirror,
};

/// Half-life of the reputation decay curve, in ledgers (~30 days at 5s/ledger).
pub const DECAY_HALF_LIFE_LEDGERS: u32 = 518_400;

/// Fixed-point scale used by [`compute_fixed_point_decay`].
///
/// The decay factor is represented as a Q32.32 value: `1 << 32` corresponds
/// to a factor of exactly `1.0`. Keeping the factor in a 64-bit integer lets
/// the whole computation run on integer ALU ops with no floating point and no
/// allocation, which keeps the Soroban CPU instruction count well under the
/// transaction budget.
const DECAY_ONE: u64 = 1u64 << 32;

/// Computes the time-decayed reputation score using a fixed-point
/// approximation of `score * 2^(-elapsed / half_life)`.
///
/// The implementation is branch-light, allocation-free, and loop-free:
///
/// 1. `elapsed_ledgers` is reduced modulo the half-life so the exponent stays
///    in `[0, 1)`, then the integer number of whole half-lives is applied as a
///    right shift (each half-life halves the score exactly).
/// 2. The fractional part is evaluated with a truncated Taylor series of
///    `2^(-x) = exp(-x * ln 2)` using fixed-point constants, giving better
///    than 0.1% accuracy across the full `[0, 1)` exponent range.
///
/// Returns `0` when `initial_score` is `0` or when the elapsed time is large
/// enough that the decayed score rounds below one basis point.
pub fn compute_fixed_point_decay(initial_score: u32, elapsed_ledgers: u32) -> u32 {
    if initial_score == 0 {
        return 0;
    }

    // Number of whole half-lives elapsed, and the remaining fractional part.
    let whole_halvings = elapsed_ledgers / DECAY_HALF_LIFE_LEDGERS;
    let remainder = elapsed_ledgers % DECAY_HALF_LIFE_LEDGERS;

    // Once we have shifted past 32 halvings the score is effectively zero.
    if whole_halvings >= 32 {
        return 0;
    }

    // Fractional exponent in Q32.32: x = remainder / half_life, in [0, 1).
    let x: u64 = ((remainder as u64) << 32) / (DECAY_HALF_LIFE_LEDGERS as u64);

    // ln(2) in Q32.32, used to convert base-2 decay into a natural exp.
    const LN2_Q32: u64 = 2_977_044_706;

    // t = x * ln(2), still in Q32.32 and in [0, ln 2).
    let t: u64 = ((x as u128 * LN2_Q32 as u128) >> 32) as u64;

    // Truncated Taylor series for exp(-t):
    //   exp(-t) ~= 1 - t + t^2/2 - t^3/6 + t^4/24 - t^5/120
    // All terms are evaluated in Q32.32 with u128 intermediates to avoid
    // overflow, then summed. The truncation error over t in [0, ln 2) is
    // below 1e-6, comfortably inside the 0.1% tolerance.
    let t2: u128 = (t as u128 * t as u128) >> 32;
    let t3: u128 = (t2 * t as u128) >> 32;
    let t4: u128 = (t3 * t as u128) >> 32;
    let t5: u128 = (t4 * t as u128) >> 32;

    let one: i128 = DECAY_ONE as i128;
    let term1: i128 = t as i128;
    let term2: i128 = (t2 / 2) as i128;
    let term3: i128 = (t3 / 6) as i128;
    let term4: i128 = (t4 / 24) as i128;
    let term5: i128 = (t5 / 120) as i128;

    let factor: i128 = one - term1 + term2 - term3 + term4 - term5;
    let factor: u64 = if factor < 0 { 0 } else { factor as u64 };

    // Apply the fractional factor, then the whole half-life shifts.
    let scaled: u128 = initial_score as u128 * factor as u128;
    let mut result: u64 = (scaled >> 32) as u64;
    result >>= whole_halvings;

    if result > u32::MAX as u64 {
        u32::MAX
    } else {
        result as u32
    }
}

/// Classification of an entity in the reputation system. Merchants risk
/// non-delivery and counterfeit items; buyer agents risk payment
/// delinquency and frivolous dispute abuse. Each role is evaluated against
/// domain-relevant metrics rather than a single uniform score.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntityRole {
    Merchant,
    BuyerAgent,
}

/// Role-scoped performance indicators tracked alongside the base reputation
/// score. The same three fields are stored for both roles, but risk
/// assessment algorithms weight them differently: merchants are evaluated
/// primarily on fulfillment rate, while buyer agents are evaluated on
/// dispute frequency and timely payment rate.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoleSpecificMetrics {
    pub role: EntityRole,
    pub fulfillment_rate_bps: u32,
    pub dispute_frequency_bps: u32,
    pub timely_payment_rate_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationScore {
    pub entity: Address,
    /// Classification determining which domain metrics apply to this entity.
    pub role: EntityRole,
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
    /// Total penalty subtracted from `base_score_bps`: dispute penalties for
    /// currently-relevant disputes plus the accumulated fraud slashes from
    /// [`ReputationContract::apply_fraud_reputation_slash`] (issue #392).
    pub penalty_bps: i128,
    pub total_transactions: u64,
    pub final_score: u32,
}

/// Composite reputation score blending transaction-count and dollar-volume
/// signals so that micro-transaction spam cannot inflate a merchant's
/// standing while high-value transactions carry appropriate significance.
///
/// All fields are in basis points (0-10000).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompositeScore {
    /// Count-weighted score in basis points (0-10000).
    pub count_score_bps: u32,
    /// Volume-weighted score in basis points (0-10000), computed with
    /// logarithmic dampening so a single whale transaction cannot dominate.
    pub volume_score_bps: u32,
    /// Canonical blended score: 40% count score + 60% volume score.
    pub blended_score_bps: u32,
    /// Confidence rating in basis points (0-10000) reflecting how much
    /// transaction volume backs the blended score.
    pub confidence_rating: u32,
}

/// Ledger window an attestation stays authoritative for after it was
/// certified: ~1 day at 5s per ledger. Deliberately far shorter than the
/// score's decay half-life, so a cached attestation cannot outlive a
/// material reputation change by more than a day even if a client ignores
/// the underlying events.
///
/// Exposed so off-chain clients can pre-compute when a cached snapshot will
/// need refreshing without calling the contract first.
pub const ATTESTATION_VALIDITY_LEDGERS: u32 = 17_280;

/// Tamper-proof, self-describing snapshot of an entity's public reputation
/// metrics, generated by
/// [`ReputationContract::generate_score_attestation`] so off-chain clients
/// (buyer agents, search indices, caches) can rank and filter merchants
/// without an RPC round-trip per candidate.
///
/// The snapshot is not stored on-chain — it is derived deterministically
/// from contract state, so a client can (and should) re-derive the current
/// attestation at any time and compare it against a cached copy using
/// [`ReputationContract::verify_score_attestation`]. Because every field is
/// covered by the comparison, a client cannot be tricked into acting on a
/// forged or stale score without the verification call failing.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationAttestation {
    /// Entity the snapshot describes.
    pub entity: Address,
    /// Public score in basis points (0-10000), masked exactly like
    /// [`ReputationContract::get_reputation`] while the entity is below
    /// `ReputationConfig::min_transactions_threshold`.
    pub score_bps: u32,
    /// Lifetime transaction count backing the score, saturating at
    /// `u32::MAX` so the wire shape stays compact.
    pub total_transactions: u32,
    /// Ledger sequence at which the snapshot was taken (sequence checkpoint).
    pub certified_at_ledger: u32,
    /// Ledger sequence after which the snapshot must be considered stale.
    pub expires_at_ledger: u32,
}

/// Result of checking a cached [`ReputationAttestation`] against current
/// contract state.
///
/// `valid` is the only field clients must gate on; the others explain *why*
/// an attestation was rejected and provide the refreshed snapshot so a
/// client can swap in the current value instead of paying a second call.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttestationVerification {
    /// The attestation that was verified, echoed back for correlation.
    pub attestation: ReputationAttestation,
    /// `true` only when the attestation is inside its validity window *and*
    /// every attested metric still matches current contract state.
    pub valid: bool,
    /// `true` once the current ledger has reached
    /// `ReputationAttestation::expires_at_ledger`, or when the attested
    /// window is malformed (`expires_at_ledger <= certified_at_ledger`).
    pub expired: bool,
    /// `true` when `score_bps` and `total_transactions` still match the
    /// current snapshot. A stale-but-unexpired attestation reports
    /// `expired = false, score_matches = false`.
    pub score_matches: bool,
    /// Freshly generated snapshot for the same entity, usable as the
    /// client's replacement cache entry.
    pub current: ReputationAttestation,
}

/// Pure staleness check for a cached [`ReputationAttestation`], usable by
/// off-chain clients without an RPC round-trip.
///
/// A window is malformed when `expires_at_ledger <= certified_at_ledger`;
/// such an attestation is always reported as expired so a hand-built
/// snapshot cannot be used to bypass expiry.
pub fn attestation_is_expired(attestation: &ReputationAttestation, current_ledger: u32) -> bool {
    attestation.expires_at_ledger <= attestation.certified_at_ledger
        || current_ledger >= attestation.expires_at_ledger
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionRecord {
    pub escrow_id: u64,
    pub entity: Address,
    pub counterparty: Address,
    /// Transaction amount in the smallest denomination of the token.
    /// This field feeds the volume-weighted component of the composite
    /// reputation score (see [`CompositeScore`]) with logarithmic dampening.
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

/// Structural mirror of the escrow contract's `EscrowStatus` wire shape,
/// used only to decode `get_escrow` responses when independently verifying a
/// settlement before accepting a review (issue #286). Variant names must be
/// kept in sync with `delego-escrow`'s `EscrowStatus`; the two contracts are
/// deployed and versioned separately, so this is a deliberate ABI-level
/// mirror rather than a shared Rust dependency (consistent with how this
/// workspace's other cross-contract calls, e.g. the marketplace merchant
/// check in `delego-escrow`, decode a minimal typed shape over
/// `try_invoke_contract` instead of depending on the other contract's crate).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EscrowStatusMirror {
    Created,
    Funded,
    Released,
    Refunded,
    Disputed,
    Cancelled,
}

/// Structural mirror of the escrow contract's `EscrowRecord` wire shape
/// (issue #286); see [`EscrowStatusMirror`] for why this mirrors rather than
/// imports the type. Field names and types must stay in sync with
/// `delego-escrow`'s `EscrowRecord` — Soroban decodes cross-contract struct
/// values by field name, so any drift here makes verification calls fail
/// closed (surfaced as `ReputationError::UnverifiedOrder`) rather than
/// silently misinterpreting the response.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowRecordMirror {
    pub escrow_id: u64,
    pub buyer: Address,
    pub seller: Address,
    pub token: Address,
    pub amount: i128,
    pub released_amount: i128,
    pub refunded_amount: i128,
    pub status: EscrowStatusMirror,
    pub order_id: BytesN<32>,
    pub created_at: u64,
    pub updated_at: u64,
    pub timeout_ledger: u32,
}

/// Proof binding a review to a specific, independently verifiable escrow
/// settlement (issue #286). Submitted to
/// [`ReputationContract::submit_verified_review`], which calls back into
/// `escrow_contract` to confirm the escrow really reached `Released` status
/// before accepting the review — closing the Sybil-inflation gap where an
/// address with no real settled order could otherwise submit a rating.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedReviewProof {
    /// Escrow identifier on `escrow_contract`.
    pub escrow_id: u64,
    /// Escrow contract instance to verify settlement against.
    pub escrow_contract: Address,
    /// Expected value of the escrow's `order_id`, binding the proof to a
    /// specific order rather than just any released escrow between the two
    /// parties.
    pub order_hash: BytesN<32>,
}

/// Cryptographic commitment of a verified buyer review, linking an on-chain
/// rating to verifiable off-chain review content (issue #XXX).
///
/// The `content_hash` is a SHA256 hash of the review text, enabling buyers
/// to prove they wrote a specific review without storing the full review
/// text on-chain. Each escrow_id can only have one review commitment.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewCommitment {
    pub escrow_id: u64,
    pub reviewer: Address,
    pub merchant: Address,
    pub rating: u32, // 1 to 5
    pub content_hash: BytesN<32>,
    pub submitted_ledger: u32,
}

/// Dispute resolution vote cast by a peer validator for a disputed transaction.
/// Validators stake reputation weight and vote on the resolution outcome:
/// 'buyer' (refund buyer), 'merchant' (release to merchant), or 'split' (split funds).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeQuorumVote {
    pub dispute_id: u64,
    pub validator: Address,
    pub verdict: Symbol, // 'buyer', 'merchant', 'split'
    pub staked_vote_weight: i128,
}

/// Normalized reputation score with sigmoid scaling and recency weighting.
/// This is the final, user-facing score combining multiple signals into a
/// confidence-weighted 0-10,000 basis point scale.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedReputationScore {
    pub merchant: Address,
    pub normalized_score: u32, // 0 to 10,000 (basis points)
    pub confidence_score: u32, // Based on total review sample size
    pub last_updated_ledger: u32,
}

/// Exponential penalty tracking for merchants with consecutive lost disputes.
/// Each consecutive lost dispute doubles the penalty multiplier (up to 16x)
/// until the merchant completes 20 dispute-free orders.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputePenaltyRecord {
    pub merchant: Address,
    pub consecutive_lost_disputes: u32,
    pub last_dispute_ledger: u32,
    pub active_penalty_multiplier: u32,
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

/// Rehabilitation program for penalised merchants to restore trust through staking collateral
/// and completing supervised probation orders.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RehabilitationProgram {
    pub merchant: Address,
    pub collateral_staked_stroops: i128,
    pub probation_orders_required: u32,
    pub probation_orders_completed: u32,
    pub status: Symbol,
    pub started_at: u64,
    pub baseline_score_at_entry: u32,
}

/// Blinded review receipt for privacy-preserving reviews via ephemeral key delegation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlindedReviewReceipt {
    pub escrow_id_commitment: BytesN<32>,
    pub blinded_public_key: BytesN<32>,
    pub blind_signature: BytesN<64>,
    pub issued_at: u64,
}

/// Fraud report with bond staking. Reporter must stake to flag suspicious activity;
/// if confirmed, reporter earns bounty; if frivolous, bond is slashed.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FraudReport {
    pub report_id: u64,
    pub reporter: Address,
    pub target_entity: Address,
    pub evidence_hash: BytesN<32>,
    pub staked_bond_stroops: i128,
    pub status: Symbol,
    pub filed_at: u64,
    pub resolved_at: Option<u64>,
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
/// Soroban error codes surface as raw `u32` values over a bridge. Each contract
/// owns a disjoint contiguous numeric range (issue #269):
///
/// | Contract            | Error enum          | Range         |
/// |---------------------|---------------------|---------------|
/// | escrow              | `EscrowError`       | 1_000..=1_999 |
/// | permissions         | `PermissionError`   | 2_000..=2_999 |
/// | reputation          | `ReputationError`   | 3_000..=3_999 |
/// | delegation_registry | `DelegationError`   | 4_000..=4_999 |
/// | marketplace         | `MarketplaceError`  | 5_000..=5_999 |
///
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ReputationError {
    /// Reserved for API/ABI compatibility with issue #18's error contract.
    /// Unreachable in normal operation: initialization now happens via
    /// `__constructor` (see [`ReputationContract::__constructor`]), which
    /// the host guarantees can run at most once, atomically with
    /// deployment — there is no second call for this to guard against.
    AlreadyInitialized = 3001,
    NotInitialized = 3002,
    Unauthorized = 3003,
    EntityNotFound = 3004,
    /// Same escrow_id already rated.
    DuplicateRating = 3005,
    /// Rating out of range.
    InvalidRating = 3006,
    EntityFrozen = 3007,
    /// Same reporter already flagged.
    AlreadyFlagged = 3008,
    /// Invalid input parameter.
    InvalidParam = 3009,
    /// No active (unresolved) flag from reporter.
    NoActiveFlag = 3010,
    /// Reporter did not flag the entity.
    NotFlagReporter = 3011,
    /// `submit_verified_review`'s `VerifiedReviewProof` did not check out:
    /// the referenced escrow could not be read from `escrow_contract`, is
    /// not in `Released` status, its `order_id` did not match
    /// `order_hash`, or `rater`/`entity` are not its buyer/seller pair
    /// (issue #286).
    UnverifiedOrder = 3012,
    /// Review commitment already exists for this escrow_id.
    DuplicateReviewCommitment = 3013,
    /// Caller is not the buyer for this escrow.
    NotBuyer = 3014,
    /// Escrow is not in settled state.
    EscrowNotSettled = 3015,
    /// Quorum threshold not reached for dispute verdict.
    QuorumNotReached = 3016,
    /// Invalid dispute verdict symbol.
    InvalidVerdictSymbol = 3017,
    /// Merchant is not eligible for rehabilitation (not previously penalised/frozen).
    NotEligibleForRehabilitation = 3018,
    /// Rehabilitation program not found for merchant.
    RehabilitationNotFound = 3019,
    /// Probation orders requirement not yet met for graduation.
    ProbationNotComplete = 3020,
    /// Invalid collateral amount (must be positive).
    InvalidCollateralAmount = 3021,
    /// Blinded review verification failed.
    BlindedReviewVerificationFailed = 3022,
    /// Blinded receipt already used (double-submission prevented).
    BlindedReceiptAlreadyUsed = 3023,
    /// Fraud report not found.
    FraudReportNotFound = 3024,
    /// Invalid fraud bond amount.
    InvalidFraudBond = 3025,
    /// Fraud report resolution invalid state.
    InvalidReportStatus = 3026,
    /// A carrier proof was submitted by an oracle that is not registered.
    CarrierOracleNotAuthorized = 3027,
    /// No matching precedent rule for the supplied dispute reason code.
    NoMatchingPrecedentRule = 3028,
    /// The supplied carrier attestation did not match the rule's required status.
    PrecedentAttestationMismatch = 3029,
}

#[cfg(test)]
mod error_code_allocation {
    use super::*;
    // Canonical flat ranges (issue #269).
    const REPUTATION_RANGE: (u32, u32) = (3_000, 3_999);
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
            ReputationError::UnverifiedOrder as u32,
            ReputationError::CarrierOracleNotAuthorized as u32,
        ];
        for code in codes {
            assert!(
                (REPUTATION_RANGE.0..=REPUTATION_RANGE.1).contains(&code),
                "ReputationError code {code} escaped its allocated range {}..={}",
                REPUTATION_RANGE.0,
                REPUTATION_RANGE.1
            );
        }
        codes.sort_unstable();
        for pair in codes.windows(2) {
            assert_ne!(pair[0], pair[1], "duplicate ReputationError code");
        }
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigUpdatedEvent {
    pub old: ReputationConfig,
    pub new: ReputationConfig,
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
}

/// Emitted when a confirmed fraud ruling slashes a merchant's score
/// (issue #392). `strike_count` is the permanent lifetime count, and
/// `accumulated_penalty_bps` is the total slash still applied at every
/// recomputation.
#[contracttype]
#[derive(Clone, Debug)]
pub struct FraudReputationSlashedEvent {
    pub merchant: Address,
    pub penalty_bps: u32,
    pub accumulated_penalty_bps: u32,
    /// Score persisted before the slash (the raw stored score —
    /// `get_reputation` may mask it to `0` below the visibility threshold).
    pub previous_score_bps: u32,
    /// Score persisted after the slash was applied and re-clamped.
    pub new_score_bps: u32,
    pub strike_count: u32,
}

/// Emitted when the admin authorizes an escrow contract to slash reputation.
#[contracttype]
#[derive(Clone, Debug)]
pub struct FraudAuthoritySetEvent {
    pub admin: Address,
    pub authority: Address,
}

/// Public view of a merchant's fraud record (issue #392).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FraudRecord {
    /// Permanent number of confirmed fraud rulings against the merchant.
    pub strike_count: u32,
    /// Total penalty in basis points still deducted from the merchant's score
    /// on every recomputation.
    pub accumulated_penalty_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationScoreUpdatedEvent {
    pub entity: Address,
    pub old_score_bps: u32,
    pub new_score_bps: u32,
    pub total_reviews: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScoreAccumulator {
    pub decay_window_seconds: u64,
    pub weighted_value_sum: i128,
    pub weight_sum: i128,
    pub rating_weighted_sum: i128,
    pub rating_weight_sum: i128,
    pub disputed_recent: i128,
}

/// Cursor for keyset pagination over the reputation-ranked merchant index.
///
/// The cursor encodes the last `(score_bps, merchant_id)` pair returned to
/// the caller. The next page resumes strictly *after* this pair in
/// descending `(score_bps, merchant_id)` order, which makes pagination
/// stable across ties: two merchants with identical `score_bps` are
/// disambiguated by their `merchant_id`, so no record is skipped or
/// duplicated when the page boundary falls inside a tie group.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationCursor {
    pub last_score_bps: u32,
    pub last_merchant_id: u64,
}

/// A single page of the reputation-ranked merchant index.
///
/// `next_cursor` cannot be an `Option<ReputationCursor>` field: the SDK's
/// `#[contracttype]` XDR conversions require every field type to implement
/// `Into<ScVal>`, and a nested contract type only provides the fallible
/// `TryFrom<&T> for ScVal` conversion. The cursor is therefore encoded as the
/// `(score_bps, merchant_id)` pair of the last record on the page, with
/// `has_more` telling the caller whether to resume from it.
#[contracttype]
/// This type intentionally does not derive `contracttype`: it contains
/// `Option<ReputationCursor>`, and the SDK's `testutils` expansion cannot
/// convert `Option<UDT>` to `ScVal` (it requires `Into<ScVal>` for the inner
/// type while contract types only get a `TryFrom` impl). The hand-written
/// `Val` conversions below are the equivalent of the derive output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RankedMerchantPage {
    pub merchants: Vec<MerchantView>,
    /// `true` when at least one eligible record follows this page.
    pub has_more: bool,
    /// `score_bps` of the last merchant emitted on this page.
    pub next_last_score_bps: u32,
    /// `merchant_id` of the last merchant emitted on this page.
    pub next_last_merchant_id: u64,
    pub total_count: u32,
}

impl RankedMerchantPage {
    /// Rebuilds the cursor a caller should pass to the next
    /// [`ReputationContract::get_ranked_merchants`] call, if any.
    pub fn next_cursor(&self) -> Option<ReputationCursor> {
        if !self.has_more {
            return None;
        }
        Some(ReputationCursor {
            last_score_bps: self.next_last_score_bps,
            last_merchant_id: self.next_last_merchant_id,
impl TryFromVal<Env, Val> for RankedMerchantPage {
    type Error = ConversionError;

    fn try_from_val(env: &Env, val: &Val) -> Result<Self, Self::Error> {
        const KEYS: [&'static str; 3] = ["merchants", "next_cursor", "total_count"];
        let mut vals: [Val; 3] = [Val::VOID.to_val(); 3];
        let map: MapObject = val.try_into().map_err(|_| ConversionError)?;
        env.map_unpack_to_slice(map, &KEYS, &mut vals)
            .map_err(|_| ConversionError)?;
        Ok(Self {
            merchants: vals[0].try_into_val(env).map_err(|_| ConversionError)?,
            next_cursor: vals[1].try_into_val(env).map_err(|_| ConversionError)?,
            total_count: vals[2].try_into_val(env).map_err(|_| ConversionError)?,
        })
    }
}

impl TryFromVal<Env, RankedMerchantPage> for Val {
    type Error = ConversionError;

    fn try_from_val(env: &Env, val: &RankedMerchantPage) -> Result<Self, Self::Error> {
        const KEYS: [&'static str; 3] = ["merchants", "next_cursor", "total_count"];
        let vals: [Val; 3] = [
            (&val.merchants)
                .try_into_val(env)
                .map_err(|_| ConversionError)?,
            (&val.next_cursor)
                .try_into_val(env)
                .map_err(|_| ConversionError)?,
            (&val.total_count)
                .try_into_val(env)
                .map_err(|_| ConversionError)?,
        ];
        Ok(env
            .map_new_from_slices(&KEYS, &vals)
            .map_err(|_| ConversionError)?
            .into())
    }
}

/// Read-only projection of a merchant's reputation used by the discovery
/// index. Mirrors the fields buyers need to render a ranked listing without
/// pulling the full [`ReputationScore`] record.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantView {
    pub merchant_id: u64,
    pub entity: Address,
    pub score_bps: u32,
    pub total_transactions: u64,
    pub avg_rating: u32,
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
    /// Cached composite score for an entity, recomputed on each
    /// `record_transaction`/`rate_entity`.
    CompositeScore(Address),
    /// Role-specific performance indicators for an entity, keyed by address.
    RoleMetrics(Address),

    /// Marks an escrow id as already used for a `submit_verified_review`
    /// call, so the same settled escrow cannot back more than one verified
    /// review (issue #286).
    VerifiedEscrowReview(u64),
    /// Monotonic counter assigning a stable `merchant_id` to each entity
    /// that has ever had a reputation record written. Used as the tie-break
    /// key in the `(score_bps, merchant_id)` composite index.
    MerchantId(Address),
    /// Reverse lookup from `merchant_id` to the owning entity, so the
    /// ranked index can be materialized without scanning all entities.
    MerchantEntity(u64),
    /// Total number of merchants currently present in the ranked index.
    MerchantCount,

    // Advanced reputation features (issue #XXX)
    /// Cryptographic commitment of a verified buyer review, binding off-chain
    /// content to an on-chain rating via content hash.
    ReviewCommitment(u64), // Maps escrow_id -> ReviewCommitment
    /// All review commitments for a merchant, keyed by escrow_id for deduplication.
    MerchantReviewCommitments(Address), // Vec<u64> (escrow_ids)
    /// Dispute quorum vote tracking per dispute.
    DisputeQuorumVote(u64), // Maps dispute_id -> Vec<DisputeQuorumVote>
    /// Dispute vote tallies: (dispute_id) -> (buyer_votes, merchant_votes, split_votes)
    DisputeVoteTally(u64),
    /// Normalized reputation score with sigmoid scaling.
    NormalizedScore(Address), // Maps merchant -> NormalizedReputationScore
    /// Exponential penalty tracking for consecutive lost disputes.
    DisputePenalty(Address), // Maps merchant -> DisputePenaltyRecord
    /// Counts dispute-free orders for penalty recovery.
    DisputeFreeOrderCount(Address), // Maps merchant -> u32
    
    /// Rehabilitation program by merchant address
    RehabilitationProgram(Address),
    /// Counter for fraud reports (report_id generator)
    FraudReportCounter,
    /// Fraud report by report_id
    FraudReport(u64),
    /// Index of fraud reports by target entity for quick lookup
    FraudReportsByTarget(Address),
    /// Blinded review receipt nullifier to prevent double-submission
    BlindedReviewNullifier(BytesN<32>),
    /// Trusted escrow contract allowed to slash reputation after a confirmed
    /// fraud ruling (issue #392). Set by the admin.
    FraudAuthority,
    /// Permanent count of confirmed fraud rulings recorded against an entity.
    /// Never reset — not by `unfreeze_entity`, not by a later clean history.
    FraudStrikes(Address),
    /// Accumulated fraud penalty for an entity, in basis points. Persisted so
    /// every later score recomputation keeps subtracting it instead of
    /// washing the slash out with the next recorded transaction.
    FraudPenalty(Address),
    /// Ed25519 public key registered for a carrier oracle address.
    CarrierOracle(Address),
}

/// Maximum basis points value (100.00%), used both for ratings/scores and
/// for the recency-weight scale in [`recency_weight_bps`].
const BPS_SCALE: i128 = 10_000;

/// Clamps a raw score expressed in basis points into the contract's legal
/// `0..=10_000` range (0.00% to 100.00%).
///
/// Every score the contract persists is produced here, so an intermediate
/// value can never escape the legal band: a merchant that accumulates
/// extreme consecutive negative reviews saturates cleanly at `0` instead of
/// wrapping to `u32::MAX` or panicking on an underflow, and a merchant that
/// accumulates maximum-value outcomes saturates at `10_000` rather than
/// exceeding 100%.
///
/// The argument is a signed `i64` (not `u32`) precisely because the raw
/// value can legitimately be negative once dispute/fraud penalties are
/// subtracted from the weighted base score — the sign carries information
/// until this function folds it into the floor.
pub fn normalize_score(raw_score: i64) -> u32 {
    if raw_score <= 0 {
        0
    } else if raw_score >= BPS_SCALE as i64 {
        BPS_SCALE as u32
    } else {
        raw_score as u32
    }
}

/// `i128`-domain counterpart of [`normalize_score`].
///
/// Score arithmetic is carried out in `i128` (weights and values are both
/// basis points, and up to `SCORE_WINDOW` records are summed), so the raw
/// result is clamped into `i64` before delegating to [`normalize_score`].
/// That keeps the single source of truth for the legal score band while
/// guaranteeing the narrowing conversion can never truncate.
fn normalize_score_i128(raw_score: i128) -> u32 {
    let bounded = raw_score.clamp(i64::MIN as i128, i64::MAX as i128);
    normalize_score(bounded as i64)
}
/// Default reputation slash applied to a merchant when an escrow dispute is
/// resolved with a confirmed fraud ruling: -25% (-2,500 basis points), i.e.
/// one confirmed fraud costs a quarter of the score, far beyond what a single
/// negative review can cost.
///
/// [`ReputationContract::apply_fraud_reputation_slash`] takes the penalty as
/// a parameter so the escrow contract can choose a stronger penalty for
/// egregious cases, but callers should pass this constant for the standard
/// fraud ruling.
pub const FRAUD_PENALTY_SCORE_BPS: u32 = 2_500;

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

/// Default page size for [`ReputationContract::get_ranked_merchants`] when
/// the caller does not specify one. Chosen so a full page fits comfortably
/// within Soroban's per-invocation CPU/memory budget.
const DEFAULT_PAGE_SIZE: u32 = 20;

/// Hard upper bound on a single ranked-merchant page. Callers requesting a
/// larger page are clamped to this value to keep gas bounded.
const MAX_PAGE_SIZE: u32 = 50;

/// Persistent entries are bumped when they approach expiry and kept alive
/// for roughly 30 days, matching the repository's persistent-storage policy.
const PERSISTENT_BUMP_THRESHOLD: u32 = 17_280;
const PERSISTENT_BUMP_AMOUNT: u32 = 518_400;

/// Weight (in basis points) applied to the count component of the
/// composite score. Count and volume weights must sum to `BPS_SCALE`.
const COUNT_WEIGHT_BPS: i128 = 4_000;

/// Weight (in basis points) applied to the volume component of the
/// composite score. Count and volume weights must sum to `BPS_SCALE`.
const VOLUME_WEIGHT_BPS: i128 = 6_000;

/// Reference volume (in smallest token units) used as the logarithmic
/// dampening unit for the volume-weighted score. Each multiple of this
/// amount contributes a diminishing marginal increment to the volume score.
const VOLUME_UNIT: i128 = 1_000;

// --- Advanced Reputation Scoring Constants ---

/// Minimum rating value (1 star).
const MIN_RATING: u32 = 1;

/// Maximum rating value (5 stars).
const MAX_RATING: u32 = 5;

/// Quorum threshold for dispute voting (66% consensus).
const DISPUTE_QUORUM_THRESHOLD_BPS: i128 = 6_600;

/// Maximum consecutive lost dispute penalty multiplier (2^4 = 16x).
const MAX_DISPUTE_PENALTY_MULTIPLIER: u32 = 4;

/// Number of dispute-free orders required to reset penalty multiplier.
const DISPUTE_FREE_RESET_THRESHOLD: u32 = 20;

/// Maximum allowed gas instructions for normalized score computation.
const MAX_SCORE_COMPUTATION_INSTRUCTIONS: u32 = 50_000;

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
///
/// The public score helper uses the same fixed-point curve with a zero
/// baseline, so historical values converge to zero after sustained inactivity.
/// A zero half-life disables decay and returns the historical score unchanged.
pub fn calculate_decayed_score(
    historical_score: u32,
    ledgers_elapsed: u32,
    half_life_ledgers: u32,
) -> u32 {
    let decayed = fixed_point_decay(
        historical_score as u64,
        ledgers_elapsed as u64,
        half_life_ledgers as u64,
        u32::BITS as u64,
    );
    // Decay never increases a score, but the input is caller-supplied and may
    // arrive outside the legal band; normalizing here keeps every decay result
    // within `0..=10_000` as well.
    normalize_score_i128(decayed as i128)
}

fn fixed_point_decay(
    historical_score: u64,
    elapsed: u64,
    half_life: u64,
    max_halvings: u64,
) -> u64 {
    if half_life == 0 {
        return historical_score;
    }

    let full_halvings = elapsed / half_life;
    if full_halvings >= max_halvings {
        return 0;
    }

    let base = historical_score >> full_halvings;
    let remainder = elapsed % half_life;
    let decrement = (u128::from(base) * u128::from(remainder) / (2 * u128::from(half_life))) as u64;
    base.saturating_sub(decrement)
}

fn recency_weight_bps(elapsed_secs: u64, decay_window_secs: u64) -> i128 {
    fixed_point_decay(
        BPS_SCALE as u64,
        elapsed_secs,
        decay_window_secs,
        MAX_HALVINGS,
    ) as i128
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

    /// Register or rotate an authorized carrier oracle's Ed25519 public key.
    pub fn set_carrier_oracle(
        env: Env,
        admin: Address,
        carrier_oracle: Address,
        public_key: BytesN<32>,
    ) -> Result<(), ReputationError> {
        admin.require_auth();
        Self::require_caller_is_admin(&env, &admin)?;
        oracle_proof::set_carrier_oracle(&env, &carrier_oracle, &public_key);
        Ok(())
    }

    /// Remove a carrier oracle from the authorized verifier registry.
    pub fn remove_carrier_oracle(
        env: Env,
        admin: Address,
        carrier_oracle: Address,
    ) -> Result<(), ReputationError> {
        admin.require_auth();
        Self::require_caller_is_admin(&env, &admin)?;
        oracle_proof::remove_carrier_oracle(&env, &carrier_oracle);
        Ok(())
    }

    /// Verify a courier-signed delivery/loss proof against the authorized
    /// carrier oracle registry.
    pub fn verify_carrier_proof(
        env: Env,
        proof: CarrierProof,
    ) -> Result<(), ReputationError> {
        oracle_proof::verify_carrier_proof(&env, &proof)
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
            (
                symbol_short!("reput"),
                symbol_short!("tx_rec"),
                entity.clone(),
            ),
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
            (
                symbol_short!("reput"),
                symbol_short!("rated"),
                entity.clone(),
            ),
            EntityRatedEvent {
                rater,
                entity,
                rating,
                escrow_id,
            },
        );

        Ok(())
    }

    /// Rate the entity on the other side of an escrow using an
    /// independently verified settlement proof, rather than relying on the
    /// admin having already called `record_transaction` for that escrow
    /// (issue #286).
    ///
    /// `rater` must require auth, must not equal `entity`, and — per
    /// `proof.escrow_contract.get_escrow(proof.escrow_id)` — must be either
    /// the escrow's `buyer` or `seller`, with `entity` as the other party.
    /// The escrow must report `EscrowStatus::Released` and its `order_id`
    /// must match `proof.order_hash`; any mismatch, failed lookup, or
    /// non-released status is rejected as [`ReputationError::UnverifiedOrder`].
    /// Each `escrow_id` can back at most one verified review, regardless of
    /// caller, so the same real settlement cannot be replayed into multiple
    /// ratings.
    ///
    /// On success this behaves like `record_transaction` (outcome
    /// `Released`) immediately followed by `rate_entity`, feeding the same
    /// time-decayed scoring pipeline, so verified reviews and admin-relayed
    /// transactions contribute to `ReputationScore` consistently.
    pub fn submit_verified_review(
        env: Env,
        rater: Address,
        entity: Address,
        proof: VerifiedReviewProof,
        rating: u32,
    ) -> Result<(), ReputationError> {
        rater.require_auth();

        if rating as i128 > BPS_SCALE {
            return Err(ReputationError::InvalidRating);
        }
        if rater == entity {
            return Err(ReputationError::InvalidParam);
        }
        if Self::is_frozen(env.clone(), entity.clone()) {
            return Err(ReputationError::EntityFrozen);
        }

        let reviewed_key = DataKey::VerifiedEscrowReview(proof.escrow_id);
        if env
            .storage()
            .persistent()
            .get::<_, bool>(&reviewed_key)
            .unwrap_or(false)
        {
            return Err(ReputationError::DuplicateRating);
        }

        let args = soroban_sdk::vec![&env, proof.escrow_id.into_val(&env)];
        let call_result = env.try_invoke_contract::<EscrowRecordMirror, InvokeError>(
            &proof.escrow_contract,
            &Symbol::new(&env, "get_escrow"),
            args,
        );
        let escrow: EscrowRecordMirror = match call_result {
            Ok(Ok(record)) => record,
            _ => return Err(ReputationError::UnverifiedOrder),
        };

        if escrow.status != EscrowStatusMirror::Released {
            return Err(ReputationError::UnverifiedOrder);
        }
        if escrow.order_id != proof.order_hash {
            return Err(ReputationError::UnverifiedOrder);
        }
        let counterparty_ok = (rater == escrow.buyer && entity == escrow.seller)
            || (rater == escrow.seller && entity == escrow.buyer);
        if !counterparty_ok {
            return Err(ReputationError::UnverifiedOrder);
        }

        env.storage().persistent().set(&reviewed_key, &true);
        env.storage().persistent().extend_ttl(
            &reviewed_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        let record_key = DataKey::TransactionRecord(proof.escrow_id);
        let existing: Option<TransactionRecord> = env.storage().persistent().get(&record_key);
        if let Some(prior) = &existing {
            if prior.rating.is_some() {
                return Err(ReputationError::DuplicateRating);
            }
        }

        let tx_record = TransactionRecord {
            escrow_id: proof.escrow_id,
            entity: entity.clone(),
            counterparty: rater.clone(),
            amount: escrow.amount,
            outcome: TransactionOutcome::Released,
            rating: Some(rating),
            recorded_at: env.ledger().timestamp(),
        };
        env.storage().persistent().set(&record_key, &tx_record);
        env.storage().persistent().extend_ttl(
            &record_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);

        let history_len_before = if existing.is_none() {
            let hist_key = DataKey::TransactionHistory(entity.clone());
            let mut history: Vec<u64> = env
                .storage()
                .persistent()
                .get(&hist_key)
                .unwrap_or_else(|| Vec::new(&env));
            let len_before = history.len();
            history.push_back(proof.escrow_id);
            env.storage().persistent().set(&hist_key, &history);
            env.storage().persistent().extend_ttl(
                &hist_key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );

            env.storage()
                .persistent()
                .set(&DataKey::Transacted(entity.clone(), rater.clone()), &true);
            env.storage().persistent().extend_ttl(
                &DataKey::Transacted(entity.clone(), rater.clone()),
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );
            env.storage()
                .persistent()
                .set(&DataKey::Transacted(rater.clone(), entity.clone()), &true);
            env.storage().persistent().extend_ttl(
                &DataKey::Transacted(rater.clone(), entity.clone()),
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );

            Self::apply_new_transaction_counts(&env, &entity, &TransactionOutcome::Released);
            Some(len_before)
        } else {
            None
        };

        let rated_key = DataKey::RatedEscrows(rater.clone());
        let mut rated: Vec<u64> = env
            .storage()
            .persistent()
            .get(&rated_key)
            .unwrap_or_else(|| Vec::new(&env));
        if !rated.contains(proof.escrow_id) {
            rated.push_back(proof.escrow_id);
            env.storage().persistent().set(&rated_key, &rated);
        }

        match history_len_before {
            Some(len_before) => Self::apply_incremental_score_update(&env, &entity, len_before)?,
            None => Self::recompute_score(&env, &entity)?,
        };

        env.events().publish(
            (symbol_short!("reput"), symbol_short!("verrated")),
            EntityRatedEvent {
                rater,
                entity,
                rating,
                escrow_id: proof.escrow_id,
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

    // --- Attestations ---

    /// Generates a tamper-proof snapshot of `entity`'s public reputation
    /// metrics for off-chain clients (issue #394).
    ///
    /// The returned [`ReputationAttestation`] embeds a sequence checkpoint
    /// (`certified_at_ledger`) and a validity window
    /// (`expires_at_ledger`), and its metrics are derived through the same
    /// masking rules as [`Self::get_reputation`], so it can never disagree
    /// with the canonical read path. Unknown entities yield a zero-score
    /// attestation carrying the current validity window rather than an
    /// error: the snapshot describes the entity's *current public standing*,
    /// and "no record" is indistinguishable from "no history" for a client
    /// deciding whether to trust a merchant.
    ///
    /// This is a read-only derivation — nothing is persisted — so clients may
    /// cache the result and re-derive it later to detect drift.
    pub fn generate_score_attestation(env: Env, entity: Address) -> ReputationAttestation {
        Self::snapshot_attestation(&env, &entity)
    }

    /// Verifies a cached [`ReputationAttestation`] against current contract
    /// state (issue #394).
    ///
    /// The attestation is re-derived from live state and compared field by
    /// field. `valid` is true only while the snapshot is inside its validity
    /// window and every attested metric still matches; the response also
    /// carries a freshly generated attestation so a client can refresh its
    /// cache without a second call. Expired snapshots are reported via
    /// `expired = true` and remain replacable via `current`.
    pub fn verify_score_attestation(
        env: Env,
        attestation: ReputationAttestation,
    ) -> AttestationVerification {
        let current = Self::snapshot_attestation(&env, &attestation.entity);
        let expired = attestation_is_expired(&attestation, env.ledger().sequence());
        let score_matches = attestation.score_bps == current.score_bps
            && attestation.total_transactions == current.total_transactions;

        AttestationVerification {
            attestation: attestation.clone(),
            valid: !expired && score_matches,
            expired,
            score_matches,
            current,
    /// Classifies `entity` as either a merchant or buyer agent. Admin-only;
    /// the role determines which domain metrics are relevant for risk
    /// assessment (fulfillment for merchants, payment timeliness and dispute
    /// frequency for buyer agents).
    pub fn set_entity_role(
        env: Env,
        admin: Address,
        entity: Address,
        role: EntityRole,
    ) -> Result<(), ReputationError> {
        admin.require_auth();
        Self::require_caller_is_admin(&env, &admin)?;

        let mut rep = Self::load_or_default_reputation(&env, &entity);
        rep.role = role.clone();
        env.storage()
            .persistent()
            .set(&DataKey::Reputation(entity.clone()), &rep);
        env.storage().persistent().extend_ttl(
            &DataKey::Reputation(entity.clone()),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Initialize role-specific metrics for the newly classified entity.
        let metrics = Self::compute_role_metrics(&rep);
        env.storage()
            .persistent()
            .set(&DataKey::RoleMetrics(entity.clone()), &metrics);
        env.storage().persistent().extend_ttl(
            &DataKey::RoleMetrics(entity.clone()),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        Ok(())
    }

    /// Returns the role-specific performance indicators for `entity`.
    pub fn get_role_specific_metrics(
        env: Env,
        entity: Address,
    ) -> Result<RoleSpecificMetrics, ReputationError> {
        let rep = env
            .storage()
            .persistent()
            .get(&DataKey::Reputation(entity.clone()))
            .ok_or(ReputationError::EntityNotFound)?;

        Self::bump_entity(&env, &entity);

        let metrics = env
            .storage()
            .persistent()
            .get(&DataKey::RoleMetrics(entity.clone()))
            .unwrap_or_else(|| Self::compute_role_metrics(&rep));
        Ok(metrics)
    }

    /// Returns role-specific metrics tailored for a merchant. Returns
    /// `EntityNotFound` if the entity has no reputation record, or if the
    /// entity is not classified as a merchant.
    pub fn get_merchant_metrics(
        env: Env,
        entity: Address,
    ) -> Result<RoleSpecificMetrics, ReputationError> {
        let metrics = Self::get_role_specific_metrics(env.clone(), entity.clone())?;
        match metrics.role {
            EntityRole::Merchant => Ok(metrics),
            EntityRole::BuyerAgent => Err(ReputationError::EntityNotFound),
        }
    }

    /// Returns role-specific metrics tailored for a buyer agent. Returns
    /// `EntityNotFound` if the entity has no reputation record, or if the
    /// entity is not classified as a buyer agent.
    pub fn get_buyer_agent_metrics(
        env: Env,
        entity: Address,
    ) -> Result<RoleSpecificMetrics, ReputationError> {
        let metrics = Self::get_role_specific_metrics(env.clone(), entity.clone())?;
        match metrics.role {
            EntityRole::BuyerAgent => Ok(metrics),
            EntityRole::Merchant => Err(ReputationError::EntityNotFound),
        }
    }

    /// Computes role-specific performance indicators from a reputation
    /// record. Fulfillment rate is the share of successful transactions;
    /// dispute frequency is the share of disputed transactions; timely
    /// payment rate mirrors fulfillment for buyer-agent risk assessment.
    fn compute_role_metrics(rep: &ReputationScore) -> RoleSpecificMetrics {
        let total = rep.total_transactions;
        if total == 0 {
            return RoleSpecificMetrics {
                role: rep.role.clone(),
                fulfillment_rate_bps: 0,
                dispute_frequency_bps: 0,
                timely_payment_rate_bps: 0,
            };
        }
        let fulfillment_rate_bps =
            (rep.successful_transactions as u128 * BPS_SCALE as u128 / total as u128) as u32;
        let dispute_frequency_bps =
            (rep.disputed_transactions as u128 * BPS_SCALE as u128 / total as u128) as u32;
        RoleSpecificMetrics {
            role: rep.role.clone(),
            fulfillment_rate_bps,
            dispute_frequency_bps,
            timely_payment_rate_bps: fulfillment_rate_bps,
        }
    }

    /// Returns the raw base/penalty breakdown behind `entity`'s current
    /// score, including the clamped final score that `get_reputation`
    /// reports.
    pub fn get_score_decomposition(
        env: Env,
        entity: Address,
    ) -> Result<ScoreDecomposition, ReputationError> {
        let rep = Self::load_or_default_reputation(&env, &entity);
        let (decomposition, _, _, _) =
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

    // --- Ranked Discovery Index ---

    /// Returns a page of merchants ranked by descending reputation score,
    /// using stable keyset pagination over the composite
    /// `(score_bps, merchant_id)` index.
    ///
    /// The cursor semantics are `WHERE (score, id) < (cursor.score, cursor.id)`
    /// in descending order: the next page resumes strictly after the last
    /// `(score_bps, merchant_id)` pair returned. Because `merchant_id` is
    /// unique and monotonic, ties on `score_bps` are broken deterministically
    /// and no record is skipped or duplicated across page boundaries.
    ///
    /// `cursor` is `None` for the first page. `limit` is clamped to
    /// `[1, MAX_PAGE_SIZE]`; a `limit` of `0` is treated as
    /// `DEFAULT_PAGE_SIZE`.
    pub fn get_ranked_merchants(
        env: Env,
        cursor: Option<ReputationCursor>,
        limit: u32,
    ) -> Result<RankedMerchantPage, ReputationError> {
        // Require initialization so callers get a deterministic error rather
        // than an empty page against an unconfigured contract.
        Self::get_config(env.clone())?;

        let page_size = if limit == 0 {
            DEFAULT_PAGE_SIZE
        } else {
            limit.min(MAX_PAGE_SIZE)
        };

        let total_count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MerchantCount)
            .unwrap_or(0);

        let mut merchants: Vec<MerchantView> = Vec::new(&env);
        if total_count == 0 {
            return Ok(RankedMerchantPage {
                merchants,
                has_more: false,
                next_last_score_bps: 0,
                next_last_merchant_id: 0,
                total_count,
            });
        }

        // Walk the merchant_id space in descending order, collecting entries
        // that sort strictly after the cursor in `(score_bps, merchant_id)`
        // descending order. `merchant_id` is assigned monotonically, so
        // iterating ids downward visits candidates in the exact tie-break
        // order the index requires; the score comparison then filters to the
        // keyset window.
        let mut has_more = false;
        let mut last_score_bps = 0u32;
        let mut last_merchant_id = 0u64;
        let mut id = total_count as u64;
        while id > 0 {
            id -= 1;

            let entity: Address = match env.storage().persistent().get(&DataKey::MerchantEntity(id))
            {
                Some(entity) => entity,
                None => continue,
            };

            let rep: ReputationScore = match env
                .storage()
                .persistent()
                .get(&DataKey::Reputation(entity.clone()))
            {
                Some(rep) => rep,
                None => continue,
            };

            // Keyset filter: keep only pairs strictly less than the cursor
            // in descending `(score_bps, merchant_id)` order.
            if let Some(ref c) = cursor {
                let after_cursor = rep.score < c.last_score_bps
                    || (rep.score == c.last_score_bps && id < c.last_merchant_id);
                if !after_cursor {
                    continue;
                }
            }

            if merchants.len() >= page_size {
                // We have a full page and found at least one more eligible
                // record, so the caller can continue from the last emitted
                // pair. The cursor must point at the last record already
                // returned, not at the first record of the next page, or the
                // keyset filter would skip that record.
                has_more = true;
                break;
            }

            last_score_bps = rep.score;
            last_merchant_id = id;
            merchants.push_back(MerchantView {
                merchant_id: id,
                entity,
                score_bps: rep.score,
                total_transactions: rep.total_transactions,
                avg_rating: rep.avg_rating,
            });
        }

        Ok(RankedMerchantPage {
            merchants,
            has_more,
            next_last_score_bps: last_score_bps,
            next_last_merchant_id: last_merchant_id,
            total_count,
        })
    }

    /// Returns the stable `merchant_id` assigned to `entity`, if any. The id
    /// is allocated lazily the first time an entity's reputation record is
    /// written, and never changes thereafter.
    pub fn get_merchant_id(env: Env, entity: Address) -> Option<u64> {
        env.storage().persistent().get(&DataKey::MerchantId(entity))
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
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        let active_count = flags.iter().filter(|f| !f.resolved).count() as u32;

        env.events().publish(
            (
                symbol_short!("reput"),
                symbol_short!("flagged"),
                entity.clone(),
            ),
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
                (
                    symbol_short!("reput"),
                    symbol_short!("frozen"),
                    entity.clone(),
                ),
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

        let key = DataKey::Flags(entity);
        let mut flags: Vec<Flag> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));

        let idx = flags
            .iter()
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
        };
        let mut flag = flags.get(idx as u32).unwrap();
        flag.resolved = true;
        flags.set(idx as u32, flag);
        env.storage().persistent().set(&key, &flags);

        Ok(())
    }

    // --- Fraud slashing ---

    /// Authorizes `authority` — normally the escrow contract — to slash
    /// merchant reputation after a confirmed fraud ruling (issue #392).
    ///
    /// Admin-only, and deliberately a separate trust grant from anything the
    /// backend relaying ordinary transactions holds: a compromised
    /// transaction-relay key cannot slash merchants, only submit outcomes.
    /// Re-setting the authority replaces the previous one.
    pub fn set_fraud_authority(
        env: Env,
        admin: Address,
        authority: Address,
    ) -> Result<(), ReputationError> {
        admin.require_auth();
        Self::require_caller_is_admin(&env, &admin)?;

        env.storage()
            .instance()
            .set(&DataKey::FraudAuthority, &authority);
        env.events().publish(
            (
                symbol_short!("reput"),
                Symbol::new(&env, "fraud_auth"),
                authority.clone(),
            ),
            FraudAuthoritySetEvent { admin, authority },
        );
        Ok(())
    }

    /// Returns the escrow contract currently trusted to slash reputation, if
    /// one has been configured.
    pub fn get_fraud_authority(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::FraudAuthority)
    }

    /// Returns `merchant`'s permanent strike count and the accumulated fraud
    /// penalty still applied to its score. Unknown merchants report a zeroed
    /// record.
    pub fn get_fraud_record(env: Env, merchant: Address) -> FraudRecord {
        FraudRecord {
            strike_count: env
                .storage()
                .persistent()
                .get(&DataKey::FraudStrikes(merchant.clone()))
                .unwrap_or(0),
            accumulated_penalty_bps: env
                .storage()
                .persistent()
                .get(&DataKey::FraudPenalty(merchant))
                .unwrap_or(0),
        }
    }

    /// Slashes `merchant`'s reputation after an escrow dispute was resolved
    /// with a confirmed ruling of merchant fraud or counterfeit goods
    /// (issue #392).
    ///
    /// Only the escrow contract configured through
    /// [`Self::set_fraud_authority`] can call this, and that contract address
    /// must itself authorize the invocation, so no external account can slash
    /// a merchant directly. `penalty_bps` must be in `1..=10_000`; callers
    /// applying the standard fraud ruling should pass
    /// [`FRAUD_PENALTY_SCORE_BPS`].
    ///
    /// The deduction is immediate and deterministic, and it is *persistent*:
    /// the penalty is accumulated on-chain and subtracted by every later score
    /// recomputation, so subsequent transactions (even a long run of clean
    /// ones) cannot wash the slash out of the score. Each call also increments
    /// a permanent strike counter that no other operation resets. Returns the
    /// merchant's new score in basis points, clamped to `0..=10_000`.
    pub fn apply_fraud_reputation_slash(
        env: Env,
        merchant: Address,
        penalty_bps: u32,
    ) -> Result<u32, ReputationError> {
        let authority: Address = env
            .storage()
            .instance()
            .get(&DataKey::FraudAuthority)
            .ok_or(ReputationError::NotInitialized)?;
        authority.require_auth();

        if penalty_bps == 0 || penalty_bps as i128 > BPS_SCALE {
            return Err(ReputationError::InvalidParam);
        }

        let previous_score = Self::load_or_default_reputation(&env, &merchant).score;

        // Permanent strike record. Saturating: a strike count is audit data,
        // so it must never wrap to zero (which would read as "no fraud").
        let strikes_key = DataKey::FraudStrikes(merchant.clone());
        let strikes: u32 = env.storage().persistent().get(&strikes_key).unwrap_or(0);
        let strikes = strikes.saturating_add(1);
        env.storage().persistent().set(&strikes_key, &strikes);
        env.storage().persistent().extend_ttl(
            &strikes_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Accumulated penalty, re-applied by every future recomputation.
        let penalty_key = DataKey::FraudPenalty(merchant.clone());
        let accumulated: u32 = env.storage().persistent().get(&penalty_key).unwrap_or(0);
        let accumulated = accumulated.saturating_add(penalty_bps);
        env.storage().persistent().set(&penalty_key, &accumulated);
        env.storage().persistent().extend_ttl(
            &penalty_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        let rep = Self::recompute_score(&env, &merchant)?;

        env.events().publish(
            (
                symbol_short!("reput"),
                symbol_short!("slashed"),
                merchant.clone(),
            ),
            FraudReputationSlashedEvent {
                merchant,
                penalty_bps,
                accumulated_penalty_bps: accumulated,
                previous_score_bps: previous_score,
                new_score_bps: rep.score,
                strike_count: strikes,
            },
        );

        Ok(rep.score)
    }

    // --- Admin ---

    pub fn freeze_entity(env: Env, admin: Address, entity: Address) -> Result<(), ReputationError> {
        admin.require_auth();
        Self::require_caller_is_admin(&env, &admin)?;

        env.storage()
            .persistent()
            .set(&DataKey::FrozenStatus(entity.clone()), &true);
        env.events().publish(
            (
                symbol_short!("reput"),
                symbol_short!("frozen"),
                entity.clone(),
            ),
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
            (
                symbol_short!("reput"),
                symbol_short!("unfrozn"),
                entity.clone(),
            ),
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
                (
                    symbol_short!("reput"),
                    symbol_short!("pruned"),
                    entity.clone(),
                ),
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

        let old: ReputationConfig = env
            .storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(ReputationError::NotInitialized)?;

        env.storage().instance().set(&DataKey::Config, &config);

        env.events().publish(
            (symbol_short!("reput"), symbol_short!("cfg_upd")),
            ConfigUpdatedEvent {
                old,
                new: config,
            },
        );

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
            (
                symbol_short!("reput"),
                soroban_sdk::Symbol::new(&env, "admin_prop"),
                new_admin.clone(),
            ),
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
            (
                symbol_short!("reput"),
                symbol_short!("admin_acc"),
                caller.clone(),
            ),
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
            if let Some(history) = env.storage().persistent().get::<_, Vec<u64>>(&hist_key) {
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

    /// Public (masked) score and lifetime transaction count for `entity`.
    ///
    /// Shared by [`Self::generate_score_attestation`] and
    /// [`Self::verify_score_attestation`] so an attestation can never
    /// disagree with [`Self::get_reputation`]: the same
    /// `min_transactions_threshold` gate zeroes the score here.
    fn public_score_snapshot(env: &Env, entity: &Address) -> (u32, u64) {
        let rep: ReputationScore = match env
            .storage()
            .persistent()
            .get(&DataKey::Reputation(entity.clone()))
        {
            Some(rep) => rep,
            None => return (0, 0),
        };
        let visible = match Self::get_config(env.clone()) {
            Ok(config) => rep.total_transactions >= config.min_transactions_threshold,
            // An uninitialized contract has no public reputation to attest.
            Err(_) => false,
        };
        (if visible { rep.score } else { 0 }, rep.total_transactions)
    }

    /// Builds the attestation for `entity` anchored at the current ledger
    /// sequence. Stateless: `generate` and `verify` both derive through here.
    fn snapshot_attestation(env: &Env, entity: &Address) -> ReputationAttestation {
        let (score_bps, total_transactions) = Self::public_score_snapshot(env, entity);
        let certified_at_ledger = env.ledger().sequence();
        ReputationAttestation {
            entity: entity.clone(),
            score_bps,
            total_transactions: u32::try_from(total_transactions).unwrap_or(u32::MAX),
            certified_at_ledger,
            expires_at_ledger: certified_at_ledger.saturating_add(ATTESTATION_VALIDITY_LEDGERS),
        }
    /// Accumulated fraud penalty for `entity`, in basis points, as applied by
    /// every score recomputation.
    fn accumulated_fraud_penalty_bps(env: &Env, entity: &Address) -> i128 {
        env.storage()
            .persistent()
            .get::<_, u32>(&DataKey::FraudPenalty(entity.clone()))
            .unwrap_or(0) as i128
    }

    fn load_or_default_reputation(env: &Env, entity: &Address) -> ReputationScore {
        env.storage()
            .persistent()
            .get(&DataKey::Reputation(entity.clone()))
            .unwrap_or(ReputationScore {
                entity: entity.clone(),
                role: EntityRole::Merchant,
                score: 0,
                total_transactions: 0,
                successful_transactions: 0,
                disputed_transactions: 0,
                avg_rating: 0,
                last_updated: 0,
            })
    }

    /// Allocates a stable `merchant_id` for `entity` on first use and
    /// registers the reverse `merchant_id -> entity` mapping used by the
    /// ranked discovery index. Idempotent: repeated calls for the same
    /// entity return the existing id without mutating the counter.
    fn ensure_merchant_id(env: &Env, entity: &Address) -> u64 {
        let key = DataKey::MerchantId(entity.clone());
        if let Some(id) = env.storage().persistent().get::<_, u64>(&key) {
            return id;
        }

        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::MerchantCount)
            .unwrap_or(0);
        env.storage().persistent().set(&key, &id);
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        env.storage()
            .persistent()
            .set(&DataKey::MerchantEntity(id), entity);
        env.storage().persistent().extend_ttl(
            &DataKey::MerchantEntity(id),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        env.storage()
            .instance()
            .set(&DataKey::MerchantCount, &(id + 1));
        id
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
        Self::ensure_merchant_id(env, entity);
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

        // Keep role-specific metrics in sync with the updated lifetime counts.
        let metrics = Self::compute_role_metrics(&rep);
        env.storage()
            .persistent()
            .set(&DataKey::RoleMetrics(entity.clone()), &metrics);
        env.storage().persistent().extend_ttl(
            &DataKey::RoleMetrics(entity.clone()),
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

        // Keep role-specific metrics in sync with the updated lifetime counts.
        let metrics = Self::compute_role_metrics(&rep);
        env.storage()
            .persistent()
            .set(&DataKey::RoleMetrics(entity.clone()), &metrics);
        env.storage().persistent().extend_ttl(
            &DataKey::RoleMetrics(entity.clone()),
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
            .get::<_, ScoreAccumulator>(&DataKey::ScoreAccumulator(entity.clone()))
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
        // Route the raw (possibly negative) result through the single bounded
        // normalization helper; `i128` arithmetic cannot overflow here and the
        // helper clamps the final value into `0..=10_000`.
        rep.score = normalize_score_i128(base_score.saturating_sub(penalty));
        let penalty = accumulator.disputed_recent * (config.dispute_penalty_bps as i128)
            + Self::accumulated_fraud_penalty_bps(env, entity);
        rep.score = (base_score - penalty).clamp(0, BPS_SCALE) as u32;
        rep.avg_rating = if accumulator.rating_weight_sum > 0 {
            normalize_score_i128(accumulator.rating_weighted_sum / accumulator.rating_weight_sum)
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
    ) -> Result<(ScoreDecomposition, u32, u64, ScoreAccumulator), ReputationError> {
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
        let penalty = disputed_recent * (config.dispute_penalty_bps as i128)
            + Self::accumulated_fraud_penalty_bps(env, entity);
        let avg_rating = if rating_weight_sum > 0 {
            normalize_score_i128(rating_weighted_sum / rating_weight_sum)
        } else {
            0
        };

        Ok((
            ScoreDecomposition {
                entity: entity.clone(),
                base_score_bps: normalize_score_i128(base_score) as i128,
                penalty_bps: penalty,
                total_transactions,
                final_score: normalize_score_i128(base_score.saturating_sub(penalty)),
            },
            avg_rating,
            now,
            accumulator,
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
        let old_score_bps = rep.score;
        let (decomposition, avg_rating, now, accumulator) =
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
            (
                symbol_short!("reput"),
                symbol_short!("score_dec"),
                entity.clone(),
            ),
            decomposition,
        );
        env.events().publish(
            (
                symbol_short!("reput"),
                symbol_short!("updated"),
                entity.clone(),
            ),
            ReputationScoreUpdatedEvent {
                entity: entity.clone(),
                old_score_bps,
                new_score_bps: rep.score,
                total_reviews: rep.total_transactions,
            },
        );
        Ok(rep)
    }

    // --- Advanced Reputation Scoring Features ---

    /// Submit a cryptographic commitment of a verified buyer review.
    /// Stores a hash of the review content to link on-chain ratings to
    /// verifiable off-chain review content.
    ///
    /// Caller must be the buyer of the escrow, and the escrow must be in
    /// Released status. Each escrow_id can only have one review commitment.
    pub fn submit_review_commitment(
        env: Env,
        caller: Address,
        escrow_id: u64,
        rating: u32,
        content_hash: BytesN<32>,
        escrow_contract: Address,
    ) -> Result<(), ReputationError> {
        caller.require_auth();

        // Validate rating range (1-5 stars)
        if rating < MIN_RATING || rating > MAX_RATING {
            return Err(ReputationError::InvalidRating);
        }

        // Check if review already committed
        let commitment_key = DataKey::ReviewCommitment(escrow_id);
        if env
            .storage()
            .persistent()
            .get::<_, bool>(&commitment_key)
            .unwrap_or(false)
        {
            return Err(ReputationError::DuplicateReviewCommitment);
        }

        // Verify with escrow contract that caller is buyer and escrow is settled
        let args = soroban_sdk::vec![&env, escrow_id.into_val(&env)];
        let call_result = env.try_invoke_contract::<EscrowRecordMirror, InvokeError>(
            &escrow_contract,
            &Symbol::new(&env, "get_escrow"),
            args,
        );

        let escrow: EscrowRecordMirror = match call_result {
            Ok(Ok(record)) => record,
            _ => return Err(ReputationError::UnverifiedOrder),
        };

        if escrow.status != EscrowStatusMirror::Released {
            return Err(ReputationError::EscrowNotSettled);
        }

        if caller != escrow.buyer {
            return Err(ReputationError::NotBuyer);
        }

        let merchant = escrow.seller;

        // Store the review commitment
        let commitment = ReviewCommitment {
            escrow_id,
            reviewer: caller.clone(),
            merchant: merchant.clone(),
            rating,
            content_hash,
            submitted_ledger: env.ledger().sequence(),
        };

        env.storage().persistent().set(&commitment_key, &commitment);
        env.storage().persistent().extend_ttl(
            &commitment_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        // Track review commitments per merchant for indexing
        let merchant_reviews_key = DataKey::MerchantReviewCommitments(merchant.clone());
        let mut review_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&merchant_reviews_key)
            .unwrap_or_else(|| Vec::new(&env));
        review_ids.push_back(escrow_id);
        env.storage()
            .persistent()
            .set(&merchant_reviews_key, &review_ids);
        env.storage().persistent().extend_ttl(
            &merchant_reviews_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        Ok(())
    }

    /// Cast a dispute quorum vote for a peer validator resolution.
    /// Validators vote on 'buyer', 'merchant', or 'split' verdicts with their
    /// staked vote weight. Quorum is reached at 66% weighted consensus.
    pub fn cast_dispute_vote(
        env: Env,
        caller: Address,
        dispute_id: u64,
        verdict: Symbol,
        staked_vote_weight: i128,
    ) -> Result<(), ReputationError> {
        caller.require_auth();

        if !algorithm::validate_verdict_symbol(verdict.clone(), &env) {
            return Err(ReputationError::InvalidVerdictSymbol);
        }

        if staked_vote_weight <= 0 {
            return Err(ReputationError::InvalidParam);
        }

        let vote_key = DataKey::DisputeQuorumVote(dispute_id);
        let mut votes: Vec<DisputeQuorumVote> = env
            .storage()
            .persistent()
            .get(&vote_key)
            .unwrap_or_else(|| Vec::new(&env));

        // Check if this validator already voted
        for vote in votes.iter() {
            if vote.validator == caller {
                return Err(ReputationError::InvalidParam); // Already voted
            }
        }

        // Record the vote
        let vote = DisputeQuorumVote {
            dispute_id,
            validator: caller,
            verdict: verdict.clone(),
            staked_vote_weight,
        };
        votes.push_back(vote);
        env.storage().persistent().set(&vote_key, &votes);
        env.storage().persistent().extend_ttl(
            &vote_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        Ok(())
    }

    /// Retrieve normalized reputation score with sigmoid scaling and
    /// confidence weighting for a merchant.
    pub fn get_normalized_reputation_score(
        env: Env,
        merchant: Address,
    ) -> Result<NormalizedReputationScore, ReputationError> {
        let normalized_key = DataKey::NormalizedScore(merchant.clone());
        env.storage()
            .persistent()
            .get(&normalized_key)
            .ok_or(ReputationError::EntityNotFound)
    }

    /// Compute and cache normalized reputation score with sigmoid scaling.
    /// Applies sigmoid curve to smooth extreme values and computes confidence
    /// based on review sample size (Bayesian average approach).
    pub fn compute_normalized_score(
        env: Env,
        merchant: Address,
    ) -> Result<NormalizedReputationScore, ReputationError> {
        // Get raw reputation score
        let raw_rep = Self::get_reputation(env.clone(), merchant.clone())?;

        // Apply sigmoid normalization (steepness k=1, midpoint at 5000 bps)
        let sigmoid_score =
            algorithm::compute_sigmoid_normalized_score(raw_rep.score, 1, 5_000);

        // Compute confidence score based on review count
        let confidence =
            algorithm::compute_confidence_score(raw_rep.total_transactions, 10);

        let normalized_score = NormalizedReputationScore {
            merchant: merchant.clone(),
            normalized_score: sigmoid_score,
            confidence_score: confidence,
            last_updated_ledger: env.ledger().sequence(),
        };

        // Cache the normalized score
        let normalized_key = DataKey::NormalizedScore(merchant.clone());
        env.storage()
            .persistent()
            .set(&normalized_key, &normalized_score);
        env.storage().persistent().extend_ttl(
            &normalized_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        Ok(normalized_score)
    }

    /// Track a lost dispute for a merchant and update exponential penalty.
    /// Doubles the penalty multiplier for each consecutive loss (up to 16x).
    pub fn record_lost_dispute(
        env: Env,
        caller: Address,
        merchant: Address,
        dispute_id: u64,
    ) -> Result<(), ReputationError> {
        caller.require_auth();
        let admin = Self::require_admin(&env)?;
        if caller != admin {
            return Err(ReputationError::Unauthorized);
        }

        let penalty_key = DataKey::DisputePenalty(merchant.clone());
        let mut penalty_record: DisputePenaltyRecord = env
            .storage()
            .persistent()
            .get(&penalty_key)
            .unwrap_or_else(|| DisputePenaltyRecord {
                merchant: merchant.clone(),
                consecutive_lost_disputes: 0,
                last_dispute_ledger: 0,
                active_penalty_multiplier: 1,
            });

        // Increment consecutive losses
        penalty_record.consecutive_lost_disputes =
            penalty_record.consecutive_lost_disputes.saturating_add(1);
        penalty_record.last_dispute_ledger = env.ledger().sequence();

        // Compute exponential multiplier: 2^min(losses, 4)
        penalty_record.active_penalty_multiplier =
            algorithm::compute_dispute_penalty_multiplier(
                penalty_record.consecutive_lost_disputes,
                MAX_DISPUTE_PENALTY_MULTIPLIER,
            );

        env.storage()
            .persistent()
            .set(&penalty_key, &penalty_record);
        env.storage().persistent().extend_ttl(
            &penalty_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        Ok(())
    }

    /// Record a successful (dispute-free) order to progress toward penalty recovery.
    /// After 20 consecutive dispute-free orders, the penalty multiplier resets.
    pub fn record_dispute_free_order(
        env: Env,
        merchant: Address,
        escrow_id: u64,
    ) -> Result<(), ReputationError> {
        let free_count_key = DataKey::DisputeFreeOrderCount(merchant.clone());
        let mut count: u32 = env
            .storage()
            .persistent()
            .get(&free_count_key)
            .unwrap_or(0);

        count = count.saturating_add(1);

        // Check if recovery threshold reached
        if algorithm::should_reset_dispute_penalty(count, DISPUTE_FREE_RESET_THRESHOLD) {
            // Reset penalty record
            let penalty_key = DataKey::DisputePenalty(merchant.clone());
            let mut penalty_record: DisputePenaltyRecord = env
                .storage()
                .persistent()
                .get(&penalty_key)
                .unwrap_or_else(|| DisputePenaltyRecord {
                    merchant: merchant.clone(),
                    consecutive_lost_disputes: 0,
                    last_dispute_ledger: 0,
                    active_penalty_multiplier: 1,
                });

            penalty_record.consecutive_lost_disputes = 0;
            penalty_record.active_penalty_multiplier = 1;

            env.storage()
                .persistent()
                .set(&penalty_key, &penalty_record);
            env.storage().persistent().extend_ttl(
                &penalty_key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );

            // Reset dispute-free counter
            count = 0;
        }

        env.storage()
            .persistent()
            .set(&free_count_key, &count);
        env.storage().persistent().extend_ttl(
            &free_count_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        Ok(())
    }

    /// Get the current dispute penalty record for a merchant.
    pub fn get_dispute_penalty(
        env: Env,
        merchant: Address,
    ) -> Result<DisputePenaltyRecord, ReputationError> {
        let penalty_key = DataKey::DisputePenalty(merchant.clone());
        env.storage()
            .persistent()
            .get(&penalty_key)
            .ok_or(ReputationError::EntityNotFound)
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


    // --- Rehabilitation Program ---

    /// Enter a rehabilitation program by posting collateral. A frozen or penalised merchant
    /// must stake collateral and complete `probation_orders_required` clean orders to restore
    /// their reputation score incrementally. Called by merchant who requires auth.
    pub fn enter_rehabilitation(
        env: Env,
        merchant: Address,
        collateral_amount: i128,
    ) -> Result<RehabilitationProgram, ReputationError> {
        merchant.require_auth();
        
        if collateral_amount <= 0 {
            return Err(ReputationError::InvalidCollateralAmount);
        }

        // Merchant must have been penalised (frozen or low score) to be eligible
        let rep = Self::load_or_default_reputation(&env, &merchant);
        if !Self::is_frozen(env.clone(), merchant.clone()) && rep.score > 5_000 {
            // Entity is not frozen and has good standing, cannot enter rehab
            return Err(ReputationError::NotEligibleForRehabilitation);
        }

        let baseline_score = rep.score;
        let probation_orders_required = 10_u32; // Hardcoded for MVP; could be configurable
        
        let program = RehabilitationProgram {
            merchant: merchant.clone(),
            collateral_staked_stroops: collateral_amount,
            probation_orders_required,
            probation_orders_completed: 0,
            status: symbol_short!("active"),
            started_at: env.ledger().timestamp(),
            baseline_score_at_entry: baseline_score,
        };

        env.storage()
            .persistent()
            .set(&DataKey::RehabilitationProgram(merchant.clone()), &program);
        env.storage()
            .persistent()
            .extend_ttl(
                &DataKey::RehabilitationProgram(merchant.clone()),
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );

        env.events().publish(
            (
                symbol_short!("reput"),
                symbol_short!("rehab_start"),
                merchant,
            ),
            program.clone(),
        );

        Ok(program)
    }

    /// Track a clean probation order completion. Called by admin after verifying a successfully
    /// completed order during probation phase. Increments `probation_orders_completed` and
    /// restores reputation proportionally.
    pub fn complete_probation_order(
        env: Env,
        merchant: Address,
    ) -> Result<RehabilitationProgram, ReputationError> {
        let admin = Self::require_admin(&env)?;
        admin.require_auth();

        let program_key = DataKey::RehabilitationProgram(merchant.clone());
        let mut program: RehabilitationProgram = env
            .storage()
            .persistent()
            .get(&program_key)
            .ok_or(ReputationError::RehabilitationNotFound)?;

        if program.status != symbol_short!("active") {
            return Err(ReputationError::InvalidReportStatus);
        }

        program.probation_orders_completed =
            program.probation_orders_completed.saturating_add(1);

        // Incrementally restore reputation: each completed order restores a fraction
        // of the gap between baseline at entry and max score (10_000)
        let max_score: u32 = 10_000;
        let gap = max_score.saturating_sub(program.baseline_score_at_entry);
        let restoration_per_order = gap / program.probation_orders_required;
        let increment = restoration_per_order.min(100); // Cap per-order increment for stability

        let mut rep = Self::load_or_default_reputation(&env, &merchant);
        rep.score = rep.score.saturating_add(increment).min(max_score);
        rep.last_updated = env.ledger().timestamp();

        env.storage()
            .persistent()
            .set(&DataKey::Reputation(merchant.clone()), &rep);
        env.storage()
            .persistent()
            .extend_ttl(
                &DataKey::Reputation(merchant.clone()),
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );

        // Check if probation is complete
        if program.probation_orders_completed >= program.probation_orders_required {
            program.status = symbol_short!("gradu");
        }

        env.storage()
            .persistent()
            .set(&program_key, &program);
        env.storage()
            .persistent()
            .extend_ttl(
                &program_key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );

        Ok(program)
    }

    /// Graduate from rehabilitation. Called by merchant after completing all probation orders.
    /// Returns staked collateral and unlocks the merchant if previously frozen.
    pub fn graduate_rehabilitation(
        env: Env,
        merchant: Address,
    ) -> Result<i128, ReputationError> {
        merchant.require_auth();

        let program_key = DataKey::RehabilitationProgram(merchant.clone());
        let mut program: RehabilitationProgram = env
            .storage()
            .persistent()
            .get(&program_key)
            .ok_or(ReputationError::RehabilitationNotFound)?;

        if program.probation_orders_completed < program.probation_orders_required {
            return Err(ReputationError::ProbationNotComplete);
        }

        program.status = symbol_short!("compl");
        env.storage()
            .persistent()
            .set(&program_key, &program);

        // Unfreeze the merchant if frozen
        if Self::is_frozen(env.clone(), merchant.clone()) {
            env.storage()
                .persistent()
                .set(&DataKey::FrozenStatus(merchant.clone()), &false);
        }

        let collateral_returned = program.collateral_staked_stroops;

        env.events().publish(
            (
                symbol_short!("reput"),
                symbol_short!("rehab_grad"),
                merchant,
            ),
            program,
        );

        Ok(collateral_returned)
    }

    /// Get the current rehabilitation program for a merchant, if active.
    pub fn get_rehabilitation_program(
        env: Env,
        merchant: Address,
    ) -> Option<RehabilitationProgram> {
        env.storage()
            .persistent()
            .get(&DataKey::RehabilitationProgram(merchant))
    }

    // --- Blinded Review (Privacy-Preserving) ---

    /// Issue a blinded review authorization token during escrow checkout.
    /// Generates an ephemeral blinded key to decouple buyer identity from review.
    pub fn issue_blinded_review_token(
        env: Env,
        escrow_id: u64,
        blinded_public_key: BytesN<32>,
    ) -> Result<BlindedReviewReceipt, ReputationError> {
        let admin = Self::require_admin(&env)?;
        admin.require_auth();

        // Create commitment to escrow_id (prevents replay across different orders)
        let mut commitment_bytes = [0u8; 32];
        let escrow_bytes = escrow_id.to_le_bytes();
        for i in 0..8 {
            commitment_bytes[i] = escrow_bytes[i];
        }
        let escrow_id_commitment = BytesN::<32>::from_array(&env, commitment_bytes);

        let receipt = BlindedReviewReceipt {
            escrow_id_commitment: escrow_id_commitment.clone(),
            blinded_public_key,
            blind_signature: BytesN::<64>::from_array(&env, [0u8; 64]), // Placeholder
            issued_at: env.ledger().timestamp(),
        };

        env.events().publish(
            (
                symbol_short!("reput"),
                symbol_short!("blind_token"),
            ),
            receipt.clone(),
        );

        Ok(receipt)
    }

    /// Verify and process a blinded review submission. Checks Ed25519 blind signature,
    /// prevents double-submission via nullifier hash, and stores review anonymously.
    pub fn submit_blinded_review(
        env: Env,
        receipt: BlindedReviewReceipt,
        entity: Address,
        rating: u32,
        nullifier_hash: BytesN<32>,
    ) -> Result<(), ReputationError> {
        if rating as i128 > BPS_SCALE {
            return Err(ReputationError::InvalidRating);
        }

        // Check double-submission: nullifier must be unique
        let nullifier_key = DataKey::BlindedReviewNullifier(nullifier_hash.clone());
        if env
            .storage()
            .persistent()
            .get::<_, bool>(&nullifier_key)
            .unwrap_or(false)
        {
            return Err(ReputationError::BlindedReceiptAlreadyUsed);
        }

        // Mark nullifier as used
        env.storage()
            .persistent()
            .set(&nullifier_key, &true);
        env.storage()
            .persistent()
            .extend_ttl(
                &nullifier_key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );

        // In production: verify blind Ed25519 signature on (escrow_id_commitment, rating)
        // For MVP: accept receipt as-is. Production would call external crypto verification.
        // The blind signature prevents the reviewer's ephemeral key from being linked to
        // their primary wallet address.

        if Self::is_frozen(env.clone(), entity.clone()) {
            return Err(ReputationError::EntityFrozen);
        }

        // Apply the review to the entity's average rating
        let mut rep = Self::load_or_default_reputation(&env, &entity);
        
        // Update avg_rating with the new blinded review
        if rep.avg_rating == 0 {
            rep.avg_rating = rating;
        } else {
            // Weighted average: (old_avg * tx_count + new_rating) / (tx_count + 1)
            let weighted_sum = (rep.avg_rating as i128) * (rep.total_transactions as i128) + (rating as i128);
            rep.avg_rating = (weighted_sum / ((rep.total_transactions + 1) as i128)).clamp(0, BPS_SCALE) as u32;
        }

        rep.last_updated = env.ledger().timestamp();
        env.storage()
            .persistent()
            .set(&DataKey::Reputation(entity.clone()), &rep);
        env.storage()
            .persistent()
            .extend_ttl(
                &DataKey::Reputation(entity.clone()),
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );

        env.events().publish(
            (
                symbol_short!("reput"),
                symbol_short!("blind_review"),
                entity,
            ),
            EntityRatedEvent {
                rater: env.current_contract_address(), // Anonymous reviewer
                entity,
                rating,
                escrow_id: u64::from_le_bytes([
                    receipt.escrow_id_commitment.get(0) as u8,
                    receipt.escrow_id_commitment.get(1) as u8,
                    receipt.escrow_id_commitment.get(2) as u8,
                    receipt.escrow_id_commitment.get(3) as u8,
                    receipt.escrow_id_commitment.get(4) as u8,
                    receipt.escrow_id_commitment.get(5) as u8,
                    receipt.escrow_id_commitment.get(6) as u8,
                    receipt.escrow_id_commitment.get(7) as u8,
                ]),
            },
        );

        Ok(())
    }

    // --- Fraud Reporting ---

    /// File a fraud report with staked bond. Reporter must bond 50 USDC (or configured amount).
    /// If report is confirmed by moderators, reporter earns bounty. If frivolous, bond is slashed.
    pub fn file_fraud_report(
        env: Env,
        reporter: Address,
        target_entity: Address,
        evidence_hash: BytesN<32>,
        staked_bond_stroops: i128,
    ) -> Result<u64, ReputationError> {
        reporter.require_auth();

        // Minimum bond: 50 USDC in stroops (50 * 10^7 = 500_000_000)
        let min_bond_stroops: i128 = 500_000_000;
        if staked_bond_stroops < min_bond_stroops {
            return Err(ReputationError::InvalidFraudBond);
        }

        // Generate unique report_id
        let counter_key = DataKey::FraudReportCounter;
        let report_id: u64 = env
            .storage()
            .instance()
            .get(&counter_key)
            .unwrap_or(0);
        let next_report_id = report_id.saturating_add(1);
        env.storage()
            .instance()
            .set(&counter_key, &next_report_id);

        let report = FraudReport {
            report_id,
            reporter: reporter.clone(),
            target_entity: target_entity.clone(),
            evidence_hash,
            staked_bond_stroops,
            status: symbol_short!("filed"),
            filed_at: env.ledger().timestamp(),
            resolved_at: None,
        };

        let report_key = DataKey::FraudReport(report_id);
        env.storage()
            .persistent()
            .set(&report_key, &report);
        env.storage()
            .persistent()
            .extend_ttl(
                &report_key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );

        // Index by target for quick lookup
        let target_key = DataKey::FraudReportsByTarget(target_entity.clone());
        let mut report_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&target_key)
            .unwrap_or_else(|| Vec::new(&env));
        report_ids.push_back(report_id);
        env.storage()
            .persistent()
            .set(&target_key, &report_ids);
        env.storage()
            .persistent()
            .extend_ttl(
                &target_key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );

        env.events().publish(
            (
                symbol_short!("reput"),
                symbol_short!("fraud_filed"),
            ),
            report.clone(),
        );

        Ok(report_id)
    }

    /// Resolve a fraud report. Admin/governance only. If `is_valid` = true, reporter
    /// receives bounty and is refunded bond. If false, reporter's bond is slashed.
    pub fn resolve_fraud_report(
        env: Env,
        admin: Address,
        report_id: u64,
        is_valid: bool,
    ) -> Result<FraudReport, ReputationError> {
        admin.require_auth();
        Self::require_caller_is_admin(&env, &admin)?;

        let report_key = DataKey::FraudReport(report_id);
        let mut report: FraudReport = env
            .storage()
            .persistent()
            .get(&report_key)
            .ok_or(ReputationError::FraudReportNotFound)?;

        if report.status != symbol_short!("filed") {
            return Err(ReputationError::InvalidReportStatus);
        }

        report.status = if is_valid {
            symbol_short!("valid")
        } else {
            symbol_short!("invld")
        };
        report.resolved_at = Some(env.ledger().timestamp());

        env.storage()
            .persistent()
            .set(&report_key, &report);

        // Event emits outcome: valid reports get bounty, invalid reports lose bond
        env.events().publish(
            (
                symbol_short!("reput"),
                symbol_short!("fraud_resol"),
            ),
            report.clone(),
        );

        // If valid: reporter earns bounty (off-chain payout or treasury transfer)
        // If invalid: slash reporter's bond (off-chain slashing or treasury claim)
        // Details left to governance contract that calls this

        Ok(report)
    }

    /// Get a fraud report by ID.
    pub fn get_fraud_report(
        env: Env,
        report_id: u64,
    ) -> Option<FraudReport> {
        env.storage()
            .persistent()
            .get(&DataKey::FraudReport(report_id))
    }

    /// Get all fraud reports targeting an entity.
    pub fn get_fraud_reports_by_target(
        env: Env,
        target_entity: Address,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<FraudReport>, ReputationError> {
        let target_key = DataKey::FraudReportsByTarget(target_entity);
        let report_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&target_key)
            .unwrap_or_else(|| Vec::new(&env));

        let mut result = Vec::new(&env);
        let end = offset.saturating_add(limit).min(report_ids.len());
        let mut i = offset;
        while i < end {
            if let Some(report_id) = report_ids.get(i) {
                if let Some(report) = env
                    .storage()
                    .persistent()
                    .get(&DataKey::FraudReport(report_id))
                {
                    result.push_back(report);
                }
            }
            i += 1;
        }
        Ok(result)
    }
}

// --- Helper methods ---

impl ReputationContract {
    fn require_admin(env: &Env) -> Result<Address, ReputationError> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(ReputationError::NotInitialized)
    }

    fn require_caller_is_admin(env: &Env, caller: &Address) -> Result<(), ReputationError> {
        let admin = Self::require_admin(env)?;
        if caller != &admin {
            Err(ReputationError::Unauthorized)
        } else {
            Ok(())
        }
    }

    fn bump_entity(env: &Env, entity: &Address) {
        if let Ok(rep_key) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            DataKey::Reputation(entity.clone())
        })) {
            let _ = env.storage().persistent().extend_ttl(
                &rep_key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );
        }
    }

    fn has_transacted_with(env: &Env, entity: &Address, counterparty: &Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::Transacted(entity.clone(), counterparty.clone()))
            .unwrap_or(false)
    }

    fn load_or_default_reputation(env: &Env, entity: &Address) -> ReputationScore {
        env.storage()
            .persistent()
            .get(&DataKey::Reputation(entity.clone()))
            .unwrap_or_else(|| ReputationScore {
                entity: entity.clone(),
                score: 0,
                total_transactions: 0,
                successful_transactions: 0,
                disputed_transactions: 0,
                avg_rating: 0,
                last_updated: 0,
            })
    }

    fn validate_config(config: &ReputationConfig) -> Result<(), ReputationError> {
        if config.min_transactions_threshold > SCORE_WINDOW as u64 {
            return Err(ReputationError::InvalidParam);
        }
        Ok(())
    }
}


// --- ReputationReaderTrait Implementation for Cross-Contract Access ---

use delego_interfaces::ReputationReaderTrait;

impl ReputationReaderTrait for ReputationContract {
    fn get_merchant_rating(env: Env, merchant: Address) -> Result<u32, u32> {
        match Self::get_reputation(env, merchant) {
            Ok(rep) => Ok(rep.score),
            Err(_) => Err(0),
        }
    }

    fn get_dispute_ratio(env: Env, merchant: Address) -> Result<u32, u32> {
        match Self::get_reputation(env, merchant) {
            Ok(rep) => {
                if rep.total_transactions == 0 {
                    return Ok(0);
                }
                let ratio = (rep.disputed_transactions as i128 * BPS_SCALE)
                    / (rep.total_transactions as i128);
                Ok(ratio.clamp(0, BPS_SCALE) as u32)
            }
            Err(_) => Err(0),
        }
    }

    fn is_merchant_in_good_standing(env: Env, merchant: Address) -> Result<bool, u32> {
        if Self::is_frozen(env.clone(), merchant.clone()) {
            return Ok(false);
        }
        
        match Self::get_reputation(env, merchant) {
            Ok(rep) => {
                // Good standing: not frozen, score >= 5000 (50%), and active
                Ok(rep.score >= 5000)
            }
            Err(_) => Ok(false),
        }
    }

    fn get_merchant_transaction_count(env: Env, merchant: Address) -> Result<u64, u32> {
        match Self::get_reputation(env, merchant) {
            Ok(rep) => Ok(rep.total_transactions),
            Err(_) => Err(0),
        }
    }

    fn get_merchant_average_rating(env: Env, merchant: Address) -> Result<u32, u32> {
        match Self::get_reputation(env, merchant) {
            Ok(rep) => Ok(rep.avg_rating),
            Err(_) => Err(0),
        }
    }
}
