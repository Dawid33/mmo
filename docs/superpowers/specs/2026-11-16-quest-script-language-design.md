# Quest script language — MVP design

Date: 2026-11-16. Status: design approved in conversation; to be planned and built by another agent.
Working name: **quest script** (files `.qs`, crates `quest-*`). The name is a placeholder — the owner may rename it.

Marks used below: **[decided]** = the owner decided it; **[call]** = the designer's call, made in the
conversation's spirit — the builder may revisit a [call] if it turns out wrong, and should say so.

---

## 1. Intent

Errantry (`docs/game-design.md`) has fortress players whose troubles raise quests and adventurers who solve
them. Players will build their own quests and puzzles with in-world **quest blocks** — like Minecraft's
command blocks — that run code the player types in. This document designs that code's **language and its
runtime**, and the **MVP: the language standing alone**, tested extensively outside the game before any
integration.

Why a language of our own (research done 2026-11-16, see §4): the sim is deterministic with rollback
netcode. A script that runs inside the sim must give bit-identical results and step counts on native
(x86/ARM) and wasm32 in the browser, must be **pausable mid-execution at a fixed step budget per tick and
resumable next tick**, and its whole state must be plain data that is hashed, undone on rollback and saved in
world cuts. No existing embeddable language offers a pausable state that can be serialized and rolled back
(Rhai cannot pause, only abort; piccolo's heap is a GC arena we cannot hash/undo/save; a paused wasmi call is
opaque engine state).

The payoff: script authors write free-form code — loops, long computations — without thinking about ticks or
budgets. The VM stops at exactly the same instruction on every machine and continues next tick.

## 2. Goals, non-goals, anti-goals

**Goals (MVP)**
- A compiler and VM for the language in §5–§8, plus a standalone runner, in a **separate Cargo workspace**
  inside this repo (§10).
- Deterministic execution: same program + same inputs ⇒ same results, same fuel consumption, same state hash
  on native and wasm32.
- Pause/resume at an instruction boundary on fuel exhaustion; VM state is serializable, hashable and
  undoable to a checkpoint (the rollback bar: `hash(before) == hash(after undo)`, bit-exact).
- The in-game execution model (events, handler queue, promises, namespaces) implemented against a **stub
  host** so it can be tested standalone.
- Extensive standalone tests (§11).

**Non-goals (MVP)**
- Any game integration: no quest block entity, no sim events, no coordinator round trip, no Postgres, no
  Slint editor. `game`, `server`, `client` must not depend on the language (§10).
- Recursion, closures/lambdas, function values, generics with trait bounds, traits, glob imports.
- Sharing namespaces between players; reading other players' namespaces.
- Performance work beyond "reasonable": correctness and determinism first.

**Anti-goals (a failure even if it works)**
- Anything that makes the game workspace rebuild or wait on the language's builds.
- Any nondeterminism source: platform float functions, hash-seeded iteration, wall-clock time, a GC that runs
  on memory pressure or time, a fuel count that depends on the host.
- Errors that can be silently ignored.
- Behaviour that changes for already-compiled bytecode when the VM is upgraded (bytecode is versioned, §6.8).

## 3. Constraints from the project

- Determinism rules from `CLAUDE.md` apply to the VM: `libm` for every transcendental, no `HashMap`
  iteration in state (ordered or insertion-ordered maps only), randomness only from the host (in game: the
  sim's stateless `roll(tick, subject, salt)`), tunables as named consts.
- Hashing uses the `StateHash` trait (`crates/state-hash`), never std `Hash` or serialized bytes. The VM may
  depend on `crates/state-hash` by path (it has no game dependencies).
- `game`'s `#[rollback]` macro cannot be used outside `game` (it expands against `game`'s crate-root
  re-exports). The VM has its **own undo log**; the later integration bridges it to the region's undo.
- Region threads are single-threaded; a block's scripts never run in parallel with themselves.
- Clients predict regions and roll back on misprediction (`game::Region::reconcile`). In game the VM runs on
  server and predicting clients alike — hence the determinism and undo requirements. A server event the
  client did not predict (e.g. a promise resolution) costs one rollback + replay, like another player's input.
- `CLAUDE.md`: keep the justfile in sync; every `just` build/test is CPU-capped via `{{cap}}`.

## 4. Approaches considered

1. **Embed an existing language** (Rhai, Starlark, piccolo/Lua, Luau via mlua, wasmi running compiled
   modules). Rejected: none can pause at a fuel limit into a state we can hash, undo and save; C-based ones
   (Luau, Lua, QuickJS) don't build cleanly for `wasm32-unknown-unknown`; fuel/op counts drift across
   library versions.
2. **Own language, tree-walking interpreter.** Rejected: evaluation lives on the Rust stack, so it can't stop
   mid-expression and resume next tick without contorting into continuation-passing style.
