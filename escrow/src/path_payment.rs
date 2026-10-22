//! Multi-hop path payment resolution with slippage bounds.
//!
//! When a buyer pays in one asset (e.g. USDC) and the merchant settles in
//! another (e.g. XLM), the realised fill must respect a minimum destination
//! floor. Unused source tokens above what the route consumed are refunded to
//! the buyer.

use soroban_sdk::{contracttype, token, Address, Env};

use crate::EscrowError;

/// Arguments describing a cross-currency path payment and its slippage window.
#[contracttype]
#[derive(Clone, Debug)]
pub struct PathPaymentArgs {
    /// Order / escrow identifier the payment settles.
    pub order_id: u64,
    /// Token the buyer funded (path input).
    pub source_token: Address,
    /// Token the merchant receives (path output).
    pub destination_token: Address,
    /// Strict maximum amount of `source_token` the route may spend.
    pub max_source_amount: i128,
    /// Quoted destination amount used for off-chain reference.
    pub expected_destination_amount: i128,
    /// Slippage floor: minimum acceptable amount of `destination_token`.
    pub min_destination_amount: i128,
}

/// Validate a realised path-payment fill against its slippage bounds and refund
/// any unused source tokens to the buyer.
///
/// Checks the strict maximum-send (`source_spent <= max_source_amount`) and
/// minimum-receive (`received_amount >= min_destination_amount`) window, then
/// transfers `max_source_amount - source_spent` of `source_token` back to
/// `buyer` when the route did not consume the full allowance.
///
/// # Errors
/// - [`EscrowError::InvalidAmount`] — non-positive bound or fill amounts.
/// - [`EscrowError::SlippageExceeded`] — fill is outside the send/receive window.
pub fn resolve_path_payment(
    env: &Env,
    args: &PathPaymentArgs,
    buyer: &Address,
    source_spent: i128,
    received_amount: i128,
) -> Result<(), EscrowError> {
    if args.max_source_amount <= 0
        || args.min_destination_amount <= 0
        || args.expected_destination_amount <= 0
        || source_spent <= 0
        || received_amount <= 0
    {
        return Err(EscrowError::InvalidAmount);
    }
    if source_spent > args.max_source_amount {
        return Err(EscrowError::SlippageExceeded);
    }

    assert_min_destination(received_amount, args.min_destination_amount)?;

    let surplus = args.max_source_amount - source_spent;
    if surplus > 0 {
        token::Client::new(env, &args.source_token).transfer(
            &env.current_contract_address(),
            buyer,
            &surplus,
        );
    }

    Ok(())
}

/// Assert that the realised destination amount meets the slippage floor.
pub fn assert_min_destination(
    received_amount: i128,
    min_destination_amount: i128,
) -> Result<(), EscrowError> {
    if received_amount < min_destination_amount {
        return Err(EscrowError::SlippageExceeded);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        testutils::Address as _,
        token::{Client as TokenClient, StellarAssetClient},
        Env,
    };

    fn mint_source(env: &Env, holder: &Address, amount: i128) -> Address {
        let admin = Address::generate(env);
        let token = env
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        StellarAssetClient::new(env, &token).mint(holder, &amount);
        token
    }

    #[test]
    fn reverts_when_received_amount_is_below_slippage_floor() {
        let env = Env::default();
        env.mock_all_auths();
        let buyer = Address::generate(&env);
        let dest = Address::generate(&env);
        let host = Address::generate(&env);
        let source = mint_source(&env, &host, 1_000);

        let args = PathPaymentArgs {
            order_id: 1,
            source_token: source,
            destination_token: dest,
            max_source_amount: 1_000,
            expected_destination_amount: 500,
            min_destination_amount: 480,
        };

        let err = env
            .as_contract(&host, || {
                resolve_path_payment(&env, &args, &buyer, 900, 479)
            })
            .unwrap_err();
        assert_eq!(err, EscrowError::SlippageExceeded);
        assert_eq!(TokenClient::new(&env, &args.source_token).balance(&buyer), 0);
    }

    #[test]
    fn refunds_unused_source_tokens_to_buyer() {
        let env = Env::default();
        env.mock_all_auths();
        let buyer = Address::generate(&env);
        let dest = Address::generate(&env);
        let host = Address::generate(&env);
        let source = mint_source(&env, &host, 1_000);

        let args = PathPaymentArgs {
            order_id: 7,
            source_token: source.clone(),
            destination_token: dest,
            max_source_amount: 1_000,
            expected_destination_amount: 500,
            min_destination_amount: 480,
        };

        env.as_contract(&host, || {
            resolve_path_payment(&env, &args, &buyer, 750, 490).unwrap();
        });

        assert_eq!(TokenClient::new(&env, &source).balance(&buyer), 250);
        assert_eq!(TokenClient::new(&env, &source).balance(&host), 750);
    }

    #[test]
    fn accepts_exact_floor_with_no_surplus() {
        let env = Env::default();
        env.mock_all_auths();
        let buyer = Address::generate(&env);
        let dest = Address::generate(&env);
        let host = Address::generate(&env);
        let source = mint_source(&env, &host, 1_000);

        let args = PathPaymentArgs {
            order_id: 2,
            source_token: source.clone(),
            destination_token: dest,
            max_source_amount: 1_000,
            expected_destination_amount: 500,
            min_destination_amount: 480,
        };

        env.as_contract(&host, || {
            resolve_path_payment(&env, &args, &buyer, 1_000, 480).unwrap();
        });

        assert_eq!(TokenClient::new(&env, &source).balance(&buyer), 0);
        assert_eq!(TokenClient::new(&env, &source).balance(&host), 1_000);
    }
}
