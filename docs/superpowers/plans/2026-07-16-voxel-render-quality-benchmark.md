# Voxel Render Quality and Benchmark Controls Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add one startup quality configuration shared by interactive and benchmark rendering, then make benchmark JSON identify the exact quality and terrain workload being measured.

**Architecture:** `RenderQualityConfig` is resolved once beside `TerrainRenderConfig`. Main-world camera/light setup consumes it, render-world voxel pipelines receive the resolved shadow key through extracted view state, and renderer-owned snapshots feed nullable benchmark fields. The `just render-bench` recipe maps named presets to environment defaults while explicit environment values remain authoritative.

**Tech Stack:** Rust, Bevy 0.18.1 camera/light/AA APIs, WGSL shader definitions, serde JSON, root `justfile`.

## Global Constraints

- Preserve native defaults exactly: SMAA High, Gaussian shadows, four cascades, 2048 shadow map, render scale 1.0.
- wasm32 reads no process environment and keeps its current portable AA behavior and scale 1.0.
- Quality state is renderer-only: do not touch simulation, rollback, packets, or server code.
- Missing renderer metrics serialize as `null` and are named in `unavailable_metrics`; zero is reserved for measured zero.
- Use `MainPassResolutionOverride(UVec2)` for the 3D main-pass extent; do not shrink the window or UI target.
- Any test exceeding 100 ms must use `#[ignore = "slow: >100ms — run via just test-full"]`.
- The approved design is `docs/superpowers/specs/2026-07-16-voxel-render-quality-benchmark-design.md`.

---

### Task 1: Pure quality configuration and validation

**Files:**
- Modify: `crates/client/src/renderer/config.rs`
- Modify: `crates/client/src/renderer/mod.rs`

**Interfaces:**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum VoxelAaMode { Off, Fxaa, SmaaLow, SmaaMedium, SmaaHigh }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum VoxelShadowFilter { Hardware2x2, Gaussian }

#[derive(Resource, Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RenderQualityConfig {
    pub aa: VoxelAaMode,
    pub shadow_filter: VoxelShadowFilter,
    pub shadow_cascades: u8,
    pub shadow_map_size: u32,
    pub render_scale: f32,
}
```

- [ ] Write table-driven tests for every documented value, every default, wasm override suppression, both scale endpoints, out-of-range/NaN scale, and errors that name the variable and allowed domain.
- [ ] Run `cargo test -p client renderer::config::tests -- --nocapture`; expect the new tests to fail because the types and parser do not exist.
- [ ] Implement `RenderQualityConfig::resolve(aa, filter, cascades, map_size, scale, is_wasm)` and native-only `from_env()`. Parse scale with `is_finite()` and inclusive `0.50..=1.00`; accept cascade/map values only from the specified sets.
- [ ] Initialize the resource in `RendererPlugin::build` immediately after `TerrainRenderConfig`, so invalid input fails before camera/light setup.
- [ ] Run the focused test again; expect all configuration tests to pass.
- [ ] Commit with `git commit -m "feat(client): add validated render quality configuration"`.

### Task 2: Apply AA and fixed internal resolution to cameras

**Files:**
- Modify: `crates/client/src/renderer/config.rs`
- Modify: `crates/client/src/renderer/mod.rs`
- Modify: `crates/client/src/main.rs`

**Interfaces:**

```rust
pub fn configure_render_quality(
    mut commands: Commands,
    quality: Res<RenderQualityConfig>,
    cameras: Query<(Entity, &Camera), With<Camera3d>>,
);

