//! Emergency liquidity unwinding for compromised DeFi pools (issue #597).
//!
//! If an integrated lending protocol is exploited or pauses withdrawals, every
//! escrow staked in it would otherwise be stuck: each settlement path redeems
//! the Blend position first, and a failing pool call aborts the settlement.
//!
//! [`EmergencyUnwindTrait::emergency_detach_defi_pool`] lets the admin set
//! isolate such a pool. Once detached:
//!
//! - new escrows can no longer opt into the pool
//!   ([`EscrowError::DefiPoolDetached`]), and escrows configured before the
//!   detach keep their deposit in the escrow vault instead of supplying it;
//! - redemptions never call the pool again. The escrow's position is written
//!   off and the buyer/merchant settlement is paid from the contract's primary
//!   escrow reserves. The written-off principal is tracked per pool
//!   ([`EscrowContract::get_stranded_principal`]) so it can be reconciled if
//!   the protocol later returns funds.
//!
//! # Authorization
//!
//! Detaching is a multisig action that reuses the contract's existing M-of-N
//! admin threshold (`set_upgrade_threshold`, at least
//! [`MIN_UPGRADE_THRESHOLD`]). Each call records the calling admin's approval;
//! the pool is detached by the call that brings valid approvals up to the
//! threshold. Approvals from addresses that have since lost admin rights do
//! not count.

use super::*;

/// Emergency entry point for isolating a compromised external DeFi pool.
pub trait EmergencyUnwindTrait {
    /// Record `admin`'s approval to detach `pool`, detaching it once the
    /// multisig threshold is met. Returns `true` when this call detached the
    /// pool and `false` when the approval was recorded but more are needed.
    fn emergency_detach_defi_pool(
        env: Env,
        admin: Address,
        pool: Address,
    ) -> Result<bool, EscrowError>;
}

/// Emitted under `(escrow, pl_dt_apr)` for each approval that does not yet
/// meet the threshold.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefiPoolDetachApproved {
    /// Pool being detached.
    pub pool: Address,
    /// Admin who approved.
    pub admin: Address,
    /// Valid approvals so far.
    pub approvals: u32,
    /// Approvals required.
    pub threshold: u32,
}

/// Emitted under `(escrow, pl_detach)` when a pool is detached.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefiPoolDetachedEvent {
    /// Pool that was detached.
    pub pool: Address,
    /// Admins whose approvals detached it.
    pub approvers: Vec<Address>,
    /// Ledger sequence of the detach.
    pub detached_at_ledger: u32,
}

#[contracttype]
#[derive(Clone)]
pub(crate) enum UnwindKey {
    DetachApprovals(Address),
}

/// Reject `pool` for new deposits if it has been detached.
pub fn assert_pool_accepting_deposits(env: &Env, pool: &Address) -> Result<(), EscrowError> {
    if blend_yield::is_pool_detached(env, pool) {
        return Err(EscrowError::DefiPoolDetached);
    }
    Ok(())
}

fn detach_approvals(env: &Env, pool: &Address) -> Vec<Address> {
    let key = UnwindKey::DetachApprovals(pool.clone());
    let approvals: Option<Vec<Address>> = env.storage().persistent().get(&key);
    if approvals.is_some() {
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
    }
    approvals.unwrap_or_else(|| Vec::new(env))
}

