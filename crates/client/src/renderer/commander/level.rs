//! The working level: the block `y` the fortress player is looking at and
//! designating on, plus whether everything above it is cut away. Client-local
//! (nothing in the sim depends on it). The cutaway shader and the designation
//! tools read [`WorkingLevel`] and [`cut_plane_world_y`].
//!
//! Two modes, switched by the cut (C, or the tool wheel's "Hide above"):
//!
//! - Cut on ("Hide above", the default): everything above the level is cut
//!   away, the camera rides the level's floor (zooming with the wheel), and
//!   Space / Shift (or PgUp / PgDn) step the level, repeating while held,
//!   Ctrl ×[`LEVEL_STEP_FAST`]; Home goes back to the surface level.
//! - Cut off: the free camera, following nothing (not the terrain, trees or
//!   the cut). The level is the focus's ([`derive_level`]). Space / Shift
//!   move it straight up / down (in `super::pan_and_zoom`) with no ceiling,
//!   PgUp / PgDn by one level; Home puts it back over the surface.
//!
//! - Top-down (V, `super::top_down`): the cut is always on, as above, and
//!   the wheel steps the level too (`wheel_step_level`); turning the cut
//!   off leaves top-down for the free camera.
//!
//! Either way the level is where the focus is, so switching never changes
//! it: turning the cut off puts the free camera's focus on the level's
//! floor, turning it on keeps the focus's level.

use bevy::prelude::*;

use game::{RegionId, BLOCK_VOXELS, UNDERGROUND_DEPTH};

use super::height::HeightGrid;
use super::tiles::TileMirror;
use super::CommanderCamera;
use crate::renderer::cursor::CursorPolicy;

/// Levels per key step with Ctrl held.
pub const LEVEL_STEP_FAST: i32 = 10;
/// Holding a level key (PgUp / PgDn) steps again every this many seconds,
/// with no longer pause after the first step.
pub const LEVEL_KEY_REPEAT_INTERVAL_S: f32 = 0.08;
/// Keys that step the level (cut on) / move the camera one level (cut off).
pub const LEVEL_UP_KEYS: [KeyCode; 1] = [KeyCode::PageUp];
pub const LEVEL_DOWN_KEYS: [KeyCode; 1] = [KeyCode::PageDown];
/// With the cut on these step the level too (with it off they move the
/// camera, `super::RISE_KEYS` / `super::SINK_KEYS`).
pub const CUT_LEVEL_UP_KEYS: [KeyCode; 2] = [KeyCode::Space, KeyCode::PageUp];
pub const CUT_LEVEL_DOWN_KEYS: [KeyCode; 3] = [KeyCode::ShiftLeft, KeyCode::ShiftRight, KeyCode::PageDown];
/// How far above the highest loaded surface the level may climb (cut on).
pub const LEVELS_ABOVE_SURFACE: i32 = 8;
/// The fast (×[`LEVEL_STEP_FAST`]) modifier for the level keys.
pub const LEVEL_FAST_KEYS: [KeyCode; 2] = [KeyCode::ControlLeft, KeyCode::ControlRight];
/// The key that toggles the cut.
pub const CUT_KEY: KeyCode = KeyCode::KeyC;
/// The key that brings the camera back above the surface.
pub const SURFACE_KEY: KeyCode = KeyCode::Home;
/// How far below the lowest loaded surface the camera may sink, in blocks
/// beyond the underground depth (bedrock).
pub const LEVELS_BELOW_BEDROCK: i32 = 8;

/// The block `y` being worked on (`None` until the focus column is loaded)
/// and whether the world above it is cut away (on by default).
#[derive(Resource)]
pub struct WorkingLevel {
    pub level: Option<i32>,
    pub cut: bool,
}

impl Default for WorkingLevel {
    fn default() -> Self {
        Self { level: None, cut: true }
    }
}

/// The level after `steps` steps (±1 each, ±[`LEVEL_STEP_FAST`] when
/// `fast`), clamped to `range` — widened to include `level` itself, so a
/// level the free camera left outside it (high in the air) steps back
/// toward it instead of jumping.
pub fn step_level(level: i32, steps: i32, fast: bool, range: (i32, i32)) -> i32 {
    let step = if fast { LEVEL_STEP_FAST } else { 1 };
    let (lo, hi) = (range.0.min(level), range.1.max(range.0).max(level));
    (level + steps * step).clamp(lo, hi)
}

