#![cfg(test)]

use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events, Ledger},
    Address, BytesN, Env, Symbol, TryFromVal,
};

fn setup() -> (
    Env,
    DelegationRegistryClient<'static>,
    Address,
    Address,
    BytesN<32>,
    Address,
) {
    let env = Env::default();
    let contract_id = env.register(DelegationRegistry, ());
    let client = DelegationRegistryClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let owner = Address::generate(&env);
    let agent_id = BytesN::from_array(&env, &[1; 32]);
    let permissions_contract = Address::generate(&env);

    client.initialize(&admin);

    (env, client, admin, owner, agent_id, permissions_contract)
}

#[test]
fn test_full_lifecycle() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Agent_X");

    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);
    assert_eq!(id, 1);

    let record = client.get_delegation(&id);
    assert_eq!(record.status, DelegationStatus::Active);
    assert!(client.is_authorized(&id, &agent_id));

    client.pause_delegation(&id);
    let record = client.get_delegation(&id);
    assert_eq!(record.status, DelegationStatus::Paused);
    assert!(!client.is_authorized(&id, &agent_id));

    client.resume_delegation(&id);
    let record = client.get_delegation(&id);
    assert_eq!(record.status, DelegationStatus::Active);
    assert!(client.is_authorized(&id, &agent_id));

    client.revoke_delegation(&id);
    let record = client.get_delegation(&id);
    assert_eq!(record.status, DelegationStatus::Revoked);
    assert!(!client.is_authorized(&id, &agent_id));
}

#[test]
fn test_expiry_behavior() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    env.ledger().set_sequence_number(100);
    let label = Symbol::new(&env, "Agent_Y");

    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &100);
    assert!(client.is_authorized(&id, &agent_id));

    env.ledger().set_sequence_number(200);
    assert!(!client.is_authorized(&id, &agent_id));
}

#[test]
fn test_unauthorized_access() {
    // Without mock_all_auths, create_delegation should fail with auth error
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    let label = Symbol::new(&env, "Agent_Z");

    // The Soroban test env panics on missing auth, so we test via try_ returning an error
    let result =
        client.try_create_delegation(&owner, &agent_id, &permissions_contract, &label, &100);
    assert!(result.is_err());
}

// ── #322 Typed-error tests ──────────────────────────────────────────────────

#[test]
fn test_zero_agent_id_rejected_with_typed_error() {
    let (env, client, _, owner, _, permissions_contract) = setup();
    env.mock_all_auths();

    let zero_agent_id = BytesN::from_array(&env, &[0u8; 32]);
    let label = Symbol::new(&env, "Zero_Agent");

    // The all-zero sentinel must be refused at creation — it would otherwise
    // seed an authorization record with a dead id.
    let result =
        client.try_create_delegation(&owner, &zero_agent_id, &permissions_contract, &label, &1000);
    assert_eq!(result, Err(Ok(DelegationError::InvalidAgentId)));

    // No delegation record may exist for the refused id.
    let records = client.get_delegations_by_owner(&owner);
    assert_eq!(records.len(), 0);
}

#[test]
fn test_resume_active_fails_with_typed_error() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Agent_Y");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &100);

    // Can only resume a paused delegation — should return NotPaused
    let result = client.try_resume_delegation(&id);
    assert_eq!(result, Err(Ok(DelegationError::NotPaused)));
}

#[test]
fn test_pause_non_active_fails_with_typed_error() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Agent_PA");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    client.pause_delegation(&id);
    // Delegation is already Paused — pausing again should return NotActive
    let result = client.try_pause_delegation(&id);
    assert_eq!(result, Err(Ok(DelegationError::NotActive)));
}

#[test]
fn test_not_found_returns_typed_error() {
    let (env, client, _, _, _, _) = setup();
    env.mock_all_auths();

    let result = client.try_pause_delegation(&9999u64);
    assert_eq!(result, Err(Ok(DelegationError::NotFound)));
}

#[test]
fn test_already_initialized_returns_typed_error() {
    let (env, client, admin, _, _, _) = setup();
    env.mock_all_auths();

    let result = client.try_initialize(&admin);
    assert_eq!(result, Err(Ok(DelegationError::AlreadyInitialized)));
}

