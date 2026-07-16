use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc,
};

use bevy::ecs::system::lifetimeless::Read;
use bevy::{
    camera::primitives::Frustum,
    core_pipeline::{core_3d::graph::Node3d, experimental::mip_generation::ViewDepthPyramid},
    pbr::{graph::NodePbr, ViewLightEntities},
    prelude::*,
    render::{
        batching::gpu_preprocessing::GpuPreprocessingSupport,
        render_graph::{Node, NodeRunError, RenderGraph, RenderGraphContext, RenderLabel},
        render_resource::{binding_types as bt, *},
        renderer::{RenderAdapterInfo, RenderContext, RenderDevice, RenderQueue},
        settings::WgpuFeatures,
        view::ExtractedView,
    },
};
use crossbeam::channel::{unbounded, Receiver, Sender};

use super::arena::GpuNearChunkMeta;
use super::queue::chunk_in_view;
use crate::renderer::config::NearRenderBackend;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuNearCapabilities {
    pub gpu_culling: bool,
    pub compute: bool,
    pub indirect_first_instance: bool,
    pub multi_draw_indirect: bool,
    pub multi_draw_indirect_count: bool,
}

#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActiveNearMode {
    Disabled,
    #[default]
    GroupedDirect,
    GpuDriven {
        indirect_count: bool,
    },
}

#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestedNearBackend(pub NearRenderBackend);

#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NearRendererMetrics {
    pub active_chunks: u32,
    pub frustum_visible_chunks: u32,
    /// Exact main-forward HZB survivor count. `None` when HZB was not the
    /// forward cull source or benchmark-only asynchronous readback is disabled.
    pub hzb_visible_chunks: Option<u32>,
    /// Main-forward indirect command records submitted. Indirect-count mode
    /// submits the GPU survivor count; fixed mode submits every stable slot,
    /// including rejected slots whose command has zero vertices. Direct modes
    /// submit no indirect records.
    pub indirect_draw_count: Option<u32>,
    pub resident_quads: u64,
    pub submitted_quads: Option<u64>,
    pub submitted_vertices: Option<u64>,
}

/// Presence enables near count-buffer readback. Only `RenderBenchmarkPlugin`
/// inserts this marker into the render world.
#[derive(Resource, Default)]
pub struct NearTelemetryEnabled;

pub fn resolve_near_mode(
    requested: NearRenderBackend,
    capabilities: GpuNearCapabilities,
) -> ActiveNearMode {
    match requested {
        NearRenderBackend::PerChunk => ActiveNearMode::Disabled,
        NearRenderBackend::GroupedDirect => ActiveNearMode::GroupedDirect,
        NearRenderBackend::GpuDriven
            if capabilities.gpu_culling
                && capabilities.compute
                && capabilities.indirect_first_instance
                && capabilities.multi_draw_indirect =>
        {
            ActiveNearMode::GpuDriven {
                indirect_count: capabilities.multi_draw_indirect_count,
            }
        }
        NearRenderBackend::GpuDriven => ActiveNearMode::GroupedDirect,
    }
}

pub const fn gate_near_mode_on_pipeline_readiness(
    resolved: ActiveNearMode,
    all_compute_pipelines_ready: bool,
) -> ActiveNearMode {
    if matches!(resolved, ActiveNearMode::GpuDriven { .. }) && !all_compute_pipelines_ready {
        ActiveNearMode::GroupedDirect
    } else {
        resolved
    }
}

/// Returns the exact CPU-known main-forward indirect submission count. GPU
/// modes require per-view telemetry because count mode uses survivors while
/// fixed mode uses that view's current slot high-water count.
pub const fn exact_indirect_draw_count(mode: ActiveNearMode) -> Option<u32> {
    match mode {
        ActiveNearMode::Disabled | ActiveNearMode::GroupedDirect => Some(0),
        ActiveNearMode::GpuDriven { .. } => None,
    }
}

pub fn runtime_capabilities(
    support: &GpuPreprocessingSupport,
    device: &RenderDevice,
    adapter: &RenderAdapterInfo,
) -> GpuNearCapabilities {
    let features = device.features();
    GpuNearCapabilities {
        gpu_culling: support.is_culling_supported(),
        compute: device.limits().max_compute_workgroup_size_x > 0,
        indirect_first_instance: features.contains(WgpuFeatures::INDIRECT_FIRST_INSTANCE),
        // wgpu 27 exposes fixed-count multi-draw on every backend (native or
        // backend-emulated); only the count-buffer variant remains a feature.
        multi_draw_indirect: true,
        // Match Bevy's 0.18 binned-phase guard for wgpu#7974.
        multi_draw_indirect_count: features.contains(WgpuFeatures::MULTI_DRAW_INDIRECT_COUNT)
            && adapter.backend.to_str() != "dx12",
    }
}

pub fn extract_requested_near_backend(
    mut commands: Commands,
    config: bevy::render::Extract<Option<Res<crate::renderer::config::TerrainRenderConfig>>>,
) {
    if let Some(config) = config.as_deref() {
        commands.insert_resource(RequestedNearBackend(config.near));
    }
}

pub fn prepare_active_near_mode(
    request: Option<Res<RequestedNearBackend>>,
    support: Res<GpuPreprocessingSupport>,
    device: Res<RenderDevice>,
    adapter: Res<RenderAdapterInfo>,
    cull_pipeline: Option<Res<GpuNearCullPipeline>>,
    pipeline_cache: Res<PipelineCache>,
    mut active: ResMut<ActiveNearMode>,
) {
    let requested = request
        .as_deref()
        .map_or(NearRenderBackend::GroupedDirect, |request| request.0);
    let capability_mode =
        resolve_near_mode(requested, runtime_capabilities(&support, &device, &adapter));
    let pipelines_ready = cull_pipeline
        .as_deref()
        .is_some_and(|pipeline| pipeline.all_compiled(&pipeline_cache));
    let resolved = gate_near_mode_on_pipeline_readiness(capability_mode, pipelines_ready);
    if *active != resolved {
        if requested == NearRenderBackend::GpuDriven
            && capability_mode == ActiveNearMode::GroupedDirect
        {
            warn!("VOXEL_NEAR_RENDER=gpu is unsupported on this device; using grouped-direct");
        } else if requested == NearRenderBackend::GpuDriven
            && resolved == ActiveNearMode::GroupedDirect
        {
            debug!("near terrain GPU pipelines are compiling; using grouped-direct");
        } else {
            info!("near terrain renderer: {resolved:?}");
        }
        *active = resolved;
    }
}

/// WebGPU's non-indexed indirect draw ABI.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DrawIndirectArgs {
    pub vertex_count: u32,
    pub instance_count: u32,
    pub first_vertex: u32,
    pub first_instance: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuNearCullView {
    clip_from_world: [f32; 16],
    viewport_and_capacity: [u32; 4],
}

