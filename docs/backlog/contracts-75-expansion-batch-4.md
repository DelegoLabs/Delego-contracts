# Smart Contracts 75 Expansion Issues Backlog (Batch 4) (75 Issues)

Developer-ready smart contract issues covering compilation and syntax error fixes, Soroban SDK version alignment, core escrow settlement logic, merchant staking bonds, tiered verification state machines, granular delegation permissions, reputation weighting, and Blend DeFi yield protocol adapters.

**Total Issues:** 75
**Target Sizing:** 1–3 developer-days per issue.
**Prerequisites:** All issues include explicit Soroban Rust data types, traits, tasks, and acceptance criteria.

---

## Fix Mismatched Closing Delimiters and Syntax Errors in escrow/src/lib.rs

- **Estimate:** 2 days
- **Context:** Compiling `delego-escrow` fails with 7 syntax errors caused by conflicted merges across lines 8837, 13571, 13781, 14251, 15834, 16323, and 16768. Missing closing braces in helper functions and test modules (`multi_oracle_tests`, `min_fee_floor_tests`, `approve_emergency_rescue_requires_admin`) break the entire crate AST.

### Data Types & Schemas
```rust
pub trait EscrowInterface {
    fn update_merchant_volume_and_tier(env: &Env, merchant: &Address, amount: i128) -> Result<bool, EscrowError>;
    fn calculate_tiered_fee(env: &Env, merchant: &Address, amount: i128) -> Result<i128, EscrowError>;
    fn approve_emergency_rescue(env: &Env, admin: &Address, token: &Address) -> Result<bool, EscrowError>;
}
```

### Tasks
- Inspect `escrow/src/lib.rs` and reconcile mismatched braces and parens at lines 8837, 13571, 13781, 14251, 15834, and 16323.
- Remove dangling delimiters and merge artifacts at EOF line 16768.
- Run `cargo check -p delego-escrow` and ensure clean syntax parsing.

### Acceptance Criteria
- `cargo check -p delego-escrow` parses without syntax errors or unclosed delimiter panics.
- All test modules compile and pass syntax validation.

---

## Fix Unclosed Delimiter in marketplace/src/lib.rs Test Harness

- **Estimate:** 1 day
- **Context:** `cargo check -p delego-marketplace` fails with `error: this file contains an unclosed delimiter` at `marketplace/src/lib.rs:5311:3`. The test module `error_code_uniqueness_tests` starting at line 4053 contains an unclosed function bracket around line 4062 that throws off brace matching.

### Data Types & Schemas
```rust
#[cfg(test)]
mod error_code_uniqueness_tests {
    use super::*;

    fn marketplace_error_codes() -> [u32; 21] {
        [
            MarketplaceError::MerchantNotFound as u32,
            MarketplaceError::AlreadyRegistered as u32,
            MarketplaceError::InvalidBondAmount as u32,
            // ...
        ]
    }
}
```

### Tasks
- Audit `marketplace/src/lib.rs` from lines 4050 to 4150.
- Fix the indentation and matching brace for `fn marketplace_error_codes()`.
- Verify `cargo check -p delego-marketplace` succeeds.

### Acceptance Criteria
- `delego-marketplace` compiles without syntax or delimiter errors.
- The error code uniqueness unit test runs and passes.

---

## Fix Duplicate VerificationGracePeriod Enum Variant in permissions/src/lib.rs

- **Estimate:** 1 day
- **Context:** In `permissions/src/lib.rs`, `PermissionError` defines `VerificationGracePeriod` twice: once at line 223 (`VerificationGracePeriod = 2415`) and again at line 241 (`VerificationGracePeriod = 2421`), causing E0428, E0081 (discriminant collisions), and E0004 (non-exhaustive pattern errors under `#[contracterror]`).

### Data Types & Schemas
```rust
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum PermissionError {
    PermissionNotFound = 2302,
    NotFound = 2001,
    VerificationGracePeriod = 2415,
    ProposalAlreadyApproved = 2421,
    EphemeralRelayerUnsupported = 2425,
    // Duplicate 2421 removed
}
```

### Tasks
- Remove duplicate `VerificationGracePeriod = 2421` variant at line 241 of `permissions/src/lib.rs`.
- Verify discriminant 2421 belongs solely to `ProposalAlreadyApproved`.
- Run `cargo check -p delego-permissions` to confirm resolution of E0428 and E0004.

### Acceptance Criteria
- Enum variant is defined exactly once with consistent discriminant.
- `cargo check -p delego-permissions` completes without error code collisions.

---

## Fix Duplicate NotFlagReporter Variant Definition in reputation/src/lib.rs

- **Estimate:** 1 day
- **Context:** `cargo check -p delego-reputation` fails with `error[E0428]: the name 'NotFlagReporter' is defined multiple times` in `reputation/src/lib.rs`. Line 318 defines `NotFlagReporter = 0x0003_000B` and line 325 defines `NotFlagReporter = 3011`, triggering E0428 and non-exhaustive pattern match errors.

### Data Types & Schemas
```rust
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ReputationError {
    NotFlagReporter = 3011, // Unified canonical error code
    UnverifiedOrder = 3012,
}
```

### Tasks
- Remove redundant `NotFlagReporter` definition at line 325 of `reputation/src/lib.rs`.
- Ensure single canonical error code is used across all dispute and review functions.
- Verify `cargo check -p delego-reputation` succeeds.

### Acceptance Criteria
- No duplicate enum names exist in `ReputationError`.
- `delego-reputation` compiles cleanly without pattern errors.

---

## Fix Invalid Manifest Keys in reputation/Cargo.toml

- **Estimate:** 1 day
- **Context:** Cargo issues warnings on workspace checks: `unused manifest key: hook` and `unused manifest key: profile.release.lvm-lto` in `reputation/Cargo.toml`. Profiles in non-root packages are ignored and invalid keys cause linter noise.

### Data Types & Schemas
```toml
# Clean Cargo.toml snippet for reputation crate
[package]
name = "delego-reputation"
version = "0.0.1"
edition = "2021"

[dependencies]
soroban-sdk = { workspace = true }
delego-interfaces = { path = "../interfaces" }
```

### Tasks
- Remove invalid `hook` and `profile.release.lvm-lto` blocks from `reputation/Cargo.toml`.
- Ensure workspace-level profile configuration in root `Cargo.toml` governs release flags.
- Verify `cargo check` executes without manifest warnings.

### Acceptance Criteria
- Cargo emits zero warnings regarding unused manifest keys for `delego-reputation`.

---

## Fix Missing Documentation Comments Across delegation_registry Public Items

- **Estimate:** 1 day
- **Context:** The `delegation_registry` crate has `#![warn(missing_docs)]` enabled at `delegation_registry/src/lib.rs:5`, producing 93 compiler warnings for undocumented public constants, structs, enums, and functions (`MAX_SWEEP_BATCH_SIZE`, `DelegationStatus`, etc.).

### Data Types & Schemas
```rust
/// Maximum number of expired delegations that can be swept in a single call.
pub const MAX_SWEEP_BATCH_SIZE: u32 = 50;

/// Represents the lifecycle state of a delegated authority grant.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DelegationStatus {
    /// Delegation is currently active and valid for signing.
    Active = 1,
    /// Delegation has been explicitly revoked by the owner.
    Revoked = 2,
    /// Delegation has passed its expiration ledger timestamp.
    Expired = 3,
}
```

### Tasks
- Add standard Rust doc comments to all public types, constants, and functions in `delegation_registry/src/lib.rs`.
- Document function arguments, return values, and potential errors.
- Run `cargo check -p delego-delegation-registry` and verify 0 missing doc warnings.

### Acceptance Criteria
- `cargo check -p delego-delegation-registry` produces zero missing documentation warnings.

---

## Fix Unused Variables and Redundant Mutability Warnings in permissions/src/lib.rs

- **Estimate:** 1 day
- **Context:** In `permissions/src/lib.rs`, `record` at line 3582 triggers `unused_mut` and `execution_time` at lines 5465-5466 triggers `unused_variable`. Redundant bindings from conflicting merges pollute compiler diagnostics.

### Data Types & Schemas
```rust
// Fixed variable bindings
let record: PermissionRecord = env.storage().persistent().get(&key).unwrap();
let _execution_time = env.ledger().timestamp() + 86400;
```

### Tasks
- Remove redundant `mut` modifier on `record` in `permissions/src/lib.rs:3582`.
- Consolidate duplicate `execution_time` declarations at lines 5465-5466 or prefix unused tests with `_`.
- Confirm compiler warnings drop to zero.

