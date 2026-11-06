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
//! user force of `mass × VOXEL_BODY_GRAVITY` (the world's
//! `movement::GRAVITY`) downward, set once before insertion (rapier keeps
//! user forces across steps).
//!
//! **Toppling.** A felled tree part (`detach_blocks_pivoting_safe`) starts
//! in the rigid motion of a turn about its butt, not a spin about its centre
//! of mass (which swings the butt up and over), spun hard enough to go over
//! its base; its lowest branches do not collide (they would snap); and once
//! it lands its turning is damped hard, as a rigid crown cannot crush.
//! Leaves collide in a body (not standing). Colliders grip (high friction)
//! and never bounce, and soft CCD stops a fast landing at the surface.
//!
//! **Crossing seams.** A voxel body is an [`crate::EntityKind::Body`] with an
//! `EntityId`, so `scan_boundaries` hands it to the neighbour its centre of
//! mass moves into, like any dynamic body (`body.rs`): its `BodyState`
//! (velocities, explicit mass, weight, damping, sleep) and its compound
//! collider (read back exactly as a `ColliderSpec`) rebuild it there, and
//! the [`VoxelBody`] travels in `Carried` (`migrate.rs`) — its model (the
//! receiver re-announces it to the renderer, `AddVoxelBody`), its settle
//! state, and its [`VoxelBody::drops`], which leave in `Carried::drops` so
//! a body lost on the way (the manager's mailbox overflow) puts them down
//! instead of losing them. The collider's material (friction, restitution)
//! is not in the spec: the receiver sets it again (`set_voxel_body_material_safe`).

use std::collections::{BTreeMap, BTreeSet};

use rapier3d::math::{Pose, Vector};
use rapier3d::prelude::{ColliderBuilder, MassProperties, RigidBodyBuilder, SharedShape};
use slotmapd::Key as _;
use state_hash::StateHash;

use crate::edit::BlockPos;
use crate::movement::UNITS_PER_METER;
use crate::{
    block_collider_cells, block_props, Block, BlockId, ChiselData, ColliderScale, EntityKey, GameDataTransactionKind,
    GameDataUpdate, GameDataUpdateKind, Rollback, BLOCK_VOXELS, HALF_BLOCK_VOXELS, TICK_RATE,
};

