use super::{ReputationContract, ReputationError};
use soroban_sdk::{testutils::Address as _, Address, Env};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn setup() -> (Env, super::ReputationContractClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(ReputationContract, ());
    let client = super::ReputationContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin);
    (env, client, admin)
}

// ── Initialisation ────────────────────────────────────────────────────────────

#[test]
fn test_initialize_sets_admin() {
    let (_env, client, admin) = setup();
    assert_eq!(client.get_admin(), admin);
}

#[test]
fn test_double_initialize_fails() {
    let (_env, client, admin) = setup();
    let result = client.try_initialize(&admin);
    assert_eq!(result, Err(Ok(ReputationError::AlreadyInitialized)));
}

// ── record_transaction ────────────────────────────────────────────────────────

#[test]
fn test_record_transaction_basic() {
    let (env, client, _admin) = setup();
    let entity = Address::generate(&env);
    client.record_transaction(&entity, &1000, &5);
    let score = client.get_reputation(&entity);
    assert_eq!(score.transaction_count, 1);
    assert_eq!(score.average_rating, 5);
    assert_eq!(score.total_rating, 5);
}

#[test]
fn test_record_transaction_multiple_computes_average() {
    let (env, client, _admin) = setup();
    let entity = Address::generate(&env);
    client.record_transaction(&entity, &100, &4);
    client.record_transaction(&entity, &200, &2);
    let score = client.get_reputation(&entity);
    assert_eq!(score.transaction_count, 2);
    // (4 + 2) / 2 = 3
    assert_eq!(score.average_rating, 3);
    assert_eq!(score.total_rating, 6);
}

#[test]
fn test_record_transaction_invalid_rating_zero() {
    let (env, client, _admin) = setup();
    let entity = Address::generate(&env);
    let result = client.try_record_transaction(&entity, &100, &0);
    assert_eq!(result, Err(Ok(ReputationError::InvalidRating)));
}

#[test]
fn test_record_transaction_invalid_rating_above_max() {
    let (env, client, _admin) = setup();
    let entity = Address::generate(&env);
    let result = client.try_record_transaction(&entity, &100, &6);
    assert_eq!(result, Err(Ok(ReputationError::InvalidRating)));
}

#[test]
fn test_record_transaction_invalid_amount_zero() {
    let (env, client, _admin) = setup();
    let entity = Address::generate(&env);
    let result = client.try_record_transaction(&entity, &0, &3);
    assert_eq!(result, Err(Ok(ReputationError::InvalidAmount)));
}

#[test]
fn test_record_transaction_invalid_amount_negative() {
    let (env, client, _admin) = setup();
    let entity = Address::generate(&env);
    let result = client.try_record_transaction(&entity, &-1, &3);
    assert_eq!(result, Err(Ok(ReputationError::InvalidAmount)));
}

#[test]
fn test_record_transaction_min_rating() {
    let (env, client, _admin) = setup();
    let entity = Address::generate(&env);
    client.record_transaction(&entity, &500, &1);
    let score = client.get_reputation(&entity);
    assert_eq!(score.average_rating, 1);
}

#[test]
fn test_record_transaction_max_rating() {
    let (env, client, _admin) = setup();
    let entity = Address::generate(&env);
    client.record_transaction(&entity, &500, &5);
    let score = client.get_reputation(&entity);
    assert_eq!(score.average_rating, 5);
}

#[test]
fn test_record_transaction_accumulates_entity_score() {
    let (env, client, _admin) = setup();
    let entity = Address::generate(&env);
    for _ in 0..10 {
        client.record_transaction(&entity, &100, &3);
    }
    let score = client.get_reputation(&entity);
    assert_eq!(score.transaction_count, 10);
    assert_eq!(score.average_rating, 3);
}

#[test]
fn test_record_transaction_different_entities_independent() {
    let (env, client, _admin) = setup();
    let a = Address::generate(&env);
    let b = Address::generate(&env);
    client.record_transaction(&a, &100, &5);
    client.record_transaction(&b, &100, &1);
    assert_eq!(client.get_reputation(&a).average_rating, 5);
    assert_eq!(client.get_reputation(&b).average_rating, 1);
}

// ── get_reputation ────────────────────────────────────────────────────────────

#[test]
fn test_get_reputation_not_found() {
    let (env, client, _admin) = setup();
    let unknown = Address::generate(&env);
    let result = client.try_get_reputation(&unknown);
    assert_eq!(result, Err(Ok(ReputationError::NotFound)));
}

// ── get_history ───────────────────────────────────────────────────────────────