### Acceptance Criteria
- All `unused_mut` and `unused_variable` warnings resolved in `delego-permissions`.

---

## Fix Soroban SDK Version Consistency Across All Workspace Member Crates

- **Estimate:** 1 day
- **Context:** Multiple crates in the contracts repository specify different minor versions of `soroban-sdk` or duplicate dependency declarations instead of inheriting `{ workspace = true }`. This causes duplicate symbol definitions and build bloat.

### Data Types & Schemas
```toml
[workspace.dependencies]
soroban-sdk = "22.0.0"

# In member Cargo.toml:
[dependencies]
soroban-sdk = { workspace = true }
```

### Tasks
- Audit `escrow/Cargo.toml`, `marketplace/Cargo.toml`, `permissions/Cargo.toml`, `reputation/Cargo.toml`, and `delegation_registry/Cargo.toml`.
- Ensure all member crates use `soroban-sdk = { workspace = true }`.
- Verify workspace lockfile builds cleanly with single `soroban-sdk` entry.

### Acceptance Criteria
- All crates resolve identical `soroban-sdk` version from workspace root.
- `cargo check --workspace` compiles with unified dependency graph.

---

## Fix Overflow-Checks Build Flag Consistency Across Workspace Crates

- **Estimate:** 1 day
- **Context:** Release profiles in Soroban smart contracts must strictly enforce integer overflow checks to prevent underflow/overflow attacks in balance math. The root `Cargo.toml` needs explicit `overflow-checks = true` inherited across all profiles.

### Data Types & Schemas
```toml
[profile.release]
overflow-checks = true
opt-level = "z"
panic = "abort"
lto = true

[profile.test]
overflow-checks = true
```

### Tasks
- Configure `overflow-checks = true` in root `Cargo.toml` under `[profile.release]`, `[profile.dev]`, and `[profile.test]`.
- Add unit test asserting integer overflow in token calculations panics or returns error rather than wrapping.

### Acceptance Criteria
- Arithmetic overflow in contract functions causes instant abort or `ContractError` in both debug and release builds.

---

## Fix Offline Contract Test Scaffolding in tests/src/ Crate

- **Estimate:** 1 day
- **Context:** The integration tests in `tests/src/` fail when running in environments without internet access because mock token fixtures attempt to contact external network services instead of utilizing the Soroban mock auth environment.

### Data Types & Schemas
```rust
pub fn create_mock_token_contract<'a>(env: &'a Env, admin: &Address) -> (Address, token::Client<'a>) {
    let token_address = env.register_stellar_asset_contract(admin.clone());
    let client = token::Client::new(env, &token_address);
    (token_address, client)
}
```

### Tasks
- Refactor test scaffolding in `tests/src/fixtures.rs` to use Soroban built-in `register_stellar_asset_contract`.
- Ensure all integration test suites run 100% offline without network I/O.

### Acceptance Criteria
- `cargo test -p delego-tests` passes completely in an isolated offline sandbox.

---

## Implement Mutual Consent Early Escrow Release Mechanism in escrow

- **Estimate:** 2 days
- **Context:** Currently, escrow funds are locked until either the full inspection window passes or the buyer explicitly triggers release. If both merchant and buyer sign an early release agreement, the contract should disburse funds immediately regardless of remaining inspection time.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct MutualConsentReleaseArgs {
    pub order_id: u64,
    pub buyer_signature: BytesN<64>,
    pub merchant_signature: BytesN<64>,
    pub release_timestamp: u64,
}

pub trait MutualReleaseTrait {
    fn mutual_release(env: Env, args: MutualConsentReleaseArgs) -> Result<bool, EscrowError>;
}
```

### Tasks
- Add `mutual_release` function to `EscrowContract` in `escrow/src/lib.rs`.
- Verify signatures of both buyer and merchant against SHA-256 hash of `(order_id, release_timestamp)`.
- Emit `EscrowEarlyReleased` contract event.

### Acceptance Criteria
- Releases funds immediately when valid signatures from both parties are submitted.
- Rejects call if either signature is invalid or order is already settled.

---

## Implement Pro-Rata Multi-Asset Partial Refund Settlement Distribution

- **Estimate:** 2 days
- **Context:** In multi-item orders where one item is defective, the buyer and merchant may agree on a partial refund percentage (e.g. 40%). The contract must calculate the split, deduct platform fees pro-rata, and disburse token transfers in a single atomic transaction.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct PartialRefundResolution {
    pub order_id: u64,
    pub buyer_bps: u32,     // Basis points (e.g. 4000 = 40%)
    pub merchant_bps: u32,  // Basis points (e.g. 6000 = 60%)
}

pub trait PartialRefundTrait {
    fn resolve_partial_refund(env: Env, resolution: PartialRefundResolution) -> Result<bool, EscrowError>;
}
```

### Tasks
- Implement `resolve_partial_refund` in `escrow/src/lib.rs` enforcing `buyer_bps + merchant_bps == 10000`.
- Calculate pro-rata token transfers without precision loss.
- Emit `PartialRefundSettled` event with breakdown.

### Acceptance Criteria
- Transfers exact computed token amounts to buyer and merchant.
- Requires mutual authorization or arbiter signature.

---

## Implement Dynamic Minimum Fee Floor Enforcement in escrow

- **Estimate:** 2 days
- **Context:** For low-value micro-transactions, percentage-based platform fees can evaluate to zero stroops due to integer truncation. A dynamic minimum fee floor must be enforced based on asset decimal precision.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct FeeConfig {
    pub basis_points: u32,
    pub min_fee_floor_stroops: i128,
    pub max_fee_ceiling_stroops: i128,
}
```

### Tasks
- Store `FeeConfig` in contract instance storage.
- In fee calculation helper, evaluate `max(amount * bps / 10000, min_fee_floor_stroops)`.
- Add unit tests verifying fee floor on 1 stroop orders.

### Acceptance Criteria
- Orders below threshold pay exact `min_fee_floor_stroops`.
- Zero stroop platform fees are strictly prevented.

---

## Implement Buyer Inspection Window Extension Protocol in escrow

- **Estimate:** 2 days
- **Context:** When international shipping is delayed, buyers need to extend the inspection window before automated release occurs. The extension should require either merchant approval or be permitted once for up to 7 days if tracking shows transit delay.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct ExtensionRequest {
    pub order_id: u64,
    pub additional_ledgers: u32,
    pub merchant_approved: bool,
}
```

### Tasks
- Implement `request_window_extension` in `escrow/src/lib.rs`.
- Validate that `additional_ledgers` does not exceed maximum allowable cap (e.g. 100,000 ledgers ~ 7 days).
- Update escrow auto-release ledger deadline.

### Acceptance Criteria
- Deadline is extended successfully if within policy bounds.
- Rejects extensions on orders already in dispute or completed.

---

## Implement Automated Fee Rebate Distribution for Staked Merchants

- **Estimate:** 2 days
- **Context:** Merchants holding high reputation tier stakes should receive monthly fee rebates credited back to their contract balance. The contract should calculate and credit eligible rebates upon settlement.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Copy, Clone, Debug)]
pub struct TierFeeDiscount {
    pub tier: u32, // 1: Bronze, 2: Silver, 3: Gold, 4: Platinum
    pub discount_bps: u32, // e.g. 1000 = 10% discount on platform fee
}
```

### Tasks
- Cross-call `delego-marketplace` to query merchant tier.
- Apply fee discount basis points to platform fee deduction in `EscrowContract::settle`.
- Emit `FeeRebateApplied` contract event.

### Acceptance Criteria
- Staked merchants receive reduced platform fee deduction.
- Rebate math verified by property-based test.

---

## Implement Escrow Cancellation Grace Window for Unfunded Payment Intents

- **Estimate:** 1 day
- **Context:** If a buyer initializes an escrow but fails to transfer the required token deposit within 300 ledgers (~25 minutes), the merchant or relayer should be able to prune the intent without waiting for full expiry.

### Data Types & Schemas
```rust
pub trait UnfundedCancellationTrait {
    fn cancel_unfunded_intent(env: Env, order_id: u64) -> Result<bool, EscrowError>;
}
```

### Tasks
- Implement `cancel_unfunded_intent` checking `escrow.funded == false` and `env.ledger().sequence() > escrow.created_ledger + 300`.
- Delete temporary escrow storage and release stock reservation hold.
- Emit `IntentCancelled` event.

### Acceptance Criteria
- Prunes unfunded orders after 300 ledgers.
- Rejects cancellation if escrow is already funded.

---

## Implement Nonce-Guarded Escrow Release Authorizations for Relayers

- **Estimate:** 2 days
- **Context:** To facilitate gasless checkout where an off-chain relayer submits transactions on behalf of the buyer, the contract must verify an off-chain EIP-712 / SEP-style signed authorization envelope containing a single-use nonce.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct DelegatedReleaseAuth {
    pub order_id: u64,
    pub buyer_address: Address,
    pub relayer_address: Address,
    pub nonce: u64,
    pub valid_until: u64,
    pub signature: BytesN<64>,
}
```

