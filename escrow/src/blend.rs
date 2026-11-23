//! Blend protocol lending pool adapter (issue #590).
//!
//! Idle escrow capital can be supplied to a Blend V2 lending pool so that it
//! accrues interest while the escrow is open. Blend does not mint a separate
//! SAC bToken: receipt shares ("bTokens") live in the pool's
//! `Positions.supply` map, keyed by reserve index. This adapter exposes the
//! two primitive operations escrow needs, with typed [`EscrowError`] results
//! instead of host panics:
//!
//! * [`BlendLendingPoolTrait::supply_to_pool`] — deposit `amount` underlying
//!   (e.g. USDC) into the pool and return the bTokens minted to the escrow.
//! * [`BlendLendingPoolTrait::withdraw_from_pool`] — burn `btoken_amount`
//!   bTokens and return the underlying received, i.e. principal plus the
//!   interest accrued since supply.
//!
//! Every call is verified against the pool's own position and balance deltas:
//! the adapter never borrows, never touches collateral positions, and only
//! authorizes the exact nested token transfer a supply needs (no open
//! allowance is ever granted).
//!
//! The per-escrow funding / resolution lifecycle (supply on `fund`, redeem
//! before any payout) is driven by [`crate::blend_yield`], which keeps the
//! per-escrow position accounting.

use crate::EscrowError;
use delego_interfaces::blend::{underlying, BlendPoolClient, Request, SUPPLY, WITHDRAW};
use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    symbol_short, token, vec, Address, Env, IntoVal, Map, Symbol, TryFromVal, Val,
};

/// Cross-contract interface for staking escrow principal into a Blend pool.
pub trait BlendLendingPoolTrait {
    /// Supply `amount` units of `asset` from the escrow contract into `pool`.
    ///
    /// Returns the number of bTokens (supply shares) minted to the escrow.
    fn supply_to_pool(
        env: Env,
        pool: Address,
        asset: Address,
        amount: i128,
    ) -> Result<i128, EscrowError>;

    /// Burn `btoken_amount` bTokens held by the escrow in `pool` and return
    /// the underlying `asset` received (principal plus accrued interest).
    fn withdraw_from_pool(
        env: Env,
        pool: Address,
        asset: Address,
        btoken_amount: i128,
    ) -> Result<i128, EscrowError>;
}

/// Default [`BlendLendingPoolTrait`] implementation backed by the Blend V2
/// pool ABI in [`delego_interfaces::blend`].
pub struct BlendLendingPool;

fn field<T: TryFromVal<Env, Val>>(
    env: &Env,
    map: &Map<Symbol, Val>,
    name: &str,
) -> Result<T, EscrowError> {
    let value = map
        .get(Symbol::new(env, name))
        .ok_or(EscrowError::PoolNotFound)?;
    T::try_from_val(env, &value).map_err(|_| EscrowError::PoolNotFound)
}

/// Read the reserve index and current bRate for `asset` in `pool`.
fn reserve(env: &Env, pool: &BlendPoolClient, asset: &Address) -> Result<(u32, i128), EscrowError> {
    let value = match pool.try_get_reserve(asset) {
        Ok(Ok(value)) => value,
        _ => return Err(EscrowError::PoolNotFound),
    };
    let actual: Address = field(env, &value, "asset")?;
    let cfg: Map<Symbol, Val> = field(env, &value, "config")?;
    let data: Map<Symbol, Val> = field(env, &value, "data")?;
    let index: u32 = field(env, &cfg, "index")?;
    let rate: i128 = field(env, &data, "b_rate")?;
    if actual != *asset || rate <= 0 {
        return Err(EscrowError::PoolNotFound);
    }
    Ok((index, rate))
}

fn positions(
    pool: &BlendPoolClient,
    owner: &Address,
) -> Result<delego_interfaces::blend::Positions, EscrowError> {
    match pool.try_get_positions(owner) {
        Ok(Ok(value)) => Ok(value),
        _ => Err(EscrowError::PoolNotFound),
    }
}

fn validate_addresses(env: &Env, pool: &Address, asset: &Address) -> Result<(), EscrowError> {
    let this = env.current_contract_address();
    if *pool == this || *asset == this || pool == asset {
        return Err(EscrowError::InvalidAddress);
    }
    Ok(())
}

