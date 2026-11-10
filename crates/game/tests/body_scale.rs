//! The stud world's player body (`movement::BodyScale::STUD`): a 2.4 m minifigure
//! whose jumps and falls take the base parameters' time with lengths ×4/3.
use game::Rollback;
use game::movement::{BodyScale, FALL_GRAVITY, GRAVITY, MoveParams, UNITS_PER_METER};

#[test]
fn stud_scale_is_a_2_4_m_minifigure() {
    let s = BodyScale::STUD;
    // The collision capsule is 2.2 m x 0.8 m (fits a 1 m x 2.4 m corridor over floor
    // studs); the figure's eye and jump scale by 4/3.
    let h = 2.0 * (s.capsule_half_height() + s.capsule_radius()) / UNITS_PER_METER;
    assert!((h - 2.2).abs() < 1e-4, "height {h} m");
    assert!((s.capsule_radius() / UNITS_PER_METER - 0.4).abs() < 1e-6);
    assert_eq!(s.factor, 4.0 / 3.0);
}

/// Peak height jump² / (2 g) and airtime (rise jump / g, then the fall from
/// the peak under the fall gravity), in metres and seconds.
fn arc(p: MoveParams) -> (f32, f32) {
    let (g, fall) = (GRAVITY * p.gravity_scale, FALL_GRAVITY * p.gravity_scale);
    let peak = p.jump_speed * p.jump_speed / (2.0 * g);
    (peak, p.jump_speed / g + (2.0 * peak / fall).sqrt())
}

#[test]
fn stud_jump_peak_scales_with_body() {
    // The unscaled base parameters (a 1.8 m body).
    let (base_peak, base_air) = arc(MoveParams::PLAYER);
    let (stud_peak, stud_air) = arc(BodyScale::STUD.move_params(MoveParams::PLAYER));
    assert!(
        (stud_peak / base_peak - 4.0 / 3.0).abs() < 1e-4,
        "peak ratio {}",
        stud_peak / base_peak
    );
    assert!(
        (stud_air - base_air).abs() < 1e-4,
        "airtime {stud_air} s vs {base_air} s"
    );
}

/// The capsule collider of client `client`'s player body: (half height, radius).
fn player_capsule(r: &Rollback, client: game::ClientId) -> (f32, f32) {
    let e = *r.data.player_entites.get(&client).expect("player body");
    let handle = (*r.data.ecs.rigidbody.try_get(e)).expect("player rigidbody");
    let body = r.data.physics.bodies.get(handle).unwrap();
    let capsule = body
        .colliders()
        .iter()
        .find_map(|&c| {
            r.data
                .physics
                .colliders
                .get(c)
                .unwrap()
                .shape()
                .as_capsule()
                .copied()
        })
        .expect("player capsule collider");
    (capsule.half_height(), capsule.radius)
}

#[test]
fn stud_player_capsule_is_scaled() {
    let mut r = Rollback::new(None);
    r.set_body_scale_raw(BodyScale::STUD);
    r.new_transaction();
    r.create_player_safe(7);
    let (half, radius) = player_capsule(&r, 7);
    let s = BodyScale::STUD;
    assert_eq!(half, s.capsule_half_height());
    assert_eq!(radius, s.capsule_radius());
    assert_eq!(r.body_scale(), BodyScale::STUD);
}
