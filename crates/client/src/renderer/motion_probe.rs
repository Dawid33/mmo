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

    let cell = (target.pos / CHUNK).floor().as_ivec3();
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
        let step = target.pos - last;
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
    let trail = tf.translation.distance(target.pos);
    if trail > MIN_STEP {
        if crossing { s.crossing_trail.add(trail) } else { s.trail.add(trail) }
    }
    s.last_target = Some(target.pos);
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
