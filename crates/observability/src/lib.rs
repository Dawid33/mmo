//! Shared telemetry for the native login and world-service binaries.
//!
//! This layer deliberately contains no identity or payload fields. Callers attach
//! only bounded operational categories; transport and profile failures never
//! become process-health failures.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opentelemetry::KeyValue;
use opentelemetry::global;
use opentelemetry::propagation::{
    Extractor, Injector, TextMapCompositePropagator, TextMapPropagator,
};
use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::export::trace::{ExportResult, SpanData, SpanExporter};
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};
use opentelemetry_sdk::trace::{BatchConfigBuilder, BatchSpanProcessor, Sampler, TracerProvider};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

const DEFAULT_BSP_MAX_QUEUE_SIZE: usize = 2048;
const DEFAULT_BSP_MAX_EXPORT_BATCH_SIZE: usize = 256;
const DEFAULT_BSP_SCHEDULE_DELAY_MS: u64 = 2_000;
const DEFAULT_BSP_EXPORT_TIMEOUT_MS: u64 = 5_000;
#[cfg(feature = "pyroscope")]
const DEFAULT_PYROSCOPE_SAMPLE_RATE: u32 = 100;

static EXPORT_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static EXPORT_FAILURES: AtomicU64 = AtomicU64::new(0);
static PROFILE_STARTS: AtomicU64 = AtomicU64::new(0);
static PROFILE_FAILURES: AtomicU64 = AtomicU64::new(0);
static LAST_EXPORT_LOG_SECS: AtomicU64 = AtomicU64::new(0);
const TRACEPARENT_MAX: usize = 55;
const TRACESTATE_MAX: usize = 512;

/// Static, low-cardinality identity for one process.
#[derive(Clone, Debug)]
pub struct ProcessIdentity {
    pub service_name: &'static str,
    pub role: &'static str,
    pub world_id: Option<String>,
    pub environment: String,
    pub build_revision: String,
    pub cluster: Option<String>,
    pub namespace: Option<String>,
    pub pod: Option<String>,
    pub node: Option<String>,
}

/// Current sampled W3C headers for an HTTP header map. The result contains no
/// baggage or user data.
pub fn current_headers() -> Vec<(String, String)> {
    let mut carrier = Headers::default();
    inject_current(&mut carrier);
    let mut values = Vec::new();
    if let Some(value) = carrier.traceparent {
        values.push(("traceparent".into(), value));
    }
    if let Some(value) = carrier.tracestate {
        values.push(("tracestate".into(), value));
    }
    values
}

/// A bounded opaque operation id, independent of player/account identity.
pub fn operation_id() -> String {
    if let Some(trace_id) = current_trace_id() {
        return trace_id;
    }
    use rand_core::{OsRng, RngCore};
    let mut bytes = [0; 16];
    OsRng.fill_bytes(&mut bytes);
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Set a tracing span's remote parent from a validated or untrusted extracted
/// context. OpenTelemetry ignores invalid values.
pub fn set_parent(span: &tracing::Span, context: opentelemetry::Context) {
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    span.set_parent(context);
}

/// The sampled trace id of the active span, formatted for Loki/Grafana.
pub fn current_trace_id() -> Option<String> {
    correlated_ids().map(|(trace, _)| trace)
}

impl ProcessIdentity {
    pub fn from_env(service_name: &'static str, role: &'static str) -> Self {
        ProcessIdentity {
            service_name,
            role,
            world_id: nonempty("VOXEL_WORLD_ID"),
            environment: std::env::var("VOXEL_TELEMETRY_ENVIRONMENT")
                .ok()
                .and_then(trimmed)
                .unwrap_or_else(|| "development".into()),
            build_revision: option_env!("ERRANTRY_BUILD_REVISION")
                .map(str::to_owned)
                .or_else(|| nonempty("ERRANTRY_BUILD_REVISION"))
                .unwrap_or_else(|| "unknown".into()),
            cluster: nonempty("K8S_CLUSTER_NAME").or_else(|| nonempty("VOXEL_TELEMETRY_CLUSTER")),
            namespace: nonempty("K8S_NAMESPACE_NAME"),
            pod: nonempty("K8S_POD_NAME"),
            node: nonempty("K8S_NODE_NAME"),
        }
    }
}

/// Runtime telemetry settings, with standard OTEL overrides.
#[derive(Clone, Debug)]
pub struct TelemetryConfig {
    pub identity: ProcessIdentity,
    pub trace_ratio: f64,
}

impl TelemetryConfig {
    pub fn from_env(identity: ProcessIdentity, trace_ratio: f64) -> Result<Self, String> {
        if !trace_ratio.is_finite() || !(0.0..=1.0).contains(&trace_ratio) {
            return Err(format!("trace ratio {trace_ratio} is outside 0..=1"));
        }
        Ok(Self {
            identity,
            trace_ratio,
        })
    }
}

/// A validated trace carrier. It is safe to retain beside an asynchronous
/// operation because it contains only W3C context and a random operation id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceCarrier {
    pub traceparent: String,
    pub tracestate: Option<String>,
    pub operation_id: String,
}

