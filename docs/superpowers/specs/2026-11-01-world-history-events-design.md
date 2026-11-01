# World history: the world event manager — design

Status: draft for review, 2026-11-01. Next step after approval: an implementation plan
(`superpowers:writing-plans`).

Related:
- `docs/superpowers/plans/2026-11-01-civilizations-territories-handoff.md` — the society layer
  (civs, territories, religions) this design is the engine for.
- `docs/superpowers/specs/2026-10-20-dwarf-fortress-research/14-history-civilizations-religion.md`
  — how DF does history (summarised to the user in this session).
- `docs/superpowers/specs/2026-10-20-dwarf-fortress-research/09-population-and-outside-world.md`
  — migrants, the DF fortress-side coupling.
- `docs/superpowers/specs/2026-11-01-df-scale-world-terrain-design.md` — the world it lives on
  (not built yet).

## Intent and why

DF runs history as a zero-player strategy game during worldgen, then plays a different game in
fortress mode; the two meet only at a few seams (migrants, caravans, sieges). We want **one
world, seen at two levels of detail**. The user's rule, for this feature and every future one:

> **Every event that changes the world must be an event that can also happen, and be
> triggered, in fortress play.**

Why (user): the world feels alive and the player is part of it. A fortress's own people can grow
dissatisfied and leave to found their own fortress, or go back to family; a letter from a sick
relative arrives by a real courier. History is not something that happened *before* the game —
it is what keeps happening, and the player's fortress is one of its places.

What the user described, in their words where it matters:
- An abstract **world event manager**, outside the tick-based world of played regions, owns
  everything that happens in the world and coordinates the played regions.
- **Worldgen is the same system, fast-forwarded**: a minimal bootstrap (place civilisations,
  create people), then the event manager runs at speed for some years "to have something
  there". After worldgen there is no more fast-forward.
- **One shared calendar at fortress speed.** A two-week courier journey takes two in-game weeks
  for the player waiting on the reply.
- An event plays out **abstractly** (dice or criteria) where nobody is playing, and **for real**
  where someone is: the person really falls ill, a courier really leaves, and the player can
  change the outcome (heal them, refuse the courier). Unplayed fortresses still take part; their
  outcomes are deterministic and outside anyone's control.
- **Small world, detailed people.** A civilisation of dozens of fortresses is a couple of
  thousand people at most; fortresses are capped at what is technically feasible. Detailed
  simulation of each person's life (illness, family, lineage, notable events — not the mundane)
  is the point, for roleplay.
- **Travel is abstract unless played.** A courier crossing unplayed land is only a record in the
  event manager — no physics. If a region on the way is being played, they physically walk
  through it.
- **Loading a region brings it up to date.** The event manager takes the region as last saved and
  applies what has happened since, as far as it is relevant there: a courier who should be there
  is spawned; a famine removes food down to small stockpiles, with its cause applied.
- **People are portable.** When a person leaves a region, their whole game data is copied out so
  the event manager can put them back into whichever region they turn up in.
- The event log is a much finer-grained Legends. **What players get to see is undecided.**
- **Players never approve or refuse an event** (user). They see events happen and can try to
  prevent them through gameplay; once an event happens, that's it. Example: two people in a
  relationship marry — you cannot deny the marriage, but you can interfere with the relationship
  in play, and so prevent it.
