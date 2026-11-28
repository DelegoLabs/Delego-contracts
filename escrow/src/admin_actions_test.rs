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
            treasury,
            min_fee_floor_stroops: DEFAULT_MIN_FEE_STROOPS,
            max_fee_ceiling_stroops: 0,
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

// ─────────────────────────────────────────────────────────────────────────────
// Task 1: get_fee_tiers / resolve_fee_bps / compute_tiered_fee
// ─────────────────────────────────────────────────────────────────────────────

/// Helper: register the standard 3-tier table via the admin-action queue.
fn register_standard_tiers(env: &Env, client: &EscrowContractClient<'_>, admin: &Address) {
    let mut tiers = soroban_sdk::Vec::new(env);
    tiers.push_back(FeeTierConfig { volume_threshold_stroops: 0,                fee_basis_points: 150 });
    tiers.push_back(FeeTierConfig { volume_threshold_stroops: 10_000_000_000,   fee_basis_points: 100 });
    tiers.push_back(FeeTierConfig { volume_threshold_stroops: 100_000_000_000,  fee_basis_points: 50  });
    approve(env, client, admin, AdminAction::FeeTiers(tiers));
}

#[test]
fn get_fee_tiers_returns_registered_tiers() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    register_standard_tiers(&env, &client, &admin);

    let tiers = client.get_fee_tiers();
    assert_eq!(tiers.len(), 3);
    assert_eq!(tiers.get(0).unwrap().volume_threshold_stroops, 0);
    assert_eq!(tiers.get(0).unwrap().fee_basis_points, 150);
    assert_eq!(tiers.get(2).unwrap().fee_basis_points, 50);
}

#[test]
fn resolve_fee_bps_selects_correct_tier() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    register_standard_tiers(&env, &client, &admin);

    // zero volume → baseline 1.5%
    assert_eq!(client.resolve_fee_bps(&0i128), 150u32);
    // just below silver threshold → still baseline
    assert_eq!(client.resolve_fee_bps(&(10_000_000_000i128 - 1)), 150u32);
    // at silver threshold → 1.0%
    assert_eq!(client.resolve_fee_bps(&10_000_000_000i128), 100u32);
    // just below gold → silver
    assert_eq!(client.resolve_fee_bps(&(100_000_000_000i128 - 1)), 100u32);
    // at gold threshold → 0.5%
    assert_eq!(client.resolve_fee_bps(&100_000_000_000i128), 50u32);
    // above gold → gold
    assert_eq!(client.resolve_fee_bps(&i128::MAX), 50u32);
}

#[test]
fn resolve_fee_bps_falls_back_to_fee_config_when_no_tiers_configured() {
    let env = Env::default();
    // setup() initialises fee_bps = 250 (from the EscrowConfig in setup())
    let (client, _, _) = setup(&env);
    // No FeeTiers action queued — must return the flat 250 bps from FeeConfig.
    assert_eq!(client.resolve_fee_bps(&0i128), 250u32);
    assert_eq!(client.resolve_fee_bps(&i128::MAX), 250u32);
}

#[test]
fn compute_tiered_fee_calculates_correctly_at_each_tier() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    register_standard_tiers(&env, &client, &admin);

    // 1 000 USDC = 1_000_000_000 stroops at 1.5% = 15_000_000
    assert_eq!(
        client.compute_tiered_fee(&1_000_000_000i128, &0i128).unwrap(),
        15_000_000i128,
    );
    // same amount at silver → 1.0% = 10_000_000
    assert_eq!(
        client.compute_tiered_fee(&1_000_000_000i128, &10_000_000_000i128).unwrap(),
        10_000_000i128,
    );
    // same amount at gold → 0.5% = 5_000_000
    assert_eq!(
        client.compute_tiered_fee(&1_000_000_000i128, &100_000_000_000i128).unwrap(),
        5_000_000i128,
    );
}

#[test]
fn compute_tiered_fee_rejects_negative_amount() {
    let env = Env::default();
    let (client, _, _) = setup(&env);
    assert_eq!(
        client.try_compute_tiered_fee(&-1i128, &0i128),
        Err(Ok(EscrowError::InvalidAmount)),
    );
}

