//! Multi-hop AMM liquidity-pool routing for cross-border payments (issue #595).
//!
//! A buyer in Europe paying in EURC to a merchant in Japan who settles in
//! JPYC rarely has a direct EURC/JPYC pool with usable depth. The router walks
//! an ordered list of [`AmmRouteHop`]s (for example `EURC -> USDC -> JPYC`)
//! across admin-registered AMM pools, feeding each hop's realised output into
//! the next hop.
//!
//! # Price-tolerance checks
//!
//! Every hop carries its own `min_output_amount` floor, and the full route is
//! additionally bound by a cumulative slippage cap: the final output must be at
//! least `expected_output - expected_output * max_slippage_bps / 10_000`
//! (rounded in the payer's favour). `max_slippage_bps` itself may never exceed
//! [`MAX_ROUTE_SLIPPAGE_BPS`].
//!
//! Outputs are measured from the escrow contract's own token balance deltas,
//! never from a pool's reported return value, so a pool that lies about what
//! it delivered cannot satisfy a floor.
//!
//! # Atomicity
//!
//! Soroban discards every storage write and token transfer of an invocation
//! that returns an error or traps. A hop that fails, under-delivers, or breaks
//! a slippage bound makes [`EscrowContract::route_amm_payment`] return an
//! error, so the payer's input transfer and every earlier hop are rolled back
//! together and no partially-swapped funds are ever left behind.

use super::*;
use soroban_sdk::auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation};
use soroban_sdk::{contractclient, token, vec};

/// Maximum number of hops accepted in a single route.
pub const MAX_AMM_ROUTE_HOPS: u32 = 4;

/// Upper bound on the cumulative slippage a caller may tolerate (10%).
pub const MAX_ROUTE_SLIPPAGE_BPS: u32 = 1_000;

const AMM_BPS_DENOMINATOR: i128 = 10_000;

/// One swap in a multi-hop AMM route.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AmmRouteHop {
    /// Registered AMM pool that executes this hop.
    pub pool_address: Address,
    /// Token sold into the pool. Must equal the previous hop's output token.
    pub input_token: Address,
    /// Token bought from the pool.
    pub output_token: Address,
    /// Minimum realised output for this hop; the route reverts below it.
    pub min_output_amount: i128,
}

/// Token pair an admin registered for an AMM pool.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AmmPoolPair {
    /// First token of the pool's pair.
    pub token_a: Address,
    /// Second token of the pool's pair.
    pub token_b: Address,
}

/// Emitted under `(escrow, amm_route)` after a route settles successfully.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AmmRouteExecuted {
    /// Account that funded the route.
    pub payer: Address,
    /// Account that received the final output.
    pub recipient: Address,
    /// Token sold on the first hop.
    pub input_token: Address,
    /// Token bought on the last hop.
    pub output_token: Address,
    /// Amount of `input_token` sold.
    pub amount_in: i128,
    /// Amount of `output_token` delivered to `recipient`.
    pub amount_out: i128,
    /// Number of hops executed.
    pub hops: u32,
}

#[contracttype]
#[derive(Clone)]
enum AmmKey {
    Pool(Address),
}

/// Swap interface a registered AMM pool must expose.
#[contractclient(name = "AmmPoolClient")]
pub trait AmmPoolInterface {
    /// Sell exactly `amount_in` of `token_in` for `token_out`.
    ///
    /// The pool must pull the input with
    /// `token_in.transfer(sender, pool, amount_in)` (the router authorizes
    /// exactly that call and nothing else), deliver at least `min_amount_out`
    /// of `token_out` to `sender`, and return the delivered amount.
    fn swap_exact_in(
        env: Env,
        sender: Address,
        token_in: Address,
        token_out: Address,
        amount_in: i128,
        min_amount_out: i128,
    ) -> i128;
}

