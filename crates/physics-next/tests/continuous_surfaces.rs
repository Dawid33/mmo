//! Task 3.3: `QueryPipeline::cast_shape_on_continuous_surfaces` (`// mmo-fork:`
//! in the rapier fork) and the D8 separated-start filters.
//!
//! Terrain is many boxes laid flush: voxels inside a chunk collider, chunk
//! colliders side by side, collapsed full-chunk cuboids next to voxels. A
//! plain `cast_shape` hits each box as a free-standing one, so a seam between
//! two colliders reads as an edge; the continuous-surfaces cast drops normal
//! components that point through a face some collider covers. The five
//! single-shape cases of `crates/game/tests/shape_cast_grazing.rs` live in
//! `upstream_movement_probe.rs`; these are the cross-collider ones.
mod common;

use game::movement::{CHARACTER_SKIN, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS, UNITS_PER_METER};
use physics_next::PhysicsWorld;
use rapier3d::parry::query::{ShapeCastHit, ShapeCastOptions, cast_shapes};
use rapier3d::parry::shape::{Capsule, Voxels};
use rapier3d::prelude::*;

const HALF_HEIGHT: f32 = PLAYER_CAPSULE_HALF_HEIGHT;
const RADIUS: f32 = PLAYER_CAPSULE_RADIUS;
const SKIN: f32 = CHARACTER_SKIN;
const BLOCK: f32 = UNITS_PER_METER;
/// A chunk's edge: two blocks.
const CHUNK: f32 = 2.0 * BLOCK;
/// Capsule centre height resting on a floor whose top is at y = `BLOCK`.
const REST: f32 = BLOCK + HALF_HEIGHT + RADIUS;

fn skin_cast(max_time_of_impact: f32) -> ShapeCastOptions {
    ShapeCastOptions {
        target_distance: SKIN,
        stop_at_penetration: false,
        max_time_of_impact,
        compute_impact_geometry_on_penetration: true,
    }
}

/// One tick of walking with gravity along +x.
fn walking_step() -> Vector {
    Vector::new(3.4536, -0.64, 0.0)
}

/// Fixed bodies at `origins`, one collider each from `collider`, stepped once
/// so the broad phase knows them.
fn world_of(origins: &[Vector], collider: impl Fn(usize) -> Collider) -> PhysicsWorld {
    let mut w = PhysicsWorld::default();
    for (i, origin) in origins.iter().enumerate() {
        let body = w.bodies.insert(RigidBodyBuilder::fixed().translation(*origin));
        w.colliders.insert_with_parent(collider(i), body, &mut w.bodies);
    }
    w.step(&mut PhysicsPipeline::new());
    w
}

/// A chunk's one-block-deep floor of block-scale voxels (2x1x2), top at y = `BLOCK`.
fn chunk_floor() -> Collider {
    let cells: Vec<IVector> = (0..2).flat_map(|x| (0..2).map(move |z| IVector::new(x, 0, z))).collect();
    ColliderBuilder::voxels(Vector::splat(BLOCK), &cells).build()
}

/// The same floor as one cuboid (a collapsed chunk), top at y = `BLOCK`.
fn chunk_cuboid() -> Collider {
    ColliderBuilder::cuboid(0.5 * CHUNK, 0.5 * BLOCK, 0.5 * CHUNK)
        .translation(Vector::new(0.5 * CHUNK, 0.5 * BLOCK, 0.5 * CHUNK))
        .build()
}

fn continuous(w: &PhysicsWorld, pos: Vector, vel: Vector, options: ShapeCastOptions) -> Option<ShapeCastHit> {
    let qp = w.broad_phase.as_query_pipeline(w.narrow_phase.query_dispatcher(), &w.bodies, &w.colliders, QueryFilter::default());
    let capsule = Capsule::new_y(HALF_HEIGHT, RADIUS);
    qp.cast_shape_on_continuous_surfaces(&Pose::from_translation(pos), vel, &capsule, options, SKIN)
        .map(|(_, hit)| hit)
}

