# Smart Contracts 50 Additional Issues Backlog (50 Issues)

Developer-ready issues covering advanced contract security, multi-oracle consensus, gas optimizations, and architectural enhancements.
**Total Issues:** 50
**Target Sizing:** 1–2 developer-days per issue.

---

## Implement Multi-Oracle Consensus Aggregation for Complex Escrow Conditions

- **Estimate:** 2 days
- **Context:** Currently, `evaluate_and_release` in `escrow/src/lib.rs` queries a single oracle contract address. If that oracle goes offline, behaves maliciously, or gets front-run, the escrow can be stalled or incorrectly released. Implementing k-of-n oracle consensus aggregation ensures high availability and cryptographic threshold security for high-value purchases.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiOracleConfig {
    pub required_oracle_count: u32,
    pub oracle_addresses: Vec<Address>,
    pub condition_symbol: Symbol,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleVoteRecord {
    pub oracle: Address,
    pub condition_met: bool,
    pub voted_at_ledger: u32,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MultiOracleError {
    ThresholdNotMet = 1051,
    DuplicateOracleVote = 1052,
    UnauthorizedOracle = 1053,
    ConsensusWindowExpired = 1054,
}
```

### Tasks
- Define `MultiOracleConfig` and persistent storage key `DataKey::EscrowOracleVotes(u64)` in `escrow/src/lib.rs`.
- Implement `submit_oracle_attestation(env: Env, escrow_id: u64, condition_met: bool)` requiring caller auth.
- Count distinct affirmative oracle responses within the consensus window and auto-release when threshold is reached.
- Add integration test verifying 2-of-3 oracle consensus release and rejection of duplicate or unauthorized votes.

### Acceptance Criteria
- Escrow only releases once `required_oracle_count` authorized oracles submit affirmative attestations.
- Unauthorized or duplicate oracle submissions fail with typed `MultiOracleError`.

---

## Prevent Fee-on-Transfer Token Desynchronization in Escrow Accounting

- **Estimate:** 1 day
- **Context:** Certain custom Soroban tokens or bridged assets deduct a transfer fee or burn percentage on each `transfer()` invocation. In `escrow/src/lib.rs`, the contract records `escrow.amount` based on the requested deposit amount rather than verifying the actual net delta received in the contract's token balance, causing insolvent payouts during release.

### Data Types & Schemas
```rust
pub fn verify_received_deposit_delta(
    env: &Env,
    token_client: &token::Client,
    contract_address: &Address,
    balance_before: i128,
    expected_amount: i128,
) -> Result<i128, EscrowError> {
    let balance_after = token_client.balance(contract_address);
    let net_received = balance_after.checked_sub(balance_before)
        .ok_or(EscrowError::MathOverflow)?;
    if net_received < expected_amount {
        return Err(EscrowError::DepositUnderfunded);
    }
    Ok(net_received)
}
```

### Tasks
- Query contract token balance immediately prior to invoking `transfer_from()` in `fund()` / `deposit()`.
- Compute actual delta: `net_received = balance_after - balance_before`.
- Record `net_received` as `escrow.amount` and reject transactions where net received is less than agreed threshold.
- Add unit tests simulating a fee-on-transfer token.

### Acceptance Criteria
- Escrow internal ledger records exact tokens held, preventing balance deficits upon release.
- Underfunded deposits abort cleanly.

---

## Implement Two-Tiered Dispute Resolution with 48-Hour Appeal Bond

- **Estimate:** 2 days
- **Context:** Currently, dispute decisions by an arbiter or admin are immediate and final. To protect against rogue individual arbiters or mistaken rulings, implement a two-stage arbitration process where the losing party can post an appeal bond within 48 hours to escalate the dispute to an appeals council.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeAppealRecord {
    pub initial_winner: Address,
    pub appealed_by: Address,
    pub appeal_bond_amount: i128,
    pub appeal_deadline_ledger: u32,
    pub status: AppealStatus,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppealStatus {
    PendingAppealWindow,
    Appealed,
    Finalized,
}
```

### Tasks
- Modify `resolve_dispute` to transition escrow into `DisputeStatus::InitialRuling` with 48-hour appeal window.
- Implement `file_dispute_appeal(env: Env, escrow_id: u64)` requiring deposit of `appeal_bond_amount`.
- Implement `finalize_dispute_appeal(env: Env, escrow_id: u64, final_winner: Address)` by appeals council.
- Add bond slashing logic: if appeal fails, appeal bond is awarded to opposing party.

### Acceptance Criteria
- Disputes enter an appealable grace period before funds can be withdrawn.
- Uncontested rulings finalize automatically after deadline.
- Appeals bond prevents spam escalation.

---

## Prevent Seller Front-Running Cancellation on Stalled Order Acceptances

- **Estimate:** 1 day
- **Context:** Currently, `cancel` can be called by a seller to cancel an escrow in `Created` state. If a buyer deposits funds in the same ledger sequence, a malicious seller watching the mempool could submit a cancel call ahead of deposit to manipulate buyer state or lock inventory maliciously.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderAcceptanceState {
    pub seller_accepted: bool,
    pub accepted_at_ledger: u32,
    pub cancel_lockout_ledgers: u32,
}
```

### Tasks
- Enforce that `cancel` checks whether buyer deposit is currently in flight or within minimum creation window.
- Require explicit mutual agreement or timeout before unilateral cancellation is permitted.
- Add unit tests simulating race condition between buyer deposit and seller cancellation.

### Acceptance Criteria
- Sellers cannot cancel escrows once a buyer deposit has been submitted.
- Deterministic status checks prevent front-running state corruption.

---

## Implement Buyer Inspection Period with Timelocked Auto-Release

- **Estimate:** 2 days
- **Context:** For physical goods and digital assets, delivery confirmation by a carrier or oracle only indicates dispatch arrival. The buyer requires a configurable inspection window (e.g. 72 hours) to inspect items for defects before funds are finalized to seller.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectionPeriodConfig {
    pub inspection_duration_ledgers: u32,
    pub delivery_confirmed_ledger: u32,
    pub auto_release_ledger: u32,
}
```

### Tasks
- Add `Inspection` status to `EscrowStatus` enum between delivery confirmation and release.
- Set `auto_release_ledger = current_ledger + inspection_duration_ledgers` upon delivery attestation.
- Allow buyer to trigger immediate release if satisfied, or file dispute during inspection window.
- Allow seller or keeper to trigger `claim_inspection_auto_release` after window elapses without dispute.

### Acceptance Criteria
- Funds cannot be claimed by seller during active inspection period.
- Inspection window expires into automatic release if no dispute is filed.

---

## Implement Mutual Cryptographic Voucher for Dynamic Escrow Timeout Extension

- **Estimate:** 2 days
- **Context:** When international shipping delays occur, buyer and seller often agree off-chain to extend the delivery timeout. Currently, extending timeout requires on-chain admin intervention. Supporting mutual cryptographic vouchers allows parties to extend deadlines gas-efficiently.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeoutExtensionVoucher {
    pub escrow_id: u64,
    pub new_timeout_ledger: u32,
    pub nonce: u64,
    pub buyer_signature: BytesN<64>,
    pub seller_signature: BytesN<64>,
}
```

### Tasks
- Implement `extend_timeout_with_mutual_signatures(env: Env, voucher: TimeoutExtensionVoucher)`.
- Verify Ed25519 signatures of both buyer and seller over canonical extension payload.
- Assert `new_timeout_ledger > current_timeout_ledger`.
- Update escrow record and emit `EscrowTimeoutExtendedEvent`.

### Acceptance Criteria
- Escrow timeout extends without admin intervention when both parties provide valid signatures.
- Tampered vouchers or single-sided requests are rejected.

---

## Enforce Conservation of Value Invariant on Multi-Milestone Partial Releases

- **Estimate:** 1 day
- **Context:** In `partial_release` and `partial_refund`, multiple partial disbursements occur over the escrow lifetime. Without strict invariant checking, cumulative rounding or off-by-one Stroop discrepancies could allow `released + refunded > total_amount`.

### Data Types & Schemas
```rust
pub fn assert_value_conservation_invariant(
    escrow: &EscrowRecord,
    additional_release: i128,
    additional_refund: i128
) -> Result<(), EscrowError> {
    let new_released = escrow.released_amount.checked_add(additional_release)
        .ok_or(EscrowError::MathOverflow)?;
    let new_refunded = escrow.refunded_amount.checked_add(additional_refund)
        .ok_or(EscrowError::MathOverflow)?;
    let total_disbursed = new_released.checked_add(new_refunded)
        .ok_or(EscrowError::MathOverflow)?;
    if total_disbursed > escrow.amount {
        return Err(EscrowError::ExceedsTotalEscrowAmount);
    }
    Ok(())
}
```

### Tasks
- Integrate `assert_value_conservation_invariant` before modifying `released_amount` or `refunded_amount`.
- Enforce that `escrow.status = EscrowStatus::Released` if and only if `released + refunded == amount`.
- Add edge case tests with multiple fractional micro-releases.

### Acceptance Criteria
- Cumulative disbursements can never exceed initial escrow principal.
- Terminal state transition triggers exactly when remaining balance reaches zero.

---

## Implement Selective Buyer Withdrawal on Administratively Suspended Merchant Escrows

- **Estimate:** 2 days
- **Context:** If a merchant is banned or suspended in `marketplace` due to confirmed fraudulent activity, buyers with pending active escrows with that merchant need an accelerated withdrawal mechanism without waiting for the full timeout ledger.

### Data Types & Schemas
```rust
pub fn claim_fraud_restitution(
    env: Env,
    escrow_id: u64,
    marketplace_contract: Address
) -> Result<i128, EscrowError> {
    // Check merchant status in marketplace
    // If Suspended/Banned and escrow is Funded, allow buyer refund immediately
}
```

### Tasks
- Query marketplace contract to verify whether `escrow.seller` has status `MerchantStatus::Banned`.
- Allow buyer to bypass remaining timeout ledger and claim instant 100% refund of unreleased escrow funds.
- Emit `EscrowFraudRestitutionRefundedEvent`.
- Add integration test verifying immediate refund for banned merchant orders.

### Acceptance Criteria
- Buyers can reclaim funds immediately when seller is banned by governance.
- Active honest merchants are not impacted.

---

## Implement Split Yield Distribution Between Buyer and Seller on Long Escrows

- **Estimate:** 2 days
- **Context:** When funds are held in yield-bearing escrows (`YieldConfig`), accrued interest is currently allocated 100% to the seller upon release. For long holding periods or disputes, fair escrow standards require sharing accrued yield between buyer and seller based on holding ratios.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldSplitConfig {
    pub buyer_yield_share_bps: u32,  // e.g. 5000 = 50%
    pub seller_yield_share_bps: u32, // e.g. 5000 = 50%
}
```

### Tasks
- Add optional `YieldSplitConfig` to `YieldConfig`.
- Calculate proportional yield payout upon release or refund.
- Disburse respective shares to buyer and seller addresses atomically.
- Add unit tests validating yield split calculations.

### Acceptance Criteria
- Yield is distributed proportionally according to configured basis points.
- Sum of yield shares equals total accrued lending yield.

---

## Implement Atomic Batch Deposit for Multiple Escrow Orders in Single Invocation

- **Estimate:** 2 days
- **Context:** Buyers purchasing from multiple merchants in a shopping basket must currently fund each escrow in separate blockchain transactions. An atomic batch deposit method executes a single token transfer for the aggregate sum and initializes all escrows in one call.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchDepositItem {
    pub escrow_id: u64,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchDepositResult {
    pub funded_count: u32,
    pub total_deposited: i128,
}
```

### Tasks
- Implement `batch_deposit(env: Env, buyer: Address, token: Address, items: Vec<BatchDepositItem>)`.
- Transfer total sum (`sum(items.amount)`) from buyer in one `token_client.transfer_from` call.
- Update each escrow status to `EscrowStatus::Funded` and record deposit timestamps.
- Ensure entire batch reverts if any single escrow ID is invalid or already funded.

### Acceptance Criteria
- Basket checkouts execute in a single atomic transaction.
- Gas fees and transaction overhead are reduced by >60% compared to sequential deposits.

---

## Mitigate Integer Truncation Vulnerability in Basis Points Fee Calculations

- **Estimate:** 1 day
- **Context:** When platform fees or arbitrator fees are calculated on small escrow amounts, integer division `(amount * fee_bps) / 10_000` truncates to zero Stroops for amounts under 10,000 Stroops, allowing attackers to split transactions into micro-amounts to completely evade platform fees.

### Data Types & Schemas
```rust
pub fn calculate_fee_with_minimum(
    amount: i128,
    fee_bps: u32,
    min_fee_stroops: i128
) -> Result<i128, EscrowError> {
    let calculated = (amount.checked_mul(fee_bps as i128)
        .ok_or(EscrowError::MathOverflow)?) / 10_000;
    if amount > 0 && calculated < min_fee_stroops {
        Ok(min_fee_stroops.min(amount))
    } else {
        Ok(calculated)
    }
}
```

### Tasks
- Implement minimum floor fee in `escrow/src/lib.rs` fee calculations.
- Enforce `min_fee_stroops` to prevent fee evasion through micro-transaction splitting.
- Add unit tests verifying fee floor behavior across small amounts.

### Acceptance Criteria
- Micro-transactions pay the minimum floor fee rather than zero.
- Platform fee evasion via transaction fragmentation is prevented.

---

## Implement Sub-Order Item Level Refund Allocation in Disputed Escrows

- **Estimate:** 2 days
- **Context:** In multi-item purchases (e.g., 3 books and 1 laptop), a dispute may only involve one damaged book ($20) while the remaining items ($1,000) are accepted. Supporting line-item refund allocations allows partial dispute resolution without locking up uncontested funds.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubOrderItemResolution {
    pub item_id: Symbol,
    pub refund_amount: i128,
    pub release_amount: i128,
    pub is_resolved: bool,
}
```

### Tasks
- Store sub-order line items in escrow metadata.
- Implement `resolve_sub_item_dispute(env: Env, escrow_id: u64, resolution: SubOrderItemResolution)`.
- Release uncontested item funds to seller immediately while refunding disputed item amount to buyer.
- Add unit tests verifying itemized partial resolutions.

### Acceptance Criteria
- Uncontested line-item amounts are disbursed immediately without waiting for full dispute resolution.
- Total sub-item disbursements match escrow balance.

---

## Implement Gas-Optimized Escrow Status Bitflags to Reduce Storage Footprint

- **Estimate:** 1 day
- **Context:** Storing separate boolean fields for flags (`is_funded`, `is_disputed`, `is_appealed`, `is_inspected`) inside `EscrowRecord` increases serialization overhead in Soroban persistent storage. Consolidating lifecycle flags into a single bitmask reduces storage costs by ~40%.

### Data Types & Schemas
```rust
pub const ESCROW_FLAG_FUNDED: u32     = 1 << 0;
pub const ESCROW_FLAG_DISPUTED: u32   = 1 << 1;
pub const ESCROW_FLAG_APPEALED: u32   = 1 << 2;
pub const ESCROW_FLAG_INSPECTED: u32  = 1 << 3;
pub const ESCROW_FLAG_YIELD_ON: u32   = 1 << 4;

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EscrowFlags(pub u32);
```

### Tasks
- Refactor boolean flags in `EscrowRecord` to use `EscrowFlags` bitmask.
- Implement helper methods `has_flag()`, `set_flag()`, and `clear_flag()`.
- Benchmark persistent storage read and write gas savings.

### Acceptance Criteria
- Storage size per escrow record is reduced.
- All lifecycle transitions operate cleanly with bitmask operations.

---

## Prevent Unchecked Balance Drift When Escrow Asset Contract Upgrades

- **Estimate:** 1 day
- **Context:** If a custom Soroban token contract upgrades its WASM code or pauses operations while escrows hold deposits, subsequent withdrawals could fail with unhandled panics. Adding an emergency asset pause hook protects trapped funds.

### Data Types & Schemas
```rust
pub fn emergency_rescue_stalled_escrow(
    env: Env,
    escrow_id: u64,
    recovery_destination: Address
) -> Result<(), EscrowError> {
    // Admin multi-sig emergency intervention for upgraded/defunct token contracts
}
```

### Tasks
- Implement multi-sig admin rescue function for escrows stranded by broken external token contracts.
- Enforce 14-day timelock before rescue method can be invoked.
- Emit `EscrowEmergencyRescuedEvent`.

### Acceptance Criteria
- Stranded escrows can be recovered via governance timelock if underlying token fails.
- Normal escrows cannot be accessed by rescue function.

---

## Implement Dynamic Arbiter Fee Escrow Staking and Slashing Mechanism

- **Estimate:** 2 days
- **Context:** Arbiters who resolve disputes should stake collateral in the escrow contract. If an arbiter fails to rule within the statutory resolution window (e.g. 7 days), their stake is slashed and distributed to the affected parties as compensation.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArbiterStakingRecord {
    pub arbiter: Address,
    pub staked_amount: i128,
    pub assigned_disputes_count: u32,
    pub is_slashed: bool,
}
```

### Tasks
- Implement `register_arbiter_with_stake(env: Env, stake_amount: i128)`.
- Track dispute assignment deadlines.
- Implement `slash_delinquent_arbiter(env: Env, escrow_id: u64)` paying penalty to buyer and seller.
- Add tests testing stake deduction on expired dispute resolution.

### Acceptance Criteria
- Arbiters must maintain active stake to receive dispute assignments.
- Delinquent arbiters are automatically slashed upon expiration.

---

## Implement Parallel Multi-Channel Nonces for Concurrent Agent Delegated Spends

- **Estimate:** 2 days
- **Context:** Autonomous agents executing parallel sub-tasks currently serialize transactions using a single sequential nonce in `permissions/src/lib.rs`. If one transaction is delayed, all concurrent agent threads are blocked. Multi-channel nonces (e.g. 256 parallel lanes) enable high-throughput concurrent agent transactions.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    ChannelNonce(Address, Address, u32), // (owner, delegate, channel_id) -> nonce
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelSpendSignature {
    pub channel_id: u32,
    pub nonce: u64,
    pub signature: BytesN<64>,
}
```

### Tasks
- Implement channel-based nonce lookup `ChannelNonce(owner, delegate, channel_id)`.
- Allow agents to specify `channel_id` (0..=255) during spend authorization.
- Increment nonce specifically for the designated channel, preserving independent lanes.
- Add integration test verifying 10 concurrent transactions executing across different channels.

### Acceptance Criteria
- Transactions in channel A do not block or interfere with channel B nonces.
- Nonce replay protection holds independently per channel.

---

## Implement Rolling Window Velocity Caps in Addition to Per-Transaction Limits

- **Estimate:** 2 days
- **Context:** Per-transaction limits protect against single large drains, but a compromised agent can execute 100 maximum-sized transactions in rapid succession. A rolling 1-hour velocity cap limits aggregate spending within a sliding window of ledger sequences.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RollingWindowLimit {
    pub window_ledgers: u32,       // e.g. 720 ledgers (~1 hour)
    pub max_spend_in_window: i128,
    pub current_window_spend: i128,
    pub window_start_ledger: u32,
}
```

### Tasks
- Implement sliding window spend tracker in `permissions/src/lib.rs`.
- Reset window spend accumulator when `current_ledger >= window_start_ledger + window_ledgers`.
- Reject spends where `current_window_spend + spend_amount > max_spend_in_window` with `PermissionError::VelocityLimitExceeded`.
- Add unit tests validating window roll-over and rejection of rapid micro-spends.

### Acceptance Criteria
- Cumulative spend within the sliding ledger window cannot exceed `max_spend_in_window`.
- Window resets automatically when the duration elapses.

---

## Implement Function-Restricted Permission Grants (Target Contract & Method Scoping)

- **Estimate:** 2 days
- **Context:** Delegated permissions currently only validate transfer amounts and merchant addresses. Advanced agent workflows require restricting delegate authority to specific contract functions (e.g. only allow invoking `escrow.fund`, but strictly forbid calling arbitrary contract entrypoints).

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopedPermissionConfig {
    pub target_contract: Address,
    pub allowed_function_symbols: Vec<Symbol>,
}
```

### Tasks
- Add `ScopedPermissionConfig` to permission records.
- Verify during spend validation that `invoked_function` matches `allowed_function_symbols`.
- Reject invocation of unauthorized contract methods with `PermissionError::UnauthorizedFunction`.
- Write tests asserting rejection of unapproved method invocations.

### Acceptance Criteria
- Delegates can only execute pre-authorized function signatures.
- Function scoping prevents lateral privilege escalation.

---

## Prevent Allowance Drain via Gasless Relayer Tip Manipulation

- **Estimate:** 1 day
- **Context:** In `execute_spend_via_relayer`, a gasless transaction specifies a relayer reimbursement fee. If the relayer fee is deducted from the delegator's allowance without a maximum cap, a rogue relayer could submit excessive fee requests that consume the user's spending limit.

### Data Types & Schemas
```rust
pub const MAX_RELAYER_FEE_BPS: u32 = 100; // 1% maximum tip
pub const MAX_ABSOLUTE_RELAYER_STROOPS: i128 = 10_000_000; // 1 XLM

pub fn validate_relayer_fee(spend_amount: i128, relayer_fee: i128) -> Result<(), PermissionError> {
    if relayer_fee < 0 || relayer_fee > MAX_ABSOLUTE_RELAYER_STROOPS {
        return Err(PermissionError::InvalidParam);
    }
    let max_proportional = (spend_amount * MAX_RELAYER_FEE_BPS as i128) / 10_000;
    if relayer_fee > max_proportional {
        return Err(PermissionError::InvalidParam);
    }
    Ok(())
}
```

### Tasks
- Enforce both absolute and proportional caps on relayer fee parameters.
- Reject relayer transactions where the fee exceeds defined safety boundaries.
- Add unit tests validating fee ceiling enforcement.

### Acceptance Criteria
- Relayer fees cannot exceed 1% or 1 XLM cap.
- Protects delegator allowance from fee inflation.

---

## Implement Quorum Threshold Migration Without Wiping Existing Spend States

- **Estimate:** 2 days
- **Context:** For enterprise multi-owner grants (`grant_multi_owner`), changing the required signature threshold (e.g. from 2-of-3 to 3-of-5) currently requires revoking and re-granting the permission, resetting the historical `used` allowance counter and active spend nonces.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateQuorumThresholdProposal {
    pub permission_id: u64,
    pub new_threshold: u32,
    pub new_owners: Vec<Address>,
    pub signatures: Vec<BytesN<64>>,
}
```

### Tasks
- Implement `migrate_quorum_threshold(env: Env, proposal: UpdateQuorumThresholdProposal)`.
- Verify signatures of existing quorum threshold before applying changes.
- Preserve `limit_total`, `used`, and nonce sequences across the migration.
- Emit `QuorumThresholdMigratedEvent`.

### Acceptance Criteria
- Quorum configuration updates seamlessly without resetting spent allowance.
- Multi-signature authorization verifies existing threshold before migrating.

---

## Implement Calendar-Bounded Spending Grants (Start and End Ledger Sequence)

- **Estimate:** 1 day
- **Context:** Budgets are often allocated for future events (e.g. Black Friday or next month's billing cycle). Currently, permissions are active immediately upon creation. Calendar bounding allows grants to specify a future activation ledger sequence.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveWindow {
    pub not_before_ledger: u32,
    pub not_after_ledger: u32,
}
```

### Tasks
- Add `not_before_ledger` to permission grant parameters.
- Reject spend attempts where `current_ledger < not_before_ledger` with `PermissionError::GrantNotYetActive`.
- Reject spend attempts where `current_ledger > not_after_ledger` with `PermissionError::Expired`.
- Add unit tests verifying pre-activation spend rejection.

### Acceptance Criteria
- Grants cannot be spent prior to `not_before_ledger`.
- Grants expire automatically after `not_after_ledger`.

---

## Prevent Unauthorized Delegate Cancellation of Pending Allowance Decreases

- **Estimate:** 1 day
- **Context:** In `permissions/src/lib.rs`, when an owner initiates a time-locked allowance decrease, the pending decrease record can be overwritten if a delegate calls certain allowance update helpers. Decreases must be immutable and cancelable only by the owner.

### Data Types & Schemas
```rust
pub fn cancel_pending_decrease(env: Env, owner: Address, delegate: Address) -> Result<(), PermissionError> {
    owner.require_auth(); // Only delegator owner can cancel
    let key = DataKey::PendingDecrease(owner, delegate);
    if !env.storage().persistent().has(&key) {
        return Err(PermissionError::NotFound);
    }
    env.storage().persistent().remove(&key);
    Ok(())
}
```

### Tasks
- Audit all mutation methods in `permissions/src/lib.rs` for inadvertent clearance of `PendingDecrease`.
- Enforce strict `owner.require_auth()` in `cancel_pending_decrease`.
- Add tests confirming delegate cannot erase pending decrease.

### Acceptance Criteria
- Pending allowance decreases cannot be tampered with or cleared by delegates.
- Owner authority is required for cancellation.

---

## Implement Inactivity Pruning and Rent Reclamation on Expired Permissions

- **Estimate:** 1 day
- **Context:** Dormant permissions that expired months ago remain stored in persistent storage slots, incurring blockchain state bloat. Implementing an open rent reclamation method allows keeper bots to prune expired entries and reclaim rent.

### Data Types & Schemas
```rust
pub fn prune_expired_permission(
    env: Env,
    owner: Address,
    delegate: Address,
    keeper: Address
) -> Result<bool, PermissionError> {
    // Check permission expiry against current ledger
    // If expired by > 100,000 ledgers, delete record and emit event
}
```

### Tasks
- Implement `prune_expired_permission` entrypoint in `permissions/src/lib.rs`.
- Verify expiration threshold has elapsed before permitting deletion.
- Emit `PermissionPrunedEvent`.
- Add unit test testing pruning of expired records and rejection of active records.

### Acceptance Criteria
- Expired permissions can be pruned from persistent storage.
- Active permissions cannot be pruned.

---

## Implement Strict Domain Separator Hash Invalidation Across Contract Upgrades

- **Estimate:** 1 day
- **Context:** When a permissions contract is upgraded to a new WASM executable, signed meta-transaction hashes from the previous contract version could potentially be re-submitted if the domain separator does not incorporate the contract's code hash or semver string.

### Data Types & Schemas
```rust
pub fn compute_versioned_domain_separator(env: &Env) -> BytesN<32> {
    let mut payload = soroban_sdk::Bytes::new(env);
    payload.append(&env.current_contract_address().to_xdr(env));
    payload.append(&symbol_short!("PERM_V2").to_xdr(env));
    env.crypto().sha256(&payload).into()
}
```

### Tasks
- Incorporate contract semver and address into the canonical domain separator.
- Validate signatures against the versioned domain separator.
- Add test verifying that signatures generated for V1 fail verification on V2.

### Acceptance Criteria
- Signatures are strictly tied to specific contract versions.
- Prevents cross-version signature replay after contract upgrades.

---

## Enforce Maximum Parent Delegation Depth to Block Call-Stack Exhaustion

- **Estimate:** 1 day
- **Context:** In hierarchical permission delegation (`grant_child`), a child permission can derive further child permissions. Deeply nested delegation chains (e.g. 10+ levels) risk exceeding Soroban VM call stack depth and gas limits during recursive spend validation.

### Data Types & Schemas
```rust
pub const MAX_HIERARCHY_DEPTH: u32 = 3;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HierarchyMetadata {
    pub root_owner: Address,
    pub parent_permission_id: Option<u64>,
    pub depth_level: u32,
}
```

### Tasks
- Record `depth_level` in `HierarchyMetadata` upon child grant creation.
- Reject creation of child permissions where `depth_level >= MAX_HIERARCHY_DEPTH` with `PermissionError::MaxHierarchyDepthExceeded`.
- Add unit test verifying rejection of delegations exceeding depth 3.

### Acceptance Criteria
- Delegation hierarchy is strictly capped at maximum depth 3.
- Prevents stack overflow during permission verification.

---

## Implement Multi-Sig Spend Approval Queue for Enterprise Quorum Grants

- **Estimate:** 2 days
- **Context:** For enterprise multi-owner grants, co-signers cannot always sign simultaneously in real time. Implementing an on-chain approval queue allows delegates to propose spends and co-signers to sign asynchronously over a 24-hour window.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingSpendProposal {
    pub proposal_id: u64,
    pub recipient: Address,
    pub amount: i128,
    pub approvals: Vec<Address>,
    pub expires_at_ledger: u32,
    pub is_executed: bool,
}
```

### Tasks
- Implement `propose_spend(env: Env, delegate: Address, recipient: Address, amount: i128)`.
- Implement `approve_spend_proposal(env: Env, proposal_id: u64)` requiring owner auth.
- Execute token transfer automatically when approvals reach required threshold.
- Add integration test verifying asynchronous co-signing.

### Acceptance Criteria
- Enterprise co-signers can approve transactions asynchronously.
- Transaction executes automatically once quorum is satisfied.

---

## Prevent Replay of Historical Spend Signatures via Versioned Execution Epochs

- **Estimate:** 1 day
- **Context:** If an owner temporarily pauses and later resumes an agent delegation, old unexpired off-chain signatures created before the pause could be executed unexpectedly. Introducing an execution epoch counter invalidates all prior signatures upon resume.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EpochConfig {
    pub current_epoch: u32,
    pub epoch_started_ledger: u32,
}
```

### Tasks
- Increment `current_epoch` whenever a delegation is paused or resumed.
- Include `epoch` in off-chain signed spend authorizations.
- Reject signatures signed against a stale epoch with `PermissionError::StaleEpoch`.
- Add test verifying that signatures from previous epochs fail.

### Acceptance Criteria
- Pausing and resuming invalidates all pending in-flight signatures.
- Prevents execution of stale authorizations.

---

## Implement Verifier Revocation and Retrospective Attestation Invalidation

- **Estimate:** 2 days
- **Context:** In `marketplace/src/lib.rs`, authorized verifiers attest to merchant identity. If a verifier's private key is compromised or a verifier turns rogue, the admin must be able to revoke the verifier and retroactively invalidate all unconfirmed attestations issued by that verifier.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifierRevocationRecord {
    pub verifier: Address,
    pub revoked_at: u64,
    pub reason: Symbol,
}
```

### Tasks
- Implement `revoke_verifier(env: Env, verifier: Address, reason: Symbol)` gated by admin.
- Remove revoked verifier from merchant verification lists.
- Recalculate `merchant.verified` status for affected merchants, downgrading if required verification count is no longer satisfied.
- Add integration test verifying status downgrade upon verifier revocation.

### Acceptance Criteria
- Admin can revoke compromised verifiers.
- Merchants losing required verification threshold transition to unverified automatically.

---

## Handle Verification Policy Threshold Increases for Pre-Existing Merchants

- **Estimate:** 1 day
- **Context:** When governance increases the required verifications policy (e.g. from 1 to 2 verifications), merchants verified under the old policy remain marked as verified indefinitely without obtaining the second attestation.

### Data Types & Schemas
```rust
pub fn recheck_merchant_verification(
    env: &Env,
    merchant_id: u64,
    policy: &VerificationPolicy
) -> bool {
    let current_verifications = get_merchant_verifications_count(env, merchant_id);
    current_verifications >= policy.required
}
```

### Tasks
- Implement dynamic verification check rather than static boolean flag.
- Add `revalidate_merchant_status(env: Env, merchant_id: u64)` entrypoint.
- Provide 30-day grace period for existing merchants to acquire required additional attestations.
- Add test verifying policy transition behavior.

### Acceptance Criteria
- Policy updates apply fairly with transparent grace periods.
- Merchants failing to meet updated standards transition gracefully.

---

## Implement Stable Cursor Pagination by Reputation Score in Discovery Index

- **Estimate:** 2 days
- **Context:** The marketplace merchant discovery index currently sorts by sequential ID. Buyers need to query merchants ranked by reputation score with stable cursor pagination that handles ties without duplicating or skipping items.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationCursor {
    pub last_score_bps: u32,
    pub last_merchant_id: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RankedMerchantPage {
    pub merchants: Vec<MerchantView>,
    pub next_cursor: Option<ReputationCursor>,
    pub total_count: u32,
}
```

### Tasks
- Implement composite secondary index `(score_bps, merchant_id)` for stable sorting.
- Support keyset pagination: `WHERE (score, id) < (cursor.score, cursor.id)`.
- Benchmark gas and CPU instructions for 20-item page queries.
- Add unit tests validating stable pagination across merchants with identical scores.

### Acceptance Criteria
- Merchants are returned in deterministic descending reputation order.
- Pagination cursor handles identical scores without skipping records.

---

## Implement Multi-Tier Merchant Commission Schedules Based on Historical Volume

- **Estimate:** 2 days
- **Context:** Merchants currently have static `commission_rate_bps`. Implementing automated tiered commission schedules (e.g. 5% standard, 3% above 50,000 XLM volume, 1.5% above 500,000 XLM volume) incentivizes high-volume merchants natively on-chain.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommissionTier {
    pub min_settled_volume: i128,
    pub commission_rate_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantVolumeRecord {
    pub total_settled_volume: i128,
    pub active_tier_bps: u32,
    pub last_updated_ledger: u32,
}
```

### Tasks
- Track settled volume updates from escrow release events.
- Evaluate merchant qualification for higher tiers dynamically.
- Update active commission rate automatically upon crossing volume thresholds.
- Add unit tests validating volume tier transitions.

### Acceptance Criteria
- Merchants automatically receive discounted commission rates upon hitting volume milestones.
- Commission calculation matches active tier schedule.

---

## Implement Suspended Merchant Appeal Staking Bond with Automatic Slashing

- **Estimate:** 2 days
- **Context:** When a merchant is suspended for policy violations, they should be able to appeal to community governance by locking a refundable staking bond. If the appeal is rejected as frivolous, the bond is slashed into the customer restitution pool.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantAppealBond {
    pub merchant_id: u64,
    pub bond_amount: i128,
    pub bond_token: Address,
    pub appealed_at_ledger: u32,
    pub resolution_status: AppealResolution,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppealResolution {
    Pending,
    UpheldAndReinstated,
    RejectedAndSlashed,
}
```

### Tasks
- Implement `file_merchant_suspension_appeal(env: Env, merchant_id: u64, bond_amount: i128)`.
- Hold bond tokens in contract instance storage.
- Implement admin / governance resolution: refund bond on reinstatement, transfer slashed bond to treasury on rejection.
- Add unit tests simulating both appeal outcomes.

### Acceptance Criteria
- Suspended merchants must post economic stake to file appeals.
- Bonds are refunded or slashed deterministically based on ruling.

---

## Enforce Dynamic Cooldown on Sensitive Merchant Profile Metadata Updates

- **Estimate:** 1 day
- **Context:** A compromised merchant owner account could rapidly rotate payout addresses or storefront names to evade fraud monitoring. Enforcing a strict 24-hour cooldown between payout address updates prevents rapid credential hijacking.

### Data Types & Schemas
```rust
pub const PAYOUT_CHANGE_COOLDOWN_LEDGERS: u32 = 17_280; // ~24 hours

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingPayoutChange {
    pub merchant_id: u64,
    pub proposed_payout_address: Address,
    pub effective_at_ledger: u32,
}
```

### Tasks
- Require a 24-hour timelock on changes to merchant payout address.
- Notify off-chain indexers via `MerchantPayoutChangeScheduledEvent`.
- Allow merchant owner to cancel pending change during timelock if unauthorized.
- Add tests verifying timelock enforcement.

### Acceptance Criteria
- Payout address changes require a 24-hour waiting period.
- Immediate address swapping attacks are neutralized.

---

## Implement Automated Platform Fee Split with Affiliate Referral Addresses

- **Estimate:** 2 days
- **Context:** To incentivize ecosystem growth, merchants onboarded via affiliate referrers should share a percentage of platform commission fees (e.g. 20% of platform fees) directly on-chain with the referrer's address.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AffiliateConfig {
    pub referrer_address: Address,
    pub referral_share_bps: u32, // e.g. 2000 = 20% of platform fee
    pub expires_at_ledger: u32,
}
```

### Tasks
- Add optional `affiliate` configuration to merchant registration.
- Split platform fee automatically during escrow fee disbursement.
- Transfer referrer share directly to `referrer_address`.
- Add tests validating three-way fee splits (seller, platform, referrer).

### Acceptance Criteria
- Affiliates automatically receive their commission share on each settled order.
- Accounting invariants hold across all token disbursements.

---

## Prevent Sybil Merchant Flooding via Escalating Registration Deposit Curve

- **Estimate:** 1 day
- **Context:** Zero-fee merchant registration allows bots to spam thousands of phantom merchant profiles, polluting search discovery and exhausting persistent storage. Implementing a modest refundable registration deposit curtails Sybil creation.

### Data Types & Schemas
```rust
pub const BASE_REGISTRATION_DEPOSIT_STROOPS: i128 = 50_000_000; // 5 XLM

pub fn compute_registration_deposit(total_merchants_registered: u64) -> i128 {
    // Dynamic or fixed deposit schedule
    BASE_REGISTRATION_DEPOSIT_STROOPS
}
```

### Tasks
- Require `registration_deposit` in native XLM or SAC token on `register_merchant`.
- Hold deposit in contract storage until merchant voluntarily deregisters.
- Refund deposit upon clean store closure if no open escrows exist.
- Add tests validating deposit locking and refund.

### Acceptance Criteria
- Registration requires economic stake, eliminating zero-cost Sybil spam.
- Honest merchants receive 100% deposit refund upon orderly closure.

---

## Implement Merchant Storefront Closure Grace Period for Pending Escrows

- **Estimate:** 1 day
- **Context:** If a merchant closes their store via `close_merchant`, ongoing escrows could be left in an ambiguous state. A merchant cannot be closed while active or disputed escrows remain unsettled.

### Data Types & Schemas
```rust
pub fn can_close_merchant(env: &Env, merchant_id: u64, active_escrow_count: u32) -> Result<(), MarketplaceError> {
    if active_escrow_count > 0 {
        return Err(MarketplaceError::ActiveOrdersPending);
    }
    Ok(())
}
```

### Tasks
- Check active order count before allowing status transition to `MerchantStatus::Closed`.
- Reject closure requests with `MarketplaceError::ActiveOrdersPending` if escrows remain open.
- Allow merchant to transition to `MerchantStatus::Inactive` (blocking new orders while fulfilling existing ones).
- Add unit tests testing closure lifecycle.

### Acceptance Criteria
- Merchants with active orders cannot close store or withdraw registration bond.
- Protects buyers from abandoned merchant checkouts.

---

## Implement Community Governance Proposal Workflow for New Product Taxonomy Categories

- **Estimate:** 2 days
- **Context:** Marketplace category symbols are currently hardcoded or admin-controlled. A community governance proposal workflow allows verified merchants to propose and vote on adding new standardized product categories.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CategoryProposal {
    pub proposed_symbol: Symbol,
    pub proposed_by: Address,
    pub vote_count: u32,
    pub voting_deadline: u64,
    pub is_approved: bool,
}
```

### Tasks
- Implement `propose_category(env: Env, symbol: Symbol)`.
- Allow verified merchants to vote with 1-merchant-1-vote quorum.
- Promote category to canonical active list once vote threshold is met.
- Add integration test verifying proposal creation, voting, and activation.

### Acceptance Criteria
- Ecosystem can organically expand categories via merchant voting.
- Invalid or spam symbols are filtered by quorum requirements.

---

## Implement Volume-Weighted vs Count-Weighted Composite Reputation Scoring

- **Estimate:** 2 days
- **Context:** In `reputation/src/lib.rs`, every review carries equal weight regardless of transaction size. A merchant could generate 50 positive reviews on $1 items to mask a $10,000 fraudulent order. Implementing a composite score combining volume weighting and transaction count prevents rating manipulation.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompositeScore {
    pub count_score_bps: u32,
    pub volume_score_bps: u32,
    pub blended_score_bps: u32,
    pub confidence_rating: u32,
}
```

### Tasks
- Track both transaction count and dollar volume in feedback records.
- Calculate volume-weighted score using logarithmic dampening to avoid whale domination.
- Blend count score (40%) and volume score (60%) into canonical `blended_score_bps`.
- Add unit tests comparing ratings under diverse purchase amounts.

### Acceptance Criteria
- High-value transactions carry appropriate statistical significance.
- Micro-transaction spam cannot artificially elevate merchant reputation.

---

## Optimize Half-Life Exponential Decay Math to Prevent Soroban CPU Throttling

- **Estimate:** 2 days
- **Context:** Computing time decay on reputation scores using floating-point approximations or iterative loops consumes excessive CPU instructions, hitting Soroban transaction resource limits. Implementing a fast fixed-point Taylor series or lookup table optimizes gas costs.

### Data Types & Schemas
```rust
pub const DECAY_HALF_LIFE_LEDGERS: u32 = 518_400; // ~30 days

pub fn compute_fixed_point_decay(initial_score: u32, elapsed_ledgers: u32) -> u32 {
    // Fast bit-shift and fixed-point polynomial approximation
}
```

### Tasks
- Implement fixed-point integer decay function without allocations or loops.
- Benchmark CPU instruction reduction against previous implementation.
- Verify accuracy within 0.1% tolerance across 365-day simulated time spans.

### Acceptance Criteria
- Decay calculation executes in < 5,000 CPU instructions.
- Mathematical model converges accurately over multi-month intervals.

---

## Implement Slashing of Merchant Reputation Score upon Confirmed Dispute Fraud

- **Estimate:** 1 day
- **Context:** When an escrow dispute is resolved with a ruling of intentional merchant fraud or counterfeit goods, the merchant's reputation score must suffer an immediate deterministic penalty beyond a normal negative review.

### Data Types & Schemas
```rust
pub const FRAUD_PENALTY_SCORE_BPS: u32 = 2500; // -25% reputation slash

pub fn apply_fraud_reputation_slash(
    env: &Env,
    merchant: &Address,
    penalty_bps: u32
) -> Result<u32, ReputationError> {
    // Deduct penalty and record on-chain strike
}
```

### Tasks
- Allow authorized escrow contract to invoke `apply_fraud_reputation_slash`.
- Deduct fixed penalty from merchant reputation score.
- Record permanent on-chain strike count.
- Add test verifying score reduction on fraud determination.

### Acceptance Criteria
- Confirmed fraud severely degrades reputation score immediately.
- Caller authorization restricted to trusted escrow contract.

---

## Prevent Review Flooding via Cooldown Window Between Identical Buyer-Seller Pairs

- **Estimate:** 1 day
- **Context:** A colluding buyer and seller could execute multiple rapid micro-orders within minutes to flood the reputation contract with positive reviews before algorithms detect the pattern.

### Data Types & Schemas
```rust
pub const REVIEW_COOLDOWN_LEDGERS: u32 = 1_440; // ~2 hours

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    LastReviewLedger(Address, Address), // (buyer, seller) -> ledger
}
```

### Tasks
- Record `LastReviewLedger(buyer, seller)` in persistent storage.
- Reject new reviews between the same pair if `current_ledger < last_ledger + REVIEW_COOLDOWN_LEDGERS`.
- Return `ReputationError::ReviewCooldownActive` on violation.
- Add unit tests validating cooldown enforcement.

### Acceptance Criteria
- Rapid back-to-back reviews between the same accounts are blocked.
- Reviews after cooldown window succeed normally.

---

## Implement Cryptographic Reputation Score Attestation Generation for Off-Chain Clients

- **Estimate:** 2 days
- **Context:** Off-chain services (such as buyer agent LLM search engines) need to verify merchant reputation scores without making real-time RPC calls for every candidate item. Generating signed reputation attestations allows caching with cryptographic validity.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationAttestation {
    pub entity: Address,
    pub score_bps: u32,
    pub total_transactions: u32,
    pub certified_at_ledger: u32,
    pub expires_at_ledger: u32,
}
```

### Tasks
- Implement `generate_score_attestation(env: Env, entity: Address) -> ReputationAttestation`.
- Include validity window and sequence checkpoint.
- Provide client SDK helper to verify attestation against contract state.
- Add test cases testing attestation generation.

### Acceptance Criteria
- Attestations provide tamper-proof snapshots of reputation metrics.
- Expired attestations can be detected and refreshed by clients.

---

## Enforce Strict Numeric Normalization to Prevent Score Underflow and Clipping

- **Estimate:** 1 day
- **Context:** In edge cases where a merchant receives extreme consecutive negative reviews, intermediate integer subtraction could underflow or clip incorrectly, resulting in either a panic or an integer wraparound to `u32::MAX`.

### Data Types & Schemas
```rust
pub fn normalize_score(raw_score: i64) -> u32 {
    if raw_score <= 0 {
        0
    } else if raw_score >= 10_000 {
        10_000
    } else {
        raw_score as u32
    }
}
```

### Tasks
- Wrap all score recalculation math with safe bounded normalization.
- Enforce that scores remain strictly in `0..=10_000` (0% to 100%).
- Add property tests simulating 100 consecutive negative reviews.

### Acceptance Criteria
- Scores never wrap around or cause arithmetic overflow/underflow.
- Ratings are bounded cleanly between 0 and 10,000 basis points.

---

## Differentiate Scoring Weight Models Between Buyer Agents and Verified Merchants

- **Estimate:** 2 days
- **Context:** Buyer agents and merchants have fundamentally different risk profiles: merchants risk non-delivery and counterfeit items, while buyer agents risk payment delinquency and frivolous dispute abuse. Implementing specialized scoring profiles separates the two domains.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntityRole {
    Merchant,
    BuyerAgent,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoleSpecificMetrics {
    pub role: EntityRole,
    pub fulfillment_rate_bps: u32,
    pub dispute_frequency_bps: u32,
    pub timely_payment_rate_bps: u32,
}
```

### Tasks
- Add `EntityRole` classification in reputation records.
- Track role-specific parameters (fulfillment rate for merchants, dispute rate for buyers).
- Expose tailored query methods for each role.
- Add unit tests verifying separate metric calculation.

### Acceptance Criteria
- Merchants and buyer agents are evaluated using relevant domain metrics.
- Risk assessment algorithms access targeted performance indicators.

---

## Prevent Rollback Exploits from Reviving Administratively Revoked Delegations

- **Estimate:** 1 day
- **Context:** In `delegation_registry/src/lib.rs`, `rollback_delegation` allows restoring a delegation record to a previous version snapshot. If an admin or owner explicitly revoked a compromised delegation for security reasons, a rollback to an earlier version could accidentally revive the active status.

### Data Types & Schemas
```rust
pub fn validate_rollback_security(
    current: &DelegationRecord,
    target_snapshot: &DelegationRecord
) -> Result<(), DelegationError> {
    if current.status == DelegationStatus::Revoked {
        return Err(DelegationError::RevokedCannotBeRestored);
    }
    Ok(())
}
```

### Tasks
- Enforce that `DelegationStatus::Revoked` is a permanent terminal state immune to rollback.
- Reject rollback requests targeting revoked delegations with `DelegationError::RevokedCannotBeRestored`.
- Add unit test verifying that rollback cannot reactivate a revoked delegation.

### Acceptance Criteria
- Revoked delegations cannot be revived through snapshot rollback.
- Security revocation guarantees remain inviolable.

---

## Implement Batch Delegation Expiration Sweep with Yielding Storage Rent Reclaim

- **Estimate:** 2 days
- **Context:** In `delegation_registry/src/lib.rs`, sweeping expired delegations one-by-one is cost-inefficient. Implementing an iterative batch sweeper that yields rent refunds to caller incentivizes automated keeper bots to clean the registry.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SweepResult {
    pub swept_records_count: u32,
    pub next_cursor_id: Option<u64>,
    pub rent_reclaimed_stroops: i128,
}
```

### Tasks
- Implement `sweep_expired_batch(env: Env, start_id: u64, max_count: u32) -> SweepResult`.
- Iterate up to 50 records, archiving expired records and removing auxiliary keys.
- Return continuation cursor for sequential keeper processing.
- Add unit tests validating batch progression across multiple pages.

### Acceptance Criteria
- Keepers can sweep up to 50 expired delegations per transaction.
- Continuous iteration cleanups storage without out-of-gas errors.

---

## Implement Automated Delegation Keeper Heartbeat Incentive Bounty

- **Estimate:** 2 days
- **Context:** To guarantee that long-lived delegations do not expire in Soroban persistent storage TTL while active, implement a protocol keeper bounty that rewards third-party callers for extending storage TTL on delegations approaching expiration.

### Data Types & Schemas
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeeperBountyStatus {
    pub delegation_id: u64,
    pub remaining_ledgers: u32,
    pub is_eligible_for_bounty: bool,
    pub bounty_amount_stroops: i128,
}
```

### Tasks
- Define TTL threshold where keeper bounty becomes claimable (e.g. < 50,000 ledgers).
- Implement `bump_delegation_ttl_with_reward(env: Env, delegation_id: u64, keeper: Address)`.
- Extend TTL and transfer small XLM reward to keeper from protocol maintenance reserve.
- Add tests validating bounty eligibility and payout.

### Acceptance Criteria
- Keepers are incentivized to maintain contract storage freshness.
- Active delegations are protected from accidental archival.

---

## Implement Comprehensive Cross-Contract Reentrancy Fuzzing Test Suite

- **Estimate:** 2 days
- **Context:** Soroban cross-contract calls between escrow, permissions, marketplace, and external tokens require exhaustive fuzz testing with simulated adversarial contracts to prove immunity to reentrancy and unexpected state reverts.

### Data Types & Schemas
```rust
#[contract]
pub struct MaliciousReentrantAttacker;

#[contractimpl]
impl MaliciousReentrantAttacker {
    pub fn callback_hook(env: Env, target_escrow: Address, escrow_id: u64) {
        // Attempt reentrant invocation during token transfer callback
    }
}
```

### Tasks
- Build mock malicious token and oracle contracts in `tests/cross_contract.rs`.
- Attempt reentrant callbacks to `release`, `refund`, and `dispute` during fund transfer hooks.
- Assert that all reentrant attempts fail with reentrancy errors or Soroban VM reentrancy traps.
- Integrate fuzz harness into repository CI.

### Acceptance Criteria
- All contracts prove immune to reentrancy under automated attack simulation.
- Test suite passes in CI.

---

## Implement Formal Invariant Verification Suite for Multi-Contract Escrow Value Flows

- **Estimate:** 2 days
- **Context:** Define automated property-based testing assertions verifying end-to-end mathematical conservation of tokens: Buyer Wallet Balance Delta + Seller Wallet Balance Delta + Platform Fee Account Delta == 0 across all possible execution paths.

### Data Types & Schemas
```rust
#[test]
fn verify_global_token_conservation_invariant() {
    // Deploy all 5 contracts
    // Randomize 1,000 deposits, partial releases, refunds, disputes, and yield accruals
    // Assert global token supply and account balances balance to 0 Stroop discrepancy
}
```

### Tasks
- Implement global token balance tracker across test environments.
- Generate randomized multi-step workflows with varying basis points and split ratios.
- Assert zero balance leakage across 10,000 randomized state transitions.

### Acceptance Criteria
- Mathematical value conservation is validated across 10,000 iterations.
- Zero token leakage or rounding drift under all scenarios.

---

## Implement Automated Contract WASM Size and Gas Benchmarking in CI Pipeline

- **Estimate:** 1 day
- **Context:** Soroban enforces strict limits on contract WASM binary size (< 64KB recommended for optimized deployment) and execution CPU/RAM instructions. Adding automated CI benchmarking ensures pull requests do not unintentionally bloat contract binaries.

### Data Types & Schemas
```bash
# In CI script
cargo build --target wasm32-unknown-unknown --release --workspace
soroban contract optimize --wasm target/wasm32-unknown-unknown/release/*.wasm
# Assert wasm_size < 65536 bytes
```

### Tasks
- Create a GitHub Actions benchmarking workflow measuring WASM size after `soroban contract optimize`.
- Track CPU and RAM instructions for standard contract execution flows.
- Post size diff and resource consumption tables on pull requests.

### Acceptance Criteria
- WASM binaries are continuously monitored against size limits.
- CI blocks pull requests that exceed gas or binary size budgets.

---

