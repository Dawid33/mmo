
use rapier3d::prelude::*;
use slotmapd::KeyData;

use crate::{Controller, EntityKey, GameDataUpdate, Undo};

#[derive(Default)]
pub struct PhysicsController {
    pipeline: PhysicsPipeline,
}

impl Controller for PhysicsController {
    fn on_tick<'a>(&mut self, data: &mut Undo<crate::GameData>) {
        // Journaled step: one tier-2 log entry whose payload is the tick's
        // hybrid journal (StepJournal: first-touch body/collider captures plus
        // wholesale clones of the islands, narrow and broad phase), replacing
        // the whole-PhysicsState snapshot. The scope's registration license covers step()'s writes;
        // the undo closure's raw_undo_parts license covers the revert's.
        let mut journal = StepJournal::default();
        let mut scope = data.physics.undo_scope_digest();
        {
            let p = scope.raw_fields();
            self.pipeline.step_journaled(
                *p.gravity,
                &*p.integration_parameters,
                p.islands,
                p.broad_phase,
                p.narrow_phase,
                p.bodies,
                p.colliders,
                p.implules_joint_set,
                p.multi_body_joint_set,
                p.soft_bodies,
                p.ccd_solver,
                &(),
                &(),
                Some(&mut journal),
            );
        }
        // Pre-digest only when Delta verification will actually read it; Full
        // and Off ignore it, so skip the work. It hashes the bodies/colliders
        // the step captured plus the joint sets when they were not cloned
        // (quiescent) — NOT the islands/narrow/broad phase, which the revert
        // moves back wholesale from its clones (H8 review I1).
        let delta = scope.wants_delta_digest();
        let pre = if delta { journal.pre_digest() } else { 0 };
        scope.register(pre, move |phys, _| {
            let p = phys.raw_undo_parts();
            // No pipeline/CCD call after the revert: it bumps the islands'
            // `journal_reverts`, which makes the same PhysicsPipeline drop its
            // memos on its next step, and it discards the CCD solver's
            // fixed-targets cache itself (plan Task 4.3).
            let parts = PhysicsParts {
                islands: p.islands,
                broad_phase: p.broad_phase,
                narrow_phase: p.narrow_phase,
                bodies: p.bodies,
                colliders: p.colliders,
                impulse_joints: p.implules_joint_set,
                multibody_joints: p.multi_body_joint_set,
                soft_bodies: p.soft_bodies,
                ccd_solver: p.ccd_solver,
            };
            if !delta {
                // Full/Off never read the returned digest.
                journal.revert(parts);
                return 0;
            }
            // Reverts the tick AND recomputes the same footprint digest from the
            // restored live structures; Delta compares it against `pre`.
            journal.revert_digest(parts)
        });

        for handle in data.physics.islands.active_bodies() {
            let b = data.physics.bodies.get(handle).unwrap();
            let e_id = EntityKey::from(KeyData::from_ffi(b.user_data as u64));
            if let Some(_c) = data.ecs.camera.try_get(e_id) {
                data.send(GameDataUpdate::new(
                    crate::GameDataTransactionKind::Do,
                    crate::GameDataUpdateKind::UpdateCameraViewMatrix(e_id, *b.position()),
                ));
            } else if data.ecs.motion.try_get(e_id).is_some() || data.ecs.voxel_body.try_get(e_id).is_some() {
                // Camera-less movers (mobs): their pose reaches the renderer
                // only through this.
                data.send(GameDataUpdate::new(
                    crate::GameDataTransactionKind::Do,
                    crate::GameDataUpdateKind::SetEntityPosition(e_id, *b.position()),
                ));
            }
        }
    }
}

impl PhysicsController {
    pub fn new() -> Box<dyn Controller> {
        Box::new(Self {
            pipeline: PhysicsPipeline::new(),
        })
    }
}
