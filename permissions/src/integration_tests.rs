#![cfg(test)]

use crate::{
    PermissionError, PermissionStatus, PermissionsContract, PermissionsContractClient,
    RelayedSpendMessage,
};
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    xdr::ToXdr,
    Address, BytesN, Env, Symbol, TryIntoVal, Vec,
};

/// Deterministic test keypair plus its raw ed25519 public key bytes.
fn test_keypair(env: &Env, seed: u8) -> (SigningKey, BytesN<32>) {
    let signing_key = SigningKey::from_bytes(&[seed; 32]);
    let public_key = BytesN::from_array(env, &signing_key.verifying_key().to_bytes());
    (signing_key, public_key)
}

/// Sign a `RelayedSpendMessage` with the given key, returning the raw
/// 64-byte ed25519 signature over the message's canonical XDR encoding —
/// the exact bytes `execute_spend_via_relayer` re-derives and verifies.
fn sign_relayed_spend(
    env: &Env,
    signing_key: &SigningKey,
    message: RelayedSpendMessage,
) -> BytesN<64> {
    let message_bytes = message.to_xdr(env);
    let len = message_bytes.len() as usize;
    let mut buf = [0u8; 512];
    message_bytes.copy_into_slice(&mut buf[..len]);
    let signature = signing_key.sign(&buf[..len]);
    BytesN::from_array(env, &signature.to_bytes())
}

struct TestEnv {
    env: Env,
    admin: Address,
    buyer: Address,
    seller: Address,
    agent: Address,
    _token_contract_id: Address,
    _token_admin: Address,
    _escrow_contract_id: Address,
    permissions_contract_id: Address,
}

impl TestEnv {
    fn setup() -> Self {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let agent = Address::generate(&env);

        let token_admin = Address::generate(&env);
        #[allow(deprecated)]
        let token_contract_id = env.register_stellar_asset_contract(token_admin.clone());
        let token_admin_client =
            soroban_sdk::token::StellarAssetClient::new(&env, &token_contract_id);
        token_admin_client.mint(&buyer, &10000);

        let escrow_contract_id = Address::generate(&env);
        let permissions_contract_id = env.register(PermissionsContract, ());

        TestEnv {
            env,
            admin,
            buyer,
            seller,
            agent,
            _token_contract_id: token_contract_id,
            _token_admin: token_admin,
            _escrow_contract_id: escrow_contract_id,
            permissions_contract_id,
        }
    }
}

#[test]
fn test_grant_and_spend() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    let limit_per_tx = 50i128;
    let limit_total = 100i128;
    let ttl_ledgers = 3600u32;
    let mut merchants = Vec::<soroban_sdk::Address>::new(&t.env);
    merchants.push_back(t.seller.clone());

    client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    assert_eq!(
        client.try_can_spend(&t.buyer, &t.agent, &40, &t.seller),
        Ok(Ok(()))
    );

    client.execute_spend(&t.buyer, &t.agent, &40, &t.seller);

    assert_eq!(
        client.try_can_spend(&t.buyer, &t.agent, &40, &t.seller),
        Ok(Ok(()))
    );
    client.execute_spend(&t.buyer, &t.agent, &40, &t.seller);

    // Only 20 of the 100 total allowance remains, so a 30 spend is over the total limit.
    assert_eq!(
        client.try_can_spend(&t.buyer, &t.agent, &30, &t.seller),
        Err(Ok(PermissionError::ExceedsTotalLimit))
    );
}

#[test]
fn test_spend_exceeds_per_tx_limit() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    let limit_per_tx = 50i128;
    let limit_total = 100i128;
    let ttl_ledgers = 3600u32;
    let merchants = Vec::<soroban_sdk::Address>::new(&t.env);

    client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    assert_eq!(
        client.try_execute_spend(&t.buyer, &t.agent, &60, &t.seller),
        Err(Ok(PermissionError::ExceedsPerTxLimit))
    );
}

#[test]
fn test_spend_exceeds_total_limit() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    let limit_per_tx = 50i128;
    let limit_total = 100i128;
    let ttl_ledgers = 3600u32;
    let merchants = Vec::<soroban_sdk::Address>::new(&t.env);

    client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    client.execute_spend(&t.buyer, &t.agent, &50, &t.seller);
    client.execute_spend(&t.buyer, &t.agent, &50, &t.seller);

    assert_eq!(
        client.try_execute_spend(&t.buyer, &t.agent, &1, &t.seller),
        Err(Ok(PermissionError::ExceedsTotalLimit))
    );
}

