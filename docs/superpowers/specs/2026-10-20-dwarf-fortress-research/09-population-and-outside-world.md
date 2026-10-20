# 09 — Population and the outside world: migrants, nobles, caravans, diplomacy

Part of the Dwarf Fortress research (2026-10-20); start at `00-synthesis.md`. Source tags as in
`01-world-and-embark.md`.

## 1. Immigration

[wiki Immigration]:

- Migrants arrive **in groups, at most once per season**, spawn at the map edge, walk to the
  nearest meeting area and stay there until their migrant status wears off.
- **Two hardcoded waves** in the first year (size 1–10, unaffected by wealth or danger); "after
  the first two hardcoded waves, migrants will not arrive until the fortress has accumulated enough
  wealth, which must be reported by visitors such as merchants".
- Wave size is driven by **created wealth** (imports and trade figures only modify it by a
  percentage), capped by a factor that grows with the size of the fortress's whole units list
  (dwarves, invaders, merchants, animals, dead or alive). Largest reported wave: 77.
- **Dangerousness** (recent deaths, executions, insanities over 5 years, each year weighted
  half the one after it) and **unhappiness** stop migrants ("Migrants refused to journey to such a
  dangerous fortress").
- Migrant skills follow the fortress's needs: skills it uses a lot or lacks entirely are more
  likely; mining, food and basic crafts are weighted up (Tarn, DF Talk 12).
- Settings: population cap and strict population cap (also stops births).
- Strange moods need a population of at least 20.
- Visitors (v50 [mem]) come to taverns/temples/libraries and may petition for residency.

## 2. Nobles, administrators, mandates, demands

[wiki Noble, Mandate]:

- **Expedition leader** at embark; with it you can appoint bookkeeper, broker, chief medical
  dwarf, hammerer, manager, militia commander, sheriff. At population **50** a **mayor** is
  elected (replacing the leader); then captain of the guard and dungeon master become available.
- Promotion to **barony** and up (count, duke, monarch — the fortress becomes the mountainhome) is
  offered by the civilization and gated by population and wealth (Baron: population 20 and created
  wealth ~100k–200k in the wiki's table [wiki Noble — table partly read]). It is an optional
  victory goal; each step brings better caravans and more demanding nobles.
- Leaders absorb stressed dwarves' complaints (`04-…` §5).
- **Mandates**: production orders (3–6 months to fulfil) and export bans; mayor/baron 1, count
  2, duke 3, monarch 5 at once. Failing one → unhappy noble and punishment of a dwarf by the
  justice system.
- **Demands**: nobles want rooms of a minimum rank and possessions (cabinets, armor stands …),
  and some resent lower-ranked dwarves having better rooms.
- **Justice**: crimes (violating mandates, theft, assault), sheriff/captain of the guard,
  hammerer, prison sentences [mem].

## 3. Caravans and trade

[wiki Caravan, Trading, Calendar]:

- Caravans arrive by **season**: elves spring, humans summer, dwarves (your civilization)
  autumn, none in winter; others via raids "demanding tribute" [wiki Calendar, Caravan].
- They enter at the map edge with pack animals (and **wagons**, which need ramp access and 3-wide
  paths [mem]), go to the **trade depot**, and leave 30,000–40,000 ticks (25–33 days) after
  arrival; they bypass the fortress if they cannot path to the depot, and turn back if spooked or
  ambushed.
- Trading needs a depot, peace with a civilization that can reach the site, and a trader at the
  depot (a broker with appraisal skill gives exact values). Barter: the merchant accepts if the
  value received exceeds value given by an acceptable profit margin, which depends on the broker's
  social skills and the merchant's mood; counter-offers exist.
- Caravans report the fortress's wealth home — which unlocks later migrant waves (§1).
- A **liaison** comes with the civilization's caravan to negotiate next year's goods and offer
  noble promotion [mem].

## 4. Diplomacy

[mem] + [wiki Siege]:

- Diplomats from other civilizations (elves: tree-cutting quotas; humans: trade agreements);
  insulting them or exceeding quotas → war.
- War status decides who sends **sieges** and **ambushes** (`10-…`). Civilizations need a
  physical route to the site (30-tile interaction radius on the world map; islands are safe from
  sieges).
- v50 added world-map **missions**: send squads to raid, demand tribute, recover artifacts,
  rescue [mem; also referenced indirectly by "sending a squad out for a mission" in
  wiki Maximizing_framerate].

## 5. Simulation complexity

All of this is **event-driven at season granularity**: a scheduler fires "season start" →
roll migrant wave date and size, roll caravan arrival, roll invasions. The cost lies in the
agents those events spawn (migrants, merchants, guards, animals) — each is a unit with full AI.
The coupling to the world is a handful of numbers: civilization population/sites (do migrants
exist?), which neighbours can reach the site, at peace or war.

## 6. MVP tiers

| system | tier |
|---|---|
| Two hardcoded migrant waves, then seasonal waves sized by created wealth | **MVP** (the first two waves only; wealth-driven waves soon after) |
| Migrants spawn at the map edge, walk to the meeting area | **MVP** |
| Expedition leader (as a named role, no mechanics) | **MVP** |
| Mayor election, administrators (broker, bookkeeper, manager) | soon after |
| Caravans + trade depot + barter | soon after |
| Nobles' mandates, demands, justice | later |
| Diplomacy, wars, missions | later (needs a world-level civ layer) |
| Promotion to barony → mountainhome | later |

## Sources

- https://dwarffortresswiki.org/index.php/Immigration
- https://dwarffortresswiki.org/index.php/Noble
- https://dwarffortresswiki.org/index.php/Mandate
- https://dwarffortresswiki.org/index.php/Caravan
- https://dwarffortresswiki.org/index.php/Trading
- https://dwarffortresswiki.org/index.php/Calendar
- https://dwarffortresswiki.org/index.php/Siege
- http://www.bay12games.com/media/df_talk_12_transcript.html (cited by the Immigration page; not read)
