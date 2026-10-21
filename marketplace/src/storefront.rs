use crate::{
    DataKey, MarketplaceContract, MarketplaceError, Merchant, PERSISTENT_BUMP_AMOUNT,
    PERSISTENT_BUMP_THRESHOLD,
};
use soroban_sdk::{contracttype, symbol_short, Address, BytesN, Env, String};

const MAX_IPFS_CID_LEN: u32 = 128;

/// Off-chain storefront metadata and its SHA-256 content digest.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorefrontMetadata {
    pub ipfs_cid: String,
    pub content_hash: BytesN<32>,
    pub updated_at: u64,
}

/// Event emitted when a merchant updates their storefront metadata.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorefrontMetadataUpdatedEvent {
    pub merchant_id: u64,
    pub metadata: StorefrontMetadata,
}

/// Stores a validated IPFS CID and its SHA-256 digest for the merchant.
pub fn set_storefront_metadata(
    env: Env,
    merchant_id: u64,
    caller: Address,
    ipfs_cid: String,
    content_hash: BytesN<32>,
) -> Result<(), MarketplaceError> {
    caller.require_auth();

    let merchant: Merchant = env
        .storage()
        .persistent()
        .get(&DataKey::Merchant(merchant_id))
        .ok_or(MarketplaceError::MerchantNotFound)?;
    MarketplaceContract::check_not_frozen_or_closed(&merchant)?;

    if merchant.owner.as_ref() != Some(&caller) {
        return Err(MarketplaceError::Unauthorized);
    }
    if !is_valid_ipfs_cid(&ipfs_cid) {
        return Err(MarketplaceError::InvalidIpfsCid);
    }

    let metadata = StorefrontMetadata {
        ipfs_cid,
        content_hash,
        updated_at: env.ledger().timestamp(),
    };
    let key = DataKey::StorefrontMetadata(merchant_id);
    env.storage().persistent().set(&key, &metadata);
    env.storage().persistent().extend_ttl(
        &key,
        PERSISTENT_BUMP_THRESHOLD,
        PERSISTENT_BUMP_AMOUNT,
    );

    env.events().publish(
        (
            symbol_short!("mkplc"),
            symbol_short!("sf_meta"),
            merchant_id,
        ),
        StorefrontMetadataUpdatedEvent {
            merchant_id,
            metadata,
        },
    );

    Ok(())
}

fn is_valid_ipfs_cid(cid: &String) -> bool {
    let len = cid.len();
    if len == 0 || len > MAX_IPFS_CID_LEN {
        return false;
    }

    let mut bytes = [0u8; MAX_IPFS_CID_LEN as usize];
    cid.copy_into_slice(&mut bytes[..len as usize]);

    match bytes[0] {
        // CIDv0 is a 46-character base58btc multihash.
        b'Q' => {
            len == 46
                && bytes[1] == b'm'
                && bytes[..len as usize].iter().all(|byte| is_base58(*byte))
        }
        // CIDv1 uses a multibase prefix followed by lower-case base32 or base58btc.
        b'b' => {
            len >= 10
                && bytes[1..len as usize]
                    .iter()
                    .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(byte))
        }
        b'z' => {
            len >= 10
                && bytes[1..len as usize]
                    .iter()
                    .all(|byte| is_base58(*byte))
        }
        _ => false,
    }
}

fn is_base58(byte: u8) -> bool {
    matches!(
        byte,
        b'1'..=b'9'
            | b'A'..=b'H'
            | b'J'..=b'N'
            | b'P'..=b'Z'
            | b'a'..=b'k'
            | b'm'..=b'z'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MarketplaceContractClient, RegisterParams};
    use soroban_sdk::{
        symbol_short,
        testutils::{Address as _, Events as _, Ledger as _},
        Symbol, TryIntoVal,
    };

    struct TestFixture<'a> {
        env: Env,
        owner: Address,
        contract_id: Address,
        client: MarketplaceContractClient<'a>,
    }

    impl<'a> TestFixture<'a> {
        fn setup() -> Self {
            let env = Env::default();
            env.mock_all_auths();
            let admin = Address::generate(&env);
            let contract_id = env.register(MarketplaceContract, (admin,));
            let owner = Address::generate(&env);
            let client = MarketplaceContractClient::new(&env, &contract_id);
            client.register_merchant(
                &owner,
                &RegisterParams {
                    name: String::from_str(&env, "Storefront"),
                    description: String::from_str(&env, "Storefront test"),
                    category: symbol_short!("retail"),
                    image_url: String::from_str(&env, "https://example.com/logo.png"),
                    metadata: None,
                    metadata_uri: None,
                    required_verifications: 1,
                },
            );
            Self {
                env,
                owner,
                contract_id,
                client,
            }
        }
    }

    fn cid(env: &Env) -> String {
        String::from_str(env, "Qmaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    }

    #[test]
    fn stores_metadata_and_emits_update_event() {
        let f = TestFixture::setup();
        let merchant_id = f.client.get_merchant_id_by_owner(&f.owner);
        let content_hash = BytesN::from_array(&f.env, &[7; 32]);
        let storefront_cid = cid(&f.env);
        f.env.ledger().set_timestamp(1234);

        f.client
            .set_storefront_metadata(&merchant_id, &f.owner, &storefront_cid, &content_hash);

        let stored: StorefrontMetadata = f.env.as_contract(&f.contract_id, || {
            f.env
                .storage()
                .persistent()
                .get(&DataKey::StorefrontMetadata(merchant_id))
                .unwrap()
        });
        assert_eq!(
            stored,
            StorefrontMetadata {
                ipfs_cid: storefront_cid,
                content_hash: content_hash.clone(),
                updated_at: 1234,
            }
        );

        let events = f.env.events().all();
        let event = events.iter().find(|(_, topics, _)| {
            let topics: soroban_sdk::Vec<soroban_sdk::Val> = topics.clone();
            if topics.len() != 3 {
                return false;
            }
            let namespace: Result<Symbol, _> = topics.get(0).unwrap().try_into_val(&f.env);
            let action: Result<Symbol, _> = topics.get(1).unwrap().try_into_val(&f.env);
            let id: Result<u64, _> = topics.get(2).unwrap().try_into_val(&f.env);
            namespace == Ok(symbol_short!("mkplc"))
                && action == Ok(symbol_short!("sf_meta"))
                && id == Ok(merchant_id)
        });
        assert!(event.is_some());
        let (_, _, data) = event.unwrap();
        let emitted: StorefrontMetadataUpdatedEvent = data.try_into_val(&f.env).unwrap();
        assert_eq!(emitted.metadata, stored);
    }

    #[test]
    fn rejects_non_owner_and_invalid_cid() {
        let f = TestFixture::setup();
        let merchant_id = f.client.get_merchant_id_by_owner(&f.owner);
        let hash = BytesN::from_array(&f.env, &[9; 32]);
        let other = Address::generate(&f.env);

        assert_eq!(
            f.client
                .try_set_storefront_metadata(&merchant_id, &other, &cid(&f.env), &hash),
            Err(Ok(MarketplaceError::Unauthorized))
        );
        assert_eq!(
            f.client.try_set_storefront_metadata(
                &merchant_id,
                &f.owner,
                &String::from_str(&f.env, "not-a-cid"),
                &hash
            ),
            Err(Ok(MarketplaceError::InvalidIpfsCid))
        );
    }
}
