use crate::{
    AdminAcceptedEvent, AdminProposedEvent, MarketplaceContract, MarketplaceContractClient,
    MarketplaceError, MerchantRegisteredEvent, MerchantStatus, RegisterParams, Verifier,
    DataKey, MarketplaceContract, MarketplaceContractClient, MarketplaceError, MerchantStatus,
    RegisterParams, Verifier,
    MarketplaceContract, MarketplaceContractClient, MarketplaceError, MerchantCursor,
    MerchantRegisteredEvent, MerchantStatus, RegisterParams, Verifier,
    normalize_symbol, CategoryEntry, DataKey, MarketplaceContract, MarketplaceContractClient,
    MarketplaceError, Merchant, MerchantRegisteredEvent, MerchantStatus, RegisterParams,
    VerificationPolicy, Verifier,
    AdminProposedEvent, MarketplaceContract, MarketplaceContractClient, MarketplaceError,
    MarketplaceContract, MarketplaceContractClient, MarketplaceError, MerchantRegisteredEvent,
    MerchantStatus, RegisterParams, Verifier,
    MerchantStatus, MerchantValidationError, RegisterParams, Verifier,
    MarketplaceContract, MarketplaceContractClient, MarketplaceError, MerchantOperationalView,
    CategoryChange, MarketplaceContract, MarketplaceContractClient, MarketplaceError,
    MerchantCategoryChangedEvent, MerchantRegisteredEvent, MerchantStatus, RegisterParams,
    Verifier,
    MerchantStats, MerchantStatus, RegisterParams, Verifier,
    MerchantStatus, RegisterParams, Verifier, MAX_DESCRIPTION_LEN, MAX_IMAGE_URL_LEN,
    MAX_METADATA_LEN, MAX_NAME_LEN,
};
use delego_reputation::{
    ReputationConfig, ReputationContract, ReputationContractClient, TransactionOutcome,
};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events as _, Ledger as _},
    Address, Env, IntoVal, String, Symbol, TryFromVal, Val,
    Address, Env, String, Symbol, TryIntoVal,
};

const MAX_DISCOVERY_CPU_INSTRUCTIONS: u64 = 2_000_000;
const MAX_DISCOVERY_MEMORY_BYTES: u64 = 2_000_000;

fn assert_discovery_cost_within_thresholds(env: &Env) {
    let budget = env.cost_estimate().budget();
    assert!(budget.cpu_instruction_count() <= MAX_DISCOVERY_CPU_INSTRUCTIONS);
    assert!(budget.memory_bytes() <= MAX_DISCOVERY_MEMORY_BYTES);
}

struct TestFixture<'a> {
    env: Env,
    admin: Address,
    client: MarketplaceContractClient<'a>,
    _contract_id: Address,
}

impl<'a> TestFixture<'a> {
    fn setup() -> Self {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let contract_id = env.register(MarketplaceContract, (admin.clone(),));
        let client = MarketplaceContractClient::new(&env, &contract_id);

        TestFixture {
            env,
            admin,
            client,
            _contract_id: contract_id,
        }
    }
}

fn store_name(env: &Env, i: u32) -> String {
    let mut buf = [0u8; 9];
    buf[0] = b'S';
    buf[1] = b't';
    buf[2] = b'o';
    buf[3] = b'r';
    buf[4] = b'e';
    buf[5] = b'0' + ((i / 1000) % 10) as u8;
    buf[6] = b'0' + ((i / 100) % 10) as u8;
    buf[7] = b'0' + ((i / 10) % 10) as u8;
    buf[8] = b'0' + (i % 10) as u8;
    String::from_str(env, core::str::from_utf8(&buf).unwrap())
}

#[test]
fn test_constructor_and_version() {
    let f = TestFixture::setup();

    assert_eq!(f.client.get_admin(), f.admin);
    assert_eq!(f.client.get_metadata_cooldown(), 86_400);

    let ver = f.client.version();
    assert_eq!(ver.name, symbol_short!("market"));
    assert_eq!(ver.semver, symbol_short!("0_2_0"));
    let expected = env!("CARGO_PKG_VERSION").replace('.', "_");
    assert_eq!(ver.semver, soroban_sdk::Symbol::new(&f.env, &expected));
}

#[test]
fn test_register_merchant_happy_path() {
    let f = TestFixture::setup();
    let merchant_addr = Address::generate(&f.env);

    let params = RegisterParams {
        name: String::from_str(&f.env, "  Acme Store  "),
        description: String::from_str(&f.env, "  High quality tools  "),
        category: symbol_short!("tools"),
        image_url: String::from_str(&f.env, "https://cdn.example.com/logo.png"),
        metadata: Some(String::from_str(&f.env, "ipfs://Qm123")),
        required_verifications: 1,
    };

    let merchant_id = f.client.register_merchant(&merchant_addr, &params);
    assert_eq!(merchant_id, 1);

    let merchant = f.client.get_merchant(&merchant_id);
    assert_eq!(merchant.id, 1);
    assert_eq!(merchant.owner, Some(merchant_addr.clone()));
    assert_eq!(merchant.name, String::from_str(&f.env, "Acme Store"));
    assert_eq!(merchant.description, String::from_str(&f.env, "High quality tools"));
    assert_eq!(merchant.category, symbol_short!("tools"));
    assert_eq!(merchant.commission_rate_bps, 0);
    assert!(!merchant.verified);
    assert_eq!(merchant.status, MerchantStatus::Registered);

    let view = f.client.get_merchant_view(&merchant_id);
    assert_eq!(view.id, 1);
    assert!(!view.verified);
    assert_eq!(view.reputation_score, None);
}

#[test]
fn test_merchant_operational_view_combinations() {
    let f = TestFixture::setup();

    // Registered + unverified
    let owner1 = Address::generate(&f.env);
    let id1 = f.client.register_merchant(
        &owner1,
        &RegisterParams {
            name: String::from_str(&f.env, "Registered Unverified"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            required_verifications: 1,
        },
    );
    let view: MerchantOperationalView = f.client.get_merchant_operational_view(&id1);
    assert_eq!(view.id, id1);
    assert_eq!(view.name, String::from_str(&f.env, "Registered Unverified"));
    assert_eq!(view.status, MerchantStatus::Registered);
    assert!(!view.verified);
    assert!(!view.effective);

    // Verified + verified (effective active)
    let owner2 = Address::generate(&f.env);
    let verifier = Address::generate(&f.env);
    f.client.add_verifier(
        &f.admin,
        &Verifier {
            address: verifier.clone(),
            label: symbol_short!("kyc"),
            registered_at: 1,
        },
    );
    let id2 = f.client.register_merchant(
        &owner2,
        &RegisterParams {
            name: String::from_str(&f.env, "Verified Active"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            required_verifications: 1,
        },
    );
    f.client.verify_merchant(&id2, &verifier);
    let view = f.client.get_merchant_operational_view(&id2);
    assert_eq!(view.status, MerchantStatus::Verified);
    assert!(view.verified);
    assert!(view.effective);

    // Suspended + verified (gap case)
    f.client.suspend_merchant(&f.admin, &id2);
    let view = f.client.get_merchant_operational_view(&id2);
    assert_eq!(view.status, MerchantStatus::Suspended);
    assert!(view.verified);
    assert!(!view.effective);

    // Suspended + unverified
    let owner3 = Address::generate(&f.env);
    let id3 = f.client.register_merchant(
        &owner3,
        &RegisterParams {
            name: String::from_str(&f.env, "Suspended Unverified"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            required_verifications: 1,
        },
    );
    f.client.suspend_merchant(&f.admin, &id3);
    let view = f.client.get_merchant_operational_view(&id3);
    assert_eq!(view.status, MerchantStatus::Suspended);
    assert!(!view.verified);
    assert!(!view.effective);

    // Closed + verified
    let owner4 = Address::generate(&f.env);
    let id4 = f.client.register_merchant(
        &owner4,
        &RegisterParams {
            name: String::from_str(&f.env, "Closed Verified"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            required_verifications: 1,
        },
    );
    f.client.verify_merchant(&id4, &verifier);
    f.client.close_merchant(&f.admin, &id4, &symbol_short!("miscond"));
    let view = f.client.get_merchant_operational_view(&id4);
    assert_eq!(view.status, MerchantStatus::Closed);
    assert!(view.verified);
    assert!(!view.effective);

    // Closed + unverified
    let owner5 = Address::generate(&f.env);
    let id5 = f.client.register_merchant(
        &owner5,
        &RegisterParams {
            name: String::from_str(&f.env, "Closed Unverified"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            required_verifications: 1,
        },
    );
    f.client.close_merchant(&f.admin, &id5, &symbol_short!("miscond"));
    let view = f.client.get_merchant_operational_view(&id5);
    assert_eq!(view.status, MerchantStatus::Closed);
    assert!(!view.verified);
    assert!(!view.effective);
}

#[test]
fn test_register_merchant_event_schema() {
    let f = TestFixture::setup();
    let merchant_addr = Address::generate(&f.env);

    let params = RegisterParams {
        name: String::from_str(&f.env, "Schema Check Store"),
        description: String::from_str(&f.env, "Tests event payload"),
        category: symbol_short!("retail"),
        image_url: String::from_str(&f.env, "https://example.com/schema.png"),
        metadata: None,
        required_verifications: 1,
    };

    let merchant_id = f.client.register_merchant(&merchant_addr, &params);
    assert_eq!(merchant_id, 1);

    // Verify the event payload carries `owner` explicitly (not `merchant`).
    // The struct MerchantRegisteredEvent is the canonical on-chain schema.
    let expected_event = MerchantRegisteredEvent {
        merchant_id: 1,
        owner: merchant_addr.clone(),
        name: String::from_str(&f.env, "Schema Check Store"),
    };

    // Verify the event can be deserialized with the new field name.
    assert_eq!(expected_event.merchant_id, 1);
    assert_eq!(expected_event.owner, merchant_addr);
    assert_eq!(
        expected_event.name,
        String::from_str(&f.env, "Schema Check Store")
    );
}

#[test]
fn test_register_merchant_duplicate_name_and_invalid_param() {
    let f = TestFixture::setup();
    let merchant1 = Address::generate(&f.env);
    let merchant2 = Address::generate(&f.env);

    let params1 = RegisterParams {
        name: String::from_str(&f.env, "Store Unique"),
        description: String::from_str(&f.env, "First store"),
        category: symbol_short!("retail"),
        image_url: String::from_str(&f.env, "https://cdn.example.com/1.png"),
        metadata: None,
        required_verifications: 1,
    };

    let id = f.client.register_merchant(&merchant1, &params1);
    assert_eq!(id, 1);

    // Duplicate name
    let params2 = RegisterParams {
        name: String::from_str(&f.env, "Store Unique"),
        description: String::from_str(&f.env, "Second store"),
        category: symbol_short!("retail"),
        image_url: String::from_str(&f.env, "https://cdn.example.com/2.png"),
        metadata: None,
        required_verifications: 1,
    };

    let err = f.client.try_register_merchant(&merchant2, &params2);
    assert_eq!(
        err.unwrap_err().unwrap(),
        MarketplaceError::DuplicateMerchantName
    );

    // Empty name
    let params_empty = RegisterParams {
        name: String::from_str(&f.env, ""),
        description: String::from_str(&f.env, "Empty"),
        category: symbol_short!("retail"),
        image_url: String::from_str(&f.env, ""),
        metadata: None,
        required_verifications: 1,
    };

    let err_empty = f.client.try_register_merchant(&merchant2, &params_empty);
    assert_eq!(
        err_empty.unwrap_err().unwrap(),
        MarketplaceError::MerchantValidationError(MerchantValidationError::EmptyName)
    );

    // Whitespace-only name
    let params_ws_name = RegisterParams {
        name: String::from_str(&f.env, "   "),
        description: String::from_str(&f.env, "Desc"),
        category: symbol_short!("retail"),
        image_url: String::from_str(&f.env, ""),
        metadata: None,
        required_verifications: 1,
    };

    let err_ws_name = f.client.try_register_merchant(&merchant2, &params_ws_name);
    assert_eq!(
        err_ws_name.unwrap_err().unwrap(),
        MarketplaceError::MerchantValidationError(MerchantValidationError::WhitespaceOnly)
    );

    // Empty description
    let params_empty_desc = RegisterParams {
        name: String::from_str(&f.env, "Valid Store"),
        description: String::from_str(&f.env, ""),
        category: symbol_short!("retail"),
        image_url: String::from_str(&f.env, ""),
        metadata: None,
        required_verifications: 1,
    };

    let err_empty_desc = f.client.try_register_merchant(&merchant2, &params_empty_desc);
    assert_eq!(
        err_empty_desc.unwrap_err().unwrap(),
        MarketplaceError::MerchantValidationError(MerchantValidationError::EmptyDescription)
    );

    // Whitespace-only description
    let params_ws_desc = RegisterParams {
        name: String::from_str(&f.env, "Valid Store"),
        description: String::from_str(&f.env, "   "),
        category: symbol_short!("retail"),
        image_url: String::from_str(&f.env, ""),
        metadata: None,
        required_verifications: 1,
    };

    let err_ws_desc = f.client.try_register_merchant(&merchant2, &params_ws_desc);
    assert_eq!(
        err_ws_desc.unwrap_err().unwrap(),
        MarketplaceError::MerchantValidationError(MerchantValidationError::WhitespaceOnly)
    );
}

#[test]
fn test_update_merchant_profile() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);
    let stranger = Address::generate(&f.env);

    let id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Store A"),
            description: String::from_str(&f.env, "Old Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "old.png"),
            metadata: None,
            required_verifications: 1,
        },
    );

    // Unauthorized caller
    let unauth_err = f.client.try_update_merchant_profile(
        &id,
        &stranger,
        &String::from_str(&f.env, "New Name"),
        &String::from_str(&f.env, "New Desc"),
        &String::from_str(&f.env, "new.png"),
        &None,
    );
    assert_eq!(
        unauth_err.unwrap_err().unwrap(),
        MarketplaceError::Unauthorized
    );

    // Empty name rejected
    let empty_name_err = f.client.try_update_merchant_profile(
        &id,
        &owner,
        &String::from_str(&f.env, ""),
        &String::from_str(&f.env, "New Desc"),
        &String::from_str(&f.env, "new.png"),
    );
    assert_eq!(
        empty_name_err.unwrap_err().unwrap(),
        MarketplaceError::MerchantValidationError(MerchantValidationError::EmptyName)
    );

    // Whitespace-only name rejected
    let ws_name_err = f.client.try_update_merchant_profile(
        &id,
        &owner,
        &String::from_str(&f.env, "   "),
        &String::from_str(&f.env, "New Desc"),
        &String::from_str(&f.env, "new.png"),
    );
    assert_eq!(
        ws_name_err.unwrap_err().unwrap(),
        MarketplaceError::MerchantValidationError(MerchantValidationError::WhitespaceOnly)
    );

    // Empty description rejected
    let empty_desc_err = f.client.try_update_merchant_profile(
        &id,
        &owner,
        &String::from_str(&f.env, "Store A"),
        &String::from_str(&f.env, ""),
        &String::from_str(&f.env, "new.png"),
    );
    assert_eq!(
        empty_desc_err.unwrap_err().unwrap(),
        MarketplaceError::MerchantValidationError(MerchantValidationError::EmptyDescription)
    );

    // Whitespace-only description rejected
    let ws_desc_err = f.client.try_update_merchant_profile(
        &id,
        &owner,
        &String::from_str(&f.env, "Store A"),
        &String::from_str(&f.env, "   "),
        &String::from_str(&f.env, "new.png"),
    );
    assert_eq!(
        ws_desc_err.unwrap_err().unwrap(),
        MarketplaceError::MerchantValidationError(MerchantValidationError::WhitespaceOnly)
    );

    // Owner succeeds
    f.client.update_merchant_profile(
        &id,
        &owner,
        &String::from_str(&f.env, "  Store A Updated  "),
        &String::from_str(&f.env, "  New Desc  "),
        &String::from_str(&f.env, "new.png"),
        &None,
    );

    let updated = f.client.get_merchant(&id);
    assert_eq!(updated.name, String::from_str(&f.env, "Store A Updated"));
    assert_eq!(updated.description, String::from_str(&f.env, "New Desc"));
    assert_eq!(updated.image_url, String::from_str(&f.env, "new.png"));
}

#[test]
fn test_update_metadata_cooldown_and_admin_override() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);

    // Set cooldown to 1000 seconds
    f.client.set_metadata_cooldown(&f.admin, &1000);
    assert_eq!(f.client.get_metadata_cooldown(), 1000);

    f.env.ledger().set_timestamp(10_000);

    let id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Store Meta"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "logo.png"),
            metadata: Some(String::from_str(&f.env, "ipfs://v1")),
            required_verifications: 1,
        },
    );

    // Immediate update by owner should fail due to cooldown lock
    let locked_err =
        f.client
            .try_update_metadata(&id, &owner, &Some(String::from_str(&f.env, "ipfs://v2")));
    assert_eq!(
        locked_err.unwrap_err().unwrap(),
        MarketplaceError::MetadataLockActive
    );

    // Admin can override cooldown
    f.client.update_metadata(
        &id,
        &f.admin,
        &Some(String::from_str(&f.env, "ipfs://admin-override")),
    );
    let merchant = f.client.get_merchant(&id);
    assert_eq!(
        merchant.metadata,
        Some(String::from_str(&f.env, "ipfs://admin-override"))
    );

    // Advance ledger timestamp beyond cooldown
    f.env.ledger().set_timestamp(11_500);

    // Owner can now update
    f.client
        .update_metadata(&id, &owner, &Some(String::from_str(&f.env, "ipfs://v3")));
    let merchant2 = f.client.get_merchant(&id);
    assert_eq!(
        merchant2.metadata,
        Some(String::from_str(&f.env, "ipfs://v3"))
    );
}

