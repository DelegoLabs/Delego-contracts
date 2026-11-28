//! Fee calculation helpers for the escrow contract.
//!
//! Provides the base fee derivation used by the congestion-aware surcharge
//! logic in `gas_surcharge`.

/// Derives the congestion multiplier from the observed base fee relative to
/// the network's minimum base fee. Returns at least 1.
pub fn congestion_multiplier(observed_base_fee: u32, min_base_fee: u32) -> u32 {
    if min_base_fee == 0 {
        return 1;
    }
    let ratio = observed_base_fee / min_base_fee;
    ratio.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiplier_floors_at_one() {
        assert_eq!(congestion_multiplier(100, 100), 1);
        assert_eq!(congestion_multiplier(50, 100), 1);
    }

    #[test]
    fn multiplier_reflects_spike() {
        assert_eq!(congestion_multiplier(1000, 100), 10);
    }

    #[test]
    fn zero_min_fee_is_safe() {
        assert_eq!(congestion_multiplier(1000, 0), 1);
    }
}
