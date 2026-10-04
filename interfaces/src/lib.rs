//! Cross-contract interface adapters for external DeFi protocols (issue #326).
//!
//! Delego core contracts must not embed any single lending protocol's private
//! ABI.  Instead they hold an `Address` for the configured pool and drive it
//! exclusively through the [`LendingPoolInterface`] seam defined here; real
//! external protocols (Blend, Aqua, ...) ship a thin adapter implementing it,
//! and [`MockLendingPool`] simulates a pool locally for unit and integration
//! tests.

#![cfg_attr(not(test), no_std)]

mod lending_pool;
mod mock_lending_pool;

pub use lending_pool::{LendingPoolClient, LendingPoolInterface};
pub use mock_lending_pool::{MockLendingPool, MockLendingPoolClient};
