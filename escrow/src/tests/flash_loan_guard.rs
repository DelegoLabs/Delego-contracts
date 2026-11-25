// Copyright 2024 Delego Labs
// SPDX-License-Identifier: Apache-2.0
//
// Flash loan protection guard tests (issues #594, #767).
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env,
};

use crate::{
    flash_loan_guard, guard_flash_loan, tests::defi_fixture, EscrowConfig, EscrowContract,
    EscrowContractClient, EscrowError,
};

struct Setup<'a> {
    client: EscrowContractClient<'a>,
    contract_id: Address,
    buyer: Address,
    seller: Address,
    escrow_id: u64,
}

/// Registers the escrow contract and deposits a funded 1 000-unit escrow.
fn setup_funded_escrow(env: &Env) -> Setup<'_> {
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

    let order_id = BytesN::from_array(env, &[94u8; 32]);
    let escrow_id = client.deposit(
        &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
    );

    Setup {
        client,
        contract_id,
        buyer,
        seller,
        escrow_id,
    }
}

#[test]
fn guard_flash_loan_rejects_same_ledger_and_allows_later_ledger() {
    let env = Env::default();
    env.ledger().with_mut(|li| li.sequence_number = 500);

    assert_eq!(
        guard_flash_loan(&env, 500),
        Err(EscrowError::SameLedgerManipulationDetected)
    );
    assert_eq!(guard_flash_loan(&env, 499), Ok(()));
}

#[test]
fn same_ledger_staked_deposit_and_withdrawal_is_rejected() {
    let env = Env::default();
    let s = setup_funded_escrow(&env);

    // Mark the escrow balance as staked in the deposit ledger, exactly as
    // `fund` does for escrows supplied to a lending pool.
    env.as_contract(&s.contract_id, || {
        flash_loan_guard::record_deposit_ledger(&env, s.escrow_id);
    });

    // Same-ledger release and refund attempts are both rejected.
    assert_eq!(
        s.client.try_partial_release(&s.escrow_id, &s.buyer, &500i128),
        Err(Ok(EscrowError::SameLedgerManipulationDetected))
    );
    assert_eq!(
        s.client.try_refund(&s.escrow_id, &s.seller),
        Err(Ok(EscrowError::SameLedgerManipulationDetected))
    );

    // Once the ledger advances past the deposit ledger, withdrawal succeeds.
    env.ledger().with_mut(|li| li.sequence_number += 1);
    let result = s.client.partial_release(&s.escrow_id, &s.buyer, &500i128);
    assert_eq!(result.released, 500i128);
}

#[test]
fn unstaked_escrow_is_not_affected_by_guard() {
    let env = Env::default();
    let s = setup_funded_escrow(&env);

    let result = s.client.partial_release(&s.escrow_id, &s.buyer, &500i128);
    assert_eq!(result.released, 500i128);
}

/// Issue #767: an escrow configured for a yield pool whose supply rate falls
/// below the minimum viable threshold (`should_stake == false`) never opens a
/// pool position. `fund` must therefore not stamp
/// `FlashLoanKey::StakeDepositLedger`, and same-ledger release/refund must
/// not be rejected with `SameLedgerManipulationDetected`.
#[test]
fn low_apy_bypass_escrow_allows_same_ledger_release_and_refund() {
    let f = defi_fixture::setup();

    // 20 bps is below the default 50 bps floor, so `should_stake` is false.
    f.client.set_pool_supply_rate(&f.admin, &f.pool, &20);
    let id = defi_fixture::create_with_blend(&f);
    f.client.fund(&id, &f.buyer);

    // No pool position was opened and the funds stayed in the vault.
    assert_eq!(f.pool_shares(), 0);
    assert_eq!(f.balance(&f.contract), 1_000);

    // The staking deposit ledger must NOT have been recorded.
    assert!(
        f.env
            .as_contract(&f.contract, || flash_loan_guard::deposit_ledger(
                &f.env, id
            ))
            .is_none()
    );

    // Same-ledger partial release must succeed (not SameLedgerManipulationDetected).
    let result = f.client.partial_release(&id, &f.buyer, &400i128);
    assert_eq!(result.released, 400i128);

    // Same-ledger refund of the remaining balance must also succeed.
    let refunded = f.client.refund(&id, &f.seller);
    assert!(refunded);
    // Buyer receives full 10_000 back (400 from partial release + 600 from refund).
    assert_eq!(f.balance(&f.buyer), 10_000);
}

/// Issue #594 (staked side of #767): a deposit that actually reached the pool
/// still records the deposit ledger and still blocks same-ledger withdrawal.
#[test]
fn staked_escrow_still_blocks_same_ledger_withdrawal() {
    let f = defi_fixture::setup();
    let id = defi_fixture::create_with_blend(&f);
    f.client.fund(&id, &f.buyer);

    // A pool position was opened for the full deposit.
    assert_eq!(f.pool_shares(), 1_000);

    // The deposit ledger was stamped.
    assert!(
        f.env
            .as_contract(&f.contract, || flash_loan_guard::deposit_ledger(
                &f.env, id
            ))
            .is_some()
    );

    // Same-ledger withdrawal is rejected.
    assert_eq!(
        f.client.try_partial_release(&id, &f.buyer, &500i128),
        Err(Ok(EscrowError::SameLedgerManipulationDetected))
    );
    assert_eq!(
        f.client.try_refund(&id, &f.seller),
        Err(Ok(EscrowError::SameLedgerManipulationDetected))
    );

    // After the ledger advances, withdrawal succeeds.
    f.env.ledger().with_mut(|li| li.sequence_number += 1);
    let result = f.client.partial_release(&id, &f.buyer, &500i128);
    assert_eq!(result.released, 500i128);
}
