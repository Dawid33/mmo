//! What the panel knows of a world: read from its namespace (`deploy/modules/world`).
//!
//! A world is a namespace labelled `app.kubernetes.io/name=world`; its id is the
//! `app.kubernetes.io/instance` label. The objects the panel reads have the fixed
//! names the module (and `scripts/world_update.sh`) use: Deployments `coordinator`
//! and `toolbox`, StatefulSet `hosts`, ConfigMaps `host-endpoints` and
//! `release-history`, pods labelled by `app.kubernetes.io/component`.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::kube::{Kube, KubeError};
use crate::metrics::{self, PodMetrics};

pub const WORLD_SELECTOR: &str = "app.kubernetes.io/name=world";
pub const HEALTH_PORT: u16 = 9100;
pub const FORGE_COMPONENT: &str = "forge";

/// A Kubernetes object name: what a namespace, pod or Job name in a URL must look like.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Workload {
    pub desired: u32,
    pub ready: u32,
}

#[derive(Clone, Debug, Default)]
pub struct PodView {
    pub name: String,
    pub component: String,
    pub phase: String,
    pub ready: bool,
    pub restarts: u64,
    pub age: String,
    /// `/readyz`: "ok", or the 503's reason (forge, succession, vouched, starting).
    pub readyz: Option<String>,
    pub metrics: Option<PodMetrics>,
}

