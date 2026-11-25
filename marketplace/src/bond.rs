//! Merchant security bond staking and slash enforcement (issue #551).
//!
//! Merchants stake tokens into a bond vault held by the marketplace contract.
//! When a merchant commits fraud, the governance admin can slash part or all of
//! the stake and pay it to the victim or to a burn address.
//!
//! Each merchant has a single bond, denominated in the token of its first
//! deposit. Bonds are accounted per merchant, so a slash can only ever spend
//! the stake of the merchant being slashed.

use super::*;

/// Period a bond stays locked after a deposit or a slash (30 days).
pub const BOND_LOCK_PERIOD_SECS: u64 = 2_592_000;

/// Security bond staked by a merchant.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantBond {
    /// Merchant that staked the bond.
    pub merchant: Address,
    /// Token the bond is denominated in, fixed by the first deposit.
    pub token: Address,
    /// Amount currently staked in the vault.
    pub staked_amount: i128,
    /// Ledger timestamp until which the stake is locked; `0` when nothing is
    /// staked.
    pub locked_until: u64,
    /// Cumulative amount slashed from this bond.
    pub slashed_amount: i128,
}

/// Emitted when a merchant adds to its security bond.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BondDepositedEvent {
    /// Merchant that staked.
    pub merchant: Address,
    /// Token staked.
    pub token: Address,
    /// Amount added by this deposit.
    pub amount: i128,
    /// Total staked after this deposit.
    pub staked_amount: i128,
    /// Timestamp until which the stake is locked.
    pub locked_until: u64,
}

/// Emitted when the governance admin slashes a merchant's security bond.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BondSlashedEvent {
    /// Merchant that was slashed.
    pub merchant: Address,
    /// Token paid out.
    pub token: Address,
    /// Amount slashed.
    pub amount: i128,
    /// Address that received the slashed tokens.
    pub beneficiary: Address,
    /// Total still staked after the slash.
    pub staked_amount: i128,
    /// Cumulative amount slashed from this bond.
    pub slashed_amount: i128,
}

/// Storage keys owned by the bond vault. Kept in their own namespace so the
/// main `DataKey` enum does not grow.
#[contracttype]
#[derive(Clone)]
enum BondKey {
    /// Security bond of a merchant.
    SecurityBond(Address),
}

/// Merchant security bond staking (issue #551).
pub trait BondStakingTrait {
    /// Stakes `amount` of `token` from `merchant` into the bond vault.
    ///
    /// # Errors
    /// - [`MarketplaceError::ZeroBondAmount`] if `amount` is not positive.
    /// - [`MarketplaceError::InvalidParam`] if `token` differs from the token
    ///   the bond is denominated in, if the vault did not receive exactly
    ///   `amount`, or if the stake would overflow.
    fn deposit_bond(
        env: Env,
        merchant: Address,
        token: Address,
        amount: i128,
    ) -> Result<bool, MarketplaceError>;

    /// Slashes `slash_amount` from `merchant`'s bond and pays it to
    /// `beneficiary` (the victim or a burn address). Requires the governance
    /// admin's authorization.
    ///
    /// # Errors
    /// - [`MarketplaceError::NotInitialized`] if no admin is configured.
    /// - [`MarketplaceError::ZeroBondAmount`] if `slash_amount` is not positive.
    /// - [`MarketplaceError::InvalidParam`] if `beneficiary` is the merchant or
    ///   the vault itself, or if the accounting would overflow.
    /// - [`MarketplaceError::DepositNotFound`] if the merchant has no bond.
    /// - [`MarketplaceError::InsufficientDeposit`] if `slash_amount` exceeds
    ///   the staked balance.
    fn slash_bond(
        env: Env,
        merchant: Address,
        slash_amount: i128,
        beneficiary: Address,
    ) -> Result<bool, MarketplaceError>;

    /// Returns `merchant`'s security bond, if one was ever staked.
    fn get_security_bond(env: Env, merchant: Address) -> Option<MerchantBond>;
}

