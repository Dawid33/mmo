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

use crate::{BLOCK_UNITS, TICK_RATE};

// Tunables, in metres and seconds (1 m = 1 block = `BLOCK_UNITS` units), so
// they keep their meaning when TICK_RATE is retuned.
/// World units per metre.
pub const UNITS_PER_METER: f32 = BLOCK_UNITS as f32;
/// Seconds per simulation tick.
pub const TICK_DT: f32 = TICK_RATE as f32 / 1000.0;
/// Ground speed (Minecraft walk: 4.317 m/s).
pub const WALK_SPEED: f32 = 4.317;
/// Ground speed while sprinting (Minecraft: 5.612 m/s).
pub const SPRINT_SPEED: f32 = 5.612;
/// Horizontal creative-flight speed (Minecraft: 10.92 m/s).
pub const FLY_SPEED: f32 = 10.92;
/// Horizontal creative-flight speed while sprinting (Minecraft: 21.6 m/s).
pub const FLY_SPRINT_SPEED: f32 = 21.6;
/// Ascend / descend speed while flying.
pub const FLY_VERTICAL_SPEED: f32 = 7.5;
/// Downward acceleration while rising (a jump's way up). Minecraft's
/// effective 32 m/s² with its 9 m/s launch gives a 0.56 s hang that reads as
/// floaty; the height is kept (v²/2g ≈ 1.25 m, clearing a 1 m block) and the
/// time to reach it cut instead: rise = JUMP_SPEED / GRAVITY seconds.
pub const GRAVITY: f32 = 45.0;
/// Downward acceleration while falling. Heavier than the way up so the body
/// comes down faster than it went up — the usual platformer answer to a
/// floaty apex. Also what a walk off a ledge falls under.
pub const FALL_GRAVITY: f32 = GRAVITY * 1.5;
/// Fall speed cap (Minecraft's terminal velocity: 78.4 m/s).
pub const TERMINAL_SPEED: f32 = 78.4;
/// Launch speed of a jump; JUMP_SPEED² / (2 GRAVITY) is the peak height.
pub const JUMP_SPEED: f32 = 10.6;
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
// `BodyScale` snaps `step_height_m`: UNIT keeps this snap only while they match.
const _: () = assert!(GROUND_SNAP == STEP_HEIGHT);

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
pub const SWIM_EXIT_SPEED: f32 = JUMP_SPEED;

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

/// Body factor of the stud world: 1.8 m people become 2.4 m minifigures
/// (4 bricks tall). Scales the eye, the jump and gravity; the collision
/// capsule has its own size below.
pub const STUD_BODY_FACTOR: f32 = 4.0 / 3.0;
/// Collision height of a stud-world person, metres. Shorter than the 2.4 m
/// figure so a body standing on floor studs (0.1 m) passes a corridor two
/// tiles (2.4 m) high — `physics-next` `stud_voxels`.
pub const STUD_PLAYER_HEIGHT_M: f32 = 2.2;
/// Collision radius of a stud-world person, metres: 0.8 m wide, so it passes
/// a corridor one tile (2 studs, 1 m) wide.
pub const STUD_PLAYER_RADIUS_M: f32 = 0.4;
/// Step height in the stud world, metres: one 0.6 m brick course plus margin
/// (a 0.55 m step does not climb it, 0.65 m does — `physics-next`
/// `stud_voxels`). Decision point at first playtest: this, or one plate
/// (0.25 m) plus jumping a brick.
pub const STUD_STEP_HEIGHT_M: f32 = 0.65;

/// Steepest contact (radians from level) a stud-world body rests on without sliding:
/// a right angle, so it never slides. Stud terrain is boxes: a steeper-than-45° contact
/// is always a capsule's round foot on a brick's EDGE, never a slope. Sliding there
/// undid every autostep of a rise taller than the capsule's radius that began late in
/// a tick: the step lifts the body, nudges it forward only by the tick's leftover
/// motion, then gives the lift back — the foot came down on the ledge's edge, off
/// axis, and slid back off it (`physics-next` `stud_voxels::stud_body_steps_up…`).
/// Held on the edge, the next tick's step takes the rest of the rise.
pub const STUD_EDGE_HOLD_ANGLE: f32 = std::f32::consts::FRAC_PI_2;
/// The same limit for voxel-world bodies: rapier's default (45°), so terrain slopes
/// steeper than a 1:1 ramp slide a body off.
pub const VOXEL_SLOPE_SLIDE_ANGLE: f32 = std::f32::consts::FRAC_PI_4;

