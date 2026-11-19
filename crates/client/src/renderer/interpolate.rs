use bevy::prelude::*;

use super::bridge::SimTarget;

/// Frame-rate-dependent exponential smoothing toward the sim pose —
/// deliberate parity with the old renderer's lerp constants. Position and
/// rotation smooth independently (`pos_smoothing`/`rot_smoothing`) so the
/// camera can snap its look (factor 1.0) while its position still glides.
/// If tick timestamps are ever added to SetEntityPosition, replace with
/// two-snapshot interpolation.
///
/// Writes only when the pose actually changes: every terrain chunk is a
/// fixed-body sim entity with a `SimTarget`, and an unconditional write
/// marked ~68k settled transforms changed each frame, which made
/// `propagate_parent_transforms` re-walk them all (4.5 ms/frame at `full`,
/// 2026-09-22 trace).
///
/// Terrain chunk entities are skipped, by their `ChunkBricks` (present from
/// spawn): they are fixed bodies that never move, and at the 5x5 window just
/// visiting ~64k of them cost ~1 ms/frame (2026-09-23 trace). A chunk that
/// does get a new pose snaps in `drain_region_updates`, bricks included (the
/// bricks themselves have no `SimTarget`). Chunks whose snapshot bricks are
/// still being derived are skipped by their `PendingSnapshotVoxels` marker
/// too — while flying, that backlog is ~100k chunks (5 regions per seam), and
/// visiting it cost 4.6 ms/frame (2026-09-23 `fly-probe` trace).
pub fn interpolate_transforms(
    mut query: Query<
        (&mut Transform, &SimTarget),
        (
            Without<super::bricks::ChunkBricks>,
            Without<super::bridge::PendingSnapshotVoxels>,
        ),
    >,
) {
    for (mut tf, target) in &mut query {
        let goal = target.goal();
        let close = tf.translation.distance(goal) <= target.pos_snap
            && tf.rotation.angle_between(target.rot) <= target.rot_snap;
        let mut next = *tf;
        if close {
            next.translation = goal;
            next.rotation = target.rot;
        } else {
            next.translation = tf.translation.lerp(goal, target.pos_smoothing);
            next.rotation = tf.rotation.slerp(target.rot, target.rot_smoothing);
        }
        tf.set_if_neq(next);
    }
}