3. **Own language compiled to wasm, run in wasmi.** Rejected: paused-call state is opaque engine state;
   copy-on-write values, error sets and parked handlers would all be built on raw linear memory.
4. **Own language, own compiler to bytecode, own VM.** **Chosen [decided].** Full control of state layout
   (hashable/undoable/serializable), fuel defined per instruction by us, versioned semantics.
5. **Own language compiled to wasm and run by the browser's native engine** (and wasmtime/wasmi on native),
   with fuel counters and resumable state machines emitted by our compiler — possible precisely because of no
   recursion + static frames. **Deferred [decided]**: several engines to keep bit-identical, a JS boundary
   crossing per host call in the browser worker, a module instance (≥ 64 KiB) per block, wasmtime's compile
   cost — for scripts whose fuel budget caps them at a few thousand instructions per tick. The interpreter's
   state is laid out as flat memory (§6.3) so this backend can be added later with the same state format.

Repository placement: **(A) a separate Cargo workspace in this repo [decided]** vs (B) a root-workspace member
under conventions. (B) fails because the root sets `[resolver] feature-unification = "workspace"`
(`.cargo/config.toml`) — any new feature a language crate enables on a shared dependency rebuilds the game —
and shares `Cargo.lock` and the `target/` build lock.

---

## 5. The language

Syntax is Python's (indentation, `:` blocks, `#` comments, `and`/`or`/`not`, f-strings, `match`/`case`).
Semantics are Rust's (block scope, shadowing, value types, enums, exhaustive match, inferred static types).

### 5.1 Bindings and scope [decided]

```python
let x = 1          # new binding; shadows any earlier x in this or an outer scope
x = 2              # reassigns the nearest existing x (compile error if none)
let x = "now a string"   # shadowing may change the type
```

- **Block scope**: `if`/`elif`/`else`, `while`, `for`, `match` arms and function bodies open scopes; inner
  scopes see outer bindings. (Unlike Python, a binding made inside an `if` is gone after it.)
- Three kinds of variables:
  1. **Locals** — inside functions and blocks.
  2. **Top-level `let`** — written at a module's top level, outside any function. Same scoping rule (the
     outermost scope), not called "global". These are the block's **persistent state**: they survive between
     events, are part of VM state (hashed, undone, saved). Initialised once, in source order (modules in
     dependency order, then declaration order) **[call]**, when the program is first instantiated; the
     initialiser runs under fuel like a handler.
  3. **External ("global")** data — lives outside the VM (in game: Postgres, via the coordinator); reached
     only through promises (§8). Not a variable kind in the grammar.
- No `mut` keyword: every binding is reassignable **[call]**.

### 5.2 Types and inference [decided]

- Statically typed, inferred; annotations only where inference can't decide (`let n: int = …`,
  `fn f(x: int) -> str`).
- Function parameter and return types are inferred from the body. Because there is no recursion (§6.1), the
  call graph is a DAG: infer **callee-first** (reverse topological order). A parameter the body leaves
  unconstrained, or uses at two incompatible types, is a compile error asking for an annotation — no generic
  user functions in the MVP.
- Struct fields always carry type annotations (as in Rust).
- Built-in types: `int` (i64), `float` (f64, §5.9), `bool`, `str`, `list[T]`, `dict[K, V]`, `Option[T]`,
  `Promise[T]`, unit `()`; user `struct`s and `enum`s; error sets (§5.7).
- No implicit numeric conversion: `int` ↔ `float` via `float(i)`, `int(f)` (truncates; NaN/out-of-range =
  fault) **[call]**.

### 5.3 Values, copies and references [decided]

- **Everything is a value.** Assigning or passing a list, dict, string or struct copies it (copy-on-write
  under the hood, so copies are O(1) until written).

```python
let a = [1, 2]
let b = a          # copy
b.append(3)        # a == [1, 2], b == [1, 2, 3]
let r = &a         # reference
r.append(9)        # a == [1, 2, 9]
```

- **References `&x` are second-class** **[decided: `&` exists; the restrictions are the designer's call,
  accepted in conversation]**: they may be held in a local or passed as an argument; they may **not** be
  stored in a list/dict/struct/top-level variable, nor returned. Hence no cycles, no dangling, no lifetimes
  and no borrow checker.
- References may point into places: `&quest.rewards[2]`, `&ledger.entries`. A reference is a **path**
  (root slot + field/index steps) re-resolved on each use; if the path no longer exists (list shrank) the
  access is a deterministic fault.
- Field access and method calls auto-dereference. Whole-value assignment through a reference is `*r = v`
  **[call]**.
- A reference local or parameter is never re-pointed: `r = &b` is `ref-reassign` (`b` could die before `r`). Bind
  a new one with `let r = &b`, whose scope ends with its block **[as built]**.
