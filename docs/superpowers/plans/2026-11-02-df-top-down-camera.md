# DF-Style Top-Down Camera Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Dwarf Fortress–style view for the fortress: straight down, north up, orthographic, one working level at a time with the levels below darkened. The wheel steps levels, Ctrl+wheel zooms in whole pixels per block, and V toggles it with the existing 3D commander camera.

**Architecture:** The top-down view is a third rig of the existing commander camera (`renderer/commander/`), not a new camera entity. A new module `commander/top_down.rs` owns a `TopDown` resource, the pure math and its systems. Each frame the camera's `Projection` and `Transform` are derived from `TopDown` plus `CommanderCamera` / `WorkingLevel`, so entering, leaving and session reset need no extra bookkeeping. Everything that already works with the cut stays as it is: the cutaway shader with cap, depth darkening and slice plane, designation tools, selection, move orders, minimap and follow. Only the perspective-only pieces get an orthographic path: the cut cap's eye ray, the riser fade, distance fog, sun cascades, the underground lighting decision, the orbit, and the wheel's meaning.

**Tech Stack:** Rust, Bevy 0.19.1 (`OrthographicProjection`, `ScalingMode::WindowSize`, `CascadeShadowConfigBuilder`), WGSL (`assets/shaders/voxel_pull.wgsl`).

**Spec:** none written. The user skipped the design questions, so the design below is the planner's, grounded in `docs/superpowers/specs/2026-10-20-dwarf-fortress-research/12-ui-and-interaction.md` §2 and §5 (z-level navigation and view requirements) and `docs/superpowers/specs/2026-11-02-intuitive-ui-research/04-ux-principles.md` §7 ("the z-level cut is a mode too": it needs an indicator and must show what the controls currently mean).

## Design (planner's calls — check these first)

