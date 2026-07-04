# Terrain Colliders + Solid Movement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Chunks carry a rapier `Voxels` collider matching their solid voxels, and player movement is collision-corrected via `KinematicCharacterController` so terrain is solid.

**Architecture:** An entity-generic `attach_voxels_collider_safe` on `Rollback` (PhysicsState-snapshot undo, same as the capsule helper) called from `create_mesh`; colliders reach clients inside region snapshots (the vendored parry `Voxels` shape is serde-serializable). In `CameraController::on_tick`, the desired translation runs through `move_shape` with a `QueryPipeline` view borrowed from the broad phase; only the corrected translation is written, through the existing `change()` snapshot, so no new undo surface exists.

**Tech Stack:** Rust workspace; vendored rapier/parry forks (`Voxels` shape, `KinematicCharacterController`, `BroadPhaseBvh::as_query_pipeline`). Build: `~/Software/rustc_codegen_cranelift/dist/cargo-clif build --workspace --bins`. Tests: `cargo test -p game`, `cargo test -p client`.

**Spec:** `docs/superpowers/specs/2026-07-04-terrain-colliders-design.md`

## Global Constraints

- Work on a feature branch off `develop` (e.g. `terrain-colliders`).
- Entity-generic mechanisms; nothing player-only beyond selecting the mover.
- `game`/`server` stay Bevy-free; sim changes run identically on server and clients (deterministic `Real`/libm math only).
- Rollback bar: `hash(before) == hash(after undo)` bit-exact; collider insertion undo uses `physics.snapshot_raw()` — do NOT hand-roll ColliderSet inverses.
- Do not modify vendored forks.
- Existing suites stay green: `cargo test -p game`, `cargo test -p client` (no client code changes expected at all).
- Spec deviation to carry through: spawn moves to `(2, 3, 5)` (not `(0, 3, 5)`) — the floor slab spans x,z ∈ [1,31), so x=0 is over a hole; amend the spec file in Task 2's commit.

---

### Task 1: Chunk `Voxels` collider

**Files:**
- Modify: `crates/game/src/state.rs` (imports, `attach_voxels_collider_safe`, `create_mesh`)
- Test: `crates/game/tests/multi_client.rs` (helpers `server_with_players`/`r0` live here)

**Interfaces:**
- Consumes: `snapshot_raw()` undo pattern; `ChunkShape`/`VoxelType` from `crate::voxel`.
- Produces: `Rollback::attach_voxels_collider_safe(&mut self, e: EntityKey, body: RigidBodyHandle, coords: &[Point<i32>])`. Grid cell `g` spans `[g, g+1)` in body-local space (parry floor semantics), which matches the block-mesh render mesh exactly.

- [ ] **Step 1: Write the failing test**

Append to `crates/game/tests/multi_client.rs`:

```rust
#[test]
fn chunk_gets_voxels_collider() {
    let server = World::basic();
    let data = server.data(&r0());
    // Exactly one collider exists before any player joins: the chunk's.
    assert_eq!(data.physics.colliders.len(), 1);
    let (_, collider) = data.physics.colliders.iter().next().unwrap();
    assert!(collider.shape().as_voxels().is_some(), "chunk collider is a Voxels shape");
    assert!(collider.parent().is_some(), "parented to the chunk's fixed body");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test multi_client -- chunk_gets_voxels_collider`
Expected: FAIL on `assert_eq!(..., 1)` — collider set is empty today.
(If `as_voxels()` does not exist on `&dyn Shape`, use `collider.shape().shape_type()` and compare against the voxels variant — check `crates/parry/src/shape/shape.rs`; adjust the assertion, not the production code.)

- [ ] **Step 3: Implement**

In `crates/game/src/state.rs`:

Extend imports:

```rust
use parry3d::math::{Point, RawReal, Real, Vector};
use block_mesh::ndshape::ConstShape;
use crate::voxel::{Chunk, ChunkCoords, ChunkShape, Voxel, VoxelType};
```

Add next to `attach_capsule_collider_safe` in `impl Rollback`:

