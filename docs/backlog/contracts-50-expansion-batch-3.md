# Smart Contracts 50 Expansion Issues Backlog (Batch 3) (50 Issues)

Developer-ready smart contract issues covering Soroban compilation regressions, re-entrancy prevention, partial refund split settlements, merchant security deposits, delegation velocity limiters, and formal property-based invariant testing.

**Total Issues:** 50
**Target Sizing:** 2–3 developer-days per issue.
**Prerequisites:** All issues include explicit Soroban Rust structs, enums, contract errors, tasks, and acceptance criteria.

---

## Fix Duplicate NotFlagReporter Variant in ReputationError Enum Causing Non-Exhaustive Macro Errors

- **Estimate:** 2 days
- **Context:** In `reputation/src/lib.rs:325`, `NotFlagReporter` is declared twice with conflicting integer representations (`0x0003_000B` vs `3011`). This causes `error[E0004]: non-exhaustive patterns: &ReputationError::NotFlagReporter not covered` and breaks `cargo check --tests` for the entire workspace.

### Data Types & Schemas
```rust
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ReputationError {
    AlreadyInitialized = 0x0003_0001,
    Unauthorized = 0x0003_0002,
    EntityNotFound = 0x0003_0003,
    ScoreOverflow = 0x0003_0004,
    InvalidWeight = 0x0003_0005,
    CooldownActive = 0x0003_0006,
    DuplicateFlag = 0x0003_0007,
    SlashExceedsBond = 0x0003_0008,
    ReviewWindowExpired = 0x0003_0009,
    InvalidEscrowProof = 0x0003_000A,
    NotFlagReporter = 0x0003_000B,
    UnverifiedOrder = 0x0003_000C,
}
```

### Tasks
- Remove duplicate `NotFlagReporter = 3011` declaration in `reputation/src/lib.rs`.
- Standardize all error codes to canonical hex prefix `0x0003_XXXX`.
- Update match statements in contract methods to exhaustively cover all enum variants.
- Verify `cargo check --tests -p delego-reputation` compiles cleanly with zero warnings.

### Acceptance Criteria
- `reputation/src/lib.rs` compiles without non-exhaustive pattern errors.
- All unit and integration tests in `reputation` pass.

---

## Fix Cross-Contract Call Reentrancy Guard in Escrow Deposit Method

- **Estimate:** 2 days
- **Context:** In `escrow/src/lib.rs`, when the escrow contract invokes the external Soroban token client `transfer_from()`, state mutations marking the escrow as funded occur after the external token call. Apply Checks-Effects-Interactions pattern and transient reentrancy flags to protect against malicious token contracts.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReentrancyStatus {
    NotEntered,
    Entered,
}

#[contracttype]
pub enum DataKey {
    ReentrancyGuard,
    Escrow(u64),
}
```

### Tasks
- Implement reentrancy guard struct in `escrow/src/storage.rs` using transient storage.
- Verify status is `NotEntered` and set to `Entered` before calling `token::Client::transfer_from`.
- Reset status to `NotEntered` upon function completion or unwinding.
- Add integration test simulating reentrant token callback.

### Acceptance Criteria
- Reentrant calls into `deposit()` or `release()` abort with `EscrowError::ReentrancyDetected`.
- Standard deposit flows execute with negligible gas overhead (< 500 CPU instructions).

---

## Fix Transient Storage Key TTL Eviction in Marketplace Listing Cache

- **Estimate:** 2 days
- **Context:** The marketplace contract caches active merchant listings in temporary/instance storage. Under Soroban Protocol 21, keys without explicit TTL extension are evicted after 4,096 ledgers, resulting in sudden 'ListingNotFound' errors for long-standing products.

### Data Types & Schemas
```rust
pub const LISTING_BUMP_LEDGERS: u32 = 518_400; // ~30 days
pub const LISTING_THRESHOLD_LEDGERS: u32 = 172_800; // ~10 days

#[contracttype]
pub struct ListingStorageMetadata {
    pub merchant: Address,
    pub price_stroops: i128,
    pub stock: u32,
    pub last_bumped_ledger: u32,
}
```

### Tasks
- Add `env.storage().instance().extend_ttl()` in `marketplace/src/lib.rs` on all read and update operations.
- Define constant TTL bump thresholds in `marketplace/src/constants.rs`.
- Implement automated touch/refresh method `bump_listing_ttl()` for indexers.
- Add unit tests verifying storage TTL extension behavior using Soroban test environment.

### Acceptance Criteria
- Listings remain active across ledger boundaries without unexpected eviction.
- TTL bump functions execute automatically on every purchase and inventory query.

---

## Fix Arithmetic Overflow in Staked Reputation Exponential Decay Calculation

- **Estimate:** 2 days
- **Context:** In `reputation/src/scoring.rs`, calculating historical penalty decay using fixed-point exponentiation (`score * (decay_factor ^ elapsed_epochs)`) overflows `i128` if elapsed epochs exceed 32, causing a contract panic during reputation queries.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationDecayConfig {
    pub half_life_epochs: u32,
    pub min_decay_floor: u32, // Basis points
    pub max_epochs_supported: u32,
}
```

### Tasks
- Replace naive exponentiation with iterative or binary square-and-multiply fixed-point math.
- Cap `elapsed_epochs` at `max_epochs_supported` to bound calculation within safe bounds.
- Use checked arithmetic (`checked_mul`, `checked_div`) with fallback to `min_decay_floor`.
- Add unit tests with large epoch spans (e.g. 100+ epochs) asserting zero panics.

### Acceptance Criteria
- Reputation decay computation never overflows for any valid historical ledger interval.
- Decayed scores gracefully approach mathematical floor without panicking.

---

## Fix Nonce Increment Desynchronization in Multi-Sig Off-Chain Signature Verifier

