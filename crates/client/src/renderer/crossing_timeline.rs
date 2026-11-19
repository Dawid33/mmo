//! Render-thread timeline of one region crossing: from its start (the home
//! region flips, the near window or the LOD rings change, or the local body
//! disappears) to each end point — the body back, every region removed /
//! despawned / spawned / filled with voxels, near and LOD meshing idle, the
//! GPU quiet — as `[perf]` lines sharing `t_ms` with the game thread's
//! (`crossing_flip`, `window_shift`, `region_loaded`, `lod_shift`, …).
//!
//! Lines, all carrying the crossing's `id`:
//! - `crossing_start` — what opened it.
//! - `crossing_step` — one per end point, `ms` since the start.
//! - `crossing_progress` — queue depths every [`PROGRESS_INTERVAL`] while open.
//! - `crossing_done` — totals: how long until everything was quiet
//!   (`settled_ms`), frames and long frames, and what reached or left the GPU
//!   ([`GpuCounters`]: pulled-terrain uploads and releases, whole-buffer
//!   regrows; mesh assets added / modified / removed).
//!
//! Observes only: it diffs the bridge's maps and the queues each frame, so no
//! system it watches changes. Before `world_ready` a change of the LOD rings
//! opens nothing (they are still loading for the join), and rings that were
//! busy when a crossing opened do not hold it open.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

// Not `std::time::Instant`: its `now()` panics on wasm32-unknown-unknown.
use bevy::platform::time::Instant;
use bevy::prelude::*;

use super::bridge::{LocalBody, PendingSnapshotVoxels, RegionRoots, RetiredRegionRoots, SimEntity};
use super::hud::HudStatus;
use super::join_timeline::WorldReady;
use super::lod_rings::LodRings;
use super::meshing::lod::{LodBuildQueue, LodMeshTask};
use super::meshing::queue::RemeshQueue;
use super::meshing::MeshingTask;

/// All-quiet frames before a crossing is declared done (as `world_ready`).
pub const SETTLED_FRAMES: u32 = 30;
/// A crossing still not quiet after this long is closed with `timeout=1`.
pub const CROSSING_TIMEOUT: Duration = Duration::from_secs(20);
/// Seconds between `crossing_progress` lines.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);
/// `crossing_step` lines per crossing; further steps are counted, not written.
pub const MAX_STEP_LINES: u32 = 200;
/// A frame longer than this is a `long_frame` step.
pub const LONG_FRAME_MS: f32 = 50.0;
/// `long_frame` steps per crossing.
pub const MAX_LONG_FRAME_LINES: u32 = 16;

/// What reached or left the GPU, counted where it happens (the render world's
/// `prepare_chunk_quads`) and read by the timeline as deltas. Process-wide
/// totals: they only ever grow.
#[derive(Default)]
pub struct GpuCounters {
    /// Chunks whose quad records were written into the pulled-terrain arena.
    pub pull_uploads: AtomicU64,
    pub pull_upload_quads: AtomicU64,
    /// Chunks whose arena allocation was released.
    pub pull_releases: AtomicU64,
    pub pull_release_quads: AtomicU64,
    /// The record buffer outgrew its capacity: a new GPU buffer, every record
    /// written again.
    pub pull_regrows: AtomicU64,
    pub pull_regrow_bytes: AtomicU64,
    /// The pulled-terrain bind group was rebuilt.
    pub pull_bind_groups: AtomicU64,
}

pub static GPU: GpuCounters = GpuCounters {
    pull_uploads: AtomicU64::new(0),
    pull_upload_quads: AtomicU64::new(0),
    pull_releases: AtomicU64::new(0),
    pull_release_quads: AtomicU64::new(0),
    pull_regrows: AtomicU64::new(0),
    pull_regrow_bytes: AtomicU64::new(0),
    pull_bind_groups: AtomicU64::new(0),
};

impl GpuCounters {
    pub fn add(counter: &AtomicU64, n: u64) {
        counter.fetch_add(n, Ordering::Relaxed);
    }

