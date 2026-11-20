# Errantry — game design

The home of the game's direction: what the game is, who plays it and why each system exists. It says
*what* and *why*; *how* lives in `docs/superpowers/{specs,plans}/`. When a design decision is made,
record it here; when a plan contradicts this document, one of them is out of date.

Status of each statement:

- **Decided** — stated by Dawid.
- **Proposed** — a suggestion that has not been accepted yet. Do not build on it as if it were settled.
- **Open** — known to be undecided.

Sections 1–8 are decided unless marked otherwise.

**Revision 2026-11-16.** Rewritten from Dawid's notes (`docs/vault/Game Design.md`) and his answers to
their review. It replaces the 2026-11-14 version's *two loops* (an adventurer and a separate,
disembodied fortress player) with *one character who may settle*. It also replaces renown as a score
with a place in the world's history, and quests generated from world state with quests authored as
packages. What the change removes from the code is listed in §10. The earlier version is in git history.

## 1. The game in one paragraph

A Dwarf Fortress–style voxel world, forged up front with geology, climate, biomes, history and sites.
You play **one character, in first person**, and role-play them in that world. You explore, find
quests, prepare for them and complete them. Completing them moves the **world's story** forward
through its acts and **records you in the world's history**. Along the way you gather things worth
keeping, so you **claim land** and make it your home. If you take people in, the home becomes a
**settlement**, and you run it as its **mayor**.

## 2. Pillars

1. **Progression is story.** Quests and interactions are how the game advances. The reward is the story
   moving towards completion and your part in it being remembered, not a number going up.
2. **Each quest is a small puzzle.** It asks for exploration and an understanding of the game's
   mechanics. **Preparing is most of the gameplay**: getting the item, crafting it, delivering it, and
   fitting your skills and kit to the task.
3. **No power creep.** A character's power is capped early and can be re-shaped (§3.2). A quest sized for
   one player can be completed by any character at the cap, given the right preparation.
4. **One character, from wanderer to mayor.** Settlement play grows out of adventure play, in a body.
   There is no separate mode with a disembodied camera, and no embark.
5. **The world exists without you.** Sites, history and people are generated and go on; players act
   inside them.
6. **Content comes in packages.** Quests, dialogue, prefabs, site generation and the world's story are
   authored as packages and enabled for a world. They are never created from live play (§7).

## 3. Adventure

### 3.1 The flow

```
explore → discover a quest → prepare → complete → (the quest may reset for the next player)
```

- **Explore.** Exploring yields items and materials. That pushes you to settle (§5), so your things are
  protected and kept in one place. Early on you raise skills and are nudged to try things out, possibly
  through quests that are simple ones in disguise.
- **Discover a quest.** A side quest tied to a place, or one that belongs to the world's story.
- **Prepare.** Most of the game. If the quest is to bring an item somewhere, getting, crafting and
  delivering it is the gameplay. You adjust skills and items to suit the task.
- **Complete.** You may try, fail, prepare and succeed, or prepare and succeed at the first attempt.
  Neither path is the intended one.

### 3.2 The character (unchanged from 2026-11-14)

An adventurer has **skill trees, a level and an inventory**.

- **Inventory** is small — largely the hotbar.
- **Mastery** in a skill tree rises by performing actions in the game. Pushing mastery in one tree
  makes mastery in the others go down.
- **Levels** give **skill points** to spend on skills. A spent point is gone; it comes back only by
  pursuing mastery in something else, which lowers mastery in the tree the skill belongs to and frees
  the point for reuse.
- Each level also **raises the total mastery** a character can hold across all trees.

Why: the levels are a tutorial and a cap, and the cap is low — reached within a few hours, *where the
game truly starts*. Skills are a puzzle to solve, not a ladder to climb. Quests depend on the current
skill set, not absolute level. Parties matter, because complementary skills solve harder quests.

### 3.3 Renown

Renown is **not a score**. Completing quests makes you part of the world's history: a legends entry
that names you, in the world that outlives your session. Renown matters only in that soft way.

## 4. Quests

### 4.1 Types

| Type | Can be completed | On completion |
|---|---|---|
| **Repeatable** | again and again, by different players | the quest's area and conditions reset for the next player |
| **One-off** | once per world (per world-story loop, §4.2) | closed for everyone, recorded in history |
| **World-line** | as a one-off | as a one-off, and it unlocks the next part of the world's story |

