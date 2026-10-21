use soroban_sdk::{
    contracttype, symbol_short, xdr::ToXdr, Address, Bytes, BytesN, Env, Symbol,
};

use crate::{DataKey, ReputationError};

/// Courier attestation that a shipment was delivered or lost.
///
/// The signature is Ed25519 over the XDR encoding of the proof fields
/// `(domain = "CARRIER_PROOF_V1", current_contract_address,
/// carrier_oracle, tracking_number_hash, delivery_status)`. The signature
/// field itself is not part of the signed payload.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CarrierProof {
    /// Registered carrier oracle that produced this attestation.
    pub carrier_oracle: Address,
    /// Hash of the courier tracking number.
    pub tracking_number_hash: BytesN<32>,
    /// The courier's delivery outcome.
    pub delivery_status: Symbol,
    /// Ed25519 signature over the canonical carrier proof payload.
    pub signature: BytesN<64>,
}

/// Event emitted after a carrier proof has been verified successfully.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CarrierProofVerifiedEvent {
    /// Registered carrier oracle that produced this attestation.
    pub carrier_oracle: Address,
    /// Hash of the courier tracking number.
    pub tracking_number_hash: BytesN<32>,
    /// The courier's delivery outcome.
    pub delivery_status: Symbol,
}

#[contracttype]
#[derive(Clone)]
struct CarrierProofPayload {
    domain: Symbol,
    contract: Address,
    carrier_oracle: Address,
    tracking_number_hash: BytesN<32>,
    delivery_status: Symbol,
}

fn signing_payload(env: &Env, contract: &Address, proof: &CarrierProof) -> Bytes {
    CarrierProofPayload {
        domain: Symbol::new(env, "CARRIER_PROOF_V1"),
        contract: contract.clone(),
        carrier_oracle: proof.carrier_oracle.clone(),
        tracking_number_hash: proof.tracking_number_hash.clone(),
        delivery_status: proof.delivery_status.clone(),
    }
    .to_xdr(env)
}

pub(crate) fn set_carrier_oracle(env: &Env, carrier_oracle: &Address, public_key: &BytesN<32>) {
    let key = DataKey::CarrierOracle(carrier_oracle.clone());
    env.storage().persistent().set(&key, public_key);
    env.storage().persistent().extend_ttl(
        &key,
        crate::PERSISTENT_BUMP_THRESHOLD,
        crate::PERSISTENT_BUMP_AMOUNT,
    );
}

pub(crate) fn remove_carrier_oracle(env: &Env, carrier_oracle: &Address) {
    env.storage()
        .persistent()
        .remove(&DataKey::CarrierOracle(carrier_oracle.clone()));
}

pub(crate) fn verify_carrier_proof(env: &Env, proof: &CarrierProof) -> Result<(), ReputationError> {
    let key = DataKey::CarrierOracle(proof.carrier_oracle.clone());
    let public_key: BytesN<32> = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ReputationError::CarrierOracleNotAuthorized)?;
    env.storage().persistent().extend_ttl(
        &key,
        crate::PERSISTENT_BUMP_THRESHOLD,
        crate::PERSISTENT_BUMP_AMOUNT,
    );

    env.crypto()
        .ed25519_verify(
            &public_key,
            &signing_payload(env, &env.current_contract_address(), proof),
            &proof.signature,
        );

    env.events().publish(
        (
            symbol_short!("reput"),
            symbol_short!("carproof"),
            proof.carrier_oracle.clone(),
        ),
        CarrierProofVerifiedEvent {
            carrier_oracle: proof.carrier_oracle.clone(),
            tracking_number_hash: proof.tracking_number_hash.clone(),
            delivery_status: proof.delivery_status.clone(),
        },
    );

    Ok(())
}

#[cfg(test)]
pub(crate) fn test_signing_payload(env: &Env, contract: &Address, proof: &CarrierProof) -> Bytes {
    signing_payload(env, contract, proof)
}
