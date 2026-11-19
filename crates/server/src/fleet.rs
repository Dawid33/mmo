//! `VOXEL_ROLE=fleet`: the reconciler (Elastic 4). It makes the world's Kubernetes namespace
//! match the `desired_hosts` rows the coordinator's decisions write: one Deployment, Service and
//! PodDisruptionBudget `host-<name>` per row, rendered from the module's template (the ConfigMap
//! [`TEMPLATE_CONFIGMAP`], key [`TEMPLATE_KEY`]), and none for a name no row holds.
//!
//! **What it is not.** It writes nothing to the world store (opened [`world_store::OpenMode::ReadOnly`])
//! and never talks to the coordinator; nothing it does keeps two writers apart — the fence in
//! Postgres does. Deleting a host Deployment is always "SIGTERM → the host's leave cut" (a
//! disruption, never a loss). It reaches only its own cluster's API and the world's database.
//!
//! **A pass** ([`FLEET_PASS_MS`]): read the `coordinator` Deployment (its `spec.replicas` and the
//! image of its `coordinator` container), the template, and the Deployments, Services and PDBs
//! labelled [`FLEET_LABEL`]`=<world id>`, then the desired rows — **a failed read ends the pass,
//! nothing is created or deleted**; the rows are read LAST, so an object whose name they no longer
//! show was really removed, even with two reconcilers overlapping (a deleted pod's replacement
//! beside the old one in its grace period: `Recreate` orders a rollout, not a pod's replacement);
//! then [`plan_pass`] (pure) and the actions in its order. A failed action is logged (at most one line per
//! [`FLEET_LOG_EVERY_MS`] per object), counted (`mmo_fleet_errors_total`), skips the rest of that
//! host's actions in this pass, and is retried by the next pass.
//!
//! **An EMPTY read deletes every labelled host object** (plan Review Focus 6): a world re-forged
//! under a running reconciler, or a coordinator that removed every row, has no desired host, so
//! every host pod gets SIGTERM and leaves through its leave cut — a disruption, never a loss. A
//! FAILED read (a database outage, an API server that does not answer) deletes nothing and
//! creates nothing: everything running keeps running, and `/readyz` turns 503 after
//! [`FLEET_READY_MS`].
//!
//! **The stop rule** (plan decision 8): while the `coordinator` Deployment is at 0 replicas
//! (`world-stop`, a release under way) it creates and re-templates nothing — it still deletes —
//! and it never patches an existing Deployment's replicas or image: the recipes own those.
//! A host Deployment is created on the coordinator's image at that moment, at 1 replica.
//!
//! **Ruled here** (Elastic 4 Task 4): a draining row gets nothing created (a host made for a
//! name that is leaving would be a new, undrained host); an object labelled for this world but
//! without [`HOST_LABEL`] is never touched; a re-template is a `PUT` carrying the read
//! `resourceVersion`, so a Deployment scaled since the read is left alone (409, retried).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use game::coordinator::DesiredHost;
use kube_api::{Kube, KubeError};
use serde_json::{json, Value};
use world_store::{StoreHandle, StoreUrl};

use crate::claims::ServerExit;
use crate::health::{Health, STOPPED};

/// The reconciler's period (plan decision 8).
pub const FLEET_PASS_MS: u64 = 2_000;
/// `/readyz` is 200 only while the last pass without a failed read or action is at most this old.
pub const FLEET_READY_MS: u64 = 3 * FLEET_PASS_MS;
/// At most one log line per object (or per failed read) this often.
pub const FLEET_LOG_EVERY_MS: u64 = 10_000;
/// How often the loop looks at the stop flag while it waits.
const FLEET_STOP_POLL: Duration = Duration::from_millis(100);
// One API request (the loop beats between them) never outlasts `/livez`'s stall limit.
const _: () = assert!((kube_api::REQUEST_TIMEOUT.as_millis() as u64) < crate::health::LIVEZ_STALL_MS);

