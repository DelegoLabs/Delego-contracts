use crate::*;
use soroban_sdk::{testutils::Address as _, testutils::Ledger as _, Address, BytesN, Env, Vec};

const BOND: i128 = 500_000_000; // 50 USDC in stroops
const QUORUM: u32 = 3;
const WINDOW: u64 = 17_280; // ~1 day at 5s/ledger

fn setup(env: &Env) -> (ReputationContractClient<'static>, Address) {
    env.mock_all_auths();

    let admin = Address::generate(env);
    let config = ReputationConfig {
        decay_window_seconds: 2_592_000,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 1_000,
        freeze_threshold_flags: 3,
    };
    let contract_id = env.register(ReputationContract, (admin.clone(), config));
    let client = ReputationContractClient::new(env, &contract_id);

    (client, admin)
}

/// Configures the appeal mechanism with the standard test parameters.
fn configure(client: &ReputationContractClient<'static>, admin: &Address, charity: &Address) {
    client.configure_appeals(admin, &BOND, &QUORUM, &WINDOW, charity);
}

/// Writes a review commitment for `merchant` directly into storage.
///
/// `submit_review_commitment` needs a live escrow contract to verify the
/// settlement, which is out of scope here: the appeal mechanism only needs
/// the review record itself, so the test seeds it directly.
fn seed_review(
    env: &Env,
    client: &ReputationContractClient<'static>,
    review_id: u64,
    merchant: &Address,
) {
    let commitment = ReviewCommitment {
        escrow_id: review_id,
        reviewer: Address::generate(env),
        merchant: merchant.clone(),
        rating: 1,
        content_hash: BytesN::<32>::from_array(env, &[7u8; 32]),
        submitted_ledger: env.ledger().sequence(),
    };

    let contract_id = client.address.clone();
    env.as_contract(&contract_id, || {
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

fn justification(env: &Env) -> BytesN<32> {
    BytesN::<32>::from_array(env, &[9u8; 32])
}

#[test]
fn test_configure_appeals_requires_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);
    let stranger = Address::generate(&env);

    let result = client.try_configure_appeals(&stranger, &BOND, &QUORUM, &WINDOW, &charity);
    assert_eq!(result, Err(Ok(ReputationError::Unauthorized)));

    let config = client.configure_appeals(&admin, &BOND, &QUORUM, &WINDOW, &charity);
    assert_eq!(config.bond_stroops, BOND);
    assert_eq!(config.quorum, QUORUM);
    assert_eq!(config.window_ledgers, WINDOW);
    assert_eq!(config.charity, charity);
    assert_eq!(client.get_appeal_config(), Some(config));
}

#[test]
fn test_configure_appeals_rejects_invalid_parameters() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);

    assert_eq!(
        client.try_configure_appeals(&admin, &0, &QUORUM, &WINDOW, &charity),
        Err(Ok(ReputationError::InvalidAppealBond))
    );
    assert_eq!(
        client.try_configure_appeals(&admin, &BOND, &0, &WINDOW, &charity),
        Err(Ok(ReputationError::InvalidParam))
    );
    assert_eq!(
        client.try_configure_appeals(&admin, &BOND, &QUORUM, &0, &charity),
        Err(Ok(ReputationError::InvalidParam))
    );
}

#[test]
fn test_file_appeal_escrows_bond() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);
    let merchant = Address::generate(&env);
    configure(&client, &admin, &charity);

    seed_review(&env, &client, 42, &merchant);

    let appeal = client.file_appeal(&merchant, &42, &BOND, &justification(&env));
    assert_eq!(appeal.review_id, 42);
    assert_eq!(appeal.merchant, merchant);
    assert_eq!(appeal.appeal_bond_stroops, BOND);
    assert_eq!(appeal.status, AppealStatus::Pending);

    // The bond is locked in escrow for the duration of the voting window.
    assert_eq!(client.get_escrowed_appeal_bond(&merchant), BOND);

    // And the appeal is publicly readable.
    let stored = client.get_appeal(&42).expect("appeal should be stored");
    assert_eq!(stored, appeal);
    let outcome = client
        .get_appeal_outcome(&42)
        .expect("outcome should exist");
    assert_eq!(outcome.approvals, 0);
    assert_eq!(outcome.rejections, 0);
    assert_eq!(outcome.total_votes, 0);
    assert!(!outcome.hidden);
}

#[test]
fn test_file_appeal_rejects_invalid_bond() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);
    let merchant = Address::generate(&env);
    configure(&client, &admin, &charity);

    seed_review(&env, &client, 42, &merchant);

    assert_eq!(
        client.try_file_appeal(&merchant, &42, &0, &justification(&env)),
        Err(Ok(ReputationError::InvalidAppealBond))
    );
    assert_eq!(
        client.try_file_appeal(&merchant, &42, &(BOND * 2), &justification(&env)),
        Err(Ok(ReputationError::AppealBondMismatch))
    );
    // Nothing was escrowed by the rejected filings.
    assert_eq!(client.get_escrowed_appeal_bond(&merchant), 0);
}

