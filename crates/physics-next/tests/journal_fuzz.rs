//! Upgrade plan Task 4.2: seeded revert fuzzer for the hybrid `StepJournal`.
//!
//! Each seed builds a scene (fixed floor tiles, two clusters of dynamic balls,
//! a jointed pair, two kinematic capsules) and runs `FUZZ_TICKS` ticks in
//! windows of 1..=`MAX_WINDOW` journaled steps on one long-lived pipeline:
//!
//! 1. user events at the window start (impulses that wake sleepers, body and
//!    collider removals, inserts, a joint removal and re-insertion, kinematic
//!    moves that push the clusters into each other);
//! 2. snapshot the bincode of every journaled structure, step the window
//!    journaled, revert it LIFO and require every intermediate snapshot back
//!    byte for byte;
//!    Each journaled step also checks that every part's `StateHash` moved iff
//!    its bytes did (Task 4.4; `hash_disagreements`).
//! 3. advance by re-stepping the window on a FRESH pipeline (the game's
//!    post-revert rule) and require the result byte-equal to an unjournaled
//!    clone stepped the same way — which catches state the revert missed that
//!    bincode doesn't see (serde-skipped fields, caches).
//!
//! The run asserts it covered each event Review Focus 1 names: a sleeping body
//! woken by contact and by the user, an island merge, a body going to sleep,
//! body and collider removal ticks, and ticks with live joints. The staged
//! solver's writeback debug-asserts (debug builds) that every body it writes
//! was captured first.
use physics_next::{hash_disagreements, PhysicsWorld, differing_parts};
use rapier3d::prelude::*;

/// Seeds of the full run (`journal_fuzz_all_seeds`).
const FUZZ_SEEDS: u64 = 8;
/// Ticks per seed.
const FUZZ_TICKS: usize = 300;
/// Longest window of journaled steps reverted LIFO in one go.
const MAX_WINDOW: u64 = 4;
/// The game's tick (50 ms): bodies fall asleep in ~40 ticks.
const DT: f32 = 0.05;

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn f(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (self.next() % 10_000) as f32 / 10_000.0 * (hi - lo)
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<T: Copy>(&mut self, v: &[T]) -> Option<T> {
        (!v.is_empty()).then(|| v[(self.next() as usize) % v.len()])
    }
}

#[derive(Default, Debug)]
struct Coverage {
    windows: usize,
    steps: usize,
    user_wakes: usize,
    contact_wakes: usize,
    sleeps: usize,
    merges: usize,
    body_removal_steps: usize,
    collider_removal_steps: usize,
    joint_steps: usize,
}

struct Scene {
    w: PhysicsWorld,
    rng: Lcg,
    balls: Vec<RigidBodyHandle>,
    kinematic: [RigidBodyHandle; 2],
    joint: Option<ImpulseJointHandle>,
    tick: usize,
}

const CENTER: f32 = 32.0;

