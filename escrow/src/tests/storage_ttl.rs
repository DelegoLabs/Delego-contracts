//! TTL regression tests for the long-lived escrow storage paths (#768).
//!
//! Each test seeds one persistent key, ages the ledger past the bump
//! threshold, reads it through its public accessor, and verifies that the
//! entry was refreshed instead of expiring.

use crate::{
    blend_yield, emergency_unwind, flash_loan_guard, DataKey, EmergencyRescueProposal,
    EscrowConfig, EscrowContract, EscrowContractClient, YieldBearingEscrowConfig,
};
use soroban_sdk::{
    testutils::{storage::Persistent, Address as _, Ledger},
    Address, Env, Vec,
};

fn setup<'a>(env: &Env) -> (EscrowContractClient<'a>, Address, Address) {
    env.mock_all_auths();
    let admin = Address::generate(env);
    let contract_id = env.register(
        EscrowContract,
        (EscrowConfig {
            admin: admin.clone(),
            fee_bps: 0,
            treasury: Address::generate(env),
            min_amount: 1,
            max_amount: 1_000_000,
            min_fee_floor_stroops: 0,
            max_fee_ceiling_stroops: 0,
        },),
    );
    (
        EscrowContractClient::new(env, &contract_id),
        contract_id,
        admin,
    )
}

fn assert_read_extends_ttl<F, G>(env: &Env, contract: &Address, ttl: G, read: F)
where
    F: FnOnce(),
    G: Fn() -> u32,
{
    env.as_contract(contract, || {
        env.storage().instance().extend_ttl(1_000_000, 1_000_000);
    });
    let initial = ttl();
    let jump = initial - crate::PERSISTENT_BUMP_THRESHOLD + 1;
    env.ledger().with_mut(|li| li.sequence_number += jump);
    let before = ttl();
    read();
    let after = ttl();
    assert!(
        after > before,
        "persistent entry TTL was not extended on read"
    );
    assert!(after >= crate::PERSISTENT_BUMP_AMOUNT);
}

#[test]
fn emergency_rescue_proposal_survives_past_bump_threshold() {
    let env = Env::default();
    let (client, contract_id, _admin) = setup(&env);
    let escrow_id = 31u64;
    let proposal = EmergencyRescueProposal {
        escrow_id,
        recovery_destination: Address::generate(&env),
        proposed_at: env.ledger().timestamp(),
        approvals: Vec::new(&env),
        executed: false,
    };
    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::EmergencyRescueProposal(escrow_id), &proposal);
    });

    assert_read_extends_ttl(
        &env,
        &contract_id,
        || {
            env.as_contract(&contract_id, || {
                env.storage()
                    .persistent()
                    .get_ttl(&DataKey::EmergencyRescueProposal(escrow_id))
            })
        },
        || {
            assert_eq!(
                client.get_emergency_rescue_proposal(&escrow_id),
                Some(proposal)
            );
        },
    );
}

#[test]
fn detached_pool_flag_survives_past_bump_threshold() {
    let env = Env::default();
    let (client, contract_id, _admin) = setup(&env);
    let pool = Address::generate(&env);
    let key = blend_yield::Key::DetachedPool(pool.clone());
    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .set(&key, &env.ledger().sequence());
    });

    assert_read_extends_ttl(
        &env,
        &contract_id,
        || env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key)),
        || assert!(client.is_defi_pool_detached(&pool)),
    );
}

#[test]
fn stranded_principal_survives_past_bump_threshold() {
    let env = Env::default();
    let (client, contract_id, _admin) = setup(&env);
    let pool = Address::generate(&env);
    let key = blend_yield::Key::StrandedPrincipal(pool.clone());
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(&key, &900i128);
    });

    assert_read_extends_ttl(
        &env,
        &contract_id,
        || env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key)),
        || assert_eq!(client.get_stranded_principal(&pool), 900),
    );
}

#[test]
fn blend_config_survives_past_bump_threshold() {
    let env = Env::default();
    let (client, contract_id, _admin) = setup(&env);
    let escrow_id = 41u64;
    let pool = Address::generate(&env);
    let config = YieldBearingEscrowConfig {
        lending_pool_contract: pool.clone(),
        b_token_address: pool,
        accrued_yield_stroops: 7,
        yield_recipient: Address::generate(&env),
    };
    let key = blend_yield::Key::BlendConfig(escrow_id);
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(&key, &config);
    });

    assert_read_extends_ttl(
        &env,
        &contract_id,
        || env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key)),
        || assert_eq!(client.get_blend_yield_config(&escrow_id), Some(config)),
    );
}

#[test]
fn stake_deposit_ledger_survives_past_bump_threshold() {
    let env = Env::default();
    let (_client, contract_id, _admin) = setup(&env);
    let escrow_id = 43u64;
    let key = flash_loan_guard::FlashLoanKey::StakeDepositLedger(escrow_id);
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(&key, &17u32);
    });

    assert_read_extends_ttl(
        &env,
        &contract_id,
        || env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key)),
        || assert_eq!(flash_loan_guard::deposit_ledger(&env, escrow_id), Some(17)),
    );
}

#[test]
fn detach_approvals_survive_past_bump_threshold() {
    let env = Env::default();
    let (client, contract_id, _admin) = setup(&env);
    let pool = Address::generate(&env);
    let approvers = Vec::from_array(&env, [Address::generate(&env)]);
    let key = emergency_unwind::UnwindKey::DetachApprovals(pool.clone());
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(&key, &approvers);
    });

    assert_read_extends_ttl(
        &env,
        &contract_id,
        || env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key)),
        || assert_eq!(client.get_defi_pool_detach_approvals(&pool), approvers),
    );
}
