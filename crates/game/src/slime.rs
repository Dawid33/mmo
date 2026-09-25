//! Slimes: ambient hopping wildlife, after Minecraft's slime.
//!
//! A slime sits still on the ground for a random 10–30 ticks, then hops,
//! travelling only while airborne. It wanders on a random heading that it
//! re-picks every few seconds, and turns back before it reaches a region edge.
//! It never attacks: this is wildlife, not a hostile mob.
//!
//! Everything here is deterministic sim code, run identically by the server
//! and by the client's prediction. The randomness is a stateless hash of the
//! tick, the region and the entity ([`roll`]), so there is no RNG state to
//! undo-log or to drift under rollback. The only per-slime state is the small
//! [`Slime`] component. Squash and stretch is purely a render effect, derived
//! on the client from the pose stream.
//!
//! Minecraft's tick is 50 ms, the same as `TICK_RATE` today, so its tick
//! counts are used as they are.

use parry3d::math::{Real, Vector};
use na::{Quaternion, Translation3, Unit};
use slotmapd::Key as _;
use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};

use crate::movement::{self, MoveKeys, MoveParams, UNITS_PER_METER};
use crate::protocol::{ColliderSpec, RegionCoords, REGION_SIZE};
use crate::registry::BlockId;
use crate::{
    EntityKey, EntityKind, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind,
    IsometryReal, Rollback, BLOCK_VOXELS,
};

// --- Spawning -----------------------------------------------------------
/// A region tries to spawn one slime every this many ticks (100 = 5 s).
pub const SLIME_SPAWN_INTERVAL_TICKS: u64 = 100;
/// Chance, in percent, that a spawn attempt goes ahead.
pub const SLIME_SPAWN_CHANCE_PERCENT: u64 = 50;
/// A region stops spawning once it holds this many slimes. With the defaults
/// an empty region fills in about 80 s; one slime per ~500 m² at 64 m regions.
pub const SLIME_CAP_PER_REGION: usize = 8;
/// Cell-mode regions: a cell stops spawning once it holds this many slimes.
pub const SLIME_CAP_PER_CELL: usize = 8;
/// Relative odds of each size at spawn: (size, weight). Minecraft sizes.
pub const SLIME_SIZE_WEIGHTS: [(u8, u64); 3] = [(1, 60), (2, 30), (4, 10)];
/// Slimes never spawn on this block (water, id 5 in `assets/blocks/blocks.ron`;
/// block ids are stable by contract).
pub const SLIME_NO_SPAWN_BLOCK: BlockId = BlockId(5);
/// Height above the surface a slime is dropped from, in world units.
pub const SLIME_SPAWN_CLEARANCE: f32 = 2.0;

// --- Body ----------------------------------------------------------------
/// Edge of a size-1 slime's cube, in metres (Minecraft: 0.52 blocks).
pub const SLIME_EDGE_M: f32 = 0.52;

// --- Movement ------------------------------------------------------------
/// Ticks a landed slime sits still before its next hop: MIN + rand(RANGE)
/// (Minecraft: 10 + rand(20), 0.5–1.5 s).
pub const SLIME_HOP_DELAY_MIN_TICKS: u64 = 10;
pub const SLIME_HOP_DELAY_RANGE_TICKS: u64 = 20;
/// Launch speed of a hop, m/s. Peak = v² / (2 GRAVITY) ≈ 1.1 m, over a block.
pub const SLIME_JUMP_SPEED: f32 = 10.0;
/// Airborne horizontal speed, m/s: BASE + PER_SIZE × size, so bigger slimes
/// cover more ground per hop (Minecraft: 0.2 + 0.1 × size).
pub const SLIME_HOP_SPEED_BASE: f32 = 2.0;
pub const SLIME_HOP_SPEED_PER_SIZE: f32 = 0.5;
/// A wandering slime picks a new heading every MIN + rand(RANGE) ticks
/// (Minecraft: 40 + rand(60)).
pub const SLIME_TURN_MIN_TICKS: u64 = 40;
pub const SLIME_TURN_RANGE_TICKS: u64 = 60;
/// Fastest a slime turns, radians per tick (Minecraft: 90°).
pub const SLIME_MAX_TURN_PER_TICK: f32 = FRAC_PI_4;
/// Within this many world units of a region edge a slime heads back inwards.
/// Kept out of the ghost margin: a slime that crossed would wake the next
/// region, and wildlife is not worth a region genesis.
pub const SLIME_EDGE_MARGIN: f32 = 4.0 * BLOCK_VOXELS as f32;

