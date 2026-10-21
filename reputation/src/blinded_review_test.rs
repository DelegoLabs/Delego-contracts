#![cfg(test)]

use crate::*;
use soroban_sdk::{testutils::Address as TestAddress, BytesN, Env};

#[test]
fn test_issue_blinded_review_token() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    let escrow_id = 12345u64;
    let blinded_key = BytesN::<32>::from_array(&env, [42u8; 32]);

    let receipt = ReputationContract::issue_blinded_review_token(
        &env,
        escrow_id,
        blinded_key.clone(),
    )
    .expect("Should issue blinded token");

    assert_eq!(receipt.blinded_public_key, blinded_key);
    // Verify escrow_id is committed in the receipt
    assert_eq!(receipt.escrow_id_commitment.get(0), 57); // escrow_id.to_le_bytes()[0]
}

#[test]
fn test_submit_blinded_review_prevents_double_submission() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let entity = TestAddress::random(&env);
    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    let receipt = BlindedReviewReceipt {
        escrow_id_commitment: BytesN::<32>::from_array(&env, [1u8; 32]),
        blinded_public_key: BytesN::<32>::from_array(&env, [2u8; 32]),
        blind_signature: BytesN::<64>::from_array(&env, [0u8; 64]),
        issued_at: env.ledger().timestamp(),
    };

    let nullifier = BytesN::<32>::from_array(&env, [99u8; 32]);
    let rating = 8_000u32; // 80%

    // First submission succeeds
    ReputationContract::submit_blinded_review(&env, receipt.clone(), entity.clone(), rating, nullifier.clone())
        .expect("First review should succeed");

    // Second submission with same nullifier should fail
    let result = ReputationContract::submit_blinded_review(&env, receipt, entity, rating, nullifier);
    assert!(matches!(
        result,
        Err(ReputationError::BlindedReceiptAlreadyUsed)
    ));
}

#[test]
fn test_submit_blinded_review_updates_entity_rating() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let entity = TestAddress::random(&env);
    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    let receipt = BlindedReviewReceipt {
        escrow_id_commitment: BytesN::<32>::from_array(&env, [1u8; 32]),
        blinded_public_key: BytesN::<32>::from_array(&env, [2u8; 32]),
        blind_signature: BytesN::<64>::from_array(&env, [0u8; 64]),
        issued_at: env.ledger().timestamp(),
    };

    let nullifier = BytesN::<32>::from_array(&env, [99u8; 32]);
    let rating = 9_000u32; // 90%

    ReputationContract::submit_blinded_review(&env, receipt, entity.clone(), rating, nullifier)
        .expect("Should submit blinded review");

    // Entity should now have avg_rating set
    let rep = ReputationContract::get_reputation(&env, entity)
        .expect("Should get reputation");
    assert_eq!(rep.avg_rating, rating);
}

#[test]
fn test_blinded_review_invalid_rating() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let entity = TestAddress::random(&env);
    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    let receipt = BlindedReviewReceipt {
        escrow_id_commitment: BytesN::<32>::from_array(&env, [1u8; 32]),
        blinded_public_key: BytesN::<32>::from_array(&env, [2u8; 32]),
        blind_signature: BytesN::<64>::from_array(&env, [0u8; 64]),
        issued_at: env.ledger().timestamp(),
    };

    let nullifier = BytesN::<32>::from_array(&env, [99u8; 32]);
    let invalid_rating = 15_000u32; // > 10_000 basis points

    let result = ReputationContract::submit_blinded_review(&env, receipt, entity, invalid_rating, nullifier);
    assert!(matches!(result, Err(ReputationError::InvalidRating)));
}

#[test]
fn test_blinded_review_on_frozen_entity_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let entity = TestAddress::random(&env);
    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    // Freeze the entity
    ReputationContract::freeze_entity(&env, admin.clone(), entity.clone()).unwrap();

    let receipt = BlindedReviewReceipt {
        escrow_id_commitment: BytesN::<32>::from_array(&env, [1u8; 32]),
        blinded_public_key: BytesN::<32>::from_array(&env, [2u8; 32]),
        blind_signature: BytesN::<64>::from_array(&env, [0u8; 64]),
        issued_at: env.ledger().timestamp(),
    };

    let nullifier = BytesN::<32>::from_array(&env, [99u8; 32]);
    let rating = 8_000u32;

    let result = ReputationContract::submit_blinded_review(&env, receipt, entity, rating, nullifier);
    assert!(matches!(result, Err(ReputationError::EntityFrozen)));
}
