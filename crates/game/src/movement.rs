//! Minecraft-creative-style body movement: walking under gravity with jump
//! and step-up, plus double-tap-jump flying. Entity-generic — the per-entity
//! state is the [`Motion`] component and the per-tick intent is [`MoveKeys`],
//! which players fill from their input and NPCs can fill from anything.
//!
//! Everything here is `+ - *` on f32 (no transcendental calls), so it is
//! bit-identical across machines; the yaw rotation goes through the
//! libm-forced `yaw_quat`, as the look code already does.

use rapier3d::control::{CharacterAutostep, CharacterLength, KinematicCharacterController};
use rapier3d::math::{Pose, Vector};
use rapier3d::parry::query::ShapeCastOptions;
use rapier3d::parry::shape::Shape;
use rapier3d::pipeline::QueryPipeline;

use crate::{BLOCK_VOXELS, TICK_RATE};

// Tunables, in metres and seconds (1 m = 1 block = `BLOCK_VOXELS` units), so
// they keep their meaning when TICK_RATE is retuned.
/// World units per metre.
pub const UNITS_PER_METER: f32 = BLOCK_VOXELS as f32;
/// Seconds per simulation tick.
pub const TICK_DT: f32 = TICK_RATE as f32 / 1000.0;
/// Ground speed (Minecraft walk: 4.317 m/s).
pub const WALK_SPEED: f32 = 4.317;
/// Ground speed while sprinting: Minecraft's 1.3× the walk (5.612 m/s).
/// Held Ctrl or a double-tapped W (`Key::Sprint`).
pub const SPRINT_SPEED: f32 = 1.3 * WALK_SPEED;
/// Horizontal creative-flight speed (Minecraft: 10.92 m/s).
pub const FLY_SPEED: f32 = 10.92;
/// Horizontal creative-flight speed while sprinting (Minecraft: 21.6 m/s).
pub const FLY_SPRINT_SPEED: f32 = 21.6;
/// Ascend / descend speed while flying.
pub const FLY_VERTICAL_SPEED: f32 = 7.5;
// --- Minecraft's walking model (`plan_with`) -------------------------------
// Minecraft steps entities at 20 ticks/s and states its constants per tick
// (blocks/tick, blocks/tick²); ours are the same numbers in m/s at our 50 ms
// tick. The per-tick friction and drag factors only mean the same thing at
// that period, so a retuned TICK_RATE must retune them.
const _: () = assert!(
    TICK_RATE == 50,
    "movement's Minecraft constants are per 50 ms tick"
);

