#![cfg(test)]

use crate::{
    BatchDepositParams, BatchRefundParams, BatchReleaseParams, EscrowConfig, EscrowContract,
    EscrowContractClient, EscrowError, EscrowStatus, EscrowTerminalState, TreasuryShare,
    MAX_TREASURIES,
};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events, Ledger, MockAuth, MockAuthInvoke},
    Address, BytesN, Env, IntoVal, Symbol, TryIntoVal, Vec,
};

pub(crate) struct TestEnv {
    pub(crate) env: Env,
    pub(crate) admin: Address,
    pub(crate) buyer: Address,
    pub(crate) seller: Address,
    pub(crate) agent: Address,
    pub(crate) token_contract_id: Address,
    pub(crate) escrow_contract_id: Address,
}

impl TestEnv {
    pub(crate) fn setup() -> Self {
        Self::setup_with_fee_bps(0)
    }

    fn setup_with_fee_bps(fee_bps: u32) -> Self {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let agent = Address::generate(&env);
        let treasury = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_contract_id = env
            .register_stellar_asset_contract_v2(token_admin.clone())
            .address();
        let token_admin_client =
            soroban_sdk::token::StellarAssetClient::new(&env, &token_contract_id);
        token_admin_client.mint(&buyer, &10000);

        let min_amount = 100i128;
        let max_amount = 10000i128;
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps,
            treasury: treasury.clone(),
            min_amount,
            max_amount,
        };
        let escrow_contract_id = env.register(EscrowContract, (config,));
        let escrow_client = EscrowContractClient::new(&env, &escrow_contract_id);
        escrow_client.add_token(&admin, &token_contract_id);

        TestEnv {
            env,
            admin,
            buyer,
            seller,
            agent,
            token_contract_id,
            escrow_contract_id,
        }
    }

    fn order_id(&self) -> BytesN<32> {
        BytesN::from_array(&self.env, &[7u8; 32])
    }
}

pub(crate) fn deposit_escrow(t: &TestEnv, amount: i128, timeout_ledgers: u32) -> u64 {
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    escrow_client.deposit(
        &t.buyer,
        &t.seller,
        &t.token_contract_id,
        &amount,
        &t.order_id(),
        &timeout_ledgers,
        &None,
        &None,
    )
}

#[test]
fn test_deposit_with_whitelisted_token_succeeds() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    assert!(escrow_client.is_token_allowed(&t.token_contract_id));
    let escrow_id = deposit_escrow(&t, 1000, 100);

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.token, t.token_contract_id);
    assert_eq!(record.status, EscrowStatus::Funded);
}

#[test]
fn test_deposit_with_non_whitelisted_token_fails() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let other_token_admin = Address::generate(&t.env);
    let other_token_contract_id = t
        .env
        .register_stellar_asset_contract_v2(other_token_admin.clone())
        .address();

    assert_eq!(
        escrow_client.try_deposit(
            &t.buyer,
            &t.seller,
            &other_token_contract_id,
            &1000,
            &t.order_id(),
            &100,
            &None,
            &None,
        ),
        Err(Ok(EscrowError::TokenNotWhitelisted))
    );
}

#[test]
fn test_add_token_by_non_admin_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let agent = Address::generate(&env);
    let treasury = Address::generate(&env);

    let fee_bps = 0u32;
    let min_amount = 100i128;
    let max_amount = 10000i128;
    let config = EscrowConfig {
        admin: admin.clone(),
        fee_bps,
        treasury: treasury.clone(),
        min_amount,
        max_amount,
    };
    let escrow_contract_id = env.register(EscrowContract, (config,));
    let escrow_client = EscrowContractClient::new(&env, &escrow_contract_id);

    let new_token = Address::generate(&env);

    assert_eq!(
        escrow_client.try_add_token(&agent, &new_token),
        Err(Ok(EscrowError::Unauthorized))
    );
    assert!(!escrow_client.is_token_allowed(&new_token));
}

#[test]
fn test_remove_token_blocks_future_deposit() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    assert!(escrow_client.remove_token(&t.admin, &t.token_contract_id));
    assert!(!escrow_client.is_token_allowed(&t.token_contract_id));
    assert_eq!(
        escrow_client.try_deposit(
            &t.buyer,
            &t.seller,
            &t.token_contract_id,
            &1000,
            &t.order_id(),
            &100,
            &None,
            &None,
        ),
        Err(Ok(EscrowError::TokenNotWhitelisted))
    );
}

#[test]
fn test_list_tokens_returns_all_added_tokens() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let second_token = Address::generate(&t.env);

    assert!(escrow_client.add_token(&t.admin, &second_token));

    let tokens = escrow_client.list_tokens();
    assert_eq!(tokens.len(), 2);
    assert!(tokens.contains(&t.token_contract_id));
    assert!(tokens.contains(&second_token));
}

#[test]
fn test_add_token_is_idempotent() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    assert!(escrow_client.add_token(&t.admin, &t.token_contract_id));
    assert!(escrow_client.add_token(&t.admin, &t.token_contract_id));

    let tokens = escrow_client.list_tokens();
    assert_eq!(tokens.len(), 1);
    assert!(tokens.contains(&t.token_contract_id));
}

#[test]
fn test_large_whitelist_pagination() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    // Add 150 tokens
    for _ in 0..150 {
        let new_token = Address::generate(&t.env);
        assert!(escrow_client.add_token(&t.admin, &new_token));
    }

    let tokens = escrow_client.list_tokens();
    assert_eq!(tokens.len(), 151); // 1 from setup + 150

    // Test pagination
    let page_1 = escrow_client.list_tokens_paginated(&0, &50);
    assert_eq!(page_1.len(), 50);

    let page_2 = escrow_client.list_tokens_paginated(&50, &50);
    assert_eq!(page_2.len(), 50);

    let page_4 = escrow_client.list_tokens_paginated(&150, &50);
    assert_eq!(page_4.len(), 1); // Only 1 left

    let empty_page = escrow_client.list_tokens_paginated(&151, &50);
    assert_eq!(empty_page.len(), 0);
}

#[test]
fn test_full_purchase_lifecycle() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let amount = 1000i128;
    let timeout_ledgers = 100u32;

    assert_eq!(token_client.balance(&t.buyer), 10000);
    assert_eq!(token_client.balance(&t.seller), 0);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 0);

    let escrow_id = deposit_escrow(&t, amount, timeout_ledgers);

    assert_eq!(token_client.balance(&t.buyer), 9000);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 1000);

    assert!(escrow_client.release(&escrow_id, &t.buyer, &t.seller));

    assert_eq!(token_client.balance(&t.seller), 1000);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 0);

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.status, EscrowStatus::Released);
    assert_eq!(record.escrow_id, escrow_id);
}

#[test]
fn test_full_refund_lifecycle() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    assert!(escrow_client.refund(&escrow_id, &t.seller));

    assert_eq!(token_client.balance(&t.buyer), 10000);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 0);

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.status, EscrowStatus::Refunded);
}

#[test]
fn test_dispute_resolution_to_seller() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    assert!(escrow_client.dispute(&escrow_id, &t.buyer));
    assert!(escrow_client.resolve_dispute(&escrow_id, &t.admin, &true));

    assert_eq!(token_client.balance(&t.seller), 1000);
    assert_eq!(token_client.balance(&t.buyer), 9000);

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.status, EscrowStatus::Released);
}

#[test]
fn test_dispute_resolution_to_buyer() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    assert!(escrow_client.dispute(&escrow_id, &t.seller));
    assert!(escrow_client.resolve_dispute(&escrow_id, &t.admin, &false));

    assert_eq!(token_client.balance(&t.seller), 0);
    assert_eq!(token_client.balance(&t.buyer), 10000);

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.status, EscrowStatus::Refunded);
}

#[test]
fn test_dispute_blocks_release_and_refund() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    assert!(escrow_client.dispute(&escrow_id, &t.buyer));

    assert_eq!(
        escrow_client.try_release(&escrow_id, &t.buyer, &t.seller),
        Err(Ok(EscrowError::InvalidStatus))
    );
    assert_eq!(
        escrow_client.try_refund(&escrow_id, &t.seller),
        Err(Ok(EscrowError::InvalidStatus))
    );
}

#[test]
#[should_panic]
fn test_deposit_insufficient_balance() {
    let t = TestEnv::setup();
    deposit_escrow(&t, 15000, 100);
}

#[test]
fn test_release_wrong_caller() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    assert_eq!(
        escrow_client.try_release(&escrow_id, &t.agent, &t.seller),
        Err(Ok(EscrowError::Unauthorized))
    );
    assert_eq!(
        escrow_client.get_escrow(&escrow_id).status,
        EscrowStatus::Funded
    );
}

