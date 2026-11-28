//! Ephemeral, bounded spend tickets (issue #688).
//!
//! A ticket lets an owner authorize a delegate to spend up to `max_amount`
//! before `expires_at_ledger`, in one or several redemptions, without granting
//! a standing permission. Redemption only accounts for the spend; it does not
//! move tokens.

use soroban_sdk::{contracterror, contracttype, symbol_short, Address, Env};

use crate::{PERMISSION_TTL_BUMP, PERMISSION_TTL_THRESHOLD};

/// Errors returned by the spend ticket module.
///
/// Kept separate from `PermissionError`, which is close to the 50-case limit of
/// the contract spec. Codes stay within the crate's 2000-2999 block.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum TicketError {
    /// No ticket exists with the requested id.
    TicketNotFound = 2801,
    /// The ticket has reached its expiry ledger.
    TicketExpired = 2802,
    /// The ticket was revoked by its owner.
    TicketRevoked = 2803,
    /// The redemption would exceed the ticket's `max_amount`.
    ExceedsMaxAmount = 2804,
    /// The amount is not positive.
    InvalidAmount = 2805,
    /// The requested expiry ledger is not in the future.
    InvalidExpiry = 2806,
    /// The ticket id counter is exhausted.
    TicketIdOverflow = 2807,
}

/// A bounded authorization for a delegate to spend on an owner's behalf.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpendTicket {
    /// Unique ticket identifier.
    pub ticket_id: u64,
    /// Address that issued the ticket and may revoke it.
    pub owner: Address,
    /// Address allowed to redeem the ticket.
    pub delegate: Address,
    /// Maximum total amount redeemable.
    pub max_amount: i128,
    /// Amount redeemed so far.
    pub used_amount: i128,
    /// First ledger at which the ticket can no longer be redeemed.
    pub expires_at_ledger: u32,
    /// Whether the owner revoked the ticket.
    pub revoked: bool,
}

/// Storage keys owned by the ticket module.
#[contracttype]
#[derive(Clone)]
pub enum TicketDataKey {
    /// Ticket record, keyed by ticket id (persistent).
    Ticket(u64),
    /// Id assigned to the next ticket (instance).
    NextTicketId,
}

fn read_ticket(env: &Env, ticket_id: u64) -> Result<SpendTicket, TicketError> {
    env.storage()
        .persistent()
        .get(&TicketDataKey::Ticket(ticket_id))
        .ok_or(TicketError::TicketNotFound)
}

fn write_ticket(env: &Env, ticket: &SpendTicket) {
    let key = TicketDataKey::Ticket(ticket.ticket_id);
    env.storage().persistent().set(&key, ticket);
    env.storage()
        .persistent()
        .extend_ttl(&key, PERMISSION_TTL_THRESHOLD, PERMISSION_TTL_BUMP);
}

/// Issues a ticket from `owner` to `delegate` and returns its id. Requires
/// `owner`'s authorization.
pub fn issue(
    env: &Env,
    owner: Address,
    delegate: Address,
    max_amount: i128,
    expires_at_ledger: u32,
) -> Result<u64, TicketError> {
    owner.require_auth();

    if max_amount <= 0 {
        return Err(TicketError::InvalidAmount);
    }
    if expires_at_ledger <= env.ledger().sequence() {
        return Err(TicketError::InvalidExpiry);
    }

    let instance = env.storage().instance();
    let ticket_id: u64 = instance.get(&TicketDataKey::NextTicketId).unwrap_or(1);
    let next_id = ticket_id
        .checked_add(1)
        .ok_or(TicketError::TicketIdOverflow)?;
    instance.set(&TicketDataKey::NextTicketId, &next_id);
    instance.extend_ttl(PERMISSION_TTL_THRESHOLD, PERMISSION_TTL_BUMP);

    write_ticket(
        env,
        &SpendTicket {
            ticket_id,
            owner: owner.clone(),
            delegate: delegate.clone(),
            max_amount,
            used_amount: 0,
            expires_at_ledger,
            revoked: false,
        },
    );
    env.events().publish(
        (symbol_short!("perm"), symbol_short!("tkt_issue"), ticket_id),
        (owner, delegate, max_amount, expires_at_ledger),
    );
    Ok(ticket_id)
}