/// Density of tree wood (logs and branches: `ColliderScale::Half` blocks),
/// kg/m³.
pub const WOOD_DENSITY: f32 = 700.0;
/// Density of non-solid blocks (leaves), kg/m³.
pub const LEAF_DENSITY: f32 = 60.0;
/// Density of every other block (stone, soil), kg/m³.
pub const STONE_DENSITY: f32 = 2600.0;
/// Downward acceleration of a voxel body, m/s²: the world's gravity
/// (`movement::GRAVITY`, what players and slimes fall with), so a felled
/// tree falls like everything else rather than floating down at a real-world
/// 9.81 in a world whose bodies fall at 45.
pub const VOXEL_BODY_GRAVITY: f32 = crate::movement::GRAVITY;
/// Friction coefficient of a voxel body's colliders: high, so a toppling
/// tree's butt grips the ground and the body pivots about it rather than
/// skating out from under its crown.
pub const VOXEL_BODY_FRICTION: f32 = 0.9;
/// Restitution of a voxel body's colliders: none — a felled trunk lands, it
/// does not bounce.
pub const VOXEL_BODY_RESTITUTION: f32 = 0.0;
/// Angular damping of a voxel body (1/s): a little, so a landed crown
/// settles instead of rocking on its leaves.
pub const VOXEL_BODY_ANGULAR_DAMPING: f32 = 0.5;
/// Angular damping of a voxel body once it has landed (1/s): heavy. A
/// toppling tree whose crown hits the ground keeps turning about the crown
/// contact, and as a rigid body (its crown cannot crush) that would lever
/// the butt up off the ground; damped, the turn dies and gravity sets the
/// butt back down, the tree lying sloped on its crown.
pub const VOXEL_BODY_LANDED_ANGULAR_DAMPING: f32 = 8.0;
/// A body counts as landed ([`VoxelBody::landed`]) once its up axis is
/// tilted from the vertical past the angle with this cosine (30°) on a tick
/// that an impact stopped its fall ([`LANDING_IMPACT_TICKS`]): a felled
/// tree's butt landing on the stump comes before any such tilt, its crown
/// landing after it.
pub const LANDED_TILT_COS: f32 = 0.866_025_4;
/// An impact: the centre of mass's downward speed dropped in one tick by
/// more than this many ticks of gravity.
pub const LANDING_IMPACT_TICKS: f32 = 2.0;
/// Soft-CCD prediction distance of a voxel body (units): one block. At the
/// world's gravity a body is moving a block per tick after about half a
/// second of fall, and a contact rapier only finds once the shapes overlap
/// lets it sink that far into the ground on impact before it is pushed back
/// out (or, for a thin collider, through). Soft CCD finds the contact up to
/// this far ahead, so the landing is stopped at the surface.
pub const VOXEL_BODY_SOFT_CCD: f32 = BLOCK_VOXELS as f32;
/// How much more spin than just enough a toppling body starts with (see
/// `topple_angvel`): enough to clear its base's edge after losing some to
/// the landing on its stump.
pub const TOPPLE_MARGIN: f32 = 1.3;
/// Most spin (rad/s) `topple_angvel` raises a toppling body to: a small
/// sapling on a crown as wide as it is tall would need a violent flip to go
/// over, and simply sits down instead.
pub const TOPPLE_MAX_ANGVEL: f32 = 1.2;
/// Block layers at the bottom of a toppling tree part whose crown blocks
/// have no collider (see [`collision_model`]).
pub const SKIRT_SNAP_LAYERS: i16 = 2;
/// A toppling part at least this many blocks tall is a tree (rather than a
/// sapling): it snaps its skirt ([`collision_model`]) and is spun hard
/// enough to go over its base (`topple_angvel`). A sapling's crown is most
/// of it and has no trunk weight to snap it, and spun over a crown as wide
/// as it is tall it would roll away; it just sits down.
pub const TALL_PART_BLOCKS: i16 = 8;
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
    /// Removed, leaving nothing behind (a fortress chop order: no items).
    #[default]
    Despawn,
    /// Removed, its blocks turned into their drops (`drop_of`, summed per
    /// item at the detach: [`VoxelBody::drops`]) and deposited as piles at
    /// the block cell its centre of mass rests in — in whichever region
    /// then owns it (it crosses seams while it falls); a full pile spills to
    /// the cells around it — a tree a player felled: the logs lie where it
    /// landed.
    DropItems,
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
    /// Whether it has landed: tilted past [`LANDED_TILT_COS`] when an impact
    /// stopped its fall — a felled tree's crown on the ground. From then on
    /// it turns under [`VOXEL_BODY_LANDED_ANGULAR_DAMPING`].
    pub landed: bool,
    /// Its centre of mass's downward speed (units/s) after the last tick,
    /// for telling an impact ([`LANDING_IMPACT_TICKS`]).
    pub fall_speed: f32,
    /// What it turns into once settled ([`SettlePolicy::DropItems`]: its
    /// blocks' drops, [`drop_totals`], fixed at the detach; empty for
    /// [`SettlePolicy::Despawn`]). In transit it rides `Carried::drops`
    /// instead (empty here), so it is never carried twice.
    pub drops: Vec<crate::ItemStack>,
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

/// The blocks of `model` that collide. All of them, unless `toppling` (a
/// felled tree part turning about its butt): then not its skirt — the crown
/// blocks (leaves and limb wood, `Block::limb` ≠ 0) of its lowest
/// [`SKIRT_SNAP_LAYERS`] layers — when it has trunk wood to land on and is at
/// least [`TALL_PART_BLOCKS`] tall. Those branches would snap under the
/// falling trunk; rigid, a pine's lowest whorls land flat on the stump and
/// the tree stands on them. A part with no trunk (a cut-off limb) or a
/// sapling keeps everything.
pub fn collision_model(model: &VoxelModel, toppling: bool) -> VoxelModel {
    if !toppling {
        return model.clone();
    }
    let props = block_props();
    let crown = |b: &Block| b.tree_id() != 0 && (is_foliage(b.id) || b.limb() != 0);
    let has_trunk = model.blocks.values().any(|(b, _)| b.tree_id() != 0 && b.limb() == 0 && props.is_solid(b.id));
    if !has_trunk || !is_tall(model) {
        return model.clone();
    }
    let floor = model.blocks.keys().map(|o| o[1]).min().unwrap_or(0);
    let blocks = model
        .blocks
        .iter()
        .filter(|(o, (b, _))| !(crown(b) && o[1] < floor + SKIRT_SNAP_LAYERS))
        .map(|(o, v)| (*o, v.clone()))
        .collect();
    VoxelModel { blocks }
}

