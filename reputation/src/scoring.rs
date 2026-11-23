//! Exponential decay half-life scoring for merchant feedback reviews.
//!
//! Provides decay weighting functions and decayed score aggregation
//! to weight older feedback ratings lower than fresh reviews.

use soroban_sdk::contracttype;

/// Basis-point scale (100%).
const DECAY_BPS: u64 = 10_000;

/// Configuration for staked-reputation exponential decay (issue #478).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationDecayConfig {
    /// Epochs after which the decay multiplier halves.
    pub half_life_epochs: u32,
    /// Lower bound of the multiplier, in basis points.
    pub min_decay_floor: u32,
    /// Elapsed epochs are capped at this value before computing decay.
    pub max_epochs_supported: u32,
}

/// Fixed-point one (Q64).
const FP_ONE: u128 = 1u128 << 64;

/// Fixed-point (Q64) multiply of two values `<= FP_ONE`; `None` on overflow.
fn fp_mul(a: u128, b: u128) -> Option<u128> {
    let (ah, al) = (a >> 32, a & 0xFFFF_FFFF);
    let (bh, bl) = (b >> 32, b & 0xFFFF_FFFF);
    let hh = ah.checked_mul(bh)?;
    let mid = ah
        .checked_mul(bl)?
        .checked_add(al.checked_mul(bh)?)?
        .checked_add(al.checked_mul(bl)? >> 32)?;
    hh.checked_add(mid >> 32)
}

/// Per-epoch decay factor `2^(-1/half_life) = exp(-ln2 / h)` in Q64,
/// evaluated with a short Taylor series (t <= ln 2).
fn per_epoch_factor(half_life_epochs: u32) -> Option<u128> {
    let h = half_life_epochs.max(1) as u128;
    // ln(2) in Q64.
    const LN2_Q64: u128 = 12_786_308_645_202_655_660;
    let t = LN2_Q64.checked_div(h)?;
    let t2 = fp_mul(t, t)?;
    let t3 = fp_mul(t2, t)?;
    let t4 = fp_mul(t3, t)?;
    let t5 = fp_mul(t4, t)?;
    FP_ONE
        .checked_sub(t)?
        .checked_add(t2 / 2)?
        .checked_sub(t3 / 6)?
        .checked_add(t4 / 24)?
        .checked_sub(t5 / 120)
}

/// Decay multiplier in basis points after `elapsed_epochs`, computed with
/// binary square-and-multiply fixed-point exponentiation of the per-epoch
/// factor. `elapsed_epochs` is capped at `max_epochs_supported`; any
/// overflow/underflow, or a result below the floor, yields
/// `min_decay_floor`. Never panics.
pub fn compute_staked_decay_bps(config: &ReputationDecayConfig, elapsed_epochs: u32) -> u32 {
    let floor = config.min_decay_floor.min(DECAY_BPS as u32);
    let epochs = elapsed_epochs.min(config.max_epochs_supported);
    if epochs == 0 {
        return DECAY_BPS as u32;
    }
    let result = (|| -> Option<u32> {
        let mut base = per_epoch_factor(config.half_life_epochs)?;
        let mut acc = FP_ONE;
        let mut e = epochs;
        while e > 0 {
            if e & 1 == 1 {
                acc = fp_mul(acc, base)?;
            }
            e >>= 1;
            if e > 0 {
                base = fp_mul(base, base)?;
            }
        }
        let bps = acc.checked_mul(DECAY_BPS as u128)? >> 64;
        u32::try_from(bps).ok()
    })();
    match result {
        Some(bps) if bps >= floor => bps.min(DECAY_BPS as u32),
        _ => floor,
    }
}

/// Applies [`compute_staked_decay_bps`] to a staked score without overflow.
pub fn apply_staked_decay(score: u64, config: &ReputationDecayConfig, elapsed_epochs: u32) -> u64 {
    let bps = compute_staked_decay_bps(config, elapsed_epochs) as u64;
    match score.checked_mul(bps) {
        Some(v) => v / DECAY_BPS,
        None => (score / DECAY_BPS).saturating_mul(bps),
    }
}

