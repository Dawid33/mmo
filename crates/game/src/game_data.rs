use std::collections::BTreeMap;

use log::info;
use parley::{Alignment, AlignmentOptions, FontContext, Layout, LayoutContext, StyleProperty};
use rapier3d::{na::Vector3, prelude::*};
use serde::{Deserialize, Serialize};

use crate::{EntityId, Tick};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntityType {
    Layout,
    Text { content: String },
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

#[derive(Clone, Serialize, Deserialize)]
pub enum VoxelKind {
    Empty,
}

impl Default for VoxelKind {
    fn default() -> Self {
        VoxelKind::Empty
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Voxel {
    kind: VoxelKind,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Chunk {
    voxels: [[[Voxel; 16]; 16]; 16],
}

/// A representation of a regions game state with the required booking to peform
/// delta-based rollback.
#[derive(Clone, Serialize, Deserialize)]
pub struct GameData {
    pub tick: Tick,
    pub world: Chunk,
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
            world: Chunk::default(),
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

pub fn text_layout<'a>(fctx: &mut FontContext, text: &str) -> Layout<()> {
    let mut l: LayoutContext<()> = LayoutContext::new();
    let mut l = l.ranged_builder(fctx, &text, 1.0, true);
    l.push_default(StyleProperty::FontSize(16.0));
    let mut layout = l.build(text);
    layout.align(None, Alignment::Start, AlignmentOptions::default());
    layout.break_all_lines(Some(10000.0));
    layout
}

pub struct RegionData {
    pub data: GameData,
    pub font_context: FontContext,
    pub text_layouts: BTreeMap<EntityId, Layout<()>>,
}

impl RegionData {
    pub fn new(data: GameData, mut font_context: FontContext) -> Self {
        let mut text_layouts = BTreeMap::new();
        for (i, e) in data.entities.iter().enumerate() {
            match &e.kind {
                EntityType::Text { content } => {
                    info!("layout of {:?}", content);
                    let l = text_layout(&mut font_context, &content);
                    info!("layout w: {:?}", l.width());
                    info!("layout h: {:?}", l.height());
                    text_layouts.insert(i, l);
                }
                _ => (),
            }
        }
        Self {
            text_layouts,
            font_context,
            data,
        }
    }
}
