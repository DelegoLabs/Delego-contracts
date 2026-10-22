#![cfg(test)]

use crate::*;
use soroban_sdk::{testutils::Address as _, Address, Env};

fn setup(
    env: &Env,
    admin: &Address,
    config: ReputationConfig,
) -> ReputationContractClient<'static> {
    let contract_id = env.register(ReputationContract, (admin.clone(), config));
    ReputationContractClient::new(env, &contract_id)
}

#[test]
fn test_enter_rehabilitation_success() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let merchant = Address::generate(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    let client = setup(&env, &admin, config);

    // Freeze the merchant first
    client.freeze_entity(&admin, &merchant);

    // Enter rehabilitation with collateral
    let collateral = 500_000_000i128; // 50 USDC in stroops
    let program = client.enter_rehabilitation(&merchant, &collateral);

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

    let admin = Address::generate(&env);
    let merchant = Address::generate(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    let client = setup(&env, &admin, config);

    // Merchant is not frozen and has no penalties, should not be eligible
    let result = client.try_enter_rehabilitation(&merchant, &500_000_000);
    assert_eq!(
        result,
        Err(Ok(ReputationError::NotEligibleForRehabilitation))
    );
}

#[test]
fn test_complete_probation_orders() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let merchant = Address::generate(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    let client = setup(&env, &admin, config);

    // Freeze and enter rehabilitation
    client.freeze_entity(&admin, &merchant);
    let collateral = 500_000_000i128;
    client.enter_rehabilitation(&merchant, &collateral);

    // Complete probation orders
    for i in 0..5 {
        let program = client.complete_probation_order(&merchant);
        assert_eq!(program.probation_orders_completed, (i + 1) as u32);
    }

    // Check reputation improved. The public score stays masked until the
    // entity passes the transaction threshold, so the restored score is read
    // from the persisted reputation record.
    let stored = env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .get::<_, ReputationScore>(&DataKey::Reputation(merchant.clone()))
    });
    let stored = stored.expect("Should have a reputation record");
    assert!(
        stored.score > 0,
        "Score should improve with completed orders"
    );
}

#[test]
fn test_graduate_rehabilitation() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let merchant = Address::generate(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    let client = setup(&env, &admin, config);

    // Setup and enter rehabilitation
    client.freeze_entity(&admin, &merchant);
    let collateral = 500_000_000i128;
    client.enter_rehabilitation(&merchant, &collateral);

    // Complete all required orders
    for _ in 0..10 {
        client.complete_probation_order(&merchant);
    }

    // Graduate
    let returned_collateral = client.graduate_rehabilitation(&merchant);
    assert_eq!(returned_collateral, collateral);

    // Verify merchant is unfrozen
    let is_frozen = client.is_frozen(&merchant);
    assert!(!is_frozen, "Merchant should be unfrozen after graduation");
}