/// Owns the process subscriber/exporter and optional profiler until orderly exit.
pub struct TelemetryGuard {
    provider: Option<TracerProvider>,
    exporter_runtime: Option<ExporterRuntime>,
    profile: Option<Box<dyn FnOnce() + Send>>,
    identity: ProcessIdentity,
}

impl TelemetryGuard {
    /// Flushes bounded exporter work at process exit. Never panics.
    pub fn shutdown(mut self) {
        self.stop_exporter();
    }

    fn stop_exporter(&mut self) {
        if let Some(stop_profile) = self.profile.take() {
            let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
            std::thread::spawn(move || {
                stop_profile();
                let _ = done_tx.send(());
            });
            if done_rx.recv_timeout(Duration::from_secs(2)).is_err() {
                log::warn!(
                    "[telemetry] Pyroscope shutdown exceeded 2 s; the process is not held up"
                );
            }
        }
        if let Some(provider) = self.provider.take() {
            if provider.shutdown().is_err() {
                EXPORT_FAILURES.fetch_add(1, Ordering::Relaxed);
                log::warn!(
                    "[telemetry] OTLP shutdown flush failed; readiness is unchanged; role={} world_id={}",
                    self.identity.role,
                    self.identity.world_id.as_deref().unwrap_or("")
                );
            }
        }
        if let Some(runtime) = self.exporter_runtime.take() {
            runtime.stop();
        }
    }

    pub fn identity(&self) -> &ProcessIdentity {
        &self.identity
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        self.stop_exporter();
    }
}

/// Install the one process subscriber and exporters. Invalid non-secret dials
/// are fatal; backend availability is not.
pub fn init(config: TelemetryConfig) -> Result<TelemetryGuard, String> {
    install_propagators();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let sdk_disabled = std::env::var("OTEL_SDK_DISABLED")
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"));
    let endpoint = (!sdk_disabled)
        .then(|| nonempty("OTEL_EXPORTER_OTLP_ENDPOINT"))
        .flatten();
    let (otel, provider, exporter_runtime) = if endpoint.is_some() {
        let (tracer, provider, exporter_runtime) = start_exporter(config.clone())?;
        global::set_tracer_provider(provider.clone());
        (
            Some(tracing_opentelemetry::layer().with_tracer(tracer)),
            Some(provider),
            Some(exporter_runtime),
        )
    } else {
        (None, None, None)
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .event_format(CorrelatedFormat {
                    role: config.identity.role,
                    world_id: config.identity.world_id.clone(),
                })
                .with_ansi(false),
        )
        .with(otel)
        .try_init()
        .map_err(|e| format!("install the tracing subscriber: {e}"))?;

    let profile = start_profile(&config);
    let guard = TelemetryGuard {
        provider,
        exporter_runtime,
        profile,
        identity: config.identity,
    };
    log::info!(
        "[telemetry] role={} world_id={} environment={} tracing={} profiles={}",
        guard.identity.role,
        guard.identity.world_id.as_deref().unwrap_or(""),
        guard.identity.environment,
        if guard.provider.is_some() {
            "on"
        } else {
            "off"
        },
        if guard.profile.is_some() { "on" } else { "off" }
    );
    Ok(guard)
}