/// Whether the model spans at least [`TALL_PART_BLOCKS`] block layers.
fn is_tall(model: &VoxelModel) -> bool {
    let (lo, hi) = model.blocks.keys().fold((i16::MAX, i16::MIN), |(lo, hi), o| (lo.min(o[1]), hi.max(o[1])));
    hi >= lo && hi - lo + 1 >= TALL_PART_BLOCKS
}

/// `angvel` for a body toppling about `pivot` (region-local units) — when
/// it is tall ([`TALL_PART_BLOCKS`]) — raised to at least [`TOPPLE_MARGIN`] ×
/// the spin that carries its centre of mass over the hardest of the supports
/// it can tip onto: the leading bottom edge (along the direction the spin
/// moves its top) of each colliding block (`colliding`, the
/// [`collision_model`]) in its lowest third. Tipping over such an edge `p`
/// raises the centre of mass by `√(e² + h²) − h` (`e`, `h` its horizontal
/// and vertical distance from `p`), which the spin's energy about `p`,
/// `½ (I_axis + m (e² + h²)) ω²`, must cover. A part resting on wide lower
/// limbs (a pine's whorls) would otherwise land on them and stand, or teeter.
/// Capped at [`TOPPLE_MAX_ANGVEL`]. `I_axis` is the model-axis inertia about
/// the spin axis (diagonal, as everywhere here). `+ - * /` and `sqrt` only
/// (IEEE-exact): bit-identical on every target.
fn topple_angvel(
    model: &VoxelModel,
    colliding: &VoxelModel,
    origin_units: Vector,
    mprops: &MassProperties,
    angvel: Vector,
) -> Vector {
    let w = angvel.length();
    let m = mprops.mass();
    if w <= 0.0 || m <= 0.0 || !is_tall(model) {
        return angvel;
    }
    let axis = angvel / w;
    // Spinning about `axis` moves a point above the pivot along `axis × Y`.
    let d = axis.cross(Vector::Y);
    let b = BLOCK_VOXELS as f32;
    let (floor, top) = model.blocks.keys().fold((i16::MAX, i16::MIN), |(lo, hi), o| (lo.min(o[1]), hi.max(o[1])));
    let third = floor + (top - floor + 1) / 3;
    let reach = b * 0.5 * (d.x.abs() + d.z.abs());
    let com = origin_units + mprops.local_com;
    let inv = mprops.inv_principal_inertia;
    let inertia = |i: f32| if i > 0.0 { 1.0 / i } else { 0.0 };
    let i_axis = axis.x * axis.x * inertia(inv.x) + axis.y * axis.y * inertia(inv.y) + axis.z * axis.z * inertia(inv.z);
    let g = VOXEL_BODY_GRAVITY * UNITS_PER_METER;
    let mut needed = 0.0f32;
    for o in colliding.blocks.keys().filter(|o| o[1] <= third) {
        let bottom = origin_units + Vector::new(f32::from(o[0]) + 0.5, f32::from(o[1]), f32::from(o[2]) + 0.5) * b;
        let (e, h) = ((bottom - com).dot(d) + reach, com.y - bottom.y);
        if e <= 0.0 || h <= 0.0 {
            continue;
        }
        let lever2 = e * e + h * h;
        let rise = lever2.sqrt() - h;
        needed = needed.max((2.0 * m * g * rise / (i_axis + m * lever2)).sqrt());
    }
    axis * w.max((TOPPLE_MARGIN * needed).min(TOPPLE_MAX_ANGVEL))
}

