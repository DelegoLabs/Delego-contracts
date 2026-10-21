# Escrow Contract

Soroban smart contract for holding purchase funds until fulfillment.

## Functions

| Function | Auth required | Description |
|---|---|---|
| `initialize` | — | Set admin, fee config, and amount limits |
| `version` | — | Return contract name and semver |
| `create` | — | Create an unfunded escrow record in `Created` status |
| `fund` | buyer | Fund an existing `Created` escrow |
| `cancel` | seller (merchant) | Cancel an unfunded `Created` escrow (guarded, see below) |
| `accept_order` | seller (merchant) | Record acceptance of an order and re-anchor its cancel window |
| `agree_cancel` | buyer | Waive the cancel protection window for one escrow |
| `get_order_acceptance` | — | Acceptance and cancel-protection snapshot for an escrow |
| `get_cancel_eligibility` | — | Whether a seller may cancel an escrow right now, and why |
| `get_cancel_lockout` | — | Configured default cancel protection window, in ledgers |
| `set_cancel_lockout` | admin | Set the default cancel protection window for new escrows |
| `deposit` | buyer | Lock buyer funds for an order (convenience `create` + `fund`) |
| `batch_create_escrows` | buyer | Atomically create and fund up to 50 orders with aggregated token allowances |
| `release` | buyer / admin | Transfer full remaining balance to seller |
| `publish_merkle_root` | admin / co-admin | Anchor an immutable daily delivery Merkle root |
| `release_with_merkle_proof` | buyer | Release funds after proving delivery inclusion |
| `verify_merkle_proof` | — | Verify a SHA-256 Merkle path |
| `partial_release` | buyer / admin | Transfer a partial amount to seller |
| `refund` | seller / admin / buyer (after timeout) | Return funds to buyer |
| `dispute` | buyer / seller | Mark escrow as disputed |
| `resolve_dispute` | admin | Resolve dispute, release to seller or refund buyer |
| `resolve_dispute_split` | admin | Resolve a dispute with buyer, seller, and mediator payouts |
| `resolve_dispute_quorum` | any (after quorum) | Resolve via multi-arbiter quorum vote |
| `vote_dispute` | arbiter | Cast a quorum vote on a disputed escrow |
| `get_escrow` | — | Full `EscrowRecord` for an escrow id |
| `get_receipt` | — | Compact buyer-facing receipt |
| `get_merchant_receipt` | — | Seller-facing receipt with `release_eligible` flag |
| `get_release_eligibility` | — | Whether a release can proceed and why |
| `get_refund_eligibility` | — | Whether a caller can refund and why |
| `get_timeout_view` | — | Timeout metadata: ledger numbers and refundability |
| `get_escrow_metadata` | — | Optional off-chain order hash stored at deposit |
| `get_token` | — | Token address for an escrow |
| `get_fee_config` | — | Current fee config |
| `get_limits` | — | Current amount limits |
| `get_quorum_config` | — | Current arbiter quorum config |
| `get_dispute_votes` | — | Votes cast on a disputed escrow |
| `get_create_paused` | — | Whether new escrow creation is paused |
| `verify_delivery_and_release` | caller | Verify an Ed25519 delivery proof and release funds |
| `get_admin` | — | Current primary admin and pending transfer target |
| `set_limits` | admin | Update amount limits |
| `set_quorum_config` | admin | Update arbiter list and threshold |
| `update_fee` | admin | Update fee basis points |
| `add_token` | admin | Whitelist a token for deposits |
| `remove_token` | admin | Remove a token from the whitelist |
| `list_tokens` | — | List all whitelisted tokens |
| `is_token_allowed` | — | Check if a token is whitelisted |
| `set_create_paused` | admin | Pause or unpause new escrow creation |
| `set_merchant_registry` | admin | Configure marketplace checks for seller trading status |
| `set_oracle_public_key` | admin | Configure the Ed25519 key accepted for delivery proofs |
| `propose_admin` | primary admin | Start a two-step admin transfer |
| `accept_admin` | pending admin | Accept the admin role |
| `cancel_admin_transfer` | primary admin | Cancel a pending admin transfer |
| `add_co_admin` | primary admin | Add a co-admin |
| `remove_co_admin` | primary admin | Remove a co-admin |
| `is_admin` | — | Check if an address is admin or co-admin |
| `prune_dispute_votes` | admin | Prune dispute and timeout votes for settled escrows (bounded batch) |
| `archive_terminal_escrow` | — | Reclaim all persistent storage of a terminal escrow past its retention window (issue #331) |

## `get_admin`

Read-only getter for backend health checks and deployment verification. It
returns the active primary admin and includes `pending_admin` when a two-step
admin transfer has been proposed but not yet accepted.

```rust
pub fn get_admin(env: Env) -> Result<AdminView, EscrowError>
```

### `AdminView`

```rust
pub struct AdminView {
    pub admin: Address,
    pub pending_admin: Option<Address>,
}
```

### Errors

| Code | Meaning |
|---|---|
| `EscrowError::NotFound` (2) | Contract has not been initialized |

### No new storage keys, events, migrations, or environment variables

`get_admin` reads existing instance storage keys: `DataKey::Admin` and
`DataKey::PendingAdmin`. It requires no auth and does not mutate state or emit
events.

## `get_timeout_view` (issue #88)

Read-only getter that returns timeout metadata for a single escrow without
mutating any contract state. Safe to call from off-chain indexers and backend
services without auth.

```rust
pub fn get_timeout_view(env: Env, escrow_id: u64) -> Result<EscrowTimeoutView, EscrowError>
```

### `EscrowTimeoutView`

```rust
pub struct EscrowTimeoutView {
    pub escrow_id: BytesN<32>,   // 32-byte order id (same key as other receipts)
    pub timeout_ledger: u32,     // ledger sequence when buyer-refund timeout expires
    pub current_ledger: u32,     // ledger sequence at call time
    pub refundable: bool,        // true only when Funded AND current_ledger >= timeout_ledger
}
```

`refundable` is `true` only when the escrow is still in `Funded` status **and**
`current_ledger >= timeout_ledger`. Terminal states (`Released`, `Refunded`) and
`Disputed` always return `refundable: false`.

### Errors

| Code | Meaning |
|---|---|
| `EscrowError::NotFound` (2) | No escrow exists for the given `escrow_id` |

### No new storage keys or environment variables

`get_timeout_view` reads `DataKey::Escrow(escrow_id)` (existing persistent
storage) and `env.ledger().sequence()`. No new keys, migrations, or environment
variables are required.

## Merchant Cancellation

Merchants (sellers) may cancel an escrow that has been created but not yet funded (`status == Created`).
Cancellation transitions the escrow to `EscrowStatus::Cancelled` (a terminal state) and emits `EscrowCancelledEvent`.

```rust
pub fn cancel(env: Env, escrow_id: u64, caller: Address, reason: Symbol) -> Result<bool, EscrowError>
```

A unilateral `cancel` is refused for a short window after the order is created
(and after a seller acceptance) so that a seller watching the mempool cannot
front-run a buyer's pending `fund`/`deposit` submission. See
[Cancellation protection](#cancellation-protection-issue-355).

### `EscrowCancelledEvent`

```rust
pub struct EscrowCancelledEvent {
    pub escrow_id: BytesN<32>,   // 32-byte order id
    pub cancelled_by: Address,   // merchant address
    pub reason: Symbol,         // short cancellation reason symbol
}
```

### Errors

| Code | Meaning |
|---|---|
| `EscrowError::AlreadyFunded` (28) | Cannot cancel an escrow after funds are locked |
| `EscrowError::AlreadyCancelled` (27) | Escrow has already been cancelled |
| `EscrowError::InvalidStatus` (6) | Escrow is in a state that cannot be cancelled |
| `EscrowError::Unauthorized` (3) | Caller is not the merchant (`seller`) |
| `EscrowError::CancelLockoutActive` (411) | Cancel protection window is running and no agreement or timeout applies |
| `EscrowError::NotFound` (2) | No escrow exists for the given `escrow_id` |

## Cancellation protection (issue #355)

An order is created before the buyer commits funds, so a seller that watches
the mempool could otherwise get a `cancel` in front of a pending
`fund`/`deposit` and invalidate an order the buyer is already committed to.
The contract cannot observe mempool ordering, so it makes the *unilateral*
cancel lose deterministically instead:

- **Protection window.** Every escrow snapshots `cancel_lockout_ledgers`
  (default `10`, admin-configurable, `0` disables) at creation. Inside the
  window a seller can only cancel with the buyer's agreement on-chain, or
  once the escrow's own timeout is reached.
- **Acceptance re-anchors the window.** `accept_order` moves the deadline to
  the acceptance ledger, so a seller who accepts an order cannot shorten the
  buyer's window afterwards.
- **Funding always wins.** `fund`/`deposit` moves the escrow to `Funded`, and
  `cancel` then fails with `AlreadyFunded` in every ledger, protected or not.
- **Fails closed.** An escrow whose protection snapshot is missing (a legacy
  record, or an evicted entry) is treated as if the window never expires;
  only the buyer's agreement or the escrow timeout can clear it.
- **Deterministic previews.** `get_cancel_eligibility` returns exactly the
  answer `cancel` will give, with a `reason` symbol, so clients never have to
  guess at submission order.

```rust
pub struct OrderAcceptanceState {
    pub seller_accepted: bool,
    pub accepted_at_ledger: u32,
    pub cancel_lockout_ledgers: u32,
}

pub struct CancelEligibility {
    pub escrow_id: u64,
    pub eligible: bool,
    pub reason: Symbol,   // ok | agreed | timeout | lockout | funded | cancelled | badstate | notseller | notfound
}
```

### New entry points

```rust
pub fn accept_order(env: Env, escrow_id: u64, seller: Address) -> Result<bool, EscrowError>
pub fn agree_cancel(env: Env, escrow_id: u64, buyer: Address) -> Result<bool, EscrowError>
pub fn get_order_acceptance(env: Env, escrow_id: u64) -> Result<OrderAcceptanceState, EscrowError>
pub fn get_cancel_eligibility(env: Env, escrow_id: u64, caller: Address) -> CancelEligibility
pub fn get_cancel_lockout(env: Env) -> u32
pub fn set_cancel_lockout(env: Env, admin: Address, ledgers: u32) -> Result<bool, EscrowError>
```

`set_cancel_lockout` rejects values above `MAX_CANCEL_LOCKOUT_LEDGERS` (1000)
with `InvalidCancelLockout` and is admin-only; the new default is snapshotted
into each escrow at creation and on acceptance, so it never changes the
protection of an order that is already live.

### Storage keys added

`DataKey::OrderAcceptance(escrow_id)`, `DataKey::CancelLockoutLedger(escrow_id)`,
`DataKey::CancelBuyerAgreement(escrow_id)` and the instance-level
`DataKey::CancelLockoutLedgers`. All are written on the same paths as the
escrow record and bumped with the same TTL thresholds; the agreement key is
only written when a buyer agrees, and is consumed by the `cancel` it
authorized.

## Events

| Topic tuple | Payload struct | Emitted by |
|---|---|---|
| `("escrow", "created")` | `EscrowCreatedEvent` | `create` / `deposit` |
| `("escrow", "metadata")` | `EscrowMetadataEvent` | `create` / `deposit` (when metadata supplied) |
| `("escrow", "cancelled")` | `EscrowCancelledEvent` | `cancel` |
| `("escrow", "archived", escrow_id)` | `EscrowArchivedEvent` | `archive_terminal_escrow` |
| `("escrow", "accepted", escrow_id)` | `EscrowOrderAcceptedEvent` | `accept_order` |
| `("escrow", "agreed", escrow_id)` | `EscrowCancelAgreedEvent` | `agree_cancel` |
| `("escrow", "released")` | `EscrowReleasedEvent` | `partial_release` / `release` |
| `("escrow", "refunded")` | `EscrowRefundedEvent` | `refund` |
| `("escrow", "disputed")` | `EscrowDisputedEvent` | `dispute` |
| `("escrow", "resolved")` | `EscrowResolvedEvent` | `resolve_dispute` / `resolve_dispute_quorum` |
| `("escrow", "dispsplit")` | `DisputeResolvedEvent` | `resolve_dispute_split` |
| `("escrow", "merkroot", date)` | `BytesN<32>` | `publish_merkle_root` |
| `("escrow", "dctmo")` | `DualControlTimeoutSetEvent` | `set_dual_control_timeout` |
| `("escrow", "dcfb")` | `DualControlTimeoutFallbackEvent` | `handle_dual_control_timeout` |
| `("escrow", "paused")` | `EscrowPauseChangedEvent` | `set_create_paused` |
| `("admin", "proposed")` | `AdminProposedEvent` | `propose_admin` |
| `("admin", "accepted")` | `AdminAcceptedEvent` | `accept_admin` |
| `("admin", "cancelled")` | `AdminTransferCancelledEvent` | `cancel_admin_transfer` |

## Merkle delivery proofs

Oracles publish one root per UTC epoch day with
`publish_merkle_root(caller, date, root)`. The primary admin and co-admins are
the authorized publishers, and an existing root cannot be overwritten while
stored. Reading or using a root refreshes its persistent-storage TTL. Leaves
are `SHA-256(order_id)`; internal nodes are
SHA-256 of the concatenated left and right 32-byte child hashes. `index` is the
leaf's zero-based position, and `proof` lists siblings from leaf level to root.
A buyer can release a funded escrow with
`release_with_merkle_proof(escrow_id, buyer, date, proof)` only when the proof
matches both the published root and that escrow's order ID.

## Batch escrow creation

`batch_create_escrows(buyer, items)` accepts `BatchEscrowItem` values with an
absolute `timeout_ledger`. The buyer must approve the escrow contract for the
sum of each token's amounts; the contract makes one `transfer_from` call per
distinct token. Batches are limited to 50 entries, and any validation or
allowance failure reverts all created records and transfers.

## Split dispute resolution

`resolve_dispute_split(escrow_id, caller, award)` is admin-only and accepts
`DisputeResolutionAward` amounts for the buyer, seller, and mediator. All
amounts must be non-negative and sum exactly to the escrow balance. The
mediator fee is explicit and no additional platform release fee is charged.
The call transfers each nonzero award, records the buyer amount as refunded
and seller-plus-mediator amounts as released, and emits `DisputeResolvedEvent`.

## Secondary-approver expiration (issue #336)

Escrows above `DUAL_CONTROL_THRESHOLD` require a second signature from the
finance approver configured by `set_dual_control_config` before
`verify_delivery_and_release` can settle. `set_dual_control_timeout(admin,
escrow_id, timeout_ledgers, fallback_action)` (admin-only) arms an expiration on
that window, storing a `DualControlTimeout { approver_deadline_ledger,
fallback_action }` where the deadline is the current ledger sequence plus
`timeout_ledgers`. `fallback_action` must be `Disputed` or `Refunded` — the
timeout may only divert funds away from the seller, never authorize a release
the approver never signed. The escrow must be `Funded` and already have a
configured approver.

| Function | Purpose |
|---|---|
| `set_dual_control_timeout` | Arm (or re-arm) the deadline and pick the fallback action |
| `get_dual_control_timeout` | Read the stored `DualControlTimeout` |
| `is_approver_deadline_passed` | Read-only "is the window closed" check for relayers |
| `handle_dual_control_timeout` | Permissionless executor for the fallback |

Once `current_ledger >= approver_deadline_ledger`, `approve_release` returns
`ApproverDeadlineExpired` — a late signature cannot resurrect an order whose
fallback is due. Any address may then call
`handle_dual_control_timeout(escrow_id, caller)`, whose outcome is fully
determined by stored state:

- `Refunded` pays the buyer the full remaining balance and moves the escrow to
  `Refunded`. This deliberately bypasses `EscrowRecord::timeout_ledger`: the
  approver deadline, not the escrow timeout, is the clock that governs the
  fallback.
- `Disputed` only moves the escrow to `Disputed`, leaving settlement to the
  arbiter quorum; no funds move.

A fallback publishes `DualControlTimeoutFallbackEvent` (`("escrow", "dcfb")`)
alongside the ordinary `EscrowRefundedEvent` / `EscrowDisputedEvent`, so
existing settlement indexers stay in sync. `set_dual_control_timeout` publishes
`DualControlTimeoutSetEvent` (`("escrow", "dctmo")`). The stored deadline is
left in place after execution as an audit record; a second call is rejected by
the escrow's status.

## Storage reclamation for settled escrows

`archive_terminal_escrow(escrow_id)` reclaims the rent held by an escrow that
has finished. Once an escrow reaches a terminal state — `Released`, `Refunded`,
or `Cancelled` — it can never move again, but its persistent entries keep
paying rent indefinitely. The sweep deletes the `Escrow` record together with
every per-escrow auxiliary entry: `DisputeVotes`, `TimeoutExtensionVotes`,
`EscrowMetadataHash`, `EscrowMetadataSchema`, `ShipmentProof`,
`ReleaseCondition`, `DualControlConfig`, `EscrowYieldConfig`,
`RequireReleaseCondition`, and `LastBumpLedger`.

Two conditions must hold before anything is deleted. The escrow must be in a
terminal state — `Created`, `Funded`, and `Disputed` escrows are rejected with
`InvalidStatus` — and it must have been settled for at least
`ARCHIVAL_RETENTION_LEDGERS` (518,400 ledgers, ~30 days). The window is
measured from the terminal transition, which `updated_at` freezes because
every mutating entry point is gated on the terminal check, so a stale record
cannot be kept alive by a late bump. A `Released` or `Refunded` escrow that
still has an undrained balance is rejected rather than archived, so the sweep
can never strand funds. Arriving early returns
`ArchivalRetentionNotElapsed`; an unknown or already-archived escrow returns
`NotFound`.

The call requires no authorization. It can only ever touch state that is
already dead, so any keeper — or an automated rent sweeper — can submit it, and
a third party gains nothing by triggering it ahead of schedule.

`EscrowIds` and `BuyerEscrowAt` are left untouched on purpose: the paginated
listers already skip IDs whose record is gone, and rewriting a shared
append-only index per escrow would cost more rent than the sweep reclaims. An
archived escrow therefore reads back as `NotFound` from every getter, and
`EscrowArchivedEvent` is published on `("escrow", "archived", escrow_id)`
*before* the removals, recording what was purged.

## Development

```bash
cd escrow

# Run all tests
cargo test

# Build WASM for deployment
cargo build --target wasm32-unknown-unknown --release
```

Buyer escrow pagination uses `BuyerEscrowCount(buyer)` and one persistent
`BuyerEscrowAt(buyer, index)` entry per escrow. This keeps each storage entry
bounded as a buyer accumulates orders; `list_escrows_by_buyer` reads only the
requested page of indices.

Configure the marketplace address with `set_merchant_registry` to block new
escrows for suspended, banned, or closed merchant owners. A configured
registry failure rejects creation. Delivery releases require the admin-set
Ed25519 key and a `SignedDeliveryProof`; the legacy boolean
`evaluate_and_release` entry point remains for ABI compatibility but always
returns `SignedProofRequired`.

The oracle signs the XDR encoding of the ordered payload fields
`(escrow_id, carrier_code, tracking_hash, delivery_timestamp)`. The proof is
accepted only for the configured public key and when the delivery timestamp is
between the escrow's creation timestamp and the current ledger timestamp.

## Vetoable administrative changes (#329)

Fee rate/recipient changes, multi-treasury distributions, and token-contract
whitelist additions/removals require explicit review. This changes the behavior
of `update_fee`, `set_fee_distribution`, `add_token`, and `remove_token`: they
only apply an exact matching queued action after its review period. A `true`
execution result still means the change was applied, not merely scheduled.

1. The primary admin and designated security council authenticate
   `set_security_guardian(admin, guardian)` once. The guardian can be a multisig
   contract address. No critical action can be queued before this bootstrap.
2. An admin calls `queue_admin_action(admin, action)` and receives a unique ID.
   `AdminAction` supports `Fee`, `FeeConfig`, `FeeDistribution`, `AddToken`,
   `RemoveToken`, and `Guardian`. The proposal commits to the complete typed
   arguments with SHA-256 of their XDR encoding.
3. Inspect `get_admin_action(id)`. Execution requires **both** 17,280 additional
   ledgers and 86,400 elapsed seconds. Ledger speed cannot shorten the 24-hour
   review period; slower ledgers can extend it. Equality at both deadlines is
   sufficient. Arithmetic overflow rejects the proposal.
4. The current guardian may call `veto_admin_action(guardian, id)` any time
   before execution, including after unlock. The veto is permanent for that ID.
   Re-proposing the same payload receives a new ID and a fresh full review period.
5. A current admin calls `execute_admin_action(admin, id)`. It applies the stored
   arguments atomically and consumes the proposal, preventing replay. Calling a
   legacy setter with the exact queued arguments consumes the same approval.

Guardian rotation also requires review (`AdminAction::Guardian`) and the new
address's authentication at execution; the previous guardian can veto it.
Bootstrap cannot replace or disable an existing guardian. Admin queue/execute
permissions do not confer veto permission.

`schedule_fee_update` now queues `FeeConfig` and publishes its ID in the
`admin/queued` event. Fees no longer activate implicitly from `get_fee_config`.
The legacy `get_scheduled_fee_update` only exposes old pre-upgrade scheduling
state; use `get_admin_action` for new proposals. Any legacy pending fee update
must be re-proposed through the guardian review flow after upgrade.

Events use `(admin, queued|vetoed|executed)` topics; queued data includes the ID
and full proposal, while veto/execution data contains the ID. `(admin, guardian)`
announces bootstrap/rotation. Proposal storage has bounded TTL; archived/expired
records never grant execution permission. Keep normal contract instance TTL
maintenance in place; the guardian and monotonic ID live in instance storage.

This review policy covers escrow fee and token-contract whitelist configuration.
Existing upgrade quorum/timelock and operational emergency-pause controls remain
separate. Veto cancels a pending change; it does not undo an already executed
change. Reversing an executed setting requires another reviewed proposal.
# Opt-in Blend yield (issue #490)

Use `create` -> `configure_blend_yield` -> `fund` for a yield-bearing escrow.
The existing one-call `deposit` and batch creation APIs remain liquid custody
flows; they do not silently invest buyer funds.

`configure_blend_yield(env, escrow_id, admin, config)` requires both the
administrator and that escrow's buyer to authorize the invocation. It accepts
only a `Created` escrow without legacy APR-based yield configuration. The
recipient must be the buyer or the current primary fee treasury. Configuration
cannot subsequently be replaced, and legacy `set_yield_config` cannot be mixed
with Blend yield.

```text
YieldBearingEscrowConfig {
    lending_pool_contract: approved Blend V2 pool address,
    b_token_address: same pool address,
    accrued_yield_stroops: 0,
    yield_recipient: buyer or primary fee treasury
}
```

Blend's bTokens are non-transferable positions inside the pool, not separately
deployed SAC tokens. For that reason, `b_token_address` identifies the pool's
receipt ledger and must equal `lending_pool_contract`. The underlying reserve
asset is always the escrow's token (for example, USDC).

Funding uses Blend `submit` with a non-collateral Supply request. Each escrow
records only the increase in its own tracked shares. It authorizes the exact
nested token transfer without granting a standing allowance. No Borrow or
SupplyCollateral request is issued.

`release_funds(env, escrow_id, caller)` delegates to existing release validation,
seller payment and platform fee rules. Existing release/refund/dispute/swap/
rescue payout paths also redeem the position before using its principal.
The first payout (including a partial payout or keeper bounty) unwinds the
entire position; any remaining principal stays liquid for later payouts.
Realized interest goes to the configured recipient at this first redemption.
`get_blend_yield_config` reports this realized amount, not an APR estimate.
The old `get_accrued_yield` API remains the legacy simulated-yield view.

Redemption reads Blend V2's twelve-decimal `b_rate`, rounds the escrow's share
value down, checks the exact receipt-share decrease, and measures tokens
actually received. It aborts atomically if the pool fails or returns less than
the original principal. Other escrows' cash cannot cover that shortfall.
Funding and settlement emit `bl_supply` and `bl_redeem` escrow events.

**Limits:** lending is not risk-free or insured. Pool insolvency, paused
withdrawals or unavailable liquidity can delay settlement. Supply rounding may
also leave a sub-unit shortfall until interest accrues; the contract rejects
that settlement rather than silently reducing principal. Admin and buyer must
approve a trusted, compatible pool. This integration does not repair unrelated
legacy payout-accounting defects.

Focused module tests: `cargo test -p delego-interfaces --test blend_yield`.
They compile the actual `escrow/src/blend_yield.rs` through a small harness,
without patching the module or importing the legacy escrow contract. They
exercise an ABI-compatible mock, **not a deployed Blend pool**.
Full escrow lifecycle tests and deployable WASM verification remain blocked by
the existing malformed upstream escrow/marketplace/permissions sources.

References:
- [Blend integration guide](https://docs.blend.capital/tech-docs/integrations/integrate-pool)
- [Blend V2 request ABI](https://github.com/blend-capital/blend-contracts-v2/blob/ba22b487b2c5057a4ecc28b05b5193c28e4bd117/pool/src/pool/actions.rs)
- [Blend V2 reserve conversions](https://github.com/blend-capital/blend-contracts-v2/blob/ba22b487b2c5057a4ecc28b05b5193c28e4bd117/pool/src/pool/reserve.rs)
