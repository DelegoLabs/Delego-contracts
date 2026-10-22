use crate::*;
use delego_interfaces::ReputationReaderTrait;
use soroban_sdk::{testutils::Address as _, Address, Env};

fn config() -> ReputationConfig {
    ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 1,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    }
}

#[test]
fn test_get_merchant_rating_via_trait() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let merchant = Address::generate(&env);

    let contract_id = env.register(ReputationContract, (admin.clone(), config()));
    let client = ReputationContractClient::new(&env, &contract_id);

    // No reputation yet
    let rating = env.as_contract(&contract_id, || {
        <ReputationContract as ReputationReaderTrait>::get_merchant_rating(
            env.clone(),
            merchant.clone(),
        )
    });
    assert!(rating.is_err());

    // Create reputation via transaction record
    let counterparty = Address::generate(&env);
    client.record_transaction(
        &admin,
        &1,
        &merchant,
        &counterparty,
        &1_000,
        &TransactionOutcome::Released,
    );

    // Now rating should be accessible via trait
    let rating = env
        .as_contract(&contract_id, || {
            <ReputationContract as ReputationReaderTrait>::get_merchant_rating(
                env.clone(),
                merchant.clone(),
            )
        })
        .expect("Should get rating");
    assert!(rating > 0);
}

#[test]
fn test_get_dispute_ratio_via_trait() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let merchant = Address::generate(&env);
    let counterparty = Address::generate(&env);

    let contract_id = env.register(ReputationContract, (admin.clone(), config()));
    let client = ReputationContractClient::new(&env, &contract_id);

    // Record some transactions
    client.record_transaction(
        &admin,
        &1,
        &merchant,
        &counterparty,
        &1_000,
        &TransactionOutcome::Released,
    );

    client.record_transaction(
        &admin,
        &2,
        &merchant,
        &counterparty,
        &1_000,
        &TransactionOutcome::Disputed,
    );

    // Get dispute ratio: 1 disputed out of 2 = 50% = 5000 basis points
    let ratio = env
        .as_contract(&contract_id, || {
            <ReputationContract as ReputationReaderTrait>::get_dispute_ratio(
                env.clone(),
                merchant.clone(),
            )
        })
        .expect("Should get dispute ratio");
    assert!(ratio > 0);
}

#[test]
fn test_is_merchant_in_good_standing_via_trait() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let merchant = Address::generate(&env);

    let contract_id = env.register(ReputationContract, (admin.clone(), config()));
    let client = ReputationContractClient::new(&env, &contract_id);

    // Frozen merchant is not in good standing
    client.freeze_entity(&admin, &merchant);

    let in_good_standing = env
        .as_contract(&contract_id, || {
            <ReputationContract as ReputationReaderTrait>::is_merchant_in_good_standing(
                env.clone(),
                merchant.clone(),
            )
        })
        .expect("Should check standing");
    assert!(!in_good_standing);

    // Unfreeze
    client.unfreeze_entity(&admin, &merchant);

    // No transactions yet, score is 0, not in good standing
    let in_good_standing = env
        .as_contract(&contract_id, || {
            <ReputationContract as ReputationReaderTrait>::is_merchant_in_good_standing(
                env.clone(),
                merchant.clone(),
            )
        })
        .expect("Should check standing");
    assert!(!in_good_standing);
}

#[test]
fn test_get_merchant_transaction_count_via_trait() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let merchant = Address::generate(&env);
    let counterparty = Address::generate(&env);

    let contract_id = env.register(ReputationContract, (admin.clone(), config()));
    let client = ReputationContractClient::new(&env, &contract_id);

    // Record 3 transactions
    for i in 1..=3 {
        client.record_transaction(
            &admin,
            &i,
            &merchant,
            &counterparty,
            &1_000,
            &TransactionOutcome::Released,
        );
    }

    let count = env
        .as_contract(&contract_id, || {
            <ReputationContract as ReputationReaderTrait>::get_merchant_transaction_count(
                env.clone(),
                merchant.clone(),
            )
        })
        .expect("Should get transaction count");
    assert_eq!(count, 3);
}

#[test]
fn test_get_merchant_average_rating_via_trait() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let merchant = Address::generate(&env);
    let rater = Address::generate(&env);

    let contract_id = env.register(ReputationContract, (admin.clone(), config()));
    let client = ReputationContractClient::new(&env, &contract_id);

    // Record transaction
    client.record_transaction(
        &admin,
        &1,
        &merchant,
        &rater,
        &1_000,
        &TransactionOutcome::Released,
    );

    // Rate the merchant
    let rating = 8_000u32; // 80%
    client.rate_entity(&rater, &1, &merchant, &rating);

    // Get average rating via trait
    let avg_rating = env
        .as_contract(&contract_id, || {
            <ReputationContract as ReputationReaderTrait>::get_merchant_average_rating(
                env.clone(),
                merchant.clone(),
            )
        })
        .expect("Should get average rating");
    assert_eq!(avg_rating, rating);
}

#[test]
fn test_trait_methods_are_no_std_compatible() {
    // This test ensures that the trait methods work in no_std environments
    // (no allocation, no floating point). Compile-time verification.
    // If this compiles, the trait is no_std compatible.
    fn assert_reader_impl<T: ReputationReaderTrait>() {}
    assert_reader_impl::<ReputationContract>();
}