#[test]
fn test_release_with_wrong_recipient_fails() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    let wrong_recipient = Address::generate(&t.env);
    assert_eq!(
        escrow_client.try_release(&escrow_id, &t.buyer, &wrong_recipient),
        Err(Ok(EscrowError::InvalidReleaseRecipient))
    );
}

#[test]
fn test_double_release_prevention() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    assert!(escrow_client.release(&escrow_id, &t.buyer, &t.seller));
    assert_eq!(token_client.balance(&t.seller), 1000);
    assert_eq!(
        escrow_client.get_escrow(&escrow_id).status,
        EscrowStatus::Released
    );

    assert_eq!(
        escrow_client.try_release(&escrow_id, &t.buyer, &t.seller),
        Err(Ok(EscrowError::AlreadyReleased))
    );
    assert_eq!(token_client.balance(&t.seller), 1000);
}

#[test]
fn test_double_refund_prevention() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    assert!(escrow_client.refund(&escrow_id, &t.seller));
    assert_eq!(
        escrow_client.try_refund(&escrow_id, &t.seller),
        Err(Ok(EscrowError::AlreadyRefunded))
    );
}

#[test]
fn test_release_on_refunded_escrow_fails() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    assert!(escrow_client.refund(&escrow_id, &t.seller));
    assert_eq!(
        escrow_client.try_release(&escrow_id, &t.buyer, &t.seller),
        Err(Ok(EscrowError::AlreadyRefunded))
    );
}

#[test]
fn test_refund_on_released_escrow_fails() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    assert!(escrow_client.release(&escrow_id, &t.buyer, &t.seller));
    assert_eq!(
        escrow_client.try_refund(&escrow_id, &t.seller),
        Err(Ok(EscrowError::AlreadyReleased))
    );
}

#[test]
fn test_terminal_state_from_status() {
    assert_eq!(
        EscrowTerminalState::from_status(&EscrowStatus::Released),
        Some(EscrowTerminalState::Released)
    );
    assert_eq!(
        EscrowTerminalState::from_status(&EscrowStatus::Refunded),
        Some(EscrowTerminalState::Refunded)
    );
    assert_eq!(
        EscrowTerminalState::from_status(&EscrowStatus::Funded),
        None
    );
    assert_eq!(
        EscrowTerminalState::from_status(&EscrowStatus::Disputed),
        None
    );
}

#[test]
fn test_refund_before_timeout_fails() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    assert_eq!(
        escrow_client.try_refund(&escrow_id, &t.buyer),
        Err(Ok(EscrowError::TimeoutNotReached))
    );
}

#[test]
fn test_timeout_auto_refund() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let timeout_ledgers = 100u32;
    let escrow_id = deposit_escrow(&t, 1000, timeout_ledgers);

    let record = escrow_client.get_escrow(&escrow_id);
    t.env.ledger().set_sequence_number(record.timeout_ledger);

    assert!(escrow_client.refund(&escrow_id, &t.buyer));
    assert_eq!(token_client.balance(&t.buyer), 10000);
}

#[test]
fn test_deposit_requires_buyer_auth() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let deposit_invoke = MockAuthInvoke {
        contract: &t.escrow_contract_id,
        fn_name: "deposit",
        args: (
            t.buyer.clone(),
            t.seller.clone(),
            t.token_contract_id.clone(),
            1000i128,
            t.order_id(),
            100u32,
            Option::<BytesN<32>>::None,
            Option::<soroban_sdk::Symbol>::None,
        )
            .into_val(&t.env),
        sub_invokes: &[],
    };

    let res = escrow_client
        .mock_auths(&[MockAuth {
            address: &t.agent,
            invoke: &deposit_invoke,
        }])
        .try_deposit(
            &t.buyer,
            &t.seller,
            &t.token_contract_id,
            &1000,
            &t.order_id(),
            &100,
            &None,
            &None,
        );
    assert!(res.is_err());
}

#[test]
fn test_get_escrow_returns_full_record() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 500, 50);
    let record = escrow_client.get_escrow(&escrow_id);

    assert_eq!(record.escrow_id, escrow_id);
    assert_eq!(record.buyer, t.buyer);
    assert_eq!(record.seller, t.seller);
    assert_eq!(record.token, t.token_contract_id);
    assert_eq!(record.amount, 500);
    assert_eq!(record.released_amount, 0);
    assert_eq!(record.status, EscrowStatus::Funded);
    assert_eq!(record.order_id, t.order_id());
    assert!(record.timeout_ledger > t.env.ledger().sequence());
}

// ── Issue #173: RefundEligibility getter tests ──────────────────────────

#[test]
fn test_refund_eligibility_seller_always_eligible() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let eid = deposit_escrow(&t, 1000, 100);

    let re = client.get_refund_eligibility(&eid, &t.seller);
    assert_eq!(re.escrow_id, eid);
    assert!(re.eligible);
    assert_eq!(re.reason, soroban_sdk::symbol_short!("ok"));
}

#[test]
fn test_refund_eligibility_admin_always_eligible() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let eid = deposit_escrow(&t, 1000, 100);

    let re = client.get_refund_eligibility(&eid, &t.admin);
    assert!(re.eligible);
    assert_eq!(re.reason, soroban_sdk::symbol_short!("ok"));
}

#[test]
fn test_refund_eligibility_buyer_before_timeout() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let eid = deposit_escrow(&t, 1000, 100);

    let re = client.get_refund_eligibility(&eid, &t.buyer);
    assert!(!re.eligible);
    assert_eq!(re.reason, soroban_sdk::symbol_short!("timeout"));
}

#[test]
fn test_refund_eligibility_buyer_after_timeout() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let eid = deposit_escrow(&t, 1000, 100);

    let record = client.get_escrow(&eid);
    t.env.ledger().set_sequence_number(record.timeout_ledger);

    let re = client.get_refund_eligibility(&eid, &t.buyer);
    assert!(re.eligible);
    assert_eq!(re.reason, soroban_sdk::symbol_short!("ok"));
}

#[test]
fn test_refund_eligibility_not_found() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let re = client.get_refund_eligibility(&999, &t.buyer);
    assert!(!re.eligible);
    assert_eq!(re.reason, soroban_sdk::symbol_short!("notfund"));
}

#[test]
fn test_refund_eligibility_already_released() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let eid = deposit_escrow(&t, 1000, 100);
    client.release(&eid, &t.buyer, &t.seller);

    let re = client.get_refund_eligibility(&eid, &t.seller);
    assert!(!re.eligible);
    assert_eq!(re.reason, soroban_sdk::symbol_short!("released"));
}

#[test]
fn test_refund_eligibility_already_refunded() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let eid = deposit_escrow(&t, 1000, 100);
    client.refund(&eid, &t.seller);

    let re = client.get_refund_eligibility(&eid, &t.buyer);
    assert!(!re.eligible);
    assert_eq!(re.reason, soroban_sdk::symbol_short!("refunded"));
}

#[test]
fn test_refund_eligibility_disputed() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let eid = deposit_escrow(&t, 1000, 100);
    client.dispute(&eid, &t.buyer);

    let re = client.get_refund_eligibility(&eid, &t.seller);
    assert!(!re.eligible);
    assert_eq!(re.reason, soroban_sdk::symbol_short!("disputed"));
}

#[test]
fn test_refund_eligibility_unauthorized_stranger() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let eid = deposit_escrow(&t, 1000, 100);

    let re = client.get_refund_eligibility(&eid, &t.agent);
    assert!(!re.eligible);
    assert_eq!(re.reason, soroban_sdk::symbol_short!("noauth"));
}

// ── ReleaseEligibility getter tests ──────────────────────────────────────

#[test]
fn test_release_eligibility_funded_before_timeout() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let eid = deposit_escrow(&t, 1000, 100);

    let re = client.get_release_eligibility(&eid);
    assert_eq!(re.escrow_id, t.order_id());
    assert!(re.eligible);
    assert_eq!(re.reason, soroban_sdk::symbol_short!("ok"));
}

#[test]
fn test_release_eligibility_disputed_blocks_release() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let eid = deposit_escrow(&t, 1000, 100);
    client.dispute(&eid, &t.buyer);

    let re = client.get_release_eligibility(&eid);
    assert_eq!(re.escrow_id, t.order_id());
    assert!(!re.eligible);
    assert_eq!(re.reason, soroban_sdk::symbol_short!("disputed"));
}

#[test]
fn test_release_eligibility_timeout_blocks_release() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let eid = deposit_escrow(&t, 1000, 100);

    let record = client.get_escrow(&eid);
    t.env.ledger().set_sequence_number(record.timeout_ledger);

    let re = client.get_release_eligibility(&eid);
    assert_eq!(re.escrow_id, t.order_id());
    assert!(!re.eligible);
    assert_eq!(re.reason, soroban_sdk::symbol_short!("timeout"));
}