fn scaled_extent(output: UVec2, scale: f32) -> UVec2;
```

- [ ] Replace the old native SMAA test with app-world tests covering all five modes. Each case must assert `Msaa::Off`, mutual exclusion of `Fxaa`/`Smaa`, and exact `SmaaPreset`.
- [ ] Add pure `scaled_extent` tests for 1280×720 at 1.0 and 0.75, odd dimensions, and a minimum of one pixel per axis.
- [ ] Run `cargo test -p client render_quality -- --nocapture`; expect failures before implementation.
- [ ] Replace `configure_native_antialiasing` with `configure_render_quality`. Remove `Fxaa`, `Smaa`, and stale `MainPassResolutionOverride` first; insert only the selected AA and insert the override only when scale is below 1.0.
- [ ] Derive the override from `Camera::physical_target_size()`, rounded to nearest integer and clamped to at least `UVec2::ONE`. Recompute only when the output extent changes. Keep UI cameras unmodified by the `With<Camera3d>` filter.
- [ ] Register the system in the existing renderer startup/update schedule and keep wasm behavior unchanged with `cfg`-scoped environment access rather than cfg-scoping the resource.
- [ ] Run `cargo test -p client render_quality` and `cargo check -p client --target wasm32-unknown-unknown`; expect success.
- [ ] Commit with `git commit -m "feat(client): apply startup AA and render scale"`.

### Task 3: Keep pulled-terrain and Bevy shadow filtering in lockstep

**Files:**
- Modify: `crates/client/src/renderer/config.rs`
- Modify: `crates/client/src/renderer/voxel_pull/pipeline.rs`
- Modify: `crates/client/src/renderer/voxel_pull/queue.rs`

**Interfaces:**

```rust
impl VoxelShadowFilter {
    pub const fn bevy_component(self) -> ShadowFilteringMethod;
    pub const fn shader_defs(self) -> &'static [&'static str];
}

