# Far-Region Visual Chunks — Design

**Date:** 2026-07-07
**Status:** Approved (brainstormed with user; decisions below are theirs)
**Builds on:** `2026-07-05-multi-region-world-design.md` (regions as sim instances,
hub-and-spoke manager, region-local coords, park/restore lifecycle),
`2026-07-05-entity-region-handoff-design.md` (the "ghosts never wake a region" rule),
`2026-07-06-vertical-chunk-stacking-design.md` (columns / sub-chunks).

## Goal

Render terrain for regions **outside** the client's 3×3 simulated window, so the world
extends visibly toward the horizon instead of ending at the edge of the active sim area.
These far regions are **read-only, visual-only**: the server serves a stripped-down
terrain-only subset of a region's data; the client meshes it with the normal chunk pipeline
and places it in world space like any region, but it is never simulated (no physics, no
rollback, no reconcile). Far regions update **infrequently** — only when their terrain
actually changes — and viewing them must never wake or keep alive a sim thread.

This adds a **third region tier** on top of today's binary model (a client is either a full
subscriber of a running region or holds nothing): *near* (full sim, unchanged), *far*
(visual-only terrain), or *neither*.

## Decisions (locked with user)

1. **Server-authoritative snapshots.** The server ships the far region's actual current
   terrain (a stripped chunk-only payload), so terrain edits are reflected. Reuses the
   existing serialization machinery and is the future network seam.
   - Rejected: **client-side worldgen** (client already links `worldgen` and could regenerate
     base terrain for free, but it cannot show edits — a far region a player dug into would
     render pristine). Rejected: **hybrid** (worldgen base + server-shipped edit delta —
     cheapest steady state but requires a per-region edit-delta concept that does not exist
     yet). Both remain valid future bandwidth optimizations once digging/persistence make
     them worthwhile.
2. **Push-on-change, throttled refresh.** After the initial snapshot the server re-sends a
   far snapshot only when a *running* region's terrain mutates, rate-limited to at most once
   per `FAR_SNAPSHOT_MIN_INTERVAL_TICKS`. Parked/never-run regions cost exactly one snapshot
   (their terrain is frozen). This is the only option that is both correct (shows edits) and
   genuinely infrequent (silent when nothing changes).
   - Rejected: slow-timer push, pull-on-timer (both re-send redundant snapshots for terrain
     that never changed — almost all far terrain); once-only (never shows distant edits).
3. **Terrain only.** The far payload is chunk voxels — no entities, players, ghosts, bodies,
   cameras, or undo log. Entities/players at a distance are a separate future feature.
4. **Full-resolution meshing, no LOD.** Far chunks reuse the exact `build_chunk_mesh` path
   and are "loaded as a normal region" visually. LOD/downsampling is the obvious future lever
   for extending range cheaply but is explicitly out of scope here.
5. **Configurable range.** `FAR_VIEW_RADIUS` (Chebyshev radius, default `2` → a 5×5 footprint,
   one ring around the near 3×3 window) is a named constant in `game`. No UI to change it;
   the point is that it can be raised later (especially once LOD lands) by editing one
   constant.
6. **Far-viewing never costs a sim thread.** Serving a far region must not wake a parked
   region, and holding a far view must not keep a region alive. This mirrors the handoff
   spec's "ghost updates never wake a region" rule and is what makes far-viewing cheap enough
   to apply to a wide ring.

## Data types (in `game`)

```rust
/// The visual-only subset of a region's data: terrain chunks, nothing else.
/// Much smaller than a `Rollback` — a `Chunk` is 8 blocks + a sparse chisel map,
/// and columns are sentinel-bounded so only the surface band materializes.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct FarSnapshot {
    pub chunks: Vec<(ChunkCoords, Chunk)>,
}

/// Chebyshev radius of the visual-only far ring around the viewer region.
/// Default 2 → 5×5 footprint (one ring outside the near 3×3 window).
pub const FAR_VIEW_RADIUS: i32 = 2;

/// A running region emits at most one far-snapshot refresh per this many ticks,
/// coalescing bursts of terrain edits into an infrequent push.
pub const FAR_SNAPSHOT_MIN_INTERVAL_TICKS: usize = 25;
```

