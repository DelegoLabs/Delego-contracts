//! Flash loan protection guards on staked escrow balances (issue #594).
//!
//! A staked escrow balance (one supplied to a lending pool via
//! [`crate::blend_yield`]) must never be deposited and withdrawn, or have its
//! yield claimed, within the same ledger. Doing so would let an attacker
//! borrow funds, stake them, realize the pool position and repay inside a
//! single transaction — the classic flash loan sandwich.
//!
//! The ledger sequence of the staking deposit is recorded per escrow, and every
//! withdrawal / yield claim path requires `current_ledger > deposit_ledger`.
use soroban_sdk::{contracttype, panic_with_error, Env};

use crate::{EscrowError, PERSISTENT_BUMP_AMOUNT, PERSISTENT_BUMP_THRESHOLD};

/// Storage keys owned by the flash loan guard. Kept in their own namespace so
/// the main `DataKey` enum does not grow.
#[contracttype]
#[derive(Clone)]
pub(crate) enum FlashLoanKey {
    /// Ledger sequence at which the escrow's balance was staked.
    StakeDepositLedger(u64),
}

/// Rejects any activity that happens in the same ledger as `last_activity_ledger`.
///
/// # Errors
///
/// * [`EscrowError::SameLedgerManipulationDetected`] - the current ledger
///   sequence equals `last_activity_ledger`.
pub fn guard_flash_loan(env: &Env, last_activity_ledger: u32) -> Result<(), EscrowError> {
    if env.ledger().sequence() == last_activity_ledger {
        return Err(EscrowError::SameLedgerManipulationDetected);
    }
    Ok(())
}

/// Records the current ledger sequence as the staking deposit ledger of
/// `escrow_id`.
pub fn record_deposit_ledger(env: &Env, escrow_id: u64) {
    let key = FlashLoanKey::StakeDepositLedger(escrow_id);
    env.storage()
        .persistent()
        .set(&key, &env.ledger().sequence());
    env.storage()
        .persistent()
        .extend_ttl(&key, PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
}

/// Returns the recorded staking deposit ledger of `escrow_id`, if any.
pub fn deposit_ledger(env: &Env, escrow_id: u64) -> Option<u32> {
    let key = FlashLoanKey::StakeDepositLedger(escrow_id);
    let ledger: Option<u32> = env.storage().persistent().get(&key);
    if ledger.is_some() {
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
    }
    ledger
}

/// Requires `current_ledger > deposit_ledger` before a withdrawal or yield
/// claim on a staked escrow. Escrows that were never staked are unaffected.
///
/// # Errors
///
/// * [`EscrowError::SameLedgerManipulationDetected`] - the escrow was staked
///   in the current ledger.
pub fn guard_staked_withdrawal(env: &Env, escrow_id: u64) -> Result<(), EscrowError> {
    match deposit_ledger(env, escrow_id) {
        Some(ledger) => guard_flash_loan(env, ledger),
        None => Ok(()),
    }
}

/// Panicking variant of [`guard_staked_withdrawal`] for call sites that do not
/// return `Result` (e.g. pool redemption inside [`crate::blend_yield`]).
pub fn enforce_staked_withdrawal(env: &Env, escrow_id: u64) {
    if let Err(error) = guard_staked_withdrawal(env, escrow_id) {
        panic_with_error!(env, error);
    }
}