#[test]
fn test_update_metadata_change_detection_noop() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);

    // Set cooldown to 1000 seconds to make cooldown detection difficult
    f.client.set_metadata_cooldown(&f.admin, &1000);
    f.env.ledger().set_timestamp(10_000);

    let initial_metadata = Some(String::from_str(&f.env, "ipfs://original"));
    let id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "NoOp Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "logo.png"),
            metadata: initial_metadata.clone(),
            required_verifications: 1,
        },
    );

    let merchant_before = f.client.get_merchant(&id);
    assert_eq!(merchant_before.metadata, initial_metadata);
    let updated_at_before = merchant_before.updated_at;

    // Attempt to update with identical metadata (no-op)
    // Even though cooldown would normally block owner, change-detection should allow this
    // because no actual change occurs
    f.client.update_metadata(&id, &owner, &initial_metadata);

    let merchant_after = f.client.get_merchant(&id);
    // Verify metadata is unchanged
    assert_eq!(merchant_after.metadata, initial_metadata);
    // Verify updated_at timestamp was NOT updated (no write occurred)
    assert_eq!(merchant_after.updated_at, updated_at_before);
}

#[test]
fn test_update_metadata_change_detection_with_change() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);

    // Set cooldown to 0 to focus on change-detection behavior
    f.client.set_metadata_cooldown(&f.admin, &60);
    f.env.ledger().set_timestamp(10_000);

    let initial_metadata = Some(String::from_str(&f.env, "ipfs://v1"));
    let id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Change Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "logo.png"),
            metadata: initial_metadata.clone(),
            required_verifications: 1,
        },
    );

    let merchant_before = f.client.get_merchant(&id);
    assert_eq!(merchant_before.metadata, initial_metadata);

    // Advance time beyond cooldown
    f.env.ledger().set_timestamp(10_100);

    // Update with different metadata
    let new_metadata = Some(String::from_str(&f.env, "ipfs://v2"));
    f.client.update_metadata(&id, &owner, &new_metadata);

    let merchant_after = f.client.get_merchant(&id);
    // Verify metadata changed
    assert_eq!(merchant_after.metadata, new_metadata);
    // Verify updated_at was updated
    assert_eq!(merchant_after.updated_at, 10_100);
}

#[test]
fn test_update_metadata_noop_with_none_values() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);

    f.client.set_metadata_cooldown(&f.admin, &1000);
    f.env.ledger().set_timestamp(10_000);

    // Register merchant with no metadata
    let id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "NoMeta Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "logo.png"),
            metadata: None,
            required_verifications: 1,
        },
    );

    let merchant_before = f.client.get_merchant(&id);
    assert_eq!(merchant_before.metadata, None);
    let updated_at_before = merchant_before.updated_at;

    // Attempt to update with None (identical to stored value, no-op)
    f.client.update_metadata(&id, &owner, &None);

    let merchant_after = f.client.get_merchant(&id);
    // Verify metadata is still None
    assert_eq!(merchant_after.metadata, None);
    // Verify updated_at was NOT changed
    assert_eq!(merchant_after.updated_at, updated_at_before);
}

#[test]
fn test_update_metadata_change_from_none_to_value() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);

    f.client.set_metadata_cooldown(&f.admin, &60);
    f.env.ledger().set_timestamp(10_000);

    // Register merchant with no metadata
    let id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "AddMeta Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "logo.png"),
            metadata: None,
            required_verifications: 1,
        },
    );

    // Advance time beyond cooldown
    f.env.ledger().set_timestamp(10_100);

    // Add metadata (None -> Some)
    let new_metadata = Some(String::from_str(&f.env, "ipfs://added"));
    f.client.update_metadata(&id, &owner, &new_metadata);

    let merchant_after = f.client.get_merchant(&id);
    // Verify metadata was added
    assert_eq!(merchant_after.metadata, new_metadata);
    // Verify updated_at was updated
    assert_eq!(merchant_after.updated_at, 10_100);
}

#[test]
fn test_update_metadata_change_from_value_to_none() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);

    f.client.set_metadata_cooldown(&f.admin, &60);
    f.env.ledger().set_timestamp(10_000);

    // Register merchant with metadata
    let initial_metadata = Some(String::from_str(&f.env, "ipfs://remove-me"));
    let id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "RemoveMeta Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "logo.png"),
            metadata: initial_metadata.clone(),
            required_verifications: 1,
        },
    );

    // Advance time beyond cooldown
    f.env.ledger().set_timestamp(10_100);

    // Remove metadata (Some -> None)
    f.client.update_metadata(&id, &owner, &None);

    let merchant_after = f.client.get_merchant(&id);
    // Verify metadata was removed
    assert_eq!(merchant_after.metadata, None);
    // Verify updated_at was updated
    assert_eq!(merchant_after.updated_at, 10_100);
}

#[test]
fn test_metadata_cooldown_is_bounded_and_noop_is_silent() {
    let f = TestFixture::setup();

    f.client.set_metadata_cooldown(&f.admin, &0);
    assert_eq!(f.client.get_metadata_cooldown(), 60);

    f.client
        .set_metadata_cooldown(&f.admin, &(31 * 24 * 60 * 60));
    assert_eq!(f.client.get_metadata_cooldown(), 30 * 24 * 60 * 60);

    // Repeating the same value is a no-op and must not alter the config.
    f.client
        .set_metadata_cooldown(&f.admin, &(30 * 24 * 60 * 60));
    assert_eq!(f.client.get_metadata_cooldown(), 30 * 24 * 60 * 60);
}

#[test]
fn test_verifier_management() {
    let f = TestFixture::setup();
    let verifier_addr = Address::generate(&f.env);
    let stranger = Address::generate(&f.env);

    let verifier = Verifier {
        address: verifier_addr.clone(),
        label: symbol_short!("kyc"),
        registered_at: f.env.ledger().timestamp(),
    };

    // Unauthorized add
    let unauth_err = f.client.try_add_verifier(&stranger, &verifier);
    assert_eq!(
        unauth_err.unwrap_err().unwrap(),
        MarketplaceError::Unauthorized
    );

    // Admin adds verifier
    f.client.add_verifier(&f.admin, &verifier);
    let verifiers = f.client.get_verifiers();
    assert_eq!(verifiers.len(), 1);
    assert_eq!(verifiers.get(0).unwrap().address, verifier_addr);

    // Duplicate add fails
    let dup_err = f.client.try_add_verifier(&f.admin, &verifier);
    assert_eq!(
        dup_err.unwrap_err().unwrap(),
        MarketplaceError::VerifierAlreadyExists
    );

    // Admin removes verifier
    f.client.remove_verifier(&f.admin, &verifier_addr);
    assert_eq!(f.client.get_verifiers().len(), 0);

    // Removing non-existent fails
    let not_found_err = f.client.try_remove_verifier(&f.admin, &verifier_addr);
    assert_eq!(
        not_found_err.unwrap_err().unwrap(),
        MarketplaceError::VerifierNotFound
    );
}

#[test]
fn test_multi_verifier_verification_and_revocation() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);
    let v1 = Address::generate(&f.env);
    let v2 = Address::generate(&f.env);
    let unreg = Address::generate(&f.env);

    f.client.add_verifier(
        &f.admin,
        &Verifier {
            address: v1.clone(),
            label: symbol_short!("kyc"),
            registered_at: 100,
        },
    );
    f.client.add_verifier(
        &f.admin,
        &Verifier {
            address: v2.clone(),
            label: symbol_short!("audit"),
            registered_at: 100,
        },
    );

    // Register with 2 required verifications
    let id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Certified Store"),
            description: String::from_str(&f.env, "Requires 2 verifiers"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "tech.png"),
            metadata: None,
            required_verifications: 2,
        },
    );

    // Unregistered verifier cannot verify
    let unreg_err = f.client.try_verify_merchant(&id, &unreg);
    assert_eq!(
        unreg_err.unwrap_err().unwrap(),
        MarketplaceError::Unauthorized
    );

    // First verifier verifies
    f.client.verify_merchant(&id, &v1);
    let mid_state = f.client.get_merchant(&id);
    assert!(!mid_state.verified);
    assert_eq!(mid_state.status, MerchantStatus::Registered);

    // Same verifier cannot verify again
    let dup_verif = f.client.try_verify_merchant(&id, &v1);
    assert_eq!(
        dup_verif.unwrap_err().unwrap(),
        MarketplaceError::AlreadyVerified
    );

    // Second verifier verifies -> threshold reached!
    f.client.verify_merchant(&id, &v2);
    let verified_state = f.client.get_merchant(&id);
    assert!(verified_state.verified);
    assert_eq!(verified_state.status, MerchantStatus::Verified);

    let view = f.client.get_merchant_view(&id);
    assert!(view.verified);
    assert_eq!(view.status, MerchantStatus::Verified);

    // Revocation by admin
    f.client.revoke_verification(&f.admin, &id);
    let revoked_state = f.client.get_merchant(&id);
    assert!(!revoked_state.verified);
    assert_eq!(revoked_state.status, MerchantStatus::Registered);
}

