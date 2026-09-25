# Client render performance — investigation (2026-09-22)

Follow-up to `2026-09-19-client-perf-findings.md`. Nothing implemented; this ranks what to do next
from two fresh measurements.

## Setup

- Intel Iris Xe iGPU (the RX 6700 was not attached), AC power, release build, working tree dirty
  at `8e90481` + all uncommitted work (1 km horizon, fog/atmosphere, riser fade).
- `render-bench` scene: world `full` (9 near regions + 5 LOD rings, ~1,080 LOD keys), 1920×1080,
  **no vsync**, scripted 360° yaw, `gpu+raster` (native default), 4 client worker threads.
- Run 1 (plain): `target/render-bench/inv-plain/bench.json`.
- Run 2 (same scene, `trace_chrome` build): `target/render-bench/inv-trace/{bench.json,client.json}`.
  The tracing build runs ~49 ms/frame, so per-system numbers are inflated; use them to rank, not
  as absolutes.

## Headline

| metric (plain run) | p50 | p95 |
|---|---|---|
| frame wall | 33.5 ms (30 fps) | 58.5 ms |
| GPU main_opaque_pass_3d | 11.4 ms | 25.0 ms |
| GPU early prepass (depth) | 6.0 ms | 14.4 ms |
| GPU smaa | 0.9 ms | 1.0 ms |
| GPU shadows (4 cascades) | ~0.6 ms | ~1.1 ms |
| load → ready | 16.8 s | |

The frame is now **bound by both GPU and CPU**. In the trace the render thread's largest span is
`prepare_windows` (18 ms/frame), which is the swapchain acquire blocking on the GPU. Main-thread
schedule self time (PreUpdate/PostUpdate ~17–20 ms/frame) is mostly pipelined-rendering wait, not
independent work.

## GPU: geometry-bound, paid twice

Pipeline statistics, p50 per frame:

- Submitted after GPU culling: **1.87 M quads, 3.75 M triangles, 11.2 M vertex invocations**,
  in 7,062 indirect draws.
- Fragments: 2.76 M on a 2.07 M-pixel target. That is ~1.3× overdraw, so fragment shading is not
  the problem.
- **There are more triangles than pixels.** Most triangles are sub-pixel, so cost is dominated by
  vertex work and triangle setup.
- The depth prepass re-submits the identical geometry: 11.2 M vertex invocations again.
- Frustum survivors 7,281 → HZB survivors 7,062: **occlusion culling removes about 3%**. It still
  needs the full prepass, which costs ~6 ms. Open terrain seen from above has few occluders, and
  cave culling already removes the underground.
- Resident geometry: 10.6 M quads in the pull arena, of which 8.2 M are LOD-cell quads.

Why vertex work is inflated (`assets/shaders/voxel_pull.wgsl:85`):

1. **Non-indexed quads.** `quad = vi / 6u` gives 6 vertex invocations per quad, and each one
   re-fetches the 8-byte record. An indexed draw with one shared static index buffer
   (`0,1,2, 2,1,3` per quad, `vi = quad*4 + corner`) makes that 4 per quad and lets the
   post-transform cache work. That is **−33% vertex invocations** in every pass.
2. **No per-face-direction buckets.** Back faces are dropped by the rasterizer (`cull_mode: Back`)
   only after all 6 vertices are shaded. For any chunk that is not straddling the camera's axis
   planes, one of ±X, one of ±Z and usually −Y are entirely back-facing. Sorting each chunk's
   records by face into 6 ranges, and having the GPU cull compute emit only the ≤3–5 front-facing
   ranges as draws, is the standard voxel win. Estimate **~35–50% fewer quads submitted** in both
   the prepass and the main pass. Unmeasured; the real share depends on the side/top mix.

## CPU: four named per-frame costs (trace, ms/frame, inflated)