/// Downward acceleration, m/s²: Minecraft's 0.08 blocks/tick² at 20 ticks/s.
/// The world's gravity — players, slimes, item bodies and voxel bodies all
/// fall under it (`body::BODY_GRAVITY`, `voxel_body::VOXEL_BODY_GRAVITY`).
pub const GRAVITY: f32 = 32.0;
/// Vertical drag of a walker, per tick: after gravity, `vel_y *= AIR_DRAG`
/// (Minecraft: 0.98). With gravity it sets a terminal 3.92 blocks/tick.
pub const AIR_DRAG: f32 = 0.98;
/// Fall speed cap (Minecraft's terminal velocity: 78.4 m/s, what
/// [`GRAVITY`] and [`AIR_DRAG`] converge to; a clamp for anything faster).
pub const TERMINAL_SPEED: f32 = 78.4;
/// Launch speed of a jump, m/s (Minecraft: 0.42 blocks/tick). Under
/// [`GRAVITY`] and [`AIR_DRAG`] it peaks at 1.252 m and lasts 12 ticks on
/// flat ground.
pub const JUMP_SPEED: f32 = 8.4;
/// Holding jump jumps at most once per this many ticks; letting go resets
/// it (Minecraft's `jumpTicks`: 10).
pub const JUMP_COOLDOWN_TICKS: u8 = 10;
/// Forward push of a sprinting jump, m/s, along the facing (Minecraft: 0.2
/// blocks/tick): what makes sprint-jumping faster than sprinting.
pub const SPRINT_JUMP_BOOST: f32 = 4.0;
/// Horizontal velocity kept per tick on the ground (Minecraft: a block's
/// slipperiness 0.6 × 0.91).
pub const GROUND_FRICTION: f32 = 0.546;
/// Horizontal velocity kept per tick in the air (Minecraft: 0.91).
pub const AIR_FRICTION: f32 = 0.91;
/// Airborne steering as a share of the ground's (Minecraft: 0.02 / 0.1):
/// a jump keeps the speed it left with and is only nudged.
pub const AIR_ACCEL_FACTOR: f32 = 0.2;
/// A velocity component below this (m/s; Minecraft 1.9+: 0.003 blocks/tick)
/// is zeroed at the start of a tick.
pub const MOMENTUM_EPSILON: f32 = 0.003 / TICK_DT;
/// Two jump taps within this window toggle flying (Minecraft: 7 ticks).
pub const DOUBLE_TAP_WINDOW_MS: u64 = 350;
/// Ledges up to this height are walked up without jumping (Minecraft: 0.6 m).
pub const STEP_HEIGHT: f32 = 0.6;
/// The tread an autostep needs past a ledge, metres. Below the 0.5 m tread of
/// a terrain slope's upper half; rapier's `Relative(0.5)` evaluated against the
/// capsule AABB's horizontal DIAGONAL (~0.58 m) and refused it.
pub const AUTOSTEP_MIN_WIDTH_M: f32 = 0.3;
/// A walking body within this distance above the ground is pulled down onto
/// it, so stepping down stairs does not go airborne. As tall as `STEP_HEIGHT`:
/// what a body walks up it also walks down — at 0.25 m a terrain slope's
/// 0.5 m half-steps went airborne for 3-4 ticks.
pub const GROUND_SNAP: f32 = 0.6;

/// Horizontal swimming speed (at [`Immersion::Waist`] or deeper).
pub const SWIM_SPEED: f32 = 2.2;
/// Horizontal swimming speed while sprinting.
pub const SWIM_SPRINT_SPEED: f32 = 3.0;
/// Walking speed multiplier with only the feet in water ([`Immersion::Feet`]).
pub const WADE_SPEED_FACTOR: f32 = 0.75;
/// Rise speed while swimming with jump held, head under.
pub const SWIM_UP_SPEED: f32 = 2.4;
/// Dive speed while swimming with descend held.
pub const SWIM_DOWN_SPEED: f32 = 2.4;
/// Idle swimmers rise back to the surface: the rise speed, head under, with
/// neither jump nor descend held.
pub const SWIM_FLOAT_UP_SPEED: f32 = 1.0;
/// Rate (m/s²) at which a swimmer's vertical speed approaches its target.
/// As strong as [`GRAVITY`]: water stops a fall as hard as gravity starts
/// one, so a body walking off a bank into four-deep water (~13.5 m/s when
/// its waist is found wet) stops ~1.5 m above the bed. At 12 m/s² it hit the
/// bed (stopping took ~7.5 m); 40 still came within 0.32 m of it.
pub const SWIM_VERTICAL_ACCEL: f32 = 45.0;
/// Upward speed a swimmer at the surface ([`Immersion::Waist`]) holding jump
/// against a wall gets: climbs out onto a one-block ledge.
pub const SWIM_EXIT_SPEED: f32 = 9.0;

/// Standing height of a player body (Minecraft: 1.8 m). It fills two blocks
/// while still fitting through a two-block-high gap: a body of exactly 2 m
/// could not, because the character controller keeps a skin offset.
pub const PLAYER_HEIGHT: f32 = 1.8;
/// Radius of a player body; the body is `2 * PLAYER_RADIUS` wide.
pub const PLAYER_RADIUS: f32 = 0.4;
/// Eye height above the feet (Minecraft: 1.62 m).
pub const PLAYER_EYE_HEIGHT: f32 = 1.62;
/// Capsule radius in world units.
pub const PLAYER_CAPSULE_RADIUS: f32 = PLAYER_RADIUS * UNITS_PER_METER;
/// Capsule half-height (the cylinder part only) in world units:
/// total height = `2 * (half_height + radius)`.
pub const PLAYER_CAPSULE_HALF_HEIGHT: f32 =
    PLAYER_HEIGHT * UNITS_PER_METER / 2.0 - PLAYER_CAPSULE_RADIUS;
