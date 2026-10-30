//! Slint layers on screen. Each [`SlintLayerPlugin`](super::slint_overlay::SlintLayerPlugin)
//! software-renders into a window-sized `Image`; this pass draws every such
//! image, lowest order first, over the camera marked [`UiCamera`] — in the
//! slot `bevy_ui` drew in (after post-processing, before upscaling), with
//! Bevy's own blit pipeline: a straight-alpha sample, alpha-blended into the
//! main texture, the same as `bevy_ui`'s `ImageNode`.
use std::collections::HashMap;

use bevy::core_pipeline::blit::{BlitPipeline, BlitPipelineKey};
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::core_pipeline::upscaling::upscaling;
use bevy::prelude::*;
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    BindGroup, BlendState, CachedRenderPipelineId, PipelineCache, RenderPassDescriptor,
    SpecializedRenderPipelines, TextureViewId,
};
use bevy::render::renderer::{RenderContext, ViewQuery};
use bevy::render::texture::GpuImage;
use bevy::render::view::ViewTarget;
use bevy::render::{Extract, ExtractSchedule, Render, RenderApp, RenderSystems};

/// The camera the UI is drawn over — the local player's active camera (moved
/// by `bridge::enforce_single_active_camera`) or the commander camera. At most
/// one is active at a time; with none, no UI is drawn.
#[derive(Component, Clone, Copy, Default, Debug, ExtractComponent)]
pub struct UiCamera;

/// Every Slint layer's texture and its order (higher draws on top). Each
/// layer pushes itself at startup.
#[derive(Resource, Default, Clone)]
pub struct SlintLayerTextures(pub Vec<(i32, Handle<Image>)>);

/// Layers in draw order: ascending, ties kept in the order they were added.
pub fn sorted_layers(layers: &[(i32, AssetId<Image>)]) -> Vec<AssetId<Image>> {
    let mut sorted = layers.to_vec();
    sorted.sort_by_key(|(order, _)| *order); // stable
    sorted.into_iter().map(|(_, id)| id).collect()
}

/// A view gets the composite only when it is the UI camera and there is
/// something to draw.
pub fn wants_composite(is_ui_camera: bool, layers: usize) -> bool {
    is_ui_camera && layers > 0
}

/// Render world: the layers to draw this frame, in order.
#[derive(Resource, Default)]
struct ExtractedSlintLayers(Vec<AssetId<Image>>);

#[derive(Component)]
struct ViewSlintComposite(CachedRenderPipelineId, BlitPipelineKey);

pub struct SlintCompositePlugin;

impl Plugin for SlintCompositePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SlintLayerTextures>()
            .add_plugins(ExtractComponentPlugin::<UiCamera>::default());
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_resource::<ExtractedSlintLayers>()
            .add_systems(ExtractSchedule, extract_slint_layers)
            .add_systems(
                Render,
                prepare_slint_composite.in_set(RenderSystems::Prepare).ambiguous_with_all(),
            )
            .add_systems(
                Core3d,
                slint_composite_pass.after(Core3dSystems::PostProcess).before(upscaling),
            );
    }
}

fn extract_slint_layers(
    layers: Extract<Option<Res<SlintLayerTextures>>>,
    mut extracted: ResMut<ExtractedSlintLayers>,
) {
    let pairs: Vec<(i32, AssetId<Image>)> =
        layers.iter().flat_map(|l| l.0.iter()).map(|(order, h)| (*order, h.id())).collect();
    extracted.0 = sorted_layers(&pairs);
}

fn prepare_slint_composite(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<BlitPipeline>>,
    blit_pipeline: Res<BlitPipeline>,
    layers: Res<ExtractedSlintLayers>,
    views: Query<(Entity, &ViewTarget, Has<UiCamera>, Option<&ViewSlintComposite>)>,
) {
    for (entity, target, is_ui_camera, current) in &views {
        if !wants_composite(is_ui_camera, layers.0.len()) {
            if current.is_some() {
                commands.entity(entity).remove::<ViewSlintComposite>();
            }
            continue;
        }
        let key = BlitPipelineKey {
            target_format: target.main_texture_format(),
            blend_state: Some(BlendState::ALPHA_BLENDING),
            samples: 1,
            source_space: None,
        };
        if current.is_none_or(|ViewSlintComposite(_, k)| *k != key) {
            let id = pipelines.specialize(&pipeline_cache, &blit_pipeline, key);
            commands.entity(entity).insert(ViewSlintComposite(id, key));
        }
    }
}

/// One bind group per layer texture view, rebuilt when a layer's texture is
/// replaced (it is on every window resize).
#[derive(Default)]
struct BindGroupCache(HashMap<AssetId<Image>, (TextureViewId, BindGroup)>);

fn slint_composite_pass(
    view: ViewQuery<(&ViewTarget, &ViewSlintComposite, Option<&ExtractedCamera>)>,
    layers: Res<ExtractedSlintLayers>,
    images: Res<RenderAssets<GpuImage>>,
    pipeline_cache: Res<PipelineCache>,
    blit_pipeline: Res<BlitPipeline>,
    mut cache: Local<BindGroupCache>,
    mut ctx: RenderContext,
) {
    let (target, composite, camera) = view.into_inner();
    let Some(pipeline) = pipeline_cache.get_render_pipeline(composite.0) else { return };

    cache.0.retain(|id, _| layers.0.contains(id));
    for id in &layers.0 {
        let Some(image) = images.get(*id) else { continue };
        let fresh = cache.0.get(id).is_some_and(|(view_id, _)| *view_id == image.texture_view.id());
        if !fresh {
            let bind_group =
                blit_pipeline.create_bind_group(ctx.render_device(), &image.texture_view, &pipeline_cache);
            cache.0.insert(*id, (image.texture_view.id(), bind_group));
        }
    }

    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("slint_composite"),
        color_attachments: &[Some(target.get_unsampled_color_attachment())],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    if let Some(viewport) = camera.and_then(|c| c.viewport.as_ref()) {
        pass.set_camera_viewport(viewport);
    }
    pass.set_render_pipeline(pipeline);
    for id in &layers.0 {
        let Some((_, bind_group)) = cache.0.get(id) else { continue };
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> AssetId<Image> {
        AssetId::Uuid { uuid: bevy::asset::uuid::Uuid::from_u128(n) }
    }

    #[test]
    fn layers_draw_in_ascending_order_stable_on_ties() {
        let (a, b, c) = (id(1), id(2), id(3));
        assert_eq!(sorted_layers(&[(1000, a), (0, b), (0, c)]), vec![b, c, a]);
    }

    #[test]
    fn no_ui_camera_or_no_layers_composites_nothing() {
        assert!(!wants_composite(false, 2));
        assert!(!wants_composite(true, 0));
        assert!(wants_composite(true, 1));
    }
}
