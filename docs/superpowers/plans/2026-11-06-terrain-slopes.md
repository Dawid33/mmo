# Terrain Slopes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** World generation stops emitting half blocks; every 1-block rise gets a walkable, material-matched Minecraft-style slope carved into the higher column's top block, and pathfinding / the adventure walker walk up slopes instead of jumping (jumping 1 m blocks stays everywhere else).

**Architecture:** A slope is a block's own material plus `ChiselData` geometry (bottom half + top half/quarters), decided purely from a column's sampled height and its 8 neighbours in `crates/worldgen`, described by one shape module `game::slope` that worldgen, the sim (pathfinding, walker) and the client (tile mirror, height grid) all share.

**Tech Stack:** Rust, `game` / `worldgen` / `client` crates, rapier KCC (vendored fork, untouched), `just` recipes.

**Spec:** `docs/superpowers/specs/2026-11-06-terrain-slopes-design.md` (read it first — shape table, placement table, directional walk-up rule, fallback ladder).

## Global Constraints

- Work on `develop` in the main checkout. No worktrees (CLAUDE.md).
- Verify with `just test-one <crate> "<target> <filter>"` and `just smoke`. Never run whole suites. Client unit tests are a slow opt-in: ASK Dawid before running `just test-one client "--lib …"`; otherwise rely on `just smoke`'s client type-check.
- Tag any new test slower than 100 ms `#[ignore = "slow: >100ms — run via just test-full"]`, and run it with `-- --include-ignored`.
- Jumping up a full 1 m block must keep working everywhere (no change to `STEP_HEIGHT`, `JUMP_SPEED`, or pathfind's `dy = +1` successor).
- No new block ids; `Block.data` untouched; slope geometry is `ChiselData` built through `ChiselData::from_fn` (canonical cuboid list).
- Voxel constants: `BLOCK_VOXELS` = 16, `HALF_BLOCK_VOXELS` = 8 — never literals.
- Do not patch the KCC fork. If Task 2's fallback (a) fails, take fallback (b) and STOP to report.
- Game-crate goldens (`physics_goldens`, `wasm_determinism`) are hand-built and must not change; only `crates/worldgen/tests/scene_hash_golden.rs` is expected to re-pin.
- Commit after each task (Dawid commits on develop; end messages with the attribution lines from the session's system reminder).

## Review Focus

1. **Window independence.** Slope shape and tree eligibility read neighbour columns, so generating a region whole vs in windows must still give identical chunks — pinned by the existing `trees_in_terrain::window_independent_generation` (Task 4 Step 6).
2. **Region seams.** A column on a region edge reads neighbours in the next region. Generation is a pure function of world block coords, so both regions must agree — Task 4 test `slopes_agree_across_a_region_seam`.
3. **Shore and water.** A slope never sits at or under its column's water surface (an air pocket), but a step down to water still gets one — Task 4 test `no_slope_at_or_under_water`.
4. **Approach from a wrong side.** Climbing onto a slope from its high or lateral side must stay a jump climb (cost and walker), never a walk-up the body cannot make — Task 3 tests `walk_up_only_from_the_low_side` and `walker_jumps_onto_a_slope_from_its_side`.
5. **Edits on slopes.** Breaking or mining a slope block clears it to air and drops one whole block of its material, as a half block does now — Task 1 test `slope_breaks_like_a_half_block`.

---

### Task 1: `game::slope` — the shape module

**Files:**
- Create: `crates/game/src/slope.rs`
- Modify: `crates/game/src/lib.rs` (add `pub mod slope;` and re-export `pub use slope::*;` with the other glob re-exports)
- Test: unit tests inside `slope.rs`; `crates/game/tests/block_edit.rs` (edit test)

**Interfaces:**
- Produces:
  - `pub enum Side { PosX, NegX, PosZ, NegZ }` with `pub fn offset(self) -> [i32; 2]` (`[dx, dz]`).
  - `pub enum SlopeShape { Straight { low: Side }, Outer { low_x: Side, low_z: Side }, Inner { low_x: Side, low_z: Side } }` — derives `Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, StateHash`. `low_x` ∈ {PosX, NegX}, `low_z` ∈ {PosZ, NegZ}; Outer = low on both cardinals, Inner = low only on that diagonal.
  - `pub fn slope_chisel(material: BlockId, shape: SlopeShape) -> ChiselData`
  - `pub fn classify(chisel: &ChiselData) -> Option<SlopeShape>` — `Some` only for exactly one of the 12 slope geometries (any material), else `None` (half blocks, arbitrary chisels).
  - `pub fn walk_up(shape: SlopeShape, from: [i32; 2]) -> bool` — `from` = `[dx, dz]` from the slope tile to the tile the mover comes from (each −1..=1, not both 0). Straight: `from == low.offset()`; Outer: `from` ∈ {low_x, low_z, low_x+low_z diagonal}; Inner: `from == ` the low diagonal only.
  - `pub fn display_name(id: BlockId, chisel: Option<&ChiselData>) -> String` — the manifest name (`block_props().name_of`) with `_` → space and first letter capitalised, plus `" slope"` when `chisel.and_then(classify).is_some()`. E.g. `red_sand` + slope → `"Red sand slope"`, `dirt` + none → `"Dirt"`.

- [ ] **Step 1: Write the failing unit tests in `slope.rs`**

```rust
#[test] fn straight_fills_bottom_half_and_back_top_half() {
    // low side +x: every voxel with y < HALF is solid; y >= HALF solid iff x < HALF.
    let c = slope_chisel(STONE, SlopeShape::Straight { low: Side::PosX });
    for (x, y, z) in all_voxels() { assert_eq!(c.is_solid(x, y, z), y < HALF || x < HALF); }
}
#[test] fn outer_keeps_one_top_quadrant_inner_keeps_three() {
    // Outer{PosX,PosZ}: top solid iff x < HALF && z < HALF. Inner{PosX,PosZ}: top solid unless x >= HALF && z >= HALF.
}
#[test] fn classify_round_trips_all_twelve_shapes() { /* every shape, two materials */ }
#[test] fn classify_rejects_half_blocks_and_full_chisels() {
    assert_eq!(classify(&ChiselData::cuboid(STONE, [0,0,0], [BLOCK_VOXELS, HALF_BLOCK_VOXELS, BLOCK_VOXELS])), None);
}
#[test] fn slope_chisels_are_canonical() {
    // slope_chisel(..) == ChiselData::from_grid(palette, &its own grid) for every shape.
}
#[test] fn walk_up_only_from_the_low_side() {
    let s = SlopeShape::Straight { low: Side::NegZ };
    assert!(walk_up(s, [0, -1]));
    for from in [[0, 1], [1, 0], [-1, 0], [1, -1], [-1, -1]] { assert!(!walk_up(s, from)); }
    let o = SlopeShape::Outer { low_x: Side::PosX, low_z: Side::PosZ };
    for from in [[1, 0], [0, 1], [1, 1]] { assert!(walk_up(o, from)); }
    assert!(!walk_up(o, [-1, 0]));
    let i = SlopeShape::Inner { low_x: Side::PosX, low_z: Side::PosZ };
    assert!(walk_up(i, [1, 1]));
    assert!(!walk_up(i, [1, 0]) && !walk_up(i, [0, 1]));
}
#[test] fn display_names() {
    // needs the block registry installed (see how other game unit tests install it)
    assert_eq!(display_name(red_sand, Some(&slope_chisel(red_sand, SlopeShape::Straight { low: Side::PosX }))), "Red sand slope");
    assert_eq!(display_name(dirt, None), "Dirt");
    assert_eq!(display_name(dirt, Some(&half_block(dirt))), "Dirt");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `just test-one game "--lib slope::"` — Expected: compile error (module missing).

- [ ] **Step 3: Implement `slope.rs`** per the Interfaces block. `slope_chisel` uses `ChiselData::from_fn(vec![material], |x, y, z| solid.then_some(0))`. `classify` compares `chisel.cuboids()` geometry (ignoring material, requiring one material) against the 12 shapes' cuboids.

- [ ] **Step 4: Run to verify they pass** — `just test-one game "--lib slope::"`: all PASS.

- [ ] **Step 5: Edit test** — in `crates/game/tests/block_edit.rs` add `slope_breaks_like_a_half_block`: a region whose floor block is `slope_chisel(dirt, Straight{low: PosX})`; the player breaks it → the cell is air with no chisel; the same setup through `mining` drops one `ItemId::block(dirt)` (mirror the existing half-block / mining tests in that file and `tests/mining.rs` for setup). Run `just test-one game "--test it block_edit::slope"` → PASS (expected to pass without production changes; if it fails, fix `edit.rs` / `mining.rs` so a slope behaves exactly like a half block).

- [ ] **Step 6: Commit** — `feat(game): slope shape module (shapes, chisel, classify, walk_up, names)`.

---

### Task 2: Bodies walk up slopes (physics gate)

**Files:**
- Create: `crates/game/tests/slope_walk.rs`; Modify: `crates/game/tests/it.rs` (`mod slope_walk;`)
- Maybe modify: `crates/game/src/movement.rs:338-356` (`character_controller` autostep `min_width`)

**Interfaces:**
- Consumes: `slope_chisel`, `SlopeShape`, `Side` (Task 1); test helpers in `crates/game/tests/common` (`world_of`, `key`, `ticks`, `body`, `teleport`, `spawn_z_walk`, `spawn_x_band`, as `walking_smoothness.rs` uses them).
- Produces: nothing for later tasks except the answer "walk-up works" (or fallback (b), see Step 4).

- [ ] **Step 1: Write the failing tests** (tag each slow if >100 ms)

`slope_walk_up_and_down_every_facing`: for each of the 4 `Straight` facings, a flat floor at level 0 with a 1-block-high plateau starting at a row/column, whose first block toward the low side is a `slope_chisel` of that facing; teleport the body onto the low floor 2 m in front, hold W (yaw set to face the slope) for enough ticks to pass it, never pressing Space. Assert: the feet end on the plateau (feet y ≈ floor + 1 block within `0.05` m) and the max per-tick vertical rise ≤ `STEP_HEIGHT` m. Then walk back down and assert the feet return to level 0 with no tick ungrounded for more than 2 ticks.

`slope_walk_diagonal_onto_outer_and_inner`: same for `Outer` and `Inner` approached along their low diagonal.

`full_block_still_needs_a_jump`: the same plateau with no slope — walking without Space stays on level 0; with Space held it climbs (guards the "jump stays" rule).

- [ ] **Step 2: Run** — `just test-one game "--test it -- --include-ignored slope_walk::"`. Expected: `full_block_still_needs_a_jump` PASS; the slope tests either PASS already (likely — `min_width` evaluates to ≤ 0.4 m against a 0.5 m tread) or FAIL with the body stopped at the riser.

- [ ] **Step 3: If a slope test fails — fallback (a):** set autostep `min_width: CharacterLength::Absolute(0.3 * UNITS_PER_METER)` (a named const `AUTOSTEP_MIN_WIDTH_M = 0.3` beside `STEP_HEIGHT`). Re-run Step 2, then `just test-one game "--test it walking_smoothness:: shape_cast_grazing:: physics_goldens:: jump_arc::"` (with `-- --include-ignored`) — all must PASS; re-record `physics_goldens` only if they moved and say so in the commit.

- [ ] **Step 4: If still failing — fallback (b):** keep the tests as `#[ignore = "blocked: autostep cannot walk a 0.5 m tread"]`, make `pathfind::walk_up` callers treat every slope as a jump (Task 3 still lands its API, with the cost rule disabled behind `const SLOPE_WALK_UP: bool = false`), finish Task 4, and STOP before Task 5 to report to Dawid. Do not patch `crates/rapier`.

- [ ] **Step 5: Commit** — `test(game): bodies walk up terrain slopes without jumping` (+ the const if Step 3 ran).

---

### Task 3: Pathfinding and the walker use slopes

**Files:**
- Modify: `crates/game/src/orders.rs:158-178` (`TileSource`), its `Rollback` impl (~`orders.rs:295`)
- Modify: `crates/game/src/pathfind.rs` (successors cost, heuristic, new helpers)
- Modify: `crates/game/src/adventure.rs` (`Walker`, `steer`, every `Walker::new` / re-plan site), `crates/game/src/people.rs` (its walker construction and `steer` call), `crates/game/src/camera.rs:265`
- Test: `crates/game/tests/pathfind.rs`, `crates/game/tests/adventure.rs`

**Interfaces:**
- Consumes: `SlopeShape`, `classify`, `walk_up` (Task 1).
- Produces:
  - `TileSource::slope(&self, p: TilePos) -> Option<SlopeShape>` with a default `None` (test sources keep compiling). `Rollback` impl: `classify` the chisel of the block at `p`.
  - `pathfind::step_is_walk_up(src: &impl TileSource, from: TilePos, to: TilePos) -> bool` — `to[1] == from[1] + 1` and `src.slope([to[0], to[1] - 1, to[2]])` is `Some(s)` with `walk_up(s, [from[0] - to[0], from[2] - to[2]])`.
  - `pathfind::walk_ups(src: &impl TileSource, path: &[TilePos]) -> Vec<bool>` — `[false]` then `step_is_walk_up(path[i-1], path[i])`.
  - `Walker.walk_up: Vec<bool>` (same length as `path`), set wherever `path` is set; `Walker::new(goal, path, exit, walk_up)`.
  - `adventure::steer(walker, feet, grounded)` unchanged in signature: it does not hold jump when `walker.walk_up[walker.next]` is true.

- [ ] **Step 1: Failing pathfind tests** in `tests/pathfind.rs` (extend `Src` with a `slopes: HashMap<TilePos, SlopeShape>` + `fn slope`):
  - `slope_climb_costs_a_level_step`: a 1-block step with one column of `Straight` slope (low side toward the start) 3 tiles to the side of the direct route; the path goes over the slope (assert the path contains the tile above the slope block), and `find_path` still returns `Some`.
  - `wrong_side_slope_is_still_a_jump_climb`: start beside the slope (lateral); assert `!step_is_walk_up(src, beside, on_slope)` and that a path still exists (climb allowed).
  - `climbs_one_block_step` (existing) stays green — the jump climb is unchanged.

- [ ] **Step 2: Run** — `just test-one game "--test it pathfind::"` → new tests FAIL.

- [ ] **Step 3: Implement** in `pathfind.rs`: in `successors`, a `dy == 1` move adds `CLIMB_COST` only when `!step_is_walk_up(src, p, q)`; `heuristic` drops the `CLIMB_COST * dy` term (keeps it admissible). Add the two helpers. Add `TileSource::slope` default + the `Rollback` impl.

- [ ] **Step 4: Run** — `just test-one game "--test it pathfind::"` → PASS.

- [ ] **Step 5: Walker** — add `walk_up` to `Walker`, fill it with `walk_ups` at every path assignment (initial plan, re-plan, `resume_walker_safe`, people's walks); `steer` sets `jump_held: climb && !walker.walk_up[next]`. Tests in `tests/adventure.rs` (slow-tagged, like its neighbours): `walker_walks_up_a_slope_without_jumping` (MoveTo across a slope step; assert the goal is reached and that the body's `Motion` vertical velocity never reaches `JUMP_SPEED` on any tick of the walk — a jump starts at exactly `JUMP_SPEED`) and `walker_jumps_onto_a_slope_from_its_side` (goal reachable only by a lateral climb onto the slope tile: arrives).

- [ ] **Step 6: Run** — `just test-one game "--test it -- --include-ignored adventure:: pathfind:: people::"` → PASS; `just smoke` → PASS.

- [ ] **Step 7: Commit** — `feat(game): pathfinding and walkers walk up slopes from their low side`.

---

### Task 4: Worldgen — whole blocks + slopes

**Files:**
- Create: `crates/worldgen/src/slopes.rs` (pure placement rule)
- Modify: `crates/worldgen/src/lib.rs` (rounding 82–133, `ColSample` 135–155, `point_sample` 164–180, glacier 237, `tree_ground` 278–283, sample ring 537–547, cliff rule 591–596, emission 655–666, coarse tree memo 873–883, unit tests 998–1064)
- Modify: `crates/worldgen/examples/quantize_probe.rs` (report slopes, not halves)
- Test: `crates/worldgen/src/lib.rs` unit tests, `crates/worldgen/tests/{scene_hash_golden,lod_bricks,underground,trees_in_terrain,tree_fall}.rs`, new `crates/worldgen/tests/slopes.rs` (+ `mod slopes;` in `tests/it.rs`)
- Docs: CLAUDE.md worldgen paragraph ("whole blocks and half blocks only" → whole blocks + slopes; `quantize_probe` line); add a "Superseded by 2026-11-06-terrain-slopes-design.md" line at the top of `docs/superpowers/specs/2026-09-22-block-and-half-block-worldgen.md`.

**Interfaces:**
- Consumes: `SlopeShape`, `Side`, `slope_chisel`, `classify` (Task 1).
- Produces: `pub(crate) fn slope_for(c: &ColSample, n: impl Fn(i32, i32) -> ColSample) -> Option<SlopeShape>` in `slopes.rs` — `n(dx, dz)` returns the neighbour sample; implements the spec's placement table exactly: eligible only if the column has ground, `c.s > c.water_top` and `c.top == c.s`; a neighbour is low when `n.top == c.top - 1`; one low cardinal → Straight; two adjacent → Outer; none + exactly one low diagonal → Inner; anything else → `None`.

- [ ] **Step 1: Failing tests.**
  - `slopes.rs` unit tests for `slope_for` over hand-made `ColSample` neighbourhoods: each table row (straight ×4, outer, inner, opposite → None, three → None, two diagonals → None, 2-block drop → None, submerged → None, glacier → None, low neighbour under water → Straight).
  - Rewrite `terrain_is_whole_blocks_and_dry_half_blocks_only` → `terrain_is_whole_blocks_and_dry_slopes_only`: every non-tree chisel `classify`s to `Some`, at least one slope exists in the fixture, none at or under water, and no one-cuboid half block exists anywhere.
  - Update `analytic_surface_scan_matches_the_full_band_scan` to the new `SURFACE_TEST_M`.
  - `crates/worldgen/tests/slopes.rs`: `slopes_agree_across_a_region_seam` (generate regions (0,0) and (1,0); for every edge column pair, the slope decision computed from each side equals what was emitted) and `no_slope_at_or_under_water` (over a coastal region: every slope's block y > its column's water top).

- [ ] **Step 2: Run** — `just test-one worldgen "--lib slopes:: terrain_is_whole"` and `just test-one worldgen "--test it -- --include-ignored slopes::"` → FAIL.

- [ ] **Step 3: Rounding.** Replace `HALF_TEST_M` / `FULL_TEST_M` with `const SURFACE_TEST_M: f64 = 0.5;` (the surface block is the highest whose density at `by + 0.5` is positive), delete `surface_is_full` and `ColSample.full`; update the doc comment above it.

- [ ] **Step 4: Placement + emission.** In the region path compute `slope_for` per column from `at` (a second pass after `samples`, stored in a parallel `Vec<Option<SlopeShape>>` indexed like `samples`, covering the ring minus its outer block). Emission: at `by == c.top` of a slope column write `slope_chisel(block.id, shape)` instead of the old half-block cuboid. Cliff rule: `open_lo` uses `n.top` (not `n.top + 1`) when the neighbour has a slope. Ring: `ring = if with_trees { TREE_RING_BLOCKS + 1 } else { 1 }`.

- [ ] **Step 5: Trees.** `tree_ground(c, slope: bool)` → `None` on a slope column (drop the `c.full` test). Region path: pass the column's slope; coarse path (`coarse_from_patch` ~873): memo the `ColSample`s and compute `slope_for` from memoised neighbours; widen its margin assertion by 1 block (`TREE_PATCH_MARGIN` if needed).

- [ ] **Step 6: Run** — Step 2 commands → PASS; then `just test-one worldgen "--test it -- --include-ignored underground:: trees_in_terrain:: lod_bricks:: tree_fall:: player_walk::"` → PASS (`lod_bricks`' "fixture has half blocks" assertion becomes "fixture has slopes"; `tree_fall` tolerances: adjust only if a sink tolerance fails, explaining why in the commit).

- [ ] **Step 7: Re-pin the scene golden** — `SCENE_HASH_PRINT=1 just test-one worldgen "--test it -- --include-ignored --nocapture scene_hash_golden::"`, write the printed values for chunk sizes 2 and 16 into `SCENE_HASH` (run the size-2 one with `just chunk_blocks=2 …`), re-run → PASS. Also `just test-one worldgen "--lib"` and `just smoke` → PASS.

- [ ] **Step 8: Probe + docs** — update `quantize_probe` to count slopes by shape and rises walkable without a jump; run it with `nice -n 10 taskset -c 0-15 cargo run -p worldgen --release --example quantize_probe` (no recipe exists; add `just quantize-probe` wrapping that line, per the keep-the-justfile-in-sync rule) and paste its summary into the commit body. Apply the CLAUDE.md and old-spec edits listed under Files.

- [ ] **Step 9: Commit** — `feat(worldgen): whole-block terrain with material slopes on 1-block rises`.

---

### Task 5: Client — tile mirror and height grid

**Files:**
- Modify: `crates/client/src/renderer/commander/tiles.rs` (`TileMirror` + its `TileSource` impl)
- Modify: `crates/client/src/renderer/commander/height.rs` (`top_not` / `chunk_tops`)
- Test: unit tests in those two files

**Interfaces:**
- Consumes: `classify`, `SlopeShape`, `TileSource::slope` (Tasks 1, 3).
- Produces: `TileMirror` answers `slope(p)` for home-region tiles: alongside each chunk's `Box<[Block]>` it keeps `BTreeMap<u16 /* BlockIndex linear */, SlopeShape>` built from the chunk's chisels when the chunk is mirrored (`set_chunk` and the bridge path).

- [ ] **Step 1: Failing tests** — `tiles.rs`: `mirror_reports_slopes` (a chunk with one `slope_chisel` block and one half block: `slope()` is `Some(shape)` for the first, `None` for the second and for plain blocks). `height.rs`: `slope_counts_as_full_height` (for all 4 straight facings the column top equals the block's top, where the block-centre sampling used to give a half height for −x/−z facings); keep `chunk_tops_are_per_block_column` green.

- [ ] **Step 2: Implement** — mirror: build the map in the chunk-ingest path; height: a chiselled block whose `classify` is `Some` reports its full block top; other chisels keep the centre-column rule.

- [ ] **Step 3: Verify** — `just smoke` → PASS (type-checks the client). ASK Dawid before running `just test-one client "--lib commander::tiles:: commander::height::"`; run it only with the go-ahead and report the result.

- [ ] **Step 4: Commit** — `feat(client): mirror slopes for path previews; slopes are full height for the camera`.

- [ ] **Step 5: Hand-off for the live look** — tell Dawid: `just run --reset` (the debug world's saved regions keep their old half blocks), look at a hillside and a shore, walk up a slope in Adventure (F5) and with a MoveTo, and check the move preview takes slopes.
