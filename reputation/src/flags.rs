//! Sybil flag reporting with slashing for fraudulent reporters (issue #580).
//!
//! Users can flag suspicious merchant reviews as fake. To prevent malicious
//! competitors from flagging genuine reviews, reporters must stake a collateral
//! bond that is slashed if their report is deemed bad-faith. If upheld, the
//! fraudulent review is removed, and the reporter's bond is refunded plus a bounty.

use crate::*;
use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol, Vec};

/// Minimum bond amount required to flag a review (10 USDC in stroops = 100_000_000).
pub const MIN_FLAG_BOND: i128 = 100_000_000;

/// Bounty in basis points (10% = 1_000 bps) awarded to the reporter when a flag is upheld.
pub const FLAG_BOUNTY_BPS: i128 = 1_000;

/// Status of a review flag report.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ReviewFlagStatus {
    /// Flag is pending arbiter resolution.
    Pending = 1,
    /// Flag was upheld as valid: review was removed and reporter was refunded + rewarded.
    Upheld = 2,
    /// Flag was rejected as frivolous: bond was slashed (forfeited).
    Frivolous = 3,
}

/// Stored record of a flagged review.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewFlagRecord {
    /// Unique identifier for this flag report.
    pub flag_id: u64,
    /// Address of user filing the flag report.
    pub reporter: Address,
    /// Target review identifier being flagged.
    pub review_id: u64,
    /// Collateral bond staked by the reporter.
    pub bond_amount: i128,
    /// Current resolution status of this flag.
    pub status: ReviewFlagStatus,
    /// Ledger timestamp when the flag was filed.
    pub filed_at: u64,
    /// Ledger timestamp when the flag was resolved, if resolved.
    pub resolved_at: Option<u64>,
    /// Arbiter address who resolved this flag, if resolved.
    pub arbiter: Option<Address>,
    /// Bounty amount rewarded to the reporter if upheld.
    pub bounty_amount: i128,
}

/// Event emitted when a review flag is resolved.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewFlagResolved {
    /// Unique identifier of the resolved flag.
    pub flag_id: u64,
    /// Target review identifier that was flagged.
    pub review_id: u64,
    /// Address of the reporter who staked the bond.
    pub reporter: Address,
    /// Address of the arbiter who resolved the flag.
    pub arbiter: Address,
    /// Whether the flag was upheld (`true`) or rejected as frivolous (`false`).
    pub upheld: bool,
    /// Staked bond amount.
    pub bond_amount: i128,
    /// Bounty amount rewarded to the reporter if upheld (0 if rejected).
    pub bounty_amount: i128,
    /// Slashed bond amount forfeited if rejected (0 if upheld).
    pub slashed_amount: i128,
    /// Ledger timestamp when resolved.
    pub resolved_at: u64,
}

/// Trait defining review flag reporting and resolution interface (issue #580).
pub trait FlagReportingTrait {
    /// Flag a suspicious review by depositing a collateral bond.
    fn flag_review(
        env: Env,
        reporter: Address,
        review_id: u64,
        bond_amount: i128,
    ) -> Result<bool, ReputationError>;

    /// Resolve a review flag. Upheld reports remove fraudulent review and reward reporter.
    /// Frivolous reports result in bond forfeiture.
    fn resolve_flag(
        env: Env,
        arbiter: Address,
        flag_id: u64,
        upheld: bool,
    ) -> Result<bool, ReputationError>;
}

/// Checks whether a review currently exists and has not been removed.
pub fn review_exists(env: &Env, review_id: u64) -> bool {
    if is_review_removed(env.clone(), review_id) {
        return false;
    }
    if env
        .storage()
        .persistent()
        .has(&DataKey::ReviewCommitment(review_id))
    {
        return true;
    }
    if let Some(record) = env
        .storage()
        .persistent()
        .get::<_, TransactionRecord>(&DataKey::TransactionRecord(review_id))
    {
        if record.rating.is_some() {
            return true;
        }
    }
    false
}

/// Checks whether a review has been removed due to an upheld flag.
pub fn is_review_removed(env: Env, review_id: u64) -> bool {
    env.storage()
        .persistent()
        .get(&DataKey::RemovedReview(review_id))
        .unwrap_or(false)
}