#[test]
fn test_release_eligibility_terminal_release_blocks_release() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let eid = deposit_escrow(&t, 1000, 100);
    client.release(&eid, &t.buyer, &t.seller);

    let re = client.get_release_eligibility(&eid);
    assert_eq!(re.escrow_id, t.order_id());
    assert!(!re.eligible);
    assert_eq!(re.reason, soroban_sdk::symbol_short!("released"));
}

#[test]
fn test_release_eligibility_terminal_refund_blocks_release() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let eid = deposit_escrow(&t, 1000, 100);
    client.refund(&eid, &t.seller);

    let re = client.get_release_eligibility(&eid);
    assert_eq!(re.escrow_id, t.order_id());
    assert!(!re.eligible);
    assert_eq!(re.reason, soroban_sdk::symbol_short!("refunded"));
}

// ── Release-condition gate for buyer self-release (issue #48) ─────────────

#[test]
fn test_require_release_condition_gates_buyer_release() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    assert!(escrow_client.set_require_release_condition(&t.admin, &escrow_id, &true));
    assert!(escrow_client.get_require_release_condition(&escrow_id));

    // Before timeout the escrow is eligible, so the buyer may release.
    let result = escrow_client.partial_release(&escrow_id, &t.buyer, &400);
    assert_eq!(result.released, 400);
    assert_eq!(token_client.balance(&t.seller), 400);

    // After the timeout, the eligibility gate blocks the buyer's release.
    let record = escrow_client.get_escrow(&escrow_id);
    t.env.ledger().set_sequence_number(record.timeout_ledger);
    assert_eq!(
        escrow_client.try_partial_release(&escrow_id, &t.buyer, &100),
        Err(Ok(EscrowError::ConditionNotMet))
    );
    // The full `release` path is gated too.
    assert_eq!(
        escrow_client.try_release(&escrow_id, &t.buyer, &t.seller),
        Err(Ok(EscrowError::ConditionNotMet))
    );
}

#[test]
fn test_require_release_condition_does_not_gate_admin() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    assert!(escrow_client.set_require_release_condition(&t.admin, &escrow_id, &true));

    // Even after timeout, an admin-initiated release is not gated.
    let record = escrow_client.get_escrow(&escrow_id);
    t.env.ledger().set_sequence_number(record.timeout_ledger);
    assert!(escrow_client.release(&escrow_id, &t.admin, &t.seller));
    assert_eq!(token_client.balance(&t.seller), 1000);
}

#[test]
fn test_require_release_condition_default_off_allows_buyer_release() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    // Default off: the flag reads false and the buyer can release even past timeout.
    assert!(!escrow_client.get_require_release_condition(&escrow_id));

    let record = escrow_client.get_escrow(&escrow_id);
    t.env.ledger().set_sequence_number(record.timeout_ledger);
    assert!(escrow_client.release(&escrow_id, &t.buyer, &t.seller));
    assert_eq!(token_client.balance(&t.seller), 1000);
}

#[test]
fn test_require_release_condition_can_be_disabled() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    assert!(escrow_client.set_require_release_condition(&t.admin, &escrow_id, &true));
    assert!(escrow_client.set_require_release_condition(&t.admin, &escrow_id, &false));
    assert!(!escrow_client.get_require_release_condition(&escrow_id));

    let record = escrow_client.get_escrow(&escrow_id);
    t.env.ledger().set_sequence_number(record.timeout_ledger);
    assert!(escrow_client.release(&escrow_id, &t.buyer, &t.seller));
    assert_eq!(token_client.balance(&t.seller), 1000);
}

#[test]
fn test_set_require_release_condition_rejects_non_admin() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    assert_eq!(
        escrow_client.try_set_require_release_condition(&t.agent, &escrow_id, &true),
        Err(Ok(EscrowError::Unauthorized))
    );
}

#[test]
fn test_set_require_release_condition_not_found() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    assert_eq!(
        escrow_client.try_set_require_release_condition(&t.admin, &999, &true),
        Err(Ok(EscrowError::NotFound))
    );
}

// ── EscrowReceipt getter tests (get_receipt) ─────────────────────────────

/// Success path: receipt returned immediately after deposit (Funded state).
#[test]
fn test_get_receipt_funded_state() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let eid = deposit_escrow(&t, 1000, 100);
    let receipt = client.get_receipt(&eid);

    assert_eq!(receipt.escrow_id, eid);
    assert_eq!(receipt.buyer, t.buyer);
    assert_eq!(receipt.seller, t.seller);
    assert_eq!(receipt.order_id, t.order_id());
    assert_eq!(receipt.status, EscrowStatus::Funded);
}

/// Success path: receipt reflects Released status after release.
#[test]
fn test_get_receipt_released_state() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let eid = deposit_escrow(&t, 1000, 100);
    client.release(&eid, &t.buyer, &t.seller);

    let receipt = client.get_receipt(&eid);
    assert_eq!(receipt.escrow_id, eid);
    assert_eq!(receipt.status, EscrowStatus::Released);
    // Buyer and seller are unchanged after release
    assert_eq!(receipt.buyer, t.buyer);
    assert_eq!(receipt.seller, t.seller);
    assert_eq!(receipt.order_id, t.order_id());
}

/// Success path: receipt reflects Refunded status after refund.
#[test]
fn test_get_receipt_refunded_state() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let eid = deposit_escrow(&t, 1000, 100);
    client.refund(&eid, &t.seller);

    let receipt = client.get_receipt(&eid);
    assert_eq!(receipt.escrow_id, eid);
    assert_eq!(receipt.status, EscrowStatus::Refunded);
    assert_eq!(receipt.buyer, t.buyer);
    assert_eq!(receipt.seller, t.seller);
    assert_eq!(receipt.order_id, t.order_id());
}

/// Success path: receipt reflects Disputed status mid-lifecycle.
#[test]
fn test_get_receipt_disputed_state() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let eid = deposit_escrow(&t, 1000, 100);
    client.dispute(&eid, &t.buyer);

    let receipt = client.get_receipt(&eid);
    assert_eq!(receipt.escrow_id, eid);
    assert_eq!(receipt.status, EscrowStatus::Disputed);
    assert_eq!(receipt.buyer, t.buyer);
    assert_eq!(receipt.seller, t.seller);
    assert_eq!(receipt.order_id, t.order_id());
}

/// Failure path: NotFound error for a non-existent escrow id.
#[test]
fn test_get_receipt_not_found() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let result = client.try_get_receipt(&999u64);
    assert_eq!(result, Err(Ok(EscrowError::NotFound)));
}

// ── MerchantEscrowReceipt getter tests (get_merchant_receipt, issue #171) ─

/// Success path: funded escrow is release-eligible before timeout.
#[test]
fn test_get_merchant_receipt_funded_state() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let eid = deposit_escrow(&t, 1000, 100);
    let receipt = client.get_merchant_receipt(&eid);

    assert_eq!(receipt.escrow_id, t.order_id());
    assert_eq!(receipt.merchant, t.seller);
    assert_eq!(receipt.buyer, t.buyer);
    assert_eq!(receipt.status, EscrowStatus::Funded);
    assert!(receipt.release_eligible);
}

/// Success path: disputed escrow is not release-eligible.
#[test]
fn test_get_merchant_receipt_disputed_state() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let eid = deposit_escrow(&t, 1000, 100);
    client.dispute(&eid, &t.buyer);

    let receipt = client.get_merchant_receipt(&eid);
    assert_eq!(receipt.escrow_id, t.order_id());
    assert_eq!(receipt.merchant, t.seller);
    assert_eq!(receipt.buyer, t.buyer);
    assert_eq!(receipt.status, EscrowStatus::Disputed);
    assert!(!receipt.release_eligible);
}

/// Success path: released escrow is not release-eligible.
#[test]
fn test_get_merchant_receipt_released_state() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let eid = deposit_escrow(&t, 1000, 100);
    client.release(&eid, &t.buyer, &t.seller);

    let receipt = client.get_merchant_receipt(&eid);
    assert_eq!(receipt.status, EscrowStatus::Released);
    assert!(!receipt.release_eligible);
    assert_eq!(receipt.merchant, t.seller);
    assert_eq!(receipt.buyer, t.buyer);
    assert_eq!(receipt.escrow_id, t.order_id());
}

/// Success path: refunded escrow is not release-eligible.
#[test]
fn test_get_merchant_receipt_refunded_state() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let eid = deposit_escrow(&t, 1000, 100);
    client.refund(&eid, &t.seller);

    let receipt = client.get_merchant_receipt(&eid);
    assert_eq!(receipt.status, EscrowStatus::Refunded);
    assert!(!receipt.release_eligible);
    assert_eq!(receipt.merchant, t.seller);
    assert_eq!(receipt.buyer, t.buyer);
    assert_eq!(receipt.escrow_id, t.order_id());
}

