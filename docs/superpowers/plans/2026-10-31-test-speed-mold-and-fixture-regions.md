# Test Speed: mold Linker + Fixture Regions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut test wall time by (a) linking with mold instead of rust-lld, kept only if measured faster, and (b) generating each pristine test region at most once per test binary and once per source state across runs, and not re-running sim genesis for scenes that are bit-identical.

**Architecture:** mold is wired in `.cargo/config.toml` for the host target only, so raw `cargo`, rust-analyzer and `just` share one build fingerprint, the same reason the region dial lives there. Worldgen fixtures go through a new `WorldgenCache::for_fixture` disk cache. Its key adds the `Materials` digest and the `crates/game/src` sources to the production key. On top of it sits an in-process, per-key `OnceLock` memo in a shared test-support module. `tree_fall` stops building a region for its probe scene and forks its second cut from a `SerializedRegion` copy of the pristine scene instead of running a second genesis.

**Tech Stack:** mold 2.30 (Ubuntu `apt`), clang as the link driver, Rust 1.99.1 stable, `crc32fast`, `bincode`, `std::sync::{OnceLock, LazyLock, Mutex}`.

**Spec:** none written. The request was "make plan to switch to mold, and fix repeated region genesis" (2026-10-31), building on a survey of the test suite. The findings this plan argues from are in *Background* below.

## Background (the findings)

- `rustc 1.99.1` already links x86_64 Linux with the bundled `rust-lld` (the default since 1.90). mold is compared against lld, not GNU ld, so its gain must be measured, not assumed. `mold` is not installed; `apt` has `2.30.0+dfsg-1build1`. `[profile.dev]` keeps full debug info (`debug = true`), which is what makes the links big.
- Region generation in tests calls `worldgen::generate_region` directly at 23 call sites. The dev disk cache (`crates/worldgen/src/cache.rs`, `WorldgenCache::generate_cached`) is only used by the server and the client's `LocalServer`. Its key (`cache_key_for`) covers seed, block manifest, dials, forged-world crc and `crates/worldgen/src`, but **not** `Materials` (tests use several sets: the manifest set, `TreeBlocks::NONE`, synthetic `BlockId(2..)`) and **not** `crates/game/src`, whose column/chunk code shapes the output.
- Repeats inside one binary: `trees_in_terrain.rs` calls `forest_columns()` 7 times, and `tree_fall.rs` calls `bare_forest()` twice. All of these are the same region `RegionCoords::from_world(-1536.0, 2560.0)`.
- `tree_fall.rs::young_trees_land_on_the_ground` runs whole-region sim genesis (`seed_terrain_genesis` + `seed_ground_genesis` on real terrain) 3 times per (species × seed 0..3 × age {5,15,30}), about 81 times in all. The first ("probe") scene only reads `base_y()` / `top_log()`, which come from the planted tree map, not from the region. The other two are the same tree on the same ground before their different cuts.
- `game`, `server` and `client` tests build one small hand-made world each (`common::world_of`, floor columns), or go through the already-cached `LocalServer`. They are not genesis-bound and are out of scope.

## Global Constraints

- Work on `develop` in the main checkout; **never create a git worktree** (CLAUDE.md).
- The working tree carries a lot of the user's own uncommitted work: **stage only the files a task names** (`git add <paths>`, never `-a` / `.`).
- Build and test through `just` (`just test-one <crate> "<--test target> …"`) so the `cap` pinning applies; never run whole suites routinely.
- Any new test slower than 100 ms gets `#[ignore = "slow: >100ms — run via just test-full"]`.
- Tests that verify generation itself keep calling `generate_region` directly: `scene_hash_golden`, `trees_in_terrain::window_independent_generation`, `forged_world` (its a/b determinism), `region_load_cost`, `client_region_load_cost`, and the `cache.rs` unit tests.
- Never edit the region edge as a literal; derive from `REGION_BLOCKS` / `REGION_CHUNKS`.
- Timing runs share the box with the user's own benchmarks: **ask before Task 1 and before each re-measure step**, and run them on a warm cache, mains power and an idle desktop (CLAUDE.md "Check the power source too").
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **Stale fixture after a `game` change.** A change under `crates/game/src` that alters generated columns must miss the fixture cache. Pinned by `fixture_key_changes_with_game_sources` (Task 3).
2. **Two test threads asking for the same region at once.** Only one generates; the other waits and gets identical columns. Pinned by `memo_generates_each_key_once_across_threads` (Task 4).
3. **Same region, different `Materials`.** It must not share a cache entry or a memo entry. Pinned by `fixture_key_changes_with_materials` (Task 3) and `memo_keys_by_materials` (Task 4).
4. **A forked tree scene drifts from a fresh one.** The second cut must behave exactly as before. Pinned by a byte-for-byte diff of the test's printed per-fall lines before and after the change (Task 5).
5. **mold missing, or `RUSTFLAGS` set in the environment.** On a machine without mold the build fails at link time; an exported `RUSTFLAGS` silently replaces the config's rustflags (no mold, plus a full rebuild). Covered by the verification and docs in Task 2.

