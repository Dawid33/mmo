# Static Water Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Water stops being a solid block. The sea, forged lakes and rivers become implied recipe
water held outside chunk block data. Bodies, people and slimes swim and float in it. It is drawn
by its own blended pass, near and in the LOD rings. Nothing flows yet.

**Architecture:**
- Water gets a registry kind, `Liquid`, so it has no collider and no ground or ray role.
- Recipe water is one `water_top` per block column, stored in the `ColumnGround` recipe that
  already reaches every client in the region snapshot.
- A cell holds water iff:
  - its column has recipe water;
  - `surface < y <= water_top`;
  - the cell holds no block (chunk air, or an unmaterialised air slot).
- Genesis labels recipe water into bodies (`{ level, volume }`), stored as rollback state that
  this plan never mutates.
- The single sim query `Rollback::water_units_at` (own ∪ halo) feeds:
  - swimming (`movement::plan_with`);
  - buoyancy (dynamic bodies);
  - DF depth rules (pathfinding).
- The client meshes water per tile from the same recipe. It draws water as a blended
  `ExtendedMaterial<StandardMaterial, WaterExtension>`, and the LOD rings get a per-key water
  surface built from the recipe, outside the terrain fingerprint.

**Tech Stack:** Rust; the `game`, `worldgen`, `server` and `client` crates; Bevy 0.19
`MaterialPlugin`/`Transparent3d`; rapier (vendored fork, untouched); `just` recipes.

**Spec:** `docs/superpowers/specs/2026-11-06-water-revisit/00-synthesis.md` (design) and
`02-engine-today.md` (engine audit). Also:
- `docs/superpowers/plans/2026-11-06-water-engine-plan-handoff.md`: scope and binding
  decisions;
- `docs/superpowers/specs/2026-09-22-voxel-water-efficiency/03-networking-rendering.md` §B: water
  render pass and LOD water;
- `docs/superpowers/specs/2026-10-20-dwarf-fortress-research/08-fluids-and-environment.md`: DF
  depth rules.

**Builds on:** the seam-transparent regions program
(`docs/superpowers/plans/2026-11-06-seam-transparent-regions.md`), which has landed in full
(develop up to `1879620`). The names in this plan are the landed ones, checked against the code
on 2026-11-07; "Fit with seam-transparent regions" below lists every touch point. Never add a
seam special case to work around something here.

**Order and why:** every task leaves `develop` playable.
1. The liquid kind (Task 1), the query and depth rules (Task 2), swimming (Task 3) and buoyancy
   (Task 4) land while water is still block 5 in chunks; the query reads the block.
2. Task 5 adds the recipe *beside* the blocks.
3. Task 6 moves water drawing to the new pass.
4. Task 8 adds LOD water.
5. Only Task 9 stops writing water blocks.

## Global Constraints

- Work on `develop` in the main checkout. **No git worktrees** (CLAUDE.md).
- Stage only the paths a task names, never `git add -A`; Dawid has concurrent uncommitted edits.
- End every commit message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **Testing:**
  - Verify with `just test-one <crate> "<target> <filter>"` and `just smoke` (60 s limit). Never
    run whole suites.
  - Tag any test over 100 ms `#[ignore = "slow: >100ms — run via just test-full"]` and run it
    with `-- --include-ignored`.
  - **Client unit tests are a slow opt-in: ask Dawid before running
    `just test-one client "--lib …"`.** Until he says yes, rely on `just smoke`'s client
    type-check and mark the step "pending Dawid".
- **Determinism:**
  - Water state is integer-only, `BTreeMap`-only and `StateHash`-derived (never std `Hash` or
    bincode bytes).
  - No `f32` transcendentals in sim code (use `libm::` if one is ever needed).
  - Run `just determinism-wasm` after every task that changes hashed or serialized sim state
    (Tasks 1, 3, 4, 5, 9).
- **Hashing:** never touch the server's state-hash interval (`DEFAULT_STATE_HASH_INTERVAL`).
- **Sizes and dials:**
  - Never write region, chunk or brick sizes as literals. Derive them from `REGION_BLOCKS`,
    `CHUNK_BLOCKS`, `CHUNK_UNITS`, `BLOCK_VOXELS` and `UNITS_PER_METER`.
  - Tests build at region dial 64; play recipes build at 256.
- **Tunables** are named `pub const`s with a doc line (Dawid's preference). New ones:
  - `WATER_FULL: u16 = 4096`: units in one full cell, matching the prototype's `FULL`.
  - `DF_DEPTH_LEVELS: u8 = 7`, `WADE_MAX_DEPTH: u8 = 3`, `SWIM_MIN_DEPTH: u8 = 4`,
    `DROWN_DEPTH: u8 = 7`.
  - `WADE_COST: u32 = 10`.
  - `NO_WATER: i16 = i16::MIN`, `NO_BODY: u32 = u32::MAX`.
  - `WATER_TILE_BLOCKS: i32 = 64`. It must divide every `REGION_BLOCKS` choice; const-assert
    it.
  - Swimming constants, named in Task 3.
  - Buoyancy constants, named in Task 4.
  - Render constants, named in Task 6.
- **New per-world client resources, `Local`s and entity roots** must be registered:
  `add_session_reset::<R>()`, `SessionLocal`, `SessionScoped`.
- **Shaders:** compile new shaders in with `load_internal_asset!` (no hot reload). Do not change
  the `QuadRecord` layout or `voxel_pull*.wgsl` decoding.
- **Water stays out of:**
  - colliders;
  - `SetBlocks`;
  - `terrain_fingerprint` / `TerrainSnapshot`;
  - the chunk mesh, which treats liquid as empty once Task 6 lands.
- **Ice stays a solid block.** `ice` (id 23) and `glacier_ice` (id 24) are unchanged.
- Keep the `justfile` in sync. No new workflow is expected; if one appears, add a recipe.
- **Decisions this plan assumes; ask Dawid at handoff, nothing below depends on them:**
  - depth shown as DF 1–7 over `u16` (synthesis decision 3, recommended);
  - decisions 1 (ledger as the water cycle) and 2 (no client-predicted seam arrivals) only
    affect the later flow plans.

- **Seam rules** (CLAUDE.md, "Seam-transparent regions"):
  - Every new gameplay test that is not region-internal by nature uses `assert_seam_invariant`
    (`crates/game/tests/common/seam_translate.rs`). It compares discrete state exactly and
    continuous state within `LOCKSTEP_TOLERANCES` / `JITTER_TOLERANCES`.
  - Seam tests go in `crates/game/tests/handoff_schedule.rs` (in the `it` binary, run as
    `just test-one game "--test it -- --include-ignored handoff_schedule::<name>"`) or
    `crates/server/tests/threaded_world.rs`.
  - **Every new seam test name is added to the justfile's `seam_tests` list** (or matched by
    one of its filters, e.g. `handoff_schedule::`, `halo::`, `across_seam`), so
    `just test-threads` runs it.
  - A test whose outcome reads block props calls `common::install_real_block_props()` first.
    Every water test does, because liquid is a block prop.
  - Run `just test-threads` (~75 s warm) after every task that touches movement, bodies, the
    halo or region genesis (Tasks 1–5, 9).
  - No system may grow a seam special case (an edge clamp, a leash, an "outside the region"
    branch). Reads go through own ∪ halo, and writes to what a region does not own go through
    `Effects` intents.
- **Region file version:** this plan bumps `REGION_FILE_VERSION` (`crates/server/src/region_store.rs`)
  twice (Tasks 5 and 9). Each bump is **+1 from the value in the tree at that moment** (21 on
  2026-11-07). Never write a literal target value.

### Fit with seam-transparent regions

What the landed seam code provides, and what this plan does with it:

