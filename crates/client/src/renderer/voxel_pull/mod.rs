//! The pulled (mesh-free) chunk render path: per-quad records in storage
//! buffers, drawn by custom Opaque3d phase items (and AlphaMask3d ones for
//! the cutout ranges — leaves). Main-world components
//! live here; the render-world half (pipeline/prepare/queue) is split into
//! submodules. Modeled on bevy-0.18.1/examples/shader_advanced/custom_phase_item.rs.

use std::sync::Arc;

use bevy::{
    camera::primitives::Aabb,
    core_pipeline::{
        core_3d::{AlphaMask3d, Opaque3d},
        Core3d,
        prepass::{AlphaMask3dPrepass, Opaque3dPrepass},
    },
    pbr::{init_prepass_pipeline, Shadow},
    prelude::*,
    render::{
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        render_phase::AddRenderCommand,
        render_resource::SpecializedRenderPipelines,
        sync_world::{MainEntity, RenderEntity, SyncToRenderWorld},
        Extract, ExtractSchedule, Render, RenderApp, RenderStartup, RenderSystems,
    },
};

use super::meshing::records::QuadRecord;

pub mod arena;
pub mod gpu;
pub mod pipeline;
pub mod prepare;
pub mod queue;
pub mod xray;

use pipeline::{init_voxel_pull_depth_pipeline, VoxelPullDepthPipeline, VoxelPullPipeline};
use prepare::{prepare_chunk_quads, prepare_view_chunk_draws};
use queue::{
    queue_voxel_pull, queue_voxel_pull_prepass, queue_voxel_pull_shadows, DrawVoxelPull,
    DrawVoxelPullCutout, DrawVoxelPullDepth, DrawVoxelPullDepthCutout,
};

/// The pull-path replacement for `Mesh3d`: every visible greedy quad of one
/// chunk (or LOD cell), as GPU-ready records. `Arc` so render-world extraction
/// clones the handle, not the data. Requires `Visibility` because without
/// `Mesh3d` nothing else inserts it, and cave-culling writes `&mut Visibility`;
/// requires `SyncToRenderWorld` because — again without `Mesh3d` — nothing else
/// gives the entity a `RenderEntity` for extraction to target.
#[derive(Component, Clone)]
#[require(Visibility, SyncToRenderWorld)]
pub struct ChunkQuads(pub Arc<Vec<QuadRecord>>);

/// Which detail band a `ChunkQuads` entity draws in (GPU-driven path only;
/// see `meshing::coarse`). A sim chunk whose coarse twin is meshed is
/// [`DetailBand::Fine`] — drawn only within [`NearDetail`]'s switch distance
/// of the main camera — and the twin [`DetailBand::Coarse`], drawn only
/// beyond it. No component (or `Always`) = always drawn. The cull compute
/// decides per view from the same eye, so a chunk and its twin never both
/// draw nor both vanish; the CPU-culled paths draw `Fine` always and skip
/// `Coarse`.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailBand {
    Always,
    Fine,
    Coarse,
}

impl DetailBand {
    /// The cull shader's `BAND_*` code (`voxel_pull_cull.wgsl`).
    pub fn code(band: Option<&DetailBand>) -> u32 {
        match band {
            None | Some(DetailBand::Always) => 0,
            Some(DetailBand::Fine) => 1,
            Some(DetailBand::Coarse) => 2,
        }
    }
}

/// Default switch distance from full-detail sim chunks to their coarse
/// (block-scale) twins, metres of horizontal distance from the camera. At
/// 96 m a block is ~10 px and a leaf texel ~0.6 px on the reference 935 px
/// focal length, so the twin's whole blocks and solid foliage do not read as
/// a change; chiselled detail (branches, half blocks) is already sub-pixel.
pub const NEAR_DETAIL_DISTANCE_M: f32 = 96.0;

/// `VOXEL_NEAR_DETAIL_M=M` (native) moves the switch; `0` turns coarse twins
/// off (every sim chunk at full detail, as before). Read once.
pub fn near_detail_distance_m() -> f32 {
    static VALUE: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(m) = std::env::var("VOXEL_NEAR_DETAIL_M").ok().and_then(|v| v.trim().parse::<f32>().ok()) {
            return m.max(0.0);
        }
        NEAR_DETAIL_DISTANCE_M
    })
}

