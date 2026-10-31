# Adventure Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A client can switch from fortress mode to adventure mode, which puts its player character
in the world under the commander camera. You select the character, hover to see the path it would
walk, right-click to send it there, and it can walk into other regions. The camera can follow any
selected entity and stays inside the regions around the character.

**Architecture:** The mode and the walk are **sim state**. `Client.mode: PlayMode` is changed by a
new `Order::SetMode`, and a right click sends `Order::MoveTo`. The sim pathfinds with a pure,
deterministic A* (`game::pathfind`) over `orders::TileSource`. Client and server both run it on
their own replicas, so they get the same path. The resulting `Walker` component steers the existing
body controller (`camera.rs` `CameraController`) in place of keyboard keys. Crossing a region uses
the existing player handoff, and `Walker` travels in the `EntityBundle`. The path preview is the same
`find_path` run on the client's `TileMirror` (it already implements `TileSource`). Follow and the
camera bounds are client-local state in `renderer/commander/`.

**Tech Stack:** Rust stable 1.99.1; `game` (rollback sim, rapier fork KCC), Bevy 0.19.1 `client`,
Slint (`crates/slint-ui/ui/game.slint`).

**Spec:** none. This plan works from the user's request of 2026-10-30, which says:
- Adventure mode can be switched on and spawns a player character, controlled from a third-person
  view (the commander camera).
- The camera can follow selected entities: a **Follow** button next to **Clear selection**.
- Adventure mode allows going to other regions. The camera stays in the character's region (and
  the active regions around it).
- The controls are select and click. With the player selected, a right click picks where it goes.
  Before the click, a pathfinding path is drawn from the character to the hovered point.

The Design decisions below fill in the rest. They are open to review.

## Design decisions

1. **Sim-side pathfinding instead of client-sent waypoints.** The client could compute the path and
   send waypoints, which is cheaper for the sim. But the sim would then trust client geometry, and
   the path would go stale when terrain changes. DF workers will need sim pathfinding anyway. A* is
   integer-only and deterministic, so prediction and server agree. The cost is bounded by
   `MAX_PATH_NODES` and paid once per order (plus one re-plan on arrival in a new region, and one
   when stuck), not per tick.
2. **The body always exists.** Commander mode already keeps an inert, hidden player body
   (`bridge.rs` `LocalBody`), and the manager's `homes` map is keyed on it. "Spawning" the
   character therefore means `SetMode(Adventure { at })`: the body is teleported to the ground
   under the camera focus and becomes visible and selectable. Switching back to fortress mode hides
   it where it stands.
3. **Paths are region-local.** A goal in another region is stored as `Goal { region, column, y }`.
   The source region plans to the edge tile facing the goal, then walks straight out until the
   handoff fires. The arriving region re-plans from wherever the body lands. The preview draws the
   home-region part plus a dashed straight line to a goal beyond the seam. At near radius 0 the
   client has no neighbouring blocks to plan over.
4. **Picking past loaded terrain.** `GroundPick` only hits terrain in the `HeightGrid` (the near
   window). A goal in a neighbouring region therefore falls back to a plane pick at the camera's
   ground height. That pick is column-only (`y: None`), and the target region resolves the height.
5. **Right button: click = order, drag = orbit.** A right press already starts an orbit drag. A
   release whose accumulated raw motion stays under `selection::DRAG_THRESHOLD_PX` is a move order
   aimed at the press position.
6. **Follow** pins the focus to the target, eased by `FOLLOW_SMOOTHING_S`. A manual pan (WASD, edge,
   middle-drag, minimap) ends it; zoom and orbit do not. The local player is followed by its
   `LocalBody` marker, so following survives handoff. For any other entity, following ends when the
   entity is pruned.
7. **Camera bounds = the near window**: the home rect grown by `WorldScale::near_radius` regions on
   each side. At the play default (radius 0) that is the home region. A home change caused by
   walking keeps the camera placed. Only a pending Play re-centres it, as today.
8. **Adventure mode hides the fortress tools** (`tools-available` false, designate input off),
   since designations belong to fortress mode. The working level and cutaway stay.

## Global Constraints

- Work on `develop` in the main checkout. **No git worktrees.** `git add` only the files a task
  changed, and run `git status` before each commit (Dawid has uncommitted WIP in the same files:
  `bridge.rs`, `commander/*`, `state.rs`, `protocol.rs`).