`FarSnapshot` is built by a new `Region::far_snapshot(&self) -> FarSnapshot`: iterate entities
carrying a `chunk` component; recover each chunk's `ChunkCoords` from its fixed rigidbody
translation (`translation / 32`, exact — bodies are placed at `(cx*32, cy*32, cz*32)` in
`create_mesh`, `state.rs:289-300`); collect `(coords, chunk.clone())` for non-air chunks.
The manager can also build a `FarSnapshot` directly from worldgen output
(`generate_region` → `Column::materialized_sub_chunks()` → `(ChunkCoords, Chunk)`), which is
how never-run regions are served.

## Wire protocol (`protocol.rs`)

```rust
enum ClientPacket {
    // … existing …
    RequestFarRegion(RegionId),
    ReleaseFarRegion(RegionId),
}

enum ServerPacket {
    // … existing …
    /// Initial send AND refresh (replace-on-re-receipt). Removal is client-driven,
    /// so there is no "far region gone" packet.
    FarRegion(RegionId, FarSnapshot),
}
```

Far and near are independent request/release streams. The client is responsible for keeping
its near and far sets disjoint; the server tolerates overlap/duplicates (drop-and-log), as it
already does for unknown-region noise.

## Region actor (`region_runner.rs`)

```rust
enum RegionInput {
    // … existing …
    RequestFarSnapshot(ClientId),   // → FarSnapshot(client, …), targeted initial reply
}

enum RegionOutput {
    // … existing …
    FarSnapshot(ClientId, FarSnapshot),   // reply to RequestFarSnapshot
    FarSnapshotChanged(FarSnapshot),      // dirty-push; manager fans to far-subscribers
}
```

- The runner tracks a coarse **terrain-dirty** flag, set whenever a tick produces any
  `SetVoxelComponent` `GameDataUpdate` (the existing signal for voxel mutation). In `tick()`,
  if dirty **and** at least `FAR_SNAPSHOT_MIN_INTERVAL_TICKS` have elapsed since the last far
  push, it emits `FarSnapshotChanged(self.far_snapshot())` and clears the flag + resets the
  throttle. Regions still never see subscriber lists — the manager owns fan-out, and a region
  with no far-viewers simply has its `FarSnapshotChanged` dropped by the manager.
- `RegionOutput::Stopped` gains a `FarSnapshot` alongside the `SerializedRegion` so the
  manager can cache the parked region's terrain (see below). (Equivalent alternative: the
  manager derives it from the `Stopped` blob; carrying it avoids a manager-thread deserialize.)

## Manager (`world_manager.rs`) — the load-bearing part

New state:

```rust
/// Far-viewers per region, INDEPENDENT of running/parked state — a region can
/// have far-viewers while parked.
far_subscribers: BTreeMap<RegionCoords, BTreeSet<ClientId>>,
/// Frozen terrain for parked / never-run regions, so they are served without
/// waking. Kept in sync: refreshed on FarSnapshotChanged and on park (Stopped).
far_cache: BTreeMap<RegionCoords, FarSnapshot>,
```

`Session` gains `far_subscribed: BTreeSet<RegionCoords>` (mirror of the near `subscribed`).

**Serving `RequestFarRegion(rc)`:**
- Record in `session.far_subscribed` and `far_subscribers[rc]`.
- If the region is **running**: send `RegionInput::RequestFarSnapshot(client)`; relay the
  `FarSnapshot` reply to that client as `ServerPacket::FarRegion`.
- If the region is **parked or never-run**: serve `far_cache[rc]` directly (on miss, generate
  via the injected `worldgen` closure → materialize → cache), send `ServerPacket::FarRegion`.
  **Do not call `ensure_running`** — far requests never wake a region.

**`ReleaseFarRegion(rc)`:** remove from both sets. If the region is running and this leaves it
with no near subscribers and it is no-one's home, the normal grace timer applies (far-viewers
are not a keep-alive reason — see below).

**Refresh (`RegionOutput::FarSnapshotChanged(snap)` from region `rc`):** update
`far_cache[rc] = snap` and fan `ServerPacket::FarRegion(rc, snap)` out to `far_subscribers[rc]`
(drop if empty).

