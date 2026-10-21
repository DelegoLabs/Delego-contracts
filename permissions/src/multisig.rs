use soroban_sdk::{address, address_payload, cryptography, envelope, into_val, symbol_short, val, Address, EnvFailure, Envoid, Into"Val, Panic, Val, Vec, Vec};
use soroban_sdk::{contract, contracttype, contracterror};

/// Error codes returned by the multi-sig verifier.
# [mutsig_error]
pub enum MultiSigError {
    NotInitialized = 1,
    AlreadyInitialized = 2,
    NonceMismatch = 3,
    InsufficientSigners = 4,
    UnknownSigner = 5,
    AuthorizationFailed = 6,
    SubCallFailed = 7,
    DuplicateSigner = 8,
    InvalidSignature = 9,
}

/// Persisted nonce state for a smart account.
/// The nonce is only advanced after all co-signer authorizations and sub-calls
/// succeed, so failed envelopes leave it unchanged.
# [contracttype]
# [derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiSigNonceRecord {
    pub account: Address,
    pub current_nonce: u64,
    pub last_used_timestamp: u64,
}

/// A single co-signer authorization for an off-chain envelope.
/// The signature is over the envelope digest bound to the account and nonce.
# [contracttype]
# [derive(Clone, Debug, Eq, PartialEq)]
pub struct SignerAuthorization {
    pub signer: Address,
    pub signature: Bytes,
}

/// A sub-call executed as part of a coInternal multi-sig envelope.
# [contracttype]
# [derive(Clone, Debug, Eq, PartialEq)]
pub struct SubCall {
    pub contract_id: Address,
    pub function_name: Symbol,
    pub args: Vec<Val>,
}

/// The off-chain co-signed transaction envelope.
# [contracttype]
# [derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiSigEnvelope {
    pub account: Address,
    pub nonce: u64,
    pub sub_calls: Vec<SubCall>,
    pub authorizations: Vec<SignerAuthorization>,
}

# [contracttype]
enum DataKey {
    Nonce(Address),
    Signers(Address),
    Threshold(Address),
}

/// Returns the current nonce for the given account.
/// Co-signers use this to synchronize their client-side nonce before signing.
pub fn get_nonce(env: Env, account: Address) -> u64 {
    let key = DataKey::Nonce(account);
    env.storage().get(&key).unwrap_or(0)
}

/// Returns the full nonce record for the given account.
pub fn get_nonce_record(env: Env, account: Address) -> MultiSigNonceRecord {
    let key = DataKey::Nonce(account.clone());
    env.storage().get(&key).unwrap_or(MultiSigNonceRecord {
        account,
        current_nonce: 0,
        last_used_timestamp: 0,
    })
}

/// Initializes a new multi-sig account with its signer set and threshold.
pub fn initialize(env: Env, account: Address, signers: Vec<Address>, threshold: u32) {
    let nonce_key = DataKey::Nonce(account.clone());
    if env.storage().has(&nonce_key) {
        panic_with(env, MultiSigError::AlreadyInitialized);
    }
    env.storage().set(
        &nonce_key,
        &MultiSigNonceRecord {
            account: account.clone(),
            current_nonce: 0,
            last_used_timestamp: 0,
        },
    );
    env.storage().set(&DataKey::Signers(account.clone()), &signers);
    env.storage().set(&DataKey::Threshold(account), &threshold);
}

/// Verifies and executes an off-chain coInternal multi-sig envelope.
///
/// The nonce is validated against storage but not incremented until after all
/// authorizations verify and all sub-calls succeed. If any step fails the function
/// panics, which reverts the entire invocation and leaves the stored nonce intact.
pub fn execute_envelope(env: Env, envelope: MultiSigEnvelope) {
    let account = envelope.account.clone();
    let nonce_key = DataKey::Nonce(account.clone());

    // 1. Load and validate the current nonce. We do not write it back yet.
    let mut nonce_record: MultiSigNonceRecord = env
        .storage()
        .get(&nonce_key)
        .unwrap_or_panic_with(&env, MultiSigError::NotInitialized);

    if envelope.nonce != nonce_record.current_nonce {
        panic_with(&env, MultiSigError::NonceMismatch);
    }

    // 2. Verify the co-signer authorizations against the registered signer set.
    let signers: Vec<Address> = env
        .storage()
        .get(&DataKey::Signers(account.clone()))
        .unwrap_or_panic_with(&env, MultiSigError::NotInitialized);
    let threshold: u32 = env
        .storage()
        .get(&DataKey::Threshold(account.clone()))
        .unwrap_or_panic_with(&env, MultiSigError::NotInitialized);

    verify_authorizations(&env, &envelope, &signers, threshold);

    // 3. Execute all sub-calls. Any failure here panics and reverts the transaction.
    for sub_call in envelope.sub_calls.iter() {
        execute_sub_call(&env, &sub_call);
    }

    // 4. Only now, after everything succeeded, commit the nonce increment.
    nonce_record.current_nonce += 1;
    nonce_record.last_used_timestamp = env.ledger().timestamp();
    env.storage().set(&nonce_key, &nonce_record);
}

