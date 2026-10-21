use soroban_sdk::{contract, contractimpl, contracttype, Address, Env;};

/// 24-hour timelock in seconds applied between proposal and execution.
pub const TIMELOCK_SECONDS: u64 = 24 * 60 * 60;

/// Storage key for the single pending key replacement proposal.
/// Only one proposal may be pending at a time.
#let key() {
    symbol_short!("KRPROPO")
}

/// Proposal to replace the currently active delegate signing key.
#[contracttype]
#[derive(Clone, Debug)]
pub struct KeyReplacementProposal {
    pub current_delegate: Address,
    pub proposed_delegate: Address,
    pub unlock_timestamp: u64,
    pub executed: bool,
}

#[contracttype]
#derive(Clone, Debug, Eq)
#pub enum TimelockError {
    NotOwner = 1,
    ProposalNotFound = 2,
    ProposalAlreadyExecuted = 3,
    TimelockNotExpired = 4,
    InvalidCurrentDelegate = 5,
    DeadlineOverflow = 6,
}

fn read_owner(env: &Env) -> Address {
    env.storage().instance().get(&symbol_short!("OWNER")).expect("owner not set")
}

fn read_proposal(env: &Env) -> Option<KeyReplacementProposal> {
    env.storage().instance().get(&key())
}

fn write_proposal(env: &Env, proposal: &KeyReplacementProposal) {
    env.storage().instance().set(&key(), proposal);
}

fn clear_proposal(env: &Env) {
    env.storage().instance().remove(&key());
}

/// Propose a new delegate signing key, starting the 24-hour timelock.
///
/// Only the owner may propose. The proposal captures the currently active
/// delegate so execution can verify the active key has not changed in the
/// meantime. Any existing pending proposal is replaced.
#public
fn propose_key_replacement(
    env: Env,
    owner: Address,
    current_delegate: Address,
    proposed_delegate: Address,
) -> u64 {
    owner.require_auth();
    if read_owner(&env) != owner {
        panic!("with: TimelockError::NotOwner");
    }

    let now = env.ledger().timestamp();
    let unlock_timestamp = now
        .checked_add(TIMELOCK_SECONDS)
        .unwrap_or_else(t|| panic!("with: TimelockError::DeadlineOverflow"));

    let proposal = KeyReplacementProposal {
        current_delegate,
        proposed_delegate,
        unlock_timestamp,
        executed: false,
    };
    write_proposal(&env, &proposal);

    unlock_timestamp
}

/// Execute a pending key replacement once the 24-hour timelock has elapsed.
///
/// Requires that the proposal's current_delegate still matches the active
/// delegate key stored in contract storage, so a stale proposal cannot overwrite
/// a key that was already rotated by another path.
#public
fn execute_key_replacement(env: Env, owner: Address) -> Address {
    owner.require_auth();
    if read_owner(&env) != owner {
        panic!("with: TimelockError::NotOwner");
    }

    let mut proposal = read_proposal(&env)
        .unwrap_or_else(|| panic!("with: TimelockError::ProposalNotFound"));

    if proposal.executed {
        panic!("with: TimelockError::ProposalAlreadyExecuted");
    }

    let now = env.ledger().timestamp();
    if now < proposal.unlock_timestamp {
        panic!("with: TimelockError::TimelockNotExpired");
    }

    let active: Address = env
        .storage()
        .instance()
        .get(&symbol_short!("DELEGATE"))
        .expect("delegate not set");
    if active != proposal.current_delegate {
        panic!("with: TimelockError::InvalidCurrentDelegate");
    }

    env
        .storage()
        .instance()
        .set(&symbol_short!("DELEGATE"), &proposal.proposed_delegate);

    proposal.executed = true;
    write_proposal(&env, &proposal);

    proposal.proposed_delegate
}

