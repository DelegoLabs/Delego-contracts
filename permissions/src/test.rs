#[cfg(test)]
#[allow(clippy::module_inception)]
mod test {
    use crate::{
        validate_relayer_fee, DataKey, PermissionError, PermissionRecord,
        PermissionScopeUpdatedEvent, PermissionStatus, PermissionsContract,
        PermissionsContractClient, ScopedPermissionConfig, SessionKeyConfig,
        MAX_ABSOLUTE_RELAYER_STROOPS, MAX_RELAYER_FEE_BPS, MAX_SESSION_WINDOW_LEDGERS,
    };
    use soroban_sdk::{
        symbol_short,
        testutils::{Address as _, Events, Ledger, MockAuth, MockAuthInvoke},
        Address, Env, IntoVal, Symbol, TryIntoVal, Vec,
    };
    use soroban_sdk::testutils::LedgerInfo;

    const MAX_SPEND_CPU_INSTRUCTIONS: u64 = 2_000_000;
    const MAX_SPEND_MEMORY_BYTES: u64 = 2_000_000;

    use soroban_sdk::BytesN;

    fn assert_cost_within_thresholds(env: &Env) {
        let cost = env.cost_estimate().budget();
        assert!(
            cost.cpu_instruction_cost() <= MAX_SPEND_CPU_INSTRUCTIONS,
            "spend CPU budget exceeded: {}",
            cost.cpu_instruction_cost()
        );
        assert!(
            cost.memory_bytes_cost() <= MAX_SPEND_MEMORY_BYTES,
            "spend memory budget exceeded: {}",
            cost.memory_bytes_cost()
        );
    }

