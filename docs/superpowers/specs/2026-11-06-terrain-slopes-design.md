# Terrain slopes instead of half blocks — design

Date: 2026-11-06. Status: approved direction (Dawid, in conversation); details marked
**[call]** are Claude's calls, open to change.

## Why

Bodies walk up 0.6 m (`movement::STEP_HEIGHT`) and must jump every full 1 m block.
Generated terrain today rounds the surface to half blocks (one-cuboid `ChiselData`), so
gentle ground is walkable but every 1 m step is a jump, and the half blocks themselves are
a look Dawid wants gone. Fortress construction will also need stairs; how they are BUILT
in fortress mode is undecided (the building mechanics will be reworked) and is out of
scope here — but the shape introduced here is the one built stairs will reuse.

## Decisions (Dawid's)

1. **No half blocks in world generation.** The surface rounds to whole blocks.
2. **Every 1-block rise in generated terrain gets a slope** — a Minecraft-style stair
   shape, one tile per level, in the material of the block it is made of (a grass slope,
   a sand slope, a granite slope …), textured as that material. Natural ones are called
   **"<material> slope"** ("Dirt slope"), not stairs.
3. **Jumping up a full 1 m block stays.** Pathfinding keeps its `dy = +1` climb; slopes
   only make those climbs walkable where they exist. 1 m walls are not barriers.
4. Fortress-mode stair building: **out of scope** (TBD with the building rework).

## The shape

A slope is ONE block whose geometry is `ChiselData` (the same mechanism half blocks use
now), in that block's own material. Voxel units, `BLOCK_VOXELS` = 16, `HALF_BLOCK_VOXELS` = 8:

- **Straight**, low side `d` (one of ±x, ±z): bottom half `[0,0,0]–[16,8,16]` + the top
  half of the side away from `d` (e.g. low side +x → top `[0,8,0]–[8,16,16]`).
- **Outer corner**, low sides `d1`, `d2` (adjacent): bottom half + the one top quadrant
  away from both.
- **Inner corner**, low diagonal `(d1, d2)`: bottom half + three top quadrants (all but
  the one toward the low diagonal).

Every shape is octant-aligned, so colliders come out as half-grid cells only
(`block.rs` `push_block_cells`: straight 6 octants, outer 5, inner 7) — no voxel-scale
collider, same cost class as today's half block. Each rise is 0.5 m < `STEP_HEIGHT`, so
the character controller should autostep it without jumping (to be proven first — see
Risks).

**[call] Shape, not block type.** No `dirt_slope` / `grass_slope` ids: the slope is the
material's block id plus chisel geometry. So it mines, drops (one whole block item, as a
half block does today), and obeys every material-keyed rule with no mapping, and any
future surface material gets a slope for free. `Block.data` is untouched (its low bits are
the tree id; reusing them would make terrain look like trees).

**[call] One shape module in `game`** (`crates/game/src/slope.rs`): the `SlopeShape` enum,
`slope_chisel(material, shape) -> ChiselData` (built through `ChiselData::from_fn` so the
cuboid list stays canonical — hand-packing two cuboids would not match the greedy merge),
`classify(&ChiselData) -> Option<SlopeShape>`, and `display_name(block, chisel)` →
"Dirt slope" / "Dirt". `game` owns it because worldgen, the sim and the client all read it,
and built stairs will reuse it.

## Placement in worldgen

**[call] Carve, don't add.** The slope replaces the TOP block of the HIGHER column; the
lower column is untouched. So every column's surface block y (`ColumnGround.surface`,
`ColSample.top`) is exactly what whole-block rounding gives — implied ground, `is_dug_air`,
x-ray interior marking and pathfinding standable tiles need no change — and the slope's
material is the high column's own surface block (grass on a grassy step).

For a column with top `t` (rounded surface, `ColSample::top`), a cardinal neighbour is
**low** when its top is exactly `t − 1`; a diagonal likewise. Then:

| low cardinals | low diagonals | shape |
|---|---|---|
| exactly one, `d` | any | Straight, low side `d` |
| two adjacent, `d1`,`d2` | any | Outer corner `(d1, d2)` |
| none | exactly one, `(d1,d2)` | Inner corner toward it |
| two opposite, three, four | — | **[call]** none: whole block (a 1-wide ridge / nose stays a jump) |
| none | two or more | **[call]** none: whole block |

No slope when: the column's surface is at or under its water surface (the empty quarter
blocks would be air pockets — today's "submerged surface is always whole" rule), the
column carries glacier ice (`top != s`), or the column has no ground (all air). A
neighbour that is under water still counts as low (a shore step becomes a slope).
Rises of 2+ blocks are cliffs and get nothing. Slopes are decided from samples only
(the high column and its 8 neighbours), never from other slopes, so there is no cascade
and generation stays window-independent.

