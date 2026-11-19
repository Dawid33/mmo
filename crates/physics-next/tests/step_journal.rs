//! Upgrade plan Task 4.2: `crates/game/tests/step_journal.rs` against the
//! hybrid `StepJournal` of the vendored rapier 0.36 (D1): step-then-revert
//! must restore every structure byte for byte (bincode of all seven parts plus
//! the empty soft-body set, `PhysicsWorld::state_bytes`), not by std `Hash`.
//!
//! Differences from the old fork's file:
//! - The hybrid journal clones the broad phase on EVERY step (its serialized
//!   `frame_index` changes each step, so there is no clean tick; D1's known
//!   cost). `idle_tick_journal_is_empty_and_broad_untouched` becomes
//!   `idle_tick_touches_no_body_or_collider`: no first-touch capture on an idle
//!   tick, and the revert is exact; the "broad untouched" half is gone.
//! - `journal_digest_matches_after_exact_revert` and
//!   `journal_digest_matches_on_collider_removal_tick` landed with `StateHash`
//!   (D2, Task 4.4 = handoff H8), plus
//!   `journal_digest_sees_an_incomplete_revert`: a pre-step clone stands in for
//!   an exact revert; stepped bodies, or joint sets changed although they were
//!   not captured, for an incomplete one (`live_digest` reads the footprint
//!   without reverting).
//! - The perf-evidence tally prints the hybrid's numbers (the broad-phase
//!   clone is taken every tick; bodies/colliders scale with activity).
//!
//! The seeded revert fuzzer is `journal_fuzz.rs`.
use physics_next::{PhysicsWorld, differing_parts};
use rapier3d::prelude::*;

fn world() -> (PhysicsWorld, PhysicsPipeline) {
    let w = PhysicsWorld { gravity: Vector::new(0.0, -9.81, 0.0), ..Default::default() };
    (w, PhysicsPipeline::new())
}

/// One fixed cuboid per "chunk": stands in for the chunk colliders.
fn add_floor(w: &mut PhysicsWorld, x: i32, z: i32) {
    let b = w.bodies.insert(RigidBodyBuilder::fixed().translation(Vector::new((x * 32) as f32, -1.0, (z * 32) as f32)));
    w.colliders.insert_with_parent(ColliderBuilder::cuboid(16.0, 1.0, 16.0), b, &mut w.bodies);
}

fn add_ball(w: &mut PhysicsWorld, at: Vector, radius: f32) -> RigidBodyHandle {
    let b = w.bodies.insert(RigidBodyBuilder::dynamic().translation(at));
    w.colliders.insert_with_parent(ColliderBuilder::ball(radius), b, &mut w.bodies);
    b
}

#[track_caller]
fn assert_same(expect: &[Vec<u8>; 8], w: &PhysicsWorld, what: &str) {
    let got = w.state_bytes();
    assert!(got == *expect, "{what}: differs in {:?}", differing_parts(expect, &got));
}

/// Gate: mmo-fork hunks rapier rigid_body_set.rs first-touch capture buffer / journal_capture in &mut accessors (83c072f2)
#[test]
fn moving_body_dirty_ticks_lifo_revert_is_exact() {
    let (mut w, mut p) = world();
    for x in 0..8 {
        for z in 0..8 {
            add_floor(&mut w, x, z);
        }
    }
    // Falls fast enough to move its leaf past the change-detection skin.
    w.bodies.insert(RigidBodyBuilder::dynamic().translation(Vector::new(2.0, 8.0, 2.0)));
    let before = w.state_bytes();
    let journals: Vec<StepJournal> = (0..20).map(|_| w.step_journaled(&mut p)).collect();
    for j in journals.into_iter().rev() {
        w.revert(j);
    }
    assert_same(&before, &w, "20-step LIFO revert");
}

