#![cfg(test)]

use crate::{
    ChannelRelayedSpendMessage, ChannelSpendSignature, MerchantAllowlist, PermissionError,
    PermissionStatus, PermissionsContract, PermissionsContractClient, RelayedSpendMessage,
    ScopedPermissionConfig,
};
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{
    symbol_short,
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

/// Sign a `RelayedSpendMessage` against a specific versioned domain separator,
/// mirroring the on-chain `compute_versioned_domain_separator` derivation so
/// tests can produce signatures bound to a particular contract version.
fn sign_relayed_spend_with_domain(
    env: &Env,
    signing_key: &SigningKey,
    message: RelayedSpendMessage,
    domain_separator: &BytesN<32>,
) -> BytesN<64> {
    let mut payload = soroban_sdk::Bytes::new(env);
    payload.append(&domain_separator.clone().into());
    payload.append(&message.to_xdr(env));
    let len = payload.len() as usize;
    let mut buf = [0u8; 512];
    payload.copy_into_slice(&mut buf[..len]);
    let signature = signing_key.sign(&buf[..len]);
    BytesN::from_array(env, &signature.to_bytes())
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

/// Sign a `ChannelRelayedSpendMessage` with the given key, returning the
/// raw 64-byte ed25519 signature over the message's canonical XDR encoding —
/// the exact bytes `execute_spend_via_channel` re-derives and verifies.
fn sign_channel_spend(
    env: &Env,
    signing_key: &SigningKey,
    message: ChannelRelayedSpendMessage,
) -> BytesN<64> {
    let message_bytes = message.to_xdr(env);
    let len = message_bytes.len() as usize;
    let mut buf = [0u8; 512];
    message_bytes.copy_into_slice(&mut buf[..len]);
    let signature = signing_key.sign(&buf[..len]);
    BytesN::from_array(env, &signature.to_bytes())
}

/// Compute the versioned domain separator for the given contract address and
/// semver string, matching the on-chain implementation.
fn compute_versioned_domain_separator(
    env: &Env,
    contract_address: &Address,
    semver: &Symbol,
) -> BytesN<32> {
    let mut payload = soroban_sdk::Bytes::new(env);
    payload.append(&contract_address.to_xdr(env));
    payload.append(&semver.to_xdr(env));
    env.crypto().sha256(&payload).into()
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
    client.set_admin(&t.admin);

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

    t.env.ledger().set_timestamp(t.env.ledger().timestamp() + 1);
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
        epoch: 0,
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
        &0u32,
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
        epoch: 0,
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
        &0u32,
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
        epoch: 0,
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
            &0u32,
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
        epoch: 0,
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
        &0u32,
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
        epoch: 0,
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
        &0u32,
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
            &0u32,
            &signature,
        ),
        Err(Ok(PermissionError::InvalidNonce))
    );
}

#[test]
fn test_execute_spend_via_relayer_rejects_stale_epoch_after_pause_resume() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);

    let merchants = Vec::<Address>::new(&t.env);
    client.grant(&t.buyer, &t.agent, &100, &50, &merchants, &3600u32);

    let (signing_key, public_key) = test_keypair(&t.env, 23);
    client.set_relayer_key(&t.agent, &public_key);
    let expiration_ledger = t.env.ledger().sequence() + 100;
    let message = RelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 20,
        nonce: 0,
        expiration_ledger,
        epoch: 0,
    };
    let signature = sign_relayed_spend(&t.env, &signing_key, message);

    client.pause(&t.buyer, &t.agent);
    client.resume(&t.buyer, &t.agent);

    let epoch = client.get_execution_epoch(&t.buyer, &t.agent);
    assert_eq!(epoch.current_epoch, 2);
    assert_eq!(
        client.try_execute_spend_via_relayer(
            &relayer,
            &t.buyer,
            &t.agent,
            &20,
            &t.seller,
            &0u64,
            &expiration_ledger,
            &0u32,
            &signature,
        ),
        Err(Ok(PermissionError::StaleEpoch))
    );
    assert_eq!(client.get_relayer_nonce(&t.buyer, &t.agent), 0);
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
        epoch: 0,
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
        &0u32,
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
        epoch: 0,
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
            &0u32,
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
        epoch: 0,
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
            &0u32,
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
        epoch: 0,
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
            &0u32,
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
        epoch: 0,
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
        &0u32,
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

