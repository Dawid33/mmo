# Fortress Selection, Designation and Construction UI — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In the commander view, the player picks a working level (Alt+wheel), sees the world cut away above it, drags single- or multi-level boxes to designate dig / deconstruct / build-wall / build-floor / erase, and sees those designations drawn in the world. The orders are deterministic, rollback-safe sim state.

**Architecture:** The pure order rules (`game::orders`: types, validation, drag-to-box resolution) are shared by the sim and the client.
- **Sim:** the sim stores designations and constructed-tile records as `#[rollback]` maps in `GameData`, and applies a new `GameEventKind::Order` event.
- **Client mirror and tools:** the client keeps a main-thread `TileMirror` of the home region's tiles, so picking and validity previews are synchronous and use the same rules.
- **Rendering:** a cutaway clip plane and a per-cell overlay texture are added to the pulled-quad voxel shader. Construction ghosts and drag boxes are Bevy gizmos.
- **UI:** the toolbar and readouts are Slint.

No workers exist yet: designations persist until cancelled, or until a dev-only "complete all" order applies them instantly.

**Tech Stack:** Rust (stable 1.96), `game` crate (`#[rollback]` macro state, `StateHash`), Bevy 0.19.1 client (pulled-quad renderer `voxel_pull`, WGSL), Slint (`crates/slint-ui`), egui dev panel.

**Spec:** `docs/superpowers/specs/2026-10-29-voxel-selection-construction-ui/00-synthesis.md` (decisions in §6; research in 01–03 of the same folder; DF context in `docs/superpowers/specs/2026-10-20-dwarf-fortress-research/engine-mapping.md` §8).

## Before starting

1. The working tree has **Dawid's uncommitted boundary-overlay work** (`assets/shaders/voxel_pull.wgsl`, `renderer/boundaries.rs`, `voxel_pull/{pipeline,prepare,arena,mod}.rs`, `dev_ui.rs`, `renderer/mod.rs`), plus edits to `CLAUDE.md` and `scripts/run.sh`. Tasks 6, 7 and 9 edit the same files. **Ask Dawid to commit (or stash) that work first; never commit it for him.** The boundary overlay takes binding 5 of `@group(1)`; this plan adds bindings 6 and 7 after it, and `BoundaryUniform` is the pattern to copy.
2. Client unit tests are a slow opt-in (CLAUDE.md). **Ask Dawid once, before Task 4, whether running `just test-one client "--lib <filter>"` is OK.** If he says no, write the tests anyway and verify with `just smoke` (which type-checks them).
3. Live runs (`just adapter=Intel run`, `just pull-smoke`) launch the GPU on his machine: ask before each.
4. Work on `develop` in the main checkout. No worktrees.

## Global Constraints

- `game` and `server` stay Bevy-free and windowing-free; all order rules live in `game::orders`.
- Determinism: no `HashMap`/`HashSet` iteration in sim code, and no f32 transcendentals in sim paths. Every new sim type derives `StateHash`, `serde::{Serialize, Deserialize}`, `Clone`, `Debug`, `PartialEq`.
- Rollback bar: `state_hash(before) == state_hash(after rollback_last_event())`, bit-exact, for every `Order` variant.
- Tile = one 1 m block; `TilePos` = region-local block coordinates `[x, y, z]` (`game::BlockPos`, sim units / `BLOCK_VOXELS`). A level is a block `y`.
- One race: dwarves, `DWARF_HEIGHT_LEVELS: i32 = 2` (named constant in `game::orders`).
- Priorities `1..=7`, `DEFAULT_PRIORITY: u8 = 4` (DF v50).
- `MAX_ORDER_TILES: u64 = 65_536`: a `Designate`/`Erase` order whose clipped area exceeds it is rejected whole, with no state change.
- Construction material: `game::edit::PLACE_BLOCK` (no material choice yet).
- Never write the region edge as a literal: derive from `REGION_SIZE` / `REGION_CHUNKS` / `CHUNK_BLOCKS`.
- Fortress UI works in **legacy regions only** (`cells.cell_chunks == 0`). In cell mode the mirror, overlay and tools stay disabled.
- Input (spec §4/§6): **Alt+wheel** = working level ±1, Alt+Shift+wheel ±10, PgUp/PgDn ±1, works mid-drag. The plain wheel stays zoom. Esc or an RMB click (release within `DRAG_THRESHOLD_PX`) cancels; an RMB drag still orbits. Ctrl+drag erases. Ctrl+Z cancels your last order. Keys 1–7 set priority while a tool is active.
- Player-facing UI goes in Slint (`crates/slint-ui`); dev-only controls go in the egui F3 panel (`renderer/dev_ui.rs`).
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Stage only the files your task names.
- Verify with `just test-one …` for the named targets and `just smoke`. Never run whole suites.

## Review Focus

1. **Alt stuck after Alt+Tab.** If the window loses focus with Alt down, the wheel must go back to zooming on return. `wheel_routes_to_level` requires focus (Task 5 test `wheel_needs_focus_and_alt`).
2. **Huge or off-region boxes.** A drag across the whole region with many levels, or partly outside the region, must clip to the region and either apply or be rejected whole by the tile cap, never panic or partially apply (Task 2 tests `order_over_cap_is_rejected_whole` and `area_is_clipped_to_region`).
3. **Re-applied predictions.** The client replays the same `Order` after a rollback. Applying it again must give identical state, and `CancelOp` must only remove tiles still carrying that op after a later order overwrote some (Task 2 test `cancel_op_spares_tiles_overwritten_by_a_later_op`).
4. **Stale designations.** A dig designation whose tile was dug by other means (the first-person `edit.rs` path, or another order) must not change terrain when completed. It is just dropped (Task 2 test `dev_complete_drops_stale_designations`).
5. **Working level out of range.** Stepping past the deepest ground or far above the surface must clamp, not wander into empty space (Task 5 test `level_clamps_to_ground_range`).

