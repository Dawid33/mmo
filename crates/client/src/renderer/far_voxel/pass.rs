use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    Arc,
};

use bevy::{
    camera::{primitives::Frustum, MainPassResolutionOverride, Viewport},
    core_pipeline::{
        core_3d::graph::{Core3d, Node3d},
        prepass::{DeferredPrepass, DepthPrepass, MotionVectorPrepass, NormalPrepass},
    },
    ecs::{query::QueryItem, world::World},
    pbr::{
        MeshPipelineViewLayoutKey, MeshViewBindGroup, ViewEnvironmentMapUniformOffset,
        ViewFogUniformOffset, ViewLightProbesUniformOffset, ViewLightsUniformOffset,
        ViewScreenSpaceReflectionsUniformOffset,
    },
    prelude::*,
    render::{
        diagnostic::RecordDiagnostics,
        render_asset::RenderAssets,
        render_graph::{
            NodeRunError, RenderGraphContext, RenderGraphExt, RenderLabel, ViewNode, ViewNodeRunner,
        },
        render_resource::*,
        renderer::{RenderContext, RenderDevice, RenderQueue},
        texture::GpuImage,
        view::{ExtractedView, Msaa, ViewDepthTexture, ViewTarget, ViewUniformOffset},
        Extract, ExtractSchedule, Render, RenderSystems,
    },
};
use bytemuck::{Pod, Zeroable};

use super::{
    gpu::{apply_far_uploads, GpuFarBuffers},
    pipeline::{
        empty_proxy_draw_args, encode_material_lut, material_lut_bytes, FarVoxelPipeline,
        FarVoxelPipelineKey, FAR_CULL_WORKGROUP_SIZE, MATERIAL_LUT_EDGE,
    },
};
use crate::renderer::{
    block_textures::BlockTextureLayers,
    config::{FarRenderBackend, TerrainRenderConfig},
    voxel_pull::BlockArrayTexture,
};

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct GpuFarCullUniform {
    pub planes: [Vec4; 6],
    pub cell_capacity: u32,
    pub padding: [u32; 3],
}

