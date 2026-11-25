//! Tests for arbiter panels, quorum voting, and deadline reassignment (#774).

use crate::{
    arbitration::{ArbiterKey, ARBITER_DEADLINE_LEDGERS},
    DisputeAssignment, ReputationConfig, ReputationContract, ReputationContractClient,
    ReputationError,
};
use soroban_sdk::{
    symbol_short,
    testutils::{storage::Persistent, Address as _, Events, Ledger},
    Address, Env, Symbol, TryIntoVal, Vec,
};

fn default_config() -> ReputationConfig {
    ReputationConfig {
        decay_window_seconds: 90 * 24 * 60 * 60,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 500,
        freeze_threshold_flags: 3,
    }
}

struct Setup<'a> {
    client: ReputationContractClient<'a>,
    admin: Address,
    caller: Address,
    arbiters: Vec<Address>,
}

fn setup(env: &Env) -> Setup<'_> {
    env.mock_all_auths();
    let admin = Address::generate(env);
    let contract_id = env.register(ReputationContract, (admin.clone(), default_config()));
    let client = ReputationContractClient::new(env, &contract_id);
    let mut arbiters = Vec::new(env);
    for _ in 0..5 {
        arbiters.push_back(Address::generate(env));
    }
    Setup {
        client,
        admin,
        caller: Address::generate(env),
        arbiters,
    }
}

fn emitted(env: &Env, s: &Setup<'_>, name: Symbol) -> bool {
    env.events().all().iter().any(|(contract, topics, _)| {
        if contract != s.client.address || topics.len() < 2 {
            return false;
        }
        let namespace: Option<Symbol> = topics.get(0).and_then(|v| v.try_into_val(env).ok());
        let action: Option<Symbol> = topics.get(1).and_then(|v| v.try_into_val(env).ok());
        namespace == Some(symbol_short!("reput")) && action == Some(name.clone())
    })
}

#[test]
fn panel_initialization_and_quorum_voting_finalize() {
    let env = Env::default();
    let s = setup(&env);
    let dispute_id = 1u64;

    s.client.initialize_arbitration_panel(
        &s.admin,
        &dispute_id,
        &s.arbiters,
        &s.arbiters.get(0).unwrap(),
    );

    assert!(emitted(&env, &s, symbol_short!("arb_init")));

    let tally = s.client.get_arbitration_tally(&dispute_id);
    assert_eq!(tally.dispute_id, dispute_id);
    assert_eq!(tally.total_voters, 0);
    assert!(!tally.finalized);

    for i in 0..3 {
        let tally = s.client.cast_arbitration_vote(
            &s.arbiters.get(i).unwrap(),
            &dispute_id,
            &crate::VoteOutcome::ForBuyer,
        );
        assert_eq!(tally.total_voters, i + 1);
        if i == 2 {
            assert!(emitted(&env, &s, symbol_short!("arb_vote")));
            assert!(emitted(&env, &s, symbol_short!("arb_final")));
        }
        assert!(s
            .client
            .has_arbitration_voted(&dispute_id, &s.arbiters.get(i).unwrap()));
    }

    let tally = s.client.get_arbitration_tally(&dispute_id);
    assert_eq!(tally.votes_for_buyer, 3);
    assert_eq!(tally.votes_for_merchant, 0);
    assert!(tally.finalized);
}

#[test]
fn invalid_panels_are_rejected() {
    let env = Env::default();
    let s = setup(&env);
    let dispute_id = 7u64;

    let mut short = Vec::new(&env);
    for i in 0..4 {
        short.push_back(s.arbiters.get(i).unwrap());
    }
    assert_eq!(
        s.client.try_initialize_arbitration_panel(
            &s.admin,
            &dispute_id,
            &short,
            &s.arbiters.get(0).unwrap()
        ),
        Err(Ok(ReputationError::InvalidParam))
    );

    let mut duplicated = s.arbiters.clone();
    duplicated.set(1, s.arbiters.get(0).unwrap());
    assert_eq!(
        s.client.try_initialize_arbitration_panel(
            &s.admin,
            &dispute_id,
            &duplicated,
            &s.arbiters.get(0).unwrap()
        ),
        Err(Ok(ReputationError::InvalidParam))
    );

    let outsider = Address::generate(&env);
    assert_eq!(
        s.client
            .try_initialize_arbitration_panel(&s.admin, &dispute_id, &s.arbiters, &outsider),
        Err(Ok(ReputationError::InvalidParam))
    );

    s.client.initialize_arbitration_panel(
        &s.admin,
        &dispute_id,
        &s.arbiters,
        &s.arbiters.get(0).unwrap(),
    );
    assert_eq!(
        s.client.try_initialize_arbitration_panel(
            &s.admin,
            &dispute_id,
            &s.arbiters,
            &s.arbiters.get(0).unwrap()
        ),
        Err(Ok(ReputationError::AlreadyInitialized))
    );
}