/// Per-slime behaviour state. Lives in the rollback ECS (`Ecs::slime`) and
/// rides `EntityBundle`. u64 ticks, not usize: serialized across the
/// 64-bit↔wasm32 seam.
#[derive(Debug, Default, Clone, Copy, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Slime {
    /// Facing (radians about +Y), eased toward `target_yaw`.
    pub yaw: Real,
    pub target_yaw: Real,
    /// Tick at which a wandering slime picks a new heading.
    pub next_turn_tick: u64,
    /// Tick at which a grounded slime hops.
    pub next_hop_tick: u64,
    /// Airborne from its own hop (moves forward) rather than falling.
    pub hopping: bool,
}

/// Cube edge of a slime of `size`, in world units.
pub fn slime_edge_units(size: u8) -> f32 {
    SLIME_EDGE_M * size as f32 * UNITS_PER_METER
}

/// The collider: a capsule filling the cube's height and 80 % of its width.
/// A capsule rides the character controller's proven voxel path (the seam
/// fixes were pinned on capsules) and already crosses the transfer seam.
pub fn slime_collider(size: u8) -> ColliderSpec {
    let edge = slime_edge_units(size);
    ColliderSpec::CapsuleY { half_height: 0.1 * edge, radius: 0.4 * edge }
}

/// Walking tunables for a slime of `size`.
pub fn slime_move_params(size: u8) -> MoveParams {
    MoveParams {
        walk_speed: SLIME_HOP_SPEED_BASE + SLIME_HOP_SPEED_PER_SIZE * size as f32,
        jump_speed: SLIME_JUMP_SPEED,
    }
}

/// splitmix64 finalizer.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Stateless deterministic randomness: the same (tick, subject, salt) gives
/// the same number on every machine and every replay.
pub fn roll(tick: u64, subject: u64, salt: u64) -> u64 {
    mix(mix(tick ^ salt.rotate_left(32)) ^ subject)
}

const SALT_SPAWN: u64 = 0x5EED_5117;
const SALT_TURN: u64 = 0x7042_5117;
const SALT_HOP: u64 = 0x4099_5117;

fn region_subject(r: RegionCoords) -> u64 {
    ((r.x as u32 as u64) << 32) | r.z as u32 as u64
}

fn random_yaw(r: u64) -> Real {
    Real::from((r % 3600) as f32 * (TAU / 3600.0))
}

/// Wrap an angle into [-π, π). `rem_euclid` is an exact IEEE op.
fn wrap_angle(a: f32) -> f32 {
    (a + PI).rem_euclid(TAU) - PI
}

/// Heading back toward the region interior when within the edge margin of
/// region-local `(x, z)`, else `None`. Yaw 0 faces −Z and +π/2 faces −X
/// (`yaw_quat` applied to the −Z forward).
fn inward_yaw(x: f32, z: f32) -> Option<f32> {
    let axis = |v: f32| {
        if v < SLIME_EDGE_MARGIN {
            1
        } else if v > REGION_SIZE - SLIME_EDGE_MARGIN {
            -1
        } else {
            0
        }
    };
    let yaw = match (axis(x), axis(z)) {
        (0, 0) => return None,
        (1, 0) => -FRAC_PI_2,
        (-1, 0) => FRAC_PI_2,
        (0, 1) => PI,
        (0, _) => 0.0,
        (1, 1) => -3.0 * FRAC_PI_4,
        (1, _) => -FRAC_PI_4,
        (_, 1) => 3.0 * FRAC_PI_4,
        _ => FRAC_PI_4,
    };
    Some(yaw)
}