- **Estimate:** 2 days
- **Context:** In `permissions/src/multisig.rs`, if an off-chain co-signed transaction envelope fails authorization during a sub-call, the smart account nonce increments in contract storage while the client-side nonce remains unconsumed, deadlocking subsequent signatures.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiSigNonceRecord {
    pub account: Address,
    pub current_nonce: u64,
    pub last_used_timestamp: u64,
}
```

### Tasks
- Defer nonce increment until all co-signer authorizations and sub-calls successfully complete.
- Ensure transaction reverts fully restore previous nonce state upon any authorization failure.
- Provide explicit `get_nonce(account)` query function for co-signers to synchronize.
- Write tests simulating authorization rejections and verifying nonce consistency.

### Acceptance Criteria
- Failed transactions leave account nonce unchanged.
- Successful transactions increment nonce by exactly 1.

---

## Fix Missing Ledger Sequence Boundary Checks in Ephemeral Session Key Expiration

- **Estimate:** 2 days
- **Context:** In `delegation_registry/src/lib.rs`, session key validation checks wall-clock timestamp `env.ledger().timestamp()` instead of ledger sequence number. Because testnets and validators can experience minor NTP clock skew, session keys can be falsely rejected or prematurely validated.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EphemeralSessionBounds {
    pub start_ledger: u32,
    pub expiry_ledger: u32,
    pub max_spending_limit: i128,
}
```

### Tasks
- Update session validity check to enforce `start_ledger <= current_ledger && current_ledger <= expiry_ledger`.
- Support dual checks with timestamp fallback for UI friendly display.
- Emit `SessionExpired` event when a call is attempted on an expired ledger.
- Add unit tests verifying deterministic rejection on exact boundary ledgers.

### Acceptance Criteria
- Session expiration evaluates strictly against immutable Soroban ledger sequence numbers.
- Clock skew cannot validate expired sessions or reject active ones.

---

## Fix Inconsistent Error Propagation in Cross-Contract Token Transfer Wrapper

- **Estimate:** 2 days
- **Context:** In `escrow/src/token.rs`, errors from Soroban SAC (Stellar Asset Contract) token transfers are swallowed or converted into generic `EscrowError::InternalError`, obscuring crucial failure reasons such as `InsufficientBalance` or `Unauthorized`.

### Data Types & Schemas
```rust
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum TokenTransferError {
    InsufficientBalance = 0x0001_0020,
    AllowanceExceeded = 0x0001_0021,
    TokenFrozen = 0x0001_0022,
    InvalidTokenContract = 0x0001_0023,
}
```

### Tasks
- Parse Soroban token client `Result<(), Error>` and map standard SAC error codes to typed escrow errors.
- Emit detailed diagnostic events prior to transaction abort.
- Update caller methods to bubble up exact token failure codes to client.
- Add test cases testing insufficient balance and bad allowance conditions.

### Acceptance Criteria
- Client calls receive explicit error codes indicating token-level failures.
- Zero generic internal panics on expected token balance errors.

---

## Fix Storage Footprint Bloat by Purging Settled Escrow State into Archive Footprint

- **Estimate:** 2 days
- **Context:** Settled and cancelled escrows remain indefinitely in Soroban contract persistent storage, incurring ongoing rent fees and bloating ledger size. Implement an archiving function that moves settled escrows to TTL-decaying instance storage or purges them after 180 days.

### Data Types & Schemas
```rust
#[contracttype]
pub struct EscrowArchiveHeader {
    pub escrow_id: u64,
    pub settled_ledger: u32,
    pub final_settlement_hash: BytesN<32>,
}
```

### Tasks
- Implement `archive_settled_escrow(escrow_id)` callable after 100,000 ledgers post-settlement.
- Delete verbose order line-items from persistent storage while retaining lightweight archive hash.
- Emit `EscrowArchived` event containing full state digest for off-chain indexers.
- Verify storage rent footprint reduction in Soroban budget metrics.

### Acceptance Criteria
- Settled escrow storage reclaimed from persistent contract space.
- Archive hash remains queryable to verify historical settlement authenticity.

---

## Implement Multi-Party Partial Refund Settlement Method with Split Disbursement

- **Estimate:** 3 days
- **Context:** When a buyer and merchant resolve an order dispute via negotiated compromise, the escrow contract must atomically transfer a fraction of funds to the buyer and the remainder to the merchant, deducting prorated platform fees in a single atomic transaction.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SplitSettlementRequest {
    pub escrow_id: u64,
    pub buyer_refund_amount: i128,
    pub merchant_payout_amount: i128,
    pub buyer_signature: BytesN<64>,
    pub merchant_signature: BytesN<64>,
}
```

### Tasks
- Implement `settle_partial_refund(env, request)` in `escrow/src/lib.rs`.
- Verify that `buyer_refund_amount + merchant_payout_amount + prorated_fee == total_escrow_deposit`.
- Verify signatures of both buyer and merchant against contract authorization.
- Execute split token transfers and transition escrow status to `SettledWithPartialRefund`.

### Acceptance Criteria
- Split disbursement executes atomically with zero rounding dust loss.
- Both buyer and merchant receive agreed fractions in a single transaction.

---

## Implement Progressive Escrow Tranche Unlocking Bound to Courier Checkpoint Milestones

- **Estimate:** 3 days
- **Context:** For large industrial orders or custom manufacturing, buyers and merchants require staged escrow payouts (e.g. 30% upon carrier pickup, 40% upon customs clearance, 30% upon final delivery confirmation).

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowTranche {
    pub tranche_index: u32,
    pub amount_stroops: i128,
    pub checkpoint_hash: BytesN<32>,
    pub is_released: bool,
    pub release_ledger: u32,
}
```

### Tasks
- Add tranches vector to `EscrowContract` data model.
- Implement `unlock_tranche(escrow_id, tranche_index, proof_signature)`.
- Enforce that tranches must be unlocked in sequential order.
- Add integration tests verifying partial payouts and remaining escrow balance integrity.

### Acceptance Criteria
- Merchants can withdraw milestone tranches upon verified carrier attestation.
- Remaining funds stay safely locked until final milestone completion.

---

## Implement Mutual Consent Timelock Extension for Delayed In-Transit Orders

