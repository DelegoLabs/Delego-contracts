//! Yield accrual checkpointing for pro-rata returns (issue #598).
//!
//! MasterChef-style accounting: a pool keeps one running
//! `cumulative_yield_per_share` accumulator, and each position remembers the
//! [`YieldCheckpoint`] it last settled at. The yield a position earned over
//! any ledger range is `shares * (acc_end - acc_start) / YIELD_PRECISION`, so
//! when an escrow is partially refunded after 20 days the yield is split by
//! exactly how long, and with how much capital, each party was exposed.
//!
//! # Zero token remainder
//!
//! Integer division leaves dust in two places, and both are reconciled
//! deterministically:
//!
//! 1. Accrual: `amount * YIELD_PRECISION / total_shares` keeps its division
//!    remainder in [`YieldPoolState::carry_scaled`] and folds it into the
//!    next accrual, so no accrued unit is ever dropped from the accumulator.
//! 2. Settlement: each position's earnings are rounded down. When the last
//!    shares leave the pool, everything still unallocated
//!    (`total_accrued - total_allocated`) is credited to that final position.
//!
//! Consequently every unit of accrued yield is allocated exactly once:
//! once a pool is empty, `total_allocated == total_accrued`.
//! [`allocate_pro_rata`] gives the same guarantee for one-off splits using the
//! largest-remainder method.
//!
//! This module only depends on `soroban_sdk`, so it can be exercised on its
//! own (see `interfaces/tests/yield_checkpoint.rs`).

use soroban_sdk::{contracterror, contracttype, Address, Env};

/// Fixed-point scale of `cumulative_yield_per_share` (12 decimals, matching
/// Blend's bRate scale).
pub const YIELD_PRECISION: i128 = 1_000_000_000_000;

/// Checkpointing errors, in their own namespace next to `BlendYieldError`.
#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum CheckpointError {
    /// Negative accrual, non-positive share change, or a mismatched split.
    InvalidAmount = 710,
    /// A checkpoint moved backwards in ledger sequence or accumulator value.
    LedgerRegression = 711,
    /// A withdrawal exceeds the position's shares.
    InsufficientShares = 712,
    /// Intermediate arithmetic exceeded `i128`.
    MathOverflow = 713,
}

/// Snapshot of the pool accumulator at a ledger.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldCheckpoint {
    /// Ledger sequence the snapshot was taken at.
    pub ledger_sequence: u32,
    /// Yield per share accrued since the pool opened, scaled by
    /// [`YIELD_PRECISION`].
    pub cumulative_yield_per_share: i128,
}

/// Pool-wide accrual state.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldPoolState {
    /// Latest accumulator checkpoint.
    pub checkpoint: YieldCheckpoint,
    /// Shares currently outstanding.
    pub total_shares: i128,
    /// Accrued yield (scaled by [`YIELD_PRECISION`]) not yet reflected in the
    /// accumulator because it did not divide evenly by `total_shares`.
    pub carry_scaled: i128,
    /// Yield accrued while no shares were outstanding.
    pub unallocated: i128,
    /// All yield ever accrued into the pool.
    pub total_accrued: i128,
    /// All yield ever credited to positions.
    pub total_allocated: i128,
}

/// One participant's (e.g. one escrow's) share of a pool.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldPosition {
    /// Capital-weighted shares held.
    pub shares: i128,
    /// Accumulator value the position last settled at.
    pub checkpoint: YieldCheckpoint,
    /// Yield credited to the position and not yet claimed.
    pub pending_yield: i128,
}

fn add(a: i128, b: i128) -> Result<i128, CheckpointError> {
    a.checked_add(b).ok_or(CheckpointError::MathOverflow)
}

fn sub(a: i128, b: i128) -> Result<i128, CheckpointError> {
    a.checked_sub(b).ok_or(CheckpointError::MathOverflow)
}

fn mul(a: i128, b: i128) -> Result<i128, CheckpointError> {
    a.checked_mul(b).ok_or(CheckpointError::MathOverflow)
}

