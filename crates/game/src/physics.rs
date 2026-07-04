
use rapier3d::prelude::*;
use slotmapd::KeyData;

use crate::{Controller, EntityKey, GameDataUpdate, Undo};

#[derive(Default)]
pub struct PhysicsController {
    pipeline: PhysicsPipeline,
}

impl Controller for PhysicsController {
    fn on_tick<'a>(&mut self, data: &mut Undo<crate::GameData>) {
        // snapshot_raw: whole-PhysicsState snapshot into the log (the Phase-4
        // resolution for opaque step() mutations) + raw access to every field.
        let p = data.physics.snapshot_raw();
        self.pipeline.step(
            p.gravity,
            p.integration_parameters,
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
        );

        for handle in data.physics.islands.active_bodies() {
            let b = data.physics.bodies.get(*handle).unwrap();
            let e_id = EntityKey::from(KeyData::from_ffi(b.user_data as u64));
            if let Some(_c) = data.ecs.camera.try_get(e_id) {
                data.send(GameDataUpdate::new(
                    crate::GameDataTransactionKind::Do,
                    crate::GameDataUpdateKind::UpdateCameraViewMatrix(e_id, *b.position()),
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
