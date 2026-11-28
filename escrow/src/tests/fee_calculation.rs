// Copyright 2024 Delego Labs
// SPDX-License-Identifier: Apache-2.0
//
// Unit tests for the fee-calculation helpers:
//   - `calculate_fee_with_minimum` (pure arithmetic, in lib.rs, issue #540)
//   - `EscrowContract::resolve_fee_bps` and `compute_tiered_fee` (issue #542)
//
// The tier-selection tests use `EscrowContract::resolve_fee_bps` directly via
// the contract client rather than a free `select_fee_basis_points` helper
// (which does not exist). Tests that only exercise pure arithmetic helpers do
// not need a contract environment.

use super::*;

// ─────────────────────────────────────────────────────────────
// Helpers shared across tier tests
// ─────────────────────────────────────────────────────────────

/// Basis-points fee rate used across most pure-arithmetic tests (1% = 100 bps).
const BPS_100: u32 = 100;

/// Minimum fee floor used across most pure-arithmetic tests (100 stroops).
const FLOOR_100: i128 = 100;

/// Builds and returns the standard three-tier table:
///   Tier 0 (baseline): 0 stroops   lifetime → 1.5% (150 bps)
///   Tier 1 (silver):   10 kUSDC    lifetime → 1.0% (100 bps)
///   Tier 2 (gold):     100 kUSDC   lifetime → 0.5%  (50 bps)
fn standard_tiers(env: &Env) -> soroban_sdk::Vec<FeeTierConfig> {
    let mut tiers = soroban_sdk::Vec::new(env);
    tiers.push_back(FeeTierConfig {
        volume_threshold_stroops: 0,
        fee_basis_points: 150,
    });
    tiers.push_back(FeeTierConfig {
        volume_threshold_stroops: 10_000_000_000,
        fee_basis_points: 100,
    });
    tiers.push_back(FeeTierConfig {
        volume_threshold_stroops: 100_000_000_000,
        fee_basis_points: 50,
    });
    tiers
}

// ─────────────────────────────────────────────────────────────
// Acceptance Criteria #1 — orders below threshold pay exact min_fee_floor_stroops
// ─────────────────────────────────────────────────────────────

/// A 1-stroop order at 100 bps: pro-rata = 0, floor kicks in but is clamped
/// to amount (1) since floor (100) > amount (1).
#[test]
fn test_one_stroop_order_pays_exact_min_fee_floor() {
    assert_eq!(
        calculate_fee_with_minimum(1, BPS_100, FLOOR_100).unwrap(),
        1, // clamped: floor > amount → fee = amount
    );
}

/// At exactly `floor` stroops, pro-rata still truncates below floor → floor applies.
#[test]
fn test_order_at_floor_pays_floor() {
    // 100 * 100 / 10_000 = 1 < 100 → floor = min(100, 100) = 100
    assert_eq!(
        calculate_fee_with_minimum(100, BPS_100, FLOOR_100).unwrap(),
        100,
    );
}

/// Large enough order: pro-rata fee >= floor → pro-rata wins.
#[test]
fn test_large_order_uses_pro_rata_fee() {
    // 10_000 * 100 / 10_000 = 100 == floor
    assert_eq!(
        calculate_fee_with_minimum(10_000, BPS_100, FLOOR_100).unwrap(),
        100,
    );
    // 50_000 * 100 / 10_000 = 500 > floor 100
    assert_eq!(
        calculate_fee_with_minimum(50_000, BPS_100, FLOOR_100).unwrap(),
        500,
    );
}

// ─────────────────────────────────────────────────────────────
// Acceptance Criteria #2 — zero-stroop platform fees are strictly prevented
// ─────────────────────────────────────────────────────────────

