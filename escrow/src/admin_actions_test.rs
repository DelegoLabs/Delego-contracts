use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    IntoVal, TryFromVal,
};

fn setup(env: &Env) -> (EscrowContractClient<'_>, Address, Address) {
    env.mock_all_auths();
    let admin = Address::generate(env);
    let guardian = Address::generate(env);
    let config = EscrowConfig {
        admin: admin.clone(),
        treasury: Address::generate(env),
        fee_bps: 250,
        min_amount: 1,
        max_amount: 1_000_000,
    };
    let address = env.register(EscrowContract, (config,));
    let client = EscrowContractClient::new(env, &address);
    client.set_security_guardian(&admin, &guardian);
    (client, admin, guardian)
}

pub(super) fn approve(
    env: &Env,
    client: &EscrowContractClient<'_>,
    admin: &Address,
    action: AdminAction,
) {
    if client.get_security_guardian().is_none() {
        client.set_security_guardian(admin, &Address::generate(env));
    }
    let id = client.queue_admin_action(admin, &action);
    unlock(env, &client.get_admin_action(&id).unwrap());
    client.execute_admin_action(admin, &id);
}

fn unlock(env: &Env, proposal: &QueuedAdminAction) {
    env.ledger().with_mut(|ledger| {
        ledger.sequence_number = proposal.pending.unlock_ledger;
        ledger.timestamp = proposal.unlock_timestamp;
    });
}

#[test]
fn fee_recipient_waits_for_both_exact_boundaries_and_cannot_replay() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    let before = client.get_fee_config();
    let treasury = Address::generate(&env);
    let id = client.queue_admin_action(&admin, &AdminAction::FeeConfig(300, treasury.clone()));
    let proposal = client.get_admin_action(&id).unwrap();
    assert_eq!(
        proposal.pending.unlock_ledger,
        env.ledger().sequence() + ADMIN_ACTION_DELAY_LEDGERS
    );
    assert_eq!(proposal.unlock_timestamp, env.ledger().timestamp() + 86_400);
    assert_eq!(client.get_fee_config(), before);
    assert_eq!(
        client.try_execute_admin_action(&admin, &id),
        Err(Ok(EscrowError::AdminActionLocked))
    );
    env.ledger().with_mut(|l| {
        l.sequence_number = proposal.pending.unlock_ledger;
        l.timestamp = proposal.unlock_timestamp - 1;
    });
    assert_eq!(
        client.try_execute_admin_action(&admin, &id),
        Err(Ok(EscrowError::AdminActionLocked))
    );
    env.ledger().with_mut(|l| {
        l.sequence_number -= 1;
        l.timestamp += 1;
    });
    assert_eq!(
        client.try_execute_admin_action(&admin, &id),
        Err(Ok(EscrowError::AdminActionLocked))
    );
    unlock(&env, &proposal);
    assert!(client.execute_admin_action(&admin, &id));
    assert_eq!(
        client.get_fee_config(),
        FeeConfig {
            fee_bps: 300,
            treasury
        }
    );
    assert_eq!(client.get_admin_action(&id), None);
    assert_eq!(
        client.try_execute_admin_action(&admin, &id),
        Err(Ok(EscrowError::AdminActionNotFound))
    );
}

#[test]
fn guardian_veto_is_permanent_for_id_and_requeue_restarts_delay() {
    let env = Env::default();
    let (client, admin, guardian) = setup(&env);
    let token = Address::generate(&env);
    let action = AdminAction::AddToken(token.clone());
    let id = client.queue_admin_action(&admin, &action);
    assert_eq!(
        client.try_queue_admin_action(&admin, &action),
        Err(Ok(EscrowError::AdminActionAlreadyQueued))
    );
    let proposal = client.get_admin_action(&id).unwrap();
    unlock(&env, &proposal);
    client.veto_admin_action(&guardian, &id); // Veto remains possible after unlock.
    assert!(client.get_admin_action(&id).unwrap().pending.is_vetoed);
    assert_eq!(
        client.try_execute_admin_action(&admin, &id),
        Err(Ok(EscrowError::AdminActionVetoed))
    );
    assert_eq!(
        client.try_add_token(&admin, &token),
        Err(Ok(EscrowError::AdminActionVetoed))
    );
    assert!(!client.is_token_allowed(&token));
    let next = client.queue_admin_action(&admin, &action);
    assert_ne!(id, next);
    assert_eq!(
        client.get_admin_action(&next).unwrap().unlock_timestamp,
        proposal.unlock_timestamp + 86_400
    );
    assert_eq!(
        client.try_execute_admin_action(&admin, &id),
        Err(Ok(EscrowError::AdminActionVetoed))
    );
    assert_eq!(
        client.try_execute_admin_action(&admin, &next),
        Err(Ok(EscrowError::AdminActionLocked))
    );
}

