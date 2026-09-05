#![cfg(test)]

use delego_escrow::{BatchDepositParams, EscrowContract, EscrowContractClient, EscrowStatus};
use delego_marketplace::{
    ExternalReputationScore, MarketplaceContract, MarketplaceContractClient, MerchantView,
    ReputationResolution,
use delego_escrow::{
    BatchDepositParams, EscrowConfig, EscrowContract, EscrowContractClient, EscrowStatus,
};
use delego_delegation::DelegationError;
    BatchDepositParams, EscrowContract, EscrowContractClient, EscrowError, EscrowStatus,
use delego_marketplace::MarketplaceError;
use delego_permissions::{
    PermissionError, PermissionStatus, PermissionsContract, PermissionsContractClient,
    RelayedSpendMessage,
};
use delego_reputation::{
    ReputationConfig, ReputationContract, ReputationContractClient, ReputationError,
    TransactionOutcome,
};
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{
    contractimpl, contracttype, testutils::Address as _, testutils::Ledger as _, xdr::ToXdr,
    Address, BytesN, Env, Symbol, Vec,
};

/// Deterministic test keypair plus its raw ed25519 public key bytes, mirroring
/// `permissions::integration_tests::test_keypair`.
fn test_keypair(env: &Env, seed: u8) -> (SigningKey, BytesN<32>) {
    let signing_key = SigningKey::from_bytes(&[seed; 32]);
    let public_key = BytesN::from_array(env, &signing_key.verifying_key().to_bytes());
    (signing_key, public_key)
}

/// Sign a `RelayedSpendMessage` over its canonical XDR encoding — the exact
/// bytes `execute_spend_via_relayer` re-derives and verifies.
fn sign_relayed_spend(
    env: &Env,
    signing_key: &SigningKey,
    message: RelayedSpendMessage,
) -> BytesN<64> {
    let message_bytes = message.to_xdr(env);
    let len = message_bytes.len() as usize;
    let mut buf = [0u8; 512];
    message_bytes.copy_into_slice(&mut buf[..len]);
    let signature = signing_key.sign(&buf[..len]);
    BytesN::from_array(env, &signature.to_bytes())
}

struct TestEnv {
    env: Env,
    _admin: Address,
    buyer: Address,
    seller: Address,
    agent: Address,
    token_contract_id: Address,
    escrow_contract_id: Address,
    permissions_contract_id: Address,
}

impl TestEnv {
    fn setup() -> Self {
        let env = Env::default();
        env.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&env);
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let agent = Address::generate(&env);
        let treasury = Address::generate(&env);

        let token_admin = Address::generate(&env);
        #[allow(deprecated)]
        let token_contract_id = env.register_stellar_asset_contract(token_admin.clone());
        let token_admin_client =
            soroban_sdk::token::StellarAssetClient::new(&env, &token_contract_id);
        token_admin_client.mint(&buyer, &10000);

        let fee_bps = 0u32; // 0% for tests
        let min_amount = 100i128;
        let max_amount = 10000i128;
        let config = EscrowConfig {
            admin: admin.clone(),
            fee_bps,
            treasury,
            min_amount,
            max_amount,
        };
        let escrow_contract_id = env.register(EscrowContract, (config,));
        let permissions_contract_id = env.register(PermissionsContract, ());

        let escrow_client = EscrowContractClient::new(&env, &escrow_contract_id);
        escrow_client.add_token(&admin, &token_contract_id);

        TestEnv {
            env,
            _admin: admin,
            buyer,
            seller,
            agent,
            token_contract_id,
            escrow_contract_id,
            permissions_contract_id,
        }
    }

    fn order_id(&self) -> BytesN<32> {
        BytesN::from_array(&self.env, &[1u8; 32])
    }
}
// ------------------------------------------------------------------------
// Mock reputation contract used by Marketplace::get_merchant_view tests.
// ------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
enum MockReputationState {
    Score(ExternalReputationScore),
    Missing,
    Revert,
}

struct MockReputationContract;

#[contractimpl]
impl MockReputationContract {
    pub fn set_state(env: Env, state: MockReputationState) {
        env.storage().instance().set(&Symbol::new(&env, "state"), &state);
    }

    pub fn get_score(env: Env, _entity: Address) -> Option<ExternalReputationScore> {
        mock_reputation_score(env)
    }

