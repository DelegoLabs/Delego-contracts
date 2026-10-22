//! Sub-order atomic resolution routing for multi-merchant bundles.
//!
//! A parent escrow may hold one deposit that covers several merchant
//! sub-orders. Each sub-order tracks its own status so a release or dispute
//! for merchant A never locks merchant B's share. The parent reaches a
//! terminal status only once every sub-order is terminal, and the sum of
//! sub-order amounts must always equal the deposited collateral.

use crate::{DataKey, EscrowError, EscrowRecord, EscrowStatus, EscrowTerminalState};
use soroban_sdk::{contracttype, Address, Env, Vec};

/// Maximum number of merchant sub-orders in one parent escrow.
pub const MAX_SUB_ORDERS: u32 = 64;

/// One merchant slice of a multi-merchant bundled escrow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubOrder {
    /// Caller-chosen identifier unique within the parent escrow.
    pub sub_order_id: u64,
    /// Merchant that receives this slice on release.
    pub merchant: Address,
    /// Portion of the parent deposit reserved for this merchant.
    pub amount: i128,
    /// Independent lifecycle status for this slice.
    pub status: EscrowStatus,
}

#[contracttype]
#[derive(Clone)]
enum SubOrderKey {
    /// `Vec<SubOrder>` registered under a parent escrow id.
    Orders(u64),
}

fn bump(env: &Env, key: &SubOrderKey) {
    env.storage().persistent().extend_ttl(key, 17_280, 518_400);
}

fn load(env: &Env, escrow_id: u64) -> Result<Vec<SubOrder>, EscrowError> {
    let key = SubOrderKey::Orders(escrow_id);
    let orders: Vec<SubOrder> = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(EscrowError::NotFound)?;
    bump(env, &key);
    Ok(orders)
}

fn store(env: &Env, escrow_id: u64, orders: &Vec<SubOrder>) {
    let key = SubOrderKey::Orders(escrow_id);
    env.storage().persistent().set(&key, orders);
    bump(env, &key);
}

fn find_index(orders: &Vec<SubOrder>, sub_order_id: u64) -> Result<u32, EscrowError> {
    for i in 0..orders.len() {
        if orders.get(i).ok_or(EscrowError::NotFound)?.sub_order_id == sub_order_id {
            return Ok(i);
        }
    }
    Err(EscrowError::NotFound)
}

fn is_terminal(status: &EscrowStatus) -> bool {
    EscrowTerminalState::from_status(status).is_some()
}

/// Sum of all sub-order amounts; errors on overflow.
pub fn sum_amounts(orders: &Vec<SubOrder>) -> Result<i128, EscrowError> {
    let mut total: i128 = 0;
    for i in 0..orders.len() {
        let order = orders.get(i).ok_or(EscrowError::NotFound)?;
        total = total
            .checked_add(order.amount)
            .ok_or(EscrowError::InvalidAmount)?;
    }
    Ok(total)
}

/// True when every registered sub-order is in a terminal status.
pub fn all_terminal(orders: &Vec<SubOrder>) -> bool {
    if orders.is_empty() {
        return false;
    }
    for i in 0..orders.len() {
        let Some(order) = orders.get(i) else {
            return false;
        };
        if !is_terminal(&order.status) {
            return false;
        }
    }
    true
}

/// Parent status once every sub-order is terminal.
///
/// Mixed terminal outcomes (some released, some refunded) resolve to
/// `Released` whenever any slice was paid out; otherwise `Refunded`.
pub fn parent_terminal_status(orders: &Vec<SubOrder>) -> Option<EscrowStatus> {
    if !all_terminal(orders) {
        return None;
    }
    let mut any_released = false;
    for i in 0..orders.len() {
        let order = orders.get(i)?;
        if order.status == EscrowStatus::Released {
            any_released = true;
            break;
        }
    }
    Some(if any_released {
        EscrowStatus::Released
    } else {
        EscrowStatus::Refunded
    })
}

