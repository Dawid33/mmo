# Town ranks, town lands and civilisation territory — design (DRAFT)

Status: **approved by Dawid 2026-11-14** (the proposals marked as calls included). Written from a code and docs investigation and online Dwarf Fortress
research on 2026-11-14. **(D)** marks what Dawid asked for or decided; everything else is a proposal
by Claude, marked as a call or an open question. Nothing here is built.

## Intent

**(D)** Fortress mode spans many regions. A town has a **rank** set by its number of residents; a
higher rank lets the player expand the town into nearby regions and build there, the way Cities:
Skylines unlocks neighbouring tiles.

**(D)** Civilisations own **contiguous** land, never patches scattered over the world, and not all
land is owned: the rest is **wilderness**. A town founded inside a civilisation's borders belongs to
it. The civilisation's leader is the ruler of its largest town — as in Dwarf Fortress, where a
fortress that grows big enough becomes the capital and houses the monarch, and that can be the
player's. Founding one's own civilisation and breaking away come much later, with diplomacy.

**(D)** The ranks and the ruler's title at each rank, lowest first:

| # | Rank | Title | # | Rank | Title |
|---|------|-------|---|------|-------|
| 0 | Ruins | Keeper | 10 | County | Count |
| 1 | Homestead | Settler | 11 | Fiefdom | Lord |
| 2 | Hamlet | Elder | 12 | Fortress | Marshal |
| 3 | Village | Headman | 13 | Citadel | Protector |
| 4 | Settlement | Warden | 14 | City | Governor |
| 5 | Haven | Captain | 15 | Metropolis | Chancellor |
| 6 | Town | Mayor | 16 | Duchy | Duke |
| 7 | Enclave | Burgomaster | 17 | Principality | Prince |
| 8 | Stronghold | Castellan | 18 | Crownlands | Archon |
| 9 | Barony | Baron | 19 | Kingsland | King |

**(D)** Decisions taken on the first draft:

1. Land out of view is **not** kept live.
2. The top rank is at **1000 residents**, expecting roughly 40 residents per region at most.
3. A town that shrinks **loses land**.
4. Two towns' lands never touch: a **gap of a number of regions**, a named constant to tune.
5. Claiming land is **free**.

## How it works today

A played region is 256 m square; one world-map tile is one region; a super-region (the unit a host
owns) is 16 × 16 regions. Worlds are 63² to 392² regions.

**Fortress**

- **A fortress is a history `Site` with exactly one region** (`game/src/history/records.rs`,
  `Site.region`). The coordinator, the host (`site_regions`), the protocol (`SiteMarker.region`,
  `HeldFortress.region`, `EmbarkRequest.region`) and the client all copy that one region. Embark
  forbids two sites in a region and any living site within 4 regions (`EMBARK_MIN_DISTANCE_REGIONS`).
- **Residents are `Site.members`** in the one global history on the coordinator;
  `History::living_members(site)` is the count. Regions cannot compute it. The client gets it in
  buckets of 10 (`sites_key`).
- **Population is capped by one region's capacity** (`CAPACITY_BASE` 40 … `CAPACITY_MAX` 200): births
  slow and households leave when a site is crowded. A civilisation is capped at 2000
  (`CIV_POPULATION_CAP`).
- **A citizen outside the site's region stops being a resident** when that region parks or they cross
  into a region that is not running (`history/handover.rs` `walk_home`). Abstract residents are only
  ever embodied in `site.region`.
- **Nothing checks who may build where.** Orders go to the region the player's body is in, are
  region-local and are clipped to that region (`orders.rs` `clip_to_region`, `world_manager` routing
  by `homes[client]`). A fortress player can fly into any region and designate there.
- **A region runs only while a client subscribes to it or is homed in it.** The client subscribes to a
  3 × 3 square around the fortress region; every fortress tool (designate, tile mirror, working level
  and cut, overlay, minimap, People list, the boundary line) works on the home region alone.
- **There are no jobs, stockpiles, zones, workers, wealth, ranks, titles or nobles yet.**

**Civilisations**

