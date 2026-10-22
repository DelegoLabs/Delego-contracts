//! Tests for the merchant review appeal mechanism (issue #583).

use crate::{
    AppealStatus, DataKey, ReputationConfig, ReputationContract, ReputationContractClient,
    ReputationError, ReviewAppeal, TransactionOutcome, TransactionRecord,
};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env, Vec,
};

/// Bond a merchant has to stake to file an appeal.
const BOND: i128 = 1_000_000;
/// Number of arbiter votes that resolve an appeal.
const QUORUM: u32 = 2;
/// Length of the voting window, in ledgers.
const WINDOW: u64 = 100;

fn default_config() -> ReputationConfig {
    ReputationConfig {
        decay_window_seconds: 90 * 24 * 60 * 60,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 500,
        freeze_threshold_flags: 3,
    }
}

struct Setup {
    client: ReputationContractClient<'static>,
    contract_id: Address,
    admin: Address,
    merchant: Address,
    reviewer: Address,
    arbiter_a: Address,
    arbiter_b: Address,
    charity: Address,
    token: Address,
}

fn setup(env: &Env) -> Setup {
    env.mock_all_auths();
    let admin = Address::generate(env);
    let contract_id = env.register(ReputationContract, (admin.clone(), default_config()));
    let client = ReputationContractClient::new(env, &contract_id);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(env))
        .address();

    let merchant = Address::generate(env);
    let reviewer = Address::generate(env);
    let arbiter_a = Address::generate(env);
    let arbiter_b = Address::generate(env);
    let charity = Address::generate(env);

    // The merchant has to be able to pay the bond.
    StellarAssetClient::new(env, &token).mint(&merchant, &(BOND * 4));

    Setup {
        client,
        contract_id,
        admin,
        merchant,
        reviewer,
        arbiter_a,
        arbiter_b,
        charity,
        token,
    }
}

/// Configures the appeal mechanism with the standard test parameters.
fn configure(s: &Setup) {
    s.client
        .configure_appeals(&s.admin, &s.token, &BOND, &QUORUM, &WINDOW, &s.charity);
    s.client.register_arbiter(&s.admin, &s.arbiter_a);
    s.client.register_arbiter(&s.admin, &s.arbiter_b);
}

/// Writes a review commitment for `merchant` directly into storage.
fn write_review(env: &Env, s: &Setup, review_id: u64, merchant: &Address, rating: u32) {
    let commitment = crate::ReviewCommitment {
        escrow_id: review_id,
        reviewer: Address::generate(env),
        merchant: merchant.clone(),
        rating,
        content_hash: BytesN::from_array(env, &[review_id as u8; 32]),
        submitted_ledger: env.ledger().sequence(),
    };
    env.as_contract(&s.contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::ReviewCommitment(review_id), &commitment);

        let key = DataKey::MerchantReviewCommitments(merchant.clone());
        let mut ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(env));
        ids.push_back(review_id);
        env.storage().persistent().set(&key, &ids);
    });
}

/// Writes a rated transaction for `merchant` directly into storage.
fn write_rated_transaction(env: &Env, s: &Setup, review_id: u64, merchant: &Address, rating: u32) {
    let record = TransactionRecord {
        escrow_id: review_id,
        entity: merchant.clone(),
        counterparty: Address::generate(env),
        amount: 1_000,
        outcome: TransactionOutcome::Released,
        rating: Some(rating),
        recorded_at: env.ledger().timestamp(),
    };
    env.as_contract(&s.contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::TransactionRecord(review_id), &record);

        let key = DataKey::TransactionHistory(merchant.clone());
        let mut ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(env));
        ids.push_back(review_id);
        env.storage().persistent().set(&key, &ids);
    });
}

/// Attaches a rating to an already recorded transaction.
fn set_rating(env: &Env, s: &Setup, review_id: u64, rating: u32) {
    env.as_contract(&s.contract_id, || {
        let key = DataKey::TransactionRecord(review_id);
        let mut record: TransactionRecord = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("missing transaction record"));
        record.rating = Some(rating);
        env.storage().persistent().set(&key, &record);
    });
}

fn balance(env: &Env, token: &Address, address: &Address) -> i128 {
    TokenClient::new(env, token).balance(address)
}

