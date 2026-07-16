//! Deterministic production-scene render benchmark control and JSON output.
//!
//! The harness owns timing/readiness and samples Bevy diagnostics. Counters
//! that only a renderer implementation can know are explicit optional inputs
//! in [`BenchmarkCapabilityMetrics`]; absent inputs serialize as `null`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::AppExit;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::render::diagnostic::RenderDiagnosticsPlugin;
use bevy::render::renderer::RenderAdapterInfo;
use bevy::render::{Render, RenderApp, RenderSystems};
use serde::Serialize;

use super::bridge::{RegionRoots, SimTarget};
use super::config::TerrainRenderConfig;
use super::far_voxel::gpu::GpuFarBuffers;
use super::far_voxel::lifecycle::FarVoxelUploadPending;
use super::far_voxel::residency::FarVoxelResidency;
use super::far_voxel::{FarBrickBuildTask, FarVoxelReady};
use super::lod_rings::LodRings;
use super::meshing::lod::{LodBuildQueue, LodChunk, LodMeshTask, LodRasterReady};
use super::meshing::queue::{ChunkKey, RemeshQueue};
use super::meshing::MeshingTask;
use super::voxel_pull::arena::GpuQuadArena;
use super::{ClientUpdates, GameEvents};

pub const BENCHMARK_WIDTH: u32 = 1920;
pub const BENCHMARK_HEIGHT: u32 = 1080;
pub const REQUIRED_NEAR_REGIONS: usize = 9;
pub const STABLE_READY_FRAMES: u32 = 120;
pub const LOAD_TIMEOUT: Duration = Duration::from_secs(120);
pub const WARMUP_DURATION: Duration = Duration::from_secs(10);
pub const MEASUREMENT_DURATION: Duration = Duration::from_secs(30);
pub const BENCHMARK_YAW_RADIANS: f32 = std::f32::consts::TAU;

#[derive(Clone, Debug)]
pub struct BenchmarkRunSettings {
    pub output: PathBuf,
    pub repetition: u32,
}

impl BenchmarkRunSettings {
    fn from_env() -> Option<Self> {
        if !enabled() {
            return None;
        }
        let output = std::env::var_os("RENDER_BENCH_OUTPUT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/render-bench/render-bench.json"));
        let repetition = std::env::var("RENDER_BENCH_REPETITION")
            .ok()
            .map(|value| {
                value.parse::<u32>().unwrap_or_else(|_| {
                    panic!("invalid RENDER_BENCH_REPETITION value `{value}` (expected u32)")
                })
            })
            .unwrap_or(1);
        Some(Self { output, repetition })
    }
}

pub fn enabled() -> bool {
    std::env::var("RENDER_BENCH")
        .ok()
        .is_some_and(|value| !value.is_empty() && value != "0")
}

#[derive(Resource, Clone)]
struct BenchmarkSettingsResource(BenchmarkRunSettings);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkReadiness {
    pub near_regions: usize,
    pub remesh_queue_len: usize,
    pub lod_build_queue_len: usize,
    pub pending_lod_events: usize,
    pub in_flight_tasks: usize,
    pub far_residency_pending: usize,
    pub ready_lod_counts: BTreeMap<u32, usize>,
}

impl BenchmarkReadiness {
    fn gates_ready(&self) -> bool {
        let has_full_lod_table = game::lod::LOD_RING_TABLE.iter().all(|&(_, factor)| {
            self.ready_lod_counts
                .get(&factor)
                .is_some_and(|&count| count > 0)
        });
        self.near_regions >= REQUIRED_NEAR_REGIONS
            && self.remesh_queue_len == 0
            && self.lod_build_queue_len == 0
            && self.pending_lod_events == 0
            && self.in_flight_tasks == 0
            && self.far_residency_pending == 0
            && has_full_lod_table
    }
}

#[derive(Default)]
pub struct ReadinessTracker {
    previous_lod_counts: Option<BTreeMap<u32, usize>>,
    stable_frames: u32,
}

impl ReadinessTracker {
    pub fn observe(&mut self, readiness: &BenchmarkReadiness) -> bool {
        if !readiness.gates_ready() {
            self.previous_lod_counts = None;
            self.stable_frames = 0;
            return false;
        }
        if self.previous_lod_counts.as_ref() == Some(&readiness.ready_lod_counts) {
            self.stable_frames = self.stable_frames.saturating_add(1);
        } else {
            self.previous_lod_counts = Some(readiness.ready_lod_counts.clone());
            self.stable_frames = 1;
        }
        self.stable_frames >= STABLE_READY_FRAMES
    }