- `game` stays Bevy-free. No inherent `f32` transcendental in sim code: use `libm::` (`atan2f` for
  the walker yaw).
- Rollback bar: `hash(before) == hash(after undo)`. New sim state derives `StateHash`, `Serialize`
  and `Deserialize`, and is mutated only through `_safe` / undo-scope paths.
- Tunables are named `pub const`s with units in the name or doc (memory: configurable tunables).
- Verify with `just test-one …` and `just smoke`. Run `just determinism-wasm` after Tasks 1–4, and
  `just test-threads` after Task 4. Never run whole suites. Tag tests over 100 ms
  `#[ignore = "slow: >100ms — run via just test-full"]`. Client test binaries are slow opt-ins:
  ask before running one.
- Never write the region edge as a literal: derive it from `game::REGION_BLOCKS`.
- **Dependency:** if `docs/superpowers/plans/2026-10-29-entity-seam-crossing.md` has landed,
  `EntityBundle` carries a `Vec<Travelling>` bag. `Walker` then goes in the bag as a variant instead
  of a field (Task 4). Check `crates/game/src/protocol.rs` first.

## Review Focus

1. **Goal in water, on a 2-high wall or in a sealed pit:** no path. The preview shows it red, and the
   order is refused with no state change. Pinned in Task 1 (`unreachable_*`) and Task 3
   (`move_to_unreachable_changes_nothing`).
2. **Terrain dug or built under an active walk:** the body is blocked, gets one re-plan after
   `STUCK_TICKS`, then stops. It never jitters forever. Pinned in Task 3
   (`walker_replans_once_then_gives_up`).
3. **A rollback across a MoveTo**, where the server's order lands a tick later than the client
   predicted it: undo is bit-exact. Pinned in Task 3 (`move_to_undo_bit_exact`).
4. **A walk across a seam under scheduling jitter**, where the arriving region re-plans: the body
   ends at the goal in the neighbour region. Pinned in Task 4 (`walk_into_neighbour_region`, seeded
   layer).
5. **Following the own player through a handoff**, where its `(RegionId, EntityKey)` changes:
   selection and follow stay on the player. Pinned in Task 6 (`rekey_local_player`).

---

### Task 1: Deterministic pathfinder

**Files:**
- Create: `crates/game/src/pathfind.rs`
- Modify: `crates/game/src/lib.rs` (`mod pathfind; pub use pathfind::*;`, following the glob re-export pattern)
- Test: `crates/game/tests/pathfind.rs`

**Interfaces:**
- Consumes: `orders::{TileSource, TilePos}` (`TilePos = BlockPos { x, y, z: i32 }`, region-local).
- Produces:
  - `pub const MAX_PATH_NODES: u32 = 32_768;`, `pub const STEP_COST: u32 = 10;`, `pub const DIAG_COST: u32 = 14;`, `pub const CLIMB_COST: u32 = 5;`
  - `pub fn standable(src: &impl TileSource, p: TilePos) -> bool`
  - `pub fn find_path(src: &impl TileSource, from: TilePos, to: TilePos, max_nodes: u32) -> Option<Vec<TilePos>>`. Returns `from..=to` inclusive. `Some(vec![from])` when `from == to`. `None` when unreachable, when either end is not standable, or when the budget is exhausted.
  - `pub fn nearest_standable(src: &impl TileSource, column: [i32; 2], y_hint: Option<i32>) -> Option<TilePos>`: searches the column for a standable tile, closest to `y_hint` (the topmost when it is `None`), within the region.

Rules (DF-like, block scale):
- `standable(p)`: `p` and `p + y` are passable and `p − y` is solid. Solid means neither
  `BlockId::AIR` nor water. Passable means `AIR` (water is not walkable in v1: no swimming).
