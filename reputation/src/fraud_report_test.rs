#![cfg(test)]

use crate::*;
use soroban_sdk::{testutils::Address as TestAddress, BytesN, Env};

#[test]
fn test_file_fraud_report_success() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let reporter = TestAddress::random(&env);
    let target = TestAddress::random(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    let bond = 500_000_000i128; // 50 USDC minimum
    let evidence_hash = BytesN::<32>::from_array(&env, [7u8; 32]);

    let report_id = ReputationContract::file_fraud_report(
        &env,
        reporter.clone(),
        target.clone(),
        evidence_hash.clone(),
        bond,
    )
    .expect("Should file fraud report");

    assert_eq!(report_id, 0); // First report gets id 0

    // Retrieve the report
    let report = ReputationContract::get_fraud_report(&env, report_id)
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

    let admin = TestAddress::random(&env);
    let reporter = TestAddress::random(&env);
    let target = TestAddress::random(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    let insufficient_bond = 100_000_000i128; // Less than 50 USDC
    let evidence_hash = BytesN::<32>::from_array(&env, [7u8; 32]);

    let result = ReputationContract::file_fraud_report(
        &env,
        reporter,
        target,
        evidence_hash,
        insufficient_bond,
    );

    assert!(matches!(result, Err(ReputationError::InvalidFraudBond)));
}

#[test]
fn test_resolve_fraud_report_valid() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let reporter = TestAddress::random(&env);
    let target = TestAddress::random(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    // File report
    let bond = 500_000_000i128;
    let evidence_hash = BytesN::<32>::from_array(&env, [7u8; 32]);
    let report_id = ReputationContract::file_fraud_report(
        &env,
        reporter.clone(),
        target.clone(),
        evidence_hash,
        bond,
    )
    .expect("Should file report");

    // Resolve as valid
    let resolved = ReputationContract::resolve_fraud_report(&env, admin.clone(), report_id, true)
        .expect("Should resolve report");

    assert_eq!(resolved.status, symbol_short!("valid"));
    assert!(resolved.resolved_at.is_some());
}

#[test]
fn test_resolve_fraud_report_invalid() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let reporter = TestAddress::random(&env);
    let target = TestAddress::random(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    // File report
    let bond = 500_000_000i128;
    let evidence_hash = BytesN::<32>::from_array(&env, [7u8; 32]);
    let report_id = ReputationContract::file_fraud_report(
        &env,
        reporter,
        target,
        evidence_hash,
        bond,
    )
    .expect("Should file report");

    // Resolve as invalid (frivolous report)
    let resolved = ReputationContract::resolve_fraud_report(&env, admin.clone(), report_id, false)
        .expect("Should resolve report");

    assert_eq!(resolved.status, symbol_short!("invld"));
    assert!(resolved.resolved_at.is_some());
    // Note: Actual bond slashing would be handled by governance/treasury contract
}

#[test]
fn test_get_fraud_reports_by_target() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let reporter1 = TestAddress::random(&env);
    let reporter2 = TestAddress::random(&env);
    let target = TestAddress::random(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    let bond = 500_000_000i128;

    // File multiple reports against same target
    let _id1 = ReputationContract::file_fraud_report(
        &env,
        reporter1,
        target.clone(),
        BytesN::<32>::from_array(&env, [1u8; 32]),
        bond,
    )
    .expect("Should file first report");

    let _id2 = ReputationContract::file_fraud_report(
        &env,
        reporter2,
        target.clone(),
        BytesN::<32>::from_array(&env, [2u8; 32]),
        bond,
    )
    .expect("Should file second report");

    // Retrieve all reports for target
    let reports = ReputationContract::get_fraud_reports_by_target(&env, target, 0, 10)
        .expect("Should get reports");

    assert_eq!(reports.len(), 2);
}

#[test]
fn test_fraud_report_unique_ids() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let reporter = TestAddress::random(&env);
    let target1 = TestAddress::random(&env);
    let target2 = TestAddress::random(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    let bond = 500_000_000i128;

    let id1 = ReputationContract::file_fraud_report(
        &env,
        reporter.clone(),
        target1,
        BytesN::<32>::from_array(&env, [1u8; 32]),
        bond,
    )
    .expect("Should file first report");

    let id2 = ReputationContract::file_fraud_report(
        &env,
        reporter,
        target2,
        BytesN::<32>::from_array(&env, [2u8; 32]),
        bond,
    )
    .expect("Should file second report");

    // Each report should have unique ID
    assert_ne!(id1, id2);
    assert_eq!(id1, 0);
    assert_eq!(id2, 1);
}