impl BlendLendingPoolTrait for BlendLendingPool {
    fn supply_to_pool(
        env: Env,
        pool: Address,
        asset: Address,
        amount: i128,
    ) -> Result<i128, EscrowError> {
        if amount <= 0 {
            return Err(EscrowError::ZeroAmount);
        }
        validate_addresses(&env, &pool, &asset)?;
        let owner = env.current_contract_address();
        let client = BlendPoolClient::new(&env, &pool);
        let (index, _) = reserve(&env, &client, &asset)?;
        let before = positions(&client, &owner)?;
        let previous = before.supply.get(index).unwrap_or(0);
        let token = token::Client::new(&env, &asset);
        let balance = token.balance(&owner);
        if balance < amount {
            return Err(EscrowError::InsufficientEscrowBalance);
        }

        // Authorize only the exact nested transfer escrow -> pool.
        env.authorize_as_current_contract(vec![
            &env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: asset.clone(),
                    fn_name: symbol_short!("transfer"),
                    args: (owner.clone(), pool.clone(), amount).into_val(&env),
                },
                sub_invocations: vec![&env],
            }),
        ]);
        let requests = vec![
            &env,
            Request {
                request_type: SUPPLY,
                address: asset.clone(),
                amount,
            },
        ];
        let after = match client.try_submit(&owner, &owner, &owner, &requests) {
            Ok(Ok(value)) => value,
            _ => return Err(EscrowError::BlendSupplyFailed),
        };

        let minted = after
            .supply
            .get(index)
            .unwrap_or(0)
            .checked_sub(previous)
            .ok_or(EscrowError::MathOverflow)?;
        let spent = balance
            .checked_sub(token.balance(&owner))
            .ok_or(EscrowError::MathOverflow)?;
        if minted <= 0
            || spent != amount
            || before.liabilities != after.liabilities
            || before.collateral != after.collateral
        {
            return Err(EscrowError::BlendSupplyFailed);
        }

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("bla_sup")),
            (pool, asset, amount, minted),
        );
        Ok(minted)
    }

    fn withdraw_from_pool(
        env: Env,
        pool: Address,
        asset: Address,
        btoken_amount: i128,
    ) -> Result<i128, EscrowError> {
        if btoken_amount <= 0 {
            return Err(EscrowError::ZeroAmount);
        }
        validate_addresses(&env, &pool, &asset)?;
        let owner = env.current_contract_address();
        let client = BlendPoolClient::new(&env, &pool);
        let (index, rate) = reserve(&env, &client, &asset)?;
        let before = positions(&client, &owner)?;
        let previous = before.supply.get(index).unwrap_or(0);
        if previous < btoken_amount {
            return Err(EscrowError::InsufficientPoolBalance);
        }
        // Blend withdrawals are denominated in underlying units: convert the
        // bTokens at the current bRate, which already includes accrued interest.
        let quoted = underlying(btoken_amount, rate).ok_or(EscrowError::MathOverflow)?;
        if quoted <= 0 {
            return Err(EscrowError::BlendWithdrawFailed);
        }

        let token = token::Client::new(&env, &asset);
        let balance = token.balance(&owner);
        let requests = vec![
            &env,
            Request {
                request_type: WITHDRAW,
                address: asset.clone(),
                amount: quoted,
            },
        ];
        let after = match client.try_submit(&owner, &owner, &owner, &requests) {
            Ok(Ok(value)) => value,
            _ => return Err(EscrowError::BlendWithdrawFailed),
        };

        let burnt = previous
            .checked_sub(after.supply.get(index).unwrap_or(0))
            .ok_or(EscrowError::MathOverflow)?;
        let received = token
            .balance(&owner)
            .checked_sub(balance)
            .ok_or(EscrowError::MathOverflow)?;
        if burnt <= 0
            || burnt > btoken_amount
            || received < quoted
            || before.liabilities != after.liabilities
            || before.collateral != after.collateral
        {
            return Err(EscrowError::BlendWithdrawFailed);
        }

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("bla_wdr")),
            (pool, asset, burnt, received),
        );
        Ok(received)
    }
}

/// Split a redemption into `(principal, interest)`.
///
/// Fails with [`EscrowError::BlendWithdrawFailed`] when the pool returned less
/// than the original principal, so a loss is never silently passed on.
pub fn split_principal_and_interest(
    principal: i128,
    received: i128,
) -> Result<(i128, i128), EscrowError> {
    if principal < 0 || received < principal {
        return Err(EscrowError::BlendWithdrawFailed);
    }
    let interest = received
        .checked_sub(principal)
        .ok_or(EscrowError::MathOverflow)?;
    Ok((principal, interest))
}
