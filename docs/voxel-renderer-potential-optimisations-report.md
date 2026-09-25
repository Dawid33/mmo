# Voxel Renderer Potential Optimisations Report

**Date:** July 16, 2026

**Scope:** Current shipping renderer described by `docs/voxel-rendering-architecture-report.md`, assessed against all eleven rendering-topic pages and every direct substantive technical reference they contain. The wiki source snapshot was upstream commit `a6b75ce9c06bb40d226e830415f1f90f8c281771` (June 5, 2026). The [companion reference audit](voxel-wiki-rendering-reference-audit.md) records coverage and access failures.

## Executive summary

The renderer has already adopted almost all of the category's concrete high-value techniques:

- binary greedy surface extraction with neighbor padding;
- bit-packed geometry;
- one storage-buffer record per greedy quad;
- shader-generated vertices (face/vertex pulling);
- pooled geometry;
- GPU frustum and HZB culling;
- multi-draw indirect with an indirect-count path;
- streamed LOD;
- a hybrid raster/brick-DDA far renderer.

The next useful work is therefore refinement, not a new rendering architecture. The strongest opportunities are:

1. eliminate false equal-scale LOD boundaries across separate payloads;
2. use exact emitted-geometry bounds, then test smaller render clusters only if HZB rejection remains poor;
3. reduce AO and texture work at factors where it is sub-pixel;
4. add mipmapped/explicit-LOD material sampling for distant minification;
5. reject whole back-facing orientation ranges before vertex pulling;
6. reclaim trailing arena slots and compact only when fragmentation is demonstrably costly;
7. halve mixed-brick material storage with a render-material palette, if the active palette fits in 8 bits;
8. select raster or brick rendering per far cell from measured content and projected-cost signals;
9. make meshing/upload budgets frame-time-aware while preserving bounded queues and near-first priority.

The existing 8-byte `QuadRecord`, CPU binary greedy mesher, Bevy/wgpu abstraction, and block-exact DDA should remain. A 4-byte record, direct Vulkan/OpenGL backend, SDF conversion, SVO/SVDAG replacement, or wholesale GPU meshing does not have a persuasive benefit for this renderer without new benchmark evidence.

## Baseline and evaluation method

The current architecture was taken from `docs/voxel-rendering-architecture-report.md` and spot-checked against the implementation in:

- `crates/client/src/renderer/meshing/`;
- `crates/client/src/renderer/voxel_pull/`;
- `crates/client/src/renderer/lod_rings.rs`;
- `crates/client/src/renderer/far_voxel/`;
- `assets/shaders/voxel_pull*.wgsl` and `assets/shaders/far_voxel.wgsl`.

Recommendations are ranked by likely benefit, implementation risk, portability, and how directly the wiki material applies. "Likely impact" is a hypothesis to validate, not a claimed benchmark result.

The audit followed direct, one-hop technical references from each article body. Papers were read from the linked original or an accessible mirror; repositories were assessed from their relevant documentation and implementation; books and tutorial portals were assessed at the referenced destination rather than recursively following their own bibliographies. Site chrome, social links, donation pages, and repeated navigation links were excluded. Dead originals were checked through archives, mirrors, indexed records, or current upstream source. Vertex Pooling had no recoverable article body, and two current Inigo Quilez pages blocked automated access; no recommendation depends uniquely on those pages.

## Priority matrix

