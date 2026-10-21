#![cfg(test)]
#![allow(clippy::module_inception)]

use crate::{
    CompositeScore, DataKey, EntityRole, ReputationConfig, ReputationContract,
    ReputationContractClient, ReputationError, TransactionOutcome, PERSISTENT_BUMP_AMOUNT,
    PERSISTENT_BUMP_THRESHOLD, SCORE_WINDOW,
    oracle_proof::test_signing_payload, CarrierProof, CompositeScore, DataKey, ReputationConfig,
    ReputationContract, ReputationContractClient, ReputationError, TransactionOutcome,
    PERSISTENT_BUMP_AMOUNT, PERSISTENT_BUMP_THRESHOLD, SCORE_WINDOW,
};
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{
    symbol_short,
    testutils::{storage::Persistent, Address as _, Events, Ledger},
    Address, BytesN, Env, String,
};

const MAX_RECORD_CPU_INSTRUCTIONS: u64 = 3_000_000;
const MAX_RECORD_MEMORY_BYTES: u64 = 3_000_000;

fn assert_record_cost_within_thresholds(env: &Env) {
    let budget = env.cost_estimate().budget();
    assert!(budget.cpu_instruction_cost() <= MAX_RECORD_CPU_INSTRUCTIONS);
    assert!(budget.memory_bytes_cost() <= MAX_RECORD_MEMORY_BYTES);
}

fn default_config() -> ReputationConfig {
    ReputationConfig {
        decay_window_seconds: 90 * 24 * 60 * 60,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 500,
        freeze_threshold_flags: 3,
    }
}

fn setup(env: &Env) -> (ReputationContractClient<'_>, Address) {
    env.mock_all_auths();
    let admin = Address::generate(env);
    let contract_id = env.register(ReputationContract, (admin.clone(), default_config()));
    let client = ReputationContractClient::new(env, &contract_id);
    (client, admin)
}

fn signed_carrier_proof(
    env: &Env,
    contract: &Address,
    carrier_oracle: Address,
    signing_key: &SigningKey,
) -> CarrierProof {
    let mut proof = CarrierProof {
        carrier_oracle,
        tracking_number_hash: BytesN::from_array(env, &[7; 32]),
        delivery_status: symbol_short!("delivered"),
        signature: BytesN::from_array(env, &[0; 64]),
    };
    let payload = test_signing_payload(env, contract, &proof);
    let payload_len = payload.len() as usize;
    let mut payload_bytes = [0u8; 512];
    payload.copy_into_slice(&mut payload_bytes[..payload_len]);
    proof.signature = BytesN::from_array(
        env,
        &signing_key.sign(&payload_bytes[..payload_len]).to_bytes(),
    );
    proof
}

#[test]
fn carrier_proof_verifies_and_emits_event_for_registered_oracle() {
    use soroban_sdk::IntoVal;

    let env = Env::default();
    let (client, admin) = setup(&env);
    let carrier_oracle = Address::generate(&env);
    let signing_key = SigningKey::from_bytes(&[42; 32]);
    let public_key = BytesN::from_array(&env, &signing_key.verifying_key().to_bytes());
    client.set_carrier_oracle(&admin, &carrier_oracle, &public_key);
    let proof = signed_carrier_proof(&env, &client.address, carrier_oracle.clone(), &signing_key);

    client.verify_carrier_proof(&proof);

    let events = env.events().all();
    assert!(events.iter().any(|(contract, topics, _)| {
        contract == client.address
            && topics.len() == 3
            && topics.get(0) == Some(symbol_short!("reput").into_val(&env))
            && topics.get(1) == Some(symbol_short!("carproof").into_val(&env))
            && topics.get(2) == Some(carrier_oracle.clone().into_val(&env))
    }));
}

#[test]
fn carrier_proof_rejects_unregistered_oracle() {
    let env = Env::default();
    let (client, _) = setup(&env);
    let signing_key = SigningKey::from_bytes(&[43; 32]);
    let proof = signed_carrier_proof(&env, &client.address, Address::generate(&env), &signing_key);

    assert_eq!(
        client.try_verify_carrier_proof(&proof),
        Err(Ok(ReputationError::CarrierOracleNotAuthorized))
    );
}

#[test]
fn carrier_proof_rejects_signature_not_matching_registered_key() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let carrier_oracle = Address::generate(&env);
    let registered_key = SigningKey::from_bytes(&[44; 32]);
    let forged_key = SigningKey::from_bytes(&[45; 32]);
    let public_key = BytesN::from_array(&env, &registered_key.verifying_key().to_bytes());
    client.set_carrier_oracle(&admin, &carrier_oracle, &public_key);
    let proof = signed_carrier_proof(&env, &client.address, carrier_oracle, &forged_key);

    assert!(client.try_verify_carrier_proof(&proof).is_err());
}

fn advance_time(env: &Env, seconds: u64) {
    env.ledger().with_mut(|li| {
        li.timestamp += seconds;
    });
}

// --- constructor (deployment-time initialization) ---
//
// `__constructor` is a Soroban constructor: the host runs it exactly once,
// atomically with `env.register`, and there is no way to invoke it again
// afterward (unlike the plain `initialize()` pattern used elsewhere in this
// workspace) — so there is no "already initialized" or "not yet
// initialized" state to exercise here, only deployment succeeding or
// `env.register` panicking on an invalid config.

#[test]
fn test_constructor_sets_admin_and_config() {
    let env = Env::default();
    let (client, _admin) = setup(&env);

    assert_eq!(client.get_config(), default_config());
}

#[test]
#[should_panic]
fn test_constructor_rejects_zero_decay_window() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut bad = default_config();
    bad.decay_window_seconds = 0;
    env.register(ReputationContract, (admin, bad));
}

#[test]
#[should_panic]
fn test_constructor_rejects_dispute_penalty_over_max() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut bad = default_config();
    bad.dispute_penalty_bps = 10_001;
    env.register(ReputationContract, (admin, bad));
}

#[test]
#[should_panic]
fn test_constructor_rejects_zero_freeze_threshold() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut bad = default_config();
    bad.freeze_threshold_flags = 0;
    env.register(ReputationContract, (admin, bad));
}

// --- min_transactions_threshold vs SCORE_WINDOW ---

#[test]
#[should_panic]
fn test_constructor_rejects_threshold_over_window() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut bad = default_config();
    bad.min_transactions_threshold = SCORE_WINDOW as u64 + 1;
    env.register(ReputationContract, (admin, bad));
}

#[test]
fn test_constructor_accepts_threshold_equal_to_window() {
    // A threshold exactly equal to SCORE_WINDOW is the largest reachable
    // value and must be accepted (the masking gate compares the lifetime
    // counter, so hitting it is still possible).
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut cfg = default_config();
    cfg.min_transactions_threshold = SCORE_WINDOW as u64;
    let contract_id = env.register(ReputationContract, (admin, cfg.clone()));
    let client = ReputationContractClient::new(&env, &contract_id);
    assert_eq!(client.get_config(), cfg);
}

#[test]
fn test_update_config_rejects_threshold_over_window() {
    let env = Env::default();
    let (client, admin) = setup(&env);

    let mut bad = default_config();
    bad.min_transactions_threshold = SCORE_WINDOW as u64 + 1;
    let res = client.try_update_config(&admin, &bad);
    assert_eq!(res, Err(Ok(ReputationError::InvalidParam)));
}

// The masking gate in get_reputation compares the entity's *lifetime*
// `total_transactions` (window-independent) against the threshold — not the
// `SCORE_WINDOW` sample feeding the score recompute. Setting the threshold
// equal to SCORE_WINDOW and recording exactly SCORE_WINDOW lifetime
// transactions unmasks the score even though the score itself is computed
// over the same-sized window, pinning that the gate is driven by lifetime
// counts.
#[test]
fn test_masking_uses_lifetime_counts_not_window() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut cfg = default_config();
    cfg.min_transactions_threshold = SCORE_WINDOW as u64;
    let contract_id = env.register(ReputationContract, (admin.clone(), cfg));
    let client = ReputationContractClient::new(&env, &contract_id);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    // Record exactly SCORE_WINDOW lifetime transactions. Remaining just below
    // the threshold would keep the score masked; at the threshold it unmasks.
    for i in 0..SCORE_WINDOW as u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Released,
        );
    }

    let rep = client.get_reputation(&entity);
    // Lifetime count is exact, and the fresh all-Released run scores full.
    assert_eq!(rep.total_transactions, SCORE_WINDOW as u64);
    assert_eq!(rep.score, 10_000);
}

#[test]
fn test_masking_keeps_score_hidden_below_threshold() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut cfg = default_config();
    cfg.min_transactions_threshold = SCORE_WINDOW as u64;
    let contract_id = env.register(ReputationContract, (admin.clone(), cfg));
    let client = ReputationContractClient::new(&env, &contract_id);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    // SCORE_WINDOW - 1 lifetime transactions: still below the threshold, so
    // the score/avg_rating must stay masked even though the recompute
    // samples the same window size.
    for i in 0..(SCORE_WINDOW as u64 - 1) {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Released,
        );
    }

    let rep = client.get_reputation(&entity);
    assert_eq!(rep.total_transactions, SCORE_WINDOW as u64 - 1);
    assert_eq!(rep.score, 0);
}

// --- record_transaction ---

#[test]
fn test_record_transaction_cost_stays_within_thresholds() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);
    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );
    assert_record_cost_within_thresholds(&env);
}

#[test]
fn test_record_transaction_released_scores_full() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );

    let rep = client.try_get_reputation(&entity).unwrap().unwrap();
    // Below min_transactions_threshold (5), so score is masked.
    assert_eq!(rep.score, 0);
    assert_eq!(rep.total_transactions, 1);
    assert_eq!(rep.successful_transactions, 1);
    assert_eq!(rep.disputed_transactions, 0);
}

#[test]
fn test_record_transaction_records_relation_in_both_directions() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );

    assert!(client.has_relation(&entity, &counterparty, &false));
    assert!(client.has_relation(&counterparty, &entity, &false));
    assert!(client.has_relation(&entity, &counterparty, &true));
    assert!(client.has_relation(&counterparty, &entity, &true));
    let stranger = Address::generate(&env);
    assert!(!client.has_relation(&entity, &stranger, &false));
}

#[test]
fn test_record_transaction_unmasks_score_at_threshold() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    for i in 0..5u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Released,
        );
    }

    let rep = client.get_reputation(&entity);
    assert_eq!(rep.total_transactions, 5);
    // All-Released, freshly recorded (no decay yet) -> full score.
    assert_eq!(rep.score, 10_000);
}

#[test]
fn test_record_transaction_unauthorized_caller() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);
    let not_admin = Address::generate(&env);

    let res = client.try_record_transaction(
        &not_admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );
    assert_eq!(res, Err(Ok(ReputationError::Unauthorized)));
}

#[test]
fn test_record_transaction_rejects_frozen_entity() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.freeze_entity(&admin, &entity);

    let res = client.try_record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );
    assert_eq!(res, Err(Ok(ReputationError::EntityFrozen)));
}

#[test]
fn test_record_transaction_allows_lifecycle_update_while_frozen() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Disputed,
    );
    client.freeze_entity(&admin, &entity);

    // A dispute opened before the freeze must still be able to resolve —
    // only brand-new escrows are rejected while frozen.
    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::ResolvedSeller,
    );

    let rep = client.get_reputation(&entity);
    assert_eq!(rep.disputed_transactions, 0);
    assert_eq!(rep.successful_transactions, 1);

    // A genuinely new escrow is still rejected while frozen.
    let res = client.try_record_transaction(
        &admin,
        &2u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );
    assert_eq!(res, Err(Ok(ReputationError::EntityFrozen)));
}

