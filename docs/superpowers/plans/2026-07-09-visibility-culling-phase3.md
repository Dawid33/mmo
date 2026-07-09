# Visibility Culling (Voxel Rendering Phase 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop drawing chunks the camera provably cannot see: a Tommo-style cave-culling visibility graph (per-chunk face-connectivity bitmask + per-frame BFS from the camera) on all platforms, plus Bevy's HZB occlusion culling on native.

**Architecture:** Connectivity is derived render-side data: a `u16` of the 15 unordered face-pair connections through a chunk's non-solid space, computed by flood fill inside the existing async mesh task (occupancy already in hand) and attached as a component next to `Mesh3d`. A per-frame system BFSes the chunk grid from the camera's cell — traversing grid-absent cells as open air within a bounded domain (chunk entities only exist in the materialized terrain band; sky/deep-stone cells have no entities) — applying Tommo's no-backtrack direction rule and the entry→exit mask check, frustum test last, and writes `Visibility::Hidden`/`Inherited` on chunk entities only (provably disjoint from the ghost/UI `Visibility` writers). Native cameras additionally get `DepthPrepass` + `OcclusionCulling` (own commit, trivially revertible — the component is experimental).

**Tech Stack:** Rust, Bevy 0.18.1, client crate only.

**Spec:** `docs/superpowers/specs/2026-07-08-voxel-rendering-optimization-design.md` §7 (7.3 orientation buckets stay deferred).

## Global Constraints

- All changes in `crates/client`. `game`/`server`/vendored forks untouched. Nothing enters rollback-hashed state — connectivity masks are recomputed on remesh, never journaled or hashed.
- Face indexing reuses the LOCKED `Face::index()` order (PosX=0, NegX=1, PosY=2, NegY=3, PosZ=4, NegZ=5).
- Pair indexing (LOCKED, test-pinned bijection): for faces `a < b`, `pair_index(a, b) = a*5 - a*(a-1)/2 + (b - a - 1)` — 0..15.
- Solidity for the flood fill = `!Voxel::is_air()`, interior only (padded coords 1..=32; the 0/33 shell is neighbor data and MUST be excluded).
- Over-visibility is safe (draws extra); under-visibility is the failure mode (holes in the world). Every ambiguous case resolves toward visible: grid-absent cells traverse as fully-open air; chunks without a `ChunkConnectivity` component yet (mesh task in flight) traverse as fully open AND stay visible; culling disabled ⇒ everything `Inherited`.
- Tunables as named constants: `CULL_BFS_MAX_CHEBYSHEV: i32 = 64` (hard BFS radius cap in chunks), plus the `CullingConfig` resource (`graph_enabled: bool`, default `true`) so tests and the F4 toggle can disable the graph.
- Existing invariants preserved: `VoxelData` liveness rule, task-supersede, upload budget counting, constant-Aabb insert.
- Tag any test measured >100ms with `#[ignore = "slow: >100ms — run via just test-full"]`.

