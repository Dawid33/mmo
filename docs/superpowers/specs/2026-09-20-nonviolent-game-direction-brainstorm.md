# Non-violent game direction — brainstorm notes (2026-09-20)

Status: **brainstorm in progress, nothing decided beyond the register.** No code.
Source for mechanics: Patrick Littell, *Non-violent game design patterns* v2.0 (2025);
page numbers below refer to it.

## Decisions so far

- Strictly non-violent. Read as: no combat, no hunting/fishing/butchery, non-lethal
  failure (lost, stranded, exhausted — never dead).
- Register: **cozy/social + contemplative/exploratory** as the core. Playful-competitive
  play (races, sport) is delivered by in-sim wasm mods, not the base game.
- Working assumption (not yet confirmed by Dawid): the **home-and-away hybrid** — a
  homestead that is also a waystation on a trail network; asynchronous social layer
  along the road rather than a synchronous village.

## The book's main warning: the Unemployment Problem (pp. 17–21)

Removing combat usually also removes the protagonist's *role*: skills to practise, a
world that rewards them, NPCs/players who esteem them. A "wanderer who gardens, hosts,
maps and delivers" is exactly the smörgåsbord the book warns about. Fixes it offers:

1. Ask "what is the protagonist's role in their community / what are they good at?"
2. **Institutionalize** the role with a name and supporting institutions (Pokémon
   Trainer + gyms), which also turns fetch quests into respectful requests (asking a
   baker for a cake vs. asking a warrior for one; being *the* mailman, p. 100).
3. Make sure a **win condition** exists, is known in advance, and fires often (p. 23).
   "1000 XP for stumbling on the Forest of Zorgost" is a reward, not a win condition.

Proposal to test: every player is a **Wayfarer** of one named institution (working
name "the Survey") whose job is to chart the world and keep its roads open. Lodges
(player waystations) are its "gyms". One unifying artifact: a **stamp book** (cf.
pilgrim credencial / goshuin / eki stamps) — each lodge has a stamp; the book is at
once set collection (p. 154), diary (p. 78), showcase (p. 85) and the proof-of-walking
that unlocks fast travel (p. 105).

## Pattern → mechanic map

Legend for cost: **sim** = must live in the rolled-back simulation (pay per tick ×
reconcile depth, so keep event-driven or closed-form); **derived** = read model over
confirmed state, outside rollback (free to be rich).

### Wins, progress, acknowledgement
| Pattern (page) | Mechanic here | Cost / fit |
|---|---|---|
| Reach a destination (24) | Peaks, capes, lodges you can *see* from up to ~2 km as self-chosen goals | fits the 32-region far-terrain view (a region is 64 m, so the view is ~2 km — earlier "20–30 km" claims in the session were wrong) |
| Area control, ratcheted (25) + Timelapses (262) | The lit trail network is the "map turning blue"; a server-wide world timelapse | derived: archive coarse LOD tiles periodically |
| Connections (28) | Linking lodges into a trail/fast-travel network is the shared long game | sim only for placed blocks |
| Set collection (154) | Regional sets (alpine flowers 3/5) create anticipation exploration alone lacks | derived |
| First-time bonuses (165) | Growth comes from *novel* foods/places/specimens, never repetition | pushes exploring; avoids grind |
| Diaries (78), News (79), Showcases (85) | Guestbooks; a regional gazette ("first ascent of…", "new trail opened"); homestead as walkable museum | derived — the book stresses quotidian protagonists *need* this acknowledgement |
| Leaderboards → **histograms** (261) | Per-trail histograms on several axes (time, provisions used) instead of global ranks | derived; pairs with deterministic ghost replays |

