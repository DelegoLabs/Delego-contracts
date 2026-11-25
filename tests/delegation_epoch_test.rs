use delego_delegation_registry::{DelegationRegistry, DelegationRegistryClient, DelegationStatus};
use delego_permissions::{PermissionError, PermissionsContract, PermissionsContractClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, BytesN, Env, Symbol, Vec,
};

struct Setup {
    env: Env,
    permissions: PermissionsContractClient<'static>,
    delegation: DelegationRegistryClient<'static>,
    owner: Address,
    delegate: Address,
    merchant: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let delegate = Address::generate(&env);
    let merchant = Address::generate(&env);

    let permissions_id = env.register(PermissionsContract, ());
    let permissions = PermissionsContractClient::new(&env, &permissions_id);

    let registry_id = env.register(DelegationRegistry, ());
    let delegation = DelegationRegistryClient::new(&env, &registry_id);
    let admin = Address::generate(&env);
    delegation.initialize(&admin);

    Setup {
        env,
        permissions,
        delegation,
        owner,
        delegate,
        merchant,
    }
}

fn grant(s: &Setup) {
    s.permissions.grant(
        &s.owner,
        &s.delegate,
        &1_000i128,
        &500i128,
        &Vec::new(&s.env),
        &1_000u32,
    );
}

#[test]
fn test_epoch_starts_at_zero() {
    let s = setup();
    grant(&s);

    let epoch = s.permissions.get_execution_epoch(&s.owner, &s.delegate);
    assert_eq!(epoch.current_epoch, 0);
}

#[test]
fn test_epoch_increment_on_pause_resume() {
    let s = setup();
    grant(&s);

    s.permissions.pause(&s.owner, &s.delegate);
    let paused = s.permissions.get_execution_epoch(&s.owner, &s.delegate);
    assert_eq!(paused.current_epoch, 1);

    s.permissions.resume(&s.owner, &s.delegate);
    let resumed = s.permissions.get_execution_epoch(&s.owner, &s.delegate);
    assert_eq!(resumed.current_epoch, 2);
    assert!(resumed.epoch_started_ledger >= paused.epoch_started_ledger);
}

#[test]
fn test_stale_epoch_rejected() {
    let s = setup();
    grant(&s);

    s.permissions.pause(&s.owner, &s.delegate);

    let expiration_ledger = s.env.ledger().sequence() + 100;
    let signature = BytesN::<64>::from_array(&s.env, &[0u8; 64]);
    let relayer = Address::generate(&s.env);

    let result = s.permissions.try_execute_spend_via_relayer(
        &relayer,
        &s.owner,
        &s.delegate,
        &100i128,
        &s.merchant,
        &0u64,
        &expiration_ledger,
        &0u32,
        &signature,
    );

    assert_eq!(result, Err(Ok(PermissionError::StaleEpoch)));
}

#[test]
fn test_epoch_advances_again_after_resume() {
    let s = setup();
    grant(&s);

    s.permissions.pause(&s.owner, &s.delegate);
    s.permissions.resume(&s.owner, &s.delegate);

    let expiration_ledger = s.env.ledger().sequence() + 100;
    let signature = BytesN::<64>::from_array(&s.env, &[0u8; 64]);
    let relayer = Address::generate(&s.env);

    let result = s.permissions.try_execute_spend_via_relayer(
        &relayer,
        &s.owner,
        &s.delegate,
        &100i128,
        &s.merchant,
        &0u64,
        &expiration_ledger,
        &1u32,
        &signature,
    );

    assert_eq!(result, Err(Ok(PermissionError::StaleEpoch)));
}

#[test]
fn test_delegation_registry_pause_resume_roundtrip() {
    let s = setup();

    let permissions_id = s.permissions.address.clone();
    let agent_id = BytesN::<32>::from_array(&s.env, &[7u8; 32]);
    let label = Symbol::new(&s.env, "epochtest");

    let delegation_id =
        s.delegation
            .create_delegation(&s.owner, &agent_id, &permissions_id, &label, &1_000u32);

    let active = s.delegation.get_delegation(&delegation_id);
    assert_eq!(active.status, DelegationStatus::Active);

    s.delegation.pause_delegation(&delegation_id);
    let paused = s.delegation.get_delegation(&delegation_id);
    assert_eq!(paused.status, DelegationStatus::Paused);

    s.delegation.resume_delegation(&delegation_id);
    let resumed = s.delegation.get_delegation(&delegation_id);
    assert_eq!(resumed.status, DelegationStatus::Active);
    assert!(resumed.version > paused.version);
}