/// Eye position above the body origin (the capsule centre), in world units.
/// The sim body pose is the capsule centre; cameras and view rays add this.
pub const PLAYER_EYE_OFFSET: f32 = (PLAYER_EYE_HEIGHT - PLAYER_HEIGHT / 2.0) * UNITS_PER_METER;

/// Gap the character controller keeps between a body and the terrain, in world
/// units. Absolute, not rapier's default of 1% of the body height: the voxel
/// seam fixes and their tests (`shape_cast_grazing.rs`) are pinned to this
/// skin, so it must not change when the body is resized.
pub const CHARACTER_SKIN: f32 = 0.216;

/// Rapier's `IntegrationParameters::length_unit` (plan D6): world units per
/// metre, so rapier's metre-scaled tolerances (contact prediction, sleep
/// thresholds, contact recycling) mean what they say for the dynamic bodies
/// to come. Measured neutral for today's fixed/kinematic-only world.
pub const PHYSICS_LENGTH_UNIT: f32 = UNITS_PER_METER;

/// Per-entity movement state. Lives in the rollback ECS (`Ecs::motion`) and
/// rides `EntityBundle` across region handoffs.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    state_hash::StateHash,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct Motion {
    /// Vertical velocity in units/s; only meaningful while walking.
    pub vel_y: f32,
    /// Standing on something as of the last tick.
    pub grounded: bool,
    /// Creative flight: no gravity, jump/descend move vertically.
    pub flying: bool,
    /// Tick of an unanswered jump tap, for the double-tap fly toggle.
    /// u64, not usize: this is serialized across the 64-bit↔wasm32 seam.
    pub last_jump_tap: Option<u64>,
    /// How deep the body is in water, as of the start of this tick
    /// (`Rollback::tick_immersion_safe`).
    pub immersion: Immersion,
    /// The last tick's horizontal move was blocked (it made less than half
    /// the distance it wanted): a swimmer holding jump climbs out.
    pub blocked_horizontally: bool,
    /// Horizontal velocity in units/s, world frame — the momentum of a
    /// [`MoveParams::momentum`] walker (0 for the others).
    pub vel_x: f32,
    pub vel_z: f32,
    /// Ticks until a held jump may jump again ([`JUMP_COOLDOWN_TICKS`]).
    pub jump_cooldown: u8,
}

/// How deep a body is in water: the deepest of its feet, waist and eye that
/// is in a wet cell (`crate::immersion_at`).
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    state_hash::StateHash,
)]
pub enum Immersion {
    #[default]
    Dry,
    /// Only the feet are wet: the body wades.
    Feet,
    /// The waist is wet, the eye is not: the body swims at the surface.
    Waist,
    /// The eye is under.
    Head,
}

/// One tick of movement intent, already reduced from whatever produced it.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct MoveKeys {
    pub forward: bool,
    pub back: bool,
    pub left: bool,
    pub right: bool,
    /// Jump went down this tick (edge) — drives the double-tap.
    pub jump_pressed: bool,
    /// Jump is down — jumps when grounded, ascends when flying.
    pub jump_held: bool,
    pub descend: bool,
    pub sprint: bool,
}

/// The per-kind walking tunables `plan` reads. Flight and sprint stay
/// player-only constants: nothing else flies or sprints yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveParams {
    /// Ground speed, m/s.
    pub walk_speed: f32,
    /// Launch speed of a jump, m/s.
    pub jump_speed: f32,
    /// Minecraft's horizontal momentum: keys accelerate a velocity that
    /// friction bleeds off (on the ground fast, in the air slowly, steering
    /// weak), and a sprinting jump is pushed forward. Off: the keys set the
    /// horizontal speed outright (slimes, which steer only their hops).
    pub momentum: bool,
    /// Whether a double tap of jump starts flight. Off for Adventure
    /// (survival) bodies, which also drop out of flight.
    pub allow_flight: bool,
}

