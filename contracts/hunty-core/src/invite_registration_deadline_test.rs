//! Regression tests for #1016 — `register_with_invite` must honour
//! `hunt.registration_deadline`.
//!
//! `register_player` rejected registrations once the creator's deadline had
//! passed, but `register_with_invite` performed no such check. A private hunt
//! therefore stayed joinable by invite long after the point at which public
//! registration closed, so a deadline set on a private hunt was not actually a
//! deadline — anyone holding a valid invite code could enter indefinitely.
//!
//! The fix routes both entrypoints through
//! `ensure_registration_deadline_not_passed`, so the boundary is defined once
//! and the two paths cannot drift apart again.
//!
//! Each entrypoint call runs in its own `env.as_contract` frame — the mocked auth
//! ledger keys entries per frame, so re-authorizing the same address for the
//! same function twice inside one frame trips `Error(Auth, ExistingValue)`.

use crate::errors::HuntErrorCode;
use crate::storage::Storage;
use crate::HuntyCore;
use soroban_sdk::testutils::{Address as _, Events as _, Ledger as _};
use soroban_sdk::{Address, Env, String, Vec};

const T0: u64 = 1_700_000_000;
const DEADLINE: u64 = T0 + 3_600;
const INVITE_CODE: &str = "invite-code-1234";

/// Runs a single contract entrypoint inside its own contract/auth frame.
fn step<T>(env: &Env, contract_id: &Address, f: impl FnOnce(&Env) -> T) -> T {
    env.as_contract(contract_id, || f(env))
}

/// Creates a Draft hunt with one clue. Privacy, the deadline and the clue are
/// all Draft-only, so callers configure them before activating.
fn draft_hunt(env: &Env, cid: &Address, creator: &Address) -> u64 {
    let hunt_id = step(env, cid, |env| {
        HuntyCore::create_hunt(
            env.clone(),
            creator.clone(),
            String::from_str(env, "Deadline hunt"),
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
    hunt_id
}

/// A private hunt, invite-enabled, with a registration deadline, left Active.
fn active_private_hunt(env: &Env, cid: &Address, creator: &Address) -> u64 {
    let hunt_id = draft_hunt(env, cid, creator);

    // The invite code must exist before the hunt is made private:
    // set_hunt_privacy rejects a private hunt with no code.
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
        HuntyCore::set_registration_deadline(env.clone(), hunt_id, creator.clone(), DEADLINE)
            .unwrap()
    });
    step(env, cid, |env| {
        HuntyCore::activate_hunt(env.clone(), hunt_id, creator.clone()).unwrap()
    });

    hunt_id
}

/// The same, but with no deadline configured.
fn active_private_hunt_without_deadline(env: &Env, cid: &Address, creator: &Address) -> u64 {
    let hunt_id = draft_hunt(env, cid, creator);

    // The invite code must exist before the hunt is made private:
    // set_hunt_privacy rejects a private hunt with no code.
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
        HuntyCore::activate_hunt(env.clone(), hunt_id, creator.clone()).unwrap()
    });

    hunt_id
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

/// Asserts the player has no registration state for the hunt.
#[track_caller]
fn assert_not_registered(env: &Env, cid: &Address, hunt_id: u64, player: &Address) {
    let progress = step(env, cid, |env| {
        Storage::get_player_progress(env, hunt_id, player)
    });
    assert!(
        progress.is_none(),
        "no player progress should have been persisted"
    );
}

// ---------------------------------------------------------------------------
// The regression
// ---------------------------------------------------------------------------

#[test]
fn register_with_invite_is_rejected_after_the_deadline() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let latecomer = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = active_private_hunt(&env, &cid, &creator);
    assert_eq!(
        step(&env, &cid, |env| Storage::get_hunt(env, hunt_id)
            .unwrap()
            .registration_deadline),
        DEADLINE
    );

    // Past the deadline, a valid invite no longer grants entry. Before the fix
    // this succeeded and wrote player progress.
    env.ledger().set_timestamp(DEADLINE);
    let err = register_with_invite(&env, &cid, hunt_id, &latecomer).unwrap_err();
    assert_eq!(err, HuntErrorCode::RegistrationsPaused);

    // No registration state was persisted.
    assert_not_registered(&env, &cid, hunt_id, &latecomer);
}

