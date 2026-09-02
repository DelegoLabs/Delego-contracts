use super::{MarketplaceContract, MarketplaceError, MerchantStatus, RegisterParams, Verifier};
use soroban_sdk::{symbol_short, testutils::Address as _, Address, Env, String};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn setup() -> (Env, super::MarketplaceContractClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MarketplaceContract, ());
    let client = super::MarketplaceContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin);
    (env, client, admin)
}

fn make_params(env: &Env, name: &str) -> RegisterParams {
    RegisterParams {
        name: String::from_str(env, name),
        description: String::from_str(env, "A merchant"),
        category: symbol_short!("retail"),
        image_url: String::from_str(env, "https://example.com/img.png"),
        metadata: None,
        required_verifications: 1,
    }
}

fn make_verifier(env: &Env, address: Address) -> Verifier {
    Verifier {
        address,
        label: symbol_short!("v1"),
        registered_at: env.ledger().timestamp(),
    }
}

// ── Initialisation ────────────────────────────────────────────────────────────

#[test]
fn test_initialize_sets_admin() {
    let (_env, client, admin) = setup();
    assert_eq!(client.get_admin(), admin);
}

#[test]
fn test_double_initialize_fails() {
    let (_env, client, admin) = setup();
    let result = client.try_initialize(&admin);
    assert_eq!(result, Err(Ok(MarketplaceError::AlreadyInitialized)));
}

#[test]
fn test_default_metadata_cooldown() {
    let (_env, client, _admin) = setup();
    // Default is 24 hours = 86 400 seconds.
    assert_eq!(client.get_metadata_cooldown(), 86_400u64);
}

// ── Merchant registration ─────────────────────────────────────────────────────

#[test]
fn test_register_merchant_basic() {
    let (env, client, _admin) = setup();
    let merchant = Address::generate(&env);
    let params = make_params(&env, "ShopA");
    let id = client.register_merchant(&merchant, &params);
    assert_eq!(id, 1);
    let m = client.get_merchant(&id);
    assert_eq!(m.name, String::from_str(&env, "ShopA"));
    assert_eq!(m.status, MerchantStatus::Registered);
    assert!(!m.verified);
}

#[test]
fn test_register_merchant_increments_id() {
    let (env, client, _admin) = setup();
    let m1 = Address::generate(&env);
    let m2 = Address::generate(&env);
    let id1 = client.register_merchant(&m1, &make_params(&env, "ShopA"));
    let id2 = client.register_merchant(&m2, &make_params(&env, "ShopB"));
    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
}

#[test]
fn test_register_merchant_duplicate_name_fails() {
    let (env, client, _admin) = setup();
    let m1 = Address::generate(&env);
    let m2 = Address::generate(&env);
    client.register_merchant(&m1, &make_params(&env, "Unique"));
    let result = client.try_register_merchant(&m2, &make_params(&env, "Unique"));
    assert_eq!(result, Err(Ok(MarketplaceError::NameAlreadyTaken)));
}

#[test]
fn test_register_merchant_invalid_required_verifications_zero() {
    let (env, client, _admin) = setup();
    let merchant = Address::generate(&env);
    let mut params = make_params(&env, "BadVerif");
    params.required_verifications = 0;
    let result = client.try_register_merchant(&merchant, &params);
    assert_eq!(
        result,
        Err(Ok(MarketplaceError::InvalidRequiredVerifications))
    );
}

#[test]
fn test_is_name_available_true_and_false() {
    let (env, client, _admin) = setup();
    let merchant = Address::generate(&env);
    assert!(client.is_name_available(&String::from_str(&env, "Free")));
    client.register_merchant(&merchant, &make_params(&env, "Free"));
    assert!(!client.is_name_available(&String::from_str(&env, "Free")));
}

// ── Profile updates ───────────────────────────────────────────────────────────

#[test]
fn test_update_merchant_profile_by_owner() {
    let (env, client, _admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "OrigName"));
    client.update_merchant_profile(
        &id,
        &merchant,
        &String::from_str(&env, "NewName"),
        &String::from_str(&env, "New desc"),
        &String::from_str(&env, "https://new.img"),
    );
    let m = client.get_merchant(&id);
    assert_eq!(m.name, String::from_str(&env, "NewName"));
    assert_eq!(m.description, String::from_str(&env, "New desc"));
}

