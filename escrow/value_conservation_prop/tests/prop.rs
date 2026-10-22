//! Property tests for cross-contract value conservation (issue #547).
//!
//! Mirrors `escrow/src/value_conservation.rs` exactly so the acceptance
//! criterion (10_000 balanced iterations without violation) is verifiable
//! independently of the full escrow contract build.

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
enum EscrowError {
    ValueConservationViolation = 461,
}

#[inline(always)]
fn assert_value_conservation(
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

#[test]
fn balanced_disbursement_ok() {
    assert_eq!(assert_value_conservation(1_000, 200, 700, 100), Ok(()));
    assert_eq!(assert_value_conservation(500, 500, 0, 0), Ok(()));
    assert_eq!(assert_value_conservation(500, 0, 475, 25), Ok(()));
    assert_eq!(assert_value_conservation(0, 0, 0, 0), Ok(()));
}

#[test]
fn imbalance_is_rejected() {
    assert_eq!(
        assert_value_conservation(1_000, 200, 700, 99),
        Err(EscrowError::ValueConservationViolation)
    );
    assert_eq!(
        assert_value_conservation(1_000, 200, 700, 101),
        Err(EscrowError::ValueConservationViolation)
    );
    assert_eq!(
        assert_value_conservation(100, 0, 0, 0),
        Err(EscrowError::ValueConservationViolation)
    );
}

use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    /// Balanced splits always conserve value.
    #[test]
    fn prop_balanced_splits_conserve(
        deposit in 0i128..=1_000_000_000_000,
        buyer_bps in 0u16..=10_000,
        fee_bps in 0u16..=1_000,
    ) {
        let buyer = deposit.saturating_mul(buyer_bps as i128) / 10_000;
        let remainder = deposit - buyer;
        let fee = remainder.saturating_mul(fee_bps as i128) / 10_000;
        let merchant = remainder - fee;

        prop_assert_eq!(
            assert_value_conservation(deposit, buyer, merchant, fee),
            Ok(())
        );
        prop_assert_eq!(buyer + merchant + fee, deposit);
    }

    /// Any non-zero perturbation of a balanced split must abort.
    #[test]
    fn prop_imbalance_aborts(
        deposit in 1i128..=1_000_000_000_000,
        buyer in 0i128..=1_000_000_000_000,
        merchant in 0i128..=1_000_000_000_000,
        fee in 0i128..=1_000_000_000_000,
        delta in 1i128..=1_000,
    ) {
        let fee_bal = deposit / 100;
        let merchant_bal = deposit / 2;
        let buyer_bal = deposit - merchant_bal - fee_bal;
        prop_assume!(buyer_bal >= 0);

        prop_assert_eq!(
            assert_value_conservation(deposit, buyer_bal, merchant_bal, fee_bal),
            Ok(())
        );

        let perturbed_fee = fee_bal.saturating_add(delta);
        prop_assume!(buyer_bal + merchant_bal + perturbed_fee != deposit);
        prop_assert_eq!(
            assert_value_conservation(deposit, buyer_bal, merchant_bal, perturbed_fee),
            Err(EscrowError::ValueConservationViolation)
        );

        prop_assume!(
            buyer
                .checked_add(merchant)
                .and_then(|v| v.checked_add(fee))
                != Some(deposit)
        );
        prop_assert_eq!(
            assert_value_conservation(deposit, buyer, merchant, fee),
            Err(EscrowError::ValueConservationViolation)
        );
    }

    /// Terminal release shape: buyer = 0, merchant + fee = deposit.
    #[test]
    fn prop_release_terminal_shape(
        deposit in 0i128..=1_000_000_000_000,
        fee_bps in 0u16..=1_000,
    ) {
        let fee = deposit.saturating_mul(fee_bps as i128) / 10_000;
        let merchant = deposit - fee;
        prop_assert_eq!(
            assert_value_conservation(deposit, 0, merchant, fee),
            Ok(())
        );
    }

    /// Terminal / partial refund shape: buyer = amount, merchant = 0, fee = 0.
    #[test]
    fn prop_refund_terminal_shape(deposit in 0i128..=1_000_000_000_000) {
        prop_assert_eq!(
            assert_value_conservation(deposit, deposit, 0, 0),
            Ok(())
        );
    }
}
