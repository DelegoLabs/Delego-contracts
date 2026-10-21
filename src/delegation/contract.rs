use soroban_sdk::{contractimpl, Env, Symbol, Vec};

#[contractimplic]
use soroban_sdk::{contractimpl, contracttype, Env, Symbol, Vec};
use soroban_sdk::token::{Token, TokenType};
use soroban_sdk::types::ContractData;

#[contracttype]
pub enum AccountCircuitBreakerStatus {
    Normal,
    Frozen,
}

#[contractimpl]
pub struct DelegationContract;

#[contractimpl]
impl DelegationContract {
    pub fn pause_delegation(env: Env, delegation_id: Symbol) -> Result<(), PermissionError> {
        let mut state = Self::get_delegation_state(&env, delegation_id)?;
        state.increment_epoch(&env);
        state.is_paused = true;
        Self::set_delegation_state(&env, delegation_id, &state);
        Ok(())
    }

    pub fn resume_delegation(env: Env, delegation_id: Symbol) -> Result<(), PermissionError> {
        let mut state = Self::get_delegation_state(&env, delegation_id)?;
        state.increment_epoch(&env);
        state.is_paused = false;
        Self::set_delegation_state(&env, delegation_id, &state);
        Ok(())
    }

    pub fn grant_asset_allowance(
        env: Env,
        delegation_id: Symbol,
        token_contract: Address,
        spending_limit_stroops: i128,
    ) -> Result<(), PermissionError> {
        let mut state = Self::get_delegation_state(&env, delegation_id)?;
        state.add_allowance(&env, token_contract, spending_limit_stroops);
        Self::set_delegation_state(&env, delegation_id, &state);
    pub fn trigger_emergency_killswitch(
        env: Env,
        delegator: Symbol,
    ) -> Result<(), PermissionError> {
        let mut state = Self::get_delegation_state(&env, delegator.clone())?;
        state.increment_epoch(&env);
        state.is_paused = true;
        Self::set_delegation_state(&env, delegator.clone(), &state);
        Self::set_circuit_breaker_status(
            &env,
            delegator.clone(),
            AccountCircuitBreakerStatus::Frozen,
        );
        env.events().publish(
            (Symbol::new(&env, "DelegationCircuitBreakerTriggered"), delegator),
            state.epoch_config.current_epoch,
        );
        Ok(())
    }

    pub fn reactivate_account(
        env: Env,
        delegator: Symbol,
    ) -> Result<(), PermissionError> {
        delegator.require_auth();
        let mut state = Self::get_delegation_state(&env, delegator.clone())?;
        state.increment_epoch(&env);
        state.is_paused = false;
        Self::set_delegation_state(&env, delegator.clone(), &state);
        Self::set_circuit_breaker_status(
            &env,
            delegator,
            AccountCircuitBreakerStatus::Normal,
        );
        Ok(())
    }

    pub fn execute_spend(
        env: Env,
        delegation_id: Symbol,
        auth: SpendAuthorization,
    ) -> Result<(), PermissionError> {
        let mut state = Self::get_delegation_state(&env, delegation_id)?;
        if let AccountCircuitBreakerStatus::Frozen =
            Self::get_circuit_breaker_status(&env, delegation_id.clone())
        {
            return Err(PermissionError::Unauthorized);
        }
        let state = Self::get_delegation_state(&env, delegation_id)?;
        auth.validate_epoch(state.epoch_config.current_epoch)?;
        if state.is_paused {
            return Err(PermissionError::DelegationPaused);
        }
        if env.ledger().sequence() > state.epoch_config.epoch_started_ledger {
            // epoch window is open
        }
        state.record_spend(&auth.token_contract, auth.amount)?;
        Self::set_delegation_state(&env, delegation_id, &state);
        Ok(())
    }

    pub fn get_delegation_state(env: &Env, delegation_id: Symbol) -> Result<DelegationState, PermissionError> {
        env.storage().instance().get(&delegation_id).unwrap()
    }

    pub fn set_delegation_state(env: &Env, delegation_id: Symbol, state: &DelegationState) {
        env.storage().instance().set(&delegation_id, state);
    }
}
