//! Gas fee sponsorship quota and deduction logic.
//!
//! Relayers submitting transactions on behalf of users need on-chain gas fee
//! allowances to prevent draining the relayer's native XLM balance.
//! This module tracks deposited sponsorship quotas, enforces lifetime balance
//! and per-transaction fee caps, and reimburses relayers from the user's
//! deposited sponsorship pool upon execution.

use soroban_sdk::{contracttype, symbol_short, token, Address, Env};

use crate::PermissionError;

/// Persistent-storage TTL threshold for sponsorship entries (~30 days at 5s/ledger).
pub const SPONSORSHIP_TTL_THRESHOLD: u32 = 518_400;
/// Persistent-storage TTL extension target for sponsorship entries (~60 days).
pub const SPONSORSHIP_TTL_EXTEND_TO: u32 = 1_036_800;

/// Storage keys for the gas sponsorship module (issue #689).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SponsorshipDataKey {
    /// Sponsorship policy configured by `sponsor`.
    Policy(Address),
    /// Deposited, not-yet-spent sponsorship balance (in stroops) for `sponsor`.
    Balance(Address),
}

/// Sponsor-configured rules for reimbursing a relayer's gas fees.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SponsorshipPolicy {
    /// The only relayer allowed to draw fees from this sponsorship.
    pub relayer: Address,
    /// Token in which the budget is held and fees are reimbursed (normally native XLM).
    pub fee_token: Address,
    /// Maximum fee in stroops reimbursable for a single transaction.
    pub max_fee_per_tx_stroops: i128,
    /// Remaining number of sponsored transactions.
    pub remaining_tx_count: u32,
}

fn extend_sponsorship_ttl(env: &Env, key: &SponsorshipDataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, SPONSORSHIP_TTL_THRESHOLD, SPONSORSHIP_TTL_EXTEND_TO);
}

/// Reads the sponsorship policy for `sponsor`, extending its TTL when present.
pub fn read_policy(env: &Env, sponsor: &Address) -> Option<SponsorshipPolicy> {
    let key = SponsorshipDataKey::Policy(sponsor.clone());
    let policy: Option<SponsorshipPolicy> = env.storage().persistent().get(&key);
    if policy.is_some() {
        extend_sponsorship_ttl(env, &key);
    }
    policy
}

fn write_policy(env: &Env, sponsor: &Address, policy: &SponsorshipPolicy) {
    let key = SponsorshipDataKey::Policy(sponsor.clone());
    env.storage().persistent().set(&key, policy);
    extend_sponsorship_ttl(env, &key);
}

/// Reads the remaining sponsorship balance for `sponsor` (0 when unfunded),
/// extending its TTL when present.
pub fn read_balance(env: &Env, sponsor: &Address) -> i128 {
    let key = SponsorshipDataKey::Balance(sponsor.clone());
    match env.storage().persistent().get::<_, i128>(&key) {
        Some(balance) => {
            extend_sponsorship_ttl(env, &key);
            balance
        }
        None => 0,
    }
}

fn write_balance(env: &Env, sponsor: &Address, balance: i128) {
    let key = SponsorshipDataKey::Balance(sponsor.clone());
    env.storage().persistent().set(&key, &balance);
    extend_sponsorship_ttl(env, &key);
}

/// Creates or replaces `sponsor`'s policy. The caller must have already
/// authorized `sponsor`.
pub fn configure(
    env: &Env,
    sponsor: &Address,
    relayer: &Address,
    fee_token: &Address,
    max_fee_per_tx_stroops: i128,
    remaining_tx_count: u32,
) -> Result<(), PermissionError> {
    if max_fee_per_tx_stroops <= 0 || remaining_tx_count == 0 || relayer == sponsor {
        return Err(PermissionError::InvalidParam);
    }
    // A live balance is denominated in the existing fee token; switching tokens
    // would let the stored balance be reinterpreted in a different asset.
    if let Some(existing) = read_policy(env, sponsor) {
        if existing.fee_token != *fee_token && read_balance(env, sponsor) > 0 {
            return Err(PermissionError::InvalidParam);
        }
    }
    let policy = SponsorshipPolicy {
        relayer: relayer.clone(),
        fee_token: fee_token.clone(),
        max_fee_per_tx_stroops,
        remaining_tx_count,
    };
    write_policy(env, sponsor, &policy);
    env.events().publish(
        (symbol_short!("perm"), symbol_short!("gas_cfg")),
        (sponsor.clone(), relayer.clone(), max_fee_per_tx_stroops, remaining_tx_count),
    );
    Ok(())
}

