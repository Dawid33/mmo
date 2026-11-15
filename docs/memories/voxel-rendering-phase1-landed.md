---
name: voxel-rendering-phase1-landed
description: "Phases 1-3 of the voxel rendering overhaul are merged to develop; phase 4 (LOD) is specced but not planned — don't re-research or re-plan 1-3"
metadata: 
  node_type: memory
  type: project
  originSessionId: eea0547e-6e65-4961-acd6-eb570d849702
---

Phase 1 merged 2026-07-09 (6954724); Phase 2 merged 2026-07-09 (07dd393); Phase 3 merged 2026-07-09 (e36f06f).

Phase 1: binary greedy mesher (occupancy bitmasks, (block,AO) merge key) replaced block-mesh in the client; baked AO; neighbor-aware padded culling via world-keyed `ChunkGrid` + boundary-zone crc32 fingerprints; distance/frustum-prioritized budgeted remesh queue (`MAX_IN_FLIGHT_MESH_TASKS`, `MESH_UPLOADS_PER_FRAME`). block-mesh survives as dev-dep parity/perf oracle (`crates/client/tests/mesh_parity.rs`, gate ≥2× oracle; measured 3.7× release).

Phase 2: packed 8-byte vertices — ONE `Uint32x2` attribute. `crates/client/src/renderer/meshing/pack.rs` is the bit-layout source of truth, mirrored by the decode in `assets/shaders/voxel_texture.wgsl` (layout LOCKED — change them together). Custom forward + prepass vertex shaders on the still-`ExtendedMaterial` material; `specialize` applies the packed layout to BOTH pipelines; meshes are `RENDER_WORLD`-only with constant `Aabb`+`NoAutoAabb`. AO pin is a 1e-6 epsilon (f32 ulp — bit-exact is unachievable, deliberate). Plain naga can't parse naga_oil `#import` shaders → no WGSL unit validation exists; a live pipeline run is the shader gate.

- Spec: `docs/superpowers/specs/2026-07-08-voxel-rendering-optimization-design.md`. Next: Phase 3 = Tommo cave culling + HZB occlusion, Phase 4 = LOD (client-side worldgen rings). Locked: WebGPU-only browser, AO-only lighting, [[game-direction-entities]]-neutral.
- Plans (done): `docs/superpowers/plans/2026-07-08-meshing-core-phase1.md`, `docs/superpowers/plans/2026-07-09-gpu-data-path-phase2.md`.
- Deferred minors: gather_padded iterates full 34³ (shell-only ~4.7k cells); far-region refresh despawns+respawns all 64 chunks (churn if refresh frequency rises); grid keying assumes rigidbody-set Transform on chunk entities; `camera.iter().next()` single-camera assumption; WGSL varying location-5 gap; `FACE_NORMALS[(w0>>18)&7u]` could use `min(face,5u)` belt-and-braces; enabling deferred rendering would lose voxel texturing in the G-buffer (no deferred prepass fragment).
- PENDING USER CHECK: shadow APPEARANCE on packed meshes (pipeline error-free with shadows_enabled, appearance unjudged — flat ocean casts nothing; place a block or find relief).

**Why:** avoids re-researching/re-planning finished work and keeps the phase 3-4 entry points findable.
**How to apply:** rendering perf work starts from the spec's next phase; the mesher is oracle-pinned (parity suite must stay green) and the vertex bit layout is locked (pack.rs ↔ WGSL change together).

Phase 3: Tommo cave culling — `meshing/visibility.rs`: `face_connectivity` flood fill (15-pair u16, computed in the mesh task, attached as `ChunkConnectivity` next to Mesh3d), per-frame camera BFS (no-backtrack `d^1` rule; visited key MUST include entry face — plan-corrected under-visibility bug; grid-absent cells traverse open; `CULL_BFS_MAX_CHEBYSHEV = 64` must overshoot the loaded window, REVISIT in Phase 4), `Visibility::Hidden` on unreached chunks, F4 toggles `CullingConfig.graph_enabled`. Native-only HZB: `enable_native_occlusion` inserts DepthPrepass + OcclusionCulling (isolated commit d1c0714, revert = disable; experimental component).
- KNOWN TRADE-OFF for the visual pass: Hidden chunks stop casting shadows (off-screen terrain's shadows into view vanish). First remedy if popping is visible: drop the BFS frustum rejection (visibility.rs cull_chunks), keep mask-based culling — Bevy's per-view frustum culling is shadow-correct.
- Phase 3 plan (done): `docs/superpowers/plans/2026-07-09-visibility-culling-phase3.md`.

Phase 4 (LOD + view distance): MERGED to develop 2026-07-09 (merge commit e48f4c5). Merge resolution note: develop's fc1040b had moved cull_chunks into cull_plugin (PostUpdate, current-frame frustum) — resolved by keeping LOD systems in the Update chain and letting cull_plugin own culling. 12 commits, all reviews approved, gates green, stability 3x176/176, smoke clean. Contents: downsample2/downsample_blocks; skirted LOD meshing (+1 y-bias, pack domain 33); isolated keyless LOD entity path (LodVoxelData, Transform-scaled, budget-shared full-res-wins); far ring = factor-2 LOD from streamed snapshots; worldgen rings factors 4/16 (sticky band, WORLD_SEED assumption documented); retiring-root fix (no ring-transition pop; RETIREMENT_MAX_FRAMES=600). Remaining known pops: far→worldgen bridge handoff (documented out of scope); wasm generation hitching note; texture-density steps at ring seams (eyeball in visual pass).