| Priority | Optimisation | Likely impact | Cost / risk | Portable | First metric |
|---|---|---:|---:|---:|---|
| P0 | Global equal-factor LOD neighbor index | High geometry reduction in streamed boundaries | Medium | Yes | quads by factor and boundary faces |
| P0 | Tight emitted bounds | Medium-high culling and overdraw reduction | Low-medium | Yes | frustum/HZB rejection and fragments |
| P0 | Factor-aware far shading | High when far pass is fragment-bound | Medium | Yes | far GPU time and texture samples/fragments |
| P0 | Mipmapped/explicit-LOD material sampling | Medium-high quality and cache win at distance | Medium | Yes | moiré, texture samples, far GPU time |
| P1 | Six face-orientation ranges | Medium vertex-work reduction | Medium | Native GPU path | records/vertices rejected before pull |
| P1 | Arena tail trimming and thresholded compaction | Medium after churn; low in stable scenes | Low then medium | Yes | high-water/live ratio and bytes reserved |
| P1 | 8-bit render-material IDs in mixed bricks | Up to 47% mixed-brick storage reduction | Medium | Native brick path | unique mixed bricks and resident bytes |
| P1 | Per-cell raster/brick choice | Potentially high across varied GPUs/content | High | Raster fallback remains portable | GPU time by factor/occupancy class |
| P2 | Frame-time-aware work budgets | Better p95/p99 during joins and rollback bursts | Medium | Yes | frame spikes and ready latency |
| P2 | Feedback-guided far-brick priority/residency | Workload-dependent streaming win | High | Native brick path | request usefulness, cache hit rate, pop-in |
| P2 | Smaller render clusters for sparse LOD cells | Workload-dependent | High | Yes | HZB benefit versus metadata/draw cost |
| P2 | Projected-error LOD with hysteresis | Medium, but wide server/client scope | High | Yes | visible error, resident cells, churn |

## Recommended optimisations

### 1. Connect equal-factor LOD cells across payload boundaries

`spawn_lod_root` currently wires `LodNeighbors` only within one payload. Its own comment states that cross-key borders are treated as air and emit unculled walls. This causes redundant geometry, extra record uploads, more vertex pulling, and avoidable depth work.

Add a global render-derived index keyed by `(factor, world cell coordinate)`. On arrival, refresh, and removal, rebuild the affected 26-neighbor relations and remesh both sides. Only equal-factor cells should suppress shared faces; different factors continue to use skirts. Missing data must remain air so the failure mode is temporary overdraw, never a hole.

This is the most direct surface-extraction win because it removes geometry before every later stage. It also makes the existing HZB and multidraw stages cheaper rather than adding another culling layer. The implementation is already designed, but not present in shipping code, in `docs/superpowers/specs/2026-07-16-voxel-raster-lod-optimization-design.md`.

Acceptance signal: a material reduction in LOD quad count and shared-border faces. Neutral GPU time is acceptable if it also fixes visible seams; otherwise use the normal benchmark gate.

### 2. Replace constant LOD AABBs with exact emitted bounds

Return bounds alongside meshing output and attach the same conservative AABB to mesh and pulled-record backends. Bounds must include skirt vertices, transform bias, negative coordinates, and scale. Empty outputs should have no terrain draw component.

The current culling unit is a whole 32³ cell even when geometry occupies a small fraction of it. Tighter bounds improve CPU frustum tests, GPU frustum tests, HZB mip selection, and proxy coverage without changing visible content. This follows the category's general instruction to reduce submitted geometry and overdraw, and is lower-risk than changing chunk size.

Only if exact bounds leave HZB rejection weak should the renderer test 16³ or 8³ render clusters. Smaller clusters improve occlusion granularity but increase metadata, allocations, culling invocations, and commands. Treat that as a separate benchmark experiment rather than an assumed win.

### 3. Make distant meshing and shading factor-aware

The current far raster and brick paths retain detailed AO and repeated texture sampling at all factors. The brick fragment shader also reconstructs corner AO and loops over directional lights for every hit. At factors 64 and 256, much of this work is below pixel resolution.

Recommended tiers:

- factors 1, 2, and 4: retain current AO and texture sampling;
- factor 16: use flat AO so AO discontinuities no longer split greedy rectangles;
- factors 64 and 256: flat AO plus a precomputed average color per material and face;
- brick mode: apply the same tier policy before performing neighbor AO samples or a texture lookup.

This can reduce both geometry (larger greedy rectangles) and fragment cost. Mipmapped texture-array layers should still be used at intermediate distance. The policy must preserve identical material/face selection between raster and brick paths.

Acceptance signal: at least 5% median GPU-frame improvement, p95 no more than 5% worse, and no unacceptable visual difference in the production-scene comparison.

### 4. Add a real texture-minification path

The block texture array currently has one mip level and a nearest sampler. At distance, repeated high-frequency texels and AO patterns are undersampled, producing shimmer and moiré while spending bandwidth on detail that cannot survive projection.