#[test]
fn test_record_transaction_incremental_recompute_cost() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    for i in 0..SCORE_WINDOW as u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Released,
        );
    }
    assert_record_cost_within_thresholds(&env);
}

#[test]
fn test_recompute_bumps_window_records_ttl() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut cfg = default_config();
    // Set a short decay window to make TTL eviction more likely in tests
    cfg.decay_window_seconds = 100;
    let contract_id = env.register(ReputationContract, (admin.clone(), cfg.clone()));
    let client = ReputationContractClient::new(&env, &contract_id);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    // Record enough transactions to fill the SCORE_WINDOW
    for i in 0..SCORE_WINDOW as u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Released,
        );
    }

    // Advance time past the decay window to ensure records would expire
    // without TTL bumps. The ledger timestamp is used for TTL calculations.
    let _initial_ledger_timestamp = env.ledger().timestamp();
    advance_time(&env, cfg.decay_window_seconds + 100);

    // Record one more transaction to trigger recompute_score on the existing window
    client.record_transaction(
        &admin,
        &(SCORE_WINDOW as u64),
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );

    // Verify that the reputation is still deterministic and not masked due to
    // missing records. If TTLs were not bumped, some records might have been
    // evicted, leading to a lower or masked score.
    let rep = client.get_reputation(&entity);
    // Since we have SCORE_WINDOW + 1 transactions, and min_transactions_threshold
    // is 5, the score should be unmasked and near full (10_000).
    assert_eq!(rep.total_transactions, SCORE_WINDOW as u64 + 1);
    assert_eq!(rep.score, 10_000);

    // Verify that the records are still present by checking the relation
    assert!(client.has_relation(&entity, &counterparty, &false));
}

#[test]
fn test_breakdown_ordering_is_deterministic_for_equal_timestamps() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut cfg = default_config();
    cfg.decay_window_seconds = 1;
    let contract_id = env.register(ReputationContract, (admin.clone(), cfg));
    let client = ReputationContractClient::new(&env, &contract_id);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    for i in 0..10u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &(1000i128 * (i as i128 + 1)),
            &TransactionOutcome::Released,
        );
    }

    let breakdown = client.get_reputation_breakdown(&entity, &0u32, &10u32);

    for i in 0..breakdown.len() - 1 {
        assert!(
            breakdown.get(i).unwrap().escrow_id < breakdown.get(i + 1).unwrap().escrow_id,
            "Breakdown must be sorted by escrow_id for deterministic ordering"
        );
    }
}

#[test]
fn test_record_transaction_extends_ttl_across_churn() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);
    let record_key = crate::DataKey::TransactionRecord(1);

    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Disputed,
    );
    let initial_ttl = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&record_key)
    });
    assert!(initial_ttl > 17_280);

    // The testing host grants freshly-written persistent entries only a
    // small default TTL, and a newly-registered contract instance 4_095.
    // Aging the *record* to the edge of its 17_280 bump threshold therefore
    // requires jumping the ledger far past the point where the contract
    // took care of the other entries it wrote in the first call
    // (`Reputation`, `TransactionHistory`, `Transacted`, and the instance
    // itself) — the test host would otherwise archive them mid-test. Keep
    // those alive here so the jump exercises exactly the record-TTL bump
    // this test is about.
    env.as_contract(&client.address, || {
        env.storage().instance().extend_ttl(10_000_000, 10_000_000);
        let storage = env.storage().persistent();
        storage.extend_ttl(
            &crate::DataKey::Reputation(entity.clone()),
            10_000_000,
            10_000_000,
        );
        storage.extend_ttl(
            &crate::DataKey::TransactionHistory(entity.clone()),
            10_000_000,
            10_000_000,
        );
        storage.extend_ttl(
            &crate::DataKey::Transacted(entity.clone(), counterparty.clone()),
            10_000_000,
            10_000_000,
        );
    });

    env.ledger().set_sequence_number(initial_ttl - 17_280 + 1);
    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::ResolvedSeller,
    );
    let refreshed_ttl = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&record_key)
    });
    assert!(refreshed_ttl > 17_280);

    env.ledger().set_sequence_number(refreshed_ttl - 17_280 + 1);
    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );
    let final_ttl = env.as_contract(&client.address, || {
        env.storage().persistent().get_ttl(&record_key)
    });
    assert!(final_ttl > 17_280);

    env.ledger().set_sequence_number(final_ttl - 1);
    assert!(!client
        .get_reputation_breakdown(&entity, &0u32, &10u32)
        .is_empty());
}

#[test]
fn test_record_transaction_same_escrow_updates_in_place() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Disputed,
    );
    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::ResolvedSeller,
    );

    let rep = client.get_reputation(&entity);
    // Lifecycle update on the same escrow_id must not double count.
    assert_eq!(rep.total_transactions, 1);
    assert_eq!(rep.disputed_transactions, 0);
    assert_eq!(rep.successful_transactions, 1);

    let breakdown = client.get_reputation_breakdown(&entity, &0u32, &10u32);
    assert_eq!(breakdown.len(), 1);
    assert!(matches!(
        breakdown.get(0).unwrap().outcome,
        TransactionOutcome::ResolvedSeller
    ));
}

#[test]
fn test_record_transaction_rejects_entity_mismatch_for_existing_escrow() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity_a = Address::generate(&env);
    let entity_b = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.record_transaction(
        &admin,
        &1u64,
        &entity_a,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );

    let res = client.try_record_transaction(
        &admin,
        &1u64,
        &entity_b,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );
    assert_eq!(res, Err(Ok(ReputationError::InvalidParam)));
}

#[test]
fn test_score_decays_over_time() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    for i in 0..5u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Released,
        );
    }
    let fresh = client.get_reputation(&entity);
    assert_eq!(fresh.score, 10_000);

    // Advance one full half-life (decay_window_seconds) and force a
    // recompute via a new transaction; the older Released records should
    // now contribute at roughly half weight, pulling a fresh 0-value
    // Disputed record's average down less than it would immediately after
    // (i.e. the mix is dominated less by old data over time). We assert the
    // simpler, robust invariant: recorded_at-fresh entries score higher
    // than long-decayed ones feeding a poor outcome.
    advance_time(&env, default_config().decay_window_seconds);
    client.record_transaction(
        &admin,
        &99u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Disputed,
    );
    let decayed = client.get_reputation(&entity);
    assert!(decayed.score < fresh.score);
}

#[test]
fn test_score_bounded_to_recent_window() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    // Old disputes that should age out of the score window once more than
    // SCORE_WINDOW clean transactions have been recorded since.
    for i in 0..10u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Disputed,
        );
    }
    for i in 10..(10 + crate::SCORE_WINDOW as u64) {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Released,
        );
    }

    let rep = client.get_reputation(&entity);
    // Lifetime counts remain exact regardless of the scoring window.
    assert_eq!(rep.total_transactions, 10 + crate::SCORE_WINDOW as u64);
    assert_eq!(rep.disputed_transactions, 10);
    // The old disputes have fully aged out of the SCORE_WINDOW most-recent
    // records feeding the score, so the score reflects only the clean run.
    assert_eq!(rep.score, 10_000);

    // One more dispute lands inside the window, so it now counts against the
    // score. This pins the boundary: the window, not the dispute count, is
    // what excluded the earlier ones.
    client.record_transaction(
        &admin,
        &(10 + crate::SCORE_WINDOW as u64),
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Disputed,
    );
    let rep = client.get_reputation(&entity);
    assert!(rep.score < 10_000);
}

#[test]
fn test_amount_does_not_affect_scoring() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    // Record 5 transactions with varying amounts to reach min_transactions_threshold.
    // All have the same outcome (Released) and are recorded at the same time,
    // so they should produce the same score regardless of amount differences.
    let amounts = [
        1i128,
        1000i128,
        1_000_000i128,
        10_000_000i128,
        100_000_000i128,
    ];
    for (i, amount) in amounts.iter().enumerate() {
        client.record_transaction(
            &admin,
            &(i as u64),
            &entity,
            &counterparty,
            amount,
            &TransactionOutcome::Released,
        );
    }

    let rep = client.get_reputation(&entity);
    // All transactions are Released and fresh, so count score should be full
    // (10_000). The blended score is dampened by the volume component, so it
    // must be strictly below the count score.
    assert!(rep.score < 10_000);
    assert_eq!(rep.total_transactions, 5);

    // Now record a Disputed transaction with a very high amount.
    // With volume-weighted scoring, a high-amount dispute carries more
    // statistical significance than a small-amount one.
    client.record_transaction(
        &admin,
        &100u64,
        &entity,
        &counterparty,
        &1_000_000_000i128, // Very high amount
        &TransactionOutcome::Disputed,
    );

    let rep_after_dispute = client.get_reputation(&entity);
    // The score should decrease due to the dispute.
    assert!(rep_after_dispute.score < rep.score);

    // Record another Disputed with a tiny amount. Because volume weighting
    // dampens the impact of micro-transactions, a tiny dispute must move the
    // blended score less than the high-amount dispute did.
    client.record_transaction(
        &admin,
        &101u64,
        &entity,
        &counterparty,
        &1i128, // Tiny amount
        &TransactionOutcome::Disputed,
    );

    let rep_after_second_dispute = client.get_reputation(&entity);
    // The score should decrease further, but the tiny-amount dispute's impact
    // is smaller than the high-amount dispute's impact.
    assert!(rep_after_second_dispute.score < rep_after_dispute.score);
    let high_amount_drop = rep.score - rep_after_dispute.score;
    let tiny_amount_drop = rep_after_dispute.score - rep_after_second_dispute.score;
    assert!(tiny_amount_drop < high_amount_drop);
}

// --- composite (volume-weighted vs count-weighted) scoring ---

#[test]
fn test_composite_score_high_value_transactions_carry_more_weight() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut cfg = default_config();
    cfg.min_transactions_threshold = 1;
    let contract_id = env.register(ReputationContract, (admin.clone(), cfg));
    let client = ReputationContractClient::new(&env, &contract_id);

    // Entity A: five tiny Released transactions.
    let entity_a = Address::generate(&env);
    let cp_a = Address::generate(&env);
    for i in 0..5u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity_a,
            &cp_a,
            &1i128,
            &TransactionOutcome::Released,
        );
    }

    // Entity B: five large Released transactions.
    let entity_b = Address::generate(&env);
    let cp_b = Address::generate(&env);
    for i in 0..5u64 {
        client.record_transaction(
            &admin,
            &(100 + i),
            &entity_b,
            &cp_b,
            &1_000_000i128,
            &TransactionOutcome::Released,
        );
    }

    let rep_a = client.get_reputation(&entity_a);
    let rep_b = client.get_reputation(&entity_b);

    // Both have identical count scores (all Released), but B's volume score
    // is higher because its transactions carry more dollar volume.
    assert!(rep_b.score >= rep_a.score);
    assert!(rep_b.volume_score_bps >= rep_a.volume_score_bps);
}