| system | ms/frame | cause | fix |
|---|---|---|---|
| `voxel_pull::prepare::prepare_view_chunk_draws` | 6.1 | Builds a CPU frustum-culled draw list per view (5 views) over every chunk + LOD cell, with allocations and a `Commands` insert per view. In GpuDriven mode `DrawChunkQuads` (`voxel_pull/queue.rs:132`) draws from GPU indirect buffers and **never reads the list**. Its only consumer is the render-bench `frustum_survivors` metric. | Skip list building when `ActiveNearMode::GpuDriven`: insert an empty list, or make the `ViewQuery` optional. Compute the metric only under `RENDER_BENCH`/`VOXEL_PULL_DEBUG`, or read it back from the GPU cull counters. Trivial. |
| `meshing::visibility::cull_chunks` | 6.0 | The change gate compares all 6 frustum planes, so any camera rotation re-runs the full flood plus apply over all near chunks. | "Option 3" from the 09-19 doc: gate on the camera cell only and drop the frustum test from the flood. In GPU mode the per-view frustum cull already happens on the GPU. Small. |
| `bevy_transform::propagate_parent_transforms` | 4.5 | `interpolate_transforms` (`interpolate.rs:11`) writes `Transform` through `DerefMut` unconditionally for every `SimTarget` entity. **Every chunk is a sim entity with a fixed rigid body, so it gets `SimTarget::body`** (`bridge.rs:481`). ~68k never-moving chunk transforms are marked changed every frame. | Write only when the value changes (compare first, or `bypass_change_detection` + `set_changed` on change). Better still, don't give fixed bodies a `SimTarget`. Also removes most of `interpolate_transforms` itself (0.8) and part of `check_visibility` (1.3). Trivial. |
| `write_indirect_parameters_buffers` / `write_batched_instance_buffers` | 2.4 / 2.0 | Bevy's own mesh-preprocessing batching. We have almost no Bevy meshes, so this is probably per-view fixed overhead. | Look again only after the above. |

Minor, from code reading only (not visible in the trace): `prepare_gpu_near_view_buffers` creates
the HZB bind group every frame for every view (`voxel_pull/gpu.rs:468`), and
`extract_far_pass_inputs` clones the `BlockTextureLayers` map every frame.

## Recommended order

1. **CPU quick wins, one change each, a few lines apiece:** `interpolate_transforms`
   change-gating, skipping `prepare_view_chunk_draws` in GPU mode, and the `cull_chunks` camera-cell
   gate. Together ~16 ms/frame of the traced build. On the 6700, where the GPU has headroom, these
   are what separate 60 fps from above-60.
2. **Occlusion A/B:** add a `VOXEL_OCCLUSION=off` knob around `enable_native_occlusion`
   (`meshing/visibility.rs:583`) and run `render-bench`. If dropping the prepass + HZB saves more
   than the extra overdraw costs (it currently buys 3% culling for ~6 ms of GPU), make it off by
   default on terrain-heavy scenes. A near-chunks-only prepass is the middle ground.
3. **Indexed quads**: shader + indirect-args change (`DrawIndexedIndirect` is 5 words, so the GPU
   cull shader writes a different layout). Mechanical; −33% vertex invocations everywhere.
4. **Face-direction buckets**: mesher record order + 6 range offsets in chunk metadata + cull
   compute emitting per-direction draws. The biggest GPU lever; do it together with (3), since both
   touch the indirect-draw layout.
5. Only then, if still GPU-bound: the sub-pixel triangle density. That is a visual-quality trade
   against the ≤2.5 px ring budget the user set, so it is the user's decision, not an optimisation.

## Tooling issues found on the way

- **`just render-bench` silently benchmarks the wrong server.** If port 6466 is already taken
  (for example by a running `just run`), the bench's server panics with `AddrInUse`
  (`server/src/lib.rs:145`). The client connects to the *other* server anyway, and the run looks
  like a LOD stall (`lod_roots=96/1080`), with KEEP-UP warnings and a client netcode panic when
  that server exits. The recipe should check that the server is still alive after `sleep 1` and
  fail fast. The `perf-capture`/`trace-chrome` recipes have the same shape.
- In a wait loop, `pgrep -f "<pattern>"` matches the waiting shell itself (known footgun). Wait
  on the PID instead.

## Implemented (2026-09-22, uncommitted)

