//! The panel's pages: axum handlers, askama templates (`templates/`), htmx for the
//! polling fragments and the actions.
//!
//! The panel has no login of its own: it is reached by port-forward only (no
//! Ingress). What it guards against is another web page driving it through the
//! operator's browser: every POST must carry htmx's `HX-Request: true` header (a
//! cross-site form cannot set it without a CORS preflight the panel never answers)
//! and, when the browser sends `Origin`, an origin naming the request's own `Host`.

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Form, Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};

use crate::forge::{self, ForgeForm};
use crate::ops::{OpSnapshot, Ops};
use crate::world::{self, WorldState, WorldView};

const HTMX: &str = include_str!("../assets/htmx.min.js");

#[derive(Clone)]
pub struct AppState {
    pub ops: Arc<Ops>,
    /// Grafana's base URL for the `world` dashboard link (`PANEL_GRAFANA_URL`), if any.
    pub grafana: Option<String>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/fragments/worlds", get(worlds_fragment))
        .route("/worlds/{ns}", get(world_page))
        .route("/fragments/worlds/{ns}", get(world_fragment))
        .route("/worlds/{ns}/start", post(start))
        .route("/worlds/{ns}/stop", post(stop))
        .route("/worlds/{ns}/forge", post(forge_world))
        .route("/worlds/{ns}/jobs/{job}/log", get(job_log))
        .route("/assets/htmx.min.js", get(htmx))
        .route("/healthz", get(|| async { "ok" }))
        .layer(middleware::from_fn(same_origin_posts))
        .with_state(state)
}

/// Refuses a POST without `HX-Request: true`, or whose `Origin` is another host.
async fn same_origin_posts(req: Request, next: Next) -> Response {
    if req.method() == axum::http::Method::POST
        && let Err(why) = post_allowed(req.headers())
    {
        log::warn!("[panel] refused a POST to {}: {why}", req.uri().path());
        return (StatusCode::FORBIDDEN, why).into_response();
    }
    next.run(req).await
}

pub fn post_allowed(h: &HeaderMap) -> Result<(), &'static str> {
    let hx = h.get("hx-request").and_then(|v| v.to_str().ok());
    if hx != Some("true") {
        return Err("POSTs come from the panel's own page only (HX-Request missing)");
    }
    if let Some(origin) = h.get(header::ORIGIN) {
        let origin = origin.to_str().map_err(|_| "bad Origin")?;
        let host = h
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .ok_or("no Host")?;
        let origin_host = origin
            .split_once("://")
            .map_or(origin, |(_, rest)| rest)
            .trim_end_matches('/');
        if origin_host != host {
            return Err("cross-origin POST");
        }
    }
    Ok(())
}

async fn htmx() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        HTMX,
    )
}

fn render(t: &impl Template) -> Response {
    match t.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("template: {e}")).into_response(),
    }
}

fn notice(kind: &'static str, text: impl Into<String>) -> Response {
    render(&Notice {
        kind,
        text: text.into(),
    })
}

/// The world in namespace `ns`, or an error response (bad name, not a world, no access).
#[allow(clippy::result_large_err)] // the error IS the response; one per request
async fn world_id(s: &AppState, ns: &str) -> Result<String, Response> {
    if !world::valid_name(ns) {
        return Err((StatusCode::BAD_REQUEST, "not a namespace name").into_response());
    }
    match world::world_namespace(s.ops.kube(), ns).await {
        Ok(Some(id)) => Ok(id),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            format!("{ns} is not a world namespace"),
        )
            .into_response()),
        Err(e) => Err((
            StatusCode::BAD_GATEWAY,
            format!("reading namespace {ns}: {e}"),
        )
            .into_response()),
    }
}

// ---- pages -------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "index.html")]
struct Index {}

async fn index() -> Response {
    render(&Index {})
}

#[derive(Template)]
#[template(path = "worlds.html")]
struct Worlds {
    error: Option<String>,
    rows: Vec<WorldRow>,
}

