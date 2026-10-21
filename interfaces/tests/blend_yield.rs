//! Exercise the exact escrow module without compiling the broken legacy escrow
//! crate. This is module-level coverage, not a substitute for lifecycle tests.
#[path = "../../escrow/src/blend_yield.rs"]
mod blend_yield;

use blend_yield::{BlendYieldError, YieldBearingEscrowConfig};
use delego_interfaces::blend::{underlying, Positions, Request, RATE_SCALE, SUPPLY, WITHDRAW};
use soroban_sdk::{
    contract, contractimpl, contracttype, map, symbol_short,
    testutils::{Address as _, Events},
    token, Address, Env, IntoVal, Map, Symbol, Val, Vec,
};

#[contract]
struct Harness;

#[contractimpl]
impl Harness {
    pub fn configure(env: Env, id: u64, owner: Address, cfg: YieldBearingEscrowConfig) {
        owner.require_auth();
        blend_yield::configure(&env, id, cfg);
    }
    pub fn fund(env: Env, id: u64, buyer: Address, asset: Address, amount: i128) {
        buyer.require_auth();
        token::Client::new(&env, &asset).transfer(&buyer, &env.current_contract_address(), &amount);
        blend_yield::fund(&env, id, &asset, amount);
    }
    pub fn settle(env: Env, id: u64, seller: Address, asset: Address, principal: i128) {
        blend_yield::redeem(&env, id);
        token::Client::new(&env, &asset).transfer(
            &env.current_contract_address(),
            &seller,
            &principal,
        );
    }
    pub fn redeem(env: Env, id: u64) {
        blend_yield::redeem(&env, id);
    }
    pub fn config(env: Env, id: u64) -> Option<YieldBearingEscrowConfig> {
        blend_yield::config(&env, id)
    }
}

#[contract]
struct Pool;

#[contracttype]
#[derive(Clone)]
enum PoolKey {
    Positions(Address),
    Rate,
    Paused,
    ShortPay,
    OverBurn,
}

#[contractimpl]
impl Pool {
    pub fn set_rate(env: Env, rate: i128) {
        env.storage().instance().set(&PoolKey::Rate, &rate);
    }
    pub fn set_paused(env: Env, paused: bool) {
        env.storage().instance().set(&PoolKey::Paused, &paused);
    }
    pub fn short_pay(env: Env, short: bool) {
        env.storage().instance().set(&PoolKey::ShortPay, &short);
    }
    pub fn over_burn(env: Env, enabled: bool) {
        env.storage().instance().set(&PoolKey::OverBurn, &enabled);
    }
    pub fn get_reserve(env: Env, asset: Address) -> Map<Symbol, Val> {
        let rate: i128 = env
            .storage()
            .instance()
            .get(&PoolKey::Rate)
            .unwrap_or(RATE_SCALE);
        let cfg: Map<Symbol, Val> = map![&env, (symbol_short!("index"), 0u32.into_val(&env))];
        let data: Map<Symbol, Val> = map![&env, (symbol_short!("b_rate"), rate.into_val(&env))];
        map![
            &env,
            (symbol_short!("asset"), asset.into_val(&env)),
            (symbol_short!("config"), cfg.into_val(&env)),
            (symbol_short!("data"), data.into_val(&env)),
        ]
    }
    pub fn get_positions(env: Env, address: Address) -> Positions {
        env.storage()
            .persistent()
            .get(&PoolKey::Positions(address))
            .unwrap_or(Positions {
                liabilities: Map::new(&env),
                collateral: Map::new(&env),
                supply: Map::new(&env),
            })
    }
    pub fn submit(
        env: Env,
        from: Address,
        spender: Address,
        to: Address,
        requests: Vec<Request>,
    ) -> Positions {
        spender.require_auth();
        if from != spender {
            from.require_auth();
        }
        assert!(!env
            .storage()
            .instance()
            .get(&PoolKey::Paused)
            .unwrap_or(false));
        let rate: i128 = env
            .storage()
            .instance()
            .get(&PoolKey::Rate)
            .unwrap_or(RATE_SCALE);
        let mut pos = Self::get_positions(env.clone(), from.clone());
        for req in requests {
            let current = pos.supply.get(0).unwrap_or(0);
            let token = token::Client::new(&env, &req.address);
            match req.request_type {
                SUPPLY => {
                    // Deliberately use a simple, independent reference formula.
                    let shares = req.amount * RATE_SCALE / rate;
                    assert!(shares > 0);
                    token.transfer(&spender, &env.current_contract_address(), &req.amount);
                    pos.supply.set(0, current + shares);
                }
                WITHDRAW => {
                    let mut shares = (req.amount * RATE_SCALE + rate - 1) / rate;
                    if env
                        .storage()
                        .instance()
                        .get(&PoolKey::OverBurn)
                        .unwrap_or(false)
                    {
                        shares += 1;
                    }
                    assert!(shares <= current);
                    pos.supply.set(0, current - shares);
                    let amount = if env
                        .storage()
                        .instance()
                        .get(&PoolKey::ShortPay)
                        .unwrap_or(false)
                    {
                        req.amount - 1
                    } else {
                        req.amount
                    };
                    token.transfer(&env.current_contract_address(), &to, &amount);
                }
                _ => panic!("only non-collateral supply and withdraw are allowed"),
            }
        }
        env.storage()
            .persistent()
            .set(&PoolKey::Positions(from), &pos);
        pos
    }
}

