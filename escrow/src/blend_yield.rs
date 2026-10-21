//! Opt-in, per-escrow accounting over Blend's non-collateral supply positions.
//! Failed pool calls or principal shortfalls abort the enclosing transaction.
use delego_interfaces::blend::{underlying, BlendPoolClient, Request, SUPPLY, WITHDRAW};
use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    contracterror, contracttype, panic_with_error, symbol_short, token, vec, Address, Env, IntoVal,
    Map, Symbol, TryFromVal, Val,
};

/// Configuration is immutable after funding.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldBearingEscrowConfig {
    /// An explicitly approved Blend V2 pool.
    pub lending_pool_contract: Address,
    /// The receipt ledger address: must equal the pool address.
    /// Blend does not deploy a transferable bUSDC token contract.
    pub b_token_address: Address,
    /// Realized interest paid by the first redemption, in underlying units.
    pub accrued_yield_stroops: i128,
    /// Buyer or a treasury selected with both buyer and admin authorization.
    pub yield_recipient: Address,
}

/// Errors use their own namespace instead of expanding the legacy escrow enum.
#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum BlendYieldError {
    /// Invalid, duplicate, or already-used configuration.
    InvalidConfig = 4900,
    /// Pool response cannot be decoded or violates accounting expectations.
    InvalidPool = 4901,
    /// Conversion or balance arithmetic overflowed.
    MathOverflow = 4902,
    /// Redeemed balance does not cover this escrow's principal.
    PrincipalShortfall = 4903,
    /// Position has already been funded.
    AlreadyFunded = 4904,
}

#[contracttype]
#[derive(Clone)]
enum Key {
    BlendConfig(u64),
    BlendPosition(u64),
}

#[contracttype]
#[derive(Clone)]
struct Position {
    token: Address,
    index: u32,
    shares: i128,
    principal: i128,
}

fn fail(env: &Env, error: BlendYieldError) -> ! {
    panic_with_error!(env, error)
}

fn checked(env: &Env, amount: Option<i128>) -> i128 {
    amount.unwrap_or_else(|| fail(env, BlendYieldError::MathOverflow))
}

fn bump(env: &Env, key: &Key) {
    env.storage().persistent().extend_ttl(key, 17_280, 518_400);
}

/// Read configuration, including realized interest after redemption.
pub fn config(env: &Env, id: u64) -> Option<YieldBearingEscrowConfig> {
    let key = Key::BlendConfig(id);
    let value = env.storage().persistent().get(&key);
    if value.is_some() {
        bump(env, &key);
    }
    value
}

/// Called only after the parent escrow verifies Created status and both auths.
pub fn configure(env: &Env, id: u64, value: YieldBearingEscrowConfig) {
    if value.lending_pool_contract != value.b_token_address
        || value.lending_pool_contract == env.current_contract_address()
        || value.yield_recipient == env.current_contract_address()
        || value.accrued_yield_stroops != 0
        || config(env, id).is_some()
    {
        fail(env, BlendYieldError::InvalidConfig);
    }
    env.storage()
        .persistent()
        .set(&Key::BlendConfig(id), &value);
    bump(env, &Key::BlendConfig(id));
}

fn field<T: TryFromVal<Env, Val>>(env: &Env, map: &Map<Symbol, Val>, name: &str) -> T {
    let value = map
        .get(Symbol::new(env, name))
        .unwrap_or_else(|| fail(env, BlendYieldError::InvalidPool));
    T::try_from_val(env, &value).unwrap_or_else(|_| fail(env, BlendYieldError::InvalidPool))
}

fn reserve(env: &Env, pool: &BlendPoolClient, asset: &Address) -> (u32, i128) {
    let value = pool.get_reserve(asset);
    let actual: Address = field(env, &value, "asset");
    let cfg: Map<Symbol, Val> = field(env, &value, "config");
    let data: Map<Symbol, Val> = field(env, &value, "data");
    let index = field(env, &cfg, "index");
    let rate: i128 = field(env, &data, "b_rate");
    if actual != *asset || rate <= 0 {
        fail(env, BlendYieldError::InvalidPool);
    }
    (index, rate)
}

