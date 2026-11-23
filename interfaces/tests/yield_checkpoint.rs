//! Run the yield checkpointing module's unit and property tests (issue #598)
//! on their own, independently of the full escrow crate build. The test
//! bodies live alongside the module in `escrow/src/checkpoint.rs`.
#[allow(dead_code)]
#[path = "../../escrow/src/checkpoint.rs"]
mod checkpoint;
