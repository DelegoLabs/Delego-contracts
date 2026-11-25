//! Arbiter panels, voting, deadline penalties, and reassignment (issue #774).
//!
//! Each dispute has a five-member panel. Three matching votes finalize the
//! dispute. The currently assigned arbiter must vote within 43,200 ledgers
//! (~72 hours); a missed deadline records an inactivity strike and reassigns
//! the dispute to the next eligible panel member.

use soroban_sdk::{contracttype, symbol_short, Address, Env, IntoVal, TryFromVal, Val, Vec};

use crate::{ArbiterTally, ReputationError, PERSISTENT_BUMP_AMOUNT, PERSISTENT_BUMP_THRESHOLD};

/// Number of arbiters on a dispute panel.
pub const PANEL_SIZE: u32 = 5;
/// Matching votes required to finalize a dispute.
pub const QUORUM: u32 = 3;
/// ~72 hours at five seconds per ledger.
pub const ARBITER_DEADLINE_LEDGERS: u32 = 43_200;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArbiterKey {
    /// `dispute_id -> Vec<Address>`.
    Panel(u64),
    /// `dispute_id -> ArbiterTally`.
    Tally(u64),
    /// `dispute_id -> DisputeAssignment`. Kept separate from `Tally` so the two
    /// value types can never be decoded into one another.
    Assignment(u64),
    /// `(dispute_id, arbiter) -> true` once that arbiter voted.
    Voter(u64, Address),
    /// `(dispute_id, arbiter) -> true` once that arbiter missed a deadline.
    Penalized(u64, Address),
    /// `arbiter -> lifetime inactivity strikes`.
    PenaltyCount(Address),
}