fn assert_flat(hit: Option<ShapeCastHit>, context: &str) {
    let hit = hit.unwrap_or_else(|| panic!("{context}: the floor was missed"));
    assert!(hit.time_of_impact < 1.0e-3, "{context}: toi {}", hit.time_of_impact);
    assert_eq!(hit.normal1, Vector::Y, "{context}: a flat floor's normal is straight up: {hit:?}");
}

/// Resting across the x = `CHUNK` seam between two chunk bodies, with the
/// capsule's lowest point on either side of it and on it.
fn assert_seam_is_flat(w: &PhysicsWorld, what: &str) {
    for dx in [-2.0, -0.3, -0.01, 0.0, 0.01, 0.3, 2.0] {
        for gap in [SKIN - 0.001, SKIN + 0.001] {
            let pos = Vector::new(CHUNK + dx, REST + gap, 0.5 * BLOCK);
            let d = walking_step().length();
            let hit = continuous(w, pos, walking_step() / d, skin_cast(d));
            assert_flat(hit, &format!("{what}: dx {dx} gap {gap}"));
        }
    }
}

#[test]
fn seam_between_two_voxels_chunks_is_not_an_edge() {
    let w = world_of(&[Vector::ZERO, Vector::new(CHUNK, 0.0, 0.0)], |_| chunk_floor());
    assert_seam_is_flat(&w, "voxels|voxels");
}

#[test]
fn seam_between_a_cuboid_chunk_and_a_voxels_chunk_is_not_an_edge() {
    let w = world_of(&[Vector::ZERO, Vector::new(CHUNK, 0.0, 0.0)], |i| if i == 0 { chunk_cuboid() } else { chunk_floor() });
    assert_seam_is_flat(&w, "cuboid|voxels");
    let w = world_of(&[Vector::ZERO, Vector::new(CHUNK, 0.0, 0.0)], |i| if i == 0 { chunk_floor() } else { chunk_cuboid() });
    assert_seam_is_flat(&w, "voxels|cuboid");
}

/// The per-collider cast alone (what upstream's `cast_shape` does) sees the
/// seam as an edge: this is what the probe is for.
#[test]
fn a_lone_chunk_reports_its_rim_as_an_edge() {
    let floor = Voxels::new(Vector::splat(BLOCK), &[IVector::new(0, 0, 0), IVector::new(1, 0, 0), IVector::new(0, 0, 1), IVector::new(1, 0, 1)]);
    let capsule = Capsule::new_y(HALF_HEIGHT, RADIUS);
    let pos = Pose::translation(CHUNK + 0.3, REST + SKIN - 0.001, 0.5 * BLOCK);
    let hit = cast_shapes(&Pose::IDENTITY, Vector::ZERO, &floor, &pos, -Vector::Y, &capsule, skin_cast(1.0))
        .unwrap()
        .expect("the rim");
    assert!(hit.normal1.x > 1.0e-3, "past the rim, a lone chunk's contact leans outwards: {hit:?}");
}

/// A real step between two bodies keeps its edge: the probe finds nothing
/// solid across the face the normal leans through.
#[test]
fn a_step_up_to_the_next_chunk_is_a_wall() {
    let w = world_of(&[Vector::ZERO, Vector::new(CHUNK, BLOCK, 0.0)], |_| chunk_floor());
    let pos = Vector::new(CHUNK - RADIUS - SKIN - 1.0, REST + SKIN, 0.5 * BLOCK);
    let hit = continuous(&w, pos, Vector::X, skin_cast(4.0)).expect("the step");
    assert!(hit.normal1.x < -0.9, "the step's face stops the walk: {hit:?}");
    assert!((hit.time_of_impact - 1.0).abs() < 1.0e-2, "toi {}", hit.time_of_impact);
}

