---
name: terrain-slopes-landed
description: "Worldgen half blocks replaced by Minecraft-stair \"slopes\" (2026-11-06, develop 77c8112..aae92b5); jump-up-1m kept; live look pending"
metadata:
  node_type: memory
  type: project
  originSessionId: 2d741c6b-5699-4f9f-b77b-8a70aaeb6ac5
  modified: 2026-11-06T14:08:21.663Z
---

Built 2026-11-06 on develop (77c8112..aae92b5): worldgen rounds to whole blocks and carves a stair-shaped slope (ChiselData, block's own material, `game::slope`) into the HIGHER column's top on every exact 1-block rise; pathfinding/walker walk up slopes from their low side only; client TileMirror + height grid know slopes.

**Why:** Dawid wanted half blocks gone and walkable 1 m rises; stairs needed for construction later. He explicitly said: do NOT remove jumping up 1 m blocks; fortress-mode stair BUILDING is TBD (building mechanics will be reworked).

**How to apply:** don't re-plan; built stairs should reuse `slope_chisel`/`classify`. Pending: Dawid's live look (`just run --reset`), feel of GROUND_SNAP 0.6 + AUTOSTEP_MIN_WIDTH_M 0.3, client unit tests (type-checked only), CLAUDE.md slope edits left uncommitted (file had foreign edits). REGION_FILE_VERSION bumped to 7 (old saves → .bak). Follow-ups in TODO.md. Related: [[block-half-block-worldgen]] (superseded), [[creative-movement-landed]].
