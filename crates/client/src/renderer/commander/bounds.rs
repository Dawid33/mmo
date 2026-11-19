//! Camera bounds in Adventure mode: the focus stays inside the character's
//! near window (the home region plus `near_radius` regions on every side —
//! the only terrain the sim has loaded around the body). Overview mode pans
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
/// (free pan) in Overview.
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
    let scale = scale.map_or(WorldScale::FULL, |s| *s);
    let window = status.home_region.map(|home| window_rect(home, scale.near_radius));
    let bounds = pan_bounds(mode.adventure(), window);
    let clamped = clamp_focus(cam.focus, bounds);
    if clamped != cam.focus {
        cam.focus = clamped;
    }
}

