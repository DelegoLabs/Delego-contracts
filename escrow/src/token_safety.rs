//! Token allowance safety revocation (issue #599).
//!
//! After every DeFi pool deposit or AMM swap the escrow contract may have
//! granted an ephemeral allowance to an external pool or router contract.
//! Even when the operation succeeds, a residual allowance can linger and
//! allow a malicious or compromised pool to pull funds in a later
//! transaction. `revoke_allowance_safe` eliminates that surface by
//! explicitly zeroing the allowance immediately after the external call
//! completes.
//!
//! The SEP-41 / SAC `approve` signature is:
//!   `approve(from, spender, amount, expiration_ledger)`
//! Passing `amount = 0` and `expiration_ledger = 0` is the canonical way
//! to revoke an allowance — a zero-amount approval at ledger 0 is
//! immediately expired and the host accepts it even when no live allowance
//! exists, making the call idempotent and safe to call unconditionally.

use soroban_sdk::{token, Address, Env};

/// Revoke any remaining token allowance granted to `spender` by the escrow
/// contract.
///
/// Calls `token.approve(current_contract, spender, 0, 0)` to atomically
/// reset the spender's allowance to zero. This must be called after every
/// cross-contract interaction that may have required an allowance (Blend
/// pool deposits, AMM/router swaps).
///
/// The call is unconditional: if no allowance was set, the host treats the
/// zero-amount approval as a no-op and the transaction continues normally.
pub fn revoke_allowance_safe(env: &Env, token: &Address, spender: &Address) {
    let client = token::Client::new(env, token);
    client.approve(&env.current_contract_address(), spender, &0, &0);
}
