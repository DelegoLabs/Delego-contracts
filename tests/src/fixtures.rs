use soroban_sdk::{token, Address, Env};

#[allow(deprecated)]
pub fn create_mock_token_contract<'a>(
    env: &'a Env,
    admin: &Address,
) -> (Address, token::Client<'a>) {
    let token_address = env.register_stellar_asset_contract(admin.clone());
    let client = token::Client::new(env, &token_address);
    (token_address, client)
}
