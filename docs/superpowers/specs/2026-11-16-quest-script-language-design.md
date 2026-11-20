# Quest script language — MVP design

Date: 2026-11-16. Status: design approved in conversation; to be planned and built by another agent.
Working name: **quest script** (files `.qs`, crates `quest-*`). The name is a placeholder — the owner may rename it.

Marks used below: **[decided]** = the owner decided it; **[call]** = the designer's call, made in the
conversation's spirit — the builder may revisit a [call] if it turns out wrong, and should say so.

**Game direction changed the same day** (`docs/game-design.md`, revision 2026-11-16): the standalone MVP
stands, but its in-game assumptions changed. Quests come from packages, not from players typing into
blocks. Namespaces belong to one quest instance, not to players (§8.1). "Per block" rules become per
script instance, and a script block is an admin's access point. See
`2026-11-16-quests-in-the-world-design.md` §2 and `2026-11-16-content-packages-design.md`.

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

### 5.11 Dialogue [model decided 2026-11-16; syntax decided 2026-11-16]

**Folded 2026-11-17** from the accepted construction design (`2026-11-16-quest-construction-design.md`, accepted
by Dawid 2026-11-16; worked examples `2026-11-16-quest-examples.md`). That document keeps the research and the
reasons; this section is the rule. Decided at the fold (Dawid, 2026-11-17): **conversations are private** (one
talker; others nearby hear nothing); **a bark is a one-actor `scene`**, and the `dialogue name(npc: Person)` form
stays accepted as a bark; the namespace keeps the name `world.*` (the examples' `ns.*` reads as `world.*`).

**Calls made at the fold (2026-11-17), where the design left a detail open — change any on review:** `EndReason`
is `Finished | Left | Timeout | TooDeep | Edited | OutOfRange`; an exit's type is `Exit`; a scene has no `detour`;
a role's `where` may name only earlier roles; new diagnostic codes `bad-quest-option`, `bad-objective-call`,
`bad-part-item`, `part-write`, `scene-follow-up`, `mirror-size` (warning); live timers are bounded by
`TIMERS_MAX`; a line's character count for its duration counts f-string holes by their source length.

**Decided (owner, 2026-11-16):**
- **Description, not execution.** A dialogue in quest script declaratively describes a tree of nodes, lines
  and choices, attached to NPCs. The VM does not execute it. The compiler turns it into **data** in the
  `Program`, and the **region's Rust code runs the conversation** when a player talks to an NPC.
- **Conditions are pure VM functions.** Each condition (a node's `when`, an option's `if`, a line's `when`),
  each f-string substitution and each `emit` argument compiles to a small read-only function, which the
  runner calls.
- **Effects are events.** A dialogue never changes state itself. It `emit`s events to its script instance,
  and ordinary `@on` handlers (§7) do the work.

Background and the formats it borrows from (Yarn Spinner 3's node groups, saliency and line ids; ink's
weave): `2026-11-16-dialogue-and-quest-structure-research.md`. Worked examples, including Yarn Spinner's own
samples converted: `2026-11-16-dialogue-examples.md`.

```python
let accepted = false
let dog_name = "Rufus"

event Accepted:
    player: Player

@pick(best)
dialogue farmer_talk(p: Player, npc: Person) for farmer:
    node start:
        npc: "Have you seen my dog? He ran off toward the hills."     @farmer.start
        choice:
            "I'll find him." @farmer.accept if not accepted:
                emit Accepted(player=p)
                npc: "Bless you. Bring him back before dark."          @farmer.thanks
            "What does he look like?" @farmer.ask_dog:
                npc: f"Brown, one ear up. He answers to {dog_name}."   @farmer.dog_desc
                -> start
            once "Is that blood on your sleeve?" @farmer.blood:
                detour farmer_secret
            "Goodbye." @farmer.goodbye
        npc: "Safe roads."                                             @farmer.bye

@on(Accepted)
fn accept(e):
    accepted = true
    give(e.player, Item(kind="dog whistle", count=1))

dialogue greeting(p: Player, npc: Person) for innkeeper:
    node hello when p.has_tag("outlaw"):
        npc: "Keep walking."                                           @greet.outlaw
    node hello when once per p:
        npc: "A new face! Welcome."                                    @greet.first
    node hello:
        => npc: "Morning."                                             @greet.a
        => npc: "Weather's turning."                                   @greet.b
        => npc: "You again?"                                           @greet.c when once per p
```

**A dialogue body is steps, not statements.** It holds nodes, and nodes hold steps. There is no `let`,
assignment, `if`/`while`/`for`, `match` or call in a dialogue (`not-a-step`). Logic lives in conditions and in
handlers.

| Form | Where | Meaning |
|---|---|---|
| `dialogue name(p: Player, npc: Person) [for expr [where pred]] [topic "text" [@id] [if cond] [priority n]] [exits a, b, …]:` + body | item | a **conversation**; the runner binds the player who talks and the NPC talked to. Without `topic` it is a **greeting**; with one, a **topic** in the NPC's menu (below). `for npc where pred` is a **filter** (below); `exits` declares named exits (below) |
| `dialogue name(npc: Person):` + body | item | a **bark** = a one-actor scene (below); no `choice` (`bark-choice`); started by the host command `bark(npc, name)` or the runner's ambient schedule. Any other parameter list is `dialogue-signature` |
| `scene name(a: Person, …) [where pred] [uninterruptible]:` + body | item | a **scene**: several speakers, no listener, timed lines (below); no `choice` (`scene-choice`) |
| `node name [when cond]:` + steps | dialogue body | a node; nodes sharing a name form a **node group**, one picked per entry; a body without nodes is one implicit node |
| `[speaker:] "text" [@id] [(ticks=n)] [when cond]` | step | a **line**; `text` is a string or f-string literal; `speaker` is `p`, `npc` or a name in scope (in a scene: a cast member); `(ticks=n)` overrides its derived duration (scenes and barks only, below) |
| `pause n` | step | silence for `n` ticks (an `int` literal or constant); in a conversation it ends the batch (below) |
| `choice[(timeout=ticks)]:` + options | step | offer options, wait for the pick |
| `[once \| default] "text" [@id] [if cond]` + (`:` steps, or nothing) | inside `choice:` | an option; after its steps, conversation continues after the `choice:` (ink's gather); a `default` option is picked when the timeout runs out |
| `default:` + steps | inside `choice:` | a **hidden fallback**: not offered, run when the timeout runs out (Yarn's `#fallback`) |
| `=> line [when cond]` + (`:` steps, or nothing) | step | consecutive `=>` lines form a **line group**; one is said, then its steps run |
| `when cond:` + steps | step | steps run only if `cond` holds (no `else`: write the opposite condition) |
| `once [per expr]:` + steps | step | steps run once per script instance, or once per value of `expr` (a player) |
| `-> name` | step | jump to a node group of the same dialogue |
| `detour name` | step | run another dialogue (same signature) to its end, then continue here |
| `detour name:` + arms `exit:` + steps | step | as `detour`, then run the arm of the exit the callee left by, then continue here; an exit with no arm continues straight on; an arm naming an exit the callee does not declare is `unknown-exit` |
| `emit Event(field=expr, …)` | step | queue an event to this script instance |
| `return [exit]` | step | leave this dialogue through `exit` (one of its `exits`), or through the implicit exit `done` when bare; back to the detour's caller, or end the conversation. In a scene, `-> other_scene` as the last step is a **follow-up** (below) |
| `event Name:` + fields | item | an event type of the script's own, handled with `@on(Name)` like a host event |
| `@pick(first \| best \| best_least_recent \| random_best_least_recent)` | before `dialogue` | node- and line-group selection strategy |

`cond` in `when` is `once`, `once per expr`, either followed by `if expr`, or a plain `expr`.

**Dialogues as values.** A dialogue's name in an expression is a `Dialogue` value — an id, not a function
value — for the host commands `attach_dialogue(npc, d)`, `detach_dialogue(npc, d)` and `bark(npc, d)`.

**The runner's context in conditions.** Pure functions may call `dialogue_depth()`, the current detour
depth (1 in the outermost dialogue), to bound a dialogue that detours into itself.

**What the compiler produces**

- Each dialogue becomes a **dialogue graph** in `Program.dialogues`: nodes, steps, options, groups, line ids,
  the `for` target and the `@pick` strategy.
- Every condition, substitution, `emit` argument and `for` target becomes a **pure function** in the program,
  referenced from the graph by id.
- Line texts go into the package's **string table**, keyed by line id.
- The graph is versioned with the bytecode (§6.8) and is plain data: hashed and saved with the program.

**Purity.** A pure function may read:
- top-level and quest state, roles,
- `p` and `npc`,
- its own locals.

It may call pure `fn`s and host queries. It may not:
- assign anything but its locals,
- call a host command or request,
- `emit`, `await`, or let an error escape.

Each violation is `impure-condition`. The runner calls a pure function under a small fuel budget
(`COND_FUEL`). A fault or running out of fuel counts as false (an empty substitution) and is reported to
the owner.

**The runner (region Rust code, engine side)**

- **Talking to an NPC — attached dialogues combine commutatively [owner, 2026-11-16: "commutatively combine"].**
  What a player gets depends only on the **set** of dialogues attached to the NPC, never on the order they
  were attached, nor on the package or module order:
  1. **Greeting.** The entry node groups of every attached greeting (a dialogue without `topic`) form one
     pool. The runner picks one member by score, then least recently used, then a **stable key** — the
     dialogue's qualified name and the node's position in it. A random tie-break rolls on that key. The
     picked greeting runs to its end. *As built:* between instances of different programs the key after the
     qualified name is the program's content hash, then the node position; instance ids order only instances of
     one program — so the pick never depends on how instances were numbered.
  2. **Topic menu.** Every attached topic whose `if` holds becomes one option of the NPC's menu, labelled
     with its `topic` text (a line, so it has a line id and is translated). The options are sorted by
     `priority` (higher first, default 0) and then by qualified name. Picking one runs that dialogue as a
     detour and returns to the menu.
  3. **Leaving** (closing the menu, walking away) ends the conversation. With no eligible greeting the menu
     opens at once. With no eligible topic the conversation ends after the greeting. With neither, the NPC
     has nothing to say.

  Each attached dialogue keeps its own records (`once` records, pick counts; no `visited` record was built — no
  example needs one) and emits only to its own
  script instance, so two quests' dialogues on one NPC cannot disturb each other. A dialogue's own `@pick`
  applies inside it; the greeting pool always uses `best_least_recent` with the stable key.
- **Walking the steps:**
  - says lines, evaluating their substitutions at that moment;
  - queues `emit`ted events to the script instance, in order;
  - offers choices, evaluating their `if` guards, and waits for the pick;
  - follows `->` and `detour`, with a return stack in the conversation state;
  - picks group members by `@pick`.
- **The pick is player input,** stamped into the sim at a tick and predicted by the picker's client.
- **Conversation state lives with the NPC,** per player talking to it: the dialogue, node, step, return
  stack and a pending choice. Pick counts and `once` records live with the NPC per attaching
  script instance. All of it is region state of the NPC's current region — hashed, undone, saved — and
  **travels with the NPC across a seam** like its other carried components (below). **No VM task waits during
  a conversation**, so any number of players can talk at once, and the VM's parked cap is not involved.
- **A conversation ends** on:
  - `return` from the outermost dialogue, or the end of its last node;
  - an empty node group;
  - the player leaving (`Left`), or a timeout with no `default` option;
  - the player and the NPC moving out of talking range of each other (`TALK_RANGE_BLOCKS`);
  - a return stack deeper than `DIALOGUE_STACK_MAX` (`TooDeep`);
  - one batch (below) walking more than `BATCH_STEPS_MAX` steps (`TooDeep`; as built).

  The host then raises `DialogueEnded { player, npc, dialogue, reason, exit }` to the script, so handlers can
  react. *As built:* one `DialogueEnded` per **top-level dialogue run** — the picked greeting, and each topic
  picked from the menu, each to its own instance with its own exit — so a topic's exit is seen while the player
  stays in the menu; leaving at the menu raises nothing further. `reason` is an `EndReason` (`Finished`, `Left`, `Timeout`, `TooDeep`, `Edited`, `OutOfRange`);
  `exit` is the outermost dialogue's exit, a label `<dialogue>::<exit>` (`confront::paid`), `<dialogue>::done`
  for a bare `return`, the end of the last node, or any reason but `Finished`.
- **The end acknowledgement.** After the end the conversation stays open until the talker's client sends
  **`Done`** (stamped input) or `CONVERSATION_LINGER_TICKS` pass; until then the NPC keeps facing the player.
  `DialogueEnded` is raised at the end, not at the `Done`.
- **Private.** A conversation has one talker. Lines go to that player's client only. An NPC talks to one player
  at a time; another player who talks to it meanwhile gets its `busy` bark if one is attached, else nothing.
- **Recursion:** detours may form cycles or detour into themselves. The runner's explicit stack makes that
  safe, and §6.1's rule (no script function calling itself) is untouched.
- **Read-your-emit:** an `emit`ted event is handled by the script after the step runs, at the earliest the
  next time the instance runs. A condition later in the same conversation sees the handler's effect only
  after that. Do not write a dialogue that needs it sooner.

**Across region boundaries [owner: "conversations have to work between region boundaries"; design proposed]**

Players and NPCs stand in different regions, cross seams mid-conversation, and the scripts that attach
dialogue to an NPC run in some third region. The seam rule — regions talk only by stamped messages, and a
seam changes nothing observable — is met like this:

- **One owner per conversation: the NPC's region.** The conversation is carried state of the NPC. When the
  NPC crosses a seam it moves as a stamped transfer, as every travelling component does (`carry_components`
  must classify it).
- **The player can be anywhere in talking range,** including across a seam:
  - Its picks travel to the NPC's region as stamped input, applied at T+1, as intents are.
  - Its client sees the lines as events of the NPC's region, through the region subscriptions and
    observer connections clients already have.
  - Range is measured through `WorldView`. `TALK_RANGE_BLOCKS` is at most the halo (`HALO_BLOCKS`, 8), with
    a compile-time assert, so the other party is always visible from the owner's region.
  - The player keeps only a `talking_to: Option<EntityId>` (a travelling `Client` field).
- **Conditions run in the NPC's region, over mirrored script state.** A condition reads its script
  instance's globals, but that instance may run in another region. So:
  - The **code** of every pure function is already everywhere (packages are distributed).
  - The compiler computes each program's **dialogue read set**: the globals any condition, substitution or
    `for` target can read, transitively.
  - Each script instance that attaches a dialogue to an NPC **mirrors its read set** to the NPC's current
    region. A stamped message carries it whenever a value in it changes, landing at T+1 — exactly as halo
    deltas and entity mirrors do. The mirror is rollback state in the receiving region, like the halo.
  - The runner evaluates conditions there with `eval_pure` over the mirror and `WorldView`.
  - A mirror is one tick old at most: the same latency as reading-your-`emit`, so no new rule for authors.
    *As built (stub):* every conversation reads its instance's dialogue mirror, even in the instance's own
    region; a mirror is taken only at a tick boundary with no segment open, so an instance busy mid-segment
    over several ticks hands over its mirror later than one tick (it shows what it had before the segment).
- **`emit` travels too.** An event emitted in a conversation goes to its script instance's region as a stamped
  message (T+1) and enters that instance's queue there.
- **Attachments travel with the NPC.** "Script instance S attached dialogue D" is carried state of the NPC.
  An instance whose NPC has crossed keeps mirroring to the NPC's new region. A stage change that detaches a
  dialogue reaches the NPC as a stamped message.

The script author sees none of this: a dialogue reads its script's state and the world, and emits events. The
one-tick latencies are the engine's ordinary seam latency, which every in-region path also gives.

**Unchanged from the earlier proposal**

- **Line ids** are `@path`. They are optional for `quest check` / `run` (provisional id, warning
  `missing-line-id`) and required for a package build. They are unique per package (`duplicate-line-id`).
- **A line's text is a literal** (`bad-line-text`), with markup passed through to clients.
- **An empty line group says nothing.**

**Scoring — one rule, counting conjuncts [changed 2026-11-16, construction design §7.2]**

The same rule ranks the greeting pool, node groups, line groups, ambient scene candidates and (in the
coordinator) competing quest definitions:

- **Score** = the number of **top-level conjuncts** of the condition: `a and b and c` scores 3. `once` and
  `once per p` each count 1. An `or` group counts **1** however many alternatives it has; `not x` counts 1; a
  parenthesised expression that is not an `and` counts 1. No condition scores 0. (This replaces the Yarn count,
  where each operator counted 1 and so `a or b` outscored `a`; a later Yarn importer re-scores.)
- **Ties:** fewest prior selections (least recent), then the **stable key** (the dialogue's qualified name, then
  the member's source position), then — for the `random_*` strategies only — a `roll` on that key. *As built:*
  the `random_*` roll (on the qualified name and the site) decides before the source position, which only
  breaks a tie the roll leaves.
- The default strategy is `best_least_recent`; the greeting pool always uses it.

**Filters and package-level dialogue [construction design §4.2]**

- `for npc where pred` names the dialogue's own `npc` parameter; `pred` is a pure condition over `npc`, `p`,
  roles and quest state. When a player talks to an NPC, every in-scope filter dialogue whose `pred` holds joins
  that NPC's attached set (still unordered, still commutative). In scope: a quest's filter dialogues apply to NPCs
  in the regions of its instance (the home's and its parts'); the runner evaluates them only for the NPC being
  talked to — one pure call per filter per talk, never a scan of people.
- A `for` target that evaluates to `None` (an unbound optional role) attaches nowhere.
- **Package-level content** (a dialogue or scene in a package with no `quest`, outside any instance) is
  **stateless**: its conditions may read the NPC, the player, the cast and host queries, never a script global,
  and it may not `emit` (`stateless-dialogue`). It is in scope wherever its package is enabled; its `once`
  records live with the NPC keyed by the dialogue's qualified name.

**Exits [construction design §10.2]**

`exits a, b` declares a dialogue's named exits; `return a` leaves through one; a bare `return` or the end of the
last node is the implicit exit `done` (always declared). Exits are labels on control flow, never values: a
conversation still holds no variables. The return-stack entry of a `detour x:` with arms carries its arm table.
An exit is written `<dialogue>::<exit>` in expressions; its type is `Exit`, compared with `==`. `return x` of
an undeclared exit, and an arm naming one, are `unknown-exit`.

**Scenes [construction design §5]**

- A `scene` has several speakers and no listener. Its parameters are its **cast**, all `Person`; the first is the
  **lead**, whose carried state the scene is (it crosses seams with the lead). Steps are a dialogue's minus
  `choice` (`scene-choice`) and minus `detour`; `pause n` is allowed. A bark is a one-actor scene.
- **Starting:** the host command `play(scene, a=…, b=…)` from a handler, or the runner's **ambient schedule**:
  now and then, for regions with a player within `HEAR_RANGE_BLOCKS` of idle NPCs, it binds a cast from nearby
  idle people whose `where` holds and plays one candidate, chosen by the scoring rule.
- **Follow-up:** a scene whose last step is `-> other_scene` plays `other_scene` with the same cast, mapped by
  parameter position (`scene-follow-up` if the target's cast is longer).
- **One activity per NPC:** an NPC is in at most one conversation or scene. A scene is cast only from idle NPCs.
- **Interruptible by default:** a player talking to a cast member ends the scene for all
  (`SceneEnded { scene, reason: SceneEnd::Interrupted }`) and the conversation starts. With `uninterruptible`
  the cast is busy instead. A cast member who dies, leaves range (`HALO_BLOCKS` from the lead) or is removed
  ends it (`ActorLost`); the end of its last step is `Finished`.
- **Hearing:** scene lines go to every client within `HEAR_RANGE_BLOCKS` of the speaker.

**Pacing [construction design §6]**

The sim never sees line text (it hashes line ids and substitution values). So:

- **Conversations run to the next choice.** When a conversation starts or a pick lands, the runner walks steps in
  that one tick until a `choice`, the end, or a `pause`, and sends the lines as one **batch**. The client paces
  them (typewriter, a continue key, auto-advance); a "continue" is not sim input. The sim waits only at a choice
  (the pick is stamped input) and at the end (`Done`, above). A choice's timeout counts in ticks from when it is
  offered. `pause n` ends the batch, and the runner resumes after `n` ticks. Consequence for authors: an `emit`
  between two lines takes effect at the batch's tick; put an effect after a `choice` where timing matters.
- **Scenes and barks use timed lines.** The compiler gives each line a duration in ticks from its source-locale
  text: `LINE_BASE_TICKS + chars × LINE_TICKS_PER_CHAR`, clamped to `[LINE_MIN_TICKS, LINE_MAX_TICKS]`
  (chars = Unicode scalar values of the literal text, f-string holes counted by their source length). It is stored
  in the dialogue graph, so it is part of the program bytes; translations do not change timing. `(ticks=n)`
  overrides one line's.

**Mirror size.** A condition that reads a big global mirrors all of it on each change. The compiler sums the
static size of the read set and warns (`mirror-size`) above `MIRROR_WORDS_WARN` words; a value of unbounded
size (a list, a dict, a string) counts as `MIRROR_WORDS_WARN` on its own, so reading one warns. *As built:* the
generated `participants` list is exempt (bounded by the party, and every part reads it). The farmer example at
the top of this section reads `dog_name: str` and so draws the warning.

**Attachment**

- `for expr` attaches a dialogue to the NPC `expr` names, such as a quest role or a top-level `let` holding
  a `Person`. It is evaluated (purely) when a player talks.
- A dialogue declared inside a quest `stage` is attached only while that stage is current (§5.12).
- Without `for`, a handler attaches and detaches at run time (host commands `attach_dialogue(npc, name)` /
  `detach_dialogue(npc, name)`), or a prefab names the dialogue by its qualified id.

**Edits:** the dialogue data is replaced. A conversation whose current node still exists continues there;
one whose node is gone ends (`Edited`). Records survive an edit because a record's **site** is anchored on line
ids, never on source order (as built): an option's or a group member's id, a node's name with its first line
id, a `once:` block's first line id, hashed FNV-1a 32. Two sites of one dialogue on one anchor are the error
`site-collision`.

**Open:** nothing in the model. (Per-line pacing is decided above; a conversation follows its NPC across a seam,
as carried state.) Whether to drop the bark form for `scene` stays open; both forms are accepted.

### 5.12 Quests [decided 2026-11-16; folded 2026-11-17 from the construction design §3, §7.1, §9]

```python
quest LostDog:
    requires act() >= 1
    role farmer: Person where farmer.occupation == Occupation::Farmer and farmer.alive
    role dog: Creature where dog.has_tag("pet") and dog.owner == farmer
    optional role miller: Person where miller.occupation == Occupation::Miller

    let attempts = 0

    event Accepted:
        player: Player
    event DogFound:
        player: Player
    event Nightfall:
        attempt: int

    stage Open:
        dialogue plea(p: Player, npc: Person) for farmer topic f"About {dog.name}" @lostdog.topic:
            npc: f"{dog.name} ran off toward the hills."                @lostdog.plea
            choice:
                "I'll find him." @lostdog.accept:
                    emit Accepted(player=p)
                "Not now." @lostdog.not_now
        @on(Accepted)
        fn accepted(e):
            join(e.player)
            -> Search

    stage Search:
        objective find_dog f"Find {dog.name}" @lostdog.find at dog
        objective feed_dog f"Coax {dog.name} with food" @lostdog.feed at dog
        optional objective ask_miller "Ask at the mill" @lostdog.mill at miller
        -> Returning when find_dog.done and feed_dog.done
        @on(Entered)
        fn count_attempt(e):
            attempts = attempts + 1
        @on(DogFound)
        fn found(e):
            if not find_dog.done:
                find_dog.finish()
                after(9000, Nightfall(attempt=attempts))
        @on(Nightfall)
        fn bolted(e):
            -> Search                       # a failed attempt: re-entering re-opens the objectives

    stage Returning:
        objective bring_home f"Bring {dog.name} home" @lostdog.return at farmer
        @on(DogDelivered)
        fn delivered(e):
            bring_home.finish()
            complete()                      # for every participant

    part dog_watch on dog:
        @on(PlayerNear)
        fn spotted(e):
            if stage == Stage::Search and e.player in participants:
                emit DogFound(player=e.player)
```

The full worked set (a world-line multi-region quest, a repeatable one, exits, package content) is
`2026-11-16-quest-examples.md`.

- **At most one `quest` per program**, in the crate root (`quest-placement`). One quest definition is one
  program (quests-in-the-world spec §1).
- **Header:** `[repeatable] quest Name[(options)] [at role]:`.
  - `repeatable` (a contextual word) makes it repeatable; without it a quest is one-off. Options, all named
    and constant: `cooldown=ticks` (`int`, default 0), `exclusive=bool` (no `join` while it has participants),
    `once_per_player=bool`. Options on a quest without `repeatable` are `bad-quest-option`. World-line is not a
    quest type: an act lists the quest (game-design §4.1).
  - `at role` anchors the **home** to the region where that role was bound; by default the first role's.
- **`requires cond`** (any number, quest body only): a requirement on the whole definition, checked before any
  role is filled. **`role x: T [where cond]`** binds a role at spawn; it never re-binds. **`optional role`** may
  be unbound: its value is `Option[T]`. Both `requires` and `where` use the **query subset** (below). Roles are
  read-only (`role-assign`). Standalone, the runner supplies role values (`quest run --roles`).
- **`let` and `event`** inside a quest are quest state and quest event types.
- **Stages:** the stages form an enum `Stage` the compiler generates; `stage` (a read of the current stage) is a
  persistent global that migrates by name on an edit (§6.9). The first stage is the initial one; a quest
  without a stage is `no-stage`.
  - A handler declared inside a stage runs only while that stage is current; one declared in the quest body
    runs in any stage. A dialogue declared inside a stage is attached only while that stage is current.
  - **`-> Stage`** in a handler sets the stage and ends the handler.
  - **`-> Stage when cond`** as a step of a stage (not inside a handler) is a **completion transition**: after
    every committed handler segment while the stage is current, the VM evaluates the stage's transitions in
    source order (pure functions) and takes the first that holds. *As built:* "handler segment" means a
    segment of the **home** (a part's never runs them); a run that took a transition runs again, since the new
    stage's own transition may already hold (fuel bounds a cycle); a run still due when an edit lands is carried
    across it.
  - **`Entered`**, a built-in event, is delivered once each time a stage becomes current (by `-> Stage`, a
    completion transition, or the quest's start into its first stage). `@on(Entered)` is allowed only inside a
    stage (`entered-outside-stage`).
- **Objectives:** `[optional] objective name "text" [@id] [at expr] [count n]` declares a **named, stateful**
  objective of its stage: `Open`, `Done` or `Failed`, plus a progress count with `count`.
  - Its state is quest state, persistent, migrated by qualified name (`Search.find_dog`).
  - Its name is a quest-level name: it may not shadow a role, a quest `let` or another objective
    (`duplicate-def`); another stage's objective is `Stage.name`.
  - Commands (handlers only): `obj.finish()`, `obj.fail()`, `obj.progress(n)` (done when the count reaches
    `n` total; on an objective without `count` it is `bad-objective-call`), `obj.reset()`. On a `Done` or
    `Failed` objective, `finish`, `fail` and `progress` do nothing. Pure reads: `obj.done`, `obj.failed`,
    `obj.count`.
  - **Entering a stage re-opens its objectives**, re-entering the same stage included. An earlier stage's
    objectives keep their last state until it is entered again.
  - `optional` objectives never gate anything by themselves. An `at` target that is `None` gets no marker.
  - The text is a line (§5.11 ids and placeholders). **The journal is derived**, never stored: the current
    stage's objectives with their states, a view over quest state, shown to participants.
- **Participants:** `participants: list[Player]` is built-in persistent quest state. `join(p)` adds a player
  (refused, with a host message, while an `exclusive` repeatable quest has participants, or for a
  `once_per_player` player who completed it); `leave(p)` removes one (one player's failure; the quest stays
  open). A one-off quest is **shared**: whoever completes it completes it for every participant.
- **Timers:** `after(ticks, Event(...)) -> Timer` delivers the event to the instance when its tick comes;
  `cancel(t)`. A timer set by a handler **declared in a stage** is cancelled when the stage changes; one set by a
  quest-body handler lives until it fires or is cancelled. Timers are VM state: hashed, undone, saved; at most
  `TIMERS_MAX` live (`after` beyond it faults).
- **Roles that stop resolving:** the engine raises `RoleLost { role: Role }` (`Role` is a generated enum of the
  roles, variants spelled as the role names). Without a handler: `fail()` for a required role, nothing for an
  optional one. *As built, stage-aware:* the default applies in every stage unless the quest body handles
  `RoleLost`; a stage with its own `RoleLost` handler replaces it while current; a part's handler never counts.
- **Builtins:** `complete()` (for every participant) or `complete(players)` (for those only; the others are
  recorded as present), `fail()` (the instance becomes impossible: ends it for everyone and starts the reaper),
  `decline()` (allowed only while handling `Setup`). `Setup` is delivered once when an instance or a part is
  placed, and again after a repeatable reset.
- **Repeatable reset** (on `complete()` of a `repeatable` quest): the instance resets exactly as §6.9's Reset
  (all quest and part `let`s, the stage to the first, objectives re-opened, `participants` emptied, queue, parked
  handlers, promises and timers cleared); the reaper removes what the instance placed; the roles' `where` is
  re-checked (a failure reaps the instance); after `cooldown` ticks `Setup` is delivered again. The namespace is
  kept. Completions are recorded by the coordinator per (definition, player).

**The query subset (`requires`, a role's `where`) [construction design §7.1].** The coordinator evaluates and
indexes these without the VM, so they are restricted to a conjunction (`and`) of **atoms**:

- a comparison of a fact to a literal or enum value (`x.occupation == Occupation::Smith`, `x.age >= 16`,
  `act() >= 2`), or `x.f in [A, B]`;
- a tag or kind test (`x.has_tag("…")`, `x.kind == …`);
- a relation between two roles (`a.site == b.site`, `a.owner == b`, `a != b`, `related(a, b)`,
  `near(a, b, regions=n)`);
- a world fact (`act()`, `loop()`, `completed(def, …)`, `site_kind(x.site) == …`);
- `not atom`, and an `or` of atoms about one role (one group).

Anything else is `bad-where`: a script `fn`, a script global, arithmetic beyond a comparison with a constant, a
host query not in the host description's **fact table** (the subset of pure host queries marked as facts). A
role's `where` may name only itself and roles declared before it.

**Multi-region quests: a home and its parts [construction design §3.6].** Quest state has one owner, the **home**
instance (the quest body). A **part** is a named block of the quest that runs as its own instance elsewhere:

- `part name on role:` anchors it to the role's entity (it crosses seams with it); `part name at role:` to the
  region where the role was bound. The part body holds `let`s (part state), `event`s and handlers — no stages,
  objectives, dialogues or nested parts (`bad-part-item`).
- A part **reads** quest state (the stage, objectives, participants, quest `let`s, roles) through a **mirror**
  of its read set — the same mechanism as dialogue conditions, one tick old. It **never writes** quest state
  (`part-write`): it `emit`s events to the home, which its handlers handle. Commands that are not quest-state
  writes (`place`, `give`, `decline` in `Setup`) are allowed in a part. A part's `emit` of a quest event goes to
  the home, routed by the event's qualified name.
- In a program with a quest, top-level `let`s of the script are **home state** like quest `let`s: a part reads
  them only through its mirror and never writes them (`part-write`) — every instance runs the same program, so
  an unmirrored copy would silently diverge.
- Every part is placed with the home (all must accept); a `decline()` in any part's `Setup` declines the whole
  instance. Reaping removes all of them. A single-region quest has no parts.
- A part's read set is computed like the dialogue read set and counts toward `mirror-size`.

### 5.13 Lexical rules for §5.11–5.12 [decided; folded 2026-11-17]

- **New token:** `=>`.
- **New reserved keywords:** `dialogue`, `quest`, `choice`, `once`, `when`, `where`.
- **Contextual words**, recognised by the layout pass (they remain ordinary identifiers everywhere else,
  so `let stage = 1` and `match node:` still work):
  - `node`, `stage`, `role`, `objective`, `event`, `emit`, `detour`, `part`, `scene`: when one is the first token
    of a logical line (after an optional `pub` for `scene`) and the next token is an identifier or a string.
  - `requires`, `pause`: the first token of a logical line, followed by an expression.
  - `optional`: the first token of a logical line, followed by `role` or `objective`.
  - `repeatable`: the first token of a logical line (after an optional `pub`), followed by `quest`.
  - `default`: when it is the first token of a logical line and the next token is a string, or is `:`
    followed by a newline.
  - `per`: directly after `once`.
  - `at`, `count`: on a line begun by `objective`; `at` also after a `quest` header's name or options, and
    `on` / `at` after a `part` name.
  - `topic`, `priority` and `exits`: on a line begun by `dialogue`, after the parameter list; `uninterruptible`
    on a line begun by `scene`, after the parameter list.
- Whether `scene` and `repeatable` are reserved instead is the parser builder's call; the grammar must stay
  conflict-free either way.
- **`#` still starts a comment**, which is why line ids are `@path`, not Yarn's `#line:`.
- **Decorators** generalise to `@name(args)` before `fn` and `dialogue`. `@on` is valid only on a `fn`
  and `@pick` only on a `dialogue` (`bad-decorator`).

---

## 6. Execution model

### 6.1 Static frames, no recursion [decided]

- The call graph is a DAG (recursive **calls** are rejected; recursive data and dialogue are not). Each
  function gets **one fixed frame** (a fixed number of value slots) allocated up front; the maximum call
  depth and total frame memory are known at compile time (as early Fortran).
- **Scope of the rule [decided 2026-11-16]:** it is a rule of the **quest script language** — what a
  script may contain and how the compiler lowers it. It forbids only **script functions calling
  themselves**, directly or through others (a compile error).
  - **Recursive data** in scripts, such as a type that contains itself (a tree), is allowed. Script code
    walks it with loops and a work list.
  - **Recursive dialogue**, where dialogues detour into each other, is allowed. The compiler lowers it to
    a loop with an explicit return stack in script state, never to script calls.
  - How a self-typed field is represented — for example only behind a `list` or `Option`, so that a frame
    keeps a fixed size — is the builder's call.
  - The **Rust implementation** (compiler, VM, host) has **no recursion rule**; loops are preferred
    there. One practical limit: a Rust walk whose depth a script controls (hashing, comparing, encoding
    or formatting a deeply nested value) must not be able to overflow the native stack, which is small
    on wasm32. Either write it as a loop, or bound its depth the way `ValueTooLarge` bounds its size.
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

### 9.1 Region data, rollback-transparent [owner: scripts "should have access [to region data], abstracted such that undo / rollback is transparent"; design proposed]

A script reads and changes the world it runs in through the host API, and the host API is built on the
engine's own seam-safe interfaces, so undo, rollback and seams are invisible to the script:

- **Reads are host queries over the region's `WorldView`.** They see the instance's own region plus the
  halo (`HALO_BLOCKS` = 8 around it; one tick old; already rollback state). Blocks, entities by
  `EntityId`, piles, containers, water and tags are all readable. A query is a pure function of region
  state at the current tick (§9 above), so a client's replay re-runs it and gets the same answer.
- **Entities are handles.** A `Person`, `Creature` or `Player` value is an `EntityId` (birth region, serial),
  stable across seams. A query about an entity outside the instance's reach gives `None` or the error
  `OutOfReach`; it never guesses.
- **Writes are host commands, applied when their segment commits** (§6.6) and inside the tick:
  - in the own region, they are ordinary region changes under the region's undo log;
  - on something outside it, they become the engine's `Intent`s (applied by the owner at T+1, with replies
    a tick later). A write whose outcome the script needs (taking from a container across a seam) is a
    promise answered by the intent's reply.
- **Beyond the halo** there are no synchronous reads, by the seam rule. A quest that needs to watch a far
  region runs a **part** there (§5.12); world facts come from the coordinator's fact table; shared data goes
  through the quest namespace (§8). (`world.region_query` was dropped 2026-11-16, construction design §8; if it
  ever returns it is `regions.query`, answered by the owning host and refused with `Parked`, never waking one.)
- **Undo is transparent.**
  - The VM's memory is region state: the VM's undo log (§6.6) is driven by the region's
    `new_transaction` / `rollback` / `forget`, so a client's reconcile rolls the VM back with everything
    else and replays it with the same inputs.
  - Outgoing intents and mirror messages are not rollback state. They are sent only from final ticks (the
    server never rolls back), and a client's replay sends nothing, as the engine already does.
  - The script never sees a checkpoint, a rollback or a replay.
- **Where an instance runs.** Each script instance is **anchored**:
  - to a **region** (a site's script, a quest's part in one region; quests-in-the-world §5.4); or
  - to an **entity** (an NPC's own script). An entity-anchored instance is carried state of its entity and
    crosses seams with it, so its reach moves with it.

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
  event structs (`PlayerClicked`, `PlayerEntered`, `ItemDelivered` at least). From Phase 10 it also describes
  and simulates the quest host API of `2026-11-16-quest-examples.md` ("Assumed host API"): the entity handles
  `Person`, `Creature`, `Player`, the types `Site`, `Place`, `Prefab`, `Timer`, `Exit`, the enums `Occupation`,
  `SiteKind`, `EndReason`, `SceneEnd`; the fact queries (marked as facts for the query subset) and other pure
  queries; the commands; the events `Setup`, `Entered`, `PlayerNear`, `ItemGiven`, `Killed`, `PlayerDefeated`,
  `BlockUsed`, `RoleLost`, `Talked`, `DialogueEnded`, `SceneEnded`; and **several instances** (a quest's home and
  its parts) linked by mirrors and routed `emit`s.
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
