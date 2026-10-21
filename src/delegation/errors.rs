use soroban_sdk::{contracterror, Env};

#[contracterror]
#[derive(Debug, Eq, PartialEq)]
pub enum PermissionError {
    #[msg("Permission denied")]
    PermissionDenied,

    #[msg("Signature epoch is stale")]
    StaleEpoch,

    #[msg("Invalid authorization signature")]
    InvalidSignature,

    #[msg("Token asset is not authorized for this delegation")]
    AssetNotAuthorized,

    #[msg("Spend exceeds the configured allowance")]
    ExceedsAllowance,

    #[msg("Delegation is paused")]
    DelegationPaused,

    #[msg("Delegation has expired")]
    DelegationExpired,
    #[msg("Account is frozen by circuit breaker")]
    AccountFrozen,

    #[msg("Account is not frozen")]
    AccountNotFrozen,

    #[msg("Multi-factor confirmation required")]
    MultiFactorConfirmationRequired,

    #[msg("Master key confirmation required")]
    MasterKeyConfirmationRequired,
}
