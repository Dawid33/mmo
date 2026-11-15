---
name: rapier-water-investigation
description: 2026-09-22 research on Rapier water physics — salva/MPM unfit for rollback; use the existing July cellular water plan; findings doc in specs
metadata:
  type: project
---

INVESTIGATED 2026-09-22 (docs/superpowers/specs/2026-09-22-rapier-water-physics-investigation.md). Rapier has no fluids. salva3d (0.10, rapier ^0.35, per-tick whole-state undo, HashMap/rayon order) and sparkl/wgsparkl/slosh (GPU float atomics) are UNFIT for the bit-exact rollback sim; cosmetic client-only at most.

The authoritative design already exists: docs/2026-07-15-water-implementation-plan.md (integer cellular WaterChunk layer, swim in movement::plan, since all bodies are kinematic → rapier forces do nothing). Rapier's only v1 role is "water has no collider"; buoyancy forces wait for the first dynamic body.

Amendments to that plan: forged rivers need sources/sinks or stay asleep; half-block water capacity undecided; 5×5 near window means more seams for Phase 3.

Today water = BlockId(5), opaque and solid everywhere (id != AIR). Nothing built.

**Why:** the user asked "how would rapier water fit". **How to apply:** don't re-research; the next step is plan Phase 0 (registry Air/Solid/Liquid kind), pending the open decisions in §6 of the findings doc.
