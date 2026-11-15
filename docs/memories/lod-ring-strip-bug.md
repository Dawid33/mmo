---
name: lod-ring-strip-bug
description: FIXED + CONFIRMED LIVE 2026-09-19 (uncommitted at the time; regression test run GREEN with the fix and RED against the re-introduced bug) — neighbour LOD rendered as strips because queue_far_brick_tasks wiped the raster LOD queue in raster mode
metadata:
  type: project
---

Symptom (user, Intel iGPU, `world=small`): regions around the home region rendered as thin strips; crossing into one loaded it properly. Pre-existing — streamed LOD rings were never live-smoked ([[streamed-lod-rings-design]]).

Root cause: `far_voxel::lifecycle::queue_far_brick_tasks` runs right after `meshing::lod::spawn_lod_mesh_tasks`; in raster mode it called `LodBuildQueue::remove(entity)` on every `Changed<LodVoxelData>` cell, and `remove` clears BOTH lanes — so everything beyond the first 64-task batch of each LOD root was dropped the frame it was enqueued. Evidence: `[perf-lod]` dump showed every root with ~480 cells but exactly `raster_ready=64`. Fix: don't touch the queue in raster mode. Regression test `raster_mode_meshes_every_cell_of_a_root_larger_than_the_task_budget` in `lifecycle.rs` (run 2026-09-19: green with the fix, red — "only 8 of 24 cells" — with the bug re-introduced).

Confirmed live (capture `20260919T155052Z-Intel-small`): 12 012/12 012 cells raster-ready, full coverage. Honest `small` client numbers with a full ring: ~23–26 ms frames (39–45 fps) on the Intel iGPU, GPU ~7 ms (30%) → still CPU-bound; 16 394 `ChunkQuads` entities extracted per frame — suspect `voxel_pull::extract_chunk_quads` re-inserting `ExtractedChunkQuads` for every entity every frame (unconfirmed; needs `trace-chrome`).

**Why:** `meshing/queue.rs::pipeline_app` chains the LOD systems without the brick system in between, so no harness reproduced the production order.

**How to apply:** all client-side `small`/`full` perf numbers taken before this fix (2026-09-19 captures) measured a ~13%-populated LOD ring — re-capture before using them ([[perf-capture-tooling-landed]]). Diagnostics that found it and stay useful: `[perf-lod]` section in `perf-capture` summaries (`join_timeline::log_lod_render_state`), `VOXEL_PULL_DEBUG=1`, and `cargo run --release -p worldgen --example lod_coverage -- rx rz factor` (what the server sends). When a harness test passes but live rendering is wrong, compare the harness system chain with `renderer/mod.rs`.