- **A civilisation is `{id, race, name, sites}`** — no leader, capital, territory, colour or relation
  to any other civilisation. Names are placeholders ("Civ 3"). A civilisation never ends, merges or
  splits.
- **Nothing keeps a civilisation together on the map.** Bootstrap places 4 homes at least 10 regions
  apart and runs 100 years; crowded sites send embark parties to the best free region within 16
  regions (`chain.rs` `embark_target`, whose `civ` argument is unused). Two civilisations' sites can
  interleave.
- **The player picks a civilisation and a region independently**: nothing ties the region to the
  civilisation.
- **The player is not a person in the history.** A player is a `PlayerId` holding a
  `Claim::Fortress(SiteId)`; the site only records `player_founded: bool`. `Person` has no position
  or title.
- The world map draws a site as a square in one of 8 colours by civilisation id. There is no
  political layer.
- The history file is positional bincode at version 3; new fields need a version bump and an upgrade.

## What Dwarf Fortress does

From the DF wiki, forum and bug tracker (the wiki refused direct fetches; these are search summaries,
and the sources disagree between versions):

- A fortress climbs barony → county → duchy, offered by the civilisation and gated by population and
  wealth (older versions; one unverified report says newer versions count outlying holdings: 1, 4, 9).
- The **monarch moves in** once the fortress is of City rank (Metropolis in older versions), is a
  duchy and its people are happy enough. The fortress **becomes the capital**. The monarch arrives
  with a migrant wave and an entourage. Turning the capital into the Mountainhome is a further step.
- Succession is by heir; a vacant throne can pass to a fortress resident; a dead civilisation has no
  monarch.
- **DF draws no borders.** A civilisation is its sites, with site-to-site tribute; the map colours
  sites and marks holdings. Contiguous territory is this game's own design, not a copy.

Sources: <https://dwarffortresswiki.org/Monarch>, <https://dwarffortresswiki.org/index.php/DF2014:King>,
<https://dwarffortresswiki.org/Baron>, <https://dwarffortresswiki.org/Count>,
<https://dwarffortresswiki.org/index.php/DF2014:Duke>, <https://dwarffortresswiki.org/Noble>,
<https://steamcommunity.com/app/975370/discussions/0/3757725080159122395>,
<https://dwarffortressbugtracker.com/view.php?id=6490>, <https://dwarffortresswiki.org/Map_legend>.

What is taken from it: the capital is the biggest town and can be the player's; rank is a ladder
with titles. What is left out: wealth and happiness gates, an arriving monarch, the Mountainhome.

## Goals

- Rank is a pure function of resident count, decided on the server, the same for player and AI towns.
- A town owns a set of regions (its **lands**) that grows by choice within the rank's allowance and
  shrinks when the town does.
- Inside its lands a town works as one place: residents live anywhere in it, its holder designates
  anywhere in it and nowhere else.
- Every region is either one civilisation's **territory** or wilderness; a civilisation's territory
  is one connected area; a town's civilisation follows from where it stands.
- Each civilisation has a capital — its largest town — and its ruler leads the civilisation.

## Non-goals

- Founding a civilisation, breaking away, diplomacy, war, tribute, territory changing hands.
- Jobs, hauling and stockpiles themselves (but see "What the workers work must respect").
- Co-op on one town; nobles beyond one ruler per town; wealth or happiness as a gate.
- Keeping a town on one host.

## Anti-goals

- A second kind of region, or regions of variable size.
- Rank, lands or territory computed on a client or in a region.
- A town that silently loses residents because part of it parked.
- A civilisation in two pieces.

## Approaches

**Town over many regions**

- **A. One site, many regions (chosen).** `Site` keeps its founding region as the **seat** and gains
  `lands`. Each region stays its own sim; the coordinator's history holds the town together.
- **B. Satellite sites**, one `Site` per region with a parent. Rejected: residents moving inside one
  town would be emigrants, and every reader of a site would have to merge a family.
- **C. One big region.** Rejected: the region is a build constant and the threading unit.

**Civilisation territory**

