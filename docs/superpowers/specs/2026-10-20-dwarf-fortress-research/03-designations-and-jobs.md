# 03 — Designations, jobs, labors, hauling and pathfinding

Part of the Dwarf Fortress research (2026-10-20); start at `00-synthesis.md`. Source tags as in
`01-world-and-embark.md`. **[SO-2021]** = Tarn Adams interviewed in "700,000 lines of code, 20
years, and one developer: How Dwarf Fortress is built", Stack Overflow Blog, 2021.

## 1. The control model

The player **never controls a dwarf directly**. They mark the world (designations, zones,
stockpiles, buildings) and queue work (workshop tasks, work orders); jobs come out of those
marks; idle dwarves pick jobs they are permitted to do. "Your dwarves function as
semi-autonomous entities, usually fulfilling their own basic needs, and sometimes performing
jobs when able" [wiki Labor]. This is the single most important design fact for the engine:
**player input is sparse and coarse** (a few designations a minute), and all the per-tick
activity is AI.

Job sources [wiki Labor]: designations, zones, workshop tasks, manager work orders — plus
needs-driven personal jobs (eat, drink, sleep, pray, socialise; `04-dwarves.md`) and
automatic ones (hauling to stockpiles, cleaning, recovering wounded).

## 2. Designation types

From the premium interface bar [wiki Interface] and [mem]:

- **Mining**: dig, channel, up/down/up-down stairs (v50: inferred from a box), ramp, remove
  ramps/stairs. Each designation carries a **priority 1–7** in v50 [mem] and a *marker* mode
  (planned but not active) [mem].
- **Logging**: chop trees → logs.
- **Gathering**: gather plants/fruit from shrubs and trees.
- **Smoothing / engraving / fortifications / carve track**.
- **Remove designation orders**, **remove construction**.
- **Building**: place furniture, workshops, constructions (a placement is a job to fetch the
  materials and build).
- **Stockpiles**, **Zones** (activity zones: meeting area, bedroom, dining hall, pasture, pit,
  water source, garbage dump, …), **Burrows** (areas dwarves are restricted to), **Minecart haul
  routes**.
- **Traffic**: high / normal / low / restricted.
- **Item designations**: melt, dump, hide, forbid (and claim).

The player drags designations over a rectangle, optionally across several z-levels.

## 3. Labors, work details, skills

[wiki Labor] (premium; classic used per-dwarf labor toggles instead [mem]):

- "Most jobs correspond to a labor, which dwarves can be restricted from doing." Labor =
  permission; **skill** = proficiency. Skills grow with use (`04-dwarves.md` §2).
- **Work details** group labors; each detail is *Only selected do this*, *Everybody does this*
  or *Nobody does this*. Defaults: Miners, Woodcutters, Hunters (the only routes to those three
  labors), Planters, Fisherdwarves, Plant gatherers, Stonecutters, Engravers, Haulers
  (all hauling labors), Orderlies (medical), Siege operators. Custom details can be added.
- **Specialized** dwarves only take jobs from their assigned workshops/occupations and their
  details. Occupation labors (e.g. doctor) are always allowed.
- Standing orders (global toggles such as "dwarves gather refuse", "auto-loom") and kitchen
  permissions (which ingredients may be cooked/brewed) also gate jobs.

## 4. Job selection and hauling

[mem] unless tagged:

- Jobs sit in a global job list. An idle dwarf periodically looks for a job it may do,
  favouring higher designation priority and nearer jobs; personal needs (eat/drink/sleep at
  their thresholds, high-priority needs like prayer) pre-empt fortress jobs
  [wiki Personality_need: low-priority need jobs "may be cancelled to undertake a fortress job";
  high-priority ones "will not"].
- A job reserves its target and materials so two dwarves don't take the same work.
- **Material choice is distance-ordered but obstacle-blind**: the game grows a cube around the
  workshop/dwarf until it finds valid materials, then picks by Manhattan distance, ignoring walls
  [wiki Path].
- Food choice uses the metric `max(x,y,z) + min(x,y,z)·0.414` [wiki Food].
- **Hauling** is the bulk of job volume: every loose item that a stockpile accepts becomes a
  haul job; dwarves "place the item into the empty spot that is nearest to the item, not counting
  any obstructions" [wiki Stockpile]; wheelbarrows, bins and barrels batch it. Many DF
  efficiency tricks (small stockpiles next to workshops, "quantum" stockpiles) exist because
  hauling dominates [wiki Path, Maximizing_framerate].
