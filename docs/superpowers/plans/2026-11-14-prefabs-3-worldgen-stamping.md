# Prefabs 3 — worldgen stamping — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** World generation stamps prefabs into terrain — levelled ground under them, whole across chunk and region edges, visible in the far rings — and a debug launch puts a chosen prefab beside the landing spot. With nothing installed, generation is byte-identical to today.

**Architecture:** A new module `worldgen::structures` holds placements (a resolved prefab + where + an optional ground pad) behind a `StructureSource` trait with a spatial index. `columns_from_patch` gains a structure pass (pad the sampled ground, keep trees off, grow the column band, write blocks after trees) and `coarse_from_patch` its far-terrain twin. Both core functions take the source as a parameter; only the public entry points read a process global, installed by the server from an environment variable.

**Tech Stack:** Rust; `game::prefab` (plans 1–2); crates `worldgen`, `server`, `client` (one flag in `debug_launch.rs`).

**Spec:** `docs/superpowers/plans/2026-11-14-prefabs-minecraft-import-site-generation.md` (the roadmap; this plan is its stage C1 — §4.3 and "Order around the R4 takeover work"; fixtures, containers and `RegionMeta` are C2 and NOT in this plan).

**Out of scope:** fixtures and containers at genesis; `GroundFit::Follow` / `Buried`; foundations for a placement without a pad; site plans and generators; runtime stamping; structures in water (a placement whose pad is below the water top is simply flooded).

## Global Constraints

- Work directly on `develop` in this checkout. **No git worktrees.** Other sessions (the R4 takeover work) commit to this branch and the user has uncommitted edits.
- Commit with explicit paths only: `git add <paths>` then `git commit -m "<msg>" -- <paths>`. Never `git add -A`, `git add -u`, `git commit -a`, a bare `git commit`, or `--amend`.
- Before the first edit to any file that already exists, run `git status --short -- <file>`; if it is modified, make the edit but do NOT stage or commit that file, and list it under "left uncommitted" with the exact lines added. `justfile`, `crates/server/src/lib.rs`, `crates/client/src/debug_launch.rs` and `crates/game/src/lib.rs` are likely to be such files.
- **No structure source installed ⇒ byte-identical output.** `generate_region`, `generate_columns`, `generate_region_forged`, `coarse_tile`, `coarse_water` and the worldgen cache key must give exactly what they give at the plan's base commit when no source is installed. Every task that touches generation proves it with a bincode-equality test against the unchanged entry point.
- Generation stays a pure function: a structure's effect on a column depends only on that column's world coordinates and the source — never on the window (`xs`/`zs`), the patch extent, or iteration order. `generate_columns` over any split of a region equals `generate_region`, with structures present.
- Integer math only in everything that decides blocks or heights (`+ - * /`, `div_euclid`, `rem_euclid`); `BTreeMap`/`BTreeSet`, never hash collections; no `f32`/`f64` in the new code paths.
- Never write a region or chunk edge as a literal: `REGION_BLOCKS`, `CHUNK_BLOCKS`, `REGION_CHUNKS`. Tests run at region 64 and must not assume it.
- Tests must not install process-global state from the shared `it` test binaries (the installed forged world, an installed structure source, installed prefabs): use the explicit `*_with` functions. A test that must install a global gets its own `[[test]]` target.
- `worldgen` and `game` stay engine-agnostic (no Bevy).
- Builds and tests go through `just`: `just test-one <crate> "<args>"`; several filters need `--` first (`just test-one worldgen "--test it -- a:: b::"`). Never run a whole suite as a routine check. Client tests are ask-first: do not run them. A test slower than 100 ms is tagged `#[ignore = "slow: >100ms — run via just test-full"]` — and a slow test a task depends on is run explicitly with `-- --include-ignored <name>`.
- A test whose outcome reads block props calls the crate's `install_real_block_props()` helper first (see existing worldgen / game tests).
- Commit messages end with the trailer line the session's attribution reminder gives.

## Review Focus