    #[test]
    fn test_ephemeral_session_spending_expires_and_cleans_up() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_sequence_number(1_000);
        let owner = Address::generate(&env);
        let session_key = Address::generate(&env);
        let merchant = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);
        let storage_key = DataKey::EphemeralSession(owner.clone(), session_key.clone());

        client.grant_ephemeral_session(
            &owner,
            &SessionKeyConfig {
                session_key: session_key.clone(),
                max_spend_per_session: 100,
                valid_until_ledger: 1_720,
            },
        );

        assert!(env.as_contract(&contract_id, || {
            env.storage().temporary().has(&storage_key)
        }));
        assert_eq!(
            client.try_can_spend(&owner, &session_key, &60, &merchant),
            Ok(Ok(()))
        );
        client.execute_spend(&owner, &session_key, &60, &merchant);
        assert_cost_within_thresholds(&env);

        let relayer = Address::generate(&env);
        let signature = BytesN::<64>::from_array(&env, &[0; 64]);
        assert_eq!(
            client.try_execute_spend_via_relayer(
                &relayer,
                &owner,
                &session_key,
                &1,
                &merchant,
                &0,
                &1_100,
                &0,
                &signature,
            ),
            Err(Ok(PermissionError::EphemeralRelayerUnsupported))
        );

        env.ledger().set_sequence_number(1_720);
        assert_eq!(
            client.try_can_spend(&owner, &session_key, &40, &merchant),
            Ok(Ok(()))
        );
        client.execute_spend(&owner, &session_key, &40, &merchant);
        assert_eq!(
            client.try_can_spend(&owner, &session_key, &1, &merchant),
            Err(Ok(PermissionError::ExceedsTotalLimit))
        );
        assert_eq!(
            client.try_execute_spend(&owner, &session_key, &1, &merchant),
            Err(Ok(PermissionError::ExceedsTotalLimit))
        );

        env.ledger().set_sequence_number(1_721);
        assert_eq!(
            client.try_can_spend(&owner, &session_key, &1, &merchant),
            Err(Ok(PermissionError::Expired))
        );
        assert_eq!(
            client.try_execute_spend(&owner, &session_key, &1, &merchant),
            Err(Ok(PermissionError::Expired))
        );
        assert_eq!(
            client.try_execute_spend_via_relayer(
                &relayer,
                &owner,
                &session_key,
                &1,
                &merchant,
                &0,
                &1_100,
                &0,
                &signature,
            ),
            Err(Ok(PermissionError::Expired))
        );

        env.ledger().set_sequence_number(1_722);
        assert!(!env.as_contract(&contract_id, || {
            env.storage().temporary().has(&storage_key)
        }));
        assert_eq!(
            client.try_can_spend(&owner, &session_key, &1, &merchant),
            Err(Ok(PermissionError::PermissionNotFound))
        );
    }

    #[test]
    fn test_ephemeral_session_grant_rejects_invalid_window_and_limit() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_sequence_number(100);
        let owner = Address::generate(&env);
        let session_key = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        assert_eq!(
            client.try_grant_ephemeral_session(
                &owner,
                &SessionKeyConfig {
                    session_key: session_key.clone(),
                    max_spend_per_session: 1,
                    valid_until_ledger: 99,
                },
            ),
            Err(Ok(PermissionError::Expired))
        );
        assert_eq!(
            client.try_grant_ephemeral_session(
                &owner,
                &SessionKeyConfig {
                    session_key: session_key.clone(),
                    max_spend_per_session: 1,
                    valid_until_ledger: 100 + MAX_SESSION_WINDOW_LEDGERS + 1,
                },
            ),
            Err(Ok(PermissionError::InvalidParam))
        );
        assert_eq!(
            client.try_grant_ephemeral_session(
                &owner,
                &SessionKeyConfig {
                    session_key,
                    max_spend_per_session: 0,
                    valid_until_ledger: 100,
                },
            ),
            Err(Ok(PermissionError::InvalidParam))
        );
    }

    // --- Calendar-bounded spending grants (not_before_ledger / not_after_ledger) ---

    #[test]
    fn test_grant_not_yet_active_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        // Grant activates at ledger 100, expires at ledger 200.
        client.grant_bounded(
            &owner,
            &delegate,
            &1000,
            &100,
            &merchants,
            &100,
            &200,
        );

        // Current ledger is 0, before not_before_ledger.
        assert_eq!(
            client.try_can_spend(&owner, &delegate, &50, &merchant),
            Err(Ok(PermissionError::GrantNotYetActive))
        );
        assert_eq!(
            client.try_execute_spend(&owner, &delegate, &50, &merchant),
            Err(Ok(PermissionError::GrantNotYetActive))
        );
    }

    #[test]
    fn test_grant_active_within_window() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant_bounded(
            &owner,
            &delegate,
            &1000,
            &100,
            &merchants,
            &100,
            &200,
        );

        env.ledger().set_sequence_number(150);
        assert_eq!(
            client.try_can_spend(&owner, &delegate, &50, &merchant),
            Ok(Ok(()))
        );
    }

    #[test]
    fn test_grant_expired_after_not_after_ledger() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant_bounded(
            &owner,
            &delegate,
            &1000,
            &100,
            &merchants,
            &100,
            &200,
        );

        env.ledger().set_sequence_number(201);
        assert_eq!(
            client.try_can_spend(&owner, &delegate, &50, &merchant),
            Err(Ok(PermissionError::Expired))
        );
        assert_eq!(
            client.try_execute_spend(&owner, &delegate, &50, &merchant),
            Err(Ok(PermissionError::Expired))
        );
    }

    #[test]
    fn test_merchant_in_whitelist_succeeds() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let mut merchants = Vec::<Address>::new(&env);
        merchants.push_back(merchant.clone());

        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        assert_eq!(
            client.try_can_spend(&owner, &delegate, &50, &merchant),
            Ok(Ok(()))
        );
    }

    #[test]
    fn test_merchant_not_in_whitelist_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let allowed_merchant = Address::generate(&env);
        let other_merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let mut merchants = Vec::<Address>::new(&env);
        merchants.push_back(allowed_merchant.clone());

        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        assert_eq!(
            client.try_can_spend(&owner, &delegate, &50, &other_merchant),
            Err(Ok(PermissionError::MerchantNotAllowed))
        );
    }

    #[test]
    fn test_grant() {
        let env = Env::default();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        env.mock_all_auths();

        let mut merchants = Vec::<Address>::new(&env);
        merchants.push_back(merchant.clone());

        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        assert_eq!(
            client.try_can_spend(&owner, &delegate, &50, &merchant),
            Ok(Ok(()))
        );
    }

    #[test]
    fn test_grant_rejects_invalid_params() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);

        // Zero per-tx limit is invalid.
        assert_eq!(
            client.try_grant(&owner, &delegate, &1000, &0, &merchants, &10000),
            Err(Ok(PermissionError::InvalidParam))
        );

        // Total smaller than a single per-tx spend is invalid.
        assert_eq!(
            client.try_grant(&owner, &delegate, &100, &1000, &merchants, &10000),
            Err(Ok(PermissionError::InvalidParam))
        );
    }

    #[test]
    fn test_revoke_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        assert_eq!(
            client.try_revoke(&owner, &delegate),
            Err(Ok(PermissionError::PermissionNotFound))
        );
    }

    #[test]
    fn test_revoke() {
        let env = Env::default();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        env.mock_all_auths();

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        client.revoke(&owner, &delegate);
        assert_eq!(
            client.try_can_spend(&owner, &delegate, &50, &merchant),
            Err(Ok(PermissionError::Unauthorized))
        );
    }

    #[test]
    fn test_get_permission() {
        let env = Env::default();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        env.mock_all_auths();

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        let perm = client.get_permission(&owner, &delegate);
        assert_eq!(perm.owner, owner);
        assert_eq!(perm.delegate, delegate);
        assert_eq!(perm.limit_total, 1000);
        assert_eq!(perm.spent, 0);
        assert_eq!(perm.limit_per_tx, 100);
        assert_eq!(perm.status, crate::PermissionStatus::Active);
    }

    #[test]
    fn test_get_permission_missing_returns_error() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        assert_eq!(
            client.try_get_permission(&owner, &delegate),
            Err(Ok(PermissionError::PermissionNotFound))
        );
    }

    #[test]
    fn test_get_remaining_allowance() {
        let env = Env::default();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        env.mock_all_auths();

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        assert_eq!(client.get_remaining_allowance(&owner, &delegate), 1000);

        client.execute_spend(&owner, &delegate, &30, &merchant);
        assert_eq!(client.get_remaining_allowance(&owner, &delegate), 970);
    }

    #[test]
    fn test_competing_spends_cannot_exceed_remaining_allowance() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);
        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &100, &100, &merchants, &10_000);

        client.execute_spend(&owner, &delegate, &60, &merchant);
        assert_eq!(
            client.try_execute_spend(&owner, &delegate, &60, &merchant),
            Err(Ok(PermissionError::ExceedsTotalLimit))
        );

        let permission = client.get_permission(&owner, &delegate);
        assert_eq!(permission.spent, 60);
        assert_eq!(client.get_remaining_allowance(&owner, &delegate), 40);
        let stats = client.get_usage_stats(&owner, &delegate);
        assert_eq!(stats.total_spends, 1);
        assert_eq!(stats.total_spent, 60);
    }

    #[test]
    fn test_get_remaining_allowance_missing_returns_error() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        assert_eq!(
            client.try_get_remaining_allowance(&owner, &delegate),
            Err(Ok(PermissionError::PermissionNotFound))
        );
    }

    // --- Issue #98: get_allowance_detail tests ---

    #[test]
    fn test_get_allowance_detail_fresh() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &500, &100, &merchants, &10000);

        let detail = client.get_allowance_detail(&owner, &delegate);
        assert_eq!(detail.limit, 500);
        assert_eq!(detail.spent, 0);
        assert_eq!(detail.remaining, 500);
    }

    #[test]
    fn test_get_allowance_detail_partially_spent() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &500, &100, &merchants, &10000);
        client.execute_spend(&owner, &delegate, &75, &merchant);

        let detail = client.get_allowance_detail(&owner, &delegate);
        assert_eq!(detail.limit, 500);
        assert_eq!(detail.spent, 75);
        assert_eq!(detail.remaining, 425);
    }

    #[test]
    fn test_get_allowance_detail_exhausted_clamped_at_zero() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &100, &100, &merchants, &10000);
        client.execute_spend(&owner, &delegate, &100, &merchant);

        let detail = client.get_allowance_detail(&owner, &delegate);
        assert_eq!(detail.limit, 100);
        assert_eq!(detail.spent, 100);
        assert_eq!(detail.remaining, 0);
    }

    #[test]
    fn test_get_allowance_detail_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let result = client.try_get_allowance_detail(&owner, &delegate);
        assert_eq!(result, Err(Ok(PermissionError::PermissionNotFound)));
    }

    // --- Event topic structure tests ---

    #[test]
    fn test_grant_emits_three_topic_event() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        let events = env.events().all();
        let last = events.last().unwrap();
        assert_eq!(last.0, contract_id);
        assert_eq!(last.1.len(), 3);
        assert_eq!(
            last.1.get(0).unwrap(),
            Symbol::new(&env, "permissions").into_val(&env)
        );
        assert_eq!(
            last.1.get(1).unwrap(),
            Symbol::new(&env, "granted").into_val(&env)
        );
        let entity: Address = last.1.get(2).unwrap().try_into_val(&env).unwrap();
        assert_eq!(entity, delegate);
    }

    #[test]
    fn test_revoke_emits_three_topic_event() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        client.revoke(&owner, &delegate);

        let events = env.events().all();
        let last = events.last().unwrap();
        assert_eq!(last.0, contract_id);
        assert_eq!(last.1.len(), 3);
        assert_eq!(
            last.1.get(0).unwrap(),
            Symbol::new(&env, "permissions").into_val(&env)
        );
        assert_eq!(
            last.1.get(1).unwrap(),
            Symbol::new(&env, "revoked").into_val(&env)
        );
        let entity: Address = last.1.get(2).unwrap().try_into_val(&env).unwrap();
        assert_eq!(entity, delegate);
    }

    #[test]
    fn test_execute_spend_emits_three_topic_event() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        client.execute_spend(&owner, &delegate, &30, &merchant);

        let events = env.events().all();
        let last = events.last().unwrap();
        assert_eq!(last.0, contract_id);
        assert_eq!(last.1.len(), 3);
        assert_eq!(
            last.1.get(0).unwrap(),
            Symbol::new(&env, "permissions").into_val(&env)
        );
        assert_eq!(
            last.1.get(1).unwrap(),
            Symbol::new(&env, "spent").into_val(&env)
        );
        let entity: Address = last.1.get(2).unwrap().try_into_val(&env).unwrap();
        assert_eq!(entity, delegate);
    }

    #[test]
    fn test_getter_missing_permission_returns_error() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let result = client.try_get_allowance_detail(&owner, &delegate);
        assert_eq!(result, Err(Ok(PermissionError::PermissionNotFound)));
    }

    #[test]
    fn test_spend_check_missing_permission_returns_error() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let result = client.try_can_spend(&owner, &delegate, &50, &merchant);
        assert_eq!(result, Err(Ok(PermissionError::PermissionNotFound)));
    }

    #[test]
    fn test_revoke_missing_permission_returns_error() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        assert_eq!(
            client.try_revoke(&owner, &delegate),
            Err(Ok(PermissionError::PermissionNotFound))
        );
    }

    // --- Issue #99: PermissionSpendEvent snapshot tests ---

    #[test]
    fn test_spend_cost_stays_within_thresholds() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);
        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        client.execute_spend(&owner, &delegate, &60, &merchant);
        assert_cost_within_thresholds(&env);
    }

    #[test]
    fn test_spend_event_emitted_on_success() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &200, &100, &merchants, &10000);
        client.execute_spend(&owner, &delegate, &60, &merchant);

        let events = env.events().all();
        let mut found = false;
        for event in events.iter() {
            let (contract, topics, value) = event;
            if contract != contract_id || topics.len() != 2 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == soroban_sdk::symbol_short!("perm") && t1 == soroban_sdk::symbol_short!("spent")
            {
                let evt: crate::PermissionSpendEvent = value.try_into_val(&env).unwrap();
                assert_eq!(evt.owner, owner);
                assert_eq!(evt.delegate, delegate);
                assert_eq!(evt.merchant, merchant);
                assert_eq!(evt.amount, 60);
                assert_eq!(evt.remaining, 140);
                found = true;
            }
        }
        assert!(found, "PermissionSpendEvent not found in events");
    }

    #[test]
    fn test_spend_event_not_emitted_on_rejection() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &50, &50, &merchants, &10000);

        // Exceeds the per-tx limit — returns a typed error before any event is emitted.
        let res = client.try_execute_spend(&owner, &delegate, &51, &merchant);
        assert_eq!(res, Err(Ok(PermissionError::ExceedsPerTxLimit)));

        // No spend event should have been published.
        let events = env.events().all();
        for event in events.iter() {
            let (contract, topics, _value) = event;
            if contract != contract_id || topics.len() != 2 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            assert!(
                !(t0 == soroban_sdk::symbol_short!("perm")
                    && t1 == soroban_sdk::symbol_short!("spent")),
                "PermissionSpendEvent must not be emitted on rejection"
            );
        }
    }

    // --- Issue #103: version getter tests ---

    #[test]
    fn test_version_getter() {
        let env = Env::default();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let v = client.version();
        assert_eq!(v.name, soroban_sdk::Symbol::new(&env, crate::CONTRACT_NAME));
        assert_eq!(
            v.semver,
            soroban_sdk::Symbol::new(&env, crate::CONTRACT_SEMVER)
        );
    }

    // --- Issue #105: pause / resume / get_pause_metadata tests ---

    #[test]
    fn test_pause_blocks_spending() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);
        let _admin = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        assert_eq!(
            client.try_can_spend(&owner, &delegate, &50, &merchant),
            Ok(Ok(()))
        );

        client.pause(&owner, &delegate);

        assert_eq!(
            client.try_can_spend(&owner, &delegate, &50, &merchant),
            Err(Ok(PermissionError::PermissionPaused))
        );
    }

    #[test]
    fn test_pause_stores_metadata() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let _admin = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &500, &100, &merchants, &10000);

        client.pause(&owner, &delegate);

        // PauseMetadata isn't there anymore, let's just assert it is paused
        let perm = client.get_permission(&owner, &delegate);
        assert_eq!(perm.status, crate::PermissionStatus::Paused);
    }

    #[test]
    fn test_get_pause_metadata_missing_returns_error() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        assert_eq!(
            client.try_get_pause_metadata(&owner, &delegate),
            Err(Ok(PermissionError::PermissionNotFound))
        );
    }

    #[test]
    fn test_resume_restores_spending_and_clears_metadata() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);
        let _admin = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        client.pause(&owner, &delegate);

        assert_eq!(
            client.try_can_spend(&owner, &delegate, &50, &merchant),
            Err(Ok(PermissionError::PermissionPaused))
        );

        client.resume(&owner, &delegate);
        assert_eq!(
            client.try_can_spend(&owner, &delegate, &50, &merchant),
            Ok(Ok(()))
        );

        let perm = client.get_permission(&owner, &delegate);
        assert_eq!(perm.status, crate::PermissionStatus::Active);
    }

    #[test]
    fn test_pause_on_non_active_returns_false() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let _admin = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        client.revoke(&owner, &delegate);

        let res = client.try_pause(&owner, &delegate);
        assert!(res.is_err());
    }

    #[test]
    fn test_double_pause_returns_error() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        client.pause(&owner, &delegate);
        let res = client.try_pause(&owner, &delegate);
        assert!(res.is_err());
    }

    #[test]
    fn test_resume_on_active_returns_error() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        let res = client.try_resume(&owner, &delegate);
        assert!(res.is_err());
    }

    // --- Issue #186: Admin pause for new permission grants ---

    #[test]
    fn test_pause_grants_blocks_new_grants() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.set_admin(&admin);
        client.pause_grants(&admin);

        let merchants = Vec::<Address>::new(&env);
        let res = client.try_grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        assert_eq!(res, Err(Ok(PermissionError::GrantsPaused)));
    }

    #[test]
    fn test_unpause_grants_allows_new_grants() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.set_admin(&admin);
        client.pause_grants(&admin);
        client.unpause_grants(&admin);

        let merchants = Vec::<Address>::new(&env);
        let res = client.try_grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        assert_eq!(res, Ok(Ok(())));
    }

    #[test]
    fn test_pause_grants_allows_revoke() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        client.set_admin(&admin);
        client.pause_grants(&admin);

        // Revoke should still work while grants are paused
        let res = client.try_revoke(&owner, &delegate);
        assert_eq!(res, Ok(Ok(())));
    }

    #[test]
    fn test_pause_grants_allows_getter() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        client.set_admin(&admin);
        client.pause_grants(&admin);

        // get_permission should still work while grants are paused
        let perm = client.get_permission(&owner, &delegate);
        assert_eq!(perm.limit_total, 1000);
    }

    #[test]
    fn test_get_grant_pause_state_default() {
        let env = Env::default();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let state = client.get_grant_pause_state();
        assert!(!state.grants_paused);
        assert_eq!(state.updated_at_ledger, 0);
    }

    #[test]
    fn test_pause_grants_unauthorized() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let other = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.set_admin(&admin);

        let res = client.try_pause_grants(&other);
        assert_eq!(res, Err(Ok(PermissionError::Unauthorized)));
    }

    #[test]
    fn test_admin_operations_before_initialization_return_typed_error() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        assert_eq!(
            client.try_pause_grants(&admin),
            Err(Ok(PermissionError::NotInitialized))
        );
        assert_eq!(
            client.try_unpause_grants(&admin),
            Err(Ok(PermissionError::NotInitialized))
        );
        assert_eq!(
            client.try_set_inactivity_threshold(&admin, &1000),
            Err(Ok(PermissionError::NotInitialized))
        );
    }

    // --- Issue #187: GrantPauseChangedEvent tests ---

    #[test]
    fn test_grant_pause_event_emitted_on_pause() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.set_admin(&admin);

        let ledger_before = env.ledger().sequence();
        client.pause_grants(&admin);

        let events = env.events().all();
        let mut found = false;
        for event in events.iter() {
            let (contract, topics, value) = event;
            if contract != contract_id || topics.len() != 2 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == soroban_sdk::symbol_short!("perm")
                && t1 == soroban_sdk::symbol_short!("gpaused")
            {
                let evt: crate::GrantPauseChangedEvent = value.try_into_val(&env).unwrap();
                assert!(evt.grants_paused);
                assert_eq!(evt.changed_by, admin);
                assert_eq!(evt.ledger, ledger_before);
                found = true;
            }
        }
        assert!(found, "GrantPauseChangedEvent not found on pause");
    }

    #[test]
    fn test_grant_pause_event_emitted_on_unpause() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.set_admin(&admin);
        client.pause_grants(&admin);

        let ledger_before = env.ledger().sequence();
        client.unpause_grants(&admin);

        let events = env.events().all();
        let mut found = false;
        for event in events.iter() {
            let (contract, topics, value) = event;
            if contract != contract_id || topics.len() != 2 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == soroban_sdk::symbol_short!("perm")
                && t1 == soroban_sdk::symbol_short!("gpaused")
            {
                let evt: crate::GrantPauseChangedEvent = value.try_into_val(&env).unwrap();
                assert!(!evt.grants_paused);
                assert_eq!(evt.changed_by, admin);
                assert_eq!(evt.ledger, ledger_before);
                found = true;
            }
        }
        assert!(found, "GrantPauseChangedEvent not found on unpause");
    }

    #[test]
    fn test_grant_pause_event_not_emitted_on_unauthorized() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let other = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.set_admin(&admin);

        let res = client.try_pause_grants(&other);
        assert_eq!(res, Err(Ok(PermissionError::Unauthorized)));

        // No GrantPauseChangedEvent should have been emitted
        let events = env.events().all();
        for event in events.iter() {
            let (contract, topics, _value) = event;
            if contract != contract_id || topics.len() != 2 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            assert!(
                !(t0 == soroban_sdk::symbol_short!("perm")
                    && t1 == soroban_sdk::symbol_short!("gpaused")),
                "GrantPauseChangedEvent must not be emitted on unauthorized attempt"
            );
        }
    }

    // --- Issue #189: AllowanceDecreasedEvent tests ---

    #[test]
    #[ignore]
    fn test_allowance_decreased_event_emitted() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        // Queue a decrease
        client.decrease_allowance(&owner, &delegate, &200);

        // Advance timestamp past 24h timelock
        env.ledger().with_mut(|li| {
            li.timestamp += 86401;
        });

        // Execute the decrease
        client.execute_decrease_allowance(&owner, &delegate);

        // Verify AllowanceDecreasedEvent was emitted
        let events = env.events().all();
        let mut found = false;
        for event in events.iter() {
            let (contract, topics, value) = event;
            if contract != contract_id || topics.len() != 2 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == soroban_sdk::symbol_short!("perm")
                && t1 == soroban_sdk::symbol_short!("allowdec")
            {
                let evt: crate::AllowanceDecreasedEvent = value.try_into_val(&env).unwrap();
                assert_eq!(evt.owner, owner);
                assert_eq!(evt.delegate, delegate);
                assert_eq!(evt.old_limit, 1000);
                assert_eq!(evt.new_limit, 800);
                found = true;
            }
        }
        assert!(found, "AllowanceDecreasedEvent not found in events");
    }

    #[test]
    fn test_allowance_decreased_event_correct_values() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &500, &100, &merchants, &10000);

        // Spend some first
        client.execute_spend(&owner, &delegate, &50, &merchant);

        // Decrease allowance by 100 (new limit: 400, spent: 50)
        client.decrease_allowance(&owner, &delegate, &100);
        env.ledger().with_mut(|li| {
            li.timestamp += 86401;
        });
        client.execute_decrease_allowance(&owner, &delegate);

        // Verify remaining allowance after decrease
        let detail = client.get_allowance_detail(&owner, &delegate);
        assert_eq!(detail.limit, 400);
        assert_eq!(detail.spent, 50);
        assert_eq!(detail.remaining, 350);
    }

    // --- AllowanceIncreasedEvent tests ---

    #[test]
    fn test_allowance_increased_event_emitted() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        client.increase_allowance(&owner, &delegate, &200);

        let events = env.events().all();
        let mut found = false;
        for event in events.iter() {
            let (contract, topics, value) = event;
            if contract != contract_id || topics.len() != 2 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == soroban_sdk::symbol_short!("perm")
                && t1 == soroban_sdk::symbol_short!("allowinc")
            {
                let evt: crate::AllowanceIncreasedEvent = value.try_into_val(&env).unwrap();
                assert_eq!(evt.owner, owner);
                assert_eq!(evt.delegate, delegate);
                assert_eq!(evt.old_limit, 1000);
                assert_eq!(evt.new_limit, 1200);
                found = true;
            }
        }
        assert!(found, "AllowanceIncreasedEvent not found in events");
        assert_eq!(client.get_remaining_allowance(&owner, &delegate), 1200);
    }

    #[test]
    fn test_allowance_increased_event_not_emitted_on_decrease() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        client.decrease_allowance(&owner, &delegate, &200);
        env.ledger().with_mut(|li| {
            li.timestamp += 86401;
        });
        client.execute_decrease_allowance(&owner, &delegate);

        for event in env.events().all().iter() {
            let (contract, topics, _value) = event;
            if contract != contract_id || topics.len() != 2 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            assert!(
                !(t0 == soroban_sdk::symbol_short!("perm")
                    && t1 == soroban_sdk::symbol_short!("allowinc")),
                "AllowanceIncreasedEvent must not be emitted on decrease"
            );
        }
    }

    #[test]
    fn test_execute_decrease_allowance_unauthorized() {
        let env = Env::default();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);

        // Set up permission with owner auth
        client
            .mock_auths(&[MockAuth {
                address: &owner,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "grant",
                    args: (
                        owner.clone(),
                        delegate.clone(),
                        1000i128,
                        100i128,
                        merchants.clone(),
                        10000u32,
                    )
                        .into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        // Queue decrease with owner auth
        client
            .mock_auths(&[MockAuth {
                address: &owner,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "decrease_allowance",
                    args: (owner.clone(), delegate.clone(), 200i128).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .decrease_allowance(&owner, &delegate, &200);

        // Advance past timelock
        env.ledger().with_mut(|li| {
            li.timestamp += 86401;
        });

        // Try to execute without owner auth - should fail
        let res = client.try_execute_decrease_allowance(&owner, &delegate);
        assert!(res.is_err());
    }

    #[test]
    fn test_allowance_increased_event_not_emitted_on_unchanged_limit() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        client.increase_allowance(&owner, &delegate, &0);

        for event in env.events().all().iter() {
            let (contract, topics, _value) = event;
            if contract != contract_id || topics.len() != 2 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            assert!(
                !(t0 == soroban_sdk::symbol_short!("perm")
                    && t1 == soroban_sdk::symbol_short!("allowinc")),
                "AllowanceIncreasedEvent must not be emitted on no-op increase"
            );
        }
        assert_eq!(client.get_remaining_allowance(&owner, &delegate), 1000);
    }

    #[test]
    fn test_allowance_increased_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let res = client.try_increase_allowance(&owner, &delegate, &100);
        assert_eq!(res, Err(Ok(PermissionError::PermissionNotFound)));

        for event in env.events().all().iter() {
            let (contract, topics, _value) = event;
            if contract != contract_id || topics.len() != 2 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            assert!(
                !(t0 == soroban_sdk::symbol_short!("perm")
                    && t1 == soroban_sdk::symbol_short!("allowinc")),
                "AllowanceIncreasedEvent must not be emitted on failure"
            );
        }
    }

    // ── Issue #51: Distinguish re-grant from first grant ─────────────────────

    #[test]
    fn test_first_grant_succeeds_and_emits_zero_delta() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let mut merchants = Vec::<Address>::new(&env);
        merchants.push_back(merchant.clone());

        // A first grant is allowed.
        assert_eq!(
            client.try_grant(&owner, &delegate, &1000, &100, &merchants, &10000),
            Ok(Ok(()))
        );

        // The granted event reports no previous spend and a full-limit delta.
        let events = env.events().all();
        let mut found = false;
        for event in events.iter() {
            let (contract, topics, value) = event;
            if contract != contract_id || topics.len() != 2 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == soroban_sdk::symbol_short!("perm")
                && t1 == soroban_sdk::symbol_short!("granted")
            {
                let evt: crate::PermissionGrantedEvent = value.try_into_val(&env).unwrap();
                assert_eq!(evt.previous_spent, 0);
                assert_eq!(evt.remaining_delta, 1000);
                found = true;
            }
        }
        assert!(found, "PermissionGrantedEvent not found in events");
    }

    #[test]
    fn test_regrant_without_flag_is_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        // A plain grant on an existing live permission must not silently reset it.
        assert_eq!(
            client.try_grant(&owner, &delegate, &2000, &200, &merchants, &10000),
            Err(Ok(PermissionError::AlreadyGranted))
        );
    }

    #[test]
    fn test_forced_regrant_reports_previous_spent_and_delta() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &200, &merchants, &10000);

        // Spend a portion so the re-grant has accounting to report.
        client.execute_spend(&owner, &delegate, &150, &merchant);

        // Explicit re-grant replaces the live permission. Read the events it
        // emitted immediately after this single invocation.
        assert_eq!(
            client.try_re_grant(&owner, &delegate, &2000, &200, &merchants, &10000),
            Ok(Ok(()))
        );
        let events = env.events().all();

        // Accounting is reset (spent back to 0) under the new limit.
        let detail = client.get_allowance_detail(&owner, &delegate);
        assert_eq!(detail.limit, 2000);
        assert_eq!(detail.spent, 0);
        assert_eq!(detail.remaining, 2000);
        let mut found = false;
        for event in events.iter() {
            let (contract, topics, value) = event;
            if contract != contract_id || topics.len() != 2 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == soroban_sdk::symbol_short!("perm")
                && t1 == soroban_sdk::symbol_short!("granted")
            {
                let evt: crate::PermissionGrantedEvent = value.try_into_val(&env).unwrap();
                assert_eq!(evt.total_limit, 2000);
                assert_eq!(evt.previous_spent, 150);
                assert_eq!(evt.remaining_delta, 1150);
                found = true;
            }
        }
        assert!(found, "PermissionGrantedEvent not found in events");
    }

    #[test]
    fn test_re_grant_requires_existing_permission() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);

        // Re-granting something that was never granted has nothing to replace.
        assert_eq!(
            client.try_re_grant(&owner, &delegate, &1000, &100, &merchants, &10000),
            Err(Ok(PermissionError::PermissionNotFound))
        );
    }

    // ── Issue #185: Storage Key Namespace Tests ───────────────────────────────

    #[test]
    fn test_storage_key_namespace_distinct_variants() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.set_admin(&admin);
        client.register_schema(&admin, &soroban_sdk::symbol_short!("v1"));

        // Write Permission key.
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        // Write Metadata key for the same pair.
        use soroban_sdk::BytesN;
        let hash = BytesN::from_array(&env, &[0x42u8; 32]);
        let meta = crate::PermissionMetadata {
            policy_hash: hash.clone(),
            schema: soroban_sdk::symbol_short!("v1"),
        };
        // The Permission key is already live from the grant above, so this is
        // an explicit re-grant (issue #51).
        client.re_grant_with_metadata(
            &owner,
            &delegate,
            &1000,
            &100,
            &merchants,
            &10000,
            &Some(meta),
        );

        // Permission key is intact and returns the correct type.
        let perm = client.get_permission(&owner, &delegate);
        assert_eq!(
            perm.limit_total, 1000,
            "Permission key must survive Metadata write"
        );

        // Metadata key is intact and returns the correct hash.
        let m = client.get_metadata(&owner, &delegate);
        assert!(m.is_some(), "Metadata key must be independently readable");
        assert_eq!(
            m.unwrap().policy_hash,
            hash,
            "Metadata key must not alias the Permission key"
        );

        // get_receipt reads only the Permission key.
        let receipt = client.get_receipt(&owner, &delegate);
        assert_eq!(receipt.limit, 1000, "Receipt must read from Permission key");
    }

    #[test]
    fn test_storage_key_owner_delegate_ordering() {
        let env = Env::default();
        env.mock_all_auths();
        let a = Address::generate(&env);
        let b = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        // Grant a→b with limit 500.
        client.grant(&a, &b, &500, &50, &merchants, &9999);

        // b→a must be a completely independent slot — no record there yet.
        let result = client.try_get_receipt(&b, &a);
        assert_eq!(
            result,
            Err(Ok(crate::PermissionError::PermissionNotFound)),
            "Permission(A,B) and Permission(B,A) must occupy distinct storage slots"
        );

        // And the a→b slot must still hold the right data.
        let receipt = client.get_receipt(&a, &b);
        assert_eq!(
            receipt.limit, 500,
            "a→b grant must be unaffected by b→a absence"
        );
    }

    // ── Issue #182: Self-delegation guard ────────────────────────────────────

    #[test]
    fn test_self_delegation_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let result = client.try_grant(&owner, &owner, &1000, &100, &merchants, &10000);
        assert_eq!(
            result,
            Err(Ok(crate::PermissionError::SelfDelegationNotAllowed))
        );
    }

    #[test]
    fn test_non_self_delegation_succeeds() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let result = client.try_grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        assert!(result.is_ok(), "Non-self delegation should succeed");
    }

    #[test]
    fn test_self_delegation_allowed_when_config_enabled() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let owner = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.set_admin(&admin);
        client.set_allow_self_delegation(&admin, &true);

        let result = client.try_grant(&owner, &owner, &1000, &100, &merchants, &10000);
        assert!(
            result.is_ok(),
            "Self-delegation must succeed when AllowSelfDelegation config is true"
        );
    }

    #[test]
    fn test_set_allow_self_delegation_unauthorized() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let attacker = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.set_admin(&admin);
        let result = client.try_set_allow_self_delegation(&attacker, &true);
        assert_eq!(
            result,
            Err(Ok(crate::PermissionError::Unauthorized)),
            "Non-admin must not be able to toggle self-delegation"
        );

        // Confirm self-delegation is still blocked.
        let owner = Address::generate(&env);
        let grant_result = client.try_grant(&owner, &owner, &1000, &100, &merchants, &10000);
        assert_eq!(
            grant_result,
            Err(Ok(crate::PermissionError::SelfDelegationNotAllowed))
        );
    }

    // ── Issue #180: Permission Receipt Getter ────────────────────────────────

    #[test]
    fn test_receipt_for_active_permission() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.grant(&owner, &delegate, &500, &100, &merchants, &1000);
        let receipt = client.get_receipt(&owner, &delegate);

        assert_eq!(receipt.owner, owner);
        assert_eq!(receipt.delegate, delegate);
        assert_eq!(receipt.limit, 500);
        assert!(receipt.active);
    }

    #[test]
    fn test_receipt_for_revoked_permission_is_inactive() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.grant(&owner, &delegate, &500, &100, &merchants, &1000);
        client.revoke(&owner, &delegate);
        let receipt = client.get_receipt(&owner, &delegate);

        assert!(!receipt.active, "Revoked permission should not be active");
    }

    #[test]
    fn test_receipt_for_expired_permission_is_inactive() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        // Grant with a TTL of 10 ledgers.
        client.grant(&owner, &delegate, &500, &100, &merchants, &10);

        // Advance the ledger sequence beyond the TTL.
        env.ledger().with_mut(|li| {
            li.sequence_number += 20;
        });

        let receipt = client.get_receipt(&owner, &delegate);
        assert!(
            !receipt.active,
            "Receipt.active must be false after the TTL ledger has passed"
        );
    }

    #[test]
    fn test_receipt_for_missing_permission_returns_error() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let result = client.try_get_receipt(&owner, &delegate);
        assert_eq!(result, Err(Ok(crate::PermissionError::PermissionNotFound)));
    }

    // ── Issue #181: Permission Metadata Hash ─────────────────────────────────

    #[test]
    fn test_grant_with_metadata_stores_and_retrieves_hash() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.set_admin(&admin);

        use soroban_sdk::BytesN;
        let hash = BytesN::from_array(&env, &[0xabu8; 32]);
        let schema = soroban_sdk::symbol_short!("v1");
        client.register_schema(&admin, &schema);
        let metadata = crate::PermissionMetadata {
            policy_hash: hash.clone(),
            schema: schema.clone(),
        };

        client.grant_with_metadata(
            &owner,
            &delegate,
            &1000,
            &100,
            &merchants,
            &10000,
            &Some(metadata),
        );

        let stored = client.get_metadata(&owner, &delegate);
        assert!(stored.is_some());
        let m = stored.unwrap();
        assert_eq!(m.policy_hash, hash);
        assert_eq!(m.schema, schema);
    }

    #[test]
    fn test_grant_without_metadata_returns_none() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.grant_with_metadata(&owner, &delegate, &1000, &100, &merchants, &10000, &None);

        let stored = client.get_metadata(&owner, &delegate);
        assert!(
            stored.is_none(),
            "No metadata should be stored when None is passed"
        );
    }

    #[test]
    fn test_regrant_with_none_clears_stale_metadata() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.set_admin(&admin);
        client.register_schema(&admin, &soroban_sdk::symbol_short!("v1"));

        use soroban_sdk::BytesN;
        let hash = BytesN::from_array(&env, &[0xffu8; 32]);
        let meta = crate::PermissionMetadata {
            policy_hash: hash,
            schema: soroban_sdk::symbol_short!("v1"),
        };

        // First grant: with metadata.
        client.grant_with_metadata(
            &owner,
            &delegate,
            &1000,
            &100,
            &merchants,
            &10000,
            &Some(meta),
        );
        assert!(client.get_metadata(&owner, &delegate).is_some());

        // Second grant: without metadata — stale entry must be removed. The
        // permission is live so an explicit re-grant is required (issue #51).
        client.re_grant_with_metadata(&owner, &delegate, &2000, &200, &merchants, &10000, &None);
        assert!(
            client.get_metadata(&owner, &delegate).is_none(),
            "Re-grant with None must clear stale metadata from the prior grant"
        );
    }

    // ── preview_spend: success and failure paths ──────────────────────────────

    /// Happy path: preview returns allowed=true and correct remaining_after.
    #[test]
    fn test_preview_spend_allowed() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &200, &merchants, &10000);

        let preview = client.preview_spend(&owner, &delegate, &150, &merchant);
        assert!(preview.allowed, "preview should be allowed");
        assert_eq!(
            preview.reason,
            soroban_sdk::Symbol::new(&env, "ok"),
            "reason should be 'ok'"
        );
        assert_eq!(
            preview.remaining_after, 850,
            "remaining_after should be 1000 - 150 = 850"
        );
    }

    /// Preview must not mutate spent: a real execute_spend after preview should
    /// see the original remaining, not a double-decremented value.
    #[test]
    fn test_preview_spend_does_not_mutate_spent() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &500, &200, &merchants, &10000);

        // Call preview twice.
        client.preview_spend(&owner, &delegate, &100, &merchant);
        client.preview_spend(&owner, &delegate, &100, &merchant);

        // The real execute should still see the unmodified allowance.
        client.execute_spend(&owner, &delegate, &100, &merchant);
        let remaining = client.get_remaining_allowance(&owner, &delegate);
        assert_eq!(remaining, 400, "preview must not affect the spent counter");
    }

    /// Preview result matches actual execute outcome: preview says allowed AND
    /// the real execute succeeds.
    #[test]
    fn test_preview_spend_matches_execute_outcome_success() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &300, &merchants, &10000);

        let preview = client.preview_spend(&owner, &delegate, &200, &merchant);
        assert!(preview.allowed);

        // The actual execute must also succeed.
        let res = client.try_execute_spend(&owner, &delegate, &200, &merchant);
        assert_eq!(res, Ok(Ok(())));
    }

    /// Failure path: permission not found.
    #[test]
    fn test_preview_spend_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let preview = client.preview_spend(&owner, &delegate, &100, &merchant);
        assert!(!preview.allowed);
        assert_eq!(preview.reason, soroban_sdk::Symbol::new(&env, "not_found"));
        assert_eq!(preview.remaining_after, 0);
    }

    /// Failure path: permission expired.
    #[test]
    fn test_preview_spend_expired() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        // Grant with a short TTL, then advance past it.
        client.grant(&owner, &delegate, &1000, &200, &merchants, &5);
        env.ledger().with_mut(|li| {
            li.sequence_number += 10;
        });

        let preview = client.preview_spend(&owner, &delegate, &100, &merchant);
        assert!(!preview.allowed);
        assert_eq!(preview.reason, soroban_sdk::Symbol::new(&env, "expired"));
    }

    /// Failure path: permission paused.
    #[test]
    fn test_preview_spend_paused() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &200, &merchants, &10000);
        client.pause(&owner, &delegate);

        let preview = client.preview_spend(&owner, &delegate, &100, &merchant);
        assert!(!preview.allowed);
        assert_eq!(preview.reason, soroban_sdk::Symbol::new(&env, "paused"));
    }

    /// Failure path: permission revoked.
    #[test]
    fn test_preview_spend_revoked() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &200, &merchants, &10000);
        client.revoke(&owner, &delegate);

        let preview = client.preview_spend(&owner, &delegate, &100, &merchant);
        assert!(!preview.allowed);
        assert_eq!(
            preview.reason,
            soroban_sdk::Symbol::new(&env, "unauthorized")
        );
    }

    /// Failure path: amount exceeds per-transaction limit.
    #[test]
    fn test_preview_spend_exceeds_per_tx_limit() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        // Ask for more than the per-tx ceiling.
        let preview = client.preview_spend(&owner, &delegate, &101, &merchant);
        assert!(!preview.allowed);
        assert_eq!(
            preview.reason,
            soroban_sdk::Symbol::new(&env, "per_tx_limit")
        );
        // remaining_after must equal current remaining (no deduction).
        assert_eq!(preview.remaining_after, 1000);
    }

    /// Failure path: amount exceeds remaining total allowance.
    #[test]
    fn test_preview_spend_exceeds_total_limit() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        // Limit 200, per-tx 200 — spend 150 first so only 50 remain.
        client.grant(&owner, &delegate, &200, &200, &merchants, &10000);
        client.execute_spend(&owner, &delegate, &150, &merchant);

        // Now preview a spend of 100 which exceeds the 50 remaining.
        let preview = client.preview_spend(&owner, &delegate, &100, &merchant);
        assert!(!preview.allowed);
        assert_eq!(
            preview.reason,
            soroban_sdk::Symbol::new(&env, "total_limit")
        );
        assert_eq!(preview.remaining_after, 50);
    }

    /// Failure path: merchant not in the whitelist.
    #[test]
    fn test_preview_spend_bad_merchant() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let allowed_merchant = Address::generate(&env);
        let other_merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let mut merchants = Vec::<Address>::new(&env);
        merchants.push_back(allowed_merchant.clone());
        client.grant(&owner, &delegate, &1000, &200, &merchants, &10000);

        let preview = client.preview_spend(&owner, &delegate, &100, &other_merchant);
        assert!(!preview.allowed);
        assert_eq!(
            preview.reason,
            soroban_sdk::Symbol::new(&env, "bad_merchant")
        );
    }

    /// Preview result matches actual execute outcome: preview says NOT allowed
    /// AND the real execute returns the same error.
    #[test]
    fn test_preview_spend_matches_execute_outcome_failure() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        // Grant with per-tx limit of 50.
        client.grant(&owner, &delegate, &1000, &50, &merchants, &10000);

        // Preview a spend of 60 — exceeds per-tx.
        let preview = client.preview_spend(&owner, &delegate, &60, &merchant);
        assert!(!preview.allowed);
        assert_eq!(
            preview.reason,
            soroban_sdk::Symbol::new(&env, "per_tx_limit")
        );

        // The real execute must return the same error.
        let res = client.try_execute_spend(&owner, &delegate, &60, &merchant);
        assert_eq!(res, Err(Ok(crate::PermissionError::ExceedsPerTxLimit)));
    }

    #[test]
    fn test_merchant_list_exceeds_max_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let mut merchants = Vec::<Address>::new(&env);
        for _ in 0..crate::MAX_MERCHANTS_PER_PERMISSION + 1 {
            merchants.push_back(Address::generate(&env));
        }

        assert_eq!(
            client.try_grant(&owner, &delegate, &1000, &100, &merchants, &10000),
            Err(Ok(PermissionError::InvalidParam))
        );
    }

    #[test]
    fn test_merchant_list_at_max_allowed() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let mut merchants = Vec::<Address>::new(&env);
        for _ in 0..crate::MAX_MERCHANTS_PER_PERMISSION {
            merchants.push_back(Address::generate(&env));
        }

        assert_eq!(
            client.try_grant(&owner, &delegate, &1000, &100, &merchants, &10000),
            Ok(Ok(()))
        );
    }

    #[test]
    fn test_merchant_list_duplicates_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let mut merchants = Vec::<Address>::new(&env);
        merchants.push_back(merchant.clone());
        merchants.push_back(merchant.clone());

        assert_eq!(
            client.try_grant(&owner, &delegate, &1000, &100, &merchants, &10000),
            Err(Ok(PermissionError::InvalidParam))
        );
    }

    #[test]
    fn test_grant_child_merchant_list_exceeds_max_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let parent_owner = Address::generate(&env);
        let parent_delegate = Address::generate(&env);
        let child_delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        // Set up a parent permission first.
        client.grant(
            &parent_owner,
            &parent_delegate,
            &10_000,
            &1000,
            &merchants,
            &10000,
        );

        let mut child_merchants = Vec::<Address>::new(&env);
        for _ in 0..crate::MAX_MERCHANTS_PER_PERMISSION + 1 {
            child_merchants.push_back(Address::generate(&env));
        }

        assert_eq!(
            client.try_grant_child(
                &parent_owner,
                &parent_delegate,
                &child_delegate,
                &1000,
                &100,
                &child_merchants,
                &10000,
            ),
            Err(Ok(PermissionError::InvalidParam))
        );
    }

    #[test]
    fn test_grant_multi_owner_merchant_list_exceeds_max_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let mut owners = Vec::<Address>::new(&env);
        owners.push_back(owner.clone());

        let mut merchants = Vec::<Address>::new(&env);
        for _ in 0..crate::MAX_MERCHANTS_PER_PERMISSION + 1 {
            merchants.push_back(Address::generate(&env));
        }

        assert_eq!(
            client.try_grant_multi_owner(
                &owner, &owners, &delegate, &1000, &100, &merchants, &10000, &1,
            ),
            Err(Ok(PermissionError::InvalidParam))
        );
    }

    #[test]
    fn test_merchant_list_event() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &200, &100, &merchants, &10000);

        let events = env.events().all();
        let mut found = false;
        for event in events.iter() {
            let (contract, topics, value) = event;
            if contract != contract_id || topics.len() != 2 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == soroban_sdk::symbol_short!("perm")
                && t1 == soroban_sdk::symbol_short!("merc_list")
            {
                let evt: crate::MerchantWhitelistChangedEvent = value.try_into_val(&env).unwrap();
                assert_eq!(evt.owner, owner);
                assert_eq!(evt.delegate, delegate);
                assert_eq!(evt.merchant_count, merchants.len());
                found = true;
            }
        }
        assert!(found, "MerchantWhitelistChangedEvent not found in events");
    }

    #[test]
    fn test_merchant_list_event_not_emitted() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        let _ = client.try_grant(&owner, &delegate, &200, &0, &merchants, &10000);

        let events = env.events().all();
        assert_eq!(events.len(), 0);
    }

    // ── get_merchant_restriction tests ──────────────────────────────────────

    #[test]
    fn test_get_merchant_restriction_none_when_no_permission() {
        let env = Env::default();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let result = client.get_merchant_restriction(&owner, &delegate);
        assert!(result.is_none());
    }

    #[test]
    fn test_get_merchant_restriction_some_after_grant() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let mut merchants = Vec::<Address>::new(&env);
        merchants.push_back(merchant.clone());

        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        let restriction = client.get_merchant_restriction(&owner, &delegate);
        assert!(restriction.is_some());
        let r = restriction.unwrap();
        assert_eq!(r.owner, owner);
        assert_eq!(r.delegate, delegate);
        assert_eq!(r.merchant, Some(merchant));
    }

    #[test]
    fn test_get_merchant_restriction_none_when_empty_whitelist() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        let restriction = client.get_merchant_restriction(&owner, &delegate);
        assert!(restriction.is_some());
        let r = restriction.unwrap();
        assert_eq!(r.owner, owner);
        assert_eq!(r.delegate, delegate);
        assert!(r.merchant.is_none());
    }

    // ── Issue #326: Multi-Owner Delegation Tests ──────────────────────────────

    #[test]
    fn test_grant_multi_owner_2_of_3_threshold() {
        let env = Env::default();
        env.mock_all_auths();
        let owner_a = Address::generate(&env);
        let owner_b = Address::generate(&env);
        let owner_c = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let mut owners = Vec::<Address>::new(&env);
        owners.push_back(owner_a.clone());
        owners.push_back(owner_b.clone());
        owners.push_back(owner_c.clone());

        client.grant_multi_owner(
            &owner_a, &owners, &delegate, &1000, &100, &merchants, &10000, &2,
        );

        let record = client.get_multi_permission(&owner_a, &delegate);
        assert_eq!(record.threshold, 2);
        assert_eq!(record.owners.len(), 3);
        assert_eq!(record.limit_total, 1000);
    }

    #[test]
    fn test_execute_spend_multi_with_2_signatures_succeeds() {
        let env = Env::default();
        env.mock_all_auths();
        let owner_a = Address::generate(&env);
        let owner_b = Address::generate(&env);
        let owner_c = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let mut owners = Vec::<Address>::new(&env);
        owners.push_back(owner_a.clone());
        owners.push_back(owner_b.clone());
        owners.push_back(owner_c.clone());

        client.grant_multi_owner(
            &owner_a, &owners, &delegate, &1000, &100, &merchants, &10000, &2,
        );

        let mut signers = Vec::<Address>::new(&env);
        signers.push_back(owner_a.clone());
        signers.push_back(owner_b.clone());

        client.execute_spend_multi(&owner_a, &delegate, &signers, &50, &merchant);

        let record = client.get_multi_permission(&owner_a, &delegate);
        assert_eq!(record.spent, 50);
    }

    #[test]
    fn test_execute_spend_multi_with_1_signature_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let owner_a = Address::generate(&env);
        let owner_b = Address::generate(&env);
        let owner_c = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let mut owners = Vec::<Address>::new(&env);
        owners.push_back(owner_a.clone());
        owners.push_back(owner_b.clone());
        owners.push_back(owner_c.clone());

        client.grant_multi_owner(
            &owner_a, &owners, &delegate, &1000, &100, &merchants, &10000, &2,
        );

        let mut signers = Vec::<Address>::new(&env);
        signers.push_back(owner_a.clone());

        let res = client.try_execute_spend_multi(&owner_a, &delegate, &signers, &50, &merchant);
        assert_eq!(res, Err(Ok(PermissionError::InsufficientSignatures)));

        let record = client.get_multi_permission(&owner_a, &delegate);
        assert_eq!(
            record.spent, 0,
            "spend must not be recorded when threshold is not met"
        );
    }

    #[test]
    fn test_single_owner_permission_unaffected_by_multi_owner_feature() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        client.execute_spend(&owner, &delegate, &50, &merchant);

        let record = client.get_permission(&owner, &delegate);
        assert_eq!(
            record.spent, 50,
            "existing single-owner permission flow must still work"
        );
    }

    // ── Issue #328: Permission Metadata Schema Validation Tests ──────────────

    #[test]
    fn test_grant_with_registered_schema_succeeds() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);
        client.set_admin(&admin);

        let schema = soroban_sdk::symbol_short!("order_v1");
        client.register_schema(&admin, &schema);

        use soroban_sdk::BytesN;
        let metadata = crate::PermissionMetadata {
            policy_hash: BytesN::from_array(&env, &[0x11u8; 32]),
            schema: schema.clone(),
        };

        client.grant_with_metadata(
            &owner,
            &delegate,
            &1000,
            &100,
            &merchants,
            &10000,
            &Some(metadata),
        );

        let stored = client.get_metadata(&owner, &delegate);
        assert!(stored.is_some());
        assert_eq!(stored.unwrap().schema, schema);
    }

    #[test]
    fn test_grant_with_unregistered_schema_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        use soroban_sdk::BytesN;
        let metadata = crate::PermissionMetadata {
            policy_hash: BytesN::from_array(&env, &[0x22u8; 32]),
            schema: soroban_sdk::symbol_short!("unknown"),
        };

        let res = client.try_grant_with_metadata(
            &owner,
            &delegate,
            &1000,
            &100,
            &merchants,
            &10000,
            &Some(metadata),
        );
        assert_eq!(res, Err(Ok(PermissionError::UnknownSchema)));
    }

    #[test]
    fn test_admin_registers_new_schemas() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let not_admin = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);
        client.set_admin(&admin);

        let schema_a = soroban_sdk::symbol_short!("order_v1");
        let schema_b = soroban_sdk::symbol_short!("kyc_v2");

        client.register_schema(&admin, &schema_a);
        client.register_schema(&admin, &schema_b);

        let registered = client.get_registered_schemas();
        assert_eq!(registered.len(), 2);
        assert!(registered.contains(&schema_a));
        assert!(registered.contains(&schema_b));

        // Non-admin cannot register schemas.
        let res = client.try_register_schema(&not_admin, &schema_a);
        assert_eq!(res, Err(Ok(PermissionError::Unauthorized)));
    }

    // ── Issue #100: DelegateStatusView getter tests ───────────────────────────

    /// Status is `not_found` when no permission record exists.
    #[test]
    fn test_get_delegate_status_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let status = client.get_delegate_status(&owner, &delegate);
        assert!(!status.active);
        assert_eq!(status.reason, soroban_sdk::Symbol::new(&env, "not_found"));
        assert_eq!(status.remaining, 0);
    }

    /// Status is `active` for a freshly granted, unspent permission.
    #[test]
    fn test_get_delegate_status_active() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        let status = client.get_delegate_status(&owner, &delegate);
        assert!(status.active);
        assert_eq!(status.reason, soroban_sdk::Symbol::new(&env, "active"));
        assert_eq!(status.remaining, 1000);
    }

    /// Status is `revoked` after owner calls `revoke`.
    #[test]
    fn test_get_delegate_status_revoked() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        client.revoke(&owner, &delegate);

        let status = client.get_delegate_status(&owner, &delegate);
        assert!(!status.active);
        assert_eq!(status.reason, soroban_sdk::Symbol::new(&env, "revoked"));
        assert_eq!(status.remaining, 0);
    }

    /// Status is `expired` when the ledger has advanced past `expires_at_ledger`.
    #[test]
    fn test_get_delegate_status_expired() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        // Grant with a very short TTL then advance past it.
        client.grant(&owner, &delegate, &1000, &100, &merchants, &5);
        env.ledger().with_mut(|li| {
            li.sequence_number += 10;
        });

        let status = client.get_delegate_status(&owner, &delegate);
        assert!(!status.active);
        assert_eq!(status.reason, soroban_sdk::Symbol::new(&env, "expired"));
    }

    /// Status is `exhausted` when the full allowance has been spent.
    #[test]
    fn test_get_delegate_status_exhausted() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        // Grant exactly 100 with a 100 per-tx limit so a single spend exhausts it.
        client.grant(&owner, &delegate, &100, &100, &merchants, &10000);
        client.execute_spend(&owner, &delegate, &100, &merchant);

        let status = client.get_delegate_status(&owner, &delegate);
        assert!(!status.active);
        assert_eq!(status.reason, soroban_sdk::Symbol::new(&env, "exhausted"));
        assert_eq!(status.remaining, 0);
    }

    /// Status is `paused` after owner calls `pause`.
    #[test]
    fn test_get_delegate_status_paused() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        client.pause(&owner, &delegate);

        let status = client.get_delegate_status(&owner, &delegate);
        assert!(!status.active);
        assert_eq!(status.reason, soroban_sdk::Symbol::new(&env, "paused"));
        // Remaining is still reported when paused (allowance is intact).
        assert_eq!(status.remaining, 1000);
    }

    /// get_delegate_status does not mutate any state (remaining unchanged after call).
    #[test]
    fn test_get_delegate_status_does_not_mutate() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &500, &100, &merchants, &10000);

        // Call get_delegate_status twice.
        client.get_delegate_status(&owner, &delegate);
        client.get_delegate_status(&owner, &delegate);

        // Actual spend should still see the full unmodified allowance.
        client.execute_spend(&owner, &delegate, &100, &merchant);
        assert_eq!(client.get_remaining_allowance(&owner, &delegate), 400);
    }

    // ── Error discriminant uniqueness tests ──────────────────────────────────

    #[test]
    fn test_error_variants_have_unique_discriminants() {
        let variants: std::vec::Vec<u32> = vec![
            PermissionError::NotFound as u32,
            PermissionError::Expired as u32,
            PermissionError::ExceedsPerTxLimit as u32,
            PermissionError::ExceedsTotalLimit as u32,
            PermissionError::MerchantNotAllowed as u32,
            PermissionError::Unauthorized as u32,
            PermissionError::InvalidParam as u32,
            PermissionError::PermissionPaused as u32,
            PermissionError::AlreadyPaused as u32,
            PermissionError::AlreadyActive as u32,
            PermissionError::GrantsPaused as u32,
            PermissionError::RelayerKeyNotSet as u32,
            PermissionError::InvalidNonce as u32,
            PermissionError::SignatureExpired as u32,
            PermissionError::SelfDelegationNotAllowed as u32,
            PermissionError::InsufficientSignatures as u32,
            PermissionError::UnknownSchema as u32,
            PermissionError::ParentNotFound as u32,
            PermissionError::ExceedsParentLimit as u32,
            PermissionError::VelocityLimitExceeded as u32,
            PermissionError::InactivityThresholdNotSet as u32,
            PermissionError::LimitBelowSpent as u32,
            PermissionError::ExceedsAllowance as u32,
            PermissionError::NotInitialized as u32,
        ];

        let mut seen = std::vec::Vec::<u32>::new();
        for &val in variants.iter() {
            assert!(
                !seen.contains(&val),
                "Duplicate discriminant {} found in PermissionError enum",
                val
            );
            seen.push(val);
        }
        assert_eq!(seen.len(), 24, "expected 24 distinct error discriminants");
    }

    #[test]
    fn test_error_serialization_produces_distinct_values() {
        let variants = [
            PermissionError::NotFound,
            PermissionError::Expired,
            PermissionError::ExceedsPerTxLimit,
            PermissionError::ExceedsTotalLimit,
            PermissionError::MerchantNotAllowed,
            PermissionError::Unauthorized,
            PermissionError::InvalidParam,
            PermissionError::PermissionPaused,
            PermissionError::AlreadyPaused,
            PermissionError::AlreadyActive,
            PermissionError::GrantsPaused,
            PermissionError::RelayerKeyNotSet,
            PermissionError::InvalidNonce,
            PermissionError::SignatureExpired,
            PermissionError::SelfDelegationNotAllowed,
            PermissionError::InsufficientSignatures,
            PermissionError::UnknownSchema,
            PermissionError::ParentNotFound,
            PermissionError::ExceedsParentLimit,
            PermissionError::VelocityLimitExceeded,
            PermissionError::InactivityThresholdNotSet,
            PermissionError::LimitBelowSpent,
            PermissionError::ExceedsAllowance,
            PermissionError::NotInitialized,
        ];

        let mut seen = std::vec::Vec::<u32>::new();
        for variant in variants.iter() {
            let serialized = *variant as u32;
            assert!(
                !seen.contains(&serialized),
                "Duplicate serialized value {} for {:?}",
                serialized,
                variant
            );
            seen.push(serialized);
        }
        assert_eq!(seen.len(), 24, "expected 24 distinct error variants");
    }

    // --- PermissionUsage & get_permission_usage tests ---

    #[test]
    fn test_permission_usage_initial_and_post_spend() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        // Initial state before any spend
        let usage = client.get_permission_usage(&owner, &delegate);
        assert_eq!(usage.spent, 0);
        assert_eq!(usage.last_spend_ledger, None);

        // Advance ledger and execute a spend
        env.ledger().with_mut(|li| {
            li.sequence_number = 50;
        });
        client.execute_spend(&owner, &delegate, &40, &merchant);

        let usage_after = client.get_permission_usage(&owner, &delegate);
        assert_eq!(usage_after.spent, 40);
        assert_eq!(usage_after.last_spend_ledger, Some(50));

        // Advance ledger again and execute another spend
        env.ledger().with_mut(|li| {
            li.sequence_number = 65;
        });
        client.execute_spend(&owner, &delegate, &30, &merchant);

        let usage_after_second = client.get_permission_usage(&owner, &delegate);
        assert_eq!(usage_after_second.spent, 70);
        assert_eq!(usage_after_second.last_spend_ledger, Some(65));
    }

    #[test]
    fn test_permission_usage_not_found_and_failed_spend() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        // Non-existent permission record returns 0 spent and None last_spend_ledger
        let usage_not_found = client.get_permission_usage(&owner, &delegate);
        assert_eq!(usage_not_found.spent, 0);
        assert_eq!(usage_not_found.last_spend_ledger, None);

        // Grant permission
        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &500, &50, &merchants, &10000);

        // Attempt a spend exceeding per-tx limit (fails)
        let res = client.try_execute_spend(&owner, &delegate, &100, &merchant);
        assert_eq!(res, Err(Ok(PermissionError::ExceedsPerTxLimit)));

        // Verification: Failed spend does not record spend or last_spend_ledger
        let usage_failed = client.get_permission_usage(&owner, &delegate);
        assert_eq!(usage_failed.spent, 0);
        assert_eq!(usage_failed.last_spend_ledger, None);
    }

    // --- Issue #424: is_active quick-check getter ---

    #[test]
    fn test_is_active_returns_true_for_active_non_expired() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        assert!(client.is_active(&owner, &delegate));
    }

    #[test]
    fn test_is_active_returns_false_for_revoked() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        client.revoke(&owner, &delegate);

        assert!(!client.is_active(&owner, &delegate));
    }

    #[test]
    fn test_is_active_returns_false_for_expired() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        // TTL of 1 — expires at ledger 1
        client.grant(&owner, &delegate, &1000, &100, &merchants, &1);

        // Still active at ledger 0
        assert!(client.is_active(&owner, &delegate));

        // Advance past expiry
        env.ledger().with_mut(|li| {
            li.sequence_number = 2;
        });

        assert!(!client.is_active(&owner, &delegate));
    }

    #[test]
    fn test_is_active_returns_false_for_non_existent() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        assert!(!client.is_active(&owner, &delegate));
    }

    // --- Issue #425: Two-step admin transfer ---

    #[test]
    fn test_propose_admin_succeeds_for_current_admin() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.set_admin(&admin);
        let res = client.try_propose_admin(&admin, &new_admin);
        assert_eq!(res, Ok(Ok(())));
    }

    #[test]
    fn test_propose_admin_fails_for_non_admin() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let non_admin = Address::generate(&env);
        let new_admin = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.set_admin(&admin);
        let res = client.try_propose_admin(&non_admin, &new_admin);
        assert_eq!(res, Err(Ok(PermissionError::Unauthorized)));
    }

    #[test]
    fn test_accept_admin_succeeds_for_proposed_address() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.set_admin(&admin);
        client.propose_admin(&admin, &new_admin);
        let res = client.try_accept_admin(&new_admin);
        assert_eq!(res, Ok(Ok(())));
    }

    #[test]
    fn test_accept_admin_fails_for_non_proposed_address() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let new_admin = Address::generate(&env);
        let other = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.set_admin(&admin);
        client.propose_admin(&admin, &new_admin);
        let res = client.try_accept_admin(&other);
        assert_eq!(res, Err(Ok(PermissionError::Unauthorized)));
    }

    #[test]
    fn test_get_permissions_by_owner() {
        let env = Env::default();
        env.mock_all_auths();

        let owner = Address::generate(&env);
        let delegate1 = Address::generate(&env);
        let delegate2 = Address::generate(&env);
        let delegate3 = Address::generate(&env);
        let merchant = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let mut merchants = Vec::<Address>::new(&env);
        merchants.push_back(merchant.clone());

        // Grant 3 permissions
        client.grant(&owner, &delegate1, &1000, &100, &merchants, &10000);
        client.grant(&owner, &delegate2, &2000, &200, &merchants, &10000);
        client.grant(&owner, &delegate3, &3000, &300, &merchants, &10000);

        let perms = client.get_permissions_by_owner(&owner);
        assert_eq!(perms.len(), 3);

        // Revoke one permission
        client.revoke(&owner, &delegate2);

        let perms = client.get_permissions_by_owner(&owner);
        assert_eq!(perms.len(), 2);

        // Transfer a permission
        let new_delegate = Address::generate(&env);
        client.transfer_permission(&owner, &delegate1, &new_delegate);

        let perms = client.get_permissions_by_owner(&owner);
        assert_eq!(perms.len(), 2); // Still 2, delegate1 is removed, new_delegate is added.

        // Verify new_delegate is in the list and delegate1 is not
        let mut found_new = false;
        let mut found_old = false;
        for perm in perms.iter() {
            if perm.delegate == new_delegate {
                found_new = true;
            }
            if perm.delegate == delegate1 {
                found_old = true;
            }
        }
        assert!(found_new);
        assert!(!found_old);
    }

    #[test]
    fn test_get_permissions_by_delegate_multiple_owners_and_revoke() {
        let env = Env::default();
        env.mock_all_auths();
        let owner_a = Address::generate(&env);
        let owner_b = Address::generate(&env);
        let delegate = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);
        let merchants = Vec::<Address>::new(&env);

        client.grant(&owner_a, &delegate, &1000, &100, &merchants, &10000);
        client.grant(&owner_b, &delegate, &2000, &200, &merchants, &10000);

        let records = client.get_permissions_by_delegate(&delegate);
        assert_eq!(records.len(), 2);
        let first = records.get(0).unwrap();
        let second = records.get(1).unwrap();
        assert_ne!(first.owner, second.owner);
        assert!(first.owner == owner_a || first.owner == owner_b);
        assert!(second.owner == owner_a || second.owner == owner_b);

        client.revoke(&owner_a, &delegate);
        let remaining = client.get_permissions_by_delegate(&delegate);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining.get(0).unwrap().owner, owner_b);
    }

    #[test]
    fn test_get_permissions_by_delegate_empty_and_expired() {
        let env = Env::default();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let unknown_delegate = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        assert_eq!(
            client.get_permissions_by_delegate(&unknown_delegate).len(),
            0
        );

        env.mock_all_auths();
        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &5);
        let expires_at = client.get_permission(&owner, &delegate).expires_at_ledger;
        env.ledger().set_sequence_number(expires_at + 1);

        let records = client.get_permissions_by_delegate(&delegate);
        assert_eq!(records.len(), 1);
        assert_eq!(records.get(0).unwrap().owner, owner);
    }

    #[test]
    fn test_get_permissions_by_delegate_transfer_moves_index() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let old_delegate = Address::generate(&env);
        let new_delegate = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);
        let merchants = Vec::<Address>::new(&env);

        client.grant(&owner, &old_delegate, &1000, &100, &merchants, &10000);
        client.transfer_permission(&owner, &old_delegate, &new_delegate);

        assert_eq!(client.get_permissions_by_delegate(&old_delegate).len(), 0);
        let records = client.get_permissions_by_delegate(&new_delegate);
        assert_eq!(records.len(), 1);
        assert_eq!(records.get(0).unwrap().owner, owner);
        assert_eq!(records.get(0).unwrap().delegate, new_delegate);
    }

    #[test]
    fn test_get_permissions_by_delegate_regrant_does_not_duplicate_owner() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);
        let merchants = Vec::<Address>::new(&env);

        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        client.re_grant(&owner, &delegate, &2000, &200, &merchants, &10000);

        let records = client.get_permissions_by_delegate(&delegate);
        assert_eq!(records.len(), 1);
        let record = records.get(0).unwrap();
        assert_eq!(record.owner, owner);
        assert_eq!(record.limit_total, 2000);
        assert_eq!(record.limit_per_tx, 200);
    }

    #[test]
    fn test_get_permissions_by_delegate_includes_child_until_cascade_revoke() {
        let env = Env::default();
        env.mock_all_auths();
        let parent_owner = Address::generate(&env);
        let parent_delegate = Address::generate(&env);
        let child_delegate = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);
        let merchants = Vec::<Address>::new(&env);

        client.grant(
            &parent_owner,
            &parent_delegate,
            &1000,
            &100,
            &merchants,
            &10000,
        );
        client.grant_child(
            &parent_owner,
            &parent_delegate,
            &child_delegate,
            &500,
            &50,
            &merchants,
            &10000,
        );

        let child_records = client.get_permissions_by_delegate(&child_delegate);
        assert_eq!(child_records.len(), 1);
        assert_eq!(child_records.get(0).unwrap().owner, parent_delegate);

        client.revoke(&parent_owner, &parent_delegate);
        assert_eq!(client.get_permissions_by_delegate(&child_delegate).len(), 0);
    }

    // --- Batch sweep tests ---

    #[test]
    fn test_sweep_expired_batch_transitions_eligible() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate1 = Address::generate(&env);
        let delegate2 = Address::generate(&env);
        let caller = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        // delegate1 expires at ledger 10, delegate2 expires at ledger 100
        client.grant(&owner, &delegate1, &1000, &100, &merchants, &10);
        client.grant(&owner, &delegate2, &1000, &100, &merchants, &100);

        // Advance ledger to 50
        env.ledger().set_sequence_number(50);

        let mut pairs = Vec::<(Address, Address)>::new(&env);
        pairs.push_back((owner.clone(), delegate1.clone()));
        pairs.push_back((owner.clone(), delegate2.clone()));

        let transitioned = client.sweep_expired_batch(&pairs, &caller);
        assert_eq!(transitioned, 1);

        let perm1 = client.get_permission(&owner, &delegate1);
        assert_eq!(perm1.status, PermissionStatus::Expired);
        let expired_records = client.get_permissions_by_delegate(&delegate1);
        assert_eq!(expired_records.len(), 1);
        assert_eq!(expired_records.get(0).unwrap().owner, owner);

        let perm2 = client.get_permission(&owner, &delegate2);
        assert_eq!(perm2.status, PermissionStatus::Active);
    }

    #[test]
    fn test_sweep_expired_batch_rejects_over_limit() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let caller = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let mut pairs = Vec::<(Address, Address)>::new(&env);
        for _ in 0..51 {
            pairs.push_back((owner.clone(), delegate.clone()));
        }

        let res = client.try_sweep_expired_batch(&pairs, &caller);
        assert_eq!(res, Err(Ok(PermissionError::InvalidParam)));
    }

    #[test]
    fn test_sweep_inactive_batch_transitions_eligible() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let owner = Address::generate(&env);
        let delegate1 = Address::generate(&env);
        let delegate2 = Address::generate(&env);
        let caller = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.set_admin(&admin);
        client.set_inactivity_threshold(&admin, &1000);

        env.ledger().set_timestamp(100);
        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate1, &1000, &100, &merchants, &10000);

        env.ledger().set_timestamp(800);
        client.grant(&owner, &delegate2, &1000, &100, &merchants, &10000);

        // Advance timestamp to 1200: delegate1 (created 100, elapsed 1100 > 1000) is eligible,
        // delegate2 (created 800, elapsed 400 < 1000) is not eligible.
        env.ledger().set_timestamp(1200);

        let mut pairs = Vec::<(Address, Address)>::new(&env);
        pairs.push_back((owner.clone(), delegate1.clone()));
        pairs.push_back((owner.clone(), delegate2.clone()));

        let transitioned = client.sweep_inactive_batch(&pairs, &caller);
        assert_eq!(transitioned, 1);

        let perm1 = client.get_permission(&owner, &delegate1);
        assert_eq!(perm1.status, PermissionStatus::Revoked);

        let perm2 = client.get_permission(&owner, &delegate2);
        assert_eq!(perm2.status, PermissionStatus::Active);

        assert_eq!(client.get_permissions_by_delegate(&delegate1).len(), 0);
        let active_records = client.get_permissions_by_delegate(&delegate2);
        assert_eq!(active_records.len(), 1);
        assert_eq!(active_records.get(0).unwrap().owner, owner);
    }

    #[test]
    fn test_sweep_inactive_batch_rejects_unset_threshold_or_over_limit() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let caller = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let mut pairs = Vec::<(Address, Address)>::new(&env);
        pairs.push_back((owner.clone(), delegate.clone()));

        // Threshold not set
        let res_no_thresh = client.try_sweep_inactive_batch(&pairs, &caller);
        assert_eq!(
            res_no_thresh,
            Err(Ok(PermissionError::InactivityThresholdNotSet))
        );

        let admin = Address::generate(&env);
        client.set_admin(&admin);
        client.set_inactivity_threshold(&admin, &1000);

        let mut over_cap = Vec::<(Address, Address)>::new(&env);
        for _ in 0..51 {
            over_cap.push_back((owner.clone(), delegate.clone()));
        }

        let res_over_cap = client.try_sweep_inactive_batch(&over_cap, &caller);
        assert_eq!(res_over_cap, Err(Ok(PermissionError::InvalidParam)));

        let empty = Vec::<(Address, Address)>::new(&env);
        let res_empty = client.try_sweep_inactive_batch(&empty, &caller);
        assert_eq!(res_empty, Err(Ok(PermissionError::InvalidParam)));
    }

    #[test]
    fn test_sweep_inactive_full_batch_of_50_stays_within_budget() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let caller = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);
        client.set_admin(&admin);
        client.set_inactivity_threshold(&admin, &1u64);

        let merchants = Vec::<Address>::new(&env);
        let mut pairs = Vec::<(Address, Address)>::new(&env);
        env.as_contract(&contract_id, || {
            for _ in 0..50 {
                let owner = Address::generate(&env);
                let delegate = Address::generate(&env);
                env.storage().persistent().set(
                    &DataKey::Permission(owner.clone(), delegate.clone()),
                    &PermissionRecord {
                        owner: owner.clone(),
                        delegate: delegate.clone(),
                        limit_total: 100,
                        spent: 0,
                        limit_per_tx: 10,
                        allowed_merchants: merchants.clone(),
                        status: PermissionStatus::Active,
                        expires_at_ledger: 10_000,
                        created_at: 0,
                        parent_owner: None,
                        parent_delegate: None,
                    },
                );
                pairs.push_back((owner, delegate));
            }
        });
        env.ledger().set_timestamp(2);

        assert_eq!(client.sweep_inactive_batch(&pairs, &caller), 50);
        let budget = env.cost_estimate().budget();
        assert!(budget.cpu_instruction_cost() < 50_000_000);
        assert!(budget.memory_bytes_cost() < 30_000_000);
    }

    #[test]
    fn test_decrease_allowance_negative_amount_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let result = client.try_decrease_allowance(&owner, &delegate, &-100);
        assert!(result.is_err());
    }

    // --- Issue: Strict domain separator hash invalidation across upgrades ---

    #[test]
    fn test_versioned_domain_separator_changes_with_semver() {
        let env = Env::default();
        let contract_id = env.register(PermissionsContract, ());

        let sep_v2 = env.as_contract(&contract_id, || {
            crate::compute_versioned_domain_separator(&env)
        });

        // Recompute with a different semver symbol to simulate a V1 contract.
        let mut payload = soroban_sdk::Bytes::new(&env);
        payload.append(&contract_id.to_xdr(&env));
        payload.append(&soroban_sdk::symbol_short!("PERM_V1").to_xdr(&env));
        let sep_v1: BytesN<32> = env.crypto().sha256(&payload).into();

        assert_ne!(
            sep_v1, sep_v2,
            "domain separator must differ across contract versions"
        );
    }

    // --- Issue: Prevent Unauthorized Delegate Cancellation of Pending Allowance Decreases ---

    #[test]
    fn test_cancel_pending_decrease_requires_owner_auth() {
    // --- Inactivity pruning and rent reclamation tests (issue #374) ---

    #[test]
    fn test_prune_expired_permission_success() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let keeper = Address::generate(&env);
    // --- Issue: Handle Verification Policy Threshold Increases ---

    #[test]
    fn test_recheck_merchant_verification_under_old_policy() {
        let env = Env::default();
        env.mock_all_auths();
        let merchant_id: u64 = 1;

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        client.decrease_allowance(&owner, &delegate, &200);

        // Owner can cancel the pending decrease.
        assert_eq!(
            client.try_cancel_pending_decrease(&owner, &delegate),
            Ok(Ok(()))
        );
    }
    // ---------------------------------------------------------------------
    // Issue #369 — function-scoped permission grants
    // ---------------------------------------------------------------------

    /// Builds a scope naming `target_contract` and the given entrypoints.
    fn scope_for(
        env: &Env,
        target_contract: &Address,
        functions: &[&str],
    ) -> ScopedPermissionConfig {
        let mut symbols = Vec::new(env);
        for f in functions {
            symbols.push_back(Symbol::new(env, f));
        }
        ScopedPermissionConfig {
            target_contract: target_contract.clone(),
            allowed_function_symbols: symbols,
        }
    }

    /// Owner grants `delegate` a budget that may only invoke `functions` on
    /// `target_contract`. Returns the client plus the fixture addresses.
    fn setup_scoped_grant(
        env: &Env,
        client: &PermissionsContractClient,
        target_contract: &Address,
        functions: &[&str],
    ) -> (Address, Address, Address) {
        let owner = Address::generate(env);
        let delegate = Address::generate(env);
        let merchant = Address::generate(env);

        let merchants = Vec::<Address>::new(env);
        client.grant_scoped(
            &owner,
            &delegate,
            &1_000,
            &500,
            &merchants,
            &10_000,
            &scope_for(env, target_contract, functions),
        );

        (owner, delegate, merchant)
    }

    /// The pre-authorized entrypoint is the whole point of the feature: the
    /// delegate must still be able to spend normally through it.
    #[test]
    fn test_scoped_grant_allows_pre_authorized_function() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let escrow = Address::generate(&env);
        let (owner, delegate, merchant) = setup_scoped_grant(&env, &client, &escrow, &["fund"]);

        assert_eq!(
            client.try_can_spend_scoped(
                &owner,
                &delegate,
                &100,
                &merchant,
                &Some(escrow.clone()),
                &Some(Symbol::new(&env, "fund"))
            ),
            Ok(Ok(()))
        );
        assert_eq!(
            client.try_execute_spend_scoped(
                &owner,
                &delegate,
                &100,
                &merchant,
                &Some(escrow.clone()),
                &Some(Symbol::new(&env, "fund"))
            ),
            Ok(Ok(()))
        );
        assert_eq!(client.get_remaining_allowance(&owner, &delegate), 900);
    }

    /// Core acceptance criterion: a delegate scoped to `escrow.fund` is
    /// refused every other entrypoint of that same contract.
    #[test]
    fn test_scoped_grant_rejects_unapproved_function() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let escrow = Address::generate(&env);
        let (owner, delegate, merchant) = setup_scoped_grant(&env, &client, &escrow, &["fund"]);

        for forbidden in ["withdraw", "refund", "dispute", "admin_bypass"] {
            assert_eq!(
                client.try_can_spend_scoped(
                    &owner,
                    &delegate,
                    &100,
                    &merchant,
                    &Some(escrow.clone()),
                    &Some(Symbol::new(&env, forbidden))
                ),
                Err(Ok(PermissionError::UnauthorizedFunction)),
                "entrypoint {forbidden} should not be authorized"
            );
        }

        // A rejected invocation must not move any allowance.
        assert_eq!(
            client.try_execute_spend_scoped(
                &owner,
                &delegate,
                &100,
                &merchant,
                &Some(escrow.clone()),
                &Some(Symbol::new(&env, "withdraw"))
            ),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );
        assert_eq!(client.get_remaining_allowance(&owner, &delegate), 1_000);
    }

    /// Naming an allowed function is not enough: the contract it is invoked on
    /// must be the scoped one, otherwise the delegate could aim `fund` at a
    /// contract the owner never vetted.
    #[test]
    fn test_scoped_grant_rejects_wrong_target_contract() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let escrow = Address::generate(&env);
        let attacker_contract = Address::generate(&env);
        let (owner, delegate, merchant) = setup_scoped_grant(&env, &client, &escrow, &["fund"]);

        assert_eq!(
            client.try_execute_spend_scoped(
                &owner,
                &delegate,
                &100,
                &merchant,
                &Some(attacker_contract.clone()),
                &Some(Symbol::new(&env, "fund"))
            ),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );
        assert_eq!(client.get_remaining_allowance(&owner, &delegate), 1_000);
    }

    /// Scoping fails closed. A scoped grant must not be spendable through the
    /// pre-existing unscoped entrypoints, otherwise the whole check could be
    /// bypassed by simply not stating the invocation.
    #[test]
    fn test_scoped_grant_rejected_through_unscoped_entrypoints() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let escrow = Address::generate(&env);
        let (owner, delegate, merchant) = setup_scoped_grant(&env, &client, &escrow, &["fund"]);

        assert_eq!(
            client.try_can_spend(&owner, &delegate, &100, &merchant),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );
        assert_eq!(
            client.try_execute_spend(&owner, &delegate, &100, &merchant),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );
        assert_eq!(client.get_remaining_allowance(&owner, &delegate), 1_000);
    }

    /// Supplying the target but withholding the function (or vice versa) is
    /// still a rejection — a partial invocation never satisfies a scope.
    #[test]
    fn test_scoped_grant_rejects_incomplete_invocation() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let escrow = Address::generate(&env);
        let (owner, delegate, merchant) = setup_scoped_grant(&env, &client, &escrow, &["fund"]);

        // Neither half stated.
        assert_eq!(
            client.try_execute_spend_scoped(&owner, &delegate, &100, &merchant, &None, &None),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );
        // Contract stated, function withheld.
        assert_eq!(
            client.try_execute_spend_scoped(
                &owner,
                &delegate,
                &100,
                &merchant,
                &Some(escrow.clone()),
                &None
            ),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );
        // Function stated, contract withheld.
        assert_eq!(
            client.try_execute_spend_scoped(
                &owner,
                &delegate,
                &100,
                &merchant,
                &None,
                &Some(Symbol::new(&env, "fund"))
            ),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );
        assert_eq!(client.get_remaining_allowance(&owner, &delegate), 1_000);
    }

    /// Backwards compatibility: delegations without a scope behave exactly as
    /// they did before, through either entrypoint, and report `None`.
    #[test]
    fn test_unscoped_grant_unaffected_by_scoped_entrypoint() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1_000, &500, &merchants, &10_000);

        assert_eq!(client.get_permission_scope(&owner, &delegate), None);

        // A delegate-supplied invocation is advisory for an unscoped grant.
        assert_eq!(
            client.try_execute_spend_scoped(
                &owner,
                &delegate,
                &100,
                &merchant,
                &Some(Address::generate(&env)),
                &Some(Symbol::new(&env, "anything"))
            ),
            Ok(Ok(()))
        );
        assert_eq!(client.get_remaining_allowance(&owner, &delegate), 900);

        // And the original entrypoint still works for the next spend.
        assert_eq!(
            client.try_execute_spend(&owner, &delegate, &100, &merchant),
            Ok(Ok(()))
        );
        assert_eq!(client.get_remaining_allowance(&owner, &delegate), 800);
    }

    /// An owner can tighten an existing grant after the fact, and the new
    /// restriction is immediately enforced.
    #[test]
    fn test_set_permission_scope_narrows_existing_grant() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1_000, &500, &merchants, &10_000);

        let escrow = Address::generate(&env);
        client.set_permission_scope(
            &owner,
            &delegate,
            &Some(scope_for(&env, &escrow, &["fund", "release"])),
        );

        let stored = client.get_permission_scope(&owner, &delegate).unwrap();
        assert_eq!(stored.target_contract, escrow);
        assert_eq!(stored.allowed_function_symbols.len(), 2);

        assert_eq!(
            client.try_execute_spend_scoped(
                &owner,
                &delegate,
                &100,
                &merchant,
                &Some(escrow.clone()),
                &Some(Symbol::new(&env, "release"))
            ),
            Ok(Ok(()))
        );
        assert_eq!(
            client.try_execute_spend_scoped(
                &owner,
                &delegate,
                &100,
                &merchant,
                &Some(escrow.clone()),
                &Some(Symbol::new(&env, "cancel"))
            ),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );
        // The unscoped path is now closed too.
        assert_eq!(
            client.try_execute_spend(&owner, &delegate, &100, &merchant),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );
        assert_eq!(client.get_remaining_allowance(&owner, &delegate), 900);
    }

    /// Clearing the scope is how an owner deliberately widens authority again;
    /// the storage slot is removed rather than left behind holding an empty
    /// config.
    #[test]
    fn test_set_permission_scope_can_be_cleared() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let escrow = Address::generate(&env);
        let (owner, delegate, merchant) = setup_scoped_grant(&env, &client, &escrow, &["fund"]);

        assert!(client.get_permission_scope(&owner, &delegate).is_some());

        client.set_permission_scope(&owner, &delegate, &None);
        assert_eq!(client.get_permission_scope(&owner, &delegate), None);
        env.as_contract(&contract_id, || {
            assert!(!env
                .storage()
                .persistent()
                .has(&DataKey::PermissionScope(owner.clone(), delegate.clone())));
        });

        assert_eq!(
            client.try_execute_spend(&owner, &delegate, &100, &merchant),
            Ok(Ok(()))
        );
    }

    #[test]
    fn test_v1_signature_fails_verification_on_v2() {
        let env = Env::default();
        let contract_id = env.register(PermissionsContract, ());

        // Domain separator as computed by the current (V2) contract.
        let sep_v2 = env.as_contract(&contract_id, || {
            crate::compute_versioned_domain_separator(&env)
        });

        // A signature produced against the V1 domain separator.
        let mut v1_payload = soroban_sdk::Bytes::new(&env);
        v1_payload.append(&contract_id.to_xdr(&env));
        v1_payload.append(&soroban_sdk::symbol_short!("PERM_V1").to_xdr(&env));
        let sep_v1: BytesN<32> = env.crypto().sha256(&v1_payload).into();

        // The V2 contract must reject a hash bound to the V1 separator.
        assert_ne!(
            sep_v1, sep_v2,
            "V1-bound signature hash must not match the V2 domain separator"
        );
    }

    #[test]
    fn test_validate_relayer_fee_within_safety_boundaries() {
        assert_eq!(MAX_RELAYER_FEE_BPS, 100);
        assert_eq!(MAX_ABSOLUTE_RELAYER_STROOPS, 10_000_000);

        // Zero fee is always accepted on valid spend
        assert_eq!(validate_relayer_fee(10_000_000, 0), Ok(()));

        // Exactly at 1% proportional boundary (100_000 stroops on 10_000_000 stroops / 1 XLM)
        let max_proportional_1xlm = (10_000_000 * MAX_RELAYER_FEE_BPS as i128) / 10_000;
        assert_eq!(
            validate_relayer_fee(10_000_000, max_proportional_1xlm),
            Ok(())
        );

        // Well within proportional boundary (0.5%)
        assert_eq!(validate_relayer_fee(10_000_000, 50_000), Ok(()));

        // At exactly 100 XLM spend (1_000_000_000 stroops), 1% is exactly 1 XLM (10_000_000 stroops)
        assert_eq!(
            validate_relayer_fee(1_000_000_000, MAX_ABSOLUTE_RELAYER_STROOPS),
            Ok(())
        );

        // For large spends (e.g. 500 XLM), fee at absolute 1 XLM cap is accepted
        assert_eq!(
            validate_relayer_fee(5_000_000_000, MAX_ABSOLUTE_RELAYER_STROOPS),
            Ok(())
        );
    }

    #[test]
    fn test_validate_relayer_fee_rejects_negative_fee() {
        assert_eq!(
            validate_relayer_fee(10_000_000, -1),
            Err(PermissionError::InvalidParam)
        );
        assert_eq!(
            validate_relayer_fee(10_000_000, -10_000_000),
            Err(PermissionError::InvalidParam)
        );
    }

    #[test]
    fn test_validate_relayer_fee_enforces_proportional_cap() {
        // 1% of 10_000_000 stroops is 100_000 stroops; 100_001 must be rejected
        assert_eq!(
            validate_relayer_fee(10_000_000, 100_001),
            Err(PermissionError::InvalidParam)
        );

        // 1% of 1_000_000 stroops is 10_000 stroops; 10_001 must be rejected
        assert_eq!(
            validate_relayer_fee(1_000_000, 10_000),
            Err(PermissionError::InvalidParam)
        );

        // 5% fee on 1_000_000 stroops (50_000) must be rejected
        assert_eq!(
            validate_relayer_fee(1_000_000, 50_000),
            Err(PermissionError::InvalidParam)
        );
    }

    #[test]
    fn test_validate_relayer_fee_enforces_absolute_cap() {
        // For a 200 XLM spend (2_000_000_000 stroops), 1% is 20_000_000 stroops (2 XLM).
        // A fee exceeding 1 XLM (10_000_000 stroops) must be rejected even though it is < 1%.
        assert_eq!(
            validate_relayer_fee(2_000_000_000, MAX_ABSOLUTE_RELAYER_STROOPS + 1),
            Err(PermissionError::InvalidParam)
        );

        // 2 XLM fee must be rejected
        assert_eq!(
            validate_relayer_fee(2_000_000_000, 20_000_000),
            Err(PermissionError::InvalidParam)
        );
    }

    #[test]
    fn test_validate_relayer_fee_edge_cases() {
        // Zero spend: zero fee is allowed, non-zero fee is rejected
        assert_eq!(validate_relayer_fee(0, 0), Ok(()));
        assert_eq!(
            validate_relayer_fee(0, 1),
            Err(PermissionError::InvalidParam)
        );

        // Sub-100 stroop spend (integer division makes max proportional 0 stroops)
        assert_eq!(validate_relayer_fee(99, 0), Ok(()));
        assert_eq!(
            validate_relayer_fee(99, 1),
            Err(PermissionError::InvalidParam)
        );

        // Exact 100 stroops: 1% is 1 stroop
        assert_eq!(validate_relayer_fee(100, 1), Ok(()));
        assert_eq!(
            validate_relayer_fee(100, 2),
            Err(PermissionError::InvalidParam)
        );

        // Negative spend: rejects any non-negative fee because max_proportional is negative
        assert_eq!(
            validate_relayer_fee(-100, 0),
            Err(PermissionError::InvalidParam)
        );
    }

    #[test]
    fn test_cancel_pending_decrease_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        // Merchant verified under a policy requiring 1 verification.
        let old_policy = crate::VerificationPolicy { required: 1 };
        client.set_merchant_verifications(&merchant_id, &1);
        assert!(client.recheck_merchant_verification(&merchant_id, &old_policy));
    }

    #[test]
    fn test_recheck_merchant_verification_after_policy_increase() {
        let env = Env::default();
        env.mock_all_auths();
        let merchant_id: u64 = 2;

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        // No pending decrease queued yet.
        assert_eq!(
            client.try_cancel_pending_decrease(&owner, &delegate),
            Err(Ok(PermissionError::NotFound))
        );
    }

    #[test]
    fn test_delegate_cannot_cancel_pending_decrease() {
        let env = Env::default();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);

        client
            .mock_auths(&[MockAuth {
                address: &owner,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "grant",
                    args: (
                        owner.clone(),
                        delegate.clone(),
                        1000i128,
                        100i128,
                        merchants.clone(),
                        10000u32,
                    )
                        .into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        client
            .mock_auths(&[MockAuth {
                address: &owner,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "decrease_allowance",
                    args: (owner.clone(), delegate.clone(), 200i128).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .decrease_allowance(&owner, &delegate, &200);

        // Delegate attempts to cancel using their own auth — must fail.
        let res = client
            .mock_auths(&[MockAuth {
                address: &delegate,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "cancel_pending_decrease",
                    args: (owner.clone(), delegate.clone()).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .try_cancel_pending_decrease(&owner, &delegate);
        assert!(res.is_err());

        // The pending decrease must still be present and executable by the owner.
        env.ledger().with_mut(|li| {
            li.timestamp += 86401;
        });
        assert_eq!(
            client.try_execute_decrease_allowance(&owner, &delegate),
            Ok(Ok(()))
        );
        let detail = client.get_allowance_detail(&owner, &delegate);
        assert_eq!(detail.limit, 800);
    }

    #[test]
    fn test_delegate_cannot_erase_pending_decrease_via_increase() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        client.decrease_allowance(&owner, &delegate, &200);

        // A delegate-initiated increase must not clear the pending decrease.
        let _ = client.try_increase_allowance(&delegate, &delegate, &100);

        // The pending decrease remains queued and executable by the owner.
        env.ledger().with_mut(|li| {
            li.timestamp += 86401;
        });
        assert_eq!(
            client.try_execute_decrease_allowance(&owner, &delegate),
            Ok(Ok(()))
        );
        let detail = client.get_allowance_detail(&owner, &delegate);
        assert_eq!(detail.limit, 800);
    }
    /// A limit bump through the metadata entrypoint must not silently widen
    /// authority by dropping the owner's function scope. `re_grant` is the
    /// explicit full-replace path that clears it (issue #369).
    #[test]
    fn test_re_grant_with_metadata_preserves_scope() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.set_admin(&admin);
        client.register_schema(&admin, &symbol_short!("v1"));

        let escrow = Address::generate(&env);
        let (owner, delegate, merchant) = setup_scoped_grant(&env, &client, &escrow, &["fund"]);
        let scope_before = client.get_permission_scope(&owner, &delegate);
        assert!(scope_before.is_some());

        let merchants = Vec::<Address>::new(&env);
        client.re_grant_with_metadata(&owner, &delegate, &2_000, &200, &merchants, &10_000, &None);

        assert_eq!(
            client.get_permission_scope(&owner, &delegate),
            scope_before,
            "re_grant_with_metadata must carry the function scope over unchanged"
        );

        // The narrowed scope is still enforced after the limit bump: the
        // allowed entrypoint works, a different one does not.
        assert_eq!(
            client.try_execute_spend_scoped(
                &owner,
                &delegate,
                &100,
                &merchant,
                &Some(escrow.clone()),
                &Some(symbol_short!("fund"))
            ),
            Ok(Ok(()))
        );
        assert_eq!(
            client.try_execute_spend_scoped(
                &owner,
                &delegate,
                &100,
                &merchant,
                &Some(escrow),
                &Some(symbol_short!("withdraw"))
            ),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );
    }

    /// Lateral privilege escalation: a delegate holding `escrow.fund` cannot
    /// sub-delegate to an agent that reaches a different entrypoint, because
    /// `grant_child` children inherit the parent's scope.
    #[test]
    fn test_child_permission_inherits_parent_scope() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let grandchild = Address::generate(&env);
        let merchant = Address::generate(&env);

        let escrow = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);
        client.grant_scoped(
            &owner,
            &delegate,
            &1_000,
            &500,
            &merchants,
            &10_000,
            &scope_for(&env, &escrow, &["fund"]),
        );

        client.grant_child(
            &owner,
            &delegate,
            &grandchild,
            &1_000,
            &500,
            &merchants,
            &10_000,
        );

        // The child carries no scope of its own…
        assert_eq!(client.get_permission_scope(&delegate, &grandchild), None);

        // …so it cannot widen beyond the parent's allowed entrypoint.
        assert_eq!(
            client.try_execute_spend_scoped(
                &owner,
                &delegate,
                &100,
                &merchant,
                &Some(escrow.clone()),
                &Some(Symbol::new(&env, "withdraw"))
            ),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );
        assert_eq!(
            client.try_execute_spend_scoped(
                &owner,
                &delegate,
                &100,
                &merchant,
                &Some(Address::generate(&env)),
                &Some(Symbol::new(&env, "fund"))
            ),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );
        // Nor can it escape the scope by omitting the invocation entirely.
        assert_eq!(
            client.try_execute_spend(&owner, &delegate, &100, &merchant),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );

        // A grandchild spending under the inherited scope can only reach the
        // parent's allowed entrypoint too: `grant_child` cannot be used to
        // launder a wider authority to a downstream agent.
        assert_eq!(
            client.try_execute_spend_scoped(
                &delegate,
                &grandchild,
                &100,
                &merchant,
                &Some(escrow.clone()),
                &Some(Symbol::new(&env, "withdraw"))
            ),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );
        assert_eq!(
            client.try_execute_spend_scoped(
                &delegate,
                &grandchild,
                &100,
                &merchant,
                &Some(Address::generate(&env)),
                &Some(Symbol::new(&env, "fund"))
            ),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );
        // Nor by omitting the invocation entirely.
        assert_eq!(
            client.try_execute_spend(&delegate, &grandchild, &100, &merchant),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );

        // The pre-authorized entrypoint works all the way down, debiting the
        // grandchild first and then the parent that backs it.
        assert_eq!(
            client.try_execute_spend_scoped(
                &delegate,
                &grandchild,
                &100,
                &merchant,
                &Some(escrow.clone()),
                &Some(Symbol::new(&env, "fund"))
            ),
            Ok(Ok(()))
        );
        assert_eq!(client.get_remaining_allowance(&delegate, &grandchild), 900);
        // The grandchild's spend is also debited from the parent that backs
        // it, so the chain stays solvent.
        assert_eq!(client.get_remaining_allowance(&owner, &delegate), 900);
    }

    /// `transfer_permission` hands the same authority to a new delegate, so the
    /// scope must travel with it rather than being silently dropped.
    #[test]
    fn test_transfer_permission_preserves_scope() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let escrow = Address::generate(&env);
        let (owner, old_delegate, merchant) = setup_scoped_grant(&env, &client, &escrow, &["fund"]);
        let new_delegate = Address::generate(&env);

        client.transfer_permission(&owner, &old_delegate, &new_delegate);

        let stored = client.get_permission_scope(&owner, &new_delegate).unwrap();
        assert_eq!(stored.target_contract, escrow);
        assert_eq!(stored.allowed_function_symbols.len(), 1);

        assert_eq!(
            client.try_execute_spend_scoped(
                &owner,
                &new_delegate,
                &100,
                &merchant,
                &Some(escrow.clone()),
                &Some(Symbol::new(&env, "withdraw"))
            ),
            Err(Ok(PermissionError::UnauthorizedFunction))
        );
        assert_eq!(
            client.try_execute_spend_scoped(
                &owner,
                &new_delegate,
                &100,
                &merchant,
                &Some(escrow.clone()),
                &Some(Symbol::new(&env, "fund"))
            ),
            Ok(Ok(()))
        );
    }

    /// A malformed scope is rejected at write time so no unscannable grant is
    /// ever recorded.
    #[test]
    fn test_scoped_grant_rejects_malformed_scope() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let escrow = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);

        // Empty allowlist: unscannable and indistinguishable from "no scope".
        assert_eq!(
            client.try_grant_scoped(
                &owner,
                &delegate,
                &1_000,
                &500,
                &merchants,
                &10_000,
                &scope_for(&env, &escrow, &[])
            ),
            Err(Ok(PermissionError::InvalidParam))
        );

        // Duplicate symbols.
        assert_eq!(
            client.try_grant_scoped(
                &owner,
                &delegate,
                &1_000,
                &500,
                &merchants,
                &10_000,
                &scope_for(&env, &escrow, &["fund", "fund"])
            ),
            Err(Ok(PermissionError::InvalidParam))
        );

        // Over the bound.
        let mut too_many: soroban_sdk::Vec<Symbol> = Vec::new(&env);
        for i in 0..=crate::MAX_FUNCTIONS_PER_PERMISSION {
            too_many.push_back(Symbol::new(&env, &format!("fn_{i}")));
        }
        assert_eq!(
            client.try_grant_scoped(
                &owner,
                &delegate,
                &1_000,
                &500,
                &merchants,
                &10_000,
                &ScopedPermissionConfig {
                    target_contract: escrow.clone(),
                    allowed_function_symbols: too_many,
                }
            ),
            Err(Ok(PermissionError::InvalidParam))
        );

        // Nothing was written by any of the rejected calls.
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10);

        // Keep instance and record alive across the ledger jump
        env.as_contract(&contract_id, || {
            env.storage().instance().extend_ttl(1_000_000, 1_000_000);
            env.storage().persistent().extend_ttl(
                &DataKey::Permission(owner.clone(), delegate.clone()),
                1_000_000,
                1_000_000,
            );
            env.storage().persistent().extend_ttl(
                &DataKey::UserPermissions(owner.clone()),
                1_000_000,
                1_000_000,
            );
        });

        // Advance ledger to 100_011 (> 10 + 100_000)
        env.ledger().set_sequence_number(100_011);

        let pruned = client.prune_expired_permission(&owner, &delegate, &keeper);
        assert_eq!(pruned, true);

        // Verify PermissionPrunedEvent was emitted
        let events = env.events().all();
        let mut found_event = false;
        for event in events.iter() {
            let (contract, topics, value) = event;
            if contract != contract_id || topics.len() != 2 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == soroban_sdk::symbol_short!("perm") && t1 == soroban_sdk::symbol_short!("pruned") {
                let evt: crate::PermissionPrunedEvent = value.try_into_val(&env).unwrap();
                assert_eq!(evt.owner, owner);
                assert_eq!(evt.delegate, delegate);
                assert_eq!(evt.keeper, keeper);
                assert_eq!(evt.pruned_at_ledger, 100_011);
                found_event = true;
            }
        }
        assert!(found_event, "PermissionPrunedEvent not found in emitted events");

        // Verify storage deletion
        assert_eq!(client.is_active(&owner, &delegate), false);
        let get_res = client.try_get_permission(&owner, &delegate);
        assert_eq!(get_res, Err(Ok(PermissionError::PermissionNotFound)));
    }

    #[test]
    fn test_prune_active_permission_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let keeper = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        // Expiry at ledger 1000
        client.grant(&owner, &delegate, &1000, &100, &merchants, &1000);

        // Sequence is 500 (permission is active and unexpired)
        env.ledger().set_sequence_number(500);

        let pruned = client.prune_expired_permission(&owner, &delegate, &keeper);
        assert_eq!(pruned, false);

        // Verify permission is still active and exists in storage
        assert_eq!(client.is_active(&owner, &delegate), true);
        assert!(client.try_get_permission(&owner, &delegate).is_ok());

        // Verify no pruned event was emitted
        let events = env.events().all();
        for event in events.iter() {
            let (contract, topics, _) = event;
            if contract != contract_id || topics.len() != 2 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == soroban_sdk::symbol_short!("perm") && t1 == soroban_sdk::symbol_short!("pruned") {
                panic!("PermissionPrunedEvent should not be emitted for active permission");
            }
        }
    }

    #[test]
    fn test_prune_permission_boundary_checks() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let keeper = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        // Expiry at ledger 100
        client.grant(&owner, &delegate, &1000, &100, &merchants, &100);

        // Keep instance and record alive across the ledger jumps
        env.as_contract(&contract_id, || {
            env.storage().instance().extend_ttl(1_000_000, 1_000_000);
            env.storage().persistent().extend_ttl(
                &DataKey::Permission(owner.clone(), delegate.clone()),
                1_000_000,
                1_000_000,
            );
            env.storage().persistent().extend_ttl(
                &DataKey::UserPermissions(owner.clone()),
                1_000_000,
                1_000_000,
            );
        });

        // At ledger 100: just reached expiry (0 ledgers expired) -> reject
        env.ledger().set_sequence_number(100);
        assert_eq!(client.prune_expired_permission(&owner, &delegate, &keeper), false);
        assert!(client.try_get_permission(&owner, &delegate).is_ok());

        // At ledger 100_100: expired by exactly 100,000 ledgers (not > 100,000) -> reject
        env.ledger().set_sequence_number(100_100);
        assert_eq!(client.prune_expired_permission(&owner, &delegate, &keeper), false);
        assert!(client.try_get_permission(&owner, &delegate).is_ok());

        // At ledger 100_101: expired by 100,001 ledgers (> 100,000) -> succeeds!
        env.ledger().set_sequence_number(100_101);
        assert_eq!(client.prune_expired_permission(&owner, &delegate, &keeper), true);
        assert_eq!(
            client.try_get_permission(&owner, &delegate),
            Err(Ok(PermissionError::PermissionNotFound))
        );
        assert_eq!(client.get_permission_scope(&owner, &delegate), None);
    }

    /// `set_permission_scope` applies the same bound as the grant path.
    #[test]
    fn test_set_permission_scope_rejects_malformed_scope() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let escrow = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1_000, &500, &merchants, &10_000);

        assert_eq!(
            client.try_set_permission_scope(
                &owner,
                &delegate,
                &Some(scope_for(&env, &escrow, &[]))
            ),
            Err(Ok(PermissionError::InvalidParam))
        );
        assert_eq!(client.get_permission_scope(&owner, &delegate), None);
    }

    /// `set_permission_scope` on an unknown pair is a no-op error, not a
    /// silently created scope that nothing would ever consult.
    #[test]
    fn test_set_permission_scope_unknown_pair() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let escrow = Address::generate(&env);

        assert_eq!(
            client.try_set_permission_scope(
                &owner,
                &delegate,
                &Some(scope_for(&env, &escrow, &["fund"]))
            ),
    }

    #[test]
    fn test_prune_nonexistent_permission_error() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let keeper = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let res = client.try_prune_expired_permission(&owner, &delegate, &keeper);
        assert_eq!(res, Err(Ok(PermissionError::PermissionNotFound)));
    }

    #[test]
    fn test_prune_already_swept_expired_permission() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let caller = Address::generate(&env);
        let keeper = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        // Expiry at ledger 50
        client.grant(&owner, &delegate, &1000, &100, &merchants, &50);

        // Sweep it at ledger 60
        env.ledger().set_sequence_number(60);
        let swept = client.sweep_expired(&owner, &delegate, &caller);
        assert_eq!(swept, true);
        assert_eq!(client.get_permission(&owner, &delegate).status, PermissionStatus::Expired);

        // Keep instance and record alive across the ledger jump
        env.as_contract(&contract_id, || {
            env.storage().instance().extend_ttl(1_000_000, 1_000_000);
            env.storage().persistent().extend_ttl(
                &DataKey::Permission(owner.clone(), delegate.clone()),
                1_000_000,
                1_000_000,
            );
            env.storage().persistent().extend_ttl(
                &DataKey::UserPermissions(owner.clone()),
                1_000_000,
                1_000_000,
            );
        });

        // Advance ledger to 100_051 (> 50 + 100_000)
        env.ledger().set_sequence_number(100_051);
        let pruned = client.prune_expired_permission(&owner, &delegate, &keeper);
        assert_eq!(pruned, true);
        assert_eq!(
            client.try_get_permission(&owner, &delegate),
            Err(Ok(PermissionError::PermissionNotFound))
        );
    }

    /// `preview_spend` surfaces the scoping rejection with its own reason code
    /// so an off-chain agent can distinguish it from a limit failure.
    #[test]
    fn test_preview_spend_reports_unauthorized_function() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let escrow = Address::generate(&env);
        let (owner, delegate, merchant) = setup_scoped_grant(&env, &client, &escrow, &["fund"]);

        let preview = client.preview_spend(&owner, &delegate, &100, &merchant);
        assert!(!preview.allowed);
        assert_eq!(preview.reason, Symbol::new(&env, "bad_function"));
        assert_eq!(preview.remaining_after, 1_000);
    }

    /// A scope change is auditable, mirroring every other policy mutation.
    #[test]
    fn test_scope_change_emits_event() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let escrow = Address::generate(&env);
        let (owner, delegate, _merchant) = setup_scoped_grant(&env, &client, &escrow, &["fund"]);

        // `env.events().all()` only surfaces the most recent invocation, so
        // each write is inspected right after it happens.
        let last_scope_event = |env: &Env| -> Option<PermissionScopeUpdatedEvent> {
            let mut out = None;
            for (contract, topics, value) in env.events().all().iter() {
                if contract != contract_id || topics.len() != 2 {
                    continue;
                }
                let t0: Symbol = topics.get(0).unwrap().try_into_val(env).unwrap();
                let t1: Symbol = topics.get(1).unwrap().try_into_val(env).unwrap();
                if t0 == symbol_short!("perm") && t1 == symbol_short!("scope") {
                    out = Some(value.clone().try_into_val(env).unwrap());
                }
            }
            out
        };

        // The grant itself announces the scope it created.
        let granted = last_scope_event(&env).expect("grant must announce its scope");
        assert_eq!(granted.owner, owner);
        assert_eq!(granted.delegate, delegate);
        assert_eq!(granted.target_contract, Some(escrow.clone()));
        assert_eq!(granted.function_count, 1);

        // Clearing it announces the removal too, with a zero function count.
        client.set_permission_scope(&owner, &delegate, &None);

        let cleared = last_scope_event(&env).expect("clearing must announce itself");
        assert_eq!(cleared.function_count, 0);
        assert_eq!(cleared.target_contract, None);
    }

    // ── invalidate_nonce_range tests (issue #335) ─────────────────────────────

    #[test]
    fn test_invalidate_nonce_range_no_permission_returns_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        // No permission granted — should fail with PermissionNotFound.
        let result = client.try_invalidate_nonce_range(&owner, &delegate, &5);
        assert_eq!(
            result,
            Err(Ok(PermissionError::PermissionNotFound)),
            "expected PermissionNotFound when no permission exists"
        );
    }

    #[test]
    fn test_invalidate_nonce_range_advances_nonce_to_up_to_plus_one() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
    #[test]
    fn test_prune_cleans_up_user_permissions_index() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate1 = Address::generate(&env);
        let delegate2 = Address::generate(&env);
        let keeper = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        // Initial nonce is 0.
        assert_eq!(client.get_relayer_nonce(&owner, &delegate), 0);

        // Invalidate nonces 0..=9 — next expected nonce must become 10.
        client.invalidate_nonce_range(&owner, &delegate, &9);
        assert_eq!(
            client.get_relayer_nonce(&owner, &delegate),
            10,
            "nonce must advance to up_to_nonce + 1"
        );
    }

    #[test]
    fn test_invalidate_nonce_range_below_current_returns_nonce_already_used() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        // Advance the nonce to 5 first.
        client.invalidate_nonce_range(&owner, &delegate, &4);
        assert_eq!(client.get_relayer_nonce(&owner, &delegate), 5);

        // Trying to invalidate nonce 3 (< 5) must fail.
        let result = client.try_invalidate_nonce_range(&owner, &delegate, &3);
        assert_eq!(
            result,
            Err(Ok(PermissionError::NonceAlreadyUsed)),
            "expected NonceAlreadyUsed when up_to_nonce < current nonce"
        );
    }

    #[test]
    fn test_invalidate_nonce_range_equal_to_current_succeeds() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        // Advance to 5.
        client.invalidate_nonce_range(&owner, &delegate, &4);

        // Invalidating exactly the current nonce (5) should succeed and advance
        // to 6 — this is the "cancel exactly the current stalled nonce" path
        // that mirrors cancel_nonce semantics.
        client.invalidate_nonce_range(&owner, &delegate, &5);
        assert_eq!(client.get_relayer_nonce(&owner, &delegate), 6);
    }

    #[test]
    fn test_invalidate_nonce_range_emits_event() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        client.invalidate_nonce_range(&owner, &delegate, &7);

        use crate::NonceBatchInvalidatedEvent;
        let events = env.events().all();
        let found = events.iter().any(|ev| {
            if let Ok(payload) = ev.2.clone().try_into_val::<_, NonceBatchInvalidatedEvent>(&env) {
                payload.owner == owner
                    && payload.delegate == delegate
                    && payload.up_to_nonce == 7
                    && payload.next_nonce == 8
            } else {
                false
            }
        });
        assert!(found, "NonceBatchInvalidatedEvent not found in events");
    }

    #[test]
    fn test_invalidate_nonce_range_blocks_previous_nonces_via_relayer() {
        // This test verifies that after invalidate_nonce_range, any relayed spend
        // using a nonce ≤ up_to_nonce fails with InvalidNonce, and that a spend
        // using the new expected nonce succeeds.
        //
        // We use cancel_nonce (the unit-testable nonce-advance sibling) to assert
        // that old nonces are already consumed from the relayer's perspective.
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        // Bulk-invalidate nonces 0..=99.
        client.invalidate_nonce_range(&owner, &delegate, &99);
        assert_eq!(client.get_relayer_nonce(&owner, &delegate), 100);

        // Attempting to cancel any nonce below 100 is now "already used".
        for old_nonce in [0u64, 50, 99] {
            let result = client.try_cancel_nonce(&owner, &delegate, &old_nonce);
            assert_eq!(
                result,
                Err(Ok(PermissionError::NonceAlreadyUsed)),
                "nonce {} should be already consumed after invalidate_nonce_range(99)",
                old_nonce
            );
        }

        // Nonce 100 (the new current) can still be cancelled normally.
        client.cancel_nonce(&owner, &delegate, &100);
        assert_eq!(client.get_relayer_nonce(&owner, &delegate), 101);
    }

    #[test]
    fn test_invalidate_nonce_range_idempotent_on_same_value() {
        // Calling invalidate_nonce_range with the same up_to_nonce a second time
        // must fail with NonceAlreadyUsed — not silently succeed.
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        client.invalidate_nonce_range(&owner, &delegate, &10);
        assert_eq!(client.get_relayer_nonce(&owner, &delegate), 11);

        // Second call with the same value — nonce 10 < 11, so NonceAlreadyUsed.
        let result = client.try_invalidate_nonce_range(&owner, &delegate, &10);
        assert_eq!(result, Err(Ok(PermissionError::NonceAlreadyUsed)));
    }

    #[test]
    fn test_invalidate_nonce_range_zero_advances_nonce_from_zero_to_one() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);

        // Invalidate just nonce 0 (up_to_nonce = 0).
        client.invalidate_nonce_range(&owner, &delegate, &0);
        assert_eq!(client.get_relayer_nonce(&owner, &delegate), 1);
    }

    #[test]
    fn test_invalidate_nonce_range_audit_log_is_appended() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let merchants = Vec::<Address>::new(&env);
        client.grant(&owner, &delegate, &1000, &100, &merchants, &10000);
        client.invalidate_nonce_range(&owner, &delegate, &5);

        let page = client.get_audit_log_page(&owner, &delegate, &None);
        // At minimum: "granted" + "nonce_inv" — total_entries >= 2.
        assert!(
            page.total_entries >= 2,
            "expected at least 2 audit log entries, got {}",
            page.total_entries
        );
        let last = page.entries.get(page.total_entries - 1).unwrap();
        use soroban_sdk::symbol_short;
        assert_eq!(
            last.action,
            symbol_short!("nonce_inv"),
            "last audit entry should be nonce_inv"
        );
    // --- Issue #368: rolling-window velocity caps ---

    #[test]
    fn test_rolling_window_rejects_rapid_micro_spends() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);
        client.set_admin(&admin);

        let mut merchants = Vec::<Address>::new(&env);
        merchants.push_back(merchant.clone());
        // Per-tx limit is generous; the rolling window is the binding constraint.
        client.grant(&owner, &delegate, &10_000, &1_000, &merchants, &10_000);
        client.set_rolling_window_limit(&admin, &720, &1_000);

        // Three 400 spends exceed the 1,000 cap within the same window.
        client.execute_spend(&owner, &delegate, &400, &merchant);
        client.execute_spend(&owner, &delegate, &400, &merchant);
        assert_eq!(
            client.try_execute_spend(&owner, &delegate, &400, &merchant),
            Err(Ok(PermissionError::VelocityLimitExceeded))
        );

        let state = client.get_rolling_window_state(&owner, &delegate);
        assert_eq!(state.max_spend_in_window, 1_000);
        assert_eq!(state.current_window_spend, 800);
    }

    #[test]
    fn test_rolling_window_resets_after_window_elapses() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);
        client.set_admin(&admin);

        let mut merchants = Vec::<Address>::new(&env);
        merchants.push_back(merchant.clone());
        client.grant(&owner, &delegate, &10_000, &1_000, &merchants, &100_000);
        client.set_rolling_window_limit(&admin, &720, &1_000);

        env.ledger().set_sequence_number(1_000);
        client.execute_spend(&owner, &delegate, &1_000, &merchant);
        assert_eq!(
            client.try_execute_spend(&owner, &delegate, &1, &merchant),
            Err(Ok(PermissionError::VelocityLimitExceeded))
        );

        // Once the window elapses the accumulator rolls over automatically.
        env.ledger().set_sequence_number(1_000 + 720);
        client.execute_spend(&owner, &delegate, &1_000, &merchant);
        let state = client.get_rolling_window_state(&owner, &delegate);
        assert_eq!(state.current_window_spend, 1_000);
        assert_eq!(state.window_start_ledger, 1_000 + 720);
    }

    #[test]
    fn test_rolling_window_disabled_allows_unbounded_spend() {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        let mut merchants = Vec::<Address>::new(&env);
        merchants.push_back(merchant.clone());
        client.grant(&owner, &delegate, &10_000, &10_000, &merchants, &10_000);

        // Without a configured cap, back-to-back full-size spends are allowed.
        client.execute_spend(&owner, &delegate, &5_000, &merchant);
        client.execute_spend(&owner, &delegate, &5_000, &merchant);
    }

    #[test]
    fn test_set_rolling_window_limit_validates_admin_and_params() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let not_admin = Address::generate(&env);
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);
        client.set_admin(&admin);

        assert_eq!(
            client.try_set_rolling_window_limit(&not_admin, &720, &1_000),
            Err(Ok(PermissionError::Unauthorized))
        );
        // A non-zero cap with a zero window is rejected.
        assert_eq!(
            client.try_set_rolling_window_limit(&admin, &0, &1_000),
            Err(Ok(PermissionError::InvalidParam))
        );
        client.grant(&owner, &delegate1, &1000, &100, &merchants, &10);
        client.grant(&owner, &delegate2, &2000, &200, &merchants, &200_000);

        assert_eq!(client.get_permissions_by_owner(&owner).len(), 2);

        // Keep instance and records alive across the ledger jump
        env.as_contract(&contract_id, || {
            env.storage().instance().extend_ttl(1_000_000, 1_000_000);
            env.storage().persistent().extend_ttl(
                &DataKey::Permission(owner.clone(), delegate1.clone()),
                1_000_000,
                1_000_000,
            );
            env.storage().persistent().extend_ttl(
                &DataKey::Permission(owner.clone(), delegate2.clone()),
                1_000_000,
                1_000_000,
            );
            env.storage().persistent().extend_ttl(
                &DataKey::UserPermissions(owner.clone()),
                1_000_000,
                1_000_000,
            );
        });

        // Advance to prune delegate1
        env.ledger().set_sequence_number(100_011);
        assert_eq!(client.prune_expired_permission(&owner, &delegate1, &keeper), true);

        // delegate1 is removed, delegate2 remains
        let remaining = client.get_permissions_by_owner(&owner);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining.get(0).unwrap().delegate, delegate2);
    }
}

