# Chunk loading speed — findings (2026-09-22)

Question: where does the near-chunk join time go, and what would speed up loading (incl. meshing)?
Machine: i7-12700H, Intel iGPU pinned (`adapter=Intel`), `world=full`, release, on the USB-C
supply but clocks stayed at 1.4–3.2 GHz (no power clamp seen). n=1 per configuration.

## 1. Mesher CPU cost is not the problem

`crates/client/examples/mesh_bench.rs` (new) meshes the production 3×3 window (WORLD_SEED) through
the client's own functions, off the render path:

| per chunk, 1 thread       | µs    |
|---------------------------|-------|
| `derive_voxels` + RLE (snapshot-voxel task) | 64.6 |
| `gather_padded_rle`       | 43.0  |
| `mesh_quads`              | 49.1  |
| `face_connectivity`       | 68.7  |
| mesh task total           | 161   |

28 394 chunks, 30% zero-quad (buried), 30% uniform solid, 65% unchiselled, 2.39 M quads.
Scaling: 1/2/4/8/12 threads → 6.2k / 12.0k / 22.4k / 37.9k / 47.0k chunks/s.
The whole window's near meshing is ~2.4 s of work on the 2 workers the client has, and the join
takes ~15 s → meshing CPU does not pace the join.

## 2. The client has 2 meshing workers, and 1 ECS thread

`DEFAULT_CLIENT_WORKER_THREADS = 4` sets Bevy's `max_total_threads`; Bevy 0.18.1 splits that into
io 1 / async compute 2 / compute 1. Near meshing, LOD meshing, snapshot-voxel derivation and far
bricks all share the 2 async threads, and the parallel ECS executor gets one compute thread.
(The `main.rs` comment "normal clients take half the cores" is stale.)

## 3. Captures: default 4 threads vs `VOXEL_CLIENT_WORKER_THREADS=8`

`target/perf-capture/20260922T183234Z-Intel-full` (4) vs `20260922T183341Z-Intel-full` (8):

| event                    | 4 threads | 8 threads |
|--------------------------|-----------|-----------|
| first_near_mesh          | 1.05 s    | 0.64 s    |
| snapshot_voxels_applied  | 7.70 s    | 6.55 s    |
| lod_all_received         | 7.08 s    | 8.31 s    |
| near_meshing_drained     | 14.66 s   | 11.85 s   |
| world_ready              | 15.37 s   | 12.52 s (−19%) |
| join frames (first ~3 s) | ~20 fps   | ~47 fps   |
| join frames (later)      | ~30–38 fps| ~42–45 fps|
| client avg / peak CPU    | 114 / 312%| 147 / 573%|

`load_progress` series (4 threads): remesh queue 6.2k → 11.7k → 17.6k (grows while snapshot voxels
are inserted at ~4800/s) → 15.9k → 10.8k → 5.0k → 0; the LOD raster queue (`lod_build_queue`)
peaks at ~10k at the same time. With 8 threads the tail drains 10.9k → 4.9k in 2 s ≈ 3000/s —
exactly `MESH_UPLOADS_PER_SECOND`.

Remeshing is not the multiplier: ~22k record uploads for 19 960 non-empty chunks (~10% repeats from
neighbour-zone re-dirtying), quad total matches the bench's single pass.

## 4. What paces the join

1. **The per-frame budgets.** Near uploads are `3000/s × dt` clamped to `32..=100` per frame and
   snapshot inserts `4800/s` clamped to `64..=160`; the mesh in-flight cap is `2 × uploads`.
   The tail of the join runs exactly at 3000/s. Below ~30 fps the ceilings bite (100/frame at
   20 fps = 2000/s), so the budget is effectively tied to the frame rate.
2. **The join frame rate, and the thread cap feeds it.** With 4 total threads the first seconds run
   at ~20 fps. Going to 8 threads (io 2 / async 4 / compute 2) doubled early fps, which is where
   most of the 2.9 s gain came from — more budget per second, not faster meshing.
3. **Main-thread work per chunk.** `mark_remesh_dirty` (26 crc32 boundary-zone hashes per inserted
   chunk, streamed through the RLE) is ~2.0 s of main-thread time per join (`main_stall` sum, max
   19–20 ms/frame). This is the cost the budget ceilings exist to bound.
4. The LOD raster lane (~10k items) shares the async pool and the same budget numbers.

## 5. Recommendations (ranked, none implemented)

