# 07 — Rooms, zones, buildings, furniture, mechanisms

Part of the Dwarf Fortress research (2026-10-20); start at `00-synthesis.md`. Source tags as in
`01-world-and-embark.md`.

## 1. Zones and rooms

[wiki Zone]:

- A **zone** is a painted area with a purpose: meeting area, bedroom, dining hall, office, tomb,
  barracks, archery range, hospital, pen/pasture, pit/pond, water source, fishing, gather
  fruit, garbage dump, sand/clay collection, … Zones can be placed on any revealed tile and
  can overlap (overlap lowers value).
- **Locations** upgrade meeting areas into taverns, temples, libraries, guildhalls, hospitals.
  Migrants gather at a meeting area; the embark wagon is the first one.
- **Rooms by furniture** ("Multi" mode): a room is grown from a furniture building (bed, table,
  chair, coffin, throne) out to the walls, including diagonals, and needs walls all round plus a
  door, stair or ramp.
- Rooms are **assigned** to a dwarf (bedroom, office, tomb) or communal (dining hall).

## 2. Room value and thoughts

The room's value is the value of its furniture, smoothing/engraving, and constructed walls/floors
inside it [mem]; personal rooms are ranked [wiki Zone, "Quality and value"]:

| value | bedroom | dining room | office | grave |
|---|---|---|---|---|
| 0 | Meager Quarters | Meager Dining Room | Meager Office | Grave |
| 100 | Modest Quarters | Modest Dining Room | Modest Office | Servant's Burial Chamber |
| 250 | Quarters | Dining Room | Office | Burial Chamber |
| 500 | Decent Quarters | Decent Dining Room | Decent Office | Tomb |
| 1000 | Fine Quarters | Fine Dining Room | Splendid Office | Fine Tomb |
| 1500 | Great Bedroom | Great Dining Room | Throne Room | Mausoleum |
| 2500 | Grand Bedroom | Grand Dining Room | Opulent Throne Room | Grand Mausoleum |
| 10000 | Royal Bedroom | Royal Dining Room | Royal Throne Room | Royal Mausoleum |

Room rank produces thoughts ("dined in a legendary dining room", "slept in a … bedroom"), and
nobles demand minimum ranks (`09-…` §2). Furniture quality gives "admired a fine X" thoughts
[wiki Furniture].

## 3. Buildings and furniture

[wiki Furniture] + [mem]:

- "Furniture" is anything that goes in a furniture stockpile; built furniture becomes a
  **building** on a tile: bed, table, chair, door, cabinet, coffer, armor/weapon rack, statue,
  coffin, throne, well, hatch, grate, window, floodgate, bars, cage, … Buildings block or
  allow movement and fluids per type (door: passable to dwarves when unlocked, blocks fluids when
  closed; grate: fluids pass, creatures don't; hatch: floor door).
- Workshops are multi-tile buildings (`05-…` §4); the **trade depot** is 5×5 and must be
  reachable by wagons via ramps [wiki Ramp].
- Building placement is a job: designate site → haul materials → build (Architecture/Mechanics
  labors) [mem].

## 4. Mechanisms, levers, traps

[wiki Mechanism, Lever, Trap, Floodgate, Well, Screw_pump]:

- **Mechanisms** (stone or metal) are the universal machine part: every trap, lever, pressure
  plate and gear assembly needs one; linking a device to a lever costs two.
- **Levers** toggle linked devices: doors, hatches, floodgates and grates open/close; supports
  collapse (cave-in on purpose); bridges raise/lower or retract/extend; gear assemblies engage.
  A dwarf with lever operation has to walk to it — "the delay in pulling the lever can result in
  Fun". **Pressure plates** trigger on creatures or fluids.
- **Traps** on a floor tile: stone-fall, weapon, cage (captures), upright spikes; triggered by
  hostiles (and unconscious creatures), not by conscious dwarves, guests or tame animals;
  invaders may recognise traps.
- **Floodgates** behave as walls when closed; **wells** draw water from open water below
  (freeze hazards); **screw pumps** lift fluid one level from the tile below the input to the
  output tile, by a dwarf or by power (water wheels, windmills, axles, gears).
- Mechanical power networks (water wheel → axles → gears → pumps/millstones) are a separate
  graph sim [mem].

## 5. Tile attributes: inside/outside, light/dark, above/below

[wiki Tile_attributes]: every tile is Outside/Inside, Light/Dark, Above Ground/Subterranean;
the fortress site starts "Inside Dark Subterranean" underground; roofs and constructed floors
change these. They gate farming crops (§`06`), miasma (only subterranean, `08`), rain thoughts,
cave adaptation and freezing.

## 6. Simulation complexity

- Room detection is a flood fill bounded by walls and doors, run when furniture or walls change.
- Room value is a sum over the room's tiles and buildings — recomputed on change, not per tick.
- Doors and hatches change the connectivity map — DF's source of "brief lag spikes … as the
  game's connections map needs updating", worst with devices on repeaters
  [wiki Maximizing_framerate]. Doors (in numbers) also cost line-of-sight work.
- Power networks and fluids interact (pumps); defer both.

## 7. MVP tiers

| system | tier |
|---|---|
| Buildings on tiles: bed, table, chair, door; placement as a job | **MVP** |
| Zones: meeting area, bedroom (from bed), dining hall (from table) | **MVP** |
| Room detection by flood fill, room value + rank thoughts | soon after |
| Trade depot | soon after (with caravans) |
| Hospital, barracks, tomb, other zone types | later |
| Locations (tavern, temple, library) | later |
| Mechanisms, levers, doors/hatches/bridges on levers | later |
| Traps (cage, stone-fall, weapon) | soon after the first threat |
| Floodgates, wells, pumps, power | later (with fluids) |
| Tile attributes (inside/outside, light/dark, above/below) | **MVP** (computed at embark, updated on dig/build; farming and rain need them) |

## Sources

- https://dwarffortresswiki.org/index.php/Zone
- https://dwarffortresswiki.org/index.php/Furniture
- https://dwarffortresswiki.org/index.php/Mechanism
- https://dwarffortresswiki.org/index.php/Lever
- https://dwarffortresswiki.org/index.php/Trap
- https://dwarffortresswiki.org/index.php/Floodgate
- https://dwarffortresswiki.org/index.php/Well
- https://dwarffortresswiki.org/index.php/Screw_pump
- https://dwarffortresswiki.org/index.php/Tile_attributes
- https://dwarffortresswiki.org/index.php/Ramp
- https://dwarffortresswiki.org/index.php/Maximizing_framerate
