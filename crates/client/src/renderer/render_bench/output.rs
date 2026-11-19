use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use super::metrics::{CountSummary, FloatSummary};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Instrumentation {
    Normal,
    ChromeTrace,
    Tracy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    AdapterFeatureUnavailable,
    NotApplicableToBackend,
    TelemetryReadbackDisabled,
    SampleNotObserved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UnavailableMetric {
    pub path: String,
    pub reason: UnavailableReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct TimingMetrics {
    pub frame_wall_ms: Option<FloatSummary>,
    pub fps: Option<FloatSummary>,
    pub load_to_ready_ms: Option<f64>,
    pub gpu_busy_estimate_ms: Option<FloatSummary>,
    pub elapsed_cpu: BTreeMap<String, FloatSummary>,
    pub elapsed_gpu: BTreeMap<String, FloatSummary>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct GeometryScopeMetrics {
    pub resident_quads: Option<CountSummary>,
    pub submitted_quads: Option<CountSummary>,
    pub submitted_triangles: Option<CountSummary>,
    pub submitted_vertices: Option<CountSummary>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct GeometryMetrics {
    pub near: GeometryScopeMetrics,
    pub far: GeometryScopeMetrics,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct VisibilityScopeMetrics {
    pub active_chunks: Option<CountSummary>,
    pub active_cells: Option<CountSummary>,
    pub frustum_survivors: Option<CountSummary>,
    pub hzb_survivors: Option<CountSummary>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct VisibilityMetrics {
    pub near: VisibilityScopeMetrics,
    pub far: VisibilityScopeMetrics,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DrawScopeMetrics {
    pub direct_draws: Option<CountSummary>,
    pub indirect_draws: Option<CountSummary>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DrawMetrics {
    pub near: DrawScopeMetrics,
    pub far: DrawScopeMetrics,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct MemoryScopeMetrics {
    pub logical_bytes: Option<CountSummary>,
    pub reserved_bytes: Option<CountSummary>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct MemoryMetrics {
    pub near: MemoryScopeMetrics,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PipelineStatistics {
    pub vertex_shader_invocations: BTreeMap<String, CountSummary>,
    pub clipper_invocations: BTreeMap<String, CountSummary>,
    pub clipper_primitives_out: BTreeMap<String, CountSummary>,
    pub fragment_shader_invocations: BTreeMap<String, CountSummary>,
    pub compute_shader_invocations: BTreeMap<String, CountSummary>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct BenchmarkMetrics {
    /// Compatibility summary for consumers that select the required frame
    /// metric directly; the complete timing family also lives under `timing`.
    pub frame_wall_ms: Option<FloatSummary>,
    pub timing: TimingMetrics,
    pub geometry: GeometryMetrics,
    pub visibility: VisibilityMetrics,
    pub draws: DrawMetrics,
    pub memory: MemoryMetrics,
    pub pipeline_statistics: PipelineStatistics,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AdapterMetadata {
    pub name: String,
    pub backend: String,
    pub device_type: String,
    pub driver: String,
    pub driver_info: String,
    pub vendor: u32,
    pub device: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct BenchmarkMetadata {
    pub repetition: u32,
    pub expected_repetitions: Option<u32>,
    pub instrumentation: Instrumentation,
    pub label: Option<String>,
    pub profile: Option<String>,
    pub git_revision: Option<String>,
    pub git_dirty: Option<bool>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub cpu: Option<String>,
    pub client_threads: Option<u32>,
    pub server_threads: Option<u32>,
    pub resolution: [u32; 2],
    pub vsync: bool,
    pub required_near_regions: usize,
    pub stable_ready_frames: u32,
    pub load_timeout_seconds: u64,
    pub warmup_seconds: u64,
    pub measurement_seconds: u64,
    pub camera_path: String,
    pub simulation_snapshot_frozen: bool,
    pub world_seed: u64,
    pub lod_ring_table: Vec<(i32, u32)>,
    pub near_backend_requested: Option<String>,
    pub near_backend_resolved: String,
    pub far_backend_resolved: String,
    pub adapter: Option<AdapterMetadata>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SerializableReadiness {
    pub near_regions: usize,
    pub remesh_queue_len: usize,
    pub lod_build_queue_len: usize,
    pub pending_lod_events: usize,
    pub in_flight_tasks: usize,
    pub ready_lod_counts: BTreeMap<u32, usize>,
    pub stable_frames: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct BenchmarkResult {
    pub schema_version: u32,
    pub status: &'static str,
    pub failure_reason: Option<String>,
    pub benchmark: BenchmarkMetadata,
    pub readiness_at_exit: SerializableReadiness,
    pub metrics: BenchmarkMetrics,
    pub unavailable_metrics: Vec<UnavailableMetric>,
}

impl BenchmarkResult {
    pub fn new(
        status: &'static str,
        failure_reason: Option<String>,
        benchmark: BenchmarkMetadata,
        readiness_at_exit: SerializableReadiness,
        metrics: BenchmarkMetrics,
        unavailable_metrics: Vec<UnavailableMetric>,
    ) -> Self {
        Self {
            schema_version: 3,
            status,
            failure_reason,
            benchmark,
            readiness_at_exit,
            metrics,
            unavailable_metrics,
        }
    }
}

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn write_result_atomic(path: &Path, result: &BenchmarkResult) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let extension = path.extension().and_then(OsStr::to_str).unwrap_or("json");
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!(
        "{extension}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let write_result = (|| {
        let mut file = File::create(&tmp)?;
        serde_json::to_writer_pretty(&mut file, result).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write_result
}