fn resolve_clip_from_world(
    explicit: Option<Mat4>,
    clip_from_view: Mat4,
    world_from_view: Mat4,
) -> Mat4 {
    explicit.unwrap_or_else(|| clip_from_view * world_from_view.inverse())
}

impl GpuNearCullView {
    fn new(view: &ExtractedView, chunk_capacity: u32, hzb_mip_count: u32) -> Self {
        let clip_from_world = resolve_clip_from_world(
            view.clip_from_world,
            view.clip_from_view,
            view.world_from_view.to_matrix(),
        );
        Self {
            clip_from_world: clip_from_world.to_cols_array(),
            viewport_and_capacity: [
                view.viewport.z,
                view.viewport.w,
                chunk_capacity,
                hzb_mip_count,
            ],
        }
    }
}

#[derive(Component)]
pub struct GpuNearViewBuffers {
    pub indirect: Buffer,
    pub count: Buffer,
    uniform: Buffer,
    frustum_bind_group: BindGroup,
    hzb_bind_group: Option<BindGroup>,
    /// Number of chunk slots dispatched/drawn this frame.
    pub slot_count: u32,
    /// Geometrically grown command-buffer capacity.
    pub capacity: u32,
    metadata_generation: u64,
}

#[derive(Resource)]
pub struct GpuNearCullPipeline {
    frustum_layout: BindGroupLayout,
    hzb_layout: BindGroupLayout,
    frustum_fixed_pipeline: CachedComputePipelineId,
    frustum_count_pipeline: CachedComputePipelineId,
    hzb_fixed_pipeline: CachedComputePipelineId,
    hzb_count_pipeline: CachedComputePipelineId,
}

impl GpuNearCullPipeline {
    fn all_compiled(&self, cache: &PipelineCache) -> bool {
        [
            self.frustum_fixed_pipeline,
            self.frustum_count_pipeline,
            self.hzb_fixed_pipeline,
            self.hzb_count_pipeline,
        ]
        .into_iter()
        .all(|id| cache.get_compute_pipeline(id).is_some())
    }
}

#[derive(Clone, Copy)]
struct GpuNearBufferSizes {
    slot_count: u32,
    capacity: u32,
}

impl FromWorld for GpuNearCullPipeline {
    fn from_world(world: &mut World) -> Self {
        let device = world.resource::<RenderDevice>();
        let frustum_entries = BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (
                bt::storage_buffer_read_only_sized(false, None),
                bt::storage_buffer_sized(false, None),
                bt::storage_buffer_sized(false, None),
                bt::uniform_buffer_sized(false, None),
            ),
        );
        let hzb_entries = BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (
                bt::storage_buffer_read_only_sized(false, None),
                bt::storage_buffer_sized(false, None),
                bt::storage_buffer_sized(false, None),
                bt::uniform_buffer_sized(false, None),
                bt::texture_2d(TextureSampleType::Float { filterable: false }),
            ),
        );
        let frustum_layout =
            device.create_bind_group_layout("voxel_pull_frustum_cull", &frustum_entries);
        let hzb_layout = device.create_bind_group_layout("voxel_pull_hzb_cull", &hzb_entries);
        let frustum_layout_desc =
            BindGroupLayoutDescriptor::new("voxel_pull_frustum_cull", &frustum_entries);
        let hzb_layout_desc = BindGroupLayoutDescriptor::new("voxel_pull_hzb_cull", &hzb_entries);
        let shader = world.load_asset("shaders/voxel_pull_cull.wgsl");
        let cache = world.resource::<PipelineCache>();
        let frustum_fixed_pipeline = cache.queue_compute_pipeline(ComputePipelineDescriptor {
            label: Some("voxel_pull_frustum_cull".into()),
            layout: vec![frustum_layout_desc],
            shader: shader.clone(),
            entry_point: Some("frustum_cull".into()),
            ..default()
        });
        let frustum_count_pipeline = cache.queue_compute_pipeline(ComputePipelineDescriptor {
            label: Some("voxel_pull_frustum_cull_count".into()),
            layout: vec![BindGroupLayoutDescriptor::new(
                "voxel_pull_frustum_cull",
                &frustum_entries,
            )],
            shader: shader.clone(),
            shader_defs: vec!["INDIRECT_COUNT".into()],
            entry_point: Some("frustum_cull".into()),
            ..default()
        });
        let hzb_fixed_pipeline = cache.queue_compute_pipeline(ComputePipelineDescriptor {
            label: Some("voxel_pull_hzb_cull".into()),
            layout: vec![hzb_layout_desc.clone()],
            shader: shader.clone(),
            shader_defs: vec!["HZB".into()],
            entry_point: Some("hzb_cull".into()),
            ..default()
        });
        let hzb_count_pipeline = cache.queue_compute_pipeline(ComputePipelineDescriptor {
            label: Some("voxel_pull_hzb_cull_count".into()),
            layout: vec![hzb_layout_desc],
            shader,
            shader_defs: vec!["HZB".into(), "INDIRECT_COUNT".into()],
            entry_point: Some("hzb_cull".into()),
            ..default()
        });
        Self {
            frustum_layout,
            hzb_layout,
            frustum_fixed_pipeline,
            frustum_count_pipeline,
            hzb_fixed_pipeline,
            hzb_count_pipeline,
        }
    }
}