- **T1. Stored per-region owner, grown by towns (chosen).** The history holds
  `territory: BTreeMap<RegionCoords, CivId>`. A town adds its **reach** — the regions it could ever
  claim — to its civilisation when founded; nothing is removed in this slice. Dawid's suggestion,
  made exact. Contiguity is a founding rule, so it holds by construction.
- **T2. Derived each time from site positions** (nearest seat within a radius). Nothing stored, but
  borders jump when a town is founded or dies, a town's own claimable area can shrink under it, and
  contiguity cannot be promised.
- **T3. Flood from civilisation homes at world forge** (Azgaar-style cost flood, as the 2026-11-01
  territories handoff proposed). Gives filled realms from year 0, but owns land no one lives near,
  leaves little wilderness, and does not follow what towns do during play. It can come back later as
  the *initial* territory; T1 does not exclude it.

## Design

All numbers below are named constants in `history/rules.rs`, placeholders to tune.

### 1. Rank

- `game::history::TownRank`: the 20 ranks with `name()`, `title()`, `TownRank::of(residents)`.
- The coordinator computes it from `living_members(site)` for every site. `SiteMarker` gains `rank`;
  `sites_key` includes it, so a rank change is always broadcast.
- Rank is reviewed once a month per site, so a single birth or death does not flip it within the month.
- A rank change is a history log entry (`RankChanged { site, from, to }`) and a notice to the holder.
- Ruins is a site with no living residents (today: abandoned).
- UI: the Governance line becomes "Hamlet of <site> — Elder <player>" with residents and "lands 2 / 3";
  the status line announces a rank change. `docs/game-design.md` §7.1's fixed name "the Warden of
  <site>" is replaced by the rank title (Warden is rank 4).

Table **(D: 1000 at the top, about 40 residents per region)**. Each threshold fits in the lands of
the rank before it at 40 per region; the last three ranks need every region full, so they are for
good land only:

| Rank | Residents ≥ | Lands | Rank | Residents ≥ | Lands |
|------|-------------|-------|------|-------------|-------|
| Ruins | 0 | 1 | County | 170 | 8 |
| Homestead | 1 | 1 | Fiefdom | 220 | 9 |
| Hamlet | 10 | 1 | Fortress | 280 | 10 |
| Village | 20 | 2 | Citadel | 350 | 12 |
| Settlement | 30 | 2 | City | 430 | 14 |
| Haven | 45 | 3 | Metropolis | 520 | 16 |
| Town | 60 | 4 | Duchy | 620 | 18 |
| Enclave | 80 | 5 | Principality | 720 | 21 |
| Stronghold | 100 | 6 | Crownlands | 840 | 25 |
| Barony | 130 | 7 | Kingsland | 1000 | 25 |

Consequences to tune with it: region capacity today is 40 … 200, so `CAPACITY_MAX` comes down to
about 40 (this changes how fast AI sites crowd and spread, so the bootstrap has to be looked at
again); `CIV_POPULATION_CAP` (2000) would let one Kingsland take half its civilisation's people, so
it rises or scales with territory.

### 2. Civilisation territory and wilderness

- `History.territory: BTreeMap<RegionCoords, CivId>`. A region not in it is wilderness.
- A town's **reach** is every region within `TOWN_REACH_REGIONS` (6, Chebyshev) of its seat that is
  wilderness or its own civilisation's and is joined to the seat through such regions by shared
  edges. (A 13 × 13 square when nothing is in the way: 169 regions for at most 25 lands.) The reach
  must be larger than the seat spacing (`EMBARK_MIN_DISTANCE_REGIONS`, 4): with a reach of 3 no
  region inside any border would be far enough from a town to found another, and "a town founded
  inside a civilisation's borders belongs to it" could never happen.
- **Founding a town** adds the wilderness part of its reach to its civilisation. Territory is never
  removed in this slice — a ruined town's land stays its civilisation's — so borders only grow.
