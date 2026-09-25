//! What the camera's motion looks like on screen, reduced to one
//! `[client-motion]` line per report window (DEBUG on the `perf_budget`
//! target, so `PERF_LOG=1` / `just perf-capture` show it).
//!
//! It tells two causes of a "pulled back" feel apart. The sim pose stepping
//! *against* its own direction of travel is a rollback correction. The sim
//! pose only ever advancing while the camera's trailing distance swings is
//! the render side: `interpolate_transforms` closes a fixed fraction of the
//! gap per frame, so the camera trails by a distance proportional to the
//! frame time, and a long frame lets it fall behind before it catches up.
//! Frames at a camera chunk-cell change are reported apart from the rest,
//! since that is where culling and extraction have the most to redo.

use std::time::{Duration, Instant};

use bevy::prelude::*;

use super::bridge::SimTarget;

/// How often the line is written.
const REPORT_INTERVAL: Duration = Duration::from_secs(5);
/// Frames after a camera chunk-cell change that still count as "crossing".
const CROSSING_FRAMES: u32 = 3;
/// A step shorter than this (world units) carries no direction.
const MIN_STEP: f32 = 0.05;
/// Chunk edge, world units.
const CHUNK: f32 = 32.0;

#[derive(Default)]
struct Series {
    n: u32,
    sum: f32,
    max: f32,
}

impl Series {
    fn add(&mut self, v: f32) {
        self.n += 1;
        self.sum += v;
        self.max = self.max.max(v);
    }
    fn avg(&self) -> f32 {
        if self.n > 0 { self.sum / self.n as f32 } else { 0.0 }
    }
}

#[derive(Default)]
pub struct ProbeState {
    window_start: Option<Instant>,
    last_target: Option<Vec3>,
    last_camera: Option<Vec3>,
    /// Direction the sim pose last travelled in.
    heading: Option<Vec3>,
    last_cell: Option<IVec3>,
    crossing_frames_left: u32,
    crossings: u32,
    frame_ms: Series,
    crossing_frame_ms: Series,
    /// Camera-to-sim-pose distance while moving.
    trail: Series,
    crossing_trail: Series,
    /// Steps of the sim pose against its heading (rollback corrections).
    target_back: Series,
    /// Steps of the rendered camera against the heading.
    camera_back: Series,
}

pub fn probe_camera_motion(
    time: Res<Time>,
    cameras: Query<(&Transform, &SimTarget), With<Camera3d>>,
    mut state: Local<ProbeState>,
) {
    let Some((tf, target)) = cameras.iter().next() else {
        return;
    };
    let now = Instant::now();
    let s = &mut *state;
    let start = *s.window_start.get_or_insert(now);

    let cell = (target.goal() / CHUNK).floor().as_ivec3();
    if s.last_cell.is_some_and(|last| last != cell) {
        s.crossings += 1;
        s.crossing_frames_left = CROSSING_FRAMES;
    }
    s.last_cell = Some(cell);
    let crossing = s.crossing_frames_left > 0;
    s.crossing_frames_left = s.crossing_frames_left.saturating_sub(1);

    let frame_ms = time.delta_secs() * 1000.0;
    if crossing { s.crossing_frame_ms.add(frame_ms) } else { s.frame_ms.add(frame_ms) }

    if let Some(last) = s.last_target {
        let step = target.goal() - last;
        if step.length() > MIN_STEP {
            if let Some(heading) = s.heading {
                let along = step.dot(heading);
                if along < 0.0 {
                    s.target_back.add(-along);
                }
            }
            s.heading = Some(step.normalize());
        }
    }
    if let (Some(last), Some(heading)) = (s.last_camera, s.heading) {
        let along = (tf.translation - last).dot(heading);
        if along < -MIN_STEP {
            s.camera_back.add(-along);
        }
    }
    let trail = tf.translation.distance(target.goal());
    if trail > MIN_STEP {
        if crossing { s.crossing_trail.add(trail) } else { s.trail.add(trail) }
    }
    s.last_target = Some(target.goal());
    s.last_camera = Some(tf.translation);

    if now.duration_since(start) < REPORT_INTERVAL {
        return;
    }
    log::debug!(
        target: game::perf::BUDGET_TARGET,
        "[client-motion] frames={} frame avg={:.1}ms max={:.1}ms | chunk crossings={} frame avg={:.1}ms max={:.1}ms | \
         trail avg={:.2}u max={:.2}u, crossing avg={:.2}u max={:.2}u | \
         sim pose stepped back n={} sum={:.2}u max={:.2}u | camera stepped back n={} sum={:.2}u max={:.2}u",
        s.frame_ms.n + s.crossing_frame_ms.n,
        s.frame_ms.avg(),
        s.frame_ms.max,
        s.crossings,
        s.crossing_frame_ms.avg(),
        s.crossing_frame_ms.max,
        s.trail.avg(),
        s.trail.max,
        s.crossing_trail.avg(),
        s.crossing_trail.max,
        s.target_back.n,
        s.target_back.sum,
        s.target_back.max,
        s.camera_back.n,
        s.camera_back.sum,
        s.camera_back.max,
    );
    *s = ProbeState {
        window_start: Some(now),
        last_target: s.last_target,
        last_camera: s.last_camera,
        heading: s.heading,
        last_cell: s.last_cell,
        ..Default::default()
    };
}

