use std::ops::{Deref, DerefMut};

use borrow::PartialHelper;
use crossbeam::channel::Sender;
use log::info;
use rapier3d::na::Vector3;
use rapier3d::prelude::*;
use slotmapd::KeyData;
use winit::dpi::PhysicalInsets;

use crate::data::{EntityKey, PhysicsState, Undo};
use crate::transaction::undo;

use crate::{ClientUpdateEvent, Controller, GameData, GameDataTransaction, GameDataUpdate};

#[derive(Default)]
pub struct PhysicsController {
    pipeline: PhysicsPipeline,
}

// impl GameData {
//     pub fn update_physics_client(&self) {
//         for handle in self.physics.islands.active_kinematic_bodies() {
//             let b = self.physics.bodies.get(*handle).unwrap();
//             let e_id = EntityKey::from(KeyData::from_ffi(b.user_data as u64));
//             if let Some(c) = self.ecs.camera.try_get(e_id) {
//                 self.ecs.send(GameDataUpdate::new(
//                     crate::GameDataTransactionKind::Undo,
//                     crate::GameDataUpdateKind::UpdateCameraViewMatrix(e_id, *b.position()),
//                 ));
//             }
//         }
//     }
// }

impl Controller for PhysicsController {
    fn on_tick<'a>(&mut self, data: &mut Undo<crate::GameData>) {
        let old = data.physics.deref().clone();
        data.physics.undo(move |d, _| {
            *d = old.clone();
        });

        let physics = data.physics.as_refs_mut();
        self.pipeline.step(
            physics.gravity,
            physics.integration_parameters,
            physics.islands,
            physics.broad_phase.deref_mut(),
            physics.narrow_phase,
            physics.bodies,
            physics.colliders,
            physics.implules_joint_set,
            physics.multi_body_joint_set,
            physics.ccd_solver,
            Some(physics.query_pipeline),
            &(),
            &(),
        );

        for handle in data.physics.islands.active_kinematic_bodies() {
            let b = data.physics.bodies.get(*handle).unwrap();
            let e_id = EntityKey::from(KeyData::from_ffi(b.user_data as u64));
            if let Some(c) = data.ecs.camera.try_get(e_id) {
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