    pub fn get_external_score(env: Env, _entity: Address) -> Option<ExternalReputationScore> {
        mock_reputation_score(env)
    }

    pub fn get_reputation_score(env: Env, _entity: Address) -> Option<ExternalReputationScore> {
        mock_reputation_score(env)
    }
}

fn mock_reputation_score(env: Env) -> Option<ExternalReputationScore> {
    let state: MockReputationState = env
        .storage()
        .instance()
        .get(&Symbol::new(&env, "state"))
        .unwrap_or(MockReputationState::Missing);
    match state {
        MockReputationState::Score(score) => Some(score),
        MockReputationState::Missing => None,
        MockReputationState::Revert => panic!("mock reputation contract reverted"),
    }
}

fn setup_marketplace_with_mock_reputation(
    env: &Env,
    admin: &Address,
) -> (MarketplaceContractClient, Address) {
    let mock_id = env.register(MockReputationContract, ());
    let marketplace_id = env.register(MarketplaceContract, ());
    let marketplace_client = MarketplaceContractClient::new(env, &marketplace_id);
    marketplace_client.set_reputation_source(admin, &mock_id);
    (marketplace_client, mock_id)
}

#[test]
fn test_marketplace_reputation_resolved() {
    let t = TestEnv::setup();
    let (marketplace, mock_id) = setup_marketplace_with_mock_reputation(&t.env, &t._admin);
    let mock = MockReputationContractClient::new(&t.env, &mock_id);

    let expected = ExternalReputationScore {
        entity: t.seller.clone(),
        score: 9876,
        total_transactions: 42,
        successful_transactions: 40,
        disputed_transactions: 1,
        avg_rating: 9500,
        last_updated: 123456,
    };
    mock.set_state(MockReputationState::Score(expected.clone()));

    let view = marketplace.get_merchant_view(&t.seller);
    assert_eq!(view.reputation_score, Some(expected.clone()));
    assert_eq!(
        view.reputation_resolution,
        ReputationResolution::Resolved(expected)
    );
}

#[test]
fn test_marketplace_reputation_missing_entity() {
    let t = TestEnv::setup();
    let (marketplace, mock_id) = setup_marketplace_with_mock_reputation(&t.env, &t._admin);
    let mock = MockReputationContractClient::new(&t.env, &mock_id);
    mock.set_state(MockReputationState::Missing);

    let view = marketplace.get_merchant_view(&t.seller);
    assert_eq!(view.reputation_score, None);
    assert_eq!(view.reputation_resolution, ReputationResolution::NotFound);
}

#[test]
fn test_marketplace_reputation_source_revert_is_swallowed() {
    let t = TestEnv::setup();
    let (marketplace, mock_id) = setup_marketplace_with_mock_reputation(&t.env, &t._admin);
    let mock = MockReputationContractClient::new(&t.env, &mock_id);
    mock.set_state(MockReputationState::Revert);

    let view = marketplace.get_merchant_view(&t.seller);
    assert_eq!(view.reputation_score, None);
    assert_eq!(view.reputation_resolution, ReputationResolution::Unavailable);
}

/// Simulates a delegated purchase: agent executes spend via permissions, then buyer deposits.
fn delegated_deposit(t: &TestEnv, amount: i128, timeout_ledgers: u32) -> u64 {
    let perm_client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    perm_client.execute_spend(&t.buyer, &t.agent, &amount, &t.seller);
    escrow_client.deposit(
        &t.buyer,
        &t.seller,
        &t.token_contract_id,
        &amount,
        &t.order_id(),
        &timeout_ledgers,
        &None,
        &None,
    )
}

/// Attempts a delegated purchase: checks permissions and executes spend, then buyer deposits into escrow.
fn try_delegated_deposit(
    t: &TestEnv,
    amount: i128,
    timeout_ledgers: u32,
) -> Result<u64, PermissionError> {
    let perm_client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    match perm_client.try_execute_spend(&t.buyer, &t.agent, &amount, &t.seller) {
        Ok(Ok(())) => Ok(escrow_client.deposit(
            &t.buyer,
            &t.seller,
            &t.token_contract_id,
            &amount,
            &t.order_id(),
            &timeout_ledgers,
            &None,
            &None,
        )),
        Err(Ok(e)) => Err(e),
        _ => Err(PermissionError::Unauthorized),
    }
}

