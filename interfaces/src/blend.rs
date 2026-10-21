//! Blend V2 pool ABI. bTokens are entries in `Positions.supply`, not SAC tokens.
//! See https://github.com/blend-capital/blend-contracts-v2/tree/ba22b487b2c5057a4ecc28b05b5193c28e4bd117/pool/src/pool.
use soroban_sdk::{contractclient, contracttype, Address, Env, Map, Symbol, Val, Vec};

/// Non-collateral supply request.
pub const SUPPLY: u32 = 0;
/// Withdrawal of non-collateral supply, denominated in underlying units.
pub const WITHDRAW: u32 = 1;
/// Blend V2 bRate uses twelve decimal places.
pub const RATE_SCALE: i128 = 1_000_000_000_000;

/// One action accepted by Blend's `submit`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Request {
    /// Supply (0) or withdraw (1) for this integration.
    pub request_type: u32,
    /// Underlying reserve asset.
    pub address: Address,
    /// Underlying units, not bToken units.
    pub amount: i128,
}

/// Blend's position maps, keyed by reserve index.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Positions {
    /// Borrowed positions; this integration never creates them.
    pub liabilities: Map<u32, i128>,
    /// Collateral positions; this integration never creates them.
    pub collateral: Map<u32, i128>,
    /// Non-collateral bToken balances.
    pub supply: Map<u32, i128>,
}

/// Actual Blend V2 entry points used by escrow.
#[contractclient(name = "BlendPoolClient")]
pub trait BlendPoolInterface {
    /// Return updated positions after an atomic batch.
    fn submit(
        env: Env,
        from: Address,
        spender: Address,
        to: Address,
        requests: Vec<Request>,
    ) -> Positions;
    /// Read the internal receipt balances.
    fn get_positions(env: Env, address: Address) -> Positions;
    /// Decode only needed fields from Blend's extensible contract-struct map:
    /// `asset`, `config.index`, and `data.b_rate`.
    fn get_reserve(env: Env, asset: Address) -> Map<Symbol, Val>;
}

/// Convert receipt shares to underlying units, rounding down.
///
/// Splitting the product avoids overflowing `shares * rate` when the final
/// result is representable. Rejects invalid rates and unrepresentable results.
pub fn underlying(shares: i128, rate: i128) -> Option<i128> {
    if shares < 0 || rate <= 0 {
        return None;
    }
    let whole = shares.checked_div(RATE_SCALE)?.checked_mul(rate)?;
    let remainder = shares.checked_rem(RATE_SCALE)?;
    let fraction = remainder
        .checked_mul(rate.checked_div(RATE_SCALE)?)?
        .checked_add(
            remainder
                .checked_mul(rate.checked_rem(RATE_SCALE)?)?
                .checked_div(RATE_SCALE)?,
        )?;
    whole.checked_add(fraction)
}
