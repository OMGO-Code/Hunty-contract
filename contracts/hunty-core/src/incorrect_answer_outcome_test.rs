//! Regression tests for #1017 — a wrong answer must not be reported as an error.
//!
//! `finalize_answer_submission` used to record the failed attempt, save the
//! player progress, publish `AnswerIncorrect` and then return
//! `Err(InvalidAnswer)`. A Soroban invocation that returns an error rolls back
//! every storage write and every event it made, so all of that state was
//! discarded: `recent_submissions`, `clue_last_attempts`, the processed-submission
//! nonce and the event itself. A player could therefore brute-force answers with
//! no rate limit, no cooldown and no attempt cap ever being reached.
//!
//! The fix returns `Ok(false)` for an incorrect answer and `Ok(true)` for a
//! correct one, so the failed-attempt state is committed. These tests pin that
//! behaviour down, and in particular prove the N-th attempt hits
//! `RateLimitExceeded` instead of the window being reset by every wrong guess.
//!
//! Each entrypoint call runs in its own `env.as_contract` frame: the mocked auth
//! ledger keys entries per frame, so re-authorizing the same address for the
//! same function twice inside one frame trips `Error(Auth, ExistingValue)`.

use crate::errors::HuntErrorCode;
use crate::types::AnswerIncorrectEvent;
use crate::{storage::Storage, types::HuntStatus, HuntyCore};
use soroban_sdk::testutils::{Address as _, Events as _, Ledger as _};
use soroban_sdk::{vec, Address, Bytes, BytesN, Env, IntoVal, String, Symbol, Val, Vec};

const T0: u64 = 1_700_000_000;

/// Runs a single contract entrypoint inside its own contract/auth frame.
fn step<T>(env: &Env, contract_id: &Address, f: impl FnOnce(&Env) -> T) -> T {
    env.as_contract(contract_id, || f(env))
}

/// Builds an Active, single-clue hunt whose answer is `"right"`.
///
/// `max_submissions_per_minute` is the per-minute rate limit under test and
/// `max_attempts_per_clue` is the per-clue attempt cap.
fn active_hunt(
    env: &Env,
    cid: &Address,
    creator: &Address,
    max_submissions_per_minute: u32,
    max_attempts_per_clue: u32,
) -> (u64, u32) {
    active_hunt_with_cooldown(
        env,
        cid,
        creator,
        max_submissions_per_minute,
        max_attempts_per_clue,
        0,
    )
}

/// As `active_hunt`, but also sets a per-clue attempt cooldown. Both
/// `set_max_attempts_per_clue` and the attempt cap are Draft-only, so the
/// cooldown must be configured before activation.
fn active_hunt_with_cooldown(
    env: &Env,
    cid: &Address,
    creator: &Address,
    max_submissions_per_minute: u32,
    max_attempts_per_clue: u32,
    attempt_cooldown_secs: u32,
) -> (u64, u32) {
    let hunt_id = step(env, cid, |env| {
        HuntyCore::create_hunt(
            env.clone(),
            creator.clone(),
            String::from_str(env, "Wrong answer outcome hunt"),
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
        HuntyCore::set_max_attempts_per_clue(
            env.clone(),
            hunt_id,
            creator.clone(),
            max_attempts_per_clue,
            attempt_cooldown_secs,
        )
        .unwrap()
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

fn progress(
    env: &Env,
    cid: &Address,
    hunt_id: u64,
    player: &Address,
) -> crate::types::PlayerProgress {
    step(env, cid, |env| {
        Storage::get_player_progress(env, hunt_id, player).unwrap()
    })
}

fn wrong_answer(env: &Env) -> String {
    String::from_str(env, "wrong")
}

// ---------------------------------------------------------------------------
// Core outcome semantics
// ---------------------------------------------------------------------------

#[test]
fn incorrect_answer_returns_ok_false_and_commits_the_attempt() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    // Rate limit left generous so the rate limiter is not what rejects here;
    // the attempt cap is the constraint under test.
    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, 0, 5);
    register(&env, &cid, hunt_id, &player);

    let result = step(&env, &cid, |env| {
        HuntyCore::submit_answer(
            env.clone(),
            hunt_id,
            clue_id,
            player.clone(),
            wrong_answer(env),
            1,
            T0,
        )
    });

    // The wrong answer resolves to Ok(false) rather than Err(InvalidAnswer).
    assert_eq!(result, Ok(false));

    // The failed attempt was committed, not rolled back.
    assert_eq!(
        step(&env, &cid, |env| {
            Storage::get_clue_attempt_count(env, hunt_id, clue_id, &player)
        }),
        1
    );

    // ...and no successful-answer state was granted.
    let p = progress(&env, &cid, hunt_id, &player);
    assert!(!p.completed_clues.contains(clue_id));
    assert!(!p.is_completed);
    assert_eq!(p.total_score, 0);
}

#[test]
fn correct_answer_returns_ok_true_and_is_unchanged() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, 0, 5);
    register(&env, &cid, hunt_id, &player);

    let result = step(&env, &cid, |env| {
        HuntyCore::submit_answer(
            env.clone(),
            hunt_id,
            clue_id,
            player.clone(),
            String::from_str(env, "right"),
            1,
            T0,
        )
    });
    assert_eq!(result, Ok(true));

    let p = progress(&env, &cid, hunt_id, &player);
    assert!(p.completed_clues.contains(clue_id));
    assert!(p.is_completed);
    assert!(p.total_score > 0);
}

