# Voxel Hybrid Brick Renderer — Design

**Date:** 2026-07-16  
**Status:** Approved design  
**Scope:** Native-only factor-gated brick raymarching composed with raster LOD.

## 1. Goal

Turn the experimental brick renderer from an all-far-terrain replacement into a selective hybrid whose cost scales with genuinely distant coverage. Raster terrain remains authoritative coverage for near LOD factors and for every unsupported, failed, or not-yet-ready brick cell.

The brick backend remains opt-in until it meets stricter performance gates than ordinary raster optimizations.

## 2. Current baseline

`FarRenderBackend::BrickRaymarch` currently routes all far LOD cells through deduplicated 8³ bricks. The checked-in T600/NVK sample is substantially slower and more variable than raster: the far pass dominates GPU time, while reserved GPU bytes increase despite useful brick deduplication.

The current fragment path raymarches a 4³ brick grid, then up to 24 voxels within a non-empty brick, reconstructs AO with twelve neighboring occupancy/material samples, samples the detailed block texture, lights the hit, and writes explicit fragment depth. Full 32³ cell proxies use cell bounds rather than occupied-content bounds.

## 3. Architecture and rollout stages

Keep the public backend value `brick`, but redefine it as a hybrid policy:

- factors 2, 4, and 16 render through the existing raster LOD path;
- factors 64 and 256 are eligible for brick rendering;
- ineligible or failed cells always remain raster;
- eligible raster coverage retires only after brick data, GPU upload, pipeline, and draw coverage are ready.

Implement and benchmark these stages independently:

1. factor-gated coexistence and atomic handoff;
2. tight occupied bounds and proxy reduction;
3. factor-aware cheap brick shading;
4. half-resolution far target and depth-aware composite.

The automatic capability flag remains false until the complete hybrid passes its acceptance gate on every designated native adapter.

## 4. Admission policy

Define:

```rust
pub const BRICK_MIN_FACTOR: u32 = 64;

pub fn brick_admitted(factor: u32) -> bool {
    factor >= BRICK_MIN_FACTOR
}
```

Admission additionally requires existing device/pipeline/storage capabilities and residency budget. wasm32 always returns false and uses raster.

The LOD build queue may contain raster tasks for ineligible cells and brick tasks for eligible cells at the same time. Near full-resolution work retains its independent priority lane. Brick admission does not alter `LodVoxelData`, server subscriptions, or ring assignment.

## 5. Atomic raster/brick handoff

Eligible cells progress through explicit states:

1. raster coverage active;
2. brick CPU data building;
3. brick upload pending;
4. brick resident but pass not ready;
5. brick draw-ready;
6. raster coverage retired.

Refresh reverses the transition safely: keep old brick or raster coverage until the new revision is resident, then swap revisions. Removal retires raster and brick resources together. Upload rejection, capacity exhaustion, pipeline failure, stale revision, or timeout leaves/restores raster and records a fallback reason.

There must never be a frame where an admitted cell has neither raster nor brick coverage. Temporary double coverage is allowed only when depth testing makes it visually identical and the handoff lasts at most one completed frame.

## 6. Occupied bounds and proxies

Brick preparation computes conservative occupied voxel bounds for every root:

```rust
pub struct OccupiedBounds {
    pub min: [u8; 3],
    pub max_exclusive: [u8; 3],
}
```

All-air roots have no bounds and allocate no visible cell. Bounds include every uniform and mixed non-air brick and are deduplication-independent. `GpuFarCell` stores the occupied local bounds or equivalent world-space minimum/maximum.

Frustum culling and proxy generation use occupied bounds rather than the full 32³ cell. The DDA still addresses original cell-local voxel coordinates. Camera-inside handling remains two-sided and conservative.

This phase does not draw one proxy per occupied 8³ brick. That alternative increases instance count and is considered only if occupied cell bounds fail the benchmark gate.

## 7. Cheap distant shading

Brick-rendered factors use a deliberately cheaper policy than raster terrain:

