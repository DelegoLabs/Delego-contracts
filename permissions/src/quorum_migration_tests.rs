//! Tests for `migrate_quorum_threshold` (issue #371).
//!
//! Kept in a dedicated module so the migration's guarantees — spend state and
//! nonce sequences surviving a quorum change, and the existing quorum gating
//! the change — are asserted in isolation from the rest of the suite.

use crate::{
    DataKey, EpochConfig, PermissionError, PermissionStatus, PermissionsContract,
    PermissionsContractClient, QuorumThresholdMigratedEvent, UpdateQuorumThresholdProposal,
};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events},
    vec, Address, Env, Symbol, TryIntoVal, Vec,
};

/// Five owners, a delegate that spends, and a merchant it spends at. The grant
/// is created `threshold`-of-5 so tests can exercise both widening the owner
/// set and changing the quorum size independently.
struct Fixture {
    env: Env,
    contract_id: Address,
    owners: Vec<Address>,
    delegate: Address,
    merchant: Address,
}

impl Fixture {
    fn new(threshold: u32) -> Self {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());

        let mut owners = vec![&env];
        for _ in 0..5 {
            owners.push_back(Address::generate(&env));
        }
        let delegate = Address::generate(&env);
        let merchant = Address::generate(&env);

        let f = Fixture {
            env,
            contract_id,
            owners,
            delegate,
            merchant,
        };
        f.client().grant_multi_owner(
            &f.primary(),
            &f.owners,
            &f.delegate,
            &10_000i128,
            &10_000i128,
            &Vec::<Address>::new(&f.env),
            &100_000u32,
            &threshold,
        );
        f
    }

    fn client(&self) -> PermissionsContractClient<'_> {
        PermissionsContractClient::new(&self.env, &self.contract_id)
    }

    fn owner(&self, i: u32) -> Address {
        self.owners.get(i).unwrap()
    }

    fn primary(&self) -> Address {
        self.owner(0)
    }

    /// Owners at the given indices, in order, as a signer list.
    fn signers(&self, idx: &[u32]) -> Vec<Address> {
        let mut v = Vec::new(&self.env);
        for i in idx {
            v.push_back(self.owner(*i));
        }
        v
    }

    /// A proposal to `new_threshold`-of-`idx.len()` owners. The caller fills in
    /// `signers`, which is what the migration actually gates on.
    fn proposal(&self, new_threshold: u32, idx: &[u32]) -> UpdateQuorumThresholdProposal {
        let mut new_owners = Vec::new(&self.env);
        for i in idx {
            new_owners.push_back(self.owner(*i));
        }
        UpdateQuorumThresholdProposal {
            primary_owner: self.primary(),
            delegate: self.delegate.clone(),
            new_threshold,
            new_owners,
            signers: Vec::new(&self.env),
        }
    }

    fn record(&self) -> crate::MultiOwnerPermission {
        self.client()
            .get_multi_permission(&self.primary(), &self.delegate)
    }

    /// Attempts a multi-owner spend; `true` when it settled.
    fn spend(&self, idx: &[u32], amount: i128) -> bool {
        matches!(
            self.client().try_execute_spend_multi(
                &self.primary(),
                &self.delegate,
                &self.signers(idx),
                &amount,
                &self.merchant
            ),
            Ok(Ok(()))
        )
    }
}

#[test]
fn widens_quorum_from_two_of_five_to_three_of_five() {
    let f = Fixture::new(2);

    let mut p = f.proposal(3, &[0, 1, 2, 3, 4]);
    // The quorum *in force* endorses, not the one being requested.
    p.signers = f.signers(&[0, 1]);
    f.client().migrate_quorum_threshold(&p);

    let record = f.record();
    assert_eq!(record.threshold, 3);
    assert_eq!(record.owners.len(), 5);
    // Still stored under the original key — nothing was relocated.
    assert_eq!(record.owners.get(0).unwrap(), f.primary());
}

#[test]
fn preserves_spent_allowance_and_limits_across_migration() {
    let f = Fixture::new(2);
    let before = f.record();

    assert!(f.spend(&[0, 1], 4_000));
    assert_eq!(f.record().spent, 4_000);

    let mut p = f.proposal(3, &[0, 1, 2, 3, 4]);
    p.signers = f.signers(&[0, 1]);
    f.client().migrate_quorum_threshold(&p);

    let after = f.record();
    assert_eq!(after.spent, 4_000, "used allowance must not reset");
    assert_eq!(after.limit_total, before.limit_total);
    assert_eq!(after.limit_per_tx, before.limit_per_tx);
    assert_eq!(after.expires_at_ledger, before.expires_at_ledger);
    assert_eq!(after.created_at, before.created_at);
    assert_eq!(after.status, PermissionStatus::Active);

    // The delegate draws on what was left, not on a refreshed budget.
    assert!(!f.spend(&[0, 1, 2], 7_000));
    assert!(f.spend(&[0, 1, 2], 6_000));
    assert_eq!(f.record().spent, 10_000);
}

