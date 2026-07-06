//! Game client library: the engine-neutral, in-process pieces (netcode
//! routing, rollback client, local server, render bridge) that the binary
//! wires into a Bevy app — and that tests drive headlessly via `harness`.

#[cfg(not(target_arch = "wasm32"))]
pub mod netcode;
// Compiled unconditionally now (was cfg(any(wasm32, test))): the harness and
// integration tests need LocalServer in a normal (non-cfg(test)) lib build.
pub mod local_server;
#[cfg(target_arch = "wasm32")]
pub mod netcode_web;
#[cfg(target_arch = "wasm32")]
pub mod sim_driver;
pub mod renderer;
pub mod blocks; // block registry loader (assets/blocks/blocks.ron)
pub mod instance; // GameInstanceManager (moved out of main.rs)
pub mod harness; // SimHarness — added in Task 3 (empty stub file for now)

pub use instance::GameInstanceManager;
pub use local_server::{LocalServer, LOCAL_CLIENT_ID};