// ---------------------------------------------------------------------------
// The regression proper: the rate limit must actually bite
// ---------------------------------------------------------------------------

#[test]
fn wrong_answers_hit_the_rate_limit_and_never_roll_back() {
    const LIMIT: u32 = 3;

    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, LIMIT, 100);
    register(&env, &cid, hunt_id, &player);

    // Each of the first LIMIT submissions is a wrong answer that resolves.
    for nonce in 1..=LIMIT {
        let result = step(&env, &cid, |env| {
            HuntyCore::submit_answer(
                env.clone(),
                hunt_id,
                clue_id,
                player.clone(),
                wrong_answer(env),
                nonce as u64,
                T0,
            )
        });
        assert_eq!(
            result,
            Ok(false),
            "wrong answer {nonce} should resolve Ok(false)"
        );
    }

    // The whole window is on record, one entry per submission.
    assert_eq!(
        progress(&env, &cid, hunt_id, &player)
            .recent_submissions
            .len(),
        LIMIT
    );
    assert_eq!(
        step(&env, &cid, |env| {
            Storage::get_clue_attempt_count(env, hunt_id, clue_id, &player)
        }),
        LIMIT
    );

    // The next wrong answer is refused by the rate limiter. Before the fix this
    // submission returned Ok(false) again, because the previous three had been
    // rolled back along with their timestamps.
    let err = step(&env, &cid, |env| {
        HuntyCore::submit_answer(
            env.clone(),
            hunt_id,
            clue_id,
            player.clone(),
            wrong_answer(env),
            (LIMIT + 1) as u64,
            T0,
        )
        .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::RateLimitExceeded);

    // The refusal added no window entry and no attempt.
    assert_eq!(
        progress(&env, &cid, hunt_id, &player)
            .recent_submissions
            .len(),
        LIMIT
    );
    assert_eq!(
        step(&env, &cid, |env| {
            Storage::get_clue_attempt_count(env, hunt_id, clue_id, &player)
        }),
        LIMIT
    );
}