#[test]
fn test_merchant_restriction() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    let limit_per_tx = 100i128;
    let limit_total = 1000i128;
    let ttl_ledgers = 3600u32;

    let mut merchants = Vec::<soroban_sdk::Address>::new(&t.env);
    merchants.push_back(t.seller.clone());

    client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    assert_eq!(
        client.try_can_spend(&t.buyer, &t.agent, &50, &t.seller),
        Ok(Ok(()))
    );

    let unauthorized_merchant = t.admin.clone();
    assert_eq!(
        client.try_can_spend(&t.buyer, &t.agent, &50, &unauthorized_merchant),
        Err(Ok(PermissionError::MerchantNotAllowed))
    );
}

#[test]
fn test_permission_expiry() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    let limit_per_tx = 100i128;
    let limit_total = 1000i128;
    let ttl_ledgers = 100u32;
    let merchants = Vec::<soroban_sdk::Address>::new(&t.env);

    client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    assert_eq!(
        client.try_can_spend(&t.buyer, &t.agent, &50, &t.seller),
        Ok(Ok(()))
    );

    t.env
        .ledger()
        .set_sequence_number(t.env.ledger().sequence() + ttl_ledgers + 1);

    assert_eq!(
        client.try_can_spend(&t.buyer, &t.agent, &50, &t.seller),
        Err(Ok(PermissionError::Expired))
    );
}

#[test]
fn test_revoke_prevents_spend() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    let limit_per_tx = 100i128;
    let limit_total = 1000i128;
    let ttl_ledgers = 3600u32;
    let merchants = Vec::<soroban_sdk::Address>::new(&t.env);

    client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    client.revoke(&t.buyer, &t.agent);

    assert_eq!(
        client.try_can_spend(&t.buyer, &t.agent, &50, &t.seller),
        Err(Ok(PermissionError::Unauthorized))
    );
}

#[test]
fn test_permission_events() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    let limit_per_tx = 50i128;
    let limit_total = 100i128;
    let ttl_ledgers = 3600u32;
    let mut merchants = Vec::<soroban_sdk::Address>::new(&t.env);
    merchants.push_back(t.seller.clone());

    client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );
    let events = t.env.events().all();
    let mut granted_event_found = false;
    for event in events.iter() {
        let (contract, topics, value) = event;
        if contract == t.permissions_contract_id && topics.len() == 2 {
            let topic0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&t.env).unwrap();
            let topic1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&t.env).unwrap();
            if topic0 == soroban_sdk::symbol_short!("perm")
                && topic1 == soroban_sdk::symbol_short!("granted")
            {
                let evt: crate::PermissionGrantedEvent = value.try_into_val(&t.env).unwrap();
                assert_eq!(evt.owner, t.buyer);
                assert_eq!(evt.delegate, t.agent);
                assert_eq!(evt.per_tx_limit, limit_per_tx);
                assert_eq!(evt.total_limit, limit_total);
                assert_eq!(
                    evt.expires_at_ledger,
                    t.env.ledger().sequence() + ttl_ledgers
                );
                assert_eq!(evt.merchant_count, 1);
                granted_event_found = true;
            }
        }
    }
    assert!(granted_event_found);

    client.execute_spend(&t.buyer, &t.agent, &40, &t.seller);
    let events = t.env.events().all();
    let mut spent_event_found = false;
    for event in events.iter() {
        let (contract, topics, value) = event;
        if contract == t.permissions_contract_id && topics.len() == 2 {
            let topic0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&t.env).unwrap();
            let topic1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&t.env).unwrap();
            if topic0 == soroban_sdk::symbol_short!("perm")
                && topic1 == soroban_sdk::symbol_short!("spent")
            {
                let evt: crate::PermissionSpendEvent = value.try_into_val(&t.env).unwrap();
                assert_eq!(evt.owner, t.buyer);
                assert_eq!(evt.delegate, t.agent);
                assert_eq!(evt.amount, 40);
                assert_eq!(evt.merchant, t.seller);
                assert_eq!(evt.remaining, 60);
                spent_event_found = true;
            }
        }
    }
    assert!(spent_event_found);

    client.revoke(&t.buyer, &t.agent);
    let events = t.env.events().all();
    let mut revoked_event_found = false;
    for event in events.iter() {
        let (contract, topics, value) = event;
        if contract == t.permissions_contract_id && topics.len() == 2 {
            let topic0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&t.env).unwrap();
            let topic1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&t.env).unwrap();
            if topic0 == soroban_sdk::symbol_short!("perm")
                && topic1 == soroban_sdk::symbol_short!("revoked")
            {
                let evt: crate::PermissionRevokedEvent = value.try_into_val(&t.env).unwrap();
                assert_eq!(evt.owner, t.buyer);
                assert_eq!(evt.delegate, t.agent);
                revoked_event_found = true;
            }
        }
    }
    assert!(revoked_event_found);
}

