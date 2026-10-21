//! Merchant review appeal mechanism with collateralized bonds (issue #583).
//!
//! A merchant whose reputation is damaged by a demonstrably false review can
//! challenge that review by staking a bond. Registered arbiters then vote on
//! the appeal:
//!
//! * **Approved** — the review is hidden and the bond is returned to the
//!   merchant.
//! * **Denied** — the bond is forfeited to the platform charity.
//! * **Expired** — the voting window closed without a quorum. The appeal is
//!   dismissed and the bond is returned, so a merchant is never punished for
//!   the arbiters' inaction.
//!
//! The bond is escrowed by the contract for the whole voting window, so
//! filing an appeal is not free: the stake is locked while the appeal is on
//! the docket, which is what deters frivolous appeal spam.
//!
//! Appeals target review commitments ([`crate::ReviewCommitment`]), the
//! canonical review record of this contract: a buyer's rating about a
//! merchant, bound to the off-chain review text through a content hash. The
//! review id is the escrow id the commitment was filed under.
//!
//! Hiding a review removes it from the merchant's visible review index
//! ([`crate::ReputationContract::get_visible_review_ids`]). The rating itself
//! stays part of the merchant's score history: the score is derived from the
//! incrementally maintained score accumulator, so a hidden review does not
//! retroactively change an already computed score.

use soroban_sdk::{contracttype, symbol_short, Address, BytesN, Env, Symbol, Vec};

use crate::DataKey;

/// The verdict symbol an arbiter casts to uphold an appeal.
pub fn approve_verdict() -> Symbol {
    symbol_short!("approve")
}

/// The verdict symbol an arbiter casts to reject an appeal.
pub fn deny_verdict() -> Symbol {
    symbol_short!("deny")
}

/// Status of a review appeal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppealStatus {
    /// Filed and awaiting arbiter votes.
    Pending,
    /// Quorum of approvals reached: the review is hidden and the bond is
    /// returned to the merchant.
    Approved,
    /// Quorum of rejections reached: the bond is forfeited to the charity.
    Denied,
    /// The voting window closed without a quorum: the appeal is dismissed and
    /// the bond is returned to the merchant.
    Expired,
    /// The merchant withdrew the appeal: the bond is returned to them.
    Withdrawn,
}

/// An appeal filed by a merchant against one of their reviews.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewAppeal {
    /// Id of the appealed review.
    pub review_id: u64,
    /// Merchant that filed the appeal and that the review is about.
    pub merchant: Address,
    /// Bond, in stroops, escrowed for this appeal.
    pub appeal_bond_stroops: i128,
    /// Hash of the off-chain evidence that the review is false.
    pub justification_hash: BytesN<32>,
    /// Current status of the appeal.
    pub status: AppealStatus,
}

/// Hidden flag of a review that was successfully appealed.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppealedReview {
    /// Id of the appealed review.
    pub review_id: u64,
    /// Whether the review is hidden.
    pub hidden: bool,
}

/// Configuration of the appeal mechanism, set by the contract admin.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppealConfig {
    /// Bond, in stroops, a merchant has to stake to file an appeal.
    pub bond_stroops: i128,
    /// Number of arbiter votes that resolve an appeal.
    pub quorum: u32,
    /// Length of the voting window, in ledgers.
    pub window_ledgers: u64,
    /// Address that receives bonds forfeited by denied appeals.
    pub charity: Address,
}

/// A single arbiter vote on an appeal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppealVote {
    /// Arbiter that cast the vote.
    pub arbiter: Address,
    /// Verdict the arbiter cast.
    pub verdict: Symbol,
    /// Timestamp of the vote.
    pub voted_at: u64,
}

/// Public view of an appeal together with the votes cast on it.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppealOutcome {
    /// The appeal itself.
    pub appeal: ReviewAppeal,
    /// Number of approvals cast so far.
    pub approvals: u32,
    /// Number of rejections cast so far.
    pub rejections: u32,
    /// Total number of votes cast so far.
    pub total_votes: u32,
    /// Whether the appealed review is currently hidden.
    pub hidden: bool,
}

/// Builds the public view of an appeal from the appeal record and its votes.
pub fn outcome(env: &Env, appeal: ReviewAppeal, votes: &Vec<AppealVote>) -> AppealOutcome {
    let approvals = votes
        .iter()
        .filter(|v| v.verdict == approve_verdict())
        .count() as u32;
    let rejections = votes.iter().filter(|v| v.verdict == deny_verdict()).count() as u32;

    AppealOutcome {
        approvals,
        rejections,
        total_votes: votes.len(),
        hidden: is_hidden(env, appeal.review_id),
        appeal,
    }
}

/// Returns `true` when `verdict` is a valid appeal verdict.
pub fn is_valid_verdict(verdict: &Symbol) -> bool {
    verdict == &approve_verdict() || verdict == &deny_verdict()
}

/// Reads the appeal configuration, if the admin configured one.
pub fn load_config(env: &Env) -> Option<AppealConfig> {
    env.storage().persistent().get(&DataKey::AppealConfig)
}

/// Persists the appeal configuration.
pub fn store_config(env: &Env, config: &AppealConfig) {
    env.storage()
        .persistent()
        .set(&DataKey::AppealConfig, config);
    env.storage().persistent().extend_ttl(
        &DataKey::AppealConfig,
        crate::PERSISTENT_BUMP_THRESHOLD,
        crate::PERSISTENT_BUMP_AMOUNT,
    );
}

