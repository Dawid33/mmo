# Prefabs 4 — town generator (first version) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A town laid out from the imported house prefabs — a plaza, straight streets out of it, houses on lots along both sides turned to face the street, each on its own levelled pad, roads painted onto the ground — standing in the home region of the debug world, and viewable as a picture without launching the game.

**Architecture:** Three new pieces in `worldgen`: a **paint pass** in the generator (a source can replace the top ground block over rectangles: roads, a plaza); `sites::kit`, which turns a prefab set and a style tag into houses with a known front and frontage; `sites::town`, a pure layout function producing placements and paints. A home-anchored dev source feeds the layout to the structure pass; `prefab-tool town` draws it.

**Tech Stack:** Rust; `game::prefab`, `worldgen::structures` (plans 1–3); `tools/prefab-tool` for the preview.

**Spec:** `docs/superpowers/plans/2026-11-14-prefabs-minecraft-import-site-generation.md` — §7 (the lots technique), §13 (constraints stage C1 found). This plan is the roadmap's stages E (kit, by tags instead of hand-made cut lists) and G (the town generator), against a dev source; the frozen site table (stage F) waits for R4.

**Out of scope:** walls, gates and towers; fortresses; curved or terrain-following street layouts (streets are straight and axis-aligned here); graded roads (a road follows the ground as it is); a shared terrace for a row of lots; landmarks too big for a lot; fixtures (notice board, inn); the site table and history.

## Global Constraints

- Work directly on `develop` in this checkout. **No git worktrees.** Other sessions (the R4 takeover work) commit to this branch and the user has uncommitted edits.
- Commit with explicit paths only: `git add <paths>` then `git commit -m "<msg>" -- <paths>`. Never `git add -A`, `git add -u`, `git commit -a`, a bare `git commit`, or `--amend`. End messages with the session's attribution trailer.
- Before the first edit to any file that already exists, run `git status --short -- <file>`; if it is modified, make the edit but do NOT stage or commit that file, and list the exact lines added under "left uncommitted". `justfile`, `crates/client/src/debug_launch.rs`, `crates/client/src/renderer/mod.rs`, `crates/game/src/lib.rs` are such files.
- **No structure source installed ⇒ byte-identical generation.** `structure_pass::no_structure_output_is_pinned`, `trees_in_terrain::window_independent_generation` and `scene_hash_golden` stay green with their constants untouched; run them after every change to `crates/worldgen/src/lib.rs`.
- Generation stays a pure function of world coordinates and the source: any window gives the same columns, with paints and placements present. Layout functions are pure functions of their arguments.
- Integer math only in everything that decides blocks, heights or positions (`+ - * /`, `div_euclid`, `rem_euclid`); `BTreeMap`/`BTreeSet`, never hash collections; no `f32`/`f64` in the new worldgen code. Randomness only through a stateless hash of `(seed, …)`.
- Never write a region or chunk edge as a literal (`REGION_BLOCKS`, `CHUNK_BLOCKS`).
- Tests in the shared `it` test binaries never install process-global state (`structures::install`, `forged::install`, `install_prefabs`): use explicit sources and the `_with` functions. No test depends on `prefab-imports/` or `mc-sources/` (bought content, git-ignored): kits in tests are built from prefabs made in memory.
- Builds and tests go through `just`: `just test-one <crate> "<args>"`; several filters need `--` first. No whole suites, no client tests, do not launch the game. A test slower than 100 ms is tagged `#[ignore = "slow: >100ms — run via just test-full"]` and run explicitly with `-- --include-ignored`.
- Conventions: +X east, +Z south, north is −Z; one quarter turn clockwise from above maps (x, z) → (−z, x) (`game::prefab::turn_xz`); `Facing::quarter_turns()` is 0 north, 1 east, 2 south, 3 west.

## Review Focus

