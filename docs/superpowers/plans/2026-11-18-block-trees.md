# Block Trees Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generated trees are made of whole blocks, like Minecraft: every log, branch and leaf block is a full
1 m block. Worldgen stops writing chisel masks for tree wood.

**Architecture:** The species table and `skeleton()` stay as they are. That keeps the shapes: a conical spruce,
an umbrella acacia, a buttressed jungle tree. Only the rasteriser changes. `rasterize()` stops sweeping
capsules over 16³ voxels and decides wood per block. A block is wood when its centre lies inside a segment's
tapered capsule. Each segment's axis is also walked as a face-connected (6-connected) line of blocks, so a trunk
or limb thinner than a block is still a continuous column or staircase of whole blocks. `TreeBlock.chisel` is
deleted, so the compiler finds every consumer of the masks. Leaves stay as they are (already whole blocks).

**Tech Stack:** Rust; `worldgen` (the `noise.rs` math policy), `game` (felling, block registry), the
`tree_view` example for looking at the result.

**Spec:** none. The owner asked on 2026-11-18: "Instead of trees being at voxel detail, I want each log to
be a full block like Minecraft." The old design is `docs/superpowers/specs/2026-10-29-trees-investigation.md`
(§4.5 rasteriser) and its plan `docs/superpowers/plans/2026-10-30-trees.md`. This plan replaces only §4.5's
wood fill.

## Global Constraints

- Work on `develop` in this checkout. No git worktrees. Commit only your own paths (`git diff --cached
  --name-only` before every commit, commit with a pathspec).
- Worldgen tree code uses only `+ - * /`, `sqrt`, `floor`, integer math and integer-indexed tables (no
  `sin/cos/powf`).
- `rasterize()` stays a pure function of the skeleton. Output is sorted by `offset` in (y, z, x) order, with
  unique offsets.
- Verify with `just test-one worldgen "…"` / `just test-one game "…"` and `just smoke`. Never run whole suites.
  No benchmarks.
- Tests slower than 100 ms get `#[ignore = "slow: >100ms — run via just test-full"]`.
- Saved worlds are never migrated. Regions saved before this change keep voxel trees, and regenerated regions
  get block trees. Dev worlds are re-forged (`just run --reset`, `just pg-reset`). Wiping a deployed world is the
  owner's call and is not part of this plan.

## Decisions

1. **Wood rule:** for each skeleton segment whose thinner radius is ≥ `MIN_WOOD_RADIUS` (unchanged), a block
   is wood when its centre `(b·16 + 8)` is inside the capsule (the same swept-sphere test as today, sampled once
   per block) **or** the block lies on the segment's axis walk. The thickest trunks (`trunk_radius` 13 voxels on
   oak) stay 1×1. Only flared bases and segments with a radius over ~16 voxels get wider.
2. **Axis walk:** `block_line(a, b)` goes from the block holding node `a` to the block holding node `b`. Each
   step moves along exactly one axis, choosing the axis with the largest remaining error relative to the total
   delta (integer 3D Bresenham split into face steps). The output includes both ends. Consecutive segments
   share their node's block, so a limb is one face-connected chain from the trunk to its tip.
3. **Log vs branch:** unchanged (`BRANCH_LEVEL`). Logs keep the rings-on-top texture even where they run
   sideways. Oriented logs (`_x` / `_z` variants or an axis in `Block.data`) are out of scope. `Block.data` has
   no free bits (tree id 18 + limb 6), so that would be its own plan. Listed under follow-ups.
4. **Limb attribution** is unchanged: a wood block takes the lowest limb of the segments that fill it, and
   leaves the lowest limb of their clusters.
5. **Connectivity tightens:** wood must be face-connected to the base, not 26-connected. That gives the
   Minecraft look, and no block hangs by an edge.
6. **Colliders:** log and branch blocks keep `collider: Half`. A full block is a full box either way, and Half
   still matters once a player chisels a log.

## File Structure

- Modify `crates/worldgen/src/trees/raster.rs`: block-scale `Grid`, `wood_segment`, the new `block_line`,
  and deletion of `TreeBlock::chisel`, `Wood`, `BLOCK_MASK_LEN` and `SWEEP_STEP_FRAC`. Its tests change too.
- Modify `crates/worldgen/src/lib.rs`: `PlacedTreeBlock` loses `chisel`, `tree_blocks()` stops building
  `ChiselData`, and the tree stamp near line 977 stops inserting tree chisels.
- Modify `crates/worldgen/tests/tree_fall.rs` (`scene()`) and `crates/worldgen/examples/tree_view.rs`: drop
  the mask handling.
- Possibly modify `crates/worldgen/src/trees/species.rs`: retune only if a species-shape test fails (Task 2).

---

### Task 1: Block-scale rasteriser

**Files:**
- Modify: `crates/worldgen/src/trees/raster.rs`
- Modify: `crates/worldgen/src/lib.rs` (`PlacedTreeBlock`, `tree_blocks`, the stamp at ~977)
- Modify: `crates/worldgen/tests/tree_fall.rs` (`scene`), `crates/worldgen/examples/tree_view.rs`