/// The drops (`drop_of`; leaves drop nothing) of `blocks`, summed per item,
/// in `ItemId` order.
pub fn drop_totals(blocks: impl IntoIterator<Item = Block>) -> Vec<crate::ItemStack> {
    let mut totals: BTreeMap<crate::ItemId, u32> = BTreeMap::new();
    for block in blocks {
        if let Some(stack) = crate::drop_of(block) {
            *totals.entry(stack.item).or_default() += stack.count;
        }
    }
    totals.into_iter().map(|(item, count)| crate::ItemStack { item, count }).collect()
}

impl Rollback {
    /// Deposits the drops of `blocks` ([`drop_totals`]) at `cell`.
    pub(crate) fn drop_blocks_at_safe(&mut self, cell: BlockPos, blocks: impl IntoIterator<Item = Block>, tick: u64) {
        self.deposit_drops_safe(cell, &drop_totals(blocks), tick);
    }

    /// Deposits `drops` at `cell` — one deposit per stack, whose merge /
    /// free-cell rule spills what one pile cannot hold to the cells around
    /// it. Pickable after `PICKUP_DELAY_TICKS`. Losses to the pile caps are
    /// counted by the deposit.
    pub(crate) fn deposit_drops_safe(&mut self, cell: BlockPos, drops: &[crate::ItemStack], tick: u64) {
        for stack in drops {
            let _destroyed = self.deposit_items_safe(cell, *stack, tick, crate::PICKUP_DELAY_TICKS);
        }
    }

    /// A voxel body's collider material (not part of its `ColliderSpec`):
    /// set on every collider of `handle`'s body, undo-safely — on a body
    /// rebuilt from a crossing's bundle.
    pub(crate) fn set_voxel_body_material_safe(&mut self, handle: rapier3d::prelude::RigidBodyHandle) {
        let Some(body) = self.data.physics.bodies.get(handle) else { return };
        for ch in body.colliders().to_vec() {
            self.data.physics.colliders.mutate_collider_safe(ch, |c| {
                c.set_friction(VOXEL_BODY_FRICTION);
                c.set_restitution(VOXEL_BODY_RESTITUTION);
            });
        }
    }

    /// Detaches the blocks at `positions` into one dynamic voxel body,
    /// undo-safely: the blocks are cleared from the terrain
    /// (`clear_blocks_safe`) and a new [`crate::EntityKind::Body`] entity
    /// (with an `EntityId`: it crosses seams) carries them, its body at the
    /// model's centre of mass, spinning at `angvel` (rad/s, world axes).
    /// Refused positions (bedrock, outside the region) and
    /// air are skipped; `None`, with nothing changed, when nothing is left.
    /// A set with nothing that collides (no solid or leaf block — e.g.
    /// water only) is cleared but spawns no body (it would fall through the
    /// ground until the timeout; `None` too). A leaves-only set falls like
    /// any other.
    pub fn detach_blocks_safe(&mut self, positions: &[BlockPos], policy: SettlePolicy, angvel: Vector) -> Option<EntityKey> {
        self.detach_blocks_pivoting_safe(positions, policy, angvel, None)
    }

