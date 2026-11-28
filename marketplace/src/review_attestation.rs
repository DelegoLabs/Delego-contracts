//! Single-use review attestations issued by the escrow contract on release
//! (issue #558).
//!
//! A storefront review is only credible when the reviewer really bought the
//! item, so rather than re-deriving "did this buyer pay for this order?"
//! inside the reputation contract, the escrow contract issues a
//! [`ReviewAttestation`] for the order it just released and the reputation
//! contract consumes exactly one attestation per filed review:
//!
//! 1. **Issuance.** [`issue_review_attestation`] is callable only by the
//!    configured escrow contract ([`catalog::get_authorized_escrow`]) or the
//!    platform admin. The escrow is the only component that knows an order
//!    reached `Released`, so only confirmed purchases can mint an attestation.
//!    At most one attestation exists per `order_id`.
//! 2. **Storage.** Attestations live in temporary storage with a 30-day TTL
//!    ([`REVIEW_ATTESTATION_TTL_LEDGERS`]): long enough to cover the review
//!    window, short enough that stale purchase proofs stop occupying state.
//! 3. **Verification and consumption.** [`consume_review_attestation`]
//!    verifies the buyer and merchant against the stored record and flips
//!    `consumed` in the same call, so a second review for the same order is
//!    rejected. It is callable only by the configured reputation contract
//!    ([`crate::DataKey::GlobalReputationContract`]) or the admin.

use crate::{catalog::get_authorized_escrow, DataKey, MarketplaceContract, MarketplaceError};
use soroban_sdk::{contracttype, symbol_short, Address, Env};

/// Lifetime of a review attestation in temporary storage: 30 days at 5
/// seconds per ledger.
pub const REVIEW_ATTESTATION_TTL_LEDGERS: u32 = 518_400;

/// Proof, issued by the escrow contract, that `buyer` settled `order_id` with
/// `merchant` and may therefore leave exactly one review for it.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewAttestation {
    /// Order the attestation was issued for. One attestation per order.
    pub order_id: u64,
    /// Buyer that settled the order; the only address allowed to use it.
    pub buyer: Address,
    /// Merchant the order was placed with.
    pub merchant: Address,
    /// Ledger sequence at which the attestation was issued.
    pub issued_ledger: u32,
    /// Set once the reputation contract has accepted a review backed by this
    /// attestation. A consumed attestation can never be used again.
    pub consumed: bool,
}

/// Emitted once per successful [`issue_review_attestation`] call.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewAttestationIssuedEvent {
    /// Order the attestation was issued for.
    pub order_id: u64,
    /// Buyer that may use the attestation.
    pub buyer: Address,
    /// Merchant the order was placed with.
    pub merchant: Address,
    /// Ledger sequence at which the attestation was issued.
    pub issued_ledger: u32,
}

/// Emitted once per successful [`consume_review_attestation`] call.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewAttestationConsumedEvent {
    /// Order the attestation was issued for.
    pub order_id: u64,
    /// Buyer allowed to use the attestation.
    pub buyer: Address,
    /// Merchant the order was placed with.
    pub merchant: Address,
    /// Ledger sequence at which the attestation was consumed.
    pub consumed_ledger: u32,
}

fn attestation_key(order_id: u64) -> DataKey {
    DataKey::ReviewAttestation(order_id)
}

/// Returns `true` when `caller` is the configured escrow contract or the
/// platform admin.
fn is_authorized_issuer(env: &Env, caller: &Address) -> Result<bool, MarketplaceError> {
    if caller == &MarketplaceContract::get_admin(env.clone())? {
        return Ok(true);
    }
    Ok(get_authorized_escrow(env.clone()).as_ref() == Some(caller))
}

/// Returns `true` when `caller` is the configured reputation contract or the
/// platform admin.
fn is_authorized_consumer(env: &Env, caller: &Address) -> Result<bool, MarketplaceError> {
    if caller == &MarketplaceContract::get_admin(env.clone())? {
        return Ok(true);
    }
    let reputation: Option<Address> = env
        .storage()
        .instance()
        .get(&DataKey::GlobalReputationContract);
    Ok(reputation.as_ref() == Some(caller))
}

