//! Safe token transfer wrapper (issue #333), with typed error propagation
//! for Stellar Asset Contract / SEP-41 token failures (issue #481).
//!
//! The Stellar Asset Contract (SAC) and SEP-41 tokens return `()` from
//! `transfer` and signal failure by trapping. Some custom Soroban tokens
//! instead return `bool` (ERC-20 style) and signal failure with `false`.
//! This wrapper calls `transfer` through `try_invoke_contract` with a raw
//! `Val` return type so both shapes are accepted, and everything else is
//! rejected instead of being silently treated as success.
//!
//! Historically, any trap/contract error returned by the token call was
//! collapsed into a single generic `EscrowError::TokenTransferFailed`,
//! hiding whether the failure was e.g. an insufficient balance, an
//! exhausted allowance, a frozen/deauthorized trustline, or a token
//! contract that simply doesn't behave like a token at all. This module
//! inspects the raw host `Error` returned by the failed invocation and,
//! when it carries one of the standard built-in Stellar Asset Contract
//! error codes (mirrored by well-behaved SEP-41 implementations), maps it
//! onto a specific `EscrowError` variant so callers receive the exact
//! failure reason instead of an opaque internal error. A diagnostic event
//! carrying the raw code and, when recognized, a `TokenTransferError`
//! classification is also emitted before the call is treated as failed.

use soroban_sdk::{
    contracterror, symbol_short, vec, xdr::ScErrorType, Address, Env, Error as SdkError,
    IntoVal, TryFromVal, Val, Vec,
};

use crate::EscrowError;

/// Diagnostic classification of a failed token-contract invocation,
/// published as part of the `tok_err` contract event (issue #481). This is
/// not itself returned to callers of the escrow contract -- it exists so
/// the *reason* recorded in the event stream is typed rather than a bare
/// numeric code, which keeps off-chain indexers and block explorers from
/// having to hardcode SAC error numbers.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum TokenTransferError {
    InsufficientBalance = 0x0001_0020,
    AllowanceExceeded = 0x0001_0021,
    TokenFrozen = 0x0001_0022,
    InvalidTokenContract = 0x0001_0023,
}

/// Contract-error discriminants used by the host's built-in Stellar Asset
/// Contract (and mirrored by well-behaved SEP-41 tokens) for `transfer` /
/// `transfer_from` failures.
///
/// These are listed here rather than imported because the token being
/// invoked is an arbitrary external contract address supplied at runtime,
/// not a Rust dependency of this crate -- the host only gives us back a
/// numeric contract-error code (see `soroban_env_host::builtin_contracts::
/// contract_error::ContractError`, pinned via `soroban-env-host = 22.1.3`
/// in `Cargo.lock`).
mod sac_error_code {
    /// `NegativeAmountError`
    pub const NEGATIVE_AMOUNT: u32 = 8;
    /// `AllowanceError` -- transfer amount exceeds the caller's remaining
    /// allowance from `payer`.
    pub const ALLOWANCE: u32 = 9;
    /// `BalanceError` -- transfer amount exceeds the source's balance.
    pub const BALANCE: u32 = 10;
    /// `BalanceDeauthorizedError` -- the source or destination trustline has
    /// been frozen/deauthorized (e.g. via clawback-enabled asset controls).
    pub const BALANCE_DEAUTHORIZED: u32 = 11;
    /// `TrustlineMissingError` -- the destination (or source) has no
    /// trustline for the asset, or the "token" address does not implement a
    /// valid SAC/trustline-backed asset.
    pub const TRUSTLINE_MISSING: u32 = 13;
}

/// Transfer `amount` of `token` from the escrow contract to `to`.
///
/// Accepted outcomes:
/// - call succeeds and returns void (SAC / SEP-41)  -> `Ok(())`
/// - call succeeds and returns `true`                -> `Ok(())`
/// - call succeeds and returns `false`               -> `Err(TokenTransferRejected)`
/// - call succeeds and returns any other value       -> `Err(TokenUnexpectedReturn)`
/// - call fails with a recognized SAC error code     -> `Err(TokenInsufficientBalance)` /
///   `Err(TokenAllowanceExceeded)` / `Err(TokenFrozen)` / `Err(TokenInvalidTokenContract)`
/// - call traps / fails with an unrecognized code    -> `Err(TokenTransferFailed)`
pub fn safe_token_transfer(
    env: &Env,
    token: &Address,
    to: &Address,
    amount: i128,
) -> Result<(), EscrowError> {
    if amount <= 0 {
        return Err(EscrowError::InvalidAmount);
    }

    let from = env.current_contract_address();
    let args: Vec<Val> = vec![
        env,
        from.into_val(env),
        to.into_val(env),
        amount.into_val(env),
    ];

    dispatch_transfer(env, token, args)
}

