//! Dynamic liquidity priority surcharge for relayers (congestion-aware).
//!
//! During severe ledger congestion (base fees spiking ~10x), the escrow
//! contract collects an optional priority surcharge that is forwarded to the
//! transaction submitter (relayer). The surcharge is strictly capped at
//! `MAX_PRIORITY_SURCHARGE_STROOPS` and can only be claimed by the relayer if
//! the transaction confirms within the configured target ledger window.

use soroban_sdk::{contracttype, Address, Env};

use crate::EscrowError;

/// Hard cap on the priority surcharge, in stroops.
pub const MAX_PRIORITY_SURCHARGE_STROOPS: i128 = 10_000;

/// Congestion multiplier at or below which no surcharge applies.
pub const CONGESTION_THRESHOLD: u32 = 1;

/// Number of ledgers after the submission ledger within which a transaction
/// must confirm for the relayer to claim the surcharge.
pub const TARGET_LEDGER_WINDOW: u32 = 5;

/// Storage key layout for pending surcharge claims.
#[contracttype]
pub enum SurchargeKey {
    /// Pending surcharge owed to a relayer for a given transaction id.
    Pending(u64),
}

/// A surcharge record created when a congested-period escrow action is
/// submitted by a relayer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingSurcharge {
    pub relayer: Address,
    pub amount: i128,
    pub submitted_ledger: u32,
}

/// Computes the priority surcharge for the given congestion multiplier and
/// base fee, clamped to `MAX_PRIORITY_SURCHARGE_STROOPS`.
pub fn compute_priority_surcharge(congestion_multiplier: u32, base_fee: i128) -> i128 {
    if congestion_multiplier > CONGESTION_THRESHOLD {
        base_fee
            .saturating_mul(congestion_multiplier as i128)
            .clamp(0, MAX_PRIORITY_SURCHARGE_STROOPS)
    } else {
        0
    }
}

/// Records a pending surcharge for `relayer` tied to transaction `tx_id`,
/// submitted at the current ledger. Returns the recorded amount (0 if no
/// surcharge applies).
pub fn record_surcharge(
    env: &Env,
    tx_id: u64,
    relayer: &Address,
    congestion_multiplier: u32,
    base_fee: i128,
) -> i128 {
    let amount = compute_priority_surcharge(congestion_multiplier, base_fee);
    if amount > 0 {
        let record = PendingSurcharge {
            relayer: relayer.clone(),
            amount,
            submitted_ledger: env.ledger().sequence(),
        };
        env.storage()
            .temporary()
            .set(&SurchargeKey::Pending(tx_id), &record);
        env.storage().temporary().extend_ttl(
            &SurchargeKey::Pending(tx_id),
            TARGET_LEDGER_WINDOW,
            TARGET_LEDGER_WINDOW,
        );
    }
    amount
}

/// Returns the amount a relayer may claim for `tx_id` given the confirmation
/// ledger. Fails if the record is missing, belongs to a different relayer, or
/// the confirmation falls outside the target window.
pub fn claimable_amount(
    env: &Env,
    tx_id: u64,
    relayer: &Address,
) -> Result<i128, EscrowError> {
    let record: PendingSurcharge = env
        .storage()
        .temporary()
        .get(&SurchargeKey::Pending(tx_id))
        .ok_or(EscrowError::InvalidAmount)?;

    if &record.relayer != relayer {
        return Err(EscrowError::InvalidAmount);
    }

    let current = env.ledger().sequence();
    let deadline = record
        .submitted_ledger
        .saturating_add(TARGET_LEDGER_WINDOW);
    if current > deadline {
        return Err(EscrowError::InvalidAmount);
    }

    Ok(record.amount.min(MAX_PRIORITY_SURCHARGE_STROOPS))
}

/// Claims the surcharge for `tx_id`, removing the pending record so it cannot
/// be claimed twice. Returns the claimed amount.
pub fn claim_surcharge(
    env: &Env,
    tx_id: u64,
    relayer: &Address,
) -> Result<i128, EscrowError> {
    relayer.require_auth();
    let amount = claimable_amount(env, tx_id, relayer)?;
    env.storage()
        .temporary()
        .remove(&SurchargeKey::Pending(tx_id));
    Ok(amount)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_surcharge_below_threshold() {
        assert_eq!(compute_priority_surcharge(0, 100), 0);
        assert_eq!(compute_priority_surcharge(1, 100), 0);
    }

    #[test]
    fn surcharge_scales_with_congestion() {
        assert_eq!(compute_priority_surcharge(10, 100), 1_000);
    }

    #[test]
    fn surcharge_is_capped() {
        assert_eq!(
            compute_priority_surcharge(10, 1_000_000),
            MAX_PRIORITY_SURCHARGE_STROOPS
        );
    }

    #[test]
    fn surcharge_saturates_on_overflow() {
        assert_eq!(
            compute_priority_surcharge(u32::MAX, i128::MAX),
            MAX_PRIORITY_SURCHARGE_STROOPS
        );
    }
}
