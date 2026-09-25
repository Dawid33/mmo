# Voxel Raster LOD Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce portable raster LOD geometry and shading work through global equal-factor neighbors, exact emitted bounds, factor-aware shading, and native-only face-direction indirect commands.

**Architecture:** LOD entities receive stable world-cell keys and are linked through a main-world `BTreeMap`; meshing produces conservative bounds, tiered records, and six contiguous face ranges in one result. Existing `ChunkQuads` extraction carries the derived metadata to the shared GPU arena. Native compute culling emits per-face commands, while grouped-direct/browser paths retain whole-chunk draws.

**Tech Stack:** Rust, Bevy ECS/render extraction, existing binary greedy mesher and 8-byte `QuadRecord`, WGSL compute/vertex/fragment shaders, wgpu indirect drawing.

## Global Constraints

- Do not change `game::lod`, server payloads, LOD factors/rings, or the locked 8-byte `QuadRecord` layout.
- Exact equal-factor neighbors only; uncertainty and missing data are air/overdraw, never holes.
- Face order is the existing locked `Face::index()` order everywhere.
- Stages 1–3 compile on wasm32; face buckets activate only for native GPU-driven rendering.
- Keep each stage behind a renderer toggle until its benchmark gate is recorded.
- The approved design is `docs/superpowers/specs/2026-07-16-voxel-raster-lod-optimization-design.md`.

---

### Task 0: Independent stage switches

**Files:**
- Modify: `crates/client/src/renderer/config.rs`
- Modify: `crates/client/src/renderer/mod.rs`
- Modify: `crates/client/src/renderer/render_bench.rs`

**Interfaces:**

```rust
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct RasterLodOptimizationConfig {
    pub global_neighbors: bool,
    pub tight_bounds: bool,
    pub factor_shading: bool,
    pub face_buckets: bool,
}
```

- [ ] Add parser tests for `VOXEL_RASTER_LOD_STAGES=none|neighbors|bounds|shading|face-buckets|all` and comma-separated combinations, including duplicate normalization, unknown-stage rejection, and wasm forcing `face_buckets=false`.
- [ ] Run `cargo test -p client raster_lod_stage_config -- --nocapture`; expect failure because the resource does not exist.
- [ ] Resolve the resource at startup. Keep every switch false initially; each later task must branch at its ownership boundary and preserve the existing path when disabled.
- [ ] Serialize the final four booleans in benchmark metadata so stage comparisons are unambiguous.
- [ ] Run `cargo test -p client raster_lod_stage_config`; expect success.
- [ ] Commit with `git commit -m "feat(client): add raster LOD optimization stage switches"`.

### Task 1: Stable LOD cell keys

**Files:**
- Modify: `crates/client/src/renderer/meshing/lod.rs`
- Modify: `crates/client/src/renderer/lod_rings.rs`

**Interfaces:**

```rust
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LodCellKey {
    pub factor: u32,
    pub cell: [i32; 3],
}

pub fn lod_cell_key(key: LodKey, payload_group: [i32; 3]) -> LodCellKey;
```

- [ ] Write tests using positive and negative payload group coordinates proving adjacent cells from distinct payloads produce keys differing by exactly one axis, and different factors never compare equal.
- [ ] Run `cargo test -p client lod_cell_key -- --nocapture`; expect failure because the key helper does not exist.
- [ ] Compute `cell = key.world_offset() / (32 * factor) + payload_group` with checked, exact integer arithmetic from the same coverage values used by `spawn_lod_root`; do not derive identity from floating transforms.
- [ ] Attach `LodCellKey` to every raster LOD entity and preserve it across voxel-data refreshes at the same factor/coverage.
- [ ] Run the focused tests; expect success.
- [ ] Commit with `git commit -m "feat(client): assign stable raster LOD cell keys"`.

### Task 2: Global equal-factor neighbor index and dirty propagation

**Files:**
- Modify: `crates/client/src/renderer/meshing/lod.rs`
- Modify: `crates/client/src/renderer/lod_rings.rs`
- Modify: `crates/client/src/renderer/mod.rs`

**Interfaces:**

```rust
#[derive(Resource, Default)]
pub struct LodCellIndex(pub BTreeMap<LodCellKey, Entity>);

pub fn maintain_lod_cell_index(/* Added/Changed/Removed key queries */);
```