/// Issue a single-use review attestation for an order the escrow contract has
/// released.
///
/// # Authorization
/// `caller` must authorize the call and be either the configured escrow
/// contract ([`catalog::get_authorized_escrow`]) or the platform admin. The
/// escrow passes the buyer and merchant recorded on the settled order, so an
/// attestation can only describe a purchase the escrow itself confirmed.
///
/// # Errors
/// * [`MarketplaceError::Unauthorized`] – caller is neither the escrow nor the admin.
/// * [`MarketplaceError::InvalidParam`] – `buyer` and `merchant` are equal.
/// * [`MarketplaceError::ReviewAttestationAlreadyIssued`] – an attestation
///   already exists for `order_id`.
pub fn issue_review_attestation(
    env: Env,
    caller: Address,
    order_id: u64,
    buyer: Address,
    merchant: Address,
) -> Result<ReviewAttestation, MarketplaceError> {
    caller.require_auth();

    if !is_authorized_issuer(&env, &caller)? {
        return Err(MarketplaceError::Unauthorized);
    }
    // A buyer cannot review itself, and a self-dealing attestation would let
    // one address manufacture verified reviews for its own storefront.
    if buyer == merchant {
        return Err(MarketplaceError::InvalidParam);
    }

    let key = attestation_key(order_id);
    if env.storage().temporary().has(&key) {
        return Err(MarketplaceError::ReviewAttestationAlreadyIssued);
    }

    let attestation = ReviewAttestation {
        order_id,
        buyer,
        merchant,
        issued_ledger: env.ledger().sequence(),
        consumed: false,
    };
    env.storage().temporary().set(&key, &attestation);
    env.storage().temporary().extend_ttl(
        &key,
        REVIEW_ATTESTATION_TTL_LEDGERS,
        REVIEW_ATTESTATION_TTL_LEDGERS,
    );

    env.events().publish(
        (symbol_short!("mkplc"), symbol_short!("rev_iss"), order_id),
        ReviewAttestationIssuedEvent {
            order_id,
            buyer: attestation.buyer.clone(),
            merchant: attestation.merchant.clone(),
            issued_ledger: attestation.issued_ledger,
        },
    );

    Ok(attestation)
}

/// Return the attestation issued for `order_id`, if it is still live.
pub fn get_review_attestation(env: Env, order_id: u64) -> Option<ReviewAttestation> {
    env.storage().temporary().get(&attestation_key(order_id))
}

/// Whether `buyer` holds a live, unconsumed attestation for a purchase from
/// `merchant` on `order_id`.
///
/// This is a read-only check: reading never burns the attestation. Filing the
/// review must go through [`consume_review_attestation`].
pub fn verify_review_attestation(
    env: Env,
    order_id: u64,
    buyer: Address,
    merchant: Address,
) -> bool {
    match get_review_attestation(env, order_id) {
        Some(attestation) => {
            !attestation.consumed && attestation.buyer == buyer && attestation.merchant == merchant
        }
        None => false,
    }
}

/// Verify an attestation and mark it consumed in one step.
///
/// # Authorization
/// `caller` must authorize the call and be either the configured reputation
/// contract ([`crate::DataKey::GlobalReputationContract`]) or the platform
/// admin. The reputation contract calls this while filing a review; the
/// buyer/merchant arguments it supplies are checked against the stored record
/// so an attestation cannot be spent on a different reviewer or merchant.
///
/// # Errors
/// * [`MarketplaceError::Unauthorized`] – caller is neither the reputation
///   contract nor the admin.
/// * [`MarketplaceError::ReviewAttestationNotFound`] – no live attestation
///   exists for `order_id` (never issued, or its TTL expired).
/// * [`MarketplaceError::ReviewAttestationConsumed`] – the attestation has
///   already backed a review.
/// * [`MarketplaceError::ReviewAttestationMismatch`] – `buyer` or `merchant`
///   does not match the issued attestation.
///
/// # Returns
/// The consumed attestation, with `consumed` set to `true`.
pub fn consume_review_attestation(
    env: Env,
    caller: Address,
    order_id: u64,
    buyer: Address,
    merchant: Address,
) -> Result<ReviewAttestation, MarketplaceError> {
    caller.require_auth();

    if !is_authorized_consumer(&env, &caller)? {
        return Err(MarketplaceError::Unauthorized);
    }

    let key = attestation_key(order_id);
    let mut attestation: ReviewAttestation = env
        .storage()
        .temporary()
        .get(&key)
        .ok_or(MarketplaceError::ReviewAttestationNotFound)?;

    if attestation.consumed {
        return Err(MarketplaceError::ReviewAttestationConsumed);
    }
    if attestation.buyer != buyer || attestation.merchant != merchant {
        return Err(MarketplaceError::ReviewAttestationMismatch);
    }

    attestation.consumed = true;
    env.storage().temporary().set(&key, &attestation);
    // Keep the consumed tombstone for a full window so a replay inside the
    // 30-day review period is rejected deterministically.
    env.storage().temporary().extend_ttl(
        &key,
        REVIEW_ATTESTATION_TTL_LEDGERS,
        REVIEW_ATTESTATION_TTL_LEDGERS,
    );

    env.events().publish(
        (symbol_short!("mkplc"), symbol_short!("rev_con"), order_id),
        ReviewAttestationConsumedEvent {
            order_id,
            buyer: attestation.buyer.clone(),
            merchant: attestation.merchant.clone(),
            consumed_ledger: env.ledger().sequence(),
        },
    );

    Ok(attestation)
}