/// The per-region size of a player body (spec "Player, fitted"): voxel
/// worlds keep 1.8 m people, the stud world fits 2.4 m minifigures. `factor`
/// scales the eye, the jump speed and gravity (so jump and fall *times*
/// match, lengths scale) and the autostep tread; `step_height_m` is the
/// autostep height, ground snap and depenetration reach; `height_m` /
/// `radius_m` size the collision capsule (and so the spawn clearance). Walk speed and swimming do not scale (decision point
/// at playtest). Only the player path reads it; every other walking body
/// stays at the shared constants. Lives in `GameData::body_scale`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, state_hash::StateHash)]
pub struct BodyScale {
    pub factor: f32,
    pub step_height_m: f32,
    pub height_m: f32,
    pub radius_m: f32,
}

impl Default for BodyScale {
    fn default() -> Self {
        Self::UNIT
    }
}

impl BodyScale {
    /// The shared movement constants, unscaled (every product with `factor`
    /// is exact): what every walking body but a region's player-sized ones
    /// uses (slimes).
    pub const UNIT: BodyScale =
        BodyScale { factor: 1.0, step_height_m: STEP_HEIGHT, height_m: PLAYER_HEIGHT, radius_m: PLAYER_RADIUS };
    pub const STUD: BodyScale = BodyScale {
        factor: STUD_BODY_FACTOR,
        step_height_m: STUD_STEP_HEIGHT_M,
        height_m: STUD_PLAYER_HEIGHT_M,
        radius_m: STUD_PLAYER_RADIUS_M,
    };

    /// Capsule half-height (cylinder part), world units.
    pub fn capsule_half_height(&self) -> f32 {
        // The same expression as `PLAYER_CAPSULE_HALF_HEIGHT`: bit-equal for VOXEL.
        self.height_m * UNITS_PER_METER / 2.0 - self.capsule_radius()
    }

    /// Capsule radius, world units.
    pub fn capsule_radius(&self) -> f32 {
        self.radius_m * UNITS_PER_METER
    }

    /// Eye above the body origin (capsule centre), world units.
    pub fn eye_offset(&self) -> f32 {
        PLAYER_EYE_OFFSET * self.factor
    }

    /// `base` fitted to this body: jump speed and gravity scale by `factor`.
    pub fn move_params(&self, base: MoveParams) -> MoveParams {
        MoveParams { jump_speed: base.jump_speed * self.factor, gravity_scale: base.gravity_scale * self.factor, ..base }
    }
}

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
#[derive(Debug, Default, Clone, Copy, PartialEq, state_hash::StateHash, serde::Serialize, serde::Deserialize)]
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
}

/// How deep a body is in water: the deepest of its feet, waist and eye that
/// is in a wet cell (`crate::immersion_at`).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
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
    /// Whether a double tap of jump starts flight. Off for Adventure
    /// (survival) bodies, which also drop out of flight.
    pub allow_flight: bool,
    /// Multiplies `GRAVITY`, `FALL_GRAVITY` and `TERMINAL_SPEED` (a
    /// [`BodyScale`] factor; 1.0 everywhere else).
    pub gravity_scale: f32,
}

