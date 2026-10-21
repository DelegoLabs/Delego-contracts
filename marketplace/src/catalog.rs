//! Product SKU Registry with Stock Decrement Guards (issue #555)
//!
//! Provides on-chain stock tracking for limited-edition physical or digital
//! goods in decentralised storefronts. The two core operations are:
//!
//! * [`register_sku`] – merchant-owner creates a new SKU with an initial
//!   available quantity.
//! * [`decrement_stock`] – atomically reduces the available quantity of a SKU,
//!   callable **only** by the address registered as the authorised escrow
//!   contract ([`DataKey::AuthorizedEscrow`]). This prevents overselling: if
//!   the requested quantity exceeds `available_quantity` the call reverts with
//!   [`MarketplaceError::OutOfStock`].
//!
//! A platform admin configures the authorised escrow address once via
//! [`set_authorized_escrow`]; only that address (or the admin itself) may
//! call [`decrement_stock`].

use crate::{
    DataKey, MarketplaceContract, MarketplaceError, PERSISTENT_BUMP_AMOUNT,
    PERSISTENT_BUMP_THRESHOLD,
};
use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// On-chain record for a merchant's product SKU.
///
/// Mirrors the schema specified in issue #555, using `u32` for quantity so
/// that overflow checks are inexpensive on 32-bit WASM targets.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkuRecord {
    /// Unique identifier for this SKU (merchant-scoped).
    pub sku_id: Symbol,
    /// Unit price in the smallest denomination of `token`.
    pub price: i128,
    /// Token contract used for payments against this SKU.
    pub token: Address,
    /// Current available stock count. Never drops below zero.
    pub available_quantity: u32,
    /// Whether this SKU accepts new orders. Stock decrements are refused when
    /// `is_active` is `false`.
    pub is_active: bool,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Emitted when a new SKU is registered via [`register_sku`].
#[contracttype]
#[derive(Clone, Debug)]
pub struct SkuRegisteredEvent {
    /// Merchant that owns this SKU.
    pub merchant_id: u64,
    /// The SKU identifier.
    pub sku_id: Symbol,
    /// Initial available quantity set at registration time.
    pub initial_quantity: u32,
    /// Unit price in `token` base units.
    pub price: i128,
}

/// Emitted on each successful [`decrement_stock`] call.
#[contracttype]
#[derive(Clone, Debug)]
pub struct SkuStockDecrementedEvent {
    /// Merchant that owns the SKU.
    pub merchant_id: u64,
    /// The SKU identifier.
    pub sku_id: Symbol,
    /// Quantity removed in this call.
    pub quantity: u32,
    /// Available quantity remaining after the decrement.
    pub remaining: u32,
}

// ---------------------------------------------------------------------------
// Public functions
// ---------------------------------------------------------------------------

/// Register a new product SKU for `merchant_id`.
///
/// # Authorization
/// Requires an auth entry from `merchant_owner`, who must be the registered
/// owner of `merchant_id`.
///
/// # Errors
/// * [`MarketplaceError::MerchantNotFound`] – `merchant_id` does not exist.
/// * [`MarketplaceError::Unauthorized`] – caller is not the merchant owner.
/// * [`MarketplaceError::InvalidSku`] – a SKU with `sku_id` already exists for
///   this merchant, or `price` ≤ 0.
pub fn register_sku(
    env: Env,
    merchant_owner: Address,
    merchant_id: u64,
    sku_id: Symbol,
    price: i128,
    token: Address,
    initial_quantity: u32,
) -> Result<(), MarketplaceError> {
    merchant_owner.require_auth();

    // Verify the merchant exists and the caller owns it.
    let merchant = MarketplaceContract::get_merchant(env.clone(), merchant_id)?;
    if merchant.owner.as_ref() != Some(&merchant_owner) {
        return Err(MarketplaceError::Unauthorized);
    }

    // Price must be positive.
    if price <= 0 {
        return Err(MarketplaceError::InvalidSku);
    }

    // Reject duplicate SKU IDs (merchant-scoped).
    let record_key = DataKey::SkuRecord(merchant_id, sku_id.clone());
    if env.storage().persistent().has(&record_key) {
        return Err(MarketplaceError::InvalidSku);
    }

    let record = SkuRecord {
        sku_id: sku_id.clone(),
        price,
        token,
        available_quantity: initial_quantity,
        is_active: true,
    };

    env.storage().persistent().set(&record_key, &record);
    env.storage().persistent().extend_ttl(
        &record_key,
        PERSISTENT_BUMP_THRESHOLD,
        PERSISTENT_BUMP_AMOUNT,
    );

    // Append to the merchant's catalog SKU list.
    let list_key = DataKey::MerchantCatalogSkuList(merchant_id);
    let mut sku_list: soroban_sdk::Vec<Symbol> = env
        .storage()
        .persistent()
        .get(&list_key)
        .unwrap_or_else(|| soroban_sdk::Vec::new(&env));
    sku_list.push_back(sku_id.clone());
    env.storage().persistent().set(&list_key, &sku_list);
    env.storage()
        .persistent()
        .extend_ttl(&list_key, PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);

    env.events().publish(
        (
            symbol_short!("mkplc"),
            symbol_short!("sku_reg"),
            merchant_id,
        ),
        SkuRegisteredEvent {
            merchant_id,
            sku_id,
            initial_quantity,
            price,
        },
    );

    Ok(())
}