/// A basic subscriber for one-shot CLI commands that never enable export.
pub fn init_cli() -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_ansi(false)
        .try_init()
        .map_err(|e| format!("install the logging subscriber: {e}"))
}

fn build_tracer(
    config: &TelemetryConfig,
) -> Result<(opentelemetry_sdk::trace::Tracer, TracerProvider), String> {
    let protocol =
        std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL").unwrap_or_else(|_| "http/protobuf".into());
    if protocol != "http/protobuf" {
        return Err(format!(
            "OTEL_EXPORTER_OTLP_PROTOCOL={protocol:?} is unsupported; expected http/protobuf"
        ));
    }

    let batch_timeout_ms = u64_env("OTEL_BSP_EXPORT_TIMEOUT", DEFAULT_BSP_EXPORT_TIMEOUT_MS)?;
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(nonempty("OTEL_EXPORTER_OTLP_ENDPOINT").expect("checked before build"))
        .with_timeout(Duration::from_millis(
            batch_timeout_ms.saturating_sub(250).max(1),
        ))
        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
        .build()
        .map_err(|e| format!("build the OTLP/HTTP trace exporter: {e}"))?;
    let max_queue_size = uint_env("OTEL_BSP_MAX_QUEUE_SIZE", DEFAULT_BSP_MAX_QUEUE_SIZE)?;
    let max_export_batch_size = uint_env(
        "OTEL_BSP_MAX_EXPORT_BATCH_SIZE",
        DEFAULT_BSP_MAX_EXPORT_BATCH_SIZE,
    )?;
    if max_export_batch_size > max_queue_size {
        return Err("OTEL_BSP_MAX_EXPORT_BATCH_SIZE cannot exceed OTEL_BSP_MAX_QUEUE_SIZE".into());
    }
    let batch = BatchConfigBuilder::default()
        .with_max_queue_size(max_queue_size)
        .with_max_export_batch_size(max_export_batch_size)
        .with_scheduled_delay(Duration::from_millis(u64_env(
            "OTEL_BSP_SCHEDULE_DELAY",
            DEFAULT_BSP_SCHEDULE_DELAY_MS,
        )?))
        .with_max_export_timeout(Duration::from_millis(batch_timeout_ms))
        .build();
    let resource = resource(&config.identity);
    let processor = BatchSpanProcessor::builder(
        CountingExporter { inner: exporter },
        opentelemetry_sdk::runtime::Tokio,
    )
    .with_batch_config(batch)
    .build();
    let provider = TracerProvider::builder()
        .with_sampler(sampler(config.trace_ratio)?)
        .with_resource(resource)
        .with_span_processor(processor)
        .build();
    let tracer = provider.tracer(config.identity.service_name.to_string());
    Ok((tracer, provider))
}

fn start_exporter(
    config: TelemetryConfig,
) -> Result<
    (
        opentelemetry_sdk::trace::Tracer,
        TracerProvider,
        ExporterRuntime,
    ),
    String,
> {
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let thread = std::thread::Builder::new()
        .name("otel-export".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(format!("build the OTLP exporter runtime: {error}")));
                    return;
                }
            };
            let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
            let built = {
                let _entered = runtime.enter();
                build_tracer(&config)
            };
            match built {
                Ok((tracer, provider)) => {
                    let _ = ready_tx.send(Ok((tracer, provider, stop_tx)));
                    let _ = runtime.block_on(stop_rx);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            }
        })
        .map_err(|e| format!("spawn the OTLP exporter thread: {e}"))?;
    match ready_rx
        .recv()
        .map_err(|_| "the OTLP exporter thread stopped before setup".to_string())?
    {
        Ok((tracer, provider, stop)) => Ok((
            tracer,
            provider,
            ExporterRuntime {
                stop: Some(stop),
                thread: Some(thread),
            },
        )),
        Err(error) => {
            let _ = thread.join();
            Err(error)
        }
    }
}

