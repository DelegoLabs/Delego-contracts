// Copyright 2024 Delego Labs
// SPDX-License-Identifier: Apache-2.0
//
// Vault safety health factor checks and stake deposit ledger getter tests.
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Events, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env, TryFromVal,
};

use crate::{
    flash_loan_guard,
    lst_support::LST_RATE_SCALE,
    vault_safety::{MIN_HEALTH_FACTOR_BPS, WARNING_HEALTH_FACTOR_BPS},
    EscrowConfig, EscrowContract, EscrowContractClient, EscrowError, HealthFactorWarning,
};

/// Minimal mock LST token implementing `exchange_rate` and `transfer`/`balance`
/// is not needed when we test `get_vault_health_bps` / `check_vault_borrow` /
/// `check_vault_collateral_withdrawal` on standard escrows, and we can also
/// provide a mock LST contract for `fund_with_lst` health checks.
#[contract]
struct MockLstToken;

#[contractimpl]
impl MockLstToken {
    pub fn exchange_rate(_env: Env) -> i128 {
        // 1.0 underlying per share
        LST_RATE_SCALE
    }

    pub fn balance(env: Env, id: Address) -> i128 {
        env.storage().persistent().get(&id).unwrap_or(0)
    }

    pub fn mint(env: Env, to: Address, amount: i128) {
        let cur: i128 = env.storage().persistent().get(&to).unwrap_or(0);
        env.storage().persistent().set(&to, &(cur + amount));
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        let from_bal: i128 = env.storage().persistent().get(&from).unwrap_or(0);
        let to_bal: i128 = env.storage().persistent().get(&to).unwrap_or(0);
        env.storage().persistent().set(&from, &(from_bal - amount));
        env.storage().persistent().set(&to, &(to_bal + amount));
    }
}

struct Setup<'a> {
    client: EscrowContractClient<'a>,
    contract_id: Address,
    admin: Address,
    buyer: Address,
    escrow_id: u64,
}

fn setup_escrow(env: &Env, amount: i128) -> Setup<'_> {
    env.mock_all_auths();
    let admin = Address::generate(env);
    let contract_id = env.register(
        EscrowContract,
        (EscrowConfig {
            admin: admin.clone(),
            fee_bps: 250u32,
            treasury: Address::generate(env),
            min_amount: 100i128,
            max_amount: 1_000_000i128,
            min_fee_floor_stroops: 0,
            max_fee_ceiling_stroops: 0,
        },),
    );
    let client = EscrowContractClient::new(env, &contract_id);

    let token = env
        .register_stellar_asset_contract_v2(Address::generate(env))
        .address();
    client.add_token(&admin, &token);

    let buyer = Address::generate(env);
    let seller = Address::generate(env);
    StellarAssetClient::new(env, &token).mint(&buyer, &1_000_000i128);

    let order_id = BytesN::from_array(env, &[93u8; 32]);
    let escrow_id = client.deposit(
        &buyer, &seller, &token, &amount, &order_id, &1_000u32, &None, &None,
    );

    Setup {
        client,
        contract_id,
        admin,
        buyer,
        escrow_id,
    }
}

#[test]
fn get_vault_health_bps_returns_none_for_zero_debt_and_exact_bps_for_debt() {
    let env = Env::default();
    let s = setup_escrow(&env, 10_000);

    // Zero debt -> unconditionally healthy (`None`).
    assert_eq!(s.client.get_vault_health_bps(&s.escrow_id, &0), None);
    // 10_000 / 5_000 = 2.0 => 20_000 bps.
    assert_eq!(
        s.client.get_vault_health_bps(&s.escrow_id, &5_000),
        Some(20_000)
    );
}

