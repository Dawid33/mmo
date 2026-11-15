# Handoff: write two implementation plans — (A) R0/R1 world persistence on Postgres + SQLite, (B) the forged world in the database

**Date:** 2026-11-06
**For:** a fresh session whose job is to **write two implementation plans**. It does **not**
implement them. Use the `superpowers:writing-plans` skill for each plan.
**Outputs:**
- `docs/superpowers/plans/2026-11-xx-world-store-r0-r1.md` (Plan A)
- `docs/superpowers/plans/2026-11-xx-forged-world-in-db.md` (Plan B)

Use the date you write them on.

## 1. Read first (in this order)

1. `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md` **§0** — the
   settled design. The most important parts:
   - **§0.9** (Postgres + SQLite, the forged world tiled in the database, the panel,
     LISTEN/NOTIFY);
   - **§0.10** (final decisions, roadmap R0–R6);
   - **§0.2** (the crash-consistency problem and the world-tick cut);
   - **§0.1** (what shipped since September).

   §1–§12 are the September record; read §6 (persisted shapes, entity rows) and §8 (LOD
   across instances) for background.
2. `docs/superpowers/plans/2026-11-06-seam-transparent-regions.md` — **the prerequisite**,
   being implemented now. Plan A consumes its `WorldClock` / world tick, `EntityId`, generated
   transfer bundle and the manager's `in_flight` table.
   **Check how far it has landed before relying on its interfaces:** `git log`, and grep for
   `EntityId`, `WorldClock`, `in_flight`. `region_store.rs` already has `save_world_tick` /
   `load_world_tick`, so its Task 1.2 is in.
3. `docs/superpowers/specs/2026-09-20-durable-world-persistence-design.md` and
   `docs/superpowers/plans/2026-09-22-durable-world-persistence.md`. That is the old,
   **never-executed** Stage 0 Postgres plan, marked obsolete. Reuse its findings (no terrain-edit
   event, restore-by-rebuild resets slotmap keys, golden-blob tests), not its decisions:
   - it chose "reject double login", but newest-wins shipped;
   - it planned a per-process ClientId as the account id, but `PlayerId` now exists;
   - it assumed Postgres only, but SQLite is now required too.
4. `CLAUDE.md`: the build/test rules, `crates/server` (world autosave paragraph),
   `crates/worldgen` (forged worlds paragraph), the client's single-player server spawn and the
   generator.

## 2. Decisions already made (do not re-open)

From the user, 2026-11-06 (design doc §0.3, §0.6, §0.9, §0.10):
- **Postgres is the store for the cluster. The same schema must work on SQLite** so
  single-player / single-server needs no Postgres. One `.sqlite` file per world replaces
  today's `terrain.world` + `terrain.regions/` + `<world>.players/` + `terrain.history`.
- **One logical database per world.** No `world_id` columns inside a world's database.
- **The portable region format replaces the memory-image blob.** Terrain + entity bundles
  keyed by `EntityId` + region meta. The `Rollback` blob may survive only as an optional
  same-build fast-resume cache.
- **Saves are cut at one world tick** (consistent across regions, including in-flight
  transfers).
- **Freeze on first run**: every region that ever ran is stored. **Regions are not
  pre-generated.**
- **The forged world is stored in the database, tiled, plus a coarse global layer. Hosts pull
  only what they need.** Worldgen runs up front (later from the management panel, as a
  `forge` k8s Job; single-player forges in the client as today).
- **LISTEN/NOTIFY** for terrain changes (needed from R3; the schema should carry
  `terrain_version` from the start).
- **sqlx** was the user-approved Postgres library (2026-09-22). sqlx also covers SQLite.
- **No backups yet.** **Seamless crossing only.** Rate limiting is **deferred** (§0.11) — do not
  plan it.
- Not in these plans:
  - login / JWT / accounts / coordinator split / the panel — R2;
  - the owner-epoch fencing / ownership tables — R2 / R3. Mention them only as a schema
    extension point.
  - k8s — R5.

## 3. The code today (verified 2026-11-06 — re-verify; the seam plan is changing some of it)

**Persistence (server):**
- `crates/server/src/region_store.rs`:
  - per-region files `<world>.regions/b<R>-c<C>/r.x.z.region`;
  - header: magic, `REGION_FILE_VERSION` (7), the dials, `content_crc`, payload crc;
  - payload: lz4 bincode `Rollback`;
  - written by `spawn_saver`'s thread; `save_world_tick` / `load_world_tick`; unreadable files
    are set aside as `.bak`.
