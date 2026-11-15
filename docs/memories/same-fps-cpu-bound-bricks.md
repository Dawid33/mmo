---
name: same-fps-cpu-bound-bricks
description: 2026-10-31 6700 vs Intel same fps = CPU-bound (34 ms/frame, GPU 1.6/3.5 ms); bricks direct ChildOf(region root) → full transform walk per frame; spawn_mesh_tasks full sort
metadata:
  type: project
---
INVESTIGATED 2026-10-31 (findings: docs/superpowers/specs/2026-10-31-same-fps-on-6700-and-intel-findings.md). adapter=6700 pin WORKS; client is main-thread CPU-bound at 512 m (~343k brick entities): propagate_parent_transforms ~14.7 ms/frame (one moving descendant re-walks all direct children of the region root), spawn_mesh_tasks ~13.4 ms/frame during join (whole-queue rank+sort). All monitors hang off the Intel iGPU (6700 frames PRIME-copied), 60 Hz vsync. FIXED same day: bodies root per region (RegionRoots::bodies) + pick_nearest RankCache → 6700 34→16.7 ms (vsync), Intel 35→~18 ms; join frames still ~24 ms.

**Why:** don't re-measure; this is why GPU swaps don't change fps.
**How to apply:** implement the terrain-group parent + partial-select fixes; related [[egpu-adapter-selection]].
