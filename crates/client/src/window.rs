use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    num::NonZeroUsize,
    sync::{Arc, Mutex, MutexGuard},
};

use crossbeam::channel::{Receiver, Sender};
use game::{ClientUpdateEvent, GameEventKind, RegionData};
use log::info;
use parley::{swash::scale::Render, PositionedLayoutItem};
use vello::{
    kurbo::{Affine, Line, Stroke},
    peniko::{color::palette, Fill},
    AaConfig, RendererOptions, Scene,
};
use wgpu::{
    hal::empty::Encoder,
    util::{RenderEncoder, TextureBlitter},
    Backends, CommandEncoder, Features, RenderPass, TextureAspect, TextureFormat, TextureView,
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

use crate::Command;

struct State {
    window: Arc<Window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    size: winit::dpi::PhysicalSize<u32>,
    surface: wgpu::Surface<'static>,
    surface_format: wgpu::TextureFormat,
    client_recv: Receiver<ClientUpdateEvent>,
    render_pipeline: wgpu::RenderPipeline,
    game_send: Sender<GameEventKind>,
}

impl State {
    async fn new(
        window: Arc<Window>,
        client_recv: Receiver<ClientUpdateEvent>,
        game_send: Sender<GameEventKind>,
    ) -> State {
        // let mut desc = wgpu::InstanceDescriptor::default();
        // desc.backends = Backends::VULKAN;
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .unwrap();
        // let mut desc = wgpu::DeviceDescriptor::default();
        // desc.required_features.insert(Features::BGRA8UNORM_STORAGE);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default(), None)
            .await
            .unwrap();

        let render_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Render Pipeline Layout"),
                bind_group_layouts: &[],
                push_constant_ranges: &[],
            });

        let size = window.inner_size();

        let surface = instance.create_surface(window.clone()).unwrap();
        let cap = surface.get_capabilities(&adapter);
        info!("{:?}", cap.formats);
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
                entry_point: Some("vs_main"), // 1.
                buffers: &[],                 // 2.
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                // 3.
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    // 4.
                    format: surface_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList, // 1.
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw, // 2.
                cull_mode: Some(wgpu::Face::Back),
                // Setting this to anything other than Fill requires Features::NON_FILL_POLYGON_MODE
                polygon_mode: wgpu::PolygonMode::Fill,
                // Requires Features::DEPTH_CLIP_CONTROL
                unclipped_depth: false,
                // Requires Features::CONSERVATIVE_RASTERIZATION
                conservative: false,
            },
            depth_stencil: None, // 1.
            multisample: wgpu::MultisampleState {
                count: 1,                         // 2.
                mask: !0,                         // 3.
                alpha_to_coverage_enabled: false, // 4.
            },
            multiview: None, // 5.
            cache: None,     // 6.
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
        };

        // Configure surface for the first time
        state.configure_surface();

        state
    }

    fn get_window(&self) -> &Window {
        &self.window
    }

    //Rgba8Unorm
    fn configure_surface(&self) {
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

    fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        self.size = new_size;

        // reconfigure the surface
        self.configure_surface();
    }

    fn text(
        &mut self,
        surface_texture_view: &TextureView,
        rdata: &MutexGuard<RegionData>,
    ) -> CommandEncoder {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let blitter = TextureBlitter::new(&self.device, self.surface_format);
        let target_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: self.size.width,
                height: self.size.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            format: TextureFormat::Rgba8Unorm,
            view_formats: &[],
        });
        let text_view = target_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let mut renderer = vello::Renderer::new(
            &self.device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::all(),
                num_init_threads: NonZeroUsize::new(1),
                pipeline_cache: None,
            },
        )
        .unwrap();

        for (i, e) in rdata.data.entities.iter().enumerate() {
            match &e.kind {
                game::EntityType::Text { .. } => {
                    let mut scene = Scene::new();
                    let l = rdata.text_layouts.get(&i).unwrap();

                    for line in l.lines() {
                        for item in line.items() {
                            let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                                continue;
                            };
                            let mut x = glyph_run.offset();
                            let y = glyph_run.baseline();
                            let run = glyph_run.run();
                            let font = run.font();
                            let font_size = run.font_size();
                            let synthesis = run.synthesis();
                            let glyph_xform = synthesis
                                .skew()
                                .map(|angle| Affine::skew(angle.to_radians().tan() as f64, 0.0));
                            scene
                                .draw_glyphs(font)
                                // .brush(&style.brush)
                                .hint(true)
                                // .transform(transform)
                                .glyph_transform(glyph_xform)
                                .font_size(font_size)
                                .normalized_coords(run.normalized_coords())
                                .draw(
                                    Fill::NonZero,
                                    glyph_run.glyphs().map(|glyph| {
                                        let gx = x + glyph.x;
                                        let gy = y - glyph.y;
                                        x += glyph.advance;
                                        vello::Glyph {
                                            id: glyph.id as _,
                                            x: gx,
                                            y: gy,
                                        }
                                    }),
                                );
                        }
                    }
                    renderer
                        .render_to_texture(
                            &self.device,
                            &self.queue,
                            &scene,
                            &text_view,
                            &vello::RenderParams {
                                base_color: palette::css::WHITE, // Background color
                                width: self.size.width,
                                height: self.size.height,
                                antialiasing_method: AaConfig::Msaa16,
                            },
                        )
                        .expect("Failed to render to a texture");
                }
                _ => {}
            }
        }

        // TODO: apply texture_view to 3D Mesh
        blitter.copy(
            &self.device,
            &mut encoder,
            &text_view,
            &surface_texture_view,
        );
        drop(text_view);
        encoder
    }

    fn render(&mut self, regions: &BTreeMap<usize, Arc<Mutex<RegionData>>>) {
        let data = if let Some(data) = regions.get(&0) {
            data
        } else {
            return;
        };
        let rdata = data.lock().unwrap();

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
        renderpass.draw(0..3, 0..1);

        drop(renderpass);

        self.queue.submit([background.finish()]);
        drop(rdata);
        drop(surface_texture_view);
        self.window.pre_present_notify();
        surface_texture.present();
    }
}

pub struct App {
    state: Option<State>,
    command_sender: Sender<Command>,
    regions: BTreeMap<usize, Arc<Mutex<RegionData>>>,
}

impl App {
    pub fn new(command_sender: Sender<Command>) -> Self {
        Self {
            state: None,
            command_sender,
            regions: BTreeMap::new(),
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes())
                .unwrap(),
        );
        window.set_title("Brick Racer");
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded();
        self.command_sender
            .send(Command::ConnectToServerAndScene(
                game_send.clone(),
                game_recv,
                client_send,
                SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 6466)),
            ))
            .unwrap();
        let state = pollster::block_on(State::new(window.clone(), client_recv, game_send));
        self.state = Some(state);

        window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let state = self.state.as_mut().unwrap();

        match event {
            WindowEvent::CloseRequested => {
                println!("The close button was pressed; stopping");
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                while let Ok(event) = state.client_recv.try_recv() {
                    match event {
                        ClientUpdateEvent::Region(mutex) => {
                            self.regions.insert(0, mutex);
                        }
                        ClientUpdateEvent::GameCrash(_) => todo!(),
                    }
                }
                state.render(&self.regions);
                state.get_window().request_redraw();
            }
            WindowEvent::Resized(size) => {
                state.resize(size);
            }
            _ => (),
        }
    }
}
