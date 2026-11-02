# One render unit per sim chunk (all backends; started as the WebGL2 path) — investigation

**2026-11-02 update — every backend.** The merge now applies to the pulled (native /
WebGPU) backends too: bricks always keep records, and a sim chunk's (or LOD tile's) merged,
re-sorted record list is its `ChunkQuads` there, or the source of its mesh on `PerChunk`.
Records gained a 9-bit brick placement (`w1 = w:6|h:6|layer:11|bx:3|by:3|bz:3`; 56 texture
layers exist, 11 bits = 2048) decoded by `voxel_pull.wgsl` / `voxel_pull_depth.wgsl`, which
are now compiled into the binary like `voxel_texture.wgsl` (a cached `.wgsl` in the browser
paired with the new 9-bit vertex format is what scattered the web terrain into striped
panels). Only `webgl_own_index_buffers` stays WebGL2-only: it works around Firefox's
index validation on a device without `BASE_VERTEX`.

2026-11-02. Status: IMPLEMENTED 2026-11-02 (`meshing/chunk_mesh.rs`, pack.rs 9-bit
positions); client unit tests and a browser re-profile pending.

Built as below, with two simplifications: the merged mesh lives on the **sim chunk entity
itself** (it already sits at the chunk origin under the region root, terrain chunks never
move, and avatars/rigs only touch bodies) instead of a separate `ChunkMesh` entity, so no
spawn/move/despawn plumbing; and a chunk is rebuilt whenever none of its bricks is queued
or meshing (first build and edits alike). The LOD→near handoff counts a `ChunkMeshDirty`
chunk as not yet meshed.

## Problem

The web build runs the `PerChunk` near backend (WebGL2 has no vertex-stage storage
buffers, so `VoxelPullPlugin::finish` downgrades from the pulled path). That path gives
every 32³-voxel **render brick** — 2×2×2 blocks, 2 m across — its own entity with its own
`Mesh3d`, so every brick with a face is one draw, in the main pass and again in every
shadow cascade.

Firefox profile of the web build after the index-buffer fix (18:35 capture, read with
`just firefox-profile`): frame time p50 **381 ms**; the page main thread is 73% Bevy
render work, all of it per-draw:

| system / cost | share of the main thread |
|---|---|
| `view_shadow_pass` | 19% |
| `submit_pending_command_buffers` (wgpu_hal::gles replaying every draw as WebGL calls) | 19% |
| `RenderContext::apply` | 11% |
| `main_opaque_pass_3d` | 8% |
| `batch_and_prepare_binned_render_phase` + shadow sort | 10% |
| hashbrown lookups per draw (mesh allocator slabs, `RenderMeshInstanceCpu`, `RenderMesh`) | ~20% self, inside the above |

No batching rescues this on WebGL2: without `BASE_VERTEX` Bevy gives every vertex array
its own buffer, and since 2026-11-02 every index array too (`webgl_own_index_buffers`),
so every mesh is its own draw.

## How many draws

`just brick-census --world worlds/default.world --region X,Z` (256 m play regions):

| region | sim chunks | brick entities | bricks with faces (= draws) | draws if 1 mesh/sim chunk |
|---|---|---|---|---|
| (0,0) | 629 | 82,630 | 45,864 | ≤ 629 |
| (3,2) | 565 | 82,241 | 40,662 | ≤ 565 |

~70 face bricks per sim chunk → **~70× fewer draws** (per view: main + each shadow
cascade). The fortress neighbourhood subscribes 3×3 regions, so the web client currently
asks for ~400k brick draws before frustum culling; merged it is ~5k.

## Design

Keep the brick as the **meshing** unit (neighbour gather, remesh queue, burial, edits,
`ChunkConnectivity` all stay per brick); change only the **render** unit on the
`PerChunk` backend to the sim chunk.

1. **Vertex format: chunk-local positions.** `pack.rs` word0 becomes
   `x:9 | y:9 | z:9 | face:3 | ao:2` (exactly 32 bits); word1 unchanged
   (`w:6 | h:6 | layer:16`). The 9-bit fields hold 0..=511 ⊇ 7·32 + 33. `cu`/`cv` are
   dropped — the shader has not read them since per-block texturing (pack.rs says so).
   `voxel_texture.wgsl` `unpack_local_position` and the face/ao shifts change to match
   (it is the only decoder; the prepass/shadow vertex uses the same function). LOD meshes
   (`meshing/lod.rs`) use the same format with offset 0 — positions unchanged in value.
   The pulled path's `records.rs` layout is separate and untouched.
