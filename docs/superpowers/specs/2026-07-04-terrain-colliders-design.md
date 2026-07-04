# Voxel Terrain Colliders + Solid Player Movement

**Date:** 2026-07-04
**Status:** Approved design, pending implementation

## Problem

Terrain chunks render but have no physical form: the chunk entity has a fixed
rigid body and voxel data, but no collider (`Chunk.collider` has always been
empty). Nothing in the world is solid — and because the player body is
kinematic with direct `set_next_kinematic_translation` writes, even with
terrain colliders present the player would fly straight through them.

## Goals

- Every chunk with solid voxels carries a rapier `Voxels` collider matching
  its voxel grid.
- Player movement is collision-corrected: flying into terrain stops/slides
  instead of passing through.
- Entity-generic mechanisms (per project direction): the collider helper and
  movement correction are not player-special beyond selecting the mover.

## Non-goals

- Gravity, jumping, walking, grounded behavior (`grounded` is computed by
  the controller but unused for now).
- Voxel editing and incremental collider updates (the chosen shape supports
  them via parry's `voxels_edition` API when that feature arrives).
- NPC movement; player-vs-player collision response.

## Design

### 1. Terrain collider (`crates/game/src/state.rs`)

New helper beside `attach_capsule_collider_safe`, same undo pattern
(whole-PhysicsState `snapshot_raw()` — no exact `ColliderSet` inverse in the
vendored fork, and `insert_with_parent` mutates the parent body):

```rust
pub fn attach_voxels_collider_safe(&mut self, e: EntityKey, body: RigidBodyHandle,
                                   coords: &[Point<i32>])
```

- Builds `ColliderBuilder::voxels(Vector::repeat(Real::from(1.0)), coords)`
  with `user_data` = entity key; parents it to the chunk's fixed body.
- `create_mesh` collects grid coordinates of non-Air voxels in
  `ChunkShape::delinearize` order (deterministic) and calls the helper;
  chunks with zero solid voxels get no collider.
- One collider per chunk. Distribution: the server builds colliders at world
  creation; clients receive them inside the region snapshot — the vendored
  parry `Voxels` shape is serde-serializable, so no protocol change.

### 2. Collision-corrected movement (`crates/game/src/camera.rs`)

In `CameraController::on_tick`, replace the direct translation write with
rapier's kinematic character controller:

- Build `KinematicCharacterController::default()` per tick (plain config
  value — pure function of state, deterministic libm/`Real` math like the
  rest of the sim).
- Query view: `broad_phase.as_query_pipeline(...)` over bodies/colliders
  with `QueryFilter::default().exclude_rigid_body(player_body_handle)` so
  the mover ignores its own capsule.
- Character shape/pos: the player's capsule collider (attached in the
  previous feature) and the body's current position.
- `move_shape(dt, &queries, shape, pos, desired_translation, |_| {})`
  returns the corrected translation; apply it via the existing
  `change()`-snapshot write (`set_next_kinematic_translation(t + corrected)`).
- All controller reads happen before the `change()` write borrow; mouse
  rotation handling is unchanged. `dt` = 1.0 tick (movement values are
  already per-tick).
- Fallback: if the player body has no collider (never true after the avatar
  feature, but cheap to guard), fall back to the uncorrected translation.

### 3. Spawn height

`create_player_safe` spawn moves from `(0, 1, 5)` to `(0, 3, 5)`: the
default chunk's floor slab occupies world y ∈ [1, 2), and the capsule
(half-height 0.5 + radius 0.4) centered at y=1 starts embedded in it.

## Determinism & rollback

- Collider construction and movement correction run inside the normal tick/
  event path with fork-pinned `Real` math — identical on server and clients.
- `move_shape` only *reads* the physics world; the sole write remains the
  existing snapshot-covered `set_next_kinematic_translation`, so no new undo
  surface. Collider insertion undo = PhysicsState snapshot, hash-verified.

## Testing

- **Sim (`crates/game/tests/multi_client.rs` or new `terrain.rs`):**
  1. `World::basic()`: chunk body carries exactly one collider; colliders
     set is non-empty.
  2. Movement blocked: player descends (hold `ControlLeft`) for ~60 ticks;
     final y stays above the floor top (≥ 2.0 + capsule half-extent − small
     controller offset; assert `y > 2.5` for slack) instead of tunneling to
     y ≪ 0.
  3. Existing multi-client hash-convergence suite stays green (corrected
     movement is part of the deterministic tick).
- **Client:** no changes; suite stays green.
- **Live:** two clients — flying down into the floor stops on it; flying
  sideways along the floor slides.

## Files

- `crates/game/src/state.rs` — `attach_voxels_collider_safe`, `create_mesh`
  wiring, spawn height.
- `crates/game/src/camera.rs` — character-controller movement.
- `crates/game/tests/` — new assertions/tests above.
