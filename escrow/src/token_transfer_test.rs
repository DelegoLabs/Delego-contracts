#![cfg(test)]

use soroban_sdk::{
    contract, contracterror, contractimpl, panic_with_error,
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};

use crate::{token_transfer::safe_token_transfer, EscrowError};

/// Mirrors the host's built-in Stellar Asset Contract error discriminants
/// (`soroban_env_host::builtin_contracts::contract_error::ContractError`,
/// pinned via `soroban-env-host = 22.1.3` in `Cargo.lock`) so tests can make
/// a mock token trap with the exact same codes a real SAC would, without
/// needing to engineer an actual insufficient-allowance condition through
/// the real built-in asset contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum FakeSacError {
    Allowance = 9,
    BalanceDeauthorized = 11,
    TrustlineMissing = 13,
}

// --- Mock tokens -----------------------------------------------------------

#[contract]
pub struct VoidToken;
#[contractimpl]
impl VoidToken {
    pub fn transfer(_e: Env, _from: Address, _to: Address, _amount: i128) {}
}

#[contract]
pub struct BoolTrueToken;
#[contractimpl]
impl BoolTrueToken {
    pub fn transfer(_e: Env, _from: Address, _to: Address, _amount: i128) -> bool {
        true
    }
}

#[contract]
pub struct BoolFalseToken;
#[contractimpl]
impl BoolFalseToken {
    pub fn transfer(_e: Env, _from: Address, _to: Address, _amount: i128) -> bool {
        false
    }
}

#[contract]
pub struct PanicToken;
#[contractimpl]
impl PanicToken {
    pub fn transfer(_e: Env, _from: Address, _to: Address, _amount: i128) {
        panic!("insufficient balance");
    }
}

#[contract]
pub struct AllowanceExceededToken;
#[contractimpl]
impl AllowanceExceededToken {
    pub fn transfer(e: Env, _from: Address, _to: Address, _amount: i128) {
        panic_with_error!(&e, FakeSacError::Allowance);
    }
}

#[contract]
pub struct FrozenTrustlineToken;
#[contractimpl]
impl FrozenTrustlineToken {
    pub fn transfer(e: Env, _from: Address, _to: Address, _amount: i128) {
        panic_with_error!(&e, FakeSacError::BalanceDeauthorized);
    }
}

#[contract]
pub struct MissingTrustlineToken;
#[contractimpl]
impl MissingTrustlineToken {
    pub fn transfer(e: Env, _from: Address, _to: Address, _amount: i128) {
        panic_with_error!(&e, FakeSacError::TrustlineMissing);
    }
}

#[contract]
pub struct WeirdReturnToken;
#[contractimpl]
impl WeirdReturnToken {
    pub fn transfer(_e: Env, _from: Address, _to: Address, _amount: i128) -> u32 {
        1
    }
}

// Minimal host contract so `current_contract_address()` works.
#[contract]
pub struct Host;
#[contractimpl]
impl Host {}

fn run<T: soroban_sdk::testutils::Register>(token: T, amount: i128) -> Result<(), EscrowError> {
    let env = Env::default();
    env.mock_all_auths();
    let host = env.register(Host, ());
    let token_id = env.register(token, ());
    let to = Address::generate(&env);
    env.as_contract(&host, || safe_token_transfer(&env, &token_id, &to, amount))
}

// --- Tests -----------------------------------------------------------------

#[test]
fn void_return_is_ok() {
    assert_eq!(run(VoidToken, 100), Ok(()));
}

#[test]
fn bool_true_is_ok() {
    assert_eq!(run(BoolTrueToken, 100), Ok(()));
}

#[test]
fn bool_false_is_rejected() {
    assert_eq!(run(BoolFalseToken, 100), Err(EscrowError::TokenTransferRejected));
}

#[test]
fn trapping_token_maps_to_failed() {
    assert_eq!(run(PanicToken, 100), Err(EscrowError::TokenTransferFailed));
}

#[test]
fn unexpected_return_type_is_rejected() {
    assert_eq!(run(WeirdReturnToken, 100), Err(EscrowError::TokenUnexpectedReturn));
}

#[test]
fn non_positive_amount_is_rejected_before_calling_token() {
    assert_eq!(run(VoidToken, 0), Err(EscrowError::InvalidAmount));
    assert_eq!(run(VoidToken, -5), Err(EscrowError::InvalidAmount));
}

#[test]
fn works_with_real_stellar_asset_contract() {
    let env = Env::default();
    env.mock_all_auths();
    let host = env.register(Host, ());
    let admin = Address::generate(&env);
    let to = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(admin);
    let token_id = sac.address();

    StellarAssetClient::new(&env, &token_id).mint(&host, &1_000);

    let res = env.as_contract(&host, || safe_token_transfer(&env, &token_id, &to, 400));
    assert_eq!(res, Ok(()));

    let client = TokenClient::new(&env, &token_id);
    assert_eq!(client.balance(&to), 400);
    assert_eq!(client.balance(&host), 600);
}

