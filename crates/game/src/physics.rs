use rapier3d::prelude::*;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct PhysicsState {
    bodies: RigidBodySet,
    broad_phase: DefaultBroadPhase,
    ccd_solver: CCDSolver,
    colliders: ColliderSet,
    gravity: Vector<f32>,
    integration_parameters: IntegrationParameters,
    islands: IslandManager,
    narrow_phase: NarrowPhase,
    query_pipeline: QueryPipeline,
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
        }
    }
}
