# 13 — Adventure mode and region traversal (brief, for later)

Part of the Dwarf Fortress research (2026-10-20); start at `00-synthesis.md`. Source tags as in
`01-world-and-embark.md`. Purpose: make sure the fortress MVP doesn't paint us into a corner.

## 1. What adventure mode is

[wiki Adventurer_mode]: a roguelike in the same generated world. The player creates a character
(or party) of any playable race, travels anywhere, visits towns, caves, lairs, towers, and
**previously retired or abandoned fortresses**, takes quests, fights. Building is not available
in current adventure mode; crafting is limited. Retired adventurers keep acting in the world and
appear in legends.

## 2. How movement and loading work

- **Local movement**: tile by tile on the local map around the adventurer, in adventure time
  (one tick = 0.5 s; a day is 172,800 ticks) [wiki Time, Adventurer_mode].
- **Fast travel** (`T`): move across the *world map* at coarse resolution; the same amount of game
  time passes, and travel can be interrupted (ambush) [wiki Adventurer_mode]. Leaving a site with
  stolen goods blocks fast travel until you're far enough away.
- The local area is loaded **as a window around the adventurer that shifts as they move**,
  generated from the world fields (and persisted where sites exist) [mem: a window a few embark
  squares wide; exact size unverified].
- Sites (towns, fortresses) are loaded when entered; the player's own past fortress is a site
  like any other, with its built tiles and surviving inhabitants.

## 3. Facts that constrain the fortress MVP

1. **A fortress is a site in world coordinates that persists after the player stops.**
   Retire/abandon leaves it in the world; adventure mode visits it. → Fortress state must be
   *durable* and keyed by world position (the engine's parking lot is in-memory only today).
2. **One unit model for everything.** The adventurer is a creature like a dwarf (same body,
   skills, attributes). → Make the fortress unit generic over creature raws from day one; the
   player-controlled adventurer is "a unit whose job source is player input". This matches the
   standing direction "players are entities like NPCs" (memory `game-direction-entities`).
3. **Two time scales.** Fortress ticks are 144× coarser than adventure ticks, and DF *never runs
   both at once*: entering adventure mode means the fortress is not being played. If our
   multiplayer future wants an adventurer walking into another player's live fortress, one region
   would host two time scales at once — a hard design problem DF avoids. Flag it now; don't solve
   it in the MVP.
4. **Region streaming + handoff is the adventure-mode machinery.** Our near window, LOD rings,
   region handoff, cell loading and `just fly-probe` / `shuttle-probe` exist for exactly the
   adventurer's case (one avatar crossing many regions). The fortress MVP needs almost none of
   it (one region, no crossings). → Keep it compiling and tested, don't route the fortress
   through it, don't delete it.
5. **Prediction matters for adventure, not for fortress.** An adventurer is a directly
   controlled avatar — the case client-side prediction/rollback was built for. The fortress is
   indirect control. → The netcode choice for fortress mode (`engine-mapping.md` §3) should leave
   the rollback path intact for adventure mode.
6. **The world map is shared.** Embark selection and fast travel use the same world map view.
   → Build the world-map screen once (phase 1 of the plan) and reuse it.

## 4. Tiers

All of adventure mode is **later**. The MVP obligations are the six constraints above, of which
only (1) durable fortress state and (2) generic units cost anything up front.

## Sources

- https://dwarffortresswiki.org/index.php/Adventurer_mode
- https://dwarffortresswiki.org/index.php/Time
- Memory note `game-direction-entities` (players are entities like NPCs).
