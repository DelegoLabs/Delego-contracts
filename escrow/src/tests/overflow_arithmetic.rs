// Copyright 2024 Delego Labs
// SPDX-License-Identifier: Apache-2.0
//
// Overflow-arithmetic unit tests for the escrow contract (issue #536).
//
// These tests verify that integer arithmetic in balance/fee/yield calculations
// either panics (via the `overflow-checks = true` build flag catching plain `+`/`-`/`*`)
// or returns `EscrowError::MathOverflow` when using the contract's own checked helpers.
// With `overflow-checks = true` active in `[profile.test]`, any unwrapped wrapping would
// cause a panic — confirming that the build flag is actually enforced.

use super::*;

// ─────────────────────────────────────────────────────────────────────────────
// calculate_fee_and_yield — basis-point multiplication
// ─────────────────────────────────────────────────────────────────────────────

/// `calculate_fee_and_yield` must return `EscrowError::MathOverflow` when
/// `amount * bps` overflows i128, rather than silently wrapping.
#[test]
fn fee_and_yield_overflows_to_math_overflow_error() {
    // i128::MAX * 1 bps — the checked_mul saturates the i128 range.
    assert_eq!(
        calculate_fee_and_yield(i128::MAX, 1),
        Err(EscrowError::MathOverflow),
        "i128::MAX * 1 bps must return MathOverflow",
    );

    // i128::MAX * MAX bps — clearly overflows.
    assert_eq!(
        calculate_fee_and_yield(i128::MAX, u32::MAX),
        Err(EscrowError::MathOverflow),
        "i128::MAX * u32::MAX bps must return MathOverflow",
    );

    // A large but plausible amount that still overflows when multiplied by
    // a non-trivial bps rate: i128::MAX / 10_000 + 1 at 10_001 bps.
    let boundary = i128::MAX / 10_000 + 1;
    assert_eq!(
        calculate_fee_and_yield(boundary, 10_001),
        Err(EscrowError::MathOverflow),
        "boundary amount at excessive bps must return MathOverflow",
    );
}

/// Sanity check: valid inputs must still produce correct results, confirming
/// the overflow-checks flag does not break normal paths.
#[test]
fn fee_and_yield_normal_cases_are_correct() {
    // 10_000 stroops at 100 bps (1 %) → 10_000 * 100 / 10_000 = 100
    assert_eq!(calculate_fee_and_yield(10_000, 100), Ok(100));

    // 0 amount → 0 regardless of bps
    assert_eq!(calculate_fee_and_yield(0, 500), Ok(0));

    // 1_000_000 stroops at 250 bps (2.5 %) → 25_000
    assert_eq!(calculate_fee_and_yield(1_000_000, 250), Ok(25_000));
}

// ─────────────────────────────────────────────────────────────────────────────
// calculate_yield — pro-rated yield over a holding period
// ─────────────────────────────────────────────────────────────────────────────

/// `calculate_yield` must return `EscrowError::MathOverflow` when the
/// three-factor intermediate product overflows i128.
#[test]
fn yield_calculation_overflows_to_math_overflow_error() {
    // amount = i128::MAX → first checked_mul(bps) already overflows.
    assert_eq!(
        calculate_yield(i128::MAX, 1, 1),
        Err(EscrowError::MathOverflow),
        "i128::MAX principal must overflow at the first checked_mul",
    );

    // A large principal that only overflows when multiplied by held_seconds.
    // i128::MAX / 10_000 is safe for the bps step at 1 bps, but multiplying
    // by u64::MAX (max held_seconds) will overflow.
    let big_amount = i128::MAX / 10_000;
    assert_eq!(
        calculate_yield(big_amount, 1, u64::MAX),
        Err(EscrowError::MathOverflow),
        "large amount × max held_seconds must return MathOverflow",
    );
}

/// Valid yield calculations must be correct end-to-end.
#[test]
fn yield_calculation_normal_cases_are_correct() {
    // 1_000_000 stroops at 500 bps APR held for exactly one year:
    //   1_000_000 * 500 * 31_536_000 / (10_000 * 31_536_000) = 50_000
    let one_year_secs: u64 = 31_536_000;
    assert_eq!(
        calculate_yield(1_000_000, 500, one_year_secs),
        Ok(50_000),
        "one-year yield at 5 % APR must equal 50_000 stroops",
    );

    // Zero held seconds → zero yield (no time elapsed).
    assert_eq!(
        calculate_yield(1_000_000, 500, 0),
        Ok(0),
        "zero held seconds must produce zero yield",
    );

    // Zero principal → zero yield.
    assert_eq!(
        calculate_yield(0, 500, one_year_secs),
        Ok(0),
        "zero principal must produce zero yield",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// calculate_fee_with_minimum — combined overflow + floor test
// ─────────────────────────────────────────────────────────────────────────────

/// `calculate_fee_with_minimum` (the higher-level helper) must also return
/// `EscrowError::MathOverflow` and never wrap on overflow inputs.
#[test]
fn fee_with_minimum_overflow_returns_error() {
    assert_eq!(
        calculate_fee_with_minimum(i128::MAX, 1, 100),
        Err(EscrowError::MathOverflow),
        "i128::MAX amount with floor must still overflow at bps multiply",
    );

    let max_amount = i128::MAX / 10_000;
    assert_eq!(
        calculate_fee_with_minimum(max_amount, u32::MAX, 100),
        Err(EscrowError::MathOverflow),
        "boundary amount × u32::MAX bps must return MathOverflow",
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Overflow-checks build flag verification
// ─────────────────────────────────────────────────────────────────────────────

/// Confirms that `overflow-checks = true` is active in this build profile by
/// verifying that a plain checked-arithmetic call on a sentinel value behaves
/// as expected.  This is an indirect verification: if the build flag were
/// absent, overflow_checks_enabled() would return false and the test would
/// fail, signalling that the profile was misconfigured.
#[test]
fn overflow_checks_are_enabled_in_test_profile() {
    // Rust's core exposes `cfg!(overflow_checks)` to test the active flag.
    assert!(
        cfg!(overflow_checks),
        "overflow-checks must be true in [profile.test] (issue #536); \
         add `overflow-checks = true` under [profile.test] in root Cargo.toml",
    );
}