struct ExporterRuntime {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ExporterRuntime {
    fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ExporterRuntime {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

#[derive(Debug)]
struct CountingExporter<E> {
    inner: E,
}

impl<E: SpanExporter> SpanExporter for CountingExporter<E> {
    fn export(
        &mut self,
        batch: Vec<SpanData>,
    ) -> futures_util::future::BoxFuture<'static, ExportResult> {
        EXPORT_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
        let future = self.inner.export(batch);
        Box::pin(async move {
            let result = future.await;
            if result.is_err() {
                EXPORT_FAILURES.fetch_add(1, Ordering::Relaxed);
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let last = LAST_EXPORT_LOG_SECS.swap(now, Ordering::Relaxed);
                if now.saturating_sub(last) >= 60 {
                    log::warn!("[telemetry] an OTLP batch export failed; readiness is unchanged");
                }
            }
            result
        })
    }

    fn shutdown(&mut self) {
        self.inner.shutdown();
    }

    fn force_flush(&mut self) -> futures_util::future::BoxFuture<'static, ExportResult> {
        self.inner.force_flush()
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.inner.set_resource(resource);
    }
}

fn sampler(default: f64) -> Result<Sampler, String> {
    let name = std::env::var("OTEL_TRACES_SAMPLER")
        .ok()
        .and_then(trimmed)
        .unwrap_or_else(|| "parentbased_traceidratio".into());
    let configured = std::env::var("OTEL_TRACES_SAMPLER_ARG")
        .ok()
        .and_then(trimmed);
    let ratio = match configured {
        Some(raw) => raw
            .parse::<f64>()
            .map_err(|_| format!("OTEL_TRACES_SAMPLER_ARG={raw:?} is not a number"))?,
        None => default,
    };
    if !ratio.is_finite() || !(0.0..=1.0).contains(&ratio) {
        return Err(format!("OTEL_TRACES_SAMPLER_ARG={ratio} is outside 0..=1"));
    }
    match name.as_str() {
        "always_on" => Ok(Sampler::AlwaysOn),
        "always_off" => Ok(Sampler::AlwaysOff),
        "traceidratio" => Ok(Sampler::TraceIdRatioBased(ratio)),
        "parentbased_always_on" => Ok(Sampler::ParentBased(Box::new(Sampler::AlwaysOn))),
        "parentbased_always_off" => Ok(Sampler::ParentBased(Box::new(Sampler::AlwaysOff))),
        "parentbased_traceidratio" => Ok(Sampler::ParentBased(Box::new(
            Sampler::TraceIdRatioBased(ratio),
        ))),
        other => Err(format!("OTEL_TRACES_SAMPLER={other:?} is unsupported")),
    }
}

fn resource(identity: &ProcessIdentity) -> Resource {
    let mut values = vec![
        KeyValue::new("service.name", identity.service_name),
        KeyValue::new("mmo.role", identity.role),
        KeyValue::new("deployment.environment.name", identity.environment.clone()),
        KeyValue::new("service.version", identity.build_revision.clone()),
        KeyValue::new("mmo.build_revision", identity.build_revision.clone()),
    ];
    if let Some(value) = &identity.world_id {
        values.push(KeyValue::new("mmo.world_id", value.clone()));
    }
    if let Some(value) = &identity.cluster {
        values.push(KeyValue::new("k8s.cluster.name", value.clone()));
    }
    if let Some(value) = &identity.namespace {
        values.push(KeyValue::new("k8s.namespace.name", value.clone()));
    }
    if let Some(value) = &identity.pod {
        values.push(KeyValue::new("service.instance.id", value.clone()));
        values.push(KeyValue::new("k8s.pod.name", value.clone()));
    }
    if let Some(value) = &identity.node {
        values.push(KeyValue::new("k8s.node.name", value.clone()));
    }
    Resource::new(values)
}

fn start_profile(config: &TelemetryConfig) -> Option<Box<dyn FnOnce() + Send>> {
    if !std::env::var("PYROSCOPE")
        .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
    {
        return None;
    }
    #[cfg(not(feature = "pyroscope"))]
    {
        let _ = config;
        PROFILE_FAILURES.fetch_add(1, Ordering::Relaxed);
        log::warn!(
            "[telemetry] PYROSCOPE is set but this binary was built without the pyroscope feature; no profile started"
        );
        return None;
    }
    #[cfg(feature = "pyroscope")]
    {
        use pyroscope::PyroscopeAgent;
        use pyroscope_pprofrs::{PprofConfig, pprof_backend};
        PROFILE_STARTS.fetch_add(1, Ordering::Relaxed);
        let endpoint =
            nonempty("PYROSCOPE_SERVER").unwrap_or_else(|| "http://localhost:4040".into());
        let rate = match u32_env("PYROSCOPE_SAMPLE_RATE", DEFAULT_PYROSCOPE_SAMPLE_RATE) {
            Ok(rate) if rate > 0 => rate,
            Ok(_) | Err(_) => {
                PROFILE_FAILURES.fetch_add(1, Ordering::Relaxed);
                log::error!(
                    "[telemetry] PYROSCOPE_SAMPLE_RATE must be above zero; no profile started"
                );
                return None;
            }
        };
        let mut tags = vec![
            ("role", config.identity.role),
            ("environment", config.identity.environment.as_str()),
            ("build_revision", config.identity.build_revision.as_str()),
        ];
        if let Some(world) = config.identity.world_id.as_deref() {
            tags.push(("world_id", world));
        }
        if let Some(cluster) = config.identity.cluster.as_deref() {
            tags.push(("cluster", cluster));
        }
        if let Some(namespace) = config.identity.namespace.as_deref() {
            tags.push(("namespace", namespace));
        }
        if let Some(pod) = config.identity.pod.as_deref() {
            tags.push(("pod", pod));
        }
        let builder = PyroscopeAgent::builder(endpoint, config.identity.service_name.to_string())
            .backend(pprof_backend(PprofConfig::new().sample_rate(rate)))
            .tags(tags);
        match builder.build().and_then(|agent| agent.start()) {
            Ok(running) => Some(Box::new(move || match running.stop() {
                Ok(ready) => ready.shutdown(),
                Err(error) => {
                    PROFILE_FAILURES.fetch_add(1, Ordering::Relaxed);
                    log::error!("[telemetry] Pyroscope did not stop cleanly: {error}");
                }
            })),
            Err(error) => {
                PROFILE_FAILURES.fetch_add(1, Ordering::Relaxed);
                log::error!("[telemetry] Pyroscope did not start: {error}; readiness is unchanged");
                None
            }
        }
    }
}

fn nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().and_then(trimmed)
}