impl MoveParams {
    pub const PLAYER: MoveParams =
        MoveParams { walk_speed: WALK_SPEED, jump_speed: JUMP_SPEED, allow_flight: true, gravity_scale: 1.0 };
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
    let land = if sprint { SPRINT_SPEED } else { params.walk_speed };
    let speed = if motion.flying {
        if sprint { FLY_SPRINT_SPEED } else { FLY_SPEED }
    } else if swimming {
        // Never faster than the body moves on land (slow movers swim slow).
        (if sprint { SWIM_SPRINT_SPEED } else { SWIM_SPEED }).min(land)
    } else if motion.immersion == Immersion::Feet {
        land * WADE_SPEED_FACTOR
    } else {
        land
    };
    let step = speed * UNITS_PER_METER * TICK_DT;
    // Horizontal movement follows yaw only — speed stays full regardless of
    // pitch (Minecraft behavior).
    let mut desired = crate::yaw_quat(yaw) * Vector::new(x * step, 0.0, z * step);

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
        let (v0, v1) = if motion.immersion == Immersion::Waist && keys.jump_held && motion.blocked_horizontally {
            // At the surface against a wall: out of the water onto the
            // ledge, at full speed for as long as the wall blocks and the
            // waist is wet. Head under, jump only swims up (a wall under
            // water is no ledge).
            // Scaled like the jump (`BodyScale::move_params`): gravity is, so an
            // unscaled exit carried a stud body 0.94 m where the voxel body rose 1.25 m.
            let exit = SWIM_EXIT_SPEED * params.gravity_scale * UNITS_PER_METER;
            (exit, exit)
        } else {
            let v0 = motion.vel_y;
            // Scaled with gravity: water brakes a body as hard as gravity speeds it
            // up, whatever the body's scale (unscaled, a stud body stepping off a
            // bank sank 5.4 m where the voxel body's 3.4 m scales to 4.5 m).
            let max_dv = SWIM_VERTICAL_ACCEL * params.gravity_scale * UNITS_PER_METER * TICK_DT;
            (v0, v0 + (target - v0).clamp(-max_dv, max_dv))
        };
        motion.vel_y = v1;
        (v0 + v1) * 0.5 * TICK_DT
    } else {
        // Holding jump re-jumps on every landing, as in Minecraft.
        let v0 = if motion.grounded && keys.jump_held {
            params.jump_speed * UNITS_PER_METER
        } else {
            motion.vel_y
        };
        let g = (if v0 > 0.0 { GRAVITY } else { FALL_GRAVITY }) * params.gravity_scale;
        let v1 = (v0 - g * UNITS_PER_METER * TICK_DT).max(-TERMINAL_SPEED * params.gravity_scale * UNITS_PER_METER);
        motion.vel_y = v1;
        // Trapezoid rule: exact under constant acceleration, so jump height
        // does not depend on the tick period.
        (v0 + v1) * 0.5 * TICK_DT
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
    walk_body_with(motion, keys, yaw, tick, broad_phase, narrow_phase, bodies, colliders, handle, &MoveParams::PLAYER)
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
    walk_body_scaled(motion, keys, yaw, tick, broad_phase, narrow_phase, bodies, colliders, handle, params, BodyScale::UNIT)
}

/// [`walk_body_with`] for a body of `scale` (the player path); `params`
/// should already be `scale.move_params(..)`.
#[allow(clippy::too_many_arguments)]
pub fn walk_body_scaled(
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
    scale: BodyScale,
) -> Vector {
    let desired = plan_with(motion, keys, yaw, tick, params);
    let (corrected, grounded) =
        sweep_character_scaled(broad_phase, narrow_phase, bodies, colliders, handle, motion, desired, scale);
    settle(motion, &desired, &corrected, grounded);
    corrected
}

/// Folds the collision result of this tick's move back into `motion`.
pub fn settle(motion: &mut Motion, desired: &Vector, moved: &Vector, grounded: bool) {
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
    } else if motion.vel_y < 0.0 && desired.y < 0.0 && moved.y > desired.y * 0.5 {
        // A fall that something stopped without grounding the body (a capsule
        // hanging on a brick's edge, wedged against a wall): the body keeps the
        // speed it made, not the speed it wanted. Without this `vel_y` ran on to
        // terminal speed while the body stood still, and the body shot down the
        // moment it came free.
        motion.vel_y = motion.vel_y.max((moved.y / TICK_DT).min(0.0));
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
    depenetration_lift_scaled(queries, shape, pos, BodyScale::UNIT)
}

/// [`depenetration_lift`] reaching `scale.step_height_m`.
pub fn depenetration_lift_scaled(queries: &QueryPipeline, shape: &dyn Shape, pos: &Pose, scale: BodyScale) -> f32 {
    let zero = 0.0;
    if queries.intersect_shape(*pos, shape).next().is_none() {
        return zero;
    }
    let max = scale.step_height_m * UNITS_PER_METER;
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
    sweep_character_scaled(broad_phase, narrow_phase, bodies, colliders, handle, motion, desired, BodyScale::UNIT)
}

/// [`sweep_character`] for a body of `scale`: its step height, ground snap,
/// autostep tread and depenetration reach.
#[allow(clippy::too_many_arguments)]
pub fn sweep_character_scaled(
    broad_phase: &rapier3d::prelude::DefaultBroadPhase,
    narrow_phase: &rapier3d::prelude::NarrowPhase,
    bodies: &rapier3d::prelude::RigidBodySet,
    colliders: &rapier3d::prelude::ColliderSet,
    handle: rapier3d::prelude::RigidBodyHandle,
    motion: &Motion,
    desired: Vector,
    scale: BodyScale,
) -> (Vector, bool) {
    let b = bodies.get(handle).unwrap();
    let t = b.translation();
    let Some(collider_handle) = b.colliders().first().copied() else {
        return (desired, false);
    };
    let collider = colliders.get(collider_handle).unwrap();
    let queries = broad_phase.as_query_pipeline(
        narrow_phase.query_dispatcher(),
        bodies,
        colliders,
        rapier3d::prelude::QueryFilter::default().exclude_rigid_body(handle),
    );
    // Sweep the shape upright: a player's body rotation carries the look
    // pitch, which must not tilt the collision shape.
    let upright = Pose::from_translation(t);
    // A body sunk into terrain is first put back on it.
    let lift = Vector::Y * depenetration_lift_scaled(&queries, collider.shape(), &upright, scale);
    let start = Pose::from_translation(t + lift);
    let out = character_controller_scaled(motion, scale).move_shape(
        TICK_DT,
        &queries,
        collider.shape(),
        &start,
        desired,
        |_| {},
    );
    (out.translation + lift, out.grounded)
}