/// Minimum final output allowed by the cumulative slippage cap.
///
/// The tolerance is rounded down, so the floor is rounded up: the payer is
/// never exposed to more than `max_slippage_bps` of slippage.
pub fn route_output_floor(
    expected_output: i128,
    max_slippage_bps: u32,
) -> Result<i128, EscrowError> {
    if expected_output <= 0 {
        return Err(EscrowError::InvalidAmount);
    }
    if max_slippage_bps > MAX_ROUTE_SLIPPAGE_BPS {
        return Err(EscrowError::InvalidSwapConfig);
    }
    let tolerance = expected_output
        .checked_mul(i128::from(max_slippage_bps))
        .ok_or(EscrowError::MathOverflow)?
        / AMM_BPS_DENOMINATOR;
    expected_output
        .checked_sub(tolerance)
        .ok_or(EscrowError::MathOverflow)
}

/// Registered pair for `pool`, if any.
pub fn registered_pool(env: &Env, pool: &Address) -> Option<AmmPoolPair> {
    let key = AmmKey::Pool(pool.clone());
    let pair: Option<AmmPoolPair> = env.storage().persistent().get(&key);
    if pair.is_some() {
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
    }
    pair
}

/// Check route shape, pool registration, and hop-to-hop token continuity.
///
/// Returns the route's `(input_token, output_token)`.
pub fn validate_route(
    env: &Env,
    route: &Vec<AmmRouteHop>,
) -> Result<(Address, Address), EscrowError> {
    if route.is_empty() || route.len() > MAX_AMM_ROUTE_HOPS {
        return Err(EscrowError::InvalidSwapConfig);
    }
    let me = env.current_contract_address();
    let mut previous_output: Option<Address> = None;
    for hop in route.iter() {
        if hop.min_output_amount <= 0 {
            return Err(EscrowError::InvalidAmount);
        }
        if hop.input_token == hop.output_token || hop.pool_address == me {
            return Err(EscrowError::InvalidSwapConfig);
        }
        if let Some(expected_input) = previous_output {
            if expected_input != hop.input_token {
                return Err(EscrowError::InvalidSwapConfig);
            }
        }
        let pair = registered_pool(env, &hop.pool_address).ok_or(EscrowError::PoolNotFound)?;
        let forward = pair.token_a == hop.input_token && pair.token_b == hop.output_token;
        let reverse = pair.token_b == hop.input_token && pair.token_a == hop.output_token;
        if !forward && !reverse {
            return Err(EscrowError::InvalidSwapConfig);
        }
        previous_output = Some(hop.output_token.clone());
    }
    let first = route.get(0).ok_or(EscrowError::InvalidSwapConfig)?;
    let last = route
        .get(route.len() - 1)
        .ok_or(EscrowError::InvalidSwapConfig)?;
    Ok((first.input_token, last.output_token))
}

