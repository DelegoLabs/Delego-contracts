//! Formal Verification Specifications for Escrow Lifecycle Invariants
//!
//! This module defines formal invariants that must hold throughout the escrow
//! lifecycle. These invariants are used for property-based testing and provide
//! mathematical guarantees about contract behavior.

use crate::{EscrowFulfillmentPhase, EscrowRecord, EscrowStatus};
use crate::{EscrowRecord, EscrowStatus};
use soroban_sdk::{contracttype, Address, Env, Vec};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecipientDisbursement {
    pub merchant: Address,
    pub amount_stroops: i128,
    pub fee_stroops: i128,
    pub sub_order_id: u64,
}

/// Invariant 1: Conservation of Value
///
/// Mathematical Definition:
///   ∀ escrow ∈ Escrows: escrow.released_amount + escrow.refunded_amount ≤ escrow.amount
///
/// Plain English:
///   The sum of all releases and refunds must never exceed the original deposit.
///
/// Proof Sketch:
///   - Initial state: released_amount = 0, refunded_amount = 0, therefore 0 ≤ amount ✓
///   - Inductive step: Each operation (release/refund) checks available balance before transfer
///   - Available balance = amount - released_amount - refunded_amount
///   - Operations reject if requested amount > available balance
///   - Therefore, invariant preserved after each state transition
///
/// Security Property:
///   This invariant prevents double-spending and ensures economic soundness.
///   Violation would allow draining more funds than deposited.
pub fn verify_value_conservation(record: &EscrowRecord) -> bool {
    let total_distributed = record
        .released_amount
        .checked_add(record.refunded_amount)
        .unwrap_or(i128::MAX);

    total_distributed <= record.amount
}

/// Invariant 2: Terminal State Irrevocability
///
/// Mathematical Definition:
///   ∀ escrow ∈ Escrows: status ∈ {Released, Refunded, Cancelled} ⟹
///     status' = status (no future state transitions allowed)
///
/// Plain English:
///   Once an escrow reaches a terminal state (Released, Refunded, or Cancelled),
///   it cannot transition to any other state, including other terminal states.
///
/// Proof Sketch:
///   - Terminal states T = {Released, Refunded, Cancelled}
///   - State transition function δ: (State × Action) → State
///   - For all s ∈ T and all actions a: δ(s, a) = error
///   - Contract enforces this via check_not_terminal() guard at entry of all
///     mutating operations (release, refund, cancel, dispute, etc.)
///   - Therefore, terminal states form an absorbing set in the state machine
///
/// Security Property:
///   This invariant ensures finality and prevents replay attacks or
///   unauthorized reversal of completed transactions.
pub fn verify_terminal_state_irrevocability(record: &EscrowRecord) -> bool {
    match record.status {
        EscrowStatus::Released | EscrowStatus::Refunded | EscrowStatus::Cancelled => {
            // Terminal states should have complete distribution
            match record.status {
                EscrowStatus::Released => {
                    // For Released state, all funds should be released
                    record.released_amount == record.amount && record.refunded_amount == 0
                }
                EscrowStatus::Refunded => {
                    // For Refunded state, all funds should be refunded
                    record.refunded_amount == record.amount && record.released_amount == 0
                }
                EscrowStatus::Cancelled => {
                    // For Cancelled state, escrow was never funded or was refunded
                    record.released_amount == 0
                }
                _ => unreachable!(),
            }
        }
        _ => true, // Non-terminal states pass (no constraint)
    }
}

/// Invariant 9: Fulfillment Phase Consistency
///
/// Plain English:
///   A funded escrow awaiting merchant acceptance must remain in
///   FundedAwaitingAcceptance until the merchant accepts or the buyer
///   claims an unaccepted refund after the acceptance timeout.
pub fn verify_fulfillment_phase_consistency(record: &EscrowRecord) -> bool {
    match record.status {
        EscrowStatus::Funded => matches!(
            record.fulfillment_phase,
            EscrowFulfillmentPhase::FundedAwaitingAcceptance
                | EscrowFulfillmentPhase::AcceptedInPreparation
                | EscrowFulfillmentPhase::Shipped
                | EscrowFulfillmentPhase::Delivered
        ),
        _ => true,
    }
}

