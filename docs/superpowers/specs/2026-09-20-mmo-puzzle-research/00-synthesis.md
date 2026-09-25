# What would an MMO puzzle game look like? — research synthesis (2026-09-20)

Status: **research only, nothing decided, no code.** Four web-research passes (MMO-scale
puzzle games, small-group co-op puzzlers, voxel/sandbox/UGC puzzle ecosystems, design
theory) condensed in `01-evidence.md` with sources and verification flags. This file is
the answer, read against the direction already on paper
(`../2026-09-20-nonviolent-game-direction-brainstorm.md`: strictly non-violent,
cozy + exploratory, Wayfarers charting finite pre-generated *lands*, lodges, stamp book,
beacon chains; in-sim deterministic wasm mods; players are entities like NPCs).

Evidence quality: the case histories and designer quotes are sourced; many sales and
population figures are search-snippet numbers the agents could not confirm, and several
mechanics details are from the agents' memory. `01-evidence.md` keeps the flags. Sections
3–7 below are my inference from that evidence, not something a source states.

## 1. The short answer

The obvious version — "Myst, but massively multiplayer" — has been built three times
and failed three different ways:

| Attempt | What it was | How it failed |
|---|---|---|
| Uru / Myst Online (2003, 2007) | hand-authored Ages, shared city as lobby | **content rate**: months to build, hours to solve; cancelled twice for lack of subscribers; the community outlived the content |
| The Secret World (2012) | ARG-style investigation missions | **spoilers**: a handful of solvers wrote guides, everyone else followed them; praised, expensive, commercially weak |
| Islands of Insight (2024) | ~10,000 mostly generated puzzles in a shared world | **no interdependence**: other players were visible but there was no way to collaborate, so the online part was pure cost; servers closed within the year, game converted to offline-only |

Generation fixed Uru's content-rate problem and Islands of Insight still died, which is
the most useful single data point: *volume of puzzles is not the missing ingredient —
a reason for the other players to be there is.*

Persistent puzzle worlds that did last are all something other than a box of authored
puzzles:

- **Puzzle Pirates** (2003–, still running): every verb is a skill minigame; each crew
  member's puzzle performance feeds one shared ship; an economy is the retention layer.
- **A Tale in the Desert** (2003–, low thousands of players): the "puzzles" are social and
  systemic Tests — trust games, elections, player-built puzzles rated by players — in a
  world that ends and restarts every couple of years.
- **Sky: Children of the Light**: trivial puzzles as a pretext for wordless, strictly
  positive-sum cooperation between strangers. By far the largest audience of the three.

Meanwhile the real demand for "puzzling together" is met by finite 2–4 player games
(It Takes Two, Split Fiction, Escape Simulator, We Were Here). So an MMO puzzle game that
works looks like: **small groups (2–6) doing spoiler-proof puzzles inside a large shared
world whose state they change, with community-scale one-off mysteries as seasoning** —
not thousands of people solving the same riddle.

## 2. Which kinds of puzzle survive an MMO

Koster's framing explains the table: a puzzle is a problem with one answer; once the
pattern is learned it is dead, and in an MMO it is learned once *for the whole player
base*. His content arithmetic (developer-hours to play-hours of 1:27 for a MUD quest,
~1:460 commercially) makes authored single-answer puzzles the worst content unit an
online world can have. What matters is whether knowing the answer ends the fun.

| Puzzle kind | Survives a wiki? | Why | Examples |
|---|---|---|---|
| **Insight / knowledge** | no | the revelation *is* the content | Uru, TSW, Outer Wilds |
| **Execution / skill** | yes | knowing ≠ doing | Puzzle Pirates, GW2 jumping puzzles, raid mechanics |
| **Generated instance** | partly | wiki teaches the method, not the answer; feels samey; solver mods eat anything with visible computable state | RuneScape clues, Hypixel dungeon rooms, Keep Talking |
| **Coordination** | yes | a known plan still has to be performed by N people; after mastery it becomes a social ritual | Destiny/FFXIV raids, WoW Hivemind mount |
| **Construction / optimisation** | yes | open-ended spec, many solutions; a score doesn't reveal a build | Infinifactory histograms, redstone, Create mod |
| **Social** | yes | other people are the uncertainty | ATITD Tests |
| **Distributed fragments** | one-time | each player holds a unique shard; the only kind that gets *better* with more players | Destiny's Corridors of Time, Animal Well's 50-player mural |

Co-op mechanics sort the same way by scale. Parallel decomposition, distributed
fragments, bodies-as-tools and shared mechanisms scale to a crowd. Complementary
abilities and tight timing scale only softly (helpful roles, never mandatory; windows of
seconds, not frames). Pure asymmetric information (We Were Here) does not scale at all:
single-use, needs a trusted voice partner, hard-blocks on disconnect — it survives only
in the Keep Talking form, where instances are generated and the "manual" is static.

## 3. What this project can do that the failed attempts could not

