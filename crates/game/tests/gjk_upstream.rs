//! Upstream parry GJK/EPA fixes (PR #437 + #420 subset), first hand-ported
//! into the nalgebra-era fork and now carried by parry 0.31.1 itself. Each
//! test is a port of the upstream regression test; they were written before
//! the rapier 0.36 switch and must stay green across it (upgrade plan
//! Phase 5), which is how we know upstream's versions behave like the ones we
//! validated.
use game::parry::math::{Pose, Vector};
use game::parry::query::details::cast_shapes_support_map_support_map;
use game::parry::query::{self, ShapeCastOptions};
use game::parry::shape::{Ball, Capsule, Cuboid, Cylinder, HalfSpace, Segment, Shape};

fn r(x: f32) -> f32 {
    x
}

fn v(x: f32, y: f32, z: f32) -> Vector {
    Vector::new(r(x), r(y), r(z))
}

fn at(x: f32, y: f32, z: f32) -> Pose {
    Pose::translation(r(x), r(y), r(z))
}

/// #193: a shape slightly sunk into the +Y face of a large cuboid got wildly
/// wrong shape-cast normals (e.g. [0.707, 0.027, 0.707] instead of ±Y).
#[test]
fn issue_193_slightly_penetrating_casts_report_the_face_normal() {
    let cap = Capsule::new_y(r(5.0), r(1.0));
    let ball = Ball::new(r(1.0));
    let cases: [(&dyn Shape, Pose); 4] = [
        (&cap, at(0.0, 5.0 - 0.01, 0.0)),
        (&cap, at(0.0, 5.0 - 0.02, 0.0)),
        (&ball, at(0.0, 1.0 - 0.2, 0.0)),
        (&ball, at(0.0, 1.0 - 0.200001, 0.0)),
    ];
    let g2 = Cuboid::new(v(20.0, 1.0, 20.0));
    let pos2 = at(0.0, -1.0, 0.0);
    // Velocity not being straight down played a part in the original failures.
    let vel1 = v(0.1, 1.0, 0.0);
    let options = ShapeCastOptions {
        compute_impact_geometry_on_penetration: true,
        ..Default::default()
    };
    for (i, (g1, pos1)) in cases.iter().enumerate() {
        let hit = query::cast_shapes(pos1, vel1, *g1, &pos2, Vector::ZERO, &g2, options)
            .unwrap()
            .expect("the penetrating cast should return a hit");
        assert!(
            (hit.normal2 - Vector::Y).length() < 1.0e-4,
            "case {i}: normal2 {:?} should be +Y",
            hit.normal2
        );
        assert!(
            (hit.normal1 + Vector::Y).length() < 1.0e-4,
            "case {i}: normal1 {:?} should be -Y",
            hit.normal1
        );
    }
}

/// #396: degenerate support directions (a symmetry axis) stalled the GJK
/// simplex deep inside the CSO and the stall exit reported "disjoint".
#[test]
fn issue_396_coaxial_cylinder_capsule_intersect() {
    let iso = Pose::IDENTITY;
    let big = Cylinder::new(r(50.0), r(100.0));
    let small = Capsule::new(Vector::new(r(0.0), r(-50.0), r(0.0)), Vector::new(r(0.0), r(50.0), r(0.0)), r(10.0));
    assert_eq!(query::intersection_test(&iso, &big, &iso, &small).map(|i| i.intersecting), Ok(true));
}

/// #396, the shapes the game has: the same tall capsule inside a cuboid.
#[test]
fn issue_396_coaxial_cuboid_capsule_intersect() {
    let iso = Pose::IDENTITY;
    let big = Cuboid::new(v(50.0, 100.0, 50.0));
    let small = Capsule::new(Vector::new(r(0.0), r(-50.0), r(0.0)), Vector::new(r(0.0), r(50.0), r(0.0)), r(10.0));
    assert_eq!(query::intersection_test(&iso, &big, &iso, &small).map(|i| i.intersecting), Ok(true));
}

