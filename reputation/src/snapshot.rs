//! Periodic reputation snapshot checkpointing (issue #589).
//!
//! Instead of re-aggregating every historical review on each query, the
//! contract keeps a persistent [`ReputationSnapshot`] per merchant plus a
//! small buffer of reviews recorded since `snapshot_ledger`. Queries combine
//! the two; once the buffer is older than [`SNAPSHOT_INTERVAL_LEDGERS`]
//! (~1 week) it is folded into a fresh snapshot on the next write.

use soroban_sdk::{contracttype, Address, Env, Vec};

/// ~1 week at 5s/ledger.
pub const SNAPSHOT_INTERVAL_LEDGERS: u32 = 120_960;

const SNAPSHOT_TTL_THRESHOLD: u32 = 17_280;
const SNAPSHOT_TTL_BUMP: u32 = 518_400;

/// Memoized aggregate of a merchant's reviews up to `snapshot_ledger`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationSnapshot {
    pub merchant: Address,
    pub snapshot_ledger: u32,
    pub cumulative_score: u64,
    pub review_count: u32,
}

#[contracttype]
#[derive(Clone)]
enum SnapshotKey {
    /// Latest checkpoint for a merchant.
    Snapshot(Address),
    /// Ratings recorded after the latest checkpoint.
    Pending(Address),
}

/// Builds a snapshot by folding `ratings` on top of `base` (or an empty one).
pub fn build_snapshot(
    merchant: &Address,
    base: Option<&ReputationSnapshot>,
    ratings: &Vec<u32>,
    ledger: u32,
) -> ReputationSnapshot {
    let (mut cumulative, mut count) = base
        .map(|s| (s.cumulative_score, s.review_count))
        .unwrap_or((0, 0));
    for r in ratings.iter() {
        cumulative = cumulative.saturating_add(r as u64);
        count = count.saturating_add(1);
    }
    ReputationSnapshot {
        merchant: merchant.clone(),
        snapshot_ledger: ledger,
        cumulative_score: cumulative,
        review_count: count,
    }
}

/// Latest persisted checkpoint, bumping its TTL on read.
pub fn latest_snapshot(env: &Env, merchant: &Address) -> Option<ReputationSnapshot> {
    let key = SnapshotKey::Snapshot(merchant.clone());
    let snap: Option<ReputationSnapshot> = env.storage().persistent().get(&key);
    if snap.is_some() {
        env.storage()
            .persistent()
            .extend_ttl(&key, SNAPSHOT_TTL_THRESHOLD, SNAPSHOT_TTL_BUMP);
    }
    snap
}

fn pending(env: &Env, merchant: &Address) -> Vec<u32> {
    env.storage()
        .persistent()
        .get(&SnapshotKey::Pending(merchant.clone()))
        .unwrap_or_else(|| Vec::new(env))
}

/// Records a new review rating and checkpoints when the weekly interval
/// has elapsed since the last snapshot.
pub fn record_review(env: &Env, merchant: &Address, rating: u32) {
    let now = env.ledger().sequence();
    let snap = latest_snapshot(env, merchant);
    let mut buf = pending(env, merchant);
    buf.push_back(rating);

    let last = snap.as_ref().map(|s| s.snapshot_ledger).unwrap_or(0);
    let pending_key = SnapshotKey::Pending(merchant.clone());

    if snap.is_none() || now.saturating_sub(last) >= SNAPSHOT_INTERVAL_LEDGERS {
        let new_snap = build_snapshot(merchant, snap.as_ref(), &buf, now);
        let key = SnapshotKey::Snapshot(merchant.clone());
        env.storage().persistent().set(&key, &new_snap);
        env.storage()
            .persistent()
            .extend_ttl(&key, SNAPSHOT_TTL_THRESHOLD, SNAPSHOT_TTL_BUMP);
        env.storage().persistent().remove(&pending_key);
    } else {
        env.storage().persistent().set(&pending_key, &buf);
        env.storage().persistent().extend_ttl(
            &pending_key,
            SNAPSHOT_TTL_THRESHOLD,
            SNAPSHOT_TTL_BUMP,
        );
    }
}

/// `(cumulative_score, review_count)` = checkpoint + un-checkpointed reviews.
pub fn current_totals(env: &Env, merchant: &Address) -> (u64, u32) {
    let snap = latest_snapshot(env, merchant);
    let combined = build_snapshot(
        merchant,
        snap.as_ref(),
        &pending(env, merchant),
        env.ledger().sequence(),
    );
    (combined.cumulative_score, combined.review_count)
}

/// Average rating (bps) from the snapshot path; `0` with no reviews.
pub fn current_average(env: &Env, merchant: &Address) -> u32 {
    let (sum, count) = current_totals(env, merchant);
    if count == 0 {
        0
    } else {
        (sum / count as u64) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger};
    use soroban_sdk::{contract, Env};

    #[contract]
    struct SnapshotHarness;

    #[test]
    fn snapshot_matches_full_aggregation_across_checkpoints() {
        let env = Env::default();
        let id = env.register(SnapshotHarness, ());
        let merchant = Address::generate(&env);
        let ratings = [8000u32, 6000, 9000, 4000, 10000, 7000];

        env.as_contract(&id, || {
            let mut expected_sum = 0u64;
            for (i, r) in ratings.iter().enumerate() {
                // Cross the weekly interval every other review.
                if i % 2 == 0 {
                    env.ledger()
                        .with_mut(|l| l.sequence_number += SNAPSHOT_INTERVAL_LEDGERS);
                } else {
                    env.ledger().with_mut(|l| l.sequence_number += 10);
                }
                record_review(&env, &merchant, *r);
                expected_sum += *r as u64;
                let (sum, count) = current_totals(&env, &merchant);
                assert_eq!(sum, expected_sum);
                assert_eq!(count as usize, i + 1);
            }
            let snap = latest_snapshot(&env, &merchant).unwrap();
            assert!(snap.review_count < ratings.len() as u32);
            assert_eq!(
                current_average(&env, &merchant) as u64,
                expected_sum / ratings.len() as u64
            );
        });
    }

    #[test]
    fn empty_merchant_has_zero_totals() {
        let env = Env::default();
        let id = env.register(SnapshotHarness, ());
        let merchant = Address::generate(&env);
        env.as_contract(&id, || {
            assert_eq!(current_totals(&env, &merchant), (0, 0));
            assert_eq!(current_average(&env, &merchant), 0);
        });
    }
}
