## Summary

Fixes #109. Introduces `ReputationResolution` type to distinguish between a missing reputation setup (`NotConfigured`) and a failed reputation query (`CallFailed`) instead of mapping both to `None`. Adds `get_merchant_view_detailed` and gracefully falls back to `Some(MerchantView)` for backwards compatibility.

## Type of change

- [x] Bug fix
- [x] New feature
- [ ] Breaking change
- [ ] Documentation

## Test plan

- [x] `cargo fmt --all -- --check`
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] `cargo test --workspace`
- [x] Manual testing (describe): Verified `test_reputation_resolution_states` locally.

## Checklist

- [x] Follows project code conventions
- [x] TODOs reference issues where applicable
- [x] No secrets or credentials committed