- A placement whose box straddles a region corner → each of the four regions generates its share and the union is the whole prefab, identical to a single-region stamp (Task 3).
- `generate_columns` over half-windows and a one-chunk window equals `generate_region` with a placement and its pad crossing the window edges (Task 3).
- A tree whose crown would reach into a placement's box → no tree block inside the box, and the same trees outside it whichever window generated them (Task 3).
- A carve box below the padded ground (a cellar) next to a neighbouring chunk column → the neighbour's rock is real, not implied ground showing through as void (Task 3).
- A tall prefab over a far ring's coarse cells → the coarse tile shows it at the right height and material, and shows the pad (Task 4).

---

### Task 1: Prefab helpers the structure pass needs

**Files:**
- Modify: `crates/game/src/prefab/layout.rs`, `crates/game/src/prefab/builtin.rs`, `crates/game/src/prefab/mod.rs`
- Modify: `crates/game/tests/prefab_layout.rs`, `crates/game/tests/prefab_builtin.rs`

**Interfaces:**
- Produces, in `game::prefab`:

```rust
/// The highest solid block of the prefab over world column (x, z) under this placement:
/// (world y, top material). None when the column is outside the prefab or empty.
pub fn column_top_world(p: &ResolvedPrefab, at: &Placement, x: i32, z: i32) -> Option<(i32, BlockId)>;
/// The lowest solid block of that column, as a world y.
pub fn column_bottom_world(p: &ResolvedPrefab, at: &Placement, x: i32, z: i32) -> Option<i32>;
/// Like `load_builtin_prefabs(Some(dir))` over several directories, in order, as ONE set: the same file
/// filter, the same "file stem = prefab name" and "a file name appears twice" rules across all of them.
/// A directory that does not exist is an error naming it.
pub fn load_prefab_dirs(dirs: &[&Path], block_named: &dyn Fn(&str) -> Option<BlockId>) -> Result<PrefabSet, String>;
```

`column_top_world` maps the world column back with `cell_of_world` (y is unaffected by turn and mirror: world y = placement.anchor.y + (cell y − prefab.anchor.y)).

- [ ] **Step 1: Failing tests.** `prefab_layout.rs`: `column_top_world_matches_the_layout` — for the busy test prefab under all 8 orientations and two anchors, for every world column of `placement_bounds`, `column_top_world` equals the highest written (non-carve) block of that column found by brute force over `layout_chunk`, with that block's id as material (a shaped cell's material per `ResolvedShape::top_material`), and `column_bottom_world` the lowest; a column outside the bounds → `None`. `prefab_builtin.rs`: `several_directories_load_as_one_set` (two temp dirs with one valid prefab each → a set of two, `content_hash` independent of directory order is NOT required — just equal for equal input order); `the_same_name_in_two_directories_is_refused`; `a_missing_directory_is_an_error`.
- [ ] **Step 2: Run** `just test-one game "--test it -- prefab_layout:: prefab_builtin::"` — the new tests FAIL to compile.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same command — PASS.
- [ ] **Step 5: Commit**: `feat(prefab): world-space column top and bottom; load one set from several directories`.

---

### Task 2: `worldgen::structures` — placements, the source, the pad

**Files:**
- Create: `crates/worldgen/src/structures.rs`, `crates/worldgen/tests/structures.rs`
- Modify: `crates/worldgen/src/lib.rs` (`pub mod structures;`), `crates/worldgen/tests/it.rs` (`mod structures;`)

**Interfaces:**
- Consumes: `game::prefab::{ResolvedPrefab, Placement, placement_bounds, column_top_world, column_bottom_world}`.
- Produces:

```rust
/// Level the ground under a placement. `level` is the world y of the top ground block inside the
/// placement's x/z box (the prefab's lowest layer normally sits at level + 1); over `skirt` further
/// columns the ground blends back to the natural surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pad { pub level: i32, pub skirt: u32 }
pub const MAX_PAD_SKIRT: u32 = 16;

#[derive(Debug, Clone)]
pub struct StructurePlacement { pub prefab: Arc<ResolvedPrefab>, pub at: Placement, pub pad: Option<Pad> }
impl StructurePlacement {
    /// World-block box of the prefab: (min inclusive, max exclusive).
    pub fn bounds(&self) -> ([i32; 3], [i32; 3]);
    /// The x/z box grown by the pad's skirt (0 without a pad): [x0, z0, x1, z1], max exclusive.
    pub fn reach_xz(&self) -> [i32; 4];
}

/// What a pad does to one ground column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadEffect { None, Level { surface: i32, in_box: bool } }

pub trait StructureSource: Send + Sync {
    /// Indices' worth of placements whose `reach_xz` grown by `margin` intersects the half-open x/z
    /// rectangle, in the source's own fixed order. Must not depend on anything but its arguments.
    fn placements_in(&self, x0: i32, z0: i32, x1: i32, z1: i32, margin: i32) -> Vec<&StructurePlacement>;
    /// Identity of the whole source, for the worldgen cache key.
    fn content_hash(&self) -> u32;
}

/// No placements. `content_hash()` is 0.
pub struct NoStructures;

/// An explicit list with a spatial index (x/z buckets of `STRUCTURE_BUCKET_BLOCKS` = 64).
pub struct StructureList { /* placements in the order given + BTreeMap<[i32; 2], Vec<u32>> */ }
impl StructureList {
    /// `set_hash` = the prefab set's `content_hash()` the placements were drawn from.
    pub fn new(placements: Vec<StructurePlacement>, set_hash: u32) -> Self;
    pub fn len(&self) -> usize; pub fn is_empty(&self) -> bool;
}

/// The surface of ground column (x, z) under these placements: the FIRST placement (in list order)
/// whose pad reaches the column decides. Inside its x/z box: `Level { surface: level, in_box: true }`.
/// At Chebyshev distance d in 1..=skirt from the box: `surface = level + (natural - level) * d /
/// (skirt + 1)` (i64 arithmetic, truncating division), `in_box: false`. Otherwise `PadEffect::None`.
pub fn pad_at(placements: &[&StructurePlacement], x: i32, z: i32, natural: i32) -> PadEffect;

/// Whether trees must stay off column (x, z): inside any placement's x/z box grown by `tree_reach`.
pub fn blocks_trees(placements: &[&StructurePlacement], x: i32, z: i32, tree_reach: i32) -> bool;