#[test]
fn rate_limited_wrong_answers_hold_for_the_full_window() {
    const LIMIT: u32 = 2;

    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, LIMIT, 100);
    register(&env, &cid, hunt_id, &player);

    for nonce in 1..=LIMIT {
        assert_eq!(
            step(&env, &cid, |env| {
                HuntyCore::submit_answer(
                    env.clone(),
                    hunt_id,
                    clue_id,
                    player.clone(),
                    wrong_answer(env),
                    nonce as u64,
                    T0,
                )
            }),
            Ok(false)
        );
    }

    // 59s in, the window has not rolled: still limited.
    env.ledger().set_timestamp(T0 + 59);
    let err = step(&env, &cid, |env| {
        HuntyCore::submit_answer(
            env.clone(),
            hunt_id,
            clue_id,
            player.clone(),
            wrong_answer(env),
            3,
            T0 + 59,
        )
        .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::RateLimitExceeded);

    // Past 60s the entries age out and a submission is accepted again.
    env.ledger().set_timestamp(T0 + 61);
    assert_eq!(
        step(&env, &cid, |env| {
            HuntyCore::submit_answer(
                env.clone(),
                hunt_id,
                clue_id,
                player.clone(),
                wrong_answer(env),
                4,
                T0 + 61,
            )
        }),
        Ok(false)
    );
}

// ---------------------------------------------------------------------------
// Attempt cap and cooldown
// ---------------------------------------------------------------------------

#[test]
fn attempt_cap_counts_wrong_answers() {
    const MAX_ATTEMPTS: u32 = 3;

    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    // Rate limit disabled (0) so only the attempt cap can stop the player.
    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, 0, MAX_ATTEMPTS);
    register(&env, &cid, hunt_id, &player);

    // Space the submissions out so the rate limiter is not involved at all.
    for i in 0..MAX_ATTEMPTS {
        let at = T0 + i as u64 * 61;
        env.ledger().set_timestamp(at);
        assert_eq!(
            step(&env, &cid, |env| {
                HuntyCore::submit_answer(
                    env.clone(),
                    hunt_id,
                    clue_id,
                    player.clone(),
                    wrong_answer(env),
                    i as u64 + 1,
                    at,
                )
            }),
            Ok(false)
        );
    }

    // The cap is exhausted by the wrong answers alone.
    let at = T0 + MAX_ATTEMPTS as u64 * 61;
    env.ledger().set_timestamp(at);
    let err = step(&env, &cid, |env| {
        HuntyCore::submit_answer(
            env.clone(),
            hunt_id,
            clue_id,
            player.clone(),
            wrong_answer(env),
            MAX_ATTEMPTS as u64 + 1,
            at,
        )
        .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::InvalidMaxAttempts);
}

#[test]
fn wrong_answer_persists_the_per_clue_cooldown() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt_with_cooldown(&env, &cid, &creator, 0, 100, 120);
    register(&env, &cid, hunt_id, &player);

    // First wrong answer: allowed, and records the cooldown start.
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
    assert_eq!(
        progress(&env, &cid, hunt_id, &player)
            .clue_last_attempts
            .get(clue_id),
        Some(T0)
    );

    // Second attempt inside the cooldown window is refused. This can only hold
    // if the first attempt's timestamp was committed.
    env.ledger().set_timestamp(T0 + 60);
    let err = step(&env, &cid, |env| {
        HuntyCore::submit_answer(
            env.clone(),
            hunt_id,
            clue_id,
            player.clone(),
            wrong_answer(env),
            2,
            T0 + 60,
        )
        .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::RateLimitExceeded);

    // Past the cooldown the next wrong answer is accepted again.
    env.ledger().set_timestamp(T0 + 121);
    assert_eq!(
        step(&env, &cid, |env| {
            HuntyCore::submit_answer(
                env.clone(),
                hunt_id,
                clue_id,
                player.clone(),
                wrong_answer(env),
                3,
                T0 + 121,
            )
        }),
        Ok(false)
    );
}

// ---------------------------------------------------------------------------
// Nonce and event persistence
// ---------------------------------------------------------------------------

#[test]
fn wrong_answer_persists_the_submission_nonce() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, 0, 100);
    register(&env, &cid, hunt_id, &player);

    // A wrong answer, which consumes the nonce.
    assert_eq!(
        step(&env, &cid, |env| {
            HuntyCore::submit_answer(
                env.clone(),
                hunt_id,
                clue_id,
                player.clone(),
                wrong_answer(env),
                7,
                T0,
            )
        }),
        Ok(false)
    );

    // Replaying the exact same envelope is refused. Before the fix the nonce
    // write was rolled back with the rest of the failed-attempt state, so the
    // same envelope could be replayed indefinitely.
    let err = step(&env, &cid, |env| {
        HuntyCore::submit_answer(
            env.clone(),
            hunt_id,
            clue_id,
            player.clone(),
            wrong_answer(env),
            7,
            T0,
        )
        .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::DuplicateSubmission);
}

