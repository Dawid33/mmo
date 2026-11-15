---
name: df-scale-world-and-civs-handoff
description: "2026-11-01 DF-scale terrain design (draft, awaiting approval) + civs/territories/religions handoff; user decisions on scale, height, biomes in play"
metadata:
  node_type: memory
  type: project
  originSessionId: 5aa647aa-2c14-4529-9130-8ca3b67ca9e8
  modified: 2026-11-01T12:41:59.136Z
---

2026-11-01 session: researched Azgaar FMG + DF history/civs, then wrote the DF-scale terrain design. User decisions:
- DF Medium scale (~100 km); "forging time doesn't matter".
- Real kilometre-plus ranges.
- Biomes visible in play.
- View distance later (separate rendering project).
- zstd OK for the compact world file.

Design: docs/superpowers/specs/2026-11-01-df-scale-world-terrain-design.md (APPROVED 2026-11-01). Plan: docs/superpowers/plans/2026-11-01-df-scale-world-terrain.md (21 tasks); implementer handoff: …-df-scale-world-terrain-handoff.md. Not implemented yet.
Society layer handoff: docs/superpowers/plans/2026-11-01-civilizations-territories-handoff.md. Settlements are generated on region load and are out of scope.

**Why:** the forge was island-MMO era (16 km island, 236 m peaks, no temperature or biomes); the DF direction needs a continent with climate bands and biomes before civs can be placed.

**How to apply:** don't re-research FMG/DF. The next step is brainstorming the society layer from the handoff (open questions listed there), or the plan for the terrain design once the user approves it. Related: [[game-direction-pivot]], [[world-forge-prototype]].
