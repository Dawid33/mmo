# Render Benchmark Profiling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn `just render-bench` into a one-command, report-only profiling benchmark with unambiguous geometry/GPU-work metrics, schema-v2 run artifacts, repetition aggregation, and optional compatible-baseline deltas.

**Architecture:** The existing deterministic benchmark continues to own scene readiness, warmup, measurement, and camera motion. Renderer owners publish semantic geometry and memory counters through the existing render-world snapshot, Bevy render diagnostics supply pass timings and pipeline statistics, and a standard-library Python reducer validates and compares the resulting schema-v2 JSON files. Tracy, Chrome trace, RenderDoc, and `perf` remain opt-in investigation paths and never define headline benchmark numbers.

**Tech Stack:** Rust 2021, Bevy 0.18 render diagnostics, wgpu timestamp/pipeline-statistics queries through Bevy, WGSL, Serde JSON, Python 3 standard library, Bash/Just.

## Global Constraints

- The first version is report-only: no performance threshold or regression delta may fail a valid benchmark/report command.
- Preserve the existing deterministic production seed, complete LOD table, 1920×1080 no-vsync window, 120 stable-ready frames, 10-second warmup, 30-second measurement, and 360-degree camera sweep.
- Keep `game` and `server` free of Bevy/windowing dependencies.
- Do not touch or replace patched vendored forks under `crates/`.
- The measured render path must not use a synchronous GPU map, queue-idle wait, or `Maintain::Wait`/equivalent device wait.
- Normal benchmark builds must not enable Tracy, Chrome trace, or detailed tracing.
- Unsupported or backend-inapplicable metrics serialize as `null` with a stable reason; never substitute zero.
- `resident_quads`, `submitted_quads`, `clipper_primitives_out`, and `fragment_shader_invocations` remain separate metrics.
- Python reporting code uses only the standard library.
- Any new test exceeding 100 ms must use `#[ignore = "slow: >100ms — run via just test-full"]`.
- Preserve unrelated working-tree changes. Before every commit, stage only the paths named in that task and inspect `git diff --cached`.
- Final verification is `just fmt-check`, `just clippy`, and `just test-full`, plus one hardware render-benchmark smoke run when Vulkan/DX12 hardware is available.

---

### Task 1: Extract typed metric summaries and pass samples

**Files:**
- Create: `crates/client/src/renderer/render_bench/metrics.rs`
- Modify: `crates/client/src/renderer/render_bench.rs`

**Interfaces:**
- Produces: `FloatSummary::from_samples(&[f64]) -> Option<FloatSummary>`
- Produces: `CountSummary::from_samples(&[u64]) -> Option<CountSummary>`
- Produces: `PassSamples::observe(path: &str, value: f64)` and `PassSamples::summaries() -> BTreeMap<String, FloatSummary>`
- Consumes: Bevy diagnostic paths, normalized by removing only the terminal metric suffix.

- [ ] **Step 1: Write failing summary tests**

Create `metrics.rs` with the test module first. The tests pin min/max, nearest-rank percentiles, sample count, empty input, and stable pass-name normalization:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_summary_includes_range_percentiles_and_count() {
        let values = (1..=100).map(f64::from).collect::<Vec<_>>();
        assert_eq!(
            FloatSummary::from_samples(&values),
            Some(FloatSummary {
                min: 1.0,
                p50: 50.0,
                p95: 95.0,
                p99: 99.0,
                max: 100.0,
                sample_count: 100,
            })
        );
        assert_eq!(FloatSummary::from_samples(&[]), None);
    }

    #[test]
    fn count_summary_uses_the_same_nearest_rank_contract() {
        assert_eq!(
            CountSummary::from_samples(&[9, 1, 5, 3]),
            Some(CountSummary {
                min: 1,
                p50: 3,
                p95: 9,
                p99: 9,
                max: 9,
                sample_count: 4,
            })
        );
    }

    #[test]
    fn pass_samples_remove_only_the_metric_suffix() {
        let mut samples = PassSamples::with_suffix("/elapsed_gpu");
        samples.observe("render/main_opaque_pass_3d/elapsed_gpu", 2.5);
        samples.observe("render/main_opaque_pass_3d/elapsed_gpu", 3.5);
        assert_eq!(
            samples
                .summaries()
                .get("render/main_opaque_pass_3d")
                .expect("pass summary")
                .sample_count,
            2
        );
    }
}
```

- [ ] **Step 2: Run the focused test and verify it fails**

Run: `cargo test -p client render_bench::metrics::tests --lib`

Expected: compile failures because `FloatSummary`, `CountSummary`, and `PassSamples` do not exist.

- [ ] **Step 3: Implement the summary module**

Add the complete public data shapes and a shared nearest-rank index:

```rust
use std::collections::BTreeMap;

use serde::Serialize;