// ── Issue: Strict Domain Separator Hash Invalidation Across Upgrades ──────

#[test]
fn test_versioned_domain_separator_differs_across_versions() {
    let t = TestEnv::setup();
    let v1 = compute_versioned_domain_separator(
        &t.env,
        &t.permissions_contract_id,
        &symbol_short!("PERM_V1"),
    );
    let v2 = compute_versioned_domain_separator(
        &t.env,
        &t.permissions_contract_id,
        &symbol_short!("PERM_V2"),
    );
    assert_ne!(
        v1, v2,
        "domain separators must differ across semver versions"
    );
}

#[test]
fn test_versioned_domain_separator_differs_across_addresses() {
    let t = TestEnv::setup();
    let other = Address::generate(&t.env);
    let a = compute_versioned_domain_separator(
        &t.env,
        &t.permissions_contract_id,
        &symbol_short!("PERM_V2"),
    );
    let b = compute_versioned_domain_separator(&t.env, &other, &symbol_short!("PERM_V2"));
    assert_ne!(
        a, b,
        "domain separators must differ across contract addresses"
    );
}

#[test]
fn test_signature_bound_to_v1_fails_on_v2() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);

    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    client.grant(&t.buyer, &t.agent, &1000, &100, &merchants, &3600u32);

    let (signing_key, public_key) = test_keypair(&t.env, 77);
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

    // Sign against the V1 domain separator — this is what an attacker would
    // replay after the contract is upgraded to V2.
    let v1_domain = compute_versioned_domain_separator(
        &t.env,
        &t.permissions_contract_id,
        &symbol_short!("PERM_V1"),
    );
    let v1_signature =
        sign_relayed_spend_with_domain(&t.env, &signing_key, message.clone(), &v1_domain);

    // The live contract uses the V2 domain separator, so the V1 signature must
    // be rejected outright.
    assert_eq!(
        client.try_execute_spend_via_relayer(
            &relayer,
            &t.buyer,
            &t.agent,
            &20,
            &t.seller,
            &0u64,
            &expiration_ledger,
            &v1_signature,
        ),
        Err(Ok(PermissionError::InvalidSignature))
    );
    assert_eq!(client.get_relayer_nonce(&t.buyer, &t.agent), 0);
    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 1000);

    // A signature produced against the V2 domain separator is accepted.
    let v2_domain = compute_versioned_domain_separator(
        &t.env,
        &t.permissions_contract_id,
        &symbol_short!("PERM_V2"),
    );
    let v2_signature = sign_relayed_spend_with_domain(&t.env, &signing_key, message, &v2_domain);
    client.execute_spend_via_relayer(
        &relayer,
        &t.buyer,
        &t.agent,
        &20,
        &t.seller,
        &0u64,
        &expiration_ledger,
        &v2_signature,
    );
    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 980);
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
        epoch: 0,
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
        &0u32,
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
        epoch: 0,
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
            &0u32,
            &signature,
        ),
        Err(Ok(PermissionError::ExceedsTotalLimit))
    );
}

#[test]
fn test_validate_chain_prevents_mutation_on_exceeds_parent_limit() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
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

    // Spend 50 directly on the parent. Parent remaining = 50.
    client.execute_spend(&t.buyer, &t.agent, &50, &t.seller);
    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 50);

    // Child attempts to spend 60. Child has 100 limit, so child check passes.
    // Parent only has 50 remaining, so parent check fails.
    assert_eq!(
        client.try_execute_spend(&t.agent, &child_delegate, &60, &t.seller),
        Err(Ok(PermissionError::ExceedsParentLimit))
    );

    // Child state should not be mutated (spent is 0, remaining is 100)
    assert_eq!(
        client.get_remaining_allowance(&t.agent, &child_delegate),
        100
    );
    // Parent state should not be further mutated (spent is 50, remaining is 50)
    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 50);
}

// ── Issue #296: seller-specific allowlist ─────────────────────────────────