/// D8: parry 0.31 reports a start exactly at the target distance as
/// `Converged` at TOI 0 (#106), not `PenetratingOrWithinTargetDist`. The
/// separated-start filters (the voxels cast's, and this query's for a
/// cuboid) key on the TOI: a TOI-0 hit whose corrected normal does not
/// oppose the motion is no hit, so sliding along or lifting off a floor from
/// touching it is free. Rounding decides which side of the skin a start is
/// on, hence the sweep.
#[test]
fn d8_touching_start_sliding_or_lifting_is_no_hit() {
    let away = [Vector::X, Vector::Z, Vector::new(1.0, 0.5, 0.0).normalize(), Vector::Y];
    for (what, w) in [
        ("voxels", world_of(&[Vector::ZERO], |_| chunk_floor())),
        ("cuboid", world_of(&[Vector::ZERO], |_| chunk_cuboid())),
    ] {
        for eps in [-2.0e-6, -1.0e-6, 0.0, 1.0e-6, 2.0e-6] {
            for x in [0.3, 1.7, 5.0, 17.3] {
                let touching = Vector::new(x, REST + SKIN + eps, 0.5 * BLOCK);
                for vel in away {
                    let hit = continuous(&w, touching, vel, skin_cast(4.0));
                    assert!(hit.is_none(), "{what}, eps {eps} x {x} vel {vel:?}: {hit:?}");
                }
                let hit = continuous(&w, touching, Vector::new(1.0, -0.5, 0.0).normalize(), skin_cast(4.0));
                assert_flat(hit, &format!("{what}, eps {eps} x {x}: pressing in from touching"));
            }
        }
    }
}

/// Terraces rising one voxel per chunk along +z (`Chunk::flat_floor(12 + z)`,
/// columns 3–4, rows 7–10), added in `order`, with the broad phase built at
/// `length_unit`: its leaves' fat-AABB margin scales with it, so the two
/// units give two BVH layouts over the same colliders.
fn terrace_world(length_unit: f32, reversed: bool) -> PhysicsWorld {
    let mut chunks: Vec<_> = (3..=4)
        .flat_map(|x| (7..=10).map(move |z| (x, z)))
        .map(|(x, z)| ((x, 0, z), game::Chunk::flat_floor(12 + z as u32)))
        .collect();
    if reversed {
        chunks.reverse();
    }
    let mut w = PhysicsWorld::default();
    w.params.length_unit = length_unit;
    for (coords, chunk) in &chunks {
        common::add_chunk(&mut w, *coords, chunk);
    }
    w.step(&mut PhysicsPipeline::new());
    w
}

/// A walking body resting on one terrace while its capsule already touches
/// the next one's riser edge touches both at TOI 0. Which of the tied hits the
/// cast returns decides the controller's move (the floor first strips the
/// gravity part of the step; the riser first projects it onto the edge and
/// loses ~0.6 units of the tick's walk), so it must not depend on the
/// broad phase's layout: blocking hits win, then the least-opposing one
/// (here the floor), then the lowest collider index. Before the tie pass the
/// first touching leaf in traversal order won — `length_unit` (D6) flipped
/// it on `physics_goldens::terrace_uphill_1_voxel`, at this exact pose.
#[test]
fn a_touching_tie_resolves_the_same_in_every_bvh_layout() {
    let pos = Vector::new(128.0, 34.616791, 284.503448);
    let desired = Vector::new(0.0, -1.35, 3.4536);
    let capsule = Capsule::new_y(HALF_HEIGHT, RADIUS);
    for lu in [1.0, UNITS_PER_METER] {
        for reversed in [false, true] {
            let w = terrace_world(lu, reversed);
            let qp = w.broad_phase.as_query_pipeline(w.narrow_phase.query_dispatcher(), &w.bodies, &w.colliders, QueryFilter::default());
            let (_, hit) = qp
                .cast_shape_on_continuous_surfaces(&Pose::from_translation(pos), desired.normalize(), &capsule, skin_cast(desired.length()), SKIN)
                .unwrap_or_else(|| panic!("length unit {lu}, reversed {reversed}: resting on the floor is a hit"));
            assert_eq!(hit.time_of_impact, 0.0, "length unit {lu}, reversed {reversed}: {hit:?}");
            assert_eq!(hit.normal1, Vector::Y, "length unit {lu}, reversed {reversed}: the floor, not the riser edge: {hit:?}");
        }
    }
}
