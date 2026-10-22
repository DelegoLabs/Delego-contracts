use soroban_sdk::{contracttype, Symbol};

/// Captured state recorded when a contract enters emergency pause.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PausedState {
    /// Ledger sequence at which the pause was activated.
    pub paused_at_ledger: u32,
    /// Human-readable reason for the pause (e.g. `"vuln_detected"`).
    pub reason: Symbol,
}

/// Contract-wide lifecycle state shared by emergency-pausable contracts.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContractLifecycleState {
    /// Normal operations are enabled.
    Active,
    /// Emergency pause — details in the enclosed [`PausedState`].
    Paused(PausedState),
}
