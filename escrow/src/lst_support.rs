//! Liquid staking token (LST) escrow collateral (issue #592).
//!
//! A buyer may lock a yield-bearing liquid staking token (e.g. yXLM) against
//! an escrow instead of a plain stablecoin. An LST's share count is constant
//! while each share's underlying value grows as staking rewards accrue, so the
//! escrow tracks **shares**, not raw token amounts:
//!
//! * At lock time the contract records the share balance actually received
//!   and snapshots the LST's exchange rate (underlying per share, scaled by
//!   [`LST_RATE_SCALE`]). The escrowed value is fixed at
//!   `initial_share_balance * exchange_rate_snapshot / LST_RATE_SCALE`.
//! * On completion the merchant receives only the shares needed to cover that
//!   original value at the *current* rate; every remaining share — the
//!   staking appreciation earned during the inspection period — is returned
//!   to the buyer.
//! * On refund or cancellation every share goes back to the buyer.
//!
//! Only admin-whitelisted LST contracts are accepted.

use crate::{EscrowError, PERSISTENT_BUMP_AMOUNT, PERSISTENT_BUMP_THRESHOLD};
use soroban_sdk::{contractclient, contracttype, symbol_short, token, Address, Env};

/// Fixed-point scale of [`LstTokenInterface::exchange_rate`] (7 decimals,
/// matching Stellar asset precision).
pub const LST_RATE_SCALE: i128 = 10_000_000;

/// Share-based record of LST collateral locked for one order.
#[contracttype]
#[derive(Clone, Debug)]
pub struct LstCollateralRecord {
    /// Escrow / order identifier the collateral belongs to.
    pub order_id: u64,
    /// Whitelisted LST contract address.
    pub lst_token_address: Address,
    /// LST shares actually received by the escrow at lock time.
    pub initial_share_balance: i128,
    /// Underlying value per share at lock time, scaled by [`LST_RATE_SCALE`].
    pub exchange_rate_snapshot: i128,
}

/// Exchange-rate view exposed by supported liquid staking token contracts.
#[contractclient(name = "LstTokenClient")]
pub trait LstTokenInterface {
    /// Underlying value of one share, scaled by [`LST_RATE_SCALE`].
    fn exchange_rate(env: Env) -> i128;
}

/// Outcome of settling LST collateral: shares paid to each side.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LstSettlement {
    /// Shares transferred to the merchant (covers the original value).
    pub merchant_shares: i128,
    /// Shares transferred to the buyer (staking appreciation or full refund).
    pub buyer_shares: i128,
}

#[contracttype]
#[derive(Clone)]
pub(crate) enum Key {
    LstWhitelist(Address),
    LstCollateral(u64),
    LstBuyer(u64),
}

/// Event emitted whenever the LST whitelist changes.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LstWhitelistUpdated {
    pub lst_token: Address,
    pub allowed: bool,
}

