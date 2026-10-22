//! Cross-contract value conservation invariant (issue #547).
//!
//! Guarantees that every terminal disbursement splits the escrowed principal
//! exactly across buyer refund, merchant payout, and platform fee — so
//! reentrancy or accounting bugs cannot mint or drain tokens.

use crate::EscrowError;

/// Assert that disbursements conserve the deposited principal exactly.
///
/// `initial_deposit` must equal
/// `buyer_disbursement + merchant_disbursement + fee_disbursement`.
/// Any imbalance aborts the transaction with
/// [`EscrowError::ValueConservationViolation`].
#[inline(always)]
pub fn assert_value_conservation(
    initial_deposit: i128,
    buyer_disbursement: i128,
    merchant_disbursement: i128,
    fee_disbursement: i128,
) -> Result<(), EscrowError> {
    if initial_deposit != buyer_disbursement + merchant_disbursement + fee_disbursement {
        return Err(EscrowError::ValueConservationViolation);
    }
    Ok(())
}
