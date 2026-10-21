//! Oracle client interface and price conversion for Delego Marketplace.
//!
//! Queries a trusted Soroban price oracle for asset pricing in USD,
//! validates staleness against the current ledger timestamp,
//! and converts fiat cent values to equivalent token stroops.

use soroban_sdk::{contractclient, contracttype, Address, Env};

use crate::MarketplaceError;

/// Maximum allowable difference between the oracle price timestamp and the current ledger timestamp.
pub const ORACLE_MAX_STALENESS_SECONDS: u64 = 300;

/// Number of stroops in 1 whole XLM (7 decimals).
pub const STROOPS_PER_XLM: i128 = 10_000_000;

/// Oracle price data returned by a trusted Soroban price oracle.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OraclePriceData {
    /// Price of 1 base token unit (or 1 full token) in USD scaled by `decimals`.
    pub price: i128,
    /// Unix timestamp (in seconds) when the price was recorded by the oracle.
    pub timestamp: u64,
    /// Number of decimals used to scale `price`.
    pub decimals: u32,
}

/// Cross-contract client interface for querying a trusted Soroban price oracle.
#[contractclient(name = "PriceOracleClient")]
pub trait PriceOracleInterface {
    /// Fetches the current price data for the specified asset symbol or address.
    fn lastprice(env: Env, asset: Address) -> Option<OraclePriceData>;
}

/// Verifies that the oracle price timestamp is within 300 seconds of the current ledger timestamp.
///
/// # Errors
/// * [`MarketplaceError::StaleOraclePrice`] if the price timestamp is older than 300 seconds,
///   or if the timestamp is more than 300 seconds in the future.
pub fn verify_price_staleness(
    price_timestamp: u64,
    current_ledger_timestamp: u64,
) -> Result<(), MarketplaceError> {
    let diff = if current_ledger_timestamp >= price_timestamp {
        current_ledger_timestamp - price_timestamp
    } else {
        price_timestamp - current_ledger_timestamp
    };

    if diff > ORACLE_MAX_STALENESS_SECONDS {
        return Err(MarketplaceError::StaleOraclePrice);
    }

    Ok(())
}

