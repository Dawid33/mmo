use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc,
};

use bevy::{
    camera::primitives::Frustum,
    core_pipeline::mip_generation::experimental::depth::ViewDepthPyramid,
    ecs::schedule::Schedule,
    pbr::ViewLightEntities,
    prelude::*,
    render::{
        batching::gpu_preprocessing::GpuPreprocessingSupport,
        render_resource::{binding_types as bt, *},
        renderer::{RenderAdapterInfo, RenderContext, RenderDevice, RenderQueue, ViewQuery},
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

impl ActiveNearMode {
    /// Stable benchmark/report name for the renderer that is actually drawing.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "per-chunk",
            Self::GroupedDirect => "direct",
            Self::GpuDriven { .. } => "gpu",
        }
    }
}

#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestedNearBackend(pub NearRenderBackend);

#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NearRendererMetrics {
    /// Render-world mode that produced the mode-sensitive fields below.
    pub active_mode: ActiveNearMode,
    /// Changes whenever `active_mode` changes, invalidating delayed readbacks.
    pub mode_generation: u64,
    /// Visibility source for the current mode/sample. GPU mode remains `None`
    /// until a matching asynchronous sample is observed.
    pub visibility_source: Option<ForwardCullSource>,
    pub active_chunks: u32,
    /// CPU frustum survivors summed over views. `None` in GPU-driven mode
    /// unless `VOXEL_PULL_DEBUG` is set: the GPU culls there, and the CPU
    /// count would cost a frustum test per chunk per view every frame.
    pub frustum_visible_chunks: Option<u32>,
    /// Exact main-forward HZB survivor count. `None` when HZB was not the
    /// forward cull source or benchmark-only asynchronous readback is disabled.
    pub hzb_visible_chunks: Option<u32>,
    /// Main-forward indirect command records submitted. Indirect-count mode
    /// submits the GPU survivor count; fixed mode submits every stable slot,
    /// including rejected slots whose command has zero vertices. Direct modes
    /// submit no indirect records.
    pub indirect_draw_count: Option<u32>,
    /// Exact main-forward direct terrain draws. Grouped-direct mode counts
    /// view draw records; GPU-driven mode submits no direct terrain draws.
    pub direct_draw_count: Option<u32>,
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

/// WebGPU's indexed indirect draw ABI. Every command indexes the shared quad
/// index buffer (`QuadIndexBuffer`) from 0; `base_vertex` = `record_start * 4`
/// picks the chunk's records (4 pulled vertices per quad).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DrawIndexedIndirectArgs {
    pub index_count: u32,
    pub instance_count: u32,
    pub first_index: u32,
    pub base_vertex: i32,
    pub first_instance: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuNearCullView {
    clip_from_world: [f32; 16],
    viewport_and_capacity: [u32; 4],
    /// Eye position; w = 1 lets the cull drop back-facing face ranges
    /// (perspective views), w = 0 keeps every face (orthographic shadow
    /// cascades, whose casters must not depend on the camera).
    camera: [f32; 4],
}

/// `VOXEL_FACE_CULL=off|0|false` keeps every face range (A/B runs, or a
/// quick fallback if face culling ever leaves holes). On by default. Read once.
fn face_culling_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        let value = std::env::var("VOXEL_FACE_CULL").unwrap_or_default();
        !matches!(value.to_ascii_lowercase().as_str(), "off" | "0" | "false")
    })
}