- A street paint and a house pad on the same column → the pad's box wins the ground level, the paint never recolours a block under a building, and no tree stands on a road (Task 1).
- A paint rectangle straddling a chunk edge, a region edge, and a far tile's edge → the same blocks whichever window or tile generated them (Task 1).
- A house whose door is on its east side, on a lot north of an east–west street → turned so the door faces the street (south), with its box inside the lot (Tasks 2, 3).
- Lots on both sides of a street and around the plaza corners → no two building boxes overlap or touch, and none overlaps a street or the plaza (Task 3).
- A site half under water or on a cliff → the lots there are skipped, the rest are laid out, and the layout is the same on every run (Task 3).

---

### Task 1: The paint pass

**Files:**
- Modify: `crates/worldgen/src/structures.rs`, `crates/worldgen/src/lib.rs`
- Create: `crates/worldgen/tests/structure_paint.rs`; Modify: `crates/worldgen/tests/it.rs`

**Interfaces:**
- Produces, in `worldgen::structures`:

```rust
/// Replace the top ground block of every column in a rectangle (a road, a plaza). The ground keeps its
/// height and its slope shape; only the block changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Paint { pub rect: [i32; 4] /* x0, z0, x1, z1 — world blocks, max exclusive */, pub block: BlockId }

pub trait StructureSource {
    // … existing …
    /// Paints whose rectangle grown by `margin` intersects the half-open query rectangle, in the
    /// source's own fixed order. Default: none.
    fn paints_in(&self, _x0: i32, _z0: i32, _x1: i32, _z1: i32, _margin: i32) -> Vec<Paint> { Vec::new() }
}

/// The paint for column (x, z): the FIRST paint in slice order whose rectangle contains it.
pub fn paint_at(paints: &[Paint], x: i32, z: i32) -> Option<BlockId>;

impl StructureList {
    /// A list that also carries paints; `new` is `with_paints(placements, Vec::new(), set_hash)`.
    pub fn with_paints(placements: Vec<StructurePlacement>, paints: Vec<Paint>, set_hash: u32) -> Self;
}
```

`StructureList` indexes paints in the same x/z buckets and hashes them into `content_hash` (rect and block, in order) after the placements — a list without paints hashes exactly as before.

The pass in `columns_from_patch`, with `paints = structures.paints_in(window, margin = the sample ring's margin)`; everything below is skipped when `paints` is empty and no placement is near:
1. **Trees**: a sample column with a paint gets `trees = false` (alongside the pad step, over the whole ring).
2. **Top block**: in the terrain fill, for a column with a paint that is NOT inside a placement's padded box (`pad_at(..)` not `in_box`) and is dry (`water_top <= top`) and carries no ice: the block written at `by == c.top` is the paint's block; a slope chisel on that block is built from the paint's block id. Nothing else about the column changes — `ColumnGround` is untouched.
3. A structure write still overrides a painted block.

The twin in `coarse_from_patch`: for a cell column whose centre block column has a paint under the same conditions, the material of the cell containing the terrain top is the paint's block; `trees = false` in the tree-surface sample.

- [ ] **Step 1: Failing tests** `tests/structure_paint.rs` (procedural terrain, real block names through the support module, sites chosen by scanning; slow ones ignored and run explicitly):
  - `no_paint_is_byte_identical`: a `StructureList::with_paints(vec![], vec![], h)` and a list with one far-away paint both give `generate_region_with` output equal (bincode) to `NoStructures` for a region that neither touches.
  - `a_road_recolours_the_top_block_only`: a 5-wide, 40-long paint of `cobbled_road` over dry tree-free ground: every column in the rectangle has `cobbled_road` at its top terrain block, the block below and the column's height are what the no-structure generation has, columns outside are untouched.
  - `a_road_keeps_slopes`: over a sloped column in the paint, the top block is `cobbled_road` AND carries the same chisel shape as without the paint.
  - `no_trees_on_a_road`: a paint through forest: no tree trunk stands on a painted column (a crown may overhang).
  - `a_building_beats_a_road`: a paint crossing a padded placement: inside the placement's x/z box the top ground block is what the pad alone gives (not the paint's block); in the skirt the paint's block is used.
  - `window_and_region_independence`: a paint straddling a region corner: `generate_columns_with` over half windows and a single chunk column equals `generate_region_with`, per column, in all four regions.
  - `water_and_ice_are_left_alone`: a paint over a lake column changes nothing there.
  - `the_coarse_tile_shows_the_road`: at factors 16 and 64, the cell holding the terrain top of each painted centre column is `cobbled_road`; no canopy cell over the road.
  - `first_paint_wins_and_hash_tracks_paints`: two overlapping paints → the first's block; `content_hash` differs between lists with and without a paint, and `new(p, h).content_hash() == with_paints(p, vec![], h).content_hash()`.