/// Failure path: NotFound for a non-existent escrow id.
#[test]
fn test_get_merchant_receipt_not_found() {
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let result = client.try_get_merchant_receipt(&999u64);
    assert_eq!(result, Err(Ok(EscrowError::NotFound)));
}

#[test]
fn test_version_callable_without_auth() {
    let env = Env::default();
    // Intentionally do NOT mock all auths — version() requires no auth.
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let config = EscrowConfig {
        admin,
        fee_bps: 0u32,
        treasury,
        min_amount: 100i128,
        max_amount: 10000i128,
    };
    let contract_id = env.register(EscrowContract, (config,));
    let client = EscrowContractClient::new(&env, &contract_id);

    let version = client.version();
    assert_eq!(version.name, symbol_short!("escrow"));
    assert_eq!(version.semver, symbol_short!("0_2_0"));
}

// ── Fee distribution validation (treasury addresses and shares) ──────────

#[test]
fn test_set_fee_distribution_accepts_valid_multi_treasury_config() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let mut config = Vec::new(&t.env);
    config.push_back(TreasuryShare {
        treasury: Address::generate(&t.env),
        bps: 400,
    });
    config.push_back(TreasuryShare {
        treasury: Address::generate(&t.env),
        bps: 600,
    });

    let _ = escrow_client.set_fee_distribution(&t.admin, &config);
}

#[test]
fn test_set_fee_distribution_rejects_zero_address_treasury() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let zero_address = soroban_sdk::Address::from_str(
        &t.env,
        "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
    );
    let mut config = Vec::new(&t.env);
    config.push_back(TreasuryShare {
        treasury: zero_address,
        bps: 10000,
    });

    assert!(matches!(
        escrow_client.try_set_fee_distribution(&t.admin, &config),
        Err(Ok(_))
    ));
}

#[test]
fn test_set_fee_distribution_rejects_zero_bps_share() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let mut config = Vec::new(&t.env);
    config.push_back(TreasuryShare {
        treasury: Address::generate(&t.env),
        bps: 0,
    });
    config.push_back(TreasuryShare {
        treasury: Address::generate(&t.env),
        bps: 10000,
    });

    assert!(matches!(
        escrow_client.try_set_fee_distribution(&t.admin, &config),
        Err(Ok(_))
    ));
}

#[test]
fn test_set_fee_distribution_rejects_too_many_treasuries() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let max_treasuries = MAX_TREASURIES;
    let mut config = Vec::new(&t.env);
    for _ in 0..max_treasuries {
        config.push_back(TreasuryShare {
            treasury: Address::generate(&t.env),
            bps: 1,
        });
    }
    config.push_back(TreasuryShare {
        treasury: Address::generate(&t.env),
        bps: 10000 - max_treasuries,
    });

    assert!(matches!(
        escrow_client.try_set_fee_distribution(&t.admin, &config),
        Err(Ok(_))
    ));
}

// ── Partial release tests ──────────────────────────────────────────────────

#[test]
fn test_partial_release_50_percent_stays_active() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let amount = 1000i128;
    let escrow_id = deposit_escrow(&t, amount, 100);

    let result = escrow_client.partial_release(&escrow_id, &t.buyer, &500);
    assert_eq!(result.released, 500);
    assert_eq!(result.remaining, 500);
    assert!(!result.fully_released);

    assert_eq!(token_client.balance(&t.seller), 500);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 500);

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.released_amount, 500);
    assert_eq!(record.status, EscrowStatus::Funded);
}

#[test]
fn test_partial_release_remaining_50_percent_released() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let amount = 1000i128;
    let escrow_id = deposit_escrow(&t, amount, 100);

    escrow_client.partial_release(&escrow_id, &t.buyer, &500);
    let result = escrow_client.partial_release(&escrow_id, &t.buyer, &500);

    assert_eq!(result.released, 500);
    assert_eq!(result.remaining, 0);
    assert!(result.fully_released);

    assert_eq!(token_client.balance(&t.seller), 1000);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 0);

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.released_amount, 1000);
    assert_eq!(record.status, EscrowStatus::Released);
}

#[test]
fn test_partial_release_exceeds_remaining_balance() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    escrow_client.partial_release(&escrow_id, &t.buyer, &500);

    assert_eq!(
        escrow_client.try_partial_release(&escrow_id, &t.buyer, &501),
        Err(Ok(EscrowError::InsufficientEscrowBalance))
    );
}

#[test]
fn test_partial_release_zero_amount() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    assert_eq!(
        escrow_client.try_partial_release(&escrow_id, &t.buyer, &0),
        Err(Ok(EscrowError::ZeroAmount))
    );
}

#[test]
fn test_full_release_via_release_still_works() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let amount = 1000i128;
    let escrow_id = deposit_escrow(&t, amount, 100);

    assert!(escrow_client.release(&escrow_id, &t.buyer, &t.seller));

    assert_eq!(token_client.balance(&t.seller), 1000);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 0);

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.released_amount, 1000);
    assert_eq!(record.status, EscrowStatus::Released);
}

#[test]
fn test_refund_after_partial_release_refunds_unreleased_only() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let amount = 1000i128;
    let escrow_id = deposit_escrow(&t, amount, 100);

    escrow_client.partial_release(&escrow_id, &t.buyer, &300);
    assert!(escrow_client.refund(&escrow_id, &t.seller));

    assert_eq!(token_client.balance(&t.seller), 300);
    assert_eq!(token_client.balance(&t.buyer), 9700);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 0);

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.status, EscrowStatus::Refunded);
}

// ── Issue #337: Partial refund tests ─────────────────────────────────────

#[test]
fn test_partial_refund_returns_specified_amount() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let amount = 1000i128;
    let escrow_id = deposit_escrow(&t, amount, 100);

    let result = escrow_client.partial_refund(&escrow_id, &t.seller, &400);
    assert_eq!(result.refunded, 400);
    assert_eq!(result.remaining, 600);
    assert!(!result.fully_refunded);

    assert_eq!(token_client.balance(&t.buyer), 9400);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 600);
}

#[test]
fn test_partial_refund_status_unchanged_if_not_fully_refunded() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let amount = 1000i128;
    let escrow_id = deposit_escrow(&t, amount, 100);

    escrow_client.partial_refund(&escrow_id, &t.seller, &400);

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.status, EscrowStatus::Funded);
    assert_eq!(record.refunded_amount, 400);
}

#[test]
fn test_partial_refund_full_remaining_sets_refunded_status() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let amount = 1000i128;
    let escrow_id = deposit_escrow(&t, amount, 100);

    escrow_client.partial_refund(&escrow_id, &t.seller, &400);
    let result = escrow_client.partial_refund(&escrow_id, &t.seller, &600);

    assert_eq!(result.refunded, 600);
    assert_eq!(result.remaining, 0);
    assert!(result.fully_refunded);

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.status, EscrowStatus::Refunded);
    assert_eq!(record.refunded_amount, 1000);
    assert_eq!(token_client.balance(&t.buyer), 10000);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 0);
}

#[test]
fn test_partial_refund_exceeds_remaining_balance_fails() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    escrow_client.partial_refund(&escrow_id, &t.seller, &400);

    assert_eq!(
        escrow_client.try_partial_refund(&escrow_id, &t.seller, &601),
        Err(Ok(EscrowError::InsufficientEscrowBalance))
    );
}

#[test]
fn test_partial_refund_zero_amount_fails() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    assert_eq!(
        escrow_client.try_partial_refund(&escrow_id, &t.seller, &0),
        Err(Ok(EscrowError::ZeroAmount))
    );
}

#[test]
fn test_partial_refund_buyer_before_timeout_fails() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    assert_eq!(
        escrow_client.try_partial_refund(&escrow_id, &t.buyer, &400),
        Err(Ok(EscrowError::TimeoutNotReached))
    );
}

#[test]
fn test_partial_refund_alongside_partial_release() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let amount = 1000i128;
    let escrow_id = deposit_escrow(&t, amount, 100);

    escrow_client.partial_release(&escrow_id, &t.buyer, &300);
    let result = escrow_client.partial_refund(&escrow_id, &t.seller, &700);

    assert_eq!(result.remaining, 0);
    assert!(result.fully_refunded);

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.status, EscrowStatus::Refunded);
    assert_eq!(token_client.balance(&t.seller), 300);
    assert_eq!(token_client.balance(&t.buyer), 9700);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 0);
}

#[test]
fn test_full_refund_via_refund_still_works() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let amount = 1000i128;
    let escrow_id = deposit_escrow(&t, amount, 100);

    assert!(escrow_client.refund(&escrow_id, &t.seller));

    assert_eq!(token_client.balance(&t.buyer), 10000);
    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.status, EscrowStatus::Refunded);
    assert_eq!(record.refunded_amount, 1000);
}

// ── Issue #339: Oracle conditional release tests ─────────────────────────

