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
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env,
    IntoVal, InvokeError, String, Symbol, Vec,
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
    NotFlagReporter = 0x0003_000B,
    /// `submit_verified_review`'s `VerifiedReviewProof` did not check out:
    /// the referenced escrow could not be read from `escrow_contract`, is
    /// not in `Released` status, its `order_id` did not match
    /// `order_hash`, or `rater`/`entity` are not its buyer/seller pair
    /// (issue #286).
    UnverifiedOrder = 0x0003_000C,
    NotFlagReporter = 3011,
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
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RankedMerchantPage {
    pub merchants: Vec<MerchantView>,
    pub next_cursor: Option<ReputationCursor>,
    pub total_count: u32,
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
    fixed_point_decay(
        historical_score as u64,
        ledgers_elapsed as u64,
        half_life_ledgers as u64,
        u32::BITS as u64,
    ) as u32
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
                next_cursor: None,
                total_count,
            });
        }

        // Walk the merchant_id space in descending order, collecting entries
        // that sort strictly after the cursor in `(score_bps, merchant_id)`
        // descending order. `merchant_id` is assigned monotonically, so
        // iterating ids downward visits candidates in the exact tie-break
        // order the index requires; the score comparison then filters to the
        // keyset window.
        let mut next_cursor: Option<ReputationCursor> = None;
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
                // pair.
                next_cursor = Some(ReputationCursor {
                    last_score_bps: rep.score,
                    last_merchant_id: id,
                });
                break;
            }

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
            next_cursor,
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
