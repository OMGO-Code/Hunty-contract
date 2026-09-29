//! Integration tests for the paged leaderboard scan
//! (`get_hunt_leaderboard_window`) and the registration-count read path that
//! backs it.
//!
//! Issue #1041: a page must cost `O(window_size)` storage reads instead of
//! loading every registered player and slicing in memory, while preserving the
//! paging contract (absolute `index` values, `next_index`, `finished`).

use hunty_core::{HuntyCore, HuntyCoreClient};
use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{Address, Env, String};

const START_TS: u64 = 1_700_000_000;
/// Mirrors `MAX_LEADERBOARD_SCAN_SIZE` in `hunty-core/src/lib.rs`.
const MAX_SCAN_SIZE: u32 = 200;
/// Mirrors `MAX_LEADERBOARD_SIZE` in `hunty-core/src/lib.rs`.
const MAX_LEADERBOARD_SIZE: u32 = 20;

/// Registers the contract and creates an active hunt with two required clues
/// (answers `"a1"` and `"a2"`, 10 points each). Returns the client, creator and
/// hunt id.
fn setup_active_hunt(env: &Env) -> (HuntyCoreClient<'_>, Address, u64) {
    let core_id = env.register(HuntyCore, ());
    let client = HuntyCoreClient::new(env, &core_id);
    let creator = Address::generate(env);

    let hunt_id = client.create_hunt(
        &creator,
        &String::from_str(env, "Paging Hunt"),
        &String::from_str(env, "Hunt used by the leaderboard window tests"),
        &None,
        &None,
        &0u32,
        &None,
        &None,
    );

    client.add_clue(
        &hunt_id,
        &String::from_str(env, "Question one"),
        &String::from_str(env, "a1"),
        &10u32,
        &true,
        &None,
        &None,
    );
    client.add_clue(
        &hunt_id,
        &String::from_str(env, "Question two"),
        &String::from_str(env, "a2"),
        &10u32,
        &true,
        &None,
        &None,
    );

    client.activate_hunt(&hunt_id, &creator);

    (client, creator, hunt_id)
}

/// Registers `count` players and lets the first `solving` of them solve the
/// first clue, so the window has both scored and untouched rows to return.
/// Returns the players in registration order.
fn register_players(
    env: &Env,
    client: &HuntyCoreClient,
    hunt_id: u64,
    count: u32,
    solving: u32,
) -> std::vec::Vec<Address> {
    let mut players = std::vec::Vec::new();

    for i in 0..count {
        let player = Address::generate(env);
        client.register_player(&hunt_id, &player);

        if i < solving {
            client.submit_answer(
                &hunt_id,
                &1u32,
                &player,
                &String::from_str(env, "a1"),
                &(u64::from(i) + 1),
                &(START_TS + 1),
            );
        }

        players.push(player);
    }

    players
}

#[test]
fn window_pages_through_every_registered_player() {
    let env = Env::default();
    env.ledger().set_timestamp(START_TS);
    env.mock_all_auths();

    let (client, _creator, hunt_id) = setup_active_hunt(&env);
    let players = register_players(&env, &client, hunt_id, 5, 3);

    let board = client.get_hunt_leaderboard(&hunt_id, &MAX_LEADERBOARD_SIZE);
    assert_eq!(board.total_players, 5);
    // `truncated` describes the ranked index, not the returned page: only the
    // three players that scored are ranked, so it is set even though the board
    // itself fits within `limit` rows.
    assert!(board.truncated);

    // Walk the registration index two rows at a time and stitch the pages back
    // together, exactly as an off-chain caller would.
    let mut rows: std::vec::Vec<(u32, Address, u32, bool)> = std::vec::Vec::new();
    let mut start = 0u32;
    let mut pages = 0u32;

    loop {
        let page = client.get_hunt_leaderboard_window(&hunt_id, &start, &2u32, &None);
        assert_eq!(page.queried_at, START_TS);

        for offset in 0..page.entries.len() {
            let row = page.entries.get(offset).unwrap();
            rows.push((row.index, row.player.clone(), row.score, row.is_completed));
        }

        pages += 1;

        if page.finished {
            assert_eq!(page.next_index, 5);
            break;
        }

        // A non-final page is exactly one window wide and advances `next_index`
        // past the rows it returned.
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.next_index, start + page.entries.len());

        start = page.next_index;
        assert!(pages < 10, "paging should terminate");
    }

    assert_eq!(pages, 3);
    assert_eq!(rows.len(), players.len() as usize);

    for (position, (index, player, score, is_completed)) in rows.iter().enumerate() {
        // Slice offsets must be translated back into absolute registration
        // indices, otherwise clients cannot merge pages deterministically.
        assert_eq!(*index, position as u32);
        assert_eq!(*player, players[position]);

        let progress = client.get_player_progress(&hunt_id, player);
        assert_eq!(*score, progress.total_score);
        assert_eq!(*is_completed, progress.is_completed);
    }

    // The first three players solved a clue; the remaining two never submitted.
    // Every solver submitted the same clue at the same ledger timestamp, so all
    // three earn the same (positive) score, while non-submitters stay at zero.
    let scored = rows[0].2;
    assert!(scored > 0);
    assert_eq!(rows[1].2, scored);
    assert_eq!(rows[2].2, scored);
    assert_eq!(rows[3].2, 0);
    assert_eq!(rows[4].2, 0);
}