#[test]
fn test_file_appeal_requires_existing_review() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);
    let merchant = Address::generate(&env);
    configure(&client, &admin, &charity);

    assert_eq!(
        client.try_file_appeal(&merchant, &404, &BOND, &justification(&env)),
        Err(Ok(ReputationError::ReviewNotFound))
    );
}

#[test]
fn test_file_appeal_rejects_other_merchants_review() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);
    let merchant = Address::generate(&env);
    let other = Address::generate(&env);
    configure(&client, &admin, &charity);

    seed_review(&env, &client, 42, &merchant);

    assert_eq!(
        client.try_file_appeal(&other, &42, &BOND, &justification(&env)),
        Err(Ok(ReputationError::AppealMerchantMismatch))
    );
}

#[test]
fn test_file_appeal_only_once_per_review() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);
    let merchant = Address::generate(&env);
    configure(&client, &admin, &charity);

    seed_review(&env, &client, 42, &merchant);
    client.file_appeal(&merchant, &42, &BOND, &justification(&env));

    assert_eq!(
        client.try_file_appeal(&merchant, &42, &BOND, &justification(&env)),
        Err(Ok(ReputationError::ReviewAppealAlreadyExists))
    );
    // Only the first bond is escrowed.
    assert_eq!(client.get_escrowed_appeal_bond(&merchant), BOND);
}

#[test]
fn test_approval_quorum_hides_review_and_refunds_bond() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);
    let merchant = Address::generate(&env);
    let arbiters = [
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    ];
    configure(&client, &admin, &charity);
    for arbiter in arbiters.iter() {
        client.register_arbiter(&admin, arbiter);
    }

    seed_review(&env, &client, 42, &merchant);
    client.file_appeal(&merchant, &42, &BOND, &justification(&env));

    // Two approvals are not enough for the quorum of three.
    client.vote_appeal(&arbiters[0], &42, &symbol_short!("approve"));
    let outcome = client.vote_appeal(&arbiters[1], &42, &symbol_short!("approve"));
    assert_eq!(outcome.approvals, 2);
    assert_eq!(outcome.appeal.status, AppealStatus::Pending);
    assert!(!client.is_review_hidden(&42));
    // The bond stays escrowed while the appeal is pending.
    assert_eq!(client.get_escrowed_appeal_bond(&merchant), BOND);

    // The third approval resolves the appeal.
    let outcome = client.vote_appeal(&arbiters[2], &42, &symbol_short!("approve"));
    assert_eq!(outcome.approvals, 3);
    assert_eq!(outcome.appeal.status, AppealStatus::Approved);
    assert!(outcome.hidden);

    // The review is hidden and the bond went back to the merchant.
    assert!(client.is_review_hidden(&42));
    assert_eq!(client.get_escrowed_appeal_bond(&merchant), 0);
    assert_eq!(client.get_charity_balance(&charity), 0);

    // The hidden review disappears from the merchant's visible reviews.
    let visible = client.get_visible_review_ids(&merchant);
    assert_eq!(visible.len(), 0);
}

#[test]
fn test_denial_quorum_forfeits_bond_to_charity() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);
    let merchant = Address::generate(&env);
    let arbiters = [
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    ];
    configure(&client, &admin, &charity);
    for arbiter in arbiters.iter() {
        client.register_arbiter(&admin, arbiter);
    }

    seed_review(&env, &client, 42, &merchant);
    client.file_appeal(&merchant, &42, &BOND, &justification(&env));

    client.vote_appeal(&arbiters[0], &42, &symbol_short!("deny"));
    client.vote_appeal(&arbiters[1], &42, &symbol_short!("deny"));
    let outcome = client.vote_appeal(&arbiters[2], &42, &symbol_short!("deny"));

    assert_eq!(outcome.rejections, 3);
    assert_eq!(outcome.appeal.status, AppealStatus::Denied);

    // The review stays visible and the bond is forfeited to the charity.
    assert!(!client.is_review_hidden(&42));
    assert_eq!(client.get_escrowed_appeal_bond(&merchant), 0);
    assert_eq!(client.get_charity_balance(&charity), BOND);

    let visible = client.get_visible_review_ids(&merchant);
    assert_eq!(visible.len(), 1);
}

#[test]
fn test_arbiter_cannot_vote_twice() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);
    let merchant = Address::generate(&env);
    let arbiter = Address::generate(&env);
    configure(&client, &admin, &charity);
    client.register_arbiter(&admin, &arbiter);

    seed_review(&env, &client, 42, &merchant);
    client.file_appeal(&merchant, &42, &BOND, &justification(&env));

    client.vote_appeal(&arbiter, &42, &symbol_short!("approve"));
    assert_eq!(
        client.try_vote_appeal(&arbiter, &42, &symbol_short!("approve")),
        Err(Ok(ReputationError::AppealAlreadyVoted))
    );
    assert_eq!(client.get_appeal_votes(&42).len(), 1);
}

