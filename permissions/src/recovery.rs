//! Guardian-quorum account recovery (issues #575, #686).
//!
//! An account registers an owner, a guardian set, and a quorum. A guardian can
//! then request that ownership moves to a new address. Once enough guardians
//! approve and the cool-down has elapsed, anyone may execute the request. The
//! current owner can cancel it at any time before that.

use soroban_sdk::{contracterror, contracttype, Address, Env, Symbol, Vec};

use crate::{PERMISSION_TTL_BUMP, PERMISSION_TTL_THRESHOLD};

/// Cool-down, in ledgers, between a recovery request being submitted and its
/// execution (48 hours at 5s per ledger). During this window the legitimate
/// owner may cancel.
pub const RECOVERY_COOLDOWN_LEDGERS: u32 = 34_560;

/// Maximum number of guardians allowed in a quorum.
pub const MAX_GUARDIANS: u32 = 32;

/// A pending request to move an account to a new owner.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryRequest {
    /// Owner at the time the request was submitted.
    pub owner: Address,
    /// Owner the account moves to when the request is executed.
    pub new_owner: Address,
    /// Guardians that have approved, starting with the submitter.
    pub approvals: Vec<Address>,
    /// Ledger sequence at which the request was submitted.
    pub initiated_ledger: u32,
}

/// Event emitted when a recovery is successfully executed.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountRecoveryExecuted {
    /// Account that was recovered.
    pub account: Address,
    /// Owner before the recovery.
    pub old_owner: Address,
    /// Owner after the recovery.
    pub new_owner: Address,
    /// Ledger sequence of the execution.
    pub executed_ledger: u32,
}

/// Event emitted when a recovery request is cancelled by the owner.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountRecoveryCancelled {
    /// Account whose recovery was cancelled.
    pub account: Address,
    /// Ledger sequence of the cancellation.
    pub cancelled_ledger: u32,
}

/// Storage keys used by the recovery module.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryKey {
    /// Owner of an account.
    Owner(Address),
    /// Guardian set configured for an account.
    GuardianList(Address),
    /// Currently pending recovery request for an account.
    RecoveryRequest(Address),
    /// Number of guardian approvals required for an account.
    Quorum(Address),
}

/// Errors returned by the recovery module. Codes stay within the crate's
/// 2000-2999 block.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RecoveryError {
    /// The account has no recovery configuration.
    NotInitialized = 2701,
    /// The account already has a recovery configuration.
    AlreadyInitialized = 2702,
    /// The caller is not a guardian, or not the current owner.
    Unauthorized = 2703,
    /// The guardian set or quorum is empty, too large, duplicated, or
    /// unsatisfiable.
    InvalidQuorum = 2704,
    /// A recovery request is already pending for the account.
    RequestAlreadyExists = 2705,
    /// No recovery request is pending for the account.
    NoRequest = 2706,
    /// The guardian has already approved the request.
    AlreadyApproved = 2707,
    /// Not enough guardians have approved the request.
    QuorumNotReached = 2708,
    /// The cool-down has not elapsed yet.
    CooldownNotElapsed = 2709,
    /// The proposed owner is already the owner.
    InvalidNewOwner = 2710,
    /// The end of the cool-down cannot be represented as a ledger sequence.
    LedgerOverflow = 2711,
}

fn bump(env: &Env, key: &RecoveryKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, PERMISSION_TTL_THRESHOLD, PERMISSION_TTL_BUMP);
}

fn write_request(env: &Env, account: &Address, request: &RecoveryRequest) {
    let key = RecoveryKey::RecoveryRequest(account.clone());
    env.storage().persistent().set(&key, request);
    bump(env, &key);
}

/// Keeps an account's configuration alive while a recovery is in progress.
fn bump_config(env: &Env, account: &Address) {
    bump(env, &RecoveryKey::Owner(account.clone()));
    bump(env, &RecoveryKey::GuardianList(account.clone()));
    bump(env, &RecoveryKey::Quorum(account.clone()));
}