- [ ] **Step 2: Run** `just test-one worldgen "--test it -- --include-ignored structure_paint::"` — FAIL.
- [ ] **Step 3: Implement.** Thread the paints beside `near` through the full pass and the coarse pass; `NoStructures` and the dev sources return none.
- [ ] **Step 4: Run** — PASS; then the guards: `just test-one worldgen "--test it -- --include-ignored structure_pass:: structure_coarse:: trees_in_terrain::window_independent_generation scene_hash_golden"`.
- [ ] **Step 5: Commit**: `feat(worldgen): the paint pass — a source can recolour the top ground block over rectangles (roads, plazas)`.

---

### Task 2: A town kit from a prefab set

**Files:**
- Modify: `crates/game/src/prefab/builtin.rs` (`PrefabSet::iter`), `crates/game/tests/prefab_builtin.rs`
- Create: `crates/worldgen/src/sites/mod.rs`, `crates/worldgen/src/sites/kit.rs`, `crates/worldgen/tests/sites_kit.rs`; Modify: `crates/worldgen/src/lib.rs` (`pub mod sites;`), `crates/worldgen/tests/it.rs`

**Interfaces:**
- Produces:

```rust
// game::prefab::PrefabSet
/// Every prefab, ascending by name.
pub fn iter(&self) -> impl Iterator<Item = &Arc<ResolvedPrefab>>;

// worldgen::sites::kit
pub const KIT_MIN_SIDE: i32 = 5;
pub const KIT_MAX_SIDE: i32 = 48;
pub const KIT_CENTRE_MAX_SIDE: i32 = 12;
/// A building a lot can hold, described for turn 0.
#[derive(Debug, Clone)]
pub struct KitHouse {
    pub prefab: Arc<ResolvedPrefab>,
    pub front: Facing,     // the side of the prefab's box its entrance is on
    pub frontage: i32,     // the box's extent ALONG that side (x extent for a north/south front, z for east/west)
    pub depth: i32,        // the extent across it
}
#[derive(Debug, Clone)]
pub struct TownKit { pub style: String, pub houses: Vec<KitHouse>, pub centrepieces: Vec<Arc<ResolvedPrefab>> }
/// The side of the box the entrance is on: among the prefab's `door` markers take those on the lowest
/// marker y; of these the one nearest to a side of the box (distance to the nearest of the four sides,
/// ties broken north, east, south, west, then by marker order) decides, and that nearest side is the
/// front. No door marker → `Facing::South`.
pub fn front_of(prefab: &ResolvedPrefab) -> Facing;
/// Houses: prefabs tagged both `style` and `house` whose x and z extents are within
/// `KIT_MIN_SIDE..=KIT_MAX_SIDE`. Centrepieces: prefabs tagged `statue` or `decor` (any style) whose
/// extents are at most `KIT_CENTRE_MAX_SIDE`. Both ascending by name. Fewer than 3 houses → Err naming
/// the style and how many were found.
pub fn town_kit(set: &PrefabSet, style: &str) -> Result<TownKit, String>;
/// The quarter turns that make a house's front face `toward`.
pub fn turn_to_face(front: Facing, toward: Facing) -> u8;   // (toward − front) mod 4
```