/// Command slots per chunk slot: one indexed draw per face direction
/// (`records::face_ends`), so ranges facing away from the camera are skipped.
pub const FACES_PER_SLOT: u32 = 6;

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
            camera: {
                let eye = view.world_from_view.translation();
                // Perspective projections have w_axis.w == 0.
                let perspective = view.clip_from_view.w_axis.w == 0.0;
                let cull = perspective && face_culling_enabled();
                [eye.x, eye.y, eye.z, if cull { 1.0 } else { 0.0 }]
            },
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
        u64::from(sizes.capacity.max(1))
            * u64::from(FACES_PER_SLOT)
            * std::mem::size_of::<DrawIndexedIndirectArgs>() as u64;
    let indirect = device.create_buffer(&BufferDescriptor {
        label: Some("voxel_pull_indirect_commands"),
        size: command_bytes,
        usage: BufferUsages::STORAGE | BufferUsages::INDIRECT | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let count = device.create_buffer(&BufferDescriptor {
        label: Some("voxel_pull_indirect_count"),
        // [commands, visible_quads, visible_chunks, pad]; the indirect-count
        // draw reads the command count at offset 0.
        size: std::mem::size_of::<[u32; 4]>() as u64,
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

/// Installs the ordering contract for the shared near-terrain indirect buffers
/// as `Core3d` systems (Bevy 0.19 replaced the render graph with per-camera
/// schedules). Frustum results feed only the early depth/shadow consumers.
/// Both consumers must finish before the buffers are cleared; both late
/// consumers must observe that clear. HZB then repopulates only the main-view
/// buffer for the forward pass. Benchmark readback, when enabled, copies the
/// final count before the main pass consumes it.
pub fn add_near_gpu_systems(schedule: &mut Schedule, telemetry_enabled: bool) {
    use bevy::core_pipeline::{
        mip_generation::experimental::depth::early_downsample_depth,
        prepass::node::{early_prepass, late_prepass},
        Core3dSystems,
    };
    use bevy::pbr::{per_view_shadow_pass, shared_shadow_pass, EARLY_SHADOW_PASS, LATE_SHADOW_PASS};

    schedule.add_systems((
        near_frustum_cull
            .before(early_prepass)
            .before(per_view_shadow_pass::<EARLY_SHADOW_PASS>)
            .before(shared_shadow_pass::<EARLY_SHADOW_PASS>),
        near_clear_late_phases
            .after(early_downsample_depth)
            .after(per_view_shadow_pass::<EARLY_SHADOW_PASS>)
            .after(shared_shadow_pass::<EARLY_SHADOW_PASS>)
            .before(late_prepass)
            .before(per_view_shadow_pass::<LATE_SHADOW_PASS>)
            .before(shared_shadow_pass::<LATE_SHADOW_PASS>),
        near_hzb_cull
            .after(late_prepass)
            .before(Core3dSystems::MainPass),
    ));
    if telemetry_enabled {
        schedule.add_systems(
            near_telemetry_copy
                .after(near_hzb_cull)
                .before(Core3dSystems::MainPass),
        );
    }
}

fn dispatch_cull(
    render_context: &mut RenderContext<'_, '_>,
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

pub fn near_frustum_cull(
    world: &World,
    view: ViewQuery<()>,
    views: Query<(&GpuNearViewBuffers, Option<&ViewLightEntities>)>,
    light_view_buffers: Query<&GpuNearViewBuffers>,
    mut render_context: RenderContext,
) {
    if !matches!(
        *world.resource::<ActiveNearMode>(),
        ActiveNearMode::GpuDriven { .. }
    ) {
        return;
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
    let Ok((buffers, light_views)) = views.get(view.entity())
    else {
        return;
    };
    dispatch_cull(
        &mut render_context,
        cache,
        pipeline_id,
        buffers,
        &buffers.frustum_bind_group,
        "voxel_pull_frustum_cull",
    );
    if let Some(light_views) = light_views {
        for light_view in light_views.lights.iter().copied() {
            if let Ok(buffers) = light_view_buffers.get(light_view) {
                dispatch_cull(
                    &mut render_context,
                    cache,
                    pipeline_id,
                    buffers,
                    &buffers.frustum_bind_group,
                    "voxel_pull_shadow_frustum_cull",
                );
            }
        }
    }
}

/// Indirect-count draws read their draw count from `count`, so zeroing it
/// alone empties the late passes. Only fixed-slot mode (the count is ignored,
/// every command slot is drawn) must zero the commands themselves — a buffer
/// of `capacity * FACES_PER_SLOT` commands per view, ~8 MB each at `full`
/// (~39 MB a frame over five views).
fn clear_cull_arguments(
    render_context: &mut RenderContext<'_, '_>,
    buffers: &GpuNearViewBuffers,
    indirect_count: bool,
) {
    let encoder = render_context.command_encoder();
    if !indirect_count {
        encoder.clear_buffer(&buffers.indirect, 0, None);
    }
    encoder.clear_buffer(&buffers.count, 0, None);
}

/// Clears the shared phase item's arguments after both early consumers. Bevy
/// executes the same binned prepass/shadow phase again in its late passes, so
/// leaving the frustum commands resident would draw every near chunk twice.
pub fn near_clear_late_phases(
    world: &World,
    view: ViewQuery<()>,
    views: Query<(&GpuNearViewBuffers, Option<&ViewLightEntities>)>,
    light_view_buffers: Query<&GpuNearViewBuffers>,
    mut render_context: RenderContext,
) {
    let ActiveNearMode::GpuDriven { indirect_count } = *world.resource::<ActiveNearMode>()
    else {
        return;
    };
    let Ok((buffers, light_views)) = views.get(view.entity())
    else {
        return;
    };
    clear_cull_arguments(&mut render_context, buffers, indirect_count);
    if let Some(light_views) = light_views {
        for light_view in light_views.lights.iter().copied() {
            if let Ok(buffers) = light_view_buffers.get(light_view) {
                clear_cull_arguments(&mut render_context, buffers, indirect_count);
            }
        }
    }
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
const NEAR_TELEMETRY_STAGING_BYTES: u64 = 16;
const NEAR_TELEMETRY_COUNT_BYTES: u64 = std::mem::size_of::<[u32; 4]>() as u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NearTelemetrySample {
    sequence: u64,
    mode: ActiveNearMode,
    mode_generation: u64,
    commands: u32,
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
            sample.commands
        } else {
            sample.slot_count.saturating_mul(FACES_PER_SLOT)
        },
        submitted_quads: sample.visible_quads as u64,
        // Indexed: four shaded vertices per quad.
        submitted_vertices: (sample.visible_quads as u64).saturating_mul(4),
    }
}

fn apply_near_telemetry_sample(
    mode: ActiveNearMode,
    sample: Option<NearTelemetrySample>,
    metrics: &mut NearRendererMetrics,
) {
    synchronize_near_metric_mode(mode, metrics);
    if matches!(mode, ActiveNearMode::GpuDriven { .. }) {
        // Mapping is asynchronous and does not necessarily complete every
        // render frame. Retain the newest completed sample until a newer one
        // arrives instead of exposing one-frame pulses that the main-world
        // benchmark can easily miss.
        if let Some(sample) = sample.filter(|sample| {
            sample.mode == mode && sample.mode_generation == metrics.mode_generation
        }) {
            let values = near_telemetry_values(sample);
            metrics.visibility_source = Some(sample.source);
            metrics.hzb_visible_chunks = values.hzb_visible_chunks;
            metrics.indirect_draw_count = Some(values.indirect_draw_count);
            metrics.submitted_quads = Some(values.submitted_quads);
            metrics.submitted_vertices = Some(values.submitted_vertices);
            metrics.direct_draw_count = Some(0);
        }
    } else {
        metrics.visibility_source = match mode {
            ActiveNearMode::GroupedDirect => Some(ForwardCullSource::Frustum),
            ActiveNearMode::Disabled => None,
            ActiveNearMode::GpuDriven { .. } => unreachable!(),
        };
        metrics.hzb_visible_chunks = None;
        metrics.indirect_draw_count = exact_indirect_draw_count(mode);
        metrics.direct_draw_count = match mode {
            ActiveNearMode::GroupedDirect => metrics.direct_draw_count,
            ActiveNearMode::Disabled => None,
            ActiveNearMode::GpuDriven { .. } => unreachable!(),
        };
    }
}

/// Synchronizes mode-sensitive telemetry with the renderer that is currently
/// drawing. Delayed GPU callbacks carry this generation and are ignored after
/// any transition, including a GPU -> direct -> GPU cycle.
pub(crate) fn synchronize_near_metric_mode(
    mode: ActiveNearMode,
    metrics: &mut NearRendererMetrics,
) {
    if metrics.active_mode == mode {
        return;
    }
    metrics.active_mode = mode;
    metrics.mode_generation = metrics.mode_generation.wrapping_add(1);
    metrics.visibility_source = None;
    metrics.hzb_visible_chunks = None;
    metrics.indirect_draw_count = None;
    metrics.direct_draw_count = None;
    metrics.submitted_quads = None;
    metrics.submitted_vertices = None;
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

pub fn near_hzb_cull(
    world: &World,
    view: ViewQuery<()>,
    views: Query<&GpuNearViewBuffers>,
    mut render_context: RenderContext,
) {
    if !matches!(
        *world.resource::<ActiveNearMode>(),
        ActiveNearMode::GpuDriven { .. }
    ) {
        return;
    }
    let Ok(buffers) = views.get(view.entity()) else {
        return;
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
        &mut render_context,
        world.resource::<PipelineCache>(),
        pipeline_id,
        buffers,
        bind_group,
        label,
    );
}

pub fn near_telemetry_copy(
    world: &World,
    view: ViewQuery<()>,
    views: Query<&GpuNearViewBuffers>,
    mut render_context: RenderContext,
) {
    let Some(telemetry) = world.get_resource::<NearTelemetryReadback>() else {
        return;
    };
    let ActiveNearMode::GpuDriven { indirect_count } = *world.resource::<ActiveNearMode>()
    else {
        return;
    };
    let Ok(buffers) = views.get(view.entity()) else {
        return;
    };
    let Some(reservation) = telemetry.arbiter.try_reserve() else {
        return;
    };

    let source = forward_cull_source(buffers.hzb_bind_group.is_some());
    let metrics = world.resource::<NearRendererMetrics>();
    let mode_generation = metrics.mode_generation;
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
                let counts: Vec<u32> = mapped[..NEAR_TELEMETRY_COUNT_BYTES as usize]
                    .chunks_exact(4)
                    .map(|word| u32::from_le_bytes(word.try_into().expect("four bytes")))
                    .collect();
                drop(mapped);
                callback_staging.unmap();
                let _ = sender.send(NearTelemetrySample {
                    sequence,
                    mode: ActiveNearMode::GpuDriven { indirect_count },
                    mode_generation,
                    commands: counts[0],
                    visible_quads: counts[1],
                    visible_chunks: counts[2],
                    source,
                    indirect_count,
                    slot_count,
                });
            }
            reservation.release();
        },
    );
}

/// CPU mirror of the compute shader's command construction. Returning `None`
/// prevents malformed metadata from producing an out-of-bounds indirect draw.
pub fn indirect_command(
    metadata: GpuNearChunkMeta,
    visible: bool,
    record_capacity: u32,
    transform_capacity: u32,
) -> Option<DrawIndexedIndirectArgs> {
    let record_end = metadata.record_start.checked_add(metadata.quad_count)?;
    if record_end > record_capacity || metadata.transform_slot >= transform_capacity {
        return None;
    }
    let base_vertex = i32::try_from(metadata.record_start.checked_mul(4)?).ok()?;
    let visible_index_count = metadata.quad_count.checked_mul(6)?;
    Some(DrawIndexedIndirectArgs {
        index_count: if visible { visible_index_count } else { 0 },
        instance_count: 1,
        first_index: 0,
        base_vertex,
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
            face_ends: [quad_count, quad_count, quad_count, quad_count, quad_count, quad_count, 0, 0],
        }
    }

    #[test]
    fn indirect_commands_stay_inside_record_and_transform_buffers() {
        let command = indirect_command(metadata(10, 5, 3), true, 20, 4).unwrap();
        assert_eq!(
            command,
            DrawIndexedIndirectArgs {
                index_count: 30,
                instance_count: 1,
                first_index: 0,
                base_vertex: 40,
                first_instance: 3,
            }
        );
        assert!(indirect_command(metadata(18, 5, 3), true, 20, 4).is_none());
        assert!(indirect_command(metadata(10, 5, 4), true, 20, 4).is_none());
        assert!(indirect_command(metadata(u32::MAX, 1, 0), true, u32::MAX, 1).is_none());
        assert_eq!(
            indirect_command(metadata(10, 5, 3), false, 20, 4)
                .unwrap()
                .index_count,
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
            mode: ActiveNearMode::GpuDriven {
                indirect_count: true,
            },
            mode_generation: 1,
            commands: 12,
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
                indirect_draw_count: 12,
                submitted_quads: 41,
                submitted_vertices: 164,
            }
        );

        let fixed_mode = NearTelemetrySample {
            mode: ActiveNearMode::GpuDriven {
                indirect_count: false,
            },
            indirect_count: false,
            ..count_mode
        };
        assert_eq!(
            near_telemetry_values(fixed_mode),
            NearTelemetryValues {
                hzb_visible_chunks: Some(7),
                indirect_draw_count: 120,
                submitted_quads: 41,
                submitted_vertices: 164,
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
                indirect_draw_count: 12,
                submitted_quads: 41,
                submitted_vertices: 164,
            }
        );
    }

    #[test]
    fn asynchronous_telemetry_keeps_the_latest_completed_sample() {
        let mode = ActiveNearMode::GpuDriven {
            indirect_count: true,
        };
        let mut metrics = NearRendererMetrics {
            active_mode: mode,
            hzb_visible_chunks: Some(17),
            indirect_draw_count: Some(17),
            ..default()
        };

        apply_near_telemetry_sample(mode, None, &mut metrics);

        assert_eq!(metrics.hzb_visible_chunks, Some(17));
        assert_eq!(metrics.indirect_draw_count, Some(17));
    }

    #[test]
    fn grouped_to_gpu_transition_clears_direct_telemetry_until_matching_sample_arrives() {
        let mut metrics = NearRendererMetrics {
            active_mode: ActiveNearMode::GroupedDirect,
            visibility_source: Some(ForwardCullSource::Frustum),
            indirect_draw_count: Some(0),
            direct_draw_count: Some(9),
            submitted_quads: Some(41),
            submitted_vertices: Some(246),
            ..default()
        };

        synchronize_near_metric_mode(
            ActiveNearMode::GpuDriven {
                indirect_count: true,
            },
            &mut metrics,
        );

        assert_eq!(metrics.active_mode.as_str(), "gpu");
        assert_eq!(metrics.visibility_source, None);
        assert_eq!(metrics.hzb_visible_chunks, None);
        assert_eq!(metrics.indirect_draw_count, None);
        assert_eq!(metrics.submitted_quads, None);
        assert_eq!(metrics.submitted_vertices, None);
        assert_eq!(metrics.direct_draw_count, None);
    }

    #[test]
    fn delayed_gpu_sample_from_an_older_mode_generation_is_ignored() {
        let mode = ActiveNearMode::GpuDriven {
            indirect_count: true,
        };
        let mut metrics = NearRendererMetrics {
            active_mode: mode,
            mode_generation: 8,
            ..default()
        };
        let stale = NearTelemetrySample {
            sequence: 1,
            mode,
            mode_generation: 7,
            commands: 12,
            visible_chunks: 7,
            visible_quads: 41,
            source: ForwardCullSource::Hzb,
            indirect_count: true,
            slot_count: 20,
        };

        apply_near_telemetry_sample(mode, Some(stale), &mut metrics);

        assert_eq!(metrics.visibility_source, None);
        assert_eq!(metrics.hzb_visible_chunks, None);
        assert_eq!(metrics.indirect_draw_count, None);
        assert_eq!(metrics.submitted_quads, None);
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
            mode: ActiveNearMode::GpuDriven {
                indirect_count: true,
            },
            mode_generation: 1,
            commands: 12,
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
            mode: ActiveNearMode::GpuDriven {
                indirect_count: true,
            },
            mode_generation: 1,
            commands: 12,
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

    use bevy::core_pipeline::{
        mip_generation::experimental::depth::early_downsample_depth,
        prepass::node::{early_prepass, late_prepass},
    };
    use bevy::pbr::{per_view_shadow_pass, EARLY_SHADOW_PASS, LATE_SHADOW_PASS};
    use std::any::TypeId;

    fn main_pass_marker() {}

    /// Identity of a function system. Names are blanked without `bevy/debug`,
    /// so the order checks compare system types instead.
    fn id<M>(system: impl IntoSystem<(), (), M>) -> TypeId {
        IntoSystem::into_system(system).system_type()
    }

    /// Topologically sorted systems of a `Core3d` schedule holding the
    /// near-GPU systems and the Bevy passes they are ordered against.
    fn near_gpu_system_order(telemetry_enabled: bool) -> Vec<TypeId> {
        use bevy::core_pipeline::{Core3d, Core3dSystems};
        use bevy::pbr::shared_shadow_pass;

        let mut world = World::new();
        let mut schedule = Core3d::base_schedule();
        schedule.add_systems((
            early_prepass.in_set(Core3dSystems::Prepass),
            early_downsample_depth.after(early_prepass).before(late_prepass),
            late_prepass.in_set(Core3dSystems::Prepass),
            per_view_shadow_pass::<EARLY_SHADOW_PASS>,
            shared_shadow_pass::<EARLY_SHADOW_PASS>,
            per_view_shadow_pass::<LATE_SHADOW_PASS>,
            shared_shadow_pass::<LATE_SHADOW_PASS>,
            main_pass_marker.in_set(Core3dSystems::MainPass),
        ));
        add_near_gpu_systems(&mut schedule, telemetry_enabled);
        schedule.initialize(&mut world).expect("schedule builds");
        schedule
            .systems()
            .expect("initialized")
            .map(|(_, system)| system.system_type())
            .collect()
    }

    fn position(order: &[TypeId], system: TypeId) -> usize {
        order
            .iter()
            .position(|id| *id == system)
            .expect("system is scheduled")
    }

    #[test]
    fn clear_is_after_both_early_consumers_and_before_both_late_consumers() {
        let order = near_gpu_system_order(true);
        let at = |system: TypeId| position(&order, system);
        let frustum = at(id(near_frustum_cull));
        let clear = at(id(near_clear_late_phases));
        let hzb = at(id(near_hzb_cull));
        let telemetry = at(id(near_telemetry_copy));
        let early_shadow = at(id(per_view_shadow_pass::<EARLY_SHADOW_PASS>));
        let late_shadow = at(id(per_view_shadow_pass::<LATE_SHADOW_PASS>));
        assert!(frustum < at(id(early_prepass)));
        assert!(frustum < early_shadow);
        assert!(at(id(early_downsample_depth)) < clear);
        assert!(early_shadow < clear);
        assert!(clear < at(id(late_prepass)));
        assert!(clear < late_shadow);
        assert!(at(id(late_prepass)) < hzb);
        assert!(hzb < telemetry);
        assert!(telemetry < at(id(main_pass_marker)));
    }

    #[test]
    fn schedule_omits_telemetry_when_benchmark_readback_is_disabled() {
        let order = near_gpu_system_order(false);
        assert!(!order.contains(&id(near_telemetry_copy)));
        assert!(position(&order, id(near_hzb_cull)) < position(&order, id(main_pass_marker)));
    }

    #[test]
    fn conservative_frustum_mirror_keeps_intersections_and_ignores_far_plane() {
        let clip_from_world =
            Mat4::perspective_infinite_reverse_rh(std::f32::consts::FRAC_PI_2, 1.0, 0.1);
        let frustum = bevy::camera::primitives::Frustum(bevy::math::primitives::ViewFrustum::from_clip_from_world_custom_far(
            &clip_from_world,
            &Vec3::ZERO,
            &Vec3::Z,
            1000.0,
        ));
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
