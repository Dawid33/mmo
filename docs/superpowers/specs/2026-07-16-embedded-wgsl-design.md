# Embedded WGSL Shader Design

## Goal

Bundle the client renderer's WGSL shaders into the native and wasm binaries at compile time. Shader startup must not depend on the repository-level `assets/shaders` directory being present at runtime.

## Scope

Move these five production shaders into `crates/client/src/renderer/shaders/`:

- `far_voxel.wgsl`
- `voxel_pull.wgsl`
- `voxel_pull_cull.wgsl`
- `voxel_pull_depth.wgsl`
- `voxel_texture.wgsl`

Small synthetic WGSL modules used only to satisfy Bevy imports in shader-validation tests remain inline test fixtures. They are not application shaders and are not loaded at runtime.

## Architecture

The renderer plugin registers each file as a Bevy embedded asset during application startup. The registration macro includes the source bytes at Rust compile time and exposes each shader through the existing logical `shaders/<name>.wgsl` asset path.

Existing pipeline and material code continues to request those logical paths. This preserves Bevy shader preprocessing, dependency tracking, pipeline specialization, and readable diagnostic paths without introducing fixed global handles or duplicating `Shader::from_wgsl` construction across pipelines.

The far-voxel validation test reads the production shader through the same compile-time source rather than opening a repository-relative file. This proves the test does not accidentally retain a runtime filesystem dependency.

## Failure Behavior

Missing or invalid shader source fails during compilation or the existing shader-validation tests. At runtime, pipeline compilation and capability fallback behavior remains unchanged.

## Verification

- A focused source-layout test rejects production `AssetServer` filesystem loading and verifies every expected embedded shader registration.
- Existing far-voxel WGSL preprocessing and validation tests continue to pass.
- Client library, native client binary, and wasm client checks compile with the embedded paths.
- The native pull-smoke probe confirms that embedded forward, depth, and compute shaders build and submit indirect terrain draws.
- The former production files are absent from `assets/shaders`, preventing an unpackaged runtime copy from hiding an embedding regression.

## Non-Goals

- Changing WGSL behavior, bindings, entry points, shader definitions, or pipeline layouts.
- Embedding textures, block manifests, or other runtime assets.
- Replacing Bevy's shader cache or hot-reload system outside these production shaders.
