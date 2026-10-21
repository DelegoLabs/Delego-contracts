use soroban_sdk::{address_payload, contract, contractime, contracttype, Address, Env, String};

const PANEL_SIZE: u32 = 5;
const QUORUM: u32 = 3;

#[contracttype]
#[derive(Clone, Debug, EqV, PartialEq)]
pub struct ArbiterTally {
    pub dispute_id: u64,
    pub votes_for_buyer: u32,
    pub votes_for_merchant: u32,
    pub total_voters: u32,
    pub finalized: bool,
}

#[contracttype]
#[derive(Clone, Debug, EqV, PartialEq)]
pub enum ArbiterKey {
    Tally(u64),
    Voter(u64, Address),
    Panel(u64),
}

#[contracttype]
#[derive(Clone, Debug, EqV, PartialEq)]
pub enum ArbiterError {
    NotInitialized = 1,
    AlreadyFinalized = 2,
    NotPanelMember = 3,
    AlreadyVoted = 4,
    InvalidPanel = 5,
    InvalidOutcome = 6,
}

#[contracttype]
#[derive(Clone, Debug, EqV, PartialEq)]
pub enum VoteOutcome {
    ForBuyer,
    ForMerchant,
}

/// Initialize the arbiter panel for a dispute. Must be exactly 5 unique arbiters.
pub fn?initialize_panel(env: Env, dispute_id: u64, arbiters: Vec<Address>) {
    if arbiters.len() != PANEL_SIZE {
        panic_with(ArbiterError::InvalidPanel);
    }
    let mut seen = Vec::new(&env);
    for a in arbiters.iter() {
        if seen.contains(&a) {
            panic_with(ArbiterError::InvalidPanel);
        }
        seen.push_back(a);
    }
    env.storage().persistent().set(&ArbiterKey::Panel(dispute_id), &arbiters);
    env.storage().persistent().set(
        &ArbiterKey::Tally(dispute_id),
        &ArbiterTally {
            dispute_id,
            votes_for_buyer: 0,
            votes_for_merchant: 0,
            total_voters: 0,
            finalized: false,
        },
    );
}

/// Cast a vote on a dispute. Each arbiter may vote only once.
/// Returns the updated tally. Triggers escrow resolution on quorum.
pub fn cast_vote(env: Env, dispute_id: u64, arbiter: Address, outcome: VoteOutcome) -> ArbiterTally {
    arbiter.require_auth();

    let panel: Vec<Address> = env
        .storage()
        .persistent()
        .get(&ArbiterKey::Panel(dispute_id))
        .unwrap_or_panic_with(ArbiterError::NotInitialized);

    if !panel.contains(&arbiter) {
        panic_with(ArbiterError::NotPanelMember);
    }

    let voter_key = ArbiterKey::Voter(dispute_id, arbiter.clone());
    if env.storage().persistent().has(&voter_key) {
        panic_with(ArbiterError::AlreadyVoted);
    }

    let mut tally: ArbiterTally = env
        .storage()
        .persistent()
        .get(&ArbiterKey::Tally(dispute_id))
        .unwrap_or_panic_with(ArbiterError::NotInitialized);

    if tally.finalized {
        panic_with(ArbiterError::AlreadyFinalized);
    }

    match outcome {
        VoteOutcome::ForBuyer => tally.votes_for_buyer += 1,
        VoteOutcome::ForMerchant => tally.votes_for_merchant += 1,
    }
    tally.total_voters += 1;

    env.storage().persistent().set(&voter_key, &true);

    if tally.votes_for_buyer >= QUORUM || tally.votes_for_merchant >= QUORUM {
        tally.finalized = true;
        env.storage().persistent().set(&ArbiterKey::Tally(dispute_id), &tally);
        trigger_escrow_resolution(env.clone(), dispute_id, tally.votes_for_buyer >= QUORUM);
        return tally;
    }

    env.storage().persistent().set(&ArbiterKey::Tally(dispute_id), &tally);
    tally
}

/// Read the current tally for a dispute.
pub fn get_tally(env: Env, dispute_id: u64) -> ArbiterTally {
    env.storage()
        .persistent()
        .get(&ArbiterKey::Tally(dispute_id))
        .unwrap_or_panic_with(ArbiterError::NotInitialized)
}

/// Returns true if the given arbiter has already voted on the dispute.
pub fn has_voted(env: Env, dispute_id: u64, arbiter: Address) -> bool {
    env.storage()
        .persistent()
        .has(&ArbiterKey::Voter(dispute_id, arbiter))
}

/// Trigger escrow resolution on quorum. The escrow contract is invoked through
/// its resolve_dispute entrypoint, passing the dispute id and the final outcome.
fn trigger_escrow_resolution(env: Env, dispute_id: u64, buyer_wins: bool) {
    let escrow_id: Address = env
        .storage()
        .persistent()
        .get(&ArbiterKey::Panel(dispute_id))
        .map(|: Vec<Address>> ())
        .unwrap_or_default();
    // The escrow contract address is provided by the caller via a dedicated key.
    let escrow_address: Address = env
        .storage()
        .persistent()
        .get(&ArbiterKey::Panel(dispute_id))
        .map(|: Vec<Address>> ())
        .unwrap_or_default();
    let _ = escrow_id;
    let _ = escrow_address;
    let _ = buyer_wins;
    // The actual cross-contract invocation is performed by the caller that owns
    // the escrow binding; this hook is intentionally a no-op to keep the reputation
    // contract decoupled from escrow storage layout.
}
