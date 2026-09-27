//! Upgrade plan Task 4.3 (Review Focus 2): after a `StepJournal` revert, the
//! `PhysicsPipeline`'s memos describe the step that was undone. They are
//! keyed on epochs the revert rolls back (`IslandManager::active_set_epoch`,
//! `ImpulseJointSet::assembly_epoch`, via the joint set's `selection_epochs`),
//! while the memoized data lives in the pipeline, which the journal does not
//! restore: the joint selection (`joint_constraint_indices`,
//! `joint_selection_primed`) and the staged solver's joint-assembly cache.
//!
//! The scenes have an impulse joint although the game has none today: a
//! jointed pair of balls on a floor, a lone ball asleep beside it, and a ball
//! drifting in mid-air far away, which keeps an island awake at a stable
//! active-set epoch so the solver runs every tick. At tick `SETTLE` a rollback
//! happens the way the game does it: a "wrong" input (a user impulse that
//! wakes a sleeping body) is stepped journaled, the step is reverted and the
//! input undone with the Task 4.1 primitives, and the tick is re-stepped under
//! the corrected input (no impulse). Each variant is compared, tick by tick
//! for `AFTER` ticks, with a run that only ever saw the corrected input.
//! - `PairSleeps`: the pair sleeps and the wrong impulse wakes it.
//! - `PairAwake`: the pair never sleeps and the wrong impulse wakes the lone
//!   ball instead.
//!
//! Findings (0.36 fork):
//! - **Re-stepping the same input on the same pipeline never diverged**
//!   (both scenes): the re-step bumps the epochs to the values the undone
//!   step did, and finds the memos that step computed for identical state.
//! - **A corrected input on the same pipeline DIVERGED** (before the H7
//!   review fix) at the first re-stepped tick in `PairSleeps` (bodies,
//!   colliders, broad phase, impulse joints differed): the undone step woke
//!   the pair and bumped the active-set epoch; the revert restored the old
//!   epoch and the joint set's old `selection_epochs`, so the corrected step —
//!   which wakes nothing — got a memo hit and solved the undone future's joint
//!   selection. `PairAwake` got no memo hit (its epoch moves anyway).
//! - The fix (H7 review I2): the game's undo closure reaches the physics state
//!   but not the pipeline, so the revert bumps `IslandManager::journal_reverts`
//!   (serde-skipped) and the pipeline calls `invalidate_caches` itself when it
//!   sees the counter move; the revert also discards the `CCDSolver`'s
//!   fixed-targets cache, which could name a collider the game's undo removed
//!   (`ccd_after_an_undone_fixed_collider_insert_…`: it panicked indexing the
//!   vacant slot). Same pipeline, fresh pipeline and explicit
//!   `invalidate_caches` now all match the reference byte for byte.
//! - Which part of `invalidate_caches` is load-bearing (mutation runs, H7):
//!   dropping both the selection-buffer clear and the un-priming makes
//!   `PairSleeps` diverge; either one alone suffices (the un-priming forces a
//!   re-selection that overwrites the buffer). The staged-cache reset is not
//!   observable here (its reuse gate also compares the selected joint indices
//!   and group ranges) and is kept as insurance.
use physics_next::{PhysicsWorld, differing_parts};
use rapier3d::prelude::*;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Ticks the scene settles before the rollback.
const SETTLE: usize = 80;
/// Ticks compared after the rollback.
const AFTER: usize = 40;

#[derive(Clone, Copy, Debug)]
enum Case {
    PairSleeps,
    PairAwake,
}
const CASES: [Case; 2] = [Case::PairSleeps, Case::PairAwake];

struct Scene {
    w: PhysicsWorld,
    p: PhysicsPipeline,
    pair: [RigidBodyHandle; 2],
    lone: RigidBodyHandle,
}