### Tasks
- Implement `relayed_release` in `escrow/src/lib.rs`.
- Verify buyer signature over `(order_id, nonce, valid_until)`.
- Store and increment nonce in instance storage to prevent replay.

### Acceptance Criteria
- Replays with same nonce fail with `EscrowError::NonceAlreadyUsed`.
- Valid signatures disburse funds to merchant.

---

## Implement Granular Storage Footprint TTL Extender in escrow

- **Estimate:** 2 days
- **Context:** Active escrows with long delivery times (e.g. 60-day sea freight) can expire from Soroban persistent storage if TTL is not renewed. An automated keeper interface must allow anyone to extend entry TTL with gas compensation.

### Data Types & Schemas
```rust
pub trait StorageTtlTrait {
    fn extend_escrow_ttl(env: Env, order_id: u64, ledgers_to_extend: u32) -> Result<u32, EscrowError>;
}
```

### Tasks
- Implement `extend_escrow_ttl` calling `env.storage().persistent().extend_ttl(...)`.
- Ensure safe lower and upper bounds on extension ledger counts.
- Emit `EscrowTtlExtended` event.

### Acceptance Criteria
- Successfully extends live ledgers for targeted escrow record.
- Rejects extension calls for non-existent orders.

---

## Implement Multi-Hop Asset Path Payment Resolution with Slippage Bounds

- **Estimate:** 3 days
- **Context:** When a buyer pays in USDC but the merchant requests settlement in XLM, the contract must execute a path payment with strict maximum send and minimum receive slippage bounds.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct PathPaymentArgs {
    pub order_id: u64,
    pub source_token: Address,
    pub destination_token: Address,
    pub max_source_amount: i128,
    pub expected_destination_amount: i128,
    pub min_destination_amount: i128,
}
```

### Tasks
- Implement path payment validation helper in `escrow/src/path_payment.rs`.
- Assert `received_amount >= min_destination_amount`, reverting if slippage is exceeded.
- Transfer surplus source tokens back to buyer.

### Acceptance Criteria
- Reverts if received amount drops below slippage floor.
- Refunds remaining input tokens to buyer.

---

## Implement Cross-Contract Value Conservation Invariant Assertions

- **Estimate:** 2 days
- **Context:** To protect against reentrancy and token drain exploits, the contract must verify that the sum of disbursements (buyer refund + merchant payout + platform fee) strictly equals the initial deposited escrow balance.

### Data Types & Schemas
```rust
#[inline(always)]
pub fn assert_value_conservation(
    initial_deposit: i128,
    buyer_disbursement: i128,
    merchant_disbursement: i128,
    fee_disbursement: i128
) -> Result<(), EscrowError> {
    if initial_deposit != buyer_disbursement + merchant_disbursement + fee_disbursement {
        return Err(EscrowError::ValueConservationViolation);
    }
    Ok(())
}
```

### Tasks
- Add `assert_value_conservation` check across all terminal escrow states (`release`, `refund`, `partial_refund`).
- Write comprehensive property tests with `proptest` verifying conservation under fuzz values.

### Acceptance Criteria
- Any imbalance triggers immediate panic / transaction abort.
- Property test runs 10,000 iterations without conservation violation.

---

## Implement Sub-Order Atomic Resolution Routing in Multi-Merchant Bundles

- **Estimate:** 2 days
- **Context:** When a buyer checks out a bundled basket containing sub-orders from multiple merchants, each sub-order must resolve independently while preserving the parent escrow lifecycle.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct SubOrder {
    pub sub_order_id: u64,
    pub merchant: Address,
    pub amount: i128,
    pub status: EscrowStatus,
}
```

### Tasks
- Implement sub-order mapping in `escrow/src/sub_orders.rs`.
- Allow individual sub-orders to release or dispute without locking unrelated merchant funds.
- Mark parent escrow completed when all sub-orders achieve terminal status.

### Acceptance Criteria
- Merchant A can receive payout even if Merchant B's sub-order is disputed.
- Total deposited collateral matches sum of sub-orders.

---

## Implement Arbiter Evidence Hash Registry with Blake2b-256 Digest Verification

- **Estimate:** 2 days
- **Context:** In disputed escrows, both parties upload photographic or carrier tracking evidence. The contract must register cryptographic digests (Blake2b-256) of off-chain evidence to ensure tamper-proof arbitration.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct EvidenceRecord {
    pub order_id: u64,
    pub submitter: Address,
    pub digest: BytesN<32>,
    pub submission_ledger: u32,
}
```

### Tasks
- Implement `submit_evidence_digest` in `escrow/src/dispute.rs`.
- Store up to 5 evidence digests per party in persistent storage.
- Emit `EvidenceSubmitted` event with digest and submitter address.

### Acceptance Criteria
- Prevents evidence submission after dispute resolution deadline.
- Arbiter resolution can verify against registered digests.

---

## Implement Escrow Fund Recovery Escape Hatch for Inactive Accounts

- **Estimate:** 2 days
- **Context:** If a merchant or buyer address is permanently frozen or loses its private key, funds can be trapped in escrow forever. An emergency timelocked escape hatch (after 365 days of dormancy) must allow admin rescue with multisig.

### Data Types & Schemas
```rust
pub trait EmergencyRecoveryTrait {
    fn timelocked_emergency_recover(env: Env, order_id: u64, recovery_address: Address) -> Result<bool, EscrowError>;
}
```

### Tasks
- Implement `timelocked_emergency_recover` in `escrow/src/emergency.rs`.
- Require `env.ledger().timestamp() > escrow.created_timestamp + 31_536_000` (1 year).
- Require multisig admin authorization and emit `EmergencyFundRecovered` event.

### Acceptance Criteria
- Rejects calls before 365-day timelock has elapsed.
- Releases funds to designated recovery address under multisig.

---

## Implement Merchant Security Bond Staking and Slash Enforcement in marketplace

- **Estimate:** 2 days
- **Context:** To establish merchant trustworthiness, merchants stake XLM or governance tokens into a security bond pool in `marketplace`. If a merchant commits fraudulent acts, the arbiter council can vote to slash the bond.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantBond {
    pub merchant: Address,
    pub token: Address,
    pub staked_amount: i128,
    pub locked_until: u64,
    pub slashed_amount: i128,
}

pub trait BondStakingTrait {
    fn deposit_bond(env: Env, merchant: Address, token: Address, amount: i128) -> Result<bool, MarketplaceError>;
    fn slash_bond(env: Env, merchant: Address, slash_amount: i128, beneficiary: Address) -> Result<bool, MarketplaceError>;
}
```

### Tasks
- Implement `deposit_bond` and `slash_bond` in `marketplace/src/bond.rs`.
- Transfer tokens from merchant to contract bond vault.
- Require governance admin authorization for slashing.

### Acceptance Criteria
- Bond deposits increase merchant bond balance.
- Slashing transfers penalized tokens to victim or platform burn address.

---

## Implement Tiered Verification Badge State Machine in marketplace

