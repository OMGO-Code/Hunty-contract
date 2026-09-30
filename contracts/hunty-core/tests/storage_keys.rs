use soroban_sdk::{testutils::Address as _, Address, Env, Symbol};

use hunty_core::rate_limit::{RateLimitData, RateLimiter, RATE_LIMIT_NAMESPACE, SECONDS_PER_DAY};
use hunty_core::HuntyCore;

#[test]
fn test_single_rate_limit_storage_entry_across_days() {
    let env = Env::default();
    let creator = Address::generate(&env);
    // Storage access must happen inside a contract context.
    let contract_id = env.register_contract(None, HuntyCore);

    let day1 = 0;
    let day2 = SECONDS_PER_DAY;
    let day3 = 2 * SECONDS_PER_DAY;

    env.as_contract(&contract_id, || {
        for now in [day1, day2, day3] {
            assert!(RateLimiter::check_and_increment(&env, &creator, now).is_ok());
        }

        // Entries are namespaced so they cannot collide with other features
        // keying storage by a bare Address.
        let key = (Symbol::new(&env, RATE_LIMIT_NAMESPACE), creator.clone());
        let entry: Option<RateLimitData> = env.storage().persistent().get(&key);
        assert!(entry.is_some());
        let entry = entry.unwrap();

        // A single namespaced entry holds the rolling window of creation timestamps:
        // day0 falls outside the 24h window at day3, while day2 and day3 remain.
        assert_eq!(entry.timestamps.len(), 2);
        assert_eq!(entry.timestamps.get(0), Some(day2));
        assert_eq!(entry.timestamps.get(1), Some(day3));
    });
}