/// Deposits a flag on a review with collateral bond.
pub fn flag_review(
    env: Env,
    reporter: Address,
    review_id: u64,
    bond_amount: i128,
) -> Result<bool, ReputationError> {
    reporter.require_auth();

    if bond_amount < MIN_FLAG_BOND {
        return Err(ReputationError::InvalidFraudBond);
    }

    if !review_exists(&env, review_id) {
        return Err(ReputationError::ReviewNotFound);
    }

    // Check if this reporter already has an active pending flag for this review
    let active_key = DataKey::ReporterActiveReviewFlag(reporter.clone(), review_id);
    if env.storage().persistent().has(&active_key) {
        return Err(ReputationError::AlreadyFlagged);
    }

    // Allocate new flag_id
    let counter_key = DataKey::ReviewFlagCounter;
    let flag_id: u64 = env.storage().instance().get(&counter_key).unwrap_or(1);
    let next_flag_id = flag_id.saturating_add(1);
    env.storage().instance().set(&counter_key, &next_flag_id);

    let flag = ReviewFlagRecord {
        flag_id,
        reporter: reporter.clone(),
        review_id,
        bond_amount,
        status: ReviewFlagStatus::Pending,
        filed_at: env.ledger().timestamp(),
        resolved_at: None,
        arbiter: None,
        bounty_amount: 0,
    };

    let flag_key = DataKey::ReviewFlag(flag_id);
    env.storage().persistent().set(&flag_key, &flag);
    env.storage().persistent().extend_ttl(
        &flag_key,
        PERSISTENT_BUMP_THRESHOLD,
        PERSISTENT_BUMP_AMOUNT,
    );

    // Track active flag for reporter
    env.storage().persistent().set(&active_key, &flag_id);
    env.storage().persistent().extend_ttl(
        &active_key,
        PERSISTENT_BUMP_THRESHOLD,
        PERSISTENT_BUMP_AMOUNT,
    );

    // Index flags by review
    let review_flags_key = DataKey::ReviewFlagsByReview(review_id);
    let mut flags: Vec<u64> = env
        .storage()
        .persistent()
        .get(&review_flags_key)
        .unwrap_or_else(|| Vec::new(&env));
    flags.push_back(flag_id);
    env.storage().persistent().set(&review_flags_key, &flags);
    env.storage().persistent().extend_ttl(
        &review_flags_key,
        PERSISTENT_BUMP_THRESHOLD,
        PERSISTENT_BUMP_AMOUNT,
    );

    env.events().publish(
        (symbol_short!("reput"), symbol_short!("flg_file")),
        flag,
    );

    Ok(true)
}

/// Resolves a review flag. If upheld, removes fraudulent review and rewards reporter.
/// If frivolous, slashes the reporter's bond.
pub fn resolve_flag(
    env: Env,
    arbiter: Address,
    flag_id: u64,
    upheld: bool,
) -> Result<bool, ReputationError> {
    arbiter.require_auth();

    // Arbiter must be admin or configured fraud authority
    let is_admin = ReputationContract::require_caller_is_admin(&env, &arbiter).is_ok();
    let is_fraud_auth = ReputationContract::get_fraud_authority(env.clone())
        .map(|auth| auth == arbiter)
        .unwrap_or(false);
    if !is_admin && !is_fraud_auth {
        return Err(ReputationError::Unauthorized);
    }

    let flag_key = DataKey::ReviewFlag(flag_id);
    let mut flag: ReviewFlagRecord = env
        .storage()
        .persistent()
        .get(&flag_key)
        .ok_or(ReputationError::FlagNotFound)?;

    if flag.status != ReviewFlagStatus::Pending {
        return Err(ReputationError::FlagAlreadyResolved);
    }

    // Clear active reporter tracker
    let active_key = DataKey::ReporterActiveReviewFlag(flag.reporter.clone(), flag.review_id);
    env.storage().persistent().remove(&active_key);

    let (bounty_amount, slashed_amount) = if upheld {
        flag.status = ReviewFlagStatus::Upheld;
        let bounty = (flag.bond_amount * FLAG_BOUNTY_BPS) / (BPS_SCALE as i128);
        flag.bounty_amount = bounty;

        // Upheld flags remove fraudulent review
        remove_fraudulent_review(&env, flag.review_id);

        // Reward reporter: refund bond + bounty
        let reward_key = DataKey::ReporterRewardBalance(flag.reporter.clone());
        let prior_reward: i128 = env.storage().persistent().get(&reward_key).unwrap_or(0);
        let total_payout = flag.bond_amount.saturating_add(bounty);
        env.storage()
            .persistent()
            .set(&reward_key, &prior_reward.saturating_add(total_payout));
        env.storage().persistent().extend_ttl(
            &reward_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );

        (bounty, 0i128)
    } else {
        flag.status = ReviewFlagStatus::Frivolous;

        // Frivolous report: slash bond (forfeiture)
        let slashed_key = DataKey::TotalSlashedReviewBonds;
        let total_slashed: i128 = env.storage().instance().get(&slashed_key).unwrap_or(0);
        env.storage()
            .instance()
            .set(&slashed_key, &total_slashed.saturating_add(flag.bond_amount));

        (0i128, flag.bond_amount)
    };

    let resolved_at = env.ledger().timestamp();
    flag.resolved_at = Some(resolved_at);
    flag.arbiter = Some(arbiter.clone());

    env.storage().persistent().set(&flag_key, &flag);

    // Emit ReviewFlagResolved event
    env.events().publish(
        (
            symbol_short!("reput"),
            Symbol::new(&env, "ReviewFlagResolved"),
        ),
        ReviewFlagResolved {
            flag_id,
            review_id: flag.review_id,
            reporter: flag.reporter.clone(),
            arbiter,
            upheld,
            bond_amount: flag.bond_amount,
            bounty_amount,
            slashed_amount,
            resolved_at,
        },
    );

    Ok(true)
}