mod true_oracle {
    use soroban_sdk::{contract, contractimpl, Env, Symbol};

    #[contract]
    pub struct TrueOracle;

    #[contractimpl]
    impl TrueOracle {
        pub fn resolve(_env: Env, _condition_type: Symbol) -> bool {
            true
        }
    }
}

mod false_oracle {
    use soroban_sdk::{contract, contractimpl, Env, Symbol};

    #[contract]
    pub struct FalseOracle;

    #[contractimpl]
    impl FalseOracle {
        pub fn resolve(_env: Env, _condition_type: Symbol) -> bool {
            false
        }
    }
}

mod failing_oracle {
    use soroban_sdk::{contract, contractimpl, Env, Symbol};

    #[contract]
    pub struct FailingOracle;

    #[contractimpl]
    impl FailingOracle {
        pub fn resolve(_env: Env, _condition_type: Symbol) -> bool {
            panic!("oracle unavailable");
        }
    }
}

use failing_oracle::FailingOracle;
use false_oracle::FalseOracle;
use true_oracle::TrueOracle;

#[test]
fn test_set_release_condition_and_get() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let oracle_id = t.env.register(TrueOracle, ());

    let escrow_id = deposit_escrow(&t, 1000, 100);
    let condition_type = symbol_short!("shipped");

    assert!(escrow_client.set_release_condition(
        &t.seller,
        &escrow_id,
        &condition_type,
        &oracle_id
    ));

    let condition = escrow_client.get_release_condition(&escrow_id);
    assert_eq!(condition.condition_type, condition_type);
    assert_eq!(condition.oracle_contract, oracle_id);
}

#[test]
fn test_evaluate_and_release_when_oracle_returns_true() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);
    let oracle_id = t.env.register(TrueOracle, ());

    let amount = 1000i128;
    let escrow_id = deposit_escrow(&t, amount, 100);
    let condition_type = symbol_short!("shipped");
    escrow_client.set_release_condition(&t.seller, &escrow_id, &condition_type, &oracle_id);

    let result = escrow_client.evaluate_and_release(&escrow_id, &t.agent);
    assert_eq!(result.released, amount);
    assert!(result.fully_released);

    assert_eq!(token_client.balance(&t.seller), amount);
    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.status, EscrowStatus::Released);
}

#[test]
fn test_evaluate_and_release_blocked_when_oracle_returns_false() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let oracle_id = t.env.register(FalseOracle, ());

    let escrow_id = deposit_escrow(&t, 1000, 100);
    let condition_type = symbol_short!("shipped");
    escrow_client.set_release_condition(&t.seller, &escrow_id, &condition_type, &oracle_id);

    assert_eq!(
        escrow_client.try_evaluate_and_release(&escrow_id, &t.agent),
        Err(Ok(EscrowError::ConditionNotMet))
    );

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.status, EscrowStatus::Funded);
}

#[test]
fn test_evaluate_and_release_blocked_when_oracle_fails() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let oracle_id = t.env.register(FailingOracle, ());

    let escrow_id = deposit_escrow(&t, 1000, 100);
    let condition_type = symbol_short!("shipped");
    escrow_client.set_release_condition(&t.seller, &escrow_id, &condition_type, &oracle_id);

    assert_eq!(
        escrow_client.try_evaluate_and_release(&escrow_id, &t.agent),
        Err(Ok(EscrowError::OracleCallFailed))
    );

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.status, EscrowStatus::Funded);
}

#[test]
fn test_evaluate_and_release_without_condition_fails() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    assert_eq!(
        escrow_client.try_evaluate_and_release(&escrow_id, &t.agent),
        Err(Ok(EscrowError::ReleaseConditionNotSet))
    );
}

// ── Issue #37: checked_add for timeout_ledger computation ───────────────

#[test]
fn test_deposit_timeout_at_u32_max_boundary_succeeds() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    // A small, bounded sequence keeps the contract instance live (a large jump
    // would archive it in the test host).
    let sequence = 100u32;
    t.env.ledger().set_sequence_number(sequence);

    // Exact boundary: sequence + timeout_ledgers == u32::MAX, must not panic.
    let timeout_ledgers = u32::MAX - sequence;
    let escrow_id = escrow_client.deposit(
        &t.buyer,
        &t.seller,
        &t.token_contract_id,
        &1000,
        &t.order_id(),
        &timeout_ledgers,
        &None,
        &None,
    );

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.timeout_ledger, u32::MAX);
}

#[test]
fn test_deposit_timeout_past_u32_max_returns_typed_error() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let sequence = 100u32;
    t.env.ledger().set_sequence_number(sequence);

    // One past the boundary: checked_add must return InvalidExtension, not panic.
    let timeout_ledgers = u32::MAX - sequence + 1;
    assert_eq!(
        escrow_client.try_deposit(
            &t.buyer,
            &t.seller,
            &t.token_contract_id,
            &1000,
            &t.order_id(),
            &timeout_ledgers,
            &None,
            &None,
        ),
        Err(Ok(EscrowError::InvalidExtension))
    );
}

// ── Issue #88: EscrowTimeoutView getter tests ────────────────────────────

#[test]
fn test_get_timeout_view_not_found() {
    // Returns EscrowError::NotFound for an unknown escrow id.
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let res = client.try_get_timeout_view(&999u64);
    assert_eq!(res, Err(Ok(EscrowError::NotFound)));
}

#[test]
fn test_get_timeout_view_active_before_timeout() {
    // Funded escrow before timeout: refundable must be false.
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    let record = client.get_escrow(&escrow_id);

    // Current ledger is before timeout_ledger at deposit time.
    assert!(t.env.ledger().sequence() < record.timeout_ledger);

    let view = client.get_timeout_view(&escrow_id);

    assert_eq!(view.escrow_id, t.order_id());
    assert_eq!(view.timeout_ledger, record.timeout_ledger);
    assert_eq!(view.current_ledger, t.env.ledger().sequence());
    assert!(!view.refundable);
}

#[test]
fn test_get_timeout_view_active_at_timeout() {
    // Funded escrow exactly at timeout: refundable must be true.
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    let record = client.get_escrow(&escrow_id);

    t.env.ledger().set_sequence_number(record.timeout_ledger);

    let view = client.get_timeout_view(&escrow_id);

    assert_eq!(view.timeout_ledger, record.timeout_ledger);
    assert_eq!(view.current_ledger, record.timeout_ledger);
    assert!(view.refundable);
}

#[test]
fn test_get_timeout_view_active_past_timeout() {
    // Funded escrow well past timeout: refundable must be true.
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    let record = client.get_escrow(&escrow_id);

    t.env
        .ledger()
        .set_sequence_number(record.timeout_ledger + 500);

    let view = client.get_timeout_view(&escrow_id);

    assert_eq!(view.timeout_ledger, record.timeout_ledger);
    assert!(view.current_ledger > view.timeout_ledger);
    assert!(view.refundable);
}

#[test]
fn test_get_timeout_view_released_state() {
    // Released escrow: refundable must be false regardless of ledger.
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    client.release(&escrow_id, &t.buyer, &t.seller);

    // Advance past timeout to ensure the only reason for false is the terminal state.
    let record = client.get_escrow(&escrow_id);
    t.env
        .ledger()
        .set_sequence_number(record.timeout_ledger + 10);

    let view = client.get_timeout_view(&escrow_id);

    assert_eq!(view.escrow_id, t.order_id());
    assert!(!view.refundable);
}

#[test]
fn test_get_timeout_view_refunded_state() {
    // Refunded escrow: refundable must be false.
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    client.refund(&escrow_id, &t.seller);

    let record = client.get_escrow(&escrow_id);
    t.env
        .ledger()
        .set_sequence_number(record.timeout_ledger + 10);

    let view = client.get_timeout_view(&escrow_id);

    assert_eq!(view.escrow_id, t.order_id());
    assert!(!view.refundable);
}

#[test]
fn test_get_timeout_view_disputed_state() {
    // Disputed escrow: refundable must be false even after timeout.
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    client.dispute(&escrow_id, &t.buyer);

    let record = client.get_escrow(&escrow_id);
    t.env
        .ledger()
        .set_sequence_number(record.timeout_ledger + 10);

    let view = client.get_timeout_view(&escrow_id);

    assert_eq!(view.escrow_id, t.order_id());
    assert!(!view.refundable);
}

#[test]
fn test_get_timeout_view_does_not_mutate_state() {
    // Calling the getter must not change the stored escrow record.
    let t = TestEnv::setup();
    let client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    let before = client.get_escrow(&escrow_id);

    // Call past timeout — a mutating refund would change the status.
    t.env
        .ledger()
        .set_sequence_number(before.timeout_ledger + 5);
    let _view = client.get_timeout_view(&escrow_id);

    let after = client.get_escrow(&escrow_id);

    assert_eq!(before.status, after.status);
    assert_eq!(before.amount, after.amount);
    assert_eq!(before.timeout_ledger, after.timeout_ledger);
}

