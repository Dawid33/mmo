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
//!   an exact revert, and perturbing one captured body in it for an incomplete
//!   one (`live_digest` reads the footprint without reverting).
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

#[test]
fn idle_tick_touches_no_body_or_collider() {
    let (mut w, mut p) = world();
    for x in 0..8 {
        for z in 0..8 {
            add_floor(&mut w, x, z);
        }
    }
    for _ in 0..3 {
        let _ = w.step_journaled(&mut p);
    }
    let before = w.state_bytes();
    let j = w.step_journaled(&mut p);
    assert!(j.is_empty(), "idle tick captured {} bodies, {} colliders", j.saved_body_count(), j.saved_collider_count());
    assert!(j.broad_captured(), "the hybrid clones the broad phase every step");
    w.revert(j);
    assert_same(&before, &w, "idle tick revert");
}

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

#[test]
fn ball_rests_on_untouched_terrain_no_tunneling() {
    let (mut w, mut p) = world();
    for x in 0..2 {
        for z in 0..2 {
            add_floor(&mut w, x, z);
        }
    }
    let ball = add_ball(&mut w, Vector::new(2.0, 8.0, 2.0), 0.5);
    for _ in 0..200 {
        let _ = w.step_journaled(&mut p);
    }
    let y = w.bodies[ball].translation().y;
    assert!(y > 0.0, "ball tunneled through untouched terrain: y = {y}");
}

#[test]
fn landing_on_floor_every_intermediate_tick_reverts_exactly() {
    let (mut w, mut p) = world();
    add_floor(&mut w, 0, 0);
    add_ball(&mut w, Vector::new(2.0, 3.0, 2.0), 0.5);
    let mut checkpoints = vec![w.state_bytes()];
    let mut journals = Vec::new();
    for _ in 0..90 {
        // Fall, impact (pair add + manifolds + warm start), settle.
        journals.push(w.step_journaled(&mut p));
        checkpoints.push(w.state_bytes());
    }
    for (tick, (j, expect)) in journals.into_iter().zip(checkpoints.into_iter()).enumerate().rev() {
        w.revert(j);
        assert_same(&expect, &w, &format!("revert of tick {tick}"));
    }
}

#[test]
fn flyby_pair_create_then_destroy_reverts_exactly() {
    let (mut w, mut p) = world();
    // Two adjacent tiles: at the hand-off two contact edges coexist and
    // leaving tile 0 removes a non-last edge.
    add_floor(&mut w, 0, 0);
    add_floor(&mut w, 1, 0);
    // Kinematic↔fixed pairs are filtered by default in 0.36 (Task 3.5), so
    // the bullet opts in to make the pair appear and disappear.
    let b = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(Vector::new(-40.0, 0.5, 2.0)));
    w.colliders.insert_with_parent(
        ColliderBuilder::ball(0.5).active_collision_types(ActiveCollisionTypes::all()),
        b,
        &mut w.bodies,
    );
    // Snapshot after the first user write: the step journal owns only the
    // step, the game log owns user mutations.
    let mut journals = Vec::new();
    let mut before = None;
    let mut max_pairs = 0;
    for i in 0..60 {
        let x = -40.0 + i as f32 * 2.0;
        w.bodies.get_mut(b).unwrap().set_next_kinematic_translation(Vector::new(x, 0.5, 2.0));
        if before.is_none() {
            before = Some(w.state_bytes());
        }
        journals.push(w.step_journaled(&mut p));
        max_pairs = max_pairs.max(w.narrow_phase.contact_pairs().filter(|p| p.has_any_active_contact()).count());
    }
    assert!(max_pairs >= 2, "the fly-by must touch both tiles at once (max {max_pairs})");
    // Each journal restores the pre-step value of what its step touched, so
    // reverting tick 0 last also undoes the later user writes' effects on the
    // bodies (value) and the modified list (wholesale).
    for j in journals.into_iter().rev() {
        w.revert(j);
    }
    assert_same(&before.unwrap(), &w, "fly-by LIFO revert");
}