fn evidence(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[9; 32])
}

#[test]
fn configure_appeals_requires_admin() {
    let env = Env::default();
    let s = setup(&env);
    let stranger = Address::generate(&env);

    assert_eq!(
        s.client
            .try_configure_appeals(&stranger, &s.token, &BOND, &QUORUM, &WINDOW, &s.charity),
        Err(Ok(ReputationError::Unauthorized))
    );
}

#[test]
fn configure_appeals_rejects_invalid_parameters() {
    let env = Env::default();
    let s = setup(&env);

    assert_eq!(
        s.client
            .try_configure_appeals(&s.admin, &s.token, &0i128, &QUORUM, &WINDOW, &s.charity),
        Err(Ok(ReputationError::InvalidAppealBond))
    );
    assert_eq!(
        s.client
            .try_configure_appeals(&s.admin, &s.token, &BOND, &0u32, &WINDOW, &s.charity),
        Err(Ok(ReputationError::InvalidParam))
    );
    assert_eq!(
        s.client
            .try_configure_appeals(&s.admin, &s.token, &BOND, &QUORUM, &0u64, &s.charity),
        Err(Ok(ReputationError::InvalidParam))
    );
}

#[test]
fn file_appeal_escrows_the_bond_tokens() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    let merchant_balance_before = balance(&env, &s.token, &s.merchant);
    let appeal = s
        .client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));

    assert_eq!(appeal.review_id, 1);
    assert_eq!(appeal.merchant, s.merchant);
    assert_eq!(appeal.appeal_bond_stroops, BOND);
    assert_eq!(appeal.status, AppealStatus::Pending);

    // The bond really left the merchant's wallet and sits in the contract.
    assert_eq!(
        balance(&env, &s.token, &s.merchant),
        merchant_balance_before - BOND
    );
    assert_eq!(balance(&env, &s.token, &s.client.address), BOND);
    assert_eq!(s.client.get_escrowed_appeal_bond(&s.merchant), BOND);
    assert_eq!(s.client.get_escrowed_bond_balance(), BOND);
}

#[test]
fn file_appeal_rejects_a_bond_that_does_not_match_the_configured_one() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    assert_eq!(
        s.client
            .try_file_appeal(&s.merchant, &1u64, &(BOND - 1), &evidence(&env)),
        Err(Ok(ReputationError::AppealBondMismatch))
    );
    assert_eq!(
        s.client
            .try_file_appeal(&s.merchant, &1u64, &0i128, &evidence(&env)),
        Err(Ok(ReputationError::InvalidAppealBond))
    );
}

#[test]
fn file_appeal_requires_an_existing_review() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);

    assert_eq!(
        s.client
            .try_file_appeal(&s.merchant, &42u64, &BOND, &evidence(&env)),
        Err(Ok(ReputationError::ReviewNotFound))
    );
}

#[test]
fn file_appeal_rejects_the_review_of_another_merchant() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    let other_merchant = Address::generate(&env);
    assert_eq!(
        s.client
            .try_file_appeal(&other_merchant, &1u64, &BOND, &evidence(&env)),
        Err(Ok(ReputationError::AppealMerchantMismatch))
    );
}

#[test]
fn a_review_can_only_be_appealed_once_while_the_appeal_is_pending() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));

    assert_eq!(
        s.client
            .try_file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env)),
        Err(Ok(ReputationError::ReviewAppealAlreadyExists))
    );
}

#[test]
fn approval_quorum_hides_the_review_and_returns_the_bond() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    let merchant_balance_before = balance(&env, &s.token, &s.merchant);
    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));

    s.client
        .vote_appeal(&s.arbiter_a, &1u64, &symbol_short!("approve"));
    let outcome = s
        .client
        .vote_appeal(&s.arbiter_b, &1u64, &symbol_short!("approve"));

    assert_eq!(outcome.appeal.status, AppealStatus::Approved);
    assert_eq!(outcome.approvals, 2);
    assert!(outcome.hidden);

    // The review is hidden.
    assert!(s.client.is_review_hidden(&1u64));
    assert!(s.client.get_visible_review_ids(&s.merchant).is_empty());
    assert_eq!(
        s.client.get_hidden_review_ids(&s.merchant),
        soroban_sdk::vec![&env, 1u64]
    );

    // The bond is back with the merchant.
    assert_eq!(
        balance(&env, &s.token, &s.merchant),
        merchant_balance_before
    );
    assert_eq!(balance(&env, &s.token, &s.client.address), 0);
    assert_eq!(s.client.get_escrowed_appeal_bond(&s.merchant), 0);
    assert_eq!(s.client.get_charity_balance(&s.charity), 0);
}