#[test]
fn test_decrease_allowance_timelock_defaults_to_one_day() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    assert_eq!(client.get_decrease_timelock_secs(), 86400);

    let merchants = Vec::<soroban_sdk::Address>::new(&t.env);
    client.grant(&t.buyer, &t.agent, &1000, &100, &merchants, &36000);
    client.decrease_allowance(&t.buyer, &t.agent, &200);

    t.env
        .ledger()
        .set_timestamp(t.env.ledger().timestamp() + 86399);
    assert_eq!(
        client.try_execute_decrease_allowance(&t.buyer, &t.agent),
        Err(Ok(PermissionError::TimeLockActive))
    );
}

#[test]
fn test_set_decrease_allowance_timelock_custom_value() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    assert_eq!(
        client.try_set_decrease_timelock_secs(&t.admin, &0),
        Err(Ok(PermissionError::InvalidParam))
    );
    assert_eq!(
        client.try_set_decrease_timelock_secs(&t.admin, &2592001),
        Err(Ok(PermissionError::InvalidParam))
    );

    client.set_decrease_timelock_secs(&t.admin, &3600);
    assert_eq!(client.get_decrease_timelock_secs(), 3600);

    let merchants = Vec::<soroban_sdk::Address>::new(&t.env);
    client.grant(&t.buyer, &t.agent, &1000, &100, &merchants, &36000);
    client.decrease_allowance(&t.buyer, &t.agent, &200);

    t.env
        .ledger()
        .set_timestamp(t.env.ledger().timestamp() + 3599);
    assert_eq!(
        client.try_execute_decrease_allowance(&t.buyer, &t.agent),
        Err(Ok(PermissionError::TimeLockActive))
    );

    t.env
        .ledger()
        .set_timestamp(t.env.ledger().timestamp() + 1);
    client.execute_decrease_allowance(&t.buyer, &t.agent);
    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 800);
}

#[test]
fn test_decrease_allowance_timelock() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    let limit_per_tx = 100i128;
    let limit_total = 1000i128;
    let ttl_ledgers = 36000u32;
    let merchants = Vec::<soroban_sdk::Address>::new(&t.env);

    client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    client.decrease_allowance(&t.buyer, &t.agent, &200);

    // Advance past the 24h timelock (86400 seconds)
    t.env
        .ledger()
        .set_timestamp(t.env.ledger().timestamp() + 86401);

    client.execute_decrease_allowance(&t.buyer, &t.agent);

    // Verify allowance was decreased
    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 800);
}

#[test]
fn test_decrease_allowance_timelock_blocked() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    let limit_per_tx = 100i128;
    let limit_total = 1000i128;
    let ttl_ledgers = 36000u32;
    let merchants = Vec::<soroban_sdk::Address>::new(&t.env);

    client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    client.decrease_allowance(&t.buyer, &t.agent, &200);

    // Jump time but not enough (24h = 86400 seconds)
    t.env
        .ledger()
        .set_timestamp(t.env.ledger().timestamp() + 86399);

    assert_eq!(
        client.try_execute_decrease_allowance(&t.buyer, &t.agent),
        Err(Ok(PermissionError::TimeLockActive))
    );
}

#[test]
fn test_decrease_allowance_rejects_non_positive_amounts() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let merchants = Vec::<soroban_sdk::Address>::new(&t.env);

    client.grant(&t.buyer, &t.agent, &1000, &100, &merchants, &36000);

    for amount in [-1i128, 0i128] {
        assert_eq!(
            client.try_decrease_allowance(&t.buyer, &t.agent, &amount),
            Err(Ok(PermissionError::InvalidParam))
        );
    }

    client.decrease_allowance(&t.buyer, &t.agent, &200);
}

#[test]
fn test_decrease_allowance_accepts_positive_amount() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let merchants = Vec::<soroban_sdk::Address>::new(&t.env);

    client.grant(&t.buyer, &t.agent, &1000, &100, &merchants, &36000);
    client.decrease_allowance(&t.buyer, &t.agent, &1);
}