/// How long after a home-region flip frames still count toward that crossing.
const REGION_CROSSING_WINDOW: Duration = Duration::from_secs(2);

/// Motion of the sim pose between frames. The pose only moves when a tick lands
/// (`TICK_RATE`), so a `hold` of about one tick is the steady state; a pause
/// shows as a hold several ticks long.
#[derive(Default)]
struct MotionStats {
    frames: u32,
    frame_max_ms: f32,
    /// Frames over 25 ms.
    long_frames: u32,
    hold_max_ms: f32,
    /// Largest single-frame step of the sim pose (a catch-up snap).
    jump_max: f32,
    back_n: u32,
    back_max: f32,
}

#[derive(Default)]
pub struct RegionCrossingProbe {
    home: Option<game::RegionCoords>,
    last_goal: Option<Vec3>,
    heading: Option<Vec3>,
    held_since: Option<Instant>,
    /// Open crossing: flip instant, (from, to).
    open: Option<(Instant, game::RegionCoords, game::RegionCoords)>,
    before: MotionStats,
    after: MotionStats,
}

/// One `[perf] event=region_crossing_render` line per home-region flip (a
/// handoff between two region threads, which also shifts the near window): the
/// [`REGION_CROSSING_WINDOW`] after the flip against the flight before it.
/// Pairs with the game thread's `region_crossing` line.
pub fn probe_region_crossing(
    time: Res<Time<Real>>,
    hud: Res<super::hud::HudStatus>,
    cameras: Query<(&Camera, &SimTarget), With<Camera3d>>,
    mut state: Local<RegionCrossingProbe>,
) {
    let Some(target) = cameras.iter().find(|(c, _)| c.is_active).map(|(_, t)| t) else {
        return;
    };
    let now = Instant::now();
    let s = &mut *state;
    let goal = target.goal();

    if let (Some(home), Some(prev)) = (hud.home_region, s.home) {
        if home != prev {
            if let Some((at, from, to)) = s.open.take() {
                // Crossed again inside the window: close the old one early.
                report_region_crossing(at, from, to, &s.before, &s.after);
                s.before = std::mem::take(&mut s.after);
            }
            s.open = Some((now, prev, home));
        }
    }
    s.home = hud.home_region.or(s.home);

    let stats = if s.open.is_some() { &mut s.after } else { &mut s.before };
    let frame_ms = time.delta_secs() * 1000.0;
    stats.frames += 1;
    stats.frame_max_ms = stats.frame_max_ms.max(frame_ms);
    if frame_ms > 25.0 {
        stats.long_frames += 1;
    }
    if let Some(last) = s.last_goal {
        let step = goal - last;
        if step.length() > MIN_STEP {
            if let Some(since) = s.held_since.take() {
                stats.hold_max_ms = stats.hold_max_ms.max(now.duration_since(since).as_secs_f32() * 1000.0);
            }
            stats.jump_max = stats.jump_max.max(step.length());
            if let Some(heading) = s.heading {
                let along = step.dot(heading);
                if along < 0.0 {
                    stats.back_n += 1;
                    stats.back_max = stats.back_max.max(-along);
                }
            }
            s.heading = Some(step.normalize());
        } else if s.heading.is_some() {
            s.held_since.get_or_insert(now);
        }
    }
    s.last_goal = Some(goal);

    if let Some((at, from, to)) = s.open {
        if now.duration_since(at) >= REGION_CROSSING_WINDOW {
            report_region_crossing(at, from, to, &s.before, &s.after);
            s.open = None;
            s.before = std::mem::take(&mut s.after);
        }
    }
}

fn report_region_crossing(
    _at: Instant,
    from: game::RegionCoords,
    to: game::RegionCoords,
    before: &MotionStats,
    after: &MotionStats,
) {
    game::perf_event!(
        "region_crossing_render",
        "from={},{} to={},{} | after: frames={} frame_max_ms={:.1} long_frames={} hold_max_ms={:.0} \
         jump_max={:.1}u back={} back_max={:.1}u | before: frames={} frame_max_ms={:.1} long_frames={} \
         hold_max_ms={:.0} jump_max={:.1}u back={}",
        from.x,
        from.z,
        to.x,
        to.z,
        after.frames,
        after.frame_max_ms,
        after.long_frames,
        after.hold_max_ms,
        after.jump_max,
        after.back_n,
        after.back_max,
        before.frames,
        before.frame_max_ms,
        before.long_frames,
        before.hold_max_ms,
        before.jump_max,
        before.back_n
    );
}
