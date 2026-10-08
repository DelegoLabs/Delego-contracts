use soroban_sdk::{contractclient, Address, Env, Error};

/// Cross-contract interface a lending pool must implement for use by Delego.
///
/// The escrow contract holds an `Address` identifying the configured pool and
/// drives it exclusively through the generated [`LendingPoolClient`].  The
/// non-mutating queries return plain values (the pool degrades gracefully on
/// its own), while `deposit`/`withdraw` return `Err(Error)` when the
/// underlying pool rejects the call so the caller can fall back rather than
/// hard-fail the whole escrow.
#[contractclient(name = "LendingPoolClient")]
pub trait LendingPoolInterface {
    /// Deposit `amount` of `token`; returns the shares (or new balance)
    /// received, or the error reported by the pool.
    fn deposit(env: Env, token: Address, amount: i128) -> Result<i128, Error>;

    /// Redeem `shares` of `token`; returns the underlying tokens received, or
    /// the error reported by the pool.
    fn withdraw(env: Env, token: Address, shares: i128) -> Result<i128, Error>;

    /// Total yield accrued (not principal) for `user` at the current ledger
    /// time.
    fn get_accrued_yield(env: Env, user: Address) -> i128;
}