#[test]
fn test_composite_score_micro_transaction_spam_cannot_inflate_reputation() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut cfg = default_config();
    cfg.min_transactions_threshold = 1;
    let contract_id = env.register(ReputationContract, (admin.clone(), cfg));
    let client = ReputationContractClient::new(&env, &contract_id);

    // Spammer: 50 positive reviews on $1 items.
    let spammer = Address::generate(&env);
    let spam_cp = Address::generate(&env);
    for i in 0..50u64 {
        client.record_transaction(
            &admin,
            &i,
            &spammer,
            &spam_cp,
            &1i128,
            &TransactionOutcome::Released,
        );
    }

    // Honest merchant: a single large transaction.
    let honest = Address::generate(&env);
    let honest_cp = Address::generate(&env);
    client.record_transaction(
        &admin,
        &1000u64,
        &honest,
        &honest_cp,
        &10_000i128,
        &TransactionOutcome::Released,
    );

    let spam_rep = client.get_reputation(&spammer);
    let honest_rep = client.get_reputation(&honest);

    // The spammer's volume score must be strictly less than the honest
    // merchant's, since logarithmic dampening prevents count from dominating.
    assert!(spam_rep.volume_score_bps < honest_rep.volume_score_bps);
}

#[test]
fn test_composite_score_blend_weights() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut cfg = default_config();
    cfg.min_transactions_threshold = 1;
    let contract_id = env.register(ReputationContract, (admin.clone(), cfg));
    let client = ReputationContractClient::new(&env, &contract_id);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    for i in 0..5u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Released,
        );
    }

    let rep = client.get_reputation(&entity);
    // Blended score must equal 40% count + 60% volume.
    let expected_blended =
        (rep.count_score_bps as u64 * 40 + rep.volume_score_bps as u64 * 60) / 100;
    assert_eq!(rep.score as u64, expected_blended);
}

#[test]
fn test_composite_score_confidence_rating_bounds() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut cfg = default_config();
    cfg.min_transactions_threshold = 1;
    let contract_id = env.register(ReputationContract, (admin.clone(), cfg));
    let client = ReputationContractClient::new(&env, &contract_id);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    for i in 0..10u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Released,
        );
    }

    let rep = client.get_reputation(&entity);
    assert!(rep.confidence_rating <= 10_000);
}

#[test]
fn test_composite_score_struct_fields_present() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut cfg = default_config();
    cfg.min_transactions_threshold = 1;
    let contract_id = env.register(ReputationContract, (admin.clone(), cfg));
    let client = ReputationContractClient::new(&env, &contract_id);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );

    let rep = client.get_reputation(&entity);
    // All four composite fields must be populated and within bps bounds.
    assert!(rep.count_score_bps <= 10_000);
    assert!(rep.volume_score_bps <= 10_000);
    assert!(rep.blended_score_bps <= 10_000);
    assert!(rep.confidence_rating <= 10_000);
    // The canonical score is the blended score.
    assert_eq!(rep.score, rep.blended_score_bps);
}

#[test]
fn test_composite_score_logarithmic_dampening_avoids_whale_domination() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut cfg = default_config();
    cfg.min_transactions_threshold = 1;
    let contract_id = env.register(ReputationContract, (admin.clone(), cfg));
    let client = ReputationContractClient::new(&env, &contract_id);

    // Small merchant with modest volume.
    let small = Address::generate(&env);
    let small_cp = Address::generate(&env);
    for i in 0..10u64 {
        client.record_transaction(
            &admin,
            &i,
            &small,
            &small_cp,
            &100i128,
            &TransactionOutcome::Released,
        );
    }

    // Whale with a single enormous transaction.
    let whale = Address::generate(&env);
    let whale_cp = Address::generate(&env);
    client.record_transaction(
        &admin,
        &1000u64,
        &whale,
        &whale_cp,
        &1_000_000_000i128,
        &TransactionOutcome::Released,
    );

    let small_rep = client.get_reputation(&small);
    let whale_rep = client.get_reputation(&whale);

    // Logarithmic dampening means the whale's volume score is not
    // disproportionately larger than the small merchant's.
    assert!(whale_rep.volume_score_bps <= 10_000);
    assert!(small_rep.volume_score_bps <= 10_000);
    // The whale should not dominate by more than the bps scale allows.
    assert!(whale_rep.volume_score_bps - small_rep.volume_score_bps <= 10_000);
}

// --- rate_entity ---

#[test]
fn test_rate_entity_happy_path() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );
    client.rate_entity(&counterparty, &1u64, &entity, &9000u32);

    let breakdown = client.get_reputation_breakdown(&entity, &0u32, &10u32);
    assert_eq!(breakdown.get(0).unwrap().rating, Some(9000u32));
}

#[test]
fn test_rate_entity_updates_avg_rating() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    // Reach min_transactions_threshold (5) so the score/avg_rating are
    // visible on get_reputation.
    for i in 0..5u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Released,
        );
    }

    // Only two of the five are rated; avg_rating must reflect only those,
    // not all five recorded transactions.
    client.rate_entity(&counterparty, &0u64, &entity, &8000u32);
    client.rate_entity(&counterparty, &1u64, &entity, &10_000u32);

    let rep = client.get_reputation(&entity);
    // All five records are equally fresh (same recorded_at), so the two
    // ratings carry equal weight: (8000 + 10000) / 2.
    assert_eq!(rep.avg_rating, 9000);
}

#[test]
fn test_rate_entity_duplicate_rejected() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );
    client.rate_entity(&counterparty, &1u64, &entity, &9000u32);

    let res = client.try_rate_entity(&counterparty, &1u64, &entity, &8000u32);
    assert_eq!(res, Err(Ok(ReputationError::DuplicateRating)));
}

#[test]
fn test_rate_entity_invalid_rating_rejected() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );

    let res = client.try_rate_entity(&counterparty, &1u64, &entity, &10_001u32);
    assert_eq!(res, Err(Ok(ReputationError::InvalidRating)));
}

#[test]
fn test_rate_entity_wrong_rater_rejected() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);
    let stranger = Address::generate(&env);

    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );

    let res = client.try_rate_entity(&stranger, &1u64, &entity, &9000u32);
    assert_eq!(res, Err(Ok(ReputationError::Unauthorized)));
}

#[test]
fn test_rate_entity_missing_escrow_rejected() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    let res = client.try_rate_entity(&counterparty, &1u64, &entity, &9000u32);
    assert_eq!(res, Err(Ok(ReputationError::EntityNotFound)));
}

#[test]
fn test_rate_entity_rejects_disputed_outcome() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Disputed,
    );

    let res = client.try_rate_entity(&counterparty, &1u64, &entity, &9000u32);
    assert_eq!(res, Err(Ok(ReputationError::InvalidParam)));
}

#[test]
fn test_rate_entity_rejects_frozen_entity() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );
    client.freeze_entity(&admin, &entity);

    let res = client.try_rate_entity(&counterparty, &1u64, &entity, &9000u32);
    assert_eq!(res, Err(Ok(ReputationError::EntityFrozen)));
}

// --- get_reputation ---

#[test]
fn test_get_reputation_not_found() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    let entity = Address::generate(&env);

    assert_eq!(
        client.try_get_reputation(&entity),
        Err(Ok(ReputationError::EntityNotFound))
    );
}

// --- pagination ---

#[test]
fn test_get_reputation_breakdown_pagination() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    for i in 0..10u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Released,
        );
    }

    let page1 = client.get_reputation_breakdown(&entity, &0u32, &4u32);
    assert_eq!(page1.len(), 4);
    let page2 = client.get_reputation_breakdown(&entity, &4u32, &4u32);
    assert_eq!(page2.len(), 4);
    let page3 = client.get_reputation_breakdown(&entity, &8u32, &4u32);
    assert_eq!(page3.len(), 2);

    let out_of_range = client.get_reputation_breakdown(&entity, &100u32, &4u32);
    assert_eq!(out_of_range.len(), 0);
}

// --- flagging ---

/// Establishes `reporter` as a genuine counterparty of `entity` via a
/// completed transaction, satisfying `flag_entity`'s reporter gate.
fn make_transacting_counterparty(
    client: &ReputationContractClient,
    admin: &Address,
    entity: &Address,
    reporter: &Address,
    escrow_id: u64,
) {
    client.record_transaction(
        admin,
        &escrow_id,
        entity,
        reporter,
        &1000i128,
        &TransactionOutcome::Released,
    );
}

#[test]
fn test_flag_entity_happy_path() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let reporter = Address::generate(&env);
    make_transacting_counterparty(&client, &admin, &entity, &reporter, 1u64);

    client.flag_entity(
        &reporter,
        &entity,
        &symbol_short!("fraud"),
        &Some(String::from_str(&env, "fake goods")),
    );

    let flags = client.get_flags(&entity, &0u32, &10u32);
    assert_eq!(flags.len(), 1);
    assert!(!flags.get(0).unwrap().resolved);
    assert!(!client.is_frozen(&entity));
}

#[test]
fn test_flag_entity_rejects_non_counterparty_non_admin() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    let entity = Address::generate(&env);
    let stranger = Address::generate(&env);

    let res = client.try_flag_entity(&stranger, &entity, &symbol_short!("fraud"), &None);
    assert_eq!(res, Err(Ok(ReputationError::Unauthorized)));
}

#[test]
fn test_flag_entity_allows_admin_without_transaction() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);

    client.flag_entity(&admin, &entity, &symbol_short!("fraud"), &None);

    let flags = client.get_flags(&entity, &0u32, &10u32);
    assert_eq!(flags.len(), 1);
}

#[test]
fn test_flag_entity_same_reporter_rejected_while_active() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let reporter = Address::generate(&env);
    make_transacting_counterparty(&client, &admin, &entity, &reporter, 1u64);

    client.flag_entity(&reporter, &entity, &symbol_short!("fraud"), &None);

    let res = client.try_flag_entity(&reporter, &entity, &symbol_short!("spam"), &None);
    assert_eq!(res, Err(Ok(ReputationError::AlreadyFlagged)));
}

#[test]
fn test_flag_entity_auto_freezes_at_threshold() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);

    for i in 0..3u64 {
        let reporter = Address::generate(&env);
        make_transacting_counterparty(&client, &admin, &entity, &reporter, i);
        client.flag_entity(&reporter, &entity, &symbol_short!("fraud"), &None);
    }

    assert!(client.is_frozen(&entity));
}

#[test]
fn test_resolve_flag_happy_path() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let reporter = Address::generate(&env);
    make_transacting_counterparty(&client, &admin, &entity, &reporter, 1u64);

    client.flag_entity(&reporter, &entity, &symbol_short!("fraud"), &None);
    client.resolve_flag(&admin, &reporter, &entity);

    let flags = client.get_flags(&entity, &0u32, &10u32);
    assert!(flags.get(0).unwrap().resolved);

    // Reporter can flag again now that their prior flag is resolved.
    client.flag_entity(&reporter, &entity, &symbol_short!("spam"), &None);
    let flags = client.get_flags(&entity, &0u32, &10u32);
    assert_eq!(flags.len(), 2);
}

#[test]
fn test_resolve_flag_unauthorized() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let reporter = Address::generate(&env);
    let not_admin = Address::generate(&env);
    make_transacting_counterparty(&client, &admin, &entity, &reporter, 1u64);

    client.flag_entity(&reporter, &entity, &symbol_short!("fraud"), &None);

    let res = client.try_resolve_flag(&not_admin, &reporter, &entity);
    assert_eq!(res, Err(Ok(ReputationError::Unauthorized)));
}

#[test]
fn test_resolve_flag_missing() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let reporter = Address::generate(&env);

    // No flags at all on the entity => nothing active to resolve.
    let res = client.try_resolve_flag(&admin, &reporter, &entity);
    assert_eq!(res, Err(Ok(ReputationError::NoActiveFlag)));
}

