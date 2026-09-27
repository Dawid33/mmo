//! Port of `crates/game/tests/sunk_body.rs` to the fork (Task 3.4, D7).
//!
//! A body that starts a tick sunk into terrain is put back on its surface.
//! The character controller cannot slide or step out of an overlap by
//! itself; it happens at region seams, where the region a body leaves has
//! no floor past its edge and the body's rounded bottom dips over that edge
//! on the crossing tick (about 3 units over a corner). The putting-back is
//! the GAME's `movement::depenetration_lift` (here `common::depenetration_lift`),
//! not the fork: the rig's controller runs with `depenetrate: false` (D7),
//! and `depenetration_lift_is_what_puts_it_back` shows the fork alone
//! leaves the body sunk.
mod common;

use common::{Walker, character_controller};
use game::Chunk;
use game::movement::{CHARACTER_SKIN, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS, TICK_DT};
use rapier3d::prelude::*;

/// Floor top at y = 8 over chunks 3..=5 x 3..=5.
fn floor() -> Walker {
    Walker::on(3..=5, 3..=5, |_, _| Chunk::flat_floor(8))
}

fn rest() -> f32 {
    8.0 + PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS + CHARACTER_SKIN
}

/// The player placed at (x, y, z) walking W+A (towards -x, -z) for four ticks.
fn walk_from(x: f32, y: f32, z: f32) -> Vec<(f32, f32, f32)> {
    let mut w = floor();
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
}

/// D7: without the game's lift, the fork's controller (as the game
/// configures it, `depenetrate: false`) does not put a sunk body back, with
/// or without desired motion — sinking is the game's call.
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
    let lift = common::depenetration_lift(&queries, shape, &sunk);
    assert!((lift - 3.03).abs() < 0.01, "the game's lift clears the sink: {lift}");
    let controller = character_controller(&w.motion);
    let still = controller.move_shape(TICK_DT, &queries, shape, &sunk, Vector::ZERO, |_| {});
    assert_eq!(still.translation, Vector::ZERO, "the fork leaves a sunk body where it is");
}