/// Environment: the namespace of the world's objects (required).
pub const FLEET_NAMESPACE_ENV: &str = "VOXEL_FLEET_NAMESPACE";
/// Environment: the first node port; slot `s` gets QUIC `base + 2·s` and WebTransport `base + 2·s + 1`.
pub const FLEET_PORT_BASE_ENV: &str = "VOXEL_FLEET_PORT_BASE";
/// Environment: a `kubectl proxy` base URL (optional; unset: the in-cluster API server).
pub const FLEET_KUBE_API_ENV: &str = "VOXEL_FLEET_KUBE_API";

/// The label every reconciled object carries: the world id.
pub const FLEET_LABEL: &str = "errantry/fleet";
/// The label every reconciled object carries: its `desired_hosts` name.
pub const HOST_LABEL: &str = "errantry/host";
/// A host Deployment's annotation: the sha256 (hex) of the template it was rendered from.
pub const TEMPLATE_ANNOTATION: &str = "errantry/template";
/// The ConfigMap holding the template, and its key.
pub const TEMPLATE_CONFIGMAP: &str = "host-template";
pub const TEMPLATE_KEY: &str = "template.json";
/// The Deployment whose replicas gate creation and whose image a new host gets, and its container.
pub const COORDINATOR_DEPLOYMENT: &str = "coordinator";
pub const COORDINATOR_CONTAINER: &str = "coordinator";
/// The container of a host Deployment that runs the server (the image rule's subject).
pub const HOST_CONTAINER: &str = "host";
/// Every reconciled object is `host-<name>`.
pub const OBJECT_PREFIX: &str = "host-";
/// Placeholders of the template ([`Template::render`]).
pub const NAME_PLACEHOLDER: &str = "__NAME__";
pub const QUIC_PORT_PLACEHOLDER: &str = "__QUIC_PORT__";
pub const WT_PORT_PLACEHOLDER: &str = "__WT_PORT__";
pub const IMAGE_PLACEHOLDER: &str = "__IMAGE__";

/// The three kinds of a host's objects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Service,
    Pdb,
    Deployment,
}

impl Kind {
    /// The order a host's missing objects are created in.
    pub const CREATE_ORDER: [Kind; 3] = [Kind::Service, Kind::Pdb, Kind::Deployment];
    /// The order a removed host's objects are deleted in (the pod first, its address last).
    pub const DELETE_ORDER: [Kind; 3] = [Kind::Deployment, Kind::Pdb, Kind::Service];

    /// The manifest's `kind`.
    pub fn manifest_kind(self) -> &'static str {
        match self {
            Kind::Service => "Service",
            Kind::Pdb => "PodDisruptionBudget",
            Kind::Deployment => "Deployment",
        }
    }

    /// The API path of this kind's collection in namespace `ns`.
    pub fn collection(self, ns: &str) -> String {
        match self {
            Kind::Service => format!("/api/v1/namespaces/{ns}/services"),
            Kind::Pdb => format!("/apis/policy/v1/namespaces/{ns}/poddisruptionbudgets"),
            Kind::Deployment => format!("/apis/apps/v1/namespaces/{ns}/deployments"),
        }
    }
}

/// One labelled object as listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Object {
    /// `metadata.name`.
    pub name: String,
    /// Its [`HOST_LABEL`]; `None`: never touched.
    pub host: Option<String>,
    /// `metadata.resourceVersion`.
    pub resource_version: Option<String>,
    /// A Deployment's `spec.replicas` (absent: 1, the API's default).
    pub replicas: u32,
    /// A Deployment's [`TEMPLATE_ANNOTATION`].
    pub template: Option<String>,
    /// A Deployment's [`HOST_CONTAINER`] image.
    pub image: Option<String>,
}

impl Object {
    /// The fields the plan needs from a listed object.
    pub fn from_value(v: &Value) -> Object {
        let meta = &v["metadata"];
        let s = |x: &Value| x.as_str().map(str::to_string);
        Object {
            name: s(&meta["name"]).unwrap_or_default(),
            host: s(&meta["labels"][HOST_LABEL]),
            resource_version: s(&meta["resourceVersion"]),
            replicas: replicas_of(v),
            template: s(&meta["annotations"][TEMPLATE_ANNOTATION]),
            image: container_image(v, HOST_CONTAINER),
        }
    }
}

