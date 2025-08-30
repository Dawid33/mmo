use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use crossbeam::channel::{Receiver, Sender};
use game::{
    ClientUpdateEvent, EntityId, GameData, GameDataTransactionKind, GameEventKind, IsometryReal,
    PlayerKey, Rollback, UpdateGameData,
};
#[allow(unused)]
use log::info;
use rapier3d::na::Matrix4;
use wgpu::{
    util::{BufferInitDescriptor, DeviceExt},
    BufferUsages, Device, Queue, TextureFormat,
};
use winit::window::Window;

use crate::{
    layout::CAMERA_LAYOUT_DESC,
    mesh::{ChunkMesh, Vertex},
    render_state::TrueRenderWorld,
};

// Contain copy of render world that can be displayed to the screen.
pub struct RenderWorld {
    _translate: BTreeSet<EntityId>,
    lerp_set: BTreeSet<EntityId>,
    device: Device,
    buffer: Option<(wgpu::Buffer, usize)>,
    cameras: BTreeMap<EntityId, (wgpu::Buffer, wgpu::BindGroup, IsometryReal)>,
    default_camera: (wgpu::Buffer, wgpu::BindGroup),
}

impl RenderWorld {
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

    pub fn add_region(&mut self, _id: usize, data: &GameData) {
        // for (i, e) in data.raw().entities.iter().enumerate() {
        //     match &e.kind {
        //         EntityType::Camera(c) => {
        //             // self.create_camera(
        //             //     i,
        //             //     c.build_view_projection_matrix(&IsometryReal::identity()),
        //             // );
        //         }
        //         _ => (),
        //     }
        // }
        self.set_chunk(ChunkMesh::new(&data));
    }

    pub fn new(device: Device) -> Self {
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Camera Buffer"),
            contents: bytemuck::cast_slice(Matrix4::<f32>::identity().as_slice()),
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
        Self {
            cameras: BTreeMap::new(),
            default_camera: (camera_buffer, camera_bind_group),
            device,
            buffer: None,
            lerp_set: BTreeSet::new(),
            _translate: BTreeSet::new(),
        }
    }

    #[allow(unused)]
    pub fn create_camera(&mut self, index: usize, view_proj: Matrix4<f32>) {
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Camera Buffer"),
                contents: bytemuck::cast_slice(view_proj.as_slice()),
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
        self.cameras
            .insert(index, (buffer, camera_bind_group, IsometryReal::identity()));
    }

    pub fn _translate(&mut self, _data: &GameData) {}

    #[allow(unused)]
    pub fn lerp(&mut self, data: &TrueRenderWorld, queue: &Queue) {
        self.lerp_set.retain(|l| {
            // let e = data.raw().entities.get(*l).unwrap();
            false
            // match &e.kind {
            //     EntityType::Camera(c) => {
            //         let (buf, _, current_iso) = self.cameras.get_mut(l).unwrap();
            //         *current_iso = current_iso.lerp_slerp(&e.physics_isometry, 0.05);
            //         // queue.write_buffer(
            //         //     buf,
            //         //     0,
            //         //     bytemuck::cast_slice(
            //         //         (c.build_view_projection_matrix(&current_iso)).as_slice(),
            //         //     ),
            //         // );
            //         if *current_iso == e.physics_isometry {
            //             false
            //         } else {
            //             true
            //         }
            //     }
            //     _ => false,
            // }
        });
    }
}

pub struct State {
    pub client_recv: Receiver<ClientUpdateEvent>,
    pub game_send: Sender<GameEventKind>,
    pub player: Option<PlayerKey>,
    window: Arc<Window>,
    queue: wgpu::Queue,
    size: winit::dpi::PhysicalSize<u32>,
    surface: wgpu::Surface<'static>,
    surface_format: wgpu::TextureFormat,
    render_pipeline: wgpu::RenderPipeline,
    world: RenderWorld,
    regions: BTreeMap<usize, TrueRenderWorld>,
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

        let state = State {
            window,
            world: RenderWorld::new(device),
            queue,
            size,
            surface,
            surface_format,
            client_recv,
            game_send,
            render_pipeline,
            regions: BTreeMap::new(),
            player: None,
        };

        // Configure surface for the first time
        state.configure_surface();

        state
    }

    pub fn add_region(&mut self, id: usize, data: Rollback) {
        self.world.add_region(id, &data);
        self.regions
            .insert(id, TrueRenderWorld::new(&data, &mut self.world));
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
            present_mode: wgpu::PresentMode::FifoRelaxed,
        };
        self.surface.configure(&self.world.device, &surface_config);
    }

    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        self.size = new_size;

        // reconfigure the surface
        self.configure_surface();
    }

    pub fn render(&mut self) {
        let _data = if let Some(data) = self.regions.get(&0) {
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

        let mut background = self
            .world
            .device
            .create_command_encoder(&Default::default());
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

        let cam_bind_group = if let Some(cam) = self.world.cameras.get(&0) {
            &cam.1
        } else {
            &self.world.default_camera.1
        };

        if let Some((buf, size)) = &self.world.buffer {
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

    /// Lerp game data from previous value (if applicable) to current value
    /// every frame to get smooth movement.
    pub fn lerp(&mut self) {
        for (_, data) in self.regions.iter() {
            self.world.lerp(data, &self.queue);
        }
    }

    /// Update render threads representations of game state.
    #[allow(unused)]
    pub fn update(&mut self, id: usize, event: UpdateGameData, _kind: GameDataTransactionKind) {
        let data = self.regions.get_mut(&id).unwrap();
        // info!("{:?}", event);
    }
}
