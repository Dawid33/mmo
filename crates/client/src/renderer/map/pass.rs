//! The map pass (spec §5.4): every visible [`MapView`](super::views::MapView)'s tiles drawn into the UI
//! camera's target, scissored to the view's rect, after post-processing and BEFORE the Slint composite —
//! so the Slint layers (the frame, the buttons, the markers) draw over it and the map's own rect stays a
//! transparent hole in them. One quad per tile, `assets/shaders/map_tile.wgsl` (compiled in): a palette
//! lookup by block, water by depth, hill shading from the height; thematic layers from the world map's
//! one-pixel-per-region raster.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{binding_types as bt, *};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::texture::GpuImage;
use bevy::render::view::ViewTarget;
use bevy::render::camera::ExtractedCamera;
use bevy::render::{Render, RenderApp, RenderSystems};
use worldgen::tile_map::MapLayer;

use super::palette::MapPalette;
use super::views::{MapDraws, MapLayerKind};
use crate::renderer::slint_composite::{pick_ui_view, SlintCompositeSystems, UiCamera};
use crate::renderer::world_map::WorldMapState;

/// `map_tile.wgsl`, compiled in (as the terrain shaders are).
pub const MAP_TILE_SHADER: Handle<Shader> = bevy::asset::uuid_handle!("5d0f3c8e-7a21-4b96-9e4d-2c8b1f6a0e37");

/// The map's background (past the world's edge, and where no tile is loaded yet).
pub const MAP_BACKGROUND_RGB: [f32; 3] = [0.09, 0.10, 0.12];

/// The thematic rasters of the session's world map, one per layer, and where they lie (regions).
#[derive(Resource, Clone, Default, ExtractResource)]
pub struct MapThemes {
    /// `(world map origin x, z, width, height)` the rasters were built for.
    pub frame: Option<(i32, i32, u32, u32)>,
    pub images: Vec<(MapLayer, Handle<Image>)>,
}

/// The palette's texture, for the render world.
#[derive(Resource, Clone, Default, ExtractResource)]
pub struct MapPaletteTexture(pub Option<Handle<Image>>);

pub(super) fn build(app: &mut App) {
    bevy::asset::load_internal_asset!(app, MAP_TILE_SHADER, "../../../../../assets/shaders/map_tile.wgsl", Shader::from_wgsl);
    app.init_resource::<MapThemes>()
        .init_resource::<MapPaletteTexture>()
        .init_resource::<MapDraws>()
        .add_plugins((
            ExtractResourcePlugin::<MapDraws>::default(),
            ExtractResourcePlugin::<MapThemes>::default(),
            ExtractResourcePlugin::<MapPaletteTexture>::default(),
        ))
        .add_systems(PostUpdate, (follow_palette, build_themes));
    let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
    render_app
        .init_resource::<SpecializedRenderPipelines<MapPipeline>>()
        .init_resource::<MapQuadUniforms>()
        .add_systems(Render, prepare_map.in_set(RenderSystems::PrepareBindGroups).run_if(resource_exists::<MapPipeline>))
        .add_systems(Core3d, map_pass.after(Core3dSystems::PostProcess).before(SlintCompositeSystems));
}

pub(super) fn finish(app: &mut App) {
    let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
    render_app.init_resource::<MapPipeline>();
}

fn follow_palette(palette: Option<Res<MapPalette>>, mut texture: ResMut<MapPaletteTexture>) {
    let want = palette.map(|p| p.texture.clone());
    if texture.0 != want {
        texture.0 = want;
    }
}