/// The labelled objects of one pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Objects {
    pub deployments: Vec<Object>,
    pub services: Vec<Object>,
    pub pdbs: Vec<Object>,
}

impl Objects {
    pub fn of(&self, kind: Kind) -> &[Object] {
        match kind {
            Kind::Service => &self.services,
            Kind::Pdb => &self.pdbs,
            Kind::Deployment => &self.deployments,
        }
    }

    fn has(&self, kind: Kind, host: &str) -> bool {
        self.of(kind).iter().any(|o| o.host.as_deref() == Some(host))
    }
}

/// What the pass read of the `coordinator` Deployment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoordinatorSeen {
    /// `spec.replicas` (absent: 1). 0: the world is stopped — nothing is created or re-templated.
    pub replicas: u32,
    /// The image of its [`COORDINATOR_CONTAINER`] container: what a new host runs.
    pub image: String,
}

impl CoordinatorSeen {
    pub fn from_value(v: &Value) -> Result<CoordinatorSeen, String> {
        let image = container_image(v, COORDINATOR_CONTAINER)
            .ok_or_else(|| format!("the {COORDINATOR_DEPLOYMENT} Deployment has no `{COORDINATOR_CONTAINER}` container with an image"))?;
        Ok(CoordinatorSeen { replicas: replicas_of(v), image })
    }
}

/// `spec.replicas`, absent → 1 (the API's default).
fn replicas_of(v: &Value) -> u32 {
    v["spec"]["replicas"].as_u64().map_or(1, |n| u32::try_from(n).unwrap_or(u32::MAX))
}

/// The image of the container `name` of a Deployment's pod template.
fn container_image(v: &Value, name: &str) -> Option<String> {
    v["spec"]["template"]["spec"]["containers"]
        .as_array()?
        .iter()
        .find(|c| c["name"].as_str() == Some(name))
        .and_then(|c| c["image"].as_str())
        .map(str::to_string)
}

/// The host template: the ConfigMap's text, its hash, and what renders it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    /// The JSON list of three manifests (Deployment, Service, PodDisruptionBudget) with placeholders.
    pub text: String,
    /// The sha256 of `text`, lowercase hex: a Deployment's [`TEMPLATE_ANNOTATION`].
    pub hash: String,
    /// The world id: every rendered object must carry [`FLEET_LABEL`]`=<world>`.
    pub world: String,
    /// [`FLEET_PORT_BASE_ENV`].
    pub port_base: u32,
}

/// A row's three manifests, rendered and checked.
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    pub service: Value,
    pub pdb: Value,
    pub deployment: Value,
}

impl Rendered {
    fn of(&self, kind: Kind) -> &Value {
        match kind {
            Kind::Service => &self.service,
            Kind::Pdb => &self.pdb,
            Kind::Deployment => &self.deployment,
        }
    }
}

impl Template {
    pub fn new(text: String, world: &str, port_base: u32) -> Template {
        use sha2::Digest;
        let hash = sha2::Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        Template { text, hash, world: world.to_string(), port_base }
    }

    /// The ports of `slot`: QUIC `port_base + 2·slot`, WebTransport one above.
    pub fn ports(&self, slot: u32) -> Result<(u16, u16), String> {
        let quic = u64::from(self.port_base) + 2 * u64::from(slot);
        match (u16::try_from(quic), u16::try_from(quic + 1)) {
            (Ok(q), Ok(w)) if q > 0 => Ok((q, w)),
            _ => Err(format!("slot {slot} has no port: {FLEET_PORT_BASE_ENV}={} + 2·{slot} is out of range", self.port_base)),
        }
    }

