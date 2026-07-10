# Voxel Rendering — Further Optimizations Deep Research (post-face-pulling)

**Date:** 2026-07-10
**Status:** Research notes (no decisions taken)
**Context:** Follow-up to `2026-07-09-voxel-rendering-next-steps-research.md`. That survey's
top pick (per-quad SSBO face pulling) is decided — implementation plan to follow once the
server-streamed LOD-rings work lands. This survey looked *past* it: GPU-driven
meshing/mesh shaders, raymarched sparse-voxel structures as an alternative to rasterized
meshes, texture/bindless status, streaming/residency, and production engine writeups
(2020–2026). Method: 5-angle web-search fan-out → 20 sources fetched → 99 claims
extracted → top 25 adversarially verified (3-vote panels, 2/3 refutes to kill):
21 confirmed, 4 refuted.

## Headline

The verified evidence splits cleanly:

- **Raymarched GPU voxel structures are now proven at open-world scale on mid-range
  GPUs** — but the state-of-the-art pipelines are static-scene-only and lean on GPU
  features browser WebGPU doesn't have, so they conflict with both editable terrain and
  the wasm32 target.
- **Mesh shaders and bindless are native-only for the foreseeable future** — any use is an
  optional desktop enhancement behind a browser fallback, never the primary path.
- **The editable-terrain-friendly, WebGPU-feasible winners** are quad-per-voxel far-field
  impostors (Project Ascendant) and brickmap-style raw 8³ brick storage — both compose
  with the existing meshed near field.

## Ranked shortlist (what's worth adopting after face pulling)

### 1. Quad-per-voxel far-field impostors (Ascendant-style) — top candidate

