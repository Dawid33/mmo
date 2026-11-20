---
name: trees-plant-life-investigation
description: Trees implemented as terrain blocks with falling Chop bodies; buried-root deterministic regrowth implemented 2026-11-20; old saved regions stay rootless unless newly generated
metadata:
  type: project
---
IMPLEMENTED 2026-10-30 via subagent-driven plan docs/superpowers/plans/2026-10-30-trees.md (spec specs/2026-10-29-trees-investigation.md). TREES ONLY (Dawid: no shrubs/grass).
Dawid's decisions: trees are terrain blocks (Block.data = 18-bit tree id + 6-bit limb index); chopping detaches the part beyond the cut into a free dynamic body (generic `game::voxel_body`, future: stalagmites / structural collapse / re-attach) that DESPAWNS at rest (items = separate feature); leaves no collider, logs+branches collide (on the half-block grid, `collider: Half` block prop); see-through cutout leaves; seam crossing of bodies = separate handoff (interim: tip inward from edges).
Chop rule that works on generated trees: limb components from the recorded limb index + post-cut support pass (geometry/leaf connectivity failed: twigs touch).
**2026-11-20:** the pure shape table/skeleton/rasteriser now lives in `game::tree_shape`.
New regions stamp 2–6 buried roots and persist `RegionTrees` in the region cut; a surviving
root drives bounded six-stage regrowth through ordinary rollbackable terrain writes. Legacy
regions with NULL `tree_state` retain their existing standing/cut behavior without inferred
roots. **Open:** Dawid to measure 512 m region cost; WebGL2 draws no voxel bodies and leaves
cast solid shadows there; twin hand checks remain.
**How to apply:** don't re-plan; follow-ups only. Related: [[chunk-size-two-meters]], [[entity-seam-crossing-handoff]].
