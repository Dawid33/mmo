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
/// A fixture chunk's edge: two blocks. Hand-built colliders here, so it is
/// independent of the game's `CHUNK_BLOCKS`.
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

/// Terraces rising one voxel per two blocks along +z (block row `bz` is
/// `Chunk::flat_floor(12 + bz / 2)`'s row — one 2-block chunk per terrace at
/// the original chunk size), over block columns 6–9 and rows 14–21 (units
/// x 96..160, z 224..352), added in `order`, with the broad phase built at
/// `length_unit`: its leaves' fat-AABB margin scales with it, so the two
/// units give two BVH layouts over the same colliders.
fn terrace_world(length_unit: f32, reversed: bool) -> PhysicsWorld {
    use game::{BlockIndex, CHUNK_BLOCKS};
    let (xs, zs) = (6..10usize, 14..22usize); // world block ranges
    let span = |r: &std::ops::Range<usize>| r.start / CHUNK_BLOCKS..=(r.end - 1) / CHUNK_BLOCKS;
    let mut chunks = Vec::new();
    for cx in span(&xs) {
        for cz in span(&zs) {
            let mut chunk = game::Chunk::default();
            for bz in 0..CHUNK_BLOCKS {
                let wz = cz * CHUNK_BLOCKS + bz;
                if !zs.contains(&wz) {
                    continue;
                }
                let row = game::Chunk::flat_floor(12 + (wz / 2) as u32);
                for bx in (0..CHUNK_BLOCKS).filter(|bx| xs.contains(&(cx * CHUNK_BLOCKS + bx))) {
                    for by in 0..CHUNK_BLOCKS {
                        chunk.set_block(bx, by, bz, row.block_at(bx, by, bz));
                        let i = BlockIndex::from_xyz(bx, by, bz);
                        if let Some(c) = row.chisel.get(&i) {
                            chunk.chisel.insert(i, c.clone());
                        }
                    }
                }
            }
            chunks.push(((cx as i32, 0, cz as i32), chunk));
        }
    }
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

/// Wraps the default dispatcher and records which shape each `cast_shapes`
/// call was made against (a cuboid collider costs one call).
#[derive(Default)]
struct CountingDispatcher {
    cast_against: std::sync::Mutex<Vec<usize>>,
}

impl CountingDispatcher {
    fn shapes_cast_twice(&self) -> usize {
        let mut seen = self.cast_against.lock().unwrap().clone();
        let all = seen.len();
        seen.sort_unstable();
        seen.dedup();
        all - seen.len()
    }
}

impl rapier3d::parry::query::QueryDispatcher for CountingDispatcher {
    fn intersection_test(&self, pos12: &Pose, g1: &dyn rapier3d::parry::shape::Shape, g2: &dyn rapier3d::parry::shape::Shape) -> Result<rapier3d::parry::query::ShapeIntersection, rapier3d::parry::query::Unsupported> {
        rapier3d::parry::query::DefaultQueryDispatcher.intersection_test(pos12, g1, g2)
    }
    fn distance(&self, pos12: &Pose, g1: &dyn rapier3d::parry::shape::Shape, g2: &dyn rapier3d::parry::shape::Shape) -> Result<rapier3d::parry::query::ShapeDistance, rapier3d::parry::query::Unsupported> {
        rapier3d::parry::query::DefaultQueryDispatcher.distance(pos12, g1, g2)
    }
    fn contact(&self, pos12: &Pose, g1: &dyn rapier3d::parry::shape::Shape, g2: &dyn rapier3d::parry::shape::Shape, prediction: f32) -> Result<Option<rapier3d::parry::query::Contact>, rapier3d::parry::query::Unsupported> {
        rapier3d::parry::query::DefaultQueryDispatcher.contact(pos12, g1, g2, prediction)
    }
    fn closest_points(&self, pos12: &Pose, g1: &dyn rapier3d::parry::shape::Shape, g2: &dyn rapier3d::parry::shape::Shape, max_dist: f32) -> Result<rapier3d::parry::query::ClosestPoints, rapier3d::parry::query::Unsupported> {
        rapier3d::parry::query::DefaultQueryDispatcher.closest_points(pos12, g1, g2, max_dist)
    }
    fn cast_shapes(&self, pos12: &Pose, local_vel12: Vector, g1: &dyn rapier3d::parry::shape::Shape, g2: &dyn rapier3d::parry::shape::Shape, options: ShapeCastOptions) -> Result<Option<ShapeCastHit>, rapier3d::parry::query::Unsupported> {
        self.cast_against.lock().unwrap().push(g1 as *const dyn rapier3d::parry::shape::Shape as *const () as usize);
        rapier3d::parry::query::DefaultQueryDispatcher.cast_shapes(pos12, local_vel12, g1, g2, options)
    }
    fn cast_shapes_nonlinear(&self, motion1: &rapier3d::parry::query::NonlinearRigidMotion, g1: &dyn rapier3d::parry::shape::Shape, motion2: &rapier3d::parry::query::NonlinearRigidMotion, g2: &dyn rapier3d::parry::shape::Shape, start_time: f32, end_time: f32, stop_at_penetration: bool) -> Result<Option<ShapeCastHit>, rapier3d::parry::query::Unsupported> {
        rapier3d::parry::query::DefaultQueryDispatcher.cast_shapes_nonlinear(motion1, g1, motion2, g2, start_time, end_time, stop_at_penetration)
    }
}

/// The tie pass must not cost a second cast per collider: `find_best`'s
/// results are reused, and only leaves it pruned are cast again. A walk
/// into a two-cuboid wall over a cuboid floor (a hit at TOI > 0, and one
/// resting on the floor at TOI 0) casts each collider at most once.
#[test]
fn the_tie_pass_casts_no_collider_twice() {
    let w = world_of(
        &[Vector::ZERO, Vector::new(CHUNK, BLOCK, 0.0), Vector::new(CHUNK, BLOCK, CHUNK)],
        |_| chunk_cuboid(),
    );
    let capsule = Capsule::new_y(HALF_HEIGHT, RADIUS);
    for (pos, vel, what) in [
        (Vector::new(CHUNK - RADIUS - SKIN - 1.0, REST + SKIN + 0.5, CHUNK - 0.5), Vector::X, "walking into the wall"),
        (Vector::new(0.5 * CHUNK, REST + SKIN, 0.5 * CHUNK), walking_step().normalize(), "resting on the floor"),
    ] {
        let counting = CountingDispatcher::default();
        let qp = w.broad_phase.as_query_pipeline(&counting, &w.bodies, &w.colliders, QueryFilter::default());
        let hit = qp.cast_shape_on_continuous_surfaces(&Pose::from_translation(pos), vel, &capsule, skin_cast(4.0), SKIN);
        assert!(hit.is_some(), "{what}: a hit");
        assert_eq!(counting.shapes_cast_twice(), 0, "{what}: {:?}", counting.cast_against.lock().unwrap());
    }
}
