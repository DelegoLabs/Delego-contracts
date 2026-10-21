//! Exponential decay half-life scoring for merchant feedback reviews.
//!
//! Provides decay weighting functions and decayed score aggregation
//! to weight older feedback ratings lower than fresh reviews.

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
pub fn calculate_decayed_merchant_score<I>(ratings_with_timestamps: I, current_timestamp: u64) -> u32
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
        assert!(decayed_score > 3000, "Recent positive feedback must dominate over aged negative feedback");

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
        assert!(decayed_deteriorating < 3000, "Recent poor feedback must pull score down heavily despite good past");

        // Verify calculate_decayed_merchant_score yields identical result
        let iterator_score = calculate_decayed_merchant_score(
            vec![(1000, old_review_time), (5000, fresh_review_time)],
            now,
        );
        assert_eq!(iterator_score, decayed_score);
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