impl GpuFarCullUniform {
    pub fn new(frustum: &Frustum, cell_capacity: u32) -> Self {
        Self {
            planes: frustum.half_spaces.map(|plane| plane.normal_d()),
            cell_capacity,
            padding: [0; 3],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FarGraphPoint {
    StartMainPass,
    FarVoxelPass,
    MainOpaquePass,
}

pub const FAR_PASS_GRAPH_ORDER: [FarGraphPoint; 3] = [
    FarGraphPoint::StartMainPass,
    FarGraphPoint::FarVoxelPass,
    FarGraphPoint::MainOpaquePass,
];

pub const fn pipelines_ready(cull_ready: bool, render_ready: bool) -> bool {
    cull_ready && render_ready
}

pub const fn cull_dispatch_count(cell_capacity: u32) -> u32 {
    cell_capacity.div_ceil(FAR_CULL_WORKGROUP_SIZE)
}

pub const fn should_promote_readiness(already_ready: bool, pipelines_are_ready: bool) -> bool {
    !already_ready && pipelines_are_ready
}

pub const fn far_view_allocation_bytes(visible_slot_capacity: u32) -> usize {
    let allocated_slots = if visible_slot_capacity == 0 {
        1
    } else {
        visible_slot_capacity
    };
    std::mem::size_of::<GpuFarCullUniform>()
        + std::mem::size_of::<[u32; 4]>()
        + allocated_slots as usize * std::mem::size_of::<u32>()
}

pub fn far_pass_allocation_bytes(
    material_lookup_ready: bool,
    visible_slot_capacities: impl IntoIterator<Item = u32>,
) -> usize {
    let lookup_bytes = if material_lookup_ready {
        material_lut_bytes()
    } else {
        0
    };
    lookup_bytes
        + visible_slot_capacities
            .into_iter()
            .map(far_view_allocation_bytes)
            .sum::<usize>()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FarPassStatus {
    pub statically_supported: bool,
    pub pipelines_ready: bool,
    pub visible_slot_capacity: u32,
    pub reserved_gpu_bytes: usize,
}

#[derive(Default)]
struct SharedFarPassStatus {
    statically_supported: AtomicBool,
    pipelines_ready: AtomicBool,
    visible_slot_capacity: AtomicU32,
    reserved_gpu_bytes: AtomicUsize,
}

#[derive(Resource, Clone, Default)]
pub struct FarPassStatusChannel(Arc<SharedFarPassStatus>);

impl FarPassStatusChannel {
    pub fn snapshot(&self) -> FarPassStatus {
        FarPassStatus {
            statically_supported: self.0.statically_supported.load(Ordering::Acquire),
            pipelines_ready: self.0.pipelines_ready.load(Ordering::Acquire),
            visible_slot_capacity: self.0.visible_slot_capacity.load(Ordering::Acquire),
            reserved_gpu_bytes: self.0.reserved_gpu_bytes.load(Ordering::Acquire),
        }
    }

    pub fn publish_static_device_support(&self, supported: bool) {
        self.0
            .statically_supported
            .store(supported, Ordering::Release);
    }

    pub fn publish_render_status(
        &self,
        pipelines_ready: bool,
        visible_slot_capacity: u32,
        reserved_gpu_bytes: usize,
    ) {
        if !pipelines_ready {
            self.0.pipelines_ready.store(false, Ordering::Release);
        }
        self.0
            .visible_slot_capacity
            .store(visible_slot_capacity, Ordering::Release);
        self.0
            .reserved_gpu_bytes
            .store(reserved_gpu_bytes, Ordering::Release);
        if pipelines_ready {
            self.0.pipelines_ready.store(true, Ordering::Release);
        }
    }
}

#[derive(Resource, Clone, Copy)]
struct RequestedFarBackend(FarRenderBackend);

#[derive(Resource, Clone, Default)]
struct ExtractedFarMaterialLayers(Option<BlockTextureLayers>);

#[derive(Resource, Default)]
pub struct GpuFarMaterialLookup {
    texture: Option<Texture>,
    view: Option<TextureView>,
}

impl GpuFarMaterialLookup {
    pub fn reserved_bytes(&self) -> usize {
        self.view.as_ref().map_or(0, |_| material_lut_bytes())
    }
}

#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FarVoxelPassMetrics {
    pub active_cells: u32,
    pub indirect_draws: u32,
    pub visible_slot_capacity: u32,
    pub reserved_gpu_bytes: usize,
}

/// Shared main/render-world switch used by the benchmark harness to keep the
/// expensive far pass off while immutable scene data is still streaming. The
/// production renderer defaults to enabled.
#[derive(Resource, Clone)]
pub struct FarVoxelDrawGate(Arc<AtomicBool>);

impl FarVoxelDrawGate {
    pub fn disabled() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn enable(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn enabled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl Default for FarVoxelDrawGate {
    fn default() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }
}

#[derive(Component)]
pub struct FarVoxelViewResources {
    capacity: u32,
    cull_uniform: Buffer,
    visible_slots: Buffer,
    indirect: Buffer,
    cull_bind_group: BindGroup,
    far_bind_group: BindGroup,
    render_pipeline: CachedRenderPipelineId,
}

type FarVoxelViewQuery = (
    Entity,
    &'static ExtractedView,
    &'static Frustum,
    &'static Msaa,
    Has<DepthPrepass>,
    Has<NormalPrepass>,
    Has<MotionVectorPrepass>,
    Has<DeferredPrepass>,
    Option<&'static mut FarVoxelViewResources>,
);

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
pub struct FarVoxelPassLabel;

fn extract_far_pass_inputs(
    mut commands: Commands,
    config: Extract<Res<TerrainRenderConfig>>,
    layers: Extract<Option<Res<BlockTextureLayers>>>,
) {
    commands.insert_resource(RequestedFarBackend(config.far));
    commands.insert_resource(ExtractedFarMaterialLayers(
        layers.as_ref().map(|layers| (*layers).clone()),
    ));
}

fn prepare_far_material_lookup(
    extracted: Res<ExtractedFarMaterialLayers>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut gpu: ResMut<GpuFarMaterialLookup>,
) {
    if gpu.view.is_some() {
        return;
    }
    let Some(layers) = extracted.0.as_ref() else {
        return;
    };
    let encoded = encode_material_lut(layers);
    let texture = device.create_texture_with_data(
        &queue,
        &TextureDescriptor {
            label: Some("far_voxel_material_layer_lut"),
            size: Extent3d {
                width: MATERIAL_LUT_EDGE as u32,
                height: MATERIAL_LUT_EDGE as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba16Uint,
            usage: TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        TextureDataOrder::LayerMajor,
        bytemuck::cast_slice(&encoded),
    );
    let view = texture.create_view(&TextureViewDescriptor::default());
    gpu.texture = Some(texture);
    gpu.view = Some(view);
}

#[allow(clippy::too_many_arguments)]
fn prepare_far_voxel_views(
    mut commands: Commands,
    requested: Res<RequestedFarBackend>,
    pipeline: Res<FarVoxelPipeline>,
    mut pipelines: ResMut<SpecializedRenderPipelines<FarVoxelPipeline>>,
    cache: Res<PipelineCache>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    buffers: Res<GpuFarBuffers>,
    lookup: Res<GpuFarMaterialLookup>,
    images: Res<RenderAssets<GpuImage>>,
    block_texture: Option<Res<BlockArrayTexture>>,
    mut views: Query<FarVoxelViewQuery>,
) {
    if requested.0 != FarRenderBackend::BrickRaymarch {
        for (entity, ..) in &mut views {
            commands.entity(entity).remove::<FarVoxelViewResources>();
        }
        return;
    }
    let (Some(material_view), Some(block_texture), Some(cell_buffer), Some(root_buffer)) = (
        lookup.view.as_ref(),
        block_texture.as_ref(),
        buffers.cell_buffer(),
        buffers.root_buffer(),
    ) else {
        return;
    };
    let Some(block_image) = images.get(&block_texture.0) else {
        return;
    };
    let (Some(page_0), Some(page_1)) = (buffers.mixed_page_buffer(0), buffers.mixed_page_buffer(1))
    else {
        return;
    };
    let capacity = buffers.cell_capacity();
    for (
        entity,
        view,
        frustum,
        msaa,
        depth_prepass,
        normal_prepass,
        motion_prepass,
        deferred_prepass,
        existing,
    ) in &mut views
    {
        let mut view_layout_key = MeshPipelineViewLayoutKey::from(*msaa);
        view_layout_key.set(MeshPipelineViewLayoutKey::DEPTH_PREPASS, depth_prepass);
        view_layout_key.set(MeshPipelineViewLayoutKey::NORMAL_PREPASS, normal_prepass);
        view_layout_key.set(
            MeshPipelineViewLayoutKey::MOTION_VECTOR_PREPASS,
            motion_prepass,
        );
        view_layout_key.set(
            MeshPipelineViewLayoutKey::DEFERRED_PREPASS,
            deferred_prepass,
        );
        let render_pipeline = pipelines.specialize(
            &cache,
            &pipeline,
            FarVoxelPipelineKey {
                msaa_samples: msaa.samples(),
                hdr: view.hdr,
                view_layout_key,
            },
        );

        let must_allocate = existing
            .as_ref()
            .is_none_or(|resources| resources.capacity != capacity);
        let (cull_uniform, visible_slots, indirect) = if must_allocate {
            let visible_slots = device.create_buffer(&BufferDescriptor {
                label: Some("far_voxel_visible_slots"),
                size: u64::from(capacity.max(1)) * std::mem::size_of::<u32>() as u64,
                usage: BufferUsages::STORAGE,
                mapped_at_creation: false,
            });
            let indirect = device.create_buffer_with_data(&BufferInitDescriptor {
                label: Some("far_voxel_indirect"),
                contents: bytemuck::cast_slice(&empty_proxy_draw_args()),
                usage: BufferUsages::STORAGE | BufferUsages::INDIRECT | BufferUsages::COPY_DST,
            });
            let cull_uniform = device.create_buffer_with_data(&BufferInitDescriptor {
                label: Some("far_voxel_cull_uniform"),
                contents: bytemuck::bytes_of(&GpuFarCullUniform::new(frustum, capacity)),
                usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            });
            (cull_uniform, visible_slots, indirect)
        } else {
            let resources = existing.as_ref().expect("checked above");
            (
                resources.cull_uniform.clone(),
                resources.visible_slots.clone(),
                resources.indirect.clone(),
            )
        };
        queue.write_buffer(
            &cull_uniform,
            0,
            bytemuck::bytes_of(&GpuFarCullUniform::new(frustum, capacity)),
        );
        queue.write_buffer(&indirect, 0, bytemuck::cast_slice(&empty_proxy_draw_args()));
        let cull_bind_group = device.create_bind_group(
            "far_voxel_cull",
            &pipeline.cull_layout,
            &BindGroupEntries::sequential((
                cull_uniform.as_entire_binding(),
                cell_buffer.as_entire_binding(),
                visible_slots.as_entire_binding(),
                indirect.as_entire_binding(),
            )),
        );
        let far_bind_group = device.create_bind_group(
            "far_voxel_render",
            &pipeline.far_layout,
            &BindGroupEntries::sequential((
                cell_buffer.as_entire_binding(),
                root_buffer.as_entire_binding(),
                page_0.as_entire_binding(),
                page_1.as_entire_binding(),
                visible_slots.as_entire_binding(),
                material_view,
                &block_image.texture_view,
                &block_image.sampler,
            )),
        );
        commands.entity(entity).insert(FarVoxelViewResources {
            capacity,
            cull_uniform,
            visible_slots,
            indirect,
            cull_bind_group,
            far_bind_group,
            render_pipeline,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_far_pipeline_readiness(
    requested: Res<RequestedFarBackend>,
    pipeline: Res<FarVoxelPipeline>,
    cache: Res<PipelineCache>,
    views: Query<&FarVoxelViewResources>,
    mut buffers: ResMut<GpuFarBuffers>,
    lookup: Res<GpuFarMaterialLookup>,
    mut metrics: ResMut<FarVoxelPassMetrics>,
    channel: Res<FarPassStatusChannel>,
) {
    let visible_slot_capacity = views.iter().map(|view| view.capacity).max().unwrap_or(0);
    let pass_reserved_gpu_bytes = far_pass_allocation_bytes(
        lookup.view.is_some(),
        views.iter().map(|view| view.capacity),
    );
    let pass_within_budget = buffers.set_pass_reserved_bytes(pass_reserved_gpu_bytes);
    let reserved_gpu_bytes = buffers.reserved_bytes();
    let cull_ready = cache.get_compute_pipeline(pipeline.cull_pipeline).is_some();
    let render_ready = !views.is_empty()
        && views
            .iter()
            .all(|view| cache.get_render_pipeline(view.render_pipeline).is_some());
    let ready = requested.0 == FarRenderBackend::BrickRaymarch
        && pipelines_ready(cull_ready, render_ready)
        && pass_within_budget;
    metrics.active_cells = buffers.active_cell_count();
    metrics.indirect_draws = if ready && metrics.active_cells > 0 {
        views.iter().count().min(u32::MAX as usize) as u32
    } else {
        0
    };
    metrics.visible_slot_capacity = visible_slot_capacity;
    metrics.reserved_gpu_bytes = reserved_gpu_bytes;
    channel.publish_render_status(ready, visible_slot_capacity, reserved_gpu_bytes);
}

#[derive(Default)]
pub struct FarVoxelPassNode;

impl ViewNode for FarVoxelPassNode {
    type ViewQuery = (
        &'static bevy::render::camera::ExtractedCamera,
        &'static ViewTarget,
        &'static ViewDepthTexture,
        &'static MeshViewBindGroup,
        &'static ViewUniformOffset,
        &'static ViewLightsUniformOffset,
        &'static ViewFogUniformOffset,
        &'static ViewLightProbesUniformOffset,
        &'static ViewScreenSpaceReflectionsUniformOffset,
        &'static ViewEnvironmentMapUniformOffset,
        Option<&'static MainPassResolutionOverride>,
        &'static FarVoxelViewResources,
    );

    fn run<'w>(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext<'w>,
        (
            camera,
            target,
            depth,
            mesh_view_bind_group,
            view_uniform,
            view_lights,
            view_fog,
            view_light_probes,
            view_ssr,
            view_environment_map,
            resolution_override,
            resources,
        ): QueryItem<'w, '_, Self::ViewQuery>,
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        if !world.resource::<FarVoxelDrawGate>().enabled() {
            return Ok(());
        }
        let buffers = world.resource::<GpuFarBuffers>();
        if resources.capacity == 0 || buffers.active_cell_count() == 0 {
            return Ok(());
        }
        let pipeline = world.resource::<FarVoxelPipeline>();
        let cache = world.resource::<PipelineCache>();
        let (Some(cull_pipeline), Some(render_pipeline)) = (
            cache.get_compute_pipeline(pipeline.cull_pipeline),
            cache.get_render_pipeline(resources.render_pipeline),
        ) else {
            return Ok(());
        };
        let diagnostics = render_context.diagnostic_recorder();
        {
            let mut pass =
                render_context
                    .command_encoder()
                    .begin_compute_pass(&ComputePassDescriptor {
                        label: Some("far_voxel_cull"),
                        timestamp_writes: None,
                    });
            let pass_span = diagnostics.pass_span(&mut pass, "far_voxel_cull");
            pass.set_pipeline(cull_pipeline);
            pass.set_bind_group(0, &resources.cull_bind_group, &[]);
            pass.dispatch_workgroups(cull_dispatch_count(resources.capacity), 1, 1);
            pass_span.end(&mut pass);
        }

        let mut pass = render_context.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("far_voxel_pass"),
            color_attachments: &[Some(target.get_color_attachment())],
            depth_stencil_attachment: Some(depth.get_attachment(StoreOp::Store)),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        let pass_span = diagnostics.pass_span(&mut pass, "far_voxel_pass");
        if let Some(viewport) =
            Viewport::from_viewport_and_override(camera.viewport.as_ref(), resolution_override)
        {
            pass.set_camera_viewport(&viewport);
        }
        pass.set_render_pipeline(render_pipeline);
        pass.set_bind_group(
            0,
            &mesh_view_bind_group.main,
            &[
                view_uniform.offset,
                view_lights.offset,
                view_fog.offset,
                **view_light_probes,
                **view_ssr,
                **view_environment_map,
            ],
        );
        pass.set_bind_group(1, &resources.far_bind_group, &[]);
        pass.draw_indirect(&resources.indirect, 0);
        pass_span.end(&mut pass);
        Ok(())
    }
}

pub fn install_render_pass(render_app: &mut SubApp, channel: FarPassStatusChannel) {
    render_app
        .insert_resource(channel)
        .init_resource::<FarVoxelPipeline>()
        .init_resource::<SpecializedRenderPipelines<FarVoxelPipeline>>()
        .init_resource::<ExtractedFarMaterialLayers>()
        .init_resource::<GpuFarMaterialLookup>()
        .init_resource::<FarVoxelPassMetrics>()
        .init_resource::<FarVoxelDrawGate>()
        .add_systems(ExtractSchedule, extract_far_pass_inputs)
        .add_systems(
            Render,
            (
                prepare_far_material_lookup.in_set(RenderSystems::PrepareResources),
                prepare_far_voxel_views
                    .in_set(RenderSystems::PrepareBindGroups)
                    .after(apply_far_uploads)
                    .after(prepare_far_material_lookup),
                publish_far_pipeline_readiness
                    .in_set(RenderSystems::PrepareBindGroups)
                    .after(prepare_far_voxel_views),
            ),
        )
        .add_render_graph_node::<ViewNodeRunner<FarVoxelPassNode>>(Core3d, FarVoxelPassLabel)
        .add_render_graph_edges(
            Core3d,
            (
                Node3d::StartMainPass,
                FarVoxelPassLabel,
                Node3d::MainOpaquePass,
            ),
        );
}

#[cfg(test)]
mod tests {
    use bevy::{camera::primitives::HalfSpace, prelude::*};

    use super::*;

    #[test]
    fn benchmark_draw_gate_starts_disabled_and_enables_once() {
        assert!(FarVoxelDrawGate::default().enabled());
        let gate = FarVoxelDrawGate::disabled();
        assert!(!gate.enabled());
        gate.enable();
        assert!(gate.enabled());
    }

    #[test]
    fn cull_uniform_copies_world_planes_and_cell_capacity() {
        let frustum = bevy::camera::primitives::Frustum {
            half_spaces: std::array::from_fn(|index| {
                HalfSpace::new(Vec4::new(1.0, 0.0, 0.0, index as f32 + 1.0))
            }),
        };

        let uniform = GpuFarCullUniform::new(&frustum, 73);

        assert_eq!(std::mem::size_of::<GpuFarCullUniform>(), 112);
        assert_eq!(uniform.cell_capacity, 73);
        for index in 0..6 {
            assert_eq!(uniform.planes[index], frustum.half_spaces[index].normal_d());
        }
    }

    #[test]
    fn graph_contract_places_far_pass_between_start_and_main_opaque() {
        assert_eq!(
            FAR_PASS_GRAPH_ORDER,
            [
                FarGraphPoint::StartMainPass,
                FarGraphPoint::FarVoxelPass,
                FarGraphPoint::MainOpaquePass,
            ]
        );
    }

    #[test]
    fn pipeline_readiness_requires_both_cull_and_render_pipelines() {
        assert!(!pipelines_ready(false, false));
        assert!(!pipelines_ready(true, false));
        assert!(!pipelines_ready(false, true));
        assert!(pipelines_ready(true, true));
    }

    #[test]
    fn dispatch_count_covers_capacity_without_empty_workgroup() {
        assert_eq!(cull_dispatch_count(0), 0);
        assert_eq!(cull_dispatch_count(1), 1);
        assert_eq!(cull_dispatch_count(64), 1);
        assert_eq!(cull_dispatch_count(65), 2);
    }

    #[test]
    fn readiness_transition_requeues_fallback_cells_only_once() {
        assert!(!should_promote_readiness(false, false));
        assert!(should_promote_readiness(false, true));
        assert!(!should_promote_readiness(true, true));
    }

    #[test]
    fn pass_allocation_bytes_include_lookup_and_each_view_buffer() {
        assert_eq!(far_view_allocation_bytes(0), 112 + 16 + 4);
        assert_eq!(far_view_allocation_bytes(73), 112 + 16 + 73 * 4);
        assert_eq!(far_pass_allocation_bytes(false, []), 0);
        assert_eq!(
            far_pass_allocation_bytes(true, [0, 73]),
            material_lut_bytes() + far_view_allocation_bytes(0) + far_view_allocation_bytes(73)
        );
    }

    #[test]
    fn shared_status_reports_static_and_live_readiness_separately() {
        let channel = FarPassStatusChannel::default();
        assert_eq!(channel.snapshot(), FarPassStatus::default());

        channel.publish_static_device_support(true);
        assert_eq!(
            channel.snapshot(),
            FarPassStatus {
                statically_supported: true,
                ..default()
            }
        );

        channel.publish_render_status(true, 73, 900);
        assert_eq!(
            channel.snapshot(),
            FarPassStatus {
                statically_supported: true,
                pipelines_ready: true,
                visible_slot_capacity: 73,
                reserved_gpu_bytes: 900,
            }
        );
    }
}