- **Estimate:** 2 days
- **Context:** When international shipments encounter customs delays, the automatic escrow release timelock may expire prematurely. Implement a method allowing both parties to extend the dispute grace period by mutual on-chain consent.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimelockExtensionAgreement {
    pub escrow_id: u64,
    pub additional_ledgers: u32,
    pub buyer_approved: bool,
    pub merchant_approved: bool,
}
```

### Tasks
- Add `propose_timelock_extension(escrow_id, additional_ledgers)` in `escrow/src/lib.rs`.
- Add `approve_timelock_extension(escrow_id)` requiring counterparty signature.
- Extend `auto_release_ledger` when both approvals are confirmed.
- Emit `TimelockExtended` event with new deadline.

### Acceptance Criteria
- Timelock increases only when both parties grant authorization.
- Prevents premature auto-release while goods are legitimately in transit.

---

## Implement Sliding Platform Commission Discount Based on Cumulative Merchant Settlements

- **Estimate:** 3 days
- **Context:** High-volume merchants qualify for tiered fee discounts (standard: 1.5%, silver: 1.0%, gold: 0.5%). The escrow contract must query historical settlement volume from the reputation/merchant registry and calculate fee deductions dynamically.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeTierConfig {
    pub volume_threshold_stroops: i128,
    pub fee_basis_points: u32, // e.g. 150 for 1.5%
}
```

### Tasks
- Store tiered fee table in escrow contract administrative storage.
- Query merchant lifetime settled volume during `initialize_escrow`.
- Compute fee using corresponding basis points tier.
- Write tests verifying fee calculation across tier thresholds (0, 10k USDC, 100k USDC).

### Acceptance Criteria
- High-volume merchants receive discounted commission rates automatically.
- Calculations use safe integer scaling with zero division errors.

---

## Implement Escrow Cancellation by Mutual Authorization Before Courier Pickup

- **Estimate:** 2 days
- **Context:** If a buyer accidentally places an order or merchant cannot fulfill it, provide a clean cooperative cancellation function that returns 100% of escrowed funds to the buyer with zero platform fees.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutualCancellation {
    pub escrow_id: u64,
    pub cancelled_ledger: u32,
    pub reason_code: Symbol,
}
```

### Tasks
- Implement `cancel_mutual(env, escrow_id)` requiring both buyer and merchant authorization.
- Verify escrow has not yet transitioned to `Shipped` or `Disputed`.
- Transfer total deposited balance back to buyer address.
- Transition escrow status to `CancelledByMutualConsent`.

### Acceptance Criteria
- Full deposit refunds to buyer without penalizing either party.
- Attempts to cancel after shipping flag is set are rejected.

---

## Implement Multi-Recipient Escrow Disbursement Matrix for Bundled Shopping Carts

- **Estimate:** 3 days
- **Context:** When a buyer authorizes a multi-item purchase involving multiple sellers, execute atomic multi-contract disbursements from a single master checkout deposit, routing payments to distinct merchant accounts in one ledger transaction.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecipientDisbursement {
    pub merchant: Address,
    pub amount_stroops: i128,
    pub fee_stroops: i128,
    pub sub_order_id: u64,
}
```

### Tasks
- Implement `create_bundled_escrow(env, buyer, disbursements: Vec<RecipientDisbursement>)`.
- Collect total sum in single `transfer_from` from buyer smart account.
- Instantiate atomic sub-escrow tracking records for each seller.
- Verify all sub-escrows succeed or entire transaction reverts.

### Acceptance Criteria
- Buyers fund multi-merchant shopping carts in a single atomic transaction.
- Individual merchant allocations remain isolated for independent fulfillment.

---

## Implement Automatic Escrow Unwind for Stale Orders Lacking Merchant Acceptance

- **Estimate:** 2 days
- **Context:** If a buyer funds an escrow but the merchant fails to acknowledge or accept the order within 72 hours (43,200 ledgers), the contract must permit the buyer to trigger an unconditional unilateral withdrawal.

### Data Types & Schemas
```rust
pub const ORDER_ACCEPTANCE_TIMEOUT_LEDGERS: u32 = 43_200; // ~72 hours

#[contracttype]
pub enum EscrowFulfillmentPhase {
    FundedAwaitingAcceptance,
    AcceptedInPreparation,
    Shipped,
    Delivered,
}
```

### Tasks
- Add `EscrowFulfillmentPhase` tracking to escrow state struct.
- Implement `accept_order(escrow_id)` requiring merchant authorization.
- Implement `claim_unaccepted_refund(escrow_id)` callable by buyer if deadline passed.
- Add unit tests verifying rejection of premature refund and acceptance of valid refund.

### Acceptance Criteria
- Buyers can reclaim funds without dispute arbitration if merchant remains unresponsive.
- Merchant acceptance locks the escrow into active fulfillment phase.

---

## Implement Escrow Yield Accrual Integration with Blend Lending Pool Protocol

- **Estimate:** 3 days
- **Context:** While escrow funds are held in lockup pending delivery (typically 5 to 14 days), integrate with Blend protocol pools to deposit underlying USDC and accrue yield, transferring generated interest to the buyer or platform fee treasury upon settlement.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldBearingEscrowConfig {
    pub lending_pool_contract: Address,
    pub b_token_address: Address,
    pub accrued_yield_stroops: i128,
    pub yield_recipient: Address,
}
```

### Tasks
- Create Blend lending pool client interface in `interfaces/src/blend.rs`.
- Deposit USDC into Blend pool to mint bUSDC receipt tokens upon escrow funding.
- Redeem bUSDC for original principal plus interest on `release_funds()`.
- Disburse principal to merchant and accrued interest to designated yield recipient.

### Acceptance Criteria
- Escrow earns yield during the lockup period without risking principal.
- Blend pool interactions handle exchange rate math safely.

---

## Implement Cryptographic Delivery Confirmation via Off-Chain ECDSA Courier Voucher

- **Estimate:** 3 days
- **Context:** Allow trusted logistics couriers (FedEx, DHL) to sign an off-chain digital delivery receipt containing the order hash. The merchant or courier submits this voucher to the escrow contract to trigger automated funds release.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CourierDeliveryVoucher {
    pub escrow_id: u64,
    pub tracking_number_hash: BytesN<32>,
    pub delivery_timestamp: u64,
    pub courier_public_key: BytesN<32>,
    pub courier_signature: BytesN<64>,
}
```

### Tasks
- Store authorized courier public keys in contract administrative registry.
- Implement `confirm_delivery_via_voucher(voucher)` in `escrow/src/lib.rs`.
- Verify Ed25519 signature of voucher payload against registered courier key.
- Release escrow funds immediately upon valid courier attestation.

### Acceptance Criteria
- Valid courier voucher releases escrow without requiring manual buyer confirmation.
- Unrecognized courier signatures or tampered hashes reject with error.

