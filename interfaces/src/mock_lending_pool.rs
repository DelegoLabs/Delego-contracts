//! In-repo mock lending pool implementing [`crate::LendingPoolInterface`].
//!
//! Simulates a lending pool locally so escrow yield accrual can be exercised
//! in unit and integration tests without deploying against a real external
//! protocol.  Positions accrue yield linearly from `opened_at` at a mock-wide
//! rate in basis points:
//!
//! `yield = principal * rate_bps * elapsed / (10_000 * SECONDS_PER_YEAR)`
//!
//! The pool treats `env.current_contract_address()` as the depositor, so the
//! escrow contract can be attributed a position via [`MockLendingPool`]'s
//! `set_position` seeding hook even though the escrow only reads yield.

use crate::LendingPoolInterface;
use soroban_sdk::{contract, contracterror, contractimpl, contracttype, Address, Env, Error, Vec};

/// Seconds in a 365-day year, mirroring the escrow contract's yield formula.
const SECONDS_PER_YEAR: i128 = 31_536_000;

/// Error codes surfaced by the mock lending pool.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MockLendingPoolError {
    /// Amounts must be strictly positive.
    InvalidAmount = 1,
    /// Withdraw shares exceed the caller's principal.
    InsufficientShares = 2,
    /// Yield rate must fit in the 10000 bps (100%) bound.
    InvalidYieldRate = 3,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MockPosition {
    /// Principal (in token units) notionally deposited.
    pub principal: i128,
    /// Ledger UTC timestamp when the position started accruing yield.
    pub opened_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MockDataKey {
    Position(Address, Address),
    UserTokens(Address),
    YieldRateBps,
    Paused,
}

#[contract]
pub struct MockLendingPool;

#[contractimpl]
impl MockLendingPool {
    /// Set the annual yield rate in basis points (0..=10000).
    pub fn set_yield_rate(env: Env, rate_bps: u32) -> Result<u32, MockLendingPoolError> {
        if rate_bps > 10_000 {
            return Err(MockLendingPoolError::InvalidYieldRate);
        }
        env.storage()
            .instance()
            .set(&MockDataKey::YieldRateBps, &rate_bps);
        Ok(rate_bps)
    }

    /// Current annual yield rate in basis points.
    pub fn get_yield_rate(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&MockDataKey::YieldRateBps)
            .unwrap_or(0)
    }

    /// Pause the pool; while paused every `get_accrued_yield` call aborts so
    /// callers exercise their graceful-fallback path.
    pub fn set_paused(env: Env, paused: bool) -> bool {
        env.storage().instance().set(&MockDataKey::Paused, &paused);
        paused
    }

    /// Whether the pool is currently paused.
    pub fn get_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&MockDataKey::Paused)
            .unwrap_or(false)
    }

    /// Seeding hook: directly attribute `principal` of `token` to `user`
    /// (opening a fresh position at the current ledger time).  Used to
    /// simulate the escrow's notional position, which is otherwise read-only.
    pub fn set_position(env: Env, user: Address, token: Address, principal: i128) -> i128 {
        let now = env.ledger().timestamp();
        let pos = MockPosition {
            principal,
            opened_at: now,
        };
        env.storage()
            .persistent()
            .set(&MockDataKey::Position(user.clone(), token.clone()), &pos);
        Self::add_user_token(&env, &user, &token);
        principal
    }

    /// Read back a user's position for a token (None when unset).
    pub fn get_position(env: Env, user: Address, token: Address) -> Option<MockPosition> {
        env.storage()
            .persistent()
            .get(&MockDataKey::Position(user, token))
    }

    fn add_user_token(env: &Env, user: &Address, token: &Address) {
        let mut tokens: Vec<Address> = env
            .storage()
            .persistent()
            .get(&MockDataKey::UserTokens(user.clone()))
            .unwrap_or(Vec::new(env));
        let mut present = false;
        for i in 0..tokens.len() {
            if tokens.get(i).unwrap() == token.clone() {
                present = true;
                break;
            }
        }
        if !present {
            tokens.push_back(token.clone());
            env.storage()
                .persistent()
                .set(&MockDataKey::UserTokens(user.clone()), &tokens);
        }
    }
}

#[contractimpl]
impl LendingPoolInterface for MockLendingPool {
    fn deposit(env: Env, token: Address, amount: i128) -> Result<i128, Error> {
        if amount <= 0 {
            return Err(Error::from(MockLendingPoolError::InvalidAmount));
        }
        let user = env.current_contract_address();
        let now = env.ledger().timestamp();
        let mut pos: MockPosition = env
            .storage()
            .persistent()
            .get(&MockDataKey::Position(user.clone(), token.clone()))
            .unwrap_or(MockPosition {
                principal: 0,
                opened_at: now,
            });
        if pos.principal == 0 {
            pos.opened_at = now;
        }
        pos.principal += amount;
        env.storage()
            .persistent()
            .set(&MockDataKey::Position(user.clone(), token.clone()), &pos);
        MockLendingPool::add_user_token(&env, &user, &token);
        Ok(pos.principal)
    }

    fn withdraw(env: Env, token: Address, shares: i128) -> Result<i128, Error> {
        if shares <= 0 {
            return Err(Error::from(MockLendingPoolError::InvalidAmount));
        }
        let user = env.current_contract_address();
        let mut pos: MockPosition = env
            .storage()
            .persistent()
            .get(&MockDataKey::Position(user.clone(), token.clone()))
            .ok_or(Error::from(MockLendingPoolError::InsufficientShares))?;
        if pos.principal < shares {
            return Err(Error::from(MockLendingPoolError::InsufficientShares));
        }
        pos.principal -= shares;
        env.storage()
            .persistent()
            .set(&MockDataKey::Position(user, token), &pos);
        Ok(shares)
    }

