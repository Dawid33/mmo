use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroUsize,
    sync::Arc,
};

use block_mesh::{
    greedy_quads, GreedyQuadsBuffer, MergeVoxel, VoxelVisibility, RIGHT_HANDED_Y_UP_CONFIG,
};
use bytemuck::NoUninit;
use crossbeam::{channel::Receiver, utils::Backoff};
use derive_more::Debug;
use game::{
    na::{Matrix4, Perspective3},
    parry::math::Real,
    ChunkShape, Region, VoxelType,
};
use game::{EntityId, GameData, RegionId, ASPECT};
use log::info;
use ndshape::ConstShape;
use parley::LayoutContext;
use rand::seq::IndexedRandom;
use rollback::{EntityKey, GameDataUpdate, IsometryReal, Voxel};
use wgpu::{
    util::{BufferInitDescriptor, DeviceExt},
    BindGroup, BlendComponent, BlendFactor, BlendOperation, BufferUsages, CommandBuffer,
    CommandEncoder, Device, Queue, RenderPass, RenderPipeline, Surface, Texture, TextureFormat,
    TextureView,
};
use winit::{
    dpi::PhysicalPosition,
    window::{CursorGrabMode, Window},
};

use crate::{
    layout::CAMERA_LAYOUT_DESC,
    state::{DepthTexture, UITexture},
};

#[repr(C)]
#[derive(Copy, Clone, Debug, NoUninit)]
pub struct Vertex {
    pub position: [f32; 3],
    pub tex_coord: [f32; 2],
    pub color: [f32; 3],
}

impl Vertex {
    const ATTRIBS: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x3];

    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Vertex::ATTRIBS,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ChunkMesh {
    buffer: GreedyQuadsBuffer,
    pub indices: Vec<u32>,
    pub vertices: Vec<Vertex>,
    pub normals: Vec<[f32; 3]>,
}

impl ChunkMesh {
    pub fn new(data: &Vec<Voxel>) -> Self {
        let mut buffer = GreedyQuadsBuffer::new(data.len());
        greedy_quads(
            &data,
            &ChunkShape {},
            [0; 3],
            [31, 31, 31],
            &RIGHT_HANDED_Y_UP_CONFIG.faces,
            &mut buffer,
        );
        let num_indices = buffer.quads.num_quads() * 6;
        let num_vertices = buffer.quads.num_quads() * 4;
        let mut indices = Vec::with_capacity(num_indices);
        let mut vertices = Vec::with_capacity(num_vertices);
        let mut normals = Vec::with_capacity(num_vertices);
        for (group, face) in buffer
            .quads
            .groups
            .iter()
            .zip(RIGHT_HANDED_Y_UP_CONFIG.faces.into_iter())
        {
            let quad_tex_coords = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]];
            for quad in group.into_iter() {
                indices.extend_from_slice(&face.quad_mesh_indices(vertices.len() as u32));
                for (i, position) in face.quad_mesh_positions(&quad, 1.0).iter().enumerate() {
                    vertices.push(Vertex {
                        position: *position,
                        color: [0.0, 0.0, 0.0],
                        tex_coord: [
                            quad_tex_coords[i][0] * quad.width as f32,
                            quad_tex_coords[i][1] * quad.height as f32,
                        ],
                    });
                }
                normals.extend_from_slice(&face.quad_mesh_normals());
            }
        }

        Self {
            buffer,
            indices,
            vertices,
            normals,
        }
    }
}

#[derive(Debug)]
pub enum RenderEntityType {
    UIElement,
    Default,
}

impl Default for RenderEntityType {
    fn default() -> Self {
        Self::Default
    }
}

#[derive(Debug, Default)]
pub struct RenderEntity {
    kind: RenderEntityType,
    position: Option<IsometryReal>,
    camera: Option<(Perspective3<Real>, IsometryReal)>,
    voxel_mesh: Option<ChunkMesh>,
}

pub struct TrueWorld {
    entities: BTreeMap<EntityKey, RenderEntity>,
    reciever: Receiver<GameDataUpdate>,
}