| Landed seam code | Water plan |
|---|---|
| **Halo** (`halo.rs`, `HALO_BLOCKS` = 8): neighbour chunks within the band, as fixed colliders built through `crate::collider_cells`; neighbour `ColumnGround` recipes in `GameData::halo_ground`, seeded through `HaloUpdate.ground`; `Rollback::halo_block(WorldBlockPos)` reads own ∪ halo, implied ground included. | Recipe water lives in `ColumnGround`, so `halo_ground` carries it with no new plumbing. Task 5 tests that it does. Halo colliders skip liquid automatically once `is_solid(water)` is false (Task 1; tested). The water query reads cells through `halo_block` (Task 2). |
| **`WorldView`** (`world_view.rs`; `Rollback::view() -> RegionView`): `block`, `entity`, `entities_in`, `raycast` (via `raycast_block_in` → `block_solid` → `cell_solid`), `pile`, `container`. | Task 1's change to `cell_solid` / `block_solid` makes the raycast pass through water. Task 2 adds `fn water_units(&self, p: WorldBlockPos) -> u16`. |
| **Migrated rules**: mining's `mineable` / `mineable_seen` (`mining.rs`), `pile_support` / `pile_cell_seen` (`item_piles.rs`, keyed by `WorldBlockPos`), `pathfind.rs`'s `passable_block` / `solid_block` / `is_water` (used by `PathPlan`'s fine A*), the slime spawn check (`slime.rs` `SLIME_NO_SPAWN_BLOCK`), restore (`player.rs`). | Task 1 replaces every `is_water` / `BlockId(5)` there with `is_liquid`. Task 2 puts the depth rules into `passable_block` / `standable`, which `PathPlan` and the client's preview both use. |
| **Bundle** (`migrate.rs` `Carried.motion`; `Ecs` / `Client` destructured exhaustively). | `Motion`'s new fields (Task 3) cross seams inside `Carried.motion`. This plan adds no `Ecs` component and no `Client` field; if one is ever needed, classify it in `carry_components` / `Client::arrive`. |
| **Bodies** (`body.rs`: `EntityKind::Body`, `BodyState` with `user_force` and damping, `BODY_GRAVITY`; `ColliderSpec::{CapsuleY, Cuboid, Ball, Compound}` in `protocol.rs`; voxel bodies are bodies, ticked by `tick_voxel_bodies_safe` with `VOXEL_BODY_GRAVITY`). Generic bodies have no per-tick pass of their own. | Task 4 adds one buoyancy pass over every `EntityKind::Body` (voxel bodies included). It sets the body's `user_force` and damping, which `BodyState` already carries across a seam, so nothing water-specific travels. |
| **Transfers, in-flight table, arrival mailbox, world tick.** | Nothing in this plan; static water sends no messages. See "Hooks for the flow plans" at the end. |

### Static-phase semantics (decided here; the flow plans replace them)

- **Edits never change water state.**
  - **Placing a block into a water cell:** the block wins. The cell is solid, so it is not
    water. Body `volume` and `level` are untouched; the flow plan will relevel from the
    conserved volume.
  - **Breaking that block:** the cell is water again, because it is still inside its column's
    recipe range.
  - **Breaking ice:** the cell becomes water, as when DF ice thaws.
  - **Breaking ground at or below a column's `surface`** (the seabed, a lake bed or a dug
    tunnel) leaves a **dry** cell. Nothing flows, so a dug pocket under the sea stays dry until
    the flow plan. This is a known limit; record it in TODO.md (Task 10).
- **Bodies** are labelled once at genesis and never mutated in this plan.
- **Body membership:**
  - columns are 4-connected and share an equal `water_top`, within one region;
  - a body's id is its region-local lexicographically smallest member column, packed as
    `x * REGION_BLOCKS + z`;
  - `level = (water_top + 1) * WATER_FULL`;
  - `volume = WATER_FULL ×` the number of recipe cells that are not ice at genesis.
- **Depth:** recipe water is always full, so every water cell is DF depth 7. Wading (depth 1–3)
  exists in the rules and is tested with a fake `TileSource`. Live, it only appears after the
  flow plans.
- **No drowning:** there is no health system. Swimming exposes `Immersion::Head`; damage is
  left for later.
- **Rendering departs from §B.5.** Instead of a second pulled-record pipeline, water is a
  per-tile Bevy `Mesh` with a blended material on every backend, WebGL2 included.
  - **Gains:** one code path, absorption baked into vertex colour alpha (§B.3 option (a)), and
    Bevy's own fog and lighting.
  - **Costs:** about 16 tile entities per 256 m region in `Transparent3d` (144 for the fortress
    3×3), sorted by Bevy. §B.3 wanted those sorts avoided, but at this count they cost
    microseconds.
  - **Upgrade path:** a pulled water pipeline later, once flow makes water change at 5 Hz.

## Review Focus

1. **Region seams.** Water must not draw a wall at a region seam where the neighbour's water
   continues. Swimming, wading paths and floating bodies must behave the same placed centred,
   across a seam or at a corner.
   - Test `no_side_faces_on_region_edge` (Task 6).
   - Tests `swim_is_seam_invariant` (Task 3), `pond_detour_is_seam_invariant` (Task 2) and
     `floating_body_is_seam_invariant` (Task 4).
2. **Ice over water.** A body under lake ice swims; breaking an ice block shows water in that
   cell; no water top face draws under ice.
   - Tests `ice_cell_is_not_water_until_broken` (Task 5) and `no_top_face_under_ice` (Task 6).
3. **Edits in water** follow the static semantics above.
   - Placing makes the cell dry, breaking the placed block makes it wet again, and digging the
     seabed leaves it dry.
   - Test `edits_follow_static_water_semantics` (Task 5; the seabed case re-asserted in
     Task 9).
4. **Rollback across the surface.** Undoing ticks in which a body enters, swims and leaves water
   restores the state hash bit-exactly, because `Motion` gained a field.
   - Test `rollback_across_water_surface_restores_hash` (Task 3).
5. **Stale data from before the switch.**
   - Old region saves still hold water blocks. They must be refused (`REGION_FILE_VERSION`
     bump) and regenerated, not loaded with doubled water.
   - Old worldgen cache entries must miss.
   - Test `old_region_file_version_is_refused` (Task 9).

---

### Task 1: Liquid block kind (Phase 0)

**Files:**
- Modify: `crates/game/src/registry.rs` (`BlockDef`, `props()`).
- Modify: `crates/game/src/block_props.rs` (`BlockProps`).
- Modify: `assets/blocks/blocks.ron:17`.
- Modify the sites in the decision table below.
- Test: `crates/game/tests/block_props.rs`, `crates/game/tests/collider_genesis.rs`,
  `crates/game/tests/pathfind.rs`, `crates/game/tests/block_edit.rs`,
  `crates/game/tests/orders.rs`.

**Interfaces:**
- Produces:
  - `pub enum BlockKind { Solid, Liquid }` in `block_props.rs`, re-exported. Serde default is
    `Solid`.
  - `BlockDef.kind: BlockKind` (`#[serde(default)]`).
  - `BlockProps::is_liquid(&self, id: BlockId) -> bool`.
  - `BlockProps::blocks_ray(&self, id: BlockId) -> bool`, true iff `id != AIR && !is_liquid(id)`.
  - A liquid id is always in `non_solid`, so `is_solid(water) == false`.
- Water's manifest line becomes
  `(id: 5, name: "water", textures: All("water.png"), kind: Liquid, drop: Nothing)`.

**Decision per site.** Sites are named by function; line numbers drift, so find each by name.
On 2026-11-07, `grep -rn "is_water\|BlockId(5)\|SLIME_NO_SPAWN" crates/game/src` lists the
hard-coded ones.

