use crate::{
    normalize_symbol, AdminAcceptedEvent, AdminProposedEvent, CategoryChange, CategoryEntry,
    DataKey, MarketplaceContract, MarketplaceContractClient, MarketplaceError, Merchant,
    MerchantCategoryChangedEvent, MerchantCursor, MerchantOperationalView,
    MerchantRegisteredEvent, MerchantStats, MerchantStatus, MerchantValidationError,
    RegisterParams, VerificationPolicy, Verifier, MAX_DESCRIPTION_LEN, MAX_IMAGE_URL_LEN,
    MAX_METADATA_LEN, MAX_NAME_LEN,
    CommissionTier, MerchantVolumeRecord,
    ReputationCursor, RankedMerchantPage,
    TreasurySplitConfig, RESERVE_LOCK_LEDGERS,
    AdminAcceptedEvent, AdminProposedEvent, CategoryChange, CategoryEntry, DataKey,
    MarketplaceContract, MarketplaceContractClient, MarketplaceError, Merchant,
    MerchantCategoryChangedEvent, MerchantCursor, MerchantOperationalView,
    MerchantRegisteredEvent, MerchantStats, MerchantStatus, MerchantValidationError,
    RegisterParams, VerificationPolicy, Verifier,
    MAX_DESCRIPTION_LEN, MAX_IMAGE_URL_LEN, MAX_METADATA_LEN, MAX_NAME_LEN,
    normalize_symbol,
    MerchantCategoryChangedEvent, MerchantCursor, MerchantOperationalView, MerchantRegisteredEvent,
    MerchantStats, MerchantStatus, MerchantValidationError, RegisterParams, VerificationPolicy,
    Verifier, VerifierPage, MAX_DESCRIPTION_LEN, MAX_IMAGE_URL_LEN, MAX_METADATA_LEN, MAX_NAME_LEN,
    MAX_VERIFIERS,
};
use delego_reputation::{
    ReputationConfig, ReputationContract, ReputationContractClient, TransactionOutcome,
};
use soroban_sdk::{
    symbol_short,
    testutils::{storage::Persistent as _, Address as _, Events as _, Ledger as _},
    Address, Env, IntoVal, String, Symbol, TryFromVal, TryIntoVal, Val,
};

const MAX_DISCOVERY_CPU_INSTRUCTIONS: u64 = 2_000_000;
const MAX_DISCOVERY_MEMORY_BYTES: u64 = 2_000_000;