---

### Task 1: Baseline measurements

No code. This records the "before" numbers every later task is judged against. **Ask the user first** (benchmark etiquette).

**Files:**
- Modify: this plan's *Measurements* section (bottom)

- [ ] **Step 1: Check the conditions.** Confirm mains power, not the ~88 W USB-C PD source (`cat /sys/class/power_supply/*/online`, `scaling_cur_freq` under load), and that the user's own benchmarks are not running.
- [ ] **Step 2: Warm the cache.** Run `just test-one game "--test simple --no-run"`, `just test-one client "--test harness_smoke --no-run"`, `just test-one worldgen "--test trees_in_terrain --test tree_fall --test underground --test player_walk --test lod_bricks --no-run"` and `just build`.
- [ ] **Step 3: Measure link-dominated rebuilds, 3 runs each, record the median.** Each starts with `touch` so only one small crate recompiles and the link dominates:
  - `touch crates/game/tests/simple.rs && /usr/bin/time -f %e just test-one game "--test simple --no-run"`
  - `touch crates/client/tests/harness_smoke.rs && /usr/bin/time -f %e just test-one client "--test harness_smoke --no-run"` (Bevy-sized link)
  - `touch crates/server/src/main.rs && /usr/bin/time -f %e just build`
- [ ] **Step 4: Measure the worldgen fixture tests, 3 runs, median.** Run `/usr/bin/time -f %e just test-one worldgen "--test trees_in_terrain --test tree_fall --test underground --test player_walk --test lod_bricks -- --include-ignored"`. Also time `tree_fall` on its own with `-- --include-ignored young_trees_land_on_the_ground`.
- [ ] **Step 5: Capture the tree_fall reference output** (used by Task 5): `just test-one worldgen "--test tree_fall -- --include-ignored --nocapture --test-threads=1 young_trees_land_on_the_ground" > target/test-speed/tree_fall.before.txt 2>&1` (create `target/test-speed/` first).
- [ ] **Step 6: Write the numbers into *Measurements*, with the adapter-free host conditions, and commit the plan file.**

```bash
git add docs/superpowers/plans/2026-10-31-test-speed-mold-and-fixture-regions.md
git commit -m "docs(plan): test-speed baseline measurements"
```

---

### Task 2: Link with mold (kept only if faster)

**Files:**
- Modify: `.cargo/config.toml` (new `[target.x86_64-unknown-linux-gnu]` section)
- Modify: `CLAUDE.md` ("Building and Running": one paragraph next to the toolchain sentence)

**Interfaces:**
- Consumes: Task 1's link medians.
- Produces: none for code; later tasks build with whatever linker this task leaves in place.

- [ ] **Step 1: The user installs mold.** This needs sudo, so it is one user action: `! sudo apt install mold`. Verify with `mold --version` (expect `mold 2.30.x`).
- [ ] **Step 2: Add the host-target linker config.** The `[target.wasm32-unknown-unknown]` section stays as it is (wasm is untouched). The comment must say why this lives in `config.toml` rather than the justfile: rustflags are part of every crate's fingerprint, so a justfile-only flag would rebuild Bevy each time you alternate with raw `cargo` or rust-analyzer.

```toml
[target.x86_64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-C", "link-arg=-fuse-ld=mold"]
```

- [ ] **Step 3: Rebuild and verify mold actually linked.** Run `just build`, then `readelf -p .comment target/debug/server | grep -i mold`. Expected: a line containing `mold 2.30`. If it shows only `LLD`, the rust-lld default won: add `"-C", "linker-features=-lld"` to the rustflags and re-check.
- [ ] **Step 4: Sanity.** `just smoke` passes. `just determinism-wasm` passes (it proves the wasm target is unaffected and native still matches).
- [ ] **Step 5: Re-measure Task 1 Step 3** under the same conditions (ask first) and record the medians next to the baseline.
- [ ] **Step 6: Keep or revert.** Keep mold if the Bevy-sized client test link is at least 15% faster; otherwise revert Step 2, record the numbers and the decision in *Measurements*, and skip Step 7's docs.
- [ ] **Step 7: Document in CLAUDE.md** (if kept). Cover four things: mold is a build prerequisite on the host target (`sudo apt install mold`); it is configured in `.cargo/config.toml` for one shared fingerprint; an exported `RUSTFLAGS` overrides it (no mold, and a full rebuild); wasm is unaffected.
- [ ] **Step 8: Commit.**