#[test]
fn test_permission_checked_before_escrow_fund_fails_without_permission() {
    let t = TestEnv::setup();
    let perm_client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    // Agent tries to spend without a granted permission.
    let result = perm_client.try_execute_spend(&t.buyer, &t.agent, &200, &t.seller);
    assert_eq!(
        result,
        Err(Ok(delego_permissions::PermissionError::PermissionNotFound))
    );
}

#[test]
fn test_permission_checked_before_escrow_fund_fails_exceeding_limit() {
    let t = TestEnv::setup();
    let perm_client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    let limit_total = 1000i128;
    let limit_per_tx = 500i128;
    let ttl_ledgers = 36000u32;
    let merchants = Vec::<soroban_sdk::Address>::new(&t.env);

    perm_client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    // Exceeds per-tx limit of 500.
    assert_eq!(
        perm_client.try_execute_spend(&t.buyer, &t.agent, &600, &t.seller),
        Err(Ok(delego_permissions::PermissionError::ExceedsPerTxLimit))
    );
}

#[test]
fn test_permission_checked_before_escrow_fund_succeeds() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let perm_client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    let limit_total = 1000i128;
    let limit_per_tx = 500i128;
    let ttl_ledgers = 36000u32;
    let merchants = Vec::<soroban_sdk::Address>::new(&t.env);

    perm_client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    let escrow_id = delegated_deposit(&t, 400, 3600);

    assert_eq!(escrow_id, 1);
    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.amount, 400);
}

#[test]
fn test_end_to_end_delegated_purchase() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let perm_client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    let limit_total = 1000i128;
    let limit_per_tx = 500i128;
    let ttl_ledgers = 36000u32;
    let mut merchants = Vec::<soroban_sdk::Address>::new(&t.env);
    merchants.push_back(t.seller.clone());

    perm_client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    let escrow_id = delegated_deposit(&t, 400, 3600);

    assert_eq!(token_client.balance(&t.buyer), 9600);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 400);
    assert_eq!(token_client.balance(&t.seller), 0);

    escrow_client.release(&escrow_id, &t.buyer, &t.seller);

    assert_eq!(token_client.balance(&t.buyer), 9600);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 0);
    assert_eq!(token_client.balance(&t.seller), 400);

    let record = escrow_client.get_escrow(&escrow_id);
    assert!(matches!(record.status, EscrowStatus::Released));
}

/// Simulates the recommended v1 escrow/reputation integration (issue #18):
/// an authorized backend indexer observes the escrow's release and reports
/// the outcome to the reputation contract, rather than escrow calling
/// reputation directly.
#[test]
fn test_reputation_recorded_after_escrow_release() {
    let t = TestEnv::setup();
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let perm_client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    let reputation_admin = Address::generate(&t.env);
    let reputation_contract_id = t.env.register(
        ReputationContract,
        (
            reputation_admin.clone(),
            ReputationConfig {
                decay_window_seconds: 90 * 24 * 60 * 60,
                min_transactions_threshold: 1,
                dispute_penalty_bps: 500,
                freeze_threshold_flags: 3,
            },
        ),
    );
    let reputation_client = ReputationContractClient::new(&t.env, &reputation_contract_id);

    let limit_total = 1000i128;
    let limit_per_tx = 500i128;
    let ttl_ledgers = 36000u32;
    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(t.seller.clone());
    perm_client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    let escrow_id = delegated_deposit(&t, 400, 3600);
    escrow_client.release(&escrow_id, &t.buyer, &t.seller);

    let released = escrow_client.get_escrow(&escrow_id);
    assert!(matches!(released.status, EscrowStatus::Released));

    // Backend indexer relays the observed outcome to the reputation contract.
    reputation_client.record_transaction(
        &reputation_admin,
        &escrow_id,
        &t.seller,
        &t.buyer,
        &released.amount,
        &TransactionOutcome::Released,
    );

    let reputation = reputation_client.get_reputation(&t.seller);
    assert_eq!(reputation.total_transactions, 1);
    assert_eq!(reputation.successful_transactions, 1);
    assert_eq!(reputation.score, 10_000);

    // The buyer, as counterparty, may now rate the seller for this escrow.
    reputation_client.rate_entity(&t.buyer, &escrow_id, &t.seller, &9500u32);
    let breakdown = reputation_client.get_reputation_breakdown(&t.seller, &0u32, &10u32);
    assert_eq!(breakdown.get(0).unwrap().rating, Some(9500u32));
}

