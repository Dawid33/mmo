//! Render-thread half of the `[perf]` join timeline (`game::perf`): when did
//! the first frame run, when did terrain first appear, and when was the world
//! actually on screen — plus a slow heartbeat while it is not, which is the
//! evidence for a load that never finishes.
//!
//! `world_ready` deliberately mirrors the render benchmark's readiness gates
//! (`render_bench::BenchmarkReadiness`) minus the benchmark-only one (backend
//! authority): every near region spawned, every expected
//! LOD key rooted, every queue drained and nothing in flight, held for
//! [`WORLD_READY_STABLE_FRAMES`]. The system parks itself once that fires, so
//! steady-state play pays one `bool` check per frame.

use std::collections::{BTreeMap, BTreeSet};

use bevy::prelude::*;

use super::bridge::{PendingSnapshotVoxels, RegionRoots};
use super::hud::HudStatus;
use super::lod_rings::LodRings;
use super::meshing::lod::{LodBuildQueue, LodChunk, LodMeshTask, LodRasterReady};
use super::meshing::queue::RemeshQueue;
use super::meshing::{MeshingTask, TerrainQuadCount};
use crate::world_scale::WorldScale;

/// Consecutive all-gates-green frames before `world_ready` is declared. The
/// join arrives in bursts (region snapshots, paced LOD batches), so the queues
/// go momentarily idle between them; half a second at 60 Hz rides that out.
pub const WORLD_READY_STABLE_FRAMES: u32 = 30;

/// Seconds between `load_progress` heartbeats while the world is not ready.
pub const LOAD_PROGRESS_INTERVAL_SECS: f64 = 2.0;

/// Heartbeats stop after this many lines (10 minutes), so a client that never
/// becomes ready cannot fill the log.
pub const LOAD_PROGRESS_MAX_LINES: u32 = 300;

/// Set once `world_ready` has fired (and never cleared): the autopilot waits
/// on it so a scripted run starts from the same loaded state every time.
#[derive(Resource, Default)]
pub struct WorldReady(pub bool);

#[derive(Default)]
pub struct JoinTimelineState {
    frames: u32,
    first_mesh: bool,
    near_drained: bool,
    expected_lod_keys: Option<usize>,
    /// The viewer region and near-region count `expected_lod_keys` was counted
    /// for: a near region replaces the far tile under it, so the count is redone
    /// whenever either changes (the fortress window loads its regions one by
    /// one, long after the first).
    expected_for: Option<(game::RegionCoords, usize)>,
    stable_frames: u32,
    first_green_ms: u64,
    last_progress_secs: f64,
    progress_lines: u32,
    done: bool,
}

/// Queue depths and counts behind the readiness decision. Pure data so the
/// gate logic is testable without a Bevy world.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoadDepths {
    pub near_regions: usize,
    pub required_near_regions: usize,
    pub lod_roots: usize,
    /// `None` until the viewer's region is known (the key set depends on it).
    pub expected_lod_keys: Option<usize>,
    pub remesh_queue: usize,
    pub lod_build_queue: usize,
    pub pending_lod_events: usize,
    pub in_flight_tasks: usize,
}

impl LoadDepths {
    pub fn near_meshing_idle(&self) -> bool {
        self.near_regions >= self.required_near_regions.max(1) && self.remesh_queue == 0
    }

    pub fn ready(&self) -> bool {
        self.near_regions >= self.required_near_regions
            && self
                .expected_lod_keys
                .is_some_and(|expected| self.lod_roots >= expected)
            && self.remesh_queue == 0
            && self.lod_build_queue == 0
            && self.pending_lod_events == 0
            && self.in_flight_tasks == 0
    }
}

