# 02 — The map and the z-level model

Part of the Dwarf Fortress research (2026-10-20); start at `00-synthesis.md`. Source tags as in
`01-world-and-embark.md` ([wiki], [mem], [unverified]).

## 1. Tiles and z-levels

What it does for the player: the fortress is a 3D grid seen one horizontal slice at a time.
Everything the player builds is a change of tile type; everything a dwarf does starts with
"walk to an adjacent tile".

The tile model, as DFHack's `tiletype` table exposes it [mem]: each tile is
**shape × material class × variant**:

- **shape**: `WALL`, `FLOOR`, `RAMP`, `RAMP_TOP` (the open space above a ramp, drawn ▼),
  `STAIR_UP`, `STAIR_DOWN`, `STAIR_UPDOWN`, `FORTIFICATION`, `EMPTY` (open space, no floor),
  `BOULDER`/`PEBBLES` (floor variants), tree trunk/branch/twig shapes, `BROOK_BED`, …
- **material class**: layer stone, mineral vein, soil, lava stone (obsidian), feature stone
  (e.g. adamantine), construction, frozen liquid, grass/plant, …. The *concrete* material
  (granite, sandy clay, …) is not in the tile type; it is looked up per map block from the
  biome's layer table, the vein list, or the construction list [wiki Maximizing_framerate,
  "Displaying a map tile" list].
- **variant**: rough / smooth / engraved (engravings are a separate list), worn floors.

Per tile DF also keeps **designation bits** (dig type, hidden, liquid level 0–7 + liquid type,
traffic level, outside/subterranean/light flags, …) and **occupancy** bits (building, unit,
item present) [mem]. The map is stored in **16×16×1 map blocks** [wiki Maximizing_framerate:
"a list specific to the 16x16x1 map block"].

Z-levels [wiki Z-level]: discrete layers, "around 50 z-levels of land … with an additional 15
z-levels of empty sky", 6 to 600+ depending on worldgen; ISLAND-template worlds make caverns
much taller than REGION worlds [wiki Cavern, Maximizing_framerate].

A **floor belongs to the tile above the wall it sits on**: digging out a wall at level z leaves
a floor at z; a "channel" removes that floor too, opening z to z−1 and carving a ramp on z−1
[wiki Channel]. "Walls include their own floor above them" [wiki Ramp]. Constructed floors can
exist over open space.

## 2. Movement across levels

[wiki Path, Ramp, Stairs]:

- Creatures move to the 8 neighbours on one level and straight up/down where possible, and
  **through diagonal gaps**.
- **Stairs**: up stair below a down stair (or up/down stairs) connect levels vertically. Stairs
  do not block creatures or fluids. Wagons can't use stairs.
- **Ramps** move a creature *diagonally across a level* in one step. A usable ramp needs: the
  ramp tile, open space above it, an adjacent wall on the same level (8-neighbourhood), and a
  walkable tile on top of that wall. Ramps are the only way wagons change level. "Walking up or
  down a ramp has the same movement cost as walking on level ground."
- Natural slopes on the surface are ramps. Carved/natural ramps collapse when their supporting
  wall is mined out.
- v50 overhauled stair designation: the player drags a multi-level box and the game infers
  up/down/updown stairs [wiki Stairs].

## 3. Digging and building the map

[wiki Mining, Channel, Ramp, Stairs, Designations menu] + [mem]:

| action | effect | notes |
|---|---|---|
| dig (mine) | wall → floor | stone and ore drop a boulder/stone item (v50: drop chance changed, see below); soil drops nothing [wiki Stone_layers] |
| channel | wall/floor → open space, wall below → ramp | can drop 0, 1 or 2 stones; miners will climb down into their own channel |
| up / down / up-down stair | carved into a wall | v50 infers types from a box |
| ramp | carved into a wall | |
| remove up stairs / ramps | | fast, trains miners |
| smooth | rough wall/floor → smooth | raises room value; prerequisite for engraving and fortifications |
| engrave | smooth → engraved with a quality level | depicts history; big room value |
| carve fortification | smooth wall → fortification (see/shoot through, not walk) | |
| carve track | minecart track | later |
| construct | open/floor → wall, floor, ramp, stairs, fortification from boulders, blocks, logs, bars | constructions never collapse by support rules |

Mining speed depends on the miner's skill and the material (soil fast, stone slower) [mem].
Stone drop rate: v0.47 dropped a stone ~25% of the time on stone tiles [mem, unverified for
v50]; it is a standing-order/difficulty knob in premium [unverified].

**Cave-ins** [mem]: when a dig leaves a connected mass of natural tiles with no support path to
the ground or map edge, the whole mass falls as a cave-in, injuring what it lands on and
displacing fluids ("pistons", [wiki Flow]). Stairs: "an up stair … will stop tiles falling;
a down stair will not" [wiki Stairs].

**Hidden tiles** [mem]: undug stone is hidden until a dwarf exposes it; digging reveals the
tile and its neighbours; caverns and veins are announced when first seen. Unrevealed tiles are
also the cheapest to draw [wiki Maximizing_framerate]. Damp (aquifer) and warm (near magma)
stone are revealed as warnings and cancel dig designations by default [wiki Stairs, Aquifer].

## 4. What is underground

[wiki Stone_layers, Cavern, Aquifer, Magma_sea] + `../2026-09-20-df-worldgen-research/03-geology.md`:

- **Soil**: 0 to 10+ levels; farmable without irrigation, digs fast, leaves no stone, holds
  clay/sand, often carries the aquifer; can't be smoothed or engraved.
