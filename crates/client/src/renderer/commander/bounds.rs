//! Camera bounds in Adventure mode: the focus stays inside the character's
//! near window (the home region plus `near_radius` regions on every side —
//! the only terrain the sim has loaded around the body). Fortress mode pans
//! freely. `CommanderCamera::bounds` stays the home region's rectangle (camera
//! placement, minimap); the window rect is a separate value computed here.

use bevy::prelude::*;
use game::RegionId;

use super::adventure::ClientPlayMode;
use super::{clamp_focus, region_rect, CommanderCamera};
use crate::renderer::hud::HudStatus;
use crate::world_scale::WorldScale;

/// The home rect grown by `near_radius × REGION_SIZE` on each side.
pub fn window_rect(home: RegionId, near_radius: i32) -> Rect {
    let r = region_rect(home);
    let grow = Vec2::splat(near_radius.max(0) as f32 * game::REGION_SIZE);
    Rect { min: r.min - grow, max: r.max + grow }
}

/// The rect the pan is confined to: the near window in Adventure, none
/// (free pan) in Fortress.
pub fn pan_bounds(adventure: bool, window: Option<Rect>) -> Option<Rect> {
    if adventure {
        window
    } else {
        None
    }
}

/// After panning and following: clamp the focus to the near window.
pub fn clamp_to_window(
    mode: Res<ClientPlayMode>,
    status: Res<HudStatus>,
    scale: Option<Res<WorldScale>>,
    mut cam: ResMut<CommanderCamera>,
) {
    let near_radius = scale.map_or(WorldScale::FULL.near_radius, |s| s.near_radius);
    let window = status.home_region.map(|home| window_rect(home, near_radius));
    let bounds = pan_bounds(mode.adventure(), window);
    let clamped = clamp_focus(cam.focus, bounds);
    if clamped != cam.focus {
        cam.focus = clamped;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::commander::home_change_focus;

    #[test]
    fn window_rect_radius_zero_is_home_rect() {
        let home = game::RegionCoords::new(2, -1);
        assert_eq!(window_rect(home, 0), region_rect(home));
    }

    #[test]
    fn window_rect_radius_one_is_three_regions_wide() {
        let w = window_rect(game::RegionCoords::new(2, -1), 1);
        assert_eq!(w.size(), Vec2::splat(3.0 * game::REGION_SIZE));
        assert_eq!(w.center(), region_rect(game::RegionCoords::new(2, -1)).center());
    }

    #[test]
    fn walking_home_change_keeps_focus() {
        let old = region_rect(game::RegionCoords::new(0, 0));
        let new = region_rect(game::RegionCoords::new(1, 0));
        let focus = Vec2::new(old.max.x + 10.0, old.center().y);
        assert!(new.contains(focus));
        assert_eq!(home_change_focus(focus, Some(new), true), focus);
    }

    #[test]
    fn adventure_pan_is_clamped_to_window() {
        let window = window_rect(game::RegionCoords::new(0, 0), 1);
        let far = window.max + Vec2::splat(500.0);
        let b = pan_bounds(true, Some(window));
        assert_eq!(clamp_focus(far, b), window.max);
        assert_eq!(clamp_focus(far, pan_bounds(false, Some(window))), far, "Fortress pans freely");
    }
}