```bash
git add .cargo/config.toml CLAUDE.md docs/superpowers/plans/2026-10-31-test-speed-mold-and-fixture-regions.md
git commit -m "build: link the host target with mold"
```

---

### Task 3: Fixture disk cache keyed by materials and game sources

**Files:**
- Modify: `crates/worldgen/src/material.rs` (add `Materials::digest`)
- Modify: `crates/worldgen/src/cache.rs` (add `for_fixture`, `fixture_enabled`, `fixture_key_for`, plus unit tests in its `mod tests`)

**Interfaces:**
- Produces:
  - `impl Materials { pub fn digest(&self) -> u32 }`: crc32 over every `BlockId` in declaration order, including `rocks` and every `trees` entry. Write it with an **exhaustive destructure** (`let Materials { stone, dirt, grass, sand, water, bedrock, rocks, trees } = *self;`, no `..`) so a new field fails to compile until it is hashed. Do the same for `TreeBlocks`.
  - `impl WorldgenCache { pub fn for_fixture(seed: u64, materials: &Materials) -> Self }`: enabled iff `fixture_enabled(std::env::var("WORLDGEN_CACHE").ok().as_deref())`. Directory: `target/worldgen-cache/fixture-{key:08x}/`, created like `from_env` does. Disabled (`dir: None`) if the sources can't be read. Use it as `WorldgenCache::for_fixture(seed, &m).generate_cached(rc, seed, m)`; the existing `generate_cached` / `load` / `store` / atomic write stay as they are.
  - `fn fixture_enabled(var: Option<&str>) -> bool`: `None` → `!cfg!(target_arch = "wasm32")`; `"0" | "off" | "false"` → false; anything else → true. It is on in release too, unlike the production default, because a test fixture never measures generation.
  - `fn fixture_key_for(base: u32, materials: u32, game_src: u32) -> u32`: crc32 of the three as little-endian bytes. Here `base = cache_key_for(seed, "", REGION_CHUNKS, SPAWN_ISLAND_CELLS)` (the materials digest replaces the manifest text), and `game_src` = crc32 over every file under `CARGO_MANIFEST_DIR/../game/src`, sorted by path, using the existing `collect_files`.

- [ ] **Step 1: Write the failing unit tests** in `cache.rs`'s `mod tests`:

```rust
#[test]
fn fixture_key_changes_with_materials() {
    assert_ne!(fixture_key_for(1, 2, 3), fixture_key_for(1, 4, 3));
}
#[test]
fn fixture_key_changes_with_game_sources() {
    assert_ne!(fixture_key_for(1, 2, 3), fixture_key_for(1, 2, 5));
}
#[test]
fn materials_digest_tells_tree_sets_apart() {
    let m = /* any Materials with real trees, e.g. built from the manifest */;
    let bare = Materials { trees: TreeBlocks::NONE, ..m };
    assert_ne!(m.digest(), bare.digest());
    assert_eq!(m.digest(), m.digest());
}
#[test]
fn fixture_cache_env_switch() {
    assert!(fixture_enabled(None));
    for off in ["0", "off", "false"] { assert!(!fixture_enabled(Some(off))); }
    assert!(fixture_enabled(Some("1")));
}
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn fixture_hit_is_bit_identical_to_generation() {
    // at_dir(temp) cache; generate_cached twice for one rc; the second
    // (a hit) must equal generate_region's bincode bytes exactly.
}
```

- [ ] **Step 2: Run them and confirm they fail.** `just test-one worldgen "--lib cache::"`. Expected: compile errors (`fixture_key_for`, `fixture_enabled`, `digest` not defined).
- [ ] **Step 3: Implement `Materials::digest`, `fixture_enabled`, `fixture_key_for` and `WorldgenCache::for_fixture`** as specified in Interfaces.
- [ ] **Step 4: Run the tests and confirm they pass.** `just test-one worldgen "--lib cache:: -- --include-ignored"`. Expected: all pass.
- [ ] **Step 5: Commit.**

```bash
git add crates/worldgen/src/material.rs crates/worldgen/src/cache.rs
git commit -m "feat(worldgen): fixture region cache keyed by materials and game sources"
```

---

### Task 4: In-process fixture memo and moving the fixture call sites to it

