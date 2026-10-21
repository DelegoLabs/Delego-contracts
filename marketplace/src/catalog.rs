//! Bulk Listing Registration for merchant storefronts (issue #559)
//!
//! Decentralised storefronts need a way to register many listings in one
//! transaction to avoid the gas overhead of separate calls per item.
//! This module exposes [`batch_register_listings`], which atomically registers
//! up to [`MAX_BATCH_SIZE`] listings in a single invocation.
//!
//! ## All-or-nothing atomicity
//!
//! Every listing in the batch is validated **before** any storage write is
//! committed.  If any single item fails validation the entire call returns an
//! error and the contract state is left unchanged.  Soroban's single-threaded
//! execution model guarantees there are no partial writes between the
//! validation pass and the write pass within one invocation.

use crate::{
    DataKey, MarketplaceContract, MarketplaceError, PERSISTENT_BUMP_AMOUNT,
    PERSISTENT_BUMP_THRESHOLD,
};
use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol, Vec};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of listings that may be registered in a single batch call.
/// Exceeding this limit causes the entire call to revert with
/// [`MarketplaceError::BatchTooLarge`].
pub const MAX_BATCH_SIZE: u32 = 20;

/// Maximum byte length for a listing SKU identifier.
const MAX_SKU_ID_LEN: u32 = 32;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Parameters for a single listing inside a batch registration call.
///
/// Each `ListingParams` describes one product / digital-good entry:
/// its merchant-scoped identifier, the unit price in the smallest
/// denomination of `token`, the payment token contract address, and the
/// opening stock count.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListingParams {
    /// Merchant-scoped unique identifier for this listing (≤ 32 chars).
    pub sku_id: Symbol,
    /// Unit price in the smallest denomination of `token`.  Must be > 0.
    pub price: i128,
    /// Token contract used for payments against this listing.
    pub token: Address,
    /// Opening available stock count.
    pub initial_stock: u64,
}