| Site | New rule |
|---|---|
| `block.rs` `collider_cells` / `push_block_cells`, genesis `seed_terrain_genesis` (`state.rs`), the halo's fixed colliders (`halo.rs`, through `collider_cells`) | No change. `is_solid` now excludes water, so water gets no collider, an all-water chunk no cuboid, and a halo water chunk no fixed collider. |
| `pathfind.rs` `WATER` / `is_water` / `passable_block` / `solid_block` | Delete `WATER` / `is_water`; use `block_props().is_liquid`. Keep the same rule: liquid is neither floor nor passable (Task 2 replaces it with depth rules). |
| `player.rs` `restore_translation`, `item_piles.rs` `pile_support`, `mining.rs` `mineable` / `mineable_seen` | Replace `crate::pathfind::is_water(id)` with `block_props().is_liquid(id)`. |
| `slime.rs` `SLIME_NO_SPAWN_BLOCK` | Delete it; skip a spawn when `is_liquid(top)`. |
| `edit.rs` `cell_solid` / `block_solid` (also behind `raycast_block_in`, hence `WorldView::raycast`) | A liquid cell is empty: the ray passes through it, and Place may target it (the block replaces the water). |
| `orders.rs` dig / build validity | Nothing to dig iff air or liquid; a free cell for a build is air or liquid. The felling check is unchanged. |
| `items.rs` (air drops nothing) | No change; water's `drop: Nothing` covers it. |
| `voxel_body.rs` (foliage classification, body mass boxes, chisel bounds) | Liquid counts as air: a detach never puts water into a body. |
| `voxel.rs` top surface voxel | No change: slimes still read the top voxel and test `is_liquid`. |
| `lod.rs`, `lod_builder.rs` | No change in this task (Task 8). |
| Client `block_target.rs` (crosshair ray + its water skip) | The ray uses `blocks_ray`; drop the water special case. |
| Client `commander/designate.rs`, `visible.rs`, `move_order.rs` (`solid` callbacks of the pick rays) | Use `blocks_ray`. |
| Client `commander/tiles.rs` `is_dug` | No change (dug means air). |
| Client `commander/mod.rs` (`HeightGrid::new(id_of("water"))`), `height.rs` | Skip ids where `is_liquid` instead; `HeightGrid::new` loses its `water` argument. |
| Client meshing (`binary.rs`, `bricks.rs`, `bridge.rs` `is_opaque_throughout`, `visibility.rs`, `coarse.rs`) | No change in this task: water still draws opaque until Task 6. |

- [ ] **Step 1: Write the failing tests**
  - `block_props.rs`, `water_is_liquid_and_not_solid`: an installed manifest gives
    `is_liquid(BlockId(5))`, `!is_solid(BlockId(5))` and `!blocks_ray(BlockId(5))`, and
    `blocks_ray(ice)`.
  - `collider_genesis.rs`, `all_water_chunk_has_no_collider`: a uniform water chunk seeded
    through `seed_terrain_genesis` attaches no collider. A chunk of stone under water has only
    the stone keys.
  - `block_edit.rs`, `edit_ray_passes_through_water`: a ray from above a pond hits the floor
    block. Place against the floor fills the water cell above it with the placed block.
  - `orders.rs`, `water_cell_is_free_for_build_and_empty_for_dig`: Build is valid on a water
    tile and Dig is invalid there.
  - `halo.rs`, `halo_water_chunk_has_no_collider`: a neighbour's all-water chunk inside the
    halo band adds no fixed collider to the region's physics. (`halo::` is in `seam_tests`.)
  - Every new test calls `common::install_real_block_props()` first.
- [ ] **Step 2: Run them and confirm they fail**
  - Run `just test-one game "--test it block_props::"`.
  - Run `just test-one game "--test it collider_genesis::"`.
  - Run `just test-one game "--test it block_edit::"`.
  - Run `just test-one game "--test it orders::"`.
  - Run `just test-one game "--test it -- --include-ignored halo::halo_water"`.
  - Expected: FAIL, because `is_liquid` and `blocks_ray` don't exist yet.
- [ ] **Step 3: Implement the kind and the site table above**
  - The tests' own `WATER = BlockId(5)` constants (`tests/pathfind.rs:6`,
    `tests/item_piles.rs:20`) may stay.
- [ ] **Step 4: Run Step 2's commands, then `just test-threads` and `just smoke`**
  - Expected: PASS, and smoke green (client type-check included).
- [ ] **Step 5: Run the physics and determinism checks**
  - Run `just test-one game "--test it physics_goldens::"` and `just determinism-wasm`.
  - Expected: PASS. If a pinned scene contains water, the hash changes. Re-pin only that one,
    and say in the commit which scene and why.
- [ ] **Step 6: Commit**
  - Message: `feat(water): liquid block kind — water has no collider, ray and orders see through it`

### Task 2: Water query and DF depth rules

**Files:**
- Create: `crates/game/src/water.rs` (`pub mod water; pub use water::*;` in `lib.rs`).
- Modify: `crates/game/src/orders.rs` (`TileSource`), `crates/game/src/pathfind.rs`.
- Modify: `crates/client/src/renderer/commander/tiles.rs` (`TileMirror`).
- Test: `crates/game/tests/pathfind.rs`, unit tests in `water.rs`.

**Interfaces:**
- Produces:
  - Constants `WATER_FULL`, `DF_DEPTH_LEVELS`, `WADE_MAX_DEPTH`, `SWIM_MIN_DEPTH`,
    `DROWN_DEPTH` and `WADE_COST` (values in Global Constraints).
  - `pub fn df_depth(units: u16) -> u8`: `0` for 0, else
    `ceil(units * 7 / WATER_FULL)` clamped to `1..=7`, in integer arithmetic.
  - `impl Rollback { pub fn water_units_at(&self, p: WorldBlockPos) -> u16 }` in `water.rs`. It
    reads the block through `Rollback::halo_block(p)` (own region, implied ground included, or
    the halo), and returns `0` where that is `None`, beyond own ∪ halo. In this task it returns
    `WATER_FULL` iff the block `is_liquid`. Task 5 extends it. It is the single water read in
    the sim.
  - `WorldView` gains `fn water_units(&self, p: WorldBlockPos) -> u16`; `RegionView` delegates
    to `water_units_at`.
  - New `TileSource` method `fn water_units(&self, p: TilePos) -> u16 { 0 }`:
    - `impl TileSource for Rollback` (`orders.rs`) maps the region-local `p` to
      `WorldBlockPos::from_local` and calls `water_units_at`, so `PathPlan`'s fine A* sees halo
      water.
    - `OnMap` (`path_plan.rs`) forwards it.
    - The client's `TileMirror` reads its mirrored blocks the same way.
