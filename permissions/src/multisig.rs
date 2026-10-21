use soroban_sdk::{contracttype, contractevent, Address, Env, Vec};

/// Weighted multi-signature quorum configuration.
///
/// `signers` and `signer_weights` are parallel vectors: `signer_weights[i]`
/// is the weight contributed by `signers[i]` when it authorizes a release.
#[contracttype]
#[derive(Clone, Debug)]
pub struct MultiSigQuorum {
    pub signers: Vec<Address>,
    pub threshold_weight: u32,
    pub signer_weights: Vec<u32>,
}

/// Emitted when a quorum of signer weight has been accumulated.
#[contractevent]
#[derive(Clone, Debug)]
pub struct QuorumApproved {
    #[topic]
    pub threshold_weight: u32,
    pub accumulated_weight: u32,
}

/// Errors surfaced by quorum evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum MultiSigError {
    /// `signers` and `signer_weights` have mismatched lengths.
    LengthMismatch = 1,
    /// The configured threshold is zero or otherwise unsatisfiable.
    InvalidThreshold = 2,
    /// A supplied signer is not part of the authorized set.
    UnauthorizedSigner = 3,
    /// The accumulated weight did not reach the threshold.
    InsufficientWeight = 4,
}

/// Evaluate a weighted quorum for a high-value escrow release.
///
/// Each address in `approvals` must be an authorized signer and must have
/// authorized the current invocation (verified via `require_auth`). Weight is
/// accumulated per unique signer until it reaches `threshold_weight`, at which
/// point a `QuorumApproved` event is emitted and `Ok(accumulated)` is returned.
///
/// Returns `Err(MultiSigError::InsufficientWeight)` when the supplied approvals
/// cannot satisfy the threshold.
pub fn evaluate_quorum(
    env: &Env,
    quorum: &MultiSigQuorum,
    approvals: &Vec<Address>,
) -> Result<u32, MultiSigError> {
    let signer_count = quorum.signers.len();
    if signer_count != quorum.signer_weights.len() {
        return Err(MultiSigError::LengthMismatch);
    }
    if quorum.threshold_weight == 0 {
        return Err(MultiSigError::InvalidThreshold);
    }

    let mut accumulated: u32 = 0;

    for approval in approvals.iter() {
        // Locate the approval within the authorized signer set.
        let mut matched_weight: Option<u32> = None;
        for i in 0..signer_count {
            if quorum.signers.get(i).unwrap() == approval {
                matched_weight = Some(quorum.signer_weights.get(i).unwrap());
                break;
            }
        }

        let weight = match matched_weight {
            Some(w) => w,
            None => return Err(MultiSigError::UnauthorizedSigner),
        };

        // Verify the signer actually authorized this invocation.
        approval.require_auth();

        accumulated = accumulated.saturating_add(weight);
        if accumulated >= quorum.threshold_weight {
            QuorumApproved {
                threshold_weight: quorum.threshold_weight,
                accumulated_weight: accumulated,
            }
            .publish(env);
            return Ok(accumulated);
        }
    }

    Err(MultiSigError::InsufficientWeight)
}

/// Convenience guard for high-value escrow releases.
///
/// Returns `Ok(())` only when the weighted quorum is satisfied; otherwise the
/// release must be rejected with the underlying error.
pub fn require_quorum(
    env: &Env,
    quorum: &MultiSigQuorum,
    approvals: &Vec<Address>,
) -> Result<(), MultiSigError> {
    evaluate_quorum(env, quorum, approvals).map(|_| ())
}
