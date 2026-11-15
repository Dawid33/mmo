---
name: water-static-layer-landed
description: Static water layer IMPLEMENTED 2026-11-07 (develop 299a721..3036f22): recipe water in ColumnGround, swimming, buoyancy, water pass, LOD water; live checks pending
metadata:
  type: project
---
Static water layer (plan docs/superpowers/plans/2026-11-06-water-static-layer.md) landed 2026-11-07 on develop, commits 299a721..3036f22, subagent-driven with per-task + final opus review. Water is recipe-only (`ColumnGround.water_top`), never chunk blocks; one rule `game::static_water_units`; REGION_FILE_VERSION 23 (old saves regenerate).

Deviations from the plan Dawid should know: idle swimmer at Head floats up (`SWIM_FLOAT_UP_SPEED`, no sink); `SWIM_VERTICAL_ACCEL` 45 not 12; `WATER_LINEAR_DAMPING` 6 not 2; swimmers restore in place; `player_spawn_y` lands on the water top.

**Why:** next step is the water flow plans; static limits are in TODO.md.
**How to apply:** don't re-plan the static layer. Pending Dawid: live visual/swim-feel check (`just adapter=Intel run --reset`), WebGL2 browser check, PERF_LOG 256 m run. Pre-existing unrelated client test failures: atmosphere shadows_reach_past_the_farthest_commander_zoom, 8 lod_rings:: (harness lacks rebuild_lod_tiles). Related: [[entity-seam-crossing-handoff]], [[rapier-water-investigation]].
