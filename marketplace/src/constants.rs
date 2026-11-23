//! Storage TTL constants for marketplace listings (issue #477).

use soroban_sdk::{contracttype, Address};

/// Ledgers a listing entry's TTL is extended to on bump (~30 days at 5s/ledger).
pub const LISTING_BUMP_LEDGERS: u32 = 518_400;

/// Remaining-TTL threshold below which a listing entry is bumped (~10 days).
pub const LISTING_THRESHOLD_LEDGERS: u32 = 172_800;

/// Snapshot of a listing's storage-relevant fields, returned by
/// `bump_listing_ttl` so indexers can confirm what was refreshed.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListingStorageMetadata {
    /// Owner of the merchant that published the listing.
    pub merchant: Address,
    /// Unit price in stroops.
    pub price_stroops: i128,
    /// Available stock (saturated to `u32::MAX`).
    pub stock: u32,
    /// Ledger sequence at which the TTL was last extended.
    pub last_bumped_ledger: u32,
}
