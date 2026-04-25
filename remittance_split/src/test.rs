#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as AddressTrait, Events},
    token::StellarAssetClient,
    Address, Env, IntoVal, Symbol, TryFromVal,
};

#[test]
fn test_distribution_completed_event() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);

    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_contract = env.register_stellar_asset_contract_v2(token_admin);
    let token_addr = token_contract.address();
    let stellar_client = StellarAssetClient::new(&env, &token_addr);

    // 1. Initialize split
    // percentages: 40, 30, 20, 10
    client.initialize_split(&owner, &0, &token_addr, &40, &30, &20, &10);

    // 2. Setup destination accounts
    let accounts = AccountGroup {
        spending: Address::generate(&env),
        savings: Address::generate(&env),
        bills: Address::generate(&env),
        insurance: Address::generate(&env),
    };

    // 3. Mint tokens to owner
    let total_amount = 1000i128;
    stellar_client.mint(&owner, &total_amount);

    // 4. Distribute
    let nonce = 1u64; // nonce 0 used in initialize_split
    let deadline = env.ledger().timestamp() + 3600;
    let request_hash = RemittanceSplit::compute_request_hash(
        symbol_short!("distrib"),
        owner.clone(),
        nonce,
        total_amount,
        deadline,
    );

    client.distribute_usdc(
        &token_addr,
        &owner,
        &nonce,
        &deadline,
        &request_hash,
        &accounts,
        &total_amount,
    );

    // 5. Verify events
    let events = env.events().all();

    // We expect several events:
    // - init (from initialize_split)
    // - dist_ok (unstructured)
    // - dist_comp (structured) - THIS IS THE ONE WE CARE ABOUT

    let last_event = events.last().expect("No events emitted");
    let (_contract_id, topics, data) = last_event;

    // Verify topic schema
    assert_eq!(
        topics.get(0).unwrap(),
        symbol_short!("Remitwise").into_val(&env)
    );
    assert_eq!(topics.get(1).unwrap(), (0u32).into_val(&env)); // Category: Transaction
    assert_eq!(topics.get(2).unwrap(), (1u32).into_val(&env)); // Priority: Medium
    assert_eq!(
        topics.get(3).unwrap(),
        symbol_short!("dist_comp").into_val(&env)
    );

    // Verify structured payload
    let event: DistributionCompletedEvent = DistributionCompletedEvent::try_from_val(&env, &data)
        .expect("Failed to parse DistributionCompletedEvent data");

    assert_eq!(event.from, owner);
    assert_eq!(event.total_amount, total_amount);
    assert_eq!(event.spending_amount, 400); // 40% of 1000
    assert_eq!(event.savings_amount, 300); // 30% of 1000
    assert_eq!(event.bills_amount, 200); // 20% of 1000
    assert_eq!(event.insurance_amount, 100); // 10% of 1000 handled by remainder
    assert_eq!(event.timestamp, env.ledger().timestamp());
}

#[test]
fn test_distribution_event_topic_correctness() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);

    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_contract = env.register_stellar_asset_contract_v2(token_admin);
    let token_addr = token_contract.address();
    let stellar_client = StellarAssetClient::new(&env, &token_addr);

    client.initialize_split(&owner, &0, &token_addr, &50, &50, &0, &0);

    let accounts = AccountGroup {
        spending: Address::generate(&env),
        savings: Address::generate(&env),
        bills: Address::generate(&env),
        insurance: Address::generate(&env),
    };

    stellar_client.mint(&owner, &100);

    let nonce = 1u64;
    let deadline = env.ledger().timestamp() + 3600;
    let request_hash = RemittanceSplit::compute_request_hash(
        symbol_short!("distrib"),
        owner.clone(),
        nonce,
        100,
        deadline,
    );

    client.distribute_usdc(
        &token_addr,
        &owner,
        &nonce,
        &deadline,
        &request_hash,
        &accounts,
        &100,
    );

    let events = env.events().all();
    let dist_comp_event = events
        .iter()
        .find(|e| {
            let topics = &e.1;
            topics.len() == 4 && topics.get(3).unwrap() == symbol_short!("dist_comp").into_val(&env)
        })
        .expect("DistributionCompleted event not found");

    let topics = &dist_comp_event.1;
    assert_eq!(
        topics.get(0).unwrap(),
        symbol_short!("Remitwise").into_val(&env)
    );
    assert_eq!(topics.get(1).unwrap(), (0u32).into_val(&env)); // Transaction
    assert_eq!(topics.get(2).unwrap(), (1u32).into_val(&env)); // Medium
}

