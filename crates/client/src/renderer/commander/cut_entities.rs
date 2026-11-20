//! Entities above the working level are hidden while the z-level cut is on
//! (spec §6.2). Units and models are not voxel geometry, so the terrain
//! clip does not cut them: a unit standing ON the working level is drawn
//! whole (its head may poke above the cut), one whose feet are at or above
//! the cut — standing on level + 1 or higher — is hidden. Only inside the
//! home region, like the terrain cut (ruling R10).
//!
//! Feet: a sim entity's `Transform` is its body centre, region-local under
//! the region root at `RegionId::world_offset()`. Players are a capsule
//! `PLAYER_HEIGHT` tall, slimes a cube `slime_edge_units(size)` tall, both
//! centred on the pose, so feet = world y − half the body height
//! ([`feet_offset`]).
//!
//! Composing with other `Visibility` writers (halo dedupe, the hidden local
//! body): the cut owns the entity's `Visibility` only while it hides it. On
//! hiding, [`CutHidden`] remembers what was there; a write by any other
//! system meanwhile (seen through change detection — this system's own
//! writes are not "changed" to it) replaces that remembered value, and the
//! cut re-hides. Releasing writes the remembered value back, so the cut can
//! never un-hide something another system hid.

use bevy::prelude::*;
use game::{EntityKind, REGION_SIZE, RegionId};

use super::level::{WorkingLevel, camera_cut};
use super::tiles::TileMirror;
use crate::renderer::bridge::{SimEntity, SimKind};

/// Feet this far below the cut still count as on it (world units, 1/16 m):
/// float error and the character controller's skin, not a standing height.
pub const FEET_EPSILON: f32 = 1.0;

/// Hidden by the cut; holds the visibility other systems want underneath.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutHidden(pub Visibility);

/// Body centre → feet, in world units.
pub fn feet_offset(kind: EntityKind) -> f32 {
    match kind {
        EntityKind::Player | EntityKind::Person { .. } => {
            game::movement::PLAYER_CAPSULE_HALF_HEIGHT + game::movement::PLAYER_CAPSULE_RADIUS
        }
        EntityKind::Slime { size } => game::slime::slime_edge_units(size) / 2.0,
        // Placeholder: the 1 m dev cube (bodies of other shapes come later).
        EntityKind::Body => game::BLOCK_VOXELS as f32 / 2.0,
    }
}

/// The home region's world-space xz rectangle, `[offset, offset + REGION_SIZE)`
/// at its copy in `frame`.
pub fn home_rect(frame: &crate::wrap_frame::WrapFrame, home: RegionId) -> Rect {
    let o = frame.offset(home);
    Rect::new(o[0], o[2], o[0] + REGION_SIZE, o[2] + REGION_SIZE)
}

/// Whether an entity with world-space `feet` is hidden by the cut at `cut_y`
/// (`None` = cut off) inside `home` (half-open on the far edges).
pub fn hidden_by_cut(feet: Vec3, cut_y: Option<f32>, home: Option<Rect>) -> bool {
    let (Some(cut), Some(home)) = (cut_y, home) else {
        return false;
    };
    let inside =
        (home.min.x..home.max.x).contains(&feet.x) && (home.min.y..home.max.y).contains(&feet.z);
    inside && feet.y >= cut - FEET_EPSILON
}

/// Hide / restore kind-bearing sim entities against the current cut.
pub fn hide_cut_entities(
    mut commands: Commands,
    level: Res<WorkingLevel>,
    tiles: Res<TileMirror>,
    roots: Res<crate::renderer::bridge::RegionRoots>,
    mut entities: Query<(
        Entity,
        &SimEntity,
        &SimKind,
        &Transform,
        &mut Visibility,
        Option<&mut CutHidden>,
    )>,
) {
    let cut = camera_cut(&level, &tiles);
    let frame = tiles.frame();
    let home = tiles.region().map(|home| home_rect(&frame, home));
    for (e, sim, kind, tf, mut vis, held) in &mut entities {
        // Not where the frame puts it (hidden with its tree): left alone.
        if !roots.drawn(sim.region) {
            continue;
        }
        let o = frame.offset(sim.region);
        let feet = tf.translation + Vec3::new(o[0], o[1] - feet_offset(kind.0), o[2]);
        let hide = hidden_by_cut(feet, cut, home);
        match (hide, held) {
            (true, None) => {
                commands.entity(e).insert(CutHidden(*vis));
                vis.set_if_neq(Visibility::Hidden);
            }
            (true, Some(mut held)) => {
                // Someone else wrote meanwhile: that is the value to restore.
                if vis.is_changed() {
                    held.0 = *vis;
                }
                vis.set_if_neq(Visibility::Hidden);
            }
            (false, Some(held)) => {
                let underneath = if vis.is_changed() { *vis } else { held.0 };
                vis.set_if_neq(underneath);
                commands.entity(e).remove::<CutHidden>();
            }
            (false, None) => {}
        }
    }
}
