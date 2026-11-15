---
name: wasm-webgl2-per-chunk-fallback
description: wasm client builds with Bevy webgl2 (no storage buffers); pull path cannot run there, VoxelPullPlugin::finish downgrades to PerChunk meshes
metadata:
  type: project
---

2026-09-20: the wasm client panicked at startup — `voxel_pull_chunk_layout` needs `DownlevelFlags::VERTEX_STORAGE`, and the wasm build uses Bevy's `webgl2` feature (zero storage buffers). `TerrainRenderConfig::resolve` hardcoded wasm → `GroupedDirect` as "portable"; it never was (the "wasm browser check pending" in [[voxel-rendering-phase1-landed]] / [[rendering-next-steps-research]] was this).

Fix (uncommitted on develop at time of writing): `TerrainRenderConfig::for_device` + `device_supports_quad_records` probe in `VoxelPullPlugin::finish` → main-world config drops to `NearRenderBackend::PerChunk`, pull pipelines are not created, their render systems are `run_if(resource_exists)`. Far-voxel pass was already cfg'd out on wasm. `RenderPath` no longer exists (replaced by `renderer/config.rs` in 4e0c739).

**Why:** browser therefore renders near terrain + LOD rings as 56 B/quad Mesh assets, the heaviest backend — browser perf/memory is NOT comparable to native numbers.

**How to apply:** any new render feature that binds storage buffers must be device-gated, not platform-gated. Real browser perf lever = a WebGPU wasm build (wasm default stays GroupedDirect so it would pick the pull path up automatically) or a data-texture (texelFetch) pull variant. Browser run of the fix was not verified in-session.