impl MoveParams {
    pub const PLAYER: MoveParams = MoveParams {
        walk_speed: WALK_SPEED,
        jump_speed: JUMP_SPEED,
        momentum: true,
        allow_flight: true,
    };
}

/// Advances `motion` by one tick of intent and returns the translation the
/// body *wants* this tick (world units, before collision). Feed the
/// collision-corrected result back through [`settle`].
pub fn plan(motion: &mut Motion, keys: &MoveKeys, yaw: f32, tick: u64) -> Vector {
    plan_with(motion, keys, yaw, tick, &MoveParams::PLAYER)
}

/// [`plan`] with explicit per-kind tunables.
pub fn plan_with(
    motion: &mut Motion,
    keys: &MoveKeys,
    yaw: f32,
    tick: u64,
    params: &MoveParams,
) -> Vector {
    // Flying ignores water; a swimmer's jump swims, never toggles flight.
    let swimming = !motion.flying && motion.immersion >= Immersion::Waist;
    if !params.allow_flight {
        motion.flying = false;
        motion.last_jump_tap = None;
    } else if keys.jump_pressed && !swimming {
        let window = DOUBLE_TAP_WINDOW_MS / TICK_RATE;
        match motion.last_jump_tap {
            Some(t) if tick.saturating_sub(t) <= window => {
                motion.flying = !motion.flying;
                motion.vel_y = 0.0;
                motion.last_jump_tap = None;
            }
            _ => motion.last_jump_tap = Some(tick),
        }
    }

    let axis = |pos: bool, neg: bool| (pos as i8 - neg as i8) as f32;
    let (mut x, mut z) = (axis(keys.right, keys.left), axis(keys.back, keys.forward));
    if x != 0.0 && z != 0.0 {
        // Diagonals are not faster than straight lines.
        x *= std::f32::consts::FRAC_1_SQRT_2;
        z *= std::f32::consts::FRAC_1_SQRT_2;
    }
    // Sprinting needs forward motion, as in Minecraft.
    let sprint = keys.sprint && keys.forward && !keys.back;
    let land = if sprint {
        SPRINT_SPEED
    } else {
        params.walk_speed
    };
    let speed = if motion.flying {
        if sprint { FLY_SPRINT_SPEED } else { FLY_SPEED }
    } else if swimming {
        // Never faster than the body moves on land (slow movers swim slow).
        (if sprint {
            SWIM_SPRINT_SPEED
        } else {
            SWIM_SPEED
        })
        .min(land)
    } else if motion.immersion == Immersion::Feet {
        land * WADE_SPEED_FACTOR
    } else {
        land
    };
    let step = speed * UNITS_PER_METER * TICK_DT;
    // Horizontal movement follows yaw only — speed stays full regardless of
    // pitch (Minecraft behavior).
    let wish = crate::yaw_quat(yaw) * Vector::new(x, 0.0, z);
    let walking = !motion.flying && !swimming;

    // Minecraft's tick: tiny velocities zeroed, the jump, the keys'
    // acceleration, the move by the velocity — and only then gravity, drag
    // and friction.
    for v in [&mut motion.vel_x, &mut motion.vel_y, &mut motion.vel_z] {
        if v.abs() < MOMENTUM_EPSILON * UNITS_PER_METER {
            *v = 0.0;
        }
    }
    let mut jumped = false;
    if walking {
        motion.jump_cooldown = motion.jump_cooldown.saturating_sub(1);
        if !keys.jump_held {
            motion.jump_cooldown = 0;
        } else if motion.grounded && motion.jump_cooldown == 0 {
            motion.vel_y = params.jump_speed * UNITS_PER_METER;
            motion.jump_cooldown = JUMP_COOLDOWN_TICKS;
            jumped = true;
        }
    } else {
        motion.jump_cooldown = 0;
    }

    let mut desired = if walking && params.momentum {
        if jumped && sprint {
            let facing = crate::yaw_quat(yaw) * Vector::new(0.0, 0.0, -1.0);
            motion.vel_x += facing.x * SPRINT_JUMP_BOOST * UNITS_PER_METER;
            motion.vel_z += facing.z * SPRINT_JUMP_BOOST * UNITS_PER_METER;
        }
        // The acceleration whose steady state under ground friction is
        // `speed`: Minecraft's 0.1 × 0.98 for its 0.216 blocks/tick walk.
        let (accel, friction) = if motion.grounded {
            (1.0, GROUND_FRICTION)
        } else {
            (AIR_ACCEL_FACTOR, AIR_FRICTION)
        };
        let accel = speed * UNITS_PER_METER * (1.0 - GROUND_FRICTION) * accel;
        motion.vel_x += wish.x * accel;
        motion.vel_z += wish.z * accel;
        let desired = Vector::new(motion.vel_x * TICK_DT, 0.0, motion.vel_z * TICK_DT);
        motion.vel_x *= friction;
        motion.vel_z *= friction;
        desired
    } else {
        // Set outright; a body that stops flying or swimming goes on with it.
        let desired = wish * step;
        if params.momentum {
            (motion.vel_x, motion.vel_z) = (desired.x / TICK_DT, desired.z / TICK_DT);
        }
        desired
    };

    let dy = if motion.flying {
        axis(keys.jump_held, keys.descend) * FLY_VERTICAL_SPEED * UNITS_PER_METER * TICK_DT
    } else if swimming {
        let target = match motion.immersion {
            Immersion::Head if keys.jump_held => SWIM_UP_SPEED,
            Immersion::Head if keys.descend => -SWIM_DOWN_SPEED,
            Immersion::Head => SWIM_FLOAT_UP_SPEED,
            // At the surface: jump holds it there (and climbs out below),
            // descend dives, neither floats.
            _ if keys.descend && !keys.jump_held => -SWIM_DOWN_SPEED,
            _ => 0.0,
        } * UNITS_PER_METER;
        let (v0, v1) = if motion.immersion == Immersion::Waist
            && keys.jump_held
            && motion.blocked_horizontally
        {
            // At the surface against a wall: out of the water onto the
            // ledge, at full speed for as long as the wall blocks and the
            // waist is wet. Head under, jump only swims up (a wall under
            // water is no ledge).
            let exit = SWIM_EXIT_SPEED * UNITS_PER_METER;
            (exit, exit)
        } else {
            let v0 = motion.vel_y;
            let max_dv = SWIM_VERTICAL_ACCEL * UNITS_PER_METER * TICK_DT;
            (v0, v0 + (target - v0).clamp(-max_dv, max_dv))
        };
        motion.vel_y = v1;
        (v0 + v1) * 0.5 * TICK_DT
    } else {
        // A body at rest on the ground has had its fall zeroed by `settle`;
        // Minecraft's still pushes into the floor by one tick of gravity
        // (what walking off a ledge starts from).
        let v0 = if motion.grounded && motion.vel_y == 0.0 {
            -GRAVITY * UNITS_PER_METER * TICK_DT * AIR_DRAG
        } else {
            motion.vel_y
        };
        motion.vel_y = ((v0 - GRAVITY * UNITS_PER_METER * TICK_DT) * AIR_DRAG)
            .max(-TERMINAL_SPEED * UNITS_PER_METER);
        v0 * TICK_DT
    };
    desired.y = dy;
    desired
}