#[test]
fn test_update_merchant_profile_by_admin() {
    let (env, client, admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "AdminUpdate"));
    client.update_merchant_profile(
        &id,
        &admin,
        &String::from_str(&env, "AdminUpdated"),
        &String::from_str(&env, "Desc"),
        &String::from_str(&env, "https://img"),
    );
    assert_eq!(
        client.get_merchant(&id).name,
        String::from_str(&env, "AdminUpdated")
    );
}

#[test]
fn test_update_merchant_profile_unauthorized() {
    let (env, client, _admin) = setup();
    let merchant = Address::generate(&env);
    let stranger = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "AuthTest"));
    let result = client.try_update_merchant_profile(
        &id,
        &stranger,
        &String::from_str(&env, "Hacked"),
        &String::from_str(&env, "Desc"),
        &String::from_str(&env, "https://img"),
    );
    assert_eq!(result, Err(Ok(MarketplaceError::NotOwner)));
}

#[test]
fn test_update_profile_rename_clears_old_name() {
    let (env, client, _admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "OldName"));
    client.update_merchant_profile(
        &id,
        &merchant,
        &String::from_str(&env, "NewName"),
        &String::from_str(&env, "Desc"),
        &String::from_str(&env, "https://img"),
    );
    // Old name should now be available again.
    assert!(client.is_name_available(&String::from_str(&env, "OldName")));
    assert!(!client.is_name_available(&String::from_str(&env, "NewName")));
}

// ── Metadata cooldown ─────────────────────────────────────────────────────────

#[test]
fn test_update_metadata_first_time_succeeds() {
    let (env, client, _admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "MetaTest"));
    client.update_metadata(&id, &merchant, &Some(String::from_str(&env, "QmHash1")));
    let m = client.get_merchant(&id);
    assert_eq!(m.metadata, Some(String::from_str(&env, "QmHash1")));
}

#[test]
fn test_update_metadata_cooldown_blocks_rapid_update() {
    use soroban_sdk::testutils::Ledger;
    let (env, client, _admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "CooldownTest"));
    // Set a short cooldown for the test.
    let admin_addr = client.get_admin();
    client.set_metadata_cooldown(&admin_addr, &120);

    env.ledger().set_timestamp(1000);
    client.update_metadata(&id, &merchant, &Some(String::from_str(&env, "QmFirst")));

    // Advance only 60 seconds — still within 120s cooldown.
    env.ledger().set_timestamp(1060);
    let result =
        client.try_update_metadata(&id, &merchant, &Some(String::from_str(&env, "QmSecond")));
    assert_eq!(result, Err(Ok(MarketplaceError::MetadataLockActive)));
}

#[test]
fn test_update_metadata_cooldown_passes_after_window() {
    use soroban_sdk::testutils::Ledger;
    let (env, client, admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "CoolPass"));
    client.set_metadata_cooldown(&admin, &120);

    env.ledger().set_timestamp(1000);
    client.update_metadata(&id, &merchant, &Some(String::from_str(&env, "QmFirst")));

    // Advance past the 120s cooldown.
    env.ledger().set_timestamp(1200);
    client.update_metadata(&id, &merchant, &Some(String::from_str(&env, "QmSecond")));
    assert_eq!(
        client.get_merchant(&id).metadata,
        Some(String::from_str(&env, "QmSecond"))
    );
}

#[test]
fn test_update_metadata_admin_bypasses_cooldown() {
    use soroban_sdk::testutils::Ledger;
    let (env, client, admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "AdminBypass"));
    client.set_metadata_cooldown(&admin, &3600);

    env.ledger().set_timestamp(1000);
    client.update_metadata(&id, &merchant, &Some(String::from_str(&env, "QmFirst")));

    // Admin bypasses the cooldown.
    env.ledger().set_timestamp(1010);
    client.update_metadata(&id, &admin, &Some(String::from_str(&env, "QmAdmin")));
    assert_eq!(
        client.get_merchant(&id).metadata,
        Some(String::from_str(&env, "QmAdmin"))
    );
}

#[test]
fn test_set_metadata_cooldown_invalid_below_min() {
    let (_env, client, admin) = setup();
    let result = client.try_set_metadata_cooldown(&admin, &30);
    assert_eq!(result, Err(Ok(MarketplaceError::InvalidCooldown)));
}

#[test]
fn test_set_metadata_cooldown_invalid_above_max() {
    let (_env, client, admin) = setup();
    let result = client.try_set_metadata_cooldown(&admin, &3_000_000);
    assert_eq!(result, Err(Ok(MarketplaceError::InvalidCooldown)));
}

