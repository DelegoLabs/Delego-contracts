//! Multi-signature approval management (issue #687).
//!
//! An account registers a signer set and a threshold. Any signer can then
//! propose a 32-byte payload hash, and the proposal is approved once
//! `threshold` distinct signers have authorized it. Approval is recorded
//! on-chain only: this module never executes calls on behalf of the contract.
//!
//! This is independent of `MultiOwnerPermission`, which governs the spending of
//! a delegated allowance.

use soroban_sdk::{contracterror, contracttype, symbol_short, Address, BytesN, Env, Vec};

use crate::{PERMISSION_TTL_BUMP, PERMISSION_TTL_THRESHOLD};

/// Maximum number of signers in a multi-sig configuration or weighted quorum.
pub const MAX_MULTISIG_SIGNERS: u32 = 20;

/// Errors returned by the multi-sig module.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum MultiSigError {
    /// The account already has a multi-sig configuration.
    AlreadyInitialized = 2601,
    /// The account has no multi-sig configuration.
    NotInitialized = 2602,
    /// The threshold or signer set is empty, too large, or unsatisfiable.
    InvalidThreshold = 2603,
    /// The same signer was supplied more than once.
    DuplicateSigner = 2604,
    /// The address is not part of the signer set.
    UnauthorizedSigner = 2605,
    /// The signer has already approved this proposal.
    AlreadyApproved = 2606,
    /// No proposal exists with the requested id.
    ProposalNotFound = 2607,
}

/// Signer set and approval threshold of an account.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiSigConfig {
    /// Distinct addresses allowed to propose and approve.
    pub signers: Vec<Address>,
    /// Number of distinct approvals required.
    pub threshold: u32,
}

/// A payload awaiting, or having reached, multi-sig approval.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiSigProposal {
    /// Account whose signers decide on the proposal.
    pub account: Address,
    /// Identifier, unique per account.
    pub proposal_id: u64,
    /// Hash of the off-chain payload being approved.
    pub payload_hash: BytesN<32>,
    /// Signers that have approved, starting with the proposer.
    pub approvals: Vec<Address>,
    /// Whether the approval threshold has been reached.
    pub approved: bool,
}

/// Weighted multi-signature quorum.
///
/// `signers` and `signer_weights` are parallel vectors: `signer_weights[i]` is
/// the weight contributed by `signers[i]`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiSigQuorum {
    /// Distinct addresses allowed to contribute weight.
    pub signers: Vec<Address>,
    /// Weight that must be accumulated.
    pub threshold_weight: u32,
    /// Weight of each signer.
    pub signer_weights: Vec<u32>,
}

/// Storage keys owned by the multi-sig module.
#[contracttype]
#[derive(Clone)]
enum MultiSigKey {
    /// Configuration of an account.
    Config(Address),
    /// Id assigned to an account's next proposal.
    NextProposalId(Address),
    /// Proposal, keyed by account and proposal id.
    Proposal(Address, u64),
}

fn bump(env: &Env, key: &MultiSigKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, PERMISSION_TTL_THRESHOLD, PERMISSION_TTL_BUMP);
}

fn write_proposal(env: &Env, proposal: &MultiSigProposal) {
    let key = MultiSigKey::Proposal(proposal.account.clone(), proposal.proposal_id);
    env.storage().persistent().set(&key, proposal);
    bump(env, &key);
}

fn has_duplicates(addresses: &Vec<Address>) -> bool {
    addresses
        .iter()
        .enumerate()
        .any(|(i, address)| addresses.first_index_of(&address) != Some(i as u32))
}

fn validate_config(signers: &Vec<Address>, threshold: u32) -> Result<(), MultiSigError> {
    if signers.len() > MAX_MULTISIG_SIGNERS || threshold == 0 || threshold > signers.len() {
        return Err(MultiSigError::InvalidThreshold);
    }
    if has_duplicates(signers) {
        return Err(MultiSigError::DuplicateSigner);
    }
    Ok(())
}

/// Registers the signer set and threshold of `account`. Requires `account`'s
/// authorization and can only be done once.
pub fn initialize(
    env: &Env,
    account: Address,
    signers: Vec<Address>,
    threshold: u32,
) -> Result<(), MultiSigError> {
    account.require_auth();

    let key = MultiSigKey::Config(account.clone());
    if env.storage().persistent().has(&key) {
        return Err(MultiSigError::AlreadyInitialized);
    }
    validate_config(&signers, threshold)?;

    env.storage()
        .persistent()
        .set(&key, &MultiSigConfig { signers, threshold });
    bump(env, &key);
    env.events().publish(
        (symbol_short!("perm"), symbol_short!("ms_init"), account),
        threshold,
    );
    Ok(())
}