struct WorldRow {
    id: String,
    namespace: String,
    state: &'static str,
    state_class: &'static str,
    summary: String,
    revision: String,
    generation: String,
    last_cut: String,
    operation: String,
}

async fn worlds_fragment(State(s): State<AppState>) -> Response {
    let kube = s.ops.kube();
    let list = match world::list_worlds(kube).await {
        Ok(l) => l,
        Err(e) => {
            return render(&Worlds {
                error: Some(format!("listing world namespaces: {e}")),
                rows: Vec::new(),
            });
        }
    };
    let mut rows = Vec::new();
    for (id, ns) in list {
        let w = world::read_world(kube, &id, &ns, true).await;
        let m = w.coordinator_metrics();
        rows.push(WorldRow {
            state: w.state_label(),
            state_class: state_class(w.state()),
            summary: w.summary_line(),
            revision: revision(&w),
            generation: m
                .and_then(|m| m.generation)
                .map_or("—".into(), |g| g.to_string()),
            last_cut: m.and_then(|m| m.last_cut_s).map_or("—".into(), cut_age),
            operation: s.ops.current(&ns).map_or(String::new(), |o| {
                format!("{} {} ({} ago)", o.kind, o.status(), o.age())
            }),
            id,
            namespace: ns,
        });
    }
    render(&Worlds { error: None, rows })
}

#[derive(Template)]
#[template(path = "world.html")]
struct WorldPage {
    id: String,
    namespace: String,
    presets: &'static [&'static str],
    sizes: &'static [&'static str],
    default_memory: u32,
    grafana: Option<String>,
}

async fn world_page(State(s): State<AppState>, Path(ns): Path<String>) -> Response {
    let id = match world_id(&s, &ns).await {
        Ok(id) => id,
        Err(r) => return r,
    };
    let grafana = s.grafana.as_ref().map(|g| {
        format!(
            "{}/d/world/world?var-namespace={ns}",
            g.trim_end_matches('/')
        )
    });
    render(&WorldPage {
        id,
        namespace: ns,
        presets: forge::PRESETS,
        sizes: forge::SIZES,
        default_memory: forge::DEFAULT_MEMORY_MI,
        grafana,
    })
}

#[derive(Template)]
#[template(path = "world_status.html")]
struct WorldStatus {
    namespace: String,
    state: &'static str,
    state_class: &'static str,
    summary: String,
    server_image: String,
    facts: Vec<(&'static str, String)>,
    op: Option<OpSnapshot>,
    pods: Vec<PodRow>,
    jobs: Vec<world::JobView>,
    history: Vec<(String, String)>,
    problems: Vec<String>,
}

struct PodRow {
    name: String,
    component: String,
    phase: String,
    ready: &'static str,
    restarts: u64,
    age: String,
    readyz: String,
    loop_age: String,
    generation: String,
}

async fn world_fragment(State(s): State<AppState>, Path(ns): Path<String>) -> Response {
    let id = match world_id(&s, &ns).await {
        Ok(id) => id,
        Err(r) => return r,
    };
    let w = world::read_world(s.ops.kube(), &id, &ns, true).await;
    render(&status_of(&w, s.ops.current(&ns)))
}

fn status_of(w: &WorldView, op: Option<OpSnapshot>) -> WorldStatus {
    let m = w.coordinator_metrics();
    let opt = |v: Option<u64>| v.map_or("—".to_string(), |v| v.to_string());
    let facts = vec![
        ("world generation", opt(m.and_then(|m| m.generation))),
        ("hosts live", opt(m.and_then(|m| m.hosts_live))),
        (
            "last committed cut",
            m.and_then(|m| m.last_cut_s).map_or("—".into(), cut_age),
        ),
        (
            "cuts committed / aborted",
            format!(
                "{} / {}",
                opt(m.and_then(|m| m.cuts_committed)),
                opt(m.and_then(|m| m.cuts_aborted))
            ),
        ),
        (
            "successions open / declared",
            format!(
                "{} / {}",
                opt(m.and_then(|m| m.successions_open)),
                opt(m.and_then(|m| m.successions_declared))
            ),
        ),
        ("build", revision(w)),
    ];
    let pods = w
        .pods
        .iter()
        .map(|p| {
            let pm = p.metrics.as_ref();
            PodRow {
                name: p.name.clone(),
                component: p.component.clone(),
                phase: p.phase.clone(),
                ready: if p.ready { "yes" } else { "no" },
                restarts: p.restarts,
                age: p.age.clone(),
                readyz: p.readyz.clone().unwrap_or_default(),
                loop_age: pm
                    .and_then(|m| m.loop_age_s)
                    .map_or(String::new(), |a| format!("{a:.1}s")),
                generation: pm
                    .and_then(|m| m.generation)
                    .map_or(String::new(), |g| g.to_string()),
            }
        })
        .collect();
    WorldStatus {
        namespace: w.namespace.clone(),
        state: w.state_label(),
        state_class: state_class(w.state()),
        summary: w.summary_line(),
        server_image: w.server_image.clone().unwrap_or_else(|| "—".into()),
        facts,
        op,
        pods,
        jobs: w.jobs.clone(),
        history: w.release_history.clone(),
        problems: w.problems.clone(),
    }
}

fn state_class(s: WorldState) -> &'static str {
    match s {
        WorldState::Running => "good",
        WorldState::Stopped => "idle",
        WorldState::AwaitingForge | WorldState::Changing => "warn",
        WorldState::Incomplete => "bad",
    }
}