fn detach(env: &Env, admin: Address, pool: Address) -> Result<bool, EscrowError> {
    EscrowContract::require_admin(env, &admin)?;
    if is_zero_address(env, &pool) || pool == env.current_contract_address() {
        return Err(EscrowError::InvalidAddress);
    }
    if blend_yield::is_pool_detached(env, &pool) {
        return Err(EscrowError::DefiPoolAlreadyDetached);
    }

    let key = UnwindKey::DetachApprovals(pool.clone());
    let mut approvals = detach_approvals(env, &pool);
    if approvals.contains(&admin) {
        return Err(EscrowError::AlreadyVoted);
    }
    approvals.push_back(admin.clone());

    let valid = EscrowContract::valid_upgrade_approvals(env, &approvals);
    let threshold = EscrowContract::upgrade_threshold(env);
    if valid < threshold {
        env.storage().persistent().set(&key, &approvals);
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("pl_dt_apr")),
            DefiPoolDetachApproved {
                pool,
                admin,
                approvals: valid,
                threshold,
            },
        );
        return Ok(false);
    }

    blend_yield::detach_pool(env, &pool);
    env.storage().persistent().remove(&key);
    env.events().publish(
        (symbol_short!("escrow"), symbol_short!("pl_detach")),
        DefiPoolDetachedEvent {
            pool,
            approvers: approvals,
            detached_at_ledger: env.ledger().sequence(),
        },
    );
    Ok(true)
}

#[contractimpl]
impl EscrowContract {
    /// Approve detaching a compromised external DeFi pool; see
    /// [`EmergencyUnwindTrait::emergency_detach_defi_pool`].
    ///
    /// # Errors
    /// - [`EscrowError::Unauthorized`] if `admin` is not an admin.
    /// - [`EscrowError::InvalidAddress`] for the zero address or this contract.
    /// - [`EscrowError::DefiPoolAlreadyDetached`] if `pool` is already detached.
    /// - [`EscrowError::AlreadyVoted`] if `admin` already approved.
    pub fn emergency_detach_defi_pool(
        env: Env,
        admin: Address,
        pool: Address,
    ) -> Result<bool, EscrowError> {
        detach(&env, admin, pool)
    }

    /// Whether `pool` has been detached.
    pub fn is_defi_pool_detached(env: Env, pool: Address) -> bool {
        blend_yield::is_pool_detached(&env, &pool)
    }

    /// Admins who have approved detaching `pool` so far.
    pub fn get_defi_pool_detach_approvals(env: Env, pool: Address) -> Vec<Address> {
        detach_approvals(&env, &pool)
    }

    /// Principal written off `pool` and settled from primary reserves.
    pub fn get_stranded_principal(env: Env, pool: Address) -> i128 {
        blend_yield::stranded_principal(&env, &pool)
    }
}

