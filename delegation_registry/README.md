# Delegation Registry Contract

## Overview

The Delegation Registry is a Soroban smart contract that manages delegation records, enabling secure delegation of permissions from owners to agents with fine-grained control over capabilities and lifecycle.

## Features

### Core Delegation Management

- **Create Delegations**: Create delegations with full or scoped permissions
- **Lifecycle Management**: Pause, resume, and revoke delegations
- **Admin Controls**: Emergency pause/resume/revoke by admin
- **Version History**: Complete audit trail with rollback capability
- **Permission Flags**: Bitmasked capabilities (spend, refund, dispute, delegate)
- **Expiry Management**: Time-bound delegations with automated expiry sweeping

### Advanced Features

#### Nonce-Invalidating Key Revocation

Lightweight key revocation mechanism that invalidates compromised keys without affecting delegation configuration.

**Key Functions:**

- `revoke_delegation_key()`: Increment nonce to invalidate all prior authorizations
- `get_delegation_nonce()`: Get current nonce value
- `is_authorized_with_nonce()`: Authorization check with nonce verification
- `is_authorized_for_with_nonce()`: Permission flag check with nonce verification

**Use Cases:**

- Security incident response (compromised keys)
- Periodic credential rotation
- Temporary authorization invalidation
- Key generation management

#### Delegation Sub-Accounts

Virtual isolated accounts for dedicated micro-payment agent operations.

**Key Functions:**

- `create_sub_account()`: Create a new sub-account tied to a delegation
- `deactivate_sub_account()`: Temporarily disable a sub-account
- `reactivate_sub_account()`: Re-enable a deactivated sub-account
- `get_sub_accounts_by_owner()`: List all sub-accounts for an owner
- `get_active_sub_accounts()`: List active sub-accounts with valid delegations
- `get_sub_accounts_by_delegation()`: List sub-accounts for a specific delegation

**Use Cases:**

- Isolated payment flows per agent
- Fine-grained accounting and auditing
- Blast radius limitation for compromised agents
- Multi-purpose agent segregation

## Contract Structure

### Data Types

#### `DelegationRecord`

Main delegation record containing:

- Basic info: id, owner, agent_id, permissions_contract
- Status: Active, Paused, Revoked, Expired
- Timestamps: created_at, updated_at
- Expiry: expires_at_ledger
- Versioning: version (for history)
- Permissions: PermissionBitmask
- **New**: `nonce` for key revocation tracking

#### `SubAccountRecord`

Sub-account record containing:

- Basic info: id, owner, sub_account_address
- Association: delegation_id
- Metadata: label, created_at
- Status: is_active

#### `PermissionBitmask`

Bitmasked capabilities:

- `PERM_FLAG_SPEND` (1 << 0): Move funds
- `PERM_FLAG_REFUND` (1 << 1): Claim refunds
- `PERM_FLAG_DISPUTE` (1 << 2): Open disputes
- `PERM_FLAG_DELEGATE` (1 << 3): Re-delegate authority

### Events

All events include timestamped details for audit trails.

#### Core Events

- `DelegationCreatedEvent`: New delegation created
- `DelegationPausedEvent`: Delegation paused
- `DelegationResumedEvent`: Delegation resumed
- `DelegationRevokedEvent`: Delegation revoked
- `DelegationExpiredEvent`: Delegation expired
- `PermissionFlagsChangedEvent`: Permission scope changed

#### New Events

- `DelegationKeyRevokedEvent`: Key revoked via nonce increment
- `SubAccountCreatedEvent`: Sub-account created
- `SubAccountDeactivatedEvent`: Sub-account deactivated

### Error Codes

Allocated range: 4000-4999

| Code | Error                 | Description                                    |
| ---- | --------------------- | ---------------------------------------------- |
| 4001 | NotFound              | Delegation not found                           |
| 4002 | NotActive             | Delegation not active                          |
| 4003 | NotPaused             | Delegation not paused                          |
| 4004 | Expired               | Delegation expired                             |
| 4005 | AlreadyInitialized    | Registry already initialized                   |
| 4006 | InvalidVersion        | Invalid version                                |
| 4007 | VersionNotLower       | Target version not lower                       |
| 4008 | SnapshotNotFound      | Snapshot not found                             |
| 4009 | InvalidAgentId        | Invalid agent ID                               |
| 4010 | IdExhausted           | No more IDs available                          |
| 4011 | NoPendingAdmin        | No pending admin                               |
| 4012 | NotInitialized        | Registry not initialized                       |
| 4013 | InvalidTtl            | Invalid TTL value                              |
| 4014 | NotAuthorized         | Not authorized                                 |
| 4015 | InvalidPermissionFlag | Invalid permission flag                        |
| 4016 | InvalidBatchSize      | Invalid batch size                             |
| 4017 | SubAccountNotFound    | Sub-account not found                          |
| 4018 | SubAccountNotActive   | Sub-account not active                         |
| 4019 | DelegationMismatch    | Delegation mismatch                            |
| 4020 | InvalidParam          | Zero `owner` or `permissions_contract` address |

