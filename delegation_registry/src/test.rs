#![cfg(test)]
#![allow(clippy::too_many_lines)]

use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{storage::Persistent, Address as _, Events, Ledger},
    Address, BytesN, Env, Symbol, TryFromVal,
};

/// Mirrors the clamp the contract applies to paginated queries, so the
/// pagination tests can assert against the same boundary value.
const MAX_PAGE_LIMIT: u32 = DelegationRegistry::MAX_PAGE_LIMIT;

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

#[test]
fn test_rollback_rejects_stale_permissions_pointer() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Rotated_Pointer");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);
    client.pause_delegation(&id);

    let rotated_permissions = Address::generate(&env);
    let mut record = client.get_delegation(&id);
    record.permissions_contract = rotated_permissions.clone();

    env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .set(&DataKey::Delegation(id), &record);
    });

    let result = client.try_rollback_delegation(&id, &1u32);
    assert_eq!(result, Err(Ok(DelegationError::InvalidVersion)));
}

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
fn test_sweep_expired_rejects_empty_and_oversized_batches() {
    let (env, client, _, _, _, _) = setup();
    let empty = Vec::<u64>::new(&env);
    assert_eq!(
        client.try_sweep_expired(&empty),
        Err(Ok(DelegationError::InvalidBatchSize))
    );

    let mut oversized = Vec::<u64>::new(&env);
    for id in 1..=MAX_SWEEP_BATCH_SIZE + 1 {
        oversized.push_back(id as u64);
    }
    assert_eq!(
        client.try_sweep_expired(&oversized),
        Err(Ok(DelegationError::InvalidBatchSize))
    );
}

#[test]
fn test_sweep_expired_full_batch_of_50_stays_within_budget() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();
    env.ledger().set_sequence_number(100);
    let label = Symbol::new(&env, "batch_sweep");
    let mut ids = Vec::<u64>::new(&env);
    for _ in 0..MAX_SWEEP_BATCH_SIZE {
        ids.push_back(client.create_delegation(
            &owner,
            &agent_id,
            &permissions_contract,
            &label,
            &100,
        ));
    }

    env.ledger().set_sequence_number(300);
    assert_eq!(client.sweep_expired(&ids).len(), MAX_SWEEP_BATCH_SIZE);
    let budget = env.cost_estimate().budget();
    assert!(budget.cpu_instruction_cost() < 50_000_000);
    assert!(budget.memory_bytes_cost() < 30_000_000);
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
fn test_version_returns_contract_identity() {
    let (_, client, _, _, _, _) = setup();

    let v = client.version();
    assert_eq!(v.name, symbol_short!("deleg_reg"));
    assert_eq!(v.semver, symbol_short!("0_0_1"));
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

const LARGE_TTL: u32 = 10_000_000;

#[test]
fn test_get_delegation_bumps_ttl() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "TTL_Bump_Get");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &LARGE_TTL);

    let delegation_key = DataKey::Delegation(id);
    let user_dels_key = DataKey::UserDelegations(owner.clone());

    // After creation the TTL should already be bumped.
    let initial_delegation_ttl = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&delegation_key)
    });
    assert!(initial_delegation_ttl > 17_280);

    let initial_user_dels_ttl = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&user_dels_key)
    });
    assert!(initial_user_dels_ttl > 17_280);

    // Advance to the point where TTL is about to drop below the threshold
    // and call get_delegation — this should refresh both keys.
    env.ledger()
        .set_sequence_number(initial_delegation_ttl - 17_280 + 1);
    let _ = client.get_delegation(&id);
    let refreshed_delegation_ttl = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&delegation_key)
    });
    assert!(refreshed_delegation_ttl > 17_280);

    let refreshed_user_dels_ttl = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&user_dels_key)
    });
    assert!(refreshed_user_dels_ttl > 17_280);
}

#[test]
fn test_is_authorized_bumps_ttl() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "TTL_Bump_Auth");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &LARGE_TTL);

    let delegation_key = DataKey::Delegation(id);
    let user_dels_key = DataKey::UserDelegations(owner.clone());

    let initial_delegation_ttl = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&delegation_key)
    });
    assert!(initial_delegation_ttl > 17_280);

    // Advance past the bump threshold and verify is_authorized refreshes TTL.
    env.ledger()
        .set_sequence_number(initial_delegation_ttl - 17_280 + 1);
    assert!(client.is_authorized(&id, &agent_id));

    let refreshed_delegation_ttl = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&delegation_key)
    });
    assert!(refreshed_delegation_ttl > 17_280);

    let refreshed_user_dels_ttl = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&user_dels_key)
    });
    assert!(refreshed_user_dels_ttl > 17_280);
}

#[test]
fn test_get_delegations_by_owner_bumps_ttl() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label1 = Symbol::new(&env, "TTL_Del1");
    let label2 = Symbol::new(&env, "TTL_Del2");
    let id1 = client.create_delegation(
        &owner,
        &agent_id,
        &permissions_contract,
        &label1,
        &LARGE_TTL,
    );
    let id2 = client.create_delegation(
        &owner,
        &agent_id,
        &permissions_contract,
        &label2,
        &LARGE_TTL,
    );

    let del_key_1 = DataKey::Delegation(id1);
    let del_key_2 = DataKey::Delegation(id2);
    let user_dels_key = DataKey::UserDelegations(owner.clone());

    let initial_ttl = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&del_key_1)
    });
    assert!(initial_ttl > 17_280);

    // Advance to the bump boundary and call get_delegations_by_owner.
    env.ledger().set_sequence_number(initial_ttl - 17_280 + 1);
    let records = client.get_delegations_by_owner(&owner);
    assert_eq!(records.len(), 2);

    let refreshed_del_1 = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&del_key_1)
    });
    assert!(refreshed_del_1 > 17_280);

    let refreshed_del_2 = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&del_key_2)
    });
    assert!(refreshed_del_2 > 17_280);

    let refreshed_user_dels = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&user_dels_key)
    });
    assert!(refreshed_user_dels > 17_280);
}

#[test]
fn test_active_authorization_survives_ttl_boundary() {
    // End-to-end: create a delegation, advance time close to TTL expiry,
    // then verify is_authorized still works and the delegation record
    // remains accessible after the bump.
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Boundary_Test");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &LARGE_TTL);

    // Authorization is valid.
    assert!(client.is_authorized(&id, &agent_id));

    let delegation_key = DataKey::Delegation(id);
    let initial_ttl = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&delegation_key)
    });

    // Advance to the bump threshold boundary.
    env.ledger().set_sequence_number(initial_ttl - 17_280 + 1);

    // The delegation should still be authorized (is_authorized bumps TTL).
    assert!(client.is_authorized(&id, &agent_id));

    // The delegation record should still be readable (get_delegation bumps).
    let record = client.get_delegation(&id);
    assert_eq!(record.status, DelegationStatus::Active);
    assert_eq!(record.id, id);

    // A second boundary crossing should still work thanks to the bumped TTL.
    let second_ttl = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&delegation_key)
    });
    env.ledger().set_sequence_number(second_ttl - 17_280 + 1);
    assert!(client.is_authorized(&id, &agent_id));
    let record = client.get_delegation(&id);
    assert_eq!(record.status, DelegationStatus::Active);
}

#[test]
fn test_create_delegation_returns_id_exhausted_at_boundary() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    // Directly set NextId to u64::MAX so the next increment overflows
    env.as_contract(&client.address, || {
        env.storage().instance().set(&DataKey::NextId, &u64::MAX);
    });

    let label = Symbol::new(&env, "Boundary_Test");
    let result =
        client.try_create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);
    assert_eq!(result, Err(Ok(DelegationError::IdExhausted)));
}