- Pathfinding rules (replacing Task 1's liquid special case):
  - `passable_block` = not solid. Liquid is passable.
  - `standable(p)` additionally needs `df_depth(water_units(p)) <= WADE_MAX_DEPTH`.
  - A successor whose target has depth 1–3 costs `+ WADE_COST`.

- [ ] **Step 1: Write the failing tests**
  - Unit tests in `water.rs`:
    - `df_depth_maps_units_to_sevenths`: 0→0, 1→1, `WATER_FULL / 7`→1, `WATER_FULL * 3 / 7`→3,
      `WATER_FULL * 3 / 7 + 1`→4, `WATER_FULL`→7.
  - `pathfind.rs`, against a fake `TileSource` whose `water_units` is overridden per tile:
    - `deep_water_is_not_standable`: depth 4 is not standable.
    - `shallow_water_is_wadable_at_a_cost`: depth 3 is standable, and a path across 3 tiles of
      depth 2 costs exactly `3 * WADE_COST` more than the same path dry.
    - `path_detours_around_a_pond`: a 3×3 pond of depth 7 between start and goal yields a path
      that touches none of its tiles.
  - `water_units_at_reads_liquid_blocks`: a region with a water block reports `WATER_FULL`
    there, `0` above and below it, and `0` beyond the halo.
  - `halo.rs`, `water_units_at_reads_the_halo`: a water block 3 blocks inside the east
    neighbour reads `WATER_FULL` from the spawn region at local x = `REGION_BLOCKS + 2`.
  - `handoff_schedule.rs`, `pond_detour_is_seam_invariant`: a `Scenario` in which a `MoveTo`
    must detour around a 4×4 pond of water blocks, with the pond centred on the placement
    origin. Pass it to `assert_seam_invariant`. The walker never stands on a pond tile in any
    placement. Wading (depth 1–3) is covered by the fake-`TileSource` tests above; static water
    is always depth 7.
- [ ] **Step 2: Run them and confirm they fail**
  - Run `just test-one game "--lib water::"`, `just test-one game "--test it pathfind::"` and
    `just test-one game "--test it -- --include-ignored halo::water"`.
  - Also run `just test-one game "--test it -- --include-ignored handoff_schedule::pond_detour"`.
  - Expected: FAIL (unresolved items).
- [ ] **Step 3: Implement `water.rs`, the `TileSource` method and the pathfind rules**
  - Implement them for `Rollback`, `OnMap` and `TileMirror`.
- [ ] **Step 4: Run Step 2's commands, then `just test-threads` and `just smoke`**
  - Expected: PASS, and smoke green.
- [ ] **Step 5: Commit**
  - Message: `feat(water): water_units_at query and DF depth rules for pathing`

### Task 3: Swimming

**Files:**
- Modify: `crates/game/src/movement.rs`, `crates/game/src/water.rs`, `crates/game/src/region.rs`
  (the tick).
- Test: create `crates/game/tests/swimming.rs` (add `mod swimming;` to `tests/it.rs`),
  `crates/game/tests/handoff_schedule.rs`, `crates/game/tests/handoff_state.rs`.

**Interfaces:**
- Consumes: `Rollback::water_units_at` (Task 2).
- Produces:
  - `pub enum Immersion { Dry, Feet, Waist, Head }` in `movement.rs`. It derives `Clone, Copy,
    Debug, Default (= Dry), PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, StateHash`.
  - `pub fn immersion_at(rb: &Rollback, feet: Vector, height_units: f32) -> Immersion`. `feet` is
    the region-local body position in units, as `Motion`'s callers hold it in `water.rs`. It samples the cells containing:
    - the feet (+1 unit);
    - the waist (½ height);
    - the eye (`PLAYER_EYE_HEIGHT / PLAYER_HEIGHT` of the height).

    The deepest wet sample wins; a sample is wet iff `water_units_at > 0`. It is pure and reads
    own ∪ halo, so a body at a seam samples its neighbour's water.
  - `impl Rollback { pub(crate) fn tick_immersion_safe(&mut self) }`. Each tick, for every
    entity with a `Motion` (players, people, slimes: entity-generic), it computes `immersion_at`
    from the body pose and capsule height, and writes `motion.immersion` undo-safely only when it
    changed.
    - It is called in `region.rs`'s tick right after `tick_intents_safe` and before
      `tick_slimes_safe`, so every mover plans with this tick's immersion.
    - `CameraController::on_tick` (which only has `GameData`) and `walk_body_with` need no new
      parameter.
  - `Motion` gains `pub immersion: Immersion` and `pub blocked_horizontally: bool` (hash and wire
    change). They travel across seams inside `Carried.motion`; no bundle change.
  - `plan_with(motion, keys, yaw, tick, params)` reads `motion.immersion`.
  - `character_controller(motion)`: no snap-to-ground and no autostep while
    `immersion >= Waist`, same as while flying.
- Constants in `movement.rs`, all m/s or m/s² like their neighbours:

| Constant | Value |
|---|---|
| `SWIM_SPEED` | 2.2 |
| `SWIM_SPRINT_SPEED` | 3.0 |
| `WADE_SPEED_FACTOR` | 0.75 (Feet) |
| `SWIM_UP_SPEED` | 2.4 |
| `SWIM_DOWN_SPEED` | 2.4 |
| `SWIM_SINK_SPEED` | 0.5 (idle, Head) |
| `SWIM_VERTICAL_ACCEL` | 12.0 (`vel_y` approaches its target at this rate) |
| `SWIM_EXIT_SPEED` | `JUMP_SPEED` |

- Rules:
  - **Flying** ignores water.
  - **At `Waist`** with neither jump nor descend held, the vertical target is 0: the body
    floats at the surface.
  - **At `Head`**, the target is `+SWIM_UP_SPEED` if jump is held, `-SWIM_DOWN_SPEED` if
    descend is held, else `-SWIM_SINK_SPEED`.
  - **Leaving the water:** while `immersion >= Waist`, jump held and the previous tick's
    horizontal move blocked (`settle` records it in a new
    `Motion.blocked_horizontally: bool`), set `vel_y = SWIM_EXIT_SPEED`. This lets the body
    climb out onto a 1-block ledge.
  - **Adventure** keeps no-flight; double-tap Space does nothing in water.

- [ ] **Step 1: Write the failing tests in `swimming.rs`**
  - All fixtures are hand-built regions with water blocks and call
    `common::install_real_block_props()`. Ticks go through the real region tick (see
    `jump_arc.rs` for the harness pattern), so `tick_immersion_safe` runs.
  - `body_in_deep_water_floats_at_the_surface`: dropped into a 4-deep pool, the feet settle so
    that `immersion == Waist` holds for 40 consecutive ticks, with `|vel_y| < 0.1`.
  - `holding_descend_dives_and_jump_surfaces`: descend for 20 ticks reaches `Head`, then jump
    returns to `Waist`.
  - `swim_speed_is_capped`: the horizontal speed over 20 ticks of W in water is
    ≤ `SWIM_SPEED + 1e-3` m/s.
  - `swimmer_climbs_out_onto_a_one_block_ledge`: swimming into a wall whose top is 1 block
    above the surface, holding W + jump, ends grounded on the ledge within 40 ticks.
  - `wading_slows_walking`: with only the feet in water, the walk speed is
    `WALK_SPEED * WADE_SPEED_FACTOR` ± 1e-3.
  - `rollback_across_water_surface_restores_hash`: hash the state, run 30 ticks of a body
    walking off a bank into water and swimming, undo them all, and assert the same hash
    (`game::state_hash`).
  - `people_swim_too`: a person whose wander goal is across a 3-wide, 4-deep channel does not
    sink to the bed. Its `immersion` reaches `Waist` while crossing.
  - In `handoff_schedule.rs`, `swim_is_seam_invariant`: a `Scenario` in which a player swims
    for 3 s across a 6-deep pool centred on the placement origin, holding W, and dives halfway
    with descend. Pass it to `assert_seam_invariant` (lockstep and jittery; it compares within
    the harness's named tolerances). In every placement:
    - the body's `immersion` is never `Dry` while it is over the pool;
    - its feet never touch the bed while descend is not held.
  - `handoff_state.rs`, `bundle_carries_immersion`: reuse the `bundle_round_trip_is_state_exact`
    fixture with a swimming `Motion`. `immersion` and `blocked_horizontally` are bit-equal after
    the round trip.
- [ ] **Step 2: Run them and confirm they fail**
  - Run `just test-one game "--test it -- --include-ignored swimming::"`.
  - Run `just test-one game "--test it -- --include-ignored handoff_schedule::swim_is_seam"`.
  - Run `just test-one game "--test it handoff_state::bundle_carries"`.
  - Expected: FAIL (no `Immersion`).
- [ ] **Step 3: Implement**
  - Implement `Immersion`, `immersion_at`, `tick_immersion_safe` and its call in the tick, the
    `Motion` fields, the `plan_with` branch and the controller gate.
- [ ] **Step 4: Run Step 2's command**
  - Also run `just test-one game "--test it jump_arc::"`, `just test-one game "--test it
    walking_smoothness::"`, `just test-one game "--test it physics_goldens::"`,
    `just test-threads` and `just smoke`.
  - Expected: all PASS. The jump and walking goldens are dry and must not change.
- [ ] **Step 5: Run `just determinism-wasm`**
  - Expected: PASS. Re-pin only if a pinned scene serialises `Motion`; it gained fields, so the
    crc32 may move. Say so in the commit.
- [ ] **Step 6: Commit**
  - Message: `feat(water): swimming — Immersion in Motion, float/dive/climb out, wading`

### Task 4: Buoyancy and drag for dynamic bodies

**Files:**
- Create: `crates/game/src/buoyancy.rs` (`pub mod buoyancy; pub use buoyancy::*;`).
- Modify:
  - `crates/game/src/protocol.rs` (`ColliderSpec::volume_m3`);
  - `crates/game/src/region.rs` (the tick: call the new pass).
- Test: `crates/game/tests/voxel_body.rs` (its own `[[test]]` target `voxel_body`),
  `crates/game/tests/body.rs`, `crates/game/tests/handoff_schedule.rs`.

**Interfaces:**
- Consumes:
  - `Rollback::water_units_at` (own ∪ halo, Task 2);
  - `ColliderSpec::{CapsuleY, Cuboid, Ball, Compound}` (`protocol.rs`);
  - each body's explicit mass (its rapier body's `mass()`, set from `mass_props.rs`);
  - its `user_force`.
- Produces:
  - `impl ColliderSpec { pub fn volume_m3(&self) -> f32 }`, in plain f32 arithmetic with π as a
    literal const, from unit lengths ÷ `UNITS_PER_METER`:
    - a cuboid is `8·hx·hy·hz`;
    - a ball is `4/3·π·r³`;
    - a capsule is `π·r²·2h + 4/3·π·r³`;
    - a compound is the sum of its parts.
  - `pub fn submerged_fraction(rb: &Rollback, aabb_min: Vector, aabb_max: Vector, com: Vector)
    -> f32`. It returns the share of the AABB height below the water surface, in `[0, 1]`. The
    surface is the top of the highest wet cell in the column under `com`, found by scanning up
    from `aabb_min.y` with `water_units_at`. The column may lie in the halo.
  - `pub const WATER_DENSITY: f32 = 1000.0` (kg/m³).
  - `pub const WATER_LINEAR_DAMPING: f32 = 2.0` and `pub const WATER_ANGULAR_DAMPING: f32 = 1.0`.
  - `impl Rollback { pub(crate) fn tick_buoyancy_safe(&mut self) }`, called in `region.rs`'s
    tick right after `tick_immersion_safe` (Task 3), so before the physics step.
- **The pass:** for every owned `EntityKind::Body` (generic bodies and voxel bodies alike; halo
  mirrors are kinematic and skipped), through the existing undo-safe body mutators
  (`mutate_body_safe` or the one `body.rs` uses), write only when a value changed:
  - Set its user force to `(0, (f × volume_m3 × WATER_DENSITY − mass) × G × UNITS_PER_METER,
    0)`.
    - `G` is `BODY_GRAVITY`; voxel bodies use `VOXEL_BODY_GRAVITY`. Both equal
      `movement::GRAVITY` today; use each kind's own constant.
    - Dry (`f == 0`), the force is exactly the body's existing weight force, bit-for-bit, so
      dry bodies' physics goldens do not move.
  - Set linear damping to `f × WATER_LINEAR_DAMPING`, and angular damping to
    `max(base, f × WATER_ANGULAR_DAMPING)`, where `base` is the kind's own constant
    (`BODY_ANGULAR_DAMPING`, or the voxel body's).
- **Nothing new travels in the bundle.** `BodyState` already carries `user_force` and damping,
  and the pass recomputes them at the destination from own ∪ halo water, so a crossing needs no
  water-specific code.
- **Rest rules:** a body floating at rest counts as resting by the existing rules
  (`REST_TICKS`, `BODY_SETTLE_TICKS`). So a felled log that lands in a lake despawns there
  (fortress) or deposits its `DropItems` at the cell under it (Adventure). The piles then sink
  to the bed, because `pile_support` skips liquid.

- [ ] **Step 1: Write the failing tests**
  - `log_floats_in_a_pond`: a detached 1×1×4 oak log dropped above a 6-deep pool ends at rest
    with its AABB top above the surface and its bottom below.
  - `stone_sinks_to_the_bed`: a stone block dropped in the same pool comes to rest on the bed.
  - `buoyancy_is_rollback_exact`: hash, run 60 ticks of a floating log, undo them, and assert
    the same hash.
  - `body.rs`, `dropped_cube_floats_and_dry_bodies_unchanged`: a dev-dropped 1 m cube
    (`dev_drop_body_safe`) over water ends floating, its AABB straddling the surface. The same
    cube dropped on dry ground has, after every tick, `user_force` bit-equal to
    `(0, −mass × BODY_GRAVITY × UNITS_PER_METER, 0)`, linear damping 0, and angular damping
    `BODY_ANGULAR_DAMPING`. Those are the values `body_state` gives it at spawn.
  - `handoff_schedule.rs`, `floating_body_is_seam_invariant`: a `Scenario` in which a log drops
    into a 6-deep pool centred on the placement origin, with a sideways push. Pass it to
    `assert_seam_invariant`, which compares the placements within its named tolerances. In
    every placement the log ends floating: its AABB straddles the surface. It must never end on
    the bed, and never be lost or duplicated.
- [ ] **Step 2: Run them and confirm they fail**
  - Run `just test-one game "--test voxel_body -- --include-ignored log_floats_in_a_pond stone_sinks_to_the_bed buoyancy_is_rollback_exact"`.
  - Run `just test-one game "--test it -- --include-ignored body::dropped_cube"`.
  - Run `just test-one game "--test it -- --include-ignored handoff_schedule::floating_body"`.
  - Expected: FAIL (the log sinks).
- [ ] **Step 3: Implement as specified above**
- [ ] **Step 4: Run Step 2's commands, then `just test-one game "--test it chop::"`,
  `just test-threads` and `just smoke`**
  - Expected: PASS.
- [ ] **Step 5: Run `just determinism-wasm`**
  - Expected: PASS (re-pin rules as in Task 3).
- [ ] **Step 6: Commit**
  - Message: `feat(water): buoyancy and drag for dynamic bodies`

### Task 5: Recipe water and bodies at genesis (beside the blocks)

**Files:**
- Modify: `crates/game/src/ground.rs` (`ColumnGround`).
- Modify: `crates/game/src/water.rs` (labelling, bodies, query).
- Modify: `crates/game/src/state.rs` (`GameData.water_bodies`, genesis seeding).
- Modify: `crates/game/src/region.rs:932-946` (`Region::from_columns`).
- Modify: `crates/worldgen/src/lib.rs` (`columns_from_patch`, ~:620-640).
- Test: `crates/game/tests/ground.rs`, `crates/game/tests/region_from_columns.rs`,
  `crates/worldgen/tests/` (an existing forged-world test file, or a new `water_recipe.rs`
  registered in worldgen's `tests/it.rs`).

**Interfaces:**
- Produces:
  - `ColumnGround` gains:
    - `#[serde(deserialize_with = "footprint")] pub water_top: Box<[i16]>`: the topmost water or
      ice block y, else `NO_WATER`.
    - `#[serde(deserialize_with = "footprint_u32")] pub water_body: Box<[u32]>`: the body id,
      else `NO_BODY`.
  - `Default` and `plain` fill both with `NO_WATER` / `NO_BODY`. Add
    `ColumnGround::recipe_water(&self, lx, lz) -> Option<(i32, i32)>`, the inclusive y range
    `(surface + 1, water_top)` or `None`.
  - Worldgen sets `water_top[i] = c.water_top as i16` iff `c.water_top > c.top`, else
    `NO_WATER`. It **keeps writing water blocks** in this task.
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
    StateHash)] pub struct WaterBodyId(pub u32);`
  - `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, StateHash)] pub struct
    WaterBody { pub level: i64, pub volume: u64 }`. Both are in `WATER_FULL` units; `level` is
    absolute (block y × `WATER_FULL`).
  - `pub fn label_water_bodies(ground: &mut [(ColumnCoords, ColumnGround)], is_ice: impl
    Fn(ColumnCoords, (usize, usize), i32) -> bool) -> BTreeMap<WaterBodyId, WaterBody>` in
    `water.rs`:
    - It runs a deterministic 4-connected flood over the region's block columns, in ascending
      (x, z) order, joining columns with equal `water_top`.
    - It writes `water_body`.
    - `volume` counts the cells in each recipe range for which `is_ice(column, (lx, lz), y)` is
      false.
    - `Region::from_columns` passes a closure that reads the generated `Column`'s block.
  - `GameData` gains `#[undo(map)] water_bodies: BTreeMap<WaterBodyId, WaterBody>`. It follows
    the `ground` doc pattern: genesis-only, raw insert, logged mutators unused in this plan.
    Accessor: `pub fn water_bodies(&self) -> &BTreeMap<WaterBodyId, WaterBody>`.
  - `Region::from_columns` labels before seeding, then
    `Rollback::seed_water_genesis(bodies)`.
  - `Rollback::water_units_at` becomes `WATER_FULL` iff:
    - the cell is a liquid block (the blocks still exist), **or**
    - the cell is inside `recipe_water` and its block (via `halo_block`) is AIR. The recipe is
      the region's own `ground` for own cells and `halo_ground` for halo cells, so halo water
      reads the neighbour's recipe.

    Task 9 drops the first clause.
  - `Rollback::water_body_at(&self, pos: BlockPos) -> Option<WaterBodyId>`: the column's body,
    when `water_units_at(pos) > 0`. Region-local own cells only: bodies are per region, and a
    halo cell has no body here.

