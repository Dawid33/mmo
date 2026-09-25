//! Opt-in scripted flight, for crossing benchmarks with no human at the
//! keyboard. Once the world is ready it double-taps Space into flight, climbs
//! clear of the terrain, then:
//!
//! - `VOXEL_AUTOPILOT=1` (or `fly`): holds sprint + forward for good. A region
//!   seam — a handoff between two region threads, and a shift of the near
//!   window — is crossed every `REGION_SIZE` / `FLY_SPRINT_SPEED` seconds (~3 s
//!   at the defaults). `just fly-probe`.
//! - `VOXEL_AUTOPILOT=shuttle`: flies forward to the first boundary, then goes
//!   back and forth across that one plane, reversing `VOXEL_AUTOPILOT_OVERSHOOT_M`
//!   metres past it each leg. `VOXEL_AUTOPILOT_BOUNDARY=region` (default: the
//!   seam between two region threads — every leg is a handoff, and the near
//!   window releases and re-requests a row) or `chunk` (a 32-unit chunk edge
//!   inside one region: no handoff, just the per-chunk render work).
//!   `just shuttle-probe`.
//!
//! `VOXEL_AUTOPILOT_SECONDS=N` closes the client N seconds after the climb.
//!
//! Hands off the desktop: the pilot never touches the keyboard state or the
//! window's pointer. It sends its inputs straight into the sim's input stream
//! (the same `PlayerInput` events `forward_input` would), and it enables body
//! control with `InputEvent::FpsCam(true)` rather than the E toggle — that sets
//! the sim's fps-camera mode without the `SetFreeCam` that grabs the cursor.

use std::time::Duration;

use bevy::app::AppExit;
use bevy::prelude::*;
use game::{GameEventKind, InputEvent, Key};

use super::join_timeline::WorldReady;
use super::{GameEvents, LocalPlayer};

/// One scripted input.
#[derive(Clone, Copy)]
enum Step {
    FpsCam,
    Key(Key, bool),
}

/// Take-off, as (offset from world ready, input): fps-cam on, then two Space
/// taps inside `DOUBLE_TAP_WINDOW_MS`, each edge at least two ticks from the
/// last so the sim sees every press and release (`TICK_RATE` is 50 ms).
const TAKE_OFF: &[(u64, Step)] = &[
    (0, Step::FpsCam),
    (200, Step::Key(Key::Space, true)),
    (300, Step::Key(Key::Space, false)),
    (400, Step::Key(Key::Space, true)),
    // Hold the second tap: climb ~10 m at FLY_VERTICAL_SPEED.
    (1800, Step::Key(Key::Space, false)),
];
/// When the cruise keys go down, after the climb.
const CRUISE_AT_MS: u64 = 1950;

/// Default distance a shuttle leg runs past the boundary before reversing:
/// clear of the seam margin the handoff scan uses, short enough to cross often.
const SHUTTLE_OVERSHOOT_REGION_M: f32 = 6.0;
const SHUTTLE_OVERSHOOT_CHUNK_M: f32 = 1.0;

#[derive(Clone, Copy, Debug)]
enum Mode {
    Fly,
    /// Back and forth across one plane of a `cell`-unit grid.
    Shuttle { cell: f32, overshoot: f32 },
}

/// Shuttle progress: looking for the first boundary, then bouncing across it.
#[derive(Clone, Copy, Debug)]
enum Leg {
    Seek { start: IVec2 },
    /// `axis` 0 = x, 1 = z; `seam` in world units; `heading` is the sign of
    /// the current travel along the axis; `forward` whether W (else S) is held.
    Bounce { axis: usize, seam: f32, heading: f32, forward: bool },
}

#[derive(Resource)]
pub struct Autopilot {
    mode: Mode,
    /// Close the app this long after the cruise starts.
    cruise_for: Option<Duration>,
    started: Option<Duration>,
    next_step: usize,
    cruising: bool,
    leg: Option<Leg>,
    reversals: u32,
}