#[test]
fn fee_tiers_are_validated_at_queue_time() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);

    // Descending thresholds must be rejected.
    let mut bad_tiers = soroban_sdk::Vec::new(&env);
    bad_tiers.push_back(FeeTierConfig { volume_threshold_stroops: 100, fee_basis_points: 50 });
    bad_tiers.push_back(FeeTierConfig { volume_threshold_stroops: 10,  fee_basis_points: 100 });
    assert_eq!(
        client.try_queue_admin_action(&admin, &AdminAction::FeeTiers(bad_tiers)),
        Err(Ok(EscrowError::InvalidFeeBps)),
    );

    // fee_basis_points > 1000 is refused.
    let mut too_high = soroban_sdk::Vec::new(&env);
    too_high.push_back(FeeTierConfig { volume_threshold_stroops: 0, fee_basis_points: 1001 });
    assert_eq!(
        client.try_queue_admin_action(&admin, &AdminAction::FeeTiers(too_high)),
        Err(Ok(EscrowError::InvalidFeeBps)),
    );

    // Empty tier list is refused.
    assert_eq!(
        client.try_queue_admin_action(&admin, &AdminAction::FeeTiers(soroban_sdk::Vec::new(&env))),
        Err(Ok(EscrowError::InvalidFeeBps)),
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Task 2: submit_evidence_digest
// ─────────────────────────────────────────────────────────────────────────────

/// Setup an escrow in Disputed status so evidence tests have a live order.
fn setup_disputed_escrow(
    env: &Env,
    client: &EscrowContractClient<'_>,
    admin: &Address,
) -> (Address, Address, u64) {
    env.mock_all_auths();

    // Add a token
    let token_admin = Address::generate(env);
    let token_addr = env.register(
        soroban_sdk::token::StellarAssetContract,
        (token_admin.clone(),),
    );
    let token_sac = soroban_sdk::token::StellarAssetClient::new(env, &token_addr);
    approve(env, client, admin, AdminAction::AddToken(token_addr.clone()));

    let buyer  = Address::generate(env);
    let seller = Address::generate(env);

    // Mint funds for buyer
    token_sac.mint(&buyer, &1_000_000);

    // Create + fund the escrow
    let order_id = BytesN::from_array(env, &[7u8; 32]);
    let escrow_id = client.deposit(
        &buyer,
        &seller,
        &token_addr,
        &500_000i128,
        &order_id,
        &1000u32,
        &None,
        &None,
    );

    // Transition to Disputed
    client.dispute(&escrow_id, &buyer);

    (buyer, seller, escrow_id)
}

#[test]
fn submit_evidence_digest_accepts_valid_submission_from_buyer_and_seller() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    let (buyer, seller, escrow_id) = setup_disputed_escrow(&env, &client, &admin);

    let digest_b = BytesN::from_array(&env, &[1u8; 32]);
    let digest_s = BytesN::from_array(&env, &[2u8; 32]);

    let total_b = client.submit_evidence_digest(&escrow_id, &buyer,  &digest_b);
    let total_s = client.submit_evidence_digest(&escrow_id, &seller, &digest_s);

    assert_eq!(total_b, 1u32);
    assert_eq!(total_s, 1u32);

    let recs_b = client.get_evidence_digests(&escrow_id, &buyer);
    let recs_s = client.get_evidence_digests(&escrow_id, &seller);
    assert_eq!(recs_b.len(), 1);
    assert_eq!(recs_s.len(), 1);
    assert_eq!(recs_b.get(0).unwrap().digest, digest_b);
    assert_eq!(recs_s.get(0).unwrap().digest, digest_s);
}

#[test]
fn submit_evidence_digest_enforces_five_digest_cap_per_party() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    let (buyer, _, escrow_id) = setup_disputed_escrow(&env, &client, &admin);

    for i in 0u8..5 {
        let d = BytesN::from_array(&env, &[i; 32]);
        client.submit_evidence_digest(&escrow_id, &buyer, &d);
    }

    // The 6th must fail.
    let d6 = BytesN::from_array(&env, &[9u8; 32]);
    assert_eq!(
        client.try_submit_evidence_digest(&escrow_id, &buyer, &d6),
        Err(Ok(EscrowError::EvidenceDigestLimitExceeded)),
    );
    assert_eq!(client.get_evidence_digests(&escrow_id, &buyer).len(), 5);
}

