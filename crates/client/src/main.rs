//! Game client
// #![deny(missing_docs)]
use bevy::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use crossbeam::channel::{Receiver, Sender};
#[cfg(not(target_arch = "wasm32"))]
use game::ClientUpdateEvent;
use game::GameEventKind;
#[cfg(not(target_arch = "wasm32"))]
use log::trace;
#[cfg(not(target_arch = "wasm32"))]
use log::warn;
#[cfg(not(target_arch = "wasm32"))]
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use client::renderer;
#[cfg(target_arch = "wasm32")]
use client::sim_driver;
use client::world_scale::WorldScale;
#[cfg(not(target_arch = "wasm32"))]
use client::GameInstanceManager;

/// Share of total cores given to the AsyncCompute task pool, which runs the
/// background chunk-meshing jobs (`renderer/meshing.rs`). Normal clients take
/// half the cores; render benchmarks set `VOXEL_CLIENT_WORKER_THREADS` so the
/// load phase cannot monopolize a development machine.
const ASYNC_COMPUTE_PERCENT: f32 = 0.5;
const ASYNC_COMPUTE_MIN_THREADS: usize = 1;
const ASYNC_COMPUTE_MAX_THREADS: usize = usize::MAX;
/// Bounds on the default Bevy thread cap: half the logical cores, clamped.
/// The floor is the old fixed default; the ceiling keeps a many-core box from
/// being monopolized. 8 threads (vs 4) cut `full` join time 19% (2026-09-22
/// capture) — the join's snapshot-voxel derivation and meshing share the
/// async pool, so they were paced by it. Tunable.
#[cfg(not(target_arch = "wasm32"))]
const MIN_CLIENT_WORKER_THREADS: usize = 4;
#[cfg(not(target_arch = "wasm32"))]
const MAX_CLIENT_WORKER_THREADS: usize = 8;

/// The default cap for a machine with `cores` logical cores.
#[cfg(not(target_arch = "wasm32"))]
fn default_client_worker_threads(cores: usize) -> usize {
    (cores / 2).clamp(MIN_CLIENT_WORKER_THREADS, MAX_CLIENT_WORKER_THREADS)
}

#[cfg(not(target_arch = "wasm32"))]
fn client_worker_cap(value: Option<&str>) -> Result<usize, String> {
    let Some(value) = value else {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        return Ok(default_client_worker_threads(cores));
    };
    let cap = value.parse::<usize>().map_err(|_| {
        format!("invalid VOXEL_CLIENT_WORKER_THREADS value `{value}` (expected integer >= 4)")
    })?;
    if cap < 4 {
        return Err(format!(
            "invalid VOXEL_CLIENT_WORKER_THREADS value `{value}` (expected integer >= 4)"
        ));
    }
    Ok(cap)
}

#[cfg(not(target_arch = "wasm32"))]
fn client_worker_cap_from_env() -> usize {
    let value = std::env::var("VOXEL_CLIENT_WORKER_THREADS").ok();
    client_worker_cap(value.as_deref()).unwrap_or_else(|message| panic!("{message}"))
}

fn task_pool_options(worker_cap: Option<usize>) -> bevy::app::TaskPoolOptions {
    let mut options = bevy::app::TaskPoolOptions {
        async_compute: bevy::app::TaskPoolThreadAssignmentPolicy {
            min_threads: ASYNC_COMPUTE_MIN_THREADS,
            max_threads: ASYNC_COMPUTE_MAX_THREADS,
            percent: ASYNC_COMPUTE_PERCENT,
            on_thread_spawn: None,
            on_thread_destroy: None,
        },
        ..Default::default()
    };
    if let Some(cap) = worker_cap {
        options.max_total_threads = cap;
        options.async_compute.max_threads = cap;
    }
    options
}

/// Event sent from client to game thread.
#[cfg(not(target_arch = "wasm32"))]
pub enum Command {
    /// Connect to a server, sync and start running game sim.
    ConnectToServerAndScene(
        Sender<GameEventKind>,
        Receiver<GameEventKind>,
        Sender<ClientUpdateEvent>,
        SocketAddr,
        game::DebugFlags,
        WorldScale,
    ),
    /// Quit the game thread. Should only be send when quitting the application.
    Quit,
}

#[cfg(not(target_arch = "wasm32"))]
fn start_game_thread() -> Sender<Command> {
    let (command_send, command_recv) = crossbeam::channel::unbounded();
    std::thread::spawn(move || loop {
        match command_recv.recv() {
            Ok(command) => match command {
                Command::ConnectToServerAndScene(
                    sender,
                    receiver,
                    client_sender,
                    server,
                    debug_flags,
                    world_scale,
                ) => {
                    let mut manager = GameInstanceManager::new(
                        sender,
                        receiver,
                        client_sender,
                        server,
                        debug_flags,
                    );
                    manager.set_world_scale(world_scale);
                    if let Err(e) = manager.connect_and_run() {
                        warn!("Game Crashed: {:?}", e);
                    };
                }
                Command::Quit => {
                    trace!("Game thread recieved quit command.");
                    break;
                }
            },
            Err(_e) => {
                warn!(
                    "Game thread stoped receiving command events, stopping game thread. Client probably crashed or was closed incorrectly."
                );
                break;
            }
        }
    });
    return command_send;
}

