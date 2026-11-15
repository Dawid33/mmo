---
name: block-half-block-worldgen
description: Investigation of worldgen emitting only whole/half blocks (no voxel chisel) — measured costs, options, hazards
metadata:
  type: project
---
REVISED 2026-09-22 (uncommitted): BlockShape REMOVED at user request (no special-case slabs) — half block = ordinary one-cuboid ChiselData; ChiselData is now a canonical VS-style u32 cuboid list (always re-merged from a dense grid); collider_cells takes coarsest solid cell (16/8/1) of any chisel. Earlier same day: worldgen rounds surface to nearest half block, submerged surface always whole, place_straddle deleted. Spawn snapshot 36.5→3.1 MiB, generate_region ~75→2–6 ms. Live look not done. Original investigation: findings in docs/superpowers/specs/2026-09-22-block-and-half-block-worldgen.md, probe = crates/worldgen/examples/quantize_probe.rs.
Only source of sub-block voxels is `place_straddle` (every surface column = one dense 16³ chisel). Per region: voxel today 36–41 MiB snapshot / 9M voxel collider cells / ~185 ms Region build; half-block-as-ChiselData halves that; whole-block 3–7 MiB / 0 voxel cells / 4–13 ms. Whole blocks make 4–27% of neighbour steps need a jump (STEP_HEIGHT 0.6); half blocks keep all walkable.
Recommendation: native slab shape in reserved `Block.data` + a half-scale (8-voxel) collider grid. Hazards: submerged straddles are air pockets today; forged levees/berms may round down and leak.
**Why:** user chose the native-slab option.
**How to apply:** don't re-plan; worldgen emits only whole blocks + ChiselData::cuboid half blocks; never reintroduce block-shape special cases (user wants everything voxel-editable). Related: [[terraced-ground-fix]], [[multi-scale-colliders-landed]], [[world-forge-prototype]].