- [ ] Write ECS tests for: cross-payload face neighbors suppressing the shared plane; removal restoring it; edge/corner arrivals changing AO; equal-factor-only lookup; and an unchanged refresh reaching a fixed point without repeated remesh requests.
- [ ] Run `cargo test -p client lod_neighbor_index -- --nocapture`; expect failures against the same-batch implementation.
- [ ] Insert/replace/remove keys in deterministic `BTreeMap` order. For each changed key, visit all 26 offsets and rebuild `LodNeighbors` for the changed cell and surviving exact-factor neighbors.
- [ ] On relationship change, enqueue each affected entity once in the existing dirty/remesh queue. On removal, dirty survivors before deleting the index entry. Compare the rebuilt snapshot with the existing one before marking dirty to prevent loops.
- [ ] Delete the same-batch-only wiring in `spawn_lod_root`; keep `LodNeighbors` as the task snapshot consumed by meshing.
- [ ] Add retirement/despawn cleanup and a debug assertion that index entries point to matching live keys.
- [ ] Run the focused tests and `just check`; expect success.
- [ ] Commit with `git commit -m "feat(client): resolve LOD neighbors through a global index"`.

### Task 3: Conservative bounds from final emitted quads

**Files:**
- Modify: `crates/client/src/renderer/meshing/binary.rs`
- Modify: `crates/client/src/renderer/meshing/mod.rs`
- Modify: `crates/client/src/renderer/meshing/lod.rs`
- Modify: `crates/client/src/renderer/voxel_pull/mod.rs`

**Interfaces:**

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuadBounds { pub min: Vec3, pub max: Vec3 }

impl QuadBounds {
    pub fn from_quads(quads: &[Quad], y_bias: f32, skirt_depth: f32) -> Option<Self>;
    pub fn to_aabb(self) -> Option<Aabb>;
}
```

- [ ] Add tests that decode every record corner and assert containment for all six faces, negative world origins after transform, factors 2/16/256, biased top faces, extended skirts, and empty output. Add an invalid/non-finite bounds test that selects the legacy constant AABB.
- [ ] Run `cargo test -p client quad_bounds -- --nocapture`; expect failure before `MeshOutput`/LOD task output carries bounds.
- [ ] Compute bounds after all face generation, bias, and skirt mutation. Return `None` for no quads; validate finite ordered axes before building `Aabb`.
- [ ] Add `bounds: Option<QuadBounds>` to mesh/record task output. Attach the same exact AABB to either `Mesh3d` or `ChunkQuads`; omit the terrain component for empty output.
- [ ] Keep the existing 32×33×32 AABB as a debug-warn-once fallback for invalid derived bounds.
- [ ] Run the focused tests, `cargo test -p client meshing`, and `cargo check -p client --target wasm32-unknown-unknown`; expect success.
- [ ] Commit with `git commit -m "perf(client): cull raster LOD with emitted bounds"`.

### Task 4: Factor-aware meshing policy

**Files:**
- Modify: `crates/client/src/renderer/meshing/binary.rs`
- Modify: `crates/client/src/renderer/meshing/lod.rs`
- Modify: `crates/client/src/renderer/meshing/records.rs`

**Interfaces:**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LodShadingTier { DetailedAo, DetailedFlat, AverageFlat }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LodMeshingPolicy { pub tier: LodShadingTier }

impl LodMeshingPolicy { pub const fn for_factor(factor: u32) -> Self; }
```

- [ ] Write policy tests locking 1/2/4 to `DetailedAo`, 16 to `DetailedFlat`, and 64/256 to `AverageFlat`.
- [ ] Add a mesher regression fixture where AO differences split factor-4 quads but factor-16 canonical `[3; 4]` values merge them. Assert factor selection does not change `size_of::<QuadRecord>() == 8`.
- [ ] Run `cargo test -p client lod_meshing_policy -- --nocapture`; expect failure before policy injection.
- [ ] Pass policy only into the LOD meshing entry point. For flat tiers, canonicalize AO before it enters the greedy merge key and record packing; leave the full-resolution entry point unchanged.
- [ ] Carry the tier/factor in the existing per-chunk transform metadata rather than record bits. Add a test proving refresh at a changed factor selects a new policy.
- [ ] Run focused tests and `cargo test -p client meshing`; expect success.
- [ ] Commit with `git commit -m "perf(client): simplify distant LOD ambient occlusion"`.

### Task 5: Mipmapped block array and average material-face colors

**Files:**
- Modify: `crates/client/src/renderer/mod.rs`
- Modify: `crates/client/src/renderer/block_textures.rs`
- Modify: `crates/client/src/renderer/voxel_pull/pipeline.rs`
- Modify: `assets/shaders/voxel_pull.wgsl`

**Interfaces:**

```rust
#[derive(Resource, Clone)]
pub struct MaterialFaceAverages {
    pub colors: Vec<[f32; 4]>, // block_id * 6 + Face::index()
}
```

