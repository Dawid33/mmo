# Handoff: implement R2b — coordinator / host split, owner epochs, login push (subagent-driven)

**Date:** 2026-11-07 · **For:** a fresh session that executes the plan
**Status:** plan written, decisions settled with Dawid, self-reviewed. Nothing implemented yet.
**Prerequisite:** R2a (`2026-11-07-r2a-login-accounts.md`) is fully landed — check its ledger
`.superpowers/sdd/2026-11-07-r2a-login-accounts/progress.md` and `git log` for its 11 commits
(`Credential`, `auth`, `login-api`, `login`, `VOXEL_AUTH=jwt`, client login). If R2a changed any
interface R2b names (`Authenticator`, `LoginStore`, `ApiErrorCode`, `login` subcommands), fix
R2b's plan first and tell Dawid.

## Your task

Execute `docs/superpowers/plans/2026-11-07-r2b-role-split.md`, Tasks 1–12, in order, with
**`superpowers:subagent-driven-development`** (Dawid's choice): fresh implementer per task,
fresh reviewer per task, fix loop, final whole-branch review on the most capable model.

- Ledger: `.superpowers/sdd/2026-11-07-r2b-role-split/progress.md`.
- Order: 1 → 2 → 3 (store + ownership); 4 → 5 → 6 → 7 (coordinator out of the manager);
  8 (svc-link, independent — may be done any time before 9); 9 → 10 → 11 → 12. Run them one at
  a time (shared checkout and build cache).

## Read first, in this order

1. **The plan:** its "Decisions settled in planning" (13 items), "Global Constraints", "Review
   Focus". Paste the Global Constraints and the task's Interfaces block into every brief.
2. **The design:** `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md`
   §0.4(a)(b)(d), §0.7(b)(c)(d), §7 (fencing), §0.10.
3. **`CLAUDE.md`**: `crates/game` (world_manager / region_runner, "World history" + "In the live
   world", save cuts and the conservation invariant, seam-transparent regions), `crates/server`
   (world store, cuts, orderly exit, `give_up_unparked`), testing rules incl. `just
   test-threads` and `InlineSchedule`.
4. The R1 plans' "Decisions settled" (`2026-11-07-world-store-r0-r1.md`,
   `2026-11-07-forged-world-in-db.md`) — R2b replaces R1's Decision 9 (the single-writer lock).

## Decisions to carry (don't re-open)

- **Shape B:** the host keeps the client edge, `WorldManager`, regions, LOD and its cuts; the
  coordinator owns history, calendar, claims, embark/adventure validation, legends, person info,
  daily broadcasts, notices. The world map stays on the host.
- **QUIC service link** (`svc-link`), shared token + pinned self-signed cert; R3 reuses it.
- **Exit save:** no exit cut if any region was given up; a fenced host never commits again.
- **Coordinator = sole writer of history state, durable before visible** (directives and
  claim-changing replies only after the save commits). Safe because region saves never hold
  person bodies (`crates/game/src/portable.rs:286`) and `History::after_load` makes everyone
  abstract.
- **Restart = `Attach` + `History::reattach_region`**; downtime: lifecycle messages dropped,
  requests queued, replayed once via `RequestId` + persisted memo, 120 s timeout →
  `WorldUnavailable`.
- **Owner epochs replace the Postgres advisory lock; SQLite keeps `flock` and runs `AllInOne`
  only.** R2 claims unconditionally at start (no leases — R4).
- Single-player: `AllInOne`, SQLite, no login, behaviour unchanged.

## Facts checked on 2026-11-07 (develop `d4c27ac`, before R2a)

- `WorldManager` (`crates/game/src/world_manager.rs:591-794`) holds `history` and calls it
  synchronously in ~37 places: `region_started` in `ensure_running` (2943-3017, returns
  `SpawnPerson` events), `region_stopping` (1803, 1819), `region_parked` on `PeopleParked`
  (1526), `history_region_lost` (961), `note_positions` (1507), `person_crossed` in `Departures`
  (1625, returns delivered/absorbed), `deliver_directives` (934), `advance` + `broadcast_daily` +
  `route_notices` in `maintain` (1759-1770), embark/retire/return/adventure (1110-1258),
  `landing_in` uses `history.grid()` (1092), and the cut carries `history::persist::encode`
  (2284).
- History's view of "running" mirrors the manager's (`handover.rs` `running` / `stopping`
  sets); `person_crossed` answers from it — with a FIFO link the host's own answer agrees.
- `calendar_now` follows wall time from `calendar_anchor` (`world_manager.rs:912`).
- Region threads never talk to clients; every `RegionOutput` goes through the manager. The
  manager detects dead regions by failed sends, reads `spawner.world_tick()` locally, and reaps
  by joining — untouched in R2 (regions stay in the host process).
- `RegionInput` / `RegionOutput` / `ServerEvent` have **no serde** today — R2b does not need them
  on the wire (that is R3); the new `HostMsg` / `CoordMsg` must be serde.
- Store (`crates/world-store`): `open.rs:38-105` (SQLite `flock` on `<db>.lock`, Postgres
  `pg_try_advisory_lock(WORLD_LOCK_KEY)` on a dedicated connection, then `migrate!`, then
  `check_world_row`; no read-only mode); `rows.rs:411-544` `commit_cut` = one transaction,
  deletes and re-inserts the **whole** `in_flight` table and writes `world.world_tick` (global);
  `resolve_player` = `MAX(player_id)+1` under the store thread's writer mutex (rows.rs:375);
  `world.super_region_regions` already exists (default 16); `0001_schema_v1.sql:66` holds the
  `-- R2: super_regions(owner, epoch) goes here.` comment; `schemas_match` +
  `EXPECTED` in `crates/world-store/tests/schema.rs:78-124`.
- Server process: `crates/server/src/lib.rs` `run()` 449-727 (manager on the main thread,
  `CutWriter` on it, `park_for_exit` + `finish` at exit 714); `cuts.rs` (stuck after 3 failures);
  `region_threads.rs:110-222` (store load, tile prefetch, `LoadFailed`).

## Traps (put the relevant ones in each brief)

- **No worktrees**; main checkout, `develop`, absolute paths, branch check before commit.
- **Dawid's uncommitted work** stays out of commits: `git add` only task files; CLAUDE.md /
  justfile by index-only staging; check `git diff --cached --stat`.
- **No background commands in subagents** (they derail on the notification); verify `git log`
  yourself after each task; distrust post-edit LSP diagnostics.
- **`just test-threads` after Tasks 3, 5, 6, 7, 9, 10, 11** (manager / handoff / region
  threads). Seeded tests must print a replayable seed on failure.
- **Tasks 5–6 are the risky refactor** (synchronous history → messages). Port the existing
  `world_manager::` / `history_play::` tests to `InlineWorldLink` with `coordinator_delay(0)`
  **first** and keep them green before adding the delayed-link tests; a delay of 0 must
  reproduce today's ordering exactly.
- **Never weaken the state-hash check**; if a sim-hashed type changes, `just determinism-wasm`
  (the plan expects none — `EmbarkRefusal` / `AdventureRefusal` are protocol, not sim).
- **Postgres tests:** `just pg-up`; `just test-pg`, `just test-r2`. Split roles refuse SQLite by
  design — don't "fix" a test by switching it to SQLite.
- Secrets: the service token and link certs live in git-ignored `target/dev-r2/`.

## Model / review guidance

- Use the most capable model for implementers of Tasks 2 (fencing transactions), 4
  (coordinator core), 5, 6 (manager refactor), 7 (durable-before-visible), 10 (restart), 11
  (cross-process newest-wins). Standard model is fine for 1, 3, 8, 9, 12.
- Reviewers check conservation explicitly: for every new message path, "can this duplicate or
  lose a person / player / item / claim if it is delayed, dropped, or replayed?" — and that the
  task's seeded tests would catch it.
- Final whole-branch review: the five Review Focus lines, plus single-player (`just run`)
  unchanged and `just test-threads` green.

## Done means

- All 12 tasks committed, ledger complete, final-review findings fixed or recorded.
- `just smoke`, `just test-threads`, `just test-r2`, `just test-pg "--test it"`,
  `just test-one game "--test it coordinator:: coordinator_lifecycle:: coordinator_requests::"`
  pass.
- Task 12's live check (`just dev-r2`, kill/restart the coordinator, restart the host; `just run`
  unchanged) **only with Dawid's OK**, one client, `adapter=Intel`; results in `TODO.md`.
- Report to Dawid: commits, deferrals, what R3 must pick up (host↔host link, multi-host cuts
  coordinated by the coordinator, LISTEN/NOTIFY, per-ordinal placement, the store-thread decode
  item).
