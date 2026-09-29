//! Regression tests for the security fixes in #1011-#1014:
//!
//! * **#1011** — `initialize_admin` persists the admin (and refuses a second init).
//! * **#1012** — `set_reward_config` is creator-only and Draft-only.
//! * **#1013** — the global pause (`ensure_not_paused`) gates player routes.
//! * **#1014** — `register_player` rejects a player who already has progress,
//!   including after they have claimed their reward.
//!
//! Each entrypoint call runs in its own `env.as_contract` frame — the mocked
//! auth ledger keys entries per frame, so re-authorizing the same address for
//! the same function twice inside one frame trips `Error(Auth, ExistingValue)`.

use crate::errors::HuntErrorCode;
use crate::HuntyCore;
use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{Address, Env, String};

/// Runs a single contract entrypoint inside its own contract/auth frame.
fn step<T>(env: &Env, contract_id: &Address, f: impl FnOnce(&Env) -> T) -> T {
    env.as_contract(contract_id, || f(env))
}

fn create_hunt_step(env: &Env, cid: &Address, creator: &Address, title: &str) -> u64 {
    step(env, cid, |env| {
        HuntyCore::create_hunt(
            env.clone(),
            creator.clone(),
            String::from_str(env, title),
            String::from_str(env, "Desc"),
            None,
            None,
            0u32,
            None,
            None,
        )
        .unwrap()
    })
}

// ---------- #1011 ----------

#[test]
fn initialize_admin_persists_admin_for_reward_manager() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let attacker = Address::generate(&env);
    let reward_manager = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    step(&env, &cid, |env| {
        HuntyCore::initialize_admin(env.clone(), admin.clone()).unwrap()
    });

    // The admin is persisted, so a second initialization is refused.
    let err = step(&env, &cid, |env| {
        HuntyCore::initialize_admin(env.clone(), admin.clone()).unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::Unauthorized);

    // set_reward_manager is require_admin()-gated and now resolves the
    // persisted admin instead of failing for everyone.
    step(&env, &cid, |env| {
        HuntyCore::set_reward_manager(env.clone(), admin.clone(), reward_manager.clone()).unwrap()
    });

    let err = step(&env, &cid, |env| {
        HuntyCore::set_reward_manager(env.clone(), attacker.clone(), reward_manager.clone())
            .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::Unauthorized);
}

// ---------- #1012 ----------

#[test]
fn set_reward_config_rejects_non_creator() {
    let env = Env::default();
    env.ledger().set_timestamp(1_700_000_000);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let attacker = Address::generate(&env);
    let cid = env.register(HuntyCore, ());

    let hunt_id = create_hunt_step(&env, &cid, &creator, "Hunt");

    // A non-creator must be rejected even with mocked auths.
    let err = step(&env, &cid, |env| {
        HuntyCore::set_reward_config(
            env.clone(),
            hunt_id,
            5u32,
            1_000i128,
            false,
            None,
            attacker.clone(),
        )
        .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::Unauthorized);

    // The creator may configure rewards while the hunt is still Draft.
    step(&env, &cid, |env| {
        HuntyCore::set_reward_config(
            env.clone(),
            hunt_id,
            5u32,
            1_000i128,
            false,
            None,
            creator.clone(),
        )
        .unwrap()
    });
}

// ---------- #1013 ----------

#[test]
fn contract_pause_blocks_register_player_and_submit_answer() {
    let env = Env::default();
    env.ledger().set_timestamp(1_700_000_000);
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let question = String::from_str(&env, "Q");
    let answer = String::from_str(&env, "a");
    let cid = env.register(HuntyCore, ());

    step(&env, &cid, |env| {
        HuntyCore::initialize_admin(env.clone(), admin.clone()).unwrap()
    });

    let hunt_id = create_hunt_step(&env, &cid, &creator, "Hunt");
    step(&env, &cid, |env| {
        HuntyCore::add_clue(
            env.clone(),
            hunt_id,
            question,
            answer.clone(),
            10,
            true,
            None,
            None,
        )
        .unwrap()
    });
    step(&env, &cid, |env| {
        HuntyCore::activate_hunt(env.clone(), hunt_id, creator.clone()).unwrap()
    });

    step(&env, &cid, |env| {
        HuntyCore::pause_contract(env.clone(), admin.clone()).unwrap()
    });
    assert!(step(&env, &cid, |env| HuntyCore::is_contract_paused(
        env.clone()
    )));

    let err = step(&env, &cid, |env| {
        HuntyCore::register_player(env.clone(), hunt_id, player.clone()).unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::ContractPaused);

    let err = step(&env, &cid, |env| {
        HuntyCore::submit_answer(
            env.clone(),
            hunt_id,
            1,
            player.clone(),
            answer.clone(),
            1,
            env.ledger().timestamp(),
        )
        .unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::ContractPaused);

    step(&env, &cid, |env| {
        HuntyCore::unpause_contract(env.clone(), admin.clone()).unwrap()
    });
    step(&env, &cid, |env| {
        HuntyCore::register_player(env.clone(), hunt_id, player.clone()).unwrap()
    });
}

// ---------- #1014 ----------

#[test]
fn re_registration_after_reward_claim_fails_duplicate() {
    let env = Env::default();
    env.ledger().set_timestamp(1_700_000_000);
    env.mock_all_auths();

    let creator = Address::generate(&env);
    let player = Address::generate(&env);
    let answer = String::from_str(&env, "2");
    let cid = env.register(HuntyCore, ());

    let hunt_id = create_hunt_step(&env, &cid, &creator, "Reward Hunt");
    step(&env, &cid, |env| {
        HuntyCore::add_clue(
            env.clone(),
            hunt_id,
            String::from_str(env, "What is 1+1?"),
            answer.clone(),
            10,
            true,
            None,
            None,
        )
        .unwrap()
    });

    // Reward pool is configured while the hunt is still Draft.
    step(&env, &cid, |env| {
        HuntyCore::set_reward_config(
            env.clone(),
            hunt_id,
            5u32,
            1_000i128,
            false,
            None,
            creator.clone(),
        )
        .unwrap()
    });

    step(&env, &cid, |env| {
        HuntyCore::activate_hunt(env.clone(), hunt_id, creator.clone()).unwrap()
    });
    step(&env, &cid, |env| {
        HuntyCore::register_player(env.clone(), hunt_id, player.clone()).unwrap()
    });

    // Solve the only required clue so the player can claim.
    step(&env, &cid, |env| {
        HuntyCore::submit_answer(
            env.clone(),
            hunt_id,
            1,
            player.clone(),
            answer,
            7,
            env.ledger().timestamp(),
        )
        .unwrap()
    });

    step(&env, &cid, |env| {
        HuntyCore::complete_hunt(env.clone(), hunt_id, player.clone()).unwrap()
    });

    let claimed = step(&env, &cid, |env| {
        HuntyCore::get_player_progress(env.clone(), hunt_id, player.clone())
            .unwrap()
            .reward_claimed
    });
    assert!(claimed);

    // Progress outlives the claim, so re-registration must be rejected.
    let err = step(&env, &cid, |env| {
        HuntyCore::register_player(env.clone(), hunt_id, player.clone()).unwrap_err()
    });
    assert_eq!(err, HuntErrorCode::DuplicateRegistration);
}
