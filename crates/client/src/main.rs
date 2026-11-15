//! Game client
// #![deny(missing_docs)]
use bevy::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use client::renderer;
use client::world_scale::WorldScale;
#[cfg(not(target_arch = "wasm32"))]
use client::instance::{start_game_thread, Command};

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

/// A Web Worker's entry (`web_worker`): the worker's bootstrap script calls it
/// once the module is instantiated. In the binary crate so wasm-bindgen
/// exports it from the bin's wasm.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn orion_worker_main(role: String, base: String) {
    client::web_worker::worker_main(&role, base);
}

/// `VOXEL_VSYNC=on|1|true`: vsync (`AutoVsync`); `immediate` / `mailbox` /
/// `fifo`: that present mode exactly (fails at startup if the surface lacks
/// it); unset or anything else: `AutoNoVsync`.
#[cfg(not(target_arch = "wasm32"))]
fn present_mode_from_env() -> bevy::window::PresentMode {
    use bevy::window::PresentMode;
    match std::env::var("VOXEL_VSYNC").unwrap_or_default().to_ascii_lowercase().as_str() {
        "on" | "1" | "true" => PresentMode::AutoVsync,
        "immediate" => PresentMode::Immediate,
        "mailbox" => PresentMode::Mailbox,
        "fifo" => PresentMode::Fifo,
        _ => PresentMode::AutoNoVsync,
    }
}