/// Yield earned by `shares` between two checkpoints, rounded down.
pub fn yield_between(
    shares: i128,
    from: &YieldCheckpoint,
    to: &YieldCheckpoint,
) -> Result<i128, CheckpointError> {
    if shares < 0 {
        return Err(CheckpointError::InvalidAmount);
    }
    if to.ledger_sequence < from.ledger_sequence
        || to.cumulative_yield_per_share < from.cumulative_yield_per_share
    {
        return Err(CheckpointError::LedgerRegression);
    }
    let delta = sub(to.cumulative_yield_per_share, from.cumulative_yield_per_share)?;
    Ok(mul(shares, delta)? / YIELD_PRECISION)
}

impl YieldPosition {
    /// Empty position anchored at `checkpoint`.
    pub fn new(checkpoint: YieldCheckpoint) -> Self {
        YieldPosition {
            shares: 0,
            checkpoint,
            pending_yield: 0,
        }
    }
}

impl YieldPoolState {
    /// Empty pool opened at `ledger_sequence`.
    pub fn new(ledger_sequence: u32) -> Self {
        YieldPoolState {
            checkpoint: YieldCheckpoint {
                ledger_sequence,
                cumulative_yield_per_share: 0,
            },
            total_shares: 0,
            carry_scaled: 0,
            unallocated: 0,
            total_accrued: 0,
            total_allocated: 0,
        }
    }

    /// Yield accrued but not yet credited to any position.
    pub fn remainder(&self) -> Result<i128, CheckpointError> {
        sub(self.total_accrued, self.total_allocated)
    }

    fn advance_to(&mut self, ledger_sequence: u32) -> Result<(), CheckpointError> {
        if ledger_sequence < self.checkpoint.ledger_sequence {
            return Err(CheckpointError::LedgerRegression);
        }
        self.checkpoint.ledger_sequence = ledger_sequence;
        Ok(())
    }

    /// Record `amount` of yield generated up to `ledger_sequence`.
    pub fn accrue(&mut self, ledger_sequence: u32, amount: i128) -> Result<(), CheckpointError> {
        if amount < 0 {
            return Err(CheckpointError::InvalidAmount);
        }
        if ledger_sequence < self.checkpoint.ledger_sequence {
            return Err(CheckpointError::LedgerRegression);
        }
        // Compute everything first so a failed accrual leaves state untouched.
        let total_accrued = add(self.total_accrued, amount)?;
        if self.total_shares == 0 {
            self.unallocated = add(self.unallocated, amount)?;
            self.total_accrued = total_accrued;
            self.checkpoint.ledger_sequence = ledger_sequence;
            return Ok(());
        }
        let distributable = add(amount, self.unallocated)?;
        let scaled = add(mul(distributable, YIELD_PRECISION)?, self.carry_scaled)?;
        let acc = add(self.checkpoint.cumulative_yield_per_share, scaled / self.total_shares)?;
        self.unallocated = 0;
        self.carry_scaled = scaled % self.total_shares;
        self.checkpoint.cumulative_yield_per_share = acc;
        self.checkpoint.ledger_sequence = ledger_sequence;
        self.total_accrued = total_accrued;
        Ok(())
    }

    /// Credit `position` with everything it earned since its checkpoint and
    /// move it to the current checkpoint. Returns the amount credited.
    pub fn settle(&mut self, position: &mut YieldPosition) -> Result<i128, CheckpointError> {
        let earned = yield_between(position.shares, &position.checkpoint, &self.checkpoint)?;
        position.pending_yield = add(position.pending_yield, earned)?;
        self.total_allocated = add(self.total_allocated, earned)?;
        position.checkpoint = self.checkpoint.clone();
        Ok(earned)
    }

    /// Add `shares` to `position` at `ledger_sequence`.
    pub fn deposit(
        &mut self,
        position: &mut YieldPosition,
        ledger_sequence: u32,
        shares: i128,
    ) -> Result<(), CheckpointError> {
        if shares <= 0 {
            return Err(CheckpointError::InvalidAmount);
        }
        self.advance_to(ledger_sequence)?;
        self.settle(position)?;
        position.shares = add(position.shares, shares)?;
        self.total_shares = add(self.total_shares, shares)?;
        Ok(())
    }

