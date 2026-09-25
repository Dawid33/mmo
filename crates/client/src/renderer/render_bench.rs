//! Deterministic production-scene render benchmark control and JSON output.
//!
//! The harness owns timing/readiness and samples Bevy diagnostics. Counters
//! that only a renderer implementation can know are explicit optional inputs
//! in [`BenchmarkCapabilityMetrics`]; absent inputs serialize as `null`.

mod metrics;
mod output;
mod telemetry;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use bevy::app::AppExit;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::render::diagnostic::RenderDiagnosticsPlugin;
use bevy::render::{Render, RenderApp, RenderSystems};

use super::bridge::{RegionRoots, SimTarget};
use super::config::TerrainRenderConfig;
use crate::world_scale::WorldScale;
use super::far_voxel::lifecycle::FarVoxelUploadPending;
use super::far_voxel::residency::FarVoxelResidency;
use super::far_voxel::{FarBrickBuildTask, FarRasterFallback, FarVoxelReady};
use super::lod_rings::LodRings;
use super::meshing::lod::{LodBuildQueue, LodChunk, LodMeshTask, LodRasterReady};
use super::meshing::queue::{ChunkKey, RemeshQueue};
use super::meshing::MeshingTask;
use super::meshing::TerrainQuadCount;
use super::voxel_pull::gpu::{ActiveNearMode, ForwardCullSource};
use super::voxel_pull::ChunkQuads;
use super::{ClientUpdates, GameEvents};
use metrics::{CountSummary, FloatSummary, RenderDiagnosticSamples};
use output::{
    write_result_atomic, AdapterMetadata, BenchmarkMetadata, BenchmarkMetrics, BenchmarkResult,
    DrawMetrics, DrawScopeMetrics, GeometryMetrics, GeometryScopeMetrics, Instrumentation,
    MemoryMetrics, MemoryScopeMetrics, PipelineStatistics, SerializableReadiness, TimingMetrics,
    UnavailableMetric, UnavailableReason, VisibilityMetrics, VisibilityScopeMetrics,
};
use telemetry::{capture_render_thread_snapshot, translate_snapshot, SharedRenderThreadSnapshot};

pub const BENCHMARK_WIDTH: u32 = 1920;
pub const BENCHMARK_HEIGHT: u32 = 1080;
pub const STABLE_READY_FRAMES: u32 = 120;
/// Default load budget; `RENDER_BENCH_LOAD_TIMEOUT_SECS` overrides it. The
/// readiness gate counts *frames* (`STABLE_READY_FRAMES`) while this counts
/// wall-clock seconds, so a slow adapter needs a longer budget, not a fix.
pub const LOAD_TIMEOUT: Duration = Duration::from_secs(120);
pub const WARMUP_DURATION: Duration = Duration::from_secs(10);
pub const MEASUREMENT_DURATION: Duration = Duration::from_secs(30);
pub const BENCHMARK_YAW_RADIANS: f32 = std::f32::consts::TAU;

#[derive(Clone, Debug)]
pub struct BenchmarkRunSettings {
    pub output: PathBuf,
    pub repetition: u32,
    pub instrumentation: Instrumentation,
    pub git_revision: Option<String>,
    pub git_dirty: Option<bool>,
    pub label: Option<String>,
    pub profile: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub cpu: Option<String>,
    pub expected_repetitions: Option<u32>,
    pub client_worker_threads: Option<u32>,
    pub tokio_worker_threads: Option<u32>,
    pub requested_near_backend: Option<String>,
    pub requested_far_backend: Option<String>,
    pub load_timeout: Duration,
}

impl BenchmarkRunSettings {
    fn from_env() -> Option<Self> {
        if !enabled() {
            return None;
        }
        Some(
            Self::from_values(|name| std::env::var(name).ok())
                .unwrap_or_else(|error| panic!("{error}")),
        )
    }

    fn from_values(mut get: impl FnMut(&str) -> Option<String>) -> Result<Self, String> {
        let parse_u32 = |name: &str, value: Option<String>| -> Result<Option<u32>, String> {
            value
                .map(|value| {
                    value
                        .parse::<u32>()
                        .map_err(|_| format!("invalid {name} value `{value}` (expected u32)"))
                })
                .transpose()
        };
        let git_dirty = get("RENDER_BENCH_GIT_DIRTY")
            .map(|value| match value.as_str() {
                "0" => Ok(false),
                "1" => Ok(true),
                _ => Err(format!(
                    "invalid RENDER_BENCH_GIT_DIRTY value `{value}` (expected 0 or 1)"
                )),
            })
            .transpose()?;
        let instrumentation = match get("RENDER_BENCH_INSTRUMENTATION").as_deref().unwrap_or("normal") {
            "normal" => Instrumentation::Normal,
            "chrome_trace" => Instrumentation::ChromeTrace,
            "tracy" => Instrumentation::Tracy,
            value => return Err(format!("invalid RENDER_BENCH_INSTRUMENTATION value `{value}` (expected normal, chrome_trace, or tracy)")),
        };
        Ok(Self {
            output: get("RENDER_BENCH_OUTPUT")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("target/render-bench/render-bench.json")),
            repetition: parse_u32("RENDER_BENCH_REPETITION", get("RENDER_BENCH_REPETITION"))?
                .unwrap_or(1),
            instrumentation,
            git_revision: get("RENDER_BENCH_GIT_REVISION"),
            git_dirty,
            label: get("RENDER_BENCH_LABEL"),
            profile: get("RENDER_BENCH_PROFILE"),
            os: get("RENDER_BENCH_OS"),
            arch: get("RENDER_BENCH_ARCH"),
            cpu: get("RENDER_BENCH_CPU"),
            expected_repetitions: parse_u32(
                "RENDER_BENCH_EXPECTED_REPETITIONS",
                get("RENDER_BENCH_EXPECTED_REPETITIONS"),
            )?,
            client_worker_threads: parse_u32(
                "VOXEL_CLIENT_WORKER_THREADS",
                get("VOXEL_CLIENT_WORKER_THREADS"),
            )?,
            tokio_worker_threads: parse_u32("TOKIO_WORKER_THREADS", get("TOKIO_WORKER_THREADS"))?,
            requested_near_backend: get("VOXEL_NEAR_RENDER"),
            requested_far_backend: get("VOXEL_FAR_RENDER"),
            load_timeout: parse_u32(
                "RENDER_BENCH_LOAD_TIMEOUT_SECS",
                get("RENDER_BENCH_LOAD_TIMEOUT_SECS"),
            )?
            .map_or(LOAD_TIMEOUT, |seconds| Duration::from_secs(u64::from(seconds))),
        })
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
    backend_ready: bool,
    near_runtime: Option<NearRuntimeMode>,
    far_runtime: FarRuntimeBackend,
}

impl BenchmarkReadiness {
    fn gates_ready(&self, scale: &WorldScale) -> bool {
        // A ring whose bound the near window already reaches has no keys to
        // wait for (the 5x5 window absorbs the whole factor-2 ring).
        let has_full_lod_table = scale
            .lod_table
            .iter()
            .filter(|&&(bound, _)| bound > scale.near_radius)
            .all(|&(_, factor)| {
            self.ready_lod_counts
                .get(&factor)
                .is_some_and(|&count| count > 0)
        });
        self.near_regions >= scale.near_region_count()
            && self.remesh_queue_len == 0
            && self.lod_build_queue_len == 0
            && self.pending_lod_events == 0
            && self.in_flight_tasks == 0
            && self.far_residency_pending == 0
            && self.backend_ready
            && has_full_lod_table
    }
}

#[derive(Default)]
pub struct ReadinessTracker {
    /// The world the run subscribed to: sizes the near-region and LOD gates.
    scale: WorldScale,
    previous_lod_counts: Option<BTreeMap<u32, usize>>,
    previous_backends: Option<(Option<NearRuntimeMode>, FarRuntimeBackend)>,
    stable_frames: u32,
}

