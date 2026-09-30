//! Regression tests for #1018 — every submission must be recorded exactly once.
//!
//! `submit_answer_with_hash` used to push `current_time` into
//! `progress.recent_submissions` during validation *and* pass
//! `record_failed_submission = true` to `finalize_answer_submission`, which
//! pushed the same timestamp again. On an incorrect answer the submission was
//! therefore recorded twice, and the per-minute rate limit counted two entries
//! for one submission, halving the effective limit.
//!
//! The duplicate write is gone after #1017, which removed the
//! `record_failed_submission` parameter. What is left to pin down is the
//! invariant itself: there is now a single authoritative recording point,
//! `record_submission_for_rate_limit`, and these tests prove each submission
//! adds exactly one timestamp no matter which entrypoint accepted it or whether
//! the answer was right or wrong.
//!
//! The bug is not reproducible by reverting #1017 alone (the flag no longer
//! exists), so rather than a synthetic revert these tests assert the observable
//! invariant directly: the window grows by exactly one per submission, and the
//! limit trips at exactly the configured submission count.
//!
//! Each entrypoint call runs in its own `env.as_contract` frame — the mocked auth
//! ledger keys entries per frame, so re-authorizing the same address for the
//! same function twice inside one frame trips `Error(Auth, ExistingValue)`.

use crate::errors::HuntErrorCode;
use crate::types::PlayerProgress;
use crate::{storage::Storage, HuntyCore};
use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{Address, Bytes, BytesN, Env, String};

const T0: u64 = 1_700_000_000;

/// Runs a single contract entrypoint inside its own contract/auth frame.
fn step<T>(env: &Env, contract_id: &Address, f: impl FnOnce(&Env) -> T) -> T {
    env.as_contract(contract_id, || f(env))
}

/// Builds an Active, single-clue hunt whose answer is `"right"`.
fn active_hunt(
    env: &Env,
    cid: &Address,
    creator: &Address,
    max_submissions_per_minute: u32,
) -> (u64, u32) {
    let hunt_id = step(env, cid, |env| {
        HuntyCore::create_hunt(
            env.clone(),
            creator.clone(),
            String::from_str(env, "Recording count hunt"),
            String::from_str(env, "Desc"),
            None,
            None,
            max_submissions_per_minute,
            None,
            None,
        )
        .unwrap()
    });

    step(env, cid, |env| {
        HuntyCore::set_max_attempts_per_clue(env.clone(), hunt_id, creator.clone(), 100, 0).unwrap()
    });

    let clue_id = step(env, cid, |env| {
        HuntyCore::add_clue(
            env.clone(),
            hunt_id,
            String::from_str(env, "Q"),
            String::from_str(env, "right"),
            10,
            true,
            None,
            None,
        )
        .unwrap()
    });

    step(env, cid, |env| {
        HuntyCore::activate_hunt(env.clone(), hunt_id, creator.clone()).unwrap()
    });

    (hunt_id, clue_id)
}

fn register(env: &Env, cid: &Address, hunt_id: u64, player: &Address) {
    step(env, cid, |env| {
        HuntyCore::register_player(env.clone(), hunt_id, player.clone()).unwrap()
    });
}

fn progress(env: &Env, cid: &Address, hunt_id: u64, player: &Address) -> PlayerProgress {
    step(env, cid, |env| {
        Storage::get_player_progress(env, hunt_id, player).unwrap()
    })
}

fn window_len(env: &Env, cid: &Address, hunt_id: u64, player: &Address) -> u32 {
    progress(env, cid, hunt_id, player).recent_submissions.len()
}

fn wrong_answer(env: &Env) -> String {
    String::from_str(env, "wrong")
}

fn wrong_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[9u8; 32])
}

// ---------------------------------------------------------------------------
// submit_answer records once
// ---------------------------------------------------------------------------

#[test]
fn submit_answer_records_exactly_one_timestamp_per_wrong_answer() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    // Limit high enough that it never trips, so the window keeps growing and
    // each growth step is directly observable.
    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, 50);
    register(&env, &cid, hunt_id, &player);

    for i in 0..5u64 {
        // Stay inside the 60s window so nothing prunes and the count is exact.
        let at = T0 + i * 10;
        env.ledger().set_timestamp(at);
        assert_eq!(
            step(&env, &cid, |env| {
                HuntyCore::submit_answer(
                    env.clone(),
                    hunt_id,
                    clue_id,
                    player.clone(),
                    wrong_answer(env),
                    i + 1,
                    at,
                )
            }),
            Ok(false)
        );
        assert_eq!(
            window_len(&env, &cid, hunt_id, &player),
            i as u32 + 1,
            "submission {} should add exactly one timestamp",
            i + 1
        );
    }
}

