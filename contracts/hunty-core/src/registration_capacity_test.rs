//! Regression tests for #1015 — `register_player` must honour `max_players`.
//!
//! `set_max_players` stored a cap and `register_with_invite` enforced it, but
//! `register_player` never checked it. A public hunt could therefore take
//! arbitrarily many players while its creator had configured a cap, so the cap
//! only applied to private hunts — the opposite of the useful case, since a
//! public hunt is the one that is open to anyone.
//!
//! The fix routes both paths through `complete_registration`, which enforces
//! the deadline, the capacity and the progress write in one place, so the two
//! registration paths cannot drift apart again.
//!
//! Each entrypoint call runs in its own `env.as_contract` frame — the mocked auth
//! ledger keys entries per frame, so re-authorizing the same address for the
//! same function twice inside one frame trips `Error(Auth, ExistingValue)`.

use crate::errors::HuntErrorCode;
use crate::storage::Storage;
use crate::HuntyCore;
use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{Address, Env, String};

const T0: u64 = 1_700_000_000;
const INVITE_CODE: &str = "invite-code-1234";

/// Runs a single contract entrypoint inside its own contract/auth frame.
fn step<T>(env: &Env, contract_id: &Address, f: impl FnOnce(&Env) -> T) -> T {
    env.as_contract(contract_id, || f(env))
}

/// Creates an Active public hunt with a clue and the given player cap.
fn active_public_hunt(env: &Env, cid: &Address, creator: &Address, max_players: u32) -> u64 {
    let hunt_id = step(env, cid, |env| {
        HuntyCore::create_hunt(
            env.clone(),
            creator.clone(),
            String::from_str(env, "Capacity hunt"),
            String::from_str(env, "Desc"),
            None,
            None,
            0u32,
            None,
            None,
        )
        .unwrap()
    });
    step(env, cid, |env| {
        HuntyCore::add_clue(
            env.clone(),
            hunt_id,
            String::from_str(env, "Q"),
            String::from_str(env, "a"),
            10,
            true,
            None,
            None,
        )
        .unwrap()
    });
    step(env, cid, |env| {
        HuntyCore::set_max_players(env.clone(), hunt_id, creator.clone(), max_players).unwrap()
    });
    step(env, cid, |env| {
        HuntyCore::activate_hunt(env.clone(), hunt_id, creator.clone()).unwrap()
    });
    hunt_id
}

/// Creates an Active private hunt with an invite code and the given player cap.
fn active_private_hunt(env: &Env, cid: &Address, creator: &Address, max_players: u32) -> u64 {
    let hunt_id = step(env, cid, |env| {
        HuntyCore::create_hunt(
            env.clone(),
            creator.clone(),
            String::from_str(env, "Private capacity hunt"),
            String::from_str(env, "Desc"),
            None,
            None,
            0u32,
            None,
            None,
        )
        .unwrap()
    });
    step(env, cid, |env| {
        HuntyCore::add_clue(
            env.clone(),
            hunt_id,
            String::from_str(env, "Q"),
            String::from_str(env, "a"),
            10,
            true,
            None,
            None,
        )
        .unwrap()
    });
    step(env, cid, |env| {
        HuntyCore::generate_invite_code(
            env.clone(),
            hunt_id,
            creator.clone(),
            String::from_str(env, INVITE_CODE),
        )
        .unwrap()
    });
    step(env, cid, |env| {
        HuntyCore::set_hunt_privacy(env.clone(), hunt_id, creator.clone(), true).unwrap()
    });
    step(env, cid, |env| {
        HuntyCore::set_max_players(env.clone(), hunt_id, creator.clone(), max_players).unwrap()
    });
    step(env, cid, |env| {
        HuntyCore::activate_hunt(env.clone(), hunt_id, creator.clone()).unwrap()
    });
    hunt_id
}

fn register_public(
    env: &Env,
    cid: &Address,
    hunt_id: u64,
    player: &Address,
) -> Result<(), HuntErrorCode> {
    step(env, cid, |env| {
        HuntyCore::register_player(env.clone(), hunt_id, player.clone())
    })
}

fn register_with_invite(
    env: &Env,
    cid: &Address,
    hunt_id: u64,
    player: &Address,
) -> Result<(), HuntErrorCode> {
    step(env, cid, |env| {
        HuntyCore::register_with_invite(
            env.clone(),
            hunt_id,
            player.clone(),
            String::from_str(env, INVITE_CODE),
        )
    })
}

fn player_count(env: &Env, cid: &Address, hunt_id: u64) -> u32 {
    step(env, cid, |env| Storage::get_player_count(env, hunt_id))
}

