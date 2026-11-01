# 14 — History, civilizations, territory and religion

Part of the Dwarf Fortress research; start at `00-synthesis.md`. Added 2026-11-01 (subagent
research from the DF wiki raw pages, Bay 12 devlogs 2012–2019, the 2008 Tarn Adams interview, and
the local vanilla raws `entity_default.txt` v0.47.04). This deepens `01-world-and-embark.md` §2.
Tags: [wiki:Page], [devlog:YYYY-MM], [raws], [int2008], [mem] = unverified. DF is closed
source: the documented behaviour is solid; the internals are mostly unknown (listed at the end).

Tarn's model: "a giant zero-player strategy game going on with somewhat loose turn rules and bad
AI (but thousands of agents), and history is just a record of that." [int2008]

## 1. Order and parameters
- After terrain: Placing civilizations → cave civs/pops → beasts, megabeasts → good/evil → caves →
  **prehistory (history sim)** → finalizing civ materials, art, uniforms, sites
  [wiki:Advanced_world_generation].
- **Civ count** `TOTAL_CIV_NUMBER`: medium world presets 8 / 13 / 24 / 44 / 80; large up to 160.
  The races are placed in equal numbers to the quota, and a race lacking prerequisites is skipped
  (kobolds need caves, goblins need at least 2 demon types).
- **`SITE_CAP`**: medium 260 … 2000. It counts only sites civs create later; it is the dominant
  history cost ("increase the duration of history generation by a huge amount").
- **`TOTAL_CIV_POPULATION`**: a soft cap on *living historical figures*, split by share of sites.
  Ordinary population is uncapped and abstract.
- **`END_YEAR`**: 2 … 10,000. The DF2014 presets were 5/125/250/550/1050; the v50 default is 100.
  Generation can be stopped at any year ≥ 2. Long histories tend to kill civs off.
- **Beasts**: medium/medium = 18 megabeasts, 37 semi-megabeasts, 9 titans, 243 forgotten beasts.
  `BEAST_END_YEAR` stops history once enough powers are dead.
- **Rejection**: `PLAYABLE_CIVILIZATION_REQUIRED` (a dwarf civ must place), farming resources in the
  start area, too few mountains for dwarven origins. Elves need temp ≥ 10, humans ≥ 0, dwarves
  non-aligned biomes [wiki:World_rejection, wiki:Entity_token].

## 2. Placement and spread
- Many civs per race. Starts claim `START_BIOME` / `EXCLUSIVE_START_BIOME` tiles and block each
  other; an entity with no free start space is skipped [wiki:Entity_token].
- **`BIOME_SUPPORT:<biome>:<w>`**: relative territory spread speed per biome. Civs spread over
  biomes they can't build in (humans: ocean 12, river 4, grass 3). **`SETTLEMENT_BIOME`**: where
  they may found new sites inside their territory.
- Vanilla [raws]:

  | Race | Start | Spread weights | Site type |
  |---|---|---|---|
  | Dwarves | exclusive mountain | mountain 3, others 1 | `CAVE_DETAILED` |
  | Elves | any forest | forest 3, lake/river 1 | `TREE_CITY` |
  | Humans | grass/savanna/shrub | **ocean 12**, river 4 | `CITY` |
  | Goblins | any land | all 1 | `DARK_FORTRESS` |
  | Kobolds | mountain | — | `CAVE` |

- **Site types**:
  - Dwarves: fortresses at range edges, hillocks off-mountain, mountain halls in the interior,
    linked by roads and tunnels.
  - Humans: hamlets that grow into towns if no market is near; castles.
  - Elves: forest retreats.
  - Goblins: a dark fortress on a demon's spire, plus dark pits.
  - Kobolds: hidden caves.
- Sites sit only on tiles next to non-mountain land [wiki:Site].
- **New sites are founded by settler armies that physically move.**
- Expansion is capped by `SITE_CAP`, `MAX_POP_NUMBER` (historical figures per civ) and
  `MAX_SITE_POP_NUMBER` (120).
- The founding-target scoring is **undocumented**.

