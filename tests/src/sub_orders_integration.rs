#![cfg(test)]

use delego_escrow::{
    AdminAction, EscrowConfig, EscrowContract, EscrowContractClient, EscrowError, EscrowStatus,
    SubOrder,
};
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    token, Address, BytesN, Env, Vec,
};

struct Setup {
    env: Env,
    client: EscrowContractClient<'static>,
    admin: Address,
    buyer: Address,
    merchant_a: Address,
    merchant_b: Address,
    token: Address,
    escrow_id: u64,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    let merchant_a = Address::generate(&env);
    let merchant_b = Address::generate(&env);

    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    token::StellarAssetClient::new(&env, &token).mint(&buyer, &10_000);

    let config = EscrowConfig {
        admin: admin.clone(),
        fee_bps: 0,
        treasury: admin.clone(),
        min_amount: 1,
        max_amount: 100_000,
        min_fee_floor_stroops: 0,
        max_fee_ceiling_stroops: 0,
    };
    let contract_id = env.register(EscrowContract, (config,));
    let client = EscrowContractClient::new(&env, &contract_id);
    client.set_security_guardian(&admin, &Address::generate(&env));
    let action = AdminAction::AddToken(token.clone());
    let queued = client.queue_admin_action(&admin, &action);
    let proposal = client.get_admin_action(&queued).unwrap();
    env.ledger().with_mut(|l| {
        l.sequence_number = proposal.pending.unlock_ledger;
        l.timestamp = proposal.unlock_timestamp;
    });
    client.add_token(&admin, &token);

    let escrow_id = client.deposit(
        &buyer,
        &seller,
        &token,
        &1_000,
        &BytesN::from_array(&env, &[42u8; 32]),
        &1_000u32,
        &None,
        &None,
    );

    Setup {
        env,
        client,
        admin,
        buyer,
        merchant_a,
        merchant_b,
        token,
        escrow_id,
    }
}

fn orders(s: &Setup) -> Vec<SubOrder> {
    let mut orders = Vec::new(&s.env);
    orders.push_back(SubOrder {
        sub_order_id: 1,
        merchant: s.merchant_a.clone(),
        amount: 400,
        status: EscrowStatus::Funded,
    });
    orders.push_back(SubOrder {
        sub_order_id: 2,
        merchant: s.merchant_b.clone(),
        amount: 600,
        status: EscrowStatus::Funded,
    });
    orders
}

#[test]
fn register_and_get_multiple_sub_orders() {
    let s = setup();
    s.client
        .register_sub_order(&s.buyer, &s.escrow_id, &orders(&s));

    let first = s.client.get_sub_order(&s.escrow_id, &1);
    assert_eq!(first.amount, 400);
    assert_eq!(first.merchant, s.merchant_a);
    assert_eq!(first.status, EscrowStatus::Funded);

    let second = s.client.get_sub_order(&s.escrow_id, &2);
    assert_eq!(second.amount, 600);
    assert_eq!(second.merchant, s.merchant_b);
}

#[test]
fn release_sub_order_transfers_once_and_updates_parent() {
    let s = setup();
    s.client
        .register_sub_order(&s.buyer, &s.escrow_id, &orders(&s));

    let released = s.client.release_sub_order(&s.buyer, &s.escrow_id, &1);
    assert_eq!(released.status, EscrowStatus::Released);

    let token = token::Client::new(&s.env, &s.token);
    assert_eq!(token.balance(&s.merchant_a), 400);

    let parent = s.client.get_escrow(&s.escrow_id);
    assert_eq!(parent.released_amount, 400);
    assert_eq!(parent.status, EscrowStatus::Funded);

    let replay = s.client.try_release_sub_order(&s.buyer, &s.escrow_id, &1);
    assert_eq!(replay, Err(Ok(EscrowError::AlreadyReleased)));
    assert_eq!(token.balance(&s.merchant_a), 400);
}

#[test]
fn dispute_and_authorized_release_resolution() {
    let s = setup();
    s.client
        .register_sub_order(&s.buyer, &s.escrow_id, &orders(&s));

    let disputed = s.client.dispute_sub_order(&s.merchant_a, &s.escrow_id, &1);
    assert_eq!(disputed.status, EscrowStatus::Disputed);

    let resolved = s
        .client
        .resolve_sub_order(&s.admin, &s.escrow_id, &1, &EscrowStatus::Released);
    assert_eq!(resolved.status, EscrowStatus::Released);

    let token = token::Client::new(&s.env, &s.token);
    assert_eq!(token.balance(&s.merchant_a), 400);

    let parent = s.client.get_escrow(&s.escrow_id);
    assert_eq!(parent.released_amount, 400);
}

#[test]
fn authorized_refund_resolution_returns_funds_to_buyer() {
    let s = setup();
    s.client
        .register_sub_order(&s.buyer, &s.escrow_id, &orders(&s));

    s.client.dispute_sub_order(&s.buyer, &s.escrow_id, &2);
    let resolved = s
        .client
        .resolve_sub_order(&s.admin, &s.escrow_id, &2, &EscrowStatus::Refunded);
    assert_eq!(resolved.status, EscrowStatus::Refunded);

    let token = token::Client::new(&s.env, &s.token);
    assert_eq!(token.balance(&s.buyer), 10_000 - 1_000 + 600);

    let parent = s.client.get_escrow(&s.escrow_id);
    assert_eq!(parent.refunded_amount, 600);
}

#[test]
fn unauthorized_callers_are_rejected() {
    let s = setup();
    s.client
        .register_sub_order(&s.buyer, &s.escrow_id, &orders(&s));
    let intruder = Address::generate(&s.env);

    let release = s.client.try_release_sub_order(&intruder, &s.escrow_id, &1);
    assert_eq!(release, Err(Ok(EscrowError::Unauthorized)));

    let dispute = s.client.try_dispute_sub_order(&intruder, &s.escrow_id, &1);
    assert_eq!(dispute, Err(Ok(EscrowError::Unauthorized)));

    s.client.dispute_sub_order(&s.buyer, &s.escrow_id, &1);
    let resolve =
        s.client
            .try_resolve_sub_order(&intruder, &s.escrow_id, &1, &EscrowStatus::Released);
    assert_eq!(resolve, Err(Ok(EscrowError::Unauthorized)));
}

#[test]
fn registration_exceeding_parent_amount_is_rejected() {
    let s = setup();
    let mut too_much = Vec::new(&s.env);
    too_much.push_back(SubOrder {
        sub_order_id: 1,
        merchant: s.merchant_a.clone(),
        amount: 700,
        status: EscrowStatus::Funded,
    });
    too_much.push_back(SubOrder {
        sub_order_id: 2,
        merchant: s.merchant_b.clone(),
        amount: 700,
        status: EscrowStatus::Funded,
    });

    let result = s
        .client
        .try_register_sub_order(&s.buyer, &s.escrow_id, &too_much);
    assert_eq!(result, Err(Ok(EscrowError::ExceedsTotalEscrowAmount)));
}

#[test]
fn duplicate_registration_and_invalid_transitions_are_rejected() {
    let s = setup();
    s.client
        .register_sub_order(&s.buyer, &s.escrow_id, &orders(&s));

    let duplicate = s
        .client
        .try_register_sub_order(&s.buyer, &s.escrow_id, &orders(&s));
    assert_eq!(duplicate, Err(Ok(EscrowError::InvalidStatus)));

    let resolve_undisputed =
        s.client
            .try_resolve_sub_order(&s.admin, &s.escrow_id, &1, &EscrowStatus::Released);
    assert_eq!(resolve_undisputed, Err(Ok(EscrowError::NotDisputed)));
}
