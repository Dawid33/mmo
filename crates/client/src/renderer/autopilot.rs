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
//!   window releases and re-requests a row) or `chunk` (a sim-chunk edge,
//!   `CHUNK_UNITS` — 256 units at 16-block chunks — inside one region: no
//!   handoff, just the crossing between two chunks' bodies and bricks).
//!   `just shuttle-probe`.
//! - `VOXEL_AUTOPILOT=shuttle` with `VOXEL_AUTOPILOT_BOUNDARY=host` or `thread`
//!   (the R3 go / no-go, `just go-nogo-r3`): picks the region seam nearest the
//!   start whose two sides belong to different hosts (`host`) or to one host
//!   (`thread`) — [`pick_loaded_seam`], from the client's own region → host map
//!   (`ClientUpdateEvent::RegionHosts`, built from its `ConnectionSet`; nothing is computed from
//!   host ids, which the store issues and never reuses) — with no such seam loaded it keeps
//!   flying forward and logs `autopilot_seam_wait` — turns to face the nearest point of that seam (away from its ends), flies
//!   there, turns square to it and shuttles across it as above.
//!
//! A new player is in a body, which cannot fly (`camera.rs`: flight needs a
//! mode other than Adventure). So before it holds any key the pilot writes one
//! `ToggleModeRequest` (the overview switch — the autopilot's alone, the
//! player has none) and waits until the overview is in.
//!
//! `VOXEL_AUTOPILOT_SECONDS=N` closes the client N seconds after the climb.
//!
//! Hands off the desktop: the pilot never touches the keyboard state or the
//! window's pointer. It sends its inputs straight into the sim's input stream
//! (the same `PlayerInput` events `forward_input` would), and it enables body
//! control with `InputEvent::FpsCam(true)` rather than the E toggle — that sets
//! the sim's fps-camera mode without the `SetFreeCam` that grabs the cursor.
//! It turns the same way: `InputEvent::MouseMotion` into the sim's look.

use std::time::Duration;

use bevy::app::AppExit;
use bevy::prelude::*;
use std::collections::BTreeMap;

use game::{GameEventKind, HostId, InputEvent, Key, RegionCoords};

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
/// How often the overview is asked for again while the body is still in Adventure (a
/// lost toggle would hold the probe before its start, where no deadline counts). The
/// ask names the target mode (`toggle_order` from the mode shown), so a repeat while
/// the first is in flight asks for the same thing.
const OVERVIEW_ASK_EVERY_MS: u64 = 2_000;

/// The seam shuttle logs `autopilot_seam_wait` at most this often while no seam of its kind
/// is loaded.
const SEAM_WAIT_LOG_MS: u64 = 5_000;

/// Which host serves each live region, as the client's own connection map says
/// (`ClientUpdateEvent::RegionHosts`, sent only while the autopilot runs).
#[derive(Resource, Default)]
pub struct RegionHosts(pub BTreeMap<RegionCoords, Option<HostId>>);

/// Default distance a shuttle leg runs past the boundary before reversing:
/// clear of the seam margin the handoff scan uses, short enough to cross often.
const SHUTTLE_OVERSHOOT_REGION_M: f32 = 6.0;
const SHUTTLE_OVERSHOOT_CHUNK_M: f32 = 1.0;

/// The seam shuttle is facing its goal once within this many degrees.
const TURN_DONE_DEG: f32 = 2.0;
/// A turn that leaves the heading error more than this times what it was went the
/// wrong way: the yaw sign is flipped.
const TURN_WRONG_WAY: f32 = 1.5;
/// While flying to the seam, a heading this far off its target point turns again.
const SEEK_REAIM_DEG: f32 = 10.0;
/// After a turn, the look is measured again only this much later (the input
/// goes through a tick, prediction and the render).
const TURN_SETTLE_MS: u64 = 300;
/// The seam shuttle aims at the point of the seam nearest it, kept this fraction
/// of a region away from either end of the seam.
const SEEK_EDGE_MARGIN: f32 = 0.25;
/// The seam shuttle has reached its point of the seam within this many metres.
const SEEK_ARRIVE_M: f32 = 8.0;

/// Which kind of region seam the seam shuttle crosses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SeamKind {
    /// Two regions of different hosts.
    Host,
    /// Two regions of one host (two of its region threads).
    Thread,
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Fly,
    /// Back and forth across one plane of a `cell`-unit grid.
    Shuttle { cell: f32, overshoot: f32 },
    /// Back and forth across one region seam of `kind` ([`pick_loaded_seam`]).
    Seam { kind: SeamKind, overshoot: f32 },
}

