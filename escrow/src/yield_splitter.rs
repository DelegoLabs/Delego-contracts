//! Automated yield share split engine for settlement interest (issue #591).
//!
//! When an escrow completes, the interest it accrued while held (e.g. from a
//! DeFi lending position) is split between the buyer and the merchant. The
//! default split is 50/50; a per-escrow [`crate::YieldSplitConfig`] may
//! override the merchant's share (a merchant share of `0` credits the whole
//! yield to the buyer as a cash-back reward).
//!
//! The split is computed with integer math only and the merchant share is the
//! floor of its basis-point portion; the buyer receives the exact remainder,
//! so `buyer_yield_share + merchant_yield_share == gross_yield` always holds
//! and no stroop is lost to rounding.
use soroban_sdk::{contracttype, symbol_short, token, Address, Env};

use crate::EscrowError;

/// Basis-point denominator (100.00%).
pub const YIELD_SPLIT_BPS_DENOMINATOR: u32 = 10_000;

/// Default merchant share of settlement yield, in basis points (50%).
pub const DEFAULT_MERCHANT_YIELD_SHARE_BPS: u32 = 5_000;

/// Exact allocation of an escrow's accrued yield between buyer and merchant.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldDistribution {
    /// Escrow / order the yield was accrued on.
    pub order_id: u64,
    /// Total yield accrued over the holding period.
    pub gross_yield: i128,
    /// Portion of `gross_yield` credited to the buyer.
    pub buyer_yield_share: i128,
    /// Portion of `gross_yield` credited to the merchant.
    pub merchant_yield_share: i128,
}

/// Computes the split of `gross_yield` given the merchant's share in basis
/// points.
///
/// # Errors
///
/// * [`EscrowError::InvalidYieldConfig`] - `gross_yield` is negative or
///   `merchant_share_bps` exceeds 10 000.
/// * [`EscrowError::MathOverflow`] - an intermediate product overflowed.
pub fn compute_distribution(
    order_id: u64,
    gross_yield: i128,
    merchant_share_bps: u32,
) -> Result<YieldDistribution, EscrowError> {
    if gross_yield < 0 || merchant_share_bps > YIELD_SPLIT_BPS_DENOMINATOR {
        return Err(EscrowError::InvalidYieldConfig);
    }

    let merchant_yield_share = gross_yield
        .checked_mul(merchant_share_bps as i128)
        .ok_or(EscrowError::MathOverflow)?
        / YIELD_SPLIT_BPS_DENOMINATOR as i128;
    let buyer_yield_share = gross_yield
        .checked_sub(merchant_yield_share)
        .ok_or(EscrowError::MathOverflow)?;

    // Conservation invariant: the shares must add up to the gross yield.
    assert!(
        buyer_yield_share
            .checked_add(merchant_yield_share)
            .map_or(false, |total| total == gross_yield),
        "yield split must conserve gross yield"
    );

    Ok(YieldDistribution {
        order_id,
        gross_yield,
        buyer_yield_share,
        merchant_yield_share,
    })
}

/// Transfers the computed yield shares from the contract to `buyer` and
/// `merchant` and records the distribution on the ledger as a
/// `(escrow, yld_dist, order_id)` event.
pub fn disburse(
    env: &Env,
    token_client: &token::Client,
    buyer: &Address,
    merchant: &Address,
    distribution: &YieldDistribution,
) {
    let contract = env.current_contract_address();
    if distribution.merchant_yield_share > 0 {
        token_client.transfer(&contract, merchant, &distribution.merchant_yield_share);
    }
    if distribution.buyer_yield_share > 0 {
        token_client.transfer(&contract, buyer, &distribution.buyer_yield_share);
    }

    env.events().publish(
        (
            symbol_short!("escrow"),
            symbol_short!("yld_dist"),
            distribution.order_id,
        ),
        distribution.clone(),
    );
}

