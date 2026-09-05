#![cfg(test)]
#![allow(clippy::too_many_lines)]

use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{storage::Persistent, Address as _, Events, Ledger},
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

#[test]
fn test_rollback_rejects_stale_permissions_pointer() {
    let (env, client, _, owner, agent_id, permissions_contract) = setup();
    env.mock_all_auths();

    let label = Symbol::new(&env, "Rotated_Pointer");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);

    let rotated_permissions = Address::generate(&env);
    let mut record = client.get_delegation(&id);
    record.permissions_contract = rotated_permissions.clone();

    env.as_contract(&client.address, || {
        env.storage().persistent().set(&DataKey::Delegation(id), &record);
    });

    let result = client.try_rollback_delegation(&id, &1u32);
    assert_eq!(result, Err(Ok(DelegationError::InvalidVersion)));
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

// ── TTL bumping on reads (#94) ──────────────────────────────────────────────

/// Large TTL in ledgers so the delegation's `expires_at_ledger` is never
/// reached when we advance the ledger sequence to the storage-TTL
/// boundary (thousands of sequences later).
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
// ── #90 checked_add boundary test ───────────────────────────────────────────
fn test_create_delegation_returns_id_exhausted_at_boundary() {
    // Directly set NextId to u64::MAX so the next increment overflows
    env.storage()
        .instance()
        .set(&DataKey::NextId, &u64::MAX);
    let result =
        client.try_create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);
    assert_eq!(result, Err(Ok(DelegationError::IdExhausted)));
fn test_rollback_cannot_revive_past_expiry_snapshot() {
    env.ledger().set_sequence_number(100);
    let label = Symbol::new(&env, "Past_Expiry");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &100);
    // Create a second version so there is a v1 snapshot to roll back to.
    // Advance the ledger past the original expiry (created at 100, ttl 100 => expires at 200).
    env.ledger().set_sequence_number(300);
    // Rolling back to the Active v1 snapshot must NOT revive a delegation
    // that has already expired; it should be marked Expired instead.
    client.rollback_delegation(&id, &1u32);
    assert_eq!(record.status, DelegationStatus::Expired);
// ── Lifecycle edge-case tests ─────────────────────────────────────────────
/// Rollback must be rejected when the target snapshot was captured at or
/// after expiry — reviving an expired delegation must never be allowed.
///
/// `sweep_expired` transitions the delegation to Expired and creates a
/// versioned snapshot. We then bump to the next version via
/// `revoke_delegation` so the Expired snapshot is a valid lower target,
/// then verify the rollback is still refused.
fn test_rollback_to_expired_snapshot_rejected() {
    let label = Symbol::new(&env, "Expired_Rollback");
    let id = client.create_delegation(
        &label,
        &100, // expires at ledger 200
    // v1 snapshot (Active)
    client.pause_delegation(&id); // v2 snapshot (Paused)
    // Advance past expiry and sweep — creates v3 Expired snapshot.
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
    // Revoke from Expired → bumps to v4 so v3 is now a valid lower target.
    client.revoke_delegation(&id);
    assert_eq!(client.get_delegation_version(&id), 4);
    assert_eq!(client.get_delegation_history(&id).len(), 4);
    // Rolling back to v3 (the Expired snapshot) must be rejected.
    let result = client.try_rollback_delegation(&id, &3u32);
    assert_eq!(result, Err(Ok(DelegationError::Expired)));
    // Status must remain Revoked, version unchanged.
    assert_eq!(client.get_delegation(&id).status, DelegationStatus::Revoked);
/// History must grow by exactly one entry for each state transition.
fn test_history_grows_across_transitions() {
    let label = Symbol::new(&env, "History_Growth");
    let id = client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &1000);
    // After creation: 1 snapshot (v1)
    assert_eq!(history.len(), 1);
    assert_eq!(history.get(0).unwrap().version, 1);
    // After pause: 2 snapshots (v1, v2)
    client.pause_delegation(&id);
    assert_eq!(history.len(), 2);
    assert_eq!(history.get(1).unwrap().version, 2);
    // After resume: 3 snapshots (v1, v2, v3)
    client.resume_delegation(&id);
    assert_eq!(history.get(2).unwrap().version, 3);
    // After revoke: 4 snapshots (v1, v2, v3, v4)
    assert_eq!(history.len(), 4);
    assert_eq!(history.get(3).unwrap().version, 4);
    // Each snapshot must carry the correct status.
        history.get(0).unwrap().record.status,
        DelegationStatus::Active
        history.get(1).unwrap().record.status,
        DelegationStatus::Paused
        history.get(3).unwrap().record.status,
        DelegationStatus::Revoked
/// Delegation IDs must be strictly increasing even when created by
/// different owners.
fn test_cross_owner_delegation_id_monotonicity() {
    let (env, client, _, _, _, _) = setup();
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
fn test_get_delegations_by_owner_paged_paginates() {
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
fn test_get_delegation_history_paged_paginates() {
    let label = Symbol::new(&env, "History_Page");
    for _ in 0..MAX_PAGE_LIMIT {
        client.pause_delegation(&id);
        client.resume_delegation(&id);
    let page = client.get_delegation_history_paged(&id, &0u32, &(MAX_PAGE_LIMIT + 1));
    assert_eq!(page.total, 1 + (2 * MAX_PAGE_LIMIT));
    let next = client.get_delegation_history_paged(&id, &MAX_PAGE_LIMIT, &MAX_PAGE_LIMIT);
    assert_eq!(next.items.len(), MAX_PAGE_LIMIT);
    assert_eq!(next.next_offset, Some(2 * MAX_PAGE_LIMIT));
    let last = client.get_delegation_history_paged(&id, &(2 * MAX_PAGE_LIMIT), &MAX_PAGE_LIMIT);
    assert_eq!(last.items.len(), 1);
    assert_eq!(last.next_offset, None);
fn test_get_expired_delegations_paged_paginates() {
        let label = Symbol::new(&env, "Expired_Page");
        client.create_delegation(&owner, &agent_id, &permissions_contract, &label, &100);
    let page = client.get_expired_delegations_paged(&owner, &0u32, &(MAX_PAGE_LIMIT + 1));
    let next = client.get_expired_delegations_paged(&owner, &MAX_PAGE_LIMIT, &MAX_PAGE_LIMIT);
}
