# Voxel Raster LOD Optimization — Design

**Date:** 2026-07-16  
**Status:** Approved design  
**Scope:** Portable raster-terrain geometry, culling, and distant-shading improvements.

## 1. Goal

Reduce raster terrain work after face pulling by eliminating false LOD boundaries, tightening visibility bounds, simplifying shading that cannot be perceived at distance, and optionally skipping whole face orientations before vertex pulling.

The phase applies to native and browser WebGPU. It preserves server-streamed LOD data, the locked 8-byte `QuadRecord`, the raster fallback, and all rollback boundaries.

## 2. Current limitations

- `LodNeighbors` contains optional same-batch entity references. Equal-factor cells arriving in different `LodData` payloads are treated as air at the mesh boundary, emitting redundant faces and skirts.
- Every raster LOD entity receives a constant 32×33×32 local AABB, even when its emitted geometry occupies a small part of the cell.
- AO participates in the greedy merge key at every factor, and every visible fragment samples the detailed repeated block texture.
- GPU-driven drawing emits one command for all six face orientations of a chunk. Fixed-function backface culling rejects backfaces only after record fetch and vertex reconstruction.
- The checked-in raster benchmark's HZB stage rejects only a small fraction of frustum survivors, consistent with overly broad bounds and coarse culling units.

## 3. Architecture and sequencing

Implement four independently selectable stages in this order:

1. global equal-factor LOD neighbor indexing;
2. tight emitted-geometry bounds;
3. factor-aware AO and material shading;
4. native GPU face-direction buckets.

Each stage has its own benchmark toggle until it passes the program-wide acceptance gate. Stages 1–3 are portable and intended to become defaults independently. Stage 4 is native-only; grouped-direct and per-chunk fallbacks ignore face ranges and continue drawing whole chunks.

## 4. Global LOD cell identity and neighbors

### 4.1 Identity

Attach a stable `LodCellKey` to every LOD render entity:

```rust
pub struct LodCellKey {
    pub factor: u32,
    pub cell: [i32; 3],
}
```

`cell` is the signed world-space cell coordinate at that factor, not an entity id, batch-local group, or floating transform. The LOD spawner derives it from `LodKey` coverage and the payload's group coordinate.

### 4.2 Index

Maintain a render-side `LodCellIndex: BTreeMap<LodCellKey, Entity>`. It owns no voxel data. On entity arrival, refresh, factor change, or removal it resolves all 26 offsets around the affected key.

`LodNeighbors` remains the meshing snapshot interface but is rebuilt from the global index. Only exact equal-factor neighbors qualify. Differing-factor adjacency continues to use skirts.

### 4.3 Dirty propagation

When a neighbor relationship changes, mark both cells for remeshing. Edge and corner neighbors matter because AO samples them; face neighbors also suppress shared faces. Removal first marks surviving neighbors dirty, then removes the dead entity from the index.

Missing or not-yet-ready data remains air. This may temporarily overdraw but may never create a hole. Repeated refreshes with unchanged keys must not create a remesh loop.

## 5. Tight geometry bounds

Meshing returns an optional local-space `QuadBounds` beside mesh/record output. Bounds are computed from final emitted quad corners after the LOD Y bias and skirt extension. Empty output has no bounds and no terrain component.

The main world attaches the exact `Aabb` to both `Mesh3d` and `ChunkQuads`. Extraction transforms it to world space exactly as today. Frustum, HZB, benchmark counters, and brick/raster handoff use the same bounds.

Bounds must remain conservative under negative coordinates, factor scaling, and skirts. If validation fails, fall back to the existing constant AABB and log once in debug builds.

No spatial subclustering is included initially. If tight bounds do not materially improve HZB rejection, 8³/16³ render clusters require a separate design because they alter meshing and draw granularity.

## 6. Factor-aware meshing and shading

Define named policy thresholds:

- factors 1, 2, and 4: existing AO and detailed texture sampling;
- factor 16: flat per-face AO (`[3; 4]`) so AO cannot split greedy merges; detailed texture sampling remains;
- factors 64 and 256: flat AO plus a precomputed average color per material/face instead of repeating the block texture.

`LodMeshingPolicy` is selected from `LodChunk.factor` and passed into the meshing task. The binary mesher's existing full-resolution entry point retains current behavior. The LOD entry point may omit AO from the merge key by writing the canonical flat value.

At texture load time, build:

- the existing texture array for near and intermediate terrain;
- mip levels for each array layer;
- a GPU material-face average-color lookup used by outer LOD fragments.

The vertex shader derives factor from the uniform scale in `chunk_transforms` and passes a flat shading tier to the fragment shader. The `QuadRecord` layout does not change. The per-chunk mesh fallback receives equivalent material specialization or remains disabled for factor-aware shading until parity exists.

Surface-only terrain conversion and palette reduction are excluded. They change visible content and require separate visual-error criteria.

## 7. Face-direction ranges

After meshing, order records into six stable contiguous ranges indexed by the locked `Face::index()` order. Attach:

```rust
pub struct ChunkFaceRanges(pub [FaceRange; 6]);
pub struct FaceRange {
    pub record_start: u32,
    pub quad_count: u32,
}
```

GPU metadata stores ranges relative to the chunk allocation. For the main camera, compute determines which orientations can face the camera relative to the chunk AABB and emits only those non-empty commands. For directional shadow views, selection uses the light direction. A camera inside an axis slab conservatively keeps both orientations for that axis.

Indirect-count mode compacts surviving face commands. Fixed-count mode reserves six command slots per stable chunk slot and writes zero vertex counts for rejected ranges. Every range is bounds-checked against the record arena before command emission.

The browser grouped-direct path performs one whole-chunk draw; issuing up to six direct draws per chunk is explicitly rejected because standard browser WebGPU lacks native multi-draw-indirect-count.

## 8. Data and ownership boundaries

- All new indexing, bounds, policies, lookup textures, and ranges live in `crates/client`.
- `game::lod`, packets, server cache entries, and packed LOD payloads do not change.
- Renderer-derived state is never hashed, journaled, or rolled back.
- The shared GPU record arena remains the owner of quad storage.
- The global LOD index stores entity identity only and is cleaned on retirement and despawn.

## 9. Verification

Required focused tests:

- cells from different LOD payloads become neighbors and remove their shared plane;
- removal restores exposed faces on the surviving cell;
- edge/corner arrival changes AO and dirties the correct cells;
- identical refreshes do not continuously remesh;
- tight bounds contain every decoded quad corner and include skirts;
- empty output carries no bounds;
- factor 16 ignores AO differences in its merge key while factors 1/2/4 retain them;
- factor 64/256 select average-color shading without changing record layout;
- six face ranges cover every record exactly once;
- camera and shadow face selection never reject a potentially front-facing orientation;
- indirect commands remain inside record/transform/command capacities.

Final verification includes native and wasm checks, shader validation, visual seam probes, and three-run production benchmarks.

Each stage becomes default only after the shared 5% p50 / 5% p95 acceptance gate. Neighbor indexing may become default with neutral GPU time if it materially reduces quad counts and fixes visible seams; this exception must be documented in the benchmark report.

## 10. Failure and fallback behavior

- Missing neighbor: mesh against air.
- Missing/invalid tight bound: use the constant cell AABB.
- Missing average-color lookup: use detailed texture sampling.
- Unsupported GPU-driven features: draw the whole chunk through grouped-direct.
- Invalid face range or arena bounds: skip that command, retain raster coverage next frame, and report a validation error.

All uncertainty resolves toward overdraw, never missing terrain.

## 11. Out of scope

- Changing LOD factors, ring distances, server streaming, or payload compression.
- Surface-only or heightfield terrain.
- Spatial meshlets/render clusters.
- Mesh shaders, bindless textures, and SVDAGs.
- Water and transparent terrain.
- Whole-chunk edit/remesh decomposition.
