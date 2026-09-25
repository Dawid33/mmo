# Island regions with deterministic cell loading — design

Status: design approved section-by-section 2026-09-25; nothing implemented.
Supersedes the "5×5 near window of 64 m regions" world model for islands. Folds in
`docs/superpowers/plans/2026-09-22-durable-world-persistence.md` (re-keyed from
regions to cells) and replaces the super-region claim unit of
`docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md` §1a.

## Goal

A world of islands in an infinite ocean. **One island = one region = one
deterministic lockstep sim on one thread**, replicated in full (its *active*
part) by every client on the island. Walking around an island never crosses a
region boundary. Islands may be large (tested small → large, up to 65.5 km);
inside an island, terrain is loaded Minecraft-style in **cells** around players,
deterministically, so every client executes exactly the same active cells. Idle
islands wind down to Postgres and their thread exits.

### Why (problem statement)

Today regions are 64 m squares, one OS thread each, and the client subscribes to a
5×5 window of them. Every 64 m of walking pays a handoff (extract → manager relay →
arrival), per-region clock alignment (`align_region`, `stalled`, nine `SyncClock`
streams sharing one `tick_rate`), and a window shift (a row of full region
snapshots). That is the disruption; the region-crossing convergence test has been
red since `8e90481`. Making a region a whole island removes crossings from normal
play. Making the island *fully* materialized does not scale (measured: a region's
cost is ∝ area — 578 MiB raw snapshot at 256 m), so the island is loaded in cells.

### Non-goals (MVP)

- Seamless sailing between islands (MVP: leave island → temporary ocean region →
  fast-travel menu).
- Interest-managed / partial replication. Every client on an island replicates all
  of its active state (decided: option B, full replica, lockstep).
- A derived-terrain (collider-free) snapshot — blocked by allocator history (§4).
- f64 physics / floating-origin physics (deferred; §5 precision).

## Decisions made during design

