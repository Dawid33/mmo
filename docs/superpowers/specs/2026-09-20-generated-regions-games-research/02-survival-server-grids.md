# Survival / sandbox MMOs with generated maps, server grids, transfers and seasons

Research date: 2026-09-20. Cluster 02 of the generated-regions research. Scope: shipped multiplayer
games whose worlds are large generated regions separated by a loading screen or server change, read
for what a **strictly non-violent** voxel MMO with offline-generated 8–16 km "super regions" can
reuse. Claims I could not confirm in a source this session are tagged **[unverified]**.

Fetch gaps (sites that refused the fetch; not worked around): massivelyop.com (403), gamesbeat.com /
venturebeat Eco article (403), forbes.com Worlds Adrift interview (403), wiki.play.eco (403); PC Gamer
and Steam news pages returned navigation chrome only. Where these are cited, the content comes from
search-result excerpts, not the full article.

---

## 1. Wurm Online — islands you sail between (deep)

**Structure.** Each server is one finite island with its own heightmap; ~17 servers since 2006
([arXiv go-along study](https://arxiv.org/html/2405.10208v1)). Sizes: 2048² tiles (most), 4096²
(Independence, Harmony, Cadence), 8192² for Xanadu = 32×32 km
([Server types](https://www.wurmpedia.com/index.php/Server_types),
[Xanadu](https://www.wurmpedia.com/index.php/Xanadu)). Maps are generated once and then frozen;
every tile is terraformable and changes persist forever. No wipes on the main clusters; one side
server (Jackal) is explicitly seasonal and resets when its challenges complete.

**Transition.** The diegetic frame is the sea: sail off the north edge and you arrive at the south
edge of the neighbour. Everything on the boat crosses — skills, inventory, bank, passengers (who must
be logged in); animals need cages. "Plot Course" lets a boat reach any server in the cluster, landing
on the far side from the departure island. Portals are the cheaper alternative and are deliberately
worse: items and vehicles stay behind, 10-minute cooldown
([Server travel](https://www.wurmpedia.com/index.php/Server_travel)). So: *bulk moves by ship, people
move by portal* — the same split Valheim later made famous.

**Purpose.** 100+ use-based skills, slow crafting with quality levels, and terraforming. Long-term
retention is infrastructure: highways protected by waystones, tunnels through mountains, canals,
multi-month underground complexes
([MMORPG.com highways](https://www.mmorpg.com/wurm-online/news/build-your-own-ventura-highway-after-latest-update-2000096572)).
Combat exists (PvE creatures; PvP confined to Chaos/Epic/Defiance servers) but the PvE clusters run
as a crafting society.

**Region differences / lifecycle.** Renewal is by *decay*, not wipes: a deed (settlement claim) costs
a per-tile fee plus monthly upkeep; with 30+ days in the coffers nothing decays, and abandoned
structures decay fast so land reopens
([Settlement](https://www.wurmpedia.com/index.php/Settlement),
[Decay](https://www.wurmpedia.com/index.php/Decay)). The ruins are a feature: the arXiv study
documents players touring overgrown deeds of departed friends as the world's history.

**Social.** Deeds with mayors, roles and permissions; alliances; a player economy of merchants and
mail. "Impalongs" — player-run festivals where high-skill crafters improve everyone's gear for free —
draw ~100 people ([Impalong](https://www.wurmpedia.com/index.php/Impalong)). Griefing control on PvE
is the deed permission system plus GMs.

**Worked / failed.** Worked: permanence + decay, sailing as transfer, player-built infrastructure as
endgame. Failed: **server sprawl**. The 2020 Steam launch added an isolated Northern cluster ("cut
off from all other servers"), splitting the population; devs later conceded "too many servers is
detrimental" yet opened another because free land only remained in undesirable biomes
([Steam announcement thread](https://steamcommunity.com/app/1179680/eventcomments/5188663423651414102)).
Land pressure and population density pull in opposite directions.

## 2. Eco — a finite planet with a deadline (deep)

**Structure.** One server = one small generated planet, default 0.52 km² (fine for ~10 players),
scalable; hosts are warned that a big world with few players "spreads everyone so far apart that no
economy forms" ([DoomHosting guide](https://www.doomhosting.com/blog/eco-dedicated-server-setup-guide-2026)).
A meteor hits in 30 real days unless players build the tech to destroy it. No inter-server travel.

**Purpose.** Entirely non-violent except hunting. Players specialise (skill points are scarce and a
server "collaboration" setting makes them scarcer), so nobody can self-supply; they open shops, mint
currencies, and must reach lasers without wrecking the simulated ecosystem — pollution, deforestation,
extinction, sea-level rise are all modelled
([Wikipedia](https://en.wikipedia.org/wiki/Eco_(2018_video_game))).

**Social.** The laws system is the standout: if/then laws proposed and voted on, then *enforced by the
server* (taxes, permits, bans on actions per area). Update 10 (Dec 2023) replaced the single world
government with nested **towns → countries → federations**, each with treasury, laws and an
"influence" radius driven by a culture score; three people can found a town
([Eco 10 announcement](https://eco-servers.org/blog/194/towns-countries/)).

**Lifecycle.** The meteor converts a sandbox into a season with an ending. Hosts are told to publish
season length and wipe criteria before launch.

**Worked / failed.** Worked: laws-as-code, enforced interdependence, ecological consequence as the
antagonist. Failed: retention *inside* a cycle. Community reports say most servers are dead within a
week; latecomers find a few no-lifers already supplying everything, so they cannot contribute
meaningfully; the meteor turns the game into a race and burns people out
([Steam feedback thread](https://steamcommunity.com/app/382310/discussions/8/1744516481201589863/)).
A group playing two evenings a week reaches impact barely industrialised, "which is how a lot of first
Eco servers quietly die" (DoomHosting). GamesBeat's piece is titled "We stopped the meteor, but we
couldn't stop ourselves" — the social/ecological failure outlives the win condition (article body
not fetched).

## 3. A Tale in the Desert — finite "Tellings" (deep)

**Structure.** One hand-authored Egypt, no combat at all, running since 2003. The world *ends*: a
Telling lasts roughly 1.5–2.5 years (T1 Feb 2003–Sep 2004, T2 627 days, T3 926 days), then
everything wipes and a new Telling starts with revised rules
([Wikipedia](https://en.wikipedia.org/wiki/A_Tale_in_the_Desert)). Population has always been a small
niche; low thousands at peak **[unverified]**.

**Purpose.** Seven disciplines of **Tests** — Architecture, Art, Body, Harmony (replaced Conflict in
T3), Leadership, Thought, Worship — many of which are social or judged by peers: build a sculpture
others rate, design a puzzle others solve, mentor newcomers, coordinate a multi-person ritual. At the
end of a Telling each discipline raises a monument, and an "Oracle" who passed all its tests designs a
new Test that developers implement next Telling
([Codex Gamicus](https://gamicus.gamepedia.com/A_Tale_in_the_Desert), via search excerpt). Only the first
Telling was fully "won".

**Social.** Player petitions become laws that developers actually code. Elected Demi-Pharaohs can
permanently exile up to seven players — real, controversial power. Developer-run provocations (a
hostile trader NPC, a clear-cutting skill that caused deforestation and then regulation, a plague)
gave the legal system something to chew on
([Engadget Game Archaeologist](https://www.engadget.com/2011-09-13-the-game-archaeologist-spins-a-tale-in-the-desert-the-highlight.html)).

**Worked / failed.** Worked: endings make achievements legible ("what did *this* generation build?"),
and legacy crosses the wipe as *rules and monuments*, not loot. Failed: subscription with a 24-hour
trial kept it tiny; one case of a player monopolising mines was answered by counter-griefing rather
than law, showing that legal tools need to be faster than frustration.

## 4. Valheim

Seeded procedural world, fixed 10 km radius disc, biome rings as progression
([world size guide](https://gamequery.blog/how-big-is-valheim-world)). Characters (inventory + skills)
are stored client-side and walk into any world
([Steam discussion](https://steamcommunity.com/app/892970/discussions/0/3731827313781887126/)) — great
for friends, but it lets anyone import cheated goods, and community servers rely on honour rules or
mods. The portable lesson is **portals that refuse metal**: people teleport, ore must be sailed, so
"the constraints of said portals necessitate exploring more of the world" and the map keeps its scale
([MMORPG.com editorial](https://www.mmorpg.com/editorials/valheims-smartest-design-choice-isnt-the-survival-elements-its-the-portals-2000121172)).
Combat-gated bosses are the violent part; sailing, logistics and building are not.

## 5. Last Oasis

Each oasis is a separate server (~100 players **[unverified]**) on a hex world map. Tiles spawn on one
edge and expire after ~3 weeks as the sun "burns" them; players pack their base onto a walking machine
and migrate. Travel costs water carried in the walker's hold, which caps range; offline players are
auto-migrated with designated walkers when an oasis closes
([search summary of wiki/MassivelyOP](https://lastoasis.fandom.com/wiki/Environment)). Sandstorms sweep
each map every 1–2 hours and erode anything unprotected, "cleansing" abandoned builds
([MMOs.com](https://mmos.com/news/last-oasis-deep-dives-into-its-new-travel-mechanics-and-destructive-sandstorms)).
Launch collapsed: the burn/expiry and private-server features were "heavily intertwined" with the
server failures, and 20k players rejoining at once cascaded
([VideoGamer](https://www.videogamer.com/news/last-oasis-developer-explains-what-went-wrong-with-the-servers/)).
By Season 5 the devs **removed the burn**: a home-base game made forced migration "mostly just
frustrating"; only occasional, pre-announced oasis deletions for population balance remain
([MMOs.com](https://mmos.com/news/last-oasis-burn-mechanic-season-5)). Its PvP clan dominance is the
part to discard.

## 6. Dune: Awakening

Persistent hand-built Hagga Basin plus a Deep Desert that a Coriolis storm wipes and re-rolls every
Monday; zones are joined by an **overland map** you enter by flying an ornithopter over the Shield
Wall ([Game Rant](https://gamerant.com/dune-awakening-coriolis-storm-deep-desert-server-wipe-explained/)).
Fog of war returns each week, so cartography is a repeatable job. The mistake: the best resources sat
in forced PvP. Funcom admitted 80%+ of players only ever do PvE and reworked the endgame so PvE
players are not "cut out"
([creative director letter](https://duneawakening.com/news/a-message-from-the-creative-director-on-pve-and-pvp/),
[PC Gamer](https://www.pcgamer.com/games/mmo/dune-awakening-is-changing-its-endgame-in-response-to-player-complaints-our-goal-is-not-to-force-pve-players-to-interact-with-a-pvp-system-that-they-may-have-no-interest-in/)).
Keep: permanent home + ephemeral frontier. Drop: contested scarcity.

## 7. Rust

Procedural map per server, forced wipe the first Thursday of each month with Facepunch's update;
blueprint (knowledge) wipes are rarer and separate from map wipes. Wipe day is the monthly population
peak ([wipe guide](https://minestrator.com/blog/post/rust-server-wipe-guide-map-blueprint-force-wipe-2026)).
Violent to the core, but the cadence lesson is clean: a predictable calendar reset tied to a content
patch is an event, and separating *world* reset from *knowledge* reset softens loss.

## 8. Once Human

Six-week-ish **[unverified length]** "scenario" servers with phases; blueprints, cosmetics and
currencies persist, and materials move to **Eternaland**, a private persistent island
([Game Rant](https://gamerant.com/once-human-season-reset-what-carries-over/)). Backlash to wipes
first produced Eternaland, then in March 2025 removed forced wipes: finished scenarios convert to
non-shutdown servers and players choose to stay, go home, or sign up for a new scenario
([PCGamesN](https://www.pcgamesn.com/once-human/server-wipe)). Builders hate losing builds; opt-in
seasons survived, mandatory ones did not.

## 9. Ark: Survival Evolved / Ascended

Hand-made maps clustered; you "upload" your survivor, 50 item stacks and 20 creatures at an obelisk
and download on another map; creature inventories are dropped; some maps restrict what can be
imported (Aberration), PvE↔PvP transfer is blocked
([CrossARK Transfers](https://ark.fandom.com/wiki/CrossARK_Transfers)). Map-exclusive resources and
creatures make every map worth an expedition; import whitelists protect a map's identity.

## 10. Atlas

A 15×15 grid of servers sailed between seamlessly, sold as 40,000 concurrent players
([Wikipedia](https://en.wikipedia.org/wiki/Atlas_(video_game))). Botched launch, exploits and
rollbacks aside, the structural failure was dilution: official PvE ended up with ~4–12 players per
cell, and the grid shrank to 11×11, then 9×9 PvP / 6×6 PvE with repeated wipes
([Atlas wiki](https://atlas.fandom.com/wiki/Map),
[MMORPG.com](https://www.mmorpg.com/news/atlas-will-wipe-servers-ahead-of-new-season-bring-back-bigger-pvp-maps-and-xbox-only-server-2000127492)).
Most cells were ocean and near-identical islands, so crossing a border rarely meant arriving anywhere.

## 11. Nightingale — players author the region (deep)

**Idea.** At a portal you play a **Biome card** (forest/desert/swamp) plus a **Major card**
(Antiquarian, Astrolabe, Provisioner, Herbarium, Hunt, Gloom — sets theme, points of interest,
difficulty); the generator builds a realm from that pair. Inside, **Minor cards** played at a
transmuter rewrite the rules live: low gravity, perpetual day, 2× crop yield, 1.5× harvest, comfort
bonuses ([wiki](https://nightingale.wiki.gg/wiki/Realm_Cards),
[PCGamesN](https://www.pcgamesn.com/nightingale/realm-cards)). Cards are crafted/earned, so the deck
*is* the progression. Your home ("respite") sits in a persistent Abeyance realm that you can
deliberately reset and re-roll; six players per realm, friends link realms with crafted player cards
([Steam threads](https://steamcommunity.com/app/1928980/discussions/0/4348859098832617730/)).

**Failed.** Three biomes × a few majors felt samey; reviews called it overcomplicated with "nothing to
do" ([Game8](https://game8.co/articles/reviews/nightingale-review)); min-maxers swapped minor cards
per chore (fishing card, potion card), turning wonder into a buff menu; a bug deleted realms and
houses ([MMOs.com](https://mmos.com/news/nightingale-disappearing-realms-fix)). "Realms Rebuilt"
(Sept 2024) added handcrafted story realms because procedural ones could not carry narrative or
small atmospheric detail, keeping procedural "Untamed" realms for resources
([MMORPG.com](https://www.mmorpg.com/news/nightingales-realms-rebuilt-update-is-live-with-new-campaign-unique-realms-crafting-overhauls-and-much-more-2000132795)).

## 12. Pax Dei

Identical hand-built shards, each holding ~14,000 plot owners; building is only allowed in "home
valleys" of heartland provinces ([FAQ](https://playpaxdei.com/en-us/faq/world)). Plots are rented with
monthly tokens (subscription); lapsed plots freeze for three days, then release
([MMORPG.com](https://www.mmorpg.com/news/pax-dei-introduces-paid-plot-tokens-to-claim-and-maintain-land-phases-out-launcher-before-steam-pivot-2000136141)).
Non-combat crafting/building is the strongest part; PvP zones gate top resources. Land as a paid
recurring entitlement solves ghost towns but reads as rent.

## 13. Worlds Adrift (postmortem)

Floating islands in one SpatialOS world; islands were partly **player-made** in a standalone
Island Creator (hundreds of them **[unverified]**); physics-built airships ([Wikipedia](https://en.wikipedia.org/wiki/Worlds_Adrift)).
Shut July 2019: player numbers were "roughly a third" of the worst case; staggered sales to protect
servers killed word of mouth; repeated progress wipes repelled players; open PvP let veterans farm
newcomers at the start islands and the studio defended it as a "pillar" until too late, later calling
the delay of PvE servers a bad decision (Forbes interview via search excerpt;
[MMORPG.com](https://www.mmorpg.com/worlds-adrift/news/bossa-studios-working-to-improve-early-experience-to-prevent-griefing-1000048564)).
Bespoke distributed-sim hosting cost is widely blamed too **[unverified]**.

---

## Transferable ideas for a non-violent voxel MMO

1. **The crossing is a voyage, not a menu (Wurm, Dune).** Put the loading screen behind a ship leaving
   the map edge or a flight over a boundary range; arrive on the matching edge of the neighbour. Our
   finite offline-generated regions already have edges — give them coasts or passes.
2. **Two channels: people cheap, cargo expensive (Wurm portals vs boats, Valheim ore).** Portals move
   the character only; bulk materials need a vessel and a route. Keeps 8–16 km meaningful and creates
   the haulier/shipwright profession.
3. **Region-exclusive geology (Ark, Wurm).** Offline generation with real geology can give each super
   region a genuine mineral/flora/climate signature. Trade between regions becomes the economy, and
   the loading screen becomes a customs border.
4. **Import whitelists per region (Ark Aberration).** A region can refuse certain imports to protect
   its identity or a "fresh start" ruleset, without splitting accounts.
5. **Decay + upkeep instead of wipes for home regions (Wurm, Pax Dei).** Claims cost upkeep; lapsed
   claims freeze, then decay. Voxel edits outside claims could slowly weather back toward the frozen
   generated baseline — we have that baseline for free.
6. **Ruins as history (Wurm).** Let decay be slow and visible. Abandoned settlements are content in a
   contemplative game; consider an "archive" marker so notable ruins are preserved rather than erased.
7. **Permanent homeland + ephemeral frontier (Dune, Once Human, Wurm's Jackal).** Most super regions
   never reset. A few "expedition" regions are regenerated on a calendar with a new seed and full fog
   of war; mapping, surveying and first-settlement are the repeatable activities. Nothing essential
   lives only there.
8. **Seasons that end with a monument (ATITD).** An expedition region closes with a tally and a
   permanent artefact in the homeland recording what that generation built; top contributors shape
   the next season's rules.
9. **Separate world reset from knowledge reset (Rust BP wipes, Once Human).** If anything resets, it is
   terrain and buildings; recipes, blueprints of your builds, cosmetics and reputation persist.
   Exportable build blueprints soften every loss.
10. **Laws as executable code (Eco, ATITD).** Per-settlement if/then rules the server enforces: who may
    dig where, felling quotas, taxes, zoning. In a game without violence this *is* the conflict system
    and the griefing control.
11. **Nested polities with influence radius (Eco 10).** Town → region council → federation, with
    jurisdiction growing from activity. Maps neatly to settlement → super region → world.
12. **Enforced interdependence (Eco, Wurm impalongs).** Scarce specialisation plus quality tiers make
    master crafters socially valuable; support free "improve-along" festivals as first-class events.
13. **Ecology as the antagonist (Eco).** Over-harvest and pollution with simulated consequences give
    stakes without enemies. Offline climate/hydrology data makes this credible; keep it regional and
    recoverable rather than a 30-day doomsday.
14. **Player-authored expedition regions (Nightingale, Worlds Adrift Island Creator).** Let a guild
    spend crafted "charts" (climate, geology, age, a quirk modifier) to commission a region; since
    generation is offline and expensive, the wait (days) is a feature — a ship being outfitted.
    Avoid per-chore modifier swapping: modifiers are fixed at commissioning.
15. **Infrastructure as endgame (Wurm).** Highways, tunnels, canals, ports and lighthouses that make
    the 2 km view distance pay off; protect public works with a special claim type.
16. **Auto-migration for the absent (Last Oasis).** If any region ever closes, offline players' packed
    belongings move automatically to a safe harbour. Never let a lifecycle event punish not logging in.
17. **Show population on the world map (Last Oasis S5).** Travellers see where people are, countering
    dead-region drift.
18. **Open regions only under land pressure (Wurm, Atlas inverse).** Add a super region when claimable
    *desirable* land runs out, not on a marketing beat; make every biome desirable enough to settle.

## Warnings

- **Population dilution kills server grids.** Atlas (4–12 players per cell) and Wurm's isolated
  Northern cluster show that capacity is not the constraint — density is. One process per super
  region means every new region divides the crowd. Start with few, connect all of them, never launch
  a disconnected cluster.
- **Forced migration and forced wipes lose to builders.** Last Oasis removed its signature burn; Once
  Human removed mandatory wipes; Worlds Adrift's progress wipes drove players off. Expiry must be
  opt-in, scheduled, and never touch a player's home.
- **Deadlines burn out and lock out latecomers.** Eco servers die in a week when early grinders supply
  everything. Any seasonal region needs catch-up roles and work that scales with arrivals.
- **Best resources behind a gate most players refuse** (Dune: 80% never touched PvP). Our analogue:
  do not put essentials behind competitive minigames or hardcore expedition regions.
- **Procedural sameness.** Nightingale's card combinatorics and Atlas's islands produced regions that
  differed on paper only; Nightingale had to add handcrafted realms for story. Unlimited offline CPU
  must buy *legible* differences — landmarks, named rivers, generated history — not more noise.
- **Rule-modifier min-maxing.** Swappable world modifiers become a buff menu. Fix them per region.
- **Client-held or freely transferable inventories break economies** (Valheim). Transfers must be
  server-authoritative, with per-region import rules.
- **Land rent feels punitive when it is real money** (Pax Dei). Pay upkeep with in-game activity.
- **Player justice can become exile** (ATITD Demi-Pharaoh bans). Give laws teeth over *actions and
  places*, not over a person's access to the game; and make legal remedies faster than retaliation.
- **Lifecycle features multiply technical risk.** Last Oasis's launch failure was tied to the
  expiry/migration machinery and a thundering-herd rejoin. Ship region transfer first, region expiry
  much later, and load-test mass rejoin.
- **Realm-loss bugs are fatal to trust** (Nightingale). Persistence of player builds is the product.