1. **`interpolate_transforms`** (`renderer/interpolate.rs`) builds the next pose and applies it
   with `set_if_neq`. Settled entities, including every fixed-body chunk, no longer dirty their
   `Transform`. Test: `settled_transform_is_not_marked_changed`.
2. **`prepare_view_chunk_draws`** (`voxel_pull/prepare.rs`) returns early in GpuDriven mode. Views
   only get an empty `ViewChunkDrawList` once, which `DrawChunkQuads`' `ViewQuery` requires.
   `NearRendererMetrics::frustum_visible_chunks` is now `Option<u32>`: it is `None` in GPU mode
   unless `VOXEL_PULL_DEBUG` is set, and the bench JSON reports `frustum_survivors: null` then. It
   is gated on `VOXEL_PULL_DEBUG`, not `RENDER_BENCH`, so that the bench measures what players
   run. Set it to compare frustum and HZB survivors.
3. **`cull_chunks` is NOT changed.** The "gate on camera cell only" idea turns a steady ~6 ms per
   turning frame into a frustum-less flood (~4–5× the cells) on every 32-unit cell crossing, about
   2/s while walking. That trades a steady cost for a hitch. A safe version needs design work:
   flood once with a widened frustum whose apex is pulled back by (cell half-diagonal)/sin(α),
   reuse it until the view turns past the margin (the containment argument is in the session
   notes), and/or run the flood off the main thread with a one-frame-late conservative result.

Verification: client type-check clean; `--lib interpolate::` (4), `voxel_pull::` (34) and
`render_bench::` (36) all pass. Render bench (same scene, n=1 each):

| | before | after |
|---|---|---|
| frame p50 / p95 / p99 | 33.5 / 58.5 / 65.0 ms | **28.3** / 56.1 / 61.7 ms |
| GPU pass sum p50 | 19.1 ms | 17.3 ms (6% fewer quads submitted this run, so this is noise) |

About 3.4 ms of the 5.2 ms p50 gain is CPU. The tail is GPU-bound, so the next lever is the GPU
list (occlusion A/B, then indexed quads + face buckets). Results are in
`target/render-bench/inv-after/`.

## Occlusion A/B → prepass + HZB off by default (2026-09-22, uncommitted)

Same scene, one build, `VOXEL_OCCLUSION` toggled (`target/render-bench/inv-occ-{on,off}/`):

