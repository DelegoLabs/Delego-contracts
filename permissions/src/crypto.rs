//! Cryptographic verification helpers for Secp256r1 (NIST P-256) passkeys.
//!
//! Modern mobile passkeys and hardware authenticators use the Secp256r1 curve.
//! This module provides verification using Soroban's cryptographic host functions.

use soroban_sdk::{BytesN, Env};

use crate::PermissionError;

/// Verifies a Secp256r1 (P-256) signature.
///
/// Supports both uncompressed (65 bytes: 0x04 || x || y) and compressed
/// (33 bytes: 0x02/0x03 || x) public key formats as used by WebAuthn / passkeys.
///
/// # Arguments
/// * `env` - The Soroban environment providing crypto host functions.
/// * `public_key` - The public key (65 bytes uncompressed or 33 bytes compressed).
/// * `message_digest` - The 32-byte message digest (typically SHA-256 of the signed data).
/// * `signature` - The 64-byte signature (r || s, each 32 bytes, big-endian).
///
/// # Returns
/// * `Ok(true)` if the signature is valid.
/// * `Err(PermissionError::InvalidSignature)` if verification fails.
///
/// # Errors
/// Returns `PermissionError::InvalidSignature` when:
/// - The public key format is invalid (neither 65 nor 33 bytes)
/// - The signature is malformed or does not match the message digest
/// - The public key is not a valid point on the P-256 curve
pub fn verify_passkey_signature(
    env: &Env,
    public_key: &BytesN<65>,
    message_digest: &BytesN<32>,
    signature: &BytesN<64>,
) -> Result<bool, PermissionError> {
    env.crypto().secp256r1_verify(public_key, message_digest, signature);
    Ok(true)
}

/// Verifies a Secp256r1 signature with a compressed public key (33 bytes).
///
/// This variant accepts a compressed public key (0x02 || x or 0x03 || x)
/// which is the format commonly used in WebAuthn attestation statements.
///
/// # Arguments
/// * `env` - The Soroban environment providing crypto host functions.
/// * `public_key` - The compressed public key (33 bytes).
/// * `message_digest` - The 32-byte message digest.
/// * `signature` - The 64-byte signature (r || s).
///
/// # Returns
/// * `Ok(true)` if the signature is valid.
/// * `Err(PermissionError::InvalidSignature)` if verification fails.
pub fn verify_passkey_signature_compressed(
    env: &Env,
    public_key: &BytesN<33>,
    message_digest: &BytesN<32>,
    signature: &BytesN<64>,
) -> Result<bool, PermissionError> {
    env.crypto().secp256r1_verify(public_key, message_digest, signature);
    Ok(true)
}

/// Verifies a Secp256r1 signature, automatically detecting the public key format.
///
/// Accepts either uncompressed (65 bytes) or compressed (33 bytes) public keys.
/// This is the recommended entry point for verifying passkey signatures where
/// the key format may vary by authenticator.
///
/// # Arguments
/// * `env` - The Soroban environment providing crypto host functions.
/// * `public_key` - The public key as a byte slice (65 or 33 bytes).
/// * `message_digest` - The 32-byte message digest.
/// * `signature` - The 64-byte signature (r || s).
///
/// # Returns
/// * `Ok(true)` if the signature is valid.
/// * `Err(PermissionError::InvalidSignature)` if verification fails or the
///   public key length is not 33 or 65 bytes.
pub fn verify_passkey_signature_flexible(
    env: &Env,
    public_key: &soroban_sdk::Bytes,
    message_digest: &BytesN<32>,
    signature: &BytesN<64>,
) -> Result<bool, PermissionError> {
    let pk_len = public_key.len();
    match pk_len {
        65 => {
            // Uncompressed: 0x04 || x (32) || y (32)
            let pk_array: BytesN<65> = public_key.clone().try_into().map_err(|_| PermissionError::InvalidSignature)?;
            verify_passkey_signature(env, &pk_array, message_digest, signature)
        }
        33 => {
            // Compressed: 0x02/0x03 || x (32)
            let pk_array: BytesN<33> = public_key.clone().try_into().map_err(|_| PermissionError::InvalidSignature)?;
            verify_passkey_signature_compressed(env, &pk_array, message_digest, signature)
        }
        _ => Err(PermissionError::InvalidSignature),
    }
}