#[test]
fn test_permission_revoked_after_spend_before_escrow_deposit_fails() {
    let t = TestEnv::setup();
    let perm_client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    // Arrange: Grant permission with sufficient allowance.
    let limit_total = 1000i128;
    let limit_per_tx = 500i128;
    let ttl_ledgers = 36000u32;
    let merchants = Vec::<Address>::new(&t.env);

    perm_client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    // Act 1: Agent executes spend within limits.
    perm_client.execute_spend(&t.buyer, &t.agent, &300, &t.seller);
    let perm_record = perm_client.get_permission(&t.buyer, &t.agent);
    assert_eq!(perm_record.spent, 300);

    // Act 2: Owner revokes the permission before escrow deposit.
    perm_client.revoke(&t.buyer, &t.agent);

    // Assert: Permission is no longer active and subsequent spend/deposit fails.
    assert!(!perm_client.is_active(&t.buyer, &t.agent));
    let revoked_record = perm_client.get_permission(&t.buyer, &t.agent);
    assert_eq!(revoked_record.status, PermissionStatus::Revoked);

    let spend_result = perm_client.try_execute_spend(&t.buyer, &t.agent, &200, &t.seller);
    assert_eq!(spend_result, Err(Ok(PermissionError::Unauthorized)));

    let deposit_result = try_delegated_deposit(&t, 200, 3600);
    assert_eq!(deposit_result, Err(PermissionError::Unauthorized));

    assert!(escrow_client.try_get_escrow(&1).is_err());
}

#[test]
fn test_permission_paused_during_flow() {
    let t = TestEnv::setup();
    let perm_client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    // Arrange: Grant permission and execute first spend + deposit successfully.
    let limit_total = 1000i128;
    let limit_per_tx = 500i128;
    let ttl_ledgers = 36000u32;
    let merchants = Vec::<Address>::new(&t.env);

    perm_client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    let escrow_id_1 = delegated_deposit(&t, 200, 3600);
    assert_eq!(escrow_id_1, 1);
    let perm_1 = perm_client.get_permission(&t.buyer, &t.agent);
    assert_eq!(perm_1.spent, 200);

    // Act 1: Pause the permission mid-flow.
    perm_client.pause(&t.buyer, &t.agent);

    // Assert 1: Permission is paused and subsequent spends fail.
    assert!(!perm_client.is_active(&t.buyer, &t.agent));
    let paused_record = perm_client.get_permission(&t.buyer, &t.agent);
    assert_eq!(paused_record.status, PermissionStatus::Paused);

    let spend_result = perm_client.try_execute_spend(&t.buyer, &t.agent, &200, &t.seller);
    assert_eq!(spend_result, Err(Ok(PermissionError::PermissionPaused)));

    let deposit_result = try_delegated_deposit(&t, 200, 3600);
    assert_eq!(deposit_result, Err(PermissionError::PermissionPaused));

    // Act 2: Resume the permission.
    perm_client.resume(&t.buyer, &t.agent);

    // Assert 2: Permission is active again and spend/deposit succeeds.
    assert!(perm_client.is_active(&t.buyer, &t.agent));
    let resumed_record = perm_client.get_permission(&t.buyer, &t.agent);
    assert_eq!(resumed_record.status, PermissionStatus::Active);

    let escrow_id_2 = delegated_deposit(&t, 200, 3600);
    assert_eq!(escrow_id_2, 2);
    let escrow_record_2 = escrow_client.get_escrow(&escrow_id_2);
    assert_eq!(escrow_record_2.amount, 200);

    let perm_final = perm_client.get_permission(&t.buyer, &t.agent);
    assert_eq!(perm_final.spent, 400);
}