/// Shuttle progress: looking for the first boundary, then bouncing across it.
#[derive(Clone, Copy, Debug)]
enum Leg {
    Seek { start: IVec2 },
    /// The seam shuttle: turning to face `goal` (a horizontal world direction),
    /// then `then`.
    Turn { goal: Vec2, then: AfterTurn },
    /// The seam shuttle: flying (W held) to `target`, a point of the seam.
    Approach { target: Vec2, axis: usize, seam: f32, heading: f32 },
    /// `axis` 0 = x, 1 = z; `seam` in world units; `heading` is the sign of
    /// the current travel along the axis; `forward` whether W (else S) is held.
    Bounce { axis: usize, seam: f32, heading: f32, forward: bool },
}

/// What a [`Leg::Turn`] leads to.
#[derive(Clone, Copy, Debug)]
enum AfterTurn {
    Approach { target: Vec2, axis: usize, seam: f32, heading: f32 },
    Bounce { axis: usize, seam: f32, heading: f32 },
}

#[derive(Resource)]
pub struct Autopilot {
    mode: Mode,
    /// Close the app this long after the cruise starts.
    cruise_for: Option<Duration>,
    started: Option<Duration>,
    /// When the switch to the overview was last requested.
    overview_asked: Option<Duration>,
    next_step: usize,
    cruising: bool,
    leg: Option<Leg>,
    reversals: u32,
    /// The seam shuttle's last look input: when, and the heading error it corrected.
    last_turn: Option<(Duration, f32)>,
    /// Mouse x per radian of yaw, signed: `-1 / look_delta_rad(1)` by the sim's
    /// convention; flipped if a turn makes the error worse.
    px_per_rad: f32,
    /// The seam shuttle holds W (approaching the seam; [`bounce`] drives W / S after).
    w_held: bool,
    /// When the seam shuttle last logged its state (`autopilot_state`, every 2 s).
    last_state: Option<Duration>,
    /// Progress watch of the seam shuttle: since when, the last position, and the
    /// path flown since (a path length, not a displacement: the bounce returns
    /// to where it was every two legs).
    progress: Option<(Duration, Vec2, f32)>,
    /// Space is held (climbing out of terrain the body flew into) until then.
    climb_until: Option<Duration>,
    /// When the seam shuttle last logged `autopilot_seam_wait`.
    last_wait: Option<Duration>,
}

impl Autopilot {
    pub fn from_env() -> Option<Self> {
        let var = std::env::var("VOXEL_AUTOPILOT").ok().filter(|v| !v.is_empty() && v != "0")?;
        let mode = match var.as_str() {
            "shuttle" => {
                let boundary = std::env::var("VOXEL_AUTOPILOT_BOUNDARY").unwrap_or_default();
                let seam = match boundary.as_str() {
                    "host" => Some(SeamKind::Host),
                    "thread" => Some(SeamKind::Thread),
                    _ => None,
                };
                let (cell, default_m) = if boundary == "chunk" {
                    (game::CHUNK_UNITS as f32, SHUTTLE_OVERSHOOT_CHUNK_M)
                } else {
                    (game::REGION_SIZE, SHUTTLE_OVERSHOOT_REGION_M)
                };
                let metres = std::env::var("VOXEL_AUTOPILOT_OVERSHOOT_M")
                    .ok()
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(default_m);
                let overshoot = metres * game::movement::UNITS_PER_METER;
                match seam {
                    None => Mode::Shuttle { cell, overshoot },
                    Some(kind) => Mode::Seam { kind, overshoot },
                }
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
            overview_asked: None,
            next_step: 0,
            cruising: false,
            leg: None,
            reversals: 0,
            last_turn: None,
            px_per_rad: -1.0 / game::look_delta_rad(1.0, game::LOOK_BASE_X),
            w_held: false,
            last_state: None,
            progress: None,
            climb_until: None,
            last_wait: None,
        })
    }
}

pub fn plugin(app: &mut App, autopilot: Autopilot) {
    info!("autopilot: {:?} once the world is ready", autopilot.mode);
    use crate::session::SessionResetExt as _;
    // The map is per world (`session.rs`'s lifecycle rule): a next world starts it empty.
    app.insert_resource(autopilot)
        .add_session_reset::<RegionHosts>()
        .add_systems(Update, drive_autopilot.run_if(crate::session::world_systems_enabled()));
}

