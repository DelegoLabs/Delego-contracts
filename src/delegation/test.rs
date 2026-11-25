//! Unit tests for spend validation (issue #704).

use std::panic::{catch_unwind, AssertUnwindSafe};

use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, BytesN, Env, Symbol};

use super::authorizations::SpendAuthorization;
use super::contract::{DelegationContract, DelegationContractClient};
use super::errors::PermissionError;
use super::state::DelegationState;

fn new_state(env: &Env) -> (DelegationState, Address) {
    let token = Address::generate(env);
    let mut state = DelegationState::new(env, Address::generate(env), Address::generate(env));
    state.add_allowance(env, token.clone(), 1_000);
    (state, token)
}

fn authorization(env: &Env, agent: &Address, asset: &Address) -> SpendAuthorization {
    SpendAuthorization {
        agent: agent.clone(),
        agent_pubkey: BytesN::from_array(env, &[7u8; 32]),
        asset: asset.clone(),
        amount: 100,
        epoch: 0,
        nonce: 0,
        signature: BytesN::from_array(env, &[9u8; 64]),
    }
}

#[test]
fn record_spend_rejects_non_positive_amounts() {
    let env = Env::default();
    let (mut state, token) = new_state(&env);

    assert_eq!(
        state.record_spend(&token, 0),
        Err(PermissionError::InvalidAmount)
    );
    assert_eq!(
        state.record_spend(&token, -1),
        Err(PermissionError::InvalidAmount)
    );

    // Nothing was recorded for the rejected spends.
    let idx = state.find_allowance_idx(&token).unwrap();
    assert_eq!(state.allowances.get(idx).unwrap().spent_stroops, 0);
}

#[test]
fn record_spend_rejects_overflow_without_panicking() {
    let env = Env::default();
    let token = Address::generate(&env);
    let mut state = DelegationState::new(&env, Address::generate(&env), Address::generate(&env));
    state.add_allowance(&env, token.clone(), i128::MAX);

    assert_eq!(state.record_spend(&token, i128::MAX), Ok(()));
    // i128::MAX + 1 overflows; it must surface as a typed error, not a panic.
    assert_eq!(
        state.record_spend(&token, 1),
        Err(PermissionError::ExceedsAllowance)
    );
}

#[test]
fn execute_spend_rejects_replayed_nonce() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(DelegationContract, ());
    let client = DelegationContractClient::new(&env, &contract_id);

    let delegation_id = Symbol::new(&env, "deleg");
    let owner = Address::generate(&env);
    let agent = Address::generate(&env);
    let asset = Address::generate(&env);
    client.create_delegation(&delegation_id, &owner, &agent);
    client.grant_asset_allowance(&delegation_id, &asset, &1_000);

    // The delegation expects nonce 0; this authorization reuses a future nonce.
    let mut auth = authorization(&env, &agent, &asset);
    auth.nonce = 1;

    let result = client.try_execute_spend(&delegation_id, &auth);
    assert_eq!(result, Err(Ok(PermissionError::InvalidNonce)));

    // A rejected spend never consumes the nonce, so replaying it fails again.
    assert_eq!(client.get_delegation_state(&delegation_id).nonce, 0);
    let replayed = client.try_execute_spend(&delegation_id, &auth);
    assert_eq!(replayed, Err(Ok(PermissionError::InvalidNonce)));
}

#[test]
fn execute_spend_rejects_non_positive_amount() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(DelegationContract, ());
    let client = DelegationContractClient::new(&env, &contract_id);

    let delegation_id = Symbol::new(&env, "deleg");
    let owner = Address::generate(&env);
    let agent = Address::generate(&env);
    let asset = Address::generate(&env);
    client.create_delegation(&delegation_id, &owner, &agent);
    client.grant_asset_allowance(&delegation_id, &asset, &1_000);

    let mut auth = authorization(&env, &agent, &asset);
    auth.amount = 0;
    assert_eq!(
        client.try_execute_spend(&delegation_id, &auth),
        Err(Ok(PermissionError::InvalidAmount))
    );

    auth.amount = -100;
    assert_eq!(
        client.try_execute_spend(&delegation_id, &auth),
        Err(Ok(PermissionError::InvalidAmount))
    );

    assert_eq!(client.get_delegation_state(&delegation_id).nonce, 0);
}

#[test]
fn execute_spend_rejects_forged_signature() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(DelegationContract, ());
    let client = DelegationContractClient::new(&env, &contract_id);

    let delegation_id = Symbol::new(&env, "deleg");
    let owner = Address::generate(&env);
    let agent = Address::generate(&env);
    let asset = Address::generate(&env);
    client.create_delegation(&delegation_id, &owner, &agent);
    client.grant_asset_allowance(&delegation_id, &asset, &1_000);

    let auth = authorization(&env, &agent, &asset);

    // `ed25519_verify` aborts the invocation on an invalid signature.
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let _ = client.execute_spend(&delegation_id, &auth);
    }));
    assert!(outcome.is_err(), "forged signature must be rejected");

    // No spend and no nonce consumption happened.
    let state = client.get_delegation_state(&delegation_id);
    assert_eq!(state.nonce, 0);
    let idx = state.find_allowance_idx(&asset).unwrap();
    assert_eq!(state.allowances.get(idx).unwrap().spent_stroops, 0);
}