/// Returns the multi-sig configuration of `account`.
pub fn get_config(env: &Env, account: &Address) -> Result<MultiSigConfig, MultiSigError> {
    env.storage()
        .persistent()
        .get(&MultiSigKey::Config(account.clone()))
        .ok_or(MultiSigError::NotInitialized)
}

/// Opens a proposal and returns its id. Requires the authorization of
/// `proposer`, who must be a signer and counts as the first approval.
pub fn propose(
    env: &Env,
    account: Address,
    proposer: Address,
    payload_hash: BytesN<32>,
) -> Result<u64, MultiSigError> {
    proposer.require_auth();

    let config = get_config(env, &account)?;
    if !config.signers.contains(&proposer) {
        return Err(MultiSigError::UnauthorizedSigner);
    }

    let id_key = MultiSigKey::NextProposalId(account.clone());
    let proposal_id: u64 = env.storage().persistent().get(&id_key).unwrap_or(1);
    let next_id = proposal_id
        .checked_add(1)
        .expect("multi-sig proposal id overflow");
    env.storage().persistent().set(&id_key, &next_id);
    bump(env, &id_key);
    bump(env, &MultiSigKey::Config(account.clone()));

    let approvals = Vec::from_array(env, [proposer.clone()]);
    let approved = approvals.len() >= config.threshold;
    write_proposal(
        env,
        &MultiSigProposal {
            account: account.clone(),
            proposal_id,
            payload_hash: payload_hash.clone(),
            approvals,
            approved,
        },
    );
    env.events().publish(
        (symbol_short!("perm"), symbol_short!("ms_prop"), account),
        (proposal_id, proposer, payload_hash, approved),
    );
    Ok(proposal_id)
}

/// Approves a proposal and returns whether the threshold has been reached.
/// Requires the authorization of `signer`, who must be a signer that has not
/// approved yet.
pub fn approve(
    env: &Env,
    account: Address,
    signer: Address,
    proposal_id: u64,
) -> Result<bool, MultiSigError> {
    signer.require_auth();

    let config = get_config(env, &account)?;
    if !config.signers.contains(&signer) {
        return Err(MultiSigError::UnauthorizedSigner);
    }
    let mut proposal = get_proposal(env, &account, proposal_id)?;
    if proposal.approvals.contains(&signer) {
        return Err(MultiSigError::AlreadyApproved);
    }

    proposal.approvals.push_back(signer.clone());
    proposal.approved = proposal.approvals.len() >= config.threshold;
    write_proposal(env, &proposal);
    bump(env, &MultiSigKey::Config(account.clone()));
    env.events().publish(
        (symbol_short!("perm"), symbol_short!("ms_appr"), account),
        (proposal_id, signer, proposal.approved),
    );
    Ok(proposal.approved)
}

/// Returns a proposal of `account`.
pub fn get_proposal(
    env: &Env,
    account: &Address,
    proposal_id: u64,
) -> Result<MultiSigProposal, MultiSigError> {
    env.storage()
        .persistent()
        .get(&MultiSigKey::Proposal(account.clone(), proposal_id))
        .ok_or(MultiSigError::ProposalNotFound)
}