- [ ] Add image tests for deterministic RGBA linear-space averages, transparent texels, face-layer mapping, mip dimensions down to 1×1, and stable lookup index order.
- [ ] Add shader/Rust layout tests for the average-color storage buffer and tier values.
- [ ] Run `cargo test -p client material_face_average -- --nocapture`; expect failure.
- [ ] Generate CPU mip levels for every array layer during current texture loading and set the image mip count/data consistently. Compute one linear average per material/face from the source layer.
- [ ] Upload the average table once and bind it in pulled forward rendering. Derive factor/tier from chunk transform scale; Detailed tiers sample the mipmapped array, AverageFlat reads the lookup. Keep depth/shadow shaders free of the new color binding.
- [ ] If the lookup resource is absent, specialize/use DetailedFlat and emit one warning; never omit terrain.
- [ ] Run tests, `just check`, and wasm check; expect success.
- [ ] Commit with `git commit -m "perf(client): shade outer LOD from average material colors"`.

### Task 6: Six stable face ranges per chunk

**Files:**
- Modify: `crates/client/src/renderer/meshing/records.rs`
- Modify: `crates/client/src/renderer/meshing/mod.rs`
- Modify: `crates/client/src/renderer/meshing/lod.rs`
- Modify: `crates/client/src/renderer/voxel_pull/mod.rs`
- Modify: `crates/client/src/renderer/voxel_pull/arena.rs`

**Interfaces:**

```rust
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkFaceRanges(pub [FaceRange; 6]);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FaceRange { pub record_start: u32, pub quad_count: u32 }
```

- [ ] Write tests that shuffle mixed-face quads, build records/ranges, and prove every record appears exactly once, each range contains one face, empty faces have zero count, and all `start + count` values fit the allocation.
- [ ] Run `cargo test -p client face_ranges -- --nocapture`; expect failure.
- [ ] Stable-bucket quads by `Face::index()` before record packing. Return records and relative ranges together so they cannot diverge.
- [ ] Extract ranges with `ChunkQuads` and store six relative ranges in `GpuNearChunkMeta`. Validate against the arena allocation at upload/update time.
- [ ] Keep grouped-direct and per-chunk queue code unchanged except for accepting/ignoring the component.
- [ ] Run focused tests and `just check`; expect success.
- [ ] Commit with `git commit -m "feat(client): attach face ranges to pulled terrain"`.

### Task 7: Native GPU face selection and indirect command emission

**Files:**
- Modify: `crates/client/src/renderer/voxel_pull/gpu.rs`
- Modify: `crates/client/src/renderer/voxel_pull/queue.rs`
- Modify: `assets/shaders/voxel_pull_cull.wgsl`
- Modify: `crates/client/src/renderer/render_bench.rs`

- [ ] Add CPU-oracle tests for camera and directional-light face masks. Cover camera on both sides of every axis, inside each AABB slab (both orientations retained), boundary equality, and conservative shadow direction selection.
- [ ] Add command tests for compact indirect-count mode and fixed mode: six slots per stable chunk slot, rejected/empty ranges write zero vertices, accepted commands use `record_start * 6` and `quad_count * 6`, and no command exceeds record/transform/command capacities.
- [ ] Run `cargo test -p client face_culling -- --nocapture`; expect failure.
- [ ] Extend the cull uniform with view kind/direction and metadata with six ranges. Mirror the tested CPU oracle exactly in WGSL.
- [ ] In count mode atomically compact only surviving ranges. In fixed mode write all six stable slots every frame and zero rejected commands. On any validation uncertainty, emit the whole-chunk equivalent range or retain both faces.
- [ ] Do not enable this path on wasm/grouped-direct. Add accepted/rejected face-range counters to benchmark snapshots.
- [ ] Run focused tests, `just check`, wasm check, and shader smoke through `just render-bench near=gpu far=raster quality=current` for one shortened development run.
- [ ] Commit with `git commit -m "perf(client): cull pulled terrain by face direction"`.

### Task 8: Stage gates and final verification

**Files:**
- Create: `docs/benchmarks/2026-07-16-raster-lod-optimization.md`

- [ ] Run `just fmt`, `just fmt-check`, `just clippy`, `just test`, wasm check, and `just test-full`; all must exit 0.
- [ ] Run three production repetitions for baseline and after each independently toggled stage. Record quad counts by factor, active cells, frustum/HZB ratios, face rejection, p50/p95 GPU frame, pass timings, and reserved memory.
- [ ] Require ≥5% median p50 improvement, ≤5% p95 regression, no unexplained memory increase, and clean seam/rotation/ring-crossing probes before enabling a stage by default.
- [ ] For neighbor indexing only, document the approved exception if GPU time is neutral but shared-plane quad counts fall or a visible seam is fixed.
- [ ] Confirm missing neighbors, invalid bounds, lookup absence, unsupported GPU features, and invalid ranges all select conservative raster overdraw.
- [ ] Commit with `git commit -m "docs: record raster LOD optimization gates"`.