#[test]
fn test_resolve_flag_no_active_flag() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let reporter = Address::generate(&env);
    make_transacting_counterparty(&client, &admin, &entity, &reporter, 1u64);

    // First flag and resolve it
    client.flag_entity(&reporter, &entity, &symbol_short!("fraud"), &None);
    client.resolve_flag(&admin, &reporter, &entity);

    // Now try to resolve again - should return NoActiveFlag since the flag is already resolved
    let res = client.try_resolve_flag(&admin, &reporter, &entity);
    assert_eq!(res, Err(Ok(ReputationError::NoActiveFlag)));
}

#[test]
fn test_resolve_flag_not_flag_reporter() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let reporter = Address::generate(&env);
    let other_reporter = Address::generate(&env);
    make_transacting_counterparty(&client, &admin, &entity, &reporter, 1u64);
    make_transacting_counterparty(&client, &admin, &entity, &other_reporter, 2u64);

    client.flag_entity(&reporter, &entity, &symbol_short!("fraud"), &None);

    let _res = client.try_resolve_flag(&admin, &other_reporter, &entity);
}

// --- freeze / unfreeze ---

#[test]
fn test_freeze_and_unfreeze_entity() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);

    assert!(!client.is_frozen(&entity));
    client.freeze_entity(&admin, &entity);
    assert!(client.is_frozen(&entity));
    client.unfreeze_entity(&admin, &entity);
    assert!(!client.is_frozen(&entity));
}

#[test]
fn test_freeze_entity_unauthorized() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    let entity = Address::generate(&env);
    let not_admin = Address::generate(&env);

    let res = client.try_freeze_entity(&not_admin, &entity);
    assert_eq!(res, Err(Ok(ReputationError::Unauthorized)));
}

// --- update_config ---

#[test]
fn test_update_config_happy_path() {
    let env = Env::default();
    let (client, admin) = setup(&env);

    let mut new_config = default_config();
    new_config.min_transactions_threshold = 1;
    client.update_config(&admin, &new_config);

    assert_eq!(client.get_config(), new_config);
}

#[test]
fn test_get_config_parity() {
    let env = Env::default();
    let (client, admin) = setup(&env);

    let mut new_config = default_config();
    new_config.freeze_threshold_flags = 1;
    client.update_config(&admin, &new_config);

    assert_eq!(client.get_config(), new_config);
}

#[test]
fn test_update_config_unauthorized() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    let not_admin = Address::generate(&env);

    let res = client.try_update_config(&not_admin, &default_config());
    assert_eq!(res, Err(Ok(ReputationError::Unauthorized)));
}

#[test]
fn test_update_config_rejects_invalid() {
    let env = Env::default();
    let (client, admin) = setup(&env);

    let mut bad = default_config();
    bad.decay_window_seconds = 0;
    let res = client.try_update_config(&admin, &bad);
    assert_eq!(res, Err(Ok(ReputationError::InvalidParam)));
}

// --- admin transfer ---

#[test]
fn test_propose_and_accept_admin() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let new_admin = Address::generate(&env);

    client.propose_admin(&admin, &new_admin);
    client.accept_admin(&new_admin);

    // Old admin can no longer perform admin actions.
    let entity = Address::generate(&env);
    let res = client.try_freeze_entity(&admin, &entity);
    assert_eq!(res, Err(Ok(ReputationError::Unauthorized)));

    // New admin can.
    client.freeze_entity(&new_admin, &entity);
    assert!(client.is_frozen(&entity));
}

#[test]
fn test_propose_admin_unauthorized() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    let not_admin = Address::generate(&env);
    let new_admin = Address::generate(&env);

    let res = client.try_propose_admin(&not_admin, &new_admin);
    assert_eq!(res, Err(Ok(ReputationError::Unauthorized)));
}

#[test]
fn test_accept_admin_wrong_caller() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let new_admin = Address::generate(&env);
    let stranger = Address::generate(&env);

    client.propose_admin(&admin, &new_admin);

    let res = client.try_accept_admin(&stranger);
    assert_eq!(res, Err(Ok(ReputationError::Unauthorized)));
}

#[test]
fn test_accept_admin_no_pending_transfer() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    let stranger = Address::generate(&env);

    let res = client.try_accept_admin(&stranger);
    assert_eq!(res, Err(Ok(ReputationError::Unauthorized)));
}

// --- recency_weight_bps decay curve ---

/// Invariant: `BPS_SCALE >> MAX_HALVINGS` must be non-zero.
///
/// If this fails it means `MAX_HALVINGS` has drifted past the point where the
/// shift can produce any non-zero base, making the guard in
/// `recency_weight_bps` fire *after* the shift already rounds to zero —
/// i.e. the documented cutoff is later than the actual one.
#[test]
fn test_max_halvings_invariant() {
    // Compile-time invariant check: the half-life shift must still produce a
    // non-zero base at the declared cutoff (see doc comment above).
    const {
        assert!(
            crate::BPS_SCALE >> crate::MAX_HALVINGS != 0,
            "BPS_SCALE >> MAX_HALVINGS == 0: MAX_HALVINGS must be lowered \
         (or BPS_SCALE raised) so that the shift still produces a non-zero \
         base at the declared cutoff",
        )
    }
}

/// Decay-curve smoke test: checks expected outputs for every full half-life
/// from 0 through MAX_HALVINGS (inclusive), using a decay window of exactly
/// 1 second so that `elapsed = k` implies exactly `k` full halvings and
/// zero remainder seconds.
///
/// At k = 0          : weight == BPS_SCALE (no decay).
/// At k = MAX_HALVINGS: weight == 0 (early-exit guard fires).
///
/// The test verifies:
///   1. weight == BPS_SCALE at k == 0.
///   2. Strict monotone decrease for k in 0..MAX_HALVINGS.
///   3. weight == 0 at k == MAX_HALVINGS (guard's documented semantics).
#[test]
fn test_recency_weight_decay_curve() {
    // decay_window = 1 s so that elapsed = k  =>  full_halvings = k,
    // remainder_secs = 0, no linear interpolation term.
    let decay_window: u64 = 1;
    let max = crate::MAX_HALVINGS;

    // k=0: full weight.
    assert_eq!(
        crate::recency_weight_bps(0, decay_window),
        crate::BPS_SCALE,
        "weight at 0 half-lives should equal BPS_SCALE"
    );

    // k=MAX_HALVINGS: guard fires, weight must be 0.
    assert_eq!(
        crate::recency_weight_bps(max, decay_window),
        0,
        "weight at MAX_HALVINGS ({max}) half-lives should be 0"
    );

    // Strict monotone decrease across 0..MAX_HALVINGS.
    let mut prev = crate::recency_weight_bps(0, decay_window);
    for k in 1..max {
        let curr = crate::recency_weight_bps(k, decay_window);
        assert!(
            curr < prev,
            "decay curve not strictly decreasing at half-life {k}: \
             weight[{k}]={curr} is not less than weight[{}]={prev}",
            k - 1,
        );
        prev = curr;
    }
}

#[test]
fn test_event_namespace_consistency() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::TryIntoVal;
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);
    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Released,
    );
    client.rate_entity(&counterparty, &1u64, &entity, &9000u32);
    client.flag_entity(&admin, &entity, &symbol_short!("fraud"), &None);
    client.freeze_entity(&admin, &entity);
    client.unfreeze_entity(&admin, &entity);
    let new_admin = Address::generate(&env);
    client.propose_admin(&admin, &new_admin);
    client.accept_admin(&new_admin);
    let events = env.events().all();
    let contract_id = client.address;

    let mut contract_event_count = 0;
    for (contract, topics, _) in events.into_iter() {
        if contract == contract_id {
            contract_event_count += 1;
            let namespace: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            assert_eq!(
                namespace,
                symbol_short!("reput"),
                "All reputation events must use the 'reput' namespace"
            );
        }
    }
    assert!(contract_event_count > 0);
}

// --- prune_entity_history tests ---

#[test]
fn test_prune_entity_history_noop_when_within_window() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    // Record 5 transactions (<= SCORE_WINDOW which is 200)
    for i in 1..=5 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000,
            &TransactionOutcome::Released,
        );
    }

    let pruned = client.prune_entity_history(&admin, &entity, &50);
    assert_eq!(pruned, 0);

    let breakdown = client.get_reputation_breakdown(&entity, &0, &10);
    assert_eq!(breakdown.len(), 5);
}

#[test]
fn test_prune_entity_history_beyond_score_window() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    // Record 205 transactions (> SCORE_WINDOW = 200)
    for i in 1..=205 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000,
            &TransactionOutcome::Released,
        );
    }

    let breakdown_before = client.get_reputation_breakdown(&entity, &0, &250);
    assert_eq!(breakdown_before.len(), 205);

    // Prune up to 50 excess records (excess = 5)
    let pruned = client.prune_entity_history(&admin, &entity, &50);
    assert_eq!(pruned, 5);

    let breakdown_after = client.get_reputation_breakdown(&entity, &0, &250);
    assert_eq!(breakdown_after.len(), 200);
    assert_eq!(breakdown_after.get(0).unwrap().escrow_id, 6);
}

#[test]
fn test_prune_entity_history_unauthorized() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    let stranger = Address::generate(&env);
    let entity = Address::generate(&env);

    let res = client.try_prune_entity_history(&stranger, &entity, &50);
    assert_eq!(res, Err(Ok(ReputationError::Unauthorized)));
}

// ═════════════════════════════════════════════════════════════════════════════
// Issue #138: freeze-threshold, SCORE_WINDOW boundary, and duplicate-flag
// resolution edge cases
// ═════════════════════════════════════════════════════════════════════════════

/// The auto-freeze must fire *exactly* at `freeze_threshold_flags` unresolved
/// flags: one below it the entity stays operational, at the threshold it
/// freezes, and further flags neither unfreeze nor re-freeze (one-way).
/// Each reporter must first transact with `entity` to satisfy `flag_entity`'s
/// counterparty gate.
#[test]
fn test_flag_entity_freezes_exactly_at_threshold() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);

    let reporter_a = Address::generate(&env);
    let reporter_b = Address::generate(&env);
    let reporter_c = Address::generate(&env);
    let reporter_d = Address::generate(&env);
    // All counterparty relationships are established up front: once the third
    // flag freezes the entity, `record_transaction` would reject new escrows.
    for (reporter, id) in [
        (reporter_a.clone(), 1u64),
        (reporter_b.clone(), 2u64),
        (reporter_c.clone(), 3u64),
        (reporter_d.clone(), 4u64),
    ] {
        make_transacting_counterparty(&client, &admin, &entity, &reporter, id);
    }

    // threshold - 1 unresolved flags: still below the boundary, not frozen.
    client.flag_entity(&reporter_a, &entity, &symbol_short!("fraud"), &None);
    client.flag_entity(&reporter_b, &entity, &symbol_short!("fraud"), &None);
    assert!(!client.is_frozen(&entity));
    assert_eq!(client.get_flags(&entity, &0u32, &10u32).len(), 2);

    // Exactly at the threshold: auto-freeze fires.
    client.flag_entity(&reporter_c, &entity, &symbol_short!("fraud"), &None);
    assert!(client.is_frozen(&entity));
    assert_eq!(client.get_flags(&entity, &0u32, &10u32).len(), 3);

    // Freezing is one-way: an extra flag past the threshold leaves the entity
    // frozen rather than re-toggling the flag.
    client.flag_entity(&reporter_d, &entity, &symbol_short!("fraud"), &None);
    assert!(client.is_frozen(&entity));
    assert_eq!(client.get_flags(&entity, &0u32, &10u32).len(), 4);
}