- **A played fortress will not have the same outcomes as an unplayed one, and that is expected**
  (user). Events are handled case by case: events originating in a played fortress are emergent
  from gameplay rather than an abstraction (a marriage *could* come from two characters'
  relationship in play rather than the manager's dice).

## Players and the world (user, 2026-11-01)

The model works like Dwarf Fortress; today's Play-any-region flow and the player's dwarf body in
fortress mode are development scaffolding. The final system:
- **New world → embark**, like DF: after generation you embark a new fortress **from an existing
  civilisation**, choosing **people from one or more of its existing fortresses** to bring.
- **Embark only onto empty land**, at some minimum distance from existing fortresses. A player
  **never takes command of an existing fortress.**
- **Fortress mode is always a disembodied camera.** The player is not a person in the world.
- **Retiring** a fortress hands it back to the world; afterwards the player can start a new
  fortress or come back to an existing one (a fortress they retired), or go adventuring.
- **Adventure mode**: you create your own character, who is spawned into the world (and is then a
  person like any other). You can start an adventure in a fortress you retired.
- One human player is in one mode at a time per world: you must retire your fortress before
  adventuring.
- In multiplayer, other players can adventure into your fortress.

## The first slice (user's choice)

The foundation, proven end to end by **one event chain: fortress growth, shrinking, and part of
the population embarking to found a new fortress**. The user picked it because worldgen is the
current focus: it is also how civilisations spread during the fast-forward, so the same events
build the world's history and play out in a played fortress.

It also proves the rule in its most direct form: **the player's own embark is the same Embark
event** the world sim uses to spread civilisations — triggered by a player instead of by crowding.

## Goals

1. **The world event manager** (below: "the manager"): a deterministic, threadless core in the
   `game` crate, driven by the world calendar, holding the world's people, sites and journeys,
   scheduling events, and writing the event log.
2. **The world calendar**: one clock for the whole world at fortress speed; region ticks map to
   calendar time exactly.
3. **People**: every person is a full record from birth (no anonymous population counts), with
   identity, family, membership of a site, life state and location.
4. **Sites**: fortresses as world records (place, civ, members, founding, abandonment).
5. **Authority hand-over** between the manager and played regions, both ways, at any moment of
   an event — including mid-journey and mid-event.
6. **Region catch-up on load**: the manager tells a starting region what is true now (who is
   there, where, doing what) and the region reconciles to it.
7. **The first event chain**, each event with all three parts (abstract resolver, in-fortress
   version, apply-to-region): births (with marriage as their prerequisite), deaths, migration
   between sites, and an embark party founding a new site.
8. **Worldgen bootstrap + fast-forward**: civs and first people placed after the forge; the
   manager then runs N years with no regions played; the result is the world's starting history.
9. **Persistence** of the manager's state and log with the world, so history survives a server
   restart.
10. **Statistics and a text dump** of the log, for tuning (like the forge's stats).
11. **The player's embark and retire**: embark from a civ with chosen people onto empty land
    (replacing Play-any-region as the way into fortress mode), retire, and come back to a
    retired fortress.

## Non-goals (this slice)

- Illness, letters, couriers as an event chain (the original example — the next chain; the
  journey machinery it needs is built here).
- Food, famine, items, stockpiles, jobs, labour. None exist in the sim yet.
- Religions, deities, wars, diplomacy, trade, caravans, sieges, nobles, artifacts.
- Races other than dwarves (see "Calls left to confirm").
- Names beyond a placeholder scheme; a naming system is its own project.
- Settlement layout and buildings: generated on region load, a separate project. A site's
  region in this slice is its terrain plus its people.
- Player-facing Legends / history browser. What players see is undecided (user).
- Saving played regions' terrain to disk (see "Known gaps").
- Adventure mode's character creation and the adventurer as a person. Today's adventure body
  stays as dev scaffolding, outside the history, until that project.
- Ways for the player to *interfere* with this chain's events (relationships, keeping people
  happy so they stay): they need gameplay systems that do not exist yet. In this slice the player
  watches growth, deaths and departures happen; their one lever is their own embark.

## Anti-goals

- **A world change with no in-fortress counterpart.** If the abstract side can do it, a played
  fortress must be able to have it happen. An event type enters the world sim only once its
  in-fortress version exists — the rule doubles as a build-order rule.
- **A player veto.** No event asks the player to approve or refuse it. Influence is only ever
  through gameplay, before the event happens.
- **Physics for people nobody can see.** No region is woken just to move an abstract person.
- **Per-tick polling of the whole world.** The abstract side is a queue of scheduled times.
- **Nondeterminism in the abstract side.** Same world + same inputs ⇒ same history.

## Constraints from the engine

- `game` and `server` stay Bevy-free. The manager lives in `game`, next to `WorldManager`, and
  like it is threadless and takes time as an argument, so tests drive it with the
  `InlineSpawner` and seeded schedules, and wasm's embedded `LocalServer` can run it.
- The manager is **server-authoritative and not part of rollback**. Clients never run it.
  Anything it changes inside a region enters that region as a **manager-authored event**
  (the way `CreateClient` / `CreateClientAt` do today) through `RegionInput`, so it is in the
  region's event stream, ordered, hashed and replayed by clients like any server event.
  Anything a region tells it travels as a new `RegionOutput` variant. That message pair is
  already "the future network seam".
- Region sims are deterministic and rollback-safe; the server's region is authoritative and
  never reconciles, so a fact a server region reports to the manager is final.
- Randomness on the abstract side is stateless hashing in the style of `slime::roll(tick,
  subject, salt)`: no RNG state to save or keep in step.
- Seam crossing is built (`EntityBundle` with the `Travelling` component bag, `Departures`
  routed by the manager). **Arrivals currently always wake the target region**
  (`world_manager.rs`, "Arrivals ALWAYS wake the target"); persons need a different policy
  (below).
- Regions are 512 m in play; the world map is a grid of regions (`WorldMap` / `TileSummary`).
  Abstract travel and site placement work on that grid.
- `TICK_RATE` is 50 ms (20 ticks/s). Nothing pauses (multiplayer).
- **Session lifecycle**: any new per-world client resource or entity must be session-scoped
  (`CLAUDE.md`).
- There are no NPC citizens yet: the only rigged body is the player's dwarf (`anim::RigId::Dwarf`),
  and adventure mode's `Walker` (A* + `MoveTo`, seam handoff) is the only goal-driven walking.

## The design

### 1. Calendar

- **One world clock**, in calendar ticks, advanced by the server (the manager thread) from wall
  time, at the same rate as region ticks. A region knows the calendar tick it started at; its
  own tick count maps onto the calendar exactly.
- **Calendar units are named constants** (my call; tunable): DF-shaped — a year of 12 months ×
  28 days. Default day length so a year is roughly an hour of play, as in DF. Births, ageing
  and every event delay are in calendar units, never in ticks or wall seconds.
- **The world clock runs only while the server runs.** A single-player world (one server per
  world, exiting when its client leaves) therefore stands still while nobody plays it. A
  multiplayer server's world runs for everyone.
- Worldgen fast-forward advances the same calendar as fast as the queue drains; it ends at the
  start date of play.

### 2. The manager's world records

All plain data, serialisable, deterministic containers (`BTreeMap` / ordered ids, never
`HashMap` iteration).

- **Person** (stable `PersonId`, never reused):
  - race (dwarf only for now), sex, birth date, death date + cause (if dead);
  - parents, spouse, children (lineage is just these links);
  - **citizenship**: the site they belong to (or none);
  - **location**: exactly one of
    - `AtSite(SiteId)` — abstract, at their site;
    - `Journey(JourneyId)` — abstract, travelling;
    - `InRegion(RegionCoords)` — physically present in a running region, which is authoritative
      for their body (and the manager knows only that);
  - a placeholder name (see non-goals);
  - the **portable game data** of their body: the last `EntityBundle`-style payload (component
    bag) the manager received for them, used to put them back into a region.
- **Site** (`SiteId`): region coords (one site per region in this slice), civ, founding date and
  founders, abandonment date, a **capacity** (below).
- **Civ** (`CivId`): race, its sites. Ethics, values, religion etc. are later.
- **Journey** (`JourneyId`): who (one or more persons travelling together), purpose (migrate to
  site X, embark to found at region Y), the **route** as a list of world-map regions with entry
  and exit points and calendar times, and its progress. Pathfinding is A* over the world-map
  region grid with costs from the tile summaries (sea impassable; relief and slope cost),
  deterministic.
- **Scheduled events**: a priority queue keyed by (calendar tick, sequence number) of pending
  event steps.
- **The event log**: append-only records of what happened (birth, death, marriage, emigration
  decided, departure, arrival, site founded, site abandoned), each with date, place, the people
  involved and the cause. This is the history; Legends is a reading of it later.

### 3. Events

- An event is a **small saveable state machine**: an event type, its participants, its current
  step and step data. Steps are scheduled on the queue.
- **Where an event is decided is per event type** (user: case by case). Two kinds:
  - **Manager-decided** ("directed"): the manager rolls the dice on its calendar queue, for
    played and unplayed sites alike. In a running region the region then **plays the outcome out
    physically** (a child appears, a household walks out); elsewhere it is a record. Nothing about
    the decision enters rollback state; the region holds only bodies tagged with a `PersonId`.
    **Every event in this slice is manager-decided** (user chose this, option (a)).
  - **Emergent**: the event comes out of fortress gameplay and the region reports it as a fact
    (later: an injury, bad food, a marriage arising from a relationship in play). An emergent
    type usually also has a manager-decided twin for unplayed sites. None in this slice; the
    region→manager fact channel is built and tested now so they can be added type by type.
- A manager-decided step that acts on people physically in a running region is **delivered to that
  region to play out**; a step is never resolved on both sides.
- **Played** means a region that is the fortress a player is currently playing (embarked, not
  retired). A region running for another reason (a neighbour's arrival, an adventurer, the grace
  period before parking) holds its people physically and plays manager outcomes out the same way;
  the distinction matters for later emergent events, which only a played fortress generates.
- There is no player decision point in any event: the player sees it happen.
- Each event type also has an **apply-to-region** part: how its effects so far appear in a region
  that loads after (or during) it. Apply is **state-based, not a replay of deltas**: "these people
  are here, at these places, in this state", so applying it twice is harmless and the order of
  catch-ups does not matter.
- Events can **start on either side**. The manager's scheduler starts abstract ones; a played
  region starts in-fortress ones (in later chains: an injury, bad food) and reports them, after
  which the manager tracks them like any other.

### 4. Authority hand-over

The rule: **a site is abstract exactly when its region is not running.** Persons follow their
region.

- **Region starts** (`WorldManager::ensure_running`, any reason — a player plays it, a neighbour's
  arrival, a seam ghost):
  1. the manager computes the region's **catch-up**: every person whose location resolves to this
     region now — citizens `AtSite` of a site here, and travellers whose journey is inside this
     region at this calendar time (position interpolated along the route leg) — plus the open
     event steps that involve them;
  2. it sends one catch-up message before the region's first tick; the region reconciles: spawns
     each listed person from their portable data at their place (a site's people near the site
     centre; a traveller at their interpolated point, with a walker continuing their leg),
     despawns any person body in its saved state that is not listed (they left or died while it
     was parked), and takes over the listed event steps;
  3. those persons become `InRegion`, and their pending abstract steps are cancelled/handed to
     the region.
- **While the region runs**, it reports facts as they become final: born (with parents),
  died (cause), married, a person's state changed in a way the world cares about, a person left
  through a seam or the region's outer edge. The manager records them in its records and the
  log. Positions are not reported per tick.
- **A person leaves a running region** (walks across a seam):
  - into another **running** region → the existing seam handoff; the manager only updates
    `InRegion`;
  - into a **non-running** region → **the target is not woken**. The manager takes the bundle as
    the person's portable data and continues them abstractly: a journey leg resumes from the
    crossing point at the crossing time. (A change from today's "arrivals always wake" rule, for
    persons only.)
- **Region stops** (parks): before parking it hands every person body it holds back to the
  manager (bundle + any in-progress event step state). Persons become `AtSite` (if at their own
  site) or back on their `Journey` at their current point; their pending steps go back on the
  abstract queue at the current calendar time.
- **Ordering and idempotency**: manager→region messages carry the calendar tick they were
  decided at and a sequence number; the region applies them before its next tick (like seam
  arrivals, which are untimed). Region→manager facts carry the region tick (→ calendar tick) and
  are idempotent by (person, fact) so a duplicate is ignored.
- The exact message shapes, the race between "manager decides an abstract step" and "region
  starts" (resolved by the manager being single-threaded: catch-up is computed and steps are
  handed over in the same manager step), and any acknowledgement protocol are **the builder's to
  settle** (user: "a technical implementation matter").

