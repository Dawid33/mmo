use std::ops::{Deref, DerefMut};

use crate::na::Vector3;
use borrow::PartialHelper;
use crossbeam::channel::Sender;
use log::info;
use rapier3d::prelude::*;
use slotmapd::KeyData;
use winit::dpi::PhysicalInsets;

use crate::data::{PhysicsState, Undo};
use rollback::EntityKey;

use crate::{ClientUpdateEvent, Controller, GameData, GameDataUpdate};

#[derive(Default)]
pub struct PhysicsController {
    pipeline: PhysicsPipeline,
}

impl Controller for PhysicsController {
    fn on_tick<'a>(&mut self, data: &mut Undo<crate::GameData>) {
        let physics = data.physics.change().as_refs_mut();
        self.pipeline.step(
            physics.gravity,
            physics.integration_parameters,
            physics.islands,
            physics.broad_phase,
            physics.narrow_phase,
            physics.bodies,
            physics.colliders,
            physics.implules_joint_set,
            physics.multi_body_joint_set,
            physics.ccd_solver,
            &(),
            &(),
        );

        for handle in data.physics.islands.active_bodies() {
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
