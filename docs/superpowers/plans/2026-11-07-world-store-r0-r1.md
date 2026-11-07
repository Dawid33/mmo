# World Store R0 + R1 (Postgres + SQLite, one host) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One server process keeps its whole world in one database: a `.sqlite` file or a Postgres database, with the same code for both. That covers the forged world (one blob here; Plan B tiles it), regions in a portable format, players, history and the world tick. It is restart- and crash-safe at a consistent world tick, and it builds LOD from stored regions.

**Architecture:**
- **New native-only crate `crates/world-store`.** It holds the sqlx backends (SQLite and Postgres), the two migration sets, the row codecs and one store thread.
  - The store thread owns a current-thread tokio runtime.
  - Every other thread reaches it through a cloneable, sync `StoreHandle`: requests go over a tokio unbounded mpsc (its `send` is sync, and the store's runtime can await it; a crossbeam `recv` would block that runtime), answers come back over crossbeam channels (a tokio oneshot for `resolve_player_async`).
- **`game` gains, all IO-free:**
  - the portable region format (`portable.rs`);
  - a world-tick save cut (`save_cut.rs` plus `WorldManager` changes): every region snapshots at the same tick T, and in-flight crossings are classified by an exact rule;
  - one small trait for reading stored terrain (`StoredTerrain`), for LOD and halo.
- **`server`** swaps its file stores for the handle.
- **The client never links sqlx.** It still forges a `terrain.world` file into the world dir. The server imports that file into the store on first start.

**Tech Stack:**
- sqlx 0.8, latest 0.8.x patch at implementation time: `default-features = false`, features `runtime-tokio`, `sqlite`, `postgres`, `migrate`, `macros` (only for `migrate!`); bundled SQLite.
- tokio, a current-thread runtime on the store thread.
- zstd, bincode, crossbeam.
- Postgres 17 in docker for dev and tests.

**Spec:**
- The settled design: `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md`, §0 (above all §0.2, §0.9c/f, §0.10). §6 and §8 give background.
- Handoff: `docs/superpowers/plans/2026-11-06-persistence-plans-handoff.md` §5.
- Prerequisite, landed: `docs/superpowers/plans/2026-11-06-seam-transparent-regions.md` (`WorldClock`, `EntityId`, `EntityBundle`, `in_flight`, halo, intents; all in `develop` as of `1879620`).
- Findings reused from the never-executed `docs/superpowers/plans/2026-09-22-durable-world-persistence.md`: there is no terrain-edit event; restore-by-rebuild resets slotmap keys; golden-blob tests for stored types.

## Decisions settled in planning (2026-11-07)

These are the handoff's design questions. Each was settled from code and measurements; the trade-off is given in prose.

1. **Terrain rows: one row per 16³ sim chunk.**
   - Measured on the forged debug world (256 m regions, 16-block chunks, zstd level 3) with a throwaway probe:
     - rugged or forested regions: ~700 stored chunks; per-chunk rows total 5.7 MiB (avg 8.4 KiB, max 58 KiB); one row for the whole region would be 5.3 MiB;
     - open regions: 324–384 chunks, 0.57 MiB in total.
   - Per-column rows were no smaller than per-chunk.
   - So per-chunk costs ~7–10 % more space than one region row. In return an edit rewrites ~8 KiB instead of 5 MiB, and LOD or halo can read a subset.
   - Today's lz4 `Rollback` memory images average 2.5 MiB per region.
   - Row count is ~700 per visited region: trivial for both engines.
2. **Trait shape: one concrete `StoreHandle`, not a set of traits.**
   - Both backends live inside the handle (an enum over `Pool<Sqlite>` / `Pool<Postgres>`), so a trait would have one implementation.
   - Tests use in-memory SQLite (`sqlite::memory:`), i.e. real SQL rather than a `MemoryStore`.
   - `game` needs only `StoredTerrain` (LOD and halo workers read through it).
   - `PlayerStore` stays a server trait (the network edge uses it). It gets a store-backed implementation, and `MemoryPlayerStore` stays for procedural worlds.
   - The `WorldManager` stays IO-free, as today: it *produces* cuts (`take_cut`), the way it produces saves now.
3. **Sync/async bridge: one store thread with a current-thread tokio runtime.**
   - Region threads, the manager loop and the savers are sync crossbeam code; sqlx is async.
   - One thread owns the runtime and the pool. Each request becomes a task on it, so a long cut commit does not block a region load (SQLite in WAL mode reads alongside the writer).
   - Callers block on a reply channel. The network edge, which is async, gets an `async` variant.
   - Why not reuse the server's tokio runtime: the store must also work in tools (`worldctl`) that have none. Owning one runtime in one place mirrors how the network edge is confined.
4. **The native client does not link sqlx.**
   - The generator keeps writing `terrain.world` into the staging dir, which is unchanged code.
   - The spawned server imports it into the store on first start, then deletes it.
   - Cost of the alternative: libsqlite3-sys is a C build plus sqlx and its tokio features in the client's dependency graph, for no player-visible gain.
   - The menu reads only `world.json` (see 5).
5. **`world.json` and `preview.png` stay files beside the store.** They are the menu's metadata; the client lists worlds without opening a database. Postgres worlds get their listing from the login registry in R2.
6. **No on-disk `Rollback` blob.**
   - The in-process parking lot keeps its in-memory `SerializedRegion` (same build, same process) for fast resume.
   - The store holds only the portable format. Genesis-from-rows is measured in Task 3's restore test. Revisit only if restore latency is shown to hurt.
7. **Existing worlds: their old save files are deleted, not converted** (user, 2026-11-07).
   - The legacy files are `<world>.regions/`, `<world>.players/`, `<world>.history` (+ `.bak`), the region `.bak` files and `world.tick`. Task 10 deletes them.
   - The forge files (`terrain.world`, `default.world`) are not saves: they are imported on first start like a new world's (Task 7). Old worlds keep their terrain, but every region restarts pristine, the history is bootstrapped anew and players start with a fresh kit.
   - No converter is written. The old file-format readers are deleted with `region_store.rs` and `FilePlayerStore`.
8. **Config:** `VOXEL_WORLD_DB` is a `.sqlite` path or a `postgres://` URL.
   - Unset: the store is `<VOXEL_WORLD_FILE>.with_extension("sqlite")`. The default world becomes `worlds/default.sqlite`; a client world becomes `worlds/<slug>/terrain.sqlite`.
   - `VOXEL_WORLD_FILE` keeps two meanings: `procedural` (no store, nothing persisted), or the forge file to import when the store has no world yet. If neither exists, the server forges into the store.
9. **Single writer:**
   - SQLite: an OS lock (`std::fs::File::try_lock`) on the sidecar `<db>.lock`, held for the process's life.
   - Postgres: `pg_try_advisory_lock(WORLD_LOCK_KEY)` on one dedicated connection held for the process's life. This is an R1 stand-in; R2's owner epochs replace it.
10. **The cut is a world tick in the future.**
    - The manager picks `T = world_tick + SAVE_CUT_LEAD_TICKS`. Every running region snapshots its state *at the opening of slot T*: after slot T−1 closed, before any slot-T input.
    - No region waits: a region reaches T on its own clock (it never skips a tick number).
    - A region already past T answers `CutMissed`; the manager aborts and retries with double the lead.
    - **In-flight rule (as built, Task 5):** decided by each region's output-channel order, not by stamps. At the opening of slot T a region first flushes the Tick's departures (stamped T) and the intents sent so far, then sends its snapshot. A crossing or intent routed before its source's answer is not in the source's snapshot; it is in the cut iff its target confirmed it after the target's own answer (or not at all). One routed after its source's answer is still in the source's snapshot. A region that parks before T answers from `Shutdown` with what is held for slots before T applied, before the rest. A body created or restored by the manager (move, reconnect) in a region that has not answered is held to slot T+1; a parked record keeps its state at T while the cut is pending. A cut still waiting `CUT_DEADLINE_LEADS` leads past T is aborted (a stopping region that never answers is then left out until it parks).
11. **Dirty tracking survives failures.**
    - A region compares chunk terms against the terms of its previous snapshot and sends only changed chunks.
    - The manager merges every snapshot into an `uncommitted` map per region (chunks union, newer wins; entities and meta replaced). It clears an entry only when a cut containing it commits.
    - A failed commit therefore loses nothing, and regions need no commit acknowledgement.
12. **Player records are written only in cuts.**
    - A disconnect parks the body into the manager (as today) and the record reaches the store with the next cut. A crash then restores players and regions from the *same* tick, so items cannot be duplicated or lost between a pile and an inventory.
    - Only the token → `PlayerId` allocation is written immediately.

## Sequencing: runs after the water static layer

The water static layer (`docs/superpowers/plans/2026-11-06-water-static-layer.md`; handoff `2026-11-07-water-static-layer-implementation-handoff.md`) is implemented **first**. This plan starts only once it has landed on `develop`. Before Task 1:
- **Re-verify every `file:line` reference.** They were taken at `1879620`, and water edits `state.rs`, `region_runner.rs`, `world_manager.rs`, `halo.rs` and `region_store.rs`.
- **Task 3 classification:**
  - **`GameData.water_bodies` is *persist*** (into `RegionMeta`), checked after water landed (`3036f22`). `label_water_bodies` (`water.rs:65`) runs only at genesis in `Region::from_columns` and reads the genesis chunks (ice splits bodies). So `restore_portable` inserts the stored labelled `ground` and `water_bodies` raw and never relabels. Swimming state rides in `Motion` (`Immersion`) and is already carried. `snapshot_roundtrip_is_equivalent` must include a water body.
  - The exhaustive destructure in `classify` forces the decision either way.
- **Task 4 goldens:** record them after water, since `ColumnGround` gains recipe water.
- **Task 7:** delete `region_store.rs` whatever `REGION_FILE_VERSION` water left behind; `legacy_saves_refuse_start` does not depend on the version.
- **Task 8:** water adds `LodWater` to `LodData`. A `Stored` LOD build must produce it the same way a `Region` build does; extend `stored_region_lod_shows_saved_edit` to compare the water part too.

## Global Constraints

- Work on `develop` in the main checkout. **Never create a git worktree** (CLAUDE.md), and that includes subagents. The tree holds the user's uncommitted work: `git add` only the files a task names, never `-A` or `.`, and run `git status` before each commit.
- `game` stays Bevy-free, IO-free, sync and wasm-safe: no sqlx, tokio, zstd or filesystem access in `game`. The wasm client must not pull sqlx. `world-store` is native-only; only `server` (and tools) depend on it, and the client must never depend on it.
- **No compile-time database:** never use `query!`/`query_as!`. Use runtime `sqlx::query()` and `migrate!` (which reads files, not a database). `just smoke`, `check` and `clippy` must never need Postgres.
- The schema uses only the subset both engines share: integer (`BIGINT` in pg, `INTEGER` in SQLite), `TEXT`, blob (`BYTEA` / `BLOB`). No Postgres-only types and no `world_id` columns.
- `u64` values (`EntityId.serial`, ticks, `PlayerId`) are stored as `i64` by bit cast (`as i64` / `as u64`). This is tested at `u64::MAX`.
- Blob columns hold `[format byte][zstd(bincode(T))]` (`world_store::codec`). A layout change in a stored type must fail a golden-blob test, never decode silently.
- **Never regenerate over stored data:** a store read error at region start stops that region; it never falls back to worldgen. A missing row means "never ran" and generates.
- **Never weaken the server state-hash check** (`VOXEL_STATE_HASH_INTERVAL`); a restored region is not expected to hash-match its pre-save self (physics handles are rebuilt), and it has no subscribers.
- Tunables are named `pub const`s: `SAVE_CUT_LEAD_TICKS` (20), `SAVE_CUT_LEAD_MAX_TICKS` (640), `STORE_LOAD_RETRY_SECS` (10), `STORE_POOL_MAX` (4), `WORLD_LOCK_KEY`, `STORE_FORMAT_VERSION` (1). `WORLD_SAVE_SECS` stays 60.
- Wire serialization stays `encode_packet` / `decode_packet`. The store's blobs are not wire packets.
- **Verification:**
  - Use `just test-one <crate> "<target> <filter>"` and `just smoke`.
  - Run `just test-threads` after touching `WorldManager`, `region_threads` or the handoff paths.
  - Run `just determinism-wasm` if a hashed or serialized *sim* type changes.
  - Never run whole suites. Client tests are a slow opt-in: ask first.
  - Tests over 100 ms get `#[ignore = "slow: >100ms — run via just test-full"]`.
- Postgres tests run only when `MMO_TEST_DATABASE_URL` is set; otherwise they print `skipped: MMO_TEST_DATABASE_URL unset` and pass. Each one creates its own `mmo_test_<random>` database and drops it afterwards. SQLite tests use `sqlite::memory:` or a temp dir and always run.
- The justfile gets a recipe for every new workflow (`pg-up`, `pg-down`, `pg-reset`, `pg-psql`, `test-pg`, `clean-old-worlds`). CLAUDE.md paragraphs are updated in the task that changes the behaviour, as a separate commit.
- **Ask before** any benchmark, perf capture or live run (one client, `adapter=Intel`).
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **A crash in the middle of a cut** (process killed during `commit_cut`): the store still holds the previous cut, whole, and a restart restores it. *(Task 6: `commit_cut_is_atomic`.)*
2. **A transfer straddling the cut** (a slime departs at T−1 and lands at T+1, or departs at T+1): after restore from that cut it exists exactly once. *(Task 5: `straddling_transfer_is_saved_exactly_once`, seeded jitter.)*
3. **A region whose store read fails** (corrupt chunk blob, Postgres down): that region does not start, logs the reason and is retried after `STORE_LOAD_RETRY_SECS`. Arrivals wait in the mailbox, a Play into it is refused, and it never regenerates. *(Task 7: `region_load_failure_never_regenerates`.)*
4. **A `.sqlite` opened by two servers:** the second refuses at open with `StoreError::Locked` naming the file, before touching anything. *(Task 2: `second_open_is_locked`.)*
5. **A world saved under other dials** (`REGION_BLOCKS` / `CHUNK_BLOCKS`): refused at open with `StoreError::OtherDials { stored, built }`, whose message names both values and the `just region_blocks=` fix. *(Task 2: `other_dials_refused`.)*
6. **A region that parks while a cut is pending:** its stop snapshot is valid at T and goes into that cut, and a later respawn does not lose its uncommitted chunks. *(Task 5: `park_during_pending_cut`.)*
7. **A world dir holding both a legacy `terrain.regions/` and no store:** the server refuses to start with "run `just clean-old-worlds`" instead of silently starting the world from pristine terrain while old saves sit beside it. *(Task 7: `legacy_saves_refuse_start`.)*

---

### Task 1: Dev Postgres (R0)

**Files:**
- Create: `deploy/dev/compose.yml`, `deploy/dev/README.md`
- Modify: `justfile`, `.gitignore` (if needed)

**Interfaces:**
- Produces: a Postgres on `127.0.0.1:55432`, user, password and database all `mmo`. Dev URL `postgres://mmo:mmo@127.0.0.1:55432/mmo`. Test URL convention `MMO_TEST_DATABASE_URL=postgres://mmo:mmo@127.0.0.1:55432/postgres` (tests create their own databases from there).

- [ ] **Step 1: Write `deploy/dev/compose.yml`.**
  - One service `postgres`, image `postgres:17-bookworm` (major version pinned).
  - Named volume `mmo-dev-pg`, `ports: ["127.0.0.1:55432:5432"]`, and a healthcheck with `pg_isready`.
  - It is separate from `deploy/web/compose.yml`.
- [ ] **Step 2: Add the recipes.**
  - `pg-up` runs `docker compose -f deploy/dev/compose.yml up -d --wait`.
  - `pg-down` and `pg-reset` (`down -v`, then `up`).
  - `pg-psql *args` runs `psql` in the container.
  - `test-pg *args` runs `MMO_TEST_DATABASE_URL=… just test-one world-store "{{args}}"`.
  - A justfile variable `dev_database_url`.
- [ ] **Step 3: Verify.** Run `just pg-up && just pg-psql -c 'select version()'`. Expected: a PostgreSQL 17 version line. Then run `just pg-down`.
- [ ] **Step 4: Commit** (`deploy/dev/*`, `justfile`): `feat(deploy): dev Postgres compose + just pg-* recipes`.

### Task 2: `world-store` crate — open, lock, dials, schema v1 on both engines

**Files:**
- Create:
  - `crates/world-store/Cargo.toml` (edition 2024; deps `game`, `worldgen`, `sqlx`, `tokio` (`rt`), `crossbeam`, `zstd`, `bincode`, `serde`, `log`)
  - `crates/world-store/src/lib.rs`, `src/url.rs`, `src/open.rs`
  - `crates/world-store/migrations/sqlite/0001_schema_v1.sql`, `crates/world-store/migrations/pg/0001_schema_v1.sql`
  - `crates/world-store/tests/it.rs`, `tests/schema.rs`, `tests/open.rs`
- Modify: root `Cargo.toml` (workspace member; `sqlx` in `[workspace.dependencies]`)

**Interfaces:**
- Produces:
  - `pub enum StoreUrl { Sqlite(PathBuf), Memory, Postgres(String) }`
  - `StoreUrl::parse(s: &str) -> Result<StoreUrl, StoreError>` (`postgres://` / `postgresql://` → Postgres, `sqlite::memory:` → Memory, anything else → a path)
  - `pub struct Dials { pub region_blocks: u32, pub chunk_blocks: u32 }`, plus `Dials::BUILT` from `game::REGION_BLOCKS` / `CHUNK_BLOCKS`
  - `pub enum StoreError { Backend(String), Locked(String), OtherDials { stored: Dials, built: Dials }, OtherFormat { found: u32 }, Corrupt(String) }` (`Display` gives an actionable sentence)
  - `pub(crate) enum Pool { Sqlite(sqlx::SqlitePool), Pg(sqlx::PgPool) }`
  - `pub(crate) async fn open_pool(url: &StoreUrl, dials: Dials) -> Result<(Pool, LockGuard), StoreError>`
  - `pub const STORE_FORMAT_VERSION: u32 = 1`
- **Schema v1** (same tables, columns and logical types in both files):

  | Table | Columns |
  |---|---|
  | `world` | `id` (one row, `CHECK (id = 1)`), `format_version`, `region_blocks`, `chunk_blocks`, `content_crc`, `world_tick`, `super_region_regions` (16, unused until R2), `created_utc` text |
  | `forged_artifact` | `id`, `bytes` (one row; Plan B replaces it) |
  | `regions` | `rx`, `rz` PK; `saved_tick`, `terrain_version`, `meta` blob, `ground` blob |
  | `chunks` | `rx`, `rz`, `cx`, `cy`, `cz` PK (region-local chunk coords); `data` blob |
  | `entities` | `birth_rx`, `birth_rz`, `serial` PK; `rx`, `rz` (indexed), `saved_tick`, `kind` int, `bundle` blob |
  | `players` | `player_id` PK, `token` text UNIQUE, `record` blob NULL, `saved_tick` NULL |
  | `history` | `id`, `bytes` |
  | `in_flight` | `seq` PK, `kind` int (0 transfer, 1 intent), `target_rx`, `target_rz`, `stamp`, `payload` blob |

  A comment in both files marks where R2's `super_regions(owner, epoch)` table goes.

- [ ] **Step 1: Write the failing tests.**
  - `tests/schema.rs` `schemas_match`: apply both migration sets (SQLite always; pg only with `MMO_TEST_DATABASE_URL`, else assert the SQLite side alone). Normalize to `BTreeSet<(table, column, LogicalType, nullable, pk)>`, mapping `BIGINT`/`INTEGER` → Int, `TEXT` → Text, `BYTEA`/`BLOB` → Bytes. Assert equal. Without pg, assert the SQLite set equals a checked-in expected list.
  - `tests/open.rs`:
    - `fresh_open_writes_world_row`: `world.format_version == 1`, dials == `Dials::BUILT`.
    - `other_dials_refused`: seed `region_blocks = 512` → `Err(OtherDials { stored.region_blocks: 512, .. })`; the message contains `"region_blocks=512"`.
    - `second_open_is_locked`: two opens of one temp `.sqlite` → the second is `Err(Locked(_))`, the first stays usable.
    - `newer_format_refused`: `format_version = 2` → `OtherFormat { found: 2 }`.
    - `sqlite_dollar_placeholders_bind_in_order`: `$1, $2` binds work on SQLite. If this fails, introduce `Pool::sql(&'static str)` placeholder rewriting and keep this test.
- [ ] **Step 2: Run** `just test-one world-store "--test it"`. Expected: FAIL (crate items missing).
- [ ] **Step 3: Implement.**
  - `open_pool`: SQLite options are WAL, `synchronous=NORMAL`, `foreign_keys=ON` and `create_if_missing`. Take the lock (sidecar `<db>.lock` via `File::try_lock`; pg `pg_try_advisory_lock(WORLD_LOCK_KEY)` on a connection kept inside `LockGuard`) **before** migrating.
  - Then `migrate!("migrations/sqlite"|"migrations/pg")`, insert-or-check the `world` row, and refuse on dials or format.
- [ ] **Step 4: Run.** `just test-one world-store "--test it"`, then `just test-pg "--test it schema::"` (with `just pg-up`). Expected: PASS.
- [ ] **Step 5: Commit** (crate files, root `Cargo.toml`, `Cargo.lock`): `feat(world-store): schema v1 on SQLite and Postgres, world lock, dials check`.

### Task 3: The portable region format (`game`)

**Files:**
- Create: `crates/game/src/portable.rs`, `crates/game/tests/portable.rs` (in `tests/it.rs`)
- Modify:
  - `crates/game/src/state.rs` (split `extract_entity_safe` (state.rs:1787 after water) into a non-destructive `bundle_of` + removal; make `collider_spec_of` `pub(crate)`)
  - `crates/game/src/lib.rs` (re-export)

**Interfaces:**
- Produces:
  - `pub type ChunkTerms = BTreeMap<ChunkCoords, u32>` (values from `chunk_terrain_term`, state.rs:1987 after water)
  - `pub struct RegionMeta { pub tick: Tick, pub next_game_event_id: usize, pub next_entity_serial: u64, pub designations, pub constructed, pub item_piles, pub containers, pub pile_takes, pub intents, pub intent_replies, pub intent_inbox, pub water_bodies }` (each with its `GameData` type; `water_bodies` per ruling R1)
  - `pub struct PortableRegion { pub meta: RegionMeta, pub chunks: BTreeMap<ChunkCoords, Chunk>, pub ground: Option<Vec<(ColumnCoords, ColumnGround)>>, pub entities: Vec<EntityBundle>, pub players: Vec<(ClientId, PlayerRecord)> }`:
    - `chunks` holds only the chunks changed since the given terms;
    - `ground` is `Some` only in a region's first snapshot of this incarnation, when the saved terms are empty;
    - `entities` holds every entity that is not a player and not a person, sorted by `EntityId`.
  - `PortableRegion::merge(&mut self, newer: PortableRegion)`: chunks union with newer winning; meta, entities and players replaced; ground the first `Some`.
  - `pub struct StoredRegion { pub meta: RegionMeta, pub chunks: Vec<(ChunkCoords, Chunk)>, pub ground: Vec<(ColumnCoords, ColumnGround)>, pub entities: Vec<EntityBundle> }`, plus `impl From<PortableRegion> for StoredRegion` (a whole snapshot as stored; players dropped)
  - `impl Rollback { pub fn chunk_terms(&self) -> ChunkTerms }` (the current terms; also what `restore_portable` returns)
  - `impl Rollback { pub fn bundle_of(&self, key: EntityKey) -> EntityBundle; pub fn portable_snapshot(&self, saved: &ChunkTerms) -> (PortableRegion, ChunkTerms) }` (the second value is the full current term map)
  - `pub fn restore_portable(id: RegionId, stored: StoredRegion) -> (Region, ChunkTerms)`:
    1. A `Rollback::new(None)` seeded like `Region::from_chunks` (`seed_terrain_genesis`), then `seed_ground_genesis` and `seed_water_genesis` with the stored bodies (never `label_water_bodies`).
    2. The meta maps, inserted raw and unlogged like genesis; `pile_expiry` rebuilt from `item_piles`; `tick` / `next_game_event_id` / `next_entity_serial` set in one forgotten transaction (cells have no raw setter). Only then `Region::new`, so its `base_event_id` is the stored `next_game_event_id` (`Region::from_chunks` would leave it 0).
    3. The entities, through `apply_arrival` inside `with_data`.
    4. `next_entity_serial` raised to at least the stored value.
- **Classification** (pinned by `fn classify(d: &GameData)`, which destructures `GameData` and `Ecs` with **no `..`**, so a new field does not build until it is classified):
  - **Persist:**
    - `tick` (as `RegionMeta.tick`), `next_game_event_id`, `next_entity_serial`;
    - `designations`, `constructed`, `item_piles`, `containers`, `pile_takes`;
    - `intents`, `intent_replies`, `intent_inbox`;
    - `water_bodies` (R1);
    - `ground` (first snapshot only);
    - the `chunk` component (chunks);
    - entities through `bundle_of`: `kind`, `id`, body state, collider spec, carried components (`camera`, `motion` — swimming `Immersion` included — `slime`, `animator`, `voxel_body`, `walker`, `wander`).
  - **Derive on load:** `physics` (all rapier sets), `isometry`, `bones`, `rigidbody`, the `entities` slotmap (fresh keys), `id_index`, `chunk_index`, `pile_expiry`, `arrivals`, `region`.
  - **Drop:** `halo`, `halo_ground`, `halo_entities`, `halo_piles`, `halo_containers`, `halo_sent` (reseeded by `seed_halo` at every start).
  - **Players:** `clients` and `player_entites` → `PortableRegion.players` → the `players` table.
  - **Persons:** `person_entities` (the history respawns them).

- [ ] **Step 1: Write the failing tests** (`crates/game/tests/portable.rs`).
  - `bundle_of_matches_extract`: for a slime, a body and a voxel body, `bundle_of(k)` equals what `extract_entity_safe(k)` returns, and `bundle_of` leaves `state_hash` unchanged.
  - `snapshot_roundtrip_is_equivalent`: build a region with:
    - a dug block, a placed chest holding items, a pile and a designation;
    - two slimes, a dynamic `Body` and a queued intent.
    
    - a voxel body and a labelled water body (recipe water, `common::water::floor_with_water`).
    
    Snapshot it with empty terms, restore, then snapshot again. Assert `restored == original` field by field except `meta.tick` (a position-based kinematic body's velocities are zeroed on both sides first: rapier derives them each step and `BodyState` applies velocities to dynamic bodies only, so a restored — or arrived — slime starts at rest), and assert the second snapshot's `ground` is `None` when given the restored terms.
  - `snapshot_sends_only_changed_chunks`: snapshot, edit one block, snapshot with the first call's terms. Exactly one chunk is in the second.
  - `players_and_people_are_not_entities`: a region with a player and a person yields `entities.len() == 0` and `players.len() == 1`.
  - `merge_keeps_uncommitted_chunks`: merging `{A}` with the newer `{B}` gives `{A, B}`; merging `{A1}` with `{A2}` gives `{A2}`.
  - `classify_compiles`: compile-time only; it calls `classify` on a fresh `GameData`.
  - Mark slow if over 100 ms.
- [ ] **Step 2: Run** `just test-one game "--test it portable::"`. Expected: FAIL.
- [ ] **Step 3: Implement** `bundle_of` (the pre-removal half of `extract_entity_safe`; `extract_entity_safe` now calls it), `portable_snapshot`, `restore_portable`, `merge` and `classify`.
- [ ] **Step 4: Run** `just test-one game "--test it portable::"`, then `just test-one game "--test it -- --include-ignored portable::"`. Expected: PASS. Also run `just determinism-wasm`: `extract_entity_safe` was refactored, and its scene hashes must stay equal.
- [ ] **Step 5: Commit**: `feat(game): portable region format — bundle_of, snapshot, restore, field classification`.

### Task 4: Row codecs and golden blobs

**Files:**
- Create: `crates/world-store/src/codec.rs`, `crates/world-store/tests/codec.rs`, `crates/world-store/tests/fixtures/*.bin`

**Interfaces:**
- Consumes: Task 3's `RegionMeta`, `game::Chunk`, `game::ColumnGround`, `game::EntityBundle`, `game::PlayerRecord`, `game::IntentMsg`.
- Produces:
  - `pub fn encode<T: Serialize>(format: u8, v: &T) -> Vec<u8>`
  - `pub fn decode<T: DeserializeOwned>(expected: u8, bytes: &[u8]) -> Result<T, StoreError>` (a wrong format byte or a bad zstd/bincode body → `Corrupt` with the type name)
  - per-type `const FORMAT_*: u8 = 1`

- [ ] **Step 1: Write the failing tests.**
  - `golden_blobs_decode`: for each stored type, a fixture built by a fixed constructor in the test, `encode`d, must equal the checked-in fixture bytes, and the fixture must decode back to it.
  - `MMO_RECORD_GOLDENS=1` rewrites the fixtures (the documented re-record path).
  - `wrong_format_byte_is_corrupt`.
- [ ] **Step 2: Run** `just test-one world-store "--test it codec::"`. Expected: FAIL.
- [ ] **Step 3: Implement** `codec.rs` (zstd level 3) and record the fixtures once with `MMO_RECORD_GOLDENS=1`.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit**: `feat(world-store): format-byte zstd codecs with golden blobs`.

### Task 5: The world-tick save cut (`game`)

**Files:**
- Create: `crates/game/src/save_cut.rs`, `crates/game/tests/save_cut.rs` (in `tests/it.rs`, tagged slow)
- Modify:
  - `crates/game/src/region_runner.rs`:
    - `RegionInput::Save` (:67) becomes `SaveAt(CutId, Tick)`;
    - `RegionOutput::Saved` becomes `CutSnapshot(CutId, Tick, Box<PortableRegion>)`, plus a new `CutMissed(CutId)`;
    - `Stopped` (:100) gains a `Box<PortableRegion>`;
    - the runner keeps `saved_terms: ChunkTerms` (initialised by the spawner: empty for fresh, from `restore_portable` for stored).
  - `crates/game/src/world_manager.rs`: `request_save` (:1723), `take_saves` (:1767), `saved_transfers` (:1975), `owe_source_save` (:2004), `saved_intents` (:2799), the `save_owed*` fields and `InFlight::source_saves` (:550) are replaced by the cut. `take_player_saves` (:2138) stays only for tests or is deleted (decide by its callers).
  - `crates/game/tests/handoff_*` and `world_manager` tests that assert the old exactly-once autosave: port them to cuts.

**Interfaces:**
- Consumes: Task 3's `PortableRegion`, `merge`, `portable_snapshot`.
- Produces:
  - `pub struct CutId(pub u64)`
  - `pub enum InFlightRow { Transfer { target: RegionCoords, bundle: EntityBundle, stamp: Tick }, Intent { target: RegionCoords, msg: IntentMsg } }`
  - `pub struct WorldCut { pub id: CutId, pub tick: Tick, pub regions: Vec<(RegionCoords, PortableRegion)>, pub in_flight: Vec<InFlightRow>, pub players: Vec<(PlayerId, PlayerRecord)>, pub history: Option<Vec<u8>> }` (history = `history::persist::encode`)
  - `WorldManager::begin_cut(&mut self, now: Tick) -> Option<CutId>`: `None` while one is pending.
  - `WorldManager::take_cut(&mut self) -> Option<WorldCut>`
  - `WorldManager::cut_committed(&mut self, id: CutId, versions: Vec<(RegionCoords, u64)>)`
  - `WorldManager::cut_failed(&mut self, id: CutId)`
  - `WorldManager::restore_in_flight(&mut self, rows: Vec<InFlightRow>)`: transfers go to the target's `pending_arrivals` mailbox; intents go to `intent_mailbox`.
  - `pub const SAVE_CUT_LEAD_TICKS: Tick = 20`, `pub const SAVE_CUT_LEAD_MAX_TICKS: Tick = 640`
  - `InlineSpawner::with_portable(regions: BTreeMap<RegionCoords, StoredRegion>) -> Self`, next to `with_saved_regions` (world_manager.rs:249), for restore tests
- **Runner rule:**
  - On `SaveAt(id, T)`: if `current_tick() < T`, remember it, and when the runner is about to open slot T (after slot T−1 closed, before held inputs for T) emit `CutSnapshot`.
  - If `current_tick() == T` and slot T has not opened, snapshot now.
  - Otherwise emit `CutMissed`. Each snapshot advances `saved_terms`.
- **Manager rule:**
  - `begin_cut` asks every running, non-stopping region. A region that parks before reaching T answers through `Stopped`; its portable snapshot is valid at T. A region spawned with start tick ≤ T after `begin_cut` is asked too.
  - When all have answered, merge each answer into `uncommitted[rc]`.
  - Then build `in_flight` from the `in_flight` table, `pending_arrivals` and the intent mailboxes by Decision 10's rule.
  - A player bundle in flight becomes a `PlayerRecord`; a person bundle in flight is dropped (the history owns persons).
  - `players` = the records in the snapshots + `parked_players`.
  - `cut_committed` removes the `uncommitted` entries that this cut carried (unless re-snapped since) and updates `stored_regions`. `cut_failed` keeps them.
  - `CutMissed` aborts the cut; the next `begin_cut` doubles the lead, up to the max.

- [ ] **Step 1: Write the failing tests** (`InlineSpawner` + `InlineSchedule`, seeds 0..32).
  - `cut_snapshots_every_region_at_tick_t`: 3×3 running regions, jittered. Every `CutSnapshot` carries `tick == T`, and every region's `meta.tick == T`.
  - `straddling_transfer_is_saved_exactly_once`: a slime walking across a seam with a cut at each T in a window of ±3 ticks around its departure. Restore every region of the cut into a fresh manager (`InlineSpawner::with_portable` + `restore_in_flight`), run 40 ticks, and assert the slime's `EntityId` exists exactly once.
  - `straddling_intent_is_saved_exactly_once`: the same for a chest put across the seam. Count items: after restore the total equals the total before.
  - `park_during_pending_cut`: begin a cut, then park a region before it reaches T. The cut completes with that region's stop snapshot. Respawn it from the parked blob, edit nothing and cut again: the union of the two cuts' chunks equals all its chunks.
  - `failed_cut_keeps_uncommitted`: `cut_failed`, then a cut in which only one other chunk changed. The second cut still carries the first cut's chunks.
  - `late_region_misses_and_retries`: a region whose inputs are held back past T (it keeps ticking: `InlineSpawner::hold_inputs`) answers `CutMissed`; the next `begin_cut` uses lead 40.
- [ ] **Step 2: Run** `just test-one game "--test it -- --include-ignored save_cut::"`. Expected: FAIL.
- [ ] **Step 3: Implement** the runner and manager rules above. Delete the old save bookkeeping.
- [ ] **Step 4: Run** `just test-one game "--test it -- --include-ignored save_cut::"`, then `just test-threads`, then `just smoke`. Expected: PASS.
- As built (Task 5 implementation notes):
  - The exact in-flight rule is decided by the per-region output channel's order: at the snapshot point the runner first flushes the Tick's `Departures` and the intents sent so far, then sends `CutSnapshot`. An item routed while its source has not answered is not in the source's view; it is in the cut iff its target confirmed it after its own answer (or not at all). Equivalent to the stamp rule, and it also covers what the manager makes of an item (refusals, salvage deposits, mailbox drops inherit the flag).
  - Extra output `RegionOutput::PeoplePositions` just before each `CutSnapshot` (the history's `note_positions`, which the old `Saved` fed). Extra API: `WorldManager::cut_lead`, `pending_cut`, `awaiting_cut`, `uncommitted_regions`, `stored_regions`, `parked_blob` (the server's exit-time stopgap until Task 7).
  - Any region loss while a cut is pending aborts it (no lead doubling).
  - A region respawned from its in-process parked blob starts with empty chunk terms (its first snapshot is whole again): safe over a thread death that lost newer stored chunks; costs a full snapshot per resume.
- [ ] **Step 5: Commit**: `feat(game): world-tick save cut with exact in-flight rule; replaces per-region autosave bookkeeping`.

### Task 6: The store thread and `StoreHandle`

**Files:**
- Create:
  - `crates/world-store/src/thread.rs`, `src/handle.rs`, `src/rows.rs` (the SQL, `$N` placeholders)
  - `crates/world-store/tests/handle.rs`
- Modify: `crates/world-store/src/lib.rs`

**Interfaces:**
- Consumes: Tasks 2–5.
- Produces: `pub struct StoreHandle` (`Clone + Send + Sync`), whose methods block unless noted:
  - `StoreHandle::open(url: &StoreUrl, dials: Dials) -> Result<StoreHandle, StoreError>` (spawns the `world-store` thread)
  - `world_info(&self) -> Result<WorldInfo, StoreError>`, where `pub struct WorldInfo { pub content_crc: Option<u32>, pub world_tick: Tick, pub has_artifact: bool }`
  - `put_artifact(&self, bytes: Vec<u8>, content_crc: u32) -> Result<(), StoreError>`
  - `artifact(&self) -> Result<Option<Vec<u8>>, StoreError>`
  - `load_region(&self, rc: RegionCoords) -> Result<Option<StoredRegion>, StoreError>`
  - `load_terrain(&self, rc: RegionCoords) -> Result<Option<TerrainSnapshot>, StoreError>`
  - `stored_regions(&self) -> Result<BTreeMap<RegionCoords, u64>, StoreError>` (rc → `terrain_version`)
  - `load_in_flight(&self) -> Result<Vec<InFlightRow>, StoreError>`
  - `load_history(&self) -> Result<Option<Vec<u8>>, StoreError>`
  - `commit_cut(&self, cut: WorldCut) -> crossbeam::channel::Receiver<Result<CutCommitted, StoreError>>` (non-blocking), where `pub struct CutCommitted { pub id: CutId, pub versions: Vec<(RegionCoords, u64)> }`
  - `resolve_player(&self, token: &PlayerToken) -> Result<(PlayerId, Option<PlayerRecord>), StoreError>`, plus `async fn resolve_player_async(…)` (tokio oneshot reply)
  - `shutdown(self)`: drains queued commits, then joins.
- **Commit transaction, one per cut:**
  - per region: upsert `regions` (`saved_tick = T`; `terrain_version + 1` iff it wrote at least one chunk), upsert the given chunks, `DELETE FROM entities WHERE rx,rz`, then insert the region's entities;
  - upsert `players`;
  - `DELETE FROM in_flight`, then insert the cut's rows;
  - upsert `history`;
  - `UPDATE world SET world_tick = T`.

  An entity primary-key conflict aborts the transaction with `Corrupt("entity <id> in two regions …")`: that is a conservation bug, and it must stay loud.

- [ ] **Step 1: Write the failing tests** (SQLite memory; the same body runs on pg under `MMO_TEST_DATABASE_URL`).
  - `cut_roundtrip`: commit a 2-region cut, then load. Regions, players, in-flight rows, history and `world_tick` are equal, and `terrain_version == 1`.
  - `second_cut_writes_only_given_chunks`: version 2 only for the region that had chunks.
  - `commit_cut_is_atomic`: a cut whose second region has a duplicate `EntityId` fails, and a load shows the previous cut unchanged.
  - `resolve_player_is_stable`: the same token gives the same id across a reopen; a new token gives the next id.
  - `u64_max_serial_roundtrips`.
  - `missing_region_is_none_not_error`.
  - `corrupt_chunk_is_error`: overwrite a `chunks.data` with garbage → `Err(Corrupt)`.
- [ ] **Step 2: Run** `just test-one world-store "--test it handle::"`. Expected: FAIL.
- [ ] **Step 3: Implement** the thread (one `tokio::runtime::Builder::new_current_thread()`, requests spawned as tasks, pool size `STORE_POOL_MAX`), the handle and the rows.
- [ ] **Step 4: Run** `just test-one world-store "--test it handle::"` and `just test-pg "--test it handle::"`. Expected: PASS.
- [ ] **Step 5: Commit**: `feat(world-store): store thread, StoreHandle, cut commit in one transaction`.

### Task 7: Server on the store

**Files:**
- Modify:
  - `crates/server/Cargo.toml` (+ `world-store`)
  - `crates/server/src/world_file.rs`: `resolve` → `WorldSource { Procedural, Store { url: StoreUrl, import: Option<PathBuf> } }`; `ensure_forged` → `ensure_world(handle, import, params)`; `ensure_history` reads and writes through the handle
  - `crates/server/src/lib.rs` (startup :488–629, main loop :648–695, exit :697–734, `forward_saves` :766, `forward_player_saves` :782)
  - `crates/server/src/region_threads.rs` (`with_store` :74, `spawn` :81)
  - `crates/server/src/player_store.rs`: add `StorePlayers(StoreHandle)`; remove `FilePlayerStore`
  - `crates/worldgen/src/forged.rs`: add `ForgedWorld::from_bytes(bytes: &[u8]) -> Result<ForgedWorld, ArtifactError>`. `content_crc` = crc32 of the given bytes, exactly as `load` (:636) computes it; `load` becomes `fs::read` + `from_bytes`
  - `crates/game/src/region_runner.rs` / `world_manager.rs`: `RegionOutput::LoadFailed(String)`; manager `unavailable: BTreeMap<RegionCoords, Tick>` (the world tick to retry at); `PlayRefusal::RegionUnavailable` (world_map.rs:135) and `EmbarkRefusal::RegionUnavailable` (history/chain.rs:14)
- Delete: `crates/server/src/region_store.rs`
- Tests: `crates/server/tests/store_world.rs` (own `[[test]]`, since it touches process-global forged state)

**Interfaces:**
- Consumes: `StoreHandle`, `restore_portable`, the cut API, `restore_in_flight`.
- Produces:
  - env `VOXEL_WORLD_DB`
  - `pub const STORE_LOAD_RETRY_SECS: u64 = 10`
  - `world_file::store_url_for(world_file: &Path) -> StoreUrl` (= `with_extension("sqlite")`)
- **Startup:**
  1. Resolve, then open the handle (a refusal is fatal and prints the `StoreError` sentence).
  2. If the store has no regions and a legacy `<world>.regions/` dir (or `<world>.players/`, `<world>.history`) exists, exit with: `this world has file saves from before the world store; delete them with: just clean-old-worlds`. *(As built: checked before the import, so a refused start changes nothing.)*
  3. If the store has no artifact: import `VOXEL_WORLD_FILE` if it exists (`put_artifact` with the file bytes and their crc32, which keeps `content_crc` identical to today), then delete the file; else forge into the store.
  4. `ForgedWorld::from_bytes`, then install.
  5. The clock starts at `world_info().world_tick`; `manager.set_stored_regions(stored_regions())` and `restore_in_flight(load_in_flight())`.
- **Main loop:** every `WORLD_SAVE_SECS`, `begin_cut(world_tick_now)`. Each iteration, `take_cut()` → `commit_cut`, and polled replies → `cut_committed` / `cut_failed`.
- **Region thread:** a `Deferred(None)` seed calls `load_region`:
  - `Ok(Some)` → `restore_portable`, runner `saved_terms` = the returned terms;
  - `Ok(None)` → generate;
  - `Err(e)` → send `LoadFailed(e.to_string())` and end the thread.
- **Manager on `LoadFailed`:** drop the link, mark the region unavailable until world tick + `STORE_LOAD_RETRY_TICKS` (= `STORE_LOAD_RETRY_SECS * 1000 / TICK_RATE`), keep arrivals in the mailbox, refuse `PlayRegion` / embark / return (and adventure start) there with `RegionUnavailable`, and log `error`. *(As built: the failed thread keeps its input channel open until the manager drops the link, so nothing sent meanwhile hits the dead-region respawn path; what was in flight to it goes back to the mailboxes; a session whose body was to be there is ended with `Refused`, keeping its stored record; a session still subscribed is subscribed again at the retry tick.)*
- **Orderly exit:** park all regions, then `begin_cut` (all parked, so it completes at once), then `take_cut` → `commit_cut`, wait for its reply, then `handle.shutdown()`.

- [ ] **Step 1: Write the failing tests.**
  - In `crates/game/tests/world_manager.rs`, `region_load_failure_never_regenerates`: a test spawner whose region (1, 0) answers `LoadFailed`. A slime crossing into it waits in the mailbox; `PlayRegion` there is refused `RegionUnavailable`; after the retry tick the manager spawns it again.
  - In `crates/server/tests/store_world.rs`, on a temp dir:
    - `restart_restores_world`: an inline server core on a forged test world (as built: 2 km / 64 m cells, seconds to forge), with a dug block and a slime. Exit, reopen, and the block is still dug, the slime exists once, and `world_tick ≥` the old one.
    - `legacy_saves_refuse_start`.
    - `import_deletes_forge_file`.
    - `procedural_persists_nothing`.
- [ ] **Step 2: Run** `just test-one game "--test it world_manager::region_load_failure"` and `just test-one server "--test store_world -- --include-ignored"`. Expected: FAIL.
- [ ] **Step 3: Implement** the above. Remove `region_store.rs` and `FilePlayerStore` from the server.
- [ ] **Step 4: Run** both test commands, then `just test-threads`, `just test-one server "--test threaded_world -- --include-ignored"` and `just smoke`. Expected: PASS.
- [ ] **Step 5: Commit** the code: `feat(server): world store replaces region/player/history files; cut autosave; load failure stops the region`.
- [ ] **Step 6: Commit the docs separately.** Update the CLAUDE.md `crates/server` paragraph (world autosave → world store, `VOXEL_WORLD_DB`) and the `crates/server` lines on players. Commit: `docs(CLAUDE.md): world store`.

### Task 8: LOD and halo from stored regions

**Files:**
- Modify:
  - `crates/game/src/lod_builder.rs`: `LodBuildJob` (:22) gains `Stored { key, region }`; `Tile` gains `stored: Vec<RegionCoords>`; `build_lod_job` (:66) takes `stored: &dyn StoredTerrain`
  - `crates/game/src/world_manager.rs`: `start_lod_build` (:2897), `start_tile_build` (:2949), `seed_halo` (:2477), `set_stored_regions`
  - `crates/server/src/lod_workers.rs`, `crates/server/src/region_threads.rs` (the halo generator consults the store first)
- Create: `crates/game/src/stored_terrain.rs`; `crates/world-store/src/terrain.rs` (`pub struct TerrainFromStore(pub StoreHandle)`, implementing `game::StoredTerrain` via `load_terrain`)

**Interfaces:**
- Produces:
  - `pub trait StoredTerrain: Send + Sync { fn load_terrain(&self, rc: RegionCoords) -> Result<Option<TerrainSnapshot>, String>; }`
  - `pub struct NoStoredTerrain` (tests and wasm)
  - `WorldManager::set_stored_regions(&mut self, regions: BTreeMap<RegionCoords, u64>)`
- **Rule:**
  - A region that is not running, not in `terrain_cache` and in `stored_regions` is built as `Stored`. The worker loads it and caches nothing in the manager.
  - A load error or `None` logs `warn` and builds pristine; LOD shows terrain, it never stops a region.
  - `cut_committed` invalidates LOD keys of regions whose `terrain_version` rose and that are not running (running ones already push `TerrainChanged`).
  - `seed_halo` for a stored, not-running neighbour sends `SeedHaloFromWorldgen` as today, but the server's halo generator closure returns the stored chunks in the facing windows when the store has the region.

- [ ] **Step 1: Write the failing tests** in `crates/game/tests/lod_streaming.rs`:
  - `stored_region_lod_shows_saved_edit`: a fake `StoredTerrain` returns a region with one dug column. The `RequestLod` result for that region differs from pristine exactly there.
  - `tile_overlays_include_stored`.
  - `stored_neighbour_seeds_halo_from_store` (halo test file).
- [ ] **Step 2: Run** `just test-one game "--test it lod_streaming::stored"` and `just test-one game "--test it halo::stored"`. Expected: FAIL.
- [ ] **Step 3: Implement** the rule.
- [ ] **Step 4: Run** the same commands, then `just test-threads` and `just smoke`. Expected: PASS.
- [ ] **Step 5: Commit**: `feat: LOD and halo read stored regions (fixes pristine LOD of saved regions)`. Then commit the CLAUDE.md "Known limit" sentence removal separately.

### Task 9: Single-player and the client

**Files:**
- Modify:
  - `crates/worldgen/src/world_store.rs`:
    - add `pub const STORE_FILE: &str = "terrain.sqlite"`;
    - `check_terrain` (:50) accepts a dir with `terrain.sqlite`, checking `meta.artifact_version` against `ARTIFACT_VERSION`, or with `terrain.world` (pending import, as today);
    - `WorldEntry` gains `store: PathBuf`
  - `crates/client/src/local_host.rs` (`start_on` :128: also sets `VOXEL_WORLD_DB=<entry_dir>/terrain.sqlite`)
  - `crates/client/src/debug_launch.rs` (unloadable detection unchanged in shape; `--reset` still removes the dir, so it must also remove `terrain.sqlite-wal`, `-shm` and `.lock`, which happens automatically with `remove_dir_all`)
- Test: `crates/worldgen/tests/world_store.rs` (or the existing listing tests)

**Interfaces:**
- Consumes: Task 7's env and import behaviour.
- Produces: a world dir holds `terrain.sqlite` (+ WAL sidecars), `world.json`, `preview.png` and `server.log`. A freshly generated dir holds `terrain.world` until the first server start imports it.

- [ ] **Step 1: Write the failing tests:**
  - `list_accepts_store_only_dir`;
  - `list_flags_old_artifact_in_store_dir` (meta `artifact_version` 6 → "needs re-forge");
  - `list_accepts_pending_import_dir`.
- [ ] **Step 2: Run** `just test-one worldgen "--test it world_store::"`. Expected: FAIL.
- [ ] **Step 3: Implement** the listing change and the env var.
- [ ] **Step 4: Run** the tests, `just smoke` (type-checks the client), and `just test-one client "--lib debug_launch::"`. That last one is a client test binary: **ask the user first**.
- [ ] **Step 5: Commit**: `feat(client): single-player worlds live in terrain.sqlite`. Then commit the CLAUDE.md "Worlds live in" sentence separately.

### Task 10: Delete the old world save files

**Files:**
- Create: `scripts/clean_old_worlds.sh`
- Modify: `justfile` (recipe `clean-old-worlds *args`)

**Interfaces:**
- Consumes: Task 7's startup refusal (`legacy_saves_refuse_start`), whose message names this recipe.
- Produces: `just clean-old-worlds [--yes]`.
  - It deletes, under `worlds/` only:
    - every `*.regions/` dir and `*.players/` dir;
    - every `*.history` and `*.history.bak`;
    - any `*.region.bak` left elsewhere.
  - It **never** touches `*.world` forge files, `world.json`, `preview.png`, `*.sqlite*` or anything outside `worlds/`.
  - Without `--yes` it only lists what it would delete, with sizes.

- [ ] **Step 1: Write the script.** Use `find worlds -maxdepth 2` with the patterns above. Print each path and `du -sh`, and delete with `--yes`. A missing `worlds/` dir is not an error.
- [ ] **Step 2: Dry run.** Run `just clean-old-worlds`. Expected: the list includes `worlds/debug-embarked/terrain.regions`, `terrain.players`, `terrain.history` and `worlds/default.regions`, `default.history(.bak)`, and does not include any `terrain.world`, `default.world`, `world.json` or `preview.png`.
- [ ] **Step 3: Show the user the dry-run list and get confirmation, then run** `just clean-old-worlds --yes`. The user asked for the deletion on 2026-11-07; the list is shown because `worlds/` is git-ignored and nothing deleted there can be recovered. Run it only after Task 7 has landed: before that, the running game still reads these files.
- [ ] **Step 4: Verify.** Run `just clean-old-worlds` again. Expected: nothing listed. A server start on `worlds/default.world` then imports it into `worlds/default.sqlite`, with no refusal. That last check is a live run: ask first.
- [ ] **Step 5: Commit** (`scripts/clean_old_worlds.sh`, `justfile`): `chore: just clean-old-worlds deletes pre-store save files`.

## Self-review notes

- **Handoff coverage:**
  - A0 → Task 1; A1 → Tasks 2, 6; A2 → Task 2.
  - A3 → Task 3. The equivalence is a portable-equality digest; the full `state_hash` is not expected to match.
  - A4 → Task 5; A5 → Task 7; A6 → Task 9; A7 → Task 8; A8 → Task 10 (user chose delete over convert).
  - `terrain_version` is in the schema from v1. NOTIFY waits for R3, as decided.
- **Out of scope** (deliberately): login, accounts, JWT, ownership epochs (schema comment only), k8s, rate limiting, backups.
- **Execution order:** 1–10 in order. Task 4 needs `RegionMeta` (Task 3). Task 6 needs `WorldCut` (Task 5).