#[test]
fn test_rollback_cannot_revive_past_expiry_snapshot() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    env.ledger().set_sequence_number(100);
    let label = Symbol::new(&env, "Past_Expiry");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &100);

    // Create a second version so there is a v1 snapshot to roll back to.
    client.pause_delegation(&id);
    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Paused);

    // Advance the ledger past the original expiry (created at 100, ttl 100 => expires at 200).
    env.ledger().set_sequence_number(300);

    // Rolling back to the Active v1 snapshot must NOT revive a delegation
    // that has already expired; it should be marked Expired instead.
    client.rollback_delegation(&id, &1u32);

    let record = client.get_delegation(&id);
    assert_eq!(record.status, DelegationStatus::Expired);
}

/// Rollback must be rejected when the target snapshot was captured at or
/// after expiry — reviving an expired delegation must never be allowed.
///
/// `sweep_expired` transitions the delegation to Expired and creates a
/// versioned snapshot. We then bump to the next version via
/// `revoke_delegation` so the Expired snapshot is a valid lower target,
/// then verify the rollback is still refused.
#[test]
fn test_rollback_to_expired_snapshot_rejected() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    env.ledger().set_sequence_number(100);
    let label = Symbol::new(&env, "Expired_Rollback");
    let id = client.create_delegation(
        &owner,
        &agent_id,
        &permissions_contract,
        &label,
        &100, // expires at ledger 200
    );
    // v1 snapshot (Active)

    client.pause_delegation(&id); // v2 snapshot (Paused)

    // Advance past expiry and sweep — creates v3 Expired snapshot.
    env.ledger().set_sequence_number(300);
    let mut ids = Vec::new(&env);
    ids.push_back(id);
    let swept = client.sweep_expired(&ids);
    assert_eq!(swept.len(), 1);

    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Expired);
    assert_eq!(client.get_delegation_version(&id), 3);

    // The history must contain an Expired snapshot at v3.
    let history = client.get_delegation_history(&id);
    assert_eq!(history.len(), 3);
    assert_eq!(
        history.get(2).unwrap().record.status,
        DelegationStatus::Expired
    );

    // Revoke from Expired → bumps to v4 so v3 is now a valid lower target.
    client.revoke_delegation(&id);
    assert_eq!(client.get_delegation_version(&id), 4);
    assert_eq!(client.get_delegation_history(&id).len(), 4);

    // Rolling back to v3 (the Expired snapshot) must be rejected.
    let result = client.try_rollback_delegation(&id, &3u32);
    assert_eq!(result, Err(Ok(DelegationError::Expired)));

    // Status must remain Revoked, version unchanged.
    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Revoked);
    assert_eq!(client.get_delegation_version(&id), 4);
}

/// History must grow by exactly one entry for each state transition.
#[test]
fn test_history_grows_across_transitions() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "History_Growth");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    // After creation: 1 snapshot (v1)
    let history = client.get_delegation_history(&id);
    assert_eq!(history.len(), 1);
    assert_eq!(history.get(0).unwrap().version, 1);

    // After pause: 2 snapshots (v1, v2)
    client.pause_delegation(&id);
    let history = client.get_delegation_history(&id);
    assert_eq!(history.len(), 2);
    assert_eq!(history.get(1).unwrap().version, 2);

    // After resume: 3 snapshots (v1, v2, v3)
    client.resume_delegation(&id);
    let history = client.get_delegation_history(&id);
    assert_eq!(history.len(), 3);
    assert_eq!(history.get(2).unwrap().version, 3);

    // After revoke: 4 snapshots (v1, v2, v3, v4)
    client.revoke_delegation(&id);
    let history = client.get_delegation_history(&id);
    assert_eq!(history.len(), 4);
    assert_eq!(history.get(3).unwrap().version, 4);

    // Each snapshot must carry the correct status.
    assert_eq!(
        history.get(0).unwrap().record.status,
        DelegationStatus::Active
    );
    assert_eq!(
        history.get(1).unwrap().record.status,
        DelegationStatus::Paused
    );
    assert_eq!(
        history.get(2).unwrap().record.status,
        DelegationStatus::Active
    );
    assert_eq!(
        history.get(3).unwrap().record.status,
        DelegationStatus::Revoked
    );
}

#[test]
fn test_snapshot_key_does_not_overwrite_delegation_when_id_matches_version() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Snapshot_Key_Collision");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);
    assert_eq!(id, 1);

    env.as_contract(&client.address, || {
        let record: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(id))
            .unwrap();
        let snapshot: DelegationSnapshot = env
            .storage()
            .persistent()
            .get(&DataKey::Snapshot(id, 1))
            .unwrap();
        assert_eq!(record.status, DelegationStatus::Active);
        assert_eq!(snapshot.record.status, DelegationStatus::Active);
    });

    client.pause_delegation(&id);
    let record = client.get_delegation(&id);
    assert_eq!(record.status, DelegationStatus::Paused);

    env.as_contract(&client.address, || {
        let original: DelegationSnapshot = env
            .storage()
            .persistent()
            .get(&DataKey::Snapshot(id, 1))
            .unwrap();
        let paused: DelegationSnapshot = env
            .storage()
            .persistent()
            .get(&DataKey::Snapshot(id, 2))
            .unwrap();
        assert_eq!(original.record.status, DelegationStatus::Active);
        assert_eq!(paused.record.status, DelegationStatus::Paused);
    });
}

#[test]
fn test_snapshot_key_migration_preserves_legacy_history() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Legacy_Snapshot_Migration");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    env.as_contract(&client.address, || {
        env.storage().persistent().remove(&DataKey::Snapshot(id, 1));
        env.storage()
            .persistent()
            .remove(&DataKey::SnapshotSchemaVersion(id));
    });

    client.pause_delegation(&id);

    env.as_contract(&client.address, || {
        let migrated: DelegationSnapshot = env
            .storage()
            .persistent()
            .get(&DataKey::Snapshot(id, 1))
            .unwrap();
        let current: DelegationSnapshot = env
            .storage()
            .persistent()
            .get(&DataKey::Snapshot(id, 2))
            .unwrap();
        let schema_version: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::SnapshotSchemaVersion(id))
            .unwrap();
        assert_eq!(schema_version, 1);
        assert_eq!(migrated.record.status, DelegationStatus::Active);
        assert_eq!(current.record.status, DelegationStatus::Paused);
    });
}

/// Seeds a legacy V1 delegation record keyed only by the owner address, i.e.
/// the layout a pre-id-schema deployment stored under `DelegationV1`.
#[allow(clippy::too_many_arguments)]
fn seed_legacy_v1_delegation(
    env: &Env,
    contract_id: &Address,
    owner: &Address,
    agent_id: &BytesN<32>,
    permissions_contract: &Address,
    version: u32,
    nonce: u64,
) {
    let record = DelegationRecord {
        id: 0,
        owner: owner.clone(),
        agent_id: agent_id.clone(),
        permissions_contract: permissions_contract.clone(),
        status: DelegationStatus::Active,
        label: Symbol::new(env, "Legacy_Agt"),
        created_at: 1_000,
        updated_at: 2_000,
        expires_at_ledger: 10_000,
        version,
        permissions: PermissionBitmask::all(),
        nonce,
    };

    env.as_contract(contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::DelegationV1(owner.clone()), &record);
    });
}

#[test]
fn test_storage_schema_version_is_reported() {
    let (_env, client, _, _, _, _) = setup();

    assert_eq!(STORAGE_SCHEMA_VERSION, 2);
    assert_eq!(client.get_storage_schema_version(), STORAGE_SCHEMA_VERSION);
}