/// The main camera's eye and the detail switch distance, for the cull
/// compute's `detail` uniform. Written every frame by [`track_near_detail`].
#[derive(Resource, Clone, Copy, Debug, Default, ExtractResource)]
pub struct NearDetail {
    pub eye: Vec3,
    /// World units; 0 = no eye yet / bands off.
    pub distance: f32,
    /// What the main camera can see lit by the sun: its frustum, cut off at
    /// the sun's shadow distance, as inward half-spaces (`xyz` = unit
    /// normal, `n · p + w >= 0` inside) — [`shadow_receiver_planes`]. The
    /// cull compute drops shadow casters that cannot reach it. Meaningful
    /// only when `receiver_count` is `RECEIVER_PLANES`; 0 = no such cull.
    pub receivers: [Vec4; RECEIVER_PLANES],
    pub receiver_count: u32,
}

/// Half-spaces of [`NearDetail::receivers`]: left, right, bottom, top, near, far.
pub const RECEIVER_PLANES: usize = 6;

impl NearDetail {
    pub fn cull_uniform(&self) -> [f32; 4] {
        [self.eye.x, self.eye.y, self.eye.z, self.distance]
    }
}

/// The half-spaces bounding what a camera sees within `reach` world units
/// along its view direction: the four sides and the near plane of
/// `clip_from_world` (Gribb–Hartmann; reverse-Z, so near is `w - z >= 0`),
/// and a far plane `reach` in front of `eye` (the projection's own far plane
/// is at infinity). `None` if the matrix yields a degenerate plane.
pub fn shadow_receiver_planes(clip_from_world: Mat4, eye: Vec3, forward: Vec3, reach: f32) -> Option<[Vec4; RECEIVER_PLANES]> {
    let (r0, r1, r2, r3) = (clip_from_world.row(0), clip_from_world.row(1), clip_from_world.row(2), clip_from_world.row(3));
    let far_point = eye + forward * reach;
    let raw = [r3 + r0, r3 - r0, r3 + r1, r3 - r1, r3 - r2, (-forward).extend(forward.dot(far_point))];
    let mut planes = [Vec4::ZERO; RECEIVER_PLANES];
    for (plane, raw) in planes.iter_mut().zip(raw) {
        let length = raw.truncate().length();
        if !(length.is_finite() && length > 1.0e-12) {
            return None;
        }
        *plane = raw / length;
    }
    Some(planes)
}

/// The active world camera's eye and lit view volume into [`NearDetail`].
pub fn track_near_detail(
    mut detail: ResMut<NearDetail>,
    cameras: Query<(&GlobalTransform, &Camera), (With<Camera3d>, Without<crate::session::FrontendCamera>)>,
    suns: Query<&bevy::light::CascadeShadowConfig, With<DirectionalLight>>,
) {
    let distance = near_detail_distance_m() * game::movement::UNITS_PER_METER;
    let camera = cameras.iter().find(|(_, c)| c.is_active);
    let next = match camera {
        Some((transform, camera)) => {
            let eye = transform.translation();
            // One sun; its last cascade bound is how far shadows are drawn.
            let reach = suns.iter().filter_map(|config| config.bounds.last().copied()).fold(0.0f32, f32::max);
            let clip_from_world = camera.clip_from_view() * transform.to_matrix().inverse();
            let planes = (reach > 0.0)
                .then(|| shadow_receiver_planes(clip_from_world, eye, transform.forward().as_vec3(), reach))
                .flatten();
            NearDetail {
                eye,
                distance,
                receivers: planes.unwrap_or_default(),
                receiver_count: if planes.is_some() { RECEIVER_PLANES as u32 } else { 0 },
            }
        }
        None => NearDetail::default(),
    };
    if *detail != next {
        *detail = next;
    }
}

impl PartialEq for NearDetail {
    fn eq(&self, other: &Self) -> bool {
        self.eye == other.eye
            && self.distance == other.distance
            && self.receivers == other.receivers
            && self.receiver_count == other.receiver_count
    }
}

/// The block array texture, shared with the mesh path's material. Extracted
/// into the render world (via `ExtractResource`) so `prepare` can bind it.
#[derive(Resource, Clone, ExtractResource)]
pub struct BlockArrayTexture(pub Handle<Image>);

/// Side-face layer -> top-face layer of the same block
/// (`BlockTextureLayers::side_to_top`), bound as `voxel_pull.wgsl`'s
/// `top_layers` so distant risers can fade to their block's top.
#[derive(Resource, Clone, ExtractResource)]
pub struct BlockTopLayers(pub Arc<Vec<u32>>);

