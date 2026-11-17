//! Adventure mode on the client: mode switch requests (F9, the Debug tab's
//! button and the autopilot write them; the body is the default, the overview
//! a debug view), the local body's
//! visibility and the fortress tools' off switch.
//!
//! The mode itself is sim state (`game::PlayMode` on the client's
//! `game::Client`); the client mirrors it in [`ClientPlayMode`] from the
//! sim's `UiUpdate::PlayMode` (`slint_overlay::drain_ui_updates`), never
//! guessing ahead of the sim. The button sends `Order::SetMode`; entering
//! Adventure puts the body on the tile under the camera focus
//! ([`focus_tile`]). In Adventure the body is drawn (it is hidden in
//! Overview — the commander has no body) and can be selected
//! (`selection::is_selectable`); the designation tools and the tool wheel
//! are off.

use bevy::prelude::*;
use game::rapier3d::math::Vector;
use game::{Order, PlayMode, RegionId, TilePos, REGION_BLOCKS};

use super::designate::DesignateState;
use super::follow::{Follow, FollowTarget};
use super::selection::Selection;
use super::tiles::TileMirror;
use super::CommanderCamera;
use crate::renderer::bridge::{LocalBody, SimEntity};
use crate::renderer::{GameEvents, LocalPlayer};

/// The local client's play mode as the sim last reported it.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClientPlayMode(pub PlayMode);

impl ClientPlayMode {
    pub fn adventure(self) -> bool {
        self.0 == PlayMode::Adventure
    }
}

/// Switch Overview ↔ Adventure. Written by F9 ([`overview_key`]), the Debug tab's
/// "Toggle overview" button (`DevAction::ToggleOverview`) and the autopilot.
#[derive(Message, Debug, Clone, Copy)]
pub struct ToggleModeRequest;

/// F9 switches between the overview and the body (a debug view; the Debug tab
/// has the same button).
pub const OVERVIEW_KEY: KeyCode = KeyCode::F9;

/// F9 → one [`ToggleModeRequest`], unless a UI layer has the keyboard.
pub fn overview_key(
    keys: Res<ButtonInput<KeyCode>>,
    policy: Res<crate::renderer::cursor::CursorPolicy>,
    mut requests: MessageWriter<ToggleModeRequest>,
) {
    if keys.just_pressed(OVERVIEW_KEY) && !policy.blocks_game() {
        requests.write(ToggleModeRequest);
    }
}

/// The other mode.
pub fn toggled(mode: PlayMode) -> PlayMode {
    match mode {
        PlayMode::Overview => PlayMode::Adventure,
        PlayMode::Adventure => PlayMode::Overview,
    }
}

/// The tile under the camera focus (world x, z and the ground height
/// `ground_y`, sim units), local to `home`: the air tile feet standing on
/// that ground occupy (`game::feet_tile`).
pub fn focus_tile(focus: Vec2, ground_y: f32, home: RegionId) -> TilePos {
    let o = home.world_offset();
    game::feet_tile(Vector::new(focus.x - o[0], ground_y - o[1], focus.y - o[2]))
}

/// The order the mode button sends from `mode`: the other mode, entering
/// Adventure at the focus tile (when there is a home region).
pub fn toggle_order(mode: PlayMode, cam: &CommanderCamera, home: Option<RegionId>) -> Order {
    let next = toggled(mode);
    let at = match next {
        PlayMode::Adventure => home.map(|home| {
            // A focus outside the home region (panned away) puts the body at
            // the nearest home tile, not at an out-of-region one.
            let mut t = focus_tile(cam.focus, cam.ground_y, home);
            let last = REGION_BLOCKS as i32 - 1;
            t[0] = t[0].clamp(0, last);
            t[2] = t[2].clamp(0, last);
            t
        }),
        PlayMode::Overview => None,
    };
    Order::SetMode { mode: next, at }
}

/// Mode button → `Order::SetMode`. The mode changes when the sim says so.
pub fn send_mode_toggle(
    mut requests: MessageReader<ToggleModeRequest>,
    mode: Res<ClientPlayMode>,
    cam: Res<CommanderCamera>,
    mirror: Res<TileMirror>,
    events: Res<GameEvents>,
    local: Res<LocalPlayer>,
) {
    // One press per frame is plenty; extra ones would toggle back.
    if requests.read().count() == 0 {
        return;
    }
    let Some(client) = local.0 else { return };
    let order = toggle_order(mode.0, &cam, mirror.region());
    let _ = events.0.send(game::GameEventKind::Order(client, order));
}

