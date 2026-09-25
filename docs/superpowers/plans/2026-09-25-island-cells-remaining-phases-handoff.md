# Handoff — island regions + cell loading: remaining phases (2026-09-25)

Start here, then read the spec:
`docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md`
(approved; its "Build order" section lists 7 phases, each gets its own plan via the
writing-plans skill; the user picked **native (inline) execution** for every plan so far,
with one fresh reviewer at the end).

Plans already executed: `docs/superpowers/plans/2026-09-25-cell-loading-phase1.md`,
`…-phase1b.md`, `…-phase2-large-regions.md`. Separate open bug handoff:
`docs/superpowers/plans/2026-09-25-cell-lod-flicker-handoff.md` (LOD flicker at large
region sizes — fix landed, awaiting the user's visual verdict; do that first if the
user reports it's still broken).

## User decisions (binding, from the design conversation)

- An island IS a region. One whole region runs on ONE thread (no per-chunk / per-region
  multithreading inside an island). Clients hold the whole island sim and advance it in
  lockstep (full replica — the user rejected state replication).
- Cells = Minecraft-style chunk loading inside that one sim: only active cells execute,
  and every client executes every active cell. Server authors `CellChanges`.
- Infinite ocean of islands; islands up to 64 km (`region_chunks` up to 32768).
- Leaving an island → open ocean as a temporary per-player region, fog narrows, MVP
  fast-travel menu. No gameplay between islands.
- Groups of adjacent islands share a Kubernetes pod (separate track).
- Idle islands wind down, save to Postgres, shut down.
- "Don't load surrounding regions, even in full world, only load the one region."
- The raw snapshot must shrink and be compressed ("make very large islands possible").

## Status by spec phase

Note: my plan names diverged from the spec numbering. "Phase 2 large regions" (done)
is NOT the spec's phase 2 ("cheaper activation").

| Spec phase | Status |
|---|---|
| 1 Cells inside one region | DONE (+1b wiring into server/client, +1c centre spawn & home-LOD masking) |
| (extra) Large regions | DONE: lazy per-cell worldgen (`worldgen::generate_cell`, bit-identical to `generate_region`), `CellStore::lazy` + worker-thread prefetch, sizes to 32768 chunks, only the home region loads (`LOD_RING_TABLE` >128 = `[(0, LARGE_HOME_FACTOR)]`), pristine coarse LOD for ungenerated cells, cell LOD churn fix `aae81f9` |
| 2 Cheaper activation | PARTLY DONE (plan `2026-09-25-cell-loading-phase2-cheaper-activation.md`): chunk index + incremental fingerprint landed; genesis timing pending the user's `cell_profile --time` run; water collapse deferred to 3/6 |
| 3 Island worldgen | NOT STARTED |
| 4 Wire (zstd, framed snapshot, catch-up) | NOT STARTED (packet cap overflow on wasm32 was fixed in phase-2-large) |
| 5 Postgres cell store + wind-down | NOT STARTED |
| 6 Ocean region + fast travel | NOT STARTED |
| 7 Pods | separate track, NOT STARTED |

Run it: `just region_chunks=1024 cells=16 run` (2 km region) or
`just region_chunks=128 cells=16 world=tiny run`. Knobs are BUILD-time
(`VOXEL_REGION_CHUNKS`, `VOXEL_CELL_CHUNKS`); switching recompiles `game` and dependents.
Alternating configs once produced "undefined hidden symbol" link errors — fix is
`cargo clean -p game -p server`.

## Phase 2 status (2026-09-25, second session)

Landed: `25233b7` `cell_profile` example (composition; `--time` for timings),
`6f690d2` persistent `GameData.chunk_index` (no more whole-region scans in
activation / release / `cell_hash` / `cell_terrain_term` / block edits),
`297f0dc` incremental terrain fingerprint via `GameData.cell_edits`.
Measured (1024/16, 5×5 around spawn): ≈770 chunks per 32 m cell, 19 k
chunks, 24 k colliders, 0 all-water chunks, ≈70 KiB raw / 15 KiB lz4 per
cell payload. Still open: run
`VOXEL_REGION_CHUNKS=1024 VOXEL_CELL_CHUNKS=16 cargo run --release -p worldgen --example cell_profile -- --time`
(user's call — benchmarks) to see what genesis per cell costs now; then
decide on compact `Voxels`. Pre-existing unrelated failure:
`collider_genesis::mixed_chunk_gets_block_and_voxel_scale_colliders`
(expects a 1.0-scale collider, gets the 8.0 half-block grid since 6736620).

## (Original notes) spec phase 2 — cheaper activation

Known costs from live runs (server `[server]` lines / perf logs, 1024/16):
- Server tick spikes up to ~64 ms from cell genesis inside the tick
  (`materialize_cell` → raw genesis of ~16×16 columns of chunks + colliders).
- Terrain fingerprint every `TERRAIN_HASH_INTERVAL_TICKS` (25) costs ~15–20 ms per
  check (hashes every materialized chunk; `RegionRunner::terrain_fingerprint`).
  Candidate: incremental — per-cell terms are already tracked (`CellMode.terms`);
  keep a per-cell dirty flag set by edits and only rehash dirty cells.
- Client side was already moved off the sim thread (`3dd6796`: raw `AddTerrainChunk`
  + off-thread snapshot voxel queue).
Spec items: **measure genesis-only per-cell cost first** (ask the user — they run
benchmarks themselves), then all-water sub-chunk collapse (no entity, no collider)
and compact `Voxels` serde; `CellChanges` payload size (`crates/worldgen/examples/snapshot_size.rs`).

## Then: phases 3–6 (details in spec §3–§6)

- **3 Island worldgen**: hashed quadtree of slots (`ISLAND_MAX_CELLS`), island mask in
  `column()` (`crates/worldgen/src/pipeline.rs`) with a mirrored `DensityFn` oracle node,
  relief scaled to radius, `IslandInfo { id, origin, size_cells, spawn }` via an
  `IslandDirectory` closure into `WorldManager` (game must not depend on worldgen),
  `just island=<cells>` dial, cache key gains island params (region size + cells already
  in the key: `cache_key_for(seed, manifest, region_chunks)`).
- **4 Wire**: zstd format byte in `encode_packet`/`decode_packet` (server `zstd`,
  clients incl. wasm `ruzstd`), framed snapshot stream (`SNAPSHOT_FRAME_BYTES`,
  loading-bar progress), `JOIN_CATCHUP_MAX_TICKS` replacing the
  `input_buffer.len() > 1000` panic in `reconcile`.
- **5 Postgres cell store + wind-down**: fold in the durable-persistence plan
  (`RegionStore` trait, `MemoryStore`, `PgStore` sqlx + zstd bincode, golden blobs);
  hot store LRU `DORMANT_CACHE_MAX`, checkpoints, freeze-on-first-activation with
  `worldgen_version`, `ISLAND_IDLE_GRACE` wind-down. Local Postgres exists via
  `just deploy-local` (CNPG in k0s) — deployment only, not the inner loop.
- **6 Ocean region + fast travel**: per-player ocean region, water plane, fog override,
  `Transfer` to an island spawn (login uses the same path).

## Deferred minors / known limits (carry into plans as they become relevant)

- Walking into a NEIGHBOURING region in cell mode is unsupported (moot once islands
  are single regions + ocean transfer).
- Cell regions are never reclaimed (thread + store live for the process) — phase 5.
- Dead replay arm in the client; client-role guard leaves a subscription behind; the
  near-radius log line doesn't print; `materialize` rescans all chunks
  (`cell_chunk_keys` is O(total chunks) per cell — also costs `cell_terrain_term`).
- LOD at large sizes: a real edit still re-sends the whole home LOD key (thousands of
  cells) and the client swaps roots after the first meshed child — see the LOD
  handoff; long-term fix is per-cell LOD diffing / dirty-rect keys.
- f32 precision beyond ~16 km (deferred per spec).

## Working rules (CLAUDE.md + user feedback)

- `develop`, no worktrees; commit as you go (user said to commit everything).
- Verify with `just test-one <crate> "<--test target> filter"` (+ `-- --include-ignored`
  for slow-tagged cell tests: `cell_activation`, `cell_manager`, `region_runner`,
  `lod_streaming`), `just smoke`, `just test-threads` after manager/threading changes.
  Never whole suites. Ask before client test binaries, benchmarks, or launching the game.
- Don't build a different `region_chunks`/`cells` config while the user's game is
  running; end with `just region_chunks=1024 cells=16 build` so their next run is fresh.
- Keep the justfile, CLAUDE.md and TODO.md in sync with new knobs/recipes.
- Tunables as named constants (spec §9 lists them).