### 5. The first event chain: growth, shrinking, embarking

All rates, thresholds and party sizes are named tunables shared by both resolvers.

- **Capacity (stand-in for food).** Each site has a capacity in people, from its region's
  suitability (tile summary: dry land share, rivers/lakes, relief; biomes once the terrain
  design lands). It is a placeholder until food exists; it is the only driver of crowding.
  **Crowding** = members / capacity.
- **Marriage.** Unmarried adults of the same site pair up at a rate (manager-decided, hashed roll
  per candidate pair per check). In a played fortress it is shown when it happens (log entry,
  notification). Prerequisite for births. Later a relationship system can make it emergent in
  played fortresses.
- **Birth.** A married couple has children at a rate lowered by crowding. In-fortress: a child
  body spawns next to the mother; abstract: a record only. Apply-to-region: the child is a
  citizen and appears like any other.
- **Death.** Old age by an age-dependent hazard (illness and violence come with later chains).
  In-fortress: the body is removed (a corpse when items exist). Abstract: a record.
- **Migration between sites of the same civ** (growth for one, shrinking for the other). When a
  site is crowded and another site of its civ has room and is reachable, a household (a person
  with spouse and young children) decides to move. It becomes a **journey**. Leaving a played
  fortress: the party gathers and walks out of the region edge toward the first leg of its route
  (adventure's walker); arriving at a played fortress: the party appears at the edge on the
  route's entry point and walks in — this is DF's migrant wave, but from a real site.
- **Embark (founding).** When a site is crowded and no site of its civ has room (or by a lower
  base rate when one does), a party forms: a few households, minimum party size. It picks a
  target region by suitability, distance and spacing from other sites, journeys there, and on
  arrival **founds a site** (log: founded by these people, from that site). If the target is
  being played when they arrive, they walk in and the site is founded when the party reaches the
  region's centre area. In-fortress departure as for migration.
- **The player's embark** is the same Embark event, triggered by the player (§7b). Departures
  from a played fortress just happen: the player sees the party gather and leave and cannot veto
  it (user). Later, gameplay levers (happiness, room to live) will let them prevent it.
- **Abandonment.** A site with no living members is abandoned (log entry). Its region keeps its
  terrain.
- **Shrinking** is deaths + departures; there is no famine or war in this slice.

### 6. In-fortress presence of people

- A new `EntityKind` for a person (a citizen, migrant or traveller), rigged as the dwarf, carrying
  its `PersonId` and the components it needs in the bundle's component bag, so seam crossing,
  ghosts and parking work as for any entity.
- Behaviour in this slice is minimal: idle wandering near the site centre (like slimes'
  hop-rest AI but walking), and goal walking via the existing `Walker` for departures and
  arrivals. Jobs come with workers, later.
- Players can select a person in the commander view and see who they are: name, age, site,
  spouse, parents, children (my call: the cheapest useful view; the wider question of what
  players see is open).
- The player has **no body** in fortress mode (user). Today's player dwarf body in fortress mode is
  scaffolding; this slice hides/removes it there as the embark flow replaces Play (the commander
  already leaves it inert and hidden as `LocalBody`).

### 7. Worldgen bootstrap and fast-forward

- After the forge, a **history stage** (offline, like the forge; a recipe and the main-menu
  generator run it) loads the world file, builds the region suitability grid from the tile
  summaries, and:
  1. places **N civs** (dwarves), each with one first site (a mountainhome) on suitable land with
     spacing;
  2. creates each civ's **first people**: couples and families with spread ages, sized to the
     sites' capacity share;
  3. runs the manager with **no regions running** for **Y years** — every step resolves
     abstractly — so births, deaths, marriages, migrations and embarks build the world's first
     history and sites.
- Knobs from day one: civ count, years, founding population, all chain tunables. Defaults are
  the builder's to tune by statistics (target: a handful of sites per civ after the default
  years, civs not dying out, populations within the "couple of thousand per civ" ceiling).