**Verified facts (from repo HEAD 07dd393 + bevy 0.18.1 sources — do not re-derive):**
- `MeshingTask(Task<Option<Mesh>>)` at `meshing/mod.rs:28` (private field); the spawn closure is `pool.spawn(async move { build_chunk_mesh(&padded, &BlockTextureLayers(layers)) })` at `queue.rs:134`.
- `apply_meshed_chunks` Some-arm inserts `(Mesh3d, Aabb const, NoAutoAabb)` + material; None-arm removes `(Mesh3d, MeshMaterial3d<…>, MeshingTask)`.
- No existing system writes `Visibility` on chunk entities (ghost dedupe writes only `GhostSource` entities; HUD writes only UI nodes).
- `OcclusionCulling`: `bevy::render::experimental::occlusion_culling::OcclusionCulling` (unit struct; experimental; REQUIRES `DepthPrepass` else ignored; incompatible with deferred — we don't use deferred). `DepthPrepass`: `bevy::core_pipeline::prepass::DepthPrepass` (unit struct). Neither is in the prelude.
- `Frustum` is auto-required by `Camera` (present on every `Camera3d` under DefaultPlugins; absent under MinimalPlugins tests — handle `Option<&Frustum>`). `Frustum::intersects_sphere(&Sphere, bool)` is the proven API (already used in `queue.rs:65-82`).
- Chunk entities exist only for materialized chunks (all-air subchunks collapse in `Column::normalize`; worldgen materializes only the `[lo..=hi]` terrain band). `ChunkGrid` keys = world position / 32; y-range is a thin band around terrain height.
- The Phase-2 material already has a custom prepass vertex shader, so `DepthPrepass` works with packed chunk meshes.
- Debug-toggle precedent: `hud::toggle_debug` flips things on F3; culling gets its own F4 flip on a client-only resource (no game-thread flag needed — render-side only).

**Preflight:** worktree branch at develop HEAD (≥ 07dd393), `git status` clean, `cargo test -p client 2>&1 | tail -3` green.

---

### Task 1: Connectivity flood fill (`visibility.rs`, pure functions)

**Files:**
- Create: `crates/client/src/renderer/meshing/visibility.rs`
- Modify: `crates/client/src/renderer/meshing/mod.rs` (add `pub mod visibility;`)
- Test: unit tests inside `visibility.rs`

**Interfaces:**
- Consumes: `PaddedVoxels` (`is_solid`, padded interior 1..=32), `Face` (`index`).
- Produces (used by Tasks 2–3):
  - `pub fn pair_index(a: u32, b: u32) -> u32` — requires `a < b`, both 0..6; returns 0..15.
  - `pub fn pair_bit(a: u32, b: u32) -> u16` — `1 << pair_index(min, max)` for unordered input.
  - `pub fn face_connectivity(p: &PaddedVoxels) -> u16` — bit set per face-pair connected through the chunk's own non-solid interior.
  - `pub const ALL_CONNECTED: u16 = 0x7FFF;`

- [ ] **Step 1: Write failing tests**

Create `crates/client/src/renderer/meshing/visibility.rs`:

```rust
//! Cave-culling visibility graph (Tommo/MCPE "Advanced Cave Culling"):
//! per-chunk face-pair connectivity computed by flood fill over the chunk's
//! own 32³ non-solid space, and the per-frame camera BFS that hides chunks
//! no sight line can reach. All state here is render-derived — recomputed on
//! remesh, never rollback-hashed.
//! Reference: https://tomcc.github.io/2014/08/31/visibility-1.html

use super::binary::{Face, PaddedVoxels};

#[cfg(test)]
mod tests {
    use super::*;
    use game::{BlockId, Voxel, CHUNK_VOXEL_COUNT};

    fn interior(cells: &[[usize; 3]]) -> PaddedVoxels {
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for [x, y, z] in cells {
            v[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(1));
        }
        PaddedVoxels::from_interior(&v)
    }

    fn solid_except(air: &[[usize; 3]]) -> PaddedVoxels {
        let mut v = vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT];
        for [x, y, z] in air {
            v[x + 32 * y + 32 * 32 * z] = Voxel::default();
        }
        PaddedVoxels::from_interior(&v)
    }

    #[test]
    fn pair_index_is_a_bijection_onto_0_15() {
        let mut seen = [false; 15];
        for a in 0..6u32 {
            for b in (a + 1)..6u32 {
                let i = pair_index(a, b) as usize;
                assert!(!seen[i], "pair ({a},{b}) collides at {i}");
                seen[i] = true;
            }
        }
        assert!(seen.iter().all(|&s| s), "all 15 indices hit");
    }

    #[test]
    fn empty_chunk_connects_everything() {
        assert_eq!(face_connectivity(&interior(&[])), ALL_CONNECTED);
    }

    #[test]
    fn solid_chunk_connects_nothing() {
        assert_eq!(face_connectivity(&solid_except(&[])), 0);
    }

    #[test]
    fn straight_tunnel_connects_exactly_its_two_faces() {
        // 1-voxel tunnel along X at y=5, z=5 through an otherwise solid chunk.
        let air: Vec<[usize; 3]> = (0..32).map(|x| [x, 5, 5]).collect();
        let mask = face_connectivity(&solid_except(&air));
        assert_eq!(mask, pair_bit(Face::PosX.index(), Face::NegX.index()));
    }

    #[test]
    fn l_tunnel_connects_exactly_its_corner_pair() {
        // Air along -X to center, then down to -Y: connects NegX <-> NegY only.
        let mut air: Vec<[usize; 3]> = (0..=16).map(|x| [x, 16, 16]).collect();
        air.extend((0..=16).map(|y| [16, y, 16]));
        let mask = face_connectivity(&solid_except(&air));
        assert_eq!(mask, pair_bit(Face::NegX.index(), Face::NegY.index()));
    }

    #[test]
    fn open_column_connects_vertical_pair() {
        let air: Vec<[usize; 3]> = (0..32).map(|y| [7, y, 9]).collect();
        let mask = face_connectivity(&solid_except(&air));
        assert_eq!(mask, pair_bit(Face::PosY.index(), Face::NegY.index()));
    }

    #[test]
    fn two_isolated_rooms_union_their_pairs_without_cross_connecting() {
        // Room A: tunnel X (touches ±X). Room B: tunnel Z (touches ±Z),
        // far apart, no shared air. Mask = both pairs, and NOT e.g. PosX<->PosZ.
        let mut air: Vec<[usize; 3]> = (0..32).map(|x| [x, 3, 3]).collect();
        air.extend((0..32).map(|z| [28, 28, z]));
        let mask = face_connectivity(&solid_except(&air));
        let expect = pair_bit(Face::PosX.index(), Face::NegX.index())
            | pair_bit(Face::PosZ.index(), Face::NegZ.index());
        assert_eq!(mask, expect);
    }

    #[test]
    fn interior_pocket_touching_no_face_adds_nothing() {
        // A sealed air pocket in the middle: connects no faces.
        let air: Vec<[usize; 3]> = vec![[15, 15, 15], [16, 15, 15]];
        assert_eq!(face_connectivity(&solid_except(&air)), 0);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p client --lib visibility:: 2>&1 | tail -3` (after adding `pub mod visibility;` to `meshing/mod.rs`) — Expected: COMPILE ERROR.

- [ ] **Step 3: Implement**

Add above the test module:

```rust
/// All 15 face pairs connected — the mask of an all-air (or absent) chunk.
pub const ALL_CONNECTED: u16 = 0x7FFF;

/// Index of the unordered face pair (a, b), a < b, faces in `Face::index()`
/// order. Bijection onto 0..15 (test-pinned).
pub fn pair_index(a: u32, b: u32) -> u32 {
    debug_assert!(a < b && b < 6);
    a * 5 - a * (a - 1) / 2 + (b - a - 1)
}

/// Bit for the unordered pair (a, b) in either order.
pub fn pair_bit(a: u32, b: u32) -> u16 {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    1 << pair_index(lo, hi)
}

/// Which of a cell's chunk-local coords sit on which face, in Face::index()
/// order: +X = x == 31, -X = x == 0, +Y = y == 31, -Y = y == 0, +Z/-Z same.
fn touched_faces(x: usize, y: usize, z: usize) -> u8 {
    let mut f = 0u8;
    if x == 31 { f |= 1 << 0; }
    if x == 0  { f |= 1 << 1; }
    if y == 31 { f |= 1 << 2; }
    if y == 0  { f |= 1 << 3; }
    if z == 31 { f |= 1 << 4; }
    if z == 0  { f |= 1 << 5; }
    f
}

/// Flood-fill the chunk's own 32³ non-solid space (padded interior 1..=32;
/// the 0/33 neighbor shell is deliberately excluded) and record, for each
/// connected air component, which chunk faces it touches; every pair of
/// touched faces is connected. O(32³) worst case, a few hundred µs — runs
/// inside the async mesh task, never on the main thread.
pub fn face_connectivity(p: &PaddedVoxels) -> u16 {
    const N: usize = 32;
    let idx = |x: usize, y: usize, z: usize| x + N * (y + N * z);
    let mut visited = vec![false; N * N * N];
    let mut mask = 0u16;
    let mut stack: Vec<(usize, usize, usize)> = Vec::new();

    for sz in 0..N {
        for sy in 0..N {
            for sx in 0..N {
                if visited[idx(sx, sy, sz)] || p.is_solid(sx + 1, sy + 1, sz + 1) {
                    continue;
                }
                // New air component: flood it, accumulate touched faces.
                let mut faces = 0u8;
                visited[idx(sx, sy, sz)] = true;
                stack.push((sx, sy, sz));
                while let Some((x, y, z)) = stack.pop() {
                    faces |= touched_faces(x, y, z);
                    let mut push = |nx: usize, ny: usize, nz: usize| {
                        if !visited[idx(nx, ny, nz)] && !p.is_solid(nx + 1, ny + 1, nz + 1) {
                            visited[idx(nx, ny, nz)] = true;
                            stack.push((nx, ny, nz));
                        }
                    };
                    if x + 1 < N { push(x + 1, y, z); }
                    if x > 0     { push(x - 1, y, z); }
                    if y + 1 < N { push(x, y + 1, z); }
                    if y > 0     { push(x, y - 1, z); }
                    if z + 1 < N { push(x, y, z + 1); }
                    if z > 0     { push(x, y, z - 1); }
                }
                // Union all pairs among the touched faces.
                for a in 0..6u32 {
                    if faces >> a & 1 == 0 { continue; }
                    for b in (a + 1)..6u32 {
                        if faces >> b & 1 == 1 {
                            mask |= pair_bit(a, b);
                        }
                    }
                }
                if mask == ALL_CONNECTED {
                    return mask; // can't grow further
                }
            }
        }
    }
    mask
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p client --lib visibility:: 2>&1 | tail -3` — Expected: 7 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/meshing
git commit -m "feat(client): face-connectivity flood fill for cave culling (15-pair u16 mask)"
```

---

### Task 2: Compute the mask in the mesh task; attach `ChunkConnectivity`

**Files:**
- Modify: `crates/client/src/renderer/meshing/mod.rs` (MeshOutput, MeshingTask, apply arms, tests)
- Modify: `crates/client/src/renderer/meshing/queue.rs` (spawn closure)
- Test: extended headless tests in `mod.rs`

**Interfaces:**
- Produces (used by Task 3):
  - `#[derive(Component, Copy, Clone)] pub struct ChunkConnectivity(pub u16);` (in `visibility.rs`)
  - `pub struct MeshOutput { pub mesh: Option<Mesh>, pub connectivity: u16 }` (in `meshing/mod.rs`)
  - `pub fn mesh_chunk(padded: &PaddedVoxels, layers: &BlockTextureLayers) -> MeshOutput` — `build_chunk_mesh` keeps its exact signature and semantics; `mesh_chunk` wraps it plus `face_connectivity`.
  - `apply_meshed_chunks` inserts `ChunkConnectivity` in BOTH arms (an all-air chunk is fully traversable: `ALL_CONNECTED`; its None arm keeps stripping the mesh but now inserts the mask instead of leaving none).

- [ ] **Step 1: Failing test**

Append to `meshing/mod.rs` tests (reuse the Aabb test's app pattern):

```rust
    #[test]
    fn meshed_chunks_carry_their_connectivity_mask() {
        use super::visibility::{ChunkConnectivity, ALL_CONNECTED};

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<crate::renderer::block_textures::BlockTextureLayers>()
            .init_resource::<queue::ChunkGrid>()
            .init_resource::<queue::RemeshQueue>()
            .init_resource::<queue::MeshBudgets>()
            .add_systems(
                Update,
                (queue::mark_remesh_dirty, queue::prune_chunk_grid, queue::spawn_mesh_tasks, apply_meshed_chunks)
                    .chain(),
            );
        // A solid chunk with a single straight X tunnel: mask must be exactly
        // the ±X pair — proves the real task computed real connectivity.
        let mut voxels = vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT];
        for x in 0..32 {
            voxels[x + 32 * 5 + 32 * 32 * 5] = Voxel::default();
        }
        let e = app.world_mut().spawn(crate::renderer::bridge::VoxelData(voxels)).id();
        settle_until_meshed(&mut app, e);
        let mask = app.world().entity(e).get::<ChunkConnectivity>().expect("mask attached").0;
        use super::binary::Face;
        assert_eq!(mask, super::visibility::pair_bit(Face::PosX.index(), Face::NegX.index()));

        // All-air chunk: mesh stripped, mask = ALL_CONNECTED.
        let air = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        let e2 = app.world_mut().spawn(crate::renderer::bridge::VoxelData(air)).id();
        // Air chunks never get Mesh3d; settle on the mask instead.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            app.update();
            if let Some(c) = app.world().entity(e2).get::<ChunkConnectivity>() {
                assert_eq!(c.0, ALL_CONNECTED);
                assert!(!app.world().entity(e2).contains::<Mesh3d>());
                break;
            }
            assert!(std::time::Instant::now() < deadline, "mask never attached");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
```

Run: `cargo test -p client --lib meshed_chunks_carry 2>&1 | tail -3` — Expected: COMPILE ERROR (`ChunkConnectivity`, `mesh_chunk` missing).

- [ ] **Step 2: Implement**

1. In `visibility.rs` add (with `use bevy::prelude::Component;` scoped appropriately):

```rust
/// The chunk's face-pair connectivity, attached next to `Mesh3d` by
/// `apply_meshed_chunks`. Derived render state — recomputed on every remesh.
#[derive(bevy::prelude::Component, Copy, Clone)]
pub struct ChunkConnectivity(pub u16);
```

2. In `meshing/mod.rs`:

```rust
/// Everything the async mesh task produces for one chunk.
pub struct MeshOutput {
    pub mesh: Option<Mesh>,
    pub connectivity: u16,
}

/// Mesh + connectivity in one pass over the padded snapshot.
pub fn mesh_chunk(padded: &PaddedVoxels, layers: &BlockTextureLayers) -> MeshOutput {
    MeshOutput {
        mesh: build_chunk_mesh(padded, layers),
        connectivity: visibility::face_connectivity(padded),
    }
}
```

`MeshingTask` becomes `pub struct MeshingTask(Task<MeshOutput>);`. In `apply_meshed_chunks`, poll yields `MeshOutput { mesh, connectivity }`; BOTH arms insert `visibility::ChunkConnectivity(connectivity)` (Some arm: add it to the existing insert tuple; None arm: `ec.insert(visibility::ChunkConnectivity(connectivity))` before the removals — note the None arm's `commands.entity(e).remove::<(…)>()` needs restructuring to also insert; use `let mut ec = commands.entity(e); ec.insert(...); ec.remove::<(…)>();`).

3. In `queue.rs`, the spawn closure becomes `pool.spawn(async move { mesh_chunk(&padded, &BlockTextureLayers(layers)) })` (import `mesh_chunk` from `super`).

4. Existing tests referencing the task's output type or `build_chunk_mesh` directly keep working (`build_chunk_mesh` unchanged); only the pipeline-level tests see the new component — no assertion changes required elsewhere.

- [ ] **Step 3: Full suite**

Run: `cargo test -p client 2>&1 | tail -5` — Expected: green.

- [ ] **Step 4: Commit**

```bash
git add crates/client/src/renderer/meshing
git commit -m "feat(client): mesh task computes ChunkConnectivity alongside the mesh"
```

---

### Task 3: Camera BFS culling system

**Files:**
- Modify: `crates/client/src/renderer/meshing/visibility.rs` (resource + system + F4 toggle)
- Modify: `crates/client/src/renderer/mod.rs` (register system + resource)
- Test: headless tests in `visibility.rs`

**Interfaces:**
- Produces:
  - `#[derive(Resource)] pub struct CullingConfig { pub graph_enabled: bool }` with `Default` = `true`.
  - `pub const CULL_BFS_MAX_CHEBYSHEV: i32 = 64;`
  - `pub fn cull_chunks(...)` system — registered in the Update chain AFTER `apply_meshed_chunks`.
  - `pub fn toggle_culling(...)` system — F4 flips `graph_enabled` (logs the new state via `info!`).

**Algorithm (the brief for the implementer — follow exactly):**
1. If `!config.graph_enabled`: set every chunk entity's `Visibility` to `Inherited` and return.
2. Camera: the active `Camera3d` (`Query<(&GlobalTransform, Option<&Frustum>, &Camera)>`, pick `camera.is_active`). No active camera → return (leave visibility unchanged). Camera cell = `world_translation / 32.0` floored per axis (`[i32; 3]`).
3. Expansion domain: compute the ChunkGrid keys' bounding box, inflate by 1 on every axis, and intersect with the Chebyshev ball of radius `CULL_BFS_MAX_CHEBYSHEV` around the camera cell; clamp the camera cell into the box if it's outside (flying above the band still sees down into it). Cells outside the domain are never enqueued.
4. BFS over cells `[i32; 3]` in the domain. Node state: `(cell, dirs_used: u8 /* bit per Face::index */, entry: Option<u8>)`. Visited set: `HashSet<([i32; 3], u8, u8)>` on the exact `(cell, dirs_used, entry)` triple. [PLAN CORRECTION, adjudicated during Task 3 review: the originally-planned `(cell, dirs_used)` key UNDER-VISITS — two paths reaching the same cell with identical dirs but different entry faces have different mask-permitted exits, and deduping them can deterministically hide reachable chunks (counterexample: an X-tunnel chunk at camera+(1,1,0) reached first via +X,+Y entry NegY dead-ends and masks out the +Y,+X path entering via NegX). Entry must be in the key. Still bounded: no-backtrack makes paths per-axis monotone, so dirs is a function of the cell's offset and entry is one of ≤3 opposites — ≤~3 visited states per cell.] Termination bound: the ≤64-radius domain.
5. Start node: `(camera_cell, 0)`, always visible. For each dequeued `(cell, dirs)`: mark the cell reached. For each of the 6 directions `d` (Face::index order):
   - No-backtrack: if `dirs` contains `opposite(d)`'s bit, skip. (`opposite(d) = d ^ 1` in the locked face order.)
   - Connectivity: if `cell` has a grid entity with `ChunkConnectivity(mask)`, and this node ENTERED the cell through face `f_in` (encoded: for the start cell there is no entry face — all exits allowed; otherwise `f_in = opposite(last step's d)`… implement by storing the entry face in the node: `(cell, dirs, entry: Option<u8>)`), require `mask & pair_bit(entry, exit_face_of(d)) != 0` where `exit_face_of(d) = d`. Grid-absent cells and entities without the component: fully open (no check).
   - Frustum: if the camera has a `Frustum`, test the NEIGHBOR cell's bounding sphere (center = cell*32 + 16 per axis, radius `16.0 * 3f32.sqrt()`, same as `queue.rs::chunk_rank`) with `intersects_sphere(&sphere, false)`; skip if outside. No `Frustum` (headless) → treat as inside.
   - Enqueue `(neighbor, dirs | bit(d), Some(opposite(d) as entry))` if not visited.
6. Apply: for every entity in `ChunkGrid`, set `Visibility` to `Inherited` if its cell was reached, else `Hidden` (write only on change, like `dedupe_ghosts`, to avoid change-detection churn).

- [ ] **Step 1: Failing headless tests**

Append to `visibility.rs` tests (build on the queue-test helpers' pattern — a `pipeline_app` with the full chain plus `cull_chunks` registered after `apply_meshed_chunks`, and `spawn_chunk` parenting chunks under a root; import them or replicate minimally):

```rust
    // Headless culling scene: chunks at keys along X, camera at origin cell.
    // solid = fully solid chunk (mask 0), open = all-air (no mesh, ALL_CONNECTED).

    #[test]
    fn wall_hides_what_is_behind_it() {
        // [camera cell 0,0,0 empty] [solid chunk at (1,0,0)] [tunnel chunk at (2,0,0)]
        // The solid chunk's mask is 0: BFS enters it but cannot pass through,
        // so (2,0,0) must be Hidden while (1,0,0) itself stays visible.
        // (Full test body: spawn via the real pipeline — solid chunk voxels
        // all BlockId(1); target chunk any content; camera entity with
        // GlobalTransform at (8, 8, 8) and Camera { is_active: true }; settle
        // meshes; run one update; assert Visibility values.)
    }

    #[test]
    fn open_air_gap_does_not_hide_distant_chunks() {
        // Chunks at (0,0,0) and (5,0,0) with NOTHING between them in the grid:
        // absent cells traverse as open air, so the far chunk stays visible.
    }

    #[test]
    fn opening_a_wall_reveals_the_room_behind_it() {
        // Start with the wall scene (behind chunk Hidden), then edit the solid
        // chunk's VoxelData to carve a straight X tunnel, settle the remesh,
        // run an update: the behind chunk must be Inherited again. This is the
        // spec §7.4 regression test (edit updates connectivity AND visibility).
    }

    #[test]
    fn disabling_the_graph_restores_everything() {
        // With the wall scene hidden, set CullingConfig.graph_enabled = false,
        // update once: every chunk Visibility is Inherited.
    }

    #[test]
    fn chunks_without_a_mask_yet_stay_visible_and_traversable() {
        // Spawn a chunk whose mesh task hasn't completed (huge in-flight budget
        // 0 so spawn_mesh_tasks never runs): BFS must treat it as open and NOT
        // hide anything behind it. Under-visibility is the failure mode.
    }
```

Write the full test bodies (the comments above define the scenarios and assertions; the queue.rs test module's `pipeline_app`/`spawn_chunk`/`wait_until` helpers show the established pattern — either make those helpers `pub(super)` and reuse them, or replicate the minimal subset locally; reuse is preferred). Camera note: in MinimalPlugins there is no `Frustum` and no transform propagation — spawn the camera with an explicit `GlobalTransform` and `Camera { is_active: true, ..Default::default() }` plus `Camera3d`.

Run: `cargo test -p client --lib visibility:: 2>&1 | tail -3` — Expected: COMPILE ERROR / failures.

- [ ] **Step 2: Implement the resource, system, and F4 toggle**

Follow the Algorithm block above exactly. The system signature shape:

```rust
pub fn cull_chunks(
    config: Res<CullingConfig>,
    grid: Res<super::queue::ChunkGrid>,
    cameras: Query<(&GlobalTransform, Option<&Frustum>, &Camera), With<Camera3d>>,
    masks: Query<&ChunkConnectivity>,
    mut chunks: Query<&mut Visibility, With<super::queue::ChunkKey>>,
) { /* … */ }

pub fn toggle_culling(
    keys: Res<ButtonInput<KeyCode>>,
    mut config: ResMut<CullingConfig>,
) {
    if keys.just_pressed(KeyCode::F4) {
        config.graph_enabled = !config.graph_enabled;
        info!("cave culling: {}", if config.graph_enabled { "on" } else { "off" });
    }
}
```

(`Frustum` import: `bevy::camera::primitives::{Frustum, Sphere}` as used in queue.rs. `Visibility` writes only on change.) Register in `renderer/mod.rs`: `init_resource::<meshing::visibility::CullingConfig>()`, extend the Update chain to `(mark_remesh_dirty, prune_chunk_grid, spawn_mesh_tasks, apply_meshed_chunks, visibility::cull_chunks).chain()`, and add `meshing::visibility::toggle_culling` to the non-chained Update set (near `hud::toggle_debug`).

- [ ] **Step 3: Full suite**

Run: `cargo test -p client 2>&1 | tail -5` and `cargo test -p client --lib -- --include-ignored 2>&1 | tail -3` — Expected: green. Time the new headless tests; slow-tag any >100ms.

- [ ] **Step 4: Commit**

```bash
git add crates/client/src/renderer
git commit -m "feat(client): camera BFS cave culling with no-backtrack rule and F4 toggle"
```

---

### Task 4: Native HZB occlusion culling (own, revertible commit)

**Files:**
- Modify: `crates/client/src/renderer/meshing/visibility.rs` (one small system) or `renderer/mod.rs` — implementer's choice, keep it to one site
- Test: native headless component-presence test

- [ ] **Step 1: Failing test**

```rust
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_cameras_get_depth_prepass_and_occlusion_culling() {
        use bevy::core_pipeline::prepass::DepthPrepass;
        use bevy::render::experimental::occlusion_culling::OcclusionCulling;

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_systems(Update, enable_native_occlusion);
        let cam = app.world_mut().spawn(bevy::core_pipeline::core_3d::Camera3d::default()).id();
        app.update();
        assert!(app.world().entity(cam).contains::<DepthPrepass>());
        assert!(app.world().entity(cam).contains::<OcclusionCulling>());
    }
```

(If `Camera3d`'s path differs in the prelude, use the same import the bridge uses. If `MinimalPlugins` lacks pieces these components need to merely EXIST as components, add the minimal plugin/registration the compiler asks for — they are plain unit components, so plain insertion should work.)

Run — Expected: COMPILE ERROR.

- [ ] **Step 2: Implement**

```rust
/// Native-only: opt every camera into the depth prepass + HZB two-phase
/// occlusion culling (experimental in Bevy 0.16+; REQUIRES DepthPrepass or
/// the component is ignored; incompatible with deferred, which we don't use).
/// Chunk meshes already prepass correctly via the packed-format prepass
/// vertex shader (phase 2). Own commit: revert this one commit to disable.
#[cfg(not(target_arch = "wasm32"))]
pub fn enable_native_occlusion(
    mut commands: Commands,
    cameras: Query<Entity, (With<Camera3d>, Without<bevy::core_pipeline::prepass::DepthPrepass>)>,
) {
    for e in &cameras {
        commands.entity(e).insert((
            bevy::core_pipeline::prepass::DepthPrepass,
            bevy::render::experimental::occlusion_culling::OcclusionCulling,
        ));
    }
}
```

Register in `renderer/mod.rs` under `#[cfg(not(target_arch = "wasm32"))]` in the Update set. (The `Without<DepthPrepass>` filter makes it idempotent without `Added` timing hazards — bridge inserts `Camera3d` via Commands at arbitrary times.)

- [ ] **Step 3: Suite + live smoke**

`cargo test -p client 2>&1 | tail -5` green. Then a 35s native run (server + client via `timeout`, foreground): grep the client log for wgpu/naga/pipeline errors — DepthPrepass now actually renders chunk depth through the phase-2 prepass shader, so this smoke is the real gate. Paste the grep into the report.

- [ ] **Step 4: Commit**

```bash
git add crates/client/src/renderer
git commit -m "feat(client): native HZB occlusion culling (DepthPrepass + OcclusionCulling opt-in)"
```

---

### Task 5: Gates + docs

- [ ] **Step 1:** CLAUDE.md `crates/client` bullet: `meshing/ (binary greedy mesher, AO, packed vertex format, neighbor gather, remesh queue)` → `meshing/ (binary greedy mesher, AO, packed vertex format, neighbor gather, remesh queue, cave-culling visibility)`.
- [ ] **Step 2:** Full gates, all green, synchronously: `just test`, `just test-full`, `cargo test -p server`, `cargo test -p client --release --test mesh_parity -- --include-ignored`. If a known load-flake appears, re-run the failing binary in isolation once and report both results.
- [ ] **Step 3:** Commit: `docs: phase 3 cave culling in CLAUDE.md` (body: note spec §7.3 orientation buckets remain deferred).

---

## Self-Review Notes (already applied)

- Spec §7.1 → Tasks 1–3 (mask at mesh time, BFS with view-direction rule, frustum last, Hidden for unreached, derived-state-only); §7.2 → Task 4 (native-only, requires-DepthPrepass caveat honored, own revertible commit); §7.4 → Task 1 unit shapes + Task 3's edit-opens-wall regression; §7.3 explicitly deferred in Task 5's commit body.
- The spec's "front-to-back ordering falls out of the BFS" benefit is NOT implemented (Bevy sorts draws itself) — intentional YAGNI, noted here so the final review doesn't hunt for it.
- Under-visibility guard rails are explicit constraints: absent cells open, maskless chunks open+visible, toggle restores, camera-cell clamp.
- Type consistency: `pair_bit`/`ALL_CONNECTED`/`face_connectivity`/`ChunkConnectivity` (T1/T2) consumed by T2/T3 under those names; `MeshOutput`/`mesh_chunk` (T2) consumed by queue.rs; `CullingConfig`/`cull_chunks`/`toggle_culling`/`enable_native_occlusion` (T3/T4) registered in renderer/mod.rs.
- Known risk flags: `HashSet<(cell, dirs)>` visited-set growth (bounded by domain size × ≤8 useful masks — fine at ~14k-cell domains); OcclusionCulling is experimental ("meshes turning invisible" per rustdoc) — isolated in its own commit; Task 3 tests need camera entities without DefaultPlugins (explicit GlobalTransform, no Frustum → treated as inside, which the tests rely on).