    /// Remove `shares` from `position` at `ledger_sequence` (a partial or full
    /// refund). Returns the yield credited by this call. When this empties the
    /// pool, the credited amount includes all reconciled dust.
    pub fn withdraw(
        &mut self,
        position: &mut YieldPosition,
        ledger_sequence: u32,
        shares: i128,
    ) -> Result<i128, CheckpointError> {
        if shares <= 0 {
            return Err(CheckpointError::InvalidAmount);
        }
        if shares > position.shares {
            return Err(CheckpointError::InsufficientShares);
        }
        self.advance_to(ledger_sequence)?;
        let mut credited = self.settle(position)?;
        position.shares = sub(position.shares, shares)?;
        self.total_shares = sub(self.total_shares, shares)?;
        if self.total_shares == 0 {
            let dust = self.remainder()?;
            position.pending_yield = add(position.pending_yield, dust)?;
            self.total_allocated = add(self.total_allocated, dust)?;
            credited = add(credited, dust)?;
            self.carry_scaled = 0;
            self.unallocated = 0;
        }
        Ok(credited)
    }

    /// Settle `position` and take its pending yield.
    pub fn claim(&mut self, position: &mut YieldPosition) -> Result<i128, CheckpointError> {
        self.settle(position)?;
        let amount = position.pending_yield;
        position.pending_yield = 0;
        Ok(amount)
    }
}

/// Capital-time weight of a tranche: `capital * ledgers`.
pub fn capital_time_weight(capital: i128, ledgers: u32) -> Result<i128, CheckpointError> {
    if capital < 0 {
        return Err(CheckpointError::InvalidAmount);
    }
    mul(capital, i128::from(ledgers))
}

/// Split `total` across `weights` into `out` with zero remainder.
///
/// Each share is `floor(total * w / W)`; the leftover units (fewer than
/// `weights.len()`) go one each to the largest fractional remainders, ties to
/// the lower index. The result always sums to exactly `total`. With all
/// weights zero, everything goes to index 0.
pub fn allocate_pro_rata(
    total: i128,
    weights: &[i128],
    out: &mut [i128],
) -> Result<(), CheckpointError> {
    if total < 0 || weights.is_empty() || weights.len() != out.len() {
        return Err(CheckpointError::InvalidAmount);
    }
    let mut weight_sum: i128 = 0;
    for w in weights {
        if *w < 0 {
            return Err(CheckpointError::InvalidAmount);
        }
        weight_sum = add(weight_sum, *w)?;
    }
    for slot in out.iter_mut() {
        *slot = 0;
    }
    if weight_sum == 0 {
        out[0] = total;
        return Ok(());
    }

    let mut allocated: i128 = 0;
    for (i, w) in weights.iter().enumerate() {
        out[i] = mul(total, *w)? / weight_sum;
        allocated = add(allocated, out[i])?;
    }
    let mut leftover = sub(total, allocated)?;
    // `leftover < weights.len()`. Walk indices in (remainder desc, index asc)
    // order, one unit each, by always taking the best candidate ranked after
    // the previous pick.
    let mut previous: Option<(i128, usize)> = None;
    while leftover > 0 {
        let mut best: Option<(i128, usize)> = None;
        for (i, w) in weights.iter().enumerate() {
            let rem = mul(total, *w)? % weight_sum;
            let after_previous = match previous {
                None => true,
                Some((p_rem, p_idx)) => rem < p_rem || (rem == p_rem && i > p_idx),
            };
            let beats_best = match best {
                None => true,
                Some((b_rem, _)) => rem > b_rem,
            };
            if after_previous && beats_best {
                best = Some((rem, i));
            }
        }
        let (rem, index) = best.ok_or(CheckpointError::InvalidAmount)?;
        out[index] = add(out[index], 1)?;
        previous = Some((rem, index));
        leftover -= 1;
    }
    Ok(())
}

