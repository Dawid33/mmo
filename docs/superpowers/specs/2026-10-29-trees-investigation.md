# Trees — investigation

**Date:** 2026-10-29 · **Status:** investigation, nothing built · **Ask:** trees that grow only
where they could survive, deterministic and realistic-looking generation, and chopping that
makes the part of the tree above the cut fall, like Dwarf Fortress. **Trees only**: shrubs,
grass and other plants are out of scope.

**Decided by Dawid (2026-10-29):**
1. Standing trees are **terrain blocks** (log / branch / leaf blocks written by worldgen), until
   the trunk or a branch is chopped.
2. Chopping turns **that part of the tree into its own entity**, which falls **under physics**
   (rapier) and **disappears once at rest**. No items: item handling is a separate feature,
   and the settle step is where it will plug in (§5.4).
3. **That terrain → entity step is general engine work, not a tree special case.** Future
   uses include falling stalagmites and mass / weight handling of structures: something
   detaches into a physics entity, falls, breaks apart, and is **re-added to the terrain once
   the physics settles**. Those are future work, but the mechanism is designed for them now
   (§5).
4. Leaves are **see-through** (alpha-cutout textures), which needs a renderer cutout path.
5. The detached part is a **free dynamic body from the first tick** (no hinge joint).
6. **Colliders**: leaves have **no collider**; logs **and branches** do.
7. **Seams**: a falling body crosses region seams like any entity. That is generic work in
   `docs/superpowers/plans/2026-10-29-entity-seam-crossing-handoff.md`, **not part of this
   work**.

**Assumes** the 16×16×16-block sim chunk and its block-delta edit path (`SetBlocks` update,
incremental colliders, per-block undo) from
`docs/superpowers/plans/2026-10-29-chunk-size-increase-handoff.md`. Growth, detach and re-attach
are large block edits and depend on it.

DF facts are from memory (the DF wiki returned HTTP 429 during this session) — verify before
quoting them.

---

## 1. What DF does, and where we differ

- **Trees are multi-tile objects** (since 0.40): trunk (thick trunks span 2×2 or 3×3 tiles),
  branches, twigs (the walkable canopy), roots. Shape comes from species raws:
  `MAX_TRUNK_HEIGHT`, `MAX_TRUNK_DIAMETER`, `TRUNK_PERIOD`, `BRANCH_DENSITY`, `BRANCH_RADIUS`,
  `HEAVY_BRANCHES`, `ROOT_DENSITY`, `TRUNK_BRANCHING`, …
- **Where trees grow**: biome tokens on the raw (`BIOME:…`, `WET`/`DRY`, `GOOD`/`EVIL`).
  Biomes derive from rainfall, temperature, drainage, elevation, salinity and savagery.
- **Growth**: saplings grow over years; trees spread onto suitable soil.
- **Felling**: the whole tree vanishes and logs appear where it stood. **We differ**: the part
  above the cut detaches, topples under physics, hits ground, trees and units, and becomes
  logs at rest.

## 2. Engine facts this design rests on

| Fact | Where | Consequence |
|---|---|---|
| 1 block = 1 m = 16³ voxels; chunks become 16³ blocks (handoff); each chunk is an entity with a fixed body + collider | `block.rs:15-16` | A tree spans 1–4 chunks; detach is a multi-chunk block-delta edit |
| `Block { id, data: [u8; 3] }`, `data` reserved / zeroed, serialized and hashed | `block.rs:54` | 24 bits for a **tree id** on every tree block (§4.2) |
| Sub-block detail = canonical cuboid lists (`ChiselData`); a cuboid packs min / max on the 16³ grid | `block.rs:81-110` | Thin branches voxel-exact; **90° rotations of a chiseled block are exact** (cuboids map to cuboids), which matters for re-attach (§5.4) |
| Entities carry a full `Pose`; the pulled-quad shader reads a per-chunk `world_from_local` | `state.rs:51`, `voxel_pull.wgsl:246` | A detached voxel body draws through the same path with its own pose (confirm for non-terrain voxel entities) |
| Stateless hash RNG `roll(tick, subject, salt)` | `slime.rs:121` | Placement, shape and tipping RNG with no undo state |
| Forge computes rainfall (`forge/climate.rs:41`), world file stores no climate; no biomes | `forged.rs:103` | Habitat rasters are new forge output + world-file version bump |
| Dynamic rapier bodies + `StepJournal` + `enhanced-determinism` work; **fixed bodies skip mass props because voxel shapes hit std `acos`/`cos` in glamx `SymmetricEigen3`** | CLAUDE.md "Mass properties" | Detached bodies get **explicit** mass properties (§5.3) |
| Orders are sim state (`DesignationKind {Dig, Deconstruct, Build}`) | plan `2026-10-29-fortress-selection-designation-ui.md` | `Chop` is one more kind |
| No alpha-cutout path in the block shader | `voxel_pull.wgsl` | See-through leaves are renderer work (§4.6) |

