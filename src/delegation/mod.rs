//! Delegated spending module: signed authorizations, delegation state and the
//! contract entry points that bind them together.

pub mod authorizations;
pub mod contract;
pub mod errors;
pub mod state;

#[cfg(test)]
mod test;

pub use authorizations::{SpendAuthPayload, SpendAuthorization, SPEND_AUTH_DOMAIN};
pub use contract::{DataKey, DelegationContract, DelegationContractClient};
pub use errors::PermissionError;
pub use state::{
    AccountCircuitBreaker, AccountCircuitBreakerStatus, AssetAllowance, DelegationState,
    EpochConfig, MultiAssetDelegation,
};
