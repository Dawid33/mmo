# Voxel.Wiki Rendering Reference Audit

**Audit date:** July 16, 2026

**Wiki source:** `voxel-wiki/voxel-wiki` commit `a6b75ce9c06bb40d226e830415f1f90f8c281771` (June 5, 2026)

**Purpose:** Traceability for the recommendations in [Voxel Renderer Potential Optimisations Report](voxel-renderer-potential-optimisations-report.md).

## Scope and method

The audit covers all eleven pages listed in Voxel.Wiki's Rendering category and every direct, substantive technical destination in those article bodies. It includes ordinary links, bare URLs in reference lists, footnote-defined papers, previous/next technical chapters, and accessible mirrors of dead originals. Repeated URLs within an article were deduplicated. The 181 count below is page-local: a destination repeated by two articles remains represented in both source groups.

Papers were read in full or through the complete relevant technical sections; repository references were checked through their documentation and relevant implementation; API/tutorial/book portals were read at the directly referenced destination. This is a one-hop audit, not a recursive crawl of every cited paper's own bibliography. Navigation, author profiles, social/community, commerce-only duplicates, donation, privacy, and site-generator links were excluded.

| Source group | Unique direct destinations | Audit result |
|---|---:|---|
| Rendering | 88 | Read from originals, mirrors, current source, or indexed archival material |
| Surface Extraction, Vertex Packing, Face Pulling, Vertex Pulling | 27 | 26 readable; Vertex Pooling body unavailable |
| Raytracing, Level Of Detail, Signed Distance Fields | 25 | 23 readable; two Inigo Quilez formula pages blocked automated access |
| OpenGL | 20 | Read; commerce duplicates excluded |
| Multidraw | 7 | Read; repeated specification/talk links deduplicated |
| Vulkan | 14 | Read |
| **Total page-local destinations** | **181** | **178 read; 1 unavailable body; 2 blocked** |

“Read” includes a current upstream wiki source or an authoritative/archived mirror when the exact deployed URL was dead. No optimisation recommendation depends uniquely on the unavailable or blocked pages.

## Rendering

### GPU, API, abstraction, and windowing references

Reviewed: GPU history and GPGPU applications; graphics cards; Steam Hardware Survey; GPUinfo; Direct3D/DirectX; Metal; language bindings; OpenGL and Vulkan binding indexes; wgpu and its backend matrix; bgfx and its backend matrix; OGRE; raylib; libGDX; SDL and its platform FAQ; GLFW and its platform FAQ; SFML and its platform FAQ; Winit and its feature matrix; rasterisation; OpenGL vertex-array objects; GPU parallelism.

Outcome: the existing Bevy/wgpu boundary is appropriate. The linked material supplies no evidence that a direct OpenGL or Vulkan backend would outperform the abstraction enough to repay duplicated resource, synchronization, portability, and shader ownership.

### Rendering architecture, extraction, visibility, and packing references

Reviewed: Surface Extraction; Volume Marching; Raycasting; Raysterization; Level Of Detail; Culling; Frustum Culling; Occlusion Culling; Portal Culling; Vertex Packing; Vertex Pulling; Multidraw; Sparse 64-Tree; cellular automata; 0fps *Meshing in a Minecraft Game*; 0fps voxel ambient occlusion; GigaVoxels; both Johannes Jendersie *Rendering huge amounts of Voxels* articles; Procedural World's *From Voxels to Polygons*; both Nick Gildea Dual Contouring articles; the archived Codeflow OpenGL 4 voxel experiment; the GameDev StackExchange voxel-rendering discussion; Inigo Quilez sphere projection; LearnOpenGL shadow mapping; and Valve's Source-engine shading paper.

Outcome: the current renderer already combines the strongest relevant ideas: binary greedy extraction, packed pulled faces, pooled multidraw, GPU visibility, and raster-proxy brick DDA. GigaVoxels and the 2015 brickmap thesis support a later bounded visibility-feedback/residency experiment. Analytic sphere projection is only a benchmark candidate against exact emitted AABBs.

### Basic lighting, visibility, radiosity, and photon mapping references

Reviewed: light transport theory; spectral rendering; the archived Seed of Andromeda flood-fill article; breadth-first search; Oliver Hare grid light propagation; the grid-visibility overview; radiosity; Hugo Elias's archived radiosity article; Stanford and CMU radiosity/photon-mapping lectures; Stony Brook radiosity/photon mapping; Valve's self-shadowed radiosity normal mapping; GPU Gems 2 progressive-refinement radiosity; and BRDF background.

Outcome: baked/flood-filled lighting can remain useful for immutable or slowly changing data, but arbitrary voxel edits require invalidation. None of these sources changes the primary visibility recommendations.