#[test]
fn free_fall_lifo_revert_is_exact() {
    let (mut w, mut p) = world();
    // No colliders at all: integration, activation, forces, mass props, sleep
    // bookkeeping and the modified lists.
    w.bodies.insert(RigidBodyBuilder::dynamic().translation(Vector::new(0.0, 10.0, 0.0)));
    let before = w.state_bytes();
    let journals: Vec<StepJournal> = (0..10).map(|_| w.step_journaled(&mut p)).collect();
    for j in journals.into_iter().rev() {
        w.revert(j);
    }
    assert_same(&before, &w, "free-fall 10-step LIFO revert");
}

#[test]
fn active_parent_collider_edit_reverts_exactly() {
    // The old fork's stale-flags regression: an active body's collider edited
    // through the tracking path between steps.
    let (mut w, mut p) = world();
    let b = w.bodies.insert(RigidBodyBuilder::dynamic().translation(Vector::new(0.0, 10.0, 0.0)));
    let c = w.colliders.insert_with_parent(ColliderBuilder::ball(0.5), b, &mut w.bodies);
    let _ = w.step_journaled(&mut p);
    w.colliders.get_mut(c).unwrap().set_density(2.0);
    let before = w.state_bytes();
    let j = w.step_journaled(&mut p);
    w.revert(j);
    assert_same(&before, &w, "active-parent collider edit tick");
}

#[test]
fn parented_collider_edit_reverts_exactly() {
    // A collider edit that makes the step push the (fixed) parent onto the
    // modified-bodies list: the journal must restore the pre-step list.
    let (mut w, mut p) = world();
    let b = w.bodies.insert(RigidBodyBuilder::fixed());
    let c = w.colliders.insert_with_parent(ColliderBuilder::ball(0.5), b, &mut w.bodies);
    let _ = w.step_journaled(&mut p);
    w.colliders.get_mut(c).unwrap().set_density(2.0);
    let before = w.state_bytes();
    let j = w.step_journaled(&mut p);
    w.revert(j);
    assert_same(&before, &w, "parented collider edit tick");
}

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

/// Prints the hybrid's per-tick capture for an idle and a one-moving-body
/// scene (not a gate):
/// `just test-one physics-next "--test step_journal -- --ignored --nocapture perf_evidence"`.
#[test]
#[ignore = "perf evidence, prints numbers; run with --ignored --nocapture"]
fn perf_evidence_journal_scales_with_activity() {
    fn tally(label: &str, mut w: PhysicsWorld, ticks: usize) {
        let mut p = PhysicsPipeline::new();
        let (mut bodies, mut colliders) = (0usize, 0usize);
        let broad_bytes = bincode::serialized_size(&w.broad_phase).unwrap();
        for _ in 0..ticks {
            let j = w.step_journaled(&mut p);
            bodies += j.saved_body_count();
            colliders += j.saved_collider_count();
        }
        println!("{label}: {ticks} ticks -> saved_bodies={bodies}, saved_colliders={colliders}, broad clone ~{broad_bytes} B/tick");
    }
    let (mut idle, mut p) = world();
    for x in 0..8 {
        for z in 0..8 {
            add_floor(&mut idle, x, z);
        }
    }
    for _ in 0..5 {
        let _ = idle.step_journaled(&mut p);
    }
    tally("idle", idle, 100);
    let (mut moving, mut p) = world();
    for x in 0..8 {
        for z in 0..8 {
            add_floor(&mut moving, x, z);
        }
    }
    add_ball(&mut moving, Vector::new(2.0, 60.0, 2.0), 0.4);
    for _ in 0..5 {
        let _ = moving.step_journaled(&mut p);
    }
    tally("one-moving-body", moving, 100);
}

/// Deterministic LCG so the test needs no rand dependency.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn f(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (self.next() % 10_000) as f32 / 10_000.0 * (hi - lo)
    }
}