#[derive(Clone, Debug, Default)]
pub struct JobView {
    pub name: String,
    pub args: String,
    pub status: String,
    pub age: String,
    pub active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorldState {
    Running,
    Stopped,
    /// A coordinator or host waits for `worldctl forge`.
    AwaitingForge,
    /// Replicas are set but not every pod is Ready (starting, stopping, a succession, a crash).
    Changing,
    /// The namespace lacks the coordinator or the hosts (not applied fully, or no permission).
    Incomplete,
}

impl WorldState {
    pub fn label(self) -> &'static str {
        match self {
            WorldState::Running => "running",
            WorldState::Stopped => "stopped",
            WorldState::AwaitingForge => "awaiting forge",
            WorldState::Changing => "changing",
            WorldState::Incomplete => "incomplete",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct WorldView {
    pub id: String,
    pub namespace: String,
    pub coordinator: Option<Workload>,
    pub hosts: Option<Workload>,
    /// `hosts-<i>.env` keys in ConfigMap host-endpoints: the host count a start scales to.
    pub expected_hosts: usize,
    /// The toolbox's image: the running release's server image (what a forge Job runs).
    pub server_image: Option<String>,
    pub pods: Vec<PodView>,
    pub jobs: Vec<JobView>,
    /// ConfigMap release-history, key → value, as `world_update.sh` wrote it.
    pub release_history: Vec<(String, String)>,
    /// What could not be read (shown, never fatal).
    pub problems: Vec<String>,
}

impl WorldView {
    pub fn state(&self) -> WorldState {
        derive_state(self)
    }

    pub fn state_label(&self) -> &'static str {
        self.state().label()
    }

    /// The coordinator's metrics (world-wide series live there).
    pub fn coordinator_metrics(&self) -> Option<&PodMetrics> {
        self.pods
            .iter()
            .find(|p| p.component == "coordinator")
            .and_then(|p| p.metrics.as_ref())
    }

    pub fn summary_line(&self) -> String {
        let w = |o: &Option<Workload>| {
            o.as_ref()
                .map_or("—".to_string(), |w| format!("{}/{}", w.ready, w.desired))
        };
        format!(
            "coordinator {} · hosts {} (of {})",
            w(&self.coordinator),
            w(&self.hosts),
            self.expected_hosts
        )
    }

    /// Any coordinator or host process registered (the world is forged and serving).
    pub fn any_registered(&self) -> bool {
        self.pods.iter().any(|p| {
            p.readyz.as_deref() == Some("ok")
                || p.metrics.as_ref().and_then(|m| m.registered) == Some(true)
        })
    }

    pub fn forge_running(&self) -> bool {
        self.jobs.iter().any(|j| j.active)
    }
}

/// The world's state from what was read: pure, so it is tested without a cluster.
pub fn derive_state(w: &WorldView) -> WorldState {
    let (Some(c), Some(h)) = (&w.coordinator, &w.hosts) else {
        return WorldState::Incomplete;
    };
    let server_pods = w
        .pods
        .iter()
        .filter(|p| p.component == "coordinator" || p.component == "host")
        .count();
    if c.desired == 0 && h.desired == 0 && server_pods == 0 {
        return WorldState::Stopped;
    }
    if w.pods.iter().any(|p| p.readyz.as_deref() == Some("forge")) {
        return WorldState::AwaitingForge;
    }
    let want = u32::try_from(w.expected_hosts).unwrap_or(u32::MAX);
    if c.desired >= 1 && c.ready >= 1 && h.desired == want && h.ready >= want && want >= 1 {
        return WorldState::Running;
    }
    WorldState::Changing
}

/// Every world namespace's (id, namespace), sorted by namespace.
pub async fn list_worlds(kube: &Kube) -> Result<Vec<(String, String)>, KubeError> {
    let list = kube
        .get(&format!(
            "/api/v1/namespaces?labelSelector={}",
            enc(WORLD_SELECTOR)
        ))
        .await?;
    let mut out: Vec<(String, String)> = items(&list)
        .iter()
        .filter_map(|ns| {
            let name = ns.pointer("/metadata/name")?.as_str()?.to_string();
            let id = ns
                .pointer("/metadata/labels/app.kubernetes.io~1instance")
                .and_then(Value::as_str)
                .unwrap_or(&name)
                .to_string();
            Some((id, name))
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

/// The world in namespace `ns`, or `Ok(None)` when `ns` is not a world namespace.
pub async fn world_namespace(kube: &Kube, ns: &str) -> Result<Option<String>, KubeError> {
    let v = match kube.get(&format!("/api/v1/namespaces/{ns}")).await {
        Ok(v) => v,
        Err(KubeError::NotFound) => return Ok(None),
        Err(e) => return Err(e),
    };
    let labels = v.pointer("/metadata/labels");
    let is_world = labels
        .and_then(|l| l.get("app.kubernetes.io/name"))
        .and_then(Value::as_str)
        == Some("world");
    Ok(is_world.then(|| {
        labels
            .and_then(|l| l.get("app.kubernetes.io/instance"))
            .and_then(Value::as_str)
            .unwrap_or(ns)
            .to_string()
    }))
}

/// Reads one world. `deep` also asks every server pod for `/readyz` and `/metrics`.
pub async fn read_world(kube: &Kube, id: &str, ns: &str, deep: bool) -> WorldView {
    let mut w = WorldView {
        id: id.to_string(),
        namespace: ns.to_string(),
        ..WorldView::default()
    };
    let workload = |v: &Value| Workload {
        desired: u(v.pointer("/spec/replicas")),
        ready: u(v.pointer("/status/readyReplicas")),
    };
    match kube
        .get(&format!(
            "/apis/apps/v1/namespaces/{ns}/deployments/coordinator"
        ))
        .await
    {
        Ok(v) => w.coordinator = Some(workload(&v)),
        Err(e) => w.problems.push(format!("deployment coordinator: {e}")),
    }
    match kube
        .get(&format!("/apis/apps/v1/namespaces/{ns}/statefulsets/hosts"))
        .await
    {
        Ok(v) => w.hosts = Some(workload(&v)),
        Err(e) => w.problems.push(format!("statefulset hosts: {e}")),
    }
    match kube
        .get(&format!(
            "/apis/apps/v1/namespaces/{ns}/deployments/toolbox"
        ))
        .await
    {
        Ok(v) => {
            w.server_image = v
                .pointer("/spec/template/spec/containers/0/image")
                .and_then(Value::as_str)
                .map(str::to_string)
        }
        Err(e) => w.problems.push(format!("deployment toolbox: {e}")),
    }
    match kube
        .get(&format!(
            "/api/v1/namespaces/{ns}/configmaps/host-endpoints"
        ))
        .await
    {
        Ok(v) => w.expected_hosts = host_env_keys(&v),
        Err(e) => w.problems.push(format!("configmap host-endpoints: {e}")),
    }
    match kube
        .get(&format!(
            "/api/v1/namespaces/{ns}/configmaps/release-history"
        ))
        .await
    {
        Ok(v) => {
            if let Some(data) = v.get("data").and_then(Value::as_object) {
                w.release_history = data
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                    .collect();
            }
        }
        Err(KubeError::NotFound) => {}
        Err(e) => w.problems.push(format!("configmap release-history: {e}")),
    }
    match kube.get(&format!("/api/v1/namespaces/{ns}/pods")).await {
        Ok(v) => w.pods = items(&v).iter().map(pod_view).collect(),
        Err(e) => w.problems.push(format!("pods: {e}")),
    }
    w.pods.sort_by(|a, b| a.name.cmp(&b.name));
    let jobs = format!(
        "/apis/batch/v1/namespaces/{ns}/jobs?labelSelector={}",
        enc(&format!("app.kubernetes.io/component={FORGE_COMPONENT}"))
    );
    match kube.get(&jobs).await {
        Ok(v) => w.jobs = items(&v).iter().map(job_view).collect(),
        Err(e) => w.problems.push(format!("jobs: {e}")),
    }
    w.jobs.sort_by(|a, b| b.name.cmp(&a.name));
    if deep {
        for pod in w.pods.iter_mut().filter(|p| {
            (p.component == "coordinator" || p.component == "host") && p.phase == "Running"
        }) {
            let base = format!(
                "/api/v1/namespaces/{ns}/pods/{}:{HEALTH_PORT}/proxy",
                pod.name
            );
            pod.readyz = match kube.get_text(&format!("{base}/readyz")).await {
                Ok((200, _)) => Some("ok".into()),
                Ok((503, body)) if !body.trim_start().starts_with('{') => {
                    Some(body.trim().to_string())
                }
                Ok(_) | Err(_) => None,
            };
            pod.metrics = match kube.get_text(&format!("{base}/metrics")).await {
                Ok((200, body)) => Some(metrics::parse(&body)),
                _ => None,
            };
        }
    }
    w
}

/// The number of `hosts-<i>.env` keys in ConfigMap host-endpoints (as `world-start` counts them).
pub fn host_env_keys(cm: &Value) -> usize {
    cm.get("data")
        .and_then(Value::as_object)
        .map_or(0, |d| d.keys().filter(|k| is_host_env_key(k)).count())
}

fn is_host_env_key(k: &str) -> bool {
    k.strip_prefix("hosts-")
        .and_then(|r| r.strip_suffix(".env"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// `hosts-<digits>`: a host pod of the StatefulSet.
pub fn is_host_pod(name: &str) -> bool {
    name.strip_prefix("hosts-")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

pub fn items(list: &Value) -> &[Value] {
    list.get("items")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn pod_view(p: &Value) -> PodView {
    let s = |ptr: &str| {
        p.pointer(ptr)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let statuses = p
        .pointer("/status/containerStatuses")
        .and_then(Value::as_array);
    let deleting = p.pointer("/metadata/deletionTimestamp").is_some();
    let phase = s("/status/phase");
    PodView {
        name: s("/metadata/name"),
        component: s("/metadata/labels/app.kubernetes.io~1component"),
        phase: if deleting {
            format!("{phase} (terminating)")
        } else {
            phase
        },
        ready: statuses.is_some_and(|c| {
            !c.is_empty() && c.iter().all(|c| c.get("ready") == Some(&Value::Bool(true)))
        }),
        restarts: statuses.map_or(0, |c| {
            c.iter()
                .map(|c| c.get("restartCount").and_then(Value::as_u64).unwrap_or(0))
                .sum()
        }),
        age: age(p
            .pointer("/metadata/creationTimestamp")
            .and_then(Value::as_str)),
        readyz: None,
        metrics: None,
    }
}

fn job_view(j: &Value) -> JobView {
    let n = |ptr: &str| u(j.pointer(ptr));
    let status = if n("/status/succeeded") > 0 {
        "succeeded"
    } else if n("/status/failed") > 0 {
        "failed"
    } else if n("/status/active") > 0 {
        "running"
    } else {
        "pending"
    };
    let args = j
        .pointer("/spec/template/spec/containers/0/args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .skip_while(|a| *a == "forge")
                .collect::<Vec<_>>()
                .join(" ")
                .replace("--db $(VOXEL_WORLD_DB) ", "")
                .replace("--db $(VOXEL_WORLD_DB)", "")
        })
        .unwrap_or_default();
    JobView {
        name: j
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        args,
        status: status.to_string(),
        age: age(j
            .pointer("/metadata/creationTimestamp")
            .and_then(Value::as_str)),
        active: status == "running" || status == "pending",
    }
}

fn u(v: Option<&Value>) -> u32 {
    v.and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

/// Percent-encodes a label selector for a query string.
pub fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// "3m", "2h", "5d" since an RFC 3339 UTC timestamp (`2026-11-15T10:00:00Z`).
pub fn age(ts: Option<&str>) -> String {
    ts.and_then(parse_rfc3339_utc)
        .map_or_else(|| "?".into(), |t| short_duration(now_s().saturating_sub(t)))
}

pub fn short_duration(s: u64) -> String {
    match s {
        0..=119 => format!("{s}s"),
        120..=7199 => format!("{}m", s / 60),
        7200..=172_799 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

/// Seconds since the epoch of `YYYY-MM-DDTHH:MM:SSZ` (what the API server writes).
pub fn parse_rfc3339_utc(ts: &str) -> Option<u64> {
    let b = ts.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| ts.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, s) = (num(11..13)?, num(14..16)?, num(17..19)?);
    // Days from civil (Howard Hinnant's algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + s).ok()
}