impl Autopilot {
    pub fn from_env() -> Option<Self> {
        let var = std::env::var("VOXEL_AUTOPILOT").ok().filter(|v| !v.is_empty() && v != "0")?;
        let mode = match var.as_str() {
            "shuttle" => {
                let chunk = std::env::var("VOXEL_AUTOPILOT_BOUNDARY").is_ok_and(|b| b == "chunk");
                let (cell, default_m) = if chunk {
                    (32.0, SHUTTLE_OVERSHOOT_CHUNK_M)
                } else {
                    (game::REGION_SIZE, SHUTTLE_OVERSHOOT_REGION_M)
                };
                let metres = std::env::var("VOXEL_AUTOPILOT_OVERSHOOT_M")
                    .ok()
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(default_m);
                Mode::Shuttle { cell, overshoot: metres * game::movement::UNITS_PER_METER }
            }
            _ => Mode::Fly,
        };
        let cruise_for = std::env::var("VOXEL_AUTOPILOT_SECONDS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs);
        Some(Self {
            mode,
            cruise_for,
            started: None,
            next_step: 0,
            cruising: false,
            leg: None,
            reversals: 0,
        })
    }
}

pub fn plugin(app: &mut App, autopilot: Autopilot) {
    info!("autopilot: {:?} once the world is ready", autopilot.mode);
    app.insert_resource(autopilot).add_systems(Update, drive_autopilot);
}

fn drive_autopilot(
    time: Res<Time<Real>>,
    ready: Res<WorldReady>,
    player: Res<LocalPlayer>,
    game: Res<GameEvents>,
    cameras: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    mut pilot: ResMut<Autopilot>,
    mut exit: MessageWriter<AppExit>,
) {
    let (true, Some(player)) = (ready.0, player.0) else {
        return;
    };
    let send = |input: InputEvent| {
        let _ = game.0.send(GameEventKind::PlayerInput(player, input));
    };
    let key = |key: Key, pressed: bool| send(InputEvent::Key { key, pressed });
    let now = time.elapsed();
    let started = *pilot.started.get_or_insert_with(|| {
        game::perf_event!("autopilot_start", "");
        now
    });
    let since = now - started;

    while let Some(&(at, step)) = TAKE_OFF.get(pilot.next_step) {
        if since < Duration::from_millis(at) {
            break;
        }
        match step {
            Step::FpsCam => send(InputEvent::FpsCam(true)),
            Step::Key(k, pressed) => key(k, pressed),
        }
        pilot.next_step += 1;
    }
    let cruise_at = Duration::from_millis(CRUISE_AT_MS);
    if since < cruise_at {
        return;
    }
    if !pilot.cruising {
        pilot.cruising = true;
        // Fly sprints; a chunk shuttle stays at plain flight speed so a leg
        // spans a few ticks.
        let sprint = match pilot.mode {
            Mode::Fly => true,
            Mode::Shuttle { cell, .. } => cell >= game::REGION_SIZE,
        };
        if sprint {
            key(Key::ControlLeft, true);
        }
        key(Key::KeyW, true);
    }

    if let Mode::Shuttle { cell, overshoot } = pilot.mode {
        let pos = cameras.iter().find(|(c, _)| c.is_active).map(|(_, t)| t.translation());
        if let Some(pos) = pos {
            let here = IVec2::new((pos.x / cell).floor() as i32, (pos.z / cell).floor() as i32);
            let at = [pos.x, pos.z];
            match pilot.leg {
                None => pilot.leg = Some(Leg::Seek { start: here }),
                Some(Leg::Seek { start }) if here != start => {
                    // First boundary: the plane between the two cells.
                    let axis = if here.x != start.x { 0 } else { 1 };
                    let (from, to) = if axis == 0 { (start.x, here.x) } else { (start.y, here.y) };
                    let seam = from.max(to) as f32 * cell;
                    let heading = (to - from).signum() as f32;
                    game::perf_event!(
                        "autopilot_shuttle",
                        "axis={} seam={} cell={} overshoot={}",
                        ["x", "z"][axis],
                        seam,
                        cell,
                        overshoot
                    );
                    pilot.leg = Some(Leg::Bounce { axis, seam, heading, forward: true });
                }
                Some(Leg::Bounce { axis, seam, heading, forward }) => {
                    if (at[axis] - seam) * heading >= overshoot {
                        // Past the far side: turn around (W <-> S).
                        key(if forward { Key::KeyW } else { Key::KeyS }, false);
                        key(if forward { Key::KeyS } else { Key::KeyW }, true);
                        pilot.reversals += 1;
                        pilot.leg = Some(Leg::Bounce { axis, seam, heading: -heading, forward: !forward });
                    }
                }
                _ => {}
            }
        }
    }

    if let Some(cruise) = pilot.cruise_for {
        if since >= cruise_at + cruise {
            game::perf_event!(
                "autopilot_done",
                "cruise_s={} reversals={}",
                cruise.as_secs(),
                pilot.reversals
            );
            exit.write(AppExit::Success);
        }
    }
}