- Jobs fail and are cancelled for: no path, dangerous terrain (water depth ≥ 4), interruption
  by a need or a threat, damp/warm stone revealed, materials gone.

## 5. Pathfinding (DF's actual approach)

- **A\*** with an admissible heuristic, so optimal paths: "Dwarf Fortress uses the A* search
  algorithm … the heuristic used in Dwarf Fortress is admissible, and thus the ideal path is
  always found" [wiki Path]. Tarn: "We use A*, which is fast of course, but it's not good enough
  by itself." Jump point search was rejected "because our map changes so much" [SO-2021].
- The heuristic is **Chebyshev distance in 3D** ("a chess king moving in 3D"), which in open
  caverns makes A* reach the right x/y and then flood a whole z-range looking for a way up —
  why open caverns hurt [wiki Maximizing_framerate].
- **Connected components** ("connectivity map"): "our approach has been to just keep track of
  connected components reachable by walking. … if the component numbers are the same, they know
  the call will succeed." When water splits the fortress, one side is flood-filled and re-indexed.
  This "removes nearly all failed A* calls". Flyers get no separate index (memory cost); they use
  short local flood fills in combat [SO-2021]. Map-connection changes (doors, bridges) cause lag
  spikes because the map must be re-indexed [wiki Maximizing_framerate].
- **Traffic costs** per tile: high 1, normal 2, low 5, restricted 25, adjustable
  [wiki Traffic]. Dwarves with an important job ignore traffic; other creatures ignore it.
- **Crowding**: creatures can walk over each other, but it is "much slower" and they path
  around it; crowded corridors cause "dodging", which "results in more pathfinds" [wiki Path].
- Speculation on the wiki: DF may search from destination to creature; paths may be cached;
  unreachable items may be temporarily treated as forbidden [wiki Path — marked there as
  uncertain]. Tarn mentions limited path caching only as an idea [SO-2021].
- Later releases throttle creatures that repeatedly fail to path and announce it
  [Bay 12 news via search, 53.x; unverified detail].
- **Burrows** restrict where dwarves look for jobs and walk [wiki Traffic, Labor].

**How much it costs:** "The vast majority of processing time in Dwarf Fortress is taken up by
units taking their turns, over 60% in larger forts, of which less than 10% is actually
pathfinding related" [wiki Maximizing_framerate]. Pathfinding becomes a problem only in edge
cases: trapped dwarves (especially trapped moody dwarves) retrying forever, squads locked in,
big open areas. The biggest single cost today is **line of sight**: "even after optimizations
in v50.05, the slowest part of the game by a wide margin. This is O(n²) by nature"; units more
than 26 tiles apart in any direction are not considered.

## 6. Simulation complexity per agent

| work | when | cost |
|---|---|---|
| job search | when idle (rate-limited) | scan of a job list filtered by labor; DF's real cost is in candidate filtering, not search |
| path | once per job leg, retried on block | A* over tiles, bounded by component check |
| movement | one tile per gait period (walking 900 = 9 ticks per tile, `04-dwarves.md` §6) | O(1) per step + collision/dodge |
| work progress | per tick while working | O(1) |
| line of sight / threat check | every tick in DF | O(n²) in DF — the thing to design out |

## 7. MVP tiers

| system | tier |
|---|---|
| Dig / channel / stairs / ramp / chop designations, drag box over z-range | **MVP** |
| Global job list, idle dwarf takes nearest permitted job, reservations | **MVP** |
| Labors as a fixed small set + "everybody does this" | **MVP** |
| Work details UI, specialization | soon after |
| Designation priorities 1–7 | soon after |
| Hauling to stockpiles (one item at a time) | **MVP** |
| Wheelbarrows, bins, barrels | soon after |
| A* with traffic costs + connectivity components | **MVP** (components are what make it cheap) |
| Traffic designations UI, burrows | soon after |
| Work orders / manager | later |
| Minecarts | later |

## Sources

- https://dwarffortresswiki.org/index.php/Labor
- https://dwarffortresswiki.org/index.php/Path
- https://dwarffortresswiki.org/index.php/Traffic
- https://dwarffortresswiki.org/index.php/Stockpile
- https://dwarffortresswiki.org/index.php/Food
- https://dwarffortresswiki.org/index.php/Personality_need
- https://dwarffortresswiki.org/index.php/Interface
- https://dwarffortresswiki.org/index.php/Maximizing_framerate
- https://stackoverflow.blog/2021/07/28/700000-lines-of-code-20-years-and-one-developer-how-dwarf-fortress-is-built/
- https://bay12games.com/dwarves/ (news, path-failure throttling; via search result, not read)