/// Execute `route` with `amount_in` of the first input token already held by
/// this contract, and return the realised output held by this contract.
///
/// Any error leaves the caller responsible for propagating it so the host
/// rolls back the whole invocation.
pub fn execute_route(
    env: &Env,
    route: &Vec<AmmRouteHop>,
    amount_in: i128,
    expected_output: i128,
    max_slippage_bps: u32,
) -> Result<i128, EscrowError> {
    if amount_in <= 0 {
        return Err(EscrowError::InvalidAmount);
    }
    let floor = route_output_floor(expected_output, max_slippage_bps)?;
    validate_route(env, route)?;

    let me = env.current_contract_address();
    let mut amount = amount_in;
    for hop in route.iter() {
        let token_in = token::Client::new(env, &hop.input_token);
        let token_out = token::Client::new(env, &hop.output_token);
        let in_before = token_in.balance(&me);
        let out_before = token_out.balance(&me);
        if in_before < amount {
            return Err(EscrowError::InsufficientEscrowBalance);
        }

        // Authorize only the pool's exact pull of this hop's input; the pool
        // never receives an open-ended allowance.
        env.authorize_as_current_contract(vec![
            env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: hop.input_token.clone(),
                    fn_name: symbol_short!("transfer"),
                    args: (me.clone(), hop.pool_address.clone(), amount).into_val(env),
                },
                sub_invocations: vec![env],
            }),
        ]);
        let pool = AmmPoolClient::new(env, &hop.pool_address);
        match pool.try_swap_exact_in(
            &me,
            &hop.input_token,
            &hop.output_token,
            &amount,
            &hop.min_output_amount,
        ) {
            Ok(Ok(_)) => {}
            _ => return Err(EscrowError::SwapRouterCallFailed),
        }

        let spent = in_before
            .checked_sub(token_in.balance(&me))
            .ok_or(EscrowError::MathOverflow)?;
        let received = token_out
            .balance(&me)
            .checked_sub(out_before)
            .ok_or(EscrowError::MathOverflow)?;
        if spent != amount {
            return Err(EscrowError::SwapRouterCallFailed);
        }
        if received < hop.min_output_amount {
            return Err(EscrowError::SlippageExceeded);
        }
        // Clear any residual allowance the pool may hold (issue #599).
        token_safety::revoke_allowance_safe(env, &hop.input_token, &hop.pool_address);
        amount = received;
    }

    if amount < floor {
        return Err(EscrowError::SlippageExceeded);
    }
    Ok(amount)
}

#[contractimpl]
impl EscrowContract {
    /// Register an AMM pool for `token_a`/`token_b` so routes may use it.
    /// Admin or co-admin only.
    pub fn register_amm_pool(
        env: Env,
        admin: Address,
        pool: Address,
        token_a: Address,
        token_b: Address,
    ) -> Result<bool, EscrowError> {
        Self::require_admin(&env, &admin)?;
        if is_zero_address(&env, &pool)
            || is_zero_address(&env, &token_a)
            || is_zero_address(&env, &token_b)
        {
            return Err(EscrowError::InvalidAddress);
        }
        if token_a == token_b || pool == env.current_contract_address() {
            return Err(EscrowError::InvalidSwapConfig);
        }
        let key = AmmKey::Pool(pool.clone());
        env.storage()
            .persistent()
            .set(&key, &AmmPoolPair { token_a, token_b });
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        env.events()
            .publish((symbol_short!("escrow"), symbol_short!("amm_reg")), pool);
        Ok(true)
    }

    /// Remove an AMM pool from the routing registry. Admin or co-admin only.
    pub fn deregister_amm_pool(
        env: Env,
        admin: Address,
        pool: Address,
    ) -> Result<bool, EscrowError> {
        Self::require_admin(&env, &admin)?;
        let key = AmmKey::Pool(pool.clone());
        if !env.storage().persistent().has(&key) {
            return Err(EscrowError::PoolNotFound);
        }
        env.storage().persistent().remove(&key);
        env.events()
            .publish((symbol_short!("escrow"), symbol_short!("amm_dereg")), pool);
        Ok(true)
    }

    /// Token pair registered for `pool`, if any.
    pub fn get_amm_pool(env: Env, pool: Address) -> Option<AmmPoolPair> {
        registered_pool(&env, &pool)
    }

