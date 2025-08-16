use crossbeam::channel::Sender;
use rapier3d::{na::Vector3, prelude::RigidBodyHandle};
use serde::{Deserialize, Serialize};

use crate::{
    camera::Camera,
    common::{ClientUpdateEvent, RegionId, Tick},
    physics::PhysicsState,
    CameraUniform, EntityId,
};

#[derive(Copy, Clone)]
pub enum GameDataTransactionKind {
    Do,
    Undo,
}

pub struct GameDataTransaction<'a> {
    d: &'a mut GameData,
    kind: GameDataTransactionKind,
}

impl<'a> GameDataTransaction<'a> {
    pub fn new(data: &'a mut GameData, kind: GameDataTransactionKind) -> Self {
        Self { d: data, kind }
    }

    pub fn raw(&'a self) -> &'a GameDataRaw {
        &self.d.raw
    }

    fn send(&mut self, e: UpdateGameData) {
        self.d.client.as_ref().inspect(|c| {
            c.send(ClientUpdateEvent::UpdateRegion(self.d.id, e, self.kind))
                .unwrap()
        });
    }

    pub fn tick(&mut self) {
        self.d.raw.tick += 1;
    }

    pub fn untick(&mut self) {
        self.d.raw.tick -= 1;
    }

    pub fn set_camera_uniform(&mut self, uniform: CameraUniform, cam_id: usize) {
        let camera = &mut self
            .d
            .raw
            .entities
            .get_mut(cam_id)
            .unwrap()
            .kind
            .as_camera_mut();
        camera.uniform = uniform;
        self.send(UpdateGameData::SetCameraUniform(cam_id, uniform));
    }

    pub fn update_camera(&mut self, e: EntityId) -> CameraUniform {
        let camera = match &mut self.d.raw.entities.get_mut(e).unwrap().kind {
            EntityType::Camera(camera) => camera,
            _ => panic!("Tried to translate camera but entity isn't a camera"),
        };

        // use cgmath::InnerSpace;
        // camera.position += camera.velocity;
        // let forward = camera.target - camera.eye;
        // let forward_norm = forward.normalize();

        // camera.eye += forward_norm * camera.velocity.x;
        // let right = forward_norm.cross(camera.up);
        // camera.eye += right * camera.velocity.z;

        // Redo radius calc in case the forward/backward is pressed.
        // let forward = camera.target - camera.eye;
        // let forward_mag = forward.magnitude();

        // if camera.velocity.z > 0.0 {
        //     camera.eye =
        //         camera.target - (forward + right * camera.velocity.z).normalize() * forward_mag;
        // } else {
        //     camera.eye =
        //         camera.target - (forward - right * camera.velocity.z).normalize() * forward_mag;
        // }
        let old = camera.update_view_proj();
        self.send(UpdateGameData::UpdateCamera(e));
        return old;
    }

    pub fn create_entity(&mut self, e: Entity) -> EntityId {
        let index = self.d.raw.entities.len();
        self.d.raw.entities.insert(index, e.clone());
        self.send(UpdateGameData::CreateEntity(e));
        index
    }

    pub fn remove_entity(&mut self, e: EntityId) {
        self.d.raw.entities.remove(e);
        self.send(UpdateGameData::RemoveEntity(e));
    }

    pub fn set_camera_velocity(&mut self, e: EntityId, x: f32, y: f32, z: f32) {
        // let camera = self.d.raw.entities.get_mut(e).unwrap();
        // camera.velocity = cgmath::Vector3::new(x, y, z);
        self.send(UpdateGameData::SetCameraVelocity(e, x, y, z));
    }
    pub fn set_camera_angular_velocity(&mut self, e: EntityId, x: f32, y: f32, z: f32) {
        // let camera = self.d.raw.entities.get_mut(e).unwrap().kind.as_camera_mut();
        // camera = cgmath::Vector3::new(x, y, z);
        self.send(UpdateGameData::SetCameraVelocity(e, x, y, z));
    }
}

pub struct GameData {
    raw: GameDataRaw,
    id: RegionId,
    client: Option<Sender<ClientUpdateEvent>>,
}

pub enum UpdateGameData {
    CreateEntity(Entity),
    UpdateCamera(EntityId),
    SetCameraUniform(EntityId, CameraUniform),
    RemoveEntity(EntityId),
    SetCameraVelocity(EntityId, f32, f32, f32),
    SetCameraAngularVelocity(EntityId, f32, f32, f32),
}

impl GameData {
    pub fn new(data: GameDataRaw, client: Option<Sender<ClientUpdateEvent>>, id: RegionId) -> Self {
        Self {
            raw: data,
            client,
            id,
        }
    }

    pub fn change(&mut self) -> GameDataTransaction<'_> {
        GameDataTransaction {
            d: self,
            kind: GameDataTransactionKind::Do,
        }
    }

    pub fn undo(&mut self) -> GameDataTransaction<'_> {
        GameDataTransaction {
            d: self,
            kind: GameDataTransactionKind::Undo,
        }
    }

    pub fn raw<'a>(&'a self) -> &'a GameDataRaw {
        &self.raw
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Entity {
    pub kind: EntityType,
    pub handle: Option<RigidBodyHandle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntityType {
    Layout,
    Camera(Camera),
    Text { content: String },
    Default,
}

impl Default for EntityType {
    fn default() -> Self {
        Self::Default
    }
}

impl EntityType {
    pub fn as_camera(&self) -> &Camera {
        match self {
            EntityType::Camera(camera) => camera,
            _ => panic!("Entity not camera"),
        }
    }
    pub fn as_camera_mut(&mut self) -> &mut Camera {
        match self {
            EntityType::Camera(camera) => camera,
            _ => panic!("Entity not camera"),
        }
    }
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
pub struct SimulationChunk {
    pub voxels: [[[Voxel; 4]; 4]; 4],
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GameDataRaw {
    pub tick: Tick,
    pub chunk: SimulationChunk,
    pub entities: Vec<Entity>,
    pub physics: PhysicsState,
}

impl GameDataRaw {
    pub fn new() -> Self {
        Self {
            tick: 0,
            physics: PhysicsState::new(),
            entities: Vec::new(),
            chunk: SimulationChunk::default(),
        }
    }
}
