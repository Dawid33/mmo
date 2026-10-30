//! Voxel bodies: a set of terrain blocks detached into ONE dynamic rigid body
//! that falls under gravity and is settled (today: despawned) once at rest.
//! Generic — a felled tree's part is the first user; falling stalagmites and
//! structural collapse are meant to reuse it.
//!
//! **Mass without an eigen solve.** Rapier's own mass properties for a shape
//! (or a sum of two) go through glamx `SymmetricEigen3`, whose std
//! `acos`/`cos` differ between native and wasm32. So a voxel body's colliders
//! carry `MassProperties::zero()` (adding zero to anything returns the other
//! side untouched, never reaching the solve), and the body gets its mass from
//! [`model_mass_properties`] as `additional_mass_properties`, built with
//! `MassProperties::new`: the inertia tensor's DIAGONAL about the centre of
//! mass in model axes (products of inertia dropped), with an identity
//! principal frame. Every number is `+ - * /` on per-block boxes in a fixed
//! (`BTreeMap`) order, so it is bit-identical everywhere.
//!
//! **Units.** Mass in kilograms (`*_DENSITY` kg/m³ × solid volume in m³,
//! 1 m³ = one block = `BLOCK_VOXELS`³ voxels); lengths in sim units
//! (1 m = `BLOCK_VOXELS` = 16 units), so inertia is kg·unit² and the gravity
//! force kg·unit/s².
//!
//! **Gravity.** The physics world's own gravity is zero (players and slimes
//! integrate their own, kinematically), so a voxel body carries a constant
//! user force of `mass × VOXEL_BODY_GRAVITY` downward, set once before
//! insertion (rapier keeps user forces across steps).
//!
//! **No `EntityKind`.** A voxel body is never a seam-crossing entity:
//! `scan_boundaries` walks `ecs.kind` (and would rebuild it on the other
//! side as a player capsule). A body whose centre of mass leaves the region
//! is settled instead.

use std::collections::{BTreeMap, BTreeSet};

use rapier3d::math::{Pose, Vector};
use rapier3d::prelude::{ColliderBuilder, MassProperties, RigidBodyBuilder, SharedShape};
use slotmapd::Key as _;
use state_hash::StateHash;

use crate::edit::BlockPos;
use crate::movement::UNITS_PER_METER;
use crate::{
    block_collider_cells, block_props, Block, BlockId, ChiselData, ColliderScale, EntityKey, GameDataTransactionKind,
    GameDataUpdate, GameDataUpdateKind, Rollback, BLOCK_VOXELS, HALF_BLOCK_VOXELS, REGION_SIZE, TICK_RATE,
};

/// Density of tree wood (logs and branches: `ColliderScale::Half` blocks),
/// kg/m³.
pub const WOOD_DENSITY: f32 = 700.0;
/// Density of non-solid blocks (leaves), kg/m³.
pub const LEAF_DENSITY: f32 = 60.0;
/// Density of every other block (stone, soil), kg/m³.
pub const STONE_DENSITY: f32 = 2600.0;
/// Downward acceleration of a voxel body, m/s² (real gravity).
pub const VOXEL_BODY_GRAVITY: f32 = 9.81;
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

/// The blocks of a voxel body: each block's full state by its offset from
/// the model origin block (`VoxelBody::origin`, the minimum corner of the
/// detached blocks, so every offset is ≥ 0). Model block `o` spans model
/// units `[o * BLOCK_VOXELS, (o + 1) * BLOCK_VOXELS)` per axis.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize, StateHash)]
pub struct VoxelModel {
    pub blocks: BTreeMap<[i16; 3], (Block, Option<ChiselData>)>,
}

/// What happens to a body once it is settled.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, StateHash)]
pub enum SettlePolicy {
    /// Removed, leaving nothing behind (felled trees: no items).
    #[default]
    Despawn,
}