/// One tick of a body driven by `keys` heading `yaw`: plan, collision-swept
/// move, settle. Updates `motion`; returns the collision-corrected
/// translation to apply. Shared by every walking body (players steered by
/// the keyboard or a `Walker`, and people).
pub fn walk_body(
    motion: &mut Motion,
    keys: &MoveKeys,
    yaw: f32,
    tick: u64,
    broad_phase: &rapier3d::prelude::DefaultBroadPhase,
    narrow_phase: &rapier3d::prelude::NarrowPhase,
    bodies: &rapier3d::prelude::RigidBodySet,
    colliders: &rapier3d::prelude::ColliderSet,
    handle: rapier3d::prelude::RigidBodyHandle,
) -> Vector {
    walk_body_with(
        motion,
        keys,
        yaw,
        tick,
        broad_phase,
        narrow_phase,
        bodies,
        colliders,
        handle,
        &MoveParams::PLAYER,
    )
}

/// [`walk_body`] with explicit tunables (Adventure bodies cannot fly).
#[allow(clippy::too_many_arguments)]
pub fn walk_body_with(
    motion: &mut Motion,
    keys: &MoveKeys,
    yaw: f32,
    tick: u64,
    broad_phase: &rapier3d::prelude::DefaultBroadPhase,
    narrow_phase: &rapier3d::prelude::NarrowPhase,
    bodies: &rapier3d::prelude::RigidBodySet,
    colliders: &rapier3d::prelude::ColliderSet,
    handle: rapier3d::prelude::RigidBodyHandle,
    params: &MoveParams,
) -> Vector {
    let desired = plan_with(motion, keys, yaw, tick, params);
    let sweep = sweep_in(
        broad_phase,
        narrow_phase,
        bodies,
        colliders,
        handle,
        motion,
        desired,
        None,
    );
    settle_sweep(motion, &desired, &sweep);
    sweep.moved
}

