use std::{
    collections::{BTreeMap, BTreeSet},
    num::{NonZeroU32, NonZeroU64},
    ops::BitAnd,
    sync::Arc,
};

use crossbeam::channel::{Receiver, Sender};
use game::{na::Matrix4, ClientId};
use game::{
    ClientUpdateEvent, EntityId, GameData, GameDataTransactionKind, GameDataUpdate, GameEventKind,
    RegionId, Rollback,
};
use image::EncodableLayout;
#[allow(unused)]
use log::info;
use log::warn;
use rand::seq::IndexedRandom;
use rollback::{EntityKey, IsometryReal};
use wgpu::{
    util::{BufferInitDescriptor, DeviceExt},
    BlendComponent, BlendFactor, BlendOperation, BlendState, BufferUsages, Device, Features, Queue,
    TextureFormat,
};
use winit::window::{CursorGrabMode, Window};

use crate::{
    layout::CAMERA_LAYOUT_DESC,
    render_world::{GpuEntity, RenderWorld, Vertex},
};

pub struct State {
    pub client_recv: Receiver<ClientUpdateEvent>,
    pub game_send: Sender<GameEventKind>,
    pub player: Option<ClientId>,
    pub window: Arc<Window>,
    queue: wgpu::Queue,
    size: winit::dpi::PhysicalSize<u32>,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    alpha_mode: wgpu::CompositeAlphaMode,
    depth_texture: DepthTexture,
    // ui_texture: UITexture,
    // blitter: TextureBlitter,
    surface_format: wgpu::TextureFormat,
    render_pipeline: wgpu::RenderPipeline,
    render_world: RenderWorld,
    texture_array_bind_group: wgpu::BindGroup,
}

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

pub struct UITexture {
    texture: wgpu::Texture,
    pub view: wgpu::TextureView,
}

