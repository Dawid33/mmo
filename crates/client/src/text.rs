// fn text(&mut self, surface_texture_view: &TextureView, data: &GameData) -> CommandEncoder {
//     let mut encoder = self.device.create_command_encoder(&Default::default());
//     let blitter = TextureBlitter::new(&self.device, self.surface_format);
//     let target_texture = self.device.create_texture(&wgpu::TextureDescriptor {
//         label: None,
//         size: wgpu::Extent3d {
//             width: self.size.width,
//             height: self.size.height,
//             depth_or_array_layers: 1,
//         },
//         mip_level_count: 1,
//         sample_count: 1,
//         dimension: wgpu::TextureDimension::D2,
//         usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
//         format: TextureFormat::Rgba8Unorm,
//         view_formats: &[],
//     });
//     let text_view = target_texture.create_view(&wgpu::TextureViewDescriptor::default());

//     let mut renderer = vello::Renderer::new(
//         &self.device,
//         RendererOptions {
//             use_cpu: false,
//             antialiasing_support: vello::AaSupport::all(),
//             num_init_threads: NonZeroUsize::new(1),
//             pipeline_cache: None,
//         },
//     )
//     .unwrap();

//     for (i, e) in data.raw().entities.iter().enumerate() {
//         match &e.kind {
//             game::EntityType::Text { content } => {
//                 let mut scene = Scene::new();

//                 // for line in l.lines() {
//                 //     for item in line.items() {
//                 //         let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
//                 //             continue;
//                 //         };
//                 //         let mut x = glyph_run.offset();
//                 //         let y = glyph_run.baseline();
//                 //         let run = glyph_run.run();
//                 //         let font = run.font();
//                 //         let font_size = run.font_size();
//                 //         let synthesis = run.synthesis();
//                 //         let glyph_xform = synthesis
//                 //             .skew()
//                 //             .map(|angle| Affine::skew(angle.to_radians().tan() as f64, 0.0));
//                 //         scene
//                 //             .draw_glyphs(font)
//                 //             // .brush(&style.brush)
//                 //             .hint(true)
//                 //             // .transform(transform)
//                 //             .glyph_transform(glyph_xform)
//                 //             .font_size(font_size)
//                 //             .normalized_coords(run.normalized_coords())
//                 //             .draw(
//                 //                 Fill::NonZero,
//                 //                 glyph_run.glyphs().map(|glyph| {
//                 //                     let gx = x + glyph.x;
//                 //                     let gy = y - glyph.y;
//                 //                     x += glyph.advance;
//                 //                     vello::Glyph {
//                 //                         id: glyph.id as _,
//                 //                         x: gx,
//                 //                         y: gy,
//                 //                     }
//                 //                 }),
//                 //             );
//                 //     }
//                 // }
//                 // renderer
//                 //     .render_to_texture(
//                 //         &self.device,
//                 //         &self.queue,
//                 //         &scene,
//                 //         &text_view,
//                 //         &vello::RenderParams {
//                 //             base_color: palette::css::WHITE, // Background color
//                 //             width: self.size.width,
//                 //             height: self.size.height,
//                 //             antialiasing_method: AaConfig::Msaa16,
//                 //         },
//                 //     )
//                 //     .expect("Failed to render to a texture");
//             }
//             _ => {}
//         }
//     }

//     // TODO: apply texture_view to 3D Mesh
//     blitter.copy(
//         &self.device,
//         &mut encoder,
//         &text_view,
//         &surface_texture_view,
//     );
//     drop(text_view);
//     encoder
// }
