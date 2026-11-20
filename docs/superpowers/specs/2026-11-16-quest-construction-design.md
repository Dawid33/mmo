# Quest construction — syntax and semantics (research and proposals)

Date: 2026-11-16. Status: **accepted by Dawid on 2026-11-16 ("go with your recommendations"); nothing built.**
Every **(call)** below is now decided as written, and each question of §12 is answered by its recommended option.
**(open)** items stay open. Two refinements were made while writing the examples: objective builtins are methods
(§3.2), and entering a stage re-opens its objectives (§3.2). Written from the handoff
`docs/superpowers/plans/2026-11-16-quest-construction-research-handoff.md`. Worked examples:
`2026-11-16-quest-examples.md`.

**This document does not change the language spec or its MVP plan** (`2026-11-16-quest-script-language-design.md`,
`docs/superpowers/plans/2026-11-16-quest-script-language-mvp.md`). Another session is building from them. Whatever
Dawid accepts here is folded into the spec afterwards, by him or on his word. **Folded 2026-11-17** on his word
into spec §5.11–5.13 and §9.1 (the spec is now the rule; this document keeps the research and the reasons), and
detailed into `docs/superpowers/plans/2026-11-17-quest-phase-10-dialogue-and-quests.md`. Decided at the fold:
conversations private, bark = one-actor scene (the bark form stays), no `ns.*` rename.

Marks: **(D)** = Dawid decided it; **(call)** = Claude's proposal; **(open)** = undecided. Each proposal also
says what it costs the parser the other session is building (`quest/crates/quest-lang/src/grammar.lalrpop`):

- **grammar** = new syntax;
- **checks** = new compile-time rules only;
- **runner** = engine/runner semantics only, no syntax.

## 0. Summary

| # | Question | Proposal | Cost |
|---|---|---|---|
| 1 | Quest structure beyond a flat stage enum | Keep flat stages. Objectives become **named, stateful** (done / failed, counters, `optional`). Stage completion is a declarative `-> Next when …`. Add an `Entered` event per stage and stage-scoped timers. No nested or parallel stages. | grammar (objective names, `when` on a stage transition), runner |
| 1b | Multi-region quests | One **home** instance holds quest state; **parts** (`part … on role` / `at role`) run in other regions and talk to the home only by `emit`, reading its state through the same mirror dialogue uses. | grammar (`part`), runner |
| 2 | Person selection | Roles stay bound at spawn. Add `for npc where <pred>` for generic dialogue, scoped to the instance's regions. Package-level dialogue with no instance is stateless (no `emit`). The engine adds **occupation** and **tags** first. | grammar (`where` after `for`), engine data |
| 3 | Ambient NPC-to-NPC scenes | A new item `scene`: several speakers, no choices, timed lines, a lead actor owns it. A bark is a one-actor scene. Interruptible by default. | grammar (`scene`), runner |
| 4 | Line pacing | **Conversations:** the runner runs to the next choice in one tick and the client paces the lines; the sim waits only at choices and for an end acknowledgement. **Scenes and barks:** timed lines, with durations the compiler derives from the source text. | runner (+ an optional `pause` step) |
| 5 | Requirements | `where` / `requires` use a **query subset** (conjunctions of atoms) the coordinator can index. **One scoring rule** for definitions, greetings, nodes and lines, counting **conjuncts** rather than operators. | checks; spec §5.11 scoring changes |
| 6 | `world.region_query` | **Drop it**; parts cover far regions. If it ever returns, name it `regions.query`. Optionally rename the namespace from `world.*` to `ns.*`. | host description only |
| 7 | Repeatable quests | Header `repeatable quest X(cooldown=…)`. A reset is §6.9's reset, plus reaping and re-placing the quest's prefab. Completion is recorded per (definition, player) in the coordinator. World-line is an **act's** property, not a quest type. | grammar (one word), coordinator |
| 8 | Return values from dialogue | No values. Dialogues get **named exits**: `return accepted`, branched on by `detour x:` arms and reported in `DialogueEnded.exit`. | grammar, checks, runner |

Decisions this needs from Dawid are collected in §12.

## 1. What is fixed

From the handoff, `docs/game-design.md` (revision 2026-11-16) and the spec's decided parts:

- **Quests:**
  - authored in packages; one quest namespace per instance, owned by the system;
  - types repeatable, one-off and world-line;
  - acts are quest-gated in single player and time-gated in multiplayer (D).
- **Recursion:** only script functions calling themselves are forbidden. Recursive data and recursive dialogue are fine (D).
- **Dialogue is a declarative description** that the region's Rust code runs:
  - conditions are pure VM functions;
  - effects are `emit`ted events (D);
  - attached dialogues combine **commutatively** (D);
  - conversations work **across region boundaries** (D).
- **Region data is reachable from scripts**, with undo and rollback invisible (D).
- **Seams:** regions talk only by stamped messages applied at T+1 (CLAUDE.md, Regions & seams).
  - `WorldView` sees the own region plus an 8-block halo, one tick old (`crates/game/src/world_view.rs`,
    `halo.rs:67`).
  - What crosses a seam is classified exhaustively (`crates/game/src/migrate.rs`).
- **Engine facts the proposals rest on:**
  - `Person` holds race, sex, name, birth, death, family, citizenship (`site`) and `location`; it has **no
    occupation and no tags** (`crates/game/src/history/records.rs:48`).
  - A `Site` holds its `civ`, `region`, `members` and `player_founded` (`records.rs:115`).
  - Prefabs already carry `tags: Vec<String>` (`prefab/file.rs:39`), but entities do not.

