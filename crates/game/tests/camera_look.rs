
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
    let mut yaw = 0.0_f32;
    let mut pitch = 0.0_f32;
    // Mouse up (negative dy) drives pitch toward the +limit (looking up).
    for _ in 0..10_000 {
        let (y, p) = game::apply_look(yaw, pitch, 0.0, -10_000.0);
        yaw = y;
        pitch = p;
    }
    assert!(pitch <= game::PITCH_LIMIT, "pitch exceeded +limit: {}", pitch);
    assert!(pitch >= 1.0, "pitch should reach near the top");
    // Mouse down (positive dy) drives pitch toward the -limit (looking down).
    for _ in 0..20_000 {
        let (y, p) = game::apply_look(yaw, pitch, 0.0, 10_000.0);
        yaw = y;
        pitch = p;
    }
    assert!(pitch >= -game::PITCH_LIMIT, "pitch exceeded -limit: {}", pitch);
    // Yaw accumulated freely (no clamp), stays finite.
    assert!(yaw.is_finite());
}

#[test]
fn look_quat_never_rolls() {
    // The camera-right vector must stay horizontal for any yaw/pitch.
    for &yaw in &[0.0_f32, 0.7, 3.0, -2.1] {
        for &pitch in &[0.0_f32, 0.5, -1.4, 1.4] {
            let q = game::look_quat(yaw, pitch);
            let right = q * game::math::Vec3::X;
            assert!(
                right.y.abs() < 1e-6,
                "roll detected at yaw={yaw} pitch={pitch}: right.y={}",
                right.y
            );
        }
    }
}

#[test]
fn look_quat_identity_at_zero() {
    let q = game::look_quat(0.0, 0.0);
    assert_eq!(q, game::math::Quat::IDENTITY);
}
