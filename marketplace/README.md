# Delego Marketplace Contract

The Marketplace Contract maintains a trusted on-chain registry of merchants on the Stellar network with multi-verifier verification, category and name discovery, commission configuration, metadata cooldown enforcement, status lifecycle controls, and reputation score snapshot integration.

## Features

- **Merchant Registration**: Register merchants with unique names, categories, descriptions, image URLs, and anchored metadata hashes (e.g. IPFS CID).
- **Multi-Verifier Verification**: Configure the number of distinct verifiers required (`required_verifications`) before a merchant status transitions to `Verified`.
- **Paginated Discovery**: Client-queryable paginated listing by category (`get_merchants_by_category`) and global list (`get_merchants`).
- **Commission Configuration**: Configurable per-merchant fee in basis points (`0 - 10000 bps`) settable by the merchant owner or platform admin.
- **Metadata Cooldown Lock**: Self-update of metadata by merchant owner with a configurable lockout cooldown period (`MetadataCooldown`), bypassable by admin in emergencies.
- **Security & Lifecycle Moderation**: Admin suspension (`suspend_merchant`), un-suspension (`unsuspend_merchant`), and permanent removal (`close_merchant`), completely blocking mutating operations on frozen or closed merchants.
- **Reputation Integration**: Read-only reference to scoring from the Delego Reputation Contract, snapshots dynamically into `MerchantView.reputation_score`.
- **Two-Step Admin Transfer**: Secure transfer of administrative rights via `propose_admin` and `accept_admin`.

## Contract Interface

### Initialization
```rust
pub fn __constructor(env: Env, admin: Address) -> Result<(), MarketplaceError>
```

### Merchant Lifecycle
```rust
pub fn register_merchant(env: Env, merchant: Address, params: RegisterParams) -> Result<u64, MarketplaceError>
pub fn update_merchant_profile(env: Env, merchant_id: u64, caller: Address, name: String, description: String, image_url: String) -> Result<(), MarketplaceError>
pub fn update_metadata(env: Env, merchant_id: u64, caller: Address, new_metadata: Option<String>) -> Result<(), MarketplaceError>
```

### Verification
```rust
pub fn add_verifier(env: Env, admin: Address, verifier: Verifier) -> Result<(), MarketplaceError>
pub fn remove_verifier(env: Env, admin: Address, verifier: Address) -> Result<(), MarketplaceError>
pub fn verify_merchant(env: Env, merchant_id: u64, verifier: Address) -> Result<(), MarketplaceError>
pub fn revoke_verification(env: Env, admin: Address, merchant_id: u64) -> Result<(), MarketplaceError>
```

### Discovery & Query
```rust
pub fn get_merchant(env: Env, merchant_id: u64) -> Result<Merchant, MarketplaceError>
pub fn get_merchant_view(env: Env, merchant_id: u64) -> Result<MerchantView, MarketplaceError>
pub fn get_merchants(env: Env, offset: u32, limit: u32) -> Result<Vec<MerchantView>, MarketplaceError>
pub fn get_merchants_by_category(env: Env, category: Symbol, offset: u32, limit: u32) -> Result<Vec<MerchantView>, MarketplaceError>
```

### Commission
```rust
pub fn set_merchant_commission(env: Env, merchant_id: u64, caller: Address, commission_bps: u32) -> Result<(), MarketplaceError>
pub fn get_commission(env: Env, merchant_id: u64) -> Result<u32, MarketplaceError>
```

### Moderation & Maintenance
```rust
pub fn suspend_merchant(env: Env, admin: Address, merchant_id: u64) -> Result<(), MarketplaceError>
pub fn unsuspend_merchant(env: Env, admin: Address, merchant_id: u64) -> Result<(), MarketplaceError>
pub fn close_merchant(env: Env, admin: Address, merchant_id: u64, reason: Symbol) -> Result<(), MarketplaceError>
pub fn prune_closed_merchants(env: Env, admin: Address, merchant_ids: Vec<u64>) -> Result<u32, MarketplaceError>
```

### Reputation & Admin
```rust
pub fn set_merchant_reputation(env: Env, admin: Address, merchant_id: u64, reputation: Option<Address>) -> Result<(), MarketplaceError>
pub fn set_reputation_contract(env: Env, admin: Address, reputation: Address) -> Result<(), MarketplaceError>
pub fn propose_admin(env: Env, current_admin: Address, new_admin: Address) -> Result<bool, MarketplaceError>
pub fn accept_admin(env: Env, caller: Address) -> Result<(), MarketplaceError>
pub fn set_metadata_cooldown(env: Env, admin: Address, cooldown_seconds: u64) -> Result<(), MarketplaceError>
pub fn get_metadata_cooldown(env: Env) -> u64
pub fn get_admin(env: Env) -> Result<Address, MarketplaceError>
pub fn get_verifiers(env: Env) -> Vec<Verifier>
pub fn version(env: Env) -> ContractVersion
```

## Discovery Implementation Notes

`get_merchants` and `get_merchants_by_category` iterate monotonic identifier indices stored in persistent state and slice with `offset` and bounded `limit`. Note that a full index refactor (such as status buckets or ordered key-value trees) is planned for high-throughput scaling and tracked in issue #23.
