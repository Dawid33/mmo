# Server-Streamed LOD Rings (factors 2/4/16/64/256)

**Date:** 2026-07-10
**Status:** Approved design, pre-implementation

## Problem

The horizon ends at 8 regions (~8 km). Two more exponential LOD levels
(factors 64 and 256) should extend it to 32 regions (~33 km). At the same
time, the existing beyond-subscription rendering splits across two
mechanisms with real drawbacks:

- The factor-2 far ring streams **full chunks** (`FarSnapshot`) that the
  client downsamples — heavy on bandwidth for terrain that renders at 2×
  scale anyway.
- The factor-4/16 worldgen rings generate terrain **client-side** from a
  compiled-in seed: player edits are invisible one region past the far
  ring, a foreign-seed server would render wrong terrain, and every client
  burns CPU regenerating the same pristine regions.

Naively extending the client-side approach to factors 64/256 also has the
wrong cost shape: full-res generation for up to 4 225 regions in view, and
~250 MB of mostly-air 32³ grids under the current one-grid-per-region
packing.

## Decision summary

All LOD levels become **server-streamed**. The server generates or
downsamples LOD chunks on request, caches them, and pushes refreshes for
running regions whose terrain changes. Decisions made during brainstorming:

1. **New levels:** factor 64 out to Chebyshev bound 16, factor 256 out to
   bound 32. Ring table: `[(2, 2), (4, 4), (8, 16), (16, 64), (32, 256)]`.
2. **All four+ levels server-streamed** (4/16 and the new 64/256).
3. **Freshness:** running regions re-check their terrain hash on the
   existing snapshot cadence and re-push LOD on change. Parked/pristine
   entries are immutable until the region next runs.
4. **Factor-2 fold-in:** the `FarSnapshot` full-chunk far ring is replaced
   by factor-2 LOD streaming through the same pipeline.
5. **Architecture:** client-driven pull keyed by `(key, factor)`
   (Approach A), generalizing the proven far-ring subscribe/cache/push
   pattern; **tile-granular keys** for 64/256 so one response is one
   renderable grid.

## 1. Keys, wire protocol, shared code

### LodKey

```rust
pub struct LodKey {
    pub factor: u32,        // 2 | 4 | 16 | 64 | 256
    pub coords: [i32; 2],   // region coords (factor <= 32) or tile coords
}
```

A key names one streamable unit. `tile_regions(factor) = max(1, factor / 32)`:

| factor | key unit | grids per response (x/z) | keys in its ring |
|--------|-------------------|--------------------------|------------------|
| 2      | 1 region          | 16×16 groups             | ~16 regions      |
| 4      | 1 region          | 8×8 groups               | ~56 regions      |
| 16     | 1 region          | 2×2 groups               | ~208 regions     |
| 64     | 2×2-region tile   | exactly 1                | ~200 tiles       |
| 256    | 8×8-region tile   | exactly 1                | ~49 tiles        |

Total subscriptions at full view distance ≈ 530. Cell `group` keys inside a
response are key-local (region-local for factor ≤ 32, tile-local for
64/256), in units of the grid's world span (`32 * factor`), matching the
existing spawner contract. Vertical bands make a response carry 1–2 cells
per x/z group in practice.

### Packets

```rust
ClientPacket::RequestLod(LodKey)   // subscribe + build-on-miss
ClientPacket::ReleaseLod(LodKey)   // unsubscribe
ServerPacket::LodData(LodKey, Vec<(group: [i32; 3], PackedLodGrid)>)
```

`RequestFarRegion` / `ReleaseFarRegion` / `ServerPacket::FarRegion` and the
`FarSnapshot` type are **deleted**. `FAR_VIEW_RADIUS` is subsumed by the
ring table's `(2, 2)` entry.

`PackedLodGrid` is a run-length encoding of the 32³ voxel grid in standard
`x + 32*y + 32*32*z` order: `Vec<(run_len: u16, BlockId)>`. Terrain grids
are dominated by air/uniform-rock runs — ~64 KB raw shrinks to ~1 KB
typical. Decode restores the exact `Vec<Voxel>` layout the mesher expects.
Encode/decode live in `game` with round-trip tests.

### Shared code moves

