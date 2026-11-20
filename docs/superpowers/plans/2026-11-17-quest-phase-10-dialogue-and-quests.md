# Quest script Phase 10 — dialogue and quests (standalone) Implementation Plan

> **Superseded 2026-11-17** by `2026-11-17-quest-script-completion.md`, whose Part 1 is this plan copied whole (plus
> Task 37b) and which continues through the leftovers, fuzzing, the game and packages. Execute from that file.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `dialogue`, `scene` and `quest` items compile, lower and run inside the `quest/` workspace against the
stub host — conversations, scenes, quests with stages, objectives, timers, participants and parts — with no game
integration.

**Architecture:** The front end turns every condition, substitution, `emit` argument, `for`/`where` target and
stage transition into an ordinary **synthesised pure `fn`**, so inference, the call graph and codegen handle them
like any function; purity and the query subset are extra checks over those fns. A dialogue becomes plain data
(`DialogueGraph` in `Program`), run by an engine-agnostic **runner** in `quest-vm` that never touches a live VM: it
evaluates pure fns through a `DialogueEnv` (the stub answers from the live VM or a `Mirror`) and returns `emit`s as
values. A quest lowers to generated persistent globals (stage, objectives, participants, roles), generated fns
(stage entry, transitions) and a few new VM features (script events, timers, roles, instance owners). The stub
grows into a small multi-instance world (home + parts, people, scripted talk/picks).

**Tech Stack:** Rust 2024 in the isolated `quest/` workspace; LALRPOP grammar (`quest-lang/src/grammar.lalrpop`);
`state-hash` for hashing; `libm`; wasm32 for the determinism goldens.

**Spec:** `docs/superpowers/specs/2026-11-16-quest-script-language-design.md` §5.11–5.13, §9.1 (folded 2026-11-17
from `2026-11-16-quest-construction-design.md`, which keeps the reasons). Worked examples, which are the corpus:
`docs/superpowers/specs/2026-11-16-quest-examples.md`, `docs/superpowers/specs/2026-11-16-dialogue-examples.md`.
This plan continues the MVP plan (`2026-11-16-quest-script-language-mvp.md`; its Task conventions, file structure
and Global Constraints apply) and replaces its Phase 10 outline. Handoff:
`2026-11-17-quest-phase-10-dialogue-and-quests-handoff.md`.

## Decisions settled before this plan (Dawid, 2026-11-17)

- The construction design is folded into the spec (done with this plan).
- **Phase 10 is initial implementation:** its tasks carry their own tests and goldens like Phases 0–9. The
  regression-only rule (`quest/README.md`, CLAUDE.md) resumes after Task 49.
- **`part` is fully in scope**, including several instances in the stub (home + parts, mirrors, routed `emit`s).
- Conversations are **private**; a bark is a one-actor scene (the `dialogue name(npc: Person)` form stays);
  `world.*` keeps its name.

## Global Constraints

- Everything stays in `quest/`: its own `Cargo.toml` / `Cargo.lock`; nothing in the root depends on it; only path
  dependency into the game tree is `crates/state-hash`. Run through `just quest-check | quest-test | quest-run |
  quest-wasm` (CPU-capped). Format only inside `quest/` (`cd quest && cargo fmt`), never from the root.
- Determinism: graphs, records, conversations, scenes, mirrors and timers hash with `StateHash`, never serialized
  bytes. No `HashMap`/`HashSet` in state or in the compiler's iteration paths (`BTreeMap`). `libm` for floats.
  Randomness only through a stateless `roll(tick, key, salt)`.
- **The VM, the runner and the compiler never panic on any input** — hostile program bytes, hostile picks,
  unknown instance ids → `Err` / a refusal.
- Every format change bumps `BYTECODE_VERSION` (`quest-vm/src/bytecode.rs`) once per task that changes it, and
  re-blesses the existing corpus goldens in the same commit (`QUEST_BLESS=1 just quest-test "-p quest-corpus"`,
  read the diff: only `bytecode` lines may change in old cases).
- Host API changes bump `HostApi.version` (stub: 1 → 2 in Task 37).
- New tunables go in `quest-vm/src/consts.rs` (values below), named, with a one-line doc each.
- Diagnostic codes are string literals at their call site, as today; every new code appears in at least one
  `corpus/fail/` case by Task 49.
- Work on `develop`, no worktrees; commit only this task's paths (`git add <paths>`; check
  `git diff --cached --name-only` before each commit — other sessions stage in the shared index).

**New tunables (`quest-vm/src/consts.rs`):**

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

## Review Focus

