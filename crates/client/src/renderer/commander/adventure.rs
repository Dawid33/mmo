//! Adventure mode on the client: mode switch requests (no UI sends one since
//! the F9 panel went — Adventure has no entry point for now), the local body's
//! visibility and the fortress tools' off switch.
//!
//! The mode itself is sim state (`game::PlayMode` on the client's
//! `game::Client`); the client mirrors it in [`ClientPlayMode`] from the
//! sim's `UiUpdate::PlayMode` (`slint_overlay::drain_ui_updates`), never
//! guessing ahead of the sim. The button sends `Order::SetMode`; entering
//! Adventure puts the body on the tile under the camera focus
//! ([`focus_tile`]). In Adventure the body is drawn (it is hidden in
//! Fortress — the commander has no body) and can be selected
//! (`selection::is_selectable`); the designation tools and the tool wheel
//! are off.

use bevy::prelude::*;
use game::rapier3d::math::Vector;
use game::{Order, PlayMode, RegionId, TerrainKind, TilePos, REGION_BLOCKS};

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

/// Switch Fortress ↔ Adventure. Nothing writes it at the moment (the F9
/// panel's mode button was removed with the panel); kept for the next entry
/// point.
#[derive(Message, Debug, Clone, Copy)]
pub struct ToggleModeRequest;

/// The other mode.
pub fn toggled(mode: PlayMode) -> PlayMode {
    match mode {
        PlayMode::Fortress => PlayMode::Adventure,
        PlayMode::Adventure => PlayMode::Fortress,
    }
}

/// The tile under the camera focus (world x, z and the ground height
/// `ground_y`, sim units), local to `home`, on terrain `kind`: the tile feet
/// standing on that ground occupy (`TerrainKind::feet_tile` — a 1 m level on
/// voxels, a 1.2 m tile on studs, where the floor may lie up to three plates
/// into the tile or two under it).
pub fn focus_tile(kind: TerrainKind, focus: Vec2, ground_y: f32, home: RegionId) -> TilePos {
    let o = home.world_offset();
    kind.feet_tile(Vector::new(focus.x - o[0], ground_y - o[1], focus.y - o[2]))
}

/// The order the mode button sends from `mode`: the other mode, entering
/// Adventure at the focus tile (when there is a home region).
pub fn toggle_order(mode: PlayMode, cam: &CommanderCamera, home: Option<RegionId>, kind: TerrainKind) -> Order {
    let next = toggled(mode);
    let at = match next {
        PlayMode::Adventure => home.map(|home| {
            // A focus outside the home region (panned away) puts the body at
            // the nearest home tile, not at an out-of-region one.
            let mut t = focus_tile(kind, cam.focus, cam.ground_y, home);
            let last = REGION_BLOCKS as i32 - 1;
            t[0] = t[0].clamp(0, last);
            t[2] = t[2].clamp(0, last);
            t
        }),
        PlayMode::Fortress => None,
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
    let order = toggle_order(mode.0, &cam, mirror.region(), mirror.terrain());
    let _ = events.0.send(game::GameEventKind::Order(client, order));
}

/// The body's visibility for a mode: drawn (inheriting) in Adventure,
/// hidden in Fortress.
pub fn body_visibility(mode: PlayMode) -> Visibility {
    match mode {
        PlayMode::Adventure => Visibility::Inherited,
        PlayMode::Fortress => Visibility::Hidden,
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

/// On a mode change: Adventure puts the fortress tool down; Fortress drops
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
        PlayMode::Fortress => {
            // The body is hidden in Fortress: don't stay pinned to it.
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
        let t = focus_tile(TerrainKind::Voxel, focus, ground_y, home);
        assert_eq!((t[0], t[2]), (10, 3));
        assert_eq!(t[1], 70, "the air tile on top of the ground");
        // The voxel answer is the old free function's, at any height.
        for y in [-33.0, 0.0, 7.9, 8.0, 1119.9, 1120.0, 1127.99] {
            let p = Vector::new(10.5 * UNITS_PER_METER, y, 3.2 * UNITS_PER_METER);
            assert_eq!(focus_tile(TerrainKind::Voxel, Vec2::new(o[0] + p.x, o[2] + p.z), y + o[1], home), game::feet_tile(p));
        }
    }

    /// On stud terrain the hint is a 1.2 m TILE: the same ground height is
    /// another level than on voxels, and it is the sim's own feet tile.
    #[test]
    fn focus_tile_counts_stud_tiles_on_stud_terrain() {
        let kind = TerrainKind::Stud;
        let home = RegionCoords { x: 0, z: 0 };
        let h = kind.tile_height_units();
        let focus = Vec2::new(10.5 * UNITS_PER_METER, 3.2 * UNITS_PER_METER);
        // Ground exactly on a tile boundary: feet on it stand in the tile above.
        for level in [0, 1, 58, 70] {
            let ground = kind.level_bottom_units(level);
            let t = focus_tile(kind, focus, ground, home);
            assert_eq!(t, [10, level, 3], "ground at the bottom of level {level}");
            assert!(kind.on_level(t[1], ground));
            // A floor two plates into the tile (plates, half a brick) is the same tile.
            assert_eq!(focus_tile(kind, focus, ground + game::plate_units(2), home)[1], level);
        }
        // 70 m up is level 70 on voxels and 58 on studs (70 / 1.2).
        let y = 70.0 * UNITS_PER_METER;
        assert_eq!(focus_tile(TerrainKind::Voxel, focus, y, home)[1], 70);
        assert_eq!(focus_tile(kind, focus, y, home)[1], (y / h).floor() as i32);
        assert_eq!((y / h).floor() as i32, 58);
    }

    #[test]
    fn toggle_enters_adventure_at_the_focus_and_leaves_without_a_tile() {
        let home = RegionCoords { x: 0, z: 0 };
        let cam = CommanderCamera { focus: Vec2::new(40.0, 56.0), ground_y: 32.0, ..Default::default() };
        assert_eq!(
            toggle_order(PlayMode::Fortress, &cam, Some(home), TerrainKind::Voxel),
            Order::SetMode { mode: PlayMode::Adventure, at: Some([2, 2, 3]) }
        );
        assert_eq!(
            toggle_order(PlayMode::Adventure, &cam, Some(home), TerrainKind::Voxel),
            Order::SetMode { mode: PlayMode::Fortress, at: None }
        );
        // 20 m up: level 20 on voxels, tile 17 on studs (a floor two thirds into
        // tile 16 belongs to the tile above — the sim's feet rule).
        let cam = CommanderCamera { ground_y: 320.0, ..cam };
        assert_eq!(
            toggle_order(PlayMode::Fortress, &cam, Some(home), TerrainKind::Voxel),
            Order::SetMode { mode: PlayMode::Adventure, at: Some([2, 20, 3]) }
        );
        assert_eq!(
            toggle_order(PlayMode::Fortress, &cam, Some(home), TerrainKind::Stud),
            Order::SetMode { mode: PlayMode::Adventure, at: Some([2, 17, 3]) }
        );
    }

    #[test]
    fn body_shows_only_in_adventure() {
        assert_eq!(body_visibility(PlayMode::Adventure), Visibility::Inherited);
        assert_eq!(body_visibility(PlayMode::Fortress), Visibility::Hidden);
    }
}