/// Moves `amount` of the policy's fee token from `sponsor` into the contract
/// and credits it to the sponsorship budget. Returns the new balance. The
/// caller must have already authorized `sponsor`.
pub fn fund(env: &Env, sponsor: &Address, amount: i128) -> Result<i128, PermissionError> {
    if amount <= 0 {
        return Err(PermissionError::InvalidParam);
    }
    let policy = read_policy(env, sponsor).ok_or(PermissionError::NotFound)?;
    let new_balance = read_balance(env, sponsor)
        .checked_add(amount)
        .ok_or(PermissionError::InvalidParam)?;

    token::Client::new(env, &policy.fee_token).transfer(
        sponsor,
        &env.current_contract_address(),
        &amount,
    );
    write_balance(env, sponsor, new_balance);

    env.events().publish(
        (symbol_short!("perm"), symbol_short!("gas_fund")),
        (sponsor.clone(), amount, new_balance),
    );
    Ok(new_balance)
}

/// Deducts `fee_stroops` from `sponsor`'s budget and reimburses `relayer`
/// from the contract-held pool. Returns the remaining balance. The caller
/// must have already authorized `relayer`.
pub fn charge_fee(
    env: &Env,
    sponsor: &Address,
    relayer: &Address,
    fee_stroops: i128,
) -> Result<i128, PermissionError> {
    let mut policy = read_policy(env, sponsor).ok_or(PermissionError::NotFound)?;
    let mut quota = GasSponsorshipQuota {
        relayer: policy.relayer.clone(),
        sponsor_balance_stroops: read_balance(env, sponsor),
        max_fee_per_tx_stroops: policy.max_fee_per_tx_stroops,
        remaining_tx_count: policy.remaining_tx_count,
    };
    deduct_and_reimburse_relayer(
        env,
        &mut quota,
        fee_stroops,
        relayer,
        &policy.fee_token,
        &env.current_contract_address(),
    )?;

    policy.remaining_tx_count = quota.remaining_tx_count;
    write_policy(env, sponsor, &policy);
    write_balance(env, sponsor, quota.sponsor_balance_stroops);
    Ok(quota.sponsor_balance_stroops)
}

/// Storage and tracking quota for gas fee sponsorship.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GasSponsorshipQuota {
    /// Relayer designated or executing on behalf of the user.
    pub relayer: Address,
    /// Current available sponsorship balance in stroops.
    pub sponsor_balance_stroops: i128,
    /// Maximum base gas fee in stroops reimbursable for a single transaction.
    pub max_fee_per_tx_stroops: i128,
    /// Remaining count of sponsored transactions allowed under this quota.
    pub remaining_tx_count: u32,
}

/// Deducts exact base transaction fee from a sponsor's balance and reimburses the relayer.
///
/// # Validation and Safety
/// - `fee_stroops` must be strictly positive (`> 0`).
/// - Reverts with [`PermissionError::ExceedsPerTxLimit`] if `fee_stroops` exceeds
///   `quota.max_fee_per_tx_stroops`.
/// - Reverts with [`PermissionError::ExceedsTotalLimit`] if `remaining_tx_count == 0`
///   or `fee_stroops` exceeds `quota.sponsor_balance_stroops`.
/// - Reverts with [`PermissionError::Unauthorized`] if caller/expected relayer does not match
///   `quota.relayer`.
///
/// # Reimbursement
/// - Deducts `fee_stroops` from `quota.sponsor_balance_stroops`.
/// - Decrements `quota.remaining_tx_count` by 1.
/// - If `native_token` is provided, transfers `fee_stroops` from `sponsor` (or contract escrow)
///   to `quota.relayer`.
/// - Publishes a sponsorship deduction event for off-chain indexing.
pub fn deduct_sponsorship_fee(
    env: &Env,
    quota: &mut GasSponsorshipQuota,
    fee_stroops: i128,
    relayer: &Address,
) -> Result<(), PermissionError> {
    if fee_stroops <= 0 {
        return Err(PermissionError::InvalidParam);
    }

    if quota.relayer != *relayer {
        return Err(PermissionError::Unauthorized);
    }

    if fee_stroops > quota.max_fee_per_tx_stroops {
        return Err(PermissionError::ExceedsPerTxLimit);
    }

    if quota.remaining_tx_count == 0 {
        return Err(PermissionError::ExceedsTotalLimit);
    }

    let updated_balance = quota
        .sponsor_balance_stroops
        .checked_sub(fee_stroops)
        .ok_or(PermissionError::ExceedsTotalLimit)?;

    if updated_balance < 0 {
        return Err(PermissionError::ExceedsTotalLimit);
    }

    let updated_tx_count = quota
        .remaining_tx_count
        .checked_sub(1)
        .ok_or(PermissionError::ExceedsTotalLimit)?;

    quota.sponsor_balance_stroops = updated_balance;
    quota.remaining_tx_count = updated_tx_count;

    env.events().publish(
        (symbol_short!("perm"), symbol_short!("gas_ded")),
        (relayer.clone(), fee_stroops, quota.sponsor_balance_stroops, quota.remaining_tx_count),
    );

    Ok(())
}

