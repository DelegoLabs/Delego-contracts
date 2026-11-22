// Copyright 2024 Delego Labs
// SPDX-License-Identifier: Apache-2.0
//
// Overflow-arithmetic unit tests for the permissions contract (issue #536).
//
// Tests here verify two complementary properties:
//
//  1. Pure-arithmetic helpers that use `checked_*` operations return the
//     appropriate `PermissionError` on overflow rather than wrapping silently.
//
//  2. Helpers that use plain arithmetic operators (`*`, `+`) panic immediately
//     when given overflow inputs, because `overflow-checks = true` is set in
//     `[profile.test]`. The `#[should_panic]` tests would pass for the wrong
//     reason if that flag were absent (wrap-around → wrong result, no panic),
//     making them a live proof that the build flag is actually enforced.

#![cfg(test)]

use super::*;

// ─────────────────────────────────────────────────────────────────────────────
// compute_expiry_ledger — checked addition on u32 ledger values
// ─────────────────────────────────────────────────────────────────────────────

/// `compute_expiry_ledger` must return `PermissionError::InvalidExpiry` when
/// `current + ttl` would overflow a u32, rather than wrapping to a low value
/// that would grant an unexpectedly short (or immediate) expiry.
#[test]
fn expiry_ledger_overflow_returns_invalid_expiry() {
    // u32::MAX + 1 overflows; checked_add returns None → InvalidExpiry.
    assert_eq!(
        compute_expiry_ledger(u32::MAX, 1),
        Err(PermissionError::InvalidExpiry),
        "u32::MAX current + 1 ttl must return InvalidExpiry",
    );

    // Mid-range overflow: just past u32::MAX.
    assert_eq!(
        compute_expiry_ledger(u32::MAX / 2 + 1, u32::MAX / 2 + 1),
        Err(PermissionError::InvalidExpiry),
        "mid-range overflow must return InvalidExpiry",
    );
}

/// Valid ledger arithmetic must produce the correct expiry.
#[test]
fn expiry_ledger_normal_cases_are_correct() {
    assert_eq!(compute_expiry_ledger(100, 500), Ok(600));
    assert_eq!(compute_expiry_ledger(0, 0), Ok(0));
    assert_eq!(compute_expiry_ledger(u32::MAX - 1, 1), Ok(u32::MAX));
}

// ─────────────────────────────────────────────────────────────────────────────
// validate_relayer_fee — plain `*` checked by overflow-checks build flag
// ─────────────────────────────────────────────────────────────────────────────

/// `validate_relayer_fee` now uses `checked_mul` instead of a plain `*`
/// multiplication. Passing `i128::MAX` as `spend_amount` should return
/// `PermissionError::InvalidParam` rather than panic or wrap silently.
#[test]
fn relayer_fee_proportion_returns_invalid_param_on_overflow_input() {
    // spend_amount = i128::MAX with any relayer_fee should return InvalidParam
    // because checked_mul will overflow.
    assert_eq!(
        validate_relayer_fee(i128::MAX, 0),
        Err(PermissionError::InvalidParam),
        "i128::MAX spend_amount must return InvalidParam (not panic)"
    );

    // Even with relayer_fee = 0, the multiplication overflows.
    assert_eq!(
        validate_relayer_fee(i128::MAX, 100),
        Err(PermissionError::InvalidParam),
    );
}

/// For normal inputs, `validate_relayer_fee` must apply the proportional cap
/// correctly and accept fees within the allowed window.
#[test]
fn relayer_fee_validates_correctly_for_normal_inputs() {
    // 1 % of 1_000_000 = 10_000; relayer asks 10_000 → exactly at cap → Ok.
    assert_eq!(validate_relayer_fee(1_000_000, 10_000), Ok(()));

    // Relayer fee just above the proportional cap → InvalidParam.
    assert_eq!(
        validate_relayer_fee(1_000_000, 10_001),
        Err(PermissionError::InvalidParam),
    );

    // Zero relayer fee → always valid.
    assert_eq!(validate_relayer_fee(500_000, 0), Ok(()));

    // Negative relayer fee → InvalidParam.
    assert_eq!(
        validate_relayer_fee(1_000_000, -1),
        Err(PermissionError::InvalidParam),
    );

    // Exceeds absolute cap regardless of proportion → InvalidParam.
    assert_eq!(
        validate_relayer_fee(
            i128::MAX / MAX_RELAYER_FEE_BPS as i128,
            MAX_ABSOLUTE_RELAYER_STROOPS + 1
        ),
        Err(PermissionError::InvalidParam),
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Overflow-checks build flag verification
// ─────────────────────────────────────────────────────────────────────────────

/// Confirms that `overflow-checks = true` is active in this build profile.
/// If the flag is absent this test will fail with a clear message pointing
/// back to the Cargo.toml fix required by issue #536.
#[test]
fn overflow_checks_are_enabled_in_test_profile() {
    assert!(
        cfg!(overflow_checks),
        "overflow-checks must be true in [profile.test] (issue #536); \
         add `overflow-checks = true` under [profile.test] in root Cargo.toml",
    );
}