/// Nonce and epoch sequences are keyed by `(primary_owner, delegate)`, so they
/// survive only because the migration never moves the record. They are seeded
/// directly here because `invalidate_nonce_range` is gated on a single-owner
/// `Permission` entry that a multi-owner grant does not create.
#[test]
fn preserves_nonce_and_epoch_sequences_across_migration() {
    let f = Fixture::new(2);
    let (owner, delegate) = (f.primary(), f.delegate.clone());
    f.env.as_contract(&f.contract_id, || {
        f.env.storage().persistent().set(
            &DataKey::RelayerNonce(owner.clone(), delegate.clone()),
            &7u64,
        );
        f.env.storage().persistent().set(
            &DataKey::ChannelNonce(owner.clone(), delegate.clone(), 3),
            &11u64,
        );
        f.env.storage().persistent().set(
            &DataKey::ExecutionEpoch(owner.clone(), delegate.clone()),
            &EpochConfig {
                current_epoch: 42,
                epoch_started_ledger: 1_000,
            },
        );
    });

    let mut p = f.proposal(3, &[0, 1, 2, 3, 4]);
    p.signers = f.signers(&[0, 1]);
    f.client().migrate_quorum_threshold(&p);

    f.env.as_contract(&f.contract_id, || {
        let relayer: u64 = f
            .env
            .storage()
            .persistent()
            .get(&DataKey::RelayerNonce(owner.clone(), delegate.clone()))
            .unwrap();
        let channel: u64 = f
            .env
            .storage()
            .persistent()
            .get(&DataKey::ChannelNonce(owner.clone(), delegate.clone(), 3))
            .unwrap();
        let epoch: EpochConfig = f
            .env
            .storage()
            .persistent()
            .get(&DataKey::ExecutionEpoch(owner.clone(), delegate.clone()))
            .unwrap();
        assert_eq!(relayer, 7, "relayer nonce must continue, not restart");
        assert_eq!(channel, 11, "channel nonce must continue, not restart");
        assert_eq!(epoch.current_epoch, 42, "execution epoch must be intact");
        assert_eq!(epoch.epoch_started_ledger, 1_000);
    });
}

/// Counting signers without collapsing duplicates would let `threshold = 2` be
/// satisfied by one owner listed twice, applying the migration under a weaker
/// quorum than the one in force.
#[test]
fn duplicate_signers_cannot_manufacture_quorum() {
    let f = Fixture::new(2);
    let mut p = f.proposal(3, &[0, 1, 2, 3, 4]);
    p.signers = f.signers(&[0, 0, 0, 0]);

    assert_eq!(
        f.client().try_migrate_quorum_threshold(&p),
        Err(Ok(PermissionError::InsufficientSignatures))
    );

    let record = f.record();
    assert_eq!(record.threshold, 2, "threshold must be unchanged");
    assert_eq!(record.owners.len(), 5, "owner set must be unchanged");
}

/// Gating on the *requested* threshold would let a 3-of-5 config be downgraded
/// to 2-of-5 by a single current owner.
#[test]
fn requires_the_existing_threshold_not_the_new_one() {
    let f = Fixture::new(3);
    let mut p = f.proposal(2, &[0, 1]);
    p.signers = f.signers(&[0]);

    assert_eq!(
        f.client().try_migrate_quorum_threshold(&p),
        Err(Ok(PermissionError::InsufficientSignatures))
    );

    p.signers = f.signers(&[0, 2, 3]);
    f.client().migrate_quorum_threshold(&p);
    assert_eq!(f.record().threshold, 2);
}

#[test]
fn non_owner_signers_carry_no_weight() {
    let f = Fixture::new(2);
    let stranger = Address::generate(&f.env);

    let mut p = f.proposal(3, &[0, 1, 2, 3, 4]);
    p.signers = vec![&f.env, stranger.clone(), f.owner(1)];

    assert_eq!(
        f.client().try_migrate_quorum_threshold(&p),
        Err(Ok(PermissionError::InsufficientSignatures))
    );

    p.signers = vec![&f.env, stranger, f.owner(0), f.owner(2)];
    f.client().migrate_quorum_threshold(&p);
    assert_eq!(f.record().threshold, 3);
}

