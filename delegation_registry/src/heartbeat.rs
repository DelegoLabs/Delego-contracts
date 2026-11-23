//! Keeper heartbeat verification (issue #576).
//!
//! Keeper bots that execute recurring subscription payments on behalf of a
//! delegation must prove liveness by calling [`submit_heartbeat`] at least
//! once every [`KEEPER_HEARTBEAT_TIMEOUT_SECS`] (7 days). A delegation owner
//! assigns a primary and a secondary keeper; when the primary misses its
//! heartbeat window, [`resolve_execution_keeper`] routes execution to the
//! secondary keeper and emits a `KeeperHeartbeatMissed` alert.
//!
//! [`submit_heartbeat`]: DelegationRegistry::submit_heartbeat
//! [`resolve_execution_keeper`]: DelegationRegistry::resolve_execution_keeper

use soroban_sdk::{contractimpl, contracttype, symbol_short, Address, Env};

use crate::{
    is_zero_address, DataKey, DelegationError, DelegationRegistry, DelegationRegistryArgs,
    DelegationRegistryClient, EVENT_TOPIC_CONTRACT, PERSISTENT_BUMP_AMOUNT,
    PERSISTENT_BUMP_THRESHOLD,
};

/// Maximum number of seconds a keeper may go without a heartbeat before it is
/// considered dead. 604 800 s = 7 days.
pub const KEEPER_HEARTBEAT_TIMEOUT_SECS: u64 = 604_800;

/// Latest heartbeat recorded for a keeper.
#[contracttype]
#[allow(missing_docs)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeeperHeartbeat {
    /// The keeper this heartbeat belongs to.
    pub keeper: Address,
    /// Ledger timestamp of the keeper's most recent heartbeat.
    pub last_heartbeat_timestamp: u64,
    /// `false` once the keeper has been observed missing its heartbeat window.
    pub is_active: bool,
}

/// Primary/secondary keeper assignment for a delegation.
#[contracttype]
#[allow(missing_docs)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationKeepers {
    /// Keeper that executes the delegation while it is alive.
    pub primary: Address,
    /// Fallback keeper used when the primary misses its heartbeat window.
    pub secondary: Address,
}

/// Emitted when a keeper is detected to have missed its heartbeat window and
/// execution for a delegation is routed to the secondary keeper.
#[contracttype]
#[allow(missing_docs)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeeperHeartbeatMissed {
    /// Delegation whose execution was re-routed.
    pub delegation_id: u64,
    /// Keeper that missed its heartbeat.
    pub keeper: Address,
    /// Last heartbeat timestamp recorded for `keeper` (0 if it never pinged).
    pub last_heartbeat_timestamp: u64,
    /// Keeper that execution was routed to.
    pub fallback_keeper: Address,
    /// Ledger timestamp at which the miss was detected.
    pub timestamp: u64,
}

/// Returns `true` when `heartbeat` is still within the liveness window at `now`.
///
/// A keeper is dead when `last_heartbeat + KEEPER_HEARTBEAT_TIMEOUT_SECS < now`.
pub fn is_heartbeat_live(heartbeat: &KeeperHeartbeat, now: u64) -> bool {
    heartbeat
        .last_heartbeat_timestamp
        .saturating_add(KEEPER_HEARTBEAT_TIMEOUT_SECS)
        >= now
}

fn load_heartbeat(env: &Env, keeper: &Address) -> Option<KeeperHeartbeat> {
    env.storage()
        .persistent()
        .get(&DataKey::KeeperHeartbeat(keeper.clone()))
}