// ── Merchant Escrow Cancellation Integration Test ─────────────────────────

#[test]
fn test_cancellation_full_lifecycle() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let reason = symbol_short!("out_stock");

    // Merchant creates escrow without funding
    let escrow_id = escrow_client.create(
        &t.buyer,
        &t.seller,
        &t.token_contract_id,
        &1000i128,
        &t.order_id(),
        &100u32,
        &None,
        &None,
    );

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.status, EscrowStatus::Created);

    // Balances remain untouched
    assert_eq!(token_client.balance(&t.buyer), 10000);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 0);

    // Merchant cancels escrow
    assert!(escrow_client.cancel(&escrow_id, &t.seller, &reason));

    let record_after = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record_after.status, EscrowStatus::Cancelled);

    // Attempting to fund cancelled escrow fails
    assert_eq!(
        escrow_client.try_fund(&escrow_id, &t.buyer),
        Err(Ok(EscrowError::AlreadyCancelled))
    );

    // Attempting to release cancelled escrow fails
    assert_eq!(
        escrow_client.try_release(&escrow_id, &t.buyer, &t.seller),
        Err(Ok(EscrowError::AlreadyCancelled))
    );
}

// ── Issue #333: Escrow Timeout Extension via Quorum Vote ──────────────────

fn setup_quorum(t: &TestEnv, threshold: u32) -> soroban_sdk::Vec<Address> {
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let mut arbiters = soroban_sdk::Vec::new(&t.env);
    arbiters.push_back(Address::generate(&t.env));
    arbiters.push_back(Address::generate(&t.env));
    arbiters.push_back(Address::generate(&t.env));
    escrow_client.set_quorum_config(&t.admin, &arbiters, &threshold);
    arbiters
}

#[test]
fn test_extend_timeout_via_quorum_reaches_threshold() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let arbiters = setup_quorum(&t, 2);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    let before = escrow_client.get_escrow(&escrow_id);

    // First vote: quorum not yet reached, timeout unchanged.
    let applied =
        escrow_client.extend_timeout_via_quorum(&escrow_id, &arbiters.get(0).unwrap(), &50u32);
    assert!(!applied);
    let mid = escrow_client.get_escrow(&escrow_id);
    assert_eq!(mid.timeout_ledger, before.timeout_ledger);

    // Second matching vote reaches the threshold and extends the timeout.
    let applied =
        escrow_client.extend_timeout_via_quorum(&escrow_id, &arbiters.get(1).unwrap(), &50u32);
    assert!(applied);
    let after = escrow_client.get_escrow(&escrow_id);
    assert_eq!(after.timeout_ledger, before.timeout_ledger + 50);

    // Vote log is cleared after a successful extension.
    let votes = escrow_client.get_timeout_extension_votes(&escrow_id);
    assert_eq!(votes.len(), 0);
}

#[test]
fn test_extend_timeout_via_quorum_fails_without_sufficient_votes() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let arbiters = setup_quorum(&t, 3);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    let before = escrow_client.get_escrow(&escrow_id);

    let applied =
        escrow_client.extend_timeout_via_quorum(&escrow_id, &arbiters.get(0).unwrap(), &50u32);
    assert!(!applied);

    let after = escrow_client.get_escrow(&escrow_id);
    assert_eq!(after.timeout_ledger, before.timeout_ledger);

    let votes = escrow_client.get_timeout_extension_votes(&escrow_id);
    assert_eq!(votes.len(), 1);
}

#[test]
fn test_extend_timeout_via_quorum_rejects_non_arbiter() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    setup_quorum(&t, 2);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    let stranger = Address::generate(&t.env);

    assert_eq!(
        escrow_client.try_extend_timeout_via_quorum(&escrow_id, &stranger, &50u32),
        Err(Ok(EscrowError::NotAnArbiter))
    );
}

#[test]
fn test_extend_timeout_via_quorum_rejects_duplicate_vote() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let arbiters = setup_quorum(&t, 3);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    let applied =
        escrow_client.extend_timeout_via_quorum(&escrow_id, &arbiters.get(0).unwrap(), &50u32);
    assert!(!applied);

    assert_eq!(
        escrow_client.try_extend_timeout_via_quorum(&escrow_id, &arbiters.get(0).unwrap(), &50u32),
        Err(Ok(EscrowError::AlreadyVoted))
    );
}

#[test]
fn test_extend_timeout_via_quorum_rejects_zero_extension() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let arbiters = setup_quorum(&t, 2);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    assert_eq!(
        escrow_client.try_extend_timeout_via_quorum(&escrow_id, &arbiters.get(0).unwrap(), &0u32),
        Err(Ok(EscrowError::InvalidExtension))
    );
}

// ── Issue #36: extend_timeout typed InvalidExtension error ────────────────

/// Extending the timeout to the same ledger as the current one is rejected
/// with the typed `InvalidExtension` error.
#[test]
fn test_extend_timeout_rejects_equal_ledger() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    let record = escrow_client.get_escrow(&escrow_id);

    assert_eq!(
        escrow_client.try_extend_timeout(&escrow_id, &t.buyer, &record.timeout_ledger),
        Err(Ok(EscrowError::InvalidExtension))
    );
}

/// Extending the timeout to a ledger earlier than the current one is
/// rejected with the typed `InvalidExtension` error.
#[test]
fn test_extend_timeout_rejects_lower_ledger() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    let record = escrow_client.get_escrow(&escrow_id);
    let lower = record.timeout_ledger - 5;

    assert_eq!(
        escrow_client.try_extend_timeout(&escrow_id, &t.buyer, &lower),
        Err(Ok(EscrowError::InvalidExtension))
    );
}

/// Extending the timeout to a strictly later ledger succeeds and updates the
/// record.
#[test]
fn test_extend_timeout_accepts_higher_ledger() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    let record = escrow_client.get_escrow(&escrow_id);
    let higher = record.timeout_ledger + 50;

    let res = escrow_client.try_extend_timeout(&escrow_id, &t.buyer, &higher);
    assert_eq!(res, Ok(Ok(true)));

    let after = escrow_client.get_escrow(&escrow_id);
    assert_eq!(after.timeout_ledger, higher);
}

// ── Issue #335: Escrow Liquidity Pool for Instant Settlement ──────────────

#[test]
fn test_fund_pool_increases_balance() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let funder = Address::generate(&t.env);
    let token_admin_client =
        soroban_sdk::token::StellarAssetClient::new(&t.env, &t.token_contract_id);
    token_admin_client.mint(&funder, &5000);

    let new_balance = escrow_client.fund_pool(&funder, &t.token_contract_id, &2000);
    assert_eq!(new_balance, 2000);

    let pool = escrow_client.get_liquidity_pool(&t.token_contract_id);
    assert_eq!(pool.balance, 2000);
    assert_eq!(pool.token, t.token_contract_id);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 2000);

    let newer_balance = escrow_client.fund_pool(&funder, &t.token_contract_id, &500);
    assert_eq!(newer_balance, 2500);
    assert_eq!(
        escrow_client
            .get_liquidity_pool(&t.token_contract_id)
            .balance,
        2500
    );
}

#[test]
fn test_fund_pool_rejects_non_whitelisted_token() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let other_token_admin = Address::generate(&t.env);
    let other_token = t
        .env
        .register_stellar_asset_contract_v2(other_token_admin.clone())
        .address();

    assert_eq!(
        escrow_client.try_fund_pool(&t.buyer, &other_token, &1000),
        Err(Ok(EscrowError::TokenNotWhitelisted))
    );
}

#[test]
fn test_settle_from_pool_transfers_to_seller() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let funder = Address::generate(&t.env);
    let token_admin_client =
        soroban_sdk::token::StellarAssetClient::new(&t.env, &t.token_contract_id);
    token_admin_client.mint(&funder, &5000);
    escrow_client.fund_pool(&funder, &t.token_contract_id, &5000);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    assert_eq!(token_client.balance(&t.seller), 0);

    assert!(escrow_client.settle_from_pool(&escrow_id, &t.admin));

    assert_eq!(token_client.balance(&t.seller), 1000);

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.status, EscrowStatus::Released);
    assert_eq!(record.released_amount, 1000);

    // Pool balance is debited by the settled amount, mirroring the real
    // token movement out of the reserve.
    let pool = escrow_client.get_liquidity_pool(&t.token_contract_id);
    assert_eq!(pool.balance, 4000);
}