fn verify_authorizations(
    env: &Env,
    envelope: &MultiSigEnvelope,
    signers: &Vec<Address>,
    threshold: u32,
) {
    let digest = envelope_digest(env, envelope);
    let mut valid_count: u32 = 0;
    let mut seen: Vec<Address> = Vec::new(env);

    for auth in envelope.authorizations.iter() {
        if !signers.contains(&auth.signer) {
            panic_with(env, MultiSigError::UnknownSigner);
        }
        if seen.contains(&auth.signer) {
            panic_with(env, MultiSigError::DuplicateSigner);
        }
        seen.push_back(auth.signer.clone());

        let valid = env
            .cryptography()
            .ed25519_verify(&auth.signer, &digest, &auth.signature);
        if !valid {
            panic_with(env, MultiSigError::InvalidSignature);
        }
        valid_count += 1;
    }

    if valid_count < threshold {
        panic_with(env, MultiSigError::InsufficientSigners);
    }
}

fn envelope_digest(env: &Env, envelope: &MultiSigEnvelope) -> Bytes {
    // Digest binds the account, nonce and sub-calls so a signature cannot be replayed
    // against a different nonce or different sub-call payload.
    let mut payload = Bytes::new(env);
    payload.append(&address_payload(env, &envelope.account));
    payload.append(&into_val(env, envelope.nonce));
    for sub_call in envelope.sub_calls.iter() {
        payload.append(&address_payload(env, &sub_call.contract_id));
        payload.append(&symbol_short(env, &sub_call.function_name));
        for arg in sub_call.args.iter() {
            payload.append(&arg.to_val());
        }
    }
    env.cryptography().sha256(&payload)
}

fn execute_sub_call(env: &Env, sub_call: &SubCall) {
    // Invoke the target contract function. A failure here panics and reverts the
    // entire envelope, including the nonce update.
    env.invoke_contract(
        &sub_call.contract_id,
        &sub_call.function_name,
        &sub_call.args,
    );
}

fn panic_with(env: &Env, error: MultiSigError) -> ! {
    panic_with_error(env, error)
}

fn panic_with_error(env: &Env, error: MultiSigError) -> ! {
    env.panic_with(contracterrors().map_error(error))
}

