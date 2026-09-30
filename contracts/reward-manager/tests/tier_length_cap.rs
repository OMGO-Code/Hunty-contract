//! Tier-list length cap for reward-manager (issue #1081).
//!
//! `set_pool_tiers` and `set_pool_rank_tiers` previously accepted arbitrarily
//! long tier lists. The whole `RewardPoolConfig` (tiers included) is read on
//! every distribution and by HuntyCore at completion, so an oversized config
//! makes every payout for that hunt expensive or impossible.
//!
//! These tests pin the cap added in #1081: a list at the limit is accepted, one
//! entry past the limit is rejected with `InvalidConfig`, and a rejected list
//! leaves the previously stored config untouched.
//!
//! Written as a standalone integration target because the crate's `src/test.rs`
//! unit-test module does not currently compile on `main` — see the PR
//! description.

use reward_manager::storage::Storage;
use reward_manager::{
    DistributionMode, RankRewardTier, RewardErrorCode, RewardManager, RewardPoolConfig,
    TimeBasedRewardTier, MAX_TIER_ENTRIES,
};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, Env, Vec};

fn setup() -> (Env, Address) {
    let env = Env::default();
    env.mock_all_auths();

    // `RewardManager` initializes through its 3-argument `__constructor`
    // (admin, xlm_token, hunty_core); registering with no arguments panics
    // before any tier setter can run.
    let admin = Address::generate(&env);
    let xlm_token = Address::generate(&env);
    let hunty_core = Address::generate(&env);
    let contract_id = env.register(RewardManager, (admin, xlm_token, hunty_core));

    (env, contract_id)
}

/// Write a pool config directly, mirroring the crate's `seed_pool_config`
/// helper. Pool creation is not needed to exercise the tier setters.
fn seed_config(env: &Env, contract_id: &Address, creator: &Address, hunt_id: u64) {
    let config = RewardPoolConfig {
        creator: creator.clone(),
        delegates: Vec::new(env),
        min_distribution_amount: 0,
        time_based_tiers: Vec::new(env),
        rank_based_tiers: Vec::new(env),
        frozen: false,
        token_address: Address::generate(env),
        nft_contract: None,
        target_amount: 0,
        min_distribution_interval_secs: 0,
        distribution_mode: DistributionMode::Fixed,
        vesting_period_secs: 0,
        claim_deadline: 0,
        nft_royalty_bps: 0,
        nft_transferable: true,
        frozen_by: None,
    };
    env.as_contract(contract_id, || {
        Storage::set_pool_config(env, hunt_id, &config);
    });
}

fn time_tiers(env: &Env, count: u32) -> Vec<TimeBasedRewardTier> {
    let mut tiers = Vec::new(env);
    for i in 0..count {
        tiers.push_back(TimeBasedRewardTier {
            max_completion_secs: (i as u64 + 1) * 60,
            xlm_amount: 100,
        });
    }
    tiers
}

fn rank_tiers(env: &Env, count: u32) -> Vec<RankRewardTier> {
    let mut tiers = Vec::new(env);
    for i in 0..count {
        tiers.push_back(RankRewardTier {
            rank: i + 1,
            xlm_amount: 100,
        });
    }
    tiers
}

#[test]
fn cap_is_twenty_entries() {
    // Pin the chosen limit so the documented cap (and the per-distribution
    // config read cost it bounds) cannot silently drift.
    assert_eq!(MAX_TIER_ENTRIES, 20);
}

