# Handoff: profile every test with perf and find what is slow about them

**Written:** 2026-10-31. **For:** a fresh session with no context. Read this whole file, then CLAUDE.md's "Building and Running" and "Profiling" sections.

## Goal

For every test in the first-party crates, find out where its time goes, aggregate that across the suite, and say for each costly group **whether it can be made faster without weakening what the test proves, how, and what that would save**. "It can't" is an acceptable and expected answer for many tests; say so explicitly instead of inventing a fix.

This is research plus tooling, not an optimisation pass: **change no test or product code** beyond the profiling tools listed under *Deliverables*.

## Deliverables

1. `scripts/test_profile.py` (or a `.sh` driver plus the `.py` reducer): the reusable pipeline below (inventory → time → perf → aggregate). It must be resumable: it skips any test whose output already exists.
2. A `just test-profile` recipe wrapping it, with a CLAUDE.md "Profiling" bullet in the same change (CLAUDE.md: keep the justfile in sync).
3. `docs/superpowers/specs/2026-10-31-test-profiling/`:
   - `00-synthesis.md`: the answer (format under *Phase 4*).
   - `tests.csv`: one row per test: crate, binary, test name, ignore reason (if any), wall s, user s, sys s, max RSS MiB, CPU/wall ratio, pass/fail, profiled (y/n), top cause bucket, top mechanism bucket.
   - `per-test/<crate>__<binary>__<test>.txt`: the `scripts/perf_summary.py --top 25` output for each profiled test. Commit these reduced tables, never the folded stacks or `perf.data` (those stay under `target/test-profile/`).
4. A memory entry (and MEMORY.md one-liner) pointing to the synthesis, with "don't re-profile" plus the git rev it was measured at.

## Hard rules

- **Ask the user before each long run** (Phases 1, 2 and any A/B). They run their own benchmarks on this box; tell them the expected duration. Before measuring, check the power source (CLAUDE.md: on the ~88 W USB-C PD source every core clamps to 400–700 MHz; check `scaling_cur_freq` under load). Record power and frequency in the synthesis.
- **No worktrees.** Work on `develop` in the main checkout. The tree holds lots of the user's own uncommitted work: stage only files you created (`git add <paths>`).
- **Never propose disabling or weakening hash verification** (user directive). Tests pin `HashMode::Full` (the process-global default). Report hashing as its own bucket, with its cost. A proposal to run a non-rollback test under `Delta` is a **[DECIDE] for the user**, written as such with its trade-off, never a recommendation to turn checking off. Don't propose changes to the interval hash or the Full/Delta machinery.
- **Don't churn the shared `target/`.** Anything that changes rustflags or profiles (frame pointers, a per-package `opt-level` A/B) goes in a separate `CARGO_TARGET_DIR=target/test-profile-build`. Ask first: it is a full cold build, Bevy included.
- Build through `just` / the `cap` prefix (`nice -n 10 taskset -c 0-15`) like every other build. Run the measured test processes under the same `taskset` so numbers match what developers wait on.

## Context you need

- **Inventory (2026-10-31).** About 1,540 first-party tests:
  - `game`: 48 integration binaries and about 507 tests, 21 tagged slow.
  - `client`: about 660 tests, of which about 630 are in `--lib`, one Bevy-sized binary; 24 modules build a Bevy `App`; 8 integration binaries.
  - `worldgen`: 12 binaries and about 158 tests, 21 slow.
  - `physics-next`: 18 binaries and about 117 tests.
  - `slint-ui`: 8 binaries and 40 tests, 30 slow (software Slint rendering).
  - `server`: 2 binaries, all slow.
  - `bbmodel` and `state-hash`: small.

  Slow tests are `#[ignore = "slow: >100ms — run via just test-full"]`; the other ignore reasons mark perf gates, benchmarks and known bugs (see *Exclusions*). The vendored forks (`rapier`, `parry`, `slotmapd`, `block-mesh`) are **out of scope**.
- **Build profile.** Tests run in the dev/test profile: workspace crates at `opt-level = 1`, dependencies at 3, `worldgen` at 3, full debug info. So `game`'s sim runs at O1 in every test. That alone may be the biggest single factor, and it's one of the hypotheses below.
- **Already known and planned** (`docs/superpowers/plans/2026-10-31-test-speed-mold-and-fixture-regions.md`):
  - `tree_fall::young_trees_land_on_the_ground` runs about 81 whole-region sim geneses.
  - `trees_in_terrain` generates its forest region 7 times.
  - Worldgen tests bypass the worldgen disk cache.
  - mold linking.

  Profile on the current `develop` regardless, note in the synthesis whether that plan had landed (`git log --oneline -- crates/worldgen/tests/tree_fall.rs`), and don't re-derive its findings beyond confirming their size.
