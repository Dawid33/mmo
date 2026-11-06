//! Collider shapes and explicit mass properties (`game::mass_props`): the
//! analytic primitives agree with parry, a compound's principal frame comes
//! from our own deterministic Jacobi solve, and a dynamic body's mass is
//! carried explicitly — density-0 colliders plus additional mass properties,
//! which rapier hands back bit-unchanged — so it survives insertion, a step
//! and a seam round trip without double counting.
use crate::common::{install_real_block_props, r0};
use game::parry::mass_properties::MassProperties;
use game::rapier3d::math::Vector;
use game::{BodyKind, BodyState, Carried, ColliderSpec, EntityBundle, EntityId, EntityKind, GameEventKind, Pose, Region, Rollback};

/// Every field's bits, so "equal" means bit-equal.
fn bits(mp: &MassProperties) -> [u32; 11] {
    let (c, i, q) = (mp.local_com, mp.inv_principal_inertia, mp.principal_inertia_local_frame);
    [
        c.x.to_bits(),
        c.y.to_bits(),
        c.z.to_bits(),
        mp.inv_mass.to_bits(),
        i.x.to_bits(),
        i.y.to_bits(),
        i.z.to_bits(),
        q.x.to_bits(),
        q.y.to_bits(),
        q.z.to_bits(),
        q.w.to_bits(),
    ]
}

/// A dynamic body state at `pose` carrying `mass` explicitly, floating (no
/// world gravity, scale 0).
fn dynamic_state(pose: Pose, mass: MassProperties) -> BodyState {
    BodyState {
        kind: BodyKind::Dynamic,
        mass: Some(mass),
        gravity_scale: 0.0,
        angular_damping: 0.0,
        ..BodyState::kinematic(pose)
    }
}

fn dynamic_bundle(serial: u64, pose: Pose, collider: ColliderSpec, mass: MassProperties) -> EntityBundle {
    EntityBundle {
        id: EntityId { birth_region: r0(), serial },
        kind: EntityKind::Slime { size: 1 },
        body: dynamic_state(pose, mass),
        collider,
        carried: Carried::default(),
        client: None,
        pending_inputs: Vec::new(),
    }
}

fn region() -> Region {
    install_real_block_props();
    let (send, _recv) = crossbeam::channel::unbounded();
    Region::new(Rollback::new(Some(send.clone())), Some(send), r0(), None)
}

fn tick(region: &mut Region) {
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
}

fn mid_region() -> Pose {
    let h = game::REGION_SIZE * 0.5;
    Pose::from_translation(Vector::new(h, 400.0, h))
}

/// The live body's total mass properties.
fn live_mprops(region: &Region, id: EntityId) -> MassProperties {
    let r = region.data();
    let e = *r.id_index.get(&id).expect("the body lives here");
    let h = r.ecs.rigidbody.try_get(e).expect("a body");
    r.physics.bodies.get(h).unwrap().mass_properties().local_mprops
}

/// The Task 2.2 defect: `BodyState::of` read the TOTAL mass (collider
/// contributions included) and `build()` added it again as additional mass
/// over density-carrying colliders. A dynamic body must keep exactly the mass
/// it carries — after the arrival's step, and after a second crossing.
#[test]
fn dynamic_mass_is_not_double_counted_across_a_round_trip() {
    let spec = ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 };
    let mp = MassProperties::from_capsule(
        0.5,
        Vector::new(0.0, -8.0, 0.0),
        Vector::new(0.0, 8.0, 0.0),
        6.4,
    );
    let id = EntityId { birth_region: r0(), serial: 7 };

    let mut a = region();
    a.with_data(|r| {
        r.apply_arrival(dynamic_bundle(7, mid_region(), spec.clone(), mp));
    });
    tick(&mut a);
    assert_eq!(bits(&live_mprops(&a, id)), bits(&mp), "mass after the first arrival's step");

    let mut bundle = None;
    a.with_data(|r| {
        let e = *r.data.id_index.get(&id).unwrap();
        bundle = Some(r.extract_entity_safe(e));
    });
    let bundle = bundle.unwrap();
    assert_eq!(bundle.body.mass.map(|m| bits(&m)), Some(bits(&mp)), "the bundle carries the body's own mass");

    let mut b = region();
    b.with_data(|r| {
        r.apply_arrival(bundle);
    });
    tick(&mut b);
    assert_eq!(bits(&live_mprops(&b, id)), bits(&mp), "mass after the second arrival's step");
}

fn rel_close(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol * a.abs().max(b.abs()).max(1e-12)
}

fn assert_vec_close(a: Vector, b: Vector, tol: f32, what: &str) {
    for (x, y) in [(a.x, b.x), (a.y, b.y), (a.z, b.z)] {
        assert!(rel_close(x, y, tol), "{what}: {a:?} vs {b:?}");
    }
}

/// Inertia tensors compared entry by entry, relative to the largest entry.
fn assert_mat_close(a: game::math::Mat3, b: game::math::Mat3, tol: f32, what: &str) {
    let scale = a.to_cols_array().iter().chain(b.to_cols_array().iter()).fold(0.0f32, |m, v| m.max(v.abs()));
    for (x, y) in a.to_cols_array().iter().zip(b.to_cols_array().iter()) {
        assert!((x - y).abs() <= tol * scale, "{what}:\n{a:?}\nvs\n{b:?}");
    }
}