fn main() {
    // Before any thread: it may set WGPU_ADAPTER_NAME (never the slow NVK GPU
    // by accident). `RENDER_BENCH` and probes pin theirs and are left alone.
    #[cfg(not(target_arch = "wasm32"))]
    client::adapter_pick::pin_adapter();
    // In a Web Worker the module is instantiated for its exports
    // (`orion_worker_main`), not to run the app.
    #[cfg(target_arch = "wasm32")]
    if client::web_worker::in_worker() {
        return;
    }
    // First, so every panic reaches the browser console with its message:
    // without the hook it is only "RuntimeError: unreachable" (e.g. a bad
    // `?world=` value in `WorldScale::from_env` below).
    #[cfg(target_arch = "wasm32")]
    console_error_panic_hook::set_once();

    game::perf::pin_origin();
    // Per-entry undo verification is Full by default (HASH_MODE=delta|off
    // trades it for speed). Client/server desync is checked separately by the
    // server's every-N-ticks state hash, `ServerPacket::HashedGameEvent`:
    // every 100 ticks unless the SERVER runs with VOXEL_STATE_HASH_INTERVAL=N — a
    // spawned single-player server inherits this process's env.
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

    // World-size knobs (VOXEL_WORLD / VOXEL_NEAR_RADIUS / VOXEL_LOD_RINGS,
    // `?world=` in the browser): resolved once here so a typo fails before
    // any window or socket opens. The game thread subscribes by it; the
    // render benchmark gates on it.
    let world_scale = WorldScale::from_env().unwrap_or_else(|error| panic!("{error}"));
    // Haze + camera far plane: follow the ring table's reach
    // (VOXEL_VIEW_DISTANCE_M); VOXEL_FOG_VISIBILITY_M sets the haze alone.
    let view_distance = client::renderer::atmosphere::ViewDistance::from_env(&world_scale)
        .unwrap_or_else(|error| panic!("{error}"));

    // Shared debug toggles: render thread (F3 dev panel) writes, game thread reads.
    let debug_flags = game::DebugFlags::default();

    // `client debug <preset> [--reset]` (e.g. `just run` = `debug embarked`)
    // skips the manual setup; a plain `client` opens on the main menu. Parsed
    // first, so a bad command line exits before any window opens.
    #[cfg(not(target_arch = "wasm32"))]
    let debug_scenario = {
        let args: Vec<String> = std::env::args().skip(1).collect();
        client::debug_launch::parse_args(&args)
            .and_then(|scenario| scenario.map(client::debug_launch::DebugLaunchPlugin::from_env).transpose())
            .unwrap_or_else(|message| {
                eprintln!("{message}");
                std::process::exit(2);
            })
    };
    #[cfg(target_arch = "wasm32")]
    let debug_scenario: Option<()> = None;

    // The dev bypass (`VOXEL_SKIP_MENU=1`, a set `VOXEL_WORLD_FILE`,
    // `RENDER_BENCH`, `VOXEL_AUTOPILOT`)
    // connects to 127.0.0.1:6466 at startup, as before the menu existed.
    // Otherwise the app starts with dead channels and every world session
    // gets fresh ones (`session::SessionFlowPlugin`). A debug launch starts
    // its own world, so it wins over the bypass.
    let skip_menu = debug_scenario.is_none() && client::session::skip_menu();

    #[cfg(not(target_arch = "wasm32"))]
    let (command_send, game_send, client_recv, request_send) = {
        let command_send = start_game_thread();
        let channels = client::session::SessionChannels::new();
        let app_ends = (channels.game_send.clone(), channels.client_recv.clone(), channels.request_send.clone());
        if skip_menu {
            let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 6466));
            command_send.send(channels.connect(addr, debug_flags.clone(), world_scale, None, None)).unwrap();
        }
        let (game_send, client_recv, request_send) = app_ends;
        (command_send, game_send, client_recv, request_send)
    };

    // The browser opens on the main menu with channels to nowhere; Join server
    // gives each session fresh ones (`session::web_flow`).
    #[cfg(target_arch = "wasm32")]
    let (game_send, client_recv, request_send) = {
        let channels = client::session::SessionChannels::new();
        (channels.game_send.clone(), channels.client_recv.clone(), channels.request_send.clone())
    };

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
        title: "Orion".into(),
        ..Default::default()
    };
    // Native presents without vsync by default, so the frame rate shows the
    // real per-frame cost (a vsync'd frame sits at the refresh rate however much
    // headroom it has); `VOXEL_VSYNC=on` brings vsync back. The browser paces
    // its own frames.
    #[cfg(not(target_arch = "wasm32"))]
    {
        primary_window.present_mode = present_mode_from_env();
    }
    // `VOXEL_WINDOW=WxH`: the window's physical size (perf A/B: a quarter of
    // the pixels tells a fragment-bound pass from a geometry-bound one).
    #[cfg(not(target_arch = "wasm32"))]
    if let Some((w, h)) = std::env::var("VOXEL_WINDOW")
        .ok()
        .and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?))))
    {
        primary_window.resolution = bevy::window::WindowResolution::new(w, h).with_scale_factor_override(1.0);
    }
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
    .insert_resource(world_scale)
    .insert_resource(view_distance);
    // App states + the session reset registry + the frontend camera. The menu
    // is the default; the dev bypass starts straight in the world; the browser
    // with a login service opens on the login screen (else its join-only menu).
    let login_first = cfg!(target_arch = "wasm32") && client::login::login_url().is_some();
    app.add_plugins(client::session::SessionPlugin { skip_menu, login_first });
    // The menu screens: on every build, since a bypass session can still end
    // up in the menu (leave, lost server). `skip_menu` only picks the initial
    // state; the browser's menu only joins a server (`frontend::JOIN_ONLY`).
    app.add_plugins(client::frontend::FrontendPlugin);
    // The logger exists from here on: announce the perf timeline and flush the
    // events the game/netcode threads emitted while Bevy was still starting.
    game::perf::init("client");
    // Repeated here because the game thread's own `world scale` line is logged
    // before the logger exists; capture manifests read this one.
    info!("[client] world scale: {world_scale}");
    info!(
        "[client] view distance: haze visibility {:.0} m, far plane {:.0} m, lod reach {:.0} m",
        view_distance.visibility / game::movement::UNITS_PER_METER,
        view_distance.far / game::movement::UNITS_PER_METER,
        world_scale.lod_reach_units() / game::movement::UNITS_PER_METER,
    );
    app.add_plugins(renderer::SimBridgePlugin {
        client_recv,
        game_send: game_send.clone(),
        requests: request_send,
        debug_flags: debug_flags.clone(),
        camera_mode: renderer::commander::CameraMode::from_env(),
    });
    // Starting / leaving world sessions: the local server per world, fresh
    // channels per session, back to the menu in-process.
    #[cfg(not(target_arch = "wasm32"))]
    app.insert_resource(client::session::GameThread(command_send.clone()))
        .add_plugins(client::session::SessionFlowPlugin);
    #[cfg(feature = "dev_ui")]
    app.add_plugins(renderer::dev_ui::DevUiPlugin);
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(launch) = debug_scenario {
        info!("[client] debug launch: {:?}", launch.scenario);
        app.add_plugins(launch);
    }

    // Joining and leaving servers from the browser's menu.
    #[cfg(target_arch = "wasm32")]
    app.insert_resource(client::session::SimConfig { world_scale })
        .add_plugins(client::session::SessionFlowPlugin);

    let app_exit = app.run();

    #[cfg(all(feature = "pyroscope", not(target_arch = "wasm32")))]
    if let Some(agent) = agent_running {
        let agent_ready = agent.stop().unwrap();
        agent_ready.shutdown();
    }

    // Window closed: shut the sim and game threads down. On native the
    // session was already left on `AppExit` (`SessionFlowPlugin`); wait for
    // its local server to stop so no child outlives the client.
    #[cfg(target_arch = "wasm32")]
    drop(game_send);
    #[cfg(not(target_arch = "wasm32"))]
    {
        client::session::join_server_stops();
        let _ = command_send.send(Command::Quit);
    }

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