#[test]
fn sac_insufficient_balance_maps_to_specific_error() {
    let env = Env::default();
    env.mock_all_auths();
    let host = env.register(Host, ());
    let admin = Address::generate(&env);
    let to = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(admin).address();

    StellarAssetClient::new(&env, &token_id).mint(&host, &50);

    let res = env.as_contract(&host, || safe_token_transfer(&env, &token_id, &to, 400));
    assert_eq!(res, Err(EscrowError::TokenInsufficientBalance));
}

#[test]
fn sac_allowance_exceeded_maps_to_specific_error() {
    assert_eq!(
        run(AllowanceExceededToken, 100),
        Err(EscrowError::TokenAllowanceExceeded)
    );
}

#[test]
fn sac_frozen_trustline_maps_to_specific_error() {
    assert_eq!(
        run(FrozenTrustlineToken, 100),
        Err(EscrowError::TokenFrozen)
    );
}

#[test]
fn sac_missing_trustline_maps_to_specific_error() {
    assert_eq!(
        run(MissingTrustlineToken, 100),
        Err(EscrowError::TokenInvalidTokenContract)
    );
}

// --- New comprehensive tests -----------------------------------------------

#[test]
fn safe_token_transfer_from_with_valid_void_return() {
    use crate::token_transfer::safe_token_transfer_from;
    
    let env = Env::default();
    env.mock_all_auths();
    let host = env.register(Host, ());
    let token_id = env.register(VoidToken, ());
    let payer = Address::generate(&env);
    
    let res = env.as_contract(&host, || {
        safe_token_transfer_from(&env, &token_id, &payer, 250)
    });
    
    assert_eq!(res, Ok(()));
}

#[test]
fn safe_token_transfer_from_rejects_negative_amount() {
    use crate::token_transfer::safe_token_transfer_from;
    
    let env = Env::default();
    env.mock_all_auths();
    let host = env.register(Host, ());
    let token_id = env.register(VoidToken, ());
    let payer = Address::generate(&env);
    
    let res = env.as_contract(&host, || {
        safe_token_transfer_from(&env, &token_id, &payer, -100)
    });
    
    assert_eq!(res, Err(EscrowError::InvalidAmount));
}

#[test]
fn safe_token_transfer_from_with_real_sac_insufficient_balance() {
    use crate::token_transfer::safe_token_transfer_from;
    
    let env = Env::default();
    env.mock_all_auths();
    let host = env.register(Host, ());
    let admin = Address::generate(&env);
    let payer = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(admin).address();
    
    // Mint only 50 tokens to payer
    StellarAssetClient::new(&env, &token_id).mint(&payer, &50);
    
    // Try to transfer 200 tokens (more than available)
    let res = env.as_contract(&host, || {
        safe_token_transfer_from(&env, &token_id, &payer, 200)
    });
    
    assert_eq!(res, Err(EscrowError::TokenInsufficientBalance));
}

#[test]
fn multiple_sequential_transfers_maintain_balance_integrity() {
    let env = Env::default();
    env.mock_all_auths();
    let host = env.register(Host, ());
    let admin = Address::generate(&env);
    let recipient1 = Address::generate(&env);
    let recipient2 = Address::generate(&env);
    let recipient3 = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(admin);
    let token_id = sac.address();
    
    // Mint 1000 tokens to host contract
    StellarAssetClient::new(&env, &token_id).mint(&host, &1_000);
    
    // Perform multiple transfers
    env.as_contract(&host, || {
        safe_token_transfer(&env, &token_id, &recipient1, 300).unwrap();
        safe_token_transfer(&env, &token_id, &recipient2, 250).unwrap();
        safe_token_transfer(&env, &token_id, &recipient3, 100).unwrap();
    });
    
    let client = TokenClient::new(&env, &token_id);
    
    // Verify all balances
    assert_eq!(client.balance(&recipient1), 300);
    assert_eq!(client.balance(&recipient2), 250);
    assert_eq!(client.balance(&recipient3), 100);
    assert_eq!(client.balance(&host), 350); // 1000 - 300 - 250 - 100
}

#[test]
fn edge_case_transfer_exactly_all_available_balance() {
    let env = Env::default();
    env.mock_all_auths();
    let host = env.register(Host, ());
    let admin = Address::generate(&env);
    let recipient = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(admin);
    let token_id = sac.address();
    
    // Mint exact amount
    let exact_amount = 500_i128;
    StellarAssetClient::new(&env, &token_id).mint(&host, &exact_amount);
    
    // Transfer exactly all available balance
    let res = env.as_contract(&host, || {
        safe_token_transfer(&env, &token_id, &recipient, exact_amount)
    });
    
    assert_eq!(res, Ok(()));
    
    let client = TokenClient::new(&env, &token_id);
    assert_eq!(client.balance(&recipient), exact_amount);
    assert_eq!(client.balance(&host), 0);
    
    // Attempting another transfer should fail with insufficient balance
    let res2 = env.as_contract(&host, || {
        safe_token_transfer(&env, &token_id, &recipient, 1)
    });
    
    assert_eq!(res2, Err(EscrowError::TokenInsufficientBalance));
}
