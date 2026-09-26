//! Upstream parry GJK/EPA fixes hand-ported into the vendored fork (PR #437 +
//! #420 subset). Each test is a port of the upstream regression test, in the
//! fork's nalgebra + `OrderedFloat` idiom.
use game::parry::math::{Isometry, Point, Real, Vector};
use game::parry::query::{self, ShapeCastOptions};
use game::parry::shape::{Ball, Capsule, Cuboid, Cylinder, HalfSpace, Shape};

fn r(x: f32) -> Real {
    Real::from(x)
}

fn v(x: f32, y: f32, z: f32) -> Vector<Real> {
    Vector::new(r(x), r(y), r(z))
}

fn at(x: f32, y: f32, z: f32) -> Isometry<Real> {
    Isometry::translation(r(x), r(y), r(z))
}

/// #193: a shape slightly sunk into the +Y face of a large cuboid got wildly
/// wrong shape-cast normals (e.g. [0.707, 0.027, 0.707] instead of ±Y).
#[test]
fn issue_193_slightly_penetrating_casts_report_the_face_normal() {
    let cap = Capsule::new_y(r(5.0), r(1.0));
    let ball = Ball::new(r(1.0));
    let cases: [(&dyn Shape, Isometry<Real>); 4] = [
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
        let hit = query::cast_shapes(pos1, &vel1, *g1, &pos2, &Vector::zeros(), &g2, options)
            .unwrap()
            .expect("the penetrating cast should return a hit");
        assert!(
            (hit.normal2.into_inner() - Vector::y()).norm().0 < 1.0e-4,
            "case {i}: normal2 {:?} should be +Y",
            hit.normal2
        );
        assert!(
            (hit.normal1.into_inner() + Vector::y()).norm().0 < 1.0e-4,
            "case {i}: normal1 {:?} should be -Y",
            hit.normal1
        );
    }
}

/// #396: degenerate support directions (a symmetry axis) stalled the GJK
/// simplex deep inside the CSO and the stall exit reported "disjoint".
#[test]
fn issue_396_coaxial_cylinder_capsule_intersect() {
    let iso = Isometry::identity();
    let big = Cylinder::new(r(50.0), r(100.0));
    let small = Capsule::new(Point::new(r(0.0), r(-50.0), r(0.0)), Point::new(r(0.0), r(50.0), r(0.0)), r(10.0));
    assert_eq!(query::intersection_test(&iso, &big, &iso, &small), Ok(true));
}

/// #396, the shapes the game has: the same tall capsule inside a cuboid.
#[test]
fn issue_396_coaxial_cuboid_capsule_intersect() {
    let iso = Isometry::identity();
    let big = Cuboid::new(v(50.0, 100.0, 50.0));
    let small = Capsule::new(Point::new(r(0.0), r(-50.0), r(0.0)), Point::new(r(0.0), r(50.0), r(0.0)), r(10.0));
    assert_eq!(query::intersection_test(&iso, &big, &iso, &small), Ok(true));
}

/// #396 guard: separated coaxial pairs must stay disjoint.
#[test]
fn issue_396_separated_coaxial_pairs_stay_disjoint() {
    let small = Capsule::new(Point::new(r(0.0), r(-50.0), r(0.0)), Point::new(r(0.0), r(50.0), r(0.0)), r(10.0));
    let big_cub = Cuboid::new(v(50.0, 100.0, 50.0));
    let big_cyl = Cylinder::new(r(50.0), r(100.0));
    let iso = Isometry::identity();
    // Capsule reaches 60 below its centre; cuboid top at 100 → disjoint above dy = 160.
    assert_eq!(query::intersection_test(&iso, &big_cub, &at(0.0, 161.0, 0.0), &small), Ok(false));
    // Cylinder top at 50 → disjoint above dy = 110.
    for dy in [111.0, 150.0, 400.0] {
        assert_eq!(
            query::intersection_test(&iso, &big_cyl, &at(0.0, dy, 0.0), &small),
            Ok(false),
            "axial offset {dy} should be disjoint"
        );
    }
}