fn scene(case: Case) -> Scene {
    let mut w = PhysicsWorld { gravity: Vector::new(0.0, -9.81, 0.0), ..Default::default() };
    w.params.dt = 0.05;
    let floor = w.bodies.insert(RigidBodyBuilder::fixed().translation(Vector::new(0.0, -1.0, 0.0)));
    w.colliders.insert_with_parent(ColliderBuilder::cuboid(16.0, 1.0, 16.0), floor, &mut w.bodies);
    let pair_can_sleep = matches!(case, Case::PairSleeps);
    let mut ball = |x: f32, can_sleep: bool| {
        let rb = RigidBodyBuilder::dynamic().translation(Vector::new(x, 0.4, 0.0)).angular_damping(2.0).can_sleep(can_sleep);
        let b = w.bodies.insert(rb);
        w.colliders.insert_with_parent(ColliderBuilder::ball(0.4), b, &mut w.bodies);
        b
    };
    let pair = [ball(-0.6, pair_can_sleep), ball(0.6, pair_can_sleep)];
    let lone = ball(-6.0, true);
    let joint = SphericalJointBuilder::new().local_anchor1(Vector::new(0.6, 0.0, 0.0)).local_anchor2(Vector::new(-0.6, 0.0, 0.0));
    w.impulse_joints.insert(pair[0], pair[1], joint, true);
    let drifter = w.bodies.insert(
        RigidBodyBuilder::dynamic().translation(Vector::new(8.0, 5.0, 0.0)).linvel(Vector::new(0.0, 0.0, 0.5)).gravity_scale(0.0),
    );
    w.colliders.insert_with_parent(ColliderBuilder::ball(0.4), drifter, &mut w.bodies);
    let mut s = Scene { w, p: PhysicsPipeline::new(), pair, lone };
    for _ in 0..SETTLE {
        s.w.step(&mut s.p);
    }
    assert!(s.w.bodies[s.lone].is_sleeping(), "setup: the lone ball must be asleep");
    assert!(
        s.pair.iter().all(|b| s.w.bodies[*b].is_sleeping()) == pair_can_sleep,
        "setup: the pair must be asleep iff {case:?} is PairSleeps"
    );
    s
}