#[test]
fn rejects_threshold_outside_owner_count() {
    let f = Fixture::new(2);

    let mut zero = f.proposal(0, &[0, 1, 2]);
    zero.signers = f.signers(&[0, 1]);
    assert_eq!(
        f.client().try_migrate_quorum_threshold(&zero),
        Err(Ok(PermissionError::InvalidParam))
    );

    let mut too_high = f.proposal(4, &[0, 1, 2]);
    too_high.signers = f.signers(&[0, 1]);
    assert_eq!(
        f.client().try_migrate_quorum_threshold(&too_high),
        Err(Ok(PermissionError::InvalidParam))
    );

    let mut empty = f.proposal(1, &[]);
    empty.signers = f.signers(&[0, 1]);
    assert_eq!(
        f.client().try_migrate_quorum_threshold(&empty),
        Err(Ok(PermissionError::InvalidParam))
    );
}

#[test]
fn rejects_duplicate_new_owners() {
    let f = Fixture::new(2);
    let mut p = f.proposal(2, &[0, 1, 1]);
    p.signers = f.signers(&[0, 1]);

    assert_eq!(
        f.client().try_migrate_quorum_threshold(&p),
        Err(Ok(PermissionError::InvalidParam))
    );
}

/// Relocating the record when `owners[0]` changes would strand the nonce, epoch
/// and rolling-window entries, which are keyed by the original pair.
#[test]
fn rejects_primary_owner_change() {
    let f = Fixture::new(2);
    let mut p = f.proposal(2, &[1, 2, 3]);
    p.signers = f.signers(&[0, 1]);

    assert_eq!(
        f.client().try_migrate_quorum_threshold(&p),
        Err(Ok(PermissionError::InvalidParam))
    );
}

#[test]
fn rejects_self_delegation_introduced_by_migration() {
    let f = Fixture::new(2);
    let p = UpdateQuorumThresholdProposal {
        primary_owner: f.primary(),
        delegate: f.delegate.clone(),
        new_threshold: 2,
        new_owners: vec![&f.env, f.primary(), f.delegate.clone()],
        signers: f.signers(&[0, 1]),
    };

    assert_eq!(
        f.client().try_migrate_quorum_threshold(&p),
        Err(Ok(PermissionError::SelfDelegationNotAllowed))
    );
}

#[test]
fn rejects_unknown_permission() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(PermissionsContract, ());
    let client = PermissionsContractClient::new(&env, &contract_id);

    let p = UpdateQuorumThresholdProposal {
        primary_owner: Address::generate(&env),
        delegate: Address::generate(&env),
        new_threshold: 1,
        new_owners: vec![&env, Address::generate(&env)],
        signers: Vec::new(&env),
    };

    assert_eq!(
        client.try_migrate_quorum_threshold(&p),
        Err(Ok(PermissionError::PermissionNotFound))
    );
}

#[test]
fn emits_quorum_threshold_migrated_event() {
    let f = Fixture::new(2);
    let mut p = f.proposal(3, &[0, 1, 2, 3, 4]);
    p.signers = f.signers(&[0, 1]);

    f.client().migrate_quorum_threshold(&p);

    let events = f.env.events().all();
    let mut found = false;
    for event in events.iter() {
        let (contract, topics, value) = event;
        if contract != f.contract_id || topics.len() != 2 {
            continue;
        }
        let t0: Symbol = topics.get(0).unwrap().try_into_val(&f.env).unwrap();
        let t1: Symbol = topics.get(1).unwrap().try_into_val(&f.env).unwrap();
        if t0 == symbol_short!("perm") && t1 == symbol_short!("mqmig") {
            let evt: QuorumThresholdMigratedEvent = value.try_into_val(&f.env).unwrap();
            assert_eq!(evt.primary_owner, f.primary());
            assert_eq!(evt.delegate, f.delegate);
            assert_eq!(evt.old_threshold, 2);
            assert_eq!(evt.new_threshold, 3);
            assert_eq!(evt.old_owner_count, 5);
            assert_eq!(evt.new_owner_count, 5);
            assert_eq!(evt.endorser_count, 2);
            found = true;
        }
    }
    assert!(found, "QuorumThresholdMigratedEvent not found");
}

/// After widening, spends need the new quorum — the migration has to take
/// effect, not merely rewrite stored fields.
#[test]
fn new_quorum_governs_spends_after_migration() {
    let f = Fixture::new(2);
    let mut p = f.proposal(3, &[0, 1, 2, 3, 4]);
    p.signers = f.signers(&[0, 1]);
    f.client().migrate_quorum_threshold(&p);

    assert!(!f.spend(&[0, 1], 100), "old quorum must no longer suffice");
    assert!(f.spend(&[0, 1, 2], 100), "new quorum must be accepted");
    assert_eq!(f.record().spent, 100);
}
