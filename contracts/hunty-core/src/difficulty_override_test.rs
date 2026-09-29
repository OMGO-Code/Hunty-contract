//! #1052: `set_hunt_difficulty_override` must be Draft-only and must emit
//! `HuntDifficultyOverrideSet`, so a creator/co-creator cannot re-rate a hunt
//! after players have chosen it.

use crate::errors::HuntErrorCode;
use crate::types::{HuntDifficultyOverrideSetEvent, HuntStatus};
use crate::HuntyCore;
use soroban_sdk::testutils::{Address as _, Events as _, Ledger as _};
use soroban_sdk::{vec, Address, Env, IntoVal, String, Symbol, Val, Vec};

#[test]
fn set_hunt_difficulty_override_is_draft_only_and_emits_event() {
    let env = Env::default();
    env.ledger().set_timestamp(1_700_000_000);
    env.mock_all_auths();

    let contract_id = env.register(HuntyCore, ());
    let creator = Address::generate(&env);
    let stranger = Address::generate(&env);

    let hunt_id = env.as_contract(&contract_id, || {
        let hunt_id = HuntyCore::create_hunt(
            env.clone(),
            creator.clone(),
            String::from_str(&env, "Override Hunt"),
            String::from_str(&env, "Difficulty override stays Draft-only"),
            None,
            None,
            0,
            None,
            None,
        )
        .unwrap();
        // One required clue with difficulty 3, so the derived rating with no
        // override is 3.
        HuntyCore::add_clue(
            env.clone(),
            hunt_id,
            String::from_str(&env, "Question"),
            String::from_str(&env, "answer"),
            10,
            true,
            Some(3),
            None,
        )
        .unwrap();
        hunt_id
    });

    // Draft: no override yet, the rating mirrors the clue difficulty.
    let draft = env.as_contract(&contract_id, || {
        HuntyCore::get_hunt_info(env.clone(), hunt_id).unwrap()
    });
    assert_eq!(draft.difficulty_override, None);
    assert_eq!(draft.difficulty_rating, 3);

    // Draft: setting the override updates the rating and emits the event.
    env.as_contract(&contract_id, || {
        HuntyCore::set_hunt_difficulty_override(env.clone(), hunt_id, creator.clone(), Some(5))
            .unwrap();

        let expected_event = HuntDifficultyOverrideSetEvent {
            hunt_id,
            caller: creator.clone(),
            difficulty_override: Some(5),
        };
        let expected: Vec<(Address, Vec<Val>, Val)> = vec![
            &env,
            (
                contract_id.clone(),
                (Symbol::new(&env, "HuntDifficultyOverrideSet"), hunt_id).into_val(&env),
                expected_event.into_val(&env),
            ),
        ];
        assert_eq!(env.events().all(), expected);
    });

    let set = env.as_contract(&contract_id, || {
        HuntyCore::get_hunt_info(env.clone(), hunt_id).unwrap()
    });
    assert_eq!(set.difficulty_override, Some(5));
    assert_eq!(set.difficulty_rating, 5);

    // Draft: clearing restores the clue-derived rating.
    env.as_contract(&contract_id, || {
        HuntyCore::set_hunt_difficulty_override(env.clone(), hunt_id, creator.clone(), None)
            .unwrap();
    });
    let cleared = env.as_contract(&contract_id, || {
        HuntyCore::get_hunt_info(env.clone(), hunt_id).unwrap()
    });
    assert_eq!(cleared.difficulty_override, None);
    assert_eq!(cleared.difficulty_rating, 3);

    // Only the creator or a co-creator may change the override.
    env.as_contract(&contract_id, || {
        assert_eq!(
            HuntyCore::set_hunt_difficulty_override(
                env.clone(),
                hunt_id,
                stranger.clone(),
                Some(5),
            ),
            Err(HuntErrorCode::Unauthorized)
        );
    });

    // Active: players can see the hunt, so the override is frozen.
    // NOTE: each `as_contract` block gets its own auth frame, and recording
    // mode allows one `require_auth` per address per frame, so the activation
    // and the rejected setter call live in separate blocks.
    env.as_contract(&contract_id, || {
        HuntyCore::activate_hunt(env.clone(), hunt_id, creator.clone()).unwrap();
    });
    env.as_contract(&contract_id, || {
        assert_eq!(
            HuntyCore::set_hunt_difficulty_override(env.clone(), hunt_id, creator.clone(), Some(5)),
            Err(HuntErrorCode::InvalidHuntStatus)
        );
    });

    // Completed: still frozen.
    env.as_contract(&contract_id, || {
        HuntyCore::close_hunt(env.clone(), hunt_id, creator.clone()).unwrap();
        let hunt = HuntyCore::get_hunt_info(env.clone(), hunt_id).unwrap();
        assert_eq!(hunt.status, HuntStatus::Completed);
    });
    env.as_contract(&contract_id, || {
        assert_eq!(
            HuntyCore::set_hunt_difficulty_override(env.clone(), hunt_id, creator.clone(), Some(5)),
            Err(HuntErrorCode::InvalidHuntStatus)
        );
    });
}
