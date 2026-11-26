// Copyright 2024 Delego Labs
// SPDX-License-Identifier: Apache-2.0
//
// Test sub-modules for the escrow contract (issue #540).
// Each module file lives under escrow/src/tests/ and is gated behind
// `#[cfg(test)]` at the parent level in lib.rs.

pub mod fee_calculation;
// Test sub-modules for the escrow contract.
// Each module file lives under escrow/src/tests/ and is gated behind
// `#[cfg(test)]` at the parent level in lib.rs.

/// Overflow arithmetic tests — verifies `overflow-checks = true` is active and
/// that all balance/fee/yield helpers return `MathOverflow` instead of wrapping
/// (issue #536).
pub mod overflow_arithmetic;

/// Shared escrow + Blend-compatible pool fixture for the low-APY guard
/// (issue #596) and emergency pool unwind (issue #597) tests.
pub mod defi_fixture;

/// Flash loan protection guard tests — same-ledger deposit and withdrawal of a
/// staked escrow balance is rejected (issue #594).
pub mod flash_loan_guard;

/// Persistent storage TTL regressions for emergency, yield, and flash-loan
/// guard keys (issue #768).
pub mod storage_ttl;