/// Where and what a region's spawn attempt at `tick` would place, before the
/// terrain and cap checks. Pure, so tests can predict spawns.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlimeSpawnPlan {
    /// Region-local voxel column, centred in its block.
    pub vx: i32,
    pub vz: i32,
    pub size: u8,
    pub yaw: Real,
}

/// The spawn attempt of `region` at `tick`, or `None` if there is none (not
/// an attempt tick, or the chance roll failed). Tick 0 — a region's genesis
/// tick — never spawns.
pub fn slime_spawn_plan(region: RegionCoords, tick: u64) -> Option<SlimeSpawnPlan> {
    if tick == 0 || tick % SLIME_SPAWN_INTERVAL_TICKS != 0 {
        return None;
    }
    let r = roll(tick, region_subject(region), SALT_SPAWN);
    if r % 100 >= SLIME_SPAWN_CHANCE_PERCENT {
        return None;
    }
    // A block column clear of the edge margin (twice over, so a fresh slime
    // does not start out turning back), centred in the block.
    let block = BLOCK_VOXELS as u64;
    let margin_blocks = (2.0 * SLIME_EDGE_MARGIN) as u64 / block;
    let span = REGION_SIZE as u64 / block - 2 * margin_blocks;
    let r2 = mix(r);
    let bx = margin_blocks + (r2 % span);
    let bz = margin_blocks + ((r2 >> 32) % span);
    let r3 = mix(r2);
    let total: u64 = SLIME_SIZE_WEIGHTS.iter().map(|(_, w)| w).sum();
    let mut pick = r3 % total;
    let mut size = SLIME_SIZE_WEIGHTS[0].0;
    for (s, w) in SLIME_SIZE_WEIGHTS {
        if pick < w {
            size = s;
            break;
        }
        pick -= w;
    }
    Some(SlimeSpawnPlan {
        vx: (bx * block + block / 2) as i32,
        vz: (bz * block + block / 2) as i32,
        size,
        yaw: random_yaw(mix(r3)),
    })
}

impl Rollback {
    /// One tick of slime life in `region`: maybe spawn one, then step each.
    /// Runs at the start of the region tick, before the physics step that
    /// consumes the kinematic targets it sets.
    pub fn tick_slimes_safe(&mut self, region: RegionCoords) {
        let tick = *self.data.tick as u64;
        self.maybe_spawn_slime_safe(region, tick);
        // Ghost mirrors never carry a Slime component, so only owned slimes
        // are driven. Key order is deterministic.
        let slimes: Vec<EntityKey> = self
            .ecs
            .slime
            .iter()
            .filter(|(_, s)| s.is_some())
            .map(|(k, _)| k)
            .collect();
        for e in slimes {
            self.step_slime_safe(e, tick);
        }
    }

    /// Spawn gate at region-local voxel column `(vx, vz)`. Legacy regions:
    /// always allowed (the per-region cap applies instead). Cell mode: the
    /// column's cell must be active and hold fewer than SLIME_CAP_PER_CELL.
    pub fn slime_spawn_allowed(&self, vx: i32, vz: i32) -> bool {
        let cc = self.data.cells.cell_chunks;
        if cc == 0 {
            return true;
        }
        let Some(cell) = crate::cell_of_pos(vx as f32, vz as f32, cc) else { return false };
        if !self.data.cells.active.contains(&cell) {
            return false;
        }
        let in_cell = self
            .cell_entity_keys(cell)
            .into_iter()
            .filter(|k| self.data.ecs.slime.try_get(*k).is_some())
            .count();
        in_cell < SLIME_CAP_PER_CELL
    }

