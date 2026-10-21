use super::*;
use soroban_sdk::xdr::ToXdr;
use soroban_sdk::Bytes;
use soroban_sdk::BytesN;

/// Minimum wall-clock review period for critical administrative changes.
pub const ADMIN_ACTION_DELAY_SECONDS: u64 = 86_400;
/// Approximate day of ledgers; the timestamp gate also enforces a real day.
pub const ADMIN_ACTION_DELAY_LEDGERS: u32 = 17_280;

/// Supported changes. The enum discriminant and every argument are committed.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdminAction {
    /// Change the fee rate without changing its recipient.
    Fee(u32),
    /// Change the fee rate and recipient together.
    FeeConfig(u32, Address),
    /// Replace the multi-treasury fee distribution (empty clears it).
    FeeDistribution(Vec<TreasuryShare>),
    /// Add a token contract to the whitelist.
    AddToken(Address),
    /// Remove a token contract from the whitelist.
    RemoveToken(Address),
    /// Rotate the guardian after the existing guardian's veto period.
    Guardian(Address),
    /// Authorize a courier public key to sign delivery vouchers.
    AddCourier(BytesN<32>),
    /// Revoke a courier public key.
    RemoveCourier(BytesN<32>),
}

/// Public commitment and review state for a queued administrative change.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingAdminAction {
    /// Operation name for indexers.
    pub action_type: Symbol,
    /// SHA-256 of the complete typed action's XDR encoding.
    pub payload: BytesN<32>,
    /// Earliest ledger at which execution is permitted.
    pub unlock_ledger: u32,
    /// A veto permanently prevents execution of this proposal ID.
    pub is_vetoed: bool,
}

/// Full proposal, including the timestamp gate and committed arguments.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuedAdminAction {
    /// Review state and payload commitment.
    pub pending: PendingAdminAction,
    /// Arguments used at execution, never supplied by the executor.
    pub action: AdminAction,
    /// Earliest timestamp at which execution is permitted.
    pub unlock_timestamp: u64,
}

#[contracttype]
#[derive(Clone)]
enum AdminActionKey {
    Guardian,
    LastId,
    Proposal(u64),
    Current(BytesN<32>),
    Courier(BytesN<32>),
    CourierCount,
}

fn commitment(env: &Env, action: &AdminAction) -> BytesN<32> {
    env.crypto().sha256(&action.clone().to_xdr(env)).into()
}

fn action_type(action: &AdminAction) -> Symbol {
    match action {
        AdminAction::Fee(_) => symbol_short!("fee"),
        AdminAction::FeeConfig(_, _) => symbol_short!("fee_cfg"),
        AdminAction::FeeDistribution(_) => symbol_short!("fee_dist"),
        AdminAction::AddToken(_) => symbol_short!("token_add"),
        AdminAction::RemoveToken(_) => symbol_short!("token_del"),
        AdminAction::Guardian(_) => symbol_short!("guardian"),
        AdminAction::AddCourier(_) => symbol_short!("cour_add"),
        AdminAction::RemoveCourier(_) => symbol_short!("cour_del"),
    }
}

fn validate(env: &Env, action: &AdminAction) -> Result<(), EscrowError> {
    match action {
        AdminAction::Fee(bps) | AdminAction::FeeConfig(bps, _) if *bps > 1000 => {
            return Err(EscrowError::InvalidFeeBps);
        }
        AdminAction::FeeDistribution(shares) => {
            if shares.len() > MAX_TREASURIES {
                return Err(EscrowError::MaxTreasuriesExceeded);
            }
            let mut total = 0u32;
            for share in shares.iter() {
                if is_zero_address(env, &share.treasury) {
                    return Err(EscrowError::InvalidAddress);
                }
                if share.bps == 0 {
                    return Err(EscrowError::InvalidFeeBps);
                }
                total = total
                    .checked_add(share.bps)
                    .ok_or(EscrowError::InvalidFeeBps)?;
            }
            if total > 1000 {
                return Err(EscrowError::InvalidFeeBps);
            }
        }
        _ => {}
    }
    match action {
        AdminAction::FeeConfig(_, address)
        | AdminAction::AddToken(address)
        | AdminAction::RemoveToken(address)
        | AdminAction::Guardian(address)
            if is_zero_address(env, address) =>
        {
            Err(EscrowError::InvalidAddress)
        }
        AdminAction::AddCourier(key) | AdminAction::RemoveCourier(key) => {
            let _ = key;
            Ok(())
        }
        _ => Ok(()),
    }
}