- disable reconstructed corner AO for every admitted factor;
- factor 64 retains detailed texture sampling with mipmaps;
- factor 256 uses the material-face average-color lookup from the raster-LOD phase;
- keep one ambient plus directional Lambert term;
- do not receive or cast cascaded terrain shadows.

The shader selects the tier from `GpuFarCell.factor`. Disabling AO removes all twelve `sample_same_factor` lookups at a hit. Neighbor slots remain because they preserve future seam/normal behavior and may still be used by validation.

Missing average-color data falls back to texture sampling. The material identity and face selection stay identical to raster output.

## 8. Half-resolution far rendering

Add a separate far render target at `FAR_RENDER_SCALE = 0.5` per dimension. It stores HDR color and reverse-Z depth/linear hit distance. The far raymarch samples the full-resolution near depth conservatively over the corresponding 2×2 footprint and terminates at the nearest near-geometry sample.

A full-resolution composite pass:

- reads the half-resolution color/depth;
- selects and weights the nearest 2×2 samples using depth continuity;
- rejects samples crossing a near-depth edge;
- reconstructs reverse-Z depth for the selected far hit;
- writes color and `frag_depth` only where valid far coverage exists.

The first implementation is spatial and stateless. Temporal reprojection, motion vectors, checkerboarding, and history rejection are excluded until half-resolution spatial quality and cost are measured. UI and near terrain remain full-resolution.

`VOXEL_FAR_SCALE=1.0|0.5` is native-only and recorded in benchmark metadata. Full resolution remains the experimental brick default until half-resolution passes visual and performance gates.

## 9. Metrics

Extend brick metrics with:

- admitted and raster-fallback cells by factor;
- frustum-visible proxy instances;
- occupied/full cell volume ratio;
- brick pass resolution;
- cells rendered by texture and average-color tiers;
- raster/brick handoffs and fallback reasons;
- far-pass GPU p50/p95/p99;
- resident roots, referenced bricks, unique bricks, and reserved bytes.

Metrics that require GPU readback remain benchmark-only and asynchronous.

## 10. Verification

Required tests include:

- factors 2/4/16 stay raster while 64/256 enter brick build queues;
- wasm and missing capabilities admit no cells;
- every state transition preserves at least one coverage path;
- refresh and stale upload results cannot retire current coverage;
- occupied bounds contain all non-air voxels and exclude all-air margins;
- cull/proxy bounds transform correctly for factors 64/256 and signed origins;
- admitted shading performs no AO neighbor sampling;
- factor 256 selects average colors and factor 64 selects mipmapped textures;
- half-resolution near-depth reduction chooses the conservative nearest occluder;
- composite reconstruction does not bleed far color across near-depth edges;
- fallback reasons and factor counts serialize correctly.

Final checks include shader validation, native smoke probes, camera-inside-cell coverage, fast rotation, ring crossing, and three repetitions on every designated native adapter.

The hybrid remains opt-in until, compared with raster on each adapter:

- median GPU-frame p50 is no slower;
- median GPU-frame p95 is no more than 10% slower;
- no individual run shows the current large far-pass tail spikes;
- reserved memory stays within `FAR_STORAGE_BUDGET_BYTES` and is reported;
- visual probes show no coverage gaps, depth halos, or factor-boundary seams.

Passing on one adapter does not enable automatic selection globally.

## 11. Failure behavior

- Unsupported device or wasm: raster.
- Build/upload/residency failure: keep or restore raster for that cell.
- Invalid occupied bounds: use full cell bounds for that revision.
- Missing reduced-resolution resources: render brick coverage at full resolution.
- Composite pipeline unavailable: keep raster coverage; do not expose an uncomposited far target.
- Budget exhaustion: deterministic raster fallback by stable cell order.

Every failure is local to renderer-derived state and cannot affect server subscriptions or rollback.

## 12. Out of scope

- SVO/SVDAG/Tree64 replacement.
- Mesh shaders, hardware ray tracing, bindless resources, and 64-bit atomics.
- Per-brick proxy rasterization and screen-tile candidate binning.
- Temporal reconstruction and dynamic far resolution.
- Brick rendering on browser WebGPU.
- Water, transparency, volumetric lighting, and far shadow casting.