Generate per-layer mips with alpha-safe filtering and select them explicitly or through valid texture gradients. Keep nearest sampling at the near aesthetic tier, then benchmark mipmapped sampling at factor 16 and average-color/material-face lookup at factors 64 and 256. Include oblique surfaces, repeating checker textures, camera motion, and AO discontinuities in the visual suite; anisotropic filtering is a follow-up only if oblique minification remains unacceptable.

This is distinct from reducing shader complexity: it fixes sampling correctness and may improve cache behavior even where full material texture sampling remains enabled.

### 5. Cull face orientations before vertex pulling

Order each chunk's records into six contiguous face ranges. For a camera or directional-shadow view, conservatively retain only orientations that can face the view relative to the chunk AABB. A camera inside an axis slab must keep both directions for that axis.

Fixed-function backface culling already prevents rasterization, but it happens after record fetch and vertex reconstruction. Orientation ranges can therefore remove up to roughly half of upstream vertex work; they do not reduce the cost of genuinely front-facing fragments. This is most useful in the native indirect-count path, where surviving ranges can be compacted into the existing multidraw buffer.

Do not issue up to six direct draws per chunk in the grouped-direct/browser fallback. The extra submissions can cost more than the vertex work they save. That path should continue to draw whole chunks.

### 6. Reclaim arena high-water waste before building a full compactor

The record and stable-slot allocators reuse freed ranges but never lower their high-water marks. GPU culling dispatches over `slot_allocator.high_water()`, and geometric command/metadata buffers follow that capacity. Long edit, streaming, or teleport sessions can therefore scan inactive trailing slots and retain peak memory indefinitely.

Implement in two stages:

1. cheap tail trimming when the highest record range or slot becomes free;
2. optional incremental or out-of-place idle-time compaction only when measured fragmentation crosses a threshold, such as reserved/live records greater than 1.5 for a sustained interval.

Compaction must patch record starts and re-upload affected ranges, so running it frequently would trade memory for upload spikes. A linked D3D11 vertex-atlas implementation reports rebuilds on the order of hundreds of milliseconds for tens of megabytes, which rules out a monolithic in-frame rebuild. Expose live records, record high-water, live slots, slot high-water, largest free range, and reserved bytes in diagnostics before deciding the threshold.

### 7. Compress mixed-brick materials through a render palette

Each `MixedBrick` currently stores 64 bytes of occupancy and 1,024 bytes of 16-bit material IDs, including entries for empty voxels: 1,088 bytes total. If the active render-material palette contains at most 256 entries, remap `BlockId` to a renderer-local `u8` and pack four IDs per `u32`. A mixed brick then uses 64 + 512 = 576 bytes, approximately 47% less, before deduplication.

The palette is renderer-derived state and must not replace authoritative `BlockId` in simulation, networking, hashing, or rollback. If a world exceeds 256 simultaneously renderable materials, retain the 16-bit format or introduce a separately benchmarked per-brick palette. Keep occupancy separate so empty checks remain a single cheap bit test.

This directly targets the raytracing article's two central concerns: sparse storage and traversal bandwidth. It is preferable to an SVO/SVDAG conversion because it preserves the current brick update and fallback model.

After the fixed-width `u8` format is measured, test an occupied-only material stream as a separate experiment: 64-byte occupancy, compact `u8` values only for set bits, and popcount/rank lookup. Both brickmap theses use occupancy-separated attributes. It can beat 576 bytes for sparse mixed bricks, but introduces variable allocation, fragmentation, and extra lookup work, so it is not the first implementation.

### 8. Select raster or brick rendering per far cell

The wiki correctly treats brickmaps, SVOs, and SVDAGs as trade-offs among traversal, edits, and memory rather than universal upgrades. The current fixed 8³ brick structure already supplies coarse empty/uniform skipping and block-exact local DDA. Its relative cost will vary sharply with projected size and the number of mixed bricks.

Extend benchmark telemetry to classify cells by:

- factor and projected screen area;
- empty, uniform, and mixed brick counts;
- unique-brick reuse;
- raster quad count;
- raster and brick GPU time where isolated measurement is possible.