1. **Raise the default Bevy thread cap** (e.g. 8, or `cores/2` clamped) — −19% `world_ready` for a
   config change; it also relieves the 1-thread ECS executor. It reverses the 2026-07-16 "4 threads"
   decision (don't eat the desktop), so it is the user's call.
2. **Move `boundary_zones` off the main thread**: compute the 26 zone hashes in the snapshot-voxel
   task alongside `VoxelData::new` and insert them with it. Removes ~2 s of main-thread time per join
   and is what makes a higher insert/upload rate affordable.
3. **Then raise `MESH_UPLOADS_PER_SECOND` / `SNAPSHOT_VOXELS_PER_SECOND` and their ceilings**,
   re-capturing join frame times (lesson from 2026-09-20: dt-scaled budgets need a tight ceiling).
4. **Skip buried chunks before they reach the pool** (30% produce zero quads but pay 161 µs + an
   in-flight slot): uniform-solid chunk whose 6 face neighbours present solid boundary planes →
   connectivity 0, no records, no task.
5. **Per-chunk mesher cuts** (only once 1–4 make the pool the limit): bitset flood fill for
   `face_connectivity` (43% of the task), RLE/zone data straight from the 8 blocks instead of a dense
   32³ `derive_voxels` for unchiselled chunks.
6. Not measured here: the per-item cost of the LOD raster lane (~10k items per join).

## 2026-09-23: recommendations 2 + 3 implemented (uncommitted), measured at the 5×5 window

- **#2 boundary zones off the main thread.** `VoxelData` carries an optional cache of its 26
  zone hashes: `with_boundary_zones()` fills it, `edit`/`new` start without it, and equality
  ignores it. The snapshot-voxel task warms it off-thread; `meshing::queue::boundary_zones` uses
  the cache, else computes (`compute_boundary_zones`). The queue test checks the cache equals
  on-demand hashing and that `edit` drops it.
- **#3 rates doubled.** `MESH_UPLOADS_PER_SECOND` 3000 → 6000 and
  `SNAPSHOT_VOXELS_PER_SECOND` 4800 → 9600. The per-frame ceilings (100/160) are unchanged.

Render bench (Intel iGPU, `full` = 5×5, 1080p): `inv-nocave` → `inv-zones` → `inv-rates`.

| | join main stalls | near drained | world_ready | join fps | steady p50/p95 |
|---|---|---|---|---|---|
| before | 33 (421 ms; 32× `mark_remesh_dirty`) | 27.6 s | 28.0 s | ~60 | 14.7/21.8 |
| zones off-thread | 1 (34 ms, `drain_client_updates`) | 28.5 s | 28.9 s | ~60–74 | 15.2/21.8 |
| + rates ×2 | 1 | **18.4 s** | **18.8 s** | ~58–64 | 15.1/20.5 |

What paces the join now:
- **0 to ~13 s:** snapshot-voxel derivation on the 2-thread async pool, at ~4k chunks/s, below the
  9.6k/s budget.
- **After ~13 s:** the 6000/s upload cap.

Next levers: raise the Bevy thread cap (#1, the user's call); cheaper derivation for unchiselled
chunks (RLE straight from the 8 blocks, #5); skip buried chunks (#4); then the upload cap again.

## 2026-09-23: recommendations 1, 4, 5 implemented (uncommitted)

- **#1 thread cap.** `default_client_worker_threads(cores) = (cores/2).clamp(4, 8)` replaces the
  fixed 4 (`crates/client/src/main.rs`; that is 8 on the 20-thread dev box).
  `VOXEL_CLIENT_WORKER_THREADS` still overrides it, with a minimum of 4. The `render-bench` recipe
  still passes `threads=4` on purpose.
- **#5 (derivation half) block-direct RLE.** `VoxelData::from_blocks(blocks, chisel)` builds the
  runs straight from the 8 blocks, with no dense 32³ array; chiselled blocks contribute their
  per-row materials. It is used by the snapshot-voxel task.
  `from_blocks_matches_dense_derivation_exactly` checks 500 random chunks with chiselled cuboids
  run-for-run against `VoxelData::new(derive_voxels(..))`.
- **#4 buried-chunk skip.** `meshing::queue::is_buried`: a chunk with no air whose 6 face
  neighbours exist and have no air on the shared plane. It returns `buried_mesh_output()` (no
  mesh, no records, connectivity 0) before the padded gather, mesher and connectivity flood. The
  check runs in the task, not on the main thread. `buried_shortcut_matches_the_full_mesher`
  compares 400 random configurations against the full mesher in both render modes.

| bench (Intel iGPU, 5×5, 1080p) | voxels applied | near drained | world_ready | steady p50/p95 |
|---|---|---|---|---|
| `rates` (4 threads) | 11.1 s | 18.4 s | 18.8 s | 15.1 / 20.5 |
| `derive4` (+#4 +#5, 4 threads) | 10.0 s | 16.7 s | 17.1 s | 15.1 / 20.4 |
| `derive8` (+#1, default = 8) | **8.0 s** | **14.3 s** | **14.6 s** | **12.4 / 17.4** |

Across the day at the 5×5 window, `world_ready` went 28.0 → 14.6 s. The tail after voxels are
applied (~6 s) drains at the 6000/s upload cap; that is the next knob, to raise while re-checking
join fps.