#[test]
fn lists_just_below_cap_are_accepted() {
    let (env, contract_id) = setup();
    let creator = Address::generate(&env);
    let hunt_id = 70;
    seed_config(&env, &contract_id, &creator, hunt_id);

    // `MAX_TIER_ENTRIES - 1` is the last "safely under the bound" size for both
    // tier kinds; it must be accepted and persisted verbatim.
    env.as_contract(&contract_id, || {
        RewardManager::set_pool_tiers(
            env.clone(),
            creator.clone(),
            hunt_id,
            time_tiers(&env, MAX_TIER_ENTRIES - 1),
        )
        .unwrap();
    });
    let stored = env.as_contract(&contract_id, || {
        Storage::get_pool_config(&env, hunt_id).unwrap()
    });
    assert_eq!(stored.time_based_tiers.len(), MAX_TIER_ENTRIES - 1);

    env.as_contract(&contract_id, || {
        RewardManager::set_pool_rank_tiers(
            env.clone(),
            creator.clone(),
            hunt_id,
            rank_tiers(&env, MAX_TIER_ENTRIES - 1),
        )
        .unwrap();
    });
    let stored = env.as_contract(&contract_id, || {
        Storage::get_pool_config(&env, hunt_id).unwrap()
    });
    assert_eq!(stored.rank_based_tiers.len(), MAX_TIER_ENTRIES - 1);
}

#[test]
fn time_tiers_at_cap_accepted_over_cap_rejected_without_mutation() {
    let (env, contract_id) = setup();
    let creator = Address::generate(&env);
    let hunt_id = 71;
    seed_config(&env, &contract_id, &creator, hunt_id);

    // A list exactly at the cap is accepted and persisted.
    env.as_contract(&contract_id, || {
        RewardManager::set_pool_tiers(
            env.clone(),
            creator.clone(),
            hunt_id,
            time_tiers(&env, MAX_TIER_ENTRIES),
        )
        .unwrap();
    });
    let stored = env.as_contract(&contract_id, || {
        Storage::get_pool_config(&env, hunt_id).unwrap()
    });
    assert_eq!(stored.time_based_tiers.len(), MAX_TIER_ENTRIES);

    // One entry past the cap is rejected, and the stored config is untouched.
    env.as_contract(&contract_id, || {
        let err = RewardManager::set_pool_tiers(
            env.clone(),
            creator.clone(),
            hunt_id,
            time_tiers(&env, MAX_TIER_ENTRIES + 1),
        )
        .unwrap_err();
        assert_eq!(err, RewardErrorCode::InvalidConfig);
    });
    let after = env.as_contract(&contract_id, || {
        Storage::get_pool_config(&env, hunt_id).unwrap()
    });
    assert_eq!(
        after.time_based_tiers.len(),
        MAX_TIER_ENTRIES,
        "a rejected over-cap list must not overwrite the stored config"
    );
}

#[test]
fn rank_tiers_at_cap_accepted_over_cap_rejected_without_mutation() {
    let (env, contract_id) = setup();
    let creator = Address::generate(&env);
    let hunt_id = 72;
    seed_config(&env, &contract_id, &creator, hunt_id);

    env.as_contract(&contract_id, || {
        RewardManager::set_pool_rank_tiers(
            env.clone(),
            creator.clone(),
            hunt_id,
            rank_tiers(&env, MAX_TIER_ENTRIES),
        )
        .unwrap();
    });
    let stored = env.as_contract(&contract_id, || {
        Storage::get_pool_config(&env, hunt_id).unwrap()
    });
    assert_eq!(stored.rank_based_tiers.len(), MAX_TIER_ENTRIES);

    env.as_contract(&contract_id, || {
        let err = RewardManager::set_pool_rank_tiers(
            env.clone(),
            creator.clone(),
            hunt_id,
            rank_tiers(&env, MAX_TIER_ENTRIES + 1),
        )
        .unwrap_err();
        assert_eq!(err, RewardErrorCode::InvalidConfig);
    });
    let after = env.as_contract(&contract_id, || {
        Storage::get_pool_config(&env, hunt_id).unwrap()
    });
    assert_eq!(
        after.rank_based_tiers.len(),
        MAX_TIER_ENTRIES,
        "a rejected over-cap list must not overwrite the stored config"
    );
    assert_eq!(after.rank_based_tiers.get(0).unwrap().xlm_amount, 100);
}