impl Scene {
    fn new(seed: u64) -> Self {
        let mut w = PhysicsWorld { gravity: Vector::new(0.0, -9.81, 0.0), ..Default::default() };
        w.params.dt = DT;
        let rng = Lcg(0x10u64.wrapping_add(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15)));
        for x in 0..3 {
            for z in 0..3 {
                let b = w.bodies.insert(RigidBodyBuilder::fixed().translation(Vector::new((x * 32) as f32, -1.0, (z * 32) as f32)));
                w.colliders.insert_with_parent(ColliderBuilder::cuboid(16.0, 1.0, 16.0), b, &mut w.bodies);
            }
        }
        let mut s = Self {
            w,
            rng,
            balls: Vec::new(),
            kinematic: [RigidBodyHandle::invalid(); 2],
            joint: None,
            tick: 0,
        };
        // Two clusters, far enough apart to start as separate islands.
        for cx in [CENTER - 5.0, CENTER + 5.0] {
            for _ in 0..3 {
                let at = Vector::new(cx + s.rng.f(-0.8, 0.8), s.rng.f(0.5, 3.0), CENTER + s.rng.f(-0.8, 0.8));
                s.add_ball(at);
            }
        }
        s.add_joint_pair();
        for (i, z) in [CENTER - 3.0, CENTER + 3.0].into_iter().enumerate() {
            let k = s.w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(Vector::new(CENTER - 12.0, 0.9, z)));
            s.w.colliders.insert_with_parent(ColliderBuilder::capsule_y(0.5, 0.4), k, &mut s.w.bodies);
            s.kinematic[i] = k;
        }
        s
    }

    fn add_ball(&mut self, at: Vector) -> RigidBodyHandle {
        let b = self.w.bodies.insert(RigidBodyBuilder::dynamic().translation(at).angular_damping(2.0));
        self.w.colliders.insert_with_parent(ColliderBuilder::ball(0.4), b, &mut self.w.bodies);
        self.balls.push(b);
        b
    }

    fn add_joint_pair(&mut self) {
        let z = CENTER + self.rng.f(-6.0, -4.0);
        let a = self.add_ball(Vector::new(CENTER - 0.6, 0.5, z));
        let b = self.add_ball(Vector::new(CENTER + 0.6, 0.5, z));
        let joint = SphericalJointBuilder::new().local_anchor1(Vector::new(0.6, 0.0, 0.0)).local_anchor2(Vector::new(-0.6, 0.0, 0.0));
        self.joint = Some(self.w.impulse_joints.insert(a, b, joint, true));
    }

    fn remove_body(&mut self, h: RigidBodyHandle) {
        let w = &mut self.w;
        w.bodies.remove(h, &mut w.islands, &mut w.colliders, &mut w.impulse_joints, &mut w.multibody_joints, &mut w.soft_bodies, true);
        self.balls.retain(|b| *b != h);
        if self.joint.is_some_and(|j| w.impulse_joints.get(j).is_none()) {
            self.joint = None;
        }
    }

    /// User events before a window; returns (bodies removed, colliders
    /// removed, bodies the user woke).
    fn user_events(&mut self) -> (bool, bool, Vec<RigidBodyHandle>) {
        let (mut body_removed, mut collider_removed, mut woke) = (false, false, Vec::new());
        // Kinematic capsules sweep the floor back and forth, pushing the
        // clusters (and waking/merging them).
        let phase = (self.tick % 120) as f32 / 120.0;
        let x = CENTER - 12.0 + 24.0 * (1.0 - (2.0 * phase - 1.0).abs());
        for (i, k) in self.kinematic.into_iter().enumerate() {
            let z = if i == 0 { CENTER - 3.0 } else { CENTER + 3.0 };
            self.w.bodies.get_mut(k).unwrap().set_next_kinematic_translation(Vector::new(x, 0.9, z));
        }
        if self.rng.chance(6) {
            if let Some(b) = self.rng.pick(&self.balls) {
                if self.w.bodies[b].is_sleeping() {
                    woke.push(b);
                }
                let impulse = Vector::new(self.rng.f(-3.0, 3.0), self.rng.f(0.5, 3.0), self.rng.f(-3.0, 3.0));
                self.w.bodies.get_mut(b).unwrap().apply_impulse(impulse, true);
            }
        }
        if self.rng.chance(3) {
            if let Some(b) = self.rng.pick(&self.balls) {
                self.remove_body(b);
                body_removed = true;
            }
        }
        if self.rng.chance(3) {
            if let Some(b) = self.rng.pick(&self.balls) {
                if let Some(&c) = self.w.bodies[b].colliders().first() {
                    let w = &mut self.w;
                    w.colliders.remove(c, &mut w.islands, &mut w.bodies, &mut w.soft_bodies, true);
                    collider_removed = true;
                }
            }
        }
        if self.rng.chance(4) {
            let at = Vector::new(CENTER + self.rng.f(-6.0, 6.0), self.rng.f(2.0, 4.0), CENTER + self.rng.f(-6.0, 6.0));
            self.add_ball(at);
        }
        // Fallen or collider-less balls are cleaned up (more removal ticks).
        let doomed: Vec<_> = self
            .balls
            .iter()
            .copied()
            .filter(|b| self.w.bodies[*b].translation().y < -10.0 || self.w.bodies[*b].colliders().is_empty())
            .collect();
        for b in doomed {
            self.remove_body(b);
            body_removed = true;
        }
        // The joint goes away mid-run and a new pair appears later.
        if self.tick == FUZZ_TICKS / 2 {
            if let Some(j) = self.joint.take() {
                self.w.impulse_joints.remove(j, true);
            }
        }
        if self.tick == FUZZ_TICKS * 3 / 4 && self.joint.is_none() {
            self.add_joint_pair();
        }
        (body_removed, collider_removed, woke)
    }
}

fn dynamic_bodies(w: &PhysicsWorld) -> Vec<RigidBodyHandle> {
    w.bodies.iter().filter(|(_, b)| b.is_dynamic()).map(|(h, _)| h).collect()
}