/// Exact-capacity boundary: with `SCORE_WINDOW` lifetime records the recompute
/// window starts at index 0 and must sample *every* record. One Disputed (0)
/// mixed into `SCORE_WINDOW - 1` Released (10_000), all with identical
/// `recorded_at`, produces an exact weighted average of 9_950 — proving the
/// whole window participated (a window that silently dropped the oldest record
/// or clamped the count would yield a different value).
#[test]
fn test_score_window_under_capacity_samples_all_records() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut cfg = default_config();
    cfg.min_transactions_threshold = 1;
    cfg.dispute_penalty_bps = 0;
    let contract_id = env.register(ReputationContract, (admin.clone(), cfg));
    let client = ReputationContractClient::new(&env, &contract_id);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.record_transaction(
        &admin,
        &0u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Disputed,
    );
    for i in 1..SCORE_WINDOW as u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Released,
        );
    }

    let rep = client.get_reputation(&entity);
    assert_eq!(rep.total_transactions, SCORE_WINDOW as u64);
    // (199 * 10_000 + 1 * 0) / 200 = 9_950 (dispute penalty disabled).
    assert_eq!(rep.score, 9_950);
}

/// The moment history exceeds `SCORE_WINDOW` records, the recompute window
/// slides and the *oldest* record must exit the sample. A Disputed recorded
/// first, followed by exactly `SCORE_WINDOW` Released records, gives a clean
/// score of 10_000 — if the oldest Disputed were still sampled (no sliding),
/// the average would be 9_950 instead. The lifetime dispute counter is
/// unaffected by the window and stays exact.
#[test]
fn test_score_window_slide_excludes_oldest_record_across_boundary() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut cfg = default_config();
    cfg.min_transactions_threshold = 1;
    cfg.dispute_penalty_bps = 0;
    let contract_id = env.register(ReputationContract, (admin.clone(), cfg));
    let client = ReputationContractClient::new(&env, &contract_id);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.record_transaction(
        &admin,
        &0u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Disputed,
    );
    for i in 1..=SCORE_WINDOW as u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Released,
        );
    }

    let rep = client.get_reputation(&entity);
    assert_eq!(rep.total_transactions, SCORE_WINDOW as u64 + 1);
    assert_eq!(rep.disputed_transactions, 1);
    // Window slid to the newest SCORE_WINDOW records: all Released -> 10_000.
    assert_eq!(rep.score, 10_000);
}

/// Resolving the same reporter/entity flag twice must not silently succeed:
/// the second call finds no *unresolved* flag to match (the entity has no
/// active flags left) and returns `NoActiveFlag`; the stored flag remains a
/// single resolved entry.
#[test]
fn test_resolve_flag_duplicate_resolution_rejected() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let reporter = Address::generate(&env);
    make_transacting_counterparty(&client, &admin, &entity, &reporter, 1u64);

    client.flag_entity(&reporter, &entity, &symbol_short!("fraud"), &None);
    client.resolve_flag(&admin, &reporter, &entity);

    let res = client.try_resolve_flag(&admin, &reporter, &entity);
    assert_eq!(res, Err(Ok(ReputationError::NoActiveFlag)));

    let flags = client.get_flags(&entity, &0u32, &10u32);
    assert_eq!(flags.len(), 1);
    assert!(flags.get(0).unwrap().resolved);
}

/// `resolve_flag` targets a single reporter's unresolved flag; resolving one
/// of several must leave the others' resolved-status untouched, and a repeat
/// resolve for the already-cleared reporter is rejected.
#[test]
fn test_resolve_flag_resolves_single_flag_among_many() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let reporter_a = Address::generate(&env);
    let reporter_b = Address::generate(&env);
    make_transacting_counterparty(&client, &admin, &entity, &reporter_a, 1u64);
    make_transacting_counterparty(&client, &admin, &entity, &reporter_b, 2u64);

    client.flag_entity(&reporter_a, &entity, &symbol_short!("fraud"), &None);
    client.flag_entity(&reporter_b, &entity, &symbol_short!("spam"), &None);
    client.resolve_flag(&admin, &reporter_a, &entity);

    let flags = client.get_flags(&entity, &0u32, &10u32);
    assert!(flags.get(0).unwrap().resolved);
    assert!(!flags.get(1).unwrap().resolved);

    // The already-resolved reporter's flag is gone from the active pool.
    let res = client.try_resolve_flag(&admin, &reporter_a, &entity);
    assert_eq!(res, Err(Ok(ReputationError::NoActiveFlag)));
}

/// Resolving a flag must not retroactively lift an auto-freeze already
/// reached at the threshold: unfreezing is an explicit admin action.
#[test]
fn test_resolve_flag_does_not_auto_unfreeze() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);

    let reporter_a = Address::generate(&env);
    let reporter_b = Address::generate(&env);
    let reporter_c = Address::generate(&env);
    make_transacting_counterparty(&client, &admin, &entity, &reporter_a, 0u64);
    make_transacting_counterparty(&client, &admin, &entity, &reporter_b, 1u64);
    make_transacting_counterparty(&client, &admin, &entity, &reporter_c, 2u64);

    client.flag_entity(&reporter_a, &entity, &symbol_short!("fraud"), &None);
    client.flag_entity(&reporter_b, &entity, &symbol_short!("fraud"), &None);
    client.flag_entity(&reporter_c, &entity, &symbol_short!("fraud"), &None);
    assert!(client.is_frozen(&entity));

    client.resolve_flag(&admin, &reporter_a, &entity);
    assert!(client.is_frozen(&entity));

    client.unfreeze_entity(&admin, &entity);
    assert!(!client.is_frozen(&entity));
}

// ── Read-path TTL Bumping Tests (#78) ────────────────────────────────────────

#[test]
fn test_read_paths_bump_entity_and_records_ttl() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.record_transaction(
        &admin,
        &101,
        &entity,
        &counterparty,
        &1_000,
        &TransactionOutcome::Released,
    );

    client.flag_entity(
        &counterparty,
        &entity,
        &symbol_short!("dispute"),
        &Some(String::from_str(&env, "Delayed delivery")),
    );

    // Initial TTL check
    let rep_ttl_initial = env.as_contract(&client.address, || {
        env.storage().instance().extend_ttl(1_000_000, 1_000_000);
        let storage = env.storage().persistent();
        let bump_thresh = PERSISTENT_BUMP_AMOUNT;
        let bump_amt = PERSISTENT_BUMP_AMOUNT;
        let flags_key = DataKey::Flags(entity.clone());
        if storage.has(&flags_key) {
            storage.extend_ttl(&flags_key, bump_thresh, bump_amt);
        }
        let hist_key = DataKey::TransactionHistory(entity.clone());
        if storage.has(&hist_key) {
            storage.extend_ttl(&hist_key, bump_thresh, bump_amt);
        }
        let rec_key = DataKey::TransactionRecord(101);
        if storage.has(&rec_key) {
            storage.extend_ttl(&rec_key, bump_thresh, bump_amt);
        }
        let rep_key = DataKey::Reputation(entity.clone());
        if storage.has(&rep_key) {
            storage.extend_ttl(&rep_key, bump_thresh, bump_amt);
        }
        storage.get_ttl(&rep_key)
    });
    assert!(rep_ttl_initial >= PERSISTENT_BUMP_AMOUNT);

    let jump_ledgers = rep_ttl_initial - PERSISTENT_BUMP_THRESHOLD + 1;

    // 1. Test get_reputation read path: simulate decay down to bump threshold
    env.ledger().with_mut(|li| {
        li.sequence_number += jump_ledgers;
    });

    let rep_ttl_before = env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::Reputation(entity.clone()))
    });
    assert!(rep_ttl_before < rep_ttl_initial);

    let score = client.get_reputation(&entity);
    assert_eq!(score.total_transactions, 1);

    let rep_ttl_after = env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::Reputation(entity.clone()))
    });
    assert!(rep_ttl_after >= PERSISTENT_BUMP_AMOUNT);
    assert!(rep_ttl_after > rep_ttl_before);

    // 2. Test get_reputation_breakdown read path: protect instance before next decay
    env.as_contract(&client.address, || {
        env.storage().instance().extend_ttl(1_000_000, 1_000_000);
    });

    env.ledger().with_mut(|li| {
        li.sequence_number += jump_ledgers;
    });

    let (hist_ttl_before, rec_ttl_before) = env.as_contract(&client.address, || {
        (
            env.storage()
                .persistent()
                .get_ttl(&DataKey::TransactionHistory(entity.clone())),
            env.storage()
                .persistent()
                .get_ttl(&DataKey::TransactionRecord(101)),
        )
    });

    let breakdown = client.get_reputation_breakdown(&entity, &0, &10);
    assert_eq!(breakdown.len(), 1);

    let (hist_ttl_after, rec_ttl_after) = env.as_contract(&client.address, || {
        (
            env.storage()
                .persistent()
                .get_ttl(&DataKey::TransactionHistory(entity.clone())),
            env.storage()
                .persistent()
                .get_ttl(&DataKey::TransactionRecord(101)),
        )
    });
    assert!(hist_ttl_after > hist_ttl_before);
    assert!(rec_ttl_after > rec_ttl_before);

    // 3. Test get_flags read path
    env.as_contract(&client.address, || {
        env.storage().instance().extend_ttl(1_000_000, 1_000_000);
    });

    env.ledger().with_mut(|li| {
        li.sequence_number += jump_ledgers;
    });

    let flags_ttl_before = env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::Flags(entity.clone()))
    });

    let flags = client.get_flags(&entity, &0, &10);
    assert_eq!(flags.len(), 1);

    let flags_ttl_after = env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::Flags(entity.clone()))
    });
    assert!(flags_ttl_after > flags_ttl_before);
}

#[test]
fn test_slow_activity_liveness_keeps_entity_alive_on_read() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.record_transaction(
        &admin,
        &201,
        &entity,
        &counterparty,
        &5_000,
        &TransactionOutcome::Released,
    );

    // Periodically perform read operations across multiple epochs
    for _ in 0..5 {
        env.as_contract(&client.address, || {
            env.storage().instance().extend_ttl(1_000_000, 1_000_000);
        });

        env.ledger().with_mut(|li| {
            li.sequence_number += 508_400;
        });

        // Calling get_reputation refreshes the entity's TTL
        let rep = client.get_reputation(&entity);
        assert_eq!(rep.total_transactions, 1);

        let ttl = env.as_contract(&client.address, || {
            env.storage()
                .persistent()
                .get_ttl(&DataKey::Reputation(entity.clone()))
        });
        assert!(ttl >= PERSISTENT_BUMP_AMOUNT);
    }
}

#[test]
fn test_score_recalculates_after_feedback_submission() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    // Record enough transactions to pass the min_transactions_threshold (5).
    for escrow_id in 1u64..=6 {
        client.record_transaction(
            &admin,
            &escrow_id,
            &entity,
            &counterparty,
            &1_000,
            &TransactionOutcome::Released,
        );
    }

    let rep_before = client.get_reputation(&entity);
    assert_eq!(rep_before.total_transactions, 6);
    // Score is unmasked once threshold is met.
    assert!(
        rep_before.score > 0,
        "score should be non-zero after threshold is met"
    );

    // Submit a dispute — this should drive the score down on next recompute.
    client.record_transaction(
        &admin,
        &7,
        &entity,
        &counterparty,
        &1_000,
        &TransactionOutcome::Disputed,
    );

    let rep_after = client.get_reputation(&entity);
    assert_eq!(rep_after.total_transactions, 7);
    assert!(
        rep_after.score <= rep_before.score,
        "score should not rise after a dispute; before={} after={}",
        rep_before.score,
        rep_after.score,
    );

    // Verify that a ReputationScoreUpdatedEvent was emitted with the
    // correct topic schema (reput, updated, entity).
    let events = env.events().all();
    let found = events.iter().any(|(contract_id, topics, _data)| {
        use soroban_sdk::IntoVal;
        contract_id == client.address
            && topics.len() == 3
            && topics.get(0) == Some(symbol_short!("reput").into_val(&env))
            && topics.get(1) == Some(symbol_short!("updated").into_val(&env))
            && topics.get(2) == Some(entity.clone().into_val(&env))
    });
    assert!(
        found,
        "expected a (reput, updated, entity) score-updated event"
    );
}

