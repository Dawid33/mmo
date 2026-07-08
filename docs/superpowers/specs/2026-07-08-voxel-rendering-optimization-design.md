# Voxel Rendering Optimization — Design

**Date:** 2026-07-08
**Status:** Approved (phases to be implemented sequentially, one plan per phase)
**Scope decisions (user-approved):** everything-phased program; WebGPU-only browser target (WebGL2 fallback no longer supported); lighting scope is baked AO only (light engine is a named future phase); foundation-first phase order.

## 1. Problem

The client's voxel rendering pipeline is correct but naive. Measured and code-level pain points:

- **Join-burst meshing stall.** Joining meshes ~9 regions × ~1088 dense subchunks at full resolution; `block_mesh::greedy_quads` was measured at ~65% of client CPU during the burst (live profile, 2026-07-08). Every changed chunk spawns a task immediately — no prioritization or budget.
- **No neighbor-aware border culling.** `build_chunk_mesh` meshes a 34³ padded buffer whose padding shell is always air (`crates/client/src/renderer/meshing.rs`), so all six boundary face layers are emitted even against solid neighbors. Underground/underwater terrain pays heavily.
- **Fat vertices.** ~60 bytes/vertex: Float32x3 position + Float32x3 normal + Float32x2 UV + a constant-white Float32x4 COLOR that exists only to satisfy a shader-def, + Uint32x3 texture indices.
- **Whole-chunk remesh per edit**, fresh 32768-`Voxel` allocation per remesh (`derive_voxels`), full `VoxelData` clone per task.
- **No render LOD.** The far ring (`FAR_VIEW_RADIUS`, just enabled 0→1) meshes at full resolution; beyond it the world ends. "Flat world" feel = mid-ocean spawn + no visible far terrain.
- **No occlusion culling** beyond Bevy's default per-entity frustum cull.

## 2. Research summary

Four research passes (current-pipeline map, CPU meshing, GPU-side, LOD) converged on:

| Finding | Source | Relevance |
|---|---|---|
| Binary greedy meshing: u64 occupancy masks, face-cull by bitshift, merge by trailing-bit scans; ~74µs avg per 32³ chunk (~10× classic greedy) | cgerikj/binary-greedy-meshing, TanTanDev demo (Rust+Bevy), Inspirateur crate | Direct join-burst fix |
| Neighbor-filled padding is the standard border-culling fix; block-mesh's API already assumes it | 0fps, block-mesh-rs docs | Pure geometry reduction |
| Packed vertex formats: chunk-local quantized ints, 4–8 bytes/quad-record vs ~240 bytes/quad today | Vercidium, cgerikj, voxel.wiki | ~8× mesh memory/bandwidth |
| Baked per-vertex AO (0fps corner trick, 2 bits, quad-flip fix); the only AO portable to browsers (SSAO needs compute) | 0fps AO article | Visual depth, render-only |
| Tommo cave-culling: per-chunk face-connectivity bitmask + camera BFS; culls 50–99%; pure function of occupancy → stays out of rollback state | tomcc.github.io visibility articles, Vintage Story variant | Portable occlusion culling |
| Bevy 0.16+ has GPU-driven rendering (MDI + GPU cull, native Vulkan) and experimental two-phase HZB occlusion culling (`OcclusionCulling` + `DepthPrepass`); none of it runs on WebGL2 (no compute/SSBO); WebGPU tier is "transform only" | Bevy 0.15/0.16 release notes, GpuPreprocessingSupport docs | Native wins are near-free; WebGL2 is the odd one out → dropped |
| Bevy 0.16 mesh allocator no longer relocates meshes on slab growth (PR #17793) — protects edit-heavy workloads; keep Bevy `Mesh` to retain it | Bevy PR #17793 | Argues for packed *attributes* over full vertex pulling |
| LOD from deterministic worldgen (Veloren model): far field = pure client-side function of coordinates, zero bandwidth; vs Distant Horizons' download-and-cache | Veloren devblogs, DH docs | Best fit: `generate_region` is already shared and deterministic |
| Blocky-voxel LOD seams need only skirts + ≤1-level-apart rings; Transvoxel-class machinery is for smooth terrain | 0fps blocky-LOD, godot_voxel, dexyfex | Keeps Phase 4 cheap |
| Ray marching (SVO/brickmap) and meshlets: wrong fit — edit-hostile or browser-incompatible; rasterized greedy meshes remain the right baseline | Laine-Karras, Aokana, Teardown breakdown, Bevy meshlet docs | Explicitly rejected |

Primary URLs: 0fps meshing/AO/LOD articles · github.com/cgerikj/binary-greedy-meshing · github.com/TanTanDev/binary_greedy_mesher_demo · tomcc.github.io/2014/08/31/visibility-1.html · vercidium.com/blog/voxel-world-optimisations · veloren.net/blog/devblog-100 · bevy.org/news/bevy-0-16 · voxel.wiki/wiki/vertex-pulling.

## 3. Constraints

- `game`/`server` stay Bevy-free. All rendering work lands in `crates/client` (Phase 4 additionally links `worldgen` from the client — already a dependency for offline mode — and may extend `protocol.rs` constants).
- **Nothing enters rollback-hashed state.** Meshes, AO, visibility bitmasks, LOD grids are derived render-side data: recomputed after rollback, never journaled or hashed.
- Browser target is WebGPU-only. We do not maintain or test a WebGL2 path. (Chrome/Edge 113+, Firefox 141+, Safari 26+.)
- Tunables (budgets, radii, hysteresis) are named constants even without a UI.
- Determinism-sensitive vendored forks are untouched; the mesher change is client-only.

## 4. Success criteria

1. Join burst: meshing the ~1088-subchunk join workload completes within a bounded per-frame budget with no multi-second stall; a `slow`-tagged perf gate pins total mesh time.
2. Mesh memory: ≤8 bytes/vertex on the wire to the GPU (vs ~60 today), asserted by test.
3. Border culling: a chunk face fully covered by a solid loaded neighbor emits zero quads, asserted by test.
4. Culling: in a constructed cave scene, chunks not reachable by the visibility BFS are `Visibility::Hidden`, asserted headlessly.
5. View distance (Phase 4): worldgen rings render terrain well beyond the subscription window with no cracks at ≤1-level LOD seams.
6. `just test` / `just test-full` green throughout; headless client suite passes unchanged after each phase.

## 5. Phase 1 — Meshing core

New mesher in `crates/client/src/renderer/meshing/` (module split: `binary.rs` mesher, `ao.rs`, `queue.rs` scheduling, existing task plumbing stays in `mod.rs`).

**5.1 Binary greedy mesher.** In-house port of the cgerikj algorithm (small, and we need a custom merge predicate; the Inspirateur crate is the reference if porting stalls). Occupancy packed as u64 column masks over the 34³ padded space (34 bits used per lane). Per face direction: visibility = `solid & !(solid << 1)`-family shifts; greedy merge scans runs with trailing-zero ops, merging only while `(BlockId, [ao; 4])` match. Output: quad list `(x, y, z, w, h, face, block, ao4)`. The vendored `block-mesh` stays during transition as the parity oracle; the client drops its dependency on it once parity tests pass (the fork itself remains in-tree for other consumers/reference).

**5.2 Neighbor-aware padding.** The padding shell is filled from adjacent chunks' boundary voxels instead of air. The bridge assembles a neighborhood snapshot per remesh: 6 face slices, 12 edge lines, 8 corner voxels (edges/corners needed for AO). Dirty propagation: an edit touching a chunk boundary marks the adjacent chunk(s) dirty too. Missing neighbor (unloaded) ⇒ air padding on that side (today's behavior — over-draws rather than holes). Vertical neighbors come from the same `Column`; horizontal from adjacent chunk entities via the bridge's chunk index.

