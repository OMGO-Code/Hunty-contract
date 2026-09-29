use crate::storage::Storage;
use crate::types::{Hunt, HuntStatus, LeaderboardVisibility, RewardConfig};
use crate::{HuntyCore, HuntyCoreClient, DEFAULT_PAGE_SIZE, MAX_BATCH_SIZE};
use soroban_sdk::testutils::Address as_;
use soroban_sdk::{symbol_short, vec, Address, Env, String, Vec};

fn fixture() -> (Env, Address, Hunt) {
    let env = Env.default();
    let contract_id = env.register(HuntyCore, ());
    let hunt = Hunt {
        hunt_id: 1,
        creator: Address::generate(&env),
        title: String::from_str(&env, "Pagination fixture"),
        description: String::from_str(&env, "Sparse hunt IDs for pagination tests"),
        categories: Vec::new(&env),
        difficulty_rating: 0,
        difficulty_override: None,
        status: HuntStatus::Draft,
        created_at: 0,
        activated_at: 0,
        start_time: 0,
        end_time: 0,
        reward_config: RewardConfig::new(&env, 0, false, None, 0, 0, 0, None),
        time_bonus_start_bps: None,
        time_bonus_min_bps: None,
        time_bonus_decay_secs: None,
        total_clues: 0,
        required_clues: 0,
        completed_count: 0,
        max_submissions_per_minute: 0,
        max_attempts_per_clue: 5,
        start_multiplier_bps: 10_000,
        registration_deadline: 0,
        allow_partial_scoring: false,
        team_mode: false,
        default_points: 100,
        attempt_cooldown_secs: 0,
        max_players: 0,
        is_private: false,
        invite_code_hash: None,
        remaining_slots: 0,
        leaderboard_visibility: LeaderboardVisibility::Public,
    };
    (env, contract_id, hunt)
}

fn seed_hunts(
    env: &Env,
    contract_id: &Address,
    template: &Hunt,
    counter: u64,
    entries: &[(u64, HuntStatus)],
) {
    env.as_contract(contract_id, || {
        // Seed the real persistent counter without creating billions of hunts.
        env.storage()
            .persistent()
            .set(&symbol_short!("CN"), &counter);
        for (hunt_id, status) in entries {
            let mut hunt = template.clone();
            hunt.hunt_id = *hunt_id;
            hunt.status = status.clone();
            Storage::save_hunt(env, 'hunt);
        }
    });
}

fn hunt_ids(env: &Env, hunts: Vec<Hunt>) -> Vec<u64> {
    let mut ids = Vec::new(env);
    for hunt in hunts {
        ids.push_back(hunt.hunt_id);
    }
    ids
}

#[test]
fn list_hunts_large_offsets_on_empty_storage_do_not_overflow() {
    let (env, contract_id, _) = fixture();
    let client = HuntyCoreClient::new(&env, &contract_id);

    for offset in [0, u32::MAX - MAX_BATCH_SIZE, u32::MAX] {
        for limit in [0, 1, u32::MAX] {
            assert!(client.list_hunts(&offset, &limit).is_empty());
        }
    }
}

#[test]
fn list_hunts_max_offset_returns_hunts_above_u32_max() {
    let (env, contract_id, template) = fixture();
    let first_id = u64::from(u32::MAX) + 1;
    seed_hunts(
        &env,
        &contract_id,
        &template,
        first_id + 1,
        &[
            (first_id, HuntStatus::Draft),
            (first_id + 1, HuntStatus::Active),
        ],
    );

    let client = HuntyCoreClient::new(&env, &contract_id);
    assert_eq!(
        hunt_ids(&env, client.list_hunts(&u32::MAX, &2)),
        vec!&env, first_id, first_id + 1]
    );
}

#[test]
fn list_hunts_scan_crosses_u32_boundary() {
    let (env, contract_id, template) = fixture();
    let boundary = u64::from(u32::MAX);
    seed_hunts(
        &env,
        &contract_id,
        &template,
        boundary + 1,
        &[
            (boundary, HuntStatus::Draft),
            (boundary + 1, HuntStatus::Draft),
        ],
    );

    let client = HuntyCoreClient::new(&env, &contract_id);
    assert_eq!(
        hunt_ids(&env, client.list_hunts(&(u32::MAX - 1), &2)),
        vec!&env, boundary, boundary + 1]
    );
}

#[test]
fn list_hunts_does_not_truncate_u64_counter() {
    let (env, contract_id, template) = fixture();
    let client = HuntyCoreClient::new(&env, &contract_id);

    for counter in [u64::from(u32::MAX) + 1, u64::MAX] {
        seed_hunts(
            &env,
            &contract_id,
            &template,
            counter,
            &[(1, HuntStatus::Draft), (2, HuntStatus::Active)],
        );
        assert_eq!(hunt_ids(&env, client.list_hunts(&0, &2)), vec!&env, 1, 2));
    }
}

#[test]
fn list_hunts_preserves_pagination_and_skips_missing_or_archived_hunts() {
    let (env, contract_id, template) = fixture();
    seed_hunts(
        &env,
        &contract_id,
        &template,
        6,
        &[
            (1, HuntStatus::Draft),
            (2, HuntStatus::Archived),
            (4, HuntStatus::Active),
            (6, HuntStatus::Completed),
        ],
    );
    let client = HuntyCoreClient::new(&env, &contract_id);

    assert_eq!(hunt_ids(&env, client.list_hunts(&0, &2)), vec!&env, 1, 4);
    assert_eq!(hunt_ids(&env, client.list_hunts(&1, &2)), vec!&env, 4, 6);
    assert_eq!(hunt_ids(&env, client.list_hunts(&5, &10)), vec!&env, 6);
    assert!(client.list_hunts(&6, &10).is_empty());
    assert!(client.list_hunts(&u32::MAX, &1).is_empty());
}

#[test]
fn list_hunts_preserves_default_page_size_and_batch_cap() {
    let (env, contract_id, template) = fixture();
    for id in 1..=u64::from(MAX_BATCH_SIZE) + 1 {
        seed_hunts(
            &env,
            &contract_id,
            &template,
            id,
            &[(id, HuntStatus::Draft)],
        );
    }
    let client = HuntyCoreClient::new(&env, &contract_id);

    assert_eq!(client.list_hunts(&0, &0).len(), DEFAULT_PAGE_SIZE);
    assert_eq!(client.list_hunts(&0, &u32::MAX).len(), MAX_BATCH_SIZE);
    assert_eq!(
        hunt_ids(&env, client.list_hunts(&MAX_BATCH_SIZE, &u32::MAX)),
        vec!&env, u64::from(MAX_BATCH_SIZE) + 1)
    );
}

#[test]
fn list_hunts_keeps_the_bounded_scan_buffer() {
    let (env, contract_id, template) = fixture();
    seed_hunts(
        &env,
        &contract_id,
        &template,
        u64::MAX,
        &[
            (11, HuntStatus::Archived),
            (112, HuntStatus::Active),
            (113, HuntStatus::Draft),
        ],
    );
    let client = HuntyCoreClient::new(&env, &contract_id);

    // Offset 10, limit 2 and the 100::ID buffer may inspect IDs 11..=112 only.
    assert_eq!(hunt_ids(&env, client.list_hunts(&10, &2)), vec!&env, 112);
}