    /// Renders host `name` at `slot` on `image`: the text with every placeholder substituted —
    /// `"__QUIC_PORT__"` / `"__WT_PORT__"` WITH their quotes become bare JSON numbers, then any
    /// `__QUIC_PORT__` / `__WT_PORT__` left inside a longer string become the number's digits,
    /// `__NAME__` the name and `__IMAGE__` the image — then parsed. Refused: a name or image that
    /// cannot sit in a JSON string as it is, a text that is not a list of exactly one Deployment,
    /// one Service and one PodDisruptionBudget, an object not named `host-<name>` or without both
    /// labels ([`FLEET_LABEL`] = the world, [`HOST_LABEL`] = the name), a Deployment whose
    /// [`HOST_CONTAINER`] container does not run `image`.
    pub fn render(&self, name: &str, slot: u32, image: &str) -> Result<Rendered, String> {
        let name_ok = !name.is_empty()
            && name.len() + OBJECT_PREFIX.len() <= 63
            && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !name_ok {
            return Err(format!("the host name {name:?} is not a DNS label"));
        }
        if image.is_empty() || image.chars().any(|c| c == '"' || c == '\\' || c.is_whitespace() || c.is_control()) {
            return Err(format!("the image {image:?} cannot be put in the template"));
        }
        let (quic, wt) = self.ports(slot)?;
        let quoted = |p: &str| format!("\"{p}\"");
        let text = self
            .text
            .replace(&quoted(QUIC_PORT_PLACEHOLDER), &quic.to_string())
            .replace(&quoted(WT_PORT_PLACEHOLDER), &wt.to_string())
            .replace(QUIC_PORT_PLACEHOLDER, &quic.to_string())
            .replace(WT_PORT_PLACEHOLDER, &wt.to_string())
            .replace(NAME_PLACEHOLDER, name)
            .replace(IMAGE_PLACEHOLDER, image);
        let list: Value = serde_json::from_str(&text).map_err(|e| format!("the template is not JSON once rendered: {e}"))?;
        let Some(items) = list.as_array() else {
            return Err("the template is not a JSON list".into());
        };
        let mut found: BTreeMap<Kind, Value> = BTreeMap::new();
        for item in items {
            let kind = Kind::CREATE_ORDER
                .into_iter()
                .find(|k| item["kind"].as_str() == Some(k.manifest_kind()))
                .ok_or_else(|| format!("the template holds a `{}`, not a Deployment, Service or PodDisruptionBudget", item["kind"]))?;
            if found.insert(kind, item.clone()).is_some() {
                return Err(format!("the template holds two of kind {}", kind.manifest_kind()));
            }
        }
        let object = format!("{OBJECT_PREFIX}{name}");
        for kind in Kind::CREATE_ORDER {
            let Some(item) = found.get(&kind) else {
                return Err(format!("the template has no {}", kind.manifest_kind()));
            };
            let meta = &item["metadata"];
            if meta["name"].as_str() != Some(object.as_str()) {
                return Err(format!("the template's {} is not named {object}", kind.manifest_kind()));
            }
            if meta["labels"][FLEET_LABEL].as_str() != Some(self.world.as_str()) {
                return Err(format!("the template's {} lacks the label {FLEET_LABEL}={}", kind.manifest_kind(), self.world));
            }
            if meta["labels"][HOST_LABEL].as_str() != Some(name) {
                return Err(format!("the template's {} lacks the label {HOST_LABEL}={name}", kind.manifest_kind()));
            }
        }
        let take = |k: Kind, found: &mut BTreeMap<Kind, Value>| found.remove(&k).expect("checked above");
        let rendered =
            Rendered { service: take(Kind::Service, &mut found), pdb: take(Kind::Pdb, &mut found), deployment: take(Kind::Deployment, &mut found) };
        if container_image(&rendered.deployment, HOST_CONTAINER).as_deref() != Some(image) {
            return Err(format!("the template's Deployment has no `{HOST_CONTAINER}` container running {IMAGE_PLACEHOLDER}"));
        }
        Ok(rendered)
    }

    /// A rendered Deployment as it is sent: `replicas` and the [`TEMPLATE_ANNOTATION`] set.
    fn deployment(&self, rendered: &Rendered, replicas: u32, resource_version: Option<&str>) -> Value {
        let mut d = rendered.deployment.clone();
        d["spec"]["replicas"] = json!(replicas);
        if !d["metadata"]["annotations"].is_object() {
            d["metadata"]["annotations"] = json!({});
        }
        d["metadata"]["annotations"][TEMPLATE_ANNOTATION] = json!(self.hash);
        if let Some(rv) = resource_version {
            d["metadata"]["resourceVersion"] = json!(rv);
        }
        d
    }
}

