//! Advanced reputation scoring algorithms: normalized scoring with sigmoid scaling,
//! dispute penalty multipliers, and peer validator consensus.

use soroban_sdk::{Env, Symbol};

/// Fixed-point scale used by [`exp_neg_scaled`].
const EXP_SCALE: i128 = 1_000_000_000;

/// Computes `exp(-t)` for a non-negative `t`, returned as a fixed-point value
/// scaled by [`EXP_SCALE`].
///
/// The exponential is evaluated as `exp(-t) = exp(-t/16)^16`: the truncated
/// Taylor series only ever sees a small argument (at most `0.75`), and the
/// result is then squared four times. That keeps every intermediate inside
/// `i128` while staying accurate to roughly one part in `10^8`.
fn exp_neg_scaled(t: i128) -> i128 {
    if t <= 0 {
        return EXP_SCALE;
    }
    // exp(-12) is below 1e-5, so the sigmoid is indistinguishable from 1.
    if t > 12 {
        return 0;
    }

    let u = (t * EXP_SCALE) / 16;
    let mut term = EXP_SCALE;
    let mut sum = EXP_SCALE;
    let mut n: i128 = 1;
    while n <= 16 {
        term = (term * -u) / (n * EXP_SCALE);
        if term == 0 {
            break;
        }
        sum += term;
        n += 1;
    }

    // Four squarings turn exp(-t/16) into exp(-t).
    let mut acc = sum;
    for _ in 0..4 {
        acc = (acc * acc) / EXP_SCALE;
    }
    acc.max(0)
}

/// Sigmoid function applied to raw reputation scores to create a smooth,
/// S-shaped curve that dampens extreme values while preserving rank ordering.
///
/// The sigmoid is computed using fixed-point arithmetic:
/// sigmoid(x) = 10000 / (1 + exp(-k*(x - midpoint) / 1000))
///
/// Where k controls steepness and midpoint centers the curve. One unit of
/// steepness spans 1 000 bps of raw score, so with the default `k = 1` the
/// whole 0..10 000 bps band maps onto the logistic argument -5..5.
pub fn compute_sigmoid_normalized_score(
    raw_score_bps: u32,
    k_steepness: i128,
    midpoint_bps: u32,
) -> u32 {
    const BPS_SCALE: i128 = 10_000;

    if raw_score_bps == 0 {
        return 0;
    }

    let x: i128 = raw_score_bps as i128 - midpoint_bps as i128;
    // Logistic argument, clamped so the exponential helper stays in range.
    let z: i128 = (k_steepness.saturating_mul(x) / 1_000).clamp(-12, 12);

    // sigmoid(z) = 1 / (1 + exp(-z)). For z < 0 the curve is mirrored:
    // sigmoid(z) = 1 - sigmoid(-z).
    let sigmoid_bps: i128 = if z >= 0 {
        (BPS_SCALE * EXP_SCALE) / (EXP_SCALE + exp_neg_scaled(z))
    } else {
        BPS_SCALE - (BPS_SCALE * EXP_SCALE) / (EXP_SCALE + exp_neg_scaled(-z))
    };

    sigmoid_bps.clamp(0, BPS_SCALE) as u32
}

/// Confidence score reflecting statistical significance of a merchant's reputation.
/// Uses a Bayesian average approach: confidence = sqrt(review_count / (review_count + k))
/// where k is a smoothing parameter (~10 in normalized bps).
///
/// This ensures merchants with few reviews get lower confidence ratings,
/// preventing high variance from lucky early trades.
pub fn compute_confidence_score(review_count: u64, smoothing_parameter: u64) -> u32 {
    const BPS_SCALE: i128 = 10_000;

    if review_count == 0 {
        return 0;
    }

    // Integer square root approximation: isqrt(n)
    let n = review_count as i128;
    let k = smoothing_parameter as i128;

    // confidence = sqrt(n / (n + k)) * 10000
    // = sqrt(n) * 10000 / sqrt(n + k)
    let numerator_sqrt = isqrt(n);
    let denominator_sqrt = isqrt(n + k);

    if denominator_sqrt == 0 {
        0
    } else {
        ((numerator_sqrt * BPS_SCALE) / denominator_sqrt).clamp(0, 10_000) as u32
    }
}

/// Integer square root using Newton's method (Babylonian algorithm).
fn isqrt(n: i128) -> i128 {
    if n == 0 {
        return 0;
    }
    if n < 0 {
        return 0;
    }

    let mut x = n;
    let mut y = (x + 1) / 2;

    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }

    x
}

/// Exponential penalty multiplier for consecutive lost disputes.
/// Each consecutive loss doubles the multiplier: 2^min(losses, MAX_MULTIPLIER_POWER)
///
/// This prevents merchants from easily recovering after demonstrating
/// systematic compliance problems.
pub fn compute_dispute_penalty_multiplier(consecutive_losses: u32, max_power: u32) -> u32 {
    let power = consecutive_losses.min(max_power);
    1u32.checked_shl(power).unwrap_or(u32::MAX) // 2^power
}