/// #396 guard: separated coaxial pairs must stay disjoint.
#[test]
fn issue_396_separated_coaxial_pairs_stay_disjoint() {
    let small = Capsule::new(Vector::new(r(0.0), r(-50.0), r(0.0)), Vector::new(r(0.0), r(50.0), r(0.0)), r(10.0));
    let big_cub = Cuboid::new(v(50.0, 100.0, 50.0));
    let big_cyl = Cylinder::new(r(50.0), r(100.0));
    let iso = Pose::IDENTITY;
    // Capsule reaches 60 below its centre; cuboid top at 100 → disjoint above dy = 160.
    assert_eq!(query::intersection_test(&iso, &big_cub, &at(0.0, 161.0, 0.0), &small).map(|i| i.intersecting), Ok(false));
    // Cylinder top at 50 → disjoint above dy = 110.
    for dy in [111.0, 150.0, 400.0] {
        assert_eq!(
            query::intersection_test(&iso, &big_cyl, &at(0.0, dy, 0.0), &small).map(|i| i.intersecting),
            Ok(false),
            "axial offset {dy} should be disjoint"
        );
    }
}

/// #429: the TOI came back short by an amount scaling with the target's
/// extent (large support coordinates orthogonal to the cast kept the GJK
/// upper bound from decreasing; the last-chance exit returned the unrefined
/// lower bound). Up to ~3.6 mm on our 8–16-unit cuboids.
#[test]
fn issue_429_shape_cast_toi_accuracy_does_not_scale_with_shape_extent() {
    const BALL_RADIUS: f32 = 0.5166;
    const MAX_TOI_ERROR: f32 = 2.0e-4; // 0.2 mm.
    let ball = Ball::new(r(BALL_RADIUS));
    let down = v(0.0, -1.0, 0.0);
    let mut errors = Vec::new();
    for half_extent in [5.0, 50.0, 500.0] {
        let ground = Cuboid::new(v(half_extent, 0.5, half_extent));
        let ground_pose = at(0.0, -0.5, 0.0);
        let mut max_toi_error = 0.0f32;
        // Sub-millimetre start offsets at several lateral positions (upstream's sweep).
        for i in 0..200 {
            let y = 1.177 + i as f32 * 5.0e-5;
            for (x, z) in [(1.4, 2.8), (-1.4, -0.4), (1.4, -2.0), (-1.4, 2.0)] {
                let hit = query::cast_shapes(
                    &ground_pose,
                    Vector::ZERO,
                    &ground,
                    &at(x, y, z),
                    down,
                    &ball,
                    ShapeCastOptions::with_max_time_of_impact(r(2.0)),
                )
                .unwrap()
                .expect("the downward cast should hit the cuboid");
                let expected = y - BALL_RADIUS;
                max_toi_error = max_toi_error.max((hit.time_of_impact - expected).abs());
            }
        }
        errors.push((half_extent, max_toi_error));
    }
    for (half_extent, err) in errors {
        assert!(
            err <= MAX_TOI_ERROR,
            "shape-cast TOI error {err} exceeded {MAX_TOI_ERROR} for half-extent {half_extent}"
        );
    }
}

/// #180 (C2, upstream 79833ff91b): a grazing cast returned `None` because
/// `minkowski_ray_cast` compared `min_bound` against an absolute tolerance
/// while its rounding error grows with the support coordinates. Upstream's 2D
/// MRE embedded in the z = 0 plane. Found by probing ulps around the MRE with
/// the scaled tolerance reverted: x = 15.689517 and 15.689518 (bits
/// 0x417b0843 / 0x417b0844) miss without it. Neighbours stay as guards.
/// Calls the support-map cast directly (the path `minkowski_ray_cast` is on;
/// `intersection_test` below never reaches it).
#[test]
fn issue_180_grazing_cast_is_not_ulp_sensitive() {
    let segment = Segment::new(Vector::new(r(1.0), r(1.0), r(0.0)), Vector::new(r(124.0), r(1.0), r(0.0)));
    let cuboid = Cuboid::new(v(2.0, 2.0, 2.0));
    let vel = v(0.0081385, -0.999966, 0.0);
    for bits in [0x417b_0842u32, 0x417b_0843, 0x417b_0844, 0x417b_0845] {
        let x = f32::from_bits(bits);
        let hit = cast_shapes_support_map_support_map(
            &at(x, 54.00709, 0.0),
            vel,
            &segment,
            &cuboid,
            ShapeCastOptions::default(),
        );
        assert!(hit.is_some(), "grazing cast missed at x = {x} ({bits:#x})");
    }
}