2. **Bricks keep records, not meshes.** On `PerChunk`, mesh workers run the records path
   (`pull = true`, 8 B/quad — the same CPU memory the native pulled path keeps in
   `ChunkQuads`) and `apply_meshed_chunks` stores them on the brick (a `BrickRecords`
   component, not `ChunkQuads`, so nothing pulled-path-specific keys off it) and marks the
   owning sim chunk (`BrickOf.chunk`) `ChunkMeshDirty`. No `Mesh3d` on bricks.
3. **One render entity per sim chunk.** `ChunkMesh { chunk }` entity, `ChildOf(region
   root)` at the region-local chunk origin (the sim chunk entity itself carries the sim
   pose and is never interpolated, so it should not render). Spawned / moved / despawned
   with the bricks (`land_chunk_bricks`, `move_chunk_bricks`, `despawn_chunk_bricks`),
   `SessionScoped` via its region root. Constant `Aabb` = the `CHUNK_UNITS` cube +
   `NoAutoAabb`, as bricks have today.
4. **`rebuild_chunk_meshes`** (after `apply_meshed_chunks`, `PerChunk` only): for dirty
   chunks, nearest first (`chunk_rank`), within the existing near upload budget counted
   per chunk: expand every brick's records into packed vertices + `u32` indices with the
   brick's offset (`local * BRICK_VOXELS`) added, and replace the chunk's mesh asset in
   place (`Assets::insert` on the kept handle). A chunk with bricks still queued or in
   flight waits for them before its FIRST mesh (no 70 rebuilds during a join); once it has
   a mesh, any dirty brick rebuilds it (an edit re-uploads one chunk, ~tens–hundreds KiB).
   Records → vertices is a pure function, pinned against `emit::quads_to_mesh_data` by a
   parity test (records already carry pos, face, per-corner AO, flip and layer).
5. **Readiness**: the few non-test readers of brick `Mesh3d` (`render_bench` counts) and
   the headless pipeline tests in `meshing/mod.rs` / `queue.rs` that pin the per-chunk
   backend move to the chunk entity / `BrickRecords`.

## Trade-offs

- Frustum culling is per 16 m chunk instead of per 2 m brick: more vertices submitted,
  far fewer draws; on this path draws are the cost.
- Cave culling (`meshing/visibility.rs`, off by default) can no longer hide parts of a
  chunk on this path; it keeps working on the pulled path.
- An edit re-uploads the whole chunk mesh instead of one brick; still one buffer write.
- CPU memory: records (8 B/quad) on bricks + the GPU mesh (≈56 B/quad); today the CPU side
  keeps nothing after upload. Same as the native pulled path's CPU footprint.
- Coarser alternative (one mesh per region / per 4×4 chunk columns): fewer draws still,
  but an edit re-uploads much more and frustum culling gets coarse; per sim chunk matches
  the sim's own unit (one entity, one rapier body) and is the natural step.

## LOD rings (IMPLEMENTED 2026-11-02, `meshing/lod_tiles.rs`)

The same merge for the LOD rings: a LOD root (one `LodKey`) has one child per 32³ cell
group, each meshed on its own; a factor-2 key alone spans 64 groups per edge, too wide for
9-bit positions, so a root's cells merge per **8×8×8-group tile** (positions ≤ 7·32 + 33 =
257) into a tile entity under the root (`LodTiles` map on the root, `LodGroup` on each
cell, placed and scaled like the cells). Cells never change in place (a new payload is a
new root), so a tile is built once none of its cells is queued or meshing, within the far
upload budget; its cells get `LodRasterReady` only then, so retirement's "visible
replacement" checks still mean visible. A cell with no group draws its own records.

## Not in scope, same pattern (originally)

The LOD rings on this path also create one `Mesh3d` per LOD brick (`meshing/lod.rs`,
children of each LOD root); merging per LOD root is the same change and worth doing next.
Other main-thread costs seen in the same profile: the F3 dev panel's Slint software render
(7.6%), `rebuild_minimap_base` (4.6%); on the sim worker, rollback hash verification (58%
of its CPU).

## Verification

- Native, no browser: `VOXEL_NEAR_RENDER=per-chunk just run` exercises the same path;
  `just test-one client "--lib meshing::"` (ask first: client test binary).
- Web: `just web`, Firefox profile, `just firefox-profile <file>` — expect the render
  systems' share and frame time to drop; compare draws via `RENDER_BENCH`.