---

### Task 1: `game::orders` — types, validation rules, drag resolution

**Files:**
- Create: `crates/game/src/orders.rs`
- Modify: `crates/game/src/lib.rs` (add `mod orders; pub use orders::*;` like the other modules)
- Modify: `crates/game/src/edit.rs` (factor two pure helpers out; behaviour unchanged)
- Modify: `crates/game/src/state.rs:703` (`GameData::implied_block` calls the pure helper)
- Test: `crates/game/tests/orders.rs` (new)

**Interfaces — Produces:**
```rust
pub type TilePos = crate::BlockPos; // [i32; 3], region-local blocks
pub const DWARF_HEIGHT_LEVELS: i32 = 2;
pub const DEFAULT_PRIORITY: u8 = 4;
pub const MAX_PRIORITY: u8 = 7;
pub const MAX_ORDER_TILES: u64 = 65_536;

pub struct OpId { pub client: ClientId, pub seq: u32 }            // + Copy, Ord, Hash
pub enum DesignationKind { Dig, Deconstruct, Build(BlockId) }     // + Copy, Eq
impl DesignationKind { pub fn same_tool(self, other: Self) -> bool } // Build(_) == Build(_)
pub struct Designation { pub kind: DesignationKind, pub op: OpId, pub priority: u8 }
pub struct TileBox { pub min: TilePos, pub max: TilePos }         // inclusive; + Copy, Eq
impl TileBox {
    pub fn new(a: TilePos, b: TilePos) -> Self;                   // component-wise min/max
    pub fn tile_count(&self) -> u64;
    pub fn contains(&self, p: TilePos) -> bool;
    pub fn tiles(&self) -> impl Iterator<Item = TilePos>;         // y, then z, then x ascending
    pub fn clip_to_region(&self) -> Option<TileBox>;              // x,z to 0..REGION_BLOCKS; y untouched
}
pub const REGION_BLOCKS: i32; // REGION_CHUNKS * CHUNK_BLOCKS
pub enum Order {
    Designate { op: OpId, kind: DesignationKind, priority: u8, area: TileBox },
    Erase { area: TileBox, only: Option<DesignationKind> },       // None = every kind
    CancelOp(OpId),
    DevCompleteAll,
}
pub enum Invalid { Bedrock, NothingToDig, Built, RegionEdge, Occupied, NotBuilt } // + Ord (summary keys)
pub trait TileSource {
    fn block(&self, p: TilePos) -> BlockId;      // AIR when unknown
    fn is_bedrock(&self, p: TilePos) -> bool;
    fn edge_locked(&self, p: TilePos) -> bool;   // edit.rs's outer-ring-below-surface refusal
    fn is_constructed(&self, p: TilePos) -> bool;
}
pub fn validate(src: &impl TileSource, kind: DesignationKind, p: TilePos) -> Result<(), Invalid>;
pub struct OrderSummary { pub valid: u64, pub invalid: BTreeMap<Invalid, u64> }
pub fn summarize(src: &impl TileSource, kind: DesignationKind, area: TileBox) -> OrderSummary;
pub enum DragShape { Box, Wall, Floor }
pub fn resolve_drag(shape: DragShape, anchor: TilePos, corner: TilePos) -> TileBox;
```
Pure helpers factored out of existing code (exact names; same behaviour):
- `crate::ground::implied_block(ground: Option<&ColumnGround>, coords: ChunkCoords, local: (usize, usize, usize)) -> Option<Block>`: the body of `GameData::implied_block` after its cell-mode and `chunk_index` early returns.
- `crate::edit::edge_ring_locked(pos: BlockPos, ground: Option<&ColumnGround>) -> bool`: the condition inside `breaks_into_neighbour_ground`.
- `crate::edit::is_bedrock_at(ground: Option<&ColumnGround>, pos: BlockPos, block: Block) -> bool`: the condition inside `is_bedrock`.

Validation rules, checked in this order:
- **Dig:** air → `NothingToDig`; constructed → `Built`; bedrock → `Bedrock`; edge-locked → `RegionEdge`.
- **Deconstruct:** not constructed → `NotBuilt`.
- **Build:** any non-air block (including water) → `Occupied`.

`resolve_drag`:
- `Box`: `TileBox::new(anchor, corner)`.
- `Wall`: when `anchor[1] == corner[1]`, `max.y = min.y + DWARF_HEIGHT_LEVELS - 1`; otherwise the explicit range.
- `Floor`: both corners at `y = min(anchor.y, corner.y) - 1`; the floor goes under the level you stand on.

- [ ] **Step 1: Write the failing tests** in `crates/game/tests/orders.rs`, with a test `TileSource` (a `BTreeMap<TilePos, BlockId>` plus bedrock/edge/constructed `BTreeSet`s):
  - `tiles_iterate_y_then_z_then_x`: `TileBox::new([1,0,0],[0,1,1]).tiles()` yields 8 positions starting `[0,0,0],[1,0,0],[0,0,1]`.
  - `tile_count_and_clip`: a box `[-5,0,-5]..[3,2,3]` clips to `[0,0,0]..[3,2,3]` with `tile_count() == 4*3*4`; a box entirely at `x < 0` clips to `None`; a box past `REGION_BLOCKS` clips to `max.x == REGION_BLOCKS - 1`.
  - `dig_rules`: air → `Err(NothingToDig)`, stone → `Ok`, constructed stone → `Err(Built)`, bedrock → `Err(Bedrock)`, edge → `Err(RegionEdge)`.
  - `deconstruct_needs_a_construction` and `build_needs_air` (water id counts as occupied).
  - `wall_drag_on_one_level_extrudes_dwarf_height`: `resolve_drag(Wall, [2,10,2], [5,10,3])` == `TileBox{min:[2,10,2], max:[5,11,3]}`.
  - `wall_drag_across_levels_keeps_explicit_range`: `([2,10,2],[5,13,3])` → `max.y == 13`.
  - `floor_drag_goes_under_the_level`: `([0,10,0],[3,12,3])` → `min.y == max.y == 9`.
  - `summary_counts_by_reason`: a 3-tile box with stone, air and bedrock under `Dig` → `valid == 1`, `invalid == {NothingToDig: 1, Bedrock: 1}`.