/// #70 / #180: sweeping a capsule/ball through the top of a large cuboid
/// (coplanar with a halfspace) gave false negatives from an absolute GJK
/// tolerance on ~100-unit support coordinates.
#[test]
fn issue_70_capsule_cuboid_sweep_has_no_false_negatives() {
    let capsule = Capsule::new(Point::new(r(0.0), r(-0.5), r(0.0)), Point::new(r(0.0), r(0.5), r(0.0)), r(0.5));
    let ball = Ball::new(r(0.5));
    let halfspace = HalfSpace::new(Vector::y_axis());
    let cuboid = Cuboid::new(v(50.0, 50.0, 50.0));
    let cuboid_pos = at(0.0, -50.0, 0.0);
    let id = Isometry::identity();

    let steps = 200;
    let (y_min, y_max) = (-0.5f32, 0.5f32);
    let step_size = (y_max - y_min) / steps as f32;
    let mut counts = [0; 4];
    for step in 0..steps {
        let pos = at(0.0, y_min + step_size * step as f32, 0.0);
        let hits = [
            query::intersection_test(&pos, &capsule, &id, &halfspace).unwrap(),
            query::intersection_test(&pos, &capsule, &cuboid_pos, &cuboid).unwrap(),
            query::intersection_test(&pos, &ball, &id, &halfspace).unwrap(),
            query::intersection_test(&pos, &ball, &cuboid_pos, &cuboid).unwrap(),
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
    assert!(c.dist.0.abs() < EPS_315, "expected touching dist, got {}", c.dist);
    assert!((c.point1.x.0 - 1.0).abs() < EPS_315, "point1 not on the touching plane: {:?}", c.point1);
    assert!((c.point2.x.0 - 1.0).abs() < EPS_315, "point2 not on the touching plane: {:?}", c.point2);
    for pt in [c.point1, c.point2] {
        assert!(
            (0.0..=1.0).contains(&pt.y.0) && (0.0..=1.0).contains(&pt.z.0),
            "witness point outside the touching faces: {pt:?}"
        );
    }
    assert!((c.normal1.into_inner() - Vector::x()).norm().0 < EPS_315, "normal1 {:?}", c.normal1);
    assert!((c.normal2.into_inner() + Vector::x()).norm().0 < EPS_315, "normal2 {:?}", c.normal2);
}

/// #315, arguments flipped: the normal flips too.
#[test]
fn issue_315_exactly_touching_cuboids_flipped() {
    let b = Cuboid::new(v(0.5, 0.5, 0.5));
    let pos1 = at(0.5, 0.5, 0.5);
    let pos2 = at(1.5, 0.5, 0.5);
    let c = query::contact(&pos2, &b, &pos1, &b, r(0.0)).unwrap().unwrap();
    assert!(c.dist.0.abs() < EPS_315, "expected touching dist, got {}", c.dist);
    assert!((c.point1.x.0 - 1.0).abs() < EPS_315, "point1 {:?}", c.point1);
    assert!((c.point2.x.0 - 1.0).abs() < EPS_315, "point2 {:?}", c.point2);
    assert!((c.normal1.into_inner() + Vector::x()).norm().0 < EPS_315, "normal1 {:?}", c.normal1);
}

/// #315, the shape pair the game has most: a ball resting exactly on a
/// cuboid's top face. Witnesses on the face, normal ±Y.
#[test]
fn issue_315_ball_exactly_touching_cuboid() {
    let ground = Cuboid::new(v(8.0, 0.5, 8.0));
    let ball = Ball::new(r(0.5));
    let c = query::contact(&at(0.0, -0.5, 0.0), &ground, &at(1.0, 0.5, -2.0), &ball, r(0.0))
        .unwrap()
        .unwrap();
    assert!(c.dist.0.abs() < EPS_315, "expected touching dist, got {}", c.dist);
    assert!(c.point1.y.0.abs() < EPS_315, "point1 not on the face: {:?}", c.point1);
    assert!(c.point2.y.0.abs() < EPS_315, "point2 not on the face: {:?}", c.point2);
    assert!((c.normal1.into_inner() - Vector::y()).norm().0 < EPS_315, "normal1 {:?}", c.normal1);
}

/// #315 guard: overlapping cuboids keep a penetration depth and witnesses on
/// the penetrating faces.
#[test]
fn issue_315_overlapping_cuboids() {
    let b = Cuboid::new(v(0.5, 0.5, 0.5));
    let c = query::contact(&at(0.5, 0.5, 0.5), &b, &at(1.2, 0.5, 0.5), &b, r(0.0))
        .unwrap()
        .unwrap();
    assert!((c.dist.0 + 0.3).abs() < 1.0e-4, "expected dist ~ -0.3, got {}", c.dist);
    assert!((c.normal1.into_inner() - Vector::x()).norm().0 < EPS_315);
    assert!((c.point1.x.0 - 1.0).abs() < 1.0e-4);
    assert!((c.point2.x.0 - 0.7).abs() < 1.0e-4);
}