fn trimmed(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn uint_env(name: &str, default: usize) -> Result<usize, String> {
    let Some(raw) = nonempty(name) else {
        return Ok(default);
    };
    let value = raw
        .parse::<usize>()
        .map_err(|_| format!("{name}={raw:?} is not a positive integer"))?;
    if value == 0 {
        return Err(format!("{name} must be above zero"));
    }
    Ok(value)
}

fn u64_env(name: &str, default: u64) -> Result<u64, String> {
    let Some(raw) = nonempty(name) else {
        return Ok(default);
    };
    let value = raw
        .parse::<u64>()
        .map_err(|_| format!("{name}={raw:?} is not a positive integer"))?;
    if value == 0 {
        return Err(format!("{name} must be above zero"));
    }
    Ok(value)
}

#[cfg(feature = "pyroscope")]
fn u32_env(name: &str, default: u32) -> Result<u32, String> {
    let Some(raw) = nonempty(name) else {
        return Ok(default);
    };
    raw.parse::<u32>()
        .map_err(|_| format!("{name}={raw:?} is not an integer"))
}

/// The current tracing subscriber's W3C headers, if its context is valid and sampled.
pub fn current_carrier(operation_id: impl Into<String>) -> Option<TraceCarrier> {
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    let context = tracing::Span::current().context();
    let span = context.span();
    let span_context = span.span_context();
    if !span_context.is_valid() || !span_context.is_sampled() {
        return None;
    }
    let mut carrier = Headers::default();
    global::get_text_map_propagator(|propagator| propagator.inject_context(&context, &mut carrier));
    TraceCarrier::new(
        carrier.get("traceparent")?,
        carrier.get("tracestate").map(str::to_owned),
        operation_id.into(),
    )
}

/// Parse a bounded W3C carrier from incoming HTTP headers. Invalid or unsampled
/// parents are ignored rather than affecting request behavior.
pub fn carrier_from_headers<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
    operation_id: impl Into<String>,
) -> Option<TraceCarrier> {
    let mut traceparent = None;
    let mut tracestate = None;
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("traceparent") {
            traceparent = Some(value);
        } else if name.eq_ignore_ascii_case("tracestate") {
            tracestate = Some(value);
        }
    }
    TraceCarrier::new(
        traceparent?,
        tracestate.map(str::to_owned),
        operation_id.into(),
    )
}