**Rounding:** one test height, the block's midpoint (`SURFACE_TEST_M = 0.5`):
the surface block is the highest block whose density at `by + 0.5` is positive. LOD
(`coarse_tile`) uses the same `surface_from_profile`, so coarse and full generation keep
agreeing; LOD never saw half blocks and needs no slope logic (any-solid counts a slope
cell as solid, as it counted half blocks).

**Neighbour-dependent rules that change:**
- *Cliff rule* (`open_lo`): a slope neighbour leaves its own top block open, exactly as a
  half block did (`!full` → `slope.is_some()`).
- *Trees* never stand on a slope column (a trunk would hang over the empty front).
  Tree placement reads `tree_ground` up to the sample ring's edge, so the ring grows by
  one block; the coarse path (which has a per-column memo and no ring) must sample the
  neighbours it needs the same way.

## Sim and client

- **Pathfinding topology is unchanged** (a slope is a solid block; you stand on top of it,
  at the high column's level). **[call] Directional walk-up:** a `dy = +1` step onto a tile
  whose floor is a slope is a *walk-up* only when it arrives from the shape's low side —
  Straight: from the low cardinal `d`; Outer corner: from either low cardinal or their
  diagonal; Inner corner: from the low diagonal only. A walk-up is charged like a level
  step (no `CLIMB_COST` — DF: "walking up a ramp has the same cost as level ground"), so
  paths prefer slopes over jumps; every other `dy = +1` step — onto a slope from a wrong
  side included — stays a jump climb exactly as today. The heuristic stops charging
  `CLIMB_COST * dy` so it stays admissible (A* still returns cheapest paths). That needs
  the source to know each floor's shape: `TileSource` gains `slope(p) -> Option<SlopeShape>`
  and pathfind a pure `walk_up(shape, from_dir) -> bool`; the sim impl classifies the
  chisel, the client `TileMirror` (blocks only today) keeps a sparse map of slope cells →
  shape, filled from each chunk's chisel.
- **Adventure walker** (`adventure::steer`): no jump on a walk-up step (same `walk_up`
  test on the waypoint it heads for); jumping everywhere else is unchanged.
- **Commander height grid** (`commander/height.rs`) samples the block-centre voxel column,
  which reads a slope as full or half height depending on its facing; it should read a
  chiselled block's top as its highest cuboid (the same rule `tree_fall.rs`'s `Ground`
  uses) — a slope counts as full height.
- **Names**: `display_name` is used wherever the UI names a block (nothing names terrain
  blocks yet; the function + test land now, wiring when a block tooltip exists).

## Non-goals

Building slopes/stairs in fortress mode; wedge (non-axis-aligned) geometry; slopes on
2+ m cliffs; changing jump or step height; converting already-saved regions (a saved
region keeps its half blocks; debug worlds re-create with `just run --reset`).

## Risks

1. **Autostep on a 0.5 m tread.** `character_controller` sets autostep
   `min_width: Relative(0.5)`; whether a 0.5 m-deep tread passes is unverified. Prove it
   first with a sim test on a hand-built slope (every facing, head-on and diagonal, up and
   down). `min_width` evaluates against the capsule's AABB x-dimension (≤ 0.4 m here,
   < the 0.5 m tread), so it should pass as is. **[call] Fallback ladder:** (a) set
   `min_width` to an `Absolute` value below the tread; re-record `physics_goldens` only if
   they move, and `walking_smoothness` / `shape_cast_grazing` must stay green; (b) if
   walking still fails, ship worldgen slopes anyway with the walker jumping onto them
   (`walk_up` false) and STOP to report before any KCC fork patch — patching the fork is
   Dawid's decision.
2. **Golden churn.** `crates/worldgen/tests/scene_hash_golden.rs` re-pins (generated
   region). Game-crate goldens (`physics_goldens`, `wasm_determinism`) use hand-built half
   blocks and do not change.
3. **Look.** Gentle hills become flat terraces with a slope at each 1 m edge; steep hills
   become continuous slope runs. Needs a live look (`just run --reset`).

## Affected code (from the 2026-11-06 survey)

`crates/worldgen/src/lib.rs` (rounding 82–133, `ColSample.full` 141, `point_sample` 164–180,
glacier 237, `tree_ground` 278–283, sample ring 537–547, cliff rule 591–596, emission
655–666, coarse tree memo 876–883, unit tests 998–1064), `examples/quantize_probe.rs`;
worldgen tests `scene_hash_golden`, `lod_bricks`, `underground`, `trees_in_terrain`,
`tree_fall`, `player_walk`; `crates/game/src/{pathfind.rs, adventure.rs, orders.rs
(TileSource), movement.rs}`; client `commander/{tiles.rs, height.rs}`; CLAUDE.md's worldgen
paragraph. Superseded spec: `2026-09-22-block-and-half-block-worldgen.md`.