- [ ] **Step 2: Run** `just test-one game "--test orders"`. Expected: compile failure (module missing).
- [ ] **Step 3: Implement** `orders.rs` with the interfaces above, and factor out the three helpers.
- [ ] **Step 4: Run** `just test-one game "--test orders"` → PASS. Then `just test-one game "--test underground_edit"` → PASS (the factored helpers keep behaviour).
- [ ] **Step 5: Commit** `feat(game): order types, tile validation and drag resolution`.

---

### Task 2: Sim state, `Rollback::apply_order_safe` and the `Order` event

**Files:**
- Modify: `crates/game/src/state.rs`: add two `GameData` fields after `ground`, add two `GameDataUpdateKind` variants, and add read accessors.
- Modify: `crates/game/src/orders.rs`: add `impl TileSource for Rollback` and `Rollback::apply_order_safe`.
- Modify: `crates/game/tests/wasm_determinism.rs`: re-pin the constants (new empty fields change bytes and hash).
- Modify: `crates/game/src/protocol.rs` (`GameEventKind` at line 410, `origin_client` at line 430, `matches_prediction`)
- Modify: `crates/game/src/region.rs` (priority arm at line 102, debug label at line 146, handling arm next to `UiAction` at line 616)
- Modify: `crates/game/src/world_manager.rs:479` (route like `UiAction`)
- Modify: `crates/client/src/instance.rs:668-672` (same treatment as `UiAction`)
- Modify: `crates/client/src/renderer/bridge.rs` (no-op match arms for the two new update kinds; Task 4 fills them in)
- Test: `crates/game/tests/orders.rs`, `crates/game/tests/world_manager.rs`

**Interfaces:**
- Consumes: everything from Task 1.
- Produces:
```rust
// GameData fields (inside the #[rollback] struct):
#[undo(map)] designations: BTreeMap<TilePos, Designation>,
#[undo(map)] constructed: BTreeMap<TilePos, ()>,
// accessors on GameData:
pub fn designations(&self) -> &BTreeMap<TilePos, Designation>;
pub fn constructed(&self) -> &BTreeMap<TilePos, ()>;
pub fn ground_recipes(&self) -> &BTreeMap<ColumnCoords, ColumnGround>;  // for the client mirror
// GameDataUpdateKind variants:
SetDesignations(Vec<(TilePos, Option<Designation>)>),
SetConstructed(Vec<(TilePos, bool)>),
// on Rollback:
pub fn apply_order_safe(&mut self, client: ClientId, order: Order) -> bool; // false = rejected, nothing changed
// protocol.rs:
GameEventKind::Order(ClientId, crate::Order)
```
Every match that lists `UiAction` lists `Order` beside it with the same behaviour. The `region.rs` arm calls `self.data.apply_order_safe(client, order)`. If the server overwrites a client id on ingress for `UiAction`, do the same for `Order`.
Semantics:
- `Designate`:
  - **Reject** if `op.client != client`, if `priority` is not in `1..=MAX_PRIORITY`, if the area clips to `None`, or if the clipped `tile_count() > MAX_ORDER_TILES`.
  - Otherwise, for each tile in `tiles()` order: if `validate` is Ok, `designations.insert(p, Designation{..})`. This overwrites any existing designation.
- `Erase`: reject if the clipped area exceeds the cap. Remove every designation inside the area whose kind matches `only` (`same_tool`), or all kinds when `only` is `None`. Iterate the designation map, not the area.
- `CancelOp(op)`: reject if `op.client != client`. Remove every designation whose `op == op`.
- `DevCompleteAll`:
  - Process `Build` designations in ascending y, then `Dig`/`Deconstruct` in descending y (top-down), each group in `BTreeMap` order.
  - Re-validate each tile. If still valid: Build → `set_block_safe(p, Block::new(id))` and insert into `constructed`; Dig/Deconstruct → `set_block_safe(p, AIR)` and remove from `constructed`.
  - If invalid, change nothing. Remove every designation.
- Render updates: after a mutation, `self.data.send(GameDataUpdate::new(Do, SetDesignations(changes)))`, then `emit_on_undo` with the inverse list (prior values). Do the same for `SetConstructed`. Follow `set_block_uncounted` in `edit.rs:351-358` for the pattern.