#[test]
fn test_settle_from_pool_decrements_balance_and_blocks_overcommit() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let funder = Address::generate(&t.env);
    let token_admin_client =
        soroban_sdk::token::StellarAssetClient::new(&t.env, &t.token_contract_id);
    token_admin_client.mint(&funder, &5000);
    escrow_client.fund_pool(&funder, &t.token_contract_id, &5000);

    let escrow_id = deposit_escrow(&t, 1000, 100);
    assert!(escrow_client.settle_from_pool(&escrow_id, &t.admin));

    // Balance drops by exactly the settled amount.
    assert_eq!(
        escrow_client
            .get_liquidity_pool(&t.token_contract_id)
            .balance,
        4000
    );

    // The remaining balance can still be withdrawn, but over-committing
    // beyond it is rejected.
    assert_eq!(
        escrow_client.try_withdraw_from_pool(&t.admin, &t.token_contract_id, &4001),
        Err(Ok(EscrowError::InsufficientPoolBalance))
    );

    let remaining_balance = escrow_client.withdraw_from_pool(&t.admin, &t.token_contract_id, &4000);
    assert_eq!(remaining_balance, 0);
}

#[test]
fn test_settle_from_pool_rejects_non_admin() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let funder = Address::generate(&t.env);
    let token_admin_client =
        soroban_sdk::token::StellarAssetClient::new(&t.env, &t.token_contract_id);
    token_admin_client.mint(&funder, &5000);
    escrow_client.fund_pool(&funder, &t.token_contract_id, &5000);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    assert_eq!(
        escrow_client.try_settle_from_pool(&escrow_id, &t.agent),
        Err(Ok(EscrowError::Unauthorized))
    );
}

#[test]
fn test_settle_from_pool_insufficient_liquidity_fails() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let funder = Address::generate(&t.env);
    let token_admin_client =
        soroban_sdk::token::StellarAssetClient::new(&t.env, &t.token_contract_id);
    token_admin_client.mint(&funder, &500);
    escrow_client.fund_pool(&funder, &t.token_contract_id, &500);

    let escrow_id = deposit_escrow(&t, 1000, 100);

    assert_eq!(
        escrow_client.try_settle_from_pool(&escrow_id, &t.admin),
        Err(Ok(EscrowError::InsufficientPoolBalance))
    );
}

#[test]
fn test_withdraw_from_pool_respects_available_balance() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let funder = Address::generate(&t.env);
    let token_admin_client =
        soroban_sdk::token::StellarAssetClient::new(&t.env, &t.token_contract_id);
    token_admin_client.mint(&funder, &1000);
    escrow_client.fund_pool(&funder, &t.token_contract_id, &1000);

    // Withdrawing more than the pool holds fails.
    assert_eq!(
        escrow_client.try_withdraw_from_pool(&t.admin, &t.token_contract_id, &1500),
        Err(Ok(EscrowError::InsufficientPoolBalance))
    );

    // Withdrawing within the available balance succeeds and decrements it.
    let new_balance = escrow_client.withdraw_from_pool(&t.admin, &t.token_contract_id, &400);
    assert_eq!(new_balance, 600);
    assert_eq!(
        escrow_client
            .get_liquidity_pool(&t.token_contract_id)
            .balance,
        600
    );
    assert_eq!(token_client.balance(&t.admin), 400);

    // A further withdrawal beyond the now-reduced balance fails.
    assert_eq!(
        escrow_client.try_withdraw_from_pool(&t.admin, &t.token_contract_id, &601),
        Err(Ok(EscrowError::InsufficientPoolBalance))
    );
}

#[test]
fn test_withdraw_from_pool_rejects_non_admin() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let funder = Address::generate(&t.env);
    let token_admin_client =
        soroban_sdk::token::StellarAssetClient::new(&t.env, &t.token_contract_id);
    token_admin_client.mint(&funder, &1000);
    escrow_client.fund_pool(&funder, &t.token_contract_id, &1000);

    assert_eq!(
        escrow_client.try_withdraw_from_pool(&t.agent, &t.token_contract_id, &100),
        Err(Ok(EscrowError::Unauthorized))
    );
}

#[test]
fn test_get_liquidity_pool_defaults_to_zero_for_unfunded_token() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let pool = escrow_client.get_liquidity_pool(&t.token_contract_id);
    assert_eq!(pool.balance, 0);
    assert_eq!(pool.token, t.token_contract_id);
}

// --- batch_deposit / batch_release / batch_refund (issue #317) ---

fn order_id_n(env: &Env, n: u8) -> BytesN<32> {
    BytesN::from_array(env, &[n; 32])
}

#[test]
fn test_batch_deposit_three_orders_all_succeed() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let mut orders = Vec::new(&t.env);
    for n in 1..=3u8 {
        orders.push_back(BatchDepositParams {
            seller: t.seller.clone(),
            token: t.token_contract_id.clone(),
            amount: 500,
            order_id: order_id_n(&t.env, n),
            timeout_ledgers: 100,
            order_hash: None,
            schema: None,
        });
    }

    let escrow_ids = escrow_client.batch_deposit(&t.buyer, &orders);
    assert_eq!(escrow_ids.len(), 3);

    for escrow_id in escrow_ids.iter() {
        let record = escrow_client.get_escrow(&escrow_id);
        assert_eq!(record.status, EscrowStatus::Funded);
        assert_eq!(record.amount, 500);
    }
}

#[test]
fn test_batch_deposit_with_one_invalid_order_reverts_entire_batch() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let mut orders = Vec::new(&t.env);
    orders.push_back(BatchDepositParams {
        seller: t.seller.clone(),
        token: t.token_contract_id.clone(),
        amount: 500,
        order_id: order_id_n(&t.env, 1),
        timeout_ledgers: 100,
        order_hash: None,
        schema: None,
    });
    // Second order amount is below the configured minimum (100) — invalid.
    orders.push_back(BatchDepositParams {
        seller: t.seller.clone(),
        token: t.token_contract_id.clone(),
        amount: 1,
        order_id: order_id_n(&t.env, 2),
        timeout_ledgers: 100,
        order_hash: None,
        schema: None,
    });
    orders.push_back(BatchDepositParams {
        seller: t.seller.clone(),
        token: t.token_contract_id.clone(),
        amount: 500,
        order_id: order_id_n(&t.env, 3),
        timeout_ledgers: 100,
        order_hash: None,
        schema: None,
    });

    assert_eq!(
        escrow_client.try_batch_deposit(&t.buyer, &orders),
        Err(Ok(EscrowError::AmountBelowMin))
    );

    // The whole batch reverted: the first (valid) order was not committed either.
    assert_eq!(
        escrow_client.try_get_receipt(&1u64),
        Err(Ok(EscrowError::NotFound))
    );
}

#[test]
fn test_batch_release_with_mixed_statuses() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let id_a = deposit_escrow(&t, 500, 100);
    let id_b = deposit_escrow_with_id(&t, 500, 100, 2);

    // Fully release id_a up front so it's already terminal before the batch.
    escrow_client.release(&id_a, &t.buyer, &t.seller);

    let mut releases = Vec::new(&t.env);
    releases.push_back(BatchReleaseParams {
        escrow_id: id_a,
        release_amount: 100,
    });
    releases.push_back(BatchReleaseParams {
        escrow_id: id_b,
        release_amount: 500,
    });

    // id_a is already released, so releasing again fails and the whole
    // batch (including id_b) reverts atomically.
    assert_eq!(
        escrow_client.try_batch_release(&t.buyer, &releases),
        Err(Ok(EscrowError::AlreadyReleased))
    );
    let record_b = escrow_client.get_escrow(&id_b);
    assert_eq!(record_b.status, EscrowStatus::Funded);

    // A batch touching only the still-funded escrow succeeds.
    let mut ok_releases = Vec::new(&t.env);
    ok_releases.push_back(BatchReleaseParams {
        escrow_id: id_b,
        release_amount: 500,
    });
    let results = escrow_client.batch_release(&t.buyer, &ok_releases);
    assert_eq!(results.len(), 1);
    assert!(results.get(0).unwrap().fully_released);

    let record_b_after = escrow_client.get_escrow(&id_b);
    assert_eq!(record_b_after.status, EscrowStatus::Released);
}

#[test]
fn test_batch_refund_three_orders_all_succeed() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    let id_a = deposit_escrow(&t, 500, 100);
    let id_b = deposit_escrow_with_id(&t, 500, 100, 2);

    let mut refunds = Vec::new(&t.env);
    refunds.push_back(BatchRefundParams {
        escrow_id: id_a,
        refund_amount: 500,
    });
    refunds.push_back(BatchRefundParams {
        escrow_id: id_b,
        refund_amount: 500,
    });

    // Seller (record.seller == t.seller) can refund at any time.
    let results = escrow_client.batch_refund(&t.seller, &refunds);
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.fully_refunded));

    assert_eq!(
        escrow_client.get_escrow(&id_a).status,
        EscrowStatus::Refunded
    );
    assert_eq!(
        escrow_client.get_escrow(&id_b).status,
        EscrowStatus::Refunded
    );
}