/// Redeems `amount` from a ticket and returns the amount still redeemable.
/// Requires the ticket delegate's authorization.
pub fn redeem(env: &Env, ticket_id: u64, amount: i128) -> Result<i128, TicketError> {
    let mut ticket = read_ticket(env, ticket_id)?;
    ticket.delegate.require_auth();

    if amount <= 0 {
        return Err(TicketError::InvalidAmount);
    }
    if ticket.revoked {
        return Err(TicketError::TicketRevoked);
    }
    if env.ledger().sequence() >= ticket.expires_at_ledger {
        return Err(TicketError::TicketExpired);
    }

    let used_amount = ticket
        .used_amount
        .checked_add(amount)
        .ok_or(TicketError::ExceedsMaxAmount)?;
    if used_amount > ticket.max_amount {
        return Err(TicketError::ExceedsMaxAmount);
    }
    let remaining = ticket
        .max_amount
        .checked_sub(used_amount)
        .ok_or(TicketError::ExceedsMaxAmount)?;

    ticket.used_amount = used_amount;
    write_ticket(env, &ticket);
    env.events().publish(
        (symbol_short!("perm"), symbol_short!("tkt_rdm"), ticket_id),
        (ticket.owner, ticket.delegate, amount, remaining),
    );
    Ok(remaining)
}

/// Revokes a ticket so it can no longer be redeemed. Requires the ticket
/// owner's authorization.
pub fn revoke(env: &Env, ticket_id: u64) -> Result<(), TicketError> {
    let mut ticket = read_ticket(env, ticket_id)?;
    ticket.owner.require_auth();

    if ticket.revoked {
        return Err(TicketError::TicketRevoked);
    }

    ticket.revoked = true;
    write_ticket(env, &ticket);
    env.events().publish(
        (symbol_short!("perm"), symbol_short!("tkt_rvk"), ticket_id),
        (ticket.owner, ticket.delegate),
    );
    Ok(())
}

