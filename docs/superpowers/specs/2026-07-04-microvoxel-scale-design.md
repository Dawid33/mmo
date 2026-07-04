# Microvoxel Scale: 1 Unit = 1/16 m + Multi-Chunk Floor

**Date:** 2026-07-04
**Status:** Approved design, pending implementation

## Problem

Voxels are currently roughly player-sized (Minecraft proportions: the 1.8 m
player is ~2 voxels tall). The game's direction is a world *made of tiny
voxels* — sculptable everywhere at fine granularity, not a two-tier
block/detail split.

## Research context (Vintage Story / Minecraft / Teardown)

- Minecraft: 1 m blocks; player ~2 blocks tall.
- Vintage Story: 1 m world blocks; the chisel subdivides a single block into
  a 16×16×16 microvoxel grid (1/16 m = 6.25 cm; player ≈ 29–30 microvoxels).
  VS keeps its *world grid* at 1 m — microvoxels are per-block detail only.
- Teardown ("world of tiny voxels" reference): 10 cm world grid.

This project goes further than VS: the world grid itself is microvoxel-scale.
Chosen size: **1/16 m (6.25 cm)** — VS chisel granularity as the entire
world. The cubic data cost (4096× Minecraft density per m³) was reviewed and
accepted; a 1/8 m fallback ratio remains a constants-level change if
snapshot/hash pressure demands it later.

## Approach: redefine the unit

**1 sim/render unit = 1 voxel = 1/16 m.** Voxels stay 1×1×1 in unit space,
so greedy meshing, the rapier `Voxels` collider (`voxel_size` 1.0), chunk
offsets (`coords * 32`), and all integer voxel math are untouched. Only
entity-scale constants change. (Rejected: keeping meters and setting
`voxel_size = 1/16`, which scatters fractional scale factors through the
collider builder, mesher, offsets, and every future conversion; and a
VS-style two-tier representation, which contradicts the direction.)

## Design

### 1. Scale constants (`crates/game`)

- Player capsule: `attach_capsule_collider_safe(e, handle, 8.0, 6.4)` —
  total 28.8 units = 1.8 m ≈ 29 voxels tall (VS proportions).
- `CameraController::SPEED`: 5.0 → 80.0 (8 units/tick = 0.5 m per tick =
  10 m/s fly speed at TICK_RATE 50 ms; feel-tunable).
- Camera projection (`Camera::new` and `Default`): near 0.1 → 1.0 unit
  (6 cm), far 100 → 2000 units (125 m).
- Spawn: `(128, 26, 128)` — center of the new floor (floor top y=8, capsule
  half-extent 14.4, ~3.6 units clearance).

### 2. Multi-chunk floor (`crates/game`)

- `Chunk::default()`'s bordered single-chunk slab is replaced by
  `Chunk::flat_floor(depth: u32)`: solid voxels across the **full 32×32
  footprint** for `y < depth`, air above. Full-footprint fill makes chunk
  seams tile with no gaps. `create_mesh` gains the chunk value as a
  parameter (or a variant that takes one) so callers choose content.
- `Region::create_basic` builds an **8×8 grid**: chunk entities at
  `ChunkCoords::new(x, 0, z)` for x,z ∈ 0..8 with `Chunk::flat_floor(8)` —
  a 16 m × 16 m floor, 0.5 m thick, in the single existing region. Bodies
  at `coords * 32` as today. Bridge, meshing, and snapshots already handle
  N chunk entities; this exercises N > 1 for the first time.

### 3. Renderer constants (`crates/client`)

- Avatar mesh: `Capsule3d::new(6.4, 16.0)` (28.8 units total, matches the
  sim capsule).
- `SimTarget` snap thresholds are world-unit quantities, scale ×16:
  body `pos_snap` 0.1 → 1.6; camera `pos_snap` 0.0005 → 0.008 (rotation
  snaps and smoothing factors are dimensionless, unchanged). Any other
  unit-bearing constants discovered in `interpolate.rs`/`bridge.rs` during
  implementation get the same ×16 treatment.

### 4. Determinism & rollback

No new mechanisms: all changes are constants and world-gen content executed
identically on server and clients. The existing hash-convergence suite is
the regression gate.

## Watch-item (accepted risk)

64 chunks ≈ 2.1M voxels: region snapshots grow to a few MB and full-state
hashes get proportionally heavier. Fine on localhost today. If join transfer
or reconcile hashing becomes noticeable, first lever is shrinking the test
floor to 4×4 chunks; second is revisiting the ratio (1/8 m) — both are
content/constants changes, not architecture.

## Out of scope

- Gravity/walking, voxel editing, chunk streaming/multi-region, worldgen
  crate content, texture/UV changes (voxel textures tile per-voxel as
  before, now just physically smaller).

## Testing

- Updated sim tests: collider count = 64 (one `Voxels` per chunk); floor-top
  assertions at y=8; descend test window moves to roughly (24.5, 26.5)
  — start y=26, blocked rest ≈ 8 + 14.4 + controller offset ≈ 22.4+;
  recompute exact bounds during implementation against the chosen spawn.
- Hash-convergence multi-client suite stays green throughout.
- Client suite: avatar/bridge tests unchanged in structure; constants
  updated where they assert dimensions.
- Live: two clients on the 16 m floor — world reads as tiny voxels (player
  ~29 voxels tall), movement and collision feel unchanged apart from scale,
  other player's capsule correct size.

## Files

- `crates/game/src/state.rs` — capsule dims, spawn.
- `crates/game/src/camera.rs` — SPEED, projection near/far.
- `crates/game/src/voxel.rs` — `Chunk::flat_floor`, default removal.
- `crates/game/src/lib.rs` — `World::basic` 8×8 grid.
- `crates/game/src/region.rs` — `create_basic` signature if needed.
- `crates/client/src/renderer/avatar.rs` — capsule dims.
- `crates/client/src/renderer/bridge.rs` — SimTarget snap constants.
- `crates/game/tests/multi_client.rs` — updated assertions.