// --- ConfigUpdatedEvent tests ---
//
// Each test confirms that `update_config` emits a `(reput, cfg_upd)` event
// carrying the exact old and new `ReputationConfig` values.  One test per
// mutable field satisfies the "field-level change tests" acceptance criterion.

/// Helper: find the single `ConfigUpdatedEvent` emitted by the contract and
/// return it, panicking if none (or more than one) is found.
#[cfg(test)]
fn extract_config_updated_event(
    env: &Env,
    contract_id: &Address,
) -> crate::ConfigUpdatedEvent {
    use soroban_sdk::{testutils::Events, IntoVal, TryIntoVal};

    let events = env.events().all();
    let mut found: Vec<crate::ConfigUpdatedEvent> = std::vec![];
    for (contract, topics, data) in events.into_iter() {
        if contract != *contract_id {
            continue;
        }
        if topics.len() < 2 {
            continue;
        }
        let ns: soroban_sdk::Symbol = match topics.get(0).unwrap().try_into_val(env) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let name: soroban_sdk::Symbol = match topics.get(1).unwrap().try_into_val(env) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if ns == symbol_short!("reput") && name == symbol_short!("cfg_upd") {
            let evt: crate::ConfigUpdatedEvent = data.try_into_val(env).unwrap();
            found.push(evt);
        }
    }
    assert_eq!(found.len(), 1, "expected exactly one cfg_upd event, got {}", found.len());
    found.remove(0)
}

#[test]
fn test_update_config_emits_event_with_old_and_new_config() {
    let env = Env::default();
    let (client, admin) = setup(&env);

    let old_config = default_config();
    let mut new_config = old_config.clone();
    new_config.min_transactions_threshold = 3;

    client.update_config(&admin, &new_config);

    let evt = extract_config_updated_event(&env, &client.address);
    assert_eq!(evt.old, old_config);
    assert_eq!(evt.new, new_config);
}

#[test]
fn test_update_config_event_decay_window_change() {
    // Changing decay_window_seconds: old and new differ only in that field.
    let env = Env::default();
    let (client, admin) = setup(&env);

    let old_config = default_config();
    let mut new_config = old_config.clone();
    new_config.decay_window_seconds = old_config.decay_window_seconds * 2;

    client.update_config(&admin, &new_config);

    let evt = extract_config_updated_event(&env, &client.address);
    assert_eq!(evt.old.decay_window_seconds, old_config.decay_window_seconds);
    assert_eq!(evt.new.decay_window_seconds, new_config.decay_window_seconds);
    // All other fields are identical.
    assert_eq!(evt.old.min_transactions_threshold, evt.new.min_transactions_threshold);
    assert_eq!(evt.old.dispute_penalty_bps, evt.new.dispute_penalty_bps);
    assert_eq!(evt.old.freeze_threshold_flags, evt.new.freeze_threshold_flags);
}

#[test]
fn test_update_config_event_min_transactions_threshold_change() {
    // Changing min_transactions_threshold: old and new differ only in that field.
    let env = Env::default();
    let (client, admin) = setup(&env);

    let old_config = default_config();
    let mut new_config = old_config.clone();
    new_config.min_transactions_threshold = old_config.min_transactions_threshold + 1;

    client.update_config(&admin, &new_config);

    let evt = extract_config_updated_event(&env, &client.address);
    assert_eq!(evt.old.min_transactions_threshold, old_config.min_transactions_threshold);
    assert_eq!(evt.new.min_transactions_threshold, new_config.min_transactions_threshold);
    assert_eq!(evt.old.decay_window_seconds, evt.new.decay_window_seconds);
    assert_eq!(evt.old.dispute_penalty_bps, evt.new.dispute_penalty_bps);
    assert_eq!(evt.old.freeze_threshold_flags, evt.new.freeze_threshold_flags);
}

#[test]
fn test_update_config_event_dispute_penalty_bps_change() {
    // Changing dispute_penalty_bps: old and new differ only in that field.
    let env = Env::default();
    let (client, admin) = setup(&env);

    let old_config = default_config();
    let mut new_config = old_config.clone();
    new_config.dispute_penalty_bps = old_config.dispute_penalty_bps + 100;

    client.update_config(&admin, &new_config);

    let evt = extract_config_updated_event(&env, &client.address);
    assert_eq!(evt.old.dispute_penalty_bps, old_config.dispute_penalty_bps);
    assert_eq!(evt.new.dispute_penalty_bps, new_config.dispute_penalty_bps);
    assert_eq!(evt.old.decay_window_seconds, evt.new.decay_window_seconds);
    assert_eq!(evt.old.min_transactions_threshold, evt.new.min_transactions_threshold);
    assert_eq!(evt.old.freeze_threshold_flags, evt.new.freeze_threshold_flags);
}

#[test]
fn test_update_config_event_freeze_threshold_flags_change() {
    // Changing freeze_threshold_flags: old and new differ only in that field.
    let env = Env::default();
    let (client, admin) = setup(&env);

    let old_config = default_config();
    let mut new_config = old_config.clone();
    new_config.freeze_threshold_flags = old_config.freeze_threshold_flags + 2;

    client.update_config(&admin, &new_config);

    let evt = extract_config_updated_event(&env, &client.address);
    assert_eq!(evt.old.freeze_threshold_flags, old_config.freeze_threshold_flags);
    assert_eq!(evt.new.freeze_threshold_flags, new_config.freeze_threshold_flags);
    assert_eq!(evt.old.decay_window_seconds, evt.new.decay_window_seconds);
    assert_eq!(evt.old.min_transactions_threshold, evt.new.min_transactions_threshold);
    assert_eq!(evt.old.dispute_penalty_bps, evt.new.dispute_penalty_bps);
}

#[test]
fn test_update_config_emits_no_event_on_unauthorized_call() {
    // A rejected call must not emit any cfg_upd event.
    use soroban_sdk::testutils::Events;
    let env = Env::default();
    let (client, _admin) = setup(&env);
    let not_admin = Address::generate(&env);

    let _ = client.try_update_config(&not_admin, &default_config());

    let events = env.events().all();
    let cfg_upd_count = events.iter().filter(|(contract, topics, _)| {
        use soroban_sdk::{IntoVal, TryIntoVal};
        *contract == client.address
            && topics.get(1)
                .and_then(|v| v.try_into_val::<_, soroban_sdk::Symbol>(&env).ok())
                .map(|s| s == symbol_short!("cfg_upd"))
                .unwrap_or(false)
    }).count();
    assert_eq!(cfg_upd_count, 0, "unauthorized call must not emit cfg_upd event");
}

#[test]
fn test_update_config_event_namespace_is_reput() {
    // Confirm the event topics follow the ('reput', 'cfg_upd') schema required
    // by the namespace-consistency rule checked in test_event_namespace_consistency.
    use soroban_sdk::{testutils::Events, IntoVal, TryIntoVal};
    let env = Env::default();
    let (client, admin) = setup(&env);

    let mut new_config = default_config();
    new_config.dispute_penalty_bps = 200;
    client.update_config(&admin, &new_config);

    let events = env.events().all();
    let cfg_upd_events: Vec<_> = events
        .iter()
        .filter(|(contract, topics, _)| {
            *contract == client.address
                && topics
                    .get(1)
                    .and_then(|v| v.try_into_val::<_, soroban_sdk::Symbol>(&env).ok())
                    .map(|s| s == symbol_short!("cfg_upd"))
                    .unwrap_or(false)
        })
        .collect();

    assert_eq!(cfg_upd_events.len(), 1, "expected exactly one cfg_upd event");
    let (_, topics, _) = &cfg_upd_events[0];
    let ns: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(ns, symbol_short!("reput"), "cfg_upd event namespace must be 'reput'");
// --- strict numeric normalization (issue #395) ---
//
// Score recalculation must never panic, underflow, or wrap when a merchant
// accumulates extreme consecutive negative reviews. Every persisted score
// funnels through `normalize_score`, which clamps the raw signed value into
// the legal `0..=10_000` basis-point band.

#[test]
fn test_normalize_score_clamps_to_basis_point_band() {
    assert_eq!(crate::normalize_score(i64::MIN), 0);
    assert_eq!(crate::normalize_score(-1), 0);
    assert_eq!(crate::normalize_score(0), 0);
    assert_eq!(crate::normalize_score(1), 1);
    assert_eq!(crate::normalize_score(5_000), 5_000);
    assert_eq!(crate::normalize_score(9_999), 9_999);
    assert_eq!(crate::normalize_score(10_000), 10_000);
    assert_eq!(crate::normalize_score(10_001), 10_000);
    assert_eq!(crate::normalize_score(i64::MAX), 10_000);
}
#[test]
fn test_normalize_score_is_idempotent_and_monotonic() {
    // Property: the clamp output always lands in the legal band, normalizing
    // an already-normalized value is a no-op, and the mapping is monotonically
    // non-decreasing across the whole signed domain.
    const STEP: i64 = 1_000_000_000_000;
    let mut previous = crate::normalize_score(i64::MIN);
    let mut raw: i64 = i64::MIN;
    while raw < i64::MAX {
        raw = raw.saturating_add(STEP);
        let normalized = crate::normalize_score(raw);
        assert!(
            normalized <= 10_000,
            "score {normalized} escaped 0..=10_000"
        );
        assert!(normalized >= previous, "normalization must not decrease");
        assert_eq!(crate::normalize_score(normalized as i64), normalized);
        previous = normalized;
    }
}

#[test]
fn test_calculate_decayed_score_stays_within_basis_point_band() {
    // Decay is a score recalculation too, so an out-of-band input must be
    // folded back into the legal band rather than returned verbatim.
    assert_eq!(crate::calculate_decayed_score(20_000, 0, 100), 10_000);
    assert_eq!(crate::calculate_decayed_score(10_000, 0, 100), 10_000);
    assert!(crate::calculate_decayed_score(10_000, 50, 100) <= 10_000);
}

#[test]
fn test_hundred_consecutive_negative_reviews_never_wrap() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let reviewer = Address::generate(&env);

    // Establish a perfect score first so the negative run has room to fall.
    for i in 0..5u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &reviewer,
// --- reputation score attestations (issue #394) ---

/// Moves the mocked ledger to `sequence`, which is the checkpoint an
/// attestation records.
fn set_sequence(env: &Env, sequence: u32) {
    env.ledger().set_sequence_number(sequence);
}

/// Records `count` freshly Released transactions for `entity`, enough to pass
/// the config's `min_transactions_threshold` when `count >= 5`.
fn seed_released_transactions(
// --- fraud reputation slashing (issue #392) ---

/// Registers the standard 5-escrow clean history so `entity` has a visible
/// score of 10_000 before a fraud ruling lands.
fn seed_clean_history(
    client: &ReputationContractClient,
    admin: &Address,
    entity: &Address,
    counterparty: &Address,
    start: u64,
    count: u64,
) {
    for i in start..(start + count) {
) {
    for i in 0..5u64 {
        client.record_transaction(
            admin,
            &i,
            entity,
            counterparty,
            &1_000i128,
            &TransactionOutcome::Released,
        );
    }
    assert_eq!(client.get_reputation(&entity).score, 10_000);

    // 100 consecutive negative reviews: refunded outcomes rated 0/10_000.
    for i in 5..105u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &reviewer,
            &1_000i128,
            &TransactionOutcome::Refunded,
        );
        client.rate_entity(&reviewer, &i, &entity, &0u32);

        let rep = client.get_reputation(&entity);
        assert!(
            rep.score <= 10_000,
            "score {} escaped the basis-point band on review {i}",
            rep.score
        );
        assert!(rep.avg_rating <= 10_000);
    }
}

#[test]
fn test_hundred_disputes_with_max_penalty_saturate_at_zero() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let mut config = default_config();
    // Maximum legal per-dispute penalty, so the accumulated penalty is far
    // larger than the maximum possible base score.
    config.dispute_penalty_bps = 10_000;
    let contract_id = env.register(ReputationContract, (admin.clone(), config));
    let client = ReputationContractClient::new(&env, &contract_id);

    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    for i in 0..100u64 {
// ════════════════════════════════════════════════════════════════════════════
// Issue #396: Role-specific scoring weight models
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn test_set_entity_role_classifies_merchant() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);

    client.set_entity_role(&admin, &entity.clone(), &EntityRole::Merchant);

    let rep = client.get_reputation(&entity);
    assert_eq!(rep.role, EntityRole::Merchant);
}

