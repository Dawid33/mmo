# Handoff — quest script Phase 10: dialogue and quests (standalone)

Date: 2026-11-17. For the context that implements Phase 10 of the quest-script plan: dialogue and quests compiled
and run **inside the `quest/` workspace**, with the stub host — no game integration.

## Where things stand

- **The MVP is built** (Phases 0–9; close-out `c3a80cae`, README follow-ups up to `6bd02061`). Read
  `quest/README.md` first: it is the language as built, the CLI, the tunables and the testing rule.
- **The front end already handles the dialogue and quest syntax of spec §5.11–5.13 (the version before the
  2026-11-16 construction design).** It lexes, parses, resolves and type-checks it, then reports `not-yet`
  (`quest/README.md:140`):
  - AST: `quest-lang/src/ast.rs:489` onward — `Dialogue` (with `target`, `topic`), `Node`, `Line`, `Cond`,
    `StepKind` (`Line`, `Choice`, `Group`, `When`, `Once`, `Jump`, `Detour`, `Emit`, `Return`),
    `ChoiceOption`, `OptionMark` (`once` / `default`);
  - contextual words: `indent.rs:123-170`;
  - `not-yet`: `quest-lang/src/{lib.rs,hir.rs,codegen/mod.rs}`.
- **Phase 10 is an outline** in `docs/superpowers/plans/2026-11-16-quest-script-language-mvp.md` ("Phase 10 —
  Dialogue and quests", Tasks 36–40). It says to detail it before starting. The outline and spec §5.11–5.13 /
  §9.1 are **uncommitted edits** in the working tree.

## The design, in order of precedence

1. **`docs/superpowers/specs/2026-11-16-quest-construction-design.md`** — **accepted by Dawid 2026-11-16 ("go
   with your recommendations")**, not yet folded into the spec. Where it differs from the spec, it wins. Its
   §0 summary, §11 judgement table and §12 answered questions list every change. Worked examples:
   `2026-11-16-quest-examples.md`.
2. **Spec §5.11–5.13 and §9.1** (`docs/superpowers/specs/2026-11-16-quest-script-language-design.md`). Decided
   by Dawid:
   - dialogue is **declarative data run by a runner**, never VM code;
   - conditions are **pure VM functions**, and effects are **`emit`ted events** handled by `@on` handlers;
   - attached dialogues combine **commutatively** (pooled greetings, a topic menu);
   - conversations work across region boundaries (via mirrors — standalone, a `Mirror` type and a movable
     `Conversation`).
3. **`docs/superpowers/specs/2026-11-16-dialogue-examples.md`** — Yarn Spinner samples converted, plus
   Errantry examples. Every quest-script block must compile once Phase 10 is done (after the construction
   design's changes are applied to them).

What the construction design adds on top of the spec (grammar unless noted):

- **Quests:**
  - named, stateful objectives (`count n`, `optional`, methods);
  - `-> Stage when cond` as a stage step;
  - an `Entered` event per stage, and stage-scoped timers;
  - `requires`, `optional role`, `repeatable quest X(cooldown=…)`;
  - `part … on role` / `at role` for multi-region quests;
  - the builtins `join` / `leave`, and `complete()` defaulting to the participants.
- **Dialogue:**
  - `for npc where pred`;
  - **named exits** (`exits`, `return accepted`, `detour x:` arms, `DialogueEnded.exit`);
  - `scene` (several speakers, no choices, timed lines; a bark is a one-actor scene);
  - an optional `pause` step.
- **Scoring:** count **conjuncts**, not operators — one rule for definitions, greetings, nodes and lines.
- **Pacing (runner):** a conversation runs to the next choice in one tick and the client paces the lines; the
  sim waits only at choices and for a `Done` acknowledgement. Scenes and barks use durations the compiler
  derives from the source text, stored in the graph.
- **Checks:**
  - the query subset for `where` / `requires`;
  - `stateless-dialogue`, `unknown-exit`, `scene-choice`, `entered-outside-stage`;
  - new cases of `bad-where` and `duplicate-def`;
  - a mirror read-set size warning (`MIRROR_WORDS_WARN`).
- **Dropped:** `world.region_query`. Optional: rename `world.*` to `ns.*`.

## Step 0 — before any code (with Dawid)

1. **Fold** the accepted construction design into spec §5.11–5.13 and into the plan's Phase 10. Its own
   header says folding happens "by him or on his word", so ask first. The implementing session that blocked
   spec edits has finished, but confirm it.
2. **Detail Phase 10 into tasks** in the plan (the outline's Tasks 36–40 are the skeleton), from the folded
   spec.
3. **Settle the test rule.** `CLAUDE.md` (owner's rule, 2026-11-16) says that after the MVP a test is written
   only as a regression for a bug actually found — "not alongside a new feature by default" — with fuzzing
   for broad assurance. The Phase 10 outline assumes initial-implementation tests and goldens. Ask Dawid which
   applies to Phase 10.
4. **Commits:** the spec and plan edits and the design docs above are uncommitted, and other sessions'
   hunks share these files. Commit only your own paths and hunks (stage with a pathspec; `git diff --cached
   --name-only` before each commit).

## Scope — standalone, in `quest/`

1. **Grammar additions** from the construction design. New contextual words go through the layout pass
   (`part`, `requires`, `scene`, `pause`, `optional`, `repeatable`, `exits`, `count`); reserving `scene` /
   `repeatable` is the builder's call. The grammar must stay conflict-free.
2. **Checks:**
   - purity (`impure-condition`, transitively);
   - `bad-line-text`, `missing-line-id`, `duplicate-line-id`, `dialogue-signature`, `role-assign`;
   - the query subset;
   - the construction design's new diagnostics;
   - the mirror-size warning.
3. **The dialogue graph** (`quest-vm`): stored in `Program.dialogues`, inside `to_bytes` / `from_bytes` and
   validation (hostile bytes → `Err`, never a panic). It holds the steps, groups, exits, line ids and line
   durations.
4. **Pure evaluation:** `Vm::eval_pure`, `Program.dialogue_read_set`, `Vm::mirror()`, `Mirror::eval_pure`.
5. **Lowering** of conditions, substitutions, `emit` arguments and `for` / `where` targets to pure
   functions. Quests lower to:
   - the stage enum as a persistent global;
   - objective state;
   - stage-guarded handlers, `Entered`, timers;
   - the string table (`Compiled.lines`).
6. **The reference runner** (`Conversation`, and scenes):
   - engine-agnostic plain data with `StateHash`, which the game's region code will embed;
   - commutative merge, pacing per the construction design, exits, `DIALOGUE_STACK_MAX`;
   - `emit` returned as values for the caller to route;
   - movable mid-conversation.
7. **Stub host and CLI:** `Talked` / `DialogueEnded`, scripted picks; `quest run --talk`, `--picks`, `--roles`.
   Multi-instance support in the stub is needed only if `part` is in scope — decide in Step 0.
8. **The examples compile and run:** dialogue-examples, quest-examples, the spec's snippets. Goldens on
   wasm32 if Step 0 says tests apply.

**Out of scope** (the game integration plan; the list is at the end of the plan's Phase 10):
- region anchors;
- `WorldView` host queries;
- `Intent`s;
- the VM's undo driven by region rollback;
- conversations carried with NPCs;
- mirror messages;
- the coordinator, packages, and engine person data (`occupation`, tags).

## Rules that bind

- **The `quest/` workspace stays isolated:**
  - its own `Cargo.toml` and `Cargo.lock`;
  - nothing in the root depends on it;
  - recipes `just quest-check | quest-test | quest-run | quest-wasm` (CPU-capped);
  - formatting only inside `quest/`, never `cargo fmt` from the root.
- **Determinism:** the graph and mirrors are hashed with `StateHash`, never serialized bytes. No `HashMap` in
  state. `libm`. The VM and compiler never panic on any input.
- **Work on `develop`, no worktrees.**

## Open, decide as you meet them

- **Barks:** keep the `(npc)` dialogue form, or replace it with a one-actor `scene`?
- **Conversations are private** (one talker): the construction design assumes this (§12.10). Confirm before
  the runner is built.
- **The `ns.*` rename** — do it now, while only the stub uses `world.*`, or not at all.
