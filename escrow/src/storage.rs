//! Storage utilities for the escrow contract.
//!
//! This module provides transient storage-based reentrancy guards to protect
//! against cross-contract reentrancy attacks during external token calls.

use soroban_sdk::{contracttype, Env};

/// Reentrancy status for transient storage guard.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReentrancyStatus {
    /// The guard is not currently held by any operation.
    NotEntered,
    /// The guard is held by an in-flight external call.
    Entered,
}

/// Storage key for the transient reentrancy guard.
#[contracttype]
pub enum ReentrancyKey {
    /// Global reentrancy guard for the contract instance.
    Guard,
}

/// A reentrancy guard using transient storage.
///
/// Transient storage is ideal for reentrancy guards because:
/// - It automatically expires, preventing permanent lockout if a contract
///   fails to clean up (e.g., due to a panic or trap).
/// - It's cheaper than persistent storage for short-lived locks.
/// - It's scoped to the current transaction context.
pub struct ReentrancyGuard;

impl ReentrancyGuard {
    /// The TTL (in ledgers) for the transient reentrancy guard entry.
    ///
    /// Set to a small value (e.g., 10 ledgers ~ 50 seconds) to ensure
    /// automatic cleanup if the guard isn't explicitly released.
    const GUARD_TTL: u32 = 10;

    /// Attempts to acquire the reentrancy guard.
    ///
    /// Returns `Ok(())` if the guard was successfully acquired (was `NotEntered`).
    /// Returns `Err(ReentrancyStatus::Entered)` if the guard is already held.
    pub fn try_acquire(env: &Env) -> Result<(), ReentrancyStatus> {
        let storage = env.storage().transient();
        let status: ReentrancyStatus = storage
            .get(&ReentrancyKey::Guard)
            .unwrap_or(ReentrancyStatus::NotEntered);

        if status == ReentrancyStatus::Entered {
            return Err(ReentrancyStatus::Entered);
        }

        storage.set(&ReentrancyKey::Guard, &ReentrancyStatus::Entered);
        storage.extend_ttl(&ReentrancyKey::Guard, Self::GUARD_TTL, Self::GUARD_TTL);
        Ok(())
    }

    /// Releases the reentrancy guard, resetting it to `NotEntered`.
    ///
    /// This should be called in a `finally`-like pattern (using `drop` or
    /// explicit cleanup) to ensure the guard is always released.
    pub fn release(env: &Env) {
        let storage = env.storage().transient();
        storage.set(&ReentrancyKey::Guard, &ReentrancyStatus::NotEntered);
        // No need to extend TTL for NotEntered - it will expire naturally
    }

    /// Checks if the guard is currently held (for testing/debugging).
    pub fn is_held(env: &Env) -> bool {
        let storage = env.storage().transient();
        let status: ReentrancyStatus = storage
            .get(&ReentrancyKey::Guard)
            .unwrap_or(ReentrancyStatus::NotEntered);
        status == ReentrancyStatus::Entered
    }
}

/// RAII guard that automatically releases the reentrancy lock on drop.
///
/// Usage:
/// ```ignore
/// let _guard = ReentrancyGuard::acquire(env)?;
/// // ... perform external calls ...
/// // Guard automatically released when _guard goes out of scope
/// ```
pub struct ReentrancyGuardHandle<'a> {
    env: &'a Env,
    acquired: bool,
}

impl<'a> ReentrancyGuardHandle<'a> {
    /// Attempts to acquire the reentrancy guard, returning a handle that
    /// automatically releases it on drop.
    pub fn acquire(env: &'a Env) -> Result<Self, ReentrancyStatus> {
        ReentrancyGuard::try_acquire(env)?;
        Ok(Self {
            env,
            acquired: true,
        })
    }
}

impl<'a> Drop for ReentrancyGuardHandle<'a> {
    fn drop(&mut self) {
        if self.acquired {
            ReentrancyGuard::release(self.env);
        }
    }
}

/// Executes a closure under the reentrancy guard.
///
/// This is a convenience function that acquires the guard, executes the
/// closure, and ensures the guard is released even if the closure returns
/// an error or panics.
///
/// # Example
/// ```ignore
/// with_reentrancy_guard(env, || {
///     // This code is protected against reentrancy
///     token_client.transfer_from(...);
///     // State mutations here (Checks-Effects-Interactions)
///     Ok(())
/// })
/// ```
pub fn with_reentrancy_guard<F, T, E>(env: &Env, f: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<ReentrancyStatus>,
{
    let _guard = ReentrancyGuardHandle::acquire(env).map_err(E::from)?;
    f()
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Env};

    #[test]
    fn test_reentrancy_guard_acquire_release() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::EscrowContract);

        env.as_contract(&contract_id, || {
            // Initially not held
            assert!(!ReentrancyGuard::is_held(&env));

            // Acquire guard
            let _guard = ReentrancyGuardHandle::acquire(&env).unwrap();
            assert!(ReentrancyGuard::is_held(&env));

            // Second acquire should fail
            assert!(ReentrancyGuardHandle::acquire(&env).is_err());

            // Guard released on drop
        });

        // After scope exit, guard should be released
        env.as_contract(&contract_id, || {
            assert!(!ReentrancyGuard::is_held(&env));
            let _guard = ReentrancyGuardHandle::acquire(&env).unwrap();
        });
        env.as_contract(&contract_id, || {
            assert!(!ReentrancyGuard::is_held(&env));
        });
    }

    #[test]
    fn test_with_reentrancy_guard_success() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::EscrowContract);

        let result = env.as_contract(&contract_id, || {
            with_reentrancy_guard(&env, || Ok::<i32, ReentrancyStatus>(42))
        });

        assert_eq!(result, Ok(42));
        env.as_contract(&contract_id, || {
            assert!(!ReentrancyGuard::is_held(&env));
        });
    }

    #[test]
    fn test_with_reentrancy_guard_error() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::EscrowContract);

        let result = env.as_contract(&contract_id, || {
            with_reentrancy_guard(&env, || {
                Err::<i32, ReentrancyStatus>(ReentrancyStatus::Entered)
            })
        });

        assert_eq!(result, Err(ReentrancyStatus::Entered));
        env.as_contract(&contract_id, || {
            assert!(!ReentrancyGuard::is_held(&env));
        });
    }

    #[test]
    fn test_with_reentrancy_guard_reentrant_call_fails() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::EscrowContract);

        let result = env.as_contract(&contract_id, || {
            with_reentrancy_guard(&env, || {
                // Nested call should fail
                with_reentrancy_guard(&env, || Ok::<i32, ReentrancyStatus>(1))
            })
        });

        assert_eq!(result, Err(ReentrancyStatus::Entered));
        env.as_contract(&contract_id, || {
            assert!(!ReentrancyGuard::is_held(&env));
        });
    }
}