| # | Decision |
|---|---|
| D1 | Replication stays full-replica lockstep of the whole region (island); no state replication. |
| D2 | Region = island. Cells (64 m, today's region grid) are the load unit *inside* one sim — no seams, no ghosts, one rapier world, one key space. |
| D3 | The active cell set is **authoritative server input** (`CellChanges` event), not computed inside the predicted tick. |
| D4 | Clients never keep dormant cells; every activation carries the authoritative payload + version + server-computed materialized-cell hash; client hard-fails on mismatch. Version-keyed client cache deferred. |
| D5 | Dormant cells live in a server hot store backed by Postgres; storage holds only overrides vs worldgen, payloads are complete. Freeze-on-first-activation. |
| D6 | Ambient slimes are kept in dormant cells (not despawned) by default. |
| D7 | Join snapshot = bit-exact `Rollback` of active state; zstd (+ `ruzstd` on clients), framed transfer, time-bounded catch-up. |
| D8 | Islands placed by a hashed quadtree of slots; island region = power-of-two cell square; `island=<cells>` test dial up to 1024 cells. |

## 1. Model and activation protocol

**Terms.**
- **Region / island**: one thread, one `Rollback`, one rapier world, one event
  stream, island-local coordinates.
- **Cell**: a square of `CELL_CHUNKS`×`CELL_CHUNKS` chunks (default 32 = 64 m —
  today's region grid; `CellCoords` replaces `RegionCoords` inside an island).
  - **Active**: its chunks exist in the `Rollback`; every client ticks them.
  - **Dormant**: exists only in the server cell store (§3); clients hold nothing.

**Tickets (server side).** Once per tick the island runner computes the desired
active set from authoritative player positions:
- **Required**: each player's cell + its 8 neighbours — activated immediately,
  bypassing the budget, so a player can never walk into a dormant cell.
- **Wanted**: cells within `SIM_RADIUS_CELLS` (default 2 → 5×5, today's footprint)
  of any player — activated nearest-first, at most `MAX_CELL_ACTIVATIONS_PER_TICK`.
- **Release**: a cell deactivates only after being outside
  `SIM_RADIUS_CELLS + CELL_HYSTERESIS` for `CELL_RELEASE_GRACE_TICKS`.

**Event.**
```rust
GameEventKind::CellChanges {
    deactivate: Vec<CellCoords>,          // sorted
    activate: Vec<(CellCoords, CellPayload)>, // sorted by CellCoords
}
struct CellPayload { version: u64, materialized_hash: u32, content: DormantCell }
```
- At most one per tick, emitted immediately before the `Tick` it precedes;
  deactivations applied before activations.
- `origin_client() == None`, never predicted by clients. It lands through the
  existing reconcile insertion path (`crates/game/src/region.rs:180-360`): roll back
  predictions, apply at the confirmed head, `forget()`, shift predictions +1 id,
  replay. Cost per event on a client: one replay of the prediction lead (~2 ticks).
- Because it is only ever applied to confirmed state (server forgets after every
  event too), materialization uses the cheap **unlogged** genesis path.

**Rejected:** deterministic in-tick tickets from predicted positions — would make
activation undoable and turn position mispredicts into whole-cell genesis rollbacks.

**Correctness of reactivation (D4).** Deactivation happens at the same event id on
identical state everywhere, so nothing is transferred; the server alone writes the
cell to its store. Reactivation always carries the store's current copy (which
reflects any server-side change while dormant: restore from Postgres, format or
worldgen upgrade, admin writes). The server computes `materialized_hash` over the
cell *as materialized in its own sim* (chunk data + collider content hashes +
entity state); the client recomputes after materializing and hard-fails on
mismatch — catching platform drift in materialization (native vs wasm, libm) at
the moment it happens.

## 2. Materialize / dematerialize; entities in cells

**New sim state** (in `GameData`, hashed, snapshotted):
- `active_cells: BTreeMap<CellCoords, ActiveCell { chunk_keys: Vec<EntityKey>, version: u64 }>`
- `chunk_index: BTreeMap<ChunkCoords, EntityKey>` — persistent; replaces the
  full-scan rebuild in `crates/game/src/edit.rs:88`.
- `ChunkCoords` becomes island-local (already `usize`, non-negative with the
  island origin at its square's corner). `edit.rs` `locate()` bounds become
  "chunk exists in `chunk_index`".

**Materialize** (confirmed head, unlogged):
1. Chunks in canonical `ChunkCoords` order via the raw genesis path
   (`Rollback::seed_terrain_genesis`, `crates/game/src/state.rs:565`).
2. Frozen entities from the payload in stored order via a raw variant of
   `apply_arrival`.
3. Compute and check `materialized_hash`.

**Dematerialize** (server and all clients identically):
1. Extract every non-player entity whose position lies in the cell into an
   `EntityBundle`, sorted by persistent `EntityId`. (Players cannot be there —
   required 3×3; assert.)
2. Server only: write chunks + bundles to the cell store; bump `version` if
   changed (`terrain_fingerprint`).
3. Remove colliders, bodies, ECS rows raw, in a fixed order. Allocator free lists
   and handle recycling stay identical because every replica performs the same
   removals.

**Entities crossing cells.** Inside the active set: nothing (one physics world).
Leaving it:
- Slime edge leash (`crates/game/src/slime.rs`) generalises from the region edge to
  the active set with a margin.
- Backstop: an end-of-tick deterministic scan freezes any non-player entity whose
  position is in a dormant cell into that cell's store (the `scan_boundaries` →
  Departures → manager path, targeting the store). Runs inside the tick, so it is
  predicted and undoable like today's departures.
- Slime spawner picks columns only from `active_cells`; `SLIME_CAP_PER_CELL`
  replaces the per-region cap.

**Performance risk #1 — activation cost.** A 64 m cell is ~2.7k chunks; every
client pays activation in the same tick. The measured 0.37 s "region start"
includes worldgen; the genesis-only share is unknown. Mitigations: the cell-size
dial `cell_chunks` (16 → 32 m cells, 4× cheaper each), and phase 2 (collapse
all-water sub-chunks — no entity, no collider; compact `Voxels`). **Measure
genesis-only per-cell cost before phase 3** (the user runs benchmarks; ask first).

## 3. Cell store

Extends the durable-persistence plan (`RegionStore` sync trait in `game`,
`MemoryStore`, `PgStore` in `server` with sqlx + zstd bincode behind a format byte
+ golden blobs; persistent `EntityId`; login/logout as extraction/arrival; a store
error never falls back to regenerating over a stored row), re-keyed to cells.

**Layers.**
1. Active cells — truth is the sim.
2. Hot store per island runner: `BTreeMap<CellCoords, DormantCell>`, LRU-capped at
   `DORMANT_CACHE_MAX`; an entry is evictable only after Postgres acked its latest
   version.
3. Postgres — truth for everything not active:
   `cells(island_id, cx, cz, version, worldgen_version, terrain bytea)`,
   `entities(id, island_id, cell, bundle, seq)` (never delete-by-absence),
   accounts/player bodies as in the plan.
4. Worldgen — a cell that has never been activated has no row.

**Content.** `DormantCell { version, chunks, entities: Vec<EntityBundle> }`.
Storage holds only chunks differing from worldgen; the activation payload is
always complete (worldgen + overrides, rebuilt server-side), since clients never
run worldgen.

**Lazy generation off the island thread.** A prefetch ring
(`SIM_RADIUS_CELLS + PREFETCH_CELLS`) generates/loads cells on a worker pool (LOD
worker pattern) into the hot store before they are wanted. A wanted cell that is
not ready waits (ticket retries). A required cell that is not ready holds that
player at the cell edge and is logged as a failure mode.

**Checkpoints.** Every `CHECKPOINT_INTERVAL_TICKS` (30 s) the runner serializes
each *changed* active cell without deactivating it, `seq`-stamped. Deactivation
writes immediately. Freeze-on-first-activation: first activation writes a row
recording `worldgen_version`, so worldgen tuning never rewrites seen terrain.

**Wind-down.** No players → no tickets → every cell deactivates after its grace
and is flushed; the runner is then "warm" (thread + hot store, no sim cost). It
stays warm while any neighbouring island has players (cheap fast travel). With no
players on it or its neighbours for `ISLAND_IDLE_GRACE`, it flushes, awaits the
Postgres ack, and the thread exits.

**Wildlife (D6).** Frozen slimes are kept in `entities`. Alternative (not default):
despawn-on-freeze for ambient kinds via an `EntityKind` flag.

## 4. Join snapshot and the wire

**Join** (login or fast-travel arrival): `RequestSnapshot` →
`ServerPacket::Region(island, Rollback)`, then replay of events newer than
`base_event_id` — as today, but one region, containing only active cells.
One island = one clock: the per-region `SyncClock` alignment machinery is not
needed on an island (resolves the `TODO.md` "per-region clock tracking" item).

**Wire changes** (determinism-neutral):
1. **zstd** as a new format byte in `encode_packet`/`decode_packet`
   (`crates/game/src/protocol.rs:523-567`) for large packets; server encodes with
   `zstd`, clients (including wasm) decode with pure-Rust `ruzstd`. Measured ~3×
   over lz4 on a region snapshot (0.35 vs 1.15 MiB).
2. **Framed snapshot**: the encoded snapshot goes over its own QUIC /
   WebTransport stream in `SNAPSHOT_FRAME_BYTES` frames — removes the 2 GiB
   `MAX_DECOMPRESSED_PACKET_BYTES` ceiling (whose formula also overflows on
   wasm32), avoids one giant allocation, drives a loading-screen progress bar.
3. **Time-bounded catch-up**: replace the `input_buffer.len() > 1000` panic in
   `reconcile` with `JOIN_CATCHUP_MAX_TICKS`; after load the client fast-forwards
   through buffered events before presenting.

**Bandwidth.** `CellChanges` payloads ride the ordered event stream. A compact
64 m cell is mostly block data (~0.1 MiB raw pre-half-block; expect tens of KB
zstd — measure). Worst case while flying at one activation/tick ≈ 0.5–1 MB/s per
client, bounded by `MAX_CELL_ACTIVATIONS_PER_TICK` and cell size.

**Not possible here: derived-terrain snapshot.** After many activations/
deactivations the server's slotmap and rapier allocator state (free lists,
generations, handle order) is history-dependent; a client cannot rebuild it from
chunk data, and edits reshape colliders in place. The snapshot therefore stays a
bit-exact `Rollback`. Size wins come from zstd, fewer active cells, and
format-level slimming that preserves allocator state (compact `Voxels` serde,
all-water sub-chunk collapse). A derived snapshot would first require canonical
allocators — separate project.

## 5. Region = island

**Island placement: hashed quadtree.** The plane is tiled with root slots of
`ISLAND_MAX_CELLS`² cells (1024 → 65.5 km). Each slot holds one island or splits
into four, decided by `hash(seed, slot)` with a size distribution, down to a
minimum slot size. Pure and local (≈10 levels of descent to find the island at a
point), infinite, varied sizes; neighbours are adjacent slots.

**Geometry.** An island region is a power-of-two square of cells within its slot,
covering the island's bounding box + `ISLAND_SHORE_MARGIN_CELLS` of water; the rest
of the slot is open sea. `RegionCoords` → `IslandId` (slot level + coords).
`CellCoords` / `ChunkCoords` are island-local (origin at the square's corner).

**Precision.** Local positions ≤ 2²⁰ units: 8 mm ulp at the edge of a 65.5 km
island, ≤ 2 mm up to ~16 km. The cap is a tunable; the f64 / floating-origin
physics fix is deferred until real islands need it.

**Worldgen.** Island mask inside `column()` (`crates/worldgen/src/pipeline.rs:368`)
from world coordinates: falloff from the slot's island centre, coast-noise warped;
relief (`HEIGHT_SCALE`×`MOUNTAIN_OFFSET`), ridge wavelength and `RIDGE_WARP_M` scale
with island radius, clamped (≈100 m minimum ridge wavelength; relief ≈
min(190 m, 0.15·R)). Constants become `TerrainPipeline` fields. Mirror node
(`DensityFn::IslandFalloff`, subtracting `SAMPLE_OFFSET_*`) in the `#[cfg(test)]`
graph; oracle tests run with islands on. World-coordinate evaluation keeps
`coarse_tile` LOD (other islands on the horizon) unchanged. Worldgen cache key
(`crates/worldgen/src/cache.rs:133`) gains island params and `CELL_CHUNKS` (also
fixes today's cross-region-size key collision). Option reaches wasm via
`option_env!`.

**Spawn.** Worldgen provides `IslandInfo { id, origin, size_cells, spawn }` (spawn =
flat dry coastal point), injected into `WorldManager` as an `IslandDirectory`
closure (`game` does not depend on `worldgen`). Replaces the hard-coded block (8,8)
spawn (`crates/game/src/state.rs:499`).

**Temporary ocean region (MVP).** Departing past the island square's edge
transfers the player into a per-player ocean region: no terrain cells, a water
plane, swim/float; client fog override narrows the view while in it; dies when
the player leaves. A fast-travel menu issues a `Transfer` = arrival at the chosen
island's spawn (login uses the same path).

**Test dial.** `just island=<cells> run` (`VOXEL_ISLAND_CELLS`, `option_env!` for
wasm) forces the spawn island to `<cells>`² (1 → 64 m … 1024 → 65.5 km), next to
`cell_chunks` (16 | 32).

**Pods (separate track).** A pod claims a quadtree node, so an ancestor slot's
islands (neighbours by construction) share a pod — replacing S×S super-region
claims, same lease/epoch-fencing machinery as the multi-instance research.

## 6. Client rendering

- Activation is raw (no renderer emits), so applying `CellChanges` on the client
  emits `ClientUpdateEvent::CellActivated(cell)` / `CellDeactivated(cell)`; the
  bridge adds/retires chunk roots incrementally instead of the whole-region
  `NewRegion` rebuild.
- Simulate all active cells, **mesh only those within `VIEW_RADIUS_CELLS`** of the
  camera; other players' distant active areas render via LOD.
- Dormant terrain in view: the existing server-built LOD rings, recentred on the
  player, excluding active cells (instead of the near window). LOD keys stay on the
  64 m world grid. A deactivated cell keeps its mesh until its LOD replacement is
  visibly meshed (same rule as the LOD→near handoff).
- Island-relative render origin: chunk roots at island-local coordinates; other
  islands' LOD offset relative to the current island (avoids absolute-f32 jitter far
  from the world origin).
- Ocean region: water plane + fog override.

## 7. Removed / kept

- **Removed on islands:** 5×5 near window (`desired_window`, `update_window`,
  per-neighbour `RequestRegionConnection`, recentre margin); per-region clock
  alignment (`align_region`, `stalled`, multiple `SyncClock` streams); ghosts and
  intra-island neighbour handoff (`GhostUpdate`, most of `scan_boundaries`); the
  client `World` of 25 regions (now one island, plus one during a transfer).
- **Renamed/repurposed:** `REGION_CHUNKS` → `CELL_CHUNKS`; parking lot → cell
  store; `RegionRunner` → island runner; `region_threads` → one thread per island.
- **Kept:** `WorldManager` (keyed by `IslandId`), extraction/`EntityArrived`
  (island↔ocean, login, fast travel), LOD builder, `InlineSpawner` /
  `InlineSchedule`.
- `crates/client/tests/crossing.rs` becomes an island↔ocean transfer test; it no
  longer gates island play.

## 8. Testing

- **Determinism core** (`crates/game/tests/cell_activation.rs`, `HashMode::Full`):
  server- and client-mode replicas applying the same activate/deactivate sequence
  have equal `state_hash`; activate → deactivate → reactivate round-trips content
  and entities with matching `materialized_hash`; a `CellChanges` inserted amid
  client predictions converges after replay.
- **Seeded jitter:** an `InlineSchedule` mode that shifts `CellChanges` relative to
  inputs; failures name a replayable seed.
- **Headless convergence** (slow-tagged): a player walks across many cells;
  client == server every tick.
- **Store:** `MemoryStore` tests for wind-down, restore, freeze-on-first-activation,
  checkpoints; `PgStore` integration ignored unless `DATABASE_URL`.
- **Worldgen:** quadtree determinism / locality / neighbour consistency / size
  bounds; mask vs `DensityFn` oracle; spawns are dry.
- **Wire:** `zstd` encode → `ruzstd` decode round trip, frame reassembly, catch-up
  buffer.
- **Measurements (user-run):** genesis-only per-cell cost, `CellChanges` payload
  size, join snapshot size per footprint, client activation spike
  (`crates/worldgen/examples/snapshot_size.rs` for sizes).
- Verification per CLAUDE.md: `just test-one`, `just smoke`, `just test-threads`.

## 9. Limits and tunables

Named constants: `SIM_RADIUS_CELLS`=2, `CELL_HYSTERESIS`=1,
`CELL_RELEASE_GRACE_TICKS`, `MAX_CELL_ACTIVATIONS_PER_TICK`=1, `PREFETCH_CELLS`=1,
`VIEW_RADIUS_CELLS`, `DORMANT_CACHE_MAX`, `CHECKPOINT_INTERVAL_TICKS`,
`ISLAND_IDLE_GRACE`, `ISLAND_MAX_CELLS`=1024, `ISLAND_SHORE_MARGIN_CELLS`,
`SLIME_CAP_PER_CELL`, `MAX_PLAYERS_PER_ISLAND`, `SNAPSHOT_FRAME_BYTES`,
`JOIN_CATCHUP_MAX_TICKS`; build/run dials `cell_chunks`, `island`.

Known limits:
1. Activation cost — unmeasured, risk #1.
2. Every client simulates every active cell on its island → `MAX_PLAYERS_PER_ISLAND`.
3. One thread per island (the Minecraft main-thread ceiling).
4. f32 precision beyond ~16 km.
5. Allocator history rules out a derived-terrain snapshot.

## Build order (each phase gets its own plan)

1. **Cells inside one region** — tickets, `CellChanges`, materialize/dematerialize,
   in-memory store, determinism tests. Single square island, no worldgen change.
   De-risks determinism first.
2. **Cheaper activation** — measure, then all-water sub-chunk collapse and compact
   `Voxels` serde.
3. **Island worldgen** — quadtree, mask, `IslandInfo` spawn, `island=<cells>` dial.
4. **Wire** — zstd, framed snapshot, catch-up buffer.
5. **Postgres cell store + wind-down** — folding in the durable-persistence plan.
6. **Ocean region + fast travel.**
7. **Pods** (separate track).