#[test]
fn test_decrease_allowance_rejects_pending_decrease() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let merchants = Vec::<soroban_sdk::Address>::new(&t.env);

    client.grant(&t.buyer, &t.agent, &1000, &100, &merchants, &36000);
    client.decrease_allowance(&t.buyer, &t.agent, &200);

    assert_eq!(
        client.try_decrease_allowance(&t.buyer, &t.agent, &100),
        Err(Ok(PermissionError::PendingDecreaseExists))
    );
}

#[test]
fn test_decrease_allowance_rejects_below_spent_at_schedule_time() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let merchant = soroban_sdk::Address::generate(&t.env);
    let merchants = Vec::<soroban_sdk::Address>::new(&t.env);

    client.grant(&t.buyer, &t.agent, &1000, &1000, &merchants, &36000);
    client.execute_spend(&t.buyer, &t.agent, &800, &merchant);

    let decrease = client.try_decrease_allowance(&t.buyer, &t.agent, &300);
    assert_eq!(decrease, Err(Ok(PermissionError::LimitBelowSpent)));

    // No pending decrement should have been scheduled: allowance is untouched.
    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 200);
}

#[test]
fn test_execute_decrease_allowance_rejects_below_spent_at_execution_time() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let merchant = soroban_sdk::Address::generate(&t.env);
    let merchants = Vec::<soroban_sdk::Address>::new(&t.env);

    client.grant(&t.buyer, &t.agent, &1000, &1000, &merchants, &36000);

    // Valid at schedule time: 900 <= 1000 remaining.
    client.decrease_allowance(&t.buyer, &t.agent, &900);

    // Spend moves during the timelock, undercutting the scheduled decrease.
    client.execute_spend(&t.buyer, &t.agent, &500, &merchant);
    t.env
        .ledger()
        .set_timestamp(t.env.ledger().timestamp() + 86401);

    assert_eq!(
        client.try_execute_decrease_allowance(&t.buyer, &t.agent),
        Err(Ok(PermissionError::LimitBelowSpent))
    );
}

// ── Issue #334: Gasless Spend Execution via Relayer Pattern ───────────────

#[test]
fn test_execute_spend_via_relayer_succeeds() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);

    let limit_per_tx = 50i128;
    let limit_total = 100i128;
    let ttl_ledgers = 3600u32;
    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    let (signing_key, public_key) = test_keypair(&t.env, 1);
    client.set_relayer_key(&t.agent, &public_key);

    let expiration_ledger = t.env.ledger().sequence() + 100;
    let message = RelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 40,
        nonce: 0,
        expiration_ledger,
    };
    let signature = sign_relayed_spend(&t.env, &signing_key, message);

    client.execute_spend_via_relayer(
        &relayer,
        &t.buyer,
        &t.agent,
        &40,
        &t.seller,
        &0u64,
        &expiration_ledger,
        &signature,
    );

    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 60);
    assert_eq!(client.get_relayer_nonce(&t.buyer, &t.agent), 1);
}

/// A second relayed spend inside the configured velocity interval is rejected
/// with `VelocityLimitExceeded`, while a later spend after the interval has
/// elapsed succeeds (issue #54).
#[test]
fn test_execute_spend_via_relayer_enforces_velocity_limit() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);

    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    client.grant(&t.buyer, &t.agent, &1000, &100, &merchants, &3600u32);

    // Configure a 10-ledger minimum spend interval.
    client.set_admin(&t.admin);
    client.set_velocity_limit(&t.admin, &10u32);

    let (signing_key, public_key) = test_keypair(&t.env, 9);
    client.set_relayer_key(&t.agent, &public_key);

    let expiration_ledger = t.env.ledger().sequence() + 1000;

    // First relayed spend succeeds and records the current ledger.
    let message = RelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 20,
        nonce: 0,
        expiration_ledger,
    };
    let signature = sign_relayed_spend(&t.env, &signing_key, message);
    client.execute_spend_via_relayer(
        &relayer,
        &t.buyer,
        &t.agent,
        &20,
        &t.seller,
        &0u64,
        &expiration_ledger,
        &signature,
    );

    // Second relayed spend within the interval (same ledger) is rejected.
    let message = RelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 20,
        nonce: 1,
        expiration_ledger,
    };
    let signature = sign_relayed_spend(&t.env, &signing_key, message);
    assert_eq!(
        client.try_execute_spend_via_relayer(
            &relayer,
            &t.buyer,
            &t.agent,
            &20,
            &t.seller,
            &1u64,
            &expiration_ledger,
            &signature,
        ),
        Err(Ok(PermissionError::VelocityLimitExceeded))
    );

    // After the interval elapses, a relayed spend succeeds again. The rejected
    // spend above never advanced the nonce, so it is still 1.
    t.env.ledger().with_mut(|li| {
        li.sequence_number += 10;
    });
    let message = RelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 20,
        nonce: 1,
        expiration_ledger,
    };
    let signature = sign_relayed_spend(&t.env, &signing_key, message);
    client.execute_spend_via_relayer(
        &relayer,
        &t.buyer,
        &t.agent,
        &20,
        &t.seller,
        &1u64,
        &expiration_ledger,
        &signature,
    );
    // Only the first and third relayed spends succeeded (40 total spent).
    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 960);
}