    fn get_accrued_yield(env: Env, user: Address) -> i128 {
        let paused: bool = env
            .storage()
            .instance()
            .get(&MockDataKey::Paused)
            .unwrap_or(false);
        if paused {
            panic!("MockLendingPool is paused");
        }
        let rate_bps: u32 = env
            .storage()
            .instance()
            .get(&MockDataKey::YieldRateBps)
            .unwrap_or(0);
        let now = env.ledger().timestamp();
        let tokens: Vec<Address> = env
            .storage()
            .persistent()
            .get(&MockDataKey::UserTokens(user.clone()))
            .unwrap_or(Vec::new(&env));
        let mut total: i128 = 0;
        for i in 0..tokens.len() {
            let token = tokens.get(i).unwrap();
            if let Some(pos) = env
                .storage()
                .persistent()
                .get::<MockDataKey, MockPosition>(&MockDataKey::Position(user.clone(), token))
            {
                let elapsed = now.saturating_sub(pos.opened_at) as i128;
                total +=
                    pos.principal * rate_bps as i128 * elapsed / (10_000i128 * SECONDS_PER_YEAR);
            }
        }
        total
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::testutils::Ledger;
    use soroban_sdk::{Address, Env};

    fn setup() -> (Env, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let id = env.register(MockLendingPool, ());
        let token = Address::generate(&env);
        (env, id, token)
    }

    fn clients<'a>(
        env: &'a Env,
        id: &'a Address,
    ) -> (
        crate::lending_pool::LendingPoolClient<'a>,
        MockLendingPoolClient<'a>,
    ) {
        (
            crate::lending_pool::LendingPoolClient::new(env, id),
            MockLendingPoolClient::new(env, id),
        )
    }

    #[test]
    fn deposit_and_withdraw_round_trip() {
        let (env, id, token) = setup();
        let (client, mock) = clients(&env, &id);
        env.ledger().set_timestamp(1_000u64);

        assert_eq!(client.deposit(&token, &1_000i128), 1_000i128);
        assert_eq!(client.deposit(&token, &500i128), 1_500i128);

        let pos = mock.get_position(&id, &token).unwrap();
        assert_eq!(pos.principal, 1_500i128);
        assert_eq!(pos.opened_at, 1_000u64);

        assert_eq!(client.withdraw(&token, &600i128), 600i128);
        let pos = mock.get_position(&id, &token).unwrap();
        assert_eq!(pos.principal, 900i128);
    }

    #[test]
    fn zero_amounts_are_rejected() {
        let (env, id, token) = setup();
        let (client, _mock) = clients(&env, &id);
        let dep = client.try_deposit(&token, &0i128);
        assert_eq!(
            dep,
            Err(Ok(Error::from(MockLendingPoolError::InvalidAmount)))
        );
        let wd = client.try_withdraw(&token, &0i128);
        assert_eq!(
            wd,
            Err(Ok(Error::from(MockLendingPoolError::InvalidAmount)))
        );
    }

    #[test]
    fn overdraw_is_rejected() {
        let (env, id, token) = setup();
        let (client, _mock) = clients(&env, &id);
        client.deposit(&token, &100i128);
        let wd = client.try_withdraw(&token, &101i128);
        assert_eq!(
            wd,
            Err(Ok(Error::from(MockLendingPoolError::InsufficientShares)))
        );
    }

    #[test]
    fn yield_accrues_linearly_from_open_to_now() {
        let (env, id, token) = setup();
        let (client, mock) = clients(&env, &id);
        mock.set_yield_rate(&10_000u32);

        env.ledger().set_timestamp(0u64);
        mock.set_position(&id, &token, &1_000i128);

        env.ledger().set_timestamp(31_536_000u64); // one year at 100% APR
        assert_eq!(client.get_accrued_yield(&id), 1_000i128);

        env.ledger().set_timestamp(31_536_000u64 * 2); // two years
        assert_eq!(client.get_accrued_yield(&id), 2_000i128);
    }

    #[test]
    fn manual_deposit_also_accrues_yield() {
        let (env, id, token) = setup();
        let (client, mock) = clients(&env, &id);
        mock.set_yield_rate(&1_000u32); // 10% APR

        env.ledger().set_timestamp(0u64);
        client.deposit(&token, &10_000i128);
        env.ledger().set_timestamp(31_536_000u64); // one year at 10%
        assert_eq!(client.get_accrued_yield(&id), 1_000i128);
    }

    #[test]
    fn paused_pool_aborts_yield_read() {
        let (env, id, token) = setup();
        let (client, mock) = clients(&env, &id);
        mock.set_yield_rate(&10_000u32);
        mock.set_position(&id, &token, &1_000i128);
        env.ledger().set_timestamp(31_536_000u64);

        mock.set_paused(&true);
        assert!(client.try_get_accrued_yield(&id).is_err());
    }

    #[test]
    fn no_positions_yield_zero() {
        let (env, id, _token) = setup();
        let (client, _mock) = clients(&env, &id);
        assert_eq!(client.get_accrued_yield(&id), 0i128);
    }

    #[test]
    fn rate_out_of_bounds_rejected() {
        let (env, id, _token) = setup();
        let (_client, mock) = clients(&env, &id);
        assert_eq!(
            mock.try_set_yield_rate(&10_001u32),
            Err(Ok(MockLendingPoolError::InvalidYieldRate))
        );
    }
}
