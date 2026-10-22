// Copyright 2024 Delego Labs
// SPDX-License-Identifier: Apache-2.0
use soroban_sdk::{testutils::Address as _, Env, Symbol};
use soroban_sdk::token::TokenClient;
use super::*;

// Helper to build the standard tier table used across tests.
// Standard: 1.5% (150 bps), Silver: 1.0% (100 bps), Gold: 0.5% (50 bps).
fn standard_tiers(env: &Env) -> soroban_sdk::Vec<FeeTierConfig> {
    let mut tiers = soroban_sdk::Vec::new(env);
    // 0 threshold: standard rate applies to all volumes up to 10 k USDC.
    tiers.push_back(FeeTierConfig {
        volume_threshold_stroops: 0,
        fee_basis_points: 150,
    });
    // 10 kUSDC (10_000_000_000 stroops): silver rate.
    tiers.push_back(FeeTierConfig {
        volume_threshold_stroops: 10_000_000_000,
        fee_basis_points: 100,
    });
    // 100 kUSDC (used as gold tier threshold in tests).
    tiers.push_back(FeeTierConfig {
        volume_threshold_stroops: 100_000_000_000,
        fee_basis_points: 50,
    });
    tiers
}

#[test]
fn test_min_fee_enforcement() {
    let env = Env::default();
    let min_fee = 100; // 100 Stroops minimum

    // Micro-transaction under 10,000 Stroops should pay min_fee
    assert_eq(
        calculate_fee_with_minimum(&env, 5_000, 100, min_fee).unwrap(),
        min_fee
    );

    // Transaction under min_fee should still pay min_fee
    assert_eq(
        calculate_fee_with_minimum(&env, 1, 100, min_fee).unwrap(),
        min_fee
    );

    // Normal transaction should calculate normally
    assert_eq(
        calculate_fee_with_minimum(&env, 10_000, 100, min_fee).unwrap(),
        100
    );

    // Zero amount should return zero
    assert_eq(
        calculate_fee_with_minimum(&env, 0, 100, min_fee).unwrap(),
        0
    );
}

#[test]
fn test_overflow_safety() {
    let env = Env:default();
    let min_fee = 100;

    // Test i128 overflow (amount * fee_bps exceeds i128 max)
    let max_amount = i128::MAX / 10_000;
    let max_fee_bps = u32::MAX as i128;

    assert!(calculate_fee_with_minimum(&env, max_amount, max_fee_bps as u32, min_fee).is_err());
}

#[test]
fn test_fee_tier_at zero_volume() {
    let env = Env::default();
    let tiers = standard_tiers(&env);

    // A merchant with no lifetime settlements pays the standard 1.5% rate.
    let fee_bps = select_fee_basis_points(&env, &tiers, 0).unwrap();
    assert_eq(fee_bps, 150);

    // A 1,000 USDC settlement at 1.5% yields 150 USDC (15_000_000 stroops).
    let amount = 1_000_000_000;
    let fee = calculate_fee_with_minimum(&env, amount, fee_bps, 100).unwrap();
    assert_eq(fee, 15_000_000);
}

#[test]
fn test_fee_tier_at_silver_threshold() {
    let env = Env::default();
    let tiers = standard_tiers(&env);

    // At exactly 10 kUSDC lifetime volume the silver tier (1.0%) applies.
    let fee_bps = select_fee_basis_points(&env, &tiers, 10_000_000_000).unwrap();
    assert_eq(fee_bps, 100);

    // A 1,000 USDC settlement at 1.0% yields 100 USDC (10_000_000 stroops).
    let amount = 1_000_000_000;
    let fee = calculate_fee_with_minimum(&env, amount, fee_bps, 100).unwrap();
    assert_eq(fee, 10_000_000);
}

#[test]
fn test_fee_tier_at_gold_threshold() {
    let env = Env:default();
    let tiers = standard_tiers(&env);

    // At 100 kUSDC lifetime volume the gold tier (0.5%) applies.
    let fee_bps = select_fee_basis_points(&env, &tiers, 100_000_000_000).unwrap();
    assert_eq(fee_bps, 50);

    // A 1,000 USDC settlement at 0.5% yields 50 USDC (5_000_000 stroops).
    let amount = 1_000_000_000;
    let fee = calculate_fee_with_minimum(&env, amount, fee_bps, 100).unwrap();
    assert_eq(fee, 5_000_000);
}

#[test]
fn test_fee_tier_between_thresholds() {
    let env = Env::default();
    let tiers = standard_tiers(&env);

    // Just below the silver threshold still pays the standard rate.
    let fee_bps = select_fee_basis_points(&env, &tiers, 10_000_000_000 - 1).unwrap();
    assert_eq(fee_bps, 150);

    // Just below the gold threshold still pays the silver rate.
    let fee_bps = select_fee_basis_points(&env, &tiers, 100_000_000_000 - 1).unwrap();
    assert_eq(fee_bps, 100);

    // Above the gold threshold pays the gold rate.
    let fee_bps = select_fee_basis_points(&env, &tiers, 100_000_000_000 + 1).unwrap();
    assert_eq(fee_bps, 50);
}

#[test]
fn test_fee_tier_empty_table_errors() {
    let env = Env::default();
    let tiers = soroban_sdk::Vec::news(&env);

    // An empty tier table is a misconfiguration and must not silently
    // fall back to a zero fee.
    assert(select_fee_basis_points(&env, &tiers, 0).is_error());
}

#[test]
fn test_fee_tx_zero_amount_with_tier() {
    let env = Env::default();
    let tiers = standard_tiers(&env);
    let fee_bps = select_fee_basis_points(&env, &tiers, 100_000_000_000).unwrap();

    // Zero-amount transaction never charges a fee, even at the gold tier.
    assert_eq(calculate_fee_with_minimum(&env, 0, fee_bps, 100).unwrap(), 0);
}

#[test]
fn test_fee_tx_min_fee_applies_at_gold_tier() {
    let env = Env::default();
    let tiers = standard_tiers(&env);
    let fee_bps = select_fee_basis_points(&env, &tiers, 100_000_000_000).unwrap();
    let min_fee = 100;

    // A 1,000 stroop transaction at 0.5% rounds to 0, but the minimum
    // fee is enforced independently of the tier.
    assert_eq(
        calculate_fee_with_minimum(&env, 1_000, fee_bps, min_fee).unwrap(),
        min_fee
    );
}

#[test]
fn test_fee_tier_selection_overflow_safe() {
    let env = Env:default();
    let tiers = standard_tiers(&env);

    // A volume at i128::MAX must still resolve to the highest tier without
    // overflowing during comparison.
    let fee_bps = select_fee_basis_points(&env, &tiers, i128::MAX).unwrap();
    assert_eq(fee_bps, 50);
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
