# Player Capsule Collider + Visible Avatar

**Date:** 2026-07-04
**Status:** Approved design, pending implementation

## Problem

The player is a flying camera: a kinematic rigid body with no collider and no
renderable mesh. It has no physical presence in the rapier world (nothing to
collide with, and nothing can collide with it), and other players are
invisible — after the multi-client work, a second player exists in your sim
but there is nothing to draw.

## Goals

- The player body carries a capsule collider: physical presence in the
  physics world and groundwork for future terrain collision.
- Other players are visible as a simple capsule avatar in the client.
- Movement is unchanged (fly-cam, kinematic, gravity off).

## Non-goals

- Gravity, walking, or a character controller.
- Terrain/chunk colliders (none exist today; separate project).
- Player-vs-player collision response (kinematic bodies don't push each
  other; the collider is presence only).
- Animated or modeled avatars.

## Design

### 1. Sim: capsule collider on the player body (`crates/game/src/state.rs`)

In `Rollback::create_player_safe`, after the rigid-body insert and before the
camera component:

- Build `ColliderBuilder::capsule_y(0.5, 0.4)` (≈1.8 m tall, 0.4 m radius),
  `user_data` = entity key as ffi u128 (same convention as bodies).
- Insert via `insert_with_parent(collider, body_handle, bodies)` under a
  `self.data.physics.snapshot_raw()` scope.

**Undo rationale:** the vendored rapier fork exposes exact allocator
inverses (`alloc_state`/`revert_insert`) only for `RigidBodySet`;
`ColliderSet` has none, and `insert_with_parent` additionally mutates the
parent body (mass properties, attached-collider list). The whole-PhysicsState
snapshot is the sanctioned tier-2 fallback, hash-verified at rollback, and
already the pattern `PhysicsController::on_tick` uses. Cost is one
PhysicsState clone per player creation — rare.

**Determinism:** the collider insert executes inside the `CreateClient`
event on server and every client alike, so arena state stays identical
across machines.

### 2. Renderer: avatar marker + mesh (`crates/client/src/renderer/`)

**Bridge (`bridge.rs`):** wherever a player entity is recognized, insert a
marker component instead of a mesh:

```rust
#[derive(Component)]
pub struct PlayerAvatar {
    pub local: bool,
}
```

- Snapshot walk: entities present in `data.player_entites` get
  `PlayerAvatar { local: entity == local player's entity }` (ownership
  resolved the same way as the camera gating).
- Incremental path: the `AddCameraComponent(key, client_id, ..)` arm inserts
  `PlayerAvatar { local: local_player.0 == Some(client_id) }`.

**New `avatar.rs` system**, registered alongside the meshing systems: for
every `Added<PlayerAvatar>` with `local == false`, attach
`Mesh3d(Capsule3d::new(0.4, 1.0))` and a flat `StandardMaterial`
(single shared handle cached in a resource, created lazily). Local players
get no mesh — the first-person camera sits inside the capsule and a mesh
would occlude the view. A later free-cam self-view can key off the same
marker.

### 3. Testing

- **Sim:** the existing `multi_client.rs` foreign-`CreateClient` test rolls
  back and re-applies player creation, so the snapshot undo of the collider
  insert is exercised automatically and hash-checked by the rollback
  machinery. Add an assertion that after `create_player_safe` the collider
  set contains exactly one collider parented to the player body.
- **Client (headless):** bridge tests assert `PlayerAvatar` presence and the
  `local` flag for both the snapshot path and the incremental path; an
  avatar-system test asserts `Mesh3d` attached for a remote avatar and
  absent for a local one.
- **Live:** run server + two clients; the second player appears as a capsule
  in the first window and moves as they fly.

## Files

- `crates/game/src/state.rs` — collider insert in `create_player_safe`.
- `crates/client/src/renderer/bridge.rs` — `PlayerAvatar` marker, both paths.
- `crates/client/src/renderer/avatar.rs` — new; mesh attachment system.
- `crates/client/src/renderer/mod.rs` — register system/resource.
- `crates/game/tests/multi_client.rs` — collider assertion.