---

## Implement Referral Commission Attribution via Immutable On-Chain Inviter Signatures

- **Estimate:** 2 days
- **Context:** To incentivize platform growth, allow transactions initiated via referral links to split a configurable portion of platform fees (e.g. 20% of fee) to an on-chain referrer address upon escrow settlement.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferralAttribution {
    pub referrer_address: Address,
    pub commission_basis_points: u32,
    pub referrer_signature: BytesN<64>,
}
```

### Tasks
- Add optional `ReferralAttribution` parameter to `initialize_escrow`.
- Verify referrer is not identical to buyer or merchant (self-referral prevention).
- Calculate referrer commission as fraction of platform fee.
- Disburse commission simultaneously during `release_funds`.

### Acceptance Criteria
- Referrers receive designated commission directly upon escrow completion.
- Self-referral attempts are rejected.

---

## Implement Fine-Grained Merchant Whitelist Filters for Delegated Buyer Sessions

- **Estimate:** 2 days
- **Context:** Buyers delegating spending authority to an AI agent should be able to restrict authorizations to specific verified merchant addresses or merchandise categories, blocking rogue agent purchases.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationMerchantFilter {
    pub allowed_merchants: Vec<Address>,
    pub is_strict_whitelist: bool,
    pub max_per_merchant_stroops: i128,
}
```

### Tasks
- Store merchant whitelist filter in `delegation_registry/src/storage.rs`.
- Enforce that target merchant address in checkout call is present in allowed list.
- Track per-merchant spend limits independently.
- Add unit tests verifying rejection of unauthorized merchant targets.

### Acceptance Criteria
- Agent purchases targeting non-whitelisted merchants fail with `UnauthorizedMerchant`.
- Whitelisted merchants process within assigned spending limits.

---

## Implement Sliding-Window Spending Rate Limiter for Autonomous Agent Delegations

- **Estimate:** 3 days
- **Context:** To mitigate risk if an agent key is compromised, enforce a rolling time-window spend velocity limit (e.g. max 100 USDC per 24-hour window / 14,400 ledgers) rather than only a static all-time cap.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlidingWindowRateLimit {
    pub window_ledgers: u32,
    pub max_window_spend_stroops: i128,
    pub current_window_spend: i128,
    pub window_start_ledger: u32,
}
```

### Tasks
- Implement sliding-window spend accumulator in `delegation_registry`.
- Reset window spend if `current_ledger >= window_start_ledger + window_ledgers`.
- Check that `current_window_spend + proposed_amount <= max_window_spend`.
- Add test cases testing rapid successive spends crossing ledger window boundaries.

### Acceptance Criteria
- Transactions exceeding rolling window limits revert cleanly.
- Window resets spend allowance automatically when time interval elapses.

---

## Implement Emergency Delegation Circuit Breaker Callable by Delegator Master Key

- **Estimate:** 2 days
- **Context:** Provide an instantaneous, high-priority emergency kill-switch function that immediately revokes all active delegated agent sessions and freezes pending escrows under that buyer's account.

### Data Types & Schemas
```rust
#[contracttype]
pub enum AccountCircuitBreakerStatus {
    Normal,
    Frozen,
}
```

### Tasks
- Implement `trigger_emergency_killswitch(env, delegator)` in `delegation_registry`.
- Invalidate master delegation epoch, causing all existing session keys to fail validation.
- Emit critical alert event `DelegationCircuitBreakerTriggered`.
- Implement `reactivate_account(delegator)` requiring multi-factor / master key confirmation.

### Acceptance Criteria
- Calling kill-switch instantly invalidates all active agent session keys.
- No agent transactions can execute while account remains in frozen state.

---

## Implement Multi-Asset Delegated Allowance Tracking within Single Delegation Epoch

- **Estimate:** 3 days
- **Context:** Buyers often hold multiple token assets (USDC, EURC, XLM). Upgrade the delegation registry to track separate spending limits and balances per token asset contract within a single unified delegation grant.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetAllowance {
    pub token_contract: Address,
    pub spending_limit_stroops: i128,
    pub spent_stroops: i128,
}

#[contracttype]
pub struct MultiAssetDelegation {
    pub agent: Address,
    pub allowances: Vec<AssetAllowance>,
    pub expiry_ledger: u32,
}
```

### Tasks
- Update delegation storage to support vector of `AssetAllowance` records.
- Verify requested payment asset matches authorized token in allowances list.
- Deduct spent balance from specific token allowance record.
- Add tests verifying spending in one asset does not impact limit of another.

### Acceptance Criteria
- Agents can spend multiple approved token assets within configured limits.
- Attempting to spend unapproved asset tokens returns `AssetNotAuthorized`.

---

## Implement Epoch-Based Delegation Quota Auto-Renewal with Hard Cumulative Cap

- **Estimate:** 2 days
- **Context:** Support subscription and recurring purchasing agents by allowing weekly or monthly spending quotas to automatically replenish at epoch boundaries, bounded by a cumulative lifetime ceiling.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecurringQuotaConfig {
    pub epoch_duration_ledgers: u32,
    pub per_epoch_quota_stroops: i128,
    pub lifetime_hard_cap_stroops: i128,
    pub cumulative_lifetime_spent: i128,
    pub current_epoch_index: u32,
}
```

### Tasks
- Calculate epoch index based on `env.ledger().sequence() / epoch_duration_ledgers`.
- Replenish available quota if epoch index has advanced.
- Enforce `cumulative_lifetime_spent + amount <= lifetime_hard_cap`.
- Write tests simulating epoch transitions across multiple weeks.

### Acceptance Criteria
- Allowance resets cleanly when new epoch commences.
- Lifetime hard cap cannot be exceeded under any circumstances.

---

## Implement Nonce-Invalidating Delegation Key Revocation with Timestamped Events

- **Estimate:** 2 days
- **Context:** When a buyer revokes an individual session key, the contract must record an incremented revocation nonce to ensure that any offline-signed transactions prepared before revocation cannot be replayed.

### Data Types & Schemas
```rust
#[contracttype]
pub struct RevocationRecord {
    pub session_key: Address,
    pub revoked_at_ledger: u32,
    pub min_valid_nonce: u64,
}
```

### Tasks
- Implement `revoke_session_key(session_key, new_min_nonce)` in `delegation_registry`.
- Reject signatures bearing nonces lower than `min_valid_nonce`.
- Clean up expired revocation records after retention period.
- Emit `SessionKeyRevoked` event with revocation ledger.

### Acceptance Criteria
- Pre-signed transactions signed prior to revocation fail verification.
- Revocation is immediate and permanent for that specific key.

---

## Implement Delegation Sub-Account Creation for Dedicated Micro-Payment Agents

- **Estimate:** 3 days
- **Context:** To completely isolate agent risk from the user's primary wallet, allow the delegation contract to deploy a sandboxed sub-account initialized with an allocated balance, limiting agent liability strictly to sub-account funds.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentSubAccount {
    pub sub_account_address: Address,
    pub master_owner: Address,
    pub agent_operator: Address,
    pub is_active: bool,
}
```

