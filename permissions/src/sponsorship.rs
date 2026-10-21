//! Gas fee sponsorship quota and deduction logic.
//!
//! Relayers submitting transactions on behalf of users need on-chain gas fee
//! allowances to prevent draining the relayer's native XLM balance.
//! This module tracks deposited sponsorship quotas, enforces lifetime balance
//! and per-transaction fee caps, and reimburses relayers from the user's
//! deposited sponsorship pool upon execution.

use soroban_sdk::{
    contracttype, symbol_short, Address, Env,
};

use crate::PermissionError;

/// Storage and tracking quota for gas fee sponsorship.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GasSponsorshipQuota {
    /// Relayer designated or executing on behalf of the user.
    pub relayer: Address,
    /// Current available sponsorship balance in stroops.
    pub sponsor_balance_stroops: i128,
    /// Maximum base gas fee in stroops reimbursable for a single transaction.
    pub max_fee_per_tx_stroops: i128,
    /// Remaining count of sponsored transactions allowed under this quota.
    pub remaining_tx_count: u32,
}

/// Deducts exact base transaction fee from a sponsor's balance and reimburses the relayer.
///
/// # Validation and Safety
/// - `fee_stroops` must be strictly positive (`> 0`).
/// - Reverts with [`PermissionError::ExceedsPerTxLimit`] if `fee_stroops` exceeds
///   `quota.max_fee_per_tx_stroops`.
/// - Reverts with [`PermissionError::ExceedsTotalLimit`] if `remaining_tx_count == 0`
///   or `fee_stroops` exceeds `quota.sponsor_balance_stroops`.
/// - Reverts with [`PermissionError::Unauthorized`] if caller/expected relayer does not match
///   `quota.relayer`.
///
/// # Reimbursement
/// - Deducts `fee_stroops` from `quota.sponsor_balance_stroops`.
/// - Decrements `quota.remaining_tx_count` by 1.
/// - If `native_token` is provided, transfers `fee_stroops` from `sponsor` (or contract escrow)
///   to `quota.relayer`.
/// - Publishes a sponsorship deduction event for off-chain indexing.
pub fn deduct_sponsorship_fee(
    env: &Env,
    quota: &mut GasSponsorshipQuota,
    fee_stroops: i128,
    relayer: &Address,
) -> Result<(), PermissionError> {
    if fee_stroops <= 0 {
        return Err(PermissionError::InvalidParam);
    }

    if quota.relayer != *relayer {
        return Err(PermissionError::Unauthorized);
    }

    if fee_stroops > quota.max_fee_per_tx_stroops {
        return Err(PermissionError::ExceedsPerTxLimit);
    }

    if quota.remaining_tx_count == 0 {
        return Err(PermissionError::ExceedsTotalLimit);
    }

    let updated_balance = quota
        .sponsor_balance_stroops
        .checked_sub(fee_stroops)
        .ok_or(PermissionError::ExceedsTotalLimit)?;

    if updated_balance < 0 {
        return Err(PermissionError::ExceedsTotalLimit);
    }

    quota.sponsor_balance_stroops = updated_balance;
    quota.remaining_tx_count -= 1;

    env.events().publish(
        (symbol_short!("perm"), symbol_short!("gas_ded")),
        (relayer.clone(), fee_stroops, quota.sponsor_balance_stroops, quota.remaining_tx_count),
    );

    Ok(())
}

