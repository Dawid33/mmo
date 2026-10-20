# 01 — World, history and embark

Part of the Dwarf Fortress research (2026-10-20); start at `00-synthesis.md`.
Terrain generation itself (fields, rivers, erosion, geology) is **not** repeated here. It is
covered by `../2026-09-20-df-worldgen-research/` and built as the offline forge
(`just forge-proto`). This file fills the gaps: the world → region → local hierarchy,
history and civilizations, and embark.

Source tags: **[wiki]** = dwarffortresswiki.org page named in the bullet (raw wikitext read
2026-10-20; pages are mostly v50 with some v0.47 text left in); **[mem]** = from memory of
the game, not re-verified this session; **[unverified]** = plausible but not confirmed.

## 1. The world → region → local hierarchy

What the player sees: a world map of coarse tiles; zooming in on one shows a 16×16 grid of
"local" squares; the player drags an embark rectangle over those squares; each square becomes
48×48 playable tiles.

| level | unit | size | source |
|---|---|---|---|
| world map | region tile ("world tile") | Pocket 17², Smaller 33², Small 65², Medium 129², Large 257² tiles | [wiki World_generation] |
| region tile | 16×16 local squares ("embark squares", one *mid-level map block* each) | — | [wiki World_generation, Embark] |
| local square | 48×48 tiles, full z-range | default embark 4×4 squares = **192×192 tiles** | [wiki Embark, Maximizing_framerate] |
| z | one tile tall | ~50 levels of land on average, +15 of sky above the highest point, caverns below; range 6 → 600+; "0 corresponds to sea level" | [wiki Z-level] |

One region tile is therefore 16 × 48 = 768 tiles across; a Medium world is 129 × 768 ≈ 99k
tiles across. Worldgen runs on the region-tile grid and refines down; the local map is
generated from the region tile's fields when the player embarks (see the worldgen research,
`01-pipeline-and-fields.md` §world→local refinement).

Embark size trades content against speed: "Reducing the size of your embark site from the
default 4×4 squares to 3×3 or even 2×2 will have an enormous impact on FPS" [wiki
Maximizing_framerate]. 1×1 is called too restrictive [wiki Embark]. Each local square can have
its own biome and its own aquifer [wiki Embark, Aquifer].

Named *regions* (subregions) are contiguous runs of world tiles with similar biome and
alignment (good/neutral/evil); ≥300 elevation is mountain, <100 is ocean [wiki Region].

## 2. History, civilizations, sites, legends

What it does: after terrain, worldgen **simulates history** for up to N years. Civilizations
(dwarves, humans, elves, goblins, kobolds, plus animal people and others) found sites, go to war,
produce historical figures (each one a full creature with skills, relationships, beliefs),
artifacts, books and megabeasts with kill lists. Legends mode browses the result
[wiki Legends, Megabeast]. The history makes the world feel inhabited: migrants arrive from your
civilization's other sites, caravans come from neighbours, sieges come from enemies that can
physically reach you ("armies need to physically move to the location", 30-tile radius of
interaction) [wiki Siege], and engravings depict historical events.

Rules that touch fortress mode directly:

- **Origin civilization** is chosen at embark; its population and site count drive
  immigration and caravans. Immigration stops after the second winter if the civilization is
  too small or unreachable — "select a home civilization with at least two dwarven sites on the
  map on the same land mass" [wiki Embark].
- **Neighbours** shown at embark decide who trades and who sieges [wiki Embark, Siege].
- Sieges and caravans are **world-map agents** (armies, merchants) that travel to the site; an
  island embark never gets sieges [wiki Siege].
- Larger worlds and civilizations cost fortress FPS because world events keep being processed
  in the background [wiki Maximizing_framerate].

Simulation complexity: very high to do properly (historical figures are full agents, at world
scale, over centuries). DF does it once, offline, before play — which matches our offline forge.

**Data model (raws):** civilizations are `ENTITY` raws (`entity_default.txt`): which creature,
which items/materials they use, ethics, positions (nobles), site types, and which `REACTION`s
they may perform [wiki Reaction: "reactions … must be added to a civilization's entity file"].

## 3. Embark preparation

[wiki Embark] unless tagged:

- **7 dwarves**, always.
- **Play Now!** gives preset dwarves (Miner, Woodworker, Stoneworker, Metalcrafter,
  Fisherdwarf, Planter, Expedition Leader) and a preset loadout.
- **Prepare Carefully**: default budget **1,504 points**, skills cost no points but each dwarf
  gets 10 skill ranks with no skill above 5; only 544 points are free because the rest pre-buys
  basic gear. Advanced worldgen can raise the budget to 10,000.
- Default supplies: picks, axes, an anvil, a wheelbarrow, a stepladder, alcohol, seeds, food,
  cloth and rope materials, splints and crutches. Starting animals: dogs, cats, domestic animals;
  two pack animals free when preparing carefully.
- A **wagon** brings the supplies; it can be deconstructed for wood [mem].
- Before confirming, the game warns of notable dangers (evil biome, savage wildlife, aquifer)
  [wiki Embark].

## 4. MVP tiers

| system | tier | notes |
|---|---|---|
| World map view + choose an embark rectangle | **MVP** | Our forge already produces the world; we need a world-map screen and an embark picker. |
| World → region → local hierarchy | **MVP** (as a coordinate convention) | Defines what an embark *is* in our units (`engine-mapping.md` §4). |
| Local map generation from the world artifact | **MVP** | `generate_region` over the embark rectangle, then voxels → tile map. |
| Play Now embark (fixed 7 dwarves + fixed loadout) | **MVP** | Data file, no UI. |
| Prepare Carefully (points, skills, items) | soon after | Pure UI + data; needs the item and skill raws first. |
| Origin civilization (as a data stub) | soon after | One civ with a population number drives migrants/caravans. |
| Neighbouring civs, who can reach you | later | Needs a world-level site layer. |
| History simulation, historical figures, legends | **later** | Big, offline, and orthogonal to the fortress loop. Keep the world artifact format extensible (a `sites`/`entities` section) so it can be added without breaking saves. |
| Megabeasts / forgotten beasts as named figures | later | Generated with history. |

Why history is "later" and not "never": DF's identity is the *stories*, and the stories come
from history. But a playable fortress needs none of it; the coupling is a handful of numbers
(civ population for migrants, a neighbour list for caravans/sieges). Stub those numbers in the
MVP and grow the history layer offline in the forge, where the budget is unlimited.

## Sources

- https://dwarffortresswiki.org/index.php/World_generation
- https://dwarffortresswiki.org/index.php/Embark
- https://dwarffortresswiki.org/index.php/Region
- https://dwarffortresswiki.org/index.php/Z-level
- https://dwarffortresswiki.org/index.php/Legends
- https://dwarffortresswiki.org/index.php/Siege
- https://dwarffortresswiki.org/index.php/Megabeast
- https://dwarffortresswiki.org/index.php/Maximizing_framerate
- Prior repo research: `docs/superpowers/specs/2026-09-20-df-worldgen-research/00-synthesis.md`
