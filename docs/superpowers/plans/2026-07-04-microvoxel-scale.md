# Microvoxel Scale Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Redefine the unit so 1 sim/render unit = 1 voxel = 1/16 m (player ≈ 29 voxels tall), with an 8×8-chunk 16 m × 16 m floor as the test world.

**Architecture:** Voxels stay 1×1×1 in unit space — meshing, the `Voxels` collider, and chunk offsets are untouched. All change concentrates in entity-scale constants (capsule, speed, camera planes, spawn, render snap thresholds) and world-gen content (`Chunk::flat_floor` + 8×8 grid in `World::basic`). Sim flip is one atomic task because world-gen and entity constants only test green together.

**Tech Stack:** Rust workspace; vendored rapier/parry. Build: `~/Software/rustc_codegen_cranelift/dist/cargo-clif build --workspace --bins`. Tests: `cargo test -p game`, `cargo test -p client`.

**Spec:** `docs/superpowers/specs/2026-07-04-microvoxel-scale-design.md`

## Global Constraints

- Work on a feature branch off `develop` (e.g. `microvoxel-scale`).
- 1 unit = 1/16 m exactly; player total height 28.8 units (capsule half_height 8.0 + radius 6.4).
- Floor: 8×8 chunks at y-layer 0, `flat_floor(8)` (solid full 32×32 footprint for y < 8) — seams must tile with no gaps.
- `game`/`server` stay Bevy-free; determinism unchanged (constants + content only, no new mechanisms).
- Hash-convergence multi-client suite must stay green throughout.
- Do not modify vendored forks.

---

### Task 1: Sim scale flip — world gen, capsule, speed, camera planes, spawn

**Files:**
- Modify: `crates/game/src/voxel.rs` (`Chunk::default` → all-air, new `Chunk::flat_floor`)
- Modify: `crates/game/src/state.rs` (`create_mesh` takes `chunk: Chunk`; capsule dims; spawn)
- Modify: `crates/game/src/region.rs` (`create_basic` passes `Chunk::flat_floor(8)`)
- Modify: `crates/game/src/lib.rs` (`World::basic` 8×8 loop)
- Modify: `crates/game/src/camera.rs` (`SPEED`, projection near/far)
- Test: `crates/game/tests/multi_client.rs` (updated assertions)

**Interfaces:**
- Produces: `Chunk::flat_floor(depth: u32) -> Chunk`; `Rollback::create_mesh(&mut self, coords: ChunkCoords, chunk: Chunk) -> EntityKey` (signature change); `Chunk::default()` becomes all-air (kept only because `Component<T>` requires `T: Default`).

- [ ] **Step 1: Update the tests to the new scale (RED)**

In `crates/game/tests/multi_client.rs`, replace `chunk_gets_voxels_collider`'s count assertion and the descend test's numbers:

```rust
#[test]
fn chunk_gets_voxels_collider() {
    let server = World::basic();
    let data = server.data(&r0());
    // 8x8 floor chunks, one Voxels collider each, before any player joins.
    assert_eq!(data.physics.colliders.len(), 64);
    let (_, collider) = data.physics.colliders.iter().next().unwrap();
    assert!(collider.shape().as_voxels().is_some(), "chunk collider is a Voxels shape");
    assert!(collider.parent().is_some(), "parented to the chunk's fixed body");
}
```

In `descending_player_stops_on_terrain`, replace the two assertions and their comment:

```rust
    // Floor top is y=8; capsule half-extent 14.4 -> blocked rest ~22.4+.
    // Uncorrected descent from y=26 at 8 units/tick would reach y << 0.
    assert!(y > game::parry::math::Real::from(21.0), "player tunneled through the floor: y = {y}");
    assert!(y < game::parry::math::Real::from(23.5), "player never moved down or never got blocked: y = {y}");
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p game --test multi_client -- chunk_gets_voxels_collider descending_player_stops_on_terrain`
Expected: `chunk_gets_voxels_collider` FAILS (1 != 64); descend test FAILS (old world: y settles ~2.9, below 21.0).

