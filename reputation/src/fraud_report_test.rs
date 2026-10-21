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
fn test_file_fraud_report_success() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let reporter = Address::generate(&env);
    let target = Address::generate(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    let client = setup(&env, &admin, config);

    let bond = 500_000_000i128; // 50 USDC minimum
    let evidence_hash = BytesN::<32>::from_array(&env, &[7u8; 32]);

    let report_id = client.file_fraud_report(&reporter, &target, &evidence_hash, &bond);

    assert_eq!(report_id, 0); // First report gets id 0

    // Retrieve the report
    let report = client
        .get_fraud_report(&report_id)
        .expect("Should retrieve fraud report");

    assert_eq!(report.report_id, report_id);
    assert_eq!(report.reporter, reporter);
    assert_eq!(report.target_entity, target);
    assert_eq!(report.evidence_hash, evidence_hash);
    assert_eq!(report.staked_bond_stroops, bond);
    assert_eq!(report.status, symbol_short!("filed"));
}

#[test]
fn test_file_fraud_report_insufficient_bond() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let reporter = Address::generate(&env);
    let target = Address::generate(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    let client = setup(&env, &admin, config);

    let insufficient_bond = 100_000_000i128; // Less than 50 USDC
    let evidence_hash = BytesN::<32>::from_array(&env, &[7u8; 32]);

    let result =
        client.try_file_fraud_report(&reporter, &target, &evidence_hash, &insufficient_bond);

    assert_eq!(result, Err(Ok(ReputationError::InvalidFraudBond)));
}

#[test]
fn test_resolve_fraud_report_valid() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let reporter = Address::generate(&env);
    let target = Address::generate(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    let client = setup(&env, &admin, config);

    // File report
    let bond = 500_000_000i128;
    let evidence_hash = BytesN::<32>::from_array(&env, &[7u8; 32]);
    let report_id = client.file_fraud_report(&reporter, &target, &evidence_hash, &bond);

    // Resolve as valid
    let resolved = client.resolve_fraud_report(&admin, &report_id, &true);

    assert_eq!(resolved.status, symbol_short!("valid"));
    assert!(resolved.resolved_at.is_some());
}

#[test]
fn test_resolve_fraud_report_invalid() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let reporter = Address::generate(&env);
    let target = Address::generate(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    let client = setup(&env, &admin, config);

    // File report
    let bond = 500_000_000i128;
    let evidence_hash = BytesN::<32>::from_array(&env, &[7u8; 32]);
    let report_id = client.file_fraud_report(&reporter, &target, &evidence_hash, &bond);

    // Resolve as invalid (frivolous report)
    let resolved = client.resolve_fraud_report(&admin, &report_id, &false);

    assert_eq!(resolved.status, symbol_short!("invld"));
    assert!(resolved.resolved_at.is_some());
    // Note: Actual bond slashing would be handled by governance/treasury contract
}

#[test]
fn test_get_fraud_reports_by_target() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let reporter1 = Address::generate(&env);
    let reporter2 = Address::generate(&env);
    let target = Address::generate(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    let client = setup(&env, &admin, config);

    let bond = 500_000_000i128;

    // File multiple reports against same target
    let _id1 = client.file_fraud_report(
        &reporter1,
        &target,
        &BytesN::<32>::from_array(&env, &[1u8; 32]),
        &bond,
    );

    let _id2 = client.file_fraud_report(
        &reporter2,
        &target,
        &BytesN::<32>::from_array(&env, &[2u8; 32]),
        &bond,
    );

    // Retrieve all reports for target
    let reports = client.get_fraud_reports_by_target(&target, &0, &10);

    assert_eq!(reports.len(), 2);
}

#[test]
fn test_fraud_report_unique_ids() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let reporter = Address::generate(&env);
    let target1 = Address::generate(&env);
    let target2 = Address::generate(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    let client = setup(&env, &admin, config);

    let bond = 500_000_000i128;

    let id1 = client.file_fraud_report(
        &reporter,
        &target1,
        &BytesN::<32>::from_array(&env, &[1u8; 32]),
        &bond,
    );

    let id2 = client.file_fraud_report(
        &reporter,
        &target2,
        &BytesN::<32>::from_array(&env, &[2u8; 32]),
        &bond,
    );

    // Each report should have unique ID
    assert_ne!(id1, id2);
    assert_eq!(id1, 0);
    assert_eq!(id2, 1);
}
