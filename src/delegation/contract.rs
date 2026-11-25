use soroban_sdk::{contractimpl, contracttype, Address, BytesN, Env, Symbol, Vec};
use soroban_sdk::token::{Token, TokenType};
use soroban_sdk::types::ContractData;

#[contracttype]
pub enum AccountCircuitBreakerStatus {
    Normal,
    Frozen,
}

#[contracttype]
pub enum DelegationStatus {
    Active,
    Expired,
}

#[contracttype]
pub enum DelegationError {
    NotFound,
    NotExpired,
    RewardUnavailable,
    BatchTooLarge,
    Unauthorized,
}

const MAX_SWEEP_BATCH: u32 = 50;
const KEEPER_REWARD_STROOPS: i128 = 100;

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
        Ok(())
    }

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

    /// Sweeps up to `MAX_SWEEP_BATCH` expired delegation records, removing them
    /// from storage and paying the keeper a reward from the gas rebate pool.
    /// Active (non-expired) delegations are rejected and cause the whole call to fail.
    pub fn sweep_expired_delegations(
        env: Env,
        keeper: Address,
        delegation_keys: Vec<BytesN<32>>,
    ) -> Result<u32, DelegationError> {
        keeper.require_auth();

        let count = delegation_keys.len();
        if count == 0 {
            return Ok(0);
        }
        if count > MAX_SWEEP_BATCH {
            return Err(DelegationError::BatchTooLarge);
        }

        let now = env.ledger().timestamp();

        // Validate every key first so the sweep is atomic: no partial removal.
        for i in 0..count {
            let key = delegation_keys.get(i).unwrap();
            let record: DelegationRecord = env
                .storage()
                .persistent()
                .get(&key)
                .ok_or(DelegationError::NotFound)?;
            let expired = matches!(record.status, DelegationStatus::Expired)
                || record.expires_at < now;
            if !expired {
                return Err(DelegationError::NotExpired);
            }
        }

        // Pay the keeper reward from the gas rebate pool before removal.
        let pool: i128 = env
            .storage()
            .instance()
            .get(&GAS_REBATE_POOL_KEY)
            .unwrap_or(0);
        let reward = KEEPER_REWARD_STROOPS
            .checked_mul(count as i128)
            .ok_or(DelegationError::RewardUnavailable)?;
        if pool < reward {
            return Err(DelegationError::RewardUnavailable);
        }
        env.storage()
            .instance()
            .set(&GAS_REBATE_POOL_KEY, &(pool - reward));
        let keeper_balance: i128 = env
            .storage()
            .instance()
            .get(&(KEEPER_BALANCE_PREFIX, keeper.clone()))
            .unwrap_or(0);
        env.storage().instance().set(
            &(KEEPER_BALANCE_PREFIX, keeper.clone()),
            &(keeper_balance + reward),
        );

        // Remove the validated records.
        for i in 0..count {
            let key = delegation_keys.get(i).unwrap();
            env.storage().persistent().remove(&key);
        }

        env.events().publish(
            (Symbol::new(&env, "DelegationsSwept"), keeper),
            count,
        );

        Ok(count)
    }

    pub fn get_delegation_state(env: &Env, delegation_id: Symbol) -> Result<DelegationState, PermissionError> {
        env.storage().instance().get(&delegation_id).unwrap()
    }

    pub fn set_delegation_state(env: &Env, delegation_id: Symbol, state: &DelegationState) {
        env.storage().instance().set(&delegation_id, state);
    }
}
