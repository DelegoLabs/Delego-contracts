use soroban_sdk::{contracttype, Address, Env, Symbol, Vec};
use soroban_sdk::token::{Token, TokenType};
use soroban_sdk::types::ContractData;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
put enum AccountCircuitBreakerStatus {
    Normal,
    Frozen,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EpochConfig {
    pub current_epoch: u32,
    pub epoch_started_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetAllowance {
    pub token_contract: Address,
    pub spending_limit_stroops: i128,
    pub spent_stroops: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiAssetDelegation {
    pub agent: Address,
    pub allowances: Vec<AssetAllowance>,
    pub expiry_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationState {
    pub owner: Address,
    pub agent: Address,
    pub is_paused: bool,
    pub epoch_config: EpochConfig,
    pub allowances: Vec<AssetAllowance>,
}

impl DelegationState {
    pub fn new(env: &Env, owner: Address, agent: Address) -> Self {
        DelegationState {
            owner,
            agent,
            is_paused: false,
            epoch_config: EpochConfig {
                current_epoch: 0,
                epoch_started_ledger: env.ledger().sequence(),
            },
            allowances: Vec::new(env),
        }
    }

    pub fn increment_epoch(&mut self, env: &Env) {
        self.epoch_config.current_epoch += 1;
        self.epoch_config.epoch_started_ledger = env.ledger().sequence();
    }

    pub fn add_allowance(&mut self, env: &Env, token_contract: Address, spending_limit_stroops: i128) {
        let mut idx = self.allowances.len();
        let mut i = 0;
        while i < self.allowances.len() {
            let a = self.allowances.get(i).unwrap();
            if a.token_contract == token_contract {
                idx = i;
                break;
            }
            i += 1;
        }
        let record = AssetAllowance {
            token_contract,
            spending_limit_stroops,
            spent_stroops: 0,
        };
        if idx < self.allowances.len() {
            self.allowances.set(idx, record);
        } else {
            self.allowances.push_back(record);
        }
        let _ = env;
    }

    pub fn find_allowance_idx(&self, token_contract: &Address) -> Option<u32> {
        let mut i = 0;
        while i < self.allowances.len() {
            let a = self.allowances.get(i).unwrap();
            if &a.token_contract == token_contract {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    pub fn record_spend(&mut self, token_contract: &Address, amount: i128) -> Result<(), PermissionError> {
        let idx = match self.find_allowance_idx(token_contract) {
            Some(i) => i,
            None => return Err(PermissionError::AssetNotAuthorized),
        };
        let mut record = self.allowances.get(idx).unwrap();
        let new_spent = record.spent_stroops + amount;
        if new_spent > record.spending_limit_stroops {
            return Err(PermissionError::ExceedsAllowance);
        }
        record.spent_stroops = new_spent;
        self.allowances.set(idx, record);
        Ok(()
    }
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountCircuitBreaker {
    pub delegator: Address,
    pub status: AccountCircuitBreakerStatus,
    pub frozen_epoch: u32,
    pub frozen_at: u32,
}