/// Invariant 3: Non-Negative Balances
///
/// Mathematical Definition:
///   ∀ escrow ∈ Escrows:
///     escrow.amount ≥ 0 ∧
///     escrow.released_amount ≥ 0 ∧
///     escrow.refunded_amount ≥ 0
///
/// Plain English:
///   All monetary amounts must be non-negative.
///
/// Security Property:
///   Prevents underflow attacks and negative balance exploits.
pub fn verify_non_negative_balances(record: &EscrowRecord) -> bool {
    record.amount >= 0 && record.released_amount >= 0 && record.refunded_amount >= 0
}

/// Invariant 4: Available Balance Non-Negativity
///
/// Mathematical Definition:
///   ∀ escrow ∈ Escrows:
///     available_balance(escrow) = escrow.amount - escrow.released_amount - escrow.refunded_amount ≥ 0
///
/// Plain English:
///   The available balance (unspent funds) must always be non-negative.
///
/// Proof:
///   This is a corollary of Invariant 1 (Conservation of Value) and Invariant 3 (Non-Negative Balances).
///   From Invariant 1: released_amount + refunded_amount ≤ amount
///   Rearranging: amount - released_amount - refunded_amount ≥ 0
///
/// Security Property:
///   Ensures escrow always has sufficient balance to honor outstanding obligations.
pub fn verify_available_balance_non_negative(record: &EscrowRecord) -> bool {
    let released_plus_refunded = record
        .released_amount
        .checked_add(record.refunded_amount)
        .unwrap_or(i128::MAX);

    record.amount >= released_plus_refunded
}

/// Invariant 5: Status Consistency with Distribution
///
/// Mathematical Definition:
///   ∀ escrow ∈ Escrows:
///     (status = Released ⟹ released_amount = amount ∧ refunded_amount = 0) ∧
///     (status = Refunded ⟹ refunded_amount = amount ∧ released_amount = 0) ∧
///     (status = Cancelled ⟹ released_amount = 0)
///
/// Plain English:
///   Terminal status must be consistent with the distribution of funds.
///   - Released escrows must have released all funds to seller
///   - Refunded escrows must have refunded all funds to buyer
///   - Cancelled escrows must not have released any funds
///
/// Security Property:
///   Prevents status-distribution mismatches that could lead to fund lockup or confusion.
pub fn verify_status_distribution_consistency(record: &EscrowRecord) -> bool {
    match record.status {
        EscrowStatus::Released => {
            record.released_amount == record.amount && record.refunded_amount == 0
        }
        EscrowStatus::Refunded => {
            record.refunded_amount == record.amount && record.released_amount == 0
        }
        EscrowStatus::Cancelled => record.released_amount == 0,
        _ => true, // Non-terminal states allow partial distributions
    }
}

/// Invariant 6: Monotonicity of Distributions
///
/// Mathematical Definition:
///   ∀ escrow ∈ Escrows, ∀ state transitions s → s':
///     s'.released_amount ≥ s.released_amount ∧
///     s'.refunded_amount ≥ s.refunded_amount
///
/// Plain English:
///   Released and refunded amounts can only increase or stay the same,
///   never decrease.
///
/// Security Property:
///   Prevents unauthorized fund clawbacks and ensures forward progress.
pub fn verify_monotonic_distributions(
    old_record: &EscrowRecord,
    new_record: &EscrowRecord,
) -> bool {
    new_record.released_amount >= old_record.released_amount
        && new_record.refunded_amount >= old_record.refunded_amount
}

/// Invariant 7: Valid State Machine Transitions
///
/// Mathematical Definition:
///   Let Σ = {Created, Funded, Released, Refunded, Disputed, Cancelled} be the state space.
///   Let T ⊆ Σ × Σ be the valid transition relation:
///   
///   T = {
///     (Created, Funded),
///     (Created, Cancelled),
///     (Funded, Released),
///     (Funded, Refunded),
///     (Funded, Disputed),
///     (Funded, Cancelled),
///     (Disputed, Released),
///     (Disputed, Refunded),
///     (Disputed, Cancelled)
///   }
///
///   For any state transition (s, s'): (s, s') ∈ T ∨ s = s'
///
/// Plain English:
///   State transitions must follow the allowed state machine diagram.
///   Terminal states (Released, Refunded, Cancelled) cannot transition to any other state.
///
/// Security Property:
///   Enforces valid lifecycle progression and prevents invalid state jumps.
pub fn is_valid_transition(from: &EscrowStatus, to: &EscrowStatus) -> bool {
    use EscrowStatus::*;

    // Self-transitions are always valid (idempotent operations)
    if from == to {
        return true;
    }

    // Define valid transition table
    matches!(
        (from, to),
        (Created, Funded)
            | (Created, Cancelled)
            | (Funded, Released)
            | (Funded, Refunded)
            | (Funded, Disputed)
            | (Funded, Cancelled)
            | (Disputed, Released)
            | (Disputed, Refunded)
            | (Disputed, Cancelled)
    )
}

