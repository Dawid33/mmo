# Gameplay Plants Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Flowers, herbs, mushrooms, a fern and a berry bush grow in the world as real blocks. Players can pick them,
plant them and trample nothing yet. They are drawn as swaying cross-shaped sprites, not cubes.

**Architecture:**
- **Sim:** a plant is an ordinary sim block with a new manifest property `shape: Cross` (later `Model(name)`). It is
  non-solid, `replaceable`, nearly instant to break, and drops an item. The sim needs only small changes: a place
  aimed at a plant replaces it, a plant needs solid ground under it, and breaking that ground pops the plant.
- **Worldgen:** worldgen stamps plants into the air cell above suitable ground. The pass is a pure per-column hash
  roll with patch clumping and the tree system's habitat niches.
- **Client, mesher:** the chunk mesher treats a shaped block as empty (no faces, no AO, no culling). It reports
  each shaped block cell instead.
- **Client, drawing:** each sim chunk gets one small baked plant mesh (cross quads jittered by a world-block hash),
  drawn with an alpha-mask `ExtendedMaterial` whose vertex shader sways the tops. The material reuses the block
  texture array.
- **Why baked, not instanced:** gameplay plants are sparse, so a baked mesh per chunk is cheaper than the instanced
  pass the research proposed for dense visual cover. Dense visual cover is out of scope.
- **Unchanged:** the quad record, both terrain shaders, and the far LOD (a plant counts as air there).

**Tech Stack:** Rust; `game` (registry, block props, mining/placing), `worldgen`, Bevy 0.19 client (`ExtendedMaterial`,
WGSL), Pillow (`tools/textures/gen.py`).

