# 10 — Threats, military and combat (at MVP depth)

Part of the Dwarf Fortress research (2026-10-20); start at `00-synthesis.md`. Source tags as in
`01-world-and-embark.md`.

## 1. The threat ladder

How DF escalates pressure on a growing fortress:

| threat | when | source |
|---|---|---|
| **Wildlife** (surface animals, savage-biome monsters, vermin) | from day one; set by biome savagery and alignment | [mem]; biome rules in the worldgen research |
| **Thieves / kobolds**, snatchers | early | [wiki Ambush, mem] |
| **Ambushes**: small enemy squads (6–10 each), hidden until detected; usually in the first 15 days of a season, often with caravans; elves can send 50+ | not tied to population | [wiki Ambush] |
| **Sieges**: an army (commander + squads, sometimes mounted) that wants to exterminate the fortress | civilizations at war begin sieging at **population 80** (undead any time); the army must be able to reach the site (30-tile radius on the world map) | [wiki Siege] |
| **Megabeasts** (dragons, hydras, rocs, colossi) and **titans** | "most megabeasts will not attack the player until they have accumulated a minimum wealth of 100,000☼, exported wealth of at least 10,000☼, and a minimum population of 80"; titans ignore exports | [wiki Megabeast] |
| **Forgotten beasts** (procedural, from the caverns) | attracted by wealth and noise; one spawned per underground region; all are building destroyers | [wiki Forgotten_beast] |
| **Cavern creatures**, **night creatures**, **werebeasts** (full moons), **necromancers/undead** | breaching caverns; evil biomes; world history | [wiki Cavern, Calendar] |
| **The underworld** | digging into it | [mem] |

Scheduling: "Megabeast and night creature attacks are handled the same way as caravan/diplomat
visits and migrant waves — at the beginning of the season, the game decides that it's going to
happen and sets a timer" [wiki Megabeast]. The whole outside world reaches the fortress through
**one seasonal event scheduler**.

## 2. Military

[wiki Squad] + [mem]:

- Appoint a **militia commander** (first squad's captain); further squads get captains. A squad
  holds up to **10** dwarves [mem], one uniform (weapon, armor, clothing preferences), a
  **schedule** per month (train, station, patrol, off duty, minimum soldiers on duty).
- Orders: **station** at a location, **kill** a target, patrol routes, move, defend a burrow;
  v50 adds world-map **missions** (raid, demand tribute, recover artifacts).
- Training happens in barracks (sparring, individual drills) and archery ranges; skill gain uses
  the normal skill system (`04-…` §2); military duty affects needs and stress.
- Civilians **flee** from visible hostiles (alarm/burrow orders send civilians to safety).
- Equipment: weapons, shields, armor from the forge chain (`05-…` §4); quality and material
  matter.
- Siege engines (ballista, catapult), traps (`07-…` §4) and drawbridges/moats are the
  non-soldier defence; most players lean on chokepoints, cage traps and drawbridges early.

## 3. Combat model

[wiki Combat]:

- **No hit points.** Bodies are body-part trees with tissue layers (`04-…` §10); damage resolves
  per layer from the attack's momentum (mass × velocity; velocity from weapon mass, wielder
  strength and the weapon's velocity multiplier) against the layer material's yield/fracture
  properties (`05-…` §3); edged attacks depend on contact area and penetration, blunt on mass and
  contact area. Armor deflects. Momentum transfers to neighbouring parts (a blow to the head can
  hurt the neck).
- Outcomes: bleeding, pain (unconsciousness), fractures, severed limbs, lodged weapons (wrestling
  for control), organ damage, suffocation from pierced lungs, death by blood loss.
- Defence: dodge (skill), block (shield), parry (weapon); attack types quick / heavy / precise
  change prepare/recover time and accuracy.
- Every exchange produces a detailed combat log — a big part of the storytelling.
- **Line of sight** for "friend or foe" checks is the slowest system in v50 (O(n²), bounded at
  26 tiles) [wiki Maximizing_framerate].

## 4. Simulation complexity

- Combat itself is local and short-lived; cost is per attack exchange.
- The expensive parts are the *sensing* (who sees whom, every tick, for every unit) and the
  invader AI pathing into the fortress (shortest path, so traps on the short route catch sieges
  while caravans take the depot route [wiki Path]).
- Sieges add dozens to hundreds of units at once — the largest unit-count spikes a fortress sees;
  DF offers an invasion cap setting [wiki Maximizing_framerate].

## 5. MVP tiers

| system | tier |
|---|---|
| Hostile wildlife (one surface creature type) that attacks dwarves | **MVP** (the first threat) |
| Combat: attack/defend with a wound list per body region, bleeding, death | **MVP** (minimal, `04-…` §10) |
| Civilians flee hostiles they can see (bounded-radius sensing through a spatial hash) | **MVP** |
| Corpses, burial (coffin + tomb zone) | soon after |
| Squads: create, assign, equip a weapon, kill/station orders | soon after |
| Ambushes (small goblin squads at a season start) | soon after |
| Cage traps | soon after |
| Schedules, uniforms, training, barracks | later |
| Sieges at population 80 | later |
| Megabeasts, forgotten beasts, titans | later (need history + caverns) |
| Full tissue/material combat | later |
| Siege engines, missions | later |

## Sources

- https://dwarffortresswiki.org/index.php/Siege
- https://dwarffortresswiki.org/index.php/Ambush
- https://dwarffortresswiki.org/index.php/Megabeast
- https://dwarffortresswiki.org/index.php/Forgotten_beast
- https://dwarffortresswiki.org/index.php/Squad
- https://dwarffortresswiki.org/index.php/Combat
- https://dwarffortresswiki.org/index.php/Path
- https://dwarffortresswiki.org/index.php/Cavern
- https://dwarffortresswiki.org/index.php/Calendar
- https://dwarffortresswiki.org/index.php/Maximizing_framerate