#[test]
fn test_verify_merchant_verified_count_overflow() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);
    let verifier = Address::generate(&f.env);

    f.client.add_verifier(
        &f.admin,
        &Verifier {
            address: verifier.clone(),
            label: symbol_short!("kyc"),
            registered_at: 1,
        },
    );

    let id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Overflow Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "img.png"),
            metadata: None,
            required_verifications: 1,
        },
    );

    f.env.as_contract(&f._contract_id, || {
        f.env
            .storage()
            .instance()
            .set(&DataKey::VerifiedCount(id), &u32::MAX);
    });

    let err = f.client.try_verify_merchant(&id, &verifier);
    assert_eq!(
        err.unwrap_err().unwrap(),
        MarketplaceError::VerificationCountOverflow
    );
}

#[test]
fn test_commission_rate_configuration() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);
    let stranger = Address::generate(&f.env);

    let id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Commission Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "img.png"),
            metadata: None,
            required_verifications: 1,
        },
    );

    // Over 10000 bps fails
    let err_over = f.client.try_set_merchant_commission(&id, &owner, &10_001);
    assert_eq!(
        err_over.unwrap_err().unwrap(),
        MarketplaceError::InvalidCommissionBps
    );

    // Stranger unauthorized
    let err_unauth = f.client.try_set_merchant_commission(&id, &stranger, &500);
    assert_eq!(
        err_unauth.unwrap_err().unwrap(),
        MarketplaceError::Unauthorized
    );

    // Owner sets commission
    f.client.set_merchant_commission(&id, &owner, &500); // 5.00%
    assert_eq!(f.client.get_commission(&id), 500);

    // Admin sets commission
    f.client.set_merchant_commission(&id, &f.admin, &250); // 2.50%
    assert_eq!(f.client.get_commission(&id), 250);
}

#[test]
fn test_merchant_stats_lifecycle() {
    let f = TestFixture::setup();
    let owner1 = Address::generate(&f.env);
    let owner2 = Address::generate(&f.env);

    let id1 = f.client.register_merchant(
        &owner1,
        &RegisterParams {
            name: String::from_str(&f.env, "Alpha Store"),
            description: String::from_str(&f.env, "Desc 1"),
            category: symbol_short!("food"),
            image_url: String::from_str(&f.env, "alpha.png"),
            metadata: None,
            required_verifications: 1,
        },
    );
    let id2 = f.client.register_merchant(
        &owner2,
        &RegisterParams {
            name: String::from_str(&f.env, "Beta Store"),
            description: String::from_str(&f.env, "Desc 2"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "beta.png"),
            metadata: None,
            required_verifications: 1,
        },
    );

    assert_eq!(
        f.client.get_merchant_stats(),
        MerchantStats {
            total: 2,
            active: 2,
            suspended: 0,
            closed: 0,
        }
    );

    f.client.suspend_merchant(&f.admin, &id1);
    assert_eq!(
        f.client.get_merchant_stats(),
        MerchantStats {
            total: 2,
            active: 1,
            suspended: 1,
            closed: 0,
        }
    );

    f.client.unsuspend_merchant(&f.admin, &id1);
    assert_eq!(
        f.client.get_merchant_stats(),
        MerchantStats {
            total: 2,
            active: 2,
            suspended: 0,
            closed: 0,
        }
    );

    f.client.close_merchant(&f.admin, &id2, &symbol_short!("miscond"));
    assert_eq!(
        f.client.get_merchant_stats(),
        MerchantStats {
            total: 2,
            active: 1,
            suspended: 0,
            closed: 1,
        }
    );
}

#[test]
fn test_suspension_closing_and_mutation_locking() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);
    let verifier = Address::generate(&f.env);

    f.client.add_verifier(
        &f.admin,
        &Verifier {
            address: verifier.clone(),
            label: symbol_short!("kyc"),
            registered_at: 1,
        },
    );

    let id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Safe Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("food"),
            image_url: String::from_str(&f.env, "food.png"),
            metadata: None,
            required_verifications: 1,
        },
    );

    // Admin suspends merchant
    f.client.suspend_merchant(&f.admin, &id);
    let suspended = f.client.get_merchant(&id);
    assert_eq!(suspended.status, MerchantStatus::Suspended);

    // Mutating ops must be blocked with MerchantFrozen
    let prof_err = f.client.try_update_merchant_profile(
        &id,
        &owner,
        &String::from_str(&f.env, "New"),
        &String::from_str(&f.env, "Desc"),
        &String::from_str(&f.env, "url"),
        &None,
    );
    assert_eq!(
        prof_err.unwrap_err().unwrap(),
        MarketplaceError::MerchantFrozen
    );

    let meta_err = f.client.try_update_metadata(&id, &owner, &None);
    assert_eq!(
        meta_err.unwrap_err().unwrap(),
        MarketplaceError::MerchantFrozen
    );

    let comm_err = f.client.try_set_merchant_commission(&id, &owner, &100);
    assert_eq!(
        comm_err.unwrap_err().unwrap(),
        MarketplaceError::MerchantFrozen
    );

    let verif_err = f.client.try_verify_merchant(&id, &verifier);
    assert_eq!(
        verif_err.unwrap_err().unwrap(),
        MarketplaceError::MerchantFrozen
    );

    // Admin unsuspends merchant
    f.client.unsuspend_merchant(&f.admin, &id);
    let unsuspended = f.client.get_merchant(&id);
    assert_eq!(unsuspended.status, MerchantStatus::Registered);

    // Mutating ops work again
    f.client.set_merchant_commission(&id, &owner, &150);
    assert_eq!(f.client.get_commission(&id), 150);

    // Admin closes merchant permanently
    f.client.close_merchant(&f.admin, &id, &symbol_short!("bad_cond"));
    f.client
        .close_merchant(&f.admin, &id, &symbol_short!("miscond"));
    let closed = f.client.get_merchant(&id);
    assert_eq!(closed.status, MerchantStatus::Closed);

    // Mutating ops blocked with MerchantClosed
    let comm_err_closed = f.client.try_set_merchant_commission(&id, &owner, &200);
    assert_eq!(
        comm_err_closed.unwrap_err().unwrap(),
        MarketplaceError::MerchantClosed
    );

    // Cannot suspend or unsuspend closed merchant
    let susp_err = f.client.try_suspend_merchant(&f.admin, &id);
    assert_eq!(
        susp_err.unwrap_err().unwrap(),
        MarketplaceError::MerchantClosed
    );

    let unsusp_err = f.client.try_unsuspend_merchant(&f.admin, &id);
    assert_eq!(
        unsusp_err.unwrap_err().unwrap(),
        MarketplaceError::MerchantClosed
    );
}

#[test]
fn test_paginated_discovery_cost_stays_within_thresholds() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);
    f.client.register_merchant(&owner, &RegisterParams {
        name: String::from_str(&f.env, "Cost Store"),
        description: String::from_str(&f.env, "Desc"),
        category: symbol_short!("tech"),
        image_url: String::from_str(&f.env, "url"),
        metadata: None,
        required_verifications: 1,
    });
    let _ = f.client.get_merchants(&0, &10);
    assert_discovery_cost_within_thresholds(&f.env);
}

#[test]
fn test_paginated_discovery() {
    let f = TestFixture::setup();

    for i in 1..=5 {
        let owner = Address::generate(&f.env);
        let category = if i <= 3 {
            symbol_short!("tech")
        } else {
            symbol_short!("books")
        };
        let mut name_bytes = [0u8; 8];
        name_bytes[0] = b'S';
        name_bytes[1] = b't';
        name_bytes[2] = b'o';
        name_bytes[3] = b'r';
        name_bytes[4] = b'e';
        name_bytes[5] = b'0' + i as u8;
        let name = String::from_str(&f.env, core::str::from_utf8(&name_bytes[..6]).unwrap());

        f.client.register_merchant(
            &owner,
            &RegisterParams {
                name,
                description: String::from_str(&f.env, "Desc"),
                category,
                image_url: String::from_str(&f.env, "url"),
                metadata: None,
                required_verifications: 1,
            },
        );
    }

    // Page 1: 2 items
    let page1 = f.client.get_merchants(&0, &2);
    assert_eq!(page1.items.len(), 2);
    assert_eq!(page1.total, 5);
    assert_eq!(page1.next_offset, Some(2));
    assert_eq!(page1.items.get(0).unwrap().id, 1);
    assert_eq!(page1.items.get(1).unwrap().id, 2);

    // Page 2: 2 items
    let page2 = f.client.get_merchants(&2, &2);
    assert_eq!(page2.items.len(), 2);
    assert_eq!(page2.total, 5);
    assert_eq!(page2.next_offset, Some(4));
    assert_eq!(page2.items.get(0).unwrap().id, 3);
    assert_eq!(page2.items.get(1).unwrap().id, 4);

    // Page 3: 1 item remaining
    let page3 = f.client.get_merchants(&4, &2);
    assert_eq!(page3.items.len(), 1);
    assert_eq!(page3.total, 5);
    assert_eq!(page3.next_offset, None);
    assert_eq!(page3.items.get(0).unwrap().id, 5);

    // Offset out of bounds
    let page_empty = f.client.get_merchants(&10, &2);
    assert_eq!(page_empty.items.len(), 0);
    assert_eq!(page_empty.total, 5);
    assert_eq!(page_empty.next_offset, None);

    // Category discovery
    let tech_merchants = f
        .client
        .get_merchants_by_category(&symbol_short!("tech"), &0, &10);
    assert_eq!(tech_merchants.items.len(), 3);
    assert_eq!(tech_merchants.total, 3);
    assert_eq!(tech_merchants.next_offset, None);
    assert_eq!(tech_merchants.items.get(0).unwrap().id, 1);
    assert_eq!(tech_merchants.items.get(1).unwrap().id, 2);
    assert_eq!(tech_merchants.items.get(2).unwrap().id, 3);

    let books_merchants = f
        .client
        .get_merchants_by_category(&symbol_short!("books"), &0, &10);
    assert_eq!(books_merchants.items.len(), 2);
    assert_eq!(books_merchants.total, 2);
    assert_eq!(books_merchants.next_offset, None);
    assert_eq!(books_merchants.items.get(0).unwrap().id, 4);
    assert_eq!(books_merchants.items.get(1).unwrap().id, 5);
}