/// #70: sweeping a capsule/ball through the top of a large cuboid
/// (coplanar with a halfspace) gave false negatives. `intersection_test` runs
/// `gjk::closest_points`, so what fixed this is the stall classification
/// (#193/#396, fa08f24), not the #180 ray-cast tolerance: that one is pinned
/// by `issue_180_grazing_cast_is_not_ulp_sensitive` above.
#[test]
fn issue_70_capsule_cuboid_sweep_has_no_false_negatives() {
    let capsule = Capsule::new(Vector::new(r(0.0), r(-0.5), r(0.0)), Vector::new(r(0.0), r(0.5), r(0.0)), r(0.5));
    let ball = Ball::new(r(0.5));
    let halfspace = HalfSpace::new(Vector::Y);
    let cuboid = Cuboid::new(v(50.0, 50.0, 50.0));
    let cuboid_pos = at(0.0, -50.0, 0.0);
    let id = Pose::IDENTITY;

    let steps = 200;
    let (y_min, y_max) = (-0.5f32, 0.5f32);
    let step_size = (y_max - y_min) / steps as f32;
    let mut counts = [0; 4];
    for step in 0..steps {
        let pos = at(0.0, y_min + step_size * step as f32, 0.0);
        let hits = [
            query::intersection_test(&pos, &capsule, &id, &halfspace).unwrap().intersecting,
            query::intersection_test(&pos, &capsule, &cuboid_pos, &cuboid).unwrap().intersecting,
            query::intersection_test(&pos, &ball, &id, &halfspace).unwrap().intersecting,
            query::intersection_test(&pos, &ball, &cuboid_pos, &cuboid).unwrap().intersecting,
        ];
        for (c, h) in counts.iter_mut().zip(hits) {
            *c += h as usize;
        }
    }
    assert_eq!(counts, [steps; 4], "[capsule-halfspace, capsule-cuboid, ball-halfspace, ball-cuboid]");
}

const EPS_315: f32 = 1.0e-5;

/// #315: two exactly touching cuboids: GJK's first support point lands on the
/// origin, and EPA's 0-dimensional fallback fabricated witnesses at the
/// origin. Witnesses must be on the shared face, normal ±X.
#[test]
fn issue_315_exactly_touching_cuboids() {
    let b = Cuboid::new(v(0.5, 0.5, 0.5));
    let pos1 = at(0.5, 0.5, 0.5);
    let pos2 = at(1.5, 0.5, 0.5);
    let c = query::contact(&pos1, &b, &pos2, &b, r(0.0)).unwrap().unwrap();
    assert!(c.dist.abs() < EPS_315, "expected touching dist, got {}", c.dist);
    assert!((c.point1.x - 1.0).abs() < EPS_315, "point1 not on the touching plane: {:?}", c.point1);
    assert!((c.point2.x - 1.0).abs() < EPS_315, "point2 not on the touching plane: {:?}", c.point2);
    for pt in [c.point1, c.point2] {
        assert!(
            (0.0..=1.0).contains(&pt.y) && (0.0..=1.0).contains(&pt.z),
            "witness point outside the touching faces: {pt:?}"
        );
    }
    assert!((c.normal1 - Vector::X).length() < EPS_315, "normal1 {:?}", c.normal1);
    assert!((c.normal2 + Vector::X).length() < EPS_315, "normal2 {:?}", c.normal2);
}

/// #315, arguments flipped: the normal flips too.
#[test]
fn issue_315_exactly_touching_cuboids_flipped() {
    let b = Cuboid::new(v(0.5, 0.5, 0.5));
    let pos1 = at(0.5, 0.5, 0.5);
    let pos2 = at(1.5, 0.5, 0.5);
    let c = query::contact(&pos2, &b, &pos1, &b, r(0.0)).unwrap().unwrap();
    assert!(c.dist.abs() < EPS_315, "expected touching dist, got {}", c.dist);
    assert!((c.point1.x - 1.0).abs() < EPS_315, "point1 {:?}", c.point1);
    assert!((c.point2.x - 1.0).abs() < EPS_315, "point2 {:?}", c.point2);
    assert!((c.normal1 + Vector::X).length() < EPS_315, "normal1 {:?}", c.normal1);
}

