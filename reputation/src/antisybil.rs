//! Anti-Sybil reviewer weight evaluation and escrow order cross-referencing.
//!
//! Sybil rings create disposable buyer accounts to leave fake 5-star reviews.
//! Reviews from accounts younger than 30 days or with fewer than 3 completed escrows
//! must be discounted by 80%.

use soroban_sdk::{
    contracttype, vec, Address, Env, IntoVal, InvokeError, Symbol,
};

use crate::EscrowRecordMirror;

/// Calculates the reviewer weight based on account age (in ledgers) and completed orders count.
///
/// Weight allocation:
/// - Established accounts (> 500,000 ledgers and >= 5 completed orders): 100% weight (100)
/// - Intermediate accounts (>= 2 completed orders): 50% weight (50)
/// - Brand new accounts (< 500,000 ledgers or < 2 completed orders): 20% weight (20)
///   (effectively discounted by 80%).
pub fn calculate_reviewer_weight(account_age_ledgers: u32, completed_orders: u32) -> u32 {
    if account_age_ledgers > 500_000 && completed_orders >= 5 {
        100 // Full weight
    } else if completed_orders >= 2 {
        50  // Half weight
    } else {
        20  // 20% weight for brand new accounts
    }
}

/// Paginated page of escrow records mirrored from the escrow contract.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowListPageMirror {
    pub items: soroban_sdk::Vec<EscrowRecordMirror>,
    pub total: u32,
    pub next_offset: Option<u32>,
}

/// Cross-references a buyer's completed escrow order count from the escrow contract.
///
/// Queries `list_escrows_by_buyer` on the escrow contract across pages, counting how many
/// escrows are completed (released).
pub fn get_buyer_completed_orders(
    env: &Env,
    escrow_contract: &Address,
    buyer: &Address,
) -> Result<u32, InvokeError> {
    let mut completed_count: u32 = 0;
    let mut offset: u32 = 0;
    let page_limit: u32 = 50;

    loop {
        let args = vec![
            env,
            buyer.into_val(env),
            offset.into_val(env),
            page_limit.into_val(env),
        ];

        let page: EscrowListPageMirror = env
            .try_invoke_contract(escrow_contract, &Symbol::new(env, "list_escrows_by_buyer"), args)?
            .map_err(|_| InvokeError::Contract(0))?;

        for record in page.items.iter() {
            if record.status == crate::EscrowStatusMirror::Released {
                completed_count = completed_count.saturating_add(1);
            }
        }

        match page.next_offset {
            Some(next) if next > offset => {
                offset = next;
            }
            _ => break,
        }
    }

    Ok(completed_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_reviewer_weight() {
        // Brand new account: young age (< 500_000) and 0 completed orders -> 20% weight
        assert_eq!(calculate_reviewer_weight(0, 0), 20);
        assert_eq!(calculate_reviewer_weight(100_000, 1), 20);
        assert_eq!(calculate_reviewer_weight(500_000, 1), 20);

        // Young account (< 500_000 ledgers) but with 2 to 4 completed orders -> 50% weight
        assert_eq!(calculate_reviewer_weight(100_000, 2), 50);
        assert_eq!(calculate_reviewer_weight(100_000, 4), 50);

        // Old account (> 500_000 ledgers) but only 2 to 4 completed orders -> 50% weight
        assert_eq!(calculate_reviewer_weight(600_000, 2), 50);
        assert_eq!(calculate_reviewer_weight(600_000, 4), 50);

        // Established account: > 500_000 ledgers and >= 5 completed orders -> 100% full weight
        assert_eq!(calculate_reviewer_weight(500_001, 5), 100);
        assert_eq!(calculate_reviewer_weight(1_000_000, 10), 100);

        // Edge case: Exactly 500_000 ledgers and 5 completed orders is not > 500_000, so 50% weight
        assert_eq!(calculate_reviewer_weight(500_000, 5), 50);
    }
}