#[test]
fn test_merchant_restriction_enforcement() {
    let t = TestEnv::setup();
    let perm_client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);

    // Arrange: Whitelist only seller A.
    let seller_a = t.seller.clone();
    let seller_b = Address::generate(&t.env);

    let limit_total = 1000i128;
    let limit_per_tx = 500i128;
    let ttl_ledgers = 36000u32;
    let mut merchants = Vec::<Address>::new(&t.env);
    merchants.push_back(seller_a.clone());

    perm_client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    // Act & Assert 1: Attempt spend/deposit to unwhitelisted seller B fails.
    let spend_result = perm_client.try_execute_spend(&t.buyer, &t.agent, &200, &seller_b);
    assert_eq!(spend_result, Err(Ok(PermissionError::MerchantNotAllowed)));

    let can_spend_result = perm_client.try_can_spend(&t.buyer, &t.agent, &200, &seller_b);
    assert_eq!(
        can_spend_result,
        Err(Ok(PermissionError::MerchantNotAllowed))
    );

    // Act & Assert 2: Deposit to allowed seller A succeeds.
    let escrow_id = delegated_deposit(&t, 200, 3600);
    assert_eq!(escrow_id, 1);

    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.amount, 200);
    assert_eq!(record.seller, seller_a);

    let perm = perm_client.get_permission(&t.buyer, &t.agent);
    assert_eq!(perm.spent, 200);
}

#[test]
fn test_batch_operations_across_contracts() {
    let t = TestEnv::setup();
    let perm_client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    // Arrange: Grant permission with high total limit.
    let limit_total = 5000i128;
    let limit_per_tx = 2000i128;
    let ttl_ledgers = 36000u32;
    let merchants = Vec::<Address>::new(&t.env);

    perm_client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    let amounts = [300i128, 400i128, 500i128];
    let total_spent: i128 = 1200;

    // Act 1: Execute multiple spends across contracts.
    for &amount in amounts.iter() {
        perm_client.execute_spend(&t.buyer, &t.agent, &amount, &t.seller);
    }

    // Verify cumulative allowance is correctly decremented.
    let perm = perm_client.get_permission(&t.buyer, &t.agent);
    assert_eq!(perm.spent, total_spent);
    assert_eq!(perm.limit_total - perm.spent, 3800);

    // Act 2: Use batch_deposit to create several escrows corresponding to the spends.
    let mut orders = Vec::new(&t.env);
    for (i, &amount) in amounts.iter().enumerate() {
        let order_id = BytesN::from_array(&t.env, &[(i as u8) + 1; 32]);
        orders.push_back(BatchDepositParams {
            seller: t.seller.clone(),
            token: t.token_contract_id.clone(),
            amount,
            order_id,
            timeout_ledgers: 3600,
            order_hash: None,
            schema: None,
        });
    }

    let escrow_ids = escrow_client.batch_deposit(&t.buyer, &orders);

    // Assert: Verify all escrows were created with correct amounts and balances updated.
    assert_eq!(escrow_ids.len(), 3);
    assert_eq!(escrow_ids.get(0).unwrap(), 1);
    assert_eq!(escrow_ids.get(1).unwrap(), 2);
    assert_eq!(escrow_ids.get(2).unwrap(), 3);

    assert_eq!(escrow_client.get_escrow(&1).amount, 300);
    assert_eq!(escrow_client.get_escrow(&2).amount, 400);
    assert_eq!(escrow_client.get_escrow(&3).amount, 500);

    assert_eq!(token_client.balance(&t.buyer), 10000 - total_spent);
    assert_eq!(token_client.balance(&t.escrow_contract_id), total_spent);
}

#[test]
fn test_partial_release_reduces_escrow_state_correctly() {
    let t = TestEnv::setup();
    let perm_client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let escrow_client = EscrowContractClient::new(&t.env, &t.escrow_contract_id);
    let token_client = soroban_sdk::token::Client::new(&t.env, &t.token_contract_id);

    // Arrange: Grant permission and deposit 500 into escrow.
    let limit_total = 1000i128;
    let limit_per_tx = 1000i128;
    let ttl_ledgers = 36000u32;
    let merchants = Vec::<Address>::new(&t.env);

    perm_client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    let escrow_id = delegated_deposit(&t, 500, 3600);
    assert_eq!(escrow_id, 1);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 500);
    assert_eq!(token_client.balance(&t.seller), 0);

    // Act: Partial-release 200 to the seller.
    let result = escrow_client.partial_release(&escrow_id, &t.buyer, &200);

    // Assert: PartialReleaseResult reflects the partial release.
    assert_eq!(result.released, 200);
    assert_eq!(result.remaining, 300);
    assert!(!result.fully_released);

    // Verify the stored escrow record shows released_amount = 200, remaining balance = 300, and active status.
    let record = escrow_client.get_escrow(&escrow_id);
    assert_eq!(record.released_amount, 200);
    assert_eq!(record.amount - record.released_amount, 300);
    assert_eq!(record.status, EscrowStatus::Funded);

    // Verify token transfers: seller received 200, escrow contract retains 300.
    assert_eq!(token_client.balance(&t.seller), 200);
    assert_eq!(token_client.balance(&t.escrow_contract_id), 300);
}