**Keep-alive:** `refresh_keepalive` **ignores** `far_subscribers` entirely. A region with only
far-viewers parks normally after `UNLOAD_GRACE_MS`; its far-viewers keep being served from
`far_cache`. This is the core cheapness guarantee.

**Park path:** on `RegionOutput::Stopped { serialized, far }`, store `far_cache[rc] = far` in
addition to `parked[rc] = serialized`. A parked region's terrain never changes, so the cache
stays consistent with the blob until the region is re-run and re-parked.

**Running↔parked transitions with live far-viewers:** because `far_subscribers` is keyed by
region and independent of the `RegionLink`, far-viewers survive a park/unpark. When a region
with far-viewers unparks (someone moves in and near-subscribes it), its subsequent
`FarSnapshotChanged` pushes reach those far-viewers automatically; when it re-parks, the cache
is refreshed from `Stopped`. No far-viewer bookkeeping is lost across the cycle.

## Client (`instance.rs` + render bridge)

- **Policy:** alongside `desired_window` (near 3×3 + home), compute
  `desired_far` = every region within Chebyshev `FAR_VIEW_RADIUS` of the viewer region,
  **minus** the near set (near and far are always disjoint). Diff against a new
  `far_subscribed: BTreeSet<RegionCoords>` → emit `RequestFarRegion` / `ReleaseFarRegion`.
  Same edge-trigger discipline as the near window (only fire when the derived set changes).
- **Far regions bypass the sim entirely** — no local `game::Region`, no rollback, no
  reconcile, no `pending_events`. On `ServerPacket::FarRegion(rc, snap)`: if `rc` is no longer
  in `desired_far`, release immediately (race tolerance, matching the near path); otherwise
  emit `ClientUpdateEvent::NewFarRegion(rc, snap)` (a refresh for an already-loaded far region
  is the same event — replace-on-re-receipt).
- **New `ClientUpdateEvent` variants:** `NewFarRegion(RegionId, FarSnapshot)` and
  `RemoveFarRegion(RegionId)`.

**Render bridge:**
- A new resource `FarRegionRoots: BTreeMap<RegionId, Entity>`, distinct from `RegionRoots`, so
  a region's near root and far root can never collide.
- `NewFarRegion(rc, snap)`: spawn (or, on refresh, despawn-and-respawn) a region root
  `Transform` at `rc.world_offset()`; for each `(coords, chunk)` spawn a child at local
  `(coords.x*32, coords.y*32, coords.z*32)` with `VoxelData(derive_voxels(&chunk.blocks,
  &chunk.chisel))`. The existing `queue_meshing`/`apply_meshed_chunks` systems mesh it with no
  changes. **Plain `Transform`, no rigidbody/collider** — visual only.
- `RemoveFarRegion(rc)`: despawn the far root recursively, drop the `FarRegionRoots` entry.
  In-flight mesh tasks for a removed far region are tolerated (meshing already guards on
  `VoxelData` liveness).

## Near↔far transitions

A region is near, far, or neither — never two at once. The client keeps `desired_near` and
`desired_far` disjoint by construction (far = ring minus near). On viewer movement:
- **far → near:** send `ReleaseFarRegion(rc)` + `RequestRegionConnection(rc)`. Bridge despawns
  the far root; the arriving `NewRegion` spawns the sim root.
- **near → far:** the reverse — `ReleaseRegionConnection(rc)` + `RequestFarRegion(rc)`.

