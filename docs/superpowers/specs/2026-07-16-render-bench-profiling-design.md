# Render Benchmark Profiling Design

**Date:** 2026-07-16

**Status:** Approved

## Purpose

Improve `just render-bench` into the canonical, one-command source of reproducible render-performance data. Each run must produce a machine-readable artifact and each repetition set must produce a concise human-readable report. The first version reports measurements and baseline deltas but never fails because performance regressed.

The normal benchmark remains an optimized, minimally instrumented release build. Tracy, Chrome trace, RenderDoc, and `perf` are drill-down tools for explaining results, not sources of headline comparison numbers.

## Current State

The working tree already contains a deterministic production-scene benchmark in `crates/client/src/renderer/render_bench.rs` and a `just render-bench` recipe. It:

- loads the production seed and complete LOD table;
- freezes the simulation after the expected near regions and LOD keys arrive;
- waits for renderer queues to settle for 120 stable frames;
- warms up for 10 seconds;
- measures a fixed 30-second, 360-degree camera sweep at 1920×1080 without vsync;
- repeats the run three times by default;
- records frame-time and per-pass GPU-time percentiles;
- records selected visibility, draw, far-cell, brick-dedup, and reserved-memory counters; and
- records the graphics adapter and driver in schema-v1 JSON.

The repository also already has separate recipes for Bevy render diagnostics, Chrome traces, Tracy, Pyroscope, `perf`/Hotspot, and AMD GPU utilization sampling.

The principal gaps are:

- no explicit terrain-face/quad workload;
- no pipeline primitive or shader-invocation statistics in benchmark JSON;
- no per-pass CPU timing in benchmark JSON;
- no distinction between allocated GPU capacity and logically used bytes;
- insufficient run/build/host provenance for reliable comparisons;
- no aggregation across repetitions or baseline comparison report;
- unavailable metrics list names but not reasons; and
- existing deep-profiling recipes do not consistently reproduce the deterministic benchmark scene.

## Goals

1. Produce detailed, typed timing, geometry, GPU-work, visibility, draw, and memory metrics in schema-v2 JSON.
2. Generate a terminal and Markdown summary after every repetition set.
3. Compare a candidate repetition set with a compatible baseline and show absolute and percentage deltas.
4. Make metric semantics explicit, especially the difference between terrain quads, submitted primitives, and fragment work.
5. Preserve the deterministic scene, phase timings, backend matrix, and non-blocking render loop.
6. Provide deterministic Chrome trace and Tracy drill-down variants without treating instrumented results as comparable headline data.
7. Use no new runtime Rust dependency and no third-party Python dependency.

## Non-Goals

- Performance thresholds or CI failures caused by regressions.
- A benchmark results database, web dashboard, or long-term retention service.
- Automatic statistical significance claims.
- Bundling or installing Tracy, RenderDoc, Hotspot, `perf`, or vendor GPU tools.
- Treating a RenderDoc frame or an instrumented trace as equivalent to a normal benchmark run.
- Vendor-specific GPU hardware counters in the portable schema.
- Changing terrain generation, renderer output, simulation determinism, or vendored deterministic forks.

## Chosen Approach

Extend the native benchmark artifact and add a standard-library Python reducer. Renderer-owned resources publish semantic counters through the benchmark's existing render-thread snapshot. Bevy's `RenderDiagnosticsPlugin` supplies pass timings and pipeline statistics. The reducer validates repetitions, aggregates them, emits a report, and optionally compares them with a compatible baseline.

This approach was selected over two alternatives:

1. **Tracy-first benchmark:** excellent for CPU/GPU timelines, scheduling, locks, memory, and sampling, but captures are interactive, version-coupled, large, and affected by instrumentation overhead.
2. **RenderDoc-first benchmark:** excellent for a single frame's draw calls, resources, shaders, and pipeline state, but a captured/replayed frame is not a statistically representative 30-second measurement.

Tracy and RenderDoc remain part of the documented investigation ladder.

## Architecture

The data path is:

```text
renderer semantic counters + Bevy render diagnostics
                         |
                         v
                 per-frame samples
                         |
                         v
                schema-v2 run JSON
                         |
                         v
              repetition-set reducer
                         |
                         +--> terminal/Markdown report
                         |
                         `--> optional baseline deltas