fn store(env: &Env, id: u64, proposal: &QueuedAdminAction) {
    let key = AdminActionKey::Proposal(id);
    env.storage().persistent().set(&key, proposal);
    env.storage()
        .persistent()
        .extend_ttl(&key, PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
}

/// Consume the approval only inside the transaction applying the exact action.
/// A later error rolls this removal back with the rest of the invocation.
pub(super) fn consume(env: &Env, action: AdminAction) -> Result<(), EscrowError> {
    let digest = commitment(env, &action);
    let index = AdminActionKey::Current(digest);
    let id: u64 = env
        .storage()
        .persistent()
        .get(&index)
        .ok_or(EscrowError::AdminActionNotFound)?;
    let proposal = EscrowContract::get_admin_action(env.clone(), id)
        .ok_or(EscrowError::AdminActionNotFound)?;
    if proposal.pending.is_vetoed {
        return Err(EscrowError::AdminActionVetoed);
    }
    if env.ledger().sequence() < proposal.pending.unlock_ledger
        || env.ledger().timestamp() < proposal.unlock_timestamp
    {
        return Err(EscrowError::AdminActionLocked);
    }
    env.storage().persistent().remove(&index);
    env.storage()
        .persistent()
        .remove(&AdminActionKey::Proposal(id));
    env.events()
        .publish((symbol_short!("admin"), symbol_short!("executed")), id);
    Ok(())
}

#[contractimpl]
impl EscrowContract {
    /// Bootstrap once, with primary-admin and guardian consent. Thereafter use
    /// a queued `Guardian` action; the old guardian retains the right to veto.
    pub fn set_security_guardian(
        env: Env,
        admin: Address,
        guardian: Address,
    ) -> Result<(), EscrowError> {
        admin.require_auth();
        Self::require_role(&env, &admin, ContractRole::Admin)?;
        if env.storage().instance().has(&AdminActionKey::Guardian) {
            return Err(EscrowError::GuardianAlreadySet);
        }
        validate(&env, &AdminAction::Guardian(guardian.clone()))?;
        guardian.require_auth();
        env.storage()
            .instance()
            .set(&AdminActionKey::Guardian, &guardian);
        env.events().publish(
            (symbol_short!("admin"), symbol_short!("guardian")),
            guardian,
        );
        Ok(())
    }

    /// Current security council address (may itself be a multisig contract).
    pub fn get_security_guardian(env: Env) -> Option<Address> {
        env.storage().instance().get(&AdminActionKey::Guardian)
    }

    /// Queue validated arguments for review. Duplicate live proposals are
    /// rejected. Re-proposing a vetoed action creates a fresh ID and full delay.
    pub fn queue_admin_action(
        env: Env,
        admin: Address,
        action: AdminAction,
    ) -> Result<u64, EscrowError> {
        admin.require_auth();
        let authorized = match &action {
            AdminAction::Fee(_) | AdminAction::FeeConfig(_, _) | AdminAction::FeeDistribution(_) =>
                Self::has_role(env.clone(), admin.clone(), ContractRole::FeeManager),
            _ => Self::is_admin(env.clone(), admin.clone()),
        };
        if !authorized {
            return Err(EscrowError::Unauthorized);
        }
        if Self::get_security_guardian(env.clone()).is_none() {
            return Err(EscrowError::GuardianNotSet);
        }
        validate(&env, &action)?;
        let digest = commitment(&env, &action);
        let index = AdminActionKey::Current(digest.clone());
        if let Some(id) = env.storage().persistent().get::<_, u64>(&index) {
            if let Some(old) = Self::get_admin_action(env.clone(), id) {
                if !old.pending.is_vetoed {
                    return Err(EscrowError::AdminActionAlreadyQueued);
                }
            }
        }
        let id = env
            .storage()
            .instance()
            .get::<_, u64>(&AdminActionKey::LastId)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(EscrowError::AdminActionOverflow)?;
        let proposal = QueuedAdminAction {
            pending: PendingAdminAction {
                action_type: action_type(&action),
                payload: digest,
                unlock_ledger: env
                    .ledger()
                    .sequence()
                    .checked_add(ADMIN_ACTION_DELAY_LEDGERS)
                    .ok_or(EscrowError::AdminActionOverflow)?,
                is_vetoed: false,
            },
            action,
            unlock_timestamp: env
                .ledger()
                .timestamp()
                .checked_add(ADMIN_ACTION_DELAY_SECONDS)
                .ok_or(EscrowError::AdminActionOverflow)?,
        };
        // Keep the contract and its guardian alive throughout the review window.
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_BUMP_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
        store(&env, id, &proposal);
        env.storage().persistent().set(&index, &id);
        env.storage().persistent().extend_ttl(
            &index,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
        env.storage().instance().set(&AdminActionKey::LastId, &id);
        env.events().publish(
            (symbol_short!("admin"), symbol_short!("queued")),
            (id, proposal),
        );
        Ok(id)
    }

    /// Inspect pending arguments and both execution thresholds. Executed or
    /// expired proposals return `None`; expiry never authorizes execution.
    pub fn get_admin_action(env: Env, id: u64) -> Option<QueuedAdminAction> {
        env.storage()
            .persistent()
            .get(&AdminActionKey::Proposal(id))
    }

    /// Veto a proposal, including after unlock until it has been executed.
    pub fn veto_admin_action(env: Env, guardian: Address, id: u64) -> Result<(), EscrowError> {
        guardian.require_auth();
        if Self::get_security_guardian(env.clone()) != Some(guardian) {
            return Err(EscrowError::Unauthorized);
        }
        let mut proposal =
            Self::get_admin_action(env.clone(), id).ok_or(EscrowError::AdminActionNotFound)?;
        proposal.pending.is_vetoed = true;
        store(&env, id, &proposal);
        env.events()
            .publish((symbol_short!("admin"), symbol_short!("vetoed")), id);
        Ok(())
    }

    /// Execute the stored proposal with current admin authorization. Legacy
    /// setters use the same consume gate and cannot bypass this review period.
    pub fn execute_admin_action(env: Env, admin: Address, id: u64) -> Result<bool, EscrowError> {
        admin.require_auth();
        let proposal =
            Self::get_admin_action(env.clone(), id).ok_or(EscrowError::AdminActionNotFound)?;
        let authorized = match &proposal.action {
            AdminAction::Fee(_) | AdminAction::FeeConfig(_, _) | AdminAction::FeeDistribution(_) =>
                Self::has_role(env.clone(), admin.clone(), ContractRole::FeeManager),
            _ => Self::is_admin(env.clone(), admin.clone()),
        };
        if !authorized {
            return Err(EscrowError::Unauthorized);
        }
        if proposal.pending.is_vetoed {
            return Err(EscrowError::AdminActionVetoed);
        }
        match proposal.action.clone() {
            AdminAction::Fee(bps) => Self::update_fee(env, admin, bps),
            AdminAction::FeeDistribution(shares) => Self::set_fee_distribution(env, admin, shares),
            AdminAction::AddToken(token) => Self::add_token(env, admin, token),
            AdminAction::RemoveToken(token) => Self::remove_token(env, admin, token),
            AdminAction::FeeConfig(fee_bps, treasury) => {
                admin.require_auth();
                consume(&env, proposal.action)?;
                env.storage()
                    .instance()
                    .set(&DataKey::FeeConfig, &FeeConfig { fee_bps, treasury });
                // Remove obsolete pre-upgrade scheduling state, never auto-apply it.
                env.storage()
                    .instance()
                    .remove(&DataKey::ScheduledFeeUpdate);
                Ok(true)
            }
            AdminAction::Guardian(guardian) => {
                admin.require_auth();
                guardian.require_auth();
                consume(&env, proposal.action)?;
                env.storage()
                    .instance()
                    .set(&AdminActionKey::Guardian, &guardian);
                env.events().publish(
                    (symbol_short!("admin"), symbol_short!("guardian")),
                    guardian,
                );
                Ok(true)
            }
            AdminAction::AddCourier(key) => {
                admin.require_auth();
                consume(&env, proposal.action)?;
                Self::add_courier(env, key);
                Ok(true)
            }
            AdminAction::RemoveCourier(key) => {
                admin.require_auth();
                consume(&env, proposal.action)?;
                Self::remove_courier(env, key);
                Ok(true)
            }
        }
    }

    /// Register an authorized courier public key. Only reachable through the
    /// queued admin action gate, so it inherits the review period and veto.
    pub(super) fn add_courier(env: Env, key: BytesN<32>) {
        let storage_key = AdminActionKey::Courier(key.clone());
        if !env.storage().persistent().has(&storage_key) {
            env.storage().persistent().set(&storage_key, &true);
            env.storage().persistent().extend_ttl(
                &storage_key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );
            let count: u32 = env
                .storage()
                .instance()
                .get(&AdminActionKey::CourierCount)
                .unwrap_or(0);
            env.storage()
                .instance()
                .set(&AdminActionKey::CourierCount, &(count + 1));
        }
        env.events().publish(
            (symbol_short!("courier"), symbol_short!("added")),
            key,
        );
    }

    /// Revoke a courier public key so future vouchers from it are rejected.
    pub(super) fn remove_courier(env: Env, key: BytesN<32>) {
        let storage_key = AdminActionKey::Courier(key.clone());
        if env.storage().persistent().has(&storage_key) {
            env.storage().persistent().remove(&storage_key);
            let count: u32 = env
                .storage()
                .instance()
                .get(&AdminActionKey::CourierCount)
                .unwrap_or(0);
            env.storage()
                .instance()
                .set(&AdminActionKey::CourierCount, &count.saturating_sub(1));
        }
        env.events().publish(
            (symbol_short!("courier"), symbol_short!("removed")),
            key,
        );
    }

    /// Whether a courier public key is currently authorized.
    pub fn is_authorized_courier(env: Env, key: BytesN<32>) -> bool {
        env.storage()
            .persistent()
            .get::<_, bool>(&AdminActionKey::Courier(key))
            .unwrap_or(false)
    }
}
