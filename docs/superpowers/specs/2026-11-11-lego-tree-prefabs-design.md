# Trees as LEGO-part prefabs — design

Date: 2026-11-11. Status: **parked** — direction agreed, not implemented. Once the prefab system is
built, this plan is revisited and adjusted against what that system actually provides, and only then
implemented.

Replaces the tree **shape** step of `crates/worldgen/src/stud_trees.rs` (voxel skeleton rasterised
onto stud cells and tiled with rectangular bricks). The prefab mechanism itself (format, baking,
storage, wire, rendering, collision) is planned and built by another agent; this document is the
tree side of that contract: which prefabs exist, what they are made of, and what trees need from
the mechanism.

## Decisions (the user's)

- Trees are **prefabs assembled from LEGO's own moulded plant parts**, as LEGO sets build them.
- **No tree is built from square / rectangular bricks or plates.** The skeleton rasteriser, the
  greedy box merge and `fill_box` tiling of wood and leaves go away.
- The prefab mechanism is another agent's work.
- **Any part shape is allowed** — round, moulded, organic. Only building trees out of plain
  rectangular bricks and plates is out.
- **A tree is felled whole.**
- **Colliders must fit the shape** of the tree (requirement 7).
- **No cactus for now**: the species is dropped from generation until it is taken up again.

Everything else below is my proposal.

## What the library offers

`assets/ldraw` holds parts only — no official set models — so a "set tree" here is an assembly of
moulded parts. Sizes are measured from the part geometry (1 LDU = 0.025 m; a stud is 0.5 m, a plate
0.2 m; a minifigure is 2.4 m).

| Part | LDraw name | Footprint | Height | Role |
|---|---|---|---|---|
| `3471` | Plant Tree Pyramidal 4x4x6.667 Type 1 | 2.2 m | 3.8 m | conifer crown / whole small conifer |
| `52211` | Plant Tree Pyramidal 4x4x6.667 Type 2 | 2.2 m | 3.8 m | second conifer mould |
| `2435` | Plant Tree Pyramidal 3x3x4 | 1.6 m | 2.3 m | conifer sapling |
| `3470` | Plant Tree Oval 4x4x6 | 2.2 m | 3.4 m | broadleaf crown |
| `3778` | Plant Tree Columnar 4x4x11.5 | 2.4 m | 6.8 m | tall narrow conifer |
| `2563a` | Plant Tree Palm Base | 1.0 m | 0.8 m | trunk foot (2x2 studs) |
| `2536a` / `2536b` | Plant Tree Palm Trunk | 0.7 m | 0.875 m pitch | trunk segment, tilts at each joint |
| `2566b` | Plant Tree Palm Top | 0.8 m | 1.6 m | holds four palm leaves |
| `2518` | Plant Tree Palm Leaf Large | 4.9 x 2.3 m | — | palm frond |
| `6148` | Plant Palm Leaf Small | 4.0 x 1.8 m | — | small frond |
| `2518c01` | Palm with 7-segmented trunk (official shortcut) | 8.8 m spread | 8.6 m | a finished palm, 15 parts |
| `30338` | Plant Tree Palm Base 4x4 | 2.0 m | 6.8 m | one-piece giant trunk |
| `30339` | Plant Tree Palm Leaf Quadruple | 8.65 m spread | — | one-piece canopy for `30338` |
| `2417` | Plant Leaves 6x5 | 3.0 x 2.5 m | 1 plate | leaf plate, sandwiched on a trunk stud |
| `2423` | Plant Leaves 4x3 | 2.0 x 1.3 m | 1 plate | small leaf plate |
| `6064b` | Plant Bush (spiky) | 2.0 m | 2.6 m | bush, desert shrub |
| `30131` | Tree 6x6 Hollow Stump | 3.0 m | 2.25 m | ancient stump (landmark) |
| `16981`, `1997` | Plant Vine | 8 m long / curled | — | jungle dressing |
| `10884`, `30239` | Plant Leaves 6x5 Swordleaf | 3 m | — | jungle undergrowth |
| `30176` | Plant Brick 1x1 Round with 3 Bamboo Leaves | 0.5 m stem | 1 brick | stacks into bamboo |

There is no cactus mould in the library (checked by name and keyword); cactus is dropped for now.

## Roster: species → prefabs

Seven of the eight species of `trees::SPECIES` stay, with their niches and placement; **cactus is
removed** (its niche simply grows no tree). Each gets an **age
ladder** of prefabs instead of a grown skeleton; `age` picks the rung, the seed picks a variant, a
quarter turn and a colour. "Trunk(n)" is `2563a` + n × `2536a` stacked as in `2518c01` (0.875 m
pitch, a small tilt per joint).