fn create_view_buffers(
    device: &RenderDevice,
    pipeline: &GpuNearCullPipeline,
    metadata: BindingResource<'_>,
    metadata_generation: u64,
    uniform: GpuNearCullView,
    depth_pyramid: Option<&ViewDepthPyramid>,
    sizes: GpuNearBufferSizes,
) -> GpuNearViewBuffers {
    let command_bytes =
        u64::from(sizes.capacity.max(1)) * std::mem::size_of::<DrawIndirectArgs>() as u64;
    let indirect = device.create_buffer(&BufferDescriptor {
        label: Some("voxel_pull_indirect_commands"),
        size: command_bytes,
        usage: BufferUsages::STORAGE | BufferUsages::INDIRECT | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let count = device.create_buffer(&BufferDescriptor {
        label: Some("voxel_pull_indirect_count"),
        size: std::mem::size_of::<[u32; 2]>() as u64,
        usage: BufferUsages::STORAGE
            | BufferUsages::INDIRECT
            | BufferUsages::COPY_DST
            | BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let uniform_buffer = device.create_buffer_with_data(&BufferInitDescriptor {
        label: Some("voxel_pull_cull_view"),
        contents: bytemuck::bytes_of(&uniform),
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
    });
    let frustum_bind_group = device.create_bind_group(
        "voxel_pull_frustum_cull",
        &pipeline.frustum_layout,
        &BindGroupEntries::sequential((
            metadata.clone(),
            indirect.as_entire_binding(),
            count.as_entire_binding(),
            uniform_buffer.as_entire_binding(),
        )),
    );
    let hzb_bind_group = depth_pyramid.map(|depth_pyramid| {
        device.create_bind_group(
            "voxel_pull_hzb_cull",
            &pipeline.hzb_layout,
            &BindGroupEntries::sequential((
                metadata,
                indirect.as_entire_binding(),
                count.as_entire_binding(),
                uniform_buffer.as_entire_binding(),
                &depth_pyramid.all_mips,
            )),
        )
    });
    GpuNearViewBuffers {
        indirect,
        count,
        uniform: uniform_buffer,
        frustum_bind_group,
        hzb_bind_group,
        slot_count: sizes.slot_count,
        capacity: sizes.capacity,
        metadata_generation,
    }
}

pub fn geometric_gpu_capacity(required: u32) -> u32 {
    required
        .max(1)
        .checked_next_power_of_two()
        .unwrap_or(required)
}

pub fn prepare_gpu_near_view_buffers(
    mut commands: Commands,
    mode: Res<ActiveNearMode>,
    pipeline: Res<GpuNearCullPipeline>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    arena: Res<super::arena::GpuQuadArena>,
    mut views: Query<(
        Entity,
        &ExtractedView,
        Option<&ViewDepthPyramid>,
        Option<&mut GpuNearViewBuffers>,
    )>,
) {
    if !matches!(*mode, ActiveNearMode::GpuDriven { .. }) {
        return;
    }
    let Some(metadata) = arena.metadata.binding() else {
        return;
    };
    let slot_count = arena.slot_allocator.high_water();
    if slot_count == 0 {
        return;
    }
    let capacity = geometric_gpu_capacity(slot_count);
    for (entity, view, depth_pyramid, existing) in &mut views {
        let uniform = GpuNearCullView::new(
            view,
            slot_count,
            depth_pyramid.map_or(0, |pyramid| pyramid.mip_count),
        );
        if let Some(mut existing) = existing {
            if existing.capacity == capacity
                && existing.metadata_generation == arena.metadata_generation
            {
                queue.write_buffer(&existing.uniform, 0, bytemuck::bytes_of(&uniform));
                existing.slot_count = slot_count;
                existing.hzb_bind_group = depth_pyramid.map(|depth_pyramid| {
                    device.create_bind_group(
                        "voxel_pull_hzb_cull",
                        &pipeline.hzb_layout,
                        &BindGroupEntries::sequential((
                            metadata.clone(),
                            existing.indirect.as_entire_binding(),
                            existing.count.as_entire_binding(),
                            existing.uniform.as_entire_binding(),
                            &depth_pyramid.all_mips,
                        )),
                    )
                });
                continue;
            }
        }
        commands.entity(entity).insert(create_view_buffers(
            &device,
            &pipeline,
            metadata.clone(),
            arena.metadata_generation,
            uniform,
            depth_pyramid,
            GpuNearBufferSizes {
                slot_count,
                capacity,
            },
        ));
    }
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
pub enum NearGpuNode {
    FrustumCull,
    ClearLatePhases,
    HzbCull,
    TelemetryCopy,
}

/// Installs the ordering contract for the shared near-terrain indirect buffers.
/// Frustum results feed only the early depth/shadow consumers. Both consumers
/// must finish before the buffers are cleared; both late consumers must observe
/// that clear. HZB then repopulates only the main-view buffer for the forward
/// pass. Benchmark readback, when enabled, copies the final count before the
/// main pass consumes it.
pub fn add_near_gpu_graph_edges(graph: &mut RenderGraph, telemetry_enabled: bool) {
    graph.add_node_edge(NearGpuNode::FrustumCull, Node3d::EarlyPrepass);
    graph.add_node_edge(NearGpuNode::FrustumCull, NodePbr::EarlyShadowPass);
    graph.add_node_edge(Node3d::EarlyDownsampleDepth, NearGpuNode::ClearLatePhases);
    graph.add_node_edge(NodePbr::EarlyShadowPass, NearGpuNode::ClearLatePhases);
    graph.add_node_edge(NearGpuNode::ClearLatePhases, Node3d::LatePrepass);
    graph.add_node_edge(NearGpuNode::ClearLatePhases, NodePbr::LateShadowPass);
    graph.add_node_edge(Node3d::LatePrepass, NearGpuNode::HzbCull);
    if telemetry_enabled {
        graph.add_node_edge(NearGpuNode::HzbCull, NearGpuNode::TelemetryCopy);
        graph.add_node_edge(NearGpuNode::TelemetryCopy, Node3d::StartMainPass);
    } else {
        graph.add_node_edge(NearGpuNode::HzbCull, Node3d::StartMainPass);
    }
}

fn dispatch_cull(
    render_context: &mut RenderContext<'_>,
    pipeline_cache: &PipelineCache,
    pipeline_id: CachedComputePipelineId,
    buffers: &GpuNearViewBuffers,
    bind_group: &BindGroup,
    label: &'static str,
) {
    let Some(pipeline) = pipeline_cache.get_compute_pipeline(pipeline_id) else {
        return;
    };
    render_context
        .command_encoder()
        .clear_buffer(&buffers.count, 0, None);
    let mut pass = render_context
        .command_encoder()
        .begin_compute_pass(&ComputePassDescriptor {
            label: Some(label),
            ..default()
        });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.dispatch_workgroups(buffers.slot_count.div_ceil(64), 1, 1);
}

pub struct NearFrustumCullNode {
    view_query: QueryState<(Read<GpuNearViewBuffers>, Option<Read<ViewLightEntities>>)>,
    light_view_query: QueryState<Read<GpuNearViewBuffers>>,
}

impl FromWorld for NearFrustumCullNode {
    fn from_world(world: &mut World) -> Self {
        Self {
            view_query: QueryState::new(world),
            light_view_query: QueryState::new(world),
        }
    }
}

impl Node for NearFrustumCullNode {
    fn update(&mut self, world: &mut World) {
        self.view_query.update_archetypes(world);
        self.light_view_query.update_archetypes(world);
    }

    fn run<'w>(
        &self,
        graph: &mut RenderGraphContext,
        render_context: &mut RenderContext<'w>,
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        if !matches!(
            *world.resource::<ActiveNearMode>(),
            ActiveNearMode::GpuDriven { .. }
        ) {
            return Ok(());
        }
        let pipeline = world.resource::<GpuNearCullPipeline>();
        let cache = world.resource::<PipelineCache>();
        let indirect_count = matches!(
            *world.resource::<ActiveNearMode>(),
            ActiveNearMode::GpuDriven {
                indirect_count: true
            }
        );
        let pipeline_id = if indirect_count {
            pipeline.frustum_count_pipeline
        } else {
            pipeline.frustum_fixed_pipeline
        };
        let Ok((buffers, light_views)) = self.view_query.get_manual(world, graph.view_entity())
        else {
            return Ok(());
        };
        dispatch_cull(
            render_context,
            cache,
            pipeline_id,
            buffers,
            &buffers.frustum_bind_group,
            "voxel_pull_frustum_cull",
        );
        if let Some(light_views) = light_views {
            for light_view in light_views.lights.iter().copied() {
                if let Ok(buffers) = self.light_view_query.get_manual(world, light_view) {
                    dispatch_cull(
                        render_context,
                        cache,
                        pipeline_id,
                        buffers,
                        &buffers.frustum_bind_group,
                        "voxel_pull_shadow_frustum_cull",
                    );
                }
            }
        }
        Ok(())
    }
}

fn clear_cull_arguments(render_context: &mut RenderContext<'_>, buffers: &GpuNearViewBuffers) {
    let encoder = render_context.command_encoder();
    encoder.clear_buffer(&buffers.indirect, 0, None);
    encoder.clear_buffer(&buffers.count, 0, None);
}

/// Clears the shared phase item's arguments after both early consumers. Bevy
/// executes the same binned prepass/shadow phase again in its late passes, so
/// leaving the frustum commands resident would draw every near chunk twice.
pub struct NearLatePhaseClearNode {
    view_query: QueryState<(Read<GpuNearViewBuffers>, Option<Read<ViewLightEntities>>)>,
    light_view_query: QueryState<Read<GpuNearViewBuffers>>,
}

impl FromWorld for NearLatePhaseClearNode {
    fn from_world(world: &mut World) -> Self {
        Self {
            view_query: QueryState::new(world),
            light_view_query: QueryState::new(world),
        }
    }
}

impl Node for NearLatePhaseClearNode {
    fn update(&mut self, world: &mut World) {
        self.view_query.update_archetypes(world);
        self.light_view_query.update_archetypes(world);
    }

    fn run<'w>(
        &self,
        graph: &mut RenderGraphContext,
        render_context: &mut RenderContext<'w>,
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        if !matches!(
            *world.resource::<ActiveNearMode>(),
            ActiveNearMode::GpuDriven { .. }
        ) {
            return Ok(());
        }
        let Ok((buffers, light_views)) = self.view_query.get_manual(world, graph.view_entity())
        else {
            return Ok(());
        };
        clear_cull_arguments(render_context, buffers);
        if let Some(light_views) = light_views {
            for light_view in light_views.lights.iter().copied() {
                if let Ok(buffers) = self.light_view_query.get_manual(world, light_view) {
                    clear_cull_arguments(render_context, buffers);
                }
            }
        }
        Ok(())
    }
}

pub struct NearHzbCullNode {
    view_query: QueryState<Read<GpuNearViewBuffers>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardCullSource {
    Frustum,
    Hzb,
}

pub const fn forward_cull_source(hzb_available: bool) -> ForwardCullSource {
    if hzb_available {
        ForwardCullSource::Hzb
    } else {
        ForwardCullSource::Frustum
    }
}

const NEAR_TELEMETRY_RING_SIZE: usize = 6;
const NEAR_TELEMETRY_STAGING_BYTES: u64 = 8;
const NEAR_TELEMETRY_COUNT_BYTES: u64 = std::mem::size_of::<[u32; 2]>() as u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NearTelemetrySample {
    sequence: u64,
    visible_chunks: u32,
    visible_quads: u32,
    source: ForwardCullSource,
    indirect_count: bool,
    slot_count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NearTelemetryValues {
    hzb_visible_chunks: Option<u32>,
    indirect_draw_count: u32,
    submitted_quads: u64,
    submitted_vertices: u64,
}

const fn near_telemetry_values(sample: NearTelemetrySample) -> NearTelemetryValues {
    NearTelemetryValues {
        hzb_visible_chunks: match sample.source {
            ForwardCullSource::Hzb => Some(sample.visible_chunks),
            ForwardCullSource::Frustum => None,
        },
        indirect_draw_count: if sample.indirect_count {
            sample.visible_chunks
        } else {
            sample.slot_count
        },
        submitted_quads: sample.visible_quads as u64,
        submitted_vertices: (sample.visible_quads as u64).saturating_mul(6),
    }
}

fn apply_near_telemetry_sample(
    mode: ActiveNearMode,
    sample: Option<NearTelemetrySample>,
    metrics: &mut NearRendererMetrics,
) {
    if matches!(mode, ActiveNearMode::GpuDriven { .. }) {
        // Mapping is asynchronous and does not necessarily complete every
        // render frame. Retain the newest completed sample until a newer one
        // arrives instead of exposing one-frame pulses that the main-world
        // benchmark can easily miss.
        if let Some(sample) = sample {
            let values = near_telemetry_values(sample);
            metrics.hzb_visible_chunks = values.hzb_visible_chunks;
            metrics.indirect_draw_count = Some(values.indirect_draw_count);
            metrics.submitted_quads = Some(values.submitted_quads);
            metrics.submitted_vertices = Some(values.submitted_vertices);
        }
    } else {
        metrics.hzb_visible_chunks = None;
        metrics.indirect_draw_count = exact_indirect_draw_count(mode);
    }
}

struct NearTelemetryReservation {
    slot_index: usize,
    sequence: u64,
    busy: Arc<AtomicBool>,
}

impl NearTelemetryReservation {
    fn release(self) {
        self.busy.store(false, Ordering::Release);
    }
}

/// Arbitrates a bounded staging ring and elects the first valid Core3d view
/// each frame. All operations are nonblocking because graph nodes only receive
/// shared access to render-world resources.
struct NearTelemetryArbiter {
    busy: Vec<Arc<AtomicBool>>,
    next_slot: AtomicUsize,
    next_sequence: AtomicU64,
    frame_claimed: AtomicBool,
}

impl NearTelemetryArbiter {
    fn new(slot_count: usize) -> Self {
        Self {
            busy: (0..slot_count)
                .map(|_| Arc::new(AtomicBool::new(false)))
                .collect(),
            next_slot: AtomicUsize::new(0),
            next_sequence: AtomicU64::new(1),
            frame_claimed: AtomicBool::new(false),
        }
    }

    fn begin_frame(&self) {
        self.frame_claimed.store(false, Ordering::Release);
    }

    fn try_reserve(&self) -> Option<NearTelemetryReservation> {
        if self.frame_claimed.swap(true, Ordering::AcqRel) || self.busy.is_empty() {
            return None;
        }
        let start = self.next_slot.fetch_add(1, Ordering::Relaxed) % self.busy.len();
        for offset in 0..self.busy.len() {
            let slot_index = (start + offset) % self.busy.len();
            let busy = &self.busy[slot_index];
            if busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(NearTelemetryReservation {
                    slot_index,
                    sequence: self.next_sequence.fetch_add(1, Ordering::Relaxed),
                    busy: busy.clone(),
                });
            }
        }
        None
    }
}

#[derive(Default)]
struct NearTelemetryLatest {
    highest_sequence: u64,
    pending: Option<NearTelemetrySample>,
}

impl NearTelemetryLatest {
    fn accept(&mut self, sample: NearTelemetrySample) -> bool {
        if self.highest_sequence >= sample.sequence {
            return false;
        }
        self.highest_sequence = sample.sequence;
        self.pending = Some(sample);
        true
    }

    fn take(&mut self) -> Option<NearTelemetrySample> {
        self.pending.take()
    }
}

struct NearTelemetrySlot {
    staging: Buffer,
}

#[derive(Resource)]
pub struct NearTelemetryReadback {
    slots: Vec<NearTelemetrySlot>,
    arbiter: NearTelemetryArbiter,
    sender: Sender<NearTelemetrySample>,
    receiver: Receiver<NearTelemetrySample>,
    latest: NearTelemetryLatest,
}

impl FromWorld for NearTelemetryReadback {
    fn from_world(world: &mut World) -> Self {
        let device = world.resource::<RenderDevice>();
        let slots = (0..NEAR_TELEMETRY_RING_SIZE)
            .map(|_| NearTelemetrySlot {
                staging: device.create_buffer(&BufferDescriptor {
                    label: Some("voxel_pull_near_telemetry_readback"),
                    size: NEAR_TELEMETRY_STAGING_BYTES,
                    usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
            })
            .collect();
        let (sender, receiver) = unbounded();
        Self {
            slots,
            arbiter: NearTelemetryArbiter::new(NEAR_TELEMETRY_RING_SIZE),
            sender,
            receiver,
            latest: NearTelemetryLatest::default(),
        }
    }
}

pub fn begin_near_telemetry_frame(telemetry: Res<NearTelemetryReadback>) {
    telemetry.arbiter.begin_frame();
}

/// Drains asynchronous callbacks and retains the newest completed sequence.
/// Frames without a newly completed readback keep the previous exact sample.
pub fn drain_near_telemetry_results(
    mode: Option<Res<ActiveNearMode>>,
    telemetry: Option<ResMut<NearTelemetryReadback>>,
    metrics: Option<ResMut<NearRendererMetrics>>,
) {
    let (Some(mode), Some(mut telemetry), Some(mut metrics)) = (mode, telemetry, metrics) else {
        return;
    };
    while let Ok(sample) = telemetry.receiver.try_recv() {
        telemetry.latest.accept(sample);
    }
    let sample = telemetry.latest.take();
    apply_near_telemetry_sample(*mode, sample, &mut metrics);
}

impl FromWorld for NearHzbCullNode {
    fn from_world(world: &mut World) -> Self {
        Self {
            view_query: QueryState::new(world),
        }
    }
}

impl Node for NearHzbCullNode {
    fn update(&mut self, world: &mut World) {
        self.view_query.update_archetypes(world);
    }

    fn run<'w>(
        &self,
        graph: &mut RenderGraphContext,
        render_context: &mut RenderContext<'w>,
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        if !matches!(
            *world.resource::<ActiveNearMode>(),
            ActiveNearMode::GpuDriven { .. }
        ) {
            return Ok(());
        }
        let Ok(buffers) = self.view_query.get_manual(world, graph.view_entity()) else {
            return Ok(());
        };
        let pipeline = world.resource::<GpuNearCullPipeline>();
        let indirect_count = matches!(
            *world.resource::<ActiveNearMode>(),
            ActiveNearMode::GpuDriven {
                indirect_count: true
            }
        );
        let (pipeline_id, bind_group, label) =
            match forward_cull_source(buffers.hzb_bind_group.is_some()) {
                ForwardCullSource::Hzb => (
                    if indirect_count {
                        pipeline.hzb_count_pipeline
                    } else {
                        pipeline.hzb_fixed_pipeline
                    },
                    buffers
                        .hzb_bind_group
                        .as_ref()
                        .expect("HZB source requires an HZB bind group"),
                    "voxel_pull_hzb_cull",
                ),
                ForwardCullSource::Frustum => (
                    if indirect_count {
                        pipeline.frustum_count_pipeline
                    } else {
                        pipeline.frustum_fixed_pipeline
                    },
                    &buffers.frustum_bind_group,
                    "voxel_pull_forward_frustum_cull",
                ),
            };
        dispatch_cull(
            render_context,
            world.resource::<PipelineCache>(),
            pipeline_id,
            buffers,
            bind_group,
            label,
        );
        Ok(())
    }
}

pub struct NearTelemetryCopyNode {
    view_query: QueryState<Read<GpuNearViewBuffers>>,
}

impl FromWorld for NearTelemetryCopyNode {
    fn from_world(world: &mut World) -> Self {
        Self {
            view_query: QueryState::new(world),
        }
    }
}

impl Node for NearTelemetryCopyNode {
    fn update(&mut self, world: &mut World) {
        self.view_query.update_archetypes(world);
    }

    fn run<'w>(
        &self,
        graph: &mut RenderGraphContext,
        render_context: &mut RenderContext<'w>,
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        let Some(telemetry) = world.get_resource::<NearTelemetryReadback>() else {
            return Ok(());
        };
        let ActiveNearMode::GpuDriven { indirect_count } = *world.resource::<ActiveNearMode>()
        else {
            return Ok(());
        };
        let Ok(buffers) = self.view_query.get_manual(world, graph.view_entity()) else {
            return Ok(());
        };
        let Some(reservation) = telemetry.arbiter.try_reserve() else {
            return Ok(());
        };

        let source = forward_cull_source(buffers.hzb_bind_group.is_some());
        let slot_count = buffers.slot_count;
        let sequence = reservation.sequence;
        let staging = telemetry.slots[reservation.slot_index].staging.clone();
        let callback_staging = staging.clone();
        let sender = telemetry.sender.clone();
        let encoder = render_context.command_encoder();
        encoder.copy_buffer_to_buffer(&buffers.count, 0, &staging, 0, NEAR_TELEMETRY_COUNT_BYTES);
        encoder.map_buffer_on_submit(
            &staging,
            MapMode::Read,
            0..NEAR_TELEMETRY_STAGING_BYTES,
            move |result| {
                if result.is_ok() {
                    let mapped = callback_staging
                        .slice(0..NEAR_TELEMETRY_STAGING_BYTES)
                        .get_mapped_range();
                    let counts = [
                        u32::from_le_bytes(
                            mapped[..std::mem::size_of::<u32>()]
                                .try_into()
                                .expect("near visible chunk count is exactly four bytes"),
                        ),
                        u32::from_le_bytes(
                            mapped[std::mem::size_of::<u32>()..NEAR_TELEMETRY_COUNT_BYTES as usize]
                                .try_into()
                                .expect("near visible quad count is exactly four bytes"),
                        ),
                    ];
                    drop(mapped);
                    callback_staging.unmap();
                    let _ = sender.send(NearTelemetrySample {
                        sequence,
                        visible_chunks: counts[0],
                        visible_quads: counts[1],
                        source,
                        indirect_count,
                        slot_count,
                    });
                }
                reservation.release();
            },
        );
        Ok(())
    }
}

/// CPU mirror of the compute shader's command construction. Returning `None`
/// prevents malformed metadata from producing an out-of-bounds indirect draw.
pub fn indirect_command(
    metadata: GpuNearChunkMeta,
    visible: bool,
    record_capacity: u32,
    transform_capacity: u32,
) -> Option<DrawIndirectArgs> {
    let record_end = metadata.record_start.checked_add(metadata.quad_count)?;
    if record_end > record_capacity || metadata.transform_slot >= transform_capacity {
        return None;
    }
    let first_vertex = metadata.record_start.checked_mul(6)?;
    let visible_vertex_count = metadata.quad_count.checked_mul(6)?;
    Some(DrawIndirectArgs {
        vertex_count: if visible { visible_vertex_count } else { 0 },
        instance_count: 1,
        first_vertex,
        first_instance: metadata.transform_slot,
    })
}

pub struct HzbCpuInput<'a> {
    pub nearest_reverse_z: f32,
    pub sampled_reverse_z: &'a [f32],
    pub fully_on_screen: bool,
    pub crosses_near_plane: bool,
    pub depth_bias: f32,
}

/// Converts projected NDC points to top-left-origin texture UVs before reducing
/// the bounds. The Y inversion changes which projected corner supplies each
/// bound, so reducing in NDC and converting afterward is not equivalent.
pub fn hzb_uv_bounds(ndc_points: &[Vec2]) -> Option<(Vec2, Vec2)> {
    let mut uv_min = Vec2::splat(f32::INFINITY);
    let mut uv_max = Vec2::splat(f32::NEG_INFINITY);
    for &ndc in ndc_points {
        if !ndc.is_finite() {
            return None;
        }
        let uv = ndc * Vec2::new(0.5, -0.5) + Vec2::splat(0.5);
        uv_min = uv_min.min(uv);
        uv_max = uv_max.max(uv);
    }
    (!ndc_points.is_empty()).then_some((uv_min, uv_max))
}

/// Chooses the first mip whose continuous projected footprint is no wider
/// than one texel per axis. Rasterization at a boundary can still touch two
/// texels, which the shader samples exhaustively.
pub fn select_hzb_mip(pixel_span: Vec2, mip_count: u32) -> u32 {
    if mip_count == 0 {
        return 0;
    }
    let widest = pixel_span.max(Vec2::ONE).max_element();
    (widest.log2().ceil().max(0.0) as u32).min(mip_count - 1)
}

pub fn hzb_texel_bounds(uv_min: Vec2, uv_max: Vec2, mip_size: UVec2) -> (UVec2, UVec2) {
    let size = mip_size.max(UVec2::ONE);
    let max_texel = size - UVec2::ONE;
    let to_texel = |uv: Vec2| {
        (uv.clamp(Vec2::ZERO, Vec2::ONE) * size.as_vec2())
            .floor()
            .as_uvec2()
            .min(max_texel)
    };
    (to_texel(uv_min), to_texel(uv_max))
}

/// Conservative reverse-Z HZB decision. Border/near-plane uncertainty and any
/// sample that is not definitely in front keep the chunk visible.
pub fn conservative_hzb_visible(input: HzbCpuInput<'_>) -> bool {
    if !input.fully_on_screen
        || input.crosses_near_plane
        || input.sampled_reverse_z.is_empty()
        || !input.nearest_reverse_z.is_finite()
    {
        return true;
    }
    let occlusion_threshold = input.nearest_reverse_z + input.depth_bias.max(0.0);
    !input
        .sampled_reverse_z
        .iter()
        .all(|sample| sample.is_finite() && *sample > occlusion_threshold)
}

/// CPU mirror of the GPU frustum test. Like Bevy mesh culling and the existing
/// direct path, it includes the near plane and excludes the synthetic far
/// plane used with an infinite reverse-Z projection.
pub fn conservative_frustum_visible(frustum: &Frustum, center: Vec3, half_extents: Vec3) -> bool {
    chunk_in_view(frustum, center, half_extents)
}

#[cfg(test)]
mod tests {
    use bevy::prelude::*;

    use super::*;
    use crate::renderer::config::NearRenderBackend;
    use crate::renderer::voxel_pull::arena::GpuNearChunkMeta;

    fn all_capabilities() -> GpuNearCapabilities {
        GpuNearCapabilities {
            gpu_culling: true,
            compute: true,
            indirect_first_instance: true,
            multi_draw_indirect: true,
            multi_draw_indirect_count: true,
        }
    }

    #[test]
    fn requested_gpu_falls_back_when_any_required_capability_is_missing() {
        assert_eq!(
            resolve_near_mode(NearRenderBackend::GpuDriven, all_capabilities()),
            ActiveNearMode::GpuDriven {
                indirect_count: true
            }
        );

        for capabilities in [
            GpuNearCapabilities {
                gpu_culling: false,
                ..all_capabilities()
            },
            GpuNearCapabilities {
                compute: false,
                ..all_capabilities()
            },
            GpuNearCapabilities {
                indirect_first_instance: false,
                ..all_capabilities()
            },
            GpuNearCapabilities {
                multi_draw_indirect: false,
                ..all_capabilities()
            },
        ] {
            assert_eq!(
                resolve_near_mode(NearRenderBackend::GpuDriven, capabilities),
                ActiveNearMode::GroupedDirect
            );
        }

        assert_eq!(
            resolve_near_mode(
                NearRenderBackend::GpuDriven,
                GpuNearCapabilities {
                    multi_draw_indirect_count: false,
                    ..all_capabilities()
                }
            ),
            ActiveNearMode::GpuDriven {
                indirect_count: false
            }
        );
        assert_eq!(
            resolve_near_mode(NearRenderBackend::PerChunk, all_capabilities()),
            ActiveNearMode::Disabled
        );
    }

    fn metadata(record_start: u32, quad_count: u32, transform_slot: u32) -> GpuNearChunkMeta {
        GpuNearChunkMeta {
            center: [0.0, 0.0, -10.0, 0.0],
            half_extents: [1.0, 1.0, 1.0, 0.0],
            record_start,
            quad_count,
            transform_slot,
            cave_active: 1,
        }
    }

    #[test]
    fn indirect_commands_stay_inside_record_and_transform_buffers() {
        let command = indirect_command(metadata(10, 5, 3), true, 20, 4).unwrap();
        assert_eq!(
            command,
            DrawIndirectArgs {
                vertex_count: 30,
                instance_count: 1,
                first_vertex: 60,
                first_instance: 3,
            }
        );
        assert!(indirect_command(metadata(18, 5, 3), true, 20, 4).is_none());
        assert!(indirect_command(metadata(10, 5, 4), true, 20, 4).is_none());
        assert!(indirect_command(metadata(u32::MAX, 1, 0), true, u32::MAX, 1).is_none());
        assert_eq!(
            indirect_command(metadata(10, 5, 3), false, 20, 4)
                .unwrap()
                .vertex_count,
            0,
            "fixed-slot fallback must zero culled commands"
        );
    }

    #[test]
    fn conservative_hzb_mirror_only_culls_when_every_sample_is_definitely_closer() {
        let occluders = [0.8, 0.82, 0.81, 0.9, 0.83];
        assert!(!conservative_hzb_visible(HzbCpuInput {
            nearest_reverse_z: 0.5,
            sampled_reverse_z: &occluders,
            fully_on_screen: true,
            crosses_near_plane: false,
            depth_bias: 0.001,
        }));
        assert!(conservative_hzb_visible(HzbCpuInput {
            nearest_reverse_z: 0.5,
            sampled_reverse_z: &[0.8, 0.49, 0.81, 0.9, 0.83],
            fully_on_screen: true,
            crosses_near_plane: false,
            depth_bias: 0.001,
        }));
        assert!(conservative_hzb_visible(HzbCpuInput {
            nearest_reverse_z: 0.5,
            sampled_reverse_z: &occluders,
            fully_on_screen: false,
            crosses_near_plane: false,
            depth_bias: 0.001,
        }));
        assert!(conservative_hzb_visible(HzbCpuInput {
            nearest_reverse_z: 0.5,
            sampled_reverse_z: &occluders,
            fully_on_screen: true,
            crosses_near_plane: true,
            depth_bias: 0.001,
        }));
    }

    #[test]
    fn hzb_bounds_flip_framebuffer_y_before_reducing() {
        let (uv_min, uv_max) = hzb_uv_bounds(&[Vec2::new(-1.0, 0.5), Vec2::new(0.5, -1.0)])
            .expect("two finite projected corners have bounds");

        assert_eq!(uv_min, Vec2::new(0.0, 0.25));
        assert_eq!(uv_max, Vec2::new(0.75, 1.0));
    }

    #[test]
    fn hzb_mip_uses_ceil_and_bounds_the_complete_footprint_to_two_by_two() {
        assert_eq!(select_hzb_mip(Vec2::new(3.01, 1.0), 8), 2);

        // At mip 2 an 8px base dimension is 2px. These projected bounds
        // straddle both axes, so all four texels must be sampled.
        let (texel_min, texel_max) =
            hzb_texel_bounds(Vec2::splat(0.24), Vec2::splat(0.51), UVec2::splat(2));
        assert_eq!(texel_min, UVec2::ZERO);
        assert_eq!(texel_max, UVec2::ONE);
        assert!(texel_max.x - texel_min.x <= 1);
        assert!(texel_max.y - texel_min.y <= 1);
    }

    #[test]
    fn gpu_mode_waits_until_every_compute_pipeline_is_compiled() {
        let requested = ActiveNearMode::GpuDriven {
            indirect_count: true,
        };
        assert_eq!(
            gate_near_mode_on_pipeline_readiness(requested, false),
            ActiveNearMode::GroupedDirect
        );
        assert_eq!(
            gate_near_mode_on_pipeline_readiness(requested, true),
            requested
        );
    }

    #[test]
    fn gpu_view_capacity_grows_geometrically() {
        assert_eq!(geometric_gpu_capacity(1), 1);
        assert_eq!(geometric_gpu_capacity(9), 16);
        assert_eq!(geometric_gpu_capacity(16), 16);
    }

    #[test]
    fn gpu_submission_count_waits_for_frame_telemetry() {
        assert_eq!(
            exact_indirect_draw_count(ActiveNearMode::GpuDriven {
                indirect_count: false,
            }),
            None
        );
        assert_eq!(
            exact_indirect_draw_count(ActiveNearMode::GroupedDirect),
            Some(0)
        );
    }

    #[test]
    fn telemetry_values_distinguish_hzb_survivors_from_submitted_commands() {
        let count_mode = NearTelemetrySample {
            sequence: 1,
            visible_chunks: 7,
            visible_quads: 41,
            source: ForwardCullSource::Hzb,
            indirect_count: true,
            slot_count: 20,
        };
        assert_eq!(
            near_telemetry_values(count_mode),
            NearTelemetryValues {
                hzb_visible_chunks: Some(7),
                indirect_draw_count: 7,
                submitted_quads: 41,
                submitted_vertices: 246,
            }
        );

        let fixed_mode = NearTelemetrySample {
            indirect_count: false,
            ..count_mode
        };
        assert_eq!(
            near_telemetry_values(fixed_mode),
            NearTelemetryValues {
                hzb_visible_chunks: Some(7),
                indirect_draw_count: 20,
                submitted_quads: 41,
                submitted_vertices: 246,
            }
        );

        let frustum_fallback = NearTelemetrySample {
            source: ForwardCullSource::Frustum,
            ..count_mode
        };
        assert_eq!(
            near_telemetry_values(frustum_fallback),
            NearTelemetryValues {
                hzb_visible_chunks: None,
                indirect_draw_count: 7,
                submitted_quads: 41,
                submitted_vertices: 246,
            }
        );
    }

    #[test]
    fn asynchronous_telemetry_keeps_the_latest_completed_sample() {
        let mode = ActiveNearMode::GpuDriven {
            indirect_count: true,
        };
        let mut metrics = NearRendererMetrics {
            hzb_visible_chunks: Some(17),
            indirect_draw_count: Some(17),
            ..default()
        };

        apply_near_telemetry_sample(mode, None, &mut metrics);

        assert_eq!(metrics.hzb_visible_chunks, Some(17));
        assert_eq!(metrics.indirect_draw_count, Some(17));
    }

    #[test]
    fn telemetry_ring_skips_busy_frames_without_blocking() {
        let arbiter = NearTelemetryArbiter::new(2);

        arbiter.begin_frame();
        let first = arbiter.try_reserve().expect("first slot is free");
        arbiter.begin_frame();
        let second = arbiter.try_reserve().expect("second slot is free");
        arbiter.begin_frame();
        assert!(arbiter.try_reserve().is_none());

        first.release();
        arbiter.begin_frame();
        assert_eq!(
            arbiter
                .try_reserve()
                .expect("released slot is reusable")
                .slot_index,
            0
        );
        second.release();
    }

    #[test]
    fn telemetry_elects_only_one_core_3d_view_per_frame() {
        let arbiter = NearTelemetryArbiter::new(2);
        arbiter.begin_frame();

        let elected = arbiter.try_reserve().expect("first camera is elected");
        assert!(arbiter.try_reserve().is_none());

        elected.release();
        arbiter.begin_frame();
        assert!(arbiter.try_reserve().is_some());
    }

    #[test]
    fn telemetry_latest_consumes_each_accepted_sample_once() {
        let mut latest = NearTelemetryLatest::default();
        let sample = NearTelemetrySample {
            sequence: 1,
            visible_chunks: 7,
            visible_quads: 41,
            source: ForwardCullSource::Hzb,
            indirect_count: true,
            slot_count: 20,
        };

        assert!(latest.accept(sample));
        assert_eq!(latest.take(), Some(sample));
        assert_eq!(latest.take(), None);
    }

    #[test]
    fn telemetry_latest_rejects_older_callback_after_consumption() {
        let mut latest = NearTelemetryLatest::default();
        let newer = NearTelemetrySample {
            sequence: 2,
            visible_chunks: 7,
            visible_quads: 41,
            source: ForwardCullSource::Hzb,
            indirect_count: true,
            slot_count: 20,
        };
        let older = NearTelemetrySample {
            sequence: 1,
            visible_chunks: 19,
            ..newer
        };

        assert!(latest.accept(newer));
        assert_eq!(latest.take(), Some(newer));
        assert!(!latest.accept(older));
        assert_eq!(latest.take(), None);
    }

    #[test]
    fn graph_clear_is_after_both_early_consumers_and_before_both_late_consumers() {
        use bevy::render::render_graph::{EmptyNode, RenderGraphError};

        let mut graph = RenderGraph::default();
        graph.add_node(NearGpuNode::FrustumCull, EmptyNode);
        graph.add_node(NearGpuNode::ClearLatePhases, EmptyNode);
        graph.add_node(NearGpuNode::HzbCull, EmptyNode);
        graph.add_node(NearGpuNode::TelemetryCopy, EmptyNode);
        graph.add_node(Node3d::EarlyPrepass, EmptyNode);
        graph.add_node(Node3d::EarlyDownsampleDepth, EmptyNode);
        graph.add_node(Node3d::LatePrepass, EmptyNode);
        graph.add_node(Node3d::StartMainPass, EmptyNode);
        graph.add_node(NodePbr::EarlyShadowPass, EmptyNode);
        graph.add_node(NodePbr::LateShadowPass, EmptyNode);

        add_near_gpu_graph_edges(&mut graph, true);

        macro_rules! assert_direct_edge {
            ($from:expr, $to:expr) => {
                assert!(matches!(
                    graph.try_add_node_edge($from, $to),
                    Err(RenderGraphError::EdgeAlreadyExists(_))
                ));
            };
        }
        assert_direct_edge!(Node3d::EarlyDownsampleDepth, NearGpuNode::ClearLatePhases);
        assert_direct_edge!(NodePbr::EarlyShadowPass, NearGpuNode::ClearLatePhases);
        assert_direct_edge!(NearGpuNode::ClearLatePhases, Node3d::LatePrepass);
        assert_direct_edge!(NearGpuNode::ClearLatePhases, NodePbr::LateShadowPass);
        assert_direct_edge!(Node3d::LatePrepass, NearGpuNode::HzbCull);
        assert_direct_edge!(NearGpuNode::HzbCull, NearGpuNode::TelemetryCopy);
        assert_direct_edge!(NearGpuNode::TelemetryCopy, Node3d::StartMainPass);
    }

    #[test]
    fn graph_bypasses_telemetry_when_benchmark_readback_is_disabled() {
        use bevy::render::render_graph::{EmptyNode, RenderGraphError};

        let mut graph = RenderGraph::default();
        graph.add_node(NearGpuNode::FrustumCull, EmptyNode);
        graph.add_node(NearGpuNode::ClearLatePhases, EmptyNode);
        graph.add_node(NearGpuNode::HzbCull, EmptyNode);
        graph.add_node(Node3d::EarlyPrepass, EmptyNode);
        graph.add_node(Node3d::EarlyDownsampleDepth, EmptyNode);
        graph.add_node(Node3d::LatePrepass, EmptyNode);
        graph.add_node(Node3d::StartMainPass, EmptyNode);
        graph.add_node(NodePbr::EarlyShadowPass, EmptyNode);
        graph.add_node(NodePbr::LateShadowPass, EmptyNode);

        add_near_gpu_graph_edges(&mut graph, false);

        assert!(matches!(
            graph.try_add_node_edge(NearGpuNode::HzbCull, Node3d::StartMainPass),
            Err(RenderGraphError::EdgeAlreadyExists(_))
        ));
    }

    #[test]
    fn conservative_frustum_mirror_keeps_intersections_and_ignores_far_plane() {
        let clip_from_world =
            Mat4::perspective_infinite_reverse_rh(std::f32::consts::FRAC_PI_2, 1.0, 0.1);
        let frustum = bevy::camera::primitives::Frustum::from_clip_from_world_custom_far(
            &clip_from_world,
            &Vec3::ZERO,
            &Vec3::Z,
            1000.0,
        );
        assert!(conservative_frustum_visible(
            &frustum,
            Vec3::new(0.0, 0.0, -2000.0),
            Vec3::splat(16.0),
        ));
        assert!(conservative_frustum_visible(
            &frustum,
            Vec3::new(95.0, 0.0, -100.0),
            Vec3::splat(10.0),
        ));
        assert!(!conservative_frustum_visible(
            &frustum,
            Vec3::new(200.0, 0.0, -100.0),
            Vec3::splat(10.0),
        ));
    }

    #[test]
    fn default_camera_derives_clip_from_world_when_no_explicit_matrix_exists() {
        let clip_from_view =
            Mat4::perspective_infinite_reverse_rh(std::f32::consts::FRAC_PI_2, 16.0 / 9.0, 0.1);
        let world_from_view = Mat4::from_translation(Vec3::new(4.0, 5.0, 6.0));

        assert_eq!(
            resolve_clip_from_world(None, clip_from_view, world_from_view),
            clip_from_view * world_from_view.inverse()
        );
        assert_eq!(
            resolve_clip_from_world(Some(Mat4::IDENTITY), clip_from_view, world_from_view),
            Mat4::IDENTITY
        );
    }
}