/// Computes the exponential decay weight for a review based on its age.
///
/// Applies exponential decay half-life weighting:
/// - Reviews fresh (< 180 days) receive 100% weight.
/// - Each subsequent 180-day half-life halves the weight (50%, 25%, 12%, 6%).
/// - Capped at 4 halvings.
pub fn compute_decayed_weight(review_timestamp: u64, current_timestamp: u64) -> u32 {
    let age_seconds = current_timestamp.saturating_sub(review_timestamp);
    let half_life_seconds = 180 * 86400; // 180 days
    let decay_factor = (age_seconds / half_life_seconds).min(4);
    100 >> decay_factor // 100%, 50%, 25%, 12.5%, 6.25%
}

/// A feedback review rating with its submission timestamp.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeedbackReview {
    /// Feedback rating (e.g. 0-10000 basis points or 1-5 stars).
    pub rating: u32,
    /// Timestamp when review was submitted in seconds.
    pub timestamp: u64,
}

/// Computes a decayed aggregate rating score from a set of feedback reviews.
///
/// Each review's rating is weighted by [`compute_decayed_weight`].
/// Returns 0 if the review list is empty or total weight is 0.
pub fn aggregate_decayed_score(reviews: &[FeedbackReview], current_timestamp: u64) -> u32 {
    let mut weighted_sum: u64 = 0;
    let mut total_weight: u64 = 0;

    for review in reviews {
        let weight = compute_decayed_weight(review.timestamp, current_timestamp) as u64;
        weighted_sum += (review.rating as u64) * weight;
        total_weight += weight;
    }

    if total_weight == 0 {
        0
    } else {
        (weighted_sum / total_weight) as u32
    }
}

