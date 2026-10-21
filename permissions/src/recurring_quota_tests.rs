use crate::*;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    Address, Env, Vec,
};

// At five seconds per ledger, this approximates a week. Accounting depends
// only on the ledger sequence, never on a wall-clock estimate.
const WEEK: u32 = 120_960;

fn setup(limit: i128, cap: i128) -> (Env, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    // Keep the contract and persistent fixture records live across the
    // simulated weeks; archival/restoration is separate from quota renewal.
    env.ledger().set_max_entry_ttl(WEEK * 20);
    env.ledger().set_min_persistent_entry_ttl(WEEK * 12);
    env.ledger().set_sequence_number(WEEK - 1);
    let contract = env.register(PermissionsContract, ());
    let owner = Address::generate(&env);
    let delegate = Address::generate(&env);
    let merchant = Address::generate(&env);
    let client = PermissionsContractClient::new(&env, &contract);
    client.grant(
        &owner,
        &delegate,
        &limit,
        &limit,
        &Vec::new(&env),
        &(WEEK * 8),
    );
    client.configure_recurring_quota(&owner, &delegate, &WEEK, &limit, &cap);
    (env, contract, owner, delegate, merchant)
}

#[test]
fn recurring_boundary_skipped_weeks_and_lifetime_exhaustion() {
    let (env, contract, owner, delegate, merchant) = setup(100, 250);
    let client = PermissionsContractClient::new(&env, &contract);
    client.execute_spend(&owner, &delegate, &100, &merchant);
    assert_eq!(client.get_remaining_allowance(&owner, &delegate), 0);
    assert!(client
        .try_execute_spend(&owner, &delegate, &1, &merchant)
        .is_err());
    env.ledger().set_sequence_number(WEEK);
    assert_eq!(client.get_remaining_allowance(&owner, &delegate), 100);
    assert_eq!(
        client.try_can_spend(&owner, &delegate, &100, &merchant),
        Ok(Ok(()))
    );
    client.execute_spend(&owner, &delegate, &75, &merchant);
    // Skipping three weeks replenishes only one epoch, without carrying 25.
    env.ledger().set_sequence_number(WEEK * 4);
    assert_eq!(client.get_remaining_allowance(&owner, &delegate), 75);
    client.execute_spend(&owner, &delegate, &75, &merchant);
    env.ledger().set_sequence_number(WEEK * 5);
    assert_eq!(client.get_remaining_allowance(&owner, &delegate), 0);
    assert_eq!(
        client.try_execute_spend(&owner, &delegate, &1, &merchant),
        Err(Ok(PermissionError::ExceedsTotalLimit))
    );
    assert_eq!(
        client
            .get_recurring_quota(&owner, &delegate)
            .unwrap()
            .cumulative_lifetime_spent,
        250
    );
}

#[test]
fn recurring_partial_allowance_does_not_roll_over() {
    let (env, contract, owner, delegate, merchant) = setup(100, 1_000);
    let client = PermissionsContractClient::new(&env, &contract);
    client.execute_spend(&owner, &delegate, &20, &merchant);
    env.ledger().set_sequence_number(WEEK * 3);
    assert_eq!(client.get_remaining_allowance(&owner, &delegate), 100);
    assert_eq!(client.get_allowance_detail(&owner, &delegate).spent, 0);
    assert_eq!(
        client
            .preview_spend(&owner, &delegate, &80, &merchant)
            .remaining_after,
        20
    );
    client.execute_spend(&owner, &delegate, &100, &merchant);
    assert_eq!(client.get_remaining_allowance(&owner, &delegate), 0);
}

#[test]
fn recurring_i128_boundary_and_invalid_amounts_leave_counters_unchanged() {
    let (env, contract, owner, delegate, merchant) = setup(i128::MAX, i128::MAX);
    let client = PermissionsContractClient::new(&env, &contract);
    for amount in [0, -1, i128::MIN] {
        assert_eq!(
            client.try_execute_spend(&owner, &delegate, &amount, &merchant),
            Err(Ok(PermissionError::InvalidParam))
        );
    }
    client.execute_spend(&owner, &delegate, &i128::MAX, &merchant);
    env.ledger().set_sequence_number(WEEK);
    assert_eq!(
        client.try_execute_spend(&owner, &delegate, &1, &merchant),
        Err(Ok(PermissionError::ExceedsTotalLimit))
    );
    assert_eq!(
        client
            .get_recurring_quota(&owner, &delegate)
            .unwrap()
            .cumulative_lifetime_spent,
        i128::MAX
    );
}