**Files:**
- Create: `crates/worldgen/tests/support/fixture_regions.rs`
- Create: `crates/worldgen/tests/fixture_regions.rs` (memo tests)
- Modify: `crates/worldgen/tests/trees_in_terrain.rs` (`forest_columns`; NOT `window_independent_generation`)
- Modify: `crates/worldgen/tests/tree_fall.rs` (`bare_forest`)
- Modify: `crates/worldgen/tests/underground.rs` (the two `generate_region` calls; not the forged one)
- Modify: `crates/worldgen/tests/player_walk.rs`, `crates/worldgen/tests/lod_bricks.rs`
- Modify: `CLAUDE.md` (worldgen bullet: one sentence on the fixture cache and which tests must not use it)

**Interfaces:**
- Consumes: `WorldgenCache::for_fixture`, `Materials::digest` (Task 3).
- Produces, in `support/fixture_regions.rs`, included by each binary via `#[path = "support/fixture_regions.rs"] mod fixture_regions;` (same pattern as `support/lod_reference.rs`):
  - `pub type Columns = Vec<(ColumnCoords, Column)>;`
  - `pub fn fixture_region(rc: RegionCoords, seed: u64, materials: Materials) -> Columns`: calls `fixture_region_with` with the disk cache (`WorldgenCache::for_fixture(seed, &materials).generate_cached(rc, seed, materials)`) as the generator.
  - `pub fn fixture_region_with(rc: RegionCoords, seed: u64, materials: Materials, generate: impl FnOnce() -> Columns) -> Columns`: a process-wide `LazyLock<Mutex<HashMap<(i32, i32, u64, u32), Arc<OnceLock<Arc<Columns>>>>>>` keyed by `(rc.x, rc.z, seed, materials.digest())`. The mutex is held only to fetch or insert the slot's `Arc<OnceLock>`, never while generating, so different keys generate in parallel and the same key generates once. The function returns `(*slot.get_or_init(|| Arc::new(generate()))).clone()`.
  - The file starts with `#![allow(dead_code)]`, since each binary uses a subset.

- [ ] **Step 1: Write the failing memo tests** in `tests/fixture_regions.rs`. Use cheap stand-in generators that return a tiny `Columns` (e.g. `vec![]` or one floor column) and count their calls with an `AtomicUsize`:

```rust
#[test]
fn memo_generates_each_key_once_across_threads() {
    // 8 threads × fixture_region_with(same rc/seed/mats, counting generator
    // that sleeps 50 ms); afterwards count == 1 and every result is equal.
}
#[test]
fn memo_keys_by_materials() {
    // same rc + seed, materials m and Materials { trees: TreeBlocks::NONE, ..m }:
    // count == 2.
}
```

  Each test uses a region coordinate the other doesn't (e.g. `(9001, 0)` and `(9002, 0)`), so the shared process-wide memo can't couple them.
- [ ] **Step 2: Run them and confirm they fail.** `just test-one worldgen "--test fixture_regions"`. Expected: compile error (module not found).
- [ ] **Step 3: Implement `support/fixture_regions.rs`** as specified.
- [ ] **Step 4: Run them and confirm they pass.** `just test-one worldgen "--test fixture_regions"`.
- [ ] **Step 5: Switch the fixture call sites.** Replace `worldgen::generate_region(rc, seed, m)` with `fixture_region(rc, seed, m)` in `forest_columns`, `bare_forest`, the two non-forged `underground.rs` calls, `player_walk.rs` and `lod_bricks.rs`. Leave every generation-verifying test listed in Global Constraints alone.
- [ ] **Step 6: Run the switched binaries twice, cold then warm.** `just worldgen-cache-clear`, then twice: `just test-one worldgen "--test trees_in_terrain --test tree_fall --test underground --test player_walk --test lod_bricks -- --include-ignored"`. Expected: both runs pass, and `target/worldgen-cache/fixture-*/` holds one `.bin` per distinct (region, materials) pair.
- [ ] **Step 7: Add the CLAUDE.md sentence.** Fixture regions come from `tests/support/fixture_regions.rs` (memo plus `WorldgenCache::for_fixture`, keyed additionally by `Materials` and `crates/game/src`; `WORLDGEN_CACHE=0` bypasses it). Tests that check generation itself call `generate_region` directly.
- [ ] **Step 8: Commit.**

```bash
git add crates/worldgen/tests/support/fixture_regions.rs crates/worldgen/tests/fixture_regions.rs \
  crates/worldgen/tests/trees_in_terrain.rs crates/worldgen/tests/tree_fall.rs \
  crates/worldgen/tests/underground.rs crates/worldgen/tests/player_walk.rs \
  crates/worldgen/tests/lod_bricks.rs CLAUDE.md
git commit -m "test(worldgen): generate each fixture region once per binary and per source state"
```

