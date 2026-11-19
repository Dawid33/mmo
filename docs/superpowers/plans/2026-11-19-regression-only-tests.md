# Regression-only tests in the game workspace — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete every unit and integration test in the root workspace (and `tools/`, `prototypes/`, `website/`,
`scripts/`) except regression tests, and adopt `quest/`'s rule: a test is written only as a regression for a bug that
was actually found.

**Architecture:** First an inventory: a script lists every test with its file, the commit that added it, and any
regression marker. Each test is then classified, and the owner reviews the list before anything is deleted. Deletion
goes crate by crate, and each kept test gets a uniform `Regression:` doc line. A checker (`just test-audit`, run from
`smoke`) then refuses any test without a `Regression:` or `Gate:` marker, so the rule is enforced from now on instead
of remembered.

**Tech Stack:** Rust (cargo test, `#[test]` / `#[tokio::test]` / `#[wasm_bindgen_test]`), Python 3 for the inventory
script, `just`, `git log -S` (pickaxe) for test origins.

**Spec:** the owner's request (2026-11-19): "Delete all unit tests and integration tests, keeping only regression
tests. We will follow the same testing methodology here as in the quest workspace." The methodology is the
CLAUDE.md paragraph **Tests in `quest/`** (owner's rule, 2026-11-16) and
`docs/superpowers/plans/2026-11-17-quest-script-completion.md` (Part 3, fuzzing).

## Size of the job (2026-11-19)

About 4,500 `#[test]`s in `crates/`: game 1496, client 968, rapier 417 (vendored), worldgen 413, server 318, parry 243
(vendored), world-store 211, physics-next 118, slint-ui 95, login 88, mc-import 45, svc-link 27, bbmodel 24, panel 21,
slotmapd 19 (vendored), state-hash 18, auth 11, block-mesh 9, login-api 3, pg-test-support 1. Outside `crates/`:
`tools/` 29, `prototypes/` 63, plus `website/tests/*.test.mjs` and `scripts/tests/`. Only about 20 comments say
"regression" today, and 493 of 1768 commits are `fix…`. So most tests will be classified by where they came from.

## Decisions — confirm at the Task 2 checkpoint (defaults below)

- **D1 What counts as a regression test.** A test that reproduces a bug that was actually observed: in play, in a
  review, in a hand check, in a log, or as a failure of another test. Evidence is one of: (a) a comment in or above the
  test naming the bug ("Regression", "the … desync", "was a bug", a dated live finding), or (b) the test was added by a
  `fix…` commit whose message describes the bug this test reproduces. A test that only rode along in a fix commit (it
  covers something else, or the fix's new feature) does not count. Each test is judged on its own.
- **D2 Gates stay** (marked `Gate:`, not regression tests): they are the determinism and quality contract that
  CLAUDE.md's rules point at, the counterpart of `quest/`'s corpus. The gates are `game` `wasm_determinism`,
  `physics-next` `state_hash_wasm` and `physics_goldens::`, `worldgen` `scene_hash_golden`, the `forge_quality` gate,
  and the `pipeline` `column()` ↔ `#[cfg(test)]` `DensityFn` mirror test (with the `DensityFn` graph it needs). Perf
  gates and `region_load_cost` are **not** gates: classify them like any other test.
- **D3 Vendored forks are untouched.** `crates/rapier`, `crates/parry` and `crates/slotmapd` carry upstream's tests
  (`just physics-next-upstream`); a re-import would bring them back. Every `physics-next` test that pins a
  `// mmo-fork:` hunk stays, marked `Regression:` with the hunk it pins (CLAUDE.md: every marked hunk is pinned by a
  test).
- **D4 The multi-host and seeded harnesses** (`server` `roles`, `hosts`, `takeover`, `threaded_world`, `coordinator_rt`;
  `game` `handoff_schedule`, `peer_schedule`, `peer_journal`) are classified test by test like the rest. Their
  support code (`server`'s `test-hooks` feature, `tests/common/`, `crates/pg-test-support`) stays only while a kept test
  uses it.
- **D5 Fuzzing is a separate, later plan.** This plan only deletes. Until the fuzzers exist, the gates and the kept
  regressions are the only automated net. Task 12 adds a TODO.md entry for the fuzz plan (candidates: the wire codec
  `decode_packet`, the world-store blob decoders, `HostMsg`/`CoordMsg`/peer message decoding, rollback undo
  (`hash(before) == hash(after undo)`) over random inputs, the seeded handoff schedule).
- **D6 Owner's uncommitted edits are never swept.** The working tree has the owner's changes (e.g.
  `crates/world-store/tests/fixtures/world_map.bin`). Each commit uses a pathspec (other sessions share the index);
  check `git diff --cached --name-only` before committing. A file with the owner's hunks is staged hunk by hunk (`-U3 --recount`), never
  wholesale.

## Global Constraints

- Never create git worktrees; work on `develop` in this checkout.
- Every build and test goes through `just` (the CPU cap) or is prefixed with the cap.
- No whole crate suite per task. Each task verifies by building its test targets (`--no-run`) plus `just smoke`.
  Everything runs once, through the new `just test`, in Task 13 (tell the owner first; it includes the client tests).
- Never format from the repo root or with `--all`.
- A kept test keeps its slow tag if it has one; Task 12 rewrites the tag text to
  `#[ignore = "slow: >100ms — run via just test"]` everywhere (`test-full` is gone).
- Marker format, used by the checker (Task 3): the doc comment directly above the test fn (attributes may sit between
  it and the fn) contains a line starting `Regression:` or `Gate:`. Example:
  `/// Regression: a LOD root arriving as hundreds of cells starved the mesher (live, 2026-09-19; fix a1b2c3d4).`
- `quest/` is out of scope (it already follows the rule).

## Review Focus

- **A kept test that calls a helper in a deleted module.** `tests/common/`, `#[cfg(test)]` helpers in `src`, `it.rs`
  `mod` lines: after a deletion, building with `--no-run` must succeed. Each crate task ends with that build.
- **A recipe that names a deleted test or target** (`smoke`'s `simple:: region_runner::`, `test-threads`'
  `seam_tests` list and `voxel_body` names, `bbmodel creatures::`, `region_load_cost`, `test-r2/r3/r4`,
  `client-wasm`'s `web_frame_wasm`, `slint-wasm`'s `wasm_images`). A filter that matches nothing passes silently.
  Task 12 greps every recipe filter against the remaining test names, and the closing task's `just test` runs everything.
- **A dead dev-dependency or `[[test]]` target left behind** (an empty target fails the build; an unused
  dev-dependency only costs compile time). Each crate task removes them.
- **A test that is also a gate's fixture.** `world-store/tests/fixtures/world_map.bin` and the golden hash files are
  data that gates read. Delete a fixture only when no kept test reads it (grep its name).
- **Over-keeping.** A whole file kept because its header says "regression" when only one test in it is. The marker
  goes on each test, not on the file.

---

### Task 1: The inventory script

**Files:**
- Create: `scripts/test_inventory.py`
- Create: `docs/superpowers/plans/2026-11-19-regression-only-tests-inventory.tsv` (generated, committed)

**Interfaces:**
- Produces: `scripts/test_inventory.py inventory > <tsv>`. Columns: `crate`, `file`, `line`, `name`, `kind`
  (`unit` = under `src/`, `integration` = under `tests/`, `wasm`, `js`, `py`), `origin` (short hash), `origin_subject`,
  `marker` (the first matching comment line, trimmed to 120 chars), `proposed`, `reason`.
- `proposed` is one of `keep-regression`, `keep-gate`, `review`, `delete`, `vendored`.

- [ ] **Step 1: Write `scripts/test_inventory.py inventory`.** Walk `crates/`, `tools/`, `prototypes/` for `*.rs`
  (skip `target/`). Find each test: an attribute line matching `#\[(tokio::)?test|#\[wasm_bindgen_test` followed by
  `fn <name>` within the next 6 lines, and each `proptest!` block's fns. Also list `website/tests/*.test.mjs`
  (`test(`/`it(` calls) and `scripts/tests/` (`def test_`). For the marker, take the comment block above the test plus
  the test's body. Match `(?i)regression|was a bug|used to (panic|crash|fail|hang)|desync|live,|found (in|by)|repro`.
- [ ] **Step 2: Origin.** For each test, `git log --reverse --format='%h %s' -S 'fn <name>(' -- <file>` and take the
  first line. If that is empty (the file moved), repeat without the path and take the first commit whose diff adds
  `fn <name>(`. Cache per file: one `git log -p --reverse --format=… -- <file>` per file, parsed for added
  `fn` lines, is much faster than one pickaxe per test. Fall back to the pickaxe only for names the cache missed.
- [ ] **Step 3: Proposal.** `vendored` for paths under `crates/{rapier,parry,slotmapd}`. `keep-gate` for the D2 list
  (by file and name). `keep-regression` when there is a marker AND the origin subject starts with `fix`. `review`
  when only one of the two holds, and for every test in `physics-next` (D3). `delete` otherwise. `reason` says which
  rule fired.
- [ ] **Step 4: Run it** (`python3 scripts/test_inventory.py inventory > docs/…-inventory.tsv`). Expected: about 4,600
  rows, and per-crate counts within a few percent of the "Size of the job" table. Print a summary: rows per crate ×
  `proposed`.
- [ ] **Step 5: Commit** the script and the TSV (pathspec), message `chore(tests): inventory every test and its origin`.

### Task 2: Classify, then the owner's checkpoint

**Files:**
- Modify: `docs/superpowers/plans/2026-11-19-regression-only-tests-inventory.tsv` (`proposed`, `reason`, plus a new
  column `bug` = one line naming the bug and its evidence)

- [ ] **Step 1: Resolve every `review` row and check every `keep-regression` row.** Read the test and its origin
  commit (`git show --stat <origin>` plus the message). Apply D1: set `keep-regression` with `bug` filled
  (e.g. `browser input drop: SyncClock sampling flapped — fix 1a2b3c4`), or `delete`. Hand this out per crate (one
  subagent per crate for game, client, worldgen, server; one for the rest). Each subagent gets D1–D4 and its crate's
  rows, and returns its rows only.
- [ ] **Step 2: Spot-check the deletions.** Sample 30 `delete` rows whose origin subject starts with `fix` and confirm
  that none reproduces the fixed bug. If more than 3 are wrong, redo Step 1 for that crate.
- [ ] **Step 3: Summarise for the owner.** Give counts per crate (kept regression / gate / vendored / deleted), list
  every kept test with its `bug`, list every gate, and confirm D1–D6 hold.
- [ ] **Step 4: STOP.** The owner reviews the TSV and the decisions. Do not start Task 4 until they approve. Apply
  their changes to the TSV.
- [ ] **Step 5: Commit** the TSV, message `chore(tests): classify every test — regression, gate or delete`.

### Task 3: The rule checker — `just test-audit`

**Files:**
- Modify: `scripts/test_inventory.py` (add `check`)
- Modify: `justfile` (recipe `test-audit`; `smoke` runs it first)

**Interfaces:**
- Produces: `python3 scripts/test_inventory.py check [paths…]`. It exits 1 and lists `file:line name` for every test
  outside `crates/{rapier,parry,slotmapd}` whose doc comment has no line starting `Regression:` or `Gate:`. Otherwise
  it exits 0 and prints `test-audit: N regression, M gate`.

- [ ] **Step 1: Write `check`.** It reuses the inventory's test finder and reads the doc comment as in the Global
  Constraints marker rule.
- [ ] **Step 2: Run it now.** Expected: exit 1, listing every test (nothing is marked yet). Running
  `check crates/state-hash` lists that crate's 18 tests.
- [ ] **Step 3: Add the `test-audit` recipe, but not to `smoke` yet.** Task 12 adds it to `smoke` once every crate is
  done. Until then, each crate task runs `just test-audit crates/<crate>`.
- [ ] **Step 4: Commit** (`scripts/test_inventory.py`, `justfile` hunk only), message
  `feat(tests): test-audit — every test carries a Regression: or Gate: marker`.

### Tasks 4–11: Delete, crate by crate

Every crate task uses the same steps. The groups:

| Task | Crates / paths |
|---|---|
| 4 | `auth`, `login-api`, `block-mesh`, `state-hash`, `svc-link`, `bbmodel`, `mc-import`, `panel`, `login`, `pg-test-support` |
| 5 | `world-store` |
| 6 | `worldgen` |
| 7 | `physics-next` |
| 8 | `game` |
| 9 | `server` |
| 10 | `slint-ui`, `client` (client: build-check only, via `smoke`'s `cargo check -p client --tests`) |
| 11 | `tools/`, `prototypes/`, `website/tests/`, `scripts/tests/` |

- [ ] **Step 1: Delete the `delete` rows** of the group's TSV rows: whole files when every test in them goes, else the
  fns (and a `mod tests` block left empty). Remove the dead pieces too: emptied `tests/it.rs` `mod` lines, emptied
  `[[test]]` targets in `Cargo.toml`, `#[cfg(test)]` helpers and test-only `pub(crate)` items nothing uses any more,
  unused `tests/common/` helpers, fixtures no kept test reads, and dev-dependencies nothing uses. `server`: the
  `test-hooks` feature and its `required-features` go only if no kept test needs them. `pg-test-support` goes from
  the workspace only if neither `world-store` nor `login` keeps a Postgres test. `worldgen`: the `DensityFn` graph
  stays (D2).
- [ ] **Step 2: Mark each kept test.** Put a `/// Regression: <bug> (<evidence>)` line from the TSV's `bug` column on
  each kept regression, and `/// Gate: <what it guards>` on each gate. Fold an existing "Regression…" comment into that
  line instead of keeping both.
- [ ] **Step 3: Build.** Run `just test-one <crate> "--no-run"` for each crate in the group (the server one with
  `--features test-hooks` if that feature still exists; `client` is covered by Step 5). Expected: it compiles, with no
  new `unused` warnings (`dead_code`, `unused_imports`) in that crate.
- [ ] **Step 4: Audit.** Run `just test-audit crates/<crate>` for each crate. Expected: exit 0.
- [ ] **Step 5: Smoke.** Run `just smoke`. Expected: pass. While a recipe still names a deleted filter (`smoke`'s
  `simple:: region_runner::`), fix that recipe line in this task: drop the filter, or point it at kept tests of the same
  crate. A filter left matching zero tests is a bug (Review Focus).
- [ ] **Step 6: Commit** with a pathspec of the group's paths and `justfile` hunks only (D6). Message
  `test(<crates>): keep only regression tests and gates`, with the counts (deleted / kept) in the body.

### Task 12: Recipes, rules and docs

**Files:**
- Modify: `justfile`
- Modify: `CLAUDE.md` (Testing section, plus the quest paragraph's cross-reference)
- Modify: `TODO.md`

- [ ] **Step 1: Check every recipe filter against the remaining tests.** For each `cargo test` line in the justfile,
  each filter (`seam_tests`, the `voxel_body` names, `creatures::`, `region_load_cost`, `roles`, `hosts`, `takeover`,
  `threaded_world`, `web_frame_wasm`, `wasm_images`, `wasm_determinism`, `state_hash_wasm`) must match at least one
  remaining test (grep the inventory). A recipe whose tests are all gone is deleted. Then remove it from CLAUDE.md
  and from any recipe that calls it (e.g. `test` → `test-game test-client …`).
- [ ] **Step 2: Add `test-audit` to `smoke`** as its first step (it takes well under a second).
- [ ] **Step 3: One `just test` runs every test everywhere** (the owner's ask, 2026-11-19). Replace today's `test`
  (which chains `test-game test-client test-server test-worldgen` and skips ignored tests) and `test-full` with one
  bash recipe. It runs each stage below in order and **keeps going after a failure**. At the end it prints one
  `PASS`/`FAIL` line per stage and exits 1 if any stage failed. Every `cargo` line is under `{{cap}}` and passes
  `--include-ignored` (slow-tagged tests included; the 100 ms tag now only keeps `test-one` fast). There are no
  `--skip`s: under the new rule every remaining test is a regression or a gate, so all of them run. The stages:
  1. `test-audit`.
  2. The root workspace: `cargo test --workspace --exclude rapier3d --exclude parry3d --exclude slotmapd -- --include-ignored --test-threads={{test_threads}}`,
     with `--features server/test-hooks` if that feature survived Task 9. Before writing the line, check the vendored
     crates' real package names (`cargo metadata --no-deps`).
  3. The vendored forks: `physics-next-upstream`, plus `cargo test -p slotmapd`.
  4. The serial multi-host and threaded targets, `--test-threads=1` (whichever of `roles`, `hosts`, `takeover`,
     `threaded_world` survived), with `MMO_TEST_DATABASE_URL` set as `test-r2/3/4` set it. Stage 2 must `--skip`
     exactly these targets so they do not run twice in parallel. These two lines are the only filters allowed, and
     a comment says why.
  5. Postgres: the recipe depends on `pg-up` (it waits for the database) and exports `MMO_TEST_DATABASE_URL` for every
     cargo stage, so `world-store` and `login` Postgres tests run in stage 2 instead of skipping.
  6. wasm: `determinism-wasm`, `slint-wasm`, `client-wasm` (those whose targets survived).
  7. `quest-test`, and `quest-fuzz-replay <t>` for each directory under `quest/fuzz/regressions/`.
  8. Outside the workspace: `water-proto-test -- --include-ignored` if `prototypes/water` keeps a test. `tools/*` are
     workspace members and run in stage 2.
  9. Non-Rust: `site-test`, `python3 -m unittest discover -s scripts/tests` (if a test survived Task 11), and
     `site-theme-check`.

  Delete `test-game`, `test-client`, `test-server`, `test-worldgen` and `test-full`, and every reference to them, including
  the slow-tag text `run via just test-full` → `run via just test` in every kept test
  (justfile comments, CLAUDE.md, `docs/` text that tells a reader to run them; leave dated plans alone).
  `test-one`, `test-pg`, `test-login-pg`, `test-r2/3/4` and `test-threads` stay as narrow entry points if their
  tests survived. Benchmarks that are not tests (`keepup-budget`, `soak`, `region_load_cost`'s recipe, `perf-*`)
  keep their own recipes and are not part of `just test`, unless Task 2 kept their test as a regression or gate. A
  kept one runs in stage 2 like any other.
- [ ] **Step 4: Rewrite CLAUDE.md "Testing".** Lead with the rule, worded like the quest paragraph: from 2026-11-19 the
  whole repository follows it. A test is written ONLY as a regression for a bug that was actually found, never
  pre-emptively, not "for coverage", not alongside a new feature. It carries `/// Regression: <bug> (<evidence>)`.
  Gates (`/// Gate:`) are the determinism and quality goldens of D2, and a new gate needs the owner. `just test-audit`
  (in `smoke`) enforces both markers. Broad assurance comes from fuzzing (the follow-up plan). Keep the verification
  bullets that still hold (`test-one`, `smoke`, `determinism-wasm`, `test-threads` if it survives, the 100 ms slow
  tag, `install_real_block_props`). Replace "`just test` / `just test-full` only when the user asks" with: "`just test`
  runs every test in the repository (both workspaces, the vendored forks, wasm, Postgres, the quest fuzz regressions,
  the website and scripts). Run it before calling a wide change done, and say so first: it is long". Drop any bullet whose tests are gone. Point the quest paragraph at the shared rule
  instead of restating it.
- [ ] **Step 5: Fix the architecture rules that cite deleted tests.** Grep CLAUDE.md for test names and paths
  (`physics_goldens`, `handoff_schedule`, `threaded_world`, `peer_schedule`, `scene_hash_golden`, `forge_quality`,
  `DensityFn`, `test-r2/3/4`, `twin-check` excepted). Each one either still exists or its sentence is reworded.
- [ ] **Step 6: TODO.md.** Add an entry: "Fuzzing for the game workspace (plan to write), replacing the deleted broad
  tests", listing D5's candidates.
- [ ] **Step 7: Commit** (pathspec; `CLAUDE.md`, `TODO.md` and `justfile` carry owner hunks, so stage per hunk). Message
  `docs: the game workspace keeps only regression tests and gates; test-audit in smoke`.

### Task 13: Closing — `just test`, once

- [ ] **Step 1:** `just test-audit`. Expected: exit 0, and counts equal to the TSV's kept rows.
- [ ] **Step 2:** Tell the owner that `just test` is about to run (it is long; it includes the client tests, which
  CLAUDE.md makes opt-in), then run `just test`. Expected: every stage prints `PASS`, and the exit code is 0. A
  failure means a kept test lost a helper or a fixture, or the recipe is wrong: fix that. Never delete the test to
  make it pass.
- [ ] **Step 3:** Prove the recipe reports failures: break one kept test locally (flip an assertion), run that stage
  alone, and confirm the summary line says `FAIL` and the exit code is 1. Revert the change.
- [ ] **Step 4:** `just smoke` and `just clippy`. Expected: pass.
- [ ] **Step 5:** Report the final numbers (tests before → after, per crate), the recipes removed, the `just test`
  run's wall time per stage, and the CLAUDE.md changes. Commit any fix from Steps 2–4 with a pathspec.