- `crates/server/src/player_store.rs`:
  - `trait PlayerStore: Send { fn resolve(&mut self, token: &PlayerToken) -> io::Result<(PlayerId, Option<PlayerRecord>)>; fn store(&mut self, records: Vec<(PlayerId, PlayerRecord)>) -> io::Result<()>; }`;
  - `MemoryPlayerStore`, `FilePlayerStore` (`<world>.players/`), `SharedPlayerStore`
    (`Arc<Mutex<dyn PlayerStore>>`);
  - `spawn_saver` (the `player-saver` thread); `resolve_hello`; `HELLO_MAX_BYTES` /
    `HELLO_TIMEOUT`.
- `crates/server/src/world_file.rs`: `resolve` (`VOXEL_WORLD_FILE` → `WorldSource`),
  `ensure_forged` (auto-forge if missing), `ensure_history` / `save_history`,
  `set_aside_path`.
- `crates/server/src/lib.rs`:
  - `WORLD_SAVE_SECS` = 60 (`VOXEL_SAVE_SECS`);
  - the main loop drives `WorldManager::request_save` → `take_saves()` → the world-saver
    thread, and `take_player_saves()` → the player-saver;
  - orderly exit (SIGINT/SIGTERM, last client gone, stdin closed) parks everything, saves and
    joins the savers.
- `crates/server/src/region_threads.rs`: `ThreadRegionSpawner::with_store` (a region that has
  not run this process loads its file on its own thread).

**Sim side (game):**
- `world_manager.rs`: `request_save` (~l.1519), `take_saves` (~l.1542) →
  `Vec<(RegionCoords, RegionSave)>`, `take_player_saves` (~l.1747), `set_history` (~l.722).
- `region_runner.rs`: `restore_saved(id, rollback) -> Region` (~l.189; drops stale players
  and history people), `SerializedRegion`, `RegionSeed::{Fresh, Parked, Deferred}`.
- `player.rs`: `PlayerId(u64)`, `PlayerToken(String)`,
  `PlayerRecord { client: Client, pose: Pose, region: RegionCoords, fortress: Option<SiteId>, adventure: Option<SiteId> }`.
- `history/persist.rs`: `encode` / `encode_v2` / `decode(bytes, SocietyGrid)` / `save` /
  `load` / `history_path_for`. History is world-global and stays a blob row for now.
- `GameData` (`state.rs`) — **the planning session must enumerate every field** and classify
  it for the portable format (§5 A3).

**Forged world (worldgen):**
- `crates/worldgen/src/forged.rs`:
  - `ARTIFACT_VERSION` = 7; `WorldArtifact` (~l.281): quantised `QRaster` u16 rasters, u8
    0–100 rasters, the region-id raster + region records, lakes, river reaches, volcanoes,
    edges, the geology column, spawn;
  - `to_bytes` / `from_bytes` (zstd, row-delta) / `save` / `validate` /
    `read_artifact_version`;
  - `ForgedWorld::new` / `load` / `artifact` / `content_crc` / `sample` / `patch` /
    `extended_cell_height` / `column_climate` / `biome_at` / `region_cell`;
  - **process-global** `install(Option<ForgedWorld>)` / `active()`;
  - coast distance recomputed on load;
  - ~27 B/cell resident, **~1 GiB for 100 km / 16 m**.
