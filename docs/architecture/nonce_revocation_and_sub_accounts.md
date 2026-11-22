# Nonce-Invalidating Delegation Key Revocation and Sub-Account Creation

## Overview

This document describes two new features added to the Delegation Registry contract:

1. **Nonce-Invalidating Delegation Key Revocation**: A lightweight mechanism for revoking compromised delegation keys without affecting the delegation's configuration or history.
2. **Delegation Sub-Account Creation**: Virtual isolated accounts for dedicated micro-payment agent operations.

## Feature 1: Nonce-Invalidating Delegation Key Revocation

### Motivation

Traditional delegation revocation (`revoke_delegation`) permanently terminates a delegation and transitions it to the `Revoked` status. However, there are scenarios where an owner may want to:

- Invalidate a compromised key without losing the delegation's configuration
- Rotate credentials for security purposes
- Temporarily disable authorization while keeping the delegation metadata intact

### Architecture

Each `DelegationRecord` now includes a `nonce` field (u64) that tracks the current key generation. When initialized, delegations start with `nonce = 0`.

```rust
pub struct DelegationRecord {
    // ... existing fields
    pub nonce: u64,
}
```

### Key Functions

#### `revoke_delegation_key(delegation_id: u64) -> Result<u64, DelegationError>`

Increments the delegation's nonce by 1, invalidating all prior authorizations.

**Behavior:**
- Requires owner authorization
- Increments the nonce field atomically
- Updates the delegation version and timestamp
- Creates a history snapshot
- Emits a `DelegationKeyRevokedEvent` with timestamped details
- Returns the new nonce value

**Storage:**
- Updates `DelegationRecord.nonce`
- Stores nonce separately at `DataKey::DelegationNonce(delegation_id)` for efficient lookup
- Extends TTL to keep data alive

#### `get_delegation_nonce(delegation_id: u64) -> Result<u64, DelegationError>`

Returns the current nonce for a delegation.

#### `is_authorized_with_nonce(delegation_id, agent_id, nonce) -> bool`

Extended authorization check that verifies:
1. Delegation exists and is Active
2. Has not expired
3. Agent ID matches
4. **Nonce matches the current on-chain value**

Returns `false` if any check fails, including nonce mismatch.

#### `is_authorized_for_with_nonce(delegation_id, agent_id, flag, nonce) -> bool`

Combines permission flag checking with nonce validation for complete authorization verification.

### Events

#### `DelegationKeyRevokedEvent`

Emitted when a delegation key is revoked via nonce invalidation.

```rust
pub struct DelegationKeyRevokedEvent {
    pub delegation_id: u64,
    pub owner: Address,
    pub agent: BytesN<32>,
    pub new_nonce: u64,
    pub timestamp: u64,
}
```

**Topics:** `("deleg", "key_rvk", delegation_id)`

### Usage Example

```rust
// Create a delegation (starts with nonce = 0)
let delegation_id = DelegationRegistry::create_delegation(
    env, owner, agent_id, permissions_contract, label, ttl
)?;

// Check authorization with nonce
let is_auth = DelegationRegistry::is_authorized_with_nonce(
    env, delegation_id, agent_id, 0
); // true

// Key is compromised - revoke it
let new_nonce = DelegationRegistry::revoke_delegation_key(env, delegation_id)?;
// new_nonce = 1

// Old authorization fails
let is_auth = DelegationRegistry::is_authorized_with_nonce(
    env, delegation_id, agent_id, 0
); // false - nonce mismatch

// New authorization with updated nonce
let is_auth = DelegationRegistry::is_authorized_with_nonce(
    env, delegation_id, agent_id, 1
); // true
```

### Benefits

- **Lightweight**: Does not change delegation status or configuration
- **Auditable**: Creates version snapshots and emits timestamped events
- **Backward Compatible**: Existing `is_authorized` functions continue to work
- **Efficient**: Nonce stored separately for fast lookup
- **Flexible**: Can be called multiple times to rotate keys

## Feature 2: Delegation Sub-Account Creation

### Motivation

Micro-payment agents often need isolated virtual addresses to:

- Segregate payment flows for different purposes
- Track agent-specific balances independently
- Limit blast radius of compromised agents
- Enable fine-grained auditing and accounting

### Architecture

A `SubAccountRecord` represents a virtual isolated account linked to a specific delegation.

```rust
pub struct SubAccountRecord {
    pub id: u64,
    pub owner: Address,
    pub sub_account_address: Address,
    pub delegation_id: u64,
    pub label: Symbol,
    pub created_at: u64,
    pub is_active: bool,
}
```

### Key Functions

#### `create_sub_account(owner, delegation_id, sub_account_address, label) -> Result<u64, DelegationError>`

Creates a new sub-account tied to an existing delegation.

**Behavior:**
- Requires owner authorization and `sub_account_address` authorization
- Verifies the delegation exists and belongs to the owner
- Requires the parent delegation to be `Active` (`NotActive` otherwise) and
  `ledger.sequence < expires_at_ledger` (`Expired` otherwise, even if
  `sweep_expired` has not run yet)
- Rejects a `sub_account_address` equal to the owner or the registry
  (`InvalidParam`) or already registered to any sub-account
  (`SubAccountAddressTaken`)
- Generates a unique sub-account ID per owner
- Stores the record with extended TTL and claims
  `DataKey::SubAccountAddress(sub_account_address)`
- Adds to owner's sub-account index
- Emits `SubAccountCreatedEvent` with timestamp
- Returns the sub-account ID

**Storage:**
- `DataKey::SubAccount(owner, id)` → `SubAccountRecord`
- `DataKey::NextSubAccountId(owner)` → Next available ID
- `DataKey::OwnerSubAccounts(owner)` → Vec<u64> of sub-account IDs

