# quest

The quest script language: compiler (`quest-lang`), bytecode VM (`quest-vm`), in-memory stub host and
runner (`quest-stub`) and the `quest` command (`quest-cli`). Its own Cargo workspace; use the recipes
`just quest-check | quest-test | quest-run <dir> | quest-wasm` from the repository root.

## The language in one page

Indentation-structured, statically typed with inference, Python-looking on the surface, Rust-like underneath.
A quest block is one source directory; the examples below are real programs from `corpus/run/`.

**Bindings.** `let x = 1` introduces a binding, `x = 2` reassigns it. A name lives to the end of its block;
a later `let` of the same name shadows it, and may change its type. Types are inferred; annotate where the
compiler asks (`let n: int = ...`, `needs-annotation`). Literals: `true`/`false`, ints (`1_000`, `0xff`,
64-bit), floats (`1e3`), strings, f-strings (`f"{a} of {b}"`), lists `[1, 2]`, dicts `{"a": 1}`, `None`.

**Functions.** `fn name(a, b: int) -> int:`; parameter and return types are inferred when omitted. Control
flow: `if`/`elif`/`else`, `while`, `for x in list`, `for k, v in dict.items()`, `for i in range(n)`,
`break`, `continue`, `return`, `pass`. **No recursion**: a function that calls itself, directly or through
others (methods included), is the error `recursion`. Recursive *data* (a tree) is allowed; walk it with a
loop and a list.

**Values are copied.** `let b = a` copies (cheaply, copy on write); changing `b` never changes `a`. A
reference is the exception: `&x` (also into a place: `&quest.rewards[2]`), written in a type as `&T`,
assigned through with `*r = v`. Fields, methods, `in`, slices, `str(r)` and `assert_eq` auto-dereference
(`r.count = 1`); everything else takes `*r` (arithmetic, `min`/`max`/`abs`/`float`, `print`, `assert`,
`parse_int`, `await`). A reference may be passed down to a
callee; it may not be stored or returned (`ref-escapes`). A reference local or parameter cannot be
reassigned (`r = &b` is `ref-reassign`: `b` could die before `r`); bind a new one with `let r = &b`, whose scope
ends with its block.

**Structs, impl, enums, match.**

```
struct Ledger:
    entries: list[int]
impl Ledger:
    fn total(self) -> int: ...        # self: a copy
    fn add(&self, n): ...             # &self: may mutate
enum Stage:
    Asked
    Accepted(tick: int)
match stage:
    case Stage::Accepted(tick) if tick > 100: ...
    case Stage::Accepted(_) | Stage::Done: ...
```

Construct with `Ledger(entries=[])` (by name) or positionally. `match` must be exhaustive and an arm that can
never match is an error (`non-exhaustive`, `impossible-arm`); a missing arm of an otherwise-total `match`
faults deterministically rather than falling through. In a struct or variant pattern an omitted field acts
as `_`. `Option[T]` is `Some(x)` / `None`.

**Errors.** `error ChestError = ChestEmpty | NotEnough(have: int)` declares error names (one global space,
one data shape per name; sets may include other sets). `raise ChestEmpty` / `raise NotEnough(have=3)` need
no declaration on the function: a function's error set is inferred (`fn f() -> int raises S` pins it).
A fallible call must be handled:

- `f()?` propagates (the caller's set grows), `f() else 0` a fallback value, `f() else return` /
  `else raise X` / `else break` / `else continue`, or a block `else err: ...` that must leave by `return` /
  `raise` / a value (a trailing `match` is a statement, not the block's value);
- `match f(): case Ok(v): ... case Err(NotEnough(have)): ...`, exhaustive against the set; a bare error name
  (`case NotEnough(have):`) is shorthand for `Err(NotEnough(have))`;
- the same `?` / `else` work on `Option`.

An error that escapes a handler aborts it: its changes are undone, an error report with a trace is printed.
Runtime faults (division by zero, an index out of range, the heap full, ...) abort a handler the same way.
Running out of fuel is never a fault: the handler continues next tick.

**Collections and strings.** `list`: `append`, `insert(i, x)`, `pop()` (an `Option`), `remove(i)`, `len()`,
`xs[i]`, slices `xs[a:b]` (clamped), `x in xs`. `dict`: `d[k]`, `d[k] = v`, `get(k)` (an `Option`),
`remove(k)`, `keys()`, `values()`, `items()`, `in`; iteration is in insertion order, equality ignores order,
keys are ints, strings, bools, structs and enums (not floats, lists or dicts). Mutating a collection while
iterating it is defined (iteration is by insertion position). `str`: `len()` (characters), `split`,
`trim`, `starts_with`, `ends_with`, `contains`, `to_upper`/`to_lower` (ASCII only), `+`, `parse_int(s)`
(an `Option`). Methods are called with a dot, `len()` included.

**Numbers.** `int / int` truncates toward zero, `%` is the remainder; `int(x)`, `float(n)`, `abs`, `min`,
`max`, `floor`, `ceil`, `round`, `sqrt`, `sin`, `cos`, `pow`. Floats print shortest-round-trip; NaN is
canonical; `==` on floats is IEEE, inside a list it compares bits. All of it is bit-identical on every
platform.

**Prelude.** `print(s)` (takes a `str`), `str(x)`, `assert(c)`, `assert_eq(a, b)`, `range(n)` /
`range(a, b)`, `parse_int(s)`, the numeric functions above, `emit(e)`, `Some` / `None`, `Ok` / `Err`.

**Modules.** One file is one module; `mod rewards` declares a child (`rewards.qs` or `rewards/mod.qs`; a
child that others reach into is `pub mod`), `use rewards::give_gold`, `use a::{b, c as d}`, `pub` makes an
item visible outside its module, `crate::`, `super::`, `self::` paths. `use` cycles between modules are fine.

**Handlers and events.** `@on(PlayerClicked) fn greet(e): ...` runs once per event, one handler at a
time; several handlers of one event run in table order. A handler is also a plain function. `emit(Event(...))`
queues an event (the queue holds 4: a warning while busy, then the newest is dropped with an error).

**The host.** The host describes what a script may call (`HostApi`): functions, namespaces (`world.get`),
structs and events, and enums of data-less variants (`Occupation::Smith`; a script matches them like its own
enums; the host sees `Occupation::Smith` as the variant's name). A host *property* is read without parentheses
(`npc.alive`; `npc.alive()` is `not-callable`, `p.has_tag` without its call is `missing-call`, assigning one is
`not-assignable`). Host functions take arguments by position, then by name (`near(a, b, blocks=8)`); a
parameter with a default may be left out. A fallible host query (`a.distance_to(b) else -1`, `OutOfReach`) or
command (`place(...) else: ...`, `NoRoom`) is handled like any fallible call, its error set exactly what the host
declares (an error it does not declare is a fault). The host checks a fallible command against the current tick
when it is issued: a refusal takes the fallible path and nothing is buffered; a command that passes is handed
over when its segment commits, like any other, and a failure when it is applied (the world changed in between)
goes to the host's own error channel, never back to the script. A host binds a program only after
`Program::check_api` (every host call site of the fn's kind, parameters, result and errors; every host `emit`
of the event's fields): the stub refuses one that does not fit, a loaded state file's included.

**Promises.** Host reads and writes that answer later (`world.get`, `world.set`, `world.compare_and_set`,
`chest.take`, ...) return a promise; `await p` parks the handler until it resolves (other events run
meanwhile) and yields a result you handle like any fallible call (`await p?`, `await p else 0`, `match await p:`).
Hold a promise in a local to await it later; `await all([a, b])` awaits a list. `let _ = world.set(…)`
lets a promise run detached; only a promise being created can be detached (`let _ = p` of one already held
is `unused-promise`). A promise that is not a
direct host call (a parameter, field, list element, global, a function's return) carries the union of
**every** host error (decision 21): handle `Err(_)` there. Every awaited result (and `await all`) can also
fail with the **base promise errors** `Timeout`, `Lost` and `WrongType`, which the VM makes itself whatever
the host function declares: an exhaustive `match await chest.take(...)` needs arms for them (or `Err(_)`). Every await is a segment boundary: a fault after
an await undoes only the changes since it; commands and requests of an aborted segment never reach the host.
`compare_and_set(key, expected, new)` needs `expected` and `new` of the same type (`str(e.player.id)`, not
`e.player.id`, against a string key).

## What a block's state is

The top-level `let`s. They run once, in module-dependency then declaration order, before `main` (an `await`
in an initialiser is refused). Everything else (locals, parked handlers, the queue, pending promises, timers) is VM
state that is saved and restored with them. **Edits** (`quest run --load` with a changed script): a variable
whose name and type still fit keeps its value; a new one runs its initialiser; a removed or retyped one
becomes an **orphan** kept aside (a later edit that brings the name back with a fitting type takes it again);
queued events go to the new handlers (those with none are dropped), parked handlers are cancelled and their
writes stay, an event whose handlers already started is not re-delivered, timers stay set (one whose event is
gone or no longer fits is dropped). **Reset** (`--reset`)
re-initialises everything: variables, queue, parked handlers, promises, timers, orphans.

## Tunables

All in `crates/quest-vm/src/consts.rs`; some are part of the saved format. Instance memory 262144 bytes;
fuel 2000 per tick (CLI); queue 4; promise records 16; parked handlers 4; timers 16; promise timeout 200 ticks;
command buffer 8192 bytes; inbox 8192 bytes; error trace 16 entries; reference path 32 steps; struct or
variant fields 32; one structural walk (compare, hash, format) at most 65536 words (`ValueTooLarge`);
source nesting 256.

Dialogues and quests: a condition or substitution 500 fuel (`COND_FUEL`; running out counts as false); a
conversation's detour stack 16 (`DIALOGUE_STACK_MAX`, one more ends it `TooDeep`); an ended conversation stays
open 200 ticks without `done` (`CONVERSATION_LINGER_TICKS`); one batch walks at most 4096 steps
(`BATCH_STEPS_MAX`, else `TooDeep`) and makes at most 1024 pure evaluations (`RUNNER_EVALS_MAX`, else a
conversation ends `TooDeep`, a scene `Finished`); a node group or line group has at most 64 members
(`GROUP_MEMBERS_MAX`), a dialogue at most 1024 nodes (`DIALOGUE_NODES_MAX`; both `dialogue-too-large`); a scene takes at most 16 follow-ups (`SCENE_FOLLOW_UPS_MAX`); a timed
line lasts 30 ticks plus 1 per character (`LINE_BASE_TICKS`, `LINE_TICKS_PER_CHAR`), clamped to 30..200
(`LINE_MIN_TICKS`, `LINE_MAX_TICKS`); a program's dialogue graph at most 65535 steps (`GRAPH_STEPS_MAX`); a
read set (dialogue or part) above 256 words gets the `mirror-size` warning (`MIRROR_WORDS_WARN`).

## Tests, goldens

`just quest-check` (clippy `-D warnings` plus a wasm32 build), `just quest-test ["<cargo test args>"]`,
`just quest-run <dir> [flags]`, `just quest-wasm` (the corpus determinism goldens on native and on wasm32).
`corpus/run/<case>/` holds `main.qs`, `expect.txt` and `golden.txt` (bytecode hash, per-tick fuel and state
hash, output); `corpus/fail/<case>/` the expected diagnostics. When behaviour changes on purpose, re-bless:
`QUEST_BLESS=1 just quest-test "-p quest-corpus"`, read the diff of the goldens, commit it.

A run case may start `main.qs` with a config line (`#! fuel=200 ticks=500 latency=2 fail=Lost@2 chest=gold=3
role=farmer=Person(3)`; `role=NAME=WIRE` repeats, one per role). A **world case** — a program with a quest, or a
case with `people.txt` or `script.txt` (the formats of `quest run`, below) — runs in the stub's `World` as
`quest run` does: placed with its people and roles, the script's inputs at their ticks, until the script is
used up and the world is quiescent; an `edit <dir>` input compiles the case's subdirectory `<dir>/` (whose
files no `mod` of the case reaches). Its golden's per-tick fuel is the sum over the instances, its hash
`World::hash64`, its memory hash over every instance's memory; its report lines read `[tick] <instance>:
<report>`. A run case's `expect.txt` starts with the compiler's warnings, one line each, and `output:` when
there are any. The cases `dx*` are the dialogue examples, `qx*` the quest examples, `d_spec_*` / `q_spec_*` the
spec's own (`docs/superpowers/specs/2026-11-16-*-examples.md`); each says in its header what was adapted for the
stub and where the example's prose and the run differ. `d_commutative` is also run under every order of its
`mod` lines, which must give one golden. The fuel-equivalence and save/load-at-every-tick suites run the plain
(runner) cases only.

**Testing rule (owner, 2026-11-16).** The plan's tests are the initial implementation's. From now on a test is
written only as a regression for a bug that was actually found; never pre-emptively or "for coverage". Broad
assurance comes from fuzzing (below). Phase 10 (dialogue and quests) was initial implementation too and
carried its own tests; since its close-out (Task 49) the rule applies again.

## Fuzzing

`fuzz/` is a cargo-fuzz crate (libFuzzer) with its own workspace and lockfile, excluded from this workspace, so
neither `quest-check` nor `quest-test` builds it. `just quest-fuzz <target> [secs=60] [jobs=1]` runs one target,
CPU-capped like every `just` recipe (cargo-fuzz is installed pinned under `target/tools/` on first use; a 10 s
per-input timeout, 2 GiB RSS). Run one target at a time on a shared box; a target stops at its first crash.

| Target | Input | Holds |
|---|---|---|
| `compile` | bytes as lossy UTF-8, the file `main.qs` | `compile` returns: no panic, no stack overflow |
| `program_bytes` | bytes as a program | `Program::from_bytes` is `Ok`/`Err`; one that fits the stub's API runs `main` 200 ticks of fuel 2000 |
| `vm_load` | a byte picking a run case (`RUN[b % len]`), then a saved state | `Vm::load` is `Err`, or the VM runs 200 ticks |
| `wire_decode` | bytes as a wire value | `WireValue::decode` is `Ok`/`Err`; a value re-encodes to bytes that decode to it |
| `stub_inputs` | bytes as lossy UTF-8 | `parse_people` and `parse_script` return |
| `invariants` | 32 seed bytes for a generated program, then fuel, latency, a failure rule, ticks `k` and `j`, a world case, flags | spec §11 over the generated program (fuel equivalence, the rollback bar, save/load, no leaks) and over a corpus world case (fuel equivalence, save/load to the same `World::hash64`) — its module docs list each |
| `conversation` | a byte picking a Phase 10 case, then ops (talk, pick, leave, done, tick, checkpoint, rollback, edit, shuffle) | the conversation runner over the stub's `DialogueWorld`: picks refused only for a stale offer, rollback to the checkpoint's hash, attachment order commutes, every conversation closes |

**Seeds.** The corpora (`fuzz/corpus/<target>/`, git-ignored) are seeded by `fuzz/seeds.sh` when a target has
none: the corpus's sources, people and script files, and through `quest-corpus`'s `examples/dump_seeds.rs` each
run case's program bytes, a state saved halfway through its run and wire values; `invariants` and
`conversation` get byte strings written by the script. **Re-run `fuzz/seeds.sh` after a bytecode or
save-format change** (a `BYTECODE_VERSION` bump): old seeds then only reach the version check. Files libFuzzer
found are kept beside the seeds.

**A crash becomes a regression.** A crash, timeout or leak is written to `fuzz/artifacts/<target>/`
(git-ignored). Replay it from `quest/` with `target/tools/cargo-fuzz-<ver>/bin/cargo-fuzz fuzz run <target>
<file>`, shrink it with `... fuzz tmin <target> <file>`, and name the bug. Then write the regression test in the
crate that owns the bug (the input inline when it is under 20 lines, else a file under
`fuzz/regressions/<target>/` read by the test), see it fail, fix, see it pass, replay the artifact, and commit
`fix(quest…): <what> (fuzz)`. A bug in a target itself (a property it over-claims) is fixed in the target, and
its input kept under `fuzz/regressions/<target>/` for `cargo fuzz run <target> fuzz/regressions/<target>/*` (the files: given the directory, libFuzzer
fuzzes into it). A compiler timeout on a quadratic path is to be fixed, or bounded with a new `too-complex` diagnostic, with its
regression (none found yet). The first campaign (10 minutes per target) is recorded in the plan's "As
built" (`docs/superpowers/plans/2026-11-17-quest-script-completion.md`).

## Known limits

- **`not-yet` compile errors**: a fallible host property (`x.p` of a property that declares errors), an
  unawaited `all(...)`, `after` of a host event (a timer delivers a script `event` only), a host struct's or
  event's field of the open type `Value` (the host must give it a concrete type: the stub's `ItemGiven.to`
  and `Killed.victim` are `Entity { id }`, **any** entity by its id, HostApi 6), and `play` of a scene held in
  a variable or with none of its cast named (`play(scene, a=…)` needs the scene by name). Against a host API
  other than the stub's, also a quest built-in the host has no `quest.<name>` of the right shape for, and
  `after` / `cancel` without a `Timer { id }` type.
- An unset `site` or `owner` (a person without one) refuses the query; in a condition that is false.
- Conversations and scenes read each instance's **dialogue mirror**, its committed state as of the end of the
  last tick (one tick old; a handler still running over several ticks shows what it had before it began).
- A repeatable quest's reset ends the run's conversations (`Left`) and stops its scenes.
- No `busy` bark yet: a talk to an NPC in another conversation or an uninterruptible scene is ignored.
- `print`, `assert` and `assert_eq` are allowed in a pure function (a condition): they change no state.
- `once` records are keyed by a site anchored on line ids (the option's, the first member's, the node's name and
  first line, a `once:` block's first line), so they survive edits — of lines with explicit ids only: a line
  without one gets a provisional id numbered by its position (`missing-line-id`), which an edit above it
  shifts, and its records move with it. `once: once: …` with the same first line anchors both on one id and
  is refused (`site-collision`).
- A `let` named in `pause`, `(ticks=…)`, `count` or `timeout=` stands for its initialiser's literal, read at
  compile time: assigning the `let` at run time does not change the duration or the count.
- Fuel equivalence (a run at any budget per tick ends as the unlimited run) holds under the same host timing,
  not across a promise **timeout**: it is counted in ticks (`PROMISE_TIMEOUT_TICKS`), so a budget changes how
  much another handler did before it fires.
- A quest's transitions that chain into as many stages as the quest has (a cycle, `A → B → A`, both holding)
  stop where they stand, reported (`TransitionCycle`), instead of running every tick; the stages' handlers
  re-arm them no earlier than the next tick.
- A handler that changes the stage in the middle of an event: each later handler of that event checks the stage
  when it starts, so a handler of the new stage may run for the same event.
- An edit whose current stage no longer exists falls back to the first stage, without cancelling the removed
  stage's stage-scoped timers and without re-opening the first stage's objectives.
- `mirror-size` does not count the generated `participants` list (bounded by the party; every part reads it), a
  deviation from spec §5.11's "a list counts as unbounded"; every other `str`, `list` or `dict` global still
  counts `MIRROR_WORDS_WARN` on its own.
- A program whose dialogue graph passes `GRAPH_STEPS_MAX` (or holds more than 65 536 dialogues) is reported
  as the code `internal`, not a named code.
- `pause -1` (any operand starting with `-` or `[` after `pause` or `requires`, or a `(` with no space before it)
  is a syntax error rather than `not-constant`; `requires (a or b) and c` (a space before the `(`) parses.
- Float `==` inside collections compares bits; `to_upper` is ASCII only.
- The trace of a re-raise after another failure inside one handler shows the latest entries.
- The host must discard the old VM after a successful `migrate`.
- `range(...)` and `d.items()` only as a `for` loop's iterable (`not-yet` elsewhere).
- An f-string interpolation cannot contain a string literal (`f"{parse_int("4")}"` is a syntax error): bind
  it to a local first.
- `Changed(current)` has one type per program, so a program uses `compare_and_set` at one value type only.
- **An edit can be refused by a large shared value.** A migration reads every top-level value out as an
  unshared copy, bounded by the structural-walk limit (`WORK_WORDS_MAX`, 65536 words). A modest value that
  shares big sublists (the same 700-element row appended 100 times) walks far past it, so every edit of the
  block is refused (`GlobalUnreadable { kind: ValueTooLarge }`) and the state kept for the old script;
  Reset re-initialises it, losing the state. Avoid sharing large sublists in top-level state. A
  sharing-preserving migration is future work.

## Commands

```
quest check <dir>
quest run <dir> [--fuel N] [--ticks N] [--mem BYTES] [--trace] [--latency N]
                [--fail KIND[@NTH][:OP]] [--chest KIND=N] [--save FILE] [--load FILE] [--reset]
                [--roles NAME=WIRE,...] [--people FILE] [--script FILE] [--talk PLAYER:NPC]
                [--picks N|l,...] [--lines]
```

`<dir>` is read recursively for `*.qs` files (paths relative to it, `/` separators).

`quest check` runs every compiler stage, including layout and code generation, and discards the program: a
passing check means the program compiles. It prints only diagnostics, to stderr; warnings do not fail it.

`quest run` compiles, then runs the program against the stub host: the initialisers, `main` once, then
ticks until nothing is left to do. Script output goes to stdout; diagnostics, reports and trace lines go to
stderr. A program with a `quest`, or a run with `--talk`, `--script` or `--people`, runs in the **world**
instead (below). Only a program with a quest takes `<dir>/people.txt` and `<dir>/script.txt` by default.

| Flag | Meaning | Default |
|---|---|---|
| `--fuel N` | fuel per tick | 2000 |
| `--ticks N` | tick limit | 100000 |
| `--mem BYTES` | VM memory | 262144 |
| `--latency N` | ticks between a `world.*` / `chest.take` request and its answer | 0 |
| `--trace` | one stderr line per tick: `tick=<n> fuel=<used> hash=<16 hex> status=<idle\|busy\|failed>` | off |
| `--fail KIND[@NTH][:OP]` | the NTH (default 1) request of OP (default any) fails; KIND is `timeout`, `lost`, `changed` or `namespace-gone`; only requests whose op can fail that way count (`changed` only `world.compare_and_set`, `namespace-gone` only `world.*`); repeatable | none |
| `--chest KIND=N` | N items of KIND in the chest; repeatable | empty |
| `--save FILE` | write the program and its VM state after the run | |
| `--load FILE` | start from a saved state | |
| `--reset` | re-initialise the instance (after `--load`): as if the script had just been placed | off |
| `--roles NAME=WIRE,...` | the quest's role values as wire literals (`farmer=Person(3),miller=None`); repeatable | none |
| `--people FILE` | the people the world's queries read (format below) | a quest: `<dir>/people.txt` if present |
| `--script FILE` | the world's scripted inputs (format below) | a quest: `<dir>/script.txt` if present |
| `--talk PLAYER:NPC` | the player talks to the NPC at tick 1; each offer is answered from stdin (a number, `l` to leave; the input's end leaves), the end acknowledged with `done` | off |
| `--picks N\|l,...` | `--talk`'s answers, in turn (then `l`); needs `--talk` | stdin |
| `--lines` | print line ids (`@lostdog.plea(Rufus)`) instead of their texts | off |

**The world** (`quest_stub::World`, spec §5.11, §9.1): the quest's home and one instance per `part`, the
people, conversations, scenes, and the messages between them, which land at the next tick. Placement checks
`requires` and each role's type and `where`; `Setup` is delivered at tick 0, and `main`, when there is one,
runs once in the home as the runner runs it (asked for after placement, so it runs before `Setup`'s
handlers; its reports and faults exit as in the runner). Every line
goes to stdout as `[tick] text`: a script's `print`, `Hal: <line>`, `menu: 1) … 2) …`, `end Hal: <reason>`,
and the commands the world acts on (`joined Ana`, `give Ana 1 carved_whistle`, `follow Rufus Ana`,
`completed Ana`, `failed`, `declined`, `reaped`). The run ends when the script is used up, the `--talk`
conversation is over and nothing is left to do (a `--talk` that starts no conversation, an unknown id say,
says so on stderr). `--mem` is each instance's memory. `--save` writes every instance (`QSSTATE2` for more
than one); `--load` restores them (a changed script is migrated per instance, a part's report lines prefixed
with its name) and does not deliver `Setup` again; a state whose instance count differs from the program's
(a part added or removed by the edit) is refused (exit 2). `--reset`, with or without `--load`,
re-initialises every instance and delivers `Setup` again.

`people.txt`, one entity per line (`#` starts a comment); an id names one entity:

```
fact act=1
person 3 name="Hal" occupation=Farmer site=1 pos=0,0,0 tags=witness
creature 7 name="Rufus" tags=pet owner=3 pos=2,0,0
player 1 name="Ana" skill.trade=3 inv.meat=1
site 1 kind=Village
place 9 open_ground=30
fact completed=errantry/x::Q
```

Keys: `name`, `occupation` (an `Occupation` variant), `site`, `owner` (ids), `tags` (comma-separated), `kind`
(a site's `SiteKind`; anyone else's `kind` string), `open_ground` (blocks), `region`, `pos=x,y,z`, `related`
(comma-separated ids), `skill.<name>`, `inv.<kind>`; flags `alive` (the default), `dead`, `ours`.

`script.txt`, `<tick> <input>` per line (`#` comments; ticks may not go down):

| Input | Meaning |
|---|---|
| `talk <player> <npc>` | the player starts a conversation |
| `pick <player> <n>` | option `n` (from 1) of the player's offer |
| `leave <player>` | walks away from the conversation (`Left`) |
| `done <player>` | acknowledges the conversation's end |
| `event <Name> <field>=<wire>... [@<part>]` | a host or script event to the home, or to the part; a quest's own event by its qualified name (`Q::Go`) |
| `near <player> <entity>` | `PlayerNear` to the parts anchored on the entity |
| `kill <victim> [by <player>]` | `Killed`, then `RoleLost` for its roles |
| `lose <entity>` | `RoleLost` for the roles bound to it |
| `ambient <npc> <npc>...` | the ambient schedule casts its scenes from these idle people |
| `edit <dir>` | the script is edited to that directory's (relative to `<dir>`) |

Wire literals (roles, event fields): ints, floats with a `.`, `"strings"`, `true` / `false`, `None`,
`Some(v)`, `[a, b]`, `Occupation::Smith`, entity handles `Person(3)` / `Player(1)` (a player takes its name
from the people), structs `Item(kind="meat",count=1)`.

A state file holds the program it was saved with. It loads only under the `--mem` it was saved with (another
size is refused, exit 2). `--load` of a state saved by the same program resumes it exactly; a loaded state that already ran `main` runs it again. A state saved by a changed script is
**migrated** (spec §6.9): each top-level variable whose name and type still fit keeps its value, a new one
runs its initialiser, and a value whose variable is gone or no longer fits is kept aside as an orphan (a
later edit that brings the name back with a fitting type takes it again); queued events go to the new
handlers, parked handlers are cancelled. The migration prints to stderr:

```
carried: runs
initialised: bonus
orphaned: gold
orphan: gold: int = Int(7)
```

— `carried`, `adopted`, `initialised` and `orphaned` list names (a line only when its list is not empty),
then one `orphan:` line per value kept aside (older orphans too), then `events dropped: N`,
`handlers cancelled: N` and `timers dropped: N` when not 0. A file that cannot be loaded exits 2. A migration can be refused (a
value too large to read out, say): the run prints the reason and exits 2 without writing a state file, so
the old state stays valid for the old script.

A report prints as `error: handler <name> aborted: <fault or error text>` followed by one
`  at <file>:<line>:<col>` per location, or as `warning: event <name> queued` /
`error: event <name> dropped (<reason>)`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | finished (or `check` passed) |
| 1 | compile errors, or no `fn main()` |
| 2 | a task aborted, or the instance failed, or a state could not be loaded; a role refused, a quest that called `decline()` or `fail()`, a people or script file that does not parse |
| 3 | the tick limit was reached |
| 64 | usage error |
| 66 | unreadable directory or file |