- [ ] **Step 3: Implement the world-gen half**

`crates/game/src/voxel.rs` — replace the `Default` impl and add `flat_floor`:

```rust
impl Default for Chunk {
    /// All-air. Exists because `Component<T>` requires `T: Default`;
    /// real content comes from constructors like [`Chunk::flat_floor`].
    fn default() -> Self {
        Self {
            collider: Vec::new(),
            voxels: vec![Voxel::new(VoxelType::Air); CHUNK_VOXEL_COUNT],
        }
    }
}

impl Chunk {
    /// Solid floor across the full 32×32 footprint for `y < depth`, air
    /// above. Full-footprint fill makes chunk seams tile with no gaps.
    pub fn flat_floor(depth: u32) -> Self {
        let mut voxels = Vec::with_capacity(ChunkShape::SIZE as usize);
        for i in 0..ChunkShape::SIZE {
            let [_x, y, _z] = ChunkShape::delinearize(i);
            let v = if y < depth {
                Voxel::new(VoxelType::Black)
            } else {
                Voxel::new(VoxelType::Air)
            };
            voxels.push(v);
        }
        Self { collider: Vec::new(), voxels }
    }
}
```

`crates/game/src/state.rs` — `create_mesh` takes the chunk as a parameter (first two lines change, rest stays):

```rust
    pub fn create_mesh(&mut self, coords: ChunkCoords, chunk: Chunk) -> EntityKey {
        let e = self.ecs.create_entity_safe();
        // Deterministic linearize order; grid coords are body-local.
        let solid: Vec<Point<i32>> = (0..ChunkShape::SIZE)
            ...unchanged...
```

`crates/game/src/region.rs` — `create_basic` supplies the floor chunk (add `Chunk` to the existing `use crate::{...}` list):

```rust
    pub fn create_basic(&mut self, coords: ChunkCoords) {
        self.data.create_mesh(coords, Chunk::flat_floor(8));
    }
```

`crates/game/src/lib.rs` — `World::basic` builds the 8×8 grid:

```rust
    pub fn basic() -> Self {
        let one = ChunkCoords::new(0, 0, 0);
        let mut data = Region::new(Rollback::new(None), None, one, None);
        for x in 0..8 {
            for z in 0..8 {
                data.create_basic(ChunkCoords::new(x, 0, z));
            }
        }

        return Self {
            regions: BTreeMap::from([(one, data)]),
        };
    }
```

- [ ] **Step 4: Implement the entity-scale half**

`crates/game/src/state.rs`, `create_player_safe`:

```rust
        // Spawn at the center of the 8x8-chunk floor (256x256 units = 16m x
        // 16m). Floor top y=8, capsule half-extent 14.4, ~3.6 units clear.
        let position = IsometryReal::from_parts(
            Translation3::new(Real::from(128.0), Real::from(26.0), Real::from(128.0)),
            Unit::<Quaternion<Real>>::identity(),
        );
```

and the capsule call becomes:

```rust
        // 28.8 units = 1.8 m at 1 unit = 1/16 m (~29 voxels, VS proportions).
        self.attach_capsule_collider_safe(e, handle, 8.0, 6.4);
```

`crates/game/src/camera.rs`:
- `const SPEED: Real = OrderedFloat(5.0);` → `const SPEED: Real = OrderedFloat(80.0);` (0.1 × 80 = 8 units/tick = 10 m/s).
- In `Camera::new`: `Perspective3::new(OrderedFloat(ASPECT), OrderedFloat(FOV_Y), OrderedFloat(0.1), OrderedFloat(100.0))` → `... OrderedFloat(1.0), OrderedFloat(2000.0))` (near 6 cm, far 125 m).
- In `impl Default for Camera`: same `0.1 → 1.0`, `100.0 → 2000.0` substitution.