### Tasks
- Implement sub-account factory using Soroban `env.deployer()`.
- Initialize sub-account with restricted authorization rules.
- Provide `drain_sub_account()` function returning all funds to master owner.
- Add integration tests verifying sub-account deployment and fund reclamation.

### Acceptance Criteria
- Sub-account operates autonomously within allocated balance.
- Master owner retains ultimate control to drain or deactivate sub-account.

---

## Implement Cross-Contract Delegation Permission Query Interface with Gas Metering

- **Estimate:** 2 days
- **Context:** External dApps and smart contracts need a lightweight query function `check_delegation_allowance(delegator, agent, token, amount)` to verify permissions before accepting agent transactions.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationQueryResponse {
    pub is_authorized: bool,
    pub remaining_allowance: i128,
    pub ledgers_until_expiry: u32,
}
```

### Tasks
- Implement read-only query function adhering to standardized interface in `interfaces` crate.
- Optimize storage lookups to consume < 10,000 CPU instructions.
- Return remaining spend balance and remaining validity ledgers.
- Add benchmark tests validating minimal gas footprint.

### Acceptance Criteria
- External contracts can query delegation status with minimal gas cost.
- Returns accurate authorization status without mutating storage.

---

## Implement Threshold Signature Scheme Co-Signer Registry for Corporate Buyer Treasuries

- **Estimate:** 3 days
- **Context:** Enterprise corporate accounts require M-of-N threshold signatures (e.g. 3 of 5 authorized executives) before high-value escrow disbursements can be authorized. Build a co-signer registry managing quorum grants.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorporateQuorumConfig {
    pub signers: Vec<Address>,
    pub threshold: u32,
    pub high_value_cutoff_stroops: i128,
}
```

### Tasks
- Implement quorum management methods `set_quorum(signers, threshold, cutoff)` in `permissions`.
- Verify signatures of M unique registered signers on high-value orders.
- Prevent duplicate signer votes on the same proposal.
- Write tests verifying quorum enforcement on transactions above cutoff.

### Acceptance Criteria
- Disbursements above cutoff strictly require M-of-N co-signatures.
- Transactions below cutoff execute via standard single-signer flow.

---

## Implement Merchant Escrow Security Deposit Lock for Storefront Catalog Publishing

- **Estimate:** 3 days
- **Context:** To disincentivize fraud, merchants must deposit a minimum slashable staking bond (e.g. 500 USDC) into the marketplace contract. In the event of confirmed fraud or unfulfilled orders, the bond is partially slashed.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantBondRecord {
    pub merchant: Address,
    pub staked_token: Address,
    pub staked_amount_stroops: i128,
    pub min_required_bond: i128,
    pub slash_count: u32,
    pub is_slashed: bool,
}
```

### Tasks
- Implement `stake_merchant_bond(token, amount)` transferring tokens to marketplace contract.
- Enforce that merchant catalog listings require `staked_amount >= min_required_bond`.
- Implement `slash_bond(merchant, amount, recipient)` callable only by verified dispute resolution authority.
- Allow merchant to withdraw bond after 30-day unbonding cooldown upon store closure.

### Acceptance Criteria
- Unbonded merchants cannot list products in the marketplace.
- Slashed funds transfer to injured buyer or platform insurance pool.

---

## Implement Decentralized SKU Inventory Stock Counter with Atomic Decrement

- **Estimate:** 2 days
- **Context:** Manage product inventory directly on-chain within the marketplace contract. Implement an atomic decrement function called by the escrow contract upon order creation, preventing overselling.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkuInventoryItem {
    pub sku_hash: BytesN<32>,
    pub total_stock: u32,
    pub reserved_stock: u32,
    pub available_stock: u32,
}
```

### Tasks
- Implement `reserve_inventory(sku_hash, quantity)` returning boolean success.
- Implement `commit_inventory(sku_hash, quantity)` upon order settlement.
- Implement `release_inventory(sku_hash, quantity)` upon cancellation or refund.
- Write unit tests verifying atomic decrements and out-of-stock rejections.

### Acceptance Criteria
- Inventory decrements atomically during purchase.
- Concurrent reservations beyond available stock revert immediately.

---

## Implement Storefront Temporary Maintenance Mode with Auto-Release Pause

- **Estimate:** 2 days
- **Context:** When a merchant goes on holiday or pauses operations, allow them to toggle maintenance mode on-chain. This pauses new order creation and extends auto-release deadlines on existing orders by the duration of the maintenance.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaintenanceStatus {
    pub is_active: bool,
    pub activated_at_ledger: u32,
    pub max_duration_ledgers: u32,
}
```

### Tasks
- Implement `toggle_maintenance_mode(is_active)` requiring merchant signature.
- Reject `create_order` calls when target merchant is in maintenance.
- Cap maximum maintenance duration at 14 days (201,600 ledgers).
- Emit `MerchantMaintenanceToggled` event.

### Acceptance Criteria
- New purchases are blocked while maintenance is active.
- Maintenance automatically lapses after max duration to protect buyers.

---

## Implement Merchant Payout Address Migration with 72-Hour Timelock Safety Guard

- **Estimate:** 2 days
- **Context:** If a merchant's payout address is compromised, an attacker could change the payout destination. Implement a 72-hour timelock on payout address changes, emitting emergency notifications and allowing cancellation by emergency key.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayoutMigrationRequest {
    pub merchant: Address,
    pub current_payout: Address,
    pub proposed_payout: Address,
    pub unlock_ledger: u32,
    pub status: Symbol,
}
```