impl TraceCarrier {
    pub fn new(
        traceparent: impl Into<String>,
        tracestate: Option<String>,
        operation_id: String,
    ) -> Option<Self> {
        let traceparent = traceparent.into();
        let tracestate = tracestate.map(|value| value.trim().to_owned());
        if traceparent.len() > TRACEPARENT_MAX
            || traceparent.len() < 55
            || !valid_traceparent(&traceparent)
        {
            return None;
        }
        if tracestate
            .as_ref()
            .is_some_and(|value| value.len() > TRACESTATE_MAX)
        {
            return None;
        }
        if operation_id.is_empty() || operation_id.len() > 64 {
            return None;
        }
        Some(Self {
            traceparent,
            tracestate,
            operation_id,
        })
    }

    /// A validated OpenTelemetry parent context.
    pub fn context(&self) -> opentelemetry::Context {
        let propagator = TraceContextPropagator::new();
        propagator.extract(&Headers {
            traceparent: Some(self.traceparent.clone()),
            tracestate: self.tracestate.clone(),
        })
    }
}

fn valid_traceparent(value: &str) -> bool {
    let mut parts = value.split('-');
    let Some(version) = parts.next() else {
        return false;
    };
    let Some(trace) = parts.next() else {
        return false;
    };
    let Some(parent) = parts.next() else {
        return false;
    };
    let Some(flags) = parts.next() else {
        return false;
    };
    if parts.next().is_some()
        || version.len() != 2
        || trace.len() != 32
        || parent.len() != 16
        || flags.len() != 2
    {
        return false;
    }
    let hex = |value: &str| {
        value
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    };
    // Only sampled contexts cross the durable mailbox.
    version == "00"
        && trace != "00000000000000000000000000000000"
        && parent != "0000000000000000000000"
        && hex(trace)
        && hex(parent)
        && flags == "01"
}

/// Install the standard W3C TraceContext and Baggage propagators.
pub fn install_propagators() {
    global::set_text_map_propagator(TextMapCompositePropagator::new(vec![
        Box::new(TraceContextPropagator::new()),
        Box::new(BaggagePropagator::new()),
    ]));
}

/// Insert current sampled trace headers into an HTTP request map.
pub fn inject_current<I: Injector>(injector: &mut I) {
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    let context = tracing::Span::current().context();
    global::get_text_map_propagator(|propagator| propagator.inject_context(&context, injector));
}

/// Extract an HTTP trace parent into an OpenTelemetry context.
pub fn extract_context<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> opentelemetry::Context {
    let mut values = Headers::default();
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("traceparent") {
            values.traceparent = Some(value.to_owned());
        } else if name.eq_ignore_ascii_case("tracestate") {
            values.tracestate = Some(value.to_owned());
        }
    }
    TraceContextPropagator::new().extract(&values)
}