/// Deducts exact base transaction fee from a sponsor's balance and transfers
/// native token reimbursement to the relayer.
pub fn deduct_and_reimburse_relayer(
    env: &Env,
    quota: &mut GasSponsorshipQuota,
    fee_stroops: i128,
    relayer: &Address,
    native_token: &Address,
    sponsor_source: &Address,
) -> Result<(), PermissionError> {
    deduct_sponsorship_fee(env, quota, fee_stroops, relayer)?;

    let token_client = soroban_sdk::token::Client::new(env, native_token);
    token_client.transfer(sponsor_source, relayer, &fee_stroops);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        contract, contractimpl,
        testutils::{Address as _, Events},
        token::{StellarAssetClient, TokenClient},
    };

    #[test]
    fn test_deduct_sponsorship_fee_success() {
        let env = Env::default();
        let relayer = Address::generate(&env);

        let mut quota = GasSponsorshipQuota {
            relayer: relayer.clone(),
            sponsor_balance_stroops: 50_000,
            max_fee_per_tx_stroops: 10_000,
            remaining_tx_count: 5,
        };

        let res = deduct_sponsorship_fee(&env, &mut quota, 5_000, &relayer);
        assert_eq!(res, Ok(()));
        assert_eq!(quota.sponsor_balance_stroops, 45_000);
        assert_eq!(quota.remaining_tx_count, 4);

        let events = env.events().all();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_revert_if_fee_exceeds_max_fee_per_tx() {
        let env = Env::default();
        let relayer = Address::generate(&env);

        let mut quota = GasSponsorshipQuota {
            relayer: relayer.clone(),
            sponsor_balance_stroops: 50_000,
            max_fee_per_tx_stroops: 5_000,
            remaining_tx_count: 5,
        };

        let res = deduct_sponsorship_fee(&env, &mut quota, 5_001, &relayer);
        assert_eq!(res, Err(PermissionError::ExceedsPerTxLimit));
        // Ensure state was not mutated
        assert_eq!(quota.sponsor_balance_stroops, 50_000);
        assert_eq!(quota.remaining_tx_count, 5);
    }

    #[test]
    fn test_revert_if_fee_exceeds_lifetime_balance() {
        let env = Env::default();
        let relayer = Address::generate(&env);

        let mut quota = GasSponsorshipQuota {
            relayer: relayer.clone(),
            sponsor_balance_stroops: 3_000,
            max_fee_per_tx_stroops: 10_000,
            remaining_tx_count: 5,
        };

        let res = deduct_sponsorship_fee(&env, &mut quota, 4_000, &relayer);
        assert_eq!(res, Err(PermissionError::ExceedsTotalLimit));
        assert_eq!(quota.sponsor_balance_stroops, 3_000);
        assert_eq!(quota.remaining_tx_count, 5);
    }

    #[test]
    fn test_revert_if_no_remaining_tx_count() {
        let env = Env::default();
        let relayer = Address::generate(&env);

        let mut quota = GasSponsorshipQuota {
            relayer: relayer.clone(),
            sponsor_balance_stroops: 50_000,
            max_fee_per_tx_stroops: 10_000,
            remaining_tx_count: 0,
        };

        let res = deduct_sponsorship_fee(&env, &mut quota, 1_000, &relayer);
        assert_eq!(res, Err(PermissionError::ExceedsTotalLimit));
        assert_eq!(quota.sponsor_balance_stroops, 50_000);
        assert_eq!(quota.remaining_tx_count, 0);
    }

    #[test]
    fn test_revert_if_unauthorized_relayer() {
        let env = Env::default();
        let relayer = Address::generate(&env);
        let other_relayer = Address::generate(&env);

        let mut quota = GasSponsorshipQuota {
            relayer: relayer.clone(),
            sponsor_balance_stroops: 50_000,
            max_fee_per_tx_stroops: 10_000,
            remaining_tx_count: 5,
        };

        let res = deduct_sponsorship_fee(&env, &mut quota, 1_000, &other_relayer);
        assert_eq!(res, Err(PermissionError::Unauthorized));
        assert_eq!(quota.sponsor_balance_stroops, 50_000);
        assert_eq!(quota.remaining_tx_count, 5);
    }

    #[test]
    fn test_revert_if_invalid_fee() {
        let env = Env::default();
        let relayer = Address::generate(&env);

        let mut quota = GasSponsorshipQuota {
            relayer: relayer.clone(),
            sponsor_balance_stroops: 50_000,
            max_fee_per_tx_stroops: 10_000,
            remaining_tx_count: 5,
        };

        assert_eq!(
            deduct_sponsorship_fee(&env, &mut quota, 0, &relayer),
            Err(PermissionError::InvalidParam)
        );
        assert_eq!(
            deduct_sponsorship_fee(&env, &mut quota, -100, &relayer),
            Err(PermissionError::InvalidParam)
        );
    }

    #[contract]
    pub struct MockContract;
    #[contractimpl]
    impl MockContract {}

    #[test]
    fn test_deduct_and_reimburse_relayer() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(MockContract, ());
        let token_admin = Address::generate(&env);
        let relayer = Address::generate(&env);

        let sac = env.register_stellar_asset_contract_v2(token_admin);
        let native_token = sac.address();

        let stellar_client = StellarAssetClient::new(&env, &native_token);
        stellar_client.mint(&contract_id, &100_000);

        let mut quota = GasSponsorshipQuota {
            relayer: relayer.clone(),
            sponsor_balance_stroops: 50_000,
            max_fee_per_tx_stroops: 10_000,
            remaining_tx_count: 2,
        };

        let res = env.as_contract(&contract_id, || {
            deduct_and_reimburse_relayer(
                &env,
                &mut quota,
                7_500,
                &relayer,
                &native_token,
                &contract_id,
            )
        });

        assert_eq!(res, Ok(()));
        assert_eq!(quota.sponsor_balance_stroops, 42_500);
        assert_eq!(quota.remaining_tx_count, 1);

        let token_client = TokenClient::new(&env, &native_token);
        assert_eq!(token_client.balance(&relayer), 7_500);
        assert_eq!(token_client.balance(&contract_id), 92_500);
    }
}