/// Removes a fraudulent review upon an upheld flag ruling.
fn remove_fraudulent_review(env: &Env, review_id: u64) {
    // 1. Mark as removed tombstone
    let tombstone_key = DataKey::RemovedReview(review_id);
    env.storage().persistent().set(&tombstone_key, &true);
    env.storage().persistent().extend_ttl(
        &tombstone_key,
        PERSISTENT_BUMP_THRESHOLD,
        PERSISTENT_BUMP_AMOUNT,
    );

    // 2. Remove ReviewCommitment and unlink from MerchantReviewCommitments
    let commitment_key = DataKey::ReviewCommitment(review_id);
    if let Some(commitment) = env
        .storage()
        .persistent()
        .get::<_, ReviewCommitment>(&commitment_key)
    {
        let merchant_key = DataKey::MerchantReviewCommitments(commitment.merchant.clone());
        if let Some(review_ids) = env
            .storage()
            .persistent()
            .get::<_, Vec<u64>>(&merchant_key)
        {
            let mut filtered = Vec::new(env);
            for id in review_ids.iter() {
                if id != review_id {
                    filtered.push_back(id);
                }
            }
            env.storage().persistent().set(&merchant_key, &filtered);
            env.storage().persistent().extend_ttl(
                &merchant_key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_AMOUNT,
            );
        }
        env.storage().persistent().remove(&commitment_key);
    }

    // 3. Clear rating from TransactionRecord if present
    let record_key = DataKey::TransactionRecord(review_id);
    if let Some(mut record) = env
        .storage()
        .persistent()
        .get::<_, TransactionRecord>(&record_key)
    {
        record.rating = None;
        env.storage().persistent().set(&record_key, &record);
        env.storage().persistent().extend_ttl(
            &record_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_AMOUNT,
        );
    }
}

/// Retrieves a review flag by flag_id.
pub fn get_review_flag(env: Env, flag_id: u64) -> Option<ReviewFlagRecord> {
    env.storage()
        .persistent()
        .get(&DataKey::ReviewFlag(flag_id))
}

/// Retrieves all flag IDs associated with a review_id.
pub fn get_review_flags_by_review(env: Env, review_id: u64) -> Vec<u64> {
    env.storage()
        .persistent()
        .get(&DataKey::ReviewFlagsByReview(review_id))
        .unwrap_or_else(|| Vec::new(&env))
}

/// Retrieves the total accumulated rewards (bounties + refunded bonds) for a reporter.
pub fn get_reporter_reward(env: Env, reporter: Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::ReporterRewardBalance(reporter))
        .unwrap_or(0)
}

/// Retrieves the total cumulative slashed bonds from frivolous flags.
pub fn get_total_slashed_bonds(env: Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::TotalSlashedReviewBonds)
        .unwrap_or(0)
}