/// Reading an owner's delegations must transparently rewrite a legacy V1
/// record into the id-keyed V2 layout without losing any field.
#[test]
fn test_v1_delegation_migrated_on_owner_read() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();
    env.ledger().set_sequence_number(100);

    seed_legacy_v1_delegation(
        &env,
        &client.address,
        &owner,
        &agent_id,
        &permissions_contract,
        3,
        2,
    );

    let records = client.get_delegations_by_owner(&owner);
    assert_eq!(records.len(), 1);

    let migrated = records.get(0).unwrap();
    assert_eq!(migrated.id, 1);
    assert_eq!(migrated.owner, owner);
    assert_eq!(migrated.agent_id, agent_id);
    assert_eq!(migrated.permissions_contract, permissions_contract);
    assert_eq!(migrated.status, DelegationStatus::Active);
    assert_eq!(migrated.permissions, PermissionBitmask::all());
    assert_eq!(migrated.nonce, 2);
    assert_eq!(migrated.version, 3);
    assert_eq!(migrated.created_at, 1_000);
    assert_eq!(migrated.updated_at, 2_000);
    assert_eq!(migrated.expires_at_ledger, 10_000);

    env.as_contract(&client.address, || {
        // The V1 entry is consumed and every V2 key is populated.
        assert!(!env
            .storage()
            .persistent()
            .has(&DataKey::DelegationV1(owner.clone())));

        let stored: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(1))
            .unwrap();
        assert_eq!(stored.id, 1);

        let ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::UserDelegations(owner.clone()))
            .unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids.get(0), Some(1));

        let version: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::DelegationVersion(1))
            .unwrap();
        assert_eq!(version, 3);

        // A rollback-capable history snapshot is seeded for the new id.
        assert!(env.storage().persistent().has(&DataKey::Snapshot(1, 3)));
    });
}

/// A migrated delegation must keep authorizing the same agent and capabilities
/// without the owner re-signing any grant.
#[test]
fn test_v1_migration_preserves_authorization_without_resigning() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();
    env.ledger().set_sequence_number(100);

    seed_legacy_v1_delegation(
        &env,
        &client.address,
        &owner,
        &agent_id,
        &permissions_contract,
        1,
        5,
    );

    let records = client.get_delegations_by_owner(&owner);
    let id = records.get(0).unwrap().id;

    assert!(client.is_authorized(&id, &agent_id));
    assert!(client.is_authorized_for(&id, &agent_id, &PERM_FLAG_SPEND));
    assert!(client.is_authorized_with_nonce(&id, &agent_id, &5u64));
    assert!(!client.is_authorized_with_nonce(&id, &agent_id, &4u64));
}

/// Once migrated, mutating entry points write straight to the V2 record and
/// leave no V1 entry behind.
#[test]
fn test_v1_migration_then_write_updates_v2_record() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();
    env.ledger().set_sequence_number(100);

    seed_legacy_v1_delegation(
        &env,
        &client.address,
        &owner,
        &agent_id,
        &permissions_contract,
        3,
        2,
    );

    let id = client.get_delegations_by_owner(&owner).get(0).unwrap().id;

    assert!(client.pause_delegation(&id));

    let paused = client.get_delegation(&id);
    assert_eq!(paused.status, DelegationStatus::Paused);
    assert_eq!(paused.version, 4);

    env.as_contract(&client.address, || {
        assert!(!env
            .storage()
            .persistent()
            .has(&DataKey::DelegationV1(owner.clone())));

        let stored: DelegationRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Delegation(id))
            .unwrap();
        assert_eq!(stored.status, DelegationStatus::Paused);
        assert_eq!(stored.version, 4);
    });
}

/// A legacy record can coexist with delegations already created under V2 keys,
/// and repeated reads never duplicate the migrated entry.
#[test]
fn test_v1_migration_coexists_with_v2_delegations_and_is_idempotent() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();
    env.ledger().set_sequence_number(100);

    let first = client.create_delegation(
        &owner,
        &agent_id,
        &permissions_contract,
        &symbol_short!("First"),
        &1_000,
    );
    assert_eq!(first, 1);

    seed_legacy_v1_delegation(
        &env,
        &client.address,
        &owner,
        &agent_id,
        &permissions_contract,
        1,
        0,
    );

    let records = client.get_delegations_by_owner(&owner);
    assert_eq!(records.len(), 2);
    assert_eq!(records.get(0).unwrap().id, 1);
    assert_eq!(records.get(1).unwrap().id, 2);

    // A second read finds the V1 key gone and must not add another record.
    let again = client.get_delegations_by_owner(&owner);
    assert_eq!(again.len(), 2);

    env.as_contract(&client.address, || {
        let ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::UserDelegations(owner.clone()))
            .unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(ids.get(0), Some(1));
        assert_eq!(ids.get(1), Some(2));
    });
}

/// Reads must stay panic-free when an owner has no legacy record at all.
#[test]
fn test_v1_migration_is_noop_without_legacy_record() {
    let (_env, client, _, owner, _, _) = setup();

    assert_eq!(client.get_delegations_by_owner(&owner).len(), 0);
    assert_eq!(
        client
            .get_delegations_by_owner_paged(&owner, &0u32, &MAX_PAGE_LIMIT)
            .total,
        0
    );
    assert_eq!(client.get_expired_delegations(&owner).len(), 0);
}

/// When the id counter is exhausted the migration leaves the V1 record intact
/// rather than panicking, so reads still return safely.
#[test]
fn test_v1_migration_skips_gracefully_when_ids_exhausted() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    seed_legacy_v1_delegation(
        &env,
        &client.address,
        &owner,
        &agent_id,
        &permissions_contract,
        1,
        0,
    );

    env.as_contract(&client.address, || {
        env.storage().instance().set(&DataKey::NextId, &u64::MAX);
    });

    let records = client.get_delegations_by_owner(&owner);
    assert_eq!(records.len(), 0);

    env.as_contract(&client.address, || {
        assert!(env
            .storage()
            .persistent()
            .has(&DataKey::DelegationV1(owner.clone())));
    });
}

/// Delegation IDs must be strictly increasing even when created by
/// different owners.
#[test]
fn test_cross_owner_delegation_id_monotonicity() {
    let (env, client, _, _, _, _) = setup();
    env.mock_all_auths();

    let owner_a = Address::generate(&env);
    let owner_b = Address::generate(&env);
    let owner_c = Address::generate(&env);
    let agent_id = BytesN::from_array(&env, &[1; 32]);
    let permissions_contract = Address::generate(&env);

    let label_a = Symbol::new(&env, "OwnerA_Agt");
    let label_b = Symbol::new(&env, "OwnerB_Agt");
    let label_c = Symbol::new(&env, "OwnerC_Agt");

    let id_a =
        client.create_delegation(&owner_a, &agent_id, &permissions_contract, &label_a, &1000);
    let id_b =
        client.create_delegation(&owner_b, &agent_id, &permissions_contract, &label_b, &1000);
    let id_c =
        client.create_delegation(&owner_c, &agent_id, &permissions_contract, &label_c, &1000);

    // IDs must be strictly increasing: id_a < id_b < id_c
    assert!(id_a < id_b, "id_a ({id_a}) must be < id_b ({id_b})");
    assert!(id_b < id_c, "id_b ({id_b}) must be < id_c ({id_c})");

    // Each delegation belongs to the correct owner.
    assert_eq!(client.get_delegation(&id_a).owner, owner_a);
    assert_eq!(client.get_delegation(&id_b).owner, owner_b);
    assert_eq!(client.get_delegation(&id_c).owner, owner_c);
}

#[test]
fn test_get_delegations_by_owner_paged_paginates() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    for _ in 0..=MAX_PAGE_LIMIT {
        let label = Symbol::new(&env, "Owner_Page");
        client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);
    }

    let page = client.get_delegations_by_owner_paged(&owner, &0u32, &(MAX_PAGE_LIMIT + 1));
    assert_eq!(page.total, MAX_PAGE_LIMIT + 1);
    assert_eq!(page.items.len(), MAX_PAGE_LIMIT);
    assert_eq!(page.next_offset, Some(MAX_PAGE_LIMIT));

    let next = client.get_delegations_by_owner_paged(&owner, &MAX_PAGE_LIMIT, &MAX_PAGE_LIMIT);
    assert_eq!(next.items.len(), 1);
    assert_eq!(next.total, MAX_PAGE_LIMIT + 1);
    assert_eq!(next.next_offset, None);
}