/// Parity pin, not a #315 test: the player capsule resting on a block's top
/// face, the contact pair the game has most. Capsule vs cuboid goes through
/// support-map GJK (a ball would take point projection instead). At ~1e-5
/// separation GJK's closest-points direction is ~1.6° off the face normal;
/// pristine parry 0.25.3 and parry 0.31.1 both give exactly the values below,
/// so after the rapier 0.36 switch this must still match within 1e-6.
///
/// #315's EPA bootstrap is covered by the two cuboid tests above, which do
/// reach it. The same capsule centred exactly over the block (x = z = 0)
/// stops at distance exactly 0 and returns a stale 45° direction in the fork
/// and in 0.25.3; 0.31.1 gets +Y. The upgrade plan (Task 5.4) adds that case
/// as a must-pass after the switch.
#[test]
fn capsule_touching_cuboid_matches_upstream() {
    let block = Cuboid::new(v(8.0, 8.0, 8.0));
    let capsule = Capsule::new_y(r(0.7), r(0.3));
    // Block top face at y = 0; capsule bottom (centre y - 0.7 - 0.3) on it.
    let c = query::contact(&at(0.0, -8.0, 0.0), &block, &at(1.0, 1.0, -2.0), &capsule, r(0.0))
        .unwrap()
        .unwrap();
    const TOL: f32 = 1.0e-6;
    let close = |got: Vector, want: [f32; 3]| (got - v(want[0], want[1], want[2])).length() < TOL;
    assert!((c.dist - 1.2409591e-5).abs() < TOL, "dist {}", c.dist);
    assert!(close(c.normal1, [-0.02856119, 0.9995807, -0.0047601983]), "normal1 {:?}", c.normal1);
    assert!(close(c.point1, [1.0015448, 0.0, -2.000366]), "point1 {:?}", c.point1);
    assert!(close(c.point2, [1.0015442, 1.2397766e-5, -2.000366]), "point2 {:?}", c.point2);
}

/// The case above with the capsule centred exactly over the block: it stops
/// at distance exactly 0, where the nalgebra-era fork (like pristine parry
/// 0.25.3) returned a stale 45° direction. parry 0.31.1 gets the face normal
/// (upgrade plan Task 5.4 Step 1b).
#[test]
fn capsule_centred_touching_cuboid_gets_face_normal() {
    let block = Cuboid::new(v(8.0, 8.0, 8.0));
    let capsule = Capsule::new_y(r(0.7), r(0.3));
    let c = query::contact(&at(0.0, -8.0, 0.0), &block, &at(0.0, 1.0, 0.0), &capsule, r(0.0))
        .unwrap()
        .expect("touching is a contact at prediction 0");
    assert!((c.normal1 - Vector::Y).length() < 1.0e-4, "normal1 {:?}", c.normal1);
}

/// #315 guard: overlapping cuboids keep a penetration depth and witnesses on
/// the penetrating faces.
#[test]
fn issue_315_overlapping_cuboids() {
    let b = Cuboid::new(v(0.5, 0.5, 0.5));
    let c = query::contact(&at(0.5, 0.5, 0.5), &b, &at(1.2, 0.5, 0.5), &b, r(0.0))
        .unwrap()
        .unwrap();
    assert!((c.dist + 0.3).abs() < 1.0e-4, "expected dist ~ -0.3, got {}", c.dist);
    assert!((c.normal1 - Vector::X).length() < EPS_315);
    assert!((c.point1.x - 1.0).abs() < 1.0e-4);
    assert!((c.point2.x - 0.7).abs() < 1.0e-4);
}

/// #415 (upstream PR #420): EPA rejected valid faces with an absolute
/// tolerance, although face distances carry rounding error proportional to
/// the coordinate magnitudes. A capsule sunk 1.3 mm into a 100-unit-wide
/// cuboid got no contact at all. Found by a sweep; EPA's degenerate-face
/// tolerance now scales with the simplex coordinates.
#[test]
fn issue_415_capsule_sunk_into_wide_cuboid_gets_a_contact() {
    let ground = Cuboid::new(v(50.0, 0.5, 50.0));
    let cap = Capsule::new_y(r(0.4), r(0.3));
    let pen = 0.0013;
    let c = query::contact(&at(0.0, -0.5, 0.0), &ground, &at(0.1, 0.7 - pen, -0.1), &cap, r(0.0))
        .unwrap()
        .expect("a sunk capsule must get a contact");
    assert!((c.dist + pen).abs() < 1.0e-4, "dist {} (expected {})", c.dist, -pen);
    assert!((c.normal1 - Vector::Y).length() < 1.0e-3, "normal1 {:?}", c.normal1);
}