/// Render-world copy of one chunk's draw inputs. The render world is
/// retained, so this is (re-)inserted ONLY when the chunk's quads, transform
/// or inherited visibility changed — re-inserting all ~57k chunk entities
/// every frame measured 17.6 ms/frame at the production world size
/// (2026-09-19 trace), 13 ms of it just applying the insert commands.
/// `prepare_chunk_quads` keys its uploads off `Changed<ExtractedChunkQuads>`,
/// i.e. off that insert; there is deliberately no stored "changed" flag, which
/// would go stale the first frame a chunk is NOT re-extracted.
#[derive(Component)]
pub struct ExtractedChunkQuads {
    pub records: Arc<Vec<QuadRecord>>,
    pub world_from_local: Mat4,
    /// World-space AABB for per-view frustum tests at queue time (Bevy's
    /// visibility system only frustum-culls `VisibilityClass`'d entities, which
    /// mesh-less chunks are not).
    pub center: Vec3,
    pub half_extents: Vec3,
    /// Main-world cave-connectivity result. Hidden chunks keep their stable
    /// metadata slot but are marked inactive for both CPU and GPU culling.
    pub cave_active: bool,
    /// The brick holds interior quads (`meshing::InteriorFaces`): the x-ray's
    /// interior pass draws it (`xray::InteriorChunks`).
    pub interior: bool,
    /// [`DetailBand::code`].
    pub band: u32,
}

#[derive(Resource, Default)]
pub struct RemovedChunkQuads(pub Vec<Entity>);

/// `[pull-arena]` every [`ARENA_LOG_INTERVAL`] (`PERF_LOG=1` only).
const ARENA_LOG_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

fn log_arena_stats(arena: Res<arena::GpuQuadArena>, mut last: Local<Option<std::time::Instant>>) {
    let now = std::time::Instant::now();
    if last.is_some_and(|t| now - t < ARENA_LOG_INTERVAL) {
        return;
    }
    *last = Some(now);
    arena.log_stats();
}

pub fn extract_removed_chunk_quads(
    mut commands: Commands,
    mut removed: Extract<RemovedComponents<ChunkQuads>>,
    still_alive: Extract<Query<(), With<ChunkQuads>>>,
    render_entities: Query<(Entity, &MainEntity)>,
    mut out: ResMut<RemovedChunkQuads>,
) {
    out.0.clear();
    out.0.extend(
        removed
            .read()
            .filter(|entity| !still_alive.contains(*entity)),
    );
    // The common frame removes nothing: don't walk every render entity for it.
    if out.0.is_empty() {
        return;
    }
    let removed: bevy::platform::collections::HashSet<Entity> = out.0.iter().copied().collect();
    for (render_entity, main_entity) in &render_entities {
        if removed.contains(&main_entity.id()) {
            commands
                .entity(render_entity)
                .remove::<(ExtractedChunkQuads, prepare::GpuChunkQuads)>();
        }
    }
}

/// World-space bounds (centre, half extents) of a local `aabb` under
/// `transform`: the axis-aligned box around its 8 transformed corners (each
/// world half extent sums the local ones weighted by the absolute rotated,
/// scaled axes). Terrain is only translated and scaled, which reduces this
/// exactly to `scale * half`; a voxel body's bricks are rotated too.
pub fn world_aabb(transform: &GlobalTransform, aabb: &Aabb) -> (Vec3, Vec3) {
    let affine = transform.affine();
    let center = affine.transform_point3(Vec3::from(aabb.center));
    let half = Vec3::from(aabb.half_extents);
    let m = affine.matrix3;
    let half = m.x_axis.abs() * half.x + m.y_axis.abs() * half.y + m.z_axis.abs() * half.z;
    (center, Vec3::from(half))
}