#[test]
fn test_get_admin_returns_admin_address() {
    let (_, client, admin, _, _, _) = setup();

    assert_eq!(client.get_admin(), admin);
}

#[test]
#[should_panic(expected = "Admin not set")]
fn test_get_admin_panics_when_not_initialized() {
    let env = Env::default();
    let contract_id = env.register(DelegationRegistry, ());
    let client = DelegationRegistryClient::new(&env, &contract_id);

    client.get_admin();
}

#[test]
fn test_rollback_before_version_1_returns_typed_error() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "No_Rollback_V0");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    let result = client.try_rollback_delegation(&id, &0u32);
    assert_eq!(result, Err(Ok(DelegationError::InvalidVersion)));
}

#[test]
fn test_cannot_rollback_to_current_or_future_version_typed_error() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Future_Rollback");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    client.pause_delegation(&id);

    // Try to rollback to current version (v2) — should return VersionNotLower
    let result = client.try_rollback_delegation(&id, &2u32);
    assert_eq!(result, Err(Ok(DelegationError::VersionNotLower)));
}

// ── #322 Event-emission tests ───────────────────────────────────────────────

#[test]
fn test_created_event_emitted() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Evt_Create");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    let events = env.events().all();
    // Find a "created" event
    let found = events.iter().any(|(_, topics, _)| {
        let t: soroban_sdk::Vec<soroban_sdk::Val> = topics;
        t.len() >= 2
            && Symbol::try_from_val(&env, &t.get(0).unwrap()).ok() == Some(symbol_short!("deleg"))
            && Symbol::try_from_val(&env, &t.get(1).unwrap()).ok() == Some(symbol_short!("created"))
    });
    assert!(found, "DelegationCreated event not emitted; id={id}");
}

#[test]
fn test_paused_event_emitted() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Evt_Pause");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);
    client.pause_delegation(&id);

    let events = env.events().all();
    let found = events.iter().any(|(_, topics, _)| {
        let t: soroban_sdk::Vec<soroban_sdk::Val> = topics;
        t.len() >= 2
            && Symbol::try_from_val(&env, &t.get(0).unwrap()).ok() == Some(symbol_short!("deleg"))
            && Symbol::try_from_val(&env, &t.get(1).unwrap()).ok() == Some(symbol_short!("paused"))
    });
    assert!(found, "DelegationPaused event not emitted");
}

#[test]
fn test_resumed_event_emitted() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Evt_Resume");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);
    client.pause_delegation(&id);
    client.resume_delegation(&id);

    let events = env.events().all();
    let found = events.iter().any(|(_, topics, _)| {
        let t: soroban_sdk::Vec<soroban_sdk::Val> = topics;
        t.len() >= 2
            && Symbol::try_from_val(&env, &t.get(0).unwrap()).ok() == Some(symbol_short!("deleg"))
            && Symbol::try_from_val(&env, &t.get(1).unwrap()).ok() == Some(symbol_short!("resumed"))
    });
    assert!(found, "DelegationResumed event not emitted");
}

#[test]
fn test_sweep_expired_updates_expired_delegations() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    env.ledger().set_sequence_number(100);
    let label = Symbol::new(&env, "Agent_Y");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &100);

    env.ledger().set_sequence_number(300);

    let mut ids = Vec::new(&env);
    ids.push_back(id);
    let swept = client.sweep_expired(&ids);

    assert_eq!(swept.len(), 1);
    assert_eq!(swept.get(0).unwrap(), id);

    let record = client.get_delegation(&id);
    assert_eq!(record.status, DelegationStatus::Expired);
}

#[test]
fn test_sweep_expired_is_noop_for_non_expired() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    env.ledger().set_sequence_number(100);
    let label = Symbol::new(&env, "Agent_Y");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    let mut ids = Vec::new(&env);
    ids.push_back(id);
    let swept = client.sweep_expired(&ids);

    assert_eq!(swept.len(), 0);
    let record = client.get_delegation(&id);
    assert_eq!(record.status, DelegationStatus::Active);
}