/// Resets dispute penalty if merchant has completed enough dispute-free orders.
/// Returns true if the penalty should be reset.
pub fn should_reset_dispute_penalty(dispute_free_order_count: u32, reset_threshold: u32) -> bool {
    dispute_free_order_count >= reset_threshold
}

/// Validates a dispute verdict symbol.
pub fn validate_verdict_symbol(verdict: Symbol, env: &Env) -> bool {
    let buyer = Symbol::new(env, "buyer");
    let merchant = Symbol::new(env, "merchant");
    let split = Symbol::new(env, "split");

    verdict == buyer || verdict == merchant || verdict == split
}

/// Tallies validator votes and determines if quorum is reached.
/// Returns (winner, quorum_reached, buyer_votes, merchant_votes, split_votes).
pub fn tally_dispute_votes(
    votes: &[(Symbol, i128)],
    quorum_threshold_bps: i128,
    env: &Env,
) -> (Option<Symbol>, bool, i128, i128, i128) {
    const BPS_SCALE: i128 = 10_000;

    let mut buyer_weight: i128 = 0;
    let mut merchant_weight: i128 = 0;
    let mut split_weight: i128 = 0;
    let mut total_weight: i128 = 0;

    let buyer = Symbol::new(env, "buyer");
    let merchant = Symbol::new(env, "merchant");
    let split = Symbol::new(env, "split");

    for (verdict, weight) in votes {
        total_weight = total_weight.saturating_add(*weight);
        if *verdict == buyer {
            buyer_weight = buyer_weight.saturating_add(*weight);
        } else if *verdict == merchant {
            merchant_weight = merchant_weight.saturating_add(*weight);
        } else if *verdict == split {
            split_weight = split_weight.saturating_add(*weight);
        }
    }

    if total_weight == 0 {
        return (None, false, 0, 0, 0);
    }

    // Determine winning verdict (highest vote weight)
    let winner = if buyer_weight >= merchant_weight && buyer_weight >= split_weight {
        Some(buyer.clone())
    } else if merchant_weight >= split_weight {
        Some(merchant.clone())
    } else {
        Some(split.clone())
    };

    // Check if winner reached quorum
    let winner_weight = match winner {
        Some(ref w) if *w == buyer => buyer_weight,
        Some(ref w) if *w == merchant => merchant_weight,
        Some(ref w) if *w == split => split_weight,
        _ => 0,
    };

    let winner_percentage_bps: i128 = if total_weight > 0 {
        (winner_weight * BPS_SCALE) / total_weight
    } else {
        0
    };

    let quorum_reached = winner_percentage_bps >= quorum_threshold_bps;

    (
        winner,
        quorum_reached,
        buyer_weight,
        merchant_weight,
        split_weight,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sigmoid_normalizes_scores() {
        // Middle score should map to ~5000 bps
        let mid = compute_sigmoid_normalized_score(5_000, 1, 5_000);
        assert!(mid > 4_000 && mid < 6_000, "Mid score was {mid}");

        // Extreme low should approach 0
        let low = compute_sigmoid_normalized_score(0, 1, 5_000);
        assert!(low < 1_000, "Low score was {low}");

        // Extreme high should approach 10000
        let high = compute_sigmoid_normalized_score(10_000, 1, 5_000);
        assert!(high > 9_000, "High score was {high}");
    }

    #[test]
    fn test_confidence_score_increases_with_reviews() {
        let conf_1 = compute_confidence_score(1, 10);
        let conf_10 = compute_confidence_score(10, 10);
        let conf_100 = compute_confidence_score(100, 10);

        assert!(conf_1 < conf_10, "Confidence should increase with reviews");
        assert!(
            conf_10 < conf_100,
            "Confidence should increase with reviews"
        );
        assert!(conf_100 <= 10_000, "Confidence never exceeds 10000 bps");
    }

    #[test]
    fn test_dispute_penalty_multiplier_escalation() {
        let mult_1 = compute_dispute_penalty_multiplier(1, 4);
        let mult_2 = compute_dispute_penalty_multiplier(2, 4);
        let mult_3 = compute_dispute_penalty_multiplier(3, 4);
        let mult_4 = compute_dispute_penalty_multiplier(4, 4);
        let mult_5 = compute_dispute_penalty_multiplier(5, 4);

        assert_eq!(mult_1, 2, "1 loss = 2x multiplier");
        assert_eq!(mult_2, 4, "2 losses = 4x multiplier");
        assert_eq!(mult_3, 8, "3 losses = 8x multiplier");
        assert_eq!(mult_4, 16, "4 losses = 16x multiplier");
        assert_eq!(mult_5, 16, "5+ losses capped at 16x multiplier");
    }

    #[test]
    fn test_reset_threshold() {
        assert!(!should_reset_dispute_penalty(19, 20));
        assert!(should_reset_dispute_penalty(20, 20));
        assert!(should_reset_dispute_penalty(25, 20));
    }
}