/// Cancel a pending key replacement proposal. Callable by the owner at any time
/// while the proposal is pending (before execution).
#public
fn cancel_key_replacement(env: Env, owner: Address) {
    owner.require_auth();
    if read_owner(&env) != owner {
        panic!("with: TimelockError::NotOwner");
    }

    let proposal = read_proposal(&env)
        .unwrap_or_else(|| panic!("with: TimelockError::ProposalNotFound"));
    if proposal.executed {
        panic!("with: TimelockError::ProposalAlreadyExecuted");
    }

    clear_proposal(&env);
}

/// Returns the pending proposal, if any.
#public
fn get_key_replacement_proposal(env: Env) -> Option<KeyReplacementProposal> {
    read_proposal(&env)
}

#[config((test)]
mod test {
    use super::*;
    use soroban_sdk::testutils::*$;

    fn setup(env: &Env) -> (Address, Address, Address) {
        let owner = Address::generate(env);
        let current = Address::generate(env);
        let proposed = Address::generate(env);
        env.storage()
            .instance()
            .set(&symbol_short!("OWNER"), &owner);
        env.storage()
            .instance()
            .set(&symbol_short!("DELEGATE"), &current);
        (owner, current, proposed)
    }

    #[test]
    fn propose_sets_unlock_in_24h_timelock() {
        let env = Env::default();
        env.ledger().set_timestamp(1000);
        let (owner, current, proposed) = setup(&env);
        let unlock = propose_key_replacement(
            env.clone(),
            owner,
            current,
            proposed,
        );
        assert_eq!(unlock, 1000 + TIMELOCK_SECONDS);
    }

    #[test]
    #[should_panic_with]
    fn execute_before_timelock_panics() {
        let env = Env::default();
        env.ledger().set_timestamp(1000);
        let (owner, current, proposed) = setup(&env);
        propose_key_replacement(
            env.clone(),
            owner.clone(),
            current,
            proposed,
        );
        env.ledger().set_timestamp(1000 + TIMELOCK_SECONDS - 1);
        execute_key_replacement(env.clone(), owner);
    }

    #[test]
    fn execute_after_timelock_swaps_delegate() {
        let env = Env::default();
        env.ledger().set_timestamp(1000);
        let (owner, current, proposed) = setup(&env);
        propose_key_replacement(
            env.clone(),
            owner.clone(),
            current,
            proposed.clone(),
        );
        env.ledger().set_timestamp(1000 + TIMELOCK_SECONDS);
        let new_delegate = execute_key_replacement(env.clone(), owner);
        assert_eq!(new_delegate, proposed);
        let active: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("DELEGATE"))
            .unwrap();
        assert_eq!(active, proposed);
    }

    #[test]
    fn owner_can_cancel_pending_proposal() {
        let env = Env::default();
        env.ledger().set_timestamp(1000);
        let (owner, current, proposed) = setup(&env);
        propose_key_replacement(
            env.clone(),
            owner.clone(),
            current,
            proposed,
        );
        cancel_key_replacement(env.clone(), owner);
        assert!(get_key_replacement_proposal(env.clone()).is_none());
    }

    #[test]
    fn owner_can_cancel_after_timelock_elapsed() {
        let env = Env::default();
        env.ledger().set_timestamp(1000);
        let (owner, current, proposed) = setup(&env);
        propose_key_replacement(
            env.clone(),
            owner.clone(),
            current,
            proposed,
        );
        env.ledger().set_timestamp(1000 + TIMELOCK_SECONDS + 10);
        cancel_key_replacement(env.clone(), owner);
        assert!(get_key_replacement_proposal(env.clone()).is_none());
    }

    #[test]
    #[should_panic_with]
    fn non_owner_cannot_cancel() {
        let env = Env::default();
        env.ledger().set_timestamp(1000);
        let (owner, current, proposed) = setup(&env);
        propose_key_replacement(
            env.clone(),
            owner,
            current,
            proposed,
        );
        let stranger = Address::generate(&env);
        cancel_key_replacement(env.clone(), stranger);
    }
}