#[test]
fn test_unregistered_arbiter_cannot_vote() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);
    let merchant = Address::generate(&env);
    let stranger = Address::generate(&env);
    configure(&client, &admin, &charity);

    seed_review(&env, &client, 42, &merchant);
    client.file_appeal(&merchant, &42, &BOND, &justification(&env));

    assert_eq!(
        client.try_vote_appeal(&stranger, &42, &symbol_short!("approve")),
        Err(Ok(ReputationError::ArbiterNotRegistered))
    );
    assert!(!client.is_arbiter(&stranger));
}

#[test]
fn test_invalid_verdict_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);
    let merchant = Address::generate(&env);
    let arbiter = Address::generate(&env);
    configure(&client, &admin, &charity);
    client.register_arbiter(&admin, &arbiter);

    seed_review(&env, &client, 42, &merchant);
    client.file_appeal(&merchant, &42, &BOND, &justification(&env));

    assert_eq!(
        client.try_vote_appeal(&arbiter, &42, &symbol_short!("maybe")),
        Err(Ok(ReputationError::InvalidAppealVerdict))
    );
    assert_eq!(client.get_appeal_votes(&42).len(), 0);
}

#[test]
fn test_voting_on_unknown_appeal_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);
    let arbiter = Address::generate(&env);
    configure(&client, &admin, &charity);
    client.register_arbiter(&admin, &arbiter);

    assert_eq!(
        client.try_vote_appeal(&arbiter, &404, &symbol_short!("approve")),
        Err(Ok(ReputationError::ReviewAppealNotFound))
    );
}

#[test]
fn test_vote_after_window_expired_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);
    let merchant = Address::generate(&env);
    let arbiter = Address::generate(&env);
    configure(&client, &admin, &charity);
    client.register_arbiter(&admin, &arbiter);

    seed_review(&env, &client, 42, &merchant);
    client.file_appeal(&merchant, &42, &BOND, &justification(&env));

    // Move past the voting window.
    env.ledger().set_sequence_number(WINDOW as u32 + 1);

    assert_eq!(
        client.try_vote_appeal(&arbiter, &42, &symbol_short!("approve")),
        Err(Ok(ReputationError::AppealWindowExpired))
    );
    assert_eq!(client.get_appeal_votes(&42).len(), 0);
}

#[test]
fn test_withdraw_appeal_refunds_bond() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);
    let merchant = Address::generate(&env);
    let stranger = Address::generate(&env);
    configure(&client, &admin, &charity);

    seed_review(&env, &client, 42, &merchant);
    client.file_appeal(&merchant, &42, &BOND, &justification(&env));

    // Only the merchant that filed the appeal may withdraw it.
    assert_eq!(
        client.try_withdraw_appeal(&stranger, &42),
        Err(Ok(ReputationError::NotAppealParty))
    );

    let outcome = client.withdraw_appeal(&merchant, &42);
    assert_eq!(outcome.appeal.status, AppealStatus::Withdrawn);
    assert_eq!(client.get_escrowed_appeal_bond(&merchant), 0);
    assert!(!client.is_review_hidden(&42));

    // A withdrawn appeal cannot be voted on anymore.
    let arbiter = Address::generate(&env);
    client.register_arbiter(&admin, &arbiter);
    assert_eq!(
        client.try_vote_appeal(&arbiter, &42, &symbol_short!("approve")),
        Err(Ok(ReputationError::AppealAlreadyResolved))
    );
}

#[test]
fn test_expire_appeal_refunds_bond() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);
    let merchant = Address::generate(&env);
    let caller = Address::generate(&env);
    configure(&client, &admin, &charity);

    seed_review(&env, &client, 42, &merchant);
    client.file_appeal(&merchant, &42, &BOND, &justification(&env));

    // The window is still open.
    assert_eq!(
        client.try_expire_appeal(&caller, &42),
        Err(Ok(ReputationError::AppealWindowNotExpired))
    );

    // Move past the voting window and let anybody clean the appeal up.
    env.ledger().set_sequence_number(WINDOW as u32 + 1);
    let outcome = client.expire_appeal(&caller, &42);
    assert_eq!(outcome.appeal.status, AppealStatus::Expired);
    assert_eq!(client.get_escrowed_appeal_bond(&merchant), 0);
    assert_eq!(client.get_charity_balance(&charity), 0);
    assert!(!client.is_review_hidden(&42));
}

#[test]
fn test_multiple_appeals_escrow_bonds_separately() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin) = setup(&env);
    let charity = Address::generate(&env);
    let merchant = Address::generate(&env);
    configure(&client, &admin, &charity);

    seed_review(&env, &client, 1, &merchant);
    seed_review(&env, &client, 2, &merchant);

    client.file_appeal(&merchant, &1, &BOND, &justification(&env));
    client.file_appeal(&merchant, &2, &BOND, &justification(&env));
    assert_eq!(client.get_escrowed_appeal_bond(&merchant), BOND * 2);

    // Withdrawing one appeal only refunds that appeal's bond.
    client.withdraw_appeal(&merchant, &1);
    assert_eq!(client.get_escrowed_appeal_bond(&merchant), BOND);
}
