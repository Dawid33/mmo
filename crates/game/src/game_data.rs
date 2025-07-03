use std::collections::{BinaryHeap, VecDeque};

use bones_ecs::World;
use log::info;
use rapier3d::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{EventId, GameEvent, Player, Tick};

/// A representation of a regions game state with the required booking to peform
/// delta-based rollback.
pub struct GameData {
    tick: Tick,
    last_game_event_id: EventId,
    physics: PhysicsState,
    world: World,
    rollback_log: VecDeque<GameEventRollback>,
    event_log: VecDeque<(EventId, GameEvent)>,
}

// Use serde derives for our wrapper struct.
#[derive(Serialize, Deserialize)]
struct PhysicsState {
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

/// A
pub struct Transaction<'a> {
    data: &'a mut GameData,
}

impl<'a> Drop for Transaction<'a> {
    fn drop(&mut self) {}
}

struct GameEventRollback {
    game_event_id: usize,
    rollback: Rollback,
}

enum Rollback {
    AddEntity { id: usize },
}

impl GameData {
    /// Create a new game data instance.
    pub fn new() -> Self {
        Self {
            tick: 0,
            last_game_event_id: 0,
            physics: PhysicsState::new(),
            rollback_log: VecDeque::new(),
            world: World::new(),
            event_log: VecDeque::new(),
        }
    }

    /// Create a new transaction per game event.
    pub fn transaction(&mut self, event: GameEvent) -> Transaction {
        self.event_log.push_back((self.last_game_event_id, event));
        self.last_game_event_id += 1;
        Transaction { data: self }
    }
}

impl<'a> Transaction<'a> {
    pub fn log(&mut self, rollback: Rollback) {
        self.data.rollback_log.push_back(GameEventRollback {
            rollback,
            game_event_id: self.data.last_game_event_id - 1,
        });
    }
    pub fn step_physics(&mut self) -> Result<(), String> {
        Ok(())
    }
    pub fn spawn_player(&mut self, entity: Player) -> Result<(), String> {
        self.log(Rollback::AddEntity { id: 0 });
        Ok(())
    }
}

impl Rollback {
    pub fn rollback(&self, data: &mut GameData) -> Result<(), String> {
        match self {
            Rollback::AddEntity { id } => todo!(),
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
