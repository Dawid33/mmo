use std::ops::DerefMut;

use borrow::PartialHelper;
use log::info;
use rapier3d::prelude::*;
use winit::dpi::PhysicalInsets;

use crate::transaction::undo;

use crate::{Controller, GameDataTransaction};

#[derive(Default)]
pub struct PhysicsController {
    pipeline: PhysicsPipeline,
}

impl Controller for PhysicsController {
    fn on_tick<'a>(&mut self, data: &mut crate::GameData) {
        let old = data.physics.deref_mut().clone();
        data.physics.undo(move |d| *d = old.clone());

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
            if b.is_moving() {
                info!("is_moving {:?}", b.linvel());
            }
        }
    }
}

impl PhysicsController {
    pub fn new() -> Box<Self> {
        Box::new(Self {
            pipeline: PhysicsPipeline::new(),
        })
    }
}

impl<'a> GameDataTransaction<'a> {
    pub fn update_physics(&mut self, pipeline: &mut PhysicsPipeline) {}
}