    /// Route a cross-currency payment from `payer` to `recipient`.
    ///
    /// Pulls `amount_in` of the first hop's input token from `payer`, swaps it
    /// hop by hop, and delivers the final output to `recipient`. Returns the
    /// delivered amount.
    ///
    /// # Errors
    /// - [`EscrowError::InvalidAmount`] — non-positive amount, quote, or hop floor.
    /// - [`EscrowError::InvalidSwapConfig`] — empty, too long, or discontinuous
    ///   route, or a slippage cap above [`MAX_ROUTE_SLIPPAGE_BPS`].
    /// - [`EscrowError::PoolNotFound`] — a hop uses an unregistered pool.
    /// - [`EscrowError::SwapRouterCallFailed`] — a pool call failed or did not
    ///   consume exactly the hop input.
    /// - [`EscrowError::SlippageExceeded`] — a hop or the full route fell
    ///   below its floor.
    ///
    /// Every error reverts the entire route, including the payer's transfer.
    pub fn route_amm_payment(
        env: Env,
        payer: Address,
        recipient: Address,
        amount_in: i128,
        route: Vec<AmmRouteHop>,
        expected_output: i128,
        max_slippage_bps: u32,
    ) -> Result<i128, EscrowError> {
        payer.require_auth();
        if is_zero_address(&env, &recipient) {
            return Err(EscrowError::InvalidAddress);
        }
        if amount_in <= 0 {
            return Err(EscrowError::InvalidAmount);
        }
        let (input_token, output_token) = validate_route(&env, &route)?;

        let me = env.current_contract_address();
        let input_client = token::Client::new(&env, &input_token);
        let balance_before = input_client.balance(&me);
        input_client.transfer(&payer, &me, &amount_in);
        let received =
            verify_received_deposit_delta(&env, &input_client, &me, balance_before, amount_in)?;

        let amount_out = execute_route(&env, &route, received, expected_output, max_slippage_bps)?;
        token::Client::new(&env, &output_token).transfer(&me, &recipient, &amount_out);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("amm_route")),
            AmmRouteExecuted {
                payer,
                recipient,
                input_token,
                output_token,
                amount_in: received,
                amount_out,
                hops: route.len(),
            },
        );
        Ok(amount_out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Events};
    use soroban_sdk::token::StellarAssetClient;

    #[contracttype]
    #[derive(Clone)]
    enum MockAmmKey {
        /// Output per 10_000 input units.
        RateBps,
        /// Units withheld from the output actually delivered.
        ShortBy,
        Fail,
    }

    /// Constant-rate AMM pool with knobs for failure and under-delivery.
    #[contract]
    pub struct MockAmmPool;

    #[contractimpl]
    impl MockAmmPool {
        pub fn mock_set_rate(env: Env, rate_bps: i128) {
            env.storage().instance().set(&MockAmmKey::RateBps, &rate_bps);
        }

        pub fn mock_set_short(env: Env, short_by: i128) {
            env.storage().instance().set(&MockAmmKey::ShortBy, &short_by);
        }

        pub fn mock_set_fail(env: Env, fail: bool) {
            env.storage().instance().set(&MockAmmKey::Fail, &fail);
        }

        pub fn swap_exact_in(
            env: Env,
            sender: Address,
            token_in: Address,
            token_out: Address,
            amount_in: i128,
            _min_amount_out: i128,
        ) -> i128 {
            let fail: bool = env
                .storage()
                .instance()
                .get(&MockAmmKey::Fail)
                .unwrap_or(false);
            assert!(!fail, "pool paused");
            let rate: i128 = env
                .storage()
                .instance()
                .get(&MockAmmKey::RateBps)
                .unwrap_or(10_000);
            let short: i128 = env
                .storage()
                .instance()
                .get(&MockAmmKey::ShortBy)
                .unwrap_or(0);
            let pool = env.current_contract_address();
            token::Client::new(&env, &token_in).transfer(&sender, &pool, &amount_in);
            let quoted = amount_in * rate / 10_000;
            // A hostile pool may deliver less than it reports, ignoring the floor.
            token::Client::new(&env, &token_out).transfer(&pool, &sender, &(quoted - short));
            quoted
        }
    }

    struct Fixture<'a> {
        env: Env,
        client: EscrowContractClient<'a>,
        contract: Address,
        admin: Address,
        payer: Address,
        merchant: Address,
        eurc: Address,
        usdc: Address,
        jpyc: Address,
        eurc_usdc: Address,
        usdc_jpyc: Address,
    }

    fn token(env: &Env) -> Address {
        env.register_stellar_asset_contract_v2(Address::generate(env))
            .address()
    }

    fn pool(env: &Env, rate_bps: i128) -> Address {
        let id = env.register(MockAmmPool, ());
        MockAmmPoolClient::new(env, &id).mock_set_rate(&rate_bps);
        id
    }

    fn setup<'a>() -> Fixture<'a> {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0,
            treasury: Address::generate(&env),
            min_amount: 1,
            max_amount: 1_000_000_000,
            min_fee_floor_stroops: 0,
            max_fee_ceiling_stroops: 0,
        };
        let contract = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(&env, &contract);

        let (eurc, usdc, jpyc) = (token(&env), token(&env), token(&env));
        // 1 EURC = 1.08 USDC; 1 USDC = 150 JPYC.
        let eurc_usdc = pool(&env, 10_800);
        let usdc_jpyc = pool(&env, 1_500_000);
        StellarAssetClient::new(&env, &usdc).mint(&eurc_usdc, &1_000_000_000);
        StellarAssetClient::new(&env, &jpyc).mint(&usdc_jpyc, &1_000_000_000_000);
        client.register_amm_pool(&admin, &eurc_usdc, &eurc, &usdc);
        client.register_amm_pool(&admin, &usdc_jpyc, &usdc, &jpyc);

        let payer = Address::generate(&env);
        StellarAssetClient::new(&env, &eurc).mint(&payer, &10_000);
        let merchant = Address::generate(&env);
        Fixture {
            env,
            client,
            contract,
            admin,
            payer,
            merchant,
            eurc,
            usdc,
            jpyc,
            eurc_usdc,
            usdc_jpyc,
        }
    }

    fn hop(pool: &Address, from: &Address, to: &Address, min: i128) -> AmmRouteHop {
        AmmRouteHop {
            pool_address: pool.clone(),
            input_token: from.clone(),
            output_token: to.clone(),
            min_output_amount: min,
        }
    }

    fn eurc_to_jpyc(f: &Fixture) -> Vec<AmmRouteHop> {
        vec![
            &f.env,
            hop(&f.eurc_usdc, &f.eurc, &f.usdc, 1_000),
            hop(&f.usdc_jpyc, &f.usdc, &f.jpyc, 150_000),
        ]
    }

    fn balance(env: &Env, token: &Address, who: &Address) -> i128 {
        token::Client::new(env, token).balance(who)
    }

    #[test]
    fn routes_eurc_through_usdc_to_jpyc() {
        let f = setup();
        // 1_000 EURC -> 1_080 USDC -> 162_000 JPYC.
        let out = f.client.route_amm_payment(
            &f.payer,
            &f.merchant,
            &1_000,
            &eurc_to_jpyc(&f),
            &162_000,
            &50,
        );
        assert_eq!(out, 162_000);
        assert_eq!(balance(&f.env, &f.jpyc, &f.merchant), 162_000);
        assert_eq!(balance(&f.env, &f.eurc, &f.payer), 9_000);
        // Nothing is left behind in the escrow contract.
        assert_eq!(balance(&f.env, &f.eurc, &f.contract), 0);
        assert_eq!(balance(&f.env, &f.usdc, &f.contract), 0);
        assert_eq!(balance(&f.env, &f.jpyc, &f.contract), 0);
        assert!(!f.env.events().all().is_empty());
    }

    #[test]
    fn single_hop_route_settles() {
        let f = setup();
        let route = vec![&f.env, hop(&f.eurc_usdc, &f.eurc, &f.usdc, 1_000)];
        let out = f
            .client
            .route_amm_payment(&f.payer, &f.merchant, &1_000, &route, &1_080, &0);
        assert_eq!(out, 1_080);
        assert_eq!(balance(&f.env, &f.usdc, &f.merchant), 1_080);
    }

    #[test]
    fn cumulative_slippage_cap_reverts_whole_route() {
        let f = setup();
        // Second pool now pays 140 JPYC per USDC: 1_080 * 140 = 151_200,
        // above the per-hop floor but below the 1% cap on a 162_000 quote
        // (floor 160_380).
        MockAmmPoolClient::new(&f.env, &f.usdc_jpyc).mock_set_rate(&1_400_000);
        let result = f.client.try_route_amm_payment(
            &f.payer,
            &f.merchant,
            &1_000,
            &eurc_to_jpyc(&f),
            &162_000,
            &100,
        );
        assert_eq!(result, Err(Ok(EscrowError::SlippageExceeded)));
        assert_eq!(balance(&f.env, &f.eurc, &f.payer), 10_000);
        assert_eq!(balance(&f.env, &f.jpyc, &f.merchant), 0);
    }

    #[test]
    fn slippage_within_cap_settles() {
        let f = setup();
        MockAmmPoolClient::new(&f.env, &f.usdc_jpyc).mock_set_rate(&1_490_000);
        // 1_080 * 149 = 160_920 >= floor 160_380.
        let out = f.client.route_amm_payment(
            &f.payer,
            &f.merchant,
            &1_000,
            &eurc_to_jpyc(&f),
            &162_000,
            &100,
        );
        assert_eq!(out, 160_920);
    }

    #[test]
    fn slippage_cap_above_maximum_is_rejected() {
        let f = setup();
        let result = f.client.try_route_amm_payment(
            &f.payer,
            &f.merchant,
            &1_000,
            &eurc_to_jpyc(&f),
            &162_000,
            &(MAX_ROUTE_SLIPPAGE_BPS + 1),
        );
        assert_eq!(result, Err(Ok(EscrowError::InvalidSwapConfig)));
    }

    #[test]
    fn failing_second_hop_rolls_back_first_hop() {
        let f = setup();
        MockAmmPoolClient::new(&f.env, &f.usdc_jpyc).mock_set_fail(&true);
        let pool_usdc_before = balance(&f.env, &f.usdc, &f.eurc_usdc);
        let result = f.client.try_route_amm_payment(
            &f.payer,
            &f.merchant,
            &1_000,
            &eurc_to_jpyc(&f),
            &162_000,
            &50,
        );
        assert_eq!(result, Err(Ok(EscrowError::SwapRouterCallFailed)));
        // The EURC -> USDC hop that had already succeeded is undone too.
        assert_eq!(balance(&f.env, &f.eurc, &f.payer), 10_000);
        assert_eq!(balance(&f.env, &f.eurc, &f.eurc_usdc), 0);
        assert_eq!(balance(&f.env, &f.usdc, &f.eurc_usdc), pool_usdc_before);
        assert_eq!(balance(&f.env, &f.usdc, &f.contract), 0);
    }

    #[test]
    fn under_delivering_hop_reverts_on_measured_output() {
        let f = setup();
        // Pool reports 1_080 USDC but delivers 1_080 - 100 = 980, below the
        // hop's 1_000 floor.
        MockAmmPoolClient::new(&f.env, &f.eurc_usdc).mock_set_short(&100);
        let result = f.client.try_route_amm_payment(
            &f.payer,
            &f.merchant,
            &1_000,
            &eurc_to_jpyc(&f),
            &162_000,
            &50,
        );
        assert_eq!(result, Err(Ok(EscrowError::SlippageExceeded)));
        assert_eq!(balance(&f.env, &f.eurc, &f.payer), 10_000);
    }

    #[test]
    fn per_hop_minimum_is_enforced() {
        let f = setup();
        let route = vec![
            &f.env,
            hop(&f.eurc_usdc, &f.eurc, &f.usdc, 1_081),
            hop(&f.usdc_jpyc, &f.usdc, &f.jpyc, 1),
        ];
        let result = f
            .client
            .try_route_amm_payment(&f.payer, &f.merchant, &1_000, &route, &162_000, &1_000);
        assert_eq!(result, Err(Ok(EscrowError::SlippageExceeded)));
    }

    #[test]
    fn unregistered_pool_is_rejected() {
        let f = setup();
        let rogue = pool(&f.env, 10_000);
        let route = vec![&f.env, hop(&rogue, &f.eurc, &f.usdc, 1)];
        let result = f
            .client
            .try_route_amm_payment(&f.payer, &f.merchant, &1_000, &route, &1_000, &0);
        assert_eq!(result, Err(Ok(EscrowError::PoolNotFound)));
    }

    #[test]
    fn discontinuous_route_is_rejected() {
        let f = setup();
        // Second hop sells EURC although the first hop produced USDC.
        let route = vec![
            &f.env,
            hop(&f.eurc_usdc, &f.eurc, &f.usdc, 1),
            hop(&f.eurc_usdc, &f.eurc, &f.usdc, 1),
        ];
        let result = f
            .client
            .try_route_amm_payment(&f.payer, &f.merchant, &1_000, &route, &1_000, &0);
        assert_eq!(result, Err(Ok(EscrowError::InvalidSwapConfig)));
    }

    #[test]
    fn hop_pair_must_match_registered_pool() {
        let f = setup();
        let route = vec![&f.env, hop(&f.eurc_usdc, &f.eurc, &f.jpyc, 1)];
        let result = f
            .client
            .try_route_amm_payment(&f.payer, &f.merchant, &1_000, &route, &1_000, &0);
        assert_eq!(result, Err(Ok(EscrowError::InvalidSwapConfig)));
    }

    #[test]
    fn empty_and_overlong_routes_are_rejected() {
        let f = setup();
        let empty: Vec<AmmRouteHop> = Vec::new(&f.env);
        assert_eq!(
            f.client
                .try_route_amm_payment(&f.payer, &f.merchant, &1_000, &empty, &1_000, &0),
            Err(Ok(EscrowError::InvalidSwapConfig))
        );
        let mut long = Vec::new(&f.env);
        for _ in 0..=MAX_AMM_ROUTE_HOPS {
            long.push_back(hop(&f.eurc_usdc, &f.eurc, &f.usdc, 1));
        }
        assert_eq!(
            f.client
                .try_route_amm_payment(&f.payer, &f.merchant, &1_000, &long, &1_000, &0),
            Err(Ok(EscrowError::InvalidSwapConfig))
        );
    }

    #[test]
    fn only_admin_can_register_pools() {
        let f = setup();
        let outsider = Address::generate(&f.env);
        let result =
            f.client
                .try_register_amm_pool(&outsider, &f.eurc_usdc, &f.eurc, &f.usdc);
        assert_eq!(result, Err(Ok(EscrowError::Unauthorized)));
    }

    #[test]
    fn deregistered_pool_can_no_longer_route() {
        let f = setup();
        f.client.deregister_amm_pool(&f.admin, &f.usdc_jpyc);
        assert_eq!(f.client.get_amm_pool(&f.usdc_jpyc), None);
        let result = f.client.try_route_amm_payment(
            &f.payer,
            &f.merchant,
            &1_000,
            &eurc_to_jpyc(&f),
            &162_000,
            &50,
        );
        assert_eq!(result, Err(Ok(EscrowError::PoolNotFound)));
    }

    #[test]
    fn output_floor_rounds_in_payers_favour() {
        assert_eq!(route_output_floor(10_000, 0), Ok(10_000));
        assert_eq!(route_output_floor(10_000, 100), Ok(9_900));
        // 999 * 1% = 9.99 tolerance, rounded down to 9 => floor 990.
        assert_eq!(route_output_floor(999, 100), Ok(990));
        assert_eq!(route_output_floor(0, 100), Err(EscrowError::InvalidAmount));
        assert_eq!(
            route_output_floor(i128::MAX, 1_000),
            Err(EscrowError::MathOverflow)
        );
    }
}