#[test]
fn test_execute_spend_via_relayer_rejects_replayed_nonce() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);

    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    client.grant(&t.buyer, &t.agent, &100, &50, &merchants, &3600u32);

    let (signing_key, public_key) = test_keypair(&t.env, 2);
    client.set_relayer_key(&t.agent, &public_key);

    let expiration_ledger = t.env.ledger().sequence() + 100;
    let message = RelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 20,
        nonce: 0,
        expiration_ledger,
    };
    let signature = sign_relayed_spend(&t.env, &signing_key, message);

    client.execute_spend_via_relayer(
        &relayer,
        &t.buyer,
        &t.agent,
        &20,
        &t.seller,
        &0u64,
        &expiration_ledger,
        &signature,
    );

    // Replaying the exact same signed message (nonce 0 again) is rejected.
    assert_eq!(
        client.try_execute_spend_via_relayer(
            &relayer,
            &t.buyer,
            &t.agent,
            &20,
            &t.seller,
            &0u64,
            &expiration_ledger,
            &signature,
        ),
        Err(Ok(PermissionError::InvalidNonce))
    );
}

#[test]
#[should_panic]
fn test_execute_spend_via_relayer_rejects_invalid_signature() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);

    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    client.grant(&t.buyer, &t.agent, &100, &50, &merchants, &3600u32);

    let (_registered_key, public_key) = test_keypair(&t.env, 3);
    client.set_relayer_key(&t.agent, &public_key);

    // Sign with a different key than the one registered for the delegate.
    let (wrong_key, _wrong_public) = test_keypair(&t.env, 99);
    let expiration_ledger = t.env.ledger().sequence() + 100;
    let message = RelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 20,
        nonce: 0,
        expiration_ledger,
    };
    let signature = sign_relayed_spend(&t.env, &wrong_key, message);

    client.execute_spend_via_relayer(
        &relayer,
        &t.buyer,
        &t.agent,
        &20,
        &t.seller,
        &0u64,
        &expiration_ledger,
        &signature,
    );
}

#[test]
fn test_execute_spend_via_relayer_rejects_expired_signature() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);

    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    client.grant(&t.buyer, &t.agent, &100, &50, &merchants, &3600u32);

    let (signing_key, public_key) = test_keypair(&t.env, 4);
    client.set_relayer_key(&t.agent, &public_key);

    // Expiration is already at (or before) the current ledger sequence.
    let expiration_ledger = t.env.ledger().sequence();
    let message = RelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 20,
        nonce: 0,
        expiration_ledger,
    };
    let signature = sign_relayed_spend(&t.env, &signing_key, message);

    assert_eq!(
        client.try_execute_spend_via_relayer(
            &relayer,
            &t.buyer,
            &t.agent,
            &20,
            &t.seller,
            &0u64,
            &expiration_ledger,
            &signature,
        ),
        Err(Ok(PermissionError::SignatureExpired))
    );
}

#[test]
fn test_execute_spend_via_relayer_without_registered_key_fails() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);

    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    client.grant(&t.buyer, &t.agent, &100, &50, &merchants, &3600u32);

    let (signing_key, _public_key) = test_keypair(&t.env, 5);
    let expiration_ledger = t.env.ledger().sequence() + 100;
    let message = RelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 20,
        nonce: 0,
        expiration_ledger,
    };
    let signature = sign_relayed_spend(&t.env, &signing_key, message);

    assert_eq!(
        client.try_execute_spend_via_relayer(
            &relayer,
            &t.buyer,
            &t.agent,
            &20,
            &t.seller,
            &0u64,
            &expiration_ledger,
            &signature,
        ),
        Err(Ok(PermissionError::RelayerKeyNotSet))
    );
}