#[test]
fn non_members_and_double_votes_are_rejected() {
    let env = Env::default();
    let s = setup(&env);
    let dispute_id = 9u64;
    s.client.initialize_arbitration_panel(
        &s.admin,
        &dispute_id,
        &s.arbiters,
        &s.arbiters.get(0).unwrap(),
    );

    let outsider = Address::generate(&env);
    assert_eq!(
        s.client.try_cast_arbitration_vote(
            &outsider,
            &dispute_id,
            &crate::VoteOutcome::ForMerchant
        ),
        Err(Ok(ReputationError::Unauthorized))
    );

    s.client.cast_arbitration_vote(
        &s.arbiters.get(0).unwrap(),
        &dispute_id,
        &crate::VoteOutcome::ForMerchant,
    );
    assert_eq!(
        s.client.try_cast_arbitration_vote(
            &s.arbiters.get(0).unwrap(),
            &dispute_id,
            &crate::VoteOutcome::ForMerchant
        ),
        Err(Ok(ReputationError::AppealAlreadyVoted))
    );
}

#[test]
fn deadline_penalty_reassigns_to_next_eligible_arbiter() {
    let env = Env::default();
    let s = setup(&env);
    let dispute_id = 11u64;
    let start_ledger = env.ledger().sequence();
    s.client.initialize_arbitration_panel(
        &s.admin,
        &dispute_id,
        &s.arbiters,
        &s.arbiters.get(0).unwrap(),
    );

    assert!(!s.client.check_arbitration_deadline(&s.caller, &dispute_id));

    env.ledger()
        .set_sequence_number(start_ledger + ARBITER_DEADLINE_LEDGERS + 1);
    assert!(s.client.check_arbitration_deadline(&s.caller, &dispute_id));
    assert!(emitted(&env, &s, symbol_short!("arb_pen")));
    assert!(!s.client.check_arbitration_deadline(&s.caller, &dispute_id));

    let assignment: DisputeAssignment = env.as_contract(&s.client.address, || {
        env.storage()
            .persistent()
            .get(&ArbiterKey::Assignment(dispute_id))
            .unwrap()
    });
    assert_eq!(assignment.assigned_arbiter, s.arbiters.get(1).unwrap());
    assert_eq!(
        assignment.assigned_ledger,
        start_ledger + ARBITER_DEADLINE_LEDGERS + 1
    );

    let strikes: u32 = env.as_contract(&s.client.address, || {
        env.storage()
            .persistent()
            .get(&ArbiterKey::PenaltyCount(s.arbiters.get(0).unwrap()))
            .unwrap()
    });
    assert_eq!(strikes, 1);
}

#[test]
fn finalized_dispute_cannot_be_penalized() {
    let env = Env::default();
    let s = setup(&env);
    let dispute_id = 13u64;
    s.client.initialize_arbitration_panel(
        &s.admin,
        &dispute_id,
        &s.arbiters,
        &s.arbiters.get(0).unwrap(),
    );
    for i in 0..3 {
        s.client.cast_arbitration_vote(
            &s.arbiters.get(i).unwrap(),
            &dispute_id,
            &crate::VoteOutcome::ForBuyer,
        );
    }
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + ARBITER_DEADLINE_LEDGERS + 1);
    assert_eq!(
        s.client
            .try_check_arbitration_deadline(&s.caller, &dispute_id),
        Err(Ok(ReputationError::AppealAlreadyResolved))
    );
}

#[test]
fn assignment_and_tally_keys_do_not_collide() {
    let env = Env::default();
    let s = setup(&env);
    let dispute_id = 17u64;
    s.client.initialize_arbitration_panel(
        &s.admin,
        &dispute_id,
        &s.arbiters,
        &s.arbiters.get(2).unwrap(),
    );

    env.as_contract(&s.client.address, || {
        let assignment: Option<DisputeAssignment> = env
            .storage()
            .persistent()
            .get(&ArbiterKey::Assignment(dispute_id));
        let tally: Option<crate::ArbiterTally> = env
            .storage()
            .persistent()
            .get(&ArbiterKey::Tally(dispute_id));
        assert_eq!(
            assignment.unwrap().assigned_arbiter,
            s.arbiters.get(2).unwrap()
        );
        assert_eq!(tally.unwrap().dispute_id, dispute_id);
    });
}

#[test]
fn tally_read_extends_persistent_ttl() {
    let env = Env::default();
    let s = setup(&env);
    let dispute_id = 19u64;
    s.client.initialize_arbitration_panel(
        &s.admin,
        &dispute_id,
        &s.arbiters,
        &s.arbiters.get(0).unwrap(),
    );

    env.as_contract(&s.client.address, || {
        env.storage().instance().extend_ttl(1_000_000, 1_000_000);
    });
    let initial_ttl = env.as_contract(&s.client.address, || {
        env.storage()
            .persistent()
            .get_ttl(&ArbiterKey::Tally(dispute_id))
    });
    let jump = initial_ttl - crate::PERSISTENT_BUMP_THRESHOLD + 1;
    env.ledger().with_mut(|li| li.sequence_number += jump);

    let before = env.as_contract(&s.client.address, || {
        env.storage()
            .persistent()
            .get_ttl(&ArbiterKey::Tally(dispute_id))
    });
    let tally = s.client.get_arbitration_tally(&dispute_id);
    let after = env.as_contract(&s.client.address, || {
        env.storage()
            .persistent()
            .get_ttl(&ArbiterKey::Tally(dispute_id))
    });
    assert_eq!(tally.dispute_id, dispute_id);
    assert!(after > before);
    assert!(after >= crate::PERSISTENT_BUMP_AMOUNT);
}
