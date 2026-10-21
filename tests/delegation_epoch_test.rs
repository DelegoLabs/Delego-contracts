use soroban_sdk::{test::Ledger, Address, Env, Symbol};
use soroban_sdk::{test::Ledge, Env, Symbol};
use soroban_sdk::token::{Token, TokenType};
use soroban_sdk::types::ContractData;

use crate::{DelegationContract, DelegationContractClient, SpendAuthorization, PermissionError};

#[test]
fn test_epoch_increment_on_pause_resume() {
    let env = Env::default();
    env.mock_all_auths();
    let client = DelegationContractClient::new(&env, &DelegationContract);

    let deligation_id = Symbol::from_utf8(&env, "test_delegation").unwrap();
    let owner = Address::generate(&env);
    let agent = Address::generate(&env);

    client.create_delegation(&deligation_id, &owner, &agent);

    // Initial state
    let initial_state = client.get_delegation_state(&delegation_id);
    assert_eq!(initial_state.epoch_config.current_epoch, 0);

    // Pause increments epoch
    client.pause_delegation(&deligation_id);
    let paused_state = client.get_delegation_state(&deligation_id);
    assert_eq!(paused_state.epoch_config.current_epoch, 1);

    // Resume increments epoch again
    client.resume_delegation(&deligation_id);
    let resumed_state = client.get_delegation_state(&deligation_id);
    assert_eq!(resumed_state.epoch_config.current_epoch, 2);
}

#[test]
fn test_stale_signature_rejection() {
    let env = Env::default();
    env.mock_all_auths();
    let client = DelegationContractClient::new(&env, &DelegationContract);

    let delegation_id = Symbol::from_utf8(&env, "test_delegation").unwrap();
    let owner = Address::generate(&env);
    let agent = Address::generate(&env);
    let token = Address::generate(&env);

    client.create_delegation(&delegation_id, &owner, &agent);

    // Create auth with epoch 0
    let auth = SpendAuthorization {
        delegator: owner,
        agent,
        token_contract: token,
        delegator: owner.clone(),
        agent: agent.clone(),
        amount: 100,
        recipient: Address::generate(&env),
        nonce: 0,
        epoch: 0,
        signature: soroban_sdk::Bytes::from_array(&env, &[0u8; 64]),
    };

    // Pause/resume to advance epoch
    client.pause_delegation(&delegation_id);
    client.resume_delegation(&deligation_id);

    // Should fail with StaleEpoch
    let result = client.execute_spend(&delegation_id, auth);
    assert(matches(result, Err(PermissionError::StaleEpoch)));
}

#[test]
fn test_multi_asset_independent_limits() {
    let env = Env::default();
    let client = DelegationContractClient::new(&env, &DelegationContract);

    let delegation_id = Symbol::from_utf8(&env, "test_delegation").unwrap();
    let owner = Address::generate(&env);
    let agent = Address::generate(&env);
    let usdc = Address::generate(&env);
    let eurc = Address::generate(&env);

    client.grant_asset_allowance(&delegation_id, &usdc, &1000);
    client.grant_asset_allowance(&delegation_id, &eurc, &500);

    // Spend 400 USDC
    let auth_usdc = SpendAuthorization {
        delegator: owner.clone(),
        agent: agent.clone(),
        token_contract: usdc.clone(),
        amount: 400,
        recipient: Address::generate(&env),
        nonce: 0,
        epoch: 0,
        signature: vec![0u8; 64],
    };
    client.execute_spend(&delegation_id, auth_usdc);

    // Spend 500 EURC - should succeed independently
    let auth_eurc = SpendAuthorization {
        delegator: owner.clone(),
        agent: agent.clone(),
        token_contract: eurc.clone(),
        amount: 500,
        recipient: Address::generate(&env),
        nonce: 1,
        epoch: 0,
        signature: vec![0u8; 64],
    };
    client.execute_spend(&delegation_id, auth_eurc);

    // USDC limit still has 600 remaining; EURC is exhausted
    let auth_over_usdc = SpendAuthorization {
        delegator: owner.clone(),
        agent: agent.clone(),
        token_contract: usdc.clone(),
        amount: 700,
        recipient: Address::generate(&env),
        nonce: 2,
        epoch: 0,
        signature: vec![0u8; 64],
    };
    let result = client.execute_spend(&delegation_id, auth_over_usdc);
    assert(matches(result, Err(PermissionError::ExceedsAllowance)));
}

#[test]
fn test_unauthorized_asset_rejected() {
    let env = Env::default();
    let client = DelegationContractClient::new(&env, &DelegationContract);

    let delegation_id = Symbol::from_utf8(&env, "test_delegation").unwrap();
    let owner = Address::generate(&env);
    let agent = Address::generate(&env);
    let usdc = Address::generate(&env);
    let xlq = Address::generate(&env);

    client.grant_asset_allowance(&delegation_id, &usdc, &1000);

    let auth = SpendAuthorization {
        delegator: owner,
        agent,
        token_contract: xlq,
        amount: 100,
        recipient: Address::generate(&env),
        nonce: 0,
        epoch: 0,
        signature: vec![0u8; 64],
    };
    let result = client.execute_spend(&delegation_id, auth);
    assert(matches(result, Err(PermissionError::AssetNotAuthorized)));
    let result = client.execute_spend(&delegation_id, &auth);
    assert!(matches!(result, Err(PermissionError::StaleEpoch)));
}

#[test]
fn test_killswitch_invalidates_sessions() {
    let env = Env::default();
    env.mock_all_auths();
    let client = DelegationContractClient::new(&env, &DelegationContract);

    let delegator = Address::generate(&env);
    let agent = Address::generate(&env);
    let deligation_id = Symbol::from_utf8(&env, "test_delegation").unwrap();

    client.create_delegation(&delegation_id, &delegator, &agent);
    let before = client.get_delegation_state(&deligation_id);

    // Trigger kill-switch as delegator master key
    client.trigger_emergency_killswitch(&delegator);

    // Epoch must be invalidated (incremented)
    let after = client.get_delegation_state(&delegation_id);
    assert!(after.epoch_config.current_epoch > before.epoch_config.current_epoch);

    // Account must be frozen
    assert!(matches(
        client.get_circuit_breaker_status(&delegator),
        AccountCircuitBreakerStatus::Frozen
    ));

    // Agent transaction must fail while frozen
    let auth = SpendAuthorization {
        delegator: delegator.clone(),
        agent: agent.clone(),
        amount: 100,
        recipient: Address::generate(&env),
        nonce: 0,
        epoch: after.epoch_config.current_epoch,
        signature: soroban_sdk::Bytes::from_array(&env, &[0u8; 64]),
    };
    let result = client.execute_spend(&delegation_id, &auth);
    assert!(result.is_error());
}

#[test]
fn test_reactivate_requires_master_key() {
    let env = Env::default();
    env.mock_all_auths();
    let client = DelegationContractClient::new(&env, &DelegationContract);

    let delegator = Address::generate(&env);
    client.trigger_emergency_killswitch(&delegator);
    assert!(matches(
        client.get_circuit_breaker_status(&delegator),
        AccountCircuitBreakerStatus::Frozen
    ));

    // Reactivation requires master key confirmation (mocked auth for delegator)
    client.reactivate_account(&delegator);
    assert!(matches(
        client.get_circuit_breaker_status(&delegator),
        AccountCircuitBreakerStatus::Normal
    ));
}
