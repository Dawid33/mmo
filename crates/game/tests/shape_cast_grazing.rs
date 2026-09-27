//! Shape casts that start a skin's width from a surface: where a character
//! controller leaves its shape every step, and so where every one of its
//! casts begins. Three defects of the vendored parry used to meet here; each
//! made a body on flat voxel ground sink, fall through its skin, or trip.
//! See `walking_smoothness.rs` for what they did to a walking player.
use game::parry::math::{IVector, Pose, Vector};
use game::parry::query::{cast_shapes, ShapeCastOptions, ShapeCastStatus};
use game::parry::shape::{Capsule, Cuboid, Voxels};

/// The player capsule and the character controller's skin for it.
const HALF_HEIGHT: f32 = game::movement::PLAYER_CAPSULE_HALF_HEIGHT;
const RADIUS: f32 = game::movement::PLAYER_CAPSULE_RADIUS;
const SKIN: f32 = game::movement::CHARACTER_SKIN;

fn r(x: f32) -> f32 {
    x
}

fn v(x: f32, y: f32, z: f32) -> Vector {
    Vector::new(r(x), r(y), r(z))
}

/// The character controller's cast options.
fn skin_cast(max_time_of_impact: f32) -> ShapeCastOptions {
    ShapeCastOptions {
        target_distance: r(SKIN),
        stop_at_penetration: false,
        max_time_of_impact: r(max_time_of_impact),
        compute_impact_geometry_on_penetration: true,
    }
}

/// One tick of walking with gravity: mostly along +z, slightly down.
fn walking_step() -> Vector {
    v(0.0, -0.64, 3.4536)
}

fn assert_floor_hit(hit: Option<game::parry::query::ShapeCastHit>, context: &str) {
    let hit = hit.unwrap_or_else(|| panic!("{context}: the floor was missed"));
    assert!(hit.time_of_impact < 1.0e-3, "{context}: toi {}", hit.time_of_impact);
    let n = hit.normal1;
    assert!(
        n.x.abs() < 1.0e-4 && n.z.abs() < 1.0e-4 && n.y > 0.9999,
        "{context}: a flat floor's normal is straight up, got ({}, {}, {})",
        n.x,
        n.y,
        n.z
    );
}

/// The cast is a ray cast against the shape rounded by the target distance;
/// starting on that rounded surface it missed, or returned its last search
/// direction — degrees off a flat face — as the normal.
#[test]
fn cast_starting_at_the_target_distance_hits_with_the_true_normal() {
    let cuboid = Cuboid::new(v(8.0, 8.0, 8.0));
    let capsule = Capsule::new_y(r(HALF_HEIGHT), r(RADIUS));
    // Around the skin: where time-of-impact error leaves a parked shape.
    for gap in [SKIN + 0.0015, SKIN + 0.0005, SKIN, SKIN - 0.0005, SKIN - 0.006] {
        for z in [-6.0, -2.5, 0.0, 2.01, 5.0] {
            let pos2 = Pose::translation(r(0.3), r(8.0 + HALF_HEIGHT + RADIUS + gap), r(z));
            let hit = cast_shapes(
                &Pose::IDENTITY,
                Vector::ZERO,
                &cuboid,
                &pos2,
                walking_step(),
                &capsule,
                skin_cast(1.0),
            )
            .unwrap();
            assert_floor_hit(hit, &format!("gap {gap} z {z}"));
        }
    }
}

#[test]
fn cast_moving_away_from_within_the_target_distance_is_free() {
    let cuboid = Cuboid::new(v(8.0, 8.0, 8.0));
    let capsule = Capsule::new_y(r(HALF_HEIGHT), r(RADIUS));
    let pos2 = Pose::translation(r(0.0), r(8.0 + HALF_HEIGHT + RADIUS + SKIN), r(0.0));
    for vel in [v(0.0, 0.0, 3.0), v(0.0, 1.0, 3.0)] {
        let hit =
            cast_shapes(&Pose::IDENTITY, Vector::ZERO, &cuboid, &pos2, vel, &capsule, skin_cast(1.0))
                .unwrap();
        assert!(hit.is_none(), "sliding along or lifting off the floor is no hit: {hit:?}");
    }
}

