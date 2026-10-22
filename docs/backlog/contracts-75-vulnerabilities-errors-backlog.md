# Smart Contracts Vulnerabilities, Errors & Implementation Backlog (75 Issues)

Developer-ready issues covering vulnerabilities, errors, runtime bugs, and implementations.
**Total Issues:** 75
**Target Sizing:** 1–2 developer-days per issue.

---

## Fix Duplicate Discriminants and Inconsistent Ranges in PermissionError Enum

- **Estimate:** 1 day
- **Context:** Recent pull request merges introduced duplicate discriminants (e.g. 410, 411, 2014, 2015, 2409, 2410, 2411) in `permissions/src/lib.rs`. This triggers rustc compiler errors E0081, E0004, and breaks the documented 2000-2999 error code allocation for the permissions contract.

### Data Types & Schemas
```rust
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum PermissionError {
    PermissionNotFound = 2001,
    Expired = 2002,
    ExceedsPerTxLimit = 2003,
    ExceedsTotalLimit = 2004,
    MerchantNotAllowed = 2005,
    Unauthorized = 2006,
    InvalidParam = 2007,
    PermissionPaused = 2008,
    AlreadyPaused = 2009,
    AlreadyActive = 2010,
    GrantsPaused = 2011,
    RelayerKeyNotSet = 2012,
    InvalidNonce = 2013,
    SignatureExpired = 2014,
    AlreadyGranted = 2015,
    SelfDelegationNotAllowed = 2401,
    InsufficientSignatures = 2402,
    UnknownSchema = 2403,
    ParentNotFound = 2404,
    ExceedsParentLimit = 2405,
    VelocityLimitExceeded = 2406,
    InactivityThresholdNotSet = 2407,
    PendingDecreaseExists = 2408,
    TimeLockActive = 2409,
    LimitBelowSpent = 2410,
    ExceedsAllowance = 2411,
    InvalidExpiry = 2412,
    NotInitialized = 2500,
}
```

### Tasks
- Clean up duplicate enum variants and discriminants in `permissions/src/lib.rs`.
- Enforce that all `PermissionError` variants fall strictly within the allocated range `2000..=2999`.
- Update unit tests in `error_code_tests` to verify uniqueness and absence of overlapping discriminant definitions.

### Acceptance Criteria
- `cargo check -p delego-permissions` passes without E0081 or E0004 errors.
- All error discriminants are distinct and documented in the contract specification.

---

## Resolve Corrupted Merge Syntax and Unclosed Delimiters in Escrow Contract