/// Whole wheel notches to step the level by this frame (wheel up = level up),
/// keeping the fraction (touchpads scroll in pixels) in `carry`.
pub fn wheel_level_steps(notches: f32, carry: &mut f32) -> i32 {
    *carry += notches;
    let steps = carry.trunc();
    *carry -= steps;
    steps as i32
}

/// The level after a wheel of `notches` ([`wheel_level_steps`], then
/// [`step_level`] within `range`). A step clamped at the range's end drops
/// the carried fraction, so scrolling past the end stores nothing to undo.
pub fn wheel_step_level(level: i32, notches: f32, carry: &mut f32, fast: bool, range: (i32, i32)) -> i32 {
    let steps = wheel_level_steps(notches, carry);
    if steps == 0 {
        return level;
    }
    let next = step_level(level, steps, fast, range);
    let per = if fast { LEVEL_STEP_FAST } else { 1 };
    if next != level + steps * per {
        *carry = 0.0;
    }
    next
}

/// The reachable levels for loaded surfaces spanning `surface` (sim units).
pub fn level_range(surface: (f32, f32)) -> (i32, i32) {
    let b = BLOCK_VOXELS as f32;
    (
        (surface.0 / b).floor() as i32 - UNDERGROUND_DEPTH as i32,
        (surface.1 / b).floor() as i32 + LEVELS_ABOVE_SURFACE,
    )
}

/// The free camera's working level: the one its focus (`ground_y`, world sim
/// units) is in, above ground or below.
pub fn derive_level(ground_y: f32) -> i32 {
    surface_level(ground_y)
}

/// How many steps a level key has fired once it has been held `t` seconds
/// (`t < 0`: not held): one on the press, then one every interval.
pub fn repeat_fired(t: f32) -> u32 {
    if t < 0.0 {
        0
    } else {
        1 + (t / LEVEL_KEY_REPEAT_INTERVAL_S) as u32
    }
}

/// A level key's hold time after this frame (`held_for < 0`: was not held)
/// and the steps it fires this frame.
pub fn repeat_step(held_for: f32, pressed: bool, dt: f32) -> (f32, u32) {
    if !pressed {
        return (-1.0, 0);
    }
    let next = if held_for < 0.0 { 0.0 } else { held_for + dt };
    (next, repeat_fired(next) - repeat_fired(held_for))
}

/// The lowest the camera's focus may sink (world sim units) for loaded
/// surfaces spanning `surface`: a little below bedrock under the lowest.
pub fn sink_floor(surface: (f32, f32)) -> f32 {
    surface.0 - ((UNDERGROUND_DEPTH as i32 + LEVELS_BELOW_BEDROCK) * BLOCK_VOXELS as i32) as f32
}

/// The air level above a column whose ground tops out at `surface_y`.
pub fn surface_level(surface_y: f32) -> i32 {
    (surface_y / BLOCK_VOXELS as f32).floor() as i32
}

/// World y (sim units) of the top of `level`'s blocks in `region`: the plane
/// the cutaway clips at.
pub fn cut_plane_world_y(level: i32, region: RegionId) -> f32 {
    ((level + 1) * BLOCK_VOXELS as i32) as f32 + region.world_offset()[1]
}

/// World y of the floor of the working level in `home`, independent of the cut.
pub fn level_floor_y(level: &WorkingLevel, home: Option<RegionId>) -> Option<f32> {
    let (at, region) = (level.level?, home?);
    Some(cut_plane_world_y(at, region) - BLOCK_VOXELS as f32)
}

/// The solid ground height under the camera focus, if loaded.
pub fn focus_surface(grid: &HeightGrid, focus: Vec2) -> Option<f32> {
    grid.column_at(focus.x, focus.y)
        .and_then(|(top, ground)| [ground, top].into_iter().find(|h| !h.is_nan()))
}

/// Start at the surface under the camera once it has loaded.
pub fn init_level(grid: Res<HeightGrid>, cam: Res<CommanderCamera>, mut level: ResMut<WorkingLevel>) {
    if level.level.is_none() {
        if let Some(y) = focus_surface(&grid, cam.focus) {
            level.level = Some(surface_level(y));
        }
    }
}

