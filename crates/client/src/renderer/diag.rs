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
        // unconditionally by `SimBridgePlugin` for the F3 dev panel, so it is not repeated
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
        if !app.is_plugin_added::<bevy::render::diagnostic::RenderDiagnosticsPlugin>() {
            app.add_plugins(bevy::render::diagnostic::RenderDiagnosticsPlugin);
        }

        if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
            render_app.add_systems(bevy::render::Render, log_arena_geometry);
        }
    }
}

/// Seconds between `[arena]` lines.
const ARENA_LOG_SECS: f32 = 5.0;

/// `[arena]` every [`ARENA_LOG_SECS`]: the pulled path's resident geometry —
/// live chunk slots and quads, split into sim chunks (16 m; `with_twin` =
/// those with a coarse twin, drawn only near the camera), coarse twins
/// (`meshing::coarse`) and LOD tiles — the volume every view's cull walks.
/// The LOD tiles are also split by horizontal size (a tile is 16 m × its
/// factor: `128m` = factor 8 … `8192m` = factor 512) and by face id
/// (`Face::index()` order: +X −X +Y −Y +Z −Z, then cutout) — a bottom-face
/// count near the top-face one means the far terrain's underside is being
/// meshed (`neighbors::fill_under_crust`).
fn log_arena_geometry(
    arena: Option<bevy::prelude::Res<super::voxel_pull::arena::GpuQuadArena>>,
    mut last: bevy::prelude::Local<Option<std::time::Instant>>,
) {
    let Some(arena) = arena else { return };
    let now = std::time::Instant::now();
    if last.is_some_and(|t| now.duration_since(t).as_secs_f32() < ARENA_LOG_SECS) {
        return;
    }
    *last = Some(now);
    let sim_half = game::CHUNK_UNITS as f32 * 0.5 * 1.01;
    let (mut near, mut near_q, mut fine_q, mut coarse, mut coarse_q, mut lod, mut lod_q) =
        (0u32, 0u64, 0u64, 0u32, 0u64, 0u32, 0u64);
    let mut lod_by_size = std::collections::BTreeMap::<u32, (u32, u64)>::new();
    // LOD quads per face id (`Face::index()` order), then cutout.
    let mut lod_faces = [0u64; 7];
    for m in arena.metadata.values().iter().filter(|m| m.quad_count != 0) {
        let q = u64::from(m.quad_count);
        match m.center[3] as u32 {
            2 => {
                coarse += 1;
                coarse_q += q;
            }
            band if m.half_extents[0] <= sim_half => {
                near += 1;
                near_q += q;
                if band == 1 {
                    fine_q += q;
                }
            }
            _ => {
                lod += 1;
                lod_q += q;
                let entry = lod_by_size
                    .entry(m.half_extents[0] as u32 * 2 / game::BLOCK_VOXELS as u32)
                    .or_insert((0u32, 0u64));
                entry.0 += 1;
                entry.1 += q;
                let mut prev = 0u32;
                for (f, total) in lod_faces.iter_mut().take(6).enumerate() {
                    *total += u64::from(m.face_ends[f] - prev.min(m.face_ends[f]));
                    prev = m.face_ends[f];
                }
                lod_faces[6] += q - u64::from(m.face_ends[7].min(m.quad_count));
            }
        }
    }
    // LOD tiles by horizontal size (metres): where the far quads sit.
    let by_size: Vec<String> = lod_by_size
        .iter()
        .map(|(m, (n, q))| format!("{m}m:{n}/{q}"))
        .collect();
    bevy::log::info!(
        "[arena] near_chunks={near} near_quads={near_q} (with_twin={fine_q}) coarse_twins={coarse} coarse_quads={coarse_q} lod_slots={lod} lod_quads={lod_q} lod_by_size(tiles/quads)=[{}] lod_faces={lod_faces:?}",
        by_size.join(" ")
    );
}

/// `VOXEL_EXIT_AFTER_SECS=N` closes the client cleanly (`AppExit`) N seconds after
/// start — for unattended profiling runs whose output only flushes on a clean exit
/// (the `trace_chrome` JSON), where a SIGTERM would truncate it. Native only.
pub fn exit_after_from_env() -> Option<f32> {
    std::env::var("VOXEL_EXIT_AFTER_SECS")
        .ok()?
        .parse()
        .ok()
        .filter(|s: &f32| *s > 0.0)
}

/// Writes `AppExit::Success` once `secs` of real time have passed.
pub fn exit_after_plugin(app: &mut App, secs: f32) {
    use bevy::prelude::*;
    app.add_systems(
        Update,
        move |time: Res<Time<Real>>, mut exit: MessageWriter<AppExit>| {
            if time.elapsed_secs() >= secs {
                exit.write(AppExit::Success);
            }
        },
    );
}

/// `VOXEL_SCREENSHOT=<secs>:<path>`: save the primary window to `<path>` (PNG)
/// once, `secs` of real time after start — an in-process capture for
/// before/after image comparisons of renderer changes. Native only.
pub fn screenshot_from_env() -> Option<(f32, String)> {
    let value = std::env::var("VOXEL_SCREENSHOT").ok()?;
    let (secs, path) = value.split_once(':')?;
    Some((secs.parse().ok()?, path.to_owned()))
}

/// Takes the [`screenshot_from_env`] capture.
pub fn screenshot_plugin(app: &mut App, secs: f32, path: String) {
    use bevy::prelude::*;
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    app.add_systems(
        Update,
        move |time: Res<Time<Real>>, mut commands: Commands, mut taken: Local<bool>| {
            if !*taken && time.elapsed_secs() >= secs {
                *taken = true;
                commands
                    .spawn(Screenshot::primary_window())
                    .observe(save_to_disk(path.clone()));
            }
        },
    );
}