impl ReadinessTracker {
    pub fn observe(&mut self, readiness: &BenchmarkReadiness) -> bool {
        if !readiness.gates_ready(&self.scale) {
            self.previous_lod_counts = None;
            self.previous_backends = None;
            self.stable_frames = 0;
            return false;
        }
        let backends = (readiness.near_runtime, readiness.far_runtime);
        if self.previous_lod_counts.as_ref() == Some(&readiness.ready_lod_counts)
            && self.previous_backends == Some(backends)
        {
            self.stable_frames = self.stable_frames.saturating_add(1);
        } else {
            self.previous_lod_counts = Some(readiness.ready_lod_counts.clone());
            self.previous_backends = Some(backends);
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

const NON_AUTHORITATIVE_NEAR_REASON: &str =
    "near renderer mode changed during benchmark measurement";
const NON_AUTHORITATIVE_FAR_REASON: &str =
    "far renderer entered raster fallback during benchmark measurement";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NearRuntimeMode {
    mode: ActiveNearMode,
    generation: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum FarRuntimeBackend {
    Brick,
    Raster,
    Mixed,
    #[default]
    Unavailable,
}

impl FarRuntimeBackend {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Brick => "brick",
            Self::Raster => "raster",
            Self::Mixed => "mixed",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RuntimeBackendState {
    near: Option<NearRuntimeMode>,
    far: FarRuntimeBackend,
    /// True only when every required far cell uses the configured backend.
    far_ready: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthorityAction {
    Continue,
    RestartWarmup,
    Fail(&'static str),
}

#[derive(Default)]
struct RunBackendAuthority {
    locked: Option<RuntimeBackendState>,
}

impl RunBackendAuthority {
    fn lock(&mut self, state: RuntimeBackendState) {
        self.locked = Some(state);
    }

    const fn locked(&self) -> Option<RuntimeBackendState> {
        self.locked
    }

    fn observe(&mut self, phase: BenchmarkPhase, current: RuntimeBackendState) -> AuthorityAction {
        let Some(locked) = self.locked else {
            if current.near.is_some() && current.far_ready {
                self.lock(current);
            }
            return AuthorityAction::Continue;
        };
        let near_changed = current.near.is_none() || current.near != locked.near;
        let far_changed = !current.far_ready || current.far != locked.far;
        match phase {
            BenchmarkPhase::Warming if near_changed || far_changed => {
                if current.near.is_some() && current.far_ready {
                    self.lock(current);
                }
                AuthorityAction::RestartWarmup
            }
            BenchmarkPhase::Measuring if near_changed => {
                AuthorityAction::Fail(NON_AUTHORITATIVE_NEAR_REASON)
            }
            BenchmarkPhase::Measuring if far_changed => {
                AuthorityAction::Fail(NON_AUTHORITATIVE_FAR_REASON)
            }
            _ => AuthorityAction::Continue,
        }
    }
}

fn result_backend_state(
    failed: bool,
    authority: &RunBackendAuthority,
    current: RuntimeBackendState,
) -> RuntimeBackendState {
    if failed {
        current
    } else {
        authority.locked().unwrap_or(current)
    }
}

const fn classify_far_runtime(
    total_cells: usize,
    brick_cells: usize,
    raster_cells: usize,
) -> (FarRuntimeBackend, bool) {
    let backend = match (brick_cells > 0, raster_cells > 0) {
        (true, true) => FarRuntimeBackend::Mixed,
        (true, false) => FarRuntimeBackend::Brick,
        (false, true) => FarRuntimeBackend::Raster,
        (false, false) => FarRuntimeBackend::Unavailable,
    };
    (
        backend,
        total_cells > 0 && brick_cells.saturating_add(raster_cells) == total_cells,
    )
}

const fn far_runtime_ready(
    expected: FarRuntimeBackend,
    far_expected: bool,
    total_cells: usize,
    brick_cells: usize,
    raster_cells: usize,
    raster_ready_cells: usize,
) -> bool {
    if !far_expected {
        // No LOD rings subscribed (`VOXEL_LOD_RINGS=off` / the tiny world):
        // there is no far backend to wait for, and stray cells are not ready.
        return total_cells == 0;
    }
    if total_cells == 0 {
        return false;
    }
    match expected {
        FarRuntimeBackend::Brick => brick_cells == total_cells && raster_cells == 0,
        FarRuntimeBackend::Raster => brick_cells == 0 && raster_ready_cells == total_cells,
        FarRuntimeBackend::Mixed | FarRuntimeBackend::Unavailable => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompletionDecision {
    failed: bool,
    missing_required_samples: bool,
}

impl CompletionDecision {
    const fn exit_successfully(self) -> bool {
        !self.failed
    }
}

const fn completion_decision(
    phase: BenchmarkPhase,
    has_required_frame_samples: bool,
) -> CompletionDecision {
    let missing_required_samples =
        matches!(phase, BenchmarkPhase::Complete) && !has_required_frame_samples;
    CompletionDecision {
        failed: matches!(phase, BenchmarkPhase::Failed) || missing_required_samples,
        missing_required_samples,
    }
}

pub struct BenchmarkController {
    phase: BenchmarkPhase,
    phase_started: Duration,
    load_timeout: Duration,
}

impl Default for BenchmarkController {
    fn default() -> Self {
        Self {
            phase: BenchmarkPhase::default(),
            phase_started: Duration::ZERO,
            load_timeout: LOAD_TIMEOUT,
        }
    }
}

impl BenchmarkController {
    pub fn advance(&mut self, elapsed: Duration, ready: bool) {
        match self.phase {
            BenchmarkPhase::Loading if ready => {
                self.phase = BenchmarkPhase::Warming;
                self.phase_started = elapsed;
            }
            BenchmarkPhase::Loading if elapsed > self.load_timeout => {
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

    fn restart_warmup(&mut self, elapsed: Duration) {
        self.phase = BenchmarkPhase::Warming;
        self.phase_started = elapsed;
    }

    fn fail(&mut self) {
        self.phase = BenchmarkPhase::Failed;
    }

    fn phase_elapsed(&self, elapsed: Duration) -> Duration {
        elapsed.saturating_sub(self.phase_started)
    }
}

#[derive(Clone, Debug, Default)]
pub struct BenchmarkCapabilityMetrics {
    pub near_active_chunks: Option<u64>,
    pub near_frustum_survivors: Option<u64>,
    pub near_hzb_survivors: Option<u64>,
    pub near_resident_quads: Option<u64>,
    pub near_submitted_quads: Option<u64>,
    pub near_submitted_triangles: Option<u64>,
    pub near_submitted_vertices: Option<u64>,
    pub near_direct_draws: Option<u64>,
    pub near_indirect_draws: Option<u64>,
    pub near_logical_bytes: Option<u64>,
    pub near_reserved_bytes: Option<u64>,
    pub far_cells: Option<u64>,
    pub far_resident_quads: Option<u64>,
    pub far_visible_instances: Option<u64>,
    pub far_proxy_vertices: Option<u64>,
    pub far_direct_draws: Option<u64>,
    pub far_indirect_draws: Option<u64>,
    pub far_logical_bytes: Option<u64>,
    pub far_reserved_bytes: Option<u64>,
    pub unique_bricks: Option<u64>,
    pub brick_dedup_ratio: Option<f64>,
    pub unavailable: Vec<UnavailableMetric>,
}

#[derive(Resource, Default)]
pub struct BenchmarkCapabilityMetricsInput(pub BenchmarkCapabilityMetrics);

struct BenchmarkSamples {
    frame_ms: Vec<f64>,
    fps: Vec<f64>,
    diagnostics: RenderDiagnosticSamples,
    near_active_chunks: Vec<u64>,
    near_frustum_survivors: Vec<u64>,
    near_hzb_survivors: Vec<u64>,
    near_resident_quads: Vec<u64>,
    near_submitted_quads: Vec<u64>,
    near_submitted_triangles: Vec<u64>,
    near_submitted_vertices: Vec<u64>,
    near_direct_draws: Vec<u64>,
    near_indirect_draws: Vec<u64>,
    near_logical_bytes: Vec<u64>,
    near_reserved_bytes: Vec<u64>,
    far_cells: Vec<u64>,
    far_resident_quads: Vec<u64>,
    far_visible_instances: Vec<u64>,
    far_proxy_vertices: Vec<u64>,
    far_direct_draws: Vec<u64>,
    far_indirect_draws: Vec<u64>,
    far_logical_bytes: Vec<u64>,
    far_reserved_bytes: Vec<u64>,
    unique_bricks: Vec<u64>,
    brick_dedup_ratio: Vec<f64>,
    unavailable: Vec<UnavailableMetric>,
}

impl Default for BenchmarkSamples {
    fn default() -> Self {
        Self {
            frame_ms: Vec::new(),
            fps: Vec::new(),
            diagnostics: default(),
            near_active_chunks: Vec::new(),
            near_frustum_survivors: Vec::new(),
            near_hzb_survivors: Vec::new(),
            near_resident_quads: Vec::new(),
            near_submitted_quads: Vec::new(),
            near_submitted_triangles: Vec::new(),
            near_submitted_vertices: Vec::new(),
            near_direct_draws: Vec::new(),
            near_indirect_draws: Vec::new(),
            near_logical_bytes: Vec::new(),
            near_reserved_bytes: Vec::new(),
            far_cells: Vec::new(),
            far_resident_quads: Vec::new(),
            far_visible_instances: Vec::new(),
            far_proxy_vertices: Vec::new(),
            far_direct_draws: Vec::new(),
            far_indirect_draws: Vec::new(),
            far_logical_bytes: Vec::new(),
            far_reserved_bytes: Vec::new(),
            unique_bricks: Vec::new(),
            brick_dedup_ratio: Vec::new(),
            unavailable: Vec::new(),
        }
    }
}

impl BenchmarkSamples {
    fn push_capabilities(&mut self, metrics: &BenchmarkCapabilityMetrics) {
        push_optional(&mut self.near_active_chunks, metrics.near_active_chunks);
        push_optional(
            &mut self.near_frustum_survivors,
            metrics.near_frustum_survivors,
        );
        push_optional(&mut self.near_hzb_survivors, metrics.near_hzb_survivors);
        push_optional(&mut self.near_resident_quads, metrics.near_resident_quads);
        push_optional(&mut self.near_submitted_quads, metrics.near_submitted_quads);
        push_optional(
            &mut self.near_submitted_triangles,
            metrics.near_submitted_triangles,
        );
        push_optional(
            &mut self.near_submitted_vertices,
            metrics.near_submitted_vertices,
        );
        push_optional(&mut self.near_direct_draws, metrics.near_direct_draws);
        push_optional(&mut self.near_indirect_draws, metrics.near_indirect_draws);
        push_optional(&mut self.near_logical_bytes, metrics.near_logical_bytes);
        push_optional(&mut self.near_reserved_bytes, metrics.near_reserved_bytes);
        push_optional(&mut self.far_cells, metrics.far_cells);
        push_optional(&mut self.far_resident_quads, metrics.far_resident_quads);
        push_optional(
            &mut self.far_visible_instances,
            metrics.far_visible_instances,
        );
        push_optional(&mut self.far_proxy_vertices, metrics.far_proxy_vertices);
        push_optional(&mut self.far_direct_draws, metrics.far_direct_draws);
        push_optional(&mut self.far_indirect_draws, metrics.far_indirect_draws);
        push_optional(&mut self.far_logical_bytes, metrics.far_logical_bytes);
        push_optional(&mut self.far_reserved_bytes, metrics.far_reserved_bytes);
        push_optional(&mut self.unique_bricks, metrics.unique_bricks);
        push_optional(&mut self.brick_dedup_ratio, metrics.brick_dedup_ratio);
        self.unavailable.extend(metrics.unavailable.iter().cloned());
    }
}

fn push_optional<T>(values: &mut Vec<T>, value: Option<T>) {
    if let Some(value) = value {
        values.push(value);
    }
}

#[derive(Resource, Default)]
struct BenchmarkRuntime {
    controller: BenchmarkController,
    readiness: ReadinessTracker,
    samples: BenchmarkSamples,
    last_readiness: BenchmarkReadiness,
    camera_base: Option<(Entity, Quat)>,
    snapshot_frozen: bool,
    output_written: bool,
    load_to_ready_ms: Option<f64>,
    backend_authority: RunBackendAuthority,
    current_backends: RuntimeBackendState,
    failure_reason: Option<String>,
}

impl BenchmarkRuntime {
    fn new(scale: WorldScale, load_timeout: Duration) -> Self {
        Self {
            controller: BenchmarkController {
                load_timeout,
                ..Default::default()
            },
            readiness: ReadinessTracker {
                scale,
                ..Default::default()
            },
            ..Default::default()
        }
    }
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
        // The binary inserts the env-resolved scale before this plugin; a bare
        // app (tests) falls back to the production scale.
        let scale = *app.init_resource::<WorldScale>().world().resource::<WorldScale>();
        info!("render benchmark world scale: {scale}");
        app.insert_resource(BenchmarkSettingsResource(self.settings.clone()))
            .insert_resource(shared.clone())
            .insert_resource(far_draw_gate.clone())
            .init_resource::<BenchmarkCapabilityMetricsInput>()
            .insert_resource(BenchmarkRuntime::new(scale, self.settings.load_timeout))
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
    pending_snapshot_voxels: Query<'w, 's, (), With<super::bridge::PendingSnapshotVoxels>>,
    chunks: Query<'w, 's, Option<&'static TerrainQuadCount>, With<ChunkKey>>,
    far_cells: Query<
        'w,
        's,
        (
            &'static LodChunk,
            Option<&'static TerrainQuadCount>,
            Has<FarVoxelReady>,
            Has<FarRasterFallback>,
            Has<LodRasterReady>,
            Has<Mesh3d>,
            Has<ChunkQuads>,
        ),
    >,
    far_residency: Option<Res<'w, FarVoxelResidency>>,
}

fn expected_lod_keys(
    viewer: game::RegionCoords,
    near_regions: &BTreeSet<game::RegionCoords>,
    lod_table: &[(i32, u32)],
) -> BTreeSet<game::lod::LodKey> {
    crate::instance::desired_lod(
        viewer,
        near_regions,
        &BTreeMap::new(),
        lod_table,
        game::lod::LOD_STICKY_BAND,
    )
    .1
}

fn snapshot_has_all_lod_keys(
    viewer: Option<game::RegionCoords>,
    near_regions: &BTreeSet<game::RegionCoords>,
    lod_table: &[(i32, u32)],
    loaded: impl IntoIterator<Item = game::lod::LodKey>,
) -> bool {
    let Some(viewer) = viewer else {
        return false;
    };
    loaded.into_iter().collect::<BTreeSet<_>>()
        == expected_lod_keys(viewer, near_regions, lod_table)
}

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
    let render_snapshot = shared
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let near_runtime = render_snapshot.near.map(|near| NearRuntimeMode {
        mode: near.active_mode,
        generation: near.mode_generation,
    });
    let mut brick_cells = 0usize;
    let mut raster_cells = 0usize;
    let mut raster_ready_cells = 0usize;
    let mut raster_resident_quads = 0u64;
    let mut ready_lod_counts = BTreeMap::new();
    let mut total_far_cells = 0usize;
    for (lod, quads, brick_ready, fallback, raster_ready, has_mesh, has_quads) in &scene.far_cells {
        total_far_cells = total_far_cells.saturating_add(1);
        let raster_draw_ready = raster_ready || has_mesh || has_quads;
        let raster_active = fallback || raster_draw_ready;
        if brick_ready {
            brick_cells = brick_cells.saturating_add(1);
        }
        if raster_active {
            raster_cells = raster_cells.saturating_add(1);
            raster_resident_quads =
                raster_resident_quads.saturating_add(quads.map_or(0, |quads| u64::from(quads.0)));
        }
        if raster_draw_ready {
            raster_ready_cells = raster_ready_cells.saturating_add(1);
        }
        let cell_ready_for_config = match render_config.far {
            super::config::FarRenderBackend::BrickRaymarch => brick_ready && !raster_active,
            super::config::FarRenderBackend::Raster => raster_draw_ready && !brick_ready,
        };
        if cell_ready_for_config {
            *ready_lod_counts.entry(lod.factor).or_insert(0) += 1;
        }
    }
    let (far_runtime, _) = classify_far_runtime(total_far_cells, brick_cells, raster_cells);
    let expected_far_runtime = match render_config.far {
        super::config::FarRenderBackend::BrickRaymarch => FarRuntimeBackend::Brick,
        super::config::FarRenderBackend::Raster => FarRuntimeBackend::Raster,
    };
    let current_backends = RuntimeBackendState {
        near: near_runtime,
        far: far_runtime,
        far_ready: far_runtime_ready(
            expected_far_runtime,
            !runtime.readiness.scale.lod_table.is_empty(),
            total_far_cells,
            brick_cells,
            raster_cells,
            raster_ready_cells,
        ),
    };
    runtime.current_backends = current_backends;
    if !runtime.snapshot_frozen
        && scene.roots.0.len() >= runtime.readiness.scale.near_region_count()
        && scene.client_updates.0.is_empty()
        && scene.lod_rings.pending.is_empty()
    {
        let near_regions = scene.roots.0.keys().copied().collect::<BTreeSet<_>>();
        if snapshot_has_all_lod_keys(
            scene.hud_status.viewer_region,
            &near_regions,
            runtime.readiness.scale.lod_table,
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
    let readiness = BenchmarkReadiness {
        near_regions: scene.roots.0.len(),
        remesh_queue_len: scene.remesh_queue.dirty.len(),
        lod_build_queue_len: scene.lod_build_queue.pending_count(),
        pending_lod_events: scene.lod_rings.pending.len(),
        in_flight_tasks: scene.in_flight_near.iter().count()
            + scene.in_flight_lod.iter().count()
            + scene.in_flight_bricks.iter().count()
            + scene.in_flight_far_uploads.iter().count()
            // Region chunks whose snapshot voxels are still being derived
            // off-thread: not queued for meshing yet, but not loaded either.
            + scene.pending_snapshot_voxels.iter().count(),
        far_residency_pending: scene
            .far_residency
            .as_deref()
            .map_or(0, FarVoxelResidency::pending_work_count),
        ready_lod_counts,
        backend_ready: current_backends.near.is_some() && current_backends.far_ready,
        near_runtime: current_backends.near,
        far_runtime: current_backends.far,
    };
    let ready = runtime.readiness.observe(&readiness);
    runtime.last_readiness = readiness;

    let previous_phase = runtime.controller.phase();
    let authority_action = runtime
        .backend_authority
        .observe(previous_phase, current_backends);
    match authority_action {
        AuthorityAction::Continue => runtime.controller.advance(elapsed, ready),
        AuthorityAction::RestartWarmup => {
            runtime.controller.restart_warmup(elapsed);
            runtime.samples = BenchmarkSamples::default();
            runtime.camera_base = None;
            info!("render benchmark restarted warmup after renderer backend changed");
        }
        AuthorityAction::Fail(reason) => {
            runtime.failure_reason = Some(reason.to_owned());
            runtime.controller.fail();
            error!("render benchmark failed: {reason}");
        }
    }
    let phase = runtime.controller.phase();
    if phase != previous_phase {
        info!("render benchmark phase: {previous_phase:?} -> {phase:?}");
        if phase == BenchmarkPhase::Warming {
            runtime.backend_authority.lock(current_backends);
            runtime.load_to_ready_ms = Some(elapsed.as_secs_f64() * 1_000.0);
            far_draw_gate.enable();
            info!("render benchmark enabled the completed far scene for warmup");
        }
    }

    capabilities.0.unavailable.clear();
    if !render_config.near.uses_quad_records() {
        capabilities.0.near_active_chunks = Some(scene.chunks.iter().count() as u64);
        capabilities.0.near_resident_quads = Some(
            scene
                .chunks
                .iter()
                .flatten()
                .map(|quads| u64::from(quads.0))
                .sum(),
        );
        capabilities.0.near_submitted_quads = None;
        capabilities.0.near_submitted_triangles = None;
        capabilities.0.near_submitted_vertices = None;
        capabilities.0.near_direct_draws = None;
        capabilities.0.near_indirect_draws = Some(0);
        capabilities.0.unavailable.push(UnavailableMetric {
            path: "metrics.geometry.near.submitted_quads".to_owned(),
            reason: UnavailableReason::TelemetryReadbackDisabled,
            detail: Some(
                "legacy per-chunk submission is view-specific; use per-pass clipper_primitives_out"
                    .to_owned(),
            ),
        });
    }
    capabilities.0.far_cells = Some(total_far_cells as u64);
    capabilities.0.far_resident_quads = matches!(
        far_runtime,
        FarRuntimeBackend::Raster | FarRuntimeBackend::Mixed
    )
    .then_some(raster_resident_quads);
    if let Some(residency) = scene.far_residency.as_deref() {
        capabilities.0.unique_bricks = Some(residency.unique_bricks() as u64);
        capabilities.0.brick_dedup_ratio = residency.brick_dedup_ratio();
    }
    let telemetry_far_backend = if far_runtime == FarRuntimeBackend::Brick {
        super::config::FarRenderBackend::BrickRaymarch
    } else {
        super::config::FarRenderBackend::Raster
    };
    let translated = translate_snapshot(&render_snapshot, telemetry_far_backend);
    if render_config.near.uses_quad_records() {
        capabilities.0.near_active_chunks = translated.near.active_chunks;
        capabilities.0.near_frustum_survivors = translated.near.frustum_survivors;
        capabilities.0.near_hzb_survivors = translated.near.hzb_survivors;
        capabilities.0.near_resident_quads = translated.near.resident_quads;
        capabilities.0.near_submitted_quads = translated.near.submitted_quads;
        capabilities.0.near_submitted_triangles = translated.near.submitted_triangles;
        capabilities.0.near_submitted_vertices = translated.near.submitted_vertices;
        capabilities.0.near_direct_draws = translated.near.direct_draws;
        capabilities.0.near_indirect_draws = translated.near.indirect_draws;
    }
    capabilities.0.near_logical_bytes = translated.near.logical_bytes;
    capabilities.0.near_reserved_bytes = translated.near.reserved_bytes;
    capabilities.0.far_visible_instances = translated.far.visible_instances;
    capabilities.0.far_proxy_vertices = translated.far.proxy_vertices;
    match far_runtime {
        FarRuntimeBackend::Brick => {
            capabilities.0.far_direct_draws = Some(0);
            capabilities.0.far_indirect_draws = translated.far.indirect_draws;
        }
        FarRuntimeBackend::Raster => {
            capabilities.0.far_direct_draws = None;
            capabilities.0.far_indirect_draws = Some(0);
        }
        FarRuntimeBackend::Mixed | FarRuntimeBackend::Unavailable => {
            capabilities.0.far_direct_draws = None;
            capabilities.0.far_indirect_draws = None;
        }
    }
    capabilities.0.far_logical_bytes = translated.far.logical_bytes;
    capabilities.0.far_reserved_bytes = translated.far.reserved_bytes;
    capabilities.0.unavailable.extend(translated.unavailable);

    if phase == BenchmarkPhase::Measuring {
        if let Some(frame_ms) = diagnostics
            .get(&FrameTimeDiagnosticsPlugin::FRAME_TIME)
            .and_then(|diagnostic| diagnostic.value())
        {
            runtime.samples.frame_ms.push(frame_ms);
        }
        if let Some(fps) = diagnostics
            .get(&FrameTimeDiagnosticsPlugin::FPS)
            .and_then(|diagnostic| diagnostic.value())
        {
            runtime.samples.fps.push(fps);
        }
        for diagnostic in diagnostics.iter() {
            let path = diagnostic.path().to_string();
            if let Some(value) = diagnostic.value() {
                runtime.samples.diagnostics.observe(&path, value);
            }
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
        let completion = completion_decision(phase, !runtime.samples.frame_ms.is_empty());
        let result_backends = result_backend_state(
            completion.failed,
            &runtime.backend_authority,
            runtime.current_backends,
        );
        let result = build_result(
            completion.failed,
            completion.missing_required_samples,
            &settings.0,
            result_backends.near.map(|near| near.mode),
            translated.near_visibility_source,
            result_backends.far,
            &runtime,
            render_snapshot.adapter,
        );
        match write_result_atomic(&settings.0.output, &result) {
            Ok(()) => {
                info!("render benchmark JSON: {}", settings.0.output.display());
                exits.write(if completion.exit_successfully() {
                    AppExit::Success
                } else {
                    AppExit::error()
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

fn build_result(
    failed: bool,
    missing_required_samples: bool,
    settings: &BenchmarkRunSettings,
    near_mode: Option<ActiveNearMode>,
    near_visibility_source: Option<ForwardCullSource>,
    far_runtime: FarRuntimeBackend,
    runtime: &BenchmarkRuntime,
    adapter: Option<AdapterMetadata>,
) -> BenchmarkResult {
    let samples = &runtime.samples;
    let metrics = BenchmarkMetrics {
        frame_wall_ms: FloatSummary::from_samples(&samples.frame_ms),
        timing: TimingMetrics {
            frame_wall_ms: FloatSummary::from_samples(&samples.frame_ms),
            fps: FloatSummary::from_samples(&samples.fps),
            load_to_ready_ms: runtime.load_to_ready_ms,
            gpu_busy_estimate_ms: metrics::gpu_busy_estimate_ms(&samples.diagnostics.elapsed_gpu),
            elapsed_cpu: RenderDiagnosticSamples::float_summaries(&samples.diagnostics.elapsed_cpu),
            elapsed_gpu: RenderDiagnosticSamples::float_summaries(&samples.diagnostics.elapsed_gpu),
        },
        geometry: GeometryMetrics {
            near: GeometryScopeMetrics {
                resident_quads: CountSummary::from_samples(&samples.near_resident_quads),
                submitted_quads: CountSummary::from_samples(&samples.near_submitted_quads),
                submitted_triangles: CountSummary::from_samples(&samples.near_submitted_triangles),
                submitted_vertices: CountSummary::from_samples(&samples.near_submitted_vertices),
                proxy_vertices: None,
                unique_bricks: None,
                brick_dedup_ratio: None,
            },
            far: GeometryScopeMetrics {
                resident_quads: CountSummary::from_samples(&samples.far_resident_quads),
                submitted_quads: None,
                submitted_triangles: None,
                submitted_vertices: None,
                proxy_vertices: CountSummary::from_samples(&samples.far_proxy_vertices),
                unique_bricks: CountSummary::from_samples(&samples.unique_bricks),
                brick_dedup_ratio: FloatSummary::from_samples(&samples.brick_dedup_ratio),
            },
        },
        visibility: VisibilityMetrics {
            near: VisibilityScopeMetrics {
                active_chunks: CountSummary::from_samples(&samples.near_active_chunks),
                active_cells: None,
                frustum_survivors: CountSummary::from_samples(&samples.near_frustum_survivors),
                hzb_survivors: CountSummary::from_samples(&samples.near_hzb_survivors),
                visible_instances: None,
            },
            far: VisibilityScopeMetrics {
                active_chunks: None,
                active_cells: CountSummary::from_samples(&samples.far_cells),
                frustum_survivors: None,
                hzb_survivors: None,
                visible_instances: CountSummary::from_samples(&samples.far_visible_instances),
            },
        },
        draws: DrawMetrics {
            near: DrawScopeMetrics {
                direct_draws: CountSummary::from_samples(&samples.near_direct_draws),
                indirect_draws: CountSummary::from_samples(&samples.near_indirect_draws),
            },
            far: DrawScopeMetrics {
                direct_draws: CountSummary::from_samples(&samples.far_direct_draws),
                indirect_draws: CountSummary::from_samples(&samples.far_indirect_draws),
            },
        },
        memory: MemoryMetrics {
            near: MemoryScopeMetrics {
                logical_bytes: CountSummary::from_samples(&samples.near_logical_bytes),
                reserved_bytes: CountSummary::from_samples(&samples.near_reserved_bytes),
            },
            far: MemoryScopeMetrics {
                logical_bytes: CountSummary::from_samples(&samples.far_logical_bytes),
                reserved_bytes: CountSummary::from_samples(&samples.far_reserved_bytes),
            },
        },
        pipeline_statistics: PipelineStatistics {
            vertex_shader_invocations: RenderDiagnosticSamples::count_summaries(
                &samples.diagnostics.vertex_shader_invocations,
            ),
            clipper_invocations: RenderDiagnosticSamples::count_summaries(
                &samples.diagnostics.clipper_invocations,
            ),
            clipper_primitives_out: RenderDiagnosticSamples::count_summaries(
                &samples.diagnostics.clipper_primitives_out,
            ),
            fragment_shader_invocations: RenderDiagnosticSamples::count_summaries(
                &samples.diagnostics.fragment_shader_invocations,
            ),
            compute_shader_invocations: RenderDiagnosticSamples::count_summaries(
                &samples.diagnostics.compute_shader_invocations,
            ),
        },
    };
    let mut unavailable_metrics = samples.unavailable.clone();
    unavailable_metrics.extend(
        runtime
            .last_readiness
            .ready_lod_counts
            .is_empty()
            .then_some(UnavailableMetric {
                path: "readiness_at_exit.ready_lod_counts".to_owned(),
                reason: UnavailableReason::SampleNotObserved,
                detail: None,
            }),
    );
    unavailable_metrics.push(UnavailableMetric {
        path: "metrics.timing.gpu_busy_estimate_ms".to_owned(),
        reason: UnavailableReason::SampleNotObserved,
        detail: Some("diagnostic span containment is not exposed; summing nested spans would double-count GPU work".to_owned()),
    });
    populate_metric_unavailability(
        &metrics,
        near_mode,
        near_visibility_source,
        far_runtime,
        &mut unavailable_metrics,
    );
    unavailable_metrics.sort_by(|left, right| left.path.cmp(&right.path));
    unavailable_metrics.dedup_by(|left, right| left.path == right.path);
    BenchmarkResult::new(
        if failed { "failed" } else { "complete" },
        failed.then(|| {
            if let Some(reason) = &runtime.failure_reason {
                return reason.clone();
            }
            if missing_required_samples {
                return "measurement completed without frame-time samples".to_owned();
            }
            format!(
                "scene did not stabilize within {} seconds",
                runtime.controller.load_timeout.as_secs()
            )
        }),
        BenchmarkMetadata {
            repetition: settings.repetition,
            expected_repetitions: settings.expected_repetitions,
            instrumentation: settings.instrumentation,
            label: settings.label.clone(),
            profile: settings.profile.clone(),
            git_revision: settings.git_revision.clone(),
            git_dirty: settings.git_dirty,
            os: settings.os.clone(),
            arch: settings.arch.clone(),
            cpu: settings.cpu.clone(),
            client_threads: settings.client_worker_threads,
            server_threads: settings.tokio_worker_threads,
            resolution: [BENCHMARK_WIDTH, BENCHMARK_HEIGHT],
            vsync: false,
            required_near_regions: runtime.readiness.scale.near_region_count(),
            stable_ready_frames: STABLE_READY_FRAMES,
            load_timeout_seconds: runtime.controller.load_timeout.as_secs(),
            warmup_seconds: WARMUP_DURATION.as_secs(),
            measurement_seconds: MEASUREMENT_DURATION.as_secs(),
            camera_path: "yaw_360".to_owned(),
            simulation_snapshot_frozen: runtime.snapshot_frozen,
            world_seed: worldgen::WORLD_SEED,
            lod_ring_table: runtime.readiness.scale.lod_table.to_vec(),
            near_backend_requested: settings.requested_near_backend.clone(),
            near_backend_resolved: near_mode
                .map(ActiveNearMode::as_str)
                .unwrap_or("unavailable")
                .to_owned(),
            far_backend_requested: settings.requested_far_backend.clone(),
            far_backend_resolved: far_runtime.as_str().to_owned(),
            adapter,
        },
        SerializableReadiness {
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
    )
}

fn populate_metric_unavailability(
    metrics: &BenchmarkMetrics,
    near_mode: Option<ActiveNearMode>,
    near_visibility_source: Option<ForwardCullSource>,
    far_runtime: FarRuntimeBackend,
    unavailable: &mut Vec<UnavailableMetric>,
) {
    let mut mark = |path: &str, missing: bool, reason: UnavailableReason, detail: Option<&str>| {
        if missing && !unavailable.iter().any(|entry| entry.path == path) {
            unavailable.push(UnavailableMetric {
                path: path.to_owned(),
                reason,
                detail: detail.map(str::to_owned),
            });
        }
    };
    let sample = UnavailableReason::SampleNotObserved;
    let not_applicable = UnavailableReason::NotApplicableToBackend;
    let readback_disabled = UnavailableReason::TelemetryReadbackDisabled;

    mark(
        "metrics.timing.fps",
        metrics.timing.fps.is_none(),
        sample,
        None,
    );
    for (path, missing) in [
        (
            "metrics.memory.near.logical_bytes",
            metrics.memory.near.logical_bytes.is_none(),
        ),
        (
            "metrics.memory.near.reserved_bytes",
            metrics.memory.near.reserved_bytes.is_none(),
        ),
        (
            "metrics.memory.far.logical_bytes",
            metrics.memory.far.logical_bytes.is_none(),
        ),
        (
            "metrics.memory.far.reserved_bytes",
            metrics.memory.far.reserved_bytes.is_none(),
        ),
        (
            "metrics.geometry.near.resident_quads",
            metrics.geometry.near.resident_quads.is_none(),
        ),
        (
            "metrics.visibility.near.active_chunks",
            metrics.visibility.near.active_chunks.is_none(),
        ),
        (
            "metrics.visibility.far.active_cells",
            metrics.visibility.far.active_cells.is_none(),
        ),
        (
            "metrics.draws.near.indirect_draws",
            metrics.draws.near.indirect_draws.is_none(),
        ),
        (
            "metrics.draws.far.indirect_draws",
            metrics.draws.far.indirect_draws.is_none(),
        ),
    ] {
        mark(path, missing, sample, None);
    }

    for (path, missing) in [
        (
            "metrics.geometry.near.proxy_vertices",
            metrics.geometry.near.proxy_vertices.is_none(),
        ),
        (
            "metrics.geometry.near.unique_bricks",
            metrics.geometry.near.unique_bricks.is_none(),
        ),
        (
            "metrics.geometry.near.brick_dedup_ratio",
            metrics.geometry.near.brick_dedup_ratio.is_none(),
        ),
        (
            "metrics.visibility.near.active_cells",
            metrics.visibility.near.active_cells.is_none(),
        ),
        (
            "metrics.visibility.near.visible_instances",
            metrics.visibility.near.visible_instances.is_none(),
        ),
        (
            "metrics.visibility.far.active_chunks",
            metrics.visibility.far.active_chunks.is_none(),
        ),
        (
            "metrics.visibility.far.frustum_survivors",
            metrics.visibility.far.frustum_survivors.is_none(),
        ),
        (
            "metrics.visibility.far.hzb_survivors",
            metrics.visibility.far.hzb_survivors.is_none(),
        ),
    ] {
        mark(path, missing, not_applicable, None);
    }

    let near_submission_reason = match near_mode {
        Some(ActiveNearMode::Disabled) => readback_disabled,
        Some(ActiveNearMode::GroupedDirect | ActiveNearMode::GpuDriven { .. }) | None => sample,
    };
    for (path, missing) in [
        (
            "metrics.geometry.near.submitted_quads",
            metrics.geometry.near.submitted_quads.is_none(),
        ),
        (
            "metrics.geometry.near.submitted_triangles",
            metrics.geometry.near.submitted_triangles.is_none(),
        ),
        (
            "metrics.geometry.near.submitted_vertices",
            metrics.geometry.near.submitted_vertices.is_none(),
        ),
    ] {
        mark(
            path,
            missing,
            near_submission_reason,
            Some("exact main-view submission sample was not available"),
        );
    }
    mark(
        "metrics.visibility.near.frustum_survivors",
        metrics.visibility.near.frustum_survivors.is_none(),
        match near_mode {
            Some(ActiveNearMode::Disabled) => readback_disabled,
            Some(ActiveNearMode::GroupedDirect | ActiveNearMode::GpuDriven { .. }) | None => sample,
        },
        None,
    );
    mark(
        "metrics.visibility.near.hzb_survivors",
        metrics.visibility.near.hzb_survivors.is_none(),
        match (near_mode, near_visibility_source) {
            (Some(ActiveNearMode::GpuDriven { .. }), Some(ForwardCullSource::Frustum)) => {
                not_applicable
            }
            (Some(ActiveNearMode::GpuDriven { .. }), Some(ForwardCullSource::Hzb) | None) => sample,
            (Some(ActiveNearMode::Disabled | ActiveNearMode::GroupedDirect), _) => not_applicable,
            (None, _) => sample,
        },
        None,
    );
    mark(
        "metrics.draws.near.direct_draws",
        metrics.draws.near.direct_draws.is_none(),
        match near_mode {
            Some(ActiveNearMode::Disabled) => readback_disabled,
            Some(ActiveNearMode::GroupedDirect | ActiveNearMode::GpuDriven { .. }) | None => sample,
        },
        Some("exact main-view direct draw count was not observed"),
    );

    let far_is_brick = far_runtime == FarRuntimeBackend::Brick;
    mark(
        "metrics.geometry.far.resident_quads",
        metrics.geometry.far.resident_quads.is_none(),
        if far_is_brick { not_applicable } else { sample },
        None,
    );
    for (path, missing) in [
        (
            "metrics.geometry.far.submitted_quads",
            metrics.geometry.far.submitted_quads.is_none(),
        ),
        (
            "metrics.geometry.far.submitted_triangles",
            metrics.geometry.far.submitted_triangles.is_none(),
        ),
        (
            "metrics.geometry.far.submitted_vertices",
            metrics.geometry.far.submitted_vertices.is_none(),
        ),
    ] {
        mark(
            path,
            missing,
            if far_is_brick {
                not_applicable
            } else {
                readback_disabled
            },
            None,
        );
    }
    mark(
        "metrics.geometry.far.proxy_vertices",
        metrics.geometry.far.proxy_vertices.is_none(),
        if far_is_brick { sample } else { not_applicable },
        None,
    );
    for (path, missing) in [
        (
            "metrics.geometry.far.unique_bricks",
            metrics.geometry.far.unique_bricks.is_none(),
        ),
        (
            "metrics.geometry.far.brick_dedup_ratio",
            metrics.geometry.far.brick_dedup_ratio.is_none(),
        ),
    ] {
        mark(
            path,
            missing,
            if far_is_brick { sample } else { not_applicable },
            None,
        );
    }
    mark(
        "metrics.visibility.far.visible_instances",
        metrics.visibility.far.visible_instances.is_none(),
        if far_is_brick { sample } else { not_applicable },
        None,
    );
    mark(
        "metrics.draws.far.direct_draws",
        metrics.draws.far.direct_draws.is_none(),
        if far_is_brick {
            sample
        } else {
            readback_disabled
        },
        Some("exact main-view direct draw count was not observed"),
    );

    for (path, missing) in [
        (
            "metrics.pipeline_statistics.vertex_shader_invocations",
            metrics
                .pipeline_statistics
                .vertex_shader_invocations
                .is_empty(),
        ),
        (
            "metrics.pipeline_statistics.clipper_invocations",
            metrics.pipeline_statistics.clipper_invocations.is_empty(),
        ),
        (
            "metrics.pipeline_statistics.clipper_primitives_out",
            metrics
                .pipeline_statistics
                .clipper_primitives_out
                .is_empty(),
        ),
        (
            "metrics.pipeline_statistics.fragment_shader_invocations",
            metrics
                .pipeline_statistics
                .fragment_shader_invocations
                .is_empty(),
        ),
        (
            "metrics.pipeline_statistics.compute_shader_invocations",
            metrics
                .pipeline_statistics
                .compute_shader_invocations
                .is_empty(),
        ),
    ] {
        mark(
            path,
            missing,
            sample,
            Some("no diagnostic sample was observed; adapter feature absence was not proven"),
        );
    }
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
            near_regions: WorldScale::FULL.near_region_count(),
            remesh_queue_len: 0,
            lod_build_queue_len: 0,
            pending_lod_events: 0,
            in_flight_tasks: 0,
            far_residency_pending: 0,
            ready_lod_counts,
            backend_ready: true,
            near_runtime: Some(NearRuntimeMode {
                mode: ActiveNearMode::GroupedDirect,
                generation: 1,
            }),
            far_runtime: FarRuntimeBackend::Raster,
        }
    }

    #[test]
    fn production_snapshot_requires_every_expected_lod_key() {
        let viewer = game::RegionCoords::new(0, 0);
        let near_regions = viewer
            .window_radius(WorldScale::FULL.near_radius)
            .into_iter()
            .collect::<BTreeSet<_>>();
        let table = game::lod::LOD_RING_TABLE;
        let expected = expected_lod_keys(viewer, &near_regions, table);

        assert!(!expected.is_empty());
        assert!(snapshot_has_all_lod_keys(
            Some(viewer),
            &near_regions,
            table,
            expected.iter().copied(),
        ));

        let mut incomplete = expected;
        incomplete.pop_first();
        assert!(!snapshot_has_all_lod_keys(
            Some(viewer),
            &near_regions,
            table,
            incomplete.iter().copied(),
        ));
        assert!(!snapshot_has_all_lod_keys(
            None,
            &near_regions,
            table,
            incomplete.iter().copied(),
        ));
    }

    /// The tiny world (one region, no LOD rings) must be able to pass every
    /// gate: no far cells is "far ready", and an empty ring table expects no
    /// keys and no per-factor ready counts.
    #[test]
    fn tiny_world_scale_passes_the_gates_without_any_lod() {
        let viewer = game::RegionCoords::new(0, 0);
        let near_regions = BTreeSet::from([viewer]);
        assert!(snapshot_has_all_lod_keys(
            Some(viewer),
            &near_regions,
            WorldScale::TINY.lod_table,
            std::iter::empty(),
        ));
        assert!(far_runtime_ready(FarRuntimeBackend::Raster, false, 0, 0, 0, 0));
        assert!(!far_runtime_ready(FarRuntimeBackend::Raster, false, 1, 0, 1, 1));
        assert!(!far_runtime_ready(FarRuntimeBackend::Raster, true, 0, 0, 0, 0));

        let one_region = BenchmarkReadiness {
            near_regions: 1,
            ready_lod_counts: BTreeMap::new(),
            far_runtime: FarRuntimeBackend::Unavailable,
            ..ready(&[])
        };
        assert!(one_region.gates_ready(&WorldScale::TINY));
        assert!(!one_region.gates_ready(&WorldScale::SMALL), "small needs a factor-2 cell");
        assert!(!one_region.gates_ready(&WorldScale::FULL), "full needs 25 regions");

        let mut tracker = BenchmarkRuntime::new(WorldScale::TINY, LOAD_TIMEOUT).readiness;
        for _ in 0..(STABLE_READY_FRAMES - 1) {
            assert!(!tracker.observe(&one_region));
        }
        assert!(tracker.observe(&one_region));
    }

    #[test]
    fn load_timeout_is_configurable() {
        let mut controller = BenchmarkRuntime::new(WorldScale::FULL, Duration::from_secs(600)).controller;
        controller.advance(LOAD_TIMEOUT + Duration::from_millis(1), false);
        assert_eq!(controller.phase(), BenchmarkPhase::Loading);
        controller.advance(Duration::from_secs(601), false);
        assert_eq!(controller.phase(), BenchmarkPhase::Failed);

        let settings = BenchmarkRunSettings::from_values(|name| {
            (name == "RENDER_BENCH_LOAD_TIMEOUT_SECS").then(|| "300".to_owned())
        })
        .unwrap();
        assert_eq!(settings.load_timeout, Duration::from_secs(300));
        let default = BenchmarkRunSettings::from_values(|_| None).unwrap();
        assert_eq!(default.load_timeout, LOAD_TIMEOUT);
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
    fn readiness_stability_resets_when_near_runtime_generation_changes() {
        let mut tracker = ReadinessTracker::default();
        let initial = ready(&[]);
        for _ in 0..(STABLE_READY_FRAMES - 1) {
            assert!(!tracker.observe(&initial));
        }
        let changed = BenchmarkReadiness {
            near_runtime: Some(NearRuntimeMode {
                mode: ActiveNearMode::GpuDriven {
                    indirect_count: true,
                },
                generation: 2,
            }),
            ..initial
        };

        assert!(!tracker.observe(&changed));
        assert_eq!(tracker.stable_frames(), 1);
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
    fn backend_authority_restarts_warmup_when_near_generation_changes() {
        let mut authority = RunBackendAuthority::default();
        let first = RuntimeBackendState {
            near: Some(NearRuntimeMode {
                mode: ActiveNearMode::GroupedDirect,
                generation: 4,
            }),
            far: FarRuntimeBackend::Raster,
            far_ready: true,
        };
        authority.lock(first);

        let changed = RuntimeBackendState {
            near: Some(NearRuntimeMode {
                mode: ActiveNearMode::GpuDriven {
                    indirect_count: true,
                },
                generation: 5,
            }),
            ..first
        };

        assert_eq!(
            authority.observe(BenchmarkPhase::Warming, changed),
            AuthorityAction::RestartWarmup
        );
        assert_eq!(authority.locked(), Some(changed));
    }

    #[test]
    fn backend_authority_fails_measurement_when_near_mode_changes() {
        let mut authority = RunBackendAuthority::default();
        authority.lock(RuntimeBackendState {
            near: Some(NearRuntimeMode {
                mode: ActiveNearMode::GroupedDirect,
                generation: 7,
            }),
            far: FarRuntimeBackend::Raster,
            far_ready: true,
        });

        assert_eq!(
            authority.observe(
                BenchmarkPhase::Measuring,
                RuntimeBackendState {
                    near: Some(NearRuntimeMode {
                        mode: ActiveNearMode::GpuDriven {
                            indirect_count: true,
                        },
                        generation: 8,
                    }),
                    far: FarRuntimeBackend::Raster,
                    far_ready: true,
                }
            ),
            AuthorityAction::Fail(NON_AUTHORITATIVE_NEAR_REASON)
        );
    }

    #[test]
    fn far_runtime_state_distinguishes_pure_brick_raster_and_mixed() {
        assert_eq!(
            classify_far_runtime(4, 4, 0),
            (FarRuntimeBackend::Brick, true)
        );
        assert_eq!(
            classify_far_runtime(4, 0, 4),
            (FarRuntimeBackend::Raster, true)
        );
        assert_eq!(
            classify_far_runtime(4, 3, 1),
            (FarRuntimeBackend::Mixed, true)
        );
        assert_eq!(
            classify_far_runtime(4, 3, 0),
            (FarRuntimeBackend::Brick, false)
        );
        assert!(!far_runtime_ready(FarRuntimeBackend::Raster, true, 4, 0, 4, 0));
        assert!(far_runtime_ready(FarRuntimeBackend::Raster, true, 4, 0, 4, 4));
        assert!(far_runtime_ready(FarRuntimeBackend::Brick, true, 4, 4, 0, 0));
    }

    #[test]
    fn backend_authority_restarts_on_brick_fallback_during_warmup_and_fails_during_measurement() {
        let pure_brick = RuntimeBackendState {
            near: Some(NearRuntimeMode {
                mode: ActiveNearMode::GroupedDirect,
                generation: 1,
            }),
            far: FarRuntimeBackend::Brick,
            far_ready: true,
        };
        let mixed = RuntimeBackendState {
            far: FarRuntimeBackend::Mixed,
            far_ready: false,
            ..pure_brick
        };

        let mut warming = RunBackendAuthority::default();
        warming.lock(pure_brick);
        assert_eq!(
            warming.observe(BenchmarkPhase::Warming, mixed),
            AuthorityAction::RestartWarmup
        );

        let mut measuring = RunBackendAuthority::default();
        measuring.lock(pure_brick);
        assert_eq!(
            measuring.observe(BenchmarkPhase::Measuring, mixed),
            AuthorityAction::Fail(NON_AUTHORITATIVE_FAR_REASON)
        );
    }

    #[test]
    fn provenance_parser_accepts_valid_values_and_keeps_optional_values_absent() {
        let values = BTreeMap::from([
            ("RENDER_BENCH_INSTRUMENTATION", "chrome_trace"),
            ("RENDER_BENCH_GIT_DIRTY", "1"),
            ("RENDER_BENCH_EXPECTED_REPETITIONS", "5"),
        ]);
        let settings = BenchmarkRunSettings::from_values(|name| {
            values.get(name).map(|value| (*value).to_owned())
        })
        .unwrap();
        assert_eq!(settings.instrumentation, Instrumentation::ChromeTrace);
        assert_eq!(settings.git_dirty, Some(true));
        assert_eq!(settings.expected_repetitions, Some(5));
        assert_eq!(settings.git_revision, None);
        assert_eq!(settings.label, None);
    }

    #[test]
    fn provenance_parser_rejects_invalid_boolean_and_instrumentation() {
        assert!(BenchmarkRunSettings::from_values(
            |name| (name == "RENDER_BENCH_GIT_DIRTY").then(|| "yes".to_owned())
        )
        .unwrap_err()
        .contains("expected 0 or 1"));
        assert!(BenchmarkRunSettings::from_values(
            |name| (name == "RENDER_BENCH_INSTRUMENTATION").then(|| "trace".to_owned())
        )
        .unwrap_err()
        .contains("expected normal, chrome_trace, or tracy"));
    }

    #[test]
    fn frame_time_and_fps_summaries_preserve_independent_samples() {
        let frame = FloatSummary::from_samples(&[10.0, 20.0]).unwrap();
        let fps = FloatSummary::from_samples(&[100.0, 50.0]).unwrap();
        assert_eq!((frame.min, frame.max, frame.sample_count), (10.0, 20.0, 2));
        assert_eq!((fps.min, fps.max, fps.sample_count), (50.0, 100.0, 2));
    }

    #[test]
    fn null_workload_metrics_are_explained_once_without_guessing_adapter_features() {
        let metrics = BenchmarkMetrics::default();
        let mut unavailable = Vec::new();
        populate_metric_unavailability(
            &metrics,
            Some(ActiveNearMode::Disabled),
            None,
            FarRuntimeBackend::Brick,
            &mut unavailable,
        );

        let paths = unavailable
            .iter()
            .map(|metric| metric.path.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            paths.len(),
            unavailable.len(),
            "unavailable paths are unique"
        );
        for required in [
            "metrics.geometry.near.submitted_quads",
            "metrics.geometry.near.submitted_triangles",
            "metrics.geometry.near.submitted_vertices",
            "metrics.geometry.far.submitted_quads",
            "metrics.geometry.far.submitted_triangles",
            "metrics.geometry.far.submitted_vertices",
            "metrics.visibility.far.visible_instances",
            "metrics.draws.near.direct_draws",
            "metrics.pipeline_statistics.vertex_shader_invocations",
        ] {
            assert!(
                paths.contains(required),
                "missing unavailable entry for {required}"
            );
        }
        assert_eq!(
            unavailable
                .iter()
                .find(|metric| {
                    metric.path == "metrics.pipeline_statistics.vertex_shader_invocations"
                })
                .unwrap()
                .reason,
            UnavailableReason::SampleNotObserved
        );
    }

    #[test]
    fn missing_frame_samples_produce_a_failed_result() {
        let settings = BenchmarkRunSettings::from_values(|name| {
            (name == "RENDER_BENCH_OUTPUT").then(|| "unused.json".to_owned())
        })
        .unwrap();
        let result = build_result(
            true,
            true,
            &settings,
            Some(ActiveNearMode::Disabled),
            None,
            FarRuntimeBackend::Raster,
            &BenchmarkRuntime::default(),
            None,
        );

        assert_eq!(result.status, "failed");
        assert_eq!(
            result.failure_reason.as_deref(),
            Some("measurement completed without frame-time samples")
        );
    }

    #[test]
    fn missing_required_samples_fail_both_the_result_and_exit_decision() {
        let decision = completion_decision(BenchmarkPhase::Complete, false);

        assert!(decision.failed);
        assert!(decision.missing_required_samples);
        assert!(!decision.exit_successfully());
    }

    #[test]
    fn artifact_uses_render_world_backend_when_requested_gpu_falls_back_to_direct() {
        let settings = BenchmarkRunSettings::from_values(|name| match name {
            "RENDER_BENCH_OUTPUT" => Some("unused.json".to_owned()),
            "VOXEL_NEAR_RENDER" => Some("gpu".to_owned()),
            _ => None,
        })
        .unwrap();
        let result = build_result(
            false,
            false,
            &settings,
            Some(ActiveNearMode::GroupedDirect),
            Some(ForwardCullSource::Frustum),
            FarRuntimeBackend::Raster,
            &BenchmarkRuntime::default(),
            None,
        );

        assert_eq!(
            result.benchmark.near_backend_requested.as_deref(),
            Some("gpu")
        );
        assert_eq!(result.benchmark.near_backend_resolved, "direct");
    }

    #[test]
    fn failed_artifact_reports_actual_mixed_far_fallback_and_specific_reason() {
        let settings = BenchmarkRunSettings::from_values(|name| match name {
            "RENDER_BENCH_OUTPUT" => Some("unused.json".to_owned()),
            "VOXEL_FAR_RENDER" => Some("brick".to_owned()),
            _ => None,
        })
        .unwrap();
        let runtime = BenchmarkRuntime {
            failure_reason: Some(NON_AUTHORITATIVE_FAR_REASON.to_owned()),
            ..Default::default()
        };

        let result = build_result(
            true,
            false,
            &settings,
            Some(ActiveNearMode::GpuDriven {
                indirect_count: true,
            }),
            Some(ForwardCullSource::Hzb),
            FarRuntimeBackend::Mixed,
            &runtime,
            None,
        );

        assert_eq!(result.status, "failed");
        assert_eq!(
            result.failure_reason.as_deref(),
            Some(NON_AUTHORITATIVE_FAR_REASON)
        );
        assert_eq!(result.benchmark.far_backend_resolved, "mixed");
        assert_eq!(
            result.benchmark.far_backend_requested.as_deref(),
            Some("brick")
        );
    }

    #[test]
    fn complete_result_uses_locked_backends_while_failed_result_uses_actual_backends() {
        let locked = RuntimeBackendState {
            near: Some(NearRuntimeMode {
                mode: ActiveNearMode::GpuDriven {
                    indirect_count: true,
                },
                generation: 9,
            }),
            far: FarRuntimeBackend::Brick,
            far_ready: true,
        };
        let actual_fallback = RuntimeBackendState {
            far: FarRuntimeBackend::Mixed,
            far_ready: false,
            ..locked
        };
        let mut authority = RunBackendAuthority::default();
        authority.lock(locked);

        assert_eq!(
            result_backend_state(false, &authority, actual_fallback),
            locked
        );
        assert_eq!(
            result_backend_state(true, &authority, actual_fallback),
            actual_fallback
        );
    }

    #[test]
    fn hzb_unavailability_uses_actual_visibility_source() {
        let metrics = BenchmarkMetrics::default();
        let mut unavailable = Vec::new();

        populate_metric_unavailability(
            &metrics,
            Some(ActiveNearMode::GpuDriven {
                indirect_count: true,
            }),
            Some(ForwardCullSource::Frustum),
            FarRuntimeBackend::Raster,
            &mut unavailable,
        );

        assert_eq!(
            unavailable
                .iter()
                .find(|metric| metric.path == "metrics.visibility.near.hzb_survivors")
                .unwrap()
                .reason,
            UnavailableReason::NotApplicableToBackend
        );

        let mut hzb_unavailable = Vec::new();
        populate_metric_unavailability(
            &metrics,
            Some(ActiveNearMode::GpuDriven {
                indirect_count: true,
            }),
            Some(ForwardCullSource::Hzb),
            FarRuntimeBackend::Raster,
            &mut hzb_unavailable,
        );
        assert_eq!(
            hzb_unavailable
                .iter()
                .find(|metric| metric.path == "metrics.visibility.near.hzb_survivors")
                .unwrap()
                .reason,
            UnavailableReason::SampleNotObserved
        );
    }
}
