//! Following: the commander camera tracks the selected entity until the
//! player pans away.
//!
//! The context box's Follow button (`slint_overlay`, `ui/menu-column.slint`) sets [`Follow`] for the one
//! selected entity — [`FollowTarget::LocalPlayer`] for the local body (whose
//! sim key changes when a seam handoff re-creates it), otherwise
//! [`FollowTarget::Entity`]. Any pan (WASD, arrows, screen edge, middle-drag,
//! minimap) clears it; zoom and orbit do not.

use bevy::prelude::*;
use game::{EntityKey, RegionId};

use super::selection::Selection;
use super::tiles::TileMirror;
use super::{CommanderCamera, HEIGHT_SMOOTHING_S, smooth};
use crate::renderer::bridge::{LocalBody, SimEntity, SimEntityMap};

/// What the camera follows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FollowTarget {
    /// The local body, by identity (its key changes across a handoff).
    LocalPlayer,
    /// Any other sim entity.
    Entity(RegionId, EntityKey),
}

/// The current follow target, if any.
#[derive(Resource, Default, Debug)]
pub struct Follow(pub Option<FollowTarget>);

/// Time constant of the focus easing onto the target, in seconds.
pub const FOLLOW_SMOOTHING_S: f32 = 0.1;

/// One frame of the focus easing from `focus` toward `target`
/// (frame-rate independent, like the zoom's [`smooth`]).
pub fn follow_step(focus: Vec2, target: Vec2, dt: f32) -> Vec2 {
    Vec2::new(
        smooth(focus.x, target.x, dt, FOLLOW_SMOOTHING_S),
        smooth(focus.y, target.y, dt, FOLLOW_SMOOTHING_S),
    )
}

/// Ease the focus (and the ground height) onto the followed entity. A
/// missing `Entity` target ends following.
pub fn follow_camera(
    mut follow: ResMut<Follow>,
    map: Res<SimEntityMap>,
    transforms: Query<&GlobalTransform>,
    body: Query<(&SimEntity, &GlobalTransform), With<LocalBody>>,
    mirror: Res<TileMirror>,
    time: Res<Time>,
    mut cam: ResMut<CommanderCamera>,
) {
    let Some(target) = follow.0 else { return };
    let at = match target {
        // The body in the home region: during a handoff overlap the
        // outgoing body still exists, and must not be tracked.
        FollowTarget::LocalPlayer => {
            let home = mirror.region();
            body.iter()
                .filter(|(s, _)| Some(s.region) == home)
                .map(|(_, gt)| gt.translation())
                .find(|p| *p != Vec3::ZERO)
        }
        FollowTarget::Entity(region, key) => match map
            .0
            .get(&(region, key))
            .and_then(|e| transforms.get(*e).ok())
        {
            Some(gt) => Some(gt.translation()),
            None => {
                follow.0 = None;
                return;
            }
        },
    };
    let Some(at) = at else { return };
    let dt = time.delta_secs();
    // Not clamped here: in Adventure `bounds::clamp_to_window` keeps the
    // focus inside the window afterwards.
    cam.focus = follow_step(cam.focus, Vec2::new(at.x, at.z), dt);
    cam.ground_y = smooth(cam.ground_y, at.y, dt, HEIGHT_SMOOTHING_S);
}

/// The local body's sim key changes on a seam handoff: carry the selection
/// over so the player stays selected. Runs before `prune_selection`.
pub fn rekey_local_player(
    body: Query<&SimEntity, With<LocalBody>>,
    mut selection: ResMut<Selection>,
    mut last: crate::session::SessionLocal<Option<(RegionId, EntityKey)>>,
) {
    let last = last.get();
    let Some(now) = body.iter().next().map(|s| (s.region, s.key)) else {
        return;
    };
    if let Some(old) = last.replace(now) {
        if old != now && selection.0.remove(&old) {
            selection.0.insert(now);
        }
    }
}