#[test]
fn test_set_metadata_cooldown_valid_boundary_values() {
    let (_env, client, admin) = setup();
    client.set_metadata_cooldown(&admin, &60);
    assert_eq!(client.get_metadata_cooldown(), 60u64);
    client.set_metadata_cooldown(&admin, &2_592_000);
    assert_eq!(client.get_metadata_cooldown(), 2_592_000u64);
}

// ── Verifier management ───────────────────────────────────────────────────────

#[test]
fn test_add_and_get_verifiers() {
    let (env, client, admin) = setup();
    let v_addr = Address::generate(&env);
    let verifier = make_verifier(&env, v_addr.clone());
    client.add_verifier(&admin, &verifier);
    let verifiers = client.get_verifiers();
    assert_eq!(verifiers.len(), 1);
    assert_eq!(verifiers.get(0).unwrap().address, v_addr);
}

#[test]
fn test_add_verifier_unauthorized() {
    let (env, client, _admin) = setup();
    let stranger = Address::generate(&env);
    let v_addr = Address::generate(&env);
    let verifier = make_verifier(&env, v_addr);
    let result = client.try_add_verifier(&stranger, &verifier);
    assert_eq!(result, Err(Ok(MarketplaceError::Unauthorized)));
}

#[test]
fn test_add_verifier_duplicate_fails() {
    let (env, client, admin) = setup();
    let v_addr = Address::generate(&env);
    let verifier = make_verifier(&env, v_addr.clone());
    client.add_verifier(&admin, &verifier.clone());
    let result = client.try_add_verifier(&admin, &verifier);
    assert_eq!(result, Err(Ok(MarketplaceError::VerifierAlreadyRegistered)));
}

#[test]
fn test_remove_verifier() {
    let (env, client, admin) = setup();
    let v_addr = Address::generate(&env);
    let verifier = make_verifier(&env, v_addr.clone());
    client.add_verifier(&admin, &verifier);
    client.remove_verifier(&admin, &v_addr);
    assert_eq!(client.get_verifiers().len(), 0);
}

#[test]
fn test_remove_verifier_not_found() {
    let (env, client, admin) = setup();
    let unknown = Address::generate(&env);
    let result = client.try_remove_verifier(&admin, &unknown);
    assert_eq!(result, Err(Ok(MarketplaceError::VerifierNotFound)));
}

// ── Verification lifecycle ────────────────────────────────────────────────────

#[test]
fn test_verify_merchant_transitions_to_verified_at_threshold() {
    let (env, client, admin) = setup();
    let merchant = Address::generate(&env);
    let v_addr = Address::generate(&env);
    client.add_verifier(&admin, &make_verifier(&env, v_addr.clone()));

    let id = client.register_merchant(&merchant, &make_params(&env, "VerifyMe"));
    client.verify_merchant(&id, &v_addr);
    let m = client.get_merchant(&id);
    assert!(m.verified);
    assert_eq!(m.status, MerchantStatus::Verified);
}

#[test]
fn test_verify_merchant_not_verified_until_threshold() {
    let (env, client, admin) = setup();
    let merchant = Address::generate(&env);
    let v1 = Address::generate(&env);
    let v2 = Address::generate(&env);
    client.add_verifier(&admin, &make_verifier(&env, v1.clone()));
    client.add_verifier(
        &admin,
        &Verifier {
            address: v2.clone(),
            label: symbol_short!("v2"),
            registered_at: 0,
        },
    );

    let mut params = make_params(&env, "TwoVerif");
    params.required_verifications = 2;
    let id = client.register_merchant(&merchant, &params);

    // Only one attestation — not verified yet.
    client.verify_merchant(&id, &v1);
    let m = client.get_merchant(&id);
    assert!(!m.verified);
    assert_eq!(m.status, MerchantStatus::Registered);

    // Second attestation — threshold reached.
    client.verify_merchant(&id, &v2);
    let m2 = client.get_merchant(&id);
    assert!(m2.verified);
    assert_eq!(m2.status, MerchantStatus::Verified);
}

#[test]
fn test_verify_merchant_duplicate_verifier_fails() {
    let (env, client, admin) = setup();
    let merchant = Address::generate(&env);
    let v_addr = Address::generate(&env);
    client.add_verifier(&admin, &make_verifier(&env, v_addr.clone()));

    let mut params = make_params(&env, "DupVerif");
    params.required_verifications = 2;
    let id = client.register_merchant(&merchant, &params);
    client.verify_merchant(&id, &v_addr);
    let result = client.try_verify_merchant(&id, &v_addr);
    assert_eq!(result, Err(Ok(MarketplaceError::AlreadyVerified)));
}