| | occlusion on | **off** |
|---|---|---|
| frame p50 / p95 / p99 | 27.7 / 52.9 / 57.0 ms | **21.1 / 39.1 / 42.7 ms** |
| GPU pass sum p50 | 18.1 ms | 13.2 ms |
| main pass / prepass | 10.9 / 5.7 ms | 11.7 / 0 ms |
| main-pass fragments | 2.66 M (1.3× overdraw) | 3.66 M (1.8×) |
| submitted quads | 1.79 M | 1.95 M (+8%, the HZB's whole contribution) |
| load → ready | 15.8 s | 14.8 s |

`enable_native_occlusion` is now registered only when `VOXEL_OCCLUSION=on|1|true`
(`meshing::visibility::native_occlusion_enabled`; test `occlusion_is_off_unless_opted_in`). The
voxel_pull queue and far pass already derive their pipeline keys from the camera's actual prepass
components, so no other code changed. Re-measure in a cave-heavy scene and on the 6700 before
treating this as settled. A near-chunks-only prepass is the middle ground if dense scenes need
the HZB back.

Running total vs the start of the day: frame p50 **33.5 → 21.1 ms** (30 → 47 fps), p95
58.5 → 39.1 ms.

## Indexed quads (2026-09-22, uncommitted)

Pulled quads are now drawn indexed: 4 vertices per quad instead of 6.
- `meshing/records.rs`: `QUAD_INDEX_PATTERN` (0,1,2, 2,1,3) and `CORNER_PERM` (the per-variant
  local-vertex → corner permutation). `indexed_corners_rebuild_the_slot_triangles` proves the
  triangles and winding equal `CORNER_SLOTS`.
- Both pull shaders use `vi / 4u` + `CORNER_PERM`. The cull shader emits
  `DrawIndexedIndirectArgs` (5 words, `base_vertex = record_start * 4`).
- `GpuQuadArena::quad_indices` is one shared u32 index buffer, sized to the largest chunk seen
  and grown on demand (`quad_index_capacity` / `quad_index_data`).
- `DrawChunkQuads` binds it and issues `multi_draw_indexed_indirect[_count]` or `draw_indexed`.

Evidence: vertex invocations per quad went 6.00 → 4.00, main-pass fragments were unchanged (so
winding is correct), and main-pass GPU cost per submitted quad fell ~20–25%. The render-bench run
showed frame p50 14.6 ms / p95 23.9 ms, but the world had changed under it (39% less resident
geometry after concurrent worldgen edits), so that is not a clean A/B. A snapshot-based A/B was
abandoned. The user confirmed the speed-up anecdotally in a live run.

Still open, in order: per-face-direction buckets (the next big GPU lever, and it touches the same
indirect layout), the `cull_chunks` widened-frustum redesign, and re-checking occlusion-off in
caves and on the 6700.

## Per-face-direction culling (2026-09-22, uncommitted)

- `records::quads_to_records` stable-sorts records by face id. `records::face_ends` gives the six
  cumulative range ends, plus an "unsorted → draw whole" flag for any producer that bypasses it.
- `GpuNearChunkMeta.face_ends: [u32; 8]` (metadata is now 80 bytes; the layout test pins the
  offsets). The cull uniform gains `camera: vec4` (eye; w = 1 culls).
- `voxel_pull_cull.wgsl` emits up to one indexed command per face range (`FACES_PER_SLOT` = 6
  slots per chunk). It skips +X when `eye.x <= aabb_min.x` (and likewise for the other five).
  This is conservative because every +X face plane lies at or above the AABB min. Shadow
  cascades (orthographic, w = 0) keep all faces, so shadow casting is unchanged. The count
  buffer is now `[commands, visible_quads, visible_chunks, pad]`, since the indirect-count draw
  reads commands at offset 0.
- `VOXEL_FACE_CULL=off` is the kill switch / A/B knob.
- Tests: the cull shader validates under all four def combinations, the depth shader validates,
  and record grouping/`face_ends`, the metadata layout and telemetry all pass.

Same build, same world (`inv-fc-{off,on}`):

| | off | on |
|---|---|---|
| frame p50 / p95 / p99 | 16.4 / 26.0 / 29.1 ms | 16.8 / **20.6 / 22.7** ms |
| main pass p50 / p95 | 6.17 / 13.73 ms | 5.75 / 8.91 ms |
| submitted quads | 1.28 M | 1.05 M (−18%) |

The cut is smaller than the 35–50% estimate: LOD terrain is mostly top faces, and chunks near
the eye straddle its axis planes. It is still a clear tail win. **The p50 frame is CPU-bound
again** (GPU ~7.5 ms of 16.4 ms), so the next step is a fresh `trace_chrome` run to name the
CPU owners at this point, then the `cull_chunks` widened-frustum redesign.

## Fresh trace after all of the above + indirect-clear fix (2026-09-22, uncommitted)

Steady-state trace (`inv-trace2`, traced frame ≈17.7 ms; the 3 GB JSON was deleted after
summarising):

| system | ms/frame (traced) |
|---|---|
| `prepare_windows` (swapchain acquire, waiting on GPU/present) | 7.4 |
| `cull_chunks` | 2.9 |
| `check_visibility` | 0.5 |
| `interpolate_transforms` | 0.35 (was 0.8) |
| `prepare_view_chunk_draws` | 0.25 (was 6.1) |
| `propagate_parent_transforms` | 0.15 (was 4.5) |

GPU render-engine busy from DRM fdinfo during the bench: **60–72%**. That is ~11 ms of a 16.6 ms
frame, against only ~7.5 ms in the per-pass timings, so ~4 ms of GPU work sits outside the
timed passes (compute culls, clears).

One owner of that hidden time: `NearLatePhaseClearNode` cleared every view's whole indirect
command buffer each frame (`capacity × 6 × 20 B` ≈ 8 MB per view, ~39 MB a frame). The clear
exists so that Bevy's late prepass/shadow passes don't redraw everything. In indirect-count mode
(the one active on this adapter) zeroing the count buffer alone empties those draws, so
`clear_cull_arguments` now clears the commands only in fixed-slot mode. Frame p50 15.2 → 14.0 ms,
p95 21.3 → 20.0 ms (`inv-gpubusy` vs `inv-noclear`, same world, ±1 ms noise).

What's left, smaller now: `cull_chunks` 2.9 ms (the widened-cone reuse needs a ~50° half-angle
cone, so ~2.5× the per-run flood cost; only worth it together with an off-thread flood), and
CPU/GPU overlap (the render thread waits ~7 ms on acquire while the GPU is ~65% busy). The big
remaining GPU lever is still LOD triangle density, which is a visual trade-off for the user to
decide.

