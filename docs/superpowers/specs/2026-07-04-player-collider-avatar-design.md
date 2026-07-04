# Entity Colliders + Avatars (Player First)

**Date:** 2026-07-04
**Status:** Approved design, pending implementation

## Problem

The player is a flying camera: a kinematic rigid body with no collider and no
renderable mesh. Other players are invisible — after the multi-client work, a
second player exists in your sim but there is nothing to draw.

## Direction constraint

The game is Minecraft-like. The player is an *entity like any other* — NPCs
and other entities will also carry colliders and meshes. Nothing in this
design may be player-only where an entity-generic mechanism is possible; the
player is merely the first entity kind wired through it.

## Goals

- Entities can carry a collider; the player entity gets a capsule.
- Entities can declare an appearance (`EntityKind`) that the client maps to a
  mesh; the player is the first kind. Other players render as capsules.
- Movement is unchanged (fly-cam, kinematic, gravity off).

## Non-goals

- Gravity, walking, or a character controller.
- Terrain/chunk colliders (separate project).
- Collision response between kinematic bodies.
- Animated or modeled avatars; NPC spawning itself.

## Design

### 1. Sim: generic `EntityKind` component (`crates/game/src/state.rs`)

New serialized, hashed component in the rollback state:

```rust
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, Hash)]
pub enum EntityKind {
    Player,
    // future: NPC kinds
}
```

- `Ecs` gains `kind: Component<EntityKind>` (tier-2 `Component<T>` like
  `camera`/`chunk`); `create_entity_safe` inserts the empty slot alongside
  the other component maps.
- New update variant `GameDataUpdateKind::SetEntityKind(EntityKey,
  Option<EntityKind>)`, emitted Do on set and compensated on undo via
  `emit_on_undo` (same pattern as the camera component).
- Because `EntityKind` lives in `GameData`, region snapshots carry it
  automatically — joining clients see existing entities' kinds without any
  extra protocol.

### 2. Sim: generic collider helper + player capsule

New helper on `Rollback` (entity-generic; NPCs reuse it):

```rust
pub fn attach_capsule_collider_safe(&mut self, e: EntityKey, body: RigidBodyHandle,
                                    half_height: f32, radius: f32)
```

- Builds `ColliderBuilder::capsule_y(half_height, radius)` with `user_data` =
  entity key (same ffi convention as bodies) and inserts via
  `insert_with_parent(collider, body, bodies)` under a
  `self.data.physics.snapshot_raw()` scope.
- **Undo rationale:** the vendored rapier fork has exact allocator inverses
  only for `RigidBodySet`; `ColliderSet` has none, and `insert_with_parent`
  also mutates the parent body (mass properties, attached-collider list).
  The whole-PhysicsState snapshot is the sanctioned tier-2 fallback,
  hash-verified at rollback, and already used by `PhysicsController`.
  Cost: one PhysicsState clone per entity creation — acceptable at spawn
  frequency.
- `create_player_safe` calls the helper with `(0.5, 0.4)` (≈1.8 m tall) and
  sets `kind` to `Some(EntityKind::Player)` (with the `SetEntityKind` emit).
- **Determinism:** all of this executes inside the `CreateClient` event on
  server and clients alike, so arena state stays identical across machines.

### 3. Renderer: kind → mesh mapping (`crates/client/src/renderer/`)

**Bridge (`bridge.rs`):**
- Snapshot walk: entities with a `kind` get a `SimKind(EntityKind)` Bevy
  component.
- Drain: `SetEntityKind(key, Some(kind))` inserts `SimKind(kind)`;
  `SetEntityKind(key, None)` removes it.

**New `avatar.rs` system**, registered with the renderer plugin: for every
`Added<SimKind>` entity **without `Camera3d`**, look up the kind's mesh —
`EntityKind::Player` → `Mesh3d(Capsule3d::new(0.4, 1.0))` + a shared flat
`StandardMaterial` (handles cached in a lazy resource). The `Without<Camera3d>`
filter is the local-player exclusion: only the local player's entity ever has
an active `Camera3d` (previous change), and rendering your own capsule from
inside would occlude the first-person view. NPC kinds later extend the same
match arm.

### 4. Testing

- **Sim:** existing `multi_client.rs` foreign-`CreateClient` test rolls back
  and re-applies player creation, exercising the snapshot undo of the
  collider insert under the bit-exact hash checks. Add assertions: after
  `create_player_safe`, the collider set has exactly one collider parented to
  the player body, and the entity's `kind` is `Some(EntityKind::Player)`.
- **Client (headless):** bridge tests assert `SimKind` insertion for both the
  snapshot path and the drain path (and removal on `SetEntityKind(_, None)`);
  an avatar-system test asserts `Mesh3d` attached for a kind-bearing entity
  without `Camera3d` and absent for one with `Camera3d`.
- **Live:** run server + two clients; the second player appears as a capsule
  in the first window and moves as they fly.

## Files

- `crates/game/src/state.rs` — `EntityKind`, `Ecs.kind`, `SetEntityKind`
  update, collider helper, `create_player_safe` wiring.
- `crates/client/src/renderer/bridge.rs` — `SimKind` component, both paths.
- `crates/client/src/renderer/avatar.rs` — new; kind→mesh system.
- `crates/client/src/renderer/mod.rs` — register system/resources.
- `crates/game/tests/multi_client.rs` — collider + kind assertions.
