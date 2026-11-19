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

/// Each live predicted region's recent tick times, from the game thread's
/// `ClientUpdateEvent::RegionTickTimes` (every
/// `game::perf::TICK_TIME_REPORT_INTERVAL`, native only). Shown by the F3
/// Debug tab (`dev_ui`).
#[derive(Resource, Default)]
pub struct RegionTickTimes(pub Vec<(game::RegionId, game::perf::TickTimeStats)>);

/// Show the crosshair only while the cursor is locked for mouse look (not
/// when free for menus or the commander camera, nor held for its right-drag
/// orbit), and not in third person: edits aim from the eye, not from the
/// camera behind it, so a centre mark would point elsewhere (Minecraft hides
/// it there too). Drawn by the HUD layer (`HudUi`).
pub fn crosshair_visible(grab: CursorGrabMode, orbit: bool, third_person: bool) -> bool {
    !orbit && !third_person && grab == CursorGrabMode::Locked
}

