//! Regression tests for LST funding of a `Created` escrow (issue #764).

use delego_escrow::{
    lst_support::LST_RATE_SCALE, DataKey, EscrowConfig, EscrowContract, EscrowContractClient,
    EscrowError, EscrowStatus, ESCROW_FLAG_FUNDED,
};
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env,
};

#[contracttype]
#[derive(Clone)]
enum LstKey {
    Balance(Address),
    Rate,
}

/// Minimal SEP-41 + `exchange_rate` mock so the escrow can lock LST shares.
#[contract]
struct MockLst;

#[contractimpl]
impl MockLst {
    pub fn set_rate(env: Env, rate: i128) {
        env.storage().instance().set(&LstKey::Rate, &rate);
    }

    pub fn exchange_rate(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&LstKey::Rate)
            .unwrap_or(LST_RATE_SCALE)
    }

    pub fn mint(env: Env, to: Address, amount: i128) {
        let key = LstKey::Balance(to);
        let balance: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(balance + amount));
    }

    pub fn balance(env: Env, who: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&LstKey::Balance(who))
            .unwrap_or(0)
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        let from_key = LstKey::Balance(from);
        let to_key = LstKey::Balance(to);
        let from_balance: i128 = env.storage().persistent().get(&from_key).unwrap_or(0);
        let to_balance: i128 = env.storage().persistent().get(&to_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&from_key, &(from_balance - amount));
        env.storage()
            .persistent()
            .set(&to_key, &(to_balance + amount));
    }
}

struct Fixture<'a> {
    env: Env,
    client: EscrowContractClient<'a>,
    buyer: Address,
    seller: Address,
    token: Address,
    lst: Address,
    contract: Address,
}

fn setup<'a>() -> Fixture<'a> {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let config = EscrowConfig {
        admin: admin.clone(),
        fee_bps: 0,
        treasury,
        min_amount: 1,
        max_amount: 1_000_000,
        min_fee_floor_stroops: 0,
        max_fee_ceiling_stroops: 0,
    };
    let contract = env.register(EscrowContract, (config,));
    let client = EscrowContractClient::new(&env, &contract);

    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let lst = env.register(MockLst, ());

    // Seed the token allowlist directly so the test can stay at one ledger.
    env.as_contract(&contract, || {
        env.storage()
            .instance()
            .set(&DataKey::AllowedToken(token.clone()), &0u32);
    });
    client.set_lst_whitelisted(&admin, &lst, &true);
    MockLstClient::new(&env, &lst).mint(&buyer, &1_000);

    // The test isolates the escrow state machine: `fund_with_lst` locks LST
    // shares, so the payout asset already in escrow custody is seeded here to
    // let `release` complete.
    StellarAssetClient::new(&env, &token).mint(&contract, &1_000);

    Fixture {
        env,
        client,
        buyer,
        seller,
        token,
        lst,
        contract,
    }
}

#[test]
fn funding_created_escrow_with_lst_marks_it_funded_and_settles() {
    let f = setup();
    let id = f.client.create(
        &f.buyer,
        &f.seller,
        &f.token,
        &1_000,
        &BytesN::from_array(&f.env, &[1; 32]),
        &10_000,
        &None::<BytesN<32>>,
        &None::<soroban_sdk::Symbol>,
    );

    let created = f.client.get_escrow(&id);
    assert_eq!(created.status, EscrowStatus::Created);
    assert!(!created.flags.has_flag(ESCROW_FLAG_FUNDED));

    f.env.ledger().with_mut(|ledger| ledger.timestamp += 5);
    let collateral = f.client.fund_with_lst(&id, &f.buyer, &f.lst, &1_000);
    assert_eq!(collateral.initial_share_balance, 1_000);
    assert_eq!(collateral.exchange_rate_snapshot, LST_RATE_SCALE);

    let funded = f.client.get_escrow(&id);
    assert_eq!(funded.status, EscrowStatus::Funded);
    assert!(funded.flags.has_flag(ESCROW_FLAG_FUNDED));
    assert_eq!(funded.updated_at, f.env.ledger().timestamp());
    let recorded = f.client.get_lst_collateral(&id).unwrap();
    assert_eq!(recorded.order_id, id);
    assert_eq!(recorded.lst_token_address, f.lst);
    assert_eq!(
        recorded.initial_share_balance,
        collateral.initial_share_balance
    );
    assert_eq!(
        recorded.exchange_rate_snapshot,
        collateral.exchange_rate_snapshot
    );

    // LST collateral can only be locked once, and the escrow can no longer be
    // funded through the standard path.
    assert!(matches!(
        f.client.try_fund_with_lst(&id, &f.buyer, &f.lst, &1_000),
        Err(Ok(EscrowError::LstCollateralAlreadyRecorded))
    ));
    assert_eq!(
        f.client.try_fund(&id, &f.buyer),
        Err(Ok(EscrowError::AlreadyFunded))
    );

    assert!(f.client.release(&id, &f.buyer, &f.seller));
    assert_eq!(f.client.get_escrow(&id).status, EscrowStatus::Released);

    let settlement = f.client.settle_lst_collateral(&id);
    assert_eq!(settlement.merchant_shares, 1_000);
    assert_eq!(settlement.buyer_shares, 0);
    assert!(f.client.get_lst_collateral(&id).is_none());

    let lst_client = MockLstClient::new(&f.env, &f.lst);
    assert_eq!(lst_client.balance(&f.seller), 1_000);
    assert_eq!(lst_client.balance(&f.buyer), 0);
    assert_eq!(lst_client.balance(&f.contract), 0);
    assert_eq!(
        soroban_sdk::token::Client::new(&f.env, &f.token).balance(&f.seller),
        1_000
    );
}