fn require_guardian(env: &Env, account: &Address, guardian: &Address) -> Result<(), RecoveryError> {
    if get_guardians(env, account)?.contains(guardian) {
        Ok(())
    } else {
        Err(RecoveryError::Unauthorized)
    }
}

/// Configures the owner, guardians and quorum of `account`. Requires
/// `account`'s authorization and can only be done once.
pub fn initialize_account(
    env: &Env,
    account: Address,
    owner: Address,
    guardians: Vec<Address>,
    required_approvals: u32,
) -> Result<(), RecoveryError> {
    account.require_auth();

    let owner_key = RecoveryKey::Owner(account.clone());
    let storage = env.storage().persistent();
    if storage.has(&owner_key) {
        return Err(RecoveryError::AlreadyInitialized);
    }
    // A non-zero quorum no larger than the set also rules out an empty set.
    if guardians.len() > MAX_GUARDIANS
        || required_approvals == 0
        || required_approvals > guardians.len()
    {
        return Err(RecoveryError::InvalidQuorum);
    }
    // Duplicates could make the quorum unreachable.
    for (i, guardian) in guardians.iter().enumerate() {
        if guardians.first_index_of(&guardian) != Some(i as u32) {
            return Err(RecoveryError::InvalidQuorum);
        }
    }

    storage.set(&owner_key, &owner);
    storage.set(&RecoveryKey::GuardianList(account.clone()), &guardians);
    storage.set(&RecoveryKey::Quorum(account.clone()), &required_approvals);
    bump_config(env, &account);
    Ok(())
}

/// Returns the current owner of an account.
pub fn get_owner(env: &Env, account: &Address) -> Result<Address, RecoveryError> {
    env.storage()
        .persistent()
        .get(&RecoveryKey::Owner(account.clone()))
        .ok_or(RecoveryError::NotInitialized)
}

/// Returns the guardian set of an account.
pub fn get_guardians(env: &Env, account: &Address) -> Result<Vec<Address>, RecoveryError> {
    env.storage()
        .persistent()
        .get(&RecoveryKey::GuardianList(account.clone()))
        .ok_or(RecoveryError::NotInitialized)
}

/// Returns the number of guardian approvals an account requires.
pub fn get_quorum(env: &Env, account: &Address) -> Result<u32, RecoveryError> {
    env.storage()
        .persistent()
        .get(&RecoveryKey::Quorum(account.clone()))
        .ok_or(RecoveryError::NotInitialized)
}

/// Returns the pending recovery request, if any.
pub fn get_request(env: &Env, account: &Address) -> Option<RecoveryRequest> {
    env.storage()
        .persistent()
        .get(&RecoveryKey::RecoveryRequest(account.clone()))
}

/// Submits a recovery request. The caller must be a configured guardian and is
/// counted as the first approval.
pub fn submit_recovery(
    env: &Env,
    guardian: Address,
    account: Address,
    new_owner: Address,
) -> Result<RecoveryRequest, RecoveryError> {
    guardian.require_auth();

    let owner = get_owner(env, &account)?;
    require_guardian(env, &account, &guardian)?;
    if new_owner == owner {
        return Err(RecoveryError::InvalidNewOwner);
    }
    if get_request(env, &account).is_some() {
        return Err(RecoveryError::RequestAlreadyExists);
    }

    let request = RecoveryRequest {
        owner,
        new_owner,
        approvals: Vec::from_array(env, [guardian]),
        initiated_ledger: env.ledger().sequence(),
    };
    write_request(env, &account, &request);
    bump_config(env, &account);
    Ok(request)
}

/// Approves a pending recovery request. The caller must be a configured
/// guardian that has not approved yet.
pub fn approve_recovery(
    env: &Env,
    guardian: Address,
    account: Address,
) -> Result<RecoveryRequest, RecoveryError> {
    guardian.require_auth();

    require_guardian(env, &account, &guardian)?;
    let mut request = get_request(env, &account).ok_or(RecoveryError::NoRequest)?;
    if request.approvals.contains(&guardian) {
        return Err(RecoveryError::AlreadyApproved);
    }

    request.approvals.push_back(guardian);
    write_request(env, &account, &request);
    bump_config(env, &account);
    Ok(request)
}

