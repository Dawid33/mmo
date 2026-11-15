---
name: water-revisit-2026-11
description: 2026-11-06 water re-research for today's engine (256 m regions, 16³ chunks, region threads) — water out of chunk data, recipe water, brick journal, seam flux rules
metadata:
  type: project
---

RESEARCH DONE 2026-11-06: docs/superpowers/specs/2026-11-06-water-revisit/ (00-synthesis, 01 new prior art, 02 engine audit). Nothing built.

Key conclusions:
- Water must leave chunk block data. Today it is solid block 5, and one change at 16³ chunks means a 4096-key collider clone, a whole-chunk remesh and an LOD fingerprint bump.
- Sea, lakes and rivers become implied "recipe water". Bodies are `{level, volume}`.
- The frontier sits in dense 8³ bricks with a cell budget. Undo is a first-touch brick journal per step.
- Seam flux: one owner per face, rate-capped. The client never relays predicted arrivals (orphan risk) and waits for the server's arrival event. Parked destinations use a per-region pending-volume ledger.
- Proposed: the world ledger acts as the water cycle (edge rivers, evaporation, springs).
- First step: measure mispredicts per second and log length per region thread at 256 m.
- Flow models (user agreed 2026-11-06): bodies for standing water; a frontier CA in checkerboard order; rivers as chains of reach bodies; one start-of-step, rate-capped link rule for gates, pumps and seams. Player-made water (pour, breach, gate, pump, scoop, moat) reduces to add/remove volume, open/close a cell, or a link.
- The prototype plan docs/superpowers/plans/2026-09-22-water-prototype.md was AMENDED with Tasks 6–11 (checkerboard, links, brick journal, evaporation off by default, lagged seam, player scenarios) and Task 12 (measure). Not executed.

**Why:** the user asked for water with the rollback and cross-region costs considered. The September decisions (in-sim deterministic, finite) still stand.
**How to apply:** don't re-research. The open decisions are listed at the end of the synthesis. Related: [[voxel-water-efficiency-research]], [[rapier-water-investigation]].

2026-11-06 PROTOTYPE BUILT, Tasks 1–11 (develop 63c564e..ea98f71, prototypes/water/, 62 tests, native == wasm32). Solver rules added beyond the plan:
- relevel when a member is absorbed below the top;
- mid-air donors only fall;
- an order-free per-colour CA with a receiver midpoint cap;
- links cap by room before taking, and take from the planned source;
- the seam sender subtracts its own in-flight sends.

Task 12 DONE (be30900): docs/superpowers/specs/2026-11-06-water-revisit/03-prototype-results.md. The gate passed, except one criterion that was borderline (20.2 vs 20 µs). Bodies are 30–80× cheaper per step than the per-cell CA. The brick-journal undo lost to per-cell entries. Steps that restructure a body cause spikes, so the engine needs a per-step budget for them and an undo that records span changes. Next: an engine plan (Phase 0 liquid kind, static water layer) or the follow-up experiments. The ledger and rulings are in .superpowers/sdd/2026-09-22-water-prototype/progress.md.
Handoff for the engine plan: docs/superpowers/plans/2026-11-06-water-engine-plan-handoff.md (scope: liquid kind, static layer, swimming, rendering; flow plans come later).