Three properties of this codebase line up with the surviving puzzle kinds.

**A deterministic, server-authoritative simulation makes the world itself the puzzle
engine.** Teardown had to split its multiplayer into deterministic destruction commands
plus state-synced bodies; here everyone already agrees on every outcome. That enables
three things almost for free once clears are recorded as input logs:
- *verified solutions* — the server replays a claimed solve;
- *Infinifactory-style histograms* per puzzle (blocks used, time, provisions) — already
  in the brainstorm as per-trail histograms with ghost replays;
- *proof of solvability for player-made puzzles* — Mario Maker's "creator must clear it"
  rule, enforced by replay.

**Finite lands answer the "latecomers inherit a solved world" problem without ATITD's
wipe.** ATITD needs Tellings because collective, world-changing goals are consumed once.
Opening a new land renews the frontier (first landing, first relit beacon) while settled
lands stay home. A land is a Telling that never has to be deleted.

**Forge-time history is an offline, unlimited-CPU puzzle generator.** The proven
generation pattern is "authored rules and rooms, procedural assembly, randomised
parameters" with generate-and-test. The land forge already plans generate-and-test for
hydrology and lodge sites; placing ruins, mechanisms and clue chains is the same pass,
and a solver can reject unsolvable or trivial layouts before anyone sees them.

One property cuts the other way, and it is specific to rollback netcode: **the client
runs the simulation, so every bit of in-region sim state is machine-readable on the
player's machine.** WoW's addon arms race and Hypixel's solver mods show that any puzzle
whose state is visible and computable gets solved by tooling. Hidden-state puzzles
(which pad is the right one, what is behind the wall) therefore cannot live in the sim.
Either the secret stays server-side in the derived layer (a verdict, not a state), or —
better — difficulty lives in construction, execution and coordination, where a solver
showing the answer still leaves the doing.

## 4. The design tensions, in prose

**Shared state vs. griefing.** The research's strongest structural conclusion is that
per-player puzzle state is where Uru and Islands of Insight ended up, and it is what made
their worlds pointless to share; solving should change the *shared* world. But a shared
half-solved puzzle is a griefing target, and the brainstorm already names griefing as
"the PvP of a non-violent MMO". The resolution the evidence supports is Dan Cook's:
non-rival, positive-sum state in open space (my action can add to yours, never undo it —
Journey removed player collision for exactly this reason), and rival or destructible
state only behind trust (a claim, a party, an opt-in). Sky and Death Stranding go
furthest: there is simply no harmful verb.

**Permanence vs. supply.** If a relit beacon stays lit forever, the puzzle is consumed
once per land. Death Stranding's decaying structures and the brainstorm's "beacons need
*keeping*" point the same way: solved state should decay or need upkeep, which
regenerates supply and gives hosts lasting esteem. The cost is that decay reads as
punishment in a cozy register and fights region parking — it has to be closed-form on
restore and lenient, never a streak.

**Requiring other players vs. dead doors.** Every hard N-player requirement in the
record became a dead door when population dropped (Uru's Delin/Tsogal doors, and the
bad solo retrofits afterwards). What worked: Puzzle Pirates fills empty stations with
NPC "swabbies"; LittleBigPlanet puts x2/x3/x4 areas only on optional rewards; Deep Rock
makes roles helpful, never mandatory. Since players here are entities like NPCs, an NPC
companion standing on the second pressure plate is the entity-generic answer and costs
no special case. Thresholds should be proportional to who is present, not authored per
count — Pico Park's creator: "making one stage is like making seven stages".

**Lock the volume vs. let them dig.** A destructible world dissolves authored
constraints (tunnel past the door). There are two coherent stances and hybrids leak:
Adventure-Mode style — deny by default inside a volume, whitelist verbs per item,
enforced in the sim (Wynncraft, Hypixel); or Teardown/CTM style — everything stays
editable and the challenge is systemic (time, scarcity, distance, a budget of blocks).
The first needs the claims/permissions system that is already core; the second needs no
protection at all but only suits construction and traversal puzzles. Per-player fake
blocks (the Minecraft-server trick for personal quest state) fight authoritative
collision and determinism — avoid. Instancing maps cleanly onto the region-actor model
(a parked `SerializedRegion` template spun up per party) but breaks the one-world fiction
and is exactly the path that made Uru a lobby.

**Restricting communication.** For strangers, closed affordances (pings, a synced
countdown, templated marks) are mainly an anti-toxicity tool and they work; for friends,
Discord makes any secrecy unenforceable. So stranger-facing puzzles must be solvable at
low bandwidth, and nothing may depend on players being unable to talk.

## 5. Three shapes the game could take

**A. Puzzle Pirates model — the Wayfarer's jobs are skill minigames.** Surveying,
cooking, boat handling, stamp carving each become a replayable skill puzzle whose score
feeds a shared outcome (the lodge's standing, the ship's passage). Most proven, fully
spoiler-proof, built by 3–6 people originally. Costs: the minigames are abstract
overlays detached from the voxel world and the sim; the register drifts from
contemplative toward score-chasing; content is "design another good minigame", which is
hard.