fn drive_autopilot(
    time: Res<Time<Real>>,
    ready: Res<WorldReady>,
    player: Res<LocalPlayer>,
    game: Res<GameEvents>,
    roots: Res<super::bridge::RegionRoots>,
    region_hosts: Res<RegionHosts>,
    // The body's camera (it carries the sim pose), not the frontend's: an online
    // session keeps the menu's camera entity around, parked at the origin.
    cameras: Query<(&Camera, &GlobalTransform), (With<Camera3d>, With<super::bridge::SimTarget>)>,
    mode: Res<super::commander::adventure::ClientPlayMode>,
    mut toggle: MessageWriter<super::commander::adventure::ToggleModeRequest>,
    mut pilot: ResMut<Autopilot>,
    mut exit: MessageWriter<AppExit>,
) {
    // The run's deadline holds even while the world is not ready: a region
    // flip mid-flight clears `WorldReady`, and in cell mode a neighbouring
    // region never becomes ready — the probe then flew on for minutes.
    let now = time.elapsed();
    if let (Some(started), Some(cruise)) = (pilot.started, pilot.cruise_for) {
        if now - started >= Duration::from_millis(CRUISE_AT_MS) + cruise {
            game::perf_event!(
                "autopilot_done",
                "cruise_s={} reversals={} ready={}",
                cruise.as_secs(),
                pilot.reversals,
                ready.0
            );
            exit.write(AppExit::Success);
            return;
        }
    }
    let (true, Some(player)) = (ready.0, player.0) else {
        return;
    };
    // The body cannot fly: ask for the overview (again every OVERVIEW_ASK_EVERY_MS),
    // and start only when it is in.
    if mode.adventure() {
        let due = pilot
            .overview_asked
            .is_none_or(|at| now.saturating_sub(at) >= Duration::from_millis(OVERVIEW_ASK_EVERY_MS));
        if due && pilot.started.is_none() {
            pilot.overview_asked = Some(now);
            toggle.write(super::commander::adventure::ToggleModeRequest);
        }
        if pilot.started.is_none() {
            return;
        }
    }
    let send = |input: InputEvent| {
        let _ = game.0.send(GameEventKind::PlayerInput(player, input));
    };
    let key = |key: Key, pressed: bool| send(InputEvent::Key { key, pressed });
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
        // spans a few ticks. The seam shuttle holds W itself (not while turning).
        let (sprint, forward) = match pilot.mode {
            Mode::Fly => (true, true),
            Mode::Shuttle { cell, .. } => (cell >= game::REGION_SIZE, true),
            Mode::Seam { .. } => (true, false),
        };
        if sprint {
            key(Key::ControlLeft, true);
        }
        if forward {
            key(Key::KeyW, true);
        }
    }

    // The active one of the body's cameras (a crossing briefly has two), else any:
    // while the frontend still holds the screen the body's camera is inactive.
    let camera = cameras.iter().find(|(c, _)| c.is_active).or_else(|| cameras.iter().next()).map(|(_, t)| *t);
    match pilot.mode {
        Mode::Fly => {}
        Mode::Shuttle { cell, overshoot } => {
            if let Some(t) = camera {
                shuttle(&mut pilot, t.translation(), cell, overshoot, &key);
            }
        }
        Mode::Seam { kind, overshoot } => {
            if let Some(t) = camera {
                let loaded: Vec<(RegionCoords, Option<HostId>)> =
                    roots.0.keys().map(|rc| (*rc, region_hosts.0.get(rc).copied().flatten())).collect();
                seam_shuttle(&mut pilot, now, &t, &loaded, kind, overshoot, &key, &send);
            }
        }
    }
}

/// Reverses (W <-> S) once `at` is `overshoot` past `seam` along the leg's heading.
fn bounce(pilot: &mut Autopilot, at: [f32; 2], overshoot: f32, key: &impl Fn(Key, bool)) {
    let Some(Leg::Bounce { axis, seam, heading, forward }) = pilot.leg else {
        return;
    };
    if (at[axis] - seam) * heading >= overshoot {
        // Past the far side: turn around (W <-> S).
        key(if forward { Key::KeyW } else { Key::KeyS }, false);
        key(if forward { Key::KeyS } else { Key::KeyW }, true);
        pilot.reversals += 1;
        pilot.leg = Some(Leg::Bounce { axis, seam, heading: -heading, forward: !forward });
    }
}

