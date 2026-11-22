//! Minimum yield threshold guard for escrow staking (issue #596).
//!
//! Supplying an escrow deposit to an external lending pool costs fees and
//! exposes principal to protocol risk. When the pool's supply rate drops below
//! a configured floor (default [`DEFAULT_MIN_YIELD_BPS`], i.e. 0.5% APY) the
//! expected return no longer justifies that cost, so the deposit is kept in
//! the escrow's standard vault instead and a [`YieldBypassedLowApy`] event is
//! emitted. Settlement is unaffected: an escrow that never opened a pool
//! position settles directly from custody.
//!
//! Supply rates are reported per pool by an admin with
//! [`EscrowContract::set_pool_supply_rate`]. A pool with no reported rate
//! keeps the pre-existing behaviour and is staked as configured.

use super::*;

/// Default minimum acceptable supply rate: 50 bps (0.5% APY).
pub const DEFAULT_MIN_YIELD_BPS: u32 = 50;

/// Upper bound for configured rates and thresholds (100% APY).
pub const MAX_YIELD_RATE_BPS: u32 = 10_000;

/// Whether a pool paying `pool_supply_rate_bps` clears the
/// `min_acceptable_bps` floor.
pub fn is_yield_viable(pool_supply_rate_bps: u32, min_acceptable_bps: u32) -> bool {
    pool_supply_rate_bps >= min_acceptable_bps
}

/// Emitted under `(escrow, yld_bypas, escrow_id)` when a deposit is kept in
/// the standard vault because the pool's supply rate is below the floor.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldBypassedLowApy {
    /// Escrow whose deposit was retained.
    pub escrow_id: u64,
    /// Lending pool that was skipped.
    pub pool: Address,
    /// Supply rate the pool was reporting, in basis points.
    pub pool_supply_rate_bps: u32,
    /// Configured minimum acceptable rate, in basis points.
    pub min_acceptable_bps: u32,
    /// Amount kept in the escrow vault.
    pub amount_retained: i128,
}

#[contracttype]
#[derive(Clone)]
enum YieldGuardKey {
    MinYieldBps,
    PoolSupplyRate(Address),
}

/// Configured minimum acceptable supply rate in basis points.
pub fn min_acceptable_bps(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&YieldGuardKey::MinYieldBps)
        .unwrap_or(DEFAULT_MIN_YIELD_BPS)
}

/// Last supply rate reported for `pool`, in basis points.
pub fn pool_supply_rate_bps(env: &Env, pool: &Address) -> Option<u32> {
    let key = YieldGuardKey::PoolSupplyRate(pool.clone());
    let rate: Option<u32> = env.storage().persistent().get(&key);
    if rate.is_some() {
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
    }
    rate
}

/// Decide whether `amount` for `escrow_id` may be supplied to `pool`.
///
/// Returns `false` and emits [`YieldBypassedLowApy`] when the pool's reported
/// rate is below the floor; the caller must then leave the funds in custody.
pub fn should_stake(env: &Env, escrow_id: u64, pool: &Address, amount: i128) -> bool {
    let Some(rate) = pool_supply_rate_bps(env, pool) else {
        return true;
    };
    let min = min_acceptable_bps(env);
    if is_yield_viable(rate, min) {
        return true;
    }
    env.events().publish(
        (symbol_short!("escrow"), symbol_short!("yld_bypas"), escrow_id),
        YieldBypassedLowApy {
            escrow_id,
            pool: pool.clone(),
            pool_supply_rate_bps: rate,
            min_acceptable_bps: min,
            amount_retained: amount,
        },
    );
    false
}

#[contractimpl]
impl EscrowContract {
    /// Set the minimum supply rate a pool must offer before deposits are
    /// staked. Admin or co-admin only.
    ///
    /// # Errors
    /// - [`EscrowError::Unauthorized`] if `admin` is not an admin.
    /// - [`EscrowError::InvalidYieldConfig`] if `min_bps` exceeds
    ///   [`MAX_YIELD_RATE_BPS`].
    pub fn set_min_yield_bps(env: Env, admin: Address, min_bps: u32) -> Result<bool, EscrowError> {
        Self::require_admin(&env, &admin)?;
        if min_bps > MAX_YIELD_RATE_BPS {
            return Err(EscrowError::InvalidYieldConfig);
        }
        env.storage()
            .instance()
            .set(&YieldGuardKey::MinYieldBps, &min_bps);
        Ok(true)
    }

    /// Configured minimum acceptable supply rate, in basis points.
    pub fn get_min_yield_bps(env: Env) -> u32 {
        min_acceptable_bps(&env)
    }

