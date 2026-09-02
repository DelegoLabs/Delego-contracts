//! Delego Marketplace Contract
//!
//! Maintains a trusted on-chain registry of merchants with multi-verifier
//! verification, category and name discovery, commission configuration,
//! metadata cooldown enforcement, status lifecycle controls, and reputation
//! score snapshot integration.
//!
//! # Error range
//! `MarketplaceError` discriminants occupy `5000..=5999` (per the workspace-wide
//! error-code allocation table in `docs/architecture/contracts.md`).
//!
//! # Event topic schema
//! Entity-scoped events: `(mkplc, <action>, merchant_id)`.
//! Contract-wide events: `(mkplc, <action>)`.

// Contract crates compile as no_std for release and wasm builds, but keep std
// enabled during testing so dev-dependencies and test assertions operate normally.
// This exact conditional form must be consistent across all workspace contract crates.
#![cfg_attr(not(test), no_std)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env, InvokeError,
    String, Symbol, Vec,
};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Maximum merchants returned in a single paginated call.
const MAX_PAGE_LIMIT: u32 = 50;
/// Maximum merchant ids pruned in one call.
const MAX_PRUNE_BATCH: u32 = 50;
/// Default metadata cooldown (24 hours in seconds).
const DEFAULT_COOLDOWN_SECONDS: u64 = 86_400;
/// Minimum metadata cooldown (60 seconds).
const MIN_COOLDOWN_SECONDS: u64 = 60;
/// Maximum metadata cooldown (30 days).
const MAX_COOLDOWN_SECONDS: u64 = 2_592_000;
/// Maximum commission in basis points.
const MAX_COMMISSION_BPS: u32 = 10_000;
/// Approximate ledgers per day (5-second average ledger time).
const LEDGERS_PER_DAY: u32 = 17_280;
/// TTL extension for persistent entries (~30 days).
const PERSISTENT_TTL_LEDGERS: u32 = LEDGERS_PER_DAY * 30;

// ─── Types ────────────────────────────────────────────────────────────────────

/// Lifecycle status of a merchant.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MerchantStatus {
    /// Created, not yet verified.
    Registered = 0,
    /// Passed the verification threshold.
    Verified = 1,
    /// Temporarily disabled by admin.
    Suspended = 2,
    /// Permanently removed.
    Closed = 3,
}

/// Full on-chain record for a single merchant.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Merchant {
    pub id: u64,
    pub owner: Address,
    pub name: String,
    pub description: String,
    pub category: Symbol,
    pub image_url: String,
    pub commission_rate_bps: u32,
    pub metadata: Option<String>,
    pub status: MerchantStatus,
    /// True once the verification threshold has been reached.
    pub verified: bool,
    /// How many distinct verifications are required to become Verified.
    pub required_verifications: u32,
    pub created_at: u64,
    pub updated_at: u64,
    /// Optional per-merchant reputation contract reference.
    pub reputation: Option<Address>,
}

/// Lightweight discovery view returned by paginated queries.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantView {
    pub id: u64,
    pub name: String,
    pub category: Symbol,
    pub commission_rate_bps: u32,
    pub verified: bool,
    pub status: MerchantStatus,
    /// Live snapshot from the paired reputation contract, if any.
    pub reputation_score: Option<u32>,
}

/// Parameters for `register_merchant`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterParams {
    pub name: String,
    pub description: String,
    pub category: Symbol,
    pub image_url: String,
    pub metadata: Option<String>,
    /// Number of distinct verifier attestations required before status →
    /// Verified.  Must be ≥ 1.
    pub required_verifications: u32,
}

/// A registered verifier that can attest merchants.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Verifier {
    pub address: Address,
    pub label: Symbol,
    pub registered_at: u64,
}

/// Semver-style contract version info.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    /// Symbol encoding of the semver, e.g. `0_2_0` for 0.2.0.
    /// Parsed by `scripts/check-changelog.sh` to validate CHANGELOG entries.
    pub semver: Symbol,
}