## Usage Examples

### Creating a Delegation with Key Rotation

```rust
// Create delegation
let delegation_id = DelegationRegistry::create_delegation(
    env.clone(),
    owner.clone(),
    agent_id.clone(),
    permissions_contract.clone(),
    symbol_short!("agent1"),
    100_000, // TTL in ledgers
)?;

// Initial authorization works (nonce = 0)
assert!(DelegationRegistry::is_authorized_with_nonce(
    env.clone(),
    delegation_id,
    agent_id.clone(),
    0
));

// Security incident - revoke the key
let new_nonce = DelegationRegistry::revoke_delegation_key(
    env.clone(),
    delegation_id
)?;
assert_eq!(new_nonce, 1);

// Old authorization fails
assert!(!DelegationRegistry::is_authorized_with_nonce(
    env.clone(),
    delegation_id,
    agent_id.clone(),
    0
));

// New authorization with updated nonce succeeds
assert!(DelegationRegistry::is_authorized_with_nonce(
    env.clone(),
    delegation_id,
    agent_id.clone(),
    1
));
```

### Managing Sub-Accounts

```rust
// Create delegation
let delegation_id = DelegationRegistry::create_delegation(
    env.clone(),
    owner.clone(),
    agent_id.clone(),
    permissions_contract.clone(),
    symbol_short!("micropay"),
    100_000,
)?;

// Create sub-accounts for different purposes
let payment_sub = DelegationRegistry::create_sub_account(
    env.clone(),
    owner.clone(),
    delegation_id,
    symbol_short!("payments"),
)?;

let refund_sub = DelegationRegistry::create_sub_account(
    env.clone(),
    owner.clone(),
    delegation_id,
    symbol_short!("refunds"),
)?;

// Query active sub-accounts
let active_subs = DelegationRegistry::get_active_sub_accounts(
    env.clone(),
    owner.clone()
);
assert_eq!(active_subs.len(), 2);

// Deactivate when no longer needed
DelegationRegistry::deactivate_sub_account(
    env.clone(),
    owner.clone(),
    payment_sub
)?;

// Can reactivate if delegation still valid
DelegationRegistry::reactivate_sub_account(
    env.clone(),
    owner.clone(),
    payment_sub
)?;
```

### Scoped Permissions with Nonce

```rust
// Create delegation with only spend permission
let permissions = PermissionBitmask::none().set_flag(PERM_FLAG_SPEND);
let delegation_id = DelegationRegistry::create_scoped_delegation(
    env.clone(),
    owner.clone(),
    agent_id.clone(),
    permissions_contract.clone(),
    symbol_short!("spend_only"),
    100_000,
    permissions,
)?;

// Check specific permission with nonce
assert!(DelegationRegistry::is_authorized_for_with_nonce(
    env.clone(),
    delegation_id,
    agent_id.clone(),
    PERM_FLAG_SPEND,
    0
));

// Refund permission not granted
assert!(!DelegationRegistry::is_authorized_for_with_nonce(
    env.clone(),
    delegation_id,
    agent_id.clone(),
    PERM_FLAG_REFUND,
    0
));
```

## Storage Policy

All persistent storage follows the contract's TTL policy:

- **Bump Threshold**: 17,280 ledgers (~1 day)
- **Bump Amount**: 518,400 ledgers (~30 days)
- Read paths automatically extend TTLs to keep active data alive

## Testing

Run tests with:

```bash
cargo test
```

The test suite covers:

- Core delegation lifecycle
- Permission flag management
- Version history and rollback
- Admin operations
- Nonce-based key revocation
- Sub-account creation and management
- Error conditions and edge cases

## Documentation

Additional documentation:

- [Architecture Overview](../docs/architecture/contracts.md)
- [Nonce Revocation and Sub-Accounts](../docs/architecture/nonce_revocation_and_sub_accounts.md)

## Version

Current version: 0.0.1