/// Atomically decrement the available stock of a SKU.
///
/// This is the guarded hot path for order fulfillment. It prevents overselling
/// by reverting with [`MarketplaceError::OutOfStock`] when
/// `quantity > available_quantity`.
///
/// # Authorization
/// The caller must be **either** the configured authorised escrow contract
/// ([`DataKey::AuthorizedEscrow`]) **or** the platform admin. Any other caller
/// is rejected with [`MarketplaceError::Unauthorized`].
///
/// # Errors
/// * [`MarketplaceError::SkuNotFound`] – no SKU record for `(merchant_id, sku_id)`.
/// * [`MarketplaceError::InvalidSku`] – the SKU exists but `is_active` is `false`.
/// * [`MarketplaceError::OutOfStock`] – `quantity > available_quantity`.
/// * [`MarketplaceError::Unauthorized`] – caller is not the authorised escrow or admin.
///
/// # Returns
/// `Ok(remaining)` – the `available_quantity` left after the decrement.
pub fn decrement_stock(
    env: Env,
    caller: Address,
    merchant_id: u64,
    sku_id: Symbol,
    quantity: u32,
) -> Result<u32, MarketplaceError> {
    caller.require_auth();

    // Only the authorised escrow contract or the admin may decrement stock.
    if !is_authorized_decrementer(&env, &caller)? {
        return Err(MarketplaceError::Unauthorized);
    }

    let record_key = DataKey::SkuRecord(merchant_id, sku_id.clone());
    let mut record: SkuRecord = env
        .storage()
        .persistent()
        .get(&record_key)
        .ok_or(MarketplaceError::SkuNotFound)?;

    // Inactive SKUs do not accept stock decrements.
    if !record.is_active {
        return Err(MarketplaceError::InvalidSku);
    }

    // Guard: requested quantity must not exceed available stock.
    if quantity > record.available_quantity {
        return Err(MarketplaceError::OutOfStock);
    }

    // Atomic decrement – saturating_sub as belt-and-suspenders, but the
    // guard above already guarantees this will not underflow.
    record.available_quantity = record.available_quantity.saturating_sub(quantity);

    env.storage().persistent().set(&record_key, &record);
    env.storage().persistent().extend_ttl(
        &record_key,
        PERSISTENT_BUMP_THRESHOLD,
        PERSISTENT_BUMP_AMOUNT,
    );

    let remaining = record.available_quantity;

    env.events().publish(
        (
            symbol_short!("mkplc"),
            symbol_short!("sku_dec"),
            merchant_id,
        ),
        SkuStockDecrementedEvent {
            merchant_id,
            sku_id,
            quantity,
            remaining,
        },
    );

    Ok(remaining)
}

/// Set (or clear) the authorised escrow contract address.
///
/// # Authorization
/// Admin only.
///
/// Passing `None` removes the authorisation, preventing any escrow from
/// calling [`decrement_stock`] until a new address is set.
pub fn set_authorized_escrow(
    env: Env,
    admin: Address,
    escrow: Option<Address>,
) -> Result<(), MarketplaceError> {
    admin.require_auth();
    if admin != MarketplaceContract::get_admin(env.clone())? {
        return Err(MarketplaceError::Unauthorized);
    }
    match escrow {
        Some(addr) => env
            .storage()
            .instance()
            .set(&DataKey::AuthorizedEscrow, &addr),
        None => env
            .storage()
            .instance()
            .remove(&DataKey::AuthorizedEscrow),
    }
    Ok(())
}

/// Return the currently configured authorised escrow address, if any.
pub fn get_authorized_escrow(env: Env) -> Option<Address> {
    env.storage()
        .instance()
        .get(&DataKey::AuthorizedEscrow)
}