- **Where a town may be founded** (player and AI alike; this replaces "any empty land"):
  - seat inside civilisation X's territory → the town belongs to X **(D)**; the party must come from X;
  - seat in wilderness → allowed only on the chosen civilisation's **frontier**: the seat's reach must
    touch that civilisation's territory, so the new land joins on;
  - anything else is refused ("too far from <civ>'s borders", "that is <civ>'s land"). **Open:** deep
    wilderness stays closed until founding a civilisation exists.
  - The seat spacing (`EMBARK_MIN_DISTANCE_REGIONS`) and the town gap (§3) still apply.
- **Contiguity** follows: the first site's reach is connected; every later reach is connected and
  overlaps or touches what the civilisation has. `embark_target` gets a real `civ` argument and
  searches the territory and its frontier only.
- Embark UI: the civilisation is picked first as today; the map then darkens every tile that
  civilisation cannot found on; a tile inside another civilisation's borders is refused with a line
  naming the civilisation to pick instead.
- World map: a **political** layer — territory filled in the civilisation's colour, borders drawn,
  wilderness left plain, the capital marked. Sent as `ServerPacket::Territory` (the owned regions as
  a list — territory exists only around towns, a few thousand regions) on connect and when it grows.
- Existing worlds: the upgrade replays sites in founding order to assign territory. A world forged
  under the old rules can come out with a civilisation in pieces; contiguity is promised only for
  worlds whose history is bootstrapped after this lands.

### 3. Town lands

- `Site.lands: BTreeSet<RegionCoords>`, always containing the seat; the upgrade sets `{region}`.
- **Claiming** **(D: free)**: `ClientPacket::ClaimLand(RegionCoords)` → `WorldRequest::ClaimLand` →
  the coordinator checks: the sender holds `Claim::Fortress(site)`; `lands.len()` is below the rank's
  allowance; the region is in the town's reach and its civilisation's territory; it shares an edge
  with the lands; it is in no town's lands; it is more than `TOWN_GAP_REGIONS` (2) from every other
  town's lands **(D)**. Memoised by `RequestId`.
- **Shrinking** **(D: land is lost)**: when the allowance falls below the lands held, the town is
  *overextended*. **Call:** it has `LAND_RELEASE_GRACE_DAYS` (one season) to grow back or for the
  holder to give land up (`ClientPacket::ReleaseLand`); after that the most recently claimed region
  that leaves the lands connected is released, one per month until the town fits. A town that falls
  to Ruins gives up everything but its seat at once. The grace stops a
  single death at a threshold from costing a district. Released land stays the civilisation's
  territory; what was built stays in the world; its residents walk to the seat. The seat is never
  released.
- `Site.capacity` is the sum of `grid.capacity` over the lands.
- **AI towns use the same rules**: a crowded AI town with allowance left claims the best free
  neighbouring region before it sends anyone away. Without this an AI town stops at one region
  (about 40 residents) and every capital would be a player's.
- World map: lands tinted and outlined inside the territory; claimable regions marked for the holder.
  In the world: the boundary overlay draws the lands' outline (its shader uniform holds one rectangle
  today).

### 4. Capital and leader

- Every town has a **ruler** in the history: a resident person — the eldest adult, succeeded on
  death by their eldest resident adult child, else the eldest adult (`Site.ruler: Option<PersonId>`,
  `RulerChanged` log entry). The title is the town's rank title. A player holding a town is shown as
  its ruler on their own screen ("You are its Elder"); other players see the resident ruler's name
  until accounts expose player names. This is the one piece of "nobles as people" in this slice; it
  exists so an AI capital's leader has a name.
- `Civ.capital: Option<SiteId>` — the civilisation's largest town by residents. **Call:** it moves
  only when another town exceeds the capital by `CAPITAL_MARGIN_PER_MILLE` (100) or the capital falls
  to Ruins, so two towns of equal size do not trade the seat every month. `CapitalMoved { civ, from,
  to }` is a log entry and a notice to every holder in the civilisation.
- The capital's ruler is the civilisation's leader, shown as "<title> <name> of <town>, leader of
  <civ>" in the Governance tab, on the world map and in Legends. A King needs both the largest town
  and 1000 residents; the leader of a small civilisation may be a Mayor.
- **Call:** in this slice the capital is a title and a mark on the map, nothing more. What it lets
  the leader do arrives with diplomacy.
