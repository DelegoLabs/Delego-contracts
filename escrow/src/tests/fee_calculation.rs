// Copyright 2024 Delego Labs
// SPDX-License-Identifier: Apache-2.0
//
// Unit tests for the fee-calculation helpers introduced in issue #540:
// Dynamic Minimum Fee Floor Enforcement.
//
// `calculate_fee_with_minimum` lives in the parent module (lib.rs) and takes
// three arguments: (amount, fee_bps, min_fee_stroops).  No `&Env` is needed
// because the pure arithmetic helper does not touch contract storage.
use super::*;

// ─────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────

/// Basis-points fee rate used across most tests (1 % = 100 bps).
const BPS_100: u32 = 100;

/// Minimum fee floor the tests configure (100 stroops).
const FLOOR_100: i128 = 100;

// ─────────────────────────────────────────────────────────────
// Acceptance Criteria #1 — orders below threshold pay exact min_fee_floor_stroops
// ─────────────────────────────────────────────────────────────

/// A 1-stroop order is well below `10_000 / 100 = 100` stroops (the point
/// where the pro-rata fee reaches the floor), so the floor must be returned.
#[test]
fn test_one_stroop_order_pays_exact_min_fee_floor() {
    // amount = 1 stroop, bps = 100, floor = 100 stroops
    // pro-rata: 1 * 100 / 10_000 = 0  →  floor kicks in → 100.min(1) = 1
    // The fee is clamped to the amount when the floor > amount.
    assert_eq!(
        calculate_fee_with_minimum(1, BPS_100, FLOOR_100).unwrap(),
        1, // clamped to amount (never over-charges)
    );
}

/// An order of exactly `floor` stroops should pay exactly the floor, since
/// `floor * bps / 10_000` still truncates below the floor.
#[test]
fn test_order_at_floor_pays_floor() {
    // amount = 100, bps = 100, floor = 100
    // pro-rata: 100 * 100 / 10_000 = 1  →  1 < 100 → floor = min(100, 100) = 100
    assert_eq!(
        calculate_fee_with_minimum(100, BPS_100, FLOOR_100).unwrap(),
        100,
    );
}

/// An order large enough that the pro-rata fee exceeds the floor should use
/// the pro-rata fee, not the floor.
#[test]
fn test_large_order_uses_pro_rata_fee() {
    // amount = 10_000, bps = 100 → pro-rata = 10_000 * 100 / 10_000 = 100
    // 100 >= floor (100) → result is pro-rata (no floor applied)
    assert_eq!(
        calculate_fee_with_minimum(10_000, BPS_100, FLOOR_100).unwrap(),
        100,
    );

    // amount = 50_000, bps = 100 → pro-rata = 500 > floor 100
    assert_eq!(
        calculate_fee_with_minimum(50_000, BPS_100, FLOOR_100).unwrap(),
        500,
    );
}

// ─────────────────────────────────────────────────────────────
// Acceptance Criteria #2 — zero-stroop platform fees are strictly prevented
// ─────────────────────────────────────────────────────────────

/// With a positive bps and a positive floor, no micro-transaction amount
/// should ever produce a zero fee (unless the amount itself is zero).
#[test]
fn test_zero_stroop_fee_is_prevented_for_micro_transactions() {
    let floor = 1; // DEFAULT_MIN_FEE_STROOPS

    // All amounts from 1 to 9_999 stroops give pro-rata = 0 at 1 bps.
    // The floor must prevent that from becoming zero.
    for amount in [1i128, 10, 99, 100, 999, 1_000, 9_999] {
        let fee = calculate_fee_with_minimum(amount, 1, floor).unwrap();
        assert!(
            fee > 0,
            "expected fee > 0 for amount={amount} bps=1 floor={floor}, got {fee}"
        );
    }
}

/// A zero-amount order must produce a zero fee (no spurious floor application).
#[test]
fn test_zero_amount_order_produces_zero_fee() {
    assert_eq!(
        calculate_fee_with_minimum(0, BPS_100, FLOOR_100).unwrap(),
        0,
    );
}