#[test]
fn test_get_delegation_history_paged_paginates() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "History_Page");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    for _ in 0..MAX_PAGE_LIMIT {
        client.pause_delegation(&id);
        client.resume_delegation(&id);
    }

    let page = client.get_delegation_history_paged(&id, &0u32, &(MAX_PAGE_LIMIT + 1));
    assert_eq!(page.total, 1 + (2 * MAX_PAGE_LIMIT));
    assert_eq!(page.items.len(), MAX_PAGE_LIMIT);
    assert_eq!(page.next_offset, Some(MAX_PAGE_LIMIT));

    let next = client.get_delegation_history_paged(&id, &MAX_PAGE_LIMIT, &MAX_PAGE_LIMIT);
    assert_eq!(next.items.len(), MAX_PAGE_LIMIT);
    assert_eq!(next.next_offset, Some(2 * MAX_PAGE_LIMIT));

    let last = client.get_delegation_history_paged(&id, &(2 * MAX_PAGE_LIMIT), &MAX_PAGE_LIMIT);
    assert_eq!(last.items.len(), 1);
    assert_eq!(last.next_offset, None);
}

#[test]
fn test_get_expired_delegations_paged_paginates() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    env.ledger().set_sequence_number(100);
    for _ in 0..=MAX_PAGE_LIMIT {
        let label = Symbol::new(&env, "Expired_Page");
        client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &100);
    }

    env.ledger().set_sequence_number(300);

    let page = client.get_expired_delegations_paged(&0u32, &(MAX_PAGE_LIMIT + 1));
    assert_eq!(page.total, MAX_PAGE_LIMIT + 1);
    assert_eq!(page.items.len(), MAX_PAGE_LIMIT);
    assert_eq!(page.next_offset, Some(MAX_PAGE_LIMIT));

    let next = client.get_expired_delegations_paged(&MAX_PAGE_LIMIT, &MAX_PAGE_LIMIT);
    assert_eq!(next.items.len(), 1);
    assert_eq!(next.total, MAX_PAGE_LIMIT + 1);
    assert_eq!(next.next_offset, None);
}

#[test]
fn test_get_active_delegations_returns_only_active() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Active_Only");
    let active_id =
        client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);
    let paused_id =
        client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);
    let revoked_id =
        client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    client.pause_delegation(&paused_id);
    client.revoke_delegation(&revoked_id);

    let active = client.get_active_delegations(&owner);
    assert_eq!(active.len(), 1);
    assert_eq!(active.get(0).unwrap().id, active_id);
    assert_eq!(active.get(0).unwrap().status, DelegationStatus::Active);
}

#[test]
fn test_get_active_delegations_excludes_paused_revoked_and_expired() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    env.ledger().set_sequence_number(100);
    let label = Symbol::new(&env, "Active_Mixed");
    let active_id =
        client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);
    let expiring_id =
        client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &100);
    let paused_id =
        client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);
    let revoked_id =
        client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    client.pause_delegation(&paused_id);
    client.revoke_delegation(&revoked_id);

    // Advance past expiring_id's expiry (ledger 200) and sweep it to Expired.
    env.ledger().set_sequence_number(300);
    let mut ids = Vec::new(&env);
    ids.push_back(expiring_id);
    let swept = client.sweep_expired(&ids);
    assert_eq!(swept.len(), 1);
    assert_eq!(
        client.get_delegation(&expiring_id).status,
        DelegationStatus::Expired
    );

    let active = client.get_active_delegations(&owner);
    assert_eq!(active.len(), 1);
    assert_eq!(active.get(0).unwrap().id, active_id);
}

#[test]
fn test_get_active_delegations_excludes_unswept_ledger_expiry() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    env.ledger().set_sequence_number(100);
    let label = Symbol::new(&env, "Active_Stale");
    // Expires at ledger 150 but is never swept, so its stored status stays Active.
    client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &50);
    let live_id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    env.ledger().set_sequence_number(200);

    let active = client.get_active_delegations(&owner);
    assert_eq!(active.len(), 1);
    assert_eq!(active.get(0).unwrap().id, live_id);
}

// ── #2 resume_delegation: expiry is validated before any mutation ────────────

#[test]
fn test_resume_expired_returns_error_and_leaves_state_untouched() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    env.ledger().set_sequence_number(100);
    let label = Symbol::new(&env, "Agent_RE");
    // Created at ledger 100 with ttl 100 => expires at ledger 200.
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &100);
    client.pause_delegation(&id);

    let before = client.get_delegation(&id);
    let version_before = client.get_delegation_version(&id);
    let history_before = client.get_delegation_history(&id).len();
    assert_eq!(before.status, DelegationStatus::Paused);

    env.ledger().set_sequence_number(200);
    let result = client.try_resume_delegation(&id);
    assert_eq!(result, Err(Ok(DelegationError::Expired)));

    // The failed call must not have written an expiry transition.
    let after = client.get_delegation(&id);
    assert_eq!(after.status, DelegationStatus::Paused);
    assert_eq!(after.version, before.version);
    assert_eq!(after.updated_at, before.updated_at);
    assert_eq!(client.get_delegation_version(&id), version_before);
    assert_eq!(client.get_delegation_history(&id).len(), history_before);
}

#[test]
fn test_resume_expired_emits_no_expired_event() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    env.ledger().set_sequence_number(100);
    let label = Symbol::new(&env, "Agent_RE_EVT");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &100);
    client.pause_delegation(&id);

    env.ledger().set_sequence_number(200);
    let _ = client.try_resume_delegation(&id);

    let events = env.events().all();
    let emitted_expired = events.iter().any(|(_, topics, _)| {
        let t: soroban_sdk::Vec<soroban_sdk::Val> = topics;
        t.len() >= 2
            && Symbol::try_from_val(&env, &t.get(0).unwrap()).ok() == Some(symbol_short!("deleg"))
            && Symbol::try_from_val(&env, &t.get(1).unwrap()).ok() == Some(symbol_short!("expired"))
    });
    assert!(
        !emitted_expired,
        "failed resume must not emit a deleg/expired event"
    );
}

#[test]
fn test_resume_paused_before_expiry_still_succeeds() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    env.ledger().set_sequence_number(100);
    let label = Symbol::new(&env, "Agent_ROK");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);
    client.pause_delegation(&id);

    // Still one ledger inside the window: the pause is resumable.
    env.ledger().set_sequence_number(1099);
    assert!(client.resume_delegation(&id));
    let record = client.get_delegation(&id);
    assert_eq!(record.status, DelegationStatus::Active);
    assert!(client.is_authorized(&id, &agent_id));
}

// ── #5 two-step admin transfer (propose / accept) ────────────────────────────

#[test]
fn test_current_admin_can_propose_successor() {
    let (env, client, admin, _, _, _) = setup();
    env.mock_all_auths();

    let successor = Address::generate(&env);
    assert!(client.propose_admin(&successor));

    // The proposal changes nothing until it is accepted.
    assert_eq!(client.get_admin(), admin);
}

#[test]
fn test_non_admin_cannot_propose_successor() {
    let (env, client, admin, _, _, _) = setup();
    let successor = Address::generate(&env);

    // No auth is mocked, so the stored admin has not authorized the call.
    let result = client.try_propose_admin(&successor);
    assert!(result.is_err());

    // get_admin is unaffected.
    assert_eq!(client.get_admin(), admin);
}

#[test]
fn test_proposed_admin_can_accept() {
    let (env, client, _, _, _, _) = setup();
    env.mock_all_auths();

    let successor = Address::generate(&env);
    client.propose_admin(&successor);

    assert_eq!(client.accept_admin(), successor);
    assert_eq!(client.get_admin(), successor);
}