#[cfg(all(test, not(feature = "full_suite")))]
mod tests {
    use super::*;
    use crate::{MarketplaceContractClient, RegisterParams};
    use soroban_sdk::{
        symbol_short,
        testutils::{storage::Temporary, Address as _, Ledger as _},
        String,
    };

    struct Ctx {
        env: Env,
        admin: Address,
        escrow: Address,
        reputation: Address,
        buyer: Address,
        merchant: Address,
        contract_id: Address,
        client: MarketplaceContractClient<'static>,
    }

    fn setup() -> Ctx {
        let env = Env::default();
        env.mock_all_auths();
        // Register the contract at a non-zero sequence so `issued_ledger` is
        // meaningful without jumping the ledger after registration (which
        // would archive the freshly created instance).
        env.ledger().set_sequence_number(1_000);

        let admin = Address::generate(&env);
        let contract_id = env.register(MarketplaceContract, (admin.clone(),));
        let client = MarketplaceContractClient::new(&env, &contract_id);

        let owner = Address::generate(&env);
        let params = RegisterParams {
            name: String::from_str(&env, "Attestation Merchant"),
            description: String::from_str(&env, "Issues review attestations"),
            category: symbol_short!("retail"),
            image_url: String::from_str(&env, "https://example.com/img.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 0,
        };
        crate::testutil::register_merchant_with_deposit(&env, &client, &owner, &params);

        let escrow = Address::generate(&env);
        let reputation = Address::generate(&env);
        client.set_authorized_escrow(&admin, &Some(escrow.clone()));
        client.set_reputation_contract(&admin, &reputation.clone());

        let buyer = Address::generate(&env);
        let merchant = Address::generate(&env);

        Ctx {
            env,
            admin,
            escrow,
            reputation,
            buyer,
            merchant,
            contract_id,
            client,
        }
    }

    #[test]
    fn issues_attestation_into_temporary_storage_with_30_day_ttl() {
        let ctx = setup();

        let issued =
            ctx.client
                .issue_review_attestation(&ctx.escrow, &7u64, &ctx.buyer, &ctx.merchant);

        assert_eq!(issued.order_id, 7);
        assert_eq!(issued.buyer, ctx.buyer);
        assert_eq!(issued.merchant, ctx.merchant);
        assert_eq!(issued.issued_ledger, 1_000);
        assert!(!issued.consumed);

        let stored = ctx.client.get_review_attestation(&7u64).unwrap();
        assert_eq!(stored, issued);

        // Temporary storage, expiring exactly 30 days out.
        let ttl = ctx.env.as_contract(&ctx.contract_id, || {
            ctx.env
                .storage()
                .temporary()
                .get_ttl(&DataKey::ReviewAttestation(7))
        });
        assert_eq!(ttl, REVIEW_ATTESTATION_TTL_LEDGERS);
        assert_eq!(REVIEW_ATTESTATION_TTL_LEDGERS, 30 * 24 * 60 * 60 / 5);
    }

    #[test]
    fn rejects_issuer_that_is_neither_escrow_nor_admin() {
        let ctx = setup();
        let stranger = Address::generate(&ctx.env);

        assert_eq!(
            ctx.client
                .try_issue_review_attestation(&stranger, &1u64, &ctx.buyer, &ctx.merchant),
            Err(Ok(MarketplaceError::Unauthorized))
        );
        assert!(ctx.client.get_review_attestation(&1u64).is_none());
    }

    #[test]
    fn admin_may_issue_on_behalf_of_the_escrow() {
        let ctx = setup();
        let issued =
            ctx.client
                .issue_review_attestation(&ctx.admin, &2u64, &ctx.buyer, &ctx.merchant);
        assert!(!issued.consumed);
        assert!(ctx
            .client
            .verify_review_attestation(&2u64, &ctx.buyer, &ctx.merchant));
    }

    #[test]
    fn rejects_duplicate_issuance_for_the_same_order() {
        let ctx = setup();
        let other_buyer = Address::generate(&ctx.env);

        ctx.client
            .issue_review_attestation(&ctx.escrow, &3u64, &ctx.buyer, &ctx.merchant);
        assert_eq!(
            ctx.client.try_issue_review_attestation(
                &ctx.escrow,
                &3u64,
                &other_buyer,
                &ctx.merchant
            ),
            Err(Ok(MarketplaceError::ReviewAttestationAlreadyIssued))
        );

        let stored = ctx.client.get_review_attestation(&3u64).unwrap();
        assert_eq!(stored.buyer, ctx.buyer);
    }

    #[test]
    fn rejects_self_dealing_attestation() {
        let ctx = setup();
        assert_eq!(
            ctx.client
                .try_issue_review_attestation(&ctx.escrow, &4u64, &ctx.buyer, &ctx.buyer),
            Err(Ok(MarketplaceError::InvalidParam))
        );
    }

    #[test]
    fn consume_marks_attestation_single_use() {
        let ctx = setup();
        ctx.client
            .issue_review_attestation(&ctx.escrow, &5u64, &ctx.buyer, &ctx.merchant);

        let consumed = ctx.client.consume_review_attestation(
            &ctx.reputation,
            &5u64,
            &ctx.buyer,
            &ctx.merchant,
        );
        assert!(consumed.consumed);
        assert_eq!(consumed.order_id, 5);

        // The write is persisted, and the attestation can no longer verify.
        let stored = ctx.client.get_review_attestation(&5u64).unwrap();
        assert!(stored.consumed);
        assert!(!ctx
            .client
            .verify_review_attestation(&5u64, &ctx.buyer, &ctx.merchant));

        // Second use is rejected.
        assert_eq!(
            ctx.client.try_consume_review_attestation(
                &ctx.reputation,
                &5u64,
                &ctx.buyer,
                &ctx.merchant
            ),
            Err(Ok(MarketplaceError::ReviewAttestationConsumed))
        );

        // A consumed attestation cannot be reissued for the same order.
        assert_eq!(
            ctx.client
                .try_issue_review_attestation(&ctx.escrow, &5u64, &ctx.buyer, &ctx.merchant),
            Err(Ok(MarketplaceError::ReviewAttestationAlreadyIssued))
        );
    }

    #[test]
    fn consume_rejects_mismatched_buyer_or_merchant() {
        let ctx = setup();
        let other = Address::generate(&ctx.env);
        ctx.client
            .issue_review_attestation(&ctx.escrow, &6u64, &ctx.buyer, &ctx.merchant);

        assert_eq!(
            ctx.client.try_consume_review_attestation(
                &ctx.reputation,
                &6u64,
                &other,
                &ctx.merchant
            ),
            Err(Ok(MarketplaceError::ReviewAttestationMismatch))
        );
        assert_eq!(
            ctx.client
                .try_consume_review_attestation(&ctx.reputation, &6u64, &ctx.buyer, &other),
            Err(Ok(MarketplaceError::ReviewAttestationMismatch))
        );

        // The failed attempts left the attestation untouched.
        assert!(ctx
            .client
            .verify_review_attestation(&6u64, &ctx.buyer, &ctx.merchant));
    }

    #[test]
    fn consume_is_restricted_to_the_configured_reputation_contract() {
        let ctx = setup();
        let stranger = Address::generate(&ctx.env);
        ctx.client
            .issue_review_attestation(&ctx.escrow, &8u64, &ctx.buyer, &ctx.merchant);

        assert_eq!(
            ctx.client
                .try_consume_review_attestation(&stranger, &8u64, &ctx.buyer, &ctx.merchant),
            Err(Ok(MarketplaceError::Unauthorized))
        );
        assert!(ctx
            .client
            .verify_review_attestation(&8u64, &ctx.buyer, &ctx.merchant));
    }

    #[test]
    fn consume_rejects_missing_attestation() {
        let ctx = setup();
        assert_eq!(
            ctx.client.try_consume_review_attestation(
                &ctx.reputation,
                &99u64,
                &ctx.buyer,
                &ctx.merchant
            ),
            Err(Ok(MarketplaceError::ReviewAttestationNotFound))
        );
    }
}