    fn snapshot(&self) -> GpuSnapshot {
        let get = |c: &AtomicU64| c.load(Ordering::Relaxed);
        GpuSnapshot {
            pull_uploads: get(&self.pull_uploads),
            pull_upload_quads: get(&self.pull_upload_quads),
            pull_releases: get(&self.pull_releases),
            pull_release_quads: get(&self.pull_release_quads),
            pull_regrows: get(&self.pull_regrows),
            pull_regrow_bytes: get(&self.pull_regrow_bytes),
            pull_bind_groups: get(&self.pull_bind_groups),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuSnapshot {
    pub pull_uploads: u64,
    pub pull_upload_quads: u64,
    pub pull_releases: u64,
    pub pull_release_quads: u64,
    pub pull_regrows: u64,
    pub pull_regrow_bytes: u64,
    pub pull_bind_groups: u64,
}

impl GpuSnapshot {
    fn since(&self, earlier: &GpuSnapshot) -> GpuSnapshot {
        GpuSnapshot {
            pull_uploads: self.pull_uploads - earlier.pull_uploads,
            pull_upload_quads: self.pull_upload_quads - earlier.pull_upload_quads,
            pull_releases: self.pull_releases - earlier.pull_releases,
            pull_release_quads: self.pull_release_quads - earlier.pull_release_quads,
            pull_regrows: self.pull_regrows - earlier.pull_regrows,
            pull_regrow_bytes: self.pull_regrow_bytes - earlier.pull_regrow_bytes,
            pull_bind_groups: self.pull_bind_groups - earlier.pull_bind_groups,
        }
    }
}

/// One frame's view of everything a crossing can leave unfinished. Pure data,
/// so the quiet rule is testable without a Bevy world.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Depths {
    /// The local body (or its camera) exists.
    pub body: bool,
    pub regions: usize,
    /// Region trees unmapped but not despawned yet.
    pub retired_regions: usize,
    /// Chunks waiting for their snapshot voxels.
    pub pending_voxels: usize,
    pub remesh_queue: usize,
    pub near_tasks: usize,
    pub lod_roots: usize,
    pub lod_retired: usize,
    pub lod_events: usize,
    pub lod_build_queue: usize,
    pub lod_tasks: usize,
    /// Something reached or left the GPU this frame.
    pub gpu_active: bool,
}

impl Depths {
    pub fn near_busy(&self) -> bool {
        self.pending_voxels > 0 || self.remesh_queue > 0 || self.near_tasks > 0
    }

    pub fn lod_busy(&self) -> bool {
        self.lod_events > 0 || self.lod_build_queue > 0 || self.lod_tasks > 0 || self.lod_retired > 0
    }

    /// Nothing of a crossing is still in flight.
    pub fn quiet(&self) -> bool {
        self.quiet_but_lod() && !self.lod_busy()
    }

    /// [`Self::quiet`] leaving the LOD rings out (they were busy before the
    /// crossing began; their mesh uploads still count as GPU activity).
    pub fn quiet_but_lod(&self) -> bool {
        self.body && self.retired_regions == 0 && !self.near_busy() && !self.gpu_active
    }

    /// The gates that are not green, for a `timeout=1` line.
    pub fn red(&self) -> String {
        let gates = [
            (!self.body, "body"),
            (self.retired_regions > 0, "retired_regions"),
            (self.near_busy(), "near"),
            (self.lod_busy(), "lod"),
            (self.gpu_active, "gpu"),
        ];
        let red: Vec<&str> = gates.iter().filter(|(on, _)| *on).map(|(_, name)| *name).collect();
        if red.is_empty() { "none".to_string() } else { red.join("+") }
    }
}

/// The open crossing.
struct Open {
    id: u32,
    at: Instant,
    /// False on the frame that opened it.
    opened: bool,
    /// The LOD rings were already busy when it opened: they are reported, but
    /// do not hold it open.
    lod_busy_at_start: bool,
    step_lines: u32,
    steps_dropped: u32,
    long_frame_lines: u32,
    last_progress: Instant,
    quiet_frames: u32,
    /// When the current run of quiet frames began, ms since the start.
    quiet_since_ms: f64,
    /// Regions spawned inside this crossing whose chunks still wait for voxels.
    filling: BTreeSet<game::RegionCoords>,
    body_gone_at: Option<Instant>,
    body_gap_ms: f64,
    near_was_busy: bool,
    lod_was_busy: bool,
    gpu_seen: bool,
    gpu_was_active: bool,
    last_gpu_ms: f64,
    last_near_ms: f64,
    last_lod_ms: f64,
    gpu_at_start: GpuSnapshot,
    home_flips: u32,
    regions_added: u32,
    regions_removed: u32,
    regions_despawned: u32,
    lod_roots_added: u32,
    lod_roots_removed: u32,
    mesh_added: u32,
    mesh_modified: u32,
    mesh_removed: u32,
    frames: u32,
    frame_max_ms: f32,
    /// Frames over 25 ms (as `region_crossing_render`).
    long_frames: u32,
    frames_over_50: u32,
}

impl Open {
    fn ms(&self, now: Instant) -> f64 {
        game::perf::ms(now.duration_since(self.at))
    }

    fn step(&mut self, now: Instant, step: &str, detail: std::fmt::Arguments) {
        if self.step_lines >= MAX_STEP_LINES {
            self.steps_dropped += 1;
            return;
        }
        self.step_lines += 1;
        game::perf_event!("crossing_step", "id={} ms={} step={} {}", self.id, self.ms(now), step, detail);
    }
}

#[derive(Default)]
pub struct CrossingTimelineState {
    next_id: u32,
    open: Option<Open>,
    home: Option<game::RegionCoords>,
    regions: BTreeSet<game::RegionCoords>,
    retired: Vec<(game::RegionCoords, Entity)>,
    lod_keys: BTreeSet<game::lod::LodKey>,
    body_seen: bool,
    /// A local body has existed in this session (before one does, its absence
    /// holds nothing open).
    body_ever: bool,
    gpu: GpuSnapshot,
    /// The maps above hold a first reading (nothing to diff against before).
    primed: bool,
}

/// What changed since the last frame.
#[derive(Default)]
struct Changes {
    home_flip: Option<(game::RegionCoords, game::RegionCoords)>,
    regions_added: Vec<game::RegionCoords>,
    regions_removed: Vec<game::RegionCoords>,
    regions_despawned: Vec<game::RegionCoords>,
    lod_added: u32,
    lod_removed: u32,
    body_gone: bool,
    body_back: bool,
}

impl Changes {
    /// What opens a crossing, if anything here does. The rings changing opens
    /// one only when `lod_counts` (the world is ready: before that they are
    /// still loading for the join, for minutes at the full view distance).
    fn trigger(&self, lod_counts: bool) -> Option<&'static str> {
        if self.home_flip.is_some() {
            Some("home_flip")
        } else if self.body_gone {
            Some("body_gone")
        } else if !self.regions_added.is_empty() || !self.regions_removed.is_empty() {
            Some("window_shift")
        } else if lod_counts && (self.lod_added > 0 || self.lod_removed > 0) {
            Some("lod_shift")
        } else {
            None
        }
    }
}

fn rc(r: game::RegionCoords) -> String {
    format!("{},{}", r.x, r.z)
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)] // Bevy system: params are DI, not an API
pub fn track_crossing_timeline(
    // Per session: ids and maps start again in the next world.
    mut state: crate::session::SessionLocal<CrossingTimelineState>,
    time: Res<Time<Real>>,
    world_ready: Res<WorldReady>,
    hud: Res<HudStatus>,
    roots: Res<RegionRoots>,
    retired: Res<RetiredRegionRoots>,
    rings: Res<LodRings>,
    remesh_queue: Res<RemeshQueue>,
    lod_build_queue: Res<LodBuildQueue>,
    near_tasks: Query<(), With<MeshingTask>>,
    lod_tasks: Query<(), With<LodMeshTask>>,
    pending_voxels: Query<&SimEntity, With<PendingSnapshotVoxels>>,
    bodies: Query<(), (With<SimEntity>, Or<(With<LocalBody>, With<Camera3d>)>)>,
    mut mesh_events: MessageReader<AssetEvent<Mesh>>,
) {
    let s = state.get();
    let now = Instant::now();

    // Mesh assets (the LOD rings, bodies, the per-chunk backend): each one
    // added or modified is a vertex buffer upload, each removed a free.
    let (mut mesh_added, mut mesh_modified, mut mesh_removed) = (0u32, 0u32, 0u32);
    for event in mesh_events.read() {
        match event {
            AssetEvent::Added { .. } => mesh_added += 1,
            AssetEvent::Modified { .. } => mesh_modified += 1,
            AssetEvent::Removed { .. } => mesh_removed += 1,
            _ => {}
        }
    }
    let gpu = GPU.snapshot();
    let gpu_frame = gpu.since(&s.gpu);
    s.gpu = gpu;
    let gpu_active = gpu_frame != GpuSnapshot::default() || mesh_added + mesh_modified + mesh_removed > 0;

    // Diff the bridge's maps against last frame's.
    let mut changes = Changes::default();
    if let (Some(home), Some(prev)) = (hud.home_region, s.home) {
        if home != prev {
            changes.home_flip = Some((prev, home));
        }
    }
    s.home = hud.home_region.or(s.home);

    if !roots.0.keys().eq(s.regions.iter()) {
        let live: BTreeSet<game::RegionCoords> = roots.0.keys().copied().collect();
        changes.regions_added = live.difference(&s.regions).copied().collect();
        changes.regions_removed = s.regions.difference(&live).copied().collect();
        s.regions = live;
    }
    if !retired.0.iter().eq(s.retired.iter()) {
        let live: Vec<(game::RegionCoords, Entity)> = retired.0.iter().copied().collect();
        changes.regions_despawned =
            s.retired.iter().filter(|entry| !live.contains(entry)).map(|(region, _)| *region).collect();
        s.retired = live;
    }
    if !rings.roots.keys().eq(s.lod_keys.iter()) {
        let live: BTreeSet<game::lod::LodKey> = rings.roots.keys().copied().collect();
        changes.lod_added = live.difference(&s.lod_keys).count() as u32;
        changes.lod_removed = s.lod_keys.difference(&live).count() as u32;
        s.lod_keys = live;
    }
    let body = !bodies.is_empty();
    changes.body_gone = s.body_seen && !body;
    changes.body_back = !s.body_seen && body;
    s.body_seen = body;
    s.body_ever |= body;
    let body_ever = s.body_ever;

    if !s.primed {
        // The first reading is the state, not a change.
        s.primed = true;
        return;
    }

    // Work already under way when a crossing opens (the join's LOD rings, a
    // region still meshing) is reported on its start line; rings that were
    // busy then do not hold the crossing open.
    let lod_busy_now = !rings.pending.is_empty()
        || lod_build_queue.pending_count() > 0
        || !lod_tasks.is_empty()
        || !rings.retired.is_empty();
    let near_busy_now = !remesh_queue.dirty.is_empty() || !near_tasks.is_empty() || !pending_voxels.is_empty();
    if s.open.is_none() {
        let Some(trigger) = changes.trigger(world_ready.0) else { return };
        // The join's own first region is not a crossing.
        if trigger == "window_shift" && roots.0.len() == changes.regions_added.len() {
            return;
        }
        s.next_id += 1;
        let (from, to) = match changes.home_flip {
            Some((from, to)) => (rc(from), rc(to)),
            None => ("-".to_string(), s.home.map_or("-".to_string(), rc)),
        };
        game::perf_event!(
            "crossing_start",
            "id={} trigger={} from={} to={} regions={} lod_roots={} world_ready={} lod_busy={} near_busy={}",
            s.next_id,
            trigger,
            from,
            to,
            roots.0.len(),
            rings.roots.len(),
            u8::from(world_ready.0),
            u8::from(lod_busy_now),
            u8::from(near_busy_now)
        );
        s.open = Some(Open {
            id: s.next_id,
            at: now,
            opened: false,
            lod_busy_at_start: lod_busy_now,
            step_lines: 0,
            steps_dropped: 0,
            long_frame_lines: 0,
            last_progress: now,
            quiet_frames: 0,
            quiet_since_ms: 0.0,
            filling: BTreeSet::new(),
            body_gone_at: None,
            body_gap_ms: 0.0,
            near_was_busy: false,
            lod_was_busy: false,
            gpu_seen: false,
            gpu_was_active: false,
            last_gpu_ms: 0.0,
            last_near_ms: 0.0,
            last_lod_ms: 0.0,
            // This frame's GPU work counts toward the crossing.
            gpu_at_start: gpu.since(&gpu_frame),
            home_flips: 0,
            regions_added: 0,
            regions_removed: 0,
            regions_despawned: 0,
            lod_roots_added: 0,
            lod_roots_removed: 0,
            mesh_added: 0,
            mesh_modified: 0,
            mesh_removed: 0,
            frames: 0,
            frame_max_ms: 0.0,
            long_frames: 0,
            frames_over_50: 0,
        });
    }
    let Some(o) = s.open.as_mut() else { return };

    // Steps: what changed this frame.
    if let Some((from, to)) = changes.home_flip {
        o.home_flips += 1;
        o.step(now, "home_flip", format_args!("from={} to={}", rc(from), rc(to)));
    }
    if changes.body_gone {
        o.body_gone_at = Some(now);
        o.step(now, "body_gone", format_args!(""));
    }
    if changes.body_back {
        let gap = o.body_gone_at.take().map_or(0.0, |at| game::perf::ms(now.duration_since(at)));
        o.body_gap_ms += gap;
        o.step(now, "body_back", format_args!("gap_ms={gap}"));
    }
    for region in &changes.regions_removed {
        o.regions_removed += 1;
        o.filling.remove(region);
        o.step(now, "region_removed", format_args!("region={} retired={}", rc(*region), retired.0.len()));
    }
    for region in &changes.regions_despawned {
        o.regions_despawned += 1;
        o.step(now, "region_despawned", format_args!("region={} retired={}", rc(*region), retired.0.len()));
    }
    for region in &changes.regions_added {
        o.regions_added += 1;
        o.filling.insert(*region);
        o.step(now, "region_added", format_args!("region={} regions={}", rc(*region), roots.0.len()));
    }
    if changes.lod_added > 0 || changes.lod_removed > 0 {
        o.lod_roots_added += changes.lod_added;
        o.lod_roots_removed += changes.lod_removed;
    }
    o.mesh_added += mesh_added;
    o.mesh_modified += mesh_modified;
    o.mesh_removed += mesh_removed;

    // Regions spawned in this crossing: when did their last chunk get voxels?
    let mut pending = 0usize;
    let mut still_filling: BTreeSet<game::RegionCoords> = BTreeSet::new();
    for sim in &pending_voxels {
        pending += 1;
        if !o.filling.is_empty() && o.filling.contains(&sim.region) {
            still_filling.insert(sim.region);
        }
    }
    // A region's chunk entities exist from the frame after it is mapped (the
    // spawn is a command), so it is judged from the frame after it was added.
    let landed: Vec<game::RegionCoords> = o
        .filling
        .iter()
        .filter(|region| !still_filling.contains(region) && !changes.regions_added.contains(region))
        .copied()
        .collect();
    for region in landed {
        o.filling.remove(&region);
        o.step(now, "region_voxels_landed", format_args!("region={}", rc(region)));
    }

    let depths = Depths {
        body: body || !body_ever,
        regions: roots.0.len(),
        retired_regions: retired.0.len(),
        pending_voxels: pending,
        remesh_queue: remesh_queue.dirty.len(),
        near_tasks: near_tasks.iter().count(),
        lod_roots: rings.roots.len(),
        lod_retired: rings.retired.len(),
        lod_events: rings.pending.len(),
        lod_build_queue: lod_build_queue.pending_count(),
        lod_tasks: lod_tasks.iter().count(),
        gpu_active,
    };

    // Busy / idle edges of the three pipelines.
    let ms = o.ms(now);
    match (o.near_was_busy, depths.near_busy()) {
        (false, true) => o.step(
            now,
            "near_busy",
            format_args!(
                "pending_voxels={} remesh_queue={} tasks={}",
                depths.pending_voxels, depths.remesh_queue, depths.near_tasks
            ),
        ),
        (true, false) => o.step(now, "near_idle", format_args!("")),
        _ => {}
    }
    if depths.near_busy() {
        o.last_near_ms = ms;
    }
    o.near_was_busy = depths.near_busy();
    match (o.lod_was_busy, depths.lod_busy()) {
        (false, true) => o.step(
            now,
            "lod_busy",
            format_args!(
                "events={} build_queue={} tasks={} retired={}",
                depths.lod_events, depths.lod_build_queue, depths.lod_tasks, depths.lod_retired
            ),
        ),
        (true, false) => {
            let (added, removed) = (o.lod_roots_added, o.lod_roots_removed);
            o.step(now, "lod_idle", format_args!("roots_added={added} roots_removed={removed}"));
        }
        _ => {}
    }
    if depths.lod_busy() {
        o.last_lod_ms = ms;
    }
    o.lod_was_busy = depths.lod_busy();
    if gpu_active {
        if !o.gpu_seen {
            o.gpu_seen = true;
            o.step(
                now,
                "gpu_first",
                format_args!(
                    "pull_uploads={} pull_releases={} pull_regrows={} mesh_added={} mesh_modified={} mesh_removed={}",
                    gpu_frame.pull_uploads,
                    gpu_frame.pull_releases,
                    gpu_frame.pull_regrows,
                    mesh_added,
                    mesh_modified,
                    mesh_removed
                ),
            );
        }
        if gpu_frame.pull_regrows > 0 {
            o.step(
                now,
                "gpu_regrow",
                format_args!("mib={:.1}", gpu_frame.pull_regrow_bytes as f64 / (1024.0 * 1024.0)),
            );
        }
        o.last_gpu_ms = ms;
    } else if o.gpu_was_active {
        // The end of a burst; a later burst writes another.
        let total = gpu.since(&o.gpu_at_start);
        let (added, removed) = (o.mesh_added, o.mesh_removed);
        o.step(
            now,
            "gpu_quiet",
            format_args!(
                "pull_uploads={} pull_releases={} mesh_added={added} mesh_removed={removed}",
                total.pull_uploads, total.pull_releases
            ),
        );
    }
    o.gpu_was_active = gpu_active;

    // Frames inside the crossing. `delta` is the frame before this one, so on
    // the opening frame it still belongs to the flight before the crossing.
    let frame_ms = time.delta_secs() * 1000.0;
    if o.opened {
        o.frames += 1;
        o.frame_max_ms = o.frame_max_ms.max(frame_ms);
        if frame_ms > 25.0 {
            o.long_frames += 1;
        }
        if frame_ms > LONG_FRAME_MS {
            o.frames_over_50 += 1;
            if o.long_frame_lines < MAX_LONG_FRAME_LINES {
                o.long_frame_lines += 1;
                o.step(now, "long_frame", format_args!("frame_ms={frame_ms:.1}"));
            }
        }
    }
    o.opened = true;

    if now.duration_since(o.last_progress) >= PROGRESS_INTERVAL {
        o.last_progress = now;
        game::perf_event!(
            "crossing_progress",
            "id={} ms={} body={} regions={} retired_regions={} pending_voxels={} remesh_queue={} near_tasks={} \
             lod_roots={} lod_retired={} lod_events={} lod_build_queue={} lod_tasks={} gpu_active={}",
            o.id,
            ms,
            u8::from(depths.body),
            depths.regions,
            depths.retired_regions,
            depths.pending_voxels,
            depths.remesh_queue,
            depths.near_tasks,
            depths.lod_roots,
            depths.lod_retired,
            depths.lod_events,
            depths.lod_build_queue,
            depths.lod_tasks,
            u8::from(depths.gpu_active)
        );
    }

    // Done: quiet for a while (reported from when the quiet began), or late.
    let quiet = if o.lod_busy_at_start { depths.quiet_but_lod() } else { depths.quiet() };
    if quiet && changes.trigger(world_ready.0).is_none() {
        if o.quiet_frames == 0 {
            o.quiet_since_ms = ms;
        }
        o.quiet_frames += 1;
    } else {
        o.quiet_frames = 0;
    }
    let timeout = now.duration_since(o.at) >= CROSSING_TIMEOUT;
    if o.quiet_frames < SETTLED_FRAMES && !timeout {
        return;
    }
    let total = gpu.since(&o.gpu_at_start);
    game::perf_event!(
        "crossing_done",
        "id={} settled_ms={} total_ms={} timeout={} red={} lod_busy_at_start={} home_flips={} body_gap_ms={} regions_added={} \
         regions_removed={} regions_despawned={} lod_roots_added={} lod_roots_removed={} last_near_ms={} \
         last_lod_ms={} last_gpu_ms={} pull_uploads={} pull_upload_quads={} pull_releases={} \
         pull_release_quads={} pull_regrows={} pull_regrow_mib={:.1} pull_bind_groups={} mesh_added={} \
         mesh_modified={} mesh_removed={} frames={} frame_max_ms={:.1} long_frames={} frames_over_50={} \
         steps_dropped={}",
        o.id,
        if timeout { ms } else { o.quiet_since_ms },
        ms,
        u8::from(timeout),
        if timeout { depths.red() } else { "none".to_string() },
        u8::from(o.lod_busy_at_start),
        o.home_flips,
        o.body_gap_ms,
        o.regions_added,
        o.regions_removed,
        o.regions_despawned,
        o.lod_roots_added,
        o.lod_roots_removed,
        o.last_near_ms,
        o.last_lod_ms,
        o.last_gpu_ms,
        total.pull_uploads,
        total.pull_upload_quads,
        total.pull_releases,
        total.pull_release_quads,
        total.pull_regrows,
        total.pull_regrow_bytes as f64 / (1024.0 * 1024.0),
        total.pull_bind_groups,
        o.mesh_added,
        o.mesh_modified,
        o.mesh_removed,
        o.frames,
        o.frame_max_ms,
        o.long_frames,
        o.frames_over_50,
        o.steps_dropped
    );
    s.open = None;
}

