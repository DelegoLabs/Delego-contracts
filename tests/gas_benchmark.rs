#![cfg(test)]

use delego_escrow::{EscrowConfig, EscrowContract, EscrowContractClient, Milestone, AdminAction};
use soroban_sdk::{testutils::Address as _, token, Address, BytesN, Env, Symbol, Vec};

fn milestone(env: &Env, id: u32, amount: i128, label: &str) -> Milestone {
    Milestone {
        milestone_id: id,
        amount,
        description: Symbol::new(env, label),
        is_completed: false,
        completed_at: 0,
    }
}

#[test]
fn gas_benchmark() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    let token = env.register_stellar_asset_contract_v2(Address::generate(&env)).address();
    let sac = token::StellarAssetClient::new(&env, &token);
    sac.mint(&buyer, &10_000i128);

    let config = EscrowConfig { admin: admin.clone(), fee_bps: 0, treasury: admin.clone(), min_amount: 1, max_amount: 1_000_000 };
    let escrow_id = env.register(EscrowContract, (config,));
    let client = EscrowContractClient::new(&env, &escrow_id);
    client.add_token(&admin, &token);
    // approve queued action if needed - the test setup above shows it auto-unlocks
    // but if it fails due to lock, try to handle
    let _ = client.queue_admin_action(&admin, &AdminAction::AddToken(token.clone()));
    let id = client.queue_admin_action(&admin, &AdminAction::AddToken(token.clone()));
    // simple: just call add again may work if already added? or execute queued
    // but easier to skip if already exists

    let milestones = Vec::from_array(&env, [milestone(&env, 1, 300, "m1"), milestone(&env, 2, 400, "m2"), milestone(&env, 3, 300, "m3")]);

    env.cost_estimate().budget().reset_default();
    let _id = client.create_milestone_escrow(&buyer, &seller, &token, &BytesN::from_array(&env, &[7; 32]), &1000u32, &milestones);
    let budget = env.cost_estimate().budget();
    let create_cpu = budget.cpu_instruction_cost();
    let create_mem = budget.memory_bytes_cost();

    let create_le_cpu: u64 = 15_000_000;
    let create_le_mem: u64 = 3_000_000;
    assert!(create_cpu <= create_le_cpu + (create_le_cpu / 10));
    assert!(create_mem <= create_le_mem + (create_le_mem / 10));
}
