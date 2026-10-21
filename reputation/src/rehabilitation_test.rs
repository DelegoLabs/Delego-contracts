#![cfg(test)]

use crate::*;
use soroban_sdk::{testutils::Address as TestAddress, Env, Symbol};

#[test]
fn test_enter_rehabilitation_success() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let merchant = TestAddress::random(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    // Freeze the merchant first
    ReputationContract::freeze_entity(&env, admin.clone(), merchant.clone()).unwrap();

    // Enter rehabilitation with collateral
    let collateral = 500_000_000i128; // 50 USDC in stroops
    let program = ReputationContract::enter_rehabilitation(&env, merchant.clone(), collateral)
        .expect("Should enter rehabilitation");

    assert_eq!(program.merchant, merchant);
    assert_eq!(program.collateral_staked_stroops, collateral);
    assert_eq!(program.probation_orders_completed, 0);
    assert_eq!(program.probation_orders_required, 10);
    assert_eq!(program.status, symbol_short!("active"));
}

#[test]
fn test_enter_rehabilitation_not_eligible() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let merchant = TestAddress::random(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    // Merchant is not frozen and has no penalties, should not be eligible
    let result = ReputationContract::enter_rehabilitation(&env, merchant.clone(), 500_000_000);
    assert!(matches!(
        result,
        Err(ReputationError::NotEligibleForRehabilitation)
    ));
}

#[test]
fn test_complete_probation_orders() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let merchant = TestAddress::random(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    // Freeze and enter rehabilitation
    ReputationContract::freeze_entity(&env, admin.clone(), merchant.clone()).unwrap();
    let collateral = 500_000_000i128;
    ReputationContract::enter_rehabilitation(&env, merchant.clone(), collateral).unwrap();

    // Complete probation orders
    for i in 0..5 {
        let program = ReputationContract::complete_probation_order(&env, merchant.clone())
            .expect(&format!("Should complete order {}", i + 1));
        assert_eq!(program.probation_orders_completed, (i + 1) as u32);
    }

    // Check reputation improved
    let rep = ReputationContract::get_reputation(&env, merchant.clone())
        .expect("Should get reputation");
    assert!(rep.score > 0, "Score should improve with completed orders");
}

#[test]
fn test_graduate_rehabilitation() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let merchant = TestAddress::random(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    // Setup and enter rehabilitation
    ReputationContract::freeze_entity(&env, admin.clone(), merchant.clone()).unwrap();
    let collateral = 500_000_000i128;
    ReputationContract::enter_rehabilitation(&env, merchant.clone(), collateral).unwrap();

    // Complete all required orders
    for _ in 0..10 {
        ReputationContract::complete_probation_order(&env, merchant.clone()).unwrap();
    }

    // Graduate
    let returned_collateral = ReputationContract::graduate_rehabilitation(&env, merchant.clone())
        .expect("Should graduate");
    assert_eq!(returned_collateral, collateral);

    // Verify merchant is unfrozen
    let is_frozen = ReputationContract::is_frozen(env, merchant);
    assert!(!is_frozen, "Merchant should be unfrozen after graduation");
}
