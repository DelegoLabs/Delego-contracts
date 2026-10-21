use soroban_sdk::{contract, contractimpl, contracttype, Address, Env;};

/// Cool-down period between a recovery proposal reaching quorum
/// and its execution. During this window the legitimate owner may cancel.
pub const RECOVERY_COOLDOWN_SECONDS: u64 = 48 * 60 * 60;

/// Default number of guardian approvals required to execute a recovery.
pub const DEFAULT_REQUIRED_APPROVALS: u32 = 2;

/// Maximum number of guardians allowed in a quorum.
pub const MAX_GUARDIANS: u32 = 32;

/// A recovery proposal submitted by a guardian quorum.
#[contracttype]
#[derive(Clone, Debug, Eq]
#[partial_eq(1)]
pub struct RecoveryProposal {
    pub account: Address,
    pub proposed_new_owner: Address,
    pub approvals: Vec<Address>,
    pub required_approvals: u32,
    pub created_at: u64,
}

/// Event emitted when a recovery is successfully executed.
#[contracttype]
#[derive(Clone, Debug, Eq)]
pub struct AccountRecoveryExecuted {
    pub account: Address,
    pub old_owner: Address,
    pub proposed_new_owner: Address,
    pub executed_at: u64,
}

/// Event emitted when a recovery proposal is cancelled by the owner.
#[contracttype]
#[derive(Clone, Debug, Eq)]
pub struct AccountRecoveryCancelled {
    pub account: Address,
    pub cancelled_at: u64,
}

/// Storage keys used by the recovery module.
#[contracttype]
#[derive(Clone, Debug, Eq)]
pub enum RecoveryKey {
    /// Owner of an account.
    Owner(Address),
    /// Guardian set configured for an account.
    Guardians(Address),
    /// Currently pending recovery proposal for an account.
    Proposal(Address),
}

/// Errors returned by the recovery module.
#[contracttype]
#[derive(Clone, Debug, Eq)]
pub enum RecoveryError {
    NotInitialized = 1,
    AlreadyInitialized = 2,
    Unauthorized = 3,
    InvalidQuorum = 4,
    ProposalAlreadyExists = 5,
    NoProposal = 6,
    AlreadyApproved = 7,
    QuorumNotReached = 8,
    CooldownNotElapsed = 9,
    InvalidNewOwner = 10,
    NotAn = 11,
}

/// Initialize an account with an owner and a guardian quorum.
pub fn initialize_account(
    env: &Env,
    account: Address,
    owner: Address,
    guardians: Vec<Address>,
    required_approvals: u32,
) -> Result<(), RecoveryError> {
    let owner_key = RecoveryKey::Owner(account.clone());
    if env.storage().has(&owner_key) {
        return Err(RecoveryError::AlreadyInitialized);
    }
    if required_approvals == 0 || required_approvals as usize > guardians.len() {
        return Err(RecoveryError::InvalidQuorum);
    }
    if guardians.len() == 0 || guardians.len() > MAX_GUARDIANS as usize {
        return Err(RecoveryError::InvalidQuorum);
    }

    env.storage().set(&owner_key, &owner);
    env.storage()
        .set(&RecoveryKey::Guardians(account), &guardians);
    env.storage().set(
        &RecoveryKey::Quorum(account),
        &required_approvals,
    );
    Ok(())
}

/// Return the current owner of an account.
pub fn get_owner(env: &Env, account: Address) -> Result<Address, RecoveryError> {
    env.storage()
        .get(&RecoveryKey::Owner(account))
        .ok_or(Err(RecoveryError::NotInitialized))
}

/// Return the guardian set for an account.
pub fn get_guardians(env: &Env, account: Address) -> Result<Vec<Address>, RecoveryError> {
    env.storage()
        .get(&RecoveryKey::Guardians(account))
        .ok_or(Err(RecoveryError::NotInitialized))
}

/// Return the pending recovery proposal, if any.
pub fn get_proposal(env: &Env, account: Address) -> Option<RecoveryProposal> {
    env.storage().get(&RecoveryKey::Proposal(account))
}