/// Split yield accrued on an escrow between the refunded tranche and the
/// capital that stays in escrow, in proportion to capital. Returns
/// `(refunded_tranche_yield, retained_tranche_yield)`, which sum to `accrued`.
pub fn split_partial_refund_yield(
    accrued: i128,
    refunded_capital: i128,
    retained_capital: i128,
) -> Result<(i128, i128), CheckpointError> {
    let mut out = [0i128; 2];
    allocate_pro_rata(accrued, &[refunded_capital, retained_capital], &mut out)?;
    Ok((out[0], out[1]))
}

#[contracttype]
#[derive(Clone)]
enum CheckpointKey {
    Pool(Address),
    Position(Address, u64),
}

fn bump(env: &Env, key: &CheckpointKey) {
    env.storage().persistent().extend_ttl(key, 17_280, 518_400);
}

/// Stored state of `pool`, or a fresh pool opened at the current ledger.
pub fn pool_state(env: &Env, pool: &Address) -> YieldPoolState {
    env.storage()
        .persistent()
        .get(&CheckpointKey::Pool(pool.clone()))
        .unwrap_or_else(|| YieldPoolState::new(env.ledger().sequence()))
}

/// Stored position of `escrow_id` in `pool`, or an empty one.
pub fn position(env: &Env, pool: &Address, escrow_id: u64) -> YieldPosition {
    env.storage()
        .persistent()
        .get(&CheckpointKey::Position(pool.clone(), escrow_id))
        .unwrap_or_else(|| YieldPosition::new(pool_state(env, pool).checkpoint))
}

fn save(env: &Env, pool: &Address, state: &YieldPoolState, escrow: Option<(u64, &YieldPosition)>) {
    let pool_key = CheckpointKey::Pool(pool.clone());
    env.storage().persistent().set(&pool_key, state);
    bump(env, &pool_key);
    if let Some((escrow_id, pos)) = escrow {
        let key = CheckpointKey::Position(pool.clone(), escrow_id);
        env.storage().persistent().set(&key, pos);
        bump(env, &key);
    }
}

/// Record `amount` of yield realised by `pool` at the current ledger.
pub fn record_accrual(
    env: &Env,
    pool: &Address,
    amount: i128,
) -> Result<YieldCheckpoint, CheckpointError> {
    let mut state = pool_state(env, pool);
    state.accrue(env.ledger().sequence(), amount)?;
    save(env, pool, &state, None);
    Ok(state.checkpoint)
}

/// Add `shares` for `escrow_id` in `pool` at the current ledger.
pub fn stake(
    env: &Env,
    pool: &Address,
    escrow_id: u64,
    shares: i128,
) -> Result<(), CheckpointError> {
    let mut state = pool_state(env, pool);
    let mut pos = position(env, pool, escrow_id);
    state.deposit(&mut pos, env.ledger().sequence(), shares)?;
    save(env, pool, &state, Some((escrow_id, &pos)));
    Ok(())
}

/// Remove `shares` for `escrow_id` (partial or full refund) and return the
/// yield credited by this call.
pub fn unstake(
    env: &Env,
    pool: &Address,
    escrow_id: u64,
    shares: i128,
) -> Result<i128, CheckpointError> {
    let mut state = pool_state(env, pool);
    let mut pos = position(env, pool, escrow_id);
    let credited = state.withdraw(&mut pos, env.ledger().sequence(), shares)?;
    save(env, pool, &state, Some((escrow_id, &pos)));
    Ok(credited)
}

