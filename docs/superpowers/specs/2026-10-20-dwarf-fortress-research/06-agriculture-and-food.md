# 06 — Agriculture, food and animals

Part of the Dwarf Fortress research (2026-10-20); start at `00-synthesis.md`. Source tags as in
`01-world-and-embark.md`.

## 1. Calendar and seasons

[wiki Calendar, Time]: 12 months of 28 days (336 days/year), 4 seasons of 3 months; the year
starts on 1 Granite (first day of spring). Fortress tick = 72 in-game seconds: 1,200 ticks/day,
33,600/month, 100,800/season, 403,200/year. Seasons gate crops, caravans (elves spring, humans
summer, dwarves autumn, none in winter), migrant waves and weather; "plants and caravans always
follow the calendar season", whatever the biome's own wet/dry seasons.

## 2. Farming

[wiki Farming]:

- **Farm plot**: a building painted on **soil or muddied stone** (mud comes from water covering
  a tile, i.e. irrigation); costs no materials; one crop per plot per season, chosen per season
  (or fallow).
- Crops are biome- and **light**-gated: surface crops need Light/Above Ground tiles,
  subterranean crops (plump helmet, cave wheat, …) need Dark/Subterranean; a floor over a
  channelled roof makes a tile Light.
- "Keep your farms small – 2×2, up to 4×4 … farms are surprisingly productive."
- Labor "Farming (fields)", skill Planter; yield scales with skill (old 0–5 formula,
  `04-dwarves.md` §2). Fertiliser (potash) increases yield; no crop rotation needed.
- **Seeds** come back from eating, brewing and milling plants, *not* from cooking — so the
  kitchen permission screen matters for not cooking all the seeds.
- Plump helmets (underground) and strawberries (surface) are the canonical first crops; pig
  tails give fibre for cloth.
- Plant gathering from shrubs and trees is the early-game alternative; trees regrow from saplings.

## 3. Food and drink processing

- **Still** (brewing): plant → drink (5 units per plant [mem]) + seeds; alcohol is the main drink
  (`04-dwarves.md` §7).
- **Kitchen** [wiki Kitchen]: easy (2 stacks), fine (3), lavish (4) meals; meals count as every
  ingredient for preferences; a legendary cook can raise value greatly; at least one solid
  ingredient; also renders fat into tallow.
- **Butcher's shop**: animals/corpses → meat, fat, bones, skin, organs (leather and bone chains).
- **Fishery**: raw fish → prepared fish (raw fish rots anywhere) [wiki Miasma].
- **Farmer's workshop / quern / millstone**: process plants (thread, flour, dye, milk/cheese).
- Food storage: food rots unless in a stockpile, depot, wagon, minecart, being carried, or used in a
  reaction [wiki Miasma]; barrels and pots hold food and drink.

## 4. Animals and pastures

[wiki Animal, Pasture]:

- Animals are creatures with taming/usage tokens: pets, livestock, war/hunting animals, mounts,
  pack and draft animals; usage is gated both by creature tokens and by the civilization's
  entity tokens.
- **Pasture** zones hold grazers; grass regrowth depends on biome; overgrazing starves animals.
  Underground moss/fungus works after caverns are breached.
- Animals breed (nest boxes for egg layers), can be butchered, milked, sheared; cats hunt
  vermin; dogs can be trained as war dogs.
- Hunting: hunters with crossbows kill wildlife for meat (labor only via the Hunters work detail
  [wiki Labor]).
- Each tame animal costs line-of-sight checks; caged animals are exempt — a known FPS lever
  [wiki Maximizing_framerate].

## 5. Simulation complexity

- Crops are timers per plot tile (plant → grow N days → harvest), cheap if event-driven.
- Grass regrowth is a per-tile slow process (sparse updates).
- Animals are units: same costs as dwarves minus jobs, plus breeding (population growth is a
  real FPS hazard in DF).

## 6. MVP tiers

| system | tier |
|---|---|
| Calendar (days, months, seasons) driven by ticks | **MVP** |
| Farm plot on soil, one surface + one underground crop, per-season choice | **MVP** |
| Seeds from eating/brewing | **MVP** (otherwise farming dead-ends) |
| Still (plant → drink) | **MVP** |
| Kitchen (easy meals) | soon after (dwarves can eat raw plants in the MVP) |
| Plant gathering | **MVP** (early food without farms) |
| Fertiliser, mud irrigation | later |
| Butchery, fishing, hunting | soon after (with animals) |
| Pastures, grazing, breeding | later |
| Pets, cats vs vermin, war dogs | later |

## Sources

- https://dwarffortresswiki.org/index.php/Calendar
- https://dwarffortresswiki.org/index.php/Time
- https://dwarffortresswiki.org/index.php/Farming
- https://dwarffortresswiki.org/index.php/Kitchen
- https://dwarffortresswiki.org/index.php/Animal
- https://dwarffortresswiki.org/index.php/Pasture
- https://dwarffortresswiki.org/index.php/Miasma
- https://dwarffortresswiki.org/index.php/Labor
- https://dwarffortresswiki.org/index.php/Maximizing_framerate
