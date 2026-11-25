#![cfg(test)]

use crate::*;
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

fn setup_env() -> (
    Env,
    ReputationContractClient<'static>,
    Address,
    Address,
    Address,
    Address,
) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let merchant = Address::generate(&env);
    let reviewer = Address::generate(&env);
    let reporter = Address::generate(&env);

    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 1,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };

    let contract_id = env.register(ReputationContract, (admin.clone(), config));
    let client = ReputationContractClient::new(&env, &contract_id);

    (env, client, admin, merchant, reviewer, reporter)
}

fn record_review(
    env: &Env,
    client: &ReputationContractClient<'_>,
    reviewer: &Address,
    merchant: &Address,
    review_id: u64,
    rating: u32,
) {
    let content_hash = BytesN::<32>::from_array(env, &[9u8; 32]);
    client.record_review(
        reviewer,
        merchant,
        &review_id,
        &rating,
        &content_hash,
        &false,
    )
}

#[test]
fn test_flag_review_success() {
    let (env, client, _admin, merchant, reviewer, reporter) = setup_env();

    let review_id = 100u64;
    record_review(&env, &client, &reviewer, &merchant, review_id, 4);

    let bond = 100_000_000i128; // 10 USDC
    let flagged = client.flag_review(&reporter, &review_id, &bond);
    assert!(flagged);

    // Retrieve flag record
    let flag = client.get_review_flag(&1).expect("Flag should exist");
    assert_eq!(flag.flag_id, 1);
    assert_eq!(flag.reporter, reporter);
    assert_eq!(flag.review_id, review_id);
    assert_eq!(flag.bond_amount, bond);
    assert_eq!(flag.status, ReviewFlagStatus::Pending);
    assert_eq!(flag.resolved_at, None);
    assert_eq!(flag.arbiter, None);

    // Review should still exist while pending
    assert!(!client.is_review_removed(&review_id));
}

#[test]
fn test_flag_review_insufficient_bond() {
    let (env, client, _admin, merchant, reviewer, reporter) = setup_env();

    let review_id = 101u64;
    record_review(&env, &client, &reviewer, &merchant, review_id, 5);

    let insufficient_bond = 50_000_000i128; // Less than MIN_FLAG_BOND
    let result = client.try_flag_review(&reporter, &review_id, &insufficient_bond);
    assert_eq!(result, Err(Ok(ReputationError::InvalidFraudBond)));
}

#[test]
fn test_flag_review_nonexistent() {
    let (_env, client, _admin, _merchant, _reviewer, reporter) = setup_env();

    let nonexistent_review_id = 9999u64;
    let bond = 100_000_000i128;
    let result = client.try_flag_review(&reporter, &nonexistent_review_id, &bond);
    assert_eq!(result, Err(Ok(ReputationError::ReviewNotFound)));
}

#[test]
fn test_flag_review_already_flagged_by_same_reporter() {
    let (env, client, _admin, merchant, reviewer, reporter) = setup_env();

    let review_id = 102u64;
    record_review(&env, &client, &reviewer, &merchant, review_id, 3);

    let bond = 100_000_000i128;
    client.flag_review(&reporter, &review_id, &bond);

    // Second flag by same reporter while pending must fail
    let result = client.try_flag_review(&reporter, &review_id, &bond);
    assert_eq!(result, Err(Ok(ReputationError::AlreadyFlagged)));
}

