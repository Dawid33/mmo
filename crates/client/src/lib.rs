//! Game client library: the engine-neutral, in-process pieces (netcode
//! routing, rollback client, local server, render bridge) that the binary
//! wires into a Bevy app — and that tests drive headlessly via `harness`.

#[cfg(not(target_arch = "wasm32"))]
pub mod netcode;
#[cfg(not(target_arch = "wasm32"))]
pub mod quic_verify; // which QUIC certificate the native client trusts
#[cfg(not(target_arch = "wasm32"))]
pub mod adapter_pick;
// Compiled unconditionally now (was cfg(any(wasm32, test))): the harness and
// integration tests need LocalServer in a normal (non-cfg(test)) lib build.
pub mod local_server;
pub mod login; // the login service's HTTP client + refresh-token store (R2a)
pub mod player_name; // the local player name, sent first on every connection to a local server
#[cfg(target_arch = "wasm32")]
pub mod netcode_web;
#[cfg(target_arch = "wasm32")]
pub mod sim_driver;
#[cfg(target_arch = "wasm32")]
pub mod web_worker; // Web Workers: the sim worker + the compute pool (browser multithreading)
pub mod renderer;
pub mod blocks; // block registry loader (assets/blocks/blocks.ron)
pub mod instance; // GameInstanceManager (moved out of main.rs)
pub mod connections; // one connection per server host, routed by region (R3)
pub mod wrap_frame; // the unwrapped view of a world that wraps east–west: canonical regions ↔ the render frame
pub mod regions; // predicted regions behind a message pair: inline or one thread each
pub mod frontend; // FrontendUi layer: main menu + placeholder screens
#[cfg(not(target_arch = "wasm32"))]
pub mod debug_launch; // `client debug <preset>`: skip the manual setup (e.g. `world`)
#[cfg(not(target_arch = "wasm32"))]
pub mod local_host; // the child server a menu-started session plays on
pub mod session; // AppState, SessionScoped, session reset registry, FrontendCamera
pub mod web_endpoint; // which WebTransport server the browser build dials
pub mod web_frame; // page → sim worker framing: a server frame tagged with its connection
pub mod world_scale; // near window + LOD ring table knobs (VOXEL_WORLD & co.)
pub mod harness; // SimHarness — added in Task 3 (empty stub file for now)
pub mod bevy_harness; // BevyInputHarness — headless Bevy-layer input harness

pub use instance::GameInstanceManager;
pub use local_server::{LocalServer, LOCAL_CLIENT_ID};
