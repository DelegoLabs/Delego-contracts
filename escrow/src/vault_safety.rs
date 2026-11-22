//! Health factor safety checks for staked escrow vaults (issue #593).
//!
//! When escrowed collateral is borrowed against in a lending protocol the
//! position's health factor, `collateral_value / debt_value`, must stay above
//! a liquidation-safe floor. Values are expressed in basis points
//! (`10_000` = 1.0):
//!
//! * below [`MIN_HEALTH_FACTOR_BPS`] (1.25) the operation is rejected with
//!   [`EscrowError::UnsafeHealthFactor`], so the transaction reverts;
//! * below [`WARNING_HEALTH_FACTOR_BPS`] (1.30) but still at or above the
//!   floor, a [`HealthFactorWarning`] event is emitted so keepers and
//!   off-chain monitors can top up collateral before liquidation risk.
//!
//! A vault with no debt is always healthy.

use crate::EscrowError;
use soroban_sdk::{contracttype, symbol_short, Env};

/// Basis-point denominator: `10_000` = a health factor of 1.0.
pub const HEALTH_FACTOR_SCALE_BPS: i128 = 10_000;
/// Hard floor: operations leaving the vault below 1.25 revert.
pub const MIN_HEALTH_FACTOR_BPS: i128 = 12_500;
/// Warning threshold: a [`HealthFactorWarning`] is emitted below 1.30.
pub const WARNING_HEALTH_FACTOR_BPS: i128 = 13_000;

/// Emitted when a vault's health factor is above the floor but below 1.30.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HealthFactorWarning {
    /// Escrow / vault identifier.
    pub vault_id: u64,
    /// Collateral value used for the check.
    pub collateral_value: i128,
    /// Debt value used for the check.
    pub debt_value: i128,
    /// Resulting health factor in basis points.
    pub health_factor_bps: i128,
}

/// Compute the health factor in basis points.
///
/// Returns `None` when there is no debt (the vault is unconditionally healthy).
/// Negative inputs are rejected as [`EscrowError::InvalidAmount`]; overflow as
/// [`EscrowError::MathOverflow`].
pub fn health_factor_bps(
    collateral_value: i128,
    debt_value: i128,
) -> Result<Option<i128>, EscrowError> {
    if collateral_value < 0 || debt_value < 0 {
        return Err(EscrowError::InvalidAmount);
    }
    if debt_value == 0 {
        return Ok(None);
    }
    let scaled = collateral_value
        .checked_mul(HEALTH_FACTOR_SCALE_BPS)
        .ok_or(EscrowError::MathOverflow)?;
    Ok(Some(scaled / debt_value))
}

/// Reject any vault state whose health factor is below 1.25.
pub fn assert_vault_health(collateral_value: i128, debt_value: i128) -> Result<(), EscrowError> {
    if let Some(hf_bps) = health_factor_bps(collateral_value, debt_value)? {
        if hf_bps < MIN_HEALTH_FACTOR_BPS {
            return Err(EscrowError::UnsafeHealthFactor);
        }
    }
    Ok(())
}

/// Enforce the health floor for the post-operation state of `vault_id` and
/// emit a [`HealthFactorWarning`] when it lands in the 1.25–1.30 band.
///
/// Call this with the collateral and debt values the vault *would* have after
/// a borrow, collateral withdrawal, or release; returning `Err` reverts the
/// whole operation, so under-collateralized borrowing can never be committed.
pub fn check_vault_health(
    env: &Env,
    vault_id: u64,
    collateral_value: i128,
    debt_value: i128,
) -> Result<(), EscrowError> {
    assert_vault_health(collateral_value, debt_value)?;
    if let Some(hf_bps) = health_factor_bps(collateral_value, debt_value)? {
        if hf_bps < WARNING_HEALTH_FACTOR_BPS {
            env.events().publish(
                (symbol_short!("escrow"), symbol_short!("hf_warn"), vault_id),
                HealthFactorWarning {
                    vault_id,
                    collateral_value,
                    debt_value,
                    health_factor_bps: hf_bps,
                },
            );
        }
    }
    Ok(())
}

/// Check a proposed additional borrow of `additional_debt` against the vault's
/// current `collateral_value` and `current_debt`, reverting if it would push
/// the health factor below 1.25.
pub fn assert_can_borrow(
    env: &Env,
    vault_id: u64,
    collateral_value: i128,
    current_debt: i128,
    additional_debt: i128,
) -> Result<(), EscrowError> {
    if additional_debt < 0 {
        return Err(EscrowError::InvalidAmount);
    }
    let debt_after = current_debt
        .checked_add(additional_debt)
        .ok_or(EscrowError::MathOverflow)?;
    check_vault_health(env, vault_id, collateral_value, debt_after)
}

/// Check a proposed collateral withdrawal of `withdraw_value`, reverting if it
/// would push the health factor below 1.25.
pub fn assert_can_withdraw_collateral(
    env: &Env,
    vault_id: u64,
    collateral_value: i128,
    debt_value: i128,
    withdraw_value: i128,
) -> Result<(), EscrowError> {
    if withdraw_value < 0 {
        return Err(EscrowError::InvalidAmount);
    }
    let collateral_after = collateral_value
        .checked_sub(withdraw_value)
        .ok_or(EscrowError::MathOverflow)?;
    if collateral_after < 0 {
        return Err(EscrowError::UnsafeHealthFactor);
    }
    check_vault_health(env, vault_id, collateral_after, debt_value)
}