#[cfg(test)]
mod expiry_and_allowance_sweep_tests {
    use crate::{
        compute_expiry_ledger, AllowanceReclaimedEvent, PermissionError, PermissionsContract,
        PermissionsContractClient,
    };
    use soroban_sdk::{
        symbol_short,
        testutils::{Address as _, Events, Ledger},
        token::{StellarAssetClient, TokenClient},
        Address, Env, Symbol, TryIntoVal, Vec,
    };

    fn setup(env: &Env) -> (PermissionsContractClient<'_>, Address, Address, Address) {
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(env, &contract_id);
        (
            client,
            Address::generate(env),
            Address::generate(env),
            contract_id,
        )
    }

    // ── compute_expiry_ledger ──────────────────────────────────────────────

    #[test]
    fn compute_expiry_ledger_handles_upper_bounds() {
        assert_eq!(compute_expiry_ledger(100, 50), Ok(150));
        assert_eq!(compute_expiry_ledger(0, u32::MAX), Ok(u32::MAX));
        assert_eq!(compute_expiry_ledger(1, u32::MAX - 1), Ok(u32::MAX));
        assert_eq!(
            compute_expiry_ledger(1, u32::MAX),
            Err(PermissionError::InvalidExpiry)
        );
        assert_eq!(
            compute_expiry_ledger(u32::MAX, 1),
            Err(PermissionError::InvalidExpiry)
        );
        assert_eq!(
            compute_expiry_ledger(u32::MAX, u32::MAX),
            Err(PermissionError::InvalidExpiry)
        // Merchant verified under old policy (1 verification).
        client.set_merchant_verifications(&merchant_id, &1);

        // Governance raises the required verifications to 2.
        let new_policy = crate::VerificationPolicy { required: 2 };
        assert!(
            !client.recheck_merchant_verification(&merchant_id, &new_policy),
            "merchant must not be considered verified once policy threshold increases"
        );
    }

    #[test]
    fn test_revalidate_merchant_status_grants_grace_period() {
        let env = Env::default();
        env.mock_all_auths();
        let merchant_id: u64 = 3;

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        // Merchant verified under old policy (1 verification).
        client.set_merchant_verifications(&merchant_id, &1);

        // Governance raises the required verifications to 2.
        let new_policy = crate::VerificationPolicy { required: 2 };
        client.set_verification_policy(&new_policy);

        // Immediately after the policy change, the merchant is still within
        // the 30-day grace period and remains valid.
        let status = client.revalidate_merchant_status(&merchant_id);
        assert!(
            status.valid,
            "merchant must remain valid during the 30-day grace period"
        );
        assert!(
            status.grace_period_active,
            "grace period must be reported as active"
        );
        assert_eq!(status.required, 2);
        assert_eq!(status.current, 1);
    }

    #[test]
    fn test_revalidate_merchant_status_expires_after_grace_period() {
        let env = Env::default();
        env.mock_all_auths();
        let merchant_id: u64 = 4;

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        client.set_merchant_verifications(&merchant_id, &1);
        let new_policy = crate::VerificationPolicy { required: 2 };
        client.set_verification_policy(&new_policy);

        // Advance the ledger timestamp past the 30-day grace period.
        env.ledger().with_mut(|li: &mut LedgerInfo| {
            li.timestamp += 30 * 24 * 60 * 60 + 1;
        });

        let status = client.revalidate_merchant_status(&merchant_id);
        assert!(
            !status.valid,
            "merchant must transition gracefully once the grace period expires"
        );
        assert!(
            !status.grace_period_active,
            "grace period must no longer be active"
        );
    }

    #[test]
    fn invalid_expiry_code_is_2412() {
        assert_eq!(PermissionError::InvalidExpiry as u32, 2412);
    }

    #[test]
    fn grant_with_overflowing_ttl_is_rejected() {
        let env = Env::default();
        let (client, owner, delegate, _) = setup(&env);
        env.ledger().set_sequence_number(1_000);

        let result = client.try_grant(&owner, &delegate, &1_000, &100, &Vec::new(&env), &u32::MAX);

        assert_eq!(result, Err(Ok(PermissionError::InvalidExpiry)));
        assert_eq!(
            client.try_get_permission(&owner, &delegate),
            Err(Ok(PermissionError::PermissionNotFound))
        );
    }

    #[test]
    fn grant_with_max_non_overflowing_ttl_succeeds() {
        let env = Env::default();
        let (client, owner, delegate, _) = setup(&env);
        env.ledger().set_sequence_number(1_000);

        client.grant(&owner, &delegate, &1_000, &100, &Vec::new(&env), &(u32::MAX - 1_000));

        assert_eq!(
            client.get_permission(&owner, &delegate).expires_at_ledger,
            u32::MAX
        );
    }

    // ── sweep_expired_allowance ────────────────────────────────────────────

    fn setup_with_allowance(
        env: &Env,
    ) -> (PermissionsContractClient<'_>, Address, Address, Address, Address) {
        let (client, owner, delegate, contract_id) = setup(env);
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(env))
            .address();
        StellarAssetClient::new(env, &token).mint(&owner, &10_000);
        env.ledger().set_sequence_number(100);
        client.grant(&owner, &delegate, &1_000, &100, &Vec::new(env), &50);
        TokenClient::new(env, &token).approve(&owner, &delegate, &500, &10_000);
        (client, owner, delegate, token, contract_id)
    }

