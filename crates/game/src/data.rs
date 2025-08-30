use std::ops::DerefMut;

use crate::{IsometryReal, Usize};
pub use game_data::*;
use rapier3d::math::Vector;
use rapier3d::na::{Matrix4, Vector4};
use rapier3d::prelude::{
    CCDSolver, ColliderSet, DefaultBroadPhase, ImpulseJointSet, IntegrationParameters,
    IslandManager, MultibodyJointSet, NarrowPhase, QueryPipeline, RigidBodyHandle, RigidBodySet,
};
use rollback::rollback;
use slotmap::{new_key_type, SlotMap};

new_key_type! { pub struct EntityKey; }
new_key_type! { pub struct PlayerKey; }

const ASPECT: f32 = (16 / 9) as f32;

#[derive(
    Default,
    Debug,
    rollback::serde::Serialize,
    rollback::serde::Deserialize,
    Clone,
    ::borrow::Partial,
)]
#[module(crate)]
pub struct Camera {
    fovy: f32,
    znear: f32,
    zfar: f32,
    opengl_to_wgpu_matrix: Matrix4<f32>,
    pub proj_matrix: Matrix4<f32>,
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial)]
#[module(crate)]
pub struct Player {
    pub input: usize,
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial)]
#[module(crate)]
pub struct Mesh {}

#[rollback(GameData)]
mod game_data {
    use super::*;
    use std::ops::Deref;

    pub struct GameData {
        ecs: Ecs,
        physics: PhysicsState,
        tick: usize,
        players: SlotMap<PlayerKey, EntityKey>,
    }

    pub struct Ecs {
        camera: Component<Camera>,
        isometry: Component<IsometryReal>,
        rigidbody: Component<RigidBodyHandle>,
        player: Component<Player>,
        mesh: Component<Mesh>,
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
    pub fn new() -> Self {
        Camera {
            fovy: 90.0,
            znear: 0.1,
            zfar: 100.0,
            proj_matrix: Matrix4::<f32>::identity(),
            opengl_to_wgpu_matrix: Matrix4::from_columns(&[
                Vector4::new(1.0, 0.0, 0.0, 0.0),
                Vector4::new(0.0, 1.0, 0.0, 0.0),
                Vector4::new(0.0, 0.0, 0.5, 0.0),
                Vector4::new(0.0, 0.0, 0.5, 1.0),
            ]),
        }
    }
    pub fn build_projection(&mut self) -> Matrix4<f32> {
        let proj =
            rapier3d::na::Perspective3::new(self.fovy * 0.01745329, ASPECT, self.znear, self.zfar);
        let new = self.opengl_to_wgpu_matrix * proj.as_matrix();
        self.proj_matrix = new;
        return new;
    }
}

impl Undo<Ecs> {
    pub fn create_entity_safe(&mut self) -> EntityKey {
        self.camera.insert(None);
        self.isometry.insert(None);
        self.rigidbody.insert(None);
        self.player.insert(None);
        let key = self.mesh.insert(None);
        self.undo(move |d| {
            d.camera.remove(key);
            d.isometry.remove(key);
            d.rigidbody.remove(key);
            d.player.remove(key);
        });
        key
    }
}

#[derive(Default, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Component<T>
where
    T: 'static + Default,
{
    list: SlotMap<EntityKey, Option<T>>,
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
        self.undo(move |d| *d.list.get_mut(key).unwrap() = old.clone());

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
    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn insert(&mut self, item: Option<T>) -> EntityKey {
        self.list.insert(item)
    }

    pub fn remove(&mut self, item: EntityKey) -> Option<T> {
        self.list.remove(item).unwrap()
    }

    pub fn get(&self, key: EntityKey) -> &T {
        self.list.get(key).unwrap().as_ref().unwrap()
    }

    pub fn get_mut(&mut self, key: EntityKey) -> &mut T {
        self.list.get_mut(key).unwrap().as_mut().unwrap()
    }
}
