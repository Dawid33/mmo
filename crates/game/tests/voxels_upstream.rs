//! parry #404 / #373 (upstream PR #437): voxels contact manifolds see
//! speculative contacts within the prediction distance, and voxels shape
//! casts report witness1 in shape1's frame. Both fixed locally first.
//!
//! Must stay green across the rapier 0.36 switch (upgrade plan Phase 5).
use game::parry::math::{Isometry, Point, Real, Vector};
use game::parry::query::{
    self, ContactManifold, DefaultQueryDispatcher, PersistentQueryDispatcher, ShapeCastOptions,
};
use game::parry::shape::{Ball, Capsule, Voxels};

fn r(x: f32) -> Real {
    Real::from(x)
}

fn floor(nx: i32, nz: i32) -> Voxels {
    let pts: Vec<Point<i32>> = (0..nx)
        .flat_map(|x| (0..nz).map(move |z| Point::new(x, 0, z)))
        .collect();
    Voxels::new(Vector::repeat(r(1.0)), &pts)
}

/// #404: `query::contact` has no voxels arm, so this goes through the
/// contact-manifold path the fix lives in (what rapier's narrow phase calls).
#[test]
fn voxels_manifold_sees_contacts_within_prediction() {
    let vox = floor(8, 8);
    let cap = Capsule::new_y(r(0.3), r(0.2));
    // Capsule bottom at y = 1.55 - 0.5 = 1.05; floor top at y = 1.0 → gap 0.05 < prediction 0.1.
    let pos12 = Isometry::translation(r(4.0), r(1.55), r(4.0));
    let mut manifolds: Vec<ContactManifold<(), ()>> = Vec::new();
    DefaultQueryDispatcher
        .contact_manifolds(&pos12, &vox, &cap, r(0.1), &mut manifolds, &mut None)
        .unwrap();
    let dist = manifolds
        .iter()
        .flat_map(|m| m.points.iter())
        .map(|p| p.dist.0)
        .min_by(f32::total_cmp)
        .expect("contact within prediction must be reported");
    assert!((dist - 0.05).abs() < 1.0e-3, "dist {dist}");
}

/// #373: witness1 is in shape1's (the voxels') frame, not the hit voxel's
/// own frame (which would put it at (0, 0.5, 0)). Tolerances are upstream's
/// (`issue_373_voxels_shape_cast_witness.rs`): the GJK witness on a flat face
/// is only good to a few mm (measured 3.2e-3 here).
#[test]
fn voxels_shape_cast_witness1_is_in_shape1_frame() {
    let pts: Vec<Point<i32>> = (0..20).map(|x| Point::new(x, 0, 0)).collect();
    let vox = Voxels::new(Vector::repeat(r(1.0)), &pts);
    let ball = Ball::new(r(0.5));
    let pos_vox = Isometry::identity();
    let pos_ball = Isometry::translation(r(17.5), r(3.5), r(0.5));
    let vel_ball = Vector::new(r(0.0), r(-1.0), r(0.0));
    let opts = ShapeCastOptions::with_max_time_of_impact(r(10.0));
    let near = |a: Vector<Real>, b: Vector<Real>| (a - b).norm().0 < 1.0e-2;

    let hit = query::cast_shapes(&pos_vox, &Vector::zeros(), &vox, &pos_ball, &vel_ball, &ball, opts)
        .unwrap()
        .expect("hit");
    assert!(
        (hit.time_of_impact.0 - 2.0).abs() < 1.0e-4,
        "toi {}",
        hit.time_of_impact.0
    );
    let w = hit.witness1.coords;
    assert!(near(w, Vector::new(r(17.5), r(1.0), r(0.5))), "witness1 {w:?}");
    assert!(near(hit.witness2.coords, Vector::new(r(0.0), r(-0.5), r(0.0))));
    assert!(near(hit.normal1.into_inner(), Vector::y()));
    assert!(near(hit.normal2.into_inner(), -Vector::y()));

    // The swapped shape order must agree with the direct one.
    let swapped = query::cast_shapes(&pos_ball, &vel_ball, &ball, &pos_vox, &Vector::zeros(), &vox, opts)
        .unwrap()
        .expect("swapped hit");
    assert!((swapped.time_of_impact.0 - hit.time_of_impact.0).abs() < 1.0e-2);
    assert!(near(swapped.witness1.coords, hit.witness2.coords));
    assert!(near(swapped.witness2.coords, hit.witness1.coords));
    assert!(near(swapped.normal1.into_inner(), hit.normal2.into_inner()));
    assert!(near(swapped.normal2.into_inner(), hit.normal1.into_inner()));
}
