use crate::*;
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

fn setup(
    env: &Env,
    admin: &Address,
    config: ReputationConfig,
) -> ReputationContractClient<'static> {
    let contract_id = env.register(ReputationContract, (admin.clone(), config));
    ReputationContractClient::new(env, &contract_id)
}

#[test]
fn test_issue_blinded_review_token() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    let client = setup(&env, &admin, config);

    let escrow_id = 12345u64;
    let blinded_key = BytesN::<32>::from_array(&env, &[42u8; 32]);

    let receipt = client.issue_blinded_review_token(&escrow_id, &blinded_key);

    assert_eq!(receipt.blinded_public_key, blinded_key);
    // Verify escrow_id is committed in the receipt
    assert_eq!(receipt.escrow_id_commitment.get(0), Some(57)); // escrow_id.to_le_bytes()[0]
}

#[test]
fn test_submit_blinded_review_prevents_double_submission() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let entity = Address::generate(&env);
    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    let client = setup(&env, &admin, config);

    let receipt = BlindedReviewReceipt {
        escrow_id_commitment: BytesN::<32>::from_array(&env, &[1u8; 32]),
        blinded_public_key: BytesN::<32>::from_array(&env, &[2u8; 32]),
        blind_signature: BytesN::<64>::from_array(&env, &[0u8; 64]),
        issued_at: env.ledger().timestamp(),
    };

    let nullifier = BytesN::<32>::from_array(&env, &[99u8; 32]);
    let rating = 8_000u32; // 80%

    // First submission succeeds
    client.submit_blinded_review(&receipt, &entity, &rating, &nullifier);

    // Second submission with same nullifier should fail
    let result = client.try_submit_blinded_review(&receipt, &entity, &rating, &nullifier);
    assert_eq!(result, Err(Ok(ReputationError::BlindedReceiptAlreadyUsed)));
}

#[test]
fn test_submit_blinded_review_updates_entity_rating() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let entity = Address::generate(&env);
    let rater = Address::generate(&env);
    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 1,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    let client = setup(&env, &admin, config);

    // A rating is only publicly visible once the entity has a transaction
    // history, so record one before submitting the blinded review.
    client.record_transaction(
        &admin,
        &1,
        &entity,
        &rater,
        &1_000,
        &TransactionOutcome::Released,
    );

    let receipt = BlindedReviewReceipt {
        escrow_id_commitment: BytesN::<32>::from_array(&env, &[1u8; 32]),
        blinded_public_key: BytesN::<32>::from_array(&env, &[2u8; 32]),
        blind_signature: BytesN::<64>::from_array(&env, &[0u8; 64]),
        issued_at: env.ledger().timestamp(),
    };

    let nullifier = BytesN::<32>::from_array(&env, &[99u8; 32]);
    let rating = 9_000u32; // 90%

    client.submit_blinded_review(&receipt, &entity, &rating, &nullifier);

    // Entity should now have avg_rating set
    let rep = client.get_reputation(&entity);
    assert_eq!(rep.avg_rating, rating);
}

#[test]
fn test_blinded_review_invalid_rating() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let entity = Address::generate(&env);
    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    let client = setup(&env, &admin, config);

    let receipt = BlindedReviewReceipt {
        escrow_id_commitment: BytesN::<32>::from_array(&env, &[1u8; 32]),
        blinded_public_key: BytesN::<32>::from_array(&env, &[2u8; 32]),
        blind_signature: BytesN::<64>::from_array(&env, &[0u8; 64]),
        issued_at: env.ledger().timestamp(),
    };

    let nullifier = BytesN::<32>::from_array(&env, &[99u8; 32]);
    let invalid_rating = 15_000u32; // > 10_000 basis points

    let result = client.try_submit_blinded_review(&receipt, &entity, &invalid_rating, &nullifier);
    assert_eq!(result, Err(Ok(ReputationError::InvalidRating)));
}

#[test]
fn test_blinded_review_on_frozen_entity_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let entity = Address::generate(&env);
    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    let client = setup(&env, &admin, config);

    // Freeze the entity
    client.freeze_entity(&admin, &entity);

    let receipt = BlindedReviewReceipt {
        escrow_id_commitment: BytesN::<32>::from_array(&env, &[1u8; 32]),
        blinded_public_key: BytesN::<32>::from_array(&env, &[2u8; 32]),
        blind_signature: BytesN::<64>::from_array(&env, &[0u8; 64]),
        issued_at: env.ledger().timestamp(),
    };

    let nullifier = BytesN::<32>::from_array(&env, &[99u8; 32]);
    let rating = 8_000u32;

    let result = client.try_submit_blinded_review(&receipt, &entity, &rating, &nullifier);
    assert_eq!(result, Err(Ok(ReputationError::EntityFrozen)));
}