- **Estimate:** 1 day
- **Context:** A conflicted merge in `escrow/src/lib.rs` left duplicated `if let` blocks around lines 2137-2144 and unclosed delimiters in `metadata_tests` around line 4347, causing compiler failure across the entire workspace.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowMetadata {
    pub order_hash: BytesN<32>,
    pub schema: Symbol,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowMetadataEvent {
    pub escrow_id: u64,
    pub order_hash: BytesN<32>,
    pub schema: Symbol,
}
```

### Tasks
- Remove duplicate conditional bindings and corrupted braces in `escrow/src/lib.rs`.
- Ensure `order_hash` and `schema` metadata fields are properly saved to persistent storage.
- Fix test harness delimiters in `mod metadata_tests` so `cargo test -p delego-escrow` compiles.

### Acceptance Criteria
- `escrow/src/lib.rs` compiles cleanly without syntax errors or mismatched delimiter warnings.
- The `EscrowMetadataEvent` emits consistently with matching `escrow_id` types.

---

## Fix Malformed Unicode Characters and Parser Errors in Marketplace Contract

- **Estimate:** 1 day
- **Context:** The marketplace contract contains an invalid token `\u{b6}` replacing `&params` in `marketplace/src/lib.rs:2324`, and an unclosed delimiter in `DiscoveryPage` at line 1695, breaking compilation.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterMerchantParams {
    pub name: Symbol,
    pub metadata_uri: BytesN<32>,
    pub payout_address: Address,
    pub fee_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryPage {
    pub merchants: Vec<MerchantViewDetailed>,
    pub total: u32,
    pub next_cursor: Option<MerchantCursor>,
}
```

### Tasks
- Replace `¶ms` with `&params` at `marketplace/src/lib.rs:2324`.
- Fix unclosed parenthesis and braces in `get_merchant_discovery_paginated`.
- Verify `cargo check -p delego-marketplace` completes with zero errors.

### Acceptance Criteria
- The marketplace crate compiles cleanly on both target wasm32 and host test configurations.
- Merchant registration unit tests execute and pass.

---

## Resolve Mismatched Closing Delimiter and Broken Event Publishing in Reputation Contract

- **Estimate:** 1 day
- **Context:** In `reputation/src/lib.rs:1520`, `recompute_score` has a broken `env.events().publish(...)` call with mismatched braces and indentation, resulting in compiler failure.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationScore {
    pub entity: Address,
    pub score_bps: u32,
    pub positive_count: u32,
    pub negative_count: u32,
    pub last_updated_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct ReputationScoreUpdatedEvent {
    pub entity: Address,
    pub old_score_bps: u32,
    pub new_score_bps: u32,
    pub total_reviews: u32,
}
```

### Tasks
- Correct the closing braces in `reputation/src/lib.rs` inside `recompute_score`.
- Format the event payload for `ReputationScoreUpdatedEvent` with valid topic schema `(reputation, updated, entity)`.
- Add integration test verifying score recalculation after feedback submission.

### Acceptance Criteria
- `reputation` compiles cleanly with no syntax or compiler warnings.
- Tests verify events publish correctly with proper topics.

---

## Fix Unclosed Delimiter and Paginated Loop Invariant in Delegation Registry

- **Estimate:** 1 day
- **Context:** In `delegation_registry/src/lib.rs`, `get_delegation_history_paginated` and `get_expired_delegations_paginated` contain unclosed loop delimiters and broken while-loop bounds, preventing crate compilation.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationPage {
    pub items: Vec<DelegationRecord>,
    pub total: u32,
    pub next_offset: Option<u32>,
}
```

### Tasks
- Fix structural delimiters and braces in `delegation_registry/src/lib.rs`.
- Ensure the while loop terminates deterministically when `offset + limit >= total`.
- Verify pagination edge cases when total items is zero or less than requested limit.

### Acceptance Criteria
- `cargo check -p delego-delegation-registry` compiles without errors.
- Unit tests verify boundary conditions for paginated queries.

---

## Eliminate Duplicate Spec Function Generation for get_permission in Permissions Contract

- **Estimate:** 1 day
- **Context:** The macro expansion for `#[contractimpl]` in `permissions/src/lib.rs` triggers `error: __SPEC_XDR_FN_GET_PERMISSION must be defined only once in the value namespace of this module` due to duplicate method declarations of `get_permission`.

### Data Types & Schemas
```rust
#[contractimpl]
impl PermissionsContract {
    pub fn get_permission(env: Env, owner: Address, delegate: Address) -> Result<PermissionRecord, PermissionError> {
        // Implementation
    }
}
```

### Tasks
- Audit all `contractimpl` blocks in `permissions/src/lib.rs` for duplicate method signatures.
- Consolidate `get_permission` into a single canonical entrypoint.
- Confirm Soroban contract specification export generates clean XDR without collisions.

### Acceptance Criteria
- No duplicate spec function compiler errors.
- `soroban contract build` produces a valid wasm artifact without warning.

---

## Enforce Disjoint Error Code Ranges Across All 5 Contract Crates

- **Estimate:** 1 day
- **Context:** The workspace documentation specifies disjoint numeric error code spaces (Escrow 1000-1999, Permissions 2000-2999, Reputation 3000-3999, Delegation 4000-4999, Marketplace 5000-5999), but cross_contract.rs assertions fail due to overlapping and out-of-range discriminants.

### Data Types & Schemas
```rust
pub const ESCROW_RANGE: (u32, u32) = (1000, 1999);
pub const PERMISSION_RANGE: (u32, u32) = (2000, 2999);
pub const REPUTATION_RANGE: (u32, u32) = (3000, 3999);
pub const DELEGATION_RANGE: (u32, u32) = (4000, 4999);
pub const MARKETPLACE_RANGE: (u32, u32) = (5000, 5999);
```

### Tasks
- Audit all enum discriminants across all contract crates against their assigned ranges.
- Update `tests/cross_contract.rs` to assert strict range containment and pairwise disjointness.
- Fix any lingering legacy error codes outside the assigned blocks.

### Acceptance Criteria
- All contract errors strictly adhere to their allocated ranges.
- `cargo test --test cross_contract` passes without errors.

---

## Resolve Unused Variable Warnings and Dead Code in Permissions Velocity Limiter

- **Estimate:** 1 day
- **Context:** Unused variables `expires_at_ledger`, `velocity_key`, and `execution_time` generate compiler warnings that cause strict CI builds (`-D warnings`) to fail.

### Data Types & Schemas
```rust
#[contracttype]
pub enum DataKey {
    LastSpendLedger(Address, Address),
    VelocityInterval(Address, Address),
}
```

### Tasks
- Inspect usages of `velocity_key` and ensure the last spend ledger is properly written to persistent storage.
- Prefix or remove intentionally unused debug variables.
- Configure clippy and rustc flags to ensure clean workspace builds.

### Acceptance Criteria
- `cargo clippy --workspace --all-targets` runs with 0 warnings.
- Velocity limiting correctly updates the last spend ledger on each spend.

---

## Ensure Consistent no_std Conditional Compilation Across All Workspace Crates

- **Estimate:** 1 day
- **Context:** Several contract crates deviate in their `#![cfg_attr(not(test), no_std)]` declarations, leading to link errors when compiling to target wasm32-unknown-unknown.

### Data Types & Schemas
```rust
// Standardized prelude header for all contract lib.rs files
#![no_std]
#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]
```

### Tasks
- Standardize the crate root attributes across `escrow`, `permissions`, `reputation`, `delegation_registry`, and `marketplace`.
- Add a CI validation script to verify `cargo build --target wasm32-unknown-unknown --release`.
- Verify `tests/no_std_consistency.rs` validates all crates.

### Acceptance Criteria
- All 5 crates compile cleanly to Wasm without referencing std symbols.
- `no_std_consistency` test passes.

---

## Align Soroban SDK Dependency Versions and Feature Flags Across Workspace

- **Estimate:** 1 day
- **Context:** Different member crates specify differing patch versions or feature flags for `soroban-sdk`, leading to duplicate types and trait mismatch errors during cross-contract integration tests.

### Data Types & Schemas
```toml
[workspace.dependencies]
soroban-sdk = { version = "22.0.0", default-features = false }
```

### Tasks
- Consolidate `soroban-sdk` version declarations into the root `Cargo.toml` workspace dependencies.
- Ensure all member crates use `soroban-sdk.workspace = true`.
- Verify deterministic builds with updated `Cargo.lock`.

### Acceptance Criteria
- Only a single version of `soroban-sdk` exists in `Cargo.lock`.
- All workspace member crates share identical dependencies.

---

## Fix DataKey Serialization Collision in Delegation Registry Snapshot Storage

- **Estimate:** 1 day
- **Context:** The storage key for `DelegationSnapshot` conflicts with `DelegationRecord` when snapshot version matches record ID due to ambiguous tuple encoding in instance storage.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    Delegation(u64),
    Snapshot(u64, u32), // (delegation_id, version)
    OwnerDelegations(Address),
    NextDelegationId,
}
```

### Tasks
- Refactor `DataKey` enum to use distinct variant tags for records and snapshots.
- Add tests verifying that creating snapshots does not overwrite or corrupt base delegation records.
- Add migration helper or version check for stored keys.

### Acceptance Criteria
- No key collision occurs between snapshots and active delegations.
- Snapshot rollback tests execute successfully.

---

## Correct Missing Admin Initialization Guard in Permissions Contract

- **Estimate:** 1 day
- **Context:** Calling permissions admin functions before `set_admin` is invoked panics with unhandled storage key missing error instead of returning `PermissionError::NotInitialized` (2500).

### Data Types & Schemas
```rust
pub fn require_admin(env: &Env) -> Result<Address, PermissionError> {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(PermissionError::NotInitialized)
}
```

### Tasks
- Implement `require_admin` helper returning typed `PermissionError::NotInitialized`.
- Guard `pause_grants`, `resume_grants`, and `set_inactivity_threshold` with `require_admin`.
- Add unit tests verifying uninitialized admin calls return expected error.

### Acceptance Criteria
- Contract returns `PermissionError::NotInitialized` instead of panicking on uninitialized access.
- All admin-gated calls require prior initialization.

---

## Fix Type Conversion Overflow on Micro-Stroop Currency Calculations in Escrow

- **Estimate:** 1 day
- **Context:** When calculating yield and fees on token balances represented in 7-decimal Stroop units, intermediate multiplication of `i128` by basis points can overflow if balances exceed standard bounds.

### Data Types & Schemas
```rust
pub fn calculate_fee_and_yield(amount: i128, bps: u32) -> Result<i128, EscrowError> {
    amount
        .checked_mul(bps as i128)
        .and_then(|prod| prod.checked_div(10_000))
        .ok_or(EscrowError::MathOverflow)
}
```

### Tasks
- Replace unchecked integer arithmetic with `checked_mul` and `checked_div`.
- Introduce `EscrowError::MathOverflow = 1018` for arithmetic errors.
- Add edge-case unit tests with maximum `i128::MAX` boundary values.

### Acceptance Criteria
- Zero unhandled arithmetic panics on extreme numbers.
- Fee calculations return clean error on overflow.

---

## Fix Broken Multi-Contract Build Script and Target Directory Output

- **Estimate:** 1 day
- **Context:** The `justfile` and build scripts fail on CI because the wasm output path expects `target/wasm32-unknown-unknown/release/*.wasm` but cargo generates artifacts in workspace target cache.

### Data Types & Schemas
```bash
# In justfile
build-all:
    cargo build --target wasm32-unknown-unknown --release --workspace --exclude tests
```

### Tasks
- Update `justfile` recipes to correctly target workspace release wasm files.
- Add step to strip symbols with `soroban contract optimize` where available.
- Verify all 5 contract wasm files are generated in build output.

### Acceptance Criteria
- `just build-all` generates optimized wasm binaries for all contracts.
- CI build step succeeds without missing artifact errors.

---

## Fix Serialization Truncation in Marketplace Merchant Description Field

- **Estimate:** 1 day
- **Context:** The `metadata_uri` in `marketplace` is hardcoded as `BytesN<32>`, which fails when merchants supply IPFS base58/base32 CIDv1 strings or HTTPS URIs longer than 32 bytes.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantProfile {
    pub owner: Address,
    pub name: Symbol,
    pub metadata_uri: soroban_sdk::String,
    pub fee_bps: u32,
    pub registered_at: u64,
}
```

### Tasks
- Change `metadata_uri` from `BytesN<32>` to `soroban_sdk::String` with maximum length limit (e.g. 128 bytes).
- Add validation in `register_merchant` to reject oversized URIs.
- Update marketplace unit tests.

### Acceptance Criteria
- Merchants can supply standard IPFS and HTTPS URIs up to 128 characters.
- Exceeding URI length returns `MarketplaceError::InvalidUri`.

---

## Mitigate Cross-Contract Reentrancy in Escrow Release to Lending Contracts

- **Estimate:** 2 days
- **Context:** When releasing yield-bearing escrows (`YieldConfig`), the contract calls an external lending pool before updating internal escrow status to `Released`, exposing the contract to reentrant state exploitation.

### Data Types & Schemas
```rust
#[contracttype]
pub enum ReentrancyGuard {
    Unlocked,
    Locked,
}

// Checks-Effects-Interactions pattern
```

### Tasks
- Implement an instance-storage `ReentrancyGuard` or strict Checks-Effects-Interactions ordering.
- Update escrow status to `EscrowStatus::Released` and write to storage BEFORE invoking external lending contract withdrawals.
- Write a mock reentrant contract test simulating malicious re-entry.

### Acceptance Criteria
- Internal state is committed prior to external contract calls.
- Reentrant calls to `release` or `partial_release` are rejected with `EscrowError::ReentrancyDetected`.

---

## Mitigate Oracle Front-Running and Stale Report Attacks in evaluate_and_release

- **Estimate:** 2 days
- **Context:** `evaluate_and_release` accepts an oracle contract's resolution without checking the timestamp or sequence number of the oracle's assertion, allowing malicious actors to exploit stale or front-run oracle outcomes.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleAttestation {
    pub condition_met: bool,
    pub timestamp: u64,
    pub sequence: u64,
    pub signature_hash: BytesN<32>,
}
```

### Tasks
- Update the oracle interface to return an `OracleAttestation` struct with timestamp and freshness guarantee.
- Verify that `timestamp + MAX_ORACLE_STALENESS_SECS >= env.ledger().timestamp()`.
- Reject release if the oracle assertion is stale or timestamp is in the future.

### Acceptance Criteria
- Release is rejected if oracle report is older than configured threshold.
- Unit tests verify rejection of expired oracle answers.

---

## Implement Domain Separator and Chain ID Binding for Off-Chain Relayer Signatures

- **Estimate:** 2 days
- **Context:** Relayer-submitted signatures in the permissions contract verify a raw hash of parameters without including network passphrase or contract address, allowing replay attacks across Stellar testnet and mainnet.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignaturePayload {
    pub domain: Symbol,
    pub contract: Address,
    pub network_id: BytesN<32>,
    pub nonce: u64,
    pub deadline: u64,
    pub action_hash: BytesN<32>,
}
```

### Tasks
- Construct a canonical EIP-712-style `SignaturePayload` binding `env.current_contract_address()` and network ID.
- Verify off-chain signatures against the domain-separated payload hash.
- Enforce strict monotonic nonce incrementing per delegate address.

### Acceptance Criteria
- Signatures from different networks or contracts fail verification.
- Nonce replay attacks are reliably blocked.

---

## Prevent Allowance Double-Spending Race Conditions in Concurrent Delegated Spends

- **Estimate:** 2 days
- **Context:** Multiple relayer transactions submitted in the same ledger sequence could spend against the same remaining allowance if remaining amount is evaluated before subtracting in batch execution.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpendExecutionResult {
    pub remaining_allowance: i128,
    pub new_spend_ledger: u32,
}
```

### Tasks
- Implement atomic read-check-decrement in `spend_allowance`.
- Ensure allowance deduction is immediately committed to persistent storage before token transfer.
- Add concurrent spend simulation tests.

### Acceptance Criteria
- Concurrent spends exceeding the remaining allowance abort with `PermissionError::ExceedsTotalLimit`.
- No state discrepancy occurs during partial spends.

---

## Enforce Persistent Storage TTL Bumping on Active Escrows to Prevent Data Loss

- **Estimate:** 2 days
- **Context:** Escrows with long timeouts or ongoing disputes risk entry archival if the persistent storage TTL expires before fulfillment, causing loss of escrow funds on-chain.

### Data Types & Schemas
```rust
const ESCROW_STORAGE_THRESHOLD: u32 = 100_000; // ~6 days
const ESCROW_STORAGE_BUMP: u32 = 500_000;      // ~30 days

pub fn bump_escrow_storage(env: &Env, escrow_id: u64) {
    let key = DataKey::Escrow(escrow_id);
    env.storage().persistent().extend_ttl(&key, ESCROW_STORAGE_THRESHOLD, ESCROW_STORAGE_BUMP);
}
```

### Tasks
- Add storage TTL extension on all mutating actions: `create_escrow`, `deposit`, `dispute`, and `bump_ttl` entrypoint.
- Define an open keeper entrypoint `extend_escrow_ttl(escrow_id: u64)` allowing anyone to keep an active escrow alive.
- Add integration tests verifying TTL bump behavior.

### Acceptance Criteria
- Active and disputed escrows are protected against storage eviction.
- TTL bump helper functions are covered by tests.

---

## Prevent Inadvertent Arbitrary Token Escrow Hijacking via Token Contract Allowlist

- **Estimate:** 2 days
- **Context:** Currently, `create_escrow` accepts any `Address` as the token parameter. A malicious user can deploy a malicious fake token contract that reports successful transfers while stealing buyer deposits.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenAllowlistConfig {
    pub allowed_tokens: Vec<Address>,
    pub is_restricted: bool,
}
```

### Tasks
- Implement an optional admin-managed token allowlist in the escrow contract.
- Validate that `token` is in `allowed_tokens` when restriction is enabled.
- Allow native Stellar Classic Asset SAC contracts by default.

### Acceptance Criteria
- Escrow creation fails if token contract address is not allowlisted.
- Admin can add or remove tokens with event logging.

---

## Prevent Dispute Expiration Front-Running by Seller During Grace Period

- **Estimate:** 2 days
- **Context:** If a buyer initiates a dispute near the `timeout_ledger`, a seller can front-run the transaction by calling `claim_timeout_release`, bypassing buyer dispute resolution.

### Data Types & Schemas
```rust
pub const DISPUTE_GRACE_PERIOD_LEDGERS: u32 = 17_280; // ~24 hours

pub fn can_seller_claim(current_ledger: u32, timeout_ledger: u32, is_disputed: bool) -> bool {
    !is_disputed && current_ledger >= timeout_ledger + DISPUTE_GRACE_PERIOD_LEDGERS
}
```

### Tasks
- Introduce a mandatory dispute grace period between delivery declaration and seller timeout claiming.
- Ensure buyer dispute submission is accepted during the entire grace period.
- Add test verifying seller claim fails during active dispute or grace period.

### Acceptance Criteria
- Buyers have a guaranteed window to raise disputes without being front-run by seller timeout claims.
- Seller cannot claim funds while a dispute request is pending.

---

## Add require_auth Validation on All Delegation Revocation and Pause Entrypoints

- **Estimate:** 1 day
- **Context:** In `delegation_registry`, certain pause and resume helper functions failed to verify `owner.require_auth()`, allowing unauthorized callers to pause third-party delegations.

### Data Types & Schemas
```rust
pub fn pause_delegation(env: Env, delegation_id: u64) -> Result<(), DelegationError> {
    let mut record = get_delegation(&env, delegation_id)?;
    record.owner.require_auth();
    // mutate state
}
```

### Tasks
- Audit all public mutation methods in `delegation_registry/src/lib.rs` for explicit `require_auth()` calls.
- Ensure both `owner` and authorized admin can trigger emergency pause.
- Write authorization failure test cases for unauthorized callers.

### Acceptance Criteria
- Unauthorized calls fail with Soroban auth failure.
- Owners can pause and resume their own delegations.

---

## Mitigate Sybil Rating Inflation in Reputation Score Calculation

- **Estimate:** 2 days
- **Context:** The reputation contract allows any caller with an address to submit ratings for a merchant, enabling Sybil attacks to artificially inflate scores without verified order fulfillment.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedReviewProof {
    pub escrow_id: u64,
    pub escrow_contract: Address,
    pub order_hash: BytesN<32>,
}
```

### Tasks
- Require proof of completed escrow settlement before accepting a review for a merchant.
- Verify that `escrow_contract.get_escrow(escrow_id).status == EscrowStatus::Released`.
- Record `escrow_id` as used to prevent duplicate reviews from the same purchase.

### Acceptance Criteria
- Reviews without verified settled escrows are rejected with `ReputationError::UnverifiedOrder`.
- No double-reviewing for the same escrow.

---

## Enforce Maximum Batch Sweep Limits to Prevent Out-Of-Gas Denial of Service

- **Estimate:** 1 day
- **Context:** The `sweep_inactive` functions in `permissions` and `delegation_registry` allow arbitrary batch sizes, allowing transactions to hit CPU and RAM instructions limits and bricking automated sweep jobs.

### Data Types & Schemas
```rust
pub const MAX_SWEEP_BATCH_SIZE: u32 = 50;

pub fn sweep_inactive(env: Env, max_count: u32) -> Result<u32, PermissionError> {
    if max_count == 0 || max_count > MAX_SWEEP_BATCH_SIZE {
        return Err(PermissionError::InvalidParam);
    }
    // sweep logic
}
```

### Tasks
- Enforce a strict cap on `max_count` (50 items maximum) in sweep entrypoints.
- Return count of swept records and pagination cursor for next batch.
- Add benchmark tests for maximum sweep batch size within Soroban resource limits.

### Acceptance Criteria
- Requests with batch size > 50 fail with `InvalidParam`.
- Sweeps complete well within Soroban CPU/RAM instruction budgets.

---

## Mitigate Timelock Evasion on Permission Allowance Decreases

- **Estimate:** 2 days
- **Context:** A rogue delegate could attempt to reset an allowance decrease timelock by requesting rapid repeated limit changes, postponing the enforcement of owner-initiated limit reductions.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingDecrease {
    pub new_limit: i128,
    pub unlock_timestamp: u64,
    pub initiated_by: Address,
}
```

### Tasks
- Ensure only the account owner can initiate an allowance reduction.
- Prevent delegates from overwriting or canceling pending decreases.
- Automatically apply the lower limit immediately if the new requested limit is zero.

### Acceptance Criteria
- Owner allowance reductions cannot be obstructed by delegates.
- Unit tests verify immutability of active timelocked decrease requests.

---

## Fix Micro-Rounding Asset Leak in Escrow Platform Fee Distribution

- **Estimate:** 1 day
- **Context:** During partial releases, fee distribution rounds down integer division, leaving orphaned Stroop dust trapped in the escrow contract that can never be withdrawn.

### Data Types & Schemas
```rust
pub fn compute_fee_split(amount: i128, fee_bps: u32) -> (i128, i128) {
    let fee = (amount * fee_bps as i128) / 10_000;
    let seller_amount = amount - fee;
    (seller_amount, fee)
}
```

### Tasks
- Track residual rounding dust in contract instance storage.
- Sweep accumulated platform fees and rounding dust to treasury upon final release.
- Write balance conservation test: `seller_amount + fee == total_escrow_amount`.

### Acceptance Criteria
- Zero tokens remain trapped in contract after full release.
- Conservation of funds invariant holds across all fractional division tests.

---

## Implement Velocity Limit Check Against Ledger Sequence and Ledger Timestamp

- **Estimate:** 1 day
- **Context:** The permissions velocity limiter checks minimum intervals between spends. Relying solely on timestamps can be manipulated by validator drifts (~few seconds). Using ledger sequence provides deterministic rate limiting.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VelocityLimit {
    pub min_interval_ledgers: u32,
    pub last_spend_ledger: u32,
}
```

### Tasks
- Switch velocity interval calculation to evaluate `env.ledger().sequence()`.
- Reject spends where `current_ledger < last_spend_ledger + min_interval_ledgers`.
- Add tests asserting rejection of back-to-back spends within the restricted interval.

### Acceptance Criteria
- Velocity checks are deterministic and immune to clock drift.
- Rapid spends within forbidden window fail with `VelocityLimitExceeded`.

---

## Prevent Circular Parent-Child Delegation Chains in Hierarchical Permissions

- **Estimate:** 2 days
- **Context:** Hierarchical permissions allow child permissions to inherit limits from a parent. Without cycle detection, malicious delegates can construct circular dependencies (A -> B -> A) that cause infinite recursion and out-of-gas panics.

### Data Types & Schemas
```rust
pub const MAX_DELEGATION_DEPTH: u32 = 4;

pub fn validate_hierarchy_depth(env: &Env, start_parent: u64) -> Result<(), PermissionError> {
    // DFS or loop with max depth cap
}
```

### Tasks
- Enforce a maximum tree depth of 4 levels in permission creation.
- Implement cycle detection during parent assignment to reject self-referential chains.
- Write unit tests verifying circular grant attempts are rejected with `InvalidParam`.

### Acceptance Criteria
- Permission graphs cannot exceed depth 4 or contain cycles.
- No recursion stack overflow possible during permission validation.

---

## Implement Multi-Sig Quorum Verification for Emergency Contract Upgrades

- **Estimate:** 2 days
- **Context:** Upgrading contract WASM bytecode via `upgrade()` currently relies on a single admin address, creating a single point of failure and vulnerability to compromised keys.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradeProposal {
    pub new_wasm_hash: BytesN<32>,
    pub proposed_at: u64,
    pub approvals: Vec<Address>,
    pub executed: bool,
}
```

### Tasks
- Implement an upgrade proposal queue requiring `M-of-N` admin approvals.
- Add a 48-hour timelock between proposal submission and execution.
- Emit `ContractUpgradeProposed` and `ContractUpgraded` events.

### Acceptance Criteria
- Upgrades require threshold approvals and timelock expiration.
- Single key cannot unilaterally replace contract code.

---

## Sanitize and Bound String Inputs in Marketplace Product Listings

- **Estimate:** 1 day
- **Context:** Product title, category, and metadata strings accept unbounded length, allowing denial of service through storage bloat and exorbitant gas costs on listing fetches.

### Data Types & Schemas
```rust
pub const MAX_TITLE_LEN: u32 = 64;
pub const MAX_CATEGORY_LEN: u32 = 32;
pub const MAX_DESCRIPTION_LEN: u32 = 256;
```

### Tasks
- Add input length validations on all string parameters in marketplace listings.
- Return `MarketplaceError::StringTooLong` if limits are exceeded.
- Verify storage footprint remains deterministic per listing item.

### Acceptance Criteria
- Oversized listing strings are rejected immediately.
- Storage consumption per listing is strictly bounded.

---

## Prevent Flash-Loan Balance Spoofing in Yield-Bearing Escrow Accruals

- **Estimate:** 2 days
- **Context:** Lending pool APR calculations could be manipulated if an attacker flash-deposits capital right before escrow release to manipulate yield distribution formulas.

### Data Types & Schemas
```rust
pub fn calculate_time_weighted_yield(
    principal: i128,
    apr_bps: u32,
    held_seconds: u64
) -> Result<i128, EscrowError> {
    // Time-weighted yield independent of external instant balances
}
```

### Tasks
- Decouple yield calculation from instant external contract pool balances.
- Calculate yield strictly from agreed principal, time elapsed, and configured basis points.
- Cap maximum yield payout to a safe percentage of total escrow amount.

### Acceptance Criteria
- Yield accrual cannot be manipulated by flash loan liquidity spikes.
- Mathematical model matches time-weighted rate guarantees.

---

## Validate Positive Non-Zero Amounts on All Escrow Deposits and Grants

- **Estimate:** 1 day
- **Context:** Several functions allowed `amount == 0` or negative values due to signed integer conversions, creating zero-value spam records and corrupting arithmetic checks.

### Data Types & Schemas
```rust
pub fn validate_positive_amount(amount: i128) -> Result<(), EscrowError> {
    if amount <= 0 {
        return Err(EscrowError::InvalidAmount);
    }
    Ok(())
}
```

### Tasks
- Audit all deposit, refund, and allowance grant entrypoints for `amount > 0`.
- Return `EscrowError::InvalidAmount` or `PermissionError::InvalidParam` on non-positive input.
- Add test suite testing zero and negative parameter rejections.

### Acceptance Criteria
- Zero and negative amounts are rejected across all contracts.
- Zero-value spam cannot create storage entries.

---

## Enforce Seller-Specific Allowlist on Sensitive Permission Delegations

- **Estimate:** 1 day
- **Context:** When a buyer delegates spending power to an automated agent, unrestricted merchant lists could allow an exploited agent to drain funds to arbitrary unauthorized addresses.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantAllowlist {
    pub is_enabled: bool,
    pub merchants: Vec<Address>,
}
```

### Tasks
- Validate that `target_merchant` is present in `merchants` list when allowlist is active.
- Reject transactions to non-allowlisted merchants with `PermissionError::MerchantNotAllowed`.
- Cap allowlist size to `MAX_MERCHANTS_PER_PERMISSION` (25 addresses).

### Acceptance Criteria
- Spends targeting non-whitelisted merchants are blocked.
- Allowlist enforces maximum entry bound.

---

## Mitigate Nonce Squatting and Deadlocks in Relayer Transaction Submissions

- **Estimate:** 1 day
- **Context:** If a user signs an off-chain permission change with nonce `N` and it gets dropped or censored, all subsequent nonces `N+1` are permanently blocked until `N` is processed.

### Data Types & Schemas
```rust
pub fn cancel_nonce(env: Env, owner: Address, delegate: Address, nonce: u64) -> Result<(), PermissionError> {
    owner.require_auth();
    // Invalidate specific nonce or advance expected nonce
}
```

### Tasks
- Add `invalidate_nonce` or `bump_nonce_window` entrypoint authorized by the owner.
- Allow users to skip stalled nonces without revoking the entire delegation.
- Add test verifying unblocking of subsequent transactions after nonce invalidation.

### Acceptance Criteria
- Owners can invalidate stalled nonces directly on-chain.
- Subsequent transactions can proceed without deadlock.

---

## Prevent Replay of Escrow Cancellation Orders with Identical 32-Byte Order IDs

- **Estimate:** 1 day
- **Context:** Once an escrow is cancelled, its 32-byte order ID could potentially be reused to create a new escrow in a terminal state if internal tracking maps do not enforce uniqueness.

### Data Types & Schemas
```rust
pub fn assert_order_id_unique(env: &Env, order_id: &BytesN<32>) -> Result<(), EscrowError> {
    let key = DataKey::OrderIdUsed(order_id.clone());
    if env.storage().persistent().has(&key) {
        return Err(EscrowError::OrderIdAlreadyExists);
    }
    Ok(())
}
```

### Tasks
- Persist a permanent record of all used order IDs in persistent storage.
- Reject creation of new escrows with previously used order IDs.
- Write test case testing rejection of duplicate order IDs.

### Acceptance Criteria
- Order ID reuse is strictly rejected.
- Cancelled order IDs cannot be repurposed.

---

## Implement Dead-Man's Inactivity Switch for Orphaned Delegations

- **Estimate:** 2 days
- **Context:** If an owner loses access to their private key or abandons an agent delegation, funds associated with active allowances remain indefinitely committed unless an emergency recovery condition triggers.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InactivitySwitchConfig {
    pub recovery_address: Address,
    pub inactivity_ledgers: u32,
    pub last_heartbeat_ledger: u32,
}
```

### Tasks
- Allow owners to configure an optional recovery address and inactivity interval.
- Provide a `ping_heartbeat` method for owners.
- Allow recovery address to revoke delegation and reclaim allowance if inactivity interval has elapsed.

### Acceptance Criteria
- Recovery address can reclaim orphaned permissions after inactivity timeout.
- Heartbeat pings successfully reset the countdown timer.

---

## Add Cryptographic Oracle Delivery Attestation with Ed25519 Signature Verification

- **Estimate:** 2 days
- **Context:** Relying on simple bool flags from external contracts without cryptographic signature validation exposes escrow releases to malicious intermediate oracle contracts.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedDeliveryProof {
    pub escrow_id: u64,
    pub carrier_code: Symbol,
    pub tracking_hash: BytesN<32>,
    pub delivery_timestamp: u64,
    pub oracle_pubkey: BytesN<32>,
    pub signature: BytesN<64>,
}
```

### Tasks
- Verify oracle signature using `env.crypto().ed25519_verify` against configured oracle public key.
- Assert that `signed_payload.escrow_id == escrow_id` and timestamp is valid.
- Trigger automatic escrow release upon successful cryptographic verification.

### Acceptance Criteria
- Tampered delivery proofs or invalid signatures are rejected.
- Validly signed proofs release escrow funds immediately.

---

## Enforce Gas and Execution Resource Limits in Recursive Permission Auditing

- **Estimate:** 1 day
- **Context:** Retrieving the full audit trail of 200 entries in `permissions` exceeds the maximum Soroban memory allocation if serialized in a single transaction view call.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditTrailPage {
    pub entries: Vec<AuditEntry>,
    pub total_entries: u32,
    pub next_cursor: Option<u32>,
}
```

### Tasks
- Convert full audit log queries to chunked paginated access (max 20 entries per page).
- Store audit entries with chunked indexing in persistent storage.
- Verify memory and CPU consumption remain minimal during queries.

### Acceptance Criteria
- Audit queries never exceed Soroban VM memory limits.
- Pagination operates deterministically.

---

## Implement Emergency Freeze and Circuit Breaker for Marketplace Fraud Rings

- **Estimate:** 2 days
- **Context:** If a compromised merchant publishes malicious items or conducts fraudulent volume spikes, the marketplace contract lacks an immediate administrative freeze mechanism.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MerchantStatus {
    Active,
    Suspended,
    Banned,
}
```

### Tasks
- Add `MerchantStatus` to `MerchantProfile`.
- Allow admin or authorized risk oracle to freeze merchant trading instantly.
- Prevent escrow creations targeting suspended merchants.

### Acceptance Criteria
- Suspended merchants cannot accept new orders.
- Unfreeze requires multi-sig admin action.

---

## Protect Against Storage Key Exhaustion in Escrow Buyer Index Mapping

- **Estimate:** 1 day
- **Context:** Storing an unbounded `Vec<u64>` of all escrow IDs under a single `buyer_ids_key` in persistent storage causes the serialized storage entry to grow indefinitely, eventually exceeding the 64KB entry limit.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    BuyerEscrowCount(Address),
    BuyerEscrowAt(Address, u32), // (buyer, index) -> escrow_id
}
```

### Tasks
- Refactor buyer escrow index from monolithic `Vec<u64>` to indexed keys `BuyerEscrowAt(Address, u32)`.
- Track total count with `BuyerEscrowCount(Address)`.
- Update query methods to paginate through individual indices.

### Acceptance Criteria
- Buyers can create thousands of escrows without exceeding Soroban entry size limits.
- Storage access cost is O(1) per lookup.

---

## Prevent Authorization Bypass on Escrow Metadata Schema Registration

- **Estimate:** 1 day
- **Context:** The schema registration endpoint in `escrow` allowed any caller to register arbitrary schema symbols, potentially polluting the schema catalog with deceptive entries.

### Data Types & Schemas
```rust
pub fn register_schema(env: Env, schema: Symbol, schema_definition_uri: BytesN<32>) -> Result<(), EscrowError> {
    require_admin(&env)?;
    // register logic
}
```

### Tasks
- Enforce `require_admin` on schema registration.
- Emit `SchemaRegisteredEvent` with schema symbol and definition hash.
- Add tests rejecting unauthorized schema registrations.

### Acceptance Criteria
- Only admin can register approved metadata schemas.
- Unauthorized registration returns `EscrowError::Unauthorized`.

---

## Validate Expiration Ledgers Against Arithmetic Overflow on Grants

- **Estimate:** 1 day
- **Context:** Calculating `expires_at_ledger = current_ledger + ttl_ledgers` using unchecked 32-bit addition can wrap around if `ttl_ledgers` is set to `u32::MAX`, resulting in an instant expiration bypass.

### Data Types & Schemas
```rust
pub fn compute_expiry_ledger(current: u32, ttl: u32) -> Result<u32, PermissionError> {
    current.checked_add(ttl).ok_or(PermissionError::InvalidExpiry)
}
```

### Tasks
- Replace arithmetic addition with `checked_add`.
- Return `PermissionError::InvalidExpiry` (2412) on overflow.
- Add unit tests testing upper-bound TTL values.

### Acceptance Criteria
- Arithmetic overflow in TTL is handled gracefully.
- Grants with overflowing expiry ledgers are rejected.

---

## Enforce Rate-Limited Admin Parameter Changes via Time-Delayed Execution

- **Estimate:** 2 days
- **Context:** Immediate changes to fee basis points or grace periods can disrupt in-flight orders without notice to participating buyers and merchants.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingConfigChange {
    pub new_fee_bps: u32,
    pub effective_ledger: u32,
}
```

### Tasks
- Require a minimum notice window (e.g. 10,000 ledgers) for changes to platform fee rates.
- Emit `ConfigChangeScheduledEvent`.
- Apply new parameters only once `current_ledger >= effective_ledger`.

### Acceptance Criteria
- Fee rate changes are delayed and transparent.
- Existing orders use fees agreed at creation time.

---

## Prevent Front-Running of Escrow Refund Requests on Expired Orders

- **Estimate:** 1 day
- **Context:** When an escrow reaches `timeout_ledger`, both buyer refund and seller claim become valid. A malicious seller watching the mempool could front-run a buyer refund transaction with a payout claim.

### Data Types & Schemas
```rust
pub fn resolve_timeout_claim(env: Env, escrow_id: u64, caller: Address) -> Result<(), EscrowError> {
    caller.require_auth();
    // Prioritize buyer refund if shipment proof has not been confirmed on-chain
}
```

### Tasks
- Enforce that seller claim on timeout requires verified proof of shipment on-chain.
- If no shipment proof exists, give exclusive timeout refund rights to buyer.
- Add tests testing precedence of buyer refund over seller claim.

### Acceptance Criteria
- Sellers cannot claim funds on unshipped expired orders.
- Buyer refunds take deterministic precedence.

---

## Implement Milestone-Based Partial Escrow Releases with Completion Proofs

- **Estimate:** 2 days
- **Context:** Many service and multi-stage physical orders require staggered partial releases (e.g. 30% deposit, 40% dispatch, 30% delivery) rather than all-or-nothing payouts.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Milestone {
    pub milestone_id: u32,
    pub amount: i128,
    pub description: Symbol,
    pub is_completed: bool,
    pub completed_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MilestoneEscrowConfig {
    pub milestones: Vec<Milestone>,
}
```

### Tasks
- Implement `create_milestone_escrow` supporting an array of configured milestones.
- Add `release_milestone(escrow_id: u64, milestone_id: u32)` verifying authorization.
- Emit `MilestoneReleasedEvent` on partial disbursements.
- Mark escrow as terminal once all milestones are paid.

### Acceptance Criteria
- Funds are disbursed milestone-by-milestone according to schedule.
- Total milestone amounts must equal total escrowed principal.

---

## Implement Dispute Mediation Fee Escrow Allocation and Split Award Engine

- **Estimate:** 2 days
- **Context:** Dispute resolution currently supports only 100% refund or 100% release. Real-world arbitration requires split settlements (e.g. 70% refund to buyer, 30% release to seller minus mediator fee).

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeResolutionAward {
    pub buyer_amount: i128,
    pub seller_amount: i128,
    pub mediator_fee: i128,
    pub mediator_address: Address,
}
```

### Tasks
- Implement `resolve_dispute_split(escrow_id: u64, award: DisputeResolutionAward)`.
- Validate that `buyer_amount + seller_amount + mediator_fee == escrow.amount`.
- Execute three-way token transfers and emit `DisputeResolvedEvent`.

### Acceptance Criteria
- Arbitrators can disburse custom settlement ratios.
- All tokens are fully accounted for with zero remainder.

---

## Implement Batch Escrow Creation for Agent Multi-Item Checkouts

- **Estimate:** 2 days
- **Context:** Autonomous buyer agents purchasing across multiple merchants must execute sequential transactions, incurring extra latency and multiple signing popups.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchEscrowItem {
    pub seller: Address,
    pub token: Address,
    pub amount: i128,
    pub order_id: BytesN<32>,
    pub timeout_ledger: u32,
}
```

### Tasks
- Implement `batch_create_escrows(env: Env, items: Vec<BatchEscrowItem>) -> Vec<u64>`.
- Batch token transfers using allowance to minimize transfer overhead.
- Ensure atomic execution: if any item fails, the entire batch reverts.

### Acceptance Criteria
- Agents can initialize multiple escrows in a single Soroban transaction.
- Atomicity guarantees protect buyer from partial basket failures.

---

## Implement Decay-Weighted Reputation Scoring with Recency Bias

- **Estimate:** 2 days
- **Context:** Old reviews from inactive merchants permanently skew reputation scores. Implementing an exponential time decay ensures scores accurately reflect current merchant performance.

### Data Types & Schemas
```rust
pub fn calculate_decayed_score(
    historical_score: u32,
    ledgers_elapsed: u32,
    half_life_ledgers: u32
) -> u32 {
    // Fixed-point decayed score approximation
}
```

### Tasks
- Implement fixed-point half-life decay calculation in `reputation`.
- Weight recent reviews more heavily than older reviews.
- Add tests verifying score converges toward baseline over long inactivity.

### Acceptance Criteria
- Scores decay toward default neutral rating over time without active positive reviews.
- Recent ratings carry higher mathematical weight.

---

## Implement Cryptographic Merkle Root Proof Verification for Carrier Tracking

- **Estimate:** 2 days
- **Context:** To support high-scale carrier tracking without on-chain bloat, oracles should anchor a daily Merkle root of delivery events, allowing buyers/sellers to submit inclusion proofs.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerkleDeliveryProof {
    pub root: BytesN<32>,
    pub leaf: BytesN<32>,
    pub proof: Vec<BytesN<32>>,
    pub index: u32,
}
```

### Tasks
- Implement Merkle tree path verification function in Soroban.
- Add oracle entrypoint `publish_merkle_root(date: u64, root: BytesN<32>)`.
- Allow escrow release via valid Merkle proof submission.

### Acceptance Criteria
- Valid Merkle proofs authenticate delivery without calling oracle contract directly.
- Invalid Merkle proofs are rejected.

---

## Implement Automatic Token Allowance Reclamation on Expired Delegations

- **Estimate:** 1 day
- **Context:** When a delegation expires in `delegation_registry`, the token allowance granted on the Stellar Asset Contract remains open until explicitly reset by the owner.

### Data Types & Schemas
```rust
pub fn sweep_expired_allowance(
    env: Env,
    owner: Address,
    delegate: Address,
    token: Address
) -> Result<(), PermissionError> {
    // Reset SAC allowance to 0 if delegation is expired
}
```

### Tasks
- Invoke `token_client.approve(&owner, &delegate, &0, &current_ledger)` on expired delegations.
- Allow open execution by anyone (with small reward or zero-cost execution).
- Emit `AllowanceReclaimedEvent`.

### Acceptance Criteria
- Expired delegations have their on-chain SAC spending allowances wiped.
- Prevents dormant allowance exploitation.

---

## Implement Merchant Category Code (MCC) Spending Filters in Permissions

- **Estimate:** 2 days
- **Context:** Corporate expense policies require restricting delegated AI agent spending to specific merchant categories (e.g., travel, hardware, cloud services).

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CategoryPermission {
    pub allowed_mcc_codes: Vec<u32>,
    pub per_category_limit: i128,
}
```

### Tasks
- Integrate category validation using merchant registration data in marketplace.
- Check that merchant's MCC code matches authorized categories during `spend`.
- Return `PermissionError::MerchantCategoryNotAllowed` on mismatch.

### Acceptance Criteria
- Agents cannot spend against merchants outside their allowed category codes.
- Category restrictions can be combined with global limits.

---

## Implement Time-Of-Day and Business-Hour Spending Restrictions

- **Estimate:** 2 days
- **Context:** Allow buyers to limit agent purchase execution to business hours (e.g., 9 AM to 5 PM UTC, Monday to Friday) to ensure human oversight is available.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeWindowRestriction {
    pub start_hour_utc: u32,
    pub end_hour_utc: u32,
    pub allowed_days_bitmap: u32, // Bit flags for Mon-Sun
}
```

### Tasks
- Parse ledger timestamp into UTC day-of-week and hour.
- Verify current time falls within configured `TimeWindowRestriction`.
- Reject out-of-bounds spends with `PermissionError::OutsideAuthorizedWindow`.

### Acceptance Criteria
- Spends attempted outside approved hours fail.
- Valid business hour transactions execute normally.

---

## Implement Automated Storage TTL Keeper Bounty System

- **Estimate:** 2 days
- **Context:** To incentivize off-chain bots to extend persistent storage TTL on active contracts and escrows, implement a keeper fee payout in native XLM or token.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeeperBountyConfig {
    pub bounty_token: Address,
    pub bounty_amount: i128,
    pub min_threshold_ledgers: u32,
}
```

### Tasks
- Implement `bump_ttl_with_bounty(escrow_id: u64, keeper: Address)`.
- Disburse small keeper bounty if TTL was within threshold of expiration.
- Protect against bounty draining by enforcing minimum time between bumps.

### Acceptance Criteria
- Keepers receive small compensation for keeping contract storage entries warm.
- Bounty payout cannot exceed economic cap.

---

## Implement Dynamic Gas Fee Compensation for Oracle Callback Relayers

- **Estimate:** 2 days
- **Context:** Delivery oracles invoking `evaluate_and_release` pay transaction fees out of pocket. Adding a fee rebate from the escrow deposit compensates oracles sustainably.

### Data Types & Schemas
```rust
pub const ORACLE_REBATE_BPS: u32 = 10; // 0.1% or fixed stroop amount

pub fn disburse_oracle_rebate(
    env: &Env,
    token: &Address,
    oracle: &Address,
    amount: i128
) -> Result<i128, EscrowError> {
    // rebate logic
}
```

### Tasks
- Deduct a small pre-agreed gas compensation fee upon oracle-triggered release.
- Transfer compensation directly to `caller` (the oracle relayer).
- Update escrow accounting so seller receives remainder.

### Acceptance Criteria
- Oracles are automatically reimbursed for transaction execution costs.
- Compensation details are logged in release events.

---

## Implement Native Atomic Swap Escrow Between Stellar SAC Tokens

- **Estimate:** 2 days
- **Context:** Buyers funding in USDC should be able to settle with merchants who require payment in XLM or EURC via automated path payments or AMM pools inside the escrow.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrossCurrencySwapConfig {
    pub deposit_token: Address,
    pub payout_token: Address,
    pub max_slippage_bps: u32,
    pub router_contract: Address,
}
```

### Tasks
- Add cross-currency parameters to `EscrowRecord`.
- Invoke Stellar Soroban DEX / AMM router upon release to execute swap.
- Ensure slippage protection protects both buyer deposit and merchant payout.

### Acceptance Criteria
- Escrow supports funding in token A and releasing in token B.
- Excessive slippage reverts transaction.

---

## Implement Dual-Control Multi-Sig Escrow for High-Value Enterprise Orders

- **Estimate:** 2 days
- **Context:** Orders exceeding enterprise limits (e.g. >$10,000) require approval from both the buyer agent and a human enterprise finance manager before funds are released.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DualControlConfig {
    pub secondary_approver: Address,
    pub is_secondary_approved: bool,
    pub secondary_approved_at: u64,
}
```

### Tasks
- Add optional `secondary_approver` to `EscrowRecord`.
- Require `secondary_approver.require_auth()` in `approve_release`.
- Release funds only when both agent delivery signal and finance approval are present.

### Acceptance Criteria
- High-value orders cannot release on single agent authorization alone.
- Secondary approval workflow is verified by integration tests.

---

## Implement NFT Proof-of-Purchase Receipt Minting upon Escrow Completion

- **Estimate:** 2 days
- **Context:** Merchants and buyers require non-fungible on-chain receipts certifying warranty, authenticity, and legal transfer of ownership upon escrow completion.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PurchaseReceiptData {
    pub order_id: BytesN<32>,
    pub buyer: Address,
    pub seller: Address,
    pub amount: i128,
    pub completed_at: u64,
    pub item_sku: Symbol,
}
```

### Tasks
- Call external receipt minting contract upon `EscrowStatus::Released`.
- Transfer soulbound receipt token to buyer address.
- Store minted token ID in escrow record.

### Acceptance Criteria
- Buyer receives an immutable on-chain receipt upon completed purchase.
- Minting failures do not block fund settlement.

---

## Implement Standardized Metadata Hashing with SHA-256 Digest Verification

- **Estimate:** 1 day
- **Context:** The order hash field in `EscrowRecord` is unvalidated `BytesN<32>`. Adding standardized schema formatting ensures off-chain JSON orders cannot be modified post-deposit.

### Data Types & Schemas
```rust
pub fn verify_order_metadata_digest(
    env: &Env,
    raw_json_bytes: soroban_sdk::Bytes,
    expected_hash: &BytesN<32>
) -> bool {
    let computed = env.crypto().sha256(&raw_json_bytes);
    computed.to_bytes() == *expected_hash
}
```

### Tasks
- Implement on-chain hash verification helper.
- Provide sample client SDK code to compute deterministic order JSON digests.
- Add unit tests validating schema hashing.

### Acceptance Criteria
- Order hash matches cryptographic SHA-256 digest of canonical order payload.
- SDK and contract digest calculation are bit-for-bit identical.

---

## Implement Gas-Optimized Bitmaps for Delegation Permission Flags

- **Estimate:** 1 day
- **Context:** Storing individual boolean flags (`can_spend`, `can_refund`, `can_dispute`, `can_delegate`) in separate storage slots increases serialization costs. Storing them in a single `u32` bitmask cuts storage overhead by 70%.

### Data Types & Schemas
```rust
pub const PERM_FLAG_SPEND: u32    = 1 << 0;
pub const PERM_FLAG_REFUND: u32   = 1 << 1;
pub const PERM_FLAG_DISPUTE: u32  = 1 << 2;
pub const PERM_FLAG_DELEGATE: u32 = 1 << 3;

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PermissionBitmask(pub u32);
```

### Tasks
- Replace individual boolean fields with `PermissionBitmask`.
- Provide helper methods `has_flag`, `set_flag`, `clear_flag`.
- Benchmark gas savings during permission checks.

### Acceptance Criteria
- Storage size is reduced per delegation entry.
- Permission evaluation is faster and requires fewer CPU instructions.

---

## Implement Fuzzing Harness for Arithmetic and Limit Decrements in Permissions

- **Estimate:** 2 days
- **Context:** Spending limit math and time-locked decrements must be verified against unexpected inputs, wrapping overflows, and edge cases using automated fuzz testing.

### Data Types & Schemas
```rust
#[derive(Arbitrary, Debug)]
pub struct FuzzSpendOperation {
    pub initial_limit: i128,
    pub spends: Vec<i128>,
    pub elapsed_ledgers: Vec<u32>,
}
```

### Tasks
- Write a `cargo-fuzz` or `bolero` property-based test harness for `spend_allowance`.
- Assert invariants: remaining allowance never becomes negative and total spent equals sum of valid spends.
- Integrate fuzz targets into repository testing suite.

### Acceptance Criteria
- Fuzz harness runs for 100,000 iterations without panics or invariant violations.
- All math edge cases are verified.

---

## Implement Formal Verification Specifications for Escrow Lifecycle Invariants

- **Estimate:** 2 days
- **Context:** Verify critical invariants (e.g. total released + refunded <= deposited amount, terminal states are irrevocable) using formal specifications.

### Data Types & Schemas
```rust
// Invariant 1: Conservation of value
// escrow.released_amount + escrow.refunded_amount <= escrow.amount

// Invariant 2: Terminal State Irrevocability
// If status in [Released, Refunded, Cancelled], state transitions are strictly disabled
```

### Tasks
- Draft formal invariant test harnesses testing state machine transitions.
- Test all illegal transition paths (e.g., Refunded -> Released).
- Document mathematical proof sketch in `docs/architecture/contracts.md`.

### Acceptance Criteria
- Zero illegal state transitions succeed in property-based test suite.
- Value conservation invariant holds under all test conditions.

---

## Implement Native Contract Event Topic Filtering for Off-Chain Indexers

- **Estimate:** 1 day
- **Context:** Ensure all events in `marketplace`, `permissions`, and `delegation_registry` follow the 3-topic Soroban convention `(contract, action, entity_id)` to optimize Horizon/RPC ingestion filters.

### Data Types & Schemas
```rust
// Standard topic format:
// Topic 0: Symbol (e.g. symbol_short!("escrow"))
// Topic 1: Symbol (e.g. symbol_short!("created"))
// Topic 2: Entity ID (e.g. u64 or Address)
```

### Tasks
- Audit all `env.events().publish()` calls across the five contract crates.
- Standardize topic formats so RPC filter queries do not require body parsing.
- Add unit tests asserting event topic structure and payload types.

### Acceptance Criteria
- Event indexers can subscribe to entity-level events directly via RPC filter topics.
- All event schemas are documented.

---

## Implement Cross-Contract Interface Adapters for External DeFi Protocols

- **Estimate:** 2 days
- **Context:** To support yield integration with protocols like Blend or Aqua, implement standardized trait adapters that isolate external protocol changes from core escrow logic.

### Data Types & Schemas
```rust
#[soroban_sdk::contractclient(name = "LendingPoolClient")]
pub trait LendingPoolInterface {
    fn deposit(env: Env, token: Address, amount: i128) -> Result<i128, InvokeError>;
    fn withdraw(env: Env, token: Address, shares: i128) -> Result<i128, InvokeError>;
    fn get_accrued_yield(env: Env, user: Address) -> i128;
}
```

### Tasks
- Define `LendingPoolInterface` trait in a shared contract interface crate.
- Implement mock lending pool for unit and integration testing.
- Handle contract invocation errors gracefully without aborting main escrow state.

### Acceptance Criteria
- Core escrow does not depend directly on vendor-specific lending logic.
- Mock adapter enables local simulation of yield accrual.

---

## Implement Graceful Fallback for Failed External Oracle Invocations

- **Estimate:** 1 day
- **Context:** If an external oracle contract experiences an unhandled panic or is paused, `evaluate_and_release` must not trap the caller; it should return a structured error allowing subsequent retries.

### Data Types & Schemas
```rust
pub fn safe_oracle_call(env: &Env, oracle: &Address, condition: Symbol) -> Result<bool, EscrowError> {
    // Catch cross-contract invoke errors
}
```

### Tasks
- Wrap cross-contract oracle calls in error-handling wrappers.
- Map external call failures to `EscrowError::OracleInvocationFailed` (1019).
- Add tests simulating a reverted oracle call.

### Acceptance Criteria
- Oracle panics do not cause permanent contract bricking.
- Clear error signals guide caller to retry or initiate dispute.

---

## Implement Dynamic Platform Fee Tiering Based on Merchant Volume

- **Estimate:** 2 days
- **Context:** High-volume merchants should automatically receive discounted platform fee tiers on-chain based on their trailing 30-day settled transaction volume.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeTier {
    pub min_volume: i128,
    pub fee_bps: u32,
}
```

### Tasks
- Store merchant settled volume in persistent storage.
- Evaluate applicable fee tier dynamically upon escrow creation.
- Emit `FeeTierUpdatedEvent` when merchant achieves a higher volume bracket.

### Acceptance Criteria
- High-volume merchants pay reduced basis points automatically.
- Volume tracking updates accurately on each successful escrow release.

---

## Implement Reversible Time-Locked Emergency Admin Actions with Veto Period

- **Estimate:** 2 days
- **Context:** Administrative actions such as changing fee recipient or modifying contract whitelist should undergo a 24-hour pending period where a security council can veto suspicious operations.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingAdminAction {
    pub action_type: Symbol,
    pub payload: BytesN<32>,
    pub unlock_ledger: u32,
    pub is_vetoed: bool,
}
```

### Tasks
- Queue administrative state modifications in `PendingAdminAction`.
- Allow designated security guardian address to set `is_vetoed = true`.
- Execute action only after `unlock_ledger` passes without veto.

### Acceptance Criteria
- Critical administrative actions are delayed by 24 hours.
- Security guardians can cancel malicious or erroneous admin actions.

---

## Implement Ephemeral Delegated Session Keys with Bounded Ledger Windows

- **Estimate:** 2 days
- **Context:** Autonomous agents should operate using ephemeral session keys valid for short durations (e.g. 1 hour = ~720 ledgers) that automatically become invalid without explicit on-chain revocation.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionKeyConfig {
    pub session_key: Address,
    pub max_spend_per_session: i128,
    pub valid_until_ledger: u32,
}
```

### Tasks
- Implement `grant_ephemeral_session` in `permissions`.
- Validate that `current_ledger <= valid_until_ledger` on each spend.
- Automatically reject spends from expired session keys without requiring cleanup.

### Acceptance Criteria
- Ephemeral session keys cannot spend past their designated ledger expiration.
- Session keys do not leave persistent storage waste.

---

## Implement Automated Storage Recovery Sweep for Terminal Escrow Records

- **Estimate:** 1 day
- **Context:** Once an escrow reaches a terminal state (`Released`, `Refunded`, `Cancelled`) and its retention window passes, its persistent storage entries can be archived or cleaned to reclaim rent.

### Data Types & Schemas
```rust
pub const ARCHIVAL_RETENTION_LEDGERS: u32 = 518_400; // ~30 days

pub fn archive_terminal_escrow(env: Env, escrow_id: u64) -> Result<(), EscrowError> {
    // Check terminal state and retention elapsed, clean keys
}
```

### Tasks
- Add `archive_terminal_escrow` entrypoint.
- Emit `EscrowArchivedEvent` before removing large auxiliary records.
- Verify storage rent reclamation on testnet.

### Acceptance Criteria
- Old terminal escrows can be cleaned after the archival retention period.
- Active and disputed escrows are strictly protected from premature deletion.

---

## Implement Comprehensive Cross-Contract Integration Benchmark Suite

- **Estimate:** 2 days
- **Context:** A comprehensive end-to-end benchmark tracking gas, CPU instructions, and memory usage across a full flow: Register Merchant -> Grant Permission -> Create Escrow -> Deliver -> Release.

### Data Types & Schemas
```rust
#[test]
fn benchmark_full_e2e_commerce_flow() {
    // Setup env, deploy all 5 contracts, simulate full commerce loop, assert resource limits
}
```

### Tasks
- Construct a complete multi-contract test harness in `tests/cross_contract.rs`.
- Assert CPU and RAM consumption stays under 50% of the Soroban single-transaction ceiling.
- Track benchmark regressions in CI output.

### Acceptance Criteria
- Complete multi-contract checkout and release executes within Soroban resource limits.
- Benchmark test runs in continuous integration.

---

## Implement Safe Token Transfer Wrapper with Return Value Sanitization

- **Estimate:** 1 day
- **Context:** [escrow] Ensure token transfer calls to both SAC and custom Soroban token contracts handle optional boolean return values safely.

### Data Types & Schemas
```rust
pub fn safe_token_transfer(env: &Env, token: &Address, to: &Address, amount: i128) -> Result<(), EscrowError> {
    // token transfer wrapper
}
```

### Tasks
- Implement wrapper around token transfer
- Handle void and bool return types
- Add test cases with mock token

### Acceptance Criteria
- `Implement Safe Token Transfer Wrapper with Return Value Sanitization` implemented according to technical specification.
- Comprehensive unit and integration tests pass.

---

## Implement Re-Entrancy Safe State Rollback on External Call Failure

- **Estimate:** 2 days
- **Context:** [escrow] Prevent partial state mutations when a downstream contract invocation fails midway through escrow execution.

### Data Types & Schemas
```rust
pub fn execute_atomic_operation<F, T>(env: &Env, f: F) -> Result<T, EscrowError> where F: FnOnce() -> Result<T, EscrowError> {
    // atomic execution
}
```

### Tasks
- Audit state rollback mechanics
- Ensure storage is pristine on failure
- Add failing integration test

### Acceptance Criteria
- `Implement Re-Entrancy Safe State Rollback on External Call Failure` implemented according to technical specification.
- Comprehensive unit and integration tests pass.

---

## Add Nonce Invalidation Batch Method for Compromised Agent Keychains

- **Estimate:** 1 day
- **Context:** [permissions] Allow users to invalidate a range of nonces immediately if an agent private key is suspected of being compromised.

### Data Types & Schemas
```rust
pub fn invalidate_nonce_range(env: Env, owner: Address, delegate: Address, up_to_nonce: u64) -> Result<(), PermissionError> {
    // invalidate nonces
}
```

### Tasks
- Add range invalidation entrypoint
- Verify owner auth
- Add tests asserting previous nonces fail

### Acceptance Criteria
- `Add Nonce Invalidation Batch Method for Compromised Agent Keychains` implemented according to technical specification.
- Comprehensive unit and integration tests pass.

---

## Implement Secondary Approver Expiration in Enterprise Dual-Control Escrow

- **Estimate:** 1 day
- **Context:** [escrow] If a secondary approver fails to sign within a configured time limit, allow order fallback to dispute or refund.

### Data Types & Schemas
```rust
#[contracttype]
pub struct DualControlTimeout {
    pub approver_deadline_ledger: u32,
    pub fallback_action: EscrowStatus,
}
```

### Tasks
- Implement approver deadline check
- Execute fallback on timeout
- Add unit test

### Acceptance Criteria
- `Implement Secondary Approver Expiration in Enterprise Dual-Control Escrow` implemented according to technical specification.
- Comprehensive unit and integration tests pass.

---

## Implement Dynamic Slippage Limits in Cross-Currency Path Payment Escrow

- **Estimate:** 2 days
- **Context:** [escrow] Prevent front-running sandwich attacks on path payments by enforcing maximum slippage tolerance on DEX routes.

### Data Types & Schemas
```rust
#[contracttype]
pub struct SlippageBounds {
    pub min_output_amount: i128,
    pub max_input_amount: i128,
}
```

### Tasks
- Check slippage against DEX execution result
- Revert if bounds exceeded
- Add simulated sandwich attack test

### Acceptance Criteria
- `Implement Dynamic Slippage Limits in Cross-Currency Path Payment Escrow` implemented according to technical specification.
- Comprehensive unit and integration tests pass.

---