/// Converts a fiat value in US cents to equivalent token stroops (7 decimals, 1 token = 10,000,000 stroops).
///
/// # Arguments
/// * `fiat_cents` - The amount in fiat cents (1 USD = 100 cents).
/// * `oracle_data` - The oracle price data containing price, timestamp, and decimals.
/// * `current_ledger_timestamp` - The current ledger timestamp to verify staleness against.
///
/// # Returns
/// * `Ok(stroops)` - The equivalent token amount in stroops.
/// * `Err(MarketplaceError::StaleOraclePrice)` - If the oracle data timestamp is stale (> 300s).
/// * `Err(MarketplaceError::InvalidParam)` - If `fiat_cents <= 0` or `oracle_data.price <= 0`.
pub fn fiat_cents_to_stroops(
    fiat_cents: i128,
    oracle_data: &OraclePriceData,
    current_ledger_timestamp: u64,
) -> Result<i128, MarketplaceError> {
    if fiat_cents <= 0 || oracle_data.price <= 0 {
        return Err(MarketplaceError::InvalidParam);
    }

    verify_price_staleness(oracle_data.timestamp, current_ledger_timestamp)?;

    // We need to compute:
    // (fiat_cents * 10^oracle_data.decimals * STROOPS_PER_XLM) / (100 * oracle_data.price)
    //
    // Let's compute scale = 10^decimals
    let mut scale: i128 = 1;
    for _ in 0..oracle_data.decimals {
        scale = scale
            .checked_mul(10)
            .ok_or(MarketplaceError::InvalidParam)?;
    }

    // numerator = fiat_cents * scale * STROOPS_PER_XLM
    // denominator = 100 * oracle_data.price
    //
    // Simplify 100 with STROOPS_PER_XLM (10_000_000 / 100 = 100_000)
    // numerator = fiat_cents * scale * 100_000
    // denominator = oracle_data.price
    //
    // Use checked arithmetic to prevent overflow.
    // If standard i128 arithmetic could overflow for very large numbers or high decimals,
    // we can use checked arithmetic.
    let cents_scaled = fiat_cents
        .checked_mul(scale)
        .ok_or(MarketplaceError::InvalidParam)?;

    let numerator = cents_scaled
        .checked_mul(100_000)
        .ok_or(MarketplaceError::InvalidParam)?;

    let stroops = numerator / oracle_data.price;

    Ok(stroops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_staleness_within_300_seconds_accepted() {
        let current_time = 1_000_000u64;

        // Exact same timestamp
        assert_eq!(verify_price_staleness(current_time, current_time), Ok(()));

        // 299 seconds in the past
        assert_eq!(
            verify_price_staleness(current_time - 299, current_time),
            Ok(())
        );

        // Exactly 300 seconds in the past
        assert_eq!(
            verify_price_staleness(current_time - 300, current_time),
            Ok(())
        );

        // 100 seconds in the future (within clock drift)
        assert_eq!(
            verify_price_staleness(current_time + 100, current_time),
            Ok(())
        );

        // Exactly 300 seconds in the future
        assert_eq!(
            verify_price_staleness(current_time + 300, current_time),
            Ok(())
        );
    }

    #[test]
    fn test_staleness_exceeding_300_seconds_rejected() {
        let current_time = 1_000_000u64;

        // 301 seconds old
        assert_eq!(
            verify_price_staleness(current_time - 301, current_time),
            Err(MarketplaceError::StaleOraclePrice)
        );

        // 1000 seconds old
        assert_eq!(
            verify_price_staleness(current_time - 1000, current_time),
            Err(MarketplaceError::StaleOraclePrice)
        );

        // 301 seconds in the future
        assert_eq!(
            verify_price_staleness(current_time + 301, current_time),
            Err(MarketplaceError::StaleOraclePrice)
        );
    }

    #[test]
    fn test_fiat_cents_to_stroops_conversion() {
        let current_time = 1_000_000u64;

        // Example 1:
        // Token price = $0.10 (10 cents)
        // Oracle price with 7 decimals: 0.10 * 10^7 = 1_000_000
        // Item price = 100 cents ($1.00)
        // Expected stroops = 10 tokens = 10 * 10_000_000 = 100_000_000 stroops
        let oracle_data = OraclePriceData {
            price: 1_000_000, // $0.10 with 7 decimals
            timestamp: current_time - 100,
            decimals: 7,
        };

        let stroops = fiat_cents_to_stroops(100, &oracle_data, current_time).unwrap();
        assert_eq!(stroops, 100_000_000);

        // Example 2:
        // Token price = $1.00 (e.g. USDC)
        // Decimals = 6
        // Oracle price = 1_000_000 ($1.00)
        // Item price = 250 cents ($2.50)
        // Expected: 2.5 tokens = 25_000_000 stroops
        let oracle_usdc = OraclePriceData {
            price: 1_000_000,
            timestamp: current_time - 50,
            decimals: 6,
        };
        let stroops_usdc = fiat_cents_to_stroops(250, &oracle_usdc, current_time).unwrap();
        assert_eq!(stroops_usdc, 25_000_000);

        // Example 3:
        // Token price = $0.125
        // Decimals = 3
        // Oracle price = 125
        // Item price = 50 cents ($0.50)
        // Expected: 4 tokens = 40_000_000 stroops
        let oracle_custom = OraclePriceData {
            price: 125,
            timestamp: current_time,
            decimals: 3,
        };
        let stroops_custom = fiat_cents_to_stroops(50, &oracle_custom, current_time).unwrap();
        assert_eq!(stroops_custom, 40_000_000);
    }

    #[test]
    fn test_fiat_cents_to_stroops_rejects_stale_price() {
        let current_time = 1_000_000u64;
        let stale_oracle = OraclePriceData {
            price: 1_000_000,
            timestamp: current_time - 301,
            decimals: 7,
        };

        let result = fiat_cents_to_stroops(100, &stale_oracle, current_time);
        assert_eq!(result, Err(MarketplaceError::StaleOraclePrice));
    }

    #[test]
    fn test_fiat_cents_to_stroops_invalid_params() {
        let current_time = 1_000_000u64;
        let valid_oracle = OraclePriceData {
            price: 1_000_000,
            timestamp: current_time,
            decimals: 7,
        };

        // Zero or negative fiat cents
        assert_eq!(
            fiat_cents_to_stroops(0, &valid_oracle, current_time),
            Err(MarketplaceError::InvalidParam)
        );
        assert_eq!(
            fiat_cents_to_stroops(-100, &valid_oracle, current_time),
            Err(MarketplaceError::InvalidParam)
        );

        // Zero or negative price
        let zero_price_oracle = OraclePriceData {
            price: 0,
            timestamp: current_time,
            decimals: 7,
        };
        assert_eq!(
            fiat_cents_to_stroops(100, &zero_price_oracle, current_time),
            Err(MarketplaceError::InvalidParam)
        );

        let neg_price_oracle = OraclePriceData {
            price: -500,
            timestamp: current_time,
            decimals: 7,
        };
        assert_eq!(
            fiat_cents_to_stroops(100, &neg_price_oracle, current_time),
            Err(MarketplaceError::InvalidParam)
        );
    }
}