- `CivMarker` gains `capital` and the leader's line.

### 5. Residents across the lands

- "At the site" means "in any region of its lands". `walk_home`, `settle`, `person_crossed` and
  `region_parked` keep membership for a citizen in a land region.
- `Location::AtSite` gains the region; `embody_abstract_residents` / `embody_site` embody the people
  whose region is the one starting; migrants arrive at the seat's landing.
- Notices about a site go to the client that holds it (`fortress_of`), not to clients homed in
  `site.region`.

### 6. Which regions run **(D: not all of them)**

The client subscribes to the camera's 3 × 3, clipped to lands plus a one-region ring, plus the seat.
Land out of view parks; its residents go abstract, as the history already does for any parked
region, and come back embodied where they were. Cost is what it is today at every rank. Nothing
physical happens in the part of town that is not being looked at.

### 7. Orders anywhere in the lands

- Orders and designations are addressed by **world tile**. The client splits a dragged box at region
  edges; the server routes each part by the region it names, not by the player's home (`TODO.md`
  already asks for this).
- **The permission check that is missing today:** the manager refuses an order, a designation or a
  creative edit from a fortress-mode client unless the target region is in the lands of the site the
  client holds; intents get a "not yours" refusal.
- Client: tile mirror, working level and cut, designation overlay and minimap become per-region over
  the subscribed land regions; the designate cursor is clamped to the lands; the People tab lists the
  site's residents. **Call:** one world-height working level for the whole town.

### 8. Several hosts

Lands may cross a super-region edge and so sit on two hosts. History, territory, residents and rank
are host-independent already. Missing: an observer session cannot give orders, so an order for land
on another host is forwarded by the home host over the peer link and checked there against the same
lands. Done last.

### What the workers work must respect

A job belongs to one region; anything it needs from another region travels as a transfer or an
intent; a town-wide view (stocks, the job list, labours) is assembled from per-region parts by the
coordinator or the client; nothing may assume a town is one region or one tile space.

## Order of work

Each piece lands whole and is playable on its own.

1. **Rank** — enum, table, marker, log entry, notice, Governance line; capacity retuned.
2. **Order permission** — the ownership check, with lands = the one region. Closes today's hole.
3. **Territory and wilderness** — the per-region owner, reach at founding, the founding rules for
   player and AI, the political map layer. History format bump.
4. **Town lands** — `ClaimLand` / `ReleaseLand`, gap, shrink and grace, capacity sum, AI claims, map
   and boundary drawing. After this a town owns land but still only builds at the seat.
5. **Capital and leader** — rulers, the capital rule, markers and notices.
6. **Residents across the lands.**
7. **Orders by world tile** and the per-region client tools. The largest piece.
8. **Camera-following window**, then multi-host order forwarding.

Pieces 3 and 4 both change the history format; landing them close together means one upgrade.

Tests go where the rules live: rank, reach, founding, claim, release and capital rules as `history`
unit tests, plus one property test that every civilisation's territory is connected after a
bootstrap; park/cross membership in the handover tests; routing and refusal in the `world_manager`
tests; a seeded two-region town in `game/tests/handoff_schedule.rs` (`just test-threads`).

## Decided on the second draft (D, 2026-11-14: "go with your recommendations")

1. Deep wilderness is closed to embarks until founding a civilisation exists.
2. Reach is a fixed radius around the seat (`TOWN_REACH_REGIONS`; proposed as 3, set to 6 while
   planning — see §2).
3. Losing land: a season's grace, then the newest region that leaves the lands connected goes.
4. The capital is only a title and a map mark for now.
5. AI towns get a named ruler from their residents.

Implementation plans: `docs/superpowers/plans/2026-11-14-town-ranks-lands-territory.md` (history,
coordinator, map: order-of-work pieces 1, 3, 4, 5) and
`docs/superpowers/plans/2026-11-14-town-lands-play.md` (sim and client: pieces 2, 6, 7 and the
camera window of 8). Forwarding orders to land on another host is left for its own plan once the
multi-host go/no-go is in; until then land on another host cannot be ordered.