/// A detached-terrain entity (ECS component `voxel_body`).
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize, StateHash)]
pub struct VoxelBody {
    pub model: VoxelModel,
    /// Region-local block the model's offsets are relative to.
    pub origin: [i32; 3],
    pub policy: SettlePolicy,
    /// Region tick it was detached on.
    pub spawned_tick: u64,
    /// Consecutive ticks under both rest thresholds.
    pub still_ticks: u16,
}

/// Density class of a block (kg/m³): non-solid → leaf, half-block collider
/// grid → wood, anything else → stone.
pub fn density_of(id: BlockId) -> f32 {
    let props = block_props();
    if !props.is_solid(id) {
        LEAF_DENSITY
    } else if props.collider_scale(id) == ColliderScale::Half {
        WOOD_DENSITY
    } else {
        STONE_DENSITY
    }
}

/// Whether `id` is foliage (leaves): a non-solid cutout block. Standing, it
/// has no collider; in a voxel body it collides as a whole block.
pub fn is_foliage(id: BlockId) -> bool {
    let props = block_props();
    id != BlockId::AIR && !props.is_solid(id) && props.is_cutout(id)
}

/// Model-unit origin of block offset `o`.
fn block_base(o: [i16; 3]) -> [f64; 3] {
    o.map(|v| f64::from(v) * BLOCK_VOXELS as f64)
}

/// The model's solid matter as boxes (mass kg, min, max in model units): a
/// whole block, or each non-air cuboid of a chiseled one (so a block weighs
/// its density × its solid-voxel fraction).
fn mass_boxes(model: &VoxelModel) -> Vec<(f64, [f64; 3], [f64; 3])> {
    let block_voxels = (BLOCK_VOXELS * BLOCK_VOXELS * BLOCK_VOXELS) as f64;
    let mut out = Vec::new();
    for (&o, (block, chisel)) in &model.blocks {
        let density = f64::from(density_of(block.id));
        let base = block_base(o);
        match chisel {
            None => {
                if block.id != BlockId::AIR {
                    let b = BLOCK_VOXELS as f64;
                    out.push((density, base, [base[0] + b, base[1] + b, base[2] + b]));
                }
            }
            Some(c) => {
                for (min, max, material) in c.cuboids() {
                    if material == BlockId::AIR {
                        continue;
                    }
                    let lo = [0, 1, 2].map(|a| base[a] + min[a] as f64);
                    let hi = [0, 1, 2].map(|a| base[a] + max[a] as f64);
                    let volume = (hi[0] - lo[0]) * (hi[1] - lo[1]) * (hi[2] - lo[2]);
                    out.push((density * volume / block_voxels, lo, hi));
                }
            }
        }
    }
    out
}