Use those results to derive a conservative per-cell or per-factor policy. Smooth, highly mergeable terrain may be cheaper as raster quads; sparse or highly detailed cells may favor brick traversal. Preserve raster residency until the selected brick representation is acknowledged as drawable, and retain capability/budget fallbacks.

Do not add another hierarchy inside a 32³ cell yet. The existing coarse DDA crosses at most the 4³ brick grid before an 8³ local trace; a tree adds dependent reads and divergence. Consider a wider hierarchy only if traversal counters show mixed-brick steps, rather than shading, dominate the far pass.

### 9. Adapt meshing and upload budgets to frame headroom

The fixed limits of 64 in-flight near tasks and 32 uploads per lane bound work, but they do not account for device speed or current frame pressure. A high-end machine can drain joins faster; a busy or low-end machine can still suffer apply/upload spikes.

Keep deterministic near-first/in-frustum ordering, cancellation, and hard maxima, but add a render-only time controller:

- lower new task launches and uploads after frame time exceeds the target;
- increase gradually when sustained headroom exists;
- reserve a non-zero near budget so distant work cannot starve edits;
- keep far work in its separate lane;
- report queue age and time-to-render-ready so smooth frames do not hide excessive pop-in latency.

Because renderer state is disposable and excluded from simulation determinism, adaptive scheduling does not affect authoritative results.

### 10. Evaluate feedback-guided far-brick priority only after local wins

The 2015 brickmap thesis and GigaVoxels both use visibility-generated requests, bounded production batches, temporal coherence, and a lower-detail fallback while full data is absent. A later experiment could use GPU or CPU visibility feedback to prioritize already-authorized far-cell payloads and evict cold render-derived bricks with an LRU-like policy.

This must not change server authority, correctness, or ring membership. Requests need deduplication and bounded readback; missing detail must fall back to existing raster/LOD content. Measure request usefulness, latency, residency hit rate, cache churn, and visible pop-in before considering it preferable to the current deterministic near-first scheduling.

### 11. Evaluate projected-error LOD only as a larger streaming change

The current fixed Chebyshev rings are simple and deterministic, but they do not account for field of view, viewport height, terrain occupancy, or projected cell size. A screen-space error metric with hysteresis could reduce resident and drawn far data while avoiding oscillation near thresholds.

The current upstream LOD page supplies the general perspective, screen-space, popping, and moiré rationale, but no voxel-specific ring, aggregation, or seam algorithm. This change also crosses client/server streaming and replacement semantics, so it should follow the local renderer refinements above. Any design must retain bounded factor transitions, crack coverage, pop-resistant replacement, and stable behavior under rapid camera motion.

## Optimisations that are already complete

### Binary greedy surface extraction

