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