- **Estimate:** 2 days
- **Context:** Merchants advance through Bronze, Silver, Gold, and Platinum tiers based on cumulative sales volume, dispute rates, and bond collateral.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MerchantTier {
    Unverified = 0,
    Bronze = 1,
    Silver = 2,
    Gold = 3,
    Platinum = 4,
}
```

### Tasks
- Implement tier evaluation function in `marketplace/src/tier.rs`.
- Define explicit qualification thresholds (e.g. Gold: 10,000 XLM volume, < 1% dispute rate).
- Emit `MerchantTierUpdated` event upon tier transition.

### Acceptance Criteria
- Merchants automatically upgrade when reaching threshold criteria.
- Dispute rate spike downgrades tier automatically.

---

## Implement Dynamic Platform Fee Discounts Based on Governance Tokens

- **Estimate:** 2 days
- **Context:** Merchants staking platform governance tokens in `marketplace` qualify for proportional reductions in platform commission fees.

### Data Types & Schemas
```rust
pub fn calculate_discounted_fee(base_fee_bps: u32, governance_stake: i128) -> u32 {
    if governance_stake >= 100_000_0000000 {
        base_fee_bps.saturating_sub(150) // 1.5% discount
    } else if governance_stake >= 10_000_0000000 {
        base_fee_bps.saturating_sub(75)  // 0.75% discount
    } else {
        base_fee_bps
    }
}
```

### Tasks
- Implement fee discount calculator in `marketplace/src/fees.rs`.
- Expose cross-contract query method `get_merchant_fee_bps(merchant: Address) -> u32`.
- Add unit tests covering all stake tiers.

### Acceptance Criteria
- Returns correct discounted basis points.
- Cannot reduce fee below 0 basis points.

---

## Implement Merchant Storefront Suspension Protocol with Safe Harbor

- **Estimate:** 2 days
- **Context:** When a merchant is suspended for policy violations, new order creations must be rejected immediately, but existing in-flight escrows must proceed through normal settlement without being locked.

### Data Types & Schemas
```rust
pub trait StorefrontSuspensionTrait {
    fn suspend_merchant(env: Env, admin: Address, merchant: Address, reason: Symbol) -> Result<bool, MarketplaceError>;
    fn reinstate_merchant(env: Env, admin: Address, merchant: Address) -> Result<bool, MarketplaceError>;
}
```

### Tasks
- Implement suspension logic in `marketplace/src/merchant.rs` setting `status = MerchantStatus::Suspended`.
- In `create_order`, check merchant status and reject if suspended.
- Ensure settlement methods in `escrow` remain accessible for active orders.

### Acceptance Criteria
- Suspended merchants cannot accept new orders.
- In-progress orders can be confirmed and released safely.

---

## Implement Product SKU Registry with Stock Decrement Guards in marketplace

- **Estimate:** 2 days
- **Context:** Decentralized storefronts need on-chain stock tracking for limited-edition physical or digital goods to prevent overselling.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct SkuRecord {
    pub sku_id: Symbol,
    pub price: i128,
    pub token: Address,
    pub available_quantity: u32,
    pub is_active: bool,
}
```

### Tasks
- Implement `register_sku` and `decrement_stock` in `marketplace/src/catalog.rs`.
- Revert with `MarketplaceError::OutOfStock` if requested quantity > `available_quantity`.
- Add atomic decrement function callable only by authorized `escrow` contract.

### Acceptance Criteria
- Prevents stock from dropping below zero.
- Restricted decrement authorization ensures only valid escrows reduce inventory.

---

## Implement Multi-Currency Price Oracle Query Integration in marketplace

- **Estimate:** 2 days
- **Context:** To price products in USD while settling on-chain in XLM or USDC, `marketplace` must query a trusted Soroban price oracle with staleness checks.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct OraclePriceData {
    pub price: i128,
    pub timestamp: u64,
    pub decimals: u32,
}
```

### Tasks
- Create oracle client interface in `marketplace/src/oracle.rs`.
- Verify price timestamp is within 300 seconds of current ledger timestamp.
- Convert fiat cent values to equivalent token stroops.

### Acceptance Criteria
- Rejects stale oracle prices with `MarketplaceError::StaleOraclePrice`.
- Calculates accurate stroop conversion.

---

## Implement Merchant Metadata IPFS URI Registration in marketplace

- **Estimate:** 1 day
- **Context:** Merchants store rich storefront catalogs, branding logos, and contact information off-chain on IPFS. The contract should store the IPFS CID with SHA-256 integrity verification.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct StorefrontMetadata {
    pub ipfs_cid: String,
    pub content_hash: BytesN<32>,
    pub updated_at: u64,
}
```

### Tasks
- Implement `set_storefront_metadata` in `marketplace/src/storefront.rs`.
- Validate CID format length and store in persistent storage.
- Emit `StorefrontMetadataUpdated` event.

### Acceptance Criteria
- Stores valid IPFS CID and hash.
- Restricts updates to authorized merchant owner.

---

## Implement Storefront Automated Review Verification Attestation Issuance

- **Estimate:** 2 days
- **Context:** To prevent unauthorized review spam, `marketplace` must issue a cryptographic review token upon escrow completion verifying the buyer's purchase.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct ReviewAttestation {
    pub order_id: u64,
    pub buyer: Address,
    pub merchant: Address,
    pub issued_ledger: u32,
    pub consumed: bool,
}
```

### Tasks
- Implement `issue_review_attestation` callable by `escrow` on release.
- Store attestation in temporary storage with 30-day TTL.
- Verify and mark attestation consumed in `reputation` when review is filed.

### Acceptance Criteria
- Attestations are single-use.
- Only buyers with confirmed orders receive valid attestations.

---

## Implement Merchant Bulk Listing Registration Batching in marketplace

- **Estimate:** 2 days
- **Context:** Registering listings one-by-one consumes excessive gas. A batched registration method is needed allowing merchants to add up to 20 listings in a single transaction.

### Data Types & Schemas
```rust
pub trait BulkListingTrait {
    fn batch_register_listings(env: Env, merchant: Address, listings: Vec<ListingParams>) -> Result<u32, MarketplaceError>;
}
```

### Tasks
- Implement `batch_register_listings` in `marketplace/src/catalog.rs`.
- Validate individual listing prices and token addresses in loop.
- Enforce `listings.len() <= 20` to prevent CPU resource exhaustion.

### Acceptance Criteria
- Atomically registers all items in batch.
- Fails batch if any item has invalid parameters.

---

## Implement Dispute Rate Threshold Auto-Suspension in marketplace

- **Estimate:** 2 days
- **Context:** If a merchant's dispute rate exceeds 5% of their last 100 orders, the contract should trigger an automated temporary suspension pending manual audit.

### Data Types & Schemas
```rust
pub fn evaluate_dispute_threshold(total_orders: u32, disputed_orders: u32) -> bool {
    if total_orders >= 20 {
        let dispute_bps = (disputed_orders as u64 * 10000) / (total_orders as u64);
        return dispute_bps > 500; // > 5.0%
    }
    false
}
```

### Tasks
- Implement dispute counter tracking in `marketplace/src/merchant.rs`.
- Evaluate dispute threshold upon `DisputeOpened` cross-contract notification.
- Suspend storefront and emit `MerchantAutoSuspended` event.

### Acceptance Criteria
- Suspends merchant when dispute rate exceeds 5%.
- Ignores threshold until minimum sample of 20 orders is reached.

---

## Implement Merchant Operational Status View Method in marketplace

- **Estimate:** 1 day
- **Context:** External dApps need a single consolidated read call returning merchant operational status, verification tier, active bond amount, and effective fee rate.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantOperationalView {
    pub status: MerchantStatus,
    pub tier: MerchantTier,
    pub verified: bool,
    pub effective_fee_bps: u32,
    pub bond_balance: i128,
    pub dispute_count: u32,
}
```

### Tasks
- Implement `get_merchant_operational_view` in `marketplace/src/lib.rs`.
- Consolidate storage reads into unified DTO.
- Add unit tests for various merchant states.

### Acceptance Criteria
- Returns complete merchant status in single RPC call.
- Handles unregistered merchants with `MarketplaceError::MerchantNotFound`.

---

## Implement Marketplace Commission Treasury Split Routing

- **Estimate:** 2 days
- **Context:** Platform fees collected by the marketplace must be split automatically between the DAO Community Treasury (70%), the Arbiter Compensation Pool (20%), and the Insurance Reserve (10%).

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct TreasurySplitConfig {
    pub dao_treasury: Address,
    pub dao_bps: u32,          // 7000 = 70%
    pub arbiter_pool: Address,
    pub arbiter_bps: u32,      // 2000 = 20%
    pub insurance_pool: Address,
    pub insurance_bps: u32,    // 1000 = 10%
}
```

### Tasks
- Implement `route_fee_disbursements` in `marketplace/src/treasury.rs`.
- Verify split percentages sum to exactly 10,000 basis points.
- Transfer tokens to designated pool addresses.

### Acceptance Criteria
- Accurately routes fees according to configured split ratios.
- Zero token leakage verified.

---

## Implement Storefront Liquidation Reserve Staking for High-Value Goods

- **Estimate:** 2 days
- **Context:** Merchants selling items valued over 10,000 XLM must lock a 10% liquidation reserve in the contract until 30 days post-delivery to guarantee buyer restitution.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct LiquidationReserve {
    pub merchant: Address,
    pub order_id: u64,
    pub reserve_amount: i128,
    pub unlock_ledger: u32,
}
```

