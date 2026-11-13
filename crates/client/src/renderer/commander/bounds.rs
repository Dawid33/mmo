//! Camera bounds in Adventure mode: the focus stays inside the character's
//! near window (the home region plus `near_radius` regions on every side —
//! the only terrain the sim has loaded around the body), widened to the
//! simulated fortress neighbourhood while the body is inside it. Fortress mode pans
//! freely. `CommanderCamera::bounds` stays the home region's rectangle (camera
//! placement, minimap); the window rect is a separate value computed here.

use bevy::prelude::*;
use game::RegionId;

use super::adventure::ClientPlayMode;
use super::{clamp_focus, region_rect, CommanderCamera};
use crate::renderer::history_view::HistoryView;
use crate::renderer::hud::HudStatus;
use crate::world_scale::WorldScale;

/// The home rect grown by `near_radius × REGION_SIZE` on each side.
pub fn window_rect(home: RegionId, near_radius: i32) -> Rect {
    let r = region_rect(home);
    let grow = Vec2::splat(near_radius.max(0) as f32 * game::REGION_SIZE);
    Rect { min: r.min - grow, max: r.max + grow }
}

/// The Adventure window: the body's near window, or — while the body's region
/// lies inside the fortress neighbourhood (`fortress_radius` regions around the
/// fortress, also simulated) — the box spanning both.
pub fn adventure_window(
    home: RegionId,
    near_radius: i32,
    fortress: Option<RegionId>,
    fortress_radius: i32,
) -> Rect {
    let near = window_rect(home, near_radius);
    let Some(fortress) = fortress else { return near };
    let hood = window_rect(fortress, fortress_radius);
    let home_rect = region_rect(home);
    if hood.contains(home_rect.center()) {
        near.union(hood)
    } else {
        near
    }
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
    history: Option<Res<HistoryView>>,
    mut cam: ResMut<CommanderCamera>,
) {
    let scale = scale.map_or(WorldScale::FULL, |s| *s);
    let fortress = history.and_then(|h| h.fortress_region);
    let window = status
        .home_region
        .map(|home| adventure_window(home, scale.near_radius, fortress, scale.fortress_radius));
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
    fn adventure_window_spans_fortress_neighbourhood_only_from_inside() {
        let fort = game::RegionCoords::new(0, 0);
        let hood = window_rect(fort, 1);
        // Inside the neighbourhood: the whole 3x3 is pannable.
        let next = game::RegionCoords::new(1, 0);
        assert_eq!(adventure_window(next, 0, Some(fort), 1), hood);
        // Outside it: the body's own window only.
        let away = game::RegionCoords::new(5, 0);
        assert_eq!(adventure_window(away, 0, Some(fort), 1), region_rect(away));
        // No fortress: unchanged.
        assert_eq!(adventure_window(next, 0, None, 1), region_rect(next));
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