- **Stone layers**: sedimentary (80 % chance to come first after soil; coal, iron ore, most
  fluxes — "make-or-break for steel"), *or* igneous extrusive (mutually exclusive with
  sedimentary), then metamorphic, then igneous intrusive. Layers are homogeneous and change
  sharply; ores and gems are veins, clusters and inclusions by host-layer tables.
- **Aquifers**: light (~19 in 20) or heavy, per embark square; an aquifer tile leaks water
  into open neighbours N, S, E, W and below, forever ("cannot be drained"); smoothing or mining
  the tile stops it.
- **Caverns**: three layers by default; the top of the first is "about 10–11 z-levels below the
  surface"; each spans several levels; water from pools to a flooded sea; open map edges let
  cavern creatures migrate in; forgotten beasts come from here. Passages and deep pits connect
  layers.
- **Magma sea** below the third cavern; magma pools/pipes rise from it; then the underworld
  ("HFS").

## 5. Mapping DF's grid onto our 1 m block grid

Our facts: a block is 1 m and 16³ voxels; a chunk is 32³ voxels = **2×2×2 blocks**
(`game::Chunk::blocks` has 8 entries); half blocks exist as one-cuboid `ChiselData`
(`docs/superpowers/specs/2026-09-22-block-and-half-block-worldgen.md`).

**Option A — one DF tile = one block (1 m cube), one z-level = 1 m.**
The sim grid is the block grid. A 4×4 embark is 192 m. Natural slopes in our terrain are
already 1-block steps with half blocks between, which is DF's "surface made of ramps". Dwarves
must fit a 1 m corridor, so they render at ~0.8–0.9 m: stylised, short and stocky, which suits
dwarves. Pro: every existing piece (block registry, editing, meshing, LOD, worldgen quantisation)
already speaks blocks. Con: a 1-tile corridor reads cramped in 3D; ceilings are 1 m.

**Option B — one tile = 1 m × 1 m footprint, one z-level = 2 blocks (2 m).**
Rooms read like rooms at human scale. But the terrain generator produces 1 m steps, so the
surface would have to be re-quantised to 2 m levels (staircase hills) or carry half-level
ramps everywhere; every dig removes two blocks; the voxel chunk (2 blocks tall) aligns with one
z-level, which is neat for meshing but nothing else.

**Option C — one tile = one chunk (2 m cube).** 8× the voxel volume per tile, a 192-tile
embark is 384 m, and the existing LOD/region budgets were measured for far less volume. Reject.

Recommendation: **A**, with dwarves scaled down; flagged as an open question for Dawid because
it fixes the look of the game.

Under A, our sub-block geometry gets DF meanings:

| DF concept | our representation |
|---|---|
| wall | solid block |
| floor | air tile whose block below is solid *or* a thin floor slab (constructed floor over open space = a cuboid chisel at the bottom of the tile) |
| open space | air with air (or open) below |
| ramp | a wedge or two-step chisel shape; worldgen half blocks become **natural ramps** in the tile model (they already sit where the surface changes level) |
| stairs | chisel stair shapes (up / down / up-down variants) |
| fortification | chisel lattice |
| smooth / engraved | texture variant / decal layer (no geometry change) |
| boulders, items on floor | entities on the tile, not voxels |

The tile map, not the voxel chunk, should be the simulation truth (see `engine-mapping.md` §2):
DF needs per-tile flags and liquid levels the voxel chunk doesn't carry, and the per-chunk
rapier colliders are useless to grid agents.

## 6. Simulation cost

The map is nearly free per tick: it changes only on events (dig completes, construction
completes, cave-in, freeze/thaw). The costs are on change:

- **connectivity update** (pathing components, `03-designations-and-jobs.md` §5) — DF's own
  lag spikes come from map-connection changes (doors, bridges, atom smashers on repeaters)
  [wiki Maximizing_framerate];
- **support check** for cave-ins (local flood fill);
- **remesh** on the client.

## 7. MVP tiers

| system | tier |
|---|---|
| Tile map with shapes wall / floor / open / ramp / stairs, material per tile | **MVP** |
| Dig, channel, stairs, ramp designations | **MVP** |
| Hidden / revealed tiles | **MVP** (cheap and core to the feel) |
| Soil + stone layers + ores from the forge's geology | **MVP** (two or three layers, one ore) |
| Constructions (wall, floor, ramp, stairs from boulders/logs/blocks) | soon after |
| Smoothing, engraving, fortifications | soon after (room value depends on them) |
| Cave-ins | soon after (needed before players dig big halls) |
| Aquifers | later (needs fluids) |
| Caverns, magma sea, underworld | later |
| Trees as multi-tile objects | MVP as single-column trees; multi-tile later (DF: "multi-tile trees are a known source of lag") |

## Sources

- https://dwarffortresswiki.org/index.php/Z-level
- https://dwarffortresswiki.org/index.php/Path
- https://dwarffortresswiki.org/index.php/Ramp
- https://dwarffortresswiki.org/index.php/Stairs
- https://dwarffortresswiki.org/index.php/Channel
- https://dwarffortresswiki.org/index.php/Mining
- https://dwarffortresswiki.org/index.php/Stone_layers
- https://dwarffortresswiki.org/index.php/Aquifer
- https://dwarffortresswiki.org/index.php/Cavern
- https://dwarffortresswiki.org/index.php/Magma_sea
- https://dwarffortresswiki.org/index.php/Flow
- https://dwarffortresswiki.org/index.php/Maximizing_framerate
- DFHack `df-structures` (tiletype enum) — https://github.com/DFHack/df-structures [mem: not re-read]