impl TrueWorld {
    pub fn new(data: &GameData, reciever: Receiver<GameDataUpdate>) -> Self {
        let mut entities = BTreeMap::new();

        for (id, _) in data.ecs.entities.iter() {
            let position = if let Some(handle) = data.ecs.rigidbody.try_get(id) {
                info!("adding position");
                let b = data.physics.bodies.get(*handle).unwrap();
                Some(*b.position())
            } else {
                None
            };

            let camera = if let Some(cam) = data.ecs.camera.try_get(id) {
                let handle = cam.view_matrix.unwrap();
                let view_matrix = *data.physics.bodies.get(handle).unwrap().position();
                Some((cam.proj_matrix, view_matrix))
            } else {
                None
            };

            let mesh = if let Some(mesh) = data.ecs.chunk.try_get(id) {
                info!("adding mesh");
                Some(ChunkMesh::new(&mesh.voxels))
            } else {
                None
            };

            entities.insert(
                id,
                RenderEntity {
                    kind: RenderEntityType::Default,
                    position,
                    camera,
                    voxel_mesh: mesh,
                },
            );
        }
        Self { reciever, entities }
    }
}

#[derive(Debug)]
struct VoxelMesh {
    vertices: wgpu::Buffer,
    num_vertices: usize,
    indices: wgpu::Buffer,
    num_indices: u32,
}

#[derive(Debug, Default)]
pub struct GpuEntity {
    camera: Option<(wgpu::Buffer, wgpu::Buffer, wgpu::BindGroup, IsometryReal)>,
    position: Option<IsometryReal>,
    voxel_mesh: Option<VoxelMesh>,
}

#[derive(Debug)]
pub struct GpuData {
    #[debug(skip)]
    device: Device,
    entities: BTreeMap<RegionId, BTreeMap<EntityKey, GpuEntity>>,
    current_cam: Option<(RegionId, EntityKey)>,
    default_camera: (wgpu::Buffer, wgpu::Buffer, wgpu::BindGroup),
}

impl GpuData {
    pub fn new(device: Device) -> Self {
        let view_proj = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Camera Buffer"),
            contents: bytemuck::cast_slice(Matrix4::<f32>::identity().as_slice()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let view_matrix = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Camera Buffer"),
            contents: bytemuck::cast_slice(Matrix4::<f32>::identity().as_slice()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &device.create_bind_group_layout(CAMERA_LAYOUT_DESC),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: view_proj.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: view_matrix.as_entire_binding(),
                },
            ],
            label: Some("camera_bind_group"),
        });
        Self {
            default_camera: (view_proj, view_matrix, camera_bind_group),
            entities: BTreeMap::new(),
            current_cam: None,
            device,
        }
    }

    pub fn create_camera(
        &mut self,
        region: RegionId,
        entity_key: EntityKey,
        view_proj: Perspective3<Real>,
        view_matrix: IsometryReal,
    ) -> (wgpu::Buffer, wgpu::Buffer, wgpu::BindGroup, IsometryReal) {
        let proj_matrix = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Camera Projection Buffer"),
                contents: bytemuck::cast_slice(view_proj.as_matrix().as_slice()),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let view_matrix_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Camera View Matrix Buffer"),
                contents: bytemuck::cast_slice(view_matrix.to_matrix().as_slice()),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &self.device.create_bind_group_layout(CAMERA_LAYOUT_DESC),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: proj_matrix.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: view_matrix_buf.as_entire_binding(),
                },
            ],
            label: Some("camera_bind_group"),
        });
        let mut entity = GpuEntity::default();
        (proj_matrix, view_matrix_buf, bind_group, view_matrix)
    }

    pub fn add_region(&mut self, id: RegionId, world: &TrueWorld) {
        info!("Adding region = {:?}", id);
        self.entities.insert(id, BTreeMap::new());
        for (key, e) in &world.entities {
            info!("Adding entity = {:?}", key);
            self.create_entity(&id, *key, e);
        }
    }

    pub fn remove_entity(&mut self, id: &RegionId, key: EntityKey) {
        self.entities.get_mut(&id).unwrap().remove(&key);
    }

    pub fn create_entity(&mut self, id: &RegionId, key: EntityKey, e: &RenderEntity) {
        let mut gpu_e = GpuEntity::default();
        if let Some(pos) = e.position {
            gpu_e.position = Some(pos);
        }

        if let Some(cam) = e.camera {
            self.current_cam = Some((*id, key));
            gpu_e.camera = Some(self.create_camera(*id, key, cam.0, cam.1));
        }

        if let Some(m) = &e.voxel_mesh {
            info!("Creating mesh entity");
            gpu_e.voxel_mesh = Some(VoxelMesh {
                vertices: self.device.create_buffer_init(&BufferInitDescriptor {
                    label: Some(&format!("Chunk Buffer {:?}", e)),
                    contents: &bytemuck::cast_slice(&m.vertices[..]),
                    usage: BufferUsages::VERTEX,
                }),
                num_vertices: m.vertices.len(),
                indices: self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Index Buffer"),
                        contents: bytemuck::cast_slice(&m.indices[..]),
                        usage: wgpu::BufferUsages::INDEX,
                    }),
                num_indices: m.indices.len() as u32,
            });
        }
        self.entities.get_mut(&id).unwrap().insert(key, gpu_e);
    }
}