- **A mode, not a replacement.** V toggles top-down on and off, and the 3D commander stays the default. Making top-down the default later means changing one constant (`TOP_DOWN_DEFAULT`).
- **Straight down, north up, no rotation.** Right-drag no longer orbits, so a right click is always a cancel or a move order. No 90° snaps (YAGNI).
- **Orthographic, pixel-exact zoom.** Zoom steps through `TOP_DOWN_ZOOM_PX_PER_BLOCK = [4, 6, 8, 12, 16, 24, 32, 48, 64]`, starting at 16 (1 block texel = 1 screen pixel for the 16-px block textures). The rendered focus snaps to whole screen pixels so tiles don't shimmer while panning.
- **DF controls:** the wheel steps one level per notch (Ctrl ×`LEVEL_STEP_FAST`). Ctrl+wheel and `[` / `]` zoom. Space / Shift / PgUp / PgDn step the level as with the cut on today, and Home goes to the surface. WASD / arrows / screen edge pan at `TOP_DOWN_PAN_PX_PER_S` screen pixels per second, and a middle-drag moves the ground exactly with the mouse.
- **The cut is always on in top-down.** Entering forces it on. Turning it off (C, or the wheel's "Show above") leaves top-down first and lands in the free 3D camera, so C keeps its one meaning. Leaving restores the 3D rig's yaw, pitch, distance and cut state from before entering.
- **First person and top-down exclude each other:** F5 or the First-person button leaves top-down first.
- **The eye sits `TOP_DOWN_EYE_HEIGHT_M` (512 m) above the cut plane**, with near 0 and far = eye height + `TOP_DOWN_DEPTH_M` (1024 m). Neighbouring terrain (outside the home region the cut doesn't apply) up to 512 m above the level is drawn rather than near-clipped. The GPU face cull already switches off for non-perspective views and under the cut (`voxel_pull/gpu.rs` `camera` uniform), so the faraway eye costs nothing there.
- **Lighting:** distance fog is off in top-down; the levels below already darken by depth. The sun gets top-down cascades measured from the far eye. The view counts as "underground" (no sun shadows, ambient ×`UNDERGROUND_AMBIENT_GAIN`) when the working level is below the surface at the focus column, instead of the eye-in-block test (the ortho eye is always in the air).
- **Indicator:** the F9 Fortress panel gets a "Top-down [V]" / "3D view [V]" button next to First person. Its level readout already shows the level.
- **Not in this plan:** sprite or glyph units, hidden-tile fog of war, 90° rotation, a depth bar, and the WebGL2 per-chunk fallback, where the cut doesn't exist and top-down shows the uncut surface.

## Global Constraints

- `game` / `server` stay Bevy-free: everything here is in `crates/client` and `assets/shaders`.
- Never write the region edge or block size as a literal: use `game::REGION_SIZE`, `game::BLOCK_VOXELS`, `game::movement::UNITS_PER_METER`.
- Tunables are named `pub const`s with a doc comment, in metres / pixels / seconds at the declaration, converted to sim units at use (project convention and user preference).
- Any new per-world resource is registered with `add_session_reset::<R>()` in `CommanderPlugin::build` (`commander/mod.rs`), or it leaks into the next world.
- Verification: `just smoke` (type-checks the client) after every task. Client unit tests are a slow, explicit opt-in (`just test-one client "--lib <filter>"`, full Bevy client test build), so **ask the user once before the first such run**, then reuse the warm build. Don't run whole suites.
- The user has uncommitted edits in `commander/mod.rs`, `commander/level.rs`, `commander/adventure.rs`, `cursor.rs`, `input.rs` and `slint_overlay.rs`, among others. Commit only the hunks this plan adds (`git add -p`), never the user's pending work, and never stash or revert it.
- Keep the `justfile` and `CLAUDE.md` in sync (Task 7).

## Review Focus

1. **Session end while in top-down** (Esc → main menu, or Retire, then embark again): the next world must open in the 3D rig with a perspective projection. The projection is derived from `TopDown`, which is session-reset; pinned in Task 2.
2. **Toggling mid-drag** (one designation corner placed, then V): the anchor tile stays and the second click still completes the box. The toggle changes only the camera, so nothing in `DesignateState` may be cleared. Pinned in Task 2.
3. **HiDPI / window resize:** at a window scale factor of 2 the zoom must still be whole physical pixels per block, so `units_per_px` takes the scale factor. Pinned in Task 1.
4. **Wheel at the level range's ends** (top: `LEVELS_ABOVE_SURFACE`; bottom: bedrock): the wheel clamps exactly like the keys and doesn't accumulate a backlog. Pinned in Task 3.
5. **Home region change while in top-down** (embark / ReturnTo / dev Play): stays top-down and centres on the new region. `track_home_region` / `place_on_body` already move `focus`, and the top-down transform reads it. Pinned in Task 2.

---

## File Structure

- Create `crates/client/src/renderer/commander/top_down.rs`: `TopDown` state, constants, pure math, and the systems `toggle_top_down`, `sync_top_down_projection`, `top_down_input`.
- Modify `crates/client/src/renderer/commander/mod.rs`: `pub mod top_down;`, session reset, system wiring and run conditions; `apply_camera_transform` and `underground_view` branch on `TopDown`.
- Modify `crates/client/src/renderer/commander/level.rs`: `pub fn wheel_level_steps`.
- Modify `crates/client/src/renderer/atmosphere.rs`: `top_down_fog`, `top_down_cascades`.
- Modify `assets/shaders/voxel_pull.wgsl`: ortho-aware `cut_crossing` and riser-fade pixel size.
- Modify `crates/client/src/renderer/commander/minimap.rs` (footprint test), `commander/first_person.rs` (leave top-down), `crates/slint-ui/ui/game.slint` + `crates/client/src/renderer/slint_overlay.rs` (button + `UiAction::ToggleTopDown`).
- Modify `CLAUDE.md` (commander paragraph).

---

### Task 1: Top-down state and math

**Files:**
- Create: `crates/client/src/renderer/commander/top_down.rs`
- Modify: `crates/client/src/renderer/commander/mod.rs` (`pub mod top_down;` + `add_session_reset::<top_down::TopDown>()`)

**Interfaces:**
- Produces:
  - `pub const TOP_DOWN_KEY: KeyCode = KeyCode::KeyV;`
  - `pub const TOP_DOWN_DEFAULT: bool = false;`
  - `pub const TOP_DOWN_ZOOM_PX_PER_BLOCK: [f32; 9] = [4.0, 6.0, 8.0, 12.0, 16.0, 24.0, 32.0, 48.0, 64.0];`, `pub const TOP_DOWN_ZOOM_START: usize = 4;`
  - `pub const TOP_DOWN_ZOOM_IN_KEYS: [KeyCode; 1] = [KeyCode::BracketRight]; pub const TOP_DOWN_ZOOM_OUT_KEYS: [KeyCode; 1] = [KeyCode::BracketLeft];`
  - `pub const TOP_DOWN_PAN_PX_PER_S: f32 = 900.0;`
  - `pub const TOP_DOWN_EYE_HEIGHT_M: f32 = 512.0; pub const TOP_DOWN_DEPTH_M: f32 = 1024.0;`
  - `#[derive(Resource, Debug, Clone, Copy, PartialEq)] pub struct TopDown { pub on: bool, pub zoom: usize, pub saved: Option<SavedRig> }` with `Default` = `{ on: TOP_DOWN_DEFAULT, zoom: TOP_DOWN_ZOOM_START, saved: None }`
  - `#[derive(Debug, Clone, Copy, PartialEq)] pub struct SavedRig { pub yaw: f32, pub pitch: f32, pub distance: f32, pub target_distance: f32, pub cut: bool }`
  - `pub fn active(td: Option<Res<TopDown>>) -> bool` and `pub fn inactive(td: Option<Res<TopDown>>) -> bool` (run conditions)
  - `pub fn units_per_px(px_per_block: f32, scale_factor: f32) -> f32`: sim units per PHYSICAL pixel = `BLOCK_VOXELS / (px_per_block * scale_factor)`
  - `pub fn zoom_step(zoom: usize, notches: i32) -> usize`: positive = in (more px per block), clamped to the table
  - `pub fn snap_to_pixel(p: Vec2, units_per_px: f32) -> Vec2`: round each axis to a multiple of `units_per_px`
  - `pub fn top_down_rotation() -> Quat`: looking down −Y with screen-up = −Z (north)
  - `pub fn top_down_transform(focus: Vec2, cut_plane_y: f32) -> Transform`: eye at `(focus.x, cut_plane_y + TOP_DOWN_EYE_HEIGHT_M * UNITS_PER_METER, focus.y)`
  - `pub fn top_down_projection(zoom: usize, scale_factor: f32) -> OrthographicProjection`: `near: 0.0`, `far: (TOP_DOWN_EYE_HEIGHT_M + TOP_DOWN_DEPTH_M) * UNITS_PER_METER`, `scaling_mode: ScalingMode::WindowSize`, `scale: units_per_px(TOP_DOWN_ZOOM_PX_PER_BLOCK[zoom], scale_factor) * scale_factor` (WindowSize maps one world unit to one LOGICAL pixel at scale 1: check against `bevy_camera-0.19.1/src/projection.rs` `OrthographicProjection::update` and fix the factor so that Task 1's tests and a 16 px/block screenshot agree), `..OrthographicProjection::default_3d()`

- [ ] **Step 1: Write the failing tests** (in `top_down.rs` `#[cfg(test)] mod tests`)

```rust
#[test]
fn looks_straight_down_with_north_up() {
    let r = top_down_rotation();
    assert!((r * Vec3::NEG_Z).abs_diff_eq(Vec3::NEG_Y, 1e-6), "forward is down");
    assert!((r * Vec3::Y).abs_diff_eq(Vec3::NEG_Z, 1e-6), "screen up is north");
}
#[test]
fn the_eye_sits_above_the_cut_over_the_focus() {
    let tf = top_down_transform(Vec2::new(100.0, -50.0), 1024.0);
    assert_eq!(tf.translation, Vec3::new(100.0, 1024.0 + TOP_DOWN_EYE_HEIGHT_M * UNITS_PER_METER, -50.0));
}
#[test]
fn zoom_is_whole_physical_pixels_per_block_at_any_scale_factor() {
    assert_eq!(units_per_px(16.0, 1.0) * 16.0, game::BLOCK_VOXELS as f32);
    assert_eq!(units_per_px(16.0, 2.0) * 32.0, game::BLOCK_VOXELS as f32);
}
#[test]
fn zoom_steps_clamp_to_the_table() {
    assert_eq!(zoom_step(TOP_DOWN_ZOOM_START, 1), TOP_DOWN_ZOOM_START + 1);
    assert_eq!(zoom_step(0, -3), 0);
    assert_eq!(zoom_step(TOP_DOWN_ZOOM_PX_PER_BLOCK.len() - 1, 5), TOP_DOWN_ZOOM_PX_PER_BLOCK.len() - 1);
}
#[test]
fn the_focus_snaps_to_whole_pixels() {
    let u = units_per_px(16.0, 1.0);
    let s = snap_to_pixel(Vec2::new(10.3 * u, -4.6 * u), u);
    assert_eq!(s, Vec2::new(10.0 * u, -5.0 * u));
}
#[test]
fn the_projection_reaches_from_the_eye_past_bedrock() {
    let p = top_down_projection(TOP_DOWN_ZOOM_START, 1.0);
    assert_eq!(p.near, 0.0);
    assert!(p.far >= (TOP_DOWN_EYE_HEIGHT_M + (game::UNDERGROUND_DEPTH as f32) + 8.0) * UNITS_PER_METER);
}
#[test]
fn top_down_starts_off_at_the_start_zoom() {
    assert_eq!(TopDown::default(), TopDown { on: TOP_DOWN_DEFAULT, zoom: TOP_DOWN_ZOOM_START, saved: None });
}
```

- [ ] **Step 2: Run to verify they fail.** Ask the user first (slow client opt-in). Run: `just test-one client "--lib top_down::"`. Expected: compile error, the module doesn't exist yet.
- [ ] **Step 3: Implement the constants, types and functions listed under Produces.**
- [ ] **Step 4: Run** `just test-one client "--lib top_down::"`. Expected: 7 passed.
- [ ] **Step 5: Commit** only the new file and the two `mod.rs` lines: `git add crates/client/src/renderer/commander/top_down.rs && git add -p crates/client/src/renderer/commander/mod.rs && git commit -m "feat(client): top-down camera state and math"`

### Task 2: Toggle, projection and transform

**Files:**
- Modify: `crates/client/src/renderer/commander/top_down.rs`, `commander/mod.rs` (`apply_camera_transform`, system wiring), `commander/first_person.rs`, `crates/slint-ui/ui/game.slint`, `crates/client/src/renderer/slint_overlay.rs`

**Interfaces:**
- Consumes: Task 1's `TopDown`, `SavedRig`, `top_down_transform`, `top_down_projection`, `snap_to_pixel`, `units_per_px`.
- Produces:
  - `#[derive(Message)] pub struct ToggleTopDownRequest;` (registered with `add_message` + `add_session_message_clear`, like `ToggleFirstPersonRequest`)
  - `pub fn enter(td: &mut TopDown, cam: &CommanderCamera, working: &mut WorkingLevel)`: saves the rig + cut, sets `working.cut = true`, `td.on = true`
  - `pub fn leave(td: &mut TopDown, cam: &mut CommanderCamera, working: &mut WorkingLevel)`: restores yaw / pitch / distance / target_distance / cut from `saved` (keeps `focus` and `ground_y`), `td.on = false`, `saved = None`
  - system `toggle_top_down`: V (unless a text field / overlay owns the keyboard, like the other commander keys; skipped while `first_person` is on) or a `ToggleTopDownRequest` → `enter` / `leave`. Also `leave` when `td.on && !working.cut` (C or "Show above" turned the cut off).
  - system `sync_top_down_projection`: on the `CommanderView` camera, writes `Projection::Orthographic(top_down_projection(..))` while `td.on`, and the same `Projection::Perspective` as `spawn_commander_camera` while off (factor that out as `pub fn commander_perspective() -> PerspectiveProjection` in `mod.rs`), only when the variant or the zoom changed
  - `apply_camera_transform`: when `td.on`, `top_down_transform(snap_to_pixel(cam.focus, units_per_px(..)), level::cut_plane_world_y(..))` (use the existing `cut_plane_world_y`, falling back to `cam.ground_y` while the level is `None`)
  - `UiAction::ToggleTopDown` + Slint `in property <bool> top-down; callback top-down-clicked();`, a button "Top-down [V]" / "3D view [V]" next to First person (Fortress and Adventure both)
  - `first_person::toggle_first_person`: entering first person calls `top_down::leave` first

- [ ] **Step 1: Write the failing tests** (pure `enter` / `leave` in `top_down.rs`):
  - `entering_forces_the_cut_and_leaving_restores_the_rig`: start from a rig with yaw 1.0, pitch 0.7, distance 300, cut false. `enter` gives `working.cut == true` and `td.on`. Change `cam.focus`, then `leave`: yaw, pitch, distance and cut are back to 1.0 / 0.7 / 300 / false, and focus keeps its new value.
  - `toggling_does_not_touch_the_designation_drag`: a `DesignateState` with a placed first corner is unchanged after `enter` and `leave` (the functions don't take it; assert by constructing it, calling both, and comparing a clone). This pins Review Focus 2 by keeping the API apart.
  - `a_session_reset_brings_back_perspective`: a headless `App` with `MinimalPlugins`, `CommanderPlugin`'s reset registrations and a `CommanderView` camera; set `TopDown.on = true`, run `sync_top_down_projection` (Orthographic), reset `TopDown` to default, run again: the projection is `Perspective` (Review Focus 1). Follow `headless-harness` practice: no LOD table, near radius 0.
  - `a_home_change_keeps_top_down_and_recentres`: `TopDown.on` with `home_change_focus` for a new region (`mod.rs` existing fn): `top_down_transform` x/z equal the new region's centre (Review Focus 5).
- [ ] **Step 2: Run** `just test-one client "--lib top_down::"`. Expected: the 4 new tests fail (functions missing).
- [ ] **Step 3: Implement the Produces list.** In `CommanderPlugin::build`, put `toggle_top_down` right before `level::init_level` in the main chain, and `sync_top_down_projection` right before `apply_camera_transform`.
- [ ] **Step 4: Run** `just test-one client "--lib top_down::"` (11 pass) and `just test-one slint-ui "--test it game_panel::"` (the panel still compiles and binds). Then `just smoke`.
- [ ] **Step 5: Commit** (`git add -p` on every modified file): `feat(client): V toggles a top-down orthographic commander view`

### Task 3: Top-down input (wheel = level, Ctrl+wheel = zoom, pixel-exact pan, no orbit)

**Files:**
- Modify: `crates/client/src/renderer/commander/top_down.rs`, `commander/level.rs`, `commander/mod.rs` (run conditions)

**Interfaces:**
- Consumes: Task 1's `zoom_step`, `units_per_px`; `level::step_level`, `level::level_range`, `LEVEL_FAST_KEYS`; `mod.rs` `edge_pan_dir`, `Sprint`, `clamp_focus`.
- Produces:
  - `level::wheel_level_steps(notches: f32, carry: &mut f32) -> i32`: whole notches, with the fractional remainder (pixel-unit touchpads, 40 px per notch as `pan_and_zoom` uses) kept in `carry`. Wheel up = level up (+1). The carry is cleared when the step is clamped at a range end, so no backlog builds up there (Review Focus 4).
  - `pub fn top_down_pan_step(dir: Vec2, units_per_px: f32, dt: f32) -> Vec2` = `dir.normalize_or_zero() * TOP_DOWN_PAN_PX_PER_S * units_per_px * dt` (screen x right = world +x, screen y down = world +z: north-up, no yaw)
  - `pub fn top_down_drag(delta: Vec2, units_per_px: f32) -> Vec2` = `-delta * units_per_px`
  - system `top_down_input` (runs `.run_if(top_down::active)`, in the same place as `pan_and_zoom`): wheel without Ctrl → level steps through `wheel_level_steps` + `step_level` (Ctrl held → fast). Ctrl+wheel and `[` / `]` → `td.zoom = zoom_step(..)`. Keys, edge and middle drag → `cam.focus`. It honours `CursorPolicy::ui_pointer` and `minimap::MinimapPointer` exactly as `pan_and_zoom` does, and any pan ends `Follow`, as there. Sprint multiplies the key / edge pan by `SPRINT_FACTOR`.
  - `pan_and_zoom`, `grab_orbit_pivot` and `hold_pointer_while_orbiting` get `.run_if(top_down::inactive)`. `level::level_input` keeps Space / Shift / PgUp / PgDn / Home (the cut is on, so they already step the level).

- [ ] **Step 1: Write the failing tests:**
  - `level.rs` `the_wheel_steps_whole_levels_and_keeps_the_remainder`: `wheel_level_steps(1.0, &mut 0.0) == 1`; with `carry = 0.0`, two calls at `0.6` give `0` then `1` (carry ≈ 0.2).
  - `level.rs` `the_wheel_does_not_store_steps_past_the_range_end`: at the top of `level_range`, five notches up then one notch down step the level down by exactly one. Drive it through a small pure helper `wheel_step_level(level, notches, carry, fast, range) -> i32`, also in `level.rs`, which `top_down_input` calls.
  - `top_down.rs` `pan_speed_is_constant_on_screen_at_every_zoom`: `top_down_pan_step(Vec2::X, units_per_px(16.0,1.0), 1.0).x / units_per_px(16.0,1.0) == TOP_DOWN_PAN_PX_PER_S`, and the same at 4 px/block.
  - `top_down.rs` `a_middle_drag_moves_the_ground_with_the_mouse`: `top_down_drag(Vec2::new(10.0, -5.0), u) == Vec2::new(-10.0 * u, 5.0 * u)`.
- [ ] **Step 2: Run** `just test-one client "--lib top_down:: level::"`. Expected: the new tests fail.
- [ ] **Step 3: Implement the Produces list.**
- [ ] **Step 4: Run** the same command (all pass), then `just smoke`.
- [ ] **Step 5: Commit:** `feat(client): DF controls in the top-down view — wheel steps levels, Ctrl+wheel zooms`

### Task 4: Orthographic-correct cutaway shading

**Files:**
- Modify: `assets/shaders/voxel_pull.wgsl` (`cut_crossing` ~line 459, riser fade ~line 529)
- Test: `crates/client/src/renderer/voxel_pull/pipeline.rs` (existing shader-validation tests)

**Interfaces:**
- Produces, in WGSL: `fn view_is_orthographic() -> bool { return view.clip_from_view[3][3] == 1.0; }`. `cut_crossing(p, cut)`: when orthographic, the eye ray toward `p` is the view's forward (`-view.world_from_view[2].xyz`), not `p - view.world_position`. It crosses the plane at `p + fwd * ((cut - p.y) / fwd.y)` when `p.y < cut` and `fwd.y < 0`. The riser fade's `block_px`: orthographic = `BLOCK_VOXELS * view.clip_from_view[1][1] * view.viewport.w / 2.0` (no distance term); perspective unchanged.

- [ ] **Step 1: Add a test** `ortho_paths_are_in_the_forward_shader` in `pipeline.rs`: the composed forward shader source (as the validation tests build it, with `CUTAWAY`) contains `view_is_orthographic` and still validates. This is the only headless check: the stubs can't render. Run `just test-one client "--lib voxel_pull::pipeline::"`. Expected: the new test fails.
- [ ] **Step 2: Implement the WGSL change.** If the stubbed `View` struct in the test imports lacks `clip_from_view` or `world_from_view`, extend the stub to match Bevy 0.19's `View`.
- [ ] **Step 3: Run** `just test-one client "--lib voxel_pull::pipeline::"`. Expected: all pass (the existing cutaway/xray/cutout validation tests included).
- [ ] **Step 4: Commit:** `fix(shader): the cut cap and riser fade under an orthographic view`

### Task 5: Fog, shadows and the underground look in top-down

**Files:**
- Modify: `crates/client/src/renderer/atmosphere.rs`, `crates/client/src/renderer/commander/mod.rs` (`underground_view`), `commander/top_down.rs` (cascade swap in `sync_top_down_projection`)

**Interfaces:**
- Produces:
  - `atmosphere::top_down_fog() -> DistanceFog`: `distance_fog()`'s colour with `FogFalloff::Linear { start, end }` both beyond the projection's far (`(TOP_DOWN_EYE_HEIGHT_M + TOP_DOWN_DEPTH_M) * m + 1.0`), i.e. no fog
  - `atmosphere::top_down_cascades() -> CascadeShadowConfig`: `minimum_distance = (TOP_DOWN_EYE_HEIGHT_M - TOP_DOWN_SHADOW_ABOVE_M) * m` with `pub const TOP_DOWN_SHADOW_ABOVE_M: f32 = 64.0;` (terrain up to 64 m above the level casts and receives; higher neighbouring terrain goes unshadowed, an accepted limit), `maximum_distance = (TOP_DOWN_EYE_HEIGHT_M + SHADOW_MAX_DISTANCE_M) * m`, `first_cascade_far_bound = (TOP_DOWN_EYE_HEIGHT_M + 16.0) * m`, other fields as `sun_cascades()`
  - `pub fn top_down_underground(level: Option<i32>, surface_block_y: Option<i32>) -> bool`: `level < surface` (both known), else false
  - `underground_view`: reads `TopDown`. While on, `under = top_down_underground(working.level, <surface block y at cam.focus from HeightGrid>)` and the fog wanted is `top_down_fog()`. While off, it behaves as today. It records its last choice in a `Local<Option<(bool, bool)>>` (top-down, under) and rewrites the fog only when the choice changes. Today's `fog.color` compare can't tell `top_down_fog` from `distance_fog`, which share a colour. Register that `Local` as a `SessionLocal` if it outlives a session.
  - `sync_top_down_projection` also writes the sun's `CascadeShadowConfig`: `top_down_cascades()` while on, `sun_cascades()` while off (only on a change)
- [ ] **Step 1: Write the failing tests** in `atmosphere.rs` / `mod.rs`:
  - `top_down_fog_never_reaches_the_ground`: the fog's linear start > the projection's far.
  - `top_down_shadows_cover_the_working_level_and_what_is_under_it`: `top_down_cascades()` minimum ≤ eye height − 1 level, and maximum ≥ eye height + `ZOOM_FAR_M`.
  - `a_level_below_the_surface_is_underground`: `top_down_underground(Some(10), Some(20))`, `!top_down_underground(Some(20), Some(20))`, `!top_down_underground(None, Some(20))`.
- [ ] **Step 2: Run** `just test-one client "--lib atmosphere:: commander::tests::"`. Expected: the new tests fail.
- [ ] **Step 3: Implement the Produces list.**
- [ ] **Step 4: Run** the same command (pass), then `just smoke`.
- [ ] **Step 5: Commit:** `feat(client): no fog, eye-relative shadow cascades and level-based underground lighting in top-down`

### Task 6: Picking, minimap footprint and selection under ortho

**Files:**
- Modify (tests mostly): `crates/client/src/renderer/commander/minimap.rs`, `commander/visible.rs`

The pickers already use `Camera::viewport_to_world`, which gives parallel rays starting on the near plane under an orthographic projection. They need no code change unless these tests fail. If one does, fix that picker to use the ray's own origin and direction, never the camera translation.

- [ ] **Step 1: Write the tests:**
  - `minimap.rs` `top_down_footprint_is_the_screen_rectangle`: four parallel `Ray3d`s pointing `-Y` from corners `(±a, h, ±b)` → `footprint(rays, 0.0)` is the axis-aligned rectangle `(±a, ±b)` in the order top-left, top-right, bottom-right, bottom-left (top = north = −z).
  - `visible.rs` `a_straight_down_ray_from_above_the_cut_finds_the_level_floor`: call `visible_ground_with` (existing signature) with origin far above the cut plane and `dir = -Y` over a column whose floor is the working level. Expected: the hit is the floor at the level's floor height, not cut-away rock above it.
- [ ] **Step 2: Run** `just test-one client "--lib minimap:: visible::"`. Expected: pass. If one fails, fix the picker as above and rerun.
- [ ] **Step 3: Commit:** `test(client): commander picking and minimap under the top-down projection`

### Task 7: Docs and live check

**Files:**
- Modify: `CLAUDE.md` (the `crates/client` commander-camera sentence: add the top-down mode, V, its controls and the "cut always on" rule), the `//!` header of `commander/mod.rs` and `commander/level.rs` (the two-modes paragraph becomes: cut on, cut off, top-down)

- [ ] **Step 1: Update the docs** as listed. No justfile change: there's no new command. `just run` exercises it.
- [ ] **Step 2: Run `just smoke`.** Expected: green within 60 s.
- [ ] **Step 3: Live check.** Ask the user before launching. Run `just adapter=Intel run`, press V, and confirm against this list:
  - Tiles are square and crisp at 16 px/block.
  - The wheel steps levels and the F9 level readout follows.
  - Ctrl+wheel and `[` / `]` zoom without blur.
  - A dug room under the surface shows its floor with the level below darkened.
  - The cut's cap lines up with the block grid at the screen edges (Task 4).
  - G-designation works with two clicks.
  - C goes back to the 3D free camera, and V back from there restores the old 3D rig.
  - Esc → main menu → load world opens in 3D.

  Write anything off into the plan's end as follow-ups.
- [ ] **Step 4: Commit:** `docs: top-down commander view`