#[test]
fn test_merchant_allowlist_blocks_non_allowlisted_merchant() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let rogue = Address::generate(&t.env);

    // Unrestricted grant: without an allowlist any merchant is accepted.
    client.grant(&t.buyer, &t.agent, &1000, &100, &Vec::new(&t.env), &3600u32);
    assert_eq!(
        client.try_can_spend(&t.buyer, &t.agent, &10, &rogue),
        Ok(Ok(()))
    );

    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    client.set_merchant_allowlist(&t.buyer, &t.agent, &true, &merchants);

    assert_eq!(
        client.get_merchant_allowlist(&t.buyer, &t.agent),
        Some(MerchantAllowlist {
            is_enabled: true,
            merchants: merchants.clone(),
        })
    );
    assert_eq!(
        client.try_execute_spend(&t.buyer, &t.agent, &10, &rogue),
        Err(Ok(PermissionError::MerchantNotAllowed))
    );
    client.execute_spend(&t.buyer, &t.agent, &10, &t.seller);
    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 990);

    // Disabling the allowlist lifts the restriction.
    client.set_merchant_allowlist(&t.buyer, &t.agent, &false, &merchants);
    client.execute_spend(&t.buyer, &t.agent, &10, &rogue);
    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 980);
}

#[test]
fn test_merchant_allowlist_enabled_but_empty_blocks_all_spends() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    client.grant(&t.buyer, &t.agent, &1000, &100, &Vec::new(&t.env), &3600u32);
    client.set_merchant_allowlist(&t.buyer, &t.agent, &true, &Vec::new(&t.env));

    assert_eq!(
        client.try_execute_spend(&t.buyer, &t.agent, &10, &t.seller),
        Err(Ok(PermissionError::MerchantNotAllowed))
    );
}

#[test]
fn test_merchant_allowlist_enforced_on_relayed_spend() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);
    let rogue = Address::generate(&t.env);

    client.grant(&t.buyer, &t.agent, &1000, &100, &Vec::new(&t.env), &3600u32);
    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    client.set_merchant_allowlist(&t.buyer, &t.agent, &true, &merchants);

    let (signing_key, public_key) = test_keypair(&t.env, 21);
    client.set_relayer_key(&t.agent, &public_key);

    let expiration_ledger = t.env.ledger().sequence() + 100;
    let signature = sign_relayed_spend(
        &t.env,
        &signing_key,
        RelayedSpendMessage {
            owner: t.buyer.clone(),
            delegate: t.agent.clone(),
            merchant: rogue.clone(),
            amount: 20,
            nonce: 0,
            expiration_ledger,
            epoch: 0,
        },
    );

    assert_eq!(
        client.try_execute_spend_via_relayer(
            &relayer,
            &t.buyer,
            &t.agent,
            &20,
            &rogue,
            &0u64,
            &expiration_ledger,
            &0u32,
            &signature,
        ),
        Err(Ok(PermissionError::MerchantNotAllowed))
    );
    assert_eq!(client.get_relayer_nonce(&t.buyer, &t.agent), 0);
}

#[test]
fn test_merchant_allowlist_enforces_max_size_and_uniqueness() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    client.grant(&t.buyer, &t.agent, &1000, &100, &Vec::new(&t.env), &3600u32);

    let mut merchants = Vec::<Address>::new(&t.env);
    for _ in 0..crate::MAX_MERCHANTS_PER_PERMISSION {
        merchants.push_back(Address::generate(&t.env));
    }
    client.set_merchant_allowlist(&t.buyer, &t.agent, &true, &merchants);

    merchants.push_back(Address::generate(&t.env));
    assert_eq!(
        client.try_set_merchant_allowlist(&t.buyer, &t.agent, &true, &merchants),
        Err(Ok(PermissionError::InvalidParam))
    );

    let mut duplicates = Vec::<Address>::new(&t.env);
    duplicates.push_back(t.seller.clone());
    duplicates.push_back(t.seller.clone());
    assert_eq!(
        client.try_set_merchant_allowlist(&t.buyer, &t.agent, &true, &duplicates),
        Err(Ok(PermissionError::InvalidParam))
    );
}

#[test]
fn test_merchant_allowlist_requires_existing_permission() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    assert_eq!(
        client.try_set_merchant_allowlist(&t.buyer, &t.agent, &true, &Vec::new(&t.env)),
        Err(Ok(PermissionError::PermissionNotFound))
    );
}

// ── Issue #297: relayer nonce cancellation ────────────────────────────────