/// Transfer `amount` of `token` from `payer` to the escrow contract.
///
/// This is the deposit side of a bundled escrow: the buyer (identified by
/// `payer`) funds the master checkout deposit in a single call. The caller
/// must authorize this invocation (e.g. via `payer.require_auth()` in the
/// entry point), since the SAC/SEP-41 token will check the payer's auth.
///
/// Accepted outcomes match `safe_token_transfer`.
pub fn safe_token_transfer_from(
    env: &Env,
    token: &Address,
    payer: &Address,
    amount: i128,
) -> Result<(), EscrowError> {
    if amount <= 0 {
        return Err(EscrowError::InvalidAmount);
    }

    let to = env.current_contract_address();
    let args: Vec<Val> = vec![
        env,
        payer.into_val(env),
        to.into_val(env),
        amount.into_val(env),
    ];

    dispatch_transfer(env, token, args)
}

/// Shared invocation + error-classification path for both transfer helpers.
fn dispatch_transfer(env: &Env, token: &Address, args: Vec<Val>) -> Result<(), EscrowError> {
    // `SdkError` (the host's raw, untyped `Error`) is used as the generic
    // error parameter here on purpose: the identity `TryFrom<SdkError> for
    // SdkError` conversion is infallible, so the invocation's failure arm
    // always comes back as `Err(Ok(raw_error))`, giving us the real
    // contract-error code to classify below instead of collapsing it before
    // we can inspect it.
    match env.try_invoke_contract::<Val, SdkError>(token, &symbol_short!("transfer"), args) {
        Ok(Ok(ret)) => sanitize_return(env, ret),
        // The call returned a value that could not be read back as `Val`;
        // this should not happen in practice since `Val` conversion is
        // infallible, but is handled explicitly for exhaustiveness.
        Ok(Err(_)) => Err(EscrowError::TokenUnexpectedReturn),
        // The call failed with a host/contract-level error we can inspect.
        Err(Ok(host_err)) => Err(classify_token_error(env, host_err)),
        // The call aborted (e.g. a raw VM trap not surfaced as a structured
        // contract error).
        Err(Err(_invoke_err)) => Err(EscrowError::TokenTransferFailed),
    }
}

/// Map a raw host error from a failed token invocation onto a specific
/// `EscrowError`, emitting a diagnostic event describing the failure first.
fn classify_token_error(env: &Env, err: SdkError) -> EscrowError {
    if !err.is_type(ScErrorType::Contract) {
        // Not a structured contract error (e.g. a budget/VM trap) -- no
        // specific code to report, so just surface the generic failure.
        env.events()
            .publish((symbol_short!("tok_err"), symbol_short!("trap")), ());
        return EscrowError::TokenTransferFailed;
    }

    let code = err.get_code();
    let (diagnostic, mapped) = match code {
        sac_error_code::BALANCE => (
            Some(TokenTransferError::InsufficientBalance),
            EscrowError::TokenInsufficientBalance,
        ),
        sac_error_code::ALLOWANCE => (
            Some(TokenTransferError::AllowanceExceeded),
            EscrowError::TokenAllowanceExceeded,
        ),
        sac_error_code::BALANCE_DEAUTHORIZED => (
            Some(TokenTransferError::TokenFrozen),
            EscrowError::TokenFrozen,
        ),
        sac_error_code::TRUSTLINE_MISSING => (
            Some(TokenTransferError::InvalidTokenContract),
            EscrowError::TokenInvalidTokenContract,
        ),
        // `NegativeAmountError` and anything else the host/token reports:
        // no confident mapping onto our four typed classes, so fall back to
        // the generic failure while still recording the raw code.
        _ => (None, EscrowError::TokenTransferFailed),
    };

    match diagnostic {
        Some(d) => env
            .events()
            .publish((symbol_short!("tok_err"), symbol_short!("code")), (code, d)),
        None => env
            .events()
            .publish((symbol_short!("tok_err"), symbol_short!("code")), code),
    }

    mapped
}

fn sanitize_return(env: &Env, ret: Val) -> Result<(), EscrowError> {
    if ret.is_void() {
        return Ok(());
    }
    match bool::try_from_val(env, &ret) {
        Ok(true) => Ok(()),
        Ok(false) => Err(EscrowError::TokenTransferRejected),
        Err(_) => Err(EscrowError::TokenUnexpectedReturn),
    }
}
