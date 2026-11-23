//! Shared fixture for external DeFi pool tests (issues #596 and #597): a real
//! escrow contract wired to a minimal Blend-compatible lending pool.

use core::cell::Cell;

use crate::admin_actions_test;
use crate::{
    AdminAction, EscrowConfig, EscrowContract, EscrowContractClient, YieldBearingEscrowConfig,
};
use delego_interfaces::blend::{Positions, Request, RATE_SCALE, SUPPLY, WITHDRAW};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::token::{self, StellarAssetClient};
use soroban_sdk::{
    contract, contractimpl, contracttype, map, symbol_short, Address, BytesN, Env, IntoVal, Map,
    Symbol, Val, Vec,
};

#[contracttype]
#[derive(Clone)]
enum MockBlendKey {
    Positions(Address),
    Paused,
}

/// Blend-compatible pool at a fixed 1:1 bRate. `mock_set_paused` makes every
/// `submit` trap, the way an exploited or paused protocol would.
#[contract]
pub struct MockBlendPool;

#[contractimpl]
impl MockBlendPool {
    pub fn mock_set_paused(env: Env, paused: bool) {
        env.storage().instance().set(&MockBlendKey::Paused, &paused);
    }

    pub fn get_reserve(env: Env, asset: Address) -> Map<Symbol, Val> {
        let cfg: Map<Symbol, Val> = map![&env, (symbol_short!("index"), 0u32.into_val(&env))];
        let data: Map<Symbol, Val> =
            map![&env, (symbol_short!("b_rate"), RATE_SCALE.into_val(&env))];
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
            .get(&MockBlendKey::Positions(address))
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
        let paused: bool = env
            .storage()
            .instance()
            .get(&MockBlendKey::Paused)
            .unwrap_or(false);
        assert!(!paused, "pool paused");
        let mut pos = Self::get_positions(env.clone(), from.clone());
        for req in requests {
            let current = pos.supply.get(0).unwrap_or(0);
            let token = token::Client::new(&env, &req.address);
            match req.request_type {
                SUPPLY => {
                    token.transfer(&spender, &env.current_contract_address(), &req.amount);
                    pos.supply.set(0, current + req.amount);
                }
                WITHDRAW => {
                    assert!(req.amount <= current);
                    pos.supply.set(0, current - req.amount);
                    token.transfer(&env.current_contract_address(), &to, &req.amount);
                }
                _ => panic!("unsupported request"),
            }
        }
        env.storage()
            .persistent()
            .set(&MockBlendKey::Positions(from), &pos);
        pos
    }
}

/// Escrow, Blend pool, and participants.
pub struct DefiFixture<'a> {
    pub env: Env,
    pub client: EscrowContractClient<'a>,
    pub contract: Address,
    pub admin: Address,
    pub co_admin: Address,
    pub buyer: Address,
    pub seller: Address,
    pub token: Address,
    pub pool: Address,
    next_order: Cell<u8>,
}

impl DefiFixture<'_> {
    /// Escrow-token balance of `who`.
    pub fn balance(&self, who: &Address) -> i128 {
        token::Client::new(&self.env, &self.token).balance(who)
    }

    /// Supply shares the escrow holds in the pool.
    pub fn pool_shares(&self) -> i128 {
        MockBlendPoolClient::new(&self.env, &self.pool)
            .get_positions(&self.contract)
            .supply
            .get(0)
            .unwrap_or(0)
    }

    /// Blend configuration pointing at the fixture pool.
    pub fn blend_config(&self) -> YieldBearingEscrowConfig {
        YieldBearingEscrowConfig {
            lending_pool_contract: self.pool.clone(),
            b_token_address: self.pool.clone(),
            accrued_yield_stroops: 0,
            yield_recipient: self.buyer.clone(),
        }
    }
}

/// Build the fixture: buyer holds 10_000, the pool holds 100_000 of liquidity,
/// and the escrow has two admins (primary plus one co-admin).
pub fn setup<'a>() -> DefiFixture<'a> {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let config = EscrowConfig {
        admin: admin.clone(),
        fee_bps: 0,
        treasury: Address::generate(&env),
        min_amount: 1,
        max_amount: 1_000_000,
        min_fee_floor_stroops: 0,
        max_fee_ceiling_stroops: 0,
    };
    let contract = env.register(EscrowContract, (config,));
    let client = EscrowContractClient::new(&env, &contract);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let pool = env.register(MockBlendPool, ());
    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    StellarAssetClient::new(&env, &token).mint(&buyer, &10_000);
    StellarAssetClient::new(&env, &token).mint(&pool, &100_000);
    admin_actions_test::approve(&env, &client, &admin, AdminAction::AddToken(token.clone()));
    let co_admin = Address::generate(&env);
    client.add_co_admin(&admin, &co_admin);
    DefiFixture {
        env,
        client,
        contract,
        admin,
        co_admin,
        buyer,
        seller,
        token,
        pool,
        next_order: Cell::new(1),
    }
}

/// Create an unfunded 1_000-unit escrow opted into the fixture pool.
pub fn create_with_blend(f: &DefiFixture) -> u64 {
    let id = create(f);
    f.client
        .configure_blend_yield(&id, &f.admin, &f.blend_config());
    id
}

/// Create an unfunded 1_000-unit escrow with no pool configuration.
pub fn create(f: &DefiFixture) -> u64 {
    let seed = f.next_order.get();
    f.next_order.set(seed.wrapping_add(1));
    f.client.create(
        &f.buyer,
        &f.seller,
        &f.token,
        &1_000,
        &BytesN::from_array(&f.env, &[seed; 32]),
        &10_000u32,
        &None::<BytesN<32>>,
        &None::<Symbol>,
    )
}
