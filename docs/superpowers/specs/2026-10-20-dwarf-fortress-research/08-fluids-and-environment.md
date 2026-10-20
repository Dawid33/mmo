# 08 — Fluids and environment: water, magma, pressure, temperature, weather, light, miasma

Part of the Dwarf Fortress research (2026-10-20); start at `00-synthesis.md`. Source tags as in
`01-world-and-embark.md`. Compared against our own decision in
`../2026-09-22-voxel-water-efficiency/00-synthesis.md` (deterministic in-sim cellular water,
finite volume, water bodies + frontier CA).

## 1. DF's fluid model

[wiki Water, Flow, Pressure, Magma]:

- Two fluids, **water** and **magma**; each tile holds a **level 0–7** (7 = full tile).
  Walking is safe to depth 3; at 4+ dwarves cancel jobs ("dangerous terrain") and start learning
  to swim; at 7 non-swimmers drown.
- **Basic motion**: fall straight down if possible, else spread sideways, including diagonally
  on the same level, "never move sideways and down at the same time", never up.
- **Pressure** ("teleportation") applies to water falling onto full 7/7 water, to river/brook
  sources, and to screw pumps (water and magma). The moved unit traces a path through full tiles
  — N, S, E, W, down, up, never diagonal — and appears at the nearest open tile **no higher than
  the level of the first 7/7 tile on the path**. Lakes, oceans and the magma sea refilling from the
  map edge do *not* exert pressure. Magma never exerts pressure by falling. Pressure is "a lazy
  model": the nearest outlet fills first.
- Consequences players exploit: U-bends fill to the source level minus one (undammed river) or
  to the source level (dammed river / sea-blocked river); diagonal gaps break pressure
  ("pressure regulators"); pumps isolate pressure.
- **Flow** state (a tile recently moved into) pushes items and powers water wheels; rivers have
  permanent "natural flow".
- **Evaporation** at depth 1/7 (rate unknown, faster when isolated, not on top of 7/7), more in
  hot climates; **freezing** of outside water is instant in cold seasons (walls of ice at ≥4/7,
  floors below; thawing an ice wall always gives 7/7); **mud** is left wherever water covered a
  tile (needed to farm on stone).
- **Fluid displacement by cave-in**: a falling natural wall teleports displaced fluid upward
  ("magma piston") — called a likely bug on the wiki.
- **Aquifers** are infinite leaks (`02-…` §4); river/brook sources and map-edge oceans are
  infinite; DF has no water conservation at all.

## 2. Comparison with our water decision

| | DF | our decision (2026-09-22) |
|---|---|---|
| quantisation | 7 levels per tile | `u16` amount per 1 m cell (finer); bodies with integer level |
| motion | fall, then spread incl. diagonal; per-tile CA | frontier CA (down first, then sideways) every 4 ticks |
| standing water | still a per-tile CA (slow levelling) | **water bodies**: one level for a whole lake, O(openings) drain |
| pressure | teleport along full tiles, capped at source level | communicating vessels via body merge at a common level (outlet emits by head) |
| volume | not conserved (infinite sources, evaporation, aquifers) | **finite everywhere**; sea is a huge finite body; rivers asleep until disturbed |
| determinism | n/a | bit-exact integer code, canonical order, native = wasm |

Our model already reproduces DF's *observable* behaviours that matter for fortress play —
falls, spreading, filling channels, flooding a fortress through a breach, U-bend levelling — and
is cheaper for big standing water. Gaps to close for faithfulness:

1. **Depth semantics**: map our amount to DF's 0–7 for gameplay rules (walkable ≤3/7, swim
   ≥4/7, drown at 7/7) — a pure function, no model change.
2. **Pressure exploits**: DF players expect diagonal gaps to stop pressure and pumps to isolate
   it. Bodies-at-a-common-level give communicating vessels but not DF's "minus one level when the
   river can drain" or diagonal regulators. Decide on purpose whether to mimic them; recommend
   *not* copying DF's quirks, but keep "diagonal gaps don't carry water" since our CA can choose
   orthogonal-only spreading.