/// Asserts the player has no registration state for the hunt.
#[track_caller]
fn assert_not_registered(env: &Env, cid: &Address, hunt_id: u64, player: &Address) {
    assert!(
        step(env, cid, |env| Storage::get_player_progress(
            env, hunt_id, player
        ))
        .is_none(),
        "no player progress should have been persisted"
    );
}

// ---------------------------------------------------------------------------
// The regression
// ---------------------------------------------------------------------------

#[test]
fn public_registration_is_refused_once_the_hunt_is_full() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = active_public_hunt(&env, &cid, &creator, 2);
    assert_eq!(
        step(&env, &cid, |env| Storage::get_hunt(env, hunt_id)
            .unwrap()
            .max_players),
        2
    );

    // Fill the hunt to exactly its capacity.
    for _ in 0..2 {
        let player = Address::generate(&env);
        register_public(&env, &cid, hunt_id, &player).unwrap();
    }
    assert_eq!(player_count(&env, &cid, hunt_id), 2);

    // The next public registration is refused. Before the fix it succeeded and
    // pushed the hunt past its configured cap.
    let latecomer = Address::generate(&env);
    let err = register_public(&env, &cid, hunt_id, &latecomer).unwrap_err();
    assert_eq!(err, HuntErrorCode::HuntFull);

    // Nothing was persisted and the count did not move.
    assert_not_registered(&env, &cid, hunt_id, &latecomer);
    assert_eq!(player_count(&env, &cid, hunt_id), 2);
}

#[test]
fn public_registration_succeeds_below_the_cap() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = active_public_hunt(&env, &cid, &creator, 3);

    for expected in 1..=3u32 {
        let player = Address::generate(&env);
        register_public(&env, &cid, hunt_id, &player).unwrap();
        assert_eq!(player_count(&env, &cid, hunt_id), expected);
    }
}

#[test]
fn remaining_slots_tracks_the_cap_for_public_hunts() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = active_public_hunt(&env, &cid, &creator, 2);

    let slots = |env: &Env| Storage::get_hunt(env, hunt_id).unwrap().remaining_slots;
    assert_eq!(step(&env, &cid, |env| slots(env)), 2);

    let a = Address::generate(&env);
    register_public(&env, &cid, hunt_id, &a).unwrap();
    assert_eq!(step(&env, &cid, |env| slots(env)), 1);

    let b = Address::generate(&env);
    register_public(&env, &cid, hunt_id, &b).unwrap();
    assert_eq!(step(&env, &cid, |env| slots(env)), 0);
}

// ---------------------------------------------------------------------------
// The two paths must agree
// ---------------------------------------------------------------------------

#[test]
fn invite_registration_still_enforces_the_cap() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = active_private_hunt(&env, &cid, &creator, 2);

    for _ in 0..2 {
        let player = Address::generate(&env);
        register_with_invite(&env, &cid, hunt_id, &player).unwrap();
    }
    assert_eq!(player_count(&env, &cid, hunt_id), 2);

    let latecomer = Address::generate(&env);
    let err = register_with_invite(&env, &cid, hunt_id, &latecomer).unwrap_err();
    assert_eq!(err, HuntErrorCode::HuntFull);
    assert_not_registered(&env, &cid, hunt_id, &latecomer);
    assert_eq!(player_count(&env, &cid, hunt_id), 2);
}

#[test]
fn a_full_hunt_refuses_both_paths_alike() {
    const CAP: u32 = 1;

    // One hunt filled through each path; both must refuse the next entrant.
    for use_invite in [false, true] {
        let env = Env::default();
        env.ledger().set_timestamp(T0);
        env.mock_all_auths();

        let creator = Address::generate(&env);
        let cid = env.register(HuntyCore, ());

        let hunt_id = if use_invite {
            active_private_hunt(&env, &cid, &creator, CAP)
        } else {
            active_public_hunt(&env, &cid, &creator, CAP)
        };

        let first = Address::generate(&env);
        if use_invite {
            register_with_invite(&env, &cid, hunt_id, &first).unwrap();
        } else {
            register_public(&env, &cid, hunt_id, &first).unwrap();
        }
        assert_eq!(player_count(&env, &cid, hunt_id), CAP);

        let second = Address::generate(&env);
        let err = if use_invite {
            register_with_invite(&env, &cid, hunt_id, &second).unwrap_err()
        } else {
            register_public(&env, &cid, hunt_id, &second).unwrap_err()
        };
        assert_eq!(
            err,
            HuntErrorCode::HuntFull,
            "use_invite={use_invite} should refuse with HuntFull"
        );
        assert_not_registered(&env, &cid, hunt_id, &second);
    }
}

// ---------------------------------------------------------------------------
// Unlimited and a cap of 1
// ---------------------------------------------------------------------------

#[test]
fn max_players_zero_means_unlimited() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = active_public_hunt(&env, &cid, &creator, 0);

    for _ in 0..8 {
        let player = Address::generate(&env);
        register_public(&env, &cid, hunt_id, &player).unwrap();
    }
    assert_eq!(player_count(&env, &cid, hunt_id), 8);
}