#[test]
fn test_execute_spend_via_relayer_enforces_per_tx_limit() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);

    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    client.grant(&t.buyer, &t.agent, &100, &50, &merchants, &3600u32);

    let (signing_key, public_key) = test_keypair(&t.env, 6);
    client.set_relayer_key(&t.agent, &public_key);

    let expiration_ledger = t.env.ledger().sequence() + 100;
    let message = RelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 999,
        nonce: 0,
        expiration_ledger,
    };
    let signature = sign_relayed_spend(&t.env, &signing_key, message);

    assert_eq!(
        client.try_execute_spend_via_relayer(
            &relayer,
            &t.buyer,
            &t.agent,
            &999,
            &t.seller,
            &0u64,
            &expiration_ledger,
            &signature,
        ),
        Err(Ok(PermissionError::ExceedsPerTxLimit))
    );
}

// ── Issue #336: Permission Usage Analytics On-Chain ────────────────────────

#[test]
fn test_usage_stats_update_after_each_spend() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let merchants = Vec::<Address>::new(&t.env);
    client.grant(&t.buyer, &t.agent, &1000, &500, &merchants, &3600u32);

    let empty = client.get_usage_stats(&t.buyer, &t.agent);
    assert_eq!(empty.total_spends, 0);
    assert_eq!(empty.total_spent, 0);

    client.execute_spend(&t.buyer, &t.agent, &100, &t.seller);
    let after_first = client.get_usage_stats(&t.buyer, &t.agent);
    assert_eq!(after_first.total_spends, 1);
    assert_eq!(after_first.total_spent, 100);
    assert_eq!(after_first.first_spend_ledger, t.env.ledger().sequence());
    assert_eq!(after_first.last_spend_ledger, t.env.ledger().sequence());

    t.env
        .ledger()
        .set_sequence_number(t.env.ledger().sequence() + 5);
    client.execute_spend(&t.buyer, &t.agent, &200, &t.seller);
    let after_second = client.get_usage_stats(&t.buyer, &t.agent);
    assert_eq!(after_second.total_spends, 2);
    assert_eq!(after_second.total_spent, 300);
    assert_eq!(
        after_second.first_spend_ledger,
        after_first.first_spend_ledger
    );
    assert_eq!(after_second.last_spend_ledger, t.env.ledger().sequence());
}

#[test]
fn test_usage_stats_correct_average() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let merchants = Vec::<Address>::new(&t.env);
    client.grant(&t.buyer, &t.agent, &1000, &500, &merchants, &3600u32);

    client.execute_spend(&t.buyer, &t.agent, &100, &t.seller);
    client.execute_spend(&t.buyer, &t.agent, &200, &t.seller);
    client.execute_spend(&t.buyer, &t.agent, &300, &t.seller);

    let stats = client.get_usage_stats(&t.buyer, &t.agent);
    assert_eq!(stats.total_spends, 3);
    assert_eq!(stats.total_spent, 600);
    assert_eq!(stats.average_spend, 200);
}

#[test]
fn test_usage_stats_tracks_largest_spend() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let merchants = Vec::<Address>::new(&t.env);
    client.grant(&t.buyer, &t.agent, &1000, &500, &merchants, &3600u32);

    client.execute_spend(&t.buyer, &t.agent, &150, &t.seller);
    assert_eq!(
        client.get_usage_stats(&t.buyer, &t.agent).largest_spend,
        150
    );

    client.execute_spend(&t.buyer, &t.agent, &75, &t.seller);
    assert_eq!(
        client.get_usage_stats(&t.buyer, &t.agent).largest_spend,
        150
    );

    client.execute_spend(&t.buyer, &t.agent, &400, &t.seller);
    assert_eq!(
        client.get_usage_stats(&t.buyer, &t.agent).largest_spend,
        400
    );
}

#[test]
fn test_usage_stats_not_updated_on_rejected_spend() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let merchants = Vec::<Address>::new(&t.env);
    client.grant(&t.buyer, &t.agent, &100, &50, &merchants, &3600u32);

    assert_eq!(
        client.try_execute_spend(&t.buyer, &t.agent, &999, &t.seller),
        Err(Ok(PermissionError::ExceedsPerTxLimit))
    );

    let stats = client.get_usage_stats(&t.buyer, &t.agent);
    assert_eq!(stats.total_spends, 0);
}

