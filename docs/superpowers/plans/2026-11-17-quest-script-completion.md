# Quest script — one plan to finish it: Phase 10, leftovers, fuzzing, the game, packages

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Take the quest script language from "the standalone MVP is built" to "a quest written as a package spawns
in a running world, is talked to, completed and reaped" — every open task of every quest-script document, in one
order.

**Architecture:** Three layers, finished bottom-up. (1) **The language** (`quest/`, its own workspace): Phase 10
lowers `dialogue` / `scene` / `quest` items to plain data plus synthesised pure functions and builds an
engine-agnostic runner; then the review's leftovers are closed and fuzzing replaces hand-written tests. (2) **The
region sim** (`crates/game`): a script instance is a `quest_vm::Vm` held as region state — hashed, undone by the
region's transactions, saved in cuts, run identically on the server and on predicting clients — behind a host
that answers queries from `WorldView` and turns commands into region changes and `Intent`s; conversations are
carried NPC state. (3) **The coordinator and packages**: namespaces, spawn jobs, completion, reaping and acts are
coordinator rows (single writer); definitions and bytecode come from hash-pinned content packages that clients
fetch too.

**Tech Stack:** Rust (the pinned nightly). `quest/`: LALRPOP, logos, `libm`, `state-hash`, proptest, wasm32,
cargo-fuzz (new, Part 3). Game side: the existing `game` / `server` / `client` / `world-store` / `slint-ui` crates,
Postgres or SQLite through `world-store`, bincode framed by `game::encode_packet` on the wire.

**Specs — this plan argues from all of them; executors read the ones their Part names:**

| Document (`docs/superpowers/…`) | Status | Feeds |
|---|---|---|
| `specs/2026-11-16-quest-script-language-design.md` | approved; §5.11–5.13 and §9.1 folded 2026-11-17 | everything; §5.11–5.13 → Part 1, §9.1 / §6.6 / §8 → Part 4 |
| `specs/2026-11-16-quest-construction-design.md` | accepted 2026-11-16, folded into the spec | the *reasons* behind Part 1; §3.3–3.6, §4.3, §5.2–5.3, §6, §9.3 name the runner / engine / coordinator work of Part 4 |
| `specs/2026-11-16-quest-examples.md`, `specs/2026-11-16-dialogue-examples.md` | accepted examples | the Phase 10 corpus (Task 49); the first real packages (Task 75) |
| `specs/2026-11-16-quests-in-the-world-design.md` | **draft, not approved** | Part 4 (its §13 is the order) |
| `specs/2026-11-16-content-packages-design.md` | **draft, not approved** | Part 5 |
| `docs/game-design.md` §4–§7, §10 | decided parts marked there | what Parts 4–5 are for |
| `plans/2026-11-16-quest-script-language-mvp.md` | built (Tasks 1–35) | its Global Constraints, Task conventions, "Deferred minors" and "Not done" → Parts 1–3 |
| `plans/2026-11-17-quest-phase-10-dialogue-and-quests.md` (+ its handoff) | planned, not started | **copied whole into Part 1**; this plan supersedes it |
| `specs/2026-11-16-dialogue-and-quest-structure-research.md`, `specs/2026-07-05-embedded-scripting-language-research.md`, the two `…-handoff.md` files of 2026-11-16 | background, done | nothing to build; read only for reasons |

**Precedence where they disagree** (newest decision wins):

1. The folded spec §5.11–5.13 beats the construction design's wording (the fold is the decision; the design keeps
   the reasons).
2. `quests-in-the-world` §2 beats spec §8.1: a **namespace belongs to one quest instance**, made and deleted by the
   system, never by a player; "per block" in the spec reads "per script instance"; scripts are edited by admins
   in editor mode, not typed by players (game-design §10).
3. Spec §5.12 / construction design §3.6 beat `quests-in-the-world` §5.4's last paragraph: quest state has **one
   owner, the home instance**; parts read a stamped mirror and change it only by `emit`. The namespace is for
   durable data only (what survives a repeatable reset).