/// When every sub-order is terminal, mark the parent escrow completed.
fn complete_parent_if_ready(env: &Env, escrow_id: u64, orders: &Vec<SubOrder>) -> Option<EscrowStatus> {
    let status = parent_terminal_status(orders)?;
    let key = DataKey::Escrow(escrow_id);
    if let Some(mut record) = env.storage().persistent().get::<_, EscrowRecord>(&key) {
        record.status = status.clone();
        record.updated_at = env.ledger().timestamp();
        env.storage().persistent().set(&key, &record);
    }
    Some(status)
}

/// Register the merchant sub-orders for a parent escrow.
///
/// `deposited` is the parent collateral; it must equal the sum of sub-order
/// amounts. Each sub-order must start as `Funded` with a positive amount and
/// a unique `sub_order_id`.
pub fn register(
    env: &Env,
    escrow_id: u64,
    deposited: i128,
    orders: Vec<SubOrder>,
) -> Result<(), EscrowError> {
    if orders.is_empty() || orders.len() > MAX_SUB_ORDERS {
        return Err(EscrowError::InvalidAmount);
    }
    if deposited <= 0 {
        return Err(EscrowError::InvalidAmount);
    }

    for i in 0..orders.len() {
        let order = orders.get(i).ok_or(EscrowError::InvalidAmount)?;
        if order.amount <= 0 {
            return Err(EscrowError::InvalidAmount);
        }
        if order.status != EscrowStatus::Funded {
            return Err(EscrowError::InvalidStatus);
        }
        for j in (i + 1)..orders.len() {
            let other = orders.get(j).ok_or(EscrowError::InvalidAmount)?;
            if other.sub_order_id == order.sub_order_id {
                return Err(EscrowError::InvalidAmount);
            }
        }
    }

    let total = sum_amounts(&orders)?;
    if total != deposited {
        return Err(EscrowError::InvalidAmount);
    }

    store(env, escrow_id, &orders);
    Ok(())
}

/// Return the registered sub-orders for a parent escrow, if any.
pub fn get(env: &Env, escrow_id: u64) -> Vec<SubOrder> {
    let key = SubOrderKey::Orders(escrow_id);
    match env.storage().persistent().get(&key) {
        Some(orders) => {
            bump(env, &key);
            orders
        }
        None => Vec::new(env),
    }
}

/// Release a single funded sub-order to its merchant without touching siblings.
///
/// Returns the updated sub-order and, when every sibling is also terminal, the
/// status the parent escrow should adopt.
pub fn release(
    env: &Env,
    escrow_id: u64,
    sub_order_id: u64,
) -> Result<(SubOrder, Option<EscrowStatus>), EscrowError> {
    let mut orders = load(env, escrow_id)?;
    let idx = find_index(&orders, sub_order_id)?;
    let mut order = orders.get(idx).ok_or(EscrowError::NotFound)?;

    match order.status {
        EscrowStatus::Funded => {}
        EscrowStatus::Released => return Err(EscrowError::AlreadyReleased),
        EscrowStatus::Refunded => return Err(EscrowError::AlreadyRefunded),
        EscrowStatus::Cancelled => return Err(EscrowError::AlreadyCancelled),
        _ => return Err(EscrowError::InvalidStatus),
    }

    order.status = EscrowStatus::Released;
    orders.set(idx, order.clone());
    store(env, escrow_id, &orders);

    Ok((order, complete_parent_if_ready(env, escrow_id, &orders)))
}

/// Mark a single funded sub-order disputed without locking sibling funds.
///
/// Sibling sub-orders remain independently releasable. The parent stays
/// non-terminal until this dispute is resolved to a terminal status.
pub fn dispute(
    env: &Env,
    escrow_id: u64,
    sub_order_id: u64,
) -> Result<SubOrder, EscrowError> {
    let mut orders = load(env, escrow_id)?;
    let idx = find_index(&orders, sub_order_id)?;
    let mut order = orders.get(idx).ok_or(EscrowError::NotFound)?;

    match order.status {
        EscrowStatus::Funded => {}
        EscrowStatus::Disputed => return Err(EscrowError::InvalidStatus),
        EscrowStatus::Released => return Err(EscrowError::AlreadyReleased),
        EscrowStatus::Refunded => return Err(EscrowError::AlreadyRefunded),
        EscrowStatus::Cancelled => return Err(EscrowError::AlreadyCancelled),
        _ => return Err(EscrowError::InvalidStatus),
    }

    order.status = EscrowStatus::Disputed;
    orders.set(idx, order.clone());
    store(env, escrow_id, &orders);
    Ok(order)
}

