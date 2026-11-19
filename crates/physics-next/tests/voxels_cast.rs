//! Fork policy of the voxels cast (`// mmo-fork:` in the parry fork's
//! `shape_cast_voxels_shape.rs`) beyond the cases ported from the game in
//! `upstream_movement_probe.rs`.
use game::movement::{CHARACTER_SKIN, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS, UNITS_PER_METER};
use rapier3d::parry::query::{ShapeCastOptions, cast_shapes};
use rapier3d::parry::shape::{Capsule, Voxels};
use rapier3d::prelude::*;

const BLOCK: f32 = UNITS_PER_METER;

/// Gate: mmo-fork hunk parry shape_cast_voxels_shape.rs:92 axis normal made exact; observed one-tick stall on flat ground at x≈170 (b6abc1e9)
/// A face contact reports the face's normal EXACTLY. The character
/// controller splits its motion with `normal × up`, which is exactly zero for
/// a normal of (0, 0.99999994, 0): the whole slide then counts as a
/// "vertical tangent" whose rounding-noise y reads as slipping on a
/// non-slip slope, and the tick's motion is dropped (a one-tick stall on flat
/// ground, seen at x ≈ 170 in `length_unit_does_not_change_character_movement`).
#[test]
fn voxels_cast_floor_normal_is_exactly_up() {
    let cells: Vec<IVector> = (0..4).flat_map(|x| (0..4).map(move |z| IVector::new(x, 0, z))).collect();
    let floor = Voxels::new(Vector::splat(BLOCK), &cells);
    let capsule = Capsule::new_y(PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS);
    let options = ShapeCastOptions {
        target_distance: CHARACTER_SKIN,
        stop_at_penetration: false,
        max_time_of_impact: 3.5,
        compute_impact_geometry_on_penetration: true,
    };
    let rest = BLOCK + PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS;
    let dir = Vector::new(3.4536, -1.8, 0.0).normalize();
    let mut checked = 0;
    for i in 0..60 {
        for gap in [CHARACTER_SKIN - 0.001, CHARACTER_SKIN + 1.0e-4, CHARACTER_SKIN + 0.001] {
            let x = 12.0 + i as f32 * 0.6171;
            let pos = Pose::translation(x, rest + gap, 2.0 * BLOCK + 0.0037 * i as f32);
            let hit = cast_shapes(&Pose::IDENTITY, Vector::ZERO, &floor, &pos, dir, &capsule, options)
                .unwrap()
                .unwrap_or_else(|| panic!("x {x} gap {gap}: floor missed"));
            assert_eq!(hit.normal1, Vector::Y, "x {x} gap {gap}: {hit:?}");
            checked += 1;
        }
    }
    assert_eq!(checked, 180);

    // The recorded stall: the walker's start on that tick, in the frame of
    // its chunk body (a 2x2-block floor at x = 160).
    let chunk: Vec<IVector> = (0..2).flat_map(|x| (0..2).map(move |z| IVector::new(x, 0, z))).collect();
    let chunk = Voxels::new(Vector::splat(BLOCK), &chunk);
    let pos = Pose::translation(170.31873 - 160.0, 30.617233, 7.999738);
    let motion = Vector::new(3.4536, -1.8000001, 0.0);
    let options = ShapeCastOptions { max_time_of_impact: motion.length(), ..options };
    let hit = cast_shapes(&Pose::IDENTITY, Vector::ZERO, &chunk, &pos, motion.normalize(), &capsule, options)
        .unwrap()
        .expect("floor");
    assert_eq!(hit.normal1, Vector::Y, "{hit:?}");
}