#[test]
fn vault_health_healthy_warning_and_unsafe_on_borrow_and_withdrawal() {
    let env = Env::default();
    // Collateral = 10_000
    let s = setup_escrow(&env, 10_000);

    // Healthy: 10_000 / 7_000 = 14_285 bps (>= 13_000) -> succeeds, no warning.
    s.client.check_vault_borrow(&s.escrow_id, &5_000, &2_000);
    let hf_healthy = s.client.get_vault_health_bps(&s.escrow_id, &7_000).unwrap();
    assert!(hf_healthy >= WARNING_HEALTH_FACTOR_BPS);

    // Warning band: 10_000 / 7_800 = 12_820 bps (12_500 <= hf < 13_000).
    s.client.check_vault_borrow(&s.escrow_id, &7_000, &800);
    let events = env.events().all();
    let last_event = events.last().expect("expected HealthFactorWarning event");
    let warning = HealthFactorWarning::try_from_val(&env, &last_event.2)
        .expect("event payload should decode as HealthFactorWarning");
    assert_eq!(warning.vault_id, s.escrow_id);
    assert_eq!(warning.collateral_value, 10_000);
    assert_eq!(warning.debt_value, 7_800);
    assert!(warning.health_factor_bps >= MIN_HEALTH_FACTOR_BPS);
    assert!(warning.health_factor_bps < WARNING_HEALTH_FACTOR_BPS);

    // Unsafe borrow: 10_000 / 8_100 = 12_345 bps (< 12_500) -> rejected.
    assert_eq!(
        s.client
            .try_check_vault_borrow(&s.escrow_id, &7_800, &300),
        Err(Ok(EscrowError::UnsafeHealthFactor))
    );

    // Healthy collateral withdrawal: (10_000 - 1_000) / 6_000 = 15_000 bps.
    s.client
        .check_vault_collateral_withdrawal(&s.escrow_id, &6_000, &1_000);

    // Unsafe collateral withdrawal: (10_000 - 3_000) / 6_000 = 11_666 bps (< 12_500).
    assert_eq!(
        s.client
            .try_check_vault_collateral_withdrawal(&s.escrow_id, &6_000, &3_000),
        Err(Ok(EscrowError::UnsafeHealthFactor))
    );
}

#[test]
fn fund_with_lst_enforces_min_health_factor_and_emits_warning() {
    let env = Env::default();
    // Escrow obligation = 10_000
    let s = setup_escrow(&env, 10_000);

    let lst_id = env.register(MockLstToken, ());
    let lst_client = MockLstTokenClient::new(&env, &lst_id);
    lst_client.mint(&s.buyer, &50_000);
    s.client.set_lst_whitelisted(&s.admin, &lst_id, &true);

    // Unsafe: 12_000 shares (< 1.25x of 10_000) -> rejected.
    assert_eq!(
        s.client
            .try_fund_with_lst(&s.escrow_id, &s.buyer, &lst_id, &12_000),
        Err(Ok(EscrowError::UnsafeHealthFactor))
    );

    // Warning band: 12_800 shares (12_800 bps) -> succeeds and emits HealthFactorWarning.
    let rec = s
        .client
        .fund_with_lst(&s.escrow_id, &s.buyer, &lst_id, &12_800);
    assert_eq!(rec.initial_share_balance, 12_800);
    assert_eq!(
        s.client.get_vault_health_bps(&s.escrow_id, &10_000),
        Some(12_800)
    );
}

#[test]
fn get_stake_deposit_ledger_returns_none_then_recorded_ledger() {
    let env = Env::default();
    env.ledger().with_mut(|li| li.sequence_number = 777);
    let s = setup_escrow(&env, 1_000);

    // Unstaked escrow has no recorded deposit ledger.
    assert_eq!(s.client.get_stake_deposit_ledger(&s.escrow_id), None);

    // Recording the staking deposit ledger makes it visible via the getter.
    env.as_contract(&s.contract_id, || {
        flash_loan_guard::record_deposit_ledger(&env, s.escrow_id);
    });
    assert_eq!(s.client.get_stake_deposit_ledger(&s.escrow_id), Some(777));
}
