# Voxel Hybrid Brick Renderer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Redefine native `brick` mode as a safe hybrid: raster covers factors 2/4/16 and every fallback, while factors 64/256 may atomically transition to a cheaper bounded brick pass and optional half-resolution composite.

**Architecture:** Admission and coverage state live in main-world far-cell lifecycle code. Raster and brick build queues coexist; revision-tagged readiness events drive a state machine that never retires the last valid coverage. Brick preparation emits occupied bounds and shading tier metadata. The render world uses those bounds for proxies, then optionally raymarches into a half-resolution HDR/depth target and composites it into the full-resolution main view.

**Tech Stack:** Rust, Bevy ECS/render graph 0.18.1, wgpu storage/indirect resources, WGSL raymarch and composite shaders, existing raster LOD and brick residency systems.

## Global Constraints

- This plan depends on the quality/benchmark plan and raster plan's mipmapped texture array plus `MaterialFaceAverages`.
- wasm32 and unsupported adapters always use raster; keep `TerrainRenderCapabilities::compiled().brick_raymarch` false through this plan.
- Coverage invariants outrank performance: an admitted live cell always has raster or draw-ready brick coverage.
- Stale revisions cannot mutate current coverage or residency ownership.
- Use stable cell order for deterministic budget fallback; renderer state remains outside rollback/hashing.
- The approved design is `docs/superpowers/specs/2026-07-16-voxel-hybrid-brick-renderer-design.md`.

---

### Task 0: Independent hybrid experiment switches

**Files:**
- Modify: `crates/client/src/renderer/config.rs`
- Modify: `crates/client/src/renderer/render_bench.rs`

**Interfaces:**

```rust
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct HybridBrickExperimentConfig {
    pub occupied_bounds: bool,
    pub cheap_shading: bool,
}
```

- [ ] Add native parser tests for `VOXEL_BRICK_OCCUPIED_BOUNDS=0|1` and `VOXEL_BRICK_CHEAP_SHADING=0|1`, precise invalid-value errors, defaults false while brick remains experimental, and wasm forcing both false.
- [ ] Run `cargo test -p client hybrid_brick_experiment_config -- --nocapture`; expect failure because the resource does not exist.
- [ ] Resolve the resource at startup and serialize it in benchmark metadata. Tasks 3 and 4 must retain full-cell bounds/current shading when their switch is false. Half resolution remains independently controlled by `VOXEL_FAR_SCALE` in Task 5.
- [ ] Run `cargo test -p client hybrid_brick_experiment_config`; expect success.
- [ ] Commit with `git commit -m "feat(client): add hybrid brick experiment switches"`.

### Task 1: Factor/capability admission policy

**Files:**
- Modify: `crates/client/src/renderer/config.rs`
- Modify: `crates/client/src/renderer/far_voxel/lifecycle.rs`
- Modify: `crates/client/src/renderer/meshing/lod.rs`

**Interfaces:**

```rust
pub const BRICK_MIN_FACTOR: u32 = 64;

pub const fn brick_admitted(factor: u32) -> bool {
    factor >= BRICK_MIN_FACTOR
}

pub fn brick_admitted_for(
    factor: u32,
    backend: FarRenderBackend,
    capabilities: TerrainRenderCapabilities,
    is_wasm: bool,
) -> bool;
```

- [ ] Write table tests proving 2/4/16 remain raster, 64/256 are eligible only for `brick` on capable native devices, and wasm/missing capability/raster backend reject every factor.
- [ ] Run `cargo test -p client brick_admission -- --nocapture`; expect failure.
- [ ] Apply admission at the LOD build-routing point: enqueue raster meshing for ineligible cells and brick preparation for eligible cells without blocking near/full-resolution priority work.
- [ ] Preserve the public `VOXEL_FAR_RENDER=brick` spelling and leave automatic capability selection false.
- [ ] Run focused tests and wasm check; expect success.
- [ ] Commit with `git commit -m "feat(client): gate brick rendering by distant LOD factor"`.

### Task 2: Explicit coverage state machine