/// The reference: only ever the corrected input (no impulse).
fn reference(case: Case) -> Vec<[Vec<u8>; 8]> {
    let mut s = scene(case);
    (0..AFTER)
        .map(|_| {
            s.w.step(&mut s.p);
            s.w.state_bytes()
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum AfterRevert {
    SamePipeline,
    FreshPipeline,
    InvalidateCaches,
}

/// Rollback at tick `SETTLE`: journaled step under the wrong input (a user
/// impulse waking a sleeping body) when `wrong_input`, else under the
/// corrected one (none); revert; undo the input. Returns the scene on the
/// pre-step state.
fn rollback(case: Case, wrong_input: bool) -> Scene {
    let mut s = scene(case);
    let before = s.w.state_bytes();
    let target = match case {
        Case::PairSleeps => s.pair[0],
        Case::PairAwake => s.lone,
    };
    let pre = s.w.bodies[target].clone();
    let was_modified = s.w.bodies.is_marked_modified(target);
    if wrong_input {
        s.w.bodies.get_mut(target).unwrap().apply_impulse(Vector::new(0.0, 1.5, 0.0), true);
    }
    let j = s.w.step_journaled(&mut s.p);
    if wrong_input {
        assert!(!s.w.bodies[target].is_sleeping(), "the wrong input must wake {target:?}");
    }
    s.w.revert(j);
    if wrong_input {
        s.w.bodies.restore_raw(target, pre);
        if !was_modified {
            s.w.bodies.pop_modified(target);
        }
    }
    let got = s.w.state_bytes();
    assert!(got == before, "rollback must land on the pre-step state: {:?}", differing_parts(&before, &got));
    s
}

/// `AFTER` ticks under the corrected input, after `after`.
fn advance(mut s: Scene, after: AfterRevert) -> Vec<[Vec<u8>; 8]> {
    match after {
        AfterRevert::SamePipeline => {}
        AfterRevert::FreshPipeline => {
            s.p = PhysicsPipeline::new();
            s.w.ccd = CCDSolver::new();
        }
        AfterRevert::InvalidateCaches => {
            s.p.invalidate_caches();
            s.w.ccd.invalidate_fixed_targets_cache();
        }
    }
    (0..AFTER)
        .map(|_| {
            s.w.step(&mut s.p);
            s.w.state_bytes()
        })
        .collect()
}

/// First tick (after the rollback) whose state differs from the reference.
fn first_divergence(got: &[[Vec<u8>; 8]], expect: &[[Vec<u8>; 8]]) -> Option<(usize, Vec<&'static str>)> {
    got.iter().zip(expect).enumerate().find(|(_, (g, e))| g != e).map(|(t, (g, e))| (t, differing_parts(e, g)))
}

#[test]
fn fresh_pipeline_after_revert_matches_a_never_reverted_run() {
    for case in CASES {
        let expect = reference(case);
        for wrong_input in [false, true] {
            let got = advance(rollback(case, wrong_input), AfterRevert::FreshPipeline);
            assert_eq!(first_divergence(&got, &expect), None, "{case:?}, fresh pipeline, wrong_input={wrong_input}");
        }
    }
}

#[test]
fn invalidated_caches_after_revert_match_a_never_reverted_run() {
    for case in CASES {
        let expect = reference(case);
        for wrong_input in [false, true] {
            let got = advance(rollback(case, wrong_input), AfterRevert::InvalidateCaches);
            assert_eq!(first_divergence(&got, &expect), None, "{case:?}, invalidate_caches, wrong_input={wrong_input}");
        }
    }
}

#[test]
fn same_pipeline_replaying_the_same_input_does_not_diverge() {
    for case in CASES {
        let got = advance(rollback(case, false), AfterRevert::SamePipeline);
        assert_eq!(first_divergence(&got, &reference(case)), None, "{case:?}");
    }
}

/// H7 review I2: the game's undo closure reaches the physics state but not
/// the pipeline, so after a revert the SAME pipeline must be safe: the revert
/// bumps a counter in the `IslandManager` and the pipeline invalidates its
/// memos when it sees it move. (Before that fix this re-step diverged at the
/// first tick in `PairSleeps`: the undone step's joint selection was reused.)
#[test]
fn same_pipeline_after_a_corrected_input_matches_a_never_reverted_run() {
    for case in CASES {
        let expect = reference(case);
        let s = rollback(case, true);
        let got = catch_unwind(AssertUnwindSafe(|| advance(s, AfterRevert::SamePipeline)));
        let got = got.unwrap_or_else(|_| panic!("{case:?}: same-pipeline re-step panicked"));
        assert_eq!(first_divergence(&got, &expect), None, "{case:?}, same pipeline after a corrected input");
    }
}

/// H7 review I2 (CCD): the game's terrain edit is a fixed collider insert.
/// A journaled step with it rebuilds `CCDSolver`'s fixed-targets cache; the
/// rollback reverts the step and undoes the insert (`revert_insert` pops the
/// modified list), so the re-step sees no user change and would keep the
/// cache — with the vacant wall in it. The revert must invalidate it.
#[test]
fn ccd_after_an_undone_fixed_collider_insert_matches_a_never_inserted_run() {
    fn world() -> (PhysicsWorld, PhysicsPipeline, RigidBodyHandle) {
        let mut w = PhysicsWorld::default();
        w.params.dt = 0.05;
        let ball = w.bodies.insert(
            // A fast dynamic body WITHOUT `ccd_enabled` is a non-bullet CCD
            // origin in 0.36: it sweeps the cached fixed-target list.
            RigidBodyBuilder::dynamic().translation(Vector::new(-6.0, 0.0, 0.0)).linvel(Vector::new(60.0, 0.0, 0.0)),
        );
        w.colliders.insert_with_parent(ColliderBuilder::ball(0.2), ball, &mut w.bodies);
        let mut p = PhysicsPipeline::new();
        for _ in 0..2 {
            w.step(&mut p);
        }
        (w, p, ball)
    }
    let (mut r, mut rp, ball) = world();
    let expect: Vec<_> = (0..5)
        .map(|_| {
            r.step(&mut rp);
            r.state_bytes()
        })
        .collect();

    let (mut w, mut p, _) = world();
    let (head, len) = w.colliders.alloc_state();
    let wall = w.colliders.insert(ColliderBuilder::cuboid(0.1, 5.0, 5.0).translation(Vector::new(2.0, 0.0, 0.0)));
    let j = w.step_journaled(&mut p);
    assert!(w.bodies[ball].translation().x < 2.0, "setup: CCD must stop the ball at the wall");
    w.revert(j);
    w.colliders.revert_insert(wall, head, len);
    let got = catch_unwind(AssertUnwindSafe(|| {
        (0..5)
            .map(|_| {
                w.step(&mut p);
                w.state_bytes()
            })
            .collect::<Vec<_>>()
    }))
    .unwrap_or_else(|_| panic!("re-step after the undone insert panicked"));
    assert_eq!(first_divergence(&got, &expect), None);
}

/// H7 review M1 (re-graded): `invalidate_caches` right after a zero-`dt`
/// step must not lose the broad phase. Such a step leaves its deferred tree
/// optimization pending in the pipeline, holding the whole tree.
#[test]
fn invalidate_caches_after_a_zero_dt_step_keeps_the_broad_phase() {
    fn run(invalidate: bool) -> Vec<u8> {
        let mut w = PhysicsWorld { gravity: Vector::new(0.0, -9.81, 0.0), ..Default::default() };
        let floor = w.colliders.insert(ColliderBuilder::cuboid(16.0, 1.0, 16.0).translation(Vector::new(0.0, -1.0, 0.0)));
        let _ = floor;
        let b = w.bodies.insert(RigidBodyBuilder::dynamic().translation(Vector::new(0.0, 1.0, 0.0)));
        w.colliders.insert_with_parent(ColliderBuilder::ball(0.4), b, &mut w.bodies);
        let mut p = PhysicsPipeline::new();
        // Past the first broad-phase pass (which never defers), then a zero-dt
        // step whose update has enough changed leaves to defer an optimization.
        w.params.dt = 0.05;
        for _ in 0..3 {
            w.step(&mut p);
        }
        w.bodies.get_mut(b).unwrap().set_translation(Vector::new(0.0, 1.5, 0.0), true);
        w.params.dt = 0.0;
        w.step(&mut p);
        if invalidate {
            p.invalidate_caches();
        }
        w.params.dt = 0.05;
        for _ in 0..40 {
            w.step(&mut p);
        }
        assert!(w.bodies[b].translation().y > 0.0, "invalidate={invalidate}: the ball fell through the floor");
        bincode::serialize(&w.bodies).unwrap()
    }
    assert!(run(true) == run(false), "invalidate_caches changed the simulation");
}

/// H7 review M1: a journaled zero-`dt` step must not leave its deferred tree
/// optimization pending in the pipeline (it owns the tree of the state the
/// revert discards): reverting it and stepping on matches a run that never
/// took it.
#[test]
fn reverted_zero_dt_step_steps_on_like_a_run_that_never_took_it() {
    fn scene() -> (PhysicsWorld, PhysicsPipeline, RigidBodyHandle) {
        let mut w = PhysicsWorld { gravity: Vector::new(0.0, -9.81, 0.0), ..Default::default() };
        w.colliders.insert(ColliderBuilder::cuboid(16.0, 1.0, 16.0).translation(Vector::new(0.0, -1.0, 0.0)));
        let b = w.bodies.insert(RigidBodyBuilder::dynamic().translation(Vector::new(0.0, 1.0, 0.0)));
        w.colliders.insert_with_parent(ColliderBuilder::ball(0.4), b, &mut w.bodies);
        let mut p = PhysicsPipeline::new();
        w.params.dt = 0.05;
        for _ in 0..3 {
            w.step(&mut p);
        }
        (w, p, b)
    }
    let run = |zero_dt_rollback: bool| {
        let (mut w, mut p, b) = scene();
        let pre = w.bodies[b].clone();
        let was_modified = w.bodies.is_marked_modified(b);
        if zero_dt_rollback {
            // A user move makes the zero-dt update defer an optimization.
            w.bodies.get_mut(b).unwrap().set_translation(Vector::new(0.0, 1.5, 0.0), true);
            w.params.dt = 0.0;
            let j = w.step_journaled(&mut p);
            w.revert(j);
            w.bodies.restore_raw(b, pre);
            if !was_modified {
                w.bodies.pop_modified(b);
            }
            w.params.dt = 0.05;
        }
        (0..40)
            .map(|_| {
                w.step(&mut p);
                w.state_bytes()
            })
            .collect::<Vec<_>>()
    };
    let expect = run(false);
    assert_eq!(first_divergence(&run(true), &expect), None);
}