- Output: the manager's state + log saved next to the world (below); a stats summary (people
  alive/dead, sites per civ, events by type, run time) like the forge's.
- Rejection, like DF: a world where no civ survives the fast-forward is reported and re-run with
  the next sub-seed.

### 7b. Embark, retire, return (the player's way in)

- **Embark** (replaces today's Play-any-region on the world map as the way into fortress mode;
  the dev Play path may stay behind a dev switch):
  1. the player picks a **civ** (dwarves: any living civ);
  2. picks **people** from one or more of that civ's sites (a list per site: name, age, family),
     within a party size limit (tunable, DF-like ~7);
  3. picks a **region of empty land**: no site there, and at least a minimum distance (tunable,
     in regions) from every existing site; the world map greys out the rest;
  4. the manager runs the **Embark event** with those people: they leave their sites (log:
     departed with the expedition) and **found the new site at once** — the party is there when
     the fortress starts, as in DF; the journey is recorded as having happened (my call: making
     the player wait out a real journey before playing would be poor; the shared calendar is not
     advanced for it). The site's region starts as the player's fortress and the people spawn in.
- **Retire**: from the in-game menu. The fortress stops being played; its region parks as usual,
  its people go back to the manager, and the site continues abstractly like any other.
- **Return**: the player can come back to a fortress **they retired** (sites record who founded
  them as a player). Taking command of a site the player did not found is not possible.
