//! Cross-contract interface for reading merchant reputation from external DeFi contracts.
//! 
//! Allows third-party lending, insurance, and underwriting contracts to query merchant
//! reputation scores, dispute ratios, and standing status for credit assessment.

use soroban_sdk::{Address, Env};

/// Reputation reader trait for external DeFi contracts.
/// 
/// Implemented by the reputation contract to allow external contracts (lenders, insurers, etc.)
/// to evaluate merchant creditworthiness using standardized queries.
pub trait ReputationReaderTrait {
    /// Get merchant's current reputation rating (0-10000 basis points).
    ///
    /// # Arguments
    /// * `env` - Soroban environment
    /// * `merchant` - Address of merchant to query
    ///
    /// # Returns
    /// Rating in basis points (0-10000), or error if merchant not found
    fn get_merchant_rating(env: Env, merchant: Address) -> Result<u32, u32>;

    /// Get merchant's dispute ratio (disputes / total transactions * 10000).
    ///
    /// # Arguments
    /// * `env` - Soroban environment
    /// * `merchant` - Address of merchant to query
    ///
    /// # Returns
    /// Dispute ratio in basis points (0-10000), or error
    fn get_dispute_ratio(env: Env, merchant: Address) -> Result<u32, u32>;

    /// Check if merchant is in good standing (not frozen, score > threshold).
    ///
    /// # Arguments
    /// * `env` - Soroban environment
    /// * `merchant` - Address of merchant to query
    ///
    /// # Returns
    /// True if merchant is in good standing, false otherwise or error
    fn is_merchant_in_good_standing(env: Env, merchant: Address) -> Result<bool, u32>;

    /// Get total number of transactions for a merchant.
    ///
    /// # Arguments
    /// * `env` - Soroban environment
    /// * `merchant` - Address of merchant to query
    ///
    /// # Returns
    /// Total transaction count, or error if merchant not found
    fn get_merchant_transaction_count(env: Env, merchant: Address) -> Result<u64, u32>;

    /// Get average rating for a merchant (0-10000 basis points).
    ///
    /// # Arguments
    /// * `env` - Soroban environment
    /// * `merchant` - Address of merchant to query
    ///
    /// # Returns
    /// Average rating in basis points, or error if merchant not found
    fn get_merchant_average_rating(env: Env, merchant: Address) -> Result<u32, u32>;
}