/// `LogPlugin` filter. `RUST_LOG`, when set, replaces it wholesale (Bevy's
/// rule), so `PERF_LOG=1` is the supported way to see the DEBUG-level
/// `[client-budget]` / `[client] mispredicts/s` lines: it raises only their
/// dedicated target, not every `debug!` in the workspace.
fn log_filter(budget_lines: bool) -> String {
    let mut filter = String::from("wgpu=error,naga=warn");
    if budget_lines {
        filter.push_str(&format!(",{}=debug", game::perf::BUDGET_TARGET));
    }
    filter
}

#[cfg(not(target_arch = "wasm32"))]
fn app_exit_code(exit: &bevy::app::AppExit) -> i32 {
    match exit {
        bevy::app::AppExit::Success => 0,
        bevy::app::AppExit::Error(code) => i32::from(code.get()),
    }
}

fn main() {
    game::perf::pin_origin();
    // Per-entry verification is off by default (desync is caught by the
    // periodic tick-interval state hash); HASH_MODE=delta re-enables
    // footprint-digest self-checks, HASH_MODE=full whole-field verification
    // when chasing a footprint/capture gap. Tests and the convergence
    // harnesses pin HashMode::Full themselves.
    game::set_hash_mode_from_env();

    // Continuous profiling: build with `--features pyroscope` and set PYROSCOPE
    // to push flamegraphs to a local Pyroscope (`just profile-client`). Off
    // unless both the feature and the env var are present, so release runs are
    // never instrumented.
    #[cfg(all(feature = "pyroscope", not(target_arch = "wasm32")))]
    let agent_running = if std::env::var("PYROSCOPE").is_ok() {
        use pyroscope::PyroscopeAgent;
        use pyroscope_pprofrs::{pprof_backend, PprofConfig};
        let agent = PyroscopeAgent::builder("http://localhost:4040", "client")
            .backend(pprof_backend(PprofConfig::new().sample_rate(100)))
            .build()
            .unwrap();
        Some(agent.start().unwrap())
    } else {
        None
    };

    // World-size knobs (VOXEL_WORLD / VOXEL_NEAR_RADIUS / VOXEL_LOD_RINGS):
    // resolved once here so a typo fails before any window or socket opens.
    // The game thread subscribes by it; the render benchmark gates on it.
    let world_scale = WorldScale::from_env().unwrap_or_else(|error| panic!("{error}"));

    // Shared debug toggles: render thread (F3 dev panel) writes, game thread reads.
    let debug_flags = game::DebugFlags::default();

    #[cfg(target_arch = "wasm32")]
    console_error_panic_hook::set_once();

    #[cfg(not(target_arch = "wasm32"))]
    let (command_send, game_send, client_recv) = {
        let command_send = start_game_thread();
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded();
        command_send
            .send(Command::ConnectToServerAndScene(
                game_send.clone(),
                game_recv,
                client_send,
                SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 6466)),
                debug_flags.clone(),
                world_scale,
            ))
            .unwrap();
        (command_send, game_send, client_recv)
    };

    #[cfg(target_arch = "wasm32")]
    let (sim, game_send, client_recv) = sim_driver::start_wasm_sim(debug_flags.clone());

    // Bevy's default asset root is CARGO_MANIFEST_DIR/assets (crates/client/assets,
    // a local, gitignored scratch dir left over from earlier prototyping). The
    // workspace's actual tracked asset tree (assets/blocks, assets/shaders/...) lives
    // two levels up at the repo root, so point the file-asset source there instead.
    // On wasm, assets are fetched over HTTP relative to the served root, and
    // wasm-server-runner serves ./assets from the working directory.
    #[cfg(not(target_arch = "wasm32"))]
    let asset_path = "../../assets";
    #[cfg(target_arch = "wasm32")]
    let asset_path = "assets";

    #[allow(unused_mut)]
    let mut primary_window = bevy::window::Window {
        title: "Labour of Love".into(),
        ..Default::default()
    };
    #[cfg(not(target_arch = "wasm32"))]
    if renderer::render_bench::enabled() {
        primary_window.resolution = bevy::window::WindowResolution::new(
            renderer::render_bench::BENCHMARK_WIDTH,
            renderer::render_bench::BENCHMARK_HEIGHT,
        )
        .with_scale_factor_override(1.0);
        primary_window.present_mode = bevy::window::PresentMode::AutoNoVsync;
    }
    // Track the canvas's parent (the full-viewport <body> in index.html) so the
    // render buffer resizes with the browser window instead of CSS-stretching.
    #[cfg(target_arch = "wasm32")]
    {
        primary_window.fit_canvas_to_parent = true;
    }

    let mut app = App::new();
    #[cfg(not(target_arch = "wasm32"))]
    let task_pool_options = task_pool_options(Some(client_worker_cap_from_env()));
    #[cfg(target_arch = "wasm32")]
    let task_pool_options = task_pool_options(None);
    app.add_plugins(
        DefaultPlugins
            .set(bevy::window::WindowPlugin {
                primary_window: Some(primary_window),
                ..Default::default()
            })
            .set(bevy::log::LogPlugin {
                filter: log_filter(game::perf::budget_lines_requested()),
                ..Default::default()
            })
            .set(bevy::asset::AssetPlugin {
                file_path: asset_path.into(),
                ..Default::default()
            })
            .set(bevy::app::TaskPoolPlugin { task_pool_options })
            .set(renderer::render_plugin()),
    )
    .insert_resource(world_scale);
    // The logger exists from here on: announce the perf timeline and flush the
    // events the game/netcode threads emitted while Bevy was still starting.
    game::perf::init("client");
    // Repeated here because the game thread's own `world scale` line is logged
    // before the logger exists; capture manifests read this one.
    info!("[client] world scale: {world_scale}");
    app.add_plugins(renderer::SimBridgePlugin {
        client_recv,
        game_send: game_send.clone(),
        debug_flags: debug_flags.clone(),
    });
    #[cfg(feature = "dev_ui")]
    app.add_plugins(renderer::dev_ui::DevUiPlugin);

    #[cfg(target_arch = "wasm32")]
    app.insert_resource(sim)
        .add_systems(Update, sim_driver::drive_sim);

    let app_exit = app.run();

    #[cfg(all(feature = "pyroscope", not(target_arch = "wasm32")))]
    if let Some(agent) = agent_running {
        let agent_ready = agent.stop().unwrap();
        agent_ready.shutdown();
    }

    // Window closed: shut the sim and game threads down.
    let _ = game_send.send(GameEventKind::Quit);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = command_send.send(Command::Quit);

    #[cfg(not(target_arch = "wasm32"))]
    {
        let exit_code = app_exit_code(&app_exit);
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = app_exit;
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn perf_log_raises_only_the_budget_target() {
        assert_eq!(log_filter(false), "wgpu=error,naga=warn");
        assert_eq!(log_filter(true), "wgpu=error,naga=warn,perf_budget=debug");
    }

    #[test]
    fn client_worker_cap_accepts_valid_override() {
        assert_eq!(client_worker_cap(Some("6")), Ok(6));
    }

    #[test]
    fn client_worker_cap_rejects_invalid_values() {
        let malformed = client_worker_cap(Some("many")).unwrap_err();
        assert!(malformed.contains("VOXEL_CLIENT_WORKER_THREADS"));
        assert!(malformed.contains("integer >= 4"));

        let undersized = client_worker_cap(Some("3")).unwrap_err();
        assert!(undersized.contains("VOXEL_CLIENT_WORKER_THREADS"));
        assert!(undersized.contains("integer >= 4"));
    }

    #[test]
    fn client_worker_cap_limits_combined_bevy_pools() {
        let options = task_pool_options(Some(client_worker_cap(Some("6")).unwrap()));
        assert_eq!(options.max_total_threads, 6);
        assert_eq!(options.async_compute.max_threads, 6);
        assert_eq!(options.async_compute.percent, ASYNC_COMPUTE_PERCENT);
    }

    #[test]
    fn default_worker_cap_is_half_the_cores_clamped() {
        assert_eq!(default_client_worker_threads(1), 4, "floor");
        assert_eq!(default_client_worker_threads(8), 4);
        assert_eq!(default_client_worker_threads(12), 6);
        assert_eq!(default_client_worker_threads(20), 8, "this dev box");
        assert_eq!(default_client_worker_threads(64), 8, "ceiling");
        let unset = client_worker_cap(None).unwrap();
        assert!((MIN_CLIENT_WORKER_THREADS..=MAX_CLIENT_WORKER_THREADS).contains(&unset));
    }

    #[test]
    fn bevy_app_errors_propagate_to_the_process_status() {
        assert_eq!(app_exit_code(&bevy::app::AppExit::Success), 0);
        assert_eq!(app_exit_code(&bevy::app::AppExit::error()), 1);
        assert_eq!(app_exit_code(&bevy::app::AppExit::from_code(23)), 23);
    }
}