4. `world.region_query` is dropped (spec §9.1); a far region is watched by a part.
5. `quest/README.md` beats the spec where the build deviated (the MVP plan's "What differs from the plan"); Task 51
   carries those back into the spec.

## Where things stand (2026-11-17)

- **Built:** the MVP — Tasks 1–35, about 29,500 lines, 403 tests, 16 run and 14 fail corpus cases, goldens
  reproduced on wasm32. `quest check` / `quest run` work against the stub host.
- **Parsed but refused:** `dialogue` and `quest` items in the *pre-fold* syntax lex, parse, resolve and type-check,
  then report `not-yet` (`quest-lang/src/{lib.rs:104,codegen/mod.rs:235}`).
- **Not started:** Phase 10 (no commit, no working-tree change in `quest/`); fuzzing; anything in `crates/` — the
  root workspace has no reference to `quest` beyond the `exclude` line.
- **Done elsewhere and relied on here:** embark/return removed (2026-11-17), which `quests-in-the-world` §13 put
  first; the coordinator's state is a versioned row with write-before-send (Elastic 2a).

## The whole road, in order

| Part | Tasks | What exists afterwards | Needs |
|---|---|---|---|
| 1 — Phase 10: dialogue and quests, standalone | 36–49 (+37b) | every example of the two example documents compiles and runs in `quest run`; goldens on wasm32 | — |
| 2 — MVP leftovers | 50–52 | hostile bytes and saved state are safe to take from a store or the network; the spec says what was built | Part 1 |
| 3 — Fuzzing | 53–55 | coverage-guided fuzzers over the compiler, the formats, the VM invariants and the runner; a first campaign triaged | Part 1 (runner target), Part 2 |
| 4 — The game | 56–71 | a hand-placed script instance → conversations → namespaces → spawned, completed and reaped quests → acts → editor mode | Parts 1–2; **gate G1, G2** |
| 5 — Content packages | 72–77 | `quest pkg`, a world lock, clients fetching by hash, the first real packages, the package server | Tasks 72–73 can start after Part 1; the rest after Task 68; **gate G1, G5** |
| 6 — Not planned | — | the list of what stays open, so nobody thinks it was forgotten | — |

Parts 2 and 3 may run beside the start of Part 4 (different crates). Tasks 72–73 (`quest pkg` in `quest/`) may run
beside Part 4. Nothing else is parallel: Part 4's tasks each build on the one before.

## Gates — decisions Dawid makes before the Part that needs them

- **G1 — approve the two drafts.** `quests-in-the-world` and `content-packages` are marked "draft, not approved".
  Parts 4 and 5 build their **(call)** items as written. The ones tasks depend on: requirements as a declarative
  query over coordinator facts and region snapshots (§5.2); the job id as the idempotency key and "Running only
  after a committed cut" (§5.4); everything a quest creates tagged with its instance id (§7); budgets as tunables
  (§8); admins as a world-store table granted by `worldctl` (§10); one package version per world, the lock in the
  world row, a world keeping copies of its builds (packages §3, §5).
- **G2 — the isolation rule changes.** CLAUDE.md says nothing in the root may depend on `quest/`. Part 4 needs
  `game` to link the VM. **Recommended:** `quest/` stays its own workspace (its tests, CLI and lockfile untouched)
  and `crates/game` takes `quest-vm` as a path dependency — the root `exclude` stays, so the crate is built by the
  root but is no member of it. `quest-lang` is linked only by `server` (an admin's edit compiles there) and never
  by `game` or `client`. The alternative of spec §10 — moving the crates into the root workspace — would end the
  separate lockfile and the two-agents rule for no gain.
- **G3 — the test rule outside `quest/`.** Read here as: the regression-only rule covers code **inside `quest/`**
  after Task 49. Game-side code of Parts 4–5 follows CLAUDE.md "Testing" as every other game feature does (its own
  tests, `just determinism-wasm`, `just test-threads`). Say so if the rule was meant wider.
- **G4 — Task 37b.** This plan adds fallible host *queries* to Phase 10 (beside Task 37's fallible commands), with
  tests, because spec §9.1's `OutOfReach` needs them and after Task 49 a feature in `quest/` carries no tests.
  Dropping it leaves the `not-yet` in place until the game host needs it (Task 58).
- **G5 — opens the drafts leave, with the default this plan takes:** a reaped quest's item in an inventory →
  becomes an ordinary item (Task 68); a non-admin sees nothing at a script block (Task 71); a lock change with
  clients connected → they are asked to rejoin (Task 74); no script-function escape hatch for requirements (Task
  69); the package server **waits** for the management PostgreSQL instance (Task 77 is last and may never start
  from this plan); a quest never targets a parked region.

## Global Constraints

Inside `quest/` (Parts 1–3; from the MVP plan and the Phase 10 plan, unchanged):

- `quest/` keeps its own `Cargo.toml` / `Cargo.lock` / `target/`; the only path dependency out of it is
  `crates/state-hash` (`default-features = false`). Run through `just quest-check | quest-test | quest-run |
  quest-wasm` (CPU-capped). Format only inside `quest/` (`cd quest && cargo fmt -p <crate>`), never from the root,
  never `--all`; check the whole repository's `git status` after any formatter run.
- `quest-vm`, `quest-lang`, `quest-stub`, `quest-corpus` build for `wasm32-unknown-unknown`; no filesystem, time,
  thread or environment access in `quest-vm` and `quest-lang`. `#![forbid(unsafe_code)]` in every crate.
- Determinism: everything that is state hashes with `StateHash`, never serialized bytes. No `HashMap` / `HashSet`
  in state or in the compiler's iteration paths. `libm` for floats, canonical NaN. Randomness only through a
  stateless `roll(tick, key, salt)`. No `usize` in serialized or hashed bytes. Byte-identical
  `Program::to_bytes()` on every platform.
- **The VM, the runner and the compiler never panic on any input** — source text, program bytes, saved state, host
  values, picks, instance ids → a diagnostic, an `Err`, a fault or a refusal.
- Every format change bumps `BYTECODE_VERSION` once per task and re-blesses the corpus goldens in the same commit
  (`QUEST_BLESS=1 just quest-test "-p quest-corpus"`; read the diff — only `bytecode` lines may change in old
  cases). Host API changes bump `HostApi.version`.
- Tunables are named consts in `quest-vm/src/consts.rs`, one doc line each.
- **Tests in `quest/`:** Part 1 is initial implementation — its tasks carry their tests (Dawid, 2026-11-17). From
  Task 50 on, a test in `quest/` is written **only as a regression for a bug actually found** (a review finding
  and a fuzz crash both count). A feature added to `quest/` after Task 49 is checked by running the corpus and a
  hand-run example, and by the fuzzers.

In the game tree (Parts 4–5; CLAUDE.md, the lines that bind here):

- `game`, `server`, `worldgen` stay engine-agnostic. Script instances are **region-local sim state**: hashed
  (`StateHash`), undone bit-exactly (`hash(before) == hash(after undo)`), saved in the region's cut part; ordered
  state in `BTreeMap`; no inherent `f32` transcendentals.
- The coordinator never writes region state; a cut part writes only rows of its own regions; **no table two
  hosts' parts can both write**. Every store write is fenced. A new coordinator field is classified in
  `projection!`; a new `WorldManager` field in `host_fields!`; a new `Ecs` / `Client` field in `carry_components`
  / `Client::arrive` (no `..`).
- Anything entering the sim enters as a stamped event at one world tick on every machine, applied right before the
  next `Tick` in canonical order (`SlotInbox`) — never on receipt. Nothing wakes a parked region for a quest.
  Intents and mirror messages are sent only from final ticks; a client's replay sends nothing.
- Write-before-send on the coordinator; requests memoised by `RequestId`; a job id / promise id / instance id is
  the idempotency key of everything that can be re-delivered.
- Wire: `bincode` through `game::encode_packet`; a link change bumps `LINK_PROTOCOL_VERSION`. Old worlds are wiped
  and re-forged, never migrated (migrations are still added, never edited).
- UI is Slint only; a per-world client resource is registered for session reset. wasm: no shared-memory threads.
- Verify with `just test-one <crate> "<filter>"`, `just smoke`; `just determinism-wasm` after every task that adds
  sim state or changes what is hashed; `just test-threads` after anything crossing a seam; each touched crate's
  whole non-client suite once per Part. Tests slower than 100 ms are `#[ignore = "slow: >100ms — run via just
  test-full"]`. Client tests: ask first.
- Git: `develop`, no worktrees, never `git add -A`, never `git stash`; commit only this task's paths with a
  pathspec and check `git diff --cached --name-only` first — other sessions stage in the shared index. For shared
  files (`CLAUDE.md`, `justfile`, `Cargo.toml`, the spec) stage only your hunks (`git diff -U3`, drop foreign
  hunks, `git apply --cached --recount`). Keep the `justfile` in sync with every new command.

**Phase 10 tunables (`quest-vm/src/consts.rs`):**

| Name | Value | Meaning |
|---|---|---|
| `COND_FUEL` | 500 | fuel for one pure-function evaluation |
| `DIALOGUE_STACK_MAX` | 16 | detour depth; deeper ends with `TooDeep` |
| `CONVERSATION_LINGER_TICKS` | 200 | after the end, wait this long for `Done` |
| `LINE_BASE_TICKS` | 30 | timed-line duration base (1.5 s at 20 ticks/s) |
| `LINE_TICKS_PER_CHAR` | 1 | per Unicode scalar of source text |
| `LINE_MIN_TICKS` / `LINE_MAX_TICKS` | 30 / 200 | clamp |
| `TIMERS_MAX` | 16 | live timers per instance; `after` beyond faults (`TooManyTimers`) |
| `MIRROR_WORDS_WARN` | 256 | `mirror-size` warning threshold |
| `GRAPH_STEPS_MAX` | 65535 | steps per program (format bound, validated) |

**Game-side tunables (new, named consts in `crates/game/src/script/consts.rs`; values are first guesses to
measure, not spec values):** `SCRIPT_FUEL_PER_TICK` 2000 per instance, `REGION_SCRIPT_FUEL_PER_TICK` 20000 (the
region's whole budget, shared in instance-id order), `SCRIPT_INSTANCES_PER_REGION_MAX` 32, `TALK_RANGE_BLOCKS` 4
(must stay ≤ `HALO_BLOCKS`), `HEAR_RANGE_BLOCKS` 16, `QUEST_SETUP_DEADLINE_MS` 30000, `QUEST_BINDING_COOLDOWN_MS`
600000, `QUESTS_PER_REGION_MAX` 2, `QUESTS_PER_LIVE_AREA_MAX` 6, `QUESTS_PER_WORLD_MAX` 256,
`QUEST_CHECK_PERIOD_MS` 5000, `LIVE_AREA_REGIONS` 2, `QUEST_FAULTS_BEFORE_REAP` 8, `QUEST_LIFETIME_DEFAULT_MS`
3 days.

## Review Focus

Phase 10 (each has its test in the task named — Part 1):

1. **A stale pick** — against a menu or choice that changed since it was offered. Expect a refusal plus a re-sent
   offer, never a wrong branch or a panic. Task 45 `stale_pick_is_refused_and_menu_resent`,
   `out_of_range_pick_is_refused`.
2. **A condition that faults or runs out of fuel.** Expect: counts as false, reported once, the conversation goes
   on. Task 45 `faulting_guard_hides_option`, Task 41 `eval_pure_out_of_fuel_is_err_and_state_unchanged`.
3. **An edit mid-conversation / mid-stage.** Expect `Edited` for the conversation; a removed stage falls back to
   the first with a migration-report line. Task 45 `edit_removing_current_node_ends_edited`, Task 44
   `edit_removing_current_stage_falls_back_to_first`.
4. **Role values that do not fit.** Expect a load refusal naming the role. Task 47
   `role_of_wrong_type_is_refused`, `role_failing_where_is_refused`.
5. **Two emits racing one transition.** Expect one stage entry, one `Entered`, the stale stage timer cancelled.
   Task 44 `stage_timer_cancelled_on_change`, `two_finishes_one_entry`.

The game (Parts 4–5):

6. **A host crash between the coordinator's row and the region's cut** — a job delivered and accepted, the host
   dies before the cut; or a completion reported, then rolled back. Expect: re-delivery makes no second instance
   (job id), a completion is recorded once (instance id), a reward is not granted twice. Task 67
   `redelivery_after_rollback_makes_one_instance`, Task 68 `completion_recorded_once_across_a_host_rollback`.
7. **A misprediction while a handler is parked or mid-segment** — the client rolls a region back across an
   `await`, a conversation batch, a timer firing. Expect: bit-exact undo, the replay reproduces the server's
   hash, nothing is sent twice. Task 57 `script_state_undo_is_bit_exact_across_a_park`, Task 62
   `conversation_survives_rollback_and_replay`.
8. **The NPC, the talker or a part's entity crossing a seam mid-conversation**, and a talker walking out of range
   or disconnecting. Expect: the conversation moves with the NPC, picks follow, an absent talker ends it `Left` /
   `OutOfRange`; no orphaned conversation holds the NPC forever. Task 63 `conversation_crosses_a_seam_with_its_npc`,
   Task 62 `talker_gone_ends_out_of_range`.
9. **A script that never finishes or grows without bound** — a fuel-hungry handler in a busy region, 32 instances
   wanting fuel at once, an instance faulting every event. Expect: the region's tick cost stays inside
   `REGION_SCRIPT_FUEL_PER_TICK`, starved instances continue next tick in a fair order, the faulting quest is
   reaped after `QUEST_FAULTS_BEFORE_REAP`. Task 57 `region_fuel_budget_is_shared_fairly`, Task 68
   `faulting_instance_is_reaped`.
10. **Bytes from outside** — a region row holding a script state saved by another bytecode version, a package
    build whose hash does not match, a program a client fetched that is truncated. Expect: a `LoadFailed` region
    (never regenerated, never a panic), a refused build, a refused join with a reason. Task 59
    `region_with_unknown_bytecode_version_is_load_failed`, Task 74 `build_with_wrong_hash_is_refused`.

---
## Part 1 — Phase 10: dialogue and quests, standalone (Tasks 36–49)

Copied whole from `2026-11-17-quest-phase-10-dialogue-and-quests.md` (which this plan supersedes), with one
addition: Task 37b. Spec: §5.11–5.13, §9.1. The MVP plan's Task conventions apply: run tests with
`just quest-test "-p <crate> <filter>"`; "FAIL" means the test fails or does not compile because the item is
missing; after every task `just quest-check` is clean; every task ends with one commit of `quest/` only. "Steps
1–5 as usual" = write the tests → run, FAIL → implement → run, PASS, plus the crate's suite and `just
quest-check` → commit.

**How it works.** The front end turns every condition, substitution, `emit` argument, `for`/`where` target and
stage transition into an ordinary **synthesised pure `fn`**, so inference, the call graph and codegen handle them
like any function; purity and the query subset are extra checks over those fns. A dialogue becomes plain data
(`DialogueGraph` in `Program`), run by an engine-agnostic **runner** in `quest-vm` that never touches a live VM: it
evaluates pure fns through a `DialogueEnv` and returns `emit`s as values. A quest lowers to generated persistent
globals (stage, objectives, participants, roles), generated fns (stage entry, transitions) and a few new VM
features (script events, timers, roles, instance owners). The stub grows into a small multi-instance world.

**Decisions settled (Dawid, 2026-11-17):** the construction design is folded into the spec; Phase 10 is initial
implementation (tests and goldens like Phases 0–9); `part` is fully in scope, several instances in the stub
included; conversations are private; a bark is a one-actor scene (the `dialogue name(npc: Person)` form stays);
`world.*` keeps its name.

### File structure

```
quest/crates/quest-lang/src/
  grammar.lalrpop, indent.rs, ast.rs, ast/dump.rs, parse.rs    # Task 36
  hir.rs, hir/dialogue.rs (new), resolve/dialogue.rs, resolve/quest.rs (new)   # Task 38
  infer/dialogue.rs (new), purity.rs (new), query.rs (new), readset.rs (new)   # Task 39
  codegen/dialogue.rs (new), codegen/quest.rs (new), lib.rs (`Compiled.lines`) # Tasks 43–44
quest/crates/quest-vm/src/
  host.rs (HostEnum, property, fact)                            # Task 37
  program.rs, bytecode.rs, asm.rs, sched.rs, interp.rs, vm.rs   # Tasks 40–42
  timers.rs (new), pure.rs (new: eval_pure, Mirror)             # Tasks 40–41
  dialogue.rs (new: the graph format)                           # Task 42
  dialogue_run.rs (new: Conversation, Records), scene_run.rs (new)   # Tasks 45–46
quest/crates/quest-stub/src/
  api.rs (quest host API), people.rs (new: fixture), world.rs (new: instances, routing, talk, scenes)  # 37, 47
quest/crates/quest-cli/src/main.rs                              # Task 48
quest/corpus/{run,fail}/…                                       # every task adds cases; Task 49 the examples
```

---

### Phase 10a — Front end

#### Task 36: Grammar — the construction design's syntax

**Files:** Modify `quest-lang/src/{grammar.lalrpop,indent.rs,ast.rs,ast/dump.rs,parse.rs}`; Test
`quest-lang/tests/{lex.rs,parse_items.rs}`.

**Interfaces — Produces (AST):**

```rust
// ItemKind gains / changes:
Quest(Quest)                                   // was Quest(String, Vec<Item>)
Scene(Dialogue)                                // a `scene` item; Dialogue.kind = Scene
Role(Role)                                     // was Role(String, Type, Option<Expr>)
Objective(Objective)                           // was Objective(Expr, Option<LineId>, Option<Expr>)
Requires(Expr)
Transition(String /*stage*/, Expr /*cond*/)    // `-> Stage when cond` as a stage member
Part(Part)
pub struct Quest { pub name: String, pub repeatable: bool, pub options: Vec<Arg>, pub at: Option<String>, pub body: Vec<Item> }
pub struct Role { pub name: String, pub ty: Type, pub cond: Option<Expr>, pub optional: bool }
pub struct Objective { pub name: String, pub text: Expr, pub id: Option<LineId>, pub at: Option<Expr>,
                       pub count: Option<Expr>, pub optional: bool }
pub struct Part { pub name: String, pub anchor: PartAnchor, pub role: String, pub body: Vec<Item> }
pub enum PartAnchor { On, At }
// Dialogue gains:
pub kind: DialogueKind /* Conversation | Scene */, pub filter: Option<Expr> /* `for npc where` */,
pub exits: Vec<(String, Span)>, pub cast_cond: Option<Expr> /* scene `where` */, pub uninterruptible: bool
// Line gains: pub ticks: Option<Expr>
// StepKind gains / changes:
Detour(String, Vec<ExitArm>)                   // arms empty = plain detour
Return(Option<String>)
Pause(Expr)
pub struct ExitArm { pub exit: String, pub steps: Vec<Step>, pub span: Span }
```

Contextual words exactly as spec §5.13 (folded). `scene` and `repeatable` stay **contextual** (not reserved):
`let scene = 1` must keep parsing. New tokens `Tok::{Part,Scene,Requires,Pause,Optional,Repeatable,Exits,Count,
Uninterruptible,On}`. `-> X when c` is a `Transition` only as a direct stage member; inside a handler `-> X` stays
`StmtKind::Jump`.

Tests (`parse_items.rs`, s-expression dumps): `quest_header_forms` (`repeatable quest B(cooldown=72000,
exclusive=true) at camp:` → `(quest B repeatable (cooldown 72000) (exclusive true) at camp …)`);
`objectives_named_counted_optional`; `transition_is_a_stage_member`; `part_on_and_at`; `scene_with_where_pause_ticks`
(`(ticks=60)` on a line, `pause 30`); `dialogue_exits_and_detour_arms`; `return_exit`; `for_npc_where`;
`contextual_words_stay_identifiers` (`let scene = 1`, `let part = 2`, `fn pause(x)`, `count = 3` parse as before);
`quest_examples_doc_parses` — every python block of `2026-11-16-quest-examples.md` (fixtures `QX1`…`QX6`, the §6
fragments included) parses with no diagnostics; `dialogue_grammar_has_no_conflicts` still passes (LALRPOP build).
`lex.rs`: `phase10_contextual_words`.

- [ ] **Step 1:** tests (copy the examples verbatim into fixtures; the existing `EX*` fixtures updated to the folded
  syntax where the fold changed it — objectives become named: `objective find "…"`).
- [ ] **Step 2:** `just quest-test "-p quest-lang --test parse_items --test lex"` → FAIL.
- [ ] **Step 3:** grammar, layout pass, AST, dump; `resolve` gets only the edits needed to compile (new variants
  pushed to `deferred` as today → still `not-yet`).
- [ ] **Step 4:** PASS; `just quest-test "-p quest-lang"`; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): the grammar of quests, parts, scenes, exits and objectives`.

#### Task 37: Host description — enums, properties, facts; the stub's quest API

**Files:** Modify `quest-vm/src/host.rs`, `quest-lang/src/{resolve/*,infer/call.rs}`, `quest-stub/src/api.rs`;
Test `quest-lang/tests/check_host.rs` (new), `quest-stub/tests/stub.rs`.

**Interfaces — Produces:**

```rust
pub struct HostApi { …, pub enums: Vec<HostEnum> }        // new field, after `structs`
pub struct HostEnum { pub name: String, pub variants: Vec<String> }   // data-less variants only
pub struct HostFn { …, pub property: bool, pub fact: bool }
pub enum HostTy { …, Enum(String) }
```

- `property: true` (a receiver `Query` with no params) is read as `x.name`, no parentheses; calling it with
  parentheses is `not-callable`, and reading a non-property method without them is `missing-call`.
- `fact: true` marks a query the coordinator can evaluate: only facts are allowed in the query subset (Task 39).
- Host enum values are `WireValue::Variant { name: "Occupation::Smith", fields: [] }` and type as a VM enum.
- The stub's API (version 2) adds the spec §9.1 list (folded): structs `Person{id:Int}`, `Creature{id:Int}`,
  `Site{id:Int}`, `Place{id:Int}`, `Prefab{name:Str}`, `Timer{id:Int}`; enums `Occupation{Farmer, Miller, Smith,
  Guard, Elder, Miner, Merchant, Innkeeper, None}`, `SiteKind{Village, Town, Mine, Camp}`; facts `act()`,
  `loop()`, `completed(def: Str)`, `near(a: Value, b: Value, regions: Int, blocks: Int)` (named args, defaults
  0), properties `alive`, `occupation`, `site`, `owner`, `kind`, `name` on `Person` / `Creature` (and `Player`
  keeps its fields), `has_tag(t)` (fact), `site_kind(s)` (fact), `Place.open_ground(blocks)` (fact); pure
  queries `Player.skill(name)`, `Player.inventory_count(kind)`, `ours(e: Value)`, `prefab(name)`; commands
  `take(p, kind, n)`, `follow(c, p)`, `place(prefab, at)` (raises `NoRoom`; a fallible command is allowed from
  this task — remove that `not-yet`), `tag(e, t)`; events `PlayerNear{player}`, `ItemGiven{player, to: Value,
  item}`, `Killed{victim: Value, by: Option<Player>}`, `PlayerDefeated{player}`, `BlockUsed{player, tag: Str}`,
  `Setup{}`; dynamic attachment commands `attach_dialogue(npc: Person, d: Dialogue)`, `detach_dialogue(npc, d)`,
  `bark(npc, d)`; and, in the namespace `quest` (a reserved word, so no script can call them by name), the host
  side of the quest builtins Task 44 lowers to: commands `quest.joined(p)`, `quest.left(p)`,
  `quest.join_refused(p)`, `quest.complete(ps: list[Player])`, `quest.fail()`, `quest.decline()`,
  `quest.play(scene: Dialogue, cast: list[Value])`, the fact `quest.completed_by(p)` and the query
  `quest.dialogue_depth()`. (`Entered`, `RoleLost`, `Talked`, `DialogueEnded`, `SceneEnded`, `EndReason`, `SceneEnd`, `Exit`,
  `Stage`, `Role` and the quest builtins are the compiler's own, Tasks 38/44.)

Tests: `host_enum_values_and_match` (exhaustive `match o: case Occupation::Smith … case _`), `property_read`,
`property_called_is_error`, `method_without_call_is_error`, `fallible_command_handled` (`place(...) else:`);
stub: `stub_api_v2_is_well_formed` (no duplicate names; every `HostTy::Struct/Enum` named exists).

- [ ] Steps 1–5 as usual; commit `feat(quest): host enums, properties and facts; the stub's quest API`.

#### Task 37b: Fallible host queries (added by this plan — gate G4)

**Files:** Modify `quest-vm/src/{host.rs,interp.rs,bytecode.rs}`, `quest-lang/src/{infer/call.rs,codegen/handlers.rs}`,
`quest-stub/src/{api.rs,host.rs}`; Test `quest-lang/tests/check_host.rs`, `quest-vm/tests/interp.rs`.

**Why here:** spec §9.1 — "a query about an entity outside the instance's reach gives `None` or the error
`OutOfReach`". Task 37 lifts the `not-yet` for fallible commands; this lifts it for queries, the last `not-yet`
of the host interface.

**Interfaces — Produces:**

```rust
pub enum QueryError { Raised(WireValue /* a Variant naming a declared error */), Failed(String /* host bug: a fault */) }
pub trait Host { fn query(&mut self, f: u16, args: &[WireValue]) -> Result<WireValue, QueryError>; … }
```

- A `HostFn` of kind `Query` with a non-empty `raises` is a fallible call like any fallible `fn`: `?`, `else`,
  `match`, an exact inferred set. `Raised` of an error the function does not declare is the fault `HostError`.
- A fallible query is **not pure-safe by default**: inside a synthesised pure fn an escaping error is already
  `impure-condition` (Task 39); a handled one (`q() else false`) is fine.
- The stub gains one fallible query for the tests and the corpus: `Person.distance_to(other: Value) -> Int raises
  OutOfReach` (fails when either entity is not in `people.txt`).

Tests: `fallible_query_must_be_handled` (`unhandled-fallible`), `fallible_query_else_and_match`,
`query_raising_undeclared_error_faults`; VM: `query_error_takes_the_error_path_and_costs_fixed_fuel`.

- [ ] Steps 1–5 as usual (re-bless for the instruction change); commit `feat(quest): fallible host queries`.

#### Task 38: HIR and resolution of dialogues, scenes, quests and parts

**Files:** Create `quest-lang/src/hir/dialogue.rs`, `quest-lang/src/resolve/quest.rs`; Modify
`hir.rs`, `resolve/{mod.rs,dialogue.rs,items.rs}`, `lib.rs` (drop the `not-yet` loop); Test
`quest-lang/tests/resolve.rs` (replace `not_yet_until_phase_10` with the tests below).

**Interfaces — Produces (`hir`):**

```rust
DefKind::Dialogue(Dialogue)                 // was a unit variant; scenes too
DefKind::Quest(Quest)
DefKind::Fn(Fn) with Fn.pure: Option<PureSite>   // set on synthesised fns
pub enum PureSite { Cond, Subst, EmitArg, Target, Filter, CastCond, Where(RoleIdx), Requires, Transition(StageIdx) }
pub struct Dialogue { pub kind: DialogueKind, pub quest: Option<DefId>, pub stage: Option<StageIdx>,
  pub cast: Vec<LocalId> /* p, npc | the scene's params */, pub target: Option<DefId /*fn*/>,
  pub filter: Option<DefId>, pub topic: Option<Topic>, pub exits: Vec<String> /* [0] == "done" */,
  pub pick: Pick, pub uninterruptible: bool, pub cast_cond: Option<DefId>, pub nodes: Vec<Node>, pub stateless: bool }
// Node / Step mirror the AST, with every Expr replaced by the DefId of a synthesised pure fn,
// `Jump` by a node-group index, `Detour` by (DefId of the callee dialogue, arms by exit index),
// `Emit` by (EventRef, arg fns), `Return` by exit index, `Line.text` by (LineId, Vec<subst fn>)
pub struct Quest { pub name: String, pub repeatable: Option<RepeatOpts>, pub home: RoleIdx, pub roles: Vec<RoleDef>,
  pub requires: Vec<DefId>, pub stages: Vec<StageDef>, pub parts: Vec<PartDef>,
  pub globals: Vec<DefId> /* quest lets */, pub events: Vec<DefId> }
pub struct StageDef { pub name: String, pub objectives: Vec<ObjectiveDef>, pub transitions: Vec<(StageIdx, DefId)>,
  pub dialogues: Vec<DefId>, pub handlers: Vec<DefId> }
pub struct ObjectiveDef { pub name: String, pub text: LineRef, pub at: Option<DefId>, pub count: Option<i64>, pub optional: bool }
pub struct PartDef { pub name: String, pub anchor: PartAnchor, pub role: RoleIdx, pub globals: Vec<DefId>, pub handlers: Vec<DefId> }
pub struct RoleDef { pub name: String, pub ty: Type, pub optional: bool, pub cond: Option<DefId> }
```

**Synthesis rule:** each condition-like expression becomes a `hir::Fn` named
`<dialogue qualified name>$<site>#<n>` (`n` = source order in the dialogue, stable), with parameters = the cast
(`p: Player, npc: Person` for a conversation; the scene's params; none for `requires`/transitions; the role
itself plus earlier roles for a `where`), return type `bool` for conditions, `str` for substitutions, the field's
type for an emit argument, `Person`/`Option[Person]` for a target. Names in scope inside it: the cast, roles
(`Option[T]` for optional ones), quest and top-level globals, `stage`, `participants`, objectives,
`dialogue_depth()`. Synthesised fns enter `layout::plan` like any `Fn` (they get fn ids).

**Generated names (quest scope):** `Stage` (enum of stages, source order), `Role` (enum of roles),
`stage` (global, type `Stage`), `participants` (global `list[Player]`), each objective (global of the built-in
struct `Objective { state: int, count: int }`, qualified `Q::<Stage>.<name>`), each role (read-only global
`Q::role.<name>`), each exit (`<dialogue>::<exit>`, type `Exit`), the events `Entered { stage: Stage }` and
`RoleLost { role: Role }`; compiler-known host-shaped events everywhere: `Talked { player, npc: Person }`,
`DialogueEnded { player, npc: Person, dialogue: Dialogue, reason: EndReason, exit: Exit }`,
`SceneEnded { scene: Dialogue, reason: SceneEnd }`. `Dialogue` and `Exit` are opaque types whose wire value is
the qualified name as a `Str` (`"confront"`, `"confront::paid"`); they support only `==` / `!=`. Enums `EndReason { Finished, Left, Timeout, TooDeep, Edited,
OutOfRange }`, `SceneEnd { Finished, Interrupted, ActorLost }`.

**Structural diagnostics here:** `duplicate-def` (an objective shadowing a role, a let or another objective; two
stages, parts or exits of one name); `entered-outside-stage`; `unknown-exit` (`return x` / an arm `x` not
declared by that dialogue); `scene-choice`; `bark-choice` (kept); `scene-follow-up` (`->` to a scene whose cast is
longer, or `->` not the last step of a scene); `dialogue-signature` (a scene parameter not `Person`); `bad-line-text`
(line text not a string or f-string literal); `missing-line-id` (warning; the provisional id is
`<dialogue>.<n>`); `bad-part-item` (a stage, objective, dialogue, scene or part inside a part);
`bad-quest-option` (an unknown option, a non-constant value, any option without `repeatable`). A `where` naming a
later role is left to Task 39 (`bad-where`); resolution only records the role order.
`decline()` outside a `Setup` handler is `decline-outside-setup`.

Tests (`resolve.rs`): `quest_names_resolve` (Stage, Role, stage, participants, objectives, `Stage.obj`, roles,
exits); `synthesised_fns_are_named_stably` (`signatures()` lists `farmer_talk$cond#0` … identical across two
compiles with module files reordered); `objective_shadowing_role_is_duplicate_def`; `entered_outside_stage`;
`unknown_exit_in_return_and_arm`; `scene_choice_and_follow_up`; `bad_line_text`; `missing_line_id_is_a_warning`;
`bad_part_item`; `bad_quest_option`; `decline_outside_setup`.

- [ ] Steps 1–5; commit `feat(quest): resolve dialogues, scenes, quests and parts into HIR`.

#### Task 39: Types and the Phase 10 checks

**Files:** Create `quest-lang/src/infer/dialogue.rs`, `quest-lang/src/{purity.rs,query.rs,readset.rs}`; Modify
`infer/mod.rs`, `callgraph.rs`, `lib.rs`; Test `quest-lang/tests/check_dialogue.rs`.

**Interfaces — Produces:**

```rust
// Typed gains:
pub read_sets: ReadSets
pub struct ReadSets { pub dialogue: BTreeSet<DefId /*global*/>, pub parts: Vec<BTreeSet<DefId>> }
pub fn purity(krate: &hir::Crate, typed: &Typed, host: &HostApi) -> Vec<Diagnostic>;       // purity.rs
pub fn query_subset(krate: &hir::Crate, typed: &Typed, host: &HostApi) -> Vec<Diagnostic>; // query.rs
pub fn read_sets(krate: &hir::Crate, typed: &Typed) -> (ReadSets, Vec<Diagnostic> /* mirror-size */); // readset.rs
```

- **Types:** a condition is `bool` (`type-mismatch`), a substitution anything `str()` accepts, an `emit` argument
  matches its field, a `choice(timeout=…)` / `pause` / `(ticks=…)` / `count` an `int` constant expression
  (`not-constant`), a target `Person` or `Option[Person]`, a scene's cast `Person`.
  Objective methods: `finish()`, `fail()`, `reset()` → unit, `progress(n: int)` → unit (`bad-objective-call` on
  an objective without `count`), reads `done`, `failed` (bool), `count` (int). Builtins: `join(p: Player)`,
  `leave(p: Player)`, `complete()`, `complete(players: list[Player])`, `fail()`, `decline()`,
  `after(ticks: int, e: <event>) -> Timer`, `cancel(t: Timer)`, `play(scene: Dialogue, …cast by name)`.
- **`role-assign`:** assigning a role, `stage`, `participants` or an objective directly (only methods and
  builtins change them).
- **`part-write`:** code reachable only from a part's handlers that assigns a quest global, calls an objective
  command, `join`, `leave`, `complete`, `fail` or `-> Stage`.
- **Purity (`impure-condition`)**, over each synthesised fn and transitively every `fn` it calls (via the call
  graph): no assignment except to its own locals, no host `Command`/`Promise` call, no quest builtin, no `emit`,
  no `await`, a non-empty inferred error set (an escaping error). The message names the offending call chain.
- **Stateless (`stateless-dialogue`):** a dialogue with `for npc where` or a `scene`, declared **outside any
  quest**, is package-level content: its synthesised fns may read no script global (top-level `let`) and it may
  not `emit`. (Fold-time call: a site script's own top-level dialogue with a plain `for` stays stateful, as spec
  §5.11's example.)
- **Query subset (`bad-where`)** on a role's `where` and `requires`, spec §5.12 verbatim: atoms of facts (host
  fns/properties with `fact: true`) compared with literals/enum values, `in [..]`, tag/kind tests, role
  relations, `not atom`, an `or` of atoms about one role. Refused: a script `fn`, a script global, a non-fact
  query, arithmetic beyond comparing with a constant, a later role.
- **Scores:** each node / group member / candidate gets its conjunct count (spec §5.11 scoring); stored on the
  HIR node for codegen.
- **Read sets:** the dialogue read set = every global read by any dialogue/scene/transition synthesised fn and
  everything it calls; each part's read set = quest globals read by that part's handlers and what they call.
  `mirror-size` warning when the static word size of a read set exceeds `MIRROR_WORDS_WARN` (a list, dict or
  string counts as `MIRROR_WORDS_WARN` alone).

Tests (`check_dialogue.rs`): `condition_must_be_bool`; `emit_arg_types`; `timeout_must_be_constant_int`;
`impure_condition_direct` (`give` in a guard); `impure_condition_transitive` (a guard calls `fn a` calls `fn b`
that assigns a global — message lists `a → b`); `impure_condition_escaping_error`; `pure_fn_with_locals_is_fine`;
`stateless_dialogue_reads_global`, `stateless_dialogue_emits`; `bad_where_script_fn`, `bad_where_global`,
`bad_where_non_fact`, `bad_where_later_role`, `where_subset_accepts_all_atoms` (every atom form of spec §5.12);
`role_assign`; `part_write_assign`, `part_write_via_called_fn`, `part_reads_are_fine`;
`objective_progress_without_count`; `scores_count_conjuncts` (`a and b` → 2, `a or b` → 1, `once per p` → 1,
`not (a and b)` → 1, none → 0); `read_set_is_transitive_and_minimal` (a global only a handler reads is absent);
`mirror_size_warns_on_list`.

- [ ] Steps 1–5; commit `feat(quest): type and check dialogue, scene and quest code — purity, query subset, read sets`.

---

### Phase 10b — VM

#### Task 40: Script events, timers, roles and instance owners

**Files:** Create `quest-vm/src/timers.rs`; Modify `program.rs`, `bytecode.rs`, `asm.rs`, `sched.rs`,
`interp.rs`, `vm.rs`, `save.rs`, `migrate.rs`, `consts.rs`; Test `quest-vm/tests/{sched.rs,program.rs}`.

**Interfaces — Produces:**

```rust
// Program gains (written after `handlers` in to_bytes/from_bytes, validated, BYTECODE_VERSION 1 → 2):
pub events: Vec<EventInfo>                // script events: { name: String, ty: TypeId }
pub quest: Option<QuestInfo>
pub struct QuestInfo { pub name: String, pub roles: Vec<RoleInfo>, pub stages: Vec<String>,
  pub stage_global: u16, pub participants_global: u16, pub home_role: u16,
  pub repeat: Option<RepeatInfo>, pub parts: Vec<PartInfo>, pub requires: Vec<u16 /*fn*/>,
  pub start_fn: u16 /* enters the first stage */, pub read_set: Vec<u16 /*global*/> }
pub struct RoleInfo { pub name: String, pub global: u16, pub ty: TypeId, pub optional: bool, pub cond: Option<u16> }
pub struct RepeatInfo { pub cooldown: u64, pub exclusive: bool, pub once_per_player: bool }
pub struct PartInfo { pub name: String, pub anchor: Anchor, pub role: u16, pub read_set: Vec<u16> }
pub enum Anchor { On, At }
// HandlerInfo gains: pub owner: u16 /* 0 = home or plain program, 1 + part index */, pub stage: Option<u16>
// VmConfig gains: pub instance: u16 /* owner selected */, pub roles: Vec<(String, WireValue)>
// Instructions (each with a fuel cost, pinned by test): Enqueue { event: u16 } (pops the event value),
// After { event: u16 } (pops ticks, value; pushes the timer id), CancelTimer, CancelStageTimers
pub enum LoadError { …, Role(String /* missing | wrong type: names the role */) }
impl Vm { pub fn timers(&self) -> Vec<(u64 /*due*/, u64 /*id*/, String /*event*/)>; }
```

- `deliver` accepts host events and the program's script events; only handlers of `VmConfig.instance` register.
- `Enqueue` queues to the instance's own queue (same overflow rules as host events).
- Timers live in VM memory (hashed, undone, saved, migrated, cleared by `reset`), at most `TIMERS_MAX`; `tick(now)`
  delivers every timer with `due <= now` in (due, id) order before running the queue. A timer created by a task
  whose handler has `stage: Some(s)` is stage-scoped; `CancelStageTimers` cancels those. Timer ids never repeat
  within an epoch.
- `Vm::new` writes each role value into its global before the initialisers (wire → value with `from_wire`;
  `fits` against `ty`, an optional role as `Option`); a missing required role or a misfit is `LoadError::Role`.

Tests: `script_event_is_delivered`; `enqueue_overflows_like_host_events`; `only_owner_handlers_run` (a part-owned
handler is silent in the home VM and vice versa); `timer_fires_at_due_tick`; `timers_fire_in_due_then_id_order`;
`stage_timer_cancelled`; `timer_survives_save_load_and_is_hashed`; `timer_undone_with_its_segment` (a segment
that sets a timer then faults leaves none); `too_many_timers_faults`; `reset_clears_timers`;
`role_values_installed`, `missing_role_refused`, `misfit_role_refused`; `program_v2_roundtrip`,
`hostile_quest_info_is_err` (a role global out of range, a part role out of range, a stage index out of range —
each `Err`, no panic).

- [ ] Steps 1–5 (re-bless goldens for the version bump); commit `feat(quest-vm): script events, timers, roles, instance owners`.

#### Task 41: Pure evaluation and mirrors

**Files:** Create `quest-vm/src/pure.rs`; Modify `vm.rs`, `interp.rs`, `lib.rs`; Test `quest-vm/tests/pure.rs`.

**Interfaces — Produces:**

```rust
pub enum PureFault { Fault(FaultKind), OutOfFuel, Impure, Raised(String), BadCall }
impl Vm {
    /// Runs fn `f` with `args` (wire values fitted to its params) under `fuel`; no task, no segment, no queue.
    /// The VM's state (memory, hash) afterwards equals before: it runs on a checkpoint and rolls back.
    /// A command, request, Enqueue or After is `Impure`; an escaping error `Raised`; a bad fn id/arity `BadCall`.
    pub fn eval_pure(&mut self, f: u16, args: &[WireValue], fuel: u64, host: &mut dyn Host)
        -> Result<WireValue, PureFault>;
    pub fn mirror(&self, set: &[u16]) -> Mirror;          // the globals of `set` as wire values
    pub fn install_mirror(&mut self, m: &Mirror) -> Result<(), LoadError>;  // part VMs: overwrite those globals
}
#[derive(Clone, Debug, PartialEq)] pub struct Mirror { pub globals: BTreeMap<u16, WireValue> }
impl StateHash for Mirror { … }
impl Mirror {
    /// Evaluate against the mirror: a scratch VM of `program` with the mirror installed.
    pub fn eval_pure(&self, program: &Arc<Program>, f: u16, args: &[WireValue], fuel: u64, host: &mut dyn Host)
        -> Result<WireValue, PureFault>;
}
```

`install_mirror` writes through the undo log (a part's rollback undoes it). Callers compare
`state_hash(&mirror)` to decide whether to resend; there is no change tracking in the VM.

Tests: `eval_pure_returns_value`; `eval_pure_out_of_fuel_is_err_and_state_unchanged` (`hash64()` and
`mem_bytes()` equal before/after; Review Focus 2); `eval_pure_fault_is_err`; `eval_pure_impure_is_refused`
(hand-assembled fn issuing a command); `mirror_eval_equals_live_eval` (same answers for 50 generated states);
`mirror_holds_only_its_set`; `install_mirror_is_undone_by_rollback`; `mirror_hash_ignores_order`.

- [ ] Steps 1–5; commit `feat(quest-vm): eval_pure and mirrors`.

#### Task 42: The dialogue graph format

**Files:** Create `quest-vm/src/dialogue.rs`; Modify `program.rs` (field `dialogues: DialogueGraph`, after
`quest`), `asm.rs`, `bytecode.rs` (version 2 → 3), `consts.rs`; Test `quest-vm/tests/dialogue_format.rs`.

**Interfaces — Produces:**

```rust
pub struct DialogueGraph { pub dialogues: Vec<DialogueDef>, pub steps: Vec<StepDef> /* all, flat */ }
pub struct DialogueDef { pub name: String, pub kind: DialogueKind, pub cast: u8, pub stage: Option<u16>,
  pub target: Option<u16 /*fn*/>, pub filter: Option<u16>, pub cast_cond: Option<u16>, pub topic: Option<TopicDef>,
  pub exits: Vec<String> /* [0] = "done" */, pub pick: Pick, pub stateless: bool, pub interruptible: bool,
  pub nodes: Vec<NodeDef>, pub score: u8 /* scene candidate */ }
pub enum DialogueKind { Conversation, Scene }           // a bark is Scene with cast 1
pub enum Pick { First, Best, BestLeastRecent, RandomBestLeastRecent }
pub struct TopicDef { pub text: LineDef, pub cond: Option<u16>, pub priority: i64 }
pub struct NodeDef { pub name: String, pub cond: CondDef, pub score: u8, pub body: Block }
pub struct CondDef { pub once: Option<OnceDef>, pub guard: Option<u16> }
pub struct OnceDef { pub site: u32, pub per: Option<u16 /*fn*/> }
pub struct Block { pub first: u32, pub len: u32 }      // a range of `steps`
pub struct LineDef { pub id: String, pub speaker: Speaker, pub subst: Vec<u16>, pub ticks: u32 }
pub enum Speaker { Cast(u8), Fn(u16) }
pub struct MemberDef { pub line: LineDef, pub cond: CondDef, pub score: u8, pub body: Block }
pub struct OptionDef { pub text: LineDef, pub guard: Option<u16>, pub once: Option<u32 /*site*/>, pub default: bool, pub body: Block }
pub enum StepDef {
  Line { line: LineDef, cond: CondDef }, Group { site: u32, members: Vec<MemberDef> },
  Choice { timeout: Option<u32>, options: Vec<OptionDef>, fallback: Option<Block> },
  When { cond: CondDef, body: Block }, Once { once: OnceDef, body: Block },
  Jump { group: u16 /* index of the first node of the group */ }, Detour { dialogue: u16, arms: Vec<(u16, Block)> },
  Emit { event: u16 /* Program.events or host event index, see `script` */, script: bool, args: Vec<u16> },
  Return { exit: u16 }, Pause { ticks: u32 }, FollowUp { scene: u16 },
}
pub fn line_ticks(source_chars: u32) -> u32;   // LINE_BASE_TICKS + chars × LINE_TICKS_PER_CHAR, clamped
```

Validation (in `Program::validate`): every fn id exists and is a synthesised pure fn of the arity its site
needs (cast length, or +0 for `once per`); every `Block` lies inside `steps`, blocks are properly nested and no
step is shared between two blocks; `Jump`/`Detour`/`FollowUp`/exit indices in range; a scene has no `Choice`;
`steps.len() <= GRAPH_STEPS_MAX`; `cast` 1..=`STRUCT_FIELDS_MAX`. Hostile bytes → `Err`, never a panic.
`StateHash` for every type.

Tests: `graph_roundtrip`; `line_ticks_clamps` (0 chars → 30, 50 → 80, 1000 → 200); `hostile_graph_is_err`
(proptest: mutate one byte of a valid program 2000 times — `from_bytes` returns `Ok` or `Err`, never panics);
`overlapping_blocks_refused`; `scene_with_choice_refused`; `wrong_arity_condition_refused`.

- [ ] Steps 1–5 (re-bless); commit `feat(quest-vm): the dialogue graph format`.

---

### Phase 10c — Lowering

#### Task 43: Codegen — dialogues and scenes

**Files:** Create `quest-lang/src/codegen/dialogue.rs`; Modify `codegen/mod.rs`, `codegen/handlers.rs` (script
`emit` → `Enqueue`), `lib.rs`; Test `quest-lang/tests/run_dialogue.rs`.

**Interfaces — Produces:**

```rust
pub struct Compiled { pub program: Program, pub warnings: Vec<Diagnostic>,
                      pub lines: BTreeMap<String /*line id*/, String /*source text*/> }
```

- Each HIR dialogue/scene → one `DialogueDef` (program order = qualified-name order, so module order cannot
  change it); its synthesised fns are ordinary fns already in the layout.
- Line text → `Compiled.lines` keyed by id (provisional ids included); f-string holes appear as `{0}`, `{1}` in
  the stored text, matching `LineDef.subst` order. `ticks` = `(ticks=n)` or `line_ticks(chars)`.
- `emit Ev(...)` **in a handler** of a script event now compiles to `Enqueue` (the host `emit(e)` prelude stays
  for host events).
- `dialogue_depth()` lowers to the host query `quest.dialogue_depth()`; `DialogueEnv::eval` receives the depth and
  the stub answers it from there.
- `not-yet` is gone for top-level dialogues and scenes.

Tests (`run_dialogue.rs`, inspecting `Program.dialogues` and `Compiled.lines` directly — no runner yet): `farmer_graph_shape`
(spec §5.11 farmer: 1 node, steps Line, Choice(4 options, the third `once`), Line; option 2 jumps to `start`);
`lines_table_has_text_and_holes` (`@farmer.dog_desc` → `"Brown, one ear up. He answers to {0}."`);
`graph_independent_of_module_order`; `scene_line_ticks_from_source` (`(ticks=60)` → 60; `"..."` → 33);
`script_emit_in_handler_enqueues`.

- [ ] Steps 1–5; commit `feat(quest): lower dialogues and scenes to the graph`.

#### Task 44: Codegen — quests and parts

**Files:** Create `quest-lang/src/codegen/quest.rs`; Modify `codegen/{mod.rs,handlers.rs,stmt.rs}`,
`layout.rs`; Test `quest-lang/tests/run_quest.rs`.

**Interfaces — Consumes:** Task 40's `QuestInfo`, instructions; Task 38's HIR `Quest`.

Lowering (all names exact — they are migration keys):
- Globals `Q::stage`, `Q::participants`, `Q::<Stage>.<obj>` (`Objective { state, count }`, state 0 open, 1 done,
  2 failed), `Q::role.<name>`, quest `let`s as `Q::<name>`, part `let`s as `Q::<part>::<name>`.
- Fns `$enter::Q::<Stage>` (set `stage`; re-open that stage's objectives (state 0, count 0);
  `CancelStageTimers`; `Enqueue Entered { stage }`), `$start::Q` (= enter the first stage; runs after the
  initialisers, before `main`, in the home instance only), `$transitions::Q` (evaluates the current stage's
  transition fns in source order, enters the first that holds).
- `-> Stage` in a handler = call `$enter` then return. `$transitions` is called at **every segment end of a home
  handler**: before each `await` and at every return (plain and error-free). Stage handlers get the prologue
  `if stage != S: return` and `HandlerInfo.stage = Some(S)`; part handlers `owner = 1 + part`.
- Objective methods inline: `finish` (open → done), `fail` (open → failed), `progress(n)` (open: `count += n`;
  `count >= target` → done), `reset` (state 0, count 0).
- `join(p)`: `exclusive` with participants and `p` not in them → `quest.join_refused(p)`; with `once_per_player`
  and `quest.completed_by(p)` → `quest.join_refused(p)`; else append if absent and `quest.joined(p)`. `leave(p)`:
  remove, `quest.left(p)`. `complete()` / `complete(ps)`: `quest.complete(ps)` (all participants when no
  argument). `fail()` → `quest.fail()`, `decline()` → `quest.decline()`. `after` → `After`, `cancel` →
  `CancelTimer`, `play(scene, a=…, …)` → `quest.play(scene, [cast in parameter order])`.
- `RoleLost` with no user handler: a generated handler that calls `fail()` unless the role is optional.
- `QuestInfo` filled from all of it; `read_set` from Task 39.
- Edits: the stage migrates by name like any global; when its stage no longer exists the migration
  re-initialises `Q::stage` (so the quest restarts at the first stage) and reports `initialised: Q::stage`.

Tests (`run_quest.rs`, through `quest_stub::Runner` with roles given): `starts_in_first_stage_with_entered`;
`handler_jump_enters_stage`; `transition_when_both_objectives_done` (spec §5.12: `find_dog.finish()` alone stays,
both → `Returning`); `two_finishes_one_entry` (Review Focus 5); `reentering_reopens_objectives`;
`stage_timer_cancelled_on_change` (Review Focus 5); `quest_body_timer_survives_stage_change`; `progress_counts`;
`stage_handlers_only_in_their_stage`; `join_exclusive_refused`; `role_lost_default_fails_required_only`;
`edit_removing_current_stage_falls_back_to_first` (Review Focus 3); `part_handlers_compiled_with_owner`.

- [ ] Steps 1–5; commit `feat(quest): lower quests — stages, objectives, transitions, timers, participants, parts`.

---

### Phase 10d — The runner

#### Task 45: Conversations

**Files:** Create `quest-vm/src/dialogue_run.rs`; Modify `lib.rs`; Test `quest-lang/tests/conversation.rs` (it
compiles `.qs` fixtures and has both crates; `quest-vm` never depends on `quest-lang`).

**Interfaces — Produces:**

```rust
pub struct InstanceId(pub u32);
pub struct Attachment { pub instance: InstanceId, pub dialogue: u16 }
pub trait DialogueEnv {
    fn graph(&self, i: InstanceId) -> Option<&DialogueGraph>;
    fn eval(&mut self, i: InstanceId, f: u16, args: &[WireValue], depth: u32) -> Result<WireValue, PureFault>;
    fn report(&mut self, i: InstanceId, fault: PureFault);          // once per fault site per conversation
}
#[derive(Default)] pub struct Records { /* per NPC and attaching instance: once sites, pick counts, visited */ }
pub struct Conversation { /* player, npc, stack of frames {instance, dialogue, step, arms}, pending offer, state */ }
pub enum ConvState { Running, AwaitingPick, Paused { until: u64 }, Ended { linger_until: u64 }, Closed }
pub struct Batch { pub lines: Vec<SaidLine>, pub offer: Option<Offer>, pub emits: Vec<Emitted>, pub ended: Option<Ended> }
pub struct SaidLine { pub instance: InstanceId, pub id: String, pub speaker: WireValue, pub args: Vec<WireValue> }
pub struct Offer { pub menu: bool, pub options: Vec<SaidLine>, pub timeout_at: Option<u64>, pub serial: u32 }
pub struct Emitted { pub instance: InstanceId, pub event: String, pub value: WireValue }
pub struct Ended { pub reason: EndReason, pub dialogue: String, pub exit: String }
pub enum PickRefused { NoOffer, Stale { resent: Offer }, OutOfRange { resent: Offer } }
impl Conversation {
    pub fn start(env: &mut dyn DialogueEnv, attached: &[Attachment], rec: &mut Records,
                 player: WireValue, npc: WireValue, now: u64) -> (Conversation, Batch);
    pub fn pick(&mut self, env: &mut dyn DialogueEnv, attached: &[Attachment], rec: &mut Records,
                serial: u32, option: u32, now: u64) -> Result<Batch, PickRefused>;
    pub fn tick(&mut self, env: &mut dyn DialogueEnv, rec: &mut Records, now: u64) -> Option<Batch>; // pause, timeout, linger
    pub fn leave(&mut self, now: u64) -> Batch;
    pub fn done(&mut self);                       // the client's Done: Ended → Closed
    pub fn rebind(&mut self, env: &mut dyn DialogueEnv) -> Option<Batch>;   // after an edit: Edited if its node is gone
    pub fn state(&self) -> &ConvState;
}
impl StateHash for Conversation, Records, Batch …
```

Behaviour, spec §5.11 verbatim: the greeting pool (entry node groups of every attached greeting, filter
dialogues already resolved into `attached` by the caller) picked by score → least recent → stable key
(qualified name, node position); then the topic menu (topics whose `if` holds, priority desc, then qualified
name); a topic runs as a detour and returns to a **re-evaluated** menu; batching to the next choice / end /
pause; `once` / `once per`; groups by `@pick`; `detour` with arms; exits; `DIALOGUE_STACK_MAX` → `TooDeep`;
timeout → the `default` option or the hidden `default:` block, else `Timeout`; `leave` → `Left`; empty group →
says nothing; end → `Ended`, then `Closed` on `done()` or after `CONVERSATION_LINGER_TICKS`. A pick carries the
offer's `serial`; a pick against an older serial is `Stale` (the current offer re-sent). Guards evaluate with
`env.eval`; an `Err` counts as false and is reported. Random strategies roll with
`roll(now, hash(qualified name, site), salt)` (a stateless hash; the same function the stub uses).
Records are keyed by (instance, dialogue qualified name, site) so two scripts never disturb each other.

Tests (`quest-lang/tests/conversation.rs`, an env over a live `Vm`): `farmer_transcript` (spec §5.11: start → line
+ 4 options; pick "What does he look like?" → description with the dog's name, jump back, the `once` option still
offered once); `greeting_pool_by_score` (outlaw tag → "Keep walking."; first visit → "A new face!"; then the
group); `menu_sorted_by_priority_then_name`; `topic_returns_to_reevaluated_menu`; `detour_cycle_ends`;
`self_detour_bounded_by_depth`; `too_deep_ends`; `exit_arms` (haggle `agreed` / `refused`, `Ended.exit`);
`timeout_picks_default`, `timeout_runs_hidden_default`, `timeout_without_default_ends`; `pause_ends_batch`;
`linger_then_closed`; `faulting_guard_hides_option` (Review Focus 2); `stale_pick_is_refused_and_menu_resent`,
`out_of_range_pick_is_refused` (Review Focus 1); `emit_handler_changes_later_condition` (read-your-emit: visible
only after the VM ran the handler); `edit_removing_current_node_ends_edited` (Review Focus 3);
`moved_mid_conversation_same_transcript` (serialize `Conversation` + `Records` through `StateHash`-checked
clone into a fresh env, continue: identical batches and hashes); **`commutativity`** — three programs attach
greetings and topics to one NPC; all 6 attachment orders × 2 module orders give the same greeting, menu,
transcript and state hash.

- [ ] Steps 1–5; commit `feat(quest-vm): the conversation runner`.

#### Task 46: Scenes and barks

**Files:** Create `quest-vm/src/scene_run.rs`; Test `quest-lang/tests/scene.rs`.

**Interfaces — Produces:**

```rust
pub struct ScenePlay { /* instance, scene, cast, step cursor, next_at, follow-ups taken */ }
pub struct SceneOut { pub lines: Vec<SaidLine>, pub emits: Vec<Emitted>, pub ended: Option<SceneEnd> }
impl ScenePlay {
    pub fn start(env: &mut dyn DialogueEnv, i: InstanceId, scene: u16, cast: Vec<WireValue>, now: u64) -> ScenePlay;
    pub fn tick(&mut self, env: &mut dyn DialogueEnv, rec: &mut Records, now: u64) -> SceneOut;
    pub fn interrupt(&mut self) -> Option<SceneOut>;    // None when uninterruptible
    pub fn actor_lost(&mut self) -> SceneOut;
    pub fn interruptible(&self) -> bool;
}
/// Ambient casting: the best candidate by score → least recently played → stable key, its cast bound from
/// `idle` in order, where the scene's `where` holds. Deterministic given its inputs.
pub fn cast_ambient(env: &mut dyn DialogueEnv, candidates: &[Attachment], idle: &[WireValue],
                    played: &BTreeMap<String, u64>, now: u64) -> Option<(Attachment, Vec<WireValue>)>;
```

A line is said at its step's tick and holds for its `ticks`; `pause n` waits; `=>` groups and `when` apply; a
`FollowUp` starts the next scene with the cast by position after the current one ends; a one-actor scene is a
bark (`bark(npc, d)` from a handler = `play` with one actor).

Tests: `argument_timeline` (Sealed Mine `argument`: line ticks from source text, `pause 30`, the `(ticks=60)`
"..."); `follow_up_keeps_cast`; `interrupt_ends_interruptible`; `uninterruptible_refuses_interrupt`;
`actor_lost_ends`; `bark_is_one_actor_scene`; `crier_group_by_act` (act 1 → the act-1 line wins);
`cast_ambient_deterministic` (same inputs, any order of `candidates` → same pick).

- [ ] Steps 1–5; commit `feat(quest-vm): scenes and barks`.

---

### Phase 10e — The stub world, the CLI, the corpus

#### Task 47: The stub world — instances, people, routing, talk

**Files:** Create `quest-stub/src/{people.rs,world.rs}`; Modify `api.rs`, `host.rs`, `runner.rs`, `lib.rs`; Test
`quest-stub/tests/world.rs` (+ `quest-lang/tests/run_world.rs` for compiled programs).

**Interfaces — Produces:**

```rust
/// `people.txt` of a case: one entity per line —
/// `person 3 name="Hal" occupation=Farmer site=1 alive tags=pet,witness owner=7`
/// `creature 7 name="Rufus" tags=pet owner=3` · `player 1 name="Ana" skill.trade=3 inv.meat=1`
/// `site 1 kind=Village` · `place 9 open_ground=30` · `fact act=1` · `fact completed=errantry/x::Q`
pub struct People { … }  pub fn parse_people(text: &str) -> Result<People, String>;
/// A scripted input line (`script.txt`, or the CLI): `<tick> talk <player> <npc>` · `<tick> pick <player> <n>`
/// · `<tick> leave <player>` · `<tick> done <player>` · `<tick> event <Name> <field>=<wire>… [@<part>]`
/// · `<tick> near <player> <entity>` · `<tick> kill <victim> [by <player>]` · `<tick> lose <entity>`
/// · `<tick> ambient <npc> <npc>…` · `<tick> edit <dir>`
pub enum Input { … }  pub fn parse_script(text: &str) -> Result<Vec<(u64, Input)>, String>;
pub struct World { /* programs, instances (home + parts, each a Runner), people, conversations per NPC,
                      scenes, records, mirrors, pending routed messages (applied at T+1), output log */ }
impl World {
    pub fn new(program: Arc<Program>, people: People, roles: Vec<(String, WireValue)>, cfg: StubConfig,
               fuel: u64) -> Result<World, String>;   // checks requires + each role's where (eval_pure) first
    pub fn step(&mut self, inputs: &[Input]) -> StepReport;   // one tick for every instance
    pub fn hash64(&self) -> u64;                       // StateHash over everything above
    pub fn quiescent(&self) -> bool;
}
```

- **Instances:** the home (owner 0) and one VM per part (`instance = 1 + i`), all from the one program. Every
  tick, in this order: apply last tick's routed messages (part `emit`s → home queue; home mirror → each part's
  `install_mirror`, sent only when its `state_hash` changed); deliver inputs; tick each instance in id order;
  run conversations and scenes; collect new emits and mirrors for T+1. Same-region emits (a conversation of an
  NPC whose dialogue belongs to the home) also land at T+1 — the stub models every instance as its own region.
- **Talk:** `talk` builds `attached` from the home's stage dialogues whose `for` target evaluates to that NPC,
  the program's top-level dialogues with that target, those attached by `attach_dialogue` (minus detached ones), and in-scope filter dialogues whose `where` holds; raises
  `Talked`; a second talker to a busy NPC gets nothing (private conversations). Batches print as
  `[t] Hal: <text with substitutions>` and `[t] menu: 1) … 2) …`; `DialogueEnded` is delivered on the end.
- **Host commands** of Task 44 (`quest.*`): `joined`/`left`/`join_refused` print; `complete(ps)` records completions and, for a
  repeatable quest, resets the home and parts (`Vm::reset`), reaps (prints `reaped`), re-checks roles, and
  delivers `Setup` after `cooldown`; `fail()` ends the instance (`failed` printed; no further delivery);
  `decline()` during `Setup` refuses the placement (`declined`, exit 2 in the CLI); `play` starts a scene;
  `place` succeeds unless the place has `open_ground=0` (`NoRoom`).
- `lose <entity>` raises `RoleLost` for the role bound to it.

Tests: `people_and_script_parse` (+ errors name the line); `role_of_wrong_type_is_refused`,
`role_failing_where_is_refused` (Review Focus 4); `part_emit_reaches_home_next_tick`;
`part_reads_home_state_one_tick_old`; `mirror_sent_only_on_change`; `busy_npc_ignores_second_talker`;
`repeatable_resets_after_cooldown_with_namespace_kept`; `fail_stops_delivery`; `world_hash_deterministic`.

- [ ] Steps 1–5; commit `feat(quest-stub): a multi-instance world with people, talk, scenes and parts`.

#### Task 48: The CLI

**Files:** Modify `quest-cli/src/main.rs`, `quest/README.md`; Test `quest-cli/tests/cli.rs`.

`quest run <dir>` gains: `--roles name=<wire>,…` (wire literal syntax: `Person(3)`, `None`), `--people FILE`
(default `<dir>/people.txt` if present), `--script FILE` (default `<dir>/script.txt` if present),
`--talk <player>:<npc>` (a `talk` at tick 1, then picks read from stdin one per offer: a number, `l` to leave),
`--picks 1,2,1` (the scripted form of `--talk`: answers each offer in turn, then `done`), `--lines` (print line ids
instead of text). A program with a quest uses the `World` runner; without, the current `Runner` (unchanged
output). Exit 2 for a refused role / `decline` / `fail`. README: commands table, the people and script formats,
the Phase 10 tunables; the "Known MVP limits" `not-yet` bullet for dialogue/quest removed.

Tests: `run_with_roles_and_picks` (Lost Dog, `--picks` → expected transcript); `talk_reads_stdin`;
`bad_roles_exit_2`; `script_file_default`.

- [ ] Steps 1–5; commit `feat(quest-cli): talk, picks, roles, people and scripts`.

#### Task 49: Corpus, goldens, close-out

**Files:** Create `quest/corpus/run/<case>/` and `quest/corpus/fail/<case>/` below; Modify
`quest-corpus/{build.rs,src/harness.rs}` (embed `people.txt` and `script.txt`; run quest cases through `World`);
`quest/README.md`; `CLAUDE.md` (the test-rule note: Phase 10 was initial implementation; the regression rule
applies again); `docs/superpowers/specs/2026-11-16-dialogue-examples.md` and `…-quest-examples.md` (mark each
block's corpus case); this plan's "As built".

Run cases (each `main.qs` + `expect.txt` + `golden.txt`, quests also `people.txt` + `script.txt`):
- `d_spec_farmer`, `d_spec_greeting` (spec §5.11); `q_spec_lostdog` (spec §5.12 snippet);
- `dx1_saliency` … `dx7_two_scripts` — the seven `2026-11-16-dialogue-examples.md` blocks, after applying the
  fold (scores by conjuncts; barks as written; named objectives in example 7);
- `qx1_lost_dog` (home + `dog_watch` part, two players, a bolt and a retry), `qx2_sealed_mine` (part `at mine`,
  the staged scene, counter objective), `qx3_bandit_camp` (four completions; the namespace counter survives the resets and
  the fourth `Setup` places the veteran camp), `qx4_debt` (exits from a library module standing in for `errantry/trade`),
  `qx5_town_life` (stateless filter dialogue, ambient cast scene with follow-up, the crier);
- `d_detour_cycle`, `d_too_deep`, `d_fault_is_false`, `d_read_your_emit`, `d_edit_ends_node`,
  `d_commutative` (three modules attaching to one NPC; the harness also runs every permutation of module order
  and asserts identical goldens).

Fail cases, one per new code: `bad-where`, `role-assign`, `impure-condition`, `bad-line-text`, `missing-line-id`
(warning-only case, in `run/` with the warning in `expect.txt`), `duplicate-line-id`, `dialogue-signature`,
`stateless-dialogue`, `unknown-exit`, `scene-choice`, `scene-follow-up`, `entered-outside-stage`,
`duplicate-def` (objective vs role), `bad-part-item`, `part-write`, `bad-quest-option`, `bad-objective-call`,
`decline-outside-setup`, `mirror-size` (warning, `run/`) — the §6 fragments of the quest examples among them.

- [ ] **Step 1:** write the cases; `QUEST_BLESS=1 just quest-test "-p quest-corpus"`; read every new golden and
  `expect.txt` by hand against the examples' prose ("What happens") — a golden is blessed only when it says what
  the example says.
- [ ] **Step 2:** `just quest-test` (whole workspace), `just quest-check`, `just quest-wasm` (all goldens
  reproduce on wasm32).
- [ ] **Step 3:** close-out: README, CLAUDE.md test-rule line, spec coverage below ticked, "As built" written
  (deviations from this plan, with reasons).
- [ ] **Step 4:** Commit `feat(quest): Phase 10 corpus and goldens; close-out`.

---

### Phase 10 spec coverage

| Spec (folded) | Tasks | Corpus (Task 49, ✓) |
|---|---|---|
| §5.11 form table, lexis (§5.13) | 36 | every `dx*`; fail `dialogue_signature`, `bark_choice`, `scene_choice` |
| §5.11 description not execution, pure conditions, `emit` as events | 38, 39, 43, 45 | `dx4_interrogation`, `dx5_vendors`; fail `impure_condition` |
| §5.11 purity, `COND_FUEL`, fault = false | 39, 41, 45 | `d_fault_is_false` |
| §5.11 the runner: pooled greetings, topic menu, walking steps, ends, `DialogueEnded`, `Done`, private | 45, 47 | `d_spec_*`, `dx2`, `dx6`, `dx7`, `d_detour_cycle`, `d_too_deep` |
| §5.11 scoring by conjuncts | 39 (scores), 45, 46 | `dx1_saliency`, `d_spec_greeting`, `dx4` |
| §5.11 filters, package-level stateless content | 38, 39, 47 | `qx5_town_life`, `d_commutative`; fail `stateless_dialogue` |
| §5.11 exits | 36, 38, 42, 45 | `qx4_debt`; fail `unknown_exit` |
| §5.11 scenes, barks, follow-ups, interruption, ambient casting | 36, 42, 46, 47 | `qx2_sealed_mine`, `qx5_town_life`, `dx2`; fail `scene_follow_up` |
| §5.11 pacing (batches, timed lines, `pause`) | 42, 43, 45, 46 | `qx2_sealed_mine`, `dx3_timeouts`, `d_read_your_emit`; fail `not_constant_range` |
| §5.11 across regions: read set, mirrors, routed `emit`, movable conversation | 39, 41, 45, 47 | `qx1_lost_dog`, `d_read_your_emit` |
| §5.11 mirror size | 39 | `mirror_size`, `d_spec_farmer` |
| §5.11 line ids, string table, edits | 38, 43, 45 | `missing_line_id`, `d_edit_ends_node`; fail `duplicate_line_id`, `bad_line_text`, `site_collision` |
| §5.12 header, `repeatable` options, `at role` | 36, 38, 40, 44, 47 | `qx2`, `qx3_bandit_camp`; fail `bad_quest_option*` |
| §5.12 `requires`, roles, optional roles, query subset | 36, 38, 39, 40, 47 | `q_spec_lostdog`, `qx1`; fail `bad_where`, `role_assign` |
| §5.12 stages, transitions, `Entered` | 38, 44 | `qx1`, `q_spec_lostdog`; fail `entered_outside_stage`, `no_stage` |
| §5.12 objectives, journal | 38, 39, 44 | `qx1`, `qx2`, `qx3`; fail `bad_objective_call`, `duplicate_def` |
| §5.12 participants, builtins, timers, `RoleLost`, repeatable reset | 40, 44, 47 | `qx1`, `qx3`; fail `decline_outside_setup` |
| §5.12 parts | 36, 38, 39, 40, 41, 44, 47 | `qx1`, `qx2`, `qx2_sealed_mine_no_room`; fail `bad_part_item`, `part_write` |
| §9.1 stub host Phase 10 API, several instances | 37, 47, 48 | every world case; `distance_to` |

**Not in Part 1** (all of it is Part 4): region anchors and `WorldView` host queries; `Intent`s; the VM's
undo driven by region rollback; conversations as carried NPC state (`carry_components`, `Client.talking_to`);
stamped picks and mirror messages between regions; `TALK_RANGE_BLOCKS <= HALO_BLOCKS`, `HEAR_RANGE_BLOCKS`;
the coordinator (requirement indexing, completion records, act gating); packages and their string tables per
locale; engine person data (`occupation`, tags).

---

## Part 2 — MVP leftovers (in `quest/`; regression-only test rule)

Source: the MVP plan's "Deferred minors, by crate" and "Not done", `quest/README.md` "Known MVP limits".
Every item of those lists is placed below: fixed in Task 50, written down in Task 51, or left on purpose
(Part 6).

#### Task 50: The deferred minors that matter once bytes come from a store or the network

**Files:** Modify `quest-vm/src/{mem.rs,hash.rs,alloc.rs,object.rs,save.rs,program.rs,dict.rs,interp_heap.rs,wire.rs,vm.rs,migrate.rs}`,
`quest-stub/src/{runner.rs,namespace.rs}`, `quest-cli/src/main.rs`, `justfile` (`quest-wasm`); Test: one
regression per fix, in the existing test file of that module.

Each line is one fix and one regression test named after it. They are bugs the reviews found, so the rule allows
the tests.

| # | Fix | Regression test |
|---|---|---|
| 1 | `Mem::from_bytes` refuses a length that is no multiple of 8; `VmConfig.mem_bytes` above `u32::MAX` is `LoadError` (no silent truncation in `Mem::len`) | `mem.rs` `from_bytes_refuses_ragged_length`, `save.rs` `oversized_mem_is_refused` |
| 2 | `Fnv64::write_u64` hashes `to_le_bytes()` (it used native endianness; goldens do not change on little-endian) | `mem.rs` `fnv_u64_is_little_endian` (a pinned value) |
| 3 | `Vm::load` refuses a state whose size differs from `cfg.mem_bytes` | `save.rs` `load_checks_mem_bytes` |
| 4 | `Vm::load` fails the instance (`Corrupt`) on an `H_FUEL_DEBT` above `FUEL_DEBT_MAX` (new const: the largest debt one instruction can leave), on a current fn id that does not exist, and on a live block with header kind `Free` | `save.rs` `load_of_absurd_fuel_debt_is_corrupt`, `load_of_unknown_fn_is_corrupt` |
| 5 | `Heap::ah` and every address sum use `checked_add` → `Corrupt`; `hdr()` refuses a zombie | `alloc.rs` `address_overflow_is_corrupt` |
| 6 | `Program::validate` refuses an enum with more variants than the `u16` tag holds and an instruction with unknown immediate flags | `program.rs` `validate_rejects_variant_overflow`, `validate_rejects_unknown_flags` |
| 7 | `SetKey` and `from_wire` release what they retained when they fail (`ValueTooLarge`, a corrupt dict) | `dicts.rs` `failed_set_key_leaks_nothing`, `wire.rs` `failed_from_wire_leaks_nothing` (heap words back to baseline) |
| 8 | `DictKeyAt` / `DictValAt` charge fuel per probe (a fuel-table change: `BYTECODE_VERSION` bump, re-bless) | `dicts.rs` `dict_probe_is_charged` |
| 9 | `Vm::migrate` takes `self` by value and returns the new VM or `(old, MigrateError)` — the "host must discard the old VM" rule becomes a type | `run_edits.rs` — the existing tests compile against the new signature; no new test |
| 10 | stub: the runner's request expiry and `Vm::tick`'s timeout pass agree at zero fuel; an `op: None` failure rule answers only with errors the op declares | `stub.rs` `zero_fuel_tick_times_out_once`, `any_op_failure_respects_declared_errors` |
| 11 | CLI: `read_tree` refuses a symlink cycle (exit 66); the state file is written to a temp name and renamed; drop reasons print as text, not `Debug` | `cli.rs` `symlink_cycle_is_exit_66`, `drop_reason_is_plain_text` |
| 12 | `just quest-wasm` fails when the wasm32 half ran 0 tests | none (a recipe) — run it once with the filter broken to see it fail |

- [ ] **Step 1:** for each row, write the regression test, see it fail, fix, see it pass
  (`just quest-test "-p <crate> --test <file> <name>"`).
- [ ] **Step 2:** rows 2 and 8: `QUEST_BLESS=1 just quest-test "-p quest-corpus"`; the golden diff shows only
  `bytecode` lines (row 8 also `fuel` lines of cases that index dicts) — read it.
- [ ] **Step 3:** `just quest-test`, `just quest-check`, `just quest-wasm`.
- [ ] **Step 4:** Commit per crate: `fix(quest-vm): …`, `fix(quest-stub): …`, `fix(quest-cli): …`,
  `chore(quest): quest-wasm fails on zero tests` (the `justfile` hunk staged alone).

Left as documented limits (README "Known limits", unchanged): the single error-trace buffer; ASCII-only case
mapping; bitwise float `==` inside collections; `range(...)` / `d.items()` only as a `for` iterable; an unawaited
`all(...)`; one `Changed(current)` type per program; an f-string hole cannot hold a string literal; the
quest-lang cosmetic double reports and quadratic lookups (the fuzzers of Part 3 decide whether any is a real
hang); a migration that cannot read a large *shared* value refuses the edit.

#### Task 51: The spec says what was built and what was decided since

**Files:** Modify `docs/superpowers/specs/2026-11-16-quest-script-language-design.md` (staged by filtered patch —
it holds other sessions' hunks), `quest/README.md`,
`docs/superpowers/plans/2026-11-16-quest-script-language-mvp.md` ("Not done": point at this plan), `CLAUDE.md`
(the quest paragraph: plan pointer; the test-rule sentence after Task 49).

Edits, each a sentence or a paragraph, no redesign:

- §8.1 — namespaces belong to one quest instance, system-made (precedence 2); "a block" → "a script instance"
  throughout §6.9, §7, §8.
- §8.2 — the example reads `compare_and_set("wyrm_quest", "open", str(e.player.id))` and
  `give(e.player, await chest.take("gold", 20)?)`; the base promise errors `Timeout` / `Lost` / `WrongType`;
  decision 21 (promise error sets by origin).
- §5.3 — `ref-reassign`; §5.7 — a bare error name is `Err(name)`; §6.1 — recursion is forbidden for calls only.
- §9.1 — fallible queries (`OutOfReach`) as built in Task 37b.
- §10 — five crates, not three; the G2 decision once made ("Integration later" paragraph).
- §13 — replace the game-integration clause with a pointer to this plan's Part 4; keep the rest.
- README — "Known MVP limits" → "Known limits"; drop the lines Part 1 and Task 37b removed.

- [ ] **Step 1:** make the edits; `git diff -U3 <spec>` → keep only your hunks → `git apply --cached --recount`.
- [ ] **Step 2:** `git diff --cached` shows only these lines; commit `docs(quest): the spec as built and as decided since`.

#### Task 52: Whole-branch review of `quest/` (checkpoint, not code)

The MVP's "Not done" lists "whole-branch review by the owner"; Phase 10 adds as much again.

- [ ] **Step 1:** ask Dawid whether he reviews it himself or wants `/code-review` over
  `git diff 083a612b..HEAD -- quest` (say what it costs before running it).
- [ ] **Step 2:** every finding that is a bug → a regression test and a fix, committed as `fix(quest): …`; every
  finding that is a limit → a README line.
- [ ] **Step 3:** `just quest-test`, `just quest-check`, `just quest-wasm` — all green before Part 4 links the VM.

---

## Part 3 — Fuzzing (in `quest/fuzz/`)

Source: the owner's rule (2026-11-16) — "broad assurance comes from fuzzing (planned later)"; spec §11's
properties; the MVP plan's Review Focus 1–2. The proptest suites of `quest-corpus` stay as they are; the
fuzzers reuse their generator.

`quest/fuzz/` is a cargo-fuzz crate with its **own** `[workspace]` (cargo-fuzz's default) and lockfile, excluded
from `quest/Cargo.toml` (`exclude = ["fuzz"]`), so libFuzzer and the sanitizer flags never reach `just
quest-test`. `forbid(unsafe_code)` stays in the five crates; the fuzz crate is glue only.

#### Task 53: The fuzz crate, the byte-level targets, the recipe

**Files:** Create `quest/fuzz/{Cargo.toml,fuzz_targets/{compile,program_bytes,vm_load,wire_decode,stub_inputs}.rs,seeds.sh}`;
Modify `quest/Cargo.toml` (`exclude`), `justfile` (`quest-fuzz`), `quest/README.md`, `.gitignore`
(`quest/fuzz/{corpus,artifacts,target}`).

**Interfaces — Produces:**

```
just quest-fuzz <target> [secs=60] [jobs=1]     # {{cap}} cargo fuzz run … -- -max_total_time=<secs> -timeout=10 -rss_limit_mb=2048
```

cargo-fuzz is installed pinned, the way `quest-wasm` pins wasm-bindgen: `cargo install cargo-fuzz --version
<the newest at build time, written into the recipe> --root quest/target/tools/cargo-fuzz-<ver>`.

| Target | Input | Property |
|---|---|---|
| `compile` | bytes → lossy UTF-8 → a one-file tree | `quest_lang::compile` returns; no panic, no stack overflow, under the 10 s timeout |
| `program_bytes` | bytes | `Program::from_bytes` is `Ok` or `Err`; an `Ok` program loads into a `Vm` and runs 200 ticks against the stub with fuel 2000 without a panic; its status is never anything but idle / busy / `Failed` |
| `vm_load` | (a seed-chosen corpus program, bytes as a saved state) | `Vm::load` is `Err` or a VM that runs 200 ticks without a panic (ending `Failed(Corrupt)` is fine) |
| `wire_decode` | bytes | `WireValue::decode` is `Ok`/`Err`; `Ok(v)` re-encodes to bytes that decode to `v` |
| `stub_inputs` | bytes → text | `parse_people` and `parse_script` (Task 47) return `Ok`/`Err` |

`seeds.sh` copies `corpus/{run,fail}/*/*.qs` into `fuzz/corpus/compile/`, and writes each run case's
`Program::to_bytes()` and one saved state into the other targets' seed directories (through a small
`quest-corpus` example binary, `examples/dump_seeds.rs`).

- [ ] **Step 1:** the crate, the five targets, `seeds.sh`, the recipe (CPU-capped, `nice`d as every recipe).
- [ ] **Step 2:** `just quest-fuzz compile 30` and each other target for 30 s — each starts, loads its seeds and
  reports executions per second. A crash found here is a result, not a failure of the task: keep the artifact for
  Task 55.
- [ ] **Step 3:** `just quest-check` and `just quest-test` unchanged (the fuzz crate is outside both).
- [ ] **Step 4:** Commit `test(quest): cargo-fuzz targets over source, program bytes, saved state and wire values`
  (the `justfile` and `.gitignore` hunks staged alone).

#### Task 54: The semantic targets — VM invariants and the runner

**Files:** Create `quest/fuzz/fuzz_targets/{invariants,conversation}.rs`; Modify
`quest-corpus/src/generate.rs`, `quest/fuzz/Cargo.toml`.

**Interfaces — Produces:**

```rust
/// quest-corpus: the proptest generator driven by a seed instead of a runner, so libFuzzer can steer it.
pub fn program_from_seed(seed: [u8; 32]) -> String;     // generate.rs — `program()` over a `TestRng::from_seed(ChaCha, &seed)`
```

| Target | Input | Properties (spec §11) |
|---|---|---|
| `invariants` | 32 seed bytes + (fuel 1..=5000, latency 0..=8, a failure rule, a tick k, a tick j) | the generated program compiles (the generator's contract); **fuel equivalence** — output and final hash equal the unlimited-fuel run; **rollback bar** — checkpoint at k, run to j, roll back: `hash64()` and `mem_bytes()` equal; re-run reproduces the hashes; **save/load** — save at k, load, continue: the same trace; **no leaks** — heap words back to the post-init baseline when idle |
| `conversation` | a seed-chosen program of the Phase 10 corpus (compiled once per process) + bytes read as ops: `talk p n`, `pick serial n`, `leave`, `done`, `tick`, `checkpoint`, `rollback`, `edit <one of 3 variants>`, `shuffle attachments` | never a panic; every pick is `Ok` or a `PickRefused` carrying the current offer; after `rollback` the world hash equals the checkpoint's; shuffling attachment order never changes the next batch (commutativity); a conversation always reaches `Closed` within `CONVERSATION_LINGER_TICKS` + `DIALOGUE_STACK_MAX` × the longest pause once inputs stop |

- [ ] **Step 1:** `program_from_seed`; the two targets.
- [ ] **Step 2:** `just quest-fuzz invariants 60`, `just quest-fuzz conversation 60` — both run.
- [ ] **Step 3:** Commit `test(quest): fuzz the VM invariants and the conversation runner`.

#### Task 55: The first campaign, triaged

Fuzzing is CPU-heavy on a box the owner benchmarks on.

- [ ] **Step 1:** **ask Dawid** before running: seven targets × 30 minutes, one job each, capped (`just quest-fuzz
  <target> 1800`), or the length he prefers.
- [ ] **Step 2:** for every artifact: `cargo fuzz tmin` it, name the bug, write the regression test in the owning
  crate's test file (the input inline when it is under 20 lines, else a file under `quest/fuzz/regressions/<target>/`
  read by the test), fix, commit `fix(quest): <what> (fuzz)`.
- [ ] **Step 3:** a timeout that is a real quadratic path in the compiler (the deferred `Cx::local` / `use`
  fixpoint / occurs-check items) → fix it or bound it with a diagnostic (`too-complex`), with its regression.
- [ ] **Step 4:** README "Fuzzing": the targets, the recipe, how a crash becomes a regression. This plan's "As
  built": executions per target, crashes found and fixed.
- [ ] **Step 5:** Commit `docs(quest): the fuzzing campaign and how to run it`.


---

## Part 4 — The game (gates G1, G2; game-side test rules, G3)

Source: spec §9.1, §5.11 "Across region boundaries", §6.6, §6.8, §6.9, §8; `quests-in-the-world` (its §13 is the
order); construction design §3.3–3.6, §4.3, §5.2–5.3, §6, §9.2–9.3.

**How detailed this Part is.** Parts 1–3 are pinned to the line. Part 4 pins what the designs and the code
decide today — files, type and message names, where each thing lives, the tests that prove it — and leaves what
only the code in front of the implementer can settle (exact field lists, the closure shapes of the undo macro).
As the MVP plan did with Phase 10: **before starting a phase below, its first step is to read the named files
and write the exact signatures into this plan**. A phase that finds its design wrong stops and says so.

**File structure (new):**

```
quest/crates/quest-api/src/lib.rs          # game_api() -> HostApi: ONE description, used by the stub and the game   (Task 58)
crates/game/src/script/
  mod.rs consts.rs                         # the module, the tunables                                               (56)
  programs.rs                              # ProgramHash, the process-wide registry of immutable Arc<Program>        (57)
  store.rs                                 # ScriptStore: the instances of one region; tick, fuel, undo bridge       (57)
  host.rs query.rs command.rs events.rs    # RegionHost: quest_vm::Host over WorldView / region changes / sim events (58)
  saved.rs                                 # SavedScript: an instance as bytes — cuts, snapshots, seam transfers     (59, 61)
  talk.rs scenes.rs                        # conversations and scenes as carried NPC state; DialogueEnv              (62, 65)
  mirror.rs route.rs                       # mirrors, routed emits, where an instance lives                          (63)
  reap.rs                                  # tags, reaping                                                           (68)
crates/game/src/coordinator/quests/
  mod.rs facts.rs jobs.rs outcome.rs story.rs namespace.rs    # the coordinator's side                               (66–70)
crates/world-store/migrations/{sqlite,pg}/0017_script_programs.sql … 0023_region_lock.sql   # numbers as of 2026-11-17 (highest is 0016): take the next free ones
crates/world-store/src/{scripts.rs,namespaces.rs,quests.rs,snapshot.rs,admins.rs}
crates/client/src/renderer/{dialogue.rs,journal.rs,speech.rs,script_editor.rs}
crates/slint-ui/ui/{dialogue.slint,journal.slint,script-editor.slint}
```

Existing files every phase touches, and why (found 2026-11-17):

| File | What is there | Rule |
|---|---|---|
| `crates/game/src/state.rs:243–400` | `#[rollback(GameData)]`; `GameData` (:255), `Ecs` (:367), `Component<T>` (:554), `create_entity_safe` (:519) | new state is a field here; whole-struct state with its own closures follows `physics: PhysicsState` (`physics.rs:20`, `undo_scope_digest()`) |
| `crates/macros/src/lib.rs:1397–1577` | `Rollback::{new_transaction, rollback, forget, current, oldest}` | the VM's `checkpoint` / `rollback_to` / `forget_before` hang off these |
| `crates/game/src/region.rs:755–1289` | `handle_event`, `reconcile`, the `Tick` arm (:775 `tick_intents_safe`), `GameEventKind` dispatch (:1048) | scripts tick here, after intents |
| `crates/game/src/migrate.rs:13–116`, `portable.rs:255` | `Carried`, `Client::arrive`, `carry_components`, `restore_components_safe`, `classify` — all destructure with no `..` | every new `Ecs` / `GameData` / `Client` field is classified in each |
| `crates/game/src/region_runner.rs:32,103,487,630`, `slot_order.rs:69,215` | `RegionInput::Transfer(GameEventKind, Tick)`, `RegionOutput`, `SlotInbox`, `slot_class` | every new cross-region message is a `GameEventKind` with a slot class, sent as a `Transfer` |
| `crates/game/src/world_view.rs:87`, `intents.rs:60–429` | `WorldView`, `Target`, `Intent`, `IntentReply`, `apply_intent_safe` | reads and far writes |
| `crates/game/src/portable.rs:32–183`, `world-store/src/codec.rs:10`, `rows.rs:1293,1711` | `RegionMeta`, `PortableRegion`, `restore_portable`; `FORMAT_REGION_META`; `load_region`, `cut_rows` | script state is saved here |
| `crates/game/src/coordinator/msg.rs:18–251`, `mod.rs:245–290,672,722,1339`, `svc-link/src/lib.rs:56` | `HostMsg`, `CoordMsg`, `WorldRequest`, `RequestId`; `projection!`; dispatch, `on_maintain`, `request`; `LINK_PROTOCOL_VERSION` (8) | new messages; durable fields classified |
| `crates/game/src/history/records.rs:48,115` | `Person` (no occupation, no tags), `Site` | Task 60 |
| `crates/game/src/protocol.rs:588,737,815,929`, `ui.rs:20` | `EntityBundle`, `ClientPacket`, `ServerPacket`, `GameEventKind`, `UiAction` | player input for talk is a `UiAction` (already stamped and predicted) |
| `crates/client/src/renderer/slint_overlay.rs:783`, `chat/ui.rs:40`, `regions/runner.rs:165` | Slint glue, the predicted region loop | Task 64 |
| `crates/world-store/src/ctl.rs:75–80` | `worldctl` (hand-parsed subcommands) | new operator commands |

### Phase 4a — One script instance in a region (quests-in-the-world §13 step 2)

#### Task 56: `game` links the VM; `quest build`

**Files:** Modify `crates/game/Cargo.toml` (`quest-vm = { path = "../../quest/crates/quest-vm" }`), `Cargo.lock`,
`CLAUDE.md` (the quest paragraph: what may now depend on what), `quest-cli/src/main.rs`, `quest/README.md`,
`justfile` (`script-fixtures`); Create `crates/game/src/script/{mod.rs,consts.rs}`,
`crates/game/tests/fixtures/scripts/<name>/{main.qs,program.qprog}`, `crates/game/tests/it/script_link.rs`.

**Interfaces — Produces:**

```
quest build <dir> -o <file.qprog> [--host game|stub]   # Program::to_bytes() + the lines table; exit codes as `check`
just script-fixtures                                    # rebuilds every crates/game/tests/fixtures/scripts/*/program.qprog
```

```rust
// .qprog container (quest-vm/src/program.rs): magic "QPRG", u32 container version, u32 len + Program::to_bytes(),
// u32 count + (line id, text) pairs sorted by id. `Qprog::{to_bytes, from_bytes}`; hostile bytes → Err.
pub struct Qprog { pub program: Program, pub lines: BTreeMap<String, String> }
```

`game` never links `quest-lang`: its tests run **checked-in compiled fixtures** beside their source. The quest
workspace stays excluded from the root; `quest-vm` is built by the root as a non-member path dependency.

Tests: `script_link::fixture_program_loads_and_runs` (load `hello/program.qprog`, `Vm::new`, 10 ticks against a
recording host, the expected print); `script_link::fixtures_are_current` is **not** written (it would need the
compiler) — `just script-fixtures` followed by `git diff --exit-code crates/game/tests/fixtures/scripts` is the
check, run in this task and whenever a `BYTECODE_VERSION` bump lands.

- [ ] **Step 1:** `quest build` and `Qprog` (in `quest/`: no new test — the rule; run it on `corpus/run/handlers`
  and load the result back with `quest run --program <file>` — add that flag).
- [ ] **Step 2:** the dependency, the module, the fixture, the test. `just test-one game "--test it script_link::"` → PASS.
- [ ] **Step 3:** prove the isolation still holds where it matters: `cargo tree -p game -i quest-vm` shows only
  `game`; `grep -c lalrpop Cargo.lock` is 0; `just quest-test` builds nothing new; `just smoke`;
  `just determinism-wasm`; `just client-wasm`.
- [ ] **Step 4:** Commits: `feat(quest-cli): quest build and the .qprog container`; `feat(game): link quest-vm`
  (`Cargo.toml`, `Cargo.lock`, `CLAUDE.md`, `justfile` hunks staged alone).

#### Task 57: Script instances are region state

**Files:** Create `crates/game/src/script/{programs.rs,store.rs}`; Modify `state.rs` (`GameData.scripts`),
`region.rs` (the `Tick` arm; new `GameEventKind` arms), `protocol.rs`, `slot_order.rs` (`slot_class`),
`migrate.rs` + `portable.rs` (`classify`), `lib.rs` (re-exports — the macro needs the crate-root glob); Test
`crates/game/tests/it/script_state.rs`.

**Interfaces — Produces:**

```rust
pub struct ProgramHash(pub [u8; 32]);                       // SHA-256 of the .qprog bytes
pub mod programs { pub fn install(h: ProgramHash, p: Arc<Qprog>); pub fn get(h: &ProgramHash) -> Option<Arc<Qprog>>; }
//  ^ process-wide and immutable, like the installed forged world: NOT rollback state, never hashed.
pub struct QuestInstanceId(pub u64);                        // issued by the coordinator (Task 67); dev ids have the top bit set
pub struct ScriptInstanceId { pub quest: QuestInstanceId, pub owner: u16 /* 0 home, 1 + part index */ }
pub enum Anchor { Region, Entity(EntityId) }
pub struct ScriptInstance { pub program: ProgramHash, pub anchor: Anchor, pub vm: Vm, pub faults: u32 }
pub struct ScriptStore { instances: BTreeMap<ScriptInstanceId, ScriptInstance>, next_fuel_first: u64 }
// GameEventKind gains (each with a slot class; all arrive as stamped Transfers or region-local events):
ScriptPlace { id: ScriptInstanceId, program: ProgramHash, anchor: Anchor, roles: Vec<(String, WireValue)> },
ScriptRemove { id: ScriptInstanceId },
ScriptDeliver { id: ScriptInstanceId, event: String, value: WireValue },
```

- **Tick:** after `tick_intents_safe`, `tick_scripts_safe` runs every instance in id order starting from a
  rotating first (`next_fuel_first`), each with `min(SCRIPT_FUEL_PER_TICK, what is left of
  REGION_SCRIPT_FUEL_PER_TICK)`; an instance that got none continues next tick. Out of fuel is never a fault.
- **Undo bridge (spec §6.6, §9.1):** the VM keeps its own undo log. A region transaction that touches an instance
  takes `vm.checkpoint()` first and registers `move |d, _| d.instance(id).vm.rollback_to(cp)`; placing registers a
  remove, removing registers a re-insert of the saved instance. `forget_before` is called for checkpoints older
  than the region's oldest live transaction. **`ScriptPlace` is idempotent on `id`** (an existing id changes
  nothing — the re-delivery rule of Task 67).
- **Hash:** `StateHash for ScriptStore` = each id, program hash, anchor and `vm.hash64()`.
- A missing program (`programs::get` → `None`) leaves the instance inert and reports once; it is never dropped.
- Prints and reports go to a rate-limited `[script] instance=… ` log line per region window, never per tick.
- Until Task 58 the host is a stand-in that answers no query and logs commands.

Tests (`script_state.rs`, fixtures from Task 56): `place_is_idempotent_on_id`;
`script_state_undo_is_bit_exact_across_a_park` (Review Focus 7: place, deliver an event whose handler awaits,
run 5 ticks in 5 transactions, roll all back — `state_hash` equals the start; forward again — equals the first
run); `client_replay_reproduces_the_hash` (`reconcile` over the same events); `vm_undo_log_stays_bounded` (10 000
forgotten ticks: `vm.log_len()` below a constant); `region_fuel_budget_is_shared_fairly` (Review Focus 9: 32
instances each wanting 2000; no tick spends more than `REGION_SCRIPT_FUEL_PER_TICK`; after 32 ticks every
instance ran); `missing_program_is_inert_not_dropped`; `remove_then_undo_restores_the_instance_bit_exact`.

- [ ] **Step 0:** read `state.rs`, `physics.rs:20`, `macros/src/lib.rs:641–849`; write the exact closure shapes here.
- [ ] Steps 1–4 (tests → FAIL → implement → PASS): `just test-one game "--test it script_state::"`.
- [ ] **Step 5:** `just determinism-wasm` (an empty `ScriptStore` changes pinned hashes: re-record
  `wasm_determinism` / `scene_hash_golden` **on purpose**, saying so in the commit); `just test-threads`;
  `just smoke`.
- [ ] **Step 6:** measure once, in release, 32 idle instances and 32 busy ones against the region tick time
  (`just perf-server` — **ask before running**); write the number into "As built". This is the evidence Part 6's
  wasm-backend line waits for.
- [ ] **Step 7:** Commit `feat(game): script instances as region state — ticked, hashed, undone`.

#### Task 58: The game host — queries, commands, events

**Files:** Create `quest/crates/quest-api/{Cargo.toml,src/lib.rs}`, `crates/game/src/script/{host.rs,query.rs,command.rs,events.rs}`;
Modify `quest/Cargo.toml` (member), `quest-stub/src/api.rs` (re-export `quest_api::game_api()` as its v2 API —
the description moves, the stub's behaviour does not), `quest-cli` (`--host game` = the same table),
`crates/game/Cargo.toml` (`quest-api`), `region.rs`; Test `crates/game/tests/it/script_host.rs`.

**Interfaces — Produces:**

```rust
pub fn game_api() -> HostApi;                 // quest-api: THE description (Task 37's list); HostApi.version = GAME_HOST_API (2)
pub struct RegionHost<'a> { /* &dyn WorldView, the instance's id and anchor, out: &mut ScriptOut */ }
impl quest_vm::Host for RegionHost<'_> { … }
pub struct ScriptOut { pub commands: Vec<(ScriptInstanceId, u16, Vec<WireValue>)>, pub requests: Vec<…>, pub reports: Vec<…> }
```

- **One description for the stub and the game** — a program that compiles for one compiles for the other; a
  `Qprog` records the host version and `Vm::new` refuses a mismatch (spec §9.1).
- **Entity handles** are `EntityId { birth_region, serial }` in the wire struct's `id` (`Person`, `Creature`,
  `Player` each `{ id: Int }`, the two halves packed into the `Int` — the packing is a named fn with a round-trip
  test).
- **Queries** read the `WorldView` (own region ∪ halo, one tick old). An entity or block outside it → `None`
  where the signature has an `Option`, else `OutOfReach` (Task 37b). World facts (`act()`, `loop()`,
  `completed(def)`) read region-local values the coordinator pushes (Task 70; until then act 1, loop 1, nothing
  completed). Person facts (`occupation`, `site`, tags) wait for Task 60 and answer `None` / `false` until then.
- **Commands** are collected during the VM tick and applied **after** it, in issue order, inside the same tick
  and transaction: own-region targets as ordinary `*_safe` changes; anything else waits for Task 61. The `quest.*`
  commands are recorded into `ScriptOut` for the tasks that own them (62, 65, 68).
- **Requests** (promises) are recorded; **only the server, from a final tick, acts on them** (Task 66). A client's
  replay records and drops.
- **Events** raised by the sim into instances whose roles or anchor they concern: `PlayerNear` (a player within
  `TALK_RANGE_BLOCKS` × 4 of an entity a role binds, edge-triggered), `Killed`, `PlayerDefeated`, `ItemGiven`,
  `BlockUsed`, `RoleLost` (a role's entity removed for good — not a seam crossing), `Setup` (once, after
  `ScriptPlace`).

Tests (`script_host.rs`): `entity_handle_round_trips`; `query_reads_a_halo_entity`;
`query_beyond_the_halo_is_out_of_reach`; `command_applies_when_its_segment_commits_inside_the_tick`;
`aborted_segment_changes_nothing_in_the_region`; `killed_reaches_the_instance_whose_role_it_is`;
`role_lost_on_removal_not_on_seam_crossing`; `request_is_recorded_once_and_not_on_replay`. (No test pins "the stub
and the game share one API": the one shared function is the guarantee.)

- [ ] Step 0 (read `world_view.rs`, `intents.rs` module doc; pin signatures) → Steps 1–4 → `just test-one game "--test it script_host::"`;
  `just quest-test`, `just quest-check`; `just determinism-wasm`; commit `feat(game): the region host for scripts`.

#### Task 59: Saved, loaded, and present on clients

**Files:** Create `crates/game/src/script/saved.rs`, `crates/world-store/src/scripts.rs`,
`migrations/{sqlite,pg}/0017_script_programs.sql`; Modify `portable.rs` (`RegionMeta.scripts`), `codec.rs`
(`FORMAT_REGION_META` 1 → 2), `rows.rs`, `ctl.rs` (`worldctl script add|list|remove`), `protocol.rs`,
`world_manager/mod.rs`, `crates/client/src/{connections.rs,regions/*}`, `debug_launch.rs`, `justfile`; Test
`world-store/tests/scripts.rs`, `game/tests/it/script_saved.rs`.

**Interfaces — Produces:**

```rust
pub struct SavedScript { pub id: ScriptInstanceId, pub program: ProgramHash, pub anchor: Anchor, pub state: Vec<u8> /* Vm::save */ }
// world-store: script_programs(hash BLOB PRIMARY KEY, name TEXT NOT NULL, bytes BLOB NOT NULL, enabled INTEGER NOT NULL)
pub async fn put_program(…, hash, name, bytes) / get_program(hash) / list_programs();
// worldctl script add <url> <file.qprog> | script list <url> | script remove <url> <hash>
ClientPacket::RequestScriptProgram(ProgramHash)      ServerPacket::ScriptProgram { hash, bytes }
```

- `script_programs` is written **only by the operator and, later, the coordinator** (Task 74) — never by a cut
  part, so no two hosts' parts write it. Hosts read it when a region loads or a `ScriptPlace` names an unknown hash.
- A region row whose script needs a program the store does not have, a program of another `BYTECODE_VERSION` /
  host version, or a state `Vm::load` refuses, is **`LoadFailed`** — retried later, never regenerated, never a
  panic (Review Focus 10; spec §6.8's cheap answer).
- A client asks the **home** connection for a hash it lacks; a region snapshot that names one waits for it.
  The bytes are checked against the hash before `programs::install`.
- `client debug world --script <fixture>` places one dev instance (top-bit id) at the spawn site:
  `just run script=<fixture>`.

Tests: `scripts::program_round_trips_and_lists` (both backends, `pg-test-support`);
`script_saved::region_with_scripts_round_trips_through_portable` (hash equal after `portable_snapshot` →
`restore_portable`); `region_with_unknown_bytecode_version_is_load_failed`; `region_with_missing_program_is_load_failed`;
`worldctl.rs` `script_add_list_remove`.

- [ ] Steps 0–4; `just test-one world-store "--test scripts"`, `just test-one game "--test it script_saved::"`;
  world-store's whole suite once; `just determinism-wasm`; `just smoke`.
- [ ] **Step 5 (hand check, for TODO.md):** `just run script=hello` → the script's line appears; quit, run again →
  its state is where it was.
- [ ] **Step 6:** Commit `feat(game,world-store): script instances in cuts; programs by hash; clients fetch them`.

#### Task 60: What scripts can know about people — occupation, tags, sites

Source: construction design §4.3.

**Files:** Modify `crates/game/src/history/{records.rs,persist.rs}` (`Person.occupation`, `Person.tags`;
`HISTORY_FILE_VERSION` 5 → 6), the history generator that makes people, `state.rs` (`Ecs.tags: Component<Tags>`),
`migrate.rs` + `portable.rs` (classify: tags travel), `people.rs` (`spawn_person_safe` carries occupation, site
and tags onto the body), `script/query.rs`, `script/command.rs` (`tag(e, t)`); Test `game/tests/it/script_people.rs`.

**Interfaces — Produces:**

```rust
pub enum Occupation { Farmer, Miller, Smith, Guard, Elder, Miner, Merchant, Innkeeper, None }   // = quest_api's enum, same order
pub struct Tags(pub BTreeSet<String>);     // at most TAGS_MAX (16) of at most TAG_LEN_MAX (32) bytes
```

Occupations are assigned at the forge by a stateless roll over (person id, site kind, age): every populated site
gets at least one `Elder` and one `Innkeeper` or `Merchant` when it has the people for it — the examples' roles
must be bindable. Worlds are re-forged, not migrated.

Tests: `every_populated_site_has_an_elder`; `occupation_is_stable_across_two_forges`;
`tags_travel_across_a_seam` (seeded layer, `handoff_schedule`); `tag_command_is_undone_with_its_transaction`;
`person_facts_reach_a_script_query`.

- [ ] Steps 0–4; `just determinism-wasm`; `just test-threads`; check whether `build_world_map` output changed
  (if so bump `WORLD_MAP_VERSION`); commit `feat(game): occupations and tags — the person data scripts read`.

#### Task 61: Entity anchors and writes beyond the region

Source: spec §9.1 (anchors; `Intent`s; promises answered by an intent's reply).

**Files:** Modify `migrate.rs` (`Carried.scripts: Vec<SavedScript>`), `script/store.rs`, `script/command.rs`,
`intents.rs` (new `Intent` variants only where no existing one fits — `GiveItem { to, stack, by }`,
`TagEntity { tag, by }`; `Intent::by()` extended), `portable.rs`; Test `game/tests/handoff_schedule.rs` (seeded),
`game/tests/it/script_intents.rs`.

- An **entity-anchored** instance leaves with its entity: `carry_components` takes it out of the store as a
  `SavedScript` (`Vm::save` — the undo log does not travel; the transfer is a transaction boundary on both sides)
  and `restore_components_safe` puts it back. Its promises keep their ids.
- A command whose target is **not in the own region** becomes an `Intent` through `Effects::send`, applied by the
  owner at T+1. Where the script needs the outcome (`chest.take` across a seam) the host function is a promise
  and the `IntentReply` resolves it (`Granted` → the value, `Refused` → the declared error).
- A command on a target **beyond the halo** is dropped with one report (`OutOfReach`): scripts watch far places
  with a part.

Tests: `entity_anchored_instance_crosses_with_its_entity` (seeded schedule; hash equal to the no-seam run —
"a seam changes nothing observable"); `parked_handler_resumes_after_crossing`;
`far_take_is_a_promise_answered_by_the_intent_reply`; `refused_intent_is_the_declared_error`;
`command_beyond_the_halo_is_reported_once`.

- [ ] Steps 0–4; `just test-threads`; `just determinism-wasm`; commit
  `feat(game): entity-anchored scripts cross seams; far writes are intents`.

### Phase 4b — Talking (spec §5.11 in a world)

#### Task 62: Conversations in the NPC's region

**Files:** Create `crates/game/src/script/talk.rs`; Modify `state.rs` (`GameData.talk: Talk`), `ui.rs`
(`UiAction::{Talk { npc: EntityId }, Pick { serial: u32, option: u32 }, LeaveTalk, TalkDone}`), `region.rs`,
`people.rs` (an NPC in a conversation stands and faces the talker), `protocol.rs`, `migrate.rs` / `portable.rs`;
Test `game/tests/it/script_talk.rs`.

**Interfaces — Consumes** Part 1's `Conversation`, `Records`, `Attachment`, `DialogueEnv`, `Batch`.
**Produces:**

```rust
pub struct Talk { by_npc: BTreeMap<EntityId, NpcTalk> }
pub struct NpcTalk { pub attached: Vec<(ScriptInstanceId, u16 /*dialogue*/)>, pub records: Records,
                     pub conversation: Option<(ClientId, Conversation)> }
// what the talker's client is told (through the region's update channel, so prediction shows it at once):
GameDataUpdate::DialogueBatch { npc: EntityId, batch: Batch }
```

- **First step (amendment, Task 52 review A4):** the runner state has no byte form yet. Before any of it becomes
  region state, `quest-vm` gives `Conversation`, `Records`, `ScenePlay` and `Mirror` a canonical byte encoding
  (`encode` / `decode`, no `usize`, `BTreeMap` order, byte-identical on every platform) whose decode returns `Err`
  on hostile bytes (never a panic, every count bounded before it allocates); adds `Records::forget_instance(i)`
  (an ended or reaped instance's records pruned — today they only grow); settles the key of a **stateless**
  dialogue's records (its docs and its code disagree, review A4 — one rule, written in both); and bounds `once per` growth (one record per distinct `per` value, unbounded today:
  a named cap in `consts.rs`, the oldest dropped first, or a ruling that argues it out). Its own tests (initial
  implementation of the byte form) plus a hostile-bytes case for the fuzz targets of Part 3.
- A `Talk` builds the attachment list from the instances **in this region** (Task 63 adds the others), raises
  `Talked`, starts the conversation and emits the first batch — all in the tick the input is stamped at.
  Conversations are **private**: a second talker gets the NPC's `busy` bark or nothing.
- `DialogueEnv::eval` evaluates over the owning instance's **committed dialogue mirror** (ruling T47-2, spec
  §5.11 "Conditions run … over mirrored script state"), never over the live VM mid-segment: the mirror
  (`Vm::mirror(&QuestInfo::read_set ∪ {stage})`, refused `Busy` mid-segment or while initialisers are pending) is
  taken at tick boundaries, kept while unchanged, and evaluated with `COND_FUEL` on a `quest_vm::pure::MirrorVm`
  built from (program, `VmConfig`, `Mirror`) — the one scratch-VM path, which the stub already uses
  (`quest-stub/src/world/env.rs`) and `Mirror::eval_pure` delegates to (both **built in the Task 52 fix wave**,
  with the mirror's program content hash, A2, and the refusal while initialising, A1). `emit`s land in the
  owning instance's queue at **T+1** (the stub's rule, so the corpus goldens describe the game too).
- Wherever the game binds a program (a load, an edit, a state read back), it first calls
  `Program::check_api(&HostApi)` (**built in the Task 52 fix wave**, A6) and refuses a program that does not fit;
  the stub's `quest_stub::bind` is the model. A placement runs the initialisers first (`Vm::initialise`, built in
  the same wave) so `requires`, `where` and the first mirror read set globals, not zeros. Their host commands
  are held until the placement is accepted (a refused one acts on none of them), then acted on as a tick's — at
  every placement: the first, a repeatable's re-placement when it is reaped, and a reset (the stub's
  `World::hold` / `release`, Task 55).
- Ends: `Finished`; `Left` (`LeaveTalk`, or the talker's client gone); `OutOfRange` (talker and NPC more than
  `TALK_RANGE_BLOCKS` apart through `WorldView`, checked each tick); `Timeout`; `TooDeep`; `Edited` (Task 71).
  `DialogueEnded` is delivered at the end; the NPC is released at `TalkDone` or after `CONVERSATION_LINGER_TICKS`.
- `Talk`, `Records` and the conversation are rollback state (snapshot-before closures), hashed, saved in
  `RegionMeta`, and classified as travelling (Task 63 moves them).

Tests: `talk_starts_and_first_batch_arrives_in_the_same_tick`; `pick_walks_to_the_next_choice`;
`second_talker_is_refused`; `talker_gone_ends_out_of_range` (Review Focus 8);
`conversation_survives_rollback_and_replay` (Review Focus 7: mispredict a pick, reconcile — hash equals the
server's, one batch shown); `emit_is_visible_one_tick_later`; `linger_releases_the_npc`;
`transcript_equals_the_stub_golden` (the `qx1_lost_dog` fixture's lines, in the game, equal `corpus/run/qx1_lost_dog/expect.txt`'s
dialogue lines).

- [ ] Steps 0–4; `just determinism-wasm`; commit `feat(game): conversations as region state`.

#### Task 63: Across seams — mirrors, routed emits, parts, a conversation that travels

Source: spec §5.11 "Across region boundaries", §5.12 parts.

**Files:** Create `crates/game/src/script/{mirror.rs,route.rs}`; Modify `protocol.rs` (`GameEventKind`),
`slot_order.rs`, `region_runner.rs` (`RegionOutput::ScriptMsgs`), `world_manager/{mod.rs,peers.rs,succession.rs}`
(`host_fields!`), `peer/{mod.rs,journal.rs}`, `migrate.rs` (`Carried.talk`), `script/{talk.rs,store.rs}`; Test
`game/tests/handoff_schedule.rs`, `game/tests/peer_schedule.rs`, `server/tests/threaded_world.rs`.

**Interfaces — Produces:**

```rust
pub enum ScriptMsg {                                    // region → region, stamped, applied at T+1 through SlotInbox
    Mirror { from: ScriptInstanceId, to: MirrorTarget, mirror: quest_vm::Mirror },
    Emit { to: ScriptInstanceId, event: String, value: WireValue },
    Attach { npc: EntityId, from: ScriptInstanceId, dialogue: u16, on: bool },
    Pick { npc: EntityId, by: ClientId, serial: u32, option: u32 },   // a talker in the neighbour region
}
pub enum MirrorTarget { Npc(EntityId), Part(ScriptInstanceId) }
GameEventKind::Script(ScriptMsg)
// WorldManager: where each instance and each talked-to NPC is — classified in host_fields!
script_at: BTreeMap<ScriptInstanceId, RegionCoords>
```

- An instance sends its **dialogue read set** (`QuestInfo.read_set`) as a `Mirror` to each region holding an NPC
  it attaches to, and a home sends each **part read set** to its part — only when the mirror's `state_hash`
  changed, only from final ticks. The receiver stores mirrors as rollback state and evaluates conditions on a
  `MirrorVm` per mirror (kept while the mirror is unchanged; the one scratch path, Task 62), not a fresh VM per
  call. A mirror carries its program's content hash: one of another program (home and part migrated at
  different ticks) is refused by `Vm::install_mirror` and `MirrorVm::new` — the sender re-sends after an edit.
  What each part was last sent (the mirror's hash), each instance's last dialogue mirror and a repeatable's due
  `Setup` tick (`setup_at`) are saved state, kept as is: a load never re-derives or re-takes them (a part or
  instance mid-segment cannot give them again), and drops a record only where the loaded state shows another —
  re-sending an unchanged mirror installs equal values anew and the part's memory parts from a run never saved
  (the stub's `World::saved` / `restore` / `load_vms`, Task 55).
- The conversation, the attachments and the mirrors are **carried state of the NPC** (`Carried.talk`); the sender
  re-targets when it learns the NPC moved (the manager's `script_at` / entity routing).
- Between hosts a `ScriptMsg` is a `Reliable` peer message and is **journalled** (the peer-journal rule: a new
  state-bearing `Reliable` is journalled or argued out); ordered by the sender's stamp.
- A message for an instance that no longer exists is dropped and counted (`script_msgs_to_nowhere=` on the
  `[server]` line) — never an error loop.

Tests: `conversation_crosses_a_seam_with_its_npc` (Review Focus 8; seeded: same transcript and hash as the
one-region run); `talker_in_the_neighbour_region_picks_by_stamped_message`; `mirror_is_at_most_one_tick_old`;
`mirror_sent_only_on_change`; `part_emit_reaches_home_next_tick`; `part_in_another_host_reads_the_mirror`
(`peer_schedule`); `script_msgs_are_journalled_and_survive_a_succession` (`peer_journal::`);
`msg_to_a_removed_instance_is_counted`.

- [ ] Steps 0–4; `just test-threads`; `just test-r3`; `just test-r4` (the journal changed); `just determinism-wasm`;
  commit `feat(game): script mirrors, routed emits and travelling conversations`.

#### Task 64: The client — dialogue panel, pacing, journal, speech

**Files:** Create `crates/slint-ui/ui/{dialogue.slint,journal.slint}`,
`crates/client/src/renderer/{dialogue.rs,journal.rs,speech.rs}`; Modify `slint_overlay.rs`, `ui/index.slint`,
`session.rs` (session reset), `CursorPolicy` users, `protocol.rs` (`ServerPacket::Journal`); Test (ask first —
client tests are opt-in) `client --lib dialogue::`.

- **Lines are paced by the client** (construction design §6.1): a batch's lines show one after another, each for
  its `LineDef.ticks` or until the player advances; the options appear after the last line; `TalkDone` is sent
  when the player closes the ended conversation.
- **Text** comes from the program's lines table (`Qprog.lines`) by line id, `{0}`-style holes filled from
  `SaidLine.args`; a missing id shows the id (Task 76 adds locales).
- **Journal** (spec §5.12: derived, never stored): the home's region sends each participant
  `ServerPacket::Journal { quest, stage, objectives: Vec<(LineId, args, state, count, target)> }` when it changes;
  the panel lists the open objectives of the player's quests.
- **Speech** (scenes, barks): text above the speaker for clients within `HEAR_RANGE_BLOCKS`.
- Widgets follow the Slint rules (no `visible` / `opacity` on a component root; `GameTheme.apply()` in `init`).

- [ ] **Step 1:** panels and glue; `just smoke`; `just slint-wasm`.
- [ ] **Step 2:** ask Dawid before `just test-one client "--lib dialogue::"` (pacing: `batch_lines_show_in_order`,
  `options_wait_for_the_last_line`, `missing_line_id_shows_the_id`).
- [ ] **Step 3 (hand check, into TODO.md):** `just run script=lost_dog_site` → walk to Hal, talk, pick through the
  tree, the journal shows "Find the dog"; `just run-two` → the second player gets the busy bark.
- [ ] **Step 4:** Commit `feat(client): the dialogue panel, the journal and speech`.

#### Task 65: Scenes, barks and ambient casting in a region

Source: spec §5.11 scenes; construction design §5.2–5.3.

**Files:** Create `crates/game/src/script/scenes.rs`; Modify `script/talk.rs`, `script/command.rs` (`quest.play`,
`bark`), `people.rs`, `migrate.rs` (`Carried.scene` — a scene is carried state of its **lead**), `consts.rs`
(`AMBIENT_SCENE_PERIOD_TICKS` 200, `AMBIENT_SCENE_CHANCE_PCT` 25); Test `game/tests/it/script_scenes.rs`.

- A scene runs on Part 1's `ScenePlay`; its lines reach every client within `HEAR_RANGE_BLOCKS` of the speaker.
- **Ambient:** every `AMBIENT_SCENE_PERIOD_TICKS`, for a region with a player within hearing of idle NPCs, a
  stateless `roll(tick, region, salt)` under `AMBIENT_SCENE_CHANCE_PCT` calls `cast_ambient` over the stateless
  scenes of enabled programs (package-level content has no instance: its graph is evaluated with no globals).
- A `Talk` to an actor **interrupts** an interruptible scene (`SceneEnded { Interrupted }`) and is refused by an
  uninterruptible one; an actor removed or out of the lead's halo ends it `ActorLost`.

Tests: `staged_scene_timeline_equals_the_stub_golden` (`qx2_sealed_mine`'s `argument`); `talk_interrupts`;
`uninterruptible_refuses_talk`; `actor_lost_ends`; `ambient_cast_is_deterministic_across_replay`;
`scene_crosses_a_seam_with_its_lead` (seeded).

- [ ] Steps 0–4; `just test-threads`; `just determinism-wasm`; commit `feat(game): scenes, barks and ambient casting`.

### Phase 4c — The coordinator's side (quests-in-the-world §13 steps 3–6)

#### Task 66: Quest namespaces — the promise path

Source: spec §8 as amended (precedence 2); quests-in-the-world §2.

**Files:** Create `migrations/{sqlite,pg}/0018_quest_namespaces.sql`, `world-store/src/namespaces.rs`,
`crates/game/src/coordinator/quests/{mod.rs,namespace.rs}`; Modify `coordinator/{msg.rs,mod.rs}`,
`svc-link/src/lib.rs` (`LINK_PROTOCOL_VERSION` 8 → 9), `world_manager/world_link.rs`, `script/host.rs`,
`protocol.rs` (`GameEventKind::ScriptAnswer`), `server/src/coordinator_rt.rs`; Test `world-store/tests/namespaces.rs`,
`game/tests/coordinator.rs`, `server` `coordinator_rt` (`--features test-hooks`).

**Interfaces — Produces:**

```rust
// quest_namespace(instance BIGINT, key TEXT, value BYTEA, PRIMARY KEY (instance, key))
// quest_namespace_answers(instance BIGINT, promise BIGINT, answer BYTEA, PRIMARY KEY (instance, promise))
pub enum NsOp { Get(String), Set(String, WireValue), Add(String, i64), CompareAndSet { key, expected, new }, Delete(String) }
HostMsg::Namespace { instance: QuestInstanceId, promise: u64, op: NsOp }
CoordMsg::NamespaceAnswer { instance, promise, answer: Result<WireValue, WireValue> }
GameEventKind::ScriptAnswer { id: ScriptInstanceId, promise: u64, answer: Result<WireValue, WireValue> }   // stamped
```

- **The coordinator is the single writer.** One fenced transaction per request applies the op **and** records the
  answer under `(instance, promise)`; a resend finds the answer and changes nothing (the promise id is the
  idempotency key; it carries the instance epoch, so a Reset never reuses one). The answer is sent **after** the
  commit (write-before-send). These rows are not part of `core`: they are unbounded.
- A host re-sends every unanswered request of its instances on re-`Attach`. An answer for an instance the host no
  longer holds is forwarded by `script_at` or dropped and counted.
- The host stamps the answer into the region at a world tick; clients receive it in the event stream and send
  nothing. A namespace of a closed instance answers `NamespaceGone`.
- Role `all` / SQLite uses the same table through the inline link.

Tests: `namespaces::each_op_and_wrong_type`; `resent_request_is_answered_from_the_record`;
`write_is_final_across_a_host_rollback` (the region rolls back to its cut, the namespace keeps the write, the
re-run handler's `compare_and_set` sees `Changed`); `closed_instance_is_namespace_gone`;
`answer_enters_the_sim_at_one_tick_on_server_and_client`.

- [ ] Steps 0–4; `just test-r2`; world-store's whole suite once; commit
  `feat(coordinator): quest namespaces — fenced, idempotent, answered by stamped events`.

#### Task 67: Definitions, requirements over world facts, spawn jobs and the failure path

Source: quests-in-the-world §4, §5.1–§5.4 (world facts only), §8.

**Files:** Create `migrations/{sqlite,pg}/0019_quests.sql`, `world-store/src/quests.rs`,
`coordinator/quests/{facts.rs,jobs.rs}`; Modify `coordinator/{mod.rs (projection!, on_maintain),msg.rs}`,
`world_manager/{mod.rs,world_link.rs}`, `save_cut.rs` / `two_phase` reporting, `script/consts.rs`; Test
`game/tests/coordinator.rs`, `world-store/tests/quests.rs`, `server` `roles` / `hosts`.

**Interfaces — Produces:**

```rust
// quest_definitions(def TEXT PRIMARY KEY /* <program name>::<Quest> */, program BLOB, repeatable, cooldown_ms, …)
// quest_instances(id BIGINT PRIMARY KEY, def, state /* Pending|SettingUp|Running|Completed|Failed|Reaped */,
//                 bindings BYTEA, home_rx, home_rz, parts BYTEA, deadline_ms, epoch)
// quest_bound(kind, entity BIGINT, instance BIGINT, PRIMARY KEY (kind, entity))      -- a person or site in one job only
// quest_cooldowns(def, binding_key, until_ms)
HostMsg::LiveRegions { regions: Vec<RegionCoords> }                       // with the host's periodic messages
CoordMsg::QuestSetup { instance, def_program: ProgramHash, targets: Vec<(u16 /*owner*/, RegionCoords, Anchor)>,
                       roles: Vec<(String, WireValue)>, deadline_ms: u64 }
HostMsg::QuestSetupResult { instance, owner: u16, accepted: bool }
HostMsg::CutPrepared { …, quests: Vec<ScriptInstanceId> }                 // what this part's regions hold
```

- **Index** (§5.1): when `script_programs` changes, every enabled program with `Program.quest` becomes a
  definition row; nothing is re-parsed per check.
- **Check** (§5.2): every `QUEST_CHECK_PERIOD_MS`, for definitions that could start, restricted to **live areas**
  (a region with a player, or within `LIVE_AREA_REGIONS` of one, from `LiveRegions`). `requires` and each role's
  `where` are the program's own pure functions (the query subset guarantees they call only **facts**), run with
  `eval_pure` in a scratch VM against a `FactHost` over what the coordinator holds: history (people, their site,
  occupation, tags; sites and kinds), the story state, completions. Roles are bound in declaration order from the
  people and sites of the live areas; the first full binding wins. **Evaluate, do not index**, until a measurement
  says otherwise.
- **Job** (§5.3): one fenced transaction inserts the instance row, reserves the bindings and checks the budgets
  (`QUESTS_PER_REGION_MAX`, `QUESTS_PER_LIVE_AREA_MAX`, `QUESTS_PER_WORLD_MAX`) and the cooldown. The instance id
  is the idempotency key of everything after.
- **Placement** (§5.4): `QuestSetup` → the host stamps `ScriptPlace` (idempotent on id, Task 57) for the home and
  each part → `Setup` runs → `accept` (the handler returns) or `decline()` → `QuestSetupResult`. The instance is
  **Running only when a committed cut's parts list every one of its script instances**; a host that rolls back
  before that is re-sent the job. A decline in any target, or the deadline, fails the job: bindings released, a
  cooldown written, the other targets get `ScriptRemove`. The coordinator never wakes a parked region for a job.
- After *Running* the coordinator is hands-off.

Tests: `quest_program_is_indexed_once`; `requirements_bind_roles_in_order` (the Lost Dog: a farmer whose pet is
alive); `job_reserves_bindings` (a second definition cannot take the same person);
`budget_refuses_the_job`; `redelivery_after_rollback_makes_one_instance` (Review Focus 6);
`running_only_after_a_committed_cut`; `decline_releases_and_cools_down`;
`multi_region_decline_removes_the_others`; `deadline_fails_the_job`; `no_job_for_a_parked_region`.

- [ ] Steps 0–4; `just test-r2`, `just test-r3`; world-store's and game's whole non-client suites once;
- [ ] **Hand check (TODO.md):** `just script-fixtures`, `worldctl script add` the Lost Dog, `just run` → within a
  minute near a village a farmer has the quest.
- [ ] Commit `feat(coordinator): quest definitions, spawn jobs and the failure path`.

#### Task 68: Completion, repeatable reset, reaping

Source: quests-in-the-world §6–§7; construction design §3.3–3.5, §9.2–9.3; game-design §4.1.

**Files:** Create `crates/game/src/script/reap.rs`, `coordinator/quests/outcome.rs`; Modify `0019_quests.sql`
(same migration if Task 67 is not yet released, else `0019b`: `quest_completions(def, player, count, last_ms)`),
`state.rs` (`Ecs.quest: Component<QuestInstanceId>`; the item stack's quest tag), `migrate.rs` / `portable.rs`,
`script/command.rs` (`quest.complete|fail|joined|left|join_refused`), `coordinator/msg.rs`, `history/` (a legends
entry); Test `game/tests/it/script_reap.rs`, `game/tests/coordinator.rs`.

**Interfaces — Produces:**

```rust
HostMsg::QuestOutcome { instance, epoch: u32, outcome: Outcome /* Completed(Vec<PlayerId>) | Failed | Abandoned */ }
CoordMsg::QuestReset { instance, epoch: u32 }        // repeatable, after its cooldown: Vm::reset in home and parts
CoordMsg::QuestReap { instance }                     // → GameEventKind::ScriptReap, stamped
```

- **Recorded once**, idempotent on `(instance, epoch)`: one-off / world-line → closed for everyone, a history
  entry naming the participants; repeatable → `quest_completions` per (definition, player), a history entry only
  for a first completion and at milestones (10, 100).
- **Reset** (repeatable): reap what the run created, `Vm::reset` (the epoch rises), the namespace is **kept**,
  roles re-checked, `Setup` again after the cooldown.
- **Tagged with the instance id:** script instances, spawned entities and people, placed blocks (a list in the
  instance's saved record), quest items. **Reap** removes them in the region as a stamped event (undone and saved
  like any change); an item in a player's inventory becomes an ordinary item (G5). The coordinator's row is
  *Reaped* only after a committed cut.
- **Triggers:** the script's own end; a lifetime (the definition's, default `QUEST_LIFETIME_DEFAULT_MS`) counted
  only while no player is in the live area; `QUEST_FAULTS_BEFORE_REAP` consecutive aborted handlers; the program
  disabled; an admin (Task 71).

Tests: `completion_recorded_once_across_a_host_rollback` (Review Focus 6); `one_off_closes_for_everyone`;
`repeatable_counts_per_player_and_resets_with_namespace_kept` (the Bandit Camp: the fourth run places the veteran
camp); `reap_removes_everything_tagged_and_is_undone_by_rollback`; `reaped_item_becomes_ordinary`;
`lifetime_does_not_run_while_a_player_is_near`; `faulting_instance_is_reaped` (Review Focus 9);
`first_completion_and_milestones_reach_history`.

- [ ] Steps 0–4; `just determinism-wasm`; `just test-threads`; `just test-r2`; commit
  `feat(quests): completion, repeatable reset and reaping`.

#### Task 69: Requirements over region snapshots

Source: quests-in-the-world §5.2 (region facts), §12 last bullet.

**Files:** Create `world-store/src/snapshot.rs`; Modify `coordinator/quests/facts.rs`, `server/src/coordinator_rt.rs`
(a worker beside the coordinator — its loop never waits on these reads, as `login_link::OrderResolver` does not);
Test `world-store/tests/snapshot.rs`, `game/tests/coordinator.rs`.

```rust
/// Decode-only: the rows of the last committed cut, off the sim. Never links the region sim.
pub struct RegionFacts { pub tick: u64, pub entities: Vec<(EntityId, KindTag, BTreeSet<String>)>,
                         pub fixtures: …, pub open_ground: …, pub scripts: Vec<ScriptInstanceId> }
pub async fn region_facts(store, rc: RegionCoords) -> Result<Option<RegionFacts>, StoreError>;   // read-only handle
```

The facts `has_tag`, `near(…, blocks)` and `Place.open_ground` are answered from these; a fact about a region
with no saved row is "unknown" and fails the requirement. Facts are up to one save interval stale: **the setup's
decline is the answer, not a fresher read**. No script-function escape hatch (G5).

Tests: `facts_come_from_the_last_committed_cut`; `unsaved_region_fails_the_requirement`;
`stale_binding_is_declined_at_setup`; `coordinator_loop_never_waits_on_a_snapshot_read` (test-hooks: a held read
does not delay a heartbeat-driven transition).

- [ ] Steps 0–4; `just test-r2`; commit `feat(coordinator): region-snapshot facts for quest requirements`.

#### Task 70: Acts

Source: quests-in-the-world §9; game-design §4.2; quest-examples §2 (the `[[act]]` sketch).

**Files:** Create `coordinator/quests/story.rs`, `migrations/{sqlite,pg}/0020_acts.sql`; Modify `coordinator/mod.rs`
(`projection!` durable: `story`), `coordinator/state.rs` (`COORDINATOR_STATE_VERSION` 5 → 6), `msg.rs`
(`CoordMsg::Story { act, loop_no }`), `script/query.rs`, `ctl.rs` (`worldctl act set|list` until packages carry
acts, Task 74), the forge's world settings (`story_mode: Single | Multi`); Test `game/tests/coordinator.rs`.

```rust
pub struct Story { pub act: u32, pub loop_no: u32, pub opened_ms: u64 }          // in the core projection
// acts(n INTEGER PRIMARY KEY, id TEXT, quests BYTEA /* definition ids */, min_ms BIGINT)
```

A quest is world-line because an act lists it. The next act opens when every listed quest is completed and — in a
`Multi` world — `min_ms` has passed since the act opened (time-gated, not time-started). `act()` / `loop()` reach
regions as a stamped region-local value; `completed(def)` is answered per loop. After the last act the line
restarts at act 1 with `loop_no + 1`; what a later loop changes is open (Part 6).

Tests: `single_world_opens_the_next_act_on_completion`; `multi_world_waits_for_the_minimum_time`;
`time_alone_opens_nothing`; `requires_act_keeps_a_quest_out_of_other_acts`; `story_survives_a_coordinator_restart`
(it is in the row); `core_of_the_older_format_is_refused`.

- [ ] Steps 0–4; `just test-r2`; commit `feat(coordinator): acts and the world's story state`.

### Phase 4d — Editor mode

#### Task 71: Admins, the script block, editing a live instance

Source: quests-in-the-world §10; game-design §6; spec §6.9;
`specs/2026-11-14-creative-mode-and-chat-design.md` as the starting point for the mode itself.

**Files:** Create `migrations/{sqlite,pg}/0021_admins.sql`, `world-store/src/admins.rs`,
`crates/slint-ui/ui/script-editor.slint`, `crates/client/src/renderer/script_editor.rs`; Modify `ctl.rs`
(`worldctl admin grant|revoke|list <url> <player>`), `adventure.rs` (`PlayMode::Editor`; `Overview` gated the
same way), `protocol.rs`, `world_manager/mod.rs`, `crates/server/Cargo.toml` (`quest-lang`, `quest-api` — the
only crate that links the compiler, G2), `server/src/script_edit.rs` (new), block props (a `ScriptBlock` kind),
`script/{store.rs,talk.rs}`; Test `world-store/tests/worldctl.rs`, `server` tests, `game/tests/it/script_edit.rs`.

```rust
ClientPacket::ScriptAdmin(ScriptAdminReq)        ServerPacket::ScriptAdmin(ScriptAdminReply)
pub enum ScriptAdminReq { List { region }, Source { id }, Edit { id, files: Vec<(String, String)> }, Reset { id }, Reap { quest } }
pub enum ScriptAdminReply { Instances(Vec<…>), Source(Vec<(String, String)>), Diagnostics(Vec<String>),
                            Migrated(MigrationReport), Refused(String) }
GameEventKind::ScriptEdit { id, program: ProgramHash }      // stamped; Vm::migrate at the tick boundary
GameEventKind::ScriptReset { id }
```

- Admins are **world data** (a world-store table, never the login service). A session learns `admin` at spawn;
  every `ScriptAdmin` request and every mode switch to `Editor` / `Overview` is checked on the **host**.
- A **script block** is an access point: it holds no code; using it as an admin in editor mode opens the list of
  that region's instances. A non-admin gets nothing (G5).
- **Edit:** the server compiles the files against `quest_api::game_api()`; diagnostics go back as text; a clean
  build is stored in `script_programs` (with its source archive, so `Source` can return it and the editor can
  export it), clients fetch it (Task 59), then `ScriptEdit` is stamped: `Vm::migrate` (Task 50's consuming
  signature), conversations of that instance `rebind` (ends `Edited` when their node is gone), the migration
  report returns to the admin. A refused migration leaves the instance as it was. Rollback does not cross an edit
  or a Reset (MVP decision 14): both are transaction boundaries the region forgets behind.
- An edit is a hot fix to that instance only; keeping it means exporting it back into the package source.

Tests: `admin_grant_revoke_list`; `non_admin_script_request_is_refused`; `non_admin_cannot_enter_editor_mode`;
`edit_with_errors_returns_diagnostics_and_changes_nothing`; `edit_migrates_at_a_tick_boundary_on_server_and_client`;
`edit_ends_a_conversation_whose_node_is_gone`; `refused_migration_keeps_the_old_instance`; `reset_equals_fresh`.

- [ ] Steps 0–4; `just smoke`; `just slint-wasm`; `just determinism-wasm`;
- [ ] **Hand check (TODO.md):** grant yourself admin, place a script block, change a line of Hal's dialogue, talk
  to him again.
- [ ] Commit `feat: editor mode — admins, the script block, live script edits`; write Part 4's "As built".

---

## Part 5 — Content packages (gates G1, G5)

Source: `content-packages` design; game-design §7; quest-examples (package headers, the `[[act]]` sketch, `use
trade::haggle::haggle`). Tasks 72–73 are in `quest/` (regression-only test rule: checked by building the example
packages) and may start after Part 1. Tasks 74–76 need Task 66's world tables and Task 67's index.

Package tooling gets its **own crate, `quest/crates/quest-pkg`** (TOML, semver and SHA-256 dependencies), so
`quest-vm` and `quest-lang` keep their "no serde" build and their wasm32 target; only `quest-cli` links it. It must
not pull in game crates (packages §8).

#### Task 72: The package format — `quest pkg new | check | build`

**Files:** Create `quest/crates/quest-pkg/{Cargo.toml,src/{lib.rs,manifest.rs,archive.rs,build.rs}}`,
`packages/README.md`; Modify `quest/Cargo.toml`, `quest-cli/src/main.rs`, `quest/README.md`, `justfile`
(`quest-pkg`).

**Interfaces — Produces:**

```toml
# package.toml
[package]
name = "errantry/lost-dog"        # <publisher>/<name>; the crate name other packages `use` is the part after the slash, `-` → `_`
version = "0.1.0"                 # semver
host_api = 2                      # the HostApi.version it compiles against
[dependencies]
"errantry/trade" = "^0.1"
[[program]]                       # one compiled Program each; a program holds at most one quest
root = "src/main.qs"
[[act]]                           # optional (Task 70's rows)
id = "act-2"; title = "@act2.title"; quests = ["errantry/act-two::SealedMine"]; min_days = 7
```

```rust
pub struct Build { pub hash: [u8; 32], pub archive: Vec<u8> }
pub fn build(dir: &Path, deps: &Lock) -> Result<Build, Vec<Diagnostic>>;
pub fn open(archive: &[u8]) -> Result<Package, PkgError>;      // verifies structure; hostile bytes → Err
```

- **The archive is canonical** (packages §2): an uncompressed ustar stream, paths sorted, mtime 0, mode 0644,
  uid/gid 0, no extended headers; the **build hash is SHA-256 of those bytes**. Building the same sources twice,
  on native or anywhere else, gives the same hash.
- Contents: `package.toml`, `package.lock`, `src/**`, `build/<program>.qprog` (Task 56's container),
  `build/lines.default.tsv` (line id, text — every program's table merged; a duplicate id across programs is
  `duplicate-line-id`), `prefabs/**` (Task 76).
- **Qualified ids** are `<publisher>/<name>::<item>`; a quest definition's id is `<package>::<Quest>`.
- `quest pkg check` compiles every program against the host API version the manifest declares and prints
  diagnostics; `quest pkg new <publisher>/<name>` writes a manifest and `src/main.qs`.

- [ ] **Step 1:** the crate and the three commands.
- [ ] **Step 2:** check by use (no new tests — the rule): make `packages/errantry/lost-dog/` from quest-examples §1;
  `just quest-pkg build packages/errantry/lost-dog` twice → the same hash printed; `quest pkg check` on a copy
  with a type error → the diagnostic and exit 1; `tar -tvf` the archive → sorted, zeroed metadata.
- [ ] **Step 3:** `just quest-check` (the wasm32 build excludes `quest-pkg` as it excludes `quest-cli`).
- [ ] **Step 4:** Commit `feat(quest): packages — the manifest, the canonical archive, pkg new/check/build`.

#### Task 73: Dependencies and the lockfile — `quest pkg lock`

**Files:** Create `quest-pkg/src/{resolve.rs,lock.rs,registry.rs}`; Modify `quest-lang/src/modules.rs` (a
dependency is an extra crate root: `use trade::haggle::haggle`), `quest-pkg/src/build.rs`, `quest-cli`.

```rust
pub trait Registry { fn versions(&self, name: &str) -> Vec<(Version, [u8; 32])>; fn fetch(&self, hash: &[u8; 32]) -> Option<Vec<u8>>; }
pub struct DirRegistry(pub PathBuf);       // a directory of built archives — the only registry until Task 77
pub struct Lock { pub packages: BTreeMap<String, (Version, [u8; 32])> }     // package.lock: sorted text, one line each
pub fn resolve(manifest: &Manifest, reg: &dyn Registry) -> Result<Lock, ResolveError>;
```

- Semver ranges while developing; the lock pins **version and hash**; a built archive carries its lock, so
  nothing a world runs is resolved again (packages §3).
- **One version of a package per resolution**: a conflict is `ResolveError::Conflict` naming both requirers.
- A dependency's `pub` items are reachable as `<crate>::…`; its programs are **not** re-exported — a library
  package (`errantry/trade`) has no `[[program]]` of its own and is compiled into each dependent program.

- [ ] **Step 1:** resolver, lock, `DirRegistry`, the module-root change.
- [ ] **Step 2:** check by use: build `packages/errantry/trade`, then `quest pkg lock --registry <dir>` and
  `quest pkg build` in `packages/errantry/old-debts` (quest-examples §4) → it compiles and `quest run` plays the
  haggle with `--picks`; two dependents asking for `^0.1` and `^0.2` of one package → the conflict message.
- [ ] **Step 3:** if either check exposes a bug, it gets its regression test (the rule's one case).
- [ ] **Step 4:** Commit `feat(quest): package dependencies, the resolver and the lockfile`.

#### Task 74: A world's lock — enabling packages, clients fetching builds

Source: packages §3–§5, §7 ("enabling is a world-side operation").

**Files:** Create `migrations/{sqlite,pg}/0022_packages.sql`, `world-store/src/packages.rs`; Modify `ctl.rs`
(`worldctl package enable|disable|list|lock`), `crates/world-store/Cargo.toml` (it may link `quest-pkg` for
`open` and `resolve` — a path dependency like `quest-vm`; confirm under G2), `coordinator/quests/mod.rs` (the
index and the acts now come from the lock), `protocol.rs` (the join answer's `lock_hash`;
`ClientPacket::RequestBuild(hash)` / `ServerPacket::Build`), `crates/client/src/{connections.rs,login/flow.rs}`
(a build cache by hash — on disk natively, IndexedDB in the worker), `justfile` (`package-dev <dir>`); Test
`world-store/tests/{packages.rs,worldctl.rs}`, `server` join tests.

```rust
// world_lock(id CHECK (id = 1), lock BYTEA, hash BYTEA)      package_builds(hash BYTEA PRIMARY KEY, name, version, archive BYTEA)
// lock_history(at_tick BIGINT, who TEXT, change TEXT)
pub const LOCK_CHANGED_REFUSAL: &str = "the world's packages changed";      // a rejoinable refusal on the client
```

- `worldctl package enable <url> <archive>…` resolves the new set against what the world has, refuses a version
  conflict, **copies the builds into the world** (a world never depends on a package server staying up), writes
  the lock and its history, fills `script_programs`, the definition index and the acts. `disable` reaps that
  package's live quests (Task 68's trigger) and never deletes a build a saved region still names.
- A change that adds or removes worldgen content (prefabs, Task 76) prints the warning of packages §4.
- **Clients:** the join answer carries the lock hash; a client fetches each build it lacks **by hash**, verifies
  SHA-256 before use and caches it forever. A lock change with clients connected ends their sessions with
  `LOCK_CHANGED_REFUSAL`; they rejoin by themselves and fetch (G5). Single-player worlds read builds from disk.
- `just package-dev <dir>` builds a local package directory and enables it in the dev world — the inner loop.

Tests: `packages::enable_writes_lock_builds_and_history`; `two_versions_of_one_package_are_refused`;
`disable_keeps_builds_saved_regions_name`; `build_with_wrong_hash_is_refused` (Review Focus 10, on the host and on
the client); `join_answer_carries_the_lock_hash`; `lock_change_refuses_sessions_with_the_rejoinable_reason`.

- [ ] Steps 0–4; world-store's whole suite once; `just smoke`; `just client-wasm`; commit
  `feat: a world's package lock; clients fetch builds by hash`.

#### Task 75: The first packages — the examples, playable

**Files:** Create `packages/errantry/{lost-dog,act-two,bandits,trade,old-debts,town-life}/` (from quest-examples
§1–§5, their `people.txt` / `script.txt` beside them for `quest run`); Modify `justfile` (`packages-build`),
`debug_launch.rs` (the debug world enables them), `TODO.md` (the hand checks), `quest/corpus` (the `qx*` cases now
read their sources from `packages/` — one source, two uses).

- [ ] **Step 1:** the six packages; `just packages-build` → six hashes; `just quest-test "-p quest-corpus"`
  unchanged (same sources, same goldens).
- [ ] **Step 2:** `just run --reset` → the debug world has them enabled.
- [ ] **Step 3 (hand checks, written into TODO.md for Dawid):** the Lost Dog from "a farmer has a quest" to the
  reward, with a second player joining (`just run-two`); the Bandit Camp completed twice (the reset, the cooldown,
  the count); the Sealed Mine's staged scene heard by a bystander and act 2 → act 3 in a single-player world; a
  haggle through the shared dialogue; the town crier and an ambient scene in a town with no quest running; walk the
  dog's part across a region seam; quit mid-conversation and return.
- [ ] **Step 4:** Commit `feat(packages): the example quests as the first content packages`.

#### Task 76: Locales, and prefabs from packages

Source: packages §1 (string table per locale; prefabs), §4 (the lock that made a region).

**Files:** Modify `quest-pkg/src/build.rs` (`lines/<locale>.tsv` in the source tree → `build/lines.<locale>.tsv`;
a line id missing from a locale is a warning), `crates/client/src/renderer/dialogue.rs` (the client's language
setting picks the table; a missing line falls back to `default`), `crates/game/src/prefab/` (a runtime stamping
path: `layout_chunk` per touched chunk, applied as tagged block writes), `script/command.rs` (`place(prefab, at)`
→ `NoRoom` when the footprint is not open ground), `migrations/{sqlite,pg}/0023_region_lock.sql` (`regions.lock_hash`),
`rows.rs`, `worldgen` site generation (prefabs of enabled packages join `PrefabSet`); Test
`game/tests/it/script_prefab.rs`, `world-store/tests/packages.rs`.

- Prefabs keep the existing file format (`game::prefab::file`); a package's are named `<package>::<name>`.
- Stamping today happens only at worldgen genesis; a script's `place` is the first **runtime** stamp: its block
  writes are tagged with the quest instance (Task 68 reaps them) and undone with the transaction.
- A region row records the hash of the lock it was generated under, so odd terrain traces to a package change.
- If a prefab reaches `build_world_map`'s output, bump `WORLD_MAP_VERSION`.

Tests: `placed_prefab_is_tagged_and_reaped`; `place_without_room_raises_no_room`; `place_is_undone_by_rollback`;
`region_row_records_its_lock_hash`; client (ask first): `missing_locale_line_falls_back_to_default`.

- [ ] Steps 0–4; `just determinism-wasm`; `just world-map` after a forge with a package prefab (look at it);
  commit `feat: package locales and prefabs`; write Part 5's "As built".

#### Task 77: The package server — waits (G5)

Source: packages §7. **Not started from this plan**: it lives on the management PostgreSQL instance, which is
planned and not built (login, Keycloak and chat move there after the elastic-hosts work), and nothing above needs
it — `DirRegistry` and `worldctl package enable <archive>` cover every flow with files.

When it starts it gets its own plan. What that plan inherits from here: the archive format and hash (Task 72),
`Registry` as the client trait (Task 73 — an `HttpRegistry` beside `DirRegistry`), fetch-by-hash as a public,
immutable, cacheable route, yank = hidden from resolution but fetchable by hash, publish authenticated through
Keycloak via the login service, upload checks = `quest pkg check` (structure only, never content), archives in a
Garage bucket `packages`, a `deploy/modules/package-server` and orion root with an ECR repository and recipes as
for `login`, and a database that is **never** the world cluster's `pg`.

---

## Part 6 — Not planned (so nobody thinks it was forgotten)

Each line names where it was raised. None blocks Parts 1–5.

| Item | Raised in | Why not here |
|---|---|---|
| The wasm backend (scripts compiled to wasm modules, same memory layout) | spec §13 | "planned, after the interpreter"; nothing measured says the interpreter is too slow. Measure in Task 57 first. |
| Function parameters / function values, read-only references, generics, traits | spec §13 | language growth with no example that needs it |
| Namespace sharing between quests / players | spec §8.1, §13 | superseded: a namespace belongs to one quest instance |
| Bytecode upgrade policy in game (keep old interpreters vs migrate idle instances) | spec §6.8 | Task 59 takes the cheap answer (an unknown version is `LoadFailed`; worlds are re-forged); a real policy waits for the first format change after a world people care about |
| A sharing-preserving state migration | README "Known limits" | an edit is refused, state stays valid; Reset is the way out |
| A Yarn importer | dialogue research §5, packages §1 | the native constructs are the format (packages §9 "dialogue format" closes with Part 1) |
| Site generators and other worldgen *code* in packages | packages §1, §4 | no design for what language or API a site generator is written against; Task 76 ships prefabs only |
| `quest test` (a `#[test]`-style runner) | spec §9.1 (optional), MVP decision 16 | `assert` in `main()` and the corpus cover it |
| A browser / in-game script editor with diagnostics as you type | spec §10, §13 | Task 71 is a text box and a compile on the server; the compiler already builds for wasm32 when this is wanted |
| What a later story loop changes; party size and quests; AI adventurers | game-design §4.2, §11.3, §5 | game design, open |
| Holdings (land claims) and their permission checks | quests-in-the-world §11 | settling, not quests — it only shares the "coordinator pushes a stamped event" pattern of Task 66 |
| Publisher organisations, licences, moderation, removing another package's quest-line entries | packages §9 | open; none blocks a first server |
| The language's real name (`quest` / `.qs` are placeholders) | spec §13 | Dawid's |

---

## Spec coverage

| Source | Where it lands |
|---|---|
| Spec §5.1–5.10, §6, §7, §8 (standalone) | built (MVP Tasks 1–35) |
| Spec §5.11–5.13, §9.1 stub | Part 1, Tasks 36–49 (its own coverage table is at the end of Part 1) |
| Spec §9.1 `OutOfReach` | Task 37b, Task 58 |
| Spec §9.1 reads over `WorldView`, entity handles | Task 58 |
| Spec §9.1 writes as commands and `Intent`s, promises answered by intent replies | Tasks 58, 61 |
| Spec §9.1 / §6.6 undo is transparent; nothing sent from a replay | Task 57 |
| Spec §9.1 anchors (region, entity) | Tasks 57, 61 |
| Spec §6.8 versioning in game | Task 59 |
| Spec §6.9 edits and Reset in game | Task 71 |
| Spec §8 namespaces and the promise path ("writes are final") | Task 66 |
| Spec §5.11 across regions: conversation owned by the NPC's region, stamped picks, mirrors, routed `emit`, attachments travel | Tasks 62, 63 |
| Spec §5.11 pacing, `Done`, hearing range, scenes and ambient casting in a world | Tasks 62, 64, 65 |
| Spec §5.12 journal derived on the client; parts in other regions | Tasks 64, 63 |
| Spec §10 "Integration later" | Task 56 (gate G2) |
| Spec §11 testing | built; extended by Part 3 |
| Spec §13 | Part 4 (game integration), Part 6 (the rest) |
| Construction design §3.3–3.5 participants, failure, roles lost — in a world | Tasks 58 (`RoleLost` from the sim), 68 |
| Construction design §3.6 home and parts | Tasks 63, 67 |
| Construction design §4.3 person data predicates need | Task 60 |
| Construction design §5.2–5.3 who starts a scene; ownership, seams, interruption | Task 65 |
| Construction design §6 pacing | Tasks 62, 64 |
| Construction design §9.2–9.3 repeatable reset and its record | Task 68 |
| Quests-in-the-world §2 | precedence rules; Task 51 |
| Quests-in-the-world §4–§5.1, §5.3–§5.4 jobs and the failure path | Task 67 |
| Quests-in-the-world §5.2 world facts / region snapshots | Tasks 67 / 69 |
| Quests-in-the-world §6 completion, §7 reaping, §8 budgets | Task 68 (budgets checked in Task 67's transaction) |
| Quests-in-the-world §9 acts | Task 70 |
| Quests-in-the-world §10 editor mode and admins | Task 71 |
| Quests-in-the-world §11 holdings | Part 6 |
| Quests-in-the-world §13 order of work | Part 4's order: step 1 done; 2 → Tasks 56–61; (conversations, Tasks 62–65, sit here because a quest with nobody to talk to proves little); 3 → 66; 4 → 67–68; 5 → 69; 6 → 70 |
| Packages §1–§3 package, identity, versions and locking | Tasks 72, 73 |
| Packages §4 enabling, disabling, the lock that made a region | Tasks 74, 76 |
| Packages §5 clients | Task 74 |
| Packages §6 trust | Task 57 (the fuel-bounded VM is the boundary), Task 72 (`pkg check`) |
| Packages §7 the package server | Task 77 |
| Packages §8 tooling | Tasks 72, 73, 74 (the dev recipe) |
| MVP plan "Deferred minors" | Task 50 (fixed) or its "left as limits" list |
| MVP plan "Not done": Phase 10 / core `not-yet` / fuzzing / whole-branch review | Part 1 / Task 37b + Task 50's limits / Part 3 / Task 52 |
| Phase 10 handoff "Step 0" and "Open" | settled 2026-11-17 (Part 1's "Decisions settled") |
| Game-design §4.1 types and records, §4.2 acts, §4.3 where quests come from, §6 editor, §7 packages | Tasks 68, 70, 67, 71, Part 5 |

## As built

(Written per Part: at Task 49, Task 55, Task 71 and Task 76 — what differs from this plan, with reasons.)

### Part 1 — Phase 10 (Tasks 36–49 and 37b), written at Task 49 (2026-11-18)

Built standalone in `quest/` as planned; every task reviewed clean after at most one fix round. The full record of
rulings and minors is the SDD ledger, `.superpowers/sdd/2026-11-17-quest-script-completion/progress.md`. What
differs from the plan, with the reason:

- **Format and host API.** `BYTECODE_VERSION` went 1 → 6 (37b, 40, 41, 42, 44 — each a bump by one from the
  current value, not the plan's absolute numbers); the stub's `HostApi` 2 → 6: named-argument defaults
  (`HostFn.defaults`), fallible queries and commands (37b; a fallible command is checked against the current
  tick when issued and buffered on success — keeps `place(...) else: decline()` meaningful), `has_tag` a fact on
  every receiver and `kind` a fact, the relation `related(a, b)`, `HostTy::Dialogue`, and `Entity { id }` (any
  entity) for `ItemGiven.to` / `Killed.victim`. A raw `Value` event field and `after` of a host event stay
  `not-yet` (README limits).
- **Syntax.** `emit Ev(…)` lowers to the prelude `emit(Ev(…))`; `quest`, `scene` and `repeatable` stay contextual
  (the spec's §5.13 lists `quest` as reserved: Task 51 aligns it). Parser gaps left: an operand starting with
  `-`, `(` or `[` after `pause` / `requires`; an objective named `at` or `count`.
- **Codes beyond the plan's list:** `not-assignable` (a host property assigned), `no-stage`, `site-collision`.
  `bad-line-text` was reachable only from a hand-built tree (the grammar said `syntax`); fixed at Task 49.
  A graph past `GRAPH_STEPS_MAX` is still `internal` (a named code needs a 65 536-step case; not cheap).
- **Semantics settled by ruling:** a program's top-level `let`s are home state in a quest program (a part mirrors
  them and may not write them); statelessness is per dialogue (a filter or a dialogue outside any quest), not per
  program; `once` record sites are anchored on line ids (FNV-1a 32; a collision is `site-collision`), so edits keep
  spent records; transitions run after every committed home segment; an event no handler of this instance takes
  is routed to the instance that has one; the default `RoleLost` → `fail()` holds unless the quest body handles
  `RoleLost`; each top-level dialogue run raises its own `DialogueEnded` with its own exit; an empty node group
  ends the conversation; ties after the qualified name go to the program's content hash, then position; a mirror
  is not installed mid-segment (retried next tick); `mirror-size` leaves the generated `participants` out (Task
  49; a spec §5.11 deviation, in the README).
- **Added tunables:** `BATCH_STEPS_MAX` (4096 steps per batch) and `SCENE_FOLLOW_UPS_MAX` (16), beside the plan's.
- **The stub world:** conditions are evaluated over each instance's dialogue mirror; roles are checked by struct
  name as well as shape; a repeatable reset ends the run's conversations and scenes and drops its attachments;
  `main` runs once in the home after placement; `people.txt` / `script.txt` are defaults for quest programs only.
- **The corpus (Task 49):** world cases run in `World` (roles from the config line, `edit` from a subdirectory,
  warnings in `expect.txt`); the fuel-equivalence and save/load suites run the runner cases only. The Lost Dog's
  `NIGHT_TICKS` (300) and the Bandit Camp's `cooldown` (100) are shortened in their cases (a golden line per
  tick); the Bandit Camp's unhandled `await world.add` became `let _ = … else 0`, the Debt's `errantry/trade` a
  module, and the stub's missing lookups (`npc_named`, `here()`, `quest_open`) stand-ins — each case's header says so.
- **Example discrepancies found** (case headers): the innkeeper's self-detour gives three tellings, not "three
  inside the first" (`dx6_innkeeper`); the Bandit Camp's `exclusive` refusal cannot happen as written — `hired`
  is a `Notice` handler, so a second `Hired` after the first moved the quest on finds no handler (`qx3`); the
  §7 table's stale-menu pick re-sends the menu only when other topics remain — with none, the conversation ends
  (`qx1`, `d_read_your_emit`).
- **Open for Dawid:** the Sealed Mine plays an uninterruptible scene on the elder while the elder's menu may be
  open, but the spec has one activity per NPC — which wins.
- **Large files** (left as built): `quest-vm` `dialogue_run.rs` (~1100 lines) and its conversation module (~1260),
  `quest-stub` `world.rs` / `people.rs`, `quest-lang` `codegen/quest.rs`, `quest-cli` `main.rs`.

### Part 3 — Fuzzing (Tasks 53–55), written at Task 55 (2026-11-18)

The fuzz crate (`quest/fuzz/`, its own workspace) holds seven targets as planned (`compile`, `program_bytes`,
`vm_load`, `wire_decode`, `stub_inputs`, `invariants`, `conversation`); `just quest-fuzz <target> [secs] [jobs]`;
README "Fuzzing" says how to run, re-seed and turn a crash into a regression. What differs, with the reason:

- **The first campaign** (controller ruling: 10 minutes per target, one job, serially, CPU-capped — not the
  plan's 7 × 30 minutes after asking): `compile` 116 978 executions, `program_bytes` 2 062 043, `vm_load`
  336 497, `wire_decode` 5 624 076, `stub_inputs` 1 415 758, `conversation` 17 065 — no finding. `invariants`
  (~3 executions/s under the sanitizer) stopped three times, each fixed and the target run again: ~384, ~788,
  ~1 019 executions, then a clean 600 s of 2 228. No compiler timeout, so no `too-complex` code was added.
- **Found by the campaign — all three in the `invariants` target itself, none in the language:** fuel
  equivalence claimed under the `Timeout` failure rule (a promise timeout is counted in ticks, so a budget
  changes how far another handler got: README "Known limits"); the unlimited run unbounded in fuel (a heavy
  generated program, 258 M fuel, past libFuzzer's timeout under the sanitizer: `UNLIMITED_FUEL_MAX`); a world
  loaded back mid-cooldown lost its repeatable's due `Setup` (the target claimed it was had; `World::setup_due`).
  The inputs are kept under `quest/fuzz/regressions/invariants/` for `just quest-fuzz-replay invariants` (a target's
  own bug has no crate test to hold it).
- **Known bugs fixed first** (Task 52/54 reviews), each with a crate regression test: a transition cycle whose
  stages handle `Entered` re-armed every round (the chain's count kept by `Entered` handlers, across ticks — a
  per-tick reset left it fuel-dependent, final review; another handler begins a new chain);
  a late scene catch-up past `RUNNER_EVALS_MAX` ended `Finished` instead of yielding (the beat is put back and
  walked again); the home's initialisers' commands ran before placement was accepted and outside the world (held
  until accepted, at every placement — the first, a repeatable's re-placement, a reset; Task 62 says the game
  does the same); `World::load_vms` forgot which mirror each part was sent, so an unchanged mirror was installed
  again and the world's hash parted (Task 54's `crash-world-load-vms-remirror`): what each part was sent, the
  dialogue mirrors and the due `Setup` are saved state kept as is (`WorldSaved`, a `QSSTATE3` file), saves
  mid-segment included (final review); Task 63 says the game keeps them.
  `BYTECODE_VERSION` stays 7: no fix changed the format, no golden changed.
- **Final review fix wave:** `just quest-fuzz-replay <target>` replays the kept inputs; `quest-check` checks the
  fuzz crate; the Timeout exemption narrowed to runs where the rule fired.