## `cull_chunks`: exact dominance pruning (2026-09-22, uncommitted)

`CullingScratch::flood` skips an arrival `(dirs D', entry e)` at a cell that already holds a
state `(D ⊆ D', e)`, or any entry if the cell is fully open (`ALL_CONNECTED`). Such an arrival
can't reach anything the dominating state won't: fewer used directions ban fewer exits, and the
successors inherit dominance. Implementation: `dominators(dirs, entry, open)`, a lazily built
`u128` mask table, checked with one AND per arrival. Proof: the randomized differential test
against the reference BFS, raised from 60 to 300 rounds, passes. The new `#[ignore]`d
`flood_timing_with_and_without_dominance_pruning` (production-sized 96×10×96 window) gives:

| | states visited | per flood | cells reached |
|---|---|---|---|
| exact-state dedupe only | 39,582 | 1.52 ms | 16,138 |
| + dominance pruning | 23,669 | 1.25 ms | 16,138 |

The gain is small because the per-run fixed work dominates: clearing the dense box, one ECS
mask lookup per chunk, and the apply loop over every chunk. The next step would be caching the
dense mask grid across runs and applying only changed cells. **Not pursued:** the trace shows
the frame is paced by GPU/presentation (render thread ~7 ms in swapchain acquire, GPU ~65%
busy), so main-thread savings no longer move the frame on this adapter.

## Where this leaves the client (Intel iGPU, `full`, 1080p, no vsync)

Frame p50 **33.5 → ~14 ms**, p95 58.5 → ~20 ms over the day. The later numbers are on a world
that changed under concurrent worldgen edits, so treat the total as indicative. The remaining
levers all need a decision or other hardware:
1. LOD triangle density (still more triangles than pixels at distance): the ≤2.5 px ring
   budget vs cost. The user's call.
2. Presentation/latency: why the render thread waits ~7 ms on acquire with the GPU ~65% busy
   (present mode, `desired_maximum_frame_latency`, compositor). Measure on the 6700 / without a
   compositor before changing anything.
3. Re-validate occlusion-off in caves and on the 6700, and check the WebGPU browser build
   (indexed + face-culled draws there go through the grouped-direct path).

## Re-measure 2026-09-23 (after Bevy 0.19, 5×5 near window, half blocks, slimes, UI)

Same scripted scene (Intel iGPU, `full` = now 25 near regions, 1080p, no vsync):
`target/render-bench/inv-base0923/bench.json` (plain) and `inv-trace0923` (traced; the 3.3 GB
JSON was deleted after summarising).

| | 2026-09-22 end (3×3) | 2026-09-23 (5×5) |
|---|---|---|
| frame p50 / p95 / p99 | 14.0 / 20.0 / 21.5 ms | **30.0 / 34.5 / 36.5 ms** |
| GPU main pass p50 | ~5.8 ms | 5.6 ms |
| GPU busy (fdinfo) | 60–72% | **25–50%** |
| load → ready | 11.4 s | 28.6 s |

**CPU-bound again.** Steady-state trace (traced frame ≈32 ms):