#[test]
fn test_cancel_nonce_unblocks_subsequent_relayed_spends() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);

    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    client.grant(&t.buyer, &t.agent, &1000, &100, &merchants, &3600u32);

    let (signing_key, public_key) = test_keypair(&t.env, 22);
    client.set_relayer_key(&t.agent, &public_key);
    let expiration_ledger = t.env.ledger().sequence() + 100;

    let sign = |nonce: u64| {
        sign_relayed_spend(
            &t.env,
            &signing_key,
            RelayedSpendMessage {
                owner: t.buyer.clone(),
                delegate: t.agent.clone(),
                merchant: t.seller.clone(),
                amount: 20,
                nonce,
                expiration_ledger,
                epoch: 0,
            },
        )
    };
    let stalled = sign(0);
    let next = sign(1);

    // Nonce 0 was dropped/censored, so nonce 1 is stuck behind it.
    assert_eq!(
        client.try_execute_spend_via_relayer(
            &relayer,
            &t.buyer,
            &t.agent,
            &20,
            &t.seller,
            &1u64,
            &expiration_ledger,
            &0u32,
            &next,
        ),
        Err(Ok(PermissionError::InvalidNonce))
    );

    client.cancel_nonce(&t.buyer, &t.agent, &0u64);
    assert_eq!(client.get_relayer_nonce(&t.buyer, &t.agent), 1);

    // The stalled message can no longer be replayed later...
    assert_eq!(
        client.try_execute_spend_via_relayer(
            &relayer,
            &t.buyer,
            &t.agent,
            &20,
            &t.seller,
            &0u64,
            &expiration_ledger,
            &0u32,
            &stalled,
        ),
        Err(Ok(PermissionError::InvalidNonce))
    );
    // ...and the next one now goes through without revoking the delegation.
    client.execute_spend_via_relayer(
        &relayer,
        &t.buyer,
        &t.agent,
        &20,
        &t.seller,
        &1u64,
        &expiration_ledger,
        &0u32,
        &next,
    );
    assert_eq!(client.get_relayer_nonce(&t.buyer, &t.agent), 2);
    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 980);
    assert!(client.is_active(&t.buyer, &t.agent));
}

#[test]
fn test_cancel_nonce_can_skip_a_window_of_nonces() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    client.grant(&t.buyer, &t.agent, &1000, &100, &Vec::new(&t.env), &3600u32);
    client.cancel_nonce(&t.buyer, &t.agent, &4u64);
    assert_eq!(client.get_relayer_nonce(&t.buyer, &t.agent), 5);
}

#[test]
fn test_cancel_nonce_rejects_consumed_nonce_and_overflow() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    client.grant(&t.buyer, &t.agent, &1000, &100, &Vec::new(&t.env), &3600u32);
    client.cancel_nonce(&t.buyer, &t.agent, &2u64);

    assert_eq!(
        client.try_cancel_nonce(&t.buyer, &t.agent, &1u64),
        Err(Ok(PermissionError::NonceAlreadyUsed))
    );
    assert_eq!(
        client.try_cancel_nonce(&t.buyer, &t.agent, &u64::MAX),
        Err(Ok(PermissionError::NonceAlreadyUsed))
    );
    assert_eq!(client.get_relayer_nonce(&t.buyer, &t.agent), 3);
}

#[test]
fn test_cancel_nonce_requires_existing_permission() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    assert_eq!(
        client.try_cancel_nonce(&t.buyer, &t.agent, &0u64),
        Err(Ok(PermissionError::PermissionNotFound))
    );
}

// ── Issue #369: function-scoped permissions vs. the relayer path ───────────

