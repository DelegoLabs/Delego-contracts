//! Cross-contract interface adapters for external DeFi protocols (issue #326).
//!
//! Protocol ABIs live here rather than inside escrow lifecycle code.
//! [`LendingPoolInterface`] and [`MockLendingPool`] retain the generic lending
//! seam used by legacy yield queries. [`blend`] exposes Blend V2's native
//! request/position interface for real supply and redemption (issue #490).

#![cfg_attr(not(test), no_std)]

pub mod blend;
mod lending_pool;
mod lifecycle;
mod mock_lending_pool;
mod reputation_reader;

pub use lending_pool::{LendingPoolClient, LendingPoolInterface};
pub use lifecycle::ContractLifecycleState;
pub use mock_lending_pool::{MockLendingPool, MockLendingPoolClient};
pub use reputation_reader::ReputationReaderTrait;
