# Quest Script Language MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A standalone compiler, bytecode VM, stub host and CLI for the quest script language, in its own Cargo
workspace at `quest/`, proven by tests to be deterministic on native and wasm32, pausable on fuel, undoable and
serializable — with no game integration.

**Architecture:** `quest-lang` compiles a directory of `.qs` files plus a host description into a `Program`
(register-style bytecode over static frames). `quest-vm` runs it: all mutable state is one flat memory of 64-bit
little-endian words (static frames, globals, heap, a shadow copy for segment abort, queue, promises), every write is
undo-logged, every instruction costs fixed fuel. `quest-stub` is the in-memory host, `quest-cli` the `quest` binary,
`quest-corpus` the `.qs` corpus with goldens that wasm32 must reproduce.

**Tech Stack:** Rust (the root's pinned nightly, edition 2024), `libm`, `state-hash` (path, no default features),
`lalrpop` 0.23 (LR(1) parser generator; `lalrpop-util` at run time) and `logos` 0.16 (lexer generator) in
`quest-lang`, `proptest` and `wasm-bindgen-test` as dev-dependencies. No serde, no bincode, no clap, no tokio,
no Bevy.

**Spec:** `docs/superpowers/specs/2026-11-16-quest-script-language-design.md` (read it whole; §N below means its
section N). Handoff: `docs/superpowers/plans/2026-11-16-quest-script-language-planning-handoff.md`.

## Global Constraints

- Everything lives under `quest/` (own `Cargo.toml` `[workspace]`, own `quest/Cargo.lock`, own `quest/target/`).
  The only files touched outside it: root `Cargo.toml` (one line, `exclude = ["quest"]`), `justfile` (four recipes),
  `CLAUDE.md` (one paragraph). Each of those three edits is its own small commit.
- Nothing in the root workspace depends on `quest/`. The only path dependency out of `quest/` is
  `../crates/state-hash` with `default-features = false` (no derive, so no `syn` build).
- `quest-vm`, `quest-lang`, `quest-stub` and `quest-corpus` build for `wasm32-unknown-unknown`. No filesystem, time,
  thread or environment access in `quest-vm` and `quest-lang`.
- Determinism: `libm` for every transcendental; NaN canonicalised to `0x7ff8_0000_0000_0000` after every
  float-producing instruction; no `HashMap`/`HashSet` anywhere in `quest-vm` or `quest-lang` (`BTreeMap`/`Vec` only);
  tunables as named consts; no `usize` in any serialized or hashed byte (widths are `u8/u16/u32/u64`).
- The compiler emits byte-identical `Program::to_bytes()` for the same sources and host description on every platform.
- `#![forbid(unsafe_code)]` in every crate. The VM and the compiler never panic on any input (source text, bytecode
  bytes, saved state, host values): they return an error, a diagnostic or a fault.
- Hashing is the `StateHash` trait. The hasher used for traces and goldens is the in-crate `Fnv64` (FNV-1a 64).
- Every recipe goes through `{{cap}}`. Never run raw `cargo` from the repo root for this workspace without
  `--manifest-path quest/Cargo.toml`.
- Git: work on `develop`, no worktrees, never `git add -A`, never `git stash`. `git add quest` is allowed (only this
  work writes there). For `Cargo.toml`, `justfile`, `CLAUDE.md` — which hold other people's uncommitted hunks — stage
  only your hunk: `git diff -U3 <file> > $TMP/p.patch`, delete the foreign hunks from the patch,
  `git apply --cached --recount $TMP/p.patch` (never `--unidiff-zero`), then `git diff --cached` must show only your
  lines before committing.
- Commit messages: `feat(quest): …` / `test(quest): …` / `chore(quest): …`, ending with the session's
  `Co-Authored-By` line.
- All of this workspace's tests run in `just quest-test` (no `#[ignore]` tier: the workspace is small and separate
  from the game's suites). Property tests run 64 cases by default (`PROPTEST_CASES` raises it).
- **After this plan (owner's rule, 2026-11-16):** the tests listed here belong to the initial implementation. Once
  the MVP is built, tests in `quest/` are written only as regressions for bugs actually found, never
  pre-emptively; fuzzing comes later. Task 35 writes this rule into `quest/README.md`.
- The `justfile` is touched three times in the whole plan (Tasks 1, 30, 34), `CLAUDE.md` and root `Cargo.toml` once.

## Review Focus

Failure modes the spec implies but does not list as tests; each has its test in the task named.

1. **Hostile or corrupt bytes** — `Program::from_bytes`, `Vm::load` and `WireValue::decode` meet truncated, bit-flipped
   or random bytes (in game they come from the store and the network). Expected: an `Err`, or a loaded VM that ends
   `Failed(Corrupt)`; never a panic, hang or out-of-bounds write. Tests in Tasks 7, 8, 14.
2. **Pathological source** — 10 000 nested parentheses, a 1 MiB line, an empty file, CRLF line endings, a BOM, tabs,
   non-ASCII identifiers, multi-byte characters before an error column. Expected: diagnostics with correct
   line:column (columns count characters), no stack overflow. Tests in Tasks 16, 17, 33.
3. **Running out of memory in the middle of something** — the heap fills inside a copy-on-write clone, while parking
   frames, while a resumed handler loads its answer; an event or answer too big for the inbox. Expected: fault
   `HeapFull` (segment undone, rollback bar still holds) or a dropped event / `Lost`; never a half-written object.
   Tests in Tasks 10, 12, 13.
4. **A host that misbehaves** — an answer for an unknown, finished or never-issued promise id; an answer or event of
   the wrong shape; an unknown event name; `tick` with fuel 0; `now` going backwards. Expected: ignored or reported
   (`WrongType`, `Dropped`), state unchanged otherwise. Tests in Tasks 12, 13.
5. **Leaks over a long life** — a block runs for months: every handler path (normal return, early `return`, `?`
   propagation, `else` fallback, `break` out of nested loops, an abort, a cancelled parked handler) must give back
   all heap. Expected: heap bytes used return to the post-init baseline once the zombie list is reaped. `no_leaks`
   assertions in Tasks 26, 27, 28, 29, 31.

## Decisions to review (Dawid)

Places where this plan goes beyond the spec or revisits a **[call]**. None reopens a **[decided]** item.

1. **Crates:** five, not three — `quest-stub` (the stub host, split from the CLI so wasm32 and tests can use it) and
   `quest-corpus` (corpus + goldens embedded by a build script, since wasm32 has no filesystem).
2. **No first-class `Result` value.** A fallible expression must be consumed where it is produced — by `?`, `else`,
   or as a `match` scrutinee. `let r = f()` on a fallible `f` is the error `unhandled-fallible`. Consequence for the
   **[call]** `await all([a, b])`: it yields `list[T]` and is fallible with the union of the sets; the first failed
   promise in list order is the error raised.
3. **Unused promises:** a `Promise` or fallible value as an expression statement is a compile error
   (`unused-promise` / `unhandled-fallible`). `let _ = world.set(k, v)` detaches a promise on purpose. A promise can
   be awaited once (a second `await` on a copy is the fault `PromiseAlreadyAwaited`).
4. **`await` always ends the segment and yields**, even when the promise is already resolved (as in JS): segment
   boundaries do not depend on latency.
5. **Continuations cannot be dropped.** A resolved promise's continuation is ordered with events by one arrival
   counter (so "the same queue" holds for ordering), but it does not occupy one of the `QUEUE_LEN` event slots; it
   is bounded by the parked cap instead. Dropping a continuation on overflow would silently kill a handler.
6. **Queue warning:** an event is "queued ⇒ warning" only when a task is in progress at delivery. Two events
   delivered in the same tick to an idle block both wait in the queue with no warning.
7. **Segment abort uses a shadow copy inside the flat memory** (one shadow word per script word + a dirty bitmap),
   not an abort log: an abort log grows without bound in a handler that loops for many ticks inside one segment.
   Cost: the script regions take about half the instance memory.
8. **Frees are deferred and paid in fuel.** A refcount reaching zero links the object into a zombie list; a reap
   step (fuel `FUEL_REAP`, at most 16 children) runs before the next instruction. So no instruction does unbounded
   freeing. Instructions whose work depends on a size (a copy-on-write clone, a string concat, parking frames)
   charge `base + words/8` and may leave a **fuel debt** the next ticks repay before running anything — the stop is
   still at an instruction boundary and identical everywhere.
9. **`print` is buffered per segment** like every other effect (an aborted handler prints nothing).
10. **`chest.take` is a promise in the stub API** (§6.6 allows it): the §8.2 example reads
    `give(e.player, await chest.take("gold", 20)?)` in the corpus.
11. **Host errors with generic data:** `Changed(current)` carries the type of the compared value; the program may
    use it at one type only (two instantiations → `error-shape`), keeping "one name, one shape".
12. **Language details the spec leaves open:** bool literals are `true` / `false` [decided 2026-11-16]; length is the
    method `.len()` on `str`, `list` and `dict`, as in Rust — there is no free `len()` function [decided
    2026-11-16]; no augmented
    assignment (`+=`), no bitwise operators, no tuples (`for k, v in d.items()` is syntax, not a tuple); negative
    indexes fault (no Python wrap-around), slices clamp; `list.remove(i)` removes by index and returns the element;
    `str.len()` counts characters; `to_upper` / `to_lower` / `trim` are ASCII-only (Unicode tables change between
    Rust versions and would change compiled programs' behaviour); floats inside structs/lists compare by bits in
    `==` (a canonical NaN equals itself there, `-0.0 != 0.0`); `await` is not allowed in top-level initialisers;
    any function may `await` (no async colouring — the whole chain is parked); top-level `let` may be `pub`.
13. **Promise ids carry an instance epoch** (`epoch << 32 | serial`); Reset raises the epoch, so ids used as
    idempotency keys are not reused after a Reset.
14. **Rollback does not cross an edit or a Reset** in the MVP (both clear the undo log).
15. **The parser is generated, not hand-written [decided 2026-11-16]:** the grammar is one LALRPOP file
    (`quest-lang/src/grammar.lalrpop`, compiled by `build.rs`), the token rules are a `logos` derive. The only
    hand-written part of the front end is the layout pass that turns line starts into `Indent` / `Dedent` /
    `Newline` tokens and splits f-strings — no generator expresses Python's indentation.
16. **`quest test` is not built** (the spec marks it optional); `assert` / `assert_eq` in `main()` cover it.

---

## Design reference

Tasks refer to these sections as D1…D11. They are decisions; where a detail is not listed, the implementer chooses
and the tests pin behaviour.

### D1. Tunables (`quest-vm/src/consts.rs`)

| const | value | meaning |
|---|---|---|
| `MEM_BYTES_DEFAULT` | 262_144 | instance memory (256 KiB) |
| `HEADER_WORDS` | 64 | header size in words (half is reserved room for later fields) |
| `INBOX_BYTES` | 8_192 | runtime heap for queued event payloads and promise answers |
| `CMD_BYTES` | 8_192 | command buffer of one segment |
| `HEAP_MIN_BYTES` | 4_096 | below this `Layout::plan` fails `MemTooSmall` |
| `QUEUE_LEN` | 4 | queued events |
| `PROMISE_MAX` | 16 | promise records |
| `PARKED_MAX` | 4 | parked handlers |
| `ERR_TRACE_MAX` | 16 | error return trace entries (later ones are counted, not kept) |
| `PROMISE_TIMEOUT_TICKS` | 200 | a request unanswered this long resolves `Timeout` |
| `FUEL_PER_TICK_DEFAULT` | 2_000 | CLI default |
| `REAP_CHILDREN_MAX` | 16 | children released per reap step |
| `WIRE_DEPTH_MAX` | 32 | nesting of a wire value |
| `STRUCT_FIELDS_MAX` | 32 | fields of a struct or variant |
| `PARSE_DEPTH_MAX` | 256 | expression / block nesting in source (`quest-lang`) |

### D2. Flat memory

`Mem` is `Box<[u8]>` addressed in aligned 64-bit little-endian words; offsets are `u32` bytes. Regions in order:

```
header | static (frames, then globals) | heap | shadow | dirty bitmap | inbox heap | queue | promises | parked | commands | error trace
        \_______ script regions ______/
```

- **Script regions** are everything a running handler writes: static slots and the heap (the allocator's own
  header — bump top, free-list heads, zombie head — is the first words of the heap region). `shadow` has one word
  per script word; `dirty` one bit per script word.
- **Runtime regions** are written only by the scheduler: header, inbox heap, queue, promise table, parked table,
  command buffer, error trace.
- Sizing (`Layout::plan(static_words, mem_bytes)`): fixed regions first (header, `INBOX_BYTES`, queue, promises,
  parked, `CMD_BYTES`, trace); of the remaining words `R`, script words `S = R * 64 / 129` (floor), shadow `S`, dirty
  `ceil(S / 64)`; heap = `S - static_words`; heap smaller than `HEAP_MIN_BYTES` → `LoadError::MemTooSmall { need }`.
- Header fields are named consts `H_*` in `layout.rs` (word index → field); word 0 is the magic
  `u64::from_le_bytes(*b"QSVMEM01")`, then bytecode version, host-API version, program content hash, `mem_bytes`,
  epoch, every region base, then scheduler fields (status, task, pc, current fn, fuel debt, promise serial, arrival
  counter, queue head/count, command length, segment-open flag, trace length, init cursor). Unused words are zero.
  The layout is versioned by the magic.

### D3. Writes, undo, segments

- `Mem::w64` logs `(offset, old)` in the undo log unless the value is unchanged. `checkpoint()` is a log position;
  `rollback_to` pops entries; `forget_before` drops history. This is the sim-rollback log (§6.6) and is not hashed
  or saved.
- `Store { mem, layout }` adds two write paths: `ws` (script region) and `wr` (runtime region). While a segment is
  open, the first `ws` to a word copies its old value to the shadow and sets its dirty bit (both logged writes).
- `segment_commit`: clear the dirty bitmap. `segment_abort`: every dirty word is written back from the shadow, then
  the bitmap is cleared. Both are ordinary logged writes, so sim rollback can cross them in either direction.
- The runtime never writes a script region while a segment is open, except through the running task. A queued
  event or an answer waits in the **inbox heap** as wire bytes (D8) and is loaded into the script heap by the
  task that consumes it, inside its segment.
- After an abort the runtime, with no segment open: resets the command buffer, frees the promise records the
  segment issued and restores the promise serial, frees the aborted task's parked object if it was a continuation.

### D4. Values

Every value is one 64-bit slot.

- `int` = i64; `float` = f64 bits; `bool` = 0/1; unit = 0; `Promise` = its id (`epoch << 32 | serial`, never 0 —
  serials start at 1).
- Heap kinds (`str`, `list`, `dict`, struct, enum, error, reference): `0` = empty slot; an **odd** value
  `(tag << 1) | 1` = a field-less enum variant (also `None`, a data-less error); a non-zero even value = byte
  offset of a heap object. `incref` / `release` ignore `0` and odd values, so one `Dup` / `Move` / `Drop` serves
  every heap kind.
- Object header, 2 words. Word 0: `refcount: u32 | kind: u8 << 32 | class: u8 << 40 | tag: u16 << 48`. Word 1:
  `len: u32 | aux: u32 << 32`. The block is `2^class` words including the header. `kind`: `Str`, `ListImm`,
  `ListRef`, `Dict`, `Raw`, `Obj` (struct / variant with data / error with data; `tag` = variant, `len` = field
  count, `aux` = bitmap of heap-kind fields), `RefPath`, `Parked`, `Free`.
- Allocator (one implementation, two instances — script heap and inbox heap): per-class LIFO free lists, else bump;
  never splits or coalesces; a freed block's payload is zeroed. Same request sequence ⇒ same addresses.
- Copy-on-write: a write needs a refcount of 1; `make_unique` clones a shared object shallowly (incref children).
  Nested places are updated by **take → mutate → put back** (`TakeField` / `SetField`, `TakeIndex` / `SetIndex`,
  `TakeKey` / `SetKey`), each step on a unique parent.
- `dict`: insertion-ordered (entries block + hash index); removal keeps the others' order; a re-inserted key goes
  last; iteration is by entry position. Key hash = FNV-1a 64 over the key's wire bytes.
- A reference (`&x`) is a `RefPath` object: root static slot + steps (field index, list index, dict key),
  re-resolved on every use; an unresolvable path is the fault `BadRef`.

### D5. Bytecode

`Instr { op: u8, a: u16, b: u16, c: u16 }`, serialized as 8 bytes `[op, 0, a_lo, a_hi, b_lo, b_hi, c_lo, c_hi]`.
Slot operands are **absolute word indexes into the static region** (frames are static, so locals, parameters of a
callee and globals are addressed the same way). `T` = the 32-bit value `b | c << 16`. Ops that write a heap-kind
slot release what the slot held.

| group | ops |
|---|---|
| load / move | `Nop`; `LoadSmall a, b` (i16 sign-extended); `LoadConst a, T` (const pool u64); `LoadStr a, T` (allocates from the string pool); `Copy a, b`; `Dup a, b` (incref); `Move a, b` (b ← 0); `Drop a` |
| int | `AddI SubI MulI DivI RemI a, b, c`; `NegI a, b`; `EqI NeI LtI LeI a, b, c` — checked; `/` truncates toward zero, `%` is Rust's remainder |
| float | `AddF SubF MulF DivF a, b, c`; `NegF a, b`; `EqF NeF LtF LeF a, b, c`; `IntToFloat a, b`; `FloatToInt a, b` (truncates; NaN / out of range = fault) |
| bool / generic | `Not a, b`; `Eq a, b, c` (deep structural equality of heap kinds) |
| control | `Jmp T`; `JmpIf a, T`; `JmpIfNot a, T`; `Call a` (fn id); `Ret`; `TracePush a` (a = 1 clears the trace first) |
| objects | `NewObj a, b, c` (shape b, tag c); `GetField a, b, c`; `TakeField a, b, c`; `SetField a, b, c` (object slot a, field b, source slot c — moved in); `GetTag a, b`; `MakeUnique a` |
| lists | `NewList a, b` (b = 1: elements are heap kinds); `ListLen a, b`; `GetIndex a, b, c`; `TakeIndex a, b, c`; `SetIndex a, b, c` |
| dicts | `NewDict a, b` (bit 0 keys, bit 1 values are heap kinds); `GetKey a, b, c`; `TakeKey a, b, c`; `SetKey a, b, c` |
| references | `RefMake a, b, c` (root slot b; c = 1: the target is a heap kind); `RefField a, b, c`; `RefIndex a, b, c`; `RefKey a, b, c`; `RefLoad a, b`; `RefTake a, b`; `RefStore a, b` |
| calls out | `CallBuiltin a, b, c` (builtin id a, arguments in consecutive slots from b, result in c); `CallHost a, b, c` (host site a, arguments from b, result in c) |
| promises | `Await a, b, c` (value → slot a, error → slot a + 1, promise slot b, expected type id c); `AwaitAll a, b, c`; `PromiseDetach a` |

- **Frames:** fn `f` owns words `[frame_base, frame_base + frame_words)`: word 0 = return link
  (`return pc | caller fn id << 32`), word 1 = return value, word 2 = error value (0 = none), then parameters, then
  one slot per binding (never reused across bindings), then temporaries. A caller writes arguments straight into
  the callee's parameter slots, `Call`s, then reads words 1 and 2 of the callee's frame.
- **Errors:** all error names of a program form one enum (tags = names sorted). `raise` = build the value in own
  word 2, `TracePush 1`, drops, `Ret`. `?` = `JmpIfNot callee.err, ok`; `Move own.err, callee.err`; `TracePush 0`;
  drops; `Ret`.
- **Builtins** (`builtins.rs`, ids fixed by declaration order, part of the bytecode version): `ListPush`,
  `ListPop`, `ListInsert`, `ListRemove`, `ListSlice`, `ListContains`, `DictGet`, `DictRemove`, `DictContains`,
  `DictKeys`, `DictValues`, `DictLen`, `DictNext`, `DictKeyAt`, `DictValAt`, `StrLen`, `StrConcatN`, `StrSplit`,
  `StrStartsWith`, `StrEndsWith`, `StrContains`, `StrTrim`, `StrUpper`, `StrLower`, `ParseInt`, `Fmt`, `AbsI`,
  `MinI`, `MaxI`, `AbsF`, `MinF`, `MaxF`, `Sqrt`, `Floor`, `Ceil`, `Round`, `Sin`, `Cos`, `Pow`, `Assert`,
  `Print`. A mutating builtin works on the object held in its first argument slot (the compiler takes the
  container out of its place, calls, puts it back).
- `BYTECODE_VERSION = 1`. Op numbers, builtin ids, the fuel table and the memory layout are part of it; a test
  pins each against literals, and changing one means bumping the version.

### D6. Fuel (`fuel.rs`)

| const | value | charged for |
|---|---|---|
| `FUEL_SIMPLE` | 1 | loads, copies, arithmetic, comparisons, jumps, `GetTag`, `ListLen`, `TracePush`, `Not` |
| `FUEL_CALL` | 2 | `Call`, `Ret` |
| `FUEL_HEAP` | 3 | `Dup`, `Move`, `Drop`, field/index get/take/set, `MakeUnique` (+ size), reference ops (+1 per path step) |
| `FUEL_ALLOC` | 5 | `NewObj`, `NewList`, `NewDict`, `LoadStr` (+ size), `RefMake` and extensions |
| `FUEL_KEY` | 6 | dict key ops (+ key size) |
| `FUEL_BUILTIN` | 4 | every builtin (+ size) |
| `FUEL_HOST_QUERY` / `_COMMAND` / `_REQUEST` | 10 / 10 / 20 | `CallHost` by kind (+ argument size) |
| `FUEL_AWAIT` / `FUEL_RESUME` | 20 / 20 | parking / restoring (+ frame words) |
| `FUEL_DISPATCH` | 10 | starting a handler (+ payload size) |
| `FUEL_REAP` | 2 | one reap step |

"+ size" = `ceil(words / 8)` of what is copied, scanned, hashed or compared. Run loop: `while fuel > 0 { reap one
step if zombies exist, else execute one instruction }`; a charge may take fuel below zero, the deficit is stored
as the debt in the header and subtracted from the next tick's budget first.

### D7. Tasks, segments, queue

- One task at a time: `Init(global)`, `Main`, `Handler(event, handler index)`, `Resume(parked index)`.
- Instantiation runs each top-level `let`'s own init function in order (modules in dependency order, then
  declaration order), each as a task. A fault or escaping error in an initialiser sets the instance `Failed`
  (reported; inert until an edit or Reset).
- A **segment** opens when a task starts or resumes and commits at `await` or at the task's end. Commit: hand the
  buffered commands, prints and promise requests to the host in order, then `segment_commit`. A fault or an
  escaping error: `segment_abort`, the cleanup of D3, `Report::HandlerAborted`.
- `deliver(event, payload)`: no handler / unknown event / wrong shape / instance failed → `Dropped(reason)`; queue
  full or inbox full → `Dropped(QueueFull | InboxFull)`; else stored with the next arrival number, and the result
  is `Queued` when a task is in progress (the owner's warning), `Accepted` otherwise.
- Handlers of one event run one after another in table order (module path, then declaration order); each is its
  own task.
- The scheduler picks, between tasks, the lowest arrival number among queued events and ready continuations.

### D8. Wire values and types

`WireValue` is the self-describing form used for namespace values, event payloads, command arguments, error data
in reports and saved globals in a migration: `Unit | Bool | Int | Float | Str | List | Dict (ordered pairs) |
Struct { name, fields: [(name, value)] } | Variant { name, fields: [(name, value)] }`. Bytes: tag `u8`; `i64`/`f64`
bits little-endian (NaN canonical); strings and counts with a `u32` length. `Program.types` is a `TypeTable` of
`TypeDesc` (with field and variant names). Loading a wire value into a type is **structural** (§8.2): fields
matched by name, any order, exactly the same set; a variant must exist in the receiving enum with matching fields;
type names are carried but not compared. A mismatch is `WrongType`.

`Fmt` (f-strings, reports) prints: `42`, `1.5`, `2.0`, `NaN`, `inf`, `true`, `()`, a top-level string raw and a
nested one quoted, `[1, 2]`, `{"a": 1}`, `Ledger(owner="ana", entries=[])`, `Stage::Accepted(tick=5)`,
`Stage::Asked`, `Some(3)`, `None`, `NotEnough(have=3)`. Floats print as Rust's `{:?}` (shortest round-trip).

### D9. Host interface (`quest-vm/src/host.rs`)

```rust
pub trait Host {
    /// Read-only, answered at once; a pure function of the arguments and the host's state this tick.
    fn query(&mut self, f: u16, args: &[WireValue]) -> Result<WireValue, String>;
    /// World-changing call, handed over when its segment commits, in the order issued.
    fn command(&mut self, f: u16, args: &[WireValue]);
    /// A promise request, handed over when its segment commits. Answer later with `Vm::resolve`.
    fn request(&mut self, id: PromiseId, f: u16, args: &[WireValue]);
    fn print(&mut self, text: &str);
    fn report(&mut self, report: Report);
}
pub struct HostApi { pub version: u32, pub structs: Vec<HostStruct>, pub events: Vec<String>,
                     pub fns: Vec<HostFn>, pub errors: Vec<HostError> }
pub struct HostFn { pub namespace: Option<String>, pub receiver: Option<String>, pub name: String,
                    pub params: Vec<(String, HostTy)>, pub ret: HostTy, pub raises: Vec<String>,
                    pub kind: HostFnKind /* Query | Command | Promise */ }
pub enum HostTy { Unit, Int, Float, Bool, Str, List(Box<HostTy>), Option(Box<HostTy>), Struct(String),
                  Value /* any storable value */, Infer /* the receiving type */, Event /* any event struct */ }
pub enum Report { EventQueued { event: String }, EventDropped { event: String, reason: DropReason },
                  HandlerAborted { task: String, cause: AbortCause }, InitFailed { global: String, cause: AbortCause } }
pub enum AbortCause { Error { name: String, data: WireValue, trace: Vec<SrcLoc>, trace_dropped: u32 },
                      Fault { kind: FaultKind, message: String, stack: Vec<SrcLoc> } }
```

`FaultKind`: `IndexOutOfRange`, `MissingKey`, `IntOverflow`, `DivByZero`, `BadRef`, `HeapFull`, `BadFloatToInt`,
`AssertFailed`, `TooManyCommands`, `TooManyPromises`, `TooManyParked`, `PromiseAlreadyAwaited`, `HostFault`,
`Corrupt` (memory that does not parse — sets the instance `Failed`, no abort is attempted).

The stub API (`quest_stub::stub_api()`, version 1): structs `Player { id: int, name: str }`,
`Item { kind: str, count: int }`; events `PlayerClicked { player: Player }`, `PlayerEntered { player: Player }`,
`ItemDelivered { player: Player, item: Item }`; commands `say(p: Player, text: str)`, `log(text: str)`,
`give(p: Player, item: Item)`, `Player.add_tag(tag: str)`, `Player.remove_tag(tag: str)`, `chest.put(item: Item)`,
`emit(e: Event)`; queries `Player.has_tag(tag: str) -> bool`, `roll(salt: int, sides: int) -> int`,
`chest.count(kind: str) -> int`; promises `chest.take(kind: str, n: int) -> Item` raising `ChestEmpty |
NotEnough(have: int)`, `world.get(key: str) -> Infer`, `world.set(key: str, v: Value) -> ()`,
`world.add(key: str, n: int) -> int`, `world.compare_and_set(key: str, expected: Value, new: Value) -> ()` raising
`Changed(current: Infer)`, `world.delete(key: str) -> ()`; every `world.*` also raises `NotFound` (get, add,
delete, compare_and_set), `WrongType` (get, add), `Timeout | Lost | NamespaceGone`.

### D10. Compiler pipeline (`quest-lang`)

```rust
pub struct SourceTree { pub files: BTreeMap<String, String> }   // "main.qs", "npc/mod.qs", … → text
pub fn check(tree: &SourceTree, host: &HostApi) -> Result<Vec<Diagnostic>, Vec<Diagnostic>>; // Ok = warnings
pub fn compile(tree: &SourceTree, host: &HostApi) -> Result<Compiled, Vec<Diagnostic>>;
pub struct Compiled { pub program: Program, pub warnings: Vec<Diagnostic> }
pub struct Diagnostic { pub severity: Severity, pub code: &'static str, pub message: String,
                        pub file: String, pub line: u32, pub col: u32, pub len: u32, pub notes: Vec<String> }
pub fn render(d: &Diagnostic, tree: &SourceTree) -> String;
```

Stages: `lex` → `parse` → `modules` (file tree) → `resolve` (names, `use`, visibility → HIR) → `callgraph`
(recursion, callee-first order) → `infer` (types, error sets, patterns, references, promises) → `layout` (fn ids
= functions sorted by qualified name; frames; globals) → `codegen` → `Program`. Internal IR shapes are the
implementer's; a later task reads them from the code. Diagnostic `code`s (tests assert code + line):

`tab-indent`, `bad-dedent`, `bad-char`, `unterminated-string`, `bad-escape`, `bad-number`, `non-ascii-ident`,
`syntax`, `too-deep`, `chained-comparison`, `mod-not-found`, `mod-ambiguous`, `duplicate-def`, `unresolved`,
`private`, `assign-undeclared`, `type-mismatch`, `needs-annotation`, `unknown-field`, `unknown-method`,
`missing-field`, `arg-count`, `bad-dict-key`, `missing-return`, `too-many-fields`, `recursion`, `error-shape`,
`unhandled-fallible`, `not-fallible`, `raises-outside-pinned`, `unused-promise`, `non-exhaustive`,
`impossible-arm`, `ref-escapes`, `ref-not-place`, `await-not-promise`, `await-in-init`, `not-storable`,
`unknown-event`, `handler-signature`, `too-many-slots`; warnings `error-typo`, `unreachable-arm`.

Rendered form (pinned):

```
error[recursion]: recursive call cycle: f -> f
 --> main.qs:2:12
  |
2 |     return f(n)
  |            ^^^^
```

### D11. Grammar decisions

- Indentation as Python: spaces only (a tab in indentation = `tab-indent`), any consistent width, a dedent must
  return to a column seen before; newlines inside `()`, `[]`, `{}` are ignored; no backslash continuation; CRLF is
  LF; a leading BOM is skipped; identifiers are ASCII (`[A-Za-z_][A-Za-z0-9_]*`).
- Literals: `42`, `1_000`, `0xff`, `1.5`, `1e3`, `true`, `false`, `"…"`, `'…'` (escapes `\n \t \\ \" \' \{ \}`),
  `f"…{expr}…"` (`{{` and `}}` for braces; no format specs).
- Precedence, loosest first: `else` (error fallback) · `or` · `and` · `not` · comparisons `== != < <= > >= in
  not in` (not chainable) · `+ -` · `* / %` · unary `- & *` · `await` · postfix (call, `.name`, `[i]`,
  `[a:b]`, `::`) · `?`. `await` takes a postfix expression without `?`; a following `?` applies to the awaited
  result: `await world.get(k)?` is `(await world.get(k))?`, `await p else 0` is `(await p) else 0`.
- `else` fallbacks: `e else <expr>`, `e else return|break|continue|raise …`, and `e else name:` + block as the
  tail of a `let`, an assignment or an expression statement (the block's last expression statement is its value).
- Construction and patterns take fields by name or by position: `Ledger(owner="ana", entries=[])`,
  `Stage::Accepted(5)`, `raise NotEnough(have=3)`, `case NotEnough(have)`.
- Top level allows only items (`fn`, `struct`, `impl`, `enum`, `error`, `mod`, `use`, `@on(...) fn`) and `let`,
  each optionally `pub`.
- **Parser technology:** tokens come from a `logos`-derived enum (`lex.rs`); a layout pass (`indent.rs`) adds
  `Indent` / `Dedent` / `Newline`, drops newlines inside brackets and splits f-strings; the grammar is
  `grammar.lalrpop` with an `extern` token block over that token stream (LALRPOP's external-lexer interface).
  Precedence is written as one grammar tier per level of the list above. No hand-written recursive descent.
- The parser recovers with LALRPOP's error token (`!`) at statement level (up to the next `Newline` of the same
  block), so one run reports several errors. An `UnrecognizedToken` becomes the diagnostic `syntax` whose message
  lists the expected tokens by their friendly names (`` `:` ``, `an indented block`, `an expression`, …).
- An LR parser does not recurse, but every later pass walks the tree recursively: each AST node records its
  depth (1 + its deepest child, computed in the grammar action) and an action that would build a node deeper
  than `PARSE_DEPTH_MAX` fails with `too-deep`. Block nesting counts the same way.
- Built-in API (all of it): functions `print(str)`, `range(n)` / `range(a, b)`, `float(int)`,
  `int(float)`, `str(x)`, `parse_int(str) -> Option[int]`, `abs`, `min`, `max` (int or float), `sqrt`, `floor`,
  `ceil`, `round`, `sin`, `cos`, `pow` (float), `assert(bool)`, `assert_eq(a, b)`, `all(list[Promise[T]])`;
  `list`: `len()`, `append(x)`, `pop() -> Option[T]`, `insert(i, x)`, `remove(i) -> T`, `x in xs`, `xs[i]`, `xs[a:b]`;
  `dict`: `len()`, `d[k]`, `d[k] = v`, `get(k) -> Option[V]`, `remove(k) -> Option[V]`, `k in d`, `keys()`, `values()`,
  `for k, v in d.items()`; `str`: `len()` (characters), `+`, `split(sep) -> list[str]`, `starts_with(s)`, `ends_with(s)`,
  `contains(s)`, `trim()`, `to_upper()`, `to_lower()`.

---

## File structure

```
quest/
  Cargo.toml  Cargo.lock  README.md
  crates/quest-vm/src/
    lib.rs consts.rs hash.rs mem.rs layout.rs store.rs alloc.rs object.rs string.rs list.rs dict.rs eq.rs
    wire.rs types.rs fmt.rs bytecode.rs program.rs asm.rs host.rs fuel.rs interp.rs interp_heap.rs builtins.rs
    refs.rs sched.rs promise.rs vm.rs save.rs migrate.rs
  crates/quest-vm/tests/        one file per task + common/mod.rs (RecordingHost, assembled programs)
  crates/quest-stub/src/        lib.rs api.rs host.rs namespace.rs runner.rs
  crates/quest-lang/src/
    build.rs (lalrpop)  src: lib.rs source.rs diag.rs lex.rs indent.rs grammar.lalrpop ast.rs parse.rs modules.rs resolve.rs hir.rs
    callgraph.rs ty.rs infer/{mod,expr,stmt,call,data,errors,patterns,refs,promises}.rs exhaust.rs layout.rs
    codegen/{mod,expr,stmt,place,data,matching,errors,refs,handlers}.rs
  crates/quest-lang/tests/      check_*.rs (diagnostics), run_*.rs (end to end through quest-stub)
  crates/quest-cli/src/main.rs  crates/quest-cli/tests/cli.rs
  crates/quest-corpus/          build.rs src/lib.rs src/harness.rs src/gen.rs tests/*.rs
  corpus/run/<case>/{main.qs,…,expect.txt,golden.txt}   corpus/fail/<case>/{main.qs,…,expect.txt}
```

Dependencies: `quest-vm` → `libm`, `state-hash`; `quest-stub` → `quest-vm`; `quest-lang` → `quest-vm` (dev:
`quest-stub`); `quest-cli` → all three; `quest-corpus` → all three (dev: `proptest`, `wasm-bindgen-test`).

## Task conventions

- Run tests with `just quest-test "-p <crate> <filter>"` (Task 1 creates the recipe). "FAIL" in a step means the
  test fails or does not compile because the item is missing.
- After every task: `just quest-check` is clean (it includes clippy with `-D warnings` and the wasm32 check).
- Every task ends with one commit of `quest/` only, unless the task says otherwise.
- A test list entry is `name` — what it asserts. Write each as one `#[test]` (or one `proptest!` case).

---

## Phase 0 — Workspace

### Task 1: The workspace, the exclude, the recipes

**Files:**
- Create: `quest/Cargo.toml`, `quest/crates/quest-vm/Cargo.toml`, `quest/crates/quest-vm/src/lib.rs`,
  `quest/crates/quest-vm/src/consts.rs`
- Modify: `Cargo.toml` (root, the `[workspace]` table), `justfile` (append at the end), `CLAUDE.md` (one paragraph
  after "## Building and running"'s dials, before "### Deployment")

**Interfaces:**
- Produces: the workspace with `[workspace.package] version = "0.1.0", edition = "2024", publish = false`,
  `[workspace.dependencies]` (`libm = "0.2"`, `state-hash = { path = "../crates/state-hash", default-features = false }`,
  `proptest = "1"`, `wasm-bindgen-test = "0.3"`), `[workspace.lints.rust] unsafe_code = "forbid"`,
  `[profile.dev] opt-level = 1`; the consts of D1; recipes `quest-check`, `quest-test`.

- [ ] **Step 1: Record the baseline.** Run `just smoke` twice; the second run must print no `Compiling` line. Save
  `git diff --stat Cargo.lock` output.
- [ ] **Step 2: Create the workspace** with `quest-vm` as its only member; `lib.rs` is `#![forbid(unsafe_code)]`
  plus `pub mod consts;`; `consts.rs` holds the D1 table with a doc comment per const and one test
  `tunables_pinned` asserting every value in the table.
- [ ] **Step 3: Add `exclude = ["quest"]`** to the root `[workspace]` table, directly under `resolver = "2"`.
- [ ] **Step 4: Append the recipes** to the `justfile`:

```just
# The quest script language: its OWN Cargo workspace in quest/ (own Cargo.lock and quest/target/), so its
# builds never touch the game's. Plan: docs/superpowers/plans/2026-11-16-quest-script-language-mvp.md.
# quest-check: type-check, clippy, and the wasm32 build of the crates that must run anywhere.
quest-check:
    {{cap}} {{cargo}} clippy --manifest-path quest/Cargo.toml --workspace --all-targets -- -D warnings
    {{cap}} {{cargo}} check --manifest-path quest/Cargo.toml --workspace --exclude quest-cli --target wasm32-unknown-unknown

# quest-test "-p quest-vm --test mem" — the language's tests (all of them when no argument is given).
quest-test args="":
    {{cap}} {{cargo}} test --manifest-path quest/Cargo.toml {{ if args == "" { "--workspace" } else { args } }}
```

  The wasm32 line names no crate, so every crate added later is checked without another justfile edit (cargo only
  warns while `quest-cli` does not exist yet).
- [ ] **Step 5: Verify.** `just quest-test` → `tunables_pinned` PASS. `just quest-check` → clean. `just smoke` →
  no `Compiling` line (the game was not rebuilt). `git diff --stat Cargo.lock` equals the baseline (the root lock
  did not change). `ls quest/target quest/Cargo.lock` exist; `git status --short quest` shows no `target`.
- [ ] **Step 6: Three commits.** (a) `git add quest` → `chore(quest): the quest script language's own workspace`.
  (b) root `Cargo.toml` hunk + the `justfile` hunk, staged by filtered patch →
  `chore(quest): exclude quest/ from the root workspace; quest-check and quest-test recipes`.
  (c) the `CLAUDE.md` paragraph, staged by filtered patch → `docs: CLAUDE.md points at the quest workspace`:

```markdown
**Quest script language** (`quest/` — its OWN Cargo workspace: own `Cargo.lock` and `quest/target/`, excluded
from the root workspace; nothing in the root may depend on it, and it reaches into the game tree only for
`crates/state-hash`): the standalone compiler (`quest-lang`), bytecode VM (`quest-vm`), stub host and CLI for
player-written quest blocks. `just quest-check | quest-test | quest-run <dir> | quest-wasm`. Spec
`docs/superpowers/specs/2026-11-16-quest-script-language-design.md`, plan
`docs/superpowers/plans/2026-11-16-quest-script-language-mvp.md`.
```

---

## Phase 1 — Flat memory and values (`quest-vm`, no compiler, no interpreter)

### Task 2: `Mem`, the undo log, `Fnv64`

**Files:** Create `quest/crates/quest-vm/src/{mem.rs,hash.rs}`; Test `quest/crates/quest-vm/tests/mem.rs`

**Interfaces — Produces:**

```rust
pub struct Mem { /* bytes: Box<[u8]>, log */ }
pub struct Checkpoint(/* absolute log sequence number: u64 */);
pub enum MemError { OutOfRange { off: u32 }, Unaligned { off: u32 } }
pub enum RollbackError { Forgotten }
impl Mem {
    pub fn new(bytes: u32) -> Mem;                      // zero-filled; `bytes` rounded down to a multiple of 8
    pub fn from_bytes(bytes: Vec<u8>) -> Mem;           // empty log
    pub fn len(&self) -> u32;
    pub fn r64(&self, off: u32) -> Result<u64, MemError>;
    pub fn w64(&mut self, off: u32, v: u64) -> Result<(), MemError>;   // logs (off, old) unless old == v
    pub fn bytes(&self) -> &[u8];
    pub fn checkpoint(&self) -> Checkpoint;
    pub fn rollback_to(&mut self, cp: Checkpoint) -> Result<(), RollbackError>;
    pub fn forget_before(&mut self, cp: Checkpoint);
    pub fn log_len(&self) -> usize;
    pub fn clear_log(&mut self);
}
pub struct Fnv64(pub u64);  impl Default (offset basis 0xcbf29ce484222325);  impl core::hash::Hasher
impl state_hash::StateHash for Mem   // hash_len(len) then the raw bytes; never the log
```

Tests:
- `write_then_read_le` — `w64(8, 0x0102030405060708)`; `bytes()[8..16] == [8,7,6,5,4,3,2,1]`.
- `fnv64_known_vectors` — hash of `b""` is `0xcbf29ce484222325`, of `b"a"` is `0xaf63dc4c8601ec8c`.
- `rollback_restores_bytes_and_hash` — hash; 100 writes; `rollback_to`; bytes and hash equal.
- `nested_checkpoints` — cp1, writes, cp2, writes; rollback cp2 then cp1 each restores its state.
- `same_value_write_logs_nothing` — `log_len()` unchanged.
- `forget_before_drops_history` — `log_len()` shrinks; rollback to a later checkpoint still works; rollback to a
  forgotten one is `Err(Forgotten)` and changes nothing.
- `out_of_range_and_unaligned_are_errors` — `r64(len)`, `w64(len, 1)`, `r64(4)`.
- proptest `random_writes_rollback_bit_exact` — random (offset, value) writes with checkpoints at random points;
  rolling back to any of them equals a saved copy of the bytes.

- [ ] **Step 1:** Write `tests/mem.rs` with the tests above.
- [ ] **Step 2:** `just quest-test "-p quest-vm --test mem"` → FAIL.
- [ ] **Step 3:** Implement `mem.rs` and `hash.rs`.
- [ ] **Step 4:** Same command → PASS; `just quest-check` clean.
- [ ] **Step 5:** Commit `feat(quest): flat memory with an undo log, FNV-1a hashing`.

### Task 3: `Layout`, `Store`, segments

**Files:** Create `quest/crates/quest-vm/src/{layout.rs,store.rs}`; Test `tests/store.rs`

**Interfaces — Consumes:** `Mem`. **Produces:**

```rust
pub struct Layout { pub mem_bytes: u32, pub static_base: u32, pub static_words: u32, pub heap_base: u32,
    pub heap_end: u32, pub shadow_base: u32, pub dirty_base: u32, pub inbox_base: u32, pub inbox_end: u32,
    pub queue_base: u32, pub promises_base: u32, pub parked_base: u32, pub cmds_base: u32, pub cmds_end: u32,
    pub trace_base: u32 }
pub enum LoadError { MemTooSmall { need: u32 }, BadMagic, VersionMismatch { found: u32 }, HostApiMismatch { found: u32 },
                     ProgramMismatch, Invalid(String) }
impl Layout {
    pub fn plan(static_words: u32, mem_bytes: u32) -> Result<Layout, LoadError>;       // D2
    pub fn write_header(&self, mem: &mut Mem);                                       // magic + region bases
    pub fn read_header(mem: &Mem) -> Result<Layout, LoadError>;
    pub fn slot_off(&self, slot: u16) -> u32;                                        // static_base + slot * 8
    pub fn is_script(&self, off: u32) -> bool;
}
pub struct Store { pub mem: Mem, pub layout: Layout }
impl Store {
    pub fn r(&self, off: u32) -> Result<u64, MemError>;
    pub fn ws(&mut self, off: u32, v: u64) -> Result<(), MemError>;   // script region; shadows on first write in a segment
    pub fn wr(&mut self, off: u32, v: u64) -> Result<(), MemError>;   // runtime region
    pub fn segment_open(&mut self);  pub fn segment_commit(&mut self);  pub fn segment_abort(&mut self);
    pub fn segment_is_open(&self) -> bool;
}
```

Tests:
- `plan_regions_are_ordered_disjoint_and_fill_memory` — for `mem_bytes` 65 536 and 262 144, `static_words` 100.
- `plan_too_small` — `plan(100, 16_384)` is `MemTooSmall { need }` and `plan(100, need)` succeeds.
- `header_roundtrip` — `write_header` then `read_header` equals; a wrong magic is `BadMagic`.
- `abort_restores_script_words_only` — in a segment write a static word, a heap word and a queue word; abort; the
  two script words hold their old values, the queue word the new one.
- `abort_restores_first_value_after_many_rewrites`.
- `commit_makes_writes_final` — write, commit, open, write again, abort → the committed value.
- `write_outside_a_segment_is_not_shadowed` — `ws` with no segment open, then open + abort → value kept.
- `rollback_across_commit_then_abort` — checkpoint mid-segment; more writes; commit; `rollback_to`; `segment_abort`
  → bytes equal those at segment open.
- proptest `segments_and_rollback_match_a_model` — random `ws`/`wr`/open/commit/abort/checkpoint/rollback against
  a model that clones a `Vec<u64>` (script words compared word for word, shadow and dirty regions excluded).

- [ ] **Step 1:** Write `tests/store.rs`. - [ ] **Step 2:** `just quest-test "-p quest-vm --test store"` → FAIL.
- [ ] **Step 3:** Implement `layout.rs` (the `H_*` consts for the fields of D2) and `store.rs`.
- [ ] **Step 4:** PASS; `just quest-check`. - [ ] **Step 5:** Commit `feat(quest): memory layout and abortable segments`.

### Task 4: The allocator

**Files:** Create `src/alloc.rs`; Test `tests/alloc.rs`

**Interfaces — Consumes:** `Store`. **Produces:**

```rust
#[repr(u8)] pub enum Kind { Free = 0, Str, ListImm, ListRef, Dict, Raw, Obj, RefPath, Parked }
#[derive(Clone, Copy)] pub struct Heap { pub base: u32, pub end: u32, pub script: bool }   // script: ws, else wr
pub struct AllocError;                                                                    // region full
pub const ALLOC_HEADER_WORDS: u32;   // bump top, zombie head, used words, one free-list head per class
impl Heap {
    pub fn init(&self, st: &mut Store) -> Result<(), MemError>;
    pub fn alloc(&self, st: &mut Store, kind: Kind, payload_words: u32) -> Result<Result<u32, AllocError>, MemError>;
    pub fn free(&self, st: &mut Store, ptr: u32) -> Result<(), MemError>;      // zero the payload, push on its class list
    pub fn used_words(&self, st: &Store) -> Result<u32, MemError>;
    pub fn header(&self, st: &Store, ptr: u32) -> Result<ObjHeader, MemError>;  // rejects a pointer outside the region
    pub fn set_header(&self, st: &mut Store, ptr: u32, h: ObjHeader) -> Result<(), MemError>;
}
pub struct ObjHeader { pub refcount: u32, pub kind: Kind, pub class: u8, pub tag: u16, pub len: u32, pub aux: u32 }
```

A new object has refcount 1, a zeroed payload and `len = 0`.

Tests:
- `first_address_is_pinned` — the first `alloc` returns `heap.base + ALLOC_HEADER_WORDS * 8`.
- `same_class_block_is_reused_lifo` — `a = alloc(3)`, `b = alloc(10)`, `free(a)`, `alloc(4) == a` (3 and 4 payload
  words are both the 8-word class); free `x` then `y` of one class → next two allocs return `y` then `x`.
- `freed_payload_is_zero`; `header_packing_pinned` — refcount 7, kind `Obj`, class 3, tag 0x1234, len 5, aux 0x9
  encode to the two literal words of D4.
- `full_region_is_alloc_error` — fill the region; the next `alloc` is `Ok(Err(AllocError))`, memory unchanged.
- `bad_pointer_is_mem_error` — `header(ptr outside the region)`, `header(unaligned)`.
- `inbox_instance_is_independent` — a second `Heap` over the inbox region does not touch the script heap's bytes.
- proptest `two_runs_same_addresses`; proptest `alloc_free_rollback_bit_exact`.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-vm --test alloc"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): a deterministic size-class allocator inside the flat memory`.

### Task 5: Refcounts, the reaper, strings, lists, struct objects

**Files:** Create `src/{fault.rs,object.rs,string.rs,list.rs}`; Test `tests/objects.rs`

**Interfaces — Consumes:** `Heap`, `Store`. **Produces** (`fault.rs`: `FaultKind` as listed in D9 and
`VmResult<T> = Result<T, FaultKind>`; a `MemError` maps to `FaultKind::Corrupt`, an `AllocError` to `HeapFull`;
Task 8's `host.rs` re-exports `FaultKind`):

```rust
pub fn is_ptr(v: u64) -> bool;                                    // non-zero and even
pub fn incref(st: &mut Store, heap: Heap, v: u64) -> VmResult<()>;
pub fn release(st: &mut Store, heap: Heap, v: u64) -> VmResult<()>;   // refcount 0 → linked into the zombie list
pub trait FrameMaps { fn ref_slots(&self, fn_id: u16) -> &[u16]; }     // which words of a parked frame are heap kinds
pub fn reap_step(st: &mut Store, heap: Heap, maps: &dyn FrameMaps) -> VmResult<bool>;  // false: no zombies
pub fn make_unique(st: &mut Store, heap: Heap, v: u64) -> VmResult<(u64, u32 /* words copied */)>;
pub mod string { new(st, heap, &str) -> VmResult<u64>; read(st, v) -> VmResult<String>; byte_len; char_len;
                 concat(st, heap, parts: &[u64]) -> VmResult<u64> }
pub mod list   { new(st, heap, elems_are_refs: bool) -> VmResult<u64>; len; get(st, l, i: i64) -> VmResult<u64>;
                 take(st, l, i) -> VmResult<u64>; set(st, heap, l, i, v) -> VmResult<()>;
                 push(st, heap, l, v) -> VmResult<u64 /* the list, moved if it grew */>; pop -> VmResult<Option<u64>>;
                 insert; remove(st, heap, l, i) -> VmResult<u64>; slice(st, heap, l, a: i64, b: i64) -> VmResult<u64>;
                 contains }
pub mod obj    { new(st, heap, fields: u8, ref_bits: u32, tag: u16) -> VmResult<u64>; get_field; take_field;
                 set_field; tag(st, v) -> VmResult<u16> /* also of an odd immediate */ }
```

Mutating functions require a unique object (callers `make_unique` first) and return `Corrupt` otherwise. A reap
step takes the zombie at the head, releases at most `REAP_CHILDREN_MAX` of its children (the cursor is kept in the
zombie's refcount field) and frees the block once all are released.

Tests:
- `cow_copy_is_independent` — `a = [1, 2]`; `b = a` (incref); `make_unique(b)`; push 3 → `a` reads `[1, 2]`, the
  new list `[1, 2, 3]`, `a`'s refcount is 1.
- `immediates_are_never_counted` — `incref`/`release` of `0`, of `3` (odd), change nothing.
- `release_of_nested_value_frees_everything` — a list of 100 lists of 3 strings; release; `reap_step` until
  `false`; `used_words` back to the baseline.
- `reap_is_bounded` — a list of 1 000 strings: after one `reap_step`, at most 16 strings were freed.
- `string_roundtrip_utf8` — `"zażółć gęślą jaźń"`: `char_len` 17, `byte_len` 26, `read` equal.
- `list_faults` — `get(l, 3)` on a 3-element list and `get(l, -1)` are `IndexOutOfRange`; `pop` of empty is `None`.
- `slice_clamps` — `[1,2,3,4][1:10] == [2,3,4]`; `[3:1] == []`; a negative bound is `IndexOutOfRange`.
- `struct_fields_and_ref_bits` — fields 0 (int) and 1 (string): releasing the object releases the string only.
- `heap_full_inside_clone_leaves_source_intact` — fill the heap, `make_unique` of a shared list is `HeapFull`, the
  original list still reads back whole. (Review Focus 3)
- proptest `list_matches_vec_model_and_rolls_back` — random push/pop/insert/remove/set/dup/release vs `Vec<i64>`.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-vm --test objects"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): refcounted copy-on-write strings, lists and objects; bounded deferred frees`.

### Task 6: Dicts, key hashing, deep equality

**Files:** Create `src/{dict.rs,eq.rs}`; Test `tests/dicts.rs`

**Interfaces — Produces:**

```rust
pub mod dict { new(st, heap, keys_are_refs: bool, vals_are_refs: bool) -> VmResult<u64>; len;
    get(st, d, key: u64) -> VmResult<Option<u64>>; take; set(st, heap, d, key, val) -> VmResult<u64>;
    remove(st, heap, d, key) -> VmResult<Option<u64>>; contains;
    next(st, d, pos: i64) -> VmResult<i64>;      // first live entry position >= pos, or -1
    key_at(st, d, pos) -> VmResult<u64>; val_at(st, d, pos) -> VmResult<u64>; keys; values }
pub fn deep_eq(st: &Store, a: u64, b: u64, is_ref: bool) -> VmResult<(bool, u32 /* words compared */)>;
pub fn key_hash(st: &Store, key: u64, is_ref: bool) -> VmResult<(u64, u32)>;
```

Tests:
- `insertion_order_survives_remove_and_reinsert` — insert a, b, c; remove b; insert d; insert b → a, c, d, b.
- `overwrite_keeps_position`; `string_keys_compare_by_content` (two separately allocated `"k"`);
  `struct_keys` (an object of `(int, str)`).
- `iteration_by_position_sees_appends` — walking with `next` while inserting two keys visits them.
- `missing_key_get_is_none`; `grow_keeps_order` — 1 000 inserts then walk.
- `layout_is_deterministic` — the same operations on two stores give identical memory bytes.
- `deep_eq_cases` — equal nested lists; different lengths; a struct differing in one field; floats by bits
  (canonical NaN equals itself, `0.0` vs `-0.0` differ).
- proptest `dict_matches_ordered_model_and_rolls_back`.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-vm --test dicts"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): insertion-ordered dicts and structural equality`.

### Task 7: Wire values, the type table, formatting

**Files:** Create `src/{wire.rs,types.rs,fmt.rs}`; Test `tests/wire.rs`

**Interfaces — Produces:**

```rust
pub enum WireValue { Unit, Bool(bool), Int(i64), Float(f64), Str(String), List(Vec<WireValue>),
    Dict(Vec<(WireValue, WireValue)>), Struct { name: String, fields: Vec<(String, WireValue)> },
    Variant { name: String, fields: Vec<(String, WireValue)> } }
pub enum WireError { Truncated, Trailing, BadTag(u8), BadUtf8, TooDeep }
impl WireValue { pub fn encode(&self, out: &mut Vec<u8>); pub fn decode(bytes: &[u8]) -> Result<WireValue, WireError>; }
pub type TypeId = u16;
pub struct TypeTable(pub Vec<TypeDesc>);
pub enum TypeDesc { Unit, Bool, Int, Float, Str, List(TypeId), Dict(TypeId, TypeId), Promise(TypeId),
    Struct { name: String, fields: Vec<(String, TypeId)> },
    Enum { name: String, variants: Vec<(String, Vec<(String, TypeId)>)> } }
impl TypeTable { pub fn is_ref(&self, t: TypeId) -> bool; pub fn storable(&self, t: TypeId) -> bool; }
pub enum FromWire { WrongType, Fault(FaultKind) }
pub fn to_wire(st: &Store, types: &TypeTable, t: TypeId, v: u64) -> VmResult<WireValue>;
pub fn from_wire(st: &mut Store, heap: Heap, types: &TypeTable, t: TypeId, w: &WireValue) -> Result<u64, FromWire>;
pub fn fmt_value(st: &Store, types: &TypeTable, t: TypeId, v: u64, top_level: bool, out: &mut String) -> VmResult<()>;
```

`Option[T]` is `Enum { name: "Option", variants: [("None", []), ("Some", [("value", T)])] }`. A variant's tag is its
index. A variant without fields is the immediate `(tag << 1) | 1`.

Tests:
- `encode_decode_roundtrip` — one value using every variant, nested 3 deep; encoded bytes of
  `Struct { name: "P", fields: [("id", Int(7))] }` equal a literal byte vector written in the test.
- `nan_is_canonical_on_the_wire` — `Float(f64::from_bits(0xfff8_0000_0000_0001))` encodes the canonical bits.
- `decode_rejects_bad_input` — truncated, trailing byte, tag 200, invalid UTF-8, nesting 33 deep (`TooDeep`).
- proptest `decode_never_panics` — random byte vectors up to 512 bytes. (Review Focus 1)
- `from_wire_is_structural` — a stored `Struct { name: "A", fields: [y, x] }` loads into a type named `"B"` with
  fields `x`, `y`; a missing field, an extra field, a field of another type, a variant absent from the receiving
  enum are each `WrongType`.
- `to_wire_from_wire_roundtrip_through_the_heap` — list of structs with an `Option` field.
- `from_wire_heap_full` — a value larger than the free heap is `Fault(HeapFull)` and `used_words` is unchanged
  after reaping.
- `fmt_pins` — every example of D8, exactly.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-vm --test wire"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): self-describing wire values, the type table, value formatting`.

---

## Phase 2 — Bytecode and the VM (hand-assembled programs)

### Task 8: `Program`, the instruction set, the assembler, host types

**Files:** Create `src/{bytecode.rs,program.rs,asm.rs,host.rs,fuel.rs}`; Test `tests/program.rs`

**Interfaces — Produces:** `host.rs` exactly as D9 (plus `PromiseId(pub u64)`, `SrcLoc { file: String, line: u32,
col: u32 }`, `HostStruct { name, fields: Vec<(String, HostTy)> }`, `HostError { name, fields }`); `fuel.rs` as D6;
and

```rust
pub const BYTECODE_VERSION: u32 = 1;
#[repr(u8)] pub enum Op { /* D5, in table order, numbered from 0 */ }
#[repr(u16)] pub enum Builtin { /* D5 list, in order, from 0 */ }
pub struct Instr { pub op: Op, pub a: u16, pub b: u16, pub c: u16 }
pub struct Program { pub bytecode_version: u32, pub host_api_version: u32, pub code: Vec<Instr>,
    pub consts: Vec<u64>, pub strings: Vec<String>, pub shapes: Vec<Shape>, pub types: TypeTable,
    pub fns: Vec<FnInfo>, pub globals: Vec<GlobalInfo>, pub handlers: Vec<HandlerInfo>, pub main_fn: Option<u16>,
    pub host_sites: Vec<HostSite>, pub lines: Vec<LineEntry>, pub files: Vec<String>, pub static_words: u16 }
pub struct Shape { pub fields: u8, pub ref_bits: u32 }
pub struct FnInfo { pub name: String, pub entry: u32, pub end: u32, pub frame_base: u16, pub frame_words: u16,
                    pub ref_slots: Vec<u16> /* frame-relative words holding heap kinds */ }
pub struct GlobalInfo { pub name: String /* qualified */, pub slot: u16, pub ty: TypeId, pub init_fn: u16 }
pub struct HandlerInfo { pub event: String, pub event_ty: TypeId, pub fn_id: u16 }
pub struct HostSite { pub host_fn: u16, pub kind: HostFnKind, pub params: Vec<TypeId>, pub ret: TypeId }
pub struct LineEntry { pub pc: u32, pub file: u16, pub line: u32, pub col: u32 }
pub enum ProgramError { Truncated, BadMagic, Version(u32), Invalid(String) }
impl Program {
    pub fn to_bytes(&self) -> Vec<u8>;                                   // canonical, little-endian, hand-written
    pub fn from_bytes(bytes: &[u8]) -> Result<Program, ProgramError>;     // parses, then validate()
    pub fn validate(&self) -> Result<(), ProgramError>;
    pub fn content_hash(&self) -> u64;                                   // Fnv64 of to_bytes()
    pub fn loc(&self, pc: u32) -> SrcLoc;  pub fn fn_at(&self, pc: u32) -> Option<u16>;
}
pub struct ProgramBuilder;   // new(host_api_version); global(name, ty) -> slot; func(name, params, locals) -> FnBuilder;
                             // FnBuilder: slot(i), emit(op, a, b, c), label()/bind()/jump(op, a, label), line(n);
                             // const_u64, string, shape, ty, host_site, handler, main; finish() -> Program (lays frames out)
```

`validate` rejects: a slot operand outside `static_words`; a jump target outside its function; a fn id, const,
string, shape, type, builtin or host-site index out of range; overlapping frames; a cycle in the `Call` graph
(static frames make recursion memory-unsafe, so the VM refuses it as well as the compiler); a function whose code
does not end in `Ret` or `Jmp`; a version other than `BYTECODE_VERSION`.

Tests:
- `instr_encoding_pinned` — `Instr { AddI, a: 1, b: 2, c: 0x0304 }` → `[op, 0, 1, 0, 2, 0, 4, 3]`.
- `op_and_builtin_numbers_pinned` — a literal table of `(Op, u8)` and `(Builtin, u16)` for every member.
- `fuel_table_pinned` — every const of D6.
- `roundtrip_bytes_identical` — `from_bytes(to_bytes(p)).to_bytes() == to_bytes(p)` for a program using every table.
- `validate_rejects_*` — one test per rule above.
- proptest `from_bytes_never_panics` — truncations and single-bit flips of a valid program, and random bytes:
  `Err`, or a program that passes `validate`. (Review Focus 1)

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-vm --test program"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): the bytecode, the program format and its validation, an assembler`.

### Task 9: The interpreter core — fuel, pause, calls, numbers

**Files:** Create `src/{interp.rs,vm.rs}`; Test `tests/interp.rs`, `tests/common/mod.rs`

**Interfaces — Consumes:** Tasks 2–8. **Produces:**

```rust
pub struct VmConfig { pub mem_bytes: u32, pub epoch: u32, pub host_api_version: u32 }
pub struct Vm { /* program: Arc<Program>, store: Store, heap: Heap, inbox: Heap */ }
impl Vm {
    pub fn new(program: Arc<Program>, cfg: VmConfig) -> Result<Vm, LoadError>;   // versions checked, layout planned
    pub fn slot(&self, slot: u16) -> u64;  pub fn mem_bytes(&self) -> &[u8];  pub fn script_bytes(&self) -> &[u8];
    pub fn heap_used_words(&self) -> u32;
    #[doc(hidden)] pub fn debug_start(&mut self, fn_id: u16);                    // tests only, until Task 12
    #[doc(hidden)] pub fn debug_run(&mut self, fuel: u64, host: &mut dyn Host) -> DebugRun;
}
pub struct DebugRun { pub fuel_used: u64, pub end: RunEnd }
pub enum RunEnd { OutOfFuel, Returned, Faulted { kind: FaultKind, message: String, stack: Vec<SrcLoc> } }
```

`tests/common/mod.rs`: `RecordingHost` (records every `command`, `request`, `print`, `report`; `query` answers from
a closure) and small assembled programs shared by later tasks.

Tests (programs built with `ProgramBuilder`):
- `sum_loop` — sum of 0..100 in the return slot is 4950.
- `fuel_pause_resume_same_result` — for fuel per run in `[1, 2, 3, 7, 50, 1000]`: repeated `debug_run` until
  `Returned` gives the same `script_bytes()` and the same total `fuel_used` as one unlimited run.
- `pause_point_is_deterministic` — two VMs, one `debug_run(37)` each: identical `mem_bytes()`.
- `infinite_loop_burns_exactly_its_budget` — `Jmp` to itself: 10 runs of 100 fuel each use exactly 100, `OutOfFuel`.
- `int_faults` — `i64::MAX + 1`, `i64::MIN - 1`, `i64::MIN * -1`, `-i64::MIN`, `i64::MIN / -1` → `IntOverflow`;
  `1 / 0`, `1 % 0` → `DivByZero`; the fault's `stack[0]` is the line set with `FnBuilder::line`.
- `int_division_truncates` — `-7 / 2 == -3`, `-7 % 2 == -1`.
- `nan_is_canonical` — `0.0 / 0.0` and the negation of it have bits `0x7ff8_0000_0000_0000`.
- `float_to_int` — `2.9 → 2`, `-2.9 → -2`; NaN, `1e300`, `-1e300` → `BadFloatToInt`.
- `calls_use_static_frames` — `f` calls `g` calls `h`; arguments written into callee parameter slots; the fault
  stack inside `h` lists `h`, `g`, `f`.
- `version_mismatch_refused` — `Vm::new` with another `host_api_version` is `HostApiMismatch`.

- [ ] **Step 1:** tests + `common`. - [ ] **Step 2:** `just quest-test "-p quest-vm --test interp"` → FAIL.
- [ ] **Step 3:** implement the run loop of D6 and the load, int, float, bool, control groups of D5.
- [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): the interpreter core — fuel, pause and resume, static frames`.

### Task 10: Heap instructions and builtins

**Files:** Create `src/{interp_heap.rs,builtins.rs}`; Test `tests/interp_heap.rs`

**Interfaces — Consumes:** Tasks 5–9. **Produces:** the object, list, dict groups of D5, `Dup`/`Move`/`Drop`/
`LoadStr`/`Eq`, every builtin of D5 except `Print` (Task 12), the reaper inside the run loop.

Tests:
- `struct_field_update_is_cow` — two slots share an object; `MakeUnique` + `SetField` through one; the other is unchanged.
- `nested_take_set_updates_in_place` — `a.b[1].c = 9` by the take/put chain; a copy of `a` taken before is unchanged.
- `list_builtins`, `dict_builtins` — each builtin once against the semantics of Tasks 5–6.
- `index_and_key_faults` — `GetIndex` out of range → `IndexOutOfRange`; `GetKey` missing → `MissingKey`.
- `str_builtins` — `split("a,b,,c", ",") == ["a","b","","c"]`; `trim("  x \n") == "x"`; `to_upper("aż") == "Aż"`;
  `starts_with`, `ends_with`, `contains`; `"zażółć".len() == 6`; `parse_int("42") == Some(42)`, `"4x"` and
  `"9223372036854775808"` → `None`; `StrConcatN` of 3 parts.
- `math_builtins` — `sqrt(2.0)` bits `0x3FF6A09E667F3BCD`; `sin`, `cos`, `pow` equal the `libm` call bit for bit;
  `floor(-0.5) == -1.0`; `round(2.5) == 3.0`; `min`/`max`/`abs` for both number types (`abs(i64::MIN)` → `IntOverflow`).
- `assert_builtin_faults` — `AssertFailed` carrying the message string.
- `drop_then_reap_charges_fuel` — dropping a 1 000-element list of strings, then running: the following runs spend
  `FUEL_REAP` per step until `heap_used_words()` is back to the baseline.
- `alloc_waits_for_the_reaper` — heap full of zombies: the allocating instruction succeeds after reap steps
  instead of faulting.
- `heap_full_faults` — a push loop ends in `HeapFull`; the VM is still usable (`mem` parses, a rollback to a
  checkpoint taken before the loop restores the hash). (Review Focus 3)
- `size_dependent_fuel_leaves_a_debt` — cloning an 800-word list with 5 fuel left: the instruction completes,
  `fuel_used > 5`, and the next run's first instructions are delayed by the debt; total fuel equals the unlimited run's.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-vm --test interp_heap"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): heap instructions, builtins, the reaper in the run loop`.

### Task 11: References

**Files:** Create `src/refs.rs`; Test `tests/refs.rs`

**Interfaces — Produces:** the reference group of D5. `RefTake` makes every object on the path unique and moves
the target out (leaving 0); `RefStore` puts a value at the path (releasing what was there); `RefLoad` dups it out.

Tests:
- `ref_to_local_list_append` — `a = [1, 2]`; `r = &a`; take, push 9, store → `a == [1, 2, 9]` (§5.3).
- `ref_into_nested_place` — `&quest.rewards[2]`, store a value, read `quest` back.
- `ref_store_whole_value` — on an int root and on a list element.
- `ref_through_shared_value_cows` — `b = a`; write through `&a`; `b` unchanged.
- `ref_dict_key` — `&d["k"]` load / store; a removed key → `BadRef`.
- `ref_path_gone_faults` — `&xs[2]`, pop `xs` to length 2, `RefLoad` → `BadRef`.
- `ref_fuel_counts_steps` — a 3-step path costs `FUEL_HEAP + 3`.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-vm --test refs"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): second-class references as re-resolved paths`.

### Task 12: Tasks, segments, the event queue, commands

**Files:** Create `src/sched.rs`; Modify `src/{vm.rs,interp.rs,builtins.rs}`; Test `tests/sched.rs`

**Interfaces — Consumes:** Tasks 3, 9–11. **Produces** (D7):

```rust
impl Vm {
    pub fn start_main(&mut self) -> bool;                                   // false: no main, or already started
    pub fn deliver(&mut self, event: &str, payload: &WireValue) -> Delivery;
    pub fn tick(&mut self, now: u64, fuel: u64, host: &mut dyn Host) -> TickReport;
    pub fn status(&self) -> Status;  pub fn is_idle(&self) -> bool;         // no task, nothing queued, nothing ready
}
pub enum Delivery { Accepted, Queued, Dropped(DropReason) }
pub enum DropReason { QueueFull, InboxFull, NoHandler, UnknownEvent, WrongShape, InstanceFailed }
pub struct TickReport { pub fuel_used: u64, pub status: Status }
pub enum Status { Idle, Busy, Failed }
```

`CallHost` of kind `Query` calls the host at once (an `Err` is the fault `HostFault`); `Command` and the `Print`
builtin append to the command buffer (full → `TooManyCommands`). `now` smaller than the last `now` is treated as
the last one.

Tests (assembled programs, `RecordingHost`):
- `init_runs_once_in_order_under_fuel` — two globals; with 3 fuel per tick the init spans ticks and ends the same.
- `commands_reach_the_host_only_at_commit` — a handler buffering `say` and spanning 5 ticks: the host sees nothing
  until the tick the handler ends, then the commands in order.
- `fault_undoes_the_segment` — handler sets a global to 5, buffers a command, divides by zero: the global holds its
  old value, the host got no command, one `HandlerAborted { Fault { DivByZero, stack } }` with the right line.
- `escaping_error_reports_name_data_trace` — `raise` in a callee, `TracePush` at two `?` sites: the report has the
  error name, its data as a `WireValue`, and the three locations in order.
- `trace_overflow_is_counted` — 20 pushes: 16 kept, `trace_dropped == 4`.
- `queue_warns_only_when_busy` — idle: two events → `Accepted`, `Accepted`; during a multi-tick handler → `Queued`.
- `queue_overflow_drops_the_newest` — busy + 4 queued, the 5th is `Dropped(QueueFull)`; the 4 run in FIFO order.
- `two_handlers_of_one_event_run_in_table_order`.
- `bad_deliveries_are_dropped` — unknown event → `UnknownEvent`; event without handlers → `NoHandler`; payload
  missing a field → `WrongShape`; payload larger than the inbox → `InboxFull`; state hash unchanged by each.
  (Review Focus 3, 4)
- `tick_with_zero_fuel_changes_nothing`; `now_going_backwards_is_clamped`. (Review Focus 4)
- `print_is_buffered_then_flushed`; `command_buffer_full_faults` → `TooManyCommands`.
- `init_fault_marks_the_instance_failed` — `Status::Failed`, `InitFailed` report, `deliver` → `InstanceFailed`.
- `rollback_mid_handler_is_bit_exact` — checkpoint at tick 3 of a 10-tick handler, run to the end, roll back: bytes
  equal; running again gives the same bytes at every tick and the same host calls.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-vm --test sched"` → FAIL.
- [ ] **Step 3:** implement; remove `debug_start` / `debug_run` uses that `tick` now covers (keep them `doc(hidden)`).
- [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): tasks, abortable segments, the event queue, buffered host commands`.

### Task 13: Promises, `await`, parking

**Files:** Create `src/promise.rs`; Modify `src/{sched.rs,interp.rs,vm.rs}`; Test `tests/promises.rs`

**Interfaces — Produces:**

```rust
impl Vm { pub fn resolve(&mut self, id: PromiseId, outcome: Result<WireValue, WireValue>) -> Resolved; }
pub enum Resolved { Accepted, Unknown }
```

`CallHost` of kind `Promise` takes a record (table full → `TooManyPromises`), buffers the request, yields the id.
At commit the request goes to `Host::request` and its deadline is `now + PROMISE_TIMEOUT_TICKS`. `Await` (D5):
commit the segment; copy the frames of the call chain into one `Parked` object and zero them (table full →
`TooManyParked`, checked before the commit); when the promise is resolved the continuation becomes ready with the
next arrival number; resuming opens a segment, restores the frames, loads the answer with `from_wire` against the
type id (mismatch → the error `WrongType`), frees the record. An `Err` outcome must be a `Variant` naming an error
the program knows, else it is `Lost`. An answer too big for the inbox is stored as `Lost`. `tick` first resolves
every record past its deadline as `Timeout`.

Tests:
- `request_reaches_the_host_at_commit` — ids are `epoch << 32 | 1`, then `| 2`.
- `await_parks_and_other_events_run` — A awaits; event B's handler changes a global; resolve; A resumes and reads
  the changed global.
- `await_of_a_resolved_promise_still_yields` — an event queued before the `await` runs before the continuation.
- `resume_order_follows_arrival` — an event delivered before the resolution runs first; one delivered after, second.
- `fault_after_await_undoes_only_the_last_segment` — `g = 1`, command X, `await`, `g = 2`, command Y, fault →
  `g == 1`, the host has X and not Y.
- `aborted_segment_requests_never_reach_the_host` — and the next request reuses the serial.
- `every_rejection_kind` — `NotFound`, `Changed(current)`, `Lost`, `NamespaceGone` resolved as `Err`; `WrongType`
  from an `Ok` of the wrong shape.
- `timeout_after_the_deadline` — unresolved: at tick `issue + 200` the handler resumes with `Timeout`; a later
  `resolve` of that id is `Unknown`.
- `caps` — a 5th concurrent `await` → `TooManyParked`; a 17th live promise → `TooManyPromises`.
- `await_twice_faults` → `PromiseAlreadyAwaited`; `detached_promise_frees_its_record_on_resolution`.
- `await_all` — answers in list order; the first failed in list order is the error; parks once.
- `resolve_unknown_or_finished_id_is_ignored` — `Unknown`, hash unchanged. (Review Focus 4)
- `oversized_answer_becomes_lost`; `heap_full_while_parking_faults_cleanly` — the segment is undone, no parked
  record remains, `heap_used_words` returns to the baseline after reaping. (Review Focus 3)
- `await_in_a_nested_call_restores_every_frame` — locals of all three frames survive.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-vm --test promises"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): promises — requests at commit, await parking, timeouts`.

### Task 14: Checkpoints, `StateHash`, save and load

**Files:** Create `src/save.rs`; Modify `src/vm.rs`; Test `tests/save.rs`

**Interfaces — Produces:**

```rust
impl Vm {
    pub fn checkpoint(&self) -> Checkpoint;  pub fn rollback_to(&mut self, cp: Checkpoint) -> Result<(), RollbackError>;
    pub fn forget_before(&mut self, cp: Checkpoint);
    pub fn hash64(&self) -> u64;                                   // Fnv64 over StateHash
    pub fn save(&self) -> Vec<u8>;                                 // magic "QSSAVE01", bytecode + host-API versions,
                                                                   // program content hash, mem length, mem bytes, orphans
    pub fn load(program: Arc<Program>, bytes: &[u8]) -> Result<Vm, LoadError>;   // empty undo log
}
impl state_hash::StateHash for Vm   // bytecode version, program content hash, memory, orphans (Task 31; empty until then)
```

`load` checks the magic, both versions, the program hash (`ProgramMismatch`), the length against the header, and
that every region base in the header equals `Layout::plan` for this program and size.

Tests:
- `save_load_at_every_tick_resumes_identically` — a scenario with a multi-tick handler, a parked handler and a
  pending promise: for each tick `t`, save at `t`, load, run both to the end → identical hashes per tick and
  identical host calls.
- `load_refuses_another_program`, `load_refuses_other_versions`, `load_refuses_wrong_length`.
- proptest `load_of_damaged_bytes_never_panics` — truncations and bit flips of a save: `Err`, or a VM whose next
  50 ticks do not panic (it may end `Failed`). (Review Focus 1)
- `hash_covers_program_identity` — two programs differing in one constant, same memory → different `hash64`.
- proptest `rollback_bar` — over the assembled scenarios of `common`: random fuel, random event and resolution
  schedule; checkpoint, run `k` ticks, roll back → `hash64` equal; run again → the same hash at every tick.
- `forget_before_bounds_the_log` — after forgetting at every tick, `log_len` after 1 000 ticks of a loop is below
  a bound derived from one tick's fuel.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-vm --test save"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; then the whole crate: `just quest-test "-p quest-vm"`; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): VM checkpoints, state hashing, save and load mid-execution`.

---

## Phase 3 — The stub host

### Task 15: `quest-stub` — the in-memory host and the runner

**Files:** Create `quest/crates/quest-stub/{Cargo.toml,src/lib.rs,src/api.rs,src/host.rs,src/namespace.rs,src/runner.rs}`,
`tests/stub.rs`; Modify `quest/Cargo.toml` (member)

**Interfaces — Consumes:** `Host`, `HostApi`, `Vm`. **Produces:**

```rust
pub fn stub_api() -> HostApi;                                            // D9, version 1
#[derive(Clone)] pub struct StubHost { pub output: Vec<String>, pub reports: Vec<Report>, /* players' tags,
    chest: BTreeMap<String, i64>, namespace store, pending requests, emitted events */ }
pub struct StubConfig { pub latency: u64, pub failures: Vec<FailureRule>, pub chest: Vec<(String, i64)> }
pub struct FailureRule { pub op: Option<String> /* "world.get", …; None = any */, pub nth: u32 /* 1-based */,
                         pub kind: FailureKind }
pub enum FailureKind { Timeout /* never answered */, Lost, Changed, NamespaceGone /* this and every later call */ }
impl StubHost { pub fn new(cfg: StubConfig) -> StubHost; }   impl Host for StubHost
pub struct Runner { pub vm: Vm, pub host: StubHost, pub now: u64 }
impl Runner {
    pub fn new(program: Arc<Program>, mem_bytes: u32, cfg: StubConfig) -> Result<Runner, LoadError>;
    pub fn step(&mut self, fuel: u64) -> TickReport;
    pub fn run_main(&mut self, fuel: u64, max_ticks: u64) -> RunOutcome;   // Finished { ticks } | TickLimit
    pub fn quiescent(&self) -> bool;       // VM idle, nothing parked, no pending request, no undelivered emit
}
```

`say` writes `"<name>: <text>"` to `output`, `log` writes `"log: <text>"`, `print` the text, `give` writes
`"give <name> <count> <kind>"`. `roll(salt, sides)` = `fnv64(salt) % sides` (0 when `sides <= 0`). One `step`, in
this order: deliver the events emitted during the previous tick (in order; a non-`Accepted` result is pushed to
`reports`); resolve the requests due at `now` in promise-id order; `vm.tick(now, fuel, host)`; `now += 1`. A
request committed during tick `t` is due at `t + latency` (latency 0 = the next tick). Namespace semantics: `get`
missing → `NotFound`; `add` on a missing key starts from 0, on a non-int → `WrongType`; `compare_and_set` with a
differing or missing current value → `Changed(current)`; `delete` missing → `NotFound`.

Tests (the host through the `Host` trait directly; the runner with one assembled program):
- `stub_api_is_pinned` — version 1 and the sorted list of every function, event, struct and error name of D9.
- `commands_write_output` — the four formats above.
- `namespace_semantics` — each operation's success and each rejection listed above.
- `latency_is_exact` — a request committed at tick 5 with latency 3 is answered in the step for tick 8.
- `failure_rules` — `Lost` on the 2nd `world.get`; `Timeout` never answers; `NamespaceGone` sticks; `Changed`.
- `chest_take` — `ChestEmpty` when the kind is absent or 0; `NotEnough(have=3)`; success lowers the count.
- `emit_goes_through_the_queue` — an emitted event is delivered at the next step, not within the tick.
- `clone_is_a_snapshot` — clone the host and checkpoint the VM, run 5 steps, restore both, run again → same `output`.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-stub"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): the stub host — players, chest, a namespace store with latency and failures`.

---

## Phase 4 — Compiler front end (`quest-lang`)

### Task 16: Sources, diagnostics, the lexer

**Files:** Create `quest/crates/quest-lang/{Cargo.toml,src/lib.rs,src/source.rs,src/diag.rs,src/lex.rs,src/indent.rs}`,
`tests/lex.rs`; Modify `quest/Cargo.toml` (`logos = "0.16"` in `[workspace.dependencies]`)

Raw tokens are a `#[derive(logos::Logos)]` enum in `lex.rs` (keywords, punctuation, identifiers, numbers, string
and f-string literals, comments and horizontal whitespace skipped, newlines kept). `indent.rs` is the layout pass
over that stream (D11). `lex()` below is the two together.

**Interfaces — Produces:** `SourceTree`, `Diagnostic`, `Severity`, `render` (D10);
`pub fn lex(file: &str, text: &str) -> (Vec<Token>, Vec<Diagnostic>)` with `Token { kind: Tok, line: u32, col: u32,
len: u32 }`; `Tok` has `Indent`, `Dedent`, `Newline`, `Eof`, keywords, punctuation, `Ident(String)`, `Int(i64)`,
`Float(f64)`, `Str(String)`, `FString(Vec<FPart>)` with `FPart::Text(String) | FPart::Expr(Vec<Token>)`.
Lines and columns are 1-based; columns count characters.

Tests:
- `indent_dedent_basic` — `"if x:\n    y\nz\n"` → `If Ident Colon Newline Indent Ident Newline Dedent Ident Newline Eof`.
- `blank_and_comment_lines_are_invisible`; `brackets_suppress_newlines`; `dedents_close_at_eof`.
- `edge_inputs` — empty text → `[Eof]`; no final newline; CRLF equals LF; a BOM is skipped. (Review Focus 2)
- `errors` — a tab in indentation → `tab-indent`; dedent to an unseen column → `bad-dedent`; `$` → `bad-char`;
  `"abc` → `unterminated-string`; `"\q"` → `bad-escape`; `1.`, `0x`, `9223372036854775808` → `bad-number`;
  `żółw = 1` → `non-ascii-ident` at 1:1.
- `columns_count_characters` — in `let s = "żż" $` the `bad-char` is at column 14.
- `numbers` — `1_000`, `0xff`, `1.5`, `1e3`.
- `strings_and_fstrings` — both quote kinds, every escape, `f"a{x + 1}b{{c}}"` → `[Text("a"), Expr(x + 1), Text("b{c}")]`.
- `a_megabyte_line_lexes` — 1 MiB of `1 + ` repeated: returns without panic. (Review Focus 2)
- `render_format` — the block of D10, exactly, and one with a note line (`  = note: …`).

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-lang --test lex"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): sources, diagnostics, the logos lexer and the indentation pass`.

### Task 17: The parser — expressions, types, patterns

**Files:** Create `quest/crates/quest-lang/build.rs` (`lalrpop::process_src()`), `src/grammar.lalrpop`,
`src/ast.rs`, `src/parse.rs`; Modify `quest/crates/quest-lang/Cargo.toml` (`lalrpop-util` dependency, `lalrpop`
build-dependency, both `0.23`, `lalrpop-util` with `default-features = false` — the built-in lexer is not used),
`quest/Cargo.toml`; Test `tests/parse_expr.rs`

**Interfaces — Produces:** `ast` node types (every node carries `line, col, len, depth`); the grammar with an
`extern` block over `lex()`'s tokens and three public entry nonterminals `ExprEntry`, `TypeEntry`,
`PatternEntry`; `parse.rs` wraps the generated parsers and maps `lalrpop_util::ParseError` to `Diagnostic`:
`pub fn parse_expr_str(text: &str) -> Result<String, Vec<Diagnostic>>` returning an s-expression dump (test aid,
`#[doc(hidden)]`), likewise `parse_type_str`, `parse_pattern_str`. An f-string's `FPart::Expr` tokens are parsed
with `ExprEntry`. The grammar must generate with no LALRPOP conflict and no `#[allow]` of one.

Tests (input → dump, per D11):
- `1 + 2 * 3` → `(+ 1 (* 2 3))`; `not a and b or c` → `(or (and (not a) b) c)`; `-a * b` → `(* (neg a) b)`.
- `a < b < c` → `chained-comparison`; `x in xs`, `x not in xs`.
- `f(x)?.y[0]` → `(index (field (? (call f x)) y) 0)`.
- `await p else 0` → `(else (await p) 0)`; `await world.get(k)?` → `(? (await (call (field world get) k)))`.
- `take(c, 20) else return` → `(else (call take c 20) (return))`.
- `&a.b[2]` → `(ref (index (field a b) 2))`; `*r` → `(deref r)`.
- `Ledger(owner="ana", entries=[])` → `(call Ledger owner="ana" entries=(list))`; `Stage::Accepted(tick=5)`;
  `{"a": 1, "b": 2}`; `[1, 2][0:1]`, `x[:b]`, `x[a:]`.
- f-string → `(fstr "total " total ", stopped at " i)`.
- types: `list[dict[str, Option[int]]]`, `()`, `npc::dialogue::Line`, `Promise[int]`.
- patterns: `_`, `5`, `"a"`, `x`, `Stage::Accepted(tick)`, `Stage::Accepted(tick=t)`, `Err(ChestEmpty | QuestClosed)`,
  `Ledger(owner=o, entries=_)`, `Some(Stage::Done)`.
- `deep_nesting_is_a_diagnostic` — 10 000 `(` then `1` then 10 000 `)` → one `too-deep`, no stack overflow; the
  same for 10 000 nested `[`, for `not` repeated 10 000 times, and for `1 + 1 + …` with 100 000 terms (a
  left-deep tree). (Review Focus 2)
- `depth_limit_is_exact` — 255 nested parentheses parse; 257 → `too-deep`.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-lang --test parse_expr"` → FAIL.
- [ ] **Step 3:** implement (one grammar tier per precedence level of D11; node depth checked in the actions).
- [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): the LALRPOP grammar — expressions, types, patterns`.

### Task 18: The parser — statements and items

**Files:** Modify `src/grammar.lalrpop` (statements, blocks, items, the `FileEntry` nonterminal, statement-level
`!` recovery), `src/ast.rs`, `src/parse.rs`; Test `tests/parse_items.rs`

**Interfaces — Produces:** `pub fn parse_file(file: &str, text: &str) -> (ast::File, Vec<Diagnostic>)`.

Tests:
- `every_spec_snippet_parses` — each code block of spec §5.1, 5.3, 5.4, 5.5, 5.6, 5.7 (wrapped in a `fn` where it
  is statements), 5.10, 7, 8.2, copied into the test, parses with zero diagnostics.
- `let_forms` — `let x = 1`, `let n: int = 1`, `x = 2`, `*r = v`, `a.b[0] = 1`, `let _ = f()`.
- `else_block_form` — the `else err:` example of §5.7; the block is attached to the `let`.
- `fn_signatures` — `fn f(x: int) -> str raises QuestError:`, `fn add(&self, n):`, `pub fn g():`, `@on(PlayerClicked)`.
- `items` — `struct`, `impl`, `enum` with unit and data variants, `error ChestError = ChestEmpty | NotEnough(have: int)`,
  `mod npc`, `use a::{b, c}`, `use a::b as c`, `pub let total = 0`.
- `match_with_guards_and_alternatives`; `for k, v in d.items():`; `for i in range(5):`; `while`, `break`,
  `continue`, `pass`, `return`, `raise NotEnough(have=3)`.
- `top_level_allows_only_items_and_let` — `print("x")` at top level → `syntax` naming what is allowed.
- `error_recovery_reports_both` — two broken statements in one function → two diagnostics, on their own lines.
- `messages_name_the_expected_token` — a missing `:` after `if x` → `syntax` at the line's end whose message
  names `` `:` `` among the expected tokens; a missing indented block → `syntax` naming `an indented block`.
- `block_nesting_limit` — 300 nested `if true:` → `too-deep`.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-lang --test parse_items"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): the LALRPOP grammar — statements, items, recovery`.

### Task 19: Modules and name resolution

**Files:** Create `src/{modules.rs,resolve.rs,hir.rs}`; Test `tests/resolve.rs`

**Interfaces — Consumes:** `parse_file`, `HostApi`. **Produces:**
`pub fn load_modules(tree: &SourceTree) -> (ast::Crate, Vec<Diagnostic>)` (root `main.qs`; `mod x` is `x.qs` or
`x/mod.qs` beside the declaring file); `pub fn resolve(krate: &ast::Crate, host: &HostApi) -> (hir::Crate,
Vec<Diagnostic>)` — every name is a definition id, a local id, a host item or a prelude item (`Some`, `None`, `Ok`,
`Err`, `Option`, `print`, `range`, `float`, `int`, `str`, `assert`, `assert_eq`, `abs`, `min`, `max`,
`sqrt`, `floor`, `ceil`, `round`, `sin`, `cos`, `pow`, `parse_int`, `all`). Test helper in `tests/common/mod.rs`:
`fn diags(files: &[(&str, &str)]) -> Vec<(String /* code */, String /* file */, u32 /* line */)>` running the whole
front end available so far against `quest_stub::stub_api()`.

Tests:
- `spec_module_tree_resolves` — the §5.10 tree and `use` lines, zero diagnostics.
- `mod_errors` — `mod nope` → `mod-not-found`; both `x.qs` and `x/mod.qs` → `mod-ambiguous`.
- `visibility` — using a non-`pub` fn, struct, enum or top-level `let` of another module → `private`, naming it;
  `pub` ones resolve; `crate::`, `super::`, `self::` paths; `use … as`.
- `use_cycles_between_modules_are_fine`.
- `scopes` — a binding made in an `if` body used after it → `unresolved`; inner scopes see outer bindings;
  `let x = 1` then `let x = "s"` is fine; `x = 2` with no `x` → `assign-undeclared`; a match-arm binding is
  scoped to its arm; a `for` variable to its loop.
- `unresolved_suggests` — `pritn("x")` → `unresolved` with a note naming `print`.
- `duplicates` — two `fn f` in one module, two fields of one name → `duplicate-def`.
- `host_names` — `world.get`, `say`, `PlayerClicked` resolve; a local named `world` shadows the namespace.
- `error_names_need_no_import` — `raise ChestEmpty` in one module, `case Err(ChestEmpty)` in another.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-lang --test resolve"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): modules, visibility and name resolution`.

---

## Phase 5 — Types and checks

Every task here extends `pub fn check(tree, host)` (D10) and tests through `diags(...)` plus
`#[doc(hidden)] pub fn signatures(tree, host) -> BTreeMap<String, String>` (qualified fn name → e.g.
`"fn(int) -> int raises ChestEmpty | NotEnough"`, error names sorted).

### Task 20: The call graph and core inference

**Files:** Create `src/{callgraph.rs,ty.rs}`, `src/infer/{mod.rs,expr.rs,stmt.rs,call.rs}`; Test `tests/check_core.rs`

**Interfaces — Produces:** `check`, `signatures`; the typed crate later tasks and codegen read. Inference is
unification inside one function, functions visited callee-first; a parameter or binding still unknown at the end
of its function is `needs-annotation` (call sites never decide a parameter's type).

Tests:
- `spec_5_4_signatures` — `reward_for` is `fn(int) -> int`, `main` is `fn() -> ()`.
- `recursion_direct` — `fn f(n): return f(n)` → `recursion`, message `recursive call cycle: f -> f`, at the call.
- `recursion_mutual` — `a` ↔ `b` → message `a -> b -> a` (the cycle starts at its smallest name); through a method.
- `needs_annotation` — `fn id(x): return x`; `let xs = []` never constrained; both name the binding.
- `mismatches` — `1 + 1.0`; `if 1:`; `let n: int = "s"`; `x = "s"` after `let x = 1`; two `return`s of different
  types; `"a" + 1`.
- `shadowing_may_change_type`; `callee_first_ignores_declaration_order` (callee declared later, in another module).
- `missing_return` — a value returned on one path only.
- `arg_count`; `conversions` — `float(1)`, `int(2.5)` typed; `int("x")` → `type-mismatch`.
- `annotations_are_checked` — `fn f(x: int) -> str: return x` → `type-mismatch`.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-lang --test check_core"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): call-graph ordering, recursion rejection, core type inference`.

### Task 21: Structs, enums, collections, builtins, host calls

**Files:** Create `src/infer/data.rs`; Modify `src/infer/*`; Test `tests/check_data.rs`

Tests:
- `struct_construction` — missing field → `missing-field`; unknown → `unknown-field`; wrong type → `type-mismatch`;
  33 fields → `too-many-fields`.
- `methods` — `l.add(5)` and `l.total()` of §5.5 type-check; `l.nope()` → `unknown-method`; calling a `&self`
  method on a non-place (`make().add(1)`) → `ref-not-place`.
- `enums` — variant construction by name and by position; `let x: Option[int] = None`; `Some(1)`.
- `collections` — `[1, "a"]` → `type-mismatch`; `{"a": 1}[0]` → `type-mismatch`; a `float` or `list` key →
  `bad-dict-key`; a struct of `int` and `str` as key is fine; `xs[0:1]` is a list; `d.get(k)` is `Option[V]`;
  `.len()` on `str`, `list` and `dict` is `int`; a free `len(xs)` → `unresolved` with a note naming `.len()`;
  `in`, `range`; `for x in 5` → `type-mismatch`; `for k, v in d.items()` binds key and value types.
- `builtin_api` — every function and method of D11's built-in API list with its type; `s[0]` on a string →
  `type-mismatch` naming the missing string indexing.
- `fstrings_take_any_type`.
- `host_calls` — `say(e.player, 1)` → `type-mismatch`; `e.nope` → `unknown-field`; `p.has_tag("x")` is `bool`;
  `roll(1, 6)` is `int`; `chest.count("gold")` is `int`; `world.set("k", world.get("k"))` → `not-storable`.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-lang --test check_data"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): typing of structs, enums, collections, builtins and host calls`.

### Task 22: Errors

**Files:** Create `src/infer/errors.rs`; Test `tests/check_errors.rs`

Tests:
- `inferred_sets` — a `take_gold` raising `ChestEmpty` and `NotEnough(have=…)`: signature
  `raises ChestEmpty | NotEnough`; a caller using `?` gets the union with its own raises.
- `must_handle` — a fallible call as a statement, or bound by `let g = take_gold(c, 20)` → `unhandled-fallible`.
- `handling_forms` — `?`, `else 0`, `else return`, the `else err:` block (must yield a value of the right type or
  diverge — a block ending in `log(...)` where an `int` is needed → `type-mismatch`), `match` with `Ok`/`Err`.
- `not_fallible` — `?` or `else` on an infallible value → `not-fallible`.
- `option_forms` — `?` on an `Option` inside a fn returning `Option`; inside one returning `int` → `type-mismatch`;
  `opt else 0`.
- `pinned_set` — `fn pay(p) -> Item raises QuestError:` raising `Other` → `raises-outside-pinned`.
- `named_sets_union` — `QuestError` contains `ChestError`'s names.
- `one_shape_per_name` — `raise NotEnough(have=3)` and `raise NotEnough` → `error-shape`; `have` as `int` and as `str`.
- `typo_warning` — `raise ChestEmtpy` while `ChestEmpty` is matched elsewhere → warning `error-typo` naming both;
  no warning for a name that is matched somewhere.
- `errors_may_escape_main_and_handlers` — no diagnostic.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-lang --test check_errors"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): error names, inferred and pinned error sets, must-handle`.

### Task 23: `match` — patterns and exhaustiveness

**Files:** Create `src/infer/patterns.rs`, `src/exhaust.rs`; Test `tests/check_match.rs`

Tests:
- `spec_5_6_is_exhaustive`; removing the `Stage::Done` arm → `non-exhaustive` naming `Stage::Done`.
- `guards_do_not_cover` — only `case Stage::Accepted(t) if t > 100` for that variant → `non-exhaustive`.
- `fallible_match` — §5.7's `match take_gold(c, 20)` is exhaustive; without the `ChestEmpty` arm →
  `non-exhaustive` naming `Err(ChestEmpty)`; with `case Err(QuestClosed)` where it cannot occur → `impossible-arm`.
- `bool_option_nested` — `true`/`false`; `Some(Stage::Accepted(t))` + `Some(_)` + `None`.
- `literals_need_a_wildcard` — ints and strings without `_` → `non-exhaustive`.
- `alternatives` — `A | B` covers both; bindings must be the same names and types in every alternative, else `type-mismatch`.
- `struct_patterns`; `binding_types` — `case Stage::Accepted(tick)` binds an `int`.
- `unreachable_arm_warning` — an arm after `case _`.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-lang --test check_match"` → FAIL.
- [ ] **Step 3:** implement (matrix-based usefulness over constructors). - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): pattern typing, exhaustive match, impossible arms`.

### Task 24: References, promises, handlers

**Files:** Create `src/infer/{refs.rs,promises.rs}`; Test `tests/check_async.rs`

Tests:
- `ref_escapes` — a reference in a list literal, a dict value, a struct field, `Some(&x)`, an error's data, a
  top-level `let`, a `return` → `ref-escapes` each; held in a local and passed as an argument is fine.
- `ref_needs_a_place` — `&f()`, `&(a + b)` → `ref-not-place`; `&a.b[2]` is fine.
- `deref_assign_types` — `*r = "s"` on a reference to `int` → `type-mismatch`.
- `await_rules` — `await 5` → `await-not-promise`; `await` in a top-level `let` → `await-in-init`; `await` inside
  a helper function called by a handler is fine.
- `unused_promise` — `world.set("k", 1)` as a statement → `unused-promise`; `let _ = world.set("k", 1)` is fine;
  `await world.set("k", 1)` as a statement → `unhandled-fallible`.
- `get_needs_a_type` — `let n = await world.get("k")?` with `n` otherwise unconstrained → `needs-annotation`;
  `let n: int = …` is fine; used as `n + 1` is fine.
- `changed_has_one_type` — `compare_and_set` on an `int` in one function and a `str` in another → `error-shape`.
- `await_all_types` — `await all([a, b])?` is `list[int]` for two `Promise[int]`.
- `handlers` — `@on(Nope)` → `unknown-event`; two parameters or a return value → `handler-signature`; `e.player`
  is typed from the decorator; two handlers on one event are fine.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-lang --test check_async"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; the whole crate `just quest-test "-p quest-lang"`; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): reference restrictions, promise and handler checks`.

---

## Phase 6 — Code generation

Tests here are end to end: `tests/common/mod.rs` gains

```rust
pub struct Out { pub lines: Vec<String>, pub reports: Vec<Report>, pub leaked_words: i64 }
pub fn run(files: &[(&str, &str)]) -> Out;                       // compile, Runner::run_main(2_000, 10_000), default StubConfig
pub fn run_with(files: &[(&str, &str)], cfg: StubConfig, fuel: u64) -> Out;
```

`leaked_words` = `heap_used_words` once the run is quiescent and the zombie list is empty, minus its value right
after initialisation. `no_leaks` tests use programs whose top-level variables are ints only, so the expected
value is exactly 0. `run` also compiles every program twice and asserts identical `to_bytes()`.

### Task 25: Layout and core codegen

**Files:** Create `src/layout.rs`, `src/codegen/{mod.rs,expr.rs,stmt.rs}`; Modify `src/lib.rs` (`compile`);
`quest/crates/quest-lang/Cargo.toml` (dev-dependency `quest-stub`); Test `tests/run_core.rs`

**Interfaces — Produces:** `compile` (D10). Fn ids are functions sorted by qualified name (init functions named
`$init::<qualified global>`); frames in fn-id order from slot 0; globals after the frames, modules in dependency
order then declaration order; `static_words` over 65 535 → `too-many-slots`. `lines` has an entry per statement
and per call.

Tests:
- `spec_5_4` — prints `total 130, stopped at 7`.
- `block_scope_and_shadowing` — a top-level `x = 1`, `let x = 2` in `main`, `let x = 3` in an `if`: prints `3` then `2`.
- `loops` — `while` with `break` and `continue`; `for i in range(2, 5)` prints `2 3 4`.
- `top_level_init_order` — a module's globals initialise before the module that `use`s them; in declaration order.
- `calls_three_deep`; `int_and_float_printing` — `7 / 2 == 3`, `7.0 / 2.0 == 3.5`, `float(3)` prints `3.0`.
- `runtime_fault_reports_the_line` — `let z = 0` … `1 / z` on line 4 → `HandlerAborted { Fault { DivByZero } }`
  with `stack[0].line == 4`, and nothing printed by that task.
- `bytecode_is_deterministic` — compiling the same tree twice, and with the files inserted in another order,
  gives identical bytes.
- `too_many_slots` — a generated program with 70 000 locals.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-lang --test run_core"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): frame layout and code generation for the core language`.

### Task 26: Codegen — data, places, `match`, collections

**Files:** Create `src/codegen/{place.rs,data.rs,matching.rs}`; Test `tests/run_data.rs`

A `for` over a place (a variable or a field/index path) re-reads the place at every step; over any other
expression it iterates a hidden copy.

Tests:
- `spec_5_3_copy` — `let b = a`, `b.append(3)` → prints `[1, 2] [1, 2, 3]`.
- `spec_5_5_total` — `Ledger(owner="ana", entries=[2, 3]).total()` is 5 (the `&self` method `add` is Task 28).
- `nested_place_assignment` — `q.rewards[2].count = 5` changes `q`; a copy of `q` taken before does not change.
- `spec_5_6_match` — each of the four arms, by constructing each stage.
- `dict_order` — insert `b`, `a`, `c`; remove `a`; insert `a`; iterate → `b c a`.
- `mutation_during_iteration` — `for x in xs:` appending 4 when `x == 1` prints `1 2 3 4`.
- `slices_and_in`; `fstrings_of_every_kind` — the D8 examples through f-strings.
- `structural_equality` — structs and lists with `==` and `!=`; struct as a dict key.
- `option_match` — `Some`/`None`; `d.get(k) else 0`.
- `no_leaks` — a `main` that builds and drops nested lists, dicts, structs and strings in loops, with early
  `return`, `break` and `continue` inside nested loops: `leaked_words == 0`. (Review Focus 5)

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-lang --test run_data"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): codegen for structs, enums, collections, places and match`.

### Task 27: Codegen — errors

**Files:** Create `src/codegen/errors.rs`; Test `tests/run_errors.rs`

Tests:
- `spec_5_7_forms` — one program per form (`?`, `else 0`, `else return`, `else err:` block, `match`), each with
  the chest holding 3 gold: expected lines written out in the test (e.g. the `match` prints `Only 3 left.`).
- `escaping_error_report` — an error raised two calls down and propagated by two `?`: the report's name, its data
  `have = 3`, and `trace` lines = the `raise` line, then each `?` line, in order.
- `option_question_mark`; `raise_with_data_bound_in_match`.
- `error_data_formats` — `f"{err}"` in an `else err:` block prints `NotEnough(have=3)`.
- `no_leaks_on_error_paths` — strings and lists live at a `?`, an `else return` and a `raise`: `leaked_words == 0`.
  (Review Focus 5)

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-lang --test run_errors"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): codegen for raise, ?, else and fallible match with return traces`.

### Task 28: Codegen — references and `&self`

**Files:** Create `src/codegen/refs.rs`; Test `tests/run_refs.rs`

Tests:
- `spec_5_3_reference` — `r.append(9)` → `a == [1, 2, 9]`.
- `spec_5_5_add` — `l.add(5)` then `l.total() == 5`; `self` by value does not change the caller's struct.
- `reference_into_a_place` — `&quest.rewards[2]`, write through it, print `quest`.
- `reference_passed_down_two_calls`; `deref_assign` — `*r = 7` on an `int` local and on a list element.
- `reference_to_a_top_level_variable_from_a_function`.
- `bad_ref_fault_line` — a reference to `xs[2]` after `xs.pop()` → `Fault { BadRef }` at the use's line.
- `no_leaks_with_references` — `leaked_words == 0`. (Review Focus 5)

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-lang --test run_refs"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): codegen for references and &self methods`.

### Task 29: Codegen — handlers, host calls, `await`

**Files:** Create `src/codegen/handlers.rs`; Test `tests/run_handlers.rs`

Tests (`main` emits events with `emit(...)`; the runner delivers them):
- `spec_7_handlers` — `emit(PlayerClicked(player=Player(id=1, name="ana")))` → `ana: Well met.`; the `warn`
  handler prints `ana: Begone.` only after `add_tag("outlaw")`.
- `handlers_are_plain_functions` — `main` calling `greet(...)` directly prints the same.
- `handler_order` — two handlers of one event in modules `a` and `b`: `a`'s runs first whatever the file order.
- `spec_8_2_ask` — with latency 2 and `dragons_slain = 4` stored: the lines in order, `await p else 0` yields 0
  when the key is missing.
- `spec_8_2_claim_race` — two `ItemDelivered` events: the first prints the `give` line, the second
  `Another claimed it first.`
- `state_changes_across_await_are_visible` — a second event's handler changes a top-level variable while the
  first is parked.
- `await_in_a_helper_function`; `await_all`; `detached_promise` (`let _ = world.set(...)` and the value is stored).
- `fault_after_await` — the lines printed before the `await` stay, those after do not, the earlier write is kept.
- `queue_overflow_from_scripts` — a handler looping for 20 ticks while `main` emitted 6 events → 4 run,
  `EventDropped { QueueFull }` twice, `EventQueued` reports present.
- `timeout_path` — a `Timeout` failure rule: the handler's `match` arm for `Timeout` runs at tick `issue + 200`.
- `no_leaks_across_awaits` — parked handlers holding strings and lists, resumed, aborted, and timed out:
  `leaked_words == 0`. (Review Focus 5)

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-lang --test run_handlers"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; the whole crate; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): codegen for handlers, host calls and await`.

---

## Phase 7 — The CLI

### Task 30: `quest check` and `quest run`

**Files:** Create `quest/crates/quest-cli/{Cargo.toml,src/main.rs,tests/cli.rs}`, `quest/README.md`; Modify
`quest/Cargo.toml`, `justfile` (the `quest-run` recipe, own commit)

**Interfaces — Produces:** the binary `quest`:

```
quest check <dir>
quest run <dir> [--fuel N] [--ticks N] [--mem BYTES] [--trace] [--latency N]
                [--fail KIND[@NTH][:OP]] [--chest KIND=N] [--save FILE] [--load FILE] [--reset]
```

`<dir>` is read recursively for `*.qs` into a `SourceTree` (paths relative to it, `/` separators). Defaults:
`--fuel 2000`, `--ticks 100000`, `--mem 262144`, `--latency 0`. Script output goes to stdout; diagnostics,
reports and trace lines to stderr. Trace line: `tick=<n> fuel=<used> hash=<16 hex digits> status=<idle|busy|failed>`.
A report prints as `error: handler <name> aborted: <fault or error text>` followed by one `  at <file>:<line>:<col>`
per location, or `warning: event <name> queued` / `error: event <name> dropped (<reason>)`. Exit codes: 0 finished;
1 compile errors; 2 a task aborted or the instance failed; 3 the tick limit was reached; 64 usage; 66 unreadable
directory or file. `--save` writes the VM state after the run; `--load` starts from one (Task 31 makes it migrate
when the program changed; here it requires the same program, else exit 2 with `LoadError`).

```just
# quest-run <dir> [flags] — compile and run a quest script directory (see quest/README.md for the flags).
quest-run dir *args:
    {{cap}} {{cargo}} run -q --manifest-path quest/Cargo.toml -p quest-cli -- run {{dir}} {{args}}
```

Tests (`env!("CARGO_BIN_EXE_quest")`, temp dirs under `CARGO_TARGET_TMPDIR`):
- `check_ok_is_silent_exit_0`; `check_reports_rendered_diagnostics_exit_1` — stderr equals the D10 block.
- `warnings_do_not_fail` — an `error-typo` warning prints, exit 0.
- `run_prints_output` — §5.4 prints `total 130, stopped at 7`, exit 0.
- `trace_lines` — `--trace --fuel 50`: one stderr line per tick matching the format; two runs are identical.
- `tick_limit_exit_3` — `while true: pass` with `--ticks 20`.
- `abort_exit_2` — a division by zero prints the report with its `at main.qs:<line>:<col>` line.
- `save_then_load_same_program` — a script counting runs in a top-level variable prints 1, then 2 after `--load`.
- `usage_errors_exit_64`; `missing_dir_exit_66`; `no_main_exit_1` (message: `no fn main() in main.qs`).

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-cli"` → FAIL.
- [ ] **Step 3:** implement (hand-written argument parsing; README: the commands, flags, exit codes).
- [ ] **Step 4:** PASS; `just quest-run <a temp dir with the §5.4 program>` prints the line; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): the quest CLI — check and run`; then the `quest-run` justfile commit.

---

## Phase 8 — Edits and Reset

### Task 31: State migration and Reset

**Files:** Create `quest/crates/quest-vm/src/migrate.rs`; Modify `src/{vm.rs,save.rs}`,
`quest/crates/quest-cli/src/main.rs`; Test `quest/crates/quest-lang/tests/run_edits.rs`, `quest-cli/tests/cli.rs`

**Interfaces — Produces:**

```rust
pub struct Orphan { pub name: String, pub ty: String /* fmt of the old type */, pub value: WireValue }
pub struct MigrationReport { pub carried: Vec<String>, pub initialised: Vec<String>, pub orphaned: Vec<String>,
                             pub adopted: Vec<String>, pub events_dropped: u32, pub handlers_cancelled: u32 }
impl Vm {
    pub fn migrate(self, new: Arc<Program>, cfg: VmConfig) -> Result<(Vm, MigrationReport), LoadError>;
    pub fn reset(&mut self);                 // epoch + 1; afterwards identical to Vm::new(program, cfg with that epoch)
    pub fn orphans(&self) -> &[Orphan];
}
```

`migrate` (§6.9), in order: abort an open segment; cancel parked handlers and free every promise record (later
answers are `Unknown`); read every global and every queued event out as wire values; build a fresh VM for the new
program with the same epoch; for each new global: a same-named old global or orphan whose value loads structurally
(`from_wire`) is carried / adopted and its init task is skipped, otherwise its initialiser will run and an
incompatible old value becomes an orphan; old globals with no new counterpart become orphans; queued events are
re-delivered in order (dropped when no handler hooks them or the shape no longer fits). Orphans are part of
`save` and `StateHash`. Both operations clear the undo log. CLI: `--load` of a state saved by another program
migrates and prints the report to stderr (`carried: a, b` …); `--reset` resets after loading.

Tests (`run_edits.rs`: compile program A, run, `migrate` to program B compiled from edited source, run):
- `same_name_compatible_is_carried`; `new_variable_is_initialised`; `removed_variable_is_orphaned` (listed with
  name, type text and value); `retyped_variable_is_orphaned_and_reinitialised` (`int` → `str`);
  `struct_with_a_new_field_is_incompatible`; `orphan_is_adopted_when_the_name_returns`.
- `queued_events_reach_the_new_handlers`; `events_without_a_handler_are_dropped` (`events_dropped == 1`).
- `parked_handlers_are_cancelled_but_writes_stay` — a handler parked after `await world.set("k", 1)` committed: the
  stub store holds `k`; the later answer is `Unknown`; `handlers_cancelled == 1`.
- `edit_mid_segment_aborts_it` — the half-run handler's change to a global is not carried.
- `reset_equals_fresh` — after a busy run with parked handlers, queued events and orphans: `reset()` then
  `mem_bytes()` equals those of `Vm::new` with `epoch + 1`, and `orphans()` is empty.
- `reset_revives_a_failed_instance`; `promise_ids_after_reset_do_not_repeat`.
- `no_leaks_after_migrate_and_cancel` — heap use after a migration that cancelled parked handlers equals a fresh
  instance's with the same globals. (Review Focus 5)
- CLI: `save_then_load_changed_program` — prints the carried value and the report; `--reset` prints initial values.

- [ ] **Step 1:** tests. - [ ] **Step 2:** `just quest-test "-p quest-lang --test run_edits"` → FAIL.
- [ ] **Step 3:** implement. - [ ] **Step 4:** PASS; `just quest-test "-p quest-cli"`; `just quest-check`.
- [ ] **Step 5:** Commit `feat(quest): script edits keep state by name, Reset re-initialises`.

---

## Phase 9 — Corpus and determinism

### Task 32: `quest-corpus` — the corpus, expectations, goldens

**Files:** Create `quest/crates/quest-corpus/{Cargo.toml,build.rs,src/lib.rs,src/harness.rs,tests/corpus.rs}`,
`quest/corpus/run/*`, `quest/corpus/fail/*`; Modify `quest/Cargo.toml`

**Interfaces — Produces:**

```rust
pub struct Case { pub name: &'static str, pub files: &'static [(&'static str, &'static str)],
                  pub expect: &'static str, pub golden: Option<&'static str> }
pub const RUN: &[Case];  pub const FAIL: &[Case];        // generated by build.rs from quest/corpus, sorted by name
pub struct CaseConfig { pub fuel: u64, pub ticks: u64, pub latency: u64, pub failures: Vec<FailureRule>,
                        pub chest: Vec<(String, i64)> }
pub fn config_of(case: &Case) -> CaseConfig;             // from main.qs's first line: `#! fuel=200 ticks=500 latency=2 fail=Lost@2 chest=gold=3`
pub fn golden_of(case: &Case) -> String;                 // compile + run → the golden text
```

Golden text: line 1 `bytecode <fnv64 of Program::to_bytes(), 16 hex>`; then one `tick <n> fuel <used> hash <16 hex>`
per tick; then `mem <fnv64 of the final mem bytes>`; then `output:` and the output lines; then `reports:` and one
line per report. `expect.txt` is hand-written: for `run/` the output lines, for `fail/` the rendered diagnostics.
`QUEST_BLESS=1` (native only) rewrites `golden.txt`; a golden changes only in a commit whose message says why.

Corpus `run/` (each from the spec section named, `expect.txt` written by hand before the first run):
`bindings` (5.1), `copies_and_refs` (5.3), `control` (5.4), `ledger` (5.5), `stage_match` (5.6), `errors_all_forms`
(5.7), `collections` (5.8), `numbers` (5.9: integer division, remainder, float printing, `sqrt`, `sin`, `pow`,
NaN), `modules` (the 5.10 tree), `handlers` (7), `promises_ask` and `promises_claim` (8.2, latency 2),
`long_loop` (a 50 000-iteration loop at `fuel=200`), `queue_overflow`, `timeout`, `fault_after_await`.
Corpus `fail/`: `recursion_direct`, `recursion_mutual`, `needs_annotation`, `non_exhaustive`, `impossible_arm`,
`ref_stored`, `ref_returned`, `unhandled_fallible`, `private_item`, `error_shape`, `unused_promise`,
`bad_indentation`.

Tests (`tests/corpus.rs`, native):
- `run_cases_match_expect` — every `RUN` case's output equals `expect.txt`.
- `fail_cases_match_expect` — every `FAIL` case's rendered diagnostics equal `expect.txt`.
- `goldens_are_current` — `golden_of(case) == case.golden` for every run case (with `QUEST_BLESS=1`: write instead).
- `every_run_case_has_a_golden`.

- [ ] **Step 1:** write the corpus sources, the `expect.txt` files and `tests/corpus.rs`.
- [ ] **Step 2:** `just quest-test "-p quest-corpus --test corpus"` → FAIL (no harness, no goldens).
- [ ] **Step 3:** implement `build.rs`, `harness.rs`; fix any spec example the compiler or VM gets wrong (a bug in
  an earlier task's crate is fixed there, with a test added to that crate).
- [ ] **Step 4:** `QUEST_BLESS=1 just quest-test "-p quest-corpus --test corpus"`, read every `golden.txt` once,
  then the plain run → PASS; `just quest-check`.
- [ ] **Step 5:** Commit `test(quest): the program corpus — every spec example, expectations and goldens`.

### Task 33: Behaviour suites over the corpus

**Files:** Create `quest/crates/quest-corpus/src/gen.rs`, `tests/{fuel.rs,rollback.rs,save_load.rs,faults.rs,robust.rs}`

**Interfaces — Produces:** `pub fn gen::program() -> impl Strategy<Value = String>` (native only): well-typed
sources by construction — top-level `int`, `list[int]`, `dict[str, int]` and one struct; a DAG of 1–4 functions;
one handler and a `main` that emits 0–3 events; statements from: arithmetic assignment reduced `% 1000`, list
append / pop / index write, dict set / remove, struct field write, `if`, `while` bounded by a counter ≤ 20, a call,
`raise` + `else`, `await world.add("k", n) else 0`, a reference write.

Tests:
- `fuel_equivalence` — every run case without promises (`latency` absent): fuel 7, 50 and 333 per tick each give
  the same output and the same final `script_bytes()` as fuel `u64::MAX`.
- `pause_points_repeat` — two runs of each case with its own config: identical per-tick hashes.
- proptest `rollback_bar_on_generated_programs` — generated program; at a random tick checkpoint the VM and clone
  the host; run `k` more ticks; roll back and restore: `hash64` equal; run again: the same hash at every tick and
  the same output.
- `save_load_at_every_tick` — every run case: for each of its first 200 ticks, save, load, continue: the remaining
  golden lines are identical.
- `each_fault_kind` — one source per `FaultKind` a script can cause (`IndexOutOfRange`, `MissingKey`,
  `IntOverflow`, `DivByZero`, `BadRef`, `HeapFull`, `BadFloatToInt`, `AssertFailed`, `TooManyCommands`,
  `TooManyPromises`, `TooManyParked`, `PromiseAlreadyAwaited`): a top-level counter set before the fault is back
  to its old value, nothing was printed by the task, the report names the kind and a line.
- proptest `compiler_never_panics` — random strings from the token alphabet (keywords, punctuation, names,
  newlines, runs of spaces) and random bytes decoded lossily, each up to 2 KiB: `compile` returns. (Review Focus 2)
- `generated_programs_compile` — 64 generated programs compile with no diagnostics (the generator is sound).

- [ ] **Step 1:** tests + generator. - [ ] **Step 2:** `just quest-test "-p quest-corpus"` → the new tests FAIL or expose bugs.
- [ ] **Step 3:** fix what they expose, each fix with a focused test in the crate that owns the bug.
- [ ] **Step 4:** `just quest-test` (everything) → PASS; `just quest-check`.
- [ ] **Step 5:** Commit `test(quest): fuel, rollback, save/load, fault and robustness suites`.

### Task 34: wasm32 determinism

**Files:** Create `quest/crates/quest-corpus/tests/determinism.rs`; Modify `quest/crates/quest-corpus/Cargo.toml`
(`[target.'cfg(target_arch = "wasm32")'.dev-dependencies] wasm-bindgen-test`; `proptest` under
`cfg(not(target_arch = "wasm32"))`; the native-only test files get `#![cfg(not(target_arch = "wasm32"))]`),
`justfile` (the `quest-wasm` recipe, own commit)

```just
# The quest corpus goldens (bytecode hash, per-tick state hash and fuel, final memory, output) recorded on
# native must be reproduced on wasm32 (node, wasm-bindgen-test-runner — from quest/'s own Cargo.lock).
quest-wasm:
    #!/usr/bin/env bash
    set -euo pipefail
    ver=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' quest/Cargo.lock)
    root="$PWD/quest/target/tools/wasm-bindgen-$ver"
    [ -x "$root/bin/wasm-bindgen-test-runner" ] || {{cap}} cargo install --locked wasm-bindgen-cli --version "$ver" --root "$root"
    {{cap}} {{cargo}} test --manifest-path quest/Cargo.toml -p quest-corpus --test determinism
    CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$root/bin/wasm-bindgen-test-runner" \
        {{cap}} {{cargo}} test --manifest-path quest/Cargo.toml -p quest-corpus --test determinism --target wasm32-unknown-unknown
```

Tests (`determinism.rs`, each `#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]` and
`#[cfg_attr(not(target_arch = "wasm32"), test)]`):
- `goldens_reproduce` — for every run case `golden_of(case) == case.golden` (bytecode hash: the compiler is
  deterministic; tick lines: state hash and fuel; `mem`: state bytes; output).
- `fail_diagnostics_reproduce` — every fail case renders the same text.
- `allocator_addresses_reproduce` — a fixed alloc/free sequence returns a literal address list written in the test.
- `float_bits_reproduce` — `sqrt(2.0)`, `sin(1.0)`, `cos(1.0)`, `pow(2.0, 0.5)`, `0.1 + 0.2` through a compiled
  program print the literal bit patterns written in the test (taken from the first native run).

- [ ] **Step 1:** the test file and the recipe. - [ ] **Step 2:** `just quest-wasm` → native PASS, wasm32 result observed.
- [ ] **Step 3:** fix every wasm32 difference at its source (a `usize` in a hash, an unordered container, a std
  float function); never by re-recording a golden per target.
- [ ] **Step 4:** `just quest-wasm` → both PASS; `just quest-check`.
- [ ] **Step 5:** Commit `test(quest): the corpus goldens reproduce on wasm32`; then the `quest-wasm` justfile commit.

### Task 35: Close-out

**Files:** Modify `quest/README.md`, this plan
(an "As built" section at the end: what differs from the plan, measured numbers)

- [ ] **Step 1:** Run `just quest-check`, `just quest-test`, `just quest-wasm`; record test counts and wall time.
- [ ] **Step 2:** Run `just smoke` → no `Compiling game` line; `git diff --stat Cargo.lock` unchanged by this work;
  `grep -rn "quest" Cargo.toml crates/*/Cargo.toml` shows only the `exclude` line.
- [ ] **Step 3:** Walk spec §11 line by line and name, in "As built", the test that covers each bullet; anything
  uncovered gets its test now.
- [ ] **Step 4:** `quest/README.md`: the language in one page (D11 and "Decisions to review" item 12 — what an author must know), the CLI,
  the tunables, how to bless goldens, and the testing rule (after the MVP: regression tests only, never
  pre-emptive ones; fuzzing later).
- [ ] **Step 5:** Commit `docs(quest): README, as-built notes`; ask Dawid for the whole-branch review.

---

## Spec coverage

| spec | tasks |
|---|---|
| §5.1 bindings, scope | 19, 25 |
| §5.2 types, inference | 20, 21 |
| §5.3 values, references | 5, 11, 24, 26, 28 |
| §5.4 functions, control flow, no recursion | 8 (validate), 20, 25 |
| §5.5–5.6 structs, enums, match | 21, 23, 26 |
| §5.7 errors, faults | 9, 12, 22, 27, 33 |
| §5.8–5.9 collections, strings, numbers | 5, 6, 9, 10, 21, 26 |
| §5.10 modules | 19 |
| §6.1–6.2 static frames, fuel | 8, 9, 10 |
| §6.3–6.5 flat memory, VM state, determinism | 2–7, 14, 34 |
| §6.6 undo, segments, buffered effects | 2, 3, 12, 13, 14, 33 |
| §6.7–6.8 goldens, versioning | 8, 14, 32, 34 |
| §6.9 edits, Reset | 31 |
| §7 events, queue | 12, 29 |
| §8 promises, namespaces | 13, 15, 24, 29 |
| §9 host interface, stub, CLI, diagnostics | 8, 15, 16, 30 |
| §10 layout, isolation, recipes | 1, 15, 16, 30, 32, 34 |
| §11 testing | every task; the cross-cutting suites in 32–34; the line-by-line check in 35 |