#[test]
fn submit_answer_rate_limit_trips_at_the_configured_submission_count() {
    const LIMIT: u32 = 4;

    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, LIMIT);
    register(&env, &cid, hunt_id, &player);

    for nonce in 1..=LIMIT as u64 {
        assert_eq!(
            step(&env, &cid, |env| {
                HuntyCore::submit_answer(
                    env.clone(),
                    hunt_id,
                    clue_id,
                    player.clone(),
                    wrong_answer(env),
                    nonce,
                    T0,
                )
            }),
            Ok(false)
        );
    }

    // Exactly LIMIT timestamps: one per submission. A second write per
    // submission would show LIMIT * 2 here and trip the limit at LIMIT / 2.
    assert_eq!(window_len(&env, &cid, hunt_id, &player), LIMIT);

    let err = step(&env, &cid, |env| {
        HuntyCore::submit_answer(
            env.clone(),
            hunt_id,
            clue_id,
            player.clone(),
            wrong_answer(env),
            LIMIT as u64 + 1,
            T0,
        )
        .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::RateLimitExceeded);
}

// ---------------------------------------------------------------------------
// submit_answer_with_hash records once
// ---------------------------------------------------------------------------

#[test]
fn submit_answer_with_hash_records_exactly_one_timestamp_per_wrong_answer() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, 50);
    register(&env, &cid, hunt_id, &player);

    let bad = wrong_hash(&env);

    for i in 0..5u64 {
        // Stay inside the 60s window so nothing prunes and the count is exact.
        let at = T0 + i * 10;
        env.ledger().set_timestamp(at);
        assert_eq!(
            step(&env, &cid, |env| {
                HuntyCore::submit_answer_with_hash(
                    env.clone(),
                    hunt_id,
                    clue_id,
                    player.clone(),
                    bad.clone(),
                    i + 1,
                    at,
                )
            }),
            Ok(false)
        );
        // This is the assertion that fails when the hash path records twice.
        assert_eq!(
            window_len(&env, &cid, hunt_id, &player),
            i as u32 + 1,
            "hash submission {} should add exactly one timestamp",
            i + 1
        );
    }
}

#[test]
fn submit_answer_with_hash_rate_limit_trips_at_the_configured_count() {
    const LIMIT: u32 = 4;

    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, LIMIT);
    register(&env, &cid, hunt_id, &player);

    let bad = wrong_hash(&env);

    for nonce in 1..=LIMIT as u64 {
        assert_eq!(
            step(&env, &cid, |env| {
                HuntyCore::submit_answer_with_hash(
                    env.clone(),
                    hunt_id,
                    clue_id,
                    player.clone(),
                    bad.clone(),
                    nonce,
                    T0,
                )
            }),
            Ok(false)
        );
    }

    // Before the fix this was LIMIT * 2 and the 3rd submission already failed.
    assert_eq!(window_len(&env, &cid, hunt_id, &player), LIMIT);

    let err = step(&env, &cid, |env| {
        HuntyCore::submit_answer_with_hash(
            env.clone(),
            hunt_id,
            clue_id,
            player.clone(),
            bad,
            LIMIT as u64 + 1,
            T0,
        )
        .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::RateLimitExceeded);
}

#[test]
fn both_submission_paths_agree_on_the_same_number_of_wrong_answers() {
    const LIMIT: u32 = 3;

    // Two identical hunts, one driven through submit_answer and one through
    // submit_answer_with_hash. Both must end up with exactly LIMIT timestamps.
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_a, clue_a) = active_hunt(&env, &cid, &creator, LIMIT);
    let (hunt_b, clue_b) = active_hunt(&env, &cid, &creator, LIMIT);
    register(&env, &cid, hunt_a, &player);
    register(&env, &cid, hunt_b, &player);

    let bad = wrong_hash(&env);

    for nonce in 1..=LIMIT as u64 {
        assert_eq!(
            step(&env, &cid, |env| {
                HuntyCore::submit_answer(
                    env.clone(),
                    hunt_a,
                    clue_a,
                    player.clone(),
                    wrong_answer(env),
                    nonce,
                    T0,
                )
            }),
            Ok(false)
        );
        assert_eq!(
            step(&env, &cid, |env| {
                HuntyCore::submit_answer_with_hash(
                    env.clone(),
                    hunt_b,
                    clue_b,
                    player.clone(),
                    bad.clone(),
                    nonce,
                    T0,
                )
            }),
            Ok(false)
        );
    }

    let len_a = window_len(&env, &cid, hunt_a, &player);
    let len_b = window_len(&env, &cid, hunt_b, &player);
    assert_eq!(len_a, LIMIT);
    assert_eq!(len_b, LIMIT);
    assert_eq!(
        len_a, len_b,
        "both paths must record the same number of submissions"
    );
}

