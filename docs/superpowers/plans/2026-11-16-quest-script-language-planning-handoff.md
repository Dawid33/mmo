# Handoff: the quest script language — MVP (standalone)

**Date:** 2026-11-16
**For:** a fresh session (Fable 5.2).
**Job:** write the implementation plan for the quest script language MVP (`superpowers:writing-plans`), have
Dawid review it. Do **not** start implementing before he approves the plan.

**Input:** the approved design — `docs/superpowers/specs/2026-11-16-quest-script-language-design.md`. Read
it whole; it is the source of truth. This handoff only orients you and lists what the plan must respect.

**Output:** `docs/superpowers/plans/2026-11-xx-quest-script-language-mvp.md` (date you write it on).

## 1. What this is, in one paragraph

Players will build quests with in-world **quest blocks** that run code they type in (Minecraft command
blocks, Factorio-style events). The code is a language of our own: **Python syntax, Rust semantics** (block
scope, `let` shadowing, inferred static types, structs + `impl`, enums + exhaustive `match`, value semantics
with copy-on-write and second-class `&` references, lists/dicts), **Zig-style errors** (any name is an error,
inferred and unionable error sets, `?` / `else` / `match`), **no recursion** (static frames, Fortran-style),
**Rust modules**, **JS-style promises** (`await`) for external data. It runs on a **bytecode VM** whose whole
state is **one flat linear memory**, executed under a **fixed fuel budget per tick**, pausable at any
instruction and resumable next tick, deterministic bit-for-bit on native and wasm32, hashable, undoable
(rollback netcode) and serializable (save cuts). The MVP is **standalone only**: compiler, VM, a stub host and
a CLI, tested extensively before any game integration.

## 2. Hard constraints for the plan

- **Separate Cargo workspace at `quest/`** (own `Cargo.toml` `[workspace]`, own `Cargo.lock`, own
  `quest/target/`); the root `Cargo.toml` gets `exclude = ["quest"]`. Reason: the root workspace sets
  `[resolver] feature-unification = "workspace"` (`.cargo/config.toml`), so a member enabling a new feature on
  a shared dependency would rebuild the game; `Cargo.lock` and `target/` locks would also be shared. **Two
  agents will work at the same time — one on the game, one on this language — and must never block or
  rebuild each other.** Nothing in the root workspace may depend on `quest/`.
- The only allowed path dependency into the game tree is `crates/state-hash` (+ derive). Do not use `game`'s
  `#[rollback]` macro; the VM has its own undo log.
- `quest-vm` **and** `quest-lang` must build for `wasm32-unknown-unknown`; no Bevy, no tokio, no windowing.
- Determinism rules from `CLAUDE.md` apply (libm, no hash-ordered iteration in state or in the compiler,
  canonical NaN, no time/threads). The compiler must emit byte-identical bytecode on every platform.
- Justfile recipes for the workspace (`quest-check`, `quest-test`, `quest-run <dir>`, `quest-wasm`), each
  through `{{cap}}` like every other recipe; a short `CLAUDE.md` paragraph pointing at the workspace. The
  justfile and `CLAUDE.md` are shared with the game agent and with Dawid — keep those edits small and
  separate commits.
- The wasm determinism check can reuse the wasm-bindgen test runner the way `just determinism-wasm` does
  (justfile, around the `determinism-wasm:` recipe) — but from `quest/`'s own `Cargo.lock`.

## 3. Decided by Dawid (do not reopen)

Python syntax with `fn` (not `def`); `let` shadows, `=` reassigns; block scope; top-level `let` = the
block's persistent state; floats allowed (deterministic f64 via libm); lists and dicts with Python literal
syntax; values copied (Rust/Swift), `&a` for references; Rust enums and `match`; errors = Zig semantics
(any name is an error, error sets union) with Python-ish syntax; no recursion; Rust module system in the MVP;
event handlers via `@on(Event)` in game, `main()` for standalone; handler order not guaranteed to authors
but deterministic and **not** shuffled; one handler at a time per block; a small event queue (queued =
warning, overflow drops the newest + script error); JS-like promises with `await`; external data in
player-owned **namespaces**, reads and writes through the coordinator, **writes are final**; bytecode VM now
with **flat memory**, a **wasm backend later** (same memory layout); script edits keep state, **Reset**
re-initialises everything; separate workspace.

Everything marked **[call]** in the spec is the designer's call — you may revisit one in the plan, saying
why. §12 of the spec lists what is left to you (bytecode design, fuel table, tunable defaults, host trait
shape, string API, diagnostics format, crate internals).

## 4. Suggested shape of the plan (yours to change)

The spec's §11 (testing) is the acceptance bar — plan tests first, phase by phase. A natural order:

1. Workspace skeleton + `exclude` + recipes (prove the game build is untouched: `just smoke` before/after
   shows no rebuild of `game`).
2. Flat-memory layout, allocator, refcount/COW values, undo log, `StateHash` over the memory — testable
   without a compiler.
3. Bytecode + interpreter core with fuel and pause/resume (hand-assembled programs).
4. Lexer (indentation) + parser + modules/name resolution.
5. Types and inference (callee-first over the DAG), error-set inference, exhaustiveness, no-recursion check,
   reference restrictions, frame sizing → codegen.
6. Handlers, queue, segments (abort undo + buffered host commands), promises and parking, namespaces in the
   stub host with latency and failure injection.
7. Script edit migration + Reset; save/load mid-execution.
8. CLI (`check`, `run`, trace), diagnostics.
9. Determinism corpus: native goldens (hash, fuel, output, bytecode) reproduced on wasm32.

Keep each task small enough for a subagent (the plan will be executed with
`superpowers:subagent-driven-development`), each with its own tests and a commit.

## 5. Working-tree rules

- Work on `develop` in this checkout. **No git worktrees** (`CLAUDE.md`).
- Dawid and another agent commit concurrently and have uncommitted work in many files (incl. `Cargo.toml`,
  `justfile`, `CLAUDE.md`). Never `git add -A`, never `git stash`. Stage only your own files; for a shared
  file holding others' hunks, stage yours with a filtered patch (three lines of context, `--recount`, never
  `--unidiff-zero`) and check the index before committing.
- The spec and this handoff are not committed yet at the time of writing — check `git status`; if still
  untracked, commit them (only them) first with a `docs:` commit.

## 6. Not in scope

Any game integration (quest block entity, sim events, coordinator promise path, Postgres namespace storage,
Slint editor), the wasm backend, function values / generics / traits, namespace sharing, the language's
final name (`quest` / `.qs` are placeholders).