struct Fixture {
    env: Env,
    escrow: Address,
    pool: Address,
    token: Address,
    buyer: Address,
    seller: Address,
    treasury: Address,
}

impl Fixture {
    fn new() -> Self {
        let env = Env::default();
        let escrow = env.register(Harness, ());
        let pool = env.register(Pool, ());
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let treasury = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        env.mock_all_auths();
        token::StellarAssetClient::new(&env, &token).mint(&buyer, &10_000);
        token::StellarAssetClient::new(&env, &token).mint(&pool, &10_000);
        // Real token auth from the escrow to the nested pool call is exercised
        // even with account auth mocked; separate test below supplies exact auth.
        Self {
            env,
            escrow,
            pool,
            token,
            buyer,
            seller,
            treasury,
        }
    }
    fn client(&self) -> HarnessClient<'_> {
        HarnessClient::new(&self.env, &self.escrow)
    }
    fn pool(&self) -> PoolClient<'_> {
        PoolClient::new(&self.env, &self.pool)
    }
    fn cfg(&self) -> YieldBearingEscrowConfig {
        YieldBearingEscrowConfig {
            lending_pool_contract: self.pool.clone(),
            b_token_address: self.pool.clone(),
            accrued_yield_stroops: 0,
            yield_recipient: self.treasury.clone(),
        }
    }
    fn configure(&self, id: u64) {
        self.client().configure(&id, &self.buyer, &self.cfg());
    }
    fn fund(&self, id: u64, amount: i128) {
        self.client().fund(&id, &self.buyer, &self.token, &amount);
    }
    fn balance(&self, address: &Address) -> i128 {
        token::Client::new(&self.env, &self.token).balance(address)
    }
}

#[test]
fn supplies_receipts_then_pays_principal_and_real_interest() {
    let f = Fixture::new();
    f.configure(1);
    f.fund(1, 1_000);
    assert_eq!(f.balance(&f.escrow), 0);
    assert_eq!(f.pool().get_positions(&f.escrow).supply.get(0), Some(1_000));
    f.pool().set_rate(&(RATE_SCALE * 11 / 10));
    f.client().settle(&1, &f.seller, &f.token, &1_000);
    let settlement_events = f.env.events().all();
    assert_eq!(f.balance(&f.seller), 1_000);
    assert_eq!(f.balance(&f.treasury), 100);
    assert_eq!(f.balance(&f.escrow), 0);
    assert_eq!(f.client().config(&1).unwrap().accrued_yield_stroops, 100);
    assert!(settlement_events.iter().any(|(_, topics, _)| {
        topics == (symbol_short!("escrow"), symbol_short!("bl_redeem"), 1u64).into_val(&f.env)
    }));
}

