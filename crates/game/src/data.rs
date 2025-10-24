use std::collections::BTreeSet;
use std::ops::{BitAndAssign, DerefMut};

use crate::input::BevyInput;
use crate::mesh::{Chunk, VoxelType};
use crate::taffy::style::BlockItemStyle;
use crate::taffy::TaffyTree;
use crate::{ChunkMesh, ClientUpdateEvent};
use crate::{GameDataTransactionKind, GameDataUpdate, GameError, IsometryReal, RegionId, Usize};
use borrow::Partial;
use crossbeam::channel::Sender;
pub use game_data::*;
use log::info;
use rapier3d::math::Vector;
use rapier3d::na::{
    Complex, ComplexField, Isometry3, Matrix4, OPoint, Perspective3, Point3, Quaternion, RealField,
    Rotation, Rotation3, Translation3, Unit, Vector3, Vector4,
};
use rapier3d::prelude::{
    CCDSolver, ColliderSet, DefaultBroadPhase, ImpulseJointSet, IntegrationParameters,
    IslandManager, LockedAxes, MultibodyJointSet, NarrowPhase, QueryPipeline, RigidBodyBuilder,
    RigidBodyHandle, RigidBodySet,
};
use rollback::rollback;
use slotmapd::secondary::Iter;
use slotmapd::{new_key_type, Key, KeyData, SecondaryMap, SlotMap, SparseSecondaryMap};
use winit::keyboard::KeyCode;

new_key_type! { pub struct EntityKey; }
new_key_type! { pub struct PlayerKey; }

#[derive(Debug, Default, Clone)]
pub struct UIElement {
    style: crate::taffy::Style,
    content: Option<String>,
}

pub const ASPECT: f32 = (16 / 9) as f32;

#[derive(Debug, rollback::serde::Serialize, rollback::serde::Deserialize, Clone, Partial)]
#[module(crate)]
pub struct Camera {
    pub opengl_to_wgpu_matrix: Matrix4<f32>,
    pub proj_matrix: Perspective3<f32>,
    pub view_matrix: Option<RigidBodyHandle>,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            opengl_to_wgpu_matrix: Default::default(),
            proj_matrix: Perspective3::new(ASPECT, 90.0, 0.1, 100.0),
            view_matrix: Default::default(),
        }
    }
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial)]
#[module(crate)]
pub struct Player {
    pub bevy: BevyInput,
    pub fps_cam_mode: bool,
}

#[rollback(GameData)]
mod game_data {
    use super::*;
    use std::ops::Deref;

    pub struct GameData {
        ecs: Ecs,
        physics: PhysicsState,
        tick: usize,
        menu: TaffyTree<()>,
        gui: TaffyTree<()>,
        players: SlotMap<PlayerKey, EntityKey>,
    }

    pub struct Ecs {
        entities: SlotMap<EntityKey, ()>,
        camera: Component<Camera>,
        isometry: Component<IsometryReal>,
        rigidbody: Component<RigidBodyHandle>,
        player: Component<Player>,
        mesh: Component<Chunk>,
    }

    pub struct PhysicsState {
        bodies: RigidBodySet,
        broad_phase: DefaultBroadPhase,
        implules_joint_set: ImpulseJointSet,
        multi_body_joint_set: MultibodyJointSet,
        ccd_solver: CCDSolver,
        colliders: ColliderSet,
        gravity: Vector<f32>,
        integration_parameters: IntegrationParameters,
        islands: IslandManager,
        narrow_phase: NarrowPhase,
        query_pipeline: QueryPipeline,
    }
}

impl Camera {
    fn new(handle: RigidBodyHandle) -> Self {
        let m = Matrix4::from_columns(&[
            Vector4::new(1.0, 0.0, 0.0, 0.0),
            Vector4::new(0.0, 1.0, 0.0, 0.0),
            Vector4::new(0.0, 0.0, 0.5, 0.0),
            Vector4::new(0.0, 0.0, 0.5, 1.0),
        ]);
        Camera {
            proj_matrix: Perspective3::from_matrix_unchecked(
                Perspective3::new(ASPECT, 90.0, 0.1, 100.0).as_matrix() * m,
            ),
            opengl_to_wgpu_matrix: m,
            view_matrix: Some(handle),
        }
    }
}

impl Camera {
    pub fn build_projection(&self) -> Matrix4<f32> {
        *self.proj_matrix.as_matrix()
    }
}

impl Undo<Ecs> {
    pub fn create_entity_safe(&mut self) -> EntityKey {
        let key = self.entities.deref_mut().insert(());
        self.camera.insert(key, None);
        self.isometry.insert(key, None);
        self.rigidbody.insert(key, None);
        self.player.insert(key, None);
        self.mesh.insert(key, None);
        self.undo(move |d, s| {
            d.entities.remove(key);
            d.camera.remove(key);
            d.isometry.remove(key);
            d.rigidbody.remove(key);
            d.player.remove(key);
            s.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                crate::GameDataUpdateKind::RemoveEntity(key),
            ));
        });
        self.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            crate::GameDataUpdateKind::CreateEntity(key),
        ));
        key
    }
}

#[derive(Default, Debug, Clone, serde::Serialize, serde::Deserialize)]
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
        + ::rollback::serde::Serialize
        + for<'a> ::rollback::serde::Deserialize<'a>,
{
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
    pub fn create_mesh(&mut self) -> EntityKey {
        let e = self.ecs.create_entity_safe();

        let mut mesh = Chunk::default();
        for x in &mut mesh.voxels {
            for y in x {
                for z in y {
                    z.kind = VoxelType::Blue;
                }
            }
        }
        mesh.voxels
            .get_mut(0)
            .unwrap()
            .get_mut(0)
            .unwrap()
            .get_mut(0)
            .unwrap()
            .kind = VoxelType::Air;
        e
    }

    pub fn create_player_safe(&mut self) -> PlayerKey {
        let e = self.ecs.create_entity_safe();
        let position = IsometryReal::from_parts(
            Translation3::new(0.0, 40.0, 5.0),
            Unit::<Quaternion<f32>>::identity(),
        );
        let body = RigidBodyBuilder::kinematic_position_based()
            .position(position)
            .gravity_scale(0.0)
            .can_sleep(true)
            .enabled_rotations(true, true, true)
            .ccd_enabled(false)
            .angular_damping(1.0)
            .user_data(e.data().as_ffi() as u128)
            .build();
        let handle = self.data.physics.bodies.insert(body);
        self.data.physics.undo(move |d, _| {
            d.bodies.remove(
                handle,
                &mut d.islands,
                &mut d.colliders,
                &mut d.implules_joint_set,
                &mut d.multi_body_joint_set,
                true,
            );
        });
        self.data.ecs.rigidbody.set_safe(e, Some(handle));
        self.data.ecs.camera.set_safe(e, Some(Camera::new(handle)));
        self.data.ecs.player.set_safe(
            e,
            Some(Player {
                fps_cam_mode: false,
                bevy: BevyInput::default(),
            }),
        );
        let key = self.data.players.insert(e);
        self.data.players.undo(move |d, _| {
            d.remove(key);
        });
        key
    }
}