3. **Infinite sources**: aquifers and brooks are infinite in DF. Our finite rule needs a supply
   story (spring = a source with a budget from the world-level water ledger, or an explicit
   exception for aquifers). Open question.
4. **Magma**: a second fluid with no pressure, cooling to obsidian on contact with water. Same
   CA, different rules; needs temperature (below).

## 3. Temperature

[wiki Temperature]:

- Temperature is a 16-bit unsigned integer in "degrees Urist" (°U = °F + 9968): water freezes at
  10000, boils at 10180, underground is 10015, dwarf body 10067, magma-adjacent stone 10075.
  Fixed-point integers, no floats — deterministic by construction.
- Tiles, items and creatures carry temperatures; materials have specific heat, ignition, melting,
  boiling points (`05-…` §3). Fire spreads by ignition; magma melts items below their melting
  point.
- **Cost**: "Temperature calculations place a significant load on the processor"; disabling it
  is reported to double FPS; items are checked for temperature, which is "the only major
  performance issue items cause" [wiki Maximizing_framerate].

## 4. Weather, seasons, light, miasma

- **Weather** [wiki Weather]: rain (minor unhappy thought outside, cleans contaminants, refills
  murky pools), snow, freezing; evil weather (blood rain, syndrome clouds) in evil biomes.
  Clouds and fog matter only in adventure mode.
- **Light / dark, inside / outside** tile attributes (`07-…` §5): drive crops, cave
  adaptation, miasma, freezing.
- **Miasma** [wiki Miasma]: rotting corpses/food produce a gas that spreads N/S/E/W/up/down
  (not diagonally), through open doors/grates, only in subterranean tiles; gives bad thoughts.
  Gases (miasma, steam, smoke) rise; DF calls these "flows" too.
- **Contaminants** (blood, vomit, mud) accumulate on tiles and creatures; cleaning is a job;
  buggy accumulation is an FPS sink in old forts [wiki Maximizing_framerate].

## 5. Simulation complexity

| system | per-tick cost in DF | notes for us |
|---|---|---|
| fluids | proportional to flowing tiles; DFHack `flows` counts "map blocks with flowing liquids, which slow the game down" | our bodies+frontier design makes settled water free |
| temperature | every tile/item near a heat source, every tick | design as event-driven: items have a temperature only when not at ambient; heat sources (magma, fire) mark a bounded dirty region |
| weather | per outside tile when it rains/snows | sparse; can be a per-region state + lazy tile effects |
| miasma | gas tiles CA | small; bounded by source count |

## 6. MVP tiers

| system | tier |
|---|---|
| Static water (forge rivers/lakes/sea) as terrain dwarves can't walk through | **MVP** (already in worldgen) |
| Drink from a water-source zone | **MVP** (fallback drink) |
| Flowing water CA + bodies (our planned design) with DF depth rules | later — but it is the most "DF" thing after digging, and floods are the canonical disaster; plan it right after the MVP |
| Freezing / thawing, evaporation, mud | later |
| Magma, obsidian | later (with caverns) |
| Temperature | later (event-driven design) |
| Weather (rain thoughts, snow) | later |
| Miasma | soon after (corpses are the first consequence of death) |
| Aquifers | later (needs fluids + a supply rule) |

## Sources

- https://dwarffortresswiki.org/index.php/Water
- https://dwarffortresswiki.org/index.php/Flow
- https://dwarffortresswiki.org/index.php/Pressure
- https://dwarffortresswiki.org/index.php/Magma
- https://dwarffortresswiki.org/index.php/Temperature
- https://dwarffortresswiki.org/index.php/Weather
- https://dwarffortresswiki.org/index.php/Miasma
- https://dwarffortresswiki.org/index.php/Tile_attributes
- https://dwarffortresswiki.org/index.php/Aquifer
- https://dwarffortresswiki.org/index.php/Maximizing_framerate
- Prior repo research: `docs/superpowers/specs/2026-09-22-voxel-water-efficiency/00-synthesis.md`
