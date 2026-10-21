#![cfg(test)]
//! Global token-conservation invariant suite for escrow value flows (issue #401).
//!
//! Every participant that can hold escrow-related value (buyer, seller,
//! platform treasury, dispute mediator and the escrow contract itself) is
//! tracked. After every randomized state transition:
//!
//! * the sum of all balance deltas is exactly 0 stroops (no leakage, no
//!   rounding drift, whatever the fee basis points or split ratios);
//! * the escrow contract holds at least the unreleased remainder of every
//!   open escrow it has accepted.
//!
//! Workflows come from a seeded xorshift stream, so a failure is reproducible
//! from the seed printed in the assertion message.

use delego_escrow::{
    DisputeResolutionAward, EscrowConfig, EscrowContract, EscrowContractClient, EscrowStatus,
};
use delego_tests::fixtures::create_mock_token_contract;
use soroban_sdk::{testutils::Address as _, testutils::Ledger as _, token, Address, BytesN, Env};

const TRANSITIONS_PER_RUN: u32 = 2_500;
/// Fee configurations (basis points), one full run each:
/// 4 runs x 2_500 transitions = 10_000 randomized state transitions.
const FEE_BPS_RUNS: [u32; 4] = [0, 1, 250, 1_000];
const MINTED: i128 = 1_000_000_000_000;

struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// Tracks token balances of every participant across the test environment.
struct BalanceTracker<'a> {
    token: token::Client<'a>,
    accounts: [Address; 5],
    initial: [i128; 5],
}

impl<'a> BalanceTracker<'a> {
    fn new(token: token::Client<'a>, accounts: [Address; 5]) -> Self {
        let initial = [
            token.balance(&accounts[0]),
            token.balance(&accounts[1]),
            token.balance(&accounts[2]),
            token.balance(&accounts[3]),
            token.balance(&accounts[4]),
        ];
        Self {
            token,
            accounts,
            initial,
        }
    }

    /// Sum of (current - initial) over all tracked accounts. Must be 0.
    fn total_delta(&self) -> i128 {
        let mut total = 0i128;
        for (account, initial) in self.accounts.iter().zip(self.initial.iter()) {
            total += self.token.balance(account) - initial;
        }
        total
    }
}

fn run_workflow(seed: u64, fee_bps: u32) {
    let env = Env::default();
    env.ledger()
        .with_mut(|l| l.min_persistent_entry_ttl = 518_400);
    env.mock_all_auths_allowing_non_root_auth();

    let admin = Address::generate(&env);
    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    let treasury = Address::generate(&env);
    let mediator = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let (token_id, token) = create_mock_token_contract(&env, &token_admin);
    token::StellarAssetClient::new(&env, &token_id).mint(&buyer, &MINTED);

    let config = EscrowConfig {
        admin: admin.clone(),
        fee_bps,
        treasury: treasury.clone(),
        min_amount: 100,
        max_amount: 10_000,
    };
    let escrow_addr = env.register(EscrowContract, (config,));
    let escrow = EscrowContractClient::new(&env, &escrow_addr);
    escrow.set_security_guardian(&admin, &Address::generate(&env));
    let action = delego_escrow::AdminAction::AddToken(token_id.clone());
    let queued = escrow.queue_admin_action(&admin, &action);
    let proposal = escrow.get_admin_action(&queued).unwrap();
    env.ledger().with_mut(|l| {
        l.sequence_number = proposal.pending.unlock_ledger;
        l.timestamp = proposal.unlock_timestamp;
    });
    escrow.execute_admin_action(&admin, &queued);

    let tracker = BalanceTracker::new(
        token.clone(),
        [
            buyer.clone(),
            seller.clone(),
            treasury.clone(),
            mediator.clone(),
            escrow_addr.clone(),
        ],
    );
    assert_eq!(tracker.total_delta(), 0);

    let mut rng = Xorshift(seed.max(1));
    let mut open: std::vec::Vec<u64> = std::vec::Vec::new();

    for step in 0..TRANSITIONS_PER_RUN {
        let pick = rng.below(8);
        if open.is_empty() || pick == 0 {
            // New randomized deposit.
            let amount = 100 + rng.below(9_901) as i128;
            let mut order = [0u8; 32];
            order[..8].copy_from_slice(&rng.next().to_le_bytes());
            let order_id = BytesN::from_array(&env, &order);
            if let Ok(Ok(id)) = escrow.try_deposit(
                &buyer, &seller, &token_id, &amount, &order_id, &50, &None, &None,
            ) {
                open.push(id);
            }
        } else {
            let idx = rng.below(open.len() as u64) as usize;
            let id = open[idx];
            let record = escrow.get_escrow(&id);
            let remaining = record.amount - record.released_amount - record.refunded_amount;
            // Random split ratio of the remainder, in basis points.
            let ratio_bps = rng.below(10_001) as i128;
            let slice = (remaining * ratio_bps / 10_000).max(1).min(remaining.max(1));

            match pick {
                1 | 2 => {
                    let _ = escrow.try_partial_release(&id, &buyer, &slice);
                }
                3 => {
                    let _ = escrow.try_release(&id, &buyer, &seller);
                }
                4 => {
                    let _ = escrow.try_partial_refund(&id, &seller, &slice);
                }
                5 => {
                    let _ = escrow.try_refund(&id, &seller);
                }
                6 => {
                    let _ = escrow.try_dispute(&id, &buyer);
                }
                _ => {
                    let mediator_fee = remaining * (rng.below(501) as i128) / 10_000;
                    let buyer_amount = (remaining - mediator_fee) * ratio_bps / 10_000;
                    let _ = escrow.try_resolve_dispute_split(
                        &id,
                        &admin,
                        &DisputeResolutionAward {
                            buyer_amount,
                            seller_amount: remaining - mediator_fee - buyer_amount,
                            mediator_fee,
                            mediator_address: mediator.clone(),
                        },
                    );
                }
            }

            let after = escrow.get_escrow(&id);
            if matches!(
                after.status,
                EscrowStatus::Released | EscrowStatus::Refunded | EscrowStatus::Cancelled
            ) {
                open.swap_remove(idx);
            }
        }

        assert_eq!(
            tracker.total_delta(),
            0,
            "seed {seed} fee_bps {fee_bps} step {step}: token leakage detected"
        );

        let mut owed = 0i128;
        for id in &open {
            let r = escrow.get_escrow(id);
            owed += r.amount - r.released_amount - r.refunded_amount;
        }
        assert!(
            token.balance(&escrow_addr) >= owed,
            "seed {seed} fee_bps {fee_bps} step {step}: escrow holds less than open remainders"
        );
    }

    // Global supply is untouched: every minted stroop is still accounted for.
    let supply: i128 = tracker.accounts.iter().map(|a| token.balance(a)).sum();
    assert_eq!(supply, MINTED, "seed {seed} fee_bps {fee_bps}: supply drift");
}

#[test]
fn verify_global_token_conservation_invariant() {
    for (i, fee_bps) in FEE_BPS_RUNS.iter().enumerate() {
        run_workflow(0x9E37_79B9_7F4A_7C15 ^ (i as u64 + 1), *fee_bps);
    }
}
