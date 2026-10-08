//! Property-based fuzz harness for the spend-allowance arithmetic and
//! time-locked allowance-decrement paths (issue #323).
//!
//! This workspace does not use `cargo-fuzz`/`libFuzzer` anywhere (no `fuzz/`
//! crate, no `libfuzzer-sys` in `Cargo.lock`), so rather than introducing a
//! new fuzzing framework from nothing, this harness follows the same
//! `Arbitrary`-based input generation `cargo-fuzz` itself is built on (the
//! `arbitrary` crate is already present in `Cargo.lock`, pulled in
//! transitively via `soroban-sdk`/`stellar-xdr`), but drives it from a
//! plain `#[cfg(test)]` module gated behind the `arbitrary` dev-dependency,
//! matching every other test module in this crate (`mod test;`,
//! `mod integration_tests;`).
//!
//! Each iteration seeds a small deterministic PRNG (no `rand` dependency
//! exists in this workspace either), turns the resulting bytes into an
//! [`Unstructured`] buffer, and lets `arbitrary` derive a
//! [`FuzzSpendOperation`] from it exactly as a `cargo-fuzz` target would.
//! The generated operation then drives the *real* contract entry points
//! (`grant`, `execute_spend`, `decrease_allowance`,
//! `execute_decrease_allowance`) through `PermissionsContractClient`, and
//! every outcome is checked against the invariants called out in issue
//! #323:
//!
//! - remaining allowance (`limit_total - spent`) never goes negative,
//! - `spent` always equals the exact sum of the spends that were actually
//!   accepted,
//! - a time-locked allowance decrease can never execute before its
//!   time-lock elapses, and never leaves `limit_total < spent`,
//! - no fuzzed input causes a host-level panic/trap (checked arithmetic
//!   throughout `apply_spend`/`decrease_allowance` means every invalid
//!   input must surface as a typed `PermissionError`, never a panic).

#[cfg(test)]
#[allow(clippy::module_inception)]
mod fuzz_tests {
    use crate::{PermissionError, PermissionsContract, PermissionsContractClient};
    use arbitrary::{Arbitrary, Unstructured};
    use soroban_sdk::testutils::{Address as _, Ledger};
    use soroban_sdk::Address;
    use soroban_sdk::Env;

    /// Mirrors the data shape given in issue #323's acceptance criteria.
    #[derive(Arbitrary, Debug, Clone)]
    struct FuzzSpendOperation {
        initial_limit: i128,
        spends: Vec<i128>,
        elapsed_ledgers: Vec<u32>,
    }