- [ ] **Step 5: Run the full game suite**

Run: `cargo test -p game`
Expected: all PASS, including both updated tests and the untouched hash-convergence tests. Note: `World::basic` is now 64 chunks — the suite will be measurably slower (snapshot clones per `join_client`); that's the accepted cost, not a failure. If `descending_player_stops_on_terrain` fails with y ≈ 26 (never moved), check the spawn actually changed; if y ≈ 11.6 (half-extent ignored), the capsule dims didn't take.

- [ ] **Step 6: Commit**

```bash
git add crates/game/src/voxel.rs crates/game/src/state.rs crates/game/src/region.rs crates/game/src/lib.rs crates/game/src/camera.rs crates/game/tests/multi_client.rs
git commit -m "feat(game): microvoxel scale - 1 unit = 1/16 m, 8x8-chunk floor"
```

---

### Task 2: Client render constants

**Files:**
- Modify: `crates/client/src/renderer/avatar.rs` (capsule dims)
- Modify: `crates/client/src/renderer/bridge.rs` (`SimTarget` snap thresholds)

**Interfaces:**
- Consumes: sim capsule 8.0/6.4 from Task 1 (avatar mirrors it).
- Produces: no API change — constants only.

- [ ] **Step 1: Update the constants**

`crates/client/src/renderer/avatar.rs`, in `attach_avatars`:

```rust
                            // Total height 28.8 units = 1.8 m at 1/16 m per
                            // unit: mirrors the sim capsule capsule_y(8.0, 6.4)
                            // in create_player_safe.
                            meshes.add(Capsule3d::new(6.4, 16.0)),
```

`crates/client/src/renderer/bridge.rs`, `SimTarget` constructors — snap
thresholds are world-unit distances, ×16; smoothing and rotation snaps are
dimensionless/radians, unchanged:

```rust
impl SimTarget {
    pub fn body(pos: Vec3, rot: Quat) -> Self {
        Self { pos, rot, smoothing: 0.5, pos_snap: 1.6, rot_snap: 0.1 }
    }
    pub fn camera(pos: Vec3, rot: Quat) -> Self {
        Self { pos, rot, smoothing: 0.1, pos_snap: 0.008, rot_snap: 0.001 }
    }
}
```

- [ ] **Step 2: Check `interpolate.rs` for unit-bearing constants**

Run: `grep -n "0\.\|const" crates/client/src/renderer/interpolate.rs | head -30`
Read any hits: constants that are *distances* get ×16; per-frame lerp
factors and epsilons on normalized quantities stay. If its tests construct
`SimTarget` with literal fields (not the constructors), leave them — they
test the interpolation math, not world scale.

- [ ] **Step 3: Run the client suite**

Run: `cargo test -p client`
Expected: all 23 PASS (avatar/bridge tests assert component presence, not dimensions).

- [ ] **Step 4: Commit**

```bash
git add crates/client/src/renderer/avatar.rs crates/client/src/renderer/bridge.rs
git commit -m "feat(client): render constants for microvoxel scale"
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

Verify (E for free-cam, WASD/Space/Ctrl):
- The world reads as tiny voxels: the floor is a 16 m × 16 m expanse of 6 cm voxels, texture tiling correspondingly fine.
- Movement feels ~10 m/s; descending stops on the floor; edges of the floor slab visible at the world border.
- The other player's capsule is correctly proportioned (~29 voxels tall) and rests on the floor.
- Join time for client 2 is tolerable despite the ~MB-scale snapshot (watch-item from the spec; note the observed delay in the report).
- No panics, no `Hash verification failed`, no reconcile spam, no camera ambiguities in any log.

Then stop all three processes.

- [ ] **Step 3: Verify skill / final review**

Run the superpowers:verification-before-completion flow, then superpowers:finishing-a-development-branch (merge to `develop` after user choice).
