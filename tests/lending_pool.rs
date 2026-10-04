#![cfg(test)]

use delego_escrow::{EscrowConfig, EscrowContract, EscrowContractClient};
use delego_interfaces::{LendingPoolClient, MockLendingPool, MockLendingPoolClient};
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{token, Address, BytesN, Env};

/// Cross-contract integration: the escrow contract drives an external lending
/// pool exclusively through the shared `LendingPoolInterface` client (issue
/// #326), falling back to its internal APR estimate when the pool is
/// unreachable, paused, or reports a non-positive figure.
#[test]
fn escrow_delegates_yield_to_lending_pool() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    let treasury = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token = env.register_stellar_asset_contract(token_admin);
    let token_client = token::StellarAssetClient::new(&env, &token);
    token_client.mint(&buyer, &10_000i128);

    let escrow_config = EscrowConfig {
        admin: admin.clone(),
        fee_bps: 0u32,
        treasury,
        min_amount: 100i128,
        max_amount: 10_000i128,
    };
    let escrow_id_addr = env.register(EscrowContract, (escrow_config,));
    let escrow = EscrowContractClient::new(&env, &escrow_id_addr);
    escrow.add_token(&admin, &token);

    let order_id = BytesN::from_array(&env, &[96u8; 32]);
    let escrow_id = escrow.deposit(
        &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
    );

    // Configure the mock pool as this escrow's lending contract.
    let pool_id_addr = env.register(MockLendingPool, ());
    let pool = MockLendingPoolClient::new(&env, &pool_id_addr);
    let pool_client = LendingPoolClient::new(&env, &pool_id_addr);
    escrow.set_yield_config(&admin, &escrow_id, &pool_id_addr, &500u32); // 5% internal

    // Seed the escrow's notional pool position at a deliberately different
    // accrual rate (100% APR); the pool's own accounting must win.
    pool.set_position(&escrow_id_addr, &token, &1_000i128);
    pool.set_yield_rate(&10_000u32);
    env.ledger().with_mut(|li| {
        li.sequence_number = 1000;
        li.timestamp = 31_536_000; // ~1 year
    });

    // The shared client is consumable from any caller; here it confirms the
    // escrow's position yields 1000 after one year at 100% APR.
    assert_eq!(pool_client.get_accrued_yield(&escrow_id_addr), 1_000i128);

    let view = escrow.get_accrued_yield(&escrow_id);
    assert_eq!(view.accrued, 1_000i128, "pool-reported yield must win");
    assert_eq!(
        view.apy_bps, 500,
        "apy_bps reflects the escrow config, not the pool"
    );
    assert_eq!(view.held_seconds, 31_536_000);
}

/// When the pool pauses, the escrow read falls back to the internal estimate
/// rather than reverting the whole call.
#[test]
fn escrow_falls_back_when_pool_paused() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    let treasury = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token = env.register_stellar_asset_contract(token_admin);
    let token_client = token::StellarAssetClient::new(&env, &token);
    token_client.mint(&buyer, &10_000i128);

    let escrow_config = EscrowConfig {
        admin: admin.clone(),
        fee_bps: 0u32,
        treasury,
        min_amount: 100i128,
        max_amount: 10_000i128,
    };
    let escrow_id_addr = env.register(EscrowContract, (escrow_config,));
    let escrow = EscrowContractClient::new(&env, &escrow_id_addr);
    escrow.add_token(&admin, &token);

    let order_id = BytesN::from_array(&env, &[97u8; 32]);
    let escrow_id = escrow.deposit(
        &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
    );

    let pool_id_addr = env.register(MockLendingPool, ());
    let pool = MockLendingPoolClient::new(&env, &pool_id_addr);
    escrow.set_yield_config(&admin, &escrow_id, &pool_id_addr, &500u32);

    pool.set_position(&escrow_id_addr, &token, &1_000i128);
    pool.set_yield_rate(&10_000u32);
    env.ledger().with_mut(|li| {
        li.sequence_number = 1000;
        li.timestamp = 31_536_000;
    });

    assert_eq!(escrow.get_accrued_yield(&escrow_id).accrued, 1_000i128);

    pool.set_paused(&true);
    // Internal estimate for 1000 principal at 5% APR over one year = 50.
    assert_eq!(
        escrow.get_accrued_yield(&escrow_id).accrued,
        50i128,
        "paused pool must not break yield reads"
    );
}

/// A pool with no position reports zero; the escrow treats zero as
/// non-authoritative and falls back to its internal estimate.
#[test]
fn escrow_falls_back_when_pool_reports_zero() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    let treasury = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token = env.register_stellar_asset_contract(token_admin);
    let token_client = token::StellarAssetClient::new(&env, &token);
    token_client.mint(&buyer, &10_000i128);

    let escrow_config = EscrowConfig {
        admin: admin.clone(),
        fee_bps: 0u32,
        treasury,
        min_amount: 100i128,
        max_amount: 10_000i128,
    };
    let escrow_id_addr = env.register(EscrowContract, (escrow_config,));
    let escrow = EscrowContractClient::new(&env, &escrow_id_addr);
    escrow.add_token(&admin, &token);

    let order_id = BytesN::from_array(&env, &[98u8; 32]);
    let escrow_id = escrow.deposit(
        &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
    );

    let pool_id_addr = env.register(MockLendingPool, ());
    escrow.set_yield_config(&admin, &escrow_id, &pool_id_addr, &500u32);

    env.ledger().with_mut(|li| {
        li.sequence_number = 1000;
        li.timestamp = 31_536_000;
    });

    assert_eq!(
        escrow.get_accrued_yield(&escrow_id).accrued,
        50i128,
        "zero pool report must fall back internally"
    );
}
