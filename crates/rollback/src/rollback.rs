//! Issues:
//! Re-do this whole thing by applying the attribute to a module and
//! putting all rollbackable structs inside that module.
//! That would make it possible to not need to do any recursion for rollback or
//! forget, also eliminating the edge case of setting a rollback implementor who
//! is inside an undo

extern crate nalgebra as na;

use block_mesh::ndshape::{ConstShape, ConstShape3u32};
use block_mesh::{MergeVoxel, VoxelVisibility};
use borrow::Partial;
use crossbeam::channel::Sender;
use log::info;
use nalgebra::Perspective3;
use nalgebra::{
    ComplexField, Isometry3, Matrix4, OPoint, Point3, Quaternion, RealField, Rotation, Rotation3,
    Translation3, Unit, Vector3, Vector4,
};
use ordered_float::OrderedFloat;
use parry3d::math::{RawReal, Real};
use rapier3d::math::Vector;
use rapier3d::prelude::{
    CCDSolver, ColliderHandle, ColliderSet, DefaultBroadPhase, ImpulseJointSet,
    IntegrationParameters, IslandManager, LockedAxes, MultibodyJointSet, NarrowPhase,
    QueryPipeline, RigidBodyBuilder, RigidBodyHandle, RigidBodySet,
};
use slotmapd::secondary::Iter;
use slotmapd::{DefaultKey, new_key_type};
use slotmapd::{Key, KeyData, SecondaryMap, SlotMap, SparseSecondaryMap};
use std::ops::DerefMut;
use std::sync::{Arc, atomic::AtomicUsize};
// use winit::keyboard::KeyCode;

pub use derive_more::Debug;
pub use game_data::*;
pub use macros::rollback;
pub use serde;

pub mod common;
pub use common::*;
pub mod input;

const HALF_VOXEL_SIZE: f32 = 1.0 / 2.0;
pub type ChunkShape = ConstShape3u32<32, 32, 32>;

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, Hash)]
#[module(crate)]
pub struct Chunk {
    pub voxels: Vec<Voxel>,
    pub collider: Vec<ColliderHandle>,
}

impl Default for Chunk {
    fn default() -> Self {
        let mut voxels = Vec::with_capacity(ChunkShape::SIZE as usize);
        for i in 0..ChunkShape::SIZE {
            let [mut x, mut y, mut z] = ChunkShape::delinearize(i);

            let v = if x > 0 && y > 0 && z > 0 && y < 31 && z < 31 && x < 31 {
                if y == 1 {
                    Voxel::new(VoxelType::Black)
                } else {
                    Voxel::new(VoxelType::Air)
                }
            } else {
                Voxel::new(VoxelType::Air)
            };
            voxels.push(v);
        }

        Self {
            collider: Vec::new(),
            voxels,
        }
    }
}

#[derive(
    Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, PartialOrd, Ord, Eq,
)]
pub struct ChunkCoords {
    x: usize,
    y: usize,
    z: usize,
}

impl ChunkCoords {
    pub fn new(x: usize, y: usize, z: usize) -> Self {
        Self { x, y, z }
    }
}

pub const CHUNK_VOXEL_COUNT: usize = 32 * 32 * 32;

pub type IsometryReal = na::Isometry<Real, na::Unit<na::Quaternion<Real>>, 3>;

pub type ClientId = usize;

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
    SetVoxelComponent(EntityKey, Option<Vec<Voxel>>),
    SetEntityPosition(EntityKey, IsometryReal),
    AddCameraComponent(EntityKey, Perspective3<Real>, IsometryReal),
    RemoveCameraComponent(EntityKey),
    UpdateCameraViewProj(EntityKey, Perspective3<Real>),
    UpdateCameraViewMatrix(EntityKey, IsometryReal),
    CreateEntity(EntityKey),
    RemoveEntity(EntityKey),
    SetFreeCam(ClientId, bool),
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

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, Partial, Hash)]
#[module(crate)]
pub struct Camera {
    pub opengl_to_wgpu_matrix: Matrix4<Real>,
    pub proj_matrix: Perspective3<Real>,
    pub view_matrix: Option<RigidBodyHandle>,
}

pub const ASPECT: f32 = (16 / 9) as f32;

