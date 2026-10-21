use soroban_sdk::{contracttype, Symbol};

/// Contract-wide lifecycle state shared by emergency-pausable contracts.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContractLifecycleState {
    /// Normal operations are enabled.
    Active,
    /// Emergency pause with the ledger and reason recorded for auditing.
    Paused { paused_at_ledger: u32, reason: Symbol },
}