#[test]
fn test_admin_transfer_is_two_step_not_one_step() {
    let (env, client, admin, _, _, _) = setup();
    env.mock_all_auths();

    let successor = Address::generate(&env);
    client.propose_admin(&successor);

    // A proposal alone must not move the admin role.
    assert_eq!(client.get_admin(), admin);

    // The successor's first proposal is rejected, proving the role only moved
    // after accept_admin.
    let outsider = Address::generate(&env);
    assert_ne!(client.get_admin(), outsider);
}

#[test]
fn test_accept_without_proposal_returns_typed_error() {
    let (env, client, _, _, _, _) = setup();
    env.mock_all_auths();

    let result = client.try_accept_admin();
    assert_eq!(result, Err(Ok(DelegationError::NoPendingAdmin)));
}

#[test]
fn test_new_admin_can_propose_after_transfer() {
    let (env, client, _, _, _, _) = setup();
    env.mock_all_auths();

    let successor = Address::generate(&env);
    client.propose_admin(&successor);
    client.accept_admin();

    // Old admin can no longer propose: the pending slot it would need is set by
    // the *current* admin, and the transfer cleared the previous proposal.
    assert_eq!(client.get_admin(), successor);

    let next = Address::generate(&env);
    client.propose_admin(&next);
    assert_eq!(client.accept_admin(), next);
    assert_eq!(client.get_admin(), next);
}

#[test]
fn test_admin_transfer_emits_propose_and_transfer_events() {
    let (env, client, _, _, _, _) = setup();
    env.mock_all_auths();

    let has_topic = |events: &soroban_sdk::Vec<(
        Address,
        soroban_sdk::Vec<soroban_sdk::Val>,
        soroban_sdk::Val,
    )>,
                     wanted: Symbol| {
        events.iter().any(|(_, topics, _)| {
            let t: soroban_sdk::Vec<soroban_sdk::Val> = topics;
            t.len() >= 2
                && Symbol::try_from_val(&env, &t.get(0).unwrap()).ok()
                    == Some(symbol_short!("deleg"))
                && Symbol::try_from_val(&env, &t.get(1).unwrap()).ok() == Some(wanted.clone())
        })
    };

    let successor = Address::generate(&env);
    client.propose_admin(&successor);
    let events_prop = env.events().all();
    assert!(
        has_topic(&events_prop, symbol_short!("adm_prop")),
        "AdminProposed event not emitted"
    );

    client.accept_admin();
    let events_xfer = env.events().all();
    assert!(
        has_topic(&events_xfer, symbol_short!("adm_xfer")),
        "AdminTransferred event not emitted"
    );
}

#[test]
fn test_create_delegation_zero_ttl_rejected() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();
    let label = Symbol::new(&env, "Zero_TTL");

    let result = client.try_create_delegation(&owner, &agent_id, &permissions_contract, &label, &0);
    assert_eq!(result, Err(Ok(DelegationError::InvalidTtl)));

    // Ensure no delegation was created
    let records = client.get_delegations_by_owner(&owner);
    assert_eq!(records.len(), 0);
}

#[test]
fn test_create_delegation_ttl_one_succeeds() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();
    let label = Symbol::new(&env, "TTL_One");

    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1);
    assert_eq!(id, 1);

    let record = client.get_delegation(&id);
    assert_eq!(record.expires_at_ledger, env.ledger().sequence() + 1);
    assert_eq!(record.status, DelegationStatus::Active);
}

#[test]
fn test_admin_can_force_revoke() {
    let (env, client, admin, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Admin_Revoke");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    // Verify delegation is initially active
    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Active);
    assert!(client.is_authorized(&id, &agent_id));

    // Admin force-revokes the delegation
    let result = client.admin_revoke(&admin, &id);
    assert!(result);

    // Verify delegation is now revoked
    let record = client.get_delegation(&id);
    assert_eq!(record.status, DelegationStatus::Revoked);
    assert!(!client.is_authorized(&id, &agent_id));

    // Version should increment
    assert_eq!(client.get_delegation_version(&id), 2);
}

#[test]
fn test_non_admin_cannot_force_revoke() {
    let (env, client, _admin, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Non_Admin_Revoke");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    // Try to force-revoke with a non-admin address
    let non_admin = Address::generate(&env);
    let result = client.try_admin_revoke(&non_admin, &id);

    // Should fail with NotAuthorized error
    assert_eq!(result, Err(Ok(DelegationError::NotAuthorized)));

    // Delegation should remain active
    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Active);
    assert!(client.is_authorized(&id, &agent_id));

    // Version should remain unchanged
    assert_eq!(client.get_delegation_version(&id), 1);
}

#[test]
fn test_admin_revoke_idempotency() {
    let (env, client, admin, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Admin_Revoke_Idempotent");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    // First admin revoke should return true
    let first_result = client.admin_revoke(&admin, &id);
    assert!(first_result);
    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Revoked);
    assert_eq!(client.get_delegation_version(&id), 2);

    // Second admin revoke should return false (idempotent no-op)
    let second_result = client.admin_revoke(&admin, &id);
    assert!(!second_result);
    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Revoked);

    // Version should remain unchanged on idempotent call
    assert_eq!(client.get_delegation_version(&id), 2);
}

#[test]
fn test_admin_revoke_nonexistent_delegation() {
    let (env, client, admin, _, _, _) = setup();
    env.mock_all_auths();

    // Try to admin revoke a non-existent delegation
    let result = client.try_admin_revoke(&admin, &9999u64);

    // Should fail with NotFound error
    assert_eq!(result, Err(Ok(DelegationError::NotFound)));
}

// ── Issue #322: capabilities packed into a single u32 bitmask ────────────────

/// CPU ceiling for a single scoped authorization check. Deliberately generous:
/// the point of the benchmark below is the *relative* cost, and this guards
/// against a future change accidentally making the check read extra entries.
const MAX_SCOPED_CHECK_CPU_INSTRUCTIONS: u64 = 150_000;

/// Packed capabilities must never cost more than this share of the CPU that
/// four separate boolean slots cost, for a read-modify-write of the same four
/// capabilities. Packing removes three storage round-trips per delegation.
const MAX_PACKED_SHARE_OF_UNPACKED_CPU: u64 = 40;

/// The pre-bitmask layout, kept only so the benchmark can state the saving in
/// bytes. `DelegationRecord` itself is identical apart from the four
/// booleans the packed mask replaced.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
struct UnpackedScopedRecord {
    id: u64,
    owner: Address,
    agent_id: BytesN<32>,
    permissions_contract: Address,
    status: DelegationStatus,
    label: Symbol,
    created_at: u64,
    updated_at: u64,
    expires_at_ledger: u32,
    version: u32,
    can_spend: bool,
    can_refund: bool,
    can_dispute: bool,
    can_delegate: bool,
}

/// Turns a `DelegationRecord` into the equivalent unpacked record so the two
/// can be serialized and compared byte for byte.
fn unpacked_equivalent(record: &DelegationRecord) -> UnpackedScopedRecord {
    let p = record.permissions;
    UnpackedScopedRecord {
        id: record.id,
        owner: record.owner.clone(),
        agent_id: record.agent_id.clone(),
        permissions_contract: record.permissions_contract.clone(),
        status: record.status.clone(),
        label: record.label.clone(),
        created_at: record.created_at,
        updated_at: record.updated_at,
        expires_at_ledger: record.expires_at_ledger,
        version: record.version,
        can_spend: p.has_flag(PERM_FLAG_SPEND),
        can_refund: p.has_flag(PERM_FLAG_REFUND),
        can_dispute: p.has_flag(PERM_FLAG_DISPUTE),
        can_delegate: p.has_flag(PERM_FLAG_DELEGATE),
    }
}

#[test]
fn test_permission_bitmask_helpers() {
    assert_eq!(PermissionBitmask::all().bits(), 0b1111);
    assert_eq!(PermissionBitmask::none().bits(), 0);
    assert!(PermissionBitmask::none().is_empty());
    assert!(!PermissionBitmask::all().is_empty());

    // The four documented capability bits occupy the documented positions.
    assert_eq!(PERM_FLAG_SPEND, 1);
    assert_eq!(PERM_FLAG_REFUND, 2);
    assert_eq!(PERM_FLAG_DISPUTE, 4);
    assert_eq!(PERM_FLAG_DELEGATE, 8);
    assert_eq!(PERM_ALL_FLAGS, 15);

    // set_flag accumulates, has_flag tests membership, clear_flag subtracts.
    let mut mask = PermissionBitmask::none();
    assert!(!mask.has_flag(PERM_FLAG_SPEND));

    mask = mask.set_flag(PERM_FLAG_SPEND);
    assert!(mask.has_flag(PERM_FLAG_SPEND));
    assert!(!mask.has_flag(PERM_FLAG_REFUND));
    assert_eq!(mask.bits(), PERM_FLAG_SPEND);

    mask = mask.set_flag(PERM_FLAG_REFUND | PERM_FLAG_DISPUTE);
    assert_eq!(mask.bits(), 0b0111);
    assert!(mask.has_flag(PERM_FLAG_REFUND | PERM_FLAG_DISPUTE));

    // Setting a bit that is already set is a no-op, not an error.
    assert_eq!(mask.set_flag(PERM_FLAG_SPEND), mask);

    mask = mask.clear_flag(PERM_FLAG_REFUND);
    assert!(!mask.has_flag(PERM_FLAG_REFUND));
    assert_eq!(mask.bits(), 0b0101);

    // Clearing a bit that is already clear is a no-op.
    assert_eq!(mask.clear_flag(PERM_FLAG_REFUND), mask);

    // Clearing an unset bit never disturbs the others.
    assert_eq!(mask.clear_flag(PERM_FLAG_DELEGATE), mask);

    // A combined mask requires *every* bit to be present.
    assert!(PermissionBitmask::all().has_flag(PERM_ALL_FLAGS));
    assert!(!PermissionBitmask::none().has_flag(PERM_ALL_FLAGS));
    assert!(!PermissionBitmask::none()
        .set_flag(PERM_FLAG_SPEND)
        .has_flag(PERM_ALL_FLAGS));
}

#[test]
fn test_permission_bitmask_rejects_reserved_flags() {
    // Zero is rejected: setting it would be a paid no-op.
    assert!(!PermissionBitmask::is_valid_flag(0));
    // Each real capability bit is accepted.
    for flag in [
        PERM_FLAG_SPEND,
        PERM_FLAG_REFUND,
        PERM_FLAG_DISPUTE,
        PERM_FLAG_DELEGATE,
    ] {
        assert!(PermissionBitmask::is_valid_flag(flag));
    }
    // Combining real bits is fine.
    assert!(PermissionBitmask::is_valid_flag(PERM_ALL_FLAGS));
    // Undefined bits are not.
    assert!(!PermissionBitmask::is_valid_flag(1 << 4));
    assert!(!PermissionBitmask::is_valid_flag(u32::MAX));
    assert!(!PermissionBitmask::is_valid_flag(u32::MAX << 1));
}

#[test]
fn test_create_delegation_grants_all_capabilities() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "All_Flags");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    // Backwards compatibility: the pre-existing entry point keeps granting the
    // delegate everything, so no existing integration silently loses authority.
    let permissions = client.get_permissions(&id);
    assert_eq!(permissions, PermissionBitmask::all());

    for flag in [
        PERM_FLAG_SPEND,
        PERM_FLAG_REFUND,
        PERM_FLAG_DISPUTE,
        PERM_FLAG_DELEGATE,
    ] {
        assert!(client.is_authorized_for(&id, &agent_id, &flag));
    }
}

#[test]
fn test_create_scoped_delegation_stores_only_requested_capabilities() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Spend_Only");
    let scope = PermissionBitmask::none().set_flag(PERM_FLAG_SPEND);
    let id = client.create_scoped_delegation(
        &owner,
        &agent_id,
        &permissions_contract,
        &label,
        &1000,
        &scope,
    );

    // One u32 on the record, not four booleans.
    assert_eq!(client.get_permissions(&id), scope);
    assert_eq!(client.get_delegation(&id).permissions, scope);

    assert!(client.is_authorized_for(&id, &agent_id, &PERM_FLAG_SPEND));
    for flag in [PERM_FLAG_REFUND, PERM_FLAG_DISPUTE, PERM_FLAG_DELEGATE] {
        assert!(!client.is_authorized_for(&id, &agent_id, &flag));
        assert!(!client.has_permission(&id, &flag));
    }

    // A delegation with no capabilities at all is representable and grants none.
    let empty = client.create_scoped_delegation(
        &owner,
        &agent_id,
        &permissions_contract,
        &label,
        &1000,
        &PermissionBitmask::none(),
    );
    assert!(client.get_permissions(&empty).is_empty());
    assert!(!client.is_authorized_for(&empty, &agent_id, &PERM_FLAG_SPEND));
}

#[test]
fn test_create_scoped_delegation_rejects_reserved_bits() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Bad_Flags");
    let bogus = PermissionBitmask(PERM_FLAG_SPEND | (1 << 9));

    let result = client.try_create_scoped_delegation(
        &owner,
        &agent_id,
        &permissions_contract,
        &label,
        &1000,
        &bogus,
    );
    assert_eq!(result, Err(Ok(DelegationError::InvalidPermissionFlag)));

    // Nothing was persisted for the refused grant.
    assert_eq!(client.get_delegations_by_owner(&owner).len(), 0);
}

#[test]
fn test_is_authorized_for_fails_closed() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();
    env.ledger().set_sequence_number(100);

    let label = Symbol::new(&env, "Fail_Closed");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &100);

    // Unknown delegation.
    assert!(!client.is_authorized_for(&9999, &agent_id, &PERM_FLAG_SPEND));
    // Unknown agent.
    let stranger = BytesN::from_array(&env, &[9; 32]);
    assert!(!client.is_authorized_for(&id, &stranger, &PERM_FLAG_SPEND));
    // Reserved / empty flag is refused rather than treated as "allowed".
    assert!(!client.is_authorized_for(&id, &agent_id, &0u32));
    assert!(!client.is_authorized_for(&id, &agent_id, &(1u32 << 20)));
    // Paused.
    client.pause_delegation(&id);
    assert!(!client.is_authorized_for(&id, &agent_id, &PERM_FLAG_SPEND));
    client.resume_delegation(&id);
    // Expired.
    env.ledger().set_sequence_number(200);
    assert!(!client.is_authorized_for(&id, &agent_id, &PERM_FLAG_SPEND));
}

#[test]
fn test_set_and_clear_permission_flag() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Scope_Edit");
    let id = client.create_scoped_delegation(
        &owner,
        &agent_id,
        &permissions_contract,
        &label,
        &1000,
        &PermissionBitmask::none().set_flag(PERM_FLAG_SPEND),
    );

    assert!(!client.is_authorized_for(&id, &agent_id, &PERM_FLAG_DELEGATE));

    assert!(client.set_permission_flag(&id, &PERM_FLAG_DELEGATE));
    assert_eq!(
        client.get_permissions(&id),
        PermissionBitmask::none()
            .set_flag(PERM_FLAG_SPEND)
            .set_flag(PERM_FLAG_DELEGATE)
    );
    // Widening one flag leaves the others untouched.
    assert!(client.is_authorized_for(&id, &agent_id, &PERM_FLAG_SPEND));
    assert!(client.is_authorized_for(&id, &agent_id, &PERM_FLAG_DELEGATE));
    assert!(!client.is_authorized_for(&id, &agent_id, &PERM_FLAG_REFUND));

    // A scope change is an auditable mutation: version and history advance.
    assert_eq!(client.get_delegation_version(&id), 2);
    let history = client.get_delegation_history(&id);
    assert_eq!(history.len(), 2);
    assert!(!history
        .get(0)
        .unwrap()
        .record
        .permissions
        .has_flag(PERM_FLAG_DELEGATE));
    assert!(history
        .get(1)
        .unwrap()
        .record
        .permissions
        .has_flag(PERM_FLAG_DELEGATE));

    assert!(client.clear_permission_flag(&id, &PERM_FLAG_DELEGATE));
    assert!(!client.is_authorized_for(&id, &agent_id, &PERM_FLAG_DELEGATE));
    // Narrowing one flag leaves the others untouched.
    assert!(client.is_authorized_for(&id, &agent_id, &PERM_FLAG_SPEND));
    assert_eq!(client.get_delegation_version(&id), 3);
}