/// `VOXEL_AUTOPILOT=shuttle` across the first `cell` boundary met.
fn shuttle(pilot: &mut Autopilot, pos: Vec3, cell: f32, overshoot: f32, key: &impl Fn(Key, bool)) {
    let here = IVec2::new((pos.x / cell).floor() as i32, (pos.z / cell).floor() as i32);
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
        Some(Leg::Bounce { .. }) => bounce(pilot, [pos.x, pos.z], overshoot, key),
        _ => {}
    }
}

/// The signed yaw (radians, about +Y, right-handed) that turns horizontal
/// direction `from` onto `to`; 0 when either is degenerate.
fn yaw_error(from: Vec2, to: Vec2) -> f32 {
    if from.length_squared() < 1e-12 || to.length_squared() < 1e-12 {
        return 0.0;
    }
    // In the x/z plane a rotation by +a about +Y takes (x, z) to
    // (x cos a + z sin a, -x sin a + z cos a): -Z turns toward -X.
    let cross_y = from.y * to.x - from.x * to.y;
    let dot = from.dot(to);
    cross_y.atan2(dot)
}

/// `VOXEL_AUTOPILOT=shuttle` across the [`pick_loaded_seam`] seam of `kind`.
#[allow(clippy::too_many_arguments)]
fn seam_shuttle(
    pilot: &mut Autopilot,
    now: Duration,
    camera: &GlobalTransform,
    loaded: &[(RegionCoords, Option<HostId>)],
    kind: SeamKind,
    overshoot: f32,
    key: &impl Fn(Key, bool),
    send: &impl Fn(InputEvent),
) {
    let pos = camera.translation();
    let at = Vec2::new(pos.x, pos.z);
    let facing = {
        let f = camera.forward();
        Vec2::new(f.x, f.z)
    };
    // Low-rate trace of what the pilot is doing: a probe that never crosses is
    // otherwise silent.
    if pilot.last_state.is_none_or(|t| now - t >= Duration::from_secs(2)) {
        pilot.last_state = Some(now);
        game::perf_event!(
            "autopilot_state",
            "pos={:.0},{:.0},{:.0} facing={:.2},{:.2} leg={:?} w={} px_per_rad={:.1} reversals={}",
            pos.x,
            pos.y,
            pos.z,
            facing.x,
            facing.y,
            pilot.leg,
            pilot.w_held,
            pilot.px_per_rad,
            pilot.reversals
        );
    }
    // Flying level into rising ground stops the body dead: when a leg that should
    // be moving covers under a metre of path in two seconds, climb for a while.
    if pilot.climb_until.is_some_and(|until| now >= until) {
        key(Key::Space, false);
        pilot.climb_until = None;
        pilot.progress = None;
    }
    let moving = pilot.w_held || matches!(pilot.leg, Some(Leg::Bounce { .. }));
    if !moving || pilot.climb_until.is_some() {
        pilot.progress = None;
    } else {
        let (since, last, path) = pilot.progress.unwrap_or((now, at, 0.0));
        let path = path + (at - last).length();
        if now - since >= Duration::from_millis(STUCK_WINDOW_MS) {
            if path < STUCK_PATH_M * game::movement::UNITS_PER_METER {
                game::perf_event!("autopilot_climb", "pos={:.0},{:.0},{:.0} path={:.1}", pos.x, pos.y, pos.z, path);
                key(Key::Space, true);
                pilot.climb_until = Some(now + Duration::from_millis(STUCK_CLIMB_MS));
            }
            pilot.progress = Some((now, at, 0.0));
        } else {
            pilot.progress = Some((since, at, path));
        }
    }
    let hold_w = |pilot: &mut Autopilot, on: bool| {
        if pilot.w_held != on {
            key(Key::KeyW, on);
            pilot.w_held = on;
        }
    };
    match pilot.leg {
        None => {
            let size = game::REGION_SIZE;
            let home = RegionCoords::new((pos.x / size).floor() as i32, (pos.z / size).floor() as i32);
            // Among the regions the client has: in fortress play the window stays on the
            // fortress, and a seam with an unloaded side is a wall the body stops at.
            let Some((a, b)) = pick_loaded_seam(home, loaded, kind == SeamKind::Host) else {
                // No seam of the wanted kind among the regions the client holds yet: keep
                // flying outward (never a computed owner) and say so, rarely.
                hold_w(pilot, true);
                if pilot.last_wait.is_none_or(|t| now - t >= Duration::from_millis(SEAM_WAIT_LOG_MS)) {
                    pilot.last_wait = Some(now);
                    game::perf_event!(
                        "autopilot_seam_wait",
                        "boundary={} home={},{} loaded={}",
                        if kind == SeamKind::Host { "host" } else { "thread" },
                        home.x,
                        home.z,
                        loaded.len()
                    );
                }
                return;
            };
            // `b` is `a` plus one along the seam's axis (pick_loaded_seam's order).
            let axis = if b.x != a.x { 0 } else { 1 };
            // The point of the seam nearest the body, kept to the middle half of the
            // edge (clear of the corners and the regions beyond them).
            let along = |lo: i32, p: f32| {
                p.clamp((lo as f32 + SEEK_EDGE_MARGIN) * size, (lo as f32 + 1.0 - SEEK_EDGE_MARGIN) * size)
            };
            let (seam, target) = if axis == 0 {
                let seam = b.x as f32 * size;
                (seam, Vec2::new(seam, along(a.z, at.y)))
            } else {
                let seam = b.z as f32 * size;
                (seam, Vec2::new(along(a.x, at.x), seam))
            };
            // Cross toward the far side of the seam first.
            let heading = if [at.x, at.y][axis] < seam { 1.0 } else { -1.0 };
            let owner = |rc: RegionCoords| {
                loaded.iter().find(|(r, _)| *r == rc).and_then(|(_, h)| *h).map_or(-1, |h| h.0 as i64)
            };
            game::perf_event!(
                "autopilot_seam",
                "boundary={} home={},{} a={},{} b={},{} owner_a={} owner_b={}",
                if kind == SeamKind::Host { "host" } else { "thread" },
                home.x,
                home.z,
                a.x,
                a.z,
                b.x,
                b.z,
                owner(a),
                owner(b)
            );
            pilot.leg = Some(Leg::Turn {
                goal: target - at,
                then: AfterTurn::Approach { target, axis, seam, heading },
            });
        }
        Some(Leg::Turn { goal, then }) => {
            hold_w(pilot, false);
            if pilot.last_turn.is_some_and(|(t, _)| now - t < Duration::from_millis(TURN_SETTLE_MS)) {
                return;
            }
            let goal = match then {
                // Re-aim at the target point as the body drifts.
                AfterTurn::Approach { target, .. } => target - at,
                AfterTurn::Bounce { .. } => goal,
            };
            let error = yaw_error(facing, goal);
            if error.abs() <= TURN_DONE_DEG.to_radians() {
                pilot.last_turn = None;
                match then {
                    AfterTurn::Approach { target, axis, seam, heading } => {
                        pilot.leg = Some(Leg::Approach { target, axis, seam, heading });
                    }
                    AfterTurn::Bounce { axis, seam, heading } => {
                        game::perf_event!(
                            "autopilot_shuttle",
                            "axis={} seam={} cell={} overshoot={}",
                            ["x", "z"][axis],
                            seam,
                            game::REGION_SIZE,
                            overshoot
                        );
                        // Facing the seam square on: the first leg goes forward (W).
                        hold_w(pilot, true);
                        pilot.leg = Some(Leg::Bounce { axis, seam, heading, forward: true });
                    }
                }
                return;
            }
            // The last turn made it clearly worse (about doubled, not just a look that has
            // not caught up yet): the sign convention is the other way round.
            if pilot.last_turn.is_some_and(|(_, before)| error.abs() > TURN_WRONG_WAY * before.abs()) {
                pilot.px_per_rad = -pilot.px_per_rad;
            }
            send(InputEvent::MouseMotion { dx: error * pilot.px_per_rad, dy: 0.0 });
            pilot.last_turn = Some((now, error));
        }
        Some(Leg::Approach { target, axis, seam, heading }) => {
            let to_go = target - at;
            if to_go.length() <= SEEK_ARRIVE_M * game::movement::UNITS_PER_METER {
                hold_w(pilot, false);
                let mut goal = Vec2::ZERO;
                goal[axis] = heading;
                pilot.leg = Some(Leg::Turn { goal, then: AfterTurn::Bounce { axis, seam, heading } });
                return;
            }
            if yaw_error(facing, to_go).abs() > SEEK_REAIM_DEG.to_radians() {
                hold_w(pilot, false);
                pilot.leg = Some(Leg::Turn { goal: to_go, then: AfterTurn::Approach { target, axis, seam, heading } });
                return;
            }
            hold_w(pilot, true);
        }
        // `bounce` drives W / S from here on.
        Some(Leg::Bounce { .. }) => bounce(pilot, [at.x, at.y], overshoot, key),
        Some(Leg::Seek { .. }) => {}
    }
}

