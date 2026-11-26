//! End-to-end test for storefront review attestations across the real
//! marketplace and reputation contracts (issue #558).

use delego_marketplace::{MarketplaceContract, MarketplaceContractClient};
use delego_reputation::{
    ReputationConfig, ReputationContract, ReputationContractClient, ReputationError,
};
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

fn config() -> ReputationConfig {
    ReputationConfig {
        decay_window_seconds: 90 * 24 * 60 * 60,
        min_transactions_threshold: 1,
        dispute_penalty_bps: 500,
        freeze_threshold_flags: 3,
    }
}

#[test]
fn escrow_attestation_backs_exactly_one_reputation_review() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let marketplace_id = env.register(MarketplaceContract, (admin.clone(),));
    let marketplace = MarketplaceContractClient::new(&env, &marketplace_id);

    let escrow = Address::generate(&env);
    marketplace.set_authorized_escrow(&admin, &Some(escrow.clone()));

    let rep_admin = Address::generate(&env);
    let reputation_id = env.register(ReputationContract, (rep_admin.clone(), config()));
    let reputation = ReputationContractClient::new(&env, &reputation_id);
    // Each side authorizes the other: reputation accepts this marketplace's
    // attestations, and the marketplace lets this reputation contract consume
    // them.
    reputation.set_review_attestation_authority(&rep_admin, &marketplace_id);
    marketplace.set_reputation_contract(&admin, &reputation_id);

    let buyer = Address::generate(&env);
    let merchant = Address::generate(&env);
    let content_hash = BytesN::from_array(&env, &[1u8; 32]);

    // The escrow issues one attestation for the order it released.
    marketplace.issue_review_attestation(&escrow, &77u64, &buyer, &merchant);
    assert!(marketplace.verify_review_attestation(&77u64, &buyer, &merchant));

    // Filing the review consumes the attestation through the real
    // cross-contract call.
    reputation.submit_attested_review(&buyer, &merchant, &77u64, &5u32, &content_hash);

    assert!(!marketplace.verify_review_attestation(&77u64, &buyer, &merchant));
    let stored = marketplace.get_review_attestation(&77u64).unwrap();
    assert!(stored.consumed);

    // The same order cannot back a second review.
    assert_eq!(
        reputation.try_submit_attested_review(&buyer, &merchant, &77u64, &5u32, &content_hash),
        Err(Ok(ReputationError::DuplicateReviewCommitment))
    );

    // An order the escrow never issued an attestation for is refused.
    assert_eq!(
        reputation.try_submit_attested_review(&buyer, &merchant, &78u64, &5u32, &content_hash),
        Err(Ok(ReputationError::ReviewAttestationInvalid))
    );

    // A caller that is not the escrow cannot mint an attestation.
    let stranger = Address::generate(&env);
    assert_eq!(
        marketplace.try_issue_review_attestation(&stranger, &79u64, &buyer, &merchant),
        Err(Ok(delego_marketplace::MarketplaceError::Unauthorized))
    );
}