**B. Systemic world-puzzle — the voxel simulation is the puzzle.** Open-ended specs in
the world ("get water to the beacon cistern", "cross this gorge with forty blocks",
"carry the lamp up before dark") solved with traversal, mechanisms, fluids and light;
any state that satisfies the spec passes; histograms and ghost replays compare
solutions. Best fit for determinism, mods and Koster's "systems over content"; destruction
becomes the tool rather than the threat. Costs: unproven at MMO scale — no shipped game
has done it; the most expensive in the rolled-back sim (every propagation system must
be deterministic, bounded per tick, region-local, and event-driven so a parked region
restores in closed form); propagation order becomes a compatibility contract forever
(Hypixel's adventure maps died when redstone behaviour changed).

**C. Social / seasonal — ATITD's Tests on lands.** The land's communal goal (Survey
complete, chain relit) as the recurring shared event; player-built puzzle trails rated
by other players (ATITD's Pathmaker/Venery, already half-present as the brainstorm's
view puzzles); new lands as the cadence. Cheapest in content. Costs: ATITD's scale is
low thousands; top-N passes, elections and trust games produce politics, drama and
losers, which is the opposite of the chosen register unless every Test is recast as
positive-sum.

**Recommendation:** B as the core verb set, C's structure at land scale, and one
principle borrowed from A — *individual performance feeds a shared outcome* — with
authored insight only as seasoning via forge-time history. Concretely:

- **Session scale (solo or 2–6):** traversal and provisioning as the execution puzzle;
  small mechanism sites on the trail with open-ended specs; NPC companions fill empty
  roles; co-op-gated extras only on optional rewards.
- **Land scale (community):** the beacon chain as shared, persistent, non-rival state
  that needs keeping; one distributed-fragment mystery per land (each Wayfarer's
  surveyor's notes hold a unique shard, Corridors-of-Time style) — cosmetic or
  lore reward, never a gate on anything paid or on progression (Destiny's Niobe Labs
  is the cautionary tale).
- **Hints:** in-world, position-anchored and lossy — lodge guestbooks and templated
  trail marks in the Dark Souls manner — so hint strength is a designer's dial rather
  than a wiki's.
- **UGC:** view puzzles, puzzle trails and mod plots, with creator-must-clear by replay,
  difficulty from clear-rate telemetry rather than author labels, and a skip/endless
  queue for discovery. Human curation is an ongoing operating cost (Dreams stopped
  in 2024).

## 6. What to avoid

- Authored insight puzzles as the retention engine (Uru, TSW).
- A shared world with no interdependence, or with competition over puzzle nodes
  (Islands of Insight).
- Gating progression or purchases behind a community-first solve (Niobe Labs).
- Hard N-player requirements without an NPC or solo fallback (Uru's dead doors).
- Single-winner global goals and grind finales (Curiosity, the Potato Sack's
  GLaDOS@Home).
- Letting the real web be part of the puzzle (TSW's in-game browser: the search
  snippets were the spoilers).
- Hidden-state puzzles inside the client-predicted sim (solver mods).
- An unfamiliar genre with weak first-hour legibility — named in the post-mortems of
  Glitch, Uru and Islands of Insight alike.

## 7. Architecture notes and cheap first probes

- Mechanisms and signals: define seam semantics up front (≥1 tick latency across a
  region boundary, or forbid networks spanning a seam); budget updates per region —
  unbounded player-built clocks are a server-cost attack.
- Contraptions: kinematic and axis-constrained, not free rigid bodies — journal and
  collider churn in rollback is the cost.
- Timing puzzles: windows of seconds with a game-provided synced countdown (Portal 2's
  lesson); the tick period is coarse and still being tuned.
- Verification and histograms belong in the *derived* layer (outside rollback), fed by
  input logs.
- Prerequisites are the same ones the modding report lists: block property table, a
  place/break verb, open enums, seeded RNG in `GameData`.

Probes in rising cost: (1) view puzzles / treasure maps — derived-only, near-zero
content cost, already in the brainstorm; (2) one mechanism family end to end — a
signal, a door, a beacon — with closed-form restore, to learn what the sim budget really
is; (3) a fragment mystery on a test land, to learn whether the forge can place and
verify clue chains.

## 8. Open questions for Dawid

1. Is puzzle the *core* of the Wayfarer's role, or one verb set beside gardening,
   hosting and delivery? (The brainstorm's Unemployment Problem applies: a role needs
   one thing it is good at.)
2. Protected ruins, open systemic challenges, or both in separate places — given that
   hybrids leak?
3. Does solved state decay, and how leniently, given parked regions and the cozy
   register?
4. How much of a land's puzzle content is forge-generated vs. hand-authored vs.
   player-made?
5. Are instanced spaces acceptable anywhere, or is one continuous world a hard rule?