#[contractimpl]
impl BondStakingTrait for MarketplaceContract {
    fn deposit_bond(
        env: Env,
        merchant: Address,
        token: Address,
        amount: i128,
    ) -> Result<bool, MarketplaceError> {
        merchant.require_auth();

        if amount <= 0 {
            return Err(MarketplaceError::ZeroBondAmount);
        }

        let mut bond = load_bond(&env, &merchant).unwrap_or_else(|| MerchantBond {
            merchant: merchant.clone(),
            token: token.clone(),
            staked_amount: 0,
            locked_until: 0,
            slashed_amount: 0,
        });
        if bond.token != token {
            return Err(MarketplaceError::InvalidParam);
        }
        bond.staked_amount = bond
            .staked_amount
            .checked_add(amount)
            .ok_or(MarketplaceError::InvalidParam)?;
        bond.locked_until = next_lock(&env)?.max(bond.locked_until);
        save_bond(&env, &bond);

        // Verify the vault was credited in full, so a fee-on-transfer token
        // cannot leave a bond under-collateralized.
        let vault = env.current_contract_address();
        let token_client = soroban_sdk::token::Client::new(&env, &token);
        let balance_before = token_client.balance(&vault);
        token_client.transfer(&merchant, &vault, &amount);
        if token_client.balance(&vault).checked_sub(balance_before) != Some(amount) {
            return Err(MarketplaceError::InvalidParam);
        }

        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("bond_dep"),
                merchant.clone(),
            ),
            BondDepositedEvent {
                merchant,
                token,
                amount,
                staked_amount: bond.staked_amount,
                locked_until: bond.locked_until,
            },
        );

        Ok(true)
    }

    fn slash_bond(
        env: Env,
        merchant: Address,
        slash_amount: i128,
        beneficiary: Address,
    ) -> Result<bool, MarketplaceError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(MarketplaceError::NotInitialized)?;
        admin.require_auth();

        if slash_amount <= 0 {
            return Err(MarketplaceError::ZeroBondAmount);
        }
        // Paying the offender or the vault would make the slash meaningless or
        // strand the tokens.
        let vault = env.current_contract_address();
        if beneficiary == merchant || beneficiary == vault {
            return Err(MarketplaceError::InvalidParam);
        }

        let mut bond = load_bond(&env, &merchant).ok_or(MarketplaceError::DepositNotFound)?;
        if slash_amount > bond.staked_amount {
            return Err(MarketplaceError::InsufficientDeposit);
        }
        bond.staked_amount = bond
            .staked_amount
            .checked_sub(slash_amount)
            .ok_or(MarketplaceError::InvalidParam)?;
        bond.slashed_amount = bond
            .slashed_amount
            .checked_add(slash_amount)
            .ok_or(MarketplaceError::InvalidParam)?;
        // A slash re-locks whatever stake is left; an emptied bond has nothing
        // to lock.
        bond.locked_until = if bond.staked_amount == 0 {
            0
        } else {
            next_lock(&env)?.max(bond.locked_until)
        };
        save_bond(&env, &bond);

        let token_client = soroban_sdk::token::Client::new(&env, &bond.token);
        token_client.transfer(&vault, &beneficiary, &slash_amount);

        env.events().publish(
            (
                symbol_short!("mkplc"),
                symbol_short!("bond_slsh"),
                merchant.clone(),
            ),
            BondSlashedEvent {
                merchant,
                token: bond.token,
                amount: slash_amount,
                beneficiary,
                staked_amount: bond.staked_amount,
                slashed_amount: bond.slashed_amount,
            },
        );

        Ok(true)
    }

    fn get_security_bond(env: Env, merchant: Address) -> Option<MerchantBond> {
        load_bond(&env, &merchant)
    }
}

fn load_bond(env: &Env, merchant: &Address) -> Option<MerchantBond> {
    env.storage()
        .persistent()
        .get(&BondKey::SecurityBond(merchant.clone()))
}