### Tasks
- Implement `request_payout_migration(proposed_address)` setting `unlock_ledger = current + 43,200`.
- Implement `execute_payout_migration()` callable once timelock matures.
- Implement `cancel_payout_migration()` callable immediately by current key.
- Add tests verifying rejection before timelock maturity and success after.

### Acceptance Criteria
- Payout address changes cannot take effect before 72-hour timelock expires.
- Current owner can cancel unauthorized migration requests instantly.

---

## Implement Order Cancellation Penalty Deductions from Merchant Staked Bond

- **Estimate:** 2 days
- **Context:** If a merchant repeatedly accepts orders and subsequently cancels them due to stock negligence, deduct a small penalty (e.g. 2 USDC) from their staked bond and credit it to the inconvenienced buyer.

### Data Types & Schemas
```rust
#[contracttype]
pub struct CancellationPenaltyConfig {
    pub penalty_amount_stroops: i128,
    pub max_cancellations_before_suspension: u32,
}
```

### Tasks
- Check merchant-initiated cancellation frequency in marketplace storage.
- Deduct penalty amount from merchant bond and disburse to buyer address.
- Increment merchant strike counter and suspend store if threshold is breached.
- Add tests verifying bond deduction and buyer compensation.

### Acceptance Criteria
- Merchant-initiated cancellations transfer penalty compensation to buyer.
- Excessive cancellations trigger automated merchant suspension.

---

## Implement Verifiable Product Hash Attestation Against Merchant Catalog Tampering

- **Estimate:** 2 days
- **Context:** To prevent 'bait-and-switch' attacks where a merchant alters product descriptions after purchase, the marketplace contract must store a cryptographic digest `SHA-256(title + spec + price)` committed at checkout.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductAttestation {
    pub product_id: u64,
    pub content_hash: BytesN<32>,
    pub committed_ledger: u32,
}
```

### Tasks
- Implement `commit_product_attestation(product_id, content_hash)`.
- Cross-reference attestation hash during dispute arbitration.
- Allow buyers to verify order hash matches on-chain commitment.
- Write tests verifying hash verification and detection of mismatched digests.

### Acceptance Criteria
- Product specifications anchor immutably on-chain at purchase time.
- Dispute arbitrators can cryptographically verify original product terms.

---

## Implement Multi-Currency Settlement Support with Enforced Soroban SAC Token Contracts

- **Estimate:** 3 days
- **Context:** Extend the marketplace to support payments in multiple verified Stellar Asset Contracts (USDC, EURC, XLM), enforcing an on-chain token whitelist to block unvetted counterfeit tokens.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllowedSettlementToken {
    pub token_address: Address,
    pub symbol: Symbol,
    pub decimals: u32,
    pub is_enabled: bool,
}
```

### Tasks
- Maintain list of authorized settlement tokens in contract admin storage.
- Validate that payment token matches an active whitelisted address.
- Provide admin functions `add_token()` and `remove_token()` with governance checks.
- Add test suite verifying purchases with USDC, EURC, and native XLM.

### Acceptance Criteria
- Merchants can accept diverse whitelisted stablecoins.
- Unapproved or malicious token contracts are strictly rejected.

---

## Implement Merchant Dispute Reserve Fund Pool with Dynamic Insolvency Checks

- **Estimate:** 3 days
- **Context:** Merchants must maintain a dynamic reserve pool proportional to their active open escrow volume (e.g. 10% reserve). If open escrow commitments exceed safe reserve ratios, pause new order intake.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantReservePool {
    pub merchant: Address,
    pub reserve_balance_stroops: i128,
    pub active_escrow_liability_stroops: i128,
    pub reserve_ratio_basis_points: u32, // e.g. 1000 for 10%
}
```

### Tasks
- Track aggregate open escrow liability per merchant in marketplace contract.
- Verify `reserve_balance >= (active_liability * reserve_ratio) / 10000` before accepting new orders.
- Allow merchants to top-up reserve balance at any time.
- Add unit tests simulating order volume surges and reserve ratio gating.

### Acceptance Criteria
- Blocks new orders if merchant open liability exceeds reserve coverage.
- Completing or settling orders reduces active liability and unblocks sales.

---

## Implement Verified Buyer Escrow-Bound Review Hash Commitment in Reputation Contract

- **Estimate:** 2 days
- **Context:** Store cryptographic commitments of verified buyer reviews directly in the reputation contract, ensuring that on-chain reputation scores correspond to verifiable off-chain review content.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewCommitment {
    pub escrow_id: u64,
    pub reviewer: Address,
    pub merchant: Address,
    pub rating: u32, // 1 to 5
    pub content_hash: BytesN<32>,
    pub submitted_ledger: u32,
}
```

### Tasks
- Implement `submit_review_commitment(escrow_id, rating, content_hash)` in `reputation`.
- Verify with escrow contract that caller was buyer and escrow status is settled.
- Enforce that each `escrow_id` can only submit a review commitment once.
- Update cumulative merchant rating scores atomically.

### Acceptance Criteria
- Only authenticated buyers of completed escrows can commit review scores.
- Review commitments anchor immutable SHA-256 hashes of off-chain text.

---

## Implement Time-Weighted Reputation Score Normalization with Sigmoid Bounds