/// Returns `true` when `arbiter` may vote on appeals.
pub fn is_arbiter(env: &Env, arbiter: &Address) -> bool {
    env.storage()
        .persistent()
        .get::<_, bool>(&DataKey::AppealArbiter(arbiter.clone()))
        .unwrap_or(false)
}

/// Adds `arbiter` to the arbiter registry.
pub fn register_arbiter(env: &Env, arbiter: &Address) {
    env.storage()
        .persistent()
        .set(&DataKey::AppealArbiter(arbiter.clone()), &true);
    env.storage().persistent().extend_ttl(
        &DataKey::AppealArbiter(arbiter.clone()),
        crate::PERSISTENT_BUMP_THRESHOLD,
        crate::PERSISTENT_BUMP_AMOUNT,
    );
}

/// Removes `arbiter` from the arbiter registry.
pub fn deregister_arbiter(env: &Env, arbiter: &Address) {
    env.storage()
        .persistent()
        .remove(&DataKey::AppealArbiter(arbiter.clone()));
}

/// Reads the appeal filed for `review_id`, if any.
pub fn load_appeal(env: &Env, review_id: u64) -> Option<ReviewAppeal> {
    env.storage()
        .persistent()
        .get(&DataKey::ReviewAppeal(review_id))
}

/// Persists `appeal`.
pub fn store_appeal(env: &Env, appeal: &ReviewAppeal) {
    env.storage()
        .persistent()
        .set(&DataKey::ReviewAppeal(appeal.review_id), appeal);
    env.storage().persistent().extend_ttl(
        &DataKey::ReviewAppeal(appeal.review_id),
        crate::PERSISTENT_BUMP_THRESHOLD,
        crate::PERSISTENT_BUMP_AMOUNT,
    );
}

/// Reads the ledger at which the voting window of `review_id` closes.
pub fn load_deadline(env: &Env, review_id: u64) -> Option<u64> {
    env.storage()
        .persistent()
        .get(&DataKey::AppealDeadline(review_id))
}

/// Persists the ledger at which the voting window of `review_id` closes.
pub fn store_deadline(env: &Env, review_id: u64, deadline: u64) {
    let key = DataKey::AppealDeadline(review_id);
    env.storage().persistent().set(&key, &deadline);
    env.storage().persistent().extend_ttl(
        &key,
        crate::PERSISTENT_BUMP_THRESHOLD,
        crate::PERSISTENT_BUMP_AMOUNT,
    );
}

/// Reads the arbiter votes cast on `review_id`.
pub fn load_votes(env: &Env, review_id: u64) -> Vec<AppealVote> {
    env.storage()
        .persistent()
        .get(&DataKey::AppealVotes(review_id))
        .unwrap_or_else(|| Vec::new(env))
}

/// Persists the arbiter votes cast on `review_id`.
pub fn store_votes(env: &Env, review_id: u64, votes: &Vec<AppealVote>) {
    env.storage()
        .persistent()
        .set(&DataKey::AppealVotes(review_id), votes);
    env.storage().persistent().extend_ttl(
        &DataKey::AppealVotes(review_id),
        crate::PERSISTENT_BUMP_THRESHOLD,
        crate::PERSISTENT_BUMP_AMOUNT,
    );
}

/// Returns `true` when the review `review_id` is hidden.
pub fn is_hidden(env: &Env, review_id: u64) -> bool {
    env.storage()
        .persistent()
        .get::<_, AppealedReview>(&DataKey::AppealedReview(review_id))
        .map(|r| r.hidden)
        .unwrap_or(false)
}

/// Hides or reveals the review `review_id`.
pub fn set_hidden(env: &Env, review_id: u64, hidden: bool) {
    let key = DataKey::AppealedReview(review_id);
    env.storage()
        .persistent()
        .set(&key, &AppealedReview { review_id, hidden });
    env.storage().persistent().extend_ttl(
        &key,
        crate::PERSISTENT_BUMP_THRESHOLD,
        crate::PERSISTENT_BUMP_AMOUNT,
    );
}

/// Returns the appeal bond currently escrowed for `merchant`, in stroops.
pub fn escrowed_bond(env: &Env, merchant: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::AppealBond(merchant.clone()))
        .unwrap_or(0)
}

/// Overwrites the appeal bond escrowed for `merchant`, in stroops.
pub fn set_escrowed_bond(env: &Env, merchant: &Address, amount: i128) {
    let key = DataKey::AppealBond(merchant.clone());
    env.storage().persistent().set(&key, &amount);
    env.storage().persistent().extend_ttl(
        &key,
        crate::PERSISTENT_BUMP_THRESHOLD,
        crate::PERSISTENT_BUMP_AMOUNT,
    );
}

/// Returns the bond forfeited to `charity` so far, in stroops.
pub fn charity_balance(env: &Env, charity: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::AppealCharityBalance(charity.clone()))
        .unwrap_or(0)
}

/// Credits `amount` stroops to the charity balance of `charity`.
pub fn credit_charity(env: &Env, charity: &Address, amount: i128) {
    let key = DataKey::AppealCharityBalance(charity.clone());
    let balance = charity_balance(env, charity) + amount;
    env.storage().persistent().set(&key, &balance);
    env.storage().persistent().extend_ttl(
        &key,
        crate::PERSISTENT_BUMP_THRESHOLD,
        crate::PERSISTENT_BUMP_AMOUNT,
    );
}