## 2. Additional survey

The 2026-11-16 survey (`2026-11-16-dialogue-and-quest-structure-research.md`) covered ink, Yarn Spinner 3,
Bethesda quests and the Story Manager, Witcher 3 quest graphs, storylets and statecharts. The open questions need
four more systems.

### 2.1 Bethesda Scenes (Creation Kit) — ambient multi-actor scenes

- **Phases:** a scene is an ordered list of phases.
  - A phase holds **actions**: a dialogue line, an AI package (walk somewhere, sit) or a timer.
  - A phase ends when all its actions finish, or when its completion conditions become true. The scene ends
    after its last phase.
  - An action on a dead or disabled actor counts as finished at once.
- **One scene per actor:** an actor is in at most one scene at a time. A scene whose actors are busy waits for
  them.
- **Interruption:** an *interruptible* scene ends when a non-interruptible one needs one of its actors. Flags
  decide what combat does to a scene (end, pause or continue).
- **Repeat:** a scene may repeat while a condition holds.
- **Common bugs, from modders:**
  - a phase whose package never reaches "done" stalls forever;
  - a phase with no line and no timer gets skipped.

  Lesson: **every phase needs a bounded duration.**

**What to take:** the one-scene-per-actor rule, interruptibility as a property of the scene, and actions on a
missing actor counting as done. Phases are not worth taking: a step list with timed lines is enough while there
are no AI packages to wait on.

### 2.2 Valve's response rules (Left 4 Dead, Dota 2; Elan Ruskin, GDC 2012)

- **Query:** when something happens, the game builds a **query**: a dictionary of facts about the speaker, the
  event and the world.
- **Rules:** each **rule** is a set of **criteria**, each a test on one fact. The rule that matches with the
  **most criteria** wins, so specific lines beat generic ones and a rule with few criteria is the fallback.
- **Responses:**
  - can write facts back ("remember I said this"), which is how `once` and conversation memory are built;
  - can fire a **follow-up** concept at another character after a delay, which is how two survivors banter;
  - carry no-repeat and odds options.
- **Speed:** matching stays fast because criteria are sorted and pre-bucketed by the event's concept.
- **Authoring:** writers add more specific rules beside the general ones and never edit a branching tree, so
  content grows by addition.

**What to take:**

- The scoring rule: **count criteria (conjuncts), not operators** (§7.2). It is the same idea as Yarn's
  saliency, already chosen, with a better count.
- Follow-ups are how NPC-to-NPC banter chains (§5).
- Rules grow by addition, which is what commutative combination needs.

(Sources: the talk's slides and Valve's response-system wiki, listed in §13. Both refused a fetch today, so the
details above come from secondary summaries and Claude's prior knowledge. Check before quoting them as Valve's
exact semantics.)

### 2.3 Guild Wars 2 dynamic events — shared, public, cyclic quests

- **Shared, not offered:** events run whether or not players come. Anyone nearby takes part, without accepting
  anything, and participation is measured by contribution.
- **Chains:** the outcome picks the next event, so failure is a branch, not an end. Chains **cycle**: they
  reset and run again, by design and for content budget.
- **Scaling:** difficulty scales with the number of participants.

**What to take:**

- A **one-off quest is a world puzzle shared by whoever joins** (§3.3, participants).
- A **failed attempt is a branch back to an earlier stage**, not the quest's end.
- A repeatable quest is a cycling chain (§9).

### 2.4 Line pacing in practice (Yarn Spinner's presenters)

- **Yarn's runtime does not pace.** It hands a line to a *presenter*, and the presenter says when the line is
  done: when the player presses continue, or, with *auto-advance*, after its animations plus a set delay.
- **Typewriter:** text appears by character, and a "hurry up" press finishes it at once.

**What to take:** pacing is **presentation**, chosen per presenter. Here that splits cleanly:

- the sim decides what is said and what waits for input;
- the client decides how fast text appears (§6).

## 3. Quest structure (question 1)

### 3.1 Judgement of the flat stage enum

Keep it.

- **Edits:** a single current stage migrates across edits by name (spec §6.9).
- **Readers:** a snapshot reader (the coordinator, the editor) and the journal can read it.
- **Precedent:** Bethesda ships its whole quest catalogue on flat stages plus objectives.

What a flat enum alone cannot say — parallel objectives, optional ones, partial progress — belongs on the
**objectives**, not in a deeper stage tree. Nested and parallel stages (statecharts) are **not proposed**:

- every case found while converting the examples is met by stateful objectives;
- each nesting level adds a migration rule for edits (which sub-stage survives a removed parent).

### 3.2 Stateful objectives (call; grammar)

Today `objective "text" @id at expr` is a journal line with no state. Proposal: give it a name and a state.

```python
stage Search:
    objective find_dog f"Find {dog.name}" @lostdog.find at dog
    objective find_collar "Find the collar" @lostdog.collar at collar_spot
    objective wolves "Drive off the wolves" @lostdog.wolves count 5
    optional objective ask_miller "Ask at the mill" @lostdog.mill at miller
    -> Returning when find_dog.done and find_collar.done
```

- `objective <name> "text" [@id] [at expr] [count n]` declares an objective scoped to its stage. Its state is
  `Open`, `Done` or `Failed`, plus a progress count when it has `count`.