- [ ] **Step 1: Failing tests.** `prefab_builtin.rs`: `iter_is_ascending_by_name`. `tests/sites_kit.rs` (prefabs resolved in memory, with tags and door markers):
  - `front_follows_the_door`: a 9 × 5 × 7 box with a door marker on its east face at ground level → `East`; two doors (one upstairs on the north balcony at y 4, one at y 1 on the south face) → `South`; a door one cell inside the west wall → `West`; no door → `South`.
  - `frontage_is_along_the_front`: the 9 × 5 × 7 box with an east front → frontage 7, depth 9; with a south front → frontage 9, depth 7.
  - `the_kit_filters_by_tags_and_size`: a set with medieval houses of sides 6, 20, 48, 49 and 4, a desert house, a medieval statue of side 10 and a decor of side 30 → houses are the three in range (by name), centrepieces only the statue.
  - `too_few_houses_is_an_error`.
  - `turn_to_face`: all 16 combinations against `Facing::turned`: `front.turned(turn_to_face(front, toward)) == toward`.
- [ ] **Step 2: Run** `just test-one game "--test it prefab_builtin::"` and `just test-one worldgen "--test it sites_kit::"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — PASS.
- [ ] **Step 5: Commit**: `feat(worldgen): a town kit from a prefab set — houses by style tag, each with a front and a frontage`.

---

### Task 3: The town layout

**Files:**
- Create: `crates/worldgen/src/sites/town.rs`, `crates/worldgen/tests/sites_town.rs`; Modify: `crates/worldgen/src/sites/mod.rs`, `crates/worldgen/tests/it.rs`

**Interfaces:**
- Consumes: `TownKit`, `KitHouse`, `turn_to_face` (Task 2); `structures::{StructurePlacement, Pad, Paint}`; `game::prefab::{Placement, placement_bounds}`.
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TownParams {
    pub seed: u64,
    pub arms: u8,          // 1..=4 streets out of the plaza, in the order east, west, south, north
    pub arm_len: i32,      // blocks from the plaza's edge
    pub street_w: i32,     // odd
    pub plaza: i32,        // side of the square, odd
    pub setback: i32,      // blocks between the street's edge and a house's front
    pub gap_min: i32, pub gap_max: i32,   // blocks between neighbouring houses along a street
    pub max_relief: i32,   // a lot whose ground varies by more than this is skipped
    pub pad_skirt: u32,
    pub road: BlockId, pub plaza_block: BlockId,
}
impl TownParams { pub fn small(seed: u64, road: BlockId, plaza_block: BlockId) -> Self; }
// small: arms 4, arm_len 96, street_w 5, plaza 15, setback 1, gaps 2..=5, max_relief 6, pad_skirt 3

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lot { pub name: String, pub bounds: ([i32; 3], [i32; 3]), pub faces: Facing, pub arm: u8, pub left: bool }
#[derive(Debug, Clone)]
pub struct TownLayout {
    pub placements: Vec<StructurePlacement>,   // centrepiece first (if any), then lots in the order laid
    pub paints: Vec<Paint>,                    // the plaza, then each arm's street
    pub lots: Vec<Lot>,                        // one per house placement, same order
    pub skipped: u32,                          // lot positions rejected for relief, water or overlap
}
/// A pure function of its arguments. `ground(x, z)` is the natural ground height, `wet(x, z)` whether
/// the column is under water.
pub fn layout_town(kit: &TownKit, origin: [i32; 2], p: &TownParams,
                   ground: &dyn Fn(i32, i32) -> i32, wet: &dyn Fn(i32, i32) -> bool) -> TownLayout;
/// The stateless roll used for every choice: a 64-bit mix of the seed and three small integers.
pub fn roll(seed: u64, a: u32, b: u32, c: u32) -> u64;
```

