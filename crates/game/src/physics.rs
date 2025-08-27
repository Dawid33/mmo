use log::info;
use rapier3d::prelude::*;

use crate::transaction::undo;

use crate::{Controller, GameDataTransaction};

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PhysicsState {
    pub bodies: RigidBodySet,
    pub broad_phase: DefaultBroadPhase,
    pub implules_joint_set: ImpulseJointSet,
    pub multi_body_joint_set: MultibodyJointSet,
    pub ccd_solver: CCDSolver,
    pub colliders: ColliderSet,
    pub gravity: Vector<f32>,
    pub integration_parameters: IntegrationParameters,
    pub islands: IslandManager,
    pub narrow_phase: NarrowPhase,
    pub query_pipeline: QueryPipeline,
}

impl PhysicsState {
    pub fn new() -> Self {
        Self {
            bodies: RigidBodySet::new(),
            broad_phase: BroadPhaseMultiSap::new(),
            ccd_solver: CCDSolver::new(),
            colliders: ColliderSet::new(),
            gravity: vector![0.0, -9.81, 0.0],
            integration_parameters: IntegrationParameters::default(),
            islands: IslandManager::new(),
            narrow_phase: NarrowPhase::new(),
            query_pipeline: QueryPipeline::new(),
            implules_joint_set: ImpulseJointSet::new(),
            multi_body_joint_set: MultibodyJointSet::new(),
        }
    }
}

#[derive(Default)]
pub struct PhysicsController {
    pipeline: PhysicsPipeline,
}

impl Controller for PhysicsController {
    fn on_tick<'a>(&mut self, t: &mut crate::GameDataTransaction) {
        t.update_physics(&mut self.pipeline);
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
    pub fn update_physics(&mut self, pipeline: &mut PhysicsPipeline) {
        let old = self.data.physics.clone();
        pipeline.step(
            &self.data.physics.gravity,
            &self.data.physics.integration_parameters,
            &mut self.data.physics.islands,
            &mut self.data.physics.broad_phase,
            &mut self.data.physics.narrow_phase,
            &mut self.data.physics.bodies,
            &mut self.data.physics.colliders,
            &mut self.data.physics.implules_joint_set,
            &mut self.data.physics.multi_body_joint_set,
            &mut self.data.physics.ccd_solver,
            Some(&mut self.data.physics.query_pipeline),
            &(),
            &(),
        );

        for handle in self.data.physics.islands.active_kinematic_bodies() {
            let b = self.data.physics.bodies.get(*handle).unwrap();
            if b.is_moving() {
                info!("{:?}", b.linvel());
            }
        }
        undo!(self, move |data| {
            data.physics = old.clone();
        });
    }
}
