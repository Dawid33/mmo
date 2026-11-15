# Errantry — game design

The home of the game's direction: what the game is, who plays it and why each system exists. It says
*what* and *why*; *how* lives in `docs/superpowers/{specs,plans}/`. When a design decision is made,
record it here; when a plan contradicts this document, one of them is out of date.

Status of each statement:

- **Decided** — stated by Dawid (first written down 2026-11-14).
- **Proposed** — a suggestion that has not been accepted yet. Do not build on it as if it were settled.
- **Open** — known to be undecided.

Sections 1–6 are decided unless marked otherwise.

## 1. The game in one paragraph

A Dwarf Fortress–style voxel world, forged up front with geology, climate, biomes and history, shared
by two kinds of player. **Adventurers** walk the world in a body, take quests and earn renown.
**Fortress players** direct a settlement from above and try to keep it from falling to ruin. Each loop
gives the other something to do: settlements are places adventurers visit and help, and adventurers
solve the troubles a settlement cannot solve alone.

## 2. Pillars

1. **Preparation is the game, not progression.** A character's power is capped early and can be
   re-shaped. Getting ready for a specific challenge — skills, items, companions — is the loop.
2. **No power creep.** A quest sized for one player can, in principle, be completed by a brand-new
   character who has reached the cap, with the right preparation and skill.
3. **Two loops, one world.** Adventure and fortress play are symbiotic; neither is a side mode.
4. **The world exists without you.** Sites, history and people are generated and go on; players act
   inside them.
5. **Fortress play is about aesthetics, roleplay and survival**, not a win condition.

## 3. Adventure mode

### 3.1 Goal and loop

An adventurer's goal is **prestige and renown, gained by completing quests**.

```
look for a quest → attempt it → fail → prepare → attempt again → … → succeed → renown
```

Failing first and coming back prepared is the expected path, not a punishment.

**Renown** is a score, akin to points. It does not affect gameplay, except that it may gate certain
quests — and that is unlikely.

### 3.2 The character

An adventurer has **skill trees, a level and an inventory**.

- **Inventory** is small — largely the hotbar.
- **Mastery** in a skill tree rises by performing actions in the game. Pushing mastery in one tree
  makes mastery in the others go down.
- **Levels** give **skill points** to spend on skills. A spent point is gone; it comes back only by
  pursuing mastery in something else, which lowers mastery in the tree the skill belongs to and frees
  the point for reuse.
- Each level also **raises the total mastery** a character can hold across all trees.

### 3.3 Why it works this way

- **The level system is a tutorial and a cap.** It brings players up to speed and unlocks things
  progressively. The cap is low — everyone is expected to reach it within a few hours. *The cap is
  where the game truly starts.*
- **The cap bounds a player's power**, and gives later systems something to adjust: effects that raise
  or lower a character's level or effective level.
- **Skills are a puzzle to solve, not a ladder to climb.** A character can only hold so many skills;
  a quest poses a challenge that the player must fit skills and items to.
- **Quests depend on the current skill set, not absolute level.**
- **It makes parties matter.** Adventurers whose skills complement each other can solve harder quests.

"Level cap" is the wrong phrase for this — it suggests games where the cap is the end. Better wording
is wanted (see 7.1).

## 4. Fortress mode

- The player is **a spirit inhabiting the land**, directing what is built and where. The spirit does
  not communicate directly; it is a thematic stand-in for the commander's view. (Better framings are
  welcome — see 7.1.)
- The goal is to **manage a village or group of people or creatures without it falling to ruin**.
  There is no defined goal besides survival.
- The mode is about **aesthetics, roleplay, building, and being dealt challenges to the group's survival**.
- **Some troubles can only be solved by an adventurer** — for example a monster that decides to nest
  in the region.
- What can be built: a fortress, village, town or dungeon in the long run; **a fortress for now and
  the foreseeable future**.

## 5. How the two loops meet

| | Adventurers get | Fortress players get |
|---|---|---|
| Multiplayer | player-built places to visit and quests raised by their troubles | player adventurers who come and solve those troubles |
| Single player, adventure | **pre-generated towns** and other sites | — |
| Single player, fortress | — | **AI-controlled adventurers** |

Long term, what fortress players build becomes things for adventurers to do in the world, and that
should reward the fortress player in some way. How is open (7.3).

## 6. The world and its sites

- A huge world forged up front; players play regions of it (see `CLAUDE.md`, "Direction").
- **Procedurally generated sites**: **dungeons, towns and dwarven fortresses**.
- **Minecraft worlds and builds can be imported** through a conversion step, as another source of places.

## 7. Not settled

### 7.1 Proposed — names

Suggested in `docs/superpowers/plans/2026-11-14-adventure-gameplay-roadmap.md`; none accepted yet.

| Concept | Proposal |
|---|---|
| The levelling phase / a level | Squirehood / a step |
| Reaching the cap | Being dubbed an Errant |
| Total mastery a character can hold | Focus |
| Skill point | Knack |
| The fortress player | Genius loci — "the Warden of <site>" |

### 7.2 Proposed — mechanics

The same roadmap proposes concrete rules for the systems above. The ones that most need a decision:

1. The exact mastery rules: one shared budget, gain by use, a gain drains trees the player has
   released, a skill lapses and refunds its point when its tree falls below its requirement.
2. Defeat is not death: the adventurer wakes at their last rest point with their kit.
3. Quests are world objects, closed for everyone when completed, rather than private copies.
4. Inventory is the 9-slot hotbar plus three worn slots; abilities have their own small bar.
5. Quests carry challenge tags with known counters; the notice board shows only rumours and each
   attempt reveals more.
6. A fortress trouble is simulated only while its region is running.

### 7.3 Open

- How fortress players are rewarded when adventurers use what they built.
- The goals of player-made dungeons compared with player-made towns.
- Player visits, ratings and similar for fortresses — out of scope for a long time.
- Whether renown ever gates quests.
- What changes a character's effective level, and when.
- How party size changes a quest.

## 8. Related documents

- Adventure gameplay roadmap (proposal): `docs/superpowers/plans/2026-11-14-adventure-gameplay-roadmap.md`
- Dwarf Fortress research: `docs/superpowers/specs/2026-10-20-dwarf-fortress-research/00-synthesis.md`
- Adventure mode as built: `docs/superpowers/plans/2026-10-30-adventure-mode.md`,
  `docs/superpowers/plans/2026-11-05-adventure-survival-items.md`
- World history and sites as built: `docs/superpowers/specs/2026-11-01-world-history-events-design.md`
- Current work: `TODO.md`