/// The character controller for this tick's move. Walking bodies step up
/// ledges and stick to the ground; flying and swimming (waist deep or more)
/// bodies do neither.
pub fn character_controller(motion: &Motion) -> KinematicCharacterController {
    character_controller_scaled(motion, BodyScale::UNIT)
}

/// [`character_controller`] for a body of `scale`: steps and snaps
/// `scale.step_height_m` (`GROUND_SNAP` is as tall as the step), tread
/// `AUTOSTEP_MIN_WIDTH_M` × `factor`.
pub fn character_controller_scaled(motion: &Motion, scale: BodyScale) -> KinematicCharacterController {
    let walking = !motion.flying && motion.immersion < Immersion::Waist;
    KinematicCharacterController {
        autostep: walking.then(|| CharacterAutostep {
            max_height: CharacterLength::Absolute(scale.step_height_m * UNITS_PER_METER),
            min_width: CharacterLength::Absolute(AUTOSTEP_MIN_WIDTH_M * scale.factor * UNITS_PER_METER),
            include_dynamic_bodies: false,
        }),
        snap_to_ground: walking
            .then(|| CharacterLength::Absolute(scale.step_height_m * UNITS_PER_METER)),
        offset: CharacterLength::Absolute(CHARACTER_SKIN),
        // Plan D7: `depenetration_lift` owns sunk bodies (a buried one stays
        // put); upstream's zero-motion depenetration (#485) would push a body
        // buried deeper than STEP_HEIGHT out through cuboid terrain.
        depenetrate: false,
        min_slope_slide_angle: if scale == BodyScale::STUD { STUD_EDGE_HOLD_ANGLE } else { VOXEL_SLOPE_SLIDE_ANGLE },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const YAW0: f32 = 0.0;

    fn keys() -> MoveKeys {
        MoveKeys::default()
    }

    /// Runs `plan` + a collision-free `settle` on open air / flat ground.
    fn tick(m: &mut Motion, k: &MoveKeys, t: u64, floor_y: Option<&mut f32>) -> Vector {
        let desired = plan(m, k, YAW0, t);
        let mut moved = desired;
        let mut grounded = false;
        if let Some(y) = floor_y {
            *y += desired.y;
            if *y <= 0.0 {
                moved.y = desired.y - *y;
                *y = 0.0;
                grounded = true;
            }
        }
        settle(m, &desired, &moved, grounded);
        moved
    }

    #[test]
    fn walking_forward_moves_at_walk_speed_along_minus_z() {
        let mut m = Motion { grounded: true, ..Default::default() };
        let d = plan(&mut m, &MoveKeys { forward: true, ..keys() }, YAW0, 0);
        assert_eq!(d.x, 0.0);
        assert!((d.z + WALK_SPEED * UNITS_PER_METER * TICK_DT).abs() < 1e-5);
    }

    #[test]
    fn sprint_needs_forward_and_diagonals_are_not_faster() {
        let mut m = Motion { grounded: true, ..Default::default() };
        let back = plan(&mut m, &MoveKeys { back: true, sprint: true, ..keys() }, YAW0, 0);
        assert!((back.z - WALK_SPEED * UNITS_PER_METER * TICK_DT).abs() < 1e-5);
        let diag = plan(&mut m, &MoveKeys { forward: true, right: true, ..keys() }, YAW0, 0);
        let len2 = diag.x * diag.x + diag.z * diag.z;
        let walk = WALK_SPEED * UNITS_PER_METER * TICK_DT;
        assert!((len2 - walk * walk).abs() < 1e-3);
    }

    #[test]
    fn airborne_body_accelerates_downward_and_lands() {
        let mut m = Motion::default();
        let mut y = 40.0;
        let first = tick(&mut m, &keys(), 0, Some(&mut y));
        let second = tick(&mut m, &keys(), 1, Some(&mut y));
        assert!(first.y < 0.0 && second.y < first.y, "falls faster each tick");
        for t in 2..100 {
            tick(&mut m, &keys(), t, Some(&mut y));
        }
        assert!(m.grounded);
        assert_eq!(m.vel_y, 0.0, "landing clears the fall speed");
    }

    #[test]
    fn jump_clears_one_block_and_returns_to_ground() {
        let mut m = Motion { grounded: true, ..Default::default() };
        let mut y = 0.0;
        let jump = MoveKeys { jump_pressed: true, jump_held: true, ..keys() };
        tick(&mut m, &jump, 0, Some(&mut y));
        assert!(!m.grounded && y > 0.0);
        let mut peak = y;
        let mut t = 1;
        while !m.grounded {
            tick(&mut m, &keys(), t, Some(&mut y));
            peak = peak.max(y);
            t += 1;
            assert!(t < 100, "must land");
        }
        assert!(peak > UNITS_PER_METER, "peak {peak} clears a 1 m block");
        assert!(peak < 1.5 * UNITS_PER_METER, "peak {peak} stays a Minecraft-ish jump");
        assert!(!m.flying, "a single tap never starts flight");
    }

    #[test]
    fn jump_is_ignored_in_mid_air() {
        let mut m = Motion::default();
        let d = plan(&mut m, &MoveKeys { jump_held: true, ..keys() }, YAW0, 0);
        assert!(d.y < 0.0);
    }

    #[test]
    fn double_tap_toggles_flying_and_slow_taps_do_not() {
        let tap = MoveKeys { jump_pressed: true, jump_held: true, ..keys() };
        let window = DOUBLE_TAP_WINDOW_MS / TICK_RATE;

        let mut m = Motion::default();
        plan(&mut m, &tap, YAW0, 10);
        plan(&mut m, &tap, YAW0, 10 + window + 1);
        assert!(!m.flying, "taps further apart than the window are two jumps");

        plan(&mut m, &tap, YAW0, 10 + window + 3);
        assert!(m.flying, "second tap inside the window starts flight");
        assert_eq!(m.last_jump_tap, None, "a toggle consumes both taps");

        plan(&mut m, &tap, YAW0, 100);
        plan(&mut m, &tap, YAW0, 102);
        assert!(!m.flying, "double tap again drops out of flight");
    }

    #[test]
    fn flying_hovers_ascends_descends_and_lands() {
        let mut m = Motion { flying: true, ..Default::default() };
        assert_eq!(plan(&mut m, &keys(), YAW0, 0).y, 0.0, "no gravity in flight");
        let up = plan(&mut m, &MoveKeys { jump_held: true, ..keys() }, YAW0, 1);
        let down = plan(&mut m, &MoveKeys { descend: true, ..keys() }, YAW0, 2);
        assert!(up.y > 0.0 && down.y == -up.y);

        settle(&mut m, &down, &Vector::ZERO, true);
        assert!(!m.flying, "descending onto the ground ends flight");
    }

    #[test]
    fn hitting_a_ceiling_kills_upward_speed() {
        let mut m = Motion { grounded: true, ..Default::default() };
        let d = plan(&mut m, &MoveKeys { jump_held: true, ..keys() }, YAW0, 0);
        assert!(m.vel_y > 0.0);
        settle(&mut m, &d, &Vector::ZERO, false);
        assert_eq!(m.vel_y, 0.0);
    }

    /// A fall stopped without grounding (a capsule held on an edge) keeps the
    /// speed it made, not the speed it wanted; a free fall keeps its own.
    #[test]
    fn a_blocked_fall_does_not_gather_speed() {
        let mut m = Motion::default();
        for t in 0..40 {
            let desired = plan(&mut m, &keys(), YAW0, t);
            assert!(desired.y < 0.0);
            settle(&mut m, &desired, &Vector::ZERO, false);
            assert_eq!(m.vel_y, 0.0, "tick {t}");
        }
        let desired = plan(&mut m, &keys(), YAW0, 40);
        let wanted = m.vel_y;
        settle(&mut m, &desired, &desired, false);
        assert_eq!(m.vel_y, wanted, "an unobstructed fall is untouched");
        // Sliding at a third of the wanted fall: that speed, no more.
        let desired = plan(&mut m, &keys(), YAW0, 41);
        let moved = Vector::new(0.0, desired.y / 3.0, 0.0);
        settle(&mut m, &desired, &moved, false);
        assert_eq!(m.vel_y, moved.y / TICK_DT);
    }

    #[test]
    fn fall_speed_is_capped() {
        let mut m = Motion::default();
        for t in 0..400 {
            tick(&mut m, &keys(), t, None);
        }
        assert_eq!(m.vel_y, -TERMINAL_SPEED * UNITS_PER_METER);
    }
}