fn revision(w: &WorldView) -> String {
    w.pods
        .iter()
        .find_map(|p| {
            p.metrics
                .as_ref()
                .and_then(|m| m.revision.clone().map(|r| (r, m.region_blocks.clone())))
        })
        .map_or("—".into(), |(r, rb)| match rb {
            Some(rb) => format!("{r} · region {rb}"),
            None => r,
        })
}

fn cut_age(ts: u64) -> String {
    if ts == 0 {
        "none yet".into()
    } else {
        format!(
            "{} ago",
            world::short_duration(world::now_s().saturating_sub(ts))
        )
    }
}

// ---- actions -----------------------------------------------------------------------

#[derive(Template)]
#[template(path = "notice.html")]
struct Notice {
    kind: &'static str,
    text: String,
}

async fn start(State(s): State<AppState>, Path(ns): Path<String>) -> Response {
    if let Err(r) = world_id(&s, &ns).await {
        return r;
    }
    match s.ops.start_world(&ns) {
        Ok(()) => notice(
            "ok",
            "Starting: coordinator first, then the hosts. The log is below.",
        ),
        Err(e) => notice("bad", e),
    }
}

async fn stop(State(s): State<AppState>, Path(ns): Path<String>) -> Response {
    if let Err(r) = world_id(&s, &ns).await {
        return r;
    }
    match s.ops.stop_world(&ns) {
        Ok(()) => notice(
            "ok",
            "Stopping: every host leaves through its leave cut, then the coordinator. The log is below.",
        ),
        Err(e) => notice("bad", e),
    }
}

async fn forge_world(
    State(s): State<AppState>,
    Path(ns): Path<String>,
    Form(form): Form<ForgeForm>,
) -> Response {
    let id = match world_id(&s, &ns).await {
        Ok(id) => id,
        Err(r) => return r,
    };
    let req = match form.parse() {
        Ok(r) => r,
        Err(e) => return notice("bad", e),
    };
    match s.ops.forge(&id, &ns, &req).await {
        Ok(job) => notice(
            "ok",
            format!(
                "Forge Job {job} created. It shows under Forge jobs; the coordinator and hosts turn Ready when it is done."
            ),
        ),
        Err(e) => notice("bad", e),
    }
}

async fn job_log(State(s): State<AppState>, Path((ns, job)): Path<(String, String)>) -> Response {
    if let Err(r) = world_id(&s, &ns).await {
        return r;
    }
    if !world::valid_name(&job) || !job.starts_with("forge-") {
        return (StatusCode::BAD_REQUEST, "not a forge Job name").into_response();
    }
    match s.ops.job_log(&ns, &job).await {
        Ok(text) => ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}
