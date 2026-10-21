use soroban_sdk::{contracttype, Address, Env, Symbol, Vec};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpendAuthorization {
    pub delegator: Address,
    pub agent: Address,
    pub token_contract: Address,
    pub amount: i128,
    pub recipient: Address,
    pub nonce: u64,
    pub epoch: u32,
    pub signature: Vec<u8>,
}

impl SpendAuthorization {
    pub fn validate_epoch(&self, current_epoch: u32) -> Result<(), PermissionError> {
        if self.epoch != current_epoch {
            return Err(PermissionError::StaleEpoch);
        }
        Ok(()
    }
}