#[test]
fn test_verify_merchant_unregistered_verifier_fails() {
    let (env, client, _admin) = setup();
    let merchant = Address::generate(&env);
    let fake_verifier = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "Fake"));
    let result = client.try_verify_merchant(&id, &fake_verifier);
    assert_eq!(result, Err(Ok(MarketplaceError::VerifierNotFound)));
}

#[test]
fn test_revoke_verification_resets_to_registered() {
    let (env, client, admin) = setup();
    let merchant = Address::generate(&env);
    let v_addr = Address::generate(&env);
    client.add_verifier(&admin, &make_verifier(&env, v_addr.clone()));

    let id = client.register_merchant(&merchant, &make_params(&env, "Revoke"));
    client.verify_merchant(&id, &v_addr);
    assert_eq!(client.get_merchant(&id).status, MerchantStatus::Verified);

    client.revoke_verification(&admin, &id);
    let m = client.get_merchant(&id);
    assert!(!m.verified);
    assert_eq!(m.status, MerchantStatus::Registered);
}

#[test]
fn test_revoke_verification_on_unverified_fails() {
    let (env, client, admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "Unver"));
    let result = client.try_revoke_verification(&admin, &id);
    assert_eq!(result, Err(Ok(MarketplaceError::MerchantNotVerified)));
}

// ── Commission ────────────────────────────────────────────────────────────────

#[test]
fn test_set_and_get_commission() {
    let (env, client, _admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "CommTest"));
    client.set_merchant_commission(&id, &merchant, &250);
    assert_eq!(client.get_commission(&id), 250u32);
}

#[test]
fn test_set_commission_exceeds_max_fails() {
    let (env, client, _admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "CommMax"));
    let result = client.try_set_merchant_commission(&id, &merchant, &10_001);
    assert_eq!(result, Err(Ok(MarketplaceError::InvalidCommission)));
}

#[test]
fn test_set_commission_at_max_boundary() {
    let (env, client, _admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "CommBound"));
    client.set_merchant_commission(&id, &merchant, &10_000);
    assert_eq!(client.get_commission(&id), 10_000u32);
}

#[test]
fn test_set_commission_unauthorized() {
    let (env, client, _admin) = setup();
    let merchant = Address::generate(&env);
    let stranger = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "CommAuth"));
    let result = client.try_set_merchant_commission(&id, &stranger, &100);
    assert_eq!(result, Err(Ok(MarketplaceError::NotOwner)));
}

// ── Moderation lifecycle ──────────────────────────────────────────────────────

#[test]
fn test_suspend_merchant() {
    let (env, client, admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "SuspendMe"));
    client.suspend_merchant(&admin, &id);
    assert_eq!(client.get_merchant(&id).status, MerchantStatus::Suspended);
}

#[test]
fn test_unsuspend_merchant_restores_registered() {
    let (env, client, admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "Unsuspend"));
    client.suspend_merchant(&admin, &id);
    client.unsuspend_merchant(&admin, &id);
    assert_eq!(client.get_merchant(&id).status, MerchantStatus::Registered);
}

#[test]
fn test_unsuspend_verified_merchant_restores_verified() {
    let (env, client, admin) = setup();
    let merchant = Address::generate(&env);
    let v_addr = Address::generate(&env);
    client.add_verifier(&admin, &make_verifier(&env, v_addr.clone()));

    let id = client.register_merchant(&merchant, &make_params(&env, "VerSusp"));
    client.verify_merchant(&id, &v_addr);
    client.suspend_merchant(&admin, &id);
    client.unsuspend_merchant(&admin, &id);
    assert_eq!(client.get_merchant(&id).status, MerchantStatus::Verified);
}

#[test]
fn test_close_merchant() {
    let (env, client, admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "CloseMe"));
    client.close_merchant(&admin, &id, &symbol_short!("tos_viol"));
    assert_eq!(client.get_merchant(&id).status, MerchantStatus::Closed);
}

#[test]
fn test_close_already_closed_fails() {
    let (env, client, admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "Already"));
    client.close_merchant(&admin, &id, &symbol_short!("reason"));
    let result = client.try_close_merchant(&admin, &id, &symbol_short!("reason"));
    assert_eq!(result, Err(Ok(MarketplaceError::MerchantFrozen)));
}