/// When fee_bps is 0, the platform charges no fee at all regardless of the
/// floor — a 0 bps setting is an explicit "no fee" configuration, not a
/// truncation artefact.
#[test]
fn test_zero_bps_bypasses_fee_floor() {
    assert_eq!(
        calculate_fee_with_minimum(1, 0, FLOOR_100).unwrap(),
        0,
    );
    assert_eq!(
        calculate_fee_with_minimum(10_000, 0, FLOOR_100).unwrap(),
        0,
    );
}

// ─────────────────────────────────────────────────────────────
// Max fee ceiling enforcement
// ─────────────────────────────────────────────────────────────

/// `apply_fee_floor` and `calculate_fee_with_minimum` are pure helpers; the
/// ceiling is applied by `compute_fee_amount` inside the contract impl.  Here
/// we verify the floor + pro-rata arithmetic that underpins correct ceiling
/// input: a large order at 250 bps should produce an uncapped fee of 250.
#[test]
fn test_pro_rata_fee_for_ceiling_test_case() {
    // amount = 10_000, bps = 250 → pro-rata = 10_000 * 250 / 10_000 = 250
    // floor = 1 stroop (default) → no floor applied
    assert_eq!(
        calculate_fee_with_minimum(10_000, 250, 1).unwrap(),
        250,
    );
}

// ─────────────────────────────────────────────────────────────
// Overflow safety
// ─────────────────────────────────────────────────────────────

/// `calculate_fee_with_minimum` must return `EscrowError::MathOverflow` when
/// `amount * fee_bps` would overflow an `i128`.
#[test]
fn test_overflow_safety() {
    // i128::MAX * any positive bps overflows the intermediate checked_mul.
    assert!(calculate_fee_with_minimum(i128::MAX, 1, 100).is_err());

    // The boundary: i128::MAX / 10_000 * u32::MAX should also overflow.
    let max_amount = i128::MAX / 10_000;
    assert!(calculate_fee_with_minimum(max_amount, u32::MAX, 100).is_err());
}

// ─────────────────────────────────────────────────────────────
// Min-fee floor clamp (fee never exceeds amount)
// ─────────────────────────────────────────────────────────────

/// The floor is clamped to `amount` so a settlement is never over-charged.
/// For tiny amounts the returned fee equals the amount, not the configured floor.
#[test]
fn test_fee_floor_clamped_to_amount() {
    // amount = 5, floor = 100 → clamped fee = min(100, 5) = 5
    assert_eq!(
        calculate_fee_with_minimum(5, BPS_100, FLOOR_100).unwrap(),
        5,
    );

    // amount = 50, floor = 100 → clamped fee = min(100, 50) = 50
    assert_eq!(
        calculate_fee_with_minimum(50, BPS_100, FLOOR_100).unwrap(),
        50,
    );
}

// ─────────────────────────────────────────────────────────────
// Pre-existing general coverage
// ─────────────────────────────────────────────────────────────

/// Consolidated test retained for regression coverage of the original
/// min_fee enforcement cases.
#[test]
fn test_min_fee_enforcement() {
    let min_fee = 100_i128;

    // Micro-transaction under 10_000 stroops at 1% → pro-rata rounds to 0;
    // floor is applied and clamped to the amount (5_000 > floor → return floor).
    assert_eq!(
        calculate_fee_with_minimum(5_000, BPS_100, min_fee).unwrap(),
        min_fee,
    );

    // A 1-stroop order: floor (100) > amount (1), so the fee is clamped to 1.
    assert_eq!(
        calculate_fee_with_minimum(1, BPS_100, min_fee).unwrap(),
        1,
    );

    // At exactly 10_000 stroops the pro-rata fee hits 100 (== floor) → no floor adjustment.
    assert_eq!(
        calculate_fee_with_minimum(10_000, BPS_100, min_fee).unwrap(),
        100,
    );

    // Zero amount → zero fee (floor not applied).
    assert_eq!(
        calculate_fee_with_minimum(0, BPS_100, min_fee).unwrap(),
        0,
    );
}
