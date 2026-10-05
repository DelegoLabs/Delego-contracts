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
| `grant_ephemeral_session` | owner | Grant a session key an aggregate spend limit and bounded ledger lifetime |
| `grant_child` | owner | Grant a nested permission derived from an existing grant |
| `revoke` | owner | Revoke a delegate's permission |
| `transfer_permission` | owner | Transfer a permission to another account |
| `renew_permission` | owner | Extend the expiry of a grant |
| `update_expiry` | owner | Change a grant's expiry ledger |
| `can_spend` | — | Check whether a spend is allowed (limits + expiry) |
| `execute_spend` | delegate | Spend within the grant limits; emits `PermissionSpendEvent` |
| `grant_scoped` / `re_grant_scoped` | owner | Grant/replace a permission restricted to specific contract entrypoints |
| `set_permission_scope` / `get_permission_scope` | owner | Set, replace or clear a permission's function scope |
| `can_spend_scoped` / `execute_spend_scoped` | delegate | Spend checks/execution that state the invoked contract entrypoint |
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
| `get_audit_log_page` | — | Read up to 20 retained audit entries using a zero-based cursor |
| `recheck_merchant_verification` | — | Dynamic check whether a merchant meets the current verification policy |
| `revalidate_merchant_status` | any | Re-evaluate a merchant against the current policy, applying the grace period |

## Ephemeral delegated sessions

`grant_ephemeral_session(owner, SessionKeyConfig)` authorizes `session_key` to
spend up to `max_spend_per_session` while the current ledger sequence is less
than or equal to `valid_until_ledger`. The configured window can span at most
720 ledger advances. The session configuration and aggregate spend counter use
temporary storage; Soroban removes them automatically after the expiry window,
without a persistent cleanup record or explicit revocation.

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
| `"scope"` | `PermissionScopeUpdatedEvent` | `grant_scoped` / `re_grant_scoped` / `set_permission_scope` / `transfer_permission` |

## Function-scoped permissions

Limits and merchant lists describe *how much* a delegate may spend, not *what*
it may invoke. A `ScopedPermissionConfig` narrows a grant to one target
contract and an explicit list of entrypoints:

```rust
ScopedPermissionConfig {
    target_contract: escrow_address,
    allowed_function_symbols: vec![Symbol::new(&env, "fund")],
}
```

Scoping **fails closed**:

- A scoped grant can only be spent through `can_spend_scoped` /
  `execute_spend_scoped`, which state the invoked contract and function. The
  unscoped `can_spend` / `execute_spend` entrypoints reject a scoped grant with
  `PermissionError::UnauthorizedFunction`, so the check cannot be skipped by
  omitting the invocation.
- `target_contract` must match the contract named by the invocation, and
  `invoked_function` must appear in `allowed_function_symbols`; either mismatch
  is rejected with `PermissionError::UnauthorizedFunction` and no allowance
  moves.
- `grant_child` sub-delegations inherit their parent's scope, so a delegate
  cannot escalate a narrow grant into a wider one for a downstream agent. The
  effective scope is the intersection across the whole parent chain.
- `transfer_permission` carries the scope to the incoming delegate.
- `allowed_function_symbols` must be non-empty, duplicate-free and at most
  `MAX_FUNCTIONS_PER_PERMISSION` entries.
- `re_grant` replaces the delegation wholesale and therefore clears the scope;
  `re_grant_with_metadata` exists to change limits and metadata, so it carries
  an existing scope over instead. Dropping a scope is always a deliberate act
  via `re_grant` or `set_permission_scope(owner, delegate, None)`.
- Because `RelayedSpendMessage` has no entrypoint field, a scoped grant cannot
  be spent via `execute_spend_via_relayer` — the signature cannot attest which
  function is being invoked. Relayed agents on a scoped grant submit
  `execute_spend_scoped` themselves.

The scope is stored under its own key rather than inside `PermissionRecord`, so
the serialized shape of existing permissions is unchanged and delegations
granted before this feature keep loading. An absent scope means the delegation
is unscoped and behaves exactly as before.
| `"vpolicy"` | `VerificationPolicyUpdatedEvent` | Policy update capturing the grace period |
| `"vrevalid"` | `VerificationRevalidatedEvent` | `revalidate_merchant_status` |

## Verification Policy Threshold Increases

Merchant verification is evaluated dynamically against the current
policy rather than a cached boolean flag. When governance raises the
required attestation count (e.g. from 1 to 2), merchants verified under the
previous policy are not immediately de-verified. Instead they receive a
30-day grace period (counted in ledgers) to acquire the additional
attestations.

- `recheck_merchant_verification` is the pure dynamic check. It returns
true only when the merchant's attestation count meets or exceeds the
current policy's `required` threshold.
- `revalidate_merchant_status` is the on-chain entrypoint. It re-evaluates
the merchant against the current policy and applies the grace period to
pre-existing merchants. Merchants that fail to meet the updated standard
transition gracefully to a `Suspended` status once the grace period has
elapsed.

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

Audit entries are stored under individual indexed persistent keys in a
10-entry ring buffer. `get_audit_log_page(owner, delegate, cursor)` returns
`AuditTrailPage`; follow `next_cursor` until it is `None`. Queries deserialize
at most 20 entries rather than the full trail.