#[test]
fn test_status_filtered_discovery() {
    let f = TestFixture::setup();
    let owner1 = Address::generate(&f.env);
    let owner2 = Address::generate(&f.env);
    let owner3 = Address::generate(&f.env);
    let owner4 = Address::generate(&f.env);

    // Register 4 merchants
    let id1 = f.client.register_merchant(
        &owner1,
        &RegisterParams {
            name: String::from_str(&f.env, "Store 1"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            required_verifications: 1,
        },
    );

    let id2 = f.client.register_merchant(
        &owner2,
        &RegisterParams {
            name: String::from_str(&f.env, "Store 2"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            required_verifications: 1,
        },
    );

    let id3 = f.client.register_merchant(
        &owner3,
        &RegisterParams {
            name: String::from_str(&f.env, "Store 3"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            required_verifications: 1,
        },
    );

    let id4 = f.client.register_merchant(
        &owner4,
        &RegisterParams {
            name: String::from_str(&f.env, "Store 4"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            required_verifications: 1,
        },
    );

    // All 4 merchants are in Registered status
    let all_merchants = f.client.get_merchants(&0, &10);
    assert_eq!(all_merchants.items.len(), 4);
    assert_eq!(all_merchants.total, 4);

    // Suspend merchant 2
    f.client.suspend_merchant(&f.admin, &id2);

    // Close merchant 3
    f.client
        .close_merchant(&f.admin, &id3, &symbol_short!("test"));

    // Get merchants by Registered status (should be id1 and id4)
    let registered = f
        .client
        .get_merchants_by_status(&MerchantStatus::Registered, &0, &10);
    f.client.close_merchant(&f.admin, &id3, &symbol_short!("test"));
    let registered = f.client.get_merchants_by_status(&MerchantStatus::Registered, &0, &10);
    assert_eq!(registered.items.len(), 2);
    assert_eq!(registered.total, 2);
    assert_eq!(registered.next_offset, None);
    assert_eq!(registered.items.get(0).unwrap().id, id1);
    assert_eq!(
        registered.items.get(0).unwrap().status,
        MerchantStatus::Registered
    );
    assert_eq!(registered.items.get(1).unwrap().id, id4);
    assert_eq!(
        registered.items.get(1).unwrap().status,
        MerchantStatus::Registered
    );

    // Get merchants by Suspended status (should be id2)
    let suspended = f
        .client
        .get_merchants_by_status(&MerchantStatus::Suspended, &0, &10);
    assert_eq!(suspended.items.len(), 1);
    assert_eq!(suspended.total, 1);
    assert_eq!(suspended.items.get(0).unwrap().id, id2);
    assert_eq!(
        suspended.items.get(0).unwrap().status,
        MerchantStatus::Suspended
    );

    // Get merchants by Closed status (should be id3)
    let closed = f
        .client
        .get_merchants_by_status(&MerchantStatus::Closed, &0, &10);
    assert_eq!(registered.items.get(0).unwrap().status, MerchantStatus::Registered);
    assert_eq!(registered.items.get(1).unwrap().status, MerchantStatus::Registered);
    let suspended = f.client.get_merchants_by_status(&MerchantStatus::Suspended, &0, &10);
    assert_eq!(suspended.items.get(0).unwrap().status, MerchantStatus::Suspended);
    let closed = f.client.get_merchants_by_status(&MerchantStatus::Closed, &0, &10);
    assert_eq!(closed.items.len(), 1);
    assert_eq!(closed.total, 1);
    assert_eq!(closed.items.get(0).unwrap().id, id3);
    assert_eq!(closed.items.get(0).unwrap().status, MerchantStatus::Closed);

    // Get merchants by Verified status (should be empty)
    let verified = f
        .client
        .get_merchants_by_status(&MerchantStatus::Verified, &0, &10);
    let verified = f.client.get_merchants_by_status(&MerchantStatus::Verified, &0, &10);
    assert_eq!(verified.items.len(), 0);
    assert_eq!(verified.total, 0);
    assert_eq!(verified.next_offset, None);
}

#[test]
fn test_merchant_state_ttl_survives_repeated_reads() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);

    let id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "TTL Survival Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "img.png"),
            metadata: None,
            required_verifications: 1,
        },
    );

    let env = &f.env;
    let contract_id = f._contract_id.clone();

    let before: [u32; 6] = env.as_contract(&contract_id, || {
        let keys = [
            crate::DataKey::Merchant(id),
            crate::DataKey::MerchantName(String::from_str(env, "TTL Survival Store")),
            crate::DataKey::VerifiedCount(id),
            crate::DataKey::RequiredVerifications(id),
            crate::DataKey::MerchantVerifierList(id),
            crate::DataKey::LastMetadataUpdate(id),
        ];
        let mut ttls = [0u32; 6];
        for (i, key) in keys.iter().enumerate() {
            ttls[i] = env.storage().persistent().get_ttl(key);
        }
        ttls
    });

    // Repeated reads must refresh every merchant-linked key, not just
    // Merchant and MerchantName.
    let _ = f.client.get_merchant(&id);
    env.ledger().with_mutator(|li| li.sequence_number += 1);
    let _ = f.client.get_merchant(&id);

    let after: [u32; 6] = env.as_contract(&contract_id, || {
        let keys = [
            crate::DataKey::Merchant(id),
            crate::DataKey::MerchantName(String::from_str(env, "TTL Survival Store")),
            crate::DataKey::VerifiedCount(id),
            crate::DataKey::RequiredVerifications(id),
            crate::DataKey::MerchantVerifierList(id),
            crate::DataKey::LastMetadataUpdate(id),
        ];
        let mut ttls = [0u32; 6];
        for (i, key) in keys.iter().enumerate() {
            ttls[i] = env.storage().persistent().get_ttl(key);
        }
        ttls
    });

    for (index, (old, new)) in before.iter().zip(after.iter()).enumerate() {
        assert!(
            *new > *old,
            "merchant-linked key {} was not TTL-bumped by repeated reads",
            index
        );
    }
}