#[test]
fn recurring_failed_policy_check_does_not_consume_or_renew_storage() {
    let (env, contract, owner, delegate, merchant) = setup(100, 150);
    let client = PermissionsContractClient::new(&env, &contract);
    client.execute_spend(&owner, &delegate, &50, &merchant);
    client.pause(&owner, &delegate);
    env.ledger().set_sequence_number(WEEK);
    assert!(client
        .try_execute_spend(&owner, &delegate, &100, &merchant)
        .is_err());
    let stored = env.as_contract(&contract, || {
        recurring_quota::load(&env, &owner, &delegate).unwrap()
    });
    assert_eq!(stored.config.current_epoch_index, 0);
    assert_eq!(stored.config.cumulative_lifetime_spent, 50);
    assert_eq!(stored.epoch_spent, 50);
    client.resume(&owner, &delegate);
    client.execute_spend(&owner, &delegate, &100, &merchant);
    assert_eq!(client.get_remaining_allowance(&owner, &delegate), 0);
}

#[test]
fn recurring_regrant_and_reconfiguration_cannot_reset_lifetime() {
    let (env, contract, owner, delegate, merchant) = setup(100, 100);
    let client = PermissionsContractClient::new(&env, &contract);
    client.execute_spend(&owner, &delegate, &100, &merchant);
    client.re_grant(&owner, &delegate, &100, &100, &Vec::new(&env), &(WEEK * 8));
    env.ledger().set_sequence_number(WEEK);
    assert_eq!(client.get_remaining_allowance(&owner, &delegate), 0);
    assert_eq!(
        client.try_configure_recurring_quota(&owner, &delegate, &WEEK, &100, &1_000),
        Err(Ok(PermissionError::AlreadyGranted))
    );
    assert!(client
        .try_execute_spend(&owner, &delegate, &1, &merchant)
        .is_err());
    let replacement = Address::generate(&env);
    assert_eq!(
        client.try_transfer_permission(&owner, &delegate, &replacement),
        Err(Ok(PermissionError::InvalidParam))
    );
}

#[test]
fn recurring_child_spends_charge_parent_epoch_and_lifetime() {
    let (env, contract, owner, delegate, merchant) = setup(100, 150);
    let client = PermissionsContractClient::new(&env, &contract);
    let child = Address::generate(&env);
    client.grant_child(
        &owner,
        &delegate,
        &child,
        &100,
        &100,
        &Vec::new(&env),
        &(WEEK * 6),
    );
    client.execute_spend(&delegate, &child, &60, &merchant);
    assert_eq!(client.get_remaining_allowance(&owner, &delegate), 40);
    env.ledger().set_sequence_number(WEEK);
    client.execute_spend(&owner, &delegate, &90, &merchant);
    assert_eq!(
        client.try_execute_spend(&delegate, &child, &1, &merchant),
        Err(Ok(PermissionError::ExceedsParentLimit))
    );
    assert_eq!(
        client
            .get_recurring_quota(&owner, &delegate)
            .unwrap()
            .cumulative_lifetime_spent,
        150
    );
}

#[test]
fn recurring_configuration_validates_inputs_and_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    let contract = env.register(PermissionsContract, ());
    let client = PermissionsContractClient::new(&env, &contract);
    let owner = Address::generate(&env);
    let delegate = Address::generate(&env);
    assert!(client.get_recurring_quota(&owner, &delegate).is_none());
    assert_eq!(
        client.try_configure_recurring_quota(&owner, &delegate, &WEEK, &100, &100),
        Err(Ok(PermissionError::PermissionNotFound))
    );
    client.grant(&owner, &delegate, &100, &100, &Vec::new(&env), &(WEEK * 8));
    for (duration, quota, cap) in [
        (0, 100, 100),
        (WEEK, 0, 100),
        (WEEK, -1, 100),
        (WEEK, 100, 0),
        (WEEK, 101, 100),
    ] {
        assert_eq!(
            client.try_configure_recurring_quota(&owner, &delegate, &duration, &quota, &cap),
            Err(Ok(PermissionError::InvalidParam))
        );
    }
    let before = env.events().all().len();
    client.configure_recurring_quota(&owner, &delegate, &WEEK, &100, &50);
    assert!(env.events().all().len() > before);
    assert_eq!(client.get_remaining_allowance(&owner, &delegate), 50);
}

