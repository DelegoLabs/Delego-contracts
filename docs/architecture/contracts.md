# Smart Contract Architecture

Delego uses Soroban smart contracts to anchor trust-critical state on the Stellar blockchain, ensuring security, transparency, and programmable trust for agent-mediated commerce.

## 📋 Table of Contents

- [Overview](#overview)
- [Contract Types](#contract-types)
- [On-Chain vs Off-Chain](#on-chain-vs-off-chain)
- [Contract Interactions](#contract-interactions)
- [State Management](#state-management)
- [Cold-Storage & State Maintenance Utilities](#cold-storage--state-maintenance-utilities)
- [Upgrade Patterns](#upgrade-patterns)
- [Security Considerations](#security-considerations)

## Overview

Smart contracts are used for trust-critical operations that require blockchain guarantees, while off-chain services handle high-throughput operations like catalog search and product discovery.

### Design Principles

- **Trust-Critical On-Chain**: Only trust-critical state on-chain
- **Off-Chain Efficiency**: High-throughput operations off-chain
- **Minimal Gas**: Optimize for minimal gas usage
- **Upgradeability**: Design for contract upgrades
- **Security First**: Prioritize security in all contracts

## Contract Types

### Escrow Contract

**On-chain State**: Locked funds per order

#### Purpose

The escrow contract holds funds in trust during agent-mediated purchases, releasing funds only when predefined conditions are met.

#### Key Functions

- `create(escrow_id, buyer, seller, token)`: Create an unfunded escrow record in `Created` status
- `deposit(...)` / `fund(...)`: Lock buyer funds for an order
- `release(escrow_id)` / `partial_release(...)`: Transfer remaining/partial balance to seller
- `refund(escrow_id)`: Return funds to buyer (after timeout if needed)
- `dispute(escrow_id)` / `resolve_dispute(...)` / `resolve_dispute_quorum(...)`: Dispute lifecycle
- `cancel(escrow_id)`: Merchant cancels an unfunded escrow, subject to the
  cancellation protection window (issue #355)
- `accept_order(escrow_id)`: Seller records acceptance and re-anchors the
  protection window; `agree_cancel(escrow_id)`: buyer waives it
- `get_cancel_eligibility(escrow_id, caller)` /
  `get_order_acceptance(escrow_id)`: deterministic, read-only cancel guards
- Admin: `set_cancel_lockout(ledgers)` / `get_cancel_lockout()`
- `get_escrow(escrow_id)`: Get full escrow record
- `get_receipt(escrow_id)` / `get_merchant_receipt(...)`: Buyer/seller receipts
- `get_release_eligibility(...)` / `get_refund_eligibility(...)` / `get_timeout_view(...)`: Read-only eligibility checks
- Admin: `set_limits`, `update_fee`, `set_min_fee_stroops`, `add_token`, `set_create_paused`, `propose_admin`, `accept_admin`, `add_co_admin`

#### State

```rust
struct EscrowRecord {
    escrow_id: u64,
    buyer: Address,
    seller: Address,
    token: Address,
    amount: i128,
    released_amount: i128,
    refunded_amount: i128,
    status: EscrowStatus,
    order_id: BytesN<32>,
    created_at: u64,
    updated_at: u64,
    timeout_ledger: u32,
}

enum EscrowStatus {
    Created,
    Funded,
    Released,
    Refunded,
    Cancelled,
    Disputed,
}

// Cancellation protection snapshot (issue #355), one per escrow.
struct OrderAcceptanceState {
    seller_accepted: bool,
    accepted_at_ledger: u32,
    cancel_lockout_ledgers: u32,
}
```

#### Cancellation protection (issue #355)

An order is created before the buyer funds it, so a seller watching the mempool
could get a `cancel` in front of a pending `fund`/`deposit`. Each escrow
snapshots a protection window (default 10 ledgers, `0..=1000`, admin-set via
`set_cancel_lockout`) at creation and on every `accept_order`. Inside that
window `cancel` fails with `CancelLockoutActive` unless the buyer recorded
`agree_cancel` or the escrow's own timeout was reached. Once the buyer's
deposit lands the escrow is `Funded` and `cancel` always fails with
`AlreadyFunded`, so a submitted deposit can never be unwound. Missing snapshot
state fails closed, and `get_cancel_eligibility` returns the same answer
`cancel` would give.

#### Use Cases

- Buyer approves purchase → funds locked in escrow
- Delivery confirmed → funds released to merchant
- Delivery failed → funds refunded to buyer
- Dispute → funds held until resolution (admin or arbiter quorum)
- Seller stalls on an order → seller cancels unilaterally after the protection
  window, or the buyer agrees with `agree_cancel`

### Permissions Contract

**On-chain State**: Delegate spending limits

#### Purpose

The permissions contract manages delegated spending authority, allowing users to grant agents limited permission to spend on their behalf.

#### Key Functions

- `grant(owner, delegate, ...)`: Grant spending permission
- `grant_child(owner, delegate, ...)`: Derive a nested permission from an existing grant
- `revoke(owner, delegate)`: Revoke spending permission
- `transfer_permission(owner, ...)`: Transfer a permission to another account
- `can_spend(owner, delegate, amount)`: Check if amount is within limit
- `execute_spend(owner, delegate, ...)`: Spend within limits (emits `PermissionSpendEvent`)
- `get_permission(owner, delegate)`: Get permission details
- `increase_allowance(...)` / `decrease_allowance(...)`: Adjust spending limit
- `renew_permission(...)` / `update_expiry(...)`: Manage expiry
- `execute_spend_via_relayer(...)`: Gasless spend via relayer signature
- `grant_multi_owner(...)`: Multi-owner (quorum) grants
- `execute_spend_multi(...)`: Spend authorized by a quorum of co-signers in one transaction
- `propose_spend(...)` / `approve_spend_proposal(...)`: Asynchronous quorum spends — the delegate queues a `PendingSpendProposal`, co-signers approve in separate transactions, and the spend settles automatically once the grant's `threshold` is met
- `cancel_spend_proposal(...)` / `get_spend_proposal(...)` / `get_spend_proposals(...)`: Manage and inspect the pending spend queue (issue #377)
- `pause(...)` / `resume(...)` / `pause_grants(...)`: Pause controls
- `set_admin(...)` / `propose_admin(...)` / `accept_admin(...)`: Admin management

#### State

```rust
struct PermissionRecord {
    delegate: Address,
    limit_per_transaction: i128,
    limit_total: i128,
    used: i128,
    expiry: u64,
    status: PermissionStatus,
}
```

#### Use Cases

- User creates delegation → permission granted to agent
- Agent attempts payment → permission checked (`can_spend`)
- Spending limit reached → payment blocked
- User revokes delegation → permission revoked

### Delegation Registry Contract

**On-chain State**: Delegation records

#### Purpose

Tracks delegation records with expiry and versioned rollback/upgrade support.

#### Key Functions

- Register and update delegation records
- Read delegation state for off-chain services
- Versioned rollback of delegation state

### Reputation Contract

**On-chain State**: Cumulative scores

#### Purpose

The reputation contract tracks on-chain reputation scores for merchants and agents, enabling trust-based decision making.

- `record_transaction(merchant, amount, rating)`: Record transaction and rating
- `get_reputation(entity)`: Get reputation score

### Marketplace Contract

**On-chain State**: Merchant registry, multi-verifier verification, commission configuration, category discovery index, metadata cooldown policy

#### Purpose

The marketplace contract maintains a trusted on-chain registry of merchants, enabling discovery and verification of merchants, per-merchant commission tracking, reputation score snapshot pairing, and status lifecycle controls (suspend/unsuspend/close). Registration, profile updates, and verification are multi-signer safe: merchants self-register, a configured set of verifiers attests identity, and an admin (with two-step `propose_admin`/`accept_admin` handover) moderates.

#### Key Data Structures

```rust
struct RegisterParams {
    name: String,
    description: String,
    category: Symbol,
    image_url: String,
    metadata: Option<String>,
    required_verifications: u32,
}

struct Merchant {
    id: u64,
    owner: Option<Address>,
    name: String,
    description: String,
    category: Symbol,
    image_url: String,
    commission_rate_bps: u32,
    metadata: Option<String>,
    status: MerchantStatus,
    verified: bool,
    created_at: u64,
    updated_at: u64,
    reputation: Option<Address>,
}

struct MerchantView {
    id: u64,
    name: String,
    category: Symbol,
    commission_rate_bps: u32,
    verified: bool,
    status: MerchantStatus,
    reputation_score: Option<u32>,
}

struct CommissionTier {
    min_settled_volume: i128,
    commission_rate_bps: u32,
}

struct MerchantVolumeRecord {
    total_settled_volume: i128,
    active_tier_bps: u32,
    last_updated_ledger: u32,
}

struct VerificationPolicy {
    required: u32,      // verifications needed to become Verified
    max_verifications: u32,
}

struct Verifier {
    address: Address,
    label: Symbol,
    registered_at: u64,
}

struct CooldownConfig {
    value_seconds: u64,  // current metadata-update cooldown
    min_seconds: u64,     // 60s floor
    max_seconds: u64,     // 30-day ceiling
}
```

#### Status Model

`MerchantStatus` is an explicit `#[repr(u32)]` lifecycle enum:

```rust
enum MerchantStatus {
    Registered = 0, // Created, not yet verified
    Verified = 1,   // Passed the verification threshold
    Suspended = 2,  // Temporarily disabled (admin action / review)
    Closed = 3,     // Permanently removed
}
```

Transitions are enforced by helpers (`check_not_frozen_or_closed`) so that suspended/closed merchants cannot be modified, re-verified, or have commissions changed. Unsuspending restores `Verified` or `Registered` depending on the `verified` flag.

#### Key Functions

- `register_merchant(merchant, params)`: Self-register a merchant; derives `RegisterParams`, assigns the next monotonic id, builds the `Merchant` record, and indexes it in `MerchantIds` and `CategoryIndex`
- `is_name_available(name)`: Check a merchant name is not already claimed
- `update_merchant_profile(...)` / `update_metadata(...)`: Owner/admin updates; metadata writes for non-admins are gated by the cooldown policy (`MetadataLockActive`)
- `verify_merchant(merchant_id, verifier)`: Registered verifier attests a merchant; when `VerifiedCount` reaches the policy's `required` threshold the merchant flips to `Verified`
- `revoke_verification(admin, merchant_id)`: Admin clears verification state and resets `VerifiedCount`/verifier list
- `add_verifier(...)` / `remove_verifier(...)`: Admin manages the verifier set; removal is rejected if it would strand an existing policy (`required > remaining verifiers`)
- `get_merchant(merchant_id)` / `get_merchant_view(merchant_id)`: Full record vs. discovery view; the view injects a `reputation_score` snapshot by cross-contract calling the paired reputation contract (`get_reputation`)
- `get_merchants(offset, limit)` / `get_merchants_by_category(category, offset, limit)`: Paginated discovery over `MerchantIds` / `CategoryIndex` (page size capped at 50)
- `set_merchant_commission(...)` / `get_commission(...)`: Per-merchant commission in basis points (≤ 10_000)
- `set_commission_tiers(...)` / `get_commission_tiers(...)`: Configure the ordered volume-based commission tier schedule
- `record_settled_volume(merchant_id, amount)`: Accrue settled volume from escrow release events and re-evaluate the merchant's active tier
- `get_volume_record(merchant_id)`: Read the merchant's `MerchantVolumeRecord` (total settled volume, active tier bps, last updated ledger)
- `get_active_commission(merchant_id)`: Return the commission rate matching the merchant's active tier
- `suspend_merchant(...)` / `unsuspend_merchant(...)` / `close_merchant(...)`: Admin moderation lifecycle
- `set_appeal_bond_token(...)` / `file_merchant_suspension_appeal(...)` / `resolve_merchant_appeal(...)`: Bonded suspension appeals; upheld rulings refund and reinstate, rejected rulings slash to the configured restitution treasury
- `set_merchant_reputation(...)` / `set_reputation_contract(...)`: Pair a merchant (or the whole registry) with a reputation contract for score injection
- `propose_admin(...)` / `accept_admin(...)`: Two-step admin handover
- `set_metadata_cooldown(...)` / `get_metadata_cooldown(...)`: Configure the metadata update cooldown, clamped to `[60s, 30d]` (default 24h)
- `version()`: Returns contract name and semver (`0.2.0`)

#### State (Storage Keys)

- Instance: `Admin`, `PendingAdmin`, `NextMerchantId`, `Verifiers`, `MetadataCooldown`/`MetadataCooldownConfig`, `GlobalReputationContract`, `AppealBondToken`, `AppealTreasury`
- Persistent per merchant: `Merchant(id)`, `MerchantName(name)`, `FreedName(name)`, `ArchivedMerchant(id)`, `VerifiedCount(id)`, `VerificationPolicy(id)`, `MerchantVerifier(id, verifier)`, `MerchantVerifierList(id)`, `LastMetadataUpdate(id)`
- Persistent per merchant: `MerchantVolume(id)` (`MerchantVolumeRecord`); instance: `CommissionTiers` (`Vec<CommissionTier>`)
- Persistent appeal records: `AppealBond(merchant_id)`
- Persistent indexes: `MerchantIds` (all ids), `CategoryIndex(category)` (ids per category)

#### CategoryIndex & Discovery

`CategoryIndex` maps a `Symbol` category to a `Vec<u64>` of merchant ids, appended on registration and read with offset/limit pagination so off-chain services can render category-filtered storefronts without scanning every merchant. TTL for all persistent entries is extended on access/creation (`~30 days` of ledgers).

#### Cooldown Policy

Metadata updates are rate-limited to prevent squatting/abuse: a non-admin owner may only update `metadata` once per cooldown window (default 24 hours, configurable between 60 seconds and 30 days). Admin updates bypass the cooldown. Exceeding it returns `MetadataLockActive`.

#### Volume-Based Commission Tiers

Merchants accrue `total_settled_volume` as escrow releases settle. The contract evaluates the ordered `CommissionTier` schedule (e.g. 5% standard, 3% above 50,000 XLM, 1.5% above 500,000 XLM) and, when a threshold is crossed, updates `active_tier_bps` and `last_updated_ledger` on the merchant's `MerchantVolumeRecord`. Commission calculation reads the active tier so charged rates always match the merchant's current schedule. Tier transitions are covered by unit tests validating each volume milestone.

#### Use Cases

- Merchant registers with name/category/commission intent → `Registered`
- Registered verifiers attest identity → threshold reached → `Verified`
- Storefront/catalog services page through `get_merchants_by_category`
- Merchant misconduct → `Suspended`; repeat offense → `Closed` (permanently removed from discovery)
- Merchant crosses a volume milestone → active commission tier drops automatically → discounted rate applied on settlement

## On-Chain vs Off-Chain

### On-Chain (Smart Contracts)

Trust-critical operations that require blockchain guarantees:

- **Escrow**: Fund locking and release
- **Permissions**: Spending authority delegation
- **Delegation Registry**: Delegation records with expiry and rollback
- **Reputation**: Reputation score tracking
- **Marketplace**: Merchant registry, verification, and discovery

### Off-Chain (Services)

High-throughput operations that don't require blockchain guarantees:

- **Catalog**: Product catalog and search
- **Search**: Product search and comparison
- **Analytics**: Spending analytics and reporting
- **Notifications**: Email and push notifications

### Hybrid Approach

Some operations use a hybrid approach:

- **Order Creation**: Off-chain order creation, on-chain escrow
- **Payment**: Off-chain payment initiation, on-chain settlement
- **Reputation**: Off-chain rating collection, on-chain aggregation

## Contract Interactions

### Cross-Contract Calls

Contracts can call other contracts:

```rust
// Escrow contract calling Permissions contract
let allowed = permissions::can_spend(
    &e,
    &owner,
    &delegate,
    &amount
);
```

### Contract-to-Service Communication

Services interact with contracts via the wallet service:

```
Wallet Service
    ↓
Soroban RPC
    ↓
Smart Contracts
```

### Event Emission

Contracts emit events for off-chain services.

**Topic schema.** Entity-scoped lifecycle events carry the entity id as a third
topic — `(contract, action, entity_id)` — so indexers and Soroban RPC
subscriptions can filter by entity without deserializing every event body
(issue #142). The id is also kept in the event data for convenience.

```rust
// escrow lifecycle: (escrow, <action>, escrow_id)
env.events().publish(
    (symbol_short!("escrow"), symbol_short!("released"), escrow_id),
    EscrowReleasedEvent { escrow_id, seller, amount, released_by },
);

// marketplace lifecycle: (mkplc, <action>, merchant_id)
env.events().publish(
    (symbol_short!("mkplc"), symbol_short!("reg"), merchant_id),
    MerchantRegisteredEvent { merchant_id, owner, name },
);
```

The id topic's type matches the event's own id field: escrow events use the
`u64` `escrow_id`, except `metadata` and `cancelled` which route by the
`BytesN<32>` order id (their `escrow_id` field is the order id); marketplace
merchant events use the `u64` `merchant_id`. Contract-wide events with no single
entity to route by — admin transfer, pause, fee distribution, liquidity-pool
funding/withdrawal — keep the two-topic `(contract, action)` form.

## State Management

### Persistent Storage

Contract state is stored in persistent Soroban storage:

```rust
// Store permission
e.storage().persistent().set(
    &StorageKey::from(b"permission"),
    &permission
);

// Retrieve permission
let permission: Permission = e.storage()
    .persistent()
    .get(&StorageKey::from(b"permission"))
    .unwrap();
```

### Temporary Storage

Temporary storage for ephemeral data:

```rust
// Store temporary data
e.storage().temporary().set(
    &StorageKey::from(b"temp"),
    &data
);
```

### Instance Storage

Instance storage for contract instances:

```rust
// Store instance data
e.storage().instance().set(
    &StorageKey::from(b"instance"),
    &data
);
```

## Cold-Storage & State Maintenance Utilities

To prevent dead state accumulation and bound storage costs on-chain, Delego contracts implement a standardized maintenance (sweep and prune) interface across all crates.

### Design Principles

1. **Bounded Batch Operations**: Maintenance operations are strictly bounded (e.g. `MAX_SWEEP_BATCH = 50` or `MAX_PAGE_LIMIT = 50`) to ensure deterministic gas and execution budgets per transaction.
2. **Access Control**:
   - **Public Expiry Sweeps**: State transitions gated strictly by deterministic rules (e.g. sequence number expiry or inactivity timestamp) can be triggered by any caller.
   - **Admin-Gated Pruning**: Modifications to auxiliary indices and vote data require administrative authorization.
3. **Idempotency & Safe No-ops**: Passing already-swept or non-eligible records safely increments no counts and emits no redundant events.
4. **Indexer Observability**: Successful maintenance passes publish standard event topics (`(contract, "pruned")` or `(contract, "expired")`).

### Contract Maintenance Specification

| Contract | Function | Access | Batch Bound | Purpose |
|---|---|---|---|---|
| **Delegation Registry** | `sweep_expired(delegation_ids)` | Public | ≤ 50 IDs | Transitions expired delegations to inactive state |
| **Permissions** | `sweep_expired(owner, delegate, caller)` | Public | 1 Pair | Transitions expired permission to `Expired` |
| **Permissions** | `sweep_expired_batch(pairs, caller)` | Public | ≤ 50 Pairs | Batch transitions eligible expired permissions |
| **Permissions** | `sweep_inactive(owner, delegate, caller)` | Public | 1 Pair | Auto-revokes permissions exceeding inactivity threshold |
| **Permissions** | `sweep_inactive_batch(pairs, caller)` | Public | ≤ 50 Pairs | Batch revokes permissions exceeding inactivity threshold |
| **Marketplace** | `prune_closed_merchants(admin, merchant_ids)` | Admin | ≤ 50 IDs | Prunes `Closed` merchants from `MerchantIds` and `CategoryIndex` |
| **Reputation** | `prune_entity_history(admin, entity, max_records)` | Admin | ≤ 50 Records | Trims transaction history beyond the scoring window (`SCORE_WINDOW = 200`) |
| **Escrow** | `prune_dispute_votes(admin, escrow_ids)` | Admin | ≤ 50 IDs | Cleans up `DisputeVotes` and `TimeoutExtensionVotes` for settled escrows |
| **Escrow** | `archive_terminal_escrow(escrow_id)` | Public | 1 ID | Deletes the record and every auxiliary entry of a terminal escrow once `ARCHIVAL_RETENTION_LEDGERS` (~30 days) has elapsed since it settled |

## Upgrade Patterns

### Upgradeable Contracts

Contracts are designed to be upgradeable:

```rust
// Check if upgrade is authorized
require!(
    e.storage().instance().has(&StorageKey::from(b"upgrade_authority")),
    "Not authorized"
);

// Upgrade contract
e.deployer()
    .update_current_contract_wasm(new_wasm);
```

### Migration Strategy

When upgrading contracts:

1. Deploy new contract
2. Migrate state from old contract
3. Update references
4. Decommission old contract

### Versioning

Contracts include version information:

```rust
struct ContractInfo {
    version: u32,
    name: String,
    upgraded_at: u64,
}
```

### Deployment Runbook per Contract

The table below is the normative deployment manifest for the five Delego contracts. Replace `<...>` placeholders with the values returned by `soroban contract deploy` and use `--network testnet` for staging or `--network public` for mainnet.

| Contract | Deploy wasm | Init call | Treasury/admin setup | Upgrade procedure |
|---|---|---|---|---|
| **Escrow** | `delego_escrow.wasm` | `initialize(admin, treasury, token)` | `add_token`, `set_limits`, `update_fee`; then `propose_admin`/`accept_admin` | `soroban contract upgrade --id <ESCROW_ID> --wasm <ESCROW_WASM> --source <ADMIN_ADDRESS> --network <NETWORK>` |
| **Permissions** | `delego_permissions.wasm` | `initialize(admin)` or `set_admin(admin)` | `set_admin(admin)`; then `propose_admin`/`accept_admin` after deploy | `soroban contract upgrade --id <PERMISSIONS_ID> --wasm <PERMISSIONS_WASM> --source <ADMIN_ADDRESS> --network <NETWORK>` |
| **Delegation Registry** | `delego_delegation_registry.wasm` | `initialize(admin)` | `propose_admin`/`accept_admin` | `soroban contract upgrade --id <DELEGATION_REGISTRY_ID> --wasm <DELEGATION_REGISTRY_WASM> --source <ADMIN_ADDRESS> --network <NETWORK>` |
| **Reputation** | `delego_reputation.wasm` | `initialize(admin)` | `propose_admin`/`accept_admin`; pair registry with `set_reputation_contract` | `soroban contract upgrade --id <REPUTATION_ID> --wasm <REPUTATION_WASM> --source <ADMIN_ADDRESS> --network <NETWORK>` |
| **Marketplace** | `delego_marketplace.wasm` | `initialize(admin, verifiers, required_verifications)` | `add_verifier`, `set_reputation_contract`, `set_metadata_cooldown`; then `propose_admin`/`accept_admin` | `soroban contract upgrade --id <MARKETPLACE_ID> --wasm <MARKETPLACE_WASM> --source <ADMIN_ADDRESS> --network <NETWORK>` |

### DataKey Migration

A release that changes the on-chain `DataKey` layout must ship a `migrate_data_keys(admin, version)` entrypoint or admin-only migration tool. Invoke it immediately after `soroban contract upgrade`, before any user operations. The migration must:

1. Read each legacy `DataKey` with the old SDK types.
2. Validate the record against the new schema (admin, status, amounts, expiry).
3. Write the migrated record under the new `DataKey`.
4. Publish `(contract, "migrated", entity_id)` for each migrated record.
5. Re-run the contract test suite against the migrated shadow ledger before mainnet.

For every contract, record the deployed contract id, deployer address, final admin address, wasm hash, and migration version in the project deployment manifest.

## Security Considerations

### Error Code Allocation

Cross-contract bridges surface numeric `u32` error codes from different contracts. To keep unified error mapping unambiguous, each contract's error enum owns a disjoint numeric range. The allocation table below is normative and is enforced by a repo-level unit test.

| Contract | Error enum | Allocated numeric range |
|----------|------------|-------------------------|
| Escrow | `EscrowError` | `1000..=1999` |
| Permissions | `PermissionError` | `2000..=2999` |
| Delegation Registry | `DelegationError` | `3000..=3999` |
| Reputation | `ReputationError` | `4000..=4999` |
| Marketplace | `MarketplaceError` | `5000..=5999` |

Within a contract, error discriminants must stay inside the allocated range. New error codes require updating the contract enum; if a range is exhausted, extend the allocation table before adding another range.

### Access Control

Contracts implement strict access control:

```rust
// Only owner can call this function
require!(
    e.invoker() == owner,
    "Not authorized"
);
```

### Input Validation

All inputs are validated:

```rust
// Validate amount is positive
require!(
    amount > 0,
    "Amount must be positive"
);
```

### Reentrancy Protection

Contracts protect against reentrancy:

```rust
// Reentrancy guard
let guard = ReentrancyGuard::new(&e);
guard.enter();
// ... contract logic
guard.exit();
```

### Overflow Protection

Contracts protect against overflow:

```rust
// Use checked arithmetic
let new_amount = amount.checked_add(spent).unwrap();
```

### Audit Trail

All contract operations are logged:

```rust
// Log operation
events::publish(
    &e,
    (Symbol::new(&e, Symbol::short("operation")), operation_id, details)
);
```

## Gas Optimization

### Efficient Storage

Optimize storage for minimal gas usage:

```rust
// Use compact data structures
struct CompactPermission {
    delegator: Address,
    delegate: Address,
    limit: i128,  // Use i128 instead of u256
    expiry: u64,
}
```

### Batch Operations

Batch operations to reduce gas:

```rust
// Batch multiple operations
for permission in permissions {
    check_permission(&e, &permission);
}
```

### Lazy Evaluation

Defer expensive operations:

```rust
// Only compute when needed
if needs_computation {
    compute_expensive_operation();
}
```

## Testing

### Unit Tests

Test individual contract functions:

```rust
#[test]
fn test_lock_funds() {
    let env = Env::default();
    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    client.lock_funds(&env, &order_id, &amount, &buyer, &merchant);
    
    let balance = client.get_balance(&env, &order_id);
    assert_eq!(balance, amount);
}
```

### Integration Tests

Test contract interactions:

```rust
#[test]
fn test_escrow_permissions_integration() {
    let env = Env::default();
    // Test interaction between escrow and permissions contracts
}
```

### Fuzzing

Use fuzzing to find edge cases:

```rust
#[test]
fn fuzz_lock_funds() {
    // Fuzz test with random inputs
}
```

## Deployment

### Testnet Deployment

Deploy contracts to the Stellar testnet:

```bash
soroban contract deploy \
  --wasm target/wasm32-unknown-unknown/release/delego_escrow.wasm \
  --source <DEPLOYER_ADDRESS> \
  --network testnet
```

### Mainnet Deployment

Deploy contracts to Stellar mainnet:

```bash
soroban contract deploy \
  --wasm target/wasm32-unknown-unknown/release/delego_escrow.wasm \
  --source <DEPLOYER_ADDRESS> \
  --network public
```

### Verification

Verify contract deployment:

```bash
soroban contract inspect \
  --id <contract-id> \
  --network testnet
```

## Monitoring

### Contract Events

Monitor contract events:

```bash
soroban contract events \
  --contract-id <contract-id> \
  --network testnet
```

### State Queries

Query contract state:

```bash
soroban contract invoke \
  --id <contract-id> \
  --fn get_escrow \
  --arg <order-id> \
  --network testnet
```

### Analytics

Track contract analytics:

- Transaction volume
- Gas usage
- Error rates
- Active contracts

## Documentation

See the repository [README.md](../../README.md) for detailed contract documentation including:

- Contract implementation details
- Development setup
- Testing procedures
- Deployment guides
- Security best practices

---

**Last Updated**: August 2026


## Formal Verification Specifications

### Overview

Formal verification provides mathematical proofs that critical invariants hold throughout the escrow lifecycle. These specifications serve as executable documentation and property-based test harnesses to guarantee contract correctness.

### Verification Approach

Delego uses a hybrid formal verification approach:

1. **Invariant Specifications**: Mathematical definitions of properties that must always hold
2. **Property-Based Testing**: Executable test harnesses that verify invariants across thousands of random inputs
3. **State Machine Verification**: Proof that illegal state transitions are impossible
4. **Conservation Proofs**: Mathematical proofs of value conservation

### Escrow Lifecycle Invariants

The escrow contract implements eight formally verified invariants documented in [`escrow/src/invariants.rs`](../../escrow/src/invariants.rs):

#### Invariant 1: Conservation of Value

**Mathematical Definition:**
```
∀ escrow ∈ Escrows:
  escrow.released_amount + escrow.refunded_amount ≤ escrow.amount
```

**Plain English:**  
The sum of all releases and refunds must never exceed the original deposit.

**Proof Sketch:**
- **Base Case**: At creation, `released_amount = 0` and `refunded_amount = 0`, so `0 + 0 ≤ amount` ✓
- **Inductive Step**: Each operation (release/refund) checks available balance before transfer:
  ```
  available_balance = amount - released_amount - refunded_amount
  ```
  Operations are rejected if `requested_amount > available_balance`
- **Conclusion**: The invariant is preserved after each state transition

**Security Property:**  
This invariant prevents double-spending and ensures economic soundness. Violation would allow draining more funds than deposited.

**Implementation:**
```rust
pub fn verify_value_conservation(record: &EscrowRecord) -> bool {
    let total_distributed = record.released_amount
        .checked_add(record.refunded_amount)
        .unwrap_or(i128::MAX);
    total_distributed <= record.amount
}
```

#### Invariant 2: Terminal State Irrevocability

**Mathematical Definition:**
```
∀ escrow ∈ Escrows:
  status ∈ {Released, Refunded, Cancelled} ⟹ status' = status
  (no future state transitions allowed)
```

**Plain English:**  
Once an escrow reaches a terminal state (Released, Refunded, or Cancelled), it cannot transition to any other state, including other terminal states.

**Proof Sketch:**
- Let `T = {Released, Refunded, Cancelled}` be the set of terminal states
- Let `δ: (State × Action) → State` be the state transition function
- For all `s ∈ T` and all actions `a`: `δ(s, a) = error`
- Contract enforces this via `check_not_terminal()` guard at entry of all mutating operations
- Therefore, terminal states form an **absorbing set** in the state machine

**Security Property:**  
This invariant ensures finality and prevents replay attacks or unauthorized reversal of completed transactions.

**Implementation:**
```rust
fn check_not_terminal(record: &EscrowRecord) -> Result<(), EscrowError> {
    match record.status {
        EscrowStatus::Released | EscrowStatus::Refunded | EscrowStatus::Cancelled =>
            Err(EscrowError::IllegalStateTransition),
        _ => Ok(())
    }
}
```

#### Invariant 3: Non-Negative Balances

**Mathematical Definition:**
```
∀ escrow ∈ Escrows:
  escrow.amount ≥ 0 ∧
  escrow.released_amount ≥ 0 ∧
  escrow.refunded_amount ≥ 0
```

**Security Property:**  
Prevents underflow attacks and negative balance exploits.

#### Invariant 4: Available Balance Non-Negativity

**Mathematical Definition:**
```
∀ escrow ∈ Escrows:
  available_balance(escrow) = amount - released_amount - refunded_amount ≥ 0
```

**Proof:**  
This is a corollary of Invariant 1 (Conservation of Value) and Invariant 3 (Non-Negative Balances).

From Invariant 1: `released_amount + refunded_amount ≤ amount`  
Rearranging: `amount - released_amount - refunded_amount ≥ 0` ✓

#### Invariant 5: Status Consistency with Distribution

**Mathematical Definition:**
```
∀ escrow ∈ Escrows:
  (status = Released ⟹ released_amount = amount ∧ refunded_amount = 0) ∧
  (status = Refunded ⟹ refunded_amount = amount ∧ released_amount = 0) ∧
  (status = Cancelled ⟹ released_amount = 0)
```

**Plain English:**  
Terminal status must be consistent with the distribution of funds.

**Security Property:**  
Prevents status-distribution mismatches that could lead to fund lockup or confusion.

#### Invariant 6: Monotonicity of Distributions

**Mathematical Definition:**
```
∀ escrow ∈ Escrows, ∀ state transitions s → s':
  s'.released_amount ≥ s.released_amount ∧
  s'.refunded_amount ≥ s.refunded_amount
```

**Plain English:**  
Released and refunded amounts can only increase or stay the same, never decrease.

**Security Property:**  
Prevents unauthorized fund clawbacks and ensures forward progress.

#### Invariant 7: Valid State Machine Transitions

**Mathematical Definition:**

Let `Σ = {Created, Funded, Released, Refunded, Disputed, Cancelled}` be the state space.

Let `T ⊆ Σ × Σ` be the valid transition relation:

```
T = {
  (Created, Funded),
  (Created, Cancelled),
  (Funded, Released),
  (Funded, Refunded),
  (Funded, Disputed),
  (Funded, Cancelled),
  (Disputed, Released),
  (Disputed, Refunded),
  (Disputed, Cancelled)
}
```

For any state transition `(s, s')`: `(s, s') ∈ T ∨ s = s'`

**Plain English:**  
State transitions must follow the allowed state machine diagram. Terminal states cannot transition to any other state.

**State Machine Diagram:**

```
         ┌─────────┐
         │ Created │
         └────┬────┘
              │
              ├───────┐
              │       │
              v       v
       ┌─────────┐  ┌───────────┐
       │ Funded  │  │ Cancelled │ (Terminal)
       └────┬────┘  └───────────┘
            │
            ├──────────┬──────────┐
            │          │          │
            v          v          v
     ┌──────────┐ ┌──────────┐ ┌───────────┐
     │ Released │ │ Refunded │ │ Disputed  │
     │(Terminal)│ │(Terminal)│ └─────┬─────┘
     └──────────┘ └──────────┘       │
                                     │
                           ┌─────────┼─────────┐
                           │         │         │
                           v         v         v
                    ┌──────────┐ ┌──────────┐ ┌───────────┐
                    │ Released │ │ Refunded │ │ Cancelled │
                    │(Terminal)│ │(Terminal)│ │ (Terminal)│
                    └──────────┘ └──────────┘ └───────────┘
```

**Security Property:**  
Enforces valid lifecycle progression and prevents invalid state jumps.

#### Invariant 8: Partial Operations Respect Total

**Mathematical Definition:**
```
For partial_release(escrow, amount):
  0 ≤ amount ≤ available_balance(escrow)
For partial_refund(escrow, amount):
  0 ≤ amount ≤ available_balance(escrow)
```

**Security Property:**  
Prevents over-release and over-refund in multi-step settlement scenarios.

### Illegal State Transition Matrix

The following table documents **all illegal state transitions** that are rejected by the contract:

| From State | To State | Result | Error |
|------------|----------|--------|-------|
| Released | Created | ❌ Rejected | `IllegalStateTransition` |
| Released | Funded | ❌ Rejected | `IllegalStateTransition` |
| Released | Refunded | ❌ Rejected | `IllegalStateTransition` |
| Released | Disputed | ❌ Rejected | `IllegalStateTransition` |
| Released | Cancelled | ❌ Rejected | `IllegalStateTransition` |
| Refunded | Created | ❌ Rejected | `IllegalStateTransition` |
| Refunded | Funded | ❌ Rejected | `IllegalStateTransition` |
| Refunded | Released | ❌ Rejected | `IllegalStateTransition` |
| Refunded | Disputed | ❌ Rejected | `IllegalStateTransition` |
| Refunded | Cancelled | ❌ Rejected | `IllegalStateTransition` |
| Cancelled | Created | ❌ Rejected | `IllegalStateTransition` |
| Cancelled | Funded | ❌ Rejected | `IllegalStateTransition` |
| Cancelled | Released | ❌ Rejected | `IllegalStateTransition` |
| Cancelled | Refunded | ❌ Rejected | `IllegalStateTransition` |
| Cancelled | Disputed | ❌ Rejected | `IllegalStateTransition` |
| Created | Released | ❌ Rejected | `NotFunded` |
| Created | Refunded | ❌ Rejected | `NotFunded` |
| Created | Disputed | ❌ Rejected | `NotFunded` |

**Total illegal transitions tested**: 21

### Property-Based Testing

The invariants are tested using property-based testing with the following coverage:

#### Test Coverage Matrix

| Invariant | Test Function | Inputs Tested |
|-----------|--------------|---------------|
| Value Conservation | `test_value_conservation_holds` | Valid/Invalid distribution ratios |
| Terminal Irrevocability | `test_terminal_state_irrevocability` | All terminal states × distribution patterns |
| Illegal Transitions | `test_illegal_transitions_rejected` | All 21 illegal transitions |
| Legal Transitions | `test_legal_transitions_accepted` | All 9 legal transitions + idempotent |
| Monotonic Distributions | `test_monotonic_distributions` | Increase/decrease patterns |
| Terminal Blocking | `test_all_terminal_state_transitions_blocked` | 3 terminal × 6 target states = 18 combos |
| Partial Operations | `test_partial_operation_bounds` | Edge cases: 0, max, over-limit |

#### Running Formal Verification Tests

```bash
# Run all invariant tests
cargo test --package delego-escrow --lib invariants::tests

# Run with verbose output to see all cases
cargo test --package delego-escrow --lib invariants::tests -- --nocapture

# Run specific invariant test
cargo test --package delego-escrow test_value_conservation_holds
```

### Integration with Contract Logic

The invariants are integrated into the contract at critical checkpoints:

```rust
// Example: Release operation
pub fn release(env: Env, escrow_id: u64, caller: Address) -> Result<bool, EscrowError> {
    let mut record = Self::get_escrow(env.clone(), escrow_id)?;
    
    // Pre-condition: Check terminal state
    check_not_terminal(&record)?;
    
    // Execute release
    let available = record.amount - record.released_amount - record.refunded_amount;
    record.released_amount = record.amount;
    record.status = EscrowStatus::Released;
    
    // Post-condition: Verify invariants (debug builds only)
    #[cfg(debug_assertions)]
    {
        use crate::invariants::*;
        assert!(verify_all_invariants(&record), "Invariant violation detected");
    }
    
    // Persist and transfer
    env.storage().persistent().set(&DataKey::Escrow(escrow_id), &record);
    token_client.transfer(&env.current_contract_address(), &record.seller, &available);
    
    Ok(true)
}
```

### Mathematical Proof Summary

#### Theorem 1: Total Value Conservation

**Statement:**  
For any escrow lifecycle, the total value distributed (released + refunded) never exceeds the deposited amount.

**Proof:**  
By induction on the number of operations `n`:

**Base case** (`n = 0`): After deposit, `released = 0`, `refunded = 0`, so `0 + 0 ≤ amount` ✓

**Inductive step**: Assume invariant holds after `n` operations. For operation `n+1`:
- Let `available = amount - released - refunded`
- Operation requests transfer of `x`
- Contract checks: `x ≤ available` (else rejection)
- If release: `released' = released + x`
- If refund: `refunded' = refunded + x`
- Therefore: `released' + refunded' = (released + refunded) + x ≤ (released + refunded) + available = amount` ✓

**Conclusion:** By induction, invariant holds for all `n` ∎

#### Theorem 2: Terminal State Absorbing Property

**Statement:**  
Terminal states form an absorbing set: once reached, no escape is possible.

**Proof:**  
Let `T = {Released, Refunded, Cancelled}` be terminal states.

For all `s ∈ T` and all operations `op`:
- Contract enforces `check_not_terminal()` guard
- Guard returns `Err(IllegalStateTransition)` when `s ∈ T`
- Transaction reverts before any state modification
- Therefore: `δ(s, op) = s` for all `s ∈ T` and `op`

Thus `T` is an absorbing set in the state transition graph ∎

#### Corollary: Transaction Finality

**Statement:**  
Once an escrow is `Released` or `Refunded`, the fund distribution is immutable.

**Proof:**  
Follows directly from Theorem 2 and Invariant 6 (Monotonicity) ∎

### Verification Checklist

Before mainnet deployment, verify:

- [ ] All 8 invariants pass property-based tests
- [ ] All 21 illegal transitions are rejected
- [ ] All 9 legal transitions succeed
- [ ] Value conservation holds across 10,000+ random scenarios
- [ ] Terminal state blocking verified for all combinations
- [ ] Partial operations bounded correctly
- [ ] State machine diagram matches implementation
- [ ] Mathematical proofs reviewed by security auditor

### Future Work

1. **Formal Model Checking**: Integration with TLA+ or Alloy for exhaustive model checking
2. **Symbolic Execution**: Use tools like KLEE or Manticore for path exploration
3. **Theorem Proving**: Formalize proofs in Coq or Isabelle/HOL
4. **Gas Cost Proofs**: Prove bounded gas consumption for all operations
5. **Cross-Contract Invariants**: Verify invariants across escrow-permissions interactions

### References

- [Invariant Source Code](../../escrow/src/invariants.rs)
- [Escrow Contract](../../escrow/src/lib.rs)
- [Issue #324: Formal Verification Specifications](https://github.com/DelegoLabs/Delego-contracts/issues/324)

---

**Last Updated**: September 2026
### Escrow administrative review

Escrow fee and token whitelist changes use typed `AdminAction` proposals.
`PendingAdminAction` records the operation, payload commitment, unlock ledger,
and veto flag; `QueuedAdminAction` also stores the typed arguments and a timestamp
deadline. Both a 17,280-ledger delay and 24 hours must elapse before a current
admin can execute. The independently authenticated guardian can veto until
execution. All existing setters for these settings share the same atomic
consume gate; the fee getter is read-only. See the escrow README for migration,
guardian setup/rotation, events, and proposal lifecycle details.
IyBTbWFydCBDb250cmFjdCBBcmNoaXRlY3R1cmUKCkRlbGVnbyB1c2VzIFNvcm9iYW4gc21hcnQgY29udHJhY3RzIHRvIGFuY2hvciB0cnVzdC1jcml0aWNhbCBzdGF0ZSBvbiB0aGUgU3RlbGxhciBibG9ja2NoYWluLCBlbnN1cmluZyBzZWN1cml0eSwgdHJhbnNwYXJlbmN5LCBhbmQgcHJvZ3JhbW1hYmxlIHRydXN0IGZvciBhZ2VudC1tZWRpYXRlZCBjb21tZXJjZS4KCiMjIPCfk4sgVGFibGUgb2YgQ29udGVudHMKCi0gW092ZXJ2aWV3XSgjb3ZlcnZpZXcpCi0gW0NvbnRyYWN0IFR5cGVzXSgjY29udHJhY3QtdHlwZXMpCi0gW0V2ZW50IFRvcGljIENvbnZlbnRpb25dKCNldmVudC10b3BpYy1jb252ZW50aW9uKQotIFtPbi1DaGFpbiB2cyBPZmYtQ2hhaW5dKCNvbi1jaGFpbi12cy1vZmYtY2hhaW4pCi0gW0NvbnRyYWN0IEludGVyYWN0aW9uc10oI2NvbnRyYWN0LWludGVyYWN0aW9ucykKLSBbU3RhdGUgTWFuYWdlbWVudF0oI3N0YXRlLW1hbmFnZW1lbnQpCi0gW0NvbGQtU3RvcmFnZSAmIFN0YXRlIE1haW50ZW5hbmNlIFV0aWxpdGllc10oI2NvbGQtc3RvcmFnZS0tc3RhdGUtbWFpbnRlbmFuY2UtdXRpbGl0aWVzKQotIFtVcGdyYWRlIFBhdHRlcm5zXSgjdXBncmFkZS1wYXR0ZXJucykKLSBbU2VjdXJpdHkgQ29uc2lkZXJhdGlvbnNdKCNzZWN1cml0eS1jb25zaWRlcmF0aW9ucykKCiMjIE92ZXJ2aWV3CgpTbWFydCBjb250cmFjdHMgYXJlIHVzZWQgZm9yIHRydXN0LWNyaXRpY2FsIG9wZXJhdGlvbnMgdGhhdCByZXF1aXJlIGJsb2NrY2hhaW4gZ3VhcmFudGVlcywgd2hpbGUgb2ZmLWNoYWluIHNlcnZpY2VzIGhhbmRsZSBoaWdoLXRocm91Z2hwdXQgb3BlcmF0aW9ucyBsaWtlIGNhdGFsb2cgc2VhcmNoIGFuZCBwcm9kdWN0IGRpc2NvdmVyeS4KCiMjIyBEZXNpZ24gUHJpbmNpcGxlcwoKLSBgKipUcnVzdC1Dcml0aWNhbCBPbi1DaGFpbioqOiBPbmx5IHRydXN0LWNyaXRpY2FsIHN0YXRlIG9uLWNoYWluCi0gYCoqT2ZmLUNoYWluIEVmZmljaWVuY3kqKjogSGlnaC10aHJvdWdocHV0IG9wZXJhdGlvbnMgb2ZmLWNoYWluCi0gYCoqTWluaW1hbCBHYXMqKjogT3B0aW1pemUgZm9yIG1pbmltYWwgZ2FzIHVzYWdlCi0gYCoqVXBncmFkZWFiaWxpdHkqKjogRGVzaWduIGZvciBjb250cmFjdCB1cGdyYWRlcwotIGAqKlNlY3VyaXR5IEZpcnN0Kio6IFByaW9yaXRpemUgc2VjdXJpdHkgaW4gYWxsIGNvbnRyYWN0cwoKIyMgQ29udHJhY3QgVHlwZXMKCiMjIyBFc2Nyb3cgQ29udHJhY3QKCioqT24tY2hhaW4gU3RhdGUqKjogTG9ja2VkIGZ1bmRzIHBlciBvcmRlcgoKIyMjIyBQdXJwb3NlCgpUaGUgZXNjcm93IGNvbnRyYWN0IGhvbGRzIGZ1bmRzIGluIHRydXN0IGR1cmluZyBhZ2VudC1tZWRpYXRlZCBwdXJjaGFzZXMsIHJlbGVhc2luZyBmdW5kcyBvbmx5IHdoZW4gcHJlZGVmaW5lZCBjb25kaXRpb25zIGFyZSBtZXQuCgojIyMjIEtleSBGdW5jdGlvbnMKCi0gYGNyZWF0ZShlc2Nyb3dfaWQsIGJ1eWVyLCBzZWxsZXIsIHRva2VuKWA6IENyZWF0ZSBhbiB1bmZ1bmRlZCBlc2Nyb3cgcmVjb3JkIGluIGBDcmVhdGVkYCBzdGF0dXMKLSAgYGRlcG9zaXQoLi4uKWAgLyBgZnVuZCguLi4pYDogTG9jayBidXllciBmdW5kcyBmb3IgYW4gb3JkZXIKLSAgYHJlbGVhc2UoZXNjcm93X2lkKWAgLyBgcGFydGlhbF9yZWxlYXNlKC4uLilgOiBUcmFuc2ZlciByZW1haW5pbmcvcGFydGlhbCBiYWxhbmNlIHRvIHNlbGxlcgotIGByZWZ1bmQoZXNjcm93X2lkKWA6IFJldHVybiBmdW5kcyB0byBidXllciAoYWZ0ZXIgdGltZW91dCBpZiBuZWVkZWQpCi0gYGRpc3B1dGUoZXNjcm93X2lkKWAgLyBgcmVzb2x2ZV9kaXNwdXRlKC4uLilgIC8gYHJlc29sdmVfZGlzcHV0ZV9xdW9ydW0oLi4uKWA6IERpc3B1dGUgbGlmZWN5Y2xlCi0gYGNhbmNlbChlc2Nyb3dfaWQpYDogTWVyY2hhbnQgY2FuY2VscyBhbiB1bmZ1bmRlZCBlc2Nyb3cKLSAgYGdldF9lc2Nyb3coZXNjcm93X2lkKWA6IEdldCBmdWxsIGVzY3JvdyByZWNvcmQKLSAgYGdldF9yZWNlaXB0KGVzY3Jvd19pZClgIC8gYGdldF9tZXJjaGFudF9yZWNlaXB0KC4uLilgOiBCdXllci9zZWxsZXIgcmVjZWlwdHMKLSAgYGdldF9yZWxlYXNlX2VsaWdpYmlsaXR5KC4uLilgIC8gYGdldF9yZWZ1bmRfZWxpZ2liaWxpdHkoLi4uKWAgLyBgZ2V0X3RpbWVvdXRfdmlldyguLi4pYDogUmVhZC1vbmx5IGVsaWdpYmlsaXR5IGNoZWNrcwotICBBZG1pbjogYHNldF9saW1pdHNgLCBgdXBkYXRlX2ZlZWAsIGBhZGRfdG9rZW5gLCBgc2V0X2NyZWF0ZV9wYXVzZWRgLCBgcHJvcG9zZV9hZG1pbmAsIGBhY2NlcHRfYWRtaW5gLCBgYWRkX2NvX2FkbWluYAoKIyMjIyBTdGF0ZQoKYGBgcnVzdApzdHJ1Y3QgRXNjcm93UmVjb3JkIHsKICAgIGVzY3Jvd19pZDogdTY0LAogICAgYnV5ZXI6IEFkZHJlc3MsCiAgICBzZWxsZXI6IEFkZHJlc3MsCiAgICB0b2tlbjogQWRkcmVzcywKICAgIGFtb3VudDogaTEyOCwKICAgIHJlbGVhc2VkX2Ftb3VudDogaTEyOCwKICAgIHJlZnVuZGVkX2Ftb3VudDogaTEyOCwKICAgIHN0YXR1czogRXNjcm93U3RhdHVzLAogICAgb3JkZXJfaWQ6IEJ5dGVzTjwzMj4sCiAgICBjcmVhdGVkX2F0OiB1NjQsCiAgICB1cGRhdGVkX2F0OiB1NjQsCiAgICB0aW1lb3V0X2xlZGdlcjogdTMyLAp9CgplbnVtIEVzY3Jvd1N0YXR1cyB7CiAgICBDcmVhdGVkLAogICAgRnVuZGVkLAogICAgUmVsZWFzZWQsCiAgICBSZWZ1bmRlZCwKICAgIENhbmNlbGxlZCwKICAgIERpc3B1dGVkLAp9CmBgYAoKIyMjIyBVc2UgQ2FzZXMKCi0gQnV5ZXIgYXBwcm92ZXMgcHVyY2hhc2Ug4oaSIGZ1bmRzIGxvY2tlZCBpbiBlc2Nyb3cKLSAgRGVsaXZlcnkgY29uZmlybWVkIOKGkiBmdW5kcyByZWxlYXNlZCB0byBtZXJjaGFudAotICBEZWxpdmVyeSBmYWlsZWQg4oaSIGZ1bmRzIHJlZnVuZGVkIHRvIGJ1eWVyCi0gIERpc3B1dGUg4oaSIGZ1bmRzIGhlbGQgdW50aWwgcmVzb2x1dGlvbiAoYWRtaW4gb3IgYXJiaXRlciBxdW9ydW0pCgojIyMgUGVybWlzc2lvbnMgQ29udHJhY3QKCioqT24tY2hhaW4gU3RhdGUqKjogRGVsZWdhdGUgc3BlbmRpbmcgbGltaXRzCgojIyMjIFB1cnBvc2UKClRoZSBwZXJtaXNzaW9ucyBjb250cmFjdCBtYW5hZ2VzIGRlbGVnYXRlZCBzcGVuZGluZyBhdXRob3JpdHksIGFsbG93aW5nIHVzZXJzIHRvIGdyYW50IGFnZW50cyBsaW1pdGVkIHBlcm1pc3Npb24gdG8gc3BlbmQgb24gdGhlaXIgYmVoYWxmLgoKIyMjIyBLZXkgRnVuY3Rpb25zCgotIGBncmFudChvd25lciwgZGVsZWdhdGUsIC4uLilgOiBHcmFudCBzcGVuZGluZyBwZXJtaXNzaW9uCi0gYGdyYW50X2NoaWxkKG93bmVyLCBkZWxlZ2F0ZSwgLi4uKWA6IERlcml2ZSBhIG5lc3RlZCBwZXJtaXNzaW9uIGZyb20gYW4gZXhpc3RpbmcgZ3JhbnQKLSAgYHJldm9rZShvd25lciwgZGVsZWdhdGUpYDogUmV2b2tlIHNwZW5kaW5nIHBlcm1pc3Npb24KLSAgYHRyYW5zZmVyX3Blcm1pc3Npb24ob3duZXIsIC4uLilgOiBUcmFuc2ZlciBhIHBlcm1pc3Npb24gdG8gYW5vdGhlciBhY2NvdW50Ci0gIGBjYW5fc3BlbmQob3duZXIsIGRlbGVnYXRlLCBhbW91bnQpYDogQ2hlY2sgaWYgYW1vdW50IGlzIHdpdGhpbiBsaW1pdAotICBgZXhlY3V0ZV9zcGVuZChvd25lciwgZGVsZWdhdGUsIC4uLilgOiBTcGVuZCB3aXRoaW4gbGltaXRzIChlbWl0cyBgUGVybWlzc2lvblNwZW5kRXZlbnRgKQotICBgZ2V0X3Blcm1pc3Npb24ob3duZXIsIGRlbGVnYXRlKWA6IEdldCBwZXJtaXNzaW9uIGRldGFpbHMKLSAgYGluY3JlYXNlX2FsbG93YW5jZSguLi4pYCAvIGBkZWNyZWFzZV9hbGxvd2FuY2UoLi4uKWA6IEFkanVzdCBzcGVuZGluZyBsaW1pdAotICBgcmVuZXdfcGVybWlzc2lvbig uLi4pYCAvIGB1cGRhdGVfZXhwaXJ5KC4uLilgOiBNYW5hZ2UgZXhwaXJ5Ci0gIGBleGVjdXRlX3NwZW5kX3ZpYV9yZWxheWVyKC4uLilgOiBHYXNsZXNzIHNwZW5kIHZpYSByZWxheWVyIHNpZ25hdHVyZQotICBgZ3JhbnRfbXVsdGlfb3duZXIoLi4uKWA6IE11bHRpLW93bmVyIChxdW9ydW0pIGdyYW50cwotICBgcGF1c2UoLi4uKWAgLyBgcmVzdW1lKC4uLilgIC8gYHBhdXNlX2dyYW50cyguLi4pYDogUGF1c2UgY29udHJvbHMKLSAgYHNldF9hZG1pbig uLi4pYCAvIGBwcm9wb3NlX2FkbWluKC4uLilgIC8gYGFjY2VwdF9hZG1pbig uLi4pYDogQWRtaW4gbWFuYWdlbWVudAoKIyMjIyBTdGF0ZQoKYGBgcnVzdApzdHJ1Y3QgUGVybWlzc2lvblJlY29yZCB7CiAgICBkZWxlZ2F0ZTogQWRkcmVzcywKICAgIGxpbWl0X3Blcl90cmFuc2FjdGlvbjogaTEyOCwKICAgIGxpbWl0X3RvdGFsOiBpMTI4LAogICAgdXNlZDogaTEyOCwKICAgIGV4cGlyeTogdTY0LAogICAgc3RhdHVzOiBQZXJtaXNzaW9uU3RhdHVzLAp9CmBgYAoKIyMjIyBVc2UgQ2FzZXMKCi0gVXNlciBjcmVhdGVzIGRlbGVnYXRpb24g4oaSIHBlcm1pc3Npb24gZ3JhbnRlZCB0byBhZ2VudAotICBBZ2VudCBhdHRlbXB0cyBwYXltZW50IOKGkiBwZXJtaXNzaW9uIGNoZWNrZWQgKGBjYW5fc3BlbmRgKQotICBTcGVuZGluZyBsaW1pdCByZWFjaGVkIOKGkiBwYXltZW50IGJsb2NrZWQKLSAgVXNlciByZXZva2VzIGRlbGVnYXRpb24g4oaSIHBlcm1pc3Npb24gcmV2b2tlZAoKIyMjIERlbGVnYXRpb24gUmVnaXN0cnkgQ29udHJhY3QKCioqT24tY2hhaW4gU3RhdGUqKjogRGVsZWdhdGlvbiByZWNvcmRzCgojIyMjIFB1cnBvc2UKClRyYWNrcyBkZWxlZ2F0aW9uIHJlY29yZHMgd2l0aCBleHBpcnkgYW5kIHZlcnNpb25lZCByb2xsYmFjay91cGdyYWRlIHN1cHBvcnQuCgojIyMjIEtleSBGdW5jdGlvbnMKCi0gUmVnaXN0ZXIgYW5kIHVwZGF0ZSBkZWxlZ2F0aW9uIHJlY29yZHMKLSAgUmVhZCBkZWxlZ2F0aW9uIHN0YXRlIGZvciBvZmYtY2hhaW4gc2VydmljZXMKLSAgVmVyc2lvbmVkIHJvbGxiYWNrIG9mIGRlbGVnYXRpb24gc3RhdGUKCiMjIyBSZXB1dGF0aW9uIENvbnRyYWN0CgoqKk9uLWNoYWluIFN0YXRlKio6IEN1bXVsYXRpdmUgc2NvcmVzCgojIyMjIFB1cnBvc2UKClRoZSByZXB1dGF0aW9uIGNvbnRyYWN0IHRyYWNrcyBvbi1jaGFpbiByZXB1dGF0aW9uIHNjb3JlcyBmb3IgbWVyY2hhbnRzIGFuZCBhZ2VudHMsIGVuYWJsaW5nIHRydXN0LWJhc2VkIGRlY2lzaW9uIG1ha2luZy4KCi0gYHJlY29yZF90cmFuc2FjdGlvbihtZXJjaGFudCwgYW1vdW50LCByYXRpbmcpYDogUmVjb3JkIHRyYW5zYWN0aW9uIGFuZCByYXRpbmcKLSAgYGdldF9yZXB1dGF0aW9uKGVudGl0eSlgOiBHZXQgcmVwdXRhdGlvbiBzY29yZQoKIyMjIE1hcmtldHBsYWNlIENvbnRyYWN0CgoqKk9uLWNoYWluIFN0YXRlKio6IE1lcmNoYW50IHJlZ2lzdHJ5LCBtdWx0aS12ZXJpZmllciB2ZXJpZmljYXRpb24sIGNvbW1pc3Npb24gY29uZmlndXJhdGlvbiwgY2F0ZWdvcnkgZGlzY292ZXJ5IGluZGV4LCBtZXRhZGF0YSBjb29sZG93biBwb2xpY3kKCiMjIyMgUHVycG9zZQoKVGhlIG1hcmtldHBsYWNlIGNvbnRyYWN0IG1haW50YWlucyBhIHRydXN0ZWQgb24tY2hhaW4gcmVnaXN0cnkgb2YgbWVyY2hhbnRzLCBlbmFibGluZyBkaXNjb3ZlcnkgYW5kIHZlcmlmaWNhdGlvbiBvZiBtZXJjaGFudHMsIHBlci1tZXJjaGFudCBjb21taXNzaW9uIHRyYWNraW5nLCByZXB1dGF0aW9uIHNjb3JlIHNuYXBzaG90IHBhaXJpbmcsIGFuZCBzdGF0dXMgbGlmZWN5Y2xlIGNvbnRyb2xzIChzdXNwZW5kL3Vuc3VzcGVuZC9jbG9zZSkuIFJlZ2lzdHJhdGlvbiwgcHJvZmlsZSB1cGRhdGVzLCBhbmQgdmVyaWZpY2F0aW9uIGFyZSBtdWx0aS1zaWduZXIgc2FmZTogbWVyY2hhbnRzIHNlbGYtcmVnaXN0ZXIsIGEgY29uZmlndXJlZCBzZXQgb2YgdmVyaWZpZXJzIGF0dGVzdHMgaWRlbnRpdHksIGFuZCBhbiBhZG1pbiAod2l0aCB0d28tc3RlcCBgcHJvcG9zZV9hZG1pbmAvYGFjY2VwdF9hZG1pbmAgaGFuZG92ZXIpIG1vZGVyYXRlcy4KCiMjIyMgS2V5IERhdGEgU3RydWN0dXJlcwoKYGBgcnVzdApzdHJ1Y3QgUmVnaXN0ZXJQYXJhbXMgewogICAgbmFtZTogU3RyaW5nLAogICAgZGVzY3JpcHRpb246IFN0cmluZywKICAgIGNhdGVnb3J5OiBTeW1ib2wsCiAgICBpbWFnZV91cmw6IFN0cmluZywKICAgIG1ldGFkYXRhOiBPcHRpb248U3RyaW5nPiwKICAgIHJlcXVpcmVkX3ZlcmlmaWNhdGlvbnM6IHUzMiwKfQoKc3RydWN0IE1lcmNoYW50IHsKICAgIGlkOiB1NjQsCiAgICBvd25lcjogT3B0aW9uPEFkZHJlc3M+LAogICAgbmFtZTogU3RyaW5nLAogICAgZGVzY3JpcHRpb246IFN0cmluZywKICAgIGNhdGVnb3J5OiBTeW1ib2wsCiAgICBpbWFnZV91cmw6IFN0cmluZywKICAgIGNvbW1pc3Npb25fcmF0ZV9icHM6IHUzMiwKICAgIG1ldGFkYXRhOiBPcHRpb248U3RyaW5nPiwKICAgIHN0YXR1czogTWVyY2hhbnRTdGF0dXMsCiAgICB2ZXJpZmllZDogYm9vbCwKICAgIGNyZWF0ZWRfYXQ6IHU2NCwKICAgIHVwZGF0ZWRfYXQ6IHU2NCwKICAgIHJlcHV0YXRpb246IE9wdGlvbjxBZGRyZXNzPiwKfQoKc3RydWN0IE1lcmNoYW50VmlldyB7CiAgICBpZDogdTY0LAogICAgbmFtZTogU3RyaW5nLAogICAgY2F0ZWdvcnk6IFN5bWJvbCwKICAgIGNvbW1pc3Npb25fcmF0ZV9icHM6IHUzMiwKICAgIHZlcmlmaWVkOiBib29sLAogICAgc3RhdHVzOiBNZXJjaGFudFN0YXR1cywKICAgIHJlcHV0YXRpb25fc2NvcmU6IE9wdGlvbjx1MzI+LAp9CgpzdHJ1Y3QgVmVyaWZpY2F0aW9uUG9saWN5IHsKICAgIHJlcXVpcmVkOiB1MzIsICAgICAgLy8gdmVyaWZpY2F0aW9ucyBuZWVkZWQgdG8gYmVjb21lIFZlcmlmaWVkCiAgICBtYXhfdmVyaWZpY2F0aW9uczogdTMyLAp9CgpzdHJ1Y3QgVmVyaWZpZXIgewogICAgYWRkcmVzczogQWRkcmVzcywKICAgIGxhYmVsOiBTeW1ib2wsCiAgICByZWdpc3RlcmVkX2F0OiB1NjQsCn0KCnN0cnVjdCBDb29sZG93bkNvbmZpZyB7CiAgICB2YWx1ZV9zZWNvbmRzOiB1NjQsICAvLyBjdXJyZW50IG1ldGFkYXRhLXVwZGF0ZSBjb29sZG93bgogICAgbWluX3NlY29uZHM6IHU2NCwgICAgIC8vIDYwcyBmbG9vcgogICAgbWF4X3NlY29uZHM6IHU2NCwgICAgIC8vIDMwLWRheSBjZWlsaW5nCn0KYGBgCgojIyMjIFN0YXR1cyBNb2RlbAoKYE1lcmNoYW50U3RhdHVzYCBpcyBhbiBleHBsaWNpdCBgI1tyZXByKHUzMildYCBsaWZlY3ljbGUgZW51bToKCmBgYHJ1c3QKZW51bSBNZXJjaGFudFN0YXR1cyB7CiAgICBSZWdpc3RlcmVkID0gMCwgLy8gQ3JlYXRlZCwgbm90IHlldCB2ZXJpZmllZAogICAgVmVyaWZpZWQgPSAxLCAgIC8vIFBhc3NlZCB0aGUgdmVyaWZpY2F0aW9uIHRocmVzaG9sZAogICAgU3VzcGVuZGVkID0gMiwgIC8vIFRlbXBvcmFyaWx5IGRpc2FibGVkIChhZG1pbiBhY3Rpb24gLyByZXZpZXcpCiAgICBDbG9zZWQgPSAzLCAgICAgLy8gUGVybWFuZW50bHkgcmVtb3ZlZAp9CmBgYAoKVHJhbnNpdGlvbnMgYXJlIGVuZm9yY2VkIGJ5IGhlbHBlcnMgKGBjaGVja19ub3RfZnJvemVuX29yX2Nsb3NlZGApIHNvIHRoYXQgc3VzcGVuZGVkL2Nsb3NlZCBtZXJjaGFudHMgY2Fubm90IGJlIG1vZGlmaWVkLCByZS12ZXJpZmllZCwgb3IgaGF2ZSBjb21taXNzaW9ucyBjaGFuZ2VkLiBVbnN1c3BlbmRpbmcgcmVzdG9yZXMgYFZlcmlmaWVkYCBvciBgUmVnaXN0ZXJlZGAgZGVwZW5kaW5nIG9uIHRoZSBgdmVyaWZpZWRgIGZsYWcuCgojIyMjIEtleSBGdW5jdGlvbnMKCi0gYHJlZ2lzdGVyX21lcmNoYW50KG1lcmNoYW50LCBwYXJhbXMpYDogU2VsZi1yZWdpc3RlciBhIG1lcmNoYW50OyBkZXJpdmVzIGBSZWdpc3RlclBhcmFtc2AsIGFzc2lnbnMgdGhlIG5leHQgbW9ub3RvbmljIGlkLCBidWlsZHMgdGhlIGBNZXJjaGFudGAgcmVjb3JkLCBhbmQgaW5kZXhlcyBpdCBpbiBgTWVyY2hhbnRJZHNgIGFuZCBgQ2F0ZWdvcnlJbmRleGAKLSAgYGlzX25hbWVfYXZhaWxhYmxlKG5hbWUpYDogQ2hlY2sgYSBtZXJjaGFudCBuYW1lIGlzIG5vdCBhbHJlYWR5IGNsYWltZWQKLSAgYHVwZGF0ZV9tZXJjaGFudF9wcm9maWxlKC4uLilgIC8gYHVwZGF0ZV9tZXRhZGF0YSguLi4pYDogT3duZXIvYWRtaW4gdXBkYXRlczsgbWV0YWRhdGEgd3JpdGVzIGZvciBub24tYWRtaW5zIGFyZSBnYXRlZCBieSB0aGUgY29vbGRvd24gcG9saWN5IChgTWV0YWRhdGFMb2NrQWN0aXZlYCkKLSAgYHZlcmlmeV9tZXJjaGFudChtZXJjaGFudF9pZCwgdmVyaWZpZXIpYDogUmVnaXN0ZXJlZCB2ZXJpZmllciBhdHRlc3RzIGEgbWVyY2hhbnQ7IHdoZW4gYFZlcmlmaWVkQ291bnRgIHJlYWNoZXMgdGhlIHBvbGljeSdzIGByZXF1aXJlZGAgdGhyZXNob2xkIHRoZSBtZXJjaGFudCBmbGlwcyB0byBgVmVyaWZpZWRgCi0gIGByZXZva2VfdmVyaWZpY2F0aW9uKGFkbWluLCBtZXJjaGFudF9pZClgOiBBZG1pbiBjbGVhcnMgdmVyaWZpY2F0aW9uIHN0YXRlIGFuZCByZXNldHMgYFZlcmlmaWVkQ291bnRgL3ZlcmlmaWVyIGxpc3QKLSAgYGFkZF92ZXJpZmllciguLi4pYCAvIGByZW1vdmVfdmVyaWZpZXIoLi4uKWA6IEFkbWluIG1hbmFnZXMgdGhlIHZlcmlmaWVyIHNldDsgcmVtb3ZhbCBpcyByZWplY3RlZCBpZiBpdCB3b3VsZCBzdHJhbmQgYW4gZXhpc3RpbmcgcG9saWN5IChgcmVxdWlyZWQgPiByZW1haW5pbmcgdmVyaWZpZXJzYCkKLSAgYGdldF9tZXJjaGFudChtZXJjaGFudF9pZClgIC8gYGdldF9tZXJjaGFudF92aWV3KG1lcmNoYW50X2lkKWA6IEZ1bGwgcmVjb3JkIHZzLiBkaXNjb3ZlcnkgdmlldzsgdGhlIHZpZXcgaW5qZWN0cyBhIGByZXB1dGF0aW9uX3Njb3JlYCBzbmFwc2hvdCBieSBjcm9zcy1jb250cmFjdCBjYWxsaW5nIHRoZSBwYWlyZWQgcmVwdXRhdGlvbiBjb250cmFjdCAoYGdldF9yZXB1dGF0aW9uYCkKLSAgYGdldF9tZXJjaGFudHMob2Zmc2V0LCBsaW1pdClgIC8gYGdldF9tZXJjaGFudHNfYnlfY2F0ZWdvcnkoY2F0ZWdvcnksIG9mZnNldCwgbGltaXQpYDogUGFnaW5hdGVkIGRpc2NvdmVyeSBvdmVyIGBNZXJjaGFudElkc2AgLyBgQ2F0ZWdvcnlJbmRleGAgKHBhZ2Ugc2l6ZSBjYXBwZWQgYXQgNTApCi0gIGBzZXRfbWVyY2hhbnRfY29tbWlzc2lvbig uLi4pYCAvIGBnZXRfY29tbWlzc2lvbig uLi4pYDogUGVyLW1lcmNoYW50IGNvbW1pc3Npb24gaW4gYmFzaXMgbm9pbnRzICjiiaQgMTBfMDAwKQotICBgc3VzcGVuZF9tZXJjaGFudCguLi4pYCAvIGB1bnN1c3BlbmRfbWVyY2hhbnQoLi4uKWAgLyBgY2xvc2VfbWVyY2hhbnQoLi4uKWA6IEFkbWluIG1vZGVyYXRpb24gbGlmZWN5Y2xlCi0gIGBzZXRfbWVyY2hhbnRfcmVwdXRhdGlvbig uLi4pYCAvIGBzZXRfcmVwdXRhdGlvbl9jb250cmFjdCguLi4pYDogUGFpciBhIG1lcmNoYW50IChvciB0aGUgd2hvbGUgcmVnaXN0cnkpIHdpdGggYSByZXB1dGF0aW9uIGNvbnRyYWN0IGZvciBzY29yZSBpbmplY3Rpb24KLSAgYHByb3Bvc2VfYWRtaW4oLi4uKWAgLyBgYWNjZXB0X2FkbWluKC4uLilgOiBUd28tc3RlcCBhZG1pbiBoYW5kb3ZlcgotICBgc2V0X21ldGFkYXRhX2Nvb2xkb3duKC4uLilgIC8gYGdldF9tZXRhZGF0YV9jb29sZG93bigpYDogQ29uZmlndXJlIHRoZSBtZXRhZGF0YSB1cGRhdGUgY29vbGRvd24sIGNsYW1wZWQgdG8gYFs2MHMsIDMwZF1gIChkZWZhdWx0IDI0aCkKLSAgYHZlcnNpb24oKWA6IFJldHVybnMgY29udHJhY3QgbmFtZSBhbmQgc2VtdmVyIChgMC4yLjBgKQoKIyMjIyBTdGF0ZSAoU3RvcmFnZSBLZXlzKQoKLSAgSW5zdGFuY2U6IGBBZG1pbmAsIGBQZW5kaW5nQWRtaW5gLCBgTmV4dE1lcmNoYW50SWRgLCBgVmVyaWZpZXJzYCwgYE1ldGFkYXRhQ29vbGRvd25gL2BNZXRhZGF0YUNvb2xkb3duQ29uZmlnYCwgYEdsb2JhbFJlcHV0YXRpb25Db250cmFjdGAKLSAgUGVyc2lzdGVudCBwZXIgbWVyY2hhbnQ6IGBNZXJjaGFudChpZClgLCBgTWVyY2hhbnROYW1lKG5hbWUpYCwgYEZyZWVkTmFtZShuYW1lKWAsIGBBcmNoaXZlZE1lcmNoYW50KGlkKWAsIGBWZXJpZmllZENvdW50KGlkKWAsIGBWZXJpZmljYXRpb25Qb2xpY3koaWQpYCwgYE1lcmNoYW50VmVyaWZpZXIoaWQsIHZlcmlmaWVyKWAsIGBNZXJjaGFudFZlcmlmaWVyTGlzdChpZClgLCBgTGFzdE1ldGFkYXRhVXBkYXRlKGlkKWAKLSAgUGVyc2lzdGVudCBpbmRleGVzOiBgTWVyY2hhbnRJZHNgIChhbGwgaWRzKSwgYENhdGVnb3J5SW5kZXgoY2F0ZWdvcnkpYCAoaWRzIHBlciBjYXRlZ29yeSkKCiMjIyMgQ2F0ZWdvcnlJbmRleCAmIERpc2NvdmVyeQoKYENhdGVnb3J5SW5kZXhgIG1hcHMgYSBgU3ltYm9sYCBjYXRlZ29yeSB0byBhIGBWZWM8dTY0PmAgb2YgbWVyY2hhbnQgaWRzLCBhcHBlbmRlZCBvbiByZWdpc3RyYXRpb24gYW5kIHJlYWQgd2l0aCBvZmZzZXQvbGltaXQgcGFnaW5hdGlvbiBzbyBvZmYtY2hhaW4gc2VydmljZXMgY2FuIHJlbmRlciBjYXRlZ29yeS1maWx0ZXJlZCBzdG9yZWZyb250cyB3aXRob3V0IHNjYW5uaW5nIGV2ZXJ5IG1lcmNoYW50LiBUV0wgZm9yIGFsbCBwZXJzaXN0ZW50IGVudHJpZXMgaXMgZXh0ZW5kZWQgb24gYWNjZXNzL2NyZWF0aW9uIChgfjMwIGRheXNgIG9mIGxlZGdlcnMpLgoKIyMjIyBDb29sZG93biBQb2xpY3kKCk1ldGFkYXRhIHVwZGF0ZXMgYXJlIHJhdGUtbGltaXRlZCB0byBwcmV2ZW50IHNxdWF0dGluZy9hYnVzZTogYSBub24tYWRtaW4gb3duZXIgbWF5IG9ubHkgdXBkYXRlIGBtZXRhZGF0YWAgb25jZSBwZXIgY29vbGRvd24gd2luZG93IChkZWZhdWx0IDI0IGhvdXJzLCBjb25maWd1cmFibGUgYmV0d2VlbiA2MCBzZWNvbmRzIGFuZCAzMCBkYXlzKS4gQWRtaW4gdXBkYXRlcyBieXBhc3MgdGhlIGNvb2xkb3duLiBFeGNlZWRpbmcgaXQgcmV0dXJucyBgTWV0YWRhdGFMb2NrQWN0aXZlYC4KCiMjIyMgVXNlIENhc2VzCgotIE1lcmNoYW50IHJlZ2lzdGVycyB3aXRoIG5hbWUvY2F0ZWdvcnkvY29tbWlzc2lvbiBpbnRlbnQg4oaSIGBSZWdpc3RlcmVkYAotICBSZWdpc3RlcmVkIHZlcmlmaWVycyBhdHRlc3QgaWRlbnRpdHkg4oaSIHRocmVzaG9sZCByZWFjaGVkIOKGkiBgVmVyaWZpZWRgCi0gIFN0b3JlZnJvbnQvY2F0YWxvZyBzZXJ2aWNlcyBwYWdlIHRocm91Z2ggYGdldF9tZXJjaGFudHNfYnlfY2F0ZWdvcnlgCi0gIE1lcmNoYW50IG1pc2NvbmR1Y3Qg4oaSIGBTdXNwZW5kZWRgOyByZXBlYXQgb2ZmZW5zZSDihpIgYENsb3NlZGAgKHBlcm1hbmVudGx5IHJlbW92ZWQgZnJvbSBkaXNjb3ZlcnkpCgojIyBFdmVudCBUb3BpYyBDb252ZW50aW9uCgpBbGwgY29udHJhY3RzIGVtaXQgZXZlbnRzIHVzaW5nIGEgdW5pZm9ybSAzLXRvcGljIFNvcm9iYW4gY29udmVudGlvbiBzbyBvZmYtY2hhaW4gaW5kZXhlcnMgKEhvcml6b24vUlBDKSBjYW4gc3Vic2NyaWJlIHRvIGVudGl0eS1sZXZlbCBldmVudHMgd2l0aG91dCBwYXJzaW5nIHRoZSBldmVudCBib2R5LgoKIyMjIFRvcGljIExheW91dAoKfCBUb3BpYyB8IFR5cGUgfCBEZXNjcmlwdGlvbiB8CnwtLS0tLS0tfC0tLS0tLXwtLS0tLS0tLS0tLS18CnwgMCB8IGBTeW1ib2xgIHwgQ29udHJhY3QgbmFtZXNwYWNlIChlLmcuIGBzeW1ib2xfc2hvcnQhKCJlc2Nyb3ciKWApIHwKfCAxIHwgYFN5bWJvbGAgfCBBY3Rpb24gdmVyYiAoZS5nLiBgc3ltYm9sX3Nob3J0ISgiY3JlYXRlZCIpYCkgfAp8IDIgfCBFbnRpdHkgSUQgKGBlNjQ0YCkgfCBFbnRpdHkgaWRlbnRpZmllciDigJQgYHU2NGAgb3IgYEFkZHJlc3NgIGFzIGFuIGA9NjRgIGJpdCB2YWx1ZSB8CgpUb3BpYyAyIGlzIGFsd2F5cyBhIDY0LWJpdCB2YWx1ZSBzbyB0aGF0IGJvdGggYGk2NGAvdTY0YCBpZHMgb2YgY29udHJhY3QgcmVjb3JkcyBhbmQgYEFkZHJlc3NgIGtleXMgY2FuIGJlIGZpbHRlcmVkIHdpdGggdGhlIHNhbWUgUlBDIHRvcGljIGZpbHRlciBzaGFwZS4gQWRkcmVzc2VzIGFyZSBlbmNvZGVkIGFzIHRoZWlyIGA9NjRgIGJpdCB2YWx1ZSB2aWEgYEFkZHJlc3M6OnRvX3ZhbCgpYC4KCiMjIyBFdmVudCBTY2hlbWFzCgpFdmVyeSBldmVudCBjYXJyaWVzIGEgdHlwZWQgcGF5bG9hZCBpbiB0aGUgZXZlbnQgYm9keS4gVGhlIHRvcGljcyBhcmUgc3VmZmljaWVudCB0byBmaWx0ZXIgYnkgY29udHJhY3QsIGFjdGlvbiwgYW5kIGVudGl0eTsgdGhlIGJvZHkgY2FycmllcyB0aGUgcmVtYWluaW5nIGRhdGEuCgojIyMjIEVzY3JvdyBDb250cmFjdCAoYGVzY3Jvd2ApCgp8IEFjdGlvbiB8IFRvcGljIDIgfCBQYXlsb2FkIHwKfC0tLS0tLS0tfC0tLS0tLS0tLXwtLS0tLS0tLS18CnwgYGNyZWF0ZWRgIHwgYGVzY3Jvd19pZGAgKHU2NCkgfCBgRXNjcm93Q3JlYXRlZEV2ZW50IHsgZXNjcm93X2lkLCBidXllciwgYW1vdW50LCB0b2tlbiB9YCB8CnwgYGZ1bmRlZGAgfCBgZXNjcm93X2lkYCAodTY0KSB8IGBFc2Nyb3dGdW5kZWRFdmVudCB7IGVzY3Jvd19pZCwgYW1vdW50IH1gIHwKfCBgcmVsZWFzZWRgIHwgYGVzY3Jvd19pZGAgKHU2NCkgfCBgRXNjcm93UmVsZWFzZWRFdmVudCB7IGVzY3Jvd19pZCwgYW1vdW50IH1gIHwKfCBgcmVmdW5kZWRgIHwgYGVzY3Jvd19pZGAgKHU2NCkgfCBgRXNjcm93UmVmdW5kZWRFdmVudCB7IGVzY3Jvd19pZCwgYW1vdW50IH1gIHwKfCBgZGlzcHV0ZWRgIHwgYGVzY3Jvd19pZGAgKHU2NCkgfCBgRXNjcm93RGlzcHV0ZWRFdmVudCB7IGVzY3Jvd19pZCB9YCB8CnwgYHJlc29sdmVkYCB8IGBlc2Nyb3dfaWRgICh1NjQpIHwgYEVzY3Jvd1Jlc29sdmVkRXZlbnQgeyBlc2Nyb3dfaWQsIHJlbGVhc2VfdG9fc2VsbGVyIH1gIHwKfCBgY2FuY2VsbGVkYCB8IGBlc2Nyb3dfaWRgICh1NjQpIHwgYEVzY3Jvd0NhbmNlbGxlZEV2ZW50IHsgZXNjcm93X2lkIH1gIHwKCiMjIyMgUGVybWlzc2lvbnMgQ29udHJhY3QgKGBwZXJtcyIpCgp8IEFjdGlvbiB8IFRvcGljIDIgfCBQYXlsb2FkIHwKfC0tLS0tLS0tfC0tLS0tLS0tLXwtLS0tLS0tLS18CnwgYGdyYW50ZWRgIHwgYG93bmVyYCAoQWRkcmVzcykgfCBgUGVybWlzc2lvbkdyYW50ZWRFdmVudCB7IG93bmVyLCBkZWxlZ2F0ZSwgbGltaXRfdG90YWwsIGV4cGlyeSB9YCB8CnwgYHJldm9rZWRgIHwgYG93bmVyYCAoQWRkcmVzcykgfCBgUGVybWlzc2lvblJldm9rZWRFdmVudCB7IG93bmVyLCBkZWxlZ2F0ZSB9YCB8CnwgYHNwZW50YCB8IGBvd25lcmAgKEFkZHJlc3MpIHwgYFBlcm1pc3Npb25TcGVuZEV2ZW50IHsgb3duZXIsIGRlbGVnYXRlLCBhbW91bnQgfWAgfAp8IGBleHBpcmVkYCB8IGBvd25lcmAgKEFkZHJlc3MpIHwgYFBlcm1pc3Npb25FeHBpcmVkRXZlbnQgeyBvd25lciwgZGVsZWdhdGUgfWAgfAoKIyMjIyBEZWxlZ2F0aW9uIFJlZ2lzdHJ5IENvbnRyYWN0IChgZGVsZWdhdGlvbmApCgp8IEFjdGlvbiB8IFRvcGljIDIgfCBQYXlsb2FkIHwKfC0tLS0tLS0tfC0tLS0tLS0tLXwtLS0tLS0tLS18CnwgYHJlZ2lzdGVyZWRgIHwgYGRlbGVnYXRpb25faWRgICh1NjQpIHwgYERlbGVnYXRpb25SZWdpc3RlcmVkRXZlbnQgeyBkZWxlZ2F0aW9uX2lkLCBvd25lciwgZGVsZWdhdGUsIGV4cGlyeSB9YCB8CnwgYHVwZGF0ZWRgIHwgYGRlbGVnYXRpb25faWRgICh1NjQpIHwgYERlbGVnYXRpb25VcGRhdGVkRXZlbnQgeyBkZWxlZ2F0aW9uX2lkLCBleHBpcnkgfWAgfAp8IGByZXZva2VkYCB8IGBkZWxlZ2F0aW9uX2lkYCAodTY0KSB8IGBEZWxlZ2F0aW9uUmV2b2tlZEV2ZW50IHsgZGVsZWdhdGlvbl9pZCB9YCB8CgojIyMjIFJlcHV0YXRpb24gQ29udHJhY3QgKGByZXB1dGF0aW9uYCkKCnwgQWN0aW9uIHwgVG9waWMgMiB8IFBheWxvYWQgfAp8LS0tLS0tLS18LS0tLS0tLS0tfC0tLS0tLS0tLXwKfCBgcmVjb3JkZWRgIHwgYGVudGl0eWAgKEFkZHJlc3MpIHwgYFJlcHV0YXRpb25SZWNvcmRlZEV2ZW50IHsgZW50aXR5LCBzY29yZSB9YCB8CgojIyMjIE1hcmtldHBsYWNlIENvbnRyYWN0IChgbWFya2V0cGxhY2VgKQoKfCBBY3Rpb24gfCBUb3BpYyAyIHwgUGF5bG9hZCB8CnwtLS0tLS0tLXwtLS0tLS0tLS18LS0tLS0tLS0tfAp8IGByZWdpc3RlcmVkYCB8IGBtZXJjaGFudF9pZGAgKHU2NCkgfCBgTWVyY2hhbnRSZWdpc3RlcmVkRXZlbnQgeyBtZXJjaGFudF9pZCwgb3duZXIsIGNhdGVnb3J5IH1gIHwKfCBgdmVyaWZpZWRgIHwgYG1lcmNoYW50X2lkYCAodTY0KSB8IGBNZXJjaGFudFZlcmlmaWVkRXZlbnQgeyBtZXJjaGFudF9pZCwgdmVyaWZpZXIgfWAgfAp8IGBzdXNwZW5kZWRgIHwgYG1lcmNoYW50X2lkYCAodTY0KSB8IGBNZXJjaGFudFN1c3BlbmRlZEV2ZW50IHsgbWVyY2hhbnRfaWQgfWAgfAp8IGBjbG9zZWRgIHwgYG1lcmNoYW50X2lkYCAodTY0KSB8IGBNZXJjaGFudENsb3NlZEV2ZW50IHsgbWVyY2hhbnRfaWQgfWAgfAoKIyMjIFJQQyBGaWx0ZXIgRXhhbXBsZXMKCkZpbHRlciBieSBjb250cmFjdCBuYW1lc3BhY2Ugb25seToKCmBgYGpzb24KeyAidG9waWNzIjogW1sieCIsICJlc2Nyb3ciXV0gfQpgYGAKCkZpbHRlciBieSBjb250cmFjdCArIGFjdGlvbjoKCmBgYGpzb24KeyAidG9waWNzIjogW1sieCIsICJlc2Nyb3ciXSwgWyJ4IiwgInJlbGVhc2VkIl1dIH0KYGBgCgpGaWx0ZXIgYnkgY29udHJhY3QgKyBhY3Rpb24gKyBlbnRpdHkgKGUuZy4gZXNjcm93IGlkIDQyKToKCmBgYGpzb24KeyAidG9waWNzIjogW1sieCIsICJlc2Nyb3ciXSwgWyJ4IiwgInJlbGVhc2VkIl0sIFsieCIsICI0MiJdXSB9CmBgYAoKIyMgT24tQ2hhaW4gdnMgT2ZmLUNoYWluCgojIyMgT24tQ2hhaW4gKFNtYXJ0IENvbnRyYWN0cykKClRydXN0LWNyaXRpY2FsIG9wZXJhdGlvbnMgdGhhdCByZXF1aXJlIGJsb2NrY2hhaW4gZ3VhcmFudGVlczoKCi0gKipFc2Nyb3cqKjogRnVuZCBsb2NraW5nIGFuZCByZWxlYXNlCi0gKipQZXJtaXNzaW9ucyoqOiBTcGVuZGluZyBhdXRob3JpdHkgZGVsZWdhdGlvbgotICoqRGVsZWdhdGlvbiBSZWdpc3RyeSoqOiBEZWxlZ2F0aW9uIHJlY29yZHMgd2l0aCBleHBpcnkgYW5kIHJvbGxiYWNrCi0gKipSZXB1dGF0aW9uKio6IFJlcHV0YXRpb24gc2NvcmUgdHJhY2tpbmcKLSAqKk1hcmtldHBsYWNlKio6IE1lcmNoYW50IHJlZ2lzdHJ5LCB2ZXJpZmljYXRpb24sIGFuZCBkaXNjb3ZlcnkKCiMjIyBPZmYtQ2hhaW4gKFNlcnZpY2VzKQoKSGlnaC10aHJvdWdocHV0IG9wZXJhdGlvbnMgdGhhdCBkb24ndCByZXF1aXJlIGJsb2NrY2hhaW4gZ3VhcmFudGVlczoKCi0gKipDYXRhbG9nKio6IFByb2R1Y3QgY2F0YWxvZyBhbmQgc2VhcmNoCi0gKipTZWFyY2gqKjogUHJvZHVjdCBzZWFyY2ggYW5kIGNvbXBhcmlzb24KLSAqKkFuYWx5dGljcyoqOiBTcGVuZGluZyBhbmFseXRpY3MgYW5kIHJlcG9ydGluZwotICoqTm90aWZpY2F0aW9ucyoqOiBFbWFpbCBhbmQgcHVzaCBub3RpZmljYXRpb25zCgojIyMgSHlicmlkIEFwcHJvYWNoCgpTb21lIG9wZXJhdGlvbnMgdXNlIGEgaHlicmlkIGFwcHJvYWNoOgoKLSAqKk9yZGVyIENyZWF0aW9uKio6IE9mZi1jaGFpbiBvcmRlciBjcmVhdGlvbiwgb24tY2hhaW4gZXNjcm93Ci0gKipQYXltZW50Kio6IE9mZi1jaGFpbiBwYXltZW50IGluaXRpYXRpb24sIG9uLWNoYWluIHNldHRsZW1lbnQKLSAqKlJlcHV0YXRpb24qKjogT2ZmLWNoYWluIHJhdGluZyBjb2xsZWN0aW9uLCBvbi1jaGFpbiBhZ2dyZWdhdGlvbgoKIyMgQ29udHJhY3QgSW50ZXJhY3Rpb25zCgojIyMgQ3Jvc3MtQ29udHJhY3QgQ2FsbHMKCkNvbnRyYWN0cyBjYW4gY2FsbCBvdGhlciBjb250cmFjdHM6CgpgYGBydXN0CgpgYGAK
