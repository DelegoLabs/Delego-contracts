use soroban_sdk::{contracttype, contracterror, contractevent, symbol_short!, Address, Env;};

/// Merchant verification tier.
///
/// Tiers advance as merchants accumulate sales volume and bond collateral while
/// maintaining a low dispute rate. A dispute rate spike downgrades the tier.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MerchantTier {
    Unverified = 0,
    Bronze = 1,
    Silver = 2,
    Gold = 3,
    Platinum = 4,
}

/// Cumulative merchant statistics used to evaluate the tier.
///
/// - `volume_stroops`: cumulative sales volume in stroops (1 XLM = 10^3 stroops).
/// - `disputes`: number of disputes raised against the merchant.
/// - `total_transactions`: total number of completed transactions.
/// - `bond_stroops`: collateral bonded by the merchant in stroops.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MerchantStats {
    pub volume_stroops: i128,
    pub disputes: u32,
    pub total_transactions: u32,
    pub bond_stroops: i128,
}

/// Threshold requirements for a tier.
///
/// A merchant qualifies for a tier when all conditions are met:
/// volume >= `min_volume_stroops`, bond >= `min_bond_stroops`, and the
/// dispute rate is at most `max_dispute_basis_points` (1% = 100 bps).
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TierThreshold {
    pub min_volume_stroops: i128,
    pub min_bond_stroops: i128,
    pub max_dispute_basis_points: u32,
    pub min_transactions: u32,
}

/// Event emitted whenever a merchant's tier transitions.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantTierUpdated {
    pub merchant: Address,
    pub previous_tier: MerchantTier,
    pub new_tier: MerchantTier,
    pub volume_stroops: i128,
    pub dispute_basis_points: u32,
}

const BASIS_POINTS_DENOM: u32 = 10_000;

/// Return the threshold requirements for a given tier.
///
/// Thresholds are explicit and monotonically increasing so that higher tiers
/// always demand at least as much as lower tiers.
/// - Bronze: 1,000 XLM volume, 500 XLM bond, <= 5% dispute rate.
/// - Silver: 5,000 XLM volume, 2,000 XLM bond, <= 2.5% dispute rate.
/// - Gold: 10,000 XLM volume, 5,000 XLM bond, <= 1% dispute rate.
/// - Platinum: 50,000 XLM volume, 20,000 XLM bond, <= 0.5% dispute rate.
pub fn threshold_for(tier: MerchantTier) -> TierThreshold {
    match tier {
        MerchantTier::Unverified => TierThreshold {
            min_volume_stroops: 0,
            min_bond_stroops: 0,
            max_dispute_basis_points: BASIS_POINTS_DENOM,
            min_transactions: 0,
        },
        MerchantTier::Bronze => TierThreshold {
            min_volume_stroops: 1_000 * 10_000_000,
            min_bond_stroops: 500 * 10_000_000,
            max_dispute_basis_points: 500,
            min_transactions: 5,
        },
        MerchantTier::Silver => TierThreshold {
            min_volume_stroops: 5_000 * 10_000_000,
            min_bond_stroops: 2_000 * 10_000_000,
            max_dispute_basis_points: 250,
            min_transactions: 25,
        },
        MerchantTier::Gold => TierThreshold {
            min_volume_stroops: 10_000 * 10_000_000,
            min_bond_stroops: 5_000 * 10_000_000,
            max_dispute_basis_points: 100,
            min_transactions: 100,
        },
        MerchantTier::Platinum => TierThreshold {
            min_volume_stroops: 50_000 * 10_000_000,
            min_bond_stroops: 20_000 * 10_000_000,
            max_dispute_basis_points: 50,
            min_transactions: 500,
        },
    }
}

/// Compute the dispute rate in basis-points (1% = 100 bps).
///
/// Returns `BASIS_POINTS_DENOM` if there are no transactions yet, so that
/// merchants with no history are not penalized by a zero-division artifact.
pub fn dispute_rate_basis_points(stats: &MerchantStats) -> u32 {
    if stats.total_transactions == 0 {
        return BASIS_POINTS_DENOM;
    }
    let numerator = (stats.disputes as u64) * (BASIS_POINTS_DENOM as u64);
    let rate = numerator / (stats.total_transactions as u64);
    rate as u32
}