pub struct VoxelPullPipelineKey {
    // existing fields remain
    pub shadow_filter: VoxelShadowFilter,
}
```

- [ ] Add tests proving Hardware2x2 maps to `ShadowFilteringMethod::Hardware2x2` and its shader definition, Gaussian maps to `Gaussian` and its definition, and the two keys specialize distinct pipelines.
- [ ] Run `cargo test -p client shadow_filter -- --nocapture`; expect failure because the key is still hardcoded.
- [ ] Insert the resolved `ShadowFilteringMethod` on each terrain `Camera3d` in `configure_render_quality`.
- [ ] Add `shadow_filter` to `VoxelPullPipelineKey`, source it from the extracted view's filtering method in `queue.rs`, and delete the unconditional Gaussian definitions from `shadow_filter_shader_defs`.
- [ ] Assert in a queue/pipeline unit test that custom forward and depth/shadow specialization consume the same key; leave Bevy per-chunk material specialization to its camera component.
- [ ] Run `cargo test -p client shadow_filter` and `just check`; expect success.
- [ ] Commit with `git commit -m "feat(client): specialize pulled shadows by quality filter"`.

### Task 4: Configure the terrain sun and shadow resources explicitly

**Files:**
- Modify: `crates/client/src/renderer/mod.rs`
- Modify: `crates/client/src/renderer/config.rs`

- [ ] Add a main-world integration test that spawns render setup for a non-default config and asserts the sun owns a `CascadeShadowConfig` with exactly the requested cascade count and that `DirectionalLightShadowMap.size` matches the request.
- [ ] Run `cargo test -p client shadow_quality_resources -- --nocapture`; expect failure because setup uses Bevy defaults.
- [ ] Build `CascadeShadowConfig` with `CascadeShadowConfigBuilder { num_cascades: quality.shadow_cascades as usize, ..default() }` and attach it to the existing directional-light entity.
- [ ] Insert `DirectionalLightShadowMap { size: quality.shadow_map_size as usize }` before the light plugin prepares shadow views. Keep all existing sun color, transform, and shadow-enabled values.
- [ ] Run the focused test and `just check`; expect success.
- [ ] Commit with `git commit -m "feat(client): configure terrain shadow cascades and map size"`.

### Task 5: Add renderer workload snapshots and schema-v2 benchmark metadata

**Files:**
- Modify: `crates/client/src/renderer/render_bench.rs`
- Modify: `crates/client/src/renderer/voxel_pull/arena.rs`
- Modify: `crates/client/src/renderer/voxel_pull/gpu.rs`
- Modify: `crates/client/src/renderer/meshing/lod.rs`
- Modify: `crates/client/src/renderer/lod_rings.rs`
- Modify: `crates/client/src/renderer/far_voxel/pass.rs`

**Interfaces:**

```rust
#[derive(Resource, Clone, Default)]
pub struct TerrainWorkloadSnapshot {
    pub live_quad_records: Option<u64>,
    pub quad_capacity: Option<u64>,
    pub full_res_quads: Option<u64>,
    pub lod_quads_by_factor: Option<BTreeMap<u32, u64>>,
    pub full_res_cells: Option<u64>,
    pub lod_cells_by_factor: Option<BTreeMap<u32, u64>>,
    pub frustum_rejected: Option<u64>,
    pub frustum_candidates: Option<u64>,
    pub hzb_rejected: Option<u64>,
    pub hzb_candidates: Option<u64>,
}
```

- [ ] Add serialization tests for a fully populated snapshot and a backend with missing counters. Assert missing fields are JSON `null`, ratios are `null` when the denominator is zero/unavailable, and every missing name appears once in `unavailable_metrics`.
- [ ] Add metadata tests for cascade texels (`count * size * size`), actual internal resolution, and the complete resolved quality object.
- [ ] Run `cargo test -p client render_bench::tests -- --nocapture`; expect the new assertions to fail.
- [ ] Increment the benchmark schema version. Extend `BenchmarkMetadata`/result structures with `quality_preset`, `quality`, `internal_resolution`, `shadow_map_texels`, workload fields, and calculated rejection ratios.
- [ ] Populate the main-world snapshot from live `ChunkQuads`/LOD entities and quad allocations; publish render-world cull counters asynchronously through the existing benchmark/readback channel. Do not add synchronous GPU waits.
- [ ] Use `MainPassResolutionOverride` when present and camera physical target size otherwise. Keep existing fields and their JSON names backward compatible.
- [ ] Run `cargo test -p client render_bench::tests` and `just render-bench --help` (or `just --show render-bench` if the recipe has no help mode); expect success and no benchmark launch.
- [ ] Commit with `git commit -m "feat(client): report render quality and terrain workload"`.

### Task 6: Add named quality presets and three-run comparison workflow

**Files:**
- Modify: `justfile`
- Modify: `crates/client/src/renderer/render_bench.rs`
- Modify: `scripts/render_bench.py` if present; otherwise create `scripts/compare_render_bench.py`
- Modify: `docs/benchmarks/README.md` if present; otherwise create it

- [ ] Add script tests using temporary JSON fixtures: three runs per side, median-of-run p50/p95 summaries, a passing 5%/5% gate, a failing p95 gate, missing GPU timings, and memory-regression reporting.
- [ ] Run the script test command exposed by the repository's existing script-test convention; if none exists, run `python3 -m unittest scripts.tests.test_compare_render_bench`; expect failure before the comparator exists.
- [ ] Extend `render-bench` with `quality=current` default and exact mappings: `cheap-aa`, `cheap-shadows`, `balanced`, `scale-75`. Implement shell defaulting with `${VAR:-preset_value}` so explicit environment values override presets.
- [ ] Export `VOXEL_RENDER_QUALITY_PRESET` for JSON metadata and reject unknown preset names before launching the client.
- [ ] Implement the comparator to require three result files per side, compare medians of each run's p50/p95 summaries, enforce p50 improvement ≥5% and p95 regression ≤5%, and report reserved-memory and unavailable-metric differences without concealing them.
- [ ] Document exact commands for all five presets and the three-repetition comparison.
- [ ] Run the script tests and `just --list`; expect success and a visible `render-bench` recipe.
- [ ] Commit with `git commit -m "feat(justfile): add render benchmark quality matrix"`.

### Task 7: End-to-end verification and default-parity evidence

**Files:**
- Create: `docs/benchmarks/2026-07-16-render-quality-baseline.md`

- [ ] Run `just fmt`.
- [ ] Run `just fmt-check`, `just clippy`, `just test`, `cargo check -p client --target wasm32-unknown-unknown`, and `just test-full`; all must exit 0.
- [ ] Run three `current` benchmark repetitions and one smoke run for each non-default preset with the production seed/camera path. Record command lines, adapter, commit, output files, medians, and unavailable metrics.
- [ ] Capture the existing visual probe positions under `current` and verify no holes, LOD seams, missing shadows, or unstable transitions. Record any deliberate preset quality differences separately from defects.
- [ ] Confirm current resolves to SMAA High/Gaussian/4/2048/1.0 in JSON and that internal resolution equals output resolution.
- [ ] Commit with `git commit -m "docs: record render quality benchmark baseline"`.