- **Estimate:** 3 days
- **Context:** Raw review averages can be easily skewed. Implement a normalized reputation scoring algorithm that applies a sigmoid scaling curve and weights recent orders more heavily than ancient transactions.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedReputationScore {
    pub merchant: Address,
    pub normalized_score: u32, // 0 to 10,000 (basis points)
    pub confidence_score: u32, // Based on total review sample size
    pub last_updated_ledger: u32,
}
```

### Tasks
- Implement fixed-point sigmoid score calculation in `reputation/src/algorithm.rs`.
- Weight reviews decaying exponentially by ledger age.
- Compute confidence score reflecting statistical significance (Bayesian average).
- Benchmark computation gas ensuring query stays under 50,000 CPU instructions.

### Acceptance Criteria
- Normalized score bounds smoothly between 0 and 10,000.
- Merchants with few reviews trend toward neutral median rather than extremes.

---

## Implement Sybil-Resistant Dispute Penalty Multiplier for Repeat Offenders

- **Estimate:** 2 days
- **Context:** Merchants who lose repeated disputes within short timeframes represent systemic risk. Implement an exponential penalty multiplier that doubles score deductions for each consecutive lost dispute.

### Data Types & Schemas
```rust
#[contracttype]
pub struct DisputePenaltyRecord {
    pub merchant: Address,
    pub consecutive_lost_disputes: u32,
    pub last_dispute_ledger: u32,
    pub active_penalty_multiplier: u32,
}
```

### Tasks
- Track consecutive lost disputes in reputation contract storage.
- Calculate penalty as `base_penalty * (2 ^ min(consecutive_losses, 4))`.
- Reset consecutive loss counter if merchant completes 20 dispute-free orders.
- Write tests verifying multiplier escalation and recovery resets.

### Acceptance Criteria
- Repeat dispute losses incur compounding penalties.
- Sustained good behavior allows gradual penalty multiplier recovery.

---

## Implement Peer Validator Quorum Attestation for Disputed Evidence Evaluation

- **Estimate:** 3 days
- **Context:** For disputed transactions exceeding standard limits, decentralized peer validators review evidence and vote on resolution outcomes. Implement a quorum voting contract tallying validator votes.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeQuorumVote {
    pub dispute_id: u64,
    pub validator: Address,
    pub verdict: Symbol, // 'buyer', 'merchant', 'split'
    pub staked_vote_weight: i128,
}
```

### Tasks
- Implement `cast_dispute_vote(dispute_id, verdict)` in `reputation`.
- Tally staked vote weights until reaching required quorum threshold (e.g. 66%).
- Trigger automated escrow split or refund call based on winning verdict.
- Reward voting validators with a share of arbitration fees.

### Acceptance Criteria
- Dispute resolves automatically once validator vote crosses 66% weight threshold.
- Validators cannot vote multiple times on the same dispute.

---

## Implement Merchant Reputation Rehabilitation Staking Mechanism After Dispute Resolution

- **Estimate:** 2 days
- **Context:** Provide a structured rehabilitation process where penalised merchants can post supplementary stake and complete supervised micro-orders to gradually restore their public trust score.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RehabilitationProgram {
    pub merchant: Address,
    pub collateral_staked_stroops: i128,
    pub probation_orders_required: u32,
    pub probation_orders_completed: u32,
    pub status: Symbol,
}
```

### Tasks
- Implement `enter_rehabilitation(collateral_amount)` in `reputation`.
- Track clean order completions during probation phase.
- Incrementally restore baseline reputation score as probation milestones complete.
- Return staked collateral upon full rehabilitation graduation.

### Acceptance Criteria
- Penalized merchants have a transparent, verifiable path to redeem score.
- Any dispute violation during probation forfeits rehabilitation collateral.

---

## Implement Community-Driven Fraud Flagging with Slashable Reporter Staking Bond

- **Estimate:** 3 days
- **Context:** Allow community members to flag fraudulent listings by staking a 50 USDC bond. If the flag is confirmed by moderators/validators, the reporter earns a bounty; if the report is malicious, their bond is slashed.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FraudReport {
    pub report_id: u64,
    pub reporter: Address,
    pub target_entity: Address,
    pub evidence_hash: BytesN<32>,
    pub staked_bond_stroops: i128,
    pub status: Symbol,
}
```

### Tasks
- Implement `file_fraud_report(target, evidence_hash, bond_amount)` in `reputation`.
- Lock reporter bond in contract storage.
- Implement `resolve_fraud_report(report_id, is_valid)` callable by governance.
- Disburse bounty upon valid report or slash bond upon frivolous report.

### Acceptance Criteria
- Staking requirement completely prevents spam flagging attacks.
- Valid fraud reports earn bounty from merchant slashable bond.

---

## Implement Blinded Review Signature Verification Using Ed25519 Ephemeral Keypairs

- **Estimate:** 3 days
- **Context:** To preserve buyer privacy while verifying authenticity, allow buyers to submit reviews signed with an ephemeral blinded key generated during checkout, decoupling the buyer's public identity from their review.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlindedReviewReceipt {
    pub escrow_id_commitment: BytesN<32>,
    pub blinded_public_key: BytesN<32>,
    pub blind_signature: BytesN<64>,
}
```

### Tasks
- Issue blinded review authorization token during escrow funding.
- Verify blind Ed25519 signature on review submission.
- Store verified review score without recording buyer's primary wallet address.
- Write cryptographic test cases validating blinded signature verification.

### Acceptance Criteria
- Reviews verify as authentic settled orders without revealing buyer wallet address.
- Double-submission of blinded receipts is strictly prevented via nullifier hashes.

---

## Implement Standalone Reputation Query Trait Export for External Underwriting Contracts

- **Estimate:** 2 days
- **Context:** External DeFi underwriting, lending, and insurance contracts need to evaluate merchant creditworthiness. Export a standardized Soroban trait interface allowing third-party contracts to query merchant reputation.

### Data Types & Schemas
```rust
pub trait ReputationReaderTrait {
    fn get_merchant_rating(env: Env, merchant: Address) -> Result<u32, ReputationError>;
    fn get_dispute_ratio(env: Env, merchant: Address) -> Result<u32, ReputationError>;
    fn is_merchant_in_good_standing(env: Env, merchant: Address) -> Result<bool, ReputationError>;
}
```

### Tasks
- Define `ReputationReaderTrait` in `interfaces/src/reputation.rs`.
- Implement the trait on `ReputationContract` in `reputation/src/lib.rs`.
- Optimize read functions for low-gas cross-contract invocation.
- Add cross-contract mock integration test in `tests/cross_contract.rs`.

### Acceptance Criteria
- External contracts can call reputation methods using typed Soroban client.
- Trait compiles cleanly in `no_std` mode without allocator requirements.

---

## Implement Quorum Participant Key Rotation Protocol with Preserved Signature Epochs

- **Estimate:** 3 days
- **Context:** When a corporate multi-sig participant leaves an organization, the quorum must rotate their signing key to a successor without invalidating historical transactions or causing pending proposals to fail.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignerRotationProposal {
    pub outgoing_signer: Address,
    pub incoming_signer: Address,
    pub effective_epoch: u32,
    pub approvals: Vec<Address>,
}
```

