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
/// Keyed chunks are skipped: they are fixed bodies that never move, and at the
/// 5x5 window just visiting ~64k of them cost ~1 ms/frame (2026-09-23 trace).
/// A chunk that does get a new pose snaps in `drain_region_updates`. Chunks
/// whose snapshot voxels are still being derived are not keyed yet, so they are
/// skipped by their `PendingSnapshotVoxels` marker — while flying, that backlog
/// is ~100k chunks (5 regions per seam), and visiting it cost 4.6 ms/frame
/// (2026-09-23 `fly-probe` trace).
pub fn interpolate_transforms(
    mut query: Query<
        (&mut Transform, &SimTarget),
        (
            Without<super::meshing::queue::ChunkKey>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::bridge::SimTarget;

    fn app_with(target: SimTarget, start: Transform) -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_systems(Update, interpolate_transforms);
        let e = app.world_mut().spawn((start, target)).id();
        (app, e)
    }

    fn target_goal() -> Vec3 {
        SimTarget::camera(Vec3::new(1.0, 0.0, 0.0), Quat::IDENTITY).goal()
    }

    #[test]
    fn converges_and_snaps_exactly() {
        let target = SimTarget::camera(Vec3::new(1.0, 0.0, 0.0), Quat::IDENTITY);
        let (mut app, e) = app_with(target, Transform::IDENTITY);
        for _ in 0..200 {
            app.update();
        }
        let t = app.world().entity(e).get::<Transform>().unwrap();
        assert_eq!(t.translation, target_goal(), "must snap bit-exact, not hover nearby");
    }

    #[test]
    fn settled_transform_is_not_marked_changed() {
        let target = SimTarget::body(Vec3::new(3.0, 0.0, 0.0), Quat::IDENTITY);
        let (mut app, e) = app_with(target, Transform::from_xyz(3.0, 0.0, 0.0));
        app.update();
        app.world_mut().clear_trackers();
        app.update();
        let changed = app.world().entity(e).get_ref::<Transform>().unwrap().is_changed();
        assert!(!changed, "an entity already at its target must not dirty its Transform");
    }

    #[test]
    fn keyed_chunks_are_not_interpolated() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_systems(Update, interpolate_transforms);
        let e = app
            .world_mut()
            .spawn((
                Transform::IDENTITY,
                SimTarget::body(Vec3::new(10.0, 0.0, 0.0), Quat::IDENTITY),
                crate::renderer::meshing::queue::ChunkKey([0, 0, 0]),
            ))
            .id();
        app.update();
        let t = app.world().entity(e).get::<Transform>().unwrap();
        assert_eq!(t.translation, Vec3::ZERO, "fixed chunks are skipped, never lerped");
    }

    #[test]
    fn first_step_moves_partway() {
        let target = SimTarget::body(Vec3::new(10.0, 0.0, 0.0), Quat::IDENTITY);
        let (mut app, e) = app_with(target, Transform::IDENTITY);
        app.update();
        let t = app.world().entity(e).get::<Transform>().unwrap();
        assert!(t.translation.x > 0.0 && t.translation.x < 10.0);
    }

    #[test]
    fn camera_rotation_snaps_position_lerps() {
        // The camera look must snap in one frame (no slerp-arc tilt), but its
        // position must glide — lerp partway, not jump to the target.
        let target = SimTarget::camera(
            Vec3::new(5.0, -3.0, 2.0),
            Quat::from_rotation_y(1.2),
        );
        let (mut app, e) = app_with(target, Transform::IDENTITY);
        app.update();
        let t = app.world().entity(e).get::<Transform>().unwrap();
        // Rotation: instant.
        assert!(
            t.rotation.angle_between(Quat::from_rotation_y(1.2)) < 1e-6,
            "rotation must snap in one frame"
        );
        // Position: moved toward the target but not all the way there.
        assert!(
            t.translation.length() > 0.0
                && t.translation.distance(Vec3::new(5.0, -3.0, 2.0) + crate::renderer::bridge::EYE_OFFSET) > 0.008,
            "position must lerp partway, not snap: {:?}",
            t.translation
        );
    }
}
