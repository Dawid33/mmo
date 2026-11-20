//! The world map's tile route (plan `2026-11-20-block-level-world-map`, Task 11; spec §5.2):
//! `GET /v1/map/tiles/{level}/{x}/{z}` around `map_tiles::TileSource::tile`, mounted in a route group
//! of its own on the coordinator's HTTPS listener (NOT behind its service-token guard) and on a plain
//! HTTP/1.1 listener at [`MAP_BIND_ENV`]: in a coordinator pod the cluster-internal port the `web`
//! proxy forwards `/map/<world>/v1/map/tiles/` to, rate-limited (nginx cannot speak the HTTPS API's
//! HTTP/2 upstream); for the bare dev server (role `all`) loopback only.
//!
//! Unauthenticated on purpose (owner, 2026-11-20): the surface is what LOD already shows every player;
//! abuse is bounded by the proxy's per-IP rate, by the build queue ([`MAP_BUILDS_QUEUED_MAX`]: then
//! 503 + `Retry-After: 1`) and by caching. Builds run on [`MAP_BUILD_WORKERS`] threads of their own,
//! never on a request's runtime. A tile that can never change (above `MAP_EDIT_MAX_LEVEL`) is
//! `immutable`; every other answer is `no-cache` (revalidated with its `ETag`).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use game::map_tile::{MapTileKey, MAP_LEVEL_LIMIT};
use map_tiles::{Fetched, TileSource};

/// Threads that build tiles.
pub const MAP_BUILD_WORKERS: usize = 2;
/// Builds waiting or running at most; one more is answered 503 + `Retry-After: 1`.
pub const MAP_BUILDS_QUEUED_MAX: usize = 64;
/// How often the source re-reads the stored regions' terrain versions.
pub const MAP_REVALIDATE_MS: u64 = 30_000;
/// The tile route's path (axum syntax).
pub const PATH_MAP_TILE: &str = "/v1/map/tiles/{level}/{x}/{z}";
/// Serve the tile route on this address too, plain HTTP/1.1, no auth: a coordinator pod's
/// cluster-internal port for the `web` proxy (`0.0.0.0:6470`), or role `all`'s loopback dev listener
/// (the `server` recipe sets `127.0.0.1:6470`).
pub const MAP_BIND_ENV: &str = "VOXEL_MAP_BIND";

/// The process's tile source, once its world is open (the coordinator and role `all` install it).
static SOURCE: OnceLock<Arc<TileSource>> = OnceLock::new();

/// Install the process's tile source: the coordinator's API mounts the route from then on, and its
/// terrain versions are re-read every [`MAP_REVALIDATE_MS`]. A second install is ignored.
pub fn install(source: TileSource) {
    let source = Arc::new(source);
    if SOURCE.set(source.clone()).is_err() {
        return;
    }
    let weak = Arc::downgrade(&source);
    let spawned = std::thread::Builder::new().name("map-revalidate".into()).spawn(move || loop {
        std::thread::sleep(Duration::from_millis(MAP_REVALIDATE_MS));
        let Some(source) = weak.upgrade() else { return };
        if let Err(e) = source.refresh_versions() {
            log::warn!("[map] re-reading terrain versions: {e}");
        }
    });
    if let Err(e) = spawned {
        log::warn!("[map] no revalidation thread ({e}): edits show only on a restart");
    }
}

/// The installed source's route group, if one is installed.
pub fn installed_router() -> Option<Router> {
    SOURCE.get().map(|s| router(s.clone()))
}

/// The materials of the block manifest, as the server resolves them (for a [`TileSource`]).
pub fn materials() -> worldgen::material::Materials {
    let manifest = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/blocks/blocks.ron"))
        .unwrap_or_else(|_| include_str!("../../../assets/blocks/blocks.ron").to_string());
    let registry = game::BlockRegistry::from_ron(&manifest).expect("invalid block manifest");
    worldgen::material::Materials::from_names(|name| registry.id_of(name).unwrap_or(game::BlockId::AIR))
}

type Reply = tokio::sync::oneshot::Sender<Result<Fetched, String>>;

struct Builds {
    jobs: crossbeam::channel::Sender<(MapTileKey, Option<u64>, Reply)>,
    queued: AtomicUsize,
    source: Arc<TileSource>,
}

