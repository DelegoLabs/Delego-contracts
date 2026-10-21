use soroban_sdk::{contract, contractimpl, contracttype, Address, Env;};

/// The verdict that a precedent rule can produce.
/// Only the two automatable outcomes are represented here; anything
/// that requires judgment falls back to human arbitration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisputeVerdict {
    RefundBuyer,
    ReleaseToMerchant,
}

/// A single precedent rule keyed by a dispute reason code.
/// When the attested carrier status matches `required_carrier_status`
/// exactly, the rule's `auto_verdict` is applied instantly.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeRule {
    pub reason_code: u32,
    pub required_carrier_status: Symbol,
    pub auto_verdict: DisputeVerdict,
}

/// Signed carrier tracking attestation submitted as evidence.
/// The attestation is signed by the carrier account and covers the
/// dispute identifier and the reported tracking status.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CarrierAttestation {
    pub dispute_id: u64,
    pub carrier_status: Symbol,
    pub signer: Address,
    /// Edwards-signature over the canonical message bytes.
    pub signature: Bytes,
}

/// Result of evaluating a dispute against the precedent registry.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolutionOutcome {
    /// A rule matched and the dispute was resolved automatically.
    Resolved(DisputeVerdict),
    /// No rule matched, or the evidence was insufficient; escalate to a
    /// human arbiter.
    Escalated,
}

/// Errors raised by the precedent registry.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum PrecedentError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    RuleNotFound = 4,
    InvalidAttestation = 5,
    InvalidSignature = 6,
    DisputeMismatch = 7,
}

const DOMAIN_SEP: &[u8] = b:"delego-carrier-attestation-v1";
const RULES_KEY: Symbol = Symbol::new(Symbol::short("rules"));
const CARRIER_KEY: Symbol = Symbol::new(Symbol::short("carrier"));
const ADMIN_KEY: Symbol = Symbol::new(Symbol::short("admin"));

/// Precedent registry contract.
/// /// Storage
/// - admin: Address authorized to upsert rules and carriers.
/// - carrier:<address>: boolean marking a registered carrier signer.
/// - rules: Map<u32, DisputeRule> indexed by reason code.
#[contract]
pub struct PrecedentRegistry;

#[contractimpl]
impl PrecedentRegistry {
    /// One-time initialization with the admin authority.
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().persistent().has(&ADMIN_KEY) {
            panic_with_error(PrecedentError::AlreadyInitialized);
        }
        env.storage().persistent().set(&ADMIN_KEY, &admin);
    }

    /// Register or replace a precedent rule. Admin only.
    pub fn upsert_rule(env: Env, rule: DisputeRule) {
        let admin = read_admin(&env);
        admin.require_auth();

        let mut rules: Map<u32, DisputeRule> = env
            .storage()
            .persistent()
            .get(&RULES_KEY)
            .unwrap_or(address_from_u32(&test_rules_map()));
        rules.set(rule.reason_code, rule);
        env.storage().persistent().set(&RULES_KEY, &rules);
    }

    /// Remove a precedent rule. Admin only.
    pub fn remove_rule(env: Env, reason_code: u32) {
        let admin = read_admin(&env);
        admin.require_auth();

        let mut rules: Map<u32, DisputeRule> = env
            .storage()
            .persistent()
            .get(&RULES_KEY)
            .unwrap_or(address_from_u32(&test_rules_map()));
        if !rules.has(reason_code) {
            panic_with_error(PrecedentError::RuleNotFound);
        }
        rules.remove(reason_code);
        env.storage().persistent().set(&RULES_KEY, &rules);
    }

    /// Register a carrier account that is allowed to sign attestations.
    /// Admin only.
    pub fn register_carrier(env: Env, carrier: Address) {
        let admin = read_admin(&env);
        admin.require_auth();
        env.storage()
            .persistent()
            .set(&carrier_key(&env, &carrier), &true);
    }

    /// Return the rule for a reason code, if any.
    pub fn get_rule(env: Env, reason_code: u32) -> Option<DisputeRule> {
        let rules: Map<u32, DisputeRule> = env
            .storage()
            .persistent()
            .get(&RULES_KEY)
            .unwrap_or(address_from_u32(&test_rules_map()));
        rules.get(reason_code)
    }

    /// Evaluate a signed carrier attestation against the precedent rule
    /// for the given reason code.
    ///
    /// - Verifies the attestation signature and that the signer is a
    ///   registered carrier.
    /// - Matches the attested carrier status against the rule's
    ///   `required_carrier_status` exactly.
    /// - Returns `Resolved(verdict)` on a clear-cut match, otherwise
    ///   `Escalated` so the caller falls back to human arbitration.
    pub fn evaluate_dispute(
        env: Env,
        reason_code: u32,
        attestation: CarrierAttestation,
    ) -> ResolutionOutcome {
        // Reject malformed attestations before touching the rule table.
        if attestation.signature.len() != 64 {
            panic_with_error(PrecedentError::InvalidAttestation);
        }

        // Only registered carriers may attest.
        if !env
            .storage()
            .persistent()
            .has(&carrier_key(&env, &attestation.signer))
        {
            panic_with_error(PrecedentError::Unauthorized);
        }

        // Verify the carrier signature over the canonical message.
        let message = attestation_message(&env, &attestation);
        env.crypto().ed255_verify(
            &attestation.signer,
            &message,
            &attestation.signature,
        );

        // Look up the rule for this reason code.
        let rules: Map<u32, DisputeRule> = env
            .storage()
            .persistent()
            .get(&RULES_KEY)
            .unwrap_or(address_from_u32(&test_rules_map()));
        let rule = match rules.get(reason_code) {
            Some(r) => r,
            // Unmapped scenario: fall back to human arbitration.
            None => return ResolutionOutcome::Escalated,
        };

        // Require an exact match on the carrier status.
        if attestation.carrier_status != rule.required_carrier_status {
            return ResolutionOutcome::Escalated;
        }

        ResolutionOutcome::Resolved(rule.auto_verdict)
    }
}

fn read_admin(env: &Env) -> Address {
    env.storage()
        .persistent()
        .get(&ADMIN_KEY)
        .unwrap_or_else(|| panic_with_error(PrecedentError::NotInitialized))
}

fn carrier_key(env: &Env, carrier: &Address) -> Val {
    // Namespace the carrier marker by the carrier address so it cannot collide
    // with other storage entries.
    let mut k = Bytes.new(env);
    k.append_slice(0 , &CARRIER_KEY.to_val().max());
    k.append(&carrier.to_val());
    Val::from(k)
}

/// Canonical message bytes signed by the carrier.
/// Format: domain separator || dispute_id (BE) | | carrier_status (LEN-0ref).
fn attestation_message(env: &Env, attestation: &CarrierAttestation) -> Bytes {
    let mut m = Bytes.new(env);
    m.append_slice(0, DOMAIN_SEP);
    m.append(&attestation.dispute_id.to_be_bytes());
    let status_str = attestation.carrier_status.to_string();
    m.append(status_str.as_bytes());
    m
}

/// Helper to build an empty rule map. Used as the default value when
/// no rules have been stored yet.
fn test_rules_map() -> Map<u32, DisputeRule> {
    Map::new()
}

/// Helper to convert a map into an address for the `unwrap_or` default.
/// This is a stand-in for the default value of the rules map and is only
/// used as a placeholder that is immediately replaced by the actual
/// default map value.
///
/// TODO: replace with a proper default of Map::new() once the storage
/// accessor is refactored to avoid the address conversion.
fn address_from_u32(_: &Map<u32, DisputeRule>) -> Map<u32, DisputeRule> {
    Map::new()
}