`LodCell`, `downsample2`, `build_far_lod_cells`, `downsample_cells`, and
`block_scale_cells` move from `crates/client/src/renderer/…` into a new
`game::lod` module (they are already Bevy-free), taking their unit tests
with them. Server and client share one downsampling implementation; the
client keeps only meshing and entity plumbing.

## 2. Server: cache, sources, invalidation

All in `WorldManager` (threadless core, so the same logic serves the native
server, tests, and the wasm/offline `InlineSpawner` embedded server).

### State

- **Terrain cache** (existing `far_cache`, renamed `terrain_cache`):
  `RegionCoords → columns`, seeded by region threads pushing on
  terrain-hash change (the existing cadence/message, payload already is the
  column list) and on park (`Stopped`).
- **LOD cache**: `LodKey → LodData`. Built on miss, served on hit, shared
  across clients. LRU-capped by a named constant; keys with live
  subscribers are never evicted.
- **Subscriptions**: `LodKey → set<ClientId>` plus the reverse map for
  disconnect cleanup. `ReleaseLod` unsubscribes; disconnect clears.
- **Dependency index**: `RegionCoords → dependent LodKeys` (the region's
  own per-factor keys plus the 64/256 tiles covering it), for invalidation.

### Build sources (per key, on cache miss)

1. Region has authoritative state (running-and-seeded or parked):
   downsample its `terrain_cache` columns at the key's factor.
2. Never materialized, factor 2/4/16: full `generate_region` +
   downsample — exact, ~300 regions worst case around a spawn, cached
   forever and shared across clients.
3. Never materialized, factor 64/256: the new `worldgen::coarse_tile`
   sampler (§3) covers the pristine area of the tile in one pass; any
   region inside the tile that *does* have authoritative state is overlaid
   by downsampling its real columns into the affected cells.

A running region whose terrain isn't yet in `terrain_cache` is asked for it
(the existing request/reply used by far subscription today); the build
completes when the columns arrive.

### Workers

LOD builds run on a small worker pool (`LOD_BUILD_THREADS`, default 2), not
the manager thread — a factor-4 miss costs a full region generation
(~100 ms release) and must not stall event routing. Jobs dedup by key;
results return as messages; a result whose key has lost all subscribers is
cached (if under the cap) but not sent.

### Invalidation

On a region's terrain-change push: refresh `terrain_cache`, drop the
region's dependent keys from the LOD cache, rebuild any that have
subscribers, push fresh `LodData` to them. Cadence and hashing stay exactly
where they are today (in the region runner); region threads never
downsample.

## 3. Worldgen: coarse sampler

```rust
pub fn coarse_tile(key: LodKey, seed: u64, mats: Materials) -> Vec<LodCell>
```

Pure and deterministic (same key + seed → identical cells on every
machine). Builds one `ProfileGrid` spanning the tile, then for each of the
32×32 cell columns samples the profile once at the cell center, scans for
the surface, and fills:

- solid below surface, material from `mats.material_at(depth, y)` (grass/
  dirt/sand banding survives at cell granularity);
- water between surface and sea level;
- air above.

No block materialization, no chiseling. A factor-256 tile costs 1 024
profile samples instead of 64 full region generations — roughly three
orders of magnitude cheaper.

**Accepted inexactness:** point-sampling does not bit-match a downsample of
full-res terrain (a cell whose center is just off a cliff edge can differ
by one cell). Sub-pixel at the 9+ region distances where these factors
render. A tolerance test (§5) keeps the divergence bounded.

## 4. Client: one ring system, network-fed

`worldgen_rings.rs` → `lod_rings.rs`. The assignment/stickiness/retirement
machinery survives with slots keyed by `LodKey`:

- **Table:** `[(2, 2), (4, 4), (8, 16), (16, 64), (32, 256)]` (named
  constant, tunable). A tile key's distance is the Chebyshev distance from
  the viewer region to the tile's nearest region. Sticky band, retirement
  deadline, and takeover rules carry over unchanged in meaning.
- **Exclusion:** the near window (`Regions`) is the only exclusion set.
  `FarRegionRoots` and the whole `FarSnapshot` bridge path (`NewFarRegion`,
  `spawn_far_region`, `FarLodBuild`, `finish_far_lod_builds`) are deleted —
  factor 2 arrives as ordinary `LodData`.