---

### Task 5: tree_fall without repeated sim genesis

**Files:**
- Modify: `crates/worldgen/tests/tree_fall.rs` (`scene`, `Scene`, `young_trees_land_on_the_ground`; `clone_ground` goes away if unused)

**Interfaces:**
- Consumes: `game::SerializedRegion::{from_rollback, to_rollback}` (`crates/game/src/region_runner.rs`), `Region::new(rollback, None, rc, None)`, and the `bare_forest()` from Task 4.
- Produces, all local to the file:
  - `fn plant(ground: &Ground, sp: &Species, seed: u64, age: u16) -> (BTreeMap<ChunkCoords, Chunk>, BTreeMap<TilePos, TreeBlock>)`: the block-writing half of today's `scene`, with no region. It clones only the chunks it writes into, starting from `ground.chunks`.
  - `fn base_y(tree: &BTreeMap<TilePos, TreeBlock>) -> i32` and `fn top_log(tree: &…) -> i32`: today's `Scene` method bodies. The `Scene` methods delegate to them.
  - `fn scene(ground: &Ground, sp, seed, age) -> Scene`: `plant`, then genesis exactly as today, **with `Rollback::new(None)`**. There is no client sender: the receiver was `mem::forget`-ed and never read, and a restored region carries none either, so every scene is built one way.
  - `impl Scene { fn fork(&self) -> Scene }`: `SerializedRegion::from_rollback(self.region.data())` → `to_rollback()` → `Region::new(rollback, None, self.ground.rc, None)`, with `ground` and `tree` cloned. Call it only on a pristine scene, before any chop.

- [ ] **Step 1: Check that the fork pays.** Add a temporary `#[ignore]`d test that builds one `scene(&bare_forest(), oak, 0, 30)` and prints the time of a second `scene(...)` versus a `fork()`. Run it with `--nocapture` (ask first: it's a timing). If the fork is not at least 2× cheaper than genesis, skip the fork: do Step 3 without `fork` and build both cuts with `scene`. The probe removal alone still saves a third. Delete the temporary test either way.
- [ ] **Step 2: Restructure.** In `young_trees_land_on_the_ground`, compute `(base, top)` from `plant(...)`'s tree map (no region). Build one `scene` per tree for the base cut, and take `fork()` from it **before** its chop for the mid cut. Labels, printouts, assertions and loop order stay exactly as they are. `mature_trees_topple_about_their_base` only changes to `scene(&flat_floor(4), …)`.
- [ ] **Step 3: Diff against the reference.** Run the Task 1 Step 5 command again, writing `target/test-speed/tree_fall.after.txt`, then `diff <(grep ': settled after\|: no body' target/test-speed/tree_fall.before.txt) <(grep ': settled after\|: no body' target/test-speed/tree_fall.after.txt)`. Expected: **no output**. Every fall settles after the same ticks, at the same depths and speeds. Any difference means the fork or the sender change altered the simulation; stop and investigate, don't re-baseline.
- [ ] **Step 4: Run the whole binary.** `just test-one worldgen "--test tree_fall -- --include-ignored"`. Expected: PASS.
- [ ] **Step 5: Re-measure Task 1 Step 4** (ask first) and record the numbers in *Measurements*.
- [ ] **Step 6: Commit.**

```bash
git add crates/worldgen/tests/tree_fall.rs docs/superpowers/plans/2026-10-31-test-speed-mold-and-fixture-regions.md
git commit -m "test(worldgen): tree_fall forks pristine scenes instead of re-running genesis"
```

---

## Out of scope (next levers, not in this plan)

- Merging the 48 `game` integration test binaries into a few (fewer links: probably the largest remaining build-time win).
- Leaner test debug info (`[profile.test] debug = "line-tables-only"` or `split-debuginfo = "unpacked"`). Trade-off: a separate dependency build for tests and less useful test backtraces.
- `cargo-nextest`.

## Measurements

| What | Baseline (Task 1) | After mold (Task 2) | After fixtures (Tasks 4–5) |
|---|---|---|---|
| host conditions (power, freq) | | | |
| `game --test simple` relink | | | — |
| `client --test harness_smoke` relink | | | — |
| `just build` after touching server | | | — |
| worldgen fixture binaries, cold cache | | — | |
| worldgen fixture binaries, warm cache | | — | |
| `tree_fall young_trees_land_on_the_ground` | | — | |
| genesis vs fork, one oak scene (Task 5 Step 1) | — | — | |
| mold kept? | | | |