### Light Propagation Volumes

Reviewed: BlackHC LPV; CryEngine 3 LPV; cascaded LPV with spherical radial basis functions; cascaded LPV for indirect illumination; real-time multiple scattering with LPV; Convergent Light Volumes; Octree LPV; Deferred Voxel Shading; and Delta LPV for mixed reality.

Outcome: LPV is a lighting feature with leakage, propagation, hierarchy, and update trade-offs—not a current renderer optimisation. If dynamic GI is commissioned later, it needs a separate quality/performance design.

### Radiance Cascades

Reviewed: the Radiance Cascades hub; both GM Shaders tutorials; the fundamental-radiance-cascades article; and the interactive proof of concept.

Outcome: promising screen-/probe-space GI research, but unrelated to current geometry submission and deterministic primary-hit DDA. Temporal state would require edit, rollback, disocclusion, camera-cut, and LOD invalidation.

### Voxel Cone Tracing

Reviewed: Crassin et al. voxel cone tracing; the Bremen thesis; *The Tomorrow Children* cascaded voxel cone tracing; Leif Node's implementation; Delta Voxel Cone Tracing; the KTH thesis; and the portfolio implementation.

Outcome: if GI is later required, dense clipmapped/toroidal radiance volumes are a more natural first experiment than adopting an editable SVO solely for lighting. VCT is not placed on the present optimisation roadmap.

### Path tracing

Reviewed: path-tracing background; NVIDIA's overview; Jensen's photon-mapping book material; coherent grid traversal for interactive ray tracing; bidirectional path tracing; Cyberpunk 2077 ReSTIR integration; production volume rendering; and the path-tracing lecture deck.

Outcome: these techniques address stochastic lighting. SVGF/ReSTIR/denoising should not be added to the deterministic primary-ray pass.

## Surface Extraction, packing, and pulling

### Surface extraction

Reviewed: polygon meshes; rasterisation; Marching Cubes; Extended Marching Cubes; Dual Marching Cubes; Cubical Marching Squares; isosurfaces over simplicial partitions of multiresolution grids; Dual Contouring of Hermite Data; Manifold Dual Contouring; Adaptive Skeleton Climbing; Transvoxel; greedy meshing; and binary greedy meshing.

Outcome: Marching Cubes, Dual Contouring, manifold variants, and Transvoxel target smooth scalar/Hermite surfaces. They do not supersede binary greedy meshing for block-exact faces. Binary greedy meshing independently validates neighbor padding, bitwise face masks, 8-byte quads, vertex pulling, MDI, camera-facing rejection, and testing T-junctions before applying mitigation.

### Vertex packing, face pulling, and vertex pulling

Reviewed: *Vertex Formats Part 1: Compression*; *Vertex Formats Part 2: Fetch vs Pull*; programmable vertex pulling by hannomalie and ktstephano; D3D11 dynamic vertex pulling; WebGL vertex pulling; `nlguillemot/ProgrammablePulling`; `superdump/bevy-vertex-pulling`; plus the wiki's Face Pulling and Vertex Pooling destinations.

Outcome: retain an aligned 8-byte record and coherent storage-buffer access. Pulling performance is layout- and hardware-dependent; it is not automatically faster. The D3D11 atlas account is evidence against monolithic in-frame compaction. Vertex Pooling's historical body could not be recovered, so no recommendation relies on it.

## Raytracing

Every numbered paper and both adjacent technical chapters were reviewed:

1. *A Fast Voxel Traversal Algorithm for Ray Tracing* — establishes Amanatides–Woo DDA's two comparisons and one addition per step.
2. *Real-time Ray Tracing and Editing of Large Voxel Scenes* — supports editable `8³` occupancy bricks, separate attributes, hierarchical DDA, bounded feedback requests, and fallback LOD.
3. *Novel Extended Brickmap for Real-Time Ray Tracing* — supports compact material encodings and shows memory layout/locality and fragmentation are critical.
4. *Efficient Sparse Voxel Octrees: Analysis, Extensions, and Implementation* — strong static-scene hierarchy, but more complex updates and traversal.
5. *Symmetry-aware Sparse Voxel DAGs* — extraordinary lossless static compression, with construction and edit/residency costs.
6. *Interactively Modifying Compressed Sparse Voxel Representations* — proves mutable compressed DAGs are possible, but requires specialized mutation, copying, and collection machinery.
7. *A Survey on Bounding Volume Hierarchies for Ray Tracing* — supports a separate structure for dynamic/repeated non-terrain objects, not replacing editable voxel terrain.
8. *Voxel-Based Terrain for Real-Time Virtual Simulations* — smooth scalar terrain and Transvoxel transitions, not block-exact LOD seams.
9. *Fast Hydraulic and Thermal Erosion on the GPU* — world generation, not renderer optimisation.
10. *GigaVoxels* — view/occlusion-guided production, caching, temporal coherence, and filtered multiscale data.
11. *Grid-Free Out-of-Core Voxelization to Sparse Voxel Octrees on GPU* — static triangle-mesh import, not streamed block terrain.
12. *Fast Voxel-Based Hydraulic Erosion* — world generation/simulation, not rendering.
13. *Large Scale Terrain Generation from Tectonic Uplift and Fluvial Erosion* — world generation, not rendering.
14. *Spatiotemporal Variance-Guided Filtering* — relevant only with stochastic secondary-ray lighting and robust reprojection.
15. *Hierarchical Dynamic Pathfinding for Large Voxel Worlds* — simulation/pathfinding, not rendering.
16. *ReSTIR* — reservoir reuse for stochastic direct lighting, not primary DDA traversal.
17. *ReSTIR GI* — multi-bounce path reuse plus denoising/history complexity, not a current optimisation.
18. *3D Raycasting* — confirms DDA as the grid primitive.
19. *Raysterization* — accurately names the current raster-proxy plus fragment-DDA architecture.

Outcome: retain fixed editable bricks and raster proxy coverage. SVO/SVDAG replacement remains rejected. Add the fixed `u8` material palette first; occupied-only attributes are a later measured experiment.

## Level Of Detail

Reviewed: perspective projection; general computer-graphics LOD; and moiré patterns. The deployed page was a stub during the first pass, but the audited upstream source contains these three references.

Outcome: supports screen-space/projected-error experimentation, hysteresis, and mipmapping, but supplies no voxel-specific aggregation, streaming-ring, or seam algorithm. Mipmapped or explicitly selected texture levels are a direct near-term recommendation because the current texture array has no mip chain.

## Multidraw

Reviewed after deduplication: OpenGL multidraw-indirect specifications; NVIDIA *Approaching Zero Driver Overhead*; Vulkan's multi-draw-indirect sample; VkGuide indirect drawing and GPU-driven rendering; ktstephano MDI; Ferran Sole MDI; Litasa per-instance textures; Antoine Morrier indirect rendering; and the linked practical discussion.

Outcome: pooled geometry, indirect commands, GPU culling, and indirect-count compaction are already present. Add command/batch/fallback/zero-command telemetry before redesigning submission.

## OpenGL

Reviewed: official site and wiki; pipeline overview; object model; GLSL; common mistakes; background and hardware database; `docs.gl`; blend-function tool; LearnOpenGL; OpenGL 3.3, Anton's OpenGL 4, and modern-OpenGL tutorials; the Programming Guide destinations; language bindings; glbinding; OpenTK; LWJGL; MDN WebGL; and WebGL Fundamentals.

Outcome: general API education and diagnostics, not evidence for replacing Bevy/wgpu. Commerce-only duplicates for the same book were not counted as separate technical content.

## Vulkan

Reviewed: official site; background; registry/specification; hardware database; SDK; Vulkan Guide; Vulkan Tutorial; *Vulkan in 30 Minutes*; render graphs; *I Am Graphics And So Can You*; Zeux's efficient Vulkan renderer; NVIDIA Vulkan multithreading; MoltenVK; and the linked tools/language-binding destination.

Outcome: reinforces explicit batching, resource lifetime, parallel command construction, and portability concerns. The current abstraction already exposes the required compute/storage/indirect features, so a direct Vulkan rewrite remains unjustified.

## Signed Distance Fields

Reviewed: signed-distance-function background. The old and current URLs for Inigo Quilez's 2D and 3D analytic SDF formula catalogs returned robots/403 blocks to the available fetcher. Their catalog role is independently indexed, but their bodies were not claimed as read.

Outcome: SDFs do not preserve discrete block material identity or rollback-friendly edits without another generated representation. Conservative distance metadata remains only a profiling-driven empty-space-skipping experiment.

## Access exceptions

Several exact historical links were dead or minimal, but were recoverable through mirrors, upstream source, or archival/indexed copies: Marching Cubes, Extended Marching Cubes, Dual Contouring, Amanatides–Woo, both brickmap theses, Raysterization, the Codeflow experiment, Volume Marching, GLFW's historical FAQ, and the Hugo Elias radiosity page. Vertex Pooling had no recoverable article body. The only direct technical bodies that remained blocked after trying their old and current URLs were the two Inigo Quilez SDF formula catalogs.

These exceptions are recorded so “all references” does not imply that an inaccessible server response was silently treated as read.