#[test]
fn test_suspended_merchant_blocks_profile_update() {
    let (env, client, admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "Frozen"));
    client.suspend_merchant(&admin, &id);
    let result = client.try_update_merchant_profile(
        &id,
        &merchant,
        &String::from_str(&env, "NewName"),
        &String::from_str(&env, "Desc"),
        &String::from_str(&env, "https://img"),
    );
    assert_eq!(result, Err(Ok(MarketplaceError::MerchantFrozen)));
}

#[test]
fn test_closed_merchant_blocks_verify() {
    let (env, client, admin) = setup();
    let merchant = Address::generate(&env);
    let v_addr = Address::generate(&env);
    client.add_verifier(&admin, &make_verifier(&env, v_addr.clone()));
    let id = client.register_merchant(&merchant, &make_params(&env, "ClosedVerif"));
    client.close_merchant(&admin, &id, &symbol_short!("reason"));
    let result = client.try_verify_merchant(&id, &v_addr);
    assert_eq!(result, Err(Ok(MarketplaceError::MerchantFrozen)));
}

// ── Discovery & pagination ────────────────────────────────────────────────────

#[test]
fn test_get_merchants_paginated() {
    let (env, client, _admin) = setup();
    for i in 0..5 {
        let m = Address::generate(&env);
        client.register_merchant(&m, &make_params(&env, &format!("Shop{i}")));
    }
    let page1 = client.get_merchants(&0, &3);
    assert_eq!(page1.len(), 3);
    let page2 = client.get_merchants(&3, &3);
    assert_eq!(page2.len(), 2);
}

#[test]
fn test_get_merchants_limit_too_large() {
    let (_env, client, _admin) = setup();
    let result = client.try_get_merchants(&0, &51);
    assert_eq!(result, Err(Ok(MarketplaceError::LimitTooLarge)));
}

#[test]
fn test_get_merchants_by_category() {
    let (env, client, _admin) = setup();
    let m1 = Address::generate(&env);
    let m2 = Address::generate(&env);
    let m3 = Address::generate(&env);

    let mut retail_params = make_params(&env, "RetailA");
    retail_params.category = symbol_short!("retail");
    client.register_merchant(&m1, &retail_params);

    let mut elec_params = make_params(&env, "ElecA");
    elec_params.category = symbol_short!("elec");
    client.register_merchant(&m2, &elec_params);

    let mut retail_params2 = make_params(&env, "RetailB");
    retail_params2.category = symbol_short!("retail");
    client.register_merchant(&m3, &retail_params2);

    let retail = client.get_merchants_by_category(&symbol_short!("retail"), &0, &10);
    assert_eq!(retail.len(), 2);
    let elec = client.get_merchants_by_category(&symbol_short!("elec"), &0, &10);
    assert_eq!(elec.len(), 1);
}

#[test]
fn test_get_merchants_by_category_limit_too_large() {
    let (env, client, _admin) = setup();
    let result = client.try_get_merchants_by_category(&symbol_short!("cat"), &0, &51);
    let _ = env;
    assert_eq!(result, Err(Ok(MarketplaceError::LimitTooLarge)));
}

#[test]
fn test_get_merchant_view() {
    let (env, client, _admin) = setup();
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "ViewTest"));
    let view = client.get_merchant_view(&id);
    assert_eq!(view.id, id);
    assert!(!view.verified);
    // No reputation contract paired — score should be None.
    assert_eq!(view.reputation_score, None::<u32>);
}

// ── Prune closed merchants ────────────────────────────────────────────────────

#[test]
fn test_prune_closed_merchants_removes_from_index() {
    let (env, client, admin) = setup();
    let m1 = Address::generate(&env);
    let m2 = Address::generate(&env);
    let id1 = client.register_merchant(&m1, &make_params(&env, "PruneA"));
    let id2 = client.register_merchant(&m2, &make_params(&env, "PruneB"));

    client.close_merchant(&admin, &id1, &symbol_short!("reason"));

    let ids = soroban_sdk::vec![&env, id1];
    let pruned = client.prune_closed_merchants(&admin, &ids);
    assert_eq!(pruned, 1);

    // id2 still discoverable; id1 no longer in global index.
    let all = client.get_merchants(&0, &10);
    assert_eq!(all.len(), 1);
    assert_eq!(all.get(0).unwrap().id, id2);
}