/// The signed `RelayedSpendMessage` carries no contract entrypoint, so a
/// function-scoped grant cannot be spent through the gasless relayer path:
/// the signature never attests *which* function the relayer is invoking. The
/// check fails closed rather than being skipped, so a scoped delegate's
/// allowance cannot be drained by relaying around the scope (issue #369).
#[test]
fn test_scoped_grant_rejects_relayed_spend() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);

    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());

    let mut functions = Vec::new(&t.env);
    functions.push_back(Symbol::new(&t.env, "fund"));
    client.grant_scoped(
        &t.buyer,
        &t.agent,
        &1000,
        &100,
        &merchants,
        &3600u32,
        &ScopedPermissionConfig {
            target_contract: t._escrow_contract_id.clone(),
            allowed_function_symbols: functions,
        },
    );

    let (signing_key, public_key) = test_keypair(&t.env, 11);
    client.set_relayer_key(&t.agent, &public_key);

    let expiration_ledger = t.env.ledger().sequence() + 100;
    let message = RelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 40,
        nonce: 0,
        expiration_ledger,
        epoch: 0,
    };
    let signature = sign_relayed_spend(&t.env, &signing_key, message);

    assert_eq!(
        client.try_execute_spend_via_relayer(
            &relayer,
            &t.buyer,
            &t.agent,
            &40,
            &t.seller,
            &0u64,
            &expiration_ledger,
            &0u32,
            &signature,
        ),
        Err(Ok(PermissionError::UnauthorizedFunction))
    );

    // Nothing was spent and the nonce was not consumed.
    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 1000);
    assert_eq!(client.get_relayer_nonce(&t.buyer, &t.agent), 0);

    // The delegate can still spend through the scoped entrypoint itself.
    assert_eq!(
        client.try_execute_spend_scoped(
            &t.buyer,
            &t.agent,
            &40,
            &t.seller,
            &Some(t._escrow_contract_id.clone()),
            &Some(Symbol::new(&t.env, "fund")),
        ),
        Ok(Ok(()))
    );
    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 960);
}

// ── Issue: Handle Verification Policy Threshold Increases ─────────────────

/// Helper: register a merchant with the given number of verifications under
/// the currently configured policy, returning the merchant id.
fn register_verified_merchant(
    t: &TestEnv,
    client: &PermissionsContractClient,
    verifications: u32,
) -> u64 {
    let merchant = Address::generate(&t.env);
    let id = client.register_merchant(&t.admin, &merchant);
    for _ in 0..verifications {
        client.add_merchant_verification(&t.admin, &id);
    }
    id
}

/// A merchant verified under a 1-of-N policy must not remain "verified"
/// indefinitely once governance raises the required threshold to 2. The
/// dynamic check must report them as failing the new policy, and the
/// revalidation entrypoint must transition them out of the verified state.
#[test]
fn test_policy_threshold_increase_invalidates_pre_existing_merchant() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    client.set_admin(&t.admin);

    // Governance starts with a 1-verification policy.
    client.set_verification_policy(&t.admin, &1u32);

    // Merchant is registered and fully verified under the old policy.
    let merchant_id = register_verified_merchant(&t, &client, 1);
    assert!(client.is_merchant_verified(&merchant_id));

    // Governance raises the required verifications to 2.
    client.set_verification_policy(&t.admin, &2u32);

    // The dynamic check must immediately reflect the new policy: the merchant
    // only holds 1 attestation, so they no longer satisfy the requirement.
    assert!(!client.is_merchant_verified(&merchant_id));

    // Revalidation must transition the merchant out of the verified state.
    let still_verified = client.revalidate_merchant_status(&merchant_id);
    assert!(!still_verified);
    assert!(!client.is_merchant_verified(&merchant_id));
}

/// A merchant that acquires the additional attestation within the grace
/// period must be re-validated back into the verified state.
#[test]
fn test_policy_threshold_increase_grace_period_allows_recovery() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    client.set_admin(&t.admin);

    client.set_verification_policy(&t.admin, &1u32);
    let merchant_id = register_verified_merchant(&t, &client, 1);
    assert!(client.is_merchant_verified(&merchant_id));

    // Policy is raised; merchant is now under-verified but within grace.
    client.set_verification_policy(&t.admin, &2u32);
    assert!(!client.is_merchant_verified(&merchant_id));

    // Merchant acquires the second attestation before the grace period ends.
    client.add_merchant_verification(&t.admin, &merchant_id);

    // Revalidation succeeds and the merchant is verified again.
    assert!(client.revalidate_merchant_status(&merchant_id));
    assert!(client.is_merchant_verified(&merchant_id));
}

/// Once the 30-day grace period elapses without the merchant acquiring the
/// additional attestation, revalidation must permanently fail and the
/// merchant must remain unverified.
#[test]
fn test_policy_threshold_increase_grace_period_expiry() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    client.set_admin(&t.admin);

    client.set_verification_policy(&t.admin, &1u32);
    let merchant_id = register_verified_merchant(&t, &client, 1);
    assert!(client.is_merchant_verified(&merchant_id));

    // Raise the policy and let the 30-day grace period expire.
    client.set_verification_policy(&t.admin, &2u32);
    let thirty_days_secs: u64 = 30 * 24 * 60 * 60;
    t.env
        .ledger()
        .set_timestamp(t.env.ledger().timestamp() + thirty_days_secs + 1);

    // Even if the merchant later acquires the attestation, the grace window
    // has closed and revalidation must not silently re-verify them.
    client.add_merchant_verification(&t.admin, &merchant_id);
    assert!(!client.revalidate_merchant_status(&merchant_id));
    assert!(!client.is_merchant_verified(&merchant_id));
}