/// A 3x1x3 floor of `size`-unit voxels, top face at y = `size`.
fn voxel_floor(size: f32) -> Voxels {
    let mut cells = Vec::new();
    for x in 0..3 {
        for z in 0..3 {
            cells.push(IVector::new(x, 0, z));
        }
    }
    Voxels::new(Vector::splat(r(size)), &cells)
}

fn cast_on_floor(floor: &Voxels, size: f32, x: f32, z: f32, vel: Vector) -> Option<game::parry::query::ShapeCastHit> {
    let capsule = Capsule::new_y(r(HALF_HEIGHT), r(RADIUS));
    let pos2 = Pose::translation(r(x), r(size + HALF_HEIGHT + RADIUS + SKIN + 0.001), r(z));
    let distance = vel.length();
    cast_shapes(
        &Pose::IDENTITY,
        Vector::ZERO,
        floor,
        &pos2,
        vel / distance,
        &capsule,
        skin_cast(distance),
    )
    .unwrap()
}

/// The voxels cast looked for voxels under the shape's bare AABB — which a
/// shape hovering a skin above the floor does not reach — and then moved that
/// search box with the motion instead of growing it: when the box crossed a
/// voxel border before it came down to the floor, the voxel under the shape
/// was never looked at and the shape fell through its skin for a tick.
#[test]
fn voxels_cast_sees_the_voxel_under_a_hovering_shape() {
    let floor = voxel_floor(16.0);
    // The leading face of the capsule's AABB is 0.16 short of the z = 32
    // voxel border: the search crosses it long before it descends.
    for z in [16.0 + 9.443, 32.0 - RADIUS - 0.16, 32.0 - RADIUS - 0.01] {
        assert_floor_hit(cast_on_floor(&floor, 16.0, 24.0, z, walking_step()), &format!("z {z}"));
    }
}

/// Every voxel is cast against as a free-standing box, so the seams of a flat
/// floor reported their edges: a normal leaning along the floor whenever the
/// shape's lowest point was nearer a neighbouring voxel's rim than its face.
#[test]
fn voxels_cast_reports_no_edges_inside_a_flat_floor() {
    let floor = voxel_floor(1.0);
    let capsule_floor = |x: f32, z: f32| cast_on_floor(&floor, 1.0, x, z, walking_step());
    for x in [1.5, 1.26, 1.01] {
        for z in [1.5, 1.24, 1.76, 1.99] {
            assert_floor_hit(capsule_floor(x, z), &format!("x {x} z {z}"));
        }
    }
    // A slide along the floor, a hair inside the skin, is not stopped by the
    // rim of the next voxel either.
    let slide = cast_on_floor(&floor, 1.0, 1.5, 1.24, v(0.0, 0.0, 0.7));
    assert!(
        slide.is_none_or(|hit| hit.status == ShapeCastStatus::Converged && hit.normal1.y > 0.9999),
        "a flat floor must not stop a slide along it: {slide:?}"
    );
}

/// The floor's real rim keeps its edge: nothing covers the faces around it.
#[test]
fn voxels_cast_keeps_real_edges() {
    let floor = voxel_floor(16.0);
    let capsule = Capsule::new_y(r(HALF_HEIGHT), r(RADIUS));
    // Beyond the z = 48 rim and below the top, moving back in: the capsule's
    // rounded foot meets the rim's edge.
    let pos2 = Pose::translation(r(24.0), r(16.0 + HALF_HEIGHT + RADIUS - 2.0), r(48.0 + 8.0));
    let hit = cast_shapes(
        &Pose::IDENTITY,
        Vector::ZERO,
        &floor,
        &pos2,
        v(0.0, 0.0, -1.0),
        &capsule,
        skin_cast(8.0),
    )
    .unwrap()
    .expect("the rim must stop the capsule");
    let n = hit.normal1;
    assert!(n.z > 0.3 && n.y > 0.3, "an edge contact leans: ({}, {}, {})", n.x, n.y, n.z);
}