**Files:**
- Modify: `crates/client/src/renderer/far_voxel/lifecycle.rs`
- Modify: `crates/client/src/renderer/far_voxel/residency.rs`
- Modify: `crates/client/src/renderer/lod_rings.rs`

**Interfaces:**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrickTransition {
    Building { revision: u64 },
    UploadPending { revision: u64 },
    ResidentPassPending { revision: u64 },
    DrawReady { revision: u64, overlap_frame: u64 },
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FarCoverageState {
    pub desired_revision: u64,
    pub raster_active: bool,
    pub brick_draw_revision: Option<u64>,
    pub transition: Option<BrickTransition>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FarFallbackReason {
    Unsupported, Capacity, UploadRejected, PipelineUnavailable,
    StaleRevision, Timeout, InvalidBounds, CompositeUnavailable,
}
```

- [ ] Write a pure transition-table test covering every legal event from every state. Assert every live post-state has `raster_active || brick_draw_revision.is_some()`, stale revisions are no-ops with a reason, old brick coverage can coexist with a newer transition, and raster/brick double coverage lasts at most one completed frame.
- [ ] Add ECS tests for refresh during build/upload/residency, removal in every state, capacity exhaustion, pass-not-ready, and timeout. Each must retain or restore raster coverage.
- [ ] Run `cargo test -p client far_coverage_state -- --nocapture`; expect failure against implicit marker components.
- [ ] Centralize transitions in one pure reducer and make lifecycle systems apply its commands. Keep raster entity/components until a matching revision reports render-world draw readiness, then retire after one completed overlap frame.
- [ ] On refresh, continue displaying the last valid raster or brick revision until the new revision is ready. Removal frees both resource families together.
- [ ] Record transition/fallback counters in a resource drained by benchmark capture.
- [ ] Run focused tests and `just check`; expect success.
- [ ] Commit with `git commit -m "feat(client): make raster brick handoff coverage-safe"`.

### Task 3: Occupied bounds in CPU preparation and GPU cells

**Files:**
- Modify: `crates/client/src/renderer/far_voxel/brick.rs`
- Modify: `crates/client/src/renderer/far_voxel/residency.rs`
- Modify: `crates/client/src/renderer/far_voxel/gpu.rs`
- Modify: `crates/client/src/renderer/far_voxel/pass.rs`

**Interfaces:**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OccupiedBounds {
    pub min: [u8; 3],
    pub max_exclusive: [u8; 3],
}
```

- [ ] Add fixtures for all-air roots, one boundary voxel, uniform occupied bricks, mixed bricks, disjoint occupancy, and deduplicated repeated bricks. Assert bounds contain every non-air voxel and exclude empty outer margins.
- [ ] Add transform tests for factors 64/256 and signed cell origins. Assert proxy/world AABBs remain conservative and camera-inside cases stay visible.
- [ ] Run `cargo test -p client occupied_bounds -- --nocapture`; expect failure.
- [ ] Accumulate voxel-local min/max during `BrickDag` preparation independent of dedup IDs. Store `None` for all-air; skip visible-cell allocation for it.
- [ ] Extend `GpuFarCell` with occupied local min/max and factor while preserving alignment with a compile-time size/offset test. Build frustum/proxy geometry from occupied bounds, but keep DDA addressing in original 0..32 cell coordinates.
- [ ] Validate bounds before upload; invalid data selects full-cell bounds and records `InvalidBounds` rather than removing coverage.
- [ ] Add occupied/full volume ratio to benchmark metrics.
- [ ] Run focused tests, `just check`, and one full-resolution brick smoke run; expect success and no missing proxies.
- [ ] Commit with `git commit -m "perf(client): bound brick proxies to occupied voxels"`.

### Task 4: Cheap factor-aware brick shading

**Files:**
- Modify: `crates/client/src/renderer/far_voxel/pipeline.rs`
- Modify: `crates/client/src/renderer/far_voxel/pass.rs`
- Modify: `assets/shaders/far_voxel.wgsl`
- Modify: `crates/client/src/renderer/render_bench.rs`

- [ ] Extract a testable shading-tier function and assert factor 64 selects mipmapped texture, factor 256 selects material-face average, and no admitted tier enables AO or cascaded shadows.
- [ ] Add shader-source/layout tests proving the twelve `sample_same_factor` AO calls are absent from the production hit-shading path and `GpuFarCell.factor` selects the material source.
- [ ] Run `cargo test -p client brick_shading_tier -- --nocapture`; expect failure.
- [ ] Remove reconstructed corner AO from admitted brick hit shading. Retain face/material selection, ambient term, and one directional Lambert term; do not bind/sample cascade shadows.
- [ ] Bind the raster phase's mipmapped texture array and `MaterialFaceAverages`. At factor 64 sample texture with derivatives/explicit LOD suitable for the factor; at factor 256 index the average table by material and face.
- [ ] If the average lookup is unavailable, use the texture tier and increment its fallback counter.
- [ ] Report cells rendered by each shading tier and assert brick AO-neighbor-sample metric is zero.
- [ ] Run focused tests, `just check`, and shader smoke; expect success.
- [ ] Commit with `git commit -m "perf(client): simplify distant brick shading"`.

### Task 5: Native far-scale configuration

**Files:**
- Modify: `crates/client/src/renderer/config.rs`
- Modify: `crates/client/src/renderer/render_bench.rs`

**Interfaces:**

```rust
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct FarRenderQuality { pub scale: f32 }
```

- [ ] Add parser tests accepting exactly `1.0` and `0.5`, rejecting every other value with `VOXEL_FAR_SCALE` named, ignoring environment on wasm, and defaulting brick experiments to 1.0.
- [ ] Run `cargo test -p client far_render_scale -- --nocapture`; expect failure.
- [ ] Resolve `VOXEL_FAR_SCALE` during the existing quality configuration phase. Serialize its resolved value and actual far-pass extent into benchmark metadata.
- [ ] Keep scale 1.0 when reduced-resolution resources/pipeline are unavailable and record `CompositeUnavailable`.
- [ ] Run focused tests and wasm check; expect success.
- [ ] Commit with `git commit -m "feat(client): configure native brick render scale"`.

### Task 6: Half-resolution far color/depth resources

**Files:**
- Modify: `crates/client/src/renderer/far_voxel/pass.rs`
- Modify: `crates/client/src/renderer/far_voxel/pipeline.rs`
- Modify: `assets/shaders/far_voxel.wgsl`

**Interfaces:**

```rust
pub struct FarViewTargets {
    pub extent: UVec2,
    pub color: TextureView,
    pub hit_depth: TextureView,
}

pub fn conservative_near_depth(samples: [f32; 4]) -> f32;
```

- [ ] Add reverse-Z tests proving conservative 2×2 reduction chooses the nearest occluder (largest reverse-Z value), handles clear-depth samples, and maps odd full-resolution edge pixels to valid far texels.
- [ ] Run `cargo test -p client far_depth_reduction -- --nocapture`; expect failure.
- [ ] Allocate/reuse per-view HDR color and `R32Float` hit-depth textures at `ceil(full_extent * scale)`, recreating only on extent/scale changes. Store both in `FarViewTargets`.
- [ ] At scale 0.5, make each ray conservatively reduce the corresponding full-resolution near-depth 2×2 footprint and terminate before the nearest near hit. Write transparent/no-hit plus clear depth when no far voxel is hit.
- [ ] At scale 1.0, retain the existing direct full-resolution behavior until the composite task is ready; do not expose an uncomposited half-resolution target.
- [ ] Add GPU resource bytes and far extent to metrics.
- [ ] Run focused tests and a render-graph smoke run; expect valid resources and no validation errors.
- [ ] Commit with `git commit -m "feat(client): render brick terrain into scaled far targets"`.

### Task 7: Depth-aware full-resolution composite

**Files:**
- Create: `assets/shaders/far_voxel_composite.wgsl`
- Modify: `crates/client/src/renderer/far_voxel/pipeline.rs`
- Modify: `crates/client/src/renderer/far_voxel/pass.rs`
- Modify: `crates/client/src/renderer/far_voxel/mod.rs`

- [ ] Implement a CPU reference for 2×2 candidate selection/weighting. Add tests for flat depth, a near silhouette edge, invalid far samples, reverse-Z reconstruction, and no far-color bleed across a near-depth discontinuity.
- [ ] Run `cargo test -p client far_composite -- --nocapture`; expect failure.
- [ ] Add a render-graph node after far raymarch and before downstream main-view consumers. Bind far color/depth plus full-resolution near depth, and render a full-screen triangle into the main HDR target with depth writes enabled.
- [ ] Mirror the CPU reference: reject candidates crossing the local near-depth edge, choose/weight the closest depth-continuous far samples, write color and `frag_depth` only for valid coverage, otherwise discard.
- [ ] Gate raster retirement on both brick pass and composite pipeline readiness when scale is 0.5. Pipeline failure therefore leaves raster active.
- [ ] Add a small offscreen integration probe with a foreground near rectangle and background far voxels; assert edge pixels contain no far color and valid background pixels contain far depth/color.
- [ ] Run focused tests and native smoke probes at 1.0 and 0.5; expect identical coverage and no depth halos.
- [ ] Commit with `git commit -m "feat(client): composite half-resolution brick terrain"`.

### Task 8: Hybrid metrics, fallback accounting, and deterministic budget behavior

**Files:**
- Modify: `crates/client/src/renderer/far_voxel/lifecycle.rs`
- Modify: `crates/client/src/renderer/far_voxel/residency.rs`
- Modify: `crates/client/src/renderer/render_bench.rs`

- [ ] Add serialization tests for admitted/fallback counts keyed by factor, visible proxies, occupied ratio, far resolution, shading tiers, handoffs, every fallback-reason variant, roots/bricks/bytes, and far-pass p50/p95/p99. Missing GPU readback must be nullable/unavailable.
- [ ] Add a capacity fixture with cells inserted in different event orders and assert the same stable `LodCellKey` ordering is admitted under `FAR_STORAGE_BUDGET_BYTES`.
- [ ] Run `cargo test -p client hybrid_brick_metrics -- --nocapture`; expect failure.
- [ ] Aggregate main-world lifecycle counters and asynchronous render-world metrics without blocking. Keep current resident/referenced/unique brick fields backward compatible.
- [ ] Sort admission candidates by stable key before budget allocation. Capacity losers remain raster and increment `Capacity` by factor.
- [ ] Reset per-run counters only at benchmark start; do not make interactive correctness depend on metrics availability.
- [ ] Run focused tests and `just check`; expect success.
- [ ] Commit with `git commit -m "feat(client): report hybrid brick coverage and fallbacks"`.

### Task 9: Strict rollout verification

**Files:**
- Create: `docs/benchmarks/2026-07-16-hybrid-brick-renderer.md`

- [ ] Run `just fmt`, `just fmt-check`, `just clippy`, `just test`, wasm check, and `just test-full`; all must exit 0.
- [ ] Probe camera-inside-cell, fast rotation, factor boundary/ring crossing, refresh during upload, forced capacity exhaustion, and composite pipeline suppression. Confirm every frame retains coverage and double coverage never exceeds one completed frame.
- [ ] Run three production repetitions for raster, hybrid at far scale 1.0, and hybrid at 0.5 on every designated native adapter. Record GPU frame and far-pass p50/p95/p99, tail spikes, factor counts, occupied ratio, roots/bricks/bytes, fallback reasons, and visual findings.
- [ ] Keep brick opt-in unless every adapter shows hybrid median GPU p50 no slower than raster, p95 no more than 10% slower, no current-style far-pass tail spike, memory within `FAR_STORAGE_BUDGET_BYTES`, and no gaps/halos/seams.
- [ ] Keep `TerrainRenderCapabilities::compiled().brick_raymarch` false even after a successful local adapter run; automatic enablement requires separately recorded all-adapter approval.
- [ ] Commit with `git commit -m "docs: record hybrid brick renderer rollout gates"`.