- **Existing tools to reuse:**
  - `scripts/perf_summary.py <folded> [--top N] [--thread T] [--prefix P]`: self and inclusive tables per thread and crate.
  - `inferno-collapse-perf` is on PATH.
  - `perf` 6.8 is installed and `perf_event_paranoid = -1` (no sudo needed).
  - **`perf script --no-inline` is mandatory**: without it, perf's addr2line coprocess livelocks on these binaries ("could not read first record").
- **Relevant memories** (in `~/.claude/projects/-home-dawids-Projects-mmo/memory/`): `never-skip-hash-verification`, `sim-cost-measure-hash-off` (harness ticks measured 80–90% `crc32fast` under Full), `headless-harness-world-size` (client harnesses must stay small or they OOM), `genesis-cost-reduction-landed` (slow rigs peak about 6.7 GB RSS each), `check-before-running-benchmarks`, `laptop-power-clamp`.

## Method

### Phase 0: inventory (no timing; safe to run)

1. Build every test binary of the in-scope crates (`game client server worldgen physics-next slint-ui bbmodel state-hash`): `cargo test -p <crate> --no-run --message-format=json`. Take the `executable` of each `compiler-artifact` whose `profile.test` is true, with its package and target name. This is a full build of every test binary (minutes, mostly the client); go through `cap`.
2. List tests per binary with `<bin> --list --format terse` (all tests) and `<bin> --list --format terse --ignored` (the ignored subset).
3. Get each ignored test's reason by grepping `#[ignore = "…"]` above `fn <name>` in the source. Classify it as `slow` (run it), `excluded` (see below) or `none`.
4. Write the skeleton `tests.csv`.

