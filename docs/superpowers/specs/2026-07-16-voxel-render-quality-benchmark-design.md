# Voxel Render Quality and Benchmark Controls — Design

**Date:** 2026-07-16  
**Status:** Approved design  
**Scope:** Measurement, render-quality configuration, and rollout gates for later voxel-renderer optimizations.

## 1. Goal

Make voxel-rendering changes comparable and safely selectable. The production benchmark must identify whether terrain is limited by geometry, shadows, post-processing, or resolution, while interactive builds must expose the same startup quality choices without changing simulation state.

This phase deliberately changes no terrain representation. Its output is the acceptance harness used by the raster-LOD and hybrid-brick phases.

## 2. Current baseline

The client already has:

- `TerrainRenderConfig` for near/far backend selection;
- a deterministic production-scene benchmark with fixed seed, camera sweep, readiness gates, GPU pass timings, culling counts, and reserved-byte metrics;
- native SMAA High with MSAA disabled;
- one shadow-casting directional light;
- a custom pulled-terrain shader that hardcodes Gaussian shadow filtering.

Checked-in T600/NVK samples show that the raster path is dominated by the main opaque pass, four shadow cascades, and SMAA. These samples contain one repetition each and are diagnostic evidence, not acceptance baselines.

## 3. Architecture

Add a `RenderQualityConfig` resource beside `TerrainRenderConfig`. Resolution is a pure function of explicit string inputs and target capabilities, so parsing and platform fallbacks are unit-testable. Native builds read environment overrides at startup. wasm32 keeps its current portable defaults and does not read process environment.

The resource controls:

- antialiasing mode;
- pulled-terrain shadow filtering;
- directional-shadow cascade count;
- directional-shadow map size;
- fixed internal render scale.

The benchmark records the resolved values, adds renderer-owned workload counters, and can run a named quality matrix. Interactive and benchmark paths consume the same resource; the benchmark must not carry private rendering behavior.

## 4. Configuration contract

The native environment interface is:

- `VOXEL_AA=off|fxaa|smaa-low|smaa-medium|smaa-high`;
- `VOXEL_SHADOW_FILTER=hardware-2x2|gaussian`;
- `VOXEL_SHADOW_CASCADES=1|2|3|4`;
- `VOXEL_SHADOW_MAP_SIZE=512|1024|2048|4096`;
- `VOXEL_RENDER_SCALE=0.50..1.00`, inclusive.

Invalid values fail startup and name the offending variable and allowed domain. Defaults preserve current native visuals: SMAA High, Gaussian shadows, four cascades, 2048 shadow maps, and scale 1.0. wasm32 preserves its current AA behavior, raster far backend, and scale 1.0.

No runtime settings UI, persistence file, adaptive resolution controller, TAA, DLSS, or FSR integration is part of this phase.

## 5. Renderer integration

### 5.1 Antialiasing

Replace `configure_native_antialiasing`'s unconditional SMAA insertion with one system driven by `RenderQualityConfig`. It removes conflicting AA components before inserting the selected mode. FXAA and SMAA are mutually exclusive; `off` inserts neither. MSAA remains off for every mode in this phase.

### 5.2 Shadow filtering

Extend `VoxelPullPipelineKey` with the selected filter. Shader definitions must match the camera's configured `ShadowFilteringMethod`; the custom pipeline may no longer hardcode Gaussian. The per-chunk fallback material path receives the same camera component through Bevy's normal specialization.

### 5.3 Cascade configuration

Attach an explicit `CascadeShadowConfig` to the terrain sun and insert `DirectionalLightShadowMap` from the resolved quality config. This keeps benchmark metadata and actual render resources synchronized.

### 5.4 Render scale

Render scale changes the main 3D target resolution while the window and final UI remain at output resolution. The existing final upscaling stage presents the scaled 3D target. Camera projection, HZB dimensions, far-pass view resources, and benchmark metadata use the actual scaled extent. Scale is fixed for a run; dynamic adjustment is out of scope.

## 6. Benchmark data

Extend benchmark JSON with:

- complete resolved quality configuration;
- total live quad-record count and allocated quad capacity;
- full-resolution quad count;
- LOD quad counts keyed by factor;
- full-resolution and LOD active-cell counts;
- frustum rejection count and ratio;
- HZB rejection count and ratio when available;
- shadow cascade count and shadow-map texel count;
- actual internal render resolution.

All renderer-specific fields remain nullable when the selected backend cannot provide them. Missing data is reported in `unavailable_metrics`; it is never silently encoded as zero.

The existing frame, GPU-frame, pass, indirect-draw, brick, and reserved-byte metrics remain backward compatible. Increment the benchmark schema version when fields are added.

## 7. Benchmark workflow

Keep `just render-bench` as the canonical entry point. Add a `quality` argument with named presets:

- `current`: current defaults;
- `cheap-aa`: FXAA, otherwise current;
- `cheap-shadows`: hardware 2x2, two cascades, 1024 map;
- `balanced`: SMAA Medium, hardware 2x2, three cascades, 1024 map;
- `scale-75`: balanced plus render scale 0.75.

Explicit environment values override preset fields, allowing one-variable experiments. JSON records both the preset name and final resolved fields.

Production comparisons use three repetitions per backend/quality/adapter. Reports compare the median of each run's p50 and p95 summaries rather than pooling frames across runs.

## 8. Acceptance and rollout

This phase succeeds when:

- every configuration value has parser and integration tests;
- custom and Bevy terrain paths select the same shadow filter;
- benchmark JSON completely identifies the rendered workload and quality settings;
- the existing default produces visual parity;
- native and wasm builds compile;
- `just fmt-check`, `just clippy`, `just test`, and `just test-full` pass.

Later optimizations become defaults only if a three-run comparison shows:

- median GPU-frame p50 improves by at least 5%;
- median GPU-frame p95 is no more than 5% worse;
- reserved GPU memory does not regress without an explicit design exception;
- visual probes show no holes, LOD seams, missing shadows, or unstable transitions.

A quality preset is not itself required to beat the current preset; it exposes a deliberate performance/quality tradeoff.

## 9. Failure behavior

- Invalid configuration: fail before render setup with a precise message.
- Unsupported AA or render feature: use the documented platform default and log one warning.
- Renderer counter unavailable: serialize `null` and list the metric as unavailable.
- GPU timestamp queries unavailable: preserve CPU frame metrics and mark GPU timing unavailable.

No failure may mutate simulation, rollback, protocol, or server state.

## 10. Out of scope

- Automatic quality selection by adapter name.
- Runtime settings UI and persisted user preferences.
- Dynamic resolution and temporal upscaling.
- Changes to terrain meshing, LOD data, brick residency, or network packets.
- Bevy version upgrades.