/// The seam shuttle's progress watch: its window, the path that counts as moving, and
/// how long it then holds Space (flight climbs while it is held).
const STUCK_WINDOW_MS: u64 = 2_000;
const STUCK_PATH_M: f32 = 1.0;
const STUCK_CLIMB_MS: u64 = 1_500;

/// The seam nearest `home` whose two sides are both loaded, both with a known host, and on
/// different hosts (`want_host_seam`) or on the same one, by the client's own region → host
/// map alone. Returns `(a, b)` with `b` one region past `a` in +x or +z. "Nearest": by the
/// distance from the middle of `home` to the middle of the seam, ties to the lower `a`
/// (x, then z), then x before z — the same answer every time. `None` when the loaded regions
/// hold no such seam: nothing is invented from host ids.
pub fn pick_loaded_seam(
    home: RegionCoords,
    loaded: &[(RegionCoords, Option<HostId>)],
    want_host_seam: bool,
) -> Option<(RegionCoords, RegionCoords)> {
    let host_of = |rc: RegionCoords| loaded.iter().find(|(r, _)| *r == rc).and_then(|(_, h)| *h);
    let mut best: Option<(i64, (i32, i32, u8), (RegionCoords, RegionCoords))> = None;
    for &(a, ha) in loaded {
        let Some(ha) = ha else { continue };
        for (axis, b) in [(0u8, RegionCoords::new(a.x + 1, a.z)), (1u8, RegionCoords::new(a.x, a.z + 1))] {
            let Some(hb) = host_of(b) else { continue };
            if (ha != hb) != want_host_seam {
                continue;
            }
            // Twice the offset from home's middle to the seam's middle.
            let (dx, dz) = (i64::from(a.x + b.x - 2 * home.x), i64::from(a.z + b.z - 2 * home.z));
            let candidate = (dx * dx + dz * dz, (a.x, a.z, axis), (a, b));
            if best.as_ref().is_none_or(|best| (candidate.0, candidate.1) < (best.0, best.1)) {
                best = Some(candidate);
            }
        }
    }
    best.map(|(_, _, seam)| seam)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[((i32, i32), u64)]) -> Vec<(RegionCoords, Option<HostId>)> {
        entries.iter().map(|&((x, z), h)| (RegionCoords::new(x, z), Some(HostId(h)))).collect()
    }

    #[test]
    fn a_host_seam_is_where_the_map_changes_host() {
        let loaded = map(&[((0, 0), 1), ((1, 0), 2), ((0, 1), 1)]);
        let home = RegionCoords::new(0, 0);
        let (a, b) = (RegionCoords::new(0, 0), RegionCoords::new(1, 0));
        assert_eq!(pick_loaded_seam(home, &loaded, true), Some((a, b)));
        let (a, b) = (RegionCoords::new(0, 0), RegionCoords::new(0, 1));
        assert_eq!(pick_loaded_seam(home, &loaded, false), Some((a, b)));
    }

    #[test]
    fn no_seam_is_invented_from_ids() {
        let loaded = map(&[((0, 0), 7), ((1, 0), 7), ((0, 1), 7), ((1, 1), 7)]);
        assert_eq!(pick_loaded_seam(RegionCoords::new(0, 0), &loaded, true), None);
        // A side whose host is not known yet is no seam of either kind.
        let unknown = vec![(RegionCoords::new(0, 0), Some(HostId(7))), (RegionCoords::new(1, 0), None)];
        assert_eq!(pick_loaded_seam(RegionCoords::new(0, 0), &unknown, true), None);
        assert_eq!(pick_loaded_seam(RegionCoords::new(0, 0), &unknown, false), None);
    }

    /// A positive yaw (about +Y) turns -Z (Bevy's forward) toward -X.
    #[test]
    fn yaw_error_sign() {
        let forward = Vec2::new(0.0, -1.0);
        assert!((yaw_error(forward, Vec2::new(-1.0, 0.0)) - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        assert!((yaw_error(forward, Vec2::new(1.0, 0.0)) + std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        assert_eq!(yaw_error(forward, forward), 0.0);
        assert_eq!(yaw_error(forward, Vec2::ZERO), 0.0);
    }
}
