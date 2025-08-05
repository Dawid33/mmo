use cosmic_text::Buffer;
use rapier3d::{na::Vector3, prelude::*};
use serde::{Deserialize, Serialize};

use crate::Tick;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntityType {
    TaffyTree,
    Text(String),
    Default,
}

impl Default for EntityType {
    fn default() -> Self {
        Self::Default
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Entity {
    pub kind: EntityType,
    pub position: Vector3<f32>,
}

/// A representation of a regions game state with the required booking to peform
/// delta-based rollback.
#[derive(Clone, Serialize, Deserialize)]
pub struct GameData {
    pub tick: Tick,
    pub entities: Vec<Entity>,
    pub physics: PhysicsState,
}

#[derive(Clone, Serialize, Deserialize)]
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

impl GameData {
    pub fn new() -> Self {
        Self {
            tick: 0,
            physics: PhysicsState::new(),
            entities: Vec::new(),
        }
    }
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