/// A merchant already meeting the raised policy must be unaffected by the
/// transition and revalidation must keep them verified.
#[test]
fn test_policy_threshold_increase_leaves_compliant_merchant_verified() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    client.set_admin(&t.admin);

    client.set_verification_policy(&t.admin, &1u32);
    let merchant_id = register_verified_merchant(&t, &client, 2);
    assert!(client.is_merchant_verified(&merchant_id));

    // Raising the policy to 2 leaves this merchant compliant.
    client.set_verification_policy(&t.admin, &2u32);
    assert!(client.is_merchant_verified(&merchant_id));
    assert!(client.revalidate_merchant_status(&merchant_id));
    assert!(client.is_merchant_verified(&merchant_id));
}

/// Revalidating an unknown merchant must surface a deterministic error
/// rather than silently succeeding.
#[test]
fn test_revalidate_merchant_status_unknown_merchant_fails() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    client.set_admin(&t.admin);
    client.set_verification_policy(&t.admin, &1u32);

    assert_eq!(
        client.try_revalidate_merchant_status(&999u64),
        Err(Ok(PermissionError::MerchantNotFound))
    );
}

/// Lowering the policy threshold must not revoke merchants that were already
/// verified; the dynamic check should simply continue to pass.
#[test]
fn test_policy_threshold_decrease_keeps_merchant_verified() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    client.set_admin(&t.admin);

    client.set_verification_policy(&t.admin, &2u32);
    let merchant_id = register_verified_merchant(&t, &client, 2);
    assert!(client.is_merchant_verified(&merchant_id));

    // Governance relaxes the policy back to 1.
    client.set_verification_policy(&t.admin, &1u32);
    assert!(client.is_merchant_verified(&merchant_id));
    assert!(client.revalidate_merchant_status(&merchant_id));
}

/// 10 concurrent transactions executing across different channels (issue #367).
/// Each channel has an independent nonce, so spends on channel A do not block
/// or interfere with channel B. Nonce replay protection holds independently
/// per channel.
#[test]
fn test_execute_spend_via_channel_parallel_channels() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);

    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    client.grant(&t.buyer, &t.agent, &10000, &1000, &merchants, &3600u32);

    let (signing_key, public_key) = test_keypair(&t.env, 42);
    client.set_relayer_key(&t.agent, &public_key);

    let expiration_ledger = t.env.ledger().sequence() + 1000;

    // Execute 10 spends across 10 different channels (0..9), each with nonce 0.
    // All should succeed because each channel has its own independent nonce.
    for channel_id in 0u32..10 {
        let message = ChannelRelayedSpendMessage {
            owner: t.buyer.clone(),
            delegate: t.agent.clone(),
            merchant: t.seller.clone(),
            amount: 10,
            channel_id,
            nonce: 0,
            expiration_ledger,
            epoch: 0,
        };
        let signature = sign_channel_spend(&t.env, &signing_key, message);
        let channel_sig = ChannelSpendSignature {
            channel_id,
            nonce: 0,
            signature,
        };

        client.execute_spend_via_channel(
            &relayer,
            &t.buyer,
            &t.agent,
            &10,
            &t.seller,
            &channel_sig,
            &expiration_ledger,
            &0u32,
        );
    }

    // All 10 spends succeeded (100 total spent)
    assert_eq!(client.get_remaining_allowance(&t.buyer, &t.agent), 9900);

    // Each channel's nonce advanced independently to 1
    for channel_id in 0u32..10 {
        assert_eq!(client.get_channel_nonce(&t.buyer, &t.agent, &channel_id), 1);
    }

    // Relayed (single-lane) nonce is untouched
    assert_eq!(client.get_relayer_nonce(&t.buyer, &t.agent), 0);
}

