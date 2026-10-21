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
}