#[test]
fn test_usage_stats_include_relayed_spends() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);
    let merchants = Vec::<Address>::new(&t.env);
    client.grant(&t.buyer, &t.agent, &1000, &500, &merchants, &3600u32);

    let (signing_key, public_key) = test_keypair(&t.env, 42);
    client.set_relayer_key(&t.agent, &public_key);

    let expiration_ledger = t.env.ledger().sequence() + 100;
    let message = RelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 250,
        nonce: 0,
        expiration_ledger,
    };
    let signature = sign_relayed_spend(&t.env, &signing_key, message);
    client.execute_spend_via_relayer(
        &relayer,
        &t.buyer,
        &t.agent,
        &250,
        &t.seller,
        &0u64,
        &expiration_ledger,
        &signature,
    );

    let stats = client.get_usage_stats(&t.buyer, &t.agent);
    assert_eq!(stats.total_spends, 1);
    assert_eq!(stats.total_spent, 250);
    assert_eq!(stats.largest_spend, 250);
}

// --- transfer_permission (issue #318) ---

#[test]
fn test_transfer_permission_preserves_remaining_allowance() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let new_agent = Address::generate(&t.env);

    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    client.grant(&t.buyer, &t.agent, &100, &50, &merchants, &3600u32);

    client.execute_spend(&t.buyer, &t.agent, &40, &t.seller);
    let remaining_before = client.get_remaining_allowance(&t.buyer, &t.agent);
    assert_eq!(remaining_before, 60);

    client.transfer_permission(&t.buyer, &t.agent, &new_agent);

    let new_record = client.get_permission(&t.buyer, &new_agent);
    assert_eq!(new_record.spent, 40);
    assert_eq!(new_record.limit_total, 100);
    assert_eq!(new_record.status, PermissionStatus::Active);
    assert_eq!(client.get_remaining_allowance(&t.buyer, &new_agent), 60);

    // New permission preserves the same merchant whitelist.
    assert_eq!(new_record.allowed_merchants.len(), 1);
    assert_eq!(new_record.allowed_merchants.get(0).unwrap(), t.seller);
}

#[test]
fn test_transfer_permission_revokes_old_and_emits_events() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let new_agent = Address::generate(&t.env);

    let merchants = Vec::<Address>::new(&t.env);
    client.grant(&t.buyer, &t.agent, &1000, &500, &merchants, &3600u32);

    client.transfer_permission(&t.buyer, &t.agent, &new_agent);

    let events = t.env.events().all();
    let mut found = false;
    for event in events.iter() {
        let (contract, topics, _value) = event;
        if contract != t.permissions_contract_id || topics.len() != 2 {
            continue;
        }
        let t0: Symbol = topics.get(0).unwrap().try_into_val(&t.env).unwrap();
        let t1: Symbol = topics.get(1).unwrap().try_into_val(&t.env).unwrap();
        if t0 == soroban_sdk::symbol_short!("perm") && t1 == soroban_sdk::symbol_short!("transf") {
            found = true;
            break;
        }
    }
    assert!(found, "expected a PermissionTransferredEvent to be emitted");

    let old_record = client.get_permission(&t.buyer, &t.agent);
    assert_eq!(old_record.status, PermissionStatus::Revoked);

    // Old delegate can no longer spend.
    assert_eq!(
        client.try_can_spend(&t.buyer, &t.agent, &50, &t.seller),
        Err(Ok(PermissionError::Unauthorized))
    );

    // New delegate can spend against the transferred allowance.
    assert_eq!(
        client.try_can_spend(&t.buyer, &new_agent, &50, &t.seller),
        Ok(Ok(()))
    );
}

#[test]
fn test_transfer_permission_fails_if_old_permission_not_found() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let new_agent = Address::generate(&t.env);

    assert_eq!(
        client.try_transfer_permission(&t.buyer, &t.agent, &new_agent),
        Err(Ok(PermissionError::PermissionNotFound))
    );
}

#[test]
fn test_transfer_permission_fails_for_self_transfer() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    let merchants = Vec::<Address>::new(&t.env);
    client.grant(&t.buyer, &t.agent, &1000, &500, &merchants, &3600u32);

    assert_eq!(
        client.try_transfer_permission(&t.buyer, &t.agent, &t.agent),
        Err(Ok(PermissionError::InvalidParam))
    );
}

#[test]
fn test_transfer_permission_fails_if_new_delegate_already_has_permission() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let new_agent = Address::generate(&t.env);

    let merchants = Vec::<Address>::new(&t.env);
    client.grant(&t.buyer, &t.agent, &1000, &500, &merchants, &3600u32);
    client.grant(&t.buyer, &new_agent, &1000, &500, &merchants, &3600u32);

    assert_eq!(
        client.try_transfer_permission(&t.buyer, &t.agent, &new_agent),
        Err(Ok(PermissionError::InvalidParam))
    );
}

