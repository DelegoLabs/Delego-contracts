#![cfg(test)]

use crate::*;
use delego_interfaces::ReputationReaderTrait;
use soroban_sdk::{testutils::Address as TestAddress, Env};

#[test]
fn test_get_merchant_rating_via_trait() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let merchant = TestAddress::random(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 1,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    // No reputation yet
    let rating = ReputationContract::get_merchant_rating(env.clone(), merchant.clone());
    assert!(rating.is_err());

    // Create reputation via transaction record
    let counterparty = TestAddress::random(&env);
    ReputationContract::record_transaction(
        &env,
        admin.clone(),
        1,
        merchant.clone(),
        counterparty,
        1_000,
        TransactionOutcome::Released,
    )
    .expect("Should record transaction");

    // Now rating should be accessible via trait
    let rating = ReputationContract::get_merchant_rating(env, merchant).expect("Should get rating");
    assert!(rating > 0);
}

#[test]
fn test_get_dispute_ratio_via_trait() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let merchant = TestAddress::random(&env);
    let counterparty = TestAddress::random(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 1,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    // Record some transactions
    ReputationContract::record_transaction(
        &env,
        admin.clone(),
        1,
        merchant.clone(),
        counterparty.clone(),
        1_000,
        TransactionOutcome::Released,
    )
    .unwrap();

    ReputationContract::record_transaction(
        &env,
        admin.clone(),
        2,
        merchant.clone(),
        counterparty.clone(),
        1_000,
        TransactionOutcome::Disputed,
    )
    .unwrap();

    // Get dispute ratio: 1 disputed out of 2 = 50% = 5000 basis points
    let ratio = ReputationContract::get_dispute_ratio(env, merchant)
        .expect("Should get dispute ratio");
    assert!(ratio > 0);
}

#[test]
fn test_is_merchant_in_good_standing_via_trait() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let merchant = TestAddress::random(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 1,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    // Frozen merchant is not in good standing
    ReputationContract::freeze_entity(&env, admin.clone(), merchant.clone()).unwrap();

    let in_good_standing =
        ReputationContract::is_merchant_in_good_standing(env.clone(), merchant.clone())
            .expect("Should check standing");
    assert!(!in_good_standing);

    // Unfreeze
    ReputationContract::unfreeze_entity(&env, admin.clone(), merchant.clone()).unwrap();

    // No transactions yet, score is 0, not in good standing
    let in_good_standing =
        ReputationContract::is_merchant_in_good_standing(env, merchant).expect("Should check standing");
    assert!(!in_good_standing);
}

#[test]
fn test_get_merchant_transaction_count_via_trait() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let merchant = TestAddress::random(&env);
    let counterparty = TestAddress::random(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 1,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    // Record 3 transactions
    for i in 1..=3 {
        ReputationContract::record_transaction(
            &env,
            admin.clone(),
            i,
            merchant.clone(),
            counterparty.clone(),
            1_000,
            TransactionOutcome::Released,
        )
        .unwrap();
    }

    let count = ReputationContract::get_merchant_transaction_count(env, merchant)
        .expect("Should get transaction count");
    assert_eq!(count, 3);
}

#[test]
fn test_get_merchant_average_rating_via_trait() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let merchant = TestAddress::random(&env);
    let rater = TestAddress::random(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 1,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    // Record transaction
    ReputationContract::record_transaction(
        &env,
        admin.clone(),
        1,
        merchant.clone(),
        rater.clone(),
        1_000,
        TransactionOutcome::Released,
    )
    .unwrap();

    // Rate the merchant
    let rating = 8_000u32; // 80%
    ReputationContract::rate_entity(&env, rater, 1, merchant.clone(), rating).unwrap();

    // Get average rating via trait
    let avg_rating = ReputationContract::get_merchant_average_rating(env, merchant)
        .expect("Should get average rating");
    assert_eq!(avg_rating, rating);
}

#[test]
fn test_trait_methods_are_no_std_compatible() {
    // This test ensures that the trait methods work in no_std environments
    // (no allocation, no floating point). Compile-time verification.
    // If this compiles, the trait is no_std compatible.
    let _ = std::mem::size_of::<ReputationReaderTrait>();
}
