//! Start, stop and forge — the `world-start` / `world-stop` recipes and
//! `world-ctl forge`, step for step (justfile, "One game world is one Kubernetes
//! namespace"). Start and stop run as one background operation per world; its log
//! is what the world's page shows. A second operation on a world whose first is
//! still running is refused.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::forge::ForgeRequest;
use crate::kube::{Kube, KubeError, Scalable};
use crate::world::{self, HEALTH_PORT};

#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// `WORLD_STOP_TIMEOUT` (150 s): each wait for pods to be gone.
    pub stop: Duration,
    /// `WORLD_START_TIMEOUT` (300 s): each wait for Ready.
    pub start: Duration,
    pub poll: Duration,
    /// The recipe's `sleep 1`s: lets a log follower attach, and the last lines arrive.
    pub settle: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            stop: Duration::from_secs(150),
            start: Duration::from_secs(300),
            poll: Duration::from_secs(2),
            settle: Duration::from_secs(1),
        }
    }
}

#[derive(Clone, Debug)]
pub struct OpSnapshot {
    pub kind: &'static str,
    pub started_s: u64,
    pub lines: Vec<String>,
    /// None while running.
    pub outcome: Option<Result<(), String>>,
}

impl OpSnapshot {
    pub fn running(&self) -> bool {
        self.outcome.is_none()
    }
    pub fn status(&self) -> &'static str {
        match &self.outcome {
            None => "running",
            Some(Ok(())) => "done",
            Some(Err(_)) => "failed",
        }
    }
    pub fn error(&self) -> Option<&str> {
        self.outcome
            .as_ref()
            .and_then(|o| o.as_ref().err())
            .map(String::as_str)
    }
    pub fn age(&self) -> String {
        world::short_duration(world::now_s().saturating_sub(self.started_s))
    }
}

#[derive(Clone)]
struct OpLog(Arc<Mutex<OpSnapshot>>);

impl OpLog {
    fn line(&self, s: impl Into<String>) {
        let s = s.into();
        log::info!("[panel] {s}");
        self.0.lock().expect("op log").lines.push(s);
    }
    fn finish(&self, r: Result<(), String>) {
        if let Err(e) = &r {
            log::warn!("[panel] failed: {e}");
        }
        self.0.lock().expect("op log").outcome = Some(r);
    }
}

pub struct Ops {
    kube: Kube,
    timeouts: Timeouts,
    ops: Mutex<BTreeMap<String, OpLog>>,
}