/// The model's mass properties in MODEL frame: mass (kg), centre of mass
/// (model units) and the diagonal of the inertia tensor about it, along the
/// model axes (products of inertia dropped — no eigen solve; the principal
/// frame is the identity). Zero for a massless model. Pure.
pub fn model_mass_properties(model: &VoxelModel) -> MassProperties {
    let boxes = mass_boxes(model);
    let mass: f64 = boxes.iter().map(|b| b.0).sum();
    if mass <= 0.0 {
        return MassProperties::default();
    }
    let mut com = [0.0f64; 3];
    for (m, lo, hi) in &boxes {
        for a in 0..3 {
            com[a] += m * (lo[a] + hi[a]) * 0.5;
        }
    }
    let com = com.map(|c| c / mass);
    let mut inertia = [0.0f64; 3];
    for (m, lo, hi) in &boxes {
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

/// The model's collision volume as integer model-unit boxes `(min, max)`:
/// each solid block's collider cells (the rule a terrain chunk applies:
/// tree wood on the half-block grid, so a falling log is as solid as it
/// stood) and each leaf block ([`is_foliage`]) as a whole block — unlike
/// standing leaves, a falling crown collides — on the half-block grid,
/// greedy-merged; a chiseled block's remaining voxel-grid cells contribute
/// their bounding box. A solid chiseled block that yields no cell at all
/// (thin wood that fills no half-grid octant) contributes its solid voxels'
/// bounding box, so every model with a solid or leaf block has a box. Other
/// non-solid blocks contribute nothing. Deterministic order.
pub fn model_collider_boxes(model: &VoxelModel) -> Vec<([i32; 3], [i32; 3])> {
    let h = HALF_BLOCK_VOXELS as i32;
    let mut halves: BTreeSet<[i32; 3]> = BTreeSet::new();
    let mut out = Vec::new();
    for (&o, (block, chisel)) in &model.blocks {
        let o2 = o.map(|v| i32::from(v) * 2);
        if is_foliage(block.id) {
            // Leaves: non-solid while they stand, but a falling crown lands
            // on them (a body whose leaves passed through the ground would
            // sink its crown into it). Whole block, chisel or not.
            for (dx, dy, dz) in (0..8).map(|i| (i % 2, (i / 2) % 2, i / 4)) {
                halves.insert([o2[0] + dx, o2[1] + dy, o2[2] + dz]);
            }
            continue;
        }
        let cells = block_collider_cells(*block, chisel.as_ref());
        if cells.blocks.is_empty() && cells.halves.is_empty() && cells.voxels.is_empty() {
            // Thin chiseled wood (a pine or birch limb, a few voxels across)
            // fills no half-grid octant, and a falling piece made only of it
            // would have nothing to land on. In a body (terrain keeps the
            // half grid) such a block collides as its solid voxels' bounding
            // box. Other non-solid blocks still contribute nothing.
            if let Some((lo, hi)) = chisel.as_ref().and_then(|c| solid_voxel_bounds(*block, c)) {
                let base = o.map(|v| i32::from(v) * BLOCK_VOXELS as i32);
                out.push(([0, 1, 2].map(|a| base[a] + lo[a]), [0, 1, 2].map(|a| base[a] + hi[a])));
            }
            continue;
        }
        for _ in &cells.blocks {
            for (dx, dy, dz) in (0..8).map(|i| (i % 2, (i / 2) % 2, i / 4)) {
                halves.insert([o2[0] + dx, o2[1] + dy, o2[2] + dz]);
            }
        }
        for k in &cells.halves {
            halves.insert([o2[0] + k.x, o2[1] + k.y, o2[2] + k.z]);
        }
        if let Some(first) = cells.voxels.first() {
            let (mut lo, mut hi) = (first.to_array(), first.to_array());
            for k in &cells.voxels {
                for (a, v) in k.to_array().into_iter().enumerate() {
                    lo[a] = lo[a].min(v);
                    hi[a] = hi[a].max(v);
                }
            }
            let base = o.map(|v| i32::from(v) * BLOCK_VOXELS as i32);
            out.push(([0, 1, 2].map(|a| base[a] + lo[a]), [0, 1, 2].map(|a| base[a] + hi[a] + 1)));
        }
    }
    // Greedy merge from the lowest remaining cell: along x, then whole rows
    // along y, then whole slabs along z.
    while let Some(&s) = halves.iter().next() {
        let mut e = [1, 1, 1];
        while halves.contains(&[s[0] + e[0], s[1], s[2]]) {
            e[0] += 1;
        }
        let row = |y: i32, z: i32, e0: i32, set: &BTreeSet<[i32; 3]>| (0..e0).all(|x| set.contains(&[s[0] + x, y, z]));
        while row(s[1] + e[1], s[2], e[0], &halves) {
            e[1] += 1;
        }
        while (0..e[1]).all(|y| row(s[1] + y, s[2] + e[2], e[0], &halves)) {
            e[2] += 1;
        }
        for x in 0..e[0] {
            for y in 0..e[1] {
                for z in 0..e[2] {
                    halves.remove(&[s[0] + x, s[1] + y, s[2] + z]);
                }
            }
        }
        out.push((s.map(|v| v * h), [0, 1, 2].map(|a| (s[a] + e[a]) * h)));
    }
    out
}

/// The bounding box `(min, max)` (block-local voxel units, max exclusive) of
/// a chiseled block's solid voxels — voxels of a material that is solid
/// (`BlockProps::is_solid`) — or `None` when it has none, or `block` itself
/// is not solid.
fn solid_voxel_bounds(block: Block, chisel: &ChiselData) -> Option<([i32; 3], [i32; 3])> {
    let props = block_props();
    if !props.is_solid(block.id) {
        return None;
    }
    let mut bounds: Option<([i32; 3], [i32; 3])> = None;
    for (min, max, material) in chisel.cuboids() {
        if material == BlockId::AIR || !props.is_solid(material) {
            continue;
        }
        let (min, max) = (min.map(|v| v as i32), max.map(|v| v as i32));
        bounds = Some(match bounds {
            None => (min, max),
            Some((lo, hi)) => ([0, 1, 2].map(|a| lo[a].min(min[a])), [0, 1, 2].map(|a| hi[a].max(max[a]))),
        });
    }
    bounds
}

impl Rollback {
    /// Detaches the blocks at `positions` into one dynamic voxel body,
    /// undo-safely: the blocks are cleared from the terrain
    /// (`clear_blocks_safe`) and a new entity carries them, its body at the
    /// model's centre of mass, spinning at `angvel` (rad/s, world axes).
    /// Refused positions (bedrock, the edge ring, outside the region) and
    /// air are skipped; `None`, with nothing changed, when nothing is left.
    /// A set with nothing that collides (no solid or leaf block — e.g.
    /// water only) is cleared but spawns no body (it would fall through the
    /// ground until the timeout; `None` too). A leaves-only set falls like
    /// any other.
    pub fn detach_blocks_safe(&mut self, positions: &[BlockPos], policy: SettlePolicy, angvel: Vector) -> Option<EntityKey> {
        let mut taken: BTreeMap<BlockPos, (Block, Option<ChiselData>)> = BTreeMap::new();
        for &pos in positions {
            if let Some(state) = self.clearable_block(pos) {
                taken.insert(pos, state);
            }
        }
        if taken.is_empty() {
            return None;
        }
        let mut origin = [i32::MAX; 3];
        for pos in taken.keys() {
            for a in 0..3 {
                origin[a] = origin[a].min(pos[a]);
            }
        }
        let mut model = VoxelModel::default();
        for (pos, state) in taken {
            let o = [0, 1, 2].map(|a| i16::try_from(pos[a] - origin[a]).expect("a voxel body spans < 32768 blocks"));
            model.blocks.insert(o, state);
        }
        let positions: Vec<BlockPos> = model
            .blocks
            .keys()
            .map(|o| [0, 1, 2].map(|a| origin[a] + i32::from(o[a])))
            .collect();
        self.clear_blocks_safe(&positions);

        // Nothing that collides: with no collider the body would fall
        // through the ground until the timeout, so it settles at once — and
        // as settling is a despawn, it is simply never spawned. The terrain
        // is still cleared (one undoable `clear_blocks_safe`, nothing else
        // to invert).
        let boxes = model_collider_boxes(&model);
        if boxes.is_empty() {
            return None;
        }

        let mprops = model_mass_properties(&model);
        let com = mprops.local_com;
        let origin_units = Vector::new(
            origin[0] as f32 * BLOCK_VOXELS as f32,
            origin[1] as f32 * BLOCK_VOXELS as f32,
            origin[2] as f32 * BLOCK_VOXELS as f32,
        );
        let pose = Pose::from_translation(origin_units + com);

        let e = self.data.ecs.create_entity_safe();
        let user_data = e.data().as_ffi() as u128;
        // The body frame is the COM frame: same mass and inertia, COM at 0.
        let body_mprops = MassProperties { local_com: Vector::ZERO, ..mprops };
        let mut body = RigidBodyBuilder::dynamic()
            .pose(pose)
            .angvel(angvel)
            .can_sleep(true)
            .additional_mass_properties(body_mprops)
            .user_data(user_data)
            .build();
        let weight = mprops.mass() * VOXEL_BODY_GRAVITY * UNITS_PER_METER;
        body.add_force(Vector::new(0.0, -weight, 0.0), false);
        let handle = self.insert_body_safe(body);
        self.data.ecs.rigidbody.set_safe(e, Some(handle));

        let parts: Vec<(Pose, SharedShape)> = boxes
            .into_iter()
            .map(|(lo, hi)| {
                let half = [0, 1, 2].map(|a| (hi[a] - lo[a]) as f32 * 0.5);
                let centre = Vector::new(lo[0] as f32 + half[0], lo[1] as f32 + half[1], lo[2] as f32 + half[2]);
                (Pose::from_translation(centre - com), SharedShape::cuboid(half[0], half[1], half[2]))
            })
            .collect();
        let collider = ColliderBuilder::new(SharedShape::compound(parts))
            .mass_properties(MassProperties::default())
            .user_data(user_data)
            .build();
        // The eigen-solve hazard (module docs): the body's mass must come only
        // from `body_mprops` (identity principal frame, no `SymmetricEigen3`).
        // That holds while every collider adds exactly zero mass — adding a
        // zero `MassProperties` returns the other side untouched — and the
        // compound sits at the identity offset, so rapier never transforms
        // or re-diagonalises a collider's inertia into the body frame.
        debug_assert_eq!(collider.mass_properties(), MassProperties::default(), "a voxel body collider adds no mass");
        debug_assert_eq!(*collider.position(), Pose::IDENTITY, "a voxel body's compound sits at the identity offset");
        self.attach_collider_safe(handle, collider);

        let spawned_tick = *self.data.tick as u64;
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::AddVoxelBody(e, pose, Box::new(model.clone())),
        ));
        self.data
            .ecs
            .voxel_body
            .set_safe(e, Some(VoxelBody { model, origin, policy, spawned_tick, still_ticks: 0 }));
        Some(e)
    }

    /// Once per region tick, after the physics step: count each voxel
    /// body's consecutive still ticks (under both `REST_LINVEL` and
    /// `REST_ANGVEL`, or asleep), and settle it — apply its policy — at
    /// `REST_TICKS` of them, at `SETTLE_TIMEOUT_TICKS` of age, or once its
    /// centre of mass leaves the region's `0..REGION_SIZE` in x or z.
    /// Entities in key order.
    pub fn tick_voxel_bodies_safe(&mut self, tick: u64) {
        let keys: Vec<EntityKey> = self
            .data
            .ecs
            .voxel_body
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|_| k))
            .collect();
        for e in keys {
            let Some(vb) = self.data.ecs.voxel_body.try_get(e).clone() else { continue };
            let Some(handle) = *self.data.ecs.rigidbody.try_get(e) else { continue };
            let Some(body) = self.data.physics.bodies.get(handle) else { continue };
            let (v, w) = (body.linvel(), body.angvel());
            let still = body.is_sleeping()
                || (v.dot(v) < REST_LINVEL * REST_LINVEL && w.dot(w) < REST_ANGVEL * REST_ANGVEL);
            let t = body.translation();
            let outside = !(0.0..REGION_SIZE).contains(&t.x) || !(0.0..REGION_SIZE).contains(&t.z);
            let still_ticks = if still { vb.still_ticks.saturating_add(1) } else { 0 };
            let settle = still_ticks >= REST_TICKS
                || tick.saturating_sub(vb.spawned_tick) >= SETTLE_TIMEOUT_TICKS
                || outside;
            if settle {
                match vb.policy {
                    SettlePolicy::Despawn => self.remove_entity_safe(e, None),
                }
            } else if still_ticks != vb.still_ticks {
                self.data.ecs.voxel_body.set_safe(e, Some(VoxelBody { still_ticks, ..vb }));
            }
        }
    }
}
