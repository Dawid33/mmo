# Voxel Rendering Architecture Report

This report describes the implementation in the current tree as of July 16, 2026. It focuses on shipping code rather than design documents.

## Executive summary

The renderer is a hybrid, tiered architecture built on Bevy 0.18.1:

- Near terrain uses full-resolution 32³ microvoxel chunks.
- Far terrain uses server-streamed 32³ LOD cells at factors 2, 4, 16, 64, and 256.
- A common CPU binary greedy mesher produces backend-neutral quads.
- Near rendering offers three submission backends:
  - one Bevy mesh per chunk;
  - shared GPU quad buffers with grouped direct draws;
  - compute culling plus multi-draw indirect.
- Far rendering normally rasterizes streamed LOD cells through the same mesh or quad-record representation.
- An opt-in brick-raymarch backend encodes far cells into deduplicated 8³ bricks, but it is not auto-selected and falls back to raster when capabilities, pipelines, or storage budgets are insufficient.
- Visibility is layered: hidden-face elimination, neighbor-aware meshing, cave-connectivity BFS, per-view frustum culling, native depth/HZB occlusion, and GPU compute culling.
- Rendering is explicitly derived state. Simulation, rollback, networking, and world generation remain independent of Bevy.

The central architectural strength is that geometry generation is mostly independent of draw submission. The main liabilities are duplicated Rust/WGSL binary contracts, lighting differences between backends, opaque-only material semantics, and the considerable operational complexity of the experimental brick path.

## End-to-end data flow

```text
Authoritative simulation chunks
  2×2×2 blocks + chisel data
             │ derive_voxels
             ▼
      32³ microvoxel grid
             │ bridge compresses to RLE VoxelData
             ▼
 Render-world chunk entity under a region root
             │ Changed<VoxelData>
             ▼
 ChunkGrid + boundary hashes + RemeshQueue
             │ async task, nearest/in-frustum first
             ▼
 34³ padded neighbor snapshot
             │ binary visibility + greedy merge + baked AO
             ▼
          Vec<Quad>
       ┌─────┴──────────────────┐
       ▼                        ▼
Packed Bevy Mesh          8-byte QuadRecord
per-chunk backend         direct/GPU-driven backends
       │                        │
       └────────────┬───────────┘
                    ▼
       forward + depth + shadows
```

Far LOD data enters through a parallel lane:

```text
Server-generated 32³ LOD grids
  factors 2/4/16/64/256
              │
              ▼
 LodRings → LodVoxelData children
              │
       ┌──────┴────────┐
       ▼               ▼
 raster meshing   brick preparation
       │               │
 mesh/records      deduplicated DAG
       │               │
 standard/pull     custom far pass
 render phases     proxy-cube DDA
```

The top-level registration and exact system order are in `crates/client/src/renderer/mod.rs`.

## 1. Simulation-to-render boundary

The simulation owns chunks as eight 16³-voxel blocks arranged 2×2×2. Rendering expands block and chisel data into a fixed 32³ microvoxel grid; one world unit is one sixteenth of a block-scale metre.

The Bevy bridge converts each dense grid into run-length encoded `VoxelData`:

- Runs follow `x + 32*y + 32²*z`, so terrain's naturally long X runs compress well.
- Dense storage would cost 64 KiB per chunk.
- Dense data is reconstructed only transiently for meshing.
- Render entities are children of region roots, allowing region-relative transforms while the meshing grid uses global chunk coordinates.

The relevant code is in `crates/client/src/renderer/bridge.rs` and the authoritative chunk model is in `crates/game/src/voxel.rs`.

The bridge consumes simulation updates in `PreUpdate`. `SetVoxelComponent` insertion or removal drives Bevy change detection, after which all render-side work is disposable and rebuildable. Rollback bursts are treated as last-write-wins rather than as incremental render mutations.

## 2. Remesh scheduling and neighbor correctness

Changed chunks enter a deterministic `BTreeMap`-backed dirty queue. `ChunkGrid` maps global `[x,y,z]` chunk keys to entities.

Each chunk stores CRC32 fingerprints for 26 overlapping boundary zones:

- six faces;
- twelve edges;
- eight corners.

An interior edit dirties only its own chunk. A boundary edit also dirties precisely the neighbors whose padding changed. Removing a chunk dirties all surviving neighbors so previously hidden shared-plane faces reappear.

Work is bounded and prioritized:

- Maximum 64 near meshing tasks in flight.
- Maximum 32 near uploads applied per frame.
- In-frustum chunks rank before out-of-frustum chunks.
- Within each class, nearer chunks rank first.
- Each task owns compact RLE inputs and builds its 34³ padded snapshot off the main thread.
- Replacing `MeshingTask` cancels obsolete work for that entity.

The scheduling implementation is in `crates/client/src/renderer/meshing/queue.rs`.

Near and far lanes have separate per-frame upload counters. Far raster and brick construction share a separate 64-task lane, preventing distant terrain from starving near reconstruction.

## 3. Binary greedy meshing

The common mesher operates over a 34³ snapshot: 32³ interior voxels plus a one-cell shell from all 26 neighbors.

Its algorithm is specialized for the fixed chunk size:

1. Solidity is packed into `u64` columns for each principal axis.
2. Bit shifts identify solid-to-air transitions for positive and negative faces.
3. Visible cells are transposed into per-slice bitplanes.
4. Each cell receives a merge key containing block ID and four AO values.
5. Rectangles grow along the two face-local axes while visibility and merge keys match.

This means greedy merging never crosses a material or AO discontinuity. Missing neighbors are conservatively treated as air, so incomplete streaming may temporarily overdraw boundary faces but does not create holes.

The implementation and quad contract are in `crates/client/src/renderer/meshing/binary.rs`.

Ambient occlusion uses the classic three-sample voxel corner rule:

- two occupied side cells fully occlude the corner;
- otherwise AO is `3 - occupied_samples`;
- levels map to 0.55, 0.70, 0.85, and 1.0;
- triangulation flips when the alternate diagonal gives better interpolation.

See `crates/client/src/renderer/meshing/ao.rs`.

## 4. Near rendering backends

Configuration is centralized in `TerrainRenderConfig` and controlled on native by:

- `VOXEL_NEAR_RENDER=per-chunk`
- `VOXEL_NEAR_RENDER=direct`
- `VOXEL_NEAR_RENDER=gpu`

Native auto-selection requests GPU-driven near rendering. WebAssembly always selects grouped-direct. Runtime device or pipeline failures downgrade GPU-driven rendering to grouped-direct. See `crates/client/src/renderer/config.rs`.

### Per-chunk Bevy meshes

Each greedy quad becomes:

- four vertices;
- six indices;
- one `Uint32x2` attribute per vertex;
- no conventional `POSITION` attribute.

The two packed words carry position, face, AO, corner identity, quad extent, and texture layer. A custom material extension replaces both the forward and prepass vertex shaders so Bevy can render this packed-only layout.

Each quad costs approximately 56 bytes before allocator overhead: four 8-byte vertices plus six 4-byte indices.

Relevant files are:

- `crates/client/src/renderer/meshing/emit.rs`
- `crates/client/src/renderer/meshing/pack.rs`
- `crates/client/src/renderer/voxel_material.rs`
- `assets/shaders/voxel_texture.wgsl`

This backend participates naturally in Bevy's mesh phases, PBR lighting, depth prepass, shadow passes, and mesh visibility infrastructure.

### Grouped-direct vertex pulling

Modern near backends instead encode one 8-byte `QuadRecord` per greedy quad. The record stores the quad origin, face, four AO levels, diagonal choice, width, height, and texture layer.

The render world maintains shared growable buffers for:

- all quad records;
- chunk transforms;
- stable chunk metadata.

A deterministic first-fit allocator reuses variable record ranges while a separate slot allocator keeps transform and metadata indices stable across remeshes. Buffer capacity grows geometrically. Changed chunks update only their assigned ranges in steady state.

For each camera or shadow view, CPU frustum culling builds a list of visible chunk allocations. One `NonMesh` phase item represents the entire terrain set for that view, then loops over the list and issues one direct draw per chunk.

This removes per-chunk phase-item overhead while preserving straightforward draw semantics. See:

- `crates/client/src/renderer/meshing/records.rs`
- `crates/client/src/renderer/voxel_pull/arena.rs`
- `crates/client/src/renderer/voxel_pull/queue.rs`

### GPU-driven vertex pulling

The GPU-driven path consumes the same records and arena. Compute shaders read stable chunk metadata and build indirect commands.

Required capabilities include:

- Bevy GPU culling support;
- compute;
- indirect first-instance;
- multi-draw indirect.

If supported, `MULTI_DRAW_INDIRECT_COUNT` compacts survivors and submits their exact count. Otherwise, every stable slot has an indirect command, with rejected slots receiving zero vertices.