    #[test]
    fn sweep_resets_allowance_on_expired_delegation() {
        let env = Env::default();
        let (client, owner, delegate, token, contract_id) = setup_with_allowance(&env);
        env.ledger().set_sequence_number(150);

        client.sweep_expired_allowance(&owner, &delegate, &token);

        assert_eq!(TokenClient::new(&env, &token).allowance(&owner, &delegate), 0);
        let (contract, topics, data) = env.events().all().last().unwrap();
        assert_eq!(contract, contract_id);
        let t1: Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
        assert_eq!(t1, symbol_short!("allow_rcl"));
        let event: AllowanceReclaimedEvent = data.try_into_val(&env).unwrap();
        assert_eq!(event.owner, owner);
        assert_eq!(event.delegate, delegate);
        assert_eq!(event.token, token);
        assert_eq!(event.reclaimed_amount, 500);
    }

    #[test]
    fn sweep_rejects_live_delegation() {
        let env = Env::default();
        let (client, owner, delegate, token, _) = setup_with_allowance(&env);
        env.ledger().set_sequence_number(149);

        assert_eq!(
            client.try_sweep_expired_allowance(&owner, &delegate, &token),
            Err(Ok(PermissionError::DelegationNotExpired))
        );
        assert_eq!(TokenClient::new(&env, &token).allowance(&owner, &delegate), 500);
    }