#[test]
fn submit_evidence_digest_rejects_duplicate_digest_from_same_party() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    let (buyer, _, escrow_id) = setup_disputed_escrow(&env, &client, &admin);

    let d = BytesN::from_array(&env, &[0xABu8; 32]);
    client.submit_evidence_digest(&escrow_id, &buyer, &d);
    assert_eq!(
        client.try_submit_evidence_digest(&escrow_id, &buyer, &d),
        Err(Ok(EscrowError::DuplicateEvidenceDigest)),
    );
}

#[test]
fn submit_evidence_digest_rejects_non_party_submitter() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    let (_, _, escrow_id) = setup_disputed_escrow(&env, &client, &admin);

    let outsider = Address::generate(&env);
    let d = BytesN::from_array(&env, &[0x55u8; 32]);
    assert_eq!(
        client.try_submit_evidence_digest(&escrow_id, &outsider, &d),
        Err(Ok(EscrowError::NotADisputeParty)),
    );
}

#[test]
fn submit_evidence_digest_rejects_submission_after_deadline() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    let (buyer, _, escrow_id) = setup_disputed_escrow(&env, &client, &admin);

    // Advance ledger timestamp past the 7-day (604800 s) deadline.
    env.ledger().with_mut(|l| l.timestamp = l.timestamp + 604_801);

    let d = BytesN::from_array(&env, &[0x11u8; 32]);
    assert_eq!(
        client.try_submit_evidence_digest(&escrow_id, &buyer, &d),
        Err(Ok(EscrowError::DisputeDeadlineExpired)),
    );
}

#[test]
fn submit_evidence_digest_rejects_on_non_disputed_escrow() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    env.mock_all_auths();

    let token_admin = Address::generate(&env);
    let token_addr  = env.register(
        soroban_sdk::token::StellarAssetContract,
        (token_admin.clone(),),
    );
    let token_sac = soroban_sdk::token::StellarAssetClient::new(&env, &token_addr);
    approve(&env, &client, &admin, AdminAction::AddToken(token_addr.clone()));

    let buyer  = Address::generate(&env);
    let seller = Address::generate(&env);
    token_sac.mint(&buyer, &1_000_000);

    let escrow_id = client.deposit(
        &buyer, &seller, &token_addr, &500_000i128,
        &BytesN::from_array(&env, &[0u8; 32]), &1000u32, &None, &None,
    );
    // Escrow is Funded (not Disputed).
    let d = BytesN::from_array(&env, &[0x22u8; 32]);
    assert_eq!(
        client.try_submit_evidence_digest(&escrow_id, &buyer, &d),
        Err(Ok(EscrowError::NotDisputed)),
    );
}

#[test]
fn submit_evidence_digest_emits_correct_event() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::IntoVal;

    let env = Env::default();
    let (client, admin, _) = setup(&env);
    let (buyer, _, escrow_id) = setup_disputed_escrow(&env, &client, &admin);

    let digest = BytesN::from_array(&env, &[0xFFu8; 32]);
    client.submit_evidence_digest(&escrow_id, &buyer, &digest);

    let events = env.events().all();
    let last = events.last().unwrap();
    // Topics: (escrow, evid_sub, escrow_id)
    assert_eq!(
        last.1,
        (symbol_short!("escrow"), symbol_short!("evid_sub"), escrow_id).into_val(&env),
    );
    let ev = EvidenceSubmittedEvent::try_from_val(&env, &last.2).unwrap();
    assert_eq!(ev.escrow_id, escrow_id);
    assert_eq!(ev.submitter, buyer);
    assert_eq!(ev.digest, digest);
    assert_eq!(ev.total_submitted, 1u32);
}

