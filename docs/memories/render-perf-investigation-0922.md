---
name: render-perf-investigation-0922
description: INVESTIGATED 2026-09-22; CPU fixes 1+2 IMPLEMENTED same day (uncommitted, p50 33.5→28.3 ms) — full/1080p/no-vsync Intel iGPU = 33.5 ms p50, GPU geometry-bound (tris > pixels, paid twice by prepass) + 4 named CPU costs; ranked fixes in docs/superpowers/specs/2026-09-22-client-render-perf-investigation.md
metadata:
  type: project
---
Measured with render-bench (gpu+raster, full, 1920x1080, no vsync, Intel iGPU; 6700 not attached). Frame p50 33.5 / p95 58 ms. GPU: main 11.4 + depth prepass 6.0 ms; 3.75M tris for 2.07M px; HZB culls only ~3% (7281→7062) yet needs the full prepass; quads non-indexed (6 VS invocations each), no per-face-direction buckets.
CPU (trace): prepare_view_chunk_draws 6.1 ms (list unused in GpuDriven), cull_chunks 6.0 (frustum-plane gate reruns on every turn), propagate_parent_transforms 4.5 (interpolate_transforms rewrites ~68k fixed-body chunk Transforms every frame via SimTarget::body at bridge.rs:481).
Ranked plan: CPU quick wins → VOXEL_OCCLUSION A/B → indexed quads + face buckets together. Sub-pixel LOD density is the user's visual call, not an optimisation.
Tooling trap: render-bench server panics AddrInUse if a server already runs, and its client silently joins that server (looks like a LOD stall). Wait on PIDs, not pgrep -f.

**Why:** don't re-measure or re-derive; start from the doc.
**How to apply:** pick up the ranked list; compare against target/render-bench/inv-plain/bench.json at the same scale. Related: [[perf-capture-tooling-landed]], [[lod-dh-voxy-research]].

**Implemented 2026-09-22 (uncommitted):** interpolate_transforms set_if_neq; prepare_view_chunk_draws early-out in GpuDriven (frustum metric now Option, only with VOXEL_PULL_DEBUG). cull_chunks deliberately NOT changed: the camera-cell-only gate would make every cell crossing a full frustum-less flood (a hitch); the safe design is a widened frustum with a pulled-back apex, reused within a turn margin, and/or an off-thread flood. Next: GPU (occlusion A/B, indexed quads + face buckets).

**Occlusion default flipped 2026-09-22 (uncommitted):** A/B showed the depth prepass + HZB cost ~5.7 ms to cull ~8% of quads; now off unless VOXEL_OCCLUSION=on (native_occlusion_enabled, registration-gated in renderer/mod.rs; CLAUDE.md updated). p50 27.7→21.1, p95 52.9→39.1 ms. Cumulative today: 33.5→21.1 ms p50. Not yet re-measured in caves or on the 6700.

**Indexed quads landed 2026-09-22 (uncommitted):** 4 VS/quad via CORNER_PERM + shared index buffer (GpuQuadArena::quad_indices), DrawIndexedIndirectArgs in the cull shader; the user confirmed the speed-up anecdotally and said not to A/B it further. Next GPU lever: per-face-direction buckets. Lesson: a rsync'd snapshot client ran empty (unknown cause) — for A/Bs, prefer a runtime toggle over tree snapshots, and never snapshot the repo root without excluding perf.data (6 GB).

**Face-direction culling landed 2026-09-22 (uncommitted):** records face-sorted, GpuNearChunkMeta.face_ends (80 B), per-face indexed commands (6 per slot), camera.w gate (shadows keep all faces), VOXEL_FACE_CULL=off switch. Same-world A/B: p95 26.0→20.6 ms, submitted quads −18%; p50 ~16.5 ms is CPU-bound again (GPU ~7.5 ms) → next: fresh trace, then cull_chunks redesign.

**Late 2026-09-22:** a fresh trace showed first-party CPU costs are small (cull_chunks 2.9 ms is the largest); the render thread waits ~7 ms on swapchain acquire; GPU fdinfo busy was 60–72%, ~4 ms of it outside the timed passes. Fixed: the count-mode late clear no longer zeroes the ~39 MB/frame indirect buffers (p50 15.2→14.0 ms). Next options: off-thread + widened-cone cull_chunks, CPU/GPU overlap, LOD density (the user's call).

**cull_chunks dominance pruning landed (uncommitted):** exact (differential test 300 rounds), −40% states but only −18% per flood (1.52→1.25 ms); fixed per-run work dominates; stopped because the frame is GPU/present-paced (acquire wait ~7 ms, GPU ~65%). Day total: p50 33.5→~14 ms. Remaining levers need the user's call (LOD density) or other hardware (6700, present/latency).

**2026-09-23 re-measure (5x5 window + Bevy 0.19):** p50 14→30 ms, CPU-bound (GPU 25–50%). Owners: cull_chunks 12 ms (flood only 2.9 ms; ~9 ms = per-run ECS over ~64k chunks → cache dense mask grid + diff-apply), Core3d self 7.2 ms (unknown, new in 0.19), interpolate_transforms 1.16 ms (chunks still carry SimTarget, bridge.rs:480). Join 28.6 s, paced by MESH_UPLOADS_PER_SECOND=3000 over 55k chunks. Nothing implemented.

**cull_chunks GridCache landed 2026-09-23 (uncommitted):** cached flat grid table + per-entity mask patch + diffed write-back; p50 30.0→25.7 ms; tests 22 green + mutation-checked. Still CPU-bound; next: Core3d 7.2 ms owner, chunk SimTarget, flood reuse/off-thread.

**2026-09-23 continued (uncommitted):** the real flood box is 162x35x162 = 918k cells, reaching 115–235k (mostly sky); perf shows cull_chunks at 47% of client CPU. Landed exact fixes: epoch-stamped scratch, persistent dense mask grid keyed on GridCache::generation, chunk interpolation skip (Without<ChunkKey> + snap on SetEntityPosition). p50 30.0→22.6 ms. Core3d 5.3 ms is off-CPU wait, not work. Flood left at 5–8.5 ms/turning frame; options (a) per-cell reuse, (b) off-thread + widened cone, (c) collapse empty sky, (d) height band; the user's call. `[client-cull]` report under PERF_LOG=1.

**Cave culling OFF by default 2026-09-23 (user: no caves yet; uncommitted):** production inserts CullingConfig{graph_enabled:false} (Default stays true for tests; F4/F10 re-enable). Bench p50 22.6→14.7 ms, no GPU regression (submitted quads unchanged; GPU frustum+face cull already dropped what it hid). Revisit flood options (a)-(d) when caves exist.
