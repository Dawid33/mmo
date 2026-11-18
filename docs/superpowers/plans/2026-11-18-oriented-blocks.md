# Oriented Blocks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A log, pillar, hay bale or bone block can lie along X or Z. It shows its end texture on the two faces
of its axis and its side texture, with the grain along the axis, on the other four. This works for blocks a
player places, prefab and imported builds, and the lying limbs of generated trees.

**Architecture:**
- **One block id per axis** (research §1 option (a)). A manifest entry gains `axes: (x: <id>, z: <id>)`. The
  registry expands it at load into `<name>_x` and `<name>_z` entries; the base entry is the Y variant.
- Orientation is part of the `BlockId`, so chisel palettes, greedy merging, the far LOD and the WebGL2 path
  need no change. `Block.data` stays free for the tree id.
- The client's layer map becomes six layers per block (`FaceLayers`).
- `build_layers` adds 90°-rotated copies of side textures as extra array layers.
- **No change** to the quad record, the WGSL shaders or `pack.rs`'s bit layout.

**Tech Stack:** Rust; `game` (registry, block props, mining, prefab), `client` (block textures, meshing),
`worldgen` (tree rasteriser), `mc-import`; RON manifest `assets/blocks/blocks.ron`.

**Spec:** `docs/superpowers/specs/2026-11-18-block-visuals-research.md` §1 (and "What the engine has today").

## Global Constraints

- Work on `develop` in this checkout. No git worktrees. Commit only your own paths: check
  `git diff --cached --name-only`, then commit with a pathspec (other sessions stage in the shared index).
- The quad record (`meshing/records.rs`) and its mirrors (`voxel_pull.wgsl`, `voxel_pull_depth.wgsl`, `pack.rs`
  bit layout, `voxel_texture.wgsl`) are **not** changed by this plan. If a task seems to need it, stop and ask.
- `game` and `worldgen` stay Bevy-free. Sim state stays deterministic: `BTreeMap`, never `HashMap`.
- Tests:
  - Run with `just test-one <crate> "<args>"`. More than one name filter goes after `--`
    (`"--test it -- a:: b::"`). Never run whole suites.
  - Client tests are a slow opt-in. **Ask the owner once before Task 4's test run.** Run them only there and in
    Task 5's check.
  - `just smoke` after each client-touching task.
  - `just determinism-wasm` after Task 5 (worldgen output changes).
- Tests slower than 100 ms get `#[ignore = "slow: >100ms — run via just test-full"]`.
- Worlds are never migrated. Saved worlds keep their (all-Y) logs; dev worlds are re-forged
  (`just run --reset`, `just pg-reset`).