/// The old fork's wake fuzzer: a pile of balls falls asleep, then an impulse
/// on one ball user-wakes it (dormant pair, warm start) and it contact-wakes
/// its neighbours. Per-tick standalone revert (snapshot, step, revert, assert,
/// re-step), since a user wake can't be walked back by a multi-tick replay.
#[test]
fn randomized_pile_wake_per_tick_revert_is_exact() {
    let mut rng = Lcg(0x5EED_2026_0704);
    let (mut w, mut p) = world();
    for x in 0..4 {
        for z in 0..4 {
            add_floor(&mut w, x, z);
        }
    }
    // Differences from the old scene: the pile sits mid-tile (1, 1) instead of
    // near tile (0, 0)'s edge, and the balls have angular damping. 0.36 has no
    // rolling resistance, so undamped balls rolled apart for ever (one off the
    // floor) and never slept; this way they sleep from tick ~184.
    let balls: Vec<_> = (0..6)
        .map(|_| {
            let at = Vector::new(32.0 + rng.f(-1.0, 1.0), rng.f(1.0, 5.0), 32.0 + rng.f(-1.0, 1.0));
            let b = w.bodies.insert(RigidBodyBuilder::dynamic().translation(at).angular_damping(2.0));
            w.colliders.insert_with_parent(ColliderBuilder::ball(0.4), b, &mut w.bodies);
            b
        })
        .collect();
    let sleeping = |w: &PhysicsWorld| balls.iter().filter(|b| w.bodies[**b].is_sleeping()).count();
    let (mut any_slept, mut woke_a_sleeper) = (false, false);
    for tick in 0..460 {
        let sleeping_before = sleeping(&w);
        any_slept |= sleeping_before > 0;
        if tick == 300 || tick == 350 || tick == 400 {
            let b = balls[(rng.next() as usize) % balls.len()];
            let impulse = Vector::new(rng.f(-8.0, 8.0), rng.f(3.0, 8.0), rng.f(-8.0, 8.0));
            w.bodies.get_mut(b).unwrap().apply_impulse(impulse, true);
        }
        let before = w.state_bytes();
        let j = w.step_journaled(&mut p);
        w.revert(j);
        assert_same(&before, &w, &format!("tick {tick}"));
        // Re-step on a fresh pipeline, as the game does after a revert.
        p = PhysicsPipeline::new();
        let _ = w.step_journaled(&mut p);
        if sleeping_before > 0 && sleeping(&w) < sleeping_before {
            woke_a_sleeper = true;
        }
    }
    assert!(any_slept, "the pile must fall asleep");
    assert!(woke_a_sleeper, "the impulses must wake a sleeping ball");
}

#[test]
fn journal_size_scales_with_activity_not_world_size() {
    let saved_counts = |chunks: i32| -> (usize, usize) {
        let (mut w, mut p) = world();
        for x in 0..chunks {
            for z in 0..chunks {
                add_floor(&mut w, x, z);
            }
        }
        add_ball(&mut w, Vector::new(2.0, 5.0, 2.0), 0.4);
        for _ in 0..5 {
            let _ = w.step_journaled(&mut p);
        }
        let j = w.step_journaled(&mut p); // one falling ball, mid-air
        (j.saved_body_count(), j.saved_collider_count())
    };
    let small = saved_counts(2);
    let big = saved_counts(8);
    assert_eq!(small, big, "per-tick saved bodies/colliders must not scale with chunk count");
    assert_eq!(small, (1, 1), "only the falling ball and its collider are touched");
}

#[test]
fn sleep_transition_lifo_revert_is_exact() {
    let (mut w, mut p) = world();
    let b = w.bodies.insert(RigidBodyBuilder::dynamic().gravity_scale(0.0));
    let before = w.state_bytes();
    let journals: Vec<StepJournal> = (0..240).map(|_| w.step_journaled(&mut p)).collect();
    assert!(w.bodies[b].is_sleeping(), "setup: a still body must fall asleep");
    for j in journals.into_iter().rev() {
        w.revert(j);
    }
    assert_same(&before, &w, "sleep-transition LIFO revert");
}