Project Ascendant (the vkguide author's large-scale voxel engine) renders far-field
terrain not as rasterized meshes but as **one quad (or point) per voxel**, with the pixel
shader doing an analytic ray/box intersection (Majercik et al., JCGT 2018) for exact
per-voxel hits, alongside quantized SurfaceNets meshes near the camera. [3-0, medium
confidence: single primary source]

- **Win:** replaces distant LOD-ring *meshing* entirely — no greedy meshing, no index
  buffers, no remesh cost for far rings; far-field data regenerates from worldgen exactly
  as our `worldgen_rings` already does.
- **WebGPU:** ✅ feasible today — needs only standard vertex/fragment shading.
- **Editable terrain / rollback:** orthogonal — far field is worldgen-derived,
  client-only, outside hashed sim state.
- **Caveats:** author notes a 3-quad fallback beats it at lower distances on low-compute
  GPUs (fill-rate cost of per-pixel ray/box); "production" is generous — a large personal
  engine, not a shipped commercial game.
- **Open question:** where the mesh/impostor handoff distance sits for our scenes, and
  the browser-GPU fill-rate cost vs greedy-meshed rings.

### 2. Brickmap / XBrickMap raw-brick GPU residency structure

Coarse brickgrid pointing at raw, uncompressed 8×8×8 bricks — the canonical
editable-terrain-friendly GPU voxel structure. Raw storage deliberately avoids SVO-style
hierarchical preprocessing, so GPU-resident data stays **directly editable (O(1) edits)**.
[3-0, high confidence]

- **Memory:** competitive with Laine–Karras ESVO on architectural scenes (100 vs 89 MB,
  Conference 2048³) and ~10× better on irregular/organic geometry (112 vs 1,157 MB,
  Hairball 2048³). The 3-level XBrickMap variant (sector grid → 4³ bricks → 8³ voxels) is
  ~O(1) with occasional sector-level reallocations.
- **Role for us:** not an immediate renderer change — it's the **substrate** if we later
  want GPU-resident voxel data for a raymarched far field, GPU lighting, or GPU occlusion,
  without breaking client-side edit application.
- **Caveats:** primary source is a 2015 thesis; its memory comparison methodology favors
  brickmaps (streaming-resident vs SVO static totals); its ray-throughput comparison was
  REFUTED 1-2 in verification — trust the memory/edit-cost characterization, not the
  speed tables. Modern editable-SVDAG work (Careil et al. 2020) shows compressed
  hierarchies *can* be edited interactively, just at higher cost.

### 3. Aokana-style chunked LOD streaming (ideas, not the pipeline)

Aokana (peer-reviewed, ACM PACMCGIT / I3D 2025) is the scale proof: ray-marches chunked
SVDAG scenes of ~10 **billion** voxels (64K resolution) at ~6 ms average on an RTX 3060 Ti,
pure compute pipeline (chunk frustum culling, 8×8-tile Hi-Z occlusion, indirect-dispatch
DAG ray marching into a visibility buffer), no triangle meshes; up to 9× less memory and
2–4.8× faster than HashDAG/Kämpe SVDAG baselines. [3-0, high confidence — authors' own
benchmarks, flat shading, average not worst-case]

Its **streaming design is the transferable part**: 256³-voxel chunks, octree-aggregated
LOD levels 0–8, camera-distance LOD error metric
(`LODError = ChunkSize × StreamingFactor − ‖ChunkCenter − Camera‖`), keeping only
**2–17% of on-disk scene data resident in VRAM** (San Miguel 64K: 424 MB VRAM vs ~23 GB
disk; ~8× less than HashDAG's fully-resident 3,475 MB). Conceptually matches our
server-streamed LOD rings — a template for GPU-side residency if ring data moves
GPU-resident.

**Two hard blockers rule out adopting the pipeline as-is** [3-0, high confidence]:

1. **No runtime voxel modification** — dynamic terrain is acknowledged future work with
   only sketched hybrids (dense array + rasterized meshes near player at LOD 0, SVDAG for
   LOD 1+, idle-time rebuild). Direct conflict with editable terrain + rollback.
2. **64-bit `InterlockedMax` visibility buffer** (24b depth + payload) — WGSL atomics are
   u32/i32 only; 64-bit atomicMax exists only as gpuweb proposal #5071 and native-only
   wgpu features. A wasm32 port needs a 32-bit repack redesign.

### 4. Wide 4³ trees ("Tree64") — the right tree *if* a raymarched path is ever adopted

Acceleration-structure choice for editable voxels is dominated by edit cost: flat grids
O(1), XBrickMap ~O(1), sparse trees ~O(log m) via path copying; trees/BVHs in turn suit
LOD better (chunks not fixed to a top-level grid). Among trees, 64-wide (4³ branching)
nodes consume roughly **half the memory of classic ESVO** (~0.62 vs ~1.02 B/voxel on a
358M-voxel scene) with shallower traversal. [3-0 but medium confidence — the memory figure
is from the VoxelRT author's self-described "rudimentary tests"; VoxelRT's throughput
*rankings* (StdBVH/Tree64 topping brickmaps) were REFUTED 1-2 — treat speed tables as
unverified.]

### 5. Mesh shaders / meshlets — native-only, park indefinitely

Merged in wgpu on all three native backends (Vulkan `VK_EXT_mesh_shader`, DX12 PR #8110,
Metal PR #8139, tracking #7197) but only as opt-in unstable
`Features::EXPERIMENTAL_MESH_SHADER` ("may have major bugs", breaking changes expected).
Absent from the W3C WebGPU standard (CR, March 2026 — no mesh stage) with no near-term
prospect (blocked on bindless, "likely 2026+"), and Naga's WGSL *output* backend is marked
"Unplanned/impossible" — browser WebGPU structurally cannot run them. [17-1 aggregate
across 6 claims, high confidence] Any meshlet renderer is native-desktop-only plus a full
separate browser path — not worth it for this project unless the native/browser split is
accepted for other reasons.

### 6. Bindless materials — native-only; browser path is texture arrays

All wgpu binding-array features (`TEXTURE_BINDING_ARRAY`, `BUFFER_BINDING_ARRAY`,
`PARTIALLY_BOUND_BINDING_ARRAY`, non-uniform indexing) are documented native-only;
browser WebGPU has only unlanded gpuweb proposals (bindless.md, sized-binding-arrays.md;
origin-trial momentum, nothing shipped stable). [3-0, high confidence] The voxel
texture/material system must be built on standard `texture_2d_array` or atlases with
traditional binding; bindless is at most an optional native upgrade.

## Cross-cutting: multi-draw-indirect on the browser

Dawn's `chromium-experimental-multi-draw-indirect` exists and its stated purpose is
exactly the compute-culling + pooled-buffer pattern ("rendering thousands if not millions
of objects in GPU driven rendering") — but it is not in the W3C spec (standardization
deferred post-v1, gpuweb #4349) and sits behind unsafe-webgpu flags. [3-0 on the purpose;
**two adjacent availability claims were refuted (0-3 and 1-2)** — even verified sources
disagree on exactly what is exposed where.] **Any MDI plan needs a direct
feature-detection spike, not documentation.** Standard-WebGPU fallback remains one
`draw_indirect` per orientation bucket.

## Refuted claims (do not cite)

- VoxelRT's primary-ray throughput rankings (StdBVH 215.9 / Tree64 208.9 vs XBrickMap
  165.6 Mrays/s) — 1-2.
- The 2015 brickmap thesis's 40–164 MRays/s beating-SVO comparison — 1-2.
- "Dawn exposes MDI as `wgpu::FeatureName::MultiDrawIndirect`, implementable today as an
  optional feature" — 0-3.
- "MDI is native-only in wgpu, full stop" (the clean inverse) — 1-2. The truth is the
  messy middle documented above.

## Coverage gaps (unresearched, not negative results)

Nothing survived verification on: transparency/water rendering, shadows at huge view
distances (CSM tuning, ray-marched shadows), horizon/atmosphere/fog, virtual
textures/clipmaps, TAA/upscaling, or the Teardown / Minecraft RTX / Distant Horizons /
Veloren / godot_voxel writeups (Teardown claims were extracted — per-object OBB raster +
volume-texture DDA raymarch with mip-based empty-space skipping — but fell below the
verification budget). These are candidates for a separate visual-quality-focused survey
when the pending visual pass (horizon, ring seams, shadows) gets planned.

## Time-sensitivity warning

All wgpu/WebGPU feature-status claims (mesh shaders, bindless, MDI, 64-bit atomics) are
as of mid-2026 and moving: mesh shaders shipped native-experimental in wgpu v28–v30 and
could stabilize; sized binding arrays and MDI have live gpuweb proposals and origin-trial
momentum. Recheck before committing to a fallback architecture.

## Open questions

1. Can Ascendant-style quad-per-voxel far-field impostors beat the current worldgen
   LOD-ring meshes on our actual scenes and on browser WebGPU (per-pixel ray/box
   fill-rate vs greedy-meshed rings), and where should the mesh/impostor handoff sit?
2. What does a standard-WebGPU fallback for GPU-driven drawing actually cost — one
   `draw_indirect` per orientation bucket vs Dawn's experimental multi-draw — measured on
   the pooled-buffer path already planned?
3. Is there a practical editable hybrid in Aokana's sketched direction (raw bricks/dense
   arrays near players + SVDAG/Tree64 for distant static LOD, rebuilt off the edit path)
   whose rebuild latency is compatible with rollback-driven client re-simulation?
4. Will sized binding arrays / bindless and 64-bit atomics reach stable browser WebGPU in
   2026–2027? That would reopen both the bindless material path and Nanite-style
   visibility-buffer voxel pipelines for wasm32.

## Primary sources

arxiv.org/abs/2505.02017 (Aokana, PACMCGIT/I3D 2025) — vkguide.dev/docs/ascendant/
ascendant_geometry — studenttheses.uu.nl 20.500.12932/20460 (brickmap thesis, 2015) —
github.com/dubiousconst282/VoxelRT + companion blog (Tree64, Oct 2024) —
gfx-rs/wgpu#3018, #7197, #3637, v29 mesh_shading.md, docs.rs Features —
dawn multi_draw_indirect.md — gpuweb#3015, #4349, #5071, sized-binding-arrays.md —
acko.net teardown-frame-teardown + juandiegomontoya teardown_breakdown (extracted,
unverified) — voxel-tools.readthedocs.io/performance — github.com/DouglasDwyer/octo-release —
bevy.org/news/bevy-0-16.