**5.3 Baked AO.** 0fps corner rule per vertex: `ao = (s1 && s2) ? 0 : 3 - (s1 + s2 + corner)`, 2 bits. Triangulation flips when `a00 + a11 > a01 + a10`. AO participates in the merge predicate (5.1). AO is computed from the same padded occupancy — no extra data source.

**5.4 Remesh scheduling.** Replace "spawn task on every `Changed<VoxelData>`" with: dirty set → priority queue keyed on (camera distance, in-frustum first) → per-frame budgets `MESH_TASKS_PER_FRAME` and `MESH_UPLOADS_PER_FRAME` (named constants). Existing task-supersede/cancel semantics and the `VoxelData`-is-liveness-authority rule in `apply_meshed_chunks` are preserved. The join burst becomes a drained queue over several frames, nearest-first.

**5.5 Testing.** Parity vs `block_mesh::greedy_quads` on seeded random chunks (AO disabled ⇒ identical covered voxel-face area; quad decomposition may differ). Boundary tests: solid neighbor ⇒ zero faces on the shared plane; missing neighbor ⇒ faces present. AO truth-table and quad-flip unit tests. Perf gate on the join-burst workload, `#[ignore = "slow: …"]`.

## 6. Phase 2 — GPU data path

**6.1 Packed vertex format.** Two u32s per vertex (`Uint32x2` attribute): position quantized to chunk-local integer lattice (6 bits/axis covers 0..=33), quad `w,h` (6 bits each — the shader tiles the texture via `fract`), 3-bit face direction (replaces the 12-byte normal), 2-bit AO, texture-array layer (the per-face `[top, side, bottom]` triple collapses to one layer chosen CPU-side per quad, since face direction is known per quad at mesh time). Dead COLOR attribute is gone. Decision: keep Bevy `Mesh` + custom integer attributes rather than storage-buffer vertex pulling — retains the mesh allocator (PR #17793 no-relocation), automatic batching, and native GPU-driven rendering. SSBO vertex pulling (a further ~2×, per-quad instead of per-vertex records) is a documented future option if profiling demands.

**6.2 Custom material.** `ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>` becomes one custom `Material`: vertex shader unpacks the ints (positions to chunk-local floats, face → normal, UV from quad dims); fragment keeps array-texture sampling and PBR lighting via `bevy_pbr` WGSL imports (directional shadows and ambient unchanged) plus AO darkening. Same single shared material instance for all chunks (one batch bin). Prepass short-circuit behavior re-evaluated against the custom pipeline.

**6.3 Memory.** Meshes switch to `RenderAssetUsages::RENDER_WORLD` (CPU copy freed post-upload; colliders come from rapier, not render meshes). Known hazard: Bevy #13500 — GPU memory for render-world-only assets is not reclaimed on handle drop; monitor VRAM under chunk churn and revert this sub-item if it leaks in practice.

**6.4 Testing.** Pack/unpack round-trip unit tests; bytes-per-vertex assertion; naga validation of the WGSL in a test; headless client suite unchanged; manual visual parity check (textures, shadows, tiling on merged quads).

## 7. Phase 3 — Visibility culling

**7.1 Cave-culling visibility graph (Tommo).** At mesh time (occupancy already in hand), flood-fill the chunk's non-opaque space; record which of the 15 unordered face-pairs connect, as a u16 bitmask stored on the chunk's render entity. Per frame, BFS from the camera's chunk over the loaded grid: enter a neighbor only if the step direction doesn't back-track against the view and the current chunk's bitmask connects entry→exit faces; frustum test last (most expensive). Unreached chunks get `Visibility::Hidden`. The bitmask is a pure function of occupancy: recomputed on remesh, lazily recomputed after rollback, never hashed.

**7.2 Bevy HZB occlusion culling (native).** Add `OcclusionCulling` + `DepthPrepass` to the camera on native builds (experimental in 0.16+; requires the GPU preprocessing tier, so it self-disables in the browser). Complements 7.1 at sub-chunk granularity.

**7.3 Deferred.** Orientation-bucket backface culling (6 direction buckets per chunk, ~25–35% reported) — costs 6× entities or a custom draw path in Bevy; the other two layers subsume most of it. Documented, not implemented.

**7.4 Testing.** Bitmask unit tests on known shapes (sealed box: no pairs; L-tunnel: exactly its face pair; open column: vertical pair). Headless BFS tests on a constructed cave scene asserting exact hidden/visible sets; regression test that an edit opening a wall updates connectivity and visibility next frame.

## 8. Phase 4 — LOD and view distance

Three tiers by distance:

**8.1 Near window (unchanged).** 3×3 regions, full-res meshes from authoritative server data via the Phase 1–3 pipeline.

**8.2 Subscribed far ring — downsampled real data.** Far-ring regions currently mesh at full resolution; instead, downsample the streamed snapshots 2×/4× (per cell: opaque-if-any occupancy, majority material) and mesh with the same binary mesher at coarser scale. Player edits appear automatically (the source is authoritative data); no protocol change. Subsumes the existing far-snapshot mechanism; `far_snapshot.rs` evolves to cover downsampled meshing.

**8.3 Worldgen rings — beyond subscription.** The client calls the deterministic `generate_region` density pipeline directly (client already links `worldgen` for offline mode), sampling at block scale (16×) and coarser for outer rings. Zero server bandwidth; radius bounded by ring count, not distance. v1 limitation (accepted): these rings do not reflect edits — they are beyond subscription range at coarse resolution. Server-tracked edit deltas for coarse cells are a named follow-up, out of scope here.

**8.4 Seams and popping.** Invariant: adjacent rings differ by ≤1 LOD level. Skirts: border faces extended down by one coarse-voxel height of the coarser neighbor — sufficient for cube voxels; no Transvoxel-class machinery. Hysteresis: promote-to-finer at `LOD_PROMOTE_DIST`, demote at a strictly larger `LOD_DEMOTE_DIST` (named constants). Load order: coarse ring first, refine on approach — no empty-world pop-in.

**8.5 Scheduling.** LOD generation and meshing flow through the Phase 1 priority queue at lower priority than near-field remeshes, under the same per-frame budgets.

**8.6 Testing.** Downsample determinism (repeated runs bit-identical); skirt closure on constructed ≤1-level seams (no cracks by geometry inspection); hysteresis unit tests (no thrash across a boundary walk); headless view-distance smoke test.

## 9. Risks

- **Neighbor-dirty propagation at edges/corners** subtly wrong → stale faces. Mitigation: exhaustive boundary parity tests, including edits landing exactly on chunk edges/corners.
- **Custom material regresses shadows/PBR.** Mitigation: build on `bevy_pbr` imports rather than hand-rolled lighting; manual visual pass on native + browser before declaring Phase 2 done.
- **VRAM growth under churn** (Bevy #13500 with `RENDER_WORLD`-only). Mitigation: monitor; the sub-item is independently revertible.
- **Bevy 0.18 API drift risk is low** here (mesh attributes, Material, visibility are stable APIs), but per project convention any Bevy upgrade during this work is its own tested migration.
- **Binary mesher correctness** vs the oracle. Mitigation: keep `block-mesh` as the parity oracle until seeded-random parity passes; only then remove the client dependency.

## 10. Out of scope (named future work)

- Voxel light engine (block + sky light flood-fill) — future phase; interacts with rollback state and needs its own design.
- LOD edit-delta streaming for worldgen rings (server tracks edited coarse cells).
- SSBO vertex pulling / per-quad instancing (further ~2× past Phase 2).
- Orientation-bucket backface culling.
- Ray marching (SVO/brickmap), meshlets/virtual geometry — rejected by research for editable terrain + browser.
- WebGL2 support — dropped by decision.

## 11. Implementation order

One implementation plan per phase, written when the phase starts, via the writing-plans skill: Phase 1 (meshing core) → Phase 2 (GPU data path) → Phase 3 (culling) → Phase 4 (LOD). Phases are independently shippable; each ends with `just test-full` green plus its phase-specific gates.