## 3. Territory and neighbours
- **No documented border polygons.**
  - There is an internal territory spread by `BIOME_SUPPORT`.
  - Embark shows "exactly which tiles are claimed" by a civ.
  - The fortress Holdings map colours sites by relation: own, war, peace, vassal, tributary,
    occupied [wiki:Embark, wiki:Civilization].
  - Reading this as "sites + per-site tile claims + tribute links" is inference.
- **Site-level politics (0.40+)**: every site has its own site government, and neighbouring sites
  squabble (livestock, water, right-of-way) into raids and site-to-site tribute [devlog:2013-08].
- **Neighbours = reachability**: oceans and mountain rings cut civs off. Sieges need armies that
  can physically reach you (30-tile radius; towers 10/20 — the wiki is inconsistent).
- **World constructions**: road, tunnel, bridge, wall per race [raws].

## 4. Religion
- **Deities from spheres** (130 spheres with parent/child, friend and preclude lists): a deity has
  1–2 base spheres, then friends of those, never precluded or parent/child [wiki:Sphere, Toady].
- **Civ religion**:
  - `RELIGION:PANTHEON` (dwarves, humans) or `REGIONAL_FORCE` (elves: nature plus a terrain sphere).
  - Goblins worship their demon ruler; kobolds worship nothing.
  - `RELIGION_SPHERE` forces spheres (dwarves: fortresses, jewels, metals, minerals, mountains,
    wealth).
  - `SPHERE_ALIGNMENT` does *not* bias pantheon generation.
  - Pantheon size is undocumented (~8 in v0.44, more in v0.47).
- **Organized religions (v0.47)**:
  - Prophets arise where a god is tied to the place.
  - They found monastic orders (monasteries as sites) or temple religions: shrine → local priest →
    temples → high priest → holy city with two temples.
  - Religions cross civ lines and spread along trade routes.
  - A medium 150-year world had 90 religions and 39 orders [devlog:2018-12].
- **Gods act**:
  - Death gods give necromancy slabs.
  - Profaning a temple curses the offender into a vampire or werebeast.
  - A deity raises a vaulted demon, which goes to its spire; goblins found their capital there with
    the demon ruling.
  - Without religions there are no angels, vampires or necromancers.

## 5. Entity raw data model (what drives worldgen)
- **Control**: `SITE_CONTROLLABLE` (fortress playable), `CREATURE`, `MAX_STARTING_CIV_NUMBER`,
  `MAX_POP_NUMBER`, `MAX_SITE_POP_NUMBER`.
- **Placement**: START/EXCLUSIVE_START/SETTLEMENT_BIOME, BIOME_SUPPORT, DEFAULT_SITE_TYPE,
  LIKES/TOLERATES_SITE, WORLD_CONSTRUCTION.
- **Culture**:
  - `ETHIC:<behaviour>:<reaction>`: opposed extremes go to war.
  - `VALUE:<v>:-50..50`: thresholds gate structures, e.g. KNOWLEDGE ≥ 15 → libraries;
    COOPERATION + CRAFTSMANSHIP ≥ 15 → guilds.
  - `VARIABLE_VALUE` randomizes values per civ.
- **Positions**: `POSITION` (succession, elected, land holders), `VARIABLE_POSITIONS` (laws make
  offices). Armies come from positions with military goals [wiki:Army].
- **Resources**: PERMITTED_JOB / REACTION; COMMON_DOMESTIC_*; USE_* local creatures and plants
  from about a 7×7-square neighbourhood; farming / orchard / wood.
- **Behaviour**:
  - `ACTIVE_SEASON`: dwarves autumn, elves spring, humans summer, goblins all.
  - BABYSNATCHER, ITEM_THIEF, AMBUSHER, SIEGER, SKULKING, BANDITRY:%, MERCHANT_NOBILITY.
  - WANDERER / SCOUT / MERCENARY / BEAST_HUNTER.
  - PROGRESS_TRIGGER_* decide when a civ comes to a fort.

## 6. History simulation
- **Time step: unknown.** Events carry year + tick. Everything off-screen is abstract:
  - armies at four resolutions;
  - non-historical populations as counts, promoted to figures on demand [devlog:2013-12];
  - sabotage and intrigue as abstract numbers "because so many parts need to move quickly";
  - a hidden industry/trade simulation.