/// [`walk_body`] whose sweep only collides with the colliders `keep`
/// accepts (people pass one that skips other people's bodies: they keep
/// apart by soft separation, `people.rs`).
#[allow(clippy::too_many_arguments)]
pub fn walk_body_keeping(
    motion: &mut Motion,
    keys: &MoveKeys,
    yaw: f32,
    tick: u64,
    broad_phase: &rapier3d::prelude::DefaultBroadPhase,
    narrow_phase: &rapier3d::prelude::NarrowPhase,
    bodies: &rapier3d::prelude::RigidBodySet,
    colliders: &rapier3d::prelude::ColliderSet,
    handle: rapier3d::prelude::RigidBodyHandle,
    keep: &dyn Fn(rapier3d::prelude::ColliderHandle, &rapier3d::prelude::Collider) -> bool,
) -> Vector {
    let desired = plan_with(motion, keys, yaw, tick, &MoveParams::PLAYER);
    let sweep = sweep_in(
        broad_phase,
        narrow_phase,
        bodies,
        colliders,
        handle,
        motion,
        desired,
        Some(keep),
    );
    settle_sweep(motion, &desired, &sweep);
    sweep.moved
}

/// A sweep that lifted the body at least this far (world units) above
/// anything its move asked for met something it can climb — a ledge the
/// autostep took or tried, an edge it rolled up — not a wall. Far above the
/// controller's per-hit normal nudge (1e-4), below a one-voxel terrace.
pub const CLIMB_RISE_MIN: f32 = 0.1;

/// What [`sweep_character`] made of a move.
#[derive(Clone, Copy, Debug)]
pub struct Sweep {
    /// The translation actually made.
    pub moved: Vector,
    /// Whether the body ended on the ground.
    pub grounded: bool,
    /// The sweep lifted the body ([`CLIMB_RISE_MIN`]) at some point, whether
    /// or not it stayed up: what stopped it is a step, not a wall.
    pub climbed: bool,
}

/// Folds the collision result of this tick's move back into `motion`.
/// A blocked axis loses its momentum; a walker's own tick goes through
/// [`settle_sweep`], which spares a body stopped by a step.
pub fn settle(motion: &mut Motion, desired: &Vector, moved: &Vector, grounded: bool) {
    settle_sweep(
        motion,
        desired,
        &Sweep {
            moved: *moved,
            grounded,
            climbed: false,
        },
    );
}