## 3. Habitat: where each species can survive

### 3.1 Climate fields (forge output, stored in the world file)

Computed offline at `cell_m`, stored as rasters, sampled bilinearly in `ForgedPatch`.

| Field | Source |
|---|---|
| `rain` | already computed (`climate::rainfall`); store it |
| `temperature` | `base_temp_knob − 6.5 °C/km × elevation`; optional N–S gradient knob |
| `moisture` | rain × drainage (discharge, distance to river / lake, water table) |
| `slope` | height raster |
| `soil_depth` | `MAX_SOIL_DEPTH`, geology, slope (bare rock on scree / peaks) |
| `salinity` / exposure | distance to sea, windward side |

The procedural world gets noise stand-ins (tests and tools stay procedural).

### 3.2 Species niches

`trees.ron` per species: trapezoid tolerances (`temperature: (min, opt_lo, opt_hi, max)`,
moisture, slope, soil depth, salinity), shape parameters (§4.4), wood / leaf density, log
yield. **Suitability** is the product of the responses; **density** is
`suitability × max_density`. Species compete in placement (§4.3). Biome names are display
labels only. `just world-map --layer rain|temperature|moisture|trees` is the tuning loop.

## 4. Standing trees (terrain blocks)

### 4.1 Blocks

Per species in the registry: `<sp>_log` (bark sides, ring ends), `<sp>_branch` (thin wood,
usually chiseled), `<sp>_leaves` (cutout). Plus a registry flag **`solid`** (decided): **leaves non-solid** (no collider; the edit ray
still hits them, units walk through crowns), **logs and branches solid**. Chiseled branch
blocks get voxel-scale colliders (multi-scale colliders). Measure the collider count and
physics cost of a forested region, and if it bites, collapse each thin chiseled branch
block to its bounding cuboid in the collider only.
Consequences:
- **LOD comes for free**: far rings downsample tree blocks, the visible-from-above material
  shows canopy, and forests read to 1 km.
- **Snapshots pay for every pristine tree's blocks** (forest chunks are not uniform). Measure.
- **The surface band grows**: worldgen materialises chunks up to crown tops (usually one extra
  16 m layer in forest).
- **Every terrain system sees trees for free**: edit ray, chisel, cutaway, TileMirror, later
  pathing.

### 4.2 Tree identity in `Block.data`

Every block a tree writes carries a **locally unique tree id** (24 bits; 0 = not a tree, so
player-placed logs are 0): `id = (lattice_x mod 4096) | (lattice_z mod 4096) << 12`, the
tree's base cell on the canopy lattice. Touching trees are a few cells apart, so ids never
collide locally. The base position, and from it `(species, seed, age)`, is recoverable from
the id plus any of its blocks. Ids are part of the block value, so they are serialized, hashed
and undone with it. Deltas placement can't recompute (felled / stump, grown past pristine age,
planted) live in `#[undo(map)] trees: BTreeMap<TreeKey, TreeState>`.

### 4.3 Deterministic, order-independent placement

1. **World-aligned jittered lattice** (~8 m, a power-of-two block count); one candidate per
   cell from `roll(seed, cell, salt)`: jittered base and uniform `u`.
2. **Species pick**: cumulative `density × cell_area` in fixed species order against `u`.
3. **Competition** (pure): reject a candidate if a neighbour candidate in its 3×3 cells with
   a higher priority hash lies closer than the larger crown radius.
4. **Reject** water, too-steep slope, and **crowns crossing a region seam** (a standing tree's
   blocks stay in one region, so a cut is one region's edit; unrelated to bodies crossing
   seams).
5. **Rasterise with a margin**: a chunk or cell evaluates candidates within one maximum crown
   radius and writes only its own blocks, so it is bit-identical to the whole-region build.

`age` comes from the hash too, giving a mix of saplings, mature trees and old giants.

### 4.4 Shape: parametric skeleton