The frame graph is carefully ordered:

1. Frustum compute culling populates commands for early depth and shadow passes.
2. Early depth and shadows consume them.
3. Commands are cleared to avoid duplicate late-phase draws.
4. HZB culling repopulates the main-camera commands.
5. The opaque forward pass consumes the HZB survivors.

See `crates/client/src/renderer/voxel_pull/gpu.rs` and `assets/shaders/voxel_pull_cull.wgsl`.

The forward vertex shader synthesizes six vertices per record from `vertex_index`; `instance_index` selects the chunk transform. A separate depth shader supplies camera depth and directional-shadow rendering. See `assets/shaders/voxel_pull.wgsl` and `assets/shaders/voxel_pull_depth.wgsl`.

One important parity caveat is that the pulled forward shader implements ambient plus directional Lambert lighting and directional shadows manually. It does not reproduce the complete StandardMaterial PBR path used by the per-chunk backend.

## 5. Visibility architecture

Visibility is intentionally conservative and layered.

### Geometry-level elimination

The mesher removes all faces adjacent to solid voxels, including faces hidden by neighboring chunks.

### Cave-connectivity graph

During meshing, each chunk receives a 15-bit mask describing which pairs of its six boundary faces are connected through air.

Every frame, a camera-centered BFS traverses the near `ChunkGrid`:

- movement through a chunk requires entry-to-exit connectivity;
- direct backtracking is forbidden;
- off-frustum cells are rejected when safe;
- absent cells and chunks awaiting a connectivity result are treated as open;
- traversal is bounded to a Chebyshev radius of 64 chunks.

Unreached near chunks are set to `Visibility::Hidden`. LOD cells deliberately have no `ChunkKey`, so this graph never hides far terrain.

The BFS runs in `PostUpdate`, after the current frustum is updated but before visibility propagation. See `crates/client/src/renderer/meshing/visibility.rs`.

### Frustum and occlusion culling

Depending on backend:

- Bevy handles mesh frustum culling.
- Grouped-direct uses CPU world-AABB tests per view.
- GPU-driven uses homogeneous clip-space AABB tests.
- Native cameras receive a depth prepass and Bevy experimental occlusion-culling component.
- GPU-driven forward terrain additionally uses the depth pyramid directly.

The renderer deliberately ignores the projection's legacy far-plane field because Bevy uses infinite reverse-Z; otherwise terrain beyond 1 km would disappear.

## 6. Far LOD rendering

The server streams fixed 32³ grids at factors `2, 4, 16, 64, 256`.

The ring table assigns those factors at Chebyshev region bounds `2, 4, 8, 16, 32`.

Downsampling marks an output voxel solid if any of its eight source voxels is solid. Material is the majority among solid inputs, with the lowest block ID breaking ties. See `crates/game/src/lod.rs`.

The client creates one root per `LodKey` and scaled child entities for each decoded cell. Same-scale siblings provide padding for neighbor-aware meshing.

Raster LOD uses the common greedy mesher and whichever final representation the near configuration selected:

- `per-chunk` near mode produces LOD `Mesh3d` assets;
- `direct` and `gpu` near modes produce `ChunkQuads`.

Side faces on cell borders receive one-cell downward skirts. This hides cracks between differently scaled rings. LOD transforms compensate for the skirt bias, and conservative AABBs include the extra height.

See `crates/client/src/renderer/meshing/lod.rs`.

LOD replacement is pop-resistant. When a key refreshes or changes ownership, the old root remains visible until at least one replacement child is render-ready. A 600-frame deadline prevents permanent retention for all-air or failed replacements. See `crates/client/src/renderer/lod_rings.rs`.

## 7. Experimental brick-raymarch far backend

`VOXEL_FAR_RENDER=brick` opts into the hybrid far path. It is compiled on native but deliberately excluded from automatic selection.

Each 32³ cell is partitioned into 64 bricks of 8³ voxels. A root entry is one of:

- empty;
- uniform material;
- reference to a mixed brick.

Mixed bricks store 512 occupancy bits and packed 16-bit materials. Identical roots and mixed bricks are hash-interned and reference-counted in a deterministic DAG. Total far storage is capped at 256 MiB.

The lifecycle is transactional:

1. CPU workers prepare immutable brick roots.
2. Main-world residency stages DAG and cell mutations.
3. Upload batches cross into the render world.
4. GPU upload returns ACK or NACK.
5. Only ACKed content becomes visible; rejected refreshes retain the previous resident cell where possible.
6. Unsupported devices, unready pipelines, budget exhaustion, or invalid data trigger raster fallback.

