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
    pub proxy_vertices: Option<CountSummary>,
    pub unique_bricks: Option<CountSummary>,
    pub brick_dedup_ratio: Option<FloatSummary>,
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
    pub visible_instances: Option<CountSummary>,
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
    pub far: MemoryScopeMetrics,
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
    pub far_backend_requested: Option<String>,
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
    pub far_residency_pending: usize,
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
            schema_version: 2,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_result() -> BenchmarkResult {
        let mut metrics = BenchmarkMetrics::default();
        metrics.timing.frame_wall_ms = FloatSummary::from_samples(&[1.0, 2.0, 3.0]);
        metrics.frame_wall_ms = metrics.timing.frame_wall_ms.clone();
        BenchmarkResult::new(
            "complete",
            None,
            BenchmarkMetadata {
                repetition: 1,
                expected_repetitions: None,
                instrumentation: Instrumentation::Normal,
                label: None,
                profile: None,
                git_revision: None,
                git_dirty: None,
                os: None,
                arch: None,
                cpu: None,
                client_threads: None,
                server_threads: None,
                resolution: [1920, 1080],
                vsync: false,
                required_near_regions: 9,
                stable_ready_frames: 120,
                load_timeout_seconds: 120,
                warmup_seconds: 10,
                measurement_seconds: 30,
                camera_path: "yaw_360".to_owned(),
                simulation_snapshot_frozen: true,
                world_seed: 0,
                lod_ring_table: vec![(2, 2)],
                near_backend_requested: None,
                near_backend_resolved: "gpu".to_owned(),
                far_backend_requested: None,
                far_backend_resolved: "raster".to_owned(),
                adapter: None,
            },
            SerializableReadiness::default(),
            metrics,
            Vec::new(),
        )
    }

    #[test]
    fn unavailable_reasons_have_stable_machine_strings() {
        assert_eq!(
            serde_json::to_value(UnavailableReason::AdapterFeatureUnavailable).unwrap(),
            "adapter_feature_unavailable"
        );
        assert_eq!(
            serde_json::to_value(UnavailableReason::NotApplicableToBackend).unwrap(),
            "not_applicable_to_backend"
        );
    }

    #[test]
    fn schema_v2_serializes_instrumentation_and_sample_counts() {
        let value = serde_json::to_value(fixture_result()).unwrap();
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["benchmark"]["instrumentation"], "normal");
        assert_eq!(value["metrics"]["frame_wall_ms"]["sample_count"], 3);
        assert!(value["unavailable_metrics"].is_array());
        assert_eq!(value["benchmark"]["camera_path"], "yaw_360");
        assert_eq!(
            value["benchmark"]["lod_ring_table"],
            serde_json::json!([[2, 2]])
        );
        assert_eq!(value["benchmark"]["near_backend_resolved"], "gpu");
        assert_eq!(value["benchmark"]["far_backend_resolved"], "raster");
        assert!(value["benchmark"].get("resolved_near_backend").is_none());
    }

    #[test]
    fn draw_scopes_serialize_direct_and_indirect_counts_separately() {
        let mut result = fixture_result();
        result.metrics.draws.near.direct_draws = CountSummary::from_samples(&[7]);
        result.metrics.draws.near.indirect_draws = CountSummary::from_samples(&[0]);

        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["metrics"]["draws"]["near"]["direct_draws"]["p50"], 7);
        assert_eq!(
            value["metrics"]["draws"]["near"]["indirect_draws"]["p50"],
            0
        );
    }

    #[test]
    fn atomic_write_replaces_final_file_and_cleans_temporary_sibling() {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "mmo-render-bench-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("result.json");
        fs::write(&path, b"old").unwrap();

        write_result_atomic(&path, &fixture_result()).unwrap();

        let parsed: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed["schema_version"], 2);
        assert!(!fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.path().extension() == Some(OsStr::new("tmp"))));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn atomic_write_failure_cleans_temporary_sibling() {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "mmo-render-bench-failure-{}-{sequence}",
            std::process::id()
        ));
        let path = directory.join("result.json");
        fs::create_dir_all(&path).unwrap();

        assert!(write_result_atomic(&path, &fixture_result()).is_err());
        assert!(!fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.path().extension() == Some(OsStr::new("tmp"))));
        fs::remove_dir_all(directory).unwrap();
    }
}