| Species | Young | Mature | Old | Colours (LDraw codes) |
|---|---|---|---|---|
| **pine** | `2435` on the ground (2.3 m) | Trunk(2) + `3471` (6.4 m) | Trunk(4) + `3471` (8.2 m) | foliage 2 green / 288 dark green; trunk 70 reddish brown |
| **spruce** | `2435` (2.3 m) | `52211` on the ground (3.8 m) | `3778` (6.8 m) | 288 dark green; 378 sand green in cold biomes |
| **oak** | `3470` on the ground (3.4 m) | Trunk(2) + `3470` (6.0 m) | Trunk(3), a `2417` at the top two joints, + `3470` (6.9 m, crown 3 m wide) | 2 green / 10 bright green; trunk 308 dark brown |
| **birch** | Trunk(3) white + one `2423` on top | Trunk(5) white, `2423` at three joints, `2417` on top (5.4 m) | Trunk(6) white, `2423` at four joints, `2417` on top (6.3 m) | trunk 15 white; leaves 27 lime / 10 bright green / 14 yellow |
| **acacia** | Trunk(2) leaning + one `2417` | Trunk(3) leaning, `2417` at the top two joints, turned 90° to each other (3.6 m, crown 3 m) | Trunk(4), three `2417` | leaves 330 olive green; trunk 28 dark tan |
| **palm** | Trunk(3) + `2566b` + 4 × `6148` | `2518c01` with 5 segments | `2518c01` as shipped (8.6 m) | leaves 2 green; trunk 6 brown / 70 reddish brown |
| **jungle** | Trunk(4) + `2566b` + 4 × `2518` | `30338` + `30339` (7 m, 8.65 m canopy) | `30338` + two `30339` 45° apart, `16981` vines hanging | 2 green / 288 dark green; trunk 308 dark brown |

Notes on the roster:

- **Pine vs spruce** read differently on purpose: a pine is a crown lifted on a bare trunk, a spruce
  is skirted to the ground.
- **Trunks are palm-trunk segments in every species**: it is the only moulded trunk in the library,
  and its jointed tilt gives each tree a lean that the grid cannot.
- **Leaf plates are sandwiched** between a trunk segment's stud and the next segment, alternating
  quarter turns and using the plate's off-centre holes for asymmetry.
- **Dressing, not trees** (the same mechanism, placed by habitat, no wood): `6064b` bushes, `30131`
  ancient stump, `30176` bamboo stacks, swordleaves, ferns `2682a`, flowers `3741ac0x`.
- Connections are plausible stud-in-tube fits, not verified against a purist building standard.

### Variety

A moulded tree is nearly four-fold symmetric, so rotation alone hides little. Variety comes from:

- 2–3 variants per rung (segment count ±1, lean direction, which joints carry leaves);
- one of four quarter turns per instance;
- foliage colour from a short per-species list, chosen by biome first and seed second.

That is roughly 20–30 distinct looks per species from 6–9 authored prefabs.

## Scale: trees get smaller

Voxel-derived trees were 10–32 m tall with crowns up to 16 m wide. These are **2.3–8.6 m** with
crowns of 2–9 m, i.e. one to three and a half minifigures tall — set proportions.

Consequences for existing code:

- `Species::max_height_blocks`, `crown_radius_blocks` and `ShapeParams` no longer describe anything;
  the species table shrinks to niche, ladder and colours.
- Forest density must rise to keep forests looking closed: `trees::TREE_LATTICE_BLOCKS` and the
  per-biome fill are re-tuned (check with `just world-map` and in a real forest region).
- `STUD_TREE_SCAN_TILES_H/V` in `game/src/stud_trees.rs` shrink or disappear with brick scanning.

## What trees need from the prefab mechanism

1. **Free rigid transforms per part.** LEGO's own `2518c01` tilts every trunk joint by 7–25° and
   fans the fronds; `StudBrick` (grid cell + quarter turn about Y) cannot express it. A tree prefab
   is a rigid assembly of parts with full LDraw matrices, anchored on the stud grid only at its foot.
2. **Sub-plate heights.** The trunk pitch is 35 LDU (4.375 plates) and `2435` is 91 LDU tall.
   Prefab extents do not land on plate boundaries.
3. **Per-instance colour slots.** At least two (foliage, trunk), as LDraw colour codes, so one
   authored prefab serves several biomes.
4. **Per-instance quarter turn** about Y at the foot.
5. **A tree is an instance, not bricks.** (species rung, variant, turn, colours, foot position) is a
   few bytes against today's 186–240 bricks. This is where the snapshot and stored-chunk cost of
   forests goes away; trees should not force a surface chunk to be stored whole.
6. **Instance identity.** Felling, orders and pathing need a stable id per tree. Today that is
   `stud_tree_id` in `StudBrick::data`; the placement lattice id (`trees::placement::tree_id`) can
   serve directly.