impl Ops {
    pub fn new(kube: Kube, timeouts: Timeouts) -> Arc<Ops> {
        Arc::new(Ops {
            kube,
            timeouts,
            ops: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn kube(&self) -> &Kube {
        &self.kube
    }

    /// The last operation on the world in `ns` (running or finished).
    pub fn current(&self, ns: &str) -> Option<OpSnapshot> {
        self.ops
            .lock()
            .expect("ops")
            .get(ns)
            .map(|l| l.0.lock().expect("op log").clone())
    }

    fn begin(&self, ns: &str, kind: &'static str) -> Result<OpLog, String> {
        let mut ops = self.ops.lock().expect("ops");
        if let Some(prev) = ops.get(ns) {
            let prev = prev.0.lock().expect("op log");
            if prev.running() {
                return Err(format!("a {} of this world is still running", prev.kind));
            }
        }
        let log = OpLog(Arc::new(Mutex::new(OpSnapshot {
            kind,
            started_s: world::now_s(),
            lines: Vec::new(),
            outcome: None,
        })));
        ops.insert(ns.to_string(), log.clone());
        Ok(log)
    }

    /// Starts the world in `ns` in the background.
    pub fn start_world(self: &Arc<Self>, ns: &str) -> Result<(), String> {
        let log = self.begin(ns, "start")?;
        let (me, ns) = (Arc::clone(self), ns.to_string());
        tokio::spawn(async move {
            let r = me.run_start(&ns, &log).await;
            log.finish(r);
        });
        Ok(())
    }

    /// Stops the world in `ns` in the background.
    pub fn stop_world(self: &Arc<Self>, ns: &str) -> Result<(), String> {
        let log = self.begin(ns, "stop")?;
        let (me, ns) = (Arc::clone(self), ns.to_string());
        tokio::spawn(async move {
            let r = me.run_stop(&ns, &log).await;
            log.finish(r);
        });
        Ok(())
    }

    async fn run_start(&self, ns: &str, log: &OpLog) -> Result<(), String> {
        let k = &self.kube;
        let cm = k
            .get(&format!(
                "/api/v1/namespaces/{ns}/configmaps/host-endpoints"
            ))
            .await
            .map_err(|e| format!("host-endpoints: {e}"))?;
        let count = world::host_env_keys(&cm);
        if count < 1 {
            return Err(
                "ConfigMap host-endpoints has no hosts-<i>.env key; refusing to start".into(),
            );
        }
        let count = u32::try_from(count).map_err(|_| "too many hosts")?;
        log.line("coordinator to 1");
        k.scale(ns, Scalable::Deployment, "coordinator", 1)
            .await
            .map_err(|e| format!("scale coordinator: {e}"))?;
        let deploy = format!("/apis/apps/v1/namespaces/{ns}/deployments/coordinator");
        let deadline = tokio::time::Instant::now() + self.timeouts.start;
        loop {
            if ready(k.get(&deploy).await) >= 1 {
                break;
            }
            // Unready because it waits for its forge: say so instead of waiting out the timeout.
            if self.coordinator_waits_for_forge(ns).await {
                log.line("the coordinator runs and waits for the forge; the hosts are not started");
                return Err("the world is not forged: forge it, then start again".into());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "after {} s the coordinator is not Ready",
                    self.timeouts.start.as_secs()
                ));
            }
            tokio::time::sleep(self.timeouts.poll).await;
        }
        log.line(format!("hosts to {count}"));
        k.scale(ns, Scalable::StatefulSet, "hosts", count)
            .await
            .map_err(|e| format!("scale hosts: {e}"))?;
        // OnDelete StatefulSet: no rollout status; poll readyReplicas (a failed read keeps waiting).
        let sts = format!("/apis/apps/v1/namespaces/{ns}/statefulsets/hosts");
        let deadline = tokio::time::Instant::now() + self.timeouts.start;
        loop {
            let n = ready(k.get(&sts).await);
            if n >= count {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "after {} s: {n} of {count} hosts Ready",
                    self.timeouts.start.as_secs()
                ));
            }
            tokio::time::sleep(self.timeouts.poll).await;
        }
        log.line(format!("the world is up ({count} hosts)"));
        Ok(())
    }

    async fn coordinator_waits_for_forge(&self, ns: &str) -> bool {
        let Ok(pods) = self
            .kube
            .get(&format!(
                "/api/v1/namespaces/{ns}/pods?labelSelector={}",
                world::enc("app.kubernetes.io/component=coordinator")
            ))
            .await
        else {
            return false;
        };
        for p in world::items(&pods) {
            let Some(name) = p.pointer("/metadata/name").and_then(Value::as_str) else {
                continue;
            };
            let path = format!("/api/v1/namespaces/{ns}/pods/{name}:{HEALTH_PORT}/proxy/readyz");
            if let Ok((503, body)) = self.kube.get_text(&path).await
                && body.trim() == "forge"
            {
                return true;
            }
        }
        false
    }

    async fn run_stop(&self, ns: &str, log: &OpLog) -> Result<(), String> {
        let k = &self.kube;
        let coord = k
            .get(&format!(
                "/apis/apps/v1/namespaces/{ns}/deployments/coordinator"
            ))
            .await
            .map_err(|e| format!("cannot read the coordinator; nothing changed: {e}"))?;
        let coord = coord
            .pointer("/spec/replicas")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let pods = self
            .pod_names(ns)
            .await
            .map_err(|e| format!("cannot list the pods; nothing changed: {e}"))?;
        let host_pods: Vec<String> = pods
            .iter()
            .filter(|p| world::is_host_pod(p))
            .cloned()
            .collect();
        // With the coordinator already down the hosts cannot finish their leave cut.
        if coord == 0 && !host_pods.is_empty() {
            return Err("the coordinator is at 0 replicas but host pods still exist; they cannot leave cleanly. \
                        Start the world first, or delete the host pods knowingly. Nothing changed."
                .into());
        }
        // Each host's log is followed until its pod ends: its leave line is read after it is gone.
        let followers: Vec<(String, Arc<Mutex<String>>, tokio::task::JoinHandle<()>)> = host_pods
            .iter()
            .map(|pod| {
                let buf = Arc::new(Mutex::new(String::new()));
                let task = tokio::spawn(follow(
                    k.clone(),
                    ns.to_string(),
                    pod.clone(),
                    Arc::clone(&buf),
                ));
                (pod.clone(), buf, task)
            })
            .collect();
        tokio::time::sleep(self.timeouts.settle).await;
        log.line(format!(
            "hosts to 0, waiting until no host pod is left ({} s)",
            self.timeouts.stop.as_secs()
        ));
        let scaled = k
            .scale(ns, Scalable::StatefulSet, "hosts", 0)
            .await
            .map_err(|e| format!("scale hosts: {e}"));
        let gone = match scaled {
            Ok(()) => self.wait_gone(ns, world::is_host_pod).await,
            Err(e) => Err(e),
        };
        if gone.is_ok() {
            tokio::time::sleep(self.timeouts.settle).await;
        }
        for (_, _, task) in &followers {
            task.abort();
        }
        gone?;
        log.line("the hosts' leave:");
        if followers.is_empty() {
            log.line("    (no host pod was running)");
        }
        let (mut no_cut, mut unknown) = (Vec::new(), Vec::new());
        for (pod, buf, _) in &followers {
            let text = buf.lock().expect("log buffer").clone();
            match leave_of(&text) {
                Leave::NoCut(l) => {
                    log.line(format!("    {pod}: LEFT WITHOUT A CUT: {l}"));
                    no_cut.push(pod.clone());
                }
                Leave::Cut(l) => log.line(format!("    {pod}: {l}")),
                Leave::Unknown => {
                    log.line(format!("    {pod}: no leave line seen in its log"));
                    unknown.push(pod.clone());
                }
            }
        }
        if !no_cut.is_empty() {
            log.line(format!(
                "WARNING: host(s) {} left WITHOUT a committed cut. The next start raises the world generation: \
                 the world rolls back to its last committed cut. Going on.",
                no_cut.join(" ")
            ));
        }
        if !unknown.is_empty() {
            log.line(format!(
                "WARNING: no leave line seen for {} (its log could not be followed to the end); whether it left \
                 with a cut is not known. Going on.",
                unknown.join(" ")
            ));
        }
        log.line("coordinator to 0");
        k.scale(ns, Scalable::Deployment, "coordinator", 0)
            .await
            .map_err(|e| format!("scale coordinator: {e}"))?;
        self.wait_gone(ns, |p| p.starts_with("coordinator-"))
            .await?;
        log.line("the world is down (web and toolbox keep running)");
        Ok(())
    }

    async fn pod_names(&self, ns: &str) -> Result<Vec<String>, KubeError> {
        let pods = self
            .kube
            .get(&format!("/api/v1/namespaces/{ns}/pods"))
            .await?;
        Ok(world::items(&pods)
            .iter()
            .filter_map(|p| {
                p.pointer("/metadata/name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect())
    }

    /// Until no pod name matches; a failed listing is retried, never read as empty.
    async fn wait_gone(&self, ns: &str, matches: impl Fn(&str) -> bool) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + self.timeouts.stop;
        loop {
            let what = match self.pod_names(ns).await {
                Ok(names) => {
                    let left = names.iter().filter(|n| matches(n)).count();
                    if left == 0 {
                        return Ok(());
                    }
                    format!("{left} pod(s) still there")
                }
                Err(e) => format!("the pod listing failed ({e})"),
            };
            if tokio::time::Instant::now() >= deadline {
                return Err(format!("after {} s: {what}", self.timeouts.stop.as_secs()));
            }
            tokio::time::sleep(self.timeouts.poll).await;
        }
    }

    /// Creates a forge Job in the world `id` / `ns`. Refused while a forge runs or once
    /// a coordinator or host has registered (worldctl refuses a forged store anyway).
    pub async fn forge(&self, id: &str, ns: &str, req: &ForgeRequest) -> Result<String, String> {
        if let Some(op) = self.current(ns)
            && op.running()
        {
            return Err(format!("a {} of this world is still running", op.kind));
        }
        let w = world::read_world(&self.kube, id, ns, true).await;
        if w.forge_running() {
            return Err("a forge Job of this world is still running".into());
        }
        if w.any_registered() {
            return Err(
                "a coordinator or host of this world is registered: the world is already forged"
                    .into(),
            );
        }
        let toolbox = self
            .kube
            .get(&format!(
                "/apis/apps/v1/namespaces/{ns}/deployments/toolbox"
            ))
            .await
            .map_err(|e| format!("the toolbox (its image is the forge's): {e}"))?;
        let spec = toolbox.pointer("/spec/template/spec");
        let image = spec
            .and_then(|s| s.pointer("/containers/0/image"))
            .and_then(Value::as_str)
            .ok_or("the toolbox has no image")?;
        let pull = spec
            .and_then(|s| s.get("imagePullSecrets"))
            .cloned()
            .unwrap_or_else(|| json!([]));
        let name = format!("forge-{}", world::now_s());
        self.kube
            .post(
                &format!("/apis/batch/v1/namespaces/{ns}/jobs"),
                &req.job(&name, id, image, &pull),
            )
            .await
            .map_err(|e| format!("create Job {name}: {e}"))?;
        log::info!(
            "[panel] {ns}: forge Job {name} created ({})",
            req.args()[3..].join(" ")
        );
        Ok(name)
    }

    /// The log of a forge Job's pod (the newest one).
    pub async fn job_log(&self, ns: &str, job: &str) -> Result<String, String> {
        let pods = self
            .kube
            .get(&format!(
                "/api/v1/namespaces/{ns}/pods?labelSelector={}",
                world::enc(&format!("job-name={job}"))
            ))
            .await
            .map_err(|e| e.to_string())?;
        let mut names: Vec<(String, String)> = world::items(&pods)
            .iter()
            .filter_map(|p| {
                Some((
                    p.pointer("/metadata/creationTimestamp")?
                        .as_str()?
                        .to_string(),
                    p.pointer("/metadata/name")?.as_str()?.to_string(),
                ))
            })
            .collect();
        names.sort();
        let (_, pod) = names.pop().ok_or("the Job has no pod (yet, or any more)")?;
        self.kube
            .log_tail(ns, &pod, 500)
            .await
            .map_err(|e| format!("{pod}: {e}"))
    }
}

/// `status.readyReplicas` of a read object; a failed read counts as 0.
fn ready(v: Result<Value, KubeError>) -> u32 {
    v.ok()
        .and_then(|v| v.pointer("/status/readyReplicas").and_then(Value::as_u64))
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

/// The most a followed log keeps (its tail).
const FOLLOW_KEEP: usize = 256 * 1024;

async fn follow(kube: Kube, ns: String, pod: String, buf: Arc<Mutex<String>>) {
    let mut resp = match kube.follow_log(&ns, &pod, 20).await {
        Ok(r) => r,
        Err(e) => {
            log::warn!("[panel] {ns}/{pod}: cannot follow its log: {e}");
            return;
        }
    };
    while let Ok(Some(chunk)) = resp.chunk().await {
        let mut b = buf.lock().expect("log buffer");
        b.push_str(&String::from_utf8_lossy(&chunk));
        if b.len() > FOLLOW_KEEP {
            let mut cut = b.len() - FOLLOW_KEEP;
            while !b.is_char_boundary(cut) {
                cut += 1;
            }
            b.drain(..cut);
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Leave {
    /// "left without a cut" (the next start raises the world generation).
    NoCut(String),
    /// "[server] left the world in cut …".
    Cut(String),
    Unknown,
}

/// A host's leave from its log (the lines `world-stop` and `world_update.sh` read).
pub fn leave_of(log: &str) -> Leave {
    let from_marker = |line: &str, marker: &str| {
        line.find(marker)
            .map_or(line, |i| &line[i..])
            .trim()
            .to_string()
    };
    if let Some(l) = log.lines().rfind(|l| l.contains("left without a cut")) {
        return Leave::NoCut(from_marker(l, "[server]"));
    }
    if let Some(l) = log
        .lines()
        .rfind(|l| l.contains("[server] left the world in cut"))
    {
        return Leave::Cut(from_marker(l, "[server] left the world in cut"));
    }
    Leave::Unknown
}