/// One step of a pass, applied in the order [`plan_pass`] gives.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// `DELETE` the object `name` of `kind` (gone already is success).
    Delete { kind: Kind, name: String },
    /// `POST` `body` (named `name`) to `kind`'s collection (exists already is success).
    Create { kind: Kind, name: String, body: Value },
    /// `PUT` `body` over the Deployment `name` (a re-template at 0 replicas, its read
    /// `resourceVersion` inside: a change since the read is a conflict, retried next pass).
    Replace { name: String, body: Value },
}

impl Action {
    /// The object it acts on (`host-<name>`: the same for a host's three kinds).
    pub fn object(&self) -> &str {
        match self {
            Action::Delete { name, .. } | Action::Create { name, .. } | Action::Replace { name, .. } => name,
        }
    }

    fn kind(&self) -> Kind {
        match self {
            Action::Delete { kind, .. } | Action::Create { kind, .. } => *kind,
            Action::Replace { .. } => Kind::Deployment,
        }
    }
}

/// [`plan_pass`]'s actions and the rows it could not render.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub actions: Vec<Action>,
    /// `(row name, why)`: nothing is created or re-templated for that row this pass.
    pub refused: Vec<(String, String)>,
}

/// One pass's actions (pure): [`plan`] without its refusals.
pub fn plan_pass(desired: &[DesiredHost], objects: &Objects, coordinator: &CoordinatorSeen, template: &Template) -> Vec<Action> {
    plan(desired, objects, coordinator, template).actions
}

/// One pass's plan (pure). First every labelled object whose [`HOST_LABEL`] is in no row:
/// `Delete`, per host (by name) Deployment, PDB, Service. Then — only when the coordinator's
/// `spec.replicas` > 0 — per row in slot order: a wanted row's missing objects `Create`d (Service,
/// PDB, Deployment at 1 replica on the coordinator's image, annotated with the template hash), and
/// any row's Deployment at 0 replicas whose annotation is not the hash `Replace`d (the new
/// template, still 0 replicas, its own image). A Deployment above 0 replicas is never touched.
pub fn plan(desired: &[DesiredHost], objects: &Objects, coordinator: &CoordinatorSeen, template: &Template) -> Plan {
    let mut out = Plan::default();
    let wanted: BTreeSet<&str> = desired.iter().map(|r| r.name.as_str()).collect();
    let strays: BTreeSet<&str> = Kind::DELETE_ORDER
        .iter()
        .flat_map(|&k| objects.of(k))
        .filter_map(|o| o.host.as_deref())
        .filter(|h| !wanted.contains(h))
        .collect();
    for host in strays {
        for kind in Kind::DELETE_ORDER {
            for o in objects.of(kind).iter().filter(|o| o.host.as_deref() == Some(host)) {
                out.actions.push(Action::Delete { kind, name: o.name.clone() });
            }
        }
    }
    if coordinator.replicas == 0 {
        return out;
    }
    let mut rows: Vec<&DesiredHost> = desired.iter().collect();
    rows.sort_by(|a, b| (a.slot, &a.name).cmp(&(b.slot, &b.name)));
    for row in rows {
        let object = format!("{OBJECT_PREFIX}{}", row.name);
        let missing: Vec<Kind> = Kind::CREATE_ORDER.into_iter().filter(|&k| !objects.has(k, &row.name)).collect();
        if !row.draining && !missing.is_empty() {
            match template.render(&row.name, row.slot, &coordinator.image) {
                Ok(rendered) => {
                    for kind in missing {
                        let body = match kind {
                            Kind::Deployment => template.deployment(&rendered, 1, None),
                            k => rendered.of(k).clone(),
                        };
                        out.actions.push(Action::Create { kind, name: object.clone(), body });
                    }
                }
                Err(why) => out.refused.push((row.name.clone(), why)),
            }
        }
        let stale = objects.deployments.iter().filter(|d| {
            d.host.as_deref() == Some(row.name.as_str()) && d.replicas == 0 && d.template.as_deref() != Some(template.hash.as_str())
        });
        for d in stale {
            let Some(image) = d.image.as_deref() else {
                out.refused.push((row.name.clone(), format!("the Deployment {} has no `{HOST_CONTAINER}` image to keep", d.name)));
                continue;
            };
            match template.render(&row.name, row.slot, image) {
                Ok(rendered) => out.actions.push(Action::Replace {
                    name: d.name.clone(),
                    body: template.deployment(&rendered, 0, d.resource_version.as_deref()),
                }),
                Err(why) => out.refused.push((row.name.clone(), why)),
            }
        }
    }
    out
}