/// When adding a new region:
/// - Initialize TrueWorld with data.
/// - Initialize GpuData with TrueWorld.
///
/// The loop, for each regoin:
/// - Update TrueWorld with events from sim thread.
/// - Lerp GpuData towards TrueWorld.
/// - Draw region.
pub struct RenderWorld {
    gpu: GpuData,
    true_world: BTreeMap<RegionId, TrueWorld>,
    lerp_set: BTreeMap<RegionId, BTreeSet<EntityKey>>,
    pub in_freecam: bool,
}

impl RenderWorld {
    pub fn new(device: Device) -> Self {
        Self {
            gpu: GpuData::new(device),
            lerp_set: BTreeMap::new(),
            true_world: BTreeMap::new(),
            in_freecam: false,
        }
    }

    pub fn device(&self) -> &Device {
        &self.gpu.device
    }

    pub fn add_region(
        &mut self,
        id: RegionId,
        data: &GameData,
        receiver: Receiver<GameDataUpdate>,
    ) {
        let mut set = BTreeSet::new();
        // let iter = data.into_game_update_iter();
        let world = TrueWorld::new(&data, receiver);
        for (e, _) in &world.entities {
            set.insert(*e);
        }
        self.gpu.add_region(id, &world);
        self.true_world.insert(id, world);
        self.lerp_set.insert(id, set);
    }