pub struct DepthTexture {
    texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

impl State {
    pub fn create_depth_texture(
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
        label: &str,
        size: &winit::dpi::PhysicalSize<u32>,
    ) -> DepthTexture {
        let depth_texture_size = wgpu::Extent3d {
            width: size.width,
            height: size.height,
            depth_or_array_layers: 1,
        };
        let desc = wgpu::TextureDescriptor {
            label: Some("depth_texture"),
            size: depth_texture_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        };
        let texture = device.create_texture(&desc);

        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            compare: Some(wgpu::CompareFunction::LessEqual),
            lod_min_clamp: 0.0,
            lod_max_clamp: 100.0,
            ..Default::default()
        });
        DepthTexture {
            texture,
            view,
            sampler,
        }
    }

    pub async fn new(
        window: Arc<Window>,
        client_recv: Receiver<ClientUpdateEvent>,
        game_send: Sender<GameEventKind>,
    ) -> State {
        let assets = std::fs::read_dir("assets/blocks").unwrap();
        let mut images = BTreeMap::new();
        for file in assets {
            let file = match file {
                Ok(file) => file,
                Err(_) => continue,
            };

            if let Some(extension) = file.path().extension() {
                if extension != "png" {
                    continue;
                }
                match image::ImageReader::open(file.path()) {
                    Ok(image) => match image.decode() {
                        Ok(image) => {
                            images.insert(file.file_name(), image.to_rgba8());
                        }
                        Err(e) => warn!("failed to decode {:?}, {:?}", file.path(), e),
                    },
                    Err(e) => warn!("failed to read {:?}, {:?}", file.path(), e),
                }
            }
        }

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .unwrap();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits: wgpu::Limits {
                    max_binding_array_elements_per_shader_stage: 6,
                    max_binding_array_sampler_elements_per_shader_stage: 2,
                    ..wgpu::Limits::downlevel_defaults()
                },

                required_features: wgpu::Features::POLYGON_MODE_LINE
                    | wgpu::Features::TEXTURE_BINDING_ARRAY,
                ..Default::default()
            })
            .await
            .unwrap();

        let mut tile_views = Vec::new();
        let mut mapping: BTreeMap<String, usize> = BTreeMap::new();
        for (name, tile) in images {
            use image::GenericImageView;
            let dimensions = tile.dimensions();
            let texture_size = wgpu::Extent3d {
                width: dimensions.0,
                height: dimensions.1,
                depth_or_array_layers: 1,
            };
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                size: texture_size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                label: Some(&format!("texure_{}", name.to_string_lossy())),
                view_formats: &[],
            });
            tile_views.push(texture.create_view(&wgpu::TextureViewDescriptor::default()));
            mapping.insert(name.to_string_lossy().to_string(), tile_views.len() - 1);
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &tile,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * dimensions.0),
                    rows_per_image: Some(dimensions.1),
                },
                texture_size,
            );
        }

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let texture_array_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("texture array bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: NonZeroU32::new(tile_views.len() as u32),
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: NonZeroU32::new(1),
                    },
                ],
            });

        let temp_tile_views: Vec<&wgpu::TextureView> = tile_views.iter().map(|x| x).collect();
        let texture_array_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureViewArray(&temp_tile_views[..]),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::SamplerArray(&[&sampler]),
                },
            ],
            layout: &texture_array_bind_group_layout,
            label: Some("texture array bind group"),
        });

        // let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        //     entries: &[wgpu::BindGroupEntry {
        //         binding: 0,
        //         resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
        //             buffer: &texture_index_buffer,
        //             offset: 0,
        //             size: Some(NonZeroU64::new(4).unwrap()),
        //         }),
        //     }],
        //     layout: &uniform_bind_group_layout,
        //     label: Some("texture array index uniform bind group"),
        // });

        // let mut texture_index_buffer_contents: Vec<u32> = Vec::with_capacity(384);
        // let texture_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        //     label: Some("Texture Index Buffer"),
        //     contents: bytemuck::cast_slice(&texture_index_buffer_contents),
        //     usage: wgpu::BufferUsages::UNIFORM,
        // });

        let camera_bind_group_layout = device.create_bind_group_layout(CAMERA_LAYOUT_DESC);

        let render_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Render Pipeline Layout"),
                bind_group_layouts: &[&camera_bind_group_layout, &texture_array_bind_group_layout],
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
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            view_formats: vec![],
            alpha_mode: cap.alpha_modes[0],
            width: size.width,
            height: size.height,
            desired_maximum_frame_latency: 2,
            present_mode: wgpu::PresentMode::FifoRelaxed,
        };

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let depth_texture =
            Self::create_depth_texture(&device, &surface_config, "depth_texture", &size);

        // let ui_texture_texture = device.create_texture(&wgpu::TextureDescriptor {
        //     label: None,
        //     size: wgpu::Extent3d {
        //         width: size.width,
        //         height: size.height,
        //         depth_or_array_layers: 1,
        //     },
        //     mip_level_count: 1,
        //     sample_count: 1,
        //     dimension: wgpu::TextureDimension::D2,
        //     usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        //     format: TextureFormat::Rgba8Unorm,
        //     view_formats: &[],
        // });
        // let ui_texture_view =
        //     ui_texture_texture.create_view(&wgpu::TextureViewDescriptor::default());
        // let ui_texture = UITexture {
        //     texture: ui_texture_texture,
        //     view: ui_texture_view,
        // };

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
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview: None,
            cache: None,
        });

        // let blitter = TextureBlitterBuilder::new(&device, surface_format)
        //     .blend_state(BlendState {
        //         alpha: BlendComponent::REPLACE,
        //         color: BlendComponent {
        //             src_factor: BlendFactor::SrcAlpha,
        //             dst_factor: BlendFactor::Zero,
        //             operation: BlendOperation::Add,
        //         },
        //     })
        //     .build();
        let state = State {
            window,
            render_world: RenderWorld::new(device),
            queue,
            size,
            surface,
            surface_format,
            client_recv,
            game_send,
            render_pipeline,
            player: None,
            depth_texture,
            surface_config,
            alpha_mode: cap.alpha_modes[0],
            texture_array_bind_group,
            // ui_texture,
            // blitter,
        };

        // Configure surface for the first time
        state.configure_surface();

        state
    }

    pub fn add_region(&mut self, id: RegionId, data: GameData, receiver: Receiver<GameDataUpdate>) {
        self.render_world.add_region(id, &data, receiver);
    }

    pub fn get_window(&self) -> &Window {
        &self.window
    }

    pub fn configure_surface(&self) {
        self.surface
            .configure(self.render_world.device(), &self.surface_config);
    }

    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        self.size = new_size;
        self.surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: self.surface_format,
            view_formats: vec![],
            alpha_mode: self.alpha_mode,
            width: self.size.width,
            height: self.size.height,
            desired_maximum_frame_latency: 2,
            present_mode: wgpu::PresentMode::FifoRelaxed,
        };
        self.configure_surface();
        self.depth_texture = Self::create_depth_texture(
            &self.render_world.device(),
            &self.surface_config,
            "depth_texture",
            &self.size,
        );
    }

    pub fn render(&mut self) {
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

        self.queue.submit(self.render_world.draw(
            &self.queue,
            &surface_texture_view,
            &self.depth_texture,
            &self.render_pipeline,
            &self.surface_format,
            &self.size,
            &self.texture_array_bind_group,
        ));
        drop(surface_texture_view);
        self.window.pre_present_notify();
        surface_texture.present();
    }

    pub fn update(&mut self) {
        self.render_world.update(&self.queue, &self.window);
    }
}