```rust
    /// Attach a voxel-grid collider to an entity's body. One cell per solid
    /// voxel; cell g spans [g, g+1) in body-local space, matching the render
    /// mesh. Entity-generic. Undo restores the whole PhysicsState snapshot —
    /// same rationale as attach_capsule_collider_safe.
    pub fn attach_voxels_collider_safe(
        &mut self,
        e: EntityKey,
        body: RigidBodyHandle,
        coords: &[Point<i32>],
    ) {
        let collider = ColliderBuilder::voxels(Vector::repeat(Real::from(1.0)), coords)
            .user_data(e.data().as_ffi() as u128)
            .build();
        let p = self.data.physics.snapshot_raw();
        p.colliders.insert_with_parent(collider, body, p.bodies);
    }
```

In `create_mesh`, collect solid coords from the chunk (before `set_safe` moves it) and attach after the rigidbody is set:

```rust
    pub fn create_mesh(&mut self, coords: ChunkCoords) -> EntityKey {
        let e = self.ecs.create_entity_safe();
        let chunk = Chunk::default();
        // Deterministic linearize order; grid coords are body-local.
        let solid: Vec<Point<i32>> = (0..ChunkShape::SIZE)
            .filter(|i| chunk.voxels[*i as usize].kind != VoxelType::Air)
            .map(|i| {
                let [x, y, z] = ChunkShape::delinearize(i);
                Point::new(x as i32, y as i32, z as i32)
            })
            .collect();
        self.ecs.chunk.set_safe(e, Some(chunk));
        // ... existing body build + undo_scope insert unchanged ...
        self.ecs.rigidbody.set_safe(e, Some(handle));
        if !solid.is_empty() {
            self.attach_voxels_collider_safe(e, handle, &solid);
        }
        e
    }
```

(Only the `solid` collection and the trailing `if` are new; the body-insert block in the middle stays exactly as it is.)

- [ ] **Step 4: Run the full game suite**

Run: `cargo test -p game && cargo check --workspace`
Expected: all PASS — snapshot-based tests (`join_snapshot...`, hash-convergence) now carry the chunk collider through serialization, which verifies the `Voxels` shape serde roundtrip.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/state.rs crates/game/tests/multi_client.rs
git commit -m "feat(game): chunks carry a Voxels collider built from solid voxels"
```

---

### Task 2: Collision-corrected player movement + spawn fix

**Files:**
- Modify: `crates/game/src/camera.rs` (imports + movement block)
- Modify: `crates/game/src/state.rs` (spawn height/position in `create_player_safe`)
- Modify: `docs/superpowers/specs/2026-07-04-terrain-colliders-design.md` (spawn `(2, 3, 5)` amendment)
- Test: `crates/game/tests/multi_client.rs`

**Interfaces:**
- Consumes: player capsule collider (attached by `create_player_safe`); `broad_phase.as_query_pipeline(narrow_phase.query_dispatcher(), &bodies, &colliders, filter)`; `KinematicCharacterController::move_shape(dt, &queries, shape, pos, desired_translation, events) -> EffectiveCharacterMovement`.
- Produces: no new API — behavior change only.

- [ ] **Step 1: Write the failing test**

Append to `crates/game/tests/multi_client.rs`:

```rust
#[test]
fn descending_player_stops_on_terrain() {
    let (mut server, _) = server_with_players(1);
    let mut results = BTreeMap::new();

    // Enable fps-cam movement (E toggles it during the next tick)...
    let e_press = InputEvent::Key { key: Key::KeyE, pressed: true };
    server.handle_region_event(GameEventKind::PlayerInput(0, e_press), r0()).unwrap();
    server.forget_last_event(&r0());
    server.progress_world_one_tick(&mut results);
    // ...then hold descend.
    let ctrl = InputEvent::Key { key: Key::ControlLeft, pressed: true };
    server.handle_region_event(GameEventKind::PlayerInput(0, ctrl), r0()).unwrap();
    server.forget_last_event(&r0());
    for _ in 0..60 {
        server.progress_world_one_tick(&mut results);
    }

    let data = server.data(&r0());
    let e = *data.player_entites.get(&0).unwrap();
    let handle = *data.ecs.rigidbody.get(e);
    let y: f32 = data.physics.bodies.get(handle).unwrap().translation().y.into();
    // Floor top is world y=2.0; capsule half-extent 0.9. Uncorrected descent
    // would reach y = 3 - 60*0.5 = -27. Corrected must rest just above 2.9.
    assert!(y > 2.5, "player tunneled through the floor: y = {y}");
    assert!(y < 3.0, "player never moved down: y = {y}");
}
```

Note: if `.into()` on `Real` doesn't yield `f32` directly, use `let y = data.physics.bodies.get(handle).unwrap().translation().y;` and compare against `Real::from(2.5)` / `Real::from(3.0)` — adjust the assertion, not the code under test.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test multi_client -- descending_player_stops_on_terrain`
Expected: FAIL on `y > 2.5` with a strongly negative y (direct kinematic writes ignore colliders). If it instead fails on `y < 3.0`, the E-toggle didn't take — check that the toggle tick ran before the ControlLeft press; do not proceed until the failure is the tunneling one.