/// Returns whether `approvals` carry enough weight to satisfy `quorum`.
///
/// Every approver must be a distinct quorum signer and must authorize the
/// current invocation.
pub fn evaluate_quorum(
    quorum: &MultiSigQuorum,
    approvals: &Vec<Address>,
) -> Result<bool, MultiSigError> {
    if quorum.threshold_weight == 0
        || quorum.signers.len() > MAX_MULTISIG_SIGNERS
        || quorum.signers.len() != quorum.signer_weights.len()
    {
        return Err(MultiSigError::InvalidThreshold);
    }
    if has_duplicates(&quorum.signers) || has_duplicates(approvals) {
        return Err(MultiSigError::DuplicateSigner);
    }

    let mut accumulated: u32 = 0;
    for approver in approvals.iter() {
        let weight = quorum
            .signers
            .first_index_of(&approver)
            .and_then(|index| quorum.signer_weights.get(index))
            .ok_or(MultiSigError::UnauthorizedSigner)?;
        approver.require_auth();
        accumulated = accumulated
            .checked_add(weight)
            .ok_or(MultiSigError::InvalidThreshold)?;
    }
    Ok(accumulated >= quorum.threshold_weight)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PermissionsContract, PermissionsContractClient};
    use soroban_sdk::{
        testutils::{storage::Persistent as _, Address as _},
        vec,
    };

    struct Fixture<'a> {
        env: Env,
        client: PermissionsContractClient<'a>,
        account: Address,
        signers: Vec<Address>,
    }

    impl Fixture<'_> {
        fn signer(&self, index: u32) -> Address {
            self.signers.get(index).unwrap()
        }

        fn propose(&self) -> u64 {
            self.client
                .propose_multisig(&self.account, &self.signer(0), &self.hash(1))
        }

        fn hash(&self, byte: u8) -> BytesN<32> {
            BytesN::from_array(&self.env, &[byte; 32])
        }
    }

    /// Contract with three known signers, not yet initialized.
    fn setup(env: &Env) -> Fixture<'_> {
        env.mock_all_auths();
        let contract_id = env.register(PermissionsContract, ());
        Fixture {
            env: env.clone(),
            client: PermissionsContractClient::new(env, &contract_id),
            account: Address::generate(env),
            signers: vec![
                env,
                Address::generate(env),
                Address::generate(env),
                Address::generate(env),
            ],
        }
    }

    /// Contract with a 2-of-3 multi-sig.
    fn setup_two_of_three(env: &Env) -> Fixture<'_> {
        let f = setup(env);
        f.client.init_multisig(&f.account, &f.signers, &2);
        f
    }

    #[test]
    fn initialize_stores_the_configuration_once() {
        let env = Env::default();
        let f = setup(&env);

        assert_eq!(
            f.client.try_get_multisig_config(&f.account),
            Err(Ok(MultiSigError::NotInitialized))
        );
        f.client.init_multisig(&f.account, &f.signers, &2);
        assert!(env.auths().iter().any(|(signer, _)| signer == &f.account));

        assert_eq!(
            f.client.get_multisig_config(&f.account),
            MultiSigConfig {
                signers: f.signers.clone(),
                threshold: 2,
            }
        );
        let ttl = env.as_contract(&f.client.address, || {
            env.storage()
                .persistent()
                .get_ttl(&MultiSigKey::Config(f.account.clone()))
        });
        assert!(ttl >= PERMISSION_TTL_BUMP);
        assert_eq!(
            f.client.try_init_multisig(&f.account, &f.signers, &1),
            Err(Ok(MultiSigError::AlreadyInitialized))
        );
    }

    #[test]
    fn initialize_validates_the_threshold() {
        let env = Env::default();
        let f = setup(&env);

        for threshold in [0, 4, u32::MAX] {
            assert_eq!(
                f.client
                    .try_init_multisig(&f.account, &f.signers, &threshold),
                Err(Ok(MultiSigError::InvalidThreshold))
            );
        }
        assert_eq!(
            f.client.try_init_multisig(&f.account, &Vec::new(&env), &1),
            Err(Ok(MultiSigError::InvalidThreshold))
        );

        let mut too_many = Vec::new(&env);
        for _ in 0..=MAX_MULTISIG_SIGNERS {
            too_many.push_back(Address::generate(&env));
        }
        assert_eq!(
            f.client.try_init_multisig(&f.account, &too_many, &1),
            Err(Ok(MultiSigError::InvalidThreshold))
        );

        // Both bounds of the valid range are accepted.
        f.client.init_multisig(&f.account, &f.signers, &3);
        f.client
            .init_multisig(&Address::generate(&env), &f.signers, &1);
    }

    #[test]
    fn initialize_rejects_duplicate_signers() {
        let env = Env::default();
        let f = setup(&env);
        let duplicated = vec![&env, f.signer(0), f.signer(1), f.signer(0)];

        assert_eq!(
            f.client.try_init_multisig(&f.account, &duplicated, &2),
            Err(Ok(MultiSigError::DuplicateSigner))
        );
    }

    #[test]
    fn initialize_requires_the_accounts_authorization() {
        let env = Env::default();
        let f = setup(&env);

        env.set_auths(&[]);
        assert!(f
            .client
            .try_init_multisig(&f.account, &f.signers, &2)
            .is_err());
    }

    #[test]
    fn proposal_is_approved_once_the_threshold_is_reached() {
        let env = Env::default();
        let f = setup_two_of_three(&env);

        let id = f.propose();
        assert!(env.auths().iter().any(|(signer, _)| signer == &f.signer(0)));
        let proposal = f.client.get_multisig_proposal(&f.account, &id);
        assert_eq!(proposal.payload_hash, f.hash(1));
        assert_eq!(proposal.approvals, vec![&env, f.signer(0)]);
        assert!(!proposal.approved);

        assert!(f.client.approve_multisig(&f.account, &f.signer(1), &id));
        assert!(env.auths().iter().any(|(signer, _)| signer == &f.signer(1)));
        let proposal = f.client.get_multisig_proposal(&f.account, &id);
        assert_eq!(proposal.approvals, vec![&env, f.signer(0), f.signer(1)]);
        assert!(proposal.approved);
    }

    #[test]
    fn approvals_below_the_threshold_do_not_approve() {
        let env = Env::default();
        let f = setup(&env);
        f.client.init_multisig(&f.account, &f.signers, &3);

        let id = f.propose();
        assert!(!f.client.approve_multisig(&f.account, &f.signer(1), &id));
        assert!(f.client.approve_multisig(&f.account, &f.signer(2), &id));
    }

    #[test]
    fn single_signer_threshold_approves_on_proposal() {
        let env = Env::default();
        let f = setup(&env);
        f.client.init_multisig(&f.account, &f.signers, &1);

        let id = f.propose();
        assert!(f.client.get_multisig_proposal(&f.account, &id).approved);
    }

    #[test]
    fn a_signer_cannot_approve_twice() {
        let env = Env::default();
        let f = setup_two_of_three(&env);
        let id = f.propose();

        // The proposer's approval was already counted.
        assert_eq!(
            f.client.try_approve_multisig(&f.account, &f.signer(0), &id),
            Err(Ok(MultiSigError::AlreadyApproved))
        );
        f.client.approve_multisig(&f.account, &f.signer(1), &id);
        assert_eq!(
            f.client.try_approve_multisig(&f.account, &f.signer(1), &id),
            Err(Ok(MultiSigError::AlreadyApproved))
        );
    }

    #[test]
    fn outsiders_cannot_propose_or_approve() {
        let env = Env::default();
        let f = setup_two_of_three(&env);
        let id = f.propose();
        let outsider = Address::generate(&env);

        assert_eq!(
            f.client
                .try_propose_multisig(&f.account, &outsider, &f.hash(2)),
            Err(Ok(MultiSigError::UnauthorizedSigner))
        );
        assert_eq!(
            f.client.try_approve_multisig(&f.account, &outsider, &id),
            Err(Ok(MultiSigError::UnauthorizedSigner))
        );
    }

    #[test]
    fn signers_must_authorize_proposals_and_approvals() {
        let env = Env::default();
        let f = setup_two_of_three(&env);
        let id = f.propose();

        env.set_auths(&[]);
        assert!(f
            .client
            .try_propose_multisig(&f.account, &f.signer(0), &f.hash(2))
            .is_err());
        assert!(f
            .client
            .try_approve_multisig(&f.account, &f.signer(1), &id)
            .is_err());
    }

    #[test]
    fn proposals_are_scoped_to_their_account() {
        let env = Env::default();
        let f = setup_two_of_three(&env);
        let other = Address::generate(&env);
        let id = f.propose();
        assert_eq!(f.propose(), id + 1);

        assert_eq!(
            f.client.try_approve_multisig(&other, &f.signer(1), &id),
            Err(Ok(MultiSigError::NotInitialized))
        );
        f.client.init_multisig(&other, &f.signers, &2);
        assert_eq!(
            f.client.try_approve_multisig(&other, &f.signer(1), &id),
            Err(Ok(MultiSigError::ProposalNotFound))
        );
        assert_eq!(
            f.client.try_get_multisig_proposal(&f.account, &99),
            Err(Ok(MultiSigError::ProposalNotFound))
        );
    }

    #[test]
    fn weighted_quorum_counts_distinct_authorized_signers() {
        let env = Env::default();
        let f = setup(&env);
        let quorum = MultiSigQuorum {
            signers: f.signers.clone(),
            threshold_weight: 50,
            signer_weights: vec![&env, 30, 20, 10],
        };
        let evaluate = |quorum: &MultiSigQuorum, approvals: Vec<Address>| {
            env.as_contract(&f.client.address, || evaluate_quorum(quorum, &approvals))
        };

        assert_eq!(
            evaluate(&quorum, vec![&env, f.signer(0), f.signer(1)]),
            Ok(true)
        );
        assert_eq!(
            evaluate(&quorum, vec![&env, f.signer(0), f.signer(2)]),
            Ok(false)
        );
        assert_eq!(
            evaluate(&quorum, vec![&env, f.signer(0), f.signer(0)]),
            Err(MultiSigError::DuplicateSigner)
        );
        assert_eq!(
            evaluate(&quorum, vec![&env, Address::generate(&env)]),
            Err(MultiSigError::UnauthorizedSigner)
        );

        let zero_threshold = MultiSigQuorum {
            threshold_weight: 0,
            ..quorum.clone()
        };
        let mismatched = MultiSigQuorum {
            signer_weights: vec![&env, 30],
            ..quorum
        };
        for invalid in [zero_threshold, mismatched] {
            assert_eq!(
                evaluate(&invalid, vec![&env, f.signer(0)]),
                Err(MultiSigError::InvalidThreshold)
            );
        }
    }
}