/// Replay protection: reusing a channel signature fails (issue #367).
#[test]
fn test_execute_spend_via_channel_rejects_replayed_nonce() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);

    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    client.grant(&t.buyer, &t.agent, &1000, &100, &merchants, &3600u32);

    let (signing_key, public_key) = test_keypair(&t.env, 43);
    client.set_relayer_key(&t.agent, &public_key);

    let expiration_ledger = t.env.ledger().sequence() + 1000;

    // First spend on channel 5 succeeds
    let message = ChannelRelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 50,
        channel_id: 5,
        nonce: 0,
        expiration_ledger,
        epoch: 0,
    };
    let signature = sign_channel_spend(&t.env, &signing_key, message);
    let channel_sig = ChannelSpendSignature {
        channel_id: 5,
        nonce: 0,
        signature,
    };

    client.execute_spend_via_channel(
        &relayer,
        &t.buyer,
        &t.agent,
        &50,
        &t.seller,
        &channel_sig,
        &expiration_ledger,
        &0u32,
    );

    // Replay the exact same signature (same channel, same nonce) fails
    assert_eq!(
        client.try_execute_spend_via_channel(
            &relayer,
            &t.buyer,
            &t.agent,
            &50,
            &t.seller,
            &channel_sig,
            &expiration_ledger,
            &0u32,
        ),
        Err(Ok(PermissionError::InvalidNonce))
    );

    // Channel 5 nonce is now 1
    assert_eq!(client.get_channel_nonce(&t.buyer, &t.agent, &5), 1);
    // Other channels unaffected
    assert_eq!(client.get_channel_nonce(&t.buyer, &t.agent, &0), 0);
    assert_eq!(client.get_channel_nonce(&t.buyer, &t.agent, &6), 0);
}

/// Cross-channel independence: spend on channel A doesn't affect channel B's nonce (issue #367).
#[test]
fn test_execute_spend_via_channel_independent_lanes() {
    let t = TestEnv::setup();
    let client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);

    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    client.grant(&t.buyer, &t.agent, &1000, &100, &merchants, &3600u32);

    let (signing_key, public_key) = test_keypair(&t.env, 44);
    client.set_relayer_key(&t.agent, &public_key);

    let expiration_ledger = t.env.ledger().sequence() + 1000;

    // Spend on channel 0
    let msg0 = ChannelRelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 10,
        channel_id: 0,
        nonce: 0,
        expiration_ledger,
        epoch: 0,
    };
    let sig0 = sign_channel_spend(&t.env, &signing_key, msg0);
    client.execute_spend_via_channel(
        &relayer,
        &t.buyer,
        &t.agent,
        &10,
        &t.seller,
        &ChannelSpendSignature {
            channel_id: 0,
            nonce: 0,
            signature: sig0,
        },
        &expiration_ledger,
        &0u32,
    );

    // Spend on channel 1 (should succeed independently)
    let msg1 = ChannelRelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 20,
        channel_id: 1,
        nonce: 0,
        expiration_ledger,
        epoch: 0,
    };
    let sig1 = sign_channel_spend(&t.env, &signing_key, msg1);
    client.execute_spend_via_channel(
        &relayer,
        &t.buyer,
        &t.agent,
        &20,
        &t.seller,
        &ChannelSpendSignature {
            channel_id: 1,
            nonce: 0,
            signature: sig1,
        },
        &expiration_ledger,
        &0u32,
    );

    // Channel 0 nonce advanced, channel 1 nonce advanced, but they're independent
    assert_eq!(client.get_channel_nonce(&t.buyer, &t.agent, &0), 1);
    assert_eq!(client.get_channel_nonce(&t.buyer, &t.agent, &1), 1);

    // Second spend on channel 0 uses nonce 1
    let msg0b = ChannelRelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 10,
        channel_id: 0,
        nonce: 1,
        expiration_ledger,
        epoch: 0,
    };
    let sig0b = sign_channel_spend(&t.env, &signing_key, msg0b);
    client.execute_spend_via_channel(
        &relayer,
        &t.buyer,
        &t.agent,
        &10,
        &t.seller,
        &ChannelSpendSignature {
            channel_id: 0,
            nonce: 1,
            signature: sig0b,
        },
        &expiration_ledger,
        &0u32,
    );

    // Channel 0 nonce now 2, channel 1 still 1
    assert_eq!(client.get_channel_nonce(&t.buyer, &t.agent, &0), 2);
    assert_eq!(client.get_channel_nonce(&t.buyer, &t.agent, &1), 1);
}
