//! Port of `crates/game/tests/voxels_upstream.rs` (Plan 1) to glam, run
//! against the vendored parry 0.31.1 fork: parry #404 / #373 (upstream PR
//! #437). Voxels contact manifolds see speculative contacts within the
//! prediction distance, and voxels shape casts report witness1 in shape1's
//! frame.
//!
//! Upstream 0.31.1 already has both. They stay pinned here because Task 3.2
//! replaces the voxels cast wholesale with the fork's (search domain,
//! internal edges, resting contact): the port must keep #373's witness shift
//! and fill 0.31's `subshape1` with the hit voxel.
use rapier3d::parry::query::{
    self, ContactManifold, DefaultQueryDispatcher, PersistentQueryDispatcher, ShapeCastOptions,
};
use rapier3d::parry::shape::{Ball, Capsule, Voxels};
use rapier3d::prelude::*;

fn floor(nx: i32, nz: i32) -> Voxels {
    let pts: Vec<IVector> = (0..nx).flat_map(|x| (0..nz).map(move |z| IVector::new(x, 0, z))).collect();
    Voxels::new(Vector::splat(1.0), &pts)
}

/// #404: `query::contact` has no voxels arm, so this goes through the
/// contact-manifold path the fix lives in (what rapier's narrow phase calls).
#[test]
fn voxels_manifold_sees_contacts_within_prediction() {
    let vox = floor(8, 8);
    let cap = Capsule::new_y(0.3, 0.2);
    // Capsule bottom at y = 1.55 - 0.5 = 1.05; floor top at y = 1.0 → gap 0.05 < prediction 0.1.
    let pos12 = Pose::translation(4.0, 1.55, 4.0);
    let mut manifolds: Vec<ContactManifold<(), ()>> = Vec::new();
    DefaultQueryDispatcher.contact_manifolds(&pos12, &vox, &cap, 0.1, &mut manifolds, &mut None).unwrap();
    let dist = manifolds
        .iter()
        .flat_map(|m| m.points.iter())
        .map(|p| p.dist)
        .min_by(f32::total_cmp)
        .expect("contact within prediction must be reported");
    assert!((dist - 0.05).abs() < 1.0e-3, "dist {dist}");
}

/// #373: witness1 is in shape1's (the voxels') frame, not the hit voxel's
/// own frame (which would put it at (0, 0.5, 0)). Tolerances are upstream's
/// (`issue_373_voxels_shape_cast_witness.rs`): the GJK witness on a flat face
/// is only good to a few mm.
#[test]
fn voxels_shape_cast_witness1_is_in_shape1_frame() {
    let pts: Vec<IVector> = (0..20).map(|x| IVector::new(x, 0, 0)).collect();
    let vox = Voxels::new(Vector::splat(1.0), &pts);
    let ball = Ball::new(0.5);
    let pos_vox = Pose::IDENTITY;
    let pos_ball = Pose::translation(17.5, 3.5, 0.5);
    let vel_ball = Vector::new(0.0, -1.0, 0.0);
    let opts = ShapeCastOptions::with_max_time_of_impact(10.0);
    let near = |a: Vector, b: Vector| (a - b).length() < 1.0e-2;

    let hit = query::cast_shapes(&pos_vox, Vector::ZERO, &vox, &pos_ball, vel_ball, &ball, opts)
        .unwrap()
        .expect("hit");
    assert!((hit.time_of_impact - 2.0).abs() < 1.0e-4, "toi {}", hit.time_of_impact);
    assert!(near(hit.witness1, Vector::new(17.5, 1.0, 0.5)), "witness1 {:?}", hit.witness1);
    assert!(near(hit.witness2, Vector::new(0.0, -0.5, 0.0)));
    assert!(near(hit.normal1, Vector::Y));
    assert!(near(hit.normal2, -Vector::Y));
    // 0.31's sub-shape ids: the voxel that was hit, and the ball (0).
    assert_eq!(Some(hit.subshape1), vox.linear_index(IVector::new(17, 0, 0)).map(|i| i.flat_id() as u32));
    assert_eq!(hit.subshape2, 0);

    // The swapped shape order must agree with the direct one.
    let swapped = query::cast_shapes(&pos_ball, vel_ball, &ball, &pos_vox, Vector::ZERO, &vox, opts)
        .unwrap()
        .expect("swapped hit");
    assert!((swapped.time_of_impact - hit.time_of_impact).abs() < 1.0e-2);
    assert!(near(swapped.witness1, hit.witness2));
    assert!(near(swapped.witness2, hit.witness1));
    assert!(near(swapped.normal1, hit.normal2));
    assert!(near(swapped.normal2, hit.normal1));
    assert_eq!((swapped.subshape1, swapped.subshape2), (hit.subshape2, hit.subshape1));
}