- No `&` vs `&mut` in the MVP — every reference can write.
- Mutating a collection while iterating it is defined behaviour (iteration is by index / insertion position
  at each step), bounded by fuel.

### 5.4 Functions and control flow

```python
fn reward_for(stage):              # types inferred
    if stage == 0:
        return 0
    elif stage < 3:
        return 10 * stage
    else:
        return 50

fn main():
    let total = 0
    for s in range(5):
        total = total + reward_for(s)
    let i = 0
    while i < 10:
        i = i + 1
        if i == 7:
            break
    print(f"total {total}, stopped at {i}")
```

- `fn`, not `def` [decided]. `if`/`elif`/`else`, `while`, `for x in <list|dict|range>`, `break`,
  `continue`, `return`, `pass` [call for the last few].
- `for k, v in dict.items():` iterates in **insertion order** (as Python) **[call]**.
- No recursion (direct or mutual) — compile error naming the cycle [decided].
- Future (not MVP) [decided as possible later]: (a) functions passed as compile-time-known parameters,
  monomorphised per call site (Rust `F: Fn`); (b) real function values, checked against a conservative call
  graph (an indirect call of type `fn(A)->B` may reach any address-taken function of that type).

### 5.5 Structs and methods

```python
struct Ledger:
    owner: str
    entries: list[int]

impl Ledger:
    fn total(self) -> int:          # self by value (a COW copy — cheap)
        let t = 0
        for e in self.entries:
            t = t + e
        return t

    fn add(&self, n):               # &self: may mutate
        self.entries.append(n)

let l = Ledger(owner="ana", entries=[])
l.add(5)
```

Rust's struct/impl split, Python's indentation; no classes, no inheritance [decided]. Construction syntax
`Name(field=value, …)` with all fields required **[call]**.

### 5.6 Enums and match [decided: Rust enums + match]

```python
enum Stage:
    Asked
    Accepted(tick: int)
    Done

match stage:
    case Stage::Asked:
        say(p, "Will you help?")
    case Stage::Accepted(tick) if tick > 100:
        say(p, "Still waiting on you.")
    case Stage::Accepted(_):
        pass
    case Stage::Done:
        say(p, "Thanks again.")
```

- Python 3.10's `match`/`case` keywords, Rust's semantics: exhaustive, patterns for literals, `_`,
  bindings, enum variants with positional or named fields, struct patterns, `|` alternatives, `if` guards.
- `::` for module paths and enum variants, `.` for fields and methods **[call]**.
- `Option[T]` is an ordinary enum: `Some(x)` / `None`.

### 5.7 Errors [decided: Zig semantics, Rust enums/match, Python-ish syntax]

- **Any name is a valid error**, raised without declaration; it may carry data:
  `raise ChestEmpty`, `raise NotEnough(have=3)`. Error names are **global** across the whole program (Zig): a
  name means the same error everywhere; one name used with two different data shapes is a compile error.