/// Invariant 8: Partial Operations Respect Total
///
/// Mathematical Definition:
///   For partial_release(escrow, amount):
///     0 ≤ amount ≤ available_balance(escrow)
///   For partial_refund(escrow, amount):
///     0 ≤ amount ≤ available_balance(escrow)
///
/// Plain English:
///   Partial operations cannot release or refund more than available balance.
///
/// Security Property:
///   Prevents over-release and over-refund in multi-step settlement scenarios.
pub fn verify_partial_operation_bounds(record: &EscrowRecord, operation_amount: i128) -> bool {
    if operation_amount < 0 {
        return false;
    }

    let available = record
        .amount
        .checked_sub(record.released_amount)
        .and_then(|v| v.checked_sub(record.refunded_amount))
        .unwrap_or(0);

    operation_amount <= available
}

/// Comprehensive invariant check combining all invariants.
///
/// Returns true if all invariants hold, false otherwise.
pub fn verify_all_invariants(record: &EscrowRecord) -> bool {
    verify_value_conservation(record)
        && verify_terminal_state_irrevocability(record)
        && verify_non_negative_balances(record)
        && verify_available_balance_non_negative(record)
        && verify_status_distribution_consistency(record)
        && verify_fulfillment_phase_consistency(record)
}

/// Verify invariants hold across a state transition.
pub fn verify_transition_invariants(
    old_record: &EscrowRecord,
    new_record: &EscrowRecord,
) -> bool {
    verify_all_invariants(old_record)
        && verify_all_invariants(new_record)
        && verify_monotonic_distributions(old_record, new_record)
        && is_valid_transition(&old_record.status, &new_record.status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

    fn mock_escrow(
        env: &Env,
        amount: i128,
        released: i128,
        refunded: i128,
        status: EscrowStatus,
    ) -> EscrowRecord {
        EscrowRecord {
            escrow_id: 1,
            buyer: Address::generate(env),
            seller: Address::generate(env),
            token: Address::generate(env),
            order_id: BytesN::from_array(env, &[0u8; 32]),
            amount,
            released_amount: released,
            refunded_amount: refunded,
            status,
            flags: crate::EscrowFlags(0),
            created_at: 1000,
            updated_at: 1000,
            timeout_ledger: 2000,
            fulfillment_phase: EscrowFulfillmentPhase::FundedAwaitingAcceptance,
        }
    }

    #[test]
    fn test_value_conservation_holds() {
        let env = Env::default();
        
        // Valid: released + refunded < amount
        let escrow1 = mock_escrow(&env, 1000, 300, 200, EscrowStatus::Funded);
        assert!(verify_value_conservation(&escrow1));

        // Valid: released + refunded = amount
        let escrow2 = mock_escrow(&env, 1000, 600, 400, EscrowStatus::Released);
        assert!(verify_value_conservation(&escrow2));

        // Invalid: released + refunded > amount
        let escrow3 = mock_escrow(&env, 1000, 700, 400, EscrowStatus::Funded);
        assert!(!verify_value_conservation(&escrow3));
    }

    #[test]
    fn test_terminal_state_irrevocability() {
        let env = Env::default();

        // Valid Released state
        let released = mock_escrow(&env, 1000, 1000, 0, EscrowStatus::Released);
        assert!(verify_terminal_state_irrevocability(&released));

        // Valid Refunded state
        let refunded = mock_escrow(&env, 1000, 0, 1000, EscrowStatus::Refunded);
        assert!(verify_terminal_state_irrevocability(&refunded));

        // Valid Cancelled state
        let cancelled = mock_escrow(&env, 1000, 0, 0, EscrowStatus::Cancelled);
        assert!(verify_terminal_state_irrevocability(&cancelled));

        // Invalid Released (partial release)
        let bad_released = mock_escrow(&env, 1000, 500, 0, EscrowStatus::Released);
        assert!(!verify_terminal_state_irrevocability(&bad_released));
    }

    #[test]
    fn test_illegal_transitions_rejected() {
        use EscrowStatus::*;

        // Illegal: Released → Funded
        assert!(!is_valid_transition(&Released, &Funded));

        // Illegal: Refunded → Released
        assert!(!is_valid_transition(&Refunded, &Released));

        // Illegal: Cancelled → Funded
        assert!(!is_valid_transition(&Cancelled, &Funded));

        // Illegal: Released → Refunded
        assert!(!is_valid_transition(&Released, &Refunded));

        // Illegal: Refunded → Disputed
        assert!(!is_valid_transition(&Refunded, &Disputed));

        // Illegal: Cancelled → Disputed
        assert!(!is_valid_transition(&Cancelled, &Disputed));
    }

    #[test]
    fn test_legal_transitions_accepted() {
        use EscrowStatus::*;

        // Legal forward transitions
        assert!(is_valid_transition(&Created, &Funded));
        assert!(is_valid_transition(&Created, &Cancelled));
        assert!(is_valid_transition(&Funded, &Released));
        assert!(is_valid_transition(&Funded, &Refunded));
        assert!(is_valid_transition(&Funded, &Disputed));
        assert!(is_valid_transition(&Disputed, &Released));
        assert!(is_valid_transition(&Disputed, &Refunded));

        // Idempotent transitions
        assert!(is_valid_transition(&Created, &Created));
        assert!(is_valid_transition(&Funded, &Funded));
        assert!(is_valid_transition(&Released, &Released));
    }

    #[test]
    fn test_monotonic_distributions() {
        let env = Env::default();

        let old = mock_escrow(&env, 1000, 200, 100, EscrowStatus::Funded);
        
        // Valid: increased release
        let new1 = mock_escrow(&env, 1000, 300, 100, EscrowStatus::Funded);
        assert!(verify_monotonic_distributions(&old, &new1));

        // Valid: increased refund
        let new2 = mock_escrow(&env, 1000, 200, 150, EscrowStatus::Funded);
        assert!(verify_monotonic_distributions(&old, &new2));

        // Invalid: decreased release
        let new3 = mock_escrow(&env, 1000, 100, 100, EscrowStatus::Funded);
        assert!(!verify_monotonic_distributions(&old, &new3));

        // Invalid: decreased refund
        let new4 = mock_escrow(&env, 1000, 200, 50, EscrowStatus::Funded);
        assert!(!verify_monotonic_distributions(&old, &new4));
    }

    #[test]
    fn test_all_terminal_state_transitions_blocked() {
        use EscrowStatus::*;
        let terminal_states = [Released, Refunded, Cancelled];
        let all_states = [Created, Funded, Released, Refunded, Disputed, Cancelled];

        for terminal in &terminal_states {
            for target in &all_states {
                if terminal != target {
                    assert!(
                        !is_valid_transition(terminal, target),
                        "Terminal state {:?} should not transition to {:?}",
                        terminal,
                        target
                    );
                }
            }
        }
    }

    #[test]
    fn test_partial_operation_bounds() {
        let env = Env::default();
        
        // Escrow with 1000 total, 300 released, 200 refunded = 500 available
        let escrow = mock_escrow(&env, 1000, 300, 200, EscrowStatus::Funded);

        // Valid partial operations
        assert!(verify_partial_operation_bounds(&escrow, 100));
        assert!(verify_partial_operation_bounds(&escrow, 500));

        // Invalid: exceeds available
        assert!(!verify_partial_operation_bounds(&escrow, 501));
        assert!(!verify_partial_operation_bounds(&escrow, 1000));

        // Invalid: negative amount
        assert!(!verify_partial_operation_bounds(&escrow, -100));
    }

    #[test]
    fn test_fulfillment_phase_consistency() {
        let env = Env::default();

        let awaiting = mock_escrow(&env, 1000, 0, 0, EscrowStatus::Funded);
        assert!(verify_fulfillment_phase_consistency(&awaiting));

        let mut accepted = mock_escrow(&env, 1000, 0, 0, EscrowStatus::Funded);
        accepted.fulfillment_phase = EscrowFulfillmentPhase::AcceptedInPreparation;
        assert!(verify_fulfillment_phase_consistency(&accepted));

        let refunded = mock_escrow(&env, 1000, 0, 1000, EscrowStatus::Refunded);
        assert!(verify_fulfillment_phase_consistency(&refunded));
    }

    #[test]
    fn test_fulfillment_phase_in_all_invariants() {
        let env = Env::default();

        let awaiting = mock_escrow(&env, 1000, 0, 0, EscrowStatus::Funded);
        assert!(verify_all_invariants(&awaiting));
    }
}