    /// Tiny deterministic xorshift64 PRNG so the harness needs no `rand`
    /// dependency (none exists anywhere in this workspace) while still
    /// producing a fresh, reproducible byte stream per iteration.
    fn xorshift64(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    fn gen_bytes(seed: u64, len: usize) -> Vec<u8> {
        // xorshift64 is undefined for a zero state; force it odd.
        let mut state = seed | 1;
        let mut bytes = Vec::with_capacity(len);
        while bytes.len() < len {
            bytes.extend_from_slice(&xorshift64(&mut state).to_le_bytes());
        }
        bytes.truncate(len);
        bytes
    }

    /// Clamp a fuzzed `i128` into `[0, bound]` without ever overflowing —
    /// `i128::MIN.unsigned_abs()` is `2^127`, which does not fit back into
    /// an `i128`, so the modulo reduction happens entirely in `u128` before
    /// the (now small and safe) result is cast down.
    fn clamp_nonneg(value: i128, bound: i128) -> i128 {
        let bound_u = bound as u128;
        let reduced = value.unsigned_abs() % (bound_u + 1);
        reduced as i128
    }

    /// Fuzzes `execute_spend` against a single freshly granted permission,
    /// asserting the core accounting invariants from issue #323: `spent`
    /// tracks exactly the accepted spends, remaining allowance never goes
    /// negative, and no input panics the contract.
    #[test]
    fn fuzz_spend_allowance_invariants() {
        const ITERATIONS: u64 = 100_000;

        for i in 0..ITERATIONS {
            let raw = gen_bytes(0xC0FFEE_u64 ^ i, 512);
            let mut u = Unstructured::new(&raw);
            let op = match FuzzSpendOperation::arbitrary(&mut u) {
                Ok(op) => op,
                Err(_) => continue,
            };

            // A single per-tx limit equal to the total allowance isolates
            // the total-limit / overflow invariant under test from the
            // (separately covered) per-tx limit check.
            let initial_limit = clamp_nonneg(op.initial_limit, 1_000_000_000_000i128) + 1;

            let env = Env::default();
            env.mock_all_auths();
            let owner = Address::generate(&env);
            let delegate = Address::generate(&env);
            let merchant = Address::generate(&env);
            let contract_id = env.register(PermissionsContract, ());
            let client = PermissionsContractClient::new(&env, &contract_id);
            let merchants = soroban_sdk::Vec::<Address>::new(&env);

            client.grant(
                &owner,
                &delegate,
                &initial_limit,
                &initial_limit,
                &merchants,
                &1_000_000u32,
            );

            let mut expected_spent: i128 = 0;
            let mut ledger_seq = env.ledger().sequence();

            for (idx, raw_spend) in op.spends.iter().enumerate() {
                if let Some(elapsed) = op.elapsed_ledgers.get(idx) {
                    // Bound the per-step advance so u32 sequence arithmetic
                    // in the contract (e.g. expiry math) cannot itself
                    // overflow across a single fuzz case.
                    ledger_seq = ledger_seq.saturating_add(elapsed % 1_000);
                    env.ledger().set_sequence_number(ledger_seq);
                }

                let spend_amount = *raw_spend;
                let result = client.try_execute_spend(&owner, &delegate, &spend_amount, &merchant);

                match result {
                    Ok(Ok(())) => {
                        assert!(
                            spend_amount > 0,
                            "a non-positive spend must never be accepted (op={:?})",
                            op
                        );
                        expected_spent =
                            expected_spent.checked_add(spend_amount).unwrap_or_else(|| {
                                panic!(
                                    "an accepted spend must never overflow i128 given a bounded \
                                 initial_limit (op={:?})",
                                    op
                                )
                            });
                    }
                    Ok(Err(_)) => {
                        // Cleanly rejected via a typed PermissionError:
                        // state must be unchanged.
                    }
                    Err(e) => panic!(
                        "execute_spend must never panic/host-error, got {:?} (op={:?})",
                        e, op
                    ),
                }

                let record = client.get_permission(&owner, &delegate);
                assert_eq!(
                    record.spent, expected_spent,
                    "spent must equal the exact sum of accepted spends (op={:?})",
                    op
                );
                assert!(
                    record.spent <= record.limit_total,
                    "remaining allowance must never go negative (op={:?})",
                    op
                );
                assert!(
                    record.spent >= 0,
                    "spent must never go negative (op={:?})",
                    op
                );
            }
        }
    }

    /// Fuzzes the time-locked `decrease_allowance` / `execute_decrease_allowance`
    /// pair, asserting: a non-positive decrement is always rejected, a
    /// pending decrement can never execute before its time-lock elapses, and
    /// a completed decrement never leaves `limit_total < spent`.
    #[test]
    fn fuzz_allowance_decrement_timelock_invariants() {
        const ITERATIONS: u64 = 100_000;

        for i in 0..ITERATIONS {
            let raw = gen_bytes(0xDEC0DE_u64 ^ i, 256);
            let mut u = Unstructured::new(&raw);
            let op = match FuzzSpendOperation::arbitrary(&mut u) {
                Ok(op) => op,
                Err(_) => continue,
            };

            let initial_limit = clamp_nonneg(op.initial_limit, 1_000_000_000_000i128) + 1;

            let env = Env::default();
            env.mock_all_auths();
            let owner = Address::generate(&env);
            let delegate = Address::generate(&env);
            let merchant = Address::generate(&env);
            let contract_id = env.register(PermissionsContract, ());
            let client = PermissionsContractClient::new(&env, &contract_id);
            let merchants = soroban_sdk::Vec::<Address>::new(&env);

            client.grant(
                &owner,
                &delegate,
                &initial_limit,
                &initial_limit,
                &merchants,
                &1_000_000u32,
            );

            // Optionally pre-spend a bounded amount so `spent > 0` is
            // exercised by the `LimitBelowSpent` guard below.
            let mut spent: i128 = 0;
            if let Some(first) = op.spends.first() {
                let candidate = clamp_nonneg(*first, initial_limit);
                if candidate > 0
                    && client
                        .try_execute_spend(&owner, &delegate, &candidate, &merchant)
                        .is_ok()
                {
                    spent = candidate;
                }
            }

            // A raw, possibly out-of-range decrement amount (zero, negative,
            // or larger than the whole limit) exercises the validation path.
            let raw_decrement = op.spends.get(1).copied().unwrap_or(0);

            let decrease_result = client.try_decrease_allowance(&owner, &delegate, &raw_decrement);

            if raw_decrement <= 0 {
                assert!(
                    matches!(decrease_result, Ok(Err(PermissionError::InvalidParam))),
                    "a non-positive decrement must always be rejected, got {:?} (op={:?})",
                    decrease_result,
                    op
                );
                continue;
            }

            let record_before = client.get_permission(&owner, &delegate);

            match decrease_result {
                Ok(Ok(())) => {
                    // A pending decrement now exists — executing it before
                    // the time-lock elapses must fail every time.
                    let early = client.try_execute_decrease_allowance(&owner, &delegate);
                    assert!(
                        matches!(early, Ok(Err(PermissionError::TimeLockActive))),
                        "a pending decrement must not execute before its time-lock \
                         elapses, got {:?} (op={:?})",
                        early,
                        op
                    );

                    let timelock_secs = client.get_decrease_timelock_secs();
                    let now = env.ledger().timestamp();
                    env.ledger()
                        .set_timestamp(now.saturating_add(timelock_secs).saturating_add(1));

                    match client.try_execute_decrease_allowance(&owner, &delegate) {
                        Ok(Ok(())) => {
                            let record_after = client.get_permission(&owner, &delegate);
                            assert_eq!(
                                record_after.limit_total,
                                record_before.limit_total - raw_decrement,
                                "executed decrement must reduce limit_total by exactly the \
                                 requested amount (op={:?})",
                                op
                            );
                            assert!(
                                record_after.limit_total >= record_after.spent,
                                "a completed decrement must never leave limit_total below \
                                 spent (op={:?})",
                                op
                            );
                        }
                        Ok(Err(PermissionError::LimitBelowSpent)) => {
                            assert!(
                                record_before.limit_total - raw_decrement < spent,
                                "LimitBelowSpent must only be returned when the decrement \
                                 would truly drop the limit under spent (op={:?})",
                                op
                            );
                        }
                        other => panic!(
                            "unexpected execute_decrease_allowance outcome {:?} (op={:?})",
                            other, op
                        ),
                    }
                }
                Ok(Err(PermissionError::LimitBelowSpent)) => {
                    assert!(
                        record_before.limit_total - raw_decrement < spent,
                        "LimitBelowSpent must only be returned when the decrement would \
                         truly drop the limit under spent (op={:?})",
                        op
                    );
                }
                other => panic!(
                    "unexpected decrease_allowance outcome {:?} (op={:?})",
                    other, op
                ),
            }
        }
    }
}