/// The body's visibility for a mode: drawn (inheriting) in Adventure,
/// hidden in Overview.
pub fn body_visibility(mode: PlayMode) -> Visibility {
    match mode {
        PlayMode::Adventure => Visibility::Inherited,
        PlayMode::Overview => Visibility::Hidden,
    }
}

/// Show or hide the local body (and its rig, a child that inherits) with
/// the mode. Writes only on a mode change or a newly tagged body (the bridge
/// tags it `Hidden`), so the z-level cut (`cut_entities`), which watches for
/// other writers, is not disturbed every frame.
pub fn sync_body_visibility(
    mode: Res<ClientPlayMode>,
    first_person: Res<super::first_person::FirstPerson>,
    mut bodies: Query<(Ref<LocalBody>, &mut Visibility)>,
) {
    // In first person the view is inside the body: its model would fill it.
    let want = if first_person.on { Visibility::Hidden } else { body_visibility(mode.0) };
    for (body, mut vis) in &mut bodies {
        if mode.is_changed() || first_person.is_changed() || body.is_added() {
            vis.set_if_neq(want);
        }
    }
}

/// On a mode change: Adventure puts the designate tool down; Overview drops
/// the body from the selection (it is not selectable there).
pub fn on_mode_change(
    mode: Res<ClientPlayMode>,
    mut designate: ResMut<DesignateState>,
    mut selection: ResMut<Selection>,
    mut follow: ResMut<Follow>,
    bodies: Query<&SimEntity, With<LocalBody>>,
    first_person: Res<super::first_person::FirstPerson>,
    mut to_first_person: MessageWriter<super::first_person::ToggleFirstPersonRequest>,
) {
    if !mode.is_changed() {
        return;
    }
    match mode.0 {
        PlayMode::Adventure => {
            // Survival is played from the eyes (F5 returns to the overhead
            // view); `toggle_first_person` also drops the cut and x-ray.
            if !first_person.on {
                to_first_person.write(super::first_person::ToggleFirstPersonRequest);
            }
            if designate.tool.is_some() {
                designate.set_tool(None);
            }
        }
        PlayMode::Overview => {
            // The body is hidden in Overview: don't stay pinned to it.
            if follow.0 == Some(FollowTarget::LocalPlayer) {
                follow.0 = None;
            }
            for body in &bodies {
                selection.0.remove(&(body.region, body.key));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::movement::UNITS_PER_METER;
    use game::RegionCoords;

    #[test]
    fn focus_tile_is_region_local() {
        let home = RegionCoords { x: 1, z: -2 };
        let o = home.world_offset();
        let ground_y = 70.0 * UNITS_PER_METER;
        let focus = Vec2::new(o[0] + 10.5 * UNITS_PER_METER, o[2] + 3.2 * UNITS_PER_METER);
        let t = focus_tile(focus, ground_y, home);
        assert_eq!((t[0], t[2]), (10, 3));
        assert_eq!(t[1], 70, "the air tile on top of the ground");
    }

    #[test]
    fn toggle_enters_adventure_at_the_focus_and_leaves_without_a_tile() {
        let home = RegionCoords { x: 0, z: 0 };
        let cam = CommanderCamera { focus: Vec2::new(40.0, 56.0), ground_y: 32.0, ..Default::default() };
        assert_eq!(
            toggle_order(PlayMode::Overview, &cam, Some(home)),
            Order::SetMode { mode: PlayMode::Adventure, at: Some([2, 2, 3]) }
        );
        assert_eq!(toggle_order(PlayMode::Adventure, &cam, Some(home)), Order::SetMode { mode: PlayMode::Overview, at: None });
    }

    #[test]
    fn body_shows_only_in_adventure() {
        assert_eq!(body_visibility(PlayMode::Adventure), Visibility::Inherited);
        assert_eq!(body_visibility(PlayMode::Overview), Visibility::Hidden);
    }
}