#[test]
fn a_cap_of_one_admits_exactly_one_player() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = active_public_hunt(&env, &cid, &creator, 1);

    let first = Address::generate(&env);
    register_public(&env, &cid, hunt_id, &first).unwrap();

    let second = Address::generate(&env);
    assert_eq!(
        register_public(&env, &cid, hunt_id, &second).unwrap_err(),
        HuntErrorCode::HuntFull
    );
    assert_not_registered(&env, &cid, hunt_id, &second);
    assert_eq!(player_count(&env, &cid, hunt_id), 1);
}

// ---------------------------------------------------------------------------
// set_max_players rules
// ---------------------------------------------------------------------------

#[test]
fn max_players_is_creator_or_co_creator_only_and_draft_only() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let stranger = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = active_public_hunt(&env, &cid, &creator, 0);

    // The hunt is Active now, so the cap can no longer be changed.
    let err = step(&env, &cid, |env| {
        HuntyCore::set_max_players(env.clone(), hunt_id, creator.clone(), 5).unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::InvalidHuntStatus);

    // A non-creator could not have set it in the first place.
    let draft = step(&env, &cid, |env| {
        HuntyCore::create_hunt(
            env.clone(),
            creator.clone(),
            String::from_str(env, "Draft"),
            String::from_str(env, "Desc"),
            None,
            None,
            0u32,
            None,
            None,
        )
        .unwrap()
    });
    let err = step(&env, &cid, |env| {
        HuntyCore::set_max_players(env.clone(), draft, stranger.clone(), 5).unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::Unauthorized);
}

// ---------------------------------------------------------------------------
// Ordering: other rejections still take precedence, and persist nothing
// ---------------------------------------------------------------------------

#[test]
fn a_duplicate_registration_is_still_refused_before_the_capacity_check() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    // A hunt already at capacity, so a capacity error would be the alternative.
    let hunt_id = active_public_hunt(&env, &cid, &creator, 1);
    let player = Address::generate(&env);
    register_public(&env, &cid, hunt_id, &player).unwrap();

    // The same player again: DuplicateRegistration, not HuntFull.
    let err = register_public(&env, &cid, hunt_id, &player).unwrap_err();
    assert_eq!(err, HuntErrorCode::DuplicateRegistration);
    assert_eq!(player_count(&env, &cid, hunt_id), 1);
}

#[test]
fn a_deadline_rejection_still_takes_precedence_over_capacity() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = step(&env, &cid, |env| {
        HuntyCore::create_hunt(
            env.clone(),
            creator.clone(),
            String::from_str(env, "Deadline and cap"),
            String::from_str(env, "Desc"),
            None,
            None,
            0u32,
            None,
            None,
        )
        .unwrap()
    });
    step(&env, &cid, |env| {
        HuntyCore::add_clue(
            env.clone(),
            hunt_id,
            String::from_str(env, "Q"),
            String::from_str(env, "a"),
            10,
            true,
            None,
            None,
        )
        .unwrap()
    });
    step(&env, &cid, |env| {
        HuntyCore::set_max_players(env.clone(), hunt_id, creator.clone(), 1).unwrap()
    });
    step(&env, &cid, |env| {
        HuntyCore::set_registration_deadline(env.clone(), hunt_id, creator.clone(), T0 + 100)
            .unwrap()
    });
    step(&env, &cid, |env| {
        HuntyCore::activate_hunt(env.clone(), hunt_id, creator.clone()).unwrap()
    });

    // Fill it, then cross the deadline. RegistrationsPaused, not HuntFull —
    // the deadline check runs first, and the hunt is still full.
    let first = Address::generate(&env);
    register_public(&env, &cid, hunt_id, &first).unwrap();
    assert_eq!(player_count(&env, &cid, hunt_id), 1);

    env.ledger().set_timestamp(T0 + 200);
    let latecomer = Address::generate(&env);
    assert_eq!(
        register_public(&env, &cid, hunt_id, &latecomer).unwrap_err(),
        HuntErrorCode::RegistrationsPaused
    );
    assert_not_registered(&env, &cid, hunt_id, &latecomer);
    assert_eq!(player_count(&env, &cid, hunt_id), 1);
}

#[test]
fn a_private_hunt_still_refuses_public_registration() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    // A generous cap, so capacity is not what refuses the registration.
    let hunt_id = active_private_hunt(&env, &cid, &creator, 0);
    let player = Address::generate(&env);

    let err = register_public(&env, &cid, hunt_id, &player).unwrap_err();
    assert_eq!(err, HuntErrorCode::InvalidHuntStatus);
    assert_not_registered(&env, &cid, hunt_id, &player);
}