| Approach | Look | Fit |
|---|---|---|
| L-systems | stylised, hard to tune | poor |
| **Parametric recursive skeleton** (Weber & Penn 1995, simplified; Veloren's family) | recognisable species, per-species numbers in RON | **first** |
| Space colonisation (Runions et al. 2007) | best broadleaf crowns | later upgrade, same output |

`TreeSkeleton`: nodes (parent, position, radius, level 0 trunk … 3 twig) plus leaf clusters,
**fixed-point i32 voxel units**, with angles from integer tables or `libm`, never `f32::sin`,
because worldgen runs in wasm and `cell_hash` is checked; add it to `determinism-wasm`.
**Age-parametric**: node `i` exists once `age ≥ birth_age(i)`, and lengths and radii scale with
age, so a growing tree keeps its branches.

### 4.5 Rasterisation into world blocks

- **Trunk and limbs**: a block fully inside the tapered cylinder → `<sp>_log`; a partial block →
  `ChiselData::from_fn` (bark outside, wood inside). Exact from ~12 cm radius.
- **Thin branches** below ~1 block → chiseled `<sp>_branch`, with a cuboid cap per block;
  twigs below ~2 voxels dissolve into the leaf mass.
- **Leaves**: whole `<sp>_leaves` blocks filling the clusters, dithered edge.
- Writes go only into air (or the tree's own blocks when growing), never over terrain or
  another tree.

### 4.6 See-through leaves (renderer)

- Registry `render: Cutout`, alpha from the PNG.
- **Mesher**: cutout blocks don't occlude neighbours; same-species leaf-to-leaf culling as a
  quality knob; AO treats cutout as non-occluding.
- **`voxel_pull`**: a **separate cutout pipeline** with alpha test + `discard`, so the opaque
  pipeline keeps early-z. Records split per chunk into opaque and cutout ranges (w0 bits 30–31
  are spare, `records.rs:4`). Prepass and **shadow** variants alpha-test, giving dappled shadows.
- WebGL2 per-chunk fallback: `AlphaMode::Mask` material.
- LOD rings stay opaque. `just pull-smoke` on a real adapter afterwards.

### 4.7 Growth

Through the wake-up queue (engine-mapping), never per tick. A stage is a block-delta edit:
rasterise `skeleton(age+1)` minus `skeleton(age)` into air or the tree's own blocks, and stop at
that stage if blocked. Thickening rewrites the tree's own chiseled blocks. Seeding: a hash per
(tile, season) × suitability × distance to a parent of the same species places a sapling.

## 5. General mechanism: detach terrain into a physics body, and back

The tree is the first user. The module (`crates/game/src/voxel_body.rs`) is generic.

### 5.1 Lifecycle

```
terrain blocks ──detach──▶ VoxelBody entity ──(falls, collides)──▶ at rest ──settle policy──▶
                                    │                                        ├─ despawn        (trees, now)
                                    │                                        ├─ items          (item feature, later)
                                    └──split (future)──▶ several bodies      ├─ re-attach      (rubble, stalagmites, future)
                                                                              └─ mix
```

### 5.2 Detach (`detach_blocks_safe(blocks: BlockSet, policy) -> EntityKey`)

- **Input**: a set of region-local block positions. *Who picks the set* is the caller: trees
  flood-fill their own tree id above the cut (§6.1); a future structural-integrity pass would
  pass "blocks no longer connected to supported ground"; a stalagmite passes its column.
- **One undoable edit**:
  - Read the blocks plus chisel into a **`VoxelModel`**: a local block grid in the same
    `ChunkBlocks` / chisel representation terrain uses, split into 32³ render bricks, so the
    mesher and renderer are shared.
  - Write air into the terrain (block-delta across the touched chunks).
  - Spawn the entity at the model's origin pose.
- **Grid alignment is kept**: the model's local grid is the world block grid at detach time,
  and the pose starts as a pure translation. Re-attach depends on this (§5.4).

### 5.3 The body

- **Collider**: compound of cuboids, greedy-merged from solid blocks: logs, and branches (a
  chiseled block → its cuboids, or its bounding box under a size threshold). Non-solid blocks
  (leaves) carry mass but no collider. Built once per model and cached by the model's content hash, so the seam
  handoff can send it by id.
- **Mass properties set explicitly**: mass, COM and inertia tensor summed per block from
  registry densities, in fixed order with `libm`, with collider density 0. This keeps dynamic
  voxel bodies off the eigen-decomposition path, a cross-target determinism hazard.
- **Free dynamic body** from the first tick. The caller supplies an initial impulse (trees:
  tipping; structures: none).
- **Contacts**: terrain, other bodies, units. Damage from contact events above a relative-speed
  threshold is a callback, so policy stays with the caller.
- **At rest**: rapier sleep, or speeds below named thresholds for N ticks, with a **timeout**
  that forces settlement.
- **Rollback**: everything is ordinary state (entity, model, body), journaled by the
  `StepJournal`. Add a test that rolls back mid-fall and checks the restore is bit-exact.

### 5.4 Settle policies

- **`Despawn`** (trees, now): remove the body, one undoable step, nothing left behind.
- **`Items`** (the separate item feature, later): instead of despawning, turn the model's
  blocks into items by a per-material yield table, at the body's resting pose. The voxel body
  code needs no change for it: it is one more policy at the same settle point.
- **`Reattach`** (future, designed now): write the model back into terrain.
  1. Snap the rotation to the nearest of the **24 axis-aligned orientations**. Blocks rotate
     exactly, and chisel cuboids rotate exactly because 90° maps a cuboid on the 16³ grid to a
     cuboid. Snap the translation to the block grid.
  2. Write blocks only into air. Blocks that would overlap terrain become items (rubble).
  3. If the snapped error is too large (resting at 45°), fall back to voxel resampling into
     chiseled blocks, which is lossy, or to items.

  The re-attached blocks are ordinary terrain again: edit ray, colliders and LOD all apply.
- **`Mixed`**: per-material rule (wood → items, stone → reattach).

### 5.5 Split (future, keep the hook)

A body breaking apart on impact is `split_body_safe(entity, pieces: Vec<BlockSet>)`: each
piece becomes its own body with its share of momentum, and the same settle policies apply.
Pieces come from connectivity or a fracture rule. Not built in the tree work, but `VoxelModel`
and the collider builder must work on any sub-set, so nothing tree-specific leaks in.

### 5.6 Rendering

A `VoxelBody` is a non-terrain voxel entity: its render bricks share one pose transform, and
leaves use the cutout path. Detach and re-attach must not pop. On detach, the terrain remesh
of the cleared blocks and the body's first draw land in the same frame. The bridge applies both
updates of the edit together (they come from one sim edit).

### 5.7 Seams

Bodies crossing seams is the seam handoff (generic: component-bag bundle, voxel-model collider
by id, terrain margin mirror). Until it lands, a detach caller may pick an initial impulse whose
reach stays in the home region. Trees do this as a temporary fallback, deleted when that work
lands.

## 6. Felling (the tree's use of §5)

### 6.1 Chop and falling set

- `DesignationKind::Chop` on a log or branch block with a tree id. It applies after a short
  timer until dwarves and the woodcutter job exist.
- **Falling set**: a flood fill over 26-connected blocks with the **same tree id**, seeded from
  the tree's blocks adjacent to the cut and on the far side of it (above, for a trunk cut;
  outward, for a branch cut), never entering the cut block. Everything of that tree beyond the
  cut that hangs together detaches; other trees' crowns (other ids) stay. Chopping a branch
  drops only that branch and its leaves. The fill is bounded by the species' maximum volume.
- `detach_blocks_safe(set, Despawn)`. The tree state becomes "stump" (trunk cut) or keeps
  growing (branch cut).

### 6.2 Fall

The initial angular impulse about a horizontal axis at the cut points away from the chopper or
toward the centre-of-mass lean, ties broken by `roll(tick, tree id, salt)`. A high centre of
mass over a narrow base tips it; it may slide off the stump, which is accepted. It can hang up
in another tree. Units hit above the speed threshold take damage.

### 6.3 At rest

The `Despawn` policy: the felled part disappears once at rest (or at the timeout). Logs, sticks
and seeds are the item feature's job; it replaces `Despawn` with `Items` for trees.

## 7. Order of work

Steps 4+ need the chunk-size change. Each step stands alone and can be looked at.

1. **Habitat rasters**: forge output, world-file bump, procedural stand-ins,
   `world-map --layer …`.
2. **`trees.ron`, niches and placement**: `worldgen::trees_in`, `world-map --layer trees`.
   Tests: cell vs region bit-identity, independence of call order.
3. **Skeleton + rasteriser** with a `just tree-view <species> [--seed --age]` tool;
   `determinism-wasm`.
4. **Trees in worldgen**: registry blocks (opaque leaves first), tree ids, crown band, `solid`
   flag. *Measure (ask first):* snapshot size, region start, mesh quads for a forested region.
5. **See-through leaves** (§4.6).
6. **`voxel_body` detach + body + rest detection**, generic, tested on a detached stone column
   with no tree code. Tests: native == wasm end pose, rollback mid-fall bit-exact, a physics
   golden.
7. **Chop designation + tree falling set + tipping + `Despawn` at rest**, with the interim
   seam fallback.
8. **Growth** (wake-up queue stages, seeding).
9. *(future)* `Items` policy (item feature), `Reattach`, split, structural integrity,
   stalagmites.

## 8. Related work

- Chunk size → `plans/2026-10-29-chunk-size-increase-handoff.md` (prerequisite).
- Entity seam crossing → `plans/2026-10-29-entity-seam-crossing-handoff.md` (removes the
  interim fallback; needs `VoxelBody` as a bundle type).

## 9. Decisions still open

1. **Leaf-to-leaf culling** default: fast (cull same-species neighbours) or fancy (draw all)?

Resolved 2026-10-29: leaves non-solid and branches solid; felled parts despawn at rest, with
items as a separate feature.
