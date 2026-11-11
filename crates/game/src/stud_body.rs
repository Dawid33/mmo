//! Stud bodies: bricks detached from stud terrain into ONE dynamic rigid body that
//! falls under gravity and is settled once at rest. A felled tree's part is the
//! first user.
//!
//! - **Mass** without an eigen solve: [`stud_model_mass_properties`] sums one box per
//!   brick ([`mass_properties_of_boxes`]), the colliders add zero mass.
//! - **Collider**: one compound, a cuboid per brick's cell box (a slope part collides
//!   as its box while it falls), so it reads back exactly as a `ColliderSpec`. Its
//!   material (friction, restitution) is not in the spec: a receiver sets it again
//!   (`set_stud_body_material_safe`).
//! - **Gravity**: a constant user force, as the physics world has none of its own.
//! - **Settling** ([`Rollback::tick_stud_bodies_safe`]): at `REST_TICKS` still ticks or
//!   `SETTLE_TIMEOUT_TICKS` of age the body is removed and its policy applied —
//!   `DropItems` puts [`StudBody::drops`] down as piles at the tile its centre of mass
//!   rests in.
//! - **Seams**: an [`crate::EntityKind::Body`] with an `EntityId`; the [`StudBody`]
//!   travels in `Carried::stud_body` with its drops moved to `Carried::drops`, so a
//!   body lost on the way puts them down (`migrate.rs`).
//!
//! Simple on purpose (decide at a first look at falling trees): no landed state (one
//! angular damping), no skirt snapping, and a toppling part starts with exactly the
//! spin it is given.

use std::collections::{BTreeMap, BTreeSet};

use rapier3d::math::{Pose, Vector};
use rapier3d::prelude::{ColliderBuilder, MassProperties, RigidBodyBuilder, SharedShape};
use slotmapd::Key as _;
use state_hash::StateHash;

use crate::movement::UNITS_PER_METER;
use crate::stud::{StudChunkCoords, StudShape, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS, STUD_UNITS};
use crate::stud_material::{stud_material, MAT_LEAVES, MAT_WOOD};
use crate::edit::BlockPos;
use crate::{
    EntityKey, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, Rollback, BLOCK_UNITS, TICK_RATE,
};

/// Density of tree wood, kg/m³.
pub const WOOD_DENSITY: f32 = 700.0;
/// Density of leaves, kg/m³.
pub const LEAF_DENSITY: f32 = 60.0;
/// Density of every other natural material (stone, soil), kg/m³.
pub const STONE_DENSITY: f32 = 2600.0;
/// Downward acceleration of a stud body, m/s²: the world's gravity
/// (`movement::GRAVITY`, what players and slimes fall with), so a felled
/// tree falls like everything else rather than floating down at a real-world
/// 9.81 in a world whose bodies fall at 45.
pub const STUD_BODY_GRAVITY: f32 = crate::movement::GRAVITY;
/// Friction coefficient of a stud body's colliders: high, so a toppling
/// tree's butt grips the ground and the body pivots about it rather than
/// skating out from under its crown.
pub const STUD_BODY_FRICTION: f32 = 0.9;
/// Restitution of a stud body's colliders: none — a felled trunk lands, it
/// does not bounce.
pub const STUD_BODY_RESTITUTION: f32 = 0.0;
/// Angular damping of a stud body (1/s): a little, so a landed crown
/// settles instead of rocking on its leaves.
pub const STUD_BODY_ANGULAR_DAMPING: f32 = 0.5;
/// Soft-CCD prediction distance of a stud body (units): one metre. At the
/// world's gravity a body is moving a block per tick after about half a
/// second of fall, and a contact rapier only finds once the shapes overlap
/// lets it sink that far into the ground on impact before it is pushed back
/// out (or, for a thin collider, through). Soft CCD finds the contact up to
/// this far ahead, so the landing is stopped at the surface.
pub const STUD_BODY_SOFT_CCD: f32 = BLOCK_UNITS as f32;
/// A body moving slower than this (units/s; 0.5 m/s) counts as still.
pub const REST_LINVEL: f32 = 8.0;
/// A body turning slower than this (rad/s) counts as still.
pub const REST_ANGVEL: f32 = 0.2;
/// Consecutive still ticks after which a body is settled.
pub const REST_TICKS: u16 = 10;
/// Milliseconds after which a body is settled whether or not it rests.
pub const SETTLE_TIMEOUT_MS: u64 = 10_000;
/// [`SETTLE_TIMEOUT_MS`] in ticks (`TICK_RATE` is the tick period in ms;
/// integer division, so no `f32` rounding drops a tick).
pub const SETTLE_TIMEOUT_TICKS: u64 = SETTLE_TIMEOUT_MS / TICK_RATE;

