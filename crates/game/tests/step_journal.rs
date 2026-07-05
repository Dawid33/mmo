//! Fork-level StepJournal invariants: step-then-revert must be hash-exact.
//! Style mirrors hash_restore.rs: build components directly, crc32 the Hash.
use rapier3d::prelude::*;

struct Crc32Std(crc32fast::Hasher);
impl std::hash::Hasher for Crc32Std {
    fn write(&mut self, bytes: &[u8]) { self.0.update(bytes) }
    fn finish(&self) -> u64 { self.0.clone().finalize() as u64 }
}
fn h<T: std::hash::Hash>(t: &T) -> u32 {
    let mut hasher = Crc32Std(crc32fast::Hasher::new());
    t.hash(&mut hasher);
    std::hash::Hasher::finish(&hasher) as u32
}

struct World {
    pipeline: PhysicsPipeline,
    islands: IslandManager,
    broad: BroadPhaseBvh,
    narrow: NarrowPhase,
    bodies: RigidBodySet,
    colliders: ColliderSet,
    ij: ImpulseJointSet,
    mj: MultibodyJointSet,
    ccd: CCDSolver,
    params: IntegrationParameters,
    gravity: Vector<Real>,
}
impl World {
    fn new() -> Self {
        Self {
            pipeline: PhysicsPipeline::new(),
            islands: IslandManager::new(),
            broad: BroadPhaseBvh::new(),
            narrow: NarrowPhase::new(),
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            ij: ImpulseJointSet::new(),
            mj: MultibodyJointSet::new(),
            ccd: CCDSolver::new(),
            params: IntegrationParameters::default(),
            gravity: vector![Real::from(0.0), Real::from(-9.81), Real::from(0.0)],
        }
    }
    fn step(&mut self) -> StepJournal {
        let mut j = StepJournal::default();
        self.pipeline.step_journaled(
            &self.gravity, &self.params, &mut self.islands, &mut self.broad,
            &mut self.narrow, &mut self.bodies, &mut self.colliders,
            &mut self.ij, &mut self.mj, &mut self.ccd, &(), &(), &mut j,
        );
        j
    }
    fn revert(&mut self, j: StepJournal) {
        j.revert(&mut self.islands, &mut self.broad, &mut self.narrow,
                 &mut self.bodies, &mut self.colliders, &mut self.ij, &mut self.mj);
    }
    // Task 3 asserts only the structures Task 3 covers; Task 5 upgrades
    // callers to hash_full (adds narrow), Task 4 adds broad.
    fn hash_dynamics(&self) -> (u32, u32, u32) {
        (h(&self.bodies), h(&self.colliders), h(&self.islands))
    }
}

#[test]
fn free_fall_revert_is_hash_exact() {
    let mut w = World::new();
    // No colliders at all: broad/narrow untouched; exercises integration,
    // island activation, force/mprops/sleep bookkeeping, modified lists.
    w.bodies.insert(
        RigidBodyBuilder::dynamic()
            .translation(vector![Real::from(0.0), Real::from(10.0), Real::from(0.0)])
            .build(),
    );
    let before = w.hash_dynamics();
    let journals: Vec<StepJournal> = (0..10).map(|_| w.step()).collect();
    for j in journals.into_iter().rev() {
        w.revert(j);
    }
    assert_eq!(before, w.hash_dynamics(), "free-fall 10-step LIFO revert must be exact");
}

#[test]
fn active_parent_stale_flags_revert_is_hash_exact() {
    // Regression: after any step, colliders of active bodies keep stale
    // MODIFIED|POSITION `changes` (the last-substep branch clears the modified
    // LIST but not the flags). On the next tick they are NOT in
    // `modified_colliders` (push_once no-ops on the stale flag), yet the
    // full-sweep `clear_modified_colliders` wipes their `changes` — a
    // hash-visible mutation the journal must capture.
    let mut w = World::new();
    let b = w.bodies.insert(
        RigidBodyBuilder::dynamic()
            .translation(vector![Real::from(0.0), Real::from(10.0), Real::from(0.0)])
            .build(),
    );
    let c = w.colliders.insert_with_parent(
        ColliderBuilder::ball(Real::from(0.5)).build(),
        b,
        &mut w.bodies,
    );
    // Settle one tick; the body stays active so `c` carries stale flags now.
    let _ = w.step();
    // Edit through the tracking path (no-op push: flag already set).
    w.colliders
        .get_mut(c)
        .unwrap()
        .set_density(Real::from(2.0));
    let before = w.hash_dynamics();
    let j = w.step();
    w.revert(j);
    assert_eq!(
        before,
        w.hash_dynamics(),
        "stale-flag collider tick revert must be exact"
    );
}

#[test]
fn parented_collider_edit_revert_is_hash_exact() {
    // Regression: a user edit that sets SHAPE/MASS/ENABLED/PARENT changes on an
    // attached collider makes `handle_user_changes_to_colliders` push the parent
    // body into the hashed `modified_bodies` list DURING the step. The journal
    // must save the pre-tick list, not the contaminated take_modified() result.
    // A FIXED parent keeps the collider's flags clean between steps, so the
    // edit really lands in `modified_colliders` and reaches the parent-push.
    let mut w = World::new();
    let b = w.bodies.insert(RigidBodyBuilder::fixed().build());
    let c = w.colliders.insert_with_parent(
        ColliderBuilder::ball(Real::from(0.5)).build(),
        b,
        &mut w.bodies,
    );
    // Settle one tick so the insertion-tick modified lists are drained.
    let _ = w.step();
    // User edit through the modification-tracking path: set_density flags
    // LOCAL_MASS_PROPERTIES, which triggers the parent-body push at step entry.
    w.colliders
        .get_mut(c)
        .unwrap()
        .set_density(Real::from(2.0));
    let before = w.hash_dynamics();
    let j = w.step();
    w.revert(j);
    assert_eq!(
        before,
        w.hash_dynamics(),
        "parented-collider edit tick revert must be exact"
    );
}

#[test]
fn sleep_transition_revert_is_hash_exact() {
    let mut w = World::new();
    w.bodies.insert(
        RigidBodyBuilder::dynamic()
            .translation(vector![Real::from(0.0), Real::from(0.0), Real::from(0.0)])
            .linvel(vector![Real::from(0.0), Real::from(0.0), Real::from(0.0)])
            .gravity_scale(Real::from(0.0)) // no colliders; zero-vel body falls asleep
            .build(),
    );
    let before = w.hash_dynamics();
    // Enough ticks to cross the sleep threshold (activation commits vels=0 + sleep()).
    let journals: Vec<StepJournal> = (0..120).map(|_| w.step()).collect();
    for j in journals.into_iter().rev() { w.revert(j); }
    assert_eq!(before, w.hash_dynamics());
}