/// Tracks the arbiter currently expected to vote on a dispute.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeAssignment {
    pub dispute_id: u64,
    pub assigned_arbiter: Address,
    pub assigned_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VoteOutcome {
    ForBuyer,
    ForMerchant,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArbitrationPanelInitializedEvent {
    pub dispute_id: u64,
    pub arbiters: Vec<Address>,
    pub assigned_arbiter: Address,
    pub assigned_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArbitrationVoteCastEvent {
    pub dispute_id: u64,
    pub arbiter: Address,
    pub outcome: VoteOutcome,
    pub tally: ArbiterTally,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArbitrationFinalizedEvent {
    pub dispute_id: u64,
    pub buyer_wins: bool,
    pub tally: ArbiterTally,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArbitrationDeadlinePenaltyEvent {
    pub dispute_id: u64,
    pub penalized_arbiter: Address,
    pub replacement_arbiter: Address,
    pub strike_count: u32,
    pub observed_ledger: u32,
}

fn bump(env: &Env, key: &ArbiterKey) {
    if env.storage().persistent().has(key) {
        env.storage().persistent().extend_ttl(
            key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
    }
}

fn get<T: TryFromVal<Env, Val>>(env: &Env, key: &ArbiterKey) -> Option<T> {
    let value: Option<T> = env.storage().persistent().get(key);
    if value.is_some() {
        bump(env, key);
    }
    value
}

fn set<T: IntoVal<Env, Val>>(env: &Env, key: &ArbiterKey, value: &T) {
    env.storage().persistent().set(key, value);
    bump(env, key);
}

/// Initialize a five-arbiter panel for `dispute_id` and assign its first
/// arbiter. Callers must enforce admin authorization.
pub fn initialize_panel(
    env: &Env,
    dispute_id: u64,
    arbiters: Vec<Address>,
    assigned_arbiter: Address,
) -> Result<(), ReputationError> {
    if env
        .storage()
        .persistent()
        .has(&ArbiterKey::Panel(dispute_id))
    {
        return Err(ReputationError::AlreadyInitialized);
    }
    if arbiters.len() != PANEL_SIZE || !arbiters.contains(&assigned_arbiter) {
        return Err(ReputationError::InvalidParam);
    }

    let mut seen = Vec::new(env);
    for arbiter in arbiters.iter() {
        if seen.contains(&arbiter) {
            return Err(ReputationError::InvalidParam);
        }
        seen.push_back(arbiter);
    }

    let assigned_ledger = env.ledger().sequence();
    set(env, &ArbiterKey::Panel(dispute_id), &arbiters);
    set(
        env,
        &ArbiterKey::Tally(dispute_id),
        &ArbiterTally {
            dispute_id,
            votes_for_buyer: 0,
            votes_for_merchant: 0,
            total_voters: 0,
            finalized: false,
        },
    );
    set(
        env,
        &ArbiterKey::Assignment(dispute_id),
        &DisputeAssignment {
            dispute_id,
            assigned_arbiter: assigned_arbiter.clone(),
            assigned_ledger,
        },
    );

    env.events().publish(
        (
            symbol_short!("reput"),
            symbol_short!("arb_init"),
            dispute_id,
        ),
        ArbitrationPanelInitializedEvent {
            dispute_id,
            arbiters,
            assigned_arbiter,
            assigned_ledger,
        },
    );
    Ok(())
}

/// Cast an arbiter vote. Each panel member may vote only once and the arbiter
/// authenticates the call.
pub fn cast_vote(
    env: &Env,
    dispute_id: u64,
    arbiter: &Address,
    outcome: VoteOutcome,
) -> Result<ArbiterTally, ReputationError> {
    arbiter.require_auth();

    let panel: Vec<Address> =
        get(env, &ArbiterKey::Panel(dispute_id)).ok_or(ReputationError::EntityNotFound)?;
    if !panel.contains(arbiter) {
        return Err(ReputationError::Unauthorized);
    }

    let voter_key = ArbiterKey::Voter(dispute_id, arbiter.clone());
    if env.storage().persistent().has(&voter_key) {
        return Err(ReputationError::AppealAlreadyVoted);
    }

    let mut tally: ArbiterTally =
        get(env, &ArbiterKey::Tally(dispute_id)).ok_or(ReputationError::EntityNotFound)?;
    if tally.finalized {
        return Err(ReputationError::AppealAlreadyResolved);
    }

    match outcome {
        VoteOutcome::ForBuyer => {
            tally.votes_for_buyer = tally
                .votes_for_buyer
                .checked_add(1)
                .ok_or(ReputationError::InvalidParam)?;
        }
        VoteOutcome::ForMerchant => {
            tally.votes_for_merchant = tally
                .votes_for_merchant
                .checked_add(1)
                .ok_or(ReputationError::InvalidParam)?;
        }
    }
    tally.total_voters = tally
        .total_voters
        .checked_add(1)
        .ok_or(ReputationError::InvalidParam)?;

    set(env, &voter_key, &true);
    let buyer_wins = tally.votes_for_buyer >= QUORUM || tally.votes_for_merchant >= QUORUM;
    if buyer_wins {
        tally.finalized = true;
    }
    set(env, &ArbiterKey::Tally(dispute_id), &tally);

    env.events().publish(
        (
            symbol_short!("reput"),
            symbol_short!("arb_vote"),
            dispute_id,
        ),
        ArbitrationVoteCastEvent {
            dispute_id,
            arbiter: arbiter.clone(),
            outcome: outcome.clone(),
            tally: tally.clone(),
        },
    );
    if buyer_wins {
        env.events().publish(
            (
                symbol_short!("reput"),
                symbol_short!("arb_final"),
                dispute_id,
            ),
            ArbitrationFinalizedEvent {
                dispute_id,
                buyer_wins: tally.votes_for_buyer >= QUORUM,
                tally: tally.clone(),
            },
        );
    }
    Ok(tally)
}

/// Read the current tally for a dispute, extending its TTL on read.
pub fn get_tally(env: &Env, dispute_id: u64) -> Result<ArbiterTally, ReputationError> {
    get(env, &ArbiterKey::Tally(dispute_id)).ok_or(ReputationError::EntityNotFound)
}

/// Whether `arbiter` already voted on `dispute_id`.
pub fn has_voted(env: &Env, dispute_id: u64, arbiter: &Address) -> bool {
    let key = ArbiterKey::Voter(dispute_id, arbiter.clone());
    let voted: Option<bool> = get(env, &key);
    voted.unwrap_or(false)
}

/// Apply an inactivity strike and reassign an overdue dispute.
///
/// Returns `Ok(false)` when there is no penalty to apply (deadline not
/// expired, arbiter already voted, or already penalized), and `Ok(true)` when
/// the strike and reassignment were recorded.
pub fn check_arbiter_deadline(env: &Env, dispute_id: u64) -> Result<bool, ReputationError> {
    let assignment: DisputeAssignment =
        get(env, &ArbiterKey::Assignment(dispute_id)).ok_or(ReputationError::EntityNotFound)?;
    let tally: ArbiterTally =
        get(env, &ArbiterKey::Tally(dispute_id)).ok_or(ReputationError::EntityNotFound)?;
    if tally.finalized {
        return Err(ReputationError::AppealAlreadyResolved);
    }

    let delinquent = assignment.assigned_arbiter;
    if has_voted(env, dispute_id, &delinquent)
        || env
            .storage()
            .persistent()
            .has(&ArbiterKey::Penalized(dispute_id, delinquent.clone()))
    {
        return Ok(false);
    }

    let current_ledger = env.ledger().sequence();
    let deadline_ledger = assignment
        .assigned_ledger
        .checked_add(ARBITER_DEADLINE_LEDGERS)
        .ok_or(ReputationError::InvalidParam)?;
    if current_ledger <= deadline_ledger {
        return Ok(false);
    }

    let panel: Vec<Address> =
        get(env, &ArbiterKey::Panel(dispute_id)).ok_or(ReputationError::EntityNotFound)?;
    let replacement = panel
        .iter()
        .find(|candidate| {
            candidate != &delinquent
                && !has_voted(env, dispute_id, candidate)
                && !env
                    .storage()
                    .persistent()
                    .has(&ArbiterKey::Penalized(dispute_id, candidate.clone()))
        })
        .ok_or(ReputationError::ArbiterNotRegistered)?;

    let strikes: u32 = get(env, &ArbiterKey::PenaltyCount(delinquent.clone())).unwrap_or(0);
    let strike_count = strikes
        .checked_add(1)
        .ok_or(ReputationError::InvalidParam)?;

    set(
        env,
        &ArbiterKey::Penalized(dispute_id, delinquent.clone()),
        &true,
    );
    set(
        env,
        &ArbiterKey::PenaltyCount(delinquent.clone()),
        &strike_count,
    );
    set(
        env,
        &ArbiterKey::Assignment(dispute_id),
        &DisputeAssignment {
            dispute_id,
            assigned_arbiter: replacement.clone(),
            assigned_ledger: current_ledger,
        },
    );
    env.events().publish(
        (symbol_short!("reput"), symbol_short!("arb_pen"), dispute_id),
        ArbitrationDeadlinePenaltyEvent {
            dispute_id,
            penalized_arbiter: delinquent,
            replacement_arbiter: replacement,
            strike_count,
            observed_ledger: current_ledger,
        },
    );
    Ok(true)
}
