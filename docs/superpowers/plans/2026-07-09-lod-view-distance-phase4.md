# LOD + View Distance (Voxel Rendering Phase 4) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Real view distance: the subscribed far ring renders as 2× downsampled meshes (edits still visible, no protocol change), and beyond it the client generates coarse terrain rings directly from the deterministic worldgen at zero bandwidth — with skirts and a sticky ring band handling seams and popping.

**Architecture:** LOD chunks are a parallel, deliberately-isolated render path: a `LodVoxelData` component (NOT `VoxelData`) so they never enter `ChunkGrid`, `mark_remesh_dirty`, boundary fingerprints, or the culling BFS. Each LOD chunk is a normal 32³ coarse-cell grid meshed by the existing `mesh_chunk` (packed format unchanged), rendered with an entity `Transform` scaled by the LOD factor (verified: the WGSL uses the full affine; the constant local-space `Aabb` scales at cull time). Downsampling: 2×2×2 source chunks → one coarse grid (opaque-if-any + majority material). Far ring = factor 2 from streamed snapshots; worldgen rings = factor 4 (regions at Chebyshev 3–4) and factor 16 / block scale (5–8) from client-side `generate_region` calls, generated async under a budget, keyed to the viewer region with a sticky band. Ring-boundary cracks are closed by skirts: boundary side-face quads extended one coarse cell downward via a +1 y-bias in the packed grid (positions 0..=33; the format's 6-bit fields hold it, one debug_assert relaxes).

**Tech Stack:** Rust, Bevy 0.18.1, `crates/client` only (uses the existing `worldgen` dependency; `game`/`server`/protocol untouched).

**Spec:** `docs/superpowers/specs/2026-07-08-voxel-rendering-optimization-design.md` §8.

## Global Constraints

- All changes in `crates/client`. No protocol change; `game`/`server`/vendored forks untouched. Nothing enters rollback-hashed state.
- **LOD entities carry `LodVoxelData` + `LodChunk { factor }` and must NEVER receive `VoxelData` or `ChunkKey`** — the full-res pipeline (grid, fingerprints, culling BFS) must not see them. Assert this in tests.
- Downsample rule (LOCKED): a coarse cell is solid iff ANY of its source cells is solid (opaque-if-any); its material is the majority `BlockId` among solid source cells, ties broken by LOWEST `BlockId` (deterministic).
- Ring table (named constants):
  - `FAR_LOD_FACTOR: u32 = 2` — the streamed far ring (Chebyshev ≤ `FAR_VIEW_RADIUS` = 2, unchanged).
  - `WORLDGEN_RINGS: &[(i32, u32)] = &[(4, 4), (8, 16)]` — worldgen rings: regions at Chebyshev 3..=4 render at factor 4, 5..=8 at factor 16 (block scale). `WORLDGEN_VIEW_RADIUS: i32 = 8` (the table's last bound).
  - `RING_STICKY_BAND: i32 = 1` — a region keeps its current ring unless its Chebyshev distance moves beyond the assignment boundary by more than this (the spec's promote/demote hysteresis, at region granularity).
  - `WORLDGEN_TASKS_MAX_IN_FLIGHT: usize = 2` — async generation budget.
- Skirts: LOD meshes only; boundary side-face quads (face axis ≠ Y, at grid border planes 0/32) extend 1 coarse cell downward. Implemented via a +1 y-bias: all LOD vertex y-coords shift +1 (grid occupies y 1..=33; skirts reach y 0), and the entity Transform compensates with `translation.y -= factor as f32`. `pack()`'s position debug_assert relaxes from `<= 32` to `<= 33` with a comment naming the skirt bias (the 6-bit format holds 0..=63; the WGSL decode masks `& 63u` and needs NO change).
- SPEC DEVIATIONS (documented, approved rationale):
  1. §8.4's "adjacent rings differ by ≤1 LOD level" is relaxed: v1 ships factors 2 → 4 → 16 (the 4→16 step skips 8×). Skirts are sized to each mesh's OWN coarse cell, which closes cracks at any ratio; if the 4×→16× transition pops visibly, an 8× ring is a pure-config follow-up (add `(6, 8)` to the table). Generation cost is per-region, not per-factor, so graduated rings are cheap to add later.
  2. Worldgen rings assume the client's `worldgen::WORLD_SEED` matches the server's world. True for the bundled server today; if a foreign-seed server ever exists, far terrain would be wrong beyond the subscribed ring. Named follow-up: seed in the join handshake. Document in code where the seed is used.
  3. §8.2's far-ring refresh keeps the existing despawn+respawn-on-re-receipt shape (hash-gated + throttled server-side); at factor 2 the churn is ~1/8th of today's. Incremental far-region diffing stays deferred.
  4. Ring promotion re-generates the region (no worldgen-output cache in v1) — generation is budgeted and only triggered by region-boundary crossings.
- Over-visibility guard rails still bind: LOD chunks are always `Visibility::Inherited` (never culled by the BFS — they're keyless by design); under-draw of far terrain is the failure mode, over-draw is acceptable.
- `CULL_BFS_MAX_CHEBYSHEV`'s comment must be updated: with the far ring leaving `ChunkGrid`, the grid now spans only the near 3×3 window (24 chunks) — 64 still overshoots comfortably.
- Tag any test measured >100ms with `#[ignore = "slow: >100ms — run via just test-full"]`.

**Verified facts (HEAD e36f06f — do not re-derive):**
- `FarSnapshot { pub chunks: Vec<(ChunkCoords, Chunk)> }`; far chunk `coords.x/z` are REGION-LOCAL column indices 0..8, `coords.y` is the ABSOLUTE (signed) sub-chunk index; `spawn_far_region` (bridge.rs:185-212) spawns one child per chunk at local `coords * 32` with `VoxelData(derive_voxels(...))` — TODAY far chunks enter the full-res pipeline (grid, culling); this plan removes that.
- `worldgen::generate_region(coords: RegionCoords, seed: u64, mats: Materials) -> Vec<(ColumnCoords, Column)>`; `Column::materialized_sub_chunks(&self) -> Vec<(i32, Chunk)>` (absolute sub_y, skips Air). Client already links worldgen; `local_server.rs:40-49` shows the Materials + `worldgen::WORLD_SEED` construction to mirror.
- `pack()` debug_asserts `pos <= 32` (pack.rs:33) — must become `<= 33`; hardware fields are 6-bit, WGSL masks `& 63u`, unchanged.
- Scaled Transforms are safe: WGSL uses `mesh_position_local_to_world`/`mesh_normal_local_to_world` (full affine, inverse-transpose normals); the constant Aabb is LOCAL-space and Bevy scales it by the GlobalTransform at cull time.
- Async task pattern to mirror: `spawn_mesh_tasks` tail (queue.rs:114-137) — own the data, `pool.spawn(async move { mesh_chunk(&padded, ...) })`, store the Task in a component, poll with budgeted apply.
- `mesh_chunk(padded, layers) -> MeshOutput { mesh, connectivity }` — LOD path uses `.mesh` and DISCARDS `.connectivity` (LOD chunks don't participate in culling).
- `HudStatus.viewer_region: Option<RegionCoords>` (bridge) is the client's current viewer region — the ring manager keys off it.
- `FAR_VIEW_RADIUS = 2` (5×5 minus near 3×3 = 16 far regions); `REGION_CHUNKS = 8`; `REGION_SIZE = 256.0`.
- Client far tests exist in bridge.rs (`far_region_roots_at_offset_with_meshable_chunks` expects `VoxelData` on far chunks — this plan changes that expectation deliberately).

**Preflight:** worktree branch at develop HEAD (≥ e36f06f), `git status` clean, `cargo test -p client 2>&1 | tail -3` green.

---

### Task 1: Downsampling core (`meshing/lod.rs`, pure functions)

**Files:**
- Create: `crates/client/src/renderer/meshing/lod.rs`
- Modify: `crates/client/src/renderer/meshing/mod.rs` (add `pub mod lod;`)
- Test: unit tests inside `lod.rs`

**Interfaces:**
- Produces (used by Tasks 3–5):
  - `pub fn downsample2(sources: &[Option<&[Voxel]>; 8]) -> Vec<Voxel>` — merge up to 2×2×2 source 32³ grids (index `sx + 2*sy + 4*sz`; `None` = all-air) into one 32³ coarse grid at 2× the cell size. Each coarse cell (x,y,z) draws from source grid `(x/16, y/16, z/16)`'s 2×2×2 voxel block at `((x%16)*2, (y%16)*2, (z%16)*2)`. Solid iff any source voxel solid; material = majority BlockId among solid sources, ties → lowest BlockId.
  - `pub fn downsample_blocks(chunks: &[Option<&game::Chunk>; 8]) -> Vec<Voxel>` — factor-16 fast path: each source chunk contributes its 2×2×2 `blocks` directly as coarse cells (block id non-air = solid; chiseled blocks count as their block id — chisel detail is invisible at 16×). Output: a 4×4×4-cells-per-source arrangement... NO: 8 source chunks × 8 blocks = 64 cells = 4³ — this fills only the 0..4 corner of a 32³ grid. See the layout contract below.

**Layout contract (LOCKED for Tasks 3–5):** LOD grids are always 32³ `Vec<Voxel>` in the standard `x + 32*y + 32*32*z` order, representing 32 coarse cells per axis at `factor` voxels each. Composition is the CALLER's job: Task 4 assembles a factor-2 region (8×8 columns of 32³ → per 2×2×2-chunk group one `downsample2` call → 4×4 LOD grids per vertical pair); Task 5 assembles factor-4 (two `downsample2` applications) and factor-16 (one `downsample_blocks` call per 2×2×2-chunk group yields a 4³ block of cells; 8×8 columns → the whole REGION collapses into one 16×16×(4·spans) cell field, packed into as few 32³ grids as the vertical span needs — typically ONE grid covering 2×2 regions is a non-goal in v1: one grid per region, cells 16×16×N occupying the grid's 0..16 x/z corner, is fine and simple; document the wasted volume).

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use game::{Block, BlockId, Voxel, CHUNK_VOXEL_COUNT};

    fn grid_with(cells: &[([usize; 3], u16)]) -> Vec<Voxel> {
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for ([x, y, z], id) in cells {
            v[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(*id));
        }
        v
    }

    #[test]
    fn empty_sources_downsample_to_air() {
        let out = downsample2(&[None; 8]);
        assert!(out.iter().all(|v| v.is_air()));
        assert_eq!(out.len(), CHUNK_VOXEL_COUNT);
    }

    #[test]
    fn any_solid_source_voxel_makes_the_coarse_cell_solid() {
        // Source grid 0 (the -x-y-z octant), voxel (0,0,0) solid: coarse (0,0,0) solid.
        let g = grid_with(&[([0, 0, 0], 1)]);
        let mut sources: [Option<&[Voxel]>; 8] = [None; 8];
        sources[0] = Some(&g);
        let out = downsample2(&sources);
        assert_eq!(out[0].block, BlockId(1));
        assert!(out[1..].iter().all(|v| v.is_air()), "exactly one coarse cell");
    }

    #[test]
    fn octant_placement_maps_source_grids_to_coarse_sixteenths() {
        // Source grid 7 (+x+y+z octant), voxel (31,31,31): coarse (31,31,31).
        let g = grid_with(&[([31, 31, 31], 3)]);
        let mut sources: [Option<&[Voxel]>; 8] = [None; 8];
        sources[7] = Some(&g);
        let out = downsample2(&sources);
        assert_eq!(out[31 + 32 * 31 + 32 * 32 * 31].block, BlockId(3));
    }

    #[test]
    fn majority_material_wins_ties_to_lowest() {
        // One coarse cell: 8 source voxels = 3×id2, 3×id5, 2×air → tie 3-3 → id2.
        let mut g = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for (i, (x, y, z)) in [(0, 0, 0), (1, 0, 0), (0, 1, 0)].into_iter().enumerate() {
            let _ = i;
            g[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(2));
        }
        for (x, y, z) in [(1, 1, 0), (0, 0, 1), (1, 0, 1)] {
            g[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(5));
        }
        let mut sources: [Option<&[Voxel]>; 8] = [None; 8];
        sources[0] = Some(&g);
        let out = downsample2(&sources);
        assert_eq!(out[0].block, BlockId(2), "tie broken to lowest id");
    }

    #[test]
    fn downsample_is_deterministic() {
        let g = grid_with(&[([5, 6, 7], 1), ([5, 7, 7], 2), ([4, 6, 7], 2)]);
        let mut sources: [Option<&[Voxel]>; 8] = [None; 8];
        sources[0] = Some(&g);
        assert_eq!(downsample2(&sources), downsample2(&sources));
    }

    #[test]
    fn blocks_fast_path_maps_blocks_to_cells() {
        // A chunk with one solid block at block-index (1,0,1): with the chunk
        // in source slot 0, the coarse cell (1,0,1) of the 4³ corner is solid.
        let mut chunk = game::Chunk::default();
        chunk.set_block(1, 0, 1, Block::new(BlockId(4)));
        let mut chunks: [Option<&game::Chunk>; 8] = [None; 8];
        chunks[0] = Some(&chunk);
        let out = downsample_blocks(&chunks);
        assert_eq!(out[1 + 32 * 0 + 32 * 32 * 1].block, BlockId(4));
        assert_eq!(out.iter().filter(|v| !v.is_air()).count(), 1);
    }
}
```

Run: `cargo test -p client --lib lod:: 2>&1 | tail -3` (after `pub mod lod;`) — Expected: COMPILE ERROR.

- [ ] **Step 2: Implement** — `downsample2` iterates the 32³ output; per cell, resolve the source octant (`x/16` etc., index `sx + 2*sy + 4*sz`), gather the 2×2×2 source voxels (source coords `(x%16)*2 + dx` etc.), count solids per BlockId (a tiny fixed `[(BlockId, u8); 8]` scratch — no HashMap), pick majority/lowest. `downsample_blocks` iterates the 8 source slots × each chunk's 8 blocks via `Chunk::block_at`, writing coarse cells at `(sx*2 + bx, sy*2 + by, sz*2 + bz)`; the rest of the 32³ grid stays air. Doc comments state the LOCKED rule and the layout contract verbatim.

- [ ] **Step 3:** `cargo test -p client --lib lod:: 2>&1 | tail -3` — 6 passed. Full suite tail green.

- [ ] **Step 4: Commit** — `feat(client): LOD downsampling core (opaque-if-any, majority material)`

---

### Task 2: Skirted LOD meshing (y-bias) + pack assert relaxation

**Files:**
- Modify: `crates/client/src/renderer/meshing/pack.rs` (assert `<= 33` + comment)
- Modify: `crates/client/src/renderer/meshing/lod.rs` (add `mesh_lod_grid`)
- Test: unit tests in `lod.rs` + one updated pack.rs test bound

**Interfaces:**
- Produces (used by Tasks 3–5): `pub fn mesh_lod_grid(grid: &[Voxel], neighbors: &BTreeMap<[i32; 3], &[Voxel]>, layers: &BlockTextureLayers) -> Option<Mesh>`:
  1. `gather_padded(grid, neighbors)` (same-region coarse neighbors; missing = air shell).
  2. `binary::mesh_quads(&padded)`.
  3. **Skirt pass:** for each quad with `face.axis() != 1` sitting ON a border plane (NegX at `min[0] == 0`, PosX at `min[0] == 31`, same for Z — i.e. the quad's plane coordinate is at the grid edge), extend it one cell downward: `min[1] -= 1` conceptually. Implemented WITH the bias: first shift ALL quads `min[1] += 1`, then for border side-quads only, `min[1] -= 1; h_or_extent += 1` — careful: for X/Z faces, the vertical direction is the V axis when `cross_axes(axis).1 == 1`... VERIFY against `cross_axes`: X faces have (U=Y, V=Z) so vertical extent is `w` (along U=Y); Z faces have (U=X, V=Y) so vertical is `h`. Extend the correct extent per face axis (X faces: `min[1] -= 1` is min[ua]... the quad's `min` IS a [u32;3] chunk-local corner: subtract on index 1 and grow the extent that maps to Y). Write a `fn skirt_extend(q: &mut Quad)` helper with exhaustive per-face unit tests rather than trusting this prose.
  4. `emit::quads_to_mesh(&quads, layers)`.
- The mesh's y range becomes 0..=33 (bias +1, skirts to 0). Callers compensate: `Transform.translation.y -= factor as f32` and use the biased Aabb (Task 3).

- [ ] **Step 1: Failing tests** — (a) pack.rs: change the domain test's boundary set to include `pos = [33, 33, 33]` (round-trips); (b) lod.rs: `skirted_mesh_shifts_up_and_extends_border_side_faces_down` — a full-solid 32³ grid meshed with air neighbors: every side-face quad at the border must span y 0..=33 (extent 33) while top faces sit at y 33 and bottom at y 1... define exact assertions from unpacked vertices; (c) `interior_side_faces_are_not_skirted` — a lone solid cell at (5,5,5): its side quads span y 6..7 (biased), no y=0 geometry; (d) `skirt_extend` per-face unit table.
- [ ] **Step 2: Implement** — relax the pack.rs debug_assert to `<= 33` with the comment: "33 = 32-grid corner + the LOD skirt's one-cell downward bias (see lod.rs::mesh_lod_grid); the 6-bit field holds 0..=63 and the WGSL decode is unchanged." Implement `mesh_lod_grid` + `skirt_extend` per the interface.
- [ ] **Step 3:** lod:: + pack:: + full suite green.
- [ ] **Step 4: Commit** — `feat(client): skirted LOD meshing via +1 y-bias (pack domain widens to 33)`

---

### Task 3: LOD entity plumbing — components, queue, apply

**Files:**
- Modify: `crates/client/src/renderer/meshing/lod.rs` (components + systems)
- Modify: `crates/client/src/renderer/mod.rs` (register)
- Test: headless tests in `lod.rs`

**Interfaces:**
- Produces (used by Tasks 4–5):
  - `#[derive(Component)] pub struct LodChunk { pub factor: u32 }`
  - `#[derive(Component)] pub struct LodVoxelData(pub Vec<Voxel>)` — 32³ coarse grid per the layout contract.
  - `#[derive(Component)] pub struct LodNeighbors(pub BTreeMap<[i32; 3], Entity>)` — OPTIONAL same-batch neighbor refs for gather (set by the spawner; entities must be LOD siblings).
  - `pub struct LodMeshTask(Task<Option<Mesh>>);` + systems `spawn_lod_mesh_tasks` / `apply_lod_meshes`, mirroring the queue.rs pattern but: plain FIFO over `Changed<LodVoxelData>` (no distance ranking — LOD is inherently low-priority background work), budget = share `MeshBudgets.max_in_flight` MINUS in-flight full-res tasks (full-res always wins; LOD tasks only claim leftover slots), uploads share `uploads_per_frame` the same way (count full-res applies first).
  - `apply_lod_meshes` inserts: `Mesh3d`, shared `ChunkMaterial`, `Aabb { center: Vec3A::new(16.0, 16.5, 16.0), half_extents: Vec3A::new(16.0, 16.5, 16.0) }` (covers the biased 0..=33 y range), `NoAutoAabb`. NO ChunkKey, NO ChunkConnectivity, NO grid entry — assert in tests.
  - Spawner contract (Tasks 4–5 follow it): LOD chunk entities are children of a region root, with `Transform { translation: local_cell_origin - Vec3::Y * factor, scale: Vec3::splat(factor as f32), .. }` and `(LodChunk, LodVoxelData, Visibility::default())`.

- [ ] **Step 1: Failing headless tests** — (a) `lod_chunks_mesh_and_scale_without_entering_the_grid`: spawn a root + LOD chunk (factor 2, a simple solid-floor coarse grid), run the FULL production chain (all queue systems + culling + the new lod systems), settle; assert Mesh3d + scaled Transform + biased Aabb present, AND `ChunkGrid` empty, AND no `ChunkKey`/`ChunkConnectivity` on the entity, AND `Visibility` stays `Inherited` even though the BFS ran (keyless = untouchable). (b) `full_res_tasks_starve_lod_tasks_not_vice_versa`: with `max_in_flight = 1` and both a dirty full-res chunk and a changed LOD chunk pending, the full-res chunk claims the slot first. (c) supersede-on-change parity with the full-res path.
- [ ] **Step 2: Implement** per the interfaces. The task closure: gather neighbor grids via `LodNeighbors` (query sibling `LodVoxelData`, build the `&[Voxel]` map), `mesh_lod_grid(...)`. Register: extend the chained Update pipeline with `(lod::spawn_lod_mesh_tasks, lod::apply_lod_meshes)` AFTER `apply_meshed_chunks` and BEFORE `cull_chunks` (order documented: LOD apply must not eat the upload budget before full-res applies — compute shared budgets accordingly; simplest correct: run lod apply after full-res apply and pass it the count of full-res applies this frame via a small `ResMut<FrameUploadCount>` written by `apply_meshed_chunks`... implement exactly that: a `#[derive(Resource, Default)] pub struct FrameUploads(pub usize)` reset in `mark_remesh_dirty`, incremented by both apply systems, checked against `uploads_per_frame`).
- [ ] **Step 3:** Full suite + include-ignored green; time new tests.
- [ ] **Step 4: Commit** — `feat(client): LOD chunk entity path (keyless, scaled, budget-sharing)`

---

### Task 4: Far-ring cutover to factor-2 LOD

**Files:**
- Modify: `crates/client/src/renderer/bridge.rs` (`spawn_far_region`)
- Test: updated bridge far tests + one new end-to-end

**Interfaces / behavior:**
- `spawn_far_region` no longer spawns full-res `VoxelData` children. Instead it spawns ONE `FarLodBuild(Task<Vec<LodCell>>)` marker on the far root, where the async task (mirror the pool pattern; move the snapshot in): (1) groups `snap.chunks` by 2×2×2 chunk coords (`coords.x/2, coords.y.div_euclid(2), coords.z/2` — y is signed, use div_euclid), (2) `derive_voxels` per source chunk, (3) `downsample2` per group → `LodCell { group: [i32; 3], grid: Vec<Voxel> }`.
- A small `finish_far_lod_builds` system polls the task and spawns the LOD children under the root: local translation `(group.x * 64, group.y * 64 - 2, group.z * 64)` (64 = 32 cells × factor 2; `- 2` = the y-bias compensation, `factor` = 2), `scale: Vec3::splat(2.0)`, `LodChunk { factor: 2 }`, `LodVoxelData(grid)`, plus `LodNeighbors` wired between same-region groups (adjacent `group` keys).
- The existing bridge far tests change expectation: `far_region_roots_at_offset_with_meshable_chunks` now asserts a `FarLodBuild` task exists and, after settling, LOD children with `LodVoxelData` (not `VoxelData`) — same assertion strength, new component names. Remove-far-region test unchanged (root despawn covers children).
- New end-to-end test: a `FarSnapshot` with a flat floor across 2×2×2 chunks settles into LOD children whose meshes exist and whose entities are absent from `ChunkGrid` (the old full-res far path is provably gone).

Steps: TDD as usual; full suite; commit `feat(client): far ring renders as factor-2 LOD (leaves the full-res pipeline)`.

---

### Task 5: Worldgen rings (`renderer/worldgen_rings.rs`)

**Files:**
- Create: `crates/client/src/renderer/worldgen_rings.rs`
- Modify: `crates/client/src/renderer/mod.rs` (register + resource init)
- Test: headless tests in the new module

**Interfaces / behavior (the brief for the implementer):**
- Resource `WorldgenRings { assigned: BTreeMap<RegionCoords, (u32 /*factor*/, Entity /*root*/)>, in_flight: usize, materials: Materials, seed: u64 }` — materials/seed built once like `local_server.rs:40-49` (`worldgen::WORLD_SEED`; comment the seed assumption per Global Constraints deviation 2).
- System `manage_worldgen_rings` (Update, after the bridge drains):
  1. Viewer region from `HudStatus.viewer_region` (None → no-op).
  2. Desired assignment per region within `WORLDGEN_VIEW_RADIUS`: skip regions in the near window or the far ring (`Regions`/`FarRegionRoots` resources are the truth for exclusion — a region owned by either never gets a worldgen ring; if one appears there later, despawn the worldgen root immediately); otherwise factor from `WORLDGEN_RINGS` by Chebyshev distance.
  3. Sticky band: a region already assigned keeps its factor unless its distance is outside the factor's bound by more than `RING_STICKY_BAND` (both directions — promote and demote).
  4. Regions leaving the radius (+ band): despawn root, remove entry.
  5. Regions needing (re)generation, nearest-first, while `in_flight < WORLDGEN_TASKS_MAX_IN_FLIGHT`: spawn `WorldgenTask(Task<...>)` entities carrying `(coords, factor)`; the async task runs `generate_region(coords, seed, materials)`, then per the factor: factor 4 = two `downsample2` passes over the materialized chunk grid; factor 16 = `downsample_blocks` per 2×2×2 group; producing `Vec<LodCell>` like Task 4 (y from `Column::materialized_sub_chunks` absolute sub_y, grouped with div_euclid).
  6. `finish_worldgen_tasks` polls, spawns the region root at `RegionId::world_offset()` + LOD children per Task 3's spawner contract (translation scaled by the factor: cell origin `group * 32 * factor`, minus `factor` on y), records `assigned`, decrements `in_flight`. If the region's assignment changed while generating (stale factor), drop the result.
- Headless tests: (a) ring assignment math incl. sticky band (pure function `ring_factor(dist, current: Option<u32>) -> Option<u32>` extracted and unit-tested exhaustively over distances 0..=10 with and without current); (b) exclusion: regions in near/far sets never assigned; (c) a full pipeline test with a mocked-small radius (override the resource's table? make the table a field of `WorldgenRings` defaulting to the constants so tests can shrink it) generating one region and settling to meshed LOD children (this test WILL run real worldgen for one region — time it; slow-tag if >100ms, likely yes).
- Register after bridge systems in Update (not in the meshing chain).

Steps: TDD; full suite; commit `feat(client): client-side worldgen LOD rings (factors 4 and 16, sticky band)`.

---

### Task 6: Integration hardening + gates + docs

**Files:**
- Modify: `crates/client/src/renderer/meshing/visibility.rs` (CULL_BFS comment update per Global Constraints)
- Modify: `CLAUDE.md` (client bullet: add `LOD rings` to the meshing parenthetical; worldgen bullet: note the client renders worldgen LOD rings from the same pure function)
- Test: the view-distance smoke + full gates

- [ ] **Step 1:** Update the `CULL_BFS_MAX_CHEBYSHEV` comment: grid now spans only the near 3×3 window; LOD chunks are keyless and never culled; 64 overshoots by >2×.
- [ ] **Step 2:** View-distance headless smoke: spawn the full app chain with a viewer region set; assert that after settling, LOD roots exist for worldgen-ring regions and their total mesh count is > 0 (shrunk table per Task 5's test hook).
- [ ] **Step 3:** CLAUDE.md edits.
- [ ] **Step 4:** Full gates, synchronously: `just test`, `just test-full`, `cargo test -p server`, `cargo test -p client --release --test mesh_parity -- --include-ignored`. Known-flake protocol: isolate + re-run once, report both.
- [ ] **Step 5:** Commit — `docs+test(client): phase 4 integration gates; culling comment reflects LOD-era grid extent`

---

## Self-Review Notes (already applied)

- Spec §8.1 untouched near window ✓ (no near-path edits anywhere). §8.2 → Tasks 1+4 (streamed, factor 2, edits flow because refreshes re-downsample authoritative snapshots). §8.3 → Tasks 1+5 (worldgen, factors 4/16, async + budget, seed assumption documented). §8.4 → Task 2 skirts + Task 5 sticky band; the ≤1-level invariant relaxation is Global-Constraints deviation 1 with its trigger condition. §8.5 → Task 3's budget-sharing (full-res always wins — stronger than "lower priority"). §8.6 → determinism tests (T1), skirt tests (T2), hysteresis tests (T5a), view-distance smoke (T6).
- The LOD path's isolation from the full-res pipeline is asserted, not assumed (T3a, T4's grid-absence assertion).
- Type consistency: `downsample2`/`downsample_blocks`/`mesh_lod_grid`/`LodChunk`/`LodVoxelData`/`LodNeighbors`/`LodCell`/`FrameUploads` names bind Tasks 1→5; the spawner contract (translation/scale/bias) is stated once in Task 3 and referenced by 4/5.
- Known risks flagged in-plan: `skirt_extend`'s per-face extent mapping (T2 demands exhaustive per-face tests over trusting prose); signed-y grouping (div_euclid, T4/T5); stale-generation drop (T5.6); worldgen test runtime (slow-tag); the factor-16 grid's wasted volume (documented, v1 simplicity).
- NOT in this plan (recorded): incremental far-region diff (deviation 3), worldgen-output cache (deviation 4), 8× intermediate ring (deviation 1), seed handshake (deviation 2), far-field beyond Chebyshev 8 (skybox/heightmap — future).
