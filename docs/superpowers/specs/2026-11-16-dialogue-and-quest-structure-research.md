# Dialogue and quest structure in the quest script language — research

Date: 2026-11-16. Status: **research and a recommendation**, not approved, nothing built. Asked for by
Dawid: which dialogue formats and quest-tree systems exist, and how to build them into the quest script
language (`2026-11-16-quest-script-language-design.md`) instead of bolting on a second runtime. The
content-packages spec (§1) left "dialogue trees in a standard format" open; this answers it.

Marks: **(call)** = Claude's recommendation; nothing here is decided.

## 1. The constraints any answer must meet

These come from the language spec and the engine, and they rule out most of what follows:

1. **One deterministic VM.** Dialogue state — where a conversation is, what was seen, what was chosen —
   is quest state. It must be hashed, undone on rollback, saved in cuts and identical on server and
   predicting clients. A second runtime (an ink or Yarn VM beside ours) would hold state the region
   cannot hash, undo or save.
2. **No recursive function calls, static frames** (spec §6.1). Recursive *dialogue* and recursive *data*
   are allowed (Dawid, 2026-11-16), but they must run as loops, not as functions calling themselves.
   Dialogue graphs jump between nodes freely, loop back to hubs and may detour into each other
   recursively. They therefore compile to a loop with an explicit return stack, not to calls.
3. **Waiting is `await`.** A conversation waits for the player's choice. In the VM an `await` parks the
   handler's frames on the heap and lets the instance's queue run (§8.2), so many players can be in
   conversations with one script instance at once.
4. **Edits cancel parked handlers** (§6.9), and so does a package upgrade. Anything long-lived that must
   survive an admin's fix cannot live only in a parked handler's frames.
5. **Many players, one world.** Most dialogue systems assume one reader. Here "seen this line",
   "once", and quest progress are often per player.
6. **Content ships in packages and is translated.** Text must be addressable by id, outside the code,
   so clients can render it in their language and a fix to a typo is not a code change.

## 2. Dialogue formats surveyed

### 2.1 ink (inkle) — the strongest branching-prose language

- **Structure:** knots and stitches (named sections), diverts `->` (gotos), **weave** (nested `*` once-only
  and `+` sticky choices with `-` gathers, so a whole branching exchange needs no names), **tunnels**
  (`-> knot ->` … `->->`, a call that returns), **threads** (`<- knot`, gather choices from several
  places into one menu), visit counts readable by name, `LIST` (a set-like state type), inline
  alternatives (`{a|b}` sequence, `{&a|b}` cycle, `{~a|b}` shuffle, `{!a|b}` once), `EXTERNAL` functions
  bound by the game, and a seeded random.
- **Runtime:** compiled to `.ink.json`, played by a runtime with a call stack and threads; its state
  saves to JSON. Rust: **bladeink** (a full port, with a compiler crate) and the older `inkling`.