7. **Colliders that fit the shape.** A tree's collider is a **compound of per-part shapes, each
   following its part's transform** — never one box, one hull around the whole tree, or the part's
   occupied stud cells.
   - Each solid part gets a convex hull baked from its LDraw geometry (as `tools/ldraw-bake` already
     does for slopes). The pyramidal, oval and columnar moulds are close to convex, so one hull each
     fits well; a trunk is one hull per segment, so a curved palm trunk collides as a curve.
   - A concave part (`30131` hollow stump, `30338` if its flare matters) is baked as a convex
     decomposition, not a single hull.
   - A crown lifted on a trunk leaves the space under it open: a minifigure walks under a pine's
     crown and past the trunk.
   - Leaf plates, fronds and vines are **not solid** for movement and pathing (a per-part flag), but
     still belong to the falling body's shape when the tree is felled, so a felled palm rests on its
     fronds.
   - The same compound is the pick / chop target, so the cursor selects what is drawn.
   - Hulls are deterministic bake output and part of the hashed sim state's inputs: re-baking them is
     a physics change (`just determinism-wasm`, physics goldens).
8. **Footing rule.** A moulded foot cannot "reach down" the way the brick trunk does today. Proposal:
   the foot stands on the lowest stud column under its footprint if the footprint's columns differ
   by at most one brick, sinking visually into the higher ones; otherwise the tree is not placed.
   Trunk-footed trees need 2 x 2 studs, the one-piece moulds 4 x 4.
9. **Canopy overlap is allowed.** Non-solid parts of neighbouring trees may interpenetrate; only
   solid parts claim space.
10. **Composite catalog entries.** Parts to bake: every number in the table above, plus `3742`,
    `4032a` and `3062b` (the coconut in `2518c01`).

## Felling and wood

- A tree falls **whole** (agreed): the instance becomes one dynamic body turning about its foot
  (`tip_angvel` stays). There is no partial or branch chop; the height-and-support rule goes.
- **Yield is a table, not a volume.** `mining::stud_drops` counts cells per material, which means
  nothing for a moulded part. Each rung declares its logs (and seeds / fruit later).
  Proposed logs: young 1, mature 2–3, old 4–6, jungle old 8.
- Whether a stump stays behind (as in LEGO Worlds) is open.
- Trees are smaller, so one tree yields less wood than before; density makes up for it.

## Prior art: how LEGO Worlds did trees

Researched 2026-11-11 from reviews and the community wikis (the wikis block direct fetches, so this
is from search extracts; no developer talk or file-format breakdown was found). LEGO Worlds had
trees in **two separate systems**:

- **Prop trees** — the small ones ("Small Tree", "Large Tree", "Tree 4", apple trees, stumps, charred
  and felled trees). Per the wiki, props "are made of LEGO bricks but are not integrated into the
  same system as terrain", cannot be opened in the brick builder, and use their own texture,
  coordinate and physics systems. They smash whole into studs and items, and leave stumps that
  often spawn nuts. They are found with the Discovery tool and re-placed for studs.
- **Brick-build trees** — the big forest trees ("Large Oak Tree", "Rainforest Tree", "Autumn Tree",
  "Savanna Tree", "Mangrove", "Spooky tree", "Dead Tree"). These are models stamped into the
  terrain's own brick grid, so they are editable and destructible brick by brick like the ground.
  Leaf materials exist as terrain paints (e.g. "Autumn Leaves").
- Biomes mix both: the wiki describes Whispering Woodlands as "very tall, narrow conifers and
  smaller, prop trees".

What it means here:

- Our prefab trees are LEGO Worlds' **prop** system: an instance outside the brick grid, with its own
  transform and physics, broken as a unit. Requirements 1, 5 and 7 above are the same split they made.
- LEGO Worlds reached tall forests only through brick-built trees, which the user has ruled out. Our
  height has to come from trunk segments and the tall one-piece moulds (`3778`, `30338`); 8–9 m is
  the practical ceiling.
- Worth copying: a **stump left behind** after felling (with a seed / nut drop), numbered variants
  per biome, and trees being discoverable, placeable items.

Sources: [LEGO Worlds wiki — Props](https://legoworlds.fandom.com/wiki/Props),
[Brick Builds](https://lego-worlds.fandom.com/wiki/Brick_Builds),
[Biomes](https://lego-worlds.fandom.com/wiki/Biomes),
[Fantasy Forest](https://lego-worlds.fandom.com/wiki/Fantasy_Forest),
[Brickipedia — list of brick builds](https://en.brickimedia.org/wiki/List_of_LEGO_Worlds_brick_builds),
[Wikipedia — Lego Worlds](https://en.wikipedia.org/wiki/Lego_Worlds).

## Non-goals

- The prefab format, bake, storage, wire and renderer.
- Far-distance trees (none are drawn today either).
- Growth over time and seasons (the ladder and colour slots make both possible later).
- Player-built trees.

## Open when this is revisited

1. Scale: is 2.3–8.6 m what is wanted, or should tall trees be pushed taller with longer trunks?
2. Does a felled tree leave a stump (with a seed / nut drop)?
3. Everything marked "requirement" above, checked against the prefab system as built.

## Left to the builder

- Authoring the prefabs as `.ldr` (LDraw is what `tools/ldraw-bake` already reads), reusing
  `2518c01`'s joint transforms for every trunk.
- Exact variant counts, colour lists per biome, density tuning, yield numbers.
- Hull simplification for the large moulds (`3778`, `30338`, `30339`).