/// Return true when the merchant meets every requirement for the given tier.
pub fn meets_threshold(stats: &MerchantStats, tier: MerchantTier) -> bool {
    let threshold = threshold_for(tier);
    if stats.volume_stroops < threshold.min_volume_stroops {
        return false;
    }
    if stats.bond_stroops < threshold.min_bond_stroops {
        return false;
    }
    if stats.total_transactions < threshold.min_transactions {
        return false;
    }
    if dispute_rate_basis_points(stats) > threshold.max_dispute_basis_points {
        return false;
    }
    true
}

/// Evaluate the highest tier the merchant qualifies for given its stats.
///
/// This is the core state machine transition function. Because higher tiers
/// require stricter dispute rates and higher volume/bond, a dispute rate spike
/// naturally results in a lower evaluated tier (downgrade).
pub fn evaluate_tier(stats: &MerchantStats) -> MerchantTier {
    if meets_threshold(stats, MerchantTier::Platinum) {
        return MerchantTier::Platinum;
    }
    if meets_threshold(stats, MerchantTier::Gold) {
        return MerchantTier::Gold;
    }
    if meets_threshold(stats, MerchantTier::Silver) {
        return MerchantTier::Silver;
    }
    if meets_threshold(stats, MerchantTier::Bronze) {
        return MerchantTier::Bronze;
    }
    MerchantTier::Unverified
}

/// Evaluate the new tier and emit a `MerchantTierUpdated` event if it changed.
///
/// Returns the new tier. The event is only emitted on an actual transition,
/// so consumers can rely on the event stream to track tier changes.
pub fn apply_tier_update(
    env: &Env,
    merchant: &Address,
    previous_tier: MerchantTier,
    stats: &MarchantStats,
) -> MerchantTier {
    let new_tier = evaluate_tier(stats);
    if new_tier != previous_tier {
        env.events().publish(
            (
                symbol_short!("tier_upd"),
                merchant.clone(),
            ),
            MerchantTierUpdated {
                merchant: merchant.clone(),
                previous_tier,
                new_tier,
                volume_stroops: stats.volume_stroops,
                dispute_basis_points: dispute_rate_basis_points(stats),
            },
        );
    }
    new_tier
}

#[cfg(all(test, any()))]
mod tests {
    use super::*;
    use soroban_sdk::{symbol_short!, Env as _, Symbol as _};

    fn stats(volume: i128, disputes: u32, txs: u32, bond: i128) -> MerchantStats {
        MerchantStats {
            volume_stroops: volume,
            disputes,
            total_transactions: txs,
            bond_stroops: bond,
        }
    }

    #[test]
    fn unverified_when_no_history() {
        let s = stats(0, 0, 0, 0);
        assert_eq(evaluate_tier(&s), MerchantTier::Unverified);
    }

    #[test]
    fn bronze_threshold() {
        let s = stats(1_000 * 10_000_000, 0, 5, 500 * 10_000_000);
        assert_eq(evaluate_tier(&s), MerchantTier::Bronze);
    }

    #[test]
    fn gold_threshold() {
        let s = stats(10_000 * 10_000_000, 1, 100, 5_000 * 10_000_000);
        assert_eq(evaluate_tier(&s), MerchantTier::Gold);
    }

    #[test]
    fn platinum_threshold() {
        let s = stats(50_000 * 10_000_000, 2, 500, 20_000 * 10_000_000);
        assert_eq(evaluate_tier(&s), MerchantTier::Platinum);
    }

    #[test]
    fn dispute_spike_downgrades() {
        // Gold-level volume and bond, but dispute rate above 1%.
        let s = stats(10_000 * 10_000_000, 2, 100, 5_000 * 10_000_000);
        assert_eq(evaluate_tier(&s), MerchantTier::Silver);
    }

    #[test]
    fn dispute_rate_computation() {
        let s = stats(0, 1, 100, 0);
        assert_eq(dispute_rate_basis_points(&s), 100);
    }

    #[test]
    fn event_emitted_on_transition() {
        let env = Env::default();
        let merchant = Address::generate(&testutils::env::Default::default());
        let s = stats(1_000 * 10_000_000, 0, 5, 500 * 10_000_000);
        let new = apply_tier_update(&env, &merchant, MerchantTier::Unverified, &s);
        assert_eq(new, MerchantTier::Bronze);
        let events = env.events().all();
        assert_eq(events.len(), 1);
    }

    #[test]
    fn no_event_when_unchanged() {
        let env = Env::default();
        let merchant = Address::generate(&testutils::env::Default::default());
        let s = stats(0, 0, 0, 0);
        let new = apply_tier_update(&env, &merchant, MerchantTier::Unverified, &s);
        assert_eq(new, MerchantTier::Unverified);
        assert_eq(env.events().all().len(), 0);
    }
}
