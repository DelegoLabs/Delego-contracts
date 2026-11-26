// Copyright 2024 Delego Labs
// SPDX-License-Identifier: Apache-2.0
//
// Integration coverage for pro-rata yield checkpoints across escrows funded
// into the same Blend-compatible pool (issue #770).

use delego_escrow::{
    checkpoint::YIELD_PRECISION, AdminAction, CheckpointError, EscrowConfig, EscrowContract,
    EscrowContractClient, YieldBearingEscrowConfig,
};
use delego_interfaces::blend::{Positions, Request, RATE_SCALE, SUPPLY, WITHDRAW};
use soroban_sdk::{
    contract, contractimpl, contracttype, map, symbol_short,
    testutils::{Address as _, Ledger as _},
    token,
    token::StellarAssetClient,
    Address, BytesN, Env, IntoVal, Map, Symbol, Val, Vec,
};

#[contracttype]
#[derive(Clone)]
enum MockBlendKey {
    Positions(Address),
}

#[contract]
struct MockBlendPool;

#[contractimpl]
impl MockBlendPool {
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
        let mut pos = Self::get_positions(env.clone(), from.clone());
        for req in requests.iter() {
            let current = pos.supply.get(0).unwrap_or(0);
            let token_client = token::Client::new(&env, &req.address);
            match req.request_type {
                SUPPLY => {
                    token_client.transfer(&spender, &env.current_contract_address(), &req.amount);
                    pos.supply.set(0, current + req.amount);
                }
                WITHDRAW => {
                    assert!(req.amount <= current);
                    pos.supply.set(0, current - req.amount);
                    token_client.transfer(&env.current_contract_address(), &to, &req.amount);
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

struct Fixture<'a> {
    env: Env,
    client: EscrowContractClient<'a>,
    contract: Address,
    admin: Address,
    buyer: Address,
    seller: Address,
    token: Address,
    pool: Address,
    next_order: u8,
}

impl<'a> Fixture<'a> {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let contract = env.register(
            EscrowContract,
            (EscrowConfig {
                admin: admin.clone(),
                fee_bps: 0,
                treasury,
                min_amount: 1,
                max_amount: 1_000_000,
                min_fee_floor_stroops: 0,
                max_fee_ceiling_stroops: 0,
            },),
        );
        let client = EscrowContractClient::new(&env, &contract);
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        let pool = env.register(MockBlendPool, ());
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        StellarAssetClient::new(&env, &token).mint(&buyer, &10_000);
        StellarAssetClient::new(&env, &token).mint(&pool, &100_000);

        client.set_security_guardian(&admin, &Address::generate(&env));
        let action = client.queue_admin_action(&admin, &AdminAction::AddToken(token.clone()));
        let proposal = client.get_admin_action(&action).unwrap();
        for address in [contract.clone(), pool.clone(), token.clone()] {
            env.as_contract(&address, || {
                env.storage().instance().extend_ttl(17_280, 518_400);
            });
        }
        env.ledger().with_mut(|ledger| {
            ledger.sequence_number = proposal.pending.unlock_ledger;
            ledger.timestamp = proposal.unlock_timestamp;
        });
        client.execute_admin_action(&admin, &action);

        Self {
            env,
            client,
            contract,
            admin,
            buyer,
            seller,
            token,
            pool,
            next_order: 1,
        }
    }

    fn advance(&self, ledgers: u32) {
        self.env.ledger().with_mut(|ledger| {
            ledger.sequence_number += ledgers;
            ledger.timestamp += u64::from(ledgers) * 5;
        });
        for address in [self.contract.clone(), self.pool.clone(), self.token.clone()] {
            self.env.as_contract(&address, || {
                self.env.storage().instance().extend_ttl(17_280, 518_400);
            });
        }
    }

    fn fund_blend_escrow(&mut self) -> u64 {
        let seed = self.next_order;
        self.next_order += 1;
        let escrow_id = self.client.create(
            &self.buyer,
            &self.seller,
            &self.token,
            &1_000,
            &BytesN::from_array(&self.env, &[seed; 32]),
            &50_000u32,
            &None::<BytesN<32>>,
            &None::<Symbol>,
        );
        self.client.configure_blend_yield(
            &escrow_id,
            &self.admin,
            &YieldBearingEscrowConfig {
                lending_pool_contract: self.pool.clone(),
                b_token_address: self.pool.clone(),
                accrued_yield_stroops: 0,
                yield_recipient: self.buyer.clone(),
            },
        );
        self.client.fund(&escrow_id, &self.buyer);
        escrow_id
    }

    fn release(&self, escrow_id: u64) {
        self.client.release(&escrow_id, &self.buyer, &self.seller);
    }

    fn position(&self, escrow_id: u64) -> delego_escrow::YieldPosition {
        self.client
            .get_escrow_yield_position(&self.pool, &escrow_id)
    }
}

#[test]
fn funding_registers_yield_positions_and_empty_queries_are_stable() {
    let mut f = Fixture::new();
    let first = f.fund_blend_escrow();
    let second = f.fund_blend_escrow();

    let state = f.client.get_yield_pool_state(&f.pool);
    assert_eq!(state.total_shares, 2_000);
    assert_eq!(state.total_accrued, 0);
    assert_eq!(state.checkpoint.cumulative_yield_per_share, 0);
    assert_eq!(f.position(first).shares, 1_000);
    assert_eq!(f.position(second).shares, 1_000);

    let unknown = f.client.get_escrow_yield_position(&f.pool, &999);
    assert_eq!(unknown.shares, 0);
    assert_eq!(unknown.pending_yield, 0);
}

#[test]
fn yield_is_split_pro_rata_between_two_escrows_in_one_pool() {
    let mut f = Fixture::new();
    let early = f.fund_blend_escrow();

    f.advance(100);
    assert_eq!(
        f.client
            .sync_yield_checkpoint(&f.pool, &1_000)
            .cumulative_yield_per_share,
        YIELD_PRECISION
    );

    let late = f.fund_blend_escrow();
    f.advance(100);
    f.client.sync_yield_checkpoint(&f.pool, &1_000);

    assert_eq!(f.position(early).pending_yield, 1_500);
    assert_eq!(f.position(late).pending_yield, 500);

    f.release(early);
    f.release(late);

    assert_eq!(f.position(early).pending_yield, 1_500);
    assert_eq!(f.position(late).pending_yield, 500);
    let state = f.client.get_yield_pool_state(&f.pool);
    assert_eq!(state.total_shares, 0);
    assert_eq!(state.remainder().unwrap(), 0);
}

#[test]
fn checkpoint_getter_reflects_accrual_before_settlement() {
    let mut f = Fixture::new();
    let escrow_id = f.fund_blend_escrow();
    let before = f.client.get_escrow_yield_position(&f.pool, &escrow_id);
    assert_eq!(before.pending_yield, 0);

    f.advance(10);
    f.client.sync_yield_checkpoint(&f.pool, &777);

    let after = f.client.get_escrow_yield_position(&f.pool, &escrow_id);
    assert_eq!(after.pending_yield, 777);
    assert_eq!(
        after.checkpoint,
        f.client.get_yield_pool_state(&f.pool).checkpoint
    );
}

#[test]
fn negative_accrual_is_rejected() {
    let f = Fixture::new();
    assert_eq!(
        f.client.try_sync_yield_checkpoint(&f.pool, &-1),
        Err(Ok(CheckpointError::InvalidAmount))
    );
}
