# Changelog

All notable changes to the Delego smart contracts are documented here, per
contract. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## How versions are tracked

Each contract's version mirrors the semver exposed by its on-chain `version()`
entry point, falling back to the crate version in `Cargo.toml` for contracts
that do not expose `version()` (e.g. `delegation_registry`).

[`.github/workflows/changelog.yml`](.github/workflows/changelog.yml) (via
[`scripts/check-changelog.sh`](scripts/check-changelog.sh)) fails CI when a
contract's declared version has no matching entry in this file. If you bump a
contract's version in code, record the bump here in the same PR — otherwise CI
will reject the change.

## escrow (delego-escrow)

### Unreleased

- Add dynamic platform fee tiering based on merchant settled volume (issue #328): admin-configured
  `FeeTier` ladder, per-merchant volume accumulation on every successful release, tier-aware fee
  computation on escrow release, and a `MerchantVolumeTierUpdatedEvent` emitted when a merchant
  reaches a higher volume bracket. Adds `set_fee_tiers`, `remove_fee_tier`, `get_fee_tiers`,
  `get_merchant_settled_volume`, `get_merchant_tier`, and `get_effective_fee_bps` entry points
  with `InvalidTier`/`TierLimitExceeded`/`TierNotFound` error codes.
- Fix pre-existing build breakage: declare the missing `EscrowError` variants used by the
  multi-sig upgrade and merchant-category validation paths, add the missing `DataKey::MerkleRoot`
  storage key, correct an `into_val` call in merchant-category validation, and skip the XDR spec
  export for `EscrowError` (the spec format caps error enums at 50 cases).
- Require a 24-hour vetoable review for escrow fee and token-whitelist changes, with authenticated guardian setup/rotation and explicit single-use execution (#329).

- Add immutable daily delivery Merkle roots and order-bound inclusion-proof escrow release.
- Add atomic batch escrow creation with per-token aggregate allowance transfers.
- Add admin split dispute settlements with buyer, seller, and mediator payouts.
- Delegate accrued-yield reads to the escrow's configured external lending pool
  via the shared `LendingPoolInterface` (`delego-interfaces`), falling back to
  the internal APR estimate when the pool is unreachable, paused, or reports a
  non-positive figure (issue #326).
- Add buyer-committed slippage bounds for cross-currency DEX path payments, with
  tighten-only windows, route validation, and settlement that reverts on realized
  amounts outside the committed window.
- Add secondary-approver expiration for dual-control escrows: an admin-armed
  deadline whose fallback disputes or refunds the order when the finance
  approver does not sign in time.
- Add `archive_terminal_escrow(escrow_id)`, a permissionless storage-recovery
  sweep that deletes the record and every auxiliary entry of an escrow
  (`Escrow`, `DisputeVotes`, `TimeoutExtensionVotes`, `EscrowMetadataHash`,
  `EscrowMetadataSchema`, `ShipmentProof`, `ReleaseCondition`,
  `DualControlConfig`, `EscrowYieldConfig`, `RequireReleaseCondition`,
  `LastBumpLedger`) once it has held a terminal state — `Released`,
  `Refunded`, `Cancelled` — for `ARCHIVAL_RETENTION_LEDGERS` (518,400
  ledgers, ~30 days), reclaiming the rent a settled escrow would otherwise pin
  down forever. The window runs from the terminal transition, which
  `updated_at` freezes because every mutating path is gated on
  `check_not_terminal`. Active (`Created`, `Funded`) and `Disputed` escrows are
  rejected with `EscrowError::InvalidStatus`, and an unsettled payout-terminal
  escrow is rejected rather than archived, so the sweep can never drop a live
  record. The shared `EscrowIds` and `BuyerEscrowAt` indexes are left intact and
  the paginated listers already skip ids whose record is gone, so an archived
  escrow reads back as `NotFound` rather than panicking an index lookup. Emits
  `EscrowArchivedEvent` on the `(escrow, archived, escrow_id)` topic *before*
  the removals, and reports the reclaimed entry count. New
  `EscrowError::ArchivalRetentionNotElapsed` for a sweep that arrives
  before the window has passed.
- Prevent sellers from front-running buyer deposits: `cancel` is now guarded by a
  snapshotted, admin-configurable protection window (default 10 ledgers) that can
  only be cleared by the buyer's on-chain agreement (`agree_cancel`) or the
  escrow timeout. Adds `accept_order`, `get_order_acceptance`,
  `get_cancel_eligibility`, `get_cancel_lockout`, and `set_cancel_lockout`, plus
  the new `CancelLockoutActive` (411) and `InvalidCancelLockout` (412) errors.
- Enforce a configurable minimum release fee floor so small escrows can no longer
  truncate the platform fee to zero and evade it by fragmenting a payment.

### 0.2.0 - 2026-08-29

- Initial tracked release for this contract. On-chain `version()` returns `0.2.0`
  (escrow lifecycle: create/deposit/release/refund/dispute/cancel, receipts,
  timeouts, fee config, multi-admin).

## marketplace (delego-marketplace)

### Unreleased

### 0.0.1 - 2026-09-30

- Fix pre-existing build breakage: restore missing `[features]` table header in `marketplace/Cargo.toml` so the `testutils` feature is correctly declared.

### 0.2.0 - 2026-08-29

- Initial tracked release for this contract. On-chain `version()` returns `0.2.0`
  (merchant registry and discovery: registration, multi-verifier verification,
  category/name discovery, commission config, metadata cooldown, suspend/close,
  reputation score pairing).

## permissions (delego-permissions)

### Unreleased

- Add `invalidate_nonce_range` entrypoint to bulk-invalidate all relayer nonces up to and including a given nonce for a compromised agent key recovery flow (issue #335). Owner-authorized; emits `NonceBatchInvalidatedEvent` and writes an audit log entry.
- Added an asynchronous multi-signature spend approval queue (issue #377).
  `execute_spend_multi` requires every co-signer to sign in one transaction, so
  an enterprise approval that takes hours is impossible to submit. The new
  `propose_spend` / `approve_spend_proposal` pair splits the same quorum across
  time instead: the delegate queues a `PendingSpendProposal`, each registered
  owner co-signs in a transaction of its own, and the spend settles
  automatically inside the approval that reaches the grant's `threshold` —
  no separate execution step, and the existing `MultiOwnerSpendEvent` is
  emitted so indexers need no new path. The co-signing window is the new
  `SPEND_PROPOSAL_TTL_LEDGERS` (17,280 ledgers, ~24 hours), and each delegate
  may hold at most `MAX_PENDING_SPEND_PROPOSALS` (5) live proposals. The grant
  is re-validated at every co-signature, so a proposal queued before the grant
  was paused, expired, re-whitelisted or partly drained by other spends cannot
  settle through it, and `approvals` only ever holds distinct registered owners
  so no co-signer can sign twice to manufacture a quorum. `approve_spend_proposal`
  takes the co-signer as an explicit argument and requires its auth, because
  `soroban-sdk` exposes no invoker to read the signer from.
  `cancel_spend_proposal` (delegate or any grant owner) withdraws an unsettled
  proposal, and `get_spend_proposal` / `get_spend_proposals` expose the queue.
  New errors: `ProposalNotFound` (2418), `ProposalAlreadyExecuted` (2419),
  `ProposalExpired` (2420), `ProposalAlreadyApproved` (2421),
  `NoMultiOwnerGrant` (2422), `AmbiguousMultiOwnerGrant` (2423) and
  `TooManyPendingProposals` (2424). The queue stores proposals under new
  `DataKey` slots, so existing grant records are unchanged; the new
  `("perm", "mgrant")` bookkeeping only adds a delegate→owner index used to
  resolve which grant a queued spend draws on.
- Added function-restricted permission grants (issue #369). A new
  `ScopedPermissionConfig { target_contract, allowed_function_symbols }` limits
  a delegation to one contract and an explicit list of entrypoints, so an owner
  can authorise an agent for `escrow.fund` only and refuse every other
  contract method. The scope is enforced in the new
  `can_spend_scoped` / `execute_spend_scoped` entrypoints and **fails closed**:
  the pre-existing `can_spend` / `execute_spend` entrypoints reject a scoped
  grant with the new `PermissionError::UnauthorizedFunction` (2414), so the
  check cannot be bypassed by omitting the invoked function. `grant_child`
  sub-delegations inherit their parent's scope and `transfer_permission` carries
  it to the incoming delegate, closing lateral privilege escalation along the
  parent chain. `allowed_function_symbols` is bounded by the new
  `MAX_FUNCTIONS_PER_PERMISSION` (10) and must be non-empty and duplicate-free.
  New `grant_scoped` / `re_grant_scoped` / `set_permission_scope` /
  `get_permission_scope` entry points and a `("perm", "scope")` event back the
  feature. `re_grant` clears an existing scope while `re_grant_with_metadata`
  preserves it, so a limit bump cannot silently widen a delegate's authority.
  The scope lives under its own `DataKey::PermissionScope` slot rather
  than inside `PermissionRecord`, so the serialized shape of existing
  permissions is unchanged and pre-existing delegations keep loading. Since
  `RelayedSpendMessage` carries no entrypoint field, a scoped grant is
  deliberately rejected by `execute_spend_via_relayer` rather than being
  allowed through unchecked.

### 0.1.0 - 2026-08-29

- Initial tracked release for this contract. On-chain `version()` returns `0.1.0`
  (delegated spending authority: grants, allowances, per-tx limits, relayed
  gasless spends, multi-owner grants, pause controls).

## reputation (delego-reputation)

### Unreleased

- Expose a fixed-point half-life decay helper that reduces stale scores toward zero.

### 0.0.1 - 2026-08-29

- Initial tracked release for this contract. `version()` mirrors the crate
  version (`0.0.1`): time-decayed reputation scores driven by transaction
  outcomes and ratings.

## delegation_registry (delego-delegation-registry)

### Unreleased

- Packed a delegation's four capabilities (spend, refund, dispute, delegate) into
  a single `u32` `PermissionBitmask` instead of storing them as individual
  booleans (issue #322). A delegation entry shrinks by ~72 B and reading or
  rewriting the capability set costs ~65% fewer CPU instructions, since the
  flags occupy one storage round-trip instead of four. `create_delegation`
  keeps its signature and still grants every capability; the new
  `create_scoped_delegation`, `set_permission_flag`, `clear_permission_flag`,
  `get_permissions`, `has_permission` and `is_authorized_for` entry points
  expose the scope. New `DelegationError::InvalidPermissionFlag` (315) rejects
  reserved and zero bits so a delegation can never carry capabilities the
  contract cannot interpret.

### 0.0.1 - 2026-08-29

- Initial tracked release for this contract. No on-chain `version()` entry point,
  so the crate version (`0.0.1`) is tracked here: delegation records with expiry
  and versioned rollback/upgrade support.

## interfaces (delego-interfaces)

### Unreleased

- Initial tracked release for this library (issue #326). Defines the
  shared `LendingPoolInterface` trait and its generated `LendingPoolClient`
  cross-contract adapter, plus the `MockLendingPool` test double (with
  `testutils` feature) for locally simulating external yield accrual. Escrow
  delegates `get_accrued_yield` reads to this interface.
