# 05 — Economy and production: items, stockpiles, workshops, reactions, materials, value

Part of the Dwarf Fortress research (2026-10-20); start at `00-synthesis.md`. Source tags as in
`01-world-and-embark.md`.

## 1. Items

What they are for the player: everything movable — boulders, logs, bars, blocks, food, drink,
seeds, furniture before it is built, tools, weapons, clothes, corpses. Items are the fortress's
inventory *and* its physical clutter: each sits on a tile (or in a container, or carried).

Data model [mem]: an item has a **type** (from item raws: `ITEM_WEAPON`, `ITEM_TOOL`, …; the
built-in types — BAR, BLOCKS, BOULDER, WOOD, … — are hardcoded), a **material** (any material,
§3), a quality, wear, decorations, a stack size, a location (tile / container / unit /
building), and flags (forbidden, dump, melt, hidden, in-job, foreign). Tarn: making the item
system polymorphic "was ultimately a mistake"; he now prefers a generic "tool" item whose
behaviour comes from flags (stepladder, beehive, mortar) and "if different components can just
be turned off and on, it's easier" [SO-2021]. That is an ECS argument, and matches ours.

## 2. Stockpiles

[wiki Stockpile]:

- A painted set of tiles (rectangles, can be non-contiguous) with **settings**: categories —
  ammo, animals, armor, bars/blocks, cloth, coins, corpses, finished goods, food, furniture, gems,
  leather, refuse, sheets, stone, weapons, wood — each with sub-filters by item type, material
  and **quality** (core and total quality ranges) [wiki Stockpile, "Stockpile categories",
  "Stockpile Settings"].
- One item per tile, or one container (bin/barrel/bag) per tile holding many.
- Haulers move matching loose items to the **nearest empty spot to the item** (straight-line,
  ignoring obstacles); newest items first.
- **Give/take links**: a stockpile can give only to named stockpiles/workshops and a workshop can
  take only from linked stockpiles — the main logistic control tool.
- Settings for max wheelbarrows/bins/barrels per stockpile.
- Food rots outside stockpiles; inside, it doesn't [wiki Miasma] — stockpiles are also food
  preservation.

## 3. Materials

[wiki Material_science] + [mem]:

- One material system for everything: **inorganic** materials (`INORGANIC` raws — stones, ores,
  metals, gems, glass) and **organic** materials attached to creatures and plants (bone, meat,
  leather, wood, plant fibre, seeds, milk, …) via `MATERIAL_TEMPLATE`s.
- Properties per material: solid density, specific heat, ignition/melting/boiling points, heat
  damage, colour, **material value**, and the mechanical set used in combat — impact / compressive
  / tensile / torsion / shear / bending **yield, fracture and strain-at-yield** (strain in parts per
  100,000). "At present, DF seems to only apply forces during combat."
- Stone flags: which layer type it appears in, ore yields (`METAL_ORE:IRON:100`), flux, fire-safe
  / magma-safe; economic vs. "other" stone (stone-use permissions in the labor menu)
  [wiki Labor "Stone use"].
- Metals: bars from smelting ore; alloys from reactions (bronze, steel needs flux + coal).

## 4. Workshops and reactions

Workshops are 3×3 (most) or 5×5 (trade depot, siege workshop) [mem] buildings where a dwarf
with the labor performs a task on reagents and makes products. The vanilla set
[wiki Workshop]: Carpenter's, Mason's, Craftsdwarf's, Stoneworker's, Mechanic's, Jeweler's,
Bowyer's, Butcher's, Tanner's, Farmer's, Clothier's, Dyer's, Soap maker's, Siege workshop,
Metalsmith's forge, Glass furnace, Wood furnace, Kiln [mem], Smelter, plus magma variants (no
fuel), Still, Kitchen, Loom, Quern/Millstone, Fishery, Ashery, Screw press [mem].

Most jobs are **hardcoded** ("building beds or creating glass" [wiki Reaction]); a minority are
data-driven `REACTION` raws:

```
[REACTION:<id>] [NAME:…] [BUILDING:<workshop>:<key>]
[REAGENT:A:150:BAR:NONE:POTASH:NONE]
[PRODUCT:100:1:BAR:NONE:PEARLASH:NONE][PRODUCT_DIMENSION:150]
[FUEL] [SKILL:…] [MAX_MULTIPLIER:…] [AUTOMATIC] [ADVENTURE_MODE_ENABLED]
```

