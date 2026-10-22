use soroban_sdk::{contracttype, Address, BytesN, Env};

use crate::errors::PermissionError;
use crate::storage::DataKey;

/// A single-use delegated authorization ticket.
///
/// Tickets authorize exactly one action by a `delegate` on behalf of an
/// `issuer`. Once consumed the ticket is marked and can never be replayed.
#[contracttype]
#[derive(Clone, Debug)]
pub struct OneTimeTicket {
    pub ticket_id: BytesN<32>,
    pub issuer: Address,
    pub delegate: Address,
    pub action_hash: BytesN<32>,
    pub consumed: bool,
    /// Ledger timestamp after which the ticket is no longer valid.
    pub expires_at: u64,
}

/// Create a new one-time ticket issued by `issuer` for `delegate`.
///
/// The caller must authorize the creation. The ticket is stored under its
/// `ticket_id` and starts in the unconsumed state.
pub fn create_ticket(
    env: &Env,
    ticket_id: BytesN<32>,
    issuer: Address,
    delegate: Address,
    action_hash: BytesN<32>,
    expires_at: u64,
) -> Result<OneTimeTicket, PermissionError> {
    issuer.require_auth();

    let key = DataKey::Ticket(ticket_id.clone());
    if env.storage().persistent().has(&key) {
        return Err(PermissionError::TicketAlreadyExists);
    }

    let ticket = OneTimeTicket {
        ticket_id: ticket_id.clone(),
        issuer,
        delegate,
        action_hash,
        consumed: false,
        expires_at,
    };

    env.storage().persistent().set(&key, &ticket);
    Ok(ticket)
}

/// Consume a one-time ticket, authorizing the delegate for a single action.
///
/// The ticket is marked consumed atomically within the same transaction as
/// the action execution. Replays are rejected with
/// `PermissionError::TicketAlreadyConsumed` and expired tickets with
/// `PermissionError::TicketExpired`.
pub fn consume_ticket(
    env: &Env,
    ticket_id: BytesN<32>,
    delegate: Address,
    action_hash: BytesN<32>,
) -> Result<OneTimeTicket, PermissionError> {
    delegate.require_auth();

    let key = DataKey::Ticket(ticket_id.clone());
    let mut ticket: OneTimeTicket = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(PermissionError::TicketNotFound)?;

    if ticket.consumed {
        return Err(PermissionError::TicketAlreadyConsumed);
    }

    if env.ledger().timestamp() > ticket.expires_at {
        return Err(PermissionError::TicketExpired);
    }

    if ticket.delegate != delegate {
        return Err(PermissionError::Unauthorized);
    }

    if ticket.action_hash != action_hash {
        return Err(PermissionError::InvalidActionHash);
    }

    // Mark consumed atomically in the same transaction as execution.
    ticket.consumed = true;
    env.storage().persistent().set(&key, &ticket);

    Ok(ticket)
}