- Ids **168–187** are taken by this plan (the manifest's highest id today is 167). Other plans start at 188.

## Decisions

1. **Families.** Oriented families: `oak_log` 13, `pine_log` 16, `birch_log` 19, `spruce_log` 33, `acacia_log` 36,
   `palm_log` 39, `jungle_log` 42, `quartz_pillar` 55, `bone_block` 78, `hay_bale` 164. They get X/Z ids
   168/169, 170/171, … 186/187, in that order.
   - The `stripped_*` blocks are `All(...)` with no end texture, so turning them changes nothing visible:
     they are not oriented.
   - Branches are `All` too and stay unoriented.
2. **Generated entries** copy every field of the base (textures, solid, kind, render, collider, hardness),
   except `name` (`<base>_x` / `<base>_z`) and `drop`. A base `drop` of `None` or `Same` becomes
   `Item("<base name>")`; an explicit `Nothing` or `Item` is copied. **Every variant drops and refunds the
   base's item.** X and Z variants never become items.
3. **The sim reads orientation from `BlockProps`** (installed on server and client from the same registry,
   equal by assert). It gains a family table and the methods `oriented`, `family_base`, `axis_of`, `turned`.
   A variant of a trunk base is trunk (`is_trunk`), so chopping a lying tree limb works as today.
4. **Placement** (survival right click) picks the axis of the clicked face's normal. Clicking the side of a
   block lays the log pointing out of that face, as in other block games. Creative and editor placement
   (`edit::PLACE_BLOCK`) and build designations stay on the base (Y) variant: out of scope.
5. **Texture rotation.** Bark grain runs along image V. The shader's face UV tables are: ±X U=z V=−y; ±Z U=x
   V=−y; ±Y U=x V=z. So:

   | Variant | ±X | ±Y | ±Z |
   |---|---|---|---|
   | Y (base) | side | end (top / bottom) | side |
   | X | end | side rotated 90° | side rotated 90° |
   | Z | side rotated 90° | side (grain along V = z) | end |

   The rotation is `image::imageops::rotate90` (clockwise). Grain is symmetric, so the direction only has to
   be the same everywhere. A rotated layer is interned under the key `"<path>#r90"`, so a side texture shared by
   two families yields one rotated layer.
6. **Riser fade and cutaway cap** (`side_to_top`, `top_layer` in `voxel_pull.wgsl:447`): a block contributes
   entries **only when its four side layers are equal** (upright blocks). A lying log's side layers map to
   themselves (identity).
   - Without that rule, X-log "end" sides would map the shared rings layer to bark and corrupt the
     Y-log entries.
   - The cost: a distant lying log's riser fades its normal but keeps its own texture.
7. **Worldgen:** a wood block's axis is the dominant axis of the segment that won the block's limb. That is
   the largest `|b − a|` component; ties prefer Y, then X, then Z. Only `TreePart::Log` blocks take the variant;
   branches and leaves are unoriented. A missing `_x` / `_z` name resolves to the base log, which keeps
   hand-written test manifests working.
8. **Prefab turns:** an odd quarter turn swaps X and Z variants in every block a prefab cell writes. A mirror
   swaps nothing (an axis is symmetric).

## File Structure

- `crates/game/src/registry.rs`: `Axis`, `AxisIds`, `BlockDef.axes`, the expansion at load,
  `BlockRegistry::oriented` / `axis_of`, family props.
- `crates/game/src/block_props.rs`: the family table and its four methods.
- `crates/game/src/mining.rs`: placement axis, refund to base.
- `crates/game/src/prefab/layout.rs`: turned variants in `cell_write_cached`.
- `assets/blocks/blocks.ron`: `axes:` on the ten families.
- `crates/client/src/renderer/block_textures.rs`: `FaceLayers`, rotated layers, `side_to_top` rule.
- `crates/client/src/renderer/meshing/pack.rs`, `records.rs`, `emit.rs`, `queue.rs`, `slice_plane.rs`,
  `voxel_bodies.rs`, `compute.rs` (tests): migrate from `[u32; 3]`.
- `crates/worldgen/src/trees/raster.rs`: `TreeBlock.axis`. `crates/worldgen/src/material.rs`:
  `SpeciesBlocks.log_x` / `log_z`. `crates/worldgen/src/lib.rs`: `tree_blocks` picks the variant.
- `crates/mc-import/src/classify.rs`, `crates/mc-import/src/table.rs`, `crates/mc-import/data/materials.ron`:
  the `axis` state.

## Review Focus

1. **Refund of a refused lying-log place gives an X/Z item.** A refused place must refund the base item;
   otherwise the inventory holds an unplaceable-looking `oak_log_x` stack that never stacks with `oak_log`.
   Test in Task 2.
2. **Breaking a lying log drops `oak_log`, and stacks with an upright one.** Test in Task 1 (props) and Task 2
   (pile).
3. **A turned prefab with a lying beam keeps the beam along its own run.** An odd turn must swap X↔Z; an even
   turn must not. Test in Task 3.
4. **Chopping a generated tree through a lying limb log** still fells (variant is trunk, keeps the tree id).
   Test in Task 5.
5. **A side texture shared by two families** (none today, likely later) must give one rotated layer, not a
   layer per family. Test in Task 4.

---

### Task 1: Registry and props: axis families

**Files:**
- Modify: `crates/game/src/registry.rs`, `crates/game/src/block_props.rs`, `assets/blocks/blocks.ron`
- Test: `crates/game/src/registry.rs` (`mod tests`), `crates/game/tests/block_props.rs`

**Interfaces:**
- Produces (all re-exported at the `game` crate root, as `registry` items already are):
  - `pub enum Axis { X, Y, Z }`, deriving `Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde`.
  - `Axis::of_normal(n: [i32; 3]) -> Option<Axis>`: the one non-zero component; `None` otherwise.
  - `Axis::turned(self, quarter_turns: u8) -> Axis`: X↔Z on an odd count.
  - `pub struct AxisIds { pub x: u16, pub z: u16 }` (serde).
  - `BlockDef.axes: Option<AxisIds>` with `#[serde(default)]`. RON: `axes: (x: 168, z: 169)`; implicit-some is
    already on.
  - `BlockRegistry::oriented(&self, base: BlockId, axis: Axis) -> BlockId`: the family member on `axis` for any
    member of a family; `base` itself for a block with no family.
  - `BlockRegistry::axis_of(&self, id: BlockId) -> Option<(BlockId, Axis)>`: (family base, axis) for every
    family member, the base included (Y); `None` for a block with no family.
  - `BlockProps`:
    - new field `pub(crate) oriented: BTreeMap<u16, [u16; 3]>`, keyed by every family member, holding the
      family's `[x, y, z]` ids;
    - `oriented(&self, id: BlockId, axis: Axis) -> BlockId`;
    - `family_base(&self, id: BlockId) -> BlockId` (the Y member; `id` when there is no family);
    - `axis_of(&self, id: BlockId) -> Option<Axis>`;
    - `turned(&self, id: BlockId, quarter_turns: u8) -> BlockId`.
    - The static `DEFAULT` in `block_props()` gets an empty `oriented`.

- [ ] **Step 1: Write the failing registry tests** (in `registry.rs` `mod tests`)

```rust
const ORIENTED: &str = r#"(blocks:[
    (id:0,name:"air",textures:Untextured),
    (id:1,name:"log",textures:Faces(top:"r.png",side:"b.png",bottom:"r.png"),hardness:200,axes:(x:5,z:6)),
    (id:2,name:"plank",textures:All("p.png")),
])"#;

#[test]
fn axes_expand_into_named_variants() {
    let reg = BlockRegistry::from_ron(ORIENTED).unwrap();
    assert_eq!(reg.id_of("log_x"), Some(BlockId(5)));
    assert_eq!(reg.id_of("log_z"), Some(BlockId(6)));
    let x = reg.def(BlockId(5)).unwrap();
    assert_eq!(x.textures, reg.def(BlockId(1)).unwrap().textures);
    assert_eq!(x.hardness, Some(200));
    assert_eq!(x.drop, Some(DropDef::Item("log".into())));
    assert_eq!(x.axes, None);
}

#[test]
fn oriented_and_axis_of_round_trip() {
    let reg = BlockRegistry::from_ron(ORIENTED).unwrap();
    for (axis, id) in [(Axis::X, 5), (Axis::Y, 1), (Axis::Z, 6)] {
        assert_eq!(reg.oriented(BlockId(1), axis), BlockId(id));
        assert_eq!(reg.oriented(BlockId(5), axis), BlockId(id), "from any member");
        assert_eq!(reg.axis_of(BlockId(id)), Some((BlockId(1), axis)));
    }
    assert_eq!(reg.oriented(BlockId(2), Axis::X), BlockId(2));
    assert_eq!(reg.axis_of(BlockId(2)), None);
}

#[test]
fn axes_colliding_with_an_existing_id_or_name_are_rejected() {
    let id = ORIENTED.replace("axes:(x:5,z:6)", "axes:(x:2,z:6)");
    assert!(matches!(BlockRegistry::from_ron(&id), Err(RegistryError::DuplicateId(2))));
    let name = ORIENTED.replace(r#"name:"plank""#, r#"name:"log_z""#);
    assert!(matches!(BlockRegistry::from_ron(&name), Err(RegistryError::DuplicateName(_))));
}

#[test]
fn shipped_manifest_orients_the_ten_families() {
    let reg = BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
    for base in ["oak_log", "pine_log", "birch_log", "spruce_log", "acacia_log", "palm_log",
                 "jungle_log", "quartz_pillar", "bone_block", "hay_bale"] {
        let b = reg.id_of(base).unwrap();
        assert_eq!(reg.id_of(&format!("{base}_x")), Some(reg.oriented(b, Axis::X)), "{base}");
        assert_eq!(reg.id_of(&format!("{base}_z")), Some(reg.oriented(b, Axis::Z)), "{base}");
    }
    assert_eq!(reg.axis_of(reg.id_of("stripped_oak").unwrap()), None);
}
```

- [ ] **Step 2: Write the failing props tests** (in `crates/game/tests/block_props.rs`)

```rust
#[test]
fn oriented_variants_share_the_base_item_and_trunk() {
    let props = BlockRegistry::from_ron(ORIENTED_TRUNK).unwrap().props();
    // ORIENTED_TRUNK: as ORIENTED in registry tests, with the family named "oak_log".
    let (y, x, z) = (BlockId(1), BlockId(5), BlockId(6));
    assert_eq!(props.drop(x), BlockDrop::Item(ItemId::block(y)));
    assert_eq!(props.drop(z), BlockDrop::Item(ItemId::block(y)));
    assert!(props.is_trunk(x) && props.is_trunk(z) && props.is_trunk(y));
    assert_eq!(props.family_base(x), y);
    assert_eq!(props.axis_of(z), Some(Axis::Z));
    assert_eq!(props.turned(x, 1), z);
    assert_eq!(props.turned(x, 2), x);
    assert_eq!(props.turned(y, 3), y);
    assert_eq!(props.oriented(BlockId(2), Axis::X), BlockId(2));
}

#[test]
fn axis_of_normal() {
    assert_eq!(Axis::of_normal([0, 0, -1]), Some(Axis::Z));
    assert_eq!(Axis::of_normal([1, 0, 0]), Some(Axis::X));
    assert_eq!(Axis::of_normal([0, 1, 0]), Some(Axis::Y));
    assert_eq!(Axis::of_normal([0, 0, 0]), None);
    assert_eq!(Axis::of_normal([1, 1, 0]), None);
}
```

- [ ] **Step 3: Run them to verify they fail**

Run: `just test-one game "--lib registry::"` and `just test-one game "--test it block_props::"`
Expected: compile errors (`Axis`, `axes`, `oriented` undefined).

- [ ] **Step 4: Implement**
  - The expansion runs in `from_ron` before the duplicate checks: push each generated def into the same loop,
    so a collision yields the existing `DuplicateId` / `DuplicateName`. The family table is built alongside.
  - `props()`: fill `oriented` for all three members of each family. Insert the trunk entry for a variant when
    its base is trunk; the `ends_with("_log")` test alone misses `oak_log_x`.
  - Add `axes: (x: 168, z: 169)` … `(x: 186, z: 187)` to the ten families in `blocks.ron`, in the order of
    Decision 1.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `just test-one game "--lib registry::"` and `just test-one game "--test it block_props::"`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/game/src/registry.rs crates/game/src/block_props.rs crates/game/tests/block_props.rs assets/blocks/blocks.ron
git commit -m "feat(blocks): oriented families — one block id per axis, generated at registry load" -- <those paths>
```

### Task 2: Survival placement lays the block along the clicked face

**Files:**
- Modify: `crates/game/src/mining.rs` (`survival_place_safe` ~:206-238, `refund_refused_places_safe` ~:243)
- Test: `crates/game/tests/block_edit.rs`

**Interfaces:**
- Consumes: `Axis::of_normal`, `BlockProps::oriented`, `BlockProps::family_base` (Task 1).
- Produces: nothing new.

- [ ] **Step 1: Write the failing tests** (beside `right_click_places_a_block_that_stops_the_player`; use
  the same `region` / `run` / `click` / `changed` helpers and a hotbar slot holding `oak_log`; the region must
  install the real props, `common::install_real_block_props()`)

```rust
#[test]
fn a_log_placed_on_a_side_face_lies_along_that_face() {
    // A wall block ahead at eye height; click its +z face (the face toward the player, who faces -z).
    // Expect exactly one new block, and it is oak_log_z.
    assert_eq!(diff[0].2, props.oriented(oak_log, Axis::Z));
}

#[test]
fn a_log_placed_on_the_floor_stands_upright() {
    // DOWN_45 onto a flat floor, as in right_click_places_a_block_that_stops_the_player.
    assert_eq!(diff[0].2, oak_log);
}

#[test]
fn a_refused_lying_log_refunds_the_base_item() {
    // Use the seam rig of place_block_across_seam with the owner cell pre-filled (refused: Occupied),
    // clicking a side face so the sent block is oak_log_x / oak_log_z.
    let (_, slot) = place_state(&mut rig, p);
    assert_eq!(slot.map(|s| s.item), Some(ItemId::block(oak_log)));
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `just test-one game "--test it -- block_edit::a_log_placed block_edit::a_refused_lying_log"`
Expected: the side-face test fails with `oak_log` placed instead of `oak_log_z`; the refund test may pass (it
fails only after Step 3 sends a variant; keep it as the regression).

- [ ] **Step 3: Implement**
  - In `survival_place_safe`, after the `hit.normal == [0; 3]` check, set
    `let id = block_props().oriented(id, Axis::of_normal(hit.normal).unwrap_or(Axis::Y));`.
    The inventory still takes the item it holds (the base).
  - In `refund_refused_places_safe`, refund `ItemId::block(block_props().family_base(block))`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `just test-one game "--test it -- block_edit:: mining:: items::"`
Expected: PASS, including the existing place and mine tests.

- [ ] **Step 5: Commit** `feat(mining): a placed log lies along the clicked face; refunds give the base item`.

### Task 3: Prefab turns swap X and Z variants

**Files:**
- Modify: `crates/game/src/prefab/layout.rs` (`cell_write_cached`, ~:118-133)
- Test: `crates/game/tests/prefab_layout.rs`

**Interfaces:**
- Consumes: `BlockProps::turned` (Task 1).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn an_odd_turn_swaps_lying_variants_and_an_even_turn_does_not() {
    // common::install_real_block_props(); a one-cell prefab whose cell is Block(oak_log_x).
    for (turn, want) in [(0, oak_log_x), (1, oak_log_z), (2, oak_log_x), (3, oak_log_z)] {
        let w = cell_write(cell, &Placement { turn, mirror: false, ..at }).unwrap().unwrap();
        assert_eq!(w.0.id, want, "turn {turn}");
    }
    // mirror only: unchanged
    assert_eq!(cell_write(cell, &Placement { turn: 0, mirror: true, ..at }).unwrap().unwrap().0.id, oak_log_x);
    // an upright log never changes
}
```

Add one more assertion for a `Shaped(Named)` cell (its `mat` is turned too, and so is the chisel palette it
builds) and for a `Cuboids` cell (each of `mats` is turned).

- [ ] **Step 2: Run it to verify it fails**

Run: `just test-one game "--test it prefab_layout::an_odd_turn"`
Expected: FAIL at turn 1 (`oak_log_x` kept).

- [ ] **Step 3: Implement**: map every id `cell_write_cached` writes through
  `block_props().turned(id, at.turn)`: `Block`, `Named.mat` (before the chisel cache lookup, so the cache key
  holds the turned id), and each of `mats`.

- [ ] **Step 4: Run the tests**

Run: `just test-one game "--test it prefab_layout::"`
Expected: PASS.

- [ ] **Step 5: Commit** `feat(prefab): a quarter-turned prefab turns its lying blocks`.

### Task 4: Client: six layers per block and rotated side layers

**Files:**
- Modify: `crates/client/src/renderer/block_textures.rs`, `meshing/pack.rs` (`layer_for_face`),
  `meshing/records.rs` (`quads_to_records`), `meshing/emit.rs` (:74 and tests), `meshing/queue.rs` (tests),
  `slice_plane.rs` (`top_layer_lut`, test at ~:402), `voxel_bodies.rs`, `compute.rs` (test `layers()`)
- Test: `block_textures.rs` `mod tests`, `meshing/records.rs` `mod tests`

**Interfaces:**
- Consumes: `BlockRegistry::axis_of` (Task 1).
- Produces (fixed across the three visual plans):
  - `pub struct FaceLayers(pub [u32; 6]);` indexed by `Face::index()` (PosX 0, NegX 1, PosY 2, NegY 3, PosZ 4,
    NegZ 5), deriving `Debug, Clone, Copy, PartialEq, Eq, Default, serde`.
  - `FaceLayers::upright(top: u32, side: u32, bottom: u32) -> FaceLayers`.
  - `FaceLayers::top(self) -> u32` (= `[2]`).
  - `pub struct BlockTextureLayers(pub BTreeMap<BlockId, FaceLayers>)`.
  - `pack::layer_for_face(faces: FaceLayers, face: Face) -> u32` = `faces.0[face.index() as usize]`.

- [ ] **Step 1: Write the failing tests** (in `block_textures.rs`)

```rust
#[test]
fn oriented_variants_put_ends_on_their_axis_and_rotated_sides_around() {
    // ORIENTED registry (Task 1's test manifest); load = a 4x4 image whose texel (x, y) encodes x + 4y,
    // so a rotation is observable.
    let (images, layers) = build_layers(&reg, load);
    let y = layers.0[&BlockId(1)];
    let (r, b) = (y.0[2], y.0[0]);                 // rings (top), bark (side)
    assert_eq!(y, FaceLayers::upright(r, b, r));
    let x = layers.0[&BlockId(5)];
    let rot = x.0[2];
    assert_ne!(rot, b);
    assert_eq!(x.0, [r, r, rot, rot, rot, rot]);
    let z = layers.0[&BlockId(6)];
    assert_eq!(z.0, [rot, rot, b, b, r, r]);
    assert_eq!(images[rot as usize], image::imageops::rotate90(&images[b as usize]));
}

#[test]
fn a_shared_side_texture_rotates_once() {
    // Two families, ends "r.png" and "s.png", both with side "b.png": one rotated layer, not two.
    assert_eq!(images.len(), 1 /* missing */ + 3 /* r, s, b */ + 1 /* b#r90 */);
}

#[test]
fn side_to_top_ignores_lying_variants() {
    let lut = layers.side_to_top();
    assert_eq!(lut[b as usize], r, "upright bark fades to rings");
    assert_eq!(lut[r as usize], r, "rings untouched by the X log's end sides");
    assert_eq!(lut[rot as usize], rot);
}
```

In `records.rs` tests, add `quads_to_records_picks_the_layer_per_face`: a `FaceLayers([10, 11, 12, 13, 14, 15])`
block, one quad per face, decoded layer == `10 + face.index()`.

- [ ] **Step 2: Ask the owner before running client tests** (CLAUDE.md: client tests are a slow opt-in). With
  approval run: `just test-one client "--lib -- block_textures:: meshing::records::"`
  Expected: compile errors (`FaceLayers` undefined).

- [ ] **Step 3: Implement**
  - Change `BlockTextureLayers` to `FaceLayers` and migrate every `[u32; 3]` user listed in **Files** to
    `FaceLayers::upright` / `layer_for_face` / `.top()`.
  - `build_layers`:
    - for `registry.axis_of(id) == Some((base, X | Z))`, take the base's `TextureSpec` (end = top, side =
      side), intern the side as usual and its rotated copy under `"<path>#r90"`, then lay out per the table in
      Decision 5;
    - the base (Y) stays upright;
    - the uniform-size rule and the layer-0 fallback apply to rotated layers as to any other.
  - `side_to_top`: skip blocks whose four side layers (indices 0, 1, 4, 5) differ.

- [ ] **Step 4: Run the tests**

Run: `just test-one client "--lib -- block_textures:: meshing:: slice_plane:: voxel_bodies:: compute::"` (one
run; approval from Step 2 covers it), then `just smoke`.
Expected: PASS; smoke type-checks.

- [ ] **Step 5: Commit** `feat(client): six texture layers per block; lying variants use rotated side layers`.

### Task 5: Worldgen lays lying tree limbs as X/Z logs

Fits either rasteriser state: today's chisel sweep, or block trees (`docs/superpowers/plans/2026-11-18-block-trees.md`,
not built as of 2026-11-18). If block trees has landed, the `axis` is recorded where its per-block wood decision
records the limb.

**Files:**
- Modify: `crates/worldgen/src/trees/raster.rs` (`TreeBlock`, `Grid`, `wood_segment`),
  `crates/worldgen/src/material.rs` (`SpeciesBlocks`, `TreeBlocks::from_names`, `NONE`),
  `crates/worldgen/src/lib.rs` (`tree_blocks`, ~:577-610)
- Test: `crates/worldgen/src/trees/raster.rs` `mod tests`; `crates/game/tests/chop.rs`

**Interfaces:**
- Consumes: `game::Axis` (Task 1).
- Produces:
  - `TreeBlock.axis: game::Axis` (Y for leaves and branches);
  - `pub fn segment_axis(a: [i32; 3], b: [i32; 3]) -> game::Axis` in `raster.rs`;
  - `SpeciesBlocks { log, log_x, log_z, branch, leaves }`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn segment_axis_is_the_dominant_component_ties_prefer_y_then_x() {
    assert_eq!(segment_axis([0, 0, 0], [0, 40, 3]), Axis::Y);
    assert_eq!(segment_axis([0, 0, 0], [-30, 5, 10]), Axis::X);
    assert_eq!(segment_axis([0, 0, 0], [3, -2, 25]), Axis::Z);
    assert_eq!(segment_axis([0, 0, 0], [10, 10, 10]), Axis::Y);
    assert_eq!(segment_axis([0, 0, 0], [10, 0, 10]), Axis::X);
}

#[test]
fn a_horizontal_log_segment_rasterises_as_lying_logs() {
    // A hand-built TreeSkeleton: a short vertical trunk (level 0) and one thick level-1 limb along +x, radius
    // above MIN_WOOD_RADIUS, low enough to be TreePart::Log (below BRANCH_LEVEL).
    let blocks = rasterize(&sk);
    assert!(blocks.iter().any(|b| b.part == TreePart::Log && b.axis == Axis::X));
    assert!(blocks.iter().filter(|b| b.offset[1] == 0).all(|b| b.axis == Axis::Y), "the trunk base stands");
}
```

In `crates/game/tests/chop.rs`: `chopping_a_lying_trunk_log_fells_the_limb`. Same rig as the existing chop
tests, with the block at the cut replaced by its `_x` variant carrying the same `tree_id` and limb. Assert the
same falling set as the upright case.

- [ ] **Step 2: Run them to verify they fail**

Run: `just test-one worldgen "--lib trees::raster::"`; `just test-one game "--test it chop::"`
Expected: compile errors in worldgen. The chop test may already pass (Task 1 made variants trunk); keep it as
the regression.

- [ ] **Step 3: Implement**
  - `Grid` keeps the axis next to the wood limb. Whenever a segment sets a cell's limb, which is when it wins
    the lowest-limb rule, it also sets that cell's axis to `segment_axis(parent.pos, node.pos)`.
  - `TreeBlocks::from_names` resolves `<species>_log_x` / `_z`, falling back to `log` when the name resolves
    to `AIR`. A `single_block` species uses its one block for all five.
  - In `tree_blocks`, `TreePart::Log` picks `log`, `log_x` or `log_z` by `b.axis`. The chisel palette takes
    the same id.

- [ ] **Step 4: Run the tests, then the goldens**

Run: `just test-one worldgen "--lib trees::"`, then `just test-one game "--test it chop::"`, then
`just test-one worldgen "--test scene_hash_golden"` and `just determinism-wasm`.
Expected: the unit tests pass. A golden that pins generated chunks with lying logs now differs:
- Re-record it on purpose (at chunk 16) and say so in the commit.
- Check `crates/world-store/tests/fixtures/world_map.bin`. `build_world_map` does not read tree block ids, so
  `WORLD_MAP_VERSION` should not change. If the fixture test fails, bump it (CLAUDE.md rule) and re-record.

- [ ] **Step 5: Commit** `feat(worldgen): lying tree limbs are X/Z logs` (plus "goldens re-recorded" when they were).

### Task 6: mc-import keeps Minecraft's `axis`

**Files:**
- Modify: `crates/mc-import/src/table.rs` (`Table.oriented: BTreeSet<String>`, parsed from `materials.ron`),
  `crates/mc-import/data/materials.ron` (`oriented: [ … the ten base names … ]`),
  `crates/mc-import/src/classify.rs` (`classify`'s whole-block branch)
- Test: `crates/mc-import/src/classify.rs` `mod tests` (or the crate's existing classify tests)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn an_axis_log_maps_to_its_lying_variant() {
    let t = Table::builtin();
    let c = |s| classify(&BlockState::parse(s).unwrap(), t).class;
    assert_eq!(c("minecraft:oak_log[axis=x]"), Class::Block { block: "oak_log_x".into() });
    assert_eq!(c("minecraft:oak_log[axis=z]"), Class::Block { block: "oak_log_z".into() });
    assert_eq!(c("minecraft:oak_log[axis=y]"), Class::Block { block: "oak_log".into() });
    assert_eq!(c("minecraft:stone[axis=x]"), Class::Block { block: "stone".into() });
}
```

Also extend the existing "every target exists in blocks.ron" test so that it checks each `oriented` name's
`_x` / `_z` too.

- [ ] **Step 2: Run it to verify it fails**: `just test-one mc-import "classify::"` → FAIL (`oak_log`).
- [ ] **Step 3: Implement**: when the resolved whole-block target is in `table.oriented` and
  `state.props["axis"]` is `x` or `z`, append `_x` / `_z`.
- [ ] **Step 4: Run**: `just test-one mc-import "--lib"` → PASS.
- [ ] **Step 5: Commit** `feat(mc-import): Minecraft axis logs import as lying variants`.

### Task 7: Close out

- [ ] Run `just smoke` and `just test-one game "--test it -- block_props:: block_edit:: prefab_layout:: chop::"`.
  Expected: PASS.
- [ ] Hand check (owner, `just run --reset`): a fallen or placed log shows rings on its ends and bark grain
  along its length; the chisel tool on a lying log keeps the lying texture; a lying log far away still reads
  as bark.
- [ ] In the spec, mark §1 "Built" with the commit range. Commit only that path.
