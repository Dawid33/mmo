---
name: cell-lod-flicker-resolved
description: RESOLVED 2026-09-25 — cell-mode LOD grass/dirt flicker was LOD skirts overlapping coplanar walls; near/LOD sky gap was un-remeshed neighbours of masked cells
metadata:
  node_type: memory
  type: project
  originSessionId: 03d0f4ec-da04-4b4f-8728-3e2556038250
  modified: 2026-09-25T17:57:19.968Z
---

LOD flicker in large cell-mode regions (`just region_chunks=1024 cells=16 run`) is FIXED and live-confirmed by the user 2026-09-25 (develop 3acd148, 396fe15; tools 184ed23). Don't re-investigate.

- Flicker root cause: `meshing/lod.rs` `lod_quads` skirted every border side quad, so grass-over-dirt walls overlapped coplanar; idle camera wobble (~0.002 u per 50 ms tick, sim pose moves while standing) flipped depth. Fix: `wall_below` skips the skirt.
- Sky gap: `mask_home_lod` now re-inserts neighbours' `LodVoxelData` on a mask flip.
- Steady red dirt stripes along contours = whole-block risers at factor 16, expected.
- Method that worked: F6/F7 hide LOD/near to isolate the layer; `[client-still]` (PERF_LOG=1) proved camera moves / geometry static — "flicker while still" means z-fight + camera jitter.
- Open: the idle camera wobble itself (unexplained sim-pose drift); pre-existing failing test `lod_rings::tests::plain_removal_waits_for_a_fully_meshed_near_region` (fails at HEAD 4c0c545 too).

Related: [[island-regions-cell-loading]], [[brick-far-renderer-unwanted]].