    /// 1. Update true worlds with updates from sim thread.
    /// 2. Lerp GpuEntities based on updates to true worlds
    pub fn update(&mut self, queue: &Queue, window: &Window) {
        for (i, region) in self.true_world.iter_mut() {
            while let Ok(event) = region.reciever.try_recv() {
                match event.update_kind {
                    game::GameDataUpdateKind::SetEntityPosition(entity_key, isometry) => {
                        info!("client update: {:?}", event);
                        let e = region.entities.get_mut(&entity_key).unwrap();
                        e.position = Some(isometry);
                        self.lerp_set.get_mut(i).unwrap().insert(entity_key);
                    }
                    game::GameDataUpdateKind::CreateEntity(entity_key) => {
                        info!("client update: {:?}", event);
                        let entity = RenderEntity::default();
                        self.gpu.create_entity(i, entity_key, &entity);
                        region.entities.insert(entity_key, entity);
                    }
                    game::GameDataUpdateKind::RemoveEntity(entity_key) => {
                        info!("client update: {:?}", event);
                        self.gpu.remove_entity(i, entity_key);
                        region.entities.remove(&entity_key);
                    }
                    game::GameDataUpdateKind::UpdateCameraViewProj(entity_key, matrix) => {
                        let e = region.entities.get_mut(&entity_key).unwrap();
                        e.camera.as_mut().unwrap().0 = matrix;
                        let gpu_e = self
                            .gpu
                            .entities
                            .get_mut(i)
                            .unwrap()
                            .get_mut(&entity_key)
                            .unwrap();
                        let cam_bufs = gpu_e.camera.as_ref().unwrap();
                        queue.write_buffer(
                            &cam_bufs.0,
                            0,
                            bytemuck::cast_slice(matrix.as_matrix().as_slice()),
                        );
                    }
                    game::GameDataUpdateKind::UpdateCameraViewMatrix(entity_key, matrix) => {
                        let e = region.entities.get_mut(&entity_key).unwrap();
                        e.camera.as_mut().unwrap().1 = matrix;
                        self.lerp_set.get_mut(i).unwrap().insert(entity_key);
                    }
                    game::GameDataUpdateKind::SetFreeCam(entity_key, mode) => {
                        if mode {
                            window.set_cursor_visible(false);
                            window.set_cursor_grab(CursorGrabMode::Confined);
                        } else {
                            window.set_cursor_position(PhysicalPosition::new(
                                window.inner_size().width / 2,
                                window.inner_size().height / 2,
                            ));
                            window.set_cursor_visible(true);
                            window.set_cursor_grab(CursorGrabMode::None);
                        }
                    }
                    game::GameDataUpdateKind::SetVoxelComponent(entity_key, voxel) => {
                        if let Some(v) = voxel {
                            region.entities.get_mut(&entity_key).unwrap().voxel_mesh =
                                Some(ChunkMesh::new(&v));
                        } else {
                            region.entities.get_mut(&entity_key).unwrap().voxel_mesh = None;
                        }
                    }
                    game::GameDataUpdateKind::AddCameraComponent(
                        entity_key,
                        perspective3,
                        isometry,
                    ) => {
                        let e = region.entities.get_mut(&entity_key).unwrap();
                        e.camera = Some((perspective3, isometry));
                        self.gpu
                            .entities
                            .get_mut(i)
                            .unwrap()
                            .get_mut(&entity_key)
                            .unwrap()
                            .camera =
                            Some(
                                self.gpu
                                    .create_camera(*i, entity_key, perspective3, isometry),
                            );
                        if self.gpu.current_cam.is_none() {
                            self.gpu.current_cam = Some((*i, entity_key));
                        }
                    }
                    game::GameDataUpdateKind::RemoveCameraComponent(entity_key) => {
                        let e = region.entities.get_mut(&entity_key).unwrap();
                        e.camera = None;
                    }
                }
            }
        }

        for (i, set) in &mut self.lerp_set {
            set.retain(|e| {
                let gpu_entities = self.gpu.entities.get_mut(i).unwrap();
                let mut gpu_e = gpu_entities.get_mut(e).unwrap();
                let entities = &mut self.true_world.get_mut(i).unwrap().entities;
                let entity = entities.get(e).unwrap();

                let keep = if let Some(true_pos) = entity.position {
                    let pos = gpu_e.position.as_mut().unwrap();
                    if pos
                        .translation
                        .vector
                        .metric_distance(&true_pos.translation.vector)
                        <= Real::from(0.1)
                    {
                        *pos = true_pos;
                    } else {
                        *pos = pos.lerp_slerp(&true_pos, Real::from(0.5));
                    }
                    if *pos == true_pos {
                        false
                    } else {
                        true
                    }
                } else {
                    false
                };

                let keep = if let Some(cam) = entity.camera {
                    let cam_bufs = &mut gpu_e.camera.as_mut().unwrap();

                    if cam_bufs
                        .3
                        .translation
                        .vector
                        .metric_distance(&cam.1.translation.vector)
                        <= Real::from(0.0005)
                        && cam_bufs.3.rotation.angle_to(&cam.1.rotation) < Real::from(0.001)
                    {
                        cam_bufs.3 = cam.1;
                    } else {
                        cam_bufs.3 = cam_bufs.3.lerp_slerp(&cam.1, Real::from(0.1));
                    }
                    queue.write_buffer(
                        &cam_bufs.1,
                        0,
                        bytemuck::cast_slice(cam_bufs.3.inverse().to_matrix().as_slice()),
                    );

                    if cam_bufs.3 == cam.1 {
                        false
                    } else {
                        true
                    }
                } else {
                    false
                };

                keep
            });
        }
    }

    pub fn draw(
        &mut self,
        queue: &Queue,
        surface_texture_view: &TextureView,
        depth_texture: &DepthTexture,
        render_pipeline: &RenderPipeline,
        surface_format: &wgpu::TextureFormat,
        size: &winit::dpi::PhysicalSize<u32>,
        texture_array_bind_group: &BindGroup,
    ) -> Vec<CommandBuffer> {
        let mut background = self.device().create_command_encoder(&Default::default());
        let mut renderpass = background.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &surface_texture_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_texture.view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        renderpass.set_pipeline(&render_pipeline);

        let cam_bind_group = if let Some((r, e)) = &self.gpu.current_cam {
            let e = self.gpu.entities.get(r).unwrap().get(e).unwrap();
            &e.camera.as_ref().unwrap().2
        } else {
            &self.gpu.default_camera.2
        };

        for (i, region) in self.gpu.entities.iter() {
            for (_, e) in region {
                if let Some(mesh) = &e.voxel_mesh {
                    renderpass.set_bind_group(0, cam_bind_group, &[]);
                    renderpass.set_bind_group(1, texture_array_bind_group, &[]);
                    renderpass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    renderpass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    renderpass.draw_indexed(0..mesh.num_indices, 0, 0..1);
                }
            }
        }
        drop(renderpass);
        Vec::from([background.finish()])
    }
}