- **Assignment → request:** where a slot today spawns a `WorldgenTask`, it
  sends `RequestLod(key)`; eviction past the sticky band sends
  `ReleaseLod(key)`. The client-side generation budget disappears (the
  server owns build scheduling); a slot is pending until data arrives.
- **Arrival:** `LodData(key, cells)` decodes grids and spawns a root at the
  key's world offset plus `LodCell` children under the existing spawner
  contract — translation `group * 32 * factor` with `-factor` y-bias,
  scale `splat(factor)`, `LodChunk { factor }`, `LodVoxelData`, same-batch
  `LodNeighbors`. The mesher, skirts, Aabb bias, and LodChunk isolation
  (no `ChunkKey`, no physics, outside the cave-culling BFS) are untouched.
- **Refresh pushes** are treated as a factor transition: new root spawns,
  old root retires until the replacement meshes, so a distant edit never
  pops the horizon. `LodData` for a key with no slot (released mid-flight,
  reconnect race) is dropped.
- **Deleted:** `WorldgenTask`, `build_worldgen_lod_cells`, client-side
  `downsample_cells`/`block_scale_cells` copies, and the compiled-in-seed
  hazard (the client no longer generates horizon terrain, resolving the
  foreign-seed follow-up noted in `WorldgenRings::seed`). `worldgen`
  remains a client dependency only for the offline embedded server, which
  runs the same `WorldManager` path inline — offline/wasm gets streamed
  LOD for free.

## 5. Edge cases, failure modes, testing

### Edge cases

- **Missing response** (lost request, overloaded server): the retirement
  deadline already bounds it — old terrain lingers, then pops at the
  deadline. Reconnect re-requests every assigned key.
- **Race: release then data arrives:** client drops data for unassigned
  keys; server tolerates releases for unknown keys.
- **Tile/near overlap:** 64/256 rings start ≥ 9 regions out, the near
  window is 3×3 — overlap only transiently after teleports, reconciled by
  the next assignment pass (overdraw until then, the rings' accepted
  trade).
- **Bandwidth:** initial burst ≈ 530 keys × ~1 KB RLE grids; replaces the
  far ring's full-chunk streams — a net reduction. Same reliable stream
  and ordering as today's far snapshots.
- **Server cache growth:** LRU cap (named constant); subscribed keys
  pinned.

### Testing

- `game`: moved downsample tests, `PackedLodGrid` round-trip (including
  worst-case incompressible grids), `LodKey` math (region↔tile mapping,
  world offsets, signed coords).
- `worldgen`: `coarse_tile` determinism (same key/seed twice, bit-equal);
  tolerance test — coarse cells vs downsampled full-gen on a sample tile,
  divergence bounded (surface height within one cell for ≥ some fraction
  of columns; threshold pinned during implementation).
- `game::world_manager` (threadless, `InlineSpawner`): request→data,
  cache hit (no rebuild), parked source, pristine sources (both paths),
  tile overlay of an edited region, invalidation push to subscribers only,
  release/disconnect cleanup, LRU pinning.
- `client` (headless): rings lifecycle against a stubbed packet channel
  (request on assign, release on evict, pending until arrival), spawner
  contract on arrival, refresh-retirement (old root survives until new
  meshes), drop of unassigned-key data. The existing client/server
  convergence harness extends to cover the factor-2 fold-in end-to-end.
- Live smoke: server + one client, fly toward the horizon; verify ring
  transitions, an edit propagating to a factor-4 ring, and wasm/offline.

### Documentation/config to keep in sync

- `CLAUDE.md` far-ring/worldgen-ring descriptions.
- `TODO.md` if it references the far ring.
- Justfile: no new commands expected.

## Out of scope / follow-ups

- Persisting the server LOD cache to disk across restarts.
- Cross-key `LodNeighbors` wiring (border side-faces of adjacent keys
  render unculled walls — existing accepted overdraw).
- Streaming LOD for entities (terrain only, matching today).
- Visual pass items from the rendering program (horizon fog, ring seams,
  shadows) — unchanged by this design.
