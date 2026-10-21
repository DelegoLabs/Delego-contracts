use delego_reputation::{calculate_decayed_score, compute_fixed_point_decay};

#[test]
fn score_halves_at_each_half_life_and_converges_to_zero() {
    assert_eq!(calculate_decayed_score(10_000, 0, 10), 10_000);
    assert_eq!(calculate_decayed_score(10_000, 10, 10), 5_000);
    assert_eq!(calculate_decayed_score(10_000, 20, 10), 2_500);
    assert_eq!(calculate_decayed_score(10_000, 320, 10), 0);

    let mut previous = 10_000;
    for elapsed in 1..=320 {
        let score = calculate_decayed_score(10_000, elapsed, 10);
        assert!(score <= previous, "score increased at ledger {elapsed}");
        previous = score;
    }
}

#[test]
fn zero_half_life_preserves_the_normalized_score() {
    // A zero half-life disables decay, but the result is still normalized into
    // the legal `0..=10_000` score band (see `normalize_score_i128`).
    assert_eq!(calculate_decayed_score(u32::MAX, 100, 0), 10_000);
    assert_eq!(calculate_decayed_score(10_000, 100, 0), 10_000);
}

#[test]
fn fixed_point_decay_halves_at_half_life() {
    assert_eq!(compute_fixed_point_decay(10_000, 0), 10_000);
    assert_eq!(compute_fixed_point_decay(10_000, 518_400), 5_000);
    assert_eq!(compute_fixed_point_decay(10_000, 1_036_800), 2_500);
}

#[test]
fn fixed_point_decay_is_monotonic_and_converges() {
    let mut previous = 10_000;
    for elapsed in 1..=31_536_000u32 {
        let score = compute_fixed_point_decay(10_000, elapsed);
        assert!(score <= previous, "score increased at ledger {elapsed}");
        previous = score;
    }
    assert_eq!(previous, 0);
}

#[test]
fn fixed_point_decay_never_increases_across_half_life_boundaries() {
    // Regression: a coarse exponential approximation reports 4_998 one ledger
    // before a half-life and 5_000 at the half-life itself, i.e. the score
    // would increase across the boundary.
    let mut previous = compute_fixed_point_decay(10_000, 0);
    for elapsed in 1..=(4 * 518_400u32) {
        let score = compute_fixed_point_decay(10_000, elapsed);
        assert!(score <= previous, "score increased at ledger {elapsed}");
        previous = score;
    }
}

#[test]
fn fixed_point_decay_accuracy_within_tolerance() {
    // Reference: score * 2^(-elapsed / half_life) computed in f64.
    for days in 0..=365u32 {
        let elapsed = days.saturating_mul(17_280);
        let expected = 10_000.0 * 2f64.powf(-(elapsed as f64) / 518_400.0);
        let actual = compute_fixed_point_decay(10_000, elapsed) as f64;
        let tolerance = (expected * 0.001).max(1.0);
        assert!(
            (actual - expected).abs() <= tolerance,
            "day {days}: expected {expected}, got {actual}"
        );
    }
}