**Spec:** `docs/superpowers/specs/2026-11-18-block-visuals-research.md` §3 (the "gameplay plants as sim blocks" half of
the recommended split; the visual-only half is out of scope by the owner's decision, 2026-11-18).

## Global Constraints

- Work on `develop` in this checkout; no worktrees. Commit only your own paths (`git diff --cached --name-only`, commit
  with a pathspec). Never run a formatter from the repo root or with `--all`.
- `game` and `worldgen` stay Bevy-free. Sim state stays deterministic: plants are plain `Block`s in chunks, so they are
  hashed and rollback-safe already; add no new sim structure.
- `BTreeMap`/`BTreeSet` for any ordered sim data; worldgen randomness only through `trees::placement`'s `hash01`-style
  stateless hashes; tunables as named consts.
- The quad record (`records.rs`), `voxel_pull.wgsl`, `voxel_pull_depth.wgsl`, `voxel_texture.wgsl` and `pack.rs` are NOT
  changed.
- WebGL2 must work: plant meshes use ordinary vertex attributes (no vertex-stage storage buffers).
- New per-session client entities hang under the chunk entity (already under the `SessionScoped` region root); add no
  free-standing resource or `Local` without `add_session_reset`/`SessionLocal` (`client/src/session.rs`).
- Wasm: anything a compute job returns is serde data (`MeshWire`); workers have no ECS.
- Art is original and generated deterministically by `just block-textures`; no paid generation (`just assets`).
- Worlds are re-forged, never migrated.
- Tests: `just test-one <crate> "<args>"` for touched tests only; client tests are a slow opt-in. Ask the owner once
  before the first client test run, and never launch the client or a benchmark without asking. `just smoke` is the
  routine check.
- Coordinate with `docs/superpowers/plans/2026-11-18-block-trees.md`: it edits the same tree-stamping loop in
  `worldgen/src/lib.rs` (removing `placed.chisel`). Rebase on it if it has landed; plants stamp AFTER trees either way.

## Review Focus

1. **A brick holding only a plant must still be meshed and shown.** Example: a flower in a chunk's bottom block row,
   with its ground in the chunk below. `bridge.rs` `is_all_empty` and `is_opaque_throughout` must treat a shaped block
   as present and see-through. Test in Task 5.
2. **Breaking the ground under a plant pops the plant (with its drop in survival); no plant floats.** Test in Task 3.
3. **Right-clicking a flower with a block selected replaces the flower in place.** It does not stack the block on
   the cell above. A flower aimed at a cell with nothing solid under it is refused and the item comes back. Test in
   Task 3.
4. **No plant ever appears as a cube in the far LOD or a coarse twin** (`game::lod::terrain_id` maps shaped to air).
   Test in Task 1.
5. **Worldgen never puts a plant on a slope, under water, on glacier ice, inside a building pad's box, or in a cell a
   tree filled.** Test in Task 4.

---

## Part A — Sim (`game`)

### Task 1: `shape` and `replaceable` in the registry and block props

**Files:**
- Modify: `crates/game/src/registry.rs` (`BlockDef`, `RegistryError`, `from_ron`, `props`, tests)
- Modify: `crates/game/src/block_props.rs` (`BlockProps` fields + accessors)
- Modify: `crates/game/src/lod.rs:52-54` (`terrain_id`)
- Test: `crates/game/src/registry.rs` tests, `crates/game/tests/block_props.rs`, `crates/game/src/lod.rs` tests

**Interfaces:**
- Produces:
  - `pub enum BlockShape { #[default] Cube, Cross, Model(String) }` (serde, `Clone, Debug, PartialEq, Eq, Default`), exported from the crate root.
  - `BlockDef.shape: BlockShape` (`#[serde(default)]`), `BlockDef.replaceable: bool` (`#[serde(default)]`).
  - `RegistryError::SolidShape(String)`: a non-`Cube` block that is `solid: true`.
  - `BlockProps::is_shaped(&self, BlockId) -> bool`, `BlockProps::shaped_ids(&self) -> impl Iterator<Item = BlockId> + '_`, `BlockProps::is_replaceable(&self, BlockId) -> bool` (backed by `pub(crate) shaped: BTreeSet<u16>` and `pub(crate) replaceable: BTreeSet<u16>`).
  - `terrain_id(id)` returns `AIR` for a shaped id as well as a liquid one.

- [ ] **Step 1: Write the failing tests.**
  - `registry::tests::parses_shape_and_replaceable`: a manifest with `(id:1,name:"bell",textures:All("b.png"),solid:false,shape:Cross,replaceable:true)` gives `def.shape == BlockShape::Cross`, `def.replaceable`. A block without the keys gives `Cube` and `false`.
  - `registry::tests::rejects_solid_shape`: the same block without `solid:false` gives `Err(RegistryError::SolidShape(n)) if n == "bell"`.
  - In `tests/block_props.rs`, `shaped_and_replaceable_props`: `props()` of that manifest has `is_shaped(bell)`, `is_replaceable(bell)`, `!is_solid(bell)`, `blocks_ray(bell)`. Dirt has none of the first two.
  - In `lod.rs` tests, `shaped_cells_are_air_in_the_lod`: with props installed from that manifest, `merge_group(&[bell; 8]) == Voxel::new(AIR)`. `merge_group` of 4 dirt bottoms with 4 bell tops has material dirt.
- [ ] **Step 2: Run** `just test-one game "--lib registry:: lod::shaped"` and `just test-one game "--test it block_props::"`. Expected: FAIL, the items don't exist yet.
- [ ] **Step 3: Implement.** Add the fields, validate `SolidShape` in `from_ron` after the air check, fill the two sets in `props()`, add the accessors, and extend `terrain_id` to `is_liquid(id) || is_shaped(id)`. If `BlockProps` derives serde for the wasm workers, the new fields follow it.
- [ ] **Step 4: Run the same commands.** Expected: PASS. Also `just test-one game "--test it lod_streaming::"`: still PASS.
- [ ] **Step 5: Commit** `feat(game): blocks can have a non-cube shape and be replaceable`.

### Task 2: The starter plants in the manifest, their art and their item

**Files:**
- Modify: `assets/blocks/blocks.ron` (ids 188–194; 168–187 are reserved for the oriented-block plan’s `_x`/`_z` variants)
- Modify: `tools/textures/gen.py` (a `plants()` section, run by default)
- Create: `assets/blocks/{meadowbell,sunpetal,emberwort,silverleaf,loamcap,bracken,thornberry_bush}.png` (generated)
- Modify: `crates/game/src/items.rs:13` (`EXTRA_ITEMS` gains `"thornberries"` at the END; ids of earlier extras stay)
- Modify: `crates/client/src/renderer/mod.rs:373` (`EMBEDDED_BLOCK_TEXTURES` entries for the 7 PNGs)
- Test: `crates/game/src/registry.rs` `parses_shipped_manifest`, `crates/game/tests/items.rs`

**Interfaces:**
- Produces: block names `meadowbell`, `sunpetal`, `emberwort`, `silverleaf`, `loamcap`, `bracken`,
  `thornberry_bush`. Each is `textures: All("<name>.png"), solid: false, shape: Cross, replaceable: true, hardness: 5`.
  Drops: `Same` for all but `thornberry_bush`, which has `drop: Item("thornberries")`. Extra item `"thornberries"`.

- [ ] **Step 1: Write the failing tests.**
  - Extend `parses_shipped_manifest`: every name above resolves and has `shape == BlockShape::Cross`.
  - In `tests/items.rs`, `thornberry_bush_drops_thornberries`: after `install_real_block_props()`, `drop_of(Block::new(bush))` is one `ItemId::extra_named("thornberries")`, and `drop_of(meadowbell)` is one meadowbell block item.
- [ ] **Step 2: Run** `just test-one game "--lib registry::"` and `just test-one game "--test it items::"`. Expected: FAIL.
- [ ] **Step 3: Add the manifest rows, the extra item and the art.**
  - `gen.py` `plants()` draws each sprite on a 16×16 transparent canvas, deterministically from `h()`: a stem, then a
    head of 3–5 ramp colours. Each sprite gets its own palette and silhouette. Bell heads for meadowbell, a disc for
    sunpetal, a red spike for emberwort, grey-green leaves for silverleaf, a cap on a stalk for loamcap, a frond fan
    for bracken, and a round leaf mass with red dots for thornberry_bush.
  - Alpha is 0 or 255 only (the mip chain is alpha-weighted).
  - Keep the bottom row of the stem at y = 15 so the sprite stands on the ground.
  - Run `just block-textures`. Check the contact sheet with `just block-textures --sheet`, then look at `target/textures-sheet.png`.
- [ ] **Step 4: Run the same commands.** Expected: PASS. If the client has a test that every manifest texture is embedded, run it once in Task 7's client batch.
- [ ] **Step 5: Commit** `feat(assets): seven starter plants and the thornberries item` (the manifest, gen.py, PNGs, items.rs, mod.rs).

### Task 3: Placing replaces a plant, a plant needs ground, and breaking the ground pops it

**Files:**
- Modify: `crates/game/src/mining.rs` (`survival_place_safe` :207, `mine_block_safe` :190)
- Modify: `crates/game/src/intents.rs:461-476` (`BreakBlock` edit branch, `PlaceBlock` apply)
- Modify: `crates/game/src/edit.rs:~702` (the editor place uses the same target rule)
- Test: `crates/game/tests/mining.rs`, `crates/game/tests/pathfind.rs`

**Interfaces:**
- Consumes: `BlockProps::{is_shaped, is_replaceable}` (Task 1), plant names (Task 2).
- Produces (crate-private, in `mining.rs`):
  - `fn place_target(&self, hit: &EyeHit) -> [i32; 3]`: `hit.block` when that block is replaceable, else `hit.block + hit.normal`. `EyeHit` is whatever `eye_hit` returns.
  - `fn pop_plant_above(&mut self, pos: BlockPos, tick: u64, survival: bool)`: if the block at `pos + Y` is shaped, it is mined (survival: through `mine_block_safe`, so it drops) or set to air (editor).
  - `PlaceBlock` rules:
    - It is refused with `IntentRefusal::Occupied` unless the target cell is air, liquid or replaceable, and no body overlaps it.
    - A shaped block is refused with `IntentRefusal::BadTarget` unless the block under the target is solid.
    - Replacing a plant destroys it without a drop, as leaves are overwritten today.

- [ ] **Step 1: Write the failing tests** in `tests/mining.rs`, using its `region`/`set_block`/`target`/`mouse`/`ticks`/`piles`/`set_slot` helpers and `install_real_block_props()`.
  - `placing_on_a_flower_replaces_it`: put meadowbell at the targeted cell, select dirt, right-click, run the ticks. `block(target) == dirt` and the cell above is air.
  - `a_flower_needs_ground`: select meadowbell and aim at a wall face with air under the target cell. Right-click and run the ticks: the target stays air and slot 0 still holds the meadowbell (the refusal returns it).
  - `breaking_the_ground_pops_the_plant`: meadowbell on the targeted dirt. Hold left until the dirt breaks: both cells are air, and `piles()` holds one dirt and one meadowbell.
  - `a_flower_breaks_at_once`: `ticks_to_break(meadowbell) <= Some(2)`.
  - In `tests/pathfind.rs`, `walks_through_a_flower`: the existing straight-corridor setup with meadowbell in a corridor cell still finds the same path length.
- [ ] **Step 2: Run** `just test-one game "--test it mining:: pathfind::walks_through"`. Expected: the first three FAIL; `a_flower_breaks_at_once` and the pathfind test may already pass (non-solid is already passable). Keep them as guards.
- [ ] **Step 3: Implement.**
  - `place_target` is used by `survival_place_safe` and the editor place.
  - `PlaceBlock`'s apply checks the cell rule and the support rule above.
  - `pop_plant_above` is called after a successful `mine_block_safe` set-to-air and after the editor `BreakBlock` set-to-air.
  - Regions are vertical columns, so `pos + Y` is always this region's cell. A missing cell (top of the world) pops nothing.
- [ ] **Step 4: Run the same command.** Expected: PASS. Then run the whole `mining::`, `intents::` and `block_edit::` modules once: `just test-one game "--test it mining:: intents:: block_edit::"`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(game): a place replaces a plant, a plant needs ground, and breaking its ground pops it`.

## Part B — Worldgen

### Task 4: A plant pass

**Files:**
- Create: `crates/worldgen/src/plants.rs`
- Modify: `crates/worldgen/src/lib.rs` (`pub mod plants;`; the per-column bounds loop near :855-871; stamping after the tree loop :970-981)
- Modify: `crates/worldgen/src/material.rs` (`Materials.plants: PlantBlocks`, resolved by name like `TreeBlocks`)
- Modify: `crates/worldgen/src/trees/placement.rs:99-110` (`hash01` becomes `pub(crate)`)
- Test: `crates/worldgen/src/plants.rs` tests; `crates/worldgen/src/lib.rs` tests

**Interfaces:**
- Consumes: `trees::species::Niche` (`Niche::response(v) -> f32`), `habitat::{Habitat, HabitatSource}`, the plant block names (Task 2).
- Produces:
  - `pub struct PlantKind { pub name: &'static str, pub ground: &'static [&'static str], pub temperature_c: Niche, pub moisture: Niche, pub weight: f32, pub fill: f32 }`
  - `pub const PLANTS: &[PlantKind]`: the 7 kinds, values below.
  - `pub struct PlantBlocks { kinds: Vec<(BlockId, Vec<BlockId>)> }` (block, ground ids, in `PLANTS` order), with `PlantBlocks::NONE`, `PlantBlocks::enabled()` and `PlantBlocks::resolve(&BlockRegistry) -> PlantBlocks` (any missing name → `NONE`, as `TreeBlocks` does).
  - `pub const PLANT_PATCH_BLOCKS: i32 = 4;` `pub const PLANT_MAX_FILL: f64 = 0.45;`
  - `pub fn plant_at(seed: u64, x: i32, z: i32, ground: BlockId, habitat: &dyn Fn() -> Habitat, blocks: &PlantBlocks) -> Option<BlockId>`. The habitat is lazy: it is called only after the ground and fill pre-checks pass.

Algorithm (not determined by the signature):

```text
f = hash01(seed, x, z, SALT_PLANT_FILL)
if f >= PLANT_MAX_FILL or no kind lists `ground`: None
h = habitat()
w_i = kind.weight * kind.temperature_c.response(h.temperature_c) * kind.moisture.response(h.moisture)
      for kinds listing `ground`, else 0
r = hash01(seed, x.div_euclid(PLANT_PATCH_BLOCKS), z.div_euclid(PLANT_PATCH_BLOCKS), SALT_PLANT_KIND)
pick the first i with r < w_0 + … + w_i      (r beyond the sum: None — the patch is bare)
Some(block_i) if f < kind_i.fill else None
```

The roll shared by a patch makes clumps, and the per-block fill thins them. The result depends only on `(x, z)`,
the column's own ground and its habitat, so the pass needs no neighbour reads and no seam rule. Starting values,
tuned by looking with `just run` (ask first):

| name | ground | temperature_c Niche | moisture Niche | weight | fill |
|---|---|---|---|---|---|
| meadowbell | grass | (2, 8, 18, 24) | (0.6, 1.0, 2.0, 3.0) | 0.10 | 0.35 |
| sunpetal | grass, dry_grass | (8, 14, 26, 32) | (0.3, 0.6, 1.4, 2.2) | 0.08 | 0.30 |
| emberwort | dry_grass | (14, 20, 32, 38) | (0.1, 0.3, 0.8, 1.4) | 0.06 | 0.25 |
| silverleaf | grass, podzol | (-6, 0, 10, 16) | (0.4, 0.8, 2.0, 3.0) | 0.06 | 0.30 |
| loamcap | podzol, grass | (0, 6, 16, 22) | (1.2, 1.8, 2.5, 3.5) | 0.05 | 0.20 |
| bracken | grass, podzol | (2, 8, 20, 26) | (1.0, 1.4, 2.5, 3.5) | 0.12 | 0.45 |
| thornberry_bush | grass | (4, 10, 22, 28) | (0.6, 1.0, 2.0, 2.8) | 0.03 | 0.15 |

- [ ] **Step 1: Write the failing tests** in `plants.rs`, using a test `PlantBlocks` built from fixed ids.
  - `same_inputs_same_plant`: two calls with identical arguments are equal, across 10k coordinates.
  - `wrong_ground_grows_nothing`: stone ground gives `None` for every coordinate in a 64×64 window.
  - `outside_every_niche_grows_nothing`: with `temperature_c = 60`, `None` everywhere.
  - `a_temperate_meadow_has_some_plants`: grass, 12 °C, moisture 1.2 over 256×256 gives a plant fraction in `0.02..0.30`, and at least 2 kinds appear.
  - `the_habitat_is_asked_only_after_the_cheap_rolls`: a habitat closure that counts its calls is called fewer than `PLANT_MAX_FILL × n + 1` times.
- [ ] **Step 2: Write the failing generation test** `plants_stand_only_on_open_dry_ground` in the `lib.rs` test module. Use the shipped manifest and the existing forged/procedural region fixture used by the tree tests (grep `trees_in` in `lib.rs` tests). For every plant block in the generated region, assert:
  - the block under it is in its kind's ground list;
  - its column is no slope (`slope_for` is none) and not in a pad's box;
  - it sits above `water_top` and the column's `top == s`;
  - no tree block was overwritten (generate once with `PlantBlocks::NONE` and compare: every non-plant cell is equal).
- [ ] **Step 3: Run** `just test-one worldgen "--lib plants:: plants_stand"`. Expected: FAIL (module missing).
- [ ] **Step 4: Implement.**
  - **Materials:** `Materials.plants` comes from the registry, like `trees`. `with_plants = mats.plants.enabled()`.
  - **Choosing:** in the per-column bounds loop, for each `(lx, lz)` not in a pad's box, with no slope, `c.top == c.s` and `c.s > c.water_top`, call `plant_at(trees_from.seed, bx, bz, column_material(&mats, &c, c.s), &|| trees_from.habitat.habitat(block_center_m(bx), block_center_m(bz), c.s), &mats.plants)`. Collect `(lx, lz, c.s + 1, block)` into `col_plants`, and fold `hi = hi.max(c.s + 1)` the way the trees fold theirs.
  - **Stamping:** after the tree stamping loop, write each plant only where the cell is still `AIR`.
  - **Skip:** the coarse LOD path (`:1391-1430`) never adds plants.
- [ ] **Step 5: Run** the same command. Expected: PASS. Then run the whole worldgen suite once (`just test-one worldgen ""`), plus `just determinism-wasm`.
  - Re-record a terrain golden only if it uses the shipped manifest and its diff is plants alone. Say so in the commit.
  - `WORLD_MAP_VERSION` stays (`build_world_map` is unchanged).
- [ ] **Step 6: Commit** `feat(worldgen): plants grow on open ground by habitat, in clumps`.

## Part C — Client

### Task 5: The mesher skips shaped blocks and reports their cells

**Files:**
- Modify: `crates/client/src/renderer/meshing/binary.rs:28-82` (`CutoutTable` gains a third bitset)
- Create: `crates/client/src/renderer/meshing/plants.rs` (`PlantCell`, `plant_cells`)
- Modify: `crates/client/src/renderer/meshing/mod.rs` (`MeshOutput`/`MeshWire.plants`, `mesh_chunk_classified_wire`, `requires_upload`, `apply_meshed_chunks` sets `BrickPlants`)
- Modify: `crates/client/src/renderer/bridge.rs:300-310` (`is_all_empty`, `is_opaque_throughout`)
- Modify: `crates/client/src/renderer/meshing/chunk_mesh.rs` (the `BrickPlants` component, beside `BrickRecords`)
- Test: `meshing/binary.rs` tests, `meshing/plants.rs` tests, `bridge.rs` tests

**Interfaces:**
- Consumes: `BlockProps::shaped_ids()` (Task 1).
- Produces:
  - `CutoutTable::with_shaped(self, ids) -> Self`, `CutoutTable::is_shaped(&self, BlockId) -> bool`. `from_props` includes the shaped ids. `is_empty(v)` = air ∨ liquid ∨ shaped, so a shaped block has no faces, hides no face, darkens no AO and is open for connectivity. `NONE` is unchanged.
  - `bridge::VoxelData::is_all_empty` still counts a shaped block as present. `is_opaque_throughout` is false when any shaped block is present.
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)] pub struct PlantCell { pub cell: [u8; 3], pub block: BlockId }`: `cell` is the block inside the brick, each axis `0..BRICK_BLOCKS`.
  - `pub fn plant_cells(p: &PaddedVoxels, cutout: &CutoutTable) -> Vec<PlantCell>`: reads the voxel at each block cell's minimum corner (padded index `1 + BLOCK_VOXELS * k`). The order is (y, z, x) ascending.
  - `MeshWire.plants: Vec<PlantCell>` and `MeshOutput.plants: Vec<PlantCell>`; `requires_upload` is also true for non-empty plants.
  - `#[derive(Component, Clone, Debug)] pub struct BrickPlants(pub Arc<Vec<PlantCell>>)`. It is inserted on a meshed brick with plants, removed when none, and marks its chunk `ChunkMeshDirty` exactly as `BrickRecords` does.

- [ ] **Step 1: Write the failing tests** (client, `--lib`).
  - `binary::tests::a_plant_has_no_faces_and_hides_none`: padded grid of a dirt block under a meadowbell block. `mesh_quads` gives exactly the dirt block's 6 faces, its top AO is unoccluded, and no quad has the plant's id.
  - `plants::tests::reports_each_plant_cell_once`: two plants in one brick give two `PlantCell`s with the right cells and ids. An empty brick gives none.
  - `bridge::tests::a_brick_of_only_a_plant_is_not_empty` (Review Focus 1): `!is_all_empty` and `!is_opaque_throughout`.
- [ ] **Step 2: Ask the owner once** before the first client test run. Then run `just test-one client "--lib meshing::binary::tests::a_plant meshing::plants:: bridge::tests::a_brick_of_only"`. Expected: FAIL.
- [ ] **Step 3: Implement** the interfaces above; `mesh_chunk_classified_wire` fills `plants` from `plant_cells`.
- [ ] **Step 4: Run** the same command, plus `just test-one client "--lib meshing::"` once. Expected: PASS. Then `just smoke`.
- [ ] **Step 5: Commit** `feat(client): the mesher skips shaped blocks and reports their cells`.

### Task 6: The baked plant mesh

**Files:**
- Create: `crates/client/src/renderer/decoration/mod.rs`, `crates/client/src/renderer/decoration/plant_mesh.rs`
- Modify: `crates/client/src/renderer/mod.rs` (`mod decoration;`)
- Test: `decoration/plant_mesh.rs` tests

**Interfaces:**
- Consumes: `PlantCell` (Task 5), `BlockTextureLayers`. A cross plant uses its block’s side layer: `[top, side, bottom][1]` today. If the per-axis plan (`FaceLayers`) has landed, use its +X face (all four sides are equal for an `All` texture).
- Produces:
  - `pub fn plant_hash(b: [i32; 3]) -> u64`: splitmix64's finaliser over `x·0x9E3779B97F4A7C15 ^ y·0xC2B2AE3D27D4EB4F ^ z·0x165667B19E3779F9` (as u64).
  - `pub struct PlantJitter { pub offset: [f32; 2], pub yaw: f32, pub scale: f32 }` and `pub fn plant_jitter(world_block: [i32; 3]) -> PlantJitter`:
    - `offset` uses bits 0–15 and 16–31 mapped to `±PLANT_JITTER_VOXELS`;
    - `yaw` uses bits 32–47 mapped to `[0, π/2)`;
    - `scale` uses bits 48–63 mapped to `[PLANT_SCALE_MIN, PLANT_SCALE_MAX]`.
  - Consts: `PLANT_JITTER_VOXELS: f32 = 3.0`, `PLANT_SCALE_MIN: f32 = 0.8`, `PLANT_SCALE_MAX: f32 = 1.1`.
  - `pub struct PlantMeshData { pub positions: Vec<[f32; 3]>, pub normals: Vec<[f32; 3]>, pub uvs: Vec<[f32; 2]>, pub layers: Vec<u32>, pub sway: Vec<f32>, pub indices: Vec<u32> }` with `into_mesh(self) -> Mesh`, which inserts `ATTRIBUTE_PLANT_LAYER` / `ATTRIBUTE_PLANT_SWAY` (Task 7).
  - `pub fn build_plant_mesh(parts: &[([u8; 3], &[PlantCell])], layers: &BlockTextureLayers, chunk_block_origin: [i32; 3]) -> Option<PlantMeshData>`:
    - `parts` are a chunk's bricks (brick local position, cells).
    - Positions are chunk-local voxel units.
    - Each plant is two vertical quads through the block's centre, at `yaw` and `yaw + π/2`, `BLOCK_VOXELS × scale` wide and tall, moved by `offset`, and standing on the cell's floor.
    - UV v = 0 at the top. The normal is +Y for every vertex, so plants light like the ground.
    - `sway` is 1 on top vertices and 0 on bottom ones.
    - `layer` is the block's side layer.
    - It returns `None` when there are no cells.
  - `pub fn chunk_block_origin(chunk_world_translation: Vec3) -> [i32; 3]`: `round(t / BLOCK_VOXELS)` per axis, the same rounding as `bricks::brick_grid_key`.

- [ ] **Step 1: Write the failing tests.**
  - `jitter_is_stable_and_bounded`: the same block gives equal jitter. Across 10k blocks, offsets lie in ±3 and scales in [0.8, 1.1]. Neighbouring blocks differ.
  - `one_plant_is_two_quads`: 8 vertices and 12 indices. The bottom vertices' y is the cell floor (`brick·32 + cell·16`), and the top vertices' y is `floor + 16·scale`.
  - `a_cell_lands_at_its_brick_and_cell`: a plant at brick `[1,0,1]`, cell `[0,1,1]` has its centre within `PLANT_JITTER_VOXELS·√2` of `(32+8, 16, 32+16+8)`.
  - `no_cells_no_mesh`: `None`.
  - `layer_is_the_side_layer`: with the table `{bell: [3, 4, 5]}`, every vertex layer is 4.
- [ ] **Step 2: Run** `just test-one client "--lib decoration::"`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same command. Expected: PASS.
- [ ] **Step 5: Commit** `feat(client): bake a sim chunk's plants into one jittered cross-quad mesh`.

### Task 7: The plant material, the shader and the chunk rebuild

**Files:**
- Create: `crates/client/src/renderer/decoration/material.rs`, `assets/shaders/plant.wgsl`
- Modify: `crates/client/src/renderer/mod.rs` (plugin registration; create `PlantMaterialHandle` beside `ChunkMaterial` at :1165-1195, with the same array image handle)
- Modify: `crates/client/src/renderer/meshing/chunk_mesh.rs:100-170` (`rebuild_chunk_meshes`)
- Modify: `crates/client/src/renderer/voxel_material.rs:94` (the cutaway sync also updates the plant material)
- Modify: `CLAUDE.md` (Client section, one line: plants are shaped blocks drawn by `renderer/decoration/`)

**Interfaces:**
- Consumes: `build_plant_mesh`, `chunk_block_origin` (Task 6), `BrickPlants` (Task 5), `voxel_pull::BlockArrayTexture`, `FortressViewUniform`.
- Produces:
  - `pub const ATTRIBUTE_PLANT_LAYER: MeshVertexAttribute` (Uint32, location 8) and `ATTRIBUTE_PLANT_SWAY` (Float32, location 9).
  - `#[derive(Asset, AsBindGroup, …)] pub struct PlantMaterial { #[texture(100, dimension = "2d_array")] #[sampler(101)] pub block_texture: Handle<Image>, #[uniform(102)] pub fortress: FortressViewUniform }`.
  - `#[derive(Resource)] pub struct PlantMaterialHandle(pub Handle<ExtendedMaterial<StandardMaterial, PlantMaterial>>)`: base `perceptual_roughness: 0.9`, `alpha_mode: AlphaMode::Mask(0.5)`, `double_sided: true`, `cull_mode: None`.
  - `pub const PLANT_SHADER: Handle<Shader>` (a new uuid), embedded with `load_internal_asset!` as `VOXEL_TEXTURE_SHADER` is, for the same browser-cache reason.
  - `#[derive(Component)] pub struct ChunkPlants(pub Entity)` on a chunk, pointing at its child plant entity, which carries the `PlantMesh` marker.
  - `PLANT_VIEW_BLOCKS: f32 = 96.0`.

Shader (`plant.wgsl`), one file serving the forward and prepass vertex stages so both displace identically:
- **Vertex:** world position from the mesh transform, then `+= sway · WIND_AMPLITUDE_VOXELS (1.5) · (sin(globals.time · 2π / WIND_PERIOD_S (3.0) + phase), 0, cos(…) · 0.5)`, where `phase = dot(world.xz, vec2(0.013, 0.021))`.
- **Fragment:** `textureSample(block_texture, sampler, uv, layer)`, alpha discard below 0.5, then Bevy's PBR lighting. Apply the z-level cut with the same test and uniform layout as `voxel_texture.wgsl`'s fortress branch.
- **Shadows:** the plant entity is `NotShadowCaster` in this slice.

- [ ] **Step 1: Integrate into `rebuild_chunk_meshes`.**
  - For each settled chunk, gather `(local, BrickPlants)` like the records.
  - Build `build_plant_mesh(…, chunk_block_origin(global.translation()))`.
  - Despawn the old `ChunkPlants` child and spawn a new one under the chunk: `Mesh3d`, `MeshMaterial3d(plant handle)`, `Transform::IDENTITY`, `NotShadowCaster`, `VisibilityRange` ending at `PLANT_VIEW_BLOCKS · BLOCK_VOXELS`, a constant `chunk_aabb()` and `NoAutoAabb`.
  - Do this BEFORE the `merged.is_empty()` early `continue`, so a chunk of only plants still gets them. It shares the same `uploads` increment.
  - Register the material plugin and the shader.
- [ ] **Step 2: Run** `just smoke`, then `just client-wasm`. Also run the web build check the justfile uses for WGSL on WebGL2: grep the `web` / `slint-wasm` recipes and use the one that compiles the client for wasm32. Expected: both build.
- [ ] **Step 3: Run the client batch once** (owner already asked in Task 5): `just test-one client "--lib meshing:: decoration:: block_textures::"`. If there is an embedded-texture test (grep `EMBEDDED_BLOCK_TEXTURES` in tests), run it too. Expected: PASS.
- [ ] **Step 4: Ask the owner for a hand check.** Have them run `just run --reset` (a re-forged debug world) and check:
  - plants stand on meadows and sway;
  - picking and placing works;
  - plants disappear past about 96 m;
  - the fortress cutaway hides plants above the cut;
  - the web build draws them (`just web`).
  Record what they report in the commit message of Task 8 or in `TODO.md`.
- [ ] **Step 5: Commit** `feat(client): plants are drawn as swaying cross sprites, one baked mesh per sim chunk` (the shader, material, chunk_mesh, mod.rs, voxel_material.rs, CLAUDE.md).

## Part D — Model-shaped plants (later; only after Parts A–C have shipped and the owner wants bushes as models)

### Task 8: `shape: Model(name)` baked from Blockbench

**Files:**
- Create: `assets/models/plants/<name>.bbmodel` (hand-authored in Blockbench at 16 px/m, ≤ 100 triangles, ONE 16×16 texture whose file is also the block's `All("<name>.png")`)
- Modify: `crates/client/src/renderer/decoration/plant_mesh.rs` (the model branch), `decoration/mod.rs` (a `PlantModels` resource: name → triangles)
- Test: `decoration/plant_mesh.rs` tests

**Interfaces:**
- Consumes: `crates/bbmodel` (parse cubes into triangles with 16-px UVs; check its public API for elements and faces), `BlockShape::Model`.
- Produces:
  - `pub struct PlantModel { pub positions: Vec<[f32; 3]>, pub uvs: Vec<[f32; 2]>, pub indices: Vec<u32> }`, in block-local voxel units, UVs 0..1 over the block's texture layer.
  - `PlantModels(BTreeMap<String, PlantModel>)` resource, loaded at startup (wasm: embedded bytes, like the block textures).
  - `build_plant_mesh` gains `models: &PlantModels`. A `Model(name)` cell bakes that model rotated by `yaw` and scaled by `scale`; `sway` is the vertex height fraction.
  - Model shapes still reach the mesher as shaped blocks, so Part C needs no change.

- [ ] **Step 1: Write the failing tests** `a_model_plant_bakes_its_triangles` (vertex count equals the model's; every vertex lies inside the cell ± jitter) and `an_unknown_model_falls_back_to_a_cross` (a missing name logs once and draws a cross).
- [ ] **Step 2: Run** `just test-one client "--lib decoration::"`. Expected: FAIL.
- [ ] **Step 3: Implement.** Switch `thornberry_bush` to `shape: Model("thornberry_bush")`.
- [ ] **Step 4: Run** the same command and `just smoke`. Expected: PASS. Ask the owner to look.
- [ ] **Step 5: Commit** `feat(client): model-shaped plants baked from Blockbench`.

## Follow-ups (not in this plan)

- Trampling, regrowth, berries regrowing on a bush, eating thornberries (no food system yet).
- Shade-loving plants near trees: a niche on the distance to the nearest trunk from `trees_in`.
- Plant shadows (a CUTOUT shadow variant with the sway displacement).
- `mc-import`: map Minecraft flowers to these blocks instead of dropping `plant` blocks (`mc-import/src/table.rs:17`).
- Visual-only cover (tufts, pebbles): research §3, deliberately out of scope.
