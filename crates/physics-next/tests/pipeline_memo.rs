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
//! - **Re-stepping the same input on the same pipeline does not diverge**
//!   (both scenes): the re-step bumps the epochs to the values the undone
//!   step did, and finds the memos that step computed for identical state.
//! - **A corrected input on the same pipeline DIVERGES** at the first
//!   re-stepped tick in `PairSleeps` (bodies, colliders, broad phase, impulse
//!   joints differ): the undone step woke the pair and bumped the active-set
//!   epoch; the revert restored the old epoch and the joint set's old
//!   `selection_epochs`, so the corrected step — which wakes nothing — gets a
//!   memo hit and solves the undone future's joint selection. (`PairAwake`
//!   gets no memo hit on the re-step — its active-set epoch moves anyway — and
//!   is printed, not asserted, by that test.)
//! - A fresh `PhysicsPipeline` + `CCDSolver`, or the same ones after
//!   `PhysicsPipeline::invalidate_caches` +
//!   `CCDSolver::invalidate_fixed_targets_cache`, match the reference byte for
//!   byte in every case. Which part of `invalidate_caches` is load-bearing
//!   (mutation runs, H7): dropping both the selection-buffer clear and the
//!   un-priming makes `PairSleeps` diverge; either one alone suffices (the
//!   un-priming forces a re-selection that overwrites the buffer). The
//!   staged-cache reset is not observable here (its reuse gate also compares
//!   the selected joint indices and group ranges) and is kept as insurance;
//!   `CCDSolver`'s fixed-targets cache has no fixed collider change to go
//!   stale on in these scenes.
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

/// Pins why the game must not re-step on the same pipeline without
/// `invalidate_caches`: in `PairSleeps` the undone step's joint selection is
/// reused. `PairAwake`'s outcome is printed for the record.
#[test]
fn same_pipeline_after_a_corrected_input_diverges() {
    for case in CASES {
        let expect = reference(case);
        let s = rollback(case, true);
        let outcome = catch_unwind(AssertUnwindSafe(|| advance(s, AfterRevert::SamePipeline)));
        let diverged = match outcome {
            Ok(got) => {
                let d = first_divergence(&got, &expect);
                eprintln!("{case:?}: same pipeline after a corrected input: first divergence {d:?}");
                d.is_some()
            }
            Err(e) => {
                let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()));
                eprintln!("{case:?}: same pipeline after a corrected input: re-step panicked: {msg:?}");
                true
            }
        };
        if matches!(case, Case::PairSleeps) {
            assert!(diverged, "expected the stale joint selection to change the re-step");
        }
    }
}