#[test]
fn register_with_invite_is_accepted_before_the_deadline() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = active_private_hunt(&env, &cid, &creator);

    env.ledger().set_timestamp(DEADLINE - 1);
    register_with_invite(&env, &cid, hunt_id, &player).unwrap();
    assert!(step(&env, &cid, |env| Storage::get_player_progress(
        env, hunt_id, &player
    )
    .is_some()));
}

// ---------------------------------------------------------------------------
// Boundary semantics, matching register_player
// ---------------------------------------------------------------------------

#[test]
fn the_deadline_boundary_matches_register_player() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    // Public hunt: deadline configured, then activated.
    let public_hunt = draft_hunt(&env, &cid, &creator);
    step(&env, &cid, |env| {
        HuntyCore::set_registration_deadline(env.clone(), public_hunt, creator.clone(), DEADLINE)
            .unwrap()
    });
    step(&env, &cid, |env| {
        HuntyCore::activate_hunt(env.clone(), public_hunt, creator.clone()).unwrap()
    });

    // The boundary is exclusive: deadline - 1 is accepted, deadline is refused,
    // and the invite path lands on exactly the same boundary.
    for (at, public_ok, invite_ok) in [
        (DEADLINE - 2, true, true),
        (DEADLINE - 1, true, true),
        (DEADLINE, false, false),
        (DEADLINE + 1, false, false),
    ] {
        // set_registration_deadline refuses a deadline already in the past, so
        // each iteration builds its private hunt at T0 and only then advances.
        let invite_hunt = active_private_hunt(&env, &cid, &creator);
        env.ledger().set_timestamp(at);

        let public_player = Address::generate(&env);
        let public = register_public(&env, &cid, public_hunt, &public_player);
        assert_eq!(
            public.is_ok(),
            public_ok,
            "public registration at t={at} should be ok={public_ok}"
        );
        if !public_ok {
            assert_eq!(public.unwrap_err(), HuntErrorCode::RegistrationsPaused);
            assert_not_registered(&env, &cid, public_hunt, &public_player);
        }

        // A fresh private hunt per timestamp so the two paths are compared
        // against identical state.
        let invite_player = Address::generate(&env);
        let invite = register_with_invite(&env, &cid, invite_hunt, &invite_player);
        assert_eq!(
            invite.is_ok(),
            invite_ok,
            "invite registration at t={at} should be ok={invite_ok}"
        );
        if !invite_ok {
            assert_eq!(invite.unwrap_err(), HuntErrorCode::RegistrationsPaused);
            assert_not_registered(&env, &cid, invite_hunt, &invite_player);
        }
    }
}

// ---------------------------------------------------------------------------
// No deadline configured, and other validations still intact
// ---------------------------------------------------------------------------

#[test]
fn invite_registration_still_works_without_a_deadline() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = active_private_hunt_without_deadline(&env, &cid, &creator);

    // Far past any plausible deadline: with none configured, nothing closes.
    env.ledger().set_timestamp(DEADLINE * 100);
    register_with_invite(&env, &cid, hunt_id, &player).unwrap();
    assert!(step(&env, &cid, |env| Storage::get_player_progress(
        env, hunt_id, &player
    )
    .is_some()));
}

#[test]
fn a_wrong_invite_code_is_still_rejected_before_the_deadline() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = active_private_hunt(&env, &cid, &creator);

    env.ledger().set_timestamp(T0 + 1);
    let err = step(&env, &cid, |env| {
        HuntyCore::register_with_invite(
            env.clone(),
            hunt_id,
            player.clone(),
            String::from_str(env, "wrong-code-9999"),
        )
        .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::InvalidAnswer);
    assert_not_registered(&env, &cid, hunt_id, &player);
}

#[test]
fn a_public_hunt_still_rejects_invite_registration() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = draft_hunt(&env, &cid, &creator);
    step(&env, &cid, |env| {
        HuntyCore::set_registration_deadline(env.clone(), hunt_id, creator.clone(), DEADLINE)
            .unwrap()
    });
    step(&env, &cid, |env| {
        HuntyCore::activate_hunt(env.clone(), hunt_id, creator.clone()).unwrap()
    });

    // The deadline check must not change the private-hunt requirement: a public
    // hunt is still not joinable by invite.
    let err = register_with_invite(&env, &cid, hunt_id, &player).unwrap_err();
    assert_eq!(err, HuntErrorCode::InvalidHuntStatus);
    assert_not_registered(&env, &cid, hunt_id, &player);
}

