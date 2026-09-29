#![no_std]

mod admin;
mod errors;
mod migration;
mod nft_handler;
mod storage;
mod token_handler;
mod types;
mod xlm_handler;

pub use errors::RewardErrorCode;
pub use reward_interface::{
    rank_tiers_are_strictly_ascending, resolve_rank_tier_amount, resolve_tier_amount,
    tiers_are_strictly_ascending, DistributionMode, RankBasedRewardTier, RankRewardTier,
    RewardConfig, RewardPoolConfig, TierError, TimeBasedRewardTier,
};
pub use storage::Storage;
pub use types::{
    BatchDistributionEntry, DistributionProof, DistributionRecord, DistributionStatus,
    HuntStatus, HuntyCoreClient, PendingNftMint, PoolAuditEntry, PoolDistribution, PoolOperation,
    RewardPoolStatistics, RewardPoolStatus, SemVer, ValidationResult, VestingRecord,
};

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, Symbol, Vec};

// ---------------------------------------------------------------------------
// Event types
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug)]
pub struct RewardsDistributedEvent {
    pub hunt_id: u64,
    pub player: Address,
    pub xlm_amount: i128,
    pub nft_id: Option<u64>,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct EmergencyWithdrawalLogEntry {
    pub actor: Address,
    pub amount: i128,
    pub timestamp: u64,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Minimum funding amount per deposit (1 XLM in stroops).
const MIN_FUNDING_AMOUNT: i128 = 10_000_000;
/// Maximum single-deposit amount (1 billion XLM in stroops).
const MAX_FUNDING_AMOUNT: i128 = 1_000_000_000 * 10_000_000;
/// Maximum pool balance (1 billion XLM in stroops).
const MAX_POOL_BALANCE: i128 = 1_000_000_000 * 10_000_000;
/// Maximum number of tiers allowed per pool (gas/storage cap, issue #1081).
pub const MAX_TIER_LIST_LEN: u32 = 50;

// ---------------------------------------------------------------------------
// Contract declaration
// ---------------------------------------------------------------------------

#[contract]
pub struct RewardManager;

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

impl RewardManager {
    /// Returns `Ok(())` only when the hunt's status is `Draft`.
    ///
    /// Fails closed: if HuntyCore is not configured or the cross-contract
    /// call fails for any reason, the setter is rejected with `HuntLocked`
    /// rather than silently allowing a potentially post-activation change.
    ///
    /// Called from all six payout-affecting setters after the creator auth
    /// check and before any storage write, so a rejected call changes nothing.
    fn require_hunt_editable(env: &Env, hunt_id: u64) -> Result<(), RewardErrorCode> {
        let core: Address = Storage::get_hunty_core(env)
            .ok_or(RewardErrorCode::HuntLocked)?;

        match HuntyCoreClient::new(env, &core).try_get_hunt_status(&hunt_id) {
            Ok(Ok(HuntStatus::Draft)) => Ok(()),
            _ => Err(RewardErrorCode::HuntLocked),
        }
    }

    fn require_admin(env: &Env, caller: &Address) -> Result<(), RewardErrorCode> {
        caller.require_auth();
        let admin = Storage::get_admin(env).ok_or(RewardErrorCode::Unauthorized)?;
        if admin != *caller {
            return Err(RewardErrorCode::Unauthorized);
        }
        Ok(())
    }

    fn require_pool_creator(
        env: &Env,
        caller: &Address,
        hunt_id: u64,
    ) -> Result<RewardPoolConfig, RewardErrorCode> {
        caller.require_auth();
        let config = Storage::get_pool_config(env, hunt_id).ok_or(RewardErrorCode::PoolNotFound)?;
        if config.creator != *caller {
            return Err(RewardErrorCode::Unauthorized);
        }
        Ok(config)
    }
}

// ---------------------------------------------------------------------------
// Contract entrypoints
// ---------------------------------------------------------------------------

#[contractimpl]
impl RewardManager {
    // -----------------------------------------------------------------------
    // Initialization
    // -----------------------------------------------------------------------

    /// Initializes the contract.
    ///
    /// `hunty_core` is the address of the deployed HuntyCore contract and is
    /// required for hunt-status checks in the payout-settings setters.
    pub fn initialize(
        env: Env,
        admin: Address,
        xlm_token: Address,
        hunty_core: Address,
    ) -> Result<(), RewardErrorCode> {
        admin.require_auth();
        if Storage::get_admin(&env).is_some() {
            return Err(RewardErrorCode::AlreadyInitialized);
        }
        Storage::set_admin(&env, &admin);
        Storage::set_xlm_token(&env, &xlm_token);
        Storage::set_hunty_core(&env, &hunty_core);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Pool lifecycle
    // -----------------------------------------------------------------------

    pub fn create_reward_pool(
        env: Env,
        creator: Address,
        hunt_id: u64,
        min_distribution_amount: i128,
    ) -> Result<(), RewardErrorCode> {
        creator.require_auth();
        if Storage::get_pool_config(&env, hunt_id).is_some() {
            return Err(RewardErrorCode::PoolAlreadyExists);
        }
        let xlm_token = Storage::get_xlm_token(&env).ok_or(RewardErrorCode::NotInitialized)?;
        let config = RewardPoolConfig {
            creator,
            delegates: Vec::new(&env),
            min_distribution_amount,
            time_based_tiers: Vec::new(&env),
            rank_based_tiers: Vec::new(&env),
            frozen: false,
            token_address: xlm_token,
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
        Storage::set_pool_config(&env, hunt_id, &config);
        Storage::append_audit_entry(
            &env,
            hunt_id,
            PoolAuditEntry {
                actor: config.creator.clone(),
                operation: PoolOperation::Create,
                timestamp: env.ledger().timestamp(),
                amount: None,
            },
        );
        Ok(())
    }

    pub fn create_reward_pool_with_nft(
        env: Env,
        creator: Address,
        hunt_id: u64,
        token_address: Address,
        min_distribution_amount: i128,
        nft_contract: Option<Address>,
        nft_royalty_bps: u32,
        nft_transferable: bool,
    ) -> Result<(), RewardErrorCode> {
        creator.require_auth();
        if Storage::get_pool_config(&env, hunt_id).is_some() {
            return Err(RewardErrorCode::PoolAlreadyExists);
        }
        let config = RewardPoolConfig {
            creator: creator.clone(),
            delegates: Vec::new(&env),
            min_distribution_amount,
            time_based_tiers: Vec::new(&env),
            rank_based_tiers: Vec::new(&env),
            frozen: false,
            token_address,
            nft_contract,
            target_amount: 0,
            min_distribution_interval_secs: 0,
            distribution_mode: DistributionMode::Fixed,
            vesting_period_secs: 0,
            claim_deadline: 0,
            nft_royalty_bps,
            nft_transferable,
            frozen_by: None,
        };
        Storage::set_pool_config(&env, hunt_id, &config);
        Storage::append_audit_entry(
            &env,
            hunt_id,
            PoolAuditEntry {
                actor: creator,
                operation: PoolOperation::Create,
                timestamp: env.ledger().timestamp(),
                amount: None,
            },
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Payout-settings setters — all locked once the hunt leaves Draft
    // -----------------------------------------------------------------------

    /// Replaces the time-based reward tier schedule for a pool.
    ///
    /// Passing an empty list disables time-based tiers (flat/rank rewards apply).
    /// Fails with `HuntLocked` if the hunt is no longer in Draft status.
    pub fn set_pool_tiers(
        env: Env,
        creator: Address,
        hunt_id: u64,
        tiers: Vec<TimeBasedRewardTier>,
    ) -> Result<(), RewardErrorCode> {
        let mut config = Self::require_pool_creator(&env, &creator, hunt_id)?;
        Self::require_hunt_editable(&env, hunt_id)?;

        // Validate non-empty lists; an empty list is valid (disables tiers).
        if !tiers.is_empty() {
            tiers_are_strictly_ascending(&tiers).map_err(|_| RewardErrorCode::InvalidConfig)?;
        }
        if tiers.len() > MAX_TIER_LIST_LEN {
            return Err(RewardErrorCode::InvalidConfig);
        }

        config.time_based_tiers = tiers;
        Storage::set_pool_config(&env, hunt_id, &config);
        Storage::append_audit_entry(
            &env,
            hunt_id,
            PoolAuditEntry {
                actor: creator,
                operation: PoolOperation::SetTimeTiers,
                timestamp: env.ledger().timestamp(),
                amount: None,
            },
        );
        Ok(())
    }

    /// Replaces the rank-based reward tier schedule for a pool.
    ///
    /// Passing an empty list disables rank-based tiers.
    /// Fails with `HuntLocked` if the hunt is no longer in Draft status.
    pub fn set_pool_rank_tiers(
        env: Env,
        creator: Address,
        hunt_id: u64,
        tiers: Vec<RankRewardTier>,
    ) -> Result<(), RewardErrorCode> {
        let mut config = Self::require_pool_creator(&env, &creator, hunt_id)?;
        Self::require_hunt_editable(&env, hunt_id)?;

        if !tiers.is_empty() {
            rank_tiers_are_strictly_ascending(&tiers)
                .map_err(|_| RewardErrorCode::InvalidConfig)?;
        }
        if tiers.len() > MAX_TIER_LIST_LEN {
            return Err(RewardErrorCode::InvalidConfig);
        }

        config.rank_based_tiers = tiers;
        Storage::set_pool_config(&env, hunt_id, &config);
        Storage::append_audit_entry(
            &env,
            hunt_id,
            PoolAuditEntry {
                actor: creator,
                operation: PoolOperation::SetRankTiers,
                timestamp: env.ledger().timestamp(),
                amount: None,
            },
        );
        Ok(())
    }

    /// Sets or clears the NFT contract address for a pool.
    ///
    /// Fails with `HuntLocked` if the hunt is no longer in Draft status.
    pub fn set_pool_nft_contract(
        env: Env,
        creator: Address,
        hunt_id: u64,
        nft_contract: Option<Address>,
    ) -> Result<(), RewardErrorCode> {
        let mut config = Self::require_pool_creator(&env, &creator, hunt_id)?;
        Self::require_hunt_editable(&env, hunt_id)?;

        config.nft_contract = nft_contract;
        Storage::set_pool_config(&env, hunt_id, &config);
        Storage::append_audit_entry(
            &env,
            hunt_id,
            PoolAuditEntry {
                actor: creator,
                operation: PoolOperation::SetNftContract,
                timestamp: env.ledger().timestamp(),
                amount: None,
            },
        );
        Ok(())
    }

    /// Switches the pool's distribution mode between Fixed and Proportional.
    ///
    /// Fails with `HuntLocked` if the hunt is no longer in Draft status.
    pub fn set_distribution_mode(
        env: Env,
        creator: Address,
        hunt_id: u64,
        mode: DistributionMode,
    ) -> Result<(), RewardErrorCode> {
        let mut config = Self::require_pool_creator(&env, &creator, hunt_id)?;
        Self::require_hunt_editable(&env, hunt_id)?;

        config.distribution_mode = mode;
        Storage::set_pool_config(&env, hunt_id, &config);
        Storage::append_audit_entry(
            &env,
            hunt_id,
            PoolAuditEntry {
                actor: creator,
                operation: PoolOperation::SetDistributionMode,
                timestamp: env.ledger().timestamp(),
                amount: None,
            },
        );
        Ok(())
    }

    /// Sets the vesting period for a pool's XLM rewards (0 = instant payout).
    ///
    /// Fails with `HuntLocked` if the hunt is no longer in Draft status.
    pub fn set_vesting_period_secs(
        env: Env,
        creator: Address,
        hunt_id: u64,
        vesting_period_secs: u64,
    ) -> Result<(), RewardErrorCode> {
        let mut config = Self::require_pool_creator(&env, &creator, hunt_id)?;
        Self::require_hunt_editable(&env, hunt_id)?;

        config.vesting_period_secs = vesting_period_secs;
        Storage::set_pool_config(&env, hunt_id, &config);
        Storage::append_audit_entry(
            &env,
            hunt_id,
            PoolAuditEntry {
                actor: creator,
                operation: PoolOperation::SetVestingPeriod,
                timestamp: env.ledger().timestamp(),
                amount: Some(vesting_period_secs as i128),
            },
        );
        Ok(())
    }

    /// Updates the minimum distribution amount for a pool.
    ///
    /// Fails with `HuntLocked` if the hunt is no longer in Draft status.
    pub fn update_pool_config(
        env: Env,
        creator: Address,
        hunt_id: u64,
        min_distribution_amount: i128,
    ) -> Result<(), RewardErrorCode> {
        let mut config = Self::require_pool_creator(&env, &creator, hunt_id)?;
        Self::require_hunt_editable(&env, hunt_id)?;

        if min_distribution_amount < 0 {
            return Err(RewardErrorCode::InvalidAmount);
        }
        config.min_distribution_amount = min_distribution_amount;
        Storage::set_pool_config(&env, hunt_id, &config);
        Storage::append_audit_entry(
            &env,
            hunt_id,
            PoolAuditEntry {
                actor: creator,
                operation: PoolOperation::UpdateMinAmount,
                timestamp: env.ledger().timestamp(),
                amount: Some(min_distribution_amount),
            },
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Pool funding
    // -----------------------------------------------------------------------

    pub fn fund_reward_pool(
        env: Env,
        funder: Address,
        hunt_id: u64,
        amount: i128,
    ) -> Result<(), RewardErrorCode> {
        funder.require_auth();

        if Storage::is_funding_paused(&env) {
            return Err(RewardErrorCode::FundingPaused);
        }
        if amount <= 0 {
            return Err(RewardErrorCode::InvalidAmount);
        }
        if amount < MIN_FUNDING_AMOUNT {
            return Err(RewardErrorCode::BelowMinimumFunding);
        }
        if amount > MAX_FUNDING_AMOUNT {
            return Err(RewardErrorCode::ExceedsMaximumFunding);
        }

        let config = Storage::get_pool_config(&env, hunt_id)
            .ok_or(RewardErrorCode::PoolNotFound)?;

        let current_balance = Storage::get_pool_balance(&env, hunt_id);
        let new_balance = current_balance
            .checked_add(amount)
            .ok_or(RewardErrorCode::PoolBalanceOverflow)?;
        if new_balance > MAX_POOL_BALANCE {
            return Err(RewardErrorCode::PoolBalanceOverflow);
        }

        // Transfer tokens from funder to this contract.
        let contract_addr = env.current_contract_address();
        token_handler::TokenHandler::transfer_to_contract(
            &env,
            &config.token_address,
            &funder,
            &contract_addr,
            amount,
        )
        .map_err(|_| RewardErrorCode::TransferFailed)?;

        Storage::set_pool_balance(&env, hunt_id, new_balance);
        let total = Storage::get_pool_total_deposited(&env, hunt_id) + amount;
        Storage::set_pool_total_deposited(&env, hunt_id, total);

        // Track funder.
        let mut funders = Storage::get_pool_funders(&env, hunt_id);
        let already_tracked = (0..funders.len()).any(|i| funders.get(i).unwrap() == funder);
        if !already_tracked {
            if funders.len() >= Storage::MAX_DELEGATES_PER_POOL {
                return Err(RewardErrorCode::TooManyFunders);
            }
            funders.push_back(funder.clone());
            Storage::set_pool_funders(&env, hunt_id, &funders);
        }
        let contrib = Storage::get_pool_funder_contribution(&env, hunt_id, &funder) + amount;
        Storage::set_pool_funder_contribution(&env, hunt_id, &funder, contrib);

        Storage::append_audit_entry(
            &env,
            hunt_id,
            PoolAuditEntry {
                actor: funder,
                operation: PoolOperation::Fund,
                timestamp: env.ledger().timestamp(),
                amount: Some(amount),
            },
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Pool distribution
    // -----------------------------------------------------------------------

    pub fn distribute_rewards(
        env: Env,
        hunt_id: u64,
        player: Address,
        config: RewardConfig,
    ) -> Result<(), RewardErrorCode> {
        if Storage::is_distribution_paused(&env) {
            return Err(RewardErrorCode::DistributionPaused);
        }
        if Storage::is_in_distribution(&env) {
            return Err(RewardErrorCode::ReentrancyDetected);
        }

        let pool_config =
            Storage::get_pool_config(&env, hunt_id).ok_or(RewardErrorCode::PoolNotFound)?;

        if pool_config.frozen {
            return Err(RewardErrorCode::PoolFrozen);
        }
        if Storage::is_distributed(&env, hunt_id, &player) {
            return Err(RewardErrorCode::AlreadyDistributed);
        }

        Storage::set_in_distribution(&env, true);

        let xlm_amount = config.xlm_amount.unwrap_or(0);
        if xlm_amount > 0 {
            let balance = Storage::get_pool_balance(&env, hunt_id);
            if balance < xlm_amount {
                Storage::set_in_distribution(&env, false);
                return Err(RewardErrorCode::InsufficientPool);
            }
            if xlm_amount < pool_config.min_distribution_amount
                && pool_config.min_distribution_amount > 0
            {
                Storage::set_in_distribution(&env, false);
                return Err(RewardErrorCode::BelowMinimumAmount);
            }

            let new_balance = balance - xlm_amount;
            Storage::set_pool_balance(&env, hunt_id, new_balance);

            let total_distributed =
                Storage::get_pool_total_distributed(&env, hunt_id) + xlm_amount;
            Storage::set_pool_total_distributed(&env, hunt_id, total_distributed);

            let total_global =
                Storage::get_total_xlm_distributed(&env) + xlm_amount;
            Storage::set_total_xlm_distributed(&env, total_global);

            let contract_addr = env.current_contract_address();
            xlm_handler::XlmHandler::distribute_xlm(
                &env,
                &pool_config.token_address,
                &contract_addr,
                &player,
                xlm_amount,
            );
        }

        Storage::set_distributed(&env, hunt_id, &player);
        Storage::set_distribution_record(
            &env,
            hunt_id,
            &player,
            &DistributionRecord {
                xlm_amount,
                nft_id: None,
            },
        );

        Storage::set_in_distribution(&env, false);

        env.events().publish(
            (Symbol::new(&env, "RewardsDistributed"), hunt_id),
            RewardsDistributedEvent {
                hunt_id,
                player,
                xlm_amount,
                nft_id: None,
            },
        );

        Ok(())
    }

    pub fn distribute_proportional(
        env: Env,
        hunt_id: u64,
        player: Address,
        player_score: u64,
        total_score: u64,
    ) -> Result<(), RewardErrorCode> {
        if Storage::is_distribution_paused(&env) {
            return Err(RewardErrorCode::DistributionPaused);
        }
        if total_score == 0 {
            return Err(RewardErrorCode::InvalidScore);
        }

        let pool_config =
            Storage::get_pool_config(&env, hunt_id).ok_or(RewardErrorCode::PoolNotFound)?;
        let balance = Storage::get_pool_balance(&env, hunt_id);
        let amount = (balance as u128)
            .checked_mul(player_score as u128)
            .and_then(|n| n.checked_div(total_score as u128))
            .unwrap_or(0) as i128;

        let reward = RewardConfig {
            xlm_amount: Some(amount),
            nft_contract: None,
            nft_title: soroban_sdk::String::from_str(&env, ""),
            nft_description: soroban_sdk::String::from_str(&env, ""),
            nft_image_uri: soroban_sdk::String::from_str(&env, ""),
            nft_hunt_title: soroban_sdk::String::from_str(&env, ""),
            nft_rarity: 0,
            nft_tier: 0,
            completion_rank: 0,
        };
        let _ = pool_config;
        Self::distribute_rewards(env, hunt_id, player, reward)
    }

    // -----------------------------------------------------------------------
    // Pool queries
    // -----------------------------------------------------------------------

    pub fn get_pool_config(env: Env, hunt_id: u64) -> Option<RewardPoolConfig> {
        Storage::get_pool_config(&env, hunt_id)
    }

    pub fn get_pool_balance(env: Env, hunt_id: u64) -> i128 {
        Storage::get_pool_balance(&env, hunt_id)
    }

    pub fn get_reward_pool(env: Env, hunt_id: u64) -> Option<RewardPoolStatus> {
        let config = Storage::get_pool_config(&env, hunt_id)?;
        Some(RewardPoolStatus {
            balance: Storage::get_pool_balance(&env, hunt_id),
            total_deposited: Storage::get_pool_total_deposited(&env, hunt_id),
            total_distributed: Storage::get_pool_total_distributed(&env, hunt_id),
            creator: config.creator,
            min_distribution_amount: config.min_distribution_amount,
            frozen: config.frozen,
            frozen_by: config.frozen_by,
        })
    }

    pub fn validate_pool(env: Env, hunt_id: u64, required: i128) -> Option<ValidationResult> {
        let config = Storage::get_pool_config(&env, hunt_id)?;
        let balance = Storage::get_pool_balance(&env, hunt_id);
        let is_valid = balance >= required
            && (config.min_distribution_amount == 0
                || required >= config.min_distribution_amount);
        Some(ValidationResult {
            is_valid,
            balance,
            required,
        })
    }

    pub fn get_pool_audit_log(
        env: Env,
        hunt_id: u64,
        after_index: Option<u64>,
        limit: Option<u32>,
    ) -> AuditPage {
        let total = Storage::get_pool_audit_count(&env, hunt_id);
        let start = after_index.unwrap_or(0);
        let cap = limit
            .unwrap_or(50)
            .min(50)
            .min(Storage::MAX_AUDIT_ENTRIES_PER_POOL as u32);

        let mut entries: Vec<PoolAuditEntry> = Vec::new(&env);
        let mut idx = start;
        while idx < total && entries.len() < cap {
            if let Some(entry) = Storage::get_pool_audit_entry(&env, hunt_id, idx) {
                entries.push_back(entry);
            }
            idx += 1;
        }
        AuditPage { entries, total }
    }

    // -----------------------------------------------------------------------
    // Pool refund
    // -----------------------------------------------------------------------

    pub fn refund_pool(env: Env, caller: Address, hunt_id: u64) -> Result<(), RewardErrorCode> {
        caller.require_auth();
        let config =
            Storage::get_pool_config(&env, hunt_id).ok_or(RewardErrorCode::PoolNotFound)?;
        if config.creator != caller {
            // Also allow admin.
            let admin = Storage::get_admin(&env).ok_or(RewardErrorCode::Unauthorized)?;
            if admin != caller {
                return Err(RewardErrorCode::Unauthorized);
            }
        }
        let balance = Storage::get_pool_balance(&env, hunt_id);
        if balance > 0 {
            let contract_addr = env.current_contract_address();
            xlm_handler::XlmHandler::distribute_xlm(
                &env,
                &config.token_address,
                &contract_addr,
                &caller,
                balance,
            );
            Storage::set_pool_balance(&env, hunt_id, 0);
            let total = Storage::get_pool_total_refunded(&env, hunt_id) + balance;
            Storage::set_pool_total_refunded(&env, hunt_id, total);
        }
        Storage::append_audit_entry(
            &env,
            hunt_id,
            PoolAuditEntry {
                actor: caller,
                operation: PoolOperation::Refund,
                timestamp: env.ledger().timestamp(),
                amount: Some(balance),
            },
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Freeze / unfreeze
    // -----------------------------------------------------------------------

    pub fn freeze_pool(env: Env, caller: Address, hunt_id: u64) -> Result<(), RewardErrorCode> {
        caller.require_auth();
        let mut config =
            Storage::get_pool_config(&env, hunt_id).ok_or(RewardErrorCode::PoolNotFound)?;
        // Creator or admin may freeze.
        if config.creator != caller {
            let admin = Storage::get_admin(&env).ok_or(RewardErrorCode::Unauthorized)?;
            if admin != caller {
                return Err(RewardErrorCode::Unauthorized);
            }
        }
        config.frozen = true;
        config.frozen_by = Some(caller.clone());
        Storage::set_pool_config(&env, hunt_id, &config);
        Storage::append_audit_entry(
            &env,
            hunt_id,
            PoolAuditEntry {
                actor: caller,
                operation: PoolOperation::Freeze,
                timestamp: env.ledger().timestamp(),
                amount: None,
            },
        );
        Ok(())
    }

    pub fn unfreeze_pool(env: Env, caller: Address, hunt_id: u64) -> Result<(), RewardErrorCode> {
        caller.require_auth();
        let mut config =
            Storage::get_pool_config(&env, hunt_id).ok_or(RewardErrorCode::PoolNotFound)?;

        // Only whoever froze it (or admin) may unfreeze.
        let is_admin = Storage::get_admin(&env).map(|a| a == caller).unwrap_or(false);
        let is_freezer = config.frozen_by.as_ref().map(|a| *a == caller).unwrap_or(false);
        if !is_admin && !is_freezer {
            return Err(RewardErrorCode::Unauthorized);
        }
        config.frozen = false;
        config.frozen_by = None;
        Storage::set_pool_config(&env, hunt_id, &config);
        Storage::append_audit_entry(
            &env,
            hunt_id,
            PoolAuditEntry {
                actor: caller,
                operation: PoolOperation::Unfreeze,
                timestamp: env.ledger().timestamp(),
                amount: None,
            },
        );
        Ok(())
    }

    pub fn is_pool_frozen(env: Env, hunt_id: u64) -> bool {
        Storage::get_pool_config(&env, hunt_id)
            .map(|c| c.frozen)
            .unwrap_or(false)
    }

    // -----------------------------------------------------------------------
    // Admin: NFT contract, pause controls, authorized contracts
    // -----------------------------------------------------------------------

    pub fn set_nft_reward_contract(
        env: Env,
        admin: Address,
        nft_contract: Address,
    ) -> Result<(), RewardErrorCode> {
        Self::require_admin(&env, &admin)?;
        Storage::set_nft_contract(&env, &nft_contract);
        Ok(())
    }

    pub fn pause(env: Env, admin: Address) -> Result<(), RewardErrorCode> {
        Self::require_admin(&env, &admin)?;
        Storage::set_paused(&env, true);
        Ok(())
    }

    pub fn unpause(env: Env, admin: Address) -> Result<(), RewardErrorCode> {
        Self::require_admin(&env, &admin)?;
        Storage::set_paused(&env, false);
        Ok(())
    }

    pub fn pause_funding(env: Env, admin: Address) -> Result<(), RewardErrorCode> {
        Self::require_admin(&env, &admin)?;
        Storage::set_funding_paused(&env, true);
        Ok(())
    }

    pub fn unpause_funding(env: Env, admin: Address) -> Result<(), RewardErrorCode> {
        Self::require_admin(&env, &admin)?;
        Storage::set_funding_paused(&env, false);
        Ok(())
    }

    pub fn pause_distribution(env: Env, admin: Address) -> Result<(), RewardErrorCode> {
        Self::require_admin(&env, &admin)?;
        Storage::set_distribution_paused(&env, true);
        Ok(())
    }

    pub fn unpause_distribution(env: Env, admin: Address) -> Result<(), RewardErrorCode> {
        Self::require_admin(&env, &admin)?;
        Storage::set_distribution_paused(&env, false);
        Ok(())
    }

    /// Returns `(global_paused, funding_paused, distribution_paused)`.
    pub fn get_pause_state(env: Env) -> (bool, bool, bool) {
        (
            Storage::is_paused(&env),
            Storage::is_funding_paused(&env),
            Storage::is_distribution_paused(&env),
        )
    }

    /// Returns the two granular pause flags independently of the global stop.
    pub fn get_raw_pause_flags(env: Env) -> (bool, bool) {
        Storage::raw_pause_flags(&env)
    }

    pub fn add_authorized_contract(
        env: Env,
        admin: Address,
        contract: Address,
    ) -> Result<(), RewardErrorCode> {
        Self::require_admin(&env, &admin)?;
        Storage::add_authorized_contract(&env, &contract);
        Ok(())
    }

    pub fn remove_authorized_contract(
        env: Env,
        admin: Address,
        contract: Address,
    ) -> Result<(), RewardErrorCode> {
        Self::require_admin(&env, &admin)?;
        Storage::remove_authorized_contract(&env, &contract);
        Ok(())
    }

    pub fn add_delegate(
        env: Env,
        creator: Address,
        hunt_id: u64,
        delegate: Address,
    ) -> Result<(), RewardErrorCode> {
        let mut config = Self::require_pool_creator(&env, &creator, hunt_id)?;
        if config.delegates.len() >= Storage::MAX_DELEGATES_PER_POOL {
            return Err(RewardErrorCode::TooManyDelegates);
        }
        config.delegates.push_back(delegate.clone());
        Storage::set_pool_config(&env, hunt_id, &config);
        Storage::increment_pool_delegate_count(&env, hunt_id);
        Storage::append_audit_entry(
            &env,
            hunt_id,
            PoolAuditEntry {
                actor: creator,
                operation: PoolOperation::AddDelegate,
                timestamp: env.ledger().timestamp(),
                amount: None,
            },
        );
        Ok(())
    }

    pub fn remove_delegate(
        env: Env,
        creator: Address,
        hunt_id: u64,
        delegate: Address,
    ) -> Result<(), RewardErrorCode> {
        let mut config = Self::require_pool_creator(&env, &creator, hunt_id)?;
        let mut new_delegates: Vec<Address> = Vec::new(&env);
        for i in 0..config.delegates.len() {
            let d = config.delegates.get(i).unwrap();
            if d != delegate {
                new_delegates.push_back(d);
            }
        }
        config.delegates = new_delegates;
        Storage::set_pool_config(&env, hunt_id, &config);
        Storage::append_audit_entry(
            &env,
            hunt_id,
            PoolAuditEntry {
                actor: creator,
                operation: PoolOperation::RemoveDelegate,
                timestamp: env.ledger().timestamp(),
                amount: None,
            },
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // NFT retry
    // -----------------------------------------------------------------------

    pub fn retry_failed_nft_mint(
        env: Env,
        admin: Address,
        hunt_id: u64,
        player: Address,
    ) -> Result<(), RewardErrorCode> {
        Self::require_admin(&env, &admin)?;
        let pending = Storage::get_pending_nft_mint(&env, hunt_id, &player)
            .ok_or(RewardErrorCode::NftMintPendingNotFound)?;
        nft_handler::NftHandler::mint_reward_nft(
            &env,
            &pending.nft_contract,
            &player,
            &pending.nft_title,
            &pending.nft_description,
            &pending.nft_image_uri,
            pending.nft_rarity,
            pending.nft_tier,
            hunt_id,
            pending.completion_rank,
        )
        .map_err(|_| RewardErrorCode::NftMintFailed)?;
        Storage::remove_pending_nft_mint(&env, hunt_id, &player);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Pool statistics
    // -----------------------------------------------------------------------

    pub fn get_pool_statistics(env: Env, hunt_id: u64) -> Option<RewardPoolStatistics> {
        Storage::get_pool_config(&env, hunt_id)?;
        let total_funded = Storage::get_pool_total_deposited(&env, hunt_id);
        let total_distributed = Storage::get_pool_total_distributed(&env, hunt_id);
        let distribution_count = Storage::get_pool_distribution_count(&env, hunt_id);
        let avg_distribution = if distribution_count == 0 {
            0
        } else {
            total_distributed / distribution_count as i128
        };
        let last_distribution_timestamp =
            Storage::get_pool_last_distribution_timestamp(&env, hunt_id);
        Some(RewardPoolStatistics {
            total_funded,
            total_distributed,
            distribution_count,
            avg_distribution,
            last_distribution_timestamp,
        })
    }

    pub fn get_pool_distributions(
        env: Env,
        hunt_id: u64,
        offset: u32,
        limit: u32,
    ) -> Vec<PoolDistribution> {
        Storage::get_pool_distributions(&env, hunt_id, offset, limit)
    }

    // -----------------------------------------------------------------------
    // Rank-tier helper (used in tests via apply_rank_tier)
    // -----------------------------------------------------------------------

    /// Replaces `reward_config.xlm_amount` with the configured rank-tier
    /// amount when the player's `completion_rank` matches an entry.
    pub fn apply_rank_tier(pool_config: &RewardPoolConfig, reward_config: &mut RewardConfig) {
        if let Some(amount) =
            resolve_rank_tier_amount(&pool_config.rank_based_tiers, reward_config.completion_rank)
        {
            reward_config.xlm_amount = Some(amount);
        }
    }

    // -----------------------------------------------------------------------
    // Admin withdraw
    // -----------------------------------------------------------------------

    pub fn admin_withdraw_unclaimed(
        env: Env,
        admin: Address,
        hunt_id: u64,
        amount: i128,
    ) -> Result<(), RewardErrorCode> {
        Self::require_admin(&env, &admin)?;
        let config =
            Storage::get_pool_config(&env, hunt_id).ok_or(RewardErrorCode::PoolNotFound)?;
        if amount < 0 {
            return Err(RewardErrorCode::InvalidAmount);
        }
        let balance = Storage::get_pool_balance(&env, hunt_id);
        if balance < amount {
            return Err(RewardErrorCode::InsufficientPool);
        }
        let contract_addr = env.current_contract_address();
        xlm_handler::XlmHandler::distribute_xlm(
            &env,
            &config.token_address,
            &contract_addr,
            &admin,
            amount,
        );
        Storage::set_pool_balance(&env, hunt_id, balance - amount);
        Storage::append_audit_entry(
            &env,
            hunt_id,
            PoolAuditEntry {
                actor: admin,
                operation: PoolOperation::Withdraw,
                timestamp: env.ledger().timestamp(),
                amount: Some(amount),
            },
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Schema migration
    // -----------------------------------------------------------------------

    pub fn get_schema_version(env: Env) -> u32 {
        migration::RewardManagerMigration::get_schema_version(&env)
    }
}

// ---------------------------------------------------------------------------
// Auxiliary return type for get_pool_audit_log
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditPage {
    pub entries: Vec<PoolAuditEntry>,
    pub total: u64,
}

// ---------------------------------------------------------------------------
// Test modules
// ---------------------------------------------------------------------------

#[cfg(test)]
mod test;