/// The role's settings from the environment (`var` reads one variable).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetConfig {
    pub namespace: String,
    /// `VOXEL_WORLD_ID`: the value of [`FLEET_LABEL`].
    pub world: String,
    pub port_base: u32,
}

impl FleetConfig {
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Result<FleetConfig, String> {
        let read = |name: &str| var(name).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let namespace = read(FLEET_NAMESPACE_ENV).ok_or_else(|| format!("VOXEL_ROLE=fleet needs {FLEET_NAMESPACE_ENV}"))?;
        let world_env = crate::auth_edge::WORLD_ID_ENV;
        let world = read(world_env).ok_or_else(|| format!("VOXEL_ROLE=fleet needs {world_env}: the value of its {FLEET_LABEL} label"))?;
        if world.len() > 63 || !world.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')) {
            return Err(format!("{world_env}={world:?} cannot be a label value"));
        }
        let base = read(FLEET_PORT_BASE_ENV).ok_or_else(|| format!("VOXEL_ROLE=fleet needs {FLEET_PORT_BASE_ENV}"))?;
        let port_base = match base.parse::<u32>() {
            Ok(n) if (1..=65_534).contains(&n) => n,
            _ => return Err(format!("invalid {FLEET_PORT_BASE_ENV} value `{base}` (expected a port)")),
        };
        Ok(FleetConfig { namespace, world, port_base })
    }
}

/// What one successful action did.
enum Applied {
    Created,
    Existed,
    Deleted,
    Replaced,
}

/// The reconciler: its store, its API client, its log limits.
struct Fleet {
    store: StoreHandle,
    kube: Kube,
    config: FleetConfig,
    runtime: tokio::runtime::Runtime,
    /// Per object (or failed read): when its last failure was logged.
    logged: BTreeMap<String, Instant>,
    /// The last pass saw the coordinator at 0 (`None`: no pass read it yet).
    stopped: Option<bool>,
}

/// Runs the role until a stop signal (`stop`): waits for a forged store of this build's schema
/// (`/readyz` 503 `forge`), then a pass every [`FLEET_PASS_MS`]. Always exits clean.
pub fn run(url: &StoreUrl, config: FleetConfig, kube: Kube, stop: &AtomicBool) -> ServerExit {
    let health = Health::global();
    health.require_fleet_passes();
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => crate::fatal(&format!("[fleet] the HTTP runtime: {e}")),
    };
    let Some(store) = wait_for_store(url, stop) else {
        log::info!("[fleet] stop signal while waiting for a world: exiting");
        return ServerExit::Clean;
    };
    log::info!(
        "[fleet] reconciling namespace {} for world {} (ports from {}), a pass every {FLEET_PASS_MS} ms",
        config.namespace,
        config.world,
        config.port_base
    );
    health.set_registered(true);
    let mut fleet = Fleet { store, kube, config, runtime, logged: BTreeMap::new(), stopped: None };
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        health.loop_beat();
        let ok = fleet.pass();
        health.fleet_pass_done(ok);
        health.loop_beat();
        let until = started + Duration::from_millis(FLEET_PASS_MS);
        while Instant::now() < until && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(FLEET_STOP_POLL);
        }
    }
    log::info!("[fleet] stop signal: exiting");
    health.set_registered(false);
    fleet.store.shutdown();
    ServerExit::Clean
}