- Moves go to the 8 horizontal neighbours at dy ∈ {−1, 0, +1}, each landing on a standable tile.
  For dy = +1, the origin also needs `p + 2y` passable (the jump's headroom). A diagonal is allowed
  only when both orthogonal tiles it passes are passable at the higher of the two levels
  (no corner cutting).
- The cost is `STEP_COST` or `DIAG_COST`, plus `CLIMB_COST` when dy ≠ 0. The heuristic is octile
  distance on (x, z) plus `CLIMB_COST × |dy|`, which is admissible.
- Determinism: the open set is a `BinaryHeap` of `(Reverse(f), Reverse(h), Reverse(p))`, with
  `BlockPos: Ord` (derive it if missing). `BTreeMap` for came-from and g. No `HashMap` iteration.
- Tiles outside the region are not standable (bounds from `REGION_BLOCKS`). `TileSource::block`
  already returns implied ground for `Rollback`.

- [ ] **Step 1: Write the failing tests** in `crates/game/tests/pathfind.rs`, with a `Src` stub like `tests/orders.rs`'s (a `BTreeMap<TilePos, BlockId>`, with a floor helper):
  - `straight_line_on_flat_floor`: floor at y=0, from (1,1,1) to (6,1,1) → path length 6, every step |dx|+|dz| ≥ 1, ends at the goal.
  - `diagonal_is_used_and_cheaper`: (1,1,1) → (5,1,5) has exactly 5 tiles.
  - `detours_around_a_wall`: a 2-high wall at x=3, z=0..=4. The path avoids every wall tile and passes z=5.
  - `climbs_one_block_step`: floor y=0, plus a block at (4,1,*) so the tile above it is (4,2,*). The path contains a dy=+1 step.
  - `unreachable_two_high_wall`: an enclosure of 2-high walls → `None`.
  - `unreachable_goal_in_water` and `unreachable_goal_not_standable` (the goal is mid-air) → `None`.
  - `no_corner_cutting`: two blocks touching only diagonally. The path between the gaps takes orthogonal steps.
  - `budget_exhausted_is_none`: a reachable goal with `max_nodes = 4` → `None`.
  - `deterministic`: two calls give `==` vectors on a maze with many equal-cost routes.
- [ ] **Step 2: Run** `just test-one game "--test pathfind"`. Expected: fails to compile (`find_path` not found).
- [ ] **Step 3: Implement** the functions above in `crates/game/src/pathfind.rs`.
- [ ] **Step 4: Run** `just test-one game "--test pathfind"`. Expected: all pass, each under 100 ms.
- [ ] **Step 5: Commit** `feat(game): deterministic block-scale A* pathfinder`.

### Task 2: Play mode as sim state

**Files:**
- Modify: `crates/game/src/state.rs` (`Client`, the `ecs` struct, `Rollback` methods), `crates/game/src/orders.rs` (`Order`, `apply_order_safe`), `crates/game/src/ui.rs` (`UiUpdate`)
- Create: `crates/game/src/adventure.rs` (mode, goal and walker types plus their `Rollback` impls; `lib.rs` re-export)
- Test: `crates/game/tests/adventure.rs`

**Interfaces:**
- Produces:
  - `#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, StateHash)] pub enum PlayMode { #[default] Fortress, Adventure }`
  - `Client.mode: PlayMode`. It travels with `Client` in `EntityBundle.client`.
  - `Order::SetMode { mode: PlayMode, at: Option<TilePos> }`. `at` is only read when entering Adventure.
  - `Order::MoveTo(Goal)`, where `#[derive(…same…)] pub struct Goal { pub region: RegionCoords, pub column: [i32; 2], pub y: Option<i32> }` (`column` is region-local to `region`).
  - `UiUpdate::PlayMode(PlayMode)`, sent only when it changes (like `UiUpdate::Hotbar`).
  - `impl Rollback { pub fn set_play_mode_safe(&mut self, client: ClientId, mode: PlayMode, at: Option<TilePos>) -> bool }`
- Behaviour:
  - Entering Adventure while already in Adventure is a no-op (`true`, no entries).
  - With `at`, the body moves to the standable tile `nearest_standable(src, [at.x, at.z], Some(at.y))`, its feet on the tile floor, through `set_body_pose_safe`. With no such tile it moves to the region's spawn column (`CreateClient`'s spawn rule).
  - `Fortress` clears the client's `Walker` (Task 3) and leaves the body where it stands.
  - `fps_cam_mode` is untouched: the keyboard never drives the adventure body.

- [ ] **Step 1: Failing tests** in `crates/game/tests/adventure.rs`. Use a `region_with_player()` helper, as in `tests/anim.rs` (floor via `common::floor_chunks`):
  - `new_client_is_fortress`
  - `set_mode_adventure_places_body_on_tile`: `Order::SetMode { Adventure, at: Some((10, 1, 12)) }` then one tick. The body's x/z are within 0.5 block of the tile centre, it is grounded, and the mode is Adventure.
  - `set_mode_undo_bit_exact`: `state_hash` before, apply the order event, undo → equal hash.
  - `set_mode_emits_ui_update_once`