impl EmergencyUnwindTrait for EscrowContract {
    fn emergency_detach_defi_pool(
        env: Env,
        admin: Address,
        pool: Address,
    ) -> Result<bool, EscrowError> {
        detach(&env, admin, pool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::defi_fixture::{self, DefiFixture, MockBlendPoolClient};
    use soroban_sdk::testutils::{Address as _, Events};
    use soroban_sdk::token::StellarAssetClient;

    fn detach_fully(f: &DefiFixture) {
        assert!(!f.client.emergency_detach_defi_pool(&f.admin, &f.pool));
        assert!(f.client.emergency_detach_defi_pool(&f.co_admin, &f.pool));
    }

    fn emitted(f: &DefiFixture, action: Symbol, escrow_id: u64) -> bool {
        f.env.events().all().iter().any(|(_, topics, _)| {
            topics == (symbol_short!("escrow"), action.clone(), escrow_id).into_val(&f.env)
        })
    }

    #[test]
    fn detach_requires_multisig_threshold() {
        let f = defi_fixture::setup();
        assert!(!f.client.emergency_detach_defi_pool(&f.admin, &f.pool));
        assert!(!f.client.is_defi_pool_detached(&f.pool));
        assert_eq!(f.client.get_defi_pool_detach_approvals(&f.pool).len(), 1);

        assert!(f.client.emergency_detach_defi_pool(&f.co_admin, &f.pool));
        assert!(f.client.is_defi_pool_detached(&f.pool));
        assert_eq!(f.client.get_defi_pool_detach_approvals(&f.pool).len(), 0);
    }

    #[test]
    fn unauthorized_caller_is_rejected() {
        let f = defi_fixture::setup();
        let outsider = Address::generate(&f.env);
        assert_eq!(
            f.client.try_emergency_detach_defi_pool(&outsider, &f.pool),
            Err(Ok(EscrowError::Unauthorized))
        );
        assert!(!f.client.is_defi_pool_detached(&f.pool));
    }

    #[test]
    fn duplicate_approval_is_rejected() {
        let f = defi_fixture::setup();
        f.client.emergency_detach_defi_pool(&f.admin, &f.pool);
        assert_eq!(
            f.client.try_emergency_detach_defi_pool(&f.admin, &f.pool),
            Err(Ok(EscrowError::AlreadyVoted))
        );
        assert!(!f.client.is_defi_pool_detached(&f.pool));
    }

    #[test]
    fn detaching_twice_is_rejected() {
        let f = defi_fixture::setup();
        detach_fully(&f);
        assert_eq!(
            f.client.try_emergency_detach_defi_pool(&f.admin, &f.pool),
            Err(Ok(EscrowError::DefiPoolAlreadyDetached))
        );
    }

    #[test]
    fn invalid_pool_address_is_rejected() {
        let f = defi_fixture::setup();
        assert_eq!(
            f.client
                .try_emergency_detach_defi_pool(&f.admin, &f.contract),
            Err(Ok(EscrowError::InvalidAddress))
        );
    }

    #[test]
    fn detached_pool_rejects_new_opt_ins() {
        let f = defi_fixture::setup();
        detach_fully(&f);
        let id = defi_fixture::create(&f);
        assert_eq!(
            f.client
                .try_configure_blend_yield(&id, &f.admin, &f.blend_config()),
            Err(Ok(EscrowError::DefiPoolDetached))
        );
    }

    #[test]
    fn deposit_after_detach_stays_in_vault() {
        let f = defi_fixture::setup();
        let id = defi_fixture::create_with_blend(&f);
        detach_fully(&f);
        f.client.fund(&id, &f.buyer);

        assert!(emitted(&f, symbol_short!("bl_skip"), id));
        assert_eq!(f.pool_shares(), 0);
        assert_eq!(f.balance(&f.contract), 1_000);

        f.client.refund(&id, &f.seller);
        assert_eq!(f.balance(&f.buyer), 10_000);
    }

    #[test]
    fn staked_escrow_settles_from_primary_reserves_after_detach() {
        let f = defi_fixture::setup();
        let id = defi_fixture::create_with_blend(&f);
        f.client.fund(&id, &f.buyer);
        assert_eq!(f.pool_shares(), 1_000);
        assert_eq!(f.balance(&f.contract), 0);

        // The protocol is exploited: every pool call now traps.
        MockBlendPoolClient::new(&f.env, &f.pool).mock_set_paused(&true);
        // Without a detach, settlement is blocked by the failing pool.
        assert!(f.client.try_refund(&id, &f.seller).is_err());

        detach_fully(&f);
        // Primary escrow reserves backing the settlement.
        StellarAssetClient::new(&f.env, &f.token).mint(&f.contract, &1_000);
        let pool_balance = f.balance(&f.pool);

        f.client.refund(&id, &f.seller);
        assert!(emitted(&f, symbol_short!("bl_strand"), id));
        assert_eq!(f.balance(&f.buyer), 10_000);
        assert_eq!(f.balance(&f.contract), 0);
        // The pool was never touched and its position is tracked as stranded.
        assert_eq!(f.balance(&f.pool), pool_balance);
        assert_eq!(f.client.get_stranded_principal(&f.pool), 1_000);
    }

    #[test]
    fn escrows_without_pool_are_unaffected() {
        let f = defi_fixture::setup();
        detach_fully(&f);
        let id = defi_fixture::create(&f);
        f.client.fund(&id, &f.buyer);
        assert_eq!(f.balance(&f.contract), 1_000);
        f.client.refund(&id, &f.seller);
        assert_eq!(f.balance(&f.buyer), 10_000);
        assert_eq!(f.client.get_stranded_principal(&f.pool), 0);
    }
}