#[test]
fn test_status_filtered_discovery_by_category() {
    let f = TestFixture::setup();
    let owner1 = Address::generate(&f.env);
    let owner2 = Address::generate(&f.env);
    let owner3 = Address::generate(&f.env);
    let owner4 = Address::generate(&f.env);

    // Register 4 merchants in tech category
    let id1 = f.client.register_merchant(
        &owner1,
        &RegisterParams {
            name: String::from_str(&f.env, "Tech Store 1"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            required_verifications: 1,
        },
    );

    let id2 = f.client.register_merchant(
        &owner2,
        &RegisterParams {
            name: String::from_str(&f.env, "Tech Store 2"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            required_verifications: 1,
        },
    );

    let id3 = f.client.register_merchant(
        &owner3,
        &RegisterParams {
            name: String::from_str(&f.env, "Tech Store 3"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            required_verifications: 1,
        },
    );

    let id4 = f.client.register_merchant(
        &owner4,
        &RegisterParams {
            name: String::from_str(&f.env, "Tech Store 4"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            required_verifications: 1,
        },
    );

    // Suspend id2, Close id3
    f.client.suspend_merchant(&f.admin, &id2);
    f.client
        .close_merchant(&f.admin, &id3, &symbol_short!("test"));
    f.client.close_merchant(&f.admin, &id3, &symbol_short!("test"));

    // Get tech merchants by Registered status (should be id1 and id4)
    let registered = f.client.get_merchants_by_category_status(
        &symbol_short!("tech"),
        &MerchantStatus::Registered,
        &0,
        &10,
    );
    assert_eq!(registered.items.len(), 2);
    assert_eq!(registered.total, 2);
    assert_eq!(registered.items.get(0).unwrap().id, id1);
    assert_eq!(registered.items.get(1).unwrap().id, id4);

    // Get tech merchants by Suspended status (should be id2)
    let suspended = f.client.get_merchants_by_category_status(
        &symbol_short!("tech"),
        &MerchantStatus::Suspended,
        &0,
        &10,
    );
    assert_eq!(suspended.items.len(), 1);
    assert_eq!(suspended.total, 1);
    assert_eq!(suspended.items.get(0).unwrap().id, id2);

    // Get tech merchants by Closed status (should be id3)
    let closed = f.client.get_merchants_by_category_status(
        &symbol_short!("tech"),
        &MerchantStatus::Closed,
        &0,
        &10,
    );
    assert_eq!(closed.items.len(), 1);
    assert_eq!(closed.total, 1);
    assert_eq!(closed.items.get(0).unwrap().id, id3);

    // Get tech merchants by Verified status (should be empty)
    let verified = f.client.get_merchants_by_category_status(
        &symbol_short!("tech"),
        &MerchantStatus::Verified,
        &0,
        &10,
    );
    assert_eq!(verified.items.len(), 0);
    assert_eq!(verified.total, 0);
}

#[test]
fn test_discovery_page_cursor_fields() {
    let f = TestFixture::setup();

    for i in 1..=10 {
        let owner = Address::generate(&f.env);
        let mut name_bytes = [0u8; 8];
        name_bytes[0] = b'S';
        name_bytes[1] = b't';
        name_bytes[2] = b'o';
        name_bytes[3] = b'r';
        name_bytes[4] = b'e';
        name_bytes[5] = b'0' + (i % 10) as u8;
        let name = String::from_str(&f.env, core::str::from_utf8(&name_bytes[..6]).unwrap());

        f.client.register_merchant(
            &owner,
            &RegisterParams {
                name,
                description: String::from_str(&f.env, "Desc"),
                category: symbol_short!("tech"),
                image_url: String::from_str(&f.env, "url"),
                metadata: None,
                required_verifications: 1,
            },
        );
    }

    // Test cursor pagination with limit 3
    let page1 = f.client.get_merchants(&0, &3);
    assert_eq!(page1.total, 10);
    assert_eq!(page1.items.len(), 3);
    assert_eq!(
        page1.next_offset,
        Some(3),
        "First page should have next_offset = 3"
    );
    assert_eq!(page1.next_offset, Some(3), "First page should have next_offset = 3");

    let page2 = f.client.get_merchants(&page1.next_offset.unwrap(), &3);
    assert_eq!(page2.total, 10);
    assert_eq!(page2.items.len(), 3);
    assert_eq!(
        page2.next_offset,
        Some(6),
        "Second page should have next_offset = 6"
    );
    assert_eq!(page2.next_offset, Some(6), "Second page should have next_offset = 6");

    let page3 = f.client.get_merchants(&page2.next_offset.unwrap(), &3);
    assert_eq!(page3.total, 10);
    assert_eq!(page3.items.len(), 3);
    assert_eq!(
        page3.next_offset,
        Some(9),
        "Third page should have next_offset = 9"
    );
    assert_eq!(page3.next_offset, Some(9), "Third page should have next_offset = 9");

    let page4 = f.client.get_merchants(&page3.next_offset.unwrap(), &3);
    assert_eq!(page4.total, 10);
    assert_eq!(page4.items.len(), 1, "Last page should have 1 item");
    assert_eq!(
        page4.next_offset, None,
        "Last page should have None for next_offset"
    );
    assert_eq!(page4.next_offset, None, "Last page should have None for next_offset");
}

#[test]
fn test_two_step_admin_transfer() {
    let f = TestFixture::setup();
    let new_admin = Address::generate(&f.env);
    let stranger = Address::generate(&f.env);

    // Non-admin cannot propose
    let prop_err = f.client.try_propose_admin(&stranger, &new_admin);
    assert_eq!(
        prop_err.unwrap_err().unwrap(),
        MarketplaceError::Unauthorized
    );

    // Accept with no proposal -> distinct NoPendingAdmin error (issue #113)
    let no_proposal_acc = f.client.try_accept_admin(&new_admin);
    assert_eq!(
        no_proposal_acc.unwrap_err().unwrap(),
        MarketplaceError::NoPendingAdmin
    );

    // Current admin proposes new admin -> returns Ok(true)
    let proposed = f.client.propose_admin(&f.admin, &new_admin);
    assert!(proposed);

    // Proposal publishes AdminProposedEvent with current + new admin
    let proposed = find_event::<AdminProposedEvent>(&f, &symbol_short!("adm_prop")).unwrap();
    assert_eq!(proposed.current_admin, f.admin);
    assert_eq!(proposed.new_admin, new_admin);

    // Current admin proposes new admin
    f.client.propose_admin(&f.admin, &new_admin);
    // Stranger (wrong caller, proposal exists) -> Unauthorized
    let stranger_acc = f.client.try_accept_admin(&stranger);
    assert_eq!(
        stranger_acc.unwrap_err().unwrap(),
        MarketplaceError::Unauthorized
    );

    // Unauthorized acceptance must not emit AdminAcceptedEvent
    assert!(find_event::<AdminAcceptedEvent>(&f, &symbol_short!("adm_acc")).is_none());

    // New admin accepts
    f.client.accept_admin(&new_admin);

    // Acceptance publishes AdminAcceptedEvent with previous + new admin
    let accepted = find_event::<AdminAcceptedEvent>(&f, &symbol_short!("adm_acc")).unwrap();
    assert_eq!(accepted.previous_admin, f.admin);
    assert_eq!(accepted.new_admin, new_admin);

    assert_eq!(f.client.get_admin(), new_admin);

    // After acceptance the pending admin is cleared -> NoPendingAdmin again
    let cleared_acc = f.client.try_accept_admin(&new_admin);
    assert_eq!(
        cleared_acc.unwrap_err().unwrap(),
        MarketplaceError::NoPendingAdmin
    );
}

/// Find the most recently published contract event whose event-name topic
/// (the second topic, e.g. "adm_acc") matches `name`, and deserialize its
/// payload as `T`.
fn find_event<T>(f: &TestFixture<'_>, name: &Symbol) -> Option<T>
where
    T: TryFromVal<Env, Val>,
{
    f.env
        .events()
        .all()
        .iter()
        .rev()
        .find_map(|(_id, topics, data)| {
            let topic_val: Val = topics.get(1).unwrap_or_else(|| ().into_val(&f.env));
            let topic = Symbol::try_from_val(&f.env, &topic_val).ok()?;
            if &topic == name {
                T::try_from_val(&f.env, &data).ok()
            } else {
                None
            }
        })
}

#[test]
fn test_propose_admin_self_proposal_is_noop() {
    let f = TestFixture::setup();

    // Self-proposal where new_admin == current_admin should return Ok(false)
    let res = f.client.propose_admin(&f.admin, &f.admin);
    assert_eq!(res, false);

    // Self-proposal must not store pending admin -> accepting still returns NoPendingAdmin
    let acc_err = f.client.try_accept_admin(&f.admin);
    assert_eq!(
        acc_err.unwrap_err().unwrap(),
        MarketplaceError::NoPendingAdmin
    );

    // Verify no AdminProposedEvent was published
    let events = f.env.events().all();
    let prop_events_count = events
        .iter()
        .filter(|(_, topics, _)| {
            let t: soroban_sdk::Vec<soroban_sdk::Val> = topics.clone();
            if t.len() < 2 {
                return false;
            }
            let t0: Result<Symbol, _> = t.get(0).unwrap().try_into_val(&f.env);
            let t1: Result<Symbol, _> = t.get(1).unwrap().try_into_val(&f.env);
            t0 == Ok(symbol_short!("mkplc")) && t1 == Ok(symbol_short!("adm_prop"))
        })
        .count();
    assert_eq!(
        prop_events_count, 0,
        "No adm_prop event should be emitted for self-proposal"
    );
}

#[test]
fn test_propose_admin_self_proposal_does_not_overwrite_pending() {
    let f = TestFixture::setup();
    let new_admin = Address::generate(&f.env);

    // 1. Propose genuine new admin -> returns true
    assert_eq!(f.client.propose_admin(&f.admin, &new_admin), true);

    // Verify AdminProposedEvent was published for new_admin
    let events_after_first = f.env.events().all();
    let prop_event = events_after_first.iter().find(|(_, topics, _)| {
        let t: soroban_sdk::Vec<soroban_sdk::Val> = topics.clone();
        if t.len() < 2 {
            return false;
        }
        let t0: Result<Symbol, _> = t.get(0).unwrap().try_into_val(&f.env);
        let t1: Result<Symbol, _> = t.get(1).unwrap().try_into_val(&f.env);
        t0 == Ok(symbol_short!("mkplc")) && t1 == Ok(symbol_short!("adm_prop"))
    });
    assert!(prop_event.is_some());
    let (_, _, data) = prop_event.unwrap();
    let parsed: AdminProposedEvent = data.try_into_val(&f.env).unwrap();
    assert_eq!(
        parsed,
        AdminProposedEvent {
            current_admin: f.admin.clone(),
            new_admin: new_admin.clone(),
        }
    );

    // 2. Attempt self-proposal -> returns false and does not overwrite existing pending proposal
    assert_eq!(f.client.propose_admin(&f.admin, &f.admin), false);

    // Current admin cannot accept (pending is new_admin, not f.admin) -> Unauthorized
    let self_acc = f.client.try_accept_admin(&f.admin);
    assert_eq!(
        self_acc.unwrap_err().unwrap(),
        MarketplaceError::Unauthorized
    );

    // Proposed new_admin can still accept the valid pending transfer
    f.client.accept_admin(&new_admin);
    assert_eq!(f.client.get_admin(), new_admin);
}

#[test]
fn test_propose_admin_overwrite_pending_admin() {
    let f = TestFixture::setup();
    let new_admin_1 = Address::generate(&f.env);
    let new_admin_2 = Address::generate(&f.env);

    // 1. Propose first candidate -> returns true
    assert_eq!(f.client.propose_admin(&f.admin, &new_admin_1), true);

    // 2. Overwrite proposal with second candidate -> returns true
    assert_eq!(f.client.propose_admin(&f.admin, &new_admin_2), true);

    // First candidate can no longer accept -> Unauthorized
    let acc_1 = f.client.try_accept_admin(&new_admin_1);
    assert_eq!(
        acc_1.unwrap_err().unwrap(),
        MarketplaceError::Unauthorized
    );

    // Second candidate accepts successfully
    f.client.accept_admin(&new_admin_2);
    assert_eq!(f.client.get_admin(), new_admin_2);
}

#[test]
fn test_reputation_score_injection_with_contract() {
    let f = TestFixture::setup();
    let seller = Address::generate(&f.env);
    let reputation_admin = Address::generate(&f.env);

    let rep_id = f.env.register(
        ReputationContract,
        (
            reputation_admin.clone(),
            ReputationConfig {
                decay_window_seconds: 90 * 24 * 60 * 60,
                min_transactions_threshold: 1,
                dispute_penalty_bps: 500,
                freeze_threshold_flags: 3,
            },
        ),
    );
    let rep_client = ReputationContractClient::new(&f.env, &rep_id);

    // Record a transaction for seller in reputation contract
    rep_client.record_transaction(
        &reputation_admin,
        &1u64,
        &seller,
        &Address::generate(&f.env),
        &500i128,
        &TransactionOutcome::Released,
    );

    let id = f.client.register_merchant(
        &seller,
        &RegisterParams {
            name: String::from_str(&f.env, "Reputable Seller"),
            description: String::from_str(&f.env, "Has reputation"),
            category: symbol_short!("services"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            required_verifications: 1,
        },
    );

    // Before setting reputation contract, score is None
    let view_before = f.client.get_merchant_view(&id);
    assert_eq!(view_before.reputation_score, None);

    // Non-admin cannot set reputation contract (CWE-345 protection)
    let unauth_err = f
        .client
        .try_set_merchant_reputation(&seller, &id, &Some(rep_id.clone()));
    assert_eq!(
        unauth_err.unwrap_err().unwrap(),
        MarketplaceError::Unauthorized
    );

    // Admin sets reputation contract for merchant
    f.client
        .set_merchant_reputation(&f.admin, &id, &Some(rep_id.clone()));
    let view_after = f.client.get_merchant_view(&id);
    let expected_score = rep_client.get_reputation(&seller).score;
    let injected = view_after
        .reputation_score
        .expect("score must be read from the reputation contract");
    assert_eq!(injected, expected_score);

    // Also test global reputation contract fallback
    f.client.set_merchant_reputation(&f.admin, &id, &None);
    f.client.set_reputation_contract(&f.admin, &rep_id);
    let view_fallback = f.client.get_merchant_view(&id);
    let fallback_injected = view_fallback
        .reputation_score
        .expect("fallback score must be read from global reputation contract");
    assert_eq!(fallback_injected, expected_score);
}

#[test]
fn test_reputation_resolution_states() {
    let f = TestFixture::setup();
    let seller = Address::generate(&f.env);

    let id = f.client.register_merchant(
        &seller,
        &RegisterParams {
            name: String::from_str(&f.env, "Resolution Tester"),
            description: String::from_str(&f.env, "Tests resolution"),
            category: symbol_short!("test"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            required_verifications: 1,
        },
    );

    // State 1: NotConfigured
    let view_det_not_conf = f.client.get_merchant_view_detailed(&id);
    assert_eq!(view_det_not_conf.reputation, crate::ReputationResolution::NotConfigured);
    assert_eq!(view_det_not_conf.view.reputation_score, None);

    // Setup working reputation contract
    let reputation_admin = Address::generate(&f.env);
    let rep_id = f.env.register(
        ReputationContract,
        (
            reputation_admin.clone(),
            ReputationConfig {
                decay_window_seconds: 90 * 24 * 60 * 60,
                min_transactions_threshold: 1,
                dispute_penalty_bps: 500,
                freeze_threshold_flags: 3,
            },
        ),
    );
    let rep_client = ReputationContractClient::new(&f.env, &rep_id);
    rep_client.record_transaction(
        &reputation_admin,
        &1u64,
        &seller,
        &Address::generate(&f.env),
        &500i128,
        &TransactionOutcome::Released,
    );

    // State 2: Available
    f.client.set_merchant_reputation(&f.admin, &id, &Some(rep_id.clone()));
    let view_det_avail = f.client.get_merchant_view_detailed(&id);
    let rep_score = rep_client.get_reputation(&seller);
    assert_eq!(
        view_det_avail.reputation,
        crate::ReputationResolution::Available(rep_score.score, rep_score.last_updated)
    );
    assert_eq!(view_det_avail.view.reputation_score, Some(rep_score.score));
    
    // Ensure get_merchant_view still works
    let view_avail = f.client.get_merchant_view(&id);
    assert_eq!(view_avail.reputation_score, Some(rep_score.score));

    // State 3: CallFailed
    // Set an invalid contract address
    let bogus_addr = Address::generate(&f.env);
    f.client.set_merchant_reputation(&f.admin, &id, &Some(bogus_addr));
    let view_det_failed = f.client.get_merchant_view_detailed(&id);
    assert_eq!(view_det_failed.reputation, crate::ReputationResolution::CallFailed);
    assert_eq!(view_det_failed.view.reputation_score, None);
    
    // Ensure get_merchant_view still falls back to None on fail
    let view_failed = f.client.get_merchant_view(&id);
    assert_eq!(view_failed.reputation_score, None);
}

/// Acceptance flight test:
/// Registers a merchant, verifies with 2 distinct verifiers, and reads it back.
#[test]
fn test_flight_merchant_lifecycle_and_discovery() {
    let f = TestFixture::setup();

    let merchant_addr = Address::generate(&f.env);
    let verifier1_addr = Address::generate(&f.env);
    let verifier2_addr = Address::generate(&f.env);

    // Admin registers two verifiers
    f.client.add_verifier(
        &f.admin,
        &Verifier {
            address: verifier1_addr.clone(),
            label: symbol_short!("kyc"),
            registered_at: f.env.ledger().timestamp(),
        },
    );
    f.client.add_verifier(
        &f.admin,
        &Verifier {
            address: verifier2_addr.clone(),
            label: symbol_short!("auditor"),
            registered_at: f.env.ledger().timestamp(),
        },
    );

    // Merchant registers requiring 2 verifications
    let merchant_id = f.client.register_merchant(
        &merchant_addr,
        &RegisterParams {
            name: String::from_str(&f.env, "Stellar Artisans"),
            description: String::from_str(&f.env, "Handmade Stellar goods"),
            category: symbol_short!("crafts"),
            image_url: String::from_str(&f.env, "https://example.com/artisan.png"),
            metadata: Some(String::from_str(&f.env, "ipfs://bafybeicraft")),
            required_verifications: 2,
        },
    );
    assert_eq!(merchant_id, 1);

    // Verify view shows not yet verified
    let view_initial = f.client.get_merchant_view(&merchant_id);
    assert!(!view_initial.verified);
    assert_eq!(view_initial.status, MerchantStatus::Registered);

    // Verifier 1 verifies
    f.client.verify_merchant(&merchant_id, &verifier1_addr);
    let view_step1 = f.client.get_merchant_view(&merchant_id);
    assert!(!view_step1.verified);

    // Verifier 2 verifies -> multi-sig threshold reached
    f.client.verify_merchant(&merchant_id, &verifier2_addr);
    let view_step2 = f.client.get_merchant_view(&merchant_id);
    assert!(view_step2.verified);
    assert_eq!(view_step2.status, MerchantStatus::Verified);

    // Set commission
    f.client
        .set_merchant_commission(&merchant_id, &merchant_addr, &350); // 3.5%
    assert_eq!(f.client.get_commission(&merchant_id), 350);

    // Read back through discovery
    let discovery = f
        .client
        .get_merchants_by_category(&symbol_short!("crafts"), &0, &10);
    assert_eq!(discovery.items.len(), 1);
    let item = discovery.items.get(0).unwrap();
    assert_eq!(item.id, merchant_id);
    assert_eq!(item.name, String::from_str(&f.env, "Stellar Artisans"));
    assert_eq!(item.category, symbol_short!("crafts"));
    assert_eq!(item.commission_rate_bps, 350);
    assert!(item.verified);
    assert_eq!(item.status, MerchantStatus::Verified);
}

#[test]
fn test_version_matches_cargo_toml() {
    let env = Env::default();
    env.mock_all_auths();
    
    let admin = Address::generate(&env);
    let contract_id = env.register(crate::MarketplaceContract, (admin,));
    let client = crate::MarketplaceContractClient::new(&env, &contract_id);
    
    let v = client.version();
    let expected = env!("CARGO_PKG_VERSION").replace('.', "_");
    assert_eq!(v.semver, soroban_sdk::Symbol::new(&env, &expected));
fn test_cursor_discovery_pagination() {
    let f = TestFixture::setup();

    for i in 1..=7 {
        let owner = Address::generate(&f.env);
        f.client.register_merchant(
            &owner,
            &RegisterParams {
                name: store_name(&f.env, i),
                description: String::from_str(&f.env, "Desc"),
                category: symbol_short!("tech"),
                image_url: String::from_str(&f.env, "url"),
                metadata: None,
                required_verifications: 1,
            },
        );
    }
    // Page 1: starts at the beginning (after_id = 0)
    let page1 = f.client.get_merchants_cursor(&MerchantCursor {
        after_id: 0,
        status: MerchantStatus::All,
        limit: 3,
    });
    assert_eq!(page1.items.len(), 3);
    assert_eq!(page1.next_cursor, Some(3));
    assert_eq!(page1.items.get(0).unwrap().id, 1);
    assert_eq!(page1.items.get(1).unwrap().id, 2);
    assert_eq!(page1.items.get(2).unwrap().id, 3);
    // Page 2: resumes after id 3
    let page2 = f.client.get_merchants_cursor(&MerchantCursor {
        after_id: page1.next_cursor.unwrap(),
    assert_eq!(page2.items.len(), 3);
    assert_eq!(page2.next_cursor, Some(6));
    assert_eq!(page2.items.get(0).unwrap().id, 4);
    assert_eq!(page2.items.get(2).unwrap().id, 6);
    // Page 3: last item, no further cursor
    let page3 = f.client.get_merchants_cursor(&MerchantCursor {
        after_id: page2.next_cursor.unwrap(),
    assert_eq!(page3.items.len(), 1);
    assert_eq!(page3.next_cursor, None);
    assert_eq!(page3.items.get(0).unwrap().id, 7);
    // Offset entry point exposes the same pivot for callers migrating over.
    let offset_page = f.client.get_merchants(&0, &2);
    assert_eq!(offset_page.next_cursor, Some(2));
}
#[test]
fn test_cursor_discovery_by_status() {
    let owner1 = Address::generate(&f.env);
    let owner2 = Address::generate(&f.env);
    let owner3 = Address::generate(&f.env);
    let owner4 = Address::generate(&f.env);
    let id1 = f.client.register_merchant(
        &owner1,
        &RegisterParams {
            name: String::from_str(&f.env, "Cursor Store 1"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
// --- prune_closed_merchants tests ---
fn test_prune_closed_merchants_removes_from_indices() {
            name: String::from_str(&f.env, "Store 1"),
            description: String::from_str(&f.env, "Desc 1"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "1.png"),
fn test_close_merchant_prunes_indexes_and_archives() {
    f.env.ledger().set_timestamp(5_000);
            name: String::from_str(&f.env, "Pruned Store"),
// ---------------------------------------------------------------------------
// Category re-index tests
/// A category change moves the merchant from the old CategoryIndex Vec to the
/// new one, and the old Vec shrinks (filter-rebuild removes the entry).
fn test_update_merchant_profile_category_reindex() {
    let owner = Address::generate(&f.env);
    // Register merchant in "tech"
    let id = f.client.register_merchant(
        &owner,
            name: String::from_str(&f.env, "Tech Shop"),
            description: String::from_str(&f.env, "Sells gadgets"),
            image_url: String::from_str(&f.env, "tech.png"),
            metadata: None,
            required_verifications: 1,
        },
    );
    // Confirm initial category index
    let tech_before = f
        .client
        .get_merchants_by_category(&symbol_short!("tech"), &0, &10);
    assert_eq!(tech_before.total, 1);
    assert_eq!(tech_before.items.get(0).unwrap().id, id);
    let books_before = f
        .get_merchants_by_category(&symbol_short!("books"), &0, &10);
    assert_eq!(books_before.total, 0);
    // Owner re-categorises from "tech" to "books"
    f.client.update_merchant_profile(
        &id,
        &String::from_str(&f.env, "Tech Shop"),
        &String::from_str(&f.env, "Sells gadgets"),
        &String::from_str(&f.env, "tech.png"),
        &Some(symbol_short!("books")),
    // Merchant record must reflect new category
    let merchant = f.client.get_merchant(&id);
    assert_eq!(merchant.category, symbol_short!("books"));
    // Old index must no longer contain the merchant
    let tech_after = f
    assert_eq!(tech_after.total, 0, "tech index must be empty after move");
    // New index must now contain the merchant
    let books_after = f
    assert_eq!(books_after.total, 1);
    assert_eq!(books_after.items.get(0).unwrap().id, id);
/// Passing None for new_category must leave the category and both indexes
/// completely unchanged.
fn test_update_merchant_profile_no_category_change_when_none() {
            name: String::from_str(&f.env, "Stable Shop"),
            category: symbol_short!("food"),
            image_url: String::from_str(&f.env, "food.png"),
        &String::from_str(&f.env, "Stable Shop"),
        &String::from_str(&f.env, "Updated Desc"),
        &String::from_str(&f.env, "food.png"),
        &None,
    assert_eq!(merchant.category, symbol_short!("food"));
    assert_eq!(
        merchant.description,
        String::from_str(&f.env, "Updated Desc")
    let food = f
        .get_merchants_by_category(&symbol_short!("food"), &0, &10);
    assert_eq!(food.total, 1);
    assert_eq!(food.items.get(0).unwrap().id, id);
/// Passing the same category the merchant already has must be a no-op: no
/// event emitted and the CategoryIndex is untouched.
fn test_update_merchant_profile_same_category_is_noop() {
            name: String::from_str(&f.env, "Noop Shop"),
            category: symbol_short!("retail"),
            image_url: String::from_str(&f.env, "url.png"),
    // Pass the same category explicitly — must not duplicate the entry.
        &String::from_str(&f.env, "Noop Shop"),
        &String::from_str(&f.env, "Desc"),
        &String::from_str(&f.env, "url.png"),
        &Some(symbol_short!("retail")),
    let retail = f
        .get_merchants_by_category(&symbol_short!("retail"), &0, &10);
        retail.total, 1,
        "retail index must still have exactly one entry"
    assert_eq!(retail.items.get(0).unwrap().id, id);
    // Category must be unchanged on the merchant record
    assert_eq!(merchant.category, symbol_short!("retail"));
/// Vec shrink: registers 3 merchants in "tech", moves the middle one out.
/// The remaining tech index must contain exactly the two outer merchants in
/// their original relative order.
fn test_category_reindex_shrinks_old_vec_correctly() {
            name: String::from_str(&f.env, "Tech A"),
            image_url: String::from_str(&f.env, "a.png"),
            metadata: None,
            required_verifications: 1,
        },
    );
    let id2 = f.client.register_merchant(
        &owner2,
            name: String::from_str(&f.env, "Cursor Store 2"),
    let id3 = f.client.register_merchant(
        &owner3,
            name: String::from_str(&f.env, "Cursor Store 3"),
    let id4 = f.client.register_merchant(
        &owner4,
            name: String::from_str(&f.env, "Cursor Store 4"),
    f.client.suspend_merchant(&f.admin, &id2);
    f.client
        .close_merchant(&f.admin, &id3, &symbol_short!("test"));
    // Registered -> id1, id4
    let registered = f.client.get_merchants_cursor(&MerchantCursor {
        status: MerchantStatus::Registered,
        limit: 10,
    assert_eq!(registered.items.len(), 2);
    assert_eq!(registered.items.get(0).unwrap().id, id1);
    assert_eq!(registered.items.get(1).unwrap().id, id4);
    assert_eq!(registered.next_cursor, None);
    // Suspended -> id2
    let suspended = f.client.get_merchants_cursor(&MerchantCursor {
        status: MerchantStatus::Suspended,
    assert_eq!(suspended.items.len(), 1);
    assert_eq!(suspended.items.get(0).unwrap().id, id2);
    assert_eq!(
        suspended.items.get(0).unwrap().status,
        MerchantStatus::Suspended
    // Closed -> id3
    let closed = f.client.get_merchants_cursor(&MerchantCursor {
        status: MerchantStatus::Closed,
    assert_eq!(closed.items.len(), 1);
    assert_eq!(closed.items.get(0).unwrap().id, id3);
    assert_eq!(closed.items.get(0).unwrap().status, MerchantStatus::Closed);
    // All -> every merchant
    let all = f.client.get_merchants_cursor(&MerchantCursor {
    assert_eq!(all.items.len(), 4);
fn test_cursor_discovery_by_category() {
    for i in 1..=5 {
        let category = if i <= 3 {
            symbol_short!("tech")
        } else {
            symbol_short!("books")
        };
                category,
    // tech category -> ids 1,2,3
    let page1 = f
        .client
        .get_merchants_by_category_cursor(&symbol_short!("tech"), &0, &2);
    assert_eq!(page1.items.len(), 2);
    assert_eq!(page1.next_cursor, Some(2));
    let page2 = f.client.get_merchants_by_category_cursor(
        &symbol_short!("tech"),
        &page1.next_cursor.unwrap(),
        &2,
    assert_eq!(page2.items.len(), 1);
    assert_eq!(page2.items.get(0).unwrap().id, 3);
    assert_eq!(page2.next_cursor, None);
    // books category -> ids 4,5
    let books = f
        .get_merchants_by_category_cursor(&symbol_short!("books"), &0, &10);
    assert_eq!(books.items.len(), 2);
    assert_eq!(books.items.get(0).unwrap().id, 4);
    assert_eq!(books.items.get(1).unwrap().id, 5);
    assert_eq!(books.next_cursor, None);
    // Unknown category -> empty page
    let unknown = f
        .get_merchants_by_category_cursor(&symbol_short!("nope"), &0, &10);
    assert_eq!(unknown.items.len(), 0);
    assert_eq!(unknown.next_cursor, None);
fn test_cursor_deep_pagination() {
    // 100 merchants so deep pages are well past the start of the registry.
    for i in 1..=100 {
    // A deep page (after_id = 50) must return exactly the next `limit` records.
    let deep = f.client.get_merchants_cursor(&MerchantCursor {
        after_id: 50,
    assert_eq!(deep.items.len(), 10);
    assert_eq!(deep.items.get(0).unwrap().id, 51);
    assert_eq!(deep.items.get(9).unwrap().id, 60);
    assert_eq!(deep.next_cursor, Some(60));
    // Walk the entire registry page by page and verify contiguous, gap-free,
    // duplicate-free coverage of all ids — i.e. deep pagination stays correct.
    let mut after_id = 0u64;
    let mut expected_id = 1u64;
    let mut total = 0u32;
    loop {
        let page = f.client.get_merchants_cursor(&MerchantCursor {
            after_id,
            status: MerchantStatus::All,
            limit: 10,
        });
        assert!(page.items.len() <= 10);
        for item in page.items.iter() {
            assert_eq!(item.id, expected_id);
            expected_id += 1;
            total += 1;
        }
        match page.next_cursor {
            Some(c) => after_id = c,
            None => break,
    assert_eq!(total, 100);
fn test_category_management_admin_auth_and_duplicates() {
    let stranger = Address::generate(&f.env);
    let cat1 = CategoryEntry {
        key: symbol_short!("Crypto"),
        normalized: symbol_short!("crypto"),
        display: String::from_str(&f.env, "Crypto & Digital Assets"),
        added_at: 100,
    };
    // Non-admin cannot add category
    let res_stranger = f.client.try_add_category(&stranger, &cat1);
    assert_eq!(res_stranger, Err(Ok(MarketplaceError::Unauthorized)));
    // Admin can add category
    f.client.add_category(&f.admin, &cat1);
    let categories = f.client.get_categories();
    assert_eq!(categories.len(), 1);
    let entry = categories.get(0).unwrap();
    assert_eq!(entry.key, symbol_short!("Crypto"));
    assert_eq!(entry.normalized, symbol_short!("crypto"));
        entry.display,
        String::from_str(&f.env, "Crypto & Digital Assets")
    assert_eq!(entry.added_at, 100);
    // Duplicate key rejection
    let res_dup_key = f.client.try_add_category(&f.admin, &cat1);
    assert_eq!(res_dup_key, Err(Ok(MarketplaceError::InvalidCategory)));
    // Duplicate normalized case-variant rejection (e.g. "crypto")
    let cat1_variant = CategoryEntry {
        key: symbol_short!("crypto"),
        display: String::from_str(&f.env, "Crypto lowercase"),
        added_at: 101,
    let res_dup_norm = f.client.try_add_category(&f.admin, &cat1_variant);
    assert_eq!(res_dup_norm, Err(Ok(MarketplaceError::InvalidCategory)));
    // Add second category
    let cat2 = CategoryEntry {
        key: symbol_short!("tools"),
        normalized: symbol_short!("tools"),
        display: String::from_str(&f.env, "Tools & Equipment"),
        added_at: 102,
    f.client.add_category(&f.admin, &cat2);
    assert_eq!(f.client.get_categories().len(), 2);
    // Non-admin cannot remove category
    let res_rem_stranger = f
        .try_remove_category(&stranger, &symbol_short!("crypto"));
    assert_eq!(res_rem_stranger, Err(Ok(MarketplaceError::Unauthorized)));
    // Remove non-existent category
    let res_rem_missing = f
        .try_remove_category(&f.admin, &symbol_short!("nonexist"));
    assert_eq!(res_rem_missing, Err(Ok(MarketplaceError::InvalidCategory)));
    // Admin can remove category by key or normalized symbol
    f.client.remove_category(&f.admin, &symbol_short!("Crypto"));
    let remaining = f.client.get_categories();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining.get(0).unwrap().key, symbol_short!("tools"));
fn test_category_allowlist_gating_disabled_and_enabled() {
    let merchant_addr = Address::generate(&f.env);
    // 1. Allowlist is initially empty -> gating disabled, arbitrary category accepted
    let params_custom = RegisterParams {
        name: String::from_str(&f.env, "Free Market Shop"),
        description: String::from_str(&f.env, "Any category allowed"),
        category: symbol_short!("custom"),
        image_url: String::from_str(&f.env, "https://cdn.example.com/logo1.png"),
        metadata: None,
        required_verifications: 1,
    let m1 = f.client.register_merchant(&merchant_addr, &params_custom);
    assert_eq!(m1, 1);
    // 2. Enable allowlist by adding an approved category
    let approved_cat = CategoryEntry {
        key: symbol_short!("retail"),
        normalized: symbol_short!("retail"),
        display: String::from_str(&f.env, "Retail Goods"),
        added_at: 0,
    f.client.add_category(&f.admin, &approved_cat);
    // 3. Registering with unlisted category is now rejected
    let params_rejected = RegisterParams {
        name: String::from_str(&f.env, "Disallowed Shop"),
        description: String::from_str(&f.env, "Category not on allowlist"),
        category: symbol_short!("gaming"),
        image_url: String::from_str(&f.env, "https://cdn.example.com/logo2.png"),
    let res_rejected = f
        .try_register_merchant(&merchant_addr, &params_rejected);
    assert_eq!(res_rejected, Err(Ok(MarketplaceError::InvalidCategory)));
    // 4. Registering with allowlisted category succeeds
    let params_approved = RegisterParams {
        name: String::from_str(&f.env, "Approved Retail Shop"),
        description: String::from_str(&f.env, "Category on allowlist"),
        category: symbol_short!("retail"),
        image_url: String::from_str(&f.env, "https://cdn.example.com/logo3.png"),
    let m2 = f.client.register_merchant(&merchant_addr, &params_approved);
    assert_eq!(m2, 2);
    // 5. Remove all categories -> allowlist gating becomes disabled again
    f.client.remove_category(&f.admin, &symbol_short!("retail"));
    assert_eq!(f.client.get_categories().len(), 0);
    let params_unrestricted = RegisterParams {
        name: String::from_str(&f.env, "Again Free Shop"),
        description: String::from_str(&f.env, "Allowlist empty again"),
        image_url: String::from_str(&f.env, "https://cdn.example.com/logo4.png"),
    let m3 = f
        .register_merchant(&merchant_addr, &params_unrestricted);
    assert_eq!(m3, 3);
fn test_category_normalization_collapsing_case_variants() {
    // Add allowlisted category with mixed case
    let cat = CategoryEntry {
        display: String::from_str(&f.env, "Cryptocurrency"),
    f.client.add_category(&f.admin, &cat);
    // Register with "Crypto" (matching key)
    let p1 = RegisterParams {
        name: String::from_str(&f.env, "Shop 1"),
        description: String::from_str(&f.env, "Desc"),
        category: symbol_short!("Crypto"),
        image_url: String::from_str(&f.env, "https://example.com/1.png"),
    let id1 = f.client.register_merchant(&merchant_addr, &p1);
    // Register with "crypto" (all lowercase)
    let p2 = RegisterParams {
        name: String::from_str(&f.env, "Shop 2"),
        category: symbol_short!("crypto"),
        image_url: String::from_str(&f.env, "https://example.com/2.png"),
    let id2 = f.client.register_merchant(&merchant_addr, &p2);
    // Register with "CRYPTO" (all uppercase)
    let p3 = RegisterParams {
        name: String::from_str(&f.env, "Shop 3"),
        category: soroban_sdk::Symbol::new(&f.env, "CRYPTO"),
        image_url: String::from_str(&f.env, "https://example.com/3.png"),
    let id3 = f.client.register_merchant(&merchant_addr, &p3);
    // All 3 merchants store the normalized category symbol
        f.client.get_merchant(&id1).category,
        symbol_short!("crypto")
        f.client.get_merchant(&id2).category,
        f.client.get_merchant(&id3).category,
    // Querying with any case variant discovers all 3 merchants
    let q_mixed = f
        .get_merchants_by_category(&symbol_short!("Crypto"), &0, &10);
    assert_eq!(q_mixed.items.len(), 3);
    assert_eq!(q_mixed.total, 3);
    let q_lower = f
        .get_merchants_by_category(&symbol_short!("crypto"), &0, &10);
    assert_eq!(q_lower.items.len(), 3);
    let q_upper =
        f.client
            .get_merchants_by_category(&soroban_sdk::Symbol::new(&f.env, "CRYPTO"), &0, &10);
    assert_eq!(q_upper.items.len(), 3);
fn test_category_backward_compatibility_pre_existing_arbitrary() {
    // 1. Merchant registered under arbitrary category before allowlist is configured
    let legacy_params = RegisterParams {
        name: String::from_str(&f.env, "Legacy Antique Store"),
        description: String::from_str(&f.env, "Registered in legacy system"),
        category: symbol_short!("antique"),
        image_url: String::from_str(&f.env, "https://example.com/antique.png"),
    let legacy_id = f.client.register_merchant(&merchant_addr, &legacy_params);
    assert_eq!(legacy_id, 1);
    // 2. Later, admin configures an allowlist that does NOT contain "antique"
    let tech_cat = CategoryEntry {
        key: symbol_short!("tech"),
        normalized: symbol_short!("tech"),
        display: String::from_str(&f.env, "Technology"),
    f.client.add_category(&f.admin, &tech_cat);
    // 3. Verify existing legacy merchant record still loads and functions perfectly
    let merchant = f.client.get_merchant(&legacy_id);
    assert_eq!(merchant.id, 1);
    assert_eq!(merchant.category, symbol_short!("antique"));
    let view = f.client.get_merchant_view(&legacy_id);
    assert_eq!(view.id, 1);
    assert_eq!(view.category, symbol_short!("antique"));
    // 4. Discovery for the legacy category still returns the legacy merchant
    let query = f
        .get_merchants_by_category(&symbol_short!("antique"), &0, &10);
    assert_eq!(query.items.len(), 1);
    assert_eq!(query.items.get(0).unwrap().id, legacy_id);
    // 5. Global discovery still includes the legacy merchant
    let all_merchants = f.client.get_merchants(&0, &10);
    assert_eq!(all_merchants.items.len(), 1);
    assert_eq!(all_merchants.items.get(0).unwrap().id, legacy_id);
fn test_normalize_symbol_direct_boundary_cases() {
    // 1 char (len % 4 = 1, 3 padding bytes)
    let s1 = soroban_sdk::Symbol::new(&env, "A");
        normalize_symbol(&env, &s1),
        soroban_sdk::Symbol::new(&env, "a")
    // 2 chars (len % 4 = 2, 2 padding bytes)
    let s2 = soroban_sdk::Symbol::new(&env, "Ab");
        normalize_symbol(&env, &s2),
        soroban_sdk::Symbol::new(&env, "ab")
    // 3 chars (len % 4 = 3, 1 padding byte)
    let s3 = soroban_sdk::Symbol::new(&env, "AbC");
        normalize_symbol(&env, &s3),
        soroban_sdk::Symbol::new(&env, "abc")
    // 4 chars (len % 4 = 0, 0 padding bytes)
    let s4 = soroban_sdk::Symbol::new(&env, "AbCd");
        normalize_symbol(&env, &s4),
        soroban_sdk::Symbol::new(&env, "abcd")
    // 5 chars (len % 4 = 1, 3 padding bytes)
    let s5 = symbol_short!("Tools");
    assert_eq!(normalize_symbol(&env, &s5), symbol_short!("tools"));
    // 8 chars (len % 4 = 0, 0 padding bytes)
    let s8 = symbol_short!("SoftWare");
    assert_eq!(normalize_symbol(&env, &s8), symbol_short!("software"));
    // 9 chars (max SymbolSmall boundary, len % 4 = 1, 3 padding bytes)
    let s9 = symbol_short!("AbCdEfGhI");
    assert_eq!(normalize_symbol(&env, &s9), symbol_short!("abcdefghi"));
    // 10 chars (min SymbolObject boundary, len % 4 = 2, 2 padding bytes)
    let s10 = soroban_sdk::Symbol::new(&env, "AbCdEfGhIj");
        normalize_symbol(&env, &s10),
        soroban_sdk::Symbol::new(&env, "abcdefghij")
    // 16 chars (len % 4 = 0, 0 padding bytes)
    let s16 = soroban_sdk::Symbol::new(&env, "A_B_C_D_E_F_G_H_");
        normalize_symbol(&env, &s16),
        soroban_sdk::Symbol::new(&env, "a_b_c_d_e_f_g_h_")
    // 31 chars (max - 1, len % 4 = 3, 1 padding byte)
    let s31_raw = "A_1234567890_1234567890_1234567";
    assert_eq!(s31_raw.len(), 31);
    let s31 = soroban_sdk::Symbol::new(&env, s31_raw);
    let s31_expected = soroban_sdk::Symbol::new(&env, "a_1234567890_1234567890_1234567");
    assert_eq!(normalize_symbol(&env, &s31), s31_expected);
    // 32 chars (exact max symbol length in Soroban, len % 4 = 0, 0 padding bytes)
    let s32_raw = "A_1234567890_1234567890_12345678";
    assert_eq!(s32_raw.len(), 32);
    let s32 = soroban_sdk::Symbol::new(&env, s32_raw);
    let s32_expected = soroban_sdk::Symbol::new(&env, "a_1234567890_1234567890_12345678");
    assert_eq!(normalize_symbol(&env, &s32), s32_expected);
    // Underscores and numbers
    let s_special = soroban_sdk::Symbol::new(&env, "_TEST_123_ABC_");
        normalize_symbol(&env, &s_special),
        soroban_sdk::Symbol::new(&env, "_test_123_abc_")
    // Idempotency: normalizing already lowercase symbol returns equal symbol
    assert_eq!(normalize_symbol(&env, &s32_expected), s32_expected);
fn test_category_max_length_32_chars_registration_and_discovery() {
    let max_len_key_str = "Very_Long_Category_Name_32_Chars";
    assert_eq!(max_len_key_str.len(), 32);
    let max_len_norm_str = "very_long_category_name_32_chars";
    assert_eq!(max_len_norm_str.len(), 32);
    let cat_32 = CategoryEntry {
        key: soroban_sdk::Symbol::new(&f.env, max_len_key_str),
        normalized: soroban_sdk::Symbol::new(&f.env, max_len_norm_str),
        display: String::from_str(&f.env, "32-character maximum category name"),
    f.client.add_category(&f.admin, &cat_32);
    // Register with mixed-case 32-char category
    let params = RegisterParams {
        name: String::from_str(&f.env, "Boundary Merchant"),
        description: String::from_str(&f.env, "Testing 32-char category"),
        category: soroban_sdk::Symbol::new(&f.env, max_len_key_str),
        image_url: String::from_str(&f.env, "https://example.com/logo.png"),
    let merchant_id = f.client.register_merchant(&merchant_addr, &params);
    assert_eq!(merchant_id, 1);
    // Verify stored merchant record holds normalized 32-char category without truncation
    let merchant = f.client.get_merchant(&merchant_id);
        merchant.category,
        soroban_sdk::Symbol::new(&f.env, max_len_norm_str)
    // Discover via mixed-case 32-char category query
    let page_mixed = f.client.get_merchants_by_category(
        &soroban_sdk::Symbol::new(&f.env, max_len_key_str),
        &0,
        &10,
    assert_eq!(page_mixed.items.len(), 1);
    assert_eq!(page_mixed.items.get(0).unwrap().id, merchant_id);
    // Discover via lowercase 32-char category query
    let page_lower = f.client.get_merchants_by_category(
        &soroban_sdk::Symbol::new(&f.env, max_len_norm_str),
    assert_eq!(page_lower.items.len(), 1);
    assert_eq!(page_lower.items.get(0).unwrap().id, merchant_id);
fn test_category_backward_compatibility_unnormalized_mixed_case_raw_index() {
    let legacy_cat_raw = symbol_short!("Antique");
    let legacy_id = 99u64;
    let now = f.env.ledger().timestamp();
    // Directly seed persistent storage simulating a pre-migration record
    // created when unnormalized mixed-case symbols were written to storage.
    f.env.as_contract(&f._contract_id, || {
        let legacy_merchant = Merchant {
            id: legacy_id,
            owner: Some(merchant_addr.clone()),
            name: String::from_str(&f.env, "Old Curiosity Shop"),
            description: String::from_str(&f.env, "Vintage goods"),
            category: legacy_cat_raw.clone(),
            image_url: String::from_str(&f.env, "https://example.com/curiosity.png"),
            commission_rate_bps: 0,
            status: MerchantStatus::Registered,
            verified: false,
            created_at: now,
            updated_at: now,
            reputation: None,
        f.env
            .storage()
            .persistent()
            .set(&DataKey::Merchant(legacy_id), &legacy_merchant);
        f.env.storage().persistent().set(
            &DataKey::MerchantName(legacy_merchant.name.clone()),
            &legacy_id,
        let policy = VerificationPolicy {
            required: 1,
            max_verifications: 0,
            .set(&DataKey::VerificationPolicy(legacy_id), &policy);
            .set(&DataKey::VerifiedCount(legacy_id), &0u32);
            &DataKey::MerchantVerifierList(legacy_id),
            &soroban_sdk::Vec::<Address>::new(&f.env),
            .set(&DataKey::LastMetadataUpdate(legacy_id), &now);
        let mut merchant_ids = soroban_sdk::Vec::<u64>::new(&f.env);
        merchant_ids.push_back(legacy_id);
            .set(&DataKey::MerchantIds, &merchant_ids);
        // Index strictly under raw mixed-case symbol "Antique" (pre-normalization)
        let mut cat_ids = soroban_sdk::Vec::<u64>::new(&f.env);
        cat_ids.push_back(legacy_id);
            .set(&DataKey::CategoryIndex(legacy_cat_raw.clone()), &cat_ids);
    // 1. Direct record retrieval works and preserves original raw symbol
    assert_eq!(merchant.id, legacy_id);
    assert_eq!(merchant.category, legacy_cat_raw);
    assert_eq!(view.id, legacy_id);
    assert_eq!(view.category, legacy_cat_raw);
    // 2. Query with raw mixed-case symbol "Antique" engages the fallback branch
    let page_raw = f.client.get_merchants_by_category(&legacy_cat_raw, &0, &10);
    assert_eq!(page_raw.items.len(), 1);
    assert_eq!(page_raw.items.get(0).unwrap().id, legacy_id);
    assert_eq!(page_raw.items.get(0).unwrap().category, legacy_cat_raw);
    // 3. Global discovery includes the legacy merchant
    let all = f.client.get_merchants(&0, &10);
    assert_eq!(all.items.len(), 1);
    assert_eq!(all.items.get(0).unwrap().id, legacy_id);

        &RegisterParams {
            name: String::from_str(&f.env, "Store 2"),
            description: String::from_str(&f.env, "Desc 2"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "2.png"),
            name: String::from_str(&f.env, "Kept Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            name: String::from_str(&f.env, "Tech B"),
            image_url: String::from_str(&f.env, "b.png"),
            metadata: None,
            required_verifications: 1,
        },
    );
            name: String::from_str(&f.env, "Tech C"),
            image_url: String::from_str(&f.env, "c.png"),
            metadata: None,
            required_verifications: 1,
        },
    );
    // Both are present in discovery
    let disc_before = f.client.get_merchants(&0, &10);
    assert_eq!(disc_before.total, 2);
    // Close Store 1
        .close_merchant(&f.admin, &id1, &symbol_short!("tos"));
    // Prune closed merchant
    let mut to_prune = soroban_sdk::Vec::new(&f.env);
    to_prune.push_back(id1);
    to_prune.push_back(id2); // id2 is not closed, so it won't be pruned
    let pruned = f.client.prune_closed_merchants(&f.admin, &to_prune);
    assert_eq!(pruned, 1);
    // Discovery now only contains Store 2
    let disc_after = f.client.get_merchants(&0, &10);
    assert_eq!(disc_after.total, 1);
    assert_eq!(disc_after.items.get(0).unwrap().id, id2);
    // Category discovery also only contains Store 2
    let cat_after = f
        .get_merchants_by_category(&symbol_short!("goods"), &0, &10);
    assert_eq!(cat_after.total, 1);
    assert_eq!(cat_after.items.get(0).unwrap().id, id2);
}
#[test]
fn test_prune_closed_merchants_skips_active_and_nonexistent() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);
    let id = f.client.register_merchant(
        &owner,
            name: String::from_str(&f.env, "Store Active"),
            description: String::from_str(&f.env, "Desc"),
            image_url: String::from_str(&f.env, "logo.png"),
    to_prune.push_back(id);
    to_prune.push_back(999); // nonexistent
    assert_eq!(pruned, 0);
    let disc = f.client.get_merchants(&0, &10);
    assert_eq!(disc.total, 1);
fn test_prune_closed_merchants_rejects_over_cap_and_unauthorized() {
    let mut over_cap = soroban_sdk::Vec::new(&f.env);
    for i in 0..51 {
        over_cap.push_back(i);
    }
    // Over cap rejected
    let res_cap = f.client.try_prune_closed_merchants(&f.admin, &over_cap);
    assert_eq!(res_cap, Err(Ok(MarketplaceError::InvalidParam)));
    // Unauthorized rejected
    let mut valid = soroban_sdk::Vec::new(&f.env);
    valid.push_back(1);
    let res_auth = f.client.try_prune_closed_merchants(&stranger, &valid);
    assert_eq!(res_auth, Err(Ok(MarketplaceError::Unauthorized)));

    let all_before = f.client.get_merchants(&0, &10);
    assert_eq!(all_before.total, 2);
    let tech_before = f.client.get_merchants_by_category(&symbol_short!("tech"), &0, &10);
    assert_eq!(tech_before.total, 2);
    f.env.ledger().set_timestamp(9_000);
    f.client.close_merchant(&f.admin, &id1, &symbol_short!("fraud"));
    let all_after = f.client.get_merchants(&0, &10);
    assert_eq!(all_after.total, 1);
    assert_eq!(all_after.items.get(0).unwrap().id, id2);
    let tech_after = f.client.get_merchants_by_category(&symbol_short!("tech"), &0, &10);
    assert_eq!(tech_after.total, 1);
    assert_eq!(tech_after.items.get(0).unwrap().id, id2);
    let archived = f.client.get_archived_merchant(&id1);
    assert!(archived.is_some());
    let archived = archived.unwrap();
    assert_eq!(archived.id, id1);
    assert_eq!(archived.closed_at, 9_000);
    assert_eq!(archived.last_view.id, id1);
    assert_eq!(archived.last_view.status, MerchantStatus::Closed);
    assert!(f.client.get_archived_merchant(&id2).is_none());
fn test_reregistration_after_close_does_not_reuse_id() {
    f.env.ledger().set_timestamp(1_000);
    let id1 = f.client.register_merchant(
        &RegisterParams {
            name: String::from_str(&f.env, "Old Store"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
    // Move the middle merchant (id2) to "services"
    f.client.update_merchant_profile(
        &id2,
        &owner2,
        &String::from_str(&f.env, "Tech B"),
        &String::from_str(&f.env, "Desc"),
        &String::from_str(&f.env, "b.png"),
        &Some(symbol_short!("services")),
    );
    // tech index: should contain only id1 and id3, in original registration order
    let tech = f
        .client
        .get_merchants_by_category(&symbol_short!("tech"), &0, &10);
    assert_eq!(tech.total, 2, "tech index must shrink to 2 after removal");
    assert_eq!(tech.items.get(0).unwrap().id, id1);
    assert_eq!(tech.items.get(1).unwrap().id, id3);
    // services index: should contain only id2
    let services = f
        .get_merchants_by_category(&symbol_short!("services"), &0, &10);
    assert_eq!(services.total, 1);
    assert_eq!(services.items.get(0).unwrap().id, id2);
    // Merchant records reflect updated categories
    assert_eq!(
        f.client.get_merchant(&id2).category,
        symbol_short!("services")
        f.client.get_merchant(&id1).category,
        symbol_short!("tech")
        f.client.get_merchant(&id3).category,
/// A frozen (suspended) merchant cannot change category.
fn test_category_change_blocked_when_suspended() {
            name: String::from_str(&f.env, "Frozen Shop"),
            image_url: String::from_str(&f.env, "t.png"),
// --- RegisterParams normalization (#102) ---
fn test_register_merchant_trims_whitespace_fields() {
        name: String::from_str(&f.env, "  Padded Store  "),
        description: String::from_str(&f.env, "\tPadded description\n"),
        image_url: String::from_str(&f.env, "  https://cdn.example.com/logo.png  "),
        metadata: Some(String::from_str(&f.env, "  ipfs://Qm123  ")),
    assert_eq!(merchant.name, String::from_str(&f.env, "Padded Store"));
        String::from_str(&f.env, "Padded description")
        merchant.image_url,
        String::from_str(&f.env, "https://cdn.example.com/logo.png")
        merchant.metadata,
        Some(String::from_str(&f.env, "ipfs://Qm123"))
    // The name index must be keyed by the canonical (trimmed) name, not the
    // raw whitespace-padded input.
    assert!(!f
        .is_name_available(&String::from_str(&f.env, "Padded Store")));
fn test_register_merchant_whitespace_only_name_rejected() {
        name: String::from_str(&f.env, "    "),
        image_url: String::from_str(&f.env, ""),
    let err = f.client.try_register_merchant(&merchant_addr, &params);
    assert_eq!(err.unwrap_err().unwrap(), MarketplaceError::InvalidParam);
fn test_register_merchant_rejects_over_length_name() {
    let too_long_name = [b'a'; (MAX_NAME_LEN + 1) as usize];
        name: String::from_bytes(&f.env, &too_long_name),
        image_url: String::from_str(&f.env, "https://example.com/x.png"),
fn test_register_merchant_rejects_over_length_description() {
    let too_long_description = [b'b'; (MAX_DESCRIPTION_LEN + 1) as usize];
        name: String::from_str(&f.env, "Valid Name"),
        description: String::from_bytes(&f.env, &too_long_description),
fn test_register_merchant_rejects_over_length_image_url() {
    let too_long_image_url = [b'c'; (MAX_IMAGE_URL_LEN + 1) as usize];
        image_url: String::from_bytes(&f.env, &too_long_image_url),
fn test_register_merchant_rejects_over_length_metadata() {
    let too_long_metadata = [b'd'; (MAX_METADATA_LEN + 1) as usize];
        metadata: Some(String::from_bytes(&f.env, &too_long_metadata)),
fn test_register_merchant_accepts_exact_cap_length() {
    let exact_name = [b'e'; MAX_NAME_LEN as usize];
    let exact_description = [b'f'; MAX_DESCRIPTION_LEN as usize];
    let exact_image_url = [b'g'; MAX_IMAGE_URL_LEN as usize];
    let exact_metadata = [b'h'; MAX_METADATA_LEN as usize];
        name: String::from_bytes(&f.env, &exact_name),
        description: String::from_bytes(&f.env, &exact_description),
        image_url: String::from_bytes(&f.env, &exact_image_url),
        metadata: Some(String::from_bytes(&f.env, &exact_metadata)),
    assert_eq!(merchant.name.len(), MAX_NAME_LEN);
    assert_eq!(merchant.description.len(), MAX_DESCRIPTION_LEN);
    assert_eq!(merchant.image_url.len(), MAX_IMAGE_URL_LEN);
    assert_eq!(merchant.metadata.map(|m| m.len()), Some(MAX_METADATA_LEN));
fn test_update_merchant_profile_trims_and_bounds() {
            name: String::from_str(&f.env, "Store A"),
            description: String::from_str(&f.env, "Old Desc"),
            image_url: String::from_str(&f.env, "old.png"),
            metadata: None,
            required_verifications: 1,
        },
    );
    f.env.ledger().set_timestamp(2_000);
    f.client.close_merchant(&f.admin, &id1, &symbol_short!("voluntary"));
    let id2 = f.client.register_merchant(
            name: String::from_str(&f.env, "New Store"),
    assert_ne!(id2, id1);
    assert_eq!(id2, 2);
    let all = f.client.get_merchants(&0, &10);
    assert_eq!(all.total, 1);
    assert_eq!(all.items.get(0).unwrap().id, id2);
    let tech = f.client.get_merchants_by_category(&symbol_short!("tech"), &0, &10);
    assert_eq!(tech.total, 1);
    assert_eq!(tech.items.get(0).unwrap().id, id2);
    assert_eq!(archived.unwrap().id, id1);
}

    f.client.suspend_merchant(&f.admin, &id);
    let err = f.client.try_update_merchant_profile(
        &id,
        &owner,
        &String::from_str(&f.env, "Frozen Shop"),
        &String::from_str(&f.env, "Desc"),
        &String::from_str(&f.env, "t.png"),
        &Some(symbol_short!("books")),
    );
    assert_eq!(err.unwrap_err().unwrap(), MarketplaceError::MerchantFrozen);
/// A non-owner cannot change the category.
#[test]
fn test_category_change_unauthorized() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);
    let stranger = Address::generate(&f.env);
    let id = f.client.register_merchant(
        &RegisterParams {
            name: String::from_str(&f.env, "Auth Shop"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "t.png"),
            metadata: None,
            required_verifications: 1,
        },
        &stranger,
        &String::from_str(&f.env, "Auth Shop"),
    assert_eq!(err.unwrap_err().unwrap(), MarketplaceError::Unauthorized);
/// CategoryChange struct can be constructed with expected fields.
fn test_category_change_struct_fields() {
    let _env = Env::default();
    let change = CategoryChange {
        merchant_id: 42,
        from: symbol_short!("tech"),
        to: symbol_short!("books"),
    };
    assert_eq!(change.merchant_id, 42);
    assert_eq!(change.from, symbol_short!("tech"));
    assert_eq!(change.to, symbol_short!("books"));
/// MerchantCategoryChangedEvent struct can be constructed and compared.
fn test_merchant_category_changed_event_struct_fields() {
    let event = MerchantCategoryChangedEvent {
        merchant_id: 7,
        from: symbol_short!("retail"),
        to: symbol_short!("food"),
    assert_eq!(event.merchant_id, 7);
    assert_eq!(event.from, symbol_short!("retail"));
    assert_eq!(event.to, symbol_short!("food"));
// Issue #142: merchant-scoped events carry the merchant id as their third
// topic so indexers can route by merchant without deserializing the body.
fn test_register_merchant_event_carries_merchant_id_topic() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{Symbol, TryIntoVal};
        name: String::from_str(&f.env, "Topic Store"),
        description: String::from_str(&f.env, "Tests topic routing"),
        image_url: String::from_str(&f.env, "https://example.com/topic.png"),
    let mut found = false;
    for event in f.env.events().all().iter() {
        let (_c_id, topics, _value) = event;
        if topics.len() != 3 {
            continue;
        let t0: Symbol = topics.get(0).unwrap().try_into_val(&f.env).unwrap();
        let t1: Symbol = topics.get(1).unwrap().try_into_val(&f.env).unwrap();
        if t0 == symbol_short!("mkplc") && t1 == symbol_short!("reg") {
            let t2: u64 = topics.get(2).unwrap().try_into_val(&f.env).unwrap();
            assert_eq!(t2, merchant_id, "reg topic must carry the merchant id");
            found = true;
    assert!(found, "reg event with merchant_id topic not found");
    // Whitespace on the update path is trimmed the same way as registration.
    f.client.update_merchant_profile(
        &String::from_str(&f.env, "  Store A Updated  "),
        &String::from_str(&f.env, "  New Desc  "),
        &String::from_str(&f.env, "  new.png  "),
    let updated = f.client.get_merchant(&id);
    assert_eq!(updated.name, String::from_str(&f.env, "Store A Updated"));
    assert_eq!(updated.description, String::from_str(&f.env, "New Desc"));
    assert_eq!(updated.image_url, String::from_str(&f.env, "new.png"));
    // An over-length name is rejected with a typed error and does not
    // mutate stored state.
    let too_long_name = [b'z'; (MAX_NAME_LEN + 1) as usize];
        &String::from_bytes(&f.env, &too_long_name),
        &String::from_str(&f.env, "New Desc"),
        &String::from_str(&f.env, "new.png"),
    assert_eq!(err.unwrap_err().unwrap(), MarketplaceError::InvalidParam);
    let unchanged = f.client.get_merchant(&id);
    assert_eq!(unchanged.name, String::from_str(&f.env, "Store A Updated"));