- **Events**:
  - Populations: births, marriages, deaths, migration, refugees → bandits, relationships.
  - Sites: founding, conquest, sacking, reclaiming, raids, tribute, insurrection, usurpation.
  - Wars: per civ, inherited by sites; causes are ethics clashes and baby-snatching. Battle
    resolution is undocumented; 2013: power ≈ population after tribute.
  - Megabeast lairs and attacks.
  - Artifacts: moods, heroic gear, relics, heirlooms, theft and quests.
  - Books, libraries and scholars.
  - Organizations: guilds, merchant companies, mercenaries, troupes, criminal groups.
  - Succession and variable positions.
  - Villain plots (0.47).
  - Necromancers, vampires, werebeasts.
- **Ages**:
  - Myth / Legends / Heroes, by the share of the starting powers still alive.
  - Then Three / Two / One Powers.
  - Then Age of [Race] / Golden / Twilight / Fairy Tales, by the mundane share of creatures.

## 7. Historical figures
- A small explicit share of the population: nobles and entourage, all beasts and demons, villains
  and their families, civ enemies. In play, every fortress citizen becomes one.
- They carry family, positions, kill lists, an event chronology, skills, personality facets
  (0–100) and values (−50..50, biased by the civ), a goal, worship, relationships, artifact
  claims and knowledge.
- **Scale**: a small world after 100 years ≈ 4,600 figures [devlog:2018-09].

## 8. What fortress mode reads
- **Embark**:
  - Pick a living home civ (population, sites, monarch, claimed tiles).
  - Neighbours list (hostile / war).
  - Embark goods come from the nearest capital's materials.
  - At least two of your sites on the landmass, or migration stops.
- **Migrants**: drawn from the civ's real populations; two waves hardcoded, then scaled by
  reported wealth.
- **Caravans and the liaison**: by `ACTIVE_SEASON` and progress triggers (population 20/50/80…,
  wealth 5k–300k).
- **Diplomacy**: no contact / peace / alliance / skirmish / war.
- **Sieges**: real armies on the world map, from fortress population 80. Megabeasts at 100k wealth
  and 80 population.
- **Nobles**: land-holder promotions through the liaison; the monarch moves in when the fort
  becomes the capital.
- **Other**: artifact claims, questers, missions; worship and temple requests; engravings of
  history.
- **World activation (0.40+)**: the world keeps simulating during play (births, deaths, founding,
  succession, invasions, banditry). Each session starts by simulating about two weeks of calendar
  time. Some travellers teleport, armies move physically [wiki:World_activities].

## 9. Cost
- 2008: worldgen used over 1.2 GB RAM [int2008].
- 2014: the session-start two-week simulation on a 200-year medium world took 23 s, optimised
  to 10 s [devlog:2014-07].
- Big civ populations cause a permanent FPS drop. Fortress unit lists shrink migrant waves from
  1,000 units and stop them at 3,000.
- Seed reproducibility of history is fragile (many "same seed, different results" bugs).

## Unknown publicly
The history-sim step and scheduler; the site-founding score; the territory data model beyond
claimed tiles; worldgen battle resolution; pantheon size and sphere draws; the exact
neighbour-reachability rule.

## Sources
The DF wiki (raw): World_generation, Advanced_world_generation, DF2014:World_generation,
World_rejection, Civilization, Entity_token, Site, Historical_figure, Deity, Religion, Sphere,
Force, Calendar, Legends, World_activities, Embark, Siege, Immigration, Caravan, Outpost_liaison,
Noble, Monarch, Army, War, Diplomacy, Tribute, Variable_positions, Intrigue, Villain, Guild,
Legendary_artifact, Megabeast, Necromancer, Vampire, Demon, Angel, Vault, Goblin, Town, Hamlet,
Mountain_halls, Dark_fortress, Kobold, Visitor, Art, Personality_facet/value/goal,
Maximizing_framerate. Bay 12 devlogs dev_2012 … dev_2019. Tarn Adams 2008 Game Developer
interview. Bug tracker 11278. Local raws `entity_default.txt` (v0.47.04).