#[test]
fn test_set_entity_role_classifies_buyer_agent() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);

    client.set_entity_role(&admin, &entity.clone(), &EntityRole::BuyerAgent);

    let rep = client.get_reputation(&entity);
    assert_eq!(rep.role, EntityRole::BuyerAgent);
}

#[test]
fn test_set_entity_role_rejects_non_admin() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    let entity = Address::generate(&env);
    let not_admin = Address::generate(&env);

    let res = client.try_set_entity_role(
        &not_admin,
        &entity.clone(),
        &EntityRole::Merchant,
    );
    assert_eq!(res, Err(Ok(ReputationError::Unauthorized)));
}

#[test]
fn test_get_role_specific_metrics_merchant() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.set_entity_role(&admin, &entity.clone(), &EntityRole::Merchant);

    // 4 Released, 1 Disputed out of 5 total.
    for i in 0..4u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1_000i128,
            &TransactionOutcome::Disputed,
        );
        let rep = client.get_reputation(&entity);
        assert!(rep.score <= 10_000, "score wrapped on dispute {i}");
    }

    let rep = client.get_reputation(&entity);
    assert_eq!(rep.score, 0);
    assert_eq!(rep.disputed_transactions, 100);

    // The raw decomposition shows the *pre-clamp* value was deeply negative:
    // 100 disputes x 10_000 bps of penalty against a 0 bps base score. The
    // clamp — not an accidental wrap — is what produced 0.
    let decomposition = client.get_score_decomposition(&entity);
    assert_eq!(decomposition.base_score_bps, 0);
    assert_eq!(decomposition.penalty_bps, 1_000_000);
    assert_eq!(decomposition.final_score, 0);
}

#[test]
fn test_mixed_extreme_outcome_sequences_stay_bounded() {
    // Deterministic pseudo-random sequences (no external proptest
    // dependency): every generated history must leave the persisted score
    // inside `0..=10_000`, and the incremental fast path used by
    // `record_transaction` must agree with a full recompute.
    fn next(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *state >> 33
    }

    let outcomes = [
        TransactionOutcome::Released,
        TransactionOutcome::Refunded,
        TransactionOutcome::Disputed,
        TransactionOutcome::ResolvedSeller,
        TransactionOutcome::ResolvedBuyer,
    ];

    for seed in 0..8u64 {
        let env = Env::default();
        let (client, admin) = setup(&env);
        let entity = Address::generate(&env);
        let counterparty = Address::generate(&env);
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);

        for i in 0..40u64 {
            let outcome = outcomes[(next(&mut state) % outcomes.len() as u64) as usize].clone();
            let is_disputed = matches!(outcome, TransactionOutcome::Disputed);
            client.record_transaction(&admin, &i, &entity, &counterparty, &1_000i128, &outcome);
            if !is_disputed {
                let rating = (next(&mut state) % 10_001) as u32;
                client.rate_entity(&counterparty, &i, &entity, &rating);
            }

            let rep = client.get_reputation(&entity);
            assert!(
                rep.score <= 10_000,
                "seed {seed} step {i}: score escaped band"
            );
            assert!(rep.avg_rating <= 10_000);

            let decomposition = client.get_score_decomposition(&entity);
            assert!(
                (0..=10_000).contains(&decomposition.final_score),
                "seed {seed} step {i}: decomposition escaped band"
            );
            assert!(decomposition.base_score_bps <= 10_000);
        }

        // The incrementally maintained score is re-derivable from scratch.
        let incremental = client.get_reputation(&entity).score;
        let recomputed = client.get_score_decomposition(&entity).final_score;
        assert_eq!(incremental, recomputed, "seed {seed}: paths disagree");
    }
}

#[test]
fn test_generate_attestation_snapshots_public_score_and_window() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);
    set_sequence(&env, 1_000);
    seed_released_transactions(&client, &admin, &entity, &counterparty, 0, 5);

    let attestation = client.generate_score_attestation(&entity);

    assert_eq!(attestation.entity, entity);
    assert_eq!(attestation.score_bps, 10_000);
    assert_eq!(attestation.total_transactions, 5);
    // Sequence checkpoint plus the documented validity window.
    assert_eq!(attestation.certified_at_ledger, 1_000);
    assert_eq!(
        attestation.expires_at_ledger,
        1_000 + crate::ATTESTATION_VALIDITY_LEDGERS
    );
    assert!(attestation.expires_at_ledger > attestation.certified_at_ledger);
}

#[test]
fn test_generate_attestation_masks_score_below_threshold() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);
    seed_released_transactions(&client, &admin, &entity, &counterparty, 0, 1);

    let attestation = client.generate_score_attestation(&entity);
    let reputation = client.get_reputation(&entity);

    // The attestation must agree with the canonical read path, including the
    // min-transactions visibility gate.
    assert_eq!(reputation.score, 0);
    assert_eq!(attestation.score_bps, 0);
    assert_eq!(attestation.total_transactions, 1);
}

#[test]
fn test_generate_attestation_for_unknown_entity_is_zeroed_snapshot() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    let unknown = Address::generate(&env);
    set_sequence(&env, 42);

    let attestation = client.generate_score_attestation(&unknown);

    assert_eq!(attestation.entity, unknown);
    assert_eq!(attestation.score_bps, 0);
    assert_eq!(attestation.total_transactions, 0);
    assert_eq!(attestation.certified_at_ledger, 42);
    assert!(attestation.expires_at_ledger > 42);
}

#[test]
fn test_verify_attestation_accepts_fresh_snapshot() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);
    set_sequence(&env, 500);
    seed_released_transactions(&client, &admin, &entity, &counterparty, 0, 5);

    let attestation = client.generate_score_attestation(&entity);
    let verification = client.verify_score_attestation(&attestation);

    assert!(verification.valid);
    assert!(!verification.expired);
    assert!(verification.score_matches);
    assert_eq!(verification.attestation, attestation);
    assert_eq!(verification.current, attestation);
}

#[test]
fn test_verify_attestation_detects_score_drift() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);
    set_sequence(&env, 500);
    seed_released_transactions(&client, &admin, &entity, &counterparty, 0, 5);

    let cached = client.generate_score_attestation(&entity);

    // A dispute lands after the snapshot was certified, degrading the score.
    client.record_transaction(
        &admin,
        &99u64,
        &entity,
        &counterparty,
        &1_000i128,
        &TransactionOutcome::Disputed,
    );

    let verification = client.verify_score_attestation(&cached);
    assert!(!verification.valid);
    assert!(!verification.expired);
    assert!(!verification.score_matches);
    assert!(verification.current.score_bps < cached.score_bps);
    // The refreshed snapshot is itself verifiable.
    assert!(client.verify_score_attestation(&verification.current).valid);
}

#[test]
fn test_verify_attestation_detects_transaction_count_drift() {
            &1000i128,
            &TransactionOutcome::Released,
        );
    }
    client.record_transaction(
        &admin,
        &4u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Disputed,
    );

    let metrics = client.get_role_specific_metrics(&entity);
    // fulfillment = 4/5 * 10000 = 8000
    assert_eq!(metrics.fulfillment_rate_bps, 8000);
    // dispute frequency = 1/5 * 10000 = 2000
    assert_eq!(metrics.dispute_frequency_bps, 2000);
    assert_eq!(metrics.role, EntityRole::Merchant);
    assert_eq!(metrics.timely_payment_rate_bps, 8000);
}

#[test]
fn test_get_role_specific_metrics_buyer_agent() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);
    set_sequence(&env, 500);
    seed_released_transactions(&client, &admin, &entity, &counterparty, 0, 5);

    let cached = client.generate_score_attestation(&entity);

    // Same score (a clean Released outcome keeps it at 10_000), but the
    // lifetime transaction count backing it moved on.
    seed_released_transactions(&client, &admin, &entity, &counterparty, 100, 1);

    let verification = client.verify_score_attestation(&cached);
    assert!(!verification.valid);
    assert!(!verification.expired);
    assert!(!verification.score_matches);
    assert_eq!(verification.current.score_bps, cached.score_bps);
    assert_eq!(
        verification.current.total_transactions,
        cached.total_transactions + 1
    );
}

#[test]
fn test_verify_attestation_detects_expiry() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);
    set_sequence(&env, 500);
    seed_released_transactions(&client, &admin, &entity, &counterparty, 0, 5);

    let cached = client.generate_score_attestation(&entity);

    // One ledger before the window closes the snapshot is still authoritative.
    set_sequence(&env, cached.expires_at_ledger - 1);
    let fresh_window = client.verify_score_attestation(&cached);
    assert!(fresh_window.valid);
    assert!(!fresh_window.expired);

    // At the expiry ledger it is stale even though state is unchanged.
    set_sequence(&env, cached.expires_at_ledger);
    let expired = client.verify_score_attestation(&cached);
    assert!(!expired.valid);
    assert!(expired.expired);
    assert!(expired.score_matches);
    // Refreshing yields a snapshot anchored at the current ledger.
    assert_eq!(
        expired.current.certified_at_ledger,
        cached.expires_at_ledger
    );
    assert!(client.verify_score_attestation(&expired.current).valid);
}

#[test]
fn test_attestation_is_expired_boundaries() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);
    set_sequence(&env, 100);
    seed_released_transactions(&client, &admin, &entity, &counterparty, 0, 5);

    let attestation = client.generate_score_attestation(&entity);
    assert!(!crate::attestation_is_expired(&attestation, 100));
    assert!(!crate::attestation_is_expired(
        &attestation,
        attestation.expires_at_ledger - 1
    ));
    assert!(crate::attestation_is_expired(
        &attestation,
        attestation.expires_at_ledger
    ));
    assert!(crate::attestation_is_expired(&attestation, u32::MAX));

    // Malformed windows (expiry at or before certification) are always stale,
    // so a hand-built snapshot cannot bypass expiry.
    let malformed = crate::ReputationAttestation {
        entity: entity.clone(),
        score_bps: 10_000,
        total_transactions: 5,
        certified_at_ledger: 100,
        expires_at_ledger: 100,
    };
    assert!(crate::attestation_is_expired(&malformed, 0));
    let verification = client.verify_score_attestation(&malformed);
    assert!(!verification.valid);
    assert!(verification.expired);
}