impl Camera {
    pub fn new(handle: RigidBodyHandle) -> Self {
        let m = Matrix4::from_columns(&[
            Vector4::new(
                OrderedFloat(1.0),
                OrderedFloat(0.0),
                OrderedFloat(0.0),
                OrderedFloat(0.0),
            ),
            Vector4::new(
                OrderedFloat(0.0),
                OrderedFloat(1.0),
                OrderedFloat(0.0),
                OrderedFloat(0.0),
            ),
            Vector4::new(
                OrderedFloat(0.0),
                OrderedFloat(0.0),
                OrderedFloat(0.5),
                OrderedFloat(0.0),
            ),
            Vector4::new(
                OrderedFloat(0.0),
                OrderedFloat(0.0),
                OrderedFloat(0.5),
                OrderedFloat(1.0),
            ),
        ]);
        Camera {
            proj_matrix: Perspective3::from_matrix_unchecked(
                Perspective3::new(
                    OrderedFloat(ASPECT),
                    OrderedFloat(90.0),
                    OrderedFloat(0.1),
                    OrderedFloat(100.0),
                )
                .as_matrix()
                    * m,
            ),
            opengl_to_wgpu_matrix: m,
            view_matrix: Some(handle),
        }
    }
}

impl Camera {
    pub fn build_projection(&self) -> Matrix4<crate::Real> {
        *self.proj_matrix.as_matrix()
    }
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            opengl_to_wgpu_matrix: Default::default(),
            proj_matrix: Perspective3::new(
                OrderedFloat(ASPECT),
                OrderedFloat(90.0),
                OrderedFloat(0.1),
                OrderedFloat(100.0),
            ),
            view_matrix: Default::default(),
        }
    }
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, Hash)]
#[module(crate)]
pub struct Client {
    pub input: input::WinitInput,
    pub fps_cam_mode: bool,
}

#[rollback(GameData)]
mod game_data {
    use parry3d::math::{Real, Vector};
    use rapier3d::prelude::{
        CCDSolver, ColliderSet, DefaultBroadPhase, ImpulseJointSet, IntegrationParameters,
        IslandManager, MultibodyJointSet, NarrowPhase, RigidBodySet,
    };

    use super::{
        Camera, Chunk, Client, ClientId, Component, EntityKey, IsometryReal, RigidBodyHandle,
        RollbackInfo, SlotMap,
    };
    use std::collections::BTreeMap;

    pub struct GameData {
        ecs: Ecs,
        physics: PhysicsState,
        #[undo(cell)]
        tick: usize,
        #[undo(cell)]
        next_game_event_id: usize,
        #[undo(map)]
        player_entites: BTreeMap<ClientId, EntityKey>,
        #[undo(map)]
        clients: BTreeMap<ClientId, Client>,
    }

    pub struct Ecs {
        #[undo(slotmap)]
        #[emit(insert = CreateEntity, remove = RemoveEntity)]
        entities: SlotMap<EntityKey, ()>,
        camera: Component<Camera>,
        isometry: Component<IsometryReal>,
        rigidbody: Component<RigidBodyHandle>,
        chunk: Component<Chunk>,
    }

    pub struct PhysicsState {
        bodies: RigidBodySet,
        broad_phase: DefaultBroadPhase,
        implules_joint_set: ImpulseJointSet,
        multi_body_joint_set: MultibodyJointSet,
        ccd_solver: CCDSolver,
        colliders: ColliderSet,
        gravity: Vector<Real>,
        integration_parameters: IntegrationParameters,
        islands: IslandManager,
        narrow_phase: NarrowPhase,
    }
}

impl GameData {
    pub fn into_game_update_iter(self) {}
}

impl Undo<Ecs> {
    pub fn create_entity_safe(&mut self) -> EntityKey {
        // CreateEntity / RemoveEntity emits ride the entities delta in both
        // directions — see #[emit] on the field.
        let key = self.entities.insert(());
        self.camera.insert_safe(key);
        self.isometry.insert_safe(key);
        self.rigidbody.insert_safe(key);
        self.chunk.insert_safe(key);
        key
    }
}

#[derive(Default, Debug, Clone, serde::Serialize, serde::Deserialize, Hash)]
pub struct Component<T>
where
    T: 'static + Default,
{
    list: SparseSecondaryMap<EntityKey, Option<T>>,
}