// ── Issue #55: Relayed spend propagates through parent-chain budget ─────────

/// A relayed child spend must decrement the parent budget just like a direct
/// spend does.  Before this fix, `execute_spend_via_relayer` only touched the
/// child record; after it the parent's `spent` counter must also increase.
#[test]
fn test_relayed_child_spend_decrements_parent_budget() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);
    let child_delegate = Address::generate(&t.env);

    // ── Set up parent permission: buyer → agent, total 200 ──────────────────
    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    client.grant(
        &t.buyer, &t.agent, &200i128, // parent total
        &100i128, // parent per-tx
        &merchants, &3600u32,
    );

    // ── Set up child permission: agent → child_delegate, total 100 ──────────
    // grant_child requires parent_delegate (= agent) to auth.
    client.grant_child(
        &t.buyer,
        &t.agent,
        &child_delegate,
        &100i128, // child total — carved out of parent's 200
        &100i128, // child per-tx
        &merchants,
        &3600u32,
    );

    // Confirm initial parent remaining = 200, child remaining = 100.
    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 200);
    assert_eq!(
        client.get_remaining_allowance(&t.agent, &child_delegate),
        100
    );

    // ── Register an ed25519 key for the child delegate ───────────────────────
    let (signing_key, public_key) = test_keypair(&t.env, 55);
    client.set_relayer_key(&child_delegate, &public_key);

    // ── Build and sign a relayed spend of 75 on the child permission ─────────
    let expiration_ledger = t.env.ledger().sequence() + 200;
    let message = RelayedSpendMessage {
        owner: t.agent.clone(), // child's owner == parent delegate
        delegate: child_delegate.clone(),
        merchant: t.seller.clone(),
        amount: 75,
        nonce: 0,
        expiration_ledger,
    };
    let signature = sign_relayed_spend(&t.env, &signing_key, message);

    client.execute_spend_via_relayer(
        &relayer,
        &t.agent, // owner of child permission
        &child_delegate,
        &75i128,
        &t.seller,
        &0u64,
        &expiration_ledger,
        &signature,
    );

    // ── Assertions ───────────────────────────────────────────────────────────
    // Child's remaining should drop by 75.
    assert_eq!(
        client.get_remaining_allowance(&t.agent, &child_delegate),
        25,
        "child remaining should be 100 - 75 = 25"
    );

    // Parent's remaining MUST ALSO drop by 75 (the whole point of issue #55).
    assert_eq!(
        client.get_remaining_allowance(&t.buyer, &t.agent),
        125,
        "parent remaining should be 200 - 75 = 125 (relayed spend must decrement parent)"
    );
}

/// Verify that after the fix, both direct and relayed spends through the same
/// child permission equally consume the shared parent budget, and the parent
/// cap is enforced on the relayed path.
#[test]
fn test_relayed_spend_respects_parent_budget_cap() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);
    let child_delegate = Address::generate(&t.env);

    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());

    // Parent: 100 total; child: 100 total.
    client.grant(&t.buyer, &t.agent, &100i128, &100i128, &merchants, &3600u32);
    client.grant_child(
        &t.buyer,
        &t.agent,
        &child_delegate,
        &100i128,
        &100i128,
        &merchants,
        &3600u32,
    );

    // Spend 90 directly on the child, which should propagate to the parent.
    client.execute_spend(&t.agent, &child_delegate, &90, &t.seller);
    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 10);
    assert_eq!(
        client.get_remaining_allowance(&t.agent, &child_delegate),
        10
    );

    // Now attempt a relayed spend of 20, which should be blocked by the parent
    // cap (only 10 remaining there).
    let (signing_key, public_key) = test_keypair(&t.env, 56);
    client.set_relayer_key(&child_delegate, &public_key);
    let expiration_ledger = t.env.ledger().sequence() + 200;
    let message = RelayedSpendMessage {
        owner: t.agent.clone(),
        delegate: child_delegate.clone(),
        merchant: t.seller.clone(),
        amount: 20,
        nonce: 0,
        expiration_ledger,
    };
    let signature = sign_relayed_spend(&t.env, &signing_key, message);

    // The child still has 10 remaining, but even before reaching apply_spend
    // the can_spend check on the child will block the 20 spend (child limit
    // is also only 10).
    assert_eq!(
        client.try_execute_spend_via_relayer(
            &relayer,
            &t.agent,
            &child_delegate,
            &20,
            &t.seller,
            &0u64,
            &expiration_ledger,
            &signature,
        ),
        Err(Ok(PermissionError::ExceedsTotalLimit))
    );
}
