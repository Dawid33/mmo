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
}

impl MoveParams {
    pub const PLAYER: MoveParams = MoveParams { walk_speed: WALK_SPEED, jump_speed: JUMP_SPEED, allow_flight: true };
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
    if !params.allow_flight {
        motion.flying = false;
        motion.last_jump_tap = None;
    } else if keys.jump_pressed {
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
    let speed = match (motion.flying, sprint) {
        (false, false) => params.walk_speed,
        (false, true) => SPRINT_SPEED,
        (true, false) => FLY_SPEED,
        (true, true) => FLY_SPRINT_SPEED,
    };
    let step = speed * UNITS_PER_METER * TICK_DT;
    // Horizontal movement follows yaw only — speed stays full regardless of
    // pitch (Minecraft behavior).
    let mut desired = crate::yaw_quat(yaw) * Vector::new(x * step, 0.0, z * step);

    let dy = if motion.flying {
        axis(keys.jump_held, keys.descend) * FLY_VERTICAL_SPEED * UNITS_PER_METER * TICK_DT
    } else {
        // Holding jump re-jumps on every landing, as in Minecraft.
        let v0 = if motion.grounded && keys.jump_held {
            params.jump_speed * UNITS_PER_METER
        } else {
            motion.vel_y
        };
        let g = if v0 > 0.0 { GRAVITY } else { FALL_GRAVITY };
        let v1 = (v0 - g * UNITS_PER_METER * TICK_DT).max(-TERMINAL_SPEED * UNITS_PER_METER);
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
    let desired = plan_with(motion, keys, yaw, tick, params);
    let (corrected, grounded) = sweep_character(broad_phase, narrow_phase, bodies, colliders, handle, motion, desired);
    settle(motion, &desired, &corrected, grounded);
    corrected
}

/// Folds the collision result of this tick's move back into `motion`.
pub fn settle(motion: &mut Motion, desired: &Vector, moved: &Vector, grounded: bool) {
    motion.grounded = grounded;
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
    let lift = Vector::Y * depenetration_lift(&queries, collider.shape(), &upright);
    let start = Pose::from_translation(t + lift);
    let out = character_controller(motion).move_shape(
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
/// ledges and stick to the ground; flying (or rising) bodies do neither.
pub fn character_controller(motion: &Motion) -> KinematicCharacterController {
    let walking = !motion.flying;
    KinematicCharacterController {
        autostep: walking.then(|| CharacterAutostep {
            max_height: CharacterLength::Absolute(STEP_HEIGHT * UNITS_PER_METER),
            min_width: CharacterLength::Absolute(AUTOSTEP_MIN_WIDTH_M * UNITS_PER_METER),
            include_dynamic_bodies: false,
        }),
        snap_to_ground: walking
            .then(|| CharacterLength::Absolute(GROUND_SNAP * UNITS_PER_METER)),
        offset: CharacterLength::Absolute(CHARACTER_SKIN),
        // Plan D7: `depenetration_lift` owns sunk bodies (a buried one stays
        // put); upstream's zero-motion depenetration (#485) would push a body
        // buried deeper than STEP_HEIGHT out through cuboid terrain.
        depenetrate: false,
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

    #[test]
    fn fall_speed_is_capped() {
        let mut m = Motion::default();
        for t in 0..400 {
            tick(&mut m, &keys(), t, None);
        }
        assert_eq!(m.vel_y, -TERMINAL_SPEED * UNITS_PER_METER);
    }
}