#[test]
fn test_get_history_empty_for_unknown() {
    let (env, client, _admin) = setup();
    let unknown = Address::generate(&env);
    assert_eq!(client.get_history(&unknown).len(), 0);
}

#[test]
fn test_get_history_grows_with_transactions() {
    let (env, client, _admin) = setup();
    let entity = Address::generate(&env);
    client.record_transaction(&entity, &100, &4);
    client.record_transaction(&entity, &200, &5);
    assert_eq!(client.get_history(&entity).len(), 2);
}

// ── prune_entity_history ──────────────────────────────────────────────────────

#[test]
fn test_prune_entity_history_removes_oldest() {
    let (env, client, admin) = setup();
    let entity = Address::generate(&env);
    for i in 1u32..=5 {
        client.record_transaction(&entity, &100, &i);
    }
    assert_eq!(client.get_history(&entity).len(), 5);
    // prune to 3 records
    let removed = client.prune_entity_history(&admin, &entity, &3);
    assert_eq!(removed, 2);
    assert_eq!(client.get_history(&entity).len(), 3);
}

#[test]
fn test_prune_entity_history_no_op_when_within_limit() {
    let (env, client, admin) = setup();
    let entity = Address::generate(&env);
    client.record_transaction(&entity, &100, &3);
    let removed = client.prune_entity_history(&admin, &entity, &10);
    assert_eq!(removed, 0);
    assert_eq!(client.get_history(&entity).len(), 1);
}

#[test]
fn test_prune_entity_history_unauthorized() {
    let (env, client, _admin) = setup();
    let entity = Address::generate(&env);
    let stranger = Address::generate(&env);
    client.record_transaction(&entity, &100, &3);
    let result = client.try_prune_entity_history(&stranger, &entity, &5);
    assert_eq!(result, Err(Ok(ReputationError::Unauthorized)));
}

#[test]
fn test_prune_entity_history_batch_too_large() {
    let (env, client, admin) = setup();
    let entity = Address::generate(&env);
    // max_records = 51 exceeds MAX_PRUNE_BATCH (50)
    let result = client.try_prune_entity_history(&admin, &entity, &51);
    assert_eq!(result, Err(Ok(ReputationError::BatchTooLarge)));
}

#[test]
fn test_prune_entity_history_recomputes_score() {
    let (env, client, admin) = setup();
    let entity = Address::generate(&env);
    // 3 transactions: ratings 1, 1, 5
    client.record_transaction(&entity, &100, &1);
    client.record_transaction(&entity, &100, &1);
    client.record_transaction(&entity, &100, &5);
    // prune to 1 (keep only the last: rating 5)
    client.prune_entity_history(&admin, &entity, &1);
    let score = client.get_reputation(&entity);
    assert_eq!(score.transaction_count, 1);
    assert_eq!(score.average_rating, 5);
}

// ── Admin management ──────────────────────────────────────────────────────────

#[test]
fn test_propose_and_accept_admin() {
    let (env, client, admin) = setup();
    let new_admin = Address::generate(&env);
    client.propose_admin(&admin, &new_admin);
    client.accept_admin(&new_admin);
    assert_eq!(client.get_admin(), new_admin);
}

#[test]
fn test_propose_admin_unchanged_returns_false() {
    let (env, client, admin) = setup();
    let new_admin = Address::generate(&env);
    // first proposal
    let r1 = client.propose_admin(&admin, &new_admin);
    assert!(r1);
    // same proposal again
    let r2 = client.propose_admin(&admin, &new_admin);
    assert!(!r2);
}

#[test]
fn test_propose_admin_unauthorized() {
    let (env, client, _admin) = setup();
    let stranger = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let result = client.try_propose_admin(&stranger, &new_admin);
    assert_eq!(result, Err(Ok(ReputationError::Unauthorized)));
}

#[test]
fn test_accept_admin_no_pending() {
    let (env, client, _admin) = setup();
    let stranger = Address::generate(&env);
    let result = client.try_accept_admin(&stranger);
    assert_eq!(result, Err(Ok(ReputationError::NoPendingAdmin)));
}

#[test]
fn test_accept_admin_wrong_caller() {
    let (env, client, admin) = setup();
    let proposed = Address::generate(&env);
    let wrong = Address::generate(&env);
    client.propose_admin(&admin, &proposed);
    let result = client.try_accept_admin(&wrong);
    assert_eq!(result, Err(Ok(ReputationError::NotPendingAdmin)));
}

// ── version ───────────────────────────────────────────────────────────────────

#[test]
fn test_version() {
    let (_env, client, _admin) = setup();
    assert_eq!(client.version(), 1);
}