| system | ms/frame | note |
|---|---|---|
| `cull_chunks` | **12.0** (max 18) | Was 2.9. Runs on every turning frame. The main-thread `PostUpdate` wait (~15 ms self) is this system. |
| `prepare_windows` | 9.4 | Swapchain acquire; render thread idle-waiting. |
| `Core3d` schedule self time | 7.2 | New with 0.19's per-camera `Core3d` schedule; the owner is unknown and needs a `--thread` breakdown. |
| `interpolate_transforms` | 1.16 | Was 0.35. |
| `check_visibility_cpu_culling` | 0.8 | |
| `mark_dirty_trees` | 0.57 | |

### Why `cull_chunks` is 12 ms: the flood is 2.9, the rest is ECS bookkeeping
`flood_timing_with_and_without_dominance_pruning`, rescaled to the 5×5 window (160×10×160):
**2.9 ms/flood** (43,972 cells reached; 3.6 ms without the pruning). The other ~9 ms runs over all
~64k `ChunkGrid` entries each time: the bounding-box pass over the keys, ~64k random
`masks.get(entity)` lookups to fill the dense grid, and ~64k `chunks.get_mut(entity)` in the apply
loop.
**Fix (behaviour-preserving):** keep the dense mask grid in `CullingScratch` and update it from
`Changed<ChunkConnectivity>` / removals / grid changes. Also keep the previous reached bitset and
write `Visibility` only for cells whose reached state flipped (index → entity from the grid).
Expected: ~12 → ~3 ms. After that, the camera-cell/widened-cone gate or an off-thread flood
decides whether 3 ms per turning frame matters.

### `interpolate_transforms` walks ~64k never-moving chunks
Every chunk is a fixed body and gets `SimTarget::body` at snapshot spawn (`bridge.rs:480`).
`set_if_neq` stopped the change-detection churn, but the loop still visits every chunk.
**Fix:** don't attach `SimTarget` to chunk entities (their `Transform` is set once at spawn), or
filter the query `Without<ChunkKey>`. ~1.1 ms.

### Join: paced by the mesh-upload rate cap
`near_meshing_drained` at 28.0 s for 54,940 chunks. The 5×5 window has ~2.8× the 3×3 chunks, and
`MESH_UPLOADS_PER_SECOND` = 3000 caps the drain at ≥18 s regardless of workers. Raise the rate,
mesh nearest-first for early playability, or both. The per-frame ceiling (100) at 30 fps is also
exactly 3000/s, so the frame rate and the cap bind together.

### GPU
The main pass is 5.6 ms and the GPU is 25–50% busy, so there is headroom at this scale. The GPU
levers (LOD density) wait until the CPU is fixed.

### Implemented 2026-09-23 (uncommitted): `cull_chunks` grid cache + diffed write-back
`GridCache` (`meshing/visibility.rs`) is a flat key/entity/mask table plus the grid bounding box.
It is rebuilt only when `ChunkGrid` changes; `Changed`/removed `ChunkConnectivity` is patched
per entity (removals first, then changes). The write-back is diffed against the last applied
state per entry, so a turning camera touches only chunks whose visibility flips. Safe because
`cull_chunks` is the only writer of chunk `Visibility`. The F4 disable path invalidates it.
`CullingStats::grid_rebuilds` counts rebuilds.

Tests: all 22 `meshing::visibility::` pass, plus the new
`mask_only_changes_patch_the_cached_grid_without_a_rebuild`. Mutation check: with the
per-entity patch disabled that test fails. So does the initial wall-hides-room wait, because in
production masks arrive via the patch path after the chunk is already in the grid.

Bench (same world, `inv-base0923` → `inv-cullcache`): frame p50 **30.0 → 25.7 ms**, p95
34.5 → 31.1, GPU unchanged (~7 ms), GPU still only 30–50% busy, so the frame stays CPU-bound.
Next: the `Core3d` 7.2 ms owner (one traced run with `--thread`), chunk `SimTarget` (~1.1 ms),
then the flood itself (~3 ms/turning frame: off-thread or reuse-within-cell).

### 2026-09-23 continued: the real flood, epoch-stamped scratch, chunk interpolation skip (uncommitted)

