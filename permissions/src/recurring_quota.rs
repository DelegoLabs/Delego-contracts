//! Opt-in, immutable recurring quotas for persistent single-owner permissions.

use crate::{
    DataKey, PermissionError, PermissionRecord, PermissionStatus, PermissionsContract,
    PermissionsContractArgs, PermissionsContractClient,
};
use soroban_sdk::{contractimpl, contracttype, symbol_short, Address, Env};

/// Ledger-aligned recurring allowance with a non-renewable lifetime ceiling.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecurringQuotaConfig {
    /// Number of ledgers in an epoch; must be positive.
    pub epoch_duration_ledgers: u32,
    /// Maximum spend in one epoch; unused allowance never rolls over.
    pub per_epoch_quota_stroops: i128,
    /// Maximum aggregate spend over all epochs.
    pub lifetime_hard_cap_stroops: i128,
    /// Aggregate successful spending, including spending by descendants.
    pub cumulative_lifetime_spent: i128,
    /// Last accounted epoch: ledger sequence divided by duration.
    pub current_epoch_index: u32,
}

#[contracttype]
#[derive(Clone)]
pub(crate) struct RecurringQuotaState {
    pub config: RecurringQuotaConfig,
    pub epoch_spent: i128,
}

pub(crate) fn load(env: &Env, owner: &Address, delegate: &Address) -> Option<RecurringQuotaState> {
    env.storage()
        .persistent()
        .get(&DataKey::RecurringQuota(owner.clone(), delegate.clone()))
}

impl RecurringQuotaState {
    fn refresh(&mut self, ledger: u32) {
        let epoch = ledger / self.config.epoch_duration_ledgers;
        if epoch > self.config.current_epoch_index {
            self.config.current_epoch_index = epoch;
            self.epoch_spent = 0;
        }
    }

    fn remaining(&self, record_limit: i128) -> i128 {
        let epoch_limit = record_limit.min(self.config.per_epoch_quota_stroops);
        (epoch_limit - self.epoch_spent)
            .max(0)
            .min(self.config.lifetime_hard_cap_stroops - self.config.cumulative_lifetime_spent)
    }
}

/// Computes the effective allowance without writing renewal state.
pub(crate) fn remaining(env: &Env, record: &PermissionRecord) -> i128 {
    match load(env, &record.owner, &record.delegate) {
        Some(mut state) => {
            state.refresh(env.ledger().sequence());
            state.remaining(record.limit_total)
        }
        None => record.limit_total - record.spent,
    }
}

pub(crate) fn epoch_spent(env: &Env, record: &PermissionRecord) -> i128 {
    match load(env, &record.owner, &record.delegate) {
        Some(mut state) => {
            state.refresh(env.ledger().sequence());
            state.epoch_spent
        }
        None => record.spent,
    }
}

/// Debits both budgets atomically with the surrounding contract invocation.
/// Returns false for a legacy permission.
pub(crate) fn consume(
    env: &Env,
    record: &mut PermissionRecord,
    amount: i128,
) -> Result<bool, PermissionError> {
    let Some(mut state) = load(env, &record.owner, &record.delegate) else {
        return Ok(false);
    };
    state.refresh(env.ledger().sequence());
    if amount <= 0 {
        return Err(PermissionError::InvalidParam);
    }
    if amount > state.remaining(record.limit_total) {
        return Err(PermissionError::ExceedsTotalLimit);
    }
    state.config.cumulative_lifetime_spent = state
        .config
        .cumulative_lifetime_spent
        .checked_add(amount)
        .ok_or(PermissionError::ExceedsTotalLimit)?;
    state.epoch_spent = state
        .epoch_spent
        .checked_add(amount)
        .ok_or(PermissionError::ExceedsTotalLimit)?;
    record.spent = state.epoch_spent;
    record.last_reset_ledger = env.ledger().sequence();
    let key = DataKey::RecurringQuota(record.owner.clone(), record.delegate.clone());
    env.storage().persistent().set(&key, &state);
    env.events().publish(
        (
            symbol_short!("perm"),
            symbol_short!("quota"),
            record.owner.clone(),
            record.delegate.clone(),
        ),
        state.config,
    );
    Ok(true)
}

#[contractimpl]
impl PermissionsContract {
    /// Enables recurring spending on an unused, active persistent permission.
    ///
    /// Configuration is immutable for this owner/delegate pair. Re-grants,
    /// revocation, pruning and expiry renewal never erase its lifetime counter.
    /// Transfer is rejected for configured pairs; create a separate grant for
    /// a different delegate instead. Multi-owner and ephemeral grants are not
    /// supported. The existing per-transaction limit and all other policies apply.
    pub fn configure_recurring_quota(
        env: Env,
        owner: Address,
        delegate: Address,
        epoch_duration_ledgers: u32,
        per_epoch_quota_stroops: i128,
        lifetime_hard_cap_stroops: i128,
    ) -> Result<(), PermissionError> {
        owner.require_auth();
        if epoch_duration_ledgers == 0
            || per_epoch_quota_stroops <= 0
            || lifetime_hard_cap_stroops <= 0
        {
            return Err(PermissionError::InvalidParam);
        }
        if load(&env, &owner, &delegate).is_some() {
            return Err(PermissionError::AlreadyGranted);
        }
        let record: PermissionRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Permission(owner.clone(), delegate.clone()))
            .ok_or(PermissionError::PermissionNotFound)?;
        if record.status != PermissionStatus::Active
            || record.spent != 0
            || Self::get_usage_stats(env.clone(), owner.clone(), delegate.clone()).total_spent != 0
            || per_epoch_quota_stroops > record.limit_total
            || env
                .storage()
                .temporary()
                .has(&DataKey::EphemeralSession(owner.clone(), delegate.clone()))
        {
            return Err(PermissionError::InvalidParam);
        }
        if env.ledger().sequence() >= Self::effective_expiry(&env, &record) {
            return Err(PermissionError::Expired);
        }
        let config = RecurringQuotaConfig {
            epoch_duration_ledgers,
            per_epoch_quota_stroops,
            lifetime_hard_cap_stroops,
            cumulative_lifetime_spent: 0,
            current_epoch_index: env.ledger().sequence() / epoch_duration_ledgers,
        };
        env.storage().persistent().set(
            &DataKey::RecurringQuota(owner.clone(), delegate.clone()),
            &RecurringQuotaState {
                config: config.clone(),
                epoch_spent: 0,
            },
        );
        env.events().publish(
            (
                symbol_short!("perm"),
                symbol_short!("quota_cfg"),
                owner,
                delegate,
            ),
            config,
        );
        Ok(())
    }

    /// Reads the effective current epoch without mutating counters or storage.
    pub fn get_recurring_quota(
        env: Env,
        owner: Address,
        delegate: Address,
    ) -> Option<RecurringQuotaConfig> {
        load(&env, &owner, &delegate).map(|mut state| {
            state.refresh(env.ledger().sequence());
            state.config
        })
    }
}