The bridge enforces mutual exclusion: spawning a region's near (`NewRegion`) root despawns any
existing far root for that `RegionId`, and spawning a far (`NewFarRegion`) root despawns any
existing near root — so a promotion/demotion never leaves both rendered. A ≤1-frame overlap or
gap during the swap is acceptable (same tolerance as today's region cycling).

## wasm / local server

No special-casing. `LocalServer` wraps the same `WorldManager` core, so `far_subscribers`,
`far_cache`, worldgen generation, and far-serving all run inline in the threadless core. The
browser build gets far regions for free.

## Error handling

- **Far request for an unknown/absent region:** generate from worldgen and cache; never fails
  (worldgen is total).
- **Corrupt parked blob when the region later unparks:** unchanged from today (regenerate
  deterministically); `far_cache` is independent and already holds a valid snapshot.
- **Far snapshot arriving after the client moved on:** client releases immediately (already in
  the receive path). Server tolerates `ReleaseFarRegion` for an unknown region (drop + log).
- **Region thread death:** far-viewers are in the manager's `far_subscribers`, not the dead
  `RegionLink`, so they are unaffected; they keep being served from `far_cache` until the
  region respawns.
- **Client double-holds a region (near + far) transiently during a swap:** manager tolerates;
  the client's disjoint-set construction prevents it in steady state.

## Testing

1. **Manager core (headless, in `game`):**
   - Far request for a **running** region delivers a `FarSnapshot` matching its terrain.
   - Far request for a **parked** region is served from `far_cache` **without spawning a
     thread** (assert the spawner was not invoked / region stays parked).
   - Far request for a **never-run** region generates from worldgen, caches, and serves.
   - Far-subscribers are **not** a keep-alive reason: a region whose only interest is
     far-viewers parks after the grace period.
   - `FarSnapshotChanged` fans out only to `far_subscribers` and is throttled to at most once
     per `FAR_SNAPSHOT_MIN_INTERVAL_TICKS`.
   - Payload is terrain-only (no entity/camera data leaks into `FarSnapshot`).
   - Far-viewer survives a park→unpark→repark cycle and still receives pushes.
2. **worldgen:** `generate_region` → `FarSnapshot` materialization is deterministic
   (same coords → hash-equal `FarSnapshot`).
3. **Client headless (extend the existing suite):**
   - Far-ring diff: viewer world position → expected `RequestFarRegion`/`ReleaseFarRegion`
     stream; near and far desired sets stay disjoint.
   - `NewFarRegion` roots at `world_offset()` with meshable chunk children at the right local
     offsets; the chunks actually produce triangles.
   - `RemoveFarRegion` despawns the far root and clears `FarRegionRoots`.
   - far→near promotion swaps roots without leaving both (or leaking a root); near→far demotion
     likewise.
   - Refresh (`NewFarRegion` for a loaded far region) replaces its chunks.
4. **Rollback bar untouched:** the far path never enters undo/sim, so existing
   `hash(before) == hash(after undo)` suites pass unchanged.
5. **Manual acceptance:** server + one client, roam so a region leaves the 3×3 window — it
   stays visible as a far region out to the ring edge instead of popping out; edit terrain in
   a running region with a far-viewer present and confirm the far view refreshes (throttled).
   wasm build/run check.

## Out of scope

LOD / downsampling (the lever for extending range cheaply); entities, players, or mobs at a
distance; per-region or per-client far radius; durable persistence of `far_cache` (in-memory,
like parking); the client-side-worldgen and hybrid bandwidth optimizations from Decision 1.

## Risks / notes

- **Full-res meshing cost at range.** At `FAR_VIEW_RADIUS = 2` the far ring is 16 regions
  (5×5 minus the 3×3 near window), up to ~64 chunk meshes each, though sparse columns mean
  only surface sub-chunks produce geometry. This is the reason range is a single tunable
  constant and LOD is the named future lever — do not raise `FAR_VIEW_RADIUS` far without it.
- **Dirty-flag coarseness.** Keying the terrain-dirty flag off `SetVoxelComponent` deltas is
  conservative and correct (it fires exactly when rendered voxels change). With no digging
  gameplay yet it will rarely fire, so far regions are effectively send-once in practice today
  — which is fine and matches the "infrequent" goal.
- **`far_cache` memory.** One `FarSnapshot` per far-viewed and parked region. Bounded by the
  number of regions a session roams near; acceptable pre-persistence, same posture as the
  in-memory parking lot.
- **Two coords sources for a chunk.** `far_snapshot` recovers `ChunkCoords` from the fixed
  body translation rather than a stored coords component. This is exact (translations are
  `cx*32` integers) but couples extraction to the body-placement convention in `create_mesh`;
  if a `ChunkCoords` component is ever added, prefer it.
