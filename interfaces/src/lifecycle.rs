use soroban_sdk::{contracttype, Symbol};

/// Pause metadata stored alongside the `Paused` lifecycle state.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PauseInfo {
    /// Ledger sequence at which the contract was paused.
    pub paused_at_ledger: u32,
    /// Short symbolic reason for the pause (e.g. `"vuln_detected"`).
    pub reason: Symbol,
    /// Optional ledger sequence at which the pause automatically expires.
    pub expires_at_ledger: Option<u32>,
}

/// Contract-wide lifecycle state shared by emergency-pausable contracts.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContractLifecycleState {
    /// Normal operations are enabled.
    Active,
    /// Emergency pause — carries audit metadata in a [`PauseInfo`] struct.
    Paused(PauseInfo),
    /// The contract is retired and no longer accepts new activity.
    Deprecated,
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{symbol_short, Env, IntoVal, Val};

    #[test]
    fn paused_variant_carries_pause_info() {
        let info = PauseInfo {
            paused_at_ledger: 10,
            reason: symbol_short!("vuln"),
            expires_at_ledger: Some(20),
        };
        let state = ContractLifecycleState::Paused(info.clone());
        assert_eq!(state, ContractLifecycleState::Paused(info));
        assert_ne!(state, ContractLifecycleState::Active);
        assert_ne!(ContractLifecycleState::Active, ContractLifecycleState::Deprecated);
    }

    #[test]
    fn lifecycle_state_round_trips_through_val() {
        let env = Env::default();
        let state = ContractLifecycleState::Paused(PauseInfo {
            paused_at_ledger: 1,
            reason: symbol_short!("audit"),
            expires_at_ledger: None,
        });
        let val: Val = state.clone().into_val(&env);
        let back: ContractLifecycleState = val.into_val(&env);
        assert_eq!(state, back);
    }
}
