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
in an initialiser is refused). Everything else (locals, parked handlers, the queue, pending promises) is VM
state that is saved and restored with them. **Edits** (`quest run --load` with a changed script): a variable
whose name and type still fit keeps its value; a new one runs its initialiser; a removed or retyped one
becomes an **orphan** kept aside (a later edit that brings the name back with a fitting type takes it again);
queued events go to the new handlers (those with none are dropped), parked handlers are cancelled and their
writes stay, an event whose handlers already started is not re-delivered. **Reset** (`--reset`)
re-initialises everything: variables, queue, parked handlers, promises, orphans.

## Tunables

All in `crates/quest-vm/src/consts.rs`; some are part of the saved format. Instance memory 262144 bytes;
fuel 2000 per tick (CLI); queue 4; promise records 16; parked handlers 4; promise timeout 200 ticks;
command buffer 8192 bytes; inbox 8192 bytes; error trace 16 entries; reference path 32 steps; struct or
variant fields 32; one structural walk (compare, hash, format) at most 65536 words (`ValueTooLarge`);
source nesting 256.

## Tests, goldens

`just quest-check` (clippy `-D warnings` plus a wasm32 build), `just quest-test ["<cargo test args>"]`,
`just quest-run <dir> [flags]`, `just quest-wasm` (the corpus determinism goldens on native and on wasm32).
`corpus/run/<case>/` holds `main.qs`, `expect.txt` and `golden.txt` (bytecode hash, per-tick fuel and state
hash, output); `corpus/fail/<case>/` the expected diagnostics. When behaviour changes on purpose, re-bless:
`QUEST_BLESS=1 just quest-test "-p quest-corpus"`, read the diff of the goldens, commit it.

**Testing rule (owner, 2026-11-16).** The plan's tests are the initial implementation's. From now on a test is
written only as a regression for a bug that was actually found; never pre-emptively or "for coverage". Broad
assurance comes from fuzzing, planned later.

## Known MVP limits

- **`not-yet` compile errors**: fallible host queries and commands (the stub's are promises or infallible),
  an unawaited `all(...)`, and `dialogue` / `quest` items: they parse, resolve and type-check, then report
  `not-yet` until Phase 10 lowers them.
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
```

`<dir>` is read recursively for `*.qs` files (paths relative to it, `/` separators).

`quest check` runs every compiler stage, including layout and code generation, and discards the program: a
passing check means the program compiles. It prints only diagnostics, to stderr; warnings do not fail it.

`quest run` compiles, then runs the program against the stub host: the initialisers, `main` once, then
ticks until nothing is left to do. Script output goes to stdout; diagnostics, reports and trace lines go to
stderr.

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

A state file holds the program it was saved with. `--load` of a state saved by the same program resumes it
exactly; a loaded state that already ran `main` runs it again. A state saved by a changed script is
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
then one `orphan:` line per value kept aside (older orphans too), then `events dropped: N` and
`handlers cancelled: N` when not 0. A file that cannot be loaded exits 2. A migration can be refused (a
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
| 2 | a task aborted, or the instance failed, or a state could not be loaded |
| 3 | the tick limit was reached |
| 64 | usage error |
| 66 | unreadable directory or file |
