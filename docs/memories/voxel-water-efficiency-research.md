---
name: voxel-water-efficiency-research
description: 2026-09-22 research on efficient voxel water (beat Minecraft) — bodies + frontier CA, server-solved lead-stamped WaterStep events; synthesis in specs
metadata:
  type: project
---

RESEARCH DONE 2026-09-22. Folder: docs/superpowers/specs/2026-09-22-voxel-water-efficiency/ (start at 00-synthesis.md; 01 games, 02 algorithms+benchmarks, 03 netcode/render).

Recommendation, which amends docs/2026-07-15-water-implementation-plan.md:
- water BODIES (level + volume + per-layer area + column-span membership; sea and rivers are infinite bodies) with a per-cell CA only at the frontier, in dense 8³ bricks;
- the solver runs SERVER-ONLY and emits absolute-value `WaterStep` events stamped to apply 6 ticks ahead (sent early, matched like GhostUpdate), so water state stays in rollback/hash but clients never re-solve;
- water is kept out of the terrain LOD fingerprint;
- a separate blended water render layer.

Measured: BTreeMap 8-cell chunks ≈ 50× slower than a dense u16 array (layout, not algorithm).

Research reports disagreed on predicted vs server-solved (02 said predicted); the synthesis resolves it in favour of server-solved.

USER DECIDED 2026-09-22: NOT server-solved. Water flow must be deterministic in the rollback sim on every client (bit-exact integer solver; bodies keep re-simulation cheap; cross-region flow uses handoff-style outflow/arrival events). Water is FINITE everywhere: the sea is a finite world body with a volume ledger; forged rivers stay asleep until disturbed; a thin-film minimum depth. Never re-propose server-solved water.

**Why:** the user asked for efficiency "better than Minecraft". **How to apply:** don't re-research. Still open: wild-water persistence. PROTOTYPE PLANNED (not built) 2026-09-22: docs/superpowers/plans/2026-09-22-water-prototype.md — standalone zero-dep crate prototypes/water/, bodies vs cell-only, 6 scenarios, undo/mass/wasm-golden gates; user said plan only, awaiting go + execution method (recommended: offline 64³ harness, bodies vs brick CA). Nothing implemented.

Related: [[rapier-water-investigation]]; superseded in part by [[water-revisit-2026-11]] (2026-11-06).