#[test]
fn test_resolve_review_flag_upheld_removes_review_and_rewards_reporter() {
    let (env, client, admin, merchant, reviewer, reporter) = setup_env();

    let review_id = 103u64;
    record_review(&env, &client, &reviewer, &merchant, review_id, 1);

    let bond = 100_000_000i128;
    client.flag_review(&reporter, &review_id, &bond);

    // Arbiter resolves flag as upheld (true)
    let resolved = client.resolve_review_flag(&admin, &1, &true);
    assert!(resolved);

    // Verify flag status is Upheld
    let flag = client.get_review_flag(&1).unwrap();
    assert_eq!(flag.status, ReviewFlagStatus::Upheld);
    assert_eq!(flag.arbiter, Some(admin));
    assert!(flag.resolved_at.is_some());
    let expected_bounty = (bond * FLAG_BOUNTY_BPS) / (BPS_SCALE as i128); // 10% = 10_000_000
    assert_eq!(flag.bounty_amount, expected_bounty);

    // Verify fraudulent review was removed
    assert!(client.is_review_removed(&review_id));

    // Verify reporter was rewarded (bond refunded + bounty)
    let reward = client.get_reporter_reward(&reporter);
    assert_eq!(reward, bond + expected_bounty);

    // Cannot re-flag a removed review
    let other_reporter = Address::generate(&env);
    let reflag_result = client.try_flag_review(&other_reporter, &review_id, &bond);
    assert_eq!(reflag_result, Err(Ok(ReputationError::ReviewNotFound)));
}

#[test]
fn test_resolve_review_flag_frivolous_slashes_bond_and_keeps_review() {
    let (env, client, admin, merchant, reviewer, reporter) = setup_env();

    let review_id = 104u64;
    record_review(&env, &client, &reviewer, &merchant, review_id, 5);

    let bond = 100_000_000i128;
    client.flag_review(&reporter, &review_id, &bond);

    // Arbiter resolves flag as frivolous/rejected (false)
    let resolved = client.resolve_review_flag(&admin, &1, &false);
    assert!(resolved);

    // Verify flag status is Frivolous
    let flag = client.get_review_flag(&1).unwrap();
    assert_eq!(flag.status, ReviewFlagStatus::Frivolous);
    assert_eq!(flag.bounty_amount, 0);

    // Review must NOT be removed
    assert!(!client.is_review_removed(&review_id));

    // Reporter received 0 reward
    let reward = client.get_reporter_reward(&reporter);
    assert_eq!(reward, 0);

    // Slashed bond pool increased by the full bond
    let slashed = client.get_total_slashed_bonds();
    assert_eq!(slashed, bond);
}

#[test]
fn test_resolve_review_flag_unauthorized() {
    let (env, client, _admin, merchant, reviewer, reporter) = setup_env();

    let review_id = 105u64;
    record_review(&env, &client, &reviewer, &merchant, review_id, 5);

    let bond = 100_000_000i128;
    client.flag_review(&reporter, &review_id, &bond);

    let unauthorized_caller = Address::generate(&env);
    let result = client.try_resolve_review_flag(&unauthorized_caller, &1, &true);
    assert_eq!(result, Err(Ok(ReputationError::Unauthorized)));
}

#[test]
fn test_resolve_review_flag_already_resolved() {
    let (env, client, admin, merchant, reviewer, reporter) = setup_env();

    let review_id = 106u64;
    record_review(&env, &client, &reviewer, &merchant, review_id, 5);

    let bond = 100_000_000i128;
    client.flag_review(&reporter, &review_id, &bond);

    client.resolve_review_flag(&admin, &1, &true);

    // Second resolution must fail
    let result = client.try_resolve_review_flag(&admin, &1, &false);
    assert_eq!(result, Err(Ok(ReputationError::FlagAlreadyResolved)));
}

#[test]
fn test_resolve_review_flag_nonexistent() {
    let (_env, client, admin, _merchant, _reviewer, _reporter) = setup_env();

    let result = client.try_resolve_review_flag(&admin, &9999, &true);
    assert_eq!(result, Err(Ok(ReputationError::FlagNotFound)));
}

#[test]
fn test_flag_reporting_via_trait() {
    let (env, client, admin, merchant, reviewer, reporter) = setup_env();

    let review_id = 107u64;
    record_review(&env, &client, &reviewer, &merchant, review_id, 2);

    let bond = 100_000_000i128;
    let flagged = client.flag_review(&reporter, &review_id, &bond);
    assert!(flagged);

    let resolved = client.resolve_review_flag(&admin, &1, &true);
    assert!(resolved);
}