1. **A stale pick** — a pick against a menu or choice that changed since it was offered (the option detached,
   its guard no longer holds, the index out of range, a pick after the end). Expect a refusal plus a re-sent
   offer, never a wrong branch or a panic. Test: Task 45 `stale_pick_is_refused_and_menu_resent`,
   `out_of_range_pick_is_refused`.
2. **A condition that faults or runs out of fuel** (division by zero in a guard, a long loop in a `fn` a guard
   calls). Expect: counts as false (an empty substitution), reported once to the owner, the conversation goes
   on. Test: Task 45 `faulting_guard_hides_option`, Task 41 `eval_pure_out_of_fuel_is_err_and_state_unchanged`.
3. **A quest edit mid-conversation / mid-stage** — the current node or stage removed by an edit. Expect
   `Edited` for the conversation; the stage migrates by name or, when gone, the instance resets to the first
   stage with a migration-report line. Test: Task 45 `edit_removing_current_node_ends_edited`, Task 44
   `edit_removing_current_stage_falls_back_to_first`.
4. **Role values that do not fit** (`--roles` giving a `Creature` for a `Person` role, a missing required
   role, a role whose `where` is false). Expect a load refusal naming the role (exit 2), never a run with a
   wrong binding. Test: Task 47 `role_of_wrong_type_is_refused`, `role_failing_where_is_refused`.
5. **Two emits racing one transition** (two players finish an objective in one tick; a timer firing the tick the
   stage changes). Expect: one stage entry, one `Entered`, the stale stage-scoped timer cancelled. Test: Task 44
   `stage_timer_cancelled_on_change`, `two_finishes_one_entry`.

---

## File structure

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

## Phase 10a — Front end

### Task 36: Grammar — the construction design's syntax

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

### Task 37: Host description — enums, properties, facts; the stub's quest API

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

### Task 38: HIR and resolution of dialogues, scenes, quests and parts

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

### Task 39: Types and the Phase 10 checks

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

## Phase 10b — VM

### Task 40: Script events, timers, roles and instance owners

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

### Task 41: Pure evaluation and mirrors

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

### Task 42: The dialogue graph format

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

## Phase 10c — Lowering

### Task 43: Codegen — dialogues and scenes

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

### Task 44: Codegen — quests and parts

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

## Phase 10d — The runner

### Task 45: Conversations

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

### Task 46: Scenes and barks

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

## Phase 10e — The stub world, the CLI, the corpus

### Task 47: The stub world — instances, people, routing, talk

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

### Task 48: The CLI

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

### Task 49: Corpus, goldens, close-out

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

## Spec coverage

| Spec (folded) | Tasks |
|---|---|
| §5.11 form table, lexis (§5.13) | 36 |
| §5.11 description not execution, pure conditions, `emit` as events | 38, 39, 43, 45 |
| §5.11 purity, `COND_FUEL`, fault = false | 39, 41, 45 |
| §5.11 the runner: pooled greetings, topic menu, walking steps, ends, `DialogueEnded`, `Done`, private | 45, 47 |
| §5.11 scoring by conjuncts | 39 (scores), 45, 46 |
| §5.11 filters, package-level stateless content | 38, 39, 47 |
| §5.11 exits | 36, 38, 42, 45 |
| §5.11 scenes, barks, follow-ups, interruption, ambient casting | 36, 42, 46, 47 |
| §5.11 pacing (batches, timed lines, `pause`) | 42, 43, 45, 46 |
| §5.11 across regions: read set, mirrors, routed `emit`, movable conversation | 39, 41, 45, 47 |
| §5.11 mirror size | 39 |
| §5.11 line ids, string table, edits | 38, 43, 45 |
| §5.12 header, `repeatable` options, `at role` | 36, 38, 40, 44, 47 |
| §5.12 `requires`, roles, optional roles, query subset | 36, 38, 39, 40, 47 |
| §5.12 stages, transitions, `Entered` | 38, 44 |
| §5.12 objectives, journal | 38, 39, 44 |
| §5.12 participants, builtins, timers, `RoleLost`, repeatable reset | 40, 44, 47 |
| §5.12 parts | 36, 38, 39, 40, 41, 44, 47 |
| §9.1 stub host Phase 10 API, several instances | 37, 47, 48 |

**Out of scope** (the game integration plan): region anchors and `WorldView` host queries; `Intent`s; the VM's
undo driven by region rollback; conversations as carried NPC state (`carry_components`, `Client.talking_to`);
stamped picks and mirror messages between regions; `TALK_RANGE_BLOCKS <= HALO_BLOCKS`, `HEAR_RANGE_BLOCKS`;
the coordinator (requirement indexing, completion records, act gating); packages and their string tables per
locale; engine person data (`occupation`, tags).

## As built

(Written at Task 49.)