### Tasks
- Implement reserve lock mechanism in `marketplace/src/reserve.rs`.
- Automatically deduct 10% from settlement and lock until `unlock_ledger`.
- Allow merchant withdrawal once unlock ledger sequence is reached.

### Acceptance Criteria
- Reserve cannot be unlocked prior to maturity ledger.
- Arbiters can liquidate reserve to refund buyer in dispute.

---

## Implement Granular Operation Scope Whitelisting for Delegated Session Keys

- **Estimate:** 2 days
- **Context:** Delegated session keys used by automated buyer agents must be restricted to specific contract methods (e.g. `fund_escrow`, `confirm_delivery`) while blocking high-privilege methods like `update_admin`.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum OperationScope {
    FundEscrow = 1,
    ConfirmDelivery = 2,
    SubmitReview = 4,
    RaiseDispute = 8,
    AllOperations = 0xFFFF,
}
```

### Tasks
- Implement bitmask scope validation in `permissions/src/scope.rs`.
- Check `(granted_scopes & requested_op) == requested_op` before authorizing call.
- Return `PermissionError::UnauthorizedScope` on unauthorized operations.

### Acceptance Criteria
- Enforces bitmask operation restrictions on session key invocations.
- Rejects out-of-scope calls with specific error code.

---

## Implement Ephemeral Relayer Gas Sponsoring with Allowance Limits

- **Estimate:** 2 days
- **Context:** Relayers submitting transactions on behalf of users need on-chain gas fee allowances to prevent draining the relayer's native XLM balance.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct GasSponsorshipQuota {
    pub relayer: Address,
    pub sponsor_balance_stroops: i128,
    pub max_fee_per_tx_stroops: i128,
    pub remaining_tx_count: u32,
}
```

### Tasks
- Implement sponsorship deduction logic in `permissions/src/sponsorship.rs`.
- Deduct exact base fee from sponsor balance on transaction execution.
- Revert if fee exceeds `max_fee_per_tx_stroops`.

### Acceptance Criteria
- Reimburses relayer from user's deposited sponsorship pool.
- Enforces per-transaction and lifetime balance caps.

---

## Implement Session Key Expiration Invariant Check with Strict Ledger Bounds

- **Estimate:** 1 day
- **Context:** Session keys must have an absolute ledger timestamp expiration. The contract must verify `env.ledger().timestamp() < session.expires_at` on every authorization.

### Data Types & Schemas
```rust
pub fn verify_session_active(session: &SessionKeyRecord, current_timestamp: u64) -> Result<(), PermissionError> {
    if current_timestamp >= session.expires_at {
        return Err(PermissionError::SessionExpired);
    }
    Ok(())
}
```

### Tasks
- Integrate expiration check across all delegate execution paths in `permissions/src/lib.rs`.
- Add automated test setting mock ledger timestamp past expiry.

### Acceptance Criteria
- Expired session keys are rejected immediately with `PermissionError::SessionExpired`.
- Active sessions execute smoothly.

---

## Implement Multi-Sig Quorum Weight Accumulation for High-Value Escrows

- **Estimate:** 2 days
- **Context:** For escrows exceeding 50,000 XLM, releasing funds must require signatures from at least 2 of 3 authorized company signers.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct MultiSigQuorum {
    pub signers: Vec<Address>,
    pub threshold_weight: u32,
    pub signer_weights: Vec<u32>,
}
```

### Tasks
- Implement quorum evaluation in `permissions/src/multisig.rs`.
- Verify signatures and accumulate weight until `accumulated >= threshold_weight`.
- Emit `QuorumApproved` event.

### Acceptance Criteria
- Releases high-value escrow only when threshold weight is satisfied.
- Rejects calls with insufficient signer weights.

---

## Implement Delegation Registry Sweep Method for Mass Revocation

- **Estimate:** 2 days
- **Context:** Expired delegations consume persistent storage rent. A batch sweep function is needed allowing keepers to prune up to 50 expired delegation records in exchange for a small bounty.

### Data Types & Schemas
```rust
pub trait SweepDelegationsTrait {
    fn sweep_expired_delegations(env: Env, keeper: Address, delegation_keys: Vec<BytesN<32>>) -> Result<u32, DelegationError>;
}
```

### Tasks
- Implement `sweep_expired_delegations` in `delegation_registry/src/lib.rs`.
- Verify each delegation is expired (`status == Expired` or `expires_at < now`).
- Remove records and transfer keeper reward from gas rebate pool.

### Acceptance Criteria
- Safely removes expired records.
- Rejects sweeping active delegations.

---

## Implement One-Time Delegated Authorization Tickets with Nonce Invalidation

- **Estimate:** 2 days
- **Context:** For single-action approvals (e.g. approving a one-off order return), the contract should support single-use authorization tickets that self-destruct upon execution.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct OneTimeTicket {
    pub ticket_id: BytesN<32>,
    pub issuer: Address,
    pub delegate: Address,
    pub action_hash: BytesN<32>,
    pub consumed: bool,
}
```

### Tasks
- Implement `create_ticket` and `consume_ticket` in `permissions/src/tickets.rs`.
- Mark ticket consumed atomically in same transaction.
- Prevent replay with `PermissionError::TicketAlreadyConsumed`.

### Acceptance Criteria
- Ticket executes exactly once and cannot be replayed.
- Expired tickets return error.

---

## Implement Timelocked Delegate Key Replacement Protocol

- **Estimate:** 2 days
- **Context:** Replacing an active delegate signing key requires a 24-hour timelock to allow the owner to detect and cancel unauthorized key swap attempts.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct KeyReplacementProposal {
    pub current_delegate: Address,
    pub proposed_delegate: Address,
    pub unlock_timestamp: u64,
    pub executed: bool,
}
```

### Tasks
- Implement `propose_key_replacement` and `execute_key_replacement` in `permissions/src/timelock.rs`.
- Enforce `current_timestamp >= unlock_timestamp` before execution.
- Provide `cancel_key_replacement` callable by owner.

### Acceptance Criteria
- Key replacement cannot execute before 24-hour delay.
- Owner can cancel pending proposal at any time.

---

## Implement Permission Delegation Hierarchy Restricting Sub-Delegates

- **Estimate:** 2 days
- **Context:** If a delegate is granted authority by an owner, they must not be permitted to sub-delegate greater permissions than they possess themselves (prevent privilege escalation).

### Data Types & Schemas
```rust
pub fn validate_sub_delegation(parent_scope: u32, sub_scope: u32) -> Result<(), PermissionError> {
    if (parent_scope & sub_scope) != sub_scope {
        return Err(PermissionError::PrivilegeEscalationAttempt);
    }
    Ok(())
}
```

### Tasks
- Implement sub-delegation depth counter (max depth: 2).
- Validate sub-scope is a strict subset of parent scope.
- Add unit tests attempting privilege escalation.

### Acceptance Criteria
- Sub-delegates cannot exceed parent permissions.
- Exceeding max depth reverts with `PermissionError::MaxDelegationDepthExceeded`.

---

## Implement Emergency Administrative Delegate Freeze Guard

- **Estimate:** 1 day
- **Context:** If an automated relayer or third-party agent service is hacked, an emergency admin freeze method must instantly suspend all sessions issued to that relayer address across the system.

### Data Types & Schemas
```rust
pub trait EmergencyFreezeTrait {
    fn freeze_relayer(env: Env, admin: Address, relayer: Address) -> Result<bool, PermissionError>;
    fn is_relayer_frozen(env: Env, relayer: Address) -> bool;
}
```

### Tasks
- Store frozen relayer bitmap / map in instance storage.
- Check `!is_relayer_frozen(caller)` on all delegated entry points.
- Emit `RelayerFrozen` event.

### Acceptance Criteria
- Calls from frozen relayers are blocked with `PermissionError::RelayerFrozen`.
- Freeze can only be triggered by contract admin.

---

## Implement Cryptographic Signature Verification for Secp256r1 Passkeys

- **Estimate:** 2 days
- **Context:** Modern mobile passkeys and hardware authenticators use Secp256r1 (NIST P-256) curves. The contract should verify Secp256r1 signatures using Soroban cryptographic host functions.

### Data Types & Schemas
```rust
pub fn verify_passkey_signature(
    env: &Env,
    public_key: &BytesN<65>,
    message_digest: &BytesN<32>,
    signature: &BytesN<64>
) -> Result<bool, PermissionError> {
    env.crypto().secp256r1_verify(public_key, message_digest, signature);
    Ok(true)
}
```

### Tasks
- Implement passkey verification helper in `permissions/src/crypto.rs`.
- Handle uncompressed and compressed P-256 public key formats.
- Add unit test with known RFC test vectors.

### Acceptance Criteria
- Valid Secp256r1 signatures verify successfully.
- Tampered digests or signatures return `PermissionError::InvalidSignature`.

---

## Implement Delegation Storage Key Migration Strategy for Upgrades

- **Estimate:** 2 days
- **Context:** Upgrading delegation contract storage schemas from V1 to V2 must preserve existing active delegations without requiring users to re-sign authorization grants.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub enum DataKey {
    DelegationV1(Address),
    DelegationV2(Address, Symbol),
    SchemaVersion,
}
```