/// True if the escrow contract emitted an event with the given second topic
/// under the `admin` topic namespace. Events are read immediately after the
/// emitting call: the test host enables invocation metering, which clears the
/// events buffer at the start of each subsequent contract invocation.
fn admin_event_emitted(t: &TestEnv, topic: Symbol, contract_id: &Address) -> bool {
    for event in t.env.events().all().iter() {
        let (c_id, topics, _value) = event;
        if c_id != *contract_id || topics.len() != 2 {
            continue;
        }
        let t0: Symbol = topics.get(0).unwrap().try_into_val(&t.env).unwrap();
        let t1: Symbol = topics.get(1).unwrap().try_into_val(&t.env).unwrap();
        if t0 == symbol_short!("admin") && t1 == topic {
            return true;
        }
    }
    false
}

fn deposit_escrow_with_id(t: &TestEnv, amount: i128, timeout_ledgers: u32, id_seed: u8) -> u64 {
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    escrow_client.deposit(
        &t.buyer,
        &t.seller,
        &t.token_contract_id,
        &amount,
        &order_id_n(&t.env, id_seed),
        &timeout_ledgers,
        &None,
        &None,
    )
}

// ── Upgrade + two-step admin handover integration test ───────────────────

// A minimal WebAssembly module (with the standard contract metadata section)
// that serves only as the upgrade target so `update_current_contract_wasm`
// has a real contract-code ledger entry to point at. Its exported functions
// are never invoked — after the upgrade we read persistent state directly.
const WASM_STUB: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x60, 0x00, 0x01, 0x7e, 0x60,
    0x00, 0x00, 0x03, 0x03, 0x02, 0x00, 0x01, 0x05, 0x03, 0x01, 0x00, 0x10, 0x06, 0x09, 0x01, 0x7f,
    0x01, 0x41, 0x80, 0x80, 0xc0, 0x00, 0x0b, 0x07, 0x15, 0x03, 0x06, 0x6d, 0x65, 0x6d, 0x6f, 0x72,
    0x79, 0x02, 0x00, 0x04, 0x70, 0x69, 0x6e, 0x67, 0x00, 0x00, 0x01, 0x5f, 0x00, 0x01, 0x0a, 0x09,
    0x02, 0x04, 0x00, 0x42, 0x01, 0x0b, 0x02, 0x00, 0x0b, 0x00, 0x2b, 0x0e, 0x63, 0x6f, 0x6e, 0x74,
    0x72, 0x61, 0x63, 0x74, 0x73, 0x70, 0x65, 0x63, 0x76, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x70, 0x69, 0x6e, 0x67, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x1e, 0x11, 0x63, 0x6f, 0x6e, 0x74, 0x72, 0x61, 0x63,
    0x74, 0x65, 0x6e, 0x76, 0x6d, 0x65, 0x74, 0x61, 0x76, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x16, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6f, 0x0e, 0x63, 0x6f, 0x6e, 0x74, 0x72, 0x61, 0x63,
    0x74, 0x6d, 0x65, 0x74, 0x61, 0x76, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x72,
    0x73, 0x76, 0x65, 0x72, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x31, 0x2e, 0x39, 0x37, 0x2e,
    0x31, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x72, 0x73, 0x73, 0x64, 0x6b,
    0x76, 0x65, 0x72, 0x00, 0x00, 0x00, 0x30, 0x32, 0x32, 0x2e, 0x30, 0x2e, 0x31, 0x31, 0x23, 0x33,
    0x34, 0x66, 0x37, 0x66, 0x35, 0x33, 0x61, 0x65, 0x33, 0x31, 0x65, 0x30, 0x66, 0x64, 0x30, 0x32,
    0x61, 0x61, 0x62, 0x34, 0x33, 0x36, 0x61, 0x39, 0x38, 0x37, 0x32, 0x65, 0x37, 0x39, 0x66, 0x61,
    0x36, 0x37, 0x31, 0x63, 0x61, 0x30, 0x32,
];

#[test]
fn test_upgrade_with_admin_handover_preserves_state_and_emits_event() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let contract_id = t.escrow_contract_id.clone();

    // Create an escrow so there is persistent state whose survival we assert.
    let escrow_id = deposit_escrow(&t, 1000, 100);
    let record_before = escrow_client.get_escrow(&escrow_id);
    assert!(!escrow_client.is_migrated());

    // Two-step admin handover: the current admin proposes a successor, then
    // the successor accepts the primary-admin role. Each event is read
    // immediately after the call that emits it, because the test host only
    // retains the events of the most recent invocation.
    let new_admin = Address::generate(&t.env);
    assert!(escrow_client.propose_admin(&t.admin, &new_admin));
    assert!(
        admin_event_emitted(&t, symbol_short!("proposed"), &contract_id),
        "AdminProposedEvent was not emitted"
    );
    assert_eq!(escrow_client.get_pending_admin(), Some(new_admin.clone()));

    assert!(escrow_client.accept_admin(&new_admin));
    assert!(
        admin_event_emitted(&t, symbol_short!("accepted"), &contract_id),
        "AdminAcceptedEvent was not emitted"
    );
    assert_eq!(escrow_client.get_pending_admin(), None);

    // The fresh primary admin upgrades the contract to new wasm code.
    let wasm_hash = t.env.deployer().upload_contract_wasm(WASM_STUB);
    assert!(escrow_client.upgrade(&new_admin, &wasm_hash));

    // Assert the ContractUpgradedEvent immediately after the upgrade call,
    // before any further invocation clears the events buffer.
    let events = t.env.events().all();
    let mut upgraded_found = false;
    for event in events.iter() {
        let (c_id, topics, value) = event;
        if c_id != contract_id || topics.len() != 2 {
            continue;
        }
        let t0: Symbol = topics.get(0).unwrap().try_into_val(&t.env).unwrap();
        let t1: Symbol = topics.get(1).unwrap().try_into_val(&t.env).unwrap();
        if t0 == symbol_short!("escrow") && t1 == symbol_short!("upgraded") {
            let evt: crate::ContractUpgradedEvent = value.try_into_val(&t.env).unwrap();
            assert_eq!(evt.admin, new_admin);
            assert_eq!(evt.previous_semver, symbol_short!("0_2_0"));
            assert_eq!(evt.new_wasm_hash, wasm_hash);
            upgraded_found = true;
        }
    }
    assert!(upgraded_found, "ContractUpgradedEvent was not emitted");

    // After the upgrade the contract's executable points at the stub wasm,
    // which implements nothing, so re-read persistent state directly rather
    // than dispatching through the client.
    let migrated: bool = t.env.as_contract(&contract_id, || {
        t.env
            .storage()
            .instance()
            .get(&crate::DataKey::MigrationFlag)
            .unwrap_or(false)
    });
    assert!(migrated, "migration flag must be set after upgrade");

    let record_after: crate::EscrowRecord = t.env.as_contract(&contract_id, || {
        t.env
            .storage()
            .persistent()
            .get(&crate::DataKey::Escrow(escrow_id))
            .unwrap()
    });
    assert_eq!(
        record_before, record_after,
        "escrow record must survive the upgrade"
    );
}

#[test]
fn test_split_release_multi_treasury() {
    let t = TestEnv::setup_with_fee_bps(500);
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);
    // Setup multi-treasury
    let treasury1 = Address::generate(&t.env);
    let treasury2 = Address::generate(&t.env);
    let mut shares = soroban_sdk::Vec::new(&t.env);
    shares.push_back(crate::TreasuryShare {
        treasury: treasury1.clone(),
        bps: 200,
    }); // 2%
    shares.push_back(crate::TreasuryShare {
        treasury: treasury2.clone(),
        bps: 300,
    }); // 3%
    assert!(escrow_client.set_fee_distribution(&t.admin, &shares));
    let escrow_id = deposit_escrow(&t, 10000, 100);
    let recipient1 = Address::generate(&t.env);
    let recipient2 = Address::generate(&t.env);
    let mut release_shares = soroban_sdk::Vec::new(&t.env);
    release_shares.push_back((recipient1.clone(), 4000));
    release_shares.push_back((recipient2.clone(), 6000));
    // Release shares
    assert!(escrow_client.split_release(&escrow_id, &t.buyer, &release_shares));
    // Fees:
    // total base amount = 10000
    // share1 amount = 4000
    // fee1 = 4000 * 500 / 10000 = 200. Net = 3800.
    // share2 amount = 6000
    // fee2 = 6000 * 500 / 10000 = 300. Net = 5700.
    // Total fee = 500.
    // treasury1 = 500 * 200 / 500 = 200
    // treasury2 = 500 * 300 / 500 = 300
    assert_eq!(token_client.balance(&recipient1), 3800);
    assert_eq!(token_client.balance(&recipient2), 5700);
    assert_eq!(token_client.balance(&treasury1), 200);
    assert_eq!(token_client.balance(&treasury2), 300);
}