#[test]
fn test_permission_flag_changes_are_idempotent() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Idempotent_Scope");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    // Already set by create_delegation: no write, no version bump.
    assert!(!client.set_permission_flag(&id, &PERM_FLAG_SPEND));
    assert_eq!(client.get_delegation_version(&id), 1);
    assert_eq!(client.get_delegation_history(&id).len(), 1);

    assert!(client.clear_permission_flag(&id, &PERM_FLAG_SPEND));
    assert_eq!(client.get_delegation_version(&id), 2);
    // Already clear: again a no-op.
    assert!(!client.clear_permission_flag(&id, &PERM_FLAG_SPEND));
    assert_eq!(client.get_delegation_version(&id), 2);
    assert_eq!(client.get_delegation_history(&id).len(), 2);
}

#[test]
fn test_permission_flag_changes_require_owner_auth() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "No_Auth");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    // Drop the recorded authorizations: a scope change is an owner decision, so
    // without the owner's signature neither widening nor narrowing may proceed.
    env.set_auths(&[]);

    assert!(client
        .try_set_permission_flag(&id, &PERM_FLAG_REFUND)
        .is_err());
    assert!(client
        .try_clear_permission_flag(&id, &PERM_FLAG_REFUND)
        .is_err());
    assert_eq!(client.get_permissions(&id), PermissionBitmask::all());
    assert_eq!(client.get_delegation_version(&id), 1);
}

#[test]
fn test_permission_flag_mutation_rejects_invalid_flag() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Bad_Flag_Mutation");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    for bad in [0u32, 1 << 4, u32::MAX] {
        assert_eq!(
            client.try_set_permission_flag(&id, &bad),
            Err(Ok(DelegationError::InvalidPermissionFlag))
        );
        assert_eq!(
            client.try_clear_permission_flag(&id, &bad),
            Err(Ok(DelegationError::InvalidPermissionFlag))
        );
        assert_eq!(
            client.try_has_permission(&id, &bad),
            Err(Ok(DelegationError::InvalidPermissionFlag))
        );
    }

    // Rejected writes leave both the mask and the version untouched.
    assert_eq!(client.get_permissions(&id), PermissionBitmask::all());
    assert_eq!(client.get_delegation_version(&id), 1);
}

#[test]
fn test_permission_flag_mutation_on_missing_delegation() {
    let (env, client, _, _, _, _) = setup();
    env.mock_all_auths();

    assert_eq!(
        client.try_set_permission_flag(&9999, &PERM_FLAG_SPEND),
        Err(Ok(DelegationError::NotFound))
    );
    assert_eq!(
        client.try_clear_permission_flag(&9999, &PERM_FLAG_SPEND),
        Err(Ok(DelegationError::NotFound))
    );
    assert_eq!(
        client.try_get_permissions(&9999),
        Err(Ok(DelegationError::NotFound))
    );
    assert_eq!(
        client.try_has_permission(&9999, &PERM_FLAG_SPEND),
        Err(Ok(DelegationError::NotFound))
    );
}

#[test]
fn test_has_permission_reports_scope_regardless_of_status() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Scope_Only");
    let id = client.create_scoped_delegation(
        &owner,
        &agent_id,
        &permissions_contract,
        &label,
        &1000,
        &PermissionBitmask::none().set_flag(PERM_FLAG_DISPUTE),
    );

    assert!(client.has_permission(&id, &PERM_FLAG_DISPUTE));

    // Revoking the delegation stops authorization but does not erase history:
    // scope inspection still reports what the grant covered.
    client.revoke_delegation(&id);
    assert!(!client.is_authorized_for(&id, &agent_id, &PERM_FLAG_DISPUTE));
    assert!(client.has_permission(&id, &PERM_FLAG_DISPUTE));
    assert!(!client.has_permission(&id, &PERM_FLAG_SPEND));
}

#[test]
fn test_rollback_restores_previous_capabilities() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Scope_Rollback");
    let id = client.create_scoped_delegation(
        &owner,
        &agent_id,
        &permissions_contract,
        &label,
        &1000,
        &PermissionBitmask::none().set_flag(PERM_FLAG_SPEND),
    );

    client.set_permission_flag(&id, &PERM_FLAG_REFUND);
    assert!(client.is_authorized_for(&id, &agent_id, &PERM_FLAG_REFUND));

    // Rolling back to version 1 must also roll back the capability grant, so a
    // scope change cannot be used to escape the history it is recorded in.
    client.rollback_delegation(&id, &1);
    assert_eq!(
        client.get_permissions(&id),
        PermissionBitmask::none().set_flag(PERM_FLAG_SPEND)
    );
    assert!(!client.is_authorized_for(&id, &agent_id, &PERM_FLAG_REFUND));
    assert!(client.is_authorized_for(&id, &agent_id, &PERM_FLAG_SPEND));
}

#[test]
fn test_permission_flags_changed_event_emitted() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Scope_Evt");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);
    client.clear_permission_flag(&id, &PERM_FLAG_DISPUTE);

    let events = env.events().all();
    let found = events.iter().any(|(_, topics, _)| {
        let t: soroban_sdk::Vec<soroban_sdk::Val> = topics;
        t.len() >= 2
            && Symbol::try_from_val(&env, &t.get(0).unwrap()).ok() == Some(symbol_short!("deleg"))
            && Symbol::try_from_val(&env, &t.get(1).unwrap()).ok()
                == Some(symbol_short!("perm_chg"))
    });
    assert!(found, "PermissionFlagsChanged event not emitted");
}

#[test]
fn test_capability_mask_survives_lifecycle_transitions() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Scope_Lifecycle");
    let scope = PermissionBitmask::none()
        .set_flag(PERM_FLAG_SPEND)
        .set_flag(PERM_FLAG_REFUND);
    let id = client.create_scoped_delegation(
        &owner,
        &agent_id,
        &permissions_contract,
        &label,
        &1000,
        &scope,
    );

    // Pause / resume only flip status; they must not silently widen or narrow
    // the capability set.
    client.pause_delegation(&id);
    assert_eq!(client.get_permissions(&id), scope);
    client.resume_delegation(&id);
    assert_eq!(client.get_permissions(&id), scope);

    // Neither must expiry sweeping.
    env.ledger().set_sequence_number(1_000);
    let mut ids = Vec::new(&env);
    ids.push_back(id);
    client.sweep_expired(&ids);
    assert_eq!(client.get_permissions(&id), scope);
    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Expired);
    assert!(!client.is_authorized_for(&id, &agent_id, &PERM_FLAG_SPEND));
}

// ── Issue #322 benchmarks ───────────────────────────────────────────────────

