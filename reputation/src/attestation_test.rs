//! Tests for marketplace-backed review attestations (issue #558).

use crate::{
    DataKey, ReputationConfig, ReputationContract, ReputationContractClient, ReputationError,
    ReviewAttestationMirror, ReviewCommitment,
};
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{storage::Temporary, Address as _, Ledger as _},
    Address, BytesN, Env,
};

// --- Mock marketplace ------------------------------------------------------
//
// Stands in for `delego-marketplace`: it issues one attestation per order and
// traps on any replay or buyer/merchant mismatch, exactly like the real
// contract returns `ReviewAttestationConsumed` / `ReviewAttestationMismatch`.

#[contracttype]
#[derive(Clone)]
enum MockKey {
    Attestation(u64),
}

#[contract]
pub struct MockMarketplace;

#[contractimpl]
impl MockMarketplace {
    /// Register a live, unconsumed attestation for `order_id`.
    pub fn issue(env: Env, order_id: u64, buyer: Address, merchant: Address) {
        let attestation = ReviewAttestationMirror {
            order_id,
            buyer,
            merchant,
            issued_ledger: env.ledger().sequence(),
            consumed: false,
        };
        env.storage()
            .temporary()
            .set(&MockKey::Attestation(order_id), &attestation);
    }

    /// Verify and consume the order's attestation, mirroring the real
    /// marketplace entry point's checks.
    pub fn consume_review_attestation(
        env: Env,
        _caller: Address,
        order_id: u64,
        buyer: Address,
        merchant: Address,
    ) -> ReviewAttestationMirror {
        let key = MockKey::Attestation(order_id);
        let mut attestation: ReviewAttestationMirror = env
            .storage()
            .temporary()
            .get(&key)
            .unwrap_or_else(|| panic!("attestation not found"));
        if attestation.consumed {
            panic!("attestation already consumed");
        }
        if attestation.buyer != buyer || attestation.merchant != merchant {
            panic!("attestation mismatch");
        }
        attestation.consumed = true;
        env.storage().temporary().set(&key, &attestation);
        attestation
    }
}

fn config() -> ReputationConfig {
    ReputationConfig {
        decay_window_seconds: 90 * 24 * 60 * 60,
        min_transactions_threshold: 5,
        dispute_penalty_bps: 500,
        freeze_threshold_flags: 3,
    }
}

struct Ctx {
    env: Env,
    admin: Address,
    contract_id: Address,
    marketplace: Address,
    client: ReputationContractClient<'static>,
}

fn setup() -> Ctx {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let contract_id = env.register(ReputationContract, (admin.clone(), config()));
    let client = ReputationContractClient::new(&env, &contract_id);
    let marketplace = env.register(MockMarketplace, ());

    Ctx {
        env,
        admin,
        contract_id,
        marketplace,
        client,
    }
}

fn mock(ctx: &Ctx) -> MockMarketplaceClient<'_> {
    MockMarketplaceClient::new(&ctx.env, &ctx.marketplace)
}

#[test]
fn attested_review_consumes_attestation_and_flags_verified_purchase() {
    let ctx = setup();
    ctx.client
        .set_review_attestation_authority(&ctx.admin, &ctx.marketplace);
    assert_eq!(
        ctx.client.get_review_attestation_authority(),
        Some(ctx.marketplace.clone())
    );

    let buyer = Address::generate(&ctx.env);
    let merchant = Address::generate(&ctx.env);
    let content_hash = BytesN::from_array(&ctx.env, &[3u8; 32]);
    mock(&ctx).issue(&42u64, &buyer, &merchant);

    ctx.client
        .submit_attested_review(&buyer, &merchant, &42u64, &5u32, &content_hash);

    // The review is stored as a verified purchase.
    let commitment: ReviewCommitment = ctx.env.as_contract(&ctx.contract_id, || {
        ctx.env
            .storage()
            .persistent()
            .get(&DataKey::ReviewCommitment(42))
            .unwrap()
    });
    assert!(commitment.verified_purchase);
    assert_eq!(commitment.escrow_id, 42);
    assert_eq!(commitment.reviewer, buyer);
    assert_eq!(commitment.merchant, merchant);
    assert_eq!(commitment.content_hash, content_hash);

    // The attestation was marked consumed on the marketplace.
    let consumed: ReviewAttestationMirror = ctx.env.as_contract(&ctx.marketplace, || {
        ctx.env
            .storage()
            .temporary()
            .get(&MockKey::Attestation(42))
            .unwrap()
    });
    assert!(consumed.consumed);
}