- [ ] **Step 2: Run** `just test-one game "--test adventure"`. Expected: compile failure.
- [ ] **Step 3: Implement.** Route `Order::SetMode` through `apply_order_safe`, which calls `set_play_mode_safe`. Mutate `Client.mode` under an undo scope whose inverse restores the old value.
- [ ] **Step 4: Run** `just test-one game "--test adventure"` and `just test-one game "--test orders"`. Expected: pass.
- [ ] **Step 5: Commit** `feat(game): per-client PlayMode and Order::SetMode`.

### Task 3: Walker — MoveTo, steering, stuck handling

**Files:**
- Modify: `crates/game/src/adventure.rs`, `crates/game/src/state.rs` (`ecs`: `walker: Component<crate::Walker>`), `crates/game/src/camera.rs` (`CameraController::on_tick` key source), `crates/game/src/orders.rs`
- Test: `crates/game/tests/adventure.rs`

**Interfaces:**
- Consumes: `find_path`, `nearest_standable`, `MAX_PATH_NODES` (Task 1); `PlayMode`, `Goal` (Task 2); `movement::{MoveKeys, STEP_HEIGHT}`.
- Produces:
  - `pub struct Walker { pub goal: Goal, pub path: Vec<TilePos>, pub next: u32, pub exit: Option<[i8; 2]>, pub best_dist: u32, pub stalled: u32, pub replanned: bool }` (derive like `Goal`). `exit` is the outward (dx, dz) unit step once the path's last tile is the region edge toward a goal elsewhere.
  - `pub const WAYPOINT_REACH_M: f32 = 0.3;`, `pub const STUCK_TICKS: u32` (≈ 2 s of ticks: derive it from `TICK_RATE`, the tick period in ms), `pub const MAX_GOAL_REGION_STEP: i32 = 1;` (goals at most one region away, Chebyshev).
  - `pub fn steer(walker: &Walker, feet: Vec3, grounded: bool) -> (MoveKeys, f32 /*yaw*/)`: a pure function. Forward, with yaw `libm::atan2f` toward the next waypoint's centre (or along `exit`). `jump_pressed`/`jump_held` when the next tile is higher than the current one by more than `STEP_HEIGHT` and the body is grounded. Never sprint.
  - `impl Rollback { pub fn move_to_safe(&mut self, client: ClientId, goal: Goal) -> bool }`. Returns false with no change when the client is not in Adventure, when the goal is more than `MAX_GOAL_REGION_STEP` regions away, or when there is no path. A same-region goal plans to `nearest_standable(goal.column, goal.y)`. A goal in another region plans to the standable edge tile facing it whose column is nearest the straight line to the goal (ties broken by the lowest coordinate), and sets `exit`.
- Behaviour in `CameraController::on_tick`: when the entity has a `Walker` and its client is in Adventure, `keys` and the yaw come from `steer` instead of `held(..)`. The yaw is written with the same camera undo scope plus `look_quat(yaw, 0.0)` body rotation already used for mouse look. Advance `next` while the body is within `WAYPOINT_REACH_M` of the waypoint horizontally and on its level. On the last tile with `exit: None`, remove the `Walker` (arrived). Progress means the remaining tile count went below `best_dist`. After `STUCK_TICKS` without progress: re-plan once from the current tile (`replanned = true`); if it is still stuck, remove the `Walker`. Every `Walker` change goes through `walker.set_safe`.