The layout, exactly:
1. **Plaza**: the square of side `plaza` centred on `origin`; one paint with `plaza_block`. If the kit has centrepieces: the one at index `roll(seed, 0, 0, 0) % len`, turn 0, its box centred on `origin` (x/z), with `Pad { level: ground(origin), skirt: 0 }` and its anchor at `level + 1`.
2. **Arms**, for `i in 0..arms` in the order east (+x), west (−x), south (+z), north (−z): a street of width `street_w` centred on the axis through `origin`, running from the plaza's edge outward `arm_len` blocks. Its paint (with `road`) is added AFTER its lots are laid and runs from the plaza's edge to the far edge of the last house laid on either side (no houses → no paint for that arm).
3. **Lots** on each arm, the left side first then the right (left/right looking outward from the plaza: for the east arm, left is north). A cursor `t` runs along the arm from `plaza / 2 + 1 + gap_min` (measured from `origin` along the arm). At each step `n` (counting positions tried on that side): the house is `kit.houses[roll(seed, arm·2 + side + 1, n, 0) % len]`, turned to face the street (`turn_to_face(front, the direction from the lot toward the street)`); its box is positioned so that its front face is `setback` blocks from the street's edge and its near edge along the arm is at `t`. If the box's far edge along the arm exceeds `plaza / 2 + arm_len`: stop this side. **Reject** the position (count it in `skipped`, advance `t` by 4, continue) if any of the box's four corner columns or its centre column is `wet`, or the largest minus the smallest `ground` over those five columns exceeds `max_relief`, or the box grown by 1 block in x/z overlaps any box already placed, the plaza square, or any arm's street strip (the full `arm_len` strip of every arm, laid or not). Otherwise place it: `Pad { level: ground at the middle column of its front face, skirt: pad_skirt }`, anchor y so the prefab's lowest layer is at `level + 1`; advance `t` to the box's far edge plus `gap_min + roll(seed, arm·2 + side + 1, n, 1) % (gap_max − gap_min + 1)`.
4. `mirror` is always false. Every placement's `at` is found by building a trial placement at anchor `[0, 0, 0]` with the turn, taking `placement_bounds`, and translating.

- [ ] **Step 1: Failing tests** `tests/sites_town.rs` (a synthetic kit of 6 houses of different footprints and fronts built in memory; flat `ground = |_, _| 70`, `wet = |_, _| false` unless stated; block ids from a small table):
  - `deterministic_and_seed_sensitive`: two calls with equal arguments give equal `lots`, `paints` and placement `at`s; another seed gives a different `lots` list.
  - `every_house_faces_its_street`: for each lot, the house's front side (kit front turned by the placement's turn) equals `lot.faces`, and `faces` points from the lot toward its arm's axis (east arm, left side → south, right side → north, and so on for all four arms).
  - `fronts_stand_at_the_setback`: the distance from each box's front face to its street's edge is exactly `setback`.
  - `nothing_overlaps`: no two boxes grown by 1 intersect; no box intersects the plaza or any street strip.
  - `lots_stay_within_the_arm`: every box's extent along its arm is within `plaza/2 + 1 ..= plaza/2 + arm_len`.
  - `houses_sit_on_their_pads`: each placement has a pad with `level == 70`, skirt `pad_skirt`, and its box's min y is 71.
  - `paints_cover_plaza_and_streets`: the first paint is the plaza square with `plaza_block`; each laid arm has one paint of width `street_w` starting at the plaza's edge and ending at its last house's far edge; an arm with no room (arm_len 4) has none.
  - `wet_and_steep_lots_are_skipped`: with `wet` true for x > origin.x + 40, no box on the east arm reaches past that line and `skipped > 0`; with a ground that rises 1 block per block along +z beyond origin.z + 30 and `max_relief` 3, the south arm has fewer lots than on flat ground; in both cases the other arms equal the flat layout.
  - `a_town_has_a_sensible_number_of_houses`: `small` params with the 6-house kit on flat ground lay between 12 and 60 lots.
  - `the_centrepiece_is_on_the_plaza`: with a centrepiece in the kit, placement 0 is centred on the origin and no lot overlaps it.
  - `roll_is_stable`: `roll(1, 2, 3, 4)` equals a pinned constant (recorded from the implementation) and differs when any argument changes.
