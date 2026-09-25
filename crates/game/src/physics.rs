
use rapier3d::prelude::*;
use slotmapd::KeyData;

use crate::{Controller, EntityKey, GameDataUpdate, Undo};

#[derive(Default)]
pub struct PhysicsController {
    pipeline: PhysicsPipeline,
}

impl Controller for PhysicsController {
    fn on_tick<'a>(&mut self, data: &mut Undo<crate::GameData>) {
        // Journaled step: one tier-2 log entry whose payload is the exact
        // per-tick delta (StepJournal), replacing the whole-PhysicsState
        // snapshot. The scope's registration license covers step()'s writes;
        // the undo closure's raw_undo_parts license covers the revert's.
        let mut journal = StepJournal::default();
        let mut scope = data.physics.undo_scope_digest();
        {
            let p = scope.raw_fields();
            self.pipeline.step_journaled(
                &*p.gravity,
                &*p.integration_parameters,
                p.islands,
                p.broad_phase,
                p.narrow_phase,
                p.bodies,
                p.colliders,
                p.implules_joint_set,
                p.multi_body_joint_set,
                p.ccd_solver,
                &(),
                &(),
                &mut journal,
            );
        }
        // Pre-digest only when Delta verification will actually read it (it walks
        // the journal's saved payloads); Full/Off ignore it, so skip the work.
        let pre = if scope.wants_delta_digest() {
            journal.pre_digest()
        } else {
            0
        };
        scope.register(pre, move |phys, _| {
            let p = phys.raw_undo_parts();
            // Reverts the tick AND recomputes the same footprint digest from the
            // restored live structures; Delta compares it against `pre`.
            journal.revert_digest(
                p.islands,
                p.broad_phase,
                p.narrow_phase,
                p.bodies,
                p.colliders,
                p.implules_joint_set,
                p.multi_body_joint_set,
            )
        });

        for handle in data.physics.islands.active_bodies() {
            let b = data.physics.bodies.get(*handle).unwrap();
            let e_id = EntityKey::from(KeyData::from_ffi(b.user_data as u64));
            if let Some(_c) = data.ecs.camera.try_get(e_id) {
                data.send(GameDataUpdate::new(
                    crate::GameDataTransactionKind::Do,
                    crate::GameDataUpdateKind::UpdateCameraViewMatrix(e_id, *b.position()),
                ));
            } else if data.ecs.motion.try_get(e_id).is_some() {
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
