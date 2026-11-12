# R2b cleanups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Clear the "R2b minors" of `TODO.md` so R3 (`2026-11-12-r3-multi-host.md`) starts from a smaller `world_manager.rs`, a production loop without test hooks, one Postgres test helper, a coordinator state format that can change, and a login link that neither nags nor keeps dead rows.

**Architecture:** Five independent tasks, no behaviour change except where a task says so (Task 4: kicks of ended sessions are dropped, dial failures log once a minute; Task 5: a `jwt` host without a service token refuses to start unless told otherwise; an older coordinator state is migrated instead of refused).

**Tech Stack:** Rust (dated nightly), sqlx 0.8, quinn (`svc-link`), `just`.

**Spec:** `TODO.md` line "R2b minors (deferred)"; handoff `docs/superpowers/plans/2026-11-11-r3-and-leftovers-planning-handoff.md` §5; the R2b plan `docs/superpowers/plans/2026-11-07-r2b-role-split.md`.

**Run this plan before R3.** R3 edits `world_manager.rs` in almost every task (Task 1 gives it a directory to add files to), its coordinator tests need the hooks behind the feature (Task 2), and it adds two more Postgres test files (Task 3's helper).

## Verified against the code on 2026-11-12 (`cc728a58`) — two items of the handoff are already partly done

- **"Unusable `host_link` pin + pending kicks warns every 2 s" is fixed** (commit `0104a406`, `crates/login/src/host_link.rs` `kick_blocked`: one warning per change of reason). What is left: a pin that parses but is wrong makes `svc-link`'s `dial_loop` log `"[svc-link] dial {addr} failed"` on every attempt (every ≤ 5 s, for ever), and `pending_kicks` rows never expire. Task 4 covers both.
- **The test hooks are not in `world_manager.rs`.** `crash_before_claim_reply`, `with_core` and `inspect` live in `crates/server/src/coordinator_rt.rs`, gated only by `#[doc(hidden)]`; the coordinator's `run` loop polls them every pass.
- "Coordinator envelope version mismatch is fatal" means the format byte of the saved `CoordinatorState` (`crates/game/src/coordinator/state.rs`, `COORDINATOR_STATE_VERSION = 1`): any other value refuses the start. No other "envelope" exists in the code.
- `world_manager.rs` is 4,314 lines; there is no `crates/game/src/world_manager/` directory yet.
- The four helper copies: `crates/world-store/tests/common/mod.rs` (`PgTestDb`, async), `crates/login/tests/common/mod.rs` (identical copy), `crates/server/tests/store_world.rs` (`PgDb`, sync, `drop_db`), `crates/server/tests/roles.rs` (`PgDb`, sync, `Drop`).

## Decisions settled in planning

1. **`jwt` host without a service token: fail closed.** Today it warns and admits tickets by newest epoch, and the login service's `Kick` (logout everywhere, password reset) never reaches it. A host that silently cannot be told to drop a revoked session is a production hazard, and the only user of the fallback is `just dev-login`. So: such a host refuses to start unless `VOXEL_JWT_ADMISSION=newest` is set; `dev-login` sets it. (Alternative kept out: leave the WARN. It costs nothing today, but R3 makes the login link the only placement path, so a host without it is never right outside `dev-login`.)
2. **Coordinator state of an older format: migrate; of a newer or unknown format: still fatal.** The state holds the world's people, claims and answers, so "never guess" stays the rule for anything this build does not know. An older format is known, so the decoder becomes a ladder (`v1 → v2 → … → current`). Nothing bumps the format yet (R3 as planned keeps its new coordinator state in memory, rebuilt by every `Attach`); the ladder and a committed v1 fixture mean the first change that does bump it cannot strand every saved world.
3. **Test hooks behind a cargo feature** (`test-hooks` on `server`), with `required-features` on the two test targets that use them. A self dev-dependency with the feature would be shorter, but the workspace unifies features (`CLAUDE.md` "Compile time"), which would switch the hooks on in every workspace build.
4. **Expiry of a pending kick: when its session ends, or after 24 h.** A kick only matters while the session it names can still be connected. The host reports `SessionEnded`, so that is the exact signal; the age limit covers a host that never comes back.

## Global Constraints

- Work on `develop` in the main checkout. **Never create a git worktree** (CLAUDE.md), including subagents. The tree holds the user's uncommitted work: `git add` only a task's own files, never `git add -A`; `CLAUDE.md` / `justfile` / `TODO.md` hunks via index-only staging (HEAD copy + hunk → `git update-index --cacheinfo`). Run `git status` before every commit.
- `game` stays Bevy-free, IO-free, sync, wasm-safe. The client never depends on `world-store`, `sqlx`, `svc-link` or the new test-support crate.
- The workspace's only rustls crypto provider is **ring**: `cargo tree -i aws-lc-rs` must match nothing after Task 3.
- No compile-time database (sqlx runtime queries only).
- Verification: `just test-one <crate> "<args>"` for the touched tests, `just smoke` before each commit, `just test-threads` after Task 1, `just test-r2` after Tasks 2, 3 and 5 (needs `just pg-up`). Never whole suites. Client test binaries are an explicit opt-in (this plan needs none). Tests slower than 100 ms get `#[ignore = "slow: >100ms — run via just test-full"]`.
- Keep the justfile in sync with every changed test command (Task 2 changes `test-r2`).
- Tunables are named constants.

## Review Focus

1. **A dev world saved before this plan starts after it** (coordinator state format 1 on disk, the ladder in the binary): it loads, with the same history, claims and memo — Task 5 `state::v1_blob_still_decodes`.
2. **`just test-full` / a bare `cargo test -p server` after the feature gate:** the `roles` and `coordinator_rt` targets are skipped by cargo when the feature is off, silently. Expected: every recipe that ran them still runs them — Task 2, step 4 (the recipe list is checked by `grep`).
3. **`just dev-login` after the fail-closed change:** it still starts (it sets `VOXEL_JWT_ADMISSION=newest`); a `jwt` host started by hand without a token exits with a message naming both ways out — Task 5 `jwt_admission::…`.
4. **A kick queued for a host that is down for a day:** it is dropped with one WARN naming the world, the player token and the age; a kick whose session ended is dropped silently — Task 4.
5. **The move of the world-link half changes behaviour by accident** (a method made `pub(super)` shadows nothing, but a moved `use` can change which trait method resolves): `git diff --color-moved=zebra` shows only moved lines plus visibility edits, and the seeded seam tests pass unchanged — Task 1.

---

## File structure

```
crates/game/src/world_manager.rs              → crates/game/src/world_manager/mod.rs   (git mv)
crates/game/src/world_manager/world_link.rs   NEW  the world-link half (pure move)
crates/server/Cargo.toml                      MOD  feature test-hooks; required-features on roles, coordinator_rt
crates/server/src/coordinator_rt.rs           MOD  hooks behind the feature
crates/pg-test-support/                       NEW  PgTestDb (one copy)
crates/{world-store,login}/tests/common/mod.rs, crates/server/tests/{store_world,roles}.rs   MOD  use it
crates/login/src/{store.rs,host_link.rs}      MOD  kick expiry
crates/svc-link/src/lib.rs                    MOD  dial-failure log rate
crates/game/src/coordinator/state.rs          MOD  decode ladder
crates/server/src/{lib.rs,login_link.rs}      MOD  VOXEL_JWT_ADMISSION
justfile, TODO.md, CLAUDE.md                  MOD
```

---

### Task 1: Split `world_manager.rs` — the world-link half moves to `world_manager/world_link.rs`

**Files:**
- Move: `crates/game/src/world_manager.rs` → `crates/game/src/world_manager/mod.rs` (`git mv`, so history follows)
- Create: `crates/game/src/world_manager/world_link.rs`
- Test: none new — the existing `crates/game/tests/{world_manager.rs, coordinator_lifecycle.rs, coordinator_requests.rs, handoff_schedule.rs}` are the proof.

**Interfaces:**
- Produces: no public name changes. `game::WorldManager`, `game::WorldLinkCounters`, `game::WORLD_REQUEST_TIMEOUT_SECS`, `game::ATTACH_WAIT_MS` keep their paths (`mod.rs` re-exports `pub use world_link::{…}` for the public items that moved; `game/src/lib.rs` globs the crate root, so nothing else changes).
- What moves (a second `impl<S: RegionSpawner> WorldManager<S>` block in `world_link.rs`). Names, not line numbers — the lines drift:
  - items: `WORLD_REQUEST_TIMEOUT_SECS`, `ATTACH_WAIT_MS`, `PendingRequest`, `Listed`, `Moved`, `exact_across_seams`, `Step`, `record_move`, `WorldAttach`, `WorldLinkCounters`, `WorldViewCache`, and the test module `attach_lists_tests`;
  - link setup and accessors: `set_world_link`, `calendar_now`, `world_link`, `world_link_mut`, `world_link_counters`, `pending_world_requests`, `world_outbox_len`, `world_attached`, `set_world_request_timeout_ms`, `world_sites`, `world_civs`, `world_host`, `claim_of_client`;
  - attach: `world_link_up`, `note_listed`, `warn_attach_stragglers`, `note_moved`, `try_finish_attach`;
  - link I/O: `link_send`, `watch_world_link`, `pump_world`, `flush_world`, `world_send`, `apply_world_msg`, `route_notices`;
  - lifecycle: `world_region_started`, `world_region_parked`, `history_region_lost`, `world_region_stopping`;
  - claims and requests: `send_fortress`, `embark_result`, `embark`, `retire`, `return_to`, `adventure_result`, `start_adventure`, `end_adventure`, `release_claim_of`, `world_request`, `answer_unavailable`, `expire_world_requests`, `world_reply`, `world_player_online`, `claim_pending`, `send_claims`.
- What stays in `mod.rs`: the struct (all fields, including the world-link-only ones — they get `pub(super)`), `bundle_at` (Departures use it too), and every method that mixes core work with a world-link call (`handle_server_event`, `region_output`, `maintain`, `ensure_running`, `send_to_region`, `client_connected`, `client_disconnected`, `restore_player`, `park_settled`, `move_client`, `finish_move`).
- Visibility: a moved method the core calls becomes `pub(super)`; a core method the moved half calls becomes `pub(super)`; nothing becomes `pub` that was not.

- [ ] **Step 1: Record the baseline.** Run `just test-one game "--test it -- world_manager:: coordinator:: coordinator_lifecycle:: coordinator_requests::"` and `just test-threads`; note the pass counts.
- [ ] **Step 2: Move.** `git mv` the file, create `world_link.rs`, move the items above verbatim, fix `use`s and visibility only. No renames, no reordering inside a moved item, no drive-by edits.
- [ ] **Step 3: Check it is a pure move.** `git diff --color-moved=zebra --color-moved-ws=ignore-all-space HEAD -- crates/game/src/world_manager` — every non-moved line is a `use`, a `mod`, a `pub use`, an `impl` header or a visibility keyword. `wc -l crates/game/src/world_manager/*.rs`: `mod.rs` under 3,300 lines.
- [ ] **Step 4: Run** the Step 1 commands (same pass counts) and `just smoke` — pass.
- [ ] **Step 5: Commit** `refactor(game): world-link half of WorldManager moves to world_manager/world_link.rs` (then `TODO.md`'s minors line loses the item, index-only).

### Task 2: Test hooks out of the coordinator's production loop

**Files:**
- Modify: `crates/server/Cargo.toml` (`[features] test-hooks = []`; `required-features = ["test-hooks"]` on the `roles` and `coordinator_rt` `[[test]]` targets), `crates/server/src/coordinator_rt.rs` (`CoordinatorThread::{crash_before_claim_reply, inspect, with_core}`, the `Inspect` type, the fields `inspect` / `crash_before_claim_reply` on `CoordinatorThread` and `CoordinatorRuntime`, the two uses in `run` and the `inspect` arm in `wait`), `justfile` (`test-r2`, and any recipe that names `--test roles` or `--test coordinator_rt`)
- Test: `crates/server/tests/it.rs` gets one new case (below)

**Interfaces:**
- Produces: with the feature off, `CoordinatorThread` has no `inspect` / `with_core` / `crash_before_claim_reply`, and `CoordinatorRuntime::run` neither polls an inspect channel nor checks a crash flag. With it on, the API is unchanged.
- `just test-r2` becomes `… just test-one server "--features test-hooks --test roles -- --include-ignored"`.

- [ ] **Step 1: Write the failing check** `hooks::production_build_has_no_hooks` in `crates/server/tests/it.rs` (a new module `hooks`). "This method does not exist" cannot be asserted from a test, so the check reads the source: over `include_str!("../src/coordinator_rt.rs")`, every line containing `fn with_core`, `fn inspect`, `fn crash_before_claim_reply`, `inspect.try_iter()` or `crash_before_claim_reply.load(` has `#[cfg(feature = "test-hooks")]` on one of the 3 lines before it.
- [ ] **Step 2: Run** `just test-one server "--test it -- hooks::"` — FAIL (no cfg lines yet).
- [ ] **Step 3: Gate** the items listed under Files with `#[cfg(feature = "test-hooks")]`; in `run` and `wait` the gated statements go in `#[cfg(feature = "test-hooks")] { … }` blocks.
- [ ] **Step 4: Update the recipes.** `grep -n "roles\|coordinator_rt" justfile` — every hit that runs those targets gains `--features test-hooks`; `test-full` / `test` (if they run `-p server` with all targets) gain it too. Record the list of changed recipes in the commit message.
- [ ] **Step 5: Run** `just test-one server "--test it -- hooks::"` (PASS), `just test-one server "--features test-hooks --test coordinator_rt -- --include-ignored"` (PASS), `just test-r2` (PASS, the same test count as before the change — a count of 0 means the target was skipped), `just smoke` (PASS).
- [ ] **Step 6: Commit** `refactor(server): coordinator test hooks behind the test-hooks feature`.

### Task 3: One Postgres test-support crate

**Files:**
- Create: `crates/pg-test-support/Cargo.toml`, `crates/pg-test-support/src/lib.rs`
- Modify: root `Cargo.toml` (member), `crates/world-store/Cargo.toml`, `crates/login/Cargo.toml`, `crates/server/Cargo.toml` (dev-dependency), `crates/world-store/tests/common/mod.rs`, `crates/login/tests/common/mod.rs`, `crates/server/tests/store_world.rs`, `crates/server/tests/roles.rs`

**Interfaces:**
- Produces (`pg_test_support`):
  - `pub const ADMIN_URL_ENV: &str = "MMO_TEST_DATABASE_URL";`
  - `pub const SKIP_MSG: &str = "skipped: MMO_TEST_DATABASE_URL unset";`
  - `pub fn admin_url() -> Option<String>`
  - `pub struct PgTestDb { pub url: String, /* private: admin_url, name */ }`
  - `impl PgTestDb { pub fn create(prefix: &str) -> Option<PgTestDb>; pub async fn create_async(prefix: &str) -> Option<PgTestDb>; pub fn name(&self) -> &str; }` — the database is `<prefix>_<pid>_<nanos>`; `None` when the env var is unset.
  - `impl Drop for PgTestDb` — `DROP DATABASE IF EXISTS … WITH (FORCE)` on a fresh `std::thread` with its own current-thread tokio runtime (a drop inside an async test must not start a runtime on the test's own thread).
- Dependencies: `sqlx` (features `postgres`, `runtime-tokio`, the workspace's TLS choice — copy the feature list from `crates/world-store/Cargo.toml`), `tokio` (`rt`).
- The prefixes in use stay: `mmo_test`, `mmo_srv_test`, `mmo_roles_<tag>`. `roles.rs` keeps its own `open()` helper (it needs `world-store`); `store_world.rs`'s explicit `drop_db(self)` calls become plain drops.

- [ ] **Step 1: Write the failing test** `crates/pg-test-support/tests/it.rs` `create_and_drop`: with the env var set, `PgTestDb::create("mmo_support")` → connect to `db.url` and `SELECT 1`; after `drop(db)`, `SELECT 1 FROM pg_database WHERE datname = $name` returns no row. With the env var unset → `None` (the test prints `SKIP_MSG` and returns). Slow-tagged.
- [ ] **Step 2: Run** `MMO_TEST_DATABASE_URL=postgres://mmo:mmo@127.0.0.1:55432/postgres just test-one pg-test-support "--test it -- --include-ignored"` — FAIL (crate missing).
- [ ] **Step 3: Implement** the crate; replace the four copies (each `common/mod.rs` keeps a thin `pub use pg_test_support::*;` so test files need no edits beyond the type name).
- [ ] **Step 4: Run** the Step 2 command (PASS), `just test-pg "--test it -- open:: fence::"`, `just test-login-pg`, `just test-r2`, `just test-one server "--test store_world -- --include-ignored fenced"` with the env var set (all PASS), `cargo tree -i aws-lc-rs` (prints "did not match any packages"), `just smoke`.
- [ ] **Step 5: Commit** `test: one Postgres test-support crate replaces four PgDb copies`.

### Task 4: Pending kicks expire; dial failures stop repeating

**Files:**
- Modify: `crates/login/src/store.rs` (`end_session`, new `expire_pending_kicks`), `crates/login/src/host_link.rs` (`send_pending_kicks` calls the expiry first), `crates/svc-link/src/lib.rs` (`dial_loop`)
- Test: `crates/login/tests/api.rs` (or the module that holds the kick tests — `grep -rn "pending_kicks\|kick_acked" crates/login/tests`), `crates/svc-link/tests/link.rs`

**Interfaces:**
- `pub const PENDING_KICK_TTL_SECS: u64 = 86_400;` (`host_link.rs`).
- `LoginStore::end_session` also runs, in the same transaction, `DELETE FROM pending_kicks WHERE world_id = $1 AND player_token = $2 AND session_epoch <= $3`.
- `pub async fn expire_pending_kicks(&self, now_secs: u64, ttl_secs: u64) -> Result<Vec<PendingKick>, StoreError>` — deletes and returns the rows with `created_at + ttl_secs <= now_secs`. `send_pending_kicks` logs one `warn!("[login] kick for world {world} player {token} dropped after {age} s unacknowledged")` per returned row.
- `pub const LINK_DIAL_LOG_EVERY_SECS: u64 = 60;` (`svc-link`): `dial_loop` logs the first failure of a down period at `warn`, then at most one line per `LINK_DIAL_LOG_EVERY_SECS` carrying the number of failed attempts since the last line; the retry cadence is unchanged.

- [ ] **Step 1: Write the failing tests:** `kicks::ended_session_drops_its_pending_kick` (queue a kick for epoch 3, `end_session(world, token, 3)` → `pending_kicks()` is empty); `kicks::newer_session_keeps_its_kick` (kick for epoch 4 survives `end_session(…, 3)`); `kicks::old_kick_expires_once` (a row with `created_at = now - 86_401` → `expire_pending_kicks(now, 86_400)` returns it and the table is empty; a row one second younger stays); `link::dial_failures_log_once_per_period` (a dialer against a closed port for 3 s with the retry minimum: a test logger counts lines containing `dial` and `failed` → exactly 1).
- [ ] **Step 2: Run** `just test-one login "--test it -- kicks::"` and `just test-one svc-link "--test dial_log -- --include-ignored"` (its own target, see Step 3) — FAIL.
- [ ] **Step 3: Implement.** The svc-link test needs a logger: `link.rs` is in the `it` binary, and a logger is process-global state — give the test its own `[[test]]` target (`crates/svc-link/Cargo.toml`, `name = "dial_log"`) per CLAUDE.md's testing rule, and run it as `--test dial_log`.
- [ ] **Step 4: Run** `just test-one login "--test it -- kicks::"`, `just test-one svc-link "--test dial_log -- --include-ignored"`, `just test-one svc-link "--test it -- --include-ignored"`, `just test-one server "--test login_link -- --include-ignored"`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `fix(login,svc-link): pending kicks end with their session or after a day; one dial-failure line a minute`.

### Task 5: Fail-closed `jwt` admission; coordinator state decode ladder

**Files:**
- Modify: `crates/server/src/lib.rs` (the `spawn_orders` block that today warns "auth: jwt without a service token"), `crates/server/src/login_link.rs` (the env read), `crates/game/src/coordinator/state.rs` (`decode`), `crates/server/src/world_file.rs` (`ensure_coordinator_state`: the doc comment only — a migrated state loads like any other), `justfile` (`dev-login`), `CLAUDE.md` (Auth line), `TODO.md`
- Create: `crates/game/tests/fixtures/coordinator_state_v1.bin` (written once by the test below, then committed)
- Test: `crates/game/tests/coordinator.rs` (in `it.rs`), `crates/server/tests/it.rs`

**Interfaces:**
- `pub const JWT_ADMISSION_ENV: &str = "VOXEL_JWT_ADMISSION";` and `pub enum JwtAdmission { SpawnOrders, Newest }` with `pub fn jwt_admission(token_configured: bool, env: Option<&str>) -> Result<JwtAdmission, String>` in `login_link.rs`: token → `SpawnOrders` (the env var set to `newest` together with a token is an error naming both); no token and `newest` → `Newest` (the existing WARN stays); no token and unset → `Err("auth: jwt needs the login link — set VOXEL_SERVICE_TOKEN[_FILE] (a world registered with a host link), or VOXEL_JWT_ADMISSION=newest to admit tickets by newest epoch without spawn orders or kicks (dev only)")`; any other value → `Err` naming the accepted one. `lib.rs` calls it and `fatal`s on `Err`.
- `dev-login` exports `VOXEL_JWT_ADMISSION=newest` on its server line.
- `CoordinatorState::decode(bytes) -> Result<CoordinatorState, HistoryLoadError>` becomes a ladder: `match version { 1 => decode_v1(body).map(migrate_v1), COORDINATOR_STATE_VERSION => decode_current(body), v => Err(Corrupt(format!("coordinator state format {v}; this build reads 1..={COORDINATOR_STATE_VERSION}"))) }`. While the current version is still 1 the two arms are one; the structure and the fixture are the deliverable. `struct CoordinatorStateV1` is a frozen private copy of today's three fields; `fn migrate_v1(CoordinatorStateV1) -> CoordinatorState`.
- The module doc's rule changes from "NOT migrated" to: "an older format is migrated on load and written back in the current one at the next save; a newer or unknown one refuses the start".

- [ ] **Step 1: Write the failing tests:** `jwt_admission::` in `crates/server/tests/it.rs` — four cases over `jwt_admission(…)`: `(true, None) → Ok(SpawnOrders)`, `(false, Some("newest")) → Ok(Newest)`, `(false, None) → Err` containing both `VOXEL_SERVICE_TOKEN` and `VOXEL_JWT_ADMISSION=newest`, `(true, Some("newest")) → Err`; `coordinator::state::v1_blob_still_decodes` — `include_bytes!("fixtures/coordinator_state_v1.bin")` decodes to a state with 1 claim and 1 memo entry and a history that `History` loads (the fixture is produced by an `#[ignore]`d generator test `write_v1_fixture` run once with today's `encode`); `coordinator::state::unknown_version_is_refused` (first byte 200 → `Err(Corrupt)` whose text contains `200`).
- [ ] **Step 2: Run** `just test-one server "--test it -- jwt_admission::"` and `just test-one game "--test it -- coordinator::state::"` — FAIL.
- [ ] **Step 3: Implement** (generate and commit the fixture before changing `decode`).
- [ ] **Step 4: Run** both commands, `just test-r2`, `just test-one server "--test login_link -- --include-ignored"`, `just smoke` — PASS. `grep -n "VOXEL_JWT_ADMISSION" justfile CLAUDE.md` shows the recipe and the doc line.
- [ ] **Step 5: Commit** `feat(server,game): jwt hosts need the login link unless told otherwise; coordinator state decodes through a version ladder`, then the `TODO.md` "R2b minors" line is deleted (index-only) in `docs: R2b minors done`.

---

## Self-review notes

- Coverage of handoff §5: split `world_manager.rs` → Task 1; cfg-gate the hooks → Task 2; one Postgres test-support crate → Task 3; `pending_kicks` expiry and the repeating warning → Task 4 (the 2 s warning itself was already fixed; the remaining repeat is the dial log); the two decisions (jwt fallback, state format mismatch) → Task 5.
- Names across tasks: `PgTestDb` (T3) is what R3's `hosts.rs` uses; the `test-hooks` feature (T2) is what R3's coordinator tests enable; the decode ladder (T5) is what the next coordinator state format extends; `world_manager/world_link.rs` (T1) is where R3's coordinator-facing changes land, and R3 adds sibling files beside it.
- Not here: anything multi-host (R3), `PgClaim` / failover / drain (R4), k8s (R5).
