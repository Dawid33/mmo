use game::na::Vector3;
use game::parry::math::Real;
use game::na::UnitQuaternion;
use ordered_float::OrderedFloat;

#[test]
fn look_delta_matches_minecraft_cubic_formula() {
    // Pin the documented curve independent of the constant's value.
    let f = game::LOOK_SENSITIVITY * 0.6 + 0.2;
    let expected = 100.0 * game::LOOK_BASE_X * (f * f * f * 8.0);
    assert!((game::look_delta_rad(100.0, game::LOOK_BASE_X) - expected).abs() < 1e-6);
    // Zero pixels => zero rotation.
    assert_eq!(game::look_delta_rad(0.0, game::LOOK_BASE_X), 0.0);
}

#[test]
fn pitch_clamps_and_yaw_is_free() {
    let mut yaw = OrderedFloat(0.0_f32);
    let mut pitch = OrderedFloat(0.0_f32);
    // Slam pitch up hard, many ticks.
    for _ in 0..10_000 {
        let (y, p) = game::apply_look(yaw, pitch, 0.0, 10_000.0);
        yaw = y;
        pitch = p;
    }
    assert!(pitch <= game::PITCH_LIMIT, "pitch exceeded +limit: {}", pitch);
    assert!(pitch >= OrderedFloat(1.0), "pitch should reach near the top");
    // Now slam down; must clamp at -limit.
    for _ in 0..20_000 {
        let (y, p) = game::apply_look(yaw, pitch, 0.0, -10_000.0);
        yaw = y;
        pitch = p;
    }
    assert!(pitch >= -game::PITCH_LIMIT, "pitch exceeded -limit: {}", pitch);
    // Yaw accumulated freely (no clamp), stays finite.
    assert!(yaw.into_inner().is_finite());
}

#[test]
fn look_quat_never_rolls() {
    // The camera-right vector must stay horizontal for any yaw/pitch.
    for &yaw in &[0.0_f32, 0.7, 3.0, -2.1] {
        for &pitch in &[0.0_f32, 0.5, -1.4, 1.4] {
            let q = game::look_quat(OrderedFloat(yaw), OrderedFloat(pitch));
            let right = q.transform_vector(&Vector3::<Real>::x());
            assert!(
                right.y.into_inner().abs() < 1e-6,
                "roll detected at yaw={yaw} pitch={pitch}: right.y={}",
                right.y
            );
        }
    }
}

#[test]
fn look_quat_identity_at_zero() {
    let q = game::look_quat(OrderedFloat(0.0), OrderedFloat(0.0));
    assert_eq!(q, UnitQuaternion::identity());
}
