# fix(reward-manager): enforce admin authority over admin-issued pool freezes

Closes #1077

## Problem

`freeze_pool` and `unfreeze_pool` both accepted "creator or admin" with no record
of **who** issued the freeze. During an incident an admin could freeze a pool, and
the pool creator could immediately unfreeze it — silently disabling the emergency
control. There was also no way to tell an incident freeze apart from a routine
creator freeze, so the two could never be treated differently.

The core enforcement already landed on `main` via #1139: `RewardPoolConfig::frozen_by`
records the freezer (mirrored in `reward-interface` and exposed on `get_reward_pool`),
and `unfreeze_pool` requires the admin when the freeze was not issued by the creator.

This PR closes the one remaining hole in that enforcement and makes the
regression suite actually execute.

## Changes

### 1. Fail closed on an unattributed freeze (`contracts/reward-manager/src/lib.rs`)

The old check inferred "admin freeze" as `frozen_by != creator`, defaulting to
`false` when `frozen_by` was `None`. A pool that is **frozen but carries no recorded
freezer** (freeze state written before `frozen_by` existed) therefore looked like a
creator freeze, and the creator could lift it.

```rust
let frozen_by_creator = config
    .frozen_by
    .as_ref()
    .map(|freezer| freezer == &config.creator)
    .unwrap_or(false);
// An unattributed freeze cannot be proven to be a creator freeze → admin only.
let admin_freeze = config.frozen && !frozen_by_creator;
if admin_freeze && !is_admin {
    return Err(RewardErrorCode::Unauthorized);
}
```

`freeze_pool` always records the caller, so this path only matters for legacy /
unmigrated freeze state. A pool that is **not** frozen is unaffected: both parties
can still call `unfreeze_pool` as a no-op.

| Freeze state | Creator may unfreeze | Admin may unfreeze |
| --- | --- | --- |
| Creator freeze (`frozen_by = creator`) | yes | yes |
| Admin freeze (`frozen_by = admin`) | **no** (`Unauthorized`) | yes |
| Unattributed (`frozen = true`, `frozen_by = None`) | **no** (`Unauthorized`) | yes |

### 2. Fix the `pool_freeze_authority` test fixture (it never ran)

The fixture registered the contract as `env.register(RewardManager, ())`, but
`RewardManager::__constructor` takes `(admin, xlm_token, hunty_core)`. Every case in
the target panicked with `invalid number of input arguments: 3 expected, got 0`
before exercising any freeze logic — i.e. the merged #1077 tests were dead. The
fixture now passes the constructor arguments (matching `tests/tier_length_cap.rs`),
and a regression test covers the unattributed-freeze case.

### 3. Unblock the build on `main`

`main` does not currently compile, so every Rust CI job is red for reasons unrelated
to this change. Fixed here:

- **`hunty-core`**: commit `5c4311d` overwrote `list_clues_for_hunt` with a
  non-compiling stub; the original function is restored.
- **`reward-manager`**: `distribute_rewards_legacy` and `distribute_proportional`
  still called the old 4-argument `distribute_rewards` (now takes an explicit
  `caller`); both now call `distribute_rewards_impl`, matching the existing
  `distribute_batch` legacy pattern.
- **clippy**: `MIN_INVITE_CODE_LENGTH` is now enforced (its documented purpose), and
  the ignored `Storage::add_co_creator` result is propagated.
- **reward-manager tests**: fixtures migrated to the 3-argument `__constructor`, and
  test call sites updated to the current distribution entrypoint.
- **formatting**: `cargo fmt --all` drift cleaned up.

### 4. Docs and CI maintenance

- Regenerated `docs/contract-api.md`.
- Documented the four missing storage keys (`ATTEMPT_KEY`, `RATE_LIMIT_KEY`,
  `PENDING_NFT_LIST_KEY`, `POOL_MIG_KEY`) in `docs/STORAGE_KEYS.md`.
- Updated `EXPECTED_FUNCTIONS` in `scripts/check_wasm_abi.py` for the hunty-core and
  reward-manager functions that were added since the list was last updated.
- `npm audit fix` for the moderate `ip-address` advisory (`npm audit` gate).

## Verification

- `cargo fmt --all -- --check` — clean.
- `cargo clippy --workspace -- -D warnings` — clean.
- `cargo test -p reward-manager --test pool_freeze_authority` — **6 passed**,
  including `unattributed_freeze_cannot_be_lifted_by_creator`.
- `bash scripts/ci/check_storage_keys_doc.sh` — all 85 keys documented.
- `python3 scripts/generate_api_docs.py` — no diff.

## Out of scope (pre-existing, blocks the remaining green checks)

- `nft-reward`: commit `b2d8eaa` (#1094) deleted 1,623 lines of
  `contracts/nft-reward/src/lib.rs`, removing ~35 exported functions (`transfer_nft`,
  `owner_of`, `burn_nft`, `list_all_nfts`, …). Restoring it is a separate repair and
  is **not** papered over by editing the ABI expectation list.
- The `reward-manager` unit-test suite still has pre-existing failures from the
  `__constructor` (#1076) and distribution-signature (#1061) migrations that predate
  this PR. This PR makes the suite compile and run again; finishing the fixture
  migration belongs in its own change.