#[test]
fn test_permission_expiry_between_grant_and_spend() {
    let t = TestEnv::setup();
    let perm_client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    // Arrange: Grant permission with a very short TTL.
    let limit_total = 1000i128;
    let limit_per_tx = 500i128;
    let ttl_ledgers = 10u32;
    let merchants = Vec::<Address>::new(&t.env);

    perm_client.grant(
        &t.buyer,
        &t.agent,
        &limit_total,
        &limit_per_tx,
        &merchants,
        &ttl_ledgers,
    );

    // Permission is active before expiration.
    assert!(perm_client.is_active(&t.buyer, &t.agent));
    assert_eq!(
        perm_client.try_can_spend(&t.buyer, &t.agent, &200, &t.seller),
        Ok(Ok(()))
    );

    // Act: Advance the ledger sequence past the expiry.
    let current_seq = t.env.ledger().sequence();
    t.env.ledger().with_mut(|li| {
        li.sequence_number = current_seq + ttl_ledgers + 1;
    });

    // Assert: Permission is no longer active, and spend attempt fails with Expired.
    assert!(!perm_client.is_active(&t.buyer, &t.agent));

    let spend_result = perm_client.try_execute_spend(&t.buyer, &t.agent, &200, &t.seller);
    assert_eq!(spend_result, Err(Ok(PermissionError::Expired)));

    let can_spend_result = perm_client.try_can_spend(&t.buyer, &t.agent, &200, &t.seller);
    assert_eq!(can_spend_result, Err(Ok(PermissionError::Expired)));

    let deposit_result = try_delegated_deposit(&t, 200, 3600);
    assert_eq!(deposit_result, Err(PermissionError::Expired));
}

// ------------------------------------------------------------------------
// Security-control coverage: velocity, relayer replay, and multi-owner
// quorum, wired into the same cross-contract suite as the escrow/permission
// happy paths above. Whitelist gating end-to-end is already covered by
// `test_merchant_restriction_enforcement`.
// ------------------------------------------------------------------------

#[test]
fn test_second_velocity_spend_fails() {
    let t = TestEnv::setup();
    let perm_client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);

    let merchants = Vec::<Address>::new(&t.env);
    perm_client.grant(&t.buyer, &t.agent, &1000, &500, &merchants, &36000u32);

    perm_client.set_admin(&t._admin);
    perm_client.set_velocity_limit(&t._admin, &50u32);

    // First spend succeeds and records the current ledger for velocity tracking.
    perm_client.execute_spend(&t.buyer, &t.agent, &100, &t.seller);

    // A second spend before the configured interval has elapsed is rejected.
    let second = perm_client.try_execute_spend(&t.buyer, &t.agent, &100, &t.seller);
    assert_eq!(second, Err(Ok(PermissionError::VelocityLimitExceeded)));

    // Advancing past the interval allows spending again.
    let current_seq = t.env.ledger().sequence();
    t.env.ledger().with_mut(|li| {
        li.sequence_number = current_seq + 51;
    });
    perm_client.execute_spend(&t.buyer, &t.agent, &100, &t.seller);
    assert_eq!(perm_client.get_permission(&t.buyer, &t.agent).spent, 200);
}

