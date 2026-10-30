use bevy::prelude::*;
use bevy::window::CursorGrabMode;
use game::{ConnectionState, RegionCoords};

/// Region + connection status mirrored from the game thread via
/// `ClientUpdateEvent::HudStatus`. Written by `drain_client_updates`, read by
/// the F3 dev panel (`dev_ui`). Options are `None` until the first status arrives.
#[derive(Resource, Default)]
pub struct HudStatus {
    pub home_region: Option<RegionCoords>,
    pub viewer_region: Option<RegionCoords>,
    pub connection: ConnectionState,
}

/// Show the crosshair only while the cursor is locked for mouse look (not
/// when free for menus or the commander camera, nor held for its right-drag
/// orbit). Drawn by the HUD layer (`HudUi`).
pub fn crosshair_visible(grab: CursorGrabMode, orbit: bool) -> bool {
    !orbit && grab == CursorGrabMode::Locked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crosshair_shows_only_when_locked_and_not_orbiting() {
        assert!(crosshair_visible(CursorGrabMode::Locked, false));
        assert!(!crosshair_visible(CursorGrabMode::Locked, true));
        assert!(!crosshair_visible(CursorGrabMode::None, false));
    }
}