#[test]
fn cuboid_and_ball_match_parry() {
    use game::mass_props::mass_properties;
    use game::parry::shape::{Ball, Capsule, Cuboid, Shape};
    let d = game::mass_props::BODY_DEFAULT_DENSITY;
    let he = Vector::new(8.0, 3.0, 13.5);
    let cases: [(ColliderSpec, MassProperties); 3] = [
        (ColliderSpec::Cuboid { half_extents: he }, Cuboid::new(he).mass_properties(d)),
        (ColliderSpec::Ball { radius: 5.5 }, Ball::new(5.5).mass_properties(d)),
        (ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 }, Capsule::new_y(8.0, 6.4).mass_properties(d)),
    ];
    for (spec, parry) in cases {
        let ours = mass_properties(&spec, d);
        assert!(rel_close(ours.mass(), parry.mass(), 1e-5), "{spec:?} mass {} vs {}", ours.mass(), parry.mass());
        assert_vec_close(ours.local_com, parry.local_com, 1e-5, "com");
        assert_vec_close(
            ours.principal_inertia(),
            parry.principal_inertia(),
            1e-5,
            &format!("{spec:?} principal inertia"),
        );
    }
}

/// Two cuboids in an L, the second rotated 30° about Y: our parallel-axis sum
/// + Jacobi frame against parry's compound (glamx `SymmetricEigen3`).
fn l_shape() -> ColliderSpec {
    let rot = game::math::Quat::from_rotation_y(core::f32::consts::FRAC_PI_6);
    ColliderSpec::Compound(vec![
        (Pose::from_translation(Vector::new(0.0, 8.0, 0.0)), ColliderSpec::Cuboid { half_extents: Vector::new(8.0, 8.0, 8.0) }),
        (
            Pose::from_parts(Vector::new(24.0, 4.0, 2.0), rot),
            ColliderSpec::Cuboid { half_extents: Vector::new(16.0, 4.0, 6.0) },
        ),
    ])
}

#[test]
fn compound_matches_parry_within_tolerance() {
    use game::mass_props::{mass_properties, BODY_DEFAULT_DENSITY as D};
    let spec = l_shape();
    let ours = mass_properties(&spec, D);
    let parry = spec.shared_shape().mass_properties(D);
    assert!(rel_close(ours.mass(), parry.mass(), 1e-4), "mass {} vs {}", ours.mass(), parry.mass());
    assert_vec_close(ours.local_com, parry.local_com, 1e-4, "com");
    assert_mat_close(ours.reconstruct_inertia_matrix(), parry.reconstruct_inertia_matrix(), 1e-4, "inertia tensor");
}

#[test]
fn jacobi_is_orthonormal_and_right_handed() {
    use game::math::{Mat3, Vec3};
    use game::mass_props::jacobi_eigen_sym3;
    let inputs: [[[f32; 3]; 3]; 3] = [
        [[4.0, 1.0, 0.5], [1.0, 3.0, 0.25], [0.5, 0.25, 2.0]],
        [[1.0e6, -3.0e4, 2.0e3], [-3.0e4, 2.5e6, 7.5e4], [2.0e3, 7.5e4, 1.8e6]],
        // Already diagonal, and a repeated eigenvalue.
        [[5.0, 0.0, 0.0], [0.0, 5.0, 0.0], [0.0, 0.0, 9.0]],
    ];
    for m in inputs {
        let (values, rot) = jacobi_eigen_sym3(m);
        let r = Mat3::from_quat(rot);
        assert!((r.determinant() - 1.0).abs() < 1e-5, "det {}", r.determinant());
        let rebuilt = r * Mat3::from_diagonal(values) * r.transpose();
        // Row-major input -> glam's column-major Mat3 (symmetric anyway).
        let input = Mat3::from_cols(Vec3::from(m[0]), Vec3::from(m[1]), Vec3::from(m[2]));
        assert_mat_close(rebuilt, input, 1e-5, "R diag Rᵀ");
    }
}

/// A dynamic body over a density-0 compound with explicit additional mass
/// properties keeps them bit-exact through insertion and a step.
#[test]
fn explicit_mass_survives_insertion_bit_exact() {
    let spec = l_shape();
    let mp = game::mass_props::mass_properties(&spec, game::mass_props::BODY_DEFAULT_DENSITY);
    let id = EntityId { birth_region: r0(), serial: 3 };
    let mut a = region();
    a.with_data(|r| {
        r.apply_arrival(dynamic_bundle(3, mid_region(), spec.clone(), mp));
    });
    tick(&mut a);
    assert_eq!(bits(&live_mprops(&a, id)), bits(&mp));
    let r = a.data();
    let e = *r.id_index.get(&id).unwrap();
    let h = r.ecs.rigidbody.try_get(e).unwrap();
    for ch in r.physics.bodies.get(h).unwrap().colliders() {
        assert_eq!(r.physics.colliders.get(*ch).unwrap().mass(), 0.0, "a dynamic body's colliders carry no mass");
    }
}

#[test]
fn collider_spec_read_back_round_trips() {
    let specs = [
        ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        ColliderSpec::Cuboid { half_extents: Vector::new(8.0, 3.0, 13.5) },
        ColliderSpec::Ball { radius: 5.5 },
        l_shape(),
    ];
    for (i, spec) in specs.into_iter().enumerate() {
        let serial = 100 + i as u64;
        let id = EntityId { birth_region: r0(), serial };
        let mp = game::mass_props::mass_properties(&spec, game::mass_props::BODY_DEFAULT_DENSITY);
        for body in [BodyState::kinematic(mid_region()), dynamic_state(mid_region(), mp)] {
            let mut a = region();
            let kind = body.kind;
            a.with_data(|r| {
                r.apply_arrival(EntityBundle { body, ..dynamic_bundle(serial, mid_region(), spec.clone(), mp) });
            });
            let mut out = None;
            a.with_data(|r| {
                let e = *r.data.id_index.get(&id).unwrap();
                out = Some(r.extract_entity_safe(e));
            });
            assert_eq!(out.unwrap().collider, spec, "{kind:?} read-back");
        }
    }
}
