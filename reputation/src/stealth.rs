//! Anonymous buyer reviews under ephemeral stealth keys (issue #588).
//!
//! A buyer who fears merchant retaliation can leave verified feedback without
//! ever exposing their primary Stellar payment address on-chain:
//!
//! 1. **Ticket issuance.** After a purchase settles, the trusted attestor (the
//!    reputation admin) registers a single-use purchase-proof ticket for the
//!    merchant: `ticket_commitment = sha256(ticket_secret)`. The secret is
//!    handed to the buyer off-chain. The registration carries no buyer
//!    address or escrow id, so the ticket cannot be traced back to the
//!    buyer's payment wallet from chain data.
//! 2. **Key wrapping.** Off-chain, the buyer generates a fresh Ed25519
//!    stealth keypair and encrypts the review content, wrapping the content
//!    key to that stealth key. Only the resulting ciphertext
//!    (`encrypted_payload`) is ever submitted.
//! 3. **Submission.** Anyone (typically a relayer) submits the review with the
//!    ticket secret and a signature by the stealth key over the canonical
//!    payload. No `Address::require_auth` is performed, so the buyer's
//!    account never appears as a signer or invoker.
//!
//! The contract verifies the ticket preimage, that the ticket belongs to the
//! reviewed merchant and has not been redeemed, and the stealth-key signature
//! binding the ciphertext, merchant and ticket together. The ticket and the
//! stealth key are both burnt on use, so a ticket yields exactly one review
//! and a stealth key cannot be reused to link several reviews together.

use crate::{ReputationError, PERSISTENT_BUMP_AMOUNT, PERSISTENT_BUMP_THRESHOLD};
use soroban_sdk::{contracttype, symbol_short, xdr::ToXdr, Address, Bytes, BytesN, Env, Symbol};

/// Upper bound on the encrypted review ciphertext, in bytes.
pub const MAX_ENCRYPTED_PAYLOAD_LEN: u32 = 4_096;

/// Anonymous review stored under an ephemeral stealth public key.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AnonymousReview {
    /// Monotonic review identifier.
    pub review_id: u64,
    /// Merchant the review is about.
    pub merchant: Address,
    /// Review ciphertext; the content key is wrapped to `stealth_pubkey`.
    pub encrypted_payload: Bytes,
    /// Single-use Ed25519 stealth public key that signed the submission.
    pub stealth_pubkey: BytesN<32>,
    /// `true` once the purchase-proof ticket and stealth signature verified.
    pub zk_proof_valid: bool,
}

/// Event emitted when an anonymous review is accepted. Carries no buyer data.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnonymousReviewSubmitted {
    /// Identifier of the stored review.
    pub review_id: u64,
    /// Merchant the review is about.
    pub merchant: Address,
    /// Stealth public key that signed the review.
    pub stealth_pubkey: BytesN<32>,
}

#[contracttype]
#[derive(Clone)]
enum Key {
    /// Live ticket commitment -> merchant it attests a purchase from.
    Ticket(BytesN<32>),
    /// Redeemed ticket commitment (nullifier).
    TicketUsed(BytesN<32>),
    /// Stealth public key that has already signed a review.
    StealthKeyUsed(BytesN<32>),
    /// Anonymous review by id.
    Review(u64),
    /// Next review id.
    Counter,
    /// Number of anonymous reviews per merchant.
    MerchantCount(Address),
}

/// Canonical message the stealth key signs. The ciphertext is committed by
/// hash to keep the signed payload small.
#[contracttype]
#[derive(Clone)]
struct SigningPayload {
    domain: Symbol,
    contract: Address,
    merchant: Address,
    ticket_commitment: BytesN<32>,
    payload_hash: BytesN<32>,
}