#[test]
fn two_escrows_share_pool_without_redeeming_each_others_shares() {
    let f = Fixture::new();
    f.configure(1);
    f.fund(1, 1_000);
    f.pool().set_rate(&(RATE_SCALE * 2));
    f.configure(2);
    f.fund(2, 1_000); // 500 shares, not the first escrow's 1000
    f.client().settle(&2, &f.seller, &f.token, &1_000);
    assert_eq!(f.balance(&f.treasury), 0);
    assert_eq!(f.pool().get_positions(&f.escrow).supply.get(0), Some(1_000));
    f.client().settle(&1, &f.seller, &f.token, &1_000);
    assert_eq!(f.balance(&f.seller), 2_000);
    assert_eq!(f.balance(&f.treasury), 1_000);
}

#[test]
fn short_payment_rolls_back_even_when_other_escrow_cash_is_available() {
    let f = Fixture::new();
    f.configure(1);
    f.fund(1, 1_000);
    f.fund(2, 5_000); // ordinary escrow cash must not mask a deficit
    f.pool().short_pay(&true);
    assert_eq!(
        f.client().try_settle(&1, &f.seller, &f.token, &1_000),
        Err(Ok(soroban_sdk::Error::from_contract_error(
            BlendYieldError::PrincipalShortfall as u32
        )))
    );
    assert_eq!(f.balance(&f.escrow), 5_000);
    assert_eq!(f.balance(&f.seller), 0);
    assert_eq!(f.pool().get_positions(&f.escrow).supply.get(0), Some(1_000));
    assert_eq!(f.client().config(&1).unwrap().accrued_yield_stroops, 0);
}

#[test]
fn rate_loss_aborts_without_mutating_receipts() {
    let f = Fixture::new();
    f.configure(1);
    f.fund(1, 1_000);
    f.pool().set_rate(&(RATE_SCALE / 2));
    assert!(f.client().try_redeem(&1).is_err());
    assert_eq!(f.pool().get_positions(&f.escrow).supply.get(0), Some(1_000));
}

#[test]
fn paused_pool_reverts_funding_and_settlement_and_can_be_retried() {
    let f = Fixture::new();
    f.configure(1);
    f.pool().set_paused(&true);
    assert!(f.client().try_fund(&1, &f.buyer, &f.token, &1_000).is_err());
    assert_eq!(f.balance(&f.buyer), 10_000);
    f.pool().set_paused(&false);
    f.fund(1, 1_000);
    f.pool().set_paused(&true);
    assert!(f
        .client()
        .try_settle(&1, &f.seller, &f.token, &1_000)
        .is_err());
    f.pool().set_paused(&false);
    f.client().settle(&1, &f.seller, &f.token, &1_000);
    assert_eq!(f.balance(&f.seller), 1_000);
}

#[test]
fn zero_yield_and_repeat_redemption_do_not_duplicate_payments() {
    let f = Fixture::new();
    f.configure(1);
    f.fund(1, 1_000);
    f.client().redeem(&1);
    f.client().redeem(&1);
    assert_eq!(f.balance(&f.escrow), 1_000);
    assert_eq!(f.balance(&f.treasury), 0);
    assert!(f.client().try_fund(&1, &f.buyer, &f.token, &1_000).is_err());
    assert_eq!(f.balance(&f.buyer), 9_000);
}

#[test]
fn refund_or_partial_release_can_use_redeemed_liquid_principal() {
    let f = Fixture::new();
    let mut cfg = f.cfg();
    cfg.yield_recipient = f.buyer.clone();
    f.client().configure(&1, &f.buyer, &cfg);
    f.fund(1, 1_000);
    f.pool().set_rate(&(RATE_SCALE * 12 / 10));
    f.client().settle(&1, &f.seller, &f.token, &400);
    assert_eq!(f.balance(&f.escrow), 600);
    f.client().settle(&1, &f.buyer, &f.token, &600);
    assert_eq!(f.balance(&f.buyer), 9_800);
    assert_eq!(f.balance(&f.seller), 400);
}