- [ ] **Step 1: Write the failing tests**
  - `region_from_columns.rs`:
    - `pond_columns_form_one_body`: a hand-built pond of 3×3 columns with `water_top` 70 over
      surface 67 yields one body with id = its min column, `level == 71 * WATER_FULL` and
      `volume == 27 * WATER_FULL`.
    - `separate_ponds_at_one_height_are_two_bodies`.
    - `river_steps_are_separate_bodies`: adjacent columns with `water_top` 70 and 69 give two
      bodies.
    - `ice_cell_is_not_water_until_broken`: the top recipe cell holds ice, so
      `water_units_at == 0` there; after a Break edit it is `WATER_FULL`. The ice cells are
      excluded from `volume`.
    - `edits_follow_static_water_semantics`:
      - Place stone into a recipe cell, and it reads 0; break it, and it reads `WATER_FULL`.
      - Break the bed block at `surface`, and it reads 0.
      - Assert `water_bodies` is unchanged throughout.
  - worldgen test `forged_water_recipe_matches_water_blocks`: over one forged coastal region
    (use the existing forged fixture world), for every block, `water_units_at > 0` iff
    worldgen wrote a water block there.
  - `ground.rs`, `column_ground_rejects_short_water_arrays`: the deserializer refuses a short
    `water_top` / `water_body`.
  - `halo.rs`, `halo_carries_recipe_water`: a pond whose recipe straddles the seam reads
    `water_units_at == WATER_FULL` on both sides of the seam, from either region, including
    halo cells.
  - Recipe membership must hold in the halo too: place a block into a halo water cell from the
    owning region and, after the T+1 halo delta, the neighbour reads `0` there.
