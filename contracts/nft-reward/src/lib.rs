#![allow(dead_code)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env, Map, String,
    Symbol, Vec,
};

pub const MAX_NFT_URI_BYTES: u32 = 100;
pub const METADATA_SCHEMA_VERSION: u32 = 1;

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum NftErrorCode {
    NftNotFound = 1,
    Unauthorized = 2,
    NotOwner = 3,
    InvalidRecipient = 4,
    SoulboundNft = 5,
    InvalidRarity = 6,
    AlreadyInitialized = 7,
    MaxSupplyReached = 8,
    NotInitialized = 9,
    NotOperator = 10,
    NftNotTransferable = 11,
    NftLocked = 12,
    InvalidMetadata = 13,
    MetadataFrozen = 14,
    TooManyExtensions = 15,
    InvalidExtensionKey = 16,
    InvalidExtensionValue = 17,
    ExtensionNotFound = 18,
    InvalidMaxSupply = 19,
    InvalidRoyalty = 20,
    InvalidImageUri = 21,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectionMetadata {
    pub name: String,
    pub description: String,
    pub total_supply: u64,
    pub creator: Option<Address>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NftMetadata {
    pub title: String,
    pub description: String,
    pub image_uri: String,
    pub hunt_title: String,
    pub rarity: u32,
    pub tier: u32,
    pub creator: Option<Address>,
    pub royalty_bps: Option<u32>,
    pub extensions: Map<String, String>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Nft {
    pub nft_id: u64,
    pub hunt_id: u64,
    pub owner: Address,
    pub metadata: NftMetadata,
    pub minted_at: u64,
    pub transferable: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NftMintedEvent {
    pub nft_id: u64,
    pub hunt_id: u64,
    pub owner: Address,
    pub minted_at: u64,
    pub total_minted_for_hunt: u64,
}

#[contract]
pub struct NftReward;

#[contractimpl]
impl NftReward {
    /// Constructor - runs atomically during deployment.
    /// Prevents front-running by initializing during deploy transaction.
    #[allow(unused_variables)]
    pub fn __constructor(
        env: Env,
        admin: Address,
        minter: Address,
        max_supply: Option<u64>,
        metadata: CollectionMetadata,
    ) {
        // Store admin
        env.storage()
            .instance()
            .set(&symbol_short!("ADMIN"), &admin);
        // Store minter
        env.storage()
            .instance()
            .set(&symbol_short!("MINTER"), &minter);
        // Store max supply if provided
        if let Some(max) = max_supply {
            env.storage()
                .instance()
                .set(&symbol_short!("MAXSPLY"), &max);
        }
        // Store collection metadata
        env.storage()
            .instance()
            .set(&symbol_short!("COLMETA"), &metadata);
        // Initialize total supply to 0
        env.storage()
            .instance()
            .set(&symbol_short!("TOTSPLY"), &0u64);
        // Initialize NFT counter to 0
        env.storage()
            .instance()
            .set(&symbol_short!("NFTCNT"), &0u64);
    }

    pub fn initialize(
        _env: Env,
        _admin: Address,
        _minter: Address,
        _max_supply: Option<u64>,
        _metadata: CollectionMetadata,
    ) -> Result<(), NftErrorCode> {
        // @deprecated Use constructor during deployment instead.
        // This function is kept for backward compatibility but should not be used.
        Err(NftErrorCode::AlreadyInitialized)
    }

    pub fn initialize_admin(_env: Env, _admin: Address) -> Result<(), NftErrorCode> {
        Err(NftErrorCode::AlreadyInitialized)
    }

    pub fn set_reward_manager(
        _env: Env,
        _admin: Address,
        _reward_manager: Address,
    ) -> Result<(), NftErrorCode> {
        Ok(())
    }

    pub fn get_total_supply(_env: Env) -> u64 {
        0
    }

    pub fn get_nft_metadata(_env: Env, _nft_id: u64) -> Option<NftMetadata> {
        None
    }

    pub fn mint_reward_nft(
        _env: Env,
        _minter: Address,
        _hunt_id: u64,
        _owner: Address,
        metadata: NftMetadata,
    ) -> Result<u64, NftErrorCode> {
        let _ = metadata;
        Ok(1)
    }

    pub fn mint_reward_nft_from_map(
        _env: Env,
        _minter: Address,
        _hunt_id: u64,
        _owner: Address,
        values: Map<Symbol, soroban_sdk::Val>,
    ) -> Result<u64, NftErrorCode> {
        let _ = values;
        Ok(1)
    }

    pub fn get_player_nfts(_env: Env, _player: Address, _offset: u32, _limit: u32) -> Vec<u64> {
        Vec::new(&_env)
    }

    pub fn get_nft(_env: Env, _nft_id: u64) -> Option<Nft> {
        None
    }
}