#[test]
fn approval_removes_the_false_review_from_the_average_rating() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);

    // Five settled transactions, so the merchant's score is past the
    // visibility threshold and its average rating is not masked.
    for id in 0u64..5u64 {
        s.client.record_transaction(
            &s.admin,
            &id,
            &s.merchant,
            &s.reviewer,
            &1_000i128,
            &TransactionOutcome::Released,
        );
    }

    // One honest 5-star review (escrow 1) and one fraudulent 1-star
    // review (escrow 2).
    set_rating(&env, &s, 1, 10_000);
    set_rating(&env, &s, 2, 100);

    // A fraud slash forces a recomputation of the merchant's score, which
    // is also what makes the current rating pool observable: both reviews
    // still count.
    s.client.set_fraud_authority(&s.admin, &s.admin);
    s.client.apply_fraud_reputation_slash(&s.merchant, &1u32);
    assert_eq!(s.client.get_reputation(&s.merchant).avg_rating, 5_050);

    s.client
        .file_appeal(&s.merchant, &2u64, &BOND, &evidence(&env));
    s.client
        .vote_appeal(&s.arbiter_a, &2u64, &symbol_short!("approve"));
    s.client
        .vote_appeal(&s.arbiter_b, &2u64, &symbol_short!("approve"));

    // The fraudulent review no longer counts towards the average rating.
    let after = s.client.get_reputation(&s.merchant);
    assert_eq!(after.avg_rating, 10_000);
    assert_eq!(after.total_transactions, 5);
}

#[test]
fn denial_quorum_forfeits_the_bond_to_the_charity() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    let merchant_balance_before = balance(&env, &s.token, &s.merchant);
    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));

    s.client
        .vote_appeal(&s.arbiter_a, &1u64, &symbol_short!("deny"));
    let outcome = s
        .client
        .vote_appeal(&s.arbiter_b, &1u64, &symbol_short!("deny"));

    assert_eq!(outcome.appeal.status, AppealStatus::Denied);
    assert_eq!(outcome.rejections, 2);
    assert!(!outcome.hidden);

    // The bond is forfeited to the charity.
    assert_eq!(
        balance(&env, &s.token, &s.merchant),
        merchant_balance_before - BOND
    );
    assert_eq!(balance(&env, &s.token, &s.charity), BOND);
    assert_eq!(balance(&env, &s.token, &s.client.address), 0);
    assert_eq!(s.client.get_escrowed_appeal_bond(&s.merchant), 0);
    assert_eq!(s.client.get_charity_balance(&s.charity), BOND);

    // The review stays visible.
    assert!(!s.client.is_review_hidden(&1u64));
    assert_eq!(
        s.client.get_visible_review_ids(&s.merchant),
        soroban_sdk::vec![&env, 1u64]
    );
}

#[test]
fn an_unregistered_arbiter_cannot_vote() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));

    let stranger = Address::generate(&env);
    assert_eq!(
        s.client
            .try_vote_appeal(&stranger, &1u64, &symbol_short!("approve")),
        Err(Ok(ReputationError::ArbiterNotRegistered))
    );
}

#[test]
fn an_invalid_verdict_is_rejected() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));

    assert_eq!(
        s.client
            .try_vote_appeal(&s.arbiter_a, &1u64, &symbol_short!("maybe")),
        Err(Ok(ReputationError::InvalidAppealVerdict))
    );
}

#[test]
fn an_arbiter_cannot_vote_twice() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));
    s.client
        .vote_appeal(&s.arbiter_a, &1u64, &symbol_short!("approve"));

    assert_eq!(
        s.client
            .try_vote_appeal(&s.arbiter_a, &1u64, &symbol_short!("deny")),
        Err(Ok(ReputationError::AppealAlreadyVoted))
    );
}