- **Error sets** are optional, named, and union with `|` (Zig's `||`):

```python
error ChestError = ChestEmpty | NotEnough(have: int)
error QuestError = ChestError | QuestClosed | NotYourQuest
```

- `raise` returns the error from the current function (no unwinding semantics beyond that).
- Every function's error set is **inferred** (union of what it raises and what it propagates). An optional
  annotation pins it: `fn pay(p) -> Item raises QuestError:` — raising outside the pinned set is a compile
  error.
- A fallible result must be handled — never silently ignored. Three forms:

```python
let gold = take_gold(c, 20)?            # propagate (adds the callee's set to ours)
let gold = take_gold(c, 20) else 0      # fallback value
let gold = take_gold(c, 20) else return # fallback: diverge
let gold = take_gold(c, 20) else err:   # fallback block; must yield a value or diverge
    log(f"failed: {err}")
    return

match take_gold(c, 20):
    case Ok(gold):
        give(p, gold)
    case Err(NotEnough(have)):
        say(p, f"Only {have} left.")
    case Err(ChestEmpty | QuestClosed):
        pass
```

- `match` on a fallible value is **exhaustive against the inferred set**; an arm naming an error that cannot
  occur there is a compile error (catches typos and stale arms). A bare error name as a pattern there is
  `Err(name)`: `case NotEnough(have):` reads `case Err(NotEnough(have)):` **[as built]**.
- `?` and `else` work on `Option` too (`None` propagates / falls back).
- A "did you mean" **warning** when a raised error name is never matched or declared anywhere and is within
  edit distance 2 of one that is **[call]**.
- An error escaping a handler (§7) or `main` stops it, undoes its changes to top-level state, and reports the
  error name, its data and the **error return trace** (each `?`/`raise` site it passed, with file:line).

**Faults vs errors [call]:** programming faults are not in error sets and cannot be caught: index out of
range, missing dict key via `d[k]` (use `d.get(k)` → `Option`), integer overflow, division by zero, invalid
reference path, heap cap exceeded, bad `float→int`, a value too large to compare, hash or format in one step
(`ValueTooLarge`: more than a fixed number of words visited — shared nested values make such a walk exponential). A fault aborts the running handler exactly like an
escaping error (undo + report with a stack trace). Fuel exhaustion is **not** a fault (§6.2).

### 5.8 Collections and strings [decided: lists and dicts with Python literal syntax]

- `[1, 2, 3]`, `{"a": 1}`, indexing `x[i]`, `in`, methods (`len`, `append`, `pop`, `insert`, `remove`,
  `get`, `keys`, `values`, `items`, …). Slicing `x[a:b]` **[call: include]**.
- Dict keys: `int`, `bool`, `str`, and enums/structs composed only of those **[call]**. Iteration in
  insertion order.
- `str`: immutable UTF-8 values; f-strings `f"{x}"`; indexing by character is **not** provided in the MVP
  (methods like `split`, `starts_with`, `len` in bytes or chars — builder's choice, documented) **[call]**.
- Length is the method `x.len()` on `str`, `list` and `dict`, as in Rust; there is no free `len(x)` function
  **[decided 2026-11-16]**. Bool literals are lower case: `true`, `false` **[decided 2026-11-16]**.

### 5.9 Numbers [decided: floats allowed, deterministic]

- `int` = i64 with checked arithmetic (overflow = fault). Integer `/` truncates toward zero and `%` is Rust's
  remainder (Rust numeric semantics, Python syntax) **[call]**.
- `float` = f64. Basic IEEE ops are bit-identical across x86/ARM/wasm; every transcendental goes through
  `libm`; **NaN is canonicalised** after every float-producing instruction (so state hashes match). No FMA
  contraction. (The game's physics floats are `f32` + `libm`; scripts use f64 and convert at the host
  boundary **[call]**.)

### 5.10 Modules [decided: imitate Rust, in MVP for standalone]

```
my_quest/
  main.qs          # crate root (standalone: defines fn main())
  rewards.qs       # mod rewards
  npc/
    mod.qs         # mod npc
    dialogue.qs    # mod npc::dialogue
```

```python
mod rewards
mod npc
use rewards::give_gold
use npc::dialogue::{greet, farewell}
use rewards::give_gold as pay

pub struct Ledger:
    entries: list[int]
```

- `mod`, `use`, `pub`, `crate::`, `super::`, `self::`, `as`; items private by default; a file is a module.
  No glob imports in the MVP **[call]**. Module `use` cycles are allowed (as Rust); the no-recursion rule is
  about the call graph, not the module graph.
- Error names stay global across modules.
- How modules are stored and shared in game (likely per namespace) is **open** — not part of the MVP.

---

## 6. Execution model

### 6.1 Static frames, no recursion [decided]

- The call graph is a DAG (recursive **calls** are rejected; recursive data and dialogue are not). Each
  function gets **one fixed frame** (a fixed number of value slots) allocated up front; the maximum call
  depth and total frame memory are known at compile time (as early Fortran).
- Only **one handler runs per block at a time** (§7), so one frame per function suffices. A handler parked at
  `await` has its live frames copied into the heap and restored on resume (§8.2).

### 6.2 Fuel [decided]

- Every bytecode instruction costs fuel, by a fixed table that is part of the bytecode version; host calls
  cost a fixed amount each. Fuel is the only budget — never wall-clock time.
- The host gives a block a fuel budget per tick. When it runs out, the VM stops **at an instruction
  boundary**; the state is plain data; next tick it continues. On every machine the stop happens at the same
  instruction.
- Infinite loops are harmless: they spend their block's budget each tick. (In game, a block that spends its
  whole budget for many consecutive ticks is reported to its owner — integration detail.)

### 6.3 Flat memory and heap [decided: flat memory]

- **All mutable VM state lives in one flat, fixed-size linear memory** (a byte buffer per program instance):
  the static frames, top-level variables, the heap, the event queue, parked handlers and promise records.
  Not Rust objects. Reason: a **wasm backend is planned for later** [decided] (scripts compiled to wasm modules
  run by the browser's engine / wasmtime, with compiler-inserted fuel counters and resumable state-machine
  functions); with one shared memory layout both backends produce the **same state bytes** — same hashes, same
  saves — and the interpreter's golden tests check the wasm backend. Hashing, saving and snapshotting become
  plain byte operations.
- The memory layout (header, frame area, globals area, heap, queue, …) is a versioned format, part of the
  bytecode version (§6.8). Integers little-endian, fixed widths, no padding garbage (zero-filled), so the
  bytes are identical on every platform.
- The heap is our own allocator inside that memory (deterministic: same request sequence ⇒ same addresses),
  holding lists, dicts, strings and structs, reference-counted (counts stored in the memory), copy-on-write.
  Second-class references (§5.3) make cycles impossible, so refcounting frees everything; no tracing GC.
- The memory size is fixed per instance (a named tunable; no growth). Exhausting the heap is a fault.
- Undo (§6.6) logs `(offset, old bytes)` of writes into this memory.

### 6.4 VM state

The state of one program instance is the immutable program plus its flat memory (§6.3) — conceptually:

```rust
struct Vm {
    code: Arc<Program>,   // immutable compiled bytecode + layout, version-stamped
    mem: Box<[u8]>,       // ALL mutable state: pc/call chain, frames, globals, heap, queue,
                          // parked handlers, promise table, counters, status
    undo: UndoLog,        // §6.6 — not part of the hashed/saved state
}
```

`StateHash` hashes the program's identity (version + content hash) and `mem`; saving writes the same. The
hash must match on native and wasm32.

### 6.5 Determinism checklist (the VM must satisfy all)

`libm` only; NaN canonicalised; no `HashMap`/`HashSet` in state (or never iterated); insertion-ordered dicts
with deterministic layout; allocator addresses deterministic; fuel table fixed; no time, no thread, no
randomness except a host-provided deterministic `roll`; host calls are pure functions of (VM-visible inputs,
host state passed in).

### 6.6 Undo / rollback [decided: VM integrates with rollback]

- API shape **[call]**: `vm.checkpoint() -> Checkpoint`, `vm.rollback_to(cp)`, `vm.forget_before(cp)`
  (drop undo history once confirmed). In game the region checkpoints per event/tick and rolls back on
  misprediction; standalone tests do the same.
- The VM records the old value of every slot/heap write in its undo log. Each instruction writes a bounded
  number of slots, so **one tick's undo log is bounded by that tick's fuel**.
- Rollback bar: for any program and any point, `hash(state) == hash(state after run-then-rollback)`, bit-exact.
- A handler that faults or lets an error escape is rolled back to the start of its **current segment** — the
  same mechanism. A *segment* is the stretch of a handler between its start / its last `await` and the next
  `await` / its end **[call]**. At an `await` the segment **commits**: other handlers may then run and observe
  what it wrote (as in JS), so it can no longer be undone by this handler's later fault. A fault after an
  `await` undoes only the segment since that `await`. So the abort log is per segment and is cleared at each
  commit; it is separate from the sim-rollback undo log (which is linear and covers everything, §6.6 above).
- **Host effects are buffered per segment [call — follows the conversation's "commands returned to the sim"
  model]:** game calls that change the world (`say`, `give`, `chest.take`, tag writes, …) and promise requests
  are recorded as commands in VM state and handed to the host only when the segment commits (at `await` or
  handler end). An aborted segment's commands and promise requests are discarded — nothing happens in the
  world without the state change that went with it. Consequences the builder must keep: a long handler that
  pauses on fuel across ticks emits its commands only when the segment ends; host *reads* within a segment see
  the world as of the current tick, not the segment's own buffered commands (no read-your-writes overlay in
  the MVP); world calls that can fail at apply time (e.g. `chest.take` on an emptied chest) report through the
  host's error channel / a later event, or — where the script needs the result inline — are promises.