#[allow(clippy::too_many_arguments)]
pub fn track_join_timeline(
    // Per session: a second world gets its own join timeline.
    mut state: crate::session::SessionLocal<JoinTimelineState>,
    // Paired: the function is at Bevy's 16-parameter limit.
    (time, mut world_ready): (Res<Time<Real>>, ResMut<WorldReady>),
    scale: Option<Res<WorldScale>>,
    hud: Res<HudStatus>,
    roots: Res<RegionRoots>,
    rings: Res<LodRings>,
    remesh_queue: Res<RemeshQueue>,
    lod_build_queue: Res<LodBuildQueue>,
    in_flight_near: Query<(), With<MeshingTask>>,
    in_flight_lod: Query<(), With<LodMeshTask>>,
    meshed_bricks: Query<(), (With<super::bricks::BrickOf>, With<TerrainQuadCount>)>,
    pending_snapshot_voxels: Query<(), With<PendingSnapshotVoxels>>,
    lod_cells: Query<(
        &LodChunk,
        &ChildOf,
        &Transform,
        Option<&TerrainQuadCount>,
        Has<LodRasterReady>,
        &ViewVisibility,
    )>,
    names: Query<&Name>,
) {
    let state = state.get();
    if state.done {
        return;
    }
    state.frames += 1;
    match state.frames {
        // Startup (window, adapter, pipelines queued) is behind us.
        1 => game::perf_event!("first_update"),
        // Frame 1 has been extracted and submitted. With pipelined rendering
        // this is one frame early at worst — close enough to separate
        // "seconds of shader compilation" from "instant".
        2 => game::perf_event!("first_frame_submitted"),
        _ => {}
    }

    let scale = scale.map(|s| *s).unwrap_or_default();
    if roots.0.len() >= scale.near_region_count() {
        if let Some(viewer) = hud.viewer_region.filter(|v| state.expected_for != Some((*v, roots.0.len()))) {
            state.expected_for = Some((viewer, roots.0.len()));
            let near: BTreeSet<game::RegionCoords> = roots.0.keys().copied().collect();
            let (_, keys) = crate::instance::desired_lod(
                viewer,
                &near,
                &BTreeMap::new(),
                scale.lod_table,
                game::lod::LOD_STICKY_BAND,
            );
            state.expected_lod_keys = Some(keys.len());
        }
    }

    // Chunks still waiting for their off-thread snapshot voxels are not in the
    // remesh queue yet, but they are very much unfinished near-mesh work.
    let in_flight_near_tasks =
        in_flight_near.iter().count() + pending_snapshot_voxels.iter().count();
    let depths = LoadDepths {
        near_regions: roots.0.len(),
        required_near_regions: scale.near_region_count(),
        lod_roots: rings.roots.len(),
        expected_lod_keys: state.expected_lod_keys,
        remesh_queue: remesh_queue.dirty.len(),
        lod_build_queue: lod_build_queue.pending_count(),
        pending_lod_events: rings.pending.len(),
        in_flight_tasks: in_flight_near_tasks + in_flight_lod.iter().count(),
    };

    if !state.first_mesh && !meshed_bricks.is_empty() {
        state.first_mesh = true;
        game::perf_event!("first_near_mesh", "regions={}", depths.near_regions);
    }
    if !state.near_drained
        && state.first_mesh
        && depths.near_meshing_idle()
        && in_flight_near_tasks == 0
    {
        state.near_drained = true;
        game::perf_event!(
            "near_meshing_drained",
            "regions={} meshed_bricks={}",
            depths.near_regions,
            meshed_bricks.iter().count()
        );
    }

    if depths.ready() {
        if state.stable_frames == 0 {
            state.first_green_ms = game::perf::t_ms();
        }
        state.stable_frames += 1;
        if state.stable_frames >= WORLD_READY_STABLE_FRAMES {
            state.done = true;
            world_ready.0 = true;
            game::perf_event!(
                "world_ready",
                "first_green_ms={} stable_frames={} regions={} lod_roots={} frames={}",
                state.first_green_ms,
                state.stable_frames,
                depths.near_regions,
                depths.lod_roots,
                state.frames
            );
            if game::perf::budget_lines_requested() {
                log_lod_render_state(&lod_cells, &names);
            }
        }
        return;
    }
    state.stable_frames = 0;

    let now = time.elapsed_secs_f64();
    if now - state.last_progress_secs >= LOAD_PROGRESS_INTERVAL_SECS
        && state.progress_lines < LOAD_PROGRESS_MAX_LINES
    {
        state.last_progress_secs = now;
        state.progress_lines += 1;
        game::perf_event!(
            "load_progress",
            "regions={}/{} lod_roots={}/{} remesh_queue={} lod_build_queue={} lod_events={} in_flight={} frames={} connection={:?}",
            depths.near_regions,
            depths.required_near_regions,
            depths.lod_roots,
            depths.expected_lod_keys.map_or("?".to_string(), |n| n.to_string()),
            depths.remesh_queue,
            depths.lod_build_queue,
            depths.pending_lod_events,
            depths.in_flight_tasks,
            state.frames,
            hud.connection
        );
    }
}