fn bump(env: &Env, key: &Key) {
    env.storage()
        .persistent()
        .extend_ttl(key, PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
}

/// Commitment registered for a ticket secret: `sha256(ticket_secret)`.
pub fn ticket_commitment(env: &Env, ticket_secret: &BytesN<32>) -> BytesN<32> {
    env.crypto()
        .sha256(&Bytes::from_array(env, &ticket_secret.to_array()))
        .into()
}

/// Bytes the stealth key must sign for a submission.
pub fn signing_payload(
    env: &Env,
    merchant: &Address,
    ticket_commitment: &BytesN<32>,
    encrypted_payload: &Bytes,
) -> Bytes {
    SigningPayload {
        domain: Symbol::new(env, "ANON_REVIEW_V1"),
        contract: env.current_contract_address(),
        merchant: merchant.clone(),
        ticket_commitment: ticket_commitment.clone(),
        payload_hash: env.crypto().sha256(encrypted_payload).into(),
    }
    .to_xdr(env)
}

/// Register a single-use purchase-proof ticket for `merchant`.
///
/// Callers must enforce attestor (admin) authorization.
pub fn issue_ticket(
    env: &Env,
    merchant: &Address,
    commitment: &BytesN<32>,
) -> Result<(), ReputationError> {
    let key = Key::Ticket(commitment.clone());
    let used = Key::TicketUsed(commitment.clone());
    if env.storage().persistent().has(&key) || env.storage().persistent().has(&used) {
        return Err(ReputationError::DuplicateReviewCommitment);
    }
    env.storage().persistent().set(&key, merchant);
    bump(env, &key);
    env.events().publish(
        (symbol_short!("reput"), symbol_short!("anon_tkt")),
        (merchant.clone(), commitment.clone()),
    );
    Ok(())
}

/// Verify a purchase-proof ticket and stealth signature, then store the
/// encrypted review. Returns the new review id.
pub fn submit_review(
    env: &Env,
    merchant: &Address,
    encrypted_payload: Bytes,
    stealth_pubkey: BytesN<32>,
    ticket_secret: &BytesN<32>,
    signature: &BytesN<64>,
) -> Result<u64, ReputationError> {
    if encrypted_payload.is_empty() || encrypted_payload.len() > MAX_ENCRYPTED_PAYLOAD_LEN {
        return Err(ReputationError::InvalidParam);
    }

    let commitment = ticket_commitment(env, ticket_secret);
    if env
        .storage()
        .persistent()
        .has(&Key::TicketUsed(commitment.clone()))
    {
        return Err(ReputationError::BlindedReceiptAlreadyUsed);
    }
    let ticket_key = Key::Ticket(commitment.clone());
    let ticket_merchant: Address = env
        .storage()
        .persistent()
        .get(&ticket_key)
        .ok_or(ReputationError::UnverifiedOrder)?;
    if ticket_merchant != *merchant {
        return Err(ReputationError::UnverifiedOrder);
    }
    let stealth_key = Key::StealthKeyUsed(stealth_pubkey.clone());
    if env.storage().persistent().has(&stealth_key) {
        return Err(ReputationError::BlindedReceiptAlreadyUsed);
    }

    // Traps (reverting the transaction) on an invalid signature.
    env.crypto().ed25519_verify(
        &stealth_pubkey,
        &signing_payload(env, merchant, &commitment, &encrypted_payload),
        signature,
    );

    // Burn the ticket and the stealth key before writing the review.
    env.storage().persistent().remove(&ticket_key);
    let used_key = Key::TicketUsed(commitment);
    env.storage().persistent().set(&used_key, &true);
    bump(env, &used_key);
    env.storage().persistent().set(&stealth_key, &true);
    bump(env, &stealth_key);

    let review_id: u64 = env
        .storage()
        .persistent()
        .get(&Key::Counter)
        .unwrap_or(0u64)
        .checked_add(1)
        .ok_or(ReputationError::InvalidParam)?;
    env.storage().persistent().set(&Key::Counter, &review_id);
    bump(env, &Key::Counter);

    let review = AnonymousReview {
        review_id,
        merchant: merchant.clone(),
        encrypted_payload,
        stealth_pubkey: stealth_pubkey.clone(),
        zk_proof_valid: true,
    };
    let review_key = Key::Review(review_id);
    env.storage().persistent().set(&review_key, &review);
    bump(env, &review_key);

    let count_key = Key::MerchantCount(merchant.clone());
    let count: u64 = env.storage().persistent().get(&count_key).unwrap_or(0);
    let next_count = count.checked_add(1).ok_or(ReputationError::InvalidParam)?;
    env.storage().persistent().set(&count_key, &next_count);
    bump(env, &count_key);

    env.events().publish(
        (
            symbol_short!("reput"),
            symbol_short!("anon_rev"),
            merchant.clone(),
        ),
        AnonymousReviewSubmitted {
            review_id,
            merchant: merchant.clone(),
            stealth_pubkey,
        },
    );
    Ok(review_id)
}

/// Read an anonymous review by id.
pub fn review(env: &Env, review_id: u64) -> Option<AnonymousReview> {
    let key = Key::Review(review_id);
    let value = env.storage().persistent().get(&key);
    if value.is_some() {
        bump(env, &key);
    }
    value
}

/// Number of anonymous reviews recorded for `merchant`.
pub fn merchant_review_count(env: &Env, merchant: &Address) -> u64 {
    let key = Key::MerchantCount(merchant.clone());
    let count: Option<u64> = env.storage().persistent().get(&key);
    if count.is_some() {
        bump(env, &key);
    }
    count.unwrap_or(0)
}

/// Whether a ticket commitment is registered and not yet redeemed.
pub fn is_ticket_live(env: &Env, commitment: &BytesN<32>) -> bool {
    let key = Key::Ticket(commitment.clone());
    let live = env.storage().persistent().has(&key);
    if live {
        bump(env, &key);
    }
    live
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ReputationConfig, ReputationContract, ReputationContractClient};
    use soroban_sdk::testutils::{storage::Persistent, Address as _, Ledger};

    fn setup(env: &Env) -> (ReputationContractClient<'_>, Address, Address) {
        env.mock_all_auths();
        let admin = Address::generate(env);
        let contract_id = env.register(
            ReputationContract,
            (
                admin.clone(),
                ReputationConfig {
                    decay_window_seconds: 90 * 24 * 60 * 60,
                    min_transactions_threshold: 5,
                    dispute_penalty_bps: 500,
                    freeze_threshold_flags: 3,
                },
            ),
        );
        (
            ReputationContractClient::new(env, &contract_id),
            contract_id,
            admin,
        )
    }

    #[test]
    fn ticket_liveness_getter_tracks_issuance_and_redemption() {
        let env = Env::default();
        let (client, contract_id, _admin) = setup(&env);
        let merchant = Address::generate(&env);
        let commitment = BytesN::from_array(&env, &[7u8; 32]);

        assert!(!client.is_anonymous_ticket_live(&commitment));
        client.issue_anonymous_review_ticket(&merchant, &commitment);
        assert!(client.is_anonymous_ticket_live(&commitment));

        env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .remove(&Key::Ticket(commitment.clone()));
        });
        assert!(!client.is_anonymous_ticket_live(&commitment));
    }

    #[test]
    fn ticket_liveness_read_extends_ttl() {
        let env = Env::default();
        let (client, contract_id, _admin) = setup(&env);
        let merchant = Address::generate(&env);
        let commitment = BytesN::from_array(&env, &[11u8; 32]);
        client.issue_anonymous_review_ticket(&merchant, &commitment);

        env.as_contract(&contract_id, || {
            env.storage().instance().extend_ttl(1_000_000, 1_000_000);
        });
        let initial = env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .get_ttl(&Key::Ticket(commitment.clone()))
        });
        env.ledger()
            .with_mut(|li| li.sequence_number += initial - PERSISTENT_BUMP_THRESHOLD + 1);

        let before = env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .get_ttl(&Key::Ticket(commitment.clone()))
        });
        assert!(client.is_anonymous_ticket_live(&commitment));
        let after = env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .get_ttl(&Key::Ticket(commitment.clone()))
        });
        assert!(after > before);
        assert!(after >= PERSISTENT_BUMP_AMOUNT);
    }

    #[test]
    fn merchant_count_read_extends_ttl() {
        let env = Env::default();
        let (client, contract_id, _admin) = setup(&env);
        let merchant = Address::generate(&env);
        env.as_contract(&contract_id, || {
            let key = Key::MerchantCount(merchant.clone());
            env.storage().persistent().set(&key, &3u64);
            env.storage().persistent().extend_ttl(
                &key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );
            env.storage().instance().extend_ttl(1_000_000, 1_000_000);
        });
        let initial = env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .get_ttl(&Key::MerchantCount(merchant.clone()))
        });
        env.ledger()
            .with_mut(|li| li.sequence_number += initial - PERSISTENT_BUMP_THRESHOLD + 1);

        let before = env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .get_ttl(&Key::MerchantCount(merchant.clone()))
        });
        assert_eq!(client.get_anon_review_count(&merchant), 3);
        let after = env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .get_ttl(&Key::MerchantCount(merchant.clone()))
        });
        assert!(after > before);
        assert!(after >= PERSISTENT_BUMP_AMOUNT);
    }
}