#[test]
fn recurring_configuration_requires_owner_signature() {
    let env = Env::default();
    env.mock_all_auths();
    let contract = env.register(PermissionsContract, ());
    let client = PermissionsContractClient::new(&env, &contract);
    let owner = Address::generate(&env);
    let delegate = Address::generate(&env);
    client.grant(&owner, &delegate, &100, &100, &Vec::new(&env), &WEEK);
    env.mock_auths(&[]);
    assert!(matches!(
        client.try_configure_recurring_quota(&owner, &delegate, &WEEK, &100, &200),
        Err(Err(_))
    ));
}

#[test]
fn recurring_relayer_and_channel_share_lifetime_and_rollback_nonce_on_failure() {
    use ed25519_dalek::{Signer, SigningKey};
    use soroban_sdk::{xdr::ToXdr, BytesN};

    let (env, contract, owner, delegate, merchant) = setup(100, 150);
    let client = PermissionsContractClient::new(&env, &contract);
    let relayer = Address::generate(&env);
    let signing_key = SigningKey::from_bytes(&[42; 32]);
    client.set_relayer_key(
        &delegate,
        &BytesN::from_array(&env, &signing_key.verifying_key().to_bytes()),
    );
    let epoch = client.get_execution_epoch(&owner, &delegate).current_epoch;
    let expiration_ledger = WEEK * 2;
    let message = RelayedSpendMessage {
        owner: owner.clone(),
        delegate: delegate.clone(),
        merchant: merchant.clone(),
        amount: 100,
        nonce: 0,
        expiration_ledger,
        epoch,
    };
    let signature = BytesN::from_array(
        &env,
        &signing_key
            .sign(&message.to_xdr(&env).iter().collect::<std::vec::Vec<_>>())
            .to_bytes(),
    );
    client.execute_spend_via_relayer(
        &relayer,
        &owner,
        &delegate,
        &100,
        &merchant,
        &0,
        &expiration_ledger,
        &epoch,
        &signature,
    );
    env.ledger().set_sequence_number(WEEK);
    for (amount, nonce, allowed) in [(50, 0, true), (1, 1, false)] {
        let message = ChannelRelayedSpendMessage {
            owner: owner.clone(),
            delegate: delegate.clone(),
            merchant: merchant.clone(),
            amount,
            channel_id: 3,
            nonce,
            expiration_ledger,
            epoch,
        };
        let signature = BytesN::from_array(
            &env,
            &signing_key
                .sign(&message.to_xdr(&env).iter().collect::<std::vec::Vec<_>>())
                .to_bytes(),
        );
        let result = client.try_execute_spend_via_channel(
            &relayer,
            &owner,
            &delegate,
            &amount,
            &merchant,
            &ChannelSpendSignature {
                channel_id: 3,
                nonce,
                signature,
            },
            &expiration_ledger,
            &epoch,
        );
        assert_eq!(result.is_ok(), allowed);
    }
    assert_eq!(client.get_channel_nonce(&owner, &delegate, &3), 1);
    assert_eq!(
        client
            .get_recurring_quota(&owner, &delegate)
            .unwrap()
            .cumulative_lifetime_spent,
        150
    );
}

#[test]
fn recurring_expiry_still_blocks_renewed_allowance() {
    let (env, contract, owner, delegate, merchant) = setup(100, 1_000);
    let client = PermissionsContractClient::new(&env, &contract);
    env.ledger().set_sequence_number(WEEK * 9);
    assert_eq!(
        client.try_execute_spend(&owner, &delegate, &1, &merchant),
        Err(Ok(PermissionError::Expired))
    );
    assert_eq!(
        client
            .get_recurring_quota(&owner, &delegate)
            .unwrap()
            .cumulative_lifetime_spent,
        0
    );
}