/// Return the [`SkuRecord`] for `(merchant_id, sku_id)`, or
/// [`MarketplaceError::SkuNotFound`] if it does not exist.
pub fn get_sku_record(
    env: Env,
    merchant_id: u64,
    sku_id: Symbol,
) -> Result<SkuRecord, MarketplaceError> {
    env.storage()
        .persistent()
        .get(&DataKey::SkuRecord(merchant_id, sku_id))
        .ok_or(MarketplaceError::SkuNotFound)
}

/// Return the list of all SKU IDs registered for `merchant_id`.
pub fn get_merchant_catalog(env: Env, merchant_id: u64) -> soroban_sdk::Vec<Symbol> {
    env.storage()
        .persistent()
        .get(&DataKey::MerchantCatalogSkuList(merchant_id))
        .unwrap_or_else(|| soroban_sdk::Vec::new(&env))
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Returns `true` if `caller` is either the platform admin or the configured
/// authorised escrow contract.
fn is_authorized_decrementer(env: &Env, caller: &Address) -> Result<bool, MarketplaceError> {
    let admin = MarketplaceContract::get_admin(env.clone())?;
    if caller == &admin {
        return Ok(true);
    }
    let auth_escrow: Option<Address> = env
        .storage()
        .instance()
        .get(&DataKey::AuthorizedEscrow);
    Ok(auth_escrow.as_ref() == Some(caller))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MarketplaceContract, MarketplaceContractClient, RegisterParams};
    use soroban_sdk::{
        symbol_short,
        testutils::Address as _,
        Address, Env, String,
    };

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
        let params = RegisterParams {
            name: String::from_str(&env, "Catalog Merchant"),
            description: String::from_str(&env, "Test merchant for catalog"),
            category: symbol_short!("retail"),
            image_url: String::from_str(&env, "https://example.com/img.png"),
            metadata: None,
            metadata_uri: None,
            required_verifications: 0,
        };
        let merchant_id =
            crate::testutil::register_merchant_with_deposit(&env, &client, &owner, &params);

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

    fn sku(env: &Env) -> Symbol {
        Symbol::new(env, "widget_v1")
    }

    // -----------------------------------------------------------------------
    // register_sku
    // -----------------------------------------------------------------------

    #[test]
    fn register_sku_stores_record_and_emits_event() {
        let ctx = setup();
        ctx.client.register_sku(
            &ctx.owner,
            &ctx.merchant_id,
            &sku(&ctx.env),
            &100i128,
            &ctx.token,
            &50u32,
        );

        let record = ctx
            .client
            .get_sku_record(&ctx.merchant_id, &sku(&ctx.env));
        assert_eq!(record.available_quantity, 50);
        assert_eq!(record.price, 100);
        assert!(record.is_active);

        // SKU appears in the catalog list.
        let catalog = ctx.client.get_merchant_catalog(&ctx.merchant_id);
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog.get(0).unwrap(), sku(&ctx.env));
    }

    #[test]
    fn register_sku_rejects_non_owner() {
        let ctx = setup();
        let other = Address::generate(&ctx.env);
        assert_eq!(
            ctx.client.try_register_sku(
                &other,
                &ctx.merchant_id,
                &sku(&ctx.env),
                &100i128,
                &ctx.token,
                &10u32,
            ),
            Err(Ok(MarketplaceError::Unauthorized))
        );
    }

    #[test]
    fn register_sku_rejects_zero_price() {
        let ctx = setup();
        assert_eq!(
            ctx.client.try_register_sku(
                &ctx.owner,
                &ctx.merchant_id,
                &sku(&ctx.env),
                &0i128,
                &ctx.token,
                &10u32,
            ),
            Err(Ok(MarketplaceError::InvalidSku))
        );
    }

    #[test]
    fn register_sku_rejects_duplicate_sku_id() {
        let ctx = setup();
        ctx.client.register_sku(
            &ctx.owner,
            &ctx.merchant_id,
            &sku(&ctx.env),
            &100i128,
            &ctx.token,
            &10u32,
        );
        assert_eq!(
            ctx.client.try_register_sku(
                &ctx.owner,
                &ctx.merchant_id,
                &sku(&ctx.env),
                &200i128,
                &ctx.token,
                &5u32,
            ),
            Err(Ok(MarketplaceError::InvalidSku))
        );
    }

    // -----------------------------------------------------------------------
    // decrement_stock
    // -----------------------------------------------------------------------

    #[test]
    fn decrement_stock_by_authorized_escrow_succeeds() {
        let ctx = setup();
        let escrow = Address::generate(&ctx.env);

        // Register the escrow as the authorised decrementer.
        ctx.client.set_authorized_escrow(&ctx.admin, &Some(escrow.clone()));

        ctx.client.register_sku(
            &ctx.owner,
            &ctx.merchant_id,
            &sku(&ctx.env),
            &100i128,
            &ctx.token,
            &10u32,
        );

        let remaining = ctx
            .client
            .decrement_stock(&escrow, &ctx.merchant_id, &sku(&ctx.env), &3u32);
        assert_eq!(remaining, 7);

        let record = ctx.client.get_sku_record(&ctx.merchant_id, &sku(&ctx.env));
        assert_eq!(record.available_quantity, 7);
    }

    #[test]
    fn decrement_stock_by_admin_succeeds() {
        let ctx = setup();
        ctx.client.register_sku(
            &ctx.owner,
            &ctx.merchant_id,
            &sku(&ctx.env),
            &100i128,
            &ctx.token,
            &5u32,
        );

        let remaining =
            ctx.client
                .decrement_stock(&ctx.admin, &ctx.merchant_id, &sku(&ctx.env), &2u32);
        assert_eq!(remaining, 3);
    }

    #[test]
    fn decrement_stock_prevents_stock_below_zero() {
        let ctx = setup();
        ctx.client.register_sku(
            &ctx.owner,
            &ctx.merchant_id,
            &sku(&ctx.env),
            &100i128,
            &ctx.token,
            &5u32,
        );

        assert_eq!(
            ctx.client.try_decrement_stock(
                &ctx.admin,
                &ctx.merchant_id,
                &sku(&ctx.env),
                &6u32,
            ),
            Err(Ok(MarketplaceError::OutOfStock))
        );

        // Stock must be unchanged.
        let record = ctx.client.get_sku_record(&ctx.merchant_id, &sku(&ctx.env));
        assert_eq!(record.available_quantity, 5);
    }

    #[test]
    fn decrement_stock_rejects_unauthorized_caller() {
        let ctx = setup();
        ctx.client.register_sku(
            &ctx.owner,
            &ctx.merchant_id,
            &sku(&ctx.env),
            &100i128,
            &ctx.token,
            &5u32,
        );

        let random = Address::generate(&ctx.env);
        assert_eq!(
            ctx.client.try_decrement_stock(
                &random,
                &ctx.merchant_id,
                &sku(&ctx.env),
                &1u32,
            ),
            Err(Ok(MarketplaceError::Unauthorized))
        );
    }

    #[test]
    fn decrement_stock_rejects_inactive_sku() {
        let ctx = setup();
        // Register with zero quantity then deactivate via the storage directly.
        ctx.client.register_sku(
            &ctx.owner,
            &ctx.merchant_id,
            &sku(&ctx.env),
            &100i128,
            &ctx.token,
            &10u32,
        );

        // Deactivate by writing directly to storage.
        ctx.env.as_contract(&ctx.contract_id, || {
            let key = DataKey::SkuRecord(ctx.merchant_id, sku(&ctx.env));
            let mut rec: SkuRecord = ctx.env.storage().persistent().get(&key).unwrap();
            rec.is_active = false;
            ctx.env.storage().persistent().set(&key, &rec);
        });

        assert_eq!(
            ctx.client.try_decrement_stock(
                &ctx.admin,
                &ctx.merchant_id,
                &sku(&ctx.env),
                &1u32,
            ),
            Err(Ok(MarketplaceError::InvalidSku))
        );
    }

    #[test]
    fn decrement_stock_exact_quantity_empties_stock() {
        let ctx = setup();
        ctx.client.register_sku(
            &ctx.owner,
            &ctx.merchant_id,
            &sku(&ctx.env),
            &100i128,
            &ctx.token,
            &3u32,
        );

        let remaining =
            ctx.client
                .decrement_stock(&ctx.admin, &ctx.merchant_id, &sku(&ctx.env), &3u32);
        assert_eq!(remaining, 0);
    }

    // -----------------------------------------------------------------------
    // set / get authorized escrow
    // -----------------------------------------------------------------------

    #[test]
    fn set_and_get_authorized_escrow() {
        let ctx = setup();
        assert_eq!(ctx.client.get_authorized_escrow(), None);

        let escrow = Address::generate(&ctx.env);
        ctx.client.set_authorized_escrow(&ctx.admin, &Some(escrow.clone()));
        assert_eq!(ctx.client.get_authorized_escrow(), Some(escrow));

        // Clear it.
        ctx.client.set_authorized_escrow(&ctx.admin, &None);
        assert_eq!(ctx.client.get_authorized_escrow(), None);
    }

    #[test]
    fn set_authorized_escrow_rejects_non_admin() {
        let ctx = setup();
        let other = Address::generate(&ctx.env);
        let escrow = Address::generate(&ctx.env);
        assert_eq!(
            ctx.client
                .try_set_authorized_escrow(&other, &Some(escrow)),
            Err(Ok(MarketplaceError::Unauthorized))
        );
    }
}