/// The route group over `source` (its own state; mount it with `Router::merge`).
pub fn router(source: Arc<TileSource>) -> Router {
    let (jobs, rx) = crossbeam::channel::unbounded::<(MapTileKey, Option<u64>, Reply)>();
    let builds = Arc::new(Builds { jobs, queued: AtomicUsize::new(0), source: source.clone() });
    for i in 0..MAP_BUILD_WORKERS {
        let (rx, source, b) = (rx.clone(), source.clone(), Arc::downgrade(&builds));
        let spawned = std::thread::Builder::new().name(format!("map-build-{i}")).spawn(move || {
            for (key, have, reply) in rx {
                let started = Instant::now();
                let answer = source.tile(key, have).map_err(|e| e.to_string());
                if matches!(answer, Ok(Fetched::Tile { .. })) {
                    crate::health::Health::global().map_built(started.elapsed().as_millis() as u64, source.cache_bytes() as u64);
                }
                if let Some(b) = b.upgrade() {
                    b.queued.fetch_sub(1, Ordering::SeqCst);
                }
                let _ = reply.send(answer);
            }
        });
        if let Err(e) = spawned {
            log::error!("[map] build worker {i}: {e}");
        }
    }
    Router::new().route(PATH_MAP_TILE, get(tile)).with_state(builds)
}

/// `"<hex>"` → the etag (also a bare `<hex>`, and a weak `W/"<hex>"`).
fn parse_etag(v: &str) -> Option<u64> {
    let v = v.trim().trim_start_matches("W/").trim_matches('"');
    u64::from_str_radix(v, 16).ok()
}

fn etag_header(etag: u64) -> String {
    format!("\"{etag:016x}\"")
}

async fn tile(State(b): State<Arc<Builds>>, Path((level, x, z)): Path<(u8, i32, i32)>, headers: HeaderMap) -> Response {
    let response = answer(&b, level, x, z, &headers).await;
    crate::health::Health::global().map_request(response.status().as_u16());
    response
}

async fn answer(b: &Builds, level: u8, x: i32, z: i32, headers: &HeaderMap) -> Response {
    let key = MapTileKey { level, x, z };
    if level > MAP_LEVEL_LIMIT || !b.source.in_world(key) {
        return (StatusCode::NOT_FOUND, "no such tile").into_response();
    }
    let have = headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).and_then(parse_etag);
    if b.queued.fetch_add(1, Ordering::SeqCst) >= MAP_BUILDS_QUEUED_MAX {
        b.queued.fetch_sub(1, Ordering::SeqCst);
        return (StatusCode::SERVICE_UNAVAILABLE, [(header::RETRY_AFTER, "1")], "busy").into_response();
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    if b.jobs.send((key, have, tx)).is_err() {
        b.queued.fetch_sub(1, Ordering::SeqCst);
        return (StatusCode::SERVICE_UNAVAILABLE, [(header::RETRY_AFTER, "1")], "no builder").into_response();
    }
    let cache = if TileSource::immutable(key) { "public, max-age=31536000, immutable" } else { "no-cache" };
    match rx.await {
        Ok(Ok(Fetched::Unchanged)) => {
            let etag = have.map(etag_header).unwrap_or_default();
            (StatusCode::NOT_MODIFIED, [(header::ETAG, etag), (header::CACHE_CONTROL, cache.to_string())]).into_response()
        }
        Ok(Ok(Fetched::Tile { etag, bytes })) => (
            StatusCode::OK,
            [
                (header::ETAG, etag_header(etag)),
                (header::CACHE_CONTROL, cache.to_string()),
                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            ],
            bytes.as_ref().clone(),
        )
            .into_response(),
        Ok(Err(e)) => {
            log::warn!("[map] tile {key:?}: {e}");
            (StatusCode::SERVICE_UNAVAILABLE, [(header::RETRY_AFTER, "1")], "store").into_response()
        }
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, [(header::RETRY_AFTER, "1")], "no builder").into_response(),
    }
}

/// The installed source's route on `bind`, plain HTTP/1.1, on a thread of its own. `loopback_only`
/// (role `all`, the dev server) refuses any other address.
pub fn serve_plain(bind: SocketAddr, loopback_only: bool) -> Result<(), String> {
    if loopback_only && !bind.ip().is_loopback() {
        return Err(format!("{MAP_BIND_ENV}={bind}: the dev map listener binds loopback only"));
    }
    let Some(router) = installed_router() else { return Err("no world map source (a procedural world?)".into()) };
    let listener = std::net::TcpListener::bind(bind).map_err(|e| format!("{MAP_BIND_ENV}={bind}: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    std::thread::Builder::new()
        .name("map-http".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(r) => r,
                Err(e) => return log::error!("[map] listener runtime: {e}"),
            };
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(l) => l,
                    Err(e) => return log::error!("[map] listener: {e}"),
                };
                if let Err(e) = axum::serve(listener, router).await {
                    log::error!("[map] listener: {e}");
                }
            });
        })
        .map_err(|e| e.to_string())?;
    log::info!("[map] tile route on http://{bind}/v1/map/tiles/{{level}}/{{x}}/{{z}}");
    Ok(())
}