/// Opens the store read-only once it is forged and at this build's schema, retrying every
/// [`crate::world_file::EMPTY_STORE_POLL`] (`/readyz` 503 `forge` meanwhile); `None` on a stop.
fn wait_for_store(url: &StoreUrl, stop: &AtomicBool) -> Option<StoreHandle> {
    let health = Health::global();
    health.set_waiting(Some("forge"));
    let mut logged: Option<Instant> = None;
    loop {
        let why = match StoreHandle::open(url, world_store::Dials::BUILT, world_store::OpenMode::ReadOnly) {
            Ok(store) => match store.world_info() {
                Ok(info) if info.has_artifact => {
                    health.set_waiting(None);
                    return Some(store);
                }
                Ok(_) => {
                    store.shutdown();
                    "the world store has no world yet (waiting for 'worldctl forge')".to_string()
                }
                Err(e) => {
                    store.shutdown();
                    format!("the world store cannot be read ({e})")
                }
            },
            // Not forged yet (no schema), or a schema that is not this build's: waited out.
            Err(e) => format!("the world store {url} cannot be opened read-only ({e})"),
        };
        if logged.is_none_or(|at| at.elapsed() >= Duration::from_millis(FLEET_LOG_EVERY_MS)) {
            logged = Some(Instant::now());
            log::warn!("[fleet] {why}: still waiting");
        }
        let until = Instant::now() + crate::world_file::EMPTY_STORE_POLL;
        while Instant::now() < until {
            if stop.load(Ordering::SeqCst) {
                return None;
            }
            std::thread::sleep(FLEET_STOP_POLL);
        }
    }
}

impl Fleet {
    /// One pass; `true` when every read succeeded, nothing was refused and no action failed.
    fn pass(&mut self) -> bool {
        let health = Health::global();
        // 1. + 2. The coordinator, the template, the labelled objects — BEFORE the desired rows
        // (final review, B-M3): an object listed here whose name the later read no longer shows
        // was really removed (a removed row never comes back, a name is never issued twice), so
        // even two overlapping reconcilers (a deleted pod's replacement beside the old one in its
        // grace period) never delete a host the other just created for a newer row.
        let read = self.runtime.block_on(read_cluster(&self.kube, &self.config));
        let (coordinator, template, objects) = match read {
            Ok(read) => read,
            Err(why) => {
                self.failed("read cluster", format!("{why}; nothing created or deleted this pass"));
                return false;
            }
        };
        // 3. The desired rows. A failed read ends the pass: nothing is created or deleted.
        let desired = match self.store.desired_hosts() {
            Ok(rows) => rows,
            Err(e) => {
                self.failed("read desired_hosts", format!("reading desired_hosts: {e}; nothing created or deleted this pass"));
                return false;
            }
        };
        health.fleet_seen(desired.len() as u64, objects.deployments.len() as u64);
        let stopped = coordinator.replicas == 0;
        health.set_waiting(stopped.then_some(STOPPED));
        if self.stopped != Some(stopped) {
            if stopped {
                log::info!("[fleet] the {COORDINATOR_DEPLOYMENT} Deployment is at 0 replicas: creating and re-templating nothing");
            } else {
                log::info!("[fleet] the {COORDINATOR_DEPLOYMENT} Deployment is at {} replicas: new hosts run {}", coordinator.replicas, coordinator.image);
            }
            self.stopped = Some(stopped);
        }
        // 4. The plan.
        let plan = plan(&desired, &objects, &coordinator, &template);
        let mut ok = true;
        for (row, why) in plan.refused {
            ok = false;
            self.failed(&format!("row {row}"), format!("host {row}: {why}; nothing created for it"));
        }
        // 5. The actions, in order; a failure skips the rest of that host's.
        let mut failed: BTreeSet<String> = BTreeSet::new();
        for action in &plan.actions {
            if failed.contains(action.object()) {
                continue;
            }
            let applied = self.runtime.block_on(apply(&self.kube, &self.config.namespace, action));
            health.loop_beat();
            let what = format!("{} {}", action.kind().manifest_kind(), action.object());
            match applied {
                Ok(Applied::Created) => {
                    health.fleet_created();
                    log::info!("[fleet] created {what}");
                }
                Ok(Applied::Existed) => {}
                Ok(Applied::Deleted) => {
                    health.fleet_deleted();
                    log::info!("[fleet] deleted {what}");
                }
                Ok(Applied::Replaced) => log::info!("[fleet] re-templated {what} (at 0 replicas)"),
                Err(e) => {
                    ok = false;
                    failed.insert(action.object().to_string());
                    let verb = match action {
                        Action::Delete { .. } => "deleting",
                        Action::Create { .. } => "creating",
                        Action::Replace { .. } => "re-templating",
                    };
                    self.failed(&what, format!("{verb} {what}: {e}; retried next pass"));
                }
            }
        }
        ok
    }

