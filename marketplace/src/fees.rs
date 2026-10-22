//! Fee discount calculator for merchants staking platform governance tokens.
//!
//! Merchants staking platform governance tokens in the marketplace qualify for
//! proportional reductions in platform commission fees.

/// Tier 1 governance stake threshold (100,000 tokens with 7 decimals = 1,000,000,000,000 stroops).
pub const TIER_1_STAKE_THRESHOLD: i128 = 100_000_0000000;

/// Tier 2 governance stake threshold (10,000 tokens with 7 decimals = 100,000,000,000 stroops).
pub const TIER_2_STAKE_THRESHOLD: i128 = 10_000_0000000;

/// Fee discount for Tier 1 stakers in basis points (1.5% = 150 bps).
pub const TIER_1_DISCOUNT_BPS: u32 = 150;

/// Fee discount for Tier 2 stakers in basis points (0.75% = 75 bps).
pub const TIER_2_DISCOUNT_BPS: u32 = 75;

/// Calculates the discounted commission fee in basis points based on a merchant's
/// staked platform governance tokens.
///
/// # Arguments
/// * `base_fee_bps` - The merchant's base commission rate in basis points.
/// * `governance_stake` - The amount of governance tokens staked by the merchant (in stroops).
///
/// # Returns
/// * The discounted fee in basis points, floored at `0` using saturating subtraction.
pub fn calculate_discounted_fee(base_fee_bps: u32, governance_stake: i128) -> u32 {
    if governance_stake >= 100_000_0000000 {
        base_fee_bps.saturating_sub(150) // 1.5% discount
    } else if governance_stake >= 10_000_0000000 {
        base_fee_bps.saturating_sub(75) // 0.75% discount
    } else {
        base_fee_bps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DataKey, MarketplaceContract, MarketplaceContractClient, RegisterParams,
    };
    use soroban_sdk::{
        symbol_short,
        testutils::Address as _,
        Address, Env, String,
    };

    #[test]
    fn test_no_discount_below_minimum_stake_tier() {
        assert_eq!(calculate_discounted_fee(300, 0), 300);
        assert_eq!(calculate_discounted_fee(300, -500), 300);
        assert_eq!(calculate_discounted_fee(300, 9_999_9999999), 300);
    }

    #[test]
    fn test_tier_2_discount_75_bps() {
        // Exact boundary for 10,000 token stake tier (0.75% = 75 bps discount)
        assert_eq!(calculate_discounted_fee(300, 10_000_0000000), 225);
        // Mid-tier stake
        assert_eq!(calculate_discounted_fee(300, 50_000_0000000), 225);
        // Just below Tier 1 boundary
        assert_eq!(calculate_discounted_fee(300, 99_999_9999999), 225);
    }

    #[test]
    fn test_tier_1_discount_150_bps() {
        // Exact boundary for 100,000 token stake tier (1.5% = 150 bps discount)
        assert_eq!(calculate_discounted_fee(300, 100_000_0000000), 150);
        // Above Tier 1 boundary
        assert_eq!(calculate_discounted_fee(500, 250_000_0000000), 350);
    }

    #[test]
    fn test_cannot_reduce_fee_below_zero_bps() {
        // Tier 2 saturating_sub floor at 0
        assert_eq!(calculate_discounted_fee(75, 10_000_0000000), 0);
        assert_eq!(calculate_discounted_fee(50, 10_000_0000000), 0);
        assert_eq!(calculate_discounted_fee(0, 10_000_0000000), 0);

        // Tier 1 saturating_sub floor at 0
        assert_eq!(calculate_discounted_fee(150, 100_000_0000000), 0);
        assert_eq!(calculate_discounted_fee(100, 100_000_0000000), 0);
        assert_eq!(calculate_discounted_fee(0, 100_000_0000000), 0);
    }

    #[test]
    fn test_get_merchant_fee_bps_across_all_stake_tiers() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let contract_id = env.register(MarketplaceContract, (admin.clone(),));
        let client = MarketplaceContractClient::new(&env, &contract_id);

        let merchant_owner = Address::generate(&env);
        let params = RegisterParams {
            name: String::from_str(&env, "Staked Merchant"),
            description: String::from_str(&env, "Merchant staking governance tokens"),
            category: symbol_short!("retail"),
            image_url: String::from_str(&env, "https://example.com/logo.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        };

        let merchant_id = crate::testutil::register_merchant_with_deposit(
            &env,
            &client,
            &merchant_owner,
            &params,
        );

        // Base commission = 300 bps (3.00%)
        client.set_merchant_commission(&merchant_id, &admin, &300);

        // Tier 0: No stake (0) -> 300 bps
        assert_eq!(client.get_merchant_fee_bps(&merchant_owner), 300);

        // Below Tier 2 threshold (9,999.9999999 tokens) -> 300 bps
        client.set_governance_stake(&admin, &merchant_owner, &9_999_9999999);
        assert_eq!(client.get_merchant_fee_bps(&merchant_owner), 300);

        // Tier 2: 10,000 tokens (10_000_0000000) -> 300 - 75 = 225 bps
        client.set_governance_stake(&admin, &merchant_owner, &10_000_0000000);
        assert_eq!(client.get_merchant_fee_bps(&merchant_owner), 225);

        // Tier 2 upper bound: 99,999.9999999 tokens -> 225 bps
        client.set_governance_stake(&admin, &merchant_owner, &99_999_9999999);
        assert_eq!(client.get_merchant_fee_bps(&merchant_owner), 225);

        // Tier 1: 100,000 tokens (100_000_0000000) -> 300 - 150 = 150 bps
        client.set_governance_stake(&admin, &merchant_owner, &100_000_0000000);
        assert_eq!(client.get_merchant_fee_bps(&merchant_owner), 150);

        // Floor at 0 bps when discount exceeds base fee
        client.set_merchant_commission(&merchant_id, &admin, &100);
        assert_eq!(client.get_merchant_fee_bps(&merchant_owner), 0);

        // Unregistered address defaults to 0 bps
        let unregistered = Address::generate(&env);
        env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .set(&DataKey::GovernanceStake(unregistered.clone()), &100_000_0000000i128);
        });
        assert_eq!(client.get_merchant_fee_bps(&unregistered), 0);
    }
}
