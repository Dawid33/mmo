//! Issues:
//! Re-do this whole thing by applying the attribute to a module and
//! putting all rollbackable structs inside that module.
//! That would make it possible to not need to do any recursion for rollback or
//! forget, also eliminating the edge case of setting a rollback implementor who
//! is inside an undo

extern crate nalgebra as na;

use block_mesh::{MergeVoxel, VoxelVisibility};
use borrow::Partial;
use crossbeam::channel::Sender;
use log::info;
use nalgebra::{
    Complex, ComplexField, Isometry3, Matrix4, OPoint, Perspective3, Point3, Quaternion, RealField,
    Rotation, Rotation3, Translation3, Unit, Vector3, Vector4,
};
use parry3d::math::Real;
use rapier3d::math::Vector;
use rapier3d::prelude::{
    CCDSolver, ColliderSet, DefaultBroadPhase, ImpulseJointSet, IntegrationParameters,
    IslandManager, LockedAxes, MultibodyJointSet, NarrowPhase, QueryPipeline, RigidBodyBuilder,
    RigidBodyHandle, RigidBodySet,
};
use slotmapd::secondary::Iter;
use slotmapd::{DefaultKey, Key, KeyData, SecondaryMap, SlotMap, SparseSecondaryMap, new_key_type};
use std::sync::{Arc, atomic::AtomicUsize};
use winit::keyboard::KeyCode;

pub use derive_more::Debug;
pub use macros::rollback;
pub use serde;

pub const CHUNK_VOXEL_COUNT: usize = 32 * 32 * 32;

pub type IsometryReal = na::Isometry<Real, na::Unit<na::Quaternion<Real>>, 3>;

new_key_type! { pub struct EntityKey; }
new_key_type! { pub struct PlayerKey; }

#[derive(Clone, Debug, Default)]
pub struct RollbackInfo {
    pub current: Arc<AtomicUsize>,
    pub oldest: Arc<AtomicUsize>,
}

impl RollbackInfo {
    pub fn new() -> Self {
        Self {
            current: Arc::new(AtomicUsize::new(0)),
            oldest: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[derive(Clone, Debug)]
pub enum GameDataUpdateKind {
    // CreateUIElement(DefaultKey, UIElement, IsometryReal),
    // SetUIElementStyle(DefaultKey, Style),
    SetUIElementContent(DefaultKey, Option<String>),
    RemoveUIElement(DefaultKey),
    SetVoxelComponent(EntityKey, Option<Vec<Voxel>>),
    SetEntityPosition(EntityKey, IsometryReal),
    UpdateCameraViewProj(EntityKey, Perspective3<Real>),
    UpdateCameraViewMatrix(EntityKey, IsometryReal),
    CreateEntity(EntityKey),
    RemoveEntity(EntityKey),
    SetFreeCam(EntityKey, bool),
}

#[derive(Clone, Debug)]
pub struct GameDataUpdate {
    pub do_kind: GameDataTransactionKind,
    pub update_kind: GameDataUpdateKind,
}

impl GameDataUpdate {
    pub fn new(do_kind: GameDataTransactionKind, update_kind: GameDataUpdateKind) -> Self {
        Self {
            do_kind,
            update_kind,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum GameDataTransactionKind {
    Do,
    Undo,
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Copy, Clone, Hash)]
pub struct Voxel {
    pub kind: VoxelType,
}

impl Voxel {
    pub fn new(kind: VoxelType) -> Self {
        Self { kind }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Copy, Clone, PartialEq, Eq, Hash)]
pub enum VoxelType {
    Black,
    Air,
}

impl Default for VoxelType {
    fn default() -> Self {
        VoxelType::Air
    }
}

impl block_mesh::Voxel for Voxel {
    fn get_visibility(&self) -> VoxelVisibility {
        if self.kind == VoxelType::Air {
            VoxelVisibility::Empty
        } else {
            VoxelVisibility::Opaque
        }
    }
}

impl MergeVoxel for Voxel {
    type MergeValue = VoxelType;
    type MergeValueFacingNeighbour = VoxelType;

    fn merge_value(&self) -> Self::MergeValue {
        self.kind
    }

    fn merge_value_facing_neighbour(&self) -> Self::MergeValueFacingNeighbour {
        self.kind
    }
}

// #[rollback(GameData)]
// mod game_data {
//     use super::*;
//     use std::ops::Deref;

//     pub struct GameData {
//         // ecs: Ecs,
//         // physics: PhysicsState,
//         // tick: usize,
//         // players: SlotMap<PlayerKey, EntityKey>,
//     }

// pub struct Ecs {
//     entities: SlotMap<EntityKey, ()>,
//     camera: Component<Camera>,
//     isometry: Component<IsometryReal>,
//     rigidbody: Component<RigidBodyHandle>,
//     // player: Component<Player>,
//     chunk: Component<Chunk>,
// }

// pub struct PhysicsState {
//     bodies: RigidBodySet,
//     broad_phase: DefaultBroadPhase,
//     implules_joint_set: ImpulseJointSet,
//     multi_body_joint_set: MultibodyJointSet,
//     ccd_solver: CCDSolver,
//     colliders: ColliderSet,
//     gravity: Vector<Real>,
//     integration_parameters: IntegrationParameters,
//     islands: IslandManager,
//     narrow_phase: NarrowPhase,
// }
// }