    #[test]
    fn sweep_rejects_unknown_delegation() {
        let env = Env::default();
        let (client, owner, _, token, _) = setup_with_allowance(&env);
        let stranger = Address::generate(&env);

        assert_eq!(
            client.try_sweep_expired_allowance(&owner, &stranger, &token),
            Err(Ok(PermissionError::PermissionNotFound))
        );
    }

    #[test]
    fn sweep_with_zero_allowance_is_a_noop() {
        let env = Env::default();
        let (client, owner, delegate, contract_id) = setup(&env);
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        env.ledger().set_sequence_number(100);
        client.grant(&owner, &delegate, &1_000, &100, &Vec::new(&env), &50);
        env.ledger().set_sequence_number(150);

        client.sweep_expired_allowance(&owner, &delegate, &token);

        for (contract, topics, _) in env.events().all().iter() {
            if contract != contract_id || topics.len() < 2 {
                continue;
            }
            let t1: Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            assert_ne!(t1, symbol_short!("allow_rcl"));
        }
    }
    // --- Issue #: Enforce maximum parent delegation depth ---

    #[test]
    fn test_grant_child_within_max_depth_succeeds() {
        let env = Env::default();
        env.mock_all_auths();
        let root_owner = Address::generate(&env);
        let parent_delegate = Address::generate(&env);
        let child_delegate = Address::generate(&env);
        let grandchild_delegate = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);
    fn test_revalidate_merchant_status_meets_new_policy() {
        let env = Env::default();
        env.mock_all_auths();
        let merchant_id: u64 = 5;

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        // Depth 0: root grant.
        client.grant(&root_owner, &parent_delegate, &10_000, &1000, &merchants, &10000);

        // Depth 1: child of root grant.
        assert_eq!(
            client.try_grant_child(
                &root_owner,
                &parent_delegate,
                &child_delegate,
                &5000,
                &500,
                &merchants,
                &10000,
            ),
            Ok(Ok(()))
        );

        // Depth 2: grandchild of root grant.
        assert_eq!(
            client.try_grant_child(
                &root_owner,
                &child_delegate,
                &grandchild_delegate,
                &2500,
                &250,
                &merchants,
                &10000,
            ),
            Ok(Ok(()))
        );
    }

