#![cfg(test)]

use crate::{
    FlagReportingTrait, ReputationConfig, ReputationContract, ReputationContractClient,
    ReputationError, ReviewFlagStatus, BPS_SCALE, FLAG_BOUNTY_BPS,
};
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

struct Setup {
    env: Env,
    client: ReputationContractClient<'static>,
    contract_id: Address,
    admin: Address,
    merchant: Address,
    reviewer: Address,
    reporter: Address,
}

fn setup() -> Setup {
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

    Setup {
        env,
        client,
        contract_id,
        admin,
        merchant,
        reviewer,
        reporter,
    }
}

fn record_review(s: &Setup, review_id: u64, rating: u32) {
    let content_hash = BytesN::<32>::from_array(&s.env, &[9u8; 32]);
    s.client.record_review(
        &s.reviewer,
        &s.merchant,
        &review_id,
        &rating,
        &content_hash,
        &false,
    );
}

#[test]
fn test_flag_review_success() {
    let s = setup();
    let review_id = 100u64;
    record_review(&s, review_id, 4);

    let bond = 100_000_000i128; // 10 USDC
    let flagged = s.client.flag_review(&s.reporter, &review_id, &bond);
    assert!(flagged);

    let flag = s.client.get_review_flag(&1).expect("Flag should exist");
    assert_eq!(flag.flag_id, 1);
    assert_eq!(flag.reporter, s.reporter);
    assert_eq!(flag.review_id, review_id);
    assert_eq!(flag.bond_amount, bond);
    assert_eq!(flag.status, ReviewFlagStatus::Pending);
    assert_eq!(flag.resolved_at, None);
    assert_eq!(flag.arbiter, None);

    assert!(!s.client.is_review_removed(&review_id));
}

#[test]
fn test_flag_review_insufficient_bond() {
    let s = setup();
    let review_id = 101u64;
    record_review(&s, review_id, 5);

    let insufficient_bond = 50_000_000i128;
    assert_eq!(
        s.client
            .try_flag_review(&s.reporter, &review_id, &insufficient_bond),
        Err(Ok(ReputationError::InvalidFraudBond))
    );
}

#[test]
fn test_flag_review_nonexistent() {
    let s = setup();

    assert_eq!(
        s.client
            .try_flag_review(&s.reporter, &9999u64, &100_000_000i128),
        Err(Ok(ReputationError::ReviewNotFound))
    );
}

#[test]
fn test_flag_review_already_flagged_by_same_reporter() {
    let s = setup();
    let review_id = 102u64;
    record_review(&s, review_id, 3);

    let bond = 100_000_000i128;
    s.client.flag_review(&s.reporter, &review_id, &bond);

    assert_eq!(
        s.client.try_flag_review(&s.reporter, &review_id, &bond),
        Err(Ok(ReputationError::AlreadyFlagged))
    );
}

#[test]
fn test_resolve_flag_upheld_removes_review_and_rewards_reporter() {
    let s = setup();
    let review_id = 103u64;
    record_review(&s, review_id, 1);

    let bond = 100_000_000i128;
    s.client.flag_review(&s.reporter, &review_id, &bond);

    let resolved = s.client.resolve_review_flag(&s.admin, &1, &true);
    assert!(resolved);

    let flag = s.client.get_review_flag(&1).unwrap();
    assert_eq!(flag.status, ReviewFlagStatus::Upheld);
    assert_eq!(flag.arbiter, Some(s.admin.clone()));
    assert!(flag.resolved_at.is_some());
    let expected_bounty = (bond * FLAG_BOUNTY_BPS) / BPS_SCALE;
    assert_eq!(flag.bounty_amount, expected_bounty);

    assert!(s.client.is_review_removed(&review_id));

    let reward = s.client.get_reporter_reward(&s.reporter);
    assert_eq!(reward, bond + expected_bounty);

    let other_reporter = Address::generate(&s.env);
    assert_eq!(
        s.client.try_flag_review(&other_reporter, &review_id, &bond),
        Err(Ok(ReputationError::ReviewNotFound))
    );
}

#[test]
fn test_resolve_flag_frivolous_slashes_bond_and_keeps_review() {
    let s = setup();
    let review_id = 104u64;
    record_review(&s, review_id, 5);

    let bond = 100_000_000i128;
    s.client.flag_review(&s.reporter, &review_id, &bond);

    let resolved = s.client.resolve_review_flag(&s.admin, &1, &false);
    assert!(resolved);

    let flag = s.client.get_review_flag(&1).unwrap();
    assert_eq!(flag.status, ReviewFlagStatus::Frivolous);
    assert_eq!(flag.bounty_amount, 0);

    assert!(!s.client.is_review_removed(&review_id));
    assert_eq!(s.client.get_reporter_reward(&s.reporter), 0);
    assert_eq!(s.client.get_total_slashed_bonds(), bond);
}

#[test]
fn test_resolve_flag_unauthorized() {
    let s = setup();
    let review_id = 105u64;
    record_review(&s, review_id, 5);

    let bond = 100_000_000i128;
    s.client.flag_review(&s.reporter, &review_id, &bond);

    let unauthorized_caller = Address::generate(&s.env);
    assert_eq!(
        s.client
            .try_resolve_review_flag(&unauthorized_caller, &1, &true),
        Err(Ok(ReputationError::Unauthorized))
    );
}

#[test]
fn test_resolve_flag_already_resolved() {
    let s = setup();
    let review_id = 106u64;
    record_review(&s, review_id, 5);

    let bond = 100_000_000i128;
    s.client.flag_review(&s.reporter, &review_id, &bond);

    s.client.resolve_review_flag(&s.admin, &1, &true);

    assert_eq!(
        s.client.try_resolve_review_flag(&s.admin, &1, &false),
        Err(Ok(ReputationError::FlagAlreadyResolved))
    );
}

#[test]
fn test_resolve_flag_nonexistent() {
    let s = setup();

    assert_eq!(
        s.client.try_resolve_review_flag(&s.admin, &9999, &true),
        Err(Ok(ReputationError::FlagNotFound))
    );
}

#[test]
fn test_flag_reporting_via_trait() {
    let s = setup();
    let review_id = 107u64;
    record_review(&s, review_id, 2);

    let bond = 100_000_000i128;
    let env = s.env.clone();
    let reporter = s.reporter.clone();
    let flagged = env
        .as_contract(&s.contract_id, || {
            <ReputationContract as FlagReportingTrait>::flag_review(
                env.clone(),
                reporter,
                review_id,
                bond,
            )
        })
        .expect("Should flag via trait");
    assert!(flagged);

    let resolved = s.client.resolve_review_flag(&s.admin, &1, &true);
    assert!(resolved);
}