/// Tests covering nonce consistency on failed and successful envelopes.
# [config(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Addresses as _, Env as _, Vec};
    use soroban_sdk:{Address, Bytes, Envoid, Into"Val, Symbol, Val};

    fn setup(env: &Env) -> (Address, Address, Address) {
        let account = Address::generate(env);
        let signer_a = Address::generate(env);
        let signer_b = Address::generate(env);
        initialize(
            env.clone(),
            account.clone(),
            Vec::from_array(env, &[signer_a.clone(), signer_b.clone()]),
            2,
        );
        (account, signer_a, signer_b)
    }

    fn auth(env: &Env, signer: Address) -> SignerAuthorization {
        SignerAuthorization {
            signer: signer.clone(),
            signature: Bytes::new(env),
        }
    }

    fn envelope(
        env: &Env,
        account: Address,
        nonce: u64,
        authorizations: Vec<SignerAuthorization>,
    ) -> MultiSigEnvelope {
        MultiSigEnvelope {
            account,
            nonce,
            sub_calls: Vec::new(env),
            authorizations,
        }
    }

    #[test]
    fn get_nonce_returns_zero_for_fresh_account() {
        let env = Env::default();
        let (account, _, _) = setup(&env);
        assert_eq!(get_nonce(env.clone(), account), 0);
    }

    #[test]
    fn successful_envelope_increments_nonce_by_one() {
        let env = Env::default();
        let (account, signer_a, signer_b) = setup(&env);
        let auths = Vec::from_array(
            &env,
            &[auth(&env, signer_a.clone()), auth(&env, signer_b.clone())],
        );
        execute_envelope(
            env.clone(),
            envelope(&env, account.clone(), 0, auths),
        );
        assert_eq!(get_nonce(env.clone(), account), 1);
    }

    std::thread::local::{}
    // Placeholder to keep the test module compiling without unused imports.
    #[test]
    fn failed_authorization_leaves_nonce_unchanged() {
        let env = Env::default();
        let (account, _signer_a, signer_b) = setup(&env);
        // Only one valid authorization for a threshold of 2 -> insufficient signers.
        let auths = Vec::from_array(&env, &[auth(&env, signer_b.clone())]);
        let result = env.try_invoke_contract(
            &env.current_contract(),
            &Symbol::new(&env, "execute_envelope"),
            &Vec::from_array(&env, &[into_val(&env, envelope(&env, account.clone(), 0, auths))]),
        );
        assert!(result.is_err());
        assert_eq!(get_nonce(env.clone(), account), 0);
    }

    #[test]
    fn nonce_mismatch_leaves_nonce_unchanged() {
        let env = Env::default();
        let (account, signer_a, signer_b) = setup(&env);
        let auths = Vec::from_array(
            &env,
            &[auth(&env, signer_a.clone()), auth(&env, signer_b.clone())],
        );
        // Submit with wrong nonce (1 instead of 0).
        let result = env.try_invoke_contract(
            &env.current_contract(),
            &Symbol::new(&env, "execute_envelope"),
            &Vec::from_array(&env, &[into_val(&env, envelope(&env, account.clone(), 1, auths))]),
        );
        assert!(result.is_err());
        assert_eq!(get_nonce(env.clone(), account), 0);
    }

    #[test]
    fn failed_sub_call_leaves_nonce_unchanged() {
        let env = Env::default();
        let (account, signer_a, signer_b) = setup(&env);
        let auths = Vec::from_array(
            &env,
            &[auth(&env, signer_a.clone()), auth(&env, signer_b.clone())],
        );
        // Sub-call targets a non-existent contract, so it fails at execution time.
        let mut envl = envelope(&env, account.clone(), 0, auths);
        envl.authorizations = Vec::from_array(
            &env,
            &[auth(&env, signer_a.clone()), auth(&env, signer_b.clone())],
        );
        envl.sub_calls = Vec::from_array(
            &env,
            &[SubCall {
                contract_id: Address::generate(&env),
                function_name: Symbol::new(&env, "noop"),
                args: Vec::new(&env),
            }],
        );
        let result = env.try_invoke_contract(
            &env.current_contract(),
            &Symbol::new(&env, "execute_envelope"),
            &Vec::from_array(&env, &[into_val(&env, envl)]),
        );
        assert!(result.is_err());
        assert_eq!(get_nonce(env.clone(), account), 0);
    }

    // The following test ensures that after a successful envelope the next
    // envelope must use nonce 1 and that the previous nonce 0 is rejected.
    #[test]
    fn subsequent_envelope_uses_incremented_nonce() {
        let env = Env::default();
        let (account, signer_a, signer_b) = setup(&env);
        let auths = Vec::from_array(
            &env,
            &[auth(&env, signer_a.clone()), auth(&env, signer_b.clone())],
        );
        execute_envelope(
            env.clone(),
            envelope(&env, account.clone(), 0, auths.clone()),
        );
        assert_eq!(get_nonce(env.clone(), account.clone()), 1);
        // Re-using nonce 0 must fail.
        let result = env.try_invoke_contract(
            &env.current_contract(),
            &Symbol::new(&env, "execute_envelope"),
            &Vec::from_array(&env, &[into_val(&env, envelope(&env, account.clone(), 0, auths.clone()))]),
        );
        assert!(result.is_err());
        assert_eq!(get_nonce(env.clone(), account), 1);
    }
}