/// The world map's thematic rasters (1 px per region), rebuilt when the map changes.
fn build_themes(state: Res<WorldMapState>, mut themes: ResMut<MapThemes>, mut images: ResMut<Assets<Image>>) {
    let Some(map) = state.map.as_ref() else {
        if themes.frame.is_some() {
            *themes = MapThemes::default();
        }
        return;
    };
    let frame = (map.origin.x, map.origin.z, map.width, map.height);
    if themes.frame == Some(frame) {
        return;
    }
    let mut built = Vec::new();
    for layer in MapLayer::ALL {
        let (w, h, px) = worldgen::tile_map::rasterize_layer(map, layer);
        let mut image = Image::new(
            Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
            TextureDimension::D2,
            px.iter().flatten().copied().collect(),
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        );
        image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor::nearest());
        built.push((layer, images.add(image)));
    }
    for (_, old) in std::mem::take(&mut themes.images) {
        images.remove(&old);
    }
    *themes = MapThemes { frame: Some(frame), images: built };
}

/// Per quad: where it goes and what of its tile it shows. LOCKED to `map_tile.wgsl`'s `Quad`.
#[derive(Clone, Copy, ShaderType)]
struct QuadUniform {
    /// Screen rect (physical px): min x, min y, max x, max y.
    rect: Vec4,
    /// Sub-rect of the tile, in samples.
    uv: Vec4,
    /// The target's size (px), then the tile's min block (x, z).
    target_origin: Vec4,
    /// Metres per sample of the tile, metres per pixel of the view, the layer (0 = surface, 1+ = theme),
    /// region blocks.
    params: Vec4,
    /// The theme raster's frame: origin region x, z, width, height.
    theme_frame: Vec4,
}

#[derive(Resource, Default)]
struct MapQuadUniforms {
    buffer: DynamicUniformBuffer<QuadUniform>,
    bind_group: Option<BindGroup>,
    /// Per view draw: scissor (x, y, w, h), then per quad its uniform offset and texture bind group
    /// key (`None`: the blank set — the view's background quad).
    views: Vec<([u32; 4], Vec<(u32, Option<[AssetId<Image>; 5]>)>)>,
    textures: HashMap<[AssetId<Image>; 5], ([TextureViewId; 5], BindGroup)>,
    /// 1×1 stand-ins for every texture slot (an absent theme raster, the background quad), and their
    /// bind group.
    blank: Option<Blank>,
}

struct Blank {
    _textures: Vec<Texture>,
    views: Vec<TextureView>,
    bind_group: BindGroup,
}

fn make_blank(device: &RenderDevice, layout: &BindGroupLayout) -> Blank {
    let formats = [
        TextureFormat::R16Sint,
        TextureFormat::R16Uint,
        TextureFormat::Rg8Uint,
        TextureFormat::Rgba8UnormSrgb,
        TextureFormat::Rgba8UnormSrgb,
    ];
    let textures: Vec<Texture> = formats
        .iter()
        .map(|&format| {
            device.create_texture(&TextureDescriptor {
                label: Some("map_blank"),
                size: Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format,
                usage: TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        })
        .collect();
    let views: Vec<TextureView> = textures.iter().map(|t| t.create_view(&TextureViewDescriptor::default())).collect();
    let bind_group = device.create_bind_group(
        "map_blank_textures",
        layout,
        &BindGroupEntries::sequential((&views[0], &views[1], &views[2], &views[3], &views[4])),
    );
    Blank { _textures: textures, views, bind_group }
}

#[derive(Resource)]
pub struct MapPipeline {
    quad_layout: BindGroupLayout,
    quad_layout_desc: BindGroupLayoutDescriptor,
    texture_layout: BindGroupLayout,
    texture_layout_desc: BindGroupLayoutDescriptor,
}

impl FromWorld for MapPipeline {
    fn from_world(world: &mut World) -> Self {
        let device = world.resource::<RenderDevice>();
        let quad_entries =
            BindGroupLayoutEntries::single(ShaderStages::VERTEX_FRAGMENT, bt::uniform_buffer::<QuadUniform>(true));
        // LOCKED to map_tile.wgsl group(1): height (i32), block (u32), water+flags (u32), palette, theme.
        let texture_entries = BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                bt::texture_2d(TextureSampleType::Sint),
                bt::texture_2d(TextureSampleType::Uint),
                bt::texture_2d(TextureSampleType::Uint),
                bt::texture_2d(TextureSampleType::Float { filterable: false }),
                bt::texture_2d(TextureSampleType::Float { filterable: false }),
            ),
        );
        MapPipeline {
            quad_layout: device.create_bind_group_layout("map_quad_layout", &quad_entries),
            quad_layout_desc: BindGroupLayoutDescriptor::new("map_quad_layout", &quad_entries),
            texture_layout: device.create_bind_group_layout("map_texture_layout", &texture_entries),
            texture_layout_desc: BindGroupLayoutDescriptor::new("map_texture_layout", &texture_entries),
        }
    }
}