fn bump(env: &Env, key: &Key) {
    env.storage()
        .persistent()
        .extend_ttl(key, PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
}

/// Add or remove `lst_token` from the LST whitelist. Callers must enforce
/// admin authorization. The zero address and this contract are never valid
/// collateral tokens.
pub fn set_whitelisted(env: &Env, lst_token: &Address, allowed: bool) -> Result<(), EscrowError> {
    if crate::is_zero_address(env, lst_token) || lst_token == &env.current_contract_address() {
        return Err(EscrowError::InvalidAddress);
    }
    let key = Key::LstWhitelist(lst_token.clone());
    if allowed {
        env.storage().persistent().set(&key, &true);
        bump(env, &key);
    } else {
        env.storage().persistent().remove(&key);
    }
    env.events().publish(
        (
            symbol_short!("escrow"),
            symbol_short!("lst_wl"),
            lst_token.clone(),
        ),
        LstWhitelistUpdated {
            lst_token: lst_token.clone(),
            allowed,
        },
    );
    Ok(())
}

/// Whether `lst_token` is an accepted LST collateral contract.
pub fn is_whitelisted(env: &Env, lst_token: &Address) -> bool {
    let key = Key::LstWhitelist(lst_token.clone());
    let allowed: Option<bool> = env.storage().persistent().get(&key);
    if allowed.is_some() {
        bump(env, &key);
    }
    allowed.unwrap_or(false)
}

/// Read the LST collateral record for `order_id`, if any.
pub fn collateral(env: &Env, order_id: u64) -> Option<LstCollateralRecord> {
    let key = Key::LstCollateral(order_id);
    let value = env.storage().persistent().get(&key);
    if value.is_some() {
        bump(env, &key);
    }
    value
}

/// Read and validate the current exchange rate of `lst_token`.
pub fn exchange_rate(env: &Env, lst_token: &Address) -> Result<i128, EscrowError> {
    match LstTokenClient::new(env, lst_token).try_exchange_rate() {
        Ok(Ok(rate)) if rate > 0 => Ok(rate),
        _ => Err(EscrowError::InvalidLstExchangeRate),
    }
}

/// Underlying value of `shares` at `rate`, rounded down.
pub fn share_value(shares: i128, rate: i128) -> Result<i128, EscrowError> {
    if shares < 0 || rate <= 0 {
        return Err(EscrowError::InvalidLstExchangeRate);
    }
    shares
        .checked_mul(rate)
        .and_then(|v| v.checked_div(LST_RATE_SCALE))
        .ok_or(EscrowError::MathOverflow)
}

/// Shares required to cover `value` at `rate`, rounded up so the merchant is
/// never short-changed by rounding.
fn shares_for_value(value: i128, rate: i128) -> Result<i128, EscrowError> {
    if value < 0 || rate <= 0 {
        return Err(EscrowError::InvalidLstExchangeRate);
    }
    let scaled = value
        .checked_mul(LST_RATE_SCALE)
        .ok_or(EscrowError::MathOverflow)?;
    let whole = scaled / rate;
    if scaled % rate == 0 {
        Ok(whole)
    } else {
        whole.checked_add(1).ok_or(EscrowError::MathOverflow)
    }
}

/// Pull `shares` of a whitelisted LST from `buyer` into escrow custody and
/// record the share balance plus an exchange-rate snapshot.
///
/// Callers must have verified `buyer` authorization and the escrow's status.
/// The recorded share balance is the measured balance delta, so
/// fee-on-transfer tokens cannot inflate the collateral.
pub fn lock_collateral(
    env: &Env,
    order_id: u64,
    buyer: &Address,
    lst_token: &Address,
    shares: i128,
) -> Result<LstCollateralRecord, EscrowError> {
    if shares <= 0 {
        return Err(EscrowError::ZeroAmount);
    }
    if !is_whitelisted(env, lst_token) {
        return Err(EscrowError::LstTokenNotWhitelisted);
    }
    let key = Key::LstCollateral(order_id);
    if env.storage().persistent().has(&key) {
        return Err(EscrowError::LstCollateralAlreadyRecorded);
    }
    let rate = exchange_rate(env, lst_token)?;

    let this = env.current_contract_address();
    let client = token::Client::new(env, lst_token);
    let before = client.balance(&this);
    client.transfer(buyer, &this, &shares);
    let received = client
        .balance(&this)
        .checked_sub(before)
        .ok_or(EscrowError::MathOverflow)?;
    if received != shares {
        return Err(EscrowError::DepositUnderfunded);
    }

    let record = LstCollateralRecord {
        order_id,
        lst_token_address: lst_token.clone(),
        initial_share_balance: received,
        exchange_rate_snapshot: rate,
    };
    env.storage().persistent().set(&key, &record);
    bump(env, &key);
    let buyer_key = Key::LstBuyer(order_id);
    env.storage().persistent().set(&buyer_key, buyer);
    bump(env, &buyer_key);

    env.events().publish(
        (symbol_short!("escrow"), symbol_short!("lst_lock"), order_id),
        (lst_token.clone(), received, rate),
    );
    Ok(record)
}

/// Split the locked shares at the current exchange rate.
///
/// The merchant's share count covers the original escrowed value; the rest is
/// the buyer's staking appreciation. If the rate has fallen (e.g. slashing)
/// the merchant receives every share and the buyer receives none.
pub fn split_on_completion(
    record: &LstCollateralRecord,
    current_rate: i128,
) -> Result<LstSettlement, EscrowError> {
    let original_value = share_value(record.initial_share_balance, record.exchange_rate_snapshot)?;
    let needed = shares_for_value(original_value, current_rate)?;
    let merchant_shares = if needed > record.initial_share_balance {
        record.initial_share_balance
    } else {
        needed
    };
    let buyer_shares = record
        .initial_share_balance
        .checked_sub(merchant_shares)
        .ok_or(EscrowError::MathOverflow)?;
    Ok(LstSettlement {
        merchant_shares,
        buyer_shares,
    })
}

/// Settle the LST collateral for `order_id` and clear its record.
///
/// With `completed = true` the merchant receives the original value in shares
/// and the buyer receives the accrued appreciation; otherwise every share is
/// refunded to the buyer. Callers must verify the escrow reached a terminal
/// state.
pub fn settle(
    env: &Env,
    order_id: u64,
    merchant: &Address,
    completed: bool,
) -> Result<LstSettlement, EscrowError> {
    let key = Key::LstCollateral(order_id);
    let record: LstCollateralRecord = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(EscrowError::LstCollateralNotFound)?;
    let buyer_key = Key::LstBuyer(order_id);
    let buyer: Address = env
        .storage()
        .persistent()
        .get(&buyer_key)
        .ok_or(EscrowError::LstCollateralNotFound)?;

    let settlement = if completed {
        let rate = exchange_rate(env, &record.lst_token_address)?;
        split_on_completion(&record, rate)?
    } else {
        LstSettlement {
            merchant_shares: 0,
            buyer_shares: record.initial_share_balance,
        }
    };

    // Clear state before any external transfer (checks-effects-interactions).
    env.storage().persistent().remove(&key);
    env.storage().persistent().remove(&buyer_key);

    let this = env.current_contract_address();
    let client = token::Client::new(env, &record.lst_token_address);
    if settlement.merchant_shares > 0 {
        client.transfer(&this, merchant, &settlement.merchant_shares);
    }
    if settlement.buyer_shares > 0 {
        client.transfer(&this, &buyer, &settlement.buyer_shares);
    }

    env.events().publish(
        (
            symbol_short!("escrow"),
            symbol_short!("lst_settl"),
            order_id,
        ),
        (
            settlement.merchant_shares,
            settlement.buyer_shares,
            completed,
        ),
    );
    Ok(settlement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EscrowConfig, EscrowContract, EscrowContractClient};
    use soroban_sdk::{
        testutils::{storage::Persistent, Address as _, Events, Ledger},
        TryIntoVal,
    };

    const ZERO_ACCOUNT_STRKEY: &str = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";

    fn setup<'a>(env: &Env) -> (EscrowContractClient<'a>, Address, Address) {
        env.mock_all_auths();
        let admin = Address::generate(env);
        let contract_id = env.register(
            EscrowContract,
            (EscrowConfig {
                admin: admin.clone(),
                fee_bps: 0,
                treasury: Address::generate(env),
                min_amount: 1,
                max_amount: 1_000_000,
                min_fee_floor_stroops: 0,
                max_fee_ceiling_stroops: 0,
            },),
        );
        (
            EscrowContractClient::new(env, &contract_id),
            contract_id,
            admin,
        )
    }

    #[test]
    fn zero_and_self_addresses_cannot_be_whitelisted() {
        let env = Env::default();
        let (client, _, admin) = setup(&env);
        let zero = Address::from_str(&env, ZERO_ACCOUNT_STRKEY);
        let token = Address::generate(&env);

        assert_eq!(
            client.try_set_lst_whitelisted(&admin, &zero, &true),
            Err(Ok(EscrowError::InvalidAddress))
        );
        assert_eq!(
            client.try_set_lst_whitelisted(&admin, &client.address, &true),
            Err(Ok(EscrowError::InvalidAddress))
        );
        assert!(!client.is_lst_whitelisted(&zero));

        client.set_lst_whitelisted(&admin, &token, &true);
        assert!(client.is_lst_whitelisted(&token));
        client.set_lst_whitelisted(&admin, &token, &false);
        assert!(!client.is_lst_whitelisted(&token));
    }

    #[test]
    fn whitelist_changes_emit_structured_events() {
        let env = Env::default();
        let (client, _, admin) = setup(&env);
        let token = Address::generate(&env);

        client.set_lst_whitelisted(&admin, &token, &true);
        let events = env.events().all();
        let event = events
            .iter()
            .find(|(_, topics, _)| {
                topics.len() == 3
                    && topics.get(0).and_then(|v| v.try_into_val(&env).ok())
                        == Some(symbol_short!("escrow"))
                    && topics.get(1).and_then(|v| v.try_into_val(&env).ok())
                        == Some(symbol_short!("lst_wl"))
                    && topics.get(2).and_then(|v| v.try_into_val(&env).ok()) == Some(token.clone())
            })
            .expect("lst_wl event should be emitted");
        let data: LstWhitelistUpdated = event.2.try_into_val(&env).unwrap();
        assert_eq!(data.lst_token, token);
        assert!(data.allowed);

        client.set_lst_whitelisted(&admin, &token, &false);
        let events = env.events().all();
        let last = events.last().unwrap();
        let data: LstWhitelistUpdated = last.2.try_into_val(&env).unwrap();
        assert!(!data.allowed);
    }

    #[test]
    fn whitelist_read_extends_ttl() {
        let env = Env::default();
        let (client, contract_id, admin) = setup(&env);
        let token = Address::generate(&env);
        client.set_lst_whitelisted(&admin, &token, &true);

        env.as_contract(&contract_id, || {
            env.storage().instance().extend_ttl(1_000_000, 1_000_000);
        });
        let initial = env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .get_ttl(&Key::LstWhitelist(token.clone()))
        });
        env.ledger()
            .with_mut(|li| li.sequence_number += initial - PERSISTENT_BUMP_THRESHOLD + 1);

        let before = env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .get_ttl(&Key::LstWhitelist(token.clone()))
        });
        assert!(client.is_lst_whitelisted(&token));
        let after = env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .get_ttl(&Key::LstWhitelist(token.clone()))
        });
        assert!(after > before);
        assert!(after >= PERSISTENT_BUMP_AMOUNT);
    }
}