/// Computes the decayed merchant score by weighting individual rating values and their ages.
///
/// Takes pairs of (rating, review_timestamp) and aggregates them against `current_timestamp`.
pub fn calculate_decayed_merchant_score<I>(
    ratings_with_timestamps: I,
    current_timestamp: u64,
) -> u32
where
    I: IntoIterator<Item = (u32, u64)>,
{
    let mut weighted_sum: u64 = 0;
    let mut total_weight: u64 = 0;

    for (rating, review_timestamp) in ratings_with_timestamps {
        let weight = compute_decayed_weight(review_timestamp, current_timestamp) as u64;
        weighted_sum += (rating as u64) * weight;
        total_weight += weight;
    }

    if total_weight == 0 {
        0
    } else {
        (weighted_sum / total_weight) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HALF_LIFE: u64 = 180 * 86400;

    #[test]
    fn test_compute_decayed_weight_fresh_vs_aged() {
        let now = 1_000_000_000u64;

        // Fresh review (same timestamp) receives 100% weight
        assert_eq!(compute_decayed_weight(now, now), 100);

        // Review just under 1 half-life receives 100% weight
        assert_eq!(compute_decayed_weight(now - HALF_LIFE + 1, now), 100);

        // Review exactly 1 half-life old receives 50% weight
        assert_eq!(compute_decayed_weight(now - HALF_LIFE, now), 50);

        // Review 2 half-lives old receives 25% weight
        assert_eq!(compute_decayed_weight(now - 2 * HALF_LIFE, now), 25);

        // Review 3 half-lives old receives 12% weight (100 >> 3 = 12)
        assert_eq!(compute_decayed_weight(now - 3 * HALF_LIFE, now), 12);

        // Review 4 half-lives old receives 6% weight (100 >> 4 = 6)
        assert_eq!(compute_decayed_weight(now - 4 * HALF_LIFE, now), 6);

        // Review beyond 4 half-lives caps at 6% weight
        assert_eq!(compute_decayed_weight(now - 5 * HALF_LIFE, now), 6);
        assert_eq!(compute_decayed_weight(now - 10 * HALF_LIFE, now), 6);

        // Future review timestamp (saturating_sub protects against underflow)
        assert_eq!(compute_decayed_weight(now + 1000, now), 100);
    }

    #[test]
    fn test_aggregate_decayed_score_empty() {
        assert_eq!(aggregate_decayed_score(&[], 100_000), 0);
        assert_eq!(calculate_decayed_merchant_score(vec![], 100_000), 0);
    }

    #[test]
    fn test_fresh_vs_aged_ratings_comparison() {
        let now = 2_000_000_000u64;

        // Merchant had bad reviews 2 years ago (4 half-lives ago),
        // but recent reviews are 5-star (5000 bps).
        // 2 years ~ 4 half-lives (720 days).
        let old_review_time = now - 4 * HALF_LIFE;
        let fresh_review_time = now;

        // If ratings were unweighted: (1000 + 5000) / 2 = 3000
        // With decay:
        // old weight = 6, rating = 1000 => 6_000
        // fresh weight = 100, rating = 5000 => 500_000
        // total weight = 106
        // aggregate = 506_000 / 106 = 4773 (much closer to recent 5000)
        let reviews = [
            FeedbackReview {
                rating: 1000,
                timestamp: old_review_time,
            },
            FeedbackReview {
                rating: 5000,
                timestamp: fresh_review_time,
            },
        ];

        let decayed_score = aggregate_decayed_score(&reviews, now);
        assert_eq!(decayed_score, 4773);
        assert!(
            decayed_score > 3000,
            "Recent positive feedback must dominate over aged negative feedback"
        );

        // Vice versa: merchant was good 2 years ago, but recent service is bad (1000 bps)
        let deteriorating_reviews = [
            FeedbackReview {
                rating: 5000,
                timestamp: old_review_time,
            },
            FeedbackReview {
                rating: 1000,
                timestamp: fresh_review_time,
            },
        ];

        let decayed_deteriorating = aggregate_decayed_score(&deteriorating_reviews, now);
        // (5000 * 6 + 1000 * 100) / 106 = (30_000 + 100_000) / 106 = 130_000 / 106 = 1226
        assert_eq!(decayed_deteriorating, 1226);
        assert!(
            decayed_deteriorating < 3000,
            "Recent poor feedback must pull score down heavily despite good past"
        );

        // Verify calculate_decayed_merchant_score yields identical result
        let iterator_score = calculate_decayed_merchant_score(
            vec![(1000, old_review_time), (5000, fresh_review_time)],
            now,
        );
        assert_eq!(iterator_score, decayed_score);
    }

    fn decay_cfg() -> ReputationDecayConfig {
        ReputationDecayConfig {
            half_life_epochs: 10,
            min_decay_floor: 500,
            max_epochs_supported: 1_000,
        }
    }

    #[test]
    fn staked_decay_half_life_is_about_half() {
        let bps = compute_staked_decay_bps(&decay_cfg(), 10);
        assert!((4_990..=5_010).contains(&bps), "got {bps}");
        assert_eq!(compute_staked_decay_bps(&decay_cfg(), 0), 10_000);
    }

    #[test]
    fn staked_decay_large_spans_converge_to_floor_without_panic() {
        let cfg = decay_cfg();
        let mut prev = u32::MAX;
        for epochs in [1u32, 5, 20, 50, 100, 150, 500, 1_000, 10_000, u32::MAX] {
            let bps = compute_staked_decay_bps(&cfg, epochs);
            assert!(bps <= prev, "decay must be monotonic");
            assert!(bps >= cfg.min_decay_floor);
            prev = bps;
        }
        assert_eq!(compute_staked_decay_bps(&cfg, 100), cfg.min_decay_floor);
        assert_eq!(compute_staked_decay_bps(&cfg, u32::MAX), cfg.min_decay_floor);
        assert_eq!(
            apply_staked_decay(u64::MAX, &cfg, u32::MAX),
            u64::MAX / 10_000 * 500
        );
    }

    #[test]
    fn staked_decay_degenerate_config_is_safe() {
        let cfg = ReputationDecayConfig {
            half_life_epochs: 0,
            min_decay_floor: 20_000,
            max_epochs_supported: 0,
        };
        assert_eq!(compute_staked_decay_bps(&cfg, 500), 10_000);
        let cfg = ReputationDecayConfig {
            half_life_epochs: 1,
            min_decay_floor: 0,
            max_epochs_supported: u32::MAX,
        };
        assert_eq!(compute_staked_decay_bps(&cfg, u32::MAX), 0);
    }

    #[test]
    fn test_all_fresh_reviews() {
        let now = 1_000_000u64;
        let reviews = [
            FeedbackReview {
                rating: 4000,
                timestamp: now - 100,
            },
            FeedbackReview {
                rating: 5000,
                timestamp: now,
            },
        ];

        // Both have weight 100, aggregate is exact average (4500)
        assert_eq!(aggregate_decayed_score(&reviews, now), 4500);
    }
}