- [ ] **Step 2: Run** `just test-one worldgen "--test it sites_town::"` — FAIL.
- [ ] **Step 3: Implement.** A splitmix64-style integer mix for `roll`.
- [ ] **Step 4: Run** — PASS.
- [ ] **Step 5: Commit**: `feat(worldgen): the first town layout — a plaza, straight streets, houses on lots facing the street`.

---

### Task 4: A town in the home region

**Files:**
- Modify: `crates/worldgen/src/structures.rs` (a home-anchored town source), `crates/worldgen/src/lib.rs` (`pub fn is_wet`), `crates/server/src/dev_structures.rs`, `crates/server/tests/dev_structures.rs`
- Create: `crates/worldgen/tests/sites_source.rs`; Modify: `crates/worldgen/tests/it.rs`
- Modify (left uncommitted): `crates/client/src/debug_launch.rs` (`--town <style>`), `justfile` (a `town` recipe)
- Modify: the roadmap's §8 note

**Interfaces:**
- Consumes: `town_kit`, `layout_town`, `TownParams::small` (Tasks 2–3); `DevHomeSource`'s pattern (`StructureSource::region_requested`, called by the server when a region starts running); `ground_y`.
- Produces:

```rust
// worldgen
/// Whether world column (x, z) is under water in natural terrain (the same sample generation uses).
pub fn is_wet(x: i32, z: i32, seed: u64, mats: Materials) -> bool;
// worldgen::structures
/// A town laid out once, beside the origin of the first region requested, and nowhere else.
pub struct DevTownSource { /* kit, params, origin_of, ground, wet, set_hash, OnceLock<(RegionCoords, TownLayout indexed)> */ }
impl DevTownSource {
    pub fn new(kit: TownKit, params: TownParams, set_hash: u32,
               origin_of: Box<dyn Fn(game::RegionCoords) -> [i32; 2] + Send + Sync>,
               ground: Box<dyn Fn(i32, i32) -> i32 + Send + Sync>,
               wet: Box<dyn Fn(i32, i32) -> bool + Send + Sync>) -> Self;
    pub fn home(&self) -> Option<game::RegionCoords>;
    pub fn layout(&self) -> Option<&TownLayout>;
}
impl StructureSource for DevTownSource { /* placements_in, paints_in, content_hash, region_requested */ }
```

- `content_hash`: crc32 of `set_hash`, a tag byte of its own, the style, and every field of the params.
- Server: `VOXEL_DEV_STRUCTURES=town=<style>[:<arms>[:<arm_len>]]` → `DevTownSource` with `TownParams::small(worldgen::WORLD_SEED, id("cobbled_road"), id("smooth_stone"))` overridden by the two optional numbers (arms 1..=4, arm_len 16..=256, else an error), the kit from the installed prefab set (`town_kit` — its error is a startup error), origin = the region's landing column (else its centre). A new `DevStructures::Town { style }` variant and a log line; the worldgen disk cache is off, as for the other dev sources.
- Client: `client debug embarked --town <style>[:arms[:len]]` sets `VOXEL_DEV_STRUCTURES=town=<…>` on the spawned server; refused together with `--prefab`, `--join`, and for `online`; `USAGE` updated. `justfile`: `town style *args` — `VOXEL_PREFAB_DIRS` set to every sub-directory of `prefab-imports/` joined with `:`, then the `run` recipe's commands with `--reset --town {{style}} {{args}}`; a comment above it.