#[test]
fn legacy_setters_cannot_bypass_or_substitute_payloads() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    let token = Address::generate(&env);
    assert_eq!(
        client.try_add_token(&admin, &token),
        Err(Ok(EscrowError::AdminActionNotFound))
    );
    assert_eq!(
        client.try_remove_token(&admin, &token),
        Err(Ok(EscrowError::AdminActionNotFound))
    );
    assert_eq!(
        client.try_update_fee(&admin, &300),
        Err(Ok(EscrowError::AdminActionNotFound))
    );
    assert_eq!(
        client.try_set_fee_distribution(&admin, &Vec::new(&env)),
        Err(Ok(EscrowError::AdminActionNotFound))
    );
    let id = client.queue_admin_action(&admin, &AdminAction::AddToken(token.clone()));
    unlock(&env, &client.get_admin_action(&id).unwrap());
    assert_eq!(
        client.try_remove_token(&admin, &token),
        Err(Ok(EscrowError::AdminActionNotFound))
    );
    assert_eq!(
        client.try_add_token(&admin, &Address::generate(&env)),
        Err(Ok(EscrowError::AdminActionNotFound))
    );
    assert!(client.add_token(&admin, &token));
    assert!(client.is_token_allowed(&token));
    assert_eq!(
        client.try_add_token(&admin, &token),
        Err(Ok(EscrowError::AdminActionNotFound))
    );
}

#[test]
fn fee_schedule_does_not_auto_execute_from_getter() {
    let env = Env::default();
    let (client, admin, guardian) = setup(&env);
    let before = client.get_fee_config();
    client.schedule_fee_update(&admin, &500, &Address::generate(&env));
    unlock(&env, &client.get_admin_action(&1).unwrap());
    assert_eq!(client.get_fee_config(), before);
    client.veto_admin_action(&guardian, &1);
    assert_eq!(
        client.try_execute_admin_action(&admin, &1),
        Err(Ok(EscrowError::AdminActionVetoed))
    );
    assert_eq!(client.get_fee_config(), before);
}

#[test]
fn token_removal_and_fee_distribution_execute_after_review() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    let token = Address::generate(&env);
    approve(&env, &client, &admin, AdminAction::AddToken(token.clone()));
    assert!(client.is_token_allowed(&token));
    approve(
        &env,
        &client,
        &admin,
        AdminAction::RemoveToken(token.clone()),
    );
    assert!(!client.is_token_allowed(&token));
    let shares = soroban_sdk::vec![
        &env,
        TreasuryShare {
            treasury: Address::generate(&env),
            bps: 100
        }
    ];
    approve(
        &env,
        &client,
        &admin,
        AdminAction::FeeDistribution(shares.clone()),
    );
    assert_eq!(client.get_fee_distribution(), shares);
    approve(
        &env,
        &client,
        &admin,
        AdminAction::FeeDistribution(Vec::new(&env)),
    );
    assert!(client.get_fee_distribution().is_empty());
    approve(&env, &client, &admin, AdminAction::Fee(123));
    assert_eq!(client.get_fee_config().fee_bps, 123);
}

#[test]
fn roles_and_authentication_are_enforced() {
    let env = Env::default();
    let (client, admin, guardian) = setup(&env);
    let outsider = Address::generate(&env);
    let action = AdminAction::Fee(100);
    assert_eq!(
        client.try_queue_admin_action(&outsider, &action),
        Err(Ok(EscrowError::Unauthorized))
    );
    let id = client.queue_admin_action(&admin, &action);
    assert_eq!(
        client.try_veto_admin_action(&admin, &id),
        Err(Ok(EscrowError::Unauthorized))
    );
    assert_eq!(
        client.try_execute_admin_action(&outsider, &id),
        Err(Ok(EscrowError::Unauthorized))
    );
    assert_eq!(
        client.try_set_security_guardian(&admin, &outsider),
        Err(Ok(EscrowError::GuardianAlreadySet))
    );
    env.mock_auths(&[]);
    assert!(client
        .try_queue_admin_action(&admin, &AdminAction::Fee(101))
        .is_err());
    assert!(client.try_veto_admin_action(&guardian, &id).is_err());
    assert!(client.try_execute_admin_action(&admin, &id).is_err());
}

#[test]
fn guardian_rotation_has_the_same_veto_period() {
    let env = Env::default();
    let (client, admin, guardian) = setup(&env);
    let replacement = Address::generate(&env);
    let id = client.queue_admin_action(&admin, &AdminAction::Guardian(replacement.clone()));
    assert_eq!(
        client.try_execute_admin_action(&admin, &id),
        Err(Ok(EscrowError::AdminActionLocked))
    );
    client.veto_admin_action(&guardian, &id);
    assert_eq!(client.get_security_guardian(), Some(guardian));
    approve(
        &env,
        &client,
        &admin,
        AdminAction::Guardian(replacement.clone()),
    );
    assert_eq!(client.get_security_guardian(), Some(replacement));
}