/// What happens to a body once it is settled.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, StateHash)]
pub enum SettlePolicy {
    /// Removed, leaving nothing behind (a fortress chop order: no items).
    #[default]
    Despawn,
    /// Removed, its bricks turned into their drops (summed per item at the
    /// detach: [`StudBody::drops`]) and deposited as piles at the tile its
    /// centre of mass rests in — in whichever region
    /// then owns it (it crosses seams while it falls); a full pile spills to
    /// the cells around it — a tree a player felled: the logs lie where it
    /// landed.
    DropItems,
}

/// Mass properties of a set of solid boxes `(mass kg, min, max)` in model units (a
/// stud body's bricks): centre of mass, mass and principal inertia, summed in order.
pub(crate) fn mass_properties_of_boxes(boxes: &[(f64, [f64; 3], [f64; 3])]) -> MassProperties {
    let mass: f64 = boxes.iter().map(|b| b.0).sum();
    if mass <= 0.0 {
        return MassProperties::default();
    }
    let mut com = [0.0f64; 3];
    for (m, lo, hi) in boxes {
        for a in 0..3 {
            com[a] += m * (lo[a] + hi[a]) * 0.5;
        }
    }
    let com = com.map(|c| c / mass);
    let mut inertia = [0.0f64; 3];
    for (m, lo, hi) in boxes {
        let e = [0, 1, 2].map(|a| hi[a] - lo[a]);
        let d = [0, 1, 2].map(|a| (lo[a] + hi[a]) * 0.5 - com[a]);
        for a in 0..3 {
            let (b, c) = ((a + 1) % 3, (a + 2) % 3);
            // Box about its own centre, then the parallel-axis shift.
            inertia[a] += m * (e[b] * e[b] + e[c] * e[c]) / 12.0 + m * (d[b] * d[b] + d[c] * d[c]);
        }
    }
    MassProperties::new(
        Vector::new(com[0] as f32, com[1] as f32, com[2] as f32),
        mass as f32,
        Vector::new(inertia[0] as f32, inertia[1] as f32, inertia[2] as f32),
    )
}

/// Density of plastic bricks (a material under `NATURAL_BASE`), kg/m³ (ABS).
pub const PLASTIC_DENSITY: f32 = 1050.0;
/// Most bricks one detach takes: a felled part of the biggest tree with room to spare.
/// A larger set is refused whole.
pub const MAX_STUD_BODY_BRICKS: usize = 8192;

/// One brick of a stud body: a `StudBrick` whose min cell is an offset from the
/// model's origin cell (the minimum corner of the detached bricks, so every offset
/// is ≥ 0) instead of a chunk cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize, StateHash)]
pub struct StudModelBrick {
    /// `[stud x, plate y, stud z]` from the model origin.
    pub min: [i16; 3],
    pub shape: StudShape,
    pub material: u16,
    pub data: u16,
}

impl StudModelBrick {
    /// The brick as a chunk brick at min cell 0 (for its size and part).
    fn at_zero(&self) -> crate::StudBrick {
        crate::StudBrick { min: [0; 3], shape: self.shape, material: self.material, data: self.data }
    }

    /// Cell size after rotation.
    pub fn size(&self) -> [u8; 3] {
        self.at_zero().size()
    }
}

/// The bricks of a stud body, sorted by `min`. Model cell `c` spans model units
/// `[c.x * 8, (c.x + 1) * 8) x [c.y * 3.2, (c.y + 1) * 3.2) x [c.z * 8, (c.z + 1) * 8)`.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize, StateHash)]
pub struct StudModel {
    pub bricks: Vec<StudModelBrick>,
}

