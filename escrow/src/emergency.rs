use super::*;

/// One year of dormancy, in seconds, before the emergency escape hatch opens.
pub const EMERGENCY_RECOVERY_DORMANCY_SECS: u64 = 31_536_000;

/// Emergency recovery entry point for escrows whose parties are permanently
/// unavailable (frozen account or lost key).
pub trait EmergencyRecoveryTrait {
    fn timelocked_emergency_recover(
        env: Env,
        order_id: u64,
        recovery_address: Address,
    ) -> Result<bool, EscrowError>;
}

/// Emitted when an inactive escrow is recovered through the emergency hatch.
#[contracttype]
#[derive(Clone, Debug)]
pub struct EmergencyFundRecovered {
    pub order_id: u64,
    pub recovery_address: Address,
    pub amount_recovered: i128,
    pub recovered_at: u64,
}

fn recover_funds(
    env: &Env,
    order_id: u64,
    recovery_address: Address,
) -> Result<bool, EscrowError> {
    if is_zero_address(env, &recovery_address) {
        return Err(EscrowError::InvalidAddress);
    }

    let escrow_key = DataKey::Escrow(order_id);
    let mut record: EscrowRecord = env
        .storage()
        .persistent()
        .get(&escrow_key)
        .ok_or(EscrowError::NotFound)?;

    let now = env.ledger().timestamp();
    if now <= record.created_at.saturating_add(EMERGENCY_RECOVERY_DORMANCY_SECS) {
        return Err(EscrowError::EmergencyRescueTimelockNotElapsed);
    }

    match record.status {
        EscrowStatus::Funded | EscrowStatus::Disputed | EscrowStatus::Inspection => {}
        _ => return Err(EscrowError::EscrowNotEligibleForRescue),
    }

    let amount = record
        .amount
        .saturating_sub(record.released_amount)
        .saturating_sub(record.refunded_amount);
    if amount <= 0 {
        return Err(EscrowError::InsufficientEscrowBalance);
    }

    let proposal_key = DataKey::EmergencyRescueProposal(order_id);
    let mut proposal: Option<EmergencyRescueProposal> =
        env.storage().persistent().get(&proposal_key);

    if let Some(ref existing) = proposal {
        if existing.executed {
            return Err(EscrowError::EmergencyRescueAlreadyExecuted);
        }
        if existing.recovery_destination != recovery_address {
            return Err(EscrowError::InvalidAddress);
        }
        if EscrowContract::valid_upgrade_approvals(env, &existing.approvals)
            < EscrowContract::upgrade_threshold(env)
        {
            return Err(EscrowError::EmergencyRescueThresholdNotMet);
        }
    } else {
        // No stored approvals: require every current admin to authorize this
        // call. The signer set must still meet the configured M-of-N
        // threshold, so a single key can never recover funds alone.
        let primary: Option<Address> = env.storage().instance().get(&DataKey::Admin);
        let co_admins: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AdminList)
            .unwrap_or_else(|| Vec::new(env));

        let mut signer_count = 0u32;
        if let Some(admin) = primary {
            admin.require_auth();
            signer_count += 1;
        }
        for admin in co_admins.iter() {
            admin.require_auth();
            signer_count += 1;
        }

        if signer_count < EscrowContract::upgrade_threshold(env) {
            return Err(EscrowError::EmergencyRescueThresholdNotMet);
        }
    }

    if let Some(ref mut existing) = proposal {
        existing.executed = true;
        env.storage().persistent().set(&proposal_key, existing);
    }

    record.status = EscrowStatus::Refunded;
    record.refunded_amount = record.refunded_amount.saturating_add(amount);
    record.updated_at = now;
    env.storage().persistent().set(&escrow_key, &record);
    env.storage().persistent().extend_ttl(
        &escrow_key,
        PERSISTENT_BUMP_THRESHOLD,
        PERSISTENT_BUMP_AMOUNT,
    );

    blend_yield::redeem(env, record.escrow_id);
    let token_client = soroban_sdk::token::Client::new(env, &record.token);
    token_client.transfer(&env.current_contract_address(), &recovery_address, &amount);

    env.events().publish(
        (symbol_short!("escrow"), symbol_short!("emg_recov"), order_id),
        EmergencyFundRecovered {
            order_id,
            recovery_address,
            amount_recovered: amount,
            recovered_at: now,
        },
    );

    Ok(true)
}

#[contractimpl]
impl EscrowContract {
    /// Recover escrowed funds after 365 days of dormancy.
    ///
    /// A pre-approved emergency rescue proposal must meet the configured
    /// multisig threshold. When no proposal exists, the full admin signer set
    /// must authorize the call and that set must itself meet the threshold.
    pub fn timelocked_emergency_recover(
        env: Env,
        order_id: u64,
        recovery_address: Address,
    ) -> Result<bool, EscrowError> {
        recover_funds(&env, order_id, recovery_address)
    }
}

impl EmergencyRecoveryTrait for EscrowContract {
    fn timelocked_emergency_recover(
        env: Env,
        order_id: u64,
        recovery_address: Address,
    ) -> Result<bool, EscrowError> {
        recover_funds(&env, order_id, recovery_address)
    }
}
