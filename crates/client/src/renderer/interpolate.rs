use bevy::prelude::*;

use super::bridge::SimTarget;

/// Exponential smoothing toward the sim pose. Position and rotation smooth
/// independently (`pos_smoothing`/`rot_smoothing`) so the camera can snap its
/// look (factor 1.0) while its position still glides. If tick timestamps are
/// ever added to SetEntityPosition, replace with two-snapshot interpolation.
///
/// The factors are per 60 Hz FRAME (the old renderer's lerp constants) and
/// are rescaled to the real frame time ([`smoothing_for`]). Applied as they
/// stand every frame, the glide depended on the frame rate: the sim pose
/// steps 20 times a second, and at ~160 fps the camera closed the whole gap
/// in a few frames and then stood still until the next tick — a stop-and-go
/// walk (seen in play, 2026-11-19, once the frame rate rose).
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
/// does get a new pose snaps in `drain_region_updates` (the bricks themselves
/// have no `SimTarget` and no transform). Chunks whose snapshot bricks are
/// still being derived are skipped by their `PendingSnapshotVoxels` marker
/// too — while flying, that backlog is ~100k chunks (5 regions per seam), and
/// visiting it cost 4.6 ms/frame (2026-09-23 `fly-probe` trace).
pub fn interpolate_transforms(
    time: Option<Res<Time<Real>>>,
    mut query: Query<
        (&mut Transform, &SimTarget),
        (
            Without<super::bricks::ChunkBricks>,
            Without<super::bridge::PendingSnapshotVoxels>,
        ),
    >,
) {
    // No clock (a bare harness): one reference frame per call, as before.
    let frames = time.map_or(1.0, |t| t.delta_secs() * SMOOTHING_REFERENCE_HZ);
    for (mut tf, target) in &mut query {
        let goal = target.goal();
        let close = tf.translation.distance(goal) <= target.pos_snap
            && tf.rotation.angle_between(target.rot) <= target.rot_snap;
        let mut next = *tf;
        if close {
            next.translation = goal;
            next.rotation = target.rot;
        } else {
            next.translation = tf.translation.lerp(goal, smoothing_for(target.pos_smoothing, frames));
            next.rotation = tf.rotation.slerp(target.rot, smoothing_for(target.rot_smoothing, frames));
        }
        tf.set_if_neq(next);
    }
}


/// The frame rate the `SimTarget` smoothing factors are written for.
pub const SMOOTHING_REFERENCE_HZ: f32 = 60.0;

/// A per-reference-frame smoothing factor over `frames` reference frames
/// (the real frame time x [`SMOOTHING_REFERENCE_HZ`]): what is left of the
/// gap after one reference frame, `1 - factor`, compounds over the time that
/// actually passed, so the glide covers the same share of it per second at
/// any frame rate. A factor of 1 still snaps.
pub fn smoothing_for(factor: f32, frames: f32) -> f32 {
    1.0 - (1.0 - factor.clamp(0.0, 1.0)).powf(frames.max(0.0))
}