- **Fit:** the best authoring for prose-heavy branching, and weave is its standout idea. But it is a
  single-reader story machine. Its state model (call stack, threads, visit counts per container) is a
  large compatibility surface we would have to reproduce exactly to compile ink to our bytecode. (Its
  tunnels may recurse, which is acceptable: they would run on the dialogue loop's explicit stack, §5.2.)
  It has no built-in line ids (localization goes through tags by convention).

### 2.2 Yarn Spinner — the closest fit

- **Structure:** nodes (`title:` header, `---` body `===`), lines, options (`->`, each with an optional
  `<<if>>` condition), `<<jump>>`, `<<set>>` / `<<declare>>`, typed variables, functions supplied by
  the game, and **commands** (`<<walk_to farmer gate>>`) dispatched to the game. `visited()` /
  `visited_count()` track nodes, `#line:` ids are built in for localization, and markup handles styling.
- **Yarn Spinner 3 (May 2025)** added exactly the features an MMO wants:
  - **`detour`** — a call to a node that returns.
  - **`once`** — run a block one time only.
  - **Line groups** — `=>` alternatives with `when` conditions, for barks.
  - **Node groups** — several nodes sharing one title, each with `when:` conditions; the runtime picks one.
  - **Saliency strategies:**
    - *first* — the first eligible item;
    - *best* — the highest "complexity" (each condition and boolean operator scores 1, `once` 1,
      `always` 0);
    - *best least recently viewed* — fewest selections first, then score;
    - *random best least recently viewed* — the same, but a random pick among ties.

    The selection counts belong to the strategy's own state.
  - **Smart variables** (computed from an expression) and **enums**.
- **Runtime:** a stack VM in C#. A Rust compiler and runtime exist (`yarnspinner` crates) with a Bevy
  integration (`bevy_yarnspinner`).
- **Fit:** simple semantics, close to 1:1 with what our VM already does. Commands are host calls, functions
  are host functions, detour is a call, jump is a goto. Line ids are native. Node groups with saliency are
  storylets (§3.3) at conversation scale. Its weakness is authoring deep nested choices: there is no
  weave, so every branch is a named node.

### 2.3 Others, briefly

- **ChoiceScript** (Choice of Games): indentation-based `*choice` / `#option` blocks and stats. Of note
  only as proof that **indentation-structured choices are writer-friendly**, which matters for a
  language with Python's syntax.
- **Twine** (Harlowe, SugarCube): passages and links in HTML, run by a JavaScript story format. It is
  hypertext, not a runtime we could host.
- **articy:draft** (used for Disco Elysium): a commercial graph editor whose nodes carry conditions and
  instructions in its own expression language, exported as JSON or XML. A useful model for a **visual
  editor over a compiled graph**; not a format to adopt, since it is proprietary and tool-bound.

## 3. Quest structure surveyed

### 3.1 Bethesda (Creation Kit) — stages, objectives, aliases, Story Manager

- A quest is aliases + stages + objectives + dialogue.
  - **Aliases** are roles (a reference or location) filled by conditions when the quest starts; if one
    fails to fill, the whole quest fails to start.
  - **Stages** are numbered points with script fragments.
  - **Objectives** are the journal lines shown to the player, each with targets.
- The **Story Manager** is a tree of event nodes that decides which radiant quests start in response to
  game events, by conditions on the event data.
- **Mapping:** aliases = our bindings, and alias fill conditions = start requirements. The Story Manager =
  the coordinator's requirement checks (quests-in-the-world §5). Stages = a persistent enum. Objectives
  = a journal host API. We already have most of this shape; the lesson is the clean separation of
  **roles, progress and display**.

### 3.2 The Witcher 3 (REDkit) — quest graphs

- A `.quest` file holds `.questphase` subgraphs ("phases"): nested graphs with any number of inputs and
  outputs. Signals flow through blocks.
  - **Pause blocks** hold a signal until a condition is true.
  - **Script blocks** call functions.
  - Branches run in parallel.
  - A debugger highlights the active signals.
- **Mapping:** a phase is a function; a pause is `await` on a condition or event; parallel branches are
  `await all` / `await any`. The lesson is **structured concurrency** for multi-step quests, and a
  **debugger that shows where each quest is**.
- **The catch:** in our VM a quest written as one long `await` chain lives in parked frames. An admin's
  edit or a package upgrade cancels it (§1, constraint 4). Witcher saves never face a live code edit.

### 3.3 Storylets / quality-based narrative (Failbetter, Emily Short)

- A **storylet** is atomic content with **prerequisites** and **effects** on world state, played in any
  order. Quality-based narrative gates storylets by qualities (stats).
- **Salience-based** narrative picks the most applicable item from a pool. Yarn's node groups are
  salience. A progress stat recovers ordinary branching when it is needed.
- Known cost: with lots of content, the available set overwhelms the player — so selection, not
  display of everything, is the mechanism.
- **Mapping:** this is already the shape of quest spawning (definition = storylet, requirements =
  prerequisites, the coordinator's pick = salience), and the same idea serves NPC conversations and barks.
  One concept at two scales.

### 3.4 Statecharts (Harel; SCXML)

- Hierarchical states, parallel regions, guarded transitions on events, history states.
- **Mapping:** quest progress as explicit, named state is exactly what survives an edit (state migrates by
  name, spec §6.9). It is also what an editor can draw and the coordinator can read from a snapshot to see
  how far a world-line quest got.

## 4. Options for building it in

| | Option | Verdict |
|---|---|---|
| A | Run ink or Yarn's own runtime beside the VM | **No.** Its state is outside the VM: not hashed, undone or saved with the region; two runtimes on server and client. |
| B | Compile an existing format (ink or Yarn source) to our bytecode | **Yarn: later, as an importer** — its semantics map almost 1:1. **ink: no** — weave, threads and per-container visit counting would have to match exactly. |
| C | Native dialogue and quest constructs in `.qs`, modelled on Yarn's features with ink's weave and Bethesda/statechart quest structure | **Recommended (call).** One parser, one type checker, the host API's types visible inside dialogue, no foreign state. |
| D | Data graphs (JSON, articy-style) | **Only as a view:** the editor draws the compiled graph; authors write text. |

## 5. Recommendation: dialogue in the language

> **Superseded — the execution model below is wrong.** The owner decided on 2026-11-16 that a dialogue is a
> **declarative description** run by the region's Rust code, not VM code (language spec §5.11):
> - conditions are pure VM functions the runner calls;
> - effects are `emit`ted events handled by ordinary handlers;
> - no parked handlers, no dialogue lowering to loops.
>
> The survey (§2–§4), the choice of Yarn Spinner as the model and the storylet framing still hold. What
> follows was also superseded for syntax by the language spec §5.11 and §5.13, which fit this to the
> parser as planned. The changes:
> - Line ids are `@path`, because `#` starts a comment.
> - There is no `detour` keyword: a dialogue calling a dialogue is a detour.
> - `when` without `always`: a node with no condition scores 0.
> - The default pick is `best_least_recent`.
>
> The semantics below still hold.

### 5.1 Syntax sketch (call — shape, not final)

```python
dialogue farmer(p: Player, dog: Creature):
    node start:
        "Have you seen my dog? He ran off toward the hills."        #line:farmer.start
        choice:
            "I'll find him." if stage == Stage::Open:                #line:farmer.accept
                accept(p)
                "Bless you. Bring him back before dark."            #line:farmer.thanks
            "What does he look like?":                               #line:farmer.ask_dog
                f"Brown, one ear up. He answers to {dog.name}."     #line:farmer.dog_desc
                -> start
            once "Is that blood on your sleeve?":                    #line:farmer.blood
                detour farmer_secret(p)
        "Safe roads."                                                #line:farmer.bye

dialogue greeting(p: Player):                  # a node group: one is picked by saliency
    node hello when p.has_tag("outlaw"):
        "Keep walking."                                              #line:greet.outlaw
    node hello when once per p:
        "A new face! Welcome."                                       #line:greet.first
    node hello when always:
        => "Morning."                                                #line:greet.a
        => "Weather's turning."                                      #line:greet.b
```

### 5.2 Semantics (call)

- **A string statement inside a dialogue is a line.** It is sent as a host command:
  `(line id, typed substitutions, speaker)`. The sim hashes the id and values, never the text. Clients
  look up the text in the package's **string table** for their locale and format the substitutions (plurals
  and markup are client-side, as in Yarn). Tooling writes the missing `#line:` ids back into the source,
  as Yarn's does, so authors never type them.
- **`choice:` is ink's weave in Python's indentation.** Each option's block runs, then control falls
  through to after the `choice:` block — that is the gather, with no node names needed. `once` makes an
  option once-only (ink's `*`); options are sticky by default (ink's `+`). An `if` guard hides an option.
- **A choice is an `await`.** The VM offers the options to the player through a host call and parks the
  handler until the player's pick comes back. The pick is **player input**, stamped into the sim at a tick
  like any other input, and predicted by the choosing client. It is **not** a server promise. Walking away,
  logging out or a timeout resolves it with an error (`Left`, `Timeout`), handled with the language's
  error forms. Many conversations with one instance are many parked handlers, so the per-instance
  parked cap (spec §8.2) needs a dialogue-sized default.
- **Recursive dialogue runs as a loop** (spec §6.1: only functions may not call themselves).
  - The compiler lowers each set of dialogues that can detour into each other (a strongly connected
    component; spec §5.11) **into one dispatch loop**. Its state is the
    current `(dialogue, node)` and an **explicit return stack**, a heap list in the conversation's
    frames. Both are saved, hashed and undone with the parked handler.
  - `-> node` (a jump within the dialogue) replaces the current position.
  - `detour other(...)` pushes the return position with `other`'s arguments, and the end of `other`
    pops it. Detours may therefore form cycles and recurse — the "Is that blood on your sleeve?"
    branch may detour back into the greeting.
  - The depth is bounded by the heap cap, and every step costs fuel. A runaway cycle faults with
    `HeapCapExceeded` or simply pauses on fuel. It never overflows the Rust stack.
  - ink's recursive tunnels map onto the same stack.
- **Ordinary script functions called from a dialogue** (`accept(p)`) are still calls, under the
  no-recursive-calls rule as usual.
- **Node groups and saliency** follow Yarn 3. Nodes share a name and carry `when` conditions, and a
  strategy picks one: *first*, *best* or *best least recently used*, scored like Yarn's. Random
  tie-breaks use the host's deterministic `roll`, and selection counts are VM state. **Line groups**
  (`=>`) are the same at line scale, for barks.
- **Memory is scoped explicitly.**
  - `once` means once for the whole script instance.
  - `once per p` means once per player.
  - `visited(node)` and `visited(node, p)` mirror these.
  - Per-player memory is a dict keyed by player id in VM state, so it grows with players. It is dropped
    when the quest is reaped. If that proves too large, it can move to the quest namespace.
- **Short-lived by nature.** A conversation lives in parked frames, so an edit or upgrade cancels open
  conversations. They restart, which is acceptable for dialogue. Quest **progress must not** live there
  (§6).

### 5.3 What the language and VM need

- **Parser:** `dialogue`, `node … [when …]`, `choice:` with `once` and `if`, `->`, `detour`, `=>`,
  `#line:` tags, and string statements as lines.
- **Compiler:** dialogue → one function (a node state machine); line ids → a string table emitted into
  the package; saliency tables.
- **VM:** probably no new instructions. A choice is a new promise kind resolved by an input event. The
  selection helper is a builtin.
- **Host API description** (spec §9): `say_line`, `offer_choices`, the choice-input event, `Left`, and
  the speaker and listeners — lines are broadcast to nearby players, and choices are taken only from the
  conversation's player.
- **Standalone runner:** an interactive mode (`quest run --talk`) that prints lines and reads choices,
  plus scripted choices for tests.

## 6. Recommendation: quest structure in the language

> **Superseded for syntax by the language spec §5.12**:
> - Stage handlers are ordinary `@on(Event) fn` items inside a `stage`.
> - A role's start requirement is a `where` expression.
> - `objective … at expr`.

### 6.1 Syntax sketch (call)

```python
quest LostDog:
    role farmer: Person          # Bethesda's aliases = the coordinator's bindings
    role dog: Creature

    stage Open:
        objective "Find {dog.name}"  at dog       #line:lostdog.find
        on DogFound(e):
            -> Returning
    stage Returning:
        objective "Bring {dog.name} home" at farmer  #line:lostdog.return
        on DogDelivered(e):
            complete(participants=[e.player])
        on DogDied(_):
            fail()
```

### 6.2 Semantics (call)

- **A `quest` is a flat statechart.** Its stage is persistent top-level state (an enum generated from the
  stages), so it **migrates across edits by name** (spec §6.9). The editor and the journal can show it,
  and the coordinator can read it from a region snapshot.
- **Handlers inside a stage run only in that stage.** `-> Stage` transitions. Nested stages (hierarchy)
  and parallel sub-stages ("find the dog *and* the collar") come later. Until then, parallel objectives
  are flags inside one stage.
- **Objectives are journal entries.** Each has a line id and optional targets, and the host shows them to
  participants (Bethesda's objectives). They change with the stage, so the journal is always derived from
  state, never stored separately.
- **Roles are the bindings** the quest was spawned with (quests-in-the-world §1), typed. A role that no
  longer resolves (its entity died or left) raises an event the quest must handle, rather than failing
  silently — Bethesda's empty-alias failures, made explicit.
- **Long `await` chains** (the Witcher style) stay available inside one stage for short sequences, for
  example a cutscene. They are never where progress lives.

### 6.3 One concept at two scales

The world picks quests by requirements and salience (the coordinator), and a conversation picks nodes by
`when` conditions and salience (the VM). A future step could let the coordinator's requirement query use
the same `when` syntax and scoring, so authors learn one rule. Requirements stay declarative either way
(quests-in-the-world §5.2).

## 7. Open questions for Dawid

1. Native syntax (§5–§6) with a later Yarn importer, or Yarn-format `.yarn` files compiled by `quest-lang`
   from the start (writers familiar with Yarn; two surface syntaxes for one VM)?
2. Is `once` per instance by default, and `once per p` opt-in — or the other way round?
3. May several players share one conversation, so that one chooses while others listen, or is each
   conversation private?
4. Do choices time out, and does walking away count as an answer?
5. Should the journal show only the current stage's objectives, or the history of completed ones too?

## 8. Sources

- ink: [JSON runtime format](https://github.com/inkle/ink/blob/b1d54e7444713a84770c2df1dbc6fdc83706d7aa/Documentation/ink_JSON_runtime_format.md);
  Rust port [bladeink](https://docs.rs/crate/bladeink/1.2.5/source/README.md); [inkling](https://docs.rs/crate/inkling/0.9.2);
  [narrator (Lua, state save/load)](https://fr.github.com/astrochili/narrator).
- Yarn Spinner: [3.0: what to expect](https://yarnspinner.dev/blog/yarn-spinner-30-what-to-expect),
  [node groups](https://yarnspinner.dev/docs/yarn/03-advanced/01-node-groups/),
  [saliency](https://docs.yarnspinner.dev/next/write-yarn-scripts/syntax-basics/node-groups/saliency),
  [changelog](https://docs.yarnspinner.dev/3.1/changelog), Rust [yarnspinner](https://docs.rs/yarnspinner) /
  [yarnspinner_runtime](https://docs.rs/yarnspinner_runtime), [Bevy integration](https://docs.yarnspinner.dev/next/yarn-spinner-for-other-engines/bevy/yarn-projects).
- Bethesda: [CK Quests](https://ck.uesp.net/wiki/CreationKit:Quests), [Quest Objectives tutorial](https://skyrimck.uesp.net/wiki/Bethesda_Tutorial_Quest_Objectives),
  [Story Manager tutorial](https://skyrimck.uesp.net/wiki/Bethesda_Tutorial_Story_Manager), [Radiant Story](https://skyrimck.uesp.net/wiki/Radiant_Story),
  [Fallout CK Quest (alias fill failures)](https://falloutck.uesp.net/wiki/Quest).
- The Witcher 3 REDkit: [create a quest](https://cdprojektred.atlassian.net/wiki/spaces/W3REDkit/pages/6327243/HOW-TO+Create+a+new+quest),
  [quest nodes](https://cdprojektred.atlassian.net/wiki/spaces/W3REDkit/pages/6327742/HOW-TO+Use+quest+nodes),
  [conditions and functions](https://cdprojektred.atlassian.net/wiki/spaces/W3REDkit/pages/135987240/HOW-TO+Use+quest+conditions+and+functions).
- Storylets: Emily Short, [Beyond branching](https://emshort.blog/2016/04/12/beyond-branching-quality-based-and-salience-based-narrative-structures/),
  [Storylets: you want them](https://emshort.blog/tag/quality-based-narrative/); [Quality-based narrative](https://videlais.github.io/simple-qbn/qbn).