- [ ] **Step 2: Run them and confirm they fail**
  - Run `just test-one game "--test it region_from_columns::"`.
  - Run `just test-one game "--test it ground::"`.
  - Run `just test-one worldgen "--test it -- --include-ignored water_recipe::"`.
  - Run `just test-one game "--test it -- --include-ignored halo::halo_carries_recipe_water"`.
  - Expected: FAIL.
- [ ] **Step 3: Implement as specified**
  - Bump `crates/server/src/region_store.rs` `REGION_FILE_VERSION` by one (+1 from the value
    in the tree), because `GameData` changed. Old saves are set aside as `.bak` and
    regenerate.
  - The halo already mirrors neighbours' `ColumnGround` (`GameData::halo_ground`, seeded by
    `HaloUpdate.ground`), so the new fields reach it with no halo change. If
    `halo_carries_recipe_water` fails for any reason other than the missing fields, stop and
    report rather than adding halo plumbing.
- [ ] **Step 4: Run Step 2's commands, then `just test-threads`, `just smoke` and
  `just test-one worldgen "--test scene_hash_golden"`**
  - Expected: PASS. `scene_hash_golden` may move because `ColumnGround` gained fields. Re-pin
    it with that reason in the commit.
- [ ] **Step 5: Run `just determinism-wasm`**
  - Expected: PASS (re-pin rules as in Task 3).
- [ ] **Step 6: Commit**
  - Message: `feat(water): recipe water in ColumnGround and water bodies at genesis`

### Task 6: Client water pass (near)

**Files:**
- Create: `crates/client/src/renderer/water/mod.rs` (plugin, messages, tile roots, scheduling).
- Create: `crates/client/src/renderer/water/mesh.rs` (pure tile mesher).
- Create: `crates/client/src/renderer/water/material.rs` (`WaterExtension`).
- Create: `assets/shaders/water.wgsl`.
- Modify:
  - `crates/client/src/renderer/mod.rs` (add the plugin);
  - `renderer/bridge.rs` (send `WaterMsg`s);
  - `renderer/compute.rs` (`JobKind::WaterMesh = 7` + `run_encoded`).
- Modify the meshing sites so liquid is empty:
  - `meshing/binary.rs:56, 79, 366, 379`;
  - `bricks.rs:163-166, 240-244`;
  - `bridge.rs:298-304`;
  - `meshing/visibility.rs:80, 90`;
  - `meshing/coarse.rs:178`;
  - `slice_plane.rs:65`.
- Test: unit tests in `water/mesh.rs` (`--lib water::`), `compute::` round-trip.

**Interfaces:**
- Consumes:
  - `ColumnGround::recipe_water` and `water_top` (Task 5);
  - `BlockProps::is_liquid`;
  - the bridge's `SimChunk` / `SimChunkIndex` blocks.
- Produces:
  - `#[derive(Message)] pub enum WaterMsg`:
    - `Region { region: RegionId, ground: Arc<BTreeMap<ColumnCoords, ColumnGround>> }`, sent
      when a region snapshot loads or is replaced;
    - `RemoveRegion { region: RegionId }`;
    - `ChunkEdited { region: RegionId, column: ColumnCoords }`, sent on every applied
      `SetBlocks` whose footprint has recipe water.
  - Register it with `add_session_message_clear::<WaterMsg>()`, and
    `WaterTiles(BTreeMap<(RegionId, [i32; 2]), WaterTile>)` with `add_session_reset`, as
    `item_piles.rs:284-299` does.
  - `pub struct WaterTileInput { pub origin: [i32; 2], pub columns: Vec<WaterColumn> }`, row
    major over `WATER_TILE_BLOCKS²`, `Serialize`/`Deserialize`.
  - `pub struct WaterColumn { pub runs: Vec<WaterRun> }` with `pub struct WaterRun { pub y0:
    i16, pub y1: i16, pub solid_above: bool }`.
    - Each run is an inclusive y run of water cells, built on the main thread from the recipe
      range minus non-air blocks.
    - `solid_above` is true when the cell over `y1` holds a block (e.g. ice).
    - An empty `runs` means no water.
  - The mesher also needs its horizontal neighbours' runs and solids. `WaterTileInput` carries
    the tile plus a 1-column apron (`(WATER_TILE_BLOCKS + 2)²` columns) and
    `edge: [bool; 4]`, which marks which apron sides lie outside the region.
  - `pub fn mesh_water_tile(input: &WaterTileInput) -> WaterMeshData`, a pure function.
    `WaterMeshData` holds `positions`, `normals`, `colors` (RGBA f32) and `indices`, all
    tile-local in world units.
  - `WaterMeshJob` implements `ComputeJob` (input `WaterTileInput`, output `WaterMeshData`).
- Mesher rules:
  - **Top faces** go on each run's top cell when the cell above is not water. They are
    greedy-merged over equal top y.
  - **Side faces** go toward a horizontal neighbour *inside the tile or the same region* whose
    cell at that y is neither water nor solid; they span the run's height there.
  - **No side faces on a region-edge column toward the outside.** The neighbour region's water
    is unknown, and walls must not show at seams.
  - **Under ice:** a top face whose cell above is solid (ice) is not emitted.
  - **Colour:** `WATER_COLOR` (linear RGB `[0.10, 0.32, 0.45]`). Alpha is
    `1 − exp(−WATER_ABSORPTION_PER_M × depth_m)`, clamped to `[WATER_MIN_ALPHA,
    WATER_MAX_ALPHA]` = `[0.35, 0.92]`, with `WATER_ABSORPTION_PER_M = 0.35`. `depth_m` is the
    run length for top faces and 1 m for side faces. (Client render code, so f32 `exp` is
    fine.)
