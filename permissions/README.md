# Permissions Contract

On-chain spending controls for delegated AI agent authority.

Grants allow an owner to delegate spending authority to another address
("delegate") with optional limits, expiry, and per-transaction caps. The
contract supports multi-owner grants, relayer (gasless) spends, allowances,
pause/resume, and permission transfers.

## Functions

| Function | Auth required | Description |
|---|---|---|
| `grant` | owner | Grant spending permission to a delegate |
| `grant_child` | owner | Grant a nested permission derived from an existing grant |
| `revoke` | owner | Revoke a delegate's permission |
| `transfer_permission` | owner | Transfer a permission to another account |
| `renew_permission` | owner | Extend the expiry of a grant |
| `update_expiry` | owner | Change a grant's expiry ledger |
| `can_spend` | — | Check whether a spend is allowed (limits + expiry) |
| `execute_spend` | delegate | Spend within the grant limits; emits `PermissionSpendEvent` |
| `set_relayer_key` / `get_relayer_key` | delegate | Configure the key used for relayer-signed spends |
| `execute_spend_via_relayer` | relayer signature | Gasless spend using a relayer signature + nonce |
| `grant_multi_owner` | owners | Multi-owner grant (quorum-based authorization) |
| `can_spend_multi` / `execute_spend_multi` | owners | Quorum checks and spend for multi-owner grants |
| `get_multi_permission` / `preview_spend` | — | Read-only grant and spend previews |
| `get_permission` / `get_remaining_allowance` / `get_allowance_detail` | — | Read-only allowance and grant views |
| `increase_allowance` / `decrease_allowance` | owner | Adjust a grant's allowance |
| `pause` / `resume` | owner | Pause/resume a delegate's permission |
| `get_pause_metadata` | — | Pause state and reason |
| `set_admin` / `propose_admin` / `accept_admin` | admin | Admin management (two-step transfer) |
| `pause_grants` | admin | Pause all new grants |
| `sweep_expired` / `sweep_expired_batch` | any | Sweep expired permissions into Expired status (single or bounded batch) |
| `sweep_inactive` / `sweep_inactive_batch` | any | Auto-revoke inactive permissions idle past threshold (single or bounded batch) |

## Events

Events are emitted with the topic prefix `("perm", …)`:

| Second topic | Payload struct | Emitted by |
|---|---|---|
| `"granted"` | `PermissionGrantedEvent` | `grant` / `grant_child` / `grant_multi_owner` (`"mgrant"`) |
| `"merc_list"` | — | Grant with merchant allow/deny lists |
| `"revoked"` | `PermissionRevokedEvent` | `revoke` |
| `"transf"` | `PermissionTransferredEvent` | `transfer_permission` |
| `"renewed"` / `"exp_upd"` | — | `renew_permission` / `update_expiry` |
| `"spent"` / `"mspent"` / `"relayed"` | `PermissionSpendEvent` | `execute_spend` / `execute_spend_multi` / `execute_spend_via_relayer` |
| `"allowinc"` / `"allowdec"` | — | `increase_allowance` / `decrease_allowance` |
| `"paused"` / `"resumed"` / `"gpaused"` | `PermissionPausedEvent` / `PermissionResumedEvent` | `pause` / `resume` / `pause_grants` |

## Development

```bash
cd permissions

# Run all tests
cargo test

# Build WASM for deployment
cargo build --target wasm32-unknown-unknown --release
```

> TypeScript types mirroring the on-chain records (e.g. the `PermissionGrant`
> interface) ship in [`@delegolabs/types`](https://github.com/DelegoLabs/Delego-backend),
> published from the Delego-backend repository.
