---
name: trees-plant-life-investigation
description: Trees IMPLEMENTED 2026-10-30 (develop 0171ad4..047be7e, plan plans/2026-10-30-trees.md): worldgen trees as terrain blocks, cutout leaves, generic voxel_body falling bodies, Chop (T); growth deferred; 512 m perf unmeasured
metadata:
  type: project
---
IMPLEMENTED 2026-10-30 via subagent-driven plan docs/superpowers/plans/2026-10-30-trees.md (spec specs/2026-10-29-trees-investigation.md). TREES ONLY (Dawid: no shrubs/grass).
Dawid's decisions: trees are terrain blocks (Block.data = 18-bit tree id + 6-bit limb index); chopping detaches the part beyond the cut into a free dynamic body (generic `game::voxel_body`, future: stalagmites / structural collapse / re-attach) that DESPAWNS at rest (items = separate feature); leaves no collider, logs+branches collide (on the half-block grid, `collider: Half` block prop); see-through cutout leaves; seam crossing of bodies = separate handoff (interim: tip inward from edges).
Chop rule that works on generated trees: limb components from the recorded limb index + post-cut support pass (geometry/leaf connectivity failed: twigs touch).
**Open:** Dawid to measure 512 m region cost (~60k chiseled wood blocks/region, near-quad count, gen time); growth/regrowth deferred (sim can't call worldgen skeleton; no wake queue); WebGL2 draws no voxel bodies and leaves cast solid shadows there; live game run not done.
**How to apply:** don't re-plan; follow-ups only. Related: [[chunk-size-two-meters]], [[entity-seam-crossing-handoff]].