- [ ] **Step 1: Write the failing tests** in `orders.rs`. Build the region the way `underground_edit.rs` builds its `region()` (copy that helper and `column()`), then `CreateClient(0)` + `forget_last_event()`. Every test drives `region.handle_event(GameEventKind::Order(0, …))`, and each mutating test asserts `state_hash` equality after `region.rollback_last_event()`.
  - `designate_dig_marks_only_valid_tiles_and_rolls_back`: a 4×1×4 box over the stone band at the surface level marks 16 tiles; an extra air layer above adds 0.
  - `order_over_cap_is_rejected_whole`: a box with `MAX_ORDER_TILES + 1` tiles → returns `false`, `designations().is_empty()`, hash unchanged.
  - `area_is_clipped_to_region`: a box from `[-4,y,-4]` to `[3,y,3]` marks only the 16 in-region tiles.
  - `spoofed_op_client_is_rejected`: `Order(0, Designate{op: OpId{client: 1, ..}})` → no change.
  - `bad_priority_is_rejected` (0 and 8).
  - `reapplying_the_same_order_is_identical`: apply A, hash h1, roll back, apply A again → h1.
  - `cancel_op_spares_tiles_overwritten_by_a_later_op`: op 1 digs a 4×4, op 2 digs a 2×2 inside it; `CancelOp(op1)` leaves exactly the 4 tiles of op 2.
  - `erase_filters_by_kind`: Dig + Build designations in one box; `Erase{only: Some(Dig)}` leaves the Build ones.
  - `dev_complete_builds_a_two_level_wall_then_deconstructs_it`: a `Designate Build` over `resolve_drag(Wall, …)` on air, then `DevCompleteAll` → both levels solid and in `constructed()`. Then `Designate Deconstruct` over both levels, then `DevCompleteAll` → air again, `constructed()` empty, hash equal after each rollback.
  - `dev_complete_drops_stale_designations`: designate dig on stone, `set_block_safe` that tile to AIR, `DevCompleteAll` → designation gone, no other block changed.
  - `dig_box_down_to_bedrock_skips_bedrock`: a dig box from the surface down to the bedrock level marks no bedrock tile, and `DevCompleteAll` leaves every bedrock block in place.
  - `order_event_routes_to_the_home_region` (in `world_manager.rs`, modelled on that file's existing `UiAction`/`PlayerInput` routing test): a client's `Order(Designate)` sent to the manager lands in its home region, and `designations()` there is non-empty after the tick.
- [ ] **Step 2: Run** `just test-one game "--test orders"`. Expected: FAIL to compile (`GameEventKind::Order` / fields missing).
- [ ] **Step 3: Implement** the fields, accessors, update kinds, `TileSource for Rollback` (`block` = `current_block` mapped to its id, AIR when `None`; the others call the Task 1 helpers and the `constructed` map), `apply_order_safe`, and the event variant with all its match arms.
- [ ] **Step 4: Run** `just test-one game "--test orders"` and `just test-one game "--test world_manager order_event"` → PASS.
- [ ] **Step 5: Run** `just determinism-wasm`. The region-scene constants in `wasm_determinism.rs` will now differ. Update them to the native values, re-run, and confirm wasm matches native. Then run `just smoke` → PASS.
- [ ] **Step 6: Commit** `feat(game): designations and constructions as rollback state, Order event`.

---

### Task 3: Shared ray walk and level picking

**Files:**
- Modify: `crates/game/src/edit.rs:185-235` (extract the DDA)
- Modify: `crates/game/src/orders.rs` (add `pick_level_cell`)
- Test: `crates/game/tests/orders.rs`, plus `crates/game/tests/block_edit.rs` as a regression check

**Interfaces — Produces:**
```rust
// edit.rs, pure; raycast_block_in becomes a call to it with |p| self.block_solid(p)
pub fn raycast_blocks(origin: Vector, dir: Vector, reach: f32, solid: impl Fn(BlockPos) -> bool) -> Option<BlockHit>;
// orders.rs
pub fn pick_level_cell(origin: Vector, dir: Vector, level: i32, solid: impl Fn(BlockPos) -> bool) -> Option<TilePos>;
```
`origin` and `dir` are in region-local sim units. `pick_level_cell` (DF's "tile under the cursor at the current level"):
1. Intersect the ray with the cut plane `y = (level + 1) * BLOCK_VOXELS`. Return `None` if the ray points up or is parallel.
2. Walk with `raycast_blocks` from that point (reach `(level + 1 + UNDERGROUND_DEPTH) * BLOCK_VOXELS` sim units, enough to reach bedrock).
3. Hit at `y == level` → that cell (a wall or rock cap). Hit at `y == level - 1` → `[x, level, z]` (the floor you see). Hit deeper, or no hit → the cell where the ray crosses `y = level * BLOCK_VOXELS`, at `y = level` (open space: clamp to the level).

- [ ] **Step 1: Write the failing tests** (solidity from a `BTreeSet<TilePos>`; ray straight down plus a 55° pitch case):
  - `pick_hits_rock_cap_on_the_level`: solid at `[3,10,3]`, ray down through x=3.5, z=3.5 → `[3,10,3]`.
  - `pick_sees_the_floor_below_an_air_cell`: solid only at `[3,9,3]` → `[3,10,3]`.
  - `pick_clamps_open_space_to_the_level`: nothing solid; a 55° ray → the cell where it crosses `y = 10*16`.
  - `pick_ignores_blocks_above_the_cut`: solid at `[3,11,3]` and `[3,10,3]` → `[3,10,3]`.
- [ ] **Step 2: Run** `just test-one game "--test orders pick_"` → FAIL.
- [ ] **Step 3: Implement** both functions.
- [ ] **Step 4: Run** `just test-one game "--test orders pick_"` and `just test-one game "--test block_edit"` → PASS.
- [ ] **Step 5: Commit** `feat(game): shared block ray walk and level picking`.

---

### Task 4: Client `TileMirror` of the home region

**Files:**
- Create: `crates/client/src/renderer/commander/tiles.rs`
- Modify: `crates/client/src/renderer/commander/mod.rs` (register the module, resource and systems)
- Modify: `crates/client/src/renderer/bridge.rs`: in the `NewRegion` arm (around line 495), write a `TileMirrorMsg::Reset`; in the update match (near the `SetGhostSource` arm, line 885), forward `SetDesignations`/`SetConstructed`.

**Interfaces:**
- Consumes: `TileSource`, `Designation`, `TilePos`, `ground::implied_block`, `edit::{edge_ring_locked, is_bedrock_at}`, `GameData::{designations, constructed, ground_recipes}`.
- Produces:
```rust
#[derive(Message)] pub enum TileMirrorMsg {
    Reset { region: RegionId, ground: BTreeMap<ColumnCoords, ColumnGround>,
            designations: Vec<(TilePos, Designation)>, constructed: Vec<TilePos> },
    Designations { region: RegionId, changes: Vec<(TilePos, Option<Designation>)> },
    Constructed { region: RegionId, changes: Vec<(TilePos, bool)> },
}
#[derive(Resource, Default)] pub struct TileMirror { /* home region only */ }
impl TileMirror {
    pub fn region(&self) -> Option<RegionId>;
    pub fn designations(&self) -> &BTreeMap<TilePos, Designation>;
    pub fn version(&self) -> u64;                       // bumped on any change (overlay rebuild trigger)
    pub fn set_chunk(&mut self, chunk: [i32; 3], blocks: [BlockId; CHUNK_BLOCK_COUNT]);
    pub fn remove_chunk(&mut self, chunk: [i32; 3]);
}
impl TileSource for TileMirror { … }
pub fn chunk_block_ids(voxels: &VoxelData) -> [BlockId; CHUNK_BLOCK_COUNT]; // a block's id = its first non-air voxel's, else AIR
pub fn update_tile_mirror(/* Changed<VoxelData> + RemovedComponents like height.rs:173, + MessageReader<TileMirrorMsg>, + home region */);
```
Notes:
- The chunk key is region-local: `tf.translation / CHUNK_EDGE`. `height.rs` adds the world offset; we don't.
- Only the home region is mirrored (the region the HUD reports as home); a `Reset` for another region is ignored.
- `block()` uses the mirrored chunk if present, else `implied_block(ground.get(column), …)`, else AIR.
- Messages from regions other than the mirrored one are dropped.
- Disabled in cell mode (`game::CELL_CHUNKS != 0`).

- [ ] **Step 1: Write the failing unit tests** in `tiles.rs` `#[cfg(test)]`:
  - `chunk_block_ids_take_the_first_solid_voxel`: a `VoxelData` with one stone voxel in block 5 → `[AIR,…,STONE at 5,…]`.
  - `missing_chunk_below_surface_is_implied_ground`: Reset with a `ColumnGround::plain` recipe, no chunks → `block([0, surface-3, 0]) == STONE`, and `is_bedrock` at bedrock depth.
  - `mirrored_chunk_overrides_ground`: after `set_chunk` with all AIR, that block reads AIR.
  - `designation_messages_apply_and_bump_version`.
- [ ] **Step 2:** Ask Dawid for client-test permission (see "Before starting"). If granted, run `just test-one client "--lib commander::tiles"` → FAIL; otherwise run `just smoke` → FAIL to type-check.
- [ ] **Step 3: Implement** `tiles.rs` and the bridge forwarding.
- [ ] **Step 4: Run** the same command → PASS (or `just smoke` → PASS).
- [ ] **Step 5: Commit** `feat(client): home-region tile mirror for picking and validity`.

---

### Task 5: Working level and camera behaviour

**Files:**
- Create: `crates/client/src/renderer/commander/level.rs`
- Modify: `crates/client/src/renderer/commander/mod.rs`: in `pan_and_zoom` (line 485), skip zoom when the wheel routes to the level; in `follow_terrain` (line 497), follow the cut.
- Modify: `crates/client/src/renderer/commander/height.rs`: add `HeightGrid::surface_range() -> Option<(f32, f32)>` (min and max solid surface over loaded columns, sim units).

**Interfaces — Produces:**
```rust
pub const LEVEL_STEP_FAST: i32 = 10;
pub const LEVELS_ABOVE_SURFACE: i32 = 8;
#[derive(Resource, Default)] pub struct WorkingLevel { pub level: Option<i32>, pub cut: bool } // cut defaults to true
pub fn wheel_routes_to_level(alt: bool, focused: bool) -> bool;          // alt && focused
pub fn step_level(level: i32, notches: f32, fast: bool, range: (i32, i32)) -> i32; // ±1/±10 per notch, sign of notches, clamped
pub fn level_range(surface: (f32, f32)) -> (i32, i32); // (floor(min/16) - UNDERGROUND_DEPTH, floor(max/16) + LEVELS_ABOVE_SURFACE)
pub fn surface_level(surface_y: f32) -> i32;            // the air level above ground: floor(surface_y / 16)
pub fn cut_plane_world_y(level: i32, region: RegionId) -> f32; // ((level + 1) * 16) as f32 + region world offset y
pub fn follow_target(terrain: Option<f32>, cut: Option<f32>) -> Option<f32>; // min of the two when both exist
```
Systems:
- `init_level`: once the level is `None` and the height grid has the focus column → `surface_level`.
- `step_level_input`: Alt+wheel (Alt+Shift → fast), PgUp/PgDn ±1, Home → `surface_level` at the focus, `C` toggles `cut`. Not over Slint UI (`CursorPolicy::ui_pointer`) or the minimap.
- `pan_and_zoom` zooms only when `!wheel_routes_to_level(..)`.

- [ ] **Step 1: Write the failing unit tests** in `level.rs`:
  - `wheel_needs_focus_and_alt`: (true, true) → true; (true, false), (false, true) → false.
  - `alt_wheel_steps_one_or_ten`: `step_level(50, 1.0, false, (0,100)) == 51`; `(50, -2.0, true, …) == 30`.
  - `level_clamps_to_ground_range`: `step_level(5, -10.0, true, (0,100)) == 0`; the same at the top.
  - `level_range_spans_underground_depth`: surface (1024.0, 1600.0) → `(64 - 60, 100 + 8)`.
  - `follow_target_takes_the_lower_of_terrain_and_cut`.
- [ ] **Step 2: Run** `just test-one client "--lib commander::level"` (if permitted) → FAIL.
- [ ] **Step 3: Implement** `level.rs`, the zoom gate, the follow change and `surface_range`.
- [ ] **Step 4: Run** the same → PASS; then `just smoke` → PASS.
- [ ] **Step 5: Commit** `feat(client): working level on Alt+wheel, camera follows the cut`.

---

### Task 6: Cutaway in the pulled-quad renderer

**Files:**
- Create: `crates/client/src/renderer/fortress_view.rs` (copy the `boundaries.rs` resource/extract pattern)
- Modify: `crates/client/src/renderer/mod.rs` (register it, as `BoundaryUniform` is at line 101)
- Modify: `crates/client/src/renderer/voxel_pull/pipeline.rs`: binding 6 in the layout (next to line 46); a `cutaway: bool` field in `VoxelPullPipelineKey` (line 61) and `VoxelPullDepthPipelineKey` (line 203); when set, `cull_mode: None` and shader def `CUTAWAY`.
- Modify: `crates/client/src/renderer/voxel_pull/prepare.rs` (write the uniform next to `write_boundaries`, line 386; set the key bit from the uniform)
- Modify: `crates/client/src/renderer/voxel_pull/gpu.rs:263` (`cull` false while the cut is active), plus any CPU-side per-face-direction skip in the direct path (grep `face_ends`)
- Modify: `assets/shaders/voxel_pull.wgsl` (clip, cap, depth darkening)
- Test: the existing shader-validity tests in `voxel_pull/pipeline.rs:371-400`, extended to compose with `CUTAWAY`

**Interfaces:**
- Consumes: `WorkingLevel`, `cut_plane_world_y`, `TileMirror::region()`.
- Produces:
```rust
#[derive(Resource, Clone, Default, ExtractResource, ShaderType)]
pub struct FortressViewUniform {
    /// x: cut plane world y (f32::MAX = off); y: cap darkening 0..1; z: overlay on (0/1); w: unused
    pub cut: Vec4,
    /// xyz: home region world offset; w: overlay base level (block y of texture layer 0) as f32
    pub origin: Vec4,
    /// x: overlay width (blocks, region x), y: depth (region z), z: layers; w: unused
    pub overlay_dims: UVec4,
}
pub const CAP_DARKEN: f32 = 0.25;
pub const DEPTH_DARKEN_PER_LEVEL: f32 = 0.12;
pub const DEPTH_DARKEN_FLOOR: f32 = 0.35;
pub fn cutaway_active(u: &FortressViewUniform) -> bool; // cut.x < f32::MAX
```
Shader behaviour under `CUTAWAY`:
- **Vertex:** a quad whose lowest corner is above `cut.x` is collapsed (degenerate output).
- **Fragment:** discard when `world_position.y > cut.x`.
- **Cap:** a back-facing fragment is the cap. Every visible back face is one the eye ray reaches after crossing the cut plane inside solid, because the faces above the cut are gone. Shade it at the ray ∩ plane point: the quad's material top layer, normal +Y, × `1 - CAP_DARKEN`. Depth stays at the back face; no `frag_depth`, which preserves early-z for the rest.
- **Depth darkening:** front faces below the cut are darkened by `max(DEPTH_DARKEN_FLOOR, 1 - DEPTH_DARKEN_PER_LEVEL * levels_below)`.
- The depth/shadow pipelines discard above the cut too.

Mirror the constants as WGSL `const`s with a comment naming `fortress_view.rs` as the source of truth. LOD rings (`far_voxel`) are unaffected. The per-chunk Mesh fallback (WebGL2) has no cutaway: document that in the module doc.

- [ ] **Step 1: Write the failing tests:**
  - in `pipeline.rs`, `forward_shader_is_valid_wgsl_with_cutaway` and `depth_shader_is_valid_wgsl_with_cutaway`, composing with the `CUTAWAY` def (copy the existing tests' import stubs);
  - in `fortress_view.rs`, `uniform_is_off_by_default` (`cutaway_active(&Default::default()) == false` once `Default` sets `cut.x = f32::MAX`) and `uniform_tracks_working_level` (level 10 in region (0,0) → `cut.x == 176.0`).
- [ ] **Step 2: Run** `just test-one client "--lib voxel_pull::pipeline"` and `"--lib fortress_view"` (if permitted) → FAIL.
- [ ] **Step 3: Implement** the uniform, binding, key bit, face-cull gate and shader.
- [ ] **Step 4: Run** both → PASS; then `just smoke` → PASS.
- [ ] **Step 5: Ask Dawid** to run `just adapter=Intel run` (or `just pull-smoke`). Check that Alt+wheel underground shows rock caps at the cut and tunnels below, with no holes at chunk borders. He may also want a `just render-bench` A/B with the cut on vs off, since back faces are rasterized while the cut is active.
- [ ] **Step 6: Commit** `feat(client): z-level cutaway (clip + back-face cap) in the pulled-quad path`.

---

### Task 7: Designation overlay texture

**Files:**
- Modify: `crates/client/src/renderer/fortress_view.rs` (the CPU grid, its rebuild system, and the render-world texture + upload)
- Modify: `crates/client/src/renderer/voxel_pull/pipeline.rs` (binding 7: `texture_3d<u32>`, R8Uint, fragment, no sampler)
- Modify: `crates/client/src/renderer/voxel_pull/prepare.rs` (bind it, upload the dirty sub-box)
- Modify: `assets/shaders/voxel_pull.wgsl` (overlay lookup and patterns)

**Interfaces:**
- Consumes: `TileMirror` (designations, version), `WorkingLevel`, `game::orders::{Designation, DesignationKind, Invalid, TileBox}`. `DragPreview` is defined here and stays empty until Task 8 fills it.
- Produces:
```rust
pub const OVERLAY_LEVELS: u32 = 16;                 // layers: working level and the 15 below
pub const OVERLAY_DIG: u8 = 1;
pub const OVERLAY_DECONSTRUCT: u8 = 2;
pub const OVERLAY_PREVIEW: u8 = 1 << 4;
pub const OVERLAY_INVALID: u8 = 1 << 5;
pub fn overlay_byte(d: Option<&Designation>, preview: Option<Result<(), Invalid>>) -> u8;
#[derive(Resource, Default)] pub struct DragPreview { pub area: Option<TileBox>, pub kind: Option<DesignationKind>, pub cells: Vec<(TilePos, Result<(), Invalid>)> }
#[derive(Resource)] pub struct OverlayGrid { pub base_level: i32, pub dims: UVec3, pub bytes: Vec<u8>, pub dirty: Option<(UVec3, UVec3)> }
```
Details:
- Build designations get no face tint (they sit in air). Task 8 draws them as gizmo ghosts.
- `overlay_byte` puts the kind in bits 0–2 (Build → 0) and sets PREVIEW, plus INVALID when the preview result is `Err`.
- The texture is `REGION_BLOCKS × REGION_BLOCKS × OVERLAY_LEVELS`, indexed `(x, z, y - base_level)`, so 1 MiB at 256 m regions.
- The grid rebuilds when `TileMirror::version`, the level or the preview changes. A level change uploads the whole texture; otherwise only the dirty box.
- **Shader:** the cell is `floor((world_pos - origin.xyz - normal * 0.5) / 16)`; cap fragments use the plane point minus `(0, 0.5, 0)`. Dig = amber diagonal stripes, period 0.25 m world-space. Deconstruct = red cross-hatch. PREVIEW = +35 % white. INVALID = 50 % red.

- [ ] **Step 1: Write the failing tests:**
  - in `fortress_view.rs`, `overlay_byte_encodes_kind_and_preview_bits` (dig → 1; deconstruct + Ok preview → `2 | 16`; none + Err → `16 | 32`; build → 0);
  - `grid_index_is_x_z_layer` (tile `[3, base+2, 5]` → byte index `3 + 5*W + 2*W*D`);
  - `grid_marks_dirty_box_for_a_single_change`;
  - in `pipeline.rs`, extend the cutaway shader tests so binding 7 is in the composed source.
- [ ] **Step 2: Run** `just test-one client "--lib fortress_view"` and `"--lib voxel_pull::pipeline"` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** both → PASS; `just smoke` → PASS.
- [ ] **Step 5: Commit** `feat(client): per-cell designation overlay in the voxel shader`.

---

### Task 8: Designation tools (input state machine, gizmos, orders)

**Files:**
- Create: `crates/client/src/renderer/commander/designate.rs`
- Modify: `crates/client/src/renderer/commander/selection.rs`: entity selection systems `run_if(no_tool_active)`.
- Modify: `crates/client/src/renderer/commander/mod.rs`: register the module; tool and priority keys must not collide with WASD/arrows/Space/Shift panning.

**Interfaces:**
- Consumes: `pick_level_cell`, `resolve_drag`, `summarize`, `validate`, `Order`, `OpId`, `TileMirror`, `WorkingLevel`, `DragPreview`, `GameEvents`/`LocalPlayer` (send pattern: `ui_input.rs::emit_action`), `DRAG_THRESHOLD_PX`.
- Produces:
```rust
pub enum Tool { Dig, Deconstruct, Wall, Floor, Erase }
pub const TOOL_KEYS: [(KeyCode, Tool); 5] = [(KeyG, Dig), (KeyX, Deconstruct), (KeyB, Wall), (KeyV, Floor), (KeyZ, Erase)];
pub const MAX_GHOST_BOXES: usize = 4096;
impl Tool { pub fn kind(self) -> Option<DesignationKind>; pub fn shape(self) -> DragShape; pub fn label(self) -> &'static str; pub fn key_label(self) -> &'static str }
#[derive(Resource)] pub struct DesignateState { pub tool: Option<Tool>, pub priority: u8, pub drag: Option<Drag>, pub hover: Option<TilePos>, pub next_seq: u32, pub sent: Vec<OpId> }
pub struct Drag { pub anchor: TilePos, pub corner: TilePos, pub erase: bool }
pub fn no_tool_active(state: Res<DesignateState>) -> bool;
pub fn order_for(tool: Tool, area: TileBox, erase: bool, op: OpId, priority: u8) -> Order;
// Erase tool, or Ctrl held at release → Order::Erase { area, only: tool.kind() (None for Erase) }
pub fn merge_runs(tiles: impl Iterator<Item = TilePos>) -> Vec<TileBox>; // merges x-runs per (y, z); input in TileBox::tiles() order
pub fn drag_text(area: TileBox, kind: Option<DesignationKind>, summary: &OrderSummary) -> String;
// e.g. "4 × 3 × 2 — 20 to dig · 3 bedrock · 1 region edge"; size is x × z × levels
```
Behaviour:
- **Hover:** each frame, ray from the cursor (camera viewport ray minus the home region's world offset) → `pick_level_cell(…, level, |p| mirror.block(p) != AIR)`.
- **Drag:**
  - LMB press with a tool, not over UI or minimap → `drag = Some{anchor: hover, corner: hover, erase: ctrl}`.
  - While held, `corner = [hover.x, level, hover.z]`; Alt+wheel moves `level`, so the box spans levels.
  - Release → `area = resolve_drag(tool.shape(), anchor, corner)` → `Order` via `GameEventKind::Order(local, order)`. Push the op onto `sent`.
- **Cancel:** RMB released within `DRAG_THRESHOLD_PX` of its press, or Esc: drop the drag if there is one, otherwise clear the tool. An RMB drag still orbits.
- **Undo:** Ctrl+Z pops `sent` → `CancelOp`.
- **Priority:** Digit1..Digit7 set `priority` while a tool is active.
- **DragPreview:** while dragging, `area` + `cells` from `validate` over `area.tiles()`, capped at `MAX_ORDER_TILES`; beyond the cap the preview shows the box only and the readout says "too large".
- **Gizmos:**
  - The drag box is drawn twice: depth-tested and bright, then a second `GizmoConfigGroup` with `depth_bias = -1.0` and alpha 0.3, so hidden edges show dimmed.
  - Build designations are drawn as `merge_runs` cuboids (pale blue, capped at `MAX_GHOST_BOXES`) over levels `base_level ..= level + DWARF_HEIGHT_LEVELS`.
  - The hover cell gets an outline.

- [ ] **Step 1: Write the failing unit tests** in `designate.rs`:
  - `order_for_dig_and_ctrl_erase`: Dig + erase=false → `Designate{kind: Dig, priority, op}`; Dig + erase=true → `Erase{only: Some(Dig)}`; the Erase tool → `Erase{only: None}`.
  - `wall_tool_resolves_two_levels`: `Tool::Wall.shape()` with a same-level drag → 2 levels (this goes through `resolve_drag`).
  - `merge_runs_joins_x_runs`: tiles `[0..4, 0, 0]` and `[0..2, 0, 1]` → 2 boxes.
  - `drag_text_lists_reasons_in_order`: the example string above.
  - `tool_keys_avoid_camera_keys`: no `TOOL_KEYS` entry is W, A, S, D, Q, E, Space, ShiftLeft or an arrow key.
- [ ] **Step 2: Run** `just test-one client "--lib commander::designate"` → FAIL.
- [ ] **Step 3: Implement** the systems and pure functions.
- [ ] **Step 4: Run** the same → PASS; `just smoke` → PASS.
- [ ] **Step 5: Commit** `feat(client): designation tools with multi-level drags`.

---

### Task 9: Slint toolbar, readouts, dev "complete all", docs

**Files:**
- Modify: `crates/slint-ui/ui/game.slint`: a bottom-centre tool bar, a priority selector, a level readout and a drag readout near the cursor.
- Modify: `crates/client/src/renderer/slint_overlay.rs`: new `UiAction` variants and pushing the properties (follow `push_ui_state` at line 290 and `setup_slint_overlay` at line 105).
- Modify: `crates/client/src/renderer/dev_ui.rs`: an F3 button "Complete all orders" that sends `Order::DevCompleteAll`.
- Modify: `CLAUDE.md` (the commander-camera bullet in `crates/client`: tools, Alt+wheel, cutaway; one sentence on `game::orders` in the `crates/game` bullet) and `TODO.md` (next step: workers/jobs consume designations; stockpiles and zones next).

**Interfaces — Produces** (Slint):
```slint
in property <int> active-tool: -1;      // Tool index in TOOL_KEYS order; -1 = none
in property <int> priority: 4;
in property <string> level-text;        // "Level 64 · surface 70" (or "Level 58 · 12 below surface")
in property <string> drag-text;         // from designate::drag_text, "" when not dragging
in property <length> drag-x; in property <length> drag-y;   // readout position (cursor + 16 px)
callback tool-clicked(int);
callback priority-clicked(int);
```
Rust: `slint_overlay::UiAction::{SelectTool(Option<Tool>), SetPriority(u8)}`, plus a pure `pub fn level_text(level: i32, surface_level: Option<i32>) -> String`. Each button shows its hotkey, e.g. `[G] Dig`.

- [ ] **Step 1: Write the failing unit tests:** `level_text_above_and_below_surface` (`(64, Some(70))` → `"Level 64 · 6 below surface"`; `(72, Some(70))` → `"Level 72 · 2 above surface"`; `(70, Some(70))` → `"Level 70 · surface"`) and `ui_actions_map_tool_indices` (index 2 → `SelectTool(Some(Wall))`, -1 → `None`).
- [ ] **Step 2: Run** `just test-one client "--lib slint_overlay"` → FAIL.
- [ ] **Step 3: Implement** the Slint component changes, callback wiring, property pushes, the dev button, and the docs.
- [ ] **Step 4: Run** `just test-one client "--lib slint_overlay"` → PASS; `just smoke` → PASS.
- [ ] **Step 5: Ask Dawid** for a live check with `just adapter=Intel run`:
  - designate a 2-level wall (B, one drag), then F3 "Complete all orders" → a 2 m wall;
  - designate deconstruct across both levels (X, drag, Alt+wheel up one, release), then complete → the wall is gone;
  - dig into a hillside with G; Ctrl+Z undoes the last order; Esc and RMB-click cancel.
- [ ] **Step 6: Commit** `feat(client): fortress tool bar and readouts; docs`.

---

## Out of scope (later plans)

Dwarves, jobs and the top-down deconstruction scheduling that workers need; stockpiles and zones; hidden/unrevealed tiles; channel, stairs and ramps (no block shapes yet); material choice; "magic select" flood fill; blueprints/copy-paste; priority painting over existing designations; dimming pending (unconfirmed) orders; dithered voxel ghosts in the pulled-quad path (gizmo cuboids for now); face handles on existing boxes; cutaway on the WebGL2 per-chunk fallback; cell-mode regions; chisel construction; furniture and other two-level objects (whole-object picking, headroom validity); the "waiting: blocks above" reason (needs jobs).
