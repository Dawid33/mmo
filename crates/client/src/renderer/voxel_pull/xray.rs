//! The dug-space x-ray: with the cut OFF, every dug space under the surface
//! is seen from the inside, as the cut shows it — the terrain between the
//! camera and the space is not drawn. Spec
//! `docs/superpowers/specs/2026-10-31-cave-xray-design.md`.
//!
//! 1. The mesher marks interior faces (`records::INTERIOR_BIT`: the air they
//!    face is dug, `meshing/interior.rs`) and their bricks (`InteriorFaces`).
//! 2. [`xray_interior_pass`] draws ONLY those bricks' interior quads into a
//!    per-view depth texture ([`ViewXray`]) before the opaque pass: per pixel,
//!    the nearest visible interior surface. It costs what the fortress costs.
//! 3. The forward pipelines' `XRAY` variant discards any non-interior
//!    fragment in front of that depth (keeping a [`CEILING_GHOST`] dither of
//!    it), so the space's floor and far walls show, and lights interior faces
//!    without the sun's shadow and with [`INTERIOR_AMBIENT_GAIN`] more ambient.
//!
//! The shadow pass is untouched (the ground still shades the room — step 3's
//! lighting answers for it). Inactive with the cut on, under a depth prepass
//! (`VOXEL_OCCLUSION=on` writes the occluders' depth first), on the per-chunk
//! Mesh path and the WebGL2 fallback (no records: no interior bricks), and
//! when the F3 panel's "Dug-space x-ray" is off. Picking follows the same
//! rule on the CPU (`game::xray_hit`, [`xray_on`]).

use std::collections::HashSet;

use bevy::{
    camera::{MainPassResolutionOverride, Viewport},
    core_pipeline::core_3d::CORE_3D_DEPTH_FORMAT,
    ecs::{
        query::ROQueryItem,
        system::{
            SystemParamItem,
            lifetimeless::{Read, SRes},
        },
    },
    pbr::{MeshPipelineViewLayoutKey, MeshViewBindGroup, ViewKeyCache},
    prelude::*,
    render::{
        camera::ExtractedCamera,
        extract_resource::ExtractResource,
        render_phase::{PhaseItem, RenderCommand, RenderCommandResult, TrackedRenderPass},
        render_resource::*,
        renderer::{RenderContext, RenderDevice, ViewQuery},
        texture::{CachedTexture, TextureCache},
        view::{ExtractedView, ViewDepthTexture},
    },
};

use super::super::fortress_view::{FortressViewUniform, cutaway_active};
use super::ExtractedChunkQuads;
use super::arena::GpuQuadArena;
use super::gpu::ActiveNearMode;
use super::pipeline::{VoxelPullPipeline, VoxelPullPipelineKey};
use super::prepare::GpuChunkQuads;

/// How much of the terrain in front of a dug space still draws, as an
/// ordered-dither fraction of its pixels (0 = gone; "very slightly
/// translucent"). Mirrored as `XRAY_CEILING_GHOST` in voxel_pull.wgsl.
pub const CEILING_GHOST: f32 = 0.1;
/// How far (world units along the view axis; 16 = one block) a terrain
/// fragment must lie in front of the interior surface at its pixel to count
/// as an occluder, so coplanar and silhouette fragments never flicker.
/// Mirrored as `XRAY_DEPTH_EPSILON`.
pub const DEPTH_EPSILON: f32 = 2.0;
/// Ambient gain on interior faces (they sit in the ground's shadow, so they
/// skip the sun's shadow lookup and take this much more sky light). Like
/// `commander::UNDERGROUND_AMBIENT_GAIN`. Mirrored as `XRAY_INTERIOR_AMBIENT_GAIN`.
pub const INTERIOR_AMBIENT_GAIN: f32 = 2.0;

/// Main-world switch (F3 panel), extracted. On by default: the cut-off view
/// IS the x-ray view.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq, ExtractResource)]
pub struct XraySettings {
    pub enabled: bool,
}