// ---------------------------------------------------------------------------
// A correct answer is also recorded exactly once
// ---------------------------------------------------------------------------

#[test]
fn a_correct_answer_is_recorded_once_then_clears_the_window() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, 50);
    register(&env, &cid, hunt_id, &player);

    // One wrong answer first, so the window is non-empty.
    assert_eq!(
        step(&env, &cid, |env| {
            HuntyCore::submit_answer(
                env.clone(),
                hunt_id,
                clue_id,
                player.clone(),
                wrong_answer(env),
                1,
                T0,
            )
        }),
        Ok(false)
    );
    assert_eq!(window_len(&env, &cid, hunt_id, &player), 1);

    // The correct answer is recorded once too, then the window is cleared on
    // success, so it ends at zero rather than two.
    assert_eq!(
        step(&env, &cid, |env| {
            HuntyCore::submit_answer(
                env.clone(),
                hunt_id,
                clue_id,
                player.clone(),
                String::from_str(env, "right"),
                2,
                T0,
            )
        }),
        Ok(true)
    );
    assert_eq!(window_len(&env, &cid, hunt_id, &player), 0);
    assert!(progress(&env, &cid, hunt_id, &player)
        .completed_clues
        .contains(clue_id));
}

// ---------------------------------------------------------------------------
// Unlimited and disabled tracking
// ---------------------------------------------------------------------------

#[test]
fn unlimited_sentinel_records_nothing() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    // 0 is the documented "unlimited" sentinel for max_submissions_per_minute.
    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, 0);
    register(&env, &cid, hunt_id, &player);

    for i in 0..5u64 {
        let at = T0 + i;
        env.ledger().set_timestamp(at);
        assert_eq!(
            step(&env, &cid, |env| {
                HuntyCore::submit_answer(
                    env.clone(),
                    hunt_id,
                    clue_id,
                    player.clone(),
                    wrong_answer(env),
                    i + 1,
                    at,
                )
            }),
            Ok(false)
        );
    }

    // Nothing was recorded, so nothing can be double-counted either.
    assert_eq!(window_len(&env, &cid, hunt_id, &player), 0);
}

// ---------------------------------------------------------------------------
// preview_answer shares the same recording point
// ---------------------------------------------------------------------------

#[test]
fn preview_answer_records_exactly_one_timestamp_per_call() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, 50);
    register(&env, &cid, hunt_id, &player);

    for i in 0..4u64 {
        // Stay inside the 60s window so nothing prunes and the count is exact.
        let at = T0 + i * 10;
        env.ledger().set_timestamp(at);
        assert_eq!(
            step(&env, &cid, |env| {
                HuntyCore::preview_answer(
                    env.clone(),
                    hunt_id,
                    clue_id,
                    player.clone(),
                    wrong_answer(env),
                )
            }),
            Ok(false)
        );
        assert_eq!(window_len(&env, &cid, hunt_id, &player), i as u32 + 1);
    }
}

#[test]
fn preview_answer_and_submit_answer_share_one_window() {
    const LIMIT: u32 = 2;

    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, LIMIT);
    register(&env, &cid, hunt_id, &player);

    // One preview, one submit: the window holds both, so the next submission is
    // refused. They draw on the same budget rather than tracking separately.
    assert_eq!(
        step(&env, &cid, |env| {
            HuntyCore::preview_answer(
                env.clone(),
                hunt_id,
                clue_id,
                player.clone(),
                wrong_answer(env),
            )
        }),
        Ok(false)
    );
    assert_eq!(window_len(&env, &cid, hunt_id, &player), 1);

    assert_eq!(
        step(&env, &cid, |env| {
            HuntyCore::submit_answer(
                env.clone(),
                hunt_id,
                clue_id,
                player.clone(),
                wrong_answer(env),
                1,
                T0,
            )
        }),
        Ok(false)
    );
    assert_eq!(window_len(&env, &cid, hunt_id, &player), 2);

    let err = step(&env, &cid, |env| {
        HuntyCore::submit_answer(
            env.clone(),
            hunt_id,
            clue_id,
            player.clone(),
            wrong_answer(env),
            2,
            T0,
        )
        .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::RateLimitExceeded);
}