/// A detached-bricks entity (ECS component `stud_body`).
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize, StateHash)]
pub struct StudBody {
    pub model: StudModel,
    pub policy: SettlePolicy,
    /// Region tick it was detached on.
    pub spawned_tick: u64,
    /// Consecutive ticks under both rest thresholds.
    pub still_ticks: u16,
    /// What it turns into once settled (`SettlePolicy::DropItems`:
    /// [`stud_model_drops`], fixed at the detach; empty for `Despawn`). In transit
    /// it rides `Carried::drops` instead (empty here).
    pub drops: Vec<crate::ItemStack>,
}

/// Density of a brick of `material`, kg/m³.
pub fn stud_density_of(material: u16) -> f32 {
    if material == MAT_LEAVES {
        LEAF_DENSITY
    } else if material == MAT_WOOD {
        WOOD_DENSITY
    } else if material < crate::stud_material::NATURAL_BASE {
        PLASTIC_DENSITY
    } else {
        STONE_DENSITY
    }
}

/// Model-unit size of one cell.
const CELL: [f64; 3] = [STUD_UNITS as f64, 16.0 / 5.0, STUD_UNITS as f64];

/// A brick's cell box `(min, max)` in model units.
fn brick_box(b: &StudModelBrick) -> ([f64; 3], [f64; 3]) {
    let size = b.size();
    let lo = [0, 1, 2].map(|a| f64::from(b.min[a]) * CELL[a]);
    (lo, [0, 1, 2].map(|a| lo[a] + f64::from(size[a]) * CELL[a]))
}

/// The model's mass properties in MODEL frame (one box per brick): mass in kg (density x the cell box's volume in m³),
/// centre of mass in model units, diagonal inertia about it. Pure, `+ - * /` only.
pub fn stud_model_mass_properties(model: &StudModel) -> MassProperties {
    let unit_m3 = f64::from(UNITS_PER_METER) * f64::from(UNITS_PER_METER) * f64::from(UNITS_PER_METER);
    let boxes: Vec<(f64, [f64; 3], [f64; 3])> = model
        .bricks
        .iter()
        .map(|b| {
            let (lo, hi) = brick_box(b);
            let volume = (hi[0] - lo[0]) * (hi[1] - lo[1]) * (hi[2] - lo[2]) / unit_m3;
            (f64::from(stud_density_of(b.material)) * volume, lo, hi)
        })
        .collect();
    mass_properties_of_boxes(&boxes)
}

/// What the model's bricks yield (`mining::stud_drops` of its cells per material).
pub fn stud_model_drops(model: &StudModel) -> Vec<crate::ItemStack> {
    let mut cells: BTreeMap<u16, u32> = BTreeMap::new();
    for b in &model.bricks {
        *cells.entry(b.material).or_default() += b.size().iter().map(|&v| u32::from(v)).product::<u32>();
    }
    crate::mining::stud_drops(&cells.into_iter().collect::<Vec<_>>())
}

impl Rollback {
    /// Deposits `drops` at `cell` — one deposit per stack, whose merge /
    /// free-cell rule spills what one pile cannot hold to the cells around
    /// it. Pickable after `PICKUP_DELAY_TICKS`. Losses to the pile caps are
    /// counted by the deposit.
    pub(crate) fn deposit_drops_safe(&mut self, cell: BlockPos, drops: &[crate::ItemStack], tick: u64) {
        for stack in drops {
            let _destroyed = self.deposit_items_safe(cell, *stack, tick, crate::PICKUP_DELAY_TICKS);
        }
    }

    /// A stud body's collider material (not part of its `ColliderSpec`):
    /// set on every collider of `handle`'s body, undo-safely — on a body
    /// rebuilt from a crossing's bundle.
    pub(crate) fn set_stud_body_material_safe(&mut self, handle: rapier3d::prelude::RigidBodyHandle) {
        let Some(body) = self.data.physics.bodies.get(handle) else { return };
        for ch in body.colliders().to_vec() {
            self.data.physics.colliders.mutate_collider_safe(ch, |c| {
                c.set_friction(STUD_BODY_FRICTION);
                c.set_restitution(STUD_BODY_RESTITUTION);
            });
        }
    }