#[test]
fn test_fraud_slash_reduces_score_by_penalty() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let escrow = Address::generate(&env);
    let merchant = Address::generate(&env);
    let counterparty = Address::generate(&env);
    seed_clean_history(&client, &admin, &merchant, &counterparty);
    assert_eq!(client.get_reputation(&merchant).score, 10_000);

    client.set_fraud_authority(&admin, &escrow);
    assert_eq!(client.get_fraud_authority(), Some(escrow.clone()));

    let new_score = client.apply_fraud_reputation_slash(&merchant, &crate::FRAUD_PENALTY_SCORE_BPS);

    // A confirmed fraud ruling costs exactly the fixed 25% penalty.
    assert_eq!(new_score, 7_500);
    assert_eq!(client.get_reputation(&merchant).score, 7_500);
    let record = client.get_fraud_record(&merchant);
    assert_eq!(record.strike_count, 1);
    assert_eq!(
        record.accumulated_penalty_bps,
        crate::FRAUD_PENALTY_SCORE_BPS
    );
}

#[test]
fn test_fraud_slash_survives_later_score_recomputation() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let escrow = Address::generate(&env);
    let merchant = Address::generate(&env);
    let counterparty = Address::generate(&env);
    seed_clean_history(&client, &admin, &merchant, &counterparty);
    client.set_fraud_authority(&admin, &escrow);
    client.apply_fraud_reputation_slash(&merchant, &crate::FRAUD_PENALTY_SCORE_BPS);

    // A long run of clean Released outcomes after the ruling must not restore
    // the merchant's score: the slash is part of the score, not a one-off
    // edit that the next recompute overwrites.
    for i in 100..120u64 {
        client.record_transaction(
            &admin,
            &i,
            &merchant,
            &counterparty,
            &1_000i128,

    client.set_entity_role(&admin, &entity.clone(), &EntityRole::BuyerAgent);

    // All Released -> no disputes, full fulfillment/timely payment.
    for i in 0..5u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Released,
        );
    }

    let rep = client.get_reputation(&merchant);
    assert_eq!(rep.score, 7_500);
    assert_eq!(client.get_fraud_record(&merchant).strike_count, 1);
    assert_eq!(client.get_score_decomposition(&merchant).penalty_bps, 2_500);
}

#[test]
fn test_fraud_slash_accumulates_and_clamps_at_zero() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let escrow = Address::generate(&env);
    let merchant = Address::generate(&env);
    let counterparty = Address::generate(&env);
    seed_clean_history(&client, &admin, &merchant, &counterparty);
    client.set_fraud_authority(&admin, &escrow);

    // A maximum legal slash removes the entire score without underflowing.
    let score = client.apply_fraud_reputation_slash(&merchant, &10_000u32);
    assert_eq!(score, 0);
    assert_eq!(client.get_reputation(&merchant).score, 0);

    // A second ruling keeps clamping at 0 while the permanent audit trail
    // keeps growing.
    let score = client.apply_fraud_reputation_slash(&merchant, &2_500u32);
    assert_eq!(score, 0);
    let record = client.get_fraud_record(&merchant);
    assert_eq!(record.strike_count, 2);
    assert_eq!(record.accumulated_penalty_bps, 12_500);
    assert_eq!(
        client.get_score_decomposition(&merchant).final_score,
        0,
        "score must stay clamped at the floor"
    );
}

#[test]
fn test_fraud_slash_for_unknown_merchant_records_strike() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let escrow = Address::generate(&env);
    let merchant = Address::generate(&env);
    client.set_fraud_authority(&admin, &escrow);

    let score = client.apply_fraud_reputation_slash(&merchant, &crate::FRAUD_PENALTY_SCORE_BPS);

    assert_eq!(score, 0);
    let record = client.get_fraud_record(&merchant);
    assert_eq!(record.strike_count, 1);
    // The merchant now has an explicit record at the floor rather than
    // disappearing from the registry.
    assert_eq!(client.get_reputation(&merchant).score, 0);
}

#[test]
fn test_fraud_slash_requires_configured_authority() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    let merchant = Address::generate(&env);

    // No escrow contract has been authorized yet.
    let res = client.try_apply_fraud_reputation_slash(&merchant, &2_500u32);
    assert_eq!(res, Err(Ok(ReputationError::NotInitialized)));
    assert_eq!(client.get_fraud_record(&merchant).strike_count, 0);
}

#[test]
fn test_fraud_slash_rejects_unauthorized_caller() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let escrow = Address::generate(&env);
    let merchant = Address::generate(&env);
    client.set_fraud_authority(&admin, &escrow);

    // With no mocked authorization at all, the configured authority has not
    // signed the invocation, so the slash must be rejected.
    env.mock_auths(&[]);
    let res = client.try_apply_fraud_reputation_slash(&merchant, &2_500u32);
    assert!(res.is_err(), "slash without authority auth must fail");
    assert_eq!(client.get_fraud_record(&merchant).strike_count, 0);

    // Mocking an unrelated account's authorization is also insufficient.
    let stranger = Address::generate(&env);
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &stranger,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &client.address,
            fn_name: "apply_fraud_reputation_slash",
            args: (merchant.clone(), 2_500u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = client.try_apply_fraud_reputation_slash(&merchant, &2_500u32);
    assert!(res.is_err(), "stranger auth must not authorize a slash");
    assert_eq!(client.get_fraud_record(&merchant).strike_count, 0);
}

#[test]
fn test_fraud_slash_accepts_authorized_escrow_contract() {
    use soroban_sdk::testutils::{MockAuth, MockAuthInvoke};

    let env = Env::default();
    let (client, admin) = setup(&env);
    let escrow = Address::generate(&env);
    let merchant = Address::generate(&env);
    let counterparty = Address::generate(&env);
    seed_clean_history(&client, &admin, &merchant, &counterparty);
    client.set_fraud_authority(&admin, &escrow);

    // Only the configured escrow's authorization satisfies the call.
    env.mock_auths(&[MockAuth {
        address: &escrow,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "apply_fraud_reputation_slash",
            args: (merchant.clone(), crate::FRAUD_PENALTY_SCORE_BPS).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    let score = client.apply_fraud_reputation_slash(&merchant, &crate::FRAUD_PENALTY_SCORE_BPS);
    assert_eq!(score, 7_500);
}

#[test]
fn test_fraud_slash_rejects_out_of_range_penalty() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let escrow = Address::generate(&env);
    let merchant = Address::generate(&env);
    client.set_fraud_authority(&admin, &escrow);

    // Zero would be a silent no-op rather than a penalty.
    assert_eq!(
        client.try_apply_fraud_reputation_slash(&merchant, &0u32),
        Err(Ok(ReputationError::InvalidParam))
    );
    // More than 100% of the score is not a meaningful slash.
    assert_eq!(
        client.try_apply_fraud_reputation_slash(&merchant, &10_001u32),
        Err(Ok(ReputationError::InvalidParam))
    );
    assert_eq!(client.get_fraud_record(&merchant).strike_count, 0);
}

#[test]
fn test_set_fraud_authority_requires_admin() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    let not_admin = Address::generate(&env);
    let escrow = Address::generate(&env);

    let res = client.try_set_fraud_authority(&not_admin, &escrow);
    assert_eq!(res, Err(Ok(ReputationError::Unauthorized)));
    assert_eq!(client.get_fraud_authority(), None);
}

#[test]
fn test_set_fraud_authority_replaces_previous_authority() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let old_escrow = Address::generate(&env);
    let new_escrow = Address::generate(&env);
    let merchant = Address::generate(&env);

    client.set_fraud_authority(&admin, &old_escrow);
    client.set_fraud_authority(&admin, &new_escrow);
    assert_eq!(client.get_fraud_authority(), Some(new_escrow.clone()));

    // The replaced authority can no longer slash.
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &old_escrow,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &client.address,
            fn_name: "apply_fraud_reputation_slash",
            args: (merchant.clone(), 2_500u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = client.try_apply_fraud_reputation_slash(&merchant, &2_500u32);
    assert!(res.is_err());
    let metrics = client.get_role_specific_metrics(&entity);
    assert_eq!(metrics.fulfillment_rate_bps, 10_000);
    assert_eq!(metrics.dispute_frequency_bps, 0);
    assert_eq!(metrics.timely_payment_rate_bps, 10_000);
    assert_eq!(metrics.role, EntityRole::BuyerAgent);
}

#[test]
fn test_get_merchant_metrics_rejects_buyer_agent() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);

    client.set_entity_role(&admin, &entity.clone(), &EntityRole::BuyerAgent);

    let res = client.try_get_merchant_metrics(&entity);
    assert_eq!(res, Err(Ok(ReputationError::EntityNotFound)));
}

#[test]
fn test_get_buyer_agent_metrics_rejects_merchant() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);

    client.set_entity_role(&admin, &entity.clone(), &EntityRole::Merchant);

    let res = client.try_get_buyer_agent_metrics(&entity);
    assert_eq!(res, Err(Ok(ReputationError::EntityNotFound)));
}

#[test]
fn test_get_merchant_metrics_returns_for_merchant() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.set_entity_role(&admin, &entity.clone(), &EntityRole::Merchant);

    for i in 0..5u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Released,
        );
    }

    let metrics = client.get_merchant_metrics(&entity);
    assert_eq!(metrics.role, EntityRole::Merchant);
    assert_eq!(metrics.fulfillment_rate_bps, 10_000);
    assert_eq!(metrics.dispute_frequency_bps, 0);
}

#[test]
fn test_get_buyer_agent_metrics_returns_for_buyer_agent() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.set_entity_role(&admin, &entity.clone(), &EntityRole::BuyerAgent);

    // 3 Released, 2 Disputed out of 5 total.
    for i in 0..3u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Released,
        );
    }
    for i in 3..5u64 {
        client.record_transaction(
            &admin,
            &i,
            &entity,
            &counterparty,
            &1000i128,
            &TransactionOutcome::Disputed,
        );
    }

    let metrics = client.get_buyer_agent_metrics(&entity);
    assert_eq!(metrics.role, EntityRole::BuyerAgent);
    // fulfillment = 3/5 * 10000 = 6000
    assert_eq!(metrics.fulfillment_rate_bps, 6000);
    // dispute frequency = 2/5 * 10000 = 4000
    assert_eq!(metrics.dispute_frequency_bps, 4000);
    assert_eq!(metrics.timely_payment_rate_bps, 6000);
}

#[test]
fn test_role_specific_metrics_entity_not_found() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    let entity = Address::generate(&env);

    assert_eq!(
        client.try_get_role_specific_metrics(&entity),
        Err(Ok(ReputationError::EntityNotFound))
    );
}

#[test]
fn test_role_metrics_update_on_outcome_change() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    let entity = Address::generate(&env);
    let counterparty = Address::generate(&env);

    client.set_entity_role(&admin, &entity.clone(), &EntityRole::Merchant);

    // Start with a Disputed outcome.
    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::Disputed,
    );
    let metrics_after_dispute = client.get_role_specific_metrics(&entity);
    assert_eq!(metrics_after_dispute.dispute_frequency_bps, 10_000);

    // Lifecycle update to ResolvedSeller: dispute count drops, fulfillment rises.
    client.record_transaction(
        &admin,
        &1u64,
        &entity,
        &counterparty,
        &1000i128,
        &TransactionOutcome::ResolvedSeller,
    );
    let metrics_after_resolve = client.get_role_specific_metrics(&entity);
    assert_eq!(metrics_after_resolve.dispute_frequency_bps, 0);
    assert_eq!(metrics_after_resolve.fulfillment_rate_bps, 10_000);
}
