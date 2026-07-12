//! Text-dumping performance diagnostics, opt-in via the `DIAG` env var.
//!
//! Everything here exists to answer one question — *is a slow frame CPU-bound
//! or GPU-bound?* — with numbers that can be pasted into a bug report or an
//! LLM session, as opposed to Tracy/RenderDoc, which only render to a GUI.
//!
//! `DIAG=1 just client-release` (or `just diag`) adds:
//!   * `EntityCountDiagnosticsPlugin` — `entity_count`, the main-world spawn count.
//!   * `RenderDiagnosticsPlugin` — per-render-pass `elapsed_cpu`/`elapsed_gpu` ms plus
//!     pipeline statistics (`fragment_shader_invocations` = overdraw) sourced from wgpu
//!     timestamp/pipeline-statistics queries. Vulkan and DX12 only; on other backends
//!     (incl. wasm/WebGL2) the GPU columns are simply absent and CPU time still records.
//!   * `LogDiagnosticsPlugin` — dumps all of the above to stdout every `DIAG_INTERVAL`
//!     seconds (default 2), one `name: value (avg …)` line each.
//!
//! Read it as: `frame_time` is the frame budget; the sum of the `render/*/elapsed_gpu`
//! lines is what the GPU spent inside it. GPU total ≈ frame_time means GPU-bound (and
//! `fragment_shader_invocations` says whether that is overdraw); GPU total far below
//! frame_time means the cost is CPU-side, and the next step is a `trace_chrome` capture
//! (`just trace-chrome`) for per-system durations.
//!
//! The GPU queries cost a little themselves, so this stays behind an env var rather than
//! running in every build.

use bevy::app::{App, Plugin};
use bevy::diagnostic::{EntityCountDiagnosticsPlugin, LogDiagnosticsPlugin};
use core::time::Duration;

/// Seconds between stdout dumps when `DIAG_INTERVAL` is unset.
const DEFAULT_INTERVAL_SECS: f32 = 2.0;

/// Whether the `DIAG` env var opts this run into diagnostics. Any value except
/// `0`/empty counts (wasm has no env, so this is always false there).
pub fn enabled() -> bool {
    match std::env::var("DIAG") {
        Ok(v) => !v.is_empty() && v != "0",
        Err(_) => false,
    }
}

/// Frame-time, entity-count, render-pass GPU/CPU timings, logged to stdout.
///
/// Must be added *after* `DefaultPlugins`: `RenderDiagnosticsPlugin` reaches into the
/// `RenderApp` sub-app that `RenderPlugin` creates.
pub struct DiagPlugin;

impl Plugin for DiagPlugin {
    fn build(&self, app: &mut App) {
        let wait_duration = Duration::from_secs_f32(
            std::env::var("DIAG_INTERVAL")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_INTERVAL_SECS),
        );

        // `FrameTimeDiagnosticsPlugin` (fps / frame_time / frame_count) is registered
        // unconditionally by `SimBridgePlugin` for the F3 HUD, so it is not repeated
        // here — adding a plugin twice panics. Its diagnostics still get logged below.
        app.add_plugins((
            EntityCountDiagnosticsPlugin::default(),
            LogDiagnosticsPlugin {
                wait_duration,
                // No filter: the render/* diagnostic paths are only created on the first
                // frame, so we cannot name them up front — and seeing all of them is the
                // point of the dump.
                ..Default::default()
            },
        ));

        // Bevy's own `trace_tracy` feature registers this plugin itself, and adding a
        // plugin twice panics — so the `tracy` build gets its GPU timings through Tracy
        // instead.
        #[cfg(not(feature = "tracy"))]
        app.add_plugins(bevy::render::diagnostic::RenderDiagnosticsPlugin);
    }
}