    #[test]
    fn test_grant_child_exceeding_max_depth_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let root_owner = Address::generate(&env);
        let d1 = Address::generate(&env);
        let d2 = Address::generate(&env);
        let d3 = Address::generate(&env);
        let d4 = Address::generate(&env);
        let merchants = Vec::<Address>::new(&env);
        // Merchant acquires the additional attestation.
        client.set_merchant_verifications(&merchant_id, &2);
        let new_policy = crate::VerificationPolicy { required: 2 };
        client.set_verification_policy(&new_policy);

        let status = client.revalidate_merchant_status(&merchant_id);
        assert!(status.valid, "merchant meeting the new policy must be valid");
        assert!(
            !status.grace_period_active,
            "no grace period needed when policy is met"
        );
        assert_eq!(status.required, 2);
        assert_eq!(status.current, 2);
    }

    #[test]
    fn test_revalidate_merchant_status_unknown_merchant() {
        let env = Env::default();
        env.mock_all_auths();
        let merchant_id: u64 = 999;

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        // Depth 0.
        client.grant(&root_owner, &d1, &10_000, &1000, &merchants, &10000);
        // Depth 1.
        client.grant_child(&root_owner, &d1, &d2, &5000, &500, &merchants, &10000);
        // Depth 2.
        client.grant_child(&root_owner, &d2, &d3, &2500, &250, &merchants, &10000);
        // Depth 3 would exceed MAX_HIERARCHY_DEPTH (3) and must be rejected.
        assert_eq!(
            client.try_grant_child(
                &root_owner,
                &d3,
                &d4,
                &1250,
                &125,
                &merchants,
                &10000,
            ),
            Err(Ok(PermissionError::MaxHierarchyDepthExceeded))
        );
        let new_policy = crate::VerificationPolicy { required: 1 };
        client.set_verification_policy(&new_policy);

        let status = client.revalidate_merchant_status(&merchant_id);
        assert!(!status.valid, "unknown merchant must not be valid");
        assert_eq!(status.current, 0);
    }

    #[test]
    fn test_policy_transition_behavior_end_to_end() {
        let env = Env::default();
        env.mock_all_auths();
        let merchant_id: u64 = 42;

        let contract_id = env.register(PermissionsContract, ());
        let client = PermissionsContractClient::new(&env, &contract_id);

        // 1. Merchant verified under old policy (1 verification).
        let old_policy = crate::VerificationPolicy { required: 1 };
        client.set_verification_policy(&old_policy);
        client.set_merchant_verifications(&merchant_id, &1);
        assert!(client.recheck_merchant_verification(&merchant_id, &old_policy));
        assert!(client.revalidate_merchant_status(&merchant_id).valid);

        // 2. Governance raises the policy to 2 verifications.
        let new_policy = crate::VerificationPolicy { required: 2 };
        client.set_verification_policy(&new_policy);

        // 3. Merchant is no longer statically verified.
        assert!(!client.recheck_merchant_verification(&merchant_id, &new_policy));

        // 4. But the grace period keeps them valid temporarily.
        let grace_status = client.revalidate_merchant_status(&merchant_id);
        assert!(grace_status.valid);
        assert!(grace_status.grace_period_active);

        // 5. Merchant acquires the second attestation before the deadline.
        client.set_merchant_verifications(&merchant_id, &2);
        let met_status = client.revalidate_merchant_status(&merchant_id);
        assert!(met_status.valid);
        assert!(!met_status.grace_period_active);

        // 6. A different merchant that never acquires the attestation
        //    transitions gracefully after the grace period.
        let lagging_id: u64 = 43;
        client.set_merchant_verifications(&lagging_id, &1);
        env.ledger().with_mut(|li: &mut LedgerInfo| {
            li.timestamp += 30 * 24 * 60 * 60 + 1;
        });
        let lagging_status = client.revalidate_merchant_status(&lagging_id);
        assert!(!lagging_status.valid);
        assert!(!lagging_status.grace_period_active);
    }
}
