use bevy::prelude::*;

use super::bridge::SimTarget;

/// Frame-rate-dependent exponential smoothing toward the sim pose —
/// deliberate parity with the old renderer's lerp constants. If tick
/// timestamps are ever added to SetEntityPosition, replace with
/// two-snapshot interpolation.
pub fn interpolate_transforms(mut query: Query<(&mut Transform, &SimTarget)>) {
    for (mut tf, target) in &mut query {
        let close = tf.translation.distance(target.pos) <= target.pos_snap
            && tf.rotation.angle_between(target.rot) <= target.rot_snap;
        if close {
            tf.translation = target.pos;
            tf.rotation = target.rot;
        } else {
            tf.translation = tf.translation.lerp(target.pos, target.smoothing);
            tf.rotation = tf.rotation.slerp(target.rot, target.smoothing);
        }
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

    #[test]
    fn converges_and_snaps_exactly() {
        let target = SimTarget::camera(Vec3::new(1.0, 0.0, 0.0), Quat::IDENTITY);
        let (mut app, e) = app_with(target, Transform::IDENTITY);
        for _ in 0..200 {
            app.update();
        }
        let t = app.world().entity(e).get::<Transform>().unwrap();
        assert_eq!(t.translation, Vec3::new(1.0, 0.0, 0.0), "must snap bit-exact, not hover nearby");
    }

    #[test]
    fn first_step_moves_partway() {
        let target = SimTarget::body(Vec3::new(10.0, 0.0, 0.0), Quat::IDENTITY);
        let (mut app, e) = app_with(target, Transform::IDENTITY);
        app.update();
        let t = app.world().entity(e).get::<Transform>().unwrap();
        assert!(t.translation.x > 0.0 && t.translation.x < 10.0);
    }
}