```

`crates/client/src/renderer/render_bench.rs` remains responsible for environment settings, readiness, phase transitions, camera motion, sample timing, output coordination, and process exit. Focused submodules under `crates/client/src/renderer/render_bench/` own metric summaries/schema and render-thread telemetry. Existing renderer modules remain the authoritative owners of backend-specific counters.

Counter collection must never block on the GPU during a measured frame. Existing asynchronous count-buffer readback patterns may be extended. New synchronous `map_async` waits, `device.poll(...Wait...)`, queue-idle waits, or equivalent stalls are prohibited in the measured path.

## Metric Contract

### Summary shapes

Every sampled duration and ratio uses a floating-point summary containing:

- `min`
- `p50`
- `p95`
- `p99`
- `max`
- `sample_count`

Every sampled count uses the same fields with integer values. Percentiles use the existing nearest-rank convention. Empty input produces `null`; it never produces a zero-shaped summary.

### Timing

The artifact records:

- `load_to_ready_ms`: elapsed wall time from client start until the stable-ready gate opens;
- `frame_wall_ms`: Bevy frame-time samples during the measurement phase;
- `fps`: sampled or correctly derived frame-rate summary;
- `gpu_busy_estimate_ms`: the sum of available non-overlapping per-pass GPU elapsed times for a frame;
- `pass_cpu_ms`: map from normalized pass name to CPU command-recording-time summary;
- `pass_gpu_ms`: map from normalized pass name to GPU execution-time summary; and
- fixed load timeout, warmup duration, measurement duration, and camera sweep metadata.

`gpu_busy_estimate_ms` is explicitly labeled an estimate because GPU pass timestamps may expose parallel or reordered execution and summing spans is not universally equivalent to end-to-end GPU frame latency. It must not be named `gpu_frame_ms` in schema v2.

### Geometry and GPU work

“Faces rendered” is represented by distinct metrics rather than one overloaded number:

- `resident_quads`: greedy voxel surface quads logically stored by a quad-based renderer;
- `submitted_quads`: terrain quads surviving the backend's selected visibility path and submitted for drawing;
- `submitted_triangles`: derived as two triangles per submitted terrain quad only for backends whose draw contract guarantees that relationship;
- `submitted_vertices`: exact backend submission count where available;
- `vertex_shader_invocations`: hardware pipeline-statistics result per render pass;
- `clipper_invocations`: hardware pipeline-statistics result per render pass;
- `clipper_primitives_out`: hardware pipeline-statistics result per render pass and the closest portable measure of primitives reaching rasterization;
- `fragment_shader_invocations`: hardware pipeline-statistics result per render pass and the portable overdraw/shading-work indicator; and
- `compute_shader_invocations`: hardware pipeline-statistics result per compute pass.

Metrics are grouped by meaningful scope (`near`, `far`, or render-pass name) rather than collapsed when collapsing would mix incompatible units. A proxy/raymarch far backend reports cells, proxy primitives, and shader invocations. It does not label proxy geometry or ray steps as voxel faces.

Bevy's render diagnostics expose CPU/GPU elapsed time, primitive statistics, and shader invocations. In Bevy 0.18, timestamp and pipeline-statistics results are available on Vulkan and DX12; unsupported platforms retain CPU timings while GPU-dependent fields are unavailable.

### Visibility, draws, and residency

The artifact retains and clarifies:

- active near chunks;
- frustum-visible near chunks;
- HZB-visible near chunks when HZB is the selected source;
- direct and indirect terrain draw counts, kept separate;
- resident and visible far cells;
- unique far bricks and brick deduplication ratio; and
- readiness counts for every expected LOD factor.

Counter documentation states whether a value counts logical draw calls, indirect command records, stable command slots, or GPU survivor commands. Zero is valid only when the metric applies and the observed count is zero; unsupported or inapplicable metrics are `null`.

### Memory

Memory reporting separates:

- `logical_used_gpu_bytes`: bytes occupied by live logical records;
- `reserved_gpu_bytes`: allocated buffer capacity, including slack; and
- near/far breakdowns when the owners can provide them without duplicating accounting.

Memory totals use checked or saturating conversions to `u64`. The schema documents whether resources such as textures, pipelines, bind groups, and driver allocations are outside the total; it must not imply full process VRAM usage.

### Unavailable values

Schema v2 replaces `unavailable_metrics: [name, ...]` with typed entries containing a metric path and one of these stable reasons:

- `adapter_feature_unavailable`
- `not_applicable_to_backend`
- `telemetry_readback_disabled`
- `sample_not_observed`

An optional short detail may identify the missing adapter feature or backend condition. Human-readable details are not used as machine keys.

## Reproducibility Metadata

Every run records:

- schema version and instrumentation mode (`normal`, `chrome_trace`, or `tracy`);
- git revision and dirty-worktree flag supplied by the recipe;
- Cargo profile and enabled profiling feature;
- client and server thread caps;
- OS, architecture, and CPU identity when available through existing platform facilities or recipe inputs;
- requested and resolved near/far renderer backends;
- graphics adapter, device, backend, driver, and driver information;
- resolution, present mode/vsync, world seed, LOD table, near-region requirement, stable-frame requirement, phase durations, and camera path; and
- repetition number and user-supplied run label when present.

Metadata discovery must not add a runtime dependency. Values unavailable portably may be supplied by the `justfile` or represented as `null` with an unavailable reason.

## JSON Output and Atomicity

The benchmark writes schema-v2 pretty JSON to a temporary sibling path, flushes it, and renames it over the destination. A partially written final JSON file must not survive a crash or serialization error.

A completed scene with required frame samples produces status `complete`. Scene timeout, absence of required frame samples, or output failure produces a nonzero client exit. When possible, a timeout or missing-sample failure still writes a status `failed` artifact containing readiness state and the reason. Optional adapter-dependent metrics do not fail an otherwise valid run.

Schema v1 is not silently mixed with schema v2. The report tool rejects schema-v1 input with an actionable instruction to rerun the benchmark.

## Repetition Aggregation and Comparison

`scripts/render_bench_report.py` uses only the Python standard library. It accepts a repetition-set directory or explicit JSON paths, validates every artifact, and writes a Markdown report beside the run files while printing the same essential tables to stdout.

The default report contains:

1. run identity and compatibility metadata;
2. timing summaries;
3. geometry, visibility, draw, GPU-work, and memory summaries; and
4. unavailable metrics and interpretation caveats.

Aggregation operates across complete repetitions. The report states the number of expected, complete, failed, and missing repetitions. It does not fabricate aggregate values for absent metrics. Run-level summaries are combined using an explicitly documented across-run statistic; they are not presented as if raw frame samples had been pooled.

An optional baseline adds columns for baseline, candidate, absolute delta, and percentage delta. Lower-is-better or higher-is-better annotations are descriptive only; no color, word, or exit status claims that a delta is a pass or failure.

Baseline comparison requires equality for:

- world seed and LOD table;
- resolution and present mode;
- requested and resolved renderer backends;
- client/server thread caps;
- GPU vendor/device and API backend;
- stable-ready, warmup, and measurement settings; and
- camera path.

Mismatches in those fields stop comparison with a clear error. Git revision is expected to differ. Driver, OS, CPU, dirty state, and Cargo profile differences are displayed prominently; instrumentation mode must be `normal` on both sides for headline comparison.

The report command returns nonzero for malformed data, failed/missing required repetitions, incompatible comparisons, or output failure. It always returns zero for a valid report regardless of the measured deltas.

## `justfile` Interface

`just render-bench` keeps the existing backend, adapter, repetitions, and threads arguments. It adds optional run-label and baseline inputs, passes provenance into each client process, and runs the reducer after all repetitions finish.

The normal recipe continues to build release client and server binaries once, run one server for the repetition set, and clean it up reliably.

Deterministic drill-down recipes run the same scene and camera path with:

- Bevy Chrome trace enabled; or
- Bevy Tracy tracing enabled, with the detailed-trace option remaining explicitly high overhead.

Their JSON metadata marks them instrumented. Their timing values may aid navigation but the reducer refuses to compare them as normal benchmark results.

RenderDoc remains documented rather than required by a recipe because installation, injection, capture triggering, and replay are platform-specific. A RenderDoc capture is used to inspect a representative steady-state frame, not to generate benchmark timing percentiles.

## Profiling Escalation Ladder

1. Run the normal render benchmark and read timing, pass, geometry, visibility, draw, memory, and pipeline-statistics tables.
2. If CPU-side time is unexplained, run the deterministic Chrome trace and reduce it with `scripts/trace_summary.py`.
3. If scheduling, parallelism, locks, allocations, or frame-to-frame interaction needs inspection, use the deterministic Tracy variant with a protocol-matching Tracy GUI.
4. If a GPU pass has unexpected primitive/fragment workload or resource behavior, capture a steady-state frame in RenderDoc and inspect events, resources, and shaders.
5. If a native CPU hotspot remains unclear, use `perf`/Hotspot or the existing folded-stack recipe on the optimized profiling build.

Tracy is a real-time frame and sampling profiler supporting CPU timelines, GPU contexts, locks, memory, context switches, and frame images. RenderDoc is a frame-capture graphics debugger for Vulkan, D3D, and OpenGL-class APIs. Neither replaces stable native benchmark telemetry.

## File Boundaries

Expected implementation areas are:

- `crates/client/src/renderer/render_bench.rs`: phases, sampling orchestration, camera path, exit coordination;
- `crates/client/src/renderer/render_bench/metrics.rs`: summary types, percentile/count calculations, pass-statistic collection;
- `crates/client/src/renderer/render_bench/output.rs`: schema-v2 types, unavailable reasons, validation, atomic serialization;
- `crates/client/src/renderer/render_bench/telemetry.rs`: render-thread snapshot and translation of renderer-owned metrics;
- existing near and far renderer modules: authoritative semantic counter production;
- `scripts/render_bench_report.py`: aggregation, compatibility validation, terminal/Markdown output;
- `scripts/tests/test_render_bench_report.py` and fixtures: reducer tests;
- `justfile`: orchestration, provenance inputs, aggregation, and deterministic trace variants; and
- `docs/render-benchmark.md`: operator guide, metric dictionary, worked examples, and profiler escalation ladder.

Exact module splits may be adjusted during planning if current call sites show a smaller boundary, but lifecycle control, schema/output, reducer, and renderer-owned telemetry remain separate responsibilities.

## Testing and Verification

Rust tests cover:

- nearest-rank summaries including min/max/sample count and empty input;
- aggregation of pass CPU/GPU time and all pipeline-statistics families;
- backend applicability and unavailable-reason selection;
- exact quad-to-triangle/vertex derivation only where guaranteed;
- logical-used versus reserved-byte accounting;
- schema-v2 serialization shape and stable reason strings;
- required-sample failure behavior; and
- atomic-output success and failure behavior using a temporary test directory.

Python tests use small checked-in fixtures and cover:

- a valid multi-repetition aggregate;
- incomplete and failed repetition detection;
- `null` metrics and unavailable reasons;
- absolute and percentage delta formatting, including a zero baseline;
- every compatibility-key mismatch;
- instrumented-versus-normal rejection;
- malformed JSON and schema-v1 errors; and
- deterministic Markdown output.

Acceptance verification includes:

1. focused Rust and Python tests during implementation;
2. `just render-bench repetitions=1` on an available Vulkan adapter, confirming a complete schema-v2 run and report;
3. a baseline/candidate self-comparison, confirming zero deltas and compatibility handling;
4. `just fmt-check`;
5. `just clippy`; and
6. the repository-required `just test-full`.

Hardware smoke results are recorded as evidence, not encoded as stable numeric assertions.

## Risks and Mitigations

- **Telemetry perturbs the benchmark.** Keep the normal build free of Tracy/Chrome tracing, reuse Bevy's existing query mechanism, use asynchronous readback, and disclose that query instrumentation has a cost.
- **A “face” count becomes misleading across backends.** Keep resident quads, submitted quads, rasterized primitives, proxy primitives, and fragment invocations distinct; mark inapplicable values `null`.
- **GPU pass times are summed incorrectly.** Normalize pass paths, avoid double-counting nested diagnostics, and label the sum as a busy-time estimate.
- **Comparisons mix incompatible machines or scenes.** Enforce compatibility keys before computing deltas and surface environmental differences.
- **Aggregating percentiles implies pooled raw samples.** State the across-run aggregation method and display repetition count and spread.
- **The harness grows too large.** Separate summary/schema and render-thread telemetry into focused submodules while leaving phase ownership in the existing module.
- **Dirty working trees obscure provenance.** Record both revision and dirty state; allow a run label for deliberate local experiments.

## Research References

- Bevy `RenderDiagnosticsPlugin`: <https://docs.rs/bevy_render/0.18.1/bevy_render/diagnostic/struct.RenderDiagnosticsPlugin.html>
- Bevy 0.18 feature flags (`trace_chrome`, `trace_tracy`, `detailed_trace`): <https://docs.rs/crate/bevy/0.18.1/features>
- wgpu query types and required features: <https://wgpu.rs/doc/wgpu/enum.QueryType.html>
- wgpu timestamp-query example: <https://wgpu.rs/doc/wgpu_examples/timestamp_queries/index.html>
- Tracy official repository and capabilities: <https://github.com/wolfpld/tracy>
- RenderDoc official repository and supported APIs: <https://github.com/baldurk/renderdoc>

## Approved Decisions

- Optimize for one-command, machine-readable commit comparison.
- Start report-only; do not introduce performance gates.
- Use native structured telemetry as the primary artifact.
- Retain Tracy and RenderDoc as optional drill-down tools.
- Use explicit metric semantics and capability-dependent `null` values.
- Generate a report after repetitions and support an optional compatible baseline.