- The state is **quest state**: generated persistent globals, migrated by qualified name (`Search.find_dog`) on
  an edit, like the stage.
- **Names:** an objective's name is a quest-level name. It may not shadow a role, a quest `let` or another
  objective (`duplicate-def`). Another stage's objective is named by `Stage.name`.
- **Methods (accepted 2026-11-16, replacing the free functions):**
  - Commands, callable from handlers only (they are writes): `obj.finish()`, `obj.fail()`, `obj.progress(n)` (marks
    the objective done when it reaches its count) and `obj.reset()`.
  - Reads, pure, so conditions and dialogue may use them: `obj.done`, `obj.failed`, `obj.count`.
  - Free functions `done(obj)` and so on were dropped: one name for both a command and a query read badly.
- **Entering a stage re-opens its objectives**, re-entering the same stage (`-> Search` from `Search`) included.
  Objectives belong to a visit of their stage, so a failed attempt that re-enters starts clean (§3.4). An earlier
  stage's objectives keep their last state until that stage is entered again.
- **An objective whose `at` target is an `Option` that is `None`** gets no marker. The same rule makes `for x`
  with `x == None` attach nowhere.
- `optional` objectives are shown in the journal but never gate anything by themselves.
- **Parallel objectives** are simply several objectives in one stage. Nothing orders them.

**Declarative stage transitions.** `-> Next when cond` as a step of the stage (not inside a handler) is a
**completion transition**: the VM evaluates `cond`, a pure function, after every committed handler segment while
the stage is current, and moves to the first transition that holds (in source order). That covers "both found →
return the dog" without a handler checking the pair. `-> Stage` inside a handler stays as specified.

**The journal is derived, never stored.** It is the current stage's objectives with their states, plus the
`Done` objectives of earlier stages if Dawid wants history shown (the survey's open question 5). It is a view
over quest state, so it is correct after a rollback, an edit or a seam.

### 3.3 Participants (call; runner)

The spec has `complete(participants)` but no notion of who takes part.

- **`participants`:** a built-in, persistent `list[Player]` of the quest. A player joins with the command
  `join(p)`, typically from an `Accepted` handler, and leaves with `leave(p)`.
- **The journal** is shown to participants only.
- **`complete()` with no argument** completes for all participants. With a list, it completes for those only;
  the others are recorded as present.
- **A one-off quest is shared.** Several players may join one instance and work on the same objectives (GW2's
  public model). Whoever completes it completes it for every participant. A repeatable quest may be **exclusive**
  (§9): nobody else may join while it has participants.

### 3.4 Failure and retry (call; runner)

The game's flow is "try, fail, prepare, succeed" (game-design §3.1), so most failure is **an attempt failing,
not the quest**:

- An attempt fails → the quest returns to an earlier stage, the stage's `Entered` handler resets what the
  attempt changed, and its objectives re-open (re-entering a stage re-opens them, §3.2; `obj.reset()` re-opens one).
- `fail()` is kept for the **instance** becoming impossible (the farmer died, the bound site was destroyed). It
  ends the quest for everyone and starts the reaper (quests-in-the-world §7).
- `leave(p)` is one player's failure or abandonment, and the quest stays open.

Two runtime additions make that writable without new statement forms:

- **`Entered`**, a built-in event delivered once when a stage becomes current (after `-> Stage` or a declarative
  transition). It may be handled only inside a stage (`@on(Entered)` in the quest body would be ambiguous: a
  `checks` error).
- **Timers:** `after(ticks, Event(...)) -> Timer` and `cancel(t)`.
  - The VM delivers the event to the instance when its tick comes.
  - A timer **set in a stage handler is cancelled when the stage changes** (statechart semantics: a stage's
    timers belong to it), so a stale "before dark" timer cannot fire into a later stage.
  - A timer set in a quest-body handler lives until it fires or is cancelled.
  - Timers are VM state: hashed, undone and saved.

  (Handlers have no `sleep` on purpose, spec §7; a timer is the event-shaped way to wait.)

### 3.5 Roles that stop resolving (call; runner)

A role's entity can die, leave its region set or be removed. The engine raises **`RoleLost { role }`** to the
instance at the tick it notices. It never fails silently, which is the lesson of Bethesda's empty-alias failures.
A quest that does not handle `RoleLost` gets the default: `fail()` if the role is not `optional`, and nothing if
it is.

- **Grammar:** `optional role x: T where …` may also be unbound at spawn.
- **Reading one:** an optional role's value is `Option[T]`.

### 3.6 Multi-region quests: a home and its parts (call; grammar, runner)

**The problem.** quests-in-the-world §5.4 places one script instance per target region, all running the quest's
one program, coordinating "through the namespace". With the stage, objectives and participants as top-level quest
state, **each region's instance would hold its own copy of the stage**. Two regions could disagree on where the
quest is. Namespace promises cannot fix that: they are slow, and conditions cannot `await` them.

**The proposal.** Quest state has exactly one owner:

- **The home** is the instance holding the quest body: the stage, objectives, participants and quest `let`s. It
  is region-anchored, by default to the region where the quest's first role was bound at spawn. It can be
  overridden with `quest X at role:`.
- **A part** is a named block of the quest that runs as its own script instance somewhere else:

```python
quest LostDog:
    role farmer: Person
    role dog: Creature where dog.has_tag("pet")

    event DogFound:
        player: Player

    part dog_watch on dog:              # entity-anchored: crosses seams with the dog
        @on(PlayerNear)
        fn near(e):
            if stage == Stage::Search:  # the home's stage, read through the mirror
                emit DogFound(player=e.player)
```

- **Anchoring:** `part name on role:` anchors the part to the role's entity (spec §9.1's entity anchor), and it
  crosses seams with it. `part name at role:` anchors it to the region where the role was at spawn.
