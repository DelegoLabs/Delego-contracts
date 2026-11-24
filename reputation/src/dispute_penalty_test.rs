use crate::{ReputationConfig, ReputationContract, ReputationContractClient, ReputationError};
use soroban_sdk::{testutils::Address as _, Address, Env};

struct Setup {
    env: Env,
    client: ReputationContractClient<'static>,
    admin: Address,
    merchant: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let merchant = Address::generate(&env);

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
        admin,
        merchant,
    }
}

#[test]
fn authorized_admin_records_lost_dispute() {
    let s = setup();

    s.client.record_lost_dispute(&s.admin, &s.merchant, &1u64);

    let penalty = s.client.get_dispute_penalty(&s.merchant);
    assert_eq!(penalty.consecutive_lost_disputes, 1);
    assert_eq!(penalty.active_penalty_multiplier, 2);
}

#[test]
fn replaying_dispute_id_is_rejected_with_typed_error() {
    let s = setup();

    s.client.record_lost_dispute(&s.admin, &s.merchant, &7u64);

    assert_eq!(
        s.client
            .try_record_lost_dispute(&s.admin, &s.merchant, &7u64),
        Err(Ok(ReputationError::DuplicateLostDispute))
    );
}

#[test]
fn duplicate_dispute_does_not_mutate_penalty_state() {
    let s = setup();

    s.client.record_lost_dispute(&s.admin, &s.merchant, &11u64);
    let before = s.client.get_dispute_penalty(&s.merchant);

    assert_eq!(
        s.client
            .try_record_lost_dispute(&s.admin, &s.merchant, &11u64),
        Err(Ok(ReputationError::DuplicateLostDispute))
    );

    let after = s.client.get_dispute_penalty(&s.merchant);
    assert_eq!(
        after.consecutive_lost_disputes,
        before.consecutive_lost_disputes
    );
    assert_eq!(
        after.active_penalty_multiplier,
        before.active_penalty_multiplier
    );
    assert_eq!(after.last_dispute_ledger, before.last_dispute_ledger);
}

#[test]
fn distinct_disputes_advance_exponential_penalty() {
    let s = setup();

    let expected = [(1u64, 2u32), (2, 4), (3, 8), (4, 16), (5, 16)];
    for (dispute_id, multiplier) in expected {
        s.client
            .record_lost_dispute(&s.admin, &s.merchant, &dispute_id);
        let penalty = s.client.get_dispute_penalty(&s.merchant);
        assert_eq!(penalty.consecutive_lost_disputes, dispute_id as u32);
        assert_eq!(penalty.active_penalty_multiplier, multiplier);
    }
}

#[test]
fn unauthorized_caller_cannot_record_lost_dispute() {
    let s = setup();
    let intruder = Address::generate(&s.env);

    assert_eq!(
        s.client
            .try_record_lost_dispute(&intruder, &s.merchant, &1u64),
        Err(Ok(ReputationError::Unauthorized))
    );

    assert_eq!(
        s.client.try_get_dispute_penalty(&s.merchant),
        Err(Ok(ReputationError::EntityNotFound))
    );
}

#[test]
fn fraud_authority_can_record_lost_dispute() {
    let s = setup();
    let authority = Address::generate(&s.env);

    s.client.set_fraud_authority(&s.admin, &authority);
    s.client
        .record_lost_dispute(&authority, &s.merchant, &42u64);

    let penalty = s.client.get_dispute_penalty(&s.merchant);
    assert_eq!(penalty.consecutive_lost_disputes, 1);
    assert_eq!(penalty.active_penalty_multiplier, 2);
}

#[test]
fn fraud_authority_is_still_subject_to_replay_protection() {
    let s = setup();
    let authority = Address::generate(&s.env);

    s.client.set_fraud_authority(&s.admin, &authority);
    s.client
        .record_lost_dispute(&authority, &s.merchant, &99u64);

    assert_eq!(
        s.client
            .try_record_lost_dispute(&authority, &s.merchant, &99u64),
        Err(Ok(ReputationError::DuplicateLostDispute))
    );
}