    /// Report the current supply rate of `pool`. Admin or co-admin only.
    ///
    /// # Errors
    /// - [`EscrowError::Unauthorized`] if `admin` is not an admin.
    /// - [`EscrowError::InvalidYieldConfig`] if `rate_bps` exceeds
    ///   [`MAX_YIELD_RATE_BPS`].
    pub fn set_pool_supply_rate(
        env: Env,
        admin: Address,
        pool: Address,
        rate_bps: u32,
    ) -> Result<bool, EscrowError> {
        Self::require_admin(&env, &admin)?;
        if rate_bps > MAX_YIELD_RATE_BPS {
            return Err(EscrowError::InvalidYieldConfig);
        }
        let key = YieldGuardKey::PoolSupplyRate(pool);
        env.storage().persistent().set(&key, &rate_bps);
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        Ok(true)
    }

    /// Last supply rate reported for `pool`, in basis points.
    pub fn get_pool_supply_rate(env: Env, pool: Address) -> Option<u32> {
        pool_supply_rate_bps(&env, &pool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::defi_fixture::{self, DefiFixture};
    use soroban_sdk::testutils::{Address as _, Events};

    fn bypass_emitted(f: &DefiFixture, escrow_id: u64) -> bool {
        f.env.events().all().iter().any(|(_, topics, _)| {
            topics
                == (symbol_short!("escrow"), symbol_short!("yld_bypas"), escrow_id)
                    .into_val(&f.env)
        })
    }

    #[test]
    fn viability_is_inclusive_of_threshold() {
        assert!(!is_yield_viable(49, 50));
        assert!(is_yield_viable(50, 50));
        assert!(is_yield_viable(51, 50));
        assert!(is_yield_viable(0, 0));
        assert!(!is_yield_viable(0, 1));
    }

    #[test]
    fn default_threshold_is_half_a_percent() {
        let f = defi_fixture::setup();
        assert_eq!(f.client.get_min_yield_bps(), DEFAULT_MIN_YIELD_BPS);
        assert_eq!(f.client.get_pool_supply_rate(&f.pool), None);
    }

    #[test]
    fn low_apy_bypasses_pool_and_keeps_funds_in_vault() {
        let f = defi_fixture::setup();
        f.client.set_pool_supply_rate(&f.admin, &f.pool, &20);
        let id = defi_fixture::create_with_blend(&f);
        f.client.fund(&id, &f.buyer);

        assert!(bypass_emitted(&f, id));
        assert_eq!(f.balance(&f.contract), 1_000);
        assert_eq!(f.pool_shares(), 0);

        // Settlement is served straight from the vault.
        f.client.refund(&id, &f.seller);
        assert_eq!(f.balance(&f.buyer), 10_000);
        assert_eq!(f.balance(&f.contract), 0);
    }

    #[test]
    fn apy_at_threshold_is_staked() {
        let f = defi_fixture::setup();
        f.client.set_pool_supply_rate(&f.admin, &f.pool, &50);
        let id = defi_fixture::create_with_blend(&f);
        f.client.fund(&id, &f.buyer);

        assert!(!bypass_emitted(&f, id));
        assert_eq!(f.balance(&f.contract), 0);
        assert_eq!(f.pool_shares(), 1_000);
    }

    #[test]
    fn apy_above_threshold_is_staked() {
        let f = defi_fixture::setup();
        f.client.set_pool_supply_rate(&f.admin, &f.pool, &425);
        let id = defi_fixture::create_with_blend(&f);
        f.client.fund(&id, &f.buyer);
        assert_eq!(f.pool_shares(), 1_000);
    }

    #[test]
    fn custom_threshold_is_respected() {
        let f = defi_fixture::setup();
        f.client.set_min_yield_bps(&f.admin, &300);
        f.client.set_pool_supply_rate(&f.admin, &f.pool, &250);
        let id = defi_fixture::create_with_blend(&f);
        f.client.fund(&id, &f.buyer);
        assert!(bypass_emitted(&f, id));
        assert_eq!(f.pool_shares(), 0);
        assert_eq!(f.balance(&f.contract), 1_000);
    }

    #[test]
    fn unreported_rate_keeps_existing_staking_behaviour() {
        let f = defi_fixture::setup();
        let id = defi_fixture::create_with_blend(&f);
        f.client.fund(&id, &f.buyer);
        assert_eq!(f.pool_shares(), 1_000);
    }

    #[test]
    fn only_admin_can_configure_rates() {
        let f = defi_fixture::setup();
        let outsider = Address::generate(&f.env);
        assert_eq!(
            f.client.try_set_min_yield_bps(&outsider, &100),
            Err(Ok(EscrowError::Unauthorized))
        );
        assert_eq!(
            f.client.try_set_pool_supply_rate(&outsider, &f.pool, &100),
            Err(Ok(EscrowError::Unauthorized))
        );
    }

    #[test]
    fn out_of_range_rates_are_rejected() {
        let f = defi_fixture::setup();
        assert_eq!(
            f.client
                .try_set_min_yield_bps(&f.admin, &(MAX_YIELD_RATE_BPS + 1)),
            Err(Ok(EscrowError::InvalidYieldConfig))
        );
        assert_eq!(
            f.client
                .try_set_pool_supply_rate(&f.admin, &f.pool, &(MAX_YIELD_RATE_BPS + 1)),
            Err(Ok(EscrowError::InvalidYieldConfig))
        );
    }
}