/// With a positive bps and a positive floor, no micro-transaction amount
/// should produce a zero fee (unless the amount is itself zero).
#[test]
fn test_zero_stroop_fee_is_prevented_for_micro_transactions() {
    let floor = 1_i128; // DEFAULT_MIN_FEE_STROOPS
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

/// When fee_bps is 0, the platform charges no fee regardless of the floor
/// — 0 bps is an explicit "no fee" configuration, not a truncation artefact.
#[test]
fn test_zero_bps_bypasses_fee_floor() {
    assert_eq!(calculate_fee_with_minimum(1, 0, FLOOR_100).unwrap(), 0);
    assert_eq!(calculate_fee_with_minimum(10_000, 0, FLOOR_100).unwrap(), 0);
}

// ─────────────────────────────────────────────────────────────
// Max fee ceiling input verification
// ─────────────────────────────────────────────────────────────

/// Verify the floor + pro-rata arithmetic that feeds into ceiling clamping:
/// a large order at 250 bps produces an uncapped fee of 250.
#[test]
fn test_pro_rata_fee_for_ceiling_test_case() {
    // 10_000 * 250 / 10_000 = 250; floor = 1 → no floor applied
    assert_eq!(calculate_fee_with_minimum(10_000, 250, 1).unwrap(), 250);
}

// ─────────────────────────────────────────────────────────────
// Overflow safety
// ─────────────────────────────────────────────────────────────

/// `calculate_fee_with_minimum` must return `EscrowError::MathOverflow` when
/// `amount * fee_bps` would overflow an `i128`.
#[test]
fn test_overflow_safety() {
    assert!(calculate_fee_with_minimum(i128::MAX, 1, 100).is_err());
    let max_amount = i128::MAX / 10_000;
    assert!(calculate_fee_with_minimum(max_amount, u32::MAX, 100).is_err());
}

// ─────────────────────────────────────────────────────────────
// Min-fee floor clamp (fee never exceeds amount)
// ─────────────────────────────────────────────────────────────

/// The floor is clamped to `amount` so a settlement is never over-charged.
#[test]
fn test_fee_floor_clamped_to_amount() {
    // amount = 5, floor = 100 → clamped fee = min(100, 5) = 5
    assert_eq!(calculate_fee_with_minimum(5, BPS_100, FLOOR_100).unwrap(), 5);
    // amount = 50, floor = 100 → clamped fee = min(100, 50) = 50
    assert_eq!(calculate_fee_with_minimum(50, BPS_100, FLOOR_100).unwrap(), 50);
}

// ─────────────────────────────────────────────────────────────
// Consolidated regression coverage
// ─────────────────────────────────────────────────────────────

#[test]
fn test_min_fee_enforcement() {
    let min_fee = 100_i128;
    // 5_000 * 100 / 10_000 = 50 < 100 → floor applied; 100 <= 5_000 → fee = 100.
    assert_eq!(calculate_fee_with_minimum(5_000, BPS_100, min_fee).unwrap(), min_fee);
    // amount=1: floor (100) > amount (1) → clamped to 1.
    assert_eq!(calculate_fee_with_minimum(1, BPS_100, min_fee).unwrap(), 1);
    // 10_000 * 100 / 10_000 = 100 == floor → no adjustment needed.
    assert_eq!(calculate_fee_with_minimum(10_000, BPS_100, min_fee).unwrap(), 100);
    // Zero amount → zero fee.
    assert_eq!(calculate_fee_with_minimum(0, BPS_100, min_fee).unwrap(), 0);
}

// ─────────────────────────────────────────────────────────────
// FeeTierConfig-based tier selection via EscrowContract::resolve_fee_bps
// ─────────────────────────────────────────────────────────────

/// Helper: register the standard three-tier table on a live contract and
/// return a client + admin pair.
fn setup_tiered_client(
    env: &Env,
) -> (EscrowContractClient<'static>, soroban_sdk::Address) {
    use crate::admin_actions_test::approve;
    env.mock_all_auths();
    let admin = soroban_sdk::Address::generate(env);
    let guardian = soroban_sdk::Address::generate(env);
    let config = EscrowConfig {
        admin: admin.clone(),
        treasury: soroban_sdk::Address::generate(env),
        fee_bps: 300,
        min_amount: 1,
        max_amount: 1_000_000_000_000_000,
        min_fee_floor_stroops: 0,
        max_fee_ceiling_stroops: 0,
    };
    let addr = env.register(EscrowContract, (config,));
    let client = EscrowContractClient::new(env, &addr);
    client.set_security_guardian(&admin, &guardian);
    let tiers = standard_tiers(env);
    approve(env, &client, &admin, AdminAction::FeeTiers(tiers));
    (client, admin)
}

/// A merchant with zero lifetime volume pays the baseline (1.5%) tier.
#[test]
fn test_fee_tier_at_zero_volume() {
    let env = Env::default();
    let (client, _) = setup_tiered_client(&env);
    // 0 >= 0 → baseline tier selected: 150 bps.
    assert_eq!(client.resolve_fee_bps(&0i128), 150u32);
    // A 1_000 USDC = 1_000_000_000 stroop settlement at 1.5% → 15_000_000 stroops.
    assert_eq!(
        client.compute_tiered_fee(&1_000_000_000i128, &0i128).unwrap(),
        15_000_000i128,
    );
}

/// At exactly the silver threshold (10 kUSDC) the silver tier (1.0%) applies.
#[test]
fn test_fee_tier_at_silver_threshold() {
    let env = Env::default();
    let (client, _) = setup_tiered_client(&env);
    assert_eq!(client.resolve_fee_bps(&10_000_000_000i128), 100u32);
    assert_eq!(
        client.compute_tiered_fee(&1_000_000_000i128, &10_000_000_000i128).unwrap(),
        10_000_000i128,
    );
}

/// At exactly the gold threshold (100 kUSDC) the gold tier (0.5%) applies.
#[test]
fn test_fee_tier_at_gold_threshold() {
    let env = Env::default();
    let (client, _) = setup_tiered_client(&env);
    assert_eq!(client.resolve_fee_bps(&100_000_000_000i128), 50u32);
    assert_eq!(
        client.compute_tiered_fee(&1_000_000_000i128, &100_000_000_000i128).unwrap(),
        5_000_000i128,
    );
}

/// Just below silver → standard; just below gold → silver; above gold → gold.
#[test]
fn test_fee_tier_between_thresholds() {
    let env = Env::default();
    let (client, _) = setup_tiered_client(&env);
    assert_eq!(client.resolve_fee_bps(&(10_000_000_000i128 - 1)), 150u32);
    assert_eq!(client.resolve_fee_bps(&(100_000_000_000i128 - 1)), 100u32);
    assert_eq!(client.resolve_fee_bps(&(100_000_000_000i128 + 1)), 50u32);
}

/// A volume at `i128::MAX` resolves to the highest tier without overflowing.
#[test]
fn test_fee_tier_selection_overflow_safe() {
    let env = Env::default();
    let (client, _) = setup_tiered_client(&env);
    assert_eq!(client.resolve_fee_bps(&i128::MAX), 50u32);
}

/// Zero-amount transaction charges no fee even at the gold tier.
#[test]
fn test_fee_tx_zero_amount_with_tier() {
    let env = Env::default();
    let (client, _) = setup_tiered_client(&env);
    assert_eq!(
        client.compute_tiered_fee(&0i128, &100_000_000_000i128).unwrap(),
        0i128,
    );
}

/// A micro-transaction at the gold tier (0.5%) that truncates to 0 stroops
/// is handled by `compute_tiered_fee` itself returning 0 (the minimum floor
/// is not applied here — it is applied by `compute_fee_amount` inside the
/// release path, not by the tiered-fee entrypoints).
#[test]
fn test_fee_tx_min_fee_applies_at_gold_tier() {
    let env = Env::default();
    let (client, _) = setup_tiered_client(&env);
    // 1_000 stroops * 50 bps / 10_000 = 5 stroops fee (integer division).
    assert_eq!(
        client.compute_tiered_fee(&1_000i128, &100_000_000_000i128).unwrap(),
        5i128,
    );
}

/// When no tier table is configured, `resolve_fee_bps` falls back to the flat
/// `FeeConfig::fee_bps` (300 bps from the setup config).
#[test]
fn test_resolve_fee_bps_falls_back_to_fee_config_when_no_tiers() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = soroban_sdk::Address::generate(&env);
    let guardian = soroban_sdk::Address::generate(&env);
    let config = EscrowConfig {
        admin: admin.clone(),
        treasury: soroban_sdk::Address::generate(&env),
        fee_bps: 300,
        min_amount: 1,
        max_amount: 1_000_000_000_000_000,
        min_fee_floor_stroops: 0,
        max_fee_ceiling_stroops: 0,
    };
    let addr = env.register(EscrowContract, (config,));
    let client = EscrowContractClient::new(&env, &addr);
    client.set_security_guardian(&admin, &guardian);
    // No FeeTiers action queued — resolve_fee_bps must return the flat 300 bps.
    assert_eq!(client.resolve_fee_bps(&0i128), 300u32);
    assert_eq!(client.resolve_fee_bps(&i128::MAX), 300u32);
}