/// Resolve a disputed sub-order to `Released` or `Refunded`.
///
/// Returns the updated sub-order and the parent terminal status when all
/// siblings have also reached a terminal state.
pub fn resolve(
    env: &Env,
    escrow_id: u64,
    sub_order_id: u64,
    outcome: EscrowStatus,
) -> Result<(SubOrder, Option<EscrowStatus>), EscrowError> {
    if !matches!(outcome, EscrowStatus::Released | EscrowStatus::Refunded) {
        return Err(EscrowError::InvalidStatus);
    }

    let mut orders = load(env, escrow_id)?;
    let idx = find_index(&orders, sub_order_id)?;
    let mut order = orders.get(idx).ok_or(EscrowError::NotFound)?;

    if order.status != EscrowStatus::Disputed {
        return Err(EscrowError::NotDisputed);
    }

    order.status = outcome;
    orders.set(idx, order.clone());
    store(env, escrow_id, &orders);

    Ok((order, complete_parent_if_ready(env, escrow_id, &orders)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn funded_pair(env: &Env) -> (Address, Address, Vec<SubOrder>) {
        let merchant_a = Address::generate(env);
        let merchant_b = Address::generate(env);
        let mut orders = Vec::new(env);
        orders.push_back(SubOrder {
            sub_order_id: 1,
            merchant: merchant_a.clone(),
            amount: 400,
            status: EscrowStatus::Funded,
        });
        orders.push_back(SubOrder {
            sub_order_id: 2,
            merchant: merchant_b.clone(),
            amount: 600,
            status: EscrowStatus::Funded,
        });
        (merchant_a, merchant_b, orders)
    }

    #[test]
    fn deposited_collateral_must_match_sub_order_sum() {
        let env = Env::default();
        let (_a, _b, orders) = funded_pair(&env);

        assert_eq!(sum_amounts(&orders).unwrap(), 1_000);
        assert!(register(&env, 1, 1_000, orders.clone()).is_ok());
        assert_eq!(
            register(&env, 2, 999, orders),
            Err(EscrowError::InvalidAmount)
        );
    }

    #[test]
    fn merchant_a_releases_while_merchant_b_is_disputed() {
        let env = Env::default();
        let (merchant_a, _merchant_b, orders) = funded_pair(&env);
        register(&env, 7, 1_000, orders).unwrap();

        let disputed = dispute(&env, 7, 2).unwrap();
        assert_eq!(disputed.status, EscrowStatus::Disputed);

        let (released, parent) = release(&env, 7, 1).unwrap();
        assert_eq!(released.merchant, merchant_a);
        assert_eq!(released.status, EscrowStatus::Released);
        assert_eq!(released.amount, 400);
        // Parent stays open: sibling is still disputed (non-terminal).
        assert_eq!(parent, None);

        let stored = get(&env, 7);
        assert_eq!(stored.get(0).unwrap().status, EscrowStatus::Released);
        assert_eq!(stored.get(1).unwrap().status, EscrowStatus::Disputed);
    }

    #[test]
    fn parent_completes_when_all_sub_orders_are_terminal() {
        let env = Env::default();
        let (_a, _b, orders) = funded_pair(&env);
        register(&env, 9, 1_000, orders).unwrap();

        dispute(&env, 9, 2).unwrap();
        let (_, parent_after_a) = release(&env, 9, 1).unwrap();
        assert_eq!(parent_after_a, None);

        let (_, parent_after_b) = resolve(&env, 9, 2, EscrowStatus::Refunded).unwrap();
        assert_eq!(parent_after_b, Some(EscrowStatus::Released));
        assert!(all_terminal(&get(&env, 9)));
    }
}
