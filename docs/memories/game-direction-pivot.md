---
name: game-direction-pivot
description: 2026-10-20 direction history MMO→RTS→Dwarf Fortress; develop REBUILT from ds/oldMMOCode (multi-region code is live again); RTS work on ds/rtsPivot
metadata:
  node_type: memory
  type: project
  originSessionId: a46897df-a729-4143-828f-939d05252892
  modified: 2026-10-20T15:14:02.497Z
---

2026-10-20, in one day:
1. MMO dropped → Nexus Wars-style RTS on one 256 m map; Phase 1 collapsed the world to one
   region (deleted multi-region, handoff, LOD, cells), Phase 2 built a commander camera.
2. RTS deprecated → **Dwarf Fortress–style game**: huge world generated up front, players play
   individual regions from the commander camera, later traverse regions (adventure/fortress
   mode); **MVP = fortress mode only**. Research DONE (commit 8b6efc2):
   docs/superpowers/specs/2026-10-20-dwarf-fortress-research/ (00-synthesis.md: DF mechanics,
   MVP tiers, engine-mapping recommendations — lockstep, tile map truth, z-cutaway clip+cap).
   The phased transition plan next to it was **REJECTED** by Dawid: features are implemented
   **piece by piece, roughly modelling DF** — no phase sequence. Don't propose a big phased
   roadmap again; scope one feature at a time, using the research as reference only.
3. Dawid wanted the multi-region code back for region loading/traversal, so **develop was
   rebuilt**: develop = ds/oldMMOCode (28b9bf8) + cherry-picked toolchain pin + bbmodel
   (loader, animation, asset tool) + the commander camera ported to multi-region
   (commit 0d910c1: focus clamped to the HOME region, sparse height grid, minimap of the home
   region, GroundPick = (RegionId, local point)). The old develop (RTS Phase 1 + the
   single-map commander WIP + runtime MapConfig/assets/map.ron) is preserved on branch
   **ds/rtsPivot** (1fc2444). develop is not on origin.

**Why:** the multi-region/cell/LOD/handoff memories are CURRENT code again on develop (not
history); RTS-only memories/plans are history on ds/rtsPivot.
**How to apply:** build on the multi-region engine; for DF work start from the research handoff;
ask before porting anything from ds/rtsPivot (e.g. MapConfig). Commander is the default view;
VOXEL_CAMERA=first-person (auto for RENDER_BENCH / VOXEL_AUTOPILOT) restores the body camera.
