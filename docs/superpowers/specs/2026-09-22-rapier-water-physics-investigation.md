# Rapier water physics: investigation

**Date:** 2026-09-22 · **Status:** research only, nothing implemented  
Line numbers are as of the uncommitted 2026-09-22 tree; prefer the named functions.  
**Question:** how would Rapier-based water physics simulation fit this voxel engine?  
**Short answer:** it doesn't, as simulation. Rapier has no fluids. The Dimforge fluid crates
(salva SPH, sparkl/wgsparkl/slosh MPM) can't live in a bit-exact rollback sim. Rapier's
real part in water is small:

1. Stop giving water a collider.
2. Later, apply buoyancy forces to dynamic bodies, once any exist.

The authoritative fluid should be integer cellular water, as already designed in
`docs/2026-07-15-water-implementation-plan.md`. That plan still matches the code and
remains the one to execute, with the amendments in §4.

## 1. What the Rapier ecosystem offers

| Option | What it is | Fit for our sim (rollback, bit-exact native + wasm32) |
|---|---|---|
| **rapier3d itself** | Our fork is 0.31. It has no fluid, no buoyancy and no example of either. The only mention is the `linear_damping` doc ("1.0+ = thick fluid"). The idiomatic approach is do-it-yourself: each tick, estimate the submerged volume, then call `add_force_at_point(ρ·g·V_sub)` and add drag or damping. | **Fits, for dynamic bodies only.** User forces live in `RigidBody.forces`. Writing them through `mutate_body_safe` (whole-body before-image) and letting the step's whole-body save in `StepJournal` capture the rest means no new rollback machinery. **Kinematic bodies ignore forces.** Every body today (players, slimes) is `kinematic_position_based` with gravity handled in `movement::plan`, so forces do nothing for them. |
| **salva3d** (SPH: DFSPH/IISPH/PBF, viscosity, surface tension; two-way rapier coupling) | 0.10.0 was released 2026-08-08. It depends on rapier ^0.35 / parry ^0.30 / nalgebra ^0.35 (we are on 0.31 / 0.25 / 0.34), plus `kiss3d`. It has run in wasm demos since 2019. | **Unfit.** Every particle changes every tick, so undo means snapshotting all fluid state every tick, which defeats the journal/LIFO-inverse design. The coupling keeps colliders in a `HashMap` and uses optional rayon, both of which break bit-exact ordering. It is also 4 rapier minors behind our fork, and particle state can't be streamed per region. At most it could be a client-only effect. |
| **sparkl / wgsparkl / slosh** (MPM: sand, snow, fluids) | sparkl needs CUDA. wgsparkl (WGSL/WebGPU) is small and experimental. slosh is its Slang port, "not quite there yet". wgrapier is a GPU subset of rapier. Dimforge's 2026 goals are robotics and GPU rigid bodies, not fluids. | **Unfit.** Its particle-to-grid scatter uses float atomics, whose order depends on scheduling, so results vary across GPUs and drivers. A headless server may also have no GPU. Visual effects only (splashes, spray). |
| **Rapier sensors** on water volumes | `intersection_pairs_with` / `intersect_shape` | **Pointless here.** The voxel grid already answers "is this point wet?" for free. A sensor collider per water chunk would put the sea back into the broad phase that `seed_terrain_genesis` works hard to keep small. |

No published game runs SPH or MPM fluids authoritatively under lockstep or rollback. The
games that ship multiplayer water all use grid water:

- **Minecraft:** levels 0–7, flow every 5 ticks, flow vectors push entities.
- **Vintage Story:** the same model, with salt and fresh water.
- **Dwarf Fortress:** 7 depth levels plus a pressure hop.
- **Veloren:** static liquid blocks plus density-based buoyancy and `0.5·ρ·v²·Cd` drag in
  its own physics, not rapier.
- **Teardown:** a flat plane plus buoyancy; it has no fluid sim at all.