- **A fallible host command is checked when it is issued [as built, Task 37b]:** the host answers `Ok` or one of
  its declared errors from the current tick's state (a pure read). An error takes the script's fallible path at
  once and nothing is buffered; `Ok` buffers the effect as above. A failure at apply time (the world changed
  within the segment) still goes to the host's error channel, never back to the script.

### 6.7 Determinism across platforms

Golden tests: a corpus of programs run for N ticks with fixed fuel produce recorded `(state hash, fuel used,
output)` traces on native; the same corpus on wasm32 must reproduce them exactly (§11).

### 6.8 Bytecode versioning

Every compiled program carries a bytecode/semantics version. The MVP VM refuses a mismatched version. Policy
for the game (keep old interpreters vs migrate idle blocks) is decided at integration; nothing in the MVP may
make either impossible (e.g. serialized VM state also records the version).

### 6.9 Editing a script: state is kept; "reset" re-initialises [decided]

When an admin edits a script instance's program (editor mode, game-design §10), the instance's **saved state is
kept**. A **reset** (in game a button on the script block, labelled "Reset"; standalone a CLI flag / API call)
**re-initialises the whole script instance** [decided]: all top-level variables run their initialisers again,
the queue is emptied, parked handlers and pending promises are cancelled (answers discarded; committed writes
stay — writes are final, §8.2), orphaned state (below) is gone. Afterwards the instance is exactly as if the current script had just been placed.

How the old state meets edited code **[call]**:

- The saved state records each top-level variable by its **qualified name** (module path + name) with a
  self-describing value (field/variant names, as for namespace values, §8.2) — slot offsets alone are not
  enough, since the new program lays memory out differently. Loading new code **migrates** the state:
  - same name, value structurally compatible with the new type (the §8.2 rule) → value carried over;
  - name new in the new code → its initialiser runs;
  - name gone, or its type no longer compatible → the old value is kept aside as **orphaned state**
    (invisible to the script, listed to the admin with its name and type) and the new variable, if any, runs
    its initialiser. Orphans disappear on reset (or a later edit that brings the name back with a
    compatible type takes the value again).
- **Queued events** stay queued and are delivered to the new code's handlers (dropped if no handler hooks
  that event any more).
- **Parked handlers** belong to the old code and cannot resume in the new: they are cancelled; their pending
  promises' answers are discarded (writes already committed stay — writes are final, §8.2).
- A handler mid-segment when the edit lands has its segment aborted (§6.6), as if it faulted; the edit takes
  effect at a tick boundary.
- Standalone: `quest run` can load a saved state into a changed program, so migration is testable (§11).

---

## 7. Events and handlers [decided]

In game, scripts are event-driven (Factorio-like): a script hooks into events such as a player entering an
area or clicking the script block. `main()` exists only for standalone runs and tests.

```python
@on(PlayerClicked)
fn greet(e):                     # e: PlayerClicked, inferred from the decorator
    say(e.player, "Well met.")

@on(PlayerEntered)
fn warn(e):
    if e.player.has_tag("outlaw"):
        say(e.player, "Begone.")
```

- `@on(EventType)` registers a handler; event types are host-defined structs (the standalone host defines a
  small set — §9). Several handlers may hook one event.
- **Handler order is not guaranteed to authors** [decided] — scripts must compose without depending on each
  other. The implementation's order is deterministic (e.g. by module path + declaration order) and **not**
  shuffled [decided].
- **One handler runs at a time per script instance** [decided]. Handlers run to completion, pausing only on fuel
  exhaustion (continuing next tick) or at `await` (§8). There is no `wait`/`sleep` inside handlers; state that
  must last across events lives in top-level variables and is advanced by later events.
- **Queue** [decided]: an event that arrives while the instance is still busy (a handler unfinished from an
  earlier tick) is queued. Any event queued ⇒ a **warning** to the owner. The queue is small (a named
  tunable, default **[call]** 4); when full, the **newest** event is dropped and a **script error** is shown to
  the player.
- A resolved promise's continuation (§8.2) enters the same queue as an event.

## 8. External data: promises and namespaces [decided]

### 8.1 Namespaces

- **A namespace belongs to one quest instance [changed 2026-11-16, quests-in-the-world §2; it beats the
  earlier player-owned rule]:** the system makes it for the instance and deletes it with the instance; no
  player creates, owns or deletes one. Its script instances are that quest's home and parts, in any regions.
  It holds durable data only (what survives a repeatable reset); quest state lives in the instance (§5.12).
- A script instance reads and writes only its own quest's namespace **[call, accepted]**. Sharing one between
  quests is future work.
- In game the coordinator owns namespace records and is the single writer of their data (no table two hosts
  can both write). Deleting a namespace makes every later or in-flight call from its instances fail with
  `NamespaceGone`.

### 8.2 Promises (JS-like) [decided]

```python
@on(ItemDelivered)
fn claim(e):
    match await world.compare_and_set("wyrm_quest", "open", str(e.player.id)):
        case Ok(_):
            give(e.player, await chest.take("gold", 20)?)
        case Err(Changed(winner)):
            say(e.player, "Another claimed it first.")

@on(PlayerClicked)
fn ask(e):
    let slain = await world.get("dragons_slain")?
    let p = world.get(f"renown/{e.player.id}")   # hold a promise, await later
    say(e.player, "Let me check the ledgers…")
    let r = await p else 0
```

- Operations on the instance's namespace, each returning `Promise[Result]`: `get(key)`, `set(key, v)`,
  `add(key, n)`, `compare_and_set(key, expected, new)` (error `Changed(current)`), `delete(key)`. Keys are
  strings; values are any script value (serialized). A `get` whose stored value has another type than the
  receiving binding fails with `WrongType` **[call]**. `await all([a, b])` **[call]**.
- **Stored values are self-describing [call]:** shared data is written by instances running *different*
  programs, so a stored struct/enum carries its field and variant **names** with their values (not slot
  indices or a program-local type id). A `get` checks the stored value structurally against the receiving
  type — same field names with matching types, the variant present in the receiving enum — and fails with
  `WrongType` otherwise. Type names themselves are not compared, so two programs declaring the same shape
  interoperate. (Declaring shared types once per namespace is a possible later refinement.)
