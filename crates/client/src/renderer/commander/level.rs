//! The working level: the block `y` the fortress player is looking at and
//! designating on, plus whether everything above it is cut away. Client-local
//! (nothing in the sim depends on it). The cutaway shader and the designation
//! tools read [`WorkingLevel`] and [`cut_plane_world_y`].

use bevy::prelude::*;

use game::{RegionId, BLOCK_VOXELS, UNDERGROUND_DEPTH};

use super::height::HeightGrid;
use super::tiles::TileMirror;
use super::CommanderCamera;
use crate::renderer::cursor::CursorPolicy;

/// Levels per key step with Ctrl held.
pub const LEVEL_STEP_FAST: i32 = 10;
/// Holding a level key (Space / Shift / PgUp / PgDn) steps again every this
/// many seconds, with no longer pause after the first step.
pub const LEVEL_KEY_REPEAT_INTERVAL_S: f32 = 0.08;
/// Keys that step the working level up / down.
pub const LEVEL_UP_KEYS: [KeyCode; 2] = [KeyCode::Space, KeyCode::PageUp];
pub const LEVEL_DOWN_KEYS: [KeyCode; 3] = [KeyCode::ShiftLeft, KeyCode::ShiftRight, KeyCode::PageDown];
/// The fast (×[`LEVEL_STEP_FAST`]) modifier for the level keys.
pub const LEVEL_FAST_KEYS: [KeyCode; 2] = [KeyCode::ControlLeft, KeyCode::ControlRight];
/// How far above the highest loaded surface the level may climb.
pub const LEVELS_ABOVE_SURFACE: i32 = 8;

/// The block `y` being worked on (`None` until the focus column is loaded)
/// and whether the world above it is cut away.
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

/// The reachable levels for loaded surfaces spanning `surface` (sim units).
pub fn level_range(surface: (f32, f32)) -> (i32, i32) {
    let b = BLOCK_VOXELS as f32;
    (
        (surface.0 / b).floor() as i32 - UNDERGROUND_DEPTH as i32,
        (surface.1 / b).floor() as i32 + LEVELS_ABOVE_SURFACE,
    )
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

/// The height the camera rides: the working level's floor when there is one
/// (whatever the terrain or the cut toggle says), else the terrain.
pub fn camera_baseline(terrain: Option<f32>, level_floor: Option<f32>) -> Option<f32> {
    level_floor.or(terrain)
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

/// Space / PgUp up and Shift / PgDn down (repeating while held), Ctrl with
/// any of them for ×[`LEVEL_STEP_FAST`], Home (back to the surface
/// at the focus) and C (toggle the cut). The camera rides the level's floor,
/// so this is also how the commander rises and sinks.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn step_level_input(
    keys: Res<ButtonInput<KeyCode>>,
    policy: Res<CursorPolicy>,
    grid: Res<HeightGrid>,
    cam: Res<CommanderCamera>,
    time: Res<Time>,
    mut level: ResMut<WorkingLevel>,
    mut held: Local<Option<[f32; 2]>>,
) {
    let held = held.get_or_insert([-1.0; 2]);
    if policy.blocks_game() {
        *held = [-1.0; 2];
        return;
    }
    if keys.just_pressed(KeyCode::KeyC) {
        level.cut = !level.cut;
    }
    let Some(current) = level.level else { return };
    let range = grid.surface_range().map_or((current, current), level_range);
    let fast = keys.any_pressed(LEVEL_FAST_KEYS);
    let mut next = current;
    let dt = time.delta_secs();
    let (up_t, up) = repeat_step(held[0], keys.any_pressed(LEVEL_UP_KEYS), dt);
    let (down_t, down) = repeat_step(held[1], keys.any_pressed(LEVEL_DOWN_KEYS), dt);
    *held = [up_t, down_t];
    let steps = up as i32 - down as i32;
    if steps != 0 {
        next = step_level(next, steps, fast, range);
    }
    if keys.just_pressed(KeyCode::Home) {
        if let Some(y) = focus_surface(&grid, cam.focus) {
            next = surface_level(y);
        }
    }
    if next != current {
        level.level = Some(next);
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
        for key in LEVEL_UP_KEYS.iter().chain(&LEVEL_DOWN_KEYS).chain(&LEVEL_FAST_KEYS) {
            assert!(!matches!(key, KeyCode::KeyW | KeyCode::KeyA | KeyCode::KeyS | KeyCode::KeyD), "{key:?}");
        }
    }

    #[test]
    fn steps_one_or_ten() {
        assert_eq!(step_level(50, 1, false, (0, 100)), 51);
        assert_eq!(step_level(50, -2, true, (0, 100)), 30);
    }

    #[test]
    fn level_clamps_to_ground_range() {
        assert_eq!(step_level(5, -10, true, (0, 100)), 0);
        assert_eq!(step_level(95, 10, true, (0, 100)), 100);
    }

    #[test]
    fn level_range_spans_underground_depth() {
        assert_eq!(level_range((1024.0, 1600.0)), (64 - 60, 100 + 8));
    }

    fn home0() -> RegionId {
        game::RegionCoords::new(0, 0).into()
    }

    fn wl(cut: bool) -> WorkingLevel {
        WorkingLevel { level: Some(10), cut }
    }

    #[test]
    fn baseline_is_the_level_floor_with_cut_on_or_off() {
        let home = Some(home0());
        for cut in [true, false] {
            let floor = level_floor_y(&wl(cut), home);
            assert_eq!(floor, Some(160.0));
            assert_eq!(camera_baseline(Some(500.0), floor), Some(160.0));
        }
    }

    #[test]
    fn baseline_falls_back_to_terrain_without_a_level() {
        let none = WorkingLevel { level: None, cut: true };
        assert_eq!(level_floor_y(&none, Some(home0())), None);
        assert_eq!(level_floor_y(&wl(true), None), None);
        assert_eq!(camera_baseline(Some(200.0), None), Some(200.0));
        assert_eq!(camera_baseline(None, None), None);
    }

    #[test]
    fn baseline_ignores_terrain_when_a_level_exists() {
        assert_eq!(camera_baseline(Some(500.0), Some(160.0)), Some(160.0));
        assert_eq!(camera_baseline(Some(100.0), Some(160.0)), Some(160.0));
        assert_eq!(camera_baseline(None, Some(160.0)), Some(160.0));
    }
}