fn assert_discovery_cost_within_thresholds(env: &Env) {
    let budget = env.cost_estimate().budget();
    assert!(budget.cpu_instruction_cost() <= MAX_DISCOVERY_CPU_INSTRUCTIONS);
    assert!(budget.memory_bytes_cost() <= MAX_DISCOVERY_MEMORY_BYTES);
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
    assert_eq!(ver.semver, symbol_short!("0_0_1"));
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
        metadata_uri: None,
        required_verifications: 1,
    };

    let merchant_id =
        crate::testutil::register_merchant_with_deposit(&f.env, &f.client, &merchant_addr, &params);
    assert_eq!(merchant_id, 1);

    let merchant = f.client.get_merchant(&merchant_id);
    assert_eq!(merchant.id, 1);
    assert_eq!(merchant.owner, Some(merchant_addr.clone()));
    assert_eq!(merchant.name, String::from_str(&f.env, "Acme Store"));
    assert_eq!(
        merchant.description,
        String::from_str(&f.env, "High quality tools")
    );
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
    let id1 = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner1,
        &RegisterParams {
            name: String::from_str(&f.env, "Registered Unverified"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            metadata_uri: None,
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
    let id2 = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner2,
        &RegisterParams {
            name: String::from_str(&f.env, "Verified Active"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            metadata_uri: None,
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
    let id3 = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner3,
        &RegisterParams {
            name: String::from_str(&f.env, "Suspended Unverified"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            metadata_uri: None,
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
    let id4 = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner4,
        &RegisterParams {
            name: String::from_str(&f.env, "Closed Verified"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );
    f.client.verify_merchant(&id4, &verifier);
    f.client
        .close_merchant(&f.admin, &id4, &symbol_short!("miscond"));
    let view = f.client.get_merchant_operational_view(&id4);
    assert_eq!(view.status, MerchantStatus::Closed);
    assert!(view.verified);
    assert!(!view.effective);

    // Closed + unverified
    let owner5 = Address::generate(&f.env);
    let id5 = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner5,
        &RegisterParams {
            name: String::from_str(&f.env, "Closed Unverified"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );
    f.client
        .close_merchant(&f.admin, &id5, &symbol_short!("miscond"));
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
        metadata_uri: None,
        required_verifications: 1,
    };

    let merchant_id =
        crate::testutil::register_merchant_with_deposit(&f.env, &f.client, &merchant_addr, &params);
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
        metadata_uri: None,
        required_verifications: 1,
    };

    let id =
        crate::testutil::register_merchant_with_deposit(&f.env, &f.client, &merchant1, &params1);
    assert_eq!(id, 1);

    // Duplicate name
    let params2 = RegisterParams {
        name: String::from_str(&f.env, "Store Unique"),
        description: String::from_str(&f.env, "Second store"),
        category: symbol_short!("retail"),
        image_url: String::from_str(&f.env, "https://cdn.example.com/2.png"),
        metadata: None,
        metadata_uri: None,
        required_verifications: 1,
    };

    let err = crate::testutil::try_register_merchant_with_deposit(
        &f.env, &f.client, &merchant2, &params2,
    );
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
        metadata_uri: None,
        required_verifications: 1,
    };

    let err_empty = crate::testutil::try_register_merchant_with_deposit(
        &f.env,
        &f.client,
        &merchant2,
        &params_empty,
    );
    assert_eq!(
        err_empty.unwrap_err().unwrap(),
        MarketplaceError::InvalidParam
    );

    // Whitespace-only name
    let params_ws_name = RegisterParams {
        name: String::from_str(&f.env, "   "),
        description: String::from_str(&f.env, "Desc"),
        category: symbol_short!("retail"),
        image_url: String::from_str(&f.env, ""),
        metadata: None,
        metadata_uri: None,
        required_verifications: 1,
    };

    let err_ws_name = crate::testutil::try_register_merchant_with_deposit(
        &f.env,
        &f.client,
        &merchant2,
        &params_ws_name,
    );
    assert_eq!(
        err_ws_name.unwrap_err().unwrap(),
        MarketplaceError::InvalidParam
    );

    // Empty description
    let params_empty_desc = RegisterParams {
        name: String::from_str(&f.env, "Valid Store"),
        description: String::from_str(&f.env, ""),
        category: symbol_short!("retail"),
        image_url: String::from_str(&f.env, ""),
        metadata: None,
        metadata_uri: None,
        required_verifications: 1,
    };

    let err_empty_desc = crate::testutil::try_register_merchant_with_deposit(
        &f.env,
        &f.client,
        &merchant2,
        &params_empty_desc,
    );
    assert_eq!(
        err_empty_desc.unwrap_err().unwrap(),
        MarketplaceError::InvalidParam
    );

    // Whitespace-only description
    let params_ws_desc = RegisterParams {
        name: String::from_str(&f.env, "Valid Store"),
        description: String::from_str(&f.env, "   "),
        category: symbol_short!("retail"),
        image_url: String::from_str(&f.env, ""),
        metadata: None,
        metadata_uri: None,
        required_verifications: 1,
    };

    let err_ws_desc = crate::testutil::try_register_merchant_with_deposit(
        &f.env,
        &f.client,
        &merchant2,
        &params_ws_desc,
    );
    assert_eq!(
        err_ws_desc.unwrap_err().unwrap(),
        MarketplaceError::InvalidParam
    );
}

#[test]
fn test_update_merchant_profile() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);
    let stranger = Address::generate(&f.env);

    let id = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Store A"),
            description: String::from_str(&f.env, "Old Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "old.png"),
            metadata: None,
            metadata_uri: None,
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
        &None,
    );
    assert_eq!(
        empty_name_err.unwrap_err().unwrap(),
        MarketplaceError::InvalidParam
    );

    // Whitespace-only name rejected
    let ws_name_err = f.client.try_update_merchant_profile(
        &id,
        &owner,
        &String::from_str(&f.env, "   "),
        &String::from_str(&f.env, "New Desc"),
        &String::from_str(&f.env, "new.png"),
        &None,
    );
    assert_eq!(
        ws_name_err.unwrap_err().unwrap(),
        MarketplaceError::InvalidParam
    );

    // Empty description rejected
    let empty_desc_err = f.client.try_update_merchant_profile(
        &id,
        &owner,
        &String::from_str(&f.env, "Store A"),
        &String::from_str(&f.env, ""),
        &String::from_str(&f.env, "new.png"),
        &None,
    );
    assert_eq!(
        empty_desc_err.unwrap_err().unwrap(),
        MarketplaceError::InvalidParam
    );

    // Whitespace-only description rejected
    let ws_desc_err = f.client.try_update_merchant_profile(
        &id,
        &owner,
        &String::from_str(&f.env, "Store A"),
        &String::from_str(&f.env, "   "),
        &String::from_str(&f.env, "new.png"),
        &None,
    );
    assert_eq!(
        ws_desc_err.unwrap_err().unwrap(),
        MarketplaceError::InvalidParam
    );

    // Owner succeeds
    f.client.update_merchant_profile(
        &id,
        &owner,
        &String::from_str(&f.env, "Store A Updated"),
        &String::from_str(&f.env, "New Desc"),
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

    let id = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Store Meta"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "logo.png"),
            metadata: Some(String::from_str(&f.env, "ipfs://v1")),
            metadata_uri: None,
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
    let id = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "NoOp Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "logo.png"),
            metadata: initial_metadata.clone(),
            metadata_uri: None,
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
    let id = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Change Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "logo.png"),
            metadata: initial_metadata.clone(),
            metadata_uri: None,
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
    let id = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "NoMeta Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "logo.png"),
            metadata: None,
            metadata_uri: None,
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
    let id = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "AddMeta Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "logo.png"),
            metadata: None,
            metadata_uri: None,
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
    let id = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "RemoveMeta Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "logo.png"),
            metadata: initial_metadata.clone(),
            metadata_uri: None,
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
fn test_verifier_cap_enforced() {
    let f = TestFixture::setup();

    let mut addresses = std::vec::Vec::new();
    for _ in 0..MAX_VERIFIERS {
        let addr = Address::generate(&f.env);
        f.client.add_verifier(
            &f.admin,
            &Verifier {
                address: addr.clone(),
                label: symbol_short!("v"),
                registered_at: f.env.ledger().timestamp(),
            },
        );
        addresses.push(addr);
    }
    assert_eq!(f.client.get_verifiers().len(), MAX_VERIFIERS);

    // The set is full: one more add must be rejected.
    let overflow_addr = Address::generate(&f.env);
    let overflow_err = f.client.try_add_verifier(
        &f.admin,
        &Verifier {
            address: overflow_addr.clone(),
            label: symbol_short!("v"),
            registered_at: f.env.ledger().timestamp(),
        },
    );
    assert_eq!(
        overflow_err.unwrap_err().unwrap(),
        MarketplaceError::VerifierLimitReached
    );
    assert_eq!(f.client.get_verifiers().len(), MAX_VERIFIERS);

    // Removing a verifier frees a slot again.
    let removed = addresses.remove(0);
    f.client.remove_verifier(&f.admin, &removed);
    assert_eq!(f.client.get_verifiers().len(), MAX_VERIFIERS - 1);
    f.client.add_verifier(
        &f.admin,
        &Verifier {
            address: overflow_addr,
            label: symbol_short!("v"),
            registered_at: f.env.ledger().timestamp(),
        },
    );
    assert_eq!(f.client.get_verifiers().len(), MAX_VERIFIERS);
}

#[test]
fn test_get_verifiers_page_pagination() {
    let f = TestFixture::setup();

    let mut expected = soroban_sdk::Vec::<Address>::new(&f.env);
    for i in 0..5u32 {
        let addr = Address::generate(&f.env);
        f.client.add_verifier(
            &f.admin,
            &Verifier {
                address: addr.clone(),
                label: Symbol::new(&f.env, &format!("v{i}")),
                registered_at: f.env.ledger().timestamp(),
            },
        );
        expected.push_back(addr);
    }

    // First page.
    let page1: VerifierPage = f.client.get_verifiers_page(&0, &2);
    assert_eq!(page1.total, 5);
    assert_eq!(page1.items.len(), 2);
    assert_eq!(
        page1.items.get(0).unwrap().address,
        expected.get(0).unwrap()
    );
    assert_eq!(
        page1.items.get(1).unwrap().address,
        expected.get(1).unwrap()
    );
    assert_eq!(page1.next_offset, Some(2));

    // Middle page.
    let page2: VerifierPage = f.client.get_verifiers_page(&2, &2);
    assert_eq!(page2.total, 5);
    assert_eq!(page2.items.len(), 2);
    assert_eq!(
        page2.items.get(0).unwrap().address,
        expected.get(2).unwrap()
    );
    assert_eq!(
        page2.items.get(1).unwrap().address,
        expected.get(3).unwrap()
    );
    assert_eq!(page2.next_offset, Some(4));

    // Final (partial) page: no further pages.
    let page3: VerifierPage = f.client.get_verifiers_page(&4, &2);
    assert_eq!(page3.total, 5);
    assert_eq!(page3.items.len(), 1);
    assert_eq!(
        page3.items.get(0).unwrap().address,
        expected.get(4).unwrap()
    );
    assert_eq!(page3.next_offset, None);

    // A page larger than the set is clamped.
    let big: VerifierPage = f.client.get_verifiers_page(&0, &100);
    assert_eq!(big.items.len(), 5);
    assert_eq!(big.next_offset, None);

    // Offset beyond the set yields an empty final page.
    let past: VerifierPage = f.client.get_verifiers_page(&10, &2);
    assert_eq!(past.total, 5);
    assert_eq!(past.items.len(), 0);
    assert_eq!(past.next_offset, None);

    // A zero limit yields an empty page but still points at the next offset.
    let empty: VerifierPage = f.client.get_verifiers_page(&1, &0);
    assert_eq!(empty.total, 5);
    assert_eq!(empty.items.len(), 0);
    assert_eq!(empty.next_offset, Some(1));
}

#[test]
fn test_get_verifiers_page_empty_set() {
    let f = TestFixture::setup();
    let page: VerifierPage = f.client.get_verifiers_page(&0, &10);
    assert_eq!(page.total, 0);
    assert_eq!(page.items.len(), 0);
    assert_eq!(page.next_offset, None);
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
    let id = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Certified Store"),
            description: String::from_str(&f.env, "Requires 2 verifiers"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "tech.png"),
            metadata: None,
            metadata_uri: None,
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

    let id = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Overflow Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "img.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    f.env.as_contract(&f._contract_id, || {
        f.env
            .storage()
            .persistent()
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

    let id = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Commission Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "img.png"),
            metadata: None,
            metadata_uri: None,
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
fn test_commission_tier_defaults_and_configuration() {
    let f = TestFixture::setup();
    let stranger = Address::generate(&f.env);

    // Default tiers are seeded on construction.
    let tiers = f.client.get_commission_tiers();
    assert_eq!(tiers.len(), 3);
    assert_eq!(
        tiers.get(0).unwrap(),
        CommissionTier {
            min_settled_volume: 0,
            commission_rate_bps: 500,
        }
    );
    assert_eq!(
        tiers.get(1).unwrap(),
        CommissionTier {
            min_settled_volume: 50_000,
            commission_rate_bps: 300,
        }
    );
    assert_eq!(
        tiers.get(2).unwrap(),
        CommissionTier {
            min_settled_volume: 500_000,
            commission_rate_bps: 150,
        }
    );

    // Non-admin cannot reconfigure tiers.
    let mut new_tiers = soroban_sdk::Vec::new(&f.env);
    new_tiers.push_back(CommissionTier {
        min_settled_volume: 0,
        commission_rate_bps: 400,
    });
    let err = f
        .client
        .try_set_commission_tiers(&stranger, &new_tiers);
    assert_eq!(err.unwrap_err().unwrap(), MarketplaceError::Unauthorized);

    // Admin can reconfigure tiers.
    f.client.set_commission_tiers(&f.admin, &new_tiers);
    assert_eq!(f.client.get_commission_tiers().len(), 1);
    assert_eq!(f.client.get_commission_tiers().get(0).unwrap().commission_rate_bps, 400);
}

#[test]
fn test_volume_tier_transitions_and_commission_calculation() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);

    let id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Volume Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "img.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    // Fresh merchant starts at the standard tier.
    let record = f.client.get_merchant_volume(&id);
    assert_eq!(record.total_settled_volume, 0);
    assert_eq!(record.active_tier_bps, 500);
    assert_eq!(f.client.get_commission(&id), 500);

    // Crossing the 50_000 threshold bumps to the 3% tier.
    f.client.record_settled_volume(&id, &50_000);
    let record = f.client.get_merchant_volume(&id);
    assert_eq!(record.total_settled_volume, 50_000);
    assert_eq!(record.active_tier_bps, 300);
    assert_eq!(f.client.get_commission(&id), 300);

    // Crossing the 500_000 threshold bumps to the 1.5% tier.
    f.client.record_settled_volume(&id, &450_000);
    let record = f.client.get_merchant_volume(&id);
    assert_eq!(record.total_settled_volume, 500_000);
    assert_eq!(record.active_tier_bps, 150);
    assert_eq!(f.client.get_commission(&id), 150);

    // Additional volume keeps the highest tier.
    f.client.record_settled_volume(&id, &1_000_000);
    let record = f.client.get_merchant_volume(&id);
    assert_eq!(record.total_settled_volume, 1_500_000);
    assert_eq!(record.active_tier_bps, 150);
    assert_eq!(f.client.get_commission(&id), 150);

    // Commission calculation matches the active tier schedule.
    let commission = f.client.calculate_commission(&id, &1_000);
    assert_eq!(commission, 15); // 1.5% of 1_000
}

#[test]
fn test_volume_tier_does_not_downgrade_on_manual_commission() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);

    let id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Manual Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "img.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    f.client.record_settled_volume(&id, &50_000);
    assert_eq!(f.client.get_commission(&id), 300);

    // A manual override is allowed but the tier record is preserved.
    f.client.set_merchant_commission(&id, &owner, &450);
    assert_eq!(f.client.get_commission(&id), 450);
    assert_eq!(f.client.get_merchant_volume(&id).active_tier_bps, 300);
}