#[test]
fn same_order_cannot_back_a_second_review() {
    let ctx = setup();
    ctx.client
        .set_review_attestation_authority(&ctx.admin, &ctx.marketplace);

    let buyer = Address::generate(&ctx.env);
    let merchant = Address::generate(&ctx.env);
    let content_hash = BytesN::from_array(&ctx.env, &[4u8; 32]);
    mock(&ctx).issue(&7u64, &buyer, &merchant);

    ctx.client
        .submit_attested_review(&buyer, &merchant, &7u64, &4u32, &content_hash);
    assert_eq!(
        ctx.client
            .try_submit_attested_review(&buyer, &merchant, &7u64, &4u32, &content_hash),
        Err(Ok(ReputationError::DuplicateReviewCommitment))
    );
}

#[test]
fn rejects_attestation_bound_to_another_buyer() {
    let ctx = setup();
    ctx.client
        .set_review_attestation_authority(&ctx.admin, &ctx.marketplace);

    let buyer = Address::generate(&ctx.env);
    let impostor = Address::generate(&ctx.env);
    let merchant = Address::generate(&ctx.env);
    let content_hash = BytesN::from_array(&ctx.env, &[5u8; 32]);
    mock(&ctx).issue(&9u64, &buyer, &merchant);

    assert_eq!(
        ctx.client
            .try_submit_attested_review(&impostor, &merchant, &9u64, &5u32, &content_hash),
        Err(Ok(ReputationError::ReviewAttestationInvalid))
    );
    // Nothing was stored for the impostor.
    let stored: Option<ReviewCommitment> = ctx.env.as_contract(&ctx.contract_id, || {
        ctx.env
            .storage()
            .persistent()
            .get(&DataKey::ReviewCommitment(9))
    });
    assert!(stored.is_none());
}

#[test]
fn rejects_missing_attestation() {
    let ctx = setup();
    ctx.client
        .set_review_attestation_authority(&ctx.admin, &ctx.marketplace);
    let buyer = Address::generate(&ctx.env);
    let merchant = Address::generate(&ctx.env);
    let content_hash = BytesN::from_array(&ctx.env, &[6u8; 32]);

    assert_eq!(
        ctx.client
            .try_submit_attested_review(&buyer, &merchant, &123u64, &5u32, &content_hash),
        Err(Ok(ReputationError::ReviewAttestationInvalid))
    );
}

#[test]
fn requires_configured_attestation_authority() {
    let ctx = setup();
    let buyer = Address::generate(&ctx.env);
    let merchant = Address::generate(&ctx.env);
    let content_hash = BytesN::from_array(&ctx.env, &[7u8; 32]);

    assert_eq!(
        ctx.client
            .try_submit_attested_review(&buyer, &merchant, &1u64, &5u32, &content_hash),
        Err(Ok(ReputationError::ReviewAttestationAuthorityNotSet))
    );
}

#[test]
fn set_attestation_authority_is_admin_only() {
    let ctx = setup();
    let stranger = Address::generate(&ctx.env);
    assert_eq!(
        ctx.client
            .try_set_review_attestation_authority(&stranger, &ctx.marketplace),
        Err(Ok(ReputationError::Unauthorized))
    );
}

#[test]
fn validates_reviewer_and_rating_before_spending_attestation() {
    let ctx = setup();
    ctx.client
        .set_review_attestation_authority(&ctx.admin, &ctx.marketplace);
    let buyer = Address::generate(&ctx.env);
    let merchant = Address::generate(&ctx.env);
    let content_hash = BytesN::from_array(&ctx.env, &[8u8; 32]);
    mock(&ctx).issue(&11u64, &buyer, &merchant);

    assert_eq!(
        ctx.client
            .try_submit_attested_review(&buyer, &merchant, &11u64, &0u32, &content_hash),
        Err(Ok(ReputationError::InvalidRating))
    );
    assert_eq!(
        ctx.client
            .try_submit_attested_review(&buyer, &buyer, &11u64, &5u32, &content_hash),
        Err(Ok(ReputationError::InvalidParam))
    );

    // The failed calls did not burn the attestation.
    let live: ReviewAttestationMirror = ctx.env.as_contract(&ctx.marketplace, || {
        ctx.env
            .storage()
            .temporary()
            .get(&MockKey::Attestation(11))
            .unwrap()
    });
    assert!(!live.consumed);
}