Sources:
- rapier and salva: https://crates.io/crates/salva3d, https://salva.rs/,
  https://github.com/dimforge/salva/blob/master/src/integrations/rapier/fluids_pipeline.rs
- sparkl family: https://github.com/dimforge/wgsparkl, https://github.com/dimforge/slosh
- Dimforge 2025 review: https://dimforge.com/blog/2026/01/09/the-year-2025-in-dimforge/
- Grid water in games: https://minecraft.wiki/w/Water, https://wiki.vintagestory.at/Water,
  https://veloren.net/devblog-119/, https://dwarffortresswiki.org/index.php/DF2014:Pressure,
  https://80.lv/articles/teardown-developer-breaks-down-multiplayer-and-voxel-destruction-tech

## 2. Where water stands in the engine today

Water is `BlockId(5)`, an ordinary block. Every system that asks "is this solid?" asks
`id != AIR`, so water is an **opaque, fully collidable solid**:

- **Colliders.** `collider_cells` (`block.rs`) gives water block-scale collider cells.
  An all-water ocean chunk collapses to one 32³ cuboid in `seed_terrain_genesis`
  (`state.rs:590-601`). Players and slimes walk on the sea.
- **Spawn and editing.** `surface_voxel` (`state.rs`) counts water as ground. The
  edit raycast hits water (`edit.rs:79`), so you can break water, and placing into it is
  refused.
- **Rendering.** Water is meshed by the opaque binary mesher and drawn only in
  `Opaque3d`. There is no transparent pass or water shader. It closes the cave-culling
  flood (`visibility.rs:79,89`).
- **LOD.** Water is any-solid volume in `merge_group` (`lod.rs:53-80`), so the sea draws
  as a flat opaque surface.
- **Behaviour.** No flow, swimming, buoyancy, drag or scheduled block updates exist.
  Breaking a block next to water leaves a vertical water wall. Rivers and lakes are static
  block fills, including the forged world's rivers that sit above sea level.
- **Physics bodies.** There are **no dynamic rigid bodies in production**, only kinematic
  capsules. The rapier gravity vector goes unused.

Scale reference: 1 voxel = 1/16 m, block = 16³ voxels = 1 m, chunk = 2×2×2 blocks (32³),
tick = 50 ms, region = 64 m at the default size.

## 3. How water should fit, and what Rapier does in each layer

| Layer | Mechanism | Rapier's part |
|---|---|---|
| **Liquid state** | A separate rollback-hashed `WaterChunk` layer: `u16` amount per 1 m cell, active masks, sleep. It lives in `GameData` in an ordered map (plan §"Authoritative representation"). | None. |
| **Flow** | Integer ordered-edge-sweep CA every 2 ticks. Mass-conserving, canonical order with a `tick % 4` axis permutation, and settled water visits zero cells. Undo stores one before-image per touched chunk per water step. | None. |
| **Solidity** | A registry `kind: Air \| Solid \| Liquid`, replacing ~18 `id != AIR` sites (list in §5). Water leaves `collider_cells`. | **Remove water from the collider path.** This is the only Rapier change water needs for v1. |
| **Swimming (players, slimes)** | Sample water at capsule bottom, centre and head in `movement::plan_with`. Scale gravity toward neutral, add drag, let Space and Shift swim, cap vertical speed. Adds wet/swimming/head-under state on `Motion`. | None, because the bodies are kinematic and rapier forces don't apply. The KCC sweep is unchanged, since water is no longer a collider. |
| **Currents** (later) | A per-cell flow vector from recent signed edge flux, added as bounded advection in `plan`. | None. |
| **Dynamic bodies** (boats, floating items, whenever they exist) | Sample submerged volume at fixed body-local points against the water grid, compute `ρ·g·V_sub` at the centroid plus quadratic drag, and write it with `add_force_at_point` via `mutate_body_safe` before `step_journaled`. | **Real, but deferred.** It needs the first dynamic body, not water. Rollback is already covered by the journal. |
| **Visual extras** | Shader waves, foam, underwater fog; maybe wgsparkl or salva splashes client-side. | Optional, cosmetic, never hashed. |