A world-line quest is a one-off quest in its technical form. What makes it world-line is that its
completion — or the satisfying of its requirements followed by its completion — advances the world's
story. **Decided 2026-11-16:** a quest is world-line because an **act lists it**. The quest's own source
says only repeatable or one-off.

**Decided 2026-11-16:** a repeatable completion is recorded per (quest definition, player) — a count and the
last time. The world's history and legends name a player only for their **first** completion of a quest and
when the count crosses a milestone (such as 10 or 100). How: `docs/superpowers/specs/2026-11-16-quest-construction-design.md` §9.

### 4.2 The world's story and its acts

- The world has a **main quest line, divided into acts**. Every player's goal is to complete quests,
  for their own advancement and for the world's story.
- **Single player:** acts are **quest-gated**. The story is completed normally, and finishing an act's
  world-line quests opens the next act.
- **Multiplayer:** acts are **time-gated, not time-started**. Time passing never opens an act on its
  own. An act opens only when its quests are done **and** a minimum time has passed, so players can play
  the world in their own time before the story moves on. (This reading of "time-gated, not time-started"
  is Claude's; correct it if wrong.)
- The quest line may **loop**. After the last act there may be a grace period, and then the line
  restarts. How a later loop differs, "harder" or otherwise, is **open**.

### 4.3 Where quests come from

Quests are **written as packages** (§7), with the game's script language. A quest definition says what
it needs to start, such as a person, a place or a site of a kind. While the world runs, the world
coordinator looks for places that meet those needs, **near where players are playing live**, since
spawning quests where nobody plays is wasted. There it instantiates the quest. From then on the quest's
own script runs in the region and manages it; when the quest ends, the script removes itself. Sites are
generated during the forge; quests appear only after it.

Mechanism: `docs/superpowers/specs/2026-11-16-quests-in-the-world-design.md`.

## 5. Settling

- **Claiming land.** A player claims a region, which becomes their **personal site, their home**. The
  owner decides who may break blocks there and who may perform certain interactions. That protection is
  the point of settling.
- **Becoming a settlement.** If the owner accepts people in, the home becomes a settlement. People
  **join over time or are born there**; nobody arrives by embark.
- **The mayor.** The owner manages the settlement as its mayor, predominantly **in first person**, with
  an **optional top-down view** (the existing commander camera becomes that view).
- **What it is about:** building, aesthetics, role-play, and keeping the settlement from falling to
  ruin. As before, there is no win condition.
- **Growing:** the town ranks and town lands of
  `docs/superpowers/specs/2026-11-14-town-ranks-multi-region-fortress-design.md` still describe how a
  settlement grows past its first region. Read its "fortress" as "settlement" and its player as the
  mayor; any part of it that assumes embark is gone.
- **Troubles.** A settlement's troubles can still need adventurers. In the package model a trouble is a
  quest whose start requirements name a settlement in that state (§4.3).

**Open:** how many regions one player may claim; what happens to a claim whose owner stops playing; what
"accepting people" means for other players compared with generated people; whether single-player
settlements get AI adventurers (the old design's answer to troubles nobody takes).

## 6. Modes and permissions

| Mode | Who | What |
|---|---|---|
| **Adventure** | everyone; the default | the game: §3–§5 |
| **Editor** | **admins only, a per-world permission** | a creative mode, like Minecraft's, for testing and making generation, prefabs and quests: it edits the scripts behind script blocks |

The mayor's top-down view is a view inside adventure play, not a mode.

The unapproved creative-mode design (`docs/superpowers/specs/2026-11-14-creative-mode-and-chat-design.md`)
is the starting point for editor mode. Its availability to every player in both old modes no longer
holds.

## 7. Content packages

A **package** is the unit of authored content. It can add:

- quests (repeatable, one-off or world-line), with their scripts and start requirements;
- dialogue trees for those quests, in a standard format;
- prefabs for the forge to place when it makes sites — a prefab can hold entities and scripts;
- code that directs world generation (site generators);
- acts, and additions or edits to the world's quest line.

- Packages are **enabled for a world**, at the forge or after it. Enabling or disabling worldgen
  content after the forge is allowed: making sure that this does not break the world is the
  responsibility of whoever enables or disables it. A quest whose structure a disabled package would
  have generated simply never meets its start requirements.
- Dependencies are declared, resolved by **semantic versioning** while developing, and **pinned by hash**
  in a lockfile and in every exported package.
- **Clients receive every package the world uses**, since they run its scripts too.
- A **package server** — its own deployment, on the management PostgreSQL instance — lets people upload,
  search, browse and download packages, and enable them for a world.
- Rewards are whatever a package's quests grant. Because quests are never made in live play, a player
  cannot grant themselves or others anything except through a package that is broken or malicious to
  begin with. Enabling a package is the trust decision.

Mechanism: `docs/superpowers/specs/2026-11-16-content-packages-design.md`.

## 8. The world and its sites

- A huge world forged up front; players play regions of it (see `CLAUDE.md`, "Direction").
- **Sites** — dungeons, towns, fortresses — are generated during the forge, by the engine and by enabled
  packages' site generators and prefabs.
- **Minecraft worlds and builds can be imported** through a conversion step, as another source of places.

## 9. Scale

An MMO needs a vast number of quests and things to do; packages are how they are supplied. One idea for
stretching play is to slow mastery gain when many players are on. It is **only an idea**, not pursued
unless quest supply turns out to be a real problem.

## 10. What this direction removes

Decided to go:

- **Embark and return**, and everything only they use: choosing a site and party, the embark screen,
  the coordinator's embark/return requests, the `embarked` debug launch presets and their recipes.
  **Done 2026-11-17**: `docs/superpowers/plans/2026-11-17-embark-removal.md`.
- **Fortress mode as a separate, disembodied mode**, and the "spirit inhabiting the land" framing. The
  commander camera stays as the mayor's optional view (§5).
- **Renown as a points score** (roadmap §5.4).
- **Player-owned script namespaces** (quest-script spec §8.1): a namespace now belongs to one quest
  instance.
- **Players typing scripts into blocks in live play.** Script blocks become admin access points (§6).

Embark reached into `game` (coordinator, history, protocol), `server`, `client` and `slint-ui`, so its
removal had a plan of its own (above), written before anything was deleted. Removals go before new features
that would otherwise be built around them.

## 11. Not settled

### 11.1 Proposed — names

From `docs/superpowers/plans/2026-11-14-adventure-gameplay-roadmap.md`; none accepted yet.

| Concept | Proposal |
|---|---|
| The levelling phase / a level | Squirehood / a step |
| Reaching the cap | Being dubbed an Errant |
| Total mastery a character can hold | Focus |
| Skill point | Knack |
| A player's claimed land, in code | a **holding** — "claim" already means a host's super-regions |

### 11.2 Proposed — mechanics

From the same roadmap, still waiting for a decision:

1. The exact mastery rules: one shared budget, gain by use, a gain drains trees the player has
   released, a skill lapses and refunds its point when its tree falls below its requirement.
2. Defeat is not death: the adventurer wakes at their last rest point with their kit.
3. Inventory is the 9-slot hotbar plus three worn slots; abilities have their own small bar.
4. Quests carry challenge tags with known counters; a notice board shows only rumours and each attempt
   reveals more.
5. A settlement's trouble is simulated only while its region is running.

Superseded by this revision: the roadmap's quests as coordinator-generated world objects (§5.1), renown as
a number (§5.4), and the AI-adventurer half of troubles (§7, now open, §5).

### 11.3 Open

- How a later loop of the world's story differs (§4.2).
- The claim questions of §5.
- What a script block shows to a non-admin in adventure play, if anything.
- What changes a character's effective level, and when.
- How party size changes a quest.
- Player-made dungeons and towns as content for others: now a matter of packages, or of settlements
  that packages' quests can target.

## 12. Related documents

- Quests in the world (lifecycle, script instances, namespaces, acts):
  `docs/superpowers/specs/2026-11-16-quests-in-the-world-design.md`
- Content packages and the package server: `docs/superpowers/specs/2026-11-16-content-packages-design.md`
- Quest script language (MVP, standalone): `docs/superpowers/specs/2026-11-16-quest-script-language-design.md`
- Town ranks and lands: `docs/superpowers/specs/2026-11-14-town-ranks-multi-region-fortress-design.md`
- Creative mode (the start of editor mode): `docs/superpowers/specs/2026-11-14-creative-mode-and-chat-design.md`
- Adventure gameplay roadmap (proposal, partly superseded): `docs/superpowers/plans/2026-11-14-adventure-gameplay-roadmap.md`
- Dwarf Fortress research: `docs/superpowers/specs/2026-10-20-dwarf-fortress-research/00-synthesis.md`
- World history and sites as built: `docs/superpowers/specs/2026-11-01-world-history-events-design.md`
- Dawid's working notes: `docs/vault/Game Design.md`
- Current work: `TODO.md`