**Interfaces:**
- Produces: `pub struct TreeBlock { offset: [i32; 3], part: TreePart, limb: u8 }`, with no `chisel` field.
  `pub fn rasterize(sk: &TreeSkeleton) -> Vec<TreeBlock>` keeps its signature.
  `fn block_line(a: [i32; 3], b: [i32; 3]) -> Vec<[i32; 3]>` is block coordinates in, a face-connected path out.
- Removed: `BLOCK_MASK_LEN`, `SWEEP_STEP_FRAC`, `struct Wood` and its bit rows. `Grid.wood` becomes per-cell
  `Option<(bool /*log*/, u8 /*limb*/)>`.

- [ ] **Step 1: Write the failing tests** in `raster.rs`'s `tests`:

```rust
#[test]
fn block_line_is_face_connected_and_ends_at_both_nodes() {
    for (a, b) in [([0, 0, 0], [0, 5, 0]), ([0, 0, 0], [3, 4, -2]), ([2, 7, 1], [-4, 3, 6]), ([1, 1, 1], [1, 1, 1])] {
        let l = block_line(a, b);
        assert_eq!((l[0], *l.last().unwrap()), (a, b));
        assert!(l.windows(2).all(|w| (0..3).map(|i| (w[0][i] - w[1][i]).abs()).sum::<i32>() == 1), "{a:?}→{b:?}");
        let manhattan: i32 = (0..3).map(|i| (a[i] - b[i]).abs()).sum();
        assert_eq!(l.len() as i32, manhattan + 1);
    }
}

/// A slim trunk (narrower than a block, no flare) stands on exactly one block.
#[test]
fn slim_trunks_stand_on_one_block() {
    for sp in SPECIES.iter().filter(|s| s.shape.trunk_radius < BV && s.shape.base_flare_pct == 0) {
        for seed in 0..12 {
            let base: Vec<_> = rasterize(&skeleton(sp, seed, MAX_AGE))
                .into_iter().filter(|b| b.offset[1] == 0 && b.part != TreePart::Leaves).collect();
            assert_eq!(base.iter().map(|b| b.offset).collect::<Vec<_>>(), vec![[0, 0, 0]], "{} seed {seed}", sp.name);
        }
    }
}
```

  Change `trunk_is_connected_to_base` to walk the 6 face neighbours instead of the 26. Rename it
  `wood_is_face_connected_to_base`. In `blocks_are_sorted_unique_and_well_formed`, delete the
  `partial`/`chisel` assertions and keep the order, base-is-`Log` and has-leaves assertions. Update
  `wood_wins_over_leaves`: a radius-8 trunk now gives the single column `[0, y, 0]` as `Log`, and `[1, 2, 0]`
  stays `Leaves`.

  (If `ShapeParams` names the flare field differently, use its real name. `species.rs:111` shows
  `base_flare_pct`.)

- [ ] **Step 2: Run them and confirm they fail**

Run: `just test-one worldgen "--lib trees::raster"`
Expected: compile error (`block_line` not found). Once a stub exists, `slim_trunks_stand_on_one_block` fails
on the voxel rasteriser.

- [ ] **Step 3: Implement**

  - `block_line`: Decision 2. Integer only. Ties pick the lowest axis index (x, then y, then z) so the output
    is deterministic.
  - `wood_segment(grid, a, b, ra, rb, level, limb)`: skip segments with thinner radius below
    `MIN_WOOD_RADIUS`. For every block in the capsule's block bounding box, mark it wood if its centre is within
    `r(t)` of the closest point of segment ab, where `t` is the projection clamped to [0, 1] and
    `r(t) = ra + (rb − ra)·t`. Then mark every block of `block_line(block_of(a), block_of(b))`.
    `block_of(p) = p.div_euclid(16)`, with y clamped ≥ 0 like today. Marking ORs `log` and takes the minimum
    `limb`, as `fill_row` does now.
  - `Grid::new` keeps its block bounds. `rasterize` emits `TreeBlock { offset, part, limb }`.
  - Delete the masks: `TreeBlock::chisel`, `Wood`, `BLOCK_MASK_LEN`, `SWEEP_STEP_FRAC`. Update the module doc
    comment to the new rule.
  - `lib.rs`: drop `PlacedTreeBlock::chisel` and the `ChiselData::from_fn` in `tree_blocks`. At the stamp
    (~line 977), stop inserting a tree chisel. Still REMOVE any chisel already in that cell, such as a slope's:
    a tree block over it must be whole, the same as `stamp_structures` does for a write without a chisel.
  - `tree_fall.rs` `scene()` and `tree_view.rs`: delete the mask branches. In `tree_view`, `solid` is always
    the whole block. Drop the "chiseled" count from its summary line.

- [ ] **Step 4: Run the raster tests and the compile check**