#[test]
fn get_evidence_digests_allows_arbiter_to_verify_records() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    let (buyer, seller, escrow_id) = setup_disputed_escrow(&env, &client, &admin);

    let d1 = BytesN::from_array(&env, &[1u8; 32]);
    let d2 = BytesN::from_array(&env, &[2u8; 32]);
    client.submit_evidence_digest(&escrow_id, &buyer, &d1);
    client.submit_evidence_digest(&escrow_id, &buyer, &d2);

    // Arbiter (any caller) can read back both digests.
    let records = client.get_evidence_digests(&escrow_id, &buyer);
    assert_eq!(records.len(), 2);
    assert_eq!(records.get(0).unwrap().digest, d1);
    assert_eq!(records.get(1).unwrap().digest, d2);
    assert_eq!(records.get(0).unwrap().submitter, buyer);

    // Seller's list is still empty.
    assert_eq!(client.get_evidence_digests(&escrow_id, &seller).len(), 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// Task 4: is_create_effectively_paused / emergency pause with expiry
// ─────────────────────────────────────────────────────────────────────────────

/// Helper: return a fresh token address (registered + whitelisted).
fn fresh_token(env: &Env, client: &EscrowContractClient<'_>, admin: &Address) -> Address {
    let token_admin = Address::generate(env);
    let token_addr = env.register(
        soroban_sdk::token::StellarAssetContract,
        (token_admin,),
    );
    approve(env, client, admin, AdminAction::AddToken(token_addr.clone()));
    token_addr
}

#[test]
fn emergency_pause_with_expiry_unblocks_create_after_ledger() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    env.mock_all_auths();

    let pauser = Address::generate(&env);
    client.grant_role(&admin, &pauser, &ContractRole::Pauser);

    let token = fresh_token(&env, &client, &admin);
    let token_sac = soroban_sdk::token::StellarAssetClient::new(&env, &token);
    let buyer  = Address::generate(&env);
    let seller = Address::generate(&env);
    token_sac.mint(&buyer, &10_000_000);

    // Set a 100-ledger emergency pause.
    let start_ledger = env.ledger().sequence();
    client.set_emergency_pause(&pauser, &true, &100u32);

    // Pause is active → get_create_paused returns true.
    assert!(client.get_create_paused());

    // create must fail while pause is active.
    assert_eq!(
        client.try_deposit(
            &buyer, &seller, &token, &500_000i128,
            &BytesN::from_array(&env, &[0u8; 32]), &200u32, &None, &None,
        ),
        Err(Ok(EscrowError::CreationPaused)),
    );

    // Advance to exactly the expiry ledger.
    env.ledger().with_mut(|l| l.sequence_number = start_ledger + 100);

    // get_create_paused must now return false (ledger >= expires_at).
    assert!(!client.get_create_paused());

    // create must succeed after expiry.
    let id = client.deposit(
        &buyer, &seller, &token, &500_000i128,
        &BytesN::from_array(&env, &[1u8; 32]), &200u32, &None, &None,
    );
    assert!(id > 0);
}

#[test]
fn set_create_paused_without_expiry_blocks_indefinitely() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    env.mock_all_auths();

    let pauser = Address::generate(&env);
    client.grant_role(&admin, &pauser, &ContractRole::Pauser);

    client.set_create_paused(&pauser, &true);
    assert!(client.get_create_paused());

    // Advance ledger far into the future — no expiry, still paused.
    env.ledger().with_mut(|l| l.sequence_number = l.sequence_number + 1_000_000);
    assert!(client.get_create_paused());

    // Lifting the pause explicitly restores creation.
    client.set_create_paused(&pauser, &false);
    assert!(!client.get_create_paused());
}

#[test]
fn emergency_pause_with_zero_duration_does_not_pause() {
    let env = Env::default();
    let (client, admin, _) = setup(&env);
    env.mock_all_auths();

    let pauser = Address::generate(&env);
    client.grant_role(&admin, &pauser, &ContractRole::Pauser);

    // duration=0 → expires_at = None for a true pause; paused=false also sets None.
    // Calling with paused=false and duration=0 must leave create unblocked.
    client.set_emergency_pause(&pauser, &false, &0u32);
    assert!(!client.get_create_paused());
}