#### `deactivate_sub_account(owner, sub_account_id) -> Result<bool, DelegationError>`

Marks a sub-account as inactive without deleting it.

**Behavior:**
- Requires owner authorization
- Sets `is_active = false`
- Preserves record for audit trail
- Emits `SubAccountDeactivatedEvent`
- Returns `Ok(false)` if already inactive (idempotent)

#### `reactivate_sub_account(owner, sub_account_id) -> Result<bool, DelegationError>`

Reactivates a previously deactivated sub-account.

**Behavior:**
- Requires owner authorization
- Verifies the associated delegation is still valid (not Revoked/Expired)
- Rejects with `Expired` when `ledger.sequence >= expires_at_ledger`, even if
  the delegation has not been swept yet
- Sets `is_active = true`
- Emits `SubAccountReactivatedEvent` (topic `sub_reac`)
- Returns `Ok(false)` if already active (idempotent)

#### Query Functions

- `get_sub_account(owner, sub_account_id) -> Result<SubAccountRecord, DelegationError>`
- `get_sub_accounts_by_owner(owner) -> Vec<SubAccountRecord>`: Returns all sub-accounts
- `get_active_sub_accounts(owner) -> Vec<SubAccountRecord>`: Filters for active sub-accounts with valid delegations
- `get_sub_accounts_by_delegation(delegation_id) -> Vec<SubAccountRecord>`: Returns sub-accounts for a specific delegation

### Events

#### `SubAccountCreatedEvent`

```rust
pub struct SubAccountCreatedEvent {
    pub sub_account_id: u64,
    pub owner: Address,
    pub sub_account_address: Address,
    pub delegation_id: u64,
    pub timestamp: u64,
}
```

**Topics:** `("deleg", "sub_crtd", sub_account_id)`

#### `SubAccountDeactivatedEvent`

```rust
pub struct SubAccountDeactivatedEvent {
    pub sub_account_id: u64,
    pub owner: Address,
    pub sub_account_address: Address,
    pub timestamp: u64,
}
```

**Topics:** `("deleg", "sub_deac", sub_account_id)`

### Usage Example

```rust
// Create a delegation for the agent
let delegation_id = DelegationRegistry::create_delegation(
    env, owner, agent_id, permissions_contract, label, ttl
)?;

// Create sub-accounts for different micro-payment purposes
let payment_sub = DelegationRegistry::create_sub_account(
    env, owner, delegation_id, payments_agent_address, symbol_short!("payments")
)?;

let refund_sub = DelegationRegistry::create_sub_account(
    env, owner, delegation_id, refunds_agent_address, symbol_short!("refunds")
)?;

// Get all active sub-accounts
let active_subs = DelegationRegistry::get_active_sub_accounts(env, owner);

// Deactivate when no longer needed
DelegationRegistry::deactivate_sub_account(env, owner, payment_sub)?;

// Can reactivate later if the delegation is still valid
DelegationRegistry::reactivate_sub_account(env, owner, payment_sub)?;
```

### Benefits

- **Isolation**: Each sub-account is a separate entity with its own address
- **Delegation-Linked**: Inherits permissions and lifecycle from parent delegation
- **Auditable**: All operations are timestamped and emit events
- **Flexible**: Can be deactivated and reactivated as needed
- **Scalable**: Multiple sub-accounts per delegation

## Error Codes

Three new error codes have been added to the `DelegationError` enum:

| Error Code | Value | Description |
|------------|-------|-------------|
| `SubAccountNotFound` | 4017 | The sub-account was not found |
| `SubAccountNotActive` | 4018 | The sub-account is not active |
| `DelegationMismatch` | 4019 | The delegation does not match the sub-account |

These maintain the contract's error code allocation scheme (4000-4999 for delegation registry).

## Storage Keys

New storage keys added to `DataKey` enum:

```rust
pub enum DataKey {
    // ... existing keys
    DelegationNonce(u64),              // Nonce for revocation tracking
    SubAccount(Address, u64),          // Sub-account record
    NextSubAccountId(Address),         // Next sub-account ID per owner
    OwnerSubAccounts(Address),         // Sub-account index per owner
}
```

## TTL Management

All new storage entries follow the contract's persistent storage policy:

- **Bump Threshold**: 17,280 ledgers (~1 day)
- **Bump Amount**: 518,400 ledgers (~30 days)
- TTLs are extended on reads and writes to keep active data alive

## Security Considerations

### Nonce Revocation

- **Replay Protection**: Old nonces cannot be reused after revocation
- **Atomic Updates**: Nonce increment is atomic with version bump
- **Audit Trail**: All revocations create history snapshots
- **No Downgrade**: Nonces can only increment, never decrement

### Sub-Accounts

- **Owner Authorization**: All operations require owner auth
- **Delegation Validation**: Sub-accounts cannot be reactivated if parent delegation is invalid
- **Lifecycle Coupling**: Sub-account validity is tied to delegation status
- **Address Derivation**: In production, sub-account addresses should be cryptographically derived

## Future Enhancements

### Nonce System
- Add batch nonce revocation for multiple delegations
- Support nonce expiry/rotation policies
- Add nonce history tracking for forensics

### Sub-Accounts
- Implement cryptographic address derivation (e.g., using owner + id as seed)
- Add sub-account balance tracking
- Support sub-account-specific spending limits
- Enable sub-account-to-sub-account transfers
- Add batch operations for multiple sub-accounts

## Backward Compatibility

Both features are fully backward compatible:

- Existing delegations without nonces continue to work with `is_authorized`
- New `is_authorized_with_nonce` functions are additive
- Sub-accounts are optional - delegations work without them
- No breaking changes to existing data structures or function signatures