#[test]
fn refuses_reconfiguration_and_fake_receipt_contract() {
    let f = Fixture::new();
    let mut cfg = f.cfg();
    cfg.b_token_address = f.token.clone();
    assert!(f.client().try_configure(&1, &f.buyer, &cfg).is_err());
    f.configure(1);
    assert!(f.client().try_configure(&1, &f.buyer, &f.cfg()).is_err());
}

#[test]
fn rounding_dust_cannot_be_charged_to_another_escrow() {
    let f = Fixture::new();
    f.pool().set_rate(&(RATE_SCALE * 3 / 2));
    f.configure(1);
    f.fund(1, 1_000); // 666 shares => 999 units until interest covers rounding
    assert!(f.client().try_redeem(&1).is_err());
    f.pool().set_rate(&(RATE_SCALE * 2));
    f.client().settle(&1, &f.seller, &f.token, &1_000);
    assert_eq!(f.balance(&f.treasury), 332);
}

#[test]
fn conversion_handles_large_amounts_rounding_and_invalid_inputs() {
    assert_eq!(underlying(i128::MAX, RATE_SCALE), Some(i128::MAX));
    assert_eq!(underlying(3, RATE_SCALE / 2), Some(1));
    assert_eq!(underlying(i128::MAX, RATE_SCALE + 1), None);
    assert_eq!(underlying(-1, RATE_SCALE), None);
    assert_eq!(underlying(1, 0), None);
    assert_eq!(underlying(0, RATE_SCALE), Some(0));
}

#[test]
fn nested_pool_transfer_uses_exact_contract_authorization() {
    use soroban_sdk::testutils::{MockAuth, MockAuthInvoke};
    let f = Fixture::new();
    f.configure(1);
    f.env.mock_auths(&[MockAuth {
        address: &f.buyer,
        invoke: &MockAuthInvoke {
            contract: &f.escrow,
            fn_name: "fund",
            args: (1u64, f.buyer.clone(), f.token.clone(), 1_000i128).into_val(&f.env),
            sub_invokes: &[MockAuthInvoke {
                contract: &f.token,
                fn_name: "transfer",
                args: (f.buyer.clone(), f.escrow.clone(), 1_000i128).into_val(&f.env),
                sub_invokes: &[],
            }],
        },
    }]);
    // No mock authorization for escrow -> pool -> token: production module
    // must supply its own bounded authorize_as_current_contract entry.
    f.fund(1, 1_000);
    assert_eq!(f.balance(&f.escrow), 0);
    assert_eq!(f.pool().get_positions(&f.escrow).supply.get(0), Some(1_000));
}

#[test]
fn unauthenticated_funding_cannot_move_buyers_tokens() {
    let f = Fixture::new();
    f.configure(1);
    f.env.mock_auths(&[]);
    assert!(f.client().try_fund(&1, &f.buyer, &f.token, &1_000).is_err());
    assert_eq!(f.balance(&f.buyer), 10_000);
    assert_eq!(f.balance(&f.escrow), 0);
}

#[test]
fn excess_receipt_burn_rolls_back_without_consuming_another_position() {
    let f = Fixture::new();
    f.configure(1);
    f.configure(2);
    f.fund(1, 1_000);
    f.fund(2, 1_000);
    f.pool().over_burn(&true);
    assert!(f.client().try_redeem(&1).is_err());
    assert_eq!(f.pool().get_positions(&f.escrow).supply.get(0), Some(2_000));
    assert_eq!(f.balance(&f.escrow), 0);
}

#[test]
fn no_configuration_keeps_funds_liquid() {
    let f = Fixture::new();
    f.fund(1, 1_000);
    assert!(f.client().config(&1).is_none());
    assert_eq!(f.balance(&f.escrow), 1_000);
    assert_eq!(f.pool().get_positions(&f.escrow).supply.len(), 0);
    f.client().settle(&1, &f.seller, &f.token, &1_000);
    assert_eq!(f.balance(&f.seller), 1_000);
}