impl<T> Undo<Component<T>>
where
    T: 'static
        + Default
        + Clone
        + Send
        + std::hash::Hash
        + ::serde::Serialize
        + for<'a> ::serde::Deserialize<'a>,
{
    /// Creates the (empty) component entry for a new entity, undo-safely.
    /// SparseSecondaryMap remove(insert) is hash-exact (see hash_restore.rs),
    /// so a removal closure is a true inverse here.
    pub fn insert_safe(&mut self, key: EntityKey) {
        self.undo(move |d, _| {
            d.list.remove(key);
        });
        self.list.insert(key, None);
    }

    pub fn set_safe(&mut self, key: EntityKey, item: Option<T>) {
        let old = self.list.get(key).cloned().unwrap();
        self.undo(move |d, _| *d.list.get_mut(key).unwrap() = old.clone());

        if let Some(item) = item {
            self.list.get_mut(key).unwrap().replace(item);
        } else {
            self.list.get_mut(key).unwrap().take();
        }
    }

}

impl<T> Component<T>
where
    T: 'static + Default + Clone + Send,
{
    pub fn iter(&self) -> slotmapd::sparse_secondary::Iter<'_, EntityKey, Option<T>> {
        self.list.iter()
    }
    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn insert(&mut self, e: EntityKey, item: Option<T>) {
        self.list.insert(e, item);
    }

    pub fn remove(&mut self, item: EntityKey) -> Option<T> {
        self.list.remove(item).unwrap()
    }

    pub fn get(&self, key: EntityKey) -> &T {
        self.list.get(key).unwrap().as_ref().unwrap()
    }

    pub fn try_get(&self, key: EntityKey) -> &Option<T> {
        self.list.get(key).unwrap()
    }

    pub fn get_mut(&mut self, key: EntityKey) -> &mut T {
        self.list.get_mut(key).unwrap().as_mut().unwrap()
    }
}

impl Rollback {
    pub fn create_mesh(&mut self, coords: ChunkCoords) -> EntityKey {
        let e = self.ecs.create_entity_safe();
        let chunk = Chunk::default();
        self.ecs.chunk.set_safe(e, Some(chunk));
        let position = IsometryReal::from_parts(
            Translation3::new(
                Real::from((coords.x * 32) as RawReal),
                Real::from((coords.y * 32) as RawReal),
                Real::from((coords.z * 32) as RawReal),
            ),
            Unit::<Quaternion<Real>>::identity(),
        );
        let body = RigidBodyBuilder::fixed()
            .pose(position)
            .user_data(e.data().as_ffi() as u128)
            .build();
        // The rigid-body arena hashes generation counters and the free list,
        // so removing the inserted body can't restore the pre-insert state.
        // change() snapshots the whole PhysicsState and restores it on undo.
        let handle = self.data.physics.change().bodies.insert(body);
        self.ecs.rigidbody.set_safe(e, Some(handle));
        e
    }

    pub fn create_player_safe(&mut self, client_id: ClientId) {
        let e = self.ecs.create_entity_safe();
        let position = IsometryReal::from_parts(
            Translation3::new(Real::from(0.0), Real::from(1.0), Real::from(5.0)),
            Unit::<Quaternion<Real>>::identity(),
        );
        let body = RigidBodyBuilder::kinematic_position_based()
            .pose(position)
            .gravity_scale(Real::from(0.0))
            .enabled_rotations(true, true, true)
            .ccd_enabled(false)
            .angular_damping(Real::from(1.0))
            .can_sleep(false)
            .user_data(e.data().as_ffi() as u128)
            .build();
        // Snapshot-restore for the same reason as in create_mesh: removal
        // doesn't bring the arena back to its pre-insert state.
        let handle = self.data.physics.change().bodies.insert(body);
        self.data.ecs.rigidbody.set_safe(e, Some(handle));
        let cam = Camera::new(handle);
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::AddCameraComponent(e, cam.proj_matrix, position),
        ));
        // LIFO: registered before set_safe, so the notification fires after
        // the camera value is restored.
        self.data.ecs.camera.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::RemoveCameraComponent(e),
        ));
        self.data.ecs.camera.set_safe(e, Some(cam));
        self.data.player_entites.insert(client_id, e);
    }
}