#[test]
fn test_sweep_expired_skips_revoked_and_unknown_ids() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    env.ledger().set_sequence_number(100);
    let label = Symbol::new(&env, "Agent_Y");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &100);
    client.revoke_delegation(&id);

    env.ledger().set_sequence_number(300);

    let mut ids = Vec::new(&env);
    ids.push_back(id);
    ids.push_back(999u64);
    let swept = client.sweep_expired(&ids);

    assert_eq!(swept.len(), 0);
    let record = client.get_delegation(&id);
    assert_eq!(record.status, DelegationStatus::Revoked);
}

#[test]
fn test_get_expired_delegations_returns_correct_list() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    env.ledger().set_sequence_number(100);
    let expiring_label = Symbol::new(&env, "Expiring");
    let active_label = Symbol::new(&env, "Active");

    let expiring_id = client.create_delegation(
        &owner,
        &agent_id,
        &permissions_contract,
        &expiring_label,
        &100,
    );
    let active_id = client.create_delegation(
        &owner,
        &agent_id,
        &permissions_contract,
        &active_label,
        &1000,
    );

    env.ledger().set_sequence_number(300);

    let expired = client.get_expired_delegations(&owner);
    assert_eq!(expired.len(), 1);
    assert_eq!(expired.get(0).unwrap().id, expiring_id);
    assert_ne!(expired.get(0).unwrap().id, active_id);
}

#[test]
fn test_multiple_delegations_per_owner() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label1 = Symbol::new(&env, "Shopping");
    let label2 = Symbol::new(&env, "Trading");

    client.create_delegation(&owner, &agent_id, &permissions_contract, &label1, &100);
    client.create_delegation(&owner, &agent_id, &permissions_contract, &label2, &100);

    let dels = client.get_delegations_by_owner(&owner);
    assert_eq!(dels.len(), 2);
    assert_eq!(dels.get(0).unwrap().label, label1);
    assert_eq!(dels.get(1).unwrap().label, label2);
}

#[test]
fn test_version_increments_on_each_update() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Versioned_Agt");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    assert_eq!(client.get_delegation_version(&id), 1);

    client.pause_delegation(&id);
    assert_eq!(client.get_delegation_version(&id), 2);

    client.resume_delegation(&id);
    assert_eq!(client.get_delegation_version(&id), 3);

    client.revoke_delegation(&id);
    assert_eq!(client.get_delegation_version(&id), 4);
}

#[test]
fn test_rollback_restores_previous_state() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Rollback_Test");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Active);

    client.pause_delegation(&id);
    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Paused);

    client.resume_delegation(&id);
    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Active);

    client.rollback_delegation(&id, &1u32);
    let record = client.get_delegation(&id);
    assert_eq!(record.status, DelegationStatus::Active);
    assert_eq!(client.get_delegation_version(&id), 4);
}

#[test]
fn test_updated_at_advances_across_lifecycle() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    env.ledger().set_timestamp(1_000);
    let label = Symbol::new(&env, "Updated_At_Test");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    let record = client.get_delegation(&id);
    assert_eq!(record.created_at, 1_000);
    assert_eq!(record.updated_at, 1_000);

    env.ledger().set_timestamp(2_000);
    client.pause_delegation(&id);
    let record = client.get_delegation(&id);
    assert_eq!(record.updated_at, 2_000);
    assert_eq!(record.created_at, 1_000);

    env.ledger().set_timestamp(3_000);
    client.resume_delegation(&id);
    let record = client.get_delegation(&id);
    assert_eq!(record.updated_at, 3_000);

    env.ledger().set_timestamp(4_000);
    client.rollback_delegation(&id, &1u32);
    let record = client.get_delegation(&id);
    assert_eq!(record.updated_at, 4_000);

    env.ledger().set_timestamp(5_000);
    client.revoke_delegation(&id);
    let record = client.get_delegation(&id);
    assert_eq!(record.updated_at, 5_000);
    assert_eq!(record.created_at, 1_000);
}

#[test]
fn test_updated_at_advances_on_sweep() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    env.ledger().set_sequence_number(100);
    env.ledger().set_timestamp(1_000);
    let label = Symbol::new(&env, "Sweep_Updated_At");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &100);

    env.ledger().set_sequence_number(300);
    env.ledger().set_timestamp(9_000);

    let mut ids = Vec::new(&env);
    ids.push_back(id);
    client.sweep_expired(&ids);

    let record = client.get_delegation(&id);
    assert_eq!(record.status, DelegationStatus::Expired);
    assert_eq!(record.updated_at, 9_000);
    assert_eq!(record.created_at, 1_000);
}

