// Copyright 2024 Delego Labs
// SPDX-License-Identifier: Apache-2.0
//
// Flash loan protection guard tests (issue #594).
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env,
};

use crate::{
    flash_loan_guard, guard_flash_loan, EscrowConfig, EscrowContract, EscrowContractClient,
    EscrowError,
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