fn store_heartbeat(env: &Env, heartbeat: &KeeperHeartbeat) {
    let key = DataKey::KeeperHeartbeat(heartbeat.keeper.clone());
    env.storage().persistent().set(&key, heartbeat);
    env.storage()
        .persistent()
        .extend_ttl(&key, PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
}

fn keeper_is_live(env: &Env, keeper: &Address, now: u64) -> bool {
    load_heartbeat(env, keeper)
        .map(|hb| is_heartbeat_live(&hb, now))
        .unwrap_or(false)
}

#[contractimpl]
#[allow(missing_docs)]
impl DelegationRegistry {
    /// Records a liveness heartbeat for `keeper` at the current ledger
    /// timestamp and marks it active again.
    ///
    /// The keeper must authorize the call so nobody can keep a dead keeper
    /// artificially alive.
    pub fn submit_heartbeat(env: Env, keeper: Address) -> KeeperHeartbeat {
        keeper.require_auth();

        let heartbeat = KeeperHeartbeat {
            keeper: keeper.clone(),
            last_heartbeat_timestamp: env.ledger().timestamp(),
            is_active: true,
        };
        store_heartbeat(&env, &heartbeat);

        env.events().publish(
            (EVENT_TOPIC_CONTRACT, symbol_short!("kpr_hb"), keeper),
            heartbeat.clone(),
        );

        heartbeat
    }

    /// Returns `true` when `keeper` has submitted a heartbeat within the last
    /// [`KEEPER_HEARTBEAT_TIMEOUT_SECS`]. Keepers that never pinged are dead.
    pub fn check_keeper_liveness(env: Env, keeper: Address) -> bool {
        keeper_is_live(&env, &keeper, env.ledger().timestamp())
    }

    /// Returns the stored heartbeat for `keeper`, with `is_active` reflecting
    /// liveness at the current ledger timestamp.
    pub fn get_keeper_heartbeat(env: Env, keeper: Address) -> Option<KeeperHeartbeat> {
        let now = env.ledger().timestamp();
        load_heartbeat(&env, &keeper).map(|mut hb| {
            hb.is_active = hb.is_active && is_heartbeat_live(&hb, now);
            hb
        })
    }

    /// Assigns the primary and secondary keepers for a delegation.
    ///
    /// # Errors
    ///
    /// * [`DelegationError::NotFound`] - the delegation does not exist.
    /// * [`DelegationError::InvalidParam`] - a keeper is the zero address or
    ///   `primary == secondary`.
    pub fn set_delegation_keepers(
        env: Env,
        delegation_id: u64,
        primary: Address,
        secondary: Address,
    ) -> Result<(), DelegationError> {
        let record = Self::load_delegation_by_id(&env, delegation_id)
            .ok_or(DelegationError::NotFound)?;
        record.owner.require_auth();

        if primary == secondary
            || is_zero_address(&env, &primary)
            || is_zero_address(&env, &secondary)
        {
            return Err(DelegationError::InvalidParam);
        }

        let key = DataKey::DelegationKeepers(delegation_id);
        env.storage()
            .persistent()
            .set(&key, &DelegationKeepers { primary, secondary });
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);

        Ok(())
    }

    /// Returns the keeper assignment for a delegation, if any.
    pub fn get_delegation_keepers(env: Env, delegation_id: u64) -> Option<DelegationKeepers> {
        env.storage()
            .persistent()
            .get(&DataKey::DelegationKeepers(delegation_id))
    }

    /// Returns the keeper that should execute `delegation_id` right now.
    ///
    /// The primary keeper is returned while it is alive. When the primary has
    /// missed its heartbeat window (`last_heartbeat + 604800 < now`) it is
    /// marked inactive, a `KeeperHeartbeatMissed` event is emitted and the
    /// secondary keeper is returned instead.
    ///
    /// # Errors
    ///
    /// * [`DelegationError::KeepersNotConfigured`] - no keepers are assigned.
    /// * [`DelegationError::NoLiveKeeper`] - both keepers are dead.
    pub fn resolve_execution_keeper(
        env: Env,
        delegation_id: u64,
    ) -> Result<Address, DelegationError> {
        let keepers: DelegationKeepers = env
            .storage()
            .persistent()
            .get(&DataKey::DelegationKeepers(delegation_id))
            .ok_or(DelegationError::KeepersNotConfigured)?;
        let now = env.ledger().timestamp();

        if keeper_is_live(&env, &keepers.primary, now) {
            return Ok(keepers.primary);
        }

        let last_heartbeat_timestamp = match load_heartbeat(&env, &keepers.primary) {
            Some(mut hb) => {
                let last = hb.last_heartbeat_timestamp;
                if hb.is_active {
                    hb.is_active = false;
                    store_heartbeat(&env, &hb);
                }
                last
            }
            None => 0,
        };

        if !keeper_is_live(&env, &keepers.secondary, now) {
            return Err(DelegationError::NoLiveKeeper);
        }

        env.events().publish(
            (
                EVENT_TOPIC_CONTRACT,
                symbol_short!("kpr_miss"),
                delegation_id,
            ),
            KeeperHeartbeatMissed {
                delegation_id,
                keeper: keepers.primary.clone(),
                last_heartbeat_timestamp,
                fallback_keeper: keepers.secondary.clone(),
                timestamp: now,
            },
        );

        Ok(keepers.secondary)
    }
}