/// Executes a recovery request once the quorum is reached and the cool-down has
/// elapsed, and returns the new owner.
pub fn execute_recovery(env: &Env, account: Address) -> Result<Address, RecoveryError> {
    let request = get_request(env, &account).ok_or(RecoveryError::NoRequest)?;
    if request.approvals.len() < get_quorum(env, &account)? {
        return Err(RecoveryError::QuorumNotReached);
    }
    let now = env.ledger().sequence();
    if now < execution_ledger(env, &account)? {
        return Err(RecoveryError::CooldownNotElapsed);
    }

    let old_owner = get_owner(env, &account)?;
    let storage = env.storage().persistent();
    storage.set(&RecoveryKey::Owner(account.clone()), &request.new_owner);
    storage.remove(&RecoveryKey::RecoveryRequest(account.clone()));
    bump_config(env, &account);

    env.events().publish(
        (Symbol::new(env, "AccountRecoveryExecuted"),),
        AccountRecoveryExecuted {
            account,
            old_owner,
            new_owner: request.new_owner.clone(),
            executed_ledger: now,
        },
    );
    Ok(request.new_owner)
}

/// Cancels a pending recovery request. Only the current owner may cancel: this
/// is the escape hatch that protects against a malicious guardian quorum.
pub fn cancel_recovery(env: &Env, owner: Address, account: Address) -> Result<(), RecoveryError> {
    owner.require_auth();

    if get_owner(env, &account)? != owner {
        return Err(RecoveryError::Unauthorized);
    }
    if get_request(env, &account).is_none() {
        return Err(RecoveryError::NoRequest);
    }

    env.storage()
        .persistent()
        .remove(&RecoveryKey::RecoveryRequest(account.clone()));
    bump_config(env, &account);

    env.events().publish(
        (Symbol::new(env, "AccountRecoveryCancelled"),),
        AccountRecoveryCancelled {
            account,
            cancelled_ledger: env.ledger().sequence(),
        },
    );
    Ok(())
}