#[test]
fn voting_on_an_unknown_appeal_fails() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);

    assert_eq!(
        s.client
            .try_vote_appeal(&s.arbiter_a, &7u64, &symbol_short!("approve")),
        Err(Ok(ReputationError::ReviewAppealNotFound))
    );
}

#[test]
fn a_vote_after_the_window_expired_is_rejected() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + WINDOW as u32 + 1);

    assert_eq!(
        s.client
            .try_vote_appeal(&s.arbiter_a, &1u64, &symbol_short!("approve")),
        Err(Ok(ReputationError::AppealWindowExpired))
    );
}

#[test]
fn withdrawing_an_appeal_returns_the_bond() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    let merchant_balance_before = balance(&env, &s.token, &s.merchant);
    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));

    let outcome = s.client.withdraw_appeal(&s.merchant, &1u64);
    assert_eq!(outcome.appeal.status, AppealStatus::Withdrawn);

    assert_eq!(
        balance(&env, &s.token, &s.merchant),
        merchant_balance_before
    );
    assert_eq!(balance(&env, &s.token, &s.client.address), 0);
    assert_eq!(s.client.get_escrowed_appeal_bond(&s.merchant), 0);
}

#[test]
fn only_the_merchant_can_withdraw_an_appeal() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));

    let stranger = Address::generate(&env);
    assert_eq!(
        s.client.try_withdraw_appeal(&stranger, &1u64),
        Err(Ok(ReputationError::NotAppealParty))
    );
}

#[test]
fn an_appeal_cannot_be_withdrawn_once_an_arbiter_voted() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));
    s.client
        .vote_appeal(&s.arbiter_a, &1u64, &symbol_short!("deny"));

    assert_eq!(
        s.client.try_withdraw_appeal(&s.merchant, &1u64),
        Err(Ok(ReputationError::AppealAlreadyVoted))
    );
}

#[test]
fn an_expired_appeal_returns_the_bond() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    let merchant_balance_before = balance(&env, &s.token, &s.merchant);
    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));

    env.ledger()
        .set_sequence_number(env.ledger().sequence() + WINDOW as u32 + 1);

    // Anybody may clean up an expired appeal.
    let outcome = s.client.expire_appeal(&s.reviewer, &1u64);
    assert_eq!(outcome.appeal.status, AppealStatus::Expired);

    assert_eq!(
        balance(&env, &s.token, &s.merchant),
        merchant_balance_before
    );
    assert_eq!(balance(&env, &s.token, &s.client.address), 0);
    assert_eq!(s.client.get_escrowed_appeal_bond(&s.merchant), 0);
}

#[test]
fn an_appeal_cannot_be_expired_while_the_window_is_open() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));

    assert_eq!(
        s.client.try_expire_appeal(&s.reviewer, &1u64),
        Err(Ok(ReputationError::AppealWindowNotExpired))
    );
}

#[test]
fn a_withdrawn_appeal_can_be_filed_again() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));
    s.client.withdraw_appeal(&s.merchant, &1u64);

    let appeal = s
        .client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));
    assert_eq!(appeal.status, AppealStatus::Pending);
    assert_eq!(balance(&env, &s.token, &s.client.address), BOND);
}

#[test]
fn the_votes_of_a_previous_round_do_not_count_towards_the_new_quorum() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    // The first round gets a single approval, but never reaches the quorum.
    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));
    s.client
        .vote_appeal(&s.arbiter_a, &1u64, &symbol_short!("approve"));
    assert_eq!(s.client.get_appeal_votes(&1u64).len(), 1);

    env.ledger()
        .set_sequence_number(env.ledger().sequence() + WINDOW as u32 + 1);
    s.client.expire_appeal(&s.reviewer, &1u64);

    // The merchant re-files: the second round starts from a clean slate.
    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));
    assert!(s.client.get_appeal_votes(&1u64).is_empty());

    // One approval is no longer enough to resolve the appeal.
    let outcome = s
        .client
        .vote_appeal(&s.arbiter_a, &1u64, &symbol_short!("approve"));
    assert_eq!(outcome.appeal.status, AppealStatus::Pending);
    assert_eq!(outcome.approvals, 1);
    assert!(!s.client.is_review_hidden(&1u64));

    // The second vote reaches the quorum and resolves the appeal.
    let outcome = s
        .client
        .vote_appeal(&s.arbiter_b, &1u64, &symbol_short!("approve"));
    assert_eq!(outcome.appeal.status, AppealStatus::Approved);
    assert!(s.client.is_review_hidden(&1u64));
}