    /// Detaches bricks of this region's stud terrain into one dynamic stud body,
    /// undo-safely. `picks` names them by chunk and min cell; a pick that names no
    /// brick, or one of an undiggable material, is skipped. The bricks leave their
    /// chunks (one write per chunk) and a new [`crate::EntityKind::Body`] entity
    /// carries them, its body at the model's centre of mass.
    ///
    /// With a `pivot` (region-local units) the body starts in the rigid motion of a
    /// spin at `angvel` (rad/s, world axes) about it — a felled tree turning about its
    /// butt; without one it spins about its centre of mass.
    ///
    /// `None`, with nothing changed: nothing to take, or more than
    /// [`MAX_STUD_BODY_BRICKS`].
    pub fn detach_studs_safe(
        &mut self,
        picks: &BTreeMap<StudChunkCoords, BTreeSet<[u8; 3]>>,
        policy: SettlePolicy,
        angvel: Vector,
        pivot: Option<Vector>,
    ) -> Option<EntityKey> {
        // Plan: the chunks without the picked bricks, and the bricks in region cells.
        let mut writes: Vec<(EntityKey, crate::StudChunk)> = Vec::new();
        let mut taken: Vec<([i32; 3], crate::StudBrick)> = Vec::new();
        for (c, mins) in picks {
            let Some(key) = self.stud_key_at(*c) else { continue };
            let Some(mut chunk) = self.data.ecs.stud.try_get(key).clone() else { continue };
            let base = [
                (c.x * STUD_CHUNK_STUDS) as i32,
                c.y.saturating_mul(STUD_CHUNK_PLATES as i32),
                (c.z * STUD_CHUNK_STUDS) as i32,
            ];
            let before = taken.len();
            for min in mins {
                let diggable = chunk
                    .bricks
                    .get(min)
                    .is_some_and(|b| stud_material(b.material).is_some_and(|m| m.hardness != u16::MAX));
                if !diggable {
                    continue;
                }
                if let Some(b) = chunk.bricks.remove(min) {
                    taken.push(([0, 1, 2].map(|a| base[a] + i32::from(b.min[a])), b));
                }
            }
            if taken.len() > before {
                writes.push((key, chunk));
            }
        }
        if taken.is_empty() || taken.len() > MAX_STUD_BODY_BRICKS {
            return None;
        }
        let origin = taken.iter().fold([i32::MAX; 3], |o, (min, _)| [0, 1, 2].map(|a| o[a].min(min[a])));
        let span = taken.iter().fold([0; 3], |s, (min, _)| [0, 1, 2].map(|a| s[a].max(min[a] - origin[a])));
        if span.iter().any(|&v| v > i32::from(i16::MAX)) {
            return None;
        }
        let mut bricks: Vec<StudModelBrick> = taken
            .iter()
            .map(|(min, b)| StudModelBrick {
                min: [0, 1, 2].map(|a| (min[a] - origin[a]) as i16),
                shape: b.shape,
                material: b.material,
                data: b.data,
            })
            .collect();
        bricks.sort_unstable();
        let model = StudModel { bricks };
        // The hole in region cells: piles resting on what left settle again.
        let hole_hi = taken.iter().fold([i32::MIN; 3], |h, (min, b)| {
            let s = b.size();
            [0, 1, 2].map(|a| h[a].max(min[a] + i32::from(s[a])))
        });
        for (key, chunk) in writes {
            self.set_stud_chunk_safe(key, chunk);
        }
        let _destroyed = self.stud_containers_after_write_safe(origin, hole_hi);
        let _destroyed = self.stud_piles_after_write_safe(origin, hole_hi);

        let mprops = stud_model_mass_properties(&model);
        let com = mprops.local_com;
        let origin_units = Vector::new(
            origin[0] as f32 * CELL[0] as f32,
            crate::stud::plate_units(origin[1]),
            origin[2] as f32 * CELL[2] as f32,
        );
        let pose = Pose::from_translation(origin_units + com);
        let linvel = pivot.map_or(Vector::ZERO, |p| angvel.cross(pose.translation - p));

        let e = self.data.ecs.create_entity_safe();
        self.assign_entity_id_safe(e);
        let user_data = e.data().as_ffi() as u128;
        // The body frame is the COM frame: same mass and inertia, COM at 0.
        let body_mprops = MassProperties { local_com: Vector::ZERO, ..mprops };
        let mut body = RigidBodyBuilder::dynamic()
            .pose(pose)
            .angvel(angvel)
            .linvel(linvel)
            .angular_damping(STUD_BODY_ANGULAR_DAMPING)
            .soft_ccd_prediction(STUD_BODY_SOFT_CCD)
            .can_sleep(true)
            .additional_mass_properties(body_mprops)
            .user_data(user_data)
            .build();
        let weight = mprops.mass() * STUD_BODY_GRAVITY * UNITS_PER_METER;
        body.add_force(Vector::new(0.0, -weight, 0.0), false);
        let handle = self.insert_body_safe(body);
        self.data.ecs.rigidbody.set_safe(e, Some(handle));

        let parts: Vec<(Pose, SharedShape)> = model
            .bricks
            .iter()
            .map(|b| {
                let (lo, hi) = brick_box(b);
                let half = [0, 1, 2].map(|a| ((hi[a] - lo[a]) * 0.5) as f32);
                let centre = Vector::new(lo[0] as f32 + half[0], lo[1] as f32 + half[1], lo[2] as f32 + half[2]);
                (Pose::from_translation(centre - com), SharedShape::cuboid(half[0], half[1], half[2]))
            })
            .collect();
        let collider = ColliderBuilder::new(SharedShape::compound(parts))
            .mass_properties(MassProperties::default())
            .friction(STUD_BODY_FRICTION)
            .restitution(STUD_BODY_RESTITUTION)
            .user_data(user_data)
            .build();
        // The mass comes only from `body_mprops` (no eigen solve).
        debug_assert_eq!(collider.mass_properties(), MassProperties::default(), "a stud body collider adds no mass");
        self.attach_collider_safe(handle, collider);

        let spawned_tick = *self.data.tick;
        // The model first, then the kind: a renderer seeing a `Body` kind already
        // knows it is a stud body.
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::AddStudBody(e, pose, Box::new(model.clone())),
        ));
        self.set_body_kind_safe(e);
        let drops = match policy {
            SettlePolicy::Despawn => Vec::new(),
            SettlePolicy::DropItems => stud_model_drops(&model),
        };
        self.data.ecs.stud_body.set_safe(e, Some(StudBody { model, policy, spawned_tick, still_ticks: 0, drops }));
        Some(e)
    }

    /// The tile holding region-local point `t` (sim units): where a
    /// settled stud body's drops go.
    pub(crate) fn stud_tile_of_point(t: Vector) -> crate::TilePos {
        let (w, h) = (crate::tile_width_units(), crate::tile_height_units());
        [(t.x / w).floor() as i32, (t.y / h).floor() as i32, (t.z / w).floor() as i32]
    }

    /// Once per region tick, after the physics step: count each stud body's
    /// consecutive still ticks (under both `REST_LINVEL` and `REST_ANGVEL`, or
    /// asleep) and settle it — remove it and apply its policy — at `REST_TICKS` of
    /// them, or at `SETTLE_TIMEOUT_TICKS` of age. A body whose centre of mass leaves
    /// the region is not settled: `scan_boundaries` hands it to the neighbour.
    /// Entities in key order.
    pub fn tick_stud_bodies_safe(&mut self, tick: u64) {
        let keys: Vec<EntityKey> =
            self.data.ecs.stud_body.iter().filter_map(|(k, v)| v.as_ref().map(|_| k)).collect();
        for e in keys {
            let Some(sb) = self.data.ecs.stud_body.try_get(e).clone() else { continue };
            let Some(handle) = *self.data.ecs.rigidbody.try_get(e) else { continue };
            let Some(body) = self.data.physics.bodies.get(handle) else { continue };
            let (v, w) = (body.linvel(), body.angvel());
            let still =
                body.is_sleeping() || (v.dot(v) < REST_LINVEL * REST_LINVEL && w.dot(w) < REST_ANGVEL * REST_ANGVEL);
            let still_ticks = if still { sb.still_ticks.saturating_add(1) } else { 0 };
            if still_ticks >= REST_TICKS || tick.saturating_sub(sb.spawned_tick) >= SETTLE_TIMEOUT_TICKS {
                // The body frame is the centre-of-mass frame: the drops go to the tile
                // it rests in (past the seam within the flip hysteresis, the
                // neighbour's, by an intent to its owner).
                let cell = Self::stud_tile_of_point(body.translation());
                self.remove_entity_safe(e, None);
                if sb.policy == SettlePolicy::DropItems {
                    self.deposit_drops_safe(cell, &sb.drops, tick);
                }
                continue;
            }
            if still_ticks != sb.still_ticks {
                self.data.ecs.stud_body.set_safe(e, Some(StudBody { still_ticks, ..sb }));
            }
        }
    }
}
