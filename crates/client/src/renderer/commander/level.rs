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
//! - Cut off: the free camera. The level follows it ([`derive_level`]): the
//!   surface under the focus above ground, the focus's level underground
//!   (the camera sank through the surface, see `super::vertical_move`).
//!   Space / Shift move it straight up / down (in `super::pan_and_zoom`),
//!   PgUp / PgDn by one level; Home brings it back above the surface.
//!
//! Turning the cut off leaves the camera at the level it was on (underground
//! when that is below the surface); turning it on starts at the camera's.

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
/// `fast`), clamped to `range`.
pub fn step_level(level: i32, steps: i32, fast: bool, range: (i32, i32)) -> i32 {
    let step = if fast { LEVEL_STEP_FAST } else { 1 };
    (level + steps * step).clamp(range.0, range.1.max(range.0))
}

/// The reachable levels for loaded surfaces spanning `surface` (sim units).
pub fn level_range(surface: (f32, f32)) -> (i32, i32) {
    let b = BLOCK_VOXELS as f32;
    (
        (surface.0 / b).floor() as i32 - UNDERGROUND_DEPTH as i32,
        (surface.1 / b).floor() as i32 + LEVELS_ABOVE_SURFACE,
    )
}

/// The working level for a camera: the level its focus is at when it is
/// underground (`ground_y`, world sim units), else the surface under the
/// focus (`surface`; `None` while that column is not loaded).
pub fn derive_level(underground: bool, ground_y: f32, surface: Option<f32>) -> Option<i32> {
    let baseline = if underground { Some(ground_y) } else { surface };
    baseline.map(surface_level)
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

/// Where the free camera's focus goes when the cut is turned off on `level`:
/// that level's floor, underground when it is below `surface` (the terrain
/// under the focus). `None` without a level.
pub fn uncut_camera(level: Option<i32>, home: Option<RegionId>, surface: Option<f32>) -> Option<(f32, bool)> {
    let floor = level_floor_y(&WorkingLevel { level, cut: true }, home)?;
    Some((floor, surface.is_some_and(|s| floor < s)))
}

/// With the cut off, keep the working level on the camera ([`derive_level`]);
/// on turning the cut off, leave the camera at the level it was on
/// ([`uncut_camera`]). The cut is toggled by C and the tool wheel alike, so
/// the toggle is seen as a change, not a key.
pub fn track_camera_level(
    grid: Res<HeightGrid>,
    tiles: Res<TileMirror>,
    mut cam: ResMut<CommanderCamera>,
    mut level: ResMut<WorkingLevel>,
    mut was_cut: Local<Option<bool>>,
) {
    let surface = super::focus_ground_target(&grid, cam.focus);
    if was_cut.replace(level.cut) == Some(true) && !level.cut {
        if let Some((floor, underground)) = uncut_camera(level.level, tiles.region(), surface) {
            cam.underground = underground;
            if underground {
                cam.ground_y = floor;
            }
        }
    }
    if level.cut {
        return;
    }
    let derived = derive_level(cam.underground, cam.ground_y, focus_surface(&grid, cam.focus));
    if derived.is_some() && derived != level.level {
        level.level = derived;
    }
}

/// C toggles the cut. Cut on: Space / Shift / PgUp / PgDn step the level
/// (Ctrl: ten), repeating while held, and Home goes to the surface level.
/// Cut off: PgUp / PgDn move the camera one level (Ctrl: ten), Home brings
/// it back above the surface (Space / Shift are `super::pan_and_zoom`'s).
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn level_input(
    keys: Res<ButtonInput<KeyCode>>,
    policy: Res<CursorPolicy>,
    grid: Res<HeightGrid>,
    time: Res<Time>,
    mut cam: ResMut<CommanderCamera>,
    mut level: ResMut<WorkingLevel>,
    mut held: Local<Option<[f32; 2]>>,
) {
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
    let surface = super::focus_ground_target(&grid, cam.focus);
    let floor = grid.surface_range().map(sink_floor);
    let steps = up as i32 - down as i32;
    if steps != 0 {
        let per = if fast { LEVEL_STEP_FAST } else { 1 };
        let dy = (steps * per * BLOCK_VOXELS as i32) as f32;
        super::move_vertically(&mut cam, dy, surface, floor);
    }
    if keys.just_pressed(SURFACE_KEY) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_level_key_steps_once_then_repeats() {
        let dt = 1.0 / 60.0;
        let (mut t, mut total) = (-1.0, 0);
        let mut fired_at = Vec::new();
        for frame in 0..60 {
            let (next, n) = repeat_step(t, true, dt);
            if n > 0 {
                fired_at.push(frame);
            }
            (t, total) = (next, total + n);
        }
        assert_eq!(fired_at[0], 0, "the press steps at once");
        let gap = (fired_at[1] - fired_at[0]) as f32 * dt;
        assert!((gap - LEVEL_KEY_REPEAT_INTERVAL_S).abs() <= dt, "the first repeat comes one interval later: {gap}");
        // ~1 s held: the press, then one every 0.08 s.
        assert_eq!(total, repeat_fired(59.0 * dt));
        assert_eq!(repeat_step(t, false, dt), (-1.0, 0), "release resets");
    }

    #[test]
    fn level_keys_avoid_the_pan_keys() {
        let all = LEVEL_UP_KEYS.iter().chain(&LEVEL_DOWN_KEYS).chain(&CUT_LEVEL_UP_KEYS).chain(&CUT_LEVEL_DOWN_KEYS);
        for key in all.chain(&LEVEL_FAST_KEYS).chain([&CUT_KEY, &SURFACE_KEY]) {
            assert!(!matches!(key, KeyCode::KeyW | KeyCode::KeyA | KeyCode::KeyS | KeyCode::KeyD), "{key:?}");
        }
        for key in LEVEL_UP_KEYS.iter().chain(&LEVEL_DOWN_KEYS) {
            assert!(!super::super::RISE_KEYS.contains(key) && !super::super::SINK_KEYS.contains(key), "{key:?}");
        }
    }

    #[test]
    fn cut_on_steps_one_or_ten_within_the_ground_range() {
        assert_eq!(step_level(50, 1, false, (0, 100)), 51);
        assert_eq!(step_level(50, -2, true, (0, 100)), 30);
        assert_eq!(step_level(5, -10, true, (0, 100)), 0);
        assert_eq!(level_range((1024.0, 1600.0)), (64 - 60, 100 + 8));
    }

    #[test]
    fn turning_the_cut_off_leaves_the_camera_on_its_level() {
        let b = BLOCK_VOXELS as f32;
        let home = Some(home0());
        assert_eq!(uncut_camera(Some(40), home, Some(64.0 * b)), Some((40.0 * b, true)), "dug down: underground");
        assert_eq!(uncut_camera(Some(70), home, Some(64.0 * b)), Some((70.0 * b, false)), "above the ground");
        assert_eq!(uncut_camera(None, home, Some(64.0 * b)), None);
    }

    #[test]
    fn the_level_follows_the_surface_above_ground_and_the_focus_below() {
        let b = BLOCK_VOXELS as f32;
        assert_eq!(derive_level(false, 10.0 * b, Some(64.0 * b)), Some(64), "above ground: the surface");
        assert_eq!(derive_level(false, 10.0 * b, None), None, "the surface column not loaded yet");
        assert_eq!(derive_level(true, 40.5 * b, Some(64.0 * b)), Some(40), "underground: where the focus is");
    }

    #[test]
    fn the_camera_may_sink_to_just_below_bedrock() {
        let b = BLOCK_VOXELS as f32;
        let floor = sink_floor((100.0 * b, 300.0 * b));
        assert_eq!(floor, (100 - UNDERGROUND_DEPTH as i32 - LEVELS_BELOW_BEDROCK) as f32 * b);
    }

    fn home0() -> RegionId {
        game::RegionCoords::new(0, 0).into()
    }

    #[test]
    fn the_cut_is_on_by_default_and_clips_at_the_level() {
        assert!(WorkingLevel::default().cut);
        let off = WorkingLevel { level: Some(10), cut: false };
        assert_eq!(cut_y(&off, Some(home0())), None);
        let on = WorkingLevel { level: Some(10), cut: true };
        assert_eq!(cut_y(&on, Some(home0())), Some(11.0 * BLOCK_VOXELS as f32));
        assert_eq!(level_floor_y(&on, Some(home0())), Some(10.0 * BLOCK_VOXELS as f32));
    }
}