/// Returns a ticket.
pub fn get(env: &Env, ticket_id: u64) -> Result<SpendTicket, TicketError> {
    read_ticket(env, ticket_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PermissionsContract, PermissionsContractClient};
    use soroban_sdk::testutils::{storage::Persistent as _, Address as _, Ledger as _};

    const START: u32 = 100;
    const EXPIRY: u32 = 200;

    struct Fixture<'a> {
        env: Env,
        client: PermissionsContractClient<'a>,
        owner: Address,
        delegate: Address,
    }

    impl Fixture<'_> {
        fn issue(&self, max_amount: i128) -> u64 {
            self.client
                .issue_spend_ticket(&self.owner, &self.delegate, &max_amount, &EXPIRY)
        }

        fn last_signer_is(&self, address: &Address) -> bool {
            self.env.auths().iter().any(|(signer, _)| signer == address)
        }
    }

    fn setup(env: &Env) -> Fixture<'_> {
        env.mock_all_auths();
        env.ledger().set_sequence_number(START);
        let contract_id = env.register(PermissionsContract, ());
        Fixture {
            env: env.clone(),
            client: PermissionsContractClient::new(env, &contract_id),
            owner: Address::generate(env),
            delegate: Address::generate(env),
        }
    }

    #[test]
    fn issue_stores_the_ticket_and_assigns_sequential_ids() {
        let env = Env::default();
        let f = setup(&env);

        let first = f.issue(1_000);
        assert!(f.last_signer_is(&f.owner));
        let second = f.issue(500);

        assert_eq!((first, second), (1, 2));
        assert_eq!(
            f.client.get_spend_ticket(&first),
            SpendTicket {
                ticket_id: first,
                owner: f.owner.clone(),
                delegate: f.delegate.clone(),
                max_amount: 1_000,
                used_amount: 0,
                expires_at_ledger: EXPIRY,
                revoked: false,
            }
        );
        let ttl = env.as_contract(&f.client.address, || {
            env.storage()
                .persistent()
                .get_ttl(&TicketDataKey::Ticket(first))
        });
        assert!(ttl >= PERMISSION_TTL_BUMP);
    }

    #[test]
    fn issue_rejects_invalid_amount_and_expiry() {
        let env = Env::default();
        let f = setup(&env);

        for amount in [0, -1] {
            assert_eq!(
                f.client
                    .try_issue_spend_ticket(&f.owner, &f.delegate, &amount, &EXPIRY),
                Err(Ok(TicketError::InvalidAmount))
            );
        }
        for expiry in [0, START - 1, START] {
            assert_eq!(
                f.client
                    .try_issue_spend_ticket(&f.owner, &f.delegate, &1_000, &expiry),
                Err(Ok(TicketError::InvalidExpiry))
            );
        }
    }

    #[test]
    fn partial_then_full_redemption_exhausts_the_ticket() {
        let env = Env::default();
        let f = setup(&env);
        let id = f.issue(1_000);

        assert_eq!(f.client.redeem_spend_ticket(&id, &400), 600);
        assert!(f.last_signer_is(&f.delegate));
        assert_eq!(f.client.get_spend_ticket(&id).used_amount, 400);

        // A rejected over-redemption leaves the remainder untouched.
        assert_eq!(
            f.client.try_redeem_spend_ticket(&id, &601),
            Err(Ok(TicketError::ExceedsMaxAmount))
        );
        assert_eq!(f.client.redeem_spend_ticket(&id, &600), 0);
        assert_eq!(f.client.get_spend_ticket(&id).used_amount, 1_000);
        assert_eq!(
            f.client.try_redeem_spend_ticket(&id, &1),
            Err(Ok(TicketError::ExceedsMaxAmount))
        );
    }

    #[test]
    fn single_full_redemption_succeeds() {
        let env = Env::default();
        let f = setup(&env);
        let id = f.issue(1_000);

        assert_eq!(f.client.redeem_spend_ticket(&id, &1_000), 0);
    }

    #[test]
    fn redemption_rejects_bad_amounts_and_overflow() {
        let env = Env::default();
        let f = setup(&env);
        let id = f.issue(i128::MAX);

        for amount in [0, -1] {
            assert_eq!(
                f.client.try_redeem_spend_ticket(&id, &amount),
                Err(Ok(TicketError::InvalidAmount))
            );
        }
        f.client.redeem_spend_ticket(&id, &1);
        assert_eq!(
            f.client.try_redeem_spend_ticket(&id, &i128::MAX),
            Err(Ok(TicketError::ExceedsMaxAmount))
        );
    }

    #[test]
    fn redemption_is_rejected_from_the_expiry_ledger_onwards() {
        let env = Env::default();
        let f = setup(&env);
        let id = f.issue(1_000);

        env.ledger().set_sequence_number(EXPIRY - 1);
        assert_eq!(f.client.redeem_spend_ticket(&id, &100), 900);

        for sequence in [EXPIRY, EXPIRY + 1] {
            env.ledger().set_sequence_number(sequence);
            assert_eq!(
                f.client.try_redeem_spend_ticket(&id, &100),
                Err(Ok(TicketError::TicketExpired))
            );
        }
    }

    #[test]
    fn revoked_ticket_cannot_be_redeemed_or_revoked_again() {
        let env = Env::default();
        let f = setup(&env);
        let id = f.issue(1_000);
        f.client.redeem_spend_ticket(&id, &100);

        f.client.revoke_spend_ticket(&id);
        assert!(f.last_signer_is(&f.owner));

        let ticket = f.client.get_spend_ticket(&id);
        assert!(ticket.revoked);
        assert_eq!(ticket.used_amount, 100);
        assert_eq!(
            f.client.try_redeem_spend_ticket(&id, &100),
            Err(Ok(TicketError::TicketRevoked))
        );
        assert_eq!(
            f.client.try_revoke_spend_ticket(&id),
            Err(Ok(TicketError::TicketRevoked))
        );
    }

    #[test]
    fn unknown_tickets_are_reported() {
        let env = Env::default();
        let f = setup(&env);

        assert_eq!(
            f.client.try_get_spend_ticket(&7),
            Err(Ok(TicketError::TicketNotFound))
        );
        assert_eq!(
            f.client.try_redeem_spend_ticket(&7, &1),
            Err(Ok(TicketError::TicketNotFound))
        );
        assert_eq!(
            f.client.try_revoke_spend_ticket(&7),
            Err(Ok(TicketError::TicketNotFound))
        );
    }

    #[test]
    fn every_mutation_requires_authorization() {
        let env = Env::default();
        let f = setup(&env);
        let id = f.issue(1_000);

        env.set_auths(&[]);
        assert!(f
            .client
            .try_issue_spend_ticket(&f.owner, &f.delegate, &1_000, &EXPIRY)
            .is_err());
        assert!(f.client.try_redeem_spend_ticket(&id, &100).is_err());
        assert!(f.client.try_revoke_spend_ticket(&id).is_err());

        let ticket = f.client.get_spend_ticket(&id);
        assert_eq!(ticket.used_amount, 0);
        assert!(!ticket.revoked);
    }
}