- [ ] **Step 1: Failing tests.** `tests/sites_source.rs` (in-memory kit, flat closures, no global install; `region_requested` called on the source directly): before a home `placements_in`/`paints_in` are empty; after `region_requested(A)` they return exactly the layout's placements and paints filtered by the query (compare with `layout()`), a far query is empty, a second request changes nothing; `generate_region_with(.., &source)` for A's region contains the plaza block at the origin column's top and the first lot's blocks; the hash changes with the style and with `arms`. `is_wet`: equals the generated column's recipe-water presence on 100 scanned columns of a region with a lake or sea (`ignored` if slow). Server test: the pure step resolves `town=medieval` to an error naming the style when the built-in set has no such houses (it has only `dev-hut`), and rejects `town=`, `town=x:9`, `town=x:2:5`.
- [ ] **Step 2: Run** `just test-one worldgen "--test it -- --include-ignored sites_source::"` and `just test-one server "--test dev_structures"` — FAIL.
- [ ] **Step 3: Implement**, the client flag and the justfile recipe included (uncommitted), and the roadmap note.
- [ ] **Step 4: Run** — PASS; guards (`structure_pass::no_structure_output_is_pinned`); `just smoke_timeout=900 smoke`.
- [ ] **Step 5: Commit** what is committable: `feat(worldgen,server): a dev town in the home region (VOXEL_DEV_STRUCTURES=town=<style>)`.

---

### Task 5: `prefab-tool town` — a picture of a layout

**Files:**
- Modify: `tools/prefab-tool/Cargo.toml` (dependency `worldgen`), `tools/prefab-tool/src/main.rs`, `tools/prefab-tool/src/render.rs`; Create: `tools/prefab-tool/src/town.rs`; Modify: `tools/prefab-tool/tests/cli.rs`
- Modify (left uncommitted): `justfile` — recipe `town-preview *args`

**Interfaces:**
- Produces: `prefab-tool town <style> [--dirs <a:b:…>] [--seed N] [--arms N] [--len N] [--terrain flat|seed] [-o <png>] [--scale N]`:
  - Loads the built-in set plus `--dirs` (default: every sub-directory of `prefab-imports/` that exists; none → the built-in set alone), builds the kit, lays the town out at origin `(0, 0)` with `TownParams::small` (overridden by the flags) on `--terrain flat` (ground 70, nothing wet — the default) or `--terrain seed` (procedural terrain from `--seed` via `worldgen::ground_y` / `is_wet`, around the first dry column found scanning outward from the origin).
  - Writes one top-down PNG (default `target/prefab-view/town-<style>-<seed>.png`): ground shaded by height (grey-green, lighter when higher; blue where wet), paints in their block's colour, each house drawn by its own plan (the colour of each column's top block, as `prefab view` does, turned as placed), a 1-px dark outline around every building box, a small magenta mark at the middle of each front face. Default scale: the largest integer keeping the image within 2048 px, at least 1.
  - Prints: style, houses in the kit, lots laid and skipped, the layout's extent, and one line per lot (`name  arm  side  WxD  level`). Exit 1 with the kit's error when the style has too few houses; 2 on usage.

- [ ] **Step 1: Failing tests** (`tests/cli.rs`; a temp directory holding six small hand-written `.prefab.ron` houses tagged `test-style` + `house` with door markers, passed via `--dirs`): `town_writes_a_png` (exit 0; PNG exists with the expected width for the layout's extent at `--scale 2`; stdout names the style and a positive lot count; at least 20 distinct colours in the image); `town_is_deterministic` (two runs with the same seed give byte-identical PNGs; another seed differs); `an_unknown_style_fails` (exit 1, names the style); `bad_flags` (exit 2).
- [ ] **Step 2: Run** `just test-one prefab-tool "--test cli"` — FAIL.
- [ ] **Step 3: Implement**; add the recipe (uncommitted).
- [ ] **Step 4: Run** — PASS. Then run it on the real library and LOOK: `just town-preview medieval --seed 1`, `--seed 2`, `just town-preview desert --seed 1`, and one with `--terrain seed` — read each PNG and describe what you see in the report (do the streets read as streets, do houses line them with their fronts to the road, are there gaps or pile-ups, anything absurd).
- [ ] **Step 5: Commit** `tools/prefab-tool/**`, `Cargo.lock`: `feat(tools): prefab-tool town — a top-down picture of a town layout`.