— reagents (item type + material filters + quantity), products (probability, count, type,
material), fuel requirement, skill trained, automatic enqueue; reactions must be listed in the
civilization's entity raws to be available [wiki Reaction]. For our engine **all** workshop
jobs should be reactions in data (DF's hardcoded split is history, not design).

Canonical chains (MVP-relevant first):

| chain | steps |
|---|---|
| wood | tree → (chop) log → carpenter: bed, door, table, chair, barrel, bin, bucket … |
| stone | wall → (dig) boulder → mason: door, table, chair, block, coffer; stonecrafter: crafts (trade goods) |
| drink | plant (farm/gather) → still: drink (+ seeds back from some plants) |
| food | plant / meat / fish → kitchen: easy/fine/lavish meals (2/3/4 ingredients) [mem] |
| metal | ore → smelter (fuel) → bar → forge (anvil, fuel) → weapons/armor/tools |
| fuel | log → wood furnace → charcoal; coal → coke |
| cloth | plant fibre / silk / wool → thread → loom → cloth → clothier |
| leather | butchered animal → skin → tanner → leather → leatherworker |

## 5. Quality, value and wealth

- Quality grades [wiki Item_quality]: standard, −well-crafted−, +finely-crafted+,
  \*superior\*, ≡exceptional≡, ☼masterful☼, artifact. Value multipliers ×1, ×2, ×3, ×4, ×5,
  ×12 [mem] (artifact much higher [mem]). Raw materials, blocks, smoothed tiles have no quality.
  Decorations (encrust with gems, studs, images, dyes, glaze) each add their own quality and value.
  Destroying a masterwork upsets its maker.
- Item value ≈ base value of the item type × material value × quality multiplier, plus
  decorations [mem].
- **Wealth** is the sum of all valuable things in the fortress, split into weapons, armor and
  garb, furniture, other objects, architecture (buildings, constructions, engravings), displayed,
  held/worn; imports and exports are tracked separately; a broker with appraisal and a
  bookkeeper make it visible [wiki Wealth]. Wealth **drives migrant wave size**, attracts
  invaders and forgotten beasts, and gates noble promotion [wiki Wealth, Noble, Forgotten_beast].

## 6. Simulation complexity

- Items are the count that grows without bound (thousands to tens of thousands). Per tick they
  should cost nothing; DF's item cost is **temperature checks and wear** ("the only major
  performance issue items cause" is temperature) [wiki Maximizing_framerate]. Item *stack count*
  matters more than stack size (hauling, stockpiles, pathing).
- Material-availability lists for construction menus get slow in big forts ("forbid materials …
  to reduce the time the game needs to calculate the list of available materials")
  [wiki Maximizing_framerate] — so index items by (type, material) from the start.
- Workshops: one job at a time per workshop; cost is in finding reagents (nearest-first
  search) — again an index problem.

## 7. MVP tiers

| system | tier |
|---|---|
| Items as entities with type + material + location + stack | **MVP** |
| Item index by (type, material) + spatial index | **MVP** (cheap now, impossible to retrofit cheaply) |
| Stockpiles with category settings (category level only) | **MVP** |
| Sub-filters, quality filters, give/take links | soon after |
| Bins, barrels, bags | soon after |
| Workshops: carpenter, mason, still, kitchen | **MVP** |
| Workshop jobs as data (`reactions.ron`) | **MVP** |
| Smelter, forge, fuel chain | soon after |
| Cloth, leather, glass, crafts for trade | later |
| Material raws (`materials.ron`) with value, colour, density, layer type, ore yields | **MVP** (subset of properties) |
| Mechanical properties (yield/fracture) | later (only combat uses them) |
| Quality grades + value | soon after |
| Wealth total | soon after (drives migrants) |

## Sources

- https://dwarffortresswiki.org/index.php/Stockpile
- https://dwarffortresswiki.org/index.php/Workshop
- https://dwarffortresswiki.org/index.php/Reaction
- https://dwarffortresswiki.org/index.php/Material_science
- https://dwarffortresswiki.org/index.php/Item_quality
- https://dwarffortresswiki.org/index.php/Wealth
- https://dwarffortresswiki.org/index.php/Labor
- https://dwarffortresswiki.org/index.php/Miasma
- https://dwarffortresswiki.org/index.php/Maximizing_framerate
- https://stackoverflow.blog/2021/07/28/700000-lines-of-code-20-years-and-one-developer-how-dwarf-fortress-is-built/
