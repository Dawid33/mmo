# 11 — Fail states, pacing, time scale and performance

Part of the Dwarf Fortress research (2026-10-20); start at `00-synthesis.md`. Source tags as in
`01-world-and-embark.md`; [SO-2021] as in `03-…`.

## 1. How a fortress lives and dies

"There is no internal end point, single goal … or 'You Win!' announcement in Dwarf Fortress,
therefore, eventually, almost every fortress will fall" — **losing is fun** [wiki Losing].

The shape of a typical fortress [mem, consistent with the pages cited]:

1. **Embark** (7 dwarves, a wagon): dig in, set up a farm and a still, beds, a dining room.
2. **Year 1**: two hardcoded migrant waves (`09-…` §1) roughly triple the population; food and
   booze must scale; first caravan in autumn.
3. **Years 2–3**: wealth-driven waves (often 20–30 dwarves at once), first ambushes, nobles
   (mayor at 50), mandates, workshops chains, military.
4. **Pop ~80+**: sieges, megabeasts; caverns breached; forgotten beasts; megaprojects.
5. **Death** by any of: wildlife/cavern/forgotten-beast attack, siege, flood (water or magma),
   starvation or dehydration, a **tantrum spiral** (stress cascade after deaths,
   `04-…` §5), vampires/werebeasts, or the player abandoning a slowed fort.

Explicit end states: all citizens dead ("Your settlement has crumbled to its end"), or the
player **retires** (fort keeps running in the world, playable later) or **abandons** it; both
leave a site that can be **reclaimed** later or visited in adventure mode [wiki Losing]. That
the fortress *persists in the world* after the player stops is core to DF's identity.

## 2. Time scale

[wiki Time, Calendar]:

| | fortress | adventure |
|---|---|---|
| one tick | 72 s of game time | 0.5 s |
| day | 1,200 ticks | 172,800 |
| year (336 days) | 403,200 ticks | 58,060,800 |

- Default FPS cap 100 (simulation ticks per real second; graphics FPS is separate and
  "Reducing G_FPS will not speed up the rest of the game, as everything in it runs on a separate
  thread" [wiki Maximizing_framerate]). At 100 FPS a day is 12 s and a year **67 minutes**; at
  30 FPS a year is 3 h 44 min.
- Fortress time is compressed ~144× against adventure time, which is why a walking dwarf crosses
  9 ticks per tile = 11 tiles per real second at 100 FPS (`04-…` §6): DF's movement looks fast
  because a "day" is 12 seconds.
- Counters are tuned to this: a dwarf eats every ~40–50k ticks (~35–42 days), sleeps every
  ~50k ticks, drinks booze about five times a season.
- The game pauses on important announcements; most play happens paused or at full speed.

## 3. FPS death and what DF does about it

"FPS death" = a mature fort's simulation slows until the game is unplayable. Causes, from
[wiki Maximizing_framerate] unless tagged:

| cause | note |
|---|---|
| **Units taking turns** | ">60 % in larger forts, of which less than 10 % is actually pathfinding"; each dwarf "accounts for the actual majority of frame times" |
| **Line of sight** | "the slowest part of the game by a wide margin … O(n²)"; ≥26 tiles apart in any direction is skipped; crowded meeting halls hurt; doors break LOS but cost FPS themselves |
| **Temperature** | disabling it reported to double FPS; items are checked for temperature |
| **Items** | number of stacks (hauling, stockpiles, pathing) more than stack size; ≥10,000 items notable; quality/decorated items cost more memory |
| **Pathfinding edge cases** | A* floods everything on unreachable/blocked targets; trapped (moody) dwarves; squads locked in; open caverns + Chebyshev heuristic; Restricted traffic on mandatory routes; crowded 1-wide corridors (dodging) |
| **Map connectivity changes** | doors, bridges, atom smashers on repeaters re-index the connections map → lag spikes |
| **Fluids** | flowing water/magma blocks |
| **Contaminants** | accumulate forever (bugs) |
| **Multi-tile trees** | "a known source of lag"; tree tiles are also the slowest to render |
| **Caverns** | many creatures, huge open areas |
| **World size / history** | background world events keep running |
| **Invaders** | sieges add many units; invasion caps exist |
| **Plant gatherers** | "everyone does this" with a high population |

What DF does: **A\* + connected components** (no failed searches), traffic costs, LOS radius cut
at 26 tiles, v50.05 LOS optimisation, **experimental multithreading** since 50.09 ("makes the
game even faster", in settings; later releases fixed threading crashes) [wiki Release
information/50.09, search results for 50.10/50.11]; v53.x throttles repeated path failures
[Bay 12 news via search]. Tarn on the core: "we are certainly at the edge of what we can
currently support in terms of agents and map complexity"; until v50 "we don't do any of that
[multithreading], aside from the graphical display itself" [SO-2021]. DFHack's `timestream`
decouples calendar/unit speed from FPS to make old forts feel fast.

## 4. Lessons for a deterministic sim (carried into `engine-mapping.md`)

1. **Agents are the cost, not search.** Budget per *unit-turn*; make most units idle most ticks
   (event-driven wake-ups, not polling every unit every tick).
2. **Never do O(n²) sensing.** A spatial hash bucketed by ~16 tiles makes "who can see me"
   O(n·k); run threat sensing on a cadence (every N ticks), not every tick.
3. **Connected components first**, A* second; incremental component maintenance on dig/build;
   a "path failed" cooldown per (unit, target) so nothing retries every tick.
4. **Items cost nothing per tick.** No per-item temperature or wear polling; index by type,
   material and position so jobs don't scan.
5. **Counters as timestamps** ("hungry since tick T"), not per-tick increments — zero cost per
   tick and nothing to log for undo.
6. **Temperature and fluids are event-driven** with dirty regions.
7. **Expose a speed dial** like DF's FPS cap: the same sim at 1×…N× ticks per second.
8. **Cap populations** (invaders, wildlife, migrants) as named tunables.

## 5. MVP tiers

| item | tier |
|---|---|
| Calendar at DF's tick resolution (1,200 ticks/day) | **MVP** |
| Pause + speed control | **MVP** |
| Fail state: all citizens dead → fortress lost; player abandon | **MVP** |
| Save / load the fortress | **MVP** (a DF session lasts hours; the engine's parking lot is in-memory only today) |
| Retire / reclaim, fortress persisting in the world | later (needs world persistence + traversal) |
| Announcements with auto-pause | **MVP** |
| Population caps | **MVP** (tunables) |

## Sources

- https://dwarffortresswiki.org/index.php/Losing
- https://dwarffortresswiki.org/index.php/Time
- https://dwarffortresswiki.org/index.php/Calendar
- https://dwarffortresswiki.org/index.php/Maximizing_framerate
- https://dwarffortresswiki.org/index.php/Release_information/50.09
- https://dwarffortresswiki.org/index.php/Release_information/50.10 (via search result)
- https://stackoverflow.blog/2021/07/28/700000-lines-of-code-20-years-and-one-developer-how-dwarf-fortress-is-built/