### Tasks
- Implement lazy migration helper reading V1 format and writing V2 format upon access.
- Write migration unit tests verifying backward compatibility.

### Acceptance Criteria
- Old V1 records are read seamlessly without contract panics.
- Writes automatically update record to V2 schema.

---

## Implement Account Recovery Guardian Quorum Execution in permissions

- **Estimate:** 2 days
- **Context:** If a user loses their private key, a designated guardian quorum (e.g. 2 of 3 trusted friends or institutions) can vote to transfer ownership to a new replacement key.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct RecoveryProposal {
    pub account: Address,
    pub proposed_new_owner: Address,
    pub approvals: Vec<Address>,
    pub required_approvals: u32,
    pub created_at: u64,
}
```

### Tasks
- Implement recovery submission and voting functions in `permissions/src/recovery.rs`.
- Require 48-hour cool-down period before executing recovery to allow legitimate owner to cancel.
- Emit `AccountRecoveryExecuted` event.

### Acceptance Criteria
- Successfully transfers account ownership when quorum approves.
- Original owner can cancel malicious recovery during cool-down.

---

## Implement Delegation Heartbeat Verification Checking Keeper Liveness

- **Estimate:** 2 days
- **Context:** Delegated keeper bots managing recurring subscription payments must submit periodic heartbeats. If a keeper fails to ping within 7 days, delegations fall back to secondary keepers.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct KeeperHeartbeat {
    pub keeper: Address,
    pub last_heartbeat_timestamp: u64,
    pub is_active: bool,
}
```

### Tasks
- Implement `submit_heartbeat` and `check_keeper_liveness` in `delegation_registry/src/heartbeat.rs`.
- Allow fallback to secondary keeper if `last_heartbeat + 604800 < now`.
- Emit `KeeperHeartbeatMissed` alert.

### Acceptance Criteria
- Correctly detects inactive keepers.
- Routes execution to secondary keeper when primary is dead.

---

## Implement Exponential Decay Weighting for Historical Merchant Feedback

- **Estimate:** 2 days
- **Context:** Recent merchant reviews reflect current service quality better than reviews from two years ago. The reputation contract should apply exponential decay half-life weighting to older feedback ratings.

### Data Types & Schemas
```rust
pub fn compute_decayed_weight(review_timestamp: u64, current_timestamp: u64) -> u32 {
    let age_seconds = current_timestamp.saturating_sub(review_timestamp);
    let half_life_seconds = 180 * 86400; // 180 days
    let decay_factor = (age_seconds / half_life_seconds).min(4);
    100 >> decay_factor // 100%, 50%, 25%, 12.5%, 6.25%
}
```

### Tasks
- Implement decayed score aggregator in `reputation/src/scoring.rs`.
- Weight reviews by age bucket when recalculating merchant score.
- Add unit tests comparing fresh vs aged ratings.

### Acceptance Criteria
- Older reviews have diminishing impact on reputation rating.
- Fresh reviews receive full 100% weight.

---

## Implement Anti-Sybil Review Weight Discounting Based on Buyer Account Age

- **Estimate:** 2 days
- **Context:** Sybil rings create disposable buyer accounts to leave fake 5-star reviews. Reviews from accounts younger than 30 days or with fewer than 3 completed escrows must be discounted by 80%.

### Data Types & Schemas
```rust
pub fn calculate_reviewer_weight(account_age_ledgers: u32, completed_orders: u32) -> u32 {
    if account_age_ledgers > 500_000 && completed_orders >= 5 {
        100 // Full weight
    } else if completed_orders >= 2 {
        50  // Half weight
    } else {
        20  // 20% weight for brand new accounts
    }
}
```

### Tasks
- Implement reviewer weight evaluator in `reputation/src/antisybil.rs`.
- Cross-reference buyer completed order history with `escrow`.
- Store weighted sum in merchant reputation accumulator.

### Acceptance Criteria
- Brand new accounts have minimal influence on aggregate score.
- Established buyers receive full rating weight.

---

## Implement Arbiter Voting Quorum Consensus Engine in reputation

- **Estimate:** 3 days
- **Context:** Complex disputes involving high values are adjudicated by a panel of 5 elected arbiters. The contract must collect individual votes and require a 3/5 majority to finalize resolution.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct ArbiterTally {
    pub dispute_id: u64,
    pub votes_for_buyer: u32,
    pub votes_for_merchant: u32,
    pub total_voters: u32,
    pub finalized: bool,
}
```

### Tasks
- Implement voting tally in `reputation/src/arbitration.rs`.
- Ensure each arbiter can vote only once per dispute.
- Trigger escrow resolution when quorum is reached.

### Acceptance Criteria
- Finalizes when 3 of 5 arbiters agree on outcome.
- Rejects duplicate votes from same arbiter address.

---

## Implement Sybil Flag Reporting with Slashing for Fraudulent Reporters

- **Estimate:** 2 days
- **Context:** Users can flag suspicious merchant reviews as fake. To prevent malicious competitors from flagging genuine reviews, reporters must stake a collateral bond that is slashed if their report is deemed bad-faith.

### Data Types & Schemas
```rust
pub trait FlagReportingTrait {
    fn flag_review(env: Env, reporter: Address, review_id: u64, bond_amount: i128) -> Result<bool, ReputationError>;
    fn resolve_flag(env: Env, arbiter: Address, flag_id: u64, upheld: bool) -> Result<bool, ReputationError>;
}
```

### Tasks
- Implement flag deposit and resolution in `reputation/src/flags.rs`.
- Refund bond + bounty if flag is upheld; slash bond if flag is rejected as frivolous.
- Emit `ReviewFlagResolved` event.

### Acceptance Criteria
- Upheld flags remove fraudulent review and reward reporter.
- Frivolous reports result in bond forfeiture.

---

## Implement Verified Purchase Proof Cross-Reference with escrow Contract

- **Estimate:** 2 days
- **Context:** To eliminate unverified review submissions, the `reputation` contract must cross-call `escrow` to cryptographically verify that the order ID was settled and belongs to the reviewer.

### Data Types & Schemas
```rust
pub fn verify_purchase_proof(env: &Env, escrow_contract: &Address, order_id: u64, buyer: &Address) -> Result<bool, ReputationError> {
    let client = EscrowClient::new(env, escrow_contract);
    let order_status = client.get_order_status(&order_id);
    if order_status != EscrowStatus::Released {
        return Err(ReputationError::UnverifiedOrder);
    }
    Ok(true)
}
```

### Tasks
- Implement cross-contract call to `escrow` in `reputation/src/reviews.rs`.
- Revert with `ReputationError::UnverifiedOrder` if order is unfulfilled.
- Attach on-chain proof badge to review record.

### Acceptance Criteria
- Rejects reviews for unconfirmed or fake order IDs.
- Allows verified buyers to post immutable reviews.

---

## Implement Dispute Outcome Precedent Registry for Automated Adjudication

- **Estimate:** 2 days
- **Context:** Standard disputes (e.g. tracking shows delivered vs tracking shows lost in transit) follow established rules. A precedent registry contract allows automated resolution for clear-cut cases.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct DisputeRule {
    pub reason_code: u32,
    pub required_carrier_status: Symbol,
    pub auto_verdict: DisputeVerdict, // RefundBuyer or ReleaseToMerchant
}
```

### Tasks
- Implement rule engine in `reputation/src/precedent.rs`.
- Evaluate signed carrier tracking attestation against precedent rules.
- Execute instantaneous resolution if rule criteria match exactly.