/// Settle and take all pending yield of `escrow_id` in `pool`.
pub fn claim(env: &Env, pool: &Address, escrow_id: u64) -> Result<i128, CheckpointError> {
    let mut state = pool_state(env, pool);
    let mut pos = position(env, pool, escrow_id);
    let amount = state.claim(&mut pos)?;
    save(env, pool, &state, Some((escrow_id, &pos)));
    Ok(amount)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger};
    use std::vec::Vec;

    /// Ledgers per day at a 5s close time.
    const DAY: u32 = 17_280;

    /// SplitMix64: deterministic, dependency-free randomness for property tests.
    struct Rng(u64);

    impl Rng {
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next_u64() % bound
        }
    }

    #[test]
    fn single_position_receives_every_unit() {
        let mut pool = YieldPoolState::new(0);
        let mut pos = YieldPosition::new(pool.checkpoint.clone());
        pool.deposit(&mut pos, 0, 3).unwrap();
        // 10 / 3 shares does not divide evenly; carry keeps the dust.
        pool.accrue(DAY, 10).unwrap();
        pool.accrue(2 * DAY, 7).unwrap();
        let credited = pool.withdraw(&mut pos, 2 * DAY, 3).unwrap();
        assert_eq!(credited, 17);
        assert_eq!(pos.pending_yield, 17);
        assert_eq!(pool.remainder().unwrap(), 0);
    }

    #[test]
    fn late_joiner_only_earns_from_its_checkpoint() {
        let mut pool = YieldPoolState::new(0);
        let mut early = YieldPosition::new(pool.checkpoint.clone());
        let mut late = YieldPosition::new(pool.checkpoint.clone());
        pool.deposit(&mut early, 0, 1_000).unwrap();
        pool.accrue(10 * DAY, 1_000).unwrap();
        pool.deposit(&mut late, 10 * DAY, 1_000).unwrap();
        pool.accrue(20 * DAY, 1_000).unwrap();
        pool.withdraw(&mut early, 20 * DAY, 1_000).unwrap();
        pool.withdraw(&mut late, 20 * DAY, 1_000).unwrap();
        assert_eq!(early.pending_yield, 1_500);
        assert_eq!(late.pending_yield, 500);
        assert_eq!(pool.remainder().unwrap(), 0);
    }

    #[test]
    fn partial_refund_after_twenty_days_splits_by_duration_and_capital() {
        // Buyer escrows 1_000; the pool yields 100 per day.
        let mut pool = YieldPoolState::new(0);
        let mut escrow = YieldPosition::new(pool.checkpoint.clone());
        pool.deposit(&mut escrow, 0, 1_000).unwrap();
        for day in 1..=20 {
            pool.accrue(day * DAY, 100).unwrap();
        }
        // Day 20: 400 is refunded. The 2_000 earned so far splits 40/60.
        let earned = pool.withdraw(&mut escrow, 20 * DAY, 400).unwrap();
        assert_eq!(earned, 2_000);
        let (refunded, retained) = split_partial_refund_yield(earned, 400, 600).unwrap();
        assert_eq!((refunded, retained), (800, 1_200));

        // The remaining 600 keeps earning for 10 more days, alone in the pool.
        for day in 21..=30 {
            pool.accrue(day * DAY, 100).unwrap();
        }
        let tail = pool.withdraw(&mut escrow, 30 * DAY, 600).unwrap();
        assert_eq!(tail, 1_000);
        assert_eq!(refunded + retained + tail, pool.total_accrued);
        assert_eq!(pool.remainder().unwrap(), 0);
    }

    #[test]
    fn capital_time_weights_split_exactly() {
        // 400 refunded after 20 days vs 600 held for 30 days.
        let w_refunded = capital_time_weight(400, 20 * DAY).unwrap();
        let w_retained = capital_time_weight(600, 30 * DAY).unwrap();
        let mut out = [0i128; 2];
        allocate_pro_rata(3_001, &[w_refunded, w_retained], &mut out).unwrap();
        // Weights are 8_000 : 18_000 => 923.38 / 2077.61; leftover unit goes
        // to the larger remainder.
        assert_eq!(out, [923, 2_078]);
        assert_eq!(out[0] + out[1], 3_001);
    }

    #[test]
    fn yield_between_is_additive_across_ledger_ranges() {
        let mut pool = YieldPoolState::new(0);
        let mut pos = YieldPosition::new(pool.checkpoint.clone());
        pool.deposit(&mut pos, 0, 7).unwrap();
        let a = pool.checkpoint.clone();
        pool.accrue(5, 1_000).unwrap();
        let b = pool.checkpoint.clone();
        pool.accrue(9, 333).unwrap();
        let c = pool.checkpoint.clone();
        let whole = yield_between(7, &a, &c).unwrap();
        let parts = yield_between(7, &a, &b).unwrap() + yield_between(7, &b, &c).unwrap();
        assert!(whole - parts == 0 || whole - parts == 1);
        assert_eq!(
            yield_between(7, &c, &a),
            Err(CheckpointError::LedgerRegression)
        );
    }

    #[test]
    fn yield_with_no_stakers_goes_to_next_stakers() {
        let mut pool = YieldPoolState::new(0);
        pool.accrue(1, 50).unwrap();
        let mut pos = YieldPosition::new(pool.checkpoint.clone());
        pool.deposit(&mut pos, 2, 10).unwrap();
        pool.accrue(3, 50).unwrap();
        assert_eq!(pool.withdraw(&mut pos, 3, 10).unwrap(), 100);
        assert_eq!(pool.remainder().unwrap(), 0);
    }

    #[test]
    fn invalid_operations_are_rejected() {
        let mut pool = YieldPoolState::new(10);
        let mut pos = YieldPosition::new(pool.checkpoint.clone());
        assert_eq!(pool.accrue(9, 1), Err(CheckpointError::LedgerRegression));
        assert_eq!(pool.accrue(10, -1), Err(CheckpointError::InvalidAmount));
        assert_eq!(pool.deposit(&mut pos, 10, 0), Err(CheckpointError::InvalidAmount));
        pool.deposit(&mut pos, 10, 5).unwrap();
        assert_eq!(
            pool.withdraw(&mut pos, 10, 6),
            Err(CheckpointError::InsufficientShares)
        );
        let mut huge = YieldPoolState::new(0);
        let mut whale = YieldPosition::new(huge.checkpoint.clone());
        huge.deposit(&mut whale, 0, 1).unwrap();
        assert_eq!(huge.accrue(1, i128::MAX), Err(CheckpointError::MathOverflow));
    }

    #[test]
    fn storage_helpers_track_escrows_per_pool() {
        let env = Env::default();
        let pool = Address::generate(&env);
        let contract = env.register_stellar_asset_contract_v2(Address::generate(&env));
        env.as_contract(&contract.address(), || {
            stake(&env, &pool, 1, 600).unwrap();
            stake(&env, &pool, 2, 400).unwrap();
        });
        env.ledger().with_mut(|l| l.sequence_number += 20 * DAY);
        env.as_contract(&contract.address(), || {
            let checkpoint = record_accrual(&env, &pool, 1_000).unwrap();
            assert_eq!(checkpoint.ledger_sequence, env.ledger().sequence());
            assert_eq!(unstake(&env, &pool, 2, 400).unwrap(), 400);
            assert_eq!(claim(&env, &pool, 2).unwrap(), 400);
            assert_eq!(claim(&env, &pool, 2).unwrap(), 0);
            assert_eq!(unstake(&env, &pool, 1, 600).unwrap(), 600);
            assert_eq!(pool_state(&env, &pool).remainder().unwrap(), 0);
        });
    }

    /// Property: over random deposits, partial refunds, and accruals across
    /// arbitrary ledger ranges, every accrued unit is allocated exactly once
    /// and each position stays within rounding distance of its exact
    /// pro-rata entitlement.
    #[test]
    fn prop_zero_remainder_and_pro_rata_accuracy() {
        let mut rng = Rng(0x5EED_0598);
        for _ in 0..2_000 {
            let n = 1 + rng.below(6) as usize;
            let mut pool = YieldPoolState::new(0);
            let mut positions: Vec<YieldPosition> = (0..n)
                .map(|_| YieldPosition::new(pool.checkpoint.clone()))
                .collect();
            let mut ideal = std::vec![0f64; n];
            let mut ledger: u32 = 0;
            let mut ops: u32 = 1;
            let mut accrued_while_empty = false;
            let opening = 1 + rng.below(1_000_000) as i128;
            pool.deposit(&mut positions[0], 0, opening).unwrap();

            for _ in 0..(5 + rng.below(40)) {
                ledger += rng.below(3 * DAY as u64) as u32;
                let i = rng.below(n as u64) as usize;
                match rng.below(3) {
                    0 => {
                        let shares = 1 + rng.below(1_000_000) as i128;
                        pool.deposit(&mut positions[i], ledger, shares).unwrap();
                    }
                    1 if positions[i].shares > 0 => {
                        let shares = 1 + rng.below(positions[i].shares as u64) as i128;
                        pool.withdraw(&mut positions[i], ledger, shares).unwrap();
                    }
                    _ => {
                        let amount = rng.below(10_000_000) as i128;
                        if pool.total_shares > 0 {
                            for (k, p) in positions.iter().enumerate() {
                                ideal[k] += amount as f64 * p.shares as f64
                                    / pool.total_shares as f64;
                            }
                        } else if amount > 0 {
                            accrued_while_empty = true;
                        }
                        pool.accrue(ledger, amount).unwrap();
                    }
                }
                ops += 1;
            }
            for p in positions.iter_mut() {
                let shares = p.shares;
                if shares > 0 {
                    pool.withdraw(p, ledger, shares).unwrap();
                }
            }

            assert_eq!(pool.total_shares, 0);
            // Only yield accrued after the pool last emptied may still be
            // waiting for a staker; nothing else is ever left over.
            assert_eq!(pool.remainder().unwrap(), pool.unallocated);
            let total: i128 = positions.iter().map(|p| p.pending_yield).sum();
            assert_eq!(total, pool.total_allocated);
            // Yield accrued while nobody was staked has no exact pro-rata
            // owner, so accuracy is only checked when every accrual had one.
            if !accrued_while_empty {
                assert_eq!(pool.unallocated, 0);
                assert_eq!(total, pool.total_accrued);
                let tolerance = f64::from(ops + n as u32) + 1.0;
                for (k, p) in positions.iter().enumerate() {
                    assert!(
                        (p.pending_yield as f64 - ideal[k]).abs() <= tolerance,
                        "position {k}: got {}, ideal {}",
                        p.pending_yield,
                        ideal[k]
                    );
                }
            }
        }
    }

    /// Property: `allocate_pro_rata` never leaves a remainder and never moves
    /// any share by a full unit from its exact value.
    #[test]
    fn prop_allocate_pro_rata_is_exact() {
        let mut rng = Rng(0xA110_C8ED);
        for _ in 0..5_000 {
            let n = 1 + rng.below(8) as usize;
            let weights: Vec<i128> = (0..n)
                .map(|_| rng.below(1_000_000_000_000) as i128)
                .collect();
            let total = rng.below(1_000_000_000_000_000) as i128;
            let mut out = std::vec![0i128; n];
            allocate_pro_rata(total, &weights, &mut out).unwrap();
            assert_eq!(out.iter().sum::<i128>(), total);
            let weight_sum: i128 = weights.iter().sum();
            if weight_sum > 0 {
                for (w, got) in weights.iter().zip(out.iter()) {
                    let floor = total * w / weight_sum;
                    assert!(*got == floor || *got == floor + 1);
                }
            }
        }
    }

    /// Property: any partial-refund split of accrued yield sums exactly to
    /// the accrued amount.
    #[test]
    fn prop_partial_refund_split_has_no_remainder() {
        let mut rng = Rng(0x0BAD_5EED);
        for _ in 0..5_000 {
            let accrued = rng.below(1_000_000_000_000) as i128;
            let principal = 1 + rng.below(1_000_000_000) as i128;
            let refunded = rng.below(principal as u64 + 1) as i128;
            let (a, b) =
                split_partial_refund_yield(accrued, refunded, principal - refunded).unwrap();
            assert_eq!(a + b, accrued);
            assert!(a >= 0 && b >= 0);
        }
    }
}