/// Deducts exact base transaction fee from a sponsor's balance and transfers
/// native token reimbursement to the relayer.
pub fn deduct_and_reimburse_relayer(
    env: &Env,
    quota: &mut GasSponsorshipQuota,
    fee_stroops: i128,
    relayer: &Address,
    native_token: &Address,
    sponsor_source: &Address,
) -> Result<(), PermissionError> {
    deduct_sponsorship_fee(env, quota, fee_stroops, relayer)?;

    let token_client = soroban_sdk::token::Client::new(env, native_token);
    token_client.transfer(sponsor_source, relayer, &fee_stroops);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        contract, contractimpl,
        testutils::{Address as _, Events},
        token::{StellarAssetClient, TokenClient},
    };

    #[test]
    fn test_deduct_sponsorship_fee_success() {
        let env = Env::default();
        let relayer = Address::generate(&env);

        let mut quota = GasSponsorshipQuota {
            relayer: relayer.clone(),
            sponsor_balance_stroops: 50_000,
            max_fee_per_tx_stroops: 10_000,
            remaining_tx_count: 5,
        };

        let res = deduct_sponsorship_fee(&env, &mut quota, 5_000, &relayer);
        assert_eq!(res, Ok(()));
        assert_eq!(quota.sponsor_balance_stroops, 45_000);
        assert_eq!(quota.remaining_tx_count, 4);

        let events = env.events().all();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_revert_if_fee_exceeds_max_fee_per_tx() {
        let env = Env::default();
        let relayer = Address::generate(&env);

        let mut quota = GasSponsorshipQuota {
            relayer: relayer.clone(),
            sponsor_balance_stroops: 50_000,
            max_fee_per_tx_stroops: 5_000,
            remaining_tx_count: 5,
        };

        let res = deduct_sponsorship_fee(&env, &mut quota, 5_001, &relayer);
        assert_eq!(res, Err(PermissionError::ExceedsPerTxLimit));
        // Ensure state was not mutated
        assert_eq!(quota.sponsor_balance_stroops, 50_000);
        assert_eq!(quota.remaining_tx_count, 5);
    }

    #[test]
    fn test_revert_if_fee_exceeds_lifetime_balance() {
        let env = Env::default();
        let relayer = Address::generate(&env);

        let mut quota = GasSponsorshipQuota {
            relayer: relayer.clone(),
            sponsor_balance_stroops: 3_000,
            max_fee_per_tx_stroops: 10_000,
            remaining_tx_count: 5,
        };

        let res = deduct_sponsorship_fee(&env, &mut quota, 4_000, &relayer);
        assert_eq!(res, Err(PermissionError::ExceedsTotalLimit));
        assert_eq!(quota.sponsor_balance_stroops, 3_000);
        assert_eq!(quota.remaining_tx_count, 5);
    }

    #[test]
    fn test_revert_if_no_remaining_tx_count() {
        let env = Env::default();
        let relayer = Address::generate(&env);

        let mut quota = GasSponsorshipQuota {
            relayer: relayer.clone(),
            sponsor_balance_stroops: 50_000,
            max_fee_per_tx_stroops: 10_000,
            remaining_tx_count: 0,
        };

        let res = deduct_sponsorship_fee(&env, &mut quota, 1_000, &relayer);
        assert_eq!(res, Err(PermissionError::ExceedsTotalLimit));
        assert_eq!(quota.sponsor_balance_stroops, 50_000);
        assert_eq!(quota.remaining_tx_count, 0);
    }

    #[test]
    fn test_revert_if_unauthorized_relayer() {
        let env = Env::default();
        let relayer = Address::generate(&env);
        let other_relayer = Address::generate(&env);

        let mut quota = GasSponsorshipQuota {
            relayer: relayer.clone(),
            sponsor_balance_stroops: 50_000,
            max_fee_per_tx_stroops: 10_000,
            remaining_tx_count: 5,
        };

        let res = deduct_sponsorship_fee(&env, &mut quota, 1_000, &other_relayer);
        assert_eq!(res, Err(PermissionError::Unauthorized));
        assert_eq!(quota.sponsor_balance_stroops, 50_000);
        assert_eq!(quota.remaining_tx_count, 5);
    }

    #[test]
    fn test_revert_if_invalid_fee() {
        let env = Env::default();
        let relayer = Address::generate(&env);

        let mut quota = GasSponsorshipQuota {
            relayer: relayer.clone(),
            sponsor_balance_stroops: 50_000,
            max_fee_per_tx_stroops: 10_000,
            remaining_tx_count: 5,
        };

        assert_eq!(
            deduct_sponsorship_fee(&env, &mut quota, 0, &relayer),
            Err(PermissionError::InvalidParam)
        );
        assert_eq!(
            deduct_sponsorship_fee(&env, &mut quota, -100, &relayer),
            Err(PermissionError::InvalidParam)
        );
    }

    #[contract]
    pub struct MockContract;
    #[contractimpl]
    impl MockContract {}

    #[test]
    fn test_deduct_and_reimburse_relayer() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(MockContract, ());
        let token_admin = Address::generate(&env);
        let relayer = Address::generate(&env);

        let sac = env.register_stellar_asset_contract_v2(token_admin);
        let native_token = sac.address();

        let stellar_client = StellarAssetClient::new(&env, &native_token);
        stellar_client.mint(&contract_id, &100_000);

        let mut quota = GasSponsorshipQuota {
            relayer: relayer.clone(),
            sponsor_balance_stroops: 50_000,
            max_fee_per_tx_stroops: 10_000,
            remaining_tx_count: 2,
        };

        let res = env.as_contract(&contract_id, || {
            deduct_and_reimburse_relayer(
                &env,
                &mut quota,
                7_500,
                &relayer,
                &native_token,
                &contract_id,
            )
        });

        assert_eq!(res, Ok(()));
        assert_eq!(quota.sponsor_balance_stroops, 42_500);
        assert_eq!(quota.remaining_tx_count, 1);

        let token_client = TokenClient::new(&env, &native_token);
        assert_eq!(token_client.balance(&relayer), 7_500);
        assert_eq!(token_client.balance(&contract_id), 92_500);
    }

    // ── Contract entrypoint tests (issue #689) ──────────────────────────────

    use crate::{PermissionsContract, PermissionsContractClient};

    struct SponsorEnv {
        env: Env,
        client_id: Address,
        sponsor: Address,
        relayer: Address,
        fee_token: Address,
    }

    fn setup_sponsorship(max_fee: i128, tx_count: u32) -> SponsorEnv {
        let env = Env::default();
        env.mock_all_auths();
        let client_id = env.register(PermissionsContract, ());
        let sponsor = Address::generate(&env);
        let relayer = Address::generate(&env);
        let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
        let fee_token = sac.address();
        StellarAssetClient::new(&env, &fee_token).mint(&sponsor, &1_000_000);

        let client = PermissionsContractClient::new(&env, &client_id);
        client.configure_sponsorship(&sponsor, &relayer, &fee_token, &max_fee, &tx_count);

        SponsorEnv {
            env,
            client_id,
            sponsor,
            relayer,
            fee_token,
        }
    }

    #[test]
    fn test_fund_sponsorship_credits_budget_and_escrows_tokens() {
        let s = setup_sponsorship(10_000, 5);
        let client = PermissionsContractClient::new(&s.env, &s.client_id);
        let token_client = TokenClient::new(&s.env, &s.fee_token);

        assert_eq!(client.get_sponsorship_balance(&s.sponsor), 0);
        assert_eq!(client.fund_sponsorship(&s.sponsor, &40_000), 40_000);
        assert_eq!(client.fund_sponsorship(&s.sponsor, &10_000), 50_000);
        assert_eq!(client.get_sponsorship_balance(&s.sponsor), 50_000);
        assert_eq!(token_client.balance(&s.client_id), 50_000);
        assert_eq!(token_client.balance(&s.sponsor), 950_000);

        let policy = client.get_sponsorship_policy(&s.sponsor).unwrap();
        assert_eq!(policy.relayer, s.relayer);
        assert_eq!(policy.remaining_tx_count, 5);

        assert_eq!(
            client.try_fund_sponsorship(&s.sponsor, &0),
            Err(Ok(PermissionError::InvalidParam))
        );
    }

    #[test]
    fn test_fund_sponsorship_without_policy_rejected() {
        let s = setup_sponsorship(10_000, 5);
        let client = PermissionsContractClient::new(&s.env, &s.client_id);
        let stranger = Address::generate(&s.env);
        assert_eq!(
            client.try_fund_sponsorship(&stranger, &1_000),
            Err(Ok(PermissionError::NotFound))
        );
    }

    #[test]
    fn test_charge_sponsored_fee_deducts_budget_and_reimburses_relayer() {
        let s = setup_sponsorship(10_000, 3);
        let client = PermissionsContractClient::new(&s.env, &s.client_id);
        let token_client = TokenClient::new(&s.env, &s.fee_token);
        client.fund_sponsorship(&s.sponsor, &25_000);

        assert_eq!(client.charge_sponsored_fee(&s.sponsor, &s.relayer, &7_500), 17_500);
        assert_eq!(client.get_sponsorship_balance(&s.sponsor), 17_500);
        assert_eq!(client.get_sponsorship_policy(&s.sponsor).unwrap().remaining_tx_count, 2);
        assert_eq!(token_client.balance(&s.relayer), 7_500);
        assert_eq!(token_client.balance(&s.client_id), 17_500);

        // Per-tx cap and relayer binding still apply through the entrypoint.
        assert_eq!(
            client.try_charge_sponsored_fee(&s.sponsor, &s.relayer, &10_001),
            Err(Ok(PermissionError::ExceedsPerTxLimit))
        );
        let other = Address::generate(&s.env);
        assert_eq!(
            client.try_charge_sponsored_fee(&s.sponsor, &other, &1_000),
            Err(Ok(PermissionError::Unauthorized))
        );
        assert_eq!(client.get_sponsorship_balance(&s.sponsor), 17_500);
    }

    #[test]
    fn test_charge_sponsored_fee_insufficient_budget_rejected() {
        let s = setup_sponsorship(10_000, 5);
        let client = PermissionsContractClient::new(&s.env, &s.client_id);
        let token_client = TokenClient::new(&s.env, &s.fee_token);
        client.fund_sponsorship(&s.sponsor, &4_000);

        assert_eq!(
            client.try_charge_sponsored_fee(&s.sponsor, &s.relayer, &5_000),
            Err(Ok(PermissionError::ExceedsTotalLimit))
        );
        // Nothing moved.
        assert_eq!(client.get_sponsorship_balance(&s.sponsor), 4_000);
        assert_eq!(client.get_sponsorship_policy(&s.sponsor).unwrap().remaining_tx_count, 5);
        assert_eq!(token_client.balance(&s.relayer), 0);
        assert_eq!(token_client.balance(&s.client_id), 4_000);
    }

    #[test]
    fn test_charge_sponsored_fee_exhausted_tx_count_rejected() {
        let s = setup_sponsorship(10_000, 1);
        let client = PermissionsContractClient::new(&s.env, &s.client_id);
        client.fund_sponsorship(&s.sponsor, &50_000);

        client.charge_sponsored_fee(&s.sponsor, &s.relayer, &1_000);
        assert_eq!(
            client.try_charge_sponsored_fee(&s.sponsor, &s.relayer, &1_000),
            Err(Ok(PermissionError::ExceedsTotalLimit))
        );
        assert_eq!(client.get_sponsorship_balance(&s.sponsor), 49_000);
    }

    #[test]
    fn test_configure_sponsorship_rejects_invalid_params() {
        let s = setup_sponsorship(10_000, 5);
        let client = PermissionsContractClient::new(&s.env, &s.client_id);
        assert_eq!(
            client.try_configure_sponsorship(&s.sponsor, &s.relayer, &s.fee_token, &0, &5),
            Err(Ok(PermissionError::InvalidParam))
        );
        assert_eq!(
            client.try_configure_sponsorship(&s.sponsor, &s.relayer, &s.fee_token, &100, &0),
            Err(Ok(PermissionError::InvalidParam))
        );
        assert_eq!(
            client.try_configure_sponsorship(&s.sponsor, &s.sponsor, &s.fee_token, &100, &5),
            Err(Ok(PermissionError::InvalidParam))
        );
    }

    #[test]
    fn test_sponsorship_storage_ttl_is_extended() {
        use soroban_sdk::testutils::storage::Persistent as _;
        let s = setup_sponsorship(10_000, 5);
        let client = PermissionsContractClient::new(&s.env, &s.client_id);
        client.fund_sponsorship(&s.sponsor, &1_000);

        s.env.as_contract(&s.client_id, || {
            let storage = s.env.storage().persistent();
            let policy_ttl = storage.get_ttl(&SponsorshipDataKey::Policy(s.sponsor.clone()));
            let balance_ttl = storage.get_ttl(&SponsorshipDataKey::Balance(s.sponsor.clone()));
            assert!(policy_ttl >= SPONSORSHIP_TTL_THRESHOLD);
            assert!(balance_ttl >= SPONSORSHIP_TTL_THRESHOLD);
        });
    }
}