## 4. Amendments to the July water plan

The plan predates several later changes. These are the deltas:

1. **Forged rivers and lakes above sea level** (`forged.rs`). The plan's flow model would
   drain a sloped river the moment it wakes, because nothing supplies it. Rivers need
   **source cells at their upstream reach heads and sinks at mouths**, or the whole river
   channel must stay a permanently settled "static liquid" until a terrain edit wakes it
   locally. Recommended: generated water starts asleep; `RiverReach` heads become SOURCE
   flags; a river mouth at sea level is a sink. Lakes at their spill level are already in
   equilibrium.
2. **Half blocks** now exist as one-cuboid `ChiselData` (whole + half-block worldgen).
   The plan treats "chiseled = solid, zero liquid capacity" in v1. That is still right
   underwater, because worldgen keeps submerged surface blocks whole. But a shoreline
   half block with air above it can't hold water, and a player chiselling or placing a
   slab at the waterline will make water pour around it. Decide early whether a
   half-block cell gets half capacity; this is the Vintage Story "waterlogged slab"
   question in `2026-09-22-vintage-story-slabs-and-chiseling.md`.
3. **Full-chunk cuboid collapse.** Once water is out of `collider_cells`, all-water chunks
   stop collapsing to a cuboid and produce no collider at all. That is good: it removes
   the largest collider population over the ocean. Re-check the "chunk entity exists iff
   collider exists" assumptions in `region.rs:628-639` and `edit.rs` (`create_mesh`).
4. **The near window is 5×5 now.** Cross-region flux (plan Phase 3) runs across more live
   seams. Keep the plan's "closed boundary plus counter" v1 rule, and expect seam walls
   when a player drains a lake across a region edge until Phase 3.
5. **LOD cache and snapshot.** Water leaving `Chunk` changes the `TerrainSnapshot`
   fingerprint and the snapshot size: all-water chunks become empty, which should shrink
   the 3 MiB snapshot further. `merge_group` needs a separate topmost-water pass for the
   flat sea surface. The sea already sits on every LOD lattice (`SEA_LEVEL` = 1024 units).
6. **Modding direction.** Water rules are exactly the kind of block behaviour that wasm
   mods would want to extend (other liquids, lava). Keep the solver's per-liquid
   constants in a table keyed by `BlockId`, not hard-coded to id 5.

## 5. Sites that treat "not air" as solid (Phase 0 checklist)

- `voxel.rs:147` (visibility)
- `block.rs` `collider_cells`, `derive_voxels`
- `state.rs` `surface_voxel`
- `edit.rs:79` (`cell_solid`)
- `lod.rs:53-80,108,268`
- `client/…/meshing/binary.rs:29,261`, `meshing/visibility.rs:79,89`
- `far_voxel/dda.rs`, `far_voxel/brick.rs`
- `worldgen/src/lib.rs:191-193,268-290` (water fill)
- `slime.rs:245` (no-spawn-on-water)

## 6. Recommendation

Don't add salva or any MPM crate to the sim. Execute the July plan's Phase 0 and Phase 1
(material classes, then static water with no collider, a transparent surface and
controller swimming), and treat Rapier as touched only by "water has no collider".

The first valuable step is Phase 0 on its own. Adding the registry `kind` and making
water non-solid immediately gives:
- walk-into-the-sea;
- far fewer ocean colliders;
- a working cave-culling flood.

It needs only the water rendering to stay opaque for the moment.

Open decisions for the user:
- (a) Is swimming worth doing before flow? Recommended yes, since it is Phase 1.
- (b) River sources vs permanently static rivers (§4.1).
- (c) Half-block water capacity (§4.2).
- (d) Is buoyant dynamic-body gameplay (boats, floating drops) on the roadmap at all?
  That is the only thing that would ever bring Rapier forces into water.
