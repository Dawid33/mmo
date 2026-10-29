//! Port of `crates/game/tests/sunk_body.rs` to the fork (Task 3.4, D7).
//!
//! A body that starts a tick sunk into terrain is put back on its surface.
//! The character controller cannot slide or step out of an overlap by
//! itself; it happens at region seams, where the region a body leaves has
//! no floor past its edge and the body's rounded bottom dips over that edge
//! on the crossing tick (about 3 units over a corner). The putting-back is
//! the GAME's `movement::depenetration_lift` (called directly here),
//! not the fork: the rig's controller runs with `depenetrate: false` (D7),
//! and `depenetration_lift_is_what_puts_it_back` shows the fork alone
//! leaves the body sunk.
mod common;

use common::{CHUNK, Walker};
use game::movement::{character_controller, depenetration_lift};
use game::movement::{
    CHARACTER_SKIN, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS, STEP_HEIGHT, TICK_DT, UNITS_PER_METER,
};
use game::{Block, BlockId, Chunk};
use rapier3d::control::KinematicCharacterController;
use rapier3d::prelude::*;

/// Floor top at y = 8 over x/z units 96..192: half-block `Voxels`.
fn floor() -> Walker {
    Walker::on(96.0..192.0, 96.0..192.0, |_, _| Chunk::flat_floor(8))
}

/// Full chunks, top at y = `CHUNK`: each collapses to one cuboid collider —
/// the only terrain upstream's #485 depenetration acts on.
fn cuboid_floor() -> Walker {
    Walker::on(96.0..192.0, 96.0..192.0, |_, _| Chunk::uniform(Block::new(BlockId(1))))
}

/// Resting height of the body's centre on a floor whose top is `top`.
fn rest_on(top: f32) -> f32 {
    top + PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS + CHARACTER_SKIN
}

fn rest() -> f32 {
    rest_on(8.0)
}

/// The player placed at (x, y, z) walking W+A (towards -x, -z) for four ticks.
fn walk_from(x: f32, y: f32, z: f32) -> Vec<(f32, f32, f32)> {
    walk_on(floor(), x, y, z)
}

fn walk_on(mut w: Walker, x: f32, y: f32, z: f32) -> Vec<(f32, f32, f32)> {
    w.teleport(Vector::new(x, y, z));
    w.keys.forward = true;
    w.keys.left = true;
    (0..4)
        .map(|_| {
            w.tick();
            let b = w.body();
            (b.x, b.y, b.z)
        })
        .collect()
}

#[test]
fn a_body_sunk_into_the_floor_is_put_back_on_it_and_walks_on() {
    let rest = rest();
    // Mid-floor and 4 units inside a corner; resting, over an edge, over a corner.
    for (x, z) in [(150.0, 150.0), (188.0, 188.0)] {
        for sink in [0.0, 0.86, 3.03] {
            let trail = walk_from(x, rest - sink, z);
            let at = format!("start ({x}, {}, {z}): {trail:?}", rest - sink);
            let (lx, ly, lz) = *trail.last().unwrap();
            assert!((ly - rest).abs() < 0.01, "must stand on the floor: {at}");
            assert!(lx < x - 5.0 && lz < z - 5.0, "must walk on: {at}");
        }
    }
    // The same on collapsed full chunks (cuboid colliders).
    let rest = rest_on(CHUNK);
    for sink in [0.0, 0.86, 3.03] {
        let trail = walk_on(cuboid_floor(), 150.0, rest - sink, 150.0);
        let (lx, ly, lz) = *trail.last().unwrap();
        assert!((ly - rest).abs() < 0.01 && lx < 145.0 && lz < 145.0, "cuboid, sink {sink}: {trail:?}");
    }
}

/// Without the game's lift, the fork's controller does not put a body sunk
/// into voxel terrain back: the lift is the game's. (On `Voxels` #485 is
/// inert whatever `depenetrate` says — capsule-`Voxels` `contact` is
/// unsupported — so the flag itself is pinned on cuboids, below.)
#[test]
fn depenetration_lift_is_what_puts_it_back() {
    let w = floor();
    let sunk = Pose::translation(150.0, rest() - 3.03, 150.0);
    let queries = w.w.broad_phase.as_query_pipeline(
        w.w.narrow_phase.query_dispatcher(),
        &w.w.bodies,
        &w.w.colliders,
        QueryFilter::default().exclude_rigid_body(w.body),
    );
    let shape = w.w.colliders[w.w.bodies[w.body].colliders()[0]].shape();
    let lift = depenetration_lift(&queries, shape, &sunk);
    assert!((lift - 3.03).abs() < 0.01, "the game's lift clears the sink: {lift}");
    let controller = character_controller(&w.motion);
    let still = controller.move_shape(TICK_DT, &queries, shape, &sunk, Vector::ZERO, |_| {});
    assert_eq!(still.translation, Vector::ZERO, "the fork leaves a sunk body where it is");
}

/// D7 through the rig: a body buried deeper than `STEP_HEIGHT` gets no lift
/// ("buried stays put"). #485 only runs on an exactly-zero desired move — a
/// hovering flyer with no keys — and on cuboid terrain it would push the body
/// out; the game's controller (`depenetrate: false`) leaves it.
#[test]
fn a_hovering_body_buried_in_a_collapsed_chunk_stays_put() {
    let mut w = cuboid_floor();
    let buried = Vector::new(150.0, rest_on(CHUNK) - (STEP_HEIGHT * UNITS_PER_METER + 1.0), 150.0);
    w.motion.flying = true;
    w.teleport(buried);
    w.tick();
    let after = w.body();
    assert_eq!(Vector::new(after.x, after.y, after.z), buried, "the rig moved a buried body");

    // It is the flag that holds it: upstream's default pushes it out.
    let queries = w.w.broad_phase.as_query_pipeline(
        w.w.narrow_phase.query_dispatcher(),
        &w.w.bodies,
        &w.w.colliders,
        QueryFilter::default().exclude_rigid_body(w.body),
    );
    let shape = w.w.colliders[w.w.bodies[w.body].colliders()[0]].shape();
    let upstream = KinematicCharacterController { depenetrate: true, ..character_controller(&w.motion) };
    let pushed = upstream.move_shape(TICK_DT, &queries, shape, &Pose::from_translation(buried), Vector::ZERO, |_| {});
    assert!(pushed.translation.y > 1.0, "#485 would push it out: {:?}", pushed.translation);
}
