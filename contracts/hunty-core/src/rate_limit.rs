use crate::errors::HuntErrorCode;
use crate::storage::Storage;
use crate::types::RateLimitStatus;
use soroban_sdk::{contracttype, Address, Env, Symbol, Vec};

pub const SECONDS_PER_DAY: u64 = 86_400;
pub const DEFAULT_HUNT_CREATION_LIMIT: u32 = 10;

/// TTL extension target for rate limit entries (~30 days).
const RATE_LIMIT_TTL: u32 = 30 * 24 * 60 * 60;
/// Threshold below which we extend the TTL (~15 days).
const RATE_LIMIT_TTL_THRESHOLD: u32 = 15 * 24 * 60 * 60;

/// Namespace used to avoid collisions with other features keying by a bare `Address`.
pub const RATE_LIMIT_NAMESPACE: &str = "HRATE";

/// Legacy namespace used before the fix, for migration of existing entries.
const RATE_LIMIT_LEGACY_NAMESPACE: &str = "HRATE_LEGACY";

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct RateLimitData {
    pub timestamps: Vec<u64>,
}

pub struct RateLimiter;

impl RateLimiter {
    fn key(env: &Env, creator: &Address) -> (Symbol, Address) {
        (Symbol::new(env, RATE_LIMIT_NAMESPACE), creator.clone())
    }

    fn legacy_key(env: &Env, creator: &Address) -> (Symbol, Address) {
        (
            Symbol::new(env, RATE_LIMIT_LEGACY_NAMESPACE),
            creator.clone(),
        )
    }

    /// Read the rate limit data for a creator, migrating legacy entries if needed.
    fn read(env: &Env, creator: &Address) -> Option<RateLimitData> {
        if let Some(data) = env
            .storage()
            .persistent()
            .get::<(Symbol, Address), RateLimitData>(&Self::key(env, creator))
        {
            return Some(data);
        }

        // Migrate existing entries that were stored under the bare address.
        if let Some(data) = env
            .storage()
            .persistent()
            .get::<Address, RateLimitData>(creator)
        {
            env.storage()
                .persistent()
                .set(&Self::legacy_key(env, creator), &data);
            env.storage().persistent().remove(creator);
            return Some(data);
        }

        None
    }

    fn write(env: &Env, creator: &Address, data: &RateLimitData) {
        let key = Self::key(env, creator);
        env.storage().persistent().set(&key, data);
        env.storage()
            .persistent()
            .extend_ttl(&key, RATE_LIMIT_TTL_THRESHOLD, RATE_LIMIT_TTL);
    }

    pub fn check_and_increment(
        env: &Env,
        creator: &Address,
        now: u64,
    ) -> Result<(), HuntErrorCode> {
        let limit = Storage::get_effective_hunt_creation_limit(env, creator);
        let mut data = Self::read(env, creator).unwrap_or(RateLimitData {
            timestamps: Vec::new(env),
        });

        let cutoff = now.saturating_sub(SECONDS_PER_DAY);
        data.timestamps = prune_timestamps(env, &data.timestamps, cutoff);

        if data.timestamps.len() >= limit {
            return Err(HuntErrorCode::RateLimitExceeded);
        }

        data.timestamps.push_back(now);
        Self::write(env, creator, &data);
        Ok(())
    }

    pub fn get_status(env: &Env, creator: &Address, now: u64) -> RateLimitStatus {
        let limit = Storage::get_effective_hunt_creation_limit(env, creator);
        let data = Self::read(env, creator).unwrap_or(RateLimitData {
            timestamps: Vec::new(env),
        });

        let cutoff = now.saturating_sub(SECONDS_PER_DAY);
        let recent = prune_timestamps(env, &data.timestamps, cutoff);
        let count = recent.len();

        let cooldown_seconds = if count >= limit {
            let oldest = recent.get(0).unwrap_or(0);
            (oldest + SECONDS_PER_DAY).saturating_sub(now)
        } else {
            0
        };

        RateLimitStatus {
            creations_today: count,
            daily_limit: limit,
            cooldown_seconds,
        }
    }

    pub fn require_rate_limit_admin(env: &Env, admin: &Address) -> Result<(), HuntErrorCode> {
        admin.require_auth();
        let stored = Storage::get_rate_limit_admin(env).ok_or(HuntErrorCode::Unauthorized)?;
        if stored != *admin {
            return Err(HuntErrorCode::Unauthorized);
        }
        Ok(())
    }
}

fn prune_timestamps(env: &Env, timestamps: &Vec<u64>, cutoff: u64) -> Vec<u64> {
    let mut pruned = Vec::new(env);
    let mut i = 0;
    while i < timestamps.len() {
        let timestamp = timestamps.get(i).unwrap();
        if timestamp >= cutoff {
            pruned.push_back(timestamp);
        }
        i += 1;
    }
    pruned
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::Env;

    #[test]
    fn rolling_window_across_midnight() {
        let env = Env::default();
        let contract_id = env.register(crate::HuntyCore, ());
        let creator = Address::generate(&env);

        // Create 10 hunts just before UTC midnight.
        env.as_contract(&contract_id, || {
            for _ in 0..10 {
                assert!(RateLimiter::check_and_increment(&env, &creator, 86399).is_ok());
            }

            // The 11th attempt at the same time should fail.
            assert_eq!(
                RateLimiter::check_and_increment(&env, &creator, 86399),
                Err(HuntErrorCode::RateLimitExceeded)
            );

            // Advance 2 seconds across midnight. The previous 10 creations are still within
            // the rolling 24-hour window, so the limit must still be enforced.
            assert_eq!(
                RateLimiter::check_and_increment(&env, &creator, 86401),
                Err(HuntErrorCode::RateLimitExceeded)
            );

            // get_status reports cooldown > 0 at the boundary.
            let status = RateLimiter::get_status(&env, &creator, 86401);
            assert_eq!(status.creations_today, 10);
            assert!(status.cooldown_seconds > 0);
        });
    }
}