#[test]
fn test_prune_closed_skips_non_closed() {
    let (env, client, admin) = setup();
    let m1 = Address::generate(&env);
    let id1 = client.register_merchant(&m1, &make_params(&env, "NotClosed"));
    let ids = soroban_sdk::vec![&env, id1];
    let pruned = client.prune_closed_merchants(&admin, &ids);
    assert_eq!(pruned, 0);
}

#[test]
fn test_prune_batch_too_large() {
    let (env, client, admin) = setup();
    // Build a Vec with 51 ids (all fake).
    let mut ids = soroban_sdk::vec![&env];
    for i in 1u64..=51 {
        ids.push_back(i);
    }
    let result = client.try_prune_closed_merchants(&admin, &ids);
    assert_eq!(result, Err(Ok(MarketplaceError::BatchTooLarge)));
}

// ── Reputation integration ────────────────────────────────────────────────────

#[test]
fn test_set_merchant_reputation_by_admin() {
    let (env, client, admin) = setup();
    let merchant = Address::generate(&env);
    let rep_contract = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "RepTest"));
    client.set_merchant_reputation(&admin, &id, &Some(rep_contract.clone()));
    assert_eq!(client.get_merchant(&id).reputation, Some(rep_contract));
}

#[test]
fn test_set_reputation_contract_global() {
    let (env, client, admin) = setup();
    let rep_contract = Address::generate(&env);
    client.set_reputation_contract(&admin, &rep_contract);
    // No direct getter, but we can verify via get_merchant_view returning None score
    // (the cross-contract call will fail silently since no contract is deployed there).
    let merchant = Address::generate(&env);
    let id = client.register_merchant(&merchant, &make_params(&env, "RepGlobal"));
    let view = client.get_merchant_view(&id);
    // Score will be None since there's no actual reputation contract at that address.
    assert_eq!(view.reputation_score, None::<u32>);
}

// ── Admin management ──────────────────────────────────────────────────────────

#[test]
fn test_propose_and_accept_admin() {
    let (env, client, admin) = setup();
    let new_admin = Address::generate(&env);
    client.propose_admin(&admin, &new_admin);
    client.accept_admin(&new_admin);
    assert_eq!(client.get_admin(), new_admin);
}

#[test]
fn test_propose_admin_same_returns_false() {
    let (env, client, admin) = setup();
    let new_admin = Address::generate(&env);
    let r1 = client.propose_admin(&admin, &new_admin);
    assert!(r1);
    let r2 = client.propose_admin(&admin, &new_admin);
    assert!(!r2);
}

#[test]
fn test_propose_admin_unauthorized() {
    let (env, client, _admin) = setup();
    let stranger = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let result = client.try_propose_admin(&stranger, &new_admin);
    assert_eq!(result, Err(Ok(MarketplaceError::Unauthorized)));
}

#[test]
fn test_accept_admin_no_pending() {
    let (env, client, _admin) = setup();
    let stranger = Address::generate(&env);
    let result = client.try_accept_admin(&stranger);
    assert_eq!(result, Err(Ok(MarketplaceError::NoPendingAdmin)));
}

#[test]
fn test_accept_admin_wrong_caller() {
    let (env, client, admin) = setup();
    let proposed = Address::generate(&env);
    let wrong = Address::generate(&env);
    client.propose_admin(&admin, &proposed);
    let result = client.try_accept_admin(&wrong);
    assert_eq!(result, Err(Ok(MarketplaceError::NotPendingAdmin)));
}

// ── Version ───────────────────────────────────────────────────────────────────

#[test]
fn test_version() {
    let (_env, client, _admin) = setup();
    let v = client.version();
    assert_eq!(v.major, 0);
    assert_eq!(v.minor, 2);
    assert_eq!(v.patch, 0);
    assert_eq!(v.semver, symbol_short!("0_2_0"));
}

// ── Not found ─────────────────────────────────────────────────────────────────

#[test]
fn test_get_merchant_not_found() {
    let (_env, client, _admin) = setup();
    let result = client.try_get_merchant(&9999);
    assert_eq!(result, Err(Ok(MarketplaceError::MerchantNotFound)));
}

#[test]
fn test_get_merchant_view_not_found() {
    let (_env, client, _admin) = setup();
    let result = client.try_get_merchant_view(&9999);
    assert_eq!(result, Err(Ok(MarketplaceError::MerchantNotFound)));
}