### Acceptance Criteria
- Resolves unambiguous disputes automatically without manual arbiter overhead.
- Falls back to human arbitration for unmapped scenarios.

---

## Implement Merchant Defamation Appeal Mechanism with Collateralized Bond

- **Estimate:** 2 days
- **Context:** Merchants can appeal demonstrably false reviews by staking an appeal bond. If arbiters confirm the review violates platform truthfulness policies, the review is hidden and bond returned.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct ReviewAppeal {
    pub review_id: u64,
    pub merchant: Address,
    pub appeal_bond_stroops: i128,
    pub justification_hash: BytesN<32>,
    pub status: AppealStatus,
}
```

### Tasks
- Implement appeal filing in `reputation/src/appeals.rs`.
- Hold appeal bond in escrow.
- Arbiter vote resolves appeal: if successful, hide review; if denied, forfeit bond to platform charity.

### Acceptance Criteria
- Merchants can challenge demonstrably false feedback.
- Bond cost deters frivolous appeal spam.

---

## Implement Dynamic Feedback Rating Range Validation in reputation

- **Estimate:** 1 day
- **Context:** Review scores must be strictly constrained between 1 and 100 basis points (1.0 to 5.0 stars mapped to 20 to 100). The contract must reject values outside bounds.

### Data Types & Schemas
```rust
pub fn validate_rating(rating: u32) -> Result<(), ReputationError> {
    if !(20..=100).contains(&rating) {
        return Err(ReputationError::InvalidRatingValue);
    }
    Ok(())
}
```

### Tasks
- Add validation in review submission handler in `reputation/src/lib.rs`.
- Add unit tests testing 0, 19, 101, and u32::MAX values.

### Acceptance Criteria
- Only ratings in range 20..=100 are accepted.
- Rejects out-of-range values with `ReputationError::InvalidRatingValue`.

---

## Implement Arbiter Dispute Response Deadline Penalty with Reassignment

- **Estimate:** 2 days
- **Context:** If an assigned arbiter fails to cast a vote within 72 hours (43,200 ledgers), the contract should slash a portion of their arbiter stake and reassign the case to a reserve arbiter.

### Data Types & Schemas
```rust
pub trait ArbiterLivenessTrait {
    fn check_arbiter_deadline(env: Env, dispute_id: u64, arbiter: Address) -> Result<bool, ReputationError>;
}
```

### Tasks
- Implement deadline checker in `reputation/src/arbitration.rs`.
- Verify current ledger sequence > `dispute.assigned_ledger + 43200`.
- Slash inactivity penalty and reassign to next available arbiter.

### Acceptance Criteria
- Prevents disputes from stalling indefinitely due to inactive arbiters.
- Incentivizes timely dispute resolution.

---

## Implement On-Chain Merchant Dispute Ratio Health Score Meter

- **Estimate:** 2 days
- **Context:** DApps querying merchant profiles need an on-chain health score calculated as `(successful_orders * 100) / total_orders` with zero floating point arithmetic.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct MerchantHealthScore {
    pub merchant: Address,
    pub total_orders: u32,
    pub dispute_count: u32,
    pub health_percentage: u32, // 0 to 100
    pub is_healthy: bool,
}
```

### Tasks
- Implement health calculator in `reputation/src/health.rs`.
- Mark `is_healthy = false` if health drops below 95%.
- Expose view function `get_merchant_health(merchant: Address)`.

### Acceptance Criteria
- Accurately reflects merchant order success ratio.
- Safe against division by zero on new merchants.

---

## Implement Evidence Verification Oracle Interface for Carrier Signatures

- **Estimate:** 2 days
- **Context:** In logistics disputes, courier API signatures proving delivery or loss must be verified on-chain via an oracle signature check before auto-releasing funds.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct CarrierProof {
    pub carrier_oracle: Address,
    pub tracking_number_hash: BytesN<32>,
    pub delivery_status: Symbol,
    pub signature: BytesN<64>,
}
```

### Tasks
- Implement oracle signature verification in `reputation/src/oracle_proof.rs`.
- Check that oracle address is in authorized carrier verifier registry.
- Emit `CarrierProofVerified` event.

### Acceptance Criteria
- Valid courier signatures verify cleanly.
- Rejects proofs from unauthorized oracle keys.

---

## Implement Anonymous Buyer Review Encryption Key Wrapping in reputation

- **Estimate:** 2 days
- **Context:** Buyers wishing to leave honest feedback without exposing their Stellar public address to merchant retaliation should have their review encrypted with an ephemeral stealth address.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct AnonymousReview {
    pub review_id: u64,
    pub merchant: Address,
    pub encrypted_payload: Bytes,
    pub stealth_pubkey: BytesN<32>,
    pub zk_proof_valid: bool,
}
```

### Tasks
- Implement anonymous review registration in `reputation/src/stealth.rs`.
- Store encrypted payload buffer with verified single-use attestation ticket.
- Prevent linking back to buyer's primary payment wallet.

### Acceptance Criteria
- Allows verified reviews while preserving buyer on-chain privacy.
- Requires valid purchase proof ticket.

---

## Implement Periodic Reputation Snapshot Checkpointing in reputation

- **Estimate:** 2 days
- **Context:** Calculating aggregate scores across thousands of individual review records on every query is computationally expensive. Periodic weekly snapshots should memoize historical ratings.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct ReputationSnapshot {
    pub merchant: Address,
    pub snapshot_ledger: u32,
    pub cumulative_score: u64,
    pub review_count: u32,
}
```

### Tasks
- Implement snapshot builder in `reputation/src/snapshot.rs`.
- Memoize historical sum and store in persistent storage.
- On new queries, combine latest snapshot with recent un-checkpointed reviews.

### Acceptance Criteria
- Reduces gas cost of reputation queries by up to 80%.
- Maintains 100% calculation accuracy.

---

## Implement Blend Protocol Staking Pool Adapter for Escrow Yield Accrual

- **Estimate:** 3 days
- **Context:** Idle escrow capital locked for weeks should generate yield. The `escrow` contract needs an adapter interface that deposits stablecoin escrows into Blend lending pools and retrieves bTokens.

### Data Types & Schemas
```rust
pub trait BlendLendingPoolTrait {
    fn supply_to_pool(env: Env, pool: Address, asset: Address, amount: i128) -> Result<i128, EscrowError>;
    fn withdraw_from_pool(env: Env, pool: Address, asset: Address, btoken_amount: i128) -> Result<i128, EscrowError>;
}
```

### Tasks
- Implement Blend pool cross-contract integration in `escrow/src/blend.rs`.
- Deposit USDC into lending pool upon escrow funding.
- Withdraw principal + interest upon escrow resolution.

### Acceptance Criteria
- Successfully stakes principal to Blend pool.
- Returns original principal plus accrued interest upon completion.

---

## Implement Automated Yield Share Split Engine Allocating Settlement Interest

- **Estimate:** 2 days
- **Context:** When an escrow completes, accrued interest earned from DeFi lending should be split 50/50 between buyer and merchant (or credited to buyer as a cash-back reward).

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct YieldDistribution {
    pub order_id: u64,
    pub gross_yield: i128,
    pub buyer_yield_share: i128,
    pub merchant_yield_share: i128,
}
```

### Tasks
- Implement yield distribution math in `escrow/src/yield_splitter.rs`.
- Assert `buyer_yield_share + merchant_yield_share == gross_yield`.
- Transfer yield tokens alongside escrow disbursements.

### Acceptance Criteria
- Disburses exact yield shares without precision loss.
- Records yield event on ledger.

---

## Implement Liquid Staking Token (LST) Escrow Collateral Support

