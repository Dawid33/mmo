# Voxel / sandbox games with a multi-world structure

Research date: 2026-09-20. Cluster 01 of the generated-regions games research.
Question: what can a strictly non-violent voxel MMO — built from large, offline-generated,
finite "super regions" (8–16 km, one server process each, loading screen between them) — learn
from shipped sandbox games with the same shape?

Conventions: claims are cited inline. `[unverified]` marks things I believe from general
knowledge but could not confirm against a source in this session. Blocked sources are listed
under "Research gaps" at the end.

---

## 1. Boundless (Wonderstruck, 2018; Monumental since 2022) — the closest precedent

**Structure.** One shared universe of voxel planets; "each planet is a server basically", capped
at ~100 concurrent players, clustered in real-world regions (EU / US / AUS) for ping, with new
planets generated as population grew
([Steam thread, dev reply](https://steamcommunity.com/app/324510/discussions/0/3317353727673418731/)).
About 50 permanent public planets existed at the peak
([Steam, moderator](https://steamcommunity.com/app/324510/discussions/0/2953755260401173927/)).
Planets are finite, procedurally generated, persistent and player-editable; unclaimed terrain
regenerates, claimed ("beaconed") plots do not
([Wikipedia](https://en.wikipedia.org/wiki/Boundless_(video_game))). Public planet size:
several km across, wrapping `[unverified]`; rented planets are explicitly 3 / 4.5 / 6 km
([FAQ](https://forum.playboundless.com/t/faq-sovereign-and-creative-world/50628)) — i.e. *smaller
than one of our super regions*.

Four world classes:
- **Known worlds** — permanent, plot-claimable, regenerating.
- **Exoworlds** — temporary planets that drift into orbit for days, cannot be claimed
  ("nothing can be protected"), do not regenerate, vanish forever, and carry exclusive
  materials and block colours. The devs framed them as "a platform for future new gameplay
  unrestricted by player beacons"
  ([Release 219 notes](https://steamcommunity.com/app/324510/discussions/1/1814296907960397520/)).
- **Sovereign worlds** — player-rented ($9.99 / $19.99 / $39.99 per 30 days; 10 / 20 / 40
  concurrent players). The owner picks biomes and resources, permissions, and may re-roll block
  colours every 30 days. They orbit a known world of similar tier. On lapse the world locks for
  two weeks and is then deleted, with blocks reclaimed to the owner
  ([FAQ](https://forum.playboundless.com/t/faq-sovereign-and-creative-world/50628)).
- **Creative worlds** — no rules; nothing made there can leave (same FAQ).

**Transition.** The best in the genre. A *portal* is a player-built frame of conduit blocks,
fuelled with Oort shards (about 1 shard/hour, 24/day), through which you *see the other planet
live* and simply walk ([search excerpt of wiki + forum](https://boundless.fandom.com/wiki/Portal)).
A *warp* is the one-shot version, paid in coin proportional to distance in "blinksecs", minimum
100c ([Warping wiki excerpt](https://boundless.fandom.com/wiki/Warping)). Planets are visible in
the sky of their neighbours, so the universe has a readable geography. Your whole inventory
comes with you. The gates are distance/cost, plus *atmosphere*: harsher planets need protection
skills or food buffs
([World wiki excerpt](https://boundless.fandom.com/wiki/World)).

**Purpose.** Violent part: creature hunts and "meteor" defence events, which drop Oort — so the
portal economy was in practice fuelled by group combat. Non-violent part (the majority of actual
play-time): mining and gathering, building, *prestige* (a build-quality score that ranks
settlements and makes the top builder "capital" of a planet), running shops — shops earn from
sales **and from "footfall"**, coin paid to the plot owner when other players walk through
([forum excerpt](https://boundlesstrade.net/shops/169)) — colour collecting, farming,
inter-planet arbitrage ("identify a resource selling for X in one place while another place
buys at X+Y"), and mapping/exploring
([Why I Game blog](https://whyigame.wordpress.com/2020/05/07/boundless-adventures-in-arbitrage-alts-and-alien-planets/)).

**Region differences.** Tier (T1–T7+) controls hazard and resource richness. Every planet has
its own palette of block *colours*; rare colours exist only on exoworlds or event meteors, and
"there are hundreds of colors to collect"
([gleam/colour forum threads](https://forum.playboundless.com/t/gleam-universe-thread-all-87-colors-of-all-3-types-of-chests-and-glass-blocks-have-arrived/29928)).
Colour — a purely aesthetic property — turned out to be the most durable trade good: it needs no
combat balance and builders always want it. A dev explicitly defended gating colours because
"having all colors available makes content such as exos … not as worthwhile"
([compendium thread](https://forum.playboundless.com/t/compendium-of-sovereign-world-concerns-questions-and-answers/48263)).

**Social structure.** Player guilds built the travel layer: hub buildings full of portals
(Portal Seekers, TNT Megahub, Ultima, Legendville…), malls attached to hubs, and a convention
that a settlement "gets connected" by asking a hub guild for a slot
([Portal Seekers](https://portalseekers.weebly.com/connect-my-portal.html),
[TNT](https://boundlesstrade.net/shops/169)). Portal Seekers reached ~50 planets, and players
called it "a lifesaver for some of us that had been going broke warping from planet to planet"
([farewell thread](https://forum.playboundless.com/t/goodbye-from-portal-seekers/57014)).

**What worked.** The see-through portal; hubs as emergent cities; footfall as a reason to build
public-facing places; colour as a collectible; exoworlds as a rolling expedition calendar. A
blogger found 150–300 concurrent "appropriately sized", like *A Tale in the Desert*
([blog](https://whyigame.wordpress.com/2020/05/07/boundless-adventures-in-arbitrage-alts-and-alien-planets/)).

**Why it declined.** Peak ~1,150 Steam concurrent at launch, never regained; ~120 by mid-2022;
16 concurrent when Steambase was sampled ([Steambase](https://steambase.io/games/boundless/steam-charts),
[Steam thread](https://steamcommunity.com/app/324510/discussions/0/3278065083960718577/)). Causes
named by the community:
1. **Too many worlds for the population.** 50 planets plus rentals divided among a few hundred
   players: "Hubs feel empty, shopping districts ARE empty"; new players felt "extreme isolation
   despite MMO designation" (same Steam thread).
2. **Travel infrastructure was volunteer labour with upkeep.** Portals burn fuel forever; the
   fuel comes from hunts. Portal Seekers closed in June 2021 citing "inactive leadership", "the
   redundancy of having multiple major portal networks" and preferring "building and exploring vs
   … administrative tasks" ([thread](https://forum.playboundless.com/t/goodbye-from-portal-seekers/57014)).
   When a hub guild quits, a slice of the universe becomes unreachable for newcomers.
3. **Rented worlds fragmented things further** and read as "a money grab"; when a rental lapses
   its builds vanish
   ([Steam](https://steamcommunity.com/app/324510/discussions/0/2953755260401173927/)).
4. **Grind and direction.** "Crafting recipes are bloated", RNG forging, popular play styles
   (bomb mining) removed instead of alternatives buffed, "no sense of direction in what it's
   trying to be" (Steam thread above). Veterans with forged tools out-gathered newcomers by
   orders of magnitude
   ([blog](https://whyigame.wordpress.com/2020/05/24/boundless-self-inflicted-difficulty-curve/)).
5. **Development stopped.** Wonderstruck moved onto contract work for Larian; the game sold to
   Monumental in September 2022 after a long lull
   ([Wikipedia](https://en.wikipedia.org/wiki/Boundless_(video_game)),
   [MMORPG.com](https://www.mmorpg.com/news/boundless-has-been-acquired-by-monumental-joining-crowfall-and-mythgard-on-its-roster-2000126084)).

---

## 2. Trove (Trion 2015, now Gamigo)

**Structure.** A **Hub** world (crafting stations, trading, portals) plus procedurally generated
**Adventure Worlds** entered through tiered portals gated by Power Rank (Novice → Uber-9, each
with a dominant biome), plus **Club Worlds** — permanent, guild-owned, expandable with
Terraformers ([Trove wiki: Hub](https://trove.fandom.com/wiki/Hub),
[Trovesaurus](https://trovesaurus.com/page=2910/a-look-at-adventure-worlds),
[trovegame.com](https://trovegame.com/game/cornerstones-club-worlds/)). Adventure worlds are
disposable instances that spin up and close with population `[unverified]`.

**Transition.** Walk-in portals in the Hub. The standout idea is the **Cornerstone**: a personal
16×16×200 plot that *travels with you* — claim any empty cornerstone slot in any world and your
house pastes in ([same source](https://trovegame.com/game/cornerstones-club-worlds/)).

**Purpose.** Mostly a combat loot-grind (dungeons, Shadow Tower) — not transferable. Non-violent
layers: collection/"mastery" (mounts, wings, styles unlock account-wide), fishing, gardening,
three 250-level crafting professions, club-world building
([Wikipedia](https://en.wikipedia.org/wiki/Trove_(video_game))).

**Lessons.** Splitting "disposable world you strip-mine" from "permanent world you build in" is
clean and kills griefing, but the adventure worlds have no sense of place — nobody remembers
one. Club worlds were the only real places, and they were invisible unless you were invited.

---

## 3. Portal Knights (Keen Games, 2017)

**Structure.** 44 small floating islands, each with its own theme, creatures and resources, in a
fixed progression order; terrain generated per save; 4-player co-op
([Nintendo Life](https://www.nintendolife.com/reviews/nintendo-switch/portal_knights),
[Wikipedia](https://en.wikipedia.org/wiki/Portal_Knights)).

**Transition.** Each island has a broken portal; you gather shards on the island, craft portal
stones and repair it to unlock the next ("explore an area, mine its resources then find and
repair the portal"). The loading screen is *earned by local work*.

**What failed.** In early access every player had to be on the same island: "being forced to all
go to a map together was extremely limiting"
([Steam feedback](https://steamcommunity.com/app/374040/discussions/0/405692758721089300)); later
relaxed `[unverified]`. Islands are too small to feel like places, and critics wanted more
NPCs, quests and story ([Metacritic](https://www.metacritic.com/game/portal-knights/critic-reviews/)).
Note for us: "one process per world" tempts you into "party must share a world" — don't.

---

## 4. Starbound and Terraria

**Terraria.** Characters and worlds are separate files; any character can enter any world with
its full inventory plus personal pocket storage (piggy bank/safe) that follows it everywhere
([Steam](https://steamcommunity.com/app/105600/discussions/0/648814300395842111/)). Players cite
this as "a major feature": side-worlds for fishing, rare biomes, or resources the home world
lacks. Non-violent retention: building, NPC housing (NPCs have biome and neighbour preferences,
and happy NPCs sell teleport pylons `[unverified]`), fishing quests, collections. The
combat/boss ladder is the spine and does not transfer.

**Starbound.** A procedurally generated universe of finite, wrapping 2D planets; your **ship** is
a persistent, fully decoratable home that *is* the loading screen — cockpit navigation, fuel
cost per jump, teleporter pad to beam down
([Wikipedia](https://en.wikipedia.org/wiki/Starbound)). Non-violent loop: colonies — build
rooms, place a deed, the furniture theme decides which tenant arrives; tenants pay rent, give
quests, can become crew
([Steam](https://steamcommunity.com/app/211820/discussions/0/152391246766464884/)); farming;
collecting. After the story "it becomes pretty much explore and build… strip them of their
resources, rinse and repeat"
([Steam](https://steamcommunity.com/app/211820/discussions/1/3417683648801183418/)).

**Lessons.** (a) Character-portable inventory makes multi-world feel generous rather than
fragmenting. (b) A mobile personal base makes the transition cosy instead of dead time.
(c) With effectively infinite planets no planet matters; people settle one and ignore the rest.
Our finite, hand-counted super regions are an advantage here. (d) Players build colonies mostly
because watching inhabitants *use* the build is pleasing — the rent is incidental.

---

## 5. Minecraft server networks and SMP season culture

**Networks.** A BungeeCord/Velocity proxy fronts many backend servers; players land in a
hub/lobby full of portals and NPC selectors and are handed between servers without reconnecting;
one backend can restart while the rest stays up
([mc-node](https://mc-node.net/blog/en/velocity-proxy-network/),
[xgamingserver](https://xgamingserver.com/blog/bungeecord-vs-velocity/)). Architecturally
identical to our super regions. Known weakness: hubs are menus with scenery; nobody lives there.

**Resource worlds.** Long-running SMPs pair a permanent main world with a **resource world that
resets on a schedule** — Slabserver every two months, with a fresh random seed, entered by
throwing an ender pearl through the main world's border; inventory, ender chest and advancements
sync across ([Slabserver docs](https://slabserver.org/documentation/minecraft/tweaks/resource-world/)).
It exists "to alleviate item scarcity" while keeping the built world unscarred. Players accept a
wipe when the wiped place was never home.

**Nether as travel layer.** The Nether's 1:8 distance ratio turns it into a community-built
transit system: a Nether hub at spawn with themed highways to every major base
([SMPLive wiki](https://smplive.fandom.com/wiki/Nether_Hub),
[planetminecraft](https://www.planetminecraft.com/project/nether-hub-highway-design/)).
Travel infrastructure *is* content, and its upkeep cost is zero.

**Seasons (Hermitcraft model).** "With each new season the world is reset and moved to a brand
new seed"; resets align with major game updates, and new members join at season start
([Hermitcraft wiki](https://hermitcraft.fandom.com/wiki/Season_7)). Every season re-grows a
central **shopping district** (player shops, diamond currency) and server-wide games and
storylines ([S10 district](https://hermitcraft.fandom.com/wiki/Season_10_Shopping_District)).
What keeps people for years is almost entirely non-violent: mega-builds, shops, pranks,
player-made minigames, shared lore, and the *fresh start* that flattens veteran wealth.

---

## 6. Creativerse (Playful, 2014–)

**Structure.** Every player gets their own hosted, finite, generated world; others visit by
invitation or a public list; teleporters inside a world; blueprints and player-made
"adventures" as shareable content `[unverified details]`.

**What failed — the economics.** Despite "a solid playerbase and consistent monthly revenue",
it "was never profitable", specifically because Playful-hosted worlds "had very high maintenance
costs". In December 2022 it became a $24.99 "Definitive Edition", official worlds closed the
following April, and players migrated saves to self-hosting
([Massively OP, via search excerpt](https://massivelyop.com/2023/02/21/whatever-happened-to-creativerse-for-starters-it-went-fully-buy-to-play/)).
Lesson: persistent-world count must scale with revenue, not with signups.

---

## 7. Dragon Quest Builders 2 (Square Enix / Omega Force, 2018–19)

**Structure.** A persistent **home island** (Isle of Awakening) that you terraform and populate;
hand-made story islands; and **Explorer's Shores** — material islands "procedurally generated
each time the player embarks" ([Dragon Quest Wiki](https://dragon-quest.org/w/index.php?title=Explorer's_Shores&mobileaction=toggle_view_desktop)).
Four-player co-op on the home island ([Wikipedia](https://en.wikipedia.org/wiki/Dragon_Quest_Builders_2)).

**Transition.** A boat at the dock with an NPC captain; new destinations are bought with
gratitude hearts earned from your villagers `[unverified]`.

**Purpose on the islands — the best non-violent expedition loop in the cluster.** Each island
has two **scavenger-hunt checklists** (find and examine N kinds of block, plant, animal).
Completing a list grants an **infinite supply** of one basic material at home
([LadiesGamers](https://ladiesgamers.com/mini-guide-dragon-quest-builders-2-explorer-shores/)).
You can also bring back blocks, seeds, animals and even *residents* who move to your island
([DQB2 wiki](https://dqb-2.fandom.com/wiki/Explorer%27s_Shores)). The game rewards
*observation* (a naturalist's checklist) rather than extraction, and converts exploration into
permanently less grind.

---

## 8. LEGO Fortnite (Epic, Dec 2023–)

**Structure.** Private procedurally generated worlds, up to 8 players; the owner appoints **Key
Holders** who may enter and progress the world while the owner is offline
([LEGO](https://www.lego.com/en-us/themes/fortnite/playing-with-friends-in-lego-fortnite),
[Epic support](https://www.epicgames.com/help/c-202300000001626/c-202300000001693/how-to-share-worlds-in-lego-fortnite-odyssey-a202300000013391)).
Survival or sandbox rule-sets per world. Later: the Odyssey rename, separate destination islands
(Star Wars, Lost Isles) reached by vehicle `[unverified]`, and **Brick Life**, a social
role-play city mode ([Wikipedia](https://en.wikipedia.org/wiki/Lego_Fortnite)).

**Purpose.** Village building: raise village level, attract villagers, assign them jobs.
Fishing and vehicles were the first big updates (Feb/Mar 2024) — i.e. the non-combat verbs.

**Outcome.** 2.4 M concurrent at launch, ~340 k daily uniques later; IGN: "somewhat empty at
launch"; Epic says it would be "very successful" standalone
([Dexerto](https://www.dexerto.com/fortnite/epic-games-insists-lego-fortnite-rocket-racing-are-a-success-despite-lower-player-counts-3006171/));
the Expeditions mode was added June 2025 and later removed (Wikipedia). Lessons: key-holder
permissions solve "the world belongs to whoever is online"; a villager-driven settlement is a
strong cosy goal; an 8-player private world never becomes a society.

---

## 9. Hytale (Hypixel Studios, early access 13 Jan 2026)

Cancelled by Riot in June 2025, re-acquired by its founder in November 2025, launched in early
access two months later; >1 M players, 5,000+ mods by March 2026
([Wikipedia](https://en.wikipedia.org/wiki/Hytale)). The world "Orbis" is one procedurally
generated world divided into **zones** with their own biomes and factions (four shipped by July
2026). Exploration mode is combat-RPG-led; IGN noted a "general lack of direction". Relevant
parts:
- **Community servers host several worlds under one process ("universes")** with hub → world
  transfers, and a party system that keeps groups together "across server transfers and minigame
  queues" ([ZAP docs](https://zap-hosting.com/guides/docs/hytale-teleport-between-worlds/)).
- **Portal devices / Ancient Gateways** open *timed instance worlds*. Update 6 notes: worlds
  stay open while someone is inside, a gateway's close button has "a convenient failsafe that
  will prevent this button from working if someone is still on the other side", and time in a
  portal world is capped at what it started with
  ([patch notes](https://hytale.com/news/2026/5/pre-release-patch-notes-update-6)).
- **Server-side mods** delivered on join, no client install — same delivery model we plan.

---

## 10. Others that genuinely fit

**Eco (Strange Loop, 2018).** The only *designed-non-violent* game here: "does not have any
combat between players or monsters, and there is no way for the player's character to die"
([Wikipedia](https://en.wikipedia.org/wiki/Eco_(2018_video_game))). A small finite planet per
server, a ~30-day cycle ending in a meteor the community must stop without wrecking the
simulated ecosystem. Limited specialisation points mean "one player cannot be entirely
self-sufficient"; players mint currencies, write laws, tax and subsidise
([Strange Loop](https://strangeloopgames.com/eco/)). Proof that scarcity plus interdependence
plus governance sustains a server with zero combat — and that a finite world is what makes
resource decisions matter. Weak solo (Buffed.de via Wikipedia).

**Cube World (2019).** A pure warning: gear was **region-locked** — a 178-damage staff dropped
to 5.6 across a border. "Don't punish exploration in a game based on exploration"; the world
stopped feeling like one world and became "disconnected small levels"
([Steam](https://steamcommunity.com/app/1128000/discussions/0/1626286205708827654/),
[Wikipedia](https://en.wikipedia.org/wiki/Cube_World)).

**Vintage Story (official public server).** One long-lived world; claims "must never restrict
access to translocators or public roads"
([wiki](https://wiki.vintagestory.at/The_Official_Public_Server)) — travel infrastructure
protected as a commons by rule.

---

## Transferable ideas for a non-violent voxel MMO

1. **See-through gates where you can afford them** (Boundless). Even with a loading screen,
   render a live or recent *view* of the destination in the gate frame. With server-built LOD
   already streaming, a low-LOD snapshot of the arrival site is cheap. Make departure a place,
   not a menu.
2. **System-owned trunk, player-owned branches** (Boundless's failure + Nether hubs' success).
   Each super region gets a permanent, free, server-maintained harbour/station linking to its
   neighbours. Players build local spurs *inside* a region (roads, canals, rail, waystones) with
   zero upkeep. Never make inter-region reachability depend on volunteers paying fuel.
3. **A mobile home as the loading screen** (Starbound ship, Trove cornerstone). The vessel you
   travel in — boat, caravan, airship — is a small persistent decoratable interior, simulated
   trivially by either region server. The "loading screen" is time spent in your own cabin.
4. **Expedition regions on a calendar** (exoworlds, resource worlds, Explorer's Shores). Since
   generation is offline and expensive, *curate* a rolling set of temporary super regions —
   open 4–8 weeks, unclaimable, then retired — which focuses the population and lets the
   permanent regions stay unscarred.
5. **Naturalist checklists instead of kill quests** (DQB2). Per-region field guide: observe N
   rock strata, plants, birds, river features, ruins. Completion grants a permanent perk
   (a renewable trickle of a basic material, a seed, a recipe). Rewards looking at the expensive
   generated world rather than strip-mining it.
6. **Aesthetic exclusives as the trade good** (Boundless colours). Give each region a distinct
   palette of stone, timber, clay, dye and flower variants that fall out of its generated
   geology and climate. Functionally identical, visually unique → trade without power creep.
7. **Footfall / visitor rewards** (Boundless). Reward builders when others visit — guestbook
   stamps, reputation, small currency — so public, walkable places are the rational thing to
   build. Needs anti-alt rules.
8. **Prestige and civic titles** (Boundless). Score settlements on build quality and variety;
   the top settlement names the district and appears on the region map.
9. **Inhabitants who react to builds** (Starbound tenants, DQB2 residents, LEGO villagers).
   NPC residents whose type depends on what you built, who visibly use rooms, and who can be
   invited from other regions — giving travel a cosy purpose ("find a potter from the delta").
10. **Character-portable inventory + pocket storage** (Terraria). One character, full
    inventory, and a small personal stash reachable in every region. Never region-lock tools.
11. **Key holders and graded permissions** (LEGO Fortnite, Boundless beacons) for plots and
    settlements so group builds progress while the owner is offline.
12. **Parties survive transfers** (Hytale; Portal Knights' mistake). Group members may sit in
    different regions; following a friend is one click; show a region-qualified friend list.
    Never require a party to share a region.
13. **Interdependence through specialisation** (Eco). Limited profession slots plus regional raw
    materials make trade necessary. Optional lightweight governance per region: zoning,
    protected commons, road rights (Vintage Story's rule).
14. **Opt-in seasons, not wipes** (Hermitcraft). Permanent regions never reset; a separate
    "season" region with a theme, a fresh economy and a shared project runs 3–6 months and is
    then archived read-only as a museum you can still visit.
15. **Stewardship instead of regeneration** (Eco vs Boundless regen). In a frozen, authored
    world, replanting, restoring river banks and rewilding quarries are gameplay; expedition
    regions absorb the bulk extraction.
16. **Geographic gating instead of combat tiers** (Boundless atmospheres, re-themed). High
    alpine, desert, cave and open-sea regions demand preparation — clothing, provisions, a
    better vessel, a guide — a difficulty ladder with no enemies.
17. **Readable world geography** (Boundless skies). A world map on which super regions are
    adjacent places with coastlines that match, so travel has direction and neighbours matter,
    rather than a server list.
18. **Minigame venues as destinations** (Hermitcraft, Hytale). When mods arrive, player-built
    minigame venues live *in* the world as buildings you travel to, not in a lobby menu.
19. **Generated history as the explorer's endgame.** None of these games has deep generated
    lore; all struggle with "why explore another procedural place?" (Starbound). Offline
    generation can place ruins, old roads and place-names that span regions and reward the
    mapper archetype that kept the Boundless blogger playing.

## Warnings

- **World count must track population, not ambition.** Boundless ran ~50 planets × 100 slots
  for a few hundred players; every city felt dead. Open regions only when existing ones are
  busy, and design each 8–16 km region to feel inhabited at 30–50 concurrent.
- **Upkeep on travel is a slow-motion shutdown.** Fuel-burning portals made the transport
  network a chore owned by burnt-out volunteers. Upkeep on *anything* others depend on
  eventually strands newcomers.
- **Paid or rented private worlds drain the commons, and their expiry deletes beloved builds.**
  Creativerse shows hosted per-player worlds can sink a game with healthy revenue. If private
  regions ever exist, make them exportable files, not subscriptions to persistence.
- **Do not reset progression at a border** (Cube World). Differences between regions must add
  options, never subtract capability.
- **Do not force groups to share a region** (Portal Knights early access).
- **Infinite or disposable worlds have no sense of place** (Starbound planets, Trove adventure
  worlds). Finite, named, authored-feeling regions are the asset — do not dilute them.
- **Hubs that are menus are dead space** (Minecraft lobbies). A hub needs residents, shops and
  reasons to linger.
- **Veteran-newcomer gap.** Boundless's forged tools made newcomers feel useless next to
  veterans. Keep tool power flat; let mastery show in breadth, cosmetics and reputation.
- **Combat-sourced fuel for a builder's economy.** Boundless made its mostly peaceful players
  depend on hunts for Oort. In a non-violent game the equivalent trap is making one tedious
  activity the bottleneck for everyone else's fun.
- **"No direction."** Boundless, LEGO Fortnite at launch and Hytale's exploration mode all drew
  the same criticism. A sandbox still needs a legible medium-term ladder (field guides,
  settlement levels, civic titles, seasonal projects).
- **A dev lull is fatal for a small MMO.** The Boundless population followed patch cadence
  down. Favour systems that generate content from players and from the offline generator.

## Research gaps

Blocked or unavailable this session (not worked around): `boundless.fandom.com` (HTTP 402),
`massivelyop.com` (403 — the Boundless acquisition, Creativerse and Eco pieces were read only
through search excerpts), `trionworlds.com` (403), `forums.trovegame.com` (connection refused),
`twinfinite.net` (403), `fanlore.org` (403), `support.hytale.com` (403), and the PC Gamer
Starbound review (truncated). Consequently: exact public Boundless planet dimensions, exoworld
lifetimes in hours, Trove adventure-world lifecycle, the DQB2 boat/heart cost details, and
LEGO Fortnite island travel are tagged `[unverified]`. No first-party Boundless postmortem was
found; the decline analysis rests on community threads plus the acquisition announcement.
Hytale facts post-date my training knowledge and rest on Wikipedia and the official May 2026
patch notes only.