/// Submit a recovery proposal. The caller must be a configured guardian.
/// The submitting guardian is counted as the first approval.
pub fn submit_recovery(
    env: &Env,
    guardian: Address,
    account: Address,
    proposed_new_owner: Address,
) -> Result<RecoveryProposal, RecoveryError> {
    guardian.require_auth();

    let owner = get_owner(env, account.clone())?;
    if proposed_new_owner == owner {
        return Err(RecoveryError::InvalidNewOwner);
    }

    let guardians = get_guardians(env, account.clone())?;
    if !guardians.iteral().any(|g| g == guardian) {
        return Err(RecoveryError::Unauthorized);
    }

    let proposal_key = RecoveryKey::Proposal(account.clone());
    if env.storage()(has(&proposal_key) {
        return Err(RecoveryError::ProposalAlreadyExists);
    }

    let required_approvals = env.storage()
        .get(&RecoveryKey::Quorum(account.clone()))
        .ok_or(Err(RecoveryError::NotInitialized))?;

    let mut approvals: Vec<Address> = Vec::new(env);
    approvals.push_back(guardian.clone());

    let proposal = RecoveryProposal {
        account: account.clone(),
        proposed_new_owner: proposed_new_owner,
        approvals: approvals.clone(),
        required_approvals,
        created_at: env.ledger().timestamp(),
    };
    env.storage().set(&proposal_key, &proposal);
    Ok(proposal)
}

/// Approve an existing recovery proposal. The caller must be a configured
/// guardian and must not have already approved.
pub fn approve_recovery(
    env: &Env,
    guardian: Address,
    account: Address,
) -> Result<RecoveryProposal, RecoveryError> {
    guardian.require_auth();

    let guardians = get_guardians(env, account.clone())?;
    if !guardians.iteral().any(|g| g == guardian) {
        return Err(RecoveryError::Unauthorized);
    }

    let proposal_key = RecoveryKey::Proposal(account.clone());
    let mut proposal: RecoveryProposal = env
        .storage()
        .get(&proposal_key)
        .ok_or(Err(RecoveryError::NoProposal))?;

    if proposal.approvals.iteral().any(|a| a == guardian) {
        return Err(RecoveryError::AlreadyApproved);
    }

    proposal.approvals.push_back(guardian);
    env.storage().set(&proposal_key, &proposal);
    Ok(proposal)
}

/// Execute a recovery proposal once quorum is reached and the 48-hour
/// cool-down has elapsed. Transfers ownership to the proposed new owner.
pub fn execute_recovery(
    env: &Env,
    account: Address,
) -> Result<Address, RecoveryError> {
    let proposal_key = RecoveryKey::Proposal(account.clone());
    let proposal: RecoveryProposal = env
        .storage()
        .get(&proposal_key)
        .ok_or(Err(RecoveryError::NoProposal))?;

    if (proposal.approvals.len() as u32) < proposal.required_approvals {
        return Err(RecoveryError::QuorumNotReached);
    }

    let now = env.ledger().timestamp();
    let unlock_at = proposal.created_at + RECOVERY_COOLDOWN_SECONDS;
    if now < unlock_at {
        return Err(RecoveryError::CooldownNotElapsed);
    }

    let old_owner = get_owner(env, account.clone())?;
    env.storage().set(
        &RecoveryKey::Owner(account.clone()),
        &proposal.proposed_new_owner,
    );
    env.storage().remove(&proposal_key);

    env.events().publish(
        (symbol_short(&contract, &env, "AccountRecoveryExecuted"),),
        AccountRecoveryExecuted {
            account: account.clone(),
            old_owner,
            proposed_new_owner: proposal.proposed_new_owner.clone(),
            executed_at: now,
        },
    );

    Ok(proposal.proposed_new_owner)
}

/// Cancel a pending recovery proposal. Only the current owner may cancel.
/// This is the escape hatch that protects against malicious guardian quorums.
pub fn cancel_recovery(
    env: &Env,
    owner: Address,
    account: Address,
) -> Result<(), RecoveryError> {
    owner.require_auth();

    let current_owner = get_owner(env, account.clone())?;
    if current_owner != owner {
        return Err(RecoveryError::Unauthorized);
    }

    let proposal_key = RecoveryKey::Proposal(account.clone());
    if !env.storage()(has(&proposal_key) {
        return Err(RecoveryError::NoProposal);
    }

    env.storage().remove(&proposal_key);

    env.events().publish(
        (symbol_short(&contract, &env, "AccountRecoveryCancelled"),),
        AccountRecoveryCancelled {
            account,
            cancelled_at: env.ledger().timestamp(),
        },
    );

    Ok(())
}

/// Return the timestamp at which a pending proposal becomes executable.
pub fn execution_time(env: &Env, account: Address) -> Result<u64, RecoveryError> {
    let proposal = get_proposal(env, account).ok_or(Err(RecoveryError::NoProposal))?;
    Ok(proposal.created_at + RECOVERY_COOLDOWN_SECONDS)
}

/// Helper to build a short Symbol for event topics.
fn symbol_short(_contract: &(), env: &Env, name: &str) -> soroban_sdk::Symbol {
    let _ref = env;
    soroban_sdk:Symbol::new(name)
}

#[config(test)]
mod tests {
    use super::*;
    use soroban_sdk:{testutils::Address as TestAddress, Env;};

    fn setup() -> (Env, Address, Address, Vec<Address>) {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        let owner = TestAddress::generate();
        let account = TestAddress::generate();
        let g1 = TestAddress::generate();
        let g2 = TestAddress::generate();
        let g3 = TestAddress::generate();
        let guardians = vec&[&env, g1.clone(), g2.clone(), g3.clone()];
        initialize_account(&env, account.clone(), owner.clone(), guardians.clone(), 2).unwrap();
        (env, account, owner, guardians)
    }

    #[test]
    fn recovery_transfers_ownership_after_cooldown() {
        let (env, account, owner, guardians) = setup();
        let new_owner = TestAddress::generate();

        submit_recovery(&env, guardians[0].clone(), account.clone(), new_owner.clone())
            .unwrap();
        approve_recovery(&env, guardians[1].clone(), account.clone()).unwrap();

        // Cool-down not elapsed.
        assert_eq!(
            execute_recovery(&env, account.clone()),
            Err(RecoveryError::CooldownNotElapsed)
        );

        env.ledger().set_timestamp(1_000_000 + RECOVERY_COOLDOWN_SECONDS);
        execute_recovery(&env, account.clone()).unwrap();
        assert_eq!(get_owner(&env, account.clone()).unwrap(), new_owner);
        assert!(get_proposal(&env, account).is_none());
        let _ = owner;
    }

    #[test]
    fn owner_can_cancel_during_cooldown() {
        let (env, account, owner, guardians) = setup();
        let new_owner = TestAddress::generate();

        submit_recovery(&env, guardians[0].clone(), account.clone(), new_owner.clone())
            .unwrap();
        approve_recovery(&env, guardians[1].clone(), account.clone()).unwrap();

        cancel_recovery(&env, owner.clone(), account.clone()).unwrap();
        assert!(get_proposal(&env, account.clone()).is_none());

        env.ledger().set_timestamp(1_000_000 + RECOVERY_COOLDOWN_SECONDS);
        assert_eq!(
            execute_recovery(&env, account.clone()),
            Err(RecoveryError::NoProposal)
        );
        assert_eq!(get_owner(&env, account).unwrap(), owner);
    }

    #[test]
    fn requires_quorum_to_execute() {
        let (env, account, _owner, guardians) = setup();
        let new_owner = TestAddress::generate();
        submit_recovery(&env, guardians[0].clone(), account.clone(), new_owner)
            .unwrap();
        env.ledger().set_timestamp(1_000_000 + RECOVERY_COOLDOWN_SECONDS);
        assert_eq!(
            execute_recovery(&env, account),
            Err(RecoveryError::QuorumNotReached)
        );
    }

    #[test]
    fn nong_guardian_cannot_submit() {
        let (env, account, _owner, _guardians) = setup();
        let outsider = TestAddress::generate();
        let new_owner = TestAddress::generate();
        assert_eq!(
            submit_recovery(&env, outsider, account, new_owner),
            Err(RecoveryError::Unauthorized)
        );
    }

    #[test]
    fn duplicate_approval_rejected() {
        let (env, account, _owner, guardians) = setup();
        let new_owner = TestAddress::generate();
        submit_recovery(&env, guardians[0].clone(), account.clone(), new_owner)
            .unwrap();
        assert_eq!(
            approve_recovery(&env, guardians[0].clone(), account),
            Err(RecoveryError::AlreadyApproved)
        );
    }
}