#[test]
fn test_merchant_operational_view_registered_merchant() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);

    let id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Operational Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "img.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    let view = f.client.get_merchant_operational_view(&id);
    assert_eq!(view.status, MerchantStatus::Registered);
    assert!(!view.verified);
    assert_eq!(view.effective_fee_bps, f.client.get_commission(&id));
    assert_eq!(view.bond_balance, 0);
    assert_eq!(view.dispute_count, 0);
}

#[test]
fn test_merchant_operational_view_verified_merchant() {
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
            name: String::from_str(&f.env, "Verified Operational Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "img.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );
    f.client.verify_merchant(&id, &verifier);

    let view = f.client.get_merchant_operational_view(&id);
    assert_eq!(view.status, MerchantStatus::Verified);
    assert!(view.verified);
    assert_eq!(view.effective_fee_bps, f.client.get_commission(&id));
    assert_eq!(view.bond_balance, 0);
    assert_eq!(view.dispute_count, 0);
}

#[test]
fn test_merchant_operational_view_unverified_merchant() {
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
            name: String::from_str(&f.env, "Unverified Operational Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "img.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 2,
        },
    );
    // Only one of two required verifications.
    f.client.verify_merchant(&id, &verifier);

    let view = f.client.get_merchant_operational_view(&id);
    assert_eq!(view.status, MerchantStatus::Registered);
    assert!(!view.verified);
}