The linked [binary-greedy reference](https://github.com/cgerikj/binary-greedy-meshing) uses 64-bit occupancy masks, bitwise hidden-face computation, greedy merging, neighbor data, 8-byte quads, vertex pulling, face-orientation rejection, and multidraw. The current renderer already matches most of that destination. Its next gains are in boundary completeness, scheduling, culling, and shading—not changing mesher family.

GPU meshing should remain deferred. Greedy rectangle construction contains bookkeeping and data-dependent merging, updates are already asynchronous, and a GPU path would add staging, synchronization, cancellation, and browser portability complexity. Revisit only if traces show CPU meshing is the sustained bottleneck after adaptive scheduling.

### Vertex packing, face pulling, and vertex pulling

Voxel.Wiki's vertex-pulling example stores one record per face and generates six triangle-list vertices from `vertex_id / 6` and `vertex_id % 6`; that is effectively the current renderer's face-pulling design. Storage-buffer fetch performance remains hardware- and layout-dependent, so coherent contiguous records and aligned 32-bit loads matter more than another abstraction rewrite.

The 8-byte `QuadRecord` is close to the useful minimum for this feature set. Position, face, width, height, four AO values, diagonal, and material layer do not fit cleanly in 32 bits. A 4-byte record would require dropping information, severely constraining ranges, or adding secondary fetches. Keep 8 bytes unless a complete alternative format measures better on target adapters.

### Pooled geometry and multi-draw indirect

The [Multidraw](https://voxel.wiki/wiki/multidraw/) article's chain—pooled geometry, indirect commands, vertex pulling, and GPU-driven culling—is already implemented. The renderer also uses indirect-count compaction when supported and zeroes rejected fixed slots otherwise. The remaining work is orientation granularity and high-water control, not a new submission backend.

Add submission telemetry before changing it: indirect commands per multidraw, batch breaks, zero-command ratio, compacted-command ratio, fallback frequency, and commands per visible chunk. These reveal whether submission or unused command capacity is actually material.

### Brick DDA

The [Raytracing](https://voxel.wiki/wiki/raytracing/) article recommends Amanatides–Woo-style grid traversal and exploiting homogeneous or empty space through brickmaps, SVOs, or SVDAGs. The far backend already uses cell/brick DDA, empty/uniform roots, mixed-brick deduplication, frustum culling, a depth-prepass limit, and raster fallback. More precisely, it is **raysterization**: rasterized cell proxies restrict fragment coverage, then fragment DDA finds the exact voxel hit. It is aligned with the editable-world-friendly option and should retain proxy coverage rather than becoming a full-screen ray dispatch.

## Rejected or deferred directions

### Direct OpenGL or Vulkan backend

[OpenGL](https://voxel.wiki/wiki/opengl/) and [Vulkan](https://voxel.wiki/wiki/vulkan/) are stubs containing general API references, not voxel optimisations. Bevy/wgpu already provides the portability boundary recommended by the rendering overview. A direct backend would duplicate resource, pipeline, and synchronization ownership and should require proof of a specific abstraction bottleneck.

### Signed distance fields as terrain replacement

The [SDF page](https://voxel.wiki/wiki/signed-distance-fields/) only defines a distance field and links general references. SDFs suit smooth implicit surfaces and sphere tracing, but the game needs block-exact silhouettes, discrete material identity, edits, and streamed LOD. A stored SDF adds generation, bandwidth, update, and conservative-distance correctness problems. At most, conservative distance metadata could become an empty-space-skipping experiment after profiling proves DDA traversal dominates.

### SVO/SVDAG replacement

Trees and DAGs can compress large static scenes, but edits, path copying, GPU pointer chasing, divergence, residency, and fallback behavior make them a poor default for this editable, rollback-driven MMO. The fixed brickmap is simpler and already transactional. Test material compression and per-cell backend selection first.

### More aggressive record compression

The wiki's four-byte face example stores only a position. It is not comparable to a greedy quad carrying extents, face, AO, diagonal, and material. Sub-byte or cross-record encodings can reduce bandwidth while increasing address arithmetic, extra loads, or divergence. No change should proceed without GPU measurements on multiple adapter classes.

### Smooth-surface extraction and advanced GI

Marching Cubes, Dual Contouring, Transvoxel, and their manifold/adaptive variants solve smooth scalar or Hermite terrain extraction, not the project's block-exact greedy surfaces. LPV, voxel cone tracing, radiance cascades, SVGF, ReSTIR, and path tracing solve global or stochastic lighting, not deterministic primary visibility cost. They become relevant only if the visual specification changes; temporal variants would also need explicit invalidation for edits, camera cuts, disocclusion, rollback, and LOD replacement.

### Geometry-format and culling experiments that need evidence

The binary-greedy implementation notes T-junction mitigation by slightly expanding quads and using eye-space positions. Add a camera-motion and extreme-distance T-junction stress scene, but do not expand geometry unless an artifact is reproduced. Likewise, benchmark analytic projected-sphere bounds as an optional HZB proxy against the exact emitted AABB; spheres can be cheaper to project but may be looser for cell-shaped terrain and need conservative camera-inside/near-plane handling.

A four-unique-vertex indexed representation is not an obvious win here: the current shader uses vertex index for the record/corner and instance index for the chunk transform, while a global index stream adds bytes per quad. Keep six generated vertices until a complete alternative preserves transform lookup and measures better.

## Benchmark programme

Use the existing `just render-bench backend=<pair>` and diagnostics rather than adopting optimisations on theoretical counts alone. Add counters needed to explain results:

- quads by near/LOD factor and faces removed by global neighbors;
- exact-bound versus legacy-bound volumes;
- frustum survivors, HZB survivors, orientation ranges submitted, and pulled vertices;
- commands per multidraw, batch breaks, zero-command/compaction ratios, and fallback frequency;
- record/slot live count, high-water count, fragmentation, and reserved bytes;
- per-factor raster cells, brick cells, mixed bricks, deduplication ratio, and palette width;
- meshing queue age, task duration, uploads per frame, and time-to-render-ready;
- far-pass fragment invocations and GPU time by shading tier.

Run at least three repetitions per backend on representative native adapters and the browser fallback. Compare medians of run-level p50 and p95 summaries. Use the existing default acceptance gate: median GPU-frame p50 improves by at least 5%, while p95 is no more than 5% worse. Geometry correctness or visible seam fixes may be accepted with neutral GPU time if the exception is documented.

Recommended experiment order:

1. global LOD neighbors;
2. tight bounds;
3. factor-aware raster and brick shading plus mipmapped minification;
4. face-orientation ranges;
5. arena tail trim and telemetry;
6. 8-bit brick material palette, then occupied-only attributes;
7. per-cell raster/brick policy;
8. adaptive frame budgets;
9. feedback-guided residency;
10. render clusters or projected-error LOD only if earlier counters justify them.

## Complete Voxel.Wiki category coverage

All eleven rendering-topic links on the category page were reviewed:

| Page | Substance | Outcome for this renderer |
|---|---|---|
| [Rendering](https://voxel.wiki/wiki/rendering/) | GPU architecture, packing, extraction, visibility, raster/ray hybrids, and extensive lighting families | Supports the existing hybrid; lighting references are future feature work |
| [Surface Extraction](https://voxel.wiki/wiki/surface-extraction/) | Greedy/binary greedy plus Marching Cubes, Dual Contouring, manifold and LOD-transition methods | Current block mesher family is appropriate; smooth methods are out of scope |
| [Raytracing](https://voxel.wiki/wiki/raytracing/) | DDA, sparsity, brickmaps, SVOs, SVDAGs, BVHs | Validates brick DDA; emphasizes measured structure trade-offs |
| [OpenGL API](https://voxel.wiki/wiki/opengl/) | General API, pipeline, shader, buffer, and debugging references | No renderer change |
| [Vertex Packing](https://voxel.wiki/wiki/vertex-packing/) | Bit packing and cheap shader unpacking; otherwise a stub | Already implemented; keep aligned 8-byte records |
| [Face Pulling](https://voxel.wiki/wiki/face-pulling/) | One face record, shader-generated corners; implementation is a stub | Already implemented |
| [Level Of Detail](https://voxel.wiki/wiki/level-of-detail/) | Perspective, general LOD, and moiré/minification references | Supports projected-error experiments and texture filtering, not a voxel-specific policy |
| [Multidraw](https://voxel.wiki/wiki/multidraw/) | Pooled geometry, fewer draw calls, indirect/GPU-driven direction | Already implemented; refine granularity and capacity |
| [Vulkan API](https://voxel.wiki/wiki/vulkan/) | Specification, hardware, tutorials, render graphs, multithreading, and portability | Stay with Bevy/wgpu |
| [Vertex Pulling](https://voxel.wiki/wiki/vertex-pulling/) | SSBO records indexed by generated vertex ID | Already implemented |
| [Signed Distance Fields](https://voxel.wiki/wiki/signed-distance-fields/) | Definition and analytic formula catalogs | Not a block-terrain replacement |

The category page also contains navigation, project, community, privacy, and site-generator links. They do not contain rendering techniques and are outside the technical article set summarized above.

## Conclusion

The renderer does not need another foundational rewrite. Its core geometry and submission architecture is already unusually close to the strongest concrete material linked by Voxel.Wiki. The best return should come from removing work that the present architecture still admits: false LOD boundary faces, loose bounds, imperceptible far-detail shading, back-facing pulled records, inactive arena tail capacity, and oversized brick materials.

Implement those as independently switchable experiments, make the new counters explain each result, and keep the current conservative fallback rule: uncertainty may cause temporary overdraw, never missing terrain.