/// [`settle`] from the whole [`Sweep`].
pub fn settle_sweep(motion: &mut Motion, desired: &Vector, sweep: &Sweep) {
    let Sweep {
        moved,
        grounded,
        climbed,
    } = *sweep;
    motion.grounded = grounded;
    let wanted = desired.x * desired.x + desired.z * desired.z;
    let made = moved.x * moved.x + moved.z * moved.z;
    // Less than half the distance it wanted (a quarter, squared).
    motion.blocked_horizontally = wanted > 0.0 && made < 0.25 * wanted;
    if motion.flying {
        // Touching down ends flight, as in Minecraft.
        if grounded && desired.y < 0.0 {
            motion.flying = false;
        }
        return;
    }
    let bumped_head = motion.vel_y > 0.0 && moved.y < desired.y * 0.5;
    if (grounded && motion.vel_y < 0.0) || bumped_head {
        motion.vel_y = 0.0;
    }
    // A wall stops the momentum into it (Minecraft zeroes the blocked axis)
    // — a wall, not a step: the autostep only carries the body as far past
    // a riser as the tick's travel has left, and with too little left the
    // capsule's round foot lands on the step's edge and slides back down,
    // the tick ending where it began. Zeroed there, the next tick has one
    // tick's acceleration, again too little, and the body never climbs.
    // Minecraft zeroes an axis only if it is still blocked after its step-up.
    if climbed {
        return;
    }
    if moved.x.abs() < 0.5 * desired.x.abs() {
        motion.vel_x = 0.0;
    }
    if moved.z.abs() < 0.5 * desired.z.abs() {
        motion.vel_z = 0.0;
    }
}

/// Upward lift that puts a body sunk into terrain back on its surface, in
/// world units. The character controller has no penetration recovery (the
/// vendored rapier's is unimplemented), so a body that starts a tick
/// overlapping terrain can often neither slide nor step out of it. It used to
/// happen at every region seam (no floor past the edge, so a crossing body
/// dipped and arrived sunk); the terrain halo (`halo.rs`) gives the region a
/// floor there, and `halo::walk_across_seam_never_dips` pins that a seam
/// crossing no longer needs it. It stays for real sinks (a body restored or
/// built over).
///
/// Zero when the body is clear, and also when even a `STEP_HEIGHT` lift
/// would not clear it: a buried body stays put rather than being teleported
/// up through whatever buried it.
pub fn depenetration_lift(queries: &QueryPipeline, shape: &dyn Shape, pos: &Pose) -> f32 {
    let zero = 0.0;
    if queries.intersect_shape(*pos, shape).next().is_none() {
        return zero;
    }
    let max = STEP_HEIGHT * UNITS_PER_METER;
    let up = Vector::Y;
    let mut raised = *pos;
    raised.translation += up * max;
    if queries.intersect_shape(raised, shape).next().is_some() {
        return zero;
    }
    let skin = CHARACTER_SKIN;
    let options = ShapeCastOptions {
        target_distance: skin,
        stop_at_penetration: false,
        max_time_of_impact: max,
        compute_impact_geometry_on_penetration: true,
    };
    match queries.cast_shape_on_continuous_surfaces(&raised, -up, shape, options, skin) {
        Some((_, hit)) => {
            let lift = max - hit.time_of_impact;
            if lift > 0.0 {
                crate::halo::note_lift();
            }
            lift
        }
        None => zero,
    }
}

/// Collision-corrected move of a character body: `desired` swept through the
/// world with the character controller, after lifting a body sunk into
/// terrain back onto it. Returns the translation actually made and whether
/// the body ended grounded. Read-only; the caller writes the body. A body
/// without a collider moves uncorrected. Entity-generic: players and mobs
/// both move through here.
pub fn sweep_character(
    broad_phase: &rapier3d::prelude::DefaultBroadPhase,
    narrow_phase: &rapier3d::prelude::NarrowPhase,
    bodies: &rapier3d::prelude::RigidBodySet,
    colliders: &rapier3d::prelude::ColliderSet,
    handle: rapier3d::prelude::RigidBodyHandle,
    motion: &Motion,
    desired: Vector,
) -> (Vector, bool) {
    sweep_character_in(
        broad_phase,
        narrow_phase,
        bodies,
        colliders,
        handle,
        motion,
        desired,
        None,
    )
}