- Rejections are ordinary errors: `NotFound`, `Changed`, `Timeout`, `Lost`, `NamespaceGone`, `WrongType` —
  handled with `?`, `else`, `match`. `compare_and_set` needs `expected` and `new` of one type (`str(e.player.id)`
  against a string key, above).
- **Base promise errors [as built]:** every awaited result (and `await all`) can fail with `Timeout`, `Lost` and
  `WrongType`, which the VM makes itself whatever the host function declares; an exhaustive `match` needs arms
  for them (or `Err(_)`).
- **Promise error sets by origin [MVP plan decision 21]:** `await` of a host promise call, or of a local assigned
  only from host calls with the same `raises`, raises exactly that list (plus the base errors); `await` of any
  other promise (a parameter, field, list element, global, a function's result, `all`) raises the union of
  every host promise function's errors — handle `Err(_)` there.
- **Mechanics**: issuing a promise appends a request record (id from the VM's counter — identical everywhere)
  to VM state. Only the server acts on it, after the tick is final; clients send nothing. The answer re-enters
  the sim as a stamped event at one tick on every machine and lands in the instance's queue.
- **`await` parks**: the handler's live frames are copied to the heap; the instance proceeds with its queue; on
  resolution the continuation is queued and resumes with frames restored. Parked handlers are capped per
  instance (tunable). Top-level variables may have changed across an `await` (as in JS) — only at visible
  `await`s.
- **Writes are final** [decided]: a write's promise resolves only after the coordinator committed it to
  Postgres. A host crash may roll an instance's own state back to its last cut while external data keeps the
  write — scripts treat external data as the source of truth for anything that must not be undone (who won,
  what was paid). The promise id is the idempotency key (a resend after recovery is harmless). Every request
  times out (`Timeout`) after a fixed number of ticks.

## 9. Host interface and standalone runner

- The VM talks to the world only through a **host trait** **[call: shape left to builder]** covering: game
  calls (`say`, `give`, tags, chest access, `roll`…), event delivery, promise request/resolution, the
  fuel budget per tick, warnings and errors for the owner.
- **Fallible host calls [as built, Task 37b]:** a host query or command may declare errors (`raises`). It is
  handled like any fallible call (`?`, `else`, `match`), its error set exactly the declared list; an error it
  does not declare is the fault `HostError`. A condition may use one only with the error handled
  (`q() else false`); an escaping one is `impure-condition`. A query about an entity out of reach raises
  `OutOfReach` this way.
- **The compiler learns the host API from a host description [call]:** a signature table (functions and
  their types, event structs and fields, the `world.*` operations, built-in error names) passed to
  `quest-lang` at compile time; `quest-lang` has no host knowledge of its own beyond the core built-ins
  (`print`, `range`, collections, `Option`, `Promise`). The table has its own version; a compiled
  program records the host-API version next to the bytecode version, and both are checked when the VM loads it.
  The standalone host ships the MVP's table.
- **Standalone host** (the MVP's only implementation): in-memory players/items/chests/tags; `say` and
  `print` write to stdout (and to a capture buffer for tests); an in-memory namespace store with configurable
  latency (ticks) and **failure injection** (`Timeout`, `Lost`, `Changed`, `NamespaceGone`); a small set of
  event structs (`PlayerClicked`, `PlayerEntered`, `ItemDelivered` at least).
- **The stub's host API as built (version 6):** `Entity { id }` is its "any entity" type (`ItemGiven.to`,
  `Killed.victim`; `place` at any entity); `Person.distance_to(other) -> int raises OutOfReach` is its
  fallible query (either entity missing from `people.txt`); a role's value is checked by struct name as well as
  shape, so `Person(3)` never fills a `Creature` role.
- In `main()` tests may call handlers directly (they are ordinary functions) and may `emit(Event)` to go
  through the real queue **[call]**; `assert(cond)` / `assert_eq(a, b)` faults with a message.
- **CLI `quest`** **[call: names]**: `quest check <dir>` (compile only, diagnostics), `quest run <dir>`
  (instantiate, run `main()` tick by tick under a fuel budget until done), flags for fuel per tick, tick
  limit, and a `--trace` printing per-tick hash/fuel. A `quest test <dir>` running every `#[test]`-style
  function is welcome but optional.
- Diagnostics: file:line:col, Rust-style messages with the source line and a caret.

## 10. Repository layout and isolation [decided: separate workspace]

```
quest/                     # its own Cargo workspace: Cargo.toml [workspace], Cargo.lock, target/
  crates/quest-lang/       # lexer (indentation), parser, modules, name resolution, type + error-set
                           # inference, exhaustiveness, no-recursion check, frame sizing, bytecode emit
  crates/quest-vm/         # bytecode, VM, heap, fuel, undo log, flat memory + StateHash, queue, promises,
                           # the dialogue graph, conversation and scene runners
  crates/quest-stub/       # the standalone host: its API, the in-memory world, the runner
  crates/quest-cli/        # the `quest` binary
  crates/quest-corpus/     # the .qs corpus (corpus/run, corpus/fail) and its goldens
```

**As built: five crates, not three** — the standalone host and the corpus are crates of their own beside the CLI
(both build for wasm32, where the corpus's goldens are reproduced).

- Root `Cargo.toml` gets `exclude = ["quest"]`. The language workspace inherits the root
  `rust-toolchain.toml` and `.cargo/config.toml` (harmless). Its `target/` is `quest/target` (already covered
  by the root `.gitignore`'s `target`).
- Only allowed path dependency into the game tree: `crates/state-hash` (+ its derive). Nothing in the root
  workspace depends on `quest/`.
- **`quest-vm` and `quest-lang` must both build for `wasm32-unknown-unknown`** (engine-agnostic: no Bevy, no
  tokio) **[call]**. Where scripts get compiled in game (server only, or also a browser editor) is not decided,
  so the compiler is kept able to run anywhere and **deterministic**: the same source and host description
  give byte-identical bytecode on every platform (no hash-ordered iteration anywhere in the compiler).
- Justfile recipes (all through `{{cap}}`, run from `quest/`): `quest-check`, `quest-test`, `quest-run <dir>`,
  `quest-wasm` (the wasm32 determinism corpus, reusing the wasm-bindgen test runner the way
  `determinism-wasm` does). Add a short `CLAUDE.md` paragraph pointing at the workspace and recipes.
- Integration later: implement the host trait in `game`/`server` and bridge the undo log to the region's undo
  (`docs/superpowers/plans/2026-11-17-quest-script-completion.md` Part 4). How the game links the crates is that
  plan's gate **G2**, open until Dawid decides it; its recommendation replaces the move named here: `quest/` stays
  its own workspace (tests, CLI and lockfile untouched), `crates/game` takes `quest-vm` as a path dependency with
  the root `exclude` kept, and only `server` links `quest-lang`.

## 11. Testing (the MVP is judged by this)

- **Compiler unit tests** per phase: indentation lexing, parsing, scoping/shadowing, inference (incl. the
  "needs an annotation" errors), error-set inference and pinning, exhaustive match (missing arm, impossible
  arm), recursion rejection (direct and mutual), reference restrictions (store/return refused), modules and
  visibility.
- **Program corpus** (`.qs` files with expected output/diagnostics): every example in this document compiles
  and behaves as described.
- **Fuel/pause**: a program run with fuel N per tick yields identical final state and output to the same
  program run with unlimited fuel; pause points land at identical instructions across runs.
- **Rollback**: property tests — random programs/inputs, checkpoint, run k ticks, roll back, hash equal;
  run → rollback → re-run gives identical hashes.
- **Serialization**: serialize mid-execution (incl. parked handlers and pending promises), deserialize, resume
  — identical to an uninterrupted run.
- **Cross-platform determinism**: golden (hash, fuel, output) traces recorded on native reproduced on
  wasm32; golden **bytecode** for the corpus identical on native and wasm32 (the compiler is deterministic).
- **Edits**: save state, change the program (add, remove, retype top-level variables; remove a handler),
  load — carried-over, initialised and orphaned variables as §6.9; reset re-initialises everything (variables, queue, parked handlers, promises, orphans).
- **Flat memory**: state bytes identical on native and wasm32 after the same run; allocator addresses
  identical for identical request sequences.
- **Segments**: a fault after an `await` undoes only the segment since it; an aborted segment's buffered
  commands and promise requests never reach the host.
- **Queue/promises**: queue warning, overflow drops the newest with an error; promise latency, every
  rejection kind, `await` parking/resume order, parked cap, timeout.
- **Faults**: each fault kind aborts and undoes the handler, reports a trace.

## 12. Left to the builder

Bytecode format and register/stack design (register-style over frame slots is suggested); exact fuel table;
tunable defaults (fuel per tick, heap cap, queue length, parked cap, promise timeout); the host trait's
precise shape; string API details; diagnostics formatting; internal crate structure. Any **[call]** above may
be revisited with a note in the plan.

## 13. Open for later (not MVP)

The **wasm backend** (compile scripts to wasm modules with inserted fuel counters and resumable
state-machine functions, all state in the module's linear memory in the §6.3 layout; fixed memory, NaN
canonicalised by emitted code, `libm` compiled in, no relaxed SIMD) — planned, after the interpreter;
game integration — now planned in `docs/superpowers/plans/2026-11-17-quest-script-completion.md` Part 4; how
modules live and are shared in game; namespace sharing; function
parameters (monomorphised) and function values; read-only references; generics/traits; bytecode upgrade
policy in game; the language's real name.
