//! Fork policy of the voxels cast (`// mmo-fork:` in parry-next's
//! `shape_cast_voxels_shape.rs`) beyond the cases ported from the game in
//! `upstream_movement_probe.rs`.
use game::movement::{CHARACTER_SKIN, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS, UNITS_PER_METER};
use rapier3d::parry::query::{ShapeCastOptions, cast_shapes};
use rapier3d::parry::shape::{Capsule, Cuboid, Voxels};
use rapier3d::prelude::*;

const BLOCK: f32 = UNITS_PER_METER;

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

fn options4() -> ShapeCastOptions {
    ShapeCastOptions {
        target_distance: CHARACTER_SKIN,
        stop_at_penetration: false,
        max_time_of_impact: 4.0,
        compute_impact_geometry_on_penetration: true,
    }
}

/// D8: parry 0.31 reports a start exactly at the target distance as
/// `Converged` at TOI 0 (#106), not `PenetratingOrWithinTargetDist`. Sliding
/// along a floor from there, the per-voxel cast returns such a hit (its
/// normal a few 1e-6 off +Y, so upstream counts it as closing in); once the
/// edge correction makes the normal exactly +Y, only a filter keyed on the
/// TOI rather than the status drops it: a TOI-0 hit whose normal does not
/// oppose the motion is no hit. The lone cuboid is the support-map cast's
/// own grazing policy (`NORMAL_NOISE`).
#[test]
fn d8_touching_start_sliding_or_lifting_is_no_hit() {
    let rest = BLOCK + PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS + CHARACTER_SKIN;
    let floor = Voxels::new(Vector::splat(BLOCK), &[IVector::new(0, 0, 0), IVector::new(1, 0, 0)]);
    let cuboid = Cuboid::new(Vector::new(BLOCK, 0.5 * BLOCK, 0.5 * BLOCK));
    let capsule = Capsule::new_y(PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS);
    let away = [Vector::X, Vector::Z, Vector::new(1.0, 0.5, 0.0).normalize(), Vector::Y];
    // Around the exact skin, where rounding decides which side a start is on.
    let starts = || [-2.0e-6, -1.0e-6, 0.0, 1.0e-6, 2.0e-6].into_iter().flat_map(|eps| [0.3, 1.7, 5.0].map(|x| (eps, x)));
    for (eps, x) in starts() {
        for vel in away {
            let pos = Pose::translation(BLOCK + x, rest + eps, 0.5 * BLOCK);
            let hit = cast_shapes(&Pose::IDENTITY, Vector::ZERO, &floor, &pos, vel, &capsule, options4()).unwrap();
            assert!(hit.is_none(), "voxels, eps {eps} x {x} vel {vel:?}: {hit:?}");
        }
    }
    for (eps, x) in starts() {
        for vel in away {
            let y = 0.5 * BLOCK + PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS + CHARACTER_SKIN + eps;
            let hit = cast_shapes(&Pose::IDENTITY, Vector::ZERO, &cuboid, &Pose::translation(x, y, 0.0), vel, &capsule, options4())
                .unwrap();
            assert!(hit.is_none(), "cuboid, eps {eps} x {x} vel {vel:?}: {hit:?}");
        }
    }
    // Pressing in from touching is a hit at TOI 0 with the face normal.
    let pos = Pose::translation(BLOCK + 0.3, rest, 0.5 * BLOCK);
    let hit = cast_shapes(&Pose::IDENTITY, Vector::ZERO, &floor, &pos, Vector::new(1.0, -0.5, 0.0).normalize(), &capsule, options4())
        .unwrap()
        .expect("pressing in");
    assert_eq!((hit.time_of_impact, hit.normal1), (0.0, Vector::Y), "{hit:?}");
}
