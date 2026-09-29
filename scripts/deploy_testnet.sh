#!/usr/bin/env bash
# deploy_testnet.sh — Automated testnet deployment for Hunty contracts
#
# USAGE:
#   [TESTNET_ADMIN_ADDRESS=G...] \
#   bash scripts/deploy_testnet.sh

set -euo pipefail

# ── Configuration ────────────────────────────────────────────────────────────
NETWORK_PASSPHRASE="Test SDF Network ; September 2015"
RPC_URL="https://soroban-testnet.stellar.org"
DEPLOY_DIR="docs/deployment-testnet"
ADDRESS_FILE="${DEPLOY_DIR}/deployed-addresses.md"
CONTRACTS=("nft-reward" "reward-manager" "hunty-core")
WASM_NAMES=("nft_reward" "reward_manager" "hunty_core")

# ── Helpers ──────────────────────────────────────────────────────────────────
log()  { echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] $*"; }
die()  { echo "ERROR: $*" >&2; exit 1; }

require_cmd() { command -v "$1" &>/dev/null || die "'$1' not found in PATH"; }
require_cmd stellar
require_cmd jq

# Linux provides sha256sum; macOS provides `shasum -a 256`.
sha256_file() {
  local file="$1"
  if command -v sha256sum &>/dev/null; then
    sha256sum "$file" | awk '{print $1}'
  elif command -v shasum &>/dev/null; then
    shasum -a 256 "$file" | awk '{print $1}'
  else
    die "Neither sha256sum nor shasum found in PATH (install coreutils or use macOS shasum)"
  fi
}

mkdir -p "$DEPLOY_DIR"

# ── Step 1: Set up deployer keys ─────────────────────────────────────────────
log "=== Hunty Testnet Deployment ==="

log "Validating config placeholders..."
bash scripts/validate_placeholders.sh

# Check if deployer key exists, if not generate it (which automatically funds it)
if ! stellar keys address deployer &>/dev/null; then
  log "Generating and funding testnet deployer key..."
  stellar keys generate --network testnet deployer
else
  log "Using existing deployer key."
fi

DEPLOYER_ADDRESS=$(stellar keys address deployer)
ADMIN_ADDRESS="${TESTNET_ADMIN_ADDRESS:-$DEPLOYER_ADDRESS}"

log "Deployer Address : $DEPLOYER_ADDRESS"
log "Admin Address    : $ADMIN_ADDRESS"
log "RPC URL         : $RPC_URL"

# ── Step 2: Build WASM ───────────────────────────────────────────────────────
log "Building contracts..."
stellar contract build

# Handle differences in build target directory output
if [ -d "target/wasm32v1-none/release" ]; then
  WASM_DIR="target/wasm32v1-none/release"
else
  WASM_DIR="target/wasm32v1-none/release"
fi
log "Using WASM directory: $WASM_DIR"

# Record WASM hashes
log "WASM artefact hashes:"
MANIFEST_FILE="${DEPLOY_DIR}/manifest.txt"
: > "$MANIFEST_FILE"
for wasm_name in "${WASM_NAMES[@]}"; do
  wasm_path="${WASM_DIR}/${wasm_name}.wasm"
  [[ -f "$wasm_path" ]] || die "WASM not found: $wasm_path"
  hash=$(sha256_file "$wasm_path")
  echo "  ${wasm_name}.wasm  sha256:${hash}"
  echo "${wasm_name}.wasm  sha256:${hash}" >> "$MANIFEST_FILE"
done

# ── Step 3: Query Native SAC Address ─────────────────────────────────────────
log "Querying native XLM token address on testnet..."
XLM_TOKEN_ADDRESS=$(stellar contract id asset --asset native --network testnet)
log "Native XLM token address: $XLM_TOKEN_ADDRESS"

# ── Step 4: Deploy contracts with constructor args (dependency order) ──────────
declare -A NEW_IDS

# Deploy hunty-core first (no constructor args)
log "Deploying hunty-core..."
wasm_path="${WASM_DIR}/hunty_core.wasm"
wasm_hash=$(stellar contract upload \
  --wasm "$wasm_path" \
  --rpc-url "$RPC_URL" \
  --network-passphrase "$NETWORK_PASSPHRASE" \
  --source deployer)
log "  WASM uploaded: $wasm_hash"

contract_id=$(stellar contract deploy \
  --wasm-hash "$wasm_hash" \
  --rpc-url "$RPC_URL" \
  --network-passphrase "$NETWORK_PASSPHRASE" \
  --source deployer)
log "  Contract ID  : $contract_id"
NEW_IDS["hunty-core"]="$contract_id"

# Deploy reward-manager with constructor args: admin, xlm_token, hunty_core
log "Deploying reward-manager..."
wasm_path="${WASM_DIR}/reward_manager.wasm"
wasm_hash=$(stellar contract upload \
  --wasm "$wasm_path" \
  --rpc-url "$RPC_URL" \
  --network-passphrase "$NETWORK_PASSPHRASE" \
  --source deployer)