#[test]
fn window_of_hunt_without_players_is_empty_and_finished() {
    let env = Env::default();
    env.ledger().set_timestamp(START_TS);
    env.mock_all_auths();

    let (client, _creator, hunt_id) = setup_active_hunt(&env);

    // The player total comes from the registration counter, which starts at
    // zero on a fresh hunt.
    let board = client.get_hunt_leaderboard(&hunt_id, &MAX_LEADERBOARD_SIZE);
    assert_eq!(board.total_players, 0);
    assert!(board.entries.is_empty());
    assert!(!board.truncated);

    let page = client.get_hunt_leaderboard_window(&hunt_id, &0u32, &10u32, &None);
    assert!(page.entries.is_empty());
    assert_eq!(page.next_index, 0);
    assert!(page.finished);
}

#[test]
fn start_index_beyond_registrations_returns_empty_final_page() {
    let env = Env::default();
    env.ledger().set_timestamp(START_TS);
    env.mock_all_auths();

    let (client, _creator, hunt_id) = setup_active_hunt(&env);
    register_players(&env, &client, hunt_id, 3, 0);

    let page = client.get_hunt_leaderboard_window(&hunt_id, &50u32, &10u32, &None);

    assert!(page.entries.is_empty());
    // `start_index` is clamped to the registration count so paging can never
    // run off the end of the index.
    assert_eq!(page.next_index, 3);
    assert!(page.finished);
}

#[test]
fn window_size_is_capped_at_scan_limit() {
    let env = Env::default();
    env.ledger().set_timestamp(START_TS);
    env.mock_all_auths();

    let (client, _creator, hunt_id) = setup_active_hunt(&env);
    let total = MAX_SCAN_SIZE + 5;
    register_players(&env, &client, hunt_id, total, 0);

    // A full-width page reads two ledger entries per row (registration slot +
    // player progress), which is above the 400-entry per-transaction footprint
    // budget that mainnet enforces by default. Enforcing it would panic inside
    // the host before the page could be inspected, so the clamp itself is
    // checked here with enforcement turned off; the enforced budget is covered
    // by `half_width_page_fits_enforced_resource_limits` below.
    env.cost_estimate().disable_resource_limits();

    let board = client.get_hunt_leaderboard(&hunt_id, &MAX_LEADERBOARD_SIZE);
    assert_eq!(board.total_players, total);
    assert!(board.truncated);

    // An oversized window is capped, so a single call stays within the bounded
    // scan limit instead of reading the whole index.
    let first = client.get_hunt_leaderboard_window(&hunt_id, &0u32, &u32::MAX, &None);
    assert_eq!(first.entries.len(), MAX_SCAN_SIZE);
    assert_eq!(first.next_index, MAX_SCAN_SIZE);
    assert!(!first.finished);

    let second = client.get_hunt_leaderboard_window(&hunt_id, &first.next_index, &u32::MAX, &None);
    assert_eq!(second.entries.len(), 5);
    assert_eq!(second.next_index, total);
    assert!(second.finished);
}

#[test]
fn half_width_page_fits_enforced_resource_limits() {
    let env = Env::default();
    env.ledger().set_timestamp(START_TS);
    env.mock_all_auths();

    let (client, _creator, hunt_id) = setup_active_hunt(&env);
    register_players(&env, &client, hunt_id, MAX_SCAN_SIZE + 5, 0);

    // Resource limits are enforced here: the SDK applies the mainnet limits by
    // default. A page costs roughly `window_size * 2` footprint entries, so a
    // half-width page stays inside the 400-entry per-transaction budget while
    // a full-width one does not. Callers paging near the cap should therefore
    // request narrower windows instead of relying on the clamp alone.
    let page = client.get_hunt_leaderboard_window(&hunt_id, &0u32, &(MAX_SCAN_SIZE / 2), &None);

    assert_eq!(page.entries.len(), MAX_SCAN_SIZE / 2);
    assert_eq!(page.next_index, MAX_SCAN_SIZE / 2);
    assert!(!page.finished);
}

#[test]
fn zero_window_size_returns_empty_page_without_advancing() {
    let env = Env::default();
    env.ledger().set_timestamp(START_TS);
    env.mock_all_auths();

    let (client, _creator, hunt_id) = setup_active_hunt(&env);
    register_players(&env, &client, hunt_id, 2, 0);

    // A zero-width window is a caller error: the page is empty and
    // `next_index` does not move, so callers must request at least one row.
    let page = client.get_hunt_leaderboard_window(&hunt_id, &0u32, &0u32, &None);
    assert!(page.entries.is_empty());
    assert_eq!(page.next_index, 0);
    assert!(!page.finished);
}

#[test]
fn window_for_unknown_hunt_fails() {
    let env = Env::default();
    env.ledger().set_timestamp(START_TS);
    env.mock_all_auths();

    let (client, _creator, hunt_id) = setup_active_hunt(&env);

    assert!(client
        .try_get_hunt_leaderboard_window(&(hunt_id + 1), &0u32, &10u32, &None)
        .is_err());
}