- [ ] **Step 3: Implement**

In `crates/game/src/state.rs`, `create_player_safe` — spawn out of the floor hole and above the slab:

```rust
        let position = IsometryReal::from_parts(
            Translation3::new(Real::from(2.0), Real::from(3.0), Real::from(5.0)),
            Unit::<Quaternion<Real>>::identity(),
        );
```

In `crates/game/src/camera.rs`: add imports:

```rust
use rapier3d::control::KinematicCharacterController;
use rapier3d::prelude::QueryFilter;
```

Replace the translation-apply block in `CameraController::on_tick` (currently `if linvel != Vector3::zeros() { ... set_next_kinematic_translation(t + linvel); }`):

```rust
            if linvel != Vector3::zeros() {
                let t = b.translation().clone();
                // Collision-corrected movement: slide along / stop at terrain
                // instead of teleporting through it. Queries are read-only;
                // the sole write below stays under the change() snapshot.
                let corrected = match b.colliders().first().copied() {
                    Some(collider_handle) => {
                        let collider = data.physics.colliders.get(collider_handle).unwrap();
                        let queries = data.physics.broad_phase.as_query_pipeline(
                            data.physics.narrow_phase.query_dispatcher(),
                            &data.physics.bodies,
                            &data.physics.colliders,
                            QueryFilter::default().exclude_rigid_body(handle),
                        );
                        let controller = KinematicCharacterController {
                            // Pure fly movement: no downward snap while skimming
                            // the ground.
                            snap_to_ground: None,
                            ..Default::default()
                        };
                        controller
                            .move_shape(
                                Real::from(1.0),
                                &queries,
                                collider.shape(),
                                collider.position(),
                                linvel,
                                |_| {},
                            )
                            .translation
                    }
                    // No collider on the mover: keep the uncorrected motion.
                    None => linvel,
                };
                if corrected != Vector3::zeros() {
                    // change(): whole-set snapshot. Surgical field restores are
                    // NOT exact here — set_next_kinematic_* also wakes the body
                    // and marks it modified (hashed state the closure can't
                    // restore); the snapshot covers all of it.
                    let bodies = data.physics.bodies.change();
                    bodies
                        .get_mut(handle)
                        .unwrap()
                        .set_next_kinematic_translation(t + corrected);
                }
            }
```

(`handle` and `b` are already in scope from the existing code above this block; the pre-existing comment about `change()` moves with the write. Borrow order matters: all reads — `t`, collider handle, the query — complete before `data.physics.bodies.change()` takes the mutable borrow.)

In `docs/superpowers/specs/2026-07-04-terrain-colliders-design.md`, section "3. Spawn height": change `(0, 3, 5)` to `(2, 3, 5)` and append: "x=2 because the default chunk's floor slab spans x,z ∈ [1,31) — x=0 is over a hole."

- [ ] **Step 4: Run the full suites**

Run: `cargo test -p game && cargo test -p client`
Expected: all PASS — including the new terrain test and the untouched multi-client hash-convergence tests (the corrected movement is inside the deterministic tick, so client/server hashes still match). Client suite unaffected.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/camera.rs crates/game/src/state.rs crates/game/tests/multi_client.rs docs/superpowers/specs/2026-07-04-terrain-colliders-design.md
git commit -m "feat(game): collision-corrected player movement via kinematic character controller"
```

---

### Task 3: End-to-end verification

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

Verify (press E for free-cam, then WASD/Space/Ctrl):
- Descending (Ctrl) onto the floor stops on it instead of passing through.
- Flying sideways into the floor slab edge slides along it.
- The other player's capsule still renders and moves.
- No panics, no `Hash verification failed`, no reconcile spam in any log.

Then stop all three processes.

- [ ] **Step 3: Verify skill / final review**

Run the superpowers:verification-before-completion flow, then superpowers:finishing-a-development-branch (merge to `develop` after user choice).