#[test]
fn test_relayed_spend_replay_with_old_nonce_reverts() {
    let t = TestEnv::setup();
    let perm_client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let relayer = Address::generate(&t.env);

    let merchants = Vec::<Address>::new(&t.env);
    perm_client.grant(&t.buyer, &t.agent, &1000, &500, &merchants, &36000u32);

    let (signing_key, public_key) = test_keypair(&t.env, 7);
    perm_client.set_relayer_key(&t.agent, &public_key);

    let expiration_ledger = t.env.ledger().sequence() + 1000;
    let message = RelayedSpendMessage {
        owner: t.buyer.clone(),
        delegate: t.agent.clone(),
        merchant: t.seller.clone(),
        amount: 100,
        nonce: 0,
        expiration_ledger,
    };
    let signature = sign_relayed_spend(&t.env, &signing_key, message);

    perm_client.execute_spend_via_relayer(
        &relayer,
        &t.buyer,
        &t.agent,
        &100,
        &t.seller,
        &0u64,
        &expiration_ledger,
        &signature,
    );
    assert_eq!(perm_client.get_relayer_nonce(&t.buyer, &t.agent), 1);

    // Replaying the same signed message (nonce 0 again) is rejected — the
    // delegate's expected nonce has already advanced to 1.
    let replay = perm_client.try_execute_spend_via_relayer(
        &relayer,
        &t.buyer,
        &t.agent,
        &100,
        &t.seller,
        &0u64,
        &expiration_ledger,
        &signature,
    );
    assert_eq!(replay, Err(Ok(PermissionError::InvalidNonce)));
}

#[test]
fn test_multi_owner_spend_enforces_quorum() {
    let t = TestEnv::setup();
    let perm_client = PermissionsContractClient::new(&t.env, &t.permissions_contract_id);
    let owner_b = Address::generate(&t.env);
    let owner_c = Address::generate(&t.env);

    let mut owners = Vec::<Address>::new(&t.env);
    owners.push_back(t.buyer.clone());
    owners.push_back(owner_b.clone());
    owners.push_back(owner_c.clone());
    let merchants = Vec::<Address>::new(&t.env);

    perm_client.grant_multi_owner(
        &t.buyer, &owners, &t.agent, &1000, &500, &merchants, &36000u32, &2u32,
    );

    // A single signer cannot satisfy a 2-of-3 threshold.
    let mut one_signer = Vec::<Address>::new(&t.env);
    one_signer.push_back(t.buyer.clone());
    let under_quorum =
        perm_client.try_execute_spend_multi(&t.buyer, &t.agent, &one_signer, &100, &t.seller);
    assert_eq!(
        under_quorum,
        Err(Ok(PermissionError::InsufficientSignatures))
    );

    // Two of the three registered owners satisfies the threshold.
    let mut two_signers = Vec::<Address>::new(&t.env);
    two_signers.push_back(t.buyer.clone());
    two_signers.push_back(owner_b.clone());
    perm_client.execute_spend_multi(&t.buyer, &t.agent, &two_signers, &100, &t.seller);

    let record = perm_client.get_multi_permission(&t.buyer, &t.agent);
    assert_eq!(record.spent, 100);
}

/// Cross-contract error-code allocation.
///
/// The bridge-facing error space is a single numeric `u32` code space. To make
/// numeric codes unambiguous, every code belongs to at most one contract's
/// error enum.
///
/// | Contract      | Error enum          |
/// |---------------|---------------------|
/// | Escrow        | `EscrowError`       |
/// | Permissions   | `PermissionError`   |
/// | Reputation    | `ReputationError`   |
/// | Delegation    | `DelegationError`   |
/// | Marketplace   | `MarketplaceError`  |
///
/// The test below verifies this uniqueness property for all codes accepted by
/// each contract's `TryFrom<u32>` implementation.
fn collect_error_codes<T>() -> Vec<u32>
where
    T: TryFrom<u32>,
{
    (0..=u16::MAX as u32)
        .filter(|&code| T::try_from(code).is_ok())
        .collect()
}

#[test]
fn test_error_codes_are_unique_across_contracts() {
    let error_codes = [
        ("escrow", collect_error_codes::<EscrowError>()),
        ("permissions", collect_error_codes::<PermissionError>()),
        ("reputation", collect_error_codes::<ReputationError>()),
        ("delegation", collect_error_codes::<DelegationError>()),
        ("marketplace", collect_error_codes::<MarketplaceError>()),
    ];

    let mut seen = std::collections::BTreeSet::new();
    for (contract_name, codes) in error_codes {
        assert!(
            !codes.is_empty(),
            "{contract_name} must expose at least one error code"
        );
        for code in codes {
            assert!(
                seen.insert(code),
                "error code {code} is shared with another contract"
            );
        }
    }
}