/// Gate: pins the `mmo-fork` hunk: D1 old regression named in test: broad-phase removals applied before the leaf pre-scan (collider removed+replaced in same slot stopped colliding)
/// Old regression (broad-phase removals applied before the leaf pre-scan): a
/// collider removed and replaced in the same slot in one tick must still
/// collide. Kept to show journaled steps simulate like plain ones.
#[test]
fn same_tick_remove_and_slot_reuse_keeps_replacement_collidable() {
    let (mut w, mut p) = world();
    let floor_body = w.bodies.insert(RigidBodyBuilder::fixed().translation(Vector::new(0.0, -1.0, 0.0)));
    let old_floor = w.colliders.insert_with_parent(ColliderBuilder::cuboid(16.0, 1.0, 16.0), floor_body, &mut w.bodies);
    let ball = add_ball(&mut w, Vector::new(0.0, 2.0, 0.0), 0.5);
    for _ in 0..3 {
        let _ = w.step_journaled(&mut p);
    }
    let old_idx = old_floor.into_raw_parts().0;
    w.colliders.remove(old_floor, &mut w.islands, &mut w.bodies, &mut w.soft_bodies, true);
    let new_floor = w.colliders.insert_with_parent(ColliderBuilder::cuboid(16.0, 1.0, 16.0), floor_body, &mut w.bodies);
    assert_eq!(old_idx, new_floor.into_raw_parts().0, "arena must reuse the slot for this regression to be exercised");
    for _ in 0..100 {
        let _ = w.step_journaled(&mut p);
    }
    let y = w.bodies[ball].translation().y;
    assert!(y > 0.0, "ball fell through the replaced floor: y = {y}");
}

/// Gate: mmo-fork hunk 'the journal restores values, not slots' (rigid_body_set.rs:247/311, collider_set.rs:267/307/541) on a removal tick (83c072f2)
#[test]
fn collider_removal_tick_reverts_exactly() {
    let (mut w, mut p) = world();
    add_floor(&mut w, 0, 0);
    add_floor(&mut w, 1, 0);
    let tile_body = w.bodies.insert(RigidBodyBuilder::fixed().translation(Vector::new(40.0, -1.0, 2.0)));
    let tile = w.colliders.insert_with_parent(ColliderBuilder::cuboid(16.0, 1.0, 16.0), tile_body, &mut w.bodies);
    // The ball rests on the tile that gets removed: live leaf + contact pair.
    add_ball(&mut w, Vector::new(40.0, 1.0, 2.0), 0.5);
    for _ in 0..90 {
        let _ = w.step_journaled(&mut p);
    }
    assert!(w.narrow_phase.contact_pairs().any(|p| p.collider1 == tile || p.collider2 == tile), "setup: the ball must touch the tile");
    // The removal is a user mutation, so the checkpoint is taken after it.
    w.colliders.remove(tile, &mut w.islands, &mut w.bodies, &mut w.soft_bodies, true);
    let before = w.state_bytes();
    let j = w.step_journaled(&mut p);
    w.revert(j);
    assert_same(&before, &w, "collider-removal tick");
}

/// Gate: mmo-fork hunk 'StateHash — the step journal's footprint digests' (step_journal.rs pre_digest/revert_digest, 48726806); checks both match and mismatch
#[test]
fn journal_digest_sees_an_incomplete_revert() {
    let (mut w, mut p) = world();
    add_floor(&mut w, 0, 0);
    let ball = add_ball(&mut w, Vector::new(2.0, 4.0, 2.0), 0.5);
    for _ in 0..3 {
        w.step(&mut p);
    }
    let exact = w.clone(); // what an exact revert of the next step produces
    let j = w.step_journaled(&mut p);
    assert!(j.saved_body_count() > 0);
    let pre = j.pre_digest();
    // No revert at all: the stepped footprint differs.
    assert_ne!(pre, j.live_digest(&w.parts()), "an unreverted step must not digest like the pre-step footprint");
    // An exact revert (the pre-step world) digests like the saved footprint...
    let mut reverted = exact.clone();
    assert_eq!(pre, j.live_digest(&reverted.parts()));
    // ...and one that left the captured bodies stepped does not (the stepped
    // set's modified list is the pre-step one, so only the bodies differ).
    reverted.bodies = w.bodies.clone();
    assert_eq!(reverted.bodies.iter().count(), exact.bodies.iter().count());
    assert_ne!(reverted.bodies[ball].translation(), exact.bodies[ball].translation());
    assert_ne!(pre, j.live_digest(&reverted.parts()), "bodies the revert missed");
    // Nor one whose joint sets, judged quiescent and so not cloned, changed:
    // the digest checks that assumption instead of trusting it.
    assert!(!j.joints_captured());
    let mut reverted = exact.clone();
    let floor = reverted.bodies.iter().find(|(_, b)| b.is_fixed()).unwrap().0;
    reverted.impulse_joints.insert(ball, floor, SphericalJointBuilder::new(), false);
    assert_eq!(reverted.state_bytes()[..5], exact.state_bytes()[..5], "only the joint sets differ");
    assert_ne!(pre, j.live_digest(&reverted.parts()), "joint sets the step wasn't expected to touch");
    // The real revert is exact.
    assert_eq!(pre, j.revert_digest(w.parts()));
}