fn nearest_rank_index(len: usize, percent: usize) -> usize {
    (percent * len).div_ceil(100).max(1) - 1
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FloatSummary {
    pub min: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
    pub sample_count: usize,
}

impl FloatSummary {
    pub fn from_samples(samples: &[f64]) -> Option<Self> {
        let mut sorted = samples.to_vec();
        if sorted.is_empty() {
            return None;
        }
        sorted.sort_by(f64::total_cmp);
        Some(Self {
            min: sorted[0],
            p50: sorted[nearest_rank_index(sorted.len(), 50)],
            p95: sorted[nearest_rank_index(sorted.len(), 95)],
            p99: sorted[nearest_rank_index(sorted.len(), 99)],
            max: *sorted.last().expect("non-empty checked above"),
            sample_count: sorted.len(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CountSummary {
    pub min: u64,
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub max: u64,
    pub sample_count: usize,
}

impl CountSummary {
    pub fn from_samples(samples: &[u64]) -> Option<Self> {
        let mut sorted = samples.to_vec();
        if sorted.is_empty() {
            return None;
        }
        sorted.sort_unstable();
        Some(Self {
            min: sorted[0],
            p50: sorted[nearest_rank_index(sorted.len(), 50)],
            p95: sorted[nearest_rank_index(sorted.len(), 95)],
            p99: sorted[nearest_rank_index(sorted.len(), 99)],
            max: *sorted.last().expect("non-empty checked above"),
            sample_count: sorted.len(),
        })
    }
}

#[derive(Default)]
pub struct PassSamples {
    suffix: &'static str,
    values: BTreeMap<String, Vec<f64>>,
}

impl PassSamples {
    pub fn with_suffix(suffix: &'static str) -> Self {
        Self {
            suffix,
            values: BTreeMap::new(),
        }
    }

    pub fn observe(&mut self, path: &str, value: f64) {
        let name = path.strip_suffix(self.suffix).unwrap_or(path);
        self.values.entry(name.to_owned()).or_default().push(value);
    }

    pub fn summaries(&self) -> BTreeMap<String, FloatSummary> {
        self.values
            .iter()
            .filter_map(|(name, values)| {
                FloatSummary::from_samples(values).map(|summary| (name.clone(), summary))
            })
            .collect()
    }
}
```

In `render_bench.rs`, declare `mod metrics;`, import these types, replace `Percentiles` and the old `CountSummary`, and move existing sample vectors onto the new types without changing phase behavior.

- [ ] **Step 4: Run the focused and existing benchmark unit tests**

Run: `cargo test -p client render_bench --lib`

Expected: all render-benchmark tests pass; the old percentile test is either moved to `metrics.rs` or removed as duplicate coverage.

- [ ] **Step 5: Commit the focused extraction**

```bash
git add crates/client/src/renderer/render_bench.rs crates/client/src/renderer/render_bench/metrics.rs
git diff --cached --check
git commit -m "refactor(client): type render benchmark metric summaries"
```

---

### Task 2: Make terrain quad workload authoritative

**Files:**
- Modify: `crates/client/src/renderer/meshing/mod.rs`
- Modify: `crates/client/src/renderer/meshing/lod.rs`
- Modify: `crates/client/src/renderer/voxel_pull/prepare.rs`
- Modify: `crates/client/src/renderer/voxel_pull/gpu.rs`
- Modify: `assets/shaders/voxel_pull_cull.wgsl`

**Interfaces:**
- Produces: `TerrainQuadCount(pub u32)` beside every nonempty terrain `Mesh3d` or `ChunkQuads`.
- Extends: `NearRendererMetrics` with `resident_quads`, `submitted_quads`, and `submitted_vertices`.
- Extends: benchmark-only GPU cull telemetry from `[visible_chunks, padding]` to `[visible_chunks, visible_quads]` without increasing the existing eight-byte staging allocation.
- Preserves: nonblocking six-slot asynchronous telemetry ring.

- [ ] **Step 1: Write failing meshing-output tests**

In `meshing/mod.rs`, extend the existing `pull_path_attaches_records_and_no_mesh` test and the legacy `Mesh3d` pipeline test to require the same semantic component:

```rust
let quad_count = app
    .world()
    .entity(e)
    .get::<TerrainQuadCount>()
    .expect("nonempty terrain carries its quad workload");
assert_eq!(quad_count.0 as usize, expected_quad_count);
```

Add a pure test that `mesh_chunk` reports the exact count for both output formats:

```rust
#[test]
fn mesh_output_keeps_quad_count_for_mesh_and_pull_formats() {
    let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
    voxels[5 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
    let padded = binary::PaddedVoxels::from_interior(&voxels);
    let layers = BlockTextureLayers::default();
    let mesh = mesh_chunk(&padded, &layers, false);
    let pull = mesh_chunk(&padded, &layers, true);
    assert_eq!(mesh.quad_count, pull.quad_count);
    assert_eq!(mesh.quad_count, 6);
}
```

- [ ] **Step 2: Run the meshing tests and verify they fail**

Run: `cargo test -p client renderer::meshing --lib`

Expected: compile failures for missing `TerrainQuadCount` and `MeshOutput::quad_count`.

- [ ] **Step 3: Carry quad count through meshing and LOD application**

Add the component and field:

```rust
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerrainQuadCount(pub u32);

pub struct MeshOutput {
    pub mesh: Option<Mesh>,
    pub records: Option<Vec<records::QuadRecord>>,
    pub connectivity: u16,
    pub quad_count: u32,
}
```

Set `quad_count` exactly once from `quads.len()` in `mesh_chunk` and the LOD mesh builder, using `u32::try_from(quads.len()).expect("terrain quad count fits u32")`. Insert `TerrainQuadCount(quad_count)` in both `apply_meshed_chunks` and `apply_lod_meshes` whenever a nonempty mesh/record result is attached. Remove it on the same branches that remove `Mesh3d` and `ChunkQuads` so no stale count survives a remesh or despawn.

Update every existing `MeshOutput` test literal in `meshing/mod.rs` and `meshing/lod.rs` with `quad_count: 0` for empty fixtures and the exact record/mesh quad count for nonempty fixtures. Destructure `quad_count` beside `mesh`, `records`, and `connectivity` in both apply systems.

- [ ] **Step 4: Write failing direct/GPU submission telemetry tests**

In `voxel_pull/prepare.rs`, add a pure helper test:

```rust
#[test]
fn draw_list_workload_sums_quads_and_vertices() {
    let draws = ViewChunkDrawList(vec![
        GpuChunkQuads { record_start: 0, quad_count: 4, chunk_slot: 0 },
        GpuChunkQuads { record_start: 4, quad_count: 7, chunk_slot: 1 },
    ]);
    assert_eq!(draw_list_workload(&draws), (11, 66));
}
```

In `voxel_pull/gpu.rs`, extend `telemetry_values_distinguish_hzb_survivors_from_submitted_commands` so a sample with `visible_quads: 41` returns `submitted_quads: 41` and `submitted_vertices: 246` in both indirect-count and fixed-command modes.

- [ ] **Step 5: Run the telemetry tests and verify they fail**

Run: `cargo test -p client 'renderer::voxel_pull' --lib`

Expected: compile failures for the new fields/helper.

- [ ] **Step 6: Implement direct and GPU-visible quad counts**

Extend the metric resource:

```rust
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NearRendererMetrics {
    pub active_chunks: u32,
    pub frustum_visible_chunks: u32,
    pub hzb_visible_chunks: Option<u32>,
    pub indirect_draw_count: Option<u32>,
    pub resident_quads: u64,
    pub submitted_quads: Option<u64>,
    pub submitted_vertices: Option<u64>,
}
```

Compute `resident_quads` from `arena.allocations.values().map(|a| u64::from(a.quad_count)).sum()`; do not use `RawBufferVec::capacity()` or high-water marks. For the non-shadow Core3d view in grouped-direct mode, sum `ViewChunkDrawList` quad counts and multiply by six with `saturating_mul` for vertices. Keep shadow-view work out of this semantic main-forward counter; hardware pass statistics still expose shadow workload separately.

Change the WGSL count structure and visible path:

```wgsl
struct DrawCount {
    visible_chunks: atomic<u32>,
    visible_quads: atomic<u32>,
};

if (visible) {
    atomicAdd(&draw_count.visible_chunks, 1u);
    atomicAdd(&draw_count.visible_quads, chunk.quad_count);
}
```

Update all Rust/WGSL references from `draw_count.value` to `draw_count.visible_chunks`. The existing count/readback buffers are already eight bytes; clear both words and parse the mapped bytes as `[u32; 2]`. Add `visible_quads` to `NearTelemetrySample`, then publish `submitted_quads` and `submitted_vertices` from the latest completed sample. Retain the latest sample when a callback does not finish; never wait for it.

- [ ] **Step 7: Verify shader and renderer telemetry**

Run:

```bash
cargo test -p client renderer::meshing --lib
cargo test -p client renderer::voxel_pull --lib
cargo test -p client --test near_gpu_missing_hzb
```

Expected: all pass, including Naga validation of the modified cull shader and the non-HZB fallback path.

- [ ] **Step 8: Commit authoritative terrain workload**

```bash
git add crates/client/src/renderer/meshing/mod.rs crates/client/src/renderer/meshing/lod.rs crates/client/src/renderer/voxel_pull/prepare.rs crates/client/src/renderer/voxel_pull/gpu.rs assets/shaders/voxel_pull_cull.wgsl
git diff --cached --check
git commit -m "perf(client): expose exact terrain quad workload"
```

---

### Task 3: Add schema-v2 telemetry, provenance, and atomic output

**Files:**
- Create: `crates/client/src/renderer/render_bench/telemetry.rs`
- Create: `crates/client/src/renderer/render_bench/output.rs`
- Modify: `crates/client/src/renderer/render_bench.rs`
- Modify: `crates/client/src/renderer/far_voxel/gpu.rs`
- Modify: `crates/client/src/renderer/far_voxel/pass.rs`

**Interfaces:**
- Produces: schema-v2 `BenchmarkResult` and `UnavailableMetric { path, reason, detail }`.
- Produces: `write_result_atomic(path: &Path, result: &BenchmarkResult) -> io::Result<()>`.
- Produces: pass maps for `elapsed_cpu`, `elapsed_gpu`, `vertex_shader_invocations`, `clipper_invocations`, `clipper_primitives_out`, `fragment_shader_invocations`, and `compute_shader_invocations`.
- Extends: `BenchmarkRunSettings` with instrumentation/provenance inputs from the environment.
- Consumes: `TerrainQuadCount`, `NearRendererMetrics`, `FarVoxelPassMetrics`, `GpuQuadArena`, and `GpuFarBuffers`.

- [ ] **Step 1: Write failing schema and unavailable-reason tests**

In `output.rs`, start with these tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_reasons_have_stable_machine_strings() {
        assert_eq!(
            serde_json::to_value(UnavailableReason::AdapterFeatureUnavailable).unwrap(),
            "adapter_feature_unavailable"
        );
        assert_eq!(
            serde_json::to_value(UnavailableReason::NotApplicableToBackend).unwrap(),
            "not_applicable_to_backend"
        );
    }

    #[test]
    fn schema_v2_serializes_instrumentation_and_sample_counts() {
        let value = serde_json::to_value(fixture_result()).unwrap();
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["benchmark"]["instrumentation"], "normal");
        assert_eq!(value["metrics"]["frame_wall_ms"]["sample_count"], 3);
        assert!(value["unavailable_metrics"].is_array());
    }
}
```

The local `fixture_result()` must construct the smallest complete `BenchmarkResult`; do not load a repository fixture from Rust tests.

- [ ] **Step 2: Run the schema tests and verify they fail**

Run: `cargo test -p client render_bench::output::tests --lib`

Expected: compile failures for the missing module/types.

- [ ] **Step 3: Define schema-v2 output types**

Use stable serialized enums:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Instrumentation {
    Normal,
    ChromeTrace,
    Tracy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    AdapterFeatureUnavailable,
    NotApplicableToBackend,
    TelemetryReadbackDisabled,
    SampleNotObserved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UnavailableMetric {
    pub path: String,
    pub reason: UnavailableReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
```

Define nested `TimingMetrics`, `GeometryMetrics`, `VisibilityMetrics`, `DrawMetrics`, `MemoryMetrics`, `PipelineStatistics`, `BenchmarkMetadata`, `SerializableReadiness`, and `BenchmarkResult`. Use explicit near/far fields; do not preserve the schema-v1 flattened `indirect_draws` ambiguity. Set `schema_version: 2` in exactly one constructor.

Required top-level structure:

```json
{
  "schema_version": 2,
  "status": "complete",
  "failure_reason": null,
  "benchmark": {},
  "readiness_at_exit": {},
  "metrics": {
    "timing": {},
    "geometry": {},
    "visibility": {},
    "draws": {},
    "memory": {},
    "pipeline_statistics": {}
  },
  "unavailable_metrics": []
}
```

- [ ] **Step 4: Write failing telemetry translation tests**

In `telemetry.rs`, build a `RenderThreadSnapshot` fixture with 17 active chunks, 11 frustum survivors, 7 HZB survivors, 41 resident quads, 23 submitted quads, one far indirect draw, and distinct logical/reserved byte totals. Assert that translation preserves each scope and derives `submitted_triangles = 46` and `submitted_vertices = 138` only from the exact 23-quad contract.

Add a brick-backend fixture asserting `far.resident_quads == None`, `far.proxy_vertices == Some(PROXY_VERTEX_COUNT * visible_instances)`, and an unavailable entry with reason `not_applicable_to_backend` for `metrics.geometry.far.resident_quads`.

- [ ] **Step 5: Implement renderer snapshot and memory accounting**

Move `AdapterMetadata`, `RenderThreadSnapshot`, `SharedRenderThreadSnapshot`, `capture_render_thread_snapshot`, and snapshot translation from `render_bench.rs` to `telemetry.rs`.

Add these owner methods:

```rust
impl GpuFarBuffers {
    pub fn logical_used_bytes(&self) -> usize {
        self.mixed_pages
            .iter()
            .map(GpuSlotBuffer::logical_used_bytes)
            .sum::<usize>()
            + self.roots.logical_used_bytes()
            + self.cells.logical_used_bytes()
            + FAR_STATIC_BINDING_OVERHEAD_BYTES
            + self.pass_reserved_bytes
    }
}

impl<T: Pod + Zeroable + Copy> GpuSlotBuffer<T> {
    pub fn logical_used_bytes(&self) -> usize {
        self.mirror.active_slots().saturating_mul(std::mem::size_of::<T>())
    }
}
```

For the near arena, calculate logical bytes from live allocations and slots, not backing-vector length. Calculate reserved bytes from buffer capacities as today. Keep near and far subtotals and convert each computed `logical_bytes`/`reserved_bytes` value with `u64::try_from(logical_bytes).unwrap_or(u64::MAX)` and `u64::try_from(reserved_bytes).unwrap_or(u64::MAX)` at the serialization boundary.

For the brick pass, extend `FarVoxelPassMetrics` with the exact visible instance count already written into indirect draw arguments. If no asynchronous readback exists for that count, report proxy submission counts as unavailable rather than using active-cell capacity. Hardware `clipper_primitives_out` remains available per pass independently.

- [ ] **Step 6: Write failing pass-diagnostic classification tests**

Add a pure classifier to `metrics.rs` and tests for every supported suffix:

```rust
#[test]
fn classifies_all_bevy_render_diagnostic_families() {
    assert_eq!(classify("render/ui/elapsed_cpu"), Some(("render/ui", PassMetric::ElapsedCpu)));
    assert_eq!(classify("render/ui/elapsed_gpu"), Some(("render/ui", PassMetric::ElapsedGpu)));
    assert_eq!(classify("render/main/clipper_primitives_out"), Some(("render/main", PassMetric::ClipperPrimitivesOut)));
    assert_eq!(classify("render/cull/compute_shader_invocations"), Some(("render/cull", PassMetric::ComputeShaderInvocations)));
    assert_eq!(classify("fps"), None);
}
```

- [ ] **Step 7: Collect all supported Bevy diagnostics during measurement**

Implement:

```rust
pub enum PassMetric {
    ElapsedCpu,
    ElapsedGpu,
    VertexShaderInvocations,
    ClipperInvocations,
    ClipperPrimitivesOut,
    FragmentShaderInvocations,
    ComputeShaderInvocations,
}

pub fn classify(path: &str) -> Option<(&str, PassMetric)> {
    const SUFFIXES: [(&str, PassMetric); 7] = [
        ("/elapsed_cpu", PassMetric::ElapsedCpu),
        ("/elapsed_gpu", PassMetric::ElapsedGpu),
        ("/vertex_shader_invocations", PassMetric::VertexShaderInvocations),
        ("/clipper_invocations", PassMetric::ClipperInvocations),
        ("/clipper_primitives_out", PassMetric::ClipperPrimitivesOut),
        ("/fragment_shader_invocations", PassMetric::FragmentShaderInvocations),
        ("/compute_shader_invocations", PassMetric::ComputeShaderInvocations),
    ];
    SUFFIXES
        .into_iter()
        .find_map(|(suffix, metric)| path.strip_suffix(suffix).map(|name| (name, metric)))
}
```

Derive `Clone, Copy` for `PassMetric`. Store duration families as `Vec<f64>` and invocation families as `Vec<u64>` after rejecting negative/non-finite values and saturating values above `u64::MAX as f64`. Sum only top-level, non-nested GPU spans into `gpu_busy_estimate_ms`; add a test fixture with nested paths so the total cannot double-count parent and child spans. If Bevy does not expose a trustworthy containment relationship, omit the sum and add `sample_not_observed` rather than claiming an end-to-end GPU frame duration.

- [ ] **Step 8: Add provenance inputs and required-sample validation**

Extend `BenchmarkRunSettings::from_env()` to read:

```text
RENDER_BENCH_INSTRUMENTATION = normal|chrome_trace|tracy
RENDER_BENCH_GIT_REVISION
RENDER_BENCH_GIT_DIRTY = 0|1
RENDER_BENCH_LABEL
RENDER_BENCH_PROFILE
RENDER_BENCH_OS
RENDER_BENCH_ARCH
RENDER_BENCH_CPU
RENDER_BENCH_EXPECTED_REPETITIONS
VOXEL_CLIENT_WORKER_THREADS
TOKIO_WORKER_THREADS
```

Parse through pure helpers returning descriptive errors. Add tests for valid values, invalid booleans/instrumentation, and absent optional provenance. Record requested backends from environment separately from `TerrainRenderConfig`'s resolved values.

Record `load_to_ready_ms` on the `Loading -> Warming` transition. Before declaring `complete`, require nonempty `frame_wall_ms`; otherwise set status `failed`, reason `measurement completed without frame-time samples`, write the failed artifact, and exit nonzero.

Sample `FrameTimeDiagnosticsPlugin::FPS` into its own `Vec<f64>` and summarize it with `FloatSummary`; do not invert the frame-time percentile fields because `1 / p95(frame_ms)` is not `p95(fps)`. Add a unit test with frame samples `[10.0, 20.0]` and FPS samples `[100.0, 50.0]` asserting the two summaries retain their independently observed ordering and sample counts.

For the legacy per-chunk near backend, add `TerrainQuadCount` to `BenchmarkScene`'s near-chunk query and sum it for `resident_quads`. Do not derive `submitted_quads` from Bevy's global `ViewVisibility`, which may include shadow/other views; serialize it as `null` with `telemetry_readback_disabled` and direct users to per-pass `clipper_primitives_out`. For raster far LOD, sum `TerrainQuadCount` on ready `LodChunk` entities for `far.resident_quads`. Pull-renderer near resident/submitted values continue to come from the render-thread snapshot.

- [ ] **Step 9: Write and implement atomic-output tests**

Test in a unique path under `std::env::temp_dir()` using the process id plus an atomic sequence. The test must assert that the final file parses, no sibling `.tmp` remains, and a pre-existing final file is replaced.

Implement same-directory atomic replacement:

```rust
pub fn write_result_atomic(path: &Path, result: &BenchmarkResult) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(OsStr::to_str).unwrap_or("json")));
    let mut file = File::create(&tmp)?;
    serde_json::to_writer_pretty(&mut file, result).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}
```

Use a unique temporary sibling name containing the process id/repetition if concurrent writers can target the same directory. Do not use `/tmp` for the actual benchmark write because cross-filesystem rename is not atomic.

- [ ] **Step 10: Run all render-benchmark and owner tests**

Run:

```bash
cargo test -p client render_bench --lib
cargo test -p client renderer::far_voxel --lib
cargo test -p client renderer::voxel_pull --lib
```

Expected: all pass and schema-v1 output types no longer exist.

- [ ] **Step 11: Commit schema-v2 benchmark telemetry**

```bash
git add crates/client/src/renderer/render_bench.rs crates/client/src/renderer/render_bench/metrics.rs crates/client/src/renderer/render_bench/output.rs crates/client/src/renderer/render_bench/telemetry.rs crates/client/src/renderer/far_voxel/gpu.rs crates/client/src/renderer/far_voxel/pass.rs
git diff --cached --check
git commit -m "feat(client): emit render benchmark schema v2 telemetry"
```

---

### Task 4: Build the repetition and baseline report tool

**Files:**
- Create: `scripts/render_bench_report.py`
- Create: `scripts/tests/test_render_bench_report.py`
- Create: `scripts/tests/fixtures/render_bench/baseline-run-1.json`
- Create: `scripts/tests/fixtures/render_bench/baseline-run-2.json`
- Create: `scripts/tests/fixtures/render_bench/candidate-run-1.json`
- Create: `scripts/tests/fixtures/render_bench/candidate-run-2.json`

**Interfaces:**
- CLI: `scripts/render_bench_report.py RUN_JSONS --expected N --baseline BASELINE_JSONS --markdown PATH` (the two JSON arguments accept one or more files/directories).
- Produces: stdout summary and deterministic Markdown report.
- Exit 0: valid report, regardless of positive/negative performance deltas.
- Exit 2: malformed/schema-v1/failed/missing/incompatible inputs or report write failure.

- [ ] **Step 1: Add minimal schema-v2 fixtures**

Each fixture contains complete benchmark identity plus these representative metrics:

```json
{
  "schema_version": 2,
  "status": "complete",
  "failure_reason": null,
  "benchmark": {
    "instrumentation": "normal",
    "repetition": 1,
    "expected_repetitions": 2,
    "resolution": [1920, 1080],
    "vsync": false,
    "world_seed": 918063844728,
    "lod_ring_table": [[2, 2], [4, 4], [8, 16], [16, 64], [32, 256]],
    "near_backend_requested": "gpu",
    "near_backend_resolved": "gpu",
    "far_backend_requested": "raster",
    "far_backend_resolved": "raster",
    "client_threads": 4,
    "server_threads": 4,
    "stable_ready_frames": 120,
    "warmup_seconds": 10,
    "measurement_seconds": 30,
    "camera_path": "yaw_360",
    "profile": "release",
    "git_dirty": false,
    "os": "Linux",
    "arch": "x86_64",
    "cpu": "Fixture CPU",
    "adapter": {"vendor": 4318, "device": 8118, "backend": "Vulkan", "driver": "fixture", "driver_info": "1.0"}
  },
  "metrics": {
    "timing": {"frame_wall_ms": {"min": 8.0, "p50": 10.0, "p95": 15.0, "p99": 18.0, "max": 20.0, "sample_count": 100}},
    "geometry": {"near": {"resident_quads": {"min": 1000, "p50": 1000, "p95": 1000, "p99": 1000, "max": 1000, "sample_count": 100}}},
    "visibility": {}, "draws": {}, "memory": {}, "pipeline_statistics": {}
  },
  "unavailable_metrics": []
}
```

Use these exact per-file values; all fields not named here remain identical to the JSON above:

| Fixture | repetition | git revision | frame p50 | frame min/max | near resident-quads p50 |
|---|---:|---|---:|---|---:|
| `baseline-run-1.json` | 1 | `baseline` | 8.0 | 7.0 / 12.0 | 900 |
| `baseline-run-2.json` | 2 | `baseline` | 10.0 | 8.0 / 14.0 | 1,000 |
| `candidate-run-1.json` | 1 | `candidate` | 10.0 | 8.0 / 20.0 | 1,000 |
| `candidate-run-2.json` | 2 | `candidate` | 12.0 | 9.0 / 22.0 | 1,100 |

Set `benchmark.git_revision` to the named revision. Set each summary's `p95` and `p99` between its p50 and max, and keep `sample_count: 100`. This makes the tested across-run frame-p50 medians exactly 9.0 ms (baseline) and 11.0 ms (candidate).

- [ ] **Step 2: Write failing reducer unit tests**

Use `importlib.util.spec_from_file_location` to import the script directly. Add this complete test skeleton, then fill the four fixture files with the values from Step 1:

```python
import copy
import importlib.util
import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "render_bench_report", ROOT / "scripts/render_bench_report.py"
)
report = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(report)
FIXTURES = ROOT / "scripts/tests/fixtures/render_bench"


def runs(prefix: str) -> list[dict]:
    return [report.load_run(path) for path in sorted(FIXTURES.glob(f"{prefix}-run-*.json"))]


class RenderBenchReportTests(unittest.TestCase):
    def test_aggregate_uses_median_of_run_summaries_and_keeps_spread(self):
        aggregate = report.aggregate_set(runs("candidate"))
        frame_p50 = aggregate["metrics.timing.frame_wall_ms"]["p50"]
        self.assertEqual(frame_p50.runs, 2)
        self.assertEqual(frame_p50.median, 11.0)
        self.assertEqual((frame_p50.run_min, frame_p50.run_max), (10.0, 12.0))

    def test_compare_emits_absolute_and_percentage_delta(self):
        base = report.aggregate_set(runs("baseline"))
        candidate = report.aggregate_set(runs("candidate"))
        absolute, percent = report.delta(
            base["metrics.timing.frame_wall_ms"]["p50"].median,
            candidate["metrics.timing.frame_wall_ms"]["p50"].median,
        )
        self.assertEqual(absolute, 2.0)
        self.assertEqual(percent, 2.0 / 9.0 * 100.0)

    def test_zero_baseline_percentage_is_na(self):
        self.assertEqual(report.delta(0.0, 4.0), (4.0, None))

    def test_every_core_compatibility_key_mismatch_is_rejected(self):
        base = runs("baseline")
        candidate = runs("candidate")
        for path in report.CORE_COMPATIBILITY_PATHS:
            changed = copy.deepcopy(candidate)
            report.set_path_for_test(changed[0], path, "different")
            errors = report.compatibility_errors(base, changed)
            self.assertTrue(any(path in error for error in errors), path)

    def test_driver_difference_warns_but_does_not_reject(self):
        base = runs("baseline")
        candidate = runs("candidate")
        candidate[0]["benchmark"]["adapter"]["driver"] = "different-driver"
        self.assertEqual(report.compatibility_errors(base, candidate), [])
        self.assertTrue(report.environment_warnings(base, candidate))

    def test_instrumented_input_is_not_headline_comparable(self):
        candidate = runs("candidate")
        candidate[0]["benchmark"]["instrumentation"] = "tracy"
        errors = report.compatibility_errors(runs("baseline"), candidate)
        self.assertTrue(any("instrumentation" in error for error in errors))

    def test_failed_or_missing_repetition_is_rejected(self):
        candidate = runs("candidate")
        candidate[0]["status"] = "failed"
        with self.assertRaises(report.ReportError):
            report.validate_set(candidate, expected=2)
        with self.assertRaises(report.ReportError):
            report.validate_set(runs("candidate")[:1], expected=2)

    def test_schema_v1_error_tells_the_user_to_rerun(self):
        document = runs("candidate")[0]
        document["schema_version"] = 1
        with self.assertRaisesRegex(report.ReportError, "rerun"):
            report.validate_document(document, pathlib.Path("old.json"))

    def test_markdown_output_is_deterministic(self):
        base = runs("baseline")
        candidate = runs("candidate")
        first = report.render_markdown(candidate, base)
        second = report.render_markdown(list(reversed(candidate)), list(reversed(base)))
        self.assertEqual(first, second)


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 3: Run tests and verify they fail**

Run: `python3 -m unittest scripts.tests.test_render_bench_report -v`

Expected: import failure because `scripts/render_bench_report.py` does not exist.

- [ ] **Step 4: Implement loading, flattening, and compatibility**

Use these stable interfaces and implementations for parsing and compatibility:

```python
import argparse
import dataclasses
import json
import pathlib
import statistics
import sys


class ReportError(Exception):
    pass


CORE_COMPATIBILITY_PATHS = (
    "benchmark.resolution",
    "benchmark.vsync",
    "benchmark.world_seed",
    "benchmark.lod_ring_table",
    "benchmark.near_backend_requested",
    "benchmark.near_backend_resolved",
    "benchmark.far_backend_requested",
    "benchmark.far_backend_resolved",
    "benchmark.client_threads",
    "benchmark.server_threads",
    "benchmark.stable_ready_frames",
    "benchmark.warmup_seconds",
    "benchmark.measurement_seconds",
    "benchmark.camera_path",
    "benchmark.adapter.vendor",
    "benchmark.adapter.device",
    "benchmark.adapter.backend",
)

ENVIRONMENT_WARNING_PATHS = (
    "benchmark.adapter.driver",
    "benchmark.adapter.driver_info",
    "benchmark.os",
    "benchmark.arch",
    "benchmark.cpu",
    "benchmark.profile",
    "benchmark.git_dirty",
)


def get_path(document: dict, dotted: str):
    value = document
    for part in dotted.split("."):
        value = value[part]
    return value


def set_path_for_test(document: dict, dotted: str, value) -> None:
    target = document
    parts = dotted.split(".")
    for part in parts[:-1]:
        target = target[part]
    target[parts[-1]] = value


def validate_document(document: dict, path: pathlib.Path) -> None:
    version = document.get("schema_version")
    if version != 2:
        raise ReportError(f"{path}: schema {version!r}; rerun with the schema-v2 benchmark")
    if document.get("status") != "complete":
        raise ReportError(f"{path}: benchmark status is {document.get('status')!r}")


def load_run(path: pathlib.Path) -> dict:
    with path.open(encoding="utf-8") as handle:
        document = json.load(handle)
    validate_document(document, path)
    document["_source_path"] = str(path)
    return document


def validate_set(runs: list[dict], expected: int | None) -> None:
    if not runs:
        raise ReportError("no benchmark runs supplied")
    for index, run in enumerate(runs):
        validate_document(run, pathlib.Path(run.get("_source_path", f"run-{index + 1}")))
    repetitions = sorted(run["benchmark"]["repetition"] for run in runs)
    if len(repetitions) != len(set(repetitions)):
        raise ReportError("duplicate repetition numbers")
    if expected is not None and repetitions != list(range(1, expected + 1)):
        raise ReportError(f"expected repetitions 1..{expected}, got {repetitions}")


def compatibility_errors(baseline: list[dict], candidate: list[dict]) -> list[str]:
    errors: list[str] = []
    for label, group in (("baseline", baseline), ("candidate", candidate)):
        if any(run["benchmark"]["instrumentation"] != "normal" for run in group):
            errors.append(f"{label}.benchmark.instrumentation must be normal")
    for path in CORE_COMPATIBILITY_PATHS:
        values = {json.dumps(get_path(run, path), sort_keys=True) for run in baseline + candidate}
        if len(values) != 1:
            errors.append(f"incompatible {path}: {sorted(values)}")
    return errors


def environment_warnings(baseline: list[dict], candidate: list[dict]) -> list[str]:
    warnings: list[str] = []
    for path in ENVIRONMENT_WARNING_PATHS:
        values = {json.dumps(get_path(run, path), sort_keys=True) for run in baseline + candidate}
        if len(values) != 1:
            warnings.append(f"environment differs at {path}: {sorted(values)}")
    return warnings


SUMMARY_KEYS = frozenset(("min", "p50", "p95", "p99", "max", "sample_count"))


def flatten_summaries(document: dict) -> dict[str, dict]:
    flattened: dict[str, dict] = {}

    def visit(path: str, value) -> None:
        if isinstance(value, dict) and SUMMARY_KEYS.issubset(value):
            flattened[path] = value
        elif isinstance(value, dict):
            for key in sorted(value):
                visit(f"{path}.{key}" if path else key, value[key])

    visit("metrics", document["metrics"])
    return flattened
```

`flatten_summaries` recognizes mappings containing all of `min,p50,p95,p99,max,sample_count` and leaves metadata/unavailable entries out. Sort paths and input filenames for deterministic output.

- [ ] **Step 5: Implement across-run aggregation and deltas**

Use median-of-run-summary values and expose run spread:

```python
@dataclasses.dataclass(frozen=True)
class Aggregate:
    runs: int
    median: float
    run_min: float
    run_max: float

def aggregate(values: list[float]) -> Aggregate:
    return Aggregate(len(values), statistics.median(values), min(values), max(values))


def aggregate_set(runs: list[dict]) -> dict[str, dict[str, Aggregate]]:
    validate_set(runs, expected=None)
    flattened = [flatten_summaries(run) for run in runs]
    common_paths = set(flattened[0]).intersection(*(set(item) for item in flattened[1:]))
    return {
        path: {
            key: aggregate([float(item[path][key]) for item in flattened])
            for key in ("min", "p50", "p95", "p99", "max", "sample_count")
        }
        for path in sorted(common_paths)
    }


def delta(baseline: float, candidate: float) -> tuple[float, float | None]:
    absolute = candidate - baseline
    percent = None if baseline == 0 else absolute / baseline * 100.0
    return absolute, percent
```

Never pool frame percentiles or call median-of-percentiles a pooled percentile. Report headings must say `median across N runs` and show `[run min, run max]`.

- [ ] **Step 6: Implement terminal/Markdown rendering and CLI**

Render four sections: identity, timing, geometry/GPU work, and unavailable/caveats. Comparison rows contain baseline, candidate, absolute delta, and percentage delta. Use neutral signs only; do not print `PASS`, `FAIL`, `regression`, or `improvement` based on the values.

Catch `ReportError`, `OSError`, `json.JSONDecodeError`, and `KeyError` at the CLI boundary, print one actionable message to stderr, and return 2. A valid comparison with any delta returns 0.

- [ ] **Step 7: Run reducer tests and CLI fixtures**

Run:

```bash
python3 -m unittest scripts.tests.test_render_bench_report -v
python3 scripts/render_bench_report.py scripts/tests/fixtures/render_bench/candidate-run-*.json --expected 2 --baseline scripts/tests/fixtures/render_bench/baseline-run-*.json --markdown /tmp/render-bench-report.md
```

Expected: tests pass; CLI exits 0 and prints baseline/candidate/delta columns; Markdown exists.

- [ ] **Step 8: Commit the report tool**

```bash
git add scripts/render_bench_report.py scripts/tests/test_render_bench_report.py scripts/tests/fixtures/render_bench
git diff --cached --check
git commit -m "feat(scripts): report render benchmark repetitions"
```

---

### Task 5: Make `just render-bench` produce reports and provenance

**Files:**
- Modify: `justfile`
- Modify: `crates/client/Cargo.toml` only if the existing Tracy/Chrome features need no-op aliases; otherwise leave it unchanged.

**Interfaces:**
- Normal: `just render-bench backend=gpu+raster adapter=T600 repetitions=3 threads=4 label=candidate baseline=target/render-bench-baseline`
- Chrome: `just render-bench-trace backend=gpu+raster adapter=T600 threads=4`
- Tracy: `just render-bench-tracy backend=gpu+raster adapter=T600 threads=4 detailed=`

- [ ] **Step 1: Add a shell-level validation smoke test recipe helper**

Before editing the recipe, capture its argument parsing in a Bash function embedded in the recipe or a private Just recipe. Validate:

- positive repetitions;
- threads >= 4;
- known backend pairs;
- baseline path exists when nonempty;
- label slug contains only `[A-Za-z0-9._-]` after sanitization.

Run invalid invocations and confirm exit 2 before any Cargo build:

```bash
just render-bench repetitions=0
just render-bench threads=3
just render-bench backend=unknown
just render-bench baseline=/definitely/missing
```

Expected: each exits 2 with its specific validation message and does not print a Cargo compile line.

- [ ] **Step 2: Extend the normal recipe**

Change the signature to:

```make
render-bench backend="direct+raster" adapter="" repetitions="3" threads="4" label="" baseline="":
```

At recipe start, compute provenance once:

```bash
git_revision=$(git rev-parse HEAD 2>/dev/null || true)
if git diff --quiet --ignore-submodules HEAD -- 2>/dev/null; then git_dirty=0; else git_dirty=1; fi
host_os=$(uname -s)
host_arch=$(uname -m)
host_cpu=$(lscpu 2>/dev/null | sed -n 's/^Model name:[[:space:]]*//p' | head -n 1)
```

Pass these exact variables to every client run:

```bash
RENDER_BENCH_INSTRUMENTATION=normal
RENDER_BENCH_GIT_REVISION="$git_revision"
RENDER_BENCH_GIT_DIRTY="$git_dirty"
RENDER_BENCH_LABEL="$selected_label"
RENDER_BENCH_PROFILE=release
RENDER_BENCH_OS="$host_os"
RENDER_BENCH_ARCH="$host_arch"
RENDER_BENCH_CPU="$host_cpu"
RENDER_BENCH_EXPECTED_REPETITIONS="$selected_repetitions"
```

Write each invocation's outputs into a unique set directory containing backend, adapter slug, label slug, and revision short hash so stale JSON from previous sets cannot be included. After the loop:

```bash
report="$set_dir/report.md"
report_args=("$set_dir"/run-*.json --expected "$selected_repetitions" --markdown "$report")
if [ -n "$selected_baseline" ]; then report_args+=(--baseline "$selected_baseline"); fi
scripts/render_bench_report.py "${report_args[@]}"
echo "report: $report"
```

- [ ] **Step 3: Add deterministic trace variants without duplicating orchestration**

Extract the common Bash orchestration into `scripts/render_bench.sh` only if Just cannot share it without copying more than the build command and feature/instrumentation selection. If extracted, add `scripts/render_bench.sh` to this task's file list and keep `justfile` as the canonical public interface.

Chrome trace builds with `--release --features trace_chrome`, sets `RENDER_BENCH_INSTRUMENTATION=chrome_trace`, and writes `TRACE_CHROME` inside its set directory. Tracy builds with `--profile profiling --features tracy` or `tracy-detailed`, sets `RENDER_BENCH_INSTRUMENTATION=tracy`, and prints the required protocol-matching GUI version already documented by the current recipe.

Both variants reuse `RENDER_BENCH=1`, scene/backend/thread variables, phase timing, and camera sweep. The report tool may summarize them alone but refuses baseline comparison against `normal` runs.

- [ ] **Step 4: Verify orchestration without claiming hardware performance**

Run:

```bash
just --list | rg 'render-bench|render-bench-trace|render-bench-tracy'
just render-bench repetitions=0
just render-bench threads=3
bash -n scripts/render_bench.sh  # only if the helper was created
```

Expected: recipes are listed; invalid inputs exit 2 before build; helper syntax passes when present.

- [ ] **Step 5: Commit orchestration**

```bash
git add justfile
if test -f scripts/render_bench.sh; then git add scripts/render_bench.sh; fi
git diff --cached --check
git commit -m "feat(justfile): report deterministic render benchmarks"
```

---

### Task 6: Document the metric dictionary and investigation ladder

**Files:**
- Create: `docs/render-benchmark.md`
- Modify: `justfile` comments adjacent to the benchmark and profiling recipes.

**Interfaces:** none; operator documentation only.

- [ ] **Step 1: Write the operator guide**

Include these exact sections:

1. `Quick start` with a normal three-run command and baseline comparison command.
2. `Output layout` showing set directory, run JSON files, server log, trace when applicable, and `report.md`.
3. `Metric dictionary` defining every schema-v2 path and unit.
4. `What “faces rendered” means` distinguishing resident/submitted quads, submitted triangles, clipper primitives, and fragment invocations.
5. `Compatibility rules` listing hard comparison keys and displayed warnings.
6. `Unavailable metrics` listing the four stable reasons.
7. `Profiling escalation ladder` in this order: normal report, Chrome trace, Tracy, RenderDoc, `perf`/Hotspot.
8. `Platform limitations` stating Bevy 0.18 GPU timestamps/pipeline statistics are Vulkan/DX12-only while CPU pass timing remains available elsewhere.
9. `Interpretation caveats` covering query overhead, GPU busy estimate, median-across-runs aggregation, and instrumented captures.

Use the following researched primary references:

- `https://docs.rs/bevy_render/0.18.1/bevy_render/diagnostic/struct.RenderDiagnosticsPlugin.html`
- `https://docs.rs/crate/bevy/0.18.1/features`
- `https://wgpu.rs/doc/wgpu/enum.QueryType.html`
- `https://github.com/wolfpld/tracy`
- `https://github.com/baldurk/renderdoc`

- [ ] **Step 2: Add a checked-in example report excerpt**

Use fixture-derived numbers and label them `example fixture`, not measured hardware results. Show timing and geometry rows with baseline/candidate/absolute/% columns, plus a `percentage: n/a` zero-baseline example.

- [ ] **Step 3: Verify commands and names against the code**

Run:

```bash
rg -n 'resident_quads|submitted_quads|clipper_primitives_out|fragment_shader_invocations|gpu_busy_estimate_ms' docs/render-benchmark.md crates/client/src/renderer/render_bench scripts/render_bench_report.py
just --list | rg 'render-bench|trace|tracy|perf'
```

Expected: every documented metric appears in the schema/collector, and every documented public recipe appears in `just --list`.

- [ ] **Step 4: Commit documentation**

```bash
git add docs/render-benchmark.md justfile
git diff --cached --check
git commit -m "docs: explain render benchmark profiling workflow"
```

---

### Task 7: End-to-end verification and evidence

**Files:**
- Modify only if verification exposes a defect in files already named above.

**Interfaces:** none; acceptance gate.

- [ ] **Step 1: Run focused automated tests**

Run:

```bash
cargo test -p client render_bench --lib
cargo test -p client renderer::meshing --lib
cargo test -p client renderer::voxel_pull --lib
cargo test -p client renderer::far_voxel --lib
cargo test -p client --test near_gpu_missing_hzb
python3 -m unittest scripts.tests.test_render_bench_report -v
```

Expected: all pass.

- [ ] **Step 2: Run repository static gates**

Run:

```bash
just fmt
just fmt-check
just clippy
```

Expected: formatting check and Clippy pass without warnings. Inspect `git diff` after `just fmt` and keep only formatting changes within task files.

- [ ] **Step 3: Run the required full suite**

Run: `just test-full`

Expected: all workspace tests, including ignored slow tests, pass.

- [ ] **Step 4: Run one hardware benchmark smoke**

Run on an available Vulkan/DX12 adapter:

```bash
just render-bench backend=gpu+raster repetitions=1 threads=4 label=profiling-smoke
```

Expected:

- client exits successfully after readiness, warmup, and measurement;
- one schema-v2 JSON and `report.md` are created in a fresh set directory;
- `frame_wall_ms.sample_count > 0`;
- Vulkan/DX12 run contains pass GPU time and available pipeline statistics;
- resident/submitted quad metrics are nonzero for the near GPU backend;
- no final `.tmp` file remains; and
- report command exits 0.

If no qualifying adapter/display is available, record the hardware smoke as not run and do not invent results. Automated tests and `just test-full` remain mandatory.

- [ ] **Step 5: Self-compare the smoke artifact**

Run the report script with the smoke set as both candidate and baseline.

Expected: every comparable delta is zero, compatibility succeeds, and exit status is 0.

- [ ] **Step 6: Inspect the final diff and working tree**

Run:

```bash
git diff --check
git status --short
git log --oneline -8
```

Expected: no whitespace errors; task commits are narrow and ordered; unrelated pre-existing changes remain untouched and unstaged.

## Plan Self-Review

- **Spec coverage:** schema-v2 typed summaries and provenance → Tasks 1 and 3; exact face/quad semantics and nonblocking telemetry → Task 2; pass CPU/GPU and pipeline statistics → Task 3; repetition aggregation and compatible baseline deltas → Task 4; one-command report and deterministic profiler variants → Task 5; metric definitions and Tracy/RenderDoc escalation → Task 6; required verification → Task 7.
- **Non-goals preserved:** no thresholds, dashboard, third-party Python package, bundled profiler, or vendor-specific portable counters.
- **Type consistency:** `FloatSummary`/`CountSummary`, `Instrumentation`, `UnavailableReason`, `TerrainQuadCount`, `NearRendererMetrics`, and schema-v2 metric names are introduced before downstream use.
- **Worktree safety:** every commit stages explicit paths and requires cached-diff inspection; unrelated dirty files are never reset or staged wholesale.