    fn maybe_spawn_slime_safe(&mut self, region: RegionCoords, tick: u64) {
        let Some(plan) = slime_spawn_plan(region, tick) else { return };
        if !self.slime_spawn_allowed(plan.vx, plan.vz) {
            return;
        }
        if self.data.cells.cell_chunks == 0 {
            let count = self.data.ecs.slime.iter().filter(|(_, s)| s.is_some()).count();
            if count >= SLIME_CAP_PER_REGION {
                return;
            }
        }
        let Some((surface, top)) = self.surface_voxel(plan.vx, plan.vz) else { return };
        if top == SLIME_NO_SPAWN_BLOCK {
            return;
        }
        let y = (surface + 1) as f32 + slime_edge_units(plan.size) / 2.0 + SLIME_SPAWN_CLEARANCE;
        let pos = Vector::new(Real::from(plan.vx as f32), Real::from(y), Real::from(plan.vz as f32));
        self.spawn_slime_safe(pos, plan.size, plan.yaw, tick);
    }

    /// Spawn a slime of `size` at region-local `pos` (the body centre),
    /// undo-safely. Falls to the ground before its first hop.
    pub fn spawn_slime_safe(&mut self, pos: Vector<Real>, size: u8, yaw: Real, tick: u64) -> EntityKey {
        let e = self.data.ecs.create_entity_safe();
        let pose = IsometryReal::from_parts(
            Translation3::new(pos.x, pos.y, pos.z),
            Unit::<Quaternion<Real>>::from(crate::yaw_quat(yaw)),
        );
        self.build_kinematic_body_safe(e, pose, &slime_collider(size));
        let kind = EntityKind::Slime { size };
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityKind(e, Some(kind)),
        ));
        self.data.ecs.kind.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetEntityKind(e, None),
        ));
        self.data.ecs.kind.set_safe(e, Some(kind));
        self.data.ecs.motion.set_safe(e, Some(crate::Motion::default()));
        let turn = roll(tick, e.data().as_ffi(), SALT_TURN);
        self.data.ecs.slime.set_safe(
            e,
            Some(Slime {
                yaw,
                target_yaw: yaw,
                next_turn_tick: tick + SLIME_TURN_MIN_TICKS + turn % SLIME_TURN_RANGE_TICKS,
                next_hop_tick: 0, // set on landing
                hopping: false,
            }),
        );
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityPosition(e, pose),
        ));
        e
    }

    fn step_slime_safe(&mut self, e: EntityKey, tick: u64) {
        let Some(slime0) = *self.data.ecs.slime.try_get(e) else { return };
        let Some(handle) = *self.data.ecs.rigidbody.try_get(e) else { return };
        let Some(EntityKind::Slime { size }) = *self.data.ecs.kind.try_get(e) else { return };
        let Some(body) = self.data.physics.bodies.get(handle) else { return };
        let t = *body.translation();
        let motion0 = (*self.data.ecs.motion.try_get(e)).unwrap_or_default();
        let subject = e.data().as_ffi();
        let mut slime = slime0;

        // Heading: back inwards near an edge, else re-rolled now and then.
        if let Some(inward) = inward_yaw(t.x.0, t.z.0) {
            slime.target_yaw = Real::from(inward);
        } else if tick >= slime.next_turn_tick {
            let r = roll(tick, subject, SALT_TURN);
            slime.target_yaw = random_yaw(r);
            slime.next_turn_tick = tick + SLIME_TURN_MIN_TICKS + mix(r) % SLIME_TURN_RANGE_TICKS;
        }
        let d = wrap_angle(slime.target_yaw.0 - slime.yaw.0)
            .clamp(-SLIME_MAX_TURN_PER_TICK, SLIME_MAX_TURN_PER_TICK);
        slime.yaw = Real::from(wrap_angle(slime.yaw.0 + d));

        // Hop cadence: still on the ground, a hop when due, forward in the air.
        let grounded = motion0.grounded;
        let hop = grounded && tick >= slime.next_hop_tick;
        let keys = MoveKeys {
            forward: hop || (!grounded && slime.hopping),
            // Never jump_pressed: that edge drives the double-tap flight toggle.
            jump_held: hop,
            ..Default::default()
        };
        if hop {
            slime.hopping = true;
        }

        // A slime sitting out its delay makes no move at all, so it skips the
        // character controller — the dominant sim cost — on most ticks.
        let mut moved = Vector::zeros();
        if !(grounded && keys == MoveKeys::default()) {
            let mut motion = motion0;
            let desired = movement::plan_with(&mut motion, &keys, slime.yaw, tick, &slime_move_params(size));
            let (corrected, now_grounded) = movement::sweep_character(
                &self.data.physics.broad_phase,
                &self.data.physics.narrow_phase,
                &self.data.physics.bodies,
                &self.data.physics.colliders,
                handle,
                &motion,
                desired,
            );
            movement::settle(&mut motion, &desired, &corrected, now_grounded);
            if !grounded && now_grounded {
                // Landed: sit still for a while (Minecraft counts the delay
                // from the landing, not the take-off).
                let r = roll(tick, subject, SALT_HOP);
                slime.next_hop_tick =
                    tick + SLIME_HOP_DELAY_MIN_TICKS + r % SLIME_HOP_DELAY_RANGE_TICKS;
                slime.hopping = false;
            }
            if motion != motion0 {
                self.data.ecs.motion.set_safe(e, Some(motion));
            }
            moved = corrected;
        }

        if slime != slime0 {
            self.data.ecs.slime.set_safe(e, Some(slime));
        }
        let turned = slime.yaw != slime0.yaw;
        if moved != Vector::zeros() || turned {
            let rot = crate::yaw_quat(slime.yaw);
            self.data.physics.bodies.mutate_body_safe(handle, move |b| {
                if moved != Vector::zeros() {
                    b.set_next_kinematic_translation(t + moved);
                }
                if turned {
                    b.set_next_kinematic_rotation(rot);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roll_is_deterministic_and_spread() {
        assert_eq!(roll(7, 9, 1), roll(7, 9, 1));
        assert_ne!(roll(7, 9, 1), roll(8, 9, 1));
        assert_ne!(roll(7, 9, 1), roll(7, 10, 1));
        assert_ne!(roll(7, 9, 1), roll(7, 9, 2));
    }

    #[test]
    fn wrap_angle_stays_in_range() {
        for a in [-10.0f32, -PI, -1.0, 0.0, 1.0, PI, 10.0] {
            let w = wrap_angle(a);
            assert!((-PI..PI).contains(&w), "{a} -> {w}");
        }
    }

    #[test]
    fn inward_yaw_points_inwards() {
        // Forward is yaw_quat(yaw) · (0, 0, −1) = (−sin yaw, 0, −cos yaw).
        let fwd = |y: f32| (-y.sin(), -y.cos());
        let lo = SLIME_EDGE_MARGIN / 2.0;
        let hi = REGION_SIZE - lo;
        let mid = REGION_SIZE / 2.0;
        for (x, z) in [(lo, mid), (hi, mid), (mid, lo), (mid, hi), (lo, lo), (hi, hi), (lo, hi), (hi, lo)] {
            let (fx, fz) = fwd(inward_yaw(x, z).unwrap());
            // Moving along the heading brings the point closer to the centre.
            let before = (x - mid).abs() + (z - mid).abs();
            let after = (x + fx - mid).abs() + (z + fz - mid).abs();
            assert!(after < before, "({x},{z}) heading ({fx},{fz})");
        }
        assert_eq!(inward_yaw(mid, mid), None);
    }

    #[test]
    fn collider_fills_the_cube_height() {
        for size in [1, 2, 4] {
            let ColliderSpec::CapsuleY { half_height, radius } = slime_collider(size);
            assert!((2.0 * (half_height + radius) - slime_edge_units(size)).abs() < 1e-4);
        }
    }
}
