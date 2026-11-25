//! Delegation contract: agent spending bounded by per-asset allowances.
//!
//! Every spend must arrive with a [`SpendAuthorization`] that is bound to the
//! delegation (agent key, epoch, nonce) and signed by the agent's Ed25519 key.

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, Symbol};

use super::authorizations::SpendAuthorization;
use super::errors::PermissionError;
use super::state::{AccountCircuitBreakerStatus, DelegationState};

/// Auxiliary storage keys (instance storage).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// Delegation id owned by a delegator account.
    DelegationByOwner(Address),
    /// Circuit breaker status of a delegator account.
    Status(Address),
}

#[contract]
pub struct DelegationContract;

#[contractimpl]
impl DelegationContract {
    /// Creates a delegation from `owner` to `agent`.
    pub fn create_delegation(
        env: Env,
        delegation_id: Symbol,
        owner: Address,
        agent: Address,
    ) -> Result<(), PermissionError> {
        owner.require_auth();
        if env.storage().instance().has(&delegation_id) {
            return Err(PermissionError::PermissionDenied);
        }
        let state = DelegationState::new(&env, owner.clone(), agent);
        Self::set_delegation_state(&env, delegation_id.clone(), &state);
        env.storage()
            .instance()
            .set(&DataKey::DelegationByOwner(owner.clone()), &delegation_id);
        Self::set_circuit_breaker_status(&env, owner, AccountCircuitBreakerStatus::Normal);
        Ok(())
    }

    pub fn pause_delegation(
        env: Env,
        delegation_id: Symbol,
    ) -> Result<(), PermissionError> {
        let mut state = Self::get_delegation_state(&env, delegation_id.clone())?;
        state.increment_epoch(&env);
        state.is_paused = true;
        Self::set_delegation_state(&env, delegation_id, &state);
        Ok(())
    }

    pub fn resume_delegation(
        env: Env,
        delegation_id: Symbol,
    ) -> Result<(), PermissionError> {
        let mut state = Self::get_delegation_state(&env, delegation_id.clone())?;
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
        let mut state = Self::get_delegation_state(&env, delegation_id.clone())?;
        state.add_allowance(&env, token_contract, spending_limit_stroops);
        Self::set_delegation_state(&env, delegation_id, &state);
        Ok(())
    }

    /// Freezes an account: advances the delegation epoch (invalidating every
    /// in-flight authorization) and pauses spending until reactivated.
    pub fn trigger_emergency_killswitch(
        env: Env,
        delegator: Address,
    ) -> Result<(), PermissionError> {
        delegator.require_auth();
        let delegation_id: Symbol = env
            .storage()
            .instance()
            .get(&DataKey::DelegationByOwner(delegator.clone()))
            .ok_or(PermissionError::PermissionDenied)?;
        let mut state = Self::get_delegation_state(&env, delegation_id.clone())?;
        state.increment_epoch(&env);
        state.is_paused = true;
        Self::set_delegation_state(&env, delegation_id, &state);
        Self::set_circuit_breaker_status(
            &env,
            delegator.clone(),
            AccountCircuitBreakerStatus::Frozen,
        );
        env.events().publish(
            (
                Symbol::new(&env, "CircuitBreakerTriggered"),
                delegator,
            ),
            state.epoch_config.current_epoch,
        );
        Ok(())
    }

    /// Lifts the circuit breaker; requires the delegator's master key.
    pub fn reactivate_account(
        env: Env,
        delegator: Address,
    ) -> Result<(), PermissionError> {
        delegator.require_auth();
        let delegation_id: Symbol = env
            .storage()
            .instance()
            .get(&DataKey::DelegationByOwner(delegator.clone()))
            .ok_or(PermissionError::PermissionDenied)?;
        let mut state = Self::get_delegation_state(&env, delegation_id.clone())?;
        state.increment_epoch(&env);
        state.is_paused = false;
        Self::set_delegation_state(&env, delegation_id, &state);
        Self::set_circuit_breaker_status(
            &env,
            delegator,
            AccountCircuitBreakerStatus::Normal,
        );
        Ok(())
    }

    /// Executes a delegated spend.
    ///
    /// Enforced, in order:
    /// 1. The account is not frozen by the circuit breaker.
    /// 2. The delegation is not paused.
    /// 3. `auth.agent` is the delegation's agent.
    /// 4. `auth.epoch` matches the delegation's current epoch.
    /// 5. `auth.nonce` equals the delegation's next expected nonce (replay
    ///    protection; the stored nonce is incremented on success).
    /// 6. `auth.amount` is strictly positive.
    /// 7. `auth.signature` is a valid Ed25519 signature by `auth.agent_pubkey`
    ///    over the domain-separated XDR payload for this delegation.
    /// 8. The spend fits within the asset's remaining allowance (checked
    ///    arithmetic; overflow is reported as
    ///    [`PermissionError::ExceedsAllowance`]).
    ///
    /// A forged signature aborts the invocation (`ed25519_verify` traps), so
    /// the spend is rejected and no state is written.
    pub fn execute_spend(
        env: Env,
        delegation_id: Symbol,
        auth: SpendAuthorization,
    ) -> Result<(), PermissionError> {
        let mut state = Self::get_delegation_state(&env, delegation_id.clone())?;

        if Self::get_circuit_breaker_status(&env, state.owner.clone())
            == AccountCircuitBreakerStatus::Frozen
        {
            return Err(PermissionError::AccountFrozen);
        }
        if state.is_paused {
            return Err(PermissionError::DelegationPaused);
        }

        // The authorization must have been issued for this delegation's agent.
        if auth.agent != state.agent {
            return Err(PermissionError::PermissionDenied);
        }

        auth.validate_epoch(state.epoch_config.current_epoch as u64)?;

        // Strictly sequential nonce: a replayed authorization carries a nonce
        // the delegation has already consumed, so it is rejected here before
        // any state is mutated.
        if auth.nonce != state.nonce {
            return Err(PermissionError::InvalidNonce);
        }

        if auth.amount <= 0 {
            return Err(PermissionError::InvalidAmount);
        }

        // Verified last so cheap checks fail first. The host traps when the
        // signature is forged, rejecting the spend.
        let payload = auth.signing_payload(&env, delegation_id.clone());
        env.crypto()
            .ed25519_verify(&auth.agent_pubkey, &payload, &auth.signature);

        state.record_spend(&auth.asset, auth.amount)?;
        state.nonce = auth
            .nonce
            .checked_add(1)
            .ok_or(PermissionError::InvalidNonce)?;
        Self::set_delegation_state(&env, delegation_id, &state);
        Ok(())
    }

    pub fn get_delegation_state(
        env: &Env,
        delegation_id: Symbol,
    ) -> Result<DelegationState, PermissionError> {
        env.storage()
            .instance()
            .get(&delegation_id)
            .ok_or(PermissionError::PermissionDenied)
    }

    pub fn set_delegation_state(
        env: &Env,
        delegation_id: Symbol,
        state: &DelegationState,
    ) {
        env.storage().instance().set(&delegation_id, state);
    }

    pub fn get_circuit_breaker_status(
        env: &Env,
        delegator: Address,
    ) -> AccountCircuitBreakerStatus {
        env.storage()
            .instance()
            .get(&DataKey::Status(delegator))
            .unwrap_or(AccountCircuitBreakerStatus::Normal)
    }

    pub fn set_circuit_breaker_status(
        env: &Env,
        delegator: Address,
        status: AccountCircuitBreakerStatus,
    ) {
        env.storage()
            .instance()
            .set(&DataKey::Status(delegator), &status);
    }
}
