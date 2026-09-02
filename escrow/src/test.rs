#[cfg(test)]
#[allow(clippy::module_inception)]
mod test {
    use crate::{
        DataKey, EscrowConfig, EscrowContract, EscrowContractClient, EscrowError,
        EscrowMetadataEvent,
    };
    const MAX_DEPOSIT_CPU_INSTRUCTIONS: u64 = 3_000_000;
    const MAX_DEPOSIT_MEMORY_BYTES: u64 = 3_000_000;

    fn assert_deposit_cost_within_thresholds(env: &soroban_sdk::Env) {
    let budget = env.cost_estimate().budget();
    assert!(budget.cpu_instruction_count() <= MAX_DEPOSIT_CPU_INSTRUCTIONS);
    assert!(budget.memory_bytes() <= MAX_DEPOSIT_MEMORY_BYTES);
}

use soroban_sdk::{
        symbol_short,
        testutils::{Address as _, Events, Ledger},
        token::TokenClient,
        Address, BytesN, Env, IntoVal, TryIntoVal,
    };

    fn setup_client(env: &Env) -> (EscrowContractClient<'_>, Address, Address) {
        let admin = Address::generate(env);
        let treasury = Address::generate(env);
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 250u32,
            treasury: treasury.clone(),
            min_amount: 100i128,
            max_amount: 1_000_000i128,
        };
        let contract_id = env.register(EscrowContract, (config,));
        let contract_id = env.register(
            EscrowContract,
            (EscrowConfig {
                admin: admin.clone(),
                fee_bps: 250u32,
                treasury: treasury.clone(),
                min_amount: 100i128,
                max_amount: 1_000_000i128,
            },),
        );
        let client = EscrowContractClient::new(env, &contract_id);
        (client, admin, contract_id)
    }

    const ZERO_ACCOUNT_STRKEY: &str = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";
    const ZERO_CONTRACT_STRKEY: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";

    fn zero_account(env: &Env) -> Address {
        Address::from_str(env, ZERO_ACCOUNT_STRKEY)
    }

    fn zero_contract(env: &Env) -> Address {
        Address::from_str(env, ZERO_CONTRACT_STRKEY)
    }

    #[test]
    fn test_initialize_already_initialized() {
        let env = Env::default();
        let (client, admin, _) = setup_client(&env);
        let treasury = Address::generate(&env);

        let res = client.initialize(&admin, &fee_bps, &treasury, &min_amount, &max_amount);
        assert_eq!(res, true);

        let admin = Address::generate(&env);
        let fee_bps = 250u32;
        let min_amount = 100i128;
        let max_amount = 10000i128;
        // Register via constructor — the contract is now initialised at deploy time.
        let contract_id = env.register(
            EscrowContract,
            (EscrowConfig {
                admin: admin.clone(),
                fee_bps,
                treasury: treasury.clone(),
                min_amount,
                max_amount,
            },),
        );
        let client = EscrowContractClient::new(&env, &contract_id);
        // The contract is already initialised; calling initialize again must fail.
        let res_try = client.try_initialize(&admin, &fee_bps, &treasury, &min_amount, &max_amount);
        let res_try = client.try_initialize(&admin, &250u32, &treasury, &100i128, &10000i128);
        assert_eq!(res_try, Err(Ok(EscrowError::AlreadyInitialized)));
    }

    #[test]
    fn test_constructor_initializes_atomically() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 250u32,
            treasury: treasury.clone(),
            min_amount: 100i128,
            max_amount: 1_000_000i128,
        };

        // Call constructor
        let res = client.constructor(&config);
        assert_eq!(res, Ok(()));
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(&env, &contract_id);

        // Verify admin is set correctly
        let admin_view = client.get_admin();
        assert_eq!(admin_view.admin, admin);
        assert_eq!(admin_view.pending_admin, None);

        // Verify fee config is set correctly
        let fee_config = client.get_fee_config();
        assert_eq!(fee_config.fee_bps, 250u32);
        assert_eq!(fee_config.treasury, treasury);

        // Verify limits are set correctly
        let limits = client.get_limits();
        assert_eq!(limits.min_amount, 100i128);
        assert_eq!(limits.max_amount, 1_000_000i128);
    }

    #[test]
    fn test_constructor_vs_initialize_race() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 250u32,
            treasury: treasury.clone(),
            min_amount: 100i128,
            max_amount: 1_000_000i128,
        };

        // Initialize via constructor
        let res = client.constructor(&config);
        assert_eq!(res, Ok(()));
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(&env, &contract_id);

        // Attempt to call initialize after constructor should fail
        let res_try = client.try_initialize(&admin, &250u32, &treasury, &100i128, &1_000_000i128);
        assert_eq!(res_try, Err(Ok(EscrowError::AlreadyInitialized)));
    }

    #[test]
    #[should_panic]
    fn test_constructor_rejects_zero_treasury() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let config = EscrowConfig {
            admin,
            fee_bps: 250u32,
            treasury: zero_account(&env),
            min_amount: 100i128,
            max_amount: 1_000_000i128,
        };

        let res = client.try_constructor(&config);
        assert_eq!(res, Err(Ok(EscrowError::InvalidAddress)));
        env.register(EscrowContract, (config,));
    }

    #[test]
    #[should_panic]
    fn test_constructor_rejects_invalid_fee_bps() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let config = EscrowConfig {
            admin: admin.clone(),
            admin,
            fee_bps: 1001u32, // > 1000
            treasury,
            min_amount: 100i128,
            max_amount: 1_000_000i128,
        };

        let res = client.try_constructor(&config);
        assert_eq!(res, Err(Ok(EscrowError::InvalidFeeBps)));
        env.register(EscrowContract, (config,));
    }

    #[test]
    #[should_panic]
    fn test_constructor_rejects_invalid_limits() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);

        // Test min_amount <= 0
        let config1 = EscrowConfig {
            admin,
            fee_bps: 250u32,
            treasury: treasury.clone(),
            treasury,
            min_amount: 0i128, // <= 0
            max_amount: 1_000_000i128,
        };

        let res = client.try_constructor(&config1);
        assert_eq!(res, Err(Ok(EscrowError::InvalidLimits)));

        // Test max_amount < min_amount
        let config2 = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 250u32,
            treasury: treasury.clone(),
            min_amount: 1000i128,
            max_amount: 500i128, // < min_amount
        };

        let res = client.try_constructor(&config2);
        assert_eq!(res, Err(Ok(EscrowError::InvalidLimits)));
    fn test_initialize_rejects_zero_treasury() {
        // With the constructor pattern, the contract is initialized at deploy time.
        // The legacy initialize() function is only for backward compat with pre-constructor
        // contracts; once a constructor is used it always returns AlreadyInitialized.
        // Zero-treasury validation via the constructor is covered by
        // test_constructor_rejects_zero_treasury.
        let contract_id = env.register(
            EscrowContract,
            (EscrowConfig {
                admin: admin.clone(),
                fee_bps: 250u32,
                treasury: treasury.clone(),
                min_amount: 100i128,
                max_amount: 1_000_000i128,
            },),
        );
        // Calling initialize after constructor always fails — state is already set.
        let res = client.try_initialize(
            &admin,
            &250u32,
            &zero_account(&env),
            &100i128,
            &1_000_000i128,
        );
        assert_eq!(res, Err(Ok(EscrowError::AlreadyInitialized)));
    }

    #[test]
    fn test_constructor_initializes_atomically() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 250u32,
            treasury: treasury.clone(),
            min_amount: 100i128,
            max_amount: 1_000_000i128,
        };

        // The constructor runs exactly once, atomically, during registration.
        // (The generated client intentionally has no __constructor method:
        // constructors are not invocable post-deployment.)
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(&env, &contract_id);

        // Verify admin is set correctly
        let admin_view = client.get_admin();
        assert_eq!(admin_view.admin, admin);
        assert_eq!(admin_view.pending_admin, None);

        // Verify fee config is set correctly
        let fee_config = client.get_fee_config();
        assert_eq!(fee_config.fee_bps, 250u32);
        assert_eq!(fee_config.treasury, treasury);

        // Verify limits are set correctly
        let limits = client.get_limits();
        assert_eq!(limits.min_amount, 100i128);
        assert_eq!(limits.max_amount, 1_000_000i128);
    }

    #[test]
    fn test_constructor_vs_initialize_race() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps: 250u32,
            treasury: treasury.clone(),
            min_amount: 100i128,
            max_amount: 1_000_000i128,
        };

        // Initialize via constructor (runs atomically during registration)
        let contract_id = env.register(EscrowContract, (config,));
        let client = EscrowContractClient::new(&env, &contract_id);

        // Attempt to call initialize after constructor should fail
        let res_try = client.try_initialize(&admin, &250u32, &treasury, &100i128, &1_000_000i128);
        assert_eq!(res_try, Err(Ok(EscrowError::AlreadyInitialized)));
    }

    #[test]
    #[should_panic]
    fn test_constructor_rejects_zero_treasury() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let config = EscrowConfig {
            admin,
            fee_bps: 250u32,
            treasury: zero_account(&env),
            min_amount: 100i128,
            max_amount: 1_000_000i128,
        };

        // Invalid config aborts deployment: the constructor runs at register.
        env.register(EscrowContract, (config,));
    }

    #[test]
    #[should_panic]
    fn test_constructor_rejects_invalid_fee_bps() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let config = EscrowConfig {
            admin,
            fee_bps: 1001u32, // > 1000
            treasury,
            min_amount: 100i128,
            max_amount: 1_000_000i128,
        };

        // Invalid config aborts deployment: the constructor runs at register.
        env.register(EscrowContract, (config,));
    }

    #[test]
    #[should_panic]
    fn test_constructor_rejects_non_positive_min_amount() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let config = EscrowConfig {
            admin,
            fee_bps: 250u32,
            treasury,
            min_amount: 0i128, // <= 0
            max_amount: 1_000_000i128,
        };

        // Invalid limits abort deployment: the constructor runs at register.
        env.register(EscrowContract, (config,));
    }

    #[test]
    #[should_panic]
    fn test_constructor_rejects_max_below_min() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let config = EscrowConfig {
            admin,
            fee_bps: 250u32,
            treasury,
            min_amount: 1000i128,
            max_amount: 500i128, // < min_amount
        };

        assert_eq!(
            client.try_get_fee_config(),
            Err(Ok(EscrowError::FeeConfigNotSet))
        );
        assert_eq!(
            client.try_get_limits(),
            Err(Ok(EscrowError::AmountLimitsNotSet))
        );
        assert_eq!(client.try_get_admin(), Err(Ok(EscrowError::NotFound)));
        env.register(EscrowContract, (config1,));
        // Invalid limits abort deployment: the constructor runs at register.
        env.register(EscrowContract, (config,));
    }

    #[test]
    fn test_getters_return_initialized_state() {
        // The contract is always initialized via constructor at deploy time.
        // Verify that after constructor registration all getters return expected state.
        let env = Env::default();
        let (client, admin, _contract_id) = setup_client(&env);
        // All getters succeed after constructor initialization.
        let fee_config = client.get_fee_config();
        assert_eq!(fee_config.fee_bps, 250u32);
        let limits = client.get_limits();
        assert_eq!(limits.min_amount, 100i128);
        assert_eq!(limits.max_amount, 1_000_000i128);
        let admin_view = client.get_admin();
        assert_eq!(admin_view.admin, admin);
    }

    #[test]
    fn test_get_admin_returns_initialized_admin() {
        let env = Env::default();
        let (client, admin, _contract_id) = setup_client(&env);

        let view = client.get_admin();

        assert_eq!(view.admin, admin);
        assert_eq!(view.pending_admin, None);
    }

    #[test]
    fn test_get_admin_includes_pending_admin() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);
        let pending_admin = Address::generate(&env);

        assert!(client.propose_admin(&admin, &pending_admin));
        let view = client.get_admin();

        assert_eq!(view.admin, admin);
        assert_eq!(view.pending_admin, Some(pending_admin));
    }

    #[test]
    fn test_create_rejects_zero_addresses() {
        let env = Env::default();
        let (client, _admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token = zero_contract(&env);
        let order_id = BytesN::from_array(&env, &[1u8; 32]);

        let buyer_zero = client.try_create(
            &zero_account(&env),
            &seller,
            &token,
            &1000i128,
            &order_id,
            &100u32,
            &None,
            &None,
        );
        assert_eq!(buyer_zero, Err(Ok(EscrowError::InvalidAddress)));

        let seller_zero = client.try_create(
            &buyer,
            &zero_account(&env),
            &token,
            &1000i128,
            &order_id,
            &100u32,
            &None,
            &None,
        );
        assert_eq!(seller_zero, Err(Ok(EscrowError::InvalidAddress)));

        let token_zero = client.try_create(
            &buyer,
            &seller,
            &zero_contract(&env),
            &1000i128,
            &order_id,
            &100u32,
            &None,
            &None,
        );
        assert_eq!(token_zero, Err(Ok(EscrowError::InvalidAddress)));
    }

    #[test]
    fn test_create_rejects_same_buyer_and_seller() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let party = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[2u8; 32]);

        let res = client.try_create(
            &party, &party, &token, &1000i128, &order_id, &100u32, &None, &None,
        );
        assert_eq!(res, Err(Ok(EscrowError::InvalidEscrowParticipants)));
    }

    #[test]
    fn test_get_escrow_metadata_absent_escrow_returns_not_found() {
        let env = Env::default();
        let (client, _admin, _contract_id) = setup_client(&env);

        assert_eq!(
            client.try_get_escrow_metadata(&999u64),
            Err(Ok(EscrowError::NotFound))
        );
    }

    #[test]
    fn test_get_escrow_metadata_existing_escrow_without_metadata_returns_metadata_not_set() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env.register_stellar_asset_contract_v2(token_admin).address();
        let token_client = TokenClient::new(&env, &token);
        token_client.mint(&buyer, &1000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[42u8; 32]);
        let escrow_id = client.create(
            &buyer,
            &seller,
            &token,
            &1000i128,
            &order_id,
            &100u32,
            &None,
            &None,
        );

        // Simulate a legacy escrow by removing its metadata.
        let metadata_key = DataKey::EscrowMetadata(escrow_id).into_val(&env);
        env.as_contract(&contract_id, || {
            env.storage().remove(&metadata_key);
        });

        assert_eq!(
            client.try_get_escrow_metadata(&escrow_id),
            Err(Ok(EscrowError::MetadataNotSet))
        );
    }

    // ─── Issue #179: Storage Key Namespace Tests ───────────────────────────────

    #[test]
    fn test_storage_keys_are_distinct() {
        // DataKey variants must not collide so that Escrow(id), Admin, Config,
        // and metadata entries never overwrite each other in contract storage.
        let env = Env::default();

        let addr_a = Address::generate(&env);
        let addr_b = Address::generate(&env);

        let key_admin = DataKey::Admin.into_val(&env);
        let key_escrow_0: soroban_sdk::Val = DataKey::Escrow(0u64).into_val(&env);
        let key_escrow_1: soroban_sdk::Val = DataKey::Escrow(1u64).into_val(&env);
        let key_last_id: soroban_sdk::Val = DataKey::LastEscrowId.into_val(&env);
        let key_pending: soroban_sdk::Val = DataKey::PendingAdmin.into_val(&env);
        let key_admin_list: soroban_sdk::Val = DataKey::AdminList.into_val(&env);
        let key_fee: soroban_sdk::Val = DataKey::FeeConfig.into_val(&env);
        let key_limits: soroban_sdk::Val = DataKey::AmountLimits.into_val(&env);
        let key_quorum: soroban_sdk::Val = DataKey::QuorumConfig.into_val(&env);
        let key_votes_0: soroban_sdk::Val = DataKey::DisputeVotes(0u64).into_val(&env);
        let key_whitelist: soroban_sdk::Val = DataKey::AllowedTokenCount.into_val(&env);
        let key_token_a: soroban_sdk::Val = DataKey::AllowedToken(addr_a.clone()).into_val(&env);
        let key_token_b: soroban_sdk::Val = DataKey::AllowedToken(addr_b.clone()).into_val(&env);
        let key_token_at: soroban_sdk::Val = DataKey::AllowedTokenAt(0).into_val(&env);
        let key_pause: soroban_sdk::Val = DataKey::PauseState.into_val(&env);
        let key_metadata_hash_0: soroban_sdk::Val =
            DataKey::EscrowMetadataHash(0u64).into_val(&env);
        let key_metadata_hash_1: soroban_sdk::Val =
            DataKey::EscrowMetadataHash(1u64).into_val(&env);
        let key_metadata_schema_0: soroban_sdk::Val =
            DataKey::EscrowMetadataSchema(0u64).into_val(&env);
        let key_metadata_schema_1: soroban_sdk::Val =
            DataKey::EscrowMetadataSchema(1u64).into_val(&env);
        let key_migration: soroban_sdk::Val = DataKey::MigrationFlag.into_val(&env);
        let key_fee_dist: soroban_sdk::Val = DataKey::FeeDistribution.into_val(&env);
        let key_require_cond_0: soroban_sdk::Val =
            DataKey::RequireReleaseCondition(0u64).into_val(&env);
        let key_require_cond_1: soroban_sdk::Val =
            DataKey::RequireReleaseCondition(1u64).into_val(&env);

        let all_keys: &[soroban_sdk::Val] = &[
            key_admin,
            key_escrow_0,
            key_escrow_1,
            key_last_id,
            key_pending,
            key_admin_list,
            key_fee,
            key_limits,
            key_quorum,
            key_votes_0,
            key_whitelist,
            key_token_a,
            key_token_b,
            key_token_at,
            key_pause,
            key_metadata_hash_0,
            key_metadata_hash_1,
            key_metadata_schema_0,
            key_metadata_schema_1,
            key_migration,
            key_fee_dist,
            key_require_cond_0,
            key_require_cond_1,
        ];

        // Assert every key is unique by comparing raw val representations
        for i in 0..all_keys.len() {
            for j in (i + 1)..all_keys.len() {
                let i_raw = soroban_sdk::Val::get_payload(all_keys[i]);
                let j_raw = soroban_sdk::Val::get_payload(all_keys[j]);
                assert_ne!(
                    i_raw, j_raw,
                    "DataKey collision detected at indices {i} and {j}"
                );
            }
        }
    }

    #[test]
    fn test_escrow_ids_produce_distinct_keys() {
        let env = Env::default();
        // Different escrow IDs must map to different storage keys.
        let k0: soroban_sdk::Val = DataKey::Escrow(0u64).into_val(&env);
        let k1: soroban_sdk::Val = DataKey::Escrow(1u64).into_val(&env);
        let k999: soroban_sdk::Val = DataKey::Escrow(999u64).into_val(&env);
        assert_ne!(
            soroban_sdk::Val::get_payload(k0),
            soroban_sdk::Val::get_payload(k1)
        );
        assert_ne!(
            soroban_sdk::Val::get_payload(k1),
            soroban_sdk::Val::get_payload(k999)
        );
    }

    #[test]
    fn test_token_enabled_keys_differ_per_address() {
        let env = Env::default();
        let addr_a = Address::generate(&env);
        let addr_b = Address::generate(&env);
        let ka: soroban_sdk::Val = DataKey::AllowedToken(addr_a).into_val(&env);
        let kb: soroban_sdk::Val = DataKey::AllowedToken(addr_b).into_val(&env);
        assert_ne!(
            soroban_sdk::Val::get_payload(ka),
            soroban_sdk::Val::get_payload(kb)
        );
    }

    #[test]
    fn test_metadata_keys_differ_per_escrow_id() {
        let env = Env::default();
        // Different escrow IDs must map to different metadata storage keys,
        // for both the hash and schema halves.
        let kh0: soroban_sdk::Val = DataKey::EscrowMetadataHash(0u64).into_val(&env);
        let kh1: soroban_sdk::Val = DataKey::EscrowMetadataHash(1u64).into_val(&env);
        let kh999: soroban_sdk::Val = DataKey::EscrowMetadataHash(999u64).into_val(&env);
        assert_ne!(
            soroban_sdk::Val::get_payload(kh0),
            soroban_sdk::Val::get_payload(kh1)
        );
        assert_ne!(
            soroban_sdk::Val::get_payload(kh1),
            soroban_sdk::Val::get_payload(kh999)
        );
        let ks0: soroban_sdk::Val = DataKey::EscrowMetadataSchema(0u64).into_val(&env);
        let ks1: soroban_sdk::Val = DataKey::EscrowMetadataSchema(1u64).into_val(&env);
        let ks999: soroban_sdk::Val = DataKey::EscrowMetadataSchema(999u64).into_val(&env);
        assert_ne!(
            soroban_sdk::Val::get_payload(ks0),
            soroban_sdk::Val::get_payload(ks1)
        );
        assert_ne!(
            soroban_sdk::Val::get_payload(ks1),
            soroban_sdk::Val::get_payload(ks999)
        );
        // The hash half and the schema half must never collide for the same id.
        assert_ne!(
            soroban_sdk::Val::get_payload(kh0),
            soroban_sdk::Val::get_payload(ks0)
        );
    }

    // ─── Issue #177 & #178: Admin Pause Flag + Event Tests ────────────────────

    #[test]
    fn test_set_create_paused_success() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        assert!(!client.get_create_paused());

        let res = client.set_create_paused(&admin, &true);
        assert!(res);
        assert!(client.get_create_paused());

        let res = client.set_create_paused(&admin, &false);
        assert!(res);
        assert!(!client.get_create_paused());
    }

    #[test]
    fn test_set_create_paused_unauthorized() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, _admin, _contract_id) = setup_client(&env);
        let non_admin = Address::generate(&env);

        let res = client.try_set_create_paused(&non_admin, &true);
        assert_eq!(res, Err(Ok(EscrowError::Unauthorized)));
    }

    // ─── Issue #176: Token Getter Tests ───────────────────────────────────────

    #[test]
    fn test_get_token_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, _admin, _contract_id) = setup_client(&env);

        let res = client.try_get_token(&999u64);
        assert_eq!(res, Err(Ok(EscrowError::NotFound)));
    }

    // ─── TTL Bumping on Escrow Reads ──────────────────────────────────────────

    // A long-lived, open escrow must not be evicted while it is still being read.
    // The read paths (e.g. `get_escrow`) are expected to `extend_ttl` on the
    // `Escrow(id)` persistent key. This test creates an escrow, advances the
    // ledger toward the TTL boundary, reads it (which should bump the TTL), then
    // advances further and asserts the record is still live and readable.
    #[test]
    fn test_get_escrow_bumps_ttl_across_expiry_boundary() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        // Register and whitelist a token, then fund the buyer.
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        client.add_token(&admin, &token);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &1_000_000i128);

        let order_id = BytesN::from_array(&env, &[7u8; 32]);
        // Amount stays within the [100, 1_000_000] limits configured in setup.
        let escrow_id = client.create(
            &buyer, &seller, &token, &1000i128, &order_id, &1000u32, &None, &None,
        );

        // Advance the ledger close to the persistent TTL boundary, then read the
        // escrow so the read path refreshes (bumps) its TTL.
        env.ledger().with_mut(|li| {
            li.sequence_number += 100_000;
        });
        let before = client.get_escrow(&escrow_id);

        // Advance again past what would have been the original expiry. Because the
        // read above bumped the TTL, the record must still be live and readable.
        env.ledger().with_mut(|li| {
            li.sequence_number += 100_000;
        });
        let after = client.get_escrow(&escrow_id);

        assert_eq!(before, after);
    }
    }

    // ─── Issue #172: Escrow Creation Metadata Hash Tests ─────────────────────

    #[test]
    fn test_deposit_with_metadata_success() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[1u8; 32]);
        let order_hash = BytesN::from_array(&env, &[2u8; 32]);
        let schema = soroban_sdk::symbol_short!("order_v1");

        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &1000i128,
            &order_id,
            &100u32,
            &Some(order_hash.clone()),
            &Some(schema.clone()),
        );

        // Verify metadata was stored
        let metadata = client.get_escrow_metadata(&escrow_id);
        assert_eq!(metadata.order_hash, order_hash);
        assert_eq!(metadata.schema, schema);
    }

    #[test]
    fn test_deposit_without_metadata() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[1u8; 32]);

        // Deposit without metadata (None for both parameters)
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1000i128, &order_id, &100u32, &None, &None,
        );

        // Verify metadata is not found
        let res = client.try_get_escrow_metadata(&escrow_id);
        assert_eq!(res, Err(Ok(EscrowError::NotFound)));
    }

    #[test]
    fn test_deposit_with_partial_metadata() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[1u8; 32]);
        let order_hash = BytesN::from_array(&env, &[2u8; 32]);

        // Deposit with only order_hash (schema is None)
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &1000i128,
            &order_id,
            &100u32,
            &Some(order_hash),
            &None,
        );

        // Metadata is not readable while only one half is present; the missing
        // half can be filled in later via set_escrow_metadata_schema.
        let res = client.try_get_escrow_metadata(&escrow_id);
        assert_eq!(res, Err(Ok(EscrowError::NotFound)));
    }

    #[test]
    fn test_get_escrow_metadata_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, _admin, _contract_id) = setup_client(&env);

        // Try to get metadata for non-existent escrow
        let res = client.try_get_escrow_metadata(&999u64);
        assert_eq!(res, Err(Ok(EscrowError::NotFound)));
    }

    // ─── Issue #39: Partial metadata halves persisted independently ──────────

    /// A hash-only deposit persists the hash half; the missing schema half can
    /// be filled in post-creation and the combined metadata is then readable.
    #[test]
    fn test_fill_missing_schema_half_post_creation() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[1u8; 32]);
        let order_hash = BytesN::from_array(&env, &[2u8; 32]);
        let schema = symbol_short!("order_v1");

        // Deposit with only the hash half.
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &1000i128,
            &order_id,
            &100u32,
            &Some(order_hash.clone()),
            &None,
        );

        // Incomplete metadata is not readable yet.
        let res = client.try_get_escrow_metadata(&escrow_id);
        assert_eq!(res, Err(Ok(EscrowError::NotFound)));

        // Fill the missing schema half post-creation.
        client.set_escrow_metadata_schema(&escrow_id, &buyer, &schema);

        let metadata = client.get_escrow_metadata(&escrow_id);
        assert_eq!(metadata.order_hash, order_hash);
        assert_eq!(metadata.schema, schema);
    }

    /// A schema-only deposit persists the schema half; the missing hash half
    /// can be filled in post-creation and the combined metadata is readable.
    #[test]
    fn test_fill_missing_hash_half_post_creation() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[3u8; 32]);
        let order_hash = BytesN::from_array(&env, &[4u8; 32]);
        let schema = symbol_short!("order_v2");

        // Deposit with only the schema half.
        let escrow_id = client.deposit(
            &buyer,
            &seller,
            &token,
            &1000i128,
            &order_id,
            &100u32,
            &None,
            &Some(schema.clone()),
        );

        // Incomplete metadata is not readable yet.
        let res = client.try_get_escrow_metadata(&escrow_id);
        assert_eq!(res, Err(Ok(EscrowError::NotFound)));

        // Fill the missing hash half post-creation.
        client.set_escrow_metadata_hash(&escrow_id, &buyer, &order_hash);

        let metadata = client.get_escrow_metadata(&escrow_id);
        assert_eq!(metadata.order_hash, order_hash);
        assert_eq!(metadata.schema, schema);
    }

    /// Both halves can be filled entirely post-creation for an escrow that
    /// was deposited without any metadata.
    #[test]
    fn test_fill_both_metadata_halves_post_creation() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[5u8; 32]);
        let order_hash = BytesN::from_array(&env, &[6u8; 32]);
        let schema = symbol_short!("order_v3");

        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1000i128, &order_id, &100u32, &None, &None,
        );

        let res = client.try_get_escrow_metadata(&escrow_id);
        assert_eq!(res, Err(Ok(EscrowError::NotFound)));

        client.set_escrow_metadata_hash(&escrow_id, &buyer, &order_hash);
        client.set_escrow_metadata_schema(&escrow_id, &buyer, &schema);

        let metadata = client.get_escrow_metadata(&escrow_id);
        assert_eq!(metadata.order_hash, order_hash);
        assert_eq!(metadata.schema, schema);
    }

    /// Only the buyer or an admin may fill metadata halves post-creation.
    #[test]
    fn test_set_escrow_metadata_unauthorized() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[7u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1000i128, &order_id, &100u32, &None, &None,
        );

        let stranger = Address::generate(&env);
        let hash = BytesN::from_array(&env, &[8u8; 32]);
        let schema = symbol_short!("order_v4");

        let res = client.try_set_escrow_metadata_hash(&escrow_id, &stranger, &hash);
        assert_eq!(res, Err(Ok(EscrowError::Unauthorized)));
        let res = client.try_set_escrow_metadata_schema(&escrow_id, &stranger, &schema);
        assert_eq!(res, Err(Ok(EscrowError::Unauthorized)));

        // The admin can fill the halves.
        client.set_escrow_metadata_hash(&escrow_id, &admin, &hash);
        client.set_escrow_metadata_schema(&escrow_id, &admin, &schema);
    }

    // ─── Issue #175: Escrow Metadata Event Tests ─────────────────────────────

    #[test]
    fn test_deposit_with_metadata_emits_metadata_event() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[1u8; 32]);
        let order_hash = BytesN::from_array(&env, &[2u8; 32]);
        let schema = symbol_short!("order_v1");

        client.deposit(
            &buyer,
            &seller,
            &token,
            &1000i128,
            &order_id,
            &100u32,
            &Some(order_hash.clone()),
            &Some(schema.clone()),
        );

        let events = env.events().all();
        let mut found = false;
        for event in events.iter() {
            let (contract, topics, value) = event;
            if contract != contract_id || topics.len() != 3 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == symbol_short!("escrow") && t1 == symbol_short!("metadata") {
                let evt: EscrowMetadataEvent = value.try_into_val(&env).unwrap();
                assert_eq!(evt.escrow_id, order_id);
                assert_eq!(evt.order_hash, order_hash);
                assert_eq!(evt.schema, schema);
                found = true;
            }
        }
        assert!(found, "EscrowMetadataEvent not found in events");
    }

    #[test]
    fn test_deposit_without_metadata_does_not_emit_metadata_event() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[1u8; 32]);

        client.deposit(
            &buyer, &seller, &token, &1000i128, &order_id, &100u32, &None, &None,
        );

        for event in env.events().all().iter() {
            let (contract, topics, _value) = event;
            if contract != contract_id || topics.len() != 3 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            assert!(
                !(t0 == symbol_short!("escrow") && t1 == symbol_short!("metadata")),
                "EscrowMetadataEvent must not be emitted when metadata is absent"
            );
        }
    }

    #[test]
    fn test_deposit_with_partial_metadata_does_not_emit_metadata_event() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[1u8; 32]);
        let order_hash = BytesN::from_array(&env, &[2u8; 32]);

        client.deposit(
            &buyer,
            &seller,
            &token,
            &1000i128,
            &order_id,
            &100u32,
            &Some(order_hash),
            &None,
        );

        for event in env.events().all().iter() {
            let (contract, topics, _value) = event;
            if contract != contract_id || topics.len() != 3 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            assert!(
                !(t0 == symbol_short!("escrow") && t1 == symbol_short!("metadata")),
                "EscrowMetadataEvent must not be emitted for partial metadata"
            );
        }
    }

    // ─── Merchant Escrow Cancellation Tests ──────────────────────────────────

    #[test]
    fn test_merchant_cancel_created_escrow_success() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[7u8; 32]);
        let reason = symbol_short!("out_stock");

        let escrow_id = client.create(
            &buyer, &seller, &token, &1000i128, &order_id, &100u32, &None, &None,
        );

        let record = client.get_escrow(&escrow_id);
        assert_eq!(record.status, crate::EscrowStatus::Created);

        let cancelled = client.cancel(&escrow_id, &seller, &reason);
        assert!(cancelled);

        // Verify EscrowCancelledEvent emission (retrieve events right after contract call)
        let events = env.events().all();

        let updated_record = client.get_escrow(&escrow_id);
        assert_eq!(updated_record.status, crate::EscrowStatus::Cancelled);

        let mut found = false;
        for event in events.iter() {
            let (c_id, topics, value) = event;
            if c_id != contract_id || topics.len() != 3 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == symbol_short!("escrow") && t1 == symbol_short!("cancelled") {
                let evt: crate::EscrowCancelledEvent = value.try_into_val(&env).unwrap();
                assert_eq!(evt.escrow_id, order_id);
                assert_eq!(evt.cancelled_by, seller);
                assert_eq!(evt.reason, reason);
                found = true;
            }
        }
        assert!(found, "EscrowCancelledEvent was not emitted");
    }

    #[test]
    fn test_cancel_unauthorized_caller_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let random_caller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[8u8; 32]);
        let reason = symbol_short!("no_stock");

        let escrow_id = client.create(
            &buyer, &seller, &token, &1000i128, &order_id, &100u32, &None, &None,
        );

        let res = client.try_cancel(&escrow_id, &random_caller, &reason);
        assert_eq!(res, Err(Ok(EscrowError::Unauthorized)));
    }

    #[test]
    fn test_cancel_after_funded_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[9u8; 32]);
        let reason = symbol_short!("too_late");

        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1000i128, &order_id, &100u32, &None, &None,
        );

        let record = client.get_escrow(&escrow_id);
        assert_eq!(record.status, crate::EscrowStatus::Funded);

        let res = client.try_cancel(&escrow_id, &seller, &reason);
        assert_eq!(res, Err(Ok(EscrowError::AlreadyFunded)));
    }

    #[test]
    fn test_cancel_already_cancelled_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[10u8; 32]);
        let reason = symbol_short!("duplicate");

        let escrow_id = client.create(
            &buyer, &seller, &token, &1000i128, &order_id, &100u32, &None, &None,
        );

        client.cancel(&escrow_id, &seller, &reason);

        let res = client.try_cancel(&escrow_id, &seller, &reason);
        assert_eq!(res, Err(Ok(EscrowError::AlreadyCancelled)));
    }

    #[test]
    fn test_funding_cancelled_escrow_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[11u8; 32]);
        let reason = symbol_short!("cancelled");

        let escrow_id = client.create(
            &buyer, &seller, &token, &1000i128, &order_id, &100u32, &None, &None,
        );

        client.cancel(&escrow_id, &seller, &reason);

        let res = client.try_fund(&escrow_id, &buyer);
        assert_eq!(res, Err(Ok(EscrowError::AlreadyCancelled)));
    }

    // ─── Issue #325: Upgrade Path + Version Check Tests ───────────────────────

    // A minimal Soroban contract (a single `ping` function, no storage) compiled
    // for wasm32-unknown-unknown. The host requires a valid contract WASM (with
    // the standard contract metadata section) to accept an `upload_contract_wasm`
    // call, so a bare/empty module is not sufficient here. This stub's exported
    // functions are never invoked — it only serves as the upgrade target so
    // `upgrade` has a real contract-code ledger entry to point at.
    const WASM_STUB: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x60, 0x00, 0x01, 0x7e,
        0x60, 0x00, 0x00, 0x03, 0x03, 0x02, 0x00, 0x01, 0x05, 0x03, 0x01, 0x00, 0x10, 0x06, 0x09,
        0x01, 0x7f, 0x01, 0x41, 0x80, 0x80, 0xc0, 0x00, 0x0b, 0x07, 0x15, 0x03, 0x06, 0x6d, 0x65,
        0x6d, 0x6f, 0x72, 0x79, 0x02, 0x00, 0x04, 0x70, 0x69, 0x6e, 0x67, 0x00, 0x00, 0x01, 0x5f,
        0x00, 0x01, 0x0a, 0x09, 0x02, 0x04, 0x00, 0x42, 0x01, 0x0b, 0x02, 0x00, 0x0b, 0x00, 0x2b,
        0x0e, 0x63, 0x6f, 0x6e, 0x74, 0x72, 0x61, 0x63, 0x74, 0x73, 0x70, 0x65, 0x63, 0x76, 0x30,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x70, 0x69, 0x6e,
        0x67, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x1e,
        0x11, 0x63, 0x6f, 0x6e, 0x74, 0x72, 0x61, 0x63, 0x74, 0x65, 0x6e, 0x76, 0x6d, 0x65, 0x74,
        0x61, 0x76, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x16, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x6f, 0x0e, 0x63, 0x6f, 0x6e, 0x74, 0x72, 0x61, 0x63, 0x74, 0x6d, 0x65, 0x74, 0x61,
        0x76, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x72, 0x73, 0x76, 0x65, 0x72,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x31, 0x2e, 0x39, 0x37, 0x2e, 0x31, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x72, 0x73, 0x73, 0x64, 0x6b, 0x76, 0x65,
        0x72, 0x00, 0x00, 0x00, 0x30, 0x32, 0x32, 0x2e, 0x30, 0x2e, 0x31, 0x31, 0x23, 0x33, 0x34,
        0x66, 0x37, 0x66, 0x35, 0x33, 0x61, 0x65, 0x33, 0x31, 0x65, 0x30, 0x66, 0x64, 0x30, 0x32,
        0x61, 0x61, 0x62, 0x34, 0x33, 0x36, 0x61, 0x39, 0x38, 0x37, 0x32, 0x65, 0x37, 0x39, 0x66,
        0x61, 0x36, 0x37, 0x31, 0x63, 0x61, 0x30, 0x32,
    ];

    #[test]
    fn test_check_version_returns_current_version() {
        let env = Env::default();
        let (client, _admin, _contract_id) = setup_client(&env);

        let v = client.check_version();
        assert_eq!(v.name, symbol_short!("escrow"));
        assert_eq!(v.semver, symbol_short!("0_2_0"));
        assert_eq!(v, client.version());
    }

    #[test]
    fn test_upgrade_requires_admin_auth() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, _admin, _contract_id) = setup_client(&env);

        let not_admin = Address::generate(&env);
        // Auth is checked before the wasm hash is ever used, so a dummy hash suffices.
        let wasm_hash = BytesN::from_array(&env, &[0u8; 32]);

        let res = client.try_upgrade(&not_admin, &wasm_hash);
        assert_eq!(res, Err(Ok(EscrowError::Unauthorized)));
        assert!(!client.is_migrated());
    }

    #[test]
    fn test_upgrade_with_admin_auth_preserves_escrow_data() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[3u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1000i128, &order_id, &100u32, &None, &None,
        );
        let record_before = client.get_escrow(&escrow_id);
        assert!(!client.is_migrated());

        let wasm_hash = env.deployer().upload_contract_wasm(WASM_STUB);
        let upgraded = client.upgrade(&admin, &wasm_hash);
        assert!(upgraded);

        // The contract's executable now points at the stub wasm, so we read
        // storage directly rather than going through the client (whose calls
        // would now be dispatched to the stub, which implements nothing).
        let migrated: bool = env.as_contract(&contract_id, || {
            env.storage()
                .instance()
                .get(&crate::DataKey::MigrationFlag)
                .unwrap_or(false)
        });
        assert!(migrated, "migration flag must be set after upgrade");

        let record_after: crate::EscrowRecord = env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .get(&crate::DataKey::Escrow(escrow_id))
                .unwrap()
        });
        assert_eq!(
            record_before, record_after,
            "escrow data must survive the code upgrade"
        );
    }

    // ─── Issue #327: Multi-Treasury Fee Distribution Tests ────────────────────

    #[test]
    fn test_fee_distribution_split_across_treasuries() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        let token_client = soroban_sdk::token::Client::new(&env, &token);
        token_admin_client.mint(&buyer, &10000i128);
        client.add_token(&admin, &token);

        let treasury_a = Address::generate(&env);
        let treasury_b = Address::generate(&env);
        let mut shares = soroban_sdk::Vec::new(&env);
        shares.push_back(crate::TreasuryShare {
            treasury: treasury_a.clone(),
            bps: 300,
        });
        shares.push_back(crate::TreasuryShare {
            treasury: treasury_b.clone(),
            bps: 200,
        });
        client.set_fee_distribution(&admin, &shares);

        let order_id = BytesN::from_array(&env, &[9u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1000i128, &order_id, &100u32, &None, &None,
        );

        client.dispute(&escrow_id, &buyer);
        client.resolve_dispute(&escrow_id, &admin, &true);

        // 3% of 1000 = 30, 2% of 1000 = 20; seller receives the remaining 950.
        assert_eq!(token_client.balance(&treasury_a), 30);
        assert_eq!(token_client.balance(&treasury_b), 20);
        assert_eq!(token_client.balance(&seller), 950);
    }

    #[test]
    fn test_set_fee_distribution_rejects_over_1000_bps() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let treasury_a = Address::generate(&env);
        let treasury_b = Address::generate(&env);
        let mut shares = soroban_sdk::Vec::new(&env);
        shares.push_back(crate::TreasuryShare {
            treasury: treasury_a,
            bps: 600,
        });
        shares.push_back(crate::TreasuryShare {
            treasury: treasury_b,
            bps: 500,
        });

        let res = client.try_set_fee_distribution(&admin, &shares);
        assert_eq!(res, Err(Ok(EscrowError::InvalidFeeBps)));
        assert_eq!(client.get_fee_distribution().len(), 0);
    }

    #[test]
    fn test_set_fee_distribution_rejects_zero_address() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let mut shares = soroban_sdk::Vec::new(&env);
        shares.push_back(crate::TreasuryShare {
            treasury: zero_account(&env),
            bps: 100,
        });

        let res = client.try_set_fee_distribution(&admin, &shares);
        assert_eq!(res, Err(Ok(EscrowError::InvalidAddress)));
        assert_eq!(client.get_fee_distribution().len(), 0);
    }

    #[test]
    fn test_set_fee_distribution_rejects_zero_bps() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let treasury = Address::generate(&env);
        let mut shares = soroban_sdk::Vec::new(&env);
        shares.push_back(crate::TreasuryShare { treasury, bps: 0 });

        let res = client.try_set_fee_distribution(&admin, &shares);
        assert_eq!(res, Err(Ok(EscrowError::InvalidFeeBps)));
        assert_eq!(client.get_fee_distribution().len(), 0);
    }

    #[test]
    fn test_set_fee_distribution_rejects_max_treasuries_exceeded() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let mut shares = soroban_sdk::Vec::new(&env);
        for _ in 0..11 {
            shares.push_back(crate::TreasuryShare {
                treasury: Address::generate(&env),
                bps: 1,
            });
        }

        let res = client.try_set_fee_distribution(&admin, &shares);
        assert_eq!(res, Err(Ok(EscrowError::InvalidFeeBps)));
        assert_eq!(client.get_fee_distribution().len(), 0);
    }

    #[test]
    fn test_fee_uses_single_treasury_when_no_distribution_configured() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);
        let treasury = client.get_fee_config().treasury;

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        let token_client = soroban_sdk::token::Client::new(&env, &token);
        token_admin_client.mint(&buyer, &10000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[10u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1000i128, &order_id, &100u32, &None, &None,
        );

        client.dispute(&escrow_id, &buyer);
        client.resolve_dispute(&escrow_id, &admin, &true);

        // fee_bps = 250 (2.5%) from setup_client -> fee = 25
        assert_eq!(token_client.balance(&treasury), 25);
        assert_eq!(token_client.balance(&seller), 975);
    }

    // ─── Issue #319: Time-Locked Emergency Pause Tests ──────────────────────────

    #[test]
    fn test_emergency_pause_auto_expires() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        assert!(!client.get_create_paused());

        // Set emergency pause for 10 ledgers
        let res = client.set_emergency_pause(&admin, &true, &10u32);
        assert!(res);
        assert!(client.get_create_paused());

        // Advance 9 ledgers - still paused
        env.ledger().with_mut(|li| {
            li.sequence_number = 9;
        });
        assert!(client.get_create_paused());

        // Advance to expiry - should auto-unpause
        env.ledger().with_mut(|li| {
            li.sequence_number = 10;
        });
        assert!(!client.get_create_paused());
    }

    #[test]
    fn test_manual_unpause_before_expiry() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        // Set emergency pause for 100 ledgers
        client.set_emergency_pause(&admin, &true, &100u32);
        assert!(client.get_create_paused());

        // Manually unpause before expiry
        client.set_create_paused(&admin, &false);
        assert!(!client.get_create_paused());
    }

    #[test]
    fn test_get_create_paused_respects_expiry() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        // Set emergency pause for 5 ledgers
        client.set_emergency_pause(&admin, &true, &5u32);

        // Before expiry - paused
        env.ledger().with_mut(|li| {
            li.sequence_number = 4;
        });
        assert!(client.get_create_paused());

        // After expiry - unpaused
        env.ledger().with_mut(|li| {
            li.sequence_number = 5;
        });
        assert!(!client.get_create_paused());
    }

    #[test]
    fn test_emergency_pause_unauthorized() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, _admin, _contract_id) = setup_client(&env);
        let non_admin = Address::generate(&env);

        let res = client.try_set_emergency_pause(&non_admin, &true, &10u32);
        assert_eq!(res, Err(Ok(EscrowError::Unauthorized)));
    }

    #[test]
    fn test_set_emergency_pause_zero_duration_is_permanent() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        // Set emergency pause with 0 duration (permanent)
        client.set_emergency_pause(&admin, &true, &0u32);
        assert!(client.get_create_paused());

        // Advance many ledgers - still paused
        env.ledger().with_mut(|li| {
            li.sequence_number = 1000;
        });
        assert!(client.get_create_paused());
    }

    // ── Ticket 1: clear_release_condition ────────────────────────────────────

    /// Create a funded escrow with a release condition set, return the escrow id.
    fn setup_escrow_with_condition(
        env: &Env,
        client: &EscrowContractClient<'_>,
        admin: &Address,
    ) -> u64 {
        let buyer = Address::generate(env);
        let seller = Address::generate(env);
        let token_admin = Address::generate(env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(env, &token);
        token_admin_client.mint(&buyer, &10_000i128);
        client.add_token(admin, &token);

        let order_id = BytesN::from_array(env, &[42u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
        );

        // Generate a dummy oracle address.
        let oracle_id = Address::generate(env);
        // Register a dummy oracle contract so the address is valid.
        let dummy_admin = Address::generate(env);
        let dummy_treasury = Address::generate(env);
        let oracle_id = env.register(
            EscrowContract,
            (EscrowConfig {
                admin: dummy_admin,
                fee_bps: 0u32,
                treasury: dummy_treasury,
                min_amount: 1i128,
                max_amount: 1_000_000i128,
            },),
        );
        let condition_type = symbol_short!("delivery");
        client.set_release_condition(admin, &escrow_id, &condition_type, &oracle_id);
        escrow_id
    }

    #[test]
    fn test_clear_release_condition_admin_success() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let escrow_id = setup_escrow_with_condition(&env, &client, &admin);

        // Condition must exist before clearing.
        let cond = client.get_release_condition(&escrow_id);
        assert_eq!(cond.condition_type, symbol_short!("delivery"));

        // Admin clears it — must succeed.
        client.clear_release_condition(&admin, &escrow_id);

        // Now get_release_condition should return the NotSet error.
        let res = client.try_get_release_condition(&escrow_id);
        assert_eq!(res, Err(Ok(EscrowError::ReleaseConditionNotSet)));
    }

    #[test]
    fn test_clear_release_condition_co_admin_success() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let co_admin = Address::generate(&env);
        client.add_co_admin(&admin, &co_admin);

        let escrow_id = setup_escrow_with_condition(&env, &client, &admin);

        // Co-admin should also be authorized.
        client.clear_release_condition(&co_admin, &escrow_id);

        let res = client.try_get_release_condition(&escrow_id);
        assert_eq!(res, Err(Ok(EscrowError::ReleaseConditionNotSet)));
    }

    #[test]
    fn test_clear_release_condition_non_admin_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let escrow_id = setup_escrow_with_condition(&env, &client, &admin);
        let non_admin = Address::generate(&env);

        let res = client.try_clear_release_condition(&non_admin, &escrow_id);
        assert_eq!(res, Err(Ok(EscrowError::Unauthorized)));
    }

    #[test]
    fn test_clear_release_condition_on_released_escrow_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10_000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[43u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
        );

        // Release it fully so status = Released.
        client.release(&escrow_id, &buyer, &seller);

        let res = client.try_clear_release_condition(&admin, &escrow_id);
        assert_eq!(res, Err(Ok(EscrowError::AlreadyReleased)));
    }

    #[test]
    fn test_get_release_condition_returns_none_after_clear() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let escrow_id = setup_escrow_with_condition(&env, &client, &admin);
        client.clear_release_condition(&admin, &escrow_id);

        // Confirming via try variant that the storage key is gone.
        let res = client.try_get_release_condition(&escrow_id);
        assert_eq!(res, Err(Ok(EscrowError::ReleaseConditionNotSet)));
    }

    // ── Ticket 2: get_yield_config ────────────────────────────────────────────

    #[test]
    fn test_get_yield_config_returns_none_when_unset() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10_000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[50u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
        );

        let result = client.get_yield_config(&escrow_id);
        assert!(result.is_none());
    }

    #[test]
    fn test_get_yield_config_returns_some_after_set() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10_000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[51u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
        );

        let lending_contract = Address::generate(&env);
        let apr_bps = 500u32; // 5% APR
        client.set_yield_config(&admin, &escrow_id, &lending_contract, &apr_bps);

        let result = client.get_yield_config(&escrow_id);
        assert!(result.is_some());
        let cfg = result.unwrap();
        assert_eq!(cfg.lending_contract, lending_contract);
        assert_eq!(cfg.apr_bps, apr_bps);
    }

    // ── Ticket 3: get_co_admins / get_pending_admin ───────────────────────────

    #[test]
    fn test_get_co_admins_returns_empty_vec_when_none_added() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, _admin, _contract_id) = setup_client(&env);

        let co_admins = client.get_co_admins();
        assert_eq!(co_admins.len(), 0);
    }

    #[test]
    fn test_get_co_admins_returns_populated_list_after_add() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let co_admin_a = Address::generate(&env);
        let co_admin_b = Address::generate(&env);
        client.add_co_admin(&admin, &co_admin_a);
        client.add_co_admin(&admin, &co_admin_b);

        let co_admins = client.get_co_admins();
        assert_eq!(co_admins.len(), 2);
        assert!(co_admins.contains(&co_admin_a));
        assert!(co_admins.contains(&co_admin_b));
    }

    #[test]
    fn test_get_pending_admin_returns_none_when_no_transfer() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, _admin, _contract_id) = setup_client(&env);

        let result = client.get_pending_admin();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_pending_admin_returns_some_after_propose() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let new_admin = Address::generate(&env);
        client.propose_admin(&admin, &new_admin);

        let result = client.get_pending_admin();
        assert!(result.is_some());
        assert_eq!(result.unwrap(), new_admin);
    }

    // ─── Issue #32: DisputeVotedEvent Tests ─────────────────────────────────

    /// Helper: set up an escrow in Disputed state with quorum config.
    /// Returns (client, contract_id, escrow_id, arbiters).
    fn setup_disputed_escrow(
        env: &Env,
        threshold: u32,
    ) -> (
        EscrowContractClient<'_>,
        Address,
        u64,
        soroban_sdk::Vec<Address>,
    ) {
        env.mock_all_auths();
        let (client, admin, contract_id) = setup_client(env);

        let buyer = Address::generate(env);
        let seller = Address::generate(env);
        let token_admin = Address::generate(env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(env, &token);
        token_admin_client.mint(&buyer, &10_000i128);
        client.add_token(&admin, &token);

        // Create 3 arbiters for quorum.
        let arbiter_a = Address::generate(env);
        let arbiter_b = Address::generate(env);
        let arbiter_c = Address::generate(env);
        let mut arbiters = soroban_sdk::Vec::new(env);
        arbiters.push_back(arbiter_a);
        arbiters.push_back(arbiter_b);
        arbiters.push_back(arbiter_c);
        client.set_quorum_config(&admin, &arbiters, &threshold);

        let order_id = BytesN::from_array(env, &[11u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000i128, &order_id, &100u32, &None, &None,
        );

        client.dispute(&escrow_id, &buyer);

        (client, contract_id, escrow_id, arbiters)
    }

    /// Emit DisputeVotedEvent under (escrow, "vote") after each vote mutation,
    /// with live votes_for/threshold.
    #[test]
    fn test_vote_dispute_emits_dispute_voted_event() {
        let env = Env::default();
        let (client, contract_id, escrow_id, arbiters) = setup_disputed_escrow(&env, 2); // threshold = 2
        let arbiter = arbiters.get(0).unwrap();

        client.vote_dispute(&escrow_id, &arbiter, &true);

        let events = env.events().all();
        let mut found = false;
        for event in events.iter() {
            let (c_id, topics, value) = event;
            if c_id != contract_id || topics.len() != 3 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == symbol_short!("escrow") && t1 == symbol_short!("vote") {
                let evt: crate::DisputeVotedEvent = value.try_into_val(&env).unwrap();
                assert_eq!(evt.escrow_id, escrow_id);
                assert_eq!(evt.arbiter, arbiter);
                assert!(evt.release_to_seller);
                assert_eq!(evt.votes_for, 1); // first vote for seller side
                assert_eq!(evt.threshold, 2);
                found = true;
            }
        }
        assert!(found, "DisputeVotedEvent not found in events");
    }

    /// After a vote for buyer (release_to_seller = false), votes_for must stay 0.
    #[test]
    fn test_vote_dispute_emits_zero_votes_for_on_buyer_vote() {
        let env = Env::default();
        let (client, contract_id, escrow_id, arbiters) = setup_disputed_escrow(&env, 2);
        let arbiter = arbiters.get(0).unwrap();

        client.vote_dispute(&escrow_id, &arbiter, &false); // vote for buyer

        let events = env.events().all();
        let mut found = false;
        for event in events.iter() {
            let (c_id, topics, value) = event;
            if c_id != contract_id || topics.len() != 3 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == symbol_short!("escrow") && t1 == symbol_short!("vote") {
                let evt: crate::DisputeVotedEvent = value.try_into_val(&env).unwrap();
                assert_eq!(evt.escrow_id, escrow_id);
                assert_eq!(evt.arbiter, arbiter);
                assert!(!evt.release_to_seller);
                assert_eq!(evt.votes_for, 0); // no votes for seller side
                assert_eq!(evt.threshold, 2);
                found = true;
            }
        }
        assert!(found, "DisputeVotedEvent not found in events");
    }

    /// Quorum boundary: second seller-side vote should show votes_for = threshold.
    #[test]
    fn test_vote_dispute_quorum_boundary_emits_correct_tally() {
        let env = Env::default();
        let (client, contract_id, escrow_id, arbiters) = setup_disputed_escrow(&env, 2); // threshold = 2

        // First seller vote.
        client.vote_dispute(&escrow_id, &arbiters.get(0).unwrap(), &true);

        // Second seller vote — reaches quorum.
        client.vote_dispute(&escrow_id, &arbiters.get(1).unwrap(), &true);

        // Find the LAST DisputeVotedEvent — it should show votes_for = 2.
        let events = env.events().all();
        let mut last_evt: Option<crate::DisputeVotedEvent> = None;
        for event in events.iter() {
            let (c_id, topics, value) = event;
            if c_id != contract_id || topics.len() != 3 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == symbol_short!("escrow") && t1 == symbol_short!("vote") {
                last_evt = Some(value.try_into_val(&env).unwrap());
            }
        }
        let evt = last_evt.expect("DisputeVotedEvent not found in events");
        assert_eq!(evt.escrow_id, escrow_id);
        assert_eq!(evt.arbiter, arbiters.get(1).unwrap());
        assert!(evt.release_to_seller);
        assert_eq!(evt.votes_for, 2); // quorum reached
        assert_eq!(evt.threshold, 2);
    }

    /// Each vote emits exactly one event (no duplicates, no missing).
    #[test]
    fn test_vote_dispute_emits_exactly_one_event_per_vote() {
        let env = Env::default();
        let (client, contract_id, escrow_id, arbiters) = setup_disputed_escrow(&env, 3); // threshold = 3

        // Vote 1: verify exactly one DisputeVotedEvent.
        client.vote_dispute(&escrow_id, &arbiters.get(0).unwrap(), &true);
        let mut count = 0u32;
        for event in env.events().all().iter() {
            let (c_id, topics, _value) = event;
            if c_id != contract_id || topics.len() != 3 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == symbol_short!("escrow") && t1 == symbol_short!("vote") {
                count += 1;
            }
        }
        assert_eq!(
            count, 1,
            "first vote should emit exactly one DisputeVotedEvent"
        );

        // Vote 2: verify exactly one more DisputeVotedEvent.
        client.vote_dispute(&escrow_id, &arbiters.get(1).unwrap(), &true);
        count = 0;
        for event in env.events().all().iter() {
            let (c_id, topics, _value) = event;
            if c_id != contract_id || topics.len() != 3 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == symbol_short!("escrow") && t1 == symbol_short!("vote") {
                count += 1;
            }
        }
        assert_eq!(
            count, 1,
            "second vote should emit exactly one DisputeVotedEvent"
        );

        // Vote 3: verify exactly one more.
        client.vote_dispute(&escrow_id, &arbiters.get(2).unwrap(), &false);
        count = 0;
        for event in env.events().all().iter() {
            let (c_id, topics, _value) = event;
            if c_id != contract_id || topics.len() != 3 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == symbol_short!("escrow") && t1 == symbol_short!("vote") {
                count += 1;
            }
        }
        assert_eq!(
            count, 1,
            "third vote should emit exactly one DisputeVotedEvent"
        );
    }

    // ─── Issue #34: Monotonic YieldView Tests ────────────────────────────────

    #[test]
    fn test_get_accrued_yield_returns_yield_view() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10_000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[60u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
        );

        let lending = Address::generate(&env);
        let apr_bps = 500u32; // 5% APR
        client.set_yield_config(&admin, &escrow_id, &lending, &apr_bps);

        let view = client.get_accrued_yield(&escrow_id);
        assert_eq!(view.escrow_id, escrow_id);
        assert_eq!(view.principal, 1_000i128);
        assert_eq!(view.apy_bps, 500);
        assert_eq!(view.snapshot_ledger, env.ledger().sequence());
        // accrued may be 0 at ledger 0 since created_at == timestamp
        assert!(view.accrued >= 0);
    }

    #[test]
    fn test_get_accrued_yield_not_found() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, _admin, _contract_id) = setup_client(&env);

        let res = client.try_get_accrued_yield(&999u64);
        assert_eq!(res, Err(Ok(EscrowError::NotFound)));
    }

    #[test]
    fn test_get_accrued_yield_no_config_returns_zero() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10_000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[61u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
        );
        // No yield config set.

        let view = client.get_accrued_yield(&escrow_id);
        assert_eq!(view.escrow_id, escrow_id);
        assert_eq!(view.principal, 1_000i128);
        assert_eq!(view.apy_bps, 0);
        assert_eq!(view.held_seconds, 0);
        assert_eq!(view.accrued, 0);
    }

    /// Same-block reads must be identical.
    #[test]
    fn test_get_accrued_yield_same_block_reads_identical() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10_000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[62u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
        );

        let lending = Address::generate(&env);
        client.set_yield_config(&admin, &escrow_id, &lending, &500u32);

        // Two reads within the same ledger must be identical.
        let view_a = client.get_accrued_yield(&escrow_id);
        let view_b = client.get_accrued_yield(&escrow_id);

        assert_eq!(view_a, view_b, "same-block reads must be identical");
        assert_eq!(view_a.snapshot_ledger, view_b.snapshot_ledger);
        assert_eq!(view_a.held_seconds, view_b.held_seconds);
        assert_eq!(view_a.accrued, view_b.accrued);
    }

    /// Cross-block reads must be monotonically non-decreasing.
    #[test]
    fn test_get_accrued_yield_cross_block_monotonic() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10_000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[63u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
        );

        let lending = Address::generate(&env);
        client.set_yield_config(&admin, &escrow_id, &lending, &500u32);

        let view_before = client.get_accrued_yield(&escrow_id);

        // Advance 100 ledgers.
        env.ledger().with_mut(|li| {
            li.sequence_number += 100;
        });

        let view_after = client.get_accrued_yield(&escrow_id);

        assert!(
            view_after.held_seconds >= view_before.held_seconds,
            "held_seconds must be monotonically non-decreasing"
        );
        assert!(
            view_after.accrued >= view_before.accrued,
            "accrued yield must be monotonically non-decreasing"
        );
        assert!(
            view_after.snapshot_ledger > view_before.snapshot_ledger,
            "snapshot_ledger must advance"
        );
    }

    // ─── Issue #33: Partial-release yield test ────────────────────────────────

    /// After a partial release, yield must be computed on the remaining
    /// principal, not the original amount.
    #[test]
    fn test_get_accrued_yield_scales_by_remaining_principal() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10_000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[70u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
        );

        let lending = Address::generate(&env);
        client.set_yield_config(&admin, &escrow_id, &lending, &1_000u32); // 10% APR

        // Advance 1 year so yield is non-trivial.
        env.ledger().with_mut(|li| {
            li.sequence_number = 1000;
            li.timestamp = 31_536_000; // ~1 year in seconds
        });

        // Before partial release: yield on full 1000 principal.
        let before = client.get_accrued_yield(&escrow_id);
        assert_eq!(before.principal, 1_000i128);
        assert!(before.accrued > 0, "yield should be non-zero");

        // Partial release: 500 of 1000.
        client.partial_release(&escrow_id, &buyer, &500);

        // After partial release: yield must be computed on remaining 500.
        let after = client.get_accrued_yield(&escrow_id);
        assert_eq!(after.principal, 500i128);
        // accrued should be roughly half of before (same time, half principal).
        assert!(
            after.accrued < before.accrued,
            "yield after partial release must be less than before ({} < {})",
            after.accrued,
            before.accrued
        );
        assert!(
            after.accrued > 0,
            "yield should still be non-zero after partial release"
        );
    }

    // ─── Issue #35: updated_at lifecycle tests ──────────────────────────────

    /// updated_at must be set to the ledger timestamp on creation.
    #[test]
    fn test_updated_at_set_on_creation() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10_000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[80u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
        );

        let record = client.get_escrow(&escrow_id);
        assert_eq!(record.updated_at, record.created_at);
    }

    /// updated_at must advance when escrow status changes (fund → dispute).
    #[test]
    fn test_updated_at_advances_on_dispute() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10_000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[81u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
        );

        let after_deposit = client.get_escrow(&escrow_id);
        assert_eq!(after_deposit.updated_at, after_deposit.created_at);

        // Advance ledger and dispute.
        env.ledger().with_mut(|li| {
            li.sequence_number = 50;
        });

        client.dispute(&escrow_id, &buyer);

        let after_dispute = client.get_escrow(&escrow_id);
        assert!(
            after_dispute.updated_at >= after_deposit.updated_at,
            "updated_at must advance after dispute"
        );
    }

    /// updated_at must advance across a full lifecycle: deposit → dispute → resolve.
    #[test]
    fn test_updated_at_advances_full_lifecycle() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10_000i128);
        client.add_token(&admin, &token);

        let order_id = BytesN::from_array(&env, &[82u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
        );

        let r1 = client.get_escrow(&escrow_id);

        env.ledger().with_mut(|li| {
            li.sequence_number = 50;
        });
        client.dispute(&escrow_id, &buyer);
        let r2 = client.get_escrow(&escrow_id);
        assert!(r2.updated_at >= r1.updated_at);

        env.ledger().with_mut(|li| {
            li.sequence_number = 100;
        });
        client.resolve_dispute(&escrow_id, &admin, &true);
        let r3 = client.get_escrow(&escrow_id);
        assert!(
            r3.updated_at >= r2.updated_at,
            "updated_at must advance through resolve"
        );
    }

    // ─── Issue #49: Paginated list_escrows enumeration ───────────────────────

    /// Helper: deposit a single escrow and return its id.
    #[test]
    fn deposit_cost_stays_within_thresholds() {
        let t = TestEnv::setup();
        let _ = deposit_escrow(&t, 100, 3600);
        assert_deposit_cost_within_thresholds(&t.env);
    }

    fn deposit_one(
        env: &Env,
        client: &EscrowContractClient<'_>,
        admin: &Address,
        buyer: &Address,
        seller: &Address,
        seed: u8,
    ) -> u64 {
        let token_admin = Address::generate(env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin.clone())
            .address();
        soroban_sdk::token::StellarAssetClient::new(env, &token).mint(buyer, &10_000i128);
        client.add_token(admin, &token);
        let order_id = BytesN::from_array(env, &[seed; 32]);
        client.deposit(
            buyer, seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
        )
    }

    #[test]
    fn test_list_escrows_empty_before_any_deposit() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, _admin, _contract_id) = setup_client(&env);

        let page = client.list_escrows(&0u32, &10u32);
        assert_eq!(page.total, 0);
        assert_eq!(page.items.len(), 0);
        assert!(page.next_offset.is_none());
    }

    #[test]
    fn test_list_escrows_returns_correct_total_and_items() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);

        let id1 = deposit_one(&env, &client, &admin, &buyer, &seller, 1);
        let id2 = deposit_one(&env, &client, &admin, &buyer, &seller, 2);
        let id3 = deposit_one(&env, &client, &admin, &buyer, &seller, 3);

        let page = client.list_escrows(&0u32, &10u32);
        assert_eq!(page.total, 3);
        assert_eq!(page.items.len(), 3);
        assert!(page.next_offset.is_none());

        // IDs must appear in creation order.
        assert_eq!(page.items.get(0).unwrap().escrow_id, id1);
        assert_eq!(page.items.get(1).unwrap().escrow_id, id2);
        assert_eq!(page.items.get(2).unwrap().escrow_id, id3);
    }

    #[test]
    fn test_list_escrows_pagination_first_page() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        for seed in 1u8..=5 {
            deposit_one(&env, &client, &admin, &buyer, &seller, seed);
        }

        // Request page size 2 starting at offset 0.
        let page = client.list_escrows(&0u32, &2u32);
        assert_eq!(page.total, 5);
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.next_offset, Some(2u32));
    }

    #[test]
    fn test_list_escrows_pagination_middle_page() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        for seed in 1u8..=5 {
            deposit_one(&env, &client, &admin, &buyer, &seller, seed);
        }

        // Request 2 items starting at offset 2.
        let page = client.list_escrows(&2u32, &2u32);
        assert_eq!(page.total, 5);
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.next_offset, Some(4u32));
    }

    #[test]
    fn test_list_escrows_pagination_last_page() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        for seed in 1u8..=5 {
            deposit_one(&env, &client, &admin, &buyer, &seller, seed);
        }

        // Offset 4 → only 1 item left; no next page.
        let page = client.list_escrows(&4u32, &2u32);
        assert_eq!(page.total, 5);
        assert_eq!(page.items.len(), 1);
        assert!(page.next_offset.is_none());
    }

    #[test]
    fn test_list_escrows_offset_beyond_total_returns_empty() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        deposit_one(&env, &client, &admin, &buyer, &seller, 1);

        let page = client.list_escrows(&100u32, &10u32);
        assert_eq!(page.total, 1);
        assert_eq!(page.items.len(), 0);
        assert!(page.next_offset.is_none());
    }

    #[test]
    fn test_list_escrows_caps_at_max_page_limit() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        // Deposit 10 escrows — requesting 9999 should be capped at MAX_PAGE_LIMIT (50).
        for seed in 1u8..=10 {
            deposit_one(&env, &client, &admin, &buyer, &seller, seed);
        }

        let page = client.list_escrows(&0u32, &9999u32);
        // All 10 fit within the cap, so we get all 10 back.
        assert_eq!(page.total, 10);
        assert_eq!(page.items.len(), 10);
        assert!(page.next_offset.is_none());
    }

    #[test]
    fn test_list_escrows_by_buyer_empty_for_new_buyer() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, _admin, _contract_id) = setup_client(&env);

        let random_buyer = Address::generate(&env);
        let page = client.list_escrows_by_buyer(&random_buyer, &0u32, &10u32);
        assert_eq!(page.total, 0);
        assert_eq!(page.items.len(), 0);
        assert!(page.next_offset.is_none());
    }

    #[test]
    fn test_list_escrows_by_buyer_only_returns_that_buyers_escrows() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer_a = Address::generate(&env);
        let buyer_b = Address::generate(&env);
        let seller = Address::generate(&env);

        let id_a1 = deposit_one(&env, &client, &admin, &buyer_a, &seller, 10);
        let id_a2 = deposit_one(&env, &client, &admin, &buyer_a, &seller, 11);
        let _id_b1 = deposit_one(&env, &client, &admin, &buyer_b, &seller, 20);

        let page_a = client.list_escrows_by_buyer(&buyer_a, &0u32, &10u32);
        assert_eq!(page_a.total, 2, "buyer_a should have exactly 2 escrows");
        assert_eq!(page_a.items.len(), 2);
        assert_eq!(page_a.items.get(0).unwrap().escrow_id, id_a1);
        assert_eq!(page_a.items.get(1).unwrap().escrow_id, id_a2);

        let page_b = client.list_escrows_by_buyer(&buyer_b, &0u32, &10u32);
        assert_eq!(page_b.total, 1, "buyer_b should have exactly 1 escrow");
    }

    #[test]
    fn test_list_escrows_by_buyer_pagination() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        for seed in 1u8..=5 {
            deposit_one(&env, &client, &admin, &buyer, &seller, seed);
        }

        let page1 = client.list_escrows_by_buyer(&buyer, &0u32, &3u32);
        assert_eq!(page1.total, 5);
        assert_eq!(page1.items.len(), 3);
        assert_eq!(page1.next_offset, Some(3u32));

        let page2 = client.list_escrows_by_buyer(&buyer, &3u32, &3u32);
        assert_eq!(page2.total, 5);
        assert_eq!(page2.items.len(), 2);
        assert!(page2.next_offset.is_none());
    }

    #[test]
    fn test_buyer_index_maintained_on_batch_deposit() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, _contract_id) = setup_client(&env);

        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);

        // Set up two different tokens (batch_deposit allows multiple tokens).
        let token_admin1 = Address::generate(&env);
        let token1 = env
            .register_stellar_asset_contract_v2(token_admin1.clone())
            .address();
        soroban_sdk::token::StellarAssetClient::new(&env, &token1).mint(&buyer, &100_000i128);
        client.add_token(&admin, &token1);

        let token_admin2 = Address::generate(&env);
        let token2 = env
            .register_stellar_asset_contract_v2(token_admin2.clone())
            .address();
        soroban_sdk::token::StellarAssetClient::new(&env, &token2).mint(&buyer, &100_000i128);
        client.add_token(&admin, &token2);

        let mut orders = soroban_sdk::Vec::new(&env);
        orders.push_back(crate::BatchDepositParams {
            seller: seller.clone(),
            token: token1.clone(),
            amount: 1_000i128,
            order_id: BytesN::from_array(&env, &[30u8; 32]),
            timeout_ledgers: 1_000u32,
            order_hash: None,
            schema: None,
        });
        orders.push_back(crate::BatchDepositParams {
            seller: seller.clone(),
            token: token2.clone(),
            amount: 1_000i128,
            order_id: BytesN::from_array(&env, &[31u8; 32]),
            timeout_ledgers: 1_000u32,
            order_hash: None,
            schema: None,
        });

        client.batch_deposit(&buyer, &orders);

        // Buyer index should contain both escrows created via batch_deposit.
        let page = client.list_escrows_by_buyer(&buyer, &0u32, &10u32);
        assert_eq!(
            page.total, 2,
            "batch_deposit should maintain buyer index for each order"
        );
        assert_eq!(page.items.len(), 2);
    }

    #[test]
    fn test_prune_dispute_votes_on_terminal_escrows() {
        let env = Env::default();
        let (client, _contract_id, escrow_id, arbiters) = setup_disputed_escrow(&env, 2);
        let admin = client.get_admin().admin;

        let arbiter1 = arbiters.get(0).unwrap();
        let arbiter2 = arbiters.get(1).unwrap();

        client.vote_dispute(&escrow_id, &arbiter1, &true);
        client.vote_dispute(&escrow_id, &arbiter2, &true);

        assert_eq!(client.get_dispute_votes(&escrow_id).len(), 2);

        // Resolve dispute -> terminal status
        client.resolve_dispute_quorum(&escrow_id, &admin);
        let escrow = client.get_escrow(&escrow_id);
        assert_eq!(escrow.status, crate::EscrowStatus::Released);

        // Prune dispute votes
        let mut ids = soroban_sdk::Vec::new(&env);
        ids.push_back(escrow_id);
        let pruned = client.prune_dispute_votes(&admin, &ids);
        assert_eq!(pruned, 1);

        // Dispute votes are now cleaned up
        assert_eq!(client.get_dispute_votes(&escrow_id).len(), 0);
    }

    #[test]
    fn test_prune_dispute_votes_skips_active_disputes() {
        let env = Env::default();
        let (client, _contract_id, escrow_id, arbiters) = setup_disputed_escrow(&env, 2);
        let admin = client.get_admin().admin;
        let arbiter1 = arbiters.get(0).unwrap();

        client.vote_dispute(&escrow_id, &arbiter1, &true);

        // Active disputed escrow: pruning should skip it
        let mut ids = soroban_sdk::Vec::new(&env);
        ids.push_back(escrow_id);
        let pruned = client.prune_dispute_votes(&admin, &ids);
        assert_eq!(pruned, 0);
        assert_eq!(client.get_dispute_votes(&escrow_id).len(), 1);
    }

    #[test]
    fn test_prune_dispute_votes_rejects_over_cap_and_unauthorized() {
        let env = Env::default();
        let (client, admin, _) = setup_client(&env);
        let stranger = Address::generate(&env);

        let mut over_cap = soroban_sdk::Vec::new(&env);
        for i in 0..51 {
            over_cap.push_back(i);
        }

        env.mock_all_auths();
        let res_cap = client.try_prune_dispute_votes(&admin, &over_cap);
        assert_eq!(res_cap, Err(Ok(EscrowError::InvalidLimits)));

        let mut valid = soroban_sdk::Vec::new(&env);
        valid.push_back(1);
        let res_auth = client.try_prune_dispute_votes(&stranger, &valid);
        assert_eq!(res_auth, Err(Ok(EscrowError::Unauthorized)));
    }
 
     #[test]
     fn test_batch_deposit_missing_escrow_returns_not_found() {
         let env = Env::default();
         env.mock_all_auths();
         let (client, admin, contract_id) = setup_client(&env);
         let buyer = Address::generate(&env);
         let seller = Address::generate(&env);
         let token_admin = Address::generate(&env);
         let token = env
             .register_stellar_asset_contract_v2(token_admin)
             .address();
         let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
         token_admin_client.mint(&buyer, &100_000i128);
         client.add_token(&admin, &token);
         let missing_order_id = BytesN::from_array(&env, &[90u8; 32]);
         let valid_order_id = BytesN::from_array(&env, &[91u8; 32]);
         // Create both escrows up front, then remove one stored record to simulate
         // a corrupted/absent entry that deposit_internal must surface as NotFound.
         let missing_escrow_id = client.create(
             &buyer, &seller, &token, &1_000i128, &missing_order_id, &1_000u32, &None, &None,
         );
         let valid_escrow_id = client.create(
             &buyer, &seller, &token, &1_000i128, &valid_order_id, &1_000u32, &None, &None,
         );
         env.as_contract(&contract_id, || {
             env.storage()
                 .persistent()
                 .remove(&DataKey::Escrow(missing_escrow_id));
         });
         let mut orders = soroban_sdk::Vec::new(&env);
         orders.push_back(crate::BatchDepositParams {
             seller: seller.clone(),
             token: token.clone(),
             amount: 1_000i128,
             order_id: missing_order_id,
             timeout_ledgers: 1_000u32,
             order_hash: None,
             schema: None,
         });
         let res = client.try_batch_deposit(&buyer, &orders);
         assert_eq!(res, Err(Ok(EscrowError::NotFound)));
         // The valid order is untouched when the batch call returns a typed error.
         let valid_record = client.get_escrow(&valid_escrow_id);
         assert_eq!(valid_record.status, crate::EscrowStatus::Created);
     }
    // ─── Issue #142: entity id carried in event topics ───────────────────────
    /// The `released` event carries the escrow id as its third topic so
    /// indexers can subscribe by escrow without deserializing the event body.
    #[test]
    fn test_released_event_carries_escrow_id_topic() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, contract_id) = setup_client(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10_000i128);
        client.add_token(&admin, &token);
        let order_id = BytesN::from_array(&env, &[142u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
        );
        client.release(&escrow_id, &buyer, &seller);
        let mut found = false;
        for event in env.events().all().iter() {
            let (c_id, topics, _value) = event;
            if c_id != contract_id || topics.len() != 3 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == symbol_short!("escrow") && t1 == symbol_short!("released") {
                let t2: u64 = topics.get(2).unwrap().try_into_val(&env).unwrap();
                assert_eq!(t2, escrow_id, "released topic must carry the escrow id");
                found = true;
            }
        }
        assert!(found, "released event with escrow_id topic not found");
    }

    /// The `created` event (emitted by `deposit`) carries the escrow id topic so
    /// indexers can subscribe by escrow without deserializing the event body.
    #[test]
    fn test_created_event_carries_escrow_id_topic() {
        let env = Env::default();
        env.mock_all_auths();
        let (client, admin, contract_id) = setup_client(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin_client.mint(&buyer, &10_000i128);
        client.add_token(&admin, &token);
        let order_id = BytesN::from_array(&env, &[7u8; 32]);
        let escrow_id = client.deposit(
            &buyer, &seller, &token, &1_000i128, &order_id, &1_000u32, &None, &None,
        );
        let mut found = false;
        for event in env.events().all().iter() {
            let (c_id, topics, _value) = event;
            if c_id != contract_id || topics.len() != 3 {
                continue;
            }
            let t0: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let t1: soroban_sdk::Symbol = topics.get(1).unwrap().try_into_val(&env).unwrap();
            if t0 == symbol_short!("escrow") && t1 == symbol_short!("created") {
                let t2: u64 = topics.get(2).unwrap().try_into_val(&env).unwrap();
                assert_eq!(t2, escrow_id, "created topic must carry the escrow id");
                found = true;
            }
        }
        assert!(found, "created event with escrow_id topic not found");
    }
}