/// Supply only this escrow's actual deposit. Never borrows or supplies collateral.
/// An unconfigured escrow retains the existing custody behavior.
pub fn fund(env: &Env, id: u64, asset: &Address, amount: i128) {
    let Some(cfg) = config(env, id) else { return };
    let key = Key::BlendPosition(id);
    if env.storage().persistent().has(&key) {
        fail(env, BlendYieldError::AlreadyFunded);
    }
    if amount <= 0 {
        fail(env, BlendYieldError::InvalidConfig);
    }
    let owner = env.current_contract_address();
    let pool = BlendPoolClient::new(env, &cfg.lending_pool_contract);
    let (index, _) = reserve(env, &pool, asset);
    let before = pool.get_positions(&owner);
    let previous = before.supply.get(index).unwrap_or(0);
    let token = token::Client::new(env, asset);
    let balance = token.balance(&owner);
    // The direct pool invocation is authorized by Soroban automatically.
    // Authorize only its exact nested token transfer, never an open allowance.
    env.authorize_as_current_contract(vec![
        env,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: asset.clone(),
                fn_name: symbol_short!("transfer"),
                args: (owner.clone(), cfg.lending_pool_contract.clone(), amount).into_val(env),
            },
            sub_invocations: vec![env],
        }),
    ]);
    let after = pool.submit(
        &owner,
        &owner,
        &owner,
        &vec![
            env,
            Request {
                request_type: SUPPLY,
                address: asset.clone(),
                amount,
            },
        ],
    );
    let shares = checked(
        env,
        after.supply.get(index).unwrap_or(0).checked_sub(previous),
    );
    if shares <= 0
        || before.liabilities != after.liabilities
        || before.collateral != after.collateral
        || checked(env, balance.checked_sub(token.balance(&owner))) != amount
    {
        fail(env, BlendYieldError::InvalidPool);
    }
    env.storage().persistent().set(
        &key,
        &Position {
            token: asset.clone(),
            index,
            shares,
            principal: amount,
        },
    );
    bump(env, &key);
    env.events().publish(
        (symbol_short!("escrow"), symbol_short!("bl_supply"), id),
        (amount, shares),
    );
}

/// Return the entire position to custody before ANY payout for this escrow.
///
/// A partial settlement exits lending once; remaining principal stays liquid.
/// Interest is measured from the received token delta, never a quoted APR.
/// Retains a zero-share tombstone so funding cannot be replayed.
pub fn redeem(env: &Env, id: u64) {
    let key = Key::BlendPosition(id);
    let Some(mut position): Option<Position> = env.storage().persistent().get(&key) else {
        return;
    };
    bump(env, &key);
    if position.shares == 0 {
        return;
    }
    let mut cfg = config(env, id).unwrap_or_else(|| fail(env, BlendYieldError::InvalidConfig));
    let owner = env.current_contract_address();
    let pool = BlendPoolClient::new(env, &cfg.lending_pool_contract);
    let (index, rate) = reserve(env, &pool, &position.token);
    if index != position.index {
        fail(env, BlendYieldError::InvalidPool);
    }
    let quoted = checked(env, underlying(position.shares, rate));
    if quoted < position.principal {
        fail(env, BlendYieldError::PrincipalShortfall);
    }
    let before = pool.get_positions(&owner);
    let previous = before.supply.get(index).unwrap_or(0);
    if previous < position.shares {
        fail(env, BlendYieldError::InvalidPool);
    }
    let token = token::Client::new(env, &position.token);
    let balance = token.balance(&owner);
    let after = pool.submit(
        &owner,
        &owner,
        &owner,
        &vec![
            env,
            Request {
                request_type: WITHDRAW,
                address: position.token.clone(),
                amount: quoted,
            },
        ],
    );
    let burnt = checked(
        env,
        previous.checked_sub(after.supply.get(index).unwrap_or(0)),
    );
    let received = checked(env, token.balance(&owner).checked_sub(balance));
    if burnt != position.shares
        || before.liabilities != after.liabilities
        || before.collateral != after.collateral
    {
        fail(env, BlendYieldError::InvalidPool);
    }
    if received < position.principal {
        fail(env, BlendYieldError::PrincipalShortfall);
    }
    let interest = checked(env, received.checked_sub(position.principal));
    position.shares = 0;
    env.storage().persistent().set(&key, &position);
    cfg.accrued_yield_stroops = interest;
    env.storage().persistent().set(&Key::BlendConfig(id), &cfg);
    if interest > 0 {
        token.transfer(&owner, &cfg.yield_recipient, &interest);
    }
    env.events().publish(
        (symbol_short!("escrow"), symbol_short!("bl_redeem"), id),
        (position.principal, interest),
    );
}