- **One fortress per player at a time**; a player with a played fortress cannot embark another
  until they retire it. Adventure mode (its own project) is entered only with no fortress being
  played.
- In multiplayer each player embarks separately; the empty-land and distance rules include other
  players' fortresses.

### 8. Persistence and files

- The terrain world file stays immutable. History is mutable during play, so it goes in a
  **separate file** in the world directory (`worlds/<slug>/history.*`), written atomically
  (temp + rename, like `terrain.world`), versioned like the world artifact.
- The server loads it at start and saves it periodically and on shutdown. A world forged
  before this feature has no history file → bootstrapped on first start (as `default.world` is
  forged on first start) or played without history (builder's call; a log line says which).

### 9. What the client sees in this slice

- **World map**: sites as markers (civ colour, population) — the cheap, already-wanted overlay —
  and the embark screens (§7b): civ, people, empty-land region.
- **In fortress**: people bodies; a short notification line when an event touches the fortress
  (born, died, married, a party leaving, migrants arriving); the person inspector (§6); Retire in
  the in-game menu.
- **Dev tooling**: a text dump of the log and the stats (a `just` recipe), and an F3 panel line
  with the calendar date. Player-facing Legends is out of scope.

### 10. Tests

- Determinism: the same world file + seed + inputs ⇒ the same log, bit-exact, on native (and on
  wasm32 for the manager core, since the embedded `LocalServer` will run it).
- Hand-over: the seeded-schedule layer (`InlineSchedule`) plays a region in and out of running
  in the middle of a migration journey and of an embark; persons are never lost, duplicated, or
  present in two places; every manager-decided outcome delivered to a region is played out
  there (a birth shows a child, a departure walks out). Played and unplayed runs are **not**
  expected to produce the same history (user); the tests check invariants, not equal logs.
- Embark: the chosen people leave their sites and are present in the new fortress; the target
  must be empty land at the minimum distance; retire hands the people back and the site carries
  on abstractly; return re-enters only a fortress the player founded.
- Catch-up: a parked region with a person who died abstractly loads without them; a traveller
  scheduled to be mid-region loads at the right spot.
- Leaving into a non-running region does not wake it.
- Fast-forward statistics as a smoke test with a small world and few years; long runs tagged
  slow.

## Known gaps (not this design)

- **Played regions are not saved to disk.** Parked regions live in server memory; a server
  restart regenerates terrain. With this design a restarted world still has its people and
  history (the manager reconciles them into the regenerated region), but dug terrain is lost. A
  region save is its own project.
- Settlement generation on region load (buildings for a site) is separate; until then a site is
  its people on bare terrain.
- The DF-scale terrain (biomes, 100 km) is not built; site suitability reads today's tile
  summaries behind an interface that a biome-aware version replaces.

## Left to the builder

- Module boundaries, type and message names, the exact catch-up and fact message shapes.
- The calendar's day length (default near DF's ~1 hour per year), all tunables' defaults.
- How capacity is computed from tile summaries.
- World-map route costs and the A* details.
- History file format.
- Whether an old world without a history file is bootstrapped or played without history.

## Relation to the civilisations handoff

`plans/2026-11-01-civilizations-territories-handoff.md` planned history as an offline-only forge
stage without individual figures. **This design supersedes its history parts**: history keeps
running during play, with individual people. Its other parts — territories, entity tables,
religions, other races, roads, names — come later as event types and records built on these
person/site/civ records, which should be shaped to be extended, not replaced.

## Calls left to confirm (made by me, not the user)

- Dwarves only in this slice (the only rig).
- The embark party appears at the new site at once; its journey is recorded, not played.
- A player can return only to a fortress they founded.
- Selecting a person shows a small inspector; sites show on the world map; a notification line
  for events touching the played fortress.
- One site per region.
- History lives in its own file next to the terrain file.
- A region running without being played (neighbour arrival, adventurer, grace period) plays
  manager outcomes out physically like a played one.