#[test]
fn invalid_inputs_missing_ids_and_overflows_are_rejected() {
    let env = Env::default();
    let (client, admin, guardian) = setup(&env);
    assert_eq!(
        client.try_queue_admin_action(&admin, &AdminAction::Fee(1001)),
        Err(Ok(EscrowError::InvalidFeeBps))
    );
    assert_eq!(
        client.try_execute_admin_action(&admin, &999),
        Err(Ok(EscrowError::AdminActionNotFound))
    );
    assert_eq!(
        client.try_veto_admin_action(&guardian, &999),
        Err(Ok(EscrowError::AdminActionNotFound))
    );
    env.ledger().with_mut(|l| l.timestamp = u64::MAX);
    assert_eq!(
        client.try_queue_admin_action(&admin, &AdminAction::Fee(100)),
        Err(Ok(EscrowError::AdminActionOverflow))
    );
    // Near-u32::MAX ledger testing requires a fresh contract whose storage
    // has not expired during the artificial jump.
    let near_end = Env::default();
    near_end.ledger().with_mut(|l| {
        l.sequence_number = u32::MAX - 2;
        l.min_persistent_entry_ttl = 1;
        l.min_temp_entry_ttl = 1;
        l.max_entry_ttl = 2;
    });
    let (near_client, near_admin, _) = setup(&near_end);
    assert_eq!(
        near_client.try_queue_admin_action(&near_admin, &AdminAction::Fee(100)),
        Err(Ok(EscrowError::AdminActionOverflow))
    );
}

#[test]
fn lifecycle_events_identify_the_proposal() {
    let env = Env::default();
    let (client, admin, guardian) = setup(&env);
    let id = client.queue_admin_action(&admin, &AdminAction::Fee(100));
    let queued = env.events().all().last().unwrap();
    assert_eq!(
        queued.1,
        (symbol_short!("admin"), symbol_short!("queued")).into_val(&env)
    );
    let data = <(u64, QueuedAdminAction)>::try_from_val(&env, &queued.2).unwrap();
    assert_eq!(data, (id, client.get_admin_action(&id).unwrap()));
    client.veto_admin_action(&guardian, &id);
    let vetoed = env.events().all().last().unwrap();
    assert_eq!(
        vetoed.1,
        (symbol_short!("admin"), symbol_short!("vetoed")).into_val(&env)
    );
    let next = client.queue_admin_action(&admin, &AdminAction::Fee(100));
    unlock(&env, &client.get_admin_action(&next).unwrap());
    client.execute_admin_action(&admin, &next);
    let executed = env.events().all().last().unwrap();
    assert_eq!(
        executed.1,
        (symbol_short!("admin"), symbol_short!("executed")).into_val(&env)
    );
    assert_eq!(u64::try_from_val(&env, &executed.2).unwrap(), next);
}

#[test]
fn bootstrap_requires_both_signatures_and_missing_guardian_fails_closed() {
    use soroban_sdk::testutils::{MockAuth, MockAuthInvoke};
    let env = Env::default();
    let admin = Address::generate(&env);
    let guardian = Address::generate(&env);
    let config = EscrowConfig {
        admin: admin.clone(),
        treasury: Address::generate(&env),
        fee_bps: 100,
        min_amount: 1,
        max_amount: 1000,
    };
    let address = env.register(EscrowContract, (config,));
    let client = EscrowContractClient::new(&env, &address);
    env.mock_all_auths();
    assert_eq!(
        client.try_queue_admin_action(&admin, &AdminAction::Fee(200)),
        Err(Ok(EscrowError::GuardianNotSet))
    );
    assert_eq!(
        client.try_update_fee(&admin, &200),
        Err(Ok(EscrowError::AdminActionNotFound))
    );
    assert_eq!(
        client.try_set_security_guardian(&guardian, &guardian),
        Err(Ok(EscrowError::Unauthorized))
    );
    for signer in [&admin, &guardian] {
        env.mock_auths(&[MockAuth {
            address: signer,
            invoke: &MockAuthInvoke {
                contract: &address,
                fn_name: "set_security_guardian",
                args: (admin.clone(), guardian.clone()).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        assert!(client.try_set_security_guardian(&admin, &guardian).is_err());
        assert_eq!(client.get_security_guardian(), None);
    }
    env.mock_all_auths();
    client.set_security_guardian(&admin, &guardian);
    assert_eq!(client.get_security_guardian(), Some(guardian));
}

#[test]
fn roles_are_independent_and_pause_is_limited_to_pausers() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    let pauser = Address::generate(&env);
    assert_eq!(
        client.try_set_create_paused(&pauser, &true),
        Err(Ok(EscrowError::Unauthorized))
    );
    assert!(client.grant_role(&admin, &pauser, &ContractRole::Pauser));
    assert!(client.has_role(&pauser, &ContractRole::Pauser));
    assert!(!client.has_role(&pauser, &ContractRole::FeeManager));
    assert!(client.set_create_paused(&pauser, &true));
    assert!(client.get_create_paused());
    assert!(client.revoke_role(&admin, &pauser, &ContractRole::Pauser));
    assert!(!client.has_role(&pauser, &ContractRole::Pauser));
    assert_eq!(
        client.try_set_create_paused(&pauser, &false),
        Err(Ok(EscrowError::Unauthorized))
    );
    assert!(client.is_admin(&admin));
}