#[test]
fn a_late_invite_registration_does_not_emit_a_registration_event() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = active_private_hunt(&env, &cid, &creator);

    env.ledger().set_timestamp(DEADLINE + 500);
    step(&env, &cid, |env| {
        assert_eq!(
            HuntyCore::register_with_invite(
                env.clone(),
                hunt_id,
                player.clone(),
                String::from_str(env, INVITE_CODE),
            )
            .unwrap_err(),
            HuntErrorCode::RegistrationsPaused
        );
        // A rejected registration publishes nothing.
        assert_eq!(env.events().all(), Vec::new(env));
    });
}

#[test]
fn an_existing_registration_is_unaffected_by_a_later_deadline_pass() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let early = Address::generate(&env);
    let late = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = active_private_hunt(&env, &cid, &creator);

    // One player registers in time.
    register_with_invite(&env, &cid, hunt_id, &early).unwrap();

    // After the deadline, a second player is refused but the first player's
    // registration is untouched — the deadline gates new entries, it does not
    // invalidate existing ones.
    env.ledger().set_timestamp(DEADLINE + 1);
    assert_eq!(
        register_with_invite(&env, &cid, hunt_id, &late).unwrap_err(),
        HuntErrorCode::RegistrationsPaused
    );
    assert_not_registered(&env, &cid, hunt_id, &late);

    let progress = step(&env, &cid, |env| {
        Storage::get_player_progress(env, hunt_id, &early)
    });
    assert!(progress.is_some());
}

// ---------------------------------------------------------------------------
// The invite-code hash must survive unrelated hunt updates
// ---------------------------------------------------------------------------

#[test]
fn a_configured_invite_code_survives_unrelated_hunt_updates() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = active_private_hunt(&env, &cid, &creator);

    // These setters read-modify-write the whole Hunt record. If the read
    // dropped the invite-code hash, each would silently clear it and the hunt
    // would stop being joinable.
    step(&env, &cid, |env| {
        HuntyCore::add_co_creator(
            env.clone(),
            hunt_id,
            creator.clone(),
            Address::generate(env),
        )
        .unwrap()
    });
    step(&env, &cid, |env| {
        HuntyCore::add_view_only_access(
            env.clone(),
            hunt_id,
            creator.clone(),
            Address::generate(env),
        )
        .unwrap()
    });

    // Still joinable by invite afterwards.
    env.ledger().set_timestamp(T0 + 1);
    register_with_invite(&env, &cid, hunt_id, &player).unwrap();
}

#[test]
fn the_invite_code_hash_is_never_exposed_by_public_getters() {
    let env = Env::default();
    env.ledger().set_timestamp(T0);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = active_private_hunt(&env, &cid, &creator);

    // get_hunt_info, list_hunts and search_hunts must all strip the hash. The
    // hash is salted only with the public hunt_id, so leaking it would let
    // anyone brute-force a short human-chosen invite code offline.
    let info = step(&env, &cid, |env| {
        HuntyCore::get_hunt_info(env.clone(), hunt_id).unwrap()
    });
    assert_eq!(info.invite_code_hash, None);
    // Sanity: the hunt is genuinely configured with a code.
    assert!(step(&env, &cid, |env| {
        Storage::get_hunt(&env, hunt_id)
            .unwrap()
            .invite_code_hash
            .is_some()
    }));

    let listed = step(&env, &cid, |env| HuntyCore::list_hunts(env.clone(), 0, 10));
    for i in 0..listed.len() {
        assert_eq!(listed.get(i).unwrap().invite_code_hash, None);
    }

    let found = step(&env, &cid, |env| {
        HuntyCore::search_hunts(env.clone(), String::from_str(&env, "Deadline"), 0, 10, 100)
    });
    for i in 0..found.len() {
        assert_eq!(found.get(i).unwrap().invite_code_hash, None);
    }
}