/// Returns the ledger sequence from which a pending request may be executed.
pub fn execution_ledger(env: &Env, account: &Address) -> Result<u32, RecoveryError> {
    get_request(env, account)
        .ok_or(RecoveryError::NoRequest)?
        .initiated_ledger
        .checked_add(RECOVERY_COOLDOWN_LEDGERS)
        .ok_or(RecoveryError::LedgerOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PermissionsContract;
    use soroban_sdk::{
        testutils::{storage::Persistent as _, Address as _, Ledger as _},
        vec,
    };

    const START: u32 = 1_000;

    struct Fixture {
        env: Env,
        contract_id: Address,
        account: Address,
        owner: Address,
        new_owner: Address,
        guardians: Vec<Address>,
    }

    /// Every call runs in its own contract frame, like a separate invocation.
    impl Fixture {
        fn run<T>(&self, f: impl FnOnce(&Env) -> T) -> T {
            self.env.as_contract(&self.contract_id, || f(&self.env))
        }

        /// Moves to `sequence`, keeping the contract instance live.
        fn advance_to(&self, sequence: u32) {
            self.run(|env| {
                env.storage()
                    .instance()
                    .extend_ttl(PERMISSION_TTL_THRESHOLD, PERMISSION_TTL_BUMP)
            });
            self.env.ledger().set_sequence_number(sequence);
        }

        fn guardian(&self, index: u32) -> Address {
            self.guardians.get(index).unwrap()
        }

        fn initialize(&self, guardians: &Vec<Address>, quorum: u32) -> Result<(), RecoveryError> {
            self.run(|env| {
                initialize_account(
                    env,
                    self.account.clone(),
                    self.owner.clone(),
                    guardians.clone(),
                    quorum,
                )
            })
        }

        fn submit(&self, guardian: &Address) -> Result<RecoveryRequest, RecoveryError> {
            self.run(|env| {
                submit_recovery(
                    env,
                    guardian.clone(),
                    self.account.clone(),
                    self.new_owner.clone(),
                )
            })
        }

        fn approve(&self, guardian: &Address) -> Result<RecoveryRequest, RecoveryError> {
            self.run(|env| approve_recovery(env, guardian.clone(), self.account.clone()))
        }

        fn execute(&self) -> Result<Address, RecoveryError> {
            self.run(|env| execute_recovery(env, self.account.clone()))
        }

        fn cancel(&self, owner: &Address) -> Result<(), RecoveryError> {
            self.run(|env| cancel_recovery(env, owner.clone(), self.account.clone()))
        }

        fn current_owner(&self) -> Result<Address, RecoveryError> {
            self.run(|env| get_owner(env, &self.account))
        }

        fn request(&self) -> Option<RecoveryRequest> {
            self.run(|env| get_request(env, &self.account))
        }

        fn ttl(&self, key: RecoveryKey) -> u32 {
            self.run(|env| env.storage().persistent().get_ttl(&key))
        }
    }

    fn setup_uninitialized() -> Fixture {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_sequence_number(START);
        let contract_id = env.register(PermissionsContract, ());
        let guardians = vec![
            &env,
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ];
        Fixture {
            contract_id,
            account: Address::generate(&env),
            owner: Address::generate(&env),
            new_owner: Address::generate(&env),
            guardians,
            env,
        }
    }

    /// Account guarded by three guardians with a quorum of two.
    fn setup() -> Fixture {
        let f = setup_uninitialized();
        assert_eq!(f.initialize(&f.guardians, 2), Ok(()));
        f
    }

    #[test]
    fn initialize_stores_owner_guardians_and_quorum() {
        let f = setup();

        assert_eq!(f.current_owner(), Ok(f.owner.clone()));
        assert_eq!(
            f.run(|env| get_guardians(env, &f.account)),
            Ok(f.guardians.clone())
        );
        assert_eq!(f.run(|env| get_quorum(env, &f.account)), Ok(2));
        assert_eq!(f.request(), None);
    }

    #[test]
    fn initialize_extends_the_storage_ttl() {
        let f = setup();

        for key in [
            RecoveryKey::Owner(f.account.clone()),
            RecoveryKey::GuardianList(f.account.clone()),
            RecoveryKey::Quorum(f.account.clone()),
        ] {
            assert!(f.ttl(key) >= PERMISSION_TTL_BUMP);
        }
    }

    #[test]
    fn initialize_is_one_shot() {
        let f = setup();

        assert_eq!(
            f.initialize(&f.guardians, 1),
            Err(RecoveryError::AlreadyInitialized)
        );
        assert_eq!(f.run(|env| get_quorum(env, &f.account)), Ok(2));
    }

    #[test]
    fn initialize_rejects_invalid_quorums() {
        let f = setup_uninitialized();

        for quorum in [0, 4] {
            assert_eq!(
                f.initialize(&f.guardians, quorum),
                Err(RecoveryError::InvalidQuorum)
            );
        }
        assert_eq!(
            f.initialize(&Vec::new(&f.env), 1),
            Err(RecoveryError::InvalidQuorum)
        );

        let duplicated = vec![&f.env, f.guardian(0), f.guardian(0)];
        assert_eq!(
            f.initialize(&duplicated, 2),
            Err(RecoveryError::InvalidQuorum)
        );

        let mut too_many = Vec::new(&f.env);
        for _ in 0..=MAX_GUARDIANS {
            too_many.push_back(Address::generate(&f.env));
        }
        assert_eq!(
            f.initialize(&too_many, 2),
            Err(RecoveryError::InvalidQuorum)
        );
        assert_eq!(f.current_owner(), Err(RecoveryError::NotInitialized));
    }

    #[test]
    fn submit_records_the_request() {
        let f = setup();

        let request = f.submit(&f.guardian(0)).unwrap();
        assert_eq!(
            request,
            RecoveryRequest {
                owner: f.owner.clone(),
                new_owner: f.new_owner.clone(),
                approvals: vec![&f.env, f.guardian(0)],
                initiated_ledger: START,
            }
        );
        assert_eq!(f.request(), Some(request));
        assert_eq!(
            f.run(|env| execution_ledger(env, &f.account)),
            Ok(START + RECOVERY_COOLDOWN_LEDGERS)
        );
        assert!(f.ttl(RecoveryKey::RecoveryRequest(f.account.clone())) >= PERMISSION_TTL_BUMP);
    }

    #[test]
    fn submit_rejects_invalid_requests() {
        let uninitialized = setup_uninitialized();
        assert_eq!(
            uninitialized.submit(&uninitialized.guardian(0)),
            Err(RecoveryError::NotInitialized)
        );

        let f = setup();
        assert_eq!(
            f.submit(&Address::generate(&f.env)),
            Err(RecoveryError::Unauthorized)
        );
        assert_eq!(
            f.run(|env| {
                submit_recovery(env, f.guardian(0), f.account.clone(), f.owner.clone())
            }),
            Err(RecoveryError::InvalidNewOwner)
        );

        f.submit(&f.guardian(0)).unwrap();
        assert_eq!(
            f.submit(&f.guardian(1)),
            Err(RecoveryError::RequestAlreadyExists)
        );
    }

    #[test]
    fn approve_rejects_outsiders_duplicates_and_missing_requests() {
        let f = setup();

        assert_eq!(f.approve(&f.guardian(0)), Err(RecoveryError::NoRequest));
        f.submit(&f.guardian(0)).unwrap();
        assert_eq!(
            f.approve(&f.guardian(0)),
            Err(RecoveryError::AlreadyApproved)
        );
        assert_eq!(
            f.approve(&Address::generate(&f.env)),
            Err(RecoveryError::Unauthorized)
        );
        assert_eq!(f.request().unwrap().approvals.len(), 1);
    }

    #[test]
    fn recovery_transfers_ownership_after_the_cooldown() {
        let f = setup();
        f.submit(&f.guardian(0)).unwrap();
        assert_eq!(f.approve(&f.guardian(1)).unwrap().approvals.len(), 2);

        f.advance_to(START + RECOVERY_COOLDOWN_LEDGERS - 1);
        assert_eq!(f.execute(), Err(RecoveryError::CooldownNotElapsed));
        assert_eq!(f.current_owner(), Ok(f.owner.clone()));

        f.advance_to(START + RECOVERY_COOLDOWN_LEDGERS);
        assert_eq!(f.execute(), Ok(f.new_owner.clone()));
        assert_eq!(f.current_owner(), Ok(f.new_owner.clone()));
        assert_eq!(f.request(), None);
        assert_eq!(f.execute(), Err(RecoveryError::NoRequest));
    }

    #[test]
    fn execution_requires_the_quorum() {
        let f = setup();
        f.submit(&f.guardian(0)).unwrap();

        f.advance_to(START + RECOVERY_COOLDOWN_LEDGERS);
        assert_eq!(f.execute(), Err(RecoveryError::QuorumNotReached));
        assert_eq!(f.current_owner(), Ok(f.owner.clone()));
    }

    #[test]
    fn owner_can_cancel_during_the_cooldown() {
        let f = setup();
        f.submit(&f.guardian(0)).unwrap();
        f.approve(&f.guardian(1)).unwrap();

        assert_eq!(f.cancel(&f.owner), Ok(()));
        assert_eq!(f.request(), None);
        assert_eq!(f.cancel(&f.owner), Err(RecoveryError::NoRequest));

        f.advance_to(START + RECOVERY_COOLDOWN_LEDGERS);
        assert_eq!(f.execute(), Err(RecoveryError::NoRequest));
        assert_eq!(f.current_owner(), Ok(f.owner.clone()));
    }

    #[test]
    fn only_the_owner_can_cancel() {
        let f = setup();
        f.submit(&f.guardian(0)).unwrap();

        assert_eq!(f.cancel(&f.guardian(0)), Err(RecoveryError::Unauthorized));
        assert!(f.request().is_some());
    }
}