log "  WASM uploaded: $wasm_hash"

contract_id=$(stellar contract deploy \
  --wasm-hash "$wasm_hash" \
  --rpc-url "$RPC_URL" \
  --network-passphrase "$NETWORK_PASSPHRASE" \
  --source deployer \
  -- "$ADMIN_ADDRESS" "$XLM_TOKEN_ADDRESS" "${NEW_IDS[hunty-core]}")
log "  Contract ID  : $contract_id"
NEW_IDS["reward-manager"]="$contract_id"

# Deploy nft-reward with constructor args: admin, minter, max_supply, metadata
log "Deploying nft-reward..."
wasm_path="${WASM_DIR}/nft_reward.wasm"
wasm_hash=$(stellar contract upload \
  --wasm "$wasm_path" \
  --rpc-url "$RPC_URL" \
  --network-passphrase "$NETWORK_PASSPHRASE" \
  --source deployer)
log "  WASM uploaded: $wasm_hash"

NFT_METADATA="{\"name\":\"Hunty NFT Reward\",\"description\":\"Reward NFTs for completed hunts\",\"total_supply\":0,\"creator\":\"$ADMIN_ADDRESS\"}"
contract_id=$(stellar contract deploy \
  --wasm-hash "$wasm_hash" \
  --rpc-url "$RPC_URL" \
  --network-passphrase "$NETWORK_PASSPHRASE" \
  --source deployer \
  -- "$ADMIN_ADDRESS" "$ADMIN_ADDRESS" "0" "$NFT_METADATA")
log "  Contract ID  : $contract_id"
NEW_IDS["nft-reward"]="$contract_id"

# ── Step 5: Link contracts (no separate initialize needed - done in constructor) ─
log "Linking contracts..."

# 1. Link nft-reward to reward-manager
stellar contract invoke \
  --id "${NEW_IDS[reward-manager]}" \
  --rpc-url "$RPC_URL" \
  --network-passphrase "$NETWORK_PASSPHRASE" \
  --source deployer \
  -- set_nft_reward_contract \
  --admin "$ADMIN_ADDRESS" \
  --nft_contract "${NEW_IDS[nft-reward]}"
log "  nft-reward linked to reward-manager."

# 2. Register reward-manager as an authorized minter on nft-reward
stellar contract invoke \
  --id "${NEW_IDS[nft-reward]}" \
  --rpc-url "$RPC_URL" \
  --network-passphrase "$NETWORK_PASSPHRASE" \
  --source deployer \
  -- add_authorized_contract \
  --admin "$ADMIN_ADDRESS" \
  --contract "${NEW_IDS[reward-manager]}"
log "  reward-manager registered as minter on nft-reward."

# 3. Initialize hunty-core (still needs initialize_admin)
stellar contract invoke \
  --id "${NEW_IDS[hunty-core]}" \
  --rpc-url "$RPC_URL" \
  --network-passphrase "$NETWORK_PASSPHRASE" \
  --source deployer \
  -- initialize_admin \
  --admin "$ADMIN_ADDRESS"
log "  hunty-core initialized."

# 4. Link reward-manager to hunty-core
stellar contract invoke \
  --id "${NEW_IDS[hunty-core]}" \
  --rpc-url "$RPC_URL" \
  --network-passphrase "$NETWORK_PASSPHRASE" \
  --source deployer \
  -- set_reward_manager \
  --admin "$ADMIN_ADDRESS" \
  --reward_manager "${NEW_IDS[reward-manager]}"
log "  reward-manager linked to hunty-core."

# 5. Register HuntyCore as a distributor on reward-manager
stellar contract invoke \
  --id "${NEW_IDS[reward-manager]}" \
  --rpc-url "$RPC_URL" \
  --network-passphrase "$NETWORK_PASSPHRASE" \
  --source deployer \
  -- add_authorized_contract \
  --admin "$ADMIN_ADDRESS" \
  --contract "${NEW_IDS[hunty-core]}"
log "  hunty-core registered as distributor on reward-manager."

# ── Step 6: Write deployed addresses ──────────────────────────────────────────
TIMESTAMP=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
cat >> "$ADDRESS_FILE" <<EOF

## Deployment — $TIMESTAMP

| Contract | ID |
|---|---|
| nft-reward | ${NEW_IDS[nft-reward]} |
| reward-manager | ${NEW_IDS[reward-manager]} |
| hunty-core | ${NEW_IDS[hunty-core]} |

WASM manifest: $MANIFEST_FILE
EOF

log ""
log "=== Testnet Deployment Complete ==="
log "Addresses saved to $ADDRESS_FILE"

# ── Step 7: Verify deployment ─────────────────────────────────────────────────
log "Running deployment verification..."
bash scripts/verify_deployment.sh testnet
log "Deployment verified."