impl Default for XraySettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Whether picking looks through dug spaces: enabled and the cut off (with it
/// on the cut already opens the level).
pub fn xray_on(settings: Option<&XraySettings>, cut: bool) -> bool {
    !cut && settings.is_some_and(|s| s.enabled)
}

/// Render world: the pulled bricks holding interior quads (render entities).
/// Kept from `Changed<ExtractedChunkQuads>` plus a retain over itself, never
/// a walk over every brick.
#[derive(Resource, Default, Debug)]
pub struct InteriorChunks(pub HashSet<Entity>);

/// Whether any view gets the x-ray this frame: enabled, the cut off and some
/// interior brick to open (so the plain pipelines, early-z intact, run
/// whenever there is nothing dug).
pub fn xray_wanted(
    settings: Option<&XraySettings>,
    fortress: Option<&FortressViewUniform>,
    chunks: &InteriorChunks,
) -> bool {
    settings.is_some_and(|s| s.enabled)
        && !fortress.is_some_and(cutaway_active)
        && !chunks.0.is_empty()
}

/// A depth prepass writes the occluders' depth before the forward pass can
/// discard them, so a view running one gets no x-ray.
pub fn view_allows_xray(view_layout_key: MeshPipelineViewLayoutKey) -> bool {
    !view_layout_key.contains(MeshPipelineViewLayoutKey::DEPTH_PREPASS)
}

pub fn track_interior_chunks(
    mut set: ResMut<InteriorChunks>,
    changed: Query<(Entity, &ExtractedChunkQuads), Changed<ExtractedChunkQuads>>,
    alive: Query<&ExtractedChunkQuads>,
) {
    for (entity, quads) in &changed {
        if quads.interior && quads.cave_active {
            set.0.insert(entity);
        } else {
            set.0.remove(&entity);
        }
    }
    // Removed bricks lose `ExtractedChunkQuads` (`extract_removed_chunk_quads`).
    set.0
        .retain(|e| alive.get(*e).is_ok_and(|q| q.interior && q.cave_active));
}

/// One view's x-ray: its interior depth texture, the bind group the forward
/// pass reads it through, and the interior pass's pipeline.
#[derive(Component)]
pub struct ViewXray {
    pub depth: CachedTexture,
    pub bind_group: BindGroup,
    pub pipeline: CachedRenderPipelineId,
}

/// group(2) for views without a [`ViewXray`]: a 1×1 depth texture that is
/// never written (zero = nothing interior: the shader keeps everything).
#[derive(Resource)]
pub struct XrayStandIn {
    pub bind_group: BindGroup,
    _texture: Texture,
}

pub fn init_xray_stand_in(world: &mut World) {
    let Some(pipeline) = world.get_resource::<VoxelPullPipeline>() else {
        return;
    };
    let device = world.resource::<RenderDevice>();
    let texture = device.create_texture(&TextureDescriptor {
        label: Some("voxel_pull_xray_stand_in"),
        size: Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: CORE_3D_DEPTH_FORMAT,
        usage: TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(
        "voxel_pull_xray_stand_in",
        &pipeline.xray_layout,
        &BindGroupEntries::single(&view),
    );
    world.insert_resource(XrayStandIn {
        bind_group,
        _texture: texture,
    });
}

/// A camera view `prepare_xray_views` decides for: whether it has the x-ray
/// now and whether it is the frontend's (which draws no terrain).
type XrayView = (
    Entity,
    &'static ExtractedView,
    &'static ViewDepthTexture,
    Has<ViewXray>,
    Has<crate::session::FrontendCamera>,
);

/// Give each camera view that gets the x-ray its interior depth texture
/// (the size of its depth target, single-sampled), bind group and pipeline;
/// take them from the rest. Same decision as `queue_voxel_pull`.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn prepare_xray_views(
    mut commands: Commands,
    device: Res<RenderDevice>,
    mut textures: ResMut<TextureCache>,
    pipeline: Res<VoxelPullPipeline>,
    mut pipelines: ResMut<SpecializedRenderPipelines<VoxelPullPipeline>>,
    cache: Res<PipelineCache>,
    view_key_cache: Res<ViewKeyCache>,
    settings: Option<Res<XraySettings>>,
    fortress: Option<Res<FortressViewUniform>>,
    chunks: Res<InteriorChunks>,
    views: Query<XrayView>,
    mut warned_prepass: Local<bool>,
) {
    let wanted = xray_wanted(settings.as_deref(), fortress.as_deref(), &chunks);
    for (entity, view, depth, had, frontend) in &views {
        let layout_key = view_key_cache
            .get(&view.retained_view_entity)
            .map(|key| MeshPipelineViewLayoutKey::from(*key))
            // The frontend camera's view draws no terrain, so no x-ray.
            .filter(|_| crate::session::view_draws_world(frontend));
        let Some(layout_key) = layout_key.filter(|k| wanted && view_allows_xray(*k)) else {
            if wanted && layout_key.is_some() && !*warned_prepass {
                *warned_prepass = true;
                warn!(
                    "[xray] the view runs a depth prepass (VOXEL_OCCLUSION=on?): no dug-space x-ray"
                );
            }
            if had {
                commands.entity(entity).remove::<ViewXray>();
            }
            continue;
        };
        let size = depth.texture.size();
        let texture = textures.get(
            &device,
            TextureDescriptor {
                label: Some("voxel_pull_xray_interior_depth"),
                size: Extent3d {
                    width: size.width,
                    height: size.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: CORE_3D_DEPTH_FORMAT,
                usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
        );
        let bind_group = device.create_bind_group(
            "voxel_pull_xray",
            &pipeline.xray_layout,
            &BindGroupEntries::single(&texture.default_view),
        );
        let interior_pipeline = pipelines.specialize(
            &cache,
            &pipeline,
            VoxelPullPipelineKey {
                msaa_samples: 1,
                target_format: view.target_format,
                view_layout_key: layout_key,
                cutaway: false,
                cutout: false,
                xray: false,
                interior_pass: true,
            },
        );
        commands.entity(entity).insert(ViewXray {
            depth: texture,
            bind_group,
            pipeline: interior_pipeline,
        });
    }
}

/// Binds group(2) of the forward pipelines: the view's interior depth, or
/// the stand-in.
pub struct SetXrayBindGroup<const I: usize>;

impl<P: PhaseItem, const I: usize> RenderCommand<P> for SetXrayBindGroup<I> {
    type Param = Option<SRes<XrayStandIn>>;
    type ViewQuery = Option<Read<ViewXray>>;
    type ItemQuery = ();

    fn render<'w>(
        _item: &P,
        xray: ROQueryItem<'w, '_, Self::ViewQuery>,
        _entity: Option<ROQueryItem<'w, '_, Self::ItemQuery>>,
        stand_in: SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let bind_group = match (xray, stand_in) {
            (Some(xray), _) => &xray.bind_group,
            (None, Some(stand_in)) => &stand_in.into_inner().bind_group,
            (None, None) => return RenderCommandResult::Skip,
        };
        pass.set_bind_group(I, bind_group, &[]);
        RenderCommandResult::Success
    }
}

/// The interior depth pass (`Core3d`, before the main opaque pass): clear
/// the view's interior depth to far and draw the interior bricks' opaque
/// ranges with the `INTERIOR_PASS` pipeline, which keeps only interior quads.
pub fn xray_interior_pass(
    world: &World,
    view: ViewQuery<(
        &ViewXray,
        &MeshViewBindGroup,
        &ExtractedCamera,
        Option<&MainPassResolutionOverride>,
    )>,
    mut ctx: RenderContext,
) {
    let (xray, view_bind_group, camera, resolution_override) = view.into_inner();
    if world
        .get_resource::<ActiveNearMode>()
        .is_none_or(|m| *m == ActiveNearMode::Disabled)
    {
        return;
    }
    let (Some(arena), Some(chunks), Some(cache)) = (
        world.get_resource::<GpuQuadArena>(),
        world.get_resource::<InteriorChunks>(),
        world.get_resource::<PipelineCache>(),
    ) else {
        return;
    };
    let (Some(bind_group), Some((quad_indices, _)), Some(pipeline)) = (
        &arena.bind_group,
        &arena.quad_indices,
        cache.get_render_pipeline(xray.pipeline),
    ) else {
        return;
    };
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("voxel_pull_xray_interior"),
        color_attachments: &[],
        depth_stencil_attachment: Some(RenderPassDepthStencilAttachment {
            view: &xray.depth.default_view,
            // Reverse-Z: 0 is the far plane — "no interior here".
            depth_ops: Some(Operations {
                load: LoadOp::Clear(0.0),
                store: StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    if let Some(viewport) =
        Viewport::from_viewport_and_override(camera.viewport.as_ref(), resolution_override)
    {
        pass.set_camera_viewport(&viewport);
    }
    pass.set_render_pipeline(pipeline);
    pass.set_bind_group(0, &view_bind_group.main, &view_bind_group.main_offsets);
    pass.set_bind_group(1, bind_group, &[]);
    pass.set_index_buffer(quad_indices.slice(..), IndexFormat::Uint32);
    for &entity in &chunks.0 {
        let Some(gpu) = world.get::<GpuChunkQuads>(entity) else {
            continue;
        };
        let (first, quads) = gpu.range(false);
        if quads > 0 {
            pass.draw_indexed(
                0..quads * 6,
                (first * 4) as i32,
                gpu.chunk_slot..gpu.chunk_slot + 1,
            );
        }
    }
}