See:

- `crates/client/src/renderer/far_voxel/brick.rs`
- `crates/client/src/renderer/far_voxel/residency.rs`
- `crates/client/src/renderer/far_voxel/lifecycle.rs`

The custom render pass runs between `StartMainPass` and `MainOpaquePass`:

- compute culling compacts visible cell slots;
- one indirect draw renders proxy cubes;
- the fragment shader performs cell- and brick-level DDA;
- it samples voxel material, reconstructs the hit normal and AO, and writes color and reverse-Z depth.

See `crates/client/src/renderer/far_voxel/pass.rs` and `assets/shaders/far_voxel.wgsl`.

This backend is architecturally complete enough for benchmarking, but remains the highest-risk subsystem: it has adapter gates, large residency machinery, custom lighting, custom depth behavior, and a required raster escape path.

## 8. Textures, lighting, and presentation

The shared block registry maps each block ID to either:

- one texture for all faces;
- separate top, side, and bottom textures;
- no texture.

Textures are deduplicated into a nearest-filtered, repeating 2D array. Layer zero is always a generated magenta/black missing texture. Native loads PNGs at runtime; WebAssembly embeds them.

See `crates/client/src/renderer/block_textures.rs` and `crates/client/src/renderer/mod.rs`.

The scene uses:

- one shadow-casting directional light;
- warm global ambient light;
- native SMAA with MSAA disabled;
- a depth prepass and native occlusion culling.

A current material limitation is that every non-air block—including water—is meshed as solid opaque terrain. There is no transparent voxel phase, fluid surface specialization, or per-material render classification.

## 9. Performance and memory characteristics

Notable controls and trade-offs:

- Persistent chunk voxels are RLE-compressed.
- Near meshing is asynchronous and limited to 64 tasks.
- Near and far uploads are independently capped at 32 per frame.
- Pull records cost 8 bytes per quad versus roughly 56 bytes per legacy indexed quad.
- Shared pull buffers grow geometrically and patch changed ranges.
- Freed arena ranges and slots are reused, but high-water capacities do not shrink automatically.
- Cave culling reduces submitted near chunks before render extraction.
- Grouped-direct reduces phase items to one per view but still performs one draw per visible chunk.
- GPU-driven reduces CPU draw submission through compute culling and multi-draw indirect.
- LOD retirement prefers temporary overdraw to visible horizon holes.
- Brick mode deduplicates repeated far content but reserves a substantial, explicitly capped GPU storage domain.

## 10. Validation and diagnostics

The renderer has extensive focused tests covering:

- binary mesher correctness and structured/random parity;
- parity against the vendored `block-mesh` oracle;
- AO and triangulation;
- neighbor padding and boundary dirtying;
- packed Rust/WGSL formats;
- mesh-versus-record reconstruction parity;
- cave-culling behavior and schedule placement;
- GPU capability fallback and render-graph ordering;
- arena allocation and reuse;
- LOD skirts and retirement;
- brick deduplication, residency, upload rollback, DDA, and shader validation.

The parity suite is in `crates/client/tests/mesh_parity.rs`.

Operational tools include:

- `just pull-smoke` for a headless pulled-render screenshot probe;
- `just render-bench backend=<pair>` for repeated 1920×1080 production-scene JSON benchmarks;
- `DIAG=1` for render-pass timing and pipeline statistics;
- `just test-full` as the required complete verification command.

The benchmark supports `per-chunk+raster`, `direct+raster`, `gpu+raster`, and `gpu+brick`; its canonical recipe is in the root `justfile`.

## Overall assessment

The architecture is well partitioned around a strong shared abstraction: greedy quads are the stable boundary between voxel topology and rendering strategy. That makes backend comparison credible and limits divergence in meshing correctness.

The most important future concerns are:

1. Preserve parity across the three packed formats and their WGSL decoders.
2. Resolve lighting and material differences between StandardMaterial, pulled terrain, and brick terrain.
3. Introduce explicit opaque, transparent, and fluid material classes before richer block behavior.
4. Continue treating brick mode as opt-in until adapter coverage and quality/performance gates justify automatic selection.
5. Monitor arena fragmentation and persistent high-water memory during long edit-heavy sessions.
6. Keep culling conservative: most existing safeguards correctly prefer overdraw over missing terrain.