#[test]
fn answer_incorrect_event_is_emitted_for_each_wrong_answer() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, 0, 100);
    register(&env, &cid, hunt_id, &player);

    for nonce in 1..=3u64 {
        let at = T0 + (nonce - 1) * 61;
        env.ledger().set_timestamp(at);
        // The submission and the event assertion share one contract frame:
        // `env.events().all()` reports only the current invocation, so a
        // separate frame would come back empty.
        step(&env, &cid, |env| {
            assert_eq!(
                HuntyCore::submit_answer(
                    env.clone(),
                    hunt_id,
                    clue_id,
                    player.clone(),
                    wrong_answer(env),
                    nonce,
                    at,
                ),
                Ok(false)
            );

            // Every wrong answer published exactly one AnswerIncorrect event.
            // Before the fix all three were rolled back with the invocation
            // that produced them, leaving nothing for an indexer to read.
            let expected: Vec<(Address, Vec<Val>, Val)> = vec![
                env,
                (
                    cid.clone(),
                    (Symbol::new(env, "AnswerIncorrect"), hunt_id, clue_id).into_val(env),
                    AnswerIncorrectEvent {
                        hunt_id,
                        player: player.clone(),
                        clue_id,
                        timestamp: at,
                    }
                    .into_val(env),
                ),
            ];
            assert_eq!(env.events().all(), expected);
        });
    }
}

// ---------------------------------------------------------------------------
// submit_answer_with_hash must match
// ---------------------------------------------------------------------------

#[test]
fn submit_answer_with_hash_reports_and_persists_the_same_way() {
    const LIMIT: u32 = 2;

    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, LIMIT, 100);
    register(&env, &cid, hunt_id, &player);

    let bad_hash = BytesN::from_array(&env, &[9u8; 32]);

    for nonce in 1..=LIMIT as u64 {
        assert_eq!(
            step(&env, &cid, |env| {
                HuntyCore::submit_answer_with_hash(
                    env.clone(),
                    hunt_id,
                    clue_id,
                    player.clone(),
                    bad_hash.clone(),
                    nonce,
                    T0,
                )
            }),
            Ok(false)
        );
    }

    // Same as submit_answer: the window is on record and the next attempt is
    // refused with RateLimitExceeded.
    assert_eq!(
        progress(&env, &cid, hunt_id, &player)
            .recent_submissions
            .len(),
        LIMIT
    );
    let err = step(&env, &cid, |env| {
        HuntyCore::submit_answer_with_hash(
            env.clone(),
            hunt_id,
            clue_id,
            player.clone(),
            bad_hash,
            LIMIT as u64 + 1,
            T0,
        )
        .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::RateLimitExceeded);
}

#[test]
fn submit_answer_with_hash_correct_answer_still_works() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, 0, 5);
    register(&env, &cid, hunt_id, &player);

    // Hash exactly as normalize_and_hash_answer does for the answer "right".
    let mut buf = [0u8; 256 + 12];
    buf[..8].copy_from_slice(&hunt_id.to_be_bytes());
    buf[8..12].copy_from_slice(&clue_id.to_be_bytes());
    let answer = b"right";
    buf[12..12 + answer.len()].copy_from_slice(answer);
    let good_hash = BytesN::from_array(
        &env,
        &env.crypto()
            .sha256(&Bytes::from_slice(&env, &buf[..12 + answer.len()]))
            .to_array(),
    );

    assert_eq!(
        step(&env, &cid, |env| {
            HuntyCore::submit_answer_with_hash(
                env.clone(),
                hunt_id,
                clue_id,
                player.clone(),
                good_hash,
                1,
                T0,
            )
        }),
        Ok(true)
    );
    assert!(progress(&env, &cid, hunt_id, &player)
        .completed_clues
        .contains(clue_id));
}