#[test]
fn test_version_history_is_stored() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "History_Test");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    client.pause_delegation(&id);
    client.resume_delegation(&id);

    let history = client.get_delegation_history(&id);
    assert!(!history.is_empty());

    let first_snapshot = history.get(0).unwrap();
    assert_eq!(first_snapshot.version, 1);
    assert_eq!(first_snapshot.record.status, DelegationStatus::Active);
}

#[test]
fn test_revoke_delegation_idempotency_distinguishes_first_and_subsequent_calls() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Revoke_Idempotency");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    // Initial state: Active, version 1
    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Active);
    assert_eq!(client.get_delegation_version(&id), 1);

    // First revoke: actual transition -> returns true, version increments to 2
    let first_result = client.revoke_delegation(&id);
    assert!(first_result);
    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Revoked);
    assert_eq!(client.get_delegation_version(&id), 2);

    // Second revoke: already revoked (no-op) -> returns false, version remains 2
    let second_result = client.revoke_delegation(&id);
    assert!(!second_result);
    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Revoked);
    assert_eq!(client.get_delegation_version(&id), 2);
}

#[test]
fn test_revoke_paused_delegation_returns_true() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Revoke_Paused");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    client.pause_delegation(&id);
    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Paused);
    assert_eq!(client.get_delegation_version(&id), 2);

    // Revoking from Paused state should transition to Revoked and return true
    let result = client.revoke_delegation(&id);
    assert!(result);
    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Revoked);
    assert_eq!(client.get_delegation_version(&id), 3);

    // Repeat revoke returns false
    let repeat_result = client.revoke_delegation(&id);
    assert!(!repeat_result);
    assert_eq!(client.get_delegation_version(&id), 3);
}

// ── Zero-address guard tests ─────────────────────────────────────────────────

#[test]
fn test_zero_account_permissions_contract_rejected() {
    let (env, client, _, owner, agent_id, _) = setup();
    env.mock_all_auths();

    // The canonical zero account address must be refused for permissions_contract.
    let zero_permissions = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    let label = Symbol::new(&env, "Zero_Perm_Acct");

    let result =
        client.try_create_delegation(&owner, &agent_id, &zero_permissions, &label, &1000);
    assert_eq!(result, Err(Ok(DelegationError::InvalidParam)));

    // No record should have been stored.
    let records = client.get_delegations_by_owner(&owner);
    assert_eq!(records.len(), 0);
}

#[test]
fn test_zero_contract_permissions_contract_rejected() {
    let (env, client, _, owner, agent_id, _) = setup();
    env.mock_all_auths();

    // The canonical zero contract address must be refused for permissions_contract.
    let zero_permissions = Address::from_str(
        &env,
        "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
    );
    let label = Symbol::new(&env, "Zero_Perm_Cont");

    let result =
        client.try_create_delegation(&owner, &agent_id, &zero_permissions, &label, &1000);
    assert_eq!(result, Err(Ok(DelegationError::InvalidParam)));

    let records = client.get_delegations_by_owner(&owner);
    assert_eq!(records.len(), 0);
}

#[test]
fn test_zero_account_owner_rejected() {
    let (env, client, _, _, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    // The canonical zero account address must be refused for owner.
    let zero_owner = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    let label = Symbol::new(&env, "Zero_Owner_Acct");

    let result =
        client.try_create_delegation(&zero_owner, &agent_id, &permissions_contract, &label, &1000);
    assert_eq!(result, Err(Ok(DelegationError::InvalidParam)));
}

#[test]
fn test_zero_contract_owner_rejected() {
    let (env, client, _, _, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    // The canonical zero contract address must be refused for owner.
    let zero_owner = Address::from_str(
        &env,
        "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
    );
    let label = Symbol::new(&env, "Zero_Owner_Cont");

    let result =
        client.try_create_delegation(&zero_owner, &agent_id, &permissions_contract, &label, &1000);
    assert_eq!(result, Err(Ok(DelegationError::InvalidParam)));
}