- **Estimate:** 2 days
- **Context:** Buyers should be able to fund escrows using yield-bearing liquid staking tokens (e.g. yXLM) while preserving underlying staking rewards during the inspection period.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct LstCollateralRecord {
    pub order_id: u64,
    pub lst_token_address: Address,
    pub initial_share_balance: i128,
    pub exchange_rate_snapshot: i128,
}
```

### Tasks
- Implement LST token handler in `escrow/src/lst_support.rs`.
- Track share balances instead of raw token amounts to reflect value appreciation.
- Transfer appreciated value to buyer upon completion.

### Acceptance Criteria
- Accepts whitelisted LST tokens.
- Preserves and disburses staking appreciation.

---

## Implement Automated Health Factor Check for Staked Escrow Vaults

- **Estimate:** 2 days
- **Context:** If collateralized escrow balances are borrowed against in lending protocols, the contract must monitor the health factor (`collateral_value / borrowed_value`) and prevent drops below 1.25.

### Data Types & Schemas
```rust
pub fn assert_vault_health(collateral_value: i128, debt_value: i128) -> Result<(), EscrowError> {
    if debt_value > 0 {
        let health_factor_bps = (collateral_value * 10000) / debt_value;
        if health_factor_bps < 12500 { // 1.25
            return Err(EscrowError::UnsafeHealthFactor);
        }
    }
    Ok(())
}
```

### Tasks
- Implement health factor check in `escrow/src/vault_safety.rs`.
- Revert operations that would push health factor below safety threshold.
- Emit `HealthFactorWarning` if health falls below 1.30.

### Acceptance Criteria
- Strictly blocks liquidation risk by enforcing 1.25 health factor floor.
- Prevents under-collateralized borrowing.

---

## Implement Flash Loan Protection Guards on Staked Escrow Balances

- **Estimate:** 2 days
- **Context:** Staked escrow balances must never be lent out or manipulated within the same ledger transaction to prevent economic flash loan exploits.

### Data Types & Schemas
```rust
pub fn guard_flash_loan(env: &Env, last_activity_ledger: u32) -> Result<(), EscrowError> {
    if env.ledger().sequence() == last_activity_ledger {
        return Err(EscrowError::SameLedgerManipulationDetected);
    }
    Ok(())
}
```

### Tasks
- Record `deposit_ledger` in escrow instance state.
- Require `current_ledger > deposit_ledger` before permitting withdrawal or yield claim.
- Add integration test attempting same-block deposit and withdrawal.

### Acceptance Criteria
- Same-block deposit and withdrawal is rejected.
- Immunizes escrow pools from flash loan sandwich attacks.

---

## Implement AMM Liquidity Pool Routing for Multi-Asset Cross-Border Payments

- **Estimate:** 3 days
- **Context:** When a buyer in Europe pays in EURC and a merchant in Japan settles in JPYC, the contract must route the swap through Stellar AMM liquidity pools with price tolerance checks.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct AmmRouteHop {
    pub pool_address: Address,
    pub input_token: Address,
    pub output_token: Address,
    pub min_output_amount: i128,
}
```

### Tasks
- Implement pool swap router in `escrow/src/amm_router.rs`.
- Execute multi-hop swaps across registered AMM pools.
- Enforce strict cumulative slippage cap.

### Acceptance Criteria
- Successfully routes payment across multi-asset pools.
- Reverts entire transaction if any swap in route fails.

---

## Implement Minimum Yield Threshold Guard for Escrow Staking

- **Estimate:** 1 day
- **Context:** If the APY on a lending pool drops below gas costs (e.g. < 0.5%), staking is economically unviable. The contract should revert or bypass staking if pool APY is below minimum threshold.

### Data Types & Schemas
```rust
pub fn is_yield_viable(pool_supply_rate_bps: u32, min_acceptable_bps: u32) -> bool {
    pool_supply_rate_bps >= min_acceptable_bps
}
```

### Tasks
- Add yield rate check before supplying funds to external pool.
- Hold funds in standard escrow vault if yield is below threshold.
- Emit `YieldBypassedLowApy` event.

### Acceptance Criteria
- Skips external pool deposit when APY is below threshold.
- Protects contract from negative or negligible yield returns.

---

## Implement Smart Contract Emergency Liquidity Unwinding Protocol

- **Estimate:** 2 days
- **Context:** If an integrated DeFi lending protocol experiences an exploit or paused withdrawals, an emergency unwinding protocol must allow admin to detach the integration and disburse base principal.

### Data Types & Schemas
```rust
pub trait EmergencyUnwindTrait {
    fn emergency_detach_defi_pool(env: Env, admin: Address, pool: Address) -> Result<bool, EscrowError>;
}
```

### Tasks
- Implement emergency detach in `escrow/src/emergency_unwind.rs`.
- Mark pool as disabled and prevent further deposits.
- Fallback to primary escrow reserves for buyer/merchant settlement.

### Acceptance Criteria
- Isolates compromised external pool without breaking core escrow functionality.
- Restricted to multisig admin authorization.

---

## Implement Yield Accrual Checkpointing Ledger Counter for Pro-Rata Returns

- **Estimate:** 2 days
- **Context:** When an escrow is partially refunded after 20 days of yield generation, the accumulated yield must be split based on the exact duration and capital proportion of each party.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct YieldCheckpoint {
    pub ledger_sequence: u32,
    pub cumulative_yield_per_share: i128,
}
```

### Tasks
- Implement masterchef-style `cumulative_yield_per_share` accounting in `escrow/src/checkpoint.rs`.
- Calculate exact yield accrued during escrow lifespan.
- Add property tests verifying zero rounding error.

### Acceptance Criteria
- Accurately calculates pro-rata yield over arbitrary ledger ranges.
- Mathematically verified zero token remainder.

---

## Implement Token Allowance Safety Revocation After Transfer in escrow

- **Estimate:** 1 day
- **Context:** To eliminate lingering token allowance vulnerabilities, the contract should explicitly revoke any remaining allowance granted to third-party pool contracts after a transfer completes.

### Data Types & Schemas
```rust
pub fn revoke_allowance_safe(env: &Env, token: &Address, spender: &Address) {
    let client = token::Client::new(env, token);
    client.approve(&env.current_contract_address(), spender, &0, &0);
}
```

### Tasks
- Add `revoke_allowance_safe` helper in `escrow/src/token_safety.rs`.
- Call after every DeFi pool deposit or AMM swap.
- Verify residual allowance is zero.

### Acceptance Criteria
- Token allowance is reset to 0 immediately post-execution.
- Prevents unauthorized subsequent pulls.

---

## Implement Collateral Rebalancing Trigger on Volatile Stablecoins

- **Estimate:** 2 days
- **Context:** If an escrow is funded with a decentralized stablecoin that de-pegs by more than 2% against USD, the contract should emit a rebalance alert and require the buyer to top up collateral.

### Data Types & Schemas
```rust
pub trait DepegGuardTrait {
    fn check_depeg_status(env: Env, order_id: u64, oracle: Address) -> Result<bool, EscrowError>;
    fn topup_depegged_collateral(env: Env, order_id: u64, additional_amount: i128) -> Result<bool, EscrowError>;
}
```

### Tasks
- Query oracle price and detect if `price < 98_0000` (below $0.98).
- Freeze auto-release timer until buyer tops up shortfall.
- Emit `CollateralTopupRequired` event.

### Acceptance Criteria
- Detects stablecoin de-peg > 2%.
- Protects merchant from receiving depreciated settlement collateral.

---

## Implement Dynamic Liquidity Surcharge During Stellar Congestion

- **Estimate:** 2 days
- **Context:** During severe ledger congestion where base fees spike 10x, the contract should collect an optional liquidity priority surcharge that gets forwarded to the transaction submitter.

### Data Types & Schemas
```rust
pub fn compute_priority_surcharge(congestion_multiplier: u32, base_fee: i128) -> i128 {
    if congestion_multiplier > 1 {
        base_fee.saturating_mul(congestion_multiplier as i128)
    } else {
        0
    }
}
```

### Tasks
- Implement priority surcharge helper in `escrow/src/gas_surcharge.rs`.
- Allow relayer to claim surcharge if transaction confirms within target ledger window.

### Acceptance Criteria
- Compensates relayers during high gas surge periods.
- Surcharge strictly capped at 10,000 stroops.

---

## Implement Formal Property-Based Fuzz Testing Suite for DeFi Yield Integrations

- **Estimate:** 2 days
- **Context:** DeFi integrations are susceptible to subtle rounding and accounting bugs. A formal property-based test suite using `proptest` is needed to verify balance conservation across millions of randomized deposit/withdraw sequences.

### Data Types & Schemas
```rust
#[cfg(test)]
mod yield_fuzz_tests {
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn fuzz_yield_conservation(
            principal in 1_0000000i128..1_000_000_0000000i128,
            rate_bps in 1u32..5000u32,
            duration_ledgers in 100u32..1_000_000u32
        ) {
            // Assert total disbursed == principal + computed yield
        }
    }
}
```

### Tasks
- Implement fuzzing test harness in `escrow/src/tests/yield_fuzz.rs`.
- Assert invariants: no negative balances, zero token leakage, monotonic yield growth.
- Run 50,000 iterations in cargo test.

### Acceptance Criteria
- 50,000 fuzz cases execute without invariant violation.
- Demonstrates mathematical robustness of yield accounting.

---