**Exclusions** (record them in the CSV with the reason; don't run them):
- Wall-clock perf gates and benchmarks:
  - `keepup_budget::region_tick_cost_fits_keepup_budget`
  - `reconcile_keepup::soak_realtime_keepup`
  - `region_load_cost`, `client_region_load_cost`
  - `perf_evidence_*`
  - anything ignored as "benchmark" or "timing".
- "long manual stability validation" tests.
- The known-bug ignore (soft-vs-soft explosion).
- The ~43k-body stress scene.
- `wasm32`-only tests: `wasm_determinism` and `state_hash_wasm` are run natively only.
- Skip-when-absent tests (e.g. `tile_map::default_world_map_builds` needs `worlds/default.world`): run them, but record whether they actually did anything.

### Phase 1: time every test on its own (ask first; expect roughly 30–90 min serial)

For each test, from the **package root as cwd** (that's what `cargo test` does, and some tests read repo-relative paths at runtime):

```
taskset -c 0-15 /usr/bin/time -f '%e %U %S %M' <bin> --exact '<name>' --include-ignored --test-threads=1 -q
```

- Use a per-test timeout of 600 s, and record timeouts as such.
- Leave `RUST_LOG` unset and `VOXEL_WORLD_FILE` unset (procedural world, like every test run).
- Record the `WORLDGEN_CACHE` state and whether `target/worldgen-cache/` was warm.
- Also run each binary once with a filter that matches nothing, to get per-process startup overhead. Subtract it in the CSV (relevant for the Bevy-sized client binary).
- **Sum check:** the per-crate totals should roughly match a plain `just test-one <crate> "-- --include-ignored --test-threads=1"` run. If they differ by >20%, find out why (parallelism inside tests, shared `LazyLock` setup paid only once per binary) before going on.

### Phase 2: perf the tests that matter (ask first; expect 1–2 h)

- Profile every test with wall ≥ 0.25 s (tunable), plus any binary whose many small tests add up to ≥ 5 s, where you profile the whole binary in one run.
- Command:

  ```
  perf record -e cpu-clock -F <f> --call-graph dwarf,16384 -o target/test-profile/<id>.data -- <bin> --exact '<name>' --include-ignored --test-threads=1
  ```

  Choose `<f>` as roughly 20,000 samples ÷ wall seconds, clamped to 99..4999, so every profile has a similar sample count and dwarf unwinding stays affordable.
- Then run `perf script --no-inline -i … | inferno-collapse-perf > target/test-profile/<id>.folded`, reduce with `scripts/perf_summary.py`, and delete the `.data` file. Dwarf `perf.data` files run to GBs.
- **On-CPU only.** `cpu-clock` samples only on-CPU time. For tests with CPU/wall < 0.5 (sleeps, `recv_timeout`, `pump_until` deadlines, real region threads ticking at `TICK_RATE`), the waiting is invisible to perf. Look at the source for the wall-clock waits instead, and if needed take one off-CPU profile (`perf record -e sched:sched_switch -g`) for the worst of them.
- For multi-threaded tests (`server/threaded_world`, client harness, region threads), keep per-thread tables (`--thread`).

### Phase 3: aggregate

Attribute every sample of every profiled test twice, then sum both views across the suite, weighting by the test's on-CPU seconds:

- **Cause** (the *outermost* matching frame): which part of the test is the work? Starting rule list (refine as you read stacks):
  - worldgen generation (`worldgen::generate_region`, `generate_columns`, `forged::`, `forge::`)
  - region genesis (`seed_terrain_genesis`, `seed_ground_genesis`, `Region::from_columns` / `from_chunks`)
  - sim ticks (`Region::tick`, `World::tick`, `RegionRunner::tick`, `handle_event`)
  - rollback/undo (`undo`, `revert`, `StepJournal`)
  - LOD build (`lod::`, `LodBuilder`)
  - client Bevy app setup vs updates (`App::new` / plugin build vs `App::update`)
  - meshing (`meshing::`)
  - Slint rendering (`i_slint_`, `slint::`)
  - test-side helpers and oracles (frames in the test binary's own crate, e.g. `tree_chop::`, reference implementations)
  - serialization (`bincode`, `lz4`)
  - other
- **Mechanism** (the *innermost* matching frame): what is the CPU actually doing?
  - state hashing (`crc32fast`, `state_hash`, `StateHash`)
  - rapier/parry broad, narrow and solver phases, and voxel shape builds
  - allocation and copying (`malloc`, `free`, `memcpy`, `realloc`, `drop_in_place`)
  - `BTreeMap` / `HashMap` operations
  - noise sampling
  - bincode/lz4
  - Bevy scheduling
  - Slint software rasterisation
  - other

Report the "other" share; if it's above 20%, the rules need work.

### Phase 4: judge and write `00-synthesis.md`

**Hypotheses to confirm or refute, with numbers.** Each gets a verdict:

1. `HashMode::Full` hashing dominates the sim-heavy `game` / `physics-next` / `worldgen` tests (the 2026-09-20 measurement said 80–90% of harness ticks).
2. `opt-level = 1` on workspace crates makes sim tests several times slower than they need to be. If Phase 3 points this way, ask, then A/B the top ~20 tests with `[profile.test.package.game] opt-level = 2|3` (the `#[rollback]` macro's generated code compiles inside `game`, so that one package covers it; add `client` only for the client tests) in the separate target dir. Report the speedup against the extra compile time per `game` edit: that trade-off is the decision.
3. Repeated worldgen and genesis (from the existing plan). Confirm the size; don't re-plan.
4. Client lib tests pay Bevy `App` construction per test rather than per assertion.
5. The `slint-ui` tests are dominated by software rendering of full-window frames.
6. The `server/threaded_world` and `handoff_schedule` time is wall-clock waiting on tick cadence, not CPU.

**For each costly group** (a group is a set of tests sharing a cause, e.g. "tests that tick a generated region for N seconds"), write:
- what is slow, with the cause and mechanism shares;
- the tests in the group and their combined wall time;
- why: incidental (setup, redundancy, oversized scene, too many seeds or iterations, debug opt-level, waiting) or **inherent** (the expensive thing is what the test exists to check);
- the candidate fix, its estimated saving, and its risk;
- **whether the fix changes what the test proves**, and why not if you claim it doesn't.

Keep fixes to test code and test infrastructure separate from **product hot spots** that tests happen to expose: a slow function in `game` makes the game slow too. Those are worth knowing, but they're a different kind of fix.

**Synthesis layout:**
1. A verdict paragraph.
2. Conditions: git rev, dirty state, power, frequency, cache state.
3. The top 30 tests by wall time.
4. Cause and mechanism totals.
5. The hypotheses with their verdicts.
6. Groups ranked by achievable saving.
7. An "inherent — leave alone" list.
8. [DECIDE] items for the user.
9. How to re-run: `just test-profile …`.

## Done when

- `tests.csv` covers every in-scope test (run, excluded with a reason, or failed with its output noted).
- Every test with wall ≥ 0.25 s has a per-test summary.
- The "other" mechanism share is ≤ 20%.
- `00-synthesis.md` answers each hypothesis with numbers.
- The recipe and its CLAUDE.md line are in; the memory entry is written.
- Commit only the new script(s), the justfile, CLAUDE.md and the specs directory, after asking the user whether to commit.