- Material: `type WaterMaterial = ExtendedMaterial<StandardMaterial, WaterExtension>`.
  - Base: `alpha_mode: Blend`, `perceptual_roughness: 0.15`, `reflectance: 0.5`, white
    `base_color`, so vertex colour carries tint and alpha.
  - `WaterExtension` binds `FortressViewUniform` at binding 100, synced like
    `voxel_material.rs`'s `sync_chunk_material_cut`.
  - `water.wgsl`'s fragment runs Bevy's PBR path and **discards fragments above the cut** when
    the cut applies to the fragment's home-region column. Mirror `chunk_cut_applies` in
    `voxel_texture.wgsl`.
  - `specialize` sets `cull_mode = None` (top faces seen from below) and
    `depth_write_enabled = true` (the §B.3 single-layer rule).
  - Tiles get `NotShadowCaster`.
- Scheduling:
  - A tile is meshed on `WaterMsg::Region`, and re-meshed on a `ChunkEdited` inside it.
  - Mesh tasks go through `compute::spawn`, limited by `MAX_WATER_MESH_TASKS = 8` in flight.
  - Tile entities are `SessionScoped` children of the region root, placed at the region's
    world offset plus the tile origin, like `pile_world_position`.
- Liquid is now empty to the chunk mesher, so the old opaque water disappears and the new pass
  draws it.

- [ ] **Step 1: Write the failing unit tests in `water/mesh.rs`**
  - `flat_pond_is_one_top_quad`: a 4×4 pond of equal runs gives exactly 4 vertices and 6
    indices on its top.
  - `no_top_face_under_ice`: a run with `solid_above` emits no top face.
  - `side_face_toward_a_dry_hole`: a pond column next to an in-tile column with no water and no
    solid emits one side face spanning the run.
  - `no_side_faces_on_region_edge`: a run on the tile's outer edge, when that edge is the
    region edge, emits no outward side face.
  - `deeper_water_is_more_opaque`: the top-face alpha of a 10-deep run is greater than that of
    a 1-deep run, and both lie within `[WATER_MIN_ALPHA, WATER_MAX_ALPHA]`.
  - In `compute::` tests, `water_mesh_job_round_trips`, matching the existing job round-trip
    tests.
- [ ] **Step 2: Type-check, then (with Dawid's OK) run the tests**
  - Run `just smoke` (the client type-check must fail on the missing items).
  - With Dawid's OK, run `just test-one client "--lib water::"` and
    `just test-one client "--lib compute::"`.
  - Expected: FAIL.
- [ ] **Step 3: Implement the mesher, job, material, shader, plugin, bridge messages and the
  meshing-site changes**
- [ ] **Step 4: Re-run Step 2's commands**
  - Expected: smoke green and the unit tests PASS (or "pending Dawid").
- [ ] **Step 5: Check visually, with Dawid's OK**
  - `just adapter=Intel run`:
    - the sea and a lake are blue and see-through to the bed;
    - there are no walls at region seams;
    - water above the z-level cut is hidden (C toggles it).
  - `just pull-smoke` stays green.
  - If Dawid wants the browser check: `just web`, where WebGL2 uses the same material path.
- [ ] **Step 6: Commit**
  - Message: `feat(water): blended per-tile water pass; chunk mesher treats liquid as empty`

### Task 7: Underwater view, height grid and minimap from the recipe

**Files:**
- Modify:
  - `crates/client/src/renderer/commander/mod.rs` (`eye_in_block` :1052, `underground_view`
    :1077);
  - `crates/client/src/renderer/atmosphere.rs`;
  - `crates/client/src/renderer/commander/height.rs`;
  - `crates/client/src/renderer/commander/minimap.rs`;
  - `crates/client/src/renderer/first_person.rs`, if the F5 camera has its own atmosphere
    switch.
- Test: unit tests in `height.rs` and `atmosphere.rs`.

**Interfaces:**
- Consumes: `TileSource::water_units` on `TileMirror` (Task 2), and `ColumnGround::water_top`.
- Produces:
  - `pub fn eye_in_water(mirror: &TileMirror, eye: Vec3) -> bool`: the eye's cell has
    `water_units > 0`.
  - `pub fn underwater_fog() -> DistanceFog`: `Linear` from `UNDERWATER_FOG_START_M = 2.0` to
    `UNDERWATER_FOG_END_M = 24.0`, colour `UNDERWATER_FOG_COLOR` (sRGB `[0.06, 0.22, 0.30]`).
  - `underground_view` priority: underground beats underwater, and underwater beats open
    air. Underwater keeps the sky and the sun's shadows and swaps only the fog.
  - `HeightGrid`: a column's `surface` = max(top solid voxel, recipe `water_top + 1` when
    present). `ground` stays the top non-liquid voxel. The grid takes the ground recipes from
    `TileMirrorMsg::Reset`.
  - The minimap needs no rule change: its `surface > ground` water test now sees recipe water.
- [ ] **Step 1: Write the failing unit tests**
  - `height.rs`, `recipe_water_raises_surface_not_ground`: a column with `water_top` 70 over
    ground block 64 reads `surface == 71.0` and `ground == 65.0` (metres = blocks).
  - `atmosphere.rs`, `underwater_fog_is_short_and_blue`: start < end ≤ 24, and blue > red.
- [ ] **Step 2: Type-check with `just smoke`; run the tests only with Dawid's OK**
  - Command: `just test-one client "--lib height::"` / `"--lib atmosphere::"`.
  - Expected: FAIL.
- [ ] **Step 3: Implement**
- [ ] **Step 4: Re-run Step 2's commands, then look with Dawid's OK**
  - Expected: PASS.
  - Look: in Adventure F5, dive and see the blue fog; surface and see the sky. The minimap
    shows the sea.
- [ ] **Step 5: Commit**
  - Message: `feat(water): underwater view; height grid and minimap read recipe water`

### Task 8: LOD water surface layer

**Files:**
- Modify: `crates/game/src/lod.rs` (types; `merge_group` treats liquid as air).
- Modify: `crates/game/src/protocol.rs:489` (`ServerPacket::LodData`) and its encode test at
  `:707`.
- Modify: `crates/game/src/lib.rs:209` (`ClientUpdateEvent::LodData`).
- Modify: `crates/game/src/lod_builder.rs` (`build_lod_job` / `LodResult`).
- Modify: `crates/game/src/world_manager.rs:1823`.
- Modify: `crates/worldgen/src/lib.rs` (`coarse_from_patch` water → air; new `coarse_water`).
- Modify: `crates/server/src/lod_workers.rs`.
- Modify: `crates/client/src/renderer/lod_rings.rs` (+ a water child per root, reusing
  `water::material`).
- Test: `crates/game/tests/lod_streaming.rs`, worldgen `tests/` (the coarse-tile tests),
  `crates/game/tests/packet_codec.rs`.

**Interfaces:**
- Produces:
  - `pub struct LodWater { pub side: u16, pub runs: Vec<(u16, i16, u8)> }`. It derives
    `Debug, Clone, PartialEq, Default, Serialize, Deserialize`.
    - It is an RLE, row major over the key's `side × side` LOD columns, where
      `side = region_span() × cells_per_region(factor)`.
    - Each run is `(length, water_top_block_y or NO_WATER, depth_blocks clamped to 255)`.
    - Methods: `LodWater::encode(tops: &[(i16, u8)], side: u16) -> Self`,
      `decode(&self) -> Option<Vec<(i16, u8)>>` and `is_well_formed(&self) -> bool`.
  - `ServerPacket::LodData(LodKey, LodPayload, LodWater)` and the matching
    `ClientUpdateEvent::LodData(LodKey, LodPayload, LodWater)`.
  - `pub fn coarse_water(key: &LodKey, seed: u64, mats: Materials) -> LodWater` (+ `_forged`)
    in worldgen. It takes one `point_sample` per LOD column at the same point
    `coarse_from_patch` uses:
    - water iff `c.water_top > c.top`;
    - depth = `c.water_top − c.top`.
  - `build_lod_job(job, coarse, water: &dyn Fn(&LodKey) -> LodWater, generate) -> LodResult`
    with `LodResult = (LodKey, LodPayload, LodWater)`.
    - **Every job kind takes water from the recipe**, never from chunks. A rebuild caused by
      `TerrainChanged` resends the same water, and water never enters
      `terrain_fingerprint`.
  - `coarse_from_patch` stops emitting water material: the cell is air, and the seabed shows.
  - `merge_group` skips liquid ids like AIR.
  - **Client:** `spawn_lod_root` adds one `SessionScoped` child per root with a non-empty
    `LodWater`.
    - Its mesh: greedy-merged top quads, one per run of equal top within a row and extended
      across rows. `y = (top + 1)` blocks in units, scaled by the key's factor like the cell
      children.
    - Alpha from depth uses Task 6's formula and constants.
    - It is retired with its root (no separate lifecycle).