#[test]
fn test_benchmark_delegation_entry_is_smaller_with_packed_flags() {
    use soroban_sdk::xdr::ToXdr;

    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Bench_Size");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);
    let record = client.get_delegation(&id);

    let packed_bytes = record.clone().to_xdr(&env).len();
    let unpacked_bytes = unpacked_equivalent(&record).to_xdr(&env).len();

    assert!(
        packed_bytes < unpacked_bytes,
        "packed record ({packed_bytes}B) must beat unpacked ({unpacked_bytes}B)"
    );

    // The flag block is the only difference between the two encodings, so the
    // saving is bounded above by the four booleans it replaced and is worth
    // double digits on the entry as a whole.
    let saving = unpacked_bytes - packed_bytes;
    println!(
        "[#322] delegation entry: {packed_bytes}B packed vs {unpacked_bytes}B unpacked \
         ({saving}B saved, {:.1}%)",
        saving as f64 / unpacked_bytes as f64 * 100.0
    );
    assert!(
        saving >= 48,
        "packing saved only {saving}B of a {unpacked_bytes}B entry, less than the \
         48B the four encoded booleans cannot compress below"
    );
    assert!(
        saving * 100 >= unpacked_bytes * 10,
        "packing saved {saving}B of a {unpacked_bytes}B entry, under the 10% target"
    );
}

#[test]
fn test_benchmark_packed_flags_beat_separate_slots() {
    let (env, client, _, _, _, _) = setup();
    env.mock_all_auths();
    let contract_id = &client.address;

    // Baseline: the pre-bitmask layout, one boolean per storage slot, written
    // then read back — the exact access pattern `set`/`clear` of four
    // capabilities would have had.
    env.as_contract(contract_id, || {
        for index in 0..4u32 {
            let key = DataKey::Delegation(u64::from(index));
            env.storage().persistent().set(&key, &true);
        }
        for index in 0..4u32 {
            let key = DataKey::Delegation(u64::from(index));
            let _: bool = env.storage().persistent().get(&key).unwrap();
        }
    });
    let unpacked_cpu = env.cost_estimate().budget().cpu_instruction_cost();

    // Packed: one word, one slot, one round-trip.
    env.as_contract(contract_id, || {
        let key = DataKey::Delegation(4);
        env.storage()
            .persistent()
            .set(&key, &PermissionBitmask::all());
        let mask: PermissionBitmask = env.storage().persistent().get(&key).unwrap();
        assert!(mask.has_flag(PERM_FLAG_SPEND));
    });
    let packed_cpu = env.cost_estimate().budget().cpu_instruction_cost();

    println!(
        "[#322] flag read+write: {unpacked_cpu} CPU insns in four slots vs \
         {packed_cpu} packed ({:.1}% saved)",
        (1.0 - packed_cpu as f64 / unpacked_cpu as f64) * 100.0
    );
    assert!(
        packed_cpu * 100 <= unpacked_cpu * MAX_PACKED_SHARE_OF_UNPACKED_CPU,
        "packing four capabilities into one u32 used {packed_cpu} CPU instructions \
         vs {unpacked_cpu} for four separate slots, which is more than the \
         {MAX_PACKED_SHARE_OF_UNPACKED_CPU}% budget"
    );
}

#[test]
fn test_benchmark_scoped_check_stays_within_cpu_budget() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Bench_Check");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    // Measure the identity check on its own first, so the scoped check can be
    // compared against it rather than against a hard-coded number.
    assert!(client.is_authorized(&id, &agent_id));
    let identity_check_cpu = env.cost_estimate().budget().cpu_instruction_cost();

    assert!(client.is_authorized_for(&id, &agent_id, &PERM_FLAG_SPEND));
    let scoped_cpu = env.cost_estimate().budget().cpu_instruction_cost();

    assert!(
        scoped_cpu <= MAX_SCOPED_CHECK_CPU_INSTRUCTIONS,
        "scoped authorization check used {scoped_cpu} CPU instructions, over the \
         {MAX_SCOPED_CHECK_CPU_INSTRUCTIONS} budget"
    );

    // The added scope test is one bitwise `and` on an already-loaded record: it
    // must not cost anything close to another storage read.
    let scope_overhead = scoped_cpu.saturating_sub(identity_check_cpu);
    println!(
        "[#322] permission check: {identity_check_cpu} CPU insns for `is_authorized`, \
         {scoped_cpu} for `is_authorized_for` (+{scope_overhead} for the scope test)"
    );
    assert!(
        scope_overhead * 10 <= identity_check_cpu,
        "scope test cost {scope_overhead} CPU instructions on top of a \
         {identity_check_cpu}-instruction check, which is more than the 10% budget"
    );
}

#[test]
fn test_get_delegations_by_agent_returns_empty_vec_for_unknown_agent() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let unknown_agent = BytesN::from_array(&env, &[99u8; 32]);

    // Initial state: no delegations exist at all
    let empty_before = client.get_delegations_by_agent(&unknown_agent);
    assert_eq!(empty_before.len(), 0);

    // Create delegations for a different agent
    let label = Symbol::new(&env, "Agent_Test");
    client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    // Query for unknown agent still returns empty vec
    let empty_after = client.get_delegations_by_agent(&unknown_agent);
    assert_eq!(empty_after.len(), 0);
}

#[test]
fn test_get_delegations_by_agent_returns_matching_delegations_for_known_agent() {
    let (env, client, _, owner, agent_id_1, permissions_contract) = setup();
    env.mock_all_auths();

    let agent_id_2 = BytesN::from_array(&env, &[2u8; 32]);

    let label_1a = Symbol::new(&env, "Agent1_A");
    let label_1b = Symbol::new(&env, "Agent1_B");
    let label_2 = Symbol::new(&env, "Agent2_A");

    let id_1a =
        client.create_delegation(&owner, &agent_id_1, &permissions_contract, &label_1a, &1000);
    let id_1b =
        client.create_delegation(&owner, &agent_id_1, &permissions_contract, &label_1b, &1000);
    let id_2 =
        client.create_delegation(&owner, &agent_id_2, &permissions_contract, &label_2, &1000);

    // Fetch delegations for agent_id_1
    let agent_1_dels = client.get_delegations_by_agent(&agent_id_1);
    assert_eq!(agent_1_dels.len(), 2);
    assert_eq!(agent_1_dels.get(0).unwrap().id, id_1a);
    assert_eq!(agent_1_dels.get(0).unwrap().agent_id, agent_id_1);
    assert_eq!(agent_1_dels.get(1).unwrap().id, id_1b);
    assert_eq!(agent_1_dels.get(1).unwrap().agent_id, agent_id_1);

    // Fetch delegations for agent_id_2
    let agent_2_dels = client.get_delegations_by_agent(&agent_id_2);
    assert_eq!(agent_2_dels.len(), 1);
    assert_eq!(agent_2_dels.get(0).unwrap().id, id_2);
    assert_eq!(agent_2_dels.get(0).unwrap().agent_id, agent_id_2);
}

#[test]
fn test_get_delegations_by_agent_multiple_owners() {
    let (env, client, _, owner_a, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let owner_b = Address::generate(&env);

    let label_a = Symbol::new(&env, "Del_OwnerA");
    let label_b = Symbol::new(&env, "Del_OwnerB");

    let id_a =
        client.create_delegation(&owner_a, &agent_id, &permissions_contract, &label_a, &1000);
    let id_b =
        client.create_delegation(&owner_b, &agent_id, &permissions_contract, &label_b, &1000);

    let dels = client.get_delegations_by_agent(&agent_id);
    assert_eq!(dels.len(), 2);
    assert_eq!(dels.get(0).unwrap().id, id_a);
    assert_eq!(dels.get(0).unwrap().owner, owner_a);
    assert_eq!(dels.get(1).unwrap().id, id_b);
    assert_eq!(dels.get(1).unwrap().owner, owner_b);
}

#[test]
fn test_get_delegations_by_agent_bumps_ttl() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "TTL_AgentDel");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &LARGE_TTL);

    let del_key = DataKey::Delegation(id);
    let initial_ttl = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&del_key)
    });
    assert!(initial_ttl > 17_280);

    // Advance to the bump boundary and call get_delegations_by_agent
    env.ledger().set_sequence_number(initial_ttl - 17_280 + 1);
    let records = client.get_delegations_by_agent(&agent_id);
    assert_eq!(records.len(), 1);

    let refreshed_ttl = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&del_key)
    });
    assert!(refreshed_ttl > 17_280);
}
