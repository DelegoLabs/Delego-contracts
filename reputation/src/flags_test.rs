#![cfg(test)]

use crate::*;
use soroban_sdk::{testutils::Address as TestAddress, BytesN, Env};

fn setup_env() -> (Env, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = TestAddress::random(&env);
    let merchant = TestAddress::random(&env);
    let reviewer = TestAddress::random(&env);
    let reporter = TestAddress::random(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 1,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    ReputationContract::__constructor(&env, admin.clone(), config).unwrap();

    (env, admin, merchant, reviewer, reporter)
}

#[test]
fn test_flag_review_success() {
    let (env, _admin, merchant, reviewer, reporter) = setup_env();

    let review_id = 100u64;
    let content_hash = BytesN::<32>::from_array(&env, [9u8; 32]);
    ReputationContract::record_review(
        env.clone(),
        reviewer,
        merchant,
        review_id,
        4,
        content_hash,
    )
    .expect("Should record review");

    let bond = 100_000_000i128; // 10 USDC
    let flagged = ReputationContract::flag_review(env.clone(), reporter.clone(), review_id, bond)
        .expect("Should flag review");
    assert!(flagged);

    // Retrieve flag record
    let flag = ReputationContract::get_review_flag(env.clone(), 1).expect("Flag should exist");
    assert_eq!(flag.flag_id, 1);
    assert_eq!(flag.reporter, reporter);
    assert_eq!(flag.review_id, review_id);
    assert_eq!(flag.bond_amount, bond);
    assert_eq!(flag.status, ReviewFlagStatus::Pending);
    assert_eq!(flag.resolved_at, None);
    assert_eq!(flag.arbiter, None);

    // Review should still exist while pending
    assert!(!ReputationContract::is_review_removed(env, review_id));
}

#[test]
fn test_flag_review_insufficient_bond() {
    let (env, _admin, merchant, reviewer, reporter) = setup_env();

    let review_id = 101u64;
    let content_hash = BytesN::<32>::from_array(&env, [9u8; 32]);
    ReputationContract::record_review(
        env.clone(),
        reviewer,
        merchant,
        review_id,
        5,
        content_hash,
    )
    .expect("Should record review");

    let insufficient_bond = 50_000_000i128; // Less than MIN_FLAG_BOND
    let result = ReputationContract::flag_review(env, reporter, review_id, insufficient_bond);
    assert!(matches!(result, Err(ReputationError::InvalidFraudBond)));
}

#[test]
fn test_flag_review_nonexistent() {
    let (env, _admin, _merchant, _reviewer, reporter) = setup_env();

    let nonexistent_review_id = 9999u64;
    let bond = 100_000_000i128;
    let result = ReputationContract::flag_review(env, reporter, nonexistent_review_id, bond);
    assert!(matches!(result, Err(ReputationError::ReviewNotFound)));
}

#[test]
fn test_flag_review_already_flagged_by_same_reporter() {
    let (env, _admin, merchant, reviewer, reporter) = setup_env();

    let review_id = 102u64;
    let content_hash = BytesN::<32>::from_array(&env, [9u8; 32]);
    ReputationContract::record_review(
        env.clone(),
        reviewer,
        merchant,
        review_id,
        3,
        content_hash,
    )
    .expect("Should record review");

    let bond = 100_000_000i128;
    ReputationContract::flag_review(env.clone(), reporter.clone(), review_id, bond)
        .expect("Should flag first time");

    // Second flag by same reporter while pending must fail
    let result = ReputationContract::flag_review(env, reporter, review_id, bond);
    assert!(matches!(result, Err(ReputationError::AlreadyFlagged)));
}

#[test]
fn test_resolve_flag_upheld_removes_review_and_rewards_reporter() {
    let (env, admin, merchant, reviewer, reporter) = setup_env();

    let review_id = 103u64;
    let content_hash = BytesN::<32>::from_array(&env, [9u8; 32]);
    ReputationContract::record_review(
        env.clone(),
        reviewer,
        merchant,
        review_id,
        1,
        content_hash,
    )
    .expect("Should record review");

    let bond = 100_000_000i128;
    ReputationContract::flag_review(env.clone(), reporter.clone(), review_id, bond)
        .expect("Should flag review");

    // Arbiter resolves flag as upheld (true)
    let resolved = ReputationContract::resolve_flag(env.clone(), admin.clone(), 1, true)
        .expect("Should resolve flag");
    assert!(resolved);

    // Verify flag status is Upheld
    let flag = ReputationContract::get_review_flag(env.clone(), 1).unwrap();
    assert_eq!(flag.status, ReviewFlagStatus::Upheld);
    assert_eq!(flag.arbiter, Some(admin));
    assert!(flag.resolved_at.is_some());
    let expected_bounty = (bond * FLAG_BOUNTY_BPS) / (BPS_SCALE as i128); // 10% = 10_000_000
    assert_eq!(flag.bounty_amount, expected_bounty);

    // Verify fraudulent review was removed
    assert!(ReputationContract::is_review_removed(env.clone(), review_id));

    // Verify reporter was rewarded (bond refunded + bounty)
    let reward = ReputationContract::get_reporter_reward(env.clone(), reporter);
    assert_eq!(reward, bond + expected_bounty);

    // Cannot re-flag a removed review
    let other_reporter = TestAddress::random(&env);
    let reflag_result = ReputationContract::flag_review(env, other_reporter, review_id, bond);
    assert!(matches!(reflag_result, Err(ReputationError::ReviewNotFound)));
}

#[test]
fn test_resolve_flag_frivolous_slashes_bond_and_keeps_review() {
    let (env, admin, merchant, reviewer, reporter) = setup_env();

    let review_id = 104u64;
    let content_hash = BytesN::<32>::from_array(&env, [9u8; 32]);
    ReputationContract::record_review(
        env.clone(),
        reviewer,
        merchant,
        review_id,
        5,
        content_hash,
    )
    .expect("Should record review");

    let bond = 100_000_000i128;
    ReputationContract::flag_review(env.clone(), reporter.clone(), review_id, bond)
        .expect("Should flag review");

    // Arbiter resolves flag as frivolous/rejected (false)
    let resolved = ReputationContract::resolve_flag(env.clone(), admin.clone(), 1, false)
        .expect("Should resolve flag");
    assert!(resolved);

    // Verify flag status is Frivolous
    let flag = ReputationContract::get_review_flag(env.clone(), 1).unwrap();
    assert_eq!(flag.status, ReviewFlagStatus::Frivolous);
    assert_eq!(flag.bounty_amount, 0);

    // Review must NOT be removed
    assert!(!ReputationContract::is_review_removed(env.clone(), review_id));

    // Reporter received 0 reward
    let reward = ReputationContract::get_reporter_reward(env.clone(), reporter);
    assert_eq!(reward, 0);

    // Slashed bond pool increased by the full bond
    let slashed = ReputationContract::get_total_slashed_bonds(env);
    assert_eq!(slashed, bond);
}

#[test]
fn test_resolve_flag_unauthorized() {
    let (env, _admin, merchant, reviewer, reporter) = setup_env();

    let review_id = 105u64;
    let content_hash = BytesN::<32>::from_array(&env, [9u8; 32]);
    ReputationContract::record_review(
        env.clone(),
        reviewer,
        merchant,
        review_id,
        5,
        content_hash,
    )
    .expect("Should record review");

    let bond = 100_000_000i128;
    ReputationContract::flag_review(env.clone(), reporter, review_id, bond).unwrap();

    let unauthorized_caller = TestAddress::random(&env);
    let result = ReputationContract::resolve_flag(env, unauthorized_caller, 1, true);
    assert!(matches!(result, Err(ReputationError::Unauthorized)));
}

#[test]
fn test_resolve_flag_already_resolved() {
    let (env, admin, merchant, reviewer, reporter) = setup_env();

    let review_id = 106u64;
    let content_hash = BytesN::<32>::from_array(&env, [9u8; 32]);
    ReputationContract::record_review(
        env.clone(),
        reviewer,
        merchant,
        review_id,
        5,
        content_hash,
    )
    .expect("Should record review");

    let bond = 100_000_000i128;
    ReputationContract::flag_review(env.clone(), reporter, review_id, bond).unwrap();

    ReputationContract::resolve_flag(env.clone(), admin.clone(), 1, true).unwrap();

    // Second resolution must fail
    let result = ReputationContract::resolve_flag(env, admin, 1, false);
    assert!(matches!(result, Err(ReputationError::FlagAlreadyResolved)));
}

#[test]
fn test_resolve_flag_nonexistent() {
    let (env, admin, _merchant, _reviewer, _reporter) = setup_env();

    let result = ReputationContract::resolve_flag(env, admin, 9999, true);
    assert!(matches!(result, Err(ReputationError::FlagNotFound)));
}

#[test]
fn test_flag_reporting_via_trait() {
    let (env, admin, merchant, reviewer, reporter) = setup_env();

    let review_id = 107u64;
    let content_hash = BytesN::<32>::from_array(&env, [9u8; 32]);
    ReputationContract::record_review(
        env.clone(),
        reviewer,
        merchant,
        review_id,
        2,
        content_hash,
    )
    .expect("Should record review");

    let bond = 100_000_000i128;
    // Call via trait implementation
    let flagged = <ReputationContract as FlagReportingTrait>::flag_review(
        env.clone(),
        reporter,
        review_id,
        bond,
    )
    .expect("Should flag via trait");
    assert!(flagged);

    let resolved = <ReputationContract as FlagReportingTrait>::resolve_flag(
        env,
        admin,
        1,
        true,
    )
    .expect("Should resolve via trait");
    assert!(resolved);
}
