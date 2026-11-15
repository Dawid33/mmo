---
name: chunk-loading-speed-investigation
description: "2026-11-02 cross-chunk brick burial implemented (≈half the join brick entities); earlier recs #1-#5 landed 2026-09-23"
metadata:
  node_type: memory
  type: project
  originSessionId: 3cfb14fc-0941-435a-95e1-acad3e91fdb4
  modified: 2026-09-22T18:24:18.897Z
---

2026-09-22, user asked to "investigate speeding up chunk loading speed, including meshing", then paused it before any measurement ran ("other benchmarks running").

State at pause:
- Latest captures (2026-09-20, Intel, `full`): first terrain ~1.0 s, LOD all received ~8.7 s, near meshing drained ~13 s (19k chunks) → paces `world_ready` 13.5–14.7 s. `in_flight` near its cap in `load_progress`.
- FOUND: `DEFAULT_CLIENT_WORKER_THREADS = 4` sets Bevy `max_total_threads = 4`; Bevy 0.18.1 splits that io 1 / async_compute 2 / compute 1. ALL join jobs (near mesh, LOD mesh, snapshot-voxel derive, far-voxel bricks) share 2 threads; ECS gets 1 compute thread. The comment in `crates/client/src/main.rs` says "half the cores" — stale. The cap came from the 2026-07-16 runtime-thread-caps design (deliberate: don't eat all CPUs). The first experiment is `VOXEL_CLIENT_WORKER_THREADS=8/12` capture.
- Algorithmic levers seen in code (unmeasured): a chunk is 2×2×2 blocks of 16³ voxels, yet each chunk pays a dense 32³ `derive_voxels` + RLE, a 34³ `gather_padded_rle`, `Occupancy::new` over 39k cells, and a scalar `face_connectivity` flood fill (Vec<bool> + tuple stack over 32k cells). Candidates: RLE straight from blocks, skip fully-occluded chunks, bitset/block-level connectivity.
- Written but NOT run: `crates/client/examples/mesh_bench.rs` (3×3 window at WORLD_SEED, per-stage µs/chunk + thread scaling; `./target/release/examples/mesh_bench threads=1,2,4,8,12`, already built in release). To build it, `renderer/mod.rs` line 14 went from `pub(crate) mod bridge` to `pub mod bridge` (uncommitted).

**Bench RUN 2026-09-22 (USB-C power but clocks 1.4-3.2 GHz):** 28 394 chunks in the 3x3 window; per chunk derive+rle 64.6 µs, gather 43.0, mesh_quads 49.1, face_connectivity 68.7, mesh task 161 µs; 1/2/4/8/12 threads → 6.2k/12.0k/22.4k/37.9k/47.0k chunks/s. 30% of chunks have zero quads, 30% are uniform solid. CONCLUSION: near-mesh CPU is ~8x faster than the observed ~1.5k/s join drain — the pacing is elsewhere (suspects: LOD mesh tasks sharing the 2-thread pool, and the per-frame upload/insert budgets at ~36 join fps). Next: perf-capture with VOXEL_CLIENT_WORKER_THREADS=8 (GUI, ask first).

**Captures 2026-09-22 (Intel, full, 4 vs 8 threads):** world_ready 15.37 → 12.52 s (−19%), near drained 14.66 → 11.85 s, early join fps ~20 → ~47. Tail drains at exactly MESH_UPLOADS_PER_SECOND=3000/s → the per-frame budgets (tied to join fps via ceilings 100/160) pace the join, plus ~2 s main-thread `mark_remesh_dirty` zone hashing. Remesh repeats only ~10%. Ranked recs in docs/superpowers/specs/2026-09-22-chunk-loading-speed-findings.md (raise thread cap = user's call; boundary_zones off-thread; then raise budgets; skip buried chunks; bitset connectivity). Nothing implemented.

**How to apply:** resume by running the built bench when the machine is idle (ask first), then a worker-thread capture. See [[perf-capture-tooling-landed]], [[meshing-startup-burst]], [[check-before-running-benchmarks]].

**2026-09-23 (uncommitted): recs #2 + #3 IMPLEMENTED at the 5x5 window.** Zone hashes are cached in VoxelData (with_boundary_zones, warmed in the snapshot-voxel task; equality ignores the cache); rates 3000→6000 uploads/s and 4800→9600 inserts/s, ceilings unchanged. world_ready 28.0→18.8 s, join main stalls 33→1, join fps unchanged ~60. Join now paced by snapshot derivation on the 2-thread pool (~4k/s) then the 6000/s cap. Next: thread cap (user's call), cheaper derive (#5), skip buried chunks (#4).

**2026-09-23 (uncommitted): recs #1, #4, #5(derive) IMPLEMENTED too.** Client thread default (cores/2).clamp(4,8), user approved; VoxelData::from_blocks (block-direct RLE, 500-round exactness test); is_buried in-task shortcut (400-round equivalence test vs the full mesher). 5x5 world_ready 18.8 → 14.6 s (day total 28.0 → 14.6), steady p50 15.1 → 12.4 ms with 8 threads. Tail now = the 6000/s upload cap.

**2026-11-02 (uncommitted): cross-chunk brick burial.** At 16³ chunks the join is paced by brick ENTITIES (~331k at 512 m, world_ready ~58 s per specs/2026-10-31-same-fps…; play recipes now 256 m). `just brick-census` (worldgen example) on worlds/debug-embarked at 256 m: 83k–107k spawned per region, 48–55% can never show a face (25–34% buried by the neighbour chunk, the rest by implied ground under the band). Implemented in client `renderer/bricks.rs`: snapshot batches bury face bricks via `snapshot_across` (GameData chunk_index + ground recipe); `SimChunkIndex` + `recheck_buried` at landing; `expose_across`/`expose_edited_faces` spawn skipped bricks when a neighbour lands/edits/clears/despawns with air. Live join not yet measured; the next pacers are SNAPSHOT_VOXELS_PER_SECOND 9600 and MESH_UPLOADS_PER_SECOND 6000.