/// Records the transitions of one journaled step.
fn observe(pre: &PhysicsWorld, post: &PhysicsWorld, user_woke: &[RigidBodyHandle], cov: &mut Coverage) {
    let bodies = dynamic_bodies(pre);
    for &h in &bodies {
        let (a, b) = (&pre.bodies[h], &post.bodies[h]);
        if a.is_sleeping() && !b.is_sleeping() && !user_woke.contains(&h) {
            cov.contact_wakes += 1;
        }
        if !a.is_sleeping() && b.is_sleeping() {
            cov.sleeps += 1;
        }
    }
    for (i, &h1) in bodies.iter().enumerate() {
        for &h2 in &bodies[i + 1..] {
            let before = (pre.islands.persistent_island_of(&pre.bodies, h1), pre.islands.persistent_island_of(&pre.bodies, h2));
            let after = (post.islands.persistent_island_of(&post.bodies, h1), post.islands.persistent_island_of(&post.bodies, h2));
            if before.0.is_some() && before.1.is_some() && before.0 != before.1 && after.0.is_some() && after.0 == after.1 {
                cov.merges += 1;
            }
        }
    }
}

fn run_seed(seed: u64, cov: &mut Coverage) {
    let mut scene = Scene::new(seed);
    let mut pipeline = PhysicsPipeline::new();
    while scene.tick < FUZZ_TICKS {
        let (body_removed, collider_removed, woke) = scene.user_events();
        cov.user_wakes += woke.len();
        let len = (1 + scene.rng.next() % MAX_WINDOW).min((FUZZ_TICKS - scene.tick) as u64) as usize;
        let reference = scene.w.clone();

        // Forward, journaled.
        let mut snaps = Vec::with_capacity(len);
        let mut journals = Vec::with_capacity(len);
        for k in 0..len {
            let pre_state = (scene.w.state_bytes(), scene.w.state_hashes());
            snaps.push(pre_state.0.clone());
            let pre = scene.w.clone();
            let j = scene.w.step_journaled(&mut pipeline);
            // Task 4.4: the StateHash of each part moves iff its bytes do.
            let bad = hash_disagreements(&pre_state, &(scene.w.state_bytes(), scene.w.state_hashes()));
            assert!(bad.is_empty(), "seed {seed} tick {}: bytes and StateHash disagree on whether {bad:?} changed", scene.tick + k);
            if j.joints_captured() {
                cov.joint_steps += 1;
            }
            if k == 0 {
                cov.body_removal_steps += body_removed as usize;
                cov.collider_removal_steps += collider_removed as usize;
            }
            observe(&pre, &scene.w, if k == 0 { &woke } else { &[] }, cov);
            journals.push(j);
            cov.steps += 1;
        }

        // Back, LIFO, every intermediate state exact.
        for (k, j) in journals.into_iter().enumerate().rev() {
            scene.w.revert(j);
            let got = scene.w.state_bytes();
            assert!(
                got == snaps[k],
                "seed {seed} tick {}: revert of window step {k}/{len} differs in {:?}",
                scene.tick + k,
                differing_parts(&snaps[k], &got)
            );
        }

        // Advance on a fresh pipeline, against an unjournaled clone.
        pipeline = PhysicsPipeline::new();
        let mut reference = reference;
        let mut ref_pipeline = PhysicsPipeline::new();
        for k in 0..len {
            scene.w.step(&mut pipeline);
            reference.step(&mut ref_pipeline);
            let (got, expect) = (scene.w.state_bytes(), reference.state_bytes());
            assert!(
                got == expect,
                "seed {seed} tick {}: re-step after revert differs from an unjournaled step in {:?}",
                scene.tick + k,
                differing_parts(&expect, &got)
            );
        }
        scene.tick += len;
        cov.windows += 1;
    }
}

/// One seed, fast enough for every `test-one` run.
#[test]
fn journal_fuzz_one_seed() {
    let mut cov = Coverage::default();
    run_seed(0, &mut cov);
    assert!(cov.steps == FUZZ_TICKS, "{cov:?}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn journal_fuzz_all_seeds() {
    let mut cov = Coverage::default();
    for seed in 0..FUZZ_SEEDS {
        run_seed(seed, &mut cov);
    }
    eprintln!("journal fuzz coverage over {FUZZ_SEEDS} seeds x {FUZZ_TICKS} ticks: {cov:?}");
    assert!(cov.user_wakes > 0, "no user wake of a sleeping body: {cov:?}");
    assert!(cov.contact_wakes > 0, "no contact wake of a sleeping body: {cov:?}");
    assert!(cov.sleeps > 0, "no body fell asleep: {cov:?}");
    assert!(cov.merges > 0, "no island merge: {cov:?}");
    assert!(cov.body_removal_steps > 0, "no body-removal step: {cov:?}");
    assert!(cov.collider_removal_steps > 0, "no collider-removal step: {cov:?}");
    assert!(cov.joint_steps > 0, "no step with joints: {cov:?}");
}