#[test]
fn test_merchant_operational_view_suspended_merchant() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);

    let id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Suspended Operational Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "img.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );
    f.client.suspend_merchant(&f.admin, &id);

    let view = f.client.get_merchant_operational_view(&id);
    assert_eq!(view.status, MerchantStatus::Suspended);
    assert!(!view.verified);
}

#[test]
fn test_merchant_operational_view_unregistered_merchant_not_found() {
    let f = TestFixture::setup();

    let err = f.client.try_get_merchant_operational_view(&999);
    assert_eq!(err.unwrap_err().unwrap(), MarketplaceError::MerchantNotFound);
}

#[test]
fn test_merchant_stats_lifecycle() {
    let f = TestFixture::setup();
    let owner1 = Address::generate(&f.env);
    let owner2 = Address::generate(&f.env);

    let id1 = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner1,
        &RegisterParams {
            name: String::from_str(&f.env, "Alpha Store"),
            description: String::from_str(&f.env, "Desc 1"),
            category: symbol_short!("food"),
            image_url: String::from_str(&f.env, "alpha.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );
    let id2 = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner2,
        &RegisterParams {
            name: String::from_str(&f.env, "Beta Store"),
            description: String::from_str(&f.env, "Desc 2"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "beta.png"),
            metadata: None,
            metadata_uri: None,
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

    f.client
        .close_merchant(&f.admin, &id2, &symbol_short!("miscond"));
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

    let id = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Safe Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("food"),
            image_url: String::from_str(&f.env, "food.png"),
            metadata: None,
            metadata_uri: None,
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
    f.client
        .close_merchant(&f.admin, &id, &symbol_short!("bad_cond"));
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
fn test_risk_oracle_freezes_trading_and_bans_are_permanent() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);
    let risk_oracle = Address::generate(&f.env);
    let merchant_id = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Risk Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("food"),
            image_url: String::from_str(&f.env, "food.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );
    f.client
        .set_risk_oracle(&f.admin, &Some(risk_oracle.clone()));

    assert!(f.client.is_merchant_trading(&owner));
    assert_eq!(
        f.client
            .try_suspend_merchant(&Address::generate(&f.env), &merchant_id),
        Err(Ok(MarketplaceError::Unauthorized))
    );

    f.client.suspend_merchant(&risk_oracle, &merchant_id);
    assert!(!f.client.is_merchant_trading(&owner));
    assert_eq!(
        f.client.get_merchant(&merchant_id).status,
        MerchantStatus::Suspended
    );

    f.client.unsuspend_merchant(&f.admin, &merchant_id);
    assert!(f.client.is_merchant_trading(&owner));

    f.client.ban_merchant(&risk_oracle, &merchant_id);
    assert!(!f.client.is_merchant_trading(&owner));
    assert_eq!(
        f.client.get_merchant(&merchant_id).status,
        MerchantStatus::Banned
    );
    assert_eq!(
        f.client.try_unsuspend_merchant(&f.admin, &merchant_id),
        Err(Ok(MarketplaceError::MerchantBanned))
    );
}

#[test]
fn test_merchant_owner_can_register_only_one_open_profile() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);
    let params = |name: &str| RegisterParams {
        name: String::from_str(&f.env, name),
        description: String::from_str(&f.env, "Desc"),
        category: symbol_short!("food"),
        image_url: String::from_str(&f.env, "food.png"),
        metadata: None,
        metadata_uri: None,
        required_verifications: 1,
    };
    crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner,
        &params("Store One"),
    );
    assert_eq!(
        crate::testutil::try_register_merchant_with_deposit(
            &f.env,
            &f.client,
            &owner,
            &params("Store Two")
        ),
        Err(Ok(MarketplaceError::DuplicateMerchantOwner))
    );
}