### Tasks
- Implement `propose_signer_rotation(outgoing, incoming)` in `permissions`.
- Require existing quorum threshold approvals to execute rotation.
- Advance signature epoch and update signer set atomically.
- Preserve historical execution log under previous epoch identifiers.

### Acceptance Criteria
- Signer rotation executes smoothly with quorum consent.
- Rotated out signers lose all signing privileges for new epochs immediately.

---

## Implement Timelocked Contract Upgrade Dispatcher with Emergency Freeze Capability

- **Estimate:** 3 days
- **Context:** To ensure decentralization, contract bytecode upgrades must be subject to an on-chain 7-day timelock (100,800 ledgers), allowing users to audit the proposed WASM hash and withdraw funds if they disagree.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradeProposal {
    pub new_wasm_hash: BytesN<32>,
    pub proposed_ledger: u32,
    pub executable_after_ledger: u32,
    pub is_executed: bool,
    pub is_cancelled: bool,
}
```

### Tasks
- Implement `propose_upgrade(new_wasm_hash)` in governance module.
- Enforce that `executable_after_ledger >= current_ledger + 100_800`.
- Implement `execute_upgrade()` invoking `env.deployer().update_current_contract_wasm()`.
- Provide emergency cancellation function callable by security council.

### Acceptance Criteria
- Bytecode upgrades cannot execute before 7-day timelock maturation.
- Proposed WASM hash emits publicly via Soroban event upon proposal.

---

## Implement Role-Based Access Control (RBAC) Matrix for Administrative Operations

- **Estimate:** 2 days
- **Context:** Replace monolithic admin keys with granular roles (`Admin`, `Arbitrator`, `FeeManager`, `Pauser`) in contract storage, following principle of least privilege.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ContractRole {
    Admin = 1,
    Arbitrator = 2,
    FeeManager = 3,
    Pauser = 4,
}
```

### Tasks
- Implement role storage mapping `(Address, ContractRole) -> bool`.
- Create authorization macro or helper function `require_role(role)`.
- Grant and revoke roles with events recording actor and timestamp.
- Add tests verifying role segregation across administrative functions.

### Acceptance Criteria
- Non-admin accounts cannot access privileged administrative functions.
- Roles can be granted and revoked independently without superuser transfers.

---

## Implement Contract Pause / Unpause Circuit Breaker for Catastrophic Vulnerability Mitigation

- **Estimate:** 2 days
- **Context:** In the event of an active exploit or critical dependency bug, the designated `Pauser` role must be able to immediately freeze deposit and release methods across the contract suite.

### Data Types & Schemas
```rust
#[contracttype]
pub enum ContractLifecycleState {
    Active,
    Paused { paused_at_ledger: u32, reason: Symbol },
}
```

### Tasks
- Add `ContractLifecycleState` check to entry points of escrow and marketplace.
- Implement `pause(reason)` callable only by authorized `Pauser`.
- Implement `unpause()` requiring multisig admin approval.
- Allow users to inspect contract paused status via public query.

### Acceptance Criteria
- Paused contract rejects financial state mutations with `ContractIsPaused`.
- Read-only queries and user inspection remain functional during pause.

---

## Implement Structured Soroban Event Telemetry for Sub-Ledger Indexer Synchronization

- **Estimate:** 2 days
- **Context:** Standardize event topics and payload schemas across all contracts (`escrow`, `marketplace`, `reputation`, `permissions`), ensuring high-throughput indexers can deserialize events without heuristic topic guessing.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StandardEscrowEventPayload {
    pub escrow_id: u64,
    pub buyer: Address,
    pub merchant: Address,
    pub amount_stroops: i128,
    pub token: Address,
    pub event_timestamp: u64,
}
```

### Tasks
- Audit all `env.events().publish()` invocations across the workspace.
- Standardize first topic as contract namespace (e.g. `Symbol::new(&env, 'escrow')`).
- Standardize second topic as event action (e.g. `Symbol::new(&env, 'funded')`).
- Write comprehensive snapshot tests for emitted event payloads.

### Acceptance Criteria
- All events follow uniform `(namespace, action, entity_id)` topic structure.
- Event serialization passes test suite without schema regressions.

---

## Implement Granular Gas Benchmark Test Harness for Core Escrow Operations

- **Estimate:** 2 days
- **Context:** To prevent sudden gas limit exhaustion on Soroban Mainnet, implement an automated benchmarking harness measuring CPU instructions and memory byte consumption for escrow creation, deposit, and release.

### Data Types & Schemas
```rust
pub struct GasBenchmarkResult {
    pub operation: &'static str,
    pub cpu_instructions: u64,
    pub memory_bytes: u64,
    pub ledger_entry_reads: u32,
    pub ledger_entry_writes: u32,
}
```

### Tasks
- Build benchmark test runner using `env.budget().print()` in `tests/gas_benchmark.rs`.
- Measure baseline costs across standard order workflows.
- Set hard gas ceiling assertions to fail test if budget increases > 10%.
- Document baseline gas consumption figures in `docs/architecture/gas_benchmarks.md`.

### Acceptance Criteria
- Benchmark test runs deterministically under `cargo test`.
- Regressions exceeding gas thresholds fail automated checks.

---

## Implement Property-Based Chaos Testing Suite for Cross-Contract Escrow Transitions

- **Estimate:** 3 days
- **Context:** Use property-based fuzz testing (`proptest`) to execute randomized sequences of deposits, refunds, disputes, and cancellations, verifying that total tokens locked in the escrow contract always strictly equal the sum of unfulfilled commitments.

### Data Types & Schemas
```rust
pub struct EscrowSystemInvariants {
    pub total_contract_token_balance: i128,
    pub sum_of_active_escrow_balances: i128,
    pub accrued_uncollected_fees: i128,
}
```

### Tasks
- Configure `proptest` harness in `tests/cross_contract.rs`.
- Generate randomized action sequences (create, fund, dispute, release, refund).
- Assert invariant: `token_balance == sum(active_escrows) + fees` after every action.
- Execute 10,000 randomized state transitions without invariant violation.

### Acceptance Criteria
- Property-based test runs 10,000 randomized state transitions successfully.
- Zero token leakage or insolvency conditions detected under adversarial sequences.

---