// ─── Events ───────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantRegisteredEvent {
    pub merchant_id: u64,
    pub owner: Address,
    pub name: String,
    pub category: Symbol,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantVerifiedEvent {
    pub merchant_id: u64,
    pub verifier: Address,
    pub verified_count: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct VerificationRevokedEvent {
    pub merchant_id: u64,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantSuspendedEvent {
    pub merchant_id: u64,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantUnsuspendedEvent {
    pub merchant_id: u64,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantClosedEvent {
    pub merchant_id: u64,
    pub reason: Symbol,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct AdminProposedEvent {
    pub current_admin: Address,
    pub proposed_admin: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct AdminAcceptedEvent {
    pub new_admin: Address,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantsPrunedEvent {
    pub count: u32,
}

// ─── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    /// Instance: current admin.
    Admin,
    /// Instance: pending admin (two-step handover).
    PendingAdmin,
    /// Instance: next merchant id counter.
    NextMerchantId,
    /// Instance: list of registered verifiers.
    Verifiers,
    /// Instance: global metadata cooldown in seconds.
    MetadataCooldown,
    /// Instance: global reputation contract (optional).
    GlobalReputationContract,
    /// Persistent: full Merchant record.
    Merchant(u64),
    /// Persistent: merchant name → id (uniqueness guard).
    MerchantName(String),
    /// Persistent: all merchant ids (discovery index).
    MerchantIds,
    /// Persistent: ids per category.
    CategoryIndex(Symbol),
    /// Persistent: how many verifiers have attested merchant `id`.
    VerifiedCount(u64),
    /// Persistent: list of verifier addresses that attested merchant `id`.
    MerchantVerifierList(u64),
    /// Persistent: timestamp of last metadata update for merchant `id`.
    LastMetadataUpdate(u64),
}

// ─── Errors ───────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MarketplaceError {
    /// Contract has already been initialised.
    AlreadyInitialized = 5000,
    /// Contract has not been initialised yet.
    NotInitialized = 5001,
    /// Caller is not the admin.
    Unauthorized = 5002,
    /// Merchant id was not found.
    MerchantNotFound = 5003,
    /// Merchant name is already taken.
    NameAlreadyTaken = 5004,
    /// Invalid `required_verifications` value (must be ≥ 1).
    InvalidRequiredVerifications = 5005,
    /// Commission exceeds 10 000 bps.
    InvalidCommission = 5006,
    /// Merchant is suspended or closed — no mutations allowed.
    MerchantFrozen = 5007,
    /// Caller is not the merchant owner.
    NotOwner = 5008,
    /// Verifier is not registered.
    VerifierNotFound = 5009,
    /// Verifier has already attested this merchant.
    AlreadyVerified = 5010,
    /// Metadata update is locked by the cooldown policy.
    MetadataLockActive = 5011,
    /// Cooldown value is outside the allowed range.
    InvalidCooldown = 5012,
    /// No pending admin proposal to accept.
    NoPendingAdmin = 5013,
    /// Caller is not the proposed admin.
    NotPendingAdmin = 5014,
    /// Removing this verifier would strand the verification policy.
    VerifierRemovalWouldStrandPolicy = 5015,
    /// Pagination limit exceeds the maximum page size (50).
    LimitTooLarge = 5016,
    /// Batch size exceeds the allowed ceiling (50).
    BatchTooLarge = 5017,
    /// Merchant is not in Closed status (for prune).
    MerchantNotClosed = 5018,
    /// Merchant is not in Verified status (for revoke_verification).
    MerchantNotVerified = 5019,
    /// Verifier label is already in use.
    VerifierAlreadyRegistered = 5020,
}

// ─── Contract ─────────────────────────────────────────────────────────────────

#[contract]
pub struct MarketplaceContract;

#[contractimpl]
impl MarketplaceContract {
    // ── Initialisation ────────────────────────────────────────────────────────

    /// Initialise the contract with an admin. Must be called exactly once.
    pub fn initialize(env: Env, admin: Address) -> Result<(), MarketplaceError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(MarketplaceError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::NextMerchantId, &1u64);
        env.storage()
            .instance()
            .set(&DataKey::MetadataCooldown, &DEFAULT_COOLDOWN_SECONDS);
        // Initialise empty verifier list.
        let verifiers: Vec<Verifier> = Vec::new(&env);
        env.storage()
            .instance()
            .set(&DataKey::Verifiers, &verifiers);
        Ok(())
    }

    // ── Merchant registration & profile ──────────────────────────────────────

    /// Register a new merchant.  `merchant` becomes the owner.
    pub fn register_merchant(
        env: Env,
        merchant: Address,
        params: RegisterParams,
    ) -> Result<u64, MarketplaceError> {
        merchant.require_auth();
        Self::require_initialized(&env)?;

        if params.required_verifications < 1 {
            return Err(MarketplaceError::InvalidRequiredVerifications);
        }
        if params.commission_rate_bps_exceeds_max() {
            return Err(MarketplaceError::InvalidCommission);
        }

        // Name uniqueness check.
        if env
            .storage()
            .persistent()
            .has(&DataKey::MerchantName(params.name.clone()))
        {
            return Err(MarketplaceError::NameAlreadyTaken);
        }

        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextMerchantId)
            .unwrap_or(1u64);
        env.storage()
            .instance()
            .set(&DataKey::NextMerchantId, &(id + 1));

        let now = env.ledger().timestamp();

        let record = Merchant {
            id,
            owner: merchant.clone(),
            name: params.name.clone(),
            description: params.description.clone(),
            category: params.category.clone(),
            image_url: params.image_url.clone(),
            commission_rate_bps: 0,
            metadata: params.metadata.clone(),
            status: MerchantStatus::Registered,
            verified: false,
            required_verifications: params.required_verifications,
            created_at: now,
            updated_at: now,
            reputation: None,
        };

        // Persist the merchant record.
        let mk = DataKey::Merchant(id);
        env.storage().persistent().set(&mk, &record);
        env.storage()
            .persistent()
            .extend_ttl(&mk, PERSISTENT_TTL_LEDGERS, PERSISTENT_TTL_LEDGERS);

        // Mark the name as taken.
        let nk = DataKey::MerchantName(params.name.clone());
        env.storage().persistent().set(&nk, &id);
        env.storage()
            .persistent()
            .extend_ttl(&nk, PERSISTENT_TTL_LEDGERS, PERSISTENT_TTL_LEDGERS);

        // Append to global index.
        Self::append_to_merchant_ids(&env, id);

        // Append to category index.
        Self::append_to_category_index(&env, &params.category, id);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("reg"), id),
            MerchantRegisteredEvent {
                merchant_id: id,
                owner: merchant,
                name: params.name,
                category: params.category,
            },
        );

        Ok(id)
    }

    /// Check whether a merchant name is still available.
    pub fn is_name_available(env: Env, name: String) -> bool {
        !env.storage().persistent().has(&DataKey::MerchantName(name))
    }

    /// Owner or admin may update name, description, and image URL.
    pub fn update_merchant_profile(
        env: Env,
        merchant_id: u64,
        caller: Address,
        name: String,
        description: String,
        image_url: String,
    ) -> Result<(), MarketplaceError> {
        caller.require_auth();
        let mut record = Self::load_merchant(&env, merchant_id)?;
        Self::check_not_frozen(&record)?;

        let admin = Self::get_admin_address(&env)?;
        if caller != record.owner && caller != admin {
            return Err(MarketplaceError::NotOwner);
        }

        // If renaming, check and update the name index.
        if name != record.name {
            if env
                .storage()
                .persistent()
                .has(&DataKey::MerchantName(name.clone()))
            {
                return Err(MarketplaceError::NameAlreadyTaken);
            }
            // Free the old name.
            env.storage()
                .persistent()
                .remove(&DataKey::MerchantName(record.name.clone()));
            // Claim the new name.
            let nk = DataKey::MerchantName(name.clone());
            env.storage().persistent().set(&nk, &merchant_id);
            env.storage().persistent().extend_ttl(
                &nk,
                PERSISTENT_TTL_LEDGERS,
                PERSISTENT_TTL_LEDGERS,
            );

            record.name = name;
        }

        record.description = description;
        record.image_url = image_url;
        record.updated_at = env.ledger().timestamp();

        Self::save_merchant(&env, merchant_id, &record);
        Ok(())
    }

    /// Owner (cooldown-gated) or admin (no cooldown) updates the IPFS/metadata field.
    pub fn update_metadata(
        env: Env,
        merchant_id: u64,
        caller: Address,
        new_metadata: Option<String>,
    ) -> Result<(), MarketplaceError> {
        caller.require_auth();
        let mut record = Self::load_merchant(&env, merchant_id)?;
        Self::check_not_frozen(&record)?;

        let admin = Self::get_admin_address(&env)?;
        if caller != record.owner && caller != admin {
            return Err(MarketplaceError::NotOwner);
        }

        // Non-admin callers are subject to the cooldown.
        if caller != admin {
            let cooldown: u64 = env
                .storage()
                .instance()
                .get(&DataKey::MetadataCooldown)
                .unwrap_or(DEFAULT_COOLDOWN_SECONDS);
            let now = env.ledger().timestamp();
            let last_key = DataKey::LastMetadataUpdate(merchant_id);
            if let Some(last_ts) = env.storage().persistent().get::<_, u64>(&last_key) {
                if now < last_ts + cooldown {
                    return Err(MarketplaceError::MetadataLockActive);
                }
            }
            let now2 = env.ledger().timestamp();
            env.storage().persistent().set(&last_key, &now2);
            env.storage().persistent().extend_ttl(
                &last_key,
                PERSISTENT_TTL_LEDGERS,
                PERSISTENT_TTL_LEDGERS,
            );
        }

        record.metadata = new_metadata;
        record.updated_at = env.ledger().timestamp();
        Self::save_merchant(&env, merchant_id, &record);
        Ok(())
    }

    // ── Verifier management ───────────────────────────────────────────────────

    /// Admin registers a new verifier.
    pub fn add_verifier(
        env: Env,
        admin: Address,
        verifier: Verifier,
    ) -> Result<(), MarketplaceError> {
        Self::require_admin_auth(&env, &admin)?;

        let mut verifiers: Vec<Verifier> = env
            .storage()
            .instance()
            .get(&DataKey::Verifiers)
            .unwrap_or(Vec::new(&env));

        // Reject duplicates.
        for v in verifiers.iter() {
            if v.address == verifier.address {
                return Err(MarketplaceError::VerifierAlreadyRegistered);
            }
        }

        verifiers.push_back(verifier);
        env.storage()
            .instance()
            .set(&DataKey::Verifiers, &verifiers);
        Ok(())
    }

    /// Admin removes a verifier. Fails if removal would strand existing policies.
    pub fn remove_verifier(
        env: Env,
        admin: Address,
        verifier_address: Address,
    ) -> Result<(), MarketplaceError> {
        Self::require_admin_auth(&env, &admin)?;

        let verifiers: Vec<Verifier> = env
            .storage()
            .instance()
            .get(&DataKey::Verifiers)
            .unwrap_or(Vec::new(&env));

        let mut new_verifiers: Vec<Verifier> = Vec::new(&env);
        let mut found = false;
        for v in verifiers.iter() {
            if v.address == verifier_address {
                found = true;
            } else {
                new_verifiers.push_back(v);
            }
        }
        if !found {
            return Err(MarketplaceError::VerifierNotFound);
        }

        env.storage()
            .instance()
            .set(&DataKey::Verifiers, &new_verifiers);
        Ok(())
    }

    /// Return the current verifier list.
    pub fn get_verifiers(env: Env) -> Vec<Verifier> {
        env.storage()
            .instance()
            .get(&DataKey::Verifiers)
            .unwrap_or(Vec::new(&env))
    }

    // ── Verification lifecycle ────────────────────────────────────────────────

    /// A registered verifier attests a merchant. When the required threshold
    /// is reached the merchant transitions to `Verified`.
    pub fn verify_merchant(
        env: Env,
        merchant_id: u64,
        verifier: Address,
    ) -> Result<(), MarketplaceError> {
        verifier.require_auth();
        let mut record = Self::load_merchant(&env, merchant_id)?;
        Self::check_not_frozen(&record)?;

        // Check verifier is registered.
        if !Self::is_registered_verifier(&env, &verifier) {
            return Err(MarketplaceError::VerifierNotFound);
        }

        // Check verifier hasn't already attested this merchant.
        let vlist_key = DataKey::MerchantVerifierList(merchant_id);
        let mut attested: Vec<Address> = env
            .storage()
            .persistent()
            .get(&vlist_key)
            .unwrap_or(Vec::new(&env));

        for v in attested.iter() {
            if v == verifier {
                return Err(MarketplaceError::AlreadyVerified);
            }
        }

        attested.push_back(verifier.clone());
        env.storage().persistent().set(&vlist_key, &attested);
        env.storage().persistent().extend_ttl(
            &vlist_key,
            PERSISTENT_TTL_LEDGERS,
            PERSISTENT_TTL_LEDGERS,
        );

        let count_key = DataKey::VerifiedCount(merchant_id);
        let verified_count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0u32) + 1;
        env.storage().persistent().set(&count_key, &verified_count);
        env.storage().persistent().extend_ttl(
            &count_key,
            PERSISTENT_TTL_LEDGERS,
            PERSISTENT_TTL_LEDGERS,
        );

        if verified_count >= record.required_verifications {
            record.verified = true;
            record.status = MerchantStatus::Verified;
        }
        record.updated_at = env.ledger().timestamp();
        Self::save_merchant(&env, merchant_id, &record);

        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("verified"),
                merchant_id,
            ),
            MerchantVerifiedEvent {
                merchant_id,
                verifier,
                verified_count,
            },
        );

        Ok(())
    }

    /// Admin clears the verification state for a merchant.
    pub fn revoke_verification(
        env: Env,
        admin: Address,
        merchant_id: u64,
    ) -> Result<(), MarketplaceError> {
        Self::require_admin_auth(&env, &admin)?;
        let mut record = Self::load_merchant(&env, merchant_id)?;

        if !record.verified {
            return Err(MarketplaceError::MerchantNotVerified);
        }

        record.verified = false;
        record.status = MerchantStatus::Registered;
        record.updated_at = env.ledger().timestamp();
        Self::save_merchant(&env, merchant_id, &record);

        // Clear count and verifier list.
        env.storage()
            .persistent()
            .remove(&DataKey::VerifiedCount(merchant_id));
        env.storage()
            .persistent()
            .remove(&DataKey::MerchantVerifierList(merchant_id));

        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("rev_ver"),
                merchant_id,
            ),
            VerificationRevokedEvent { merchant_id },
        );

        Ok(())
    }

    // ── Discovery & query ─────────────────────────────────────────────────────

    /// Retrieve the full merchant record.
    pub fn get_merchant(env: Env, merchant_id: u64) -> Result<Merchant, MarketplaceError> {
        Self::load_merchant(&env, merchant_id)
    }

    /// Retrieve a lightweight `MerchantView` with live reputation snapshot.
    pub fn get_merchant_view(env: Env, merchant_id: u64) -> Result<MerchantView, MarketplaceError> {
        let record = Self::load_merchant(&env, merchant_id)?;
        let reputation_score = Self::fetch_reputation_score(&env, &record);
        Ok(MerchantView {
            id: record.id,
            name: record.name,
            category: record.category,
            commission_rate_bps: record.commission_rate_bps,
            verified: record.verified,
            status: record.status,
            reputation_score,
        })
    }

    /// Paginated list of all merchants (offset + limit, max 50).
    pub fn get_merchants(
        env: Env,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<MerchantView>, MarketplaceError> {
        if limit > MAX_PAGE_LIMIT {
            return Err(MarketplaceError::LimitTooLarge);
        }
        let ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::MerchantIds)
            .unwrap_or(Vec::new(&env));

        let mut views: Vec<MerchantView> = Vec::new(&env);
        let start = offset as usize;
        let end = (offset + limit) as usize;

        for (i, id) in ids.iter().enumerate() {
            if i < start {
                continue;
            }
            if i >= end {
                break;
            }
            if let Ok(record) = Self::load_merchant(&env, id) {
                let reputation_score = Self::fetch_reputation_score(&env, &record);
                views.push_back(MerchantView {
                    id: record.id,
                    name: record.name,
                    category: record.category,
                    commission_rate_bps: record.commission_rate_bps,
                    verified: record.verified,
                    status: record.status,
                    reputation_score,
                });
            }
        }
        Ok(views)
    }

    /// Paginated list of merchants in a specific category (max 50).
    pub fn get_merchants_by_category(
        env: Env,
        category: Symbol,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<MerchantView>, MarketplaceError> {
        if limit > MAX_PAGE_LIMIT {
            return Err(MarketplaceError::LimitTooLarge);
        }
        let ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::CategoryIndex(category))
            .unwrap_or(Vec::new(&env));

        let mut views: Vec<MerchantView> = Vec::new(&env);
        let start = offset as usize;
        let end = (offset + limit) as usize;

        for (i, id) in ids.iter().enumerate() {
            if i < start {
                continue;
            }
            if i >= end {
                break;
            }
            if let Ok(record) = Self::load_merchant(&env, id) {
                let reputation_score = Self::fetch_reputation_score(&env, &record);
                views.push_back(MerchantView {
                    id: record.id,
                    name: record.name,
                    category: record.category,
                    commission_rate_bps: record.commission_rate_bps,
                    verified: record.verified,
                    status: record.status,
                    reputation_score,
                });
            }
        }
        Ok(views)
    }

    // ── Commission ────────────────────────────────────────────────────────────

    /// Owner or admin sets the per-merchant commission in basis points (≤ 10 000).
    pub fn set_merchant_commission(
        env: Env,
        merchant_id: u64,
        caller: Address,
        commission_bps: u32,
    ) -> Result<(), MarketplaceError> {
        caller.require_auth();
        let mut record = Self::load_merchant(&env, merchant_id)?;
        Self::check_not_frozen(&record)?;

        let admin = Self::get_admin_address(&env)?;
        if caller != record.owner && caller != admin {
            return Err(MarketplaceError::NotOwner);
        }
        if commission_bps > MAX_COMMISSION_BPS {
            return Err(MarketplaceError::InvalidCommission);
        }

        record.commission_rate_bps = commission_bps;
        record.updated_at = env.ledger().timestamp();
        Self::save_merchant(&env, merchant_id, &record);
        Ok(())
    }

    /// Get the commission rate (in bps) for a merchant.
    pub fn get_commission(env: Env, merchant_id: u64) -> Result<u32, MarketplaceError> {
        let record = Self::load_merchant(&env, merchant_id)?;
        Ok(record.commission_rate_bps)
    }

    // ── Moderation lifecycle ──────────────────────────────────────────────────

    /// Admin suspends a merchant (temporary block).
    pub fn suspend_merchant(
        env: Env,
        admin: Address,
        merchant_id: u64,
    ) -> Result<(), MarketplaceError> {
        Self::require_admin_auth(&env, &admin)?;
        let mut record = Self::load_merchant(&env, merchant_id)?;

        if record.status == MerchantStatus::Closed {
            return Err(MarketplaceError::MerchantFrozen);
        }

        record.status = MerchantStatus::Suspended;
        record.updated_at = env.ledger().timestamp();
        Self::save_merchant(&env, merchant_id, &record);

        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("suspend"),
                merchant_id,
            ),
            MerchantSuspendedEvent { merchant_id },
        );
        Ok(())
    }

    /// Admin lifts a suspension. Status restores to Verified or Registered.
    pub fn unsuspend_merchant(
        env: Env,
        admin: Address,
        merchant_id: u64,
    ) -> Result<(), MarketplaceError> {
        Self::require_admin_auth(&env, &admin)?;
        let mut record = Self::load_merchant(&env, merchant_id)?;

        if record.status != MerchantStatus::Suspended {
            return Err(MarketplaceError::MerchantFrozen);
        }

        record.status = if record.verified {
            MerchantStatus::Verified
        } else {
            MerchantStatus::Registered
        };
        record.updated_at = env.ledger().timestamp();
        Self::save_merchant(&env, merchant_id, &record);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("unsusp"), merchant_id),
            MerchantUnsuspendedEvent { merchant_id },
        );
        Ok(())
    }

    /// Admin permanently closes a merchant.
    pub fn close_merchant(
        env: Env,
        admin: Address,
        merchant_id: u64,
        reason: Symbol,
    ) -> Result<(), MarketplaceError> {
        Self::require_admin_auth(&env, &admin)?;
        let mut record = Self::load_merchant(&env, merchant_id)?;

        if record.status == MerchantStatus::Closed {
            return Err(MarketplaceError::MerchantFrozen);
        }

        record.status = MerchantStatus::Closed;
        record.updated_at = env.ledger().timestamp();
        Self::save_merchant(&env, merchant_id, &record);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("closed"), merchant_id),
            MerchantClosedEvent {
                merchant_id,
                reason,
            },
        );
        Ok(())
    }

    // ── Maintenance ───────────────────────────────────────────────────────────

    /// Admin prunes closed merchants from discovery indices. Returns the count
    /// actually removed. Batch size capped at 50.
    pub fn prune_closed_merchants(
        env: Env,
        admin: Address,
        merchant_ids: Vec<u64>,
    ) -> Result<u32, MarketplaceError> {
        Self::require_admin_auth(&env, &admin)?;

        if merchant_ids.len() > MAX_PRUNE_BATCH {
            return Err(MarketplaceError::BatchTooLarge);
        }

        let mut pruned = 0u32;

        for id in merchant_ids.iter() {
            let record = match Self::load_merchant(&env, id) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if record.status != MerchantStatus::Closed {
                continue;
            }

            // Remove from global MerchantIds index.
            let all_ids: Vec<u64> = env
                .storage()
                .persistent()
                .get(&DataKey::MerchantIds)
                .unwrap_or(Vec::new(&env));
            let mut new_ids: Vec<u64> = Vec::new(&env);
            for eid in all_ids.iter() {
                if eid != id {
                    new_ids.push_back(eid);
                }
            }
            env.storage()
                .persistent()
                .set(&DataKey::MerchantIds, &new_ids);

            // Remove from CategoryIndex.
            let cat_key = DataKey::CategoryIndex(record.category.clone());
            let cat_ids: Vec<u64> = env
                .storage()
                .persistent()
                .get(&cat_key)
                .unwrap_or(Vec::new(&env));
            let mut new_cat: Vec<u64> = Vec::new(&env);
            for eid in cat_ids.iter() {
                if eid != id {
                    new_cat.push_back(eid);
                }
            }
            env.storage().persistent().set(&cat_key, &new_cat);

            pruned += 1;
        }

        if pruned > 0 {
            env.events().publish(
                (symbol_short!("mkplc"), symbol_short!("pruned")),
                MerchantsPrunedEvent { count: pruned },
            );
        }

        Ok(pruned)
    }

    // ── Reputation integration ────────────────────────────────────────────────

    /// Admin links (or unlinks) a per-merchant reputation contract.
    pub fn set_merchant_reputation(
        env: Env,
        admin: Address,
        merchant_id: u64,
        reputation: Option<Address>,
    ) -> Result<(), MarketplaceError> {
        Self::require_admin_auth(&env, &admin)?;
        let mut record = Self::load_merchant(&env, merchant_id)?;
        record.reputation = reputation;
        record.updated_at = env.ledger().timestamp();
        Self::save_merchant(&env, merchant_id, &record);
        Ok(())
    }

    /// Admin sets the global reputation contract address (fallback for all merchants).
    pub fn set_reputation_contract(
        env: Env,
        admin: Address,
        reputation: Address,
    ) -> Result<(), MarketplaceError> {
        Self::require_admin_auth(&env, &admin)?;
        env.storage()
            .instance()
            .set(&DataKey::GlobalReputationContract, &reputation);
        Ok(())
    }

    // ── Metadata cooldown ─────────────────────────────────────────────────────

    /// Admin sets the metadata update cooldown (in seconds). Clamped to [60s, 30d].
    pub fn set_metadata_cooldown(
        env: Env,
        admin: Address,
        cooldown_seconds: u64,
    ) -> Result<(), MarketplaceError> {
        Self::require_admin_auth(&env, &admin)?;
        if !(MIN_COOLDOWN_SECONDS..=MAX_COOLDOWN_SECONDS).contains(&cooldown_seconds) {
            return Err(MarketplaceError::InvalidCooldown);
        }
        env.storage()
            .instance()
            .set(&DataKey::MetadataCooldown, &cooldown_seconds);
        Ok(())
    }

    /// Returns the current metadata update cooldown in seconds.
    pub fn get_metadata_cooldown(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::MetadataCooldown)
            .unwrap_or(DEFAULT_COOLDOWN_SECONDS)
    }

    // ── Admin management (two-step) ───────────────────────────────────────────

    /// Current admin proposes a new admin. Returns `true` when a new proposal
    /// is stored, `false` if the proposal was already set to the same address.
    pub fn propose_admin(
        env: Env,
        current_admin: Address,
        new_admin: Address,
    ) -> Result<bool, MarketplaceError> {
        Self::require_admin_auth(&env, &current_admin)?;

        let existing: Option<Address> = env.storage().instance().get(&DataKey::PendingAdmin);
        if existing.as_ref() == Some(&new_admin) {
            return Ok(false);
        }
        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("adm_prop")),
            AdminProposedEvent {
                current_admin,
                proposed_admin: new_admin,
            },
        );
        Ok(true)
    }

    /// Proposed admin accepts the transfer, completing it atomically.
    pub fn accept_admin(env: Env, caller: Address) -> Result<(), MarketplaceError> {
        caller.require_auth();
        let pending: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .ok_or(MarketplaceError::NoPendingAdmin)?;

        if pending != caller {
            return Err(MarketplaceError::NotPendingAdmin);
        }

        env.storage().instance().set(&DataKey::Admin, &pending);
        env.storage().instance().remove(&DataKey::PendingAdmin);

        env.events().publish(
            (symbol_short!("mkplc"), symbol_short!("adm_acc")),
            AdminAcceptedEvent { new_admin: caller },
        );
        Ok(())
    }

    /// Returns the current admin address.
    pub fn get_admin(env: Env) -> Result<Address, MarketplaceError> {
        Self::get_admin_address(&env)
    }

    // ── Version ───────────────────────────────────────────────────────────────

    /// Returns the contract version.
    pub fn version(_env: Env) -> ContractVersion {
        ContractVersion {
            major: 0,
            minor: 2,
            patch: 0,
            semver: symbol_short!("0_2_0"),
        }
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn require_initialized(env: &Env) -> Result<(), MarketplaceError> {
        if !env.storage().instance().has(&DataKey::Admin) {
            return Err(MarketplaceError::NotInitialized);
        }
        Ok(())
    }

    fn require_admin_auth(env: &Env, caller: &Address) -> Result<(), MarketplaceError> {
        caller.require_auth();
        let admin = Self::get_admin_address(env)?;
        if &admin != caller {
            return Err(MarketplaceError::Unauthorized);
        }
        Ok(())
    }

    fn get_admin_address(env: &Env) -> Result<Address, MarketplaceError> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(MarketplaceError::NotInitialized)
    }

    fn load_merchant(env: &Env, merchant_id: u64) -> Result<Merchant, MarketplaceError> {
        env.storage()
            .persistent()
            .get(&DataKey::Merchant(merchant_id))
            .ok_or(MarketplaceError::MerchantNotFound)
    }

    fn save_merchant(env: &Env, merchant_id: u64, record: &Merchant) {
        let mk = DataKey::Merchant(merchant_id);
        env.storage().persistent().set(&mk, record);
        env.storage()
            .persistent()
            .extend_ttl(&mk, PERSISTENT_TTL_LEDGERS, PERSISTENT_TTL_LEDGERS);
    }

    fn check_not_frozen(record: &Merchant) -> Result<(), MarketplaceError> {
        if record.status == MerchantStatus::Suspended || record.status == MerchantStatus::Closed {
            return Err(MarketplaceError::MerchantFrozen);
        }
        Ok(())
    }

    fn is_registered_verifier(env: &Env, address: &Address) -> bool {
        let verifiers: Vec<Verifier> = env
            .storage()
            .instance()
            .get(&DataKey::Verifiers)
            .unwrap_or(Vec::new(env));
        for v in verifiers.iter() {
            if &v.address == address {
                return true;
            }
        }
        false
    }

    fn append_to_merchant_ids(env: &Env, id: u64) {
        let key = DataKey::MerchantIds;
        let mut ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(Vec::new(env));
        ids.push_back(id);
        env.storage().persistent().set(&key, &ids);
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_TTL_LEDGERS, PERSISTENT_TTL_LEDGERS);
    }

    fn append_to_category_index(env: &Env, category: &Symbol, id: u64) {
        let key = DataKey::CategoryIndex(category.clone());
        let mut ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(Vec::new(env));
        ids.push_back(id);
        env.storage().persistent().set(&key, &ids);
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_TTL_LEDGERS, PERSISTENT_TTL_LEDGERS);
    }

    /// Attempt a cross-contract call to the reputation contract to snapshot
    /// the current score. Returns `None` on any error (missing contract,
    /// uninitialised entity, etc.) so discovery queries are never blocked.
    fn fetch_reputation_score(env: &Env, record: &Merchant) -> Option<u32> {
        // Per-merchant override takes precedence over the global contract.
        let rep_address = record.reputation.clone().or_else(|| {
            env.storage()
                .instance()
                .get(&DataKey::GlobalReputationContract)
        })?;

        // Cross-contract call: reputation_contract.get_reputation(entity)
        // Use try_invoke_contract so any error (missing contract, NotFound, etc.)
        // is surfaced as Err rather than panicking, and we return None gracefully.
        use soroban_sdk::IntoVal;
        let args = soroban_sdk::vec![env, record.owner.clone().into_val(env)];

        // We decode the result as a raw Val and extract average_rating from
        // the contracttype map encoding. Any decode failure returns None.
        let call_result = env.try_invoke_contract::<soroban_sdk::Val, InvokeError>(
            &rep_address,
            &soroban_sdk::Symbol::new(env, "get_reputation"),
            args,
        );

        match call_result {
            Ok(Ok(val)) => Self::extract_average_rating(env, val),
            _ => None,
        }
    }

    /// Extract `average_rating` from a `ReputationScore` contracttype value.
    /// The Soroban SDK encodes contracttype structs as a map from field-name
    /// Symbol to value. We look up the `average_rating` key.
    fn extract_average_rating(env: &Env, val: soroban_sdk::Val) -> Option<u32> {
        use soroban_sdk::{IntoVal, TryFromVal};
        let map =
            soroban_sdk::Map::<soroban_sdk::Val, soroban_sdk::Val>::try_from_val(env, &val).ok()?;
        let key: soroban_sdk::Val = soroban_sdk::Symbol::new(env, "average_rating").into_val(env);
        let avg_val = map.get(key)?;
        u32::try_from_val(env, &avg_val).ok()
    }
}

/// Helper on RegisterParams to check commission without needing the full record.
impl RegisterParams {
    fn commission_rate_bps_exceeds_max(&self) -> bool {
        false // commission is not in RegisterParams; default is 0
    }
}

#[cfg(test)]
mod test;