/// On-chain record for a registered listing created via [`batch_register_listings`].
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListingRecord {
    /// Merchant-scoped unique identifier, mirrors [`ListingParams::sku_id`].
    pub sku_id: Symbol,
    /// ID of the owning merchant.
    pub merchant_id: u64,
    /// Unit price in the smallest denomination of `token`.
    pub price: i128,
    /// Token contract used for payments.
    pub token: Address,
    /// Current available stock count.
    pub available_stock: u64,
    /// Whether this listing is currently open for purchase.
    pub is_active: bool,
    /// Ledger timestamp at which this listing was created.
    pub created_at: u64,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Emitted once per successful [`batch_register_listings`] call.
#[contracttype]
#[derive(Clone, Debug)]
pub struct BatchListingsRegisteredEvent {
    /// Merchant that owns the new listings.
    pub merchant_id: u64,
    /// Number of listings registered in this batch.
    pub count: u32,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Atomically register up to [`MAX_BATCH_SIZE`] listings for `merchant`.
///
/// ## Parameters
/// - `env`      — Soroban execution environment.
/// - `merchant` — Address of the merchant owner; must match the registered
///                owner of the resolved merchant record.
/// - `listings` — Batch of [`ListingParams`] to register, `1..=20` items.
///
/// ## Validation (all checked before any write)
/// - `listings.len() > MAX_BATCH_SIZE`  → [`MarketplaceError::BatchTooLarge`]
/// - `listings.len() == 0`              → returns `Ok(0)` (no-op)
/// - `listing.price <= 0`              → [`MarketplaceError::InvalidSku`]
/// - `listing.sku_id` empty or > 32 chars → [`MarketplaceError::InvalidSku`]
/// - duplicate `sku_id` within the batch → [`MarketplaceError::InvalidSku`]
/// - `sku_id` already registered        → [`MarketplaceError::InvalidSku`]
/// - `merchant` not registered          → [`MarketplaceError::MerchantNotFound`]
/// - `merchant` not the owner           → [`MarketplaceError::Unauthorized`]
///
/// ## Returns
/// `Ok(count)` — the number of listings successfully registered (equals
/// `listings.len()` on success).
pub fn batch_register_listings(
    env: Env,
    merchant: Address,
    listings: Vec<ListingParams>,
) -> Result<u32, MarketplaceError> {
    merchant.require_auth();

    // --- 1. Enforce batch size cap (fail before touching storage) -----------
    if listings.len() > MAX_BATCH_SIZE {
        return Err(MarketplaceError::BatchTooLarge);
    }
    if listings.len() == 0 {
        return Ok(0);
    }

    // --- 2. Resolve merchant_id and verify ownership -----------------------
    let merchant_id: u64 = env
        .storage()
        .persistent()
        .get(&DataKey::MerchantOwner(merchant.clone()))
        .ok_or(MarketplaceError::MerchantNotFound)?;

    let record = MarketplaceContract::get_merchant(env.clone(), merchant_id)?;
    if record.owner.as_ref() != Some(&merchant) {
        return Err(MarketplaceError::Unauthorized);
    }

    // --- 3. Validate ALL listings first (all-or-nothing atomicity) ----------
    //
    // We also collect a temporary list of the sku_ids seen so far in this
    // batch to catch intra-batch duplicates without needing storage reads.
    let mut seen: Vec<Symbol> = Vec::new(&env);

    for listing in listings.iter() {
        // Price must be positive.
        if listing.price <= 0 {
            return Err(MarketplaceError::InvalidSku);
        }

        // SKU length must be within bounds.  Symbol has no direct `.len()` so
        // we serialise to XDR and inspect the embedded length field.
        let sku_byte_len = sku_id_byte_len(&env, &listing.sku_id);
        if sku_byte_len == 0 || sku_byte_len > MAX_SKU_ID_LEN {
            return Err(MarketplaceError::InvalidSku);
        }

        // Reject intra-batch duplicates.
        for prior in seen.iter() {
            if prior == listing.sku_id {
                return Err(MarketplaceError::InvalidSku);
            }
        }
        seen.push_back(listing.sku_id.clone());

        // Reject listings that already exist in persistent storage.
        let key = DataKey::ListingRecord(merchant_id, listing.sku_id.clone());
        if env.storage().persistent().has(&key) {
            return Err(MarketplaceError::InvalidSku);
        }
    }

    // --- 4. Write phase (validation passed — safe to commit all items) ------
    let now = env.ledger().timestamp();

    // Load the merchant's existing catalog list (or start an empty one).
    let list_key = DataKey::MerchantListingSkuList(merchant_id);
    let mut sku_list: Vec<Symbol> = env
        .storage()
        .persistent()
        .get(&list_key)
        .unwrap_or_else(|| Vec::new(&env));

    let count = listings.len();

    for listing in listings.iter() {
        let listing_record = ListingRecord {
            sku_id: listing.sku_id.clone(),
            merchant_id,
            price: listing.price,
            token: listing.token.clone(),
            available_stock: listing.initial_stock,
            is_active: true,
            created_at: now,
        };

        let key = DataKey::ListingRecord(merchant_id, listing.sku_id.clone());
        env.storage().persistent().set(&key, &listing_record);
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        sku_list.push_back(listing.sku_id.clone());
    }

    // Persist the updated catalog index.
    env.storage().persistent().set(&list_key, &sku_list);
    env.storage().persistent().extend_ttl(
        &list_key,
        PERSISTENT_BUMP_THRESHOLD,
        PERSISTENT_BUMP_AMOUNT,
    );

    // Emit a single summary event for the whole batch.
    env.events().publish(
        (
            symbol_short!("mkplc"),
            symbol_short!("bulk_lst"),
            merchant_id,
        ),
        BatchListingsRegisteredEvent { merchant_id, count },
    );

    Ok(count)
}

/// Return the [`ListingRecord`] for `(merchant_id, sku_id)`, or
/// [`MarketplaceError::SkuNotFound`] if it does not exist.
pub fn get_listing(
    env: Env,
    merchant_id: u64,
    sku_id: Symbol,
) -> Result<ListingRecord, MarketplaceError> {
    env.storage()
        .persistent()
        .get(&DataKey::ListingRecord(merchant_id, sku_id))
        .ok_or(MarketplaceError::SkuNotFound)
}

/// Return all SKU IDs in the catalog for `merchant_id`.
pub fn get_merchant_listings(env: Env, merchant_id: u64) -> Vec<Symbol> {
    env.storage()
        .persistent()
        .get(&DataKey::MerchantListingSkuList(merchant_id))
        .unwrap_or_else(|| Vec::new(&env))
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Measure the byte length of a [`Symbol`] by inspecting its XDR encoding.
///
/// Returns 0 for an empty symbol.  Soroban `Symbol` values are limited to 32
/// characters by the SDK, so the byte length is always ≤ 32.
fn sku_id_byte_len(env: &Env, sym: &Symbol) -> u32 {
    use soroban_sdk::xdr::ToXdr;
    let xdr = sym.clone().to_xdr(env);
    let xdr_len = xdr.len() as usize;
    if xdr_len < 8 {
        return 0;
    }
    // XDR Symbol is: 4-byte tag + 4-byte length + chars + padding.
    // Copy into a 48-byte buffer (max XDR for a 32-char Symbol = 8 + 32 + 3 = 43).
    let mut buf = [0u8; 48];
    let copy_len = xdr_len.min(48);
    xdr.copy_into_slice(&mut buf[..copy_len]);
    u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MarketplaceContract, MarketplaceContractClient, RegisterParams};
    use soroban_sdk::{symbol_short, testutils::Address as _, Address, Env, String};

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    struct Ctx {
        env: Env,
        admin: Address,
        owner: Address,
        contract_id: Address,
        client: MarketplaceContractClient<'static>,
        merchant_id: u64,
        token: Address,
    }

    fn setup() -> Ctx {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let contract_id = env.register(MarketplaceContract, (admin.clone(),));
        let client = MarketplaceContractClient::new(&env, &contract_id);

        let owner = Address::generate(&env);
        let merchant_id = crate::testutil::register_merchant_with_deposit(
            &env,
            &client,
            &owner,
            &RegisterParams {
                name: String::from_str(&env, "Bulk Merchant"),
                description: String::from_str(&env, "Merchant for bulk listing tests"),
                category: symbol_short!("retail"),
                image_url: String::from_str(&env, "https://example.com/img.png"),
                metadata: None,
                metadata_uri: None,
                required_verifications: 0,
            },
        );

        let token = Address::generate(&env);
        Ctx {
            env,
            admin,
            owner,
            contract_id,
            client,
            merchant_id,
            token,
        }
    }

    /// Build a single valid [`ListingParams`] with a given sku symbol.
    fn listing(env: &Env, sku: Symbol, token: &Address) -> ListingParams {
        ListingParams {
            sku_id: sku,
            price: 100,
            token: token.clone(),
            initial_stock: 50,
        }
    }

    /// Build a `Vec<ListingParams>` with `n` items having sku names "sku_0".."sku_n".
    fn make_batch(env: &Env, n: u32, token: &Address) -> Vec<ListingParams> {
        // Symbol names must be ≤ 9 chars for symbol_short!, so we construct
        // them manually via Symbol::new with short names "s0".."s19".
        let mut v: Vec<ListingParams> = Vec::new(env);
        for i in 0..n {
            // Build a short unique name "s0", "s1", ..., "s19"
            let name = match i {
                0 => "s0", 1 => "s1", 2 => "s2", 3 => "s3", 4 => "s4",
                5 => "s5", 6 => "s6", 7 => "s7", 8 => "s8", 9 => "s9",
                10 => "s10", 11 => "s11", 12 => "s12", 13 => "s13", 14 => "s14",
                15 => "s15", 16 => "s16", 17 => "s17", 18 => "s18", 19 => "s19",
                _ => "sX",
            };
            v.push_back(listing(env, Symbol::new(env, name), token));
        }
        v
    }

    // -----------------------------------------------------------------------
    // Happy-path
    // -----------------------------------------------------------------------

    #[test]
    fn batch_registers_single_listing() {
        let ctx = setup();
        let listings = {
            let mut v = Vec::new(&ctx.env);
            v.push_back(listing(&ctx.env, symbol_short!("widget"), &ctx.token));
            v
        };
        let count = ctx.client.batch_register_listings(&ctx.owner, &listings);
        assert_eq!(count, 1);

        let rec = ctx
            .client
            .get_listing(&ctx.merchant_id, &symbol_short!("widget"));
        assert_eq!(rec.price, 100);
        assert_eq!(rec.available_stock, 50);
        assert!(rec.is_active);

        let catalog = ctx.client.get_merchant_listings(&ctx.merchant_id);
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog.get(0).unwrap(), symbol_short!("widget"));
    }

    #[test]
    fn batch_registers_max_20_listings() {
        let ctx = setup();
        let listings = make_batch(&ctx.env, 20, &ctx.token);
        let count = ctx.client.batch_register_listings(&ctx.owner, &listings);
        assert_eq!(count, 20);

        let catalog = ctx.client.get_merchant_listings(&ctx.merchant_id);
        assert_eq!(catalog.len(), 20);
    }

    #[test]
    fn batch_empty_returns_zero() {
        let ctx = setup();
        let empty: Vec<ListingParams> = Vec::new(&ctx.env);
        let count = ctx.client.batch_register_listings(&ctx.owner, &empty);
        assert_eq!(count, 0);
    }

    #[test]
    fn batch_accumulates_across_multiple_calls() {
        let ctx = setup();
        let first = {
            let mut v = Vec::new(&ctx.env);
            v.push_back(listing(&ctx.env, symbol_short!("a"), &ctx.token));
            v
        };
        let second = {
            let mut v = Vec::new(&ctx.env);
            v.push_back(listing(&ctx.env, symbol_short!("b"), &ctx.token));
            v.push_back(listing(&ctx.env, symbol_short!("c"), &ctx.token));
            v
        };
        ctx.client.batch_register_listings(&ctx.owner, &first);
        ctx.client.batch_register_listings(&ctx.owner, &second);

        let catalog = ctx.client.get_merchant_listings(&ctx.merchant_id);
        assert_eq!(catalog.len(), 3);
    }

    // -----------------------------------------------------------------------
    // Batch size enforcement
    // -----------------------------------------------------------------------

    #[test]
    fn batch_exceeding_20_returns_batch_too_large() {
        let ctx = setup();
        // Build 21 listings (one over the cap).
        let mut listings = make_batch(&ctx.env, 20, &ctx.token);
        listings.push_back(listing(&ctx.env, symbol_short!("extra"), &ctx.token));

        assert_eq!(
            ctx.client
                .try_batch_register_listings(&ctx.owner, &listings),
            Err(Ok(MarketplaceError::BatchTooLarge))
        );
    }

    // -----------------------------------------------------------------------
    // Authorization
    // -----------------------------------------------------------------------

    #[test]
    fn non_owner_is_rejected() {
        let ctx = setup();
        let stranger = Address::generate(&ctx.env);
        let listings = {
            let mut v = Vec::new(&ctx.env);
            v.push_back(listing(&ctx.env, symbol_short!("x"), &ctx.token));
            v
        };
        // Stranger is not a registered merchant → MerchantNotFound.
        assert_eq!(
            ctx.client
                .try_batch_register_listings(&stranger, &listings),
            Err(Ok(MarketplaceError::MerchantNotFound))
        );
    }

    #[test]
    fn unregistered_merchant_returns_not_found() {
        let ctx = setup();
        let nobody = Address::generate(&ctx.env);
        let listings = {
            let mut v = Vec::new(&ctx.env);
            v.push_back(listing(&ctx.env, symbol_short!("x"), &ctx.token));
            v
        };
        assert_eq!(
            ctx.client
                .try_batch_register_listings(&nobody, &listings),
            Err(Ok(MarketplaceError::MerchantNotFound))
        );
    }

    // -----------------------------------------------------------------------
    // Per-listing validation — all-or-nothing (batch fails if any item fails)
    // -----------------------------------------------------------------------

    #[test]
    fn zero_price_in_batch_rejects_entire_batch() {
        let ctx = setup();
        let listings = {
            let mut v = Vec::new(&ctx.env);
            v.push_back(listing(&ctx.env, symbol_short!("good"), &ctx.token));
            // Second item has zero price → should fail the whole batch.
            v.push_back(ListingParams {
                sku_id: symbol_short!("bad"),
                price: 0,
                token: ctx.token.clone(),
                initial_stock: 10,
            });
            v
        };
        assert_eq!(
            ctx.client
                .try_batch_register_listings(&ctx.owner, &listings),
            Err(Ok(MarketplaceError::InvalidSku))
        );
        // "good" must NOT have been written (all-or-nothing).
        assert_eq!(
            ctx.client
                .try_get_listing(&ctx.merchant_id, &symbol_short!("good")),
            Err(Ok(MarketplaceError::SkuNotFound))
        );
    }

    #[test]
    fn negative_price_in_batch_rejects_entire_batch() {
        let ctx = setup();
        let listings = {
            let mut v = Vec::new(&ctx.env);
            v.push_back(ListingParams {
                sku_id: symbol_short!("neg"),
                price: -50,
                token: ctx.token.clone(),
                initial_stock: 5,
            });
            v
        };
        assert_eq!(
            ctx.client
                .try_batch_register_listings(&ctx.owner, &listings),
            Err(Ok(MarketplaceError::InvalidSku))
        );
    }

    #[test]
    fn intra_batch_duplicate_sku_rejects_entire_batch() {
        let ctx = setup();
        let listings = {
            let mut v = Vec::new(&ctx.env);
            v.push_back(listing(&ctx.env, symbol_short!("dup"), &ctx.token));
            // Same sku_id a second time.
            v.push_back(listing(&ctx.env, symbol_short!("dup"), &ctx.token));
            v
        };
        assert_eq!(
            ctx.client
                .try_batch_register_listings(&ctx.owner, &listings),
            Err(Ok(MarketplaceError::InvalidSku))
        );
    }

    #[test]
    fn pre_existing_sku_rejects_entire_batch() {
        let ctx = setup();
        // Register "exist" first.
        let first = {
            let mut v = Vec::new(&ctx.env);
            v.push_back(listing(&ctx.env, symbol_short!("exist"), &ctx.token));
            v
        };
        ctx.client.batch_register_listings(&ctx.owner, &first);

        // Second batch tries to re-register "exist" alongside a new item.
        let second = {
            let mut v = Vec::new(&ctx.env);
            v.push_back(listing(&ctx.env, symbol_short!("new"), &ctx.token));
            v.push_back(listing(&ctx.env, symbol_short!("exist"), &ctx.token));
            v
        };
        assert_eq!(
            ctx.client
                .try_batch_register_listings(&ctx.owner, &second),
            Err(Ok(MarketplaceError::InvalidSku))
        );
        // "new" must NOT have been committed.
        assert_eq!(
            ctx.client
                .try_get_listing(&ctx.merchant_id, &symbol_short!("new")),
            Err(Ok(MarketplaceError::SkuNotFound))
        );
    }

    // -----------------------------------------------------------------------
    // get_listing / get_merchant_listings
    // -----------------------------------------------------------------------

    #[test]
    fn get_listing_returns_not_found_for_unknown_sku() {
        let ctx = setup();
        assert_eq!(
            ctx.client
                .try_get_listing(&ctx.merchant_id, &symbol_short!("ghost")),
            Err(Ok(MarketplaceError::SkuNotFound))
        );
    }

    #[test]
    fn get_merchant_listings_empty_before_any_batch() {
        let ctx = setup();
        let catalog = ctx.client.get_merchant_listings(&ctx.merchant_id);
        assert_eq!(catalog.len(), 0);
    }
}
