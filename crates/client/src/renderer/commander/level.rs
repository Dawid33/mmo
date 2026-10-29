//! The working level: the block `y` the fortress player is looking at and
//! designating on, plus whether everything above it is cut away. Client-local
//! (nothing in the sim depends on it). The cutaway shader and the designation
//! tools read [`WorkingLevel`] and [`cut_plane_world_y`].

use bevy::input::mouse::{AccumulatedMouseScroll, MouseScrollUnit};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use game::{RegionId, BLOCK_VOXELS, UNDERGROUND_DEPTH};

use super::height::HeightGrid;
use super::tiles::TileMirror;
use super::{minimap, CommanderCamera};
use crate::renderer::cursor::CursorPolicy;

/// Levels per wheel notch with Shift held.
pub const LEVEL_STEP_FAST: i32 = 10;
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

/// Alt + wheel steps the level; the plain wheel stays zoom.
pub fn wheel_routes_to_level(alt: bool, focused: bool) -> bool {
    alt && focused
}

/// The level after `notches` wheel notches (±1 each, ±[`LEVEL_STEP_FAST`]
/// when `fast`), clamped to `range`.
pub fn step_level(level: i32, notches: f32, fast: bool, range: (i32, i32)) -> i32 {
    let whole = notches.round() as i32;
    // A small touchpad scroll still moves one level.
    let whole = if whole == 0 && notches != 0.0 { notches.signum() as i32 } else { whole };
    let step = if fast { LEVEL_STEP_FAST } else { 1 };
    (level + whole * step).clamp(range.0, range.1.max(range.0))
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

/// The height the camera rides: the lower of the terrain and the cut plane.
pub fn follow_target(terrain: Option<f32>, cut: Option<f32>) -> Option<f32> {
    match (terrain, cut) {
        (Some(t), Some(c)) => Some(t.min(c)),
        (t, c) => t.or(c),
    }
}

/// The solid ground height under the camera focus, if loaded.
fn focus_surface(grid: &HeightGrid, focus: Vec2) -> Option<f32> {
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

/// Alt+wheel (Alt+Shift: fast), PgUp / PgDn, Home (back to the surface at the
/// focus) and C (toggle the cut).
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn step_level_input(
    keys: Res<ButtonInput<KeyCode>>,
    scroll: Res<AccumulatedMouseScroll>,
    window: Query<&Window, With<PrimaryWindow>>,
    policy: Res<CursorPolicy>,
    minimap_ptr: Res<minimap::MinimapPointer>,
    grid: Res<HeightGrid>,
    cam: Res<CommanderCamera>,
    mut level: ResMut<WorkingLevel>,
) {
    let Ok(window) = window.single() else { return };
    if policy.overlay_open {
        return;
    }
    if keys.just_pressed(KeyCode::KeyC) {
        level.cut = !level.cut;
    }
    let Some(current) = level.level else { return };
    let range = grid.surface_range().map_or((current, current), level_range);
    let fast = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    let mut next = current;
    if keys.just_pressed(KeyCode::PageUp) {
        next = step_level(next, 1.0, fast, range);
    }
    if keys.just_pressed(KeyCode::PageDown) {
        next = step_level(next, -1.0, fast, range);
    }
    if keys.just_pressed(KeyCode::Home) {
        if let Some(y) = focus_surface(&grid, cam.focus) {
            next = surface_level(y);
        }
    }
    let over_minimap = window
        .cursor_position()
        .is_some_and(|c| minimap::screen_rect(window.size()).contains(c));
    let alt = keys.any_pressed([KeyCode::AltLeft, KeyCode::AltRight]);
    if wheel_routes_to_level(alt, window.focused) && !over_minimap && !minimap_ptr.dragging && !policy.ui_pointer
    {
        let notches = match scroll.unit {
            MouseScrollUnit::Line => scroll.delta.y,
            MouseScrollUnit::Pixel => scroll.delta.y / 40.0,
        };
        // Some desktops turn Alt+wheel into horizontal scroll.
        let notches = if notches == 0.0 { scroll.delta.x } else { notches };
        if notches != 0.0 {
            next = step_level(next, notches, fast, range);
        }
    }
    if next != current {
        level.level = Some(next);
    }
}

/// The cut plane the camera follows, when the cut is on and the level known.
pub fn camera_cut(level: &WorkingLevel, tiles: &TileMirror) -> Option<f32> {
    let (at, region) = (level.level?, tiles.region()?);
    level.cut.then(|| cut_plane_world_y(at, region))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_needs_focus_and_alt() {
        assert!(wheel_routes_to_level(true, true));
        assert!(!wheel_routes_to_level(true, false));
        assert!(!wheel_routes_to_level(false, true));
    }

    #[test]
    fn alt_wheel_steps_one_or_ten() {
        assert_eq!(step_level(50, 1.0, false, (0, 100)), 51);
        assert_eq!(step_level(50, -2.0, true, (0, 100)), 30);
    }

    #[test]
    fn level_clamps_to_ground_range() {
        assert_eq!(step_level(5, -10.0, true, (0, 100)), 0);
        assert_eq!(step_level(95, 10.0, true, (0, 100)), 100);
    }

    #[test]
    fn level_range_spans_underground_depth() {
        assert_eq!(level_range((1024.0, 1600.0)), (64 - 60, 100 + 8));
    }

    #[test]
    fn follow_target_takes_the_lower_of_terrain_and_cut() {
        assert_eq!(follow_target(Some(500.0), Some(300.0)), Some(300.0));
        assert_eq!(follow_target(Some(200.0), Some(300.0)), Some(200.0));
        assert_eq!(follow_target(None, Some(300.0)), Some(300.0));
        assert_eq!(follow_target(Some(200.0), None), Some(200.0));
        assert_eq!(follow_target(None, None), None);
    }
}