// ---------------------------------------------------------------------------
// preview_answer must stay consistent with the new outcome semantics
// ---------------------------------------------------------------------------

#[test]
fn preview_answer_agrees_with_submit_answer_on_a_wrong_answer() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, 0, 100);
    register(&env, &cid, hunt_id, &player);

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

    // preview_answer only previews: it must not have completed the clue.
    let p = progress(&env, &cid, hunt_id, &player);
    assert!(!p.completed_clues.contains(clue_id));
    assert_eq!(p.total_score, 0);

    // submit_answer reports the same outcome for the same wrong answer.
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
}

// ---------------------------------------------------------------------------
// Validation failures must still be errors
// ---------------------------------------------------------------------------

#[test]
fn validation_failures_still_return_err() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let stranger = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, 0, 100);

    // Unregistered player.
    let err = step(&env, &cid, |env| {
        HuntyCore::submit_answer(
            env.clone(),
            hunt_id,
            clue_id,
            stranger.clone(),
            String::from_str(env, "right"),
            1,
            T0,
        )
        .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::PlayerNotRegistered);

    // Unknown hunt.
    let err = step(&env, &cid, |env| {
        HuntyCore::submit_answer(
            env.clone(),
            hunt_id + 999,
            clue_id,
            player.clone(),
            String::from_str(env, "right"),
            1,
            T0,
        )
        .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::HuntNotFound);

    // Empty answer is still rejected before evaluation.
    register(&env, &cid, hunt_id, &player);
    let err = step(&env, &cid, |env| {
        HuntyCore::submit_answer(
            env.clone(),
            hunt_id,
            clue_id,
            player.clone(),
            String::from_str(env, " "),
            1,
            T0,
        )
        .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::InvalidAnswer);
}

#[test]
fn a_correct_answer_after_wrong_ones_still_scores_once() {
    const LIMIT: u32 = 2;

    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, LIMIT, 100);
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

    env.ledger().set_timestamp(T0 + 61);
    assert_eq!(
        step(&env, &cid, |env| {
            HuntyCore::submit_answer(
                env.clone(),
                hunt_id,
                clue_id,
                player.clone(),
                String::from_str(env, "right"),
                LIMIT as u64 + 1,
                T0 + 61,
            )
        }),
        Ok(true)
    );

    let p = progress(&env, &cid, hunt_id, &player);
    assert!(p.completed_clues.contains(clue_id));
    assert_eq!(p.completed_clues.len(), 1u32);
    // A successful answer clears the rate-limit window.
    assert_eq!(p.recent_submissions.len(), 0u32);
    // The two wrong attempts are still on record against the clue.
    assert_eq!(
        step(&env, &cid, |env| {
            Storage::get_clue_attempt_count(env, hunt_id, clue_id, &player)
        }),
        LIMIT
    );
}

#[test]
fn hunt_status_still_gates_submission() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let (hunt_id, clue_id) = active_hunt(&env, &cid, &creator, 0, 100);
    register(&env, &cid, hunt_id, &player);

    // Active: the answer resolves.
    assert_eq!(
        step(&env, &cid, |env| {
            HuntyCore::submit_answer(
                env.clone(),
                hunt_id,
                clue_id,
                player.clone(),
                String::from_str(env, "right"),
                1,
                T0,
            )
        }),
        Ok(true)
    );

    // Once deactivated, a further submission is refused. The lifecycle gate is
    // unaffected by the incorrect-answer outcome change.
    step(&env, &cid, |env| {
        HuntyCore::deactivate_hunt(env.clone(), hunt_id, creator.clone()).unwrap()
    });
    assert_eq!(
        step(&env, &cid, |env| {
            Storage::get_hunt(env, hunt_id).unwrap().status
        }),
        HuntStatus::Paused
    );

    let err = step(&env, &cid, |env| {
        HuntyCore::submit_answer(
            env.clone(),
            hunt_id,
            clue_id,
            player.clone(),
            String::from_str(env, "right"),
            2,
            T0,
        )
        .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::HuntNotActive);
}