// ---------------------------------------------------------------------------
// Helpers shared by pagination tests
// ---------------------------------------------------------------------------

/// Register the contract, initialize split, and return (client, owner, token_addr).
fn setup_contract(env: &Env) -> (RemittanceSplitClient, Address, Address) {
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(env, &contract_id);
    let owner = Address::generate(env);
    let token_admin = Address::generate(env);
    let token_contract = env.register_stellar_asset_contract_v2(token_admin);
    let token_addr = token_contract.address();
    client.initialize_split(&owner, &0, &token_addr, &50, &30, &15, &5);
    (client, owner, token_addr)
}

/// Create `n` schedules for `owner` (side-effect only; IDs are discarded by callers).
fn create_schedules(env: &Env, client: &RemittanceSplitClient, owner: &Address, n: u32) {
    let base_time = env.ledger().timestamp();
    for i in 0..n {
        client.create_remittance_schedule(
            owner,
            &1000i128,
            &(base_time + 3600 + i as u64),
            &0u64,
        );
    }
}

// ---------------------------------------------------------------------------
// Pagination tests
// ---------------------------------------------------------------------------

/// Page 1 of N returns correct items and a valid next_cursor.
#[test]
fn test_page_first_of_many_has_next_cursor() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, owner, _) = setup_contract(&env);
    create_schedules(&env, &client, &owner, 5);

    let page = client.get_remittance_schedules_page(&owner, &0, &2);
    assert_eq!(page.count, 2);
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.next_cursor, Some(2));
}

/// Last page returns next_cursor: None.
#[test]
fn test_page_last_page_has_no_next_cursor() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, owner, _) = setup_contract(&env);
    create_schedules(&env, &client, &owner, 4);

    // Page starting at index 2 with limit 2 exhausts the list exactly.
    let page = client.get_remittance_schedules_page(&owner, &2, &2);
    assert_eq!(page.count, 2);
    assert_eq!(page.next_cursor, None);
}

/// cursor beyond the list length returns empty page with next_cursor: None.
#[test]
fn test_page_cursor_out_of_range_returns_empty() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, owner, _) = setup_contract(&env);
    create_schedules(&env, &client, &owner, 3);

    let page = client.get_remittance_schedules_page(&owner, &100, &10);
    assert_eq!(page.count, 0);
    assert_eq!(page.items.len(), 0);
    assert_eq!(page.next_cursor, None);
}

/// limit clamped to MAX_PAGE_LIMIT when caller passes an oversized value.
#[test]
fn test_page_limit_clamped_to_max() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, owner, _) = setup_contract(&env);
    // Create fewer schedules than MAX_PAGE_LIMIT so we can verify clamping doesn't panic.
    create_schedules(&env, &client, &owner, 3);

    // Passing u32::MAX should not panic and should return at most 3 items.
    let page = client.get_remittance_schedules_page(&owner, &0, &u32::MAX);
    assert_eq!(page.count, 3);
    assert_eq!(page.next_cursor, None);
}

/// Single-item list paginates correctly.
#[test]
fn test_page_single_item_list() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, owner, _) = setup_contract(&env);
    create_schedules(&env, &client, &owner, 1);

    let page = client.get_remittance_schedules_page(&owner, &0, &10);
    assert_eq!(page.count, 1);
    assert_eq!(page.next_cursor, None);
}

/// Empty owner (no schedules) returns empty page.
#[test]
fn test_page_empty_owner_returns_empty() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, owner, _) = setup_contract(&env);
    // No schedules created.
    let page = client.get_remittance_schedules_page(&owner, &0, &10);
    assert_eq!(page.count, 0);
    assert_eq!(page.items.len(), 0);
    assert_eq!(page.next_cursor, None);
}