fn save_bond(env: &Env, bond: &MerchantBond) {
    let key = BondKey::SecurityBond(bond.merchant.clone());
    env.storage().persistent().set(&key, bond);
    env.storage()
        .persistent()
        .extend_ttl(&key, PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
}

/// Lock expiry for a bond touched in the current ledger.
fn next_lock(env: &Env) -> Result<u64, MarketplaceError> {
    env.ledger()
        .timestamp()
        .checked_add(BOND_LOCK_PERIOD_SECS)
        .ok_or(MarketplaceError::InvalidParam)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        testutils::{storage::Persistent as _, Address as _, Ledger as _, MockAuth, MockAuthInvoke},
        token::{Client as TokenClient, StellarAssetClient},
        IntoVal,
    };

    const MERCHANT_FUNDS: i128 = 10_000;
    const NOW: u64 = 1_000;

    struct Fixture<'a> {
        env: Env,
        client: MarketplaceContractClient<'a>,
        contract_id: Address,
        admin: Address,
        merchant: Address,
        beneficiary: Address,
        token: Address,
    }

    impl Fixture<'_> {
        fn balance(&self, address: &Address) -> i128 {
            TokenClient::new(&self.env, &self.token).balance(address)
        }

        fn bond(&self) -> MerchantBond {
            self.client.get_security_bond(&self.merchant).unwrap()
        }

        fn slash_with_auth_of(&self, signer: &Address, amount: i128) -> bool {
            self.client
                .mock_auths(&[MockAuth {
                    address: signer,
                    invoke: &MockAuthInvoke {
                        contract: &self.contract_id,
                        fn_name: "slash_bond",
                        args: (self.merchant.clone(), amount, self.beneficiary.clone())
                            .into_val(&self.env),
                        sub_invokes: &[],
                    },
                }])
                .try_slash_bond(&self.merchant, &amount, &self.beneficiary)
                .is_ok()
        }
    }

    fn new_token(env: &Env, holder: &Address) -> Address {
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(env))
            .address();
        StellarAssetClient::new(env, &token).mint(holder, &MERCHANT_FUNDS);
        token
    }

    fn setup(env: &Env) -> Fixture<'_> {
        env.mock_all_auths();
        env.ledger().set_timestamp(NOW);
        let admin = Address::generate(env);
        let merchant = Address::generate(env);
        let contract_id = env.register(MarketplaceContract, (admin.clone(),));
        Fixture {
            env: env.clone(),
            client: MarketplaceContractClient::new(env, &contract_id),
            contract_id,
            admin,
            beneficiary: Address::generate(env),
            token: new_token(env, &merchant),
            merchant,
        }
    }

    #[test]
    fn deposit_moves_tokens_into_the_vault_and_records_the_bond() {
        let env = Env::default();
        let f = setup(&env);

        assert!(f.client.deposit_bond(&f.merchant, &f.token, &1_000));
        assert!(env.auths().iter().any(|(signer, _)| signer == &f.merchant));

        assert_eq!(f.balance(&f.merchant), MERCHANT_FUNDS - 1_000);
        assert_eq!(f.balance(&f.contract_id), 1_000);
        assert_eq!(
            f.bond(),
            MerchantBond {
                merchant: f.merchant.clone(),
                token: f.token.clone(),
                staked_amount: 1_000,
                locked_until: NOW + BOND_LOCK_PERIOD_SECS,
                slashed_amount: 0,
            }
        );
    }

    #[test]
    fn deposits_accumulate_and_extend_the_lock() {
        let env = Env::default();
        let f = setup(&env);

        f.client.deposit_bond(&f.merchant, &f.token, &1_000);
        env.ledger().set_timestamp(NOW + 500);
        f.client.deposit_bond(&f.merchant, &f.token, &250);

        let bond = f.bond();
        assert_eq!(bond.staked_amount, 1_250);
        assert_eq!(bond.locked_until, NOW + 500 + BOND_LOCK_PERIOD_SECS);
        assert_eq!(f.balance(&f.contract_id), 1_250);
    }

    #[test]
    fn deposit_extends_the_storage_ttl() {
        let env = Env::default();
        let f = setup(&env);

        f.client.deposit_bond(&f.merchant, &f.token, &1_000);

        let ttl = env.as_contract(&f.contract_id, || {
            env.storage()
                .persistent()
                .get_ttl(&BondKey::SecurityBond(f.merchant.clone()))
        });
        assert!(ttl >= PERSISTENT_BUMP_AMOUNT);
    }

    #[test]
    fn deposit_rejects_non_positive_amounts() {
        let env = Env::default();
        let f = setup(&env);

        for amount in [0, -1] {
            assert_eq!(
                f.client.try_deposit_bond(&f.merchant, &f.token, &amount),
                Err(Ok(MarketplaceError::ZeroBondAmount))
            );
        }
        assert_eq!(f.client.get_security_bond(&f.merchant), None);
    }

    #[test]
    fn deposit_rejects_a_second_token() {
        let env = Env::default();
        let f = setup(&env);
        let other_token = new_token(&env, &f.merchant);

        f.client.deposit_bond(&f.merchant, &f.token, &1_000);
        assert_eq!(
            f.client.try_deposit_bond(&f.merchant, &other_token, &1_000),
            Err(Ok(MarketplaceError::InvalidParam))
        );
        assert_eq!(f.bond().staked_amount, 1_000);
    }

    #[test]
    fn deposit_requires_the_merchants_authorization() {
        let env = Env::default();
        let f = setup(&env);

        env.set_auths(&[]);
        assert!(f
            .client
            .try_deposit_bond(&f.merchant, &f.token, &1_000)
            .is_err());
        assert_eq!(f.balance(&f.merchant), MERCHANT_FUNDS);
    }

    #[test]
    fn slash_pays_the_beneficiary_and_updates_the_bond() {
        let env = Env::default();
        let f = setup(&env);
        f.client.deposit_bond(&f.merchant, &f.token, &1_000);
        env.ledger().set_timestamp(NOW + 500);

        assert!(f.client.slash_bond(&f.merchant, &400, &f.beneficiary));
        assert!(env.auths().iter().any(|(signer, _)| signer == &f.admin));

        assert_eq!(f.balance(&f.beneficiary), 400);
        assert_eq!(f.balance(&f.contract_id), 600);
        let bond = f.bond();
        assert_eq!(bond.staked_amount, 600);
        assert_eq!(bond.slashed_amount, 400);
        assert_eq!(bond.locked_until, NOW + 500 + BOND_LOCK_PERIOD_SECS);
    }

    #[test]
    fn full_slash_clears_the_lock_and_keeps_the_history() {
        let env = Env::default();
        let f = setup(&env);
        f.client.deposit_bond(&f.merchant, &f.token, &1_000);

        f.client.slash_bond(&f.merchant, &400, &f.beneficiary);
        f.client.slash_bond(&f.merchant, &600, &f.beneficiary);

        assert_eq!(f.balance(&f.beneficiary), 1_000);
        let bond = f.bond();
        assert_eq!(bond.staked_amount, 0);
        assert_eq!(bond.slashed_amount, 1_000);
        assert_eq!(bond.locked_until, 0);
    }

    #[test]
    fn slash_is_limited_to_the_merchants_own_stake() {
        let env = Env::default();
        let f = setup(&env);
        let other = Address::generate(&env);
        StellarAssetClient::new(&env, &f.token).mint(&other, &5_000);
        f.client.deposit_bond(&other, &f.token, &5_000);
        f.client.deposit_bond(&f.merchant, &f.token, &1_000);

        assert_eq!(
            f.client.try_slash_bond(&f.merchant, &1_001, &f.beneficiary),
            Err(Ok(MarketplaceError::InsufficientDeposit))
        );
        assert_eq!(f.balance(&f.contract_id), 6_000);
    }

    #[test]
    fn slash_rejects_invalid_requests() {
        let env = Env::default();
        let f = setup(&env);

        assert_eq!(
            f.client.try_slash_bond(&f.merchant, &1, &f.beneficiary),
            Err(Ok(MarketplaceError::DepositNotFound))
        );

        f.client.deposit_bond(&f.merchant, &f.token, &1_000);
        for amount in [0, -1] {
            assert_eq!(
                f.client
                    .try_slash_bond(&f.merchant, &amount, &f.beneficiary),
                Err(Ok(MarketplaceError::ZeroBondAmount))
            );
        }
        for beneficiary in [&f.merchant, &f.contract_id] {
            assert_eq!(
                f.client.try_slash_bond(&f.merchant, &400, beneficiary),
                Err(Ok(MarketplaceError::InvalidParam))
            );
        }
        assert_eq!(f.bond().staked_amount, 1_000);
    }

    #[test]
    fn only_the_governance_admin_can_slash() {
        let env = Env::default();
        let f = setup(&env);
        f.client.deposit_bond(&f.merchant, &f.token, &1_000);

        assert!(!f.slash_with_auth_of(&f.merchant, 400));
        assert!(!f.slash_with_auth_of(&f.beneficiary, 400));
        assert_eq!(f.bond().staked_amount, 1_000);
        assert_eq!(f.balance(&f.beneficiary), 0);

        assert!(f.slash_with_auth_of(&f.admin, 400));
        assert_eq!(f.bond().staked_amount, 600);
        assert_eq!(f.balance(&f.beneficiary), 400);
    }
}