#[test]
fn impact_wakes_sleeper_every_tick_reverts_exactly() {
    let (mut w, mut p) = world();
    add_floor(&mut w, 0, 0);
    let b = add_ball(&mut w, Vector::new(2.0, 1.0, 2.0), 0.5);
    for _ in 0..300 {
        let _ = w.step_journaled(&mut p);
    }
    assert!(w.bodies[b].is_sleeping(), "precondition: B must be asleep");
    add_ball(&mut w, Vector::new(2.0, 3.0, 2.0), 0.5);
    let mut checkpoints = Vec::new();
    let mut journals = Vec::new();
    let mut woke = false;
    for _ in 0..120 {
        checkpoints.push(w.state_bytes());
        journals.push(w.step_journaled(&mut p));
        woke |= !w.bodies[b].is_sleeping();
    }
    assert!(woke, "scenario must wake B by impact");
    for (tick, (j, expect)) in journals.into_iter().zip(checkpoints).enumerate().rev() {
        w.revert(j);
        assert_same(&expect, &w, &format!("revert of tick {tick}"));
    }
}

#[test]
fn user_wake_of_resting_sleeper_reverts_exactly() {
    let (mut w, mut p) = world();
    add_floor(&mut w, 0, 0);
    let b = add_ball(&mut w, Vector::new(2.0, 1.0, 2.0), 0.5);
    for _ in 0..300 {
        let _ = w.step_journaled(&mut p);
    }
    assert!(w.bodies[b].is_sleeping(), "precondition: B must be asleep");
    w.bodies.get_mut(b).unwrap().apply_impulse(Vector::new(0.5, 1.0, 0.0), true);
    let before = w.state_bytes();
    let j = w.step_journaled(&mut p);
    w.revert(j);
    assert_same(&before, &w, "user-wake tick");
}

#[test]
fn journal_digest_matches_after_exact_revert() {
    let (mut w, mut p) = world();
    for x in 0..4 {
        for z in 0..4 {
            add_floor(&mut w, x, z);
        }
    }
    add_ball(&mut w, Vector::new(2.0, 8.0, 2.0), 0.5);
    for _ in 0..3 {
        w.step(&mut p);
    }
    // Fall, impact, settle: dirty ticks of several shapes.
    for tick in 0..60 {
        let j = w.step_journaled(&mut p);
        let pre = j.pre_digest();
        let live = j.revert_digest(w.parts());
        assert_eq!(pre, live, "tick {tick}: footprint digest after exact revert must match");
        w.step(&mut p); // advance again (state was restored)
    }
}

#[test]
fn journal_digest_matches_on_collider_removal_tick() {
    let (mut w, mut p) = world();
    add_floor(&mut w, 0, 0);
    let tile_body = w.bodies.insert(RigidBodyBuilder::fixed().translation(Vector::new(40.0, -1.0, 2.0)));
    let tile = w.colliders.insert_with_parent(ColliderBuilder::cuboid(16.0, 1.0, 16.0), tile_body, &mut w.bodies);
    add_ball(&mut w, Vector::new(40.0, 1.0, 2.0), 0.5);
    for _ in 0..90 {
        w.step(&mut p);
    }
    w.colliders.remove(tile, &mut w.islands, &mut w.bodies, &mut w.soft_bodies, true);
    let j = w.step_journaled(&mut p);
    assert!(j.broad_captured());
    let pre = j.pre_digest();
    let live = j.revert_digest(w.parts());
    assert_eq!(pre, live, "removal-tick digest must match after exact revert");
}

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
    // Nor one that left the broad phase stepped.
    let mut reverted = exact.clone();
    reverted.broad_phase = w.broad_phase.clone();
    assert_ne!(pre, j.live_digest(&reverted.parts()), "a broad phase the revert missed");
    // The real revert is exact.
    assert_eq!(pre, j.revert_digest(w.parts()));
}
