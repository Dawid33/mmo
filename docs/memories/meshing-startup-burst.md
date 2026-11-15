---
name: meshing-startup-burst
description: "greedy_quads dominance in client profiles is a one-time join burst (9 regions x 1088 dense subchunks), NOT per-tick re-meshing — measured live 2026-07-08"
metadata: 
  node_type: memory
  type: project
  originSessionId: 43d383f9-5038-45b0-9ed1-3dfb10a24d4e
---

Measured live 2026-07-08 (branch home-only-prediction, debug build, 30s run with a
temp counter in `queue_meshing`): the client queues exactly **1088 chunk mesh jobs
once per near-region load** (9 loads for the 3x3 window, staggered ~300-400ms apart
over the first ~5-22s), and **zero mesh jobs afterward** in steady state. So a
whole-process cpu-clock profile showing `block_mesh::greedy_quads` at ~65% is the
join burst aggregated over the run, not continuous re-meshing.

**Why 1088:** 64 columns x 17 *dense* subchunks per column. Air subchunks are
already skipped (`Column::materialized_sub_chunks`), but the density worldgen makes
~17 dense (mostly all-solid underground stone) subchunks per column, each paying a
full independent greedy pass. Total at join: 9 x 1088 = ~9,800 greedy passes.

**How to apply:** don't hunt for a per-tick re-mesh bug — `VoxelData` is only
written on snapshot receipt (`SetVoxelComponent` is never emitted anywhere as of
this date). Optimization levers instead: cheap uniformity pre-scan before greedy
(all-solid chunk → emit 6 boundary quads directly, or skip via neighbor
knowledge), or mesh columns instead of subchunks. The burst also competes on CPU
with the join-time keep-up window in [[client-prediction-runaway]].

**UPDATE 2026-07-09 — largely RESOLVED by [[voxel-rendering-phase1-landed]]:**
the block-mesh mesher was replaced by a binary greedy mesher measured 3.7×
faster (release) on exactly this workload, and the per-change task dump became
a distance-prioritized queue with in-flight/upload budgets, so the burst
amortizes over frames instead of stalling. `queue_meshing` no longer exists
(now `mark_remesh_dirty`/`spawn_mesh_tasks` in `renderer/meshing/queue.rs`).