#[allow(clippy::type_complexity)] // Bevy system: params are DI, not an API
pub fn extract_chunk_quads(
    mut commands: Commands,
    chunks: Extract<
        Query<
            (
                RenderEntity,
                &ChunkQuads,
                &GlobalTransform,
                &Aabb,
                &InheritedVisibility,
                Has<super::meshing::InteriorFaces>,
                Option<&DetailBand>,
            ),
            Or<(
                Changed<ChunkQuads>,
                Changed<GlobalTransform>,
                Changed<InheritedVisibility>,
                Changed<DetailBand>,
            )>,
        >,
    >,
    all_chunks: Extract<Query<&InheritedVisibility, With<ChunkQuads>>>,
    mut debug_frame: Local<u32>,
) {
    let mut extracted = 0u32;
    for (entity, quads, transform, aabb, vis, interior, band) in &chunks {
        extracted += 1;
        let mut ec = commands.entity(entity);
        // Cave-culling hides via Visibility -> InheritedVisibility=false.
        let cave_active = vis.get();
        let world_from_local = transform.to_matrix();
        let (center, half) = world_aabb(transform, aabb);
        ec.insert(ExtractedChunkQuads {
            records: quads.0.clone(),
            world_from_local,
            center,
            half_extents: half,
            cave_active,
            interior,
            band: DetailBand::code(band),
        });
    }
    // VOXEL_PULL_DEBUG=1: once a second, where do chunks disappear — here
    // (main-world Visibility, i.e. the cave-culling BFS) or at the per-view
    // frustum cull (queue.rs logs the other half)?
    *debug_frame = debug_frame.wrapping_add(1);
    if *debug_frame % 60 == 0 && std::env::var_os("VOXEL_PULL_DEBUG").is_some() {
        let total = all_chunks.iter().count();
        let hidden = all_chunks.iter().filter(|vis| !vis.get()).count();
        info!(
            "[pull-cull] extract: {total} ChunkQuads entities, {hidden} hidden by cave-culling BFS, \
             {extracted} re-extracted this frame"
        );
    }
}

/// Storage buffers the pull path binds in its busiest shader stage, the
/// fragment stage: quad records and chunk transforms (`voxel_pull.wgsl`
/// group(1) bindings 0 and 1, visible to both stages) plus the fragment-only
/// top-layer table (binding 4).
pub const PULL_STORAGE_BUFFERS_PER_STAGE: u32 = 3;

/// Whether the device can run the quad-record backends at all. They read the
/// records from storage buffers in the VERTEX stage, which WebGL2 (the wasm
/// build's `webgl2` backend) and GLES 3.0 do not have — creating
/// `voxel_pull_chunk_layout` there is a fatal wgpu validation error.
pub fn device_supports_quad_records(
    device: &bevy::render::renderer::RenderDevice,
    adapter: &bevy::render::renderer::RenderAdapter,
) -> bool {
    adapter
        .get_downlevel_capabilities()
        .flags
        .contains(bevy::render::render_resource::DownlevelFlags::VERTEX_STORAGE)
        && device.limits().max_storage_buffers_per_shader_stage >= PULL_STORAGE_BUFFERS_PER_STAGE
}

/// Index data at or above this many bytes gets its own buffer on a device without
/// `BASE_VERTEX` (WebGL2). Zero = every mesh's indices in their own buffer.
pub const WEBGL_INDEX_LARGE_THRESHOLD_BYTES: u64 = 0;

/// WebGL2 (no `BASE_VERTEX`): give every mesh its own INDEX buffer.
///
/// Bevy already gives each vertex array its own slab there, but packs index data into
/// shared slabs of up to 512 MiB. Firefox validates every `drawElements` by finding the
/// largest index in the drawn range on the CPU and caches that per buffer + range; with
/// thousands of chunk meshes drawing distinct ranges out of one index buffer (and slab
/// writes as chunks stream in) the cache thrashes and every draw re-scans its indices
/// every frame — a Firefox profile of the web build had the WebGL executor thread
/// (`CanvasRenderer`) 91% busy in `WebGLBuffer::GetIndexedFetchMaxVert` and the page
/// blocked 92% of the time waiting for it (`scripts/firefox_profile_summary.py`). One
/// buffer per mesh = one range per buffer, validated once at first draw.
fn webgl_own_index_buffers(app: &mut App) {
    let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
        return;
    };
    let world = render_app.world_mut();
    world.init_resource::<bevy::render::mesh::allocator::MeshAllocatorSettings>();
    let mut settings = world.resource_mut::<bevy::render::mesh::allocator::MeshAllocatorSettings>();
    settings.slab_allocator_settings.large_threshold = WEBGL_INDEX_LARGE_THRESHOLD_BYTES;
    info!(
        "[render] no BASE_VERTEX (WebGL2): every mesh gets its own index buffer \
         (large_threshold {WEBGL_INDEX_LARGE_THRESHOLD_BYTES} B)"
    );
}

