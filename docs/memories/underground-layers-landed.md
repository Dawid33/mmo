---
name: underground-layers-landed
description: Lazy underground IMPLEMENTED 2026-10-20 (develop a17378a..c8c5651) — ColumnGround recipes, implied ground, bedrock 60 down, forged default world, 256 m play regions; live check pending
metadata:
  type: project
---

Underground layers plan (docs/superpowers/plans/2026-10-20-underground-layers.md, spec …specs/2026-10-20-underground-layers-design.md) fully implemented via subagent-driven development on develop, a17378a..c8c5651 (20 commits).
- Each worldgen Column carries a `game::ColumnGround` (surface, geology offset, strata, bedrock, band_base_sub_y); regions keep them in `GameData.ground`; a missing chunk below the band is implied ground; a break materialises implied face neighbours in the same undoable edit; bedrock takes no write; outer edge ring of a region can't be dug below the surface (known limit).
- Forge pipeline is now `worldgen::forge`; server forges `worlds/default.world` on first start (atomic save), `world_file=procedural` for noise; tests/wasm stay procedural.
- Play recipes build REGION_CHUNKS=128 with near radius 0; tests stay 32; render-bench stays 32.
- `just cross-section` shows the slice; default world showed grass/dirt → limestone → shale → bedrock at surface−60.

**Why:** first DF-direction piece (ground to dig fortresses into).
**How to apply:** don't re-plan; pending = Dawid's live first-person dig on the forged world, first-start forge + reload, and (ask first) region start/RSS/snapshot measurement at 256 m. Next piece discussed: world map + embark + save/load (Slint UI, see [[ui-slint-for-player-ui]]).
