---
name: column-optimization-landed
description: "TerrainPipeline::column optimization IMPLEMENTED (develop 8207c21..76dc4af, 2026-07-10) — 13.6× generate_region; don't re-plan; follow-ups + TICK_RATE test breakage noted"
metadata: 
  node_type: memory
  type: project
  originSessionId: 18cd1174-a5b7-4116-82c5-f79304cf5f04
---

The column-profile optimization plan (docs/superpowers/plans/2026-07-10-column-profile-optimization.md) is FULLY IMPLEMENTED on develop (8207c21..76dc4af, 2026-07-10, final review: ready to merge). column() 818→199ns, generate_region 894→65.5ms. Tasks 2–4 bit-exact (oracle tests pin fused path against retained `DensityFn` graphs — the graphs are the test-only spec now); Task 5 (ProfileGrid, 17×17 4m lattice + bilinear) CHANGED WORLD OUTPUT deterministically; spawn re-verified y=70, WORLD_SEED unchanged. Don't re-plan; the prior-art appendix (Minecraft cells / fidget interval pruning / f64x2 determinism) is in the plan doc.

Deferred follow-ups (fold into whichever plan touches these files next):
- Block/chisel fill is the new generate_region hotspot (~65ms, column() is 0.1%).
- seed_scan.rs measures the exact field, not the shipped lattice field (≤1 block off) — route through ProfileGrid.
- ProfileGrid: add degenerate-span debug_asserts; `impl DensityFn` block-allow → cfg(test).
- Tape/batch/interval engine deferred until caves add Min/RangeChoice; LOD-direct generation for worldgen_rings may relax determinism (user: only worldgen must be deterministic, rendering need not).

Known live breakage NOT from this plan: [[tick-rate-500-broke-threaded-world-test]] — the user's TICK_RATE 200→500ms (a853794) makes server threaded_world::roaming_client_cycles_regions_across_threads fail (600ms window expects ≥9 tick events from 9 regions; sees ~5). Verified: passes on same HEAD with TICK_RATE=200.
