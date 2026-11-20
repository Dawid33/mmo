//! `panel`: serves the world panel. Env:
//! - `PANEL_BIND` (default `127.0.0.1:8090`; the pod sets `0.0.0.0:8080`);
//! - `PANEL_KUBE_API`: a `kubectl proxy` URL (outside the cluster); unset = in-cluster
//!   ServiceAccount;
//! - `PANEL_GRAFANA_URL`: Grafana's base URL, for a link to the `world` dashboard;
//! - `WORLD_STOP_TIMEOUT` / `WORLD_START_TIMEOUT` (seconds), as the recipes'.

use std::time::Duration;

use panel::kube::Kube;
use panel::ops::{Ops, Timeouts};
use panel::web::{AppState, router};

fn secs(var: &str, default: Duration) -> Duration {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .map_or(default, Duration::from_secs)
}

#[tokio::main]
async fn main() {
    let _ = simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default());
    let kube = match Kube::from_env() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("panel: {e}");
            std::process::exit(1);
        }
    };
    let defaults = Timeouts::default();
    let timeouts = Timeouts {
        stop: secs("WORLD_STOP_TIMEOUT", defaults.stop),
        start: secs("WORLD_START_TIMEOUT", defaults.start),
        ..defaults
    };
    let state = AppState {
        ops: Ops::new(kube, timeouts),
        grafana: std::env::var("PANEL_GRAFANA_URL")
            .ok()
            .filter(|s| !s.is_empty()),
    };
    let bind = std::env::var("PANEL_BIND").unwrap_or_else(|_| "127.0.0.1:8090".into());
    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("panel: bind {bind}: {e}");
            std::process::exit(1);
        }
    };
    log::info!("[panel] serving on http://{bind}/");
    let shutdown = async {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    };
    if let Err(e) = axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
    {
        eprintln!("panel: {e}");
        std::process::exit(1);
    }
}