#[test]
fn test_paginated_discovery_cost_stays_within_thresholds() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);
    crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Cost Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );
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

        crate::testutil::register_merchant_with_deposit(
            &f.env,
            &f.client,
            &owner,
            &RegisterParams {
                name,
                description: String::from_str(&f.env, "Desc"),
                category,
                image_url: String::from_str(&f.env, "url"),
                metadata: None,
                metadata_uri: None,
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
    let id1 = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner1,
        &RegisterParams {
            name: String::from_str(&f.env, "Store 1"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    let id2 = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner2,
        &RegisterParams {
            name: String::from_str(&f.env, "Store 2"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    let id3 = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner3,
        &RegisterParams {
            name: String::from_str(&f.env, "Store 3"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    let id4 = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner4,
        &RegisterParams {
            name: String::from_str(&f.env, "Store 4"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            metadata_uri: None,
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
    f.client
        .close_merchant(&f.admin, &id3, &symbol_short!("test"));
    let registered = f
        .client
        .get_merchants_by_status(&MerchantStatus::Registered, &0, &10);
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
    assert_eq!(
        registered.items.get(0).unwrap().status,
        MerchantStatus::Registered
    );
    assert_eq!(
        registered.items.get(1).unwrap().status,
        MerchantStatus::Registered
    );
    let suspended = f
        .client
        .get_merchants_by_status(&MerchantStatus::Suspended, &0, &10);
    assert_eq!(
        suspended.items.get(0).unwrap().status,
        MerchantStatus::Suspended
    );
    let closed = f
        .client
        .get_merchants_by_status(&MerchantStatus::Closed, &0, &10);
    assert_eq!(closed.items.len(), 1);
    assert_eq!(closed.total, 1);
    assert_eq!(closed.items.get(0).unwrap().id, id3);
    assert_eq!(closed.items.get(0).unwrap().status, MerchantStatus::Closed);

    // Get merchants by Verified status (should be empty)
    let verified = f
        .client
        .get_merchants_by_status(&MerchantStatus::Verified, &0, &10);
    let verified = f
        .client
        .get_merchants_by_status(&MerchantStatus::Verified, &0, &10);
    assert_eq!(verified.items.len(), 0);
    assert_eq!(verified.total, 0);
    assert_eq!(verified.next_offset, None);
}

#[test]
fn test_treasury_split_routes_fees_with_zero_residual() {
    let f = TestFixture::setup();
    let dao = Address::generate(&f.env);
    let arbiter = Address::generate(&f.env);
    let insurance = Address::generate(&f.env);

    let config = TreasurySplitConfig {
        dao_treasury: dao.clone(),
        dao_bps: 7_000,
        arbiter_pool: arbiter.clone(),
        arbiter_bps: 2_000,
        insurance_pool: insurance.clone(),
        insurance_bps: 1_000,
    };
    f.client.set_treasury_split_config(&f.admin, &config);

    let (token_id, token_client) = deploy_token(&f.env, &f.admin);
    f.client.set_fee_token(&f.admin, &token_id);

    let fee_amount: i128 = 10_000;
    token_client.mint(&f._contract_id, &fee_amount);

    f.client.route_fee_disbursements(&fee_amount);

    assert_eq!(token_client.balance(&dao), 7_000);
    assert_eq!(token_client.balance(&arbiter), 2_000);
    assert_eq!(token_client.balance(&insurance), 1_000);
    // Zero token leakage: contract retains nothing.
    assert_eq!(token_client.balance(&f._contract_id), 0);
}

#[test]
fn test_treasury_split_remainder_routed_to_insurance_pool() {
    let f = TestFixture::setup();
    let dao = Address::generate(&f.env);
    let arbiter = Address::generate(&f.env);
    let insurance = Address::generate(&f.env);

    let config = TreasurySplitConfig {
        dao_treasury: dao.clone(),
        dao_bps: 7_000,
        arbiter_pool: arbiter.clone(),
        arbiter_bps: 2_000,
        insurance_pool: insurance.clone(),
        insurance_bps: 1_000,
    };
    f.client.set_treasury_split_config(&f.admin, &config);

    let (token_id, token_client) = deploy_token(&f.env, &f.admin);
    f.client.set_fee_token(&f.admin, &token_id);

    // Amount chosen so integer division leaves a remainder.
    let fee_amount: i128 = 1_001;
    token_client.mint(&f._contract_id, &fee_amount);

    f.client.route_fee_disbursements(&fee_amount);

    let dao_share = fee_amount * 7_000 / 10_000;
    let arbiter_share = fee_amount * 2_000 / 10_000;
    assert_eq!(token_client.balance(&dao), dao_share);
    assert_eq!(token_client.balance(&arbiter), arbiter_share);
    // Remainder lands in the insurance pool.
    assert_eq!(
        token_client.balance(&insurance),
        fee_amount - dao_share - arbiter_share
    );
    assert_eq!(token_client.balance(&f._contract_id), 0);
}

#[test]
fn test_treasury_split_rejects_invalid_bps_sum() {
    let f = TestFixture::setup();
    let config = TreasurySplitConfig {
        dao_treasury: Address::generate(&f.env),
        dao_bps: 7_000,
        arbiter_pool: Address::generate(&f.env),
        arbiter_bps: 2_000,
        insurance_pool: Address::generate(&f.env),
        insurance_bps: 500,
    };
    let err = f.client.try_set_treasury_split_config(&f.admin, &config);
    assert_eq!(
        err.unwrap_err().unwrap(),
        MarketplaceError::InvalidTreasurySplit
    );
}

#[test]
fn test_liquidation_reserve_locked_for_high_value_order() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);
    let buyer = Address::generate(&f.env);

    let merchant_id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "High Value Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "img.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    let order_id: u64 = 1;
    let order_value: i128 = 20_000;
    f.env.ledger().set_sequence_number(1_000);
    f.client.lock_liquidation_reserve(
        &merchant_id,
        &order_id,
        &order_value,
        &buyer,
    );

    let reserve = f
        .client
        .get_liquidation_reserve(&order_id)
        .expect("reserve must be locked");
    assert_eq!(reserve.merchant, owner);
    assert_eq!(reserve.order_id, order_id);
    assert_eq!(reserve.reserve_amount, order_value / 10);
    assert_eq!(reserve.unlock_ledger, 1_000 + RESERVE_LOCK_LEDGERS);
}

#[test]
fn test_liquidation_reserve_early_withdrawal_fails() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);
    let buyer = Address::generate(&f.env);

    let merchant_id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Early Withdraw Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "img.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    let order_id: u64 = 2;
    f.env.ledger().set_sequence_number(500);
    f.client
        .lock_liquidation_reserve(&merchant_id, &order_id, &20_000, &buyer);

    f.env
        .ledger()
        .set_sequence_number(500 + RESERVE_LOCK_LEDGERS - 1);
    let err = f.client.try_withdraw_liquidation_reserve(&order_id, &owner);
    assert_eq!(
        err.unwrap_err().unwrap(),
        MarketplaceError::ReserveNotMature
    );
}

#[test]
fn test_liquidation_reserve_withdrawal_after_maturity() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);
    let buyer = Address::generate(&f.env);

    let merchant_id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Mature Withdraw Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "img.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    let order_id: u64 = 3;
    f.env.ledger().set_sequence_number(100);
    f.client
        .lock_liquidation_reserve(&merchant_id, &order_id, &20_000, &buyer);

    f.env
        .ledger()
        .set_sequence_number(100 + RESERVE_LOCK_LEDGERS);
    f.client.withdraw_liquidation_reserve(&order_id, &owner);
    assert!(f.client.get_liquidation_reserve(&order_id).is_none());
}

#[test]
fn test_liquidation_reserve_can_be_liquidated_by_arbiter() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);
    let buyer = Address::generate(&f.env);
    let arbiter = Address::generate(&f.env);

    f.client.set_arbiter(&f.admin, &arbiter);

    let merchant_id = f.client.register_merchant(
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "Liquidated Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "img.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    let order_id: u64 = 4;
    f.env.ledger().set_sequence_number(200);
    f.client
        .lock_liquidation_reserve(&merchant_id, &order_id, &20_000, &buyer);

    let (token_id, token_client) = deploy_token(&f.env, &f.admin);
    f.client.set_reserve_token(&f.admin, &token_id);
    token_client.mint(&f._contract_id, &2_000);

    f.client
        .liquidate_reserve(&arbiter, &order_id, &buyer);

    assert_eq!(token_client.balance(&buyer), 2_000);
    assert_eq!(token_client.balance(&f._contract_id), 0);
    assert!(f.client.get_liquidation_reserve(&order_id).is_none());
}

#[test]
fn test_merchant_state_ttl_survives_repeated_reads() {
    let f = TestFixture::setup();
    let owner = Address::generate(&f.env);

    let id = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner,
        &RegisterParams {
            name: String::from_str(&f.env, "TTL Survival Store"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("goods"),
            image_url: String::from_str(&f.env, "img.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    let env = &f.env;
    let contract_id = f._contract_id.clone();

    // Extend instance TTL so contract call does not fail during sequence jump
    env.as_contract(&contract_id, || {
        env.storage().instance().extend_ttl(1_000_000, 1_000_000);
    });

    // Advance sequence number so persistent TTLs decay below threshold
    f.env
        .ledger()
        .set_sequence_number(f.env.ledger().sequence() + 508_400);

    let before: [u32; 6] = env.as_contract(&contract_id, || {
        let keys = [
            crate::DataKey::Merchant(id),
            crate::DataKey::MerchantName(String::from_str(env, "TTL Survival Store")),
            crate::DataKey::VerifiedCount(id),
            crate::DataKey::VerificationPolicy(id),
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

    let after: [u32; 6] = env.as_contract(&contract_id, || {
        let keys = [
            crate::DataKey::Merchant(id),
            crate::DataKey::MerchantName(String::from_str(env, "TTL Survival Store")),
            crate::DataKey::VerifiedCount(id),
            crate::DataKey::VerificationPolicy(id),
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
    let id1 = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner1,
        &RegisterParams {
            name: String::from_str(&f.env, "Tech Store 1"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    let id2 = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner2,
        &RegisterParams {
            name: String::from_str(&f.env, "Tech Store 2"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    let id3 = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner3,
        &RegisterParams {
            name: String::from_str(&f.env, "Tech Store 3"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    let id4 = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &owner4,
        &RegisterParams {
            name: String::from_str(&f.env, "Tech Store 4"),
            description: String::from_str(&f.env, "Desc"),
            category: symbol_short!("tech"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    // Suspend id2, Close id3
    f.client.suspend_merchant(&f.admin, &id2);
    f.client
        .close_merchant(&f.admin, &id3, &symbol_short!("test"));
    f.client
        .close_merchant(&f.admin, &id3, &symbol_short!("test"));

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

    // Get tech merchants by Closed status (should be empty, closed pruned from index)
    let closed = f.client.get_merchants_by_category_status(
        &symbol_short!("tech"),
        &MerchantStatus::Closed,
        &0,
        &10,
    );
    assert_eq!(closed.items.len(), 0);
    assert_eq!(closed.total, 0);

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

        crate::testutil::register_merchant_with_deposit(
            &f.env,
            &f.client,
            &owner,
            &RegisterParams {
                name,
                description: String::from_str(&f.env, "Desc"),
                category: symbol_short!("tech"),
                image_url: String::from_str(&f.env, "url"),
                metadata: None,
                metadata_uri: None,
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
    assert_eq!(
        page1.next_offset,
        Some(3),
        "First page should have next_offset = 3"
    );

    let page2 = f.client.get_merchants(&page1.next_offset.unwrap(), &3);
    assert_eq!(page2.total, 10);
    assert_eq!(page2.items.len(), 3);
    assert_eq!(
        page2.next_offset,
        Some(6),
        "Second page should have next_offset = 6"
    );
    assert_eq!(
        page2.next_offset,
        Some(6),
        "Second page should have next_offset = 6"
    );

    let page3 = f.client.get_merchants(&page2.next_offset.unwrap(), &3);
    assert_eq!(page3.total, 10);
    assert_eq!(page3.items.len(), 3);
    assert_eq!(
        page3.next_offset,
        Some(9),
        "Third page should have next_offset = 9"
    );
    assert_eq!(
        page3.next_offset,
        Some(9),
        "Third page should have next_offset = 9"
    );

    let page4 = f.client.get_merchants(&page3.next_offset.unwrap(), &3);
    assert_eq!(page4.total, 10);
    assert_eq!(page4.items.len(), 1, "Last page should have 1 item");
    assert_eq!(
        page4.next_offset, None,
        "Last page should have None for next_offset"
    );
    assert_eq!(
        page4.next_offset, None,
        "Last page should have None for next_offset"
    );
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
    assert_eq!(acc_1.unwrap_err().unwrap(), MarketplaceError::Unauthorized);

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

    let id = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &seller,
        &RegisterParams {
            name: String::from_str(&f.env, "Reputable Seller"),
            description: String::from_str(&f.env, "Has reputation"),
            category: symbol_short!("services"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            metadata_uri: None,
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

    let id = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &seller,
        &RegisterParams {
            name: String::from_str(&f.env, "Resolution Tester"),
            description: String::from_str(&f.env, "Tests resolution"),
            category: symbol_short!("test"),
            image_url: String::from_str(&f.env, "url"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 1,
        },
    );

    // State 1: NotConfigured
    let view_det_not_conf = f.client.get_merchant_view_detailed(&id);
    assert_eq!(
        view_det_not_conf.reputation,
        crate::ReputationResolution::NotConfigured
    );
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
    f.client
        .set_merchant_reputation(&f.admin, &id, &Some(rep_id.clone()));
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
    f.client
        .set_merchant_reputation(&f.admin, &id, &Some(bogus_addr));
    let view_det_failed = f.client.get_merchant_view_detailed(&id);
    assert_eq!(
        view_det_failed.reputation,
        crate::ReputationResolution::CallFailed
    );
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
    let merchant_id = crate::testutil::register_merchant_with_deposit(
        &f.env,
        &f.client,
        &merchant_addr,
        &RegisterParams {
            name: String::from_str(&f.env, "Stellar Artisans"),
            description: String::from_str(&f.env, "Handmade Stellar goods"),
            category: symbol_short!("crafts"),
            image_url: String::from_str(&f.env, "https://example.com/artisan.png"),
            metadata: Some(String::from_str(&f.env, "ipfs://bafybeicraft")),
            metadata_uri: None,
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
}