/// With the cut off, keep the working level on the camera's focus
/// ([`derive_level`]); on turning the cut off, put the focus on the level's
/// floor ([`level_floor_y`]) at the opening distance ([`super::ZOOM_START_M`])
/// — the free camera has no zoom, so a wheel zoom from the cut-on mode must
/// not carry over. The level is unchanged by the switch either way. The cut
/// is toggled by C and the tool wheel alike, so the toggle is seen as a
/// change, not a key.
pub fn track_camera_level(
    tiles: Res<TileMirror>,
    mut cam: ResMut<CommanderCamera>,
    mut level: ResMut<WorkingLevel>,
    mut was_cut: crate::session::SessionLocal<Option<bool>>,
) {
    let was_cut = was_cut.get();
    if was_cut.replace(level.cut) == Some(true) && !level.cut {
        cam.distance = super::ZOOM_START_M * game::movement::UNITS_PER_METER;
        cam.target_distance = cam.distance;
        if let Some(floor) = level_floor_y(&level, tiles.region()) {
            cam.ground_y = floor;
        }
    }
    if level.cut {
        return;
    }
    // Before the first level is known (`init_level`) the focus height is a
    // placeholder, not a level.
    if level.level.is_none() {
        return;
    }
    let derived = derive_level(cam.ground_y);
    if level.level != Some(derived) {
        level.level = Some(derived);
    }
}

/// C toggles the cut. Cut on: Space / Shift / PgUp / PgDn step the level
/// (Ctrl: ten), repeating while held, and Home goes to the surface level.
/// Cut off: PgUp / PgDn move the camera one level (Ctrl: ten), Home puts it
/// back over the surface (Space / Shift are `super::pan_and_zoom`'s).
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn level_input(
    keys: Res<ButtonInput<KeyCode>>,
    policy: Res<CursorPolicy>,
    grid: Res<HeightGrid>,
    time: Res<Time>,
    mut cam: ResMut<CommanderCamera>,
    mut level: ResMut<WorkingLevel>,
    mut held: crate::session::SessionLocal<Option<[f32; 2]>>,
) {
    let held = held.get();
    let held = held.get_or_insert([-1.0; 2]);
    if policy.blocks_game() {
        *held = [-1.0; 2];
        return;
    }
    if keys.just_pressed(CUT_KEY) {
        level.cut = !level.cut;
    }
    let dt = time.delta_secs();
    let (up_keys, down_keys): (&[KeyCode], &[KeyCode]) =
        if level.cut { (&CUT_LEVEL_UP_KEYS, &CUT_LEVEL_DOWN_KEYS) } else { (&LEVEL_UP_KEYS, &LEVEL_DOWN_KEYS) };
    let (up_t, up) = repeat_step(held[0], keys.any_pressed(up_keys.iter().copied()), dt);
    let (down_t, down) = repeat_step(held[1], keys.any_pressed(down_keys.iter().copied()), dt);
    *held = [up_t, down_t];
    let fast = keys.any_pressed(LEVEL_FAST_KEYS);
    if level.cut {
        let Some(current) = level.level else { return };
        let range = grid.surface_range().map_or((current, current), level_range);
        let steps = up as i32 - down as i32;
        let mut next = if steps != 0 { step_level(current, steps, fast, range) } else { current };
        if keys.just_pressed(SURFACE_KEY) {
            if let Some(y) = focus_surface(&grid, cam.focus) {
                next = surface_level(y);
            }
        }
        if next != current {
            level.level = Some(next);
        }
        return;
    }
    let steps = up as i32 - down as i32;
    if steps != 0 {
        let per = if fast { LEVEL_STEP_FAST } else { 1 };
        let dy = (steps * per * BLOCK_VOXELS as i32) as f32;
        super::move_vertically(&mut cam, dy, grid.surface_range().map(sink_floor));
    }
    if keys.just_pressed(SURFACE_KEY) {
        let surface = focus_surface(&grid, cam.focus);
        super::surface_camera(&mut cam, surface);
    }
}

/// The cut plane the camera follows, when the cut is on and the level known.
pub fn camera_cut(level: &WorkingLevel, tiles: &TileMirror) -> Option<f32> {
    cut_y(level, tiles.region())
}

/// The cut plane in `home`, when the cut is on and the level known.
pub fn cut_y(level: &WorkingLevel, home: Option<RegionId>) -> Option<f32> {
    let (at, region) = (level.level?, home?);
    level.cut.then(|| cut_plane_world_y(at, region))
}