/// [`sweep_character`] that, given `keep`, collides only with the colliders
/// it accepts (the lift out of terrain included).
#[allow(clippy::too_many_arguments)]
pub fn sweep_character_in(
    broad_phase: &rapier3d::prelude::DefaultBroadPhase,
    narrow_phase: &rapier3d::prelude::NarrowPhase,
    bodies: &rapier3d::prelude::RigidBodySet,
    colliders: &rapier3d::prelude::ColliderSet,
    handle: rapier3d::prelude::RigidBodyHandle,
    motion: &Motion,
    desired: Vector,
    keep: Option<&dyn Fn(rapier3d::prelude::ColliderHandle, &rapier3d::prelude::Collider) -> bool>,
) -> (Vector, bool) {
    let sweep = sweep_in(
        broad_phase,
        narrow_phase,
        bodies,
        colliders,
        handle,
        motion,
        desired,
        keep,
    );
    (sweep.moved, sweep.grounded)
}

/// [`sweep_character_in`] with everything the sweep found ([`Sweep`]).
#[allow(clippy::too_many_arguments)]
fn sweep_in(
    broad_phase: &rapier3d::prelude::DefaultBroadPhase,
    narrow_phase: &rapier3d::prelude::NarrowPhase,
    bodies: &rapier3d::prelude::RigidBodySet,
    colliders: &rapier3d::prelude::ColliderSet,
    handle: rapier3d::prelude::RigidBodyHandle,
    motion: &Motion,
    desired: Vector,
    keep: Option<&dyn Fn(rapier3d::prelude::ColliderHandle, &rapier3d::prelude::Collider) -> bool>,
) -> Sweep {
    let b = bodies.get(handle).unwrap();
    let t = b.translation();
    let Some(collider_handle) = b.colliders().first().copied() else {
        return Sweep {
            moved: desired,
            grounded: false,
            climbed: false,
        };
    };
    let collider = colliders.get(collider_handle).unwrap();
    let mut filter = rapier3d::prelude::QueryFilter::default().exclude_rigid_body(handle);
    filter.predicate = keep;
    let queries =
        broad_phase.as_query_pipeline(narrow_phase.query_dispatcher(), bodies, colliders, filter);
    // Sweep the shape upright: a player's body rotation carries the look
    // pitch, which must not tilt the collision shape.
    let upright = Pose::from_translation(t);
    // A body sunk into terrain is first put back on it.
    let lift = Vector::Y * depenetration_lift(&queries, collider.shape(), &upright);
    let start = Pose::from_translation(t + lift);
    let mut peak = 0.0_f32;
    let out = character_controller(motion).move_shape(
        TICK_DT,
        &queries,
        collider.shape(),
        &start,
        desired,
        // The highest the sweep had the body when it next hit something: a
        // step taken and slid back off shows here and not in the result.
        |hit| peak = peak.max(hit.translation_applied.y),
    );
    let climbed = peak.max(out.translation.y) > desired.y.max(0.0) + CLIMB_RISE_MIN;
    Sweep {
        moved: out.translation + lift,
        grounded: out.grounded,
        climbed,
    }
}

/// The character controller for this tick's move. Walking bodies step up
/// ledges and stick to the ground; flying and swimming (waist deep or more)
/// bodies do neither.
pub fn character_controller(motion: &Motion) -> KinematicCharacterController {
    let walking = !motion.flying && motion.immersion < Immersion::Waist;
    KinematicCharacterController {
        autostep: walking.then(|| CharacterAutostep {
            max_height: CharacterLength::Absolute(STEP_HEIGHT * UNITS_PER_METER),
            min_width: CharacterLength::Absolute(AUTOSTEP_MIN_WIDTH_M * UNITS_PER_METER),
            include_dynamic_bodies: false,
        }),
        snap_to_ground: walking.then(|| CharacterLength::Absolute(GROUND_SNAP * UNITS_PER_METER)),
        offset: CharacterLength::Absolute(CHARACTER_SKIN),
        // Plan D7: `depenetration_lift` owns sunk bodies (a buried one stays
        // put); upstream's zero-motion depenetration (#485) would push a body
        // buried deeper than STEP_HEIGHT out through cuboid terrain.
        depenetrate: false,
        ..Default::default()
    }
}