/// `assets/shaders/voxel_pull.wgsl` and `voxel_pull_depth.wgsl`, compiled into
/// the binary like `voxel_material::VOXEL_TEXTURE_SHADER`: they decode the
/// quad record layout (`meshing/records.rs`), which changes with the Rust that
/// packs it, and a browser (a WebGPU build) would otherwise pair a cached old
/// `.wgsl` with new records.
pub const VOXEL_PULL_SHADER: Handle<Shader> = bevy::asset::uuid_handle!("a3c51e07-9b4d-4e2f-8d61-5f2e0b7c4d19");
/// See [`VOXEL_PULL_SHADER`].
pub const VOXEL_PULL_DEPTH_SHADER: Handle<Shader> = bevy::asset::uuid_handle!("c84f2d6a-1e93-4b07-a5c2-7d9e3f60b1a8");

pub struct VoxelPullPlugin;

impl Plugin for VoxelPullPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::load_internal_asset!(
            app,
            VOXEL_PULL_SHADER,
            "../../../../../assets/shaders/voxel_pull.wgsl",
            Shader::from_wgsl
        );
        bevy::asset::load_internal_asset!(
            app,
            VOXEL_PULL_DEPTH_SHADER,
            "../../../../../assets/shaders/voxel_pull_depth.wgsl",
            Shader::from_wgsl
        );
        app.add_plugins((
            ExtractResourcePlugin::<BlockArrayTexture>::default(),
            ExtractResourcePlugin::<BlockTopLayers>::default(),
            ExtractResourcePlugin::<super::boundaries::BoundaryUniform>::default(),
            ExtractResourcePlugin::<super::fortress_view::FortressViewUniform>::default(),
            ExtractResourcePlugin::<xray::XraySettings>::default(),
            ExtractResourcePlugin::<NearDetail>::default(),
        ));
        app.init_resource::<xray::XraySettings>();
        app.init_resource::<NearDetail>();
        app.add_systems(
            PostUpdate,
            track_near_detail.after(bevy::transform::TransformSystems::Propagate),
        );
        // Main-world tally driving the `[pull-mem]` headline log in
        // `apply_meshed_chunks`. Registered here (not in `renderer/mod.rs`) so
        // the pull path owns its own accounting resource.
        crate::session::SessionResetExt::add_session_reset::<super::meshing::PullMemStats>(app);
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_resource::<arena::GpuQuadArena>()
            .init_resource::<gpu::ActiveNearMode>()
            .init_resource::<gpu::NearRendererMetrics>()
            .init_resource::<RemovedChunkQuads>()
            .init_resource::<xray::InteriorChunks>()
            .init_resource::<SpecializedRenderPipelines<VoxelPullPipeline>>()
            .init_resource::<SpecializedRenderPipelines<VoxelPullDepthPipeline>>()
            .add_render_command::<Opaque3d, DrawVoxelPull>()
            .add_render_command::<Opaque3dPrepass, DrawVoxelPullDepth>()
            .add_render_command::<Shadow, DrawVoxelPullDepth>()
            .add_render_command::<AlphaMask3d, DrawVoxelPullCutout>()
            .add_render_command::<AlphaMask3dPrepass, DrawVoxelPullDepthCutout>()
            .add_render_command::<Shadow, DrawVoxelPullDepthCutout>()
            // The depth pipeline clones `PrepassPipeline`'s view layout, which
            // only exists after `init_prepass_pipeline` runs in `RenderStartup`.
            .add_systems(
                RenderStartup,
                init_voxel_pull_depth_pipeline.after(init_prepass_pipeline),
            )
            .add_systems(
                ExtractSchedule,
                (
                    extract_chunk_quads,
                    extract_removed_chunk_quads,
                    gpu::extract_requested_near_backend,
                ),
            )
            .add_systems(
                Render,
                (
                    // The pipelines exist only on a device that can run the
                    // pull path (see `finish`); without them nothing is drawn
                    // from records and these have no work.
                    prepare_chunk_quads
                        .in_set(RenderSystems::PrepareBindGroups)
                        .run_if(resource_exists::<VoxelPullPipeline>),
                    prepare_view_chunk_draws
                        .in_set(RenderSystems::PrepareBindGroups)
                        .after(prepare_chunk_quads),
                    gpu::prepare_active_near_mode.in_set(RenderSystems::PrepareResources),
                    // Before `Queue`, which picks the XRAY variants from it.
                    xray::track_interior_chunks.in_set(RenderSystems::PrepareAssets),
                    xray::prepare_xray_views
                        .in_set(RenderSystems::PrepareBindGroups)
                        .run_if(resource_exists::<VoxelPullPipeline>),
                    log_arena_stats
                        .after(prepare_chunk_quads)
                        .run_if(|| game::perf::budget_lines_requested()),
                    queue_voxel_pull
                        .in_set(RenderSystems::Queue)
                        .run_if(resource_exists::<VoxelPullPipeline>),
                    queue_voxel_pull_prepass
                        .in_set(RenderSystems::Queue)
                        .run_if(resource_exists::<VoxelPullDepthPipeline>),
                    queue_voxel_pull_shadows
                        .in_set(RenderSystems::Queue)
                        .run_if(resource_exists::<VoxelPullDepthPipeline>),
                ),
            );
    }

    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app(RenderApp) else {
            return;
        };
        let base_vertex = render_app
            .world()
            .resource::<bevy::render::renderer::RenderAdapter>()
            .get_downlevel_capabilities()
            .flags
            .contains(bevy::render::render_resource::DownlevelFlags::BASE_VERTEX);
        if !base_vertex {
            webgl_own_index_buffers(app);
        }
        let Some(render_app) = app.get_sub_app(RenderApp) else {
            return;
        };
        let quad_records_supported = {
            let world = render_app.world();
            device_supports_quad_records(
                world.resource::<bevy::render::renderer::RenderDevice>(),
                world.resource::<bevy::render::renderer::RenderAdapter>(),
            )
        };
        if !quad_records_supported {
            // `finish` runs before the first `Update`, so no chunk has been
            // meshed as records yet: meshing reads the downgraded config.
            if let Some(mut config) = app
                .world_mut()
                .get_resource_mut::<super::config::TerrainRenderConfig>()
            {
                let resolved = config.for_device(false);
                if *config != resolved {
                    warn!(
                        "[render] no vertex-stage storage buffers on this device (WebGL2?): \
                         near backend {} -> {}",
                        config.near.as_str(),
                        resolved.near.as_str()
                    );
                    *config = resolved;
                }
            }
            return;
        }
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        // Bevy 0.19 builds `MeshPipelineViewLayouts` in `RenderStartup`, after
        // `finish`, so the forward pipeline (which clones them) is created there
        // too — ahead of the depth pipeline, which reads it.
        render_app.add_systems(
            RenderStartup,
            (
                init_voxel_pull_pipeline
                    .after(bevy::pbr::init_mesh_pipeline_view_layouts)
                    .before(init_voxel_pull_depth_pipeline),
                xray::init_xray_stand_in.after(init_voxel_pull_pipeline),
            ),
        );
        // The x-ray's interior depth, ahead of the opaque pass that reads it.
        render_app.add_systems(
            Core3d,
            xray::xray_interior_pass
                .in_set(bevy::core_pipeline::Core3dSystems::MainPass)
                .before(bevy::core_pipeline::core_3d::main_opaque_pass_3d),
        );
        let telemetry_enabled = render_app
            .world()
            .contains_resource::<gpu::NearTelemetryEnabled>();

        let gpu_supported = {
            let world = render_app.world();
            gpu::runtime_capabilities(
                world
                    .resource::<bevy::render::batching::gpu_preprocessing::GpuPreprocessingSupport>(
                    ),
                world.resource::<bevy::render::renderer::RenderDevice>(),
                world.resource::<bevy::render::renderer::RenderAdapterInfo>(),
            )
            .gpu_culling
        };
        if gpu_supported {
            render_app
                .init_resource::<gpu::GpuNearCullPipeline>()
                .add_systems(
                    Render,
                    gpu::prepare_gpu_near_view_buffers
                        .in_set(RenderSystems::PrepareBindGroups)
                        .after(prepare_chunk_quads),
                );
            if telemetry_enabled {
                render_app
                    .init_resource::<gpu::NearTelemetryReadback>()
                    .add_systems(
                        Render,
                        gpu::begin_near_telemetry_frame.in_set(RenderSystems::PrepareResources),
                    );
            }
            let mut schedules = render_app
                .world_mut()
                .resource_mut::<bevy::ecs::schedule::Schedules>();
            gpu::add_near_gpu_systems(schedules.entry(Core3d), telemetry_enabled);
        }
    }
}

fn init_voxel_pull_pipeline(world: &mut World) {
    world.init_resource::<VoxelPullPipeline>();
}