#[derive(Default)]
struct Headers {
    traceparent: Option<String>,
    tracestate: Option<String>,
}

impl Injector for Headers {
    fn set(&mut self, key: &str, value: String) {
        if key.eq_ignore_ascii_case("traceparent") {
            self.traceparent = Some(value);
        } else if key.eq_ignore_ascii_case("tracestate") {
            self.tracestate = Some(value);
        }
    }
}

impl Extractor for Headers {
    fn get(&self, key: &str) -> Option<&str> {
        if key.eq_ignore_ascii_case("traceparent") {
            self.traceparent.as_deref()
        } else if key.eq_ignore_ascii_case("tracestate") {
            self.tracestate.as_deref()
        } else {
            None
        }
    }

    fn keys(&self) -> Vec<&str> {
        let mut keys = Vec::new();
        if self.traceparent.is_some() {
            keys.push("traceparent");
        }
        if self.tracestate.is_some() {
            keys.push("tracestate");
        }
        keys
    }
}

fn correlated_ids() -> Option<(String, String)> {
    use opentelemetry::trace::TraceContextExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    let context = tracing::Span::current().context();
    let span = context.span();
    let context = span.span_context();
    if !context.is_valid() {
        return None;
    }
    Some((
        format!("{:032x}", context.trace_id()),
        format!("{:016x}", context.span_id()),
    ))
}

#[derive(Clone)]
struct CorrelatedFormat {
    role: &'static str,
    world_id: Option<String>,
}

impl<S, N> tracing_subscriber::fmt::FormatEvent<S, N> for CorrelatedFormat
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        write!(
            writer,
            "{ms} {:>5} {}: ",
            event.metadata().level(),
            event.metadata().target()
        )?;
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        write!(writer, " role={}", self.role)?;
        if let Some(world_id) = &self.world_id {
            write!(writer, " world_id={world_id}")?;
        }
        if let Some((trace_id, span_id)) = correlated_ids() {
            write!(writer, " trace_id={trace_id} span_id={span_id}")?;
        }
        writeln!(writer)
    }
}

/// Bounded telemetry counters for the existing process metrics registries.
pub fn counters() -> TelemetryCounters {
    TelemetryCounters {
        export_attempts: EXPORT_ATTEMPTS.load(Ordering::Relaxed),
        export_failures: EXPORT_FAILURES.load(Ordering::Relaxed),
        profile_starts: PROFILE_STARTS.load(Ordering::Relaxed),
        profile_failures: PROFILE_FAILURES.load(Ordering::Relaxed),
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TelemetryCounters {
    pub export_attempts: u64,
    pub export_failures: u64,
    pub profile_starts: u64,
    pub profile_failures: u64,
}

/// Record one asynchronously observed exporter failure without blocking.
pub fn record_export_failure() {
    EXPORT_FAILURES.fetch_add(1, Ordering::Relaxed);
}

/// Record the duration of an operation if its caller wants to emit an event.
pub fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod telemetry_probe {
    /// Regression: global logger initialization and no Tokio runtime prevented a
    /// bounded OTLP failure probe from starting/flushing (observability probe, shared logger bridge and dedicated exporter runtime).
    #[test]
    fn unavailable_exporter_shuts_down_without_runtime() {
        unsafe {
            std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:1");
            std::env::set_var("OTEL_BSP_SCHEDULE_DELAY", "10");
            std::env::set_var("OTEL_BSP_EXPORT_TIMEOUT", "100");
        }
        let identity = super::ProcessIdentity::from_env("errantry-probe", "host");
        let guard = super::init(super::TelemetryConfig::from_env(identity, 1.0).unwrap()).unwrap();
        tracing::info_span!("probe.export").in_scope(|| tracing::info!("probe"));
        guard.shutdown();
        assert!(super::counters().export_failures >= 1);
    }
}