    pub const fn stable_frames(&self) -> u32 {
        self.stable_frames
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BenchmarkPhase {
    #[default]
    Loading,
    Warming,
    Measuring,
    Complete,
    Failed,
}

#[derive(Default)]
pub struct BenchmarkController {
    phase: BenchmarkPhase,
    phase_started: Duration,
}

impl BenchmarkController {
    pub fn advance(&mut self, elapsed: Duration, ready: bool) {
        match self.phase {
            BenchmarkPhase::Loading if ready => {
                self.phase = BenchmarkPhase::Warming;
                self.phase_started = elapsed;
            }
            BenchmarkPhase::Loading if elapsed > LOAD_TIMEOUT => {
                self.phase = BenchmarkPhase::Failed;
            }
            BenchmarkPhase::Warming
                if elapsed.saturating_sub(self.phase_started) >= WARMUP_DURATION =>
            {
                self.phase = BenchmarkPhase::Measuring;
                self.phase_started = elapsed;
            }
            BenchmarkPhase::Measuring
                if elapsed.saturating_sub(self.phase_started) >= MEASUREMENT_DURATION =>
            {
                self.phase = BenchmarkPhase::Complete;
            }
            _ => {}
        }
    }

    pub const fn phase(&self) -> BenchmarkPhase {
        self.phase
    }

    fn phase_elapsed(&self, elapsed: Duration) -> Duration {
        elapsed.saturating_sub(self.phase_started)
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct BenchmarkCapabilityMetrics {
    pub active_chunks: Option<u64>,
    pub frustum_chunks: Option<u64>,
    /// Exact main-forward HZB survivor count; unavailable when the forward
    /// source is the frustum fallback.
    pub hzb_chunks: Option<u64>,
    /// Total main-forward terrain indirect command records submitted across
    /// near and far renderers. Near fixed mode counts all stable slots,
    /// including commands whose culled vertex count is zero; the far pass
    /// contributes one compacted proxy command when it has active cells.
    pub indirect_draws: Option<u64>,
    pub far_cells: Option<u64>,
    pub unique_bricks: Option<u64>,
    pub brick_dedup_ratio: Option<f64>,
    pub reserved_gpu_bytes: Option<u64>,
}

#[derive(Resource, Default)]
pub struct BenchmarkCapabilityMetricsInput(pub BenchmarkCapabilityMetrics);

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Percentiles {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
}

impl Percentiles {
    pub fn from_samples(samples: &[f64]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let nearest_rank = |percent: usize| {
            let rank = (percent * sorted.len()).div_ceil(100).max(1);
            sorted[rank - 1]
        };
        Some(Self {
            p50: nearest_rank(50),
            p95: nearest_rank(95),
            p99: nearest_rank(99),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CountSummary {
    pub min: u64,
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub max: u64,
}

impl CountSummary {
    fn from_samples(samples: &[u64]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let nearest_rank = |percent: usize| {
            let rank = (percent * sorted.len()).div_ceil(100).max(1);
            sorted[rank - 1]
        };
        Some(Self {
            min: sorted[0],
            p50: nearest_rank(50),
            p95: nearest_rank(95),
            p99: nearest_rank(99),
            max: *sorted.last().expect("non-empty checked above"),
        })
    }
}

#[derive(Default)]
struct BenchmarkSamples {
    frame_ms: Vec<f64>,
    gpu_frame_ms: Vec<f64>,
    pass_gpu_ms: BTreeMap<String, Vec<f64>>,
    active_chunks: Vec<u64>,
    frustum_chunks: Vec<u64>,
    hzb_chunks: Vec<u64>,
    indirect_draws: Vec<u64>,
    far_cells: Vec<u64>,
    unique_bricks: Vec<u64>,
    brick_dedup_ratio: Vec<f64>,
    reserved_gpu_bytes: Vec<u64>,
}

impl BenchmarkSamples {
    fn push_capabilities(&mut self, metrics: &BenchmarkCapabilityMetrics) {
        push_optional(&mut self.active_chunks, metrics.active_chunks);
        push_optional(&mut self.frustum_chunks, metrics.frustum_chunks);
        push_optional(&mut self.hzb_chunks, metrics.hzb_chunks);
        push_optional(&mut self.indirect_draws, metrics.indirect_draws);
        push_optional(&mut self.far_cells, metrics.far_cells);
        push_optional(&mut self.unique_bricks, metrics.unique_bricks);
        push_optional(&mut self.brick_dedup_ratio, metrics.brick_dedup_ratio);
        push_optional(&mut self.reserved_gpu_bytes, metrics.reserved_gpu_bytes);
    }
}

fn push_optional<T>(values: &mut Vec<T>, value: Option<T>) {
    if let Some(value) = value {
        values.push(value);
    }
}

#[derive(Clone, Debug, Default, Serialize)]
struct AdapterMetadata {
    name: String,
    backend: String,
    device_type: String,
    driver: String,
    driver_info: String,
    vendor: u32,
    device: u32,
}

#[derive(Clone, Debug, Default)]
struct RenderThreadSnapshot {
    adapter: Option<AdapterMetadata>,
    near: Option<super::voxel_pull::gpu::NearRendererMetrics>,
    far_pass: Option<super::far_voxel::pass::FarVoxelPassMetrics>,
    reserved_gpu_bytes: Option<u64>,
}

#[derive(Resource, Clone, Default)]
struct SharedRenderThreadSnapshot(Arc<Mutex<RenderThreadSnapshot>>);

#[derive(Resource, Default)]
struct BenchmarkRuntime {
    controller: BenchmarkController,
    readiness: ReadinessTracker,
    samples: BenchmarkSamples,
    last_readiness: BenchmarkReadiness,
    camera_base: Option<(Entity, Quat)>,
    snapshot_frozen: bool,
    output_written: bool,
}

pub struct RenderBenchmarkPlugin {
    settings: BenchmarkRunSettings,
}

impl RenderBenchmarkPlugin {
    pub fn from_env() -> Option<Self> {
        BenchmarkRunSettings::from_env().map(|settings| Self { settings })
    }
}

impl Plugin for RenderBenchmarkPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<RenderDiagnosticsPlugin>() {
            app.add_plugins(RenderDiagnosticsPlugin);
        }

        let shared = SharedRenderThreadSnapshot::default();
        let far_draw_gate = super::far_voxel::pass::FarVoxelDrawGate::disabled();
        app.insert_resource(BenchmarkSettingsResource(self.settings.clone()))
            .insert_resource(shared.clone())
            .insert_resource(far_draw_gate.clone())
            .init_resource::<BenchmarkCapabilityMetricsInput>()
            .init_resource::<BenchmarkRuntime>()
            .add_systems(
                Update,
                run_benchmark
                    .after(super::bridge::drain_client_updates)
                    .after(super::lod_rings::apply_lod_events)
                    .after(super::meshing::lod::apply_lod_meshes)
                    .after(super::far_voxel::gpu::publish_far_uploads)
                    .after(super::interpolate::interpolate_transforms),
            );

        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app
                .insert_resource(shared)
                .insert_resource(far_draw_gate)
                .init_resource::<super::voxel_pull::gpu::NearTelemetryEnabled>()
                .add_systems(
                    Render,
                    (
                        super::voxel_pull::gpu::drain_near_telemetry_results,
                        capture_render_thread_snapshot,
                    )
                        .chain()
                        .in_set(RenderSystems::Cleanup),
                );
        }
    }
}

fn capture_render_thread_snapshot(
    adapter: Res<RenderAdapterInfo>,
    arena: Option<Res<GpuQuadArena>>,
    near: Option<Res<super::voxel_pull::gpu::NearRendererMetrics>>,
    far_pass: Option<Res<super::far_voxel::pass::FarVoxelPassMetrics>>,
    far_buffers: Option<Res<GpuFarBuffers>>,
    shared: Res<SharedRenderThreadSnapshot>,
) {
    let mut snapshot = shared
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    snapshot.adapter = Some(AdapterMetadata {
        name: adapter.name.clone(),
        backend: format!("{:?}", adapter.backend),
        device_type: format!("{:?}", adapter.device_type),
        driver: adapter.driver.clone(),
        driver_info: adapter.driver_info.clone(),
        vendor: adapter.vendor,
        device: adapter.device,
    });
    snapshot.near = near.as_deref().copied();
    snapshot.far_pass = far_pass.as_deref().copied();
    let near_bytes = arena.map_or(0, |arena| {
        arena.records.capacity() * std::mem::size_of::<super::meshing::records::QuadRecord>()
            + arena.transforms.capacity() * std::mem::size_of::<Mat4>()
            + arena.metadata.capacity()
                * std::mem::size_of::<super::voxel_pull::arena::GpuNearChunkMeta>()
    });
    let far_bytes = far_buffers
        .as_deref()
        .map_or(0, GpuFarBuffers::reserved_bytes);
    snapshot.reserved_gpu_bytes = Some((near_bytes + far_bytes) as u64);
}

fn apply_render_thread_snapshot(
    snapshot: &RenderThreadSnapshot,
    metrics: &mut BenchmarkCapabilityMetrics,
) {
    let mut near_indirect_draws = None;
    if let Some(near) = snapshot.near {
        metrics.active_chunks = Some(u64::from(near.active_chunks));
        metrics.frustum_chunks = Some(u64::from(near.frustum_visible_chunks));
        metrics.hzb_chunks = near.hzb_visible_chunks.map(u64::from);
        near_indirect_draws = near.indirect_draw_count.map(u64::from);
    }
    let far_indirect_draws = snapshot
        .far_pass
        .map_or(0, |far| u64::from(far.indirect_draws));
    metrics.indirect_draws =
        near_indirect_draws.map(|near| near.saturating_add(far_indirect_draws));
    metrics.reserved_gpu_bytes = snapshot.reserved_gpu_bytes;
}

#[derive(SystemParam)]
struct BenchmarkScene<'w, 's> {
    client_updates: Res<'w, ClientUpdates>,
    game_events: Res<'w, GameEvents>,
    hud_status: Res<'w, super::hud::HudStatus>,
    roots: Res<'w, RegionRoots>,
    remesh_queue: Res<'w, RemeshQueue>,
    lod_build_queue: Res<'w, LodBuildQueue>,
    lod_rings: Res<'w, LodRings>,
    in_flight_near: Query<'w, 's, (), With<MeshingTask>>,
    in_flight_lod: Query<'w, 's, (), With<LodMeshTask>>,
    in_flight_bricks: Query<'w, 's, (), With<FarBrickBuildTask>>,
    in_flight_far_uploads: Query<'w, 's, (), With<FarVoxelUploadPending>>,
    chunks: Query<'w, 's, (), With<ChunkKey>>,
    far_cells: Query<'w, 's, (), With<LodChunk>>,
    ready_far_cells: Query<'w, 's, &'static LodChunk, ReadyFarCellFilter>,
    far_residency: Option<Res<'w, FarVoxelResidency>>,
}

fn expected_lod_keys(
    viewer: game::RegionCoords,
    near_regions: &BTreeSet<game::RegionCoords>,
) -> BTreeSet<game::lod::LodKey> {
    crate::instance::desired_lod(
        viewer,
        near_regions,
        &BTreeMap::new(),
        game::lod::LOD_RING_TABLE,
        game::lod::LOD_STICKY_BAND,
    )
    .1
}

fn snapshot_has_all_lod_keys(
    viewer: Option<game::RegionCoords>,
    near_regions: &BTreeSet<game::RegionCoords>,
    loaded: impl IntoIterator<Item = game::lod::LodKey>,
) -> bool {
    let Some(viewer) = viewer else {
        return false;
    };
    loaded.into_iter().collect::<BTreeSet<_>>() == expected_lod_keys(viewer, near_regions)
}

type ReadyFarCellFilter = Or<(
    With<Mesh3d>,
    With<super::voxel_pull::ChunkQuads>,
    With<FarVoxelReady>,
    With<LodRasterReady>,
)>;

#[allow(clippy::too_many_arguments)]
fn run_benchmark(
    time: Res<Time<Real>>,
    diagnostics: Res<DiagnosticsStore>,
    settings: Res<BenchmarkSettingsResource>,
    render_config: Res<TerrainRenderConfig>,
    scene: BenchmarkScene,
    mut cameras: Query<(Entity, &mut Transform, Option<&SimTarget>, &Camera), With<Camera3d>>,
    shared: Res<SharedRenderThreadSnapshot>,
    far_draw_gate: Res<super::far_voxel::pass::FarVoxelDrawGate>,
    mut capabilities: ResMut<BenchmarkCapabilityMetricsInput>,
    mut runtime: ResMut<BenchmarkRuntime>,
    mut exits: MessageWriter<AppExit>,
) {
    let elapsed = time.elapsed();
    if !runtime.snapshot_frozen
        && scene.roots.0.len() >= REQUIRED_NEAR_REGIONS
        && scene.client_updates.0.is_empty()
        && scene.lod_rings.pending.is_empty()
    {
        let near_regions = scene.roots.0.keys().copied().collect::<BTreeSet<_>>();
        if snapshot_has_all_lod_keys(
            scene.hud_status.viewer_region,
            &near_regions,
            scene.lod_rings.roots.keys().copied(),
        ) {
            runtime.snapshot_frozen = true;
            if let Err(error) = scene.game_events.0.send(game::GameEventKind::Quit) {
                warn!("render benchmark could not stop the completed simulation snapshot: {error}");
            } else {
                info!(
                    "render benchmark captured the complete production snapshot; simulation stopped while renderer queues drain"
                );
            }
        }
    }
    let mut ready_lod_counts = BTreeMap::new();
    for lod in &scene.ready_far_cells {
        *ready_lod_counts.entry(lod.factor).or_insert(0) += 1;
    }
    let readiness = BenchmarkReadiness {
        near_regions: scene.roots.0.len(),
        remesh_queue_len: scene.remesh_queue.dirty.len(),
        lod_build_queue_len: scene.lod_build_queue.pending_count(),
        pending_lod_events: scene.lod_rings.pending.len(),
        in_flight_tasks: scene.in_flight_near.iter().count()
            + scene.in_flight_lod.iter().count()
            + scene.in_flight_bricks.iter().count()
            + scene.in_flight_far_uploads.iter().count(),
        far_residency_pending: scene
            .far_residency
            .as_deref()
            .map_or(0, FarVoxelResidency::pending_work_count),
        ready_lod_counts,
    };
    let ready = runtime.readiness.observe(&readiness);
    runtime.last_readiness = readiness;

    let previous_phase = runtime.controller.phase();
    runtime.controller.advance(elapsed, ready);
    let phase = runtime.controller.phase();
    if phase != previous_phase {
        info!("render benchmark phase: {previous_phase:?} -> {phase:?}");
        if phase == BenchmarkPhase::Warming {
            far_draw_gate.enable();
            info!("render benchmark enabled the completed far scene for warmup");
        }
    }

    if !render_config.near.uses_quad_records() {
        capabilities.0.active_chunks = Some(scene.chunks.iter().count() as u64);
        capabilities.0.frustum_chunks = None;
        capabilities.0.hzb_chunks = None;
        capabilities.0.indirect_draws = Some(0);
    }
    capabilities.0.far_cells = Some(scene.far_cells.iter().count() as u64);
    if let Some(residency) = scene.far_residency.as_deref() {
        capabilities.0.unique_bricks = Some(residency.unique_bricks() as u64);
        capabilities.0.brick_dedup_ratio = residency.brick_dedup_ratio();
    }
    let render_snapshot = shared
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if render_config.near.uses_quad_records() {
        apply_render_thread_snapshot(&render_snapshot, &mut capabilities.0);
    } else {
        capabilities.0.reserved_gpu_bytes = render_snapshot.reserved_gpu_bytes;
    }

    if phase == BenchmarkPhase::Measuring {
        if let Some(frame_ms) = diagnostics
            .get(&FrameTimeDiagnosticsPlugin::FRAME_TIME)
            .and_then(|diagnostic| diagnostic.value())
        {
            runtime.samples.frame_ms.push(frame_ms);
        }
        let mut gpu_frame_ms = 0.0;
        let mut has_gpu_timing = false;
        for diagnostic in diagnostics.iter() {
            let path = diagnostic.path().to_string();
            if path.starts_with("render/") && path.ends_with("/elapsed_gpu") {
                if let Some(value) = diagnostic.value() {
                    runtime
                        .samples
                        .pass_gpu_ms
                        .entry(path)
                        .or_default()
                        .push(value);
                    gpu_frame_ms += value;
                    has_gpu_timing = true;
                }
            }
        }
        if has_gpu_timing {
            runtime.samples.gpu_frame_ms.push(gpu_frame_ms);
        }
        let capability_snapshot = capabilities.0.clone();
        runtime.samples.push_capabilities(&capability_snapshot);

        let progress = (runtime.controller.phase_elapsed(elapsed).as_secs_f32()
            / MEASUREMENT_DURATION.as_secs_f32())
        .clamp(0.0, 1.0);
        if let Some((entity, mut transform, target, _)) = cameras
            .iter_mut()
            .find(|(_, _, _, camera)| camera.is_active)
        {
            let base = match runtime.camera_base {
                Some((base_entity, rotation)) if base_entity == entity => rotation,
                _ => {
                    let rotation = target.map_or(transform.rotation, |target| target.rot);
                    runtime.camera_base = Some((entity, rotation));
                    rotation
                }
            };
            transform.rotation = Quat::from_rotation_y(-BENCHMARK_YAW_RADIANS * progress) * base;
        }
    }

    if matches!(phase, BenchmarkPhase::Complete | BenchmarkPhase::Failed) && !runtime.output_written
    {
        runtime.output_written = true;
        let failed = phase == BenchmarkPhase::Failed;
        let result = build_result(
            failed,
            &settings.0,
            *render_config,
            &runtime,
            render_snapshot.adapter,
        );
        match write_result(&settings.0.output, &result) {
            Ok(()) => {
                info!("render benchmark JSON: {}", settings.0.output.display());
                exits.write(if failed {
                    AppExit::error()
                } else {
                    AppExit::Success
                });
            }
            Err(error) => {
                error!(
                    "failed to write render benchmark JSON {}: {error}",
                    settings.0.output.display()
                );
                exits.write(AppExit::error());
            }
        }
    }
}

#[derive(Serialize)]
struct LodRingMetadata {
    bound: i32,
    factor: u32,
}

#[derive(Serialize)]
struct BenchmarkMetadata {
    repetition: u32,
    resolution: [u32; 2],
    vsync: bool,
    required_near_regions: usize,
    stable_ready_frames: u32,
    load_timeout_seconds: u64,
    warmup_seconds: u64,
    measurement_seconds: u64,
    yaw_degrees: u32,
    simulation_snapshot_frozen: bool,
    world_seed: u64,
    lod_ring_table: Vec<LodRingMetadata>,
    near_backend: &'static str,
    far_backend: &'static str,
    adapter: Option<AdapterMetadata>,
}

#[derive(Serialize)]
struct BenchmarkMetricSummary {
    frame_ms: Option<Percentiles>,
    gpu_frame_ms: Option<Percentiles>,
    pass_gpu_ms: BTreeMap<String, Percentiles>,
    active_chunks: Option<CountSummary>,
    frustum_chunks: Option<CountSummary>,
    hzb_chunks: Option<CountSummary>,
    indirect_draws: Option<CountSummary>,
    far_cells: Option<CountSummary>,
    unique_bricks: Option<CountSummary>,
    brick_dedup_ratio: Option<Percentiles>,
    reserved_gpu_bytes: Option<CountSummary>,
}

#[derive(Serialize)]
struct BenchmarkResult {
    schema_version: u32,
    status: &'static str,
    failure_reason: Option<String>,
    benchmark: BenchmarkMetadata,
    readiness_at_exit: SerializableReadiness,
    metrics: BenchmarkMetricSummary,
    unavailable_metrics: Vec<&'static str>,
}

#[derive(Serialize)]
struct SerializableReadiness {
    near_regions: usize,
    remesh_queue_len: usize,
    lod_build_queue_len: usize,
    pending_lod_events: usize,
    in_flight_tasks: usize,
    far_residency_pending: usize,
    ready_lod_counts: BTreeMap<u32, usize>,
    stable_frames: u32,
}

fn build_result(
    failed: bool,
    settings: &BenchmarkRunSettings,
    config: TerrainRenderConfig,
    runtime: &BenchmarkRuntime,
    adapter: Option<AdapterMetadata>,
) -> BenchmarkResult {
    let samples = &runtime.samples;
    let pass_gpu_ms = samples
        .pass_gpu_ms
        .iter()
        .filter_map(|(path, samples)| {
            Percentiles::from_samples(samples).map(|summary| (path.clone(), summary))
        })
        .collect();
    let metrics = BenchmarkMetricSummary {
        frame_ms: Percentiles::from_samples(&samples.frame_ms),
        gpu_frame_ms: Percentiles::from_samples(&samples.gpu_frame_ms),
        pass_gpu_ms,
        active_chunks: CountSummary::from_samples(&samples.active_chunks),
        frustum_chunks: CountSummary::from_samples(&samples.frustum_chunks),
        hzb_chunks: CountSummary::from_samples(&samples.hzb_chunks),
        indirect_draws: CountSummary::from_samples(&samples.indirect_draws),
        far_cells: CountSummary::from_samples(&samples.far_cells),
        unique_bricks: CountSummary::from_samples(&samples.unique_bricks),
        brick_dedup_ratio: Percentiles::from_samples(&samples.brick_dedup_ratio),
        reserved_gpu_bytes: CountSummary::from_samples(&samples.reserved_gpu_bytes),
    };
    let unavailable_metrics = [
        ("gpu_frame_ms", metrics.gpu_frame_ms.is_none()),
        ("hzb_chunks", metrics.hzb_chunks.is_none()),
        ("indirect_draws", metrics.indirect_draws.is_none()),
        ("unique_bricks", metrics.unique_bricks.is_none()),
        ("brick_dedup_ratio", metrics.brick_dedup_ratio.is_none()),
        ("reserved_gpu_bytes", metrics.reserved_gpu_bytes.is_none()),
    ]
    .into_iter()
    .filter_map(|(name, unavailable)| unavailable.then_some(name))
    .collect();
    BenchmarkResult {
        schema_version: 1,
        status: if failed { "failed" } else { "complete" },
        failure_reason: failed.then(|| {
            format!(
                "scene did not stabilize within {} seconds",
                LOAD_TIMEOUT.as_secs()
            )
        }),
        benchmark: BenchmarkMetadata {
            repetition: settings.repetition,
            resolution: [BENCHMARK_WIDTH, BENCHMARK_HEIGHT],
            vsync: false,
            required_near_regions: REQUIRED_NEAR_REGIONS,
            stable_ready_frames: STABLE_READY_FRAMES,
            load_timeout_seconds: LOAD_TIMEOUT.as_secs(),
            warmup_seconds: WARMUP_DURATION.as_secs(),
            measurement_seconds: MEASUREMENT_DURATION.as_secs(),
            yaw_degrees: 360,
            simulation_snapshot_frozen: runtime.snapshot_frozen,
            world_seed: worldgen::WORLD_SEED,
            lod_ring_table: game::lod::LOD_RING_TABLE
                .iter()
                .map(|&(bound, factor)| LodRingMetadata { bound, factor })
                .collect(),
            near_backend: config.near.as_str(),
            far_backend: config.far.as_str(),
            adapter,
        },
        readiness_at_exit: SerializableReadiness {
            near_regions: runtime.last_readiness.near_regions,
            remesh_queue_len: runtime.last_readiness.remesh_queue_len,
            lod_build_queue_len: runtime.last_readiness.lod_build_queue_len,
            pending_lod_events: runtime.last_readiness.pending_lod_events,
            in_flight_tasks: runtime.last_readiness.in_flight_tasks,
            far_residency_pending: runtime.last_readiness.far_residency_pending,
            ready_lod_counts: runtime.last_readiness.ready_lod_counts.clone(),
            stable_frames: runtime.readiness.stable_frames(),
        },
        metrics,
        unavailable_metrics,
    }
}

fn write_result(path: &PathBuf, result: &BenchmarkResult) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = File::create(path)?;
    serde_json::to_writer_pretty(file, result).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::Duration;

    use super::*;

    fn ready(counts: &[(u32, usize)]) -> BenchmarkReadiness {
        let mut ready_lod_counts = game::lod::LOD_RING_TABLE
            .iter()
            .map(|&(_, factor)| (factor, 1))
            .collect::<BTreeMap<_, _>>();
        ready_lod_counts.extend(counts.iter().copied());
        BenchmarkReadiness {
            near_regions: REQUIRED_NEAR_REGIONS,
            remesh_queue_len: 0,
            lod_build_queue_len: 0,
            pending_lod_events: 0,
            in_flight_tasks: 0,
            far_residency_pending: 0,
            ready_lod_counts,
        }
    }

    #[test]
    fn production_snapshot_requires_every_expected_lod_key() {
        let viewer = game::RegionCoords::new(0, 0);
        let near_regions = viewer.window_3x3().into_iter().collect::<BTreeSet<_>>();
        let expected = expected_lod_keys(viewer, &near_regions);

        assert!(!expected.is_empty());
        assert!(snapshot_has_all_lod_keys(
            Some(viewer),
            &near_regions,
            expected.iter().copied(),
        ));

        let mut incomplete = expected;
        incomplete.pop_first();
        assert!(!snapshot_has_all_lod_keys(
            Some(viewer),
            &near_regions,
            incomplete.iter().copied(),
        ));
        assert!(!snapshot_has_all_lod_keys(
            None,
            &near_regions,
            incomplete.iter().copied(),
        ));
    }

    #[test]
    fn readiness_requires_120_unchanged_frames_and_resets_on_change() {
        let mut tracker = ReadinessTracker::default();
        for _ in 0..(STABLE_READY_FRAMES - 1) {
            assert!(!tracker.observe(&ready(&[(2, 8), (4, 12)])));
        }
        assert!(tracker.observe(&ready(&[(2, 8), (4, 12)])));

        assert!(!tracker.observe(&ready(&[(2, 8), (4, 13)])));
        assert_eq!(tracker.stable_frames(), 1);
        assert!(!tracker.observe(&BenchmarkReadiness {
            remesh_queue_len: 1,
            ..ready(&[(2, 8), (4, 13)])
        }));
        assert_eq!(tracker.stable_frames(), 0);
    }

    #[test]
    fn readiness_rejects_an_observable_lod_build_backlog() {
        let mut tracker = ReadinessTracker::default();
        assert!(!tracker.observe(&BenchmarkReadiness {
            lod_build_queue_len: 1,
            ..ready(&[])
        }));
        assert_eq!(tracker.stable_frames(), 0);
    }

    #[test]
    fn readiness_rejects_far_residency_metadata_work() {
        let mut tracker = ReadinessTracker::default();
        assert!(!tracker.observe(&BenchmarkReadiness {
            far_residency_pending: 2,
            ..ready(&[])
        }));
        assert_eq!(tracker.stable_frames(), 0);
    }

    #[test]
    fn phase_timing_is_load_then_warm_then_fixed_measurement() {
        let mut controller = BenchmarkController::default();
        controller.advance(Duration::from_secs(5), true);
        assert_eq!(controller.phase(), BenchmarkPhase::Warming);

        controller.advance(Duration::from_secs(14), false);
        assert_eq!(controller.phase(), BenchmarkPhase::Warming);
        controller.advance(Duration::from_secs(15), false);
        assert_eq!(controller.phase(), BenchmarkPhase::Measuring);

        controller.advance(Duration::from_secs(44), false);
        assert_eq!(controller.phase(), BenchmarkPhase::Measuring);
        controller.advance(Duration::from_secs(45), false);
        assert_eq!(controller.phase(), BenchmarkPhase::Complete);
    }

    #[test]
    fn loading_fails_after_120_seconds() {
        let mut controller = BenchmarkController::default();
        controller.advance(LOAD_TIMEOUT + Duration::from_millis(1), false);
        assert_eq!(controller.phase(), BenchmarkPhase::Failed);
    }

    #[test]
    fn percentile_summary_uses_nearest_rank() {
        let values: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(
            Percentiles::from_samples(&values),
            Some(Percentiles {
                p50: 50.0,
                p95: 95.0,
                p99: 99.0,
            })
        );
        assert_eq!(Percentiles::from_samples(&[]), None);
    }

    #[test]
    fn unavailable_capability_metrics_serialize_as_null() {
        let value = serde_json::to_value(BenchmarkCapabilityMetrics::default()).unwrap();
        assert!(value["hzb_chunks"].is_null());
        assert!(value["indirect_draws"].is_null());
        assert!(value["unique_bricks"].is_null());
        assert!(value["brick_dedup_ratio"].is_null());
        assert!(value["reserved_gpu_bytes"].is_null());
    }

    #[test]
    fn render_snapshot_combines_near_and_far_indirect_draws() {
        let snapshot = RenderThreadSnapshot {
            near: Some(super::super::voxel_pull::gpu::NearRendererMetrics {
                active_chunks: 17,
                frustum_visible_chunks: 11,
                hzb_visible_chunks: Some(7),
                indirect_draw_count: Some(3),
            }),
            far_pass: Some(super::super::far_voxel::pass::FarVoxelPassMetrics {
                indirect_draws: 1,
                ..default()
            }),
            reserved_gpu_bytes: Some(4096),
            ..default()
        };
        let mut metrics = BenchmarkCapabilityMetrics::default();

        apply_render_thread_snapshot(&snapshot, &mut metrics);

        assert_eq!(metrics.active_chunks, Some(17));
        assert_eq!(metrics.frustum_chunks, Some(11));
        assert_eq!(metrics.hzb_chunks, Some(7));
        assert_eq!(metrics.indirect_draws, Some(4));
        assert_eq!(metrics.reserved_gpu_bytes, Some(4096));
    }

    #[test]
    fn partial_indirect_aggregate_treats_missing_far_as_zero_but_unknown_near_as_unknown() {
        let mut snapshot = RenderThreadSnapshot {
            near: Some(super::super::voxel_pull::gpu::NearRendererMetrics {
                indirect_draw_count: Some(3),
                ..default()
            }),
            ..default()
        };
        let mut metrics = BenchmarkCapabilityMetrics::default();

        apply_render_thread_snapshot(&snapshot, &mut metrics);
        assert_eq!(metrics.indirect_draws, Some(3));

        snapshot
            .near
            .as_mut()
            .expect("near metrics are present")
            .indirect_draw_count = None;
        snapshot.far_pass = Some(super::super::far_voxel::pass::FarVoxelPassMetrics {
            indirect_draws: 1,
            ..default()
        });
        apply_render_thread_snapshot(&snapshot, &mut metrics);
        assert_eq!(metrics.indirect_draws, None);
    }
}
