# Multiplayer games with large generated regions behind loading screens — synthesis

2026-09-20. Four subagent reports; this file ties them together as a **menu**, not a
recommendation. The game direction is undecided (see
`../2026-09-20-nonviolent-game-direction-brainstorm.md`; the charting/relighting ideas there
are parked, not adopted).

| | report | cluster |
|---|---|---|
| 01 | `01-voxel-multiworld.md` | Boundless (deep), Trove, Portal Knights, Starbound, Terraria, Minecraft networks + SMP seasons, Creativerse, DQB2, LEGO Fortnite, Hytale, Cube World, Vintage Story |
| 02 | `02-survival-server-grids.md` | Wurm Online, Eco, A Tale in the Desert (deep), Valheim, Last Oasis, Dune: Awakening, Rust, Once Human, Ark, Atlas, Nightingale, Pax Dei, Worlds Adrift |
| 03 | `03-hub-and-expedition.md` | No Man's Sky (deep), Elite Dangerous, Astroneer, Deep Rock Galactic, Path of Exile maps, DF/Caves of Qud generated history, Starfield, Outer Wilds |
| 04 | `04-cozy-nonviolent-multiplayer.md` | Animal Crossing NH, Glitch (deep), Sky, Palia, Book of Travels, Spiritfarer, Journey, Meadow, One Hour One Life, Minecraft community servers; Cook / Horseshoe / Koster design writing |

