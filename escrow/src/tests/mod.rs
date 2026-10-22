// Copyright 2024 Delego Labs
// SPDX-License-Identifier: Apache-2.0
//
// Test sub-modules for the escrow contract (issue #540).
// Each module file lives under escrow/src/tests/ and is gated behind
// `#[cfg(test)]` at the parent level in lib.rs.

pub mod fee_calculation;