### Travel and exploration
| Pattern | Mechanic here | Cost / fit |
|---|---|---|
| Treasure maps (159) + Photography (91) | **Player-authored view puzzles**: sketch/photograph a view; others find where it was made. Verified by pose tolerance | near-zero content cost; deterministic world makes verification trivial |
| Location deduction (160), Tracking & identification (161), Scanning (157) | Naturalist's field guide with stylized, imperfect sketches; look-alike species | derived catalogue |
| Fog of war, Merchant Prince style (217) | A **rumour map**: far lands known but distorted, sharpened by surveying; fixes the Buridan's-ass "why go west?" problem. Surveyed maps are tradeable | cheap: downsample the land's world artifact + deliberate distortion |
| Climbing (110), gliding (111) | Stamina-gated ascent; falling switches to gliding rather than failing ("don't make Getting Over It by accident") | sim, but kinematic — no dynamic bodies |
| Map expansion (229), Vehicles (113) | Gate by stamina/warmth/boat, not locked doors; A Short Hike's feathers | sim, small |
| Provisioning (170) | Pack food/warmth per expedition leg; expiry per *cycle* (one leg), never per-item timers. Player-set difficulty dial | sim, tiny |
| Resting (76) | Lodge rest = save point + shared camp scene; the emotional centre of the loop | mostly client |
| Light and darkness (220) | Night travel between lit lodges as oases; keep contrast accessible | render + small sim |
| Fast travel (105), Shortcuts (104) | Only between lodges you have *walked* between, and only while the lodge is kept — gives hosts real value (Spelunky's "prove mastery before bypass") | derived from stamp book |
| Delivery (100), Trading (102) | An institutional post: choose contracts, carry several parcels (route planning). Letters between players carried physically. Prices: shallow, legible oscillation | derived + inventory |
| Seasons (73) | Book warns seasonal cycles are slow. **Place substitutes for season**: altitude within a land, and above all *different lands with different climates* — what cannot grow here grows across the sea → a reason to voyage and trade | each land's climate is a stored field of its world artifact |

### Home, hosting, growing
| Pattern | Mechanic here | Cost / fit |
|---|---|---|
| Gardening (64) | Plant suitability = lookup in the land's stored climate fields; growth = f(elapsed ticks) on region restore | required by region parking |
| Trait propagation (164), Animal husbandry (63) | Breed cuttings; traits inherit by hidden rules the community reverse-engineers | event-driven sim |
| Cooking (57) + NPC preferences (54) | TerraFirmaCraft-style flavour vectors; each *player* has a seeded preferred vector, so hosts learn their regulars. Many counters, not one value chain, so nobody spams one recipe | seeded from entity id |
| Expanding hubs (231) | NPCs met on the road move into your lodge (Suikoden); they unlock systems and keep it alive while you're offline | entity-generic NPCs; idle in parked regions |
| Multiplayer visitation (86) | Solves the blank-page problem; browser join-by-link makes "come see this" one click | existing WebTransport client |
| Festivals (75) | Exactly where mods go: minigames too thin to be permanent gain specialness by being time-limited | mod plots |
| Level editors (83) | Trail- and course-building for other players | world edits |

## Patterns to avoid (and why)

- **Leveling by use (235)** — Ultima Online's overproduction and typecasting; cap growth by results/novelty instead.
- **Punishing streaks (74)** and real-time decay — stressful, and hostile to parked regions. Lenient or none.
- **Deep opaque ecosystem/economy sims (65, 102)** — "the player should be the one having the fun"; also the most expensive thing to put in a rolled-back sim.
- **Coy evaluation (68–69)** — Stardew's unseen judge creates anxiety in a game with no fail state. Be upfront about what counts.
- Combat reskins (healing-as-damage 58, conversion 32), mazes (96), memorization (221), gacha (251), filler fishing/fetch quests.
- Value chains (152–153): gathering is worthless until long-term goals exist — build the goals (lodge, stamp book, catalogue) before the sticks.

## Architecture notes carried from the session

- Blockers for a travel game: the open crossing regression (new home region bit-inexact after a crossing) and in-memory-only region parking (a cozy game cannot lose homes).
- Anything time-based must be closed-form on restore; never tick an empty region.
- Griefing is the PvP of a non-violent MMO: claims/permissions are core, not deferred.
- Mod prerequisites (block property table, place/break verb, open enums) are the same ones plants and lodge objects need.

## Super regions change the shape of the game (added after Dawid's 2026-09-20 note)

Context: the world is divided into **super regions = one k8s pod each, loading screen
between them**, each generated offline **as its own finite world** with an unlimited CPU
budget (see `2026-09-20-multi-instance-k8s-postgres-research.md` §1a fallback, and the
addendum in `2026-09-20-df-worldgen-research/00-synthesis.md`: simulated erosion, real
rivers, geology, climate, generate-and-test, later history/settlement layers; simplest
edge = ocean all round). Call one super region a **land**.

What that gives the design:

- **A land is a place, not a slice of infinity.** Named, finite, with its own geology,
  climate and rivers. Finite means *knowable*: a survey can reach 100 %, a peak can be
  "the highest in the land". Completion becomes a legitimate win condition (ratcheted
  area control, p. 25) — impossible in an unbounded function-of-position world.
- **The loading screen is a voyage.** With an ocean edge, harbours are the doors. The
  home-and-away loop gains a second tier: walks within a land, voyages between lands.
  Boats gate map expansion (pp. 113, 229). Ship departures on a schedule (p. 70) make
  the harbour a natural waiting-together social space.
- **Lands differ on purpose.** Forge parameters (arctic, karst, desert, archipelago)
  make specimens, seeds, stone and recipes land-specific → trading and delivery between
  lands mean something (pp. 100–102), and gardens become records of voyages.
- **Generate-and-test for gameplay**, not just hydrology: reject worlds without good
  lodge sites, a snow-line range, a navigable river, passes worth finding.
- **Forge-time history** (DF's legends layer): old roads, ruined beacons, abandoned
  surveyor's notes → generated content for treasure maps, location deduction, restoration.
- **New lands as the live cadence.** A fresh land can be opened without a content team
  and without wiping old ones: the frontier renews (first landing, first ascent, first
  map) while settled lands stay home. Answers "latecomers find everything discovered".
- **Costs to design around:** friends separated by a loading screen (→ carried post,
  harbour noticeboards, easy regrouping); land is finite so claims/plots are scarce;
  one pod per land means a popular land has a population ceiling (→ lodging/harbour
  capacity as a soft diegetic cap); `?offline` needs a small downloadable land.

### Purpose / goals / win condition — three framings

Book test (pp. 17–23): the role must be esteemed by the world, and the win must be known
in advance and fire often. Wins need to exist at three scales: session, land, world.

1. **Charting** — purpose is knowledge: make the unknown land known. Land win = the
   Survey of that land completed (peaks climbed, rivers traced source to sea, species
   catalogued). Clear, finite, very (c). Risk: a finished land has no purpose left;
   first-comer advantage. Needs new lands + a personal track.
2. **Settling** — purpose is place: make the land homely. Land win = prosperity tiers
   (trails, lodges, NPC settlers arriving; expanding hubs p. 231 at land scale). Evergreen
   and very (a). Risk: sandbox drift, no ending, weak pull outward.
3. **Relighting / pilgrimage** — purpose is restoration of something the forge-time
   history lost (a beacon chain, an old road). Land win = the chain relit end to end;
   the last beacon opens the sea-route to the next land. Strongest, most legible win and
   a diegetic loading screen. Risk: finite content consumed by the first players.

Recommended layering (one purpose at each scale, none competing):
- **Session win:** reach the next lodge/peak before dark; fill a guide page; deliver a
  parcel; earn a stamp. Known in advance, several per session.
- **Personal land win:** your own pilgrimage — every lodge/beacon of a land in *your*
  stamp book. Always available to latecomers; grants the honours of that land.
- **Communal land win:** the land's Survey and beacon chain as ratcheted world state;
  beacons need *keeping*, which is what gives hosts lasting esteem.
- **World win:** completing a land's chain is what lets the community open the next
  land — map expansion at MMO scale, and the server's recurring shared event.

## Candidate direction from Dawid (2026-09-20): home archipelago + resetting expedition islands

**The idea.** An archipelago of player islands, generated to fit player count; a player claims
an island alone or with others and runs a survival-crafting / base-building loop there, which
prepares them for **expeditions**: themed, communal super regions with things to do, which
reset regularly and pay out exotic resources to bring home. Expeditions are tiered — completing
some unlocks later ones — so item-gated tiers form the tech tree. The main game is the home
base and its progression, in the spirit of Potion Craft or tech-focused modded Minecraft packs.

**Assessment (assistant).** Strongly supported by the precedent research
(`2026-09-20-generated-regions-games-research/00-synthesis.md`): it is the "permanent home +
opt-in resettable frontier" shape, the only lifecycle that survived in every game studied
(DQB2 Explorer's Shores, Animal Crossing ticket islands, Boundless exoworlds, Minecraft
resource worlds, Dune's Deep Desert). Communal expeditions concentrate the population instead
of diluting it (Boundless's failure); a quest-book-style tech tree answers "no direction"
and the book's Unemployment Problem (role: islander/expedition member; win: a completed
expedition, known in advance, frequent).

Open problems, in order of importance:
1. **What an expedition is without combat.** In DRG / Monster Hunter / Dune the expedition
   content is fighting. Candidates: traversal under hazard (cold, dark, tide, heat, height),
   extraction logistics (getting the heavy thing back to the boat), observation checklists
   (DQB2), systemic/physics puzzles in generated ruins. Regularly resetting + generated means
   challenges must be *systemic*; authored logic puzzles are solved once and wikied (book
   p. 183: lock-and-key "moon logic", "always show the goal"; PoE zones collapse to the
   efficient layout). DRG's answer: authored room archetypes, generated skin.
2. **Automation vs this engine.** Tech-modpack machines tick constantly; here every in-sim
   tick is re-simulated on mispredict, and an island with nobody on it is parked and does not
   tick. Potion-Craft-style hands-on crafting is event-driven and cheap; GregTech-style
   always-running factories need rate-based machines with closed-form catch-up on restore.
3. **Home islands cannot be one pod each** (Creativerse was sunk by per-player hosted worlds).
   A home *archipelago* super region holding ~100–150 player islands matches Cook's cohort
   size, gives neighbours within the 2 km view, and costs nothing for islands whose owners are
   offline (parked regions).
4. **Tiers split friends and gate builders.** Gate what you can extract or use, not where you
   may go; let builders buy exotics from expeditioners (Eco's interdependence; Dune's ">80 %
   never touch PvP" lesson, transposed).
5. **Communal + resetting invites a reset-day rush** (Rust wipe day). Per-player yields or
   positive-sum nodes (Palia) rather than contested ones.
6. **Register shift.** This leans systemic/engineering — not what was picked earlier
   (cozy + exploratory). Exploration lives in expeditions, cozy in the home island and
   neighbours; decide whether the tech tree is the point or the scaffold.
7. Abandoned home islands (upkeep paid in activity, freeze then archive — Wurm); server-side
   inventories across the loading screen (Valheim's broken economy); rotation must bring
   themes back (FOMO — NMS re-runs).

Engine synergies: expedition islands are disposable, so no long-term edit persistence is
needed there; one forged island artifact can back several pods when popular; each expedition
theme's special rules are a natural **wasm mod** (in-sim, deterministic); read-only snapshot
tours of home islands; the boat as the diegetic loading screen.

### Clarifications from Dawid (2026-09-20, later)

- Expedition islands are **permanent** parts of the game; they do not rotate out (exception:
  one-time themed event islands). Each has a **core progression** plus **timeless minigames**
  players return to, which keeps the island lively after the tech tree is done.
- A super region should house many players; density follows from terrain generation and
  base-building needs.
- **No economy required.** Focus: exploratory, challenge-based gameplay + base building +
  progression; then timeless minigames played with others, by players who have built an
  identity through their base and their relationships to other islanders / clans.

Consequences noted by the assistant:
- The game has a **finite campaign and a social endgame** — closer to Club Penguin / Toontown
  (home + minigames + identity; not covered by the precedent research) than to a survival MMO.
  Finite campaign sidesteps "hand-authored content cannot feed an MMO"; retention then rests
  on minigames, building and people — and on mods as the long-term minigame supply.
- No economy removes trading, wealth gaps and cross-region inventory integrity problems, but
  it means **every player must do the expeditions themselves**: the book's critical-path
  warnings (show the goal, never soft-lock, accessible difficulty, co-op help) apply in full.
  Decide whether items can be given at all; if they can, an economy will emerge anyway.
- Permanent islands make **authored challenges placed on generated terrain** viable (the
  earlier "must be systemic because it resets" argument no longer applies).
- If nothing depletes (per-player yields, per-player challenge state) and expedition terrain
  is protected or decays to the frozen baseline, **expedition islands may never need to
  reset**, and can be **sharded freely** when busy; home archipelagos cannot be sharded.
- Unifying principle proposed: **one island = one verb**. The campaign teaches a movement /
  physics skill as challenge; the island's minigame turns that skill into timeless play;
  venues sit on the progression path so newcomers and veterans cross paths.
- Identity needs an audience: the sail from home harbour to the region edge passes neighbours'
  islands inside the 2 km view; read-only snapshot tours; clan = neighbours in one archipelago.

### Decisions and leanings from Dawid (2026-09-20, latest)

- **Progression is per player.**
- Likes **one island = one verb**, but the verb need not be movement — open.
- **Next step:** design the gameplay loop on the player island and on the expedition island,
  how they interact, and how players are allowed / encouraged to interact on both.
- Likes **Potion Craft's** exploratory trial-and-error crafting; idea: **each player gets a
  randomized recipe space to discover**.
- **No automation.** Bases are **constructed for a purpose**: animals need a pen, a furnace
  needs a mini structure (functional / multiblock structures).

Thinking aids offered by the assistant (not decisions):
- *Recipe space as a map.* In Potion Craft ingredients are movements across a hidden map and
  effects are destinations. If each expedition island's ingredients move in a **new direction**,
  the tech tree becomes geometric: later effects are unreachable until you own an ingredient
  that travels that way — tier gates fall out of the crafting system itself, per player.
- *What to randomize.* Keep ingredient **behaviour shared** (a common grammar players can teach
  each other) and randomize **where things are on each player's map** (answers cannot be
  wikied). Cheap here: the map is a pure function of a per-player seed, nothing to store but
  discoveries. Precedent: roguelike unidentified items (book p. 223), hidden rules (p. 214).
- *Functional structures.* Best precedent is Dragon Quest Builders 2's room recognition and
  Terraria's housing validity: rules constrain **function, not form**, so expression survives.
  Validation is event-driven (on block change), so it costs nothing per tick — a good fit for
  the rollback sim and for parked islands. A structure's build could feed crafting parameters
  (a furnace's draught, a cellar's cool) so the base is part of the recipe space.
- *Verb candidates beyond movement:* observing/identifying, light and dark, heat and cold,
  water and tides, sound/music, growing, herding (indirect control), building that must stand
  up, navigation. Test for a verb: teachable through challenges; replayable with others as a
  minigame; brings something home (ingredient family, animal, structure type); cheap to simulate.
- *Loop skeleton to react to:* discover at home → hit a wall on your map → prepare (build the
  structure, brew the provisions) → expedition teaches the verb and yields a new ingredient
  family → home: new directions on the map, new structures → … → tree done → minigames + base.
- *Interaction hooks compatible with no economy:* brewing **for** someone whose map cannot
  reach an effect yet; co-building permissions; veterans accompanying newcomers (needs a
  reason for the veteran); positive-sum nodes; minigame venues on the progression path;
  neighbours' bases along the harbour route; read-only snapshot tours.

### Dawid's loop design (2026-09-20, latest) — three kinds of place

**Player hub**: a special island filled with NPCs that players travel to. Crafted items are
sold to NPCs (hospital, shopkeeper…) or handed in for quests, in return for other resources —
this gives items a purpose and makes progression feel natural in-world; hub/quest lines guide
players on their options.

**Player islands** — the player can:
- use resources to build a stable base without being griefed;
- create structures required for progression;
- interact with those structures, consuming resources;
- those interactions *are* the progression mechanics: exploration, discovery, trial and error,
  like Potion Craft;
- gather some resources as a tutorial start only; landscape gathering is not required after the
  first expedition; anything obtainable only on player islands must be renewable.

**Expedition islands** — the player can:
- gather resources (literal blocks, or things that spawn occasionally);
- engage in the island's own gameplay loop (TBD);
- play multiplayer minigames, which live in a separate, **non-interactable town** on an
  otherwise wild island; progression players head out into the island instead.

**Loop:** craft + build (tutorial) → first expedition, gather → progress, guided by hub /
quest line → craft, build, progress → further expeditions.

**Requirement:** an in-world explanation for expedition worlds resetting. (So: island identity
is permanent, island *state* resets — blocks are mined there.)

Assistant's refinements and open points:
- *Reset fiction options:* tides that swallow and reshape the island; living islands that
  regrow (themes = species); **mist** that "forgets" — every voyage through the fog arrives at a
  freshly remembered island; storm season; drifting islands that return changed; NPC wardens
  restoring (weak for mined terrain). Mist is the only one that also explains the loading
  screen, several simultaneous copies of one island, and new layouts per reset; hub and home
  islands sit in clear water held open by lighthouses.
- *Reset mechanics:* a global calendar reset causes a reset-day rush and a thundering herd
  (Rust, Last Oasis); a **per-copy lifetime** (a copy appears when first reached, holds about a
  day, then dissolves with warning) avoids both and lets pods scale to zero. Needs "join my
  friend's copy".
- *Same layout or new layout per reset* — open; decides how exploratory expeditions stay.
- *One deep crafting mechanic, structures as operations on it.* Potion Craft itself works this
  way (mortar = distance, water = pull to origin, bellows = finish). Structures that add new
  *moves* on the recipe map unify "structures required for progression" with discovery, and
  avoid designing a separate discovery game per station.
- *NPC requests as needs, not recipes* ("something for a fever"), several NPC relationship
  counters so no single recipe is spammed (book pp. 52–55); pay in things that cannot be
  gathered — plans for structures, hints about your own recipe map, expedition charters.
- *Both directions of feed:* home → expedition is crafted provisions/tools that make an
  island's hazard or verb manageable; expedition → home is ingredient families and structure
  materials.
- *Why other players matter* (since progression, islands and markets are all personal): co-op
  on expeditions with positive-sum gathering, minigame towns, visiting, brewing for a friend,
  and **hub community requests** (all players' deliveries build a permanent new hub wing —
  Elite's community goals without an economy).
- The hub must be sharded with a bias toward familiar faces (Cook's cohorts); arrivals must
  not interrupt anyone.

### Decisions from Dawid (2026-09-20, after the loop design)

- Expedition islands are **different every reset**, with **fixed landmarks** (villages,
  expeditionary camps…) placed on top, depending on the island. A landmark may be the spawn
  area depending on whether the player has already conquered the island.
- The "mist that forgets" fiction is **rejected**: forgetting does not change reality, so it
  cannot explain fixed landmarks in a changing landscape. The land must really change.
- **Global reset** for now.
- Crafting mechanic: TBD, Dawid is thinking about it next.
- Player interaction is **optional**; minigames and clans need multiplayer but the game must
  be **completable solo**.
- **Items can be given** between players. Having items can speed progression but never
  completes it: given end-game gear does not grant access to end-game islands; given
  ingredients speed the crafting exploration, which must still be done.
- Islands are themed around a **quest line** to complete; mechanics TBD.
- Home → expedition: board a **boat**, click the **steering wheel**, pick a destination on a
  **map**.

Assistant's proposals in response:
- *Reset fiction — "the Turning" on steadstone.* A periodic, world-wide natural event that
  physically remakes the soft land of every island, each in its own idiom (glacier calves and
  refreezes, reef regrows, dunes wander, delta silts up, jungle overgrows, volcano rebuilds).
  What never turns is **steadstone**: hard cores of bedrock. Islanders only build on
  steadstone, which is why villages persist; expedition camps are re-pitched by NPCs after each
  Turning. Real-world echo: the North Frisian *Halligen*, whose houses stand on mounds
  (*Warften*) and survive the floods that wash over everything else. The calendar is public, so
  the reset can be a hub festival rather than a loss.
- *Forge tie-in:* treat landmark footprints as **unerodible rock** in the offline erosion
  simulation, so each new landscape genuinely forms around its landmarks.
- *Quest lines on changing terrain* must refer to landmarks and to features the forge places
  by rule (the highest peak, a river source, a cave behind the camp), never fixed coordinates.
- *Progression = knowledge and deeds, never possessions.* Gates check the player's own journal
  (recipes discovered, quest lines completed). A **maker's mark** on crafted items lets
  progression quests require something *you* made, while ordinary NPC trade accepts anything.
- *Solo-completable:* co-op is always optional or scales; minigame rewards are cosmetic only.
- *The map at the wheel* is where the reset becomes visible (time to the next Turning), where
  friends and island population show, and **boarding the same boat** is the party mechanic.
- Optional softening of the global reset: each island turns on its own day, giving a weekly
  rhythm and spreading load, while staying global per island.

### Role and landmarks (Dawid, 2026-09-20)

- **Role: the player is an artisan** at the start of a journey to become a master crafter.
  Like One Piece (islands make pirates stronger) and Hunter x Hunter (journeys make hunters
  stronger), expeditions are what make you a greater artisan.
- **"The Turning" is not adopted.** Preferred: landmarks are **generic and transient** — base
  camp, pirate cove, nomad village — so NPCs can plausibly be somewhere different each time.
  Expedition islands are wild, unexplored territory. Anything needing in-world persistence
  lives in player hubs or similar areas.
- Assistant's note: this makes the simplest fiction available — the map destination is a
  *stretch of sea* (a reach, a chain), and each season the expedition moves on to a newly
  found island in it. It really is a different island; nothing has to be explained away, and
  several simultaneous copies are just different islands of the same reach.
- Real-world echo for the role: the journeyman years (*Wanderjahre* / *Walz*) — craftsmen
  travelling for years to learn from masters elsewhere, then submitting a **masterpiece** to
  the guild to become a master. Fits maker's marks, a hub guild, and a capstone deed per craft.

### What could be crafted? (assistant's brainstorm, for Dawid to pick from)

Test for a craft earning its place: an NPC wants it · it is useful on expeditions · it needs
a purposeful structure at home · its ingredients come from distinct islands · its discovery
space suits trial and error · giving it away cannot complete someone else's progression.

| Item family | Who wants it / what it is for | Home structure | Discovery style |
|---|---|---|---|
| Potions, tonics, medicines | hospital, expedition hazards (cold, heat, dark, depth) | still, herb drying loft | path on a hidden map (Potion Craft) |
| Food and drink (cooking, baking, brewing, tea) | tavern, provisions for expeditions, feasts | kitchen, oven, cellar | combining traits; ageing over time |
| Dyes, paints, inks, glazes | tailors, painters, your own base | dye vats, pigment mill | navigating colour space |
| Textiles and clothing | hazard gear, cosmetics/identity | loom, pen for wool animals | grid patterns (weave drafts) |
| Metalwork: tools, fittings, instruments | tools for gathering and climbing, boat fittings | furnace, forge, bellows | process curves (heat × time), alloy ratios |
| Ceramics and glass | jars and vials for other crafts, lamps, windows | kiln, glasshouse | firing schedules; glaze chemistry |
| Woodwork and shipwrighting | furniture, **boat upgrades** (range, cargo, speed) | sawpit, workshop, slipway | joinery patterns; material properties |
| Navigation and knowledge items | charts, instruments, field guides, recipe scrolls | study, observatory | observation on expeditions feeding drafting at home |
| Cultivated plants and animal products | ingredient supply for every other craft | garden beds, greenhouse, pens, hives | breeding/grafting with inherited traits |
| Art: sculpture, engraving, mosaic, music | patrons at the hub, pure expression | studio | chisel-scale voxel work (engine-native) |
| Charms, fragrances, incense, candles | comfort items, buffs for rest, festival goods | workbench | blending |
| Toys, kites, sleds, balls, racing boats | **equipment for the minigames** | workshop | tuning (physics parameters) |

Distinct discovery spaces, rather than crafts, are the expensive thing to design: hidden-map
navigation · process curves · grid/spatial patterns · trait combination and inheritance ·
sequence/rhythm · slow growth and ageing (closed-form over elapsed time, so it suits islands
that are not simulated while empty). Suggestion: prove the framework with **two contrasting
crafts** (e.g. potions = map navigation; kiln or forge = process curve) before adding more;
let further crafts reuse a discovery space with a different map and different stations.

### Core crafts chosen by Dawid (2026-09-20): things conceptually useful for wild expeditions

1. Potions, tonics, medicines · 2. Food and drink · 3. Textiles and clothing ·
4. Metalwork · 5. Plants and animal products.

Assistant's sketch (for Dawid to react to):

**Five crafts = five expedition needs**, which keeps them from collapsing into one stat:

| Craft | Expedition need it answers | Character of the effect | Discovery style | Home structures | Hub NPCs |
|---|---|---|---|---|---|
| Food and drink | **Sustain** — how long you can stay out | long, mild, baseline; provisions packed per expedition | blending traits (can reuse the map style with its own map) + ageing in a cellar | kitchen, oven, smokehouse, cellar | tavern keeper, expedition quartermaster |
| Textiles and clothing | **Protect** — passive defence against climate and terrain | permanent while worn; layering | grid patterns: fibre × weave × layering → warmth, breathability, water-shedding, weight | spinning wheel, loom, pens for fibre animals | outfitter, tailor; nomads who teach weaves |
| Potions, tonics, medicines | **Recover / enable** — acute, specific, short | strong and brief: night-sight, breath, steadiness, warming, cooling; cures | path on a hidden per-player map (Potion Craft) | still, cauldron, drying loft | apothecary, hospital |
| Metalwork | **Access** — what you can gather, climb, open, carry | capability, not stats: tool hardness, pitons, lanterns, cookware, still parts | process curves (heat × time, quench) and alloy ratios | furnace mini-structure, bellows, forge, anvil — build quality sets reachable heat | toolwright, ship's chandler |
| Plants and animal products | **Supply** — the renewable base at home | feeds the other four | breeding and grafting with inherited traits; slow growth computed from elapsed time | beds, greenhouse, pens, hives, coop | seed and stock market; herders |

- **Dependency loop:** plants/animals → herbs (potions), crops (food), fibre (textiles), fuel
  (metal); metalwork → tools and station parts for all; expeditions → wild ingredient
  families, ores, exotic seeds and animals. Matches the rule that home-only resources are
  renewable: craft 5 *is* the renewable base.
- **Strictly non-violent consequence:** animal products are wool, milk, eggs, honey, silk,
  shed feathers/antlers — no meat, leather or bone. Waxed canvas, bark cloth, plant rubber
  cover what leather would. Metalwork makes tools only.
- **Islands as hardships rather than verbs:** each expedition reach has a hazard profile
  (cold, heat, wet, dark, altitude, rough ground, thin air, salt). The quest line's practical
  gate is a **kit** drawn from all five crafts; the artisan grows because the island demands
  something their craft cannot yet make (the One Piece / Hunter x Hunter beat).
- **Substitutability makes random recipe spaces fair:** every hazard should have partial
  answers in several crafts (warm weave OR warming tonic OR hot food), so a player whose
  potion map puts "warmth" far away leans on clothing instead, and solo play never dead-ends.
- **Keep function and looks separate in clothing** (function from fibre and weave, looks from
  cut and dye), so the best gear is never forced to be ugly — clothing carries identity.
- **Scope:** four discovery styles across five crafts (food reuses the map style). Build the
  shared frame first — journal, free preview of a move, station interaction — then two
  contrasting crafts (potions + metalwork or textiles) before the rest.
- **Hub implied:** apothecary/hospital, tavern + quartermaster, outfitter, toolwright/chandler,
  seed-and-stock market, and the **guild hall** (ranks, journal, masterpieces). Transient
  expedition NPCs: camp quartermaster (kit advice), nomads (weaves, breeds), cove folk
  (exotic seeds).

## Open questions

1. Social layer: hybrid (assumed) vs. synchronous village vs. pure nomad.
2. Role: one institution with emergent specialisms vs. distinct professions/classes.
3. (Parked) Purpose: charting vs. settling vs. relighting — superseded if the archipelago/expedition direction is adopted.
5. Archipelago direction: is progression per player or per island/clan? Can items be given? Does anything on an expedition island ever reset? Is expedition terrain editable?
4. Land size (the forge's main parameter; rivers from range to sea want ≥ 8–16 km) and edge type (ocean all round assumed).