impl SpecializedRenderPipeline for MapPipeline {
    type Key = TextureFormat;

    fn specialize(&self, format: TextureFormat) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some("map_tile".into()),
            layout: vec![self.quad_layout_desc.clone(), self.texture_layout_desc.clone()],
            vertex: VertexState { shader: MAP_TILE_SHADER, entry_point: Some("vertex".into()), buffers: vec![], ..default() },
            fragment: Some(FragmentState {
                shader: MAP_TILE_SHADER,
                entry_point: Some("fragment".into()),
                targets: vec![Some(ColorTargetState { format, blend: None, write_mask: ColorWrites::ALL })],
                ..default()
            }),
            ..default()
        }
    }
}

/// The view the map is drawn onto (the UI camera's) and its pipeline.
#[derive(Component)]
struct ViewMapPipeline(CachedRenderPipelineId);

#[allow(clippy::too_many_arguments)]
fn prepare_map(
    mut commands: Commands,
    pipeline: Res<MapPipeline>,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<MapPipeline>>,
    views: Query<(Entity, &ViewTarget, Has<UiCamera>, Option<&ExtractedCamera>)>,
    draws: Res<MapDraws>,
    themes: Res<MapThemes>,
    palette: Res<MapPaletteTexture>,
    images: Res<RenderAssets<GpuImage>>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut uniforms: ResMut<MapQuadUniforms>,
) {
    let ui_view = pick_ui_view(views.iter().filter(|v| v.2).map(|(e, _, _, c)| (e, c.map_or(0, |c| c.order))));
    let mut target_size = Vec2::ZERO;
    for (entity, target, _, camera) in &views {
        if Some(entity) == ui_view && !draws.0.is_empty() {
            let id = pipelines.specialize(&pipeline_cache, &pipeline, target.main_texture_format());
            commands.entity(entity).insert(ViewMapPipeline(id));
            target_size = camera.and_then(|c| c.physical_target_size).map_or(Vec2::ZERO, |s| s.as_vec2());
        } else {
            commands.entity(entity).remove::<ViewMapPipeline>();
        }
    }
    let u = &mut *uniforms;
    u.views.clear();
    u.buffer.clear();
    let Some(palette) = palette.0.as_ref().map(|h| h.id()) else { return };
    if draws.0.is_empty() || target_size == Vec2::ZERO {
        return;
    }
    if u.blank.is_none() {
        u.blank = Some(make_blank(&device, &pipeline.texture_layout));
    }
    let theme_frame = themes.frame.map_or(Vec4::ZERO, |(x, z, w, h)| Vec4::new(x as f32, z as f32, w as f32, h as f32));
    for draw in &draws.0 {
        let (layer_index, theme) = match draw.layer {
            MapLayerKind::Surface => (0.0, None),
            MapLayerKind::Theme(l) => (
                1.0 + MapLayer::ALL.iter().position(|x| *x == l).unwrap_or(0) as f32,
                themes.images.iter().find(|(x, _)| *x == l).map(|(_, h)| h.id()),
            ),
        };
        // The scissor, clipped to the target.
        let s = draw.scissor.intersect(Rect::from_corners(Vec2::ZERO, target_size));
        if s.is_empty() {
            continue;
        }
        let scissor = [s.min.x as u32, s.min.y as u32, (s.width() as u32).max(1), (s.height() as u32).max(1)];
        let mut quads = Vec::new();
        // The background first: wherever no tile is loaded yet, and past the world's edge.
        let background = u.buffer.push(&QuadUniform {
            rect: Vec4::new(s.min.x, s.min.y, s.max.x, s.max.y),
            uv: Vec4::ZERO,
            target_origin: Vec4::new(target_size.x, target_size.y, 0.0, 0.0),
            params: Vec4::new(1.0, draw.m_per_px, -1.0, game::REGION_BLOCKS as f32),
            theme_frame,
        });
        quads.push((background, None));
        for q in &draw.quads {
            let key = [
                q.tile.height.id(),
                q.tile.block.id(),
                q.tile.water_flags.id(),
                palette,
                theme.unwrap_or(AssetId::default()),
            ];
            let offset = u.buffer.push(&QuadUniform {
                rect: Vec4::new(q.screen.min.x, q.screen.min.y, q.screen.max.x, q.screen.max.y),
                uv: Vec4::new(q.uv.min.x, q.uv.min.y, q.uv.max.x, q.uv.max.y),
                target_origin: Vec4::new(target_size.x, target_size.y, q.origin.x, q.origin.y),
                params: Vec4::new(q.sample_m, draw.m_per_px, layer_index, game::REGION_BLOCKS as f32),
                theme_frame,
            });
            quads.push((offset, Some(key)));
        }
        u.views.push((scissor, quads));
    }
    u.buffer.write_buffer(&device, &queue);
    let Some(binding) = u.buffer.binding() else { return };
    u.bind_group = Some(device.create_bind_group("map_quads", &pipeline.quad_layout, &BindGroupEntries::single(binding)));
    // Texture bind groups: made for new texture sets, dropped when no quad uses them.
    let used: std::collections::HashSet<[AssetId<Image>; 5]> =
        u.views.iter().flat_map(|(_, q)| q.iter().filter_map(|(_, k)| *k)).collect();
    u.textures.retain(|k, _| used.contains(k));
    let blank_theme = u.blank.as_ref().map(|b| b.views[4].clone()).expect("made above");
    for key in used {
        let views: Option<Vec<TextureView>> = key
            .iter()
            .enumerate()
            .map(|(i, id)| {
                if i == 4 && *id == AssetId::default() {
                    Some(blank_theme.clone())
                } else {
                    images.get(*id).map(|g| g.texture_view.clone())
                }
            })
            .collect();
        let Some(views) = views else { continue };
        let ids: [TextureViewId; 5] = std::array::from_fn(|i| views[i].id());
        if u.textures.get(&key).is_some_and(|(have, _)| *have == ids) {
            continue;
        }
        let bind_group = device.create_bind_group(
            "map_textures",
            &pipeline.texture_layout,
            &BindGroupEntries::sequential((&views[0], &views[1], &views[2], &views[3], &views[4])),
        );
        u.textures.insert(key, (ids, bind_group));
    }
}

fn map_pass(
    view: ViewQuery<(&ViewTarget, &ViewMapPipeline)>,
    uniforms: Res<MapQuadUniforms>,
    pipeline_cache: Res<PipelineCache>,
    mut ctx: RenderContext,
) {
    let (target, pipeline) = view.into_inner();
    let Some(pipeline) = pipeline_cache.get_render_pipeline(pipeline.0) else { return };
    let Some(quad_group) = uniforms.bind_group.as_ref() else { return };
    if uniforms.views.is_empty() {
        return;
    }
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("map_tiles"),
        color_attachments: &[Some(target.get_unsampled_color_attachment())],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(pipeline);
    for (scissor, quads) in &uniforms.views {
        pass.set_scissor_rect(scissor[0], scissor[1], scissor[2], scissor[3]);
        for (offset, key) in quads {
            let textures = match key {
                Some(key) => match uniforms.textures.get(key) {
                    Some((_, group)) => group,
                    None => continue,
                },
                None => match uniforms.blank.as_ref() {
                    Some(blank) => &blank.bind_group,
                    None => continue,
                },
            };
            pass.set_bind_group(0, quad_group, &[*offset]);
            pass.set_bind_group(1, textures, &[]);
            pass.draw(0..6, 0..1);
        }
    }
}