    /// Counts a failure and logs it, at most once per [`FLEET_LOG_EVERY_MS`] per `key`.
    fn failed(&mut self, key: &str, message: String) {
        Health::global().fleet_error();
        let every = Duration::from_millis(FLEET_LOG_EVERY_MS);
        self.logged.retain(|_, at| at.elapsed() < every);
        if !self.logged.contains_key(key) {
            self.logged.insert(key.to_string(), Instant::now());
            log::warn!("[fleet] {message}");
        }
    }
}

/// Steps 2 and 3 of a pass: the coordinator Deployment, the template, the labelled objects. The
/// loop beats after each request (each bounded by [`kube_api::REQUEST_TIMEOUT`], below
/// [`crate::health::LIVEZ_STALL_MS`]): a slow API server is not a stalled loop.
async fn read_cluster(kube: &Kube, config: &FleetConfig) -> Result<(CoordinatorSeen, Template, Objects), String> {
    let ns = &config.namespace;
    let beat = || Health::global().loop_beat();
    let coordinator = kube
        .get(&format!("{}/{COORDINATOR_DEPLOYMENT}", Kind::Deployment.collection(ns)))
        .await
        .map_err(|e| format!("reading the {COORDINATOR_DEPLOYMENT} Deployment: {e}"))?;
    beat();
    let coordinator = CoordinatorSeen::from_value(&coordinator)?;
    let map = kube
        .get(&format!("/api/v1/namespaces/{ns}/configmaps/{TEMPLATE_CONFIGMAP}"))
        .await
        .map_err(|e| format!("reading the ConfigMap {TEMPLATE_CONFIGMAP}: {e}"))?;
    beat();
    let text = map["data"][TEMPLATE_KEY]
        .as_str()
        .ok_or_else(|| format!("the ConfigMap {TEMPLATE_CONFIGMAP} has no key {TEMPLATE_KEY}"))?;
    let template = Template::new(text.to_string(), &config.world, config.port_base);
    let selector = format!("{FLEET_LABEL}={}", config.world);
    let mut objects = Objects::default();
    for kind in Kind::CREATE_ORDER {
        let items = kube
            .list(&kind.collection(ns), &selector)
            .await
            .map_err(|e| format!("listing {}s labelled {selector}: {e}", kind.manifest_kind()))?;
        beat();
        let objs: Vec<Object> = items.iter().map(Object::from_value).collect();
        match kind {
            Kind::Service => objects.services = objs,
            Kind::Pdb => objects.pdbs = objs,
            Kind::Deployment => objects.deployments = objs,
        }
    }
    Ok((coordinator, template, objects))
}

/// One action against the API server.
async fn apply(kube: &Kube, ns: &str, action: &Action) -> Result<Applied, KubeError> {
    match action {
        Action::Create { kind, body, .. } => match kube.post(&kind.collection(ns), body).await {
            Ok(_) => Ok(Applied::Created),
            Err(KubeError::Conflict) => Ok(Applied::Existed),
            Err(e) => Err(e),
        },
        // Background: the Deployment's ReplicaSets and pods go after it (each pod: SIGTERM → its leave cut).
        Action::Delete { kind, name } => {
            kube.delete(&format!("{}/{name}?propagationPolicy=Background", kind.collection(ns))).await.map(|()| Applied::Deleted)
        }
        Action::Replace { name, body } => {
            kube.put(&format!("{}/{name}", Kind::Deployment.collection(ns)), body).await.map(|_| Applied::Replaced)
        }
    }
}