    /// As [`Self::detach_blocks_safe`], with the body starting in the rigid
    /// motion of a spin at `angvel` about `pivot` (region-local units) — a
    /// felled tree turning about its butt: the centre of mass moves at
    /// `angvel × (com − pivot)`, so the pivot itself starts still — the spin
    /// raised to what topples the body over the edge of its base
    /// (`topple_angvel`). `None` spins about the centre of mass at `angvel`
    /// (no linear velocity).
    pub fn detach_blocks_pivoting_safe(
        &mut self,
        positions: &[BlockPos],
        policy: SettlePolicy,
        angvel: Vector,
        pivot: Option<Vector>,
    ) -> Option<EntityKey> {
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
        let colliding = collision_model(&model, pivot.is_some());
        let boxes = model_collider_boxes(&colliding);
        if boxes.is_empty() {
            if policy == SettlePolicy::DropItems {
                // Settled at once, where it stood: at the middle of its
                // blocks' bounds.
                let mut hi = [0i16; 3];
                for o in model.blocks.keys() {
                    for a in 0..3 {
                        hi[a] = hi[a].max(o[a]);
                    }
                }
                let cell = [0, 1, 2].map(|a| origin[a] + i32::from(hi[a]) / 2);
                let blocks: Vec<Block> = model.blocks.values().map(|(b, _)| *b).collect();
                self.drop_blocks_at_safe(cell, blocks, *self.data.tick);
            }
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
        let angvel = match pivot {
            Some(_) => topple_angvel(&model, &colliding, origin_units, &mprops, angvel),
            None => angvel,
        };
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
            .angular_damping(VOXEL_BODY_ANGULAR_DAMPING)
            .soft_ccd_prediction(VOXEL_BODY_SOFT_CCD)
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
            .friction(VOXEL_BODY_FRICTION)
            .restitution(VOXEL_BODY_RESTITUTION)
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

        let spawned_tick = *self.data.tick;
        // The model first, then the kind: a renderer seeing a `Body` kind
        // already knows it is a voxel body (no placeholder box).
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::AddVoxelBody(e, pose, Box::new(model.clone())),
        ));
        self.set_body_kind_safe(e);
        let drops = match policy {
            SettlePolicy::Despawn => Vec::new(),
            SettlePolicy::DropItems => drop_totals(model.blocks.values().map(|(b, _)| *b)),
        };
        self.data.ecs.voxel_body.set_safe(
            e,
            Some(VoxelBody { model, origin, policy, spawned_tick, still_ticks: 0, landed: false, fall_speed: 0.0, drops }),
        );
        Some(e)
    }

    /// Once per region tick, after the physics step: count each voxel
    /// body's consecutive still ticks (under both `REST_LINVEL` and
    /// `REST_ANGVEL`, or asleep), and settle it — apply its policy — at
    /// `REST_TICKS` of them, or at `SETTLE_TIMEOUT_TICKS` of age; and mark
    /// it landed (heavier angular damping) when it is. A body whose centre
    /// of mass leaves the region is not settled: `scan_boundaries` hands it
    /// to the neighbour.
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
            // Bodies start upright (identity rotation): the tilt of the up
            // axis is `(rotation · Y).y`, a quaternion rotation (+ - *).
            let fall_speed = -v.y;
            let impact = vb.fall_speed - fall_speed
                > LANDING_IMPACT_TICKS * VOXEL_BODY_GRAVITY * UNITS_PER_METER * crate::movement::TICK_DT;
            let land = !vb.landed && (body.rotation() * Vector::Y).y < LANDED_TILT_COS && impact;
            let still = body.is_sleeping()
                || (v.dot(v) < REST_LINVEL * REST_LINVEL && w.dot(w) < REST_ANGVEL * REST_ANGVEL);
            let still_ticks = if still { vb.still_ticks.saturating_add(1) } else { 0 };
            let settle = still_ticks >= REST_TICKS || tick.saturating_sub(vb.spawned_tick) >= SETTLE_TIMEOUT_TICKS;
            if settle {
                match vb.policy {
                    SettlePolicy::Despawn => self.remove_entity_safe(e, None),
                    SettlePolicy::DropItems => {
                        // The body frame is the centre-of-mass frame: the
                        // drops go to the cell its centre of mass rests in
                        // (past the seam within the flip hysteresis, the
                        // neighbour's, by an intent to its owner).
                        let t = body.translation();
                        let bv = BLOCK_VOXELS as f32;
                        let cell = [t.x, t.y, t.z].map(|v| (v / bv).floor() as i32);
                        self.remove_entity_safe(e, None);
                        self.deposit_drops_safe(cell, &vb.drops, tick);
                    }
                }
                continue;
            }
            if land {
                self.data
                    .physics
                    .bodies
                    .mutate_body_safe(handle, |b| b.set_angular_damping(VOXEL_BODY_LANDED_ANGULAR_DAMPING));
            }
            if still_ticks != vb.still_ticks || land || fall_speed.to_bits() != vb.fall_speed.to_bits() {
                let landed = vb.landed || land;
                self.data.ecs.voxel_body.set_safe(e, Some(VoxelBody { still_ticks, landed, fall_speed, ..vb }));
            }
        }
    }
}
