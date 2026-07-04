# Entity Colliders + Avatars Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Entities can carry colliders and a declared appearance (`EntityKind`); the player is the first kind — capsule collider in the sim, capsule avatar rendered for non-local players.

**Architecture:** A new `EntityKind` component in the rollback state (snapshot-carried, hash-checked) plus a `SetEntityKind` render update; a generic `attach_capsule_collider_safe` helper on `Rollback` using a whole-PhysicsState snapshot for undo (the vendored rapier fork has no exact inverse for `ColliderSet`, and `insert_with_parent` mutates the parent body); client-side, the bridge mirrors kinds into a `SimKind` Bevy component and a new `avatar.rs` system maps kind → mesh, excluding the local player via `Without<Camera3d>`.

**Tech Stack:** Rust workspace; `game` stays Bevy-free. Build: `~/Software/rustc_codegen_cranelift/dist/cargo-clif build --workspace --bins` (plain `cargo` fine for check/test). Tests: `cargo test -p game`, `cargo test -p client`.

**Spec:** `docs/superpowers/specs/2026-07-04-player-collider-avatar-design.md`

## Global Constraints

- Work on a feature branch off `develop` (e.g. `entity-collider-avatar`).
- Nothing player-only where an entity-generic mechanism is possible — the player is the first `EntityKind`, not a special case.
- `game`/`server` stay Bevy-free; determinism: sim changes execute only inside game events (`CreateClient`), identically on server and clients.
- Rollback bar: `hash(before) == hash(after undo)` bit-exact — collider undo goes through `physics.snapshot_raw()` (sanctioned tier-2 snapshot; do NOT hand-roll a ColliderSet inverse).
- Do not modify vendored forks.
- Existing suites stay green: `cargo test -p game`, `cargo test -p client`.

---

### Task 1: `EntityKind` component in the sim state

**Files:**
- Modify: `crates/game/src/state.rs` (enum, update variant, `Ecs.kind` field, `create_entity_safe`, `create_player_safe`)
- Modify: `crates/client/src/renderer/bridge.rs` (temporary no-op match arm so the workspace compiles; Task 3 replaces it)
- Test: `crates/game/tests/multi_client.rs`

**Interfaces:**
- Produces: `pub enum EntityKind { Player }` (Copy, Default = Player, serde + Hash); `GameDataUpdateKind::SetEntityKind(EntityKey, Option<EntityKind>)`; `Ecs` field `kind: Component<EntityKind>` readable as `data.ecs.kind.try_get(key) -> &Option<EntityKind>`; `create_player_safe` sets the player entity's kind to `Some(EntityKind::Player)` and emits `SetEntityKind` Do/Undo events.

- [ ] **Step 1: Write the failing test**

Append to `crates/game/tests/multi_client.rs`:

```rust
#[test]
fn create_player_sets_entity_kind() {
    let (server, _) = server_with_players(1);
    let data = server.data(&r0());
    let e = *data.player_entites.get(&0).unwrap();
    assert_eq!(*data.ecs.kind.try_get(e), Some(game::EntityKind::Player));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test multi_client -- create_player_sets_entity_kind`
Expected: FAIL to compile — no `EntityKind` in `game`, no `kind` field on `Ecs`.

- [ ] **Step 3: Implement**

In `crates/game/src/state.rs`:

Add above the `#[rollback]` module (next to `Client`):

```rust
/// What an entity *is*, for rendering and future gameplay. The player is the
/// first kind; NPC kinds extend this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, Hash)]
pub enum EntityKind {
    #[default]
    Player,
}
```

Add the update variant to `GameDataUpdateKind`:

```rust
    SetEntityKind(EntityKey, Option<EntityKind>),
```

Inside the `#[rollback(GameData)] mod game_data` module: add `EntityKind` to the `use super::{...}` import list, and add the field to `Ecs`:

```rust
    pub struct Ecs {
        #[undo(slotmap)]
        #[emit(insert = CreateEntity, remove = RemoveEntity)]
        entities: SlotMap<EntityKey, ()>,
        camera: Component<Camera>,
        isometry: Component<IsometryReal>,
        rigidbody: Component<RigidBodyHandle>,
        chunk: Component<Chunk>,
        kind: Component<EntityKind>,
    }
```

In `Undo<Ecs>::create_entity_safe`, add alongside the other component slots:

```rust
        self.kind.insert_safe(key);
```

At the end of `Rollback::create_player_safe` (after the camera `set_safe`, so a joining renderer sees camera before kind), following the camera emit pattern:

```rust
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityKind(e, Some(EntityKind::Player)),
        ));
        self.data.ecs.kind.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetEntityKind(e, None),
        ));
        self.data.ecs.kind.set_safe(e, Some(EntityKind::Player));
        self.data.player_entites.insert(client_id, e);
```

(`player_entites.insert` stays the last line; the kind block goes before it.)

In `crates/client/src/renderer/bridge.rs`, `drain_region_updates` match — temporary arm so the workspace compiles (replaced in Task 3):

```rust
                GameDataUpdateKind::SetEntityKind(_key, _kind) => {
                    // Task 3 (SimKind mirror) replaces this arm.
                }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p game && cargo check --workspace`
Expected: all game tests PASS (including the new one and the rollback-exercising `foreign_create_client...`, which now rolls back through the kind set); workspace check clean.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/state.rs crates/game/tests/multi_client.rs crates/client/src/renderer/bridge.rs
git commit -m "feat(game): EntityKind component; player entities are kind Player"
```

---

### Task 2: Generic capsule-collider helper, wired into player creation

**Files:**
- Modify: `crates/game/src/state.rs` (`attach_capsule_collider_safe`, call in `create_player_safe`, import `ColliderBuilder`)
- Test: `crates/game/tests/multi_client.rs`

**Interfaces:**
- Consumes: existing `snapshot_raw()` tier-2 snapshot API on `physics`.
- Produces: `Rollback::attach_capsule_collider_safe(&mut self, e: EntityKey, body: RigidBodyHandle, half_height: f32, radius: f32)` — entity-generic; NPC spawns reuse it.

- [ ] **Step 1: Write the failing test**

Append to `crates/game/tests/multi_client.rs`:

```rust
#[test]
fn create_player_attaches_capsule_collider() {
    let (server, _) = server_with_players(1);
    let data = server.data(&r0());
    let e = *data.player_entites.get(&0).unwrap();
    let handle = *data.ecs.rigidbody.get(e);
    let body = data.physics.bodies.get(handle).unwrap();
    assert_eq!(body.colliders().len(), 1, "player body carries exactly one collider");
    let collider = data.physics.colliders.get(body.colliders()[0]).unwrap();
    assert_eq!(collider.parent(), Some(handle));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test multi_client -- create_player_attaches_capsule_collider`
Expected: FAIL on the `colliders().len()` assertion (0 != 1).

- [ ] **Step 3: Implement**

In `crates/game/src/state.rs`: add `ColliderBuilder` to the rapier prelude import:

```rust
use rapier3d::prelude::{ColliderBuilder, RigidBodyBuilder, RigidBodyHandle};
```

Add to the `impl Rollback` block (next to `create_player_safe`):

```rust
    /// Attach a capsule collider to an entity's body. Entity-generic: player
    /// today, NPCs later. Undo restores the whole PhysicsState snapshot —
    /// the vendored fork has no exact ColliderSet inverse, and
    /// insert_with_parent also mutates the parent body's mass properties.
    pub fn attach_capsule_collider_safe(
        &mut self,
        e: EntityKey,
        body: RigidBodyHandle,
        half_height: f32,
        radius: f32,
    ) {
        let collider = ColliderBuilder::capsule_y(Real::from(half_height), Real::from(radius))
            .user_data(e.data().as_ffi() as u128)
            .build();
        let p = self.data.physics.snapshot_raw();
        p.colliders.insert_with_parent(collider, body, p.bodies);
    }
```

In `create_player_safe`, right after `self.data.ecs.rigidbody.set_safe(e, Some(handle));`:

```rust
        self.attach_capsule_collider_safe(e, handle, 0.5, 0.4);
```

- [ ] **Step 4: Run the full game suite**

Run: `cargo test -p game`
Expected: all PASS. `foreign_create_client_inserts_into_predicted_timeline` and `foreign_player_input_converges_and_undo_stays_bit_exact` now roll back and re-apply the collider insert; the hash checks inside the rollback machinery verify the snapshot undo is bit-exact.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/state.rs crates/game/tests/multi_client.rs
git commit -m "feat(game): entity-generic capsule collider helper; player body gets a capsule"
```

---

### Task 3: Bridge mirrors `EntityKind` into a `SimKind` component

**Files:**
- Modify: `crates/client/src/renderer/bridge.rs` (component, snapshot walk, drain arm, tests)

**Interfaces:**
- Consumes: `EntityKind`, `SetEntityKind` (Task 1); `data.ecs.kind.try_get(key)`.
- Produces: `#[derive(Component, Clone, Copy)] pub struct SimKind(pub game::EntityKind);` present on every kind-bearing entity, removed on `SetEntityKind(_, None)`. Task 4's avatar system consumes it.

- [ ] **Step 1: Write the failing tests**

Append to `crates/client/src/renderer/bridge.rs` `mod tests`:

```rust
    #[test]
    fn set_entity_kind_mirrors_to_sim_kind() {
        let (mut app, _c, updates, region_id) = test_app();
        app.update();
        let k = key(5);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityKind(k, Some(game::EntityKind::Player)),
        )).unwrap();
        app.update();
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).unwrap();
        assert!(app.world().entity(e).contains::<SimKind>());

        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityKind(k, None),
        )).unwrap();
        app.update();
        assert!(!app.world().entity(e).contains::<SimKind>());
    }

    #[test]
    fn snapshot_carries_entity_kind() {
        let (mut app, client) = app_shell();
        let (_update_send, update_recv) = crossbeam::channel::unbounded();
        let mut rb = Rollback::new(None);
        rb.new_transaction();
        rb.create_player_safe(0);
        rb.create_player_safe(1);
        let data = (*rb.data).clone();
        let k0 = *data.player_entites.get(&0).unwrap();
        let k1 = *data.player_entites.get(&1).unwrap();

        let region_id = ChunkCoords::new(0, 0, 0);
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_id, data, update_recv)).unwrap();
        app.update();

        let map = app.world().resource::<SimEntityMap>();
        let e0 = *map.0.get(&(region_id, k0)).unwrap();
        let e1 = *map.0.get(&(region_id, k1)).unwrap();
        assert!(app.world().entity(e0).contains::<SimKind>());
        assert!(app.world().entity(e1).contains::<SimKind>());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p client -- bridge`
Expected: both new tests FAIL to compile (`SimKind` undefined) — then, after adding only the struct, FAIL on the `contains::<SimKind>()` assertions.

- [ ] **Step 3: Implement**

In `crates/client/src/renderer/bridge.rs`:

Component (next to `VoxelData`):

```rust
/// Mirror of the sim's `EntityKind` for this entity. Consumed by the avatar
/// system to attach a renderable mesh.
#[derive(Component, Clone, Copy)]
pub struct SimKind(pub game::EntityKind);
```

Snapshot walk (`spawn_region_snapshot`), after the camera block:

```rust
        if let Some(kind) = *data.ecs.kind.try_get(key) {
            e.insert(SimKind(kind));
        }
```

Replace Task 1's temporary drain arm:

```rust
                GameDataUpdateKind::SetEntityKind(key, kind) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: SetEntityKind for unmapped {:?}", key);
                        continue;
                    };
                    match kind {
                        Some(k) => { commands.entity(e).insert(SimKind(k)); }
                        None => { commands.entity(e).remove::<SimKind>(); }
                    }
                }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p client`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/bridge.rs
git commit -m "feat(client): bridge mirrors EntityKind into SimKind component"
```

---

### Task 4: Avatar system — kind → mesh, local player excluded

**Files:**
- Create: `crates/client/src/renderer/avatar.rs`
- Modify: `crates/client/src/renderer/mod.rs` (module + resource + system registration)

**Interfaces:**
- Consumes: `SimKind` (Task 3); `Camera3d` presence as the local-player marker (only the local player's entity ever has one).
- Produces: `avatar::AvatarAssets` resource; `avatar::attach_avatars` system in `Update`.

- [ ] **Step 1: Write the failing tests**

Create `crates/client/src/renderer/avatar.rs` with tests first (implementation stubbed to compile):

```rust
use bevy::prelude::*;

use super::bridge::SimKind;
use game::EntityKind;

/// Lazily-created shared avatar assets (one mesh + material per kind for now).
#[derive(Resource, Default)]
pub struct AvatarAssets(pub Option<(Handle<Mesh>, Handle<StandardMaterial>)>);

/// Attaches a renderable capsule to kind-bearing entities. Entities with an
/// active `Camera3d` are the local player — the first-person camera sits
/// inside the capsule, so the local player gets no mesh.
pub fn attach_avatars(
    mut commands: Commands,
    added: Query<(Entity, &SimKind), (Added<SimKind>, Without<Camera3d>)>,
    mut assets: ResMut<AvatarAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let _ = (&mut commands, &added, &mut assets, &mut meshes, &mut materials);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<Mesh>();
        app.init_asset::<StandardMaterial>();
        app.init_resource::<AvatarAssets>();
        app.add_systems(Update, attach_avatars);
        app
    }

    #[test]
    fn remote_player_gets_capsule_mesh() {
        let mut app = test_app();
        let e = app.world_mut().spawn(SimKind(EntityKind::Player)).id();
        app.update();
        assert!(app.world().entity(e).contains::<Mesh3d>());
        assert!(app.world().entity(e).contains::<MeshMaterial3d<StandardMaterial>>());
    }

    #[test]
    fn local_player_with_camera_gets_no_mesh() {
        let mut app = test_app();
        let e = app
            .world_mut()
            .spawn((SimKind(EntityKind::Player), Camera3d::default()))
            .id();
        app.update();
        assert!(!app.world().entity(e).contains::<Mesh3d>());
    }

    #[test]
    fn mesh_and_material_handles_are_shared() {
        let mut app = test_app();
        let e1 = app.world_mut().spawn(SimKind(EntityKind::Player)).id();
        let e2 = app.world_mut().spawn(SimKind(EntityKind::Player)).id();
        app.update();
        let m1 = app.world().entity(e1).get::<Mesh3d>().unwrap().0.clone();
        let m2 = app.world().entity(e2).get::<Mesh3d>().unwrap().0.clone();
        assert_eq!(m1, m2);
    }
}
```

Register in `crates/client/src/renderer/mod.rs`: add `mod avatar;` next to the other modules, and in `SimBridgePlugin::build` add `.init_resource::<avatar::AvatarAssets>()` after the other `init_resource` calls and `avatar::attach_avatars` to the `Update` system tuple:

```rust
            .add_systems(
                Update,
                (
                    meshing::queue_meshing,
                    meshing::apply_meshed_chunks,
                    interpolate::interpolate_transforms,
                    avatar::attach_avatars,
                ),
            );
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p client -- avatar`
Expected: `remote_player_gets_capsule_mesh` and `mesh_and_material_handles_are_shared` FAIL (stub attaches nothing); `local_player_with_camera_gets_no_mesh` passes trivially (guards the exclusion once the body is real).

- [ ] **Step 3: Implement the system body**

Replace `attach_avatars`'s body:

```rust
    for (e, kind) in &added {
        match kind.0 {
            EntityKind::Player => {
                let (mesh, material) = assets
                    .0
                    .get_or_insert_with(|| {
                        (
                            // Total height 1.8 m: mirrors the sim capsule
                            // capsule_y(0.5, 0.4) in create_player_safe.
                            meshes.add(Capsule3d::new(0.4, 1.0)),
                            materials.add(StandardMaterial {
                                base_color: Color::srgb(0.8, 0.3, 0.3),
                                ..Default::default()
                            }),
                        )
                    })
                    .clone();
                commands.entity(e).insert((Mesh3d(mesh), MeshMaterial3d(material)));
            }
        }
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p client`
Expected: all PASS (avatar tests + bridge tests + the rest).

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/avatar.rs crates/client/src/renderer/mod.rs
git commit -m "feat(client): render capsule avatars for non-local kind-bearing entities"
```

---

### Task 5: End-to-end verification

**Files:** none (verification only).

- [ ] **Step 1: Full build and suites**

```bash
~/Software/rustc_codegen_cranelift/dist/cargo-clif build --workspace --bins
cargo test -p game
cargo test -p client
```
Expected: build succeeds; every test PASSES.

- [ ] **Step 2: Live two-client run**

```bash
./target/debug/server > /tmp/mmo-server.log 2>&1 &
WGPU_ADAPTER_NAME="6700" ./target/debug/client > /tmp/mmo-client1.log 2>&1 &
sleep 5
WGPU_ADAPTER_NAME="6700" ./target/debug/client > /tmp/mmo-client2.log 2>&1 &
```

Verify:
- Window 1 shows a capsule where player 2 spawned (0, 1, 5), moving when window 2 flies (WASD after pressing E for free-cam).
- Neither window renders its own capsule blocking the first-person view.
- No panics, no "Camera order ambiguities", no reconcile spam in either log.
- Hashes still converge: no `Hash verification failed` lines in any log (would indicate the collider snapshot undo is not bit-exact).

Then stop all three processes.

- [ ] **Step 3: Verify skill / final review**

Run the superpowers:verification-before-completion flow, then superpowers:finishing-a-development-branch (merge to `develop` after user choice).