// Process global, same shape as `forged::install` / `forged::active`:
/// Panics if a source was already installed.
pub fn install(source: Arc<dyn StructureSource>);
/// The installed source, or `&NoStructures`.
pub fn active() -> &'static dyn StructureSource;
```

`content_hash` of a `StructureList`: crc32 over `set_hash` and, per placement in order, the prefab's `name_key`, `at.anchor`, `at.turn`, `at.mirror`, and the pad (a presence byte, `level`, `skirt`), all little-endian. An empty list hashes like any other (it is not 0) — only `NoStructures` is 0. `StructureList::new` clamps a pad's `skirt` to `MAX_PAD_SKIRT`.

- [ ] **Step 1: Failing tests** `tests/structures.rs` (prefabs built in memory with `game::prefab::resolve` from small `PrefabFile`s and a three-name block table — no block props, no global install):
  - `the_index_finds_what_brute_force_finds`: 200 placements at LCG positions over ±2000 blocks with mixed sizes, turns and pads; for 100 LCG query rectangles and margins 0, 5, 40: `placements_in` returns exactly the placements whose `reach_xz` grown by the margin intersects, in list order (compare `at` values).
  - `pad_levels_the_box_and_blends_the_skirt`: a 6 × 6 box at x 10..16, z 20..26, `Pad { level: 50, skirt: 3 }`, natural surface 58: inside → `Level { 50, true }`; d = 1, 2, 3 → 52, 54, 56 with `in_box: false`; d = 4 → `None`; natural 42 → 48, 46, 44 (rises to meet); natural 50 → 50 at every distance.
  - `the_first_placement_decides`: two overlapping pads with different levels → the first in list order wins on every shared column.
  - `skirt_is_clamped`: a pad with `skirt: 1000` behaves as `MAX_PAD_SKIRT`.
  - `trees_keep_their_distance`: `blocks_trees` true inside the box and within `tree_reach` of it (Chebyshev), false one further.
  - `content_hash_tracks_content`: equal lists hash equal; changing an anchor, a turn, a pad level or `set_hash` changes it; `NoStructures.content_hash() == 0`.
  - `negative_coordinates`: a placement at x −130, z −7 is found by a query around it and not by one that only touches the bucket.
- [ ] **Step 2: Run** `just test-one worldgen "--test it structures::"` — FAIL (module missing).
- [ ] **Step 3: Implement.** No terrain sampling anywhere in this module.
- [ ] **Step 4: Run** — PASS.
- [ ] **Step 5: Commit**: `feat(worldgen): structure placements, a spatial source, and the ground pad`.

---

### Task 3: The structure pass in `columns_from_patch`

**Files:**
- Modify: `crates/worldgen/src/lib.rs` (`columns_from_patch`, `generate_columns`, `generate_region`, `generate_region_forged` and their callers inside the crate)
- Create: `crates/worldgen/tests/structure_pass.rs`; Modify: `crates/worldgen/tests/it.rs`

**Interfaces:**
- Consumes: Task 2's module; `game::prefab::{layout_chunk, ChunkStamp, placement_bounds}`.
- Produces:

```rust
/// `generate_columns` with an explicit structure source and no forged world (procedural terrain from
/// `seed`). `generate_columns` itself becomes this with `structures::active()` on the procedural branch.
pub fn generate_columns_with(coords: RegionCoords, xs: Range<usize>, zs: Range<usize>, seed: u64, mats: Materials,
                             structures: &dyn structures::StructureSource) -> Vec<(ColumnCoords, Column)>;
pub fn generate_region_with(coords: RegionCoords, seed: u64, mats: Materials,
                            structures: &dyn structures::StructureSource) -> Vec<(ColumnCoords, Column)>;
/// `generate_region_forged` with an explicit source; `generate_region_forged` passes `structures::active()`.
pub fn generate_region_forged_with(world: &forged::ForgedWorld, coords: RegionCoords, mats: Materials,
                                   structures: &dyn structures::StructureSource) -> Vec<(ColumnCoords, Column)>;