**The real flood is much bigger than the synthetic one.** A `perf` profile of steady state
(release build, 8 s) put `cull_chunks` at 47% of all client CPU samples, plus most of a 6.6%
`memset`. The new `[client-cull]` DEBUG report (`PERF_LOG=1`, every 128 floods: avg flood ms,
box, reached cells, states, skipped, grid rebuilds) shows the box is **162 × 35 × 162 = 918k
cells**. The 5×5 grid's bounding box never hits the Chebyshev cap. Each flood reaches 115k–235k
cells for only 66k chunks, so most of the walk is open sky inside the box.

**`Core3d` 5.3 ms of self time** has no hot code in the `perf` profile, so it is off-CPU waiting
inside the per-camera schedule, not work. Not pursued.

Landed (exact; flood results unchanged):
1. **Epoch-stamped scratch.** A per-cell `u32` stamp replaces clearing ~17 MB of
   `visited`/`in_frustum` every flood, and a cell resets on its first touch.
2. **Persistent dense mask grid.** It is refilled only when `(box, GridCache::generation)`
   changes; the generation bumps on rebuild and on each real mask change.
3. **`interpolate_transforms` skips keyed chunks** (`Without<ChunkKey>`). A keyed chunk that ever
   receives `SetEntityPosition` snaps its `Transform` in `drain_region_updates`.

Tests: `meshing::visibility::` 22 pass. The differential test now also floods from a second cell
with an empty mask iterator (reused grid) and compares it with the reference BFS; a mutation that
skips the per-cell reset makes it fail. `interpolate::` 5 pass (new
`keyed_chunks_are_not_interpolated`), `renderer::bridge::` 25 pass.

| bench (same scene) | frame p50 / p95 / p99 | flood avg |
|---|---|---|
| `base0923` (start of the day) | 30.0 / 34.5 / 36.5 | 7–11 ms |
| `cullcache` (grid cache) | 25.7 / 31.1 / 33.8 | 7–11 ms |
| `epoch` (+ stamped scratch, persistent masks) | 23.0 / 27.6 / 29.3 | 5.0–8.6 ms |
| `nointerp` (+ chunk interpolation skip) | **22.6 / 27.8 / 29.2** | – |

**What's left needs a decision.** The flood still costs ~5–8.5 ms per turning frame, scaling
with reached cells, mostly sky. Options:
- (a) **Reuse per camera cell.** Flood once without the frustum and reuse it until the camera
  changes cell. Turning becomes free, but a cell crossing costs a full unrestricted flood (a
  multi-frame hitch risk).
- (b) **Off-thread flood with a widened frustum.** Run it on the async pool from the persistent
  mask grid, apply a frame late, and widen the view cone (e.g. +15°) so a fast turn doesn't show
  a one-frame hole at the screen edge.
- (c) **Collapse empty sky.** Treat all-absent macro-blocks as single nodes. Exact, but a real
  redesign of the dir-state BFS.
- (d) **Cap the box height to a band around the camera.** Cheap, but could hide chunks you can
  only see over tall terrain.

### Cave culling off by default (2026-09-23, uncommitted; user: "there are currently no caves")
`renderer/mod.rs` inserts `CullingConfig { graph_enabled: false }`. `Default` stays `true`, which
the visibility tests rely on; F4 and the F10 dev panel turn it back on. The disabled path now
restores `Inherited` once, when the switch flips or a valid cache is dropped, instead of walking
every chunk each frame. The 22 visibility tests pass.

| bench | frame p50 / p95 / p99 | GPU main p50 | submitted quads | active chunks |
|---|---|---|---|---|
| `nointerp` (culling on) | 22.6 / 27.8 / 29.2 | 5.54 ms | 930k | 15.7k |
| `nocave` (culling off) | **14.7 / 21.8 / 24.4** | 4.65 ms | 860k | 65.4k |

There is no GPU regression. On surface terrain the chunks cave culling hid were already dropped
by the GPU frustum and face cull. Re-enable, and revisit the flood options (a)–(d), once caves
exist.
