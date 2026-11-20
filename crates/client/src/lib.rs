//! Game client library: the engine-neutral, in-process pieces (netcode
//! routing, rollback client, local server, render bridge) that the binary
//! wires into a Bevy app — and that tests drive headlessly via `harness`.
// The default 128 overflows the auto-trait checks of bevy's render resources (wgpu's nesting).
#![recursion_limit = "256"]

#[cfg(not(target_arch = "wasm32"))]
pub mod adapter_pick;
#[cfg(not(target_arch = "wasm32"))]
pub mod netcode;
#[cfg(not(target_arch = "wasm32"))]
pub mod quic_verify; // which QUIC certificate the native client trusts
// Compiled unconditionally now (was cfg(any(wasm32, test))): the harness and
// integration tests need LocalServer in a normal (non-cfg(test)) lib build.
pub mod bevy_harness;
pub mod blocks; // block registry loader (assets/blocks/blocks.ron)
pub mod connections; // one connection per server host, routed by region (R3)
#[cfg(not(target_arch = "wasm32"))]
pub mod debug_launch; // `client debug <preset>`: skip the manual setup (e.g. `world`)
pub mod frontend; // FrontendUi layer: main menu + placeholder screens
pub mod graphics_settings; // the Settings window's Graphics tab: render + full-detail distance, kept between runs
pub mod harness; // SimHarness — added in Task 3 (empty stub file for now)
pub mod instance; // GameInstanceManager (moved out of main.rs)
#[cfg(not(target_arch = "wasm32"))]
pub mod local_host; // the child server a menu-started session plays on
pub mod local_server;
pub mod login; // the login service's HTTP client + refresh-token store (R2a)
#[cfg(target_arch = "wasm32")]
pub mod netcode_web;
mod perf_clock; // wasm-safe monotonic timing for client instrumentation
pub mod player_name; // the local player name, sent first on every connection to a local server
pub mod regions; // predicted regions behind a message pair: inline or one thread each
pub mod renderer;
pub mod session; // AppState, SessionScoped, session reset registry, FrontendCamera
#[cfg(target_arch = "wasm32")]
pub mod sim_driver;
pub mod web_endpoint; // which WebTransport server the browser build dials
pub mod web_frame; // page → sim worker framing: a server frame tagged with its connection
#[cfg(target_arch = "wasm32")]
pub mod web_worker; // Web Workers: the sim worker + the compute pool (browser multithreading)
pub mod world_scale; // near window + LOD ring table knobs (VOXEL_WORLD & co.)
pub mod wrap_frame; // the unwrapped view of a world that wraps east–west: canonical regions ↔ the render frame // BevyInputHarness — headless Bevy-layer input harness

pub use instance::GameInstanceManager;
pub use local_server::{LOCAL_CLIENT_ID, LocalServer};