```

`columns_from_patch` gains a `structures: &dyn StructureSource` parameter. Its pass, in this order (line numbers are as of the plan's base; find the code by what it does):

1. **Query once.** `let near = structures.placements_in(window x/z, margin = ring + TREE_REACH_BLOCKS)` — the window in world blocks, the margin covering the sample ring. If `near` is empty, skip every step below (the fast path that keeps output identical).
2. **Pad the samples** — right after the `samples` vector is filled and BEFORE slopes are computed. For every sample (the whole ring, not only the window), with `(x, z)` its world column: `match pad_at(&near, x, z, c.s)`; on `Level { surface, .. }` set `c.s = surface`, `c.top = surface`, clear glacier ice on that column (`ice_from` to the value `column_sample` gives a column without ice), and leave `water_top` and `cover` alone. Then, for every sample where `blocks_trees(&near, x, z, TREE_REACH_BLOCKS)`: `c.trees = false`.
3. **Trees** then need no change: `tree_ground` already returns `None` where `c.trees` is false.
4. **Band.** For each chunk column, with its footprint's world x/z box grown by 1: for every placement in `near` whose prefab x/z box intersects it, `hi = hi.max(box max y − 1)` and `lo = lo.min(box min y)` — applied where the tree blocks raise `hi`, before `base_sub_y` is computed. Never lower `lo` below `min over the footprint of (c.s − UNDERGROUND_DEPTH + 1)` (nothing is written at or below bedrock).
5. **Write** — after the tree blocks are written and before the chunks are frozen into sub-chunks. For each sub-chunk `sub_y` of the column and each placement in `near` (list order, so a later one overrides) whose box intersects the chunk: `layout_chunk(prefab, &at, [wcx, sub_y, wcz])` with `wcx = coords.x * REGION_CHUNKS + cx` (as `i32`); for each carve index: block = air, `chisel.remove`; for each write: set the block, and insert its chisel or `chisel.remove` when it has none. Skip any cell whose world y is `<=` that block column's `c.s − UNDERGROUND_DEPTH` (bedrock and below).
6. `ColumnGround` is built from the padded samples as it is today (its `surface` is the padded ground); nothing else changes.

- [ ] **Step 1: Failing tests** `tests/structure_pass.rs` (procedural terrain via `generate_*_with`, a fixed seed, the crate's real-block-props helper, prefabs resolved in memory from `PrefabFile`s using real block names; pick a region and anchor by scanning for dry land with a helper, never a hard-coded height). Slow ones are `#[ignore]`d and run explicitly:
  - `no_source_is_byte_identical`: for three regions (one forested, one at the origin, one negative), `bincode(generate_region_with(.., &NoStructures)) == bincode(generate_region(..))`, and the same for `generate_columns` over a half window.
  - `a_hut_stands_on_its_pad`: a 7 × 5 × 7 box prefab (stone-brick floor at cell y 0, plank walls, hollow via carve, a `Stairs` roof ring) placed with its anchor at ground + 1 and `Pad { level: ground, skirt: 4 }`: read the generated columns back — every prefab cell holds its block or chisel at the right world position (compare against `layout_chunk`), the interior carve is air, the ground under the whole x/z box is at `level` (the top terrain block is at `level`, air above it outside prefab cells), and the skirt columns' tops follow `pad_at`.
  - `window_independence_with_a_structure`: a placement and pad straddling the middle of a region: `generate_columns_with` over `(0..half, all)`, `(half.., all)`, `(all, 0..half)` and a single chunk column containing the box corner, stitched or compared column by column, equals `generate_region_with` (bincode per column).
  - `a_structure_crosses_a_region_corner_whole`: a 20 × 6 × 20 prefab anchored so its box spans four regions: generating the four regions and reading every prefab cell back gives exactly the brute-force layout; no cell missing at the seams.
  - `trees_stay_out`: in a forested region with a placement: no block with a tree id inside the prefab's box nor within `TREE_REACH_BLOCKS` columns of it at or above the pad; and the set of tree blocks farther than `TREE_REACH_BLOCKS + 2 × TREE_LATTICE_BLOCKS + MAX_PAD_SKIRT` from the box is identical to the no-structure generation.
  - `a_cellar_has_real_walls`: a prefab whose carve box reaches 6 blocks below its anchor, on a pad: the carved cells are air, and each face-neighbouring cell outside the box, in the neighbouring chunk columns too, reads as rock from the column's stored sub-chunks (the neighbour column's band reaches down to the cellar floor) — check `base_sub_y` of every chunk column within one block of the box.
  - `later_placements_override`: two overlapping placements → the overlap holds the second's blocks.
  - `nothing_below_bedrock`: a prefab reaching 80 blocks below ground writes nothing at or below `surface − UNDERGROUND_DEPTH`, and generation does not panic.