- `crates/worldgen/src/forge/play.rs`: `forge_to_file(...)` (~l.180, the client generator's
  forge into the world's staging dir), `forge_estimate`, `check_play_ram`,
  `MAX_PLAY_FORGE_RAM_BYTES` (12 GiB).
- `crates/worldgen/src/world_store.rs`: the menu's on-disk world listing —
  `worlds/<slug>/{terrain.world, world.json, preview.png}`, `WorldMeta`, `LEGACY_WORLD`. The
  client's `frontend/load_world.rs` and `menu_background.rs` read it.
- `crates/worldgen/src/tile_map.rs`: `build_world_map` (the `WorldMap` the server sends on
  connect; history's `SocietyGrid::from_world_map` uses it).
- `crates/worldgen/src/cache.rs`: the worldgen disk cache, keyed by the world file's crc.
- **LOD** (`game/src/lod.rs`, `lod_builder.rs`, server `lod_workers.rs`) samples pristine
  terrain through `coarse_tile` / the forged world out to the 15 km horizon.
  - Known limit: a saved region that has not run this session gets pristine LOD.
  - The LOD sources are running / parked regions or worldgen.

**Client single-player:**
- The client spawns the `server` binary per world with `VOXEL_SINGLE_PLAYER=1`,
  `VOXEL_WORLD_FILE`, `VOXEL_SERVER_PORT`.
- Debug presets (`client debug embarked`) reuse `worlds/debug-embarked/`; `--reset`.
- `frontend/generator.rs` forges via `play::forge_to_file`.
- The browser never stores anything.

## 4. Constraints the plans must carry (into each plan's Global Constraints)

- **Workspace:**
  - `develop`, the main checkout, **no git worktrees** (also for subagents).
  - The user has uncommitted work: `git add` only a task's files, and `git status` before
    commits.
- **Crate boundaries:**
  - `game` stays Bevy-free, IO-free, sync and wasm-safe — **no sqlx / tokio / filesystem in
    `game`**. Storage traits that `game` needs live in `game` (as `RegionSpawner` / `LodBuilder`
    do); implementations live in a new native-only crate (suggested `crates/world-store`) or in
    `server`.
  - The wasm client must not pull sqlx.
- **Compile time** (CLAUDE.md, measured levers):
  - sqlx with the SQLite feature builds libsqlite3-sys (C). Check the cost and keep it out of
    the client's hot rebuild path if possible.
  - The native client needs SQLite only if it writes worlds itself (the generator,
    `--reset`). Consider having the client only spawn the server / `forge` subprocess, so the
    client binary does not link sqlx at all.
  - Decide and justify.
- **No compile-time DB:** do **not** use sqlx's `query!` macros against a live database. Use
  runtime `query()` or committed offline metadata, so `just smoke` / `check` / clippy never
  need Postgres.
- **Verification:**
  - `just test-one <crate> "<target> <filter>"`; `just smoke`.
  - `just test-threads` after touching `WorldManager` / `region_threads` / handoff.
  - `just determinism-wasm` if hashed / serialized sim types change.
  - **Never whole suites.** Client tests are a slow opt-in (ask).
  - Tests > 100 ms get `#[ignore = "slow: >100ms — run via just test-full"]`.
  - Postgres tests run only when a test database is available (e.g.
    `MMO_TEST_DATABASE_URL`; skip with a message otherwise). SQLite tests use in-memory or
    temp files and always run.
- **Tunables** are named `pub const`s. Serialization on the wire stays
  `encode_packet` / `decode_packet`.
- **justfile:**
  - new recipes for every new workflow, kept in sync: `just pg-up` / `pg-down` / `pg-reset` /
    `pg-psql`, a world-conversion tool, `forge` to a database, tests needing Postgres;
  - CLAUDE.md paragraphs updated in the task that changes the behaviour, as a separate commit.
- **Never regenerate over stored data:** a store read error at region start stops that region;
  it never falls back to worldgen silently (carry over from the 09-22 plan).
- **Never weaken the server state-hash check** (memory: never-skip-hash-verification).
- **Ask before** any benchmark, perf capture or live run (the user runs their own; one client,
  `adapter=Intel`).

## 5. Plan A — R0 + R1: the world store on Postgres and SQLite, one host

**Goal:** one server process whose whole world — the forged world (still as one blob in this
plan; Plan B tiles it), regions in the portable format, players, history and the world tick —
lives in either a Postgres database or a `.sqlite` file. The same code works against both, is
restart- and crash-safe at a consistent world tick, and builds LOD from stored regions.

**Suggested task outline** (the planning session refines, orders and sizes it per the skill):

- **A0 — dev Postgres (R0):**
  - `deploy/dev/compose.yml` (Postgres, pinned major version, a named volume, port bound to
    127.0.0.1) — separate from the dead `deploy/web/compose.yml`;
  - `just pg-up` / `pg-down` / `pg-reset` / `pg-psql`;
  - a `DATABASE_URL` default for dev;
  - a test-database convention.
- **A1 — the store crate and traits:**
  - in `game` (sync, IO-free): a `WorldStore` (or split: `RegionStore`, `PlayerStore` moved
    from `server`, `HistoryStore`, `WorldMetaStore`) with a `MemoryStore` for tests / wasm;
  - in the new native crate: `SqliteStore` and `PgStore` on sqlx;
  - **sync vs async:** savers and region threads are sync crossbeam threads, sqlx is async.
    Decide how the bridge works — e.g. one store thread owning a tokio current-thread runtime,
    fed by channels, like the network edge. Say why.
  - Two migration sets, `migrations/sqlite` and `migrations/pg`, plus a test that applies both
    and compares the logical schema (tables, columns, types mapped).
- **A2 — schema v1:**
  - `world` (format version, dials `REGION_BLOCKS` / `CHUNK_BLOCKS`, `content_crc`, world
    tick, `super_region_regions` placeholder);
  - `forged_artifact` (one blob row in this plan);
  - `regions` (rx, rz, meta, `terrain_version`, saved tick);
  - terrain rows (per sim chunk or per column — decide, with numbers);
  - `entities` (`EntityId` → region, `seq`, bundle);
  - `players` (`PlayerId`, token, record);
  - `history` (blob);
  - `in_flight` (transfers at the cut).
- **A3 — the portable region format:**
  - **Classify every `GameData` field** as persist / derive on load / drop, with an exhaustive
    test that fails when a new field is added unclassified (the seam plan's bundle
    classification is the pattern). Fields to think about at least: chunks + chisel, implied
    `ground`, designations, item piles, containers / chests, entities (players, slimes, people,
    walkers, animators), voxel bodies (transient), ghosts / halo (drop), the RNG-free timers,
    and the tick.
  - Restore = build from terrain + spawn the entities through the arrival path (what exists
    after the seam plan — check).
  - **Equivalence test:** save → restore → a terrain + entity digest is equal. The full
    `state_hash` is not, because physics handles are rebuilt; a restored region has no
    subscribers, so this is fine (§6).
  - **Write only what changed:** dirty chunks via the per-chunk crc vector that
    `terrain_fingerprint` already computes.
- **A4 — the world-tick consistent cut:**
  - `request_save` at world tick T;
  - every running region snapshots at T (no waiting — the seam plan's "no region waits");
  - the manager adds `in_flight` transfers departed < T and not yet applied, plus parked
    players and history;
  - **one transaction per cut**;
  - restore puts in-flight transfers back in flight;
  - a test with a transfer straddling the cut: no duplicate, no loss.
- **A5 — server integration:**
  - replace `region_store.rs` files, `FilePlayerStore` and the history file with the store;
  - config: `VOXEL_WORLD_DB` = a `.sqlite` path or a `postgres://` URL (decide the name and
    what happens to `VOXEL_WORLD_FILE`);
  - `ensure_forged` writes into the store;
  - the procedural world (`procedural`) keeps persisting nothing, as today;
  - the orderly exit flushes the final cut.
- **A6 — single-player and the client:**
  - `worlds/<slug>/world.sqlite` (+ `world.json` / `preview.png` as the menu's metadata, or
    move those into the database too — decide);
  - the generator forges into it;
  - debug presets and `--reset`;
  - the menu's world list and "needs re-forge" check.
- **A7 — LOD from stored regions:** a stored-but-not-running region becomes an LOD overlay
  source (`LodBuildJob`), fixing today's known limit; `terrain_version` is the invalidation key.
- **A8 — migration of existing worlds:** a `just` tool that converts `worlds/<slug>/` (file
  artifact + region files + players + history) into `world.sqlite`, or a documented decision
  to drop old dev saves. **Ask the user.**

**Design questions the planning session must settle** (present each with its trade-off in prose
before options — memory: design-questions-need-tradeoffs; ask the user only those that are
genuinely theirs):
1. Terrain row granularity: per 16³ sim chunk vs per chunk column vs per region. Weigh write
   amplification, row count and restore cost, with sizes measured on a real 256 m region —
   measuring is allowed, a perf *benchmark* run needs the user's OK.
2. Trait shape: one `WorldStore` vs several per-concern traits; where `PlayerStore` lives after
   the move.
3. The sync / async bridge.
4. Whether the native client links sqlx (compile-time cost) or delegates world writes to the
   server / a `forge` subprocess.
5. Whether `world.json` / `preview.png` move into the database.
6. Keep or drop the `Rollback` blob as a same-build fast-resume cache. Default: drop (YAGNI)
   unless restore latency measures badly.
7. Existing worlds: convert or drop (A8).

## 6. Plan B — the forged world in the database, pulled piecewise

**Goal:** the forge writes its output into the world store as **global small tables + tiled
rasters (+ a coarse layer)**. `ForgedWorld` samples through a tile source that loads only the
tiles it touches, and **every sample is bit-identical to today's whole-raster sampling.** A
host's memory then scales with its territory and view, not the world size (today ~1 GiB per
process for 100 km / 16 m).

**Depends on:** Plan A's store crate and schema mechanism (it adds tables to the same
migrations). It can start in-memory (a `TileSource` over a `WorldArtifact`) before the
database parts land.

**Suggested task outline:**
- **B1 — `TileSource` abstraction:** `ForgedWorld` samples rasters through a trait (whole
  in-memory artifact vs tiled with an LRU cache over a loader). The **pinning test** samples
  `sample` / `patch` / `column_climate` / `biome_at` / `region_cell` / `extended_cell_height`
  over a whole small world on both sources: bit-equal.
- **B2 — tile layout:**
  - tile size and halo, derived from what sampling reads across a tile edge: bicubic
    neighbourhood, river channel carving reach, lake berms, the biome dither
    (`BIOME_DITHER_M` 24 m), `extended_cell_height`;
  - recommend aligning tiles to super-regions (S = 16 regions) or a divisor;
  - coast distance: store it as a tiled raster, or prove a per-tile recompute with halo is
    exact;
  - lakes / rivers / volcanoes / biome-region records / edges / geology / spawn stay global
    tables (small) — verify their sizes on a 100 km world.
- **B3 — the coarse layer** (for the world map, the menu preview and possibly far LOD). Decide
  the LOD question explicitly:
  - (a) LOD keeps sampling full-res tiles through the LRU — exact, but memory grows with the
    15 km view;
  - (b) LOD factors ≥ some threshold sample the coarse layer — less memory, but changes LOD
    output, and the ring crack-free invariant (any-solid coarse ⊇ fine) must still hold.

  Recommend (a) first unless memory measures badly.
- **B4 — `WorldMap` stored at forge time** (it feeds the client map and `SocietyGrid`), so
  hosts never rebuild it from whole rasters.
- **B5 — writers and readers:**
  - `forge_to_file` → `forge_to_store` (single-player: the `.sqlite`);
  - a `forge` CLI subcommand that writes to any `DATABASE_URL` — **the panel's forge `Job`
    will call it in R2**;
  - `content_crc` defined over meta + tiles (the worldgen cache key and the region-save
    world check depend on it);
  - artifact versioning (`ARTIFACT_VERSION` successor).
- **B6 — install / active:** `forged::install` / `active()` stay process-global, but backed by
  the store-loading tile source. Region threads may block on tile loads, since they already
  generate on their own thread. Optional prefetch of a region's tiles when it starts.
- **B7 — replace Plan A's single artifact blob** with the tiled tables; convert
  existing `.sqlite` worlds or re-forge (ask).

**Design questions for Plan B:**
1. Tile size and halo — derive them from the sampling code, don't guess.
2. Coast distance: stored vs recomputed.
3. LOD (a) vs (b) above.
4. The LRU budget (a named const) and tile row compression (keep zstd + row-delta per tile).
5. Whether the single-player `.sqlite` also stores tiles, or keeps one blob for simplicity.
   Recommendation: tiles everywhere, one code path.

## 7. How to work

- Use the **Explore** agent for the code surveys (the `GameData` field inventory, every reader
  of `forged::active()`, sampling's cross-edge reads, the LOD sources, the client world
  listing) and keep only the conclusions. Verify any claim a subagent makes about code before
  it goes into a plan.
- Plans follow the writing-plans skill exactly: header, Global Constraints, Review Focus, tasks
  with Files / Interfaces / steps, a self-review.
- Plan A's Review Focus must at least cover:
  - a crash mid-cut;
  - a transfer straddling the cut;
  - a region whose store read fails (it stops, never regenerates);
  - a `.sqlite` opened by two servers (the file lock refuses the second);
  - a world saved under other dials (`REGION_BLOCKS` / `CHUNK_BLOCKS`) — refused with a clear
    message.
- Plan B's Review Focus must at least cover:
  - sampling exactly on a tile edge / corner;
  - rivers crossing tiles;
  - the world's outer seam (`MapExtent`, past-edge sampling);
  - LRU eviction under the 15 km LOD view;
  - two hosts reading the same tiles.
- Don't write code beyond the plans. Commit the plan files only if the user asks.
- When both plans are written, report their paths, the design questions you settled (and how)
  and the ones left for the user. Then offer the execution choice per the skill.
