# 03 — Hub-and-expedition structures (space games, ARPG map devices, generated history)

Research date: 2026-09-20. Scope: shipped multiplayer games whose worlds are large generated
places separated by a loading transition, read for what a **non-violent voxel MMO with offline-
generated 8–16 km super regions** can borrow. Claims carry inline sources; anything stated from
general knowledge that I could not confirm against a fetched page is tagged **[unverified]**.
Fetch gaps are listed at the end.

---

## 1. No Man's Sky (Hello Games, 2016–) — the main reference

**Structure.** One shared, deterministic, effectively infinite universe ("18 quintillion"
planets — [Vice](https://www.vice.com/en/article/nz7d8q/no-mans-sky-review)); each planet is a
seamless, full-size generated world, generated on the client from a seed — nothing is stored
except player deltas (discovery names, bases, terrain edits). Sessions are small: 16 players in
the Space Anomaly, 8 (console) / 32 (PC) elsewhere
([Beyond notes](https://www.nomanssky.com/beyond-update/)). Multiplayer is mostly "ambient":
you see uploaded bases and names far more often than live people.

**Transition.** Warp between systems is a hyperspace tunnel masking the load; planet landing is
seamless. Travel is gated by fuel (warp cells), drive upgrades (star colour classes) and
*knowledge*: 16 portal glyphs form a 12-glyph address for any planet, shareable out of game
([portal address wiki](https://nomanssky.miraheze.org/wiki/Portal_address),
[decoder](https://nmsportals.github.io/)). Inventory, ship and freighter all travel with you.

**Purpose.** Non-violent core: scanning/cataloguing flora, fauna and minerals; naming and
uploading discoveries; base building; farming and industrial loops; ship/multitool "hunting"
(finding a rare generated look at known coordinates); archaeology; cooking; being Overseer of a
generated settlement — naming it, choosing buildings, settling disputes
([Frontiers](https://massivelyop.com/2021/09/01/no-mans-skys-frontiers-sets-you-up-as-overseer-of-your-very-own-space-town/)).
Violent parts (sentinels, pirates) are skippable in creative/relaxed modes and are not what
retains people. Nine years on it set a post-launch Steam record of 98,285 concurrents with the
Voyagers update ([ScreenRant](https://screenrant.com/no-mans-sky-highest-player-numbers-2025/)).

**Making variety matter.** This is the canonical "oatmeal" case. Kate Compton:
"I can easily generate 10,000 bowls of plain oatmeal… mathematically speaking they will all be
completely unique. But the user will likely just see a lot of oatmeal. Perceptual uniqueness is
the real metric" ([Compton](https://galaxykate0.tumblr.com/post/139774965871/so-you-want-to-build-a-generator)).
She separates *perceptual differentiation* (not identical — easy) from *perceptual uniqueness*
(memorable character — hard), and notes humans "like perceiving evidence of process and forces,
like the pushed up soil at the base of a tree". NMS planets differentiate but rarely become
unique: one biome per planet, no rivers, no erosion, no history. Hello Games regenerated the
universe repeatedly to fix this (most recently
[Worlds Part I, 2024](https://www.nomanssky.com/2024/07/no-mans-sky-worlds-part-i/)); players
still report "nothing that made me sit up and take notice"
([Steam thread](https://steamcommunity.com/app/275850/discussions/0/4526764179299457695/)).
What actually creates value: **first-discovery naming** stored server-side, un-renamable by
others ([naming guide](https://www.gamepressure.com/nomanssky/how-to-name-planet-or-species/z290b7));
**rarity** (exotic ship models, paradise planets) turned into a coordinate-trading economy — the
community Portal Repository has 3,000+ entries; and **other players' builds**.

**Social structure.**
- *The Anomaly/Nexus*: a hub that "exists across all systems and galaxies" — summon it
  anywhere, walk in, see 15 others, take group missions, and use a teleporter that reaches "the
  base of any other player currently on the Anomaly with them" plus curated **featured bases**
  ([Beyond](https://www.nomanssky.com/beyond-update/)). The hub is not a place in the world; it
  is a lobby you can reach from every place.
- *Expeditions*: seasonal, time-limited saves. "Every player beginning an expedition starts on
  the same planet"; milestones are grouped into phases and "can be undertaken in any order";
  **Rendezvous Points** are "specific points in the galaxy at which all players are required to
  check in"; rewards are account-wide cosmetics; afterwards the save converts to a normal one.
  Weekend community missions pool progress across all platforms
  ([Expeditions](https://www.nomanssky.com/expeditions-update/)). Effect: for a few weeks an
  infinite universe collapses to one route, and the rendezvous planets fill with hundreds of
  bases and message beacons.
- *Player civilisations*: the **Galactic Hub** (founded 2016) simply chose a region and told
  everyone to move there. It claims "over 11,000 bases… uploaded in Galactic Hub space", 15+
  colonies, a yearly census, an exobiology corps and ship-hunter catalogues, run via subreddit,
  Discord and wiki. Hello Games canonised it (in-game "Galactic Hub Cartographers" missions, an
  emblem decoration in 2018, a banner in 2020)
  ([Hub wiki](https://nmsgalactichub.miraheze.org/wiki/Galactic_Hub_Project)).

**Worked / failed.** Worked: free, sustained updates; the summon-anywhere hub; expeditions as a
way to concentrate population; naming; canonising player institutions. Failed: launch
over-promise; planets without process or history; universe regenerations that altered biomes
under existing bases ([Origins note](https://www.nomanssky.com/origins-update/) says terrain was
preserved, but players [reported otherwise](https://steamcommunity.com/app/275850/discussions/0/4511002214536785386/));
the Hub has relocated more than once, most recently April 2025 — the earlier moves being caused
by universe resets is **[unverified]**. Everything civilisations needed (census, maps, borders,
events) had to be built outside the game.

## 2. Elite Dangerous (Frontier, 2014–)

**Structure.** A 1:1 Milky Way, ~400 billion systems from the Stellar Forge generator
([wiki](https://elite-dangerous.fandom.com/wiki/Stellar_Forge)); one shared persistent galaxy
state, but peer-to-peer instances of ~32 players
([Steam thread](https://steamcommunity.com/app/359320/discussions/0/355043117520018632/)).
**Transition.** The hyperspace jump is a cutscene "while the instance is found and joined" —
and it shows the destination star growing, so the load *is* the arrival. Range is gated by ship
engineering and fuel scooping; the risk of running dry is content.

**Purpose (non-violent).** Exploration is a paid profession: scan bodies, map surfaces, sample
biology, sell the data to Universal Cartographics. "First Discovered by" goes to whoever
*turns the data in* first, and unsold data is lost if your ship is destroyed
([Steam Q&A](https://steamcommunity.com/app/359320/discussions/0/1326718197212701328/)) — so a
months-long trip carries real tension without any combat. Also trading, mining, passenger
tours, and (2025) **system colonisation**, where players claim a system and shape its economy as
"system architect" ([wiki](https://elite-dangerous.fandom.com/wiki/System_Colonisation)).

**Variety.** Astrophysically plausible generation makes rarity legible: earth-likes, ringed
water worlds, close binaries and nebula views are rare *for reasons*, and named. Officially only
~0.065% of systems had been visited by Oct 2025
([Frontier on X](https://x.com/EliteDangerous/status/1973793885731119491)), so a virgin system
is always a few jumps off the beaten path. Failure: the per-system loop is thin — "fly to a
system and honk"; the recurring community verdict is "a mile wide but an inch deep"
([Frontier forums](https://forums.frontier.co.uk/threads/a-mile-wide-but-an-inch-deep.473227/)).

**Social.** Community Goals are pooled deliveries with tiered rewards that permanently change
systems. **Distant Worlds 2** (2019): 13,615 registered, 10,000+ launching together on an
18-week, waypoint-by-waypoint crossing to Beagle Point
([Kotaku](https://kotaku.com/10-000-elite-dangerous-players-start-an-18-week-journey-1831702968),
[PC Gamer](https://www.pcgamer.com/over-10000-elite-dangerous-pilots-embarked-on-distant-worlds-2-though-a-few-faceplanted-into-a-33g-planet/));
its mid-route community goal built a permanent station near the galactic core **[unverified —
from memory]**. The **Fuel Rats**, a volunteer rescue service, passed 131,000 rescues
([wiki](https://elite-dangerous.fandom.com/wiki/Fuel_Rats),
[MassivelyOP](https://massivelyop.com/2018/11/05/elite-dangerous-player-run-fuel-rats-have-rescued-50000-stranded-pilots-to-date/)):
a non-violent profession that exists only because travel has a failure state and distance is real.

## 3. Astroneer (System Era, 2019)

Seven small spherical voxel planets in one fixed solar system; terrain layout is generated per
save, planet identity (size, gravity, resources) is fixed
([wiki](https://astroneer.fandom.com/wiki/Planets)). 4-player host co-op or an 8-slot dedicated
server ([guide](https://www.savesync.games/blog/astroneer-save-sharing-guide/)). Travel is a
shuttle launch gated by thruster fuel; cargo is whatever fits on the shuttle. Later, activated
gateway chambers give instant travel but carry "only what's on your back"
([Steam thread](https://steamcommunity.com/app/361420/discussions/0/2993169283961331930)).
Entirely non-violent; purpose is a resource/tech ladder where each planet holds materials the
others lack, ending by activating every planet's core. Works: **a small number of places with
distinct roles** beats many interchangeable ones; deformable terrain is the toy. Fails: once the
ladder is climbed there is no reason to stay; no social layer beyond friends.

## 4. Deep Rock Galactic (Ghost Ship, 2018–) — structure only

A private hub (Space Rig: mission terminal, bar, wardrobe, forge, memorial hall —
[wiki](https://deeprockgalactic.fandom.com/wiki/Space_Rig)) plus disposable generated caves for
up to 4. The **drop pod** is the loading screen in both directions, and the return trip is a
timed event. The mission terminal exposes generation parameters to the player — biome, mission
type, *cave complexity*, *length*, warnings/anomalies — and pays more for harder combinations
([missions wiki](https://deeprockgalactic.wiki.gg/wiki/Missions)). Generation is hybrid:
designers "put up simple shapes manually and generated the level out of that… then the engine
would skin it", and player drilling uses "the same tech as when we are building the level";
the best moments come "when the stuff that we design intersects and creates entirely new caves"
([PCGamesN](https://www.pcgamesn.com/deep-rock-galactic/deep-rock-galactic-unreal-engine-4)).
Transferable: terrain as the puzzle (four traversal tools that each reshape or cross it),
authored skeletons under generated skin, and a hub whose job is ritual (the bar, the salute).
The shooting is separable; the mining quota, navigation in darkness and extraction are not violent.

## 5. Path of Exile's Atlas and Diablo-style hub + random zones

Hideout/town hub; a **map item** is consumed in the **map device** to open portals to a private
generated instance. The zone is the item: maps have rarities and affixes that "make them more
difficult, but also increase their potential rewards"
([wiki](https://pathofexile.fandom.com/wiki/Map)); scarabs add chosen mechanics (3–4 per map —
[Maxroll](https://maxroll.gg/poe/resources/scarabs)); the Atlas passive tree lets players "craft
their own endgame experience by choosing and enhancing their favourite game mechanics"
([PoE Vault](https://www.poe-vault.com/guides/atlas-passive-skill-tree-strategies)). Six portals
per map as the failure budget is **[unverified]**. The content is violent, but the structure is
not: **players author the parameters of the place they are about to enter, the parameters are
tradeable objects, and risk/reward is legible before departure.** Failure mode: layouts become
irrelevant — players pick the most efficient tileset and ignore the space itself.

## 6. Warframe open zones (brief)

Cetus is a 50-player public town; the Plains beyond the gate are a static ~2.25 × 2.25 km map
entered in squads of ≤4 ([wiki](https://warframe.fandom.com/wiki/Plains_of_Eidolon),
[PCGamesN](https://www.pcgamesn.com/warframe/warframe-plains-of-eidolon-open-world)). The town
gate is the loading seam. Fishing and mining carry their own gear ladders and faction standing;
the update nearly doubled concurrents
([PC Gamer](https://www.pcgamer.com/warframe-plains-of-eidolon-update-almost-doubles-concurrent-player-count/)).
Lesson: a dense public town bolted to a sparse private wilderness reads as "an MMO" with very
little server cost.

## 7. Dwarf Fortress and Caves of Qud — generated history as content

**DF** simulates civilisations, wars, artifacts and individuals for centuries before play; the
world then "persists as long as you like, over many games, recording historical events"
([USC exhibit](https://scalar.usc.edu/works/game-changers-2016-exhibit/dwarf-fortress)).
Legends mode and external viewers
([LegendsViewer](https://github.com/Kromtec/LegendsViewer-Next)) make history browsable; players
share worldgen parameter sets and seeds
([wiki](https://dwarffortresswiki.org/index.php/Advanced_world_generation)). Failure: most of it
is only visible in a menu — history rarely shows up *in the place you are standing*.

**Qud** takes the cheaper, more transferable route. Grinblat & Bucklew did not simulate
historical logic, "just as most procedural terrain generators don't aim to simulate water
erosion"; they generate historical *accounts*. Five sultans each get a **domain** (glass, ice,
stars, scholarship…) and ~12 random life events, rationalised after the fact by a replacement
grammar; events rename places ("Tappa Cave… renamed the Freezing Marsh"). Players meet single
"gospels" on shrines, paintings and engravings, or trade them with NPCs; they arrive out of
order and accrue in a journal until "a biographical narrative coheres". The design leans on
**apophenia**, the player's tendency to perceive patterns
([paper](https://www.pcgworkshop.com/archive/grinblat2017subverting.pdf)).

## 8. Starfield — why generated planets felt empty

~1,000 planets, ~10% with life; Bethesda said barren worlds are deliberate so players "feel
small" ([GamesRadar](https://www.gamesradar.com/todd-howard-says-starfields-barren-planets-are-boring-on-purpose/)).
What failed: landing generates an isolated tile that doesn't connect to its neighbours; a small
pool of prefab POIs repeats *exactly* — same layout, same notes — across planets
([GameRevolution](https://www.gamerevolution.com/guides/945190-starfield-whats-procedurally-generated-whats-hand-crafted));
generated places exist "in a vacuum without story"; and nested loading screens (ship → orbit →
land → interior) removed the feeling of travel. Repetition of authored content is noticed far
faster than blandness of generated content.

## 9. Outer Wilds — finite knowability

One tiny handcrafted solar system; no upgrades or collectibles: "if you get rid of those, all
that's left is you wanting to piece together what's going on". Every location holds several
clues pointing at *other* locations. An early hook failed because "you can't expect someone to
be curious about something specific unless they're already familiar with everything in their
immediate vicinity"
([Game Developer](https://www.gamedeveloper.com/design/live-die-repeat-how-i-outer-wilds-i-piques-curiosity-in-an-ambivalent-solar-system)).
A finite, frozen region *can* be fully known — an asset infinite generators do not have.

---

## Transferable ideas for a non-violent voxel MMO

1. **A hub reachable from everywhere, located nowhere** (NMS Anomaly). Make the loading screen
   between super regions *pass through* a shared social space (waystation, caravanserai, lantern
   ship) sharded to 30–50 people. The technical seam becomes the place you meet people.
2. **"Visit the home of someone standing next to you"** plus curated featured builds (NMS
   teleporter). Cheapest possible answer to "whose build should I go and see?".
3. **Seasonal expeditions on a fresh super region** (NMS). Offline generation suits this exactly:
   ship a new region each season, everyone starts at one landing site, milestones in any order,
   rendezvous sites where builds pile up, account-wide cosmetic rewards — then the region stays
   persistent as a settled place rather than being thrown away.
4. **Pooled community goals that leave permanent structures** (Elite CGs, colonisation). A
   bridge, lighthouse or waystation funded by thousands of deliveries, permanently on the map.
5. **Cartography as a profession with a hand-in moment** (Elite). Survey data is carried
   physically and only credited on return to a guildhall; "first surveyed by" goes to who files
   first. Non-violent tension: weather, getting lost, provisions.
6. **Naming rights on real features.** Offline hydrology/geology yields discrete, nameable
   entities — rivers, peaks, lakes, caves, passes. A finite region has a finite gazetteer; filling
   it is a shared, completable project (unlike NMS's 18 quintillion).
7. **Let travel fail softly, so rescue can exist** (Fuel Rats). Stranding (no rope, lamp out,
   boat adrift) plus a distress beacon creates a volunteer helper profession; 2 km view distance
   makes signal fires and lighthouses literal gameplay.
8. **Coordinates as shareable glyph addresses** (NMS portals). Short symbolic addresses for
   places inside a region; unlocking glyphs gates fast travel by knowledge, and addresses spread
   through wikis and chat for free.
9. **Player-authored expedition parameters** (PoE maps, DRG terminal). Small instanced pockets
   (cave systems, islands, ruins) opened by a crafted "chart" whose affixes set biome, depth,
   complexity, hazards (flooded, lightless, unstable) and yield. Charts are tradeable. Persistent
   super regions stay the home; pockets supply renewable resources so homes aren't strip-mined.
10. **Authored skeleton, generated skin** (DRG). With unlimited offline CPU, hand-design landmark
    *archetypes* (a great arch, a sinkhole city site, a drowned valley) and let them intersect the
    simulated terrain; the intersections produce the surprises.
11. **Evidence of process** (Compton). Erosion, river deltas, strata, treelines, scree — the
    offline pipeline's whole advantage is that places can show *why* they look as they do. Spend
    CPU on legible causality, not on more noise octaves.
12. **Generated accounts, not simulated history** (Qud). Give each region a few founding figures
    with a *domain*, a dozen rationalised events that rename real generated features, and scatter
    single "gospels" on shrines, murals and standing stones. Players assemble the biography
    collaboratively; an archivist profession falls out.
13. **History you stand in, not read in a menu** (DF's gap). Every generated event should leave a
    site: a ruin, a quarry scar, a terraced hillside, an abandoned road — which double as building
    plots and material sources.
14. **Few regions with distinct roles** (Astroneer). Each super region gets a signature — unique
    stone, dye plants, climate, light — so trade and travel between regions have a reason.
15. **Canonise player institutions** (Hello Games and the Galactic Hub). Build census, town
    charters, borders on the map, and event calendars *into* the game; promote real player towns
    into NPC dialogue and official maps.
16. **The transition shows the destination** (Elite's jump). Theme the load as approach — a
    descent through cloud, a boat out of fog — rendering the target region's real LOD silhouette.
17. **Knowledge as progression** (Outer Wilds). Because a region is finite and frozen, a few
    hand-tuned cross-site mysteries per region (alignments, inscriptions pointing at other sites)
    are feasible and wiki-able without being spoiled into nothing: builds and people remain.
18. **Dense town, sparse wilderness** (Warframe). Let region landing sites be high-cap social
    spaces; the wilderness can be thinly populated as long as traces (names, builds, cairns,
    beacons) are everywhere — NMS's "ambient multiplayer".

## Warnings

- **Differentiation is not uniqueness.** More seeds, more biomes and more regions do not fix
  oatmeal. Budget for fewer regions with stronger identity.
- **Never regenerate under player builds.** NMS biome resets under existing bases are the
  standing grievance. "Generated offline, then frozen" is the right call — keep the generator
  version pinned per region forever; improve worldgen only in *new* regions.
- **Exact repetition of authored pieces is noticed faster than bland generation** (Starfield).
  Any prefab ruin or landmark must be parameterised or unique per region.
- **Stacked loading screens kill the sense of travel** (Starfield). One seam per journey, themed,
  ideally doubling as the hub.
- **A thin per-place loop makes breadth worthless** (Elite's "honk"). Surveying a valley must be a
  real activity (walking it, sampling, sketching viewpoints), not a button.
- **Ladder-only purpose ends** (Astroneer). Progression alone gives weeks; months come from
  social roles, building, seasonal goals and status (names on the map).
- **Player-tunable zones collapse to the efficient layout** (PoE). If chart pockets exist, reward
  the space itself (finds tied to terrain features), not throughput.
- **Small instances in a vast space feel empty** unless traces persist. Invest in asynchronous
  presence before raising player caps.
- **Civilisations built outside the game are fragile.** The Galactic Hub survives on Reddit,
  Discord and a wiki; most imitators died. First-class tools are the difference.
- **History in a menu is unread** (DF legends). If it isn't in the landscape, it isn't content.
- **Emptiness "on purpose" is not a defence** (Starfield). Loneliness works in Elite because
  distance has cost and return has meaning; without those it is just absence.
- **Time-limited expeditions create FOMO**; NMS softened this by re-running old expeditions
  **[unverified]**. Plan re-runs or permanent late entry from the start.

## Research gaps

- Fandom wikis returned HTTP 402 to direct fetches (NMS Community Expedition page); used official
  Hello Games pages and Miraheze instead.
- Ghost Ship's "Below Decks: Cave generation" Steam post returned only page chrome; DRG
  generation detail rests on the PCGamesN interview. Community descriptions of the algorithm
  were not used.
- PC Gamer's Distant Worlds 2 article body was not retrievable (sign-up wall); figures come from
  search-result excerpts and Kotaku. The Explorer's Anchorage station and DW2 griefing/instancing
  problems are not sourced here.
- No primary GGG source for Atlas design intent; no primary Tarn Adams interview was fetched.
- Sean Murray's GDC 2017 "Building Worlds Using Math(s)" was located but not reviewed.
