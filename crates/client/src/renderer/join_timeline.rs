//! Render-thread half of the `[perf]` join timeline (`game::perf`): when did
//! the first frame run, when did terrain first appear, and when was the world
//! actually on screen — plus a slow heartbeat while it is not, which is the
//! evidence for a load that never finishes.
//!
//! `world_ready`: every near region spawned, every expected LOD key rooted,
//! every mesh queue drained and nothing in flight, held for
//! [`WORLD_READY_STABLE_FRAMES`]. The system parks itself once that fires, so
//! steady-state play pays one `bool` check per frame.

use std::collections::{BTreeMap, BTreeSet};

use bevy::prelude::*;

use super::bridge::RegionRoots;
use super::hud::HudStatus;
use super::lod_rings::LodRings;
use super::stud::far::StudLodTasks;
use super::stud::systems::{StudMeshChildren, StudMeshTasks};
use super::stud::world::StudWorld;
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
    /// Near stud chunks waiting for a mesh job.
    pub remesh_queue: usize,
    /// Far tile mesh jobs not landed yet.
    pub lod_build_queue: usize,
    pub pending_lod_events: usize,
    /// Near chunk mesh jobs in flight.
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
    time: Res<Time<Real>>,
    mut world_ready: ResMut<WorldReady>,
    scale: Option<Res<WorldScale>>,
    hud: Res<HudStatus>,
    roots: Res<RegionRoots>,
    rings: Res<LodRings>,
    stud_world: Res<StudWorld>,
    near_tasks: Res<StudMeshTasks>,
    far_tasks: Res<StudLodTasks>,
    meshed_chunks: Query<(), With<StudMeshChildren>>,
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
            // The table the instance subscribes with (`set_world_scale`).
            let table = game::stud_ring_table(scale.lod_table);
            let (_, keys) = crate::instance::desired_lod(viewer, &near, &BTreeMap::new(), &table, game::lod::LOD_STICKY_BAND);
            state.expected_lod_keys = Some(keys.len());
        }
    }

    let in_flight_near_tasks = near_tasks.len();
    let depths = LoadDepths {
        near_regions: roots.0.len(),
        required_near_regions: scale.near_region_count(),
        lod_roots: rings.roots.len(),
        expected_lod_keys: state.expected_lod_keys,
        remesh_queue: stud_world.queued(),
        lod_build_queue: far_tasks.len(),
        pending_lod_events: rings.pending.len(),
        in_flight_tasks: in_flight_near_tasks,
    };

    if !state.first_mesh && !meshed_chunks.is_empty() {
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
            "regions={} meshed_chunks={}",
            depths.near_regions,
            meshed_chunks.iter().count()
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