- [ ] **Step 1: Write the failing tests**
  - `packet_codec.rs`, `lod_data_round_trips_with_water`.
  - `lod_streaming.rs`:
    - `lod_water_is_recipe_not_terrain`: after a block edit under a lake in a running region,
      a rebuilt key's `LodWater` equals the pristine one, and `terrain_fingerprint` changes
      only because of the edited block.
    - `lod_terrain_has_no_liquid`: no decoded grid voxel of a coastal key `is_liquid`.
  - worldgen `coarse_water_matches_sea_level`: every water run over plain sea in a procedural
    key has `water_top == SEA_TOP_BLOCK`, and `side` equals the key's cell count per side.
  - `lod_water_rle_rejects_bad_length`: `is_well_formed` is false when the run lengths do not
    sum to `side²`.
- [ ] **Step 2: Run them and confirm they fail**
  - Run `just test-one game "--test it packet_codec::"`.
  - Run `just test-one game "--test it lod_streaming::"`.
  - Run `just test-one worldgen "--test it -- --include-ignored coarse"`.
  - Expected: FAIL.
- [ ] **Step 3: Implement the server, game and worldgen side, then the client child mesh**
- [ ] **Step 4: Re-run Step 2's commands, then `just smoke`**
  - Expected: PASS.
- [ ] **Step 5: Check visually, with Dawid's OK**
  - `just adapter=Intel run`: the far sea and lakes are blue sheets at one height across
    rings, the shallows are lighter, and there is no LOD water over near terrain.
- [ ] **Step 6: Commit**
  - Message: `feat(water): LOD water surface layer from the recipe, outside the terrain fingerprint`

### Task 9: Stop writing water into chunks

**Files:**
- Modify: `crates/worldgen/src/lib.rs`.
  - Block fill `:667-671`: `water_material` writes **ice only** (`by >= c.ice_from`). Every
    other cell up to `water_top` stays air.
  - Keep `hi` covering `water_top`, so ice sub-chunks are materialised.
- Modify: `crates/game/src/water.rs`: `water_units_at` drops the liquid-block clause.
- Modify: `crates/worldgen/tests/scene_hash_golden.rs` (re-pin).
- Modify: the worldgen tests that assert water blocks (find them with
  `grep -rn "water" crates/worldgen/tests`).
- Modify: `crates/server/src/region_store.rs` (`REGION_FILE_VERSION` +1 from the value in the tree).
- Test: `crates/server/tests/` region-store tests, `crates/game/tests/region_from_columns.rs`.

**Interfaces:**
- Consumes: everything above.
- Produces: chunks hold no liquid blocks. An all-water chunk becomes air and is no longer
  materialised.

- [ ] **Step 1: Write the failing tests**
  - worldgen, `no_liquid_blocks_in_generated_chunks`: over a forged coastal region and a
    procedural sea region, no generated block `is_liquid`, and `water_units_at > 0` on every
    cell of the previous test's expected recipe ranges.
  - Server region-store test `old_region_file_version_is_refused`: a header with the previous
    version (current − 1) decodes to the version error (`.bak` path).
  - Re-assert `edits_follow_static_water_semantics` (Task 5) on a generated region: digging the
    seabed leaves a dry cell.
- [ ] **Step 2: Run them and confirm they fail**
  - Run `just test-one worldgen "--test it -- --include-ignored water_recipe::"`.
  - Run `just test-one server "--test region_store"` (use the actual target name of the
    region-store tests).
  - Expected: FAIL.
- [ ] **Step 3: Implement**
  - The worldgen cache keys on `crates/worldgen/src`, so it invalidates itself.
- [ ] **Step 4: Re-run Step 2's commands**
  - Also run:
    - `just test-one worldgen "--test scene_hash_golden"` (re-pin, with the reason: no water
      blocks);
    - `just test-one game "--test it -- --include-ignored adventure::"`;
    - `just test-one game "--test it item_piles::"`;
    - `just test-one game "--test it slime::"`;
    - `just test-threads`;
    - `just smoke`.
  - Expected: PASS.
- [ ] **Step 5: Run `just determinism-wasm`**
  - Expected: PASS, with re-pins explained.
- [ ] **Step 6: Check live, with Dawid's OK**
  - `just adapter=Intel run --reset`.
  - Expected: the embarked world loads, the coast looks as after Task 8, and the adventurer
    swims in the sea and climbs out.
- [ ] **Step 7: Commit**
  - Message: `feat(water): water leaves chunk block data — recipe water only`

### Task 10: Documentation

**Files:**
- Modify: `CLAUDE.md` (worldgen, game, client and LOD paragraphs).
- Modify: `TODO.md`.
- Modify: `docs/superpowers/specs/2026-11-06-water-revisit/00-synthesis.md` (status line).

- [ ] **Step 1: Update CLAUDE.md**
  - Describe the recipe water rule and `ColumnGround.water_top` / `water_body`, and
    `GameData.water_bodies`.
  - Describe `water_units_at` as the single query, `Immersion` and the swim constants, and the
    dynamic-body buoyancy (`buoyancy.rs`).
  - Describe `BlockKind::Liquid` / `blocks_ray`, the DF depth rules in pathfinding, and the
    `renderer/water/` pass (Mesh + `WaterMaterial`, tiles, cut discard).
  - Describe `LodWater` in `LodData` and the `REGION_FILE_VERSION` bump.
  - Remove the old "water counts as opaque" and the claims that worldgen writes water blocks.
- [ ] **Step 2: Update TODO.md**
  - Record the static limits: dug pockets under water stay dry; placed blocks don't relevel;
    no drowning; no currents.
  - Record what to measure before the flow plans:
    - a `PERF_LOG=1` embarked run at 256 m, which Dawid runs himself;
    - the body-restructure budget prototype round.
- [ ] **Step 3: Commit**
  - Message: `docs(water): static water layer in CLAUDE.md and TODO.md`

---

## Hooks for the flow plans (not built here)

The landed seam code settles questions the water research left open. Plan the flow phases on top
of it:

- **Seam flux:** a water flux leaves like a departure: a stamped `RegionOutput` delivered as a
  `RegionInput::Transfer(kind, stamp)` and applied at `max(T+1, the receiver's next tick)` in the
  receiver's event stream. Level updates for a seam link ride the halo's stamped deltas (seam
  spec §3.6: "fluid deltas are halo deltas").
- **Conservation in flight:** add water volume to the manager's `in_flight` table and its
  invariant (a transfer is in exactly one of source save, `in_flight` or target save). Then every
  unit is in exactly one region or one transfer, autosave included.
- **Parked neighbours:** `WorldManager::pending_arrivals` (non-waking arrivals wait for their
  region to run, capped per region) is the pending-volume ledger the synthesis asked for. Water
  must never be dropped by its overflow rule; give it its own uncapped ledger entry, or merge
  the entries per region.
- **Cadence:** `WorldClock` numbers ticks the same in every region, and catch-up never skips a
  tick number. So "step water when `tick % WATER_STEP_TICKS == 0`" is the same tick in every
  region, and a lagging region still runs every step.
- **Open decision 2 (client-predicted seam arrivals):** revisit it. The research recommended
  not predicting them because relayed transfers orphaned. Predicted transfers now land in the
  server's slot, carry an identity, and are conservation-checked, so prediction may now be safe.
- **Intents:** pumps, gates and buckets acting on water a region does not own are `Effects`
  intents (`intents.rs`), applied at the opening of T+1 in the owner, in `(due, source
  EntityId, seq)` order.
