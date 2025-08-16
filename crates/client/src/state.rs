use std::{collections::BTreeMap, sync::Arc};

use crossbeam::channel::{Receiver, Sender};
use game::{
    CameraUniform, ClientUpdateEvent, EntityId, EntityType, GameData, GameDataTransactionKind,
    GameEventKind, UpdateGameData,
};
use wgpu::{
    util::{BufferInitDescriptor, DeviceExt},
    BufferUsages, TextureFormat,
};
use winit::window::Window;

use crate::{
    layout::CAMERA_LAYOUT_DESC,
    mesh::{ChunkMesh, Vertex},
};

pub struct State {
    pub client_recv: Receiver<ClientUpdateEvent>,
    pub game_send: Sender<GameEventKind>,
    window: Arc<Window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    size: winit::dpi::PhysicalSize<u32>,
    surface: wgpu::Surface<'static>,
    surface_format: wgpu::TextureFormat,
    render_pipeline: wgpu::RenderPipeline,
    buffer: Option<(wgpu::Buffer, usize)>,
    cameras: BTreeMap<EntityId, (wgpu::Buffer, wgpu::BindGroup)>,
    default_camera: (wgpu::Buffer, wgpu::BindGroup),
}

impl State {
    pub async fn new(
        window: Arc<Window>,
        client_recv: Receiver<ClientUpdateEvent>,
        game_send: Sender<GameEventKind>,
    ) -> State {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .unwrap();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default(), None)
            .await
            .unwrap();

        let camera_bind_group_layout = device.create_bind_group_layout(CAMERA_LAYOUT_DESC);

        let render_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Render Pipeline Layout"),
                bind_group_layouts: &[&camera_bind_group_layout],
                push_constant_ranges: &[],
            });

        let size = window.inner_size();

        let surface = instance.create_surface(window.clone()).unwrap();
        let cap = surface.get_capabilities(&adapter);
        let surface_format = cap
            .formats
            .into_iter()
            .find(|it| matches!(it, TextureFormat::Rgba8Unorm | TextureFormat::Bgra8Unorm))
            .unwrap();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Render Pipeline"),
            layout: Some(&render_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::desc()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview: None,
            cache: None,
        });

        let mut cam = game::Camera::new();
        cam.update_view_proj();
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Camera Buffer"),
            contents: bytemuck::cast_slice(&[cam.uniform]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &device.create_bind_group_layout(CAMERA_LAYOUT_DESC),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
            label: Some("camera_bind_group"),
        });
        let state = State {
            window,
            device,
            queue,
            size,
            surface,
            surface_format,
            client_recv,
            game_send,
            render_pipeline,
            buffer: None,
            cameras: BTreeMap::new(),
            default_camera: (camera_buffer, camera_bind_group),
        };

        // Configure surface for the first time
        state.configure_surface();

        state
    }

    pub fn get_window(&self) -> &Window {
        &self.window
    }

    pub fn configure_surface(&self) {
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: self.surface_format,
            view_formats: vec![],
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            width: self.size.width,
            height: self.size.height,
            desired_maximum_frame_latency: 2,
            present_mode: wgpu::PresentMode::AutoVsync,
        };
        self.surface.configure(&self.device, &surface_config);
    }

    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        self.size = new_size;

        // reconfigure the surface
        self.configure_surface();
    }

    // upload generated mesh to buffer
    pub fn set_chunk(&mut self, m: ChunkMesh) {
        self.buffer = Some((
            self.device.create_buffer_init(&BufferInitDescriptor {
                label: Some("Chunk Buffer 0"),
                contents: &bytemuck::cast_slice(&m.vertices[..]),
                usage: BufferUsages::VERTEX,
            }),
            m.vertices.len(),
        ));
    }

    pub fn render(&mut self, regions: &BTreeMap<usize, GameData>) {
        let _data = if let Some(data) = regions.get(&0) {
            data
        } else {
            return;
        };

        let surface_texture = self
            .surface
            .get_current_texture()
            .expect("failed to get surface texture");
        let surface_texture_view =
            surface_texture
                .texture
                .create_view(&wgpu::TextureViewDescriptor {
                    format: Some(self.surface_format),
                    ..Default::default()
                });

        let mut background = self.device.create_command_encoder(&Default::default());
        let mut renderpass = background.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &surface_texture_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        renderpass.set_pipeline(&self.render_pipeline); // 2.

        let (_, cam_bind_group) = if let Some(cam) = self.cameras.get(&0) {
            cam
        } else {
            &self.default_camera
        };

        if let Some((buf, size)) = &self.buffer {
            renderpass.set_bind_group(0, cam_bind_group, &[]);
            renderpass.set_vertex_buffer(0, buf.slice(..));
            renderpass.draw(0..*size as u32, 0..1);
        }

        drop(renderpass);

        self.queue.submit([background.finish()]);
        drop(surface_texture_view);
        self.window.pre_present_notify();
        surface_texture.present();
    }

    pub fn create_camera(&mut self, index: usize, uniform: CameraUniform) {
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Camera Buffer"),
                contents: bytemuck::cast_slice(&[uniform]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let camera_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &self.device.create_bind_group_layout(CAMERA_LAYOUT_DESC),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
            label: Some("camera_bind_group"),
        });
        self.cameras.insert(index, (buffer, camera_bind_group));
    }

    pub fn add_region(&mut self, data: &GameData) {
        for (i, e) in data.raw().entities.iter().enumerate() {
            match &e.kind {
                EntityType::Camera(camera) => {
                    self.create_camera(i, camera.uniform);
                }
                _ => (),
            }
        }
        self.set_chunk(ChunkMesh::new(data.raw()));
    }

    pub fn update(
        &mut self,
        event: UpdateGameData,
        data: &mut GameData,
        kind: GameDataTransactionKind,
    ) {
        match event {
            game::UpdateGameData::CreateEntity(e) => {
                let index = data.change().create_entity(e.clone());
                match &e.kind {
                    game::EntityType::Camera(camera) => {
                        self.create_camera(index, camera.uniform);
                    }
                    _ => (),
                }
            }
            game::UpdateGameData::RemoveEntity(i) => {
                let e = data.raw().entities.get(i).unwrap();
                match e.kind {
                    EntityType::Camera(_) => {
                        self.cameras.remove(&i);
                    }
                    _ => (),
                }
                data.change().remove_entity(i);
            }
            game::UpdateGameData::UpdateCamera(e) => {
                data.change().update_camera(e);
                let cam = data.raw().entities.get(e).unwrap().kind.as_camera();
                let buf = &self.cameras.get(&e).unwrap().0;
                self.queue
                    .write_buffer(buf, 0, bytemuck::cast_slice(&[cam.uniform]));
            }
            game::UpdateGameData::SetCameraVelocity(e, x, y, z) => {
                data.change().set_camera_velocity(e, x, y, z);
            }
            game::UpdateGameData::SetCameraAngularVelocity(e, x, y, z) => {
                data.change().set_camera_angular_velocity(e, x, y, z);
            }
            game::UpdateGameData::SetCameraUniform(id, uniform) => {
                data.change().set_camera_uniform(uniform, id);
            }
        }
    }
}