Run: `just test-one worldgen "--lib trees::"` then `just smoke`
Expected: the new tests pass. `height_within_species_max`, `mature_trees_fit_the_flood_bound`,
`blocks_carry_their_limb` and `pine_is_taller_than_wide` pass unchanged. `each_new_species_skeleton_fits_its_crown`
may fail on a shape assertion; that is Task 2's input, so note which ones and move on. Smoke passes.

- [ ] **Step 5: Commit**

```bash
git add crates/worldgen/src/trees/raster.rs crates/worldgen/src/lib.rs crates/worldgen/tests/tree_fall.rs crates/worldgen/examples/tree_view.rs
git commit -m "feat(worldgen): trees are whole blocks — wood by block centre + a face-connected axis walk, no chisel masks"
```

### Task 2: Shapes, felling and goldens

**Files:**
- Modify (only if needed): `crates/worldgen/src/trees/species.rs`
- Test: `crates/worldgen/src/trees/raster.rs` (species-shape tests), `crates/worldgen/tests/tree_fall.rs`,
  `crates/worldgen/tests/scene_hash_golden.rs`

**Interfaces:**
- Consumes: `rasterize` from Task 1.

- [ ] **Step 1: Run the species-shape and felling tests**

Run: `just test-one worldgen "--lib trees::raster::tests::each_new_species_skeleton_fits_its_crown"` and
`just test-one worldgen "--test it tree_fall::"`
Expected: both pass. If a shape assertion fails (the likely one is jungle's `layer(0) > layer(8)` buttress
once its trunk is quantised), retune that species' `ShapeParams` in `species.rs`, for example a larger
`base_flare_pct` or `trunk_radius`. Check it with `cargo run -p worldgen --example tree_view` (see its header
for arguments). Never loosen the assertion. Felling failures mean `game::trees::cut_blocks` relied on wood
being wider than 1 block in a layer. Report those instead of patching the test.

- [ ] **Step 2: Look at every species**

Run the `tree_view` example for each species at `MAX_AGE` and at a young age. Check that trunks are 1×1
columns (jungle wider at the base), limbs are face-connected staircases, and no wood block floats in the crown.
Note in the commit message anything that reads badly, such as long single-block twigs at level ≥ 2. If twigs
clutter the crowns, the dial is `MIN_WOOD_RADIUS`: raise it and re-run Task 1's tests.

- [ ] **Step 3: Goldens**

Run: `just test-one worldgen "--test it -- --include-ignored scene_hash_golden"` and `just determinism-wasm`
Expected: `scene_hash_golden` builds with `TreeBlocks::NONE` and should not move. If either hash moves,
re-record it (`SCENE_HASH_PRINT=1 …`, as in the test's header) and say in the commit message that it moved
because of tree wood.

- [ ] **Step 4: Commit**

```bash
git add crates/worldgen/src/trees/species.rs   # only if it changed
git commit -m "tune(worldgen): species shapes at block scale"
```

### Task 3: Look at it in the game, then docs

- [ ] **Step 1:** `just run` with `client debug world --reset` (a fresh forge). Walk into a forest. Check that
  trees are blocky, leaves are cutout, and chopping a trunk fells the tree. The far LOD crowns
  (`crown_extent`) are unchanged.
- [ ] **Step 2:** Change the first line of `docs/superpowers/specs/2026-10-29-trees-investigation.md` §4.5 to
  "superseded 2026-11-18 by `plans/2026-11-18-block-trees.md`: wood is whole blocks". Add a `TODO.md` line for
  the follow-up below.
- [ ] **Step 3:** Commit the doc paths.

## Review Focus

1. **Diagonal or leaning trunks** (`trunk_lean` 10–14 on acacia and palm): the axis walk must give a staircase
   with no gaps, and no layer more than 2 blocks wide. Covered by `wood_is_face_connected_to_base`. Also look
   at it in `tree_view`.
2. **A tree on a slope block:** the stamp must remove the slope's chisel under a tree block, or a log ends up
   carved. Task 1 Step 3 covers it. Add a check to `tree_fall.rs` `scene` that no tree cell has a chisel.
3. **Saplings (young `age`):** the trunk radius is floored at 2 voxels (`MIN_TRUNK_RADIUS`), so a sapling must
   still give at least the axis-walk column. `slim_trunks_stand_on_one_block` runs at `MAX_AGE` only, so also
   run it at age 5.
4. **Crown leaves cut off from wood:** fewer wood blocks means the leaf flood-fill (leaves face-connected to
   wood) can drop more leaves. Compare leaf counts in `tree_view`'s summary before and after. A big drop means
   the clusters need wood under them.
5. **Old worlds:** a dev world forged before this change mixes voxel trees (saved regions) with block trees
   (new regions). Re-forge it. No code handles the mix.

## Follow-ups (not in this plan)

- **Oriented logs:** rings on the end faces of sideways limbs. This needs a per-axis texture
  (`TextureSpec::Faces` has only top/side/bottom) and either axis block variants or a bit in `Block.data`
  (which is full).
