//! Signed spend authorizations.
//!
//! An agent may only spend on behalf of a delegation if it presents a
//! [`SpendAuthorization`] signed with its Ed25519 key. The signed bytes are a
//! domain-separated XDR payload (see [`SpendAuthorization::signing_payload`])
//! that binds every field of the authorization to one delegation, so a
//! signature can never be replayed for another delegation, asset, amount,
//! epoch or nonce.

use soroban_sdk::xdr::ToXdr;
use soroban_sdk::{contracttype, Address, Bytes, BytesN, Env, Symbol};

use super::errors::PermissionError;

/// Domain separator mixed into every spend authorization payload.
///
/// Including a fixed, versioned prefix in the signed bytes prevents an
/// authorization signed for this contract from being replayed against another
/// Delego contract (or a future payload format) that happens to serialize the
/// same fields.
pub const SPEND_AUTH_DOMAIN: &[u8] = b"DELEGO_SPEND_AUTH_V1";

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpendAuthorization {
    /// Agent (delegate) the authorization was issued to.
    pub agent: Address,
    /// Ed25519 public key of the agent.
    pub agent_pubkey: BytesN<32>,
    /// Asset (token contract) being spent.
    pub asset: Address,
    /// Amount to spend, in stroops.
    pub amount: i128,
    /// Delegation epoch the authorization was signed for.
    pub epoch: u64,
    /// Monotonic nonce; must match the delegation's next expected nonce.
    pub nonce: u64,
    /// Ed25519 signature over the domain-separated XDR payload.
    pub signature: BytesN<64>,
}

/// The exact bytes an agent signs for a spend.
///
/// Kept as a `#[contracttype]` so it has a stable, canonical XDR encoding. The
/// domain separator is part of the signed structure itself, which makes the
/// domain separation explicit in the payload an agent signs.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpendAuthPayload {
    pub domain: Bytes,
    pub delegation_id: Symbol,
    pub agent: Address,
    pub agent_pubkey: BytesN<32>,
    pub asset: Address,
    pub amount: i128,
    pub epoch: u64,
    pub nonce: u64,
}

impl SpendAuthorization {
    /// Rejects authorizations signed for a different delegation epoch.
    pub fn validate_epoch(&self, current_epoch: u64) -> Result<(), PermissionError> {
        if self.epoch != current_epoch {
            return Err(PermissionError::StaleEpoch);
        }
        Ok(())
    }

    /// Builds the domain-separated XDR payload that `signature` must cover.
    pub fn signing_payload(&self, env: &Env, delegation_id: Symbol) -> Bytes {
        SpendAuthPayload {
            domain: Bytes::from_slice(env, SPEND_AUTH_DOMAIN),
            delegation_id,
            agent: self.agent.clone(),
            agent_pubkey: self.agent_pubkey.clone(),
            asset: self.asset.clone(),
            amount: self.amount,
            epoch: self.epoch,
            nonce: self.nonce,
        }
        .to_xdr(env)
    }
}