Evidence quality: every report lists blocked sources (MassivelyOP, Fandom wikis, PC Gamer,
Forbes and others refused fetches; nothing was worked around) and tags affected claims
`[unverified]`. There is no first-party Boundless postmortem — its decline analysis rests on
community threads. Player-count and date claims (e.g. Book of Travels' 2026 server closure)
were not independently checked. Treat numbers as indicative.

Our constraints, for reference: strictly non-violent; super regions of roughly 8–16 km, one
server process each, **loading screen between them**; each generated offline as a finite world
with an unlimited CPU budget, then frozen and player-editable; seamless inside, ~2 km view;
deterministic sim; browser + native clients; competitive play arrives later through mods.

## 1. Closest precedents

- **Boundless** is nearly this architecture: one server per finite generated voxel planet,
  ~100 players each, player-built portals between them. Most of its play time was non-violent
  (building, shops, colour collecting, arbitrage).
- **Wurm Online**: finite generated island servers, never wiped; you sail off one map edge and
  arrive on the neighbour's matching edge with your cargo. Largely a crafting/terraforming society.
- **Animal Crossing NH**: a home island, an airport, ticketed disposable generated islands, and
  visiting friends' islands — the cozy version of the same shape.
- **No Man's Sky**: the largest mostly-non-violent generated multiplayer game; its lessons are
  about hubs, seasonal expeditions and player-founded civilisations.
- **Eco** and **A Tale in the Desert**: proof that purpose without combat can hold a server —
  through specialisation, laws and shared projects.

## 2. What the precedents agree on

1. **Density, not capacity, is what kills region grids.** Boundless ran ~50 planets for a few
   hundred players; Atlas ended at 4–12 players per cell; Wurm's devs say "too many servers is
   detrimental". Open few regions, connect all of them, add one only under land pressure.
2. **The loading screen is forgiven when it is a place and a verb** — airport, harbour, cloud
   gate, a boat you live on — and resented when stacked (Starfield) or when arrivals interrupt
   residents (Animal Crossing).
3. **Regions must be asymmetric or nobody travels.** Native fruit and local prices (Animal
   Crossing), per-planet block colours (Boundless), exclusive resources (Ark, Wurm, Astroneer).
   Aesthetic exclusives trade well without power creep.
4. **Forced expiry loses to builders.** Last Oasis removed its burning world, Once Human removed
   mandatory wipes, Worlds Adrift's wipes drove players off. What survives: a permanent home
   plus an *opt-in* resettable or temporary frontier (Dune's Deep Desert, Boundless exoworlds,
   Minecraft resource worlds, Hermitcraft seasons, NMS expeditions).
5. **"No direction" is the recurring complaint** (Boundless, Glitch, LEGO Fortnite at launch,
   Book of Travels). A sandbox still needs a legible first goal and a medium-term ladder.
6. **Generated variety reads as sameness unless it shows evidence of process** (Compton's
   "10,000 bowls of oatmeal"; Nightingale, Atlas, Starbound). Simulated erosion, rivers, strata
   and climate are exactly that evidence — this is where the offline CPU budget pays off.
7. **Hand-authored content cannot feed a small MMO** (Glitch, Book of Travels, Palia layoffs).
   The generator and the players' building have to be the content pipeline.
8. **Anything others depend on must not need upkeep from volunteers.** Boundless's
   fuel-burning portals made the travel network a chore; when the hub guild quit, parts of the
   universe became unreachable for newcomers.
9. **If the game does not provide trade/visit matchmaking, third parties will** — with bots,
   fees and scams (Turnip.Exchange). Same for player civilisations run from Discord (NMS).
10. **Griefing controls must exist before the first public test** — claims, graded
    permissions, rollback; and remove the *affordance* for harm (no body-blocking, no contested
    drops) rather than policing it (Sky, Journey, One Hour One Life's lesson).

## 3. Decisions the constraint forces, whatever the theme

| Decision | Options seen in shipped games |
|---|---|
| How the crossing is themed | voyage off the map edge onto the matching edge (Wurm) · built gate showing the destination (Boundless) · a shared social space *inside* the transition (NMS Anomaly) · time aboard your own small vessel/home (Starbound ship, Spiritfarer, Trove cornerstone) |
| What crosses | everything (Boundless, Terraria) · people cheap, cargo expensive (Wurm portals vs boats, Valheim's ore rule) · per-region import whitelists (Ark) |
| Who owns the links | system-owned trunk with player-built local branches — the lesson of Boundless's volunteer portal networks |
| Region lifecycle | all permanent (Wurm) · permanent homelands + calendar expedition regions (exoworlds, Deep Desert, NMS expeditions) · archived seasons kept as read-only museums (Hermitcraft) |
| When a region is added | under land pressure only; never a disconnected cluster |
| What differs between regions | aesthetic palette · raw materials · climate/what grows · hazards needing preparation (Boundless atmospheres re-themed as alpine/desert/sea) |
| Groups | parties must survive transfers and may span regions (Hytale; Portal Knights' mistake) |

## 4. Menu A — setting themes that make the loading screen make sense

1. **Archipelago and harbours.** Each super region is an island or coast; ships leave from a
   harbour; the ocean edge is the natural boundary for offline hydrology. Precedent: Wurm,
   Animal Crossing, Spiritfarer. Bonus: the boat as a small decoratable home you ride through
   the transition. Risk: boats push players along region edges; sea travel inside a region
   must be worth doing too.
2. **Worlds in the sky / planets with gates.** Regions are separate worlds visible from each
   other; players build gates. Precedent: Boundless. Bonus: the gate can show the
   destination's real low-detail silhouette, which the engine already streams. Risk:
   science-fantasy tone; "separate worlds" weakens the sense of one geography.
3. **One continent, mountain walls and passes.** Regions are valleys or provinces divided by
   ranges; the crossing is a pass, tunnel or rail terminus. Bonus: a readable world map where
   neighbours matter. Risk: edges must be authored as impassable, and players will test them.
4. **The caravan / travelling home.** Whatever the geography, the vehicle is yours and
   persistent; the crossing is time spent inside it. Precedent: Starbound, Spiritfarer, Trove.
   Risk: needs entity-carried interiors, which the engine does not have yet.
5. **The waystation between worlds.** Every crossing passes through a shared social place
   (sharded to tens of players), which becomes where strangers meet. Precedent: NMS Anomaly.
   Risk: a hub that is only a menu is dead space (Minecraft lobbies); it needs reasons to linger.

## 5. Menu B — gameplay spines that survive without combat

1. **Builders' economy of aesthetics** (Boundless). Region-exclusive stone, timber, clay and
   dye palettes from generated geology/climate; shops; visitor-driven rewards ("footfall");
   settlement prestige and civic titles on the region map.
2. **Homeland + expeditions** (NMS, DQB2, exoworlds, Animal Crossing tickets). Permanent regions
   stay unscarred; rotating expedition regions absorb extraction and give everyone a shared
   fresh start with milestones in any order; DQB2-style observation checklists instead of
   kill quests.
3. **Society as the game** (Eco, A Tale in the Desert). Player-written laws the server
   enforces, nested towns → region councils, specialisation that forces trade, peer-judged
   tests, season-ending monuments whose winners shape the next rules. Governance doubles as
   griefing control. Risk: player justice turning into exile; deadline burnout.
4. **Stewardship** (Eco, inverted). A frozen generated baseline means land can visibly
   recover: replanting, river-bank repair, rewilding quarries; ecology as a regional,
   recoverable antagonist rather than a 30-day doomsday.
5. **Haulage and trade routes** (Wurm, Valheim, Elite). Cargo is the expensive thing to move;
   shipwrights, hauliers, harbour markets with local prices shown in-game.
6. **Exploration as a profession with a hand-in** (Elite). Survey data is carried physically
   and credited on return; travel can fail softly, so a volunteer rescue role can exist
   (Fuel Rats); signal fires and lighthouses use the 2 km view.
7. **History you stand in** (Caves of Qud vs Dwarf Fortress). Generate *accounts*, not a
   simulation: a few founding figures, events that rename real generated features, fragments
   placed on sites; players assemble them; ruins double as building plots.
8. **Hosting and visiting** (Animal Crossing, Palia, Sky). Graded permissions, positive-sum
   shared resource nodes, wordless stranger interaction, friendship as a mutual unlock ladder,
   home cohorts of ~150 with familiar faces.
9. **Community works that open the map** (Glitch, Elite community goals). A bridge, lighthouse
   or the route to a newly generated region opens when residents pool labour; the result is
   permanent and named.
10. **Player-commissioned regions** (Nightingale, PoE maps, DRG). A group spends crafted
    "charts" (climate, geology, age, one quirk) to commission a small expedition region; the
    offline generation wait is themed as outfitting. Fix modifiers at commissioning to avoid
    min-maxing.

Setting themes and spines combine freely; most shipped games run two or three spines.

## 6. Things this engine can do that the precedents could not

- **Read-only snapshot visits** (Animal Crossing's Dream Suite): regions already serialise
  deterministically, so touring a frozen copy of a settlement — grief-proof, asynchronous,
  browser-friendly — is unusually cheap here. Also the natural form for build-contest judging
  and for archived season museums.
- **Destination preview in the gate**: the server already builds low-detail terrain for any
  region, so a crossing can show the real silhouette of where you are going.
- **Evidence of process at walking scale**: offline erosion, rivers and strata address the
  sameness problem that undermined Nightingale, Atlas and Starbound.
- **A frozen pristine baseline per region**: makes "weathering back", restoration and per-claim
  rollback conceivable (needs the terrain-edit events identified in the persistence spec).
- **Deterministic replays** for ghost runs and event highlights; **mods as in-world venues**
  rather than lobby menus (Hermitcraft, Hytale).

## 7. Warnings that apply to every option

World count must track population · no upkeep on shared travel · never regenerate or delete
under player builds (NMS, Nightingale, Boundless rentals) · never reset capability at a border
(Cube World) · never force a party into one region (Portal Knights) · no essentials behind
content most players refuse (Dune: >80 % never touched PvP) · cozy theme on a scarcity
economy becomes a job (Sky's candle runs) · "vast unknowable spaces" are un-cozy, so large
regions need landmarks, roads and warm places to return to (Project Horseshoe) · lifecycle
features multiply technical risk — ship region transfer first, anything expiring much later
(Last Oasis) · a development lull is fatal for a small MMO, so favour systems that generate
content from players and from the generator.