- [ ] **Step 1: Failing tests** (add to `tests/adventure.rs`; mark any over 100 ms slow):
  - `move_to_walks_body_to_goal`: Adventure at (4,1,4), then `MoveTo` (20,·,20). Within `40 m / WALK_SPEED` s of ticks plus 50 % margin the body stands on (20,1,20) and its `Walker` is gone.
  - `move_to_refused_in_fortress_mode`: no `Walker` and an unchanged hash.
  - `move_to_unreachable_changes_nothing`: the goal is inside a 2-high enclosure → `false`, same hash.
  - `move_to_climbs_step`: a one-block step on the route. The body ends on the upper level.
  - `move_to_undo_bit_exact`: hash, apply `MoveTo` + 10 ticks, undo all → equal.
  - `walker_replans_once_then_gives_up`: after `MoveTo`, place a 2-high wall across the path (`set_block_safe`) with no way around. Within `2 × STUCK_TICKS + 5` ticks the `Walker` is removed and `replanned` was set at least once.
  - `steer_points_at_next_waypoint`: unit tests for `steer` (yaw for +x is the same as `look_quat`'s +x convention; jump only when the next tile is higher and grounded).
- [ ] **Step 2: Run** `just test-one game "--test adventure"`. Expected: the new tests fail.
- [ ] **Step 3: Implement** `Walker`, `steer` and `move_to_safe`, and add the key-source branch to `CameraController`.
- [ ] **Step 4: Run** `just test-one game "--test adventure"`, `just test-one game "--test anim"` (the gait still plays when walker-driven), and `just determinism-wasm`. Expected: all pass, goldens unchanged (no golden scene enters Adventure).
- [ ] **Step 5: Commit** `feat(game): MoveTo orders walk the adventure body along an A* path`.

### Task 4: Walking into other regions

**Files:**
- Modify: `crates/game/src/protocol.rs` (`EntityBundle`: `walker: Option<crate::Walker>`, or a `Travelling` variant; see Global Constraints), the bundle build/inject sites in `crates/game/src/state.rs` / `region.rs` (where `animator` is carried today), `crates/game/src/adventure.rs`
- Test: `crates/game/tests/handoff_schedule.rs` (seeded), `crates/game/tests/adventure.rs`

**Interfaces:**
- Consumes: `Walker`, `Goal`, `move_to_safe` internals (Task 3).
- Produces: `impl Rollback { fn resume_walker_safe(&mut self, e: EntityKey) }`, called on `EntityArrived` injection. With `goal.region == self.coords` it re-plans the path from the body's tile to the goal. With another, still-adjacent region it re-plans to the facing edge. If the goal is now unreachable or too far, the `Walker` is dropped. `exit` is cleared by every re-plan.
- The handoff itself is unchanged. Arrivals already wake the target, and `PlayerInput` follows `homes`.

- [ ] **Step 1: Failing tests**
  - `adventure.rs` `walker_survives_bundle_roundtrip`: extract a bundle mid-walk (the helper the handoff tests use) and inject it into a fresh region. It has a `Walker` with the same `goal`.
  - `handoff_schedule.rs` `walk_into_neighbour_region` (slow, seeded `InlineSchedule`, same harness as the seam walk there): the player is in Adventure 6 blocks from the +x seam, with `MoveTo` to `Goal { region: (1,0), column: [6, z], y: None }`. Under 8 seeds the player ends homed in (1,0) on that column and the `Walker` is gone. A failure prints the seed.
- [ ] **Step 2: Run** `just test-one game "--test adventure walker_survives"`. Expected: FAIL.
- [ ] **Step 3: Implement** the bundle field and `resume_walker_safe`. Its undo is handled by `walker.set_safe`.
- [ ] **Step 4: Run** `just test-threads`, `just test-one game "--test adventure"` and `just determinism-wasm`. Expected: pass.
- [ ] **Step 5: Commit** `feat(game): adventure walks hand off across region seams`.

### Task 5: Client — mode switch, visible body, selectable player

**Files:**
- Modify: `crates/client/src/renderer/bridge.rs` (`LocalBody` visibility at the two `Visibility::Hidden` inserts, ~L727 and ~L922), `crates/client/src/renderer/commander/selection.rs` (`is_selectable`), `crates/client/src/renderer/commander/designate.rs` (disable in Adventure), `crates/client/src/renderer/slint_overlay.rs` (props and callbacks), `crates/slint-ui/ui/game.slint`, the client's `UiUpdate` handler
- Create: `crates/client/src/renderer/commander/adventure.rs` (`ClientPlayMode` resource, the mode button handler, `sync_body_visibility`)

**Interfaces:**
- Consumes: `UiUpdate::PlayMode`, `Order::SetMode` (Task 2); `CommanderCamera.focus` / `ground_y`, `GroundPick`.
- Produces:
  - `#[derive(Resource, Default, Clone, Copy, PartialEq, Eq)] pub struct ClientPlayMode(pub game::PlayMode)`
  - `pub fn is_selectable(kind: EntityKind, local_player: bool) -> bool`: players are selectable only when `local_player` and in Adventure. Callers pass `With<LocalBody>`-ness and the mode.
  - Slint `GameUi`: `in property <bool> adventure;` and `callback toggle-mode();`, with a panel button whose label is `adventure ? "Fortress mode" : "Adventure mode"`; `tools-available` is driven false in Adventure.
  - The toggle sends `Order::SetMode { mode, at }`, where `at` is the tile under the camera focus (`focus`, `ground_y`, converted to the home region's local block).
- Behaviour: `LocalBody` gets `Visibility::Inherited` in Adventure and `Hidden` in Fortress (the rig child in `rigs.rs` inherits it). Designate input and the tool bar are inactive in Adventure.

- [ ] **Step 1: Failing tests** (client `--lib`, pure functions; ask before running): `selection::tests::player_selectable_only_when_local_in_adventure`, and `adventure::tests::focus_tile_is_region_local` (focus at world (home origin + 10.5 m, y, + 3.2 m) → `TilePos { x: 10, z: 3, .. }`).
- [ ] **Step 2: Implement** the above.
- [ ] **Step 3: Run** `just smoke` (type-checks the client), plus `just test-one client "--lib selection::"` and `"--lib adventure::"` if Dawid OK'd client test runs. Also `just test-one slint-ui ""` if `game.slint` changed a headless-tested layout.
- [ ] **Step 4: Commit** `feat(client): adventure mode toggle shows and selects the player body`.

### Task 6: Follow

**Files:**
- Create: `crates/client/src/renderer/commander/follow.rs`
- Modify: `crates/client/src/renderer/commander/mod.rs` (register; pan systems clear `Follow`), `minimap.rs` (a minimap click clears `Follow`), `selection.rs` (`prune_selection` keeps the rekeyed player), `slint_overlay.rs`, `crates/slint-ui/ui/game.slint`

**Interfaces:**
- Produces:
  - `pub enum FollowTarget { LocalPlayer, Entity(RegionId, EntityKey) }`; `#[derive(Resource, Default)] pub struct Follow(pub Option<FollowTarget>)`
  - `pub const FOLLOW_SMOOTHING_S: f32 = 0.1;`
  - `pub fn follow_step(focus: Vec2, target: Vec2, dt: f32) -> Vec2`: exponential ease with `FOLLOW_SMOOTHING_S` (same form as `ZOOM_SMOOTHING_S`'s use).
  - `fn rekey_local_player(...)`: when the `LocalBody`'s `SimEntity` differs from last frame's, replace the old key in `Selection`. The test name is `rekey_local_player`.
  - Slint: `in property <bool> following;` and `callback follow-clicked();`. The button sits in the `HorizontalFlow` next to **Clear selection**, labelled `following ? "Stop following" : "Follow"` and enabled when `selected-count == 1 || following`.
- Behaviour: Follow on the selected local body → `LocalPlayer`, otherwise `Entity`. Each frame `cam.focus = clamp_focus(follow_step(..), bounds)` and `ground_y` follows the target's y the same way. Panning (WASD, arrows, edge, middle-drag, minimap) sets `Follow(None)`. Clear selection does not stop following. A target missing from `SimEntityMap` ends following (`Entity` only).

- [ ] **Step 1: Failing tests** (client `--lib`): `follow::tests::follow_step_converges` (from 0 to 100 within 1 s at 60 Hz, ending under 0.1 away), and `follow::tests::rekey_local_player` (an `App` with `MinimalPlugins`: a `LocalBody` entity with `SimEntity (r0, k0)` is selected; changing it to `(r1, k1)` leaves `Selection == {(r1, k1)}`).
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Run** `just smoke` and the two client tests (if OK'd).
- [ ] **Step 4: Commit** `feat(client): follow the selected entity with the commander camera`.

### Task 7: Camera bounds follow the character's near window

**Files:**
- Modify: `crates/client/src/renderer/commander/mod.rs` (`track_home_region`, `region_rect`), `crates/client/src/renderer/world_map.rs` (a pending-Play flag)

**Interfaces:**
- Consumes: `WorldScale::near_radius` (`crates/client/src/world_scale.rs`); `HudStatus.home_region`.
- Produces:
  - `pub fn window_rect(home: RegionId, near_radius: i32) -> Rect`: the home rect grown by `near_radius × REGION_SIZE` on each side.
  - `#[derive(Resource, Default)] pub struct PendingPlay(pub bool)` in `world_map.rs`. It is set when a `ClientRequest::PlayRegion` is sent and cleared on the next home change or on `PlayRefused`.
  - `track_home_region` un-places the camera (`placed = false`) only when `PendingPlay` is set. Otherwise it clamps the focus into the new bounds.

- [ ] **Step 1: Failing tests** (client `--lib`): `window_rect_radius_zero_is_home_rect`; `window_rect_radius_one_is_three_regions_wide`; `walking_home_change_keeps_focus` (`home_change_focus` with `placed = true` across adjacent rects keeps a focus that is inside the new rect).
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Run** `just smoke` and the tests (if OK'd).
- [ ] **Step 4: Commit** `feat(client): commander bounds follow the near window; only Play re-centres`.

### Task 8: Right-click orders and the path preview

**Files:**
- Create: `crates/client/src/renderer/commander/move_order.rs`
- Modify: `crates/client/src/renderer/commander/mod.rs` (`hold_pointer_while_orbiting` records the press and accumulated motion), `selection.rs` (`GroundPick` fallback)

**Interfaces:**
- Consumes: `find_path`, `nearest_standable`, `MAX_PATH_NODES`, `standable` (Task 1); `Order::MoveTo`, `Goal` (Task 2); `TileMirror: TileSource` (`commander/tiles.rs`); `Selection`, `ClientPlayMode`, `LocalBody`.
- Produces:
  - `GroundPick` gains a fallback. When the height march misses, it intersects the cursor ray with `y = cam.ground_y` and returns that point and its region, marked inexact: `pub struct GroundPick(pub Option<(RegionId, Vec3)>, pub bool /*exact*/)`. Update the existing readers (designate ignores inexact picks).
  - `pub fn is_click(accumulated_motion_px: f32) -> bool` (`< selection::DRAG_THRESHOLD_PX`).
  - `#[derive(Resource, Default)] pub struct PathPreview { pub key: Option<(TilePos, TilePos)>, pub path: Option<Vec<TilePos>>, pub beyond: Option<Vec3> }`. It is recomputed only when the (body tile, hovered tile) key changes. `beyond` is the world goal when the hover is in another region; the path then runs to the facing edge tile, using the same edge rule as `move_to_safe`, factored into `pub fn edge_toward(src, from, goal: &Goal) -> Option<TilePos>` in `game::adventure` so that both sides share it.
  - `pub const PATH_COLOR_OK`, `PATH_COLOR_NONE`, `PATH_LIFT_M: f32 = 0.1` (the gizmo line height above the floor).
- Behaviour:
  - Active only in Adventure with the local body selected, and only when the pointer is not over UI (`CursorPolicy::ui_pointer`).
  - The hover draws a gizmo polyline through the tile centres, `PATH_LIFT_M` above each floor, plus a dashed segment to `beyond`. With no path it draws a red cross at the hover.
  - On a right release that `is_click`, the client sends `Order::MoveTo(goal)` for the hover tile at the press position. The order is not sent when the preview has no path. The last sent path stays drawn (dimmed) until the body reaches its end or a new order replaces it. This is cosmetic only.

- [ ] **Step 1: Failing tests** (client `--lib`): `is_click_under_threshold`; `preview_recomputes_only_on_key_change` (a `PathPreview` update fn over a stub `TileSource` counts calls); `goal_from_hover_in_neighbour_region` (a hover point in region (1,0) gives `Goal { region: (1,0), column: local, y: None }`). In `game` (`tests/adventure.rs`): `edge_toward_matches_move_to_plan`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Run** `just test-one game "--test adventure"`, `just smoke`, and the client tests (if OK'd).
- [ ] **Step 4: Live check** (one client, `adapter=Intel`, ask Dawid first: memory "check before running benchmarks"). Run `just run`, press Adventure, select the dwarf, hover (the path draws), right-click (it walks), Follow, then walk across the region edge (the camera stays with it). Report what was seen. Do not claim it works without a live run.
- [ ] **Step 5: Commit** `feat(client): right-click move orders with a live path preview`, then update `TODO.md` (the adventure-mode item) and `CLAUDE.md` (the client bullet: adventure mode, follow, `move_order.rs`; the game bullet: `pathfind.rs`, `adventure.rs`). Commit those as `docs: adventure mode`.

---

## Deferred (not in this plan)

- Swimming or wading paths (water is impassable), ladders/ramps, and falls of more than one block.
- Following a non-player entity across a seam (its key changes; needs a stable entity id).
- A sim-emitted path for the drawn route (it is client-cosmetic today), and orders for other entities (DF workers).
- Goals more than one region away (would need a region-graph planner).