/// One-shot `[perf-lod]` dump at `world_ready` (`PERF_LOG=1` only): per LOD
/// root, how many cells exist / finished meshing / have quads / passed view
/// culling this frame, plus a top-down x/z map of quad density — the client-side
/// twin of `worldgen`'s `lod_coverage` example, which maps what the server
/// SENT. A full map there and a partial one here puts the loss in client
/// meshing; full in both puts it in culling or drawing. Rows are z, columns x:
/// `.` no cell, `0` cells but no quads, `1`-`9` log2-ish quad density.
#[allow(clippy::type_complexity)]
fn log_lod_render_state(
    lod_cells: &Query<(
        &LodChunk,
        &ChildOf,
        &Transform,
        Option<&TerrainQuadCount>,
        Has<LodRasterReady>,
        &ViewVisibility,
    )>,
    names: &Query<&Name>,
) {
    #[derive(Default)]
    struct Root {
        cells: u32,
        ready: u32,
        with_quads: u32,
        visible: u32,
        quads: u64,
        map: BTreeMap<(i32, i32), u32>,
    }
    let mut roots: BTreeMap<Entity, Root> = BTreeMap::new();
    for (chunk, parent, transform, quads, ready, view) in lod_cells {
        let root = roots.entry(parent.parent()).or_default();
        let span = 32.0 * chunk.factor as f32;
        let cell = (
            (transform.translation.x / span).round() as i32,
            (transform.translation.z / span).round() as i32,
        );
        let quads = quads.map_or(0, |q| q.0);
        root.cells += 1;
        root.ready += ready as u32;
        root.with_quads += (quads > 0) as u32;
        root.visible += view.get() as u32;
        root.quads += quads as u64;
        let slot = root.map.entry(cell).or_default();
        *slot = (*slot).max(quads);
    }
    for (entity, root) in &roots {
        let name = names.get(*entity).map_or("?".to_string(), |n| n.as_str().replace(' ', "_"));
        info!(
            "[perf-lod] root={name} cells={} raster_ready={} with_quads={} view_visible={} quads={}",
            root.cells, root.ready, root.with_quads, root.visible, root.quads
        );
        let (Some(min_x), Some(max_x)) =
            (root.map.keys().map(|k| k.0).min(), root.map.keys().map(|k| k.0).max())
        else {
            continue;
        };
        let (min_z, max_z) = (
            root.map.keys().map(|k| k.1).min().unwrap_or(0),
            root.map.keys().map(|k| k.1).max().unwrap_or(0),
        );
        for z in min_z..=max_z {
            let row: String = (min_x..=max_x)
                .map(|x| match root.map.get(&(x, z)) {
                    None => '.',
                    Some(0) => '0',
                    Some(&q) => char::from(b'0' + (32 - q.leading_zeros()).clamp(1, 9) as u8),
                })
                .collect();
            info!("[perf-lod]   x{min_x}..{max_x} z={z:<3} {row}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded() -> LoadDepths {
        LoadDepths {
            near_regions: 9,
            required_near_regions: 9,
            lod_roots: 570,
            expected_lod_keys: Some(570),
            ..Default::default()
        }
    }

    #[test]
    fn ready_needs_every_region_key_and_empty_queue() {
        assert!(loaded().ready());
        assert!(!LoadDepths {
            near_regions: 8,
            ..loaded()
        }
        .ready());
        assert!(!LoadDepths {
            lod_roots: 569,
            ..loaded()
        }
        .ready());
        assert!(!LoadDepths {
            expected_lod_keys: None,
            ..loaded()
        }
        .ready());
        assert!(!LoadDepths {
            remesh_queue: 1,
            ..loaded()
        }
        .ready());
        assert!(!LoadDepths {
            lod_build_queue: 1,
            ..loaded()
        }
        .ready());
        assert!(!LoadDepths {
            pending_lod_events: 1,
            ..loaded()
        }
        .ready());
        assert!(!LoadDepths {
            in_flight_tasks: 1,
            ..loaded()
        }
        .ready());
    }

    #[test]
    fn a_world_without_lod_rings_is_ready_on_its_near_window() {
        let tiny = LoadDepths {
            near_regions: 1,
            required_near_regions: 1,
            expected_lod_keys: Some(0),
            ..Default::default()
        };
        assert!(tiny.ready());
    }

    #[test]
    fn near_meshing_is_not_idle_before_any_region_exists() {
        assert!(!LoadDepths::default().near_meshing_idle());
        assert!(loaded().near_meshing_idle());
    }
}