- **Reading:** a part may **read** quest state, through a **mirror** of its read set. That is the mechanism spec
  §5.11 already defines for dialogue conditions: stamped, one tick old, rollback state in the receiver.
- **Writing:** a part never writes quest state. It `emit`s events to the home (stamped, T+1), and the home's
  handlers do the work. It has its own `let`s, if it needs any, which are part state.
- **Spawning:** every part is placed by the spawn job like the home (§5.4 there, all must accept).

  Reaping removes them all: everything carries the instance id.
- **What it reuses:**
  - dialogue's mirror and `emit` routing;
  - the engine's entity anchors;
  - the spawn job's multi-region accept.

  Nothing new crosses a seam except one more message kind.
- **A single-region quest is a quest with no parts.** That is the common case, and it looks exactly like the
  spec's §5.12 example.

**Risk to check at integration:** an `emit` to a home whose region is **parked**. Regions are never woken for a
quest (quests-in-the-world §3). The message must be held, as intents for a parked region are ("items are never
lost", the salvage path), and applied when the region runs again. A home that parks for long enough is reaped by
the lifetime rule (§7 there), which counts only while no player is near.

## 4. Person selection (question 2)

### 4.1 Roles bound at spawn — keep

`role x: T where pred` is bound once, by the coordinator's requirement check (§7). It is the quest's alias in
Bethesda's sense. It never re-binds during the instance. If a role stops resolving, that is `RoleLost` (§3.5).

### 4.2 Generic dialogue: `for npc where pred` (call; grammar)

`for expr` attaches a dialogue to one NPC. Generic dialogue — "every smith in this town remarks on the wolves
while the quest runs" — needs a **filter**:

```python
stage Search:
    dialogue wolf_talk(p: Player, npc: Person) for npc where npc.occupation == Occupation::Smith topic "The wolves?":
        npc: "Heard them last night. Bar your door."
```

- **Rules:**
  - `for npc where pred` names the dialogue's own `npc` parameter.
  - `pred` is a pure condition over `npc`, `p`, roles and quest state.
  - When a player talks to an NPC, every in-scope filter dialogue whose `pred` holds joins that NPC's attached
    set. Combination stays **commutative**: a filter adds to the set, and the set is still unordered.
- **Scope, so the runner's cost stays bounded:** a filter dialogue applies to NPCs **in the regions of its
  instance** — the home's region and its parts' regions. The runner keeps, per region, the list of filter
  dialogues in scope. It evaluates them only for the one NPC being talked to, so a filter costs one pure call per
  talk, never a scan of people.
- **Package-level dialogue with no instance** (ambient flavour shipped by a package, not tied to a quest):
  - It is **stateless**: its conditions may read the NPC, the player and world facts (§7.1). It may not read
    script globals or `emit` anything (`checks`).
  - Its `once` records live with the NPC, keyed by the dialogue's qualified name, as for attached dialogues.
  - It is in scope everywhere the package is enabled.

### 4.3 What person data predicates need (call; engine)

In order of how often quests will ask for it:

| Fact | Today | Proposal |
|---|---|---|
| race, sex, age (from `born`), alive | yes (`Person`) | expose as queries |
| family: mother, father, spouse, children | yes | expose; `related(a, b)` |
| citizenship (`site`) and its civilisation (`Site.civ`) | yes | expose; `npc.site`, `npc.civ` |
| **occupation** | **no** | add `Person.occupation: Occupation` (an enum the history sim assigns; settlements can reassign). The first thing every quest asks ("the blacksmith", "a guard"). |
| **tags** | **no** (prefabs only) | add a string set on entities and people, set by prefabs and by scripts (`tag(npc, "witness")`). Tags are how packages mark *their* people without the engine knowing their meaning. |
| relationship with a player | no | **defer**. A quest keeps its own per-player records. A shared disposition model is a design of its own (who writes it, where it lives: the coordinator, since it outlives regions). |

**Occupation before tags:** occupation is world truth the forge and the history sim own. Tags are package
annotations. Requirements index both (§7.1).

## 5. Ambient NPC-to-NPC scenes (question 3)

### 5.1 The form (call; grammar)

```python
scene gossip(a: Person, b: Person) where a.site == b.site and a != b:
    a: "Did you hear about the farmer's dog?"
    b: "Wolves, they say."
    => a: "Wolves don't take dogs whole."                   when quest_open("lost_dog")
    => a: "Wolves. Always wolves."
    pause 40
    b: "Keep your voice down."
```

- **What it is:** a `scene` is a dialogue with **several speakers and no listener**. Its parameters are its cast,
  all `Person`. Steps are those of a dialogue, minus `choice` (`scene-choice`).
- **One new step, `pause n`:** silence for `n` ticks. It is also allowed in a bark.
- **`where pred`** is the cast's requirement, used when the runner casts the scene (below).
- **A bark is a one-actor scene.** Proposal: `dialogue name(npc: Person)` stays accepted as a bark, and the docs
  call it a scene with one actor. Whether to drop the bark form for `scene` is (open). The parser already
  accepts barks, so keeping them costs nothing.

### 5.2 Who starts a scene (call; runner)

- **Explicitly:** the command `play(scene, a=…, b=…)` from a handler (a quest's staged moment).
- **Ambiently:** the runner's ambient schedule, the one barks already have.
  - Now and then, for regions with a player within `HEAR_RANGE_BLOCKS` of idle NPCs, it **casts** in-scope
    scenes: it binds the cast from nearby idle people whose `where` holds.
  - It picks one by the shared scoring rule (§7.2), least recently played, then a stable key.
  - A random tie-break uses `roll(tick, lead, salt)`.
  - In-scope means the same rule as filter dialogue (§4.2): instance scenes in their regions, package scenes
    everywhere.
- **Follow-ups** (Valve's banter): a scene's last step may be `-> other_scene`. That plays another scene with the
  same cast, mapped by parameter position, after a pause. It needs no new form: `->` between scenes of one
  program.

### 5.3 Ownership, seams and interruption (call; runner)

- **The lead owns it.** A scene's state (scene, step, its timer) is carried state of its **lead**, the first cast
  member, exactly as a conversation is carried state of its NPC. It crosses a seam with the lead.
- **Speakers across a seam:** the other cast members may be in another region. A cast member must stay within
  `HALO_BLOCKS` of the lead (the same compile-time bound as `TALK_RANGE_BLOCKS`), so the lead's region sees all
  of them through `WorldView`.
- **One activity per NPC.** An NPC is in at most one conversation or scene (Bethesda's rule). A scene is cast only
  from idle NPCs.
- **Interruptible by default.** A player talking to a cast member ends the scene for all actors
  (`SceneEnded { reason: Interrupted }`), and the conversation starts.
  - `scene name(...) uninterruptible:` makes the cast busy instead: a talk attempt gets the NPC's `busy` bark if
    one is attached, else nothing.
  - A quest's staged scene (an argument at the gate, a ceremony) is the only expected user.
- **Missing actors:** a cast member who dies, leaves range or is removed ends the scene (`ActorLost`). This is
  unlike Bethesda, where the actor's lines are just skipped: a skipped half of an argument reads as a bug.
- **Hearing:** lines go to clients within `HEAR_RANGE_BLOCKS` of the speaker, as bark lines do: subtitles or
  speech bubbles, with nothing to answer.

## 6. Line pacing (question 4)

The sim never sees line text: it hashes line ids and substitution values, and clients render text from their
locale's string table (spec §5.11). So **a duration cannot come from the text a player reads**. Two different
situations follow.

### 6.1 Conversations: run to the next choice (call; runner)

- **Batching:** when a conversation starts or a pick lands, the runner walks steps **in that one tick** until it
  reaches a `choice`, the end, or a `pause`. It sends the lines as one batch.
- **The client paces them:** a typewriter, a "continue" key, or auto-advance, whatever the UI wants — Yarn's
  presenter model. A "continue" is **not sim input**.
- **The sim waits at only two points:**
  - **a choice**: the pick is stamped input, as specified;
  - **the end**: the conversation stays open until the talker's client sends **`Done`** (stamped input), or
    `CONVERSATION_LINGER_TICKS` passes. Until then the NPC keeps facing the player and does not wander off
    mid-read.
- **Choice timeouts count from when the choice is offered,** in ticks. The client knows the batch's line count,
  so a UI may add reading time before showing the countdown. That is presentation; the sim's timeout is what it
  is, and authors size it.
- **Why:**
  - **Fewer stamped inputs:** one per choice, not one per line.
  - **No seam crossing mid-line.**
  - **Prediction:** the picker's client already predicts the next batch.
- **The price:** `emit`s between two lines take effect at the batch's tick, possibly before the player has read
  the line that announces them (an item is in the bag while "Here, take this" is still typing). Where that
  matters, the author puts the effect after a `choice` ("Take it" / "Not now"), which is usually better design
  anyway.
- **`pause n` in a conversation** ends the batch, and the runner resumes after `n` ticks: for an NPC who "thinks"
  or acts between lines.

**Alternative, rejected unless Dawid prefers it:** a stamped "continue" per line. It gives line-exact sync between
lines and world (an NPC walks while it talks), at one input per line and a seam message per line when the player
stands across a seam. That sync belongs to scenes, which have their own timing (§6.2).

### 6.2 Scenes and barks: timed lines (call; runner, compiler)

Nobody presses continue, and several players hear them, so lines take time in the sim:

- **Line duration:**
  - The compiler gives each line a **duration in ticks**, from its **source-locale** text: `LINE_BASE_TICKS +
    chars × LINE_TICKS_PER_CHAR`, clamped to `[LINE_MIN_TICKS, LINE_MAX_TICKS]`.
  - It is stored in the dialogue graph, so it is part of the program bytes: identical everywhere and versioned
    with the bytecode.
  - Translations do not change timing; a client shows its locale's text for the stored duration, as subtitles
    do with recorded voice.
- **An override per line:** `a: "…" @id (ticks=60)`. That is grammar: the `(ticks=…)` suffix, the same shape as
  `choice(timeout=…)`.
- **Waiting between lines** is `pause n`.
- **Voice, later:** a voiced line's duration would come from the audio length, recorded into the package build
  next to the string table. Nothing now.

## 7. Requirements and scoring (question 5)

### 7.1 The query subset for `where` and `requires` (call; checks)

The coordinator checks requirements periodically over world facts and region snapshots (quests-in-the-world
§5.2). It must evaluate them without the VM, and index them. So a role's `where`, and a new quest-level
`requires`, accept a **subset**:

```python
quest LostDog:
    requires act() >= 1 and not completed("errantry/lost-dog::LostDog", within_regions=4)
    role farmer: Person where farmer.occupation == Occupation::Farmer and farmer.alive
    role dog: Creature where dog.has_tag("pet") and dog.owner == farmer
    role inn: Person where inn.occupation == Occupation::Innkeeper and inn.site == farmer.site
```

- **Shape:** a conjunction (`and`) of **atoms**. An atom is one of:
  - a comparison of a fact to a literal or enum value: `x.occupation == …`, `x.age >= 16`, `act() >= 2`;
  - a tag or kind test: `x.has_tag("…")`, `x.kind == …`;
  - a relation between two roles: `a.site == b.site`, `a.owner == b`, `related(a, b)`, `near(a, b, regions=n)`;
  - a world fact: `act()`, `loop()`, `completed(def, …)`, `site_kind(x.site) == …`.
- **`or` and `not`:** `or` is allowed **inside** an atom's operand list (`x.occupation in [Smith, Miner]`) and
  between atoms of one role (`a.has_tag("pet") or a.has_tag("stray")`, one atom group). `not` is allowed on an
  atom.
- **Forbidden** (`bad-where`): calls to script `fn`s, script globals, loops, arithmetic beyond a comparison with a
  constant, and host queries not in the coordinator's fact table.
- **How it is indexed:** atoms on one role become **filters** the coordinator indexes, by occupation, kind, tag
  and site kind. Relations become **join constraints**, applied after filtering, with roles ordered by how
  selective their filters are. `requires` atoms gate the whole definition before any role is filled.
- **The fact table** is the coordinator's half of the host description (spec §9). It is versioned with it, and
  the compiler checks `where` against it.

The script-function escape hatch for requirements no query can express stays (open), as quests-in-the-world
§5.2 says.

### 7.2 One scoring rule, counting conjuncts (call; changes spec §5.11)

The same choice — the most specific applicable item wins — happens at three scales:

1. **The coordinator:** several definitions can bind the same NPC or place. A generic "the smith needs ore" and a
   specific "the dwarven smith needs ore in act 2" compete.
2. **The runner:** the greeting pool and node groups.
3. **The runner:** line groups.

**Proposal: one rule for all three, Valve's.**

- **Score** = the number of **top-level conjuncts** in the condition. `once` and `once per p` each count 1. An
  `or` group counts **1**, however many alternatives it has. `not x` counts 1. No condition scores 0.
- **Ties:** fewest prior selections (least recent), then the stable key (qualified name, then source position),
  then a `roll` for the `random_*` strategy only.

**Why change spec §5.11's count** (each condition and boolean operator counts 1, as in Yarn 3): under that count
`a or b` scores **higher** than `a`, although it is less specific. `a and b` and `a or b` tie, although one is
strictly narrower. Counting conjuncts fixes both. It also gives the coordinator a score it can compute from the
atom list of §7.1 with no evaluation. It differs from Yarn, so a later Yarn importer must re-score (a few lines).
This is the one proposal here that edits an already-specified rule; **Dawid's call**.

For definitions, the coordinator's "least recent" is "least recently spawned near here". That spreads
repeatables across a live area for free.

## 8. `world.region_query` (question 6)

`world.*` is the namespace API (`get`, `set`, `add`, `compare_and_set`, `delete`). §9.1's `world.region_query`
reads a far region, which is a different thing under the same prefix.

**Recommendation: drop it (call).**

- **Its use cases are covered:** watching something in another region is a **part** (§3.6), event-driven,
  reading and writing locally. World facts come from the coordinator's fact table (§7.1).
- **It is hard to answer honestly.** A far region may be parked (and must not be woken), or owned by another
  host. Its answer is stale by the time it lands. It would be a promise that usually returns `Parked`.

If it ever comes back, name it `regions.query(...)`. Its semantics would be: answered by the owning host at a
stamped tick, refused with `Parked` for a parked region, never waking one.

**Optional rename (call):** now that a namespace belongs to one quest instance (D), `world.get(key)` reads as if it
were world-global. `ns.get(key)` (or `shared.get`) says what it is. This is a host-description change only: the
namespace operations are not grammar.

## 9. Repeatable quests (question 7)

### 9.1 Declaring the type (call; grammar)

```python
repeatable quest Bandits(cooldown=12000, exclusive=true):
    ...
```

- **`repeatable`** is a contextual word before `quest`. A quest without it is one-off.
- **World-line is not a quest type.** game-design §4.1 already says a world-line quest is a one-off in its
  technical form. What makes it world-line is an **act definition naming it** (quests-in-the-world §9). So the
  quest source has two forms, and acts list quests by qualified id. A package that adds a quest to the world line
  edits the act, not the quest.
- **Options:**
  - `cooldown` (ticks between a completion and the next setup);
  - `exclusive` (no one may `join` while it has participants: one party at a time);
  - `once_per_player` (a player who completed it may not join again; recorded as in §9.3).

### 9.2 What a reset clears (call; runner)

On completion of a repeatable quest:

1. **The script instance resets**, exactly as spec §6.9's reset does:
   - all quest and part `let`s re-initialise;
   - the stage returns to the first, objectives re-open, `participants` empties;
   - the queue empties, parked handlers and promises are cancelled, timers too.

   It is the same code path as the admin's Reset button.
2. **Its world content resets.** Everything the instance placed carries the instance id (quests-in-the-world §7):
   - The reaper removes the tagged entities, items in the world and fixtures.
   - The quest's world content is placed again by **re-delivering `Setup`** after the cooldown: the same handler
     that placed it the first time (`place(prefab, at)`), which may `decline()` (the instance is then reaped).
     Refined while writing the examples: there is no separate "setup prefab" record.
3. **The roles are re-checked, not re-bound.** Each role's `where` is evaluated again. If any fails (the farmer
   died), the instance is reaped instead and the coordinator may spawn a fresh one elsewhere.
4. **The namespace** is kept, as the record across repeats. A definition may clear it with `on_reset_clear=true`
   (open whether that option is needed at all).

**Terrain** the players changed inside the quest's area is **not** restored by default: diffing and reverting
blocks fights players' own building and land claims. A repeatable quest that needs pristine ground declares an
`area` that is **protected** from non-quest edits while the instance lives. The quest's prefab then owns it, and
re-placing the prefab is the restore. This overlaps with holdings' permissions (quests-in-the-world §11). **(open)**

### 9.3 Is a repeatable completion recorded? (call; coordinator)

game-design §4.1 leaves "whether, and how lightly" open. Proposal:

- **The coordinator records it per (definition, player):** the count of completions and the last tick. It is
  idempotent on the instance id plus the reset count. That is what `once_per_player` and requirement atoms like
  `completed(def, by=p)` read.
- **History and legends** get an entry only for a player's **first** completion of a definition, and when a count
  crosses a named milestone (`REPEAT_LEGEND_MILESTONES`, e.g. 10, 100). "Lightly" = the world remembers that you
  were the one who cleared the bandit camp a hundred times, not each time.

## 10. Return values from dialogue (question 8)

### 10.1 Is anything missing?

The quest learns what happened in a conversation in two ways:

- **through `emit`ted events**: the right channel for effects ("accepted", "paid");
- **through `DialogueEnded { reason }`**: how it ended (`Left`, `Timeout`, `TooDeep`, `Edited`).

**There is one real gap, inside a conversation: a reusable sub-dialogue cannot tell its caller how it went.** A
package's `haggle` dialogue, detoured into from many merchants, ends in agreement or refusal, and the caller wants
to branch on that. Today the only way is `emit` plus a global plus a condition, and read-your-emit (spec §5.11)
makes that global visible only after the handler runs. That is too late for the very next step.

### 10.2 Named exits (call; grammar, checks, runner)

```python
dialogue haggle(p: Player, npc: Person) exits agreed, refused:
    npc: "Forty silver. Not a copper less."
    choice:
        "Thirty." if p.skill("trade") >= 3:
            npc: "...Fine."
            emit Discount(player=p)
            return agreed
        "Forty it is.":
            return agreed
        "Too rich for me.":
            return refused

dialogue smith(p: Player, npc: Person) for smith topic "Buy a sword":
    detour haggle:
        agreed:
            npc: "Pleasure doing business."
            emit SwordSold(player=p)
        refused:
            npc: "Your loss."
```

- **Declaring and returning:** `exits a, b, …` after the parameter list declares a dialogue's exits. `return
  <exit>` leaves through one. A plain `return` or falling off the end is the implicit exit `done`.
- **Branching:** `detour x:` with arms named by exits runs the matching arm, then continues after the detour. An
  exit with no arm continues straight on. An arm naming an exit `x` does not declare is a `checks` error
  (`unknown-exit`).
- **Reporting:** the outermost dialogue's exit is reported in **`DialogueEnded { exit }`**, so a quest handler can
  react to how a conversation ended without the dialogue `emit`ting anything.
- **What exits are:** labels on control flow, the way Witcher 3's quest phases have named outputs. They are not
  values. A conversation still holds no variables, conditions still read only script and world state, and a
  dialogue stays a description. The return stack entry gains the arm table, plain data like the rest.

**Rejected:** general return values (`let r = detour haggle` with typed results). They would need
conversation-local variables that conditions read. That makes dialogue stateful in a way the runner must hash,
mirror across seams and migrate on edits, and it reopens "dialogue is not code".

## 11. Judgement of the proposed syntax, item by item

| Proposed (spec §5.11–5.13) | Verdict |
|---|---|
| `quest Name:` with `role x: T where …` | **keep**; add `requires` (§7.1), `optional role` (§3.5), `repeatable` (§9.1), `at role` (§3.6), `part` (§3.6) |
| stages: a persistent enum migrated by name; stage-scoped handlers and dialogues; `-> Stage` | **keep**; add `-> Stage when cond` as a stage step (§3.2), the `Entered` event and stage-scoped timers (§3.4) |
| `objective "text" @id at <target>` | **change**: named, stateful, `count n`, `optional` (§3.2) |
| builtins `complete`, `fail`, `decline` | **keep**; `complete()` defaults to `participants`; add `join`, `leave`, the objective methods (§3.2) |
| dialogue as steps, not statements | **keep** — it is what makes the runner and mirrors possible |
| nodes and node groups with Yarn saliency | **keep the model; change the score** to conjunct counting (§7.2) |
| `choice:` with timeouts, `default` options, hidden `default:` | **keep**; timeouts count from the offer (§6.1) |
| line groups `=>`, `when` / `once [per p]`, `detour`, `emit` | **keep**; `detour` gains exit arms (§10.2) |
| line ids `@path`, string table per locale | **keep**; the graph also stores each line's duration (§6.2) |
| `for <npc>`, `topic` / `priority` | **keep**; add `for npc where pred` (§4.2) |
| barks `(npc)` | **keep as the one-actor scene** (§5.1); dropping the form for `scene` is open |
| conversation as carried NPC state, picks and emits stamped, read-set mirrors, anchored instances | **keep**; the same mirror carries a quest's state to its parts (§3.6). **Add a read-set size check:** a condition reading a big global (a list of every player) mirrors all of it on each change. Warn at compile time above `MIRROR_WORDS_WARN`. |
| `world.region_query` | **drop** (§8) |

New contextual words this would add (spec §5.13's mechanism, recognised by the layout pass, not reserved):

- at the start of a line: `part`, `requires`, `scene`, `pause`, `optional`, `repeatable`;
- in a header: `exits` after a dialogue's parameter list;
- after an objective: `count`.

`scene` and `repeatable` might be better reserved; that is the grammar builder's call.

New diagnostics, found while writing the examples (`2026-11-16-quest-examples.md` §6): `stateless-dialogue` (a
package-level dialogue or scene reads a script global or `emit`s), `unknown-exit`, `scene-choice`,
`entered-outside-stage`. `bad-where` and `duplicate-def` gain cases (a script `fn` in `where`; an objective
shadowing a role).

## 12. Questions for Dawid — answered 2026-11-16: the recommended option of each

1. **Objectives:** named and stateful, with declarative `-> Stage when …` transitions, instead of flags in quest
   state? (§3.2)
2. **Multi-region quests:** one home instance plus parts that read it through a mirror and write by `emit`? (§3.6)
   It changes quests-in-the-world §5.4's "instances coordinate through the namespace".
3. **Participants:** one-off quests shared by whoever joins (GW2-style), with `exclusive` only as an option for
   repeatables? (§3.3, §9.1)
4. **Pacing:** conversations run to the next choice and the client paces the lines (one `Done` input at the end),
   while scenes and barks use compiler-derived durations? Or a stamped "continue" per line? (§6)
5. **Scoring:** count conjuncts (Valve) instead of operators (Yarn), at all three scales? This edits spec §5.11.
   (§7.2)
6. **World-line** as an act's property rather than a third quest form? (§9.1)
7. **Repeatable completions:** recorded per (definition, player), with legends only for the first completion and
   milestones? (§9.3)
8. **Dialogue exits** instead of return values? (§10.2)
9. **Engine data:** add `Person.occupation` and entity tags before quest integration? (§4.3)
10. **Conversations shared or private** (the survey's open question 3): this document assumes **private** (one
    talker; others nearby hear nothing). A shared conversation would make every listener's client a predictor of
    one player's picks.

## 13. Sources

New in this document. Fetching the Valve and UESP pages failed today (503 / 403), so the points in §2.1–2.2 are
from search summaries and Claude's prior knowledge; verify before quoting.

- Bethesda Scenes: [CK Scene](https://ck.uesp.net/wiki/Scene), [Scenes tab](https://ck.uesp.net/wiki/Scenes_Tab),
  [Bethesda tutorial: scenes](https://ck.uesp.net/wiki/Bethesda_Tutorial_Scenes),
  [SCEN record format](https://en.uesp.net/wiki/Skyrim_Mod:Mod_File_Format/SCEN).
- Valve: Elan Ruskin, *AI-driven Dynamic Dialog through Fuzzy Pattern Matching*, GDC 2012 —
  [slides](https://cdn.fastly.steamstatic.com/apps/valve/2012/GDC2012_Ruskin_Elan_DynamicDialog.pdf),
  [GDC Vault](https://gdcvault.com/play/1015528/AI-driven-Dynamic-Dialog-through);
  [Left 4 Dead 2 response system](https://developer.valvesoftware.com/wiki/Left_4_Dead_2/Scripting/Response_System);
  commentary: [Emily Short](https://emshort.blog/2012/03/16/gdc-2012-talk-on-dynamic-dialogue/),
  [Robert Yang](https://www.blog.radiator.debacle.us/2012/07/rule-databases-for-contextual-narrative.html).
- Guild Wars 2: [Dynamic event (wiki)](https://wiki.guildwars2.com/wiki/Event),
  [Colin Johanson answers dynamic event questions](https://wiki.guildwars2.com/wiki/Colin_Johanson_Answers_Your_Dynamic_Event_Questions),
  [GDC: Designing Guild Wars 2 dynamic events](https://gdcvault.com/play/1013691/Designing-Guild-Wars-2-Dynamic),
  [Kill Ten Rats: space for failure](https://www.killtenrats.com/2011/10/18/gw2-space-for-failure).
- Yarn Spinner pacing: [Line Presenter](https://docs.yarnspinner.dev/components/dialogue-view/line-presenter),
  [Dialogue Advance Input](https://docs.yarnspinner.dev/components/dialogue-view/dialogue-advance-input).
- Earlier survey (ink, Yarn 3, Bethesda quests, Witcher 3, storylets, statecharts):
  `2026-11-16-dialogue-and-quest-structure-research.md` §8.