#[test]
fn a_denied_appeal_cannot_be_filed_again() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));
    s.client
        .vote_appeal(&s.arbiter_a, &1u64, &symbol_short!("deny"));
    s.client
        .vote_appeal(&s.arbiter_b, &1u64, &symbol_short!("deny"));

    assert_eq!(
        s.client
            .try_file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env)),
        Err(Ok(ReputationError::ReviewAppealAlreadyExists))
    );
}

#[test]
fn multiple_appeals_escrow_their_bonds_separately() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);
    write_review(&env, &s, 2, &s.merchant, 500);

    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));
    s.client
        .file_appeal(&s.merchant, &2u64, &BOND, &evidence(&env));

    assert_eq!(s.client.get_escrowed_appeal_bond(&s.merchant), BOND * 2);
    assert_eq!(balance(&env, &s.token, &s.client.address), BOND * 2);

    // Resolving one appeal returns only that bond.
    s.client
        .vote_appeal(&s.arbiter_a, &1u64, &symbol_short!("approve"));
    s.client
        .vote_appeal(&s.arbiter_b, &1u64, &symbol_short!("approve"));

    assert_eq!(s.client.get_escrowed_appeal_bond(&s.merchant), BOND);
    assert_eq!(balance(&env, &s.token, &s.client.address), BOND);
}

#[test]
fn a_appeal_can_target_a_rated_transaction() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_rated_transaction(&env, &s, 5, &s.merchant, 100);

    let appeal = s
        .client
        .file_appeal(&s.merchant, &5u64, &BOND, &evidence(&env));
    assert_eq!(appeal.review_id, 5);
    assert_eq!(appeal.merchant, s.merchant);
}

#[test]
fn a_transaction_without_a_rating_is_not_appealable() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);

    let record = TransactionRecord {
        escrow_id: 6,
        entity: s.merchant.clone(),
        counterparty: Address::generate(&env),
        amount: 1_000,
        outcome: TransactionOutcome::Released,
        rating: None,
        recorded_at: env.ledger().timestamp(),
    };
    env.as_contract(&s.contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::TransactionRecord(6u64), &record);
    });

    assert_eq!(
        s.client
            .try_file_appeal(&s.merchant, &6u64, &BOND, &evidence(&env)),
        Err(Ok(ReputationError::ReviewNotFound))
    );
}

#[test]
fn the_appeal_record_is_readable() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    let evidence_hash = evidence(&env);
    let appeal = s
        .client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence_hash);

    let stored: Option<ReviewAppeal> = s.client.get_appeal(&1u64);
    assert_eq!(stored, Some(appeal.clone()));
    assert_eq!(stored.unwrap().justification_hash, evidence_hash);

    assert!(s.client.get_appeal_votes(&1u64).is_empty());

    s.client
        .vote_appeal(&s.arbiter_a, &1u64, &symbol_short!("approve"));
    assert_eq!(s.client.get_appeal_votes(&1u64).len(), 1);

    let outcome = s.client.get_appeal_outcome(&1u64).unwrap();
    assert_eq!(outcome.approvals, 1);
    assert_eq!(outcome.total_votes, 1);
}

#[test]
fn the_bond_token_cannot_be_swapped_while_a_bond_is_outstanding() {
    let env = Env::default();
    let s = setup(&env);
    configure(&s);
    write_review(&env, &s, 1, &s.merchant, 500);

    s.client
        .file_appeal(&s.merchant, &1u64, &BOND, &evidence(&env));

    let other_token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    assert_eq!(
        s.client
            .try_configure_appeals(&s.admin, &other_token, &BOND, &QUORUM, &WINDOW, &s.charity),
        Err(Ok(ReputationError::InvalidParam))
    );
}