- [ ] **Step 2: Run** `just test-one worldgen "--test it -- --include-ignored structure_pass::"` — FAIL (functions missing).
- [ ] **Step 3: Implement.** Thread `structures` through every internal caller (`generate_columns`' two branches, `generate_region_forged`, any example or tool inside the crate that calls `columns_from_patch`); the public no-suffix functions pass `structures::active()`.
- [ ] **Step 4: Run** the Step 2 command — PASS. Then the existing guards, which must be unchanged: `just test-one worldgen "--test it -- --include-ignored trees_in_terrain::window_independent_generation scene_hash_golden"` and `just test-one worldgen "--lib deterministic"`.
- [ ] **Step 5: Commit**: `feat(worldgen): the structure pass — padded ground, no trees, a grown band, prefab blocks after terrain`.

---

### Task 4: The far-terrain twin in `coarse_from_patch`

**Files:**
- Modify: `crates/worldgen/src/lib.rs` (`coarse_from_patch`, `coarse_tile`, `coarse_tile_forged`, `water_from_patch` / `coarse_water` and their forged variants)
- Create: `crates/worldgen/tests/structure_coarse.rs`; Modify: `crates/worldgen/tests/it.rs`

**Interfaces:**
- Consumes: Tasks 1–3 (`pad_at`, `blocks_trees`, `column_top_world`, `StructureSource`).
- Produces:

```rust
pub fn coarse_tile_with(key: &game::lod::LodKey, seed: u64, mats: Materials,
                        structures: &dyn structures::StructureSource) -> Vec<game::lod::LodCell>;
pub fn coarse_tile_forged_with(world: &forged::ForgedWorld, key: &game::lod::LodKey, mats: Materials,
                               structures: &dyn structures::StructureSource) -> Vec<game::lod::LodCell>;
/// The same for the coarse water layer (a pad that raises ground above the water top removes the
/// water there, as it does in full generation).
pub fn coarse_water_with(/* the arguments of coarse_water */, structures: &dyn structures::StructureSource) -> /* its return type */;
```

The existing no-suffix functions pass `structures::active()`. The pass:

1. Query `placements_in` once for the key's window (margin = the tree margin the function already uses). Empty → nothing below runs.
2. **Pad**: wherever a ground column is sampled for the tile (the per-cell sample, the memoised tree-surface sample, the water sample), apply `pad_at` to its surface exactly as Task 3 step 2 does (`s`, `top`, ice cleared), and `trees = false` under `blocks_trees`.
3. **Profile**: after terrain cells are filled and before the tree pass, for each cell column, take its CENTRE block column `(x, z)` (the same point the terrain sample uses); for each placement in order whose box contains it: `column_top_world` → `(top_y, material)`; fill that cell column's cells from the first cell above the (padded) terrain top through the cell containing `top_y` with `material`, overwriting air only for cells above terrain, and — for a later placement — overwriting an earlier placement's cells. A column whose prefab column is empty contributes nothing.

- [ ] **Step 1: Failing tests** `tests/structure_coarse.rs`:
  - `no_source_is_identical`: `coarse_tile_with(.., &NoStructures) == coarse_tile(..)` for factors 16 and 64 over two keys, and the same for the water layer.
  - `a_tower_shows_at_factor_16`: a 9 × 40 × 9 solid `stone_bricks` tower on a pad: at factor 16 (one block per cell) every cell column under the tower's box is solid `stone_bricks` from above the pad to the tower's top, and the cells one beyond the box are not; the pad's level shows as the terrain top around it.
  - `a_tower_shows_at_factor_64`: at 4 blocks per cell, the cell columns whose centre block column is in the box are filled to the cell containing the top; others are not.
  - `the_coarse_tile_tracks_full_generation`: for the hut-on-a-pad region, the factor-16 tile's per-cell-column top height agrees with the top solid block of `generate_region_with` for at least 95 % of the columns within 24 blocks of the placement (the existing coarse-vs-full tolerance), and for 100 % of the columns inside the prefab's box.
  - `trees_are_suppressed_the_same_way`: no canopy cell over the box or within `TREE_REACH_BLOCKS` of it.
- [ ] **Step 2: Run** `just test-one worldgen "--test it -- --include-ignored structure_coarse::"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — PASS; then the existing coarse guards: `just test-one worldgen "--lib coarse"` and `just test-one worldgen "--test it -- --include-ignored trees_in_terrain::coarse_lod_shows_canopy"`.
- [ ] **Step 5: Commit**: `feat(worldgen): far terrain shows structures — the pad and each prefab's column profile in coarse tiles`.

---

### Task 5: Cache key, a height query, and the dev source

**Files:**
- Modify: `crates/worldgen/src/cache.rs`, `crates/worldgen/src/lib.rs`, `crates/worldgen/src/structures.rs`
- Create: `crates/worldgen/tests/structure_dev.rs`; Modify: `crates/worldgen/tests/it.rs`

**Interfaces:**
- Produces:

```rust
// lib.rs
/// The y of the top ground block of world column (x, z) — the same value generation uses as the
/// column's surface before any structure pad. Uses the installed forged world, else `seed`.
pub fn ground_y(x: i32, z: i32, seed: u64, mats: Materials) -> i32;
pub fn ground_y_forged(world: &forged::ForgedWorld, x: i32, z: i32, mats: Materials) -> i32;

// structures.rs
/// One entry of a dev structure spec: `name`, `name:dx,dz`, or `name:dx,dz:turn` (turn 0..=3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevEntry { pub prefab: String, pub offset: Option<[i32; 2]>, pub turn: u8 }
pub const DEV_PAD_SKIRT: u32 = 4;
pub const DEV_GAP_BLOCKS: i32 = 6;
/// Parses `"dev-hut,statuen-06:40,0:1"`. Errors name the bad entry.
pub fn parse_dev_spec(spec: &str) -> Result<Vec<DevEntry>, String>;
/// Lays the entries out beside `origin` (a world x/z, e.g. a landing column): an entry with an offset
/// is anchored at origin + offset; entries without one are put in a row along +x starting at
/// origin.x + DEV_GAP_BLOCKS, each after the previous one's x extent plus DEV_GAP_BLOCKS, all at
/// origin.z. Each prefab's anchor lands at (x, ground(x, z) + 1, z) with `Pad { level: ground(x, z),
/// skirt: DEV_PAD_SKIRT }`, where `ground` is called for the ANCHOR column only. Unknown prefab → Err.
pub fn dev_placements(entries: &[DevEntry], origin: [i32; 2], prefabs: &game::prefab::PrefabSet,
                      ground: &dyn Fn(i32, i32) -> i32) -> Result<Vec<StructurePlacement>, String>;
```

`cache.rs`: `cache_key_for` additionally hashes `structures::active().content_hash()` **only when it is non-zero**, so the key is unchanged with nothing installed.

- [ ] **Step 1: Failing tests** `tests/structure_dev.rs`:
  - `parse`: the three forms; errors for `""` entries, a bad number, `turn` 4, a missing name.
  - `a_row_beside_the_origin`: two prefabs without offsets (sizes 7 and 12 wide) with a flat `ground = |_, _| 70`: anchors' x so that the first box starts `DEV_GAP_BLOCKS` past the origin and the second `DEV_GAP_BLOCKS` past the first's box end; y 71; pads at level 70, skirt 4; an explicit offset is honoured; an unknown name is an error naming it.
  - `ground_y_is_the_generated_surface`: for 20 LCG columns of a procedural region, `ground_y(x, z, seed, mats)` equals the top non-air terrain block of that column in `generate_region_with(.., &NoStructures)` when the column has no tree, ice or water above ground (skip those columns; assert at least 10 were checked).
  - `the_cache_key_ignores_no_structures`: unit test in `cache.rs` — the key computed with `NoStructures` equals the key from the function as it was (hash the same inputs by hand in the test), and a source with a non-zero hash changes it. (Reach the internals with a `cache_key_with(…, structures_hash: u32)` helper that `cache_key_for` calls.)
- [ ] **Step 2: Run** `just test-one worldgen "--test it -- structure_dev::"` and `just test-one worldgen "--lib cache"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — PASS.
- [ ] **Step 5: Commit**: `feat(worldgen): ground_y, a dev structure spec, and structures in the worldgen cache key`.

---

### Task 6: Wire it into the server and the debug launch

**Files:**
- Modify (probably left uncommitted — check): `crates/server/src/lib.rs`, `crates/client/src/debug_launch.rs`, `justfile`
- Create: `crates/server/src/dev_structures.rs` (committed), `crates/server/tests/dev_structures.rs` + a `[[test]]` entry in `crates/server/Cargo.toml` (its own target: it installs globals)
- Modify: `docs/superpowers/plans/2026-11-14-prefabs-minecraft-import-site-generation.md` (a short "how to look at a prefab in the game" note under §8), `CLAUDE.md` is NOT touched (it is a dirty file owned by others — put the note in the roadmap only)

**Interfaces:**
- Consumes: `game::prefab::{load_prefab_dirs, install_prefabs, prefabs, BUILTIN_PREFAB_DIR}`; `worldgen::structures::{parse_dev_spec, dev_placements, StructureList, install}`; `worldgen::ground_y`.
- Produces:
  - `server::dev_structures::install_from_env(registry: &game::BlockRegistry, landing_of: &dyn Fn(game::RegionCoords) -> Option<[u32; 2]>, seed: u64, mats: worldgen::material::Materials) -> Result<Option<usize>, String>` — reads the environment and installs; returns the number of placements, `None` when the feature is off. Environment:
    - `VOXEL_PREFAB_DIRS` — extra prefab directories, `:`-separated, loaded together with `assets/prefabs` (native dev only; unset = the built-in set alone). The built-in set is ALWAYS installed (`install_prefabs`), with or without this.
    - `VOXEL_DEV_STRUCTURES` — `<rx>,<rz>=<spec>` where `<spec>` is a `parse_dev_spec` string: the placements are laid out by `dev_placements` beside region `(rx, rz)`'s landing column (`landing_of(rc)`, else the region centre), the origin being `(rc.x * REGION_BLOCKS + landing.x, rc.z * REGION_BLOCKS + landing.z)`, `ground` = `worldgen::ground_y`. Unset = no structure source is installed and generation is untouched.
  - In `server/src/lib.rs`: one call, after the forged world is installed and BEFORE the worldgen cache key / any generator or LOD worker is built; an `Err` aborts startup with the message; one `info!` line `[structures] N placements from VOXEL_DEV_STRUCTURES` when on.
  - `client debug embarked --prefab <spec>` (repeatable; specs are joined with `,`): the client sets `VOXEL_DEV_STRUCTURES=<rx>,<rz>=<spec>` for the server it spawns, using the embark region it chose, and passes `VOXEL_PREFAB_DIRS` through unchanged if set. `USAGE` documents it and says it needs a region that has never been saved (`--reset`, or `VOXEL_DEBUG_EMBARK_AT`).
  - `justfile`: the `run` recipe's comment gains one line with an example: `just run --reset --prefab dev-hut`, and `VOXEL_PREFAB_DIRS=prefab-imports/statuen just run --reset --prefab statuen-06`.

- [ ] **Step 1: Failing test** `crates/server/tests/dev_structures.rs` (own `[[test]]`, since it installs process globals; procedural terrain — no forged world): set `VOXEL_DEV_STRUCTURES="0,0=dev-hut"`, call `install_from_env` with the real registry, a `landing_of` returning `Some([20, 20])`, the world seed and real materials → `Ok(Some(1))`; `worldgen::structures::active().placements_in(…region (0,0)…)` has one placement named `dev-hut` whose anchor is `DEV_GAP_BLOCKS` east of column (20, 20) at `ground_y + 1`; then `worldgen::generate_region(rc(0,0), seed, mats)` contains `stone_bricks` blocks at the hut's floor cells (read three of them back by world position). A second test function in the same file must NOT install again (one process, one install) — put every assertion in the one test.
- [ ] **Step 2: Run** `just test-one server "--test dev_structures"` — FAIL (module missing).
- [ ] **Step 3: Implement** `dev_structures.rs`, wire the call in `lib.rs`, add the client flag (hand-parsed like the others, with its unit test beside the existing `parse_args` tests — do not run client tests; `just smoke` type-checks it), the justfile comment, and the roadmap note.
- [ ] **Step 4: Run** the Step 2 command — PASS; `just smoke` — pass.
- [ ] **Step 5: Commit** what is committable (`dev_structures.rs`, the test, `Cargo.toml`, the roadmap note): `feat(server): dev structures — stamp prefabs beside a region's landing from the environment`.
