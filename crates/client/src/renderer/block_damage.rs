//! Damaged blocks: the cracks of a block being mined and its wobble.
//!
//! The sim keeps what each player mines and what they left damaged
//! (`game::mining`: `Client::mining`, `Client::damaged`); the home region's
//! runner projects every such block of its region each tick
//! (`Rollback::block_damage_views`) and the game thread sends the list when it
//! changes (`ClientUpdateEvent::BlockDamage`), which the bridge stores in
//! [`BlockDamage`]. This module only draws it — nothing here reaches the sim.
//!
//! A damaged block is drawn as its own **piece** over the terrain: one root
//! entity at the block's centre (world space: the region offset is applied
//! here, at the render boundary) with two `ChunkQuads` children on the pulled
//! path, like a voxel body's bricks —
//!
//! - the block's own voxels (chisel included), meshed alone with air around it;
//! - the same quads again as a **crack shell**: every quad re-pointed at one of
//!   the [`CRACK_STAGES`] crack layers appended to the block array texture
//!   ([`crack_images`], [`CrackLayers`]) and drawn alpha-tested, a hair outside
//!   the piece ([`CRACK_LIFT`]).
//!
//! The terrain keeps the block (no remesh per strike), so the piece must hide
//! it: it is never smaller than [`PIECE_SCALE`], and a tilted piece grows by
//! [`cover_scale`] so none of its faces sinks under the block's own.
//!
//! **Wobble** ([`wobble`]): a strike pops the piece ([`WOBBLE_POP`]) and tilts
//! it ([`WOBBLE_TILT_RAD`]) about a horizontal axis that turns with every
//! strike, ringing down within [`WOBBLE_SECS`]. A block being mined is struck
//! when its piece appears or is taken up again, and every [`SWING_SECS`] after
//! (the arm's own swing, `commander::hand`). A block whose damage ran out
//! (left for `game::DAMAGE_LINGER_TICKS`) loses its cracks and wobbles once
//! more, then the piece goes. A block that broke (it left the list while it
//! was being mined) goes at once.
//!
//! **Trees.** A damaged block of tree wood is the **lead** of every other
//! block a chop there would bring down ([`BlockDamage::shake`], from
//! `Rollback::shake_blocks`): each is drawn as a piece of its own (no cracks)
//! and struck with the lead, a little later per block of height
//! ([`SHAKE_RIPPLE_SECS_PER_BLOCK`]), so the whole tree shakes at every swing.
//! When the sim fells the tree (all at once, `GameDataUpdateKind::TreeFelled`
//! → [`TreeFelledMsg`]) the terrain loses every block in one remesh, and the
//! pieces — those already shaking, and new ones for the rest — stand in for
//! them and go top to bottom ([`start_tree_vanish`]): layer by layer
//! ([`FELL_LAYER_SECS`]) each piece is struck once more and shrinks away.
//! The damage list and the felled event travel different channels, so a tree
//! piece whose view is gone lingers one wobble (`leaving`) for the event to
//! take it over.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::f32::consts::TAU;
use std::sync::Arc;

use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::NoAutoAabb;
use bevy::math::Vec3A;
use bevy::prelude::*;
use game::edit::BlockPos;
use game::{BLOCK_VOXELS, BlockDamageView, RegionId, VoxelModel};
use image::{Rgba, RgbaImage};

use super::block_textures::BlockTextureLayers;
use super::commander::hand::SWING_SECS;
use super::meshing::records::{CUTOUT_BIT, INTERIOR_BIT, LAYER_MAX, QuadRecord, record_face};
use super::voxel_bodies::mesh_body;
use super::voxel_pull::ChunkQuads;
use crate::session::SessionScoped;

/// Crack pictures, from the first hairline to the whole face.
pub const CRACK_STAGES: u32 = 8;
/// The crack pictures are drawn on this grid (one cell per voxel of a block
/// face) and scaled up to the block textures' size.
const CRACK_GRID: usize = 16;
/// Crack colour (the alpha test leaves only these texels).
const CRACK_RGBA: [u8; 4] = [22, 20, 18, 255];
/// Scale of a piece at rest, about the block's centre: just over the block,
/// so its faces win the depth test against the terrain's own.
pub const PIECE_SCALE: f32 = 1.006;
/// Scale of the crack shell relative to its piece.
pub const CRACK_LIFT: f32 = 1.006;
/// How long one wobble takes, seconds.
pub const WOBBLE_SECS: f32 = 0.3;
/// Extra scale at the top of a strike's pop.
pub const WOBBLE_POP: f32 = 0.05;
/// Largest tilt of a strike, radians.
pub const WOBBLE_TILT_RAD: f32 = 0.06;
/// The wobble rings at this frequency, Hz.
pub const WOBBLE_HZ: f32 = 9.0;
/// The ringing dies away with this time constant, seconds.
pub const WOBBLE_DECAY_SECS: f32 = 0.08;
/// The pop rises over this long, seconds (not a one-frame jump).
pub const WOBBLE_ATTACK_SECS: f32 = 0.03;
/// The first strike lands this long after a piece appears: the arm's swing
/// has to reach the block first.
pub const FIRST_STRIKE_DELAY_SECS: f32 = SWING_SECS * 0.35;
/// A shaking tree block is struck this long after its lead per block of
/// height between them: the strike runs up (and down) the tree. 0 = at once.
pub const SHAKE_RIPPLE_SECS_PER_BLOCK: f32 = 0.012;
/// A felled tree goes one layer of blocks per this long, top first, seconds…
pub const FELL_LAYER_SECS: f32 = 0.06;
/// …but never longer than this in all (a tall tree's layers go faster).
pub const FELL_MAX_SECS: f32 = 1.5;
/// A vanishing piece shrinks to nothing over the last this long of its wobble.
pub const FELL_SHRINK_SECS: f32 = 0.14;
/// Felled events remembered for replay dedupe.
const RECENT_FELLED_MAX: usize = 64;
/// The same tree felled again within this many ticks is a rollback's replay
/// of the chop (it may land a tick or two off), not another felling.
const FELL_REPLAY_TICKS: u64 = 100;
/// The tilt axis turns by this much per strike, radians (the golden angle:
/// no two strikes in a row tilt the same way).
const STRIKE_AXIS_STEP_RAD: f32 = 2.399_963;

/// The array-texture layers of the crack pictures: stage `s` is layer
/// `first + s`.
#[derive(Resource, Clone, Copy, Debug)]
pub struct CrackLayers {
    pub first: u32,
}

/// The damaged blocks of the home region, as last sent by the game thread.
#[derive(Resource, Default)]
pub struct BlockDamage {
    pub region: Option<RegionId>,
    pub blocks: Vec<BlockDamageView>,
    /// Per listed tree block (the lead): the blocks that shake with it.
    pub shake: Vec<(BlockPos, Vec<(BlockPos, game::Block)>)>,
}

/// A tree the sim felled at once in `region` (`GameDataUpdateKind::TreeFelled`).
#[derive(Message, Debug, Clone)]
pub struct TreeFelledMsg {
    pub region: RegionId,
    pub felled: game::TreeFelledView,
}

/// The quads of a whole (unchiselled) block by block id: a tree is thousands
/// of pieces of a handful of kinds, meshed once each.
#[derive(Resource, Default)]
pub struct PieceMeshes(HashMap<game::BlockId, Arc<Vec<QuadRecord>>>);

impl PieceMeshes {
    fn of(
        &mut self,
        block: game::Block,
        chisel: Option<&game::ChiselData>,
        layers: &BlockTextureLayers,
    ) -> Arc<Vec<QuadRecord>> {
        match chisel {
            Some(_) => Arc::new(piece_records(block, chisel.cloned(), layers)),
            None => self
                .0
                .entry(block.id)
                .or_insert_with(|| Arc::new(piece_records(block, None, layers)))
                .clone(),
        }
    }
}

/// A piece of a felled tree on its way out.
#[derive(Debug, Clone, Copy)]
struct Vanish {
    /// Seconds until its layer's turn.
    delay: f32,
    /// Its turn came: on its last wobble, shrinking.
    struck: bool,
}

/// The root of one damaged block's piece.
#[derive(Component)]
pub struct DamagedBlock {
    region: RegionId,
    target: BlockPos,
    /// What the piece was meshed from (a change re-meshes it).
    block: game::Block,
    chisel: Option<game::ChiselData>,
    /// The piece's quads, kept to build the crack shell of another stage.
    records: Arc<Vec<QuadRecord>>,
    stage: u32,
    /// In the last list as being mined (struck again and again).
    mining: bool,
    /// Its damage ran out: no cracks, one last wobble, then despawned.
    leaving: bool,
    /// Seconds until the next strike while `mining`.
    next_strike: f32,
    /// Seconds since the last strike began (`>= WOBBLE_SECS`: at rest).
    wobble_age: f32,
    strikes: u32,
    /// A tree block that shakes with this damaged block (no cracks, never
    /// struck by itself); `None`: a damaged block.
    lead: Option<BlockPos>,
    /// A damaged block other pieces shake with (a tree's).
    has_shake: bool,
    /// Its tree was felled: no longer follows the damage list.
    vanish: Option<Vanish>,
}

impl DamagedBlock {
    /// One more wobble, then gone.
    fn leave(&mut self) {
        self.leaving = true;
        self.wobble_age = 0.0;
        self.strikes += 1;
    }
}

/// The crack shell child of a [`DamagedBlock`].
#[derive(Component)]
pub struct CrackShell;

/// The crack stage (0..[`CRACK_STAGES`]) drawn at `progress_per_mille`.
pub fn crack_stage(progress_per_mille: u32) -> u32 {
    (progress_per_mille * CRACK_STAGES / 1000).min(CRACK_STAGES - 1)
}

/// A stateless roll in 0..1 from `seed` (the crack pictures must come out the
/// same on every start).
fn roll(seed: u32) -> f32 {
    let mut x = seed.wrapping_mul(0x9E37_79B9) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    x = x.wrapping_mul(0x297A_2D39);
    x ^= x >> 15;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

/// When each cell of the crack grid opens, in (0, 1] (`INFINITY`: never):
/// cracks run out from the middle of the face and fork, so every stage holds
/// the one before it.
fn crack_order() -> [[f32; CRACK_GRID]; CRACK_GRID] {
    const ARMS: u32 = 5;
    const ARM_STEPS: u32 = 11;
    const FORK_STEPS: u32 = 5;
    let mut order = [[f32::INFINITY; CRACK_GRID]; CRACK_GRID];
    let mut mark = |p: Vec2, at: f32| {
        let (x, y) = (p.x.floor() as i32, p.y.floor() as i32);
        if (0..CRACK_GRID as i32).contains(&x) && (0..CRACK_GRID as i32).contains(&y) {
            let cell = &mut order[y as usize][x as usize];
            *cell = cell.min(at.clamp(0.0, 1.0));
        }
    };
    let centre = Vec2::splat(CRACK_GRID as f32 / 2.0);
    for arm in 0..ARMS {
        let mut angle = (arm as f32 + roll(arm * 97 + 1) * 0.6) * TAU / ARMS as f32;
        let mut p = centre;
        for step in 0..ARM_STEPS {
            let seed = arm * 1000 + step * 10;
            angle += (roll(seed + 2) - 0.5) * 1.1;
            p += Vec2::from_angle(angle);
            let at = step as f32 / ARM_STEPS as f32;
            mark(p, at);
            // A fork now and then, opening after the arm reached it.
            if step % 3 == 2 {
                let side = if roll(seed + 3) < 0.5 { -1.0 } else { 1.0 };
                let mut fork_angle = angle + side * (0.7 + roll(seed + 4) * 0.6);
                let mut q = p;
                for f in 0..FORK_STEPS {
                    fork_angle += (roll(seed + 5 + f) - 0.5) * 0.9;
                    q += Vec2::from_angle(fork_angle);
                    mark(q, at + (f + 1) as f32 / ARM_STEPS as f32);
                }
            }
        }
    }
    // Ranked, so every stage opens about as many cells as the one before
    // (the arms leave the face early; by step alone the last stages match).
    let mut opened: Vec<f32> = order
        .iter()
        .flatten()
        .copied()
        .filter(|o| o.is_finite())
        .collect();
    opened.sort_by(f32::total_cmp);
    for cell in order.iter_mut().flatten().filter(|o| o.is_finite()) {
        let rank = opened.partition_point(|o| *o <= *cell);
        *cell = rank as f32 / opened.len() as f32;
    }
    order
}

/// The [`CRACK_STAGES`] crack pictures at the block textures' size `dim`:
/// crack texels opaque, everything else alpha 0 (drawn alpha-tested).
pub fn crack_images(dim: u32) -> Vec<RgbaImage> {
    let order = crack_order();
    (0..CRACK_STAGES)
        .map(|stage| {
            let open = (stage + 1) as f32 / CRACK_STAGES as f32;
            RgbaImage::from_fn(dim, dim, |x, y| {
                let cell =
                    |c: u32| (c as usize * CRACK_GRID / dim.max(1) as usize).min(CRACK_GRID - 1);
                let [r, g, b, a] = CRACK_RGBA;
                // The same colour under alpha 0, so filtering never bleeds another in.
                Rgba([
                    r,
                    g,
                    b,
                    if order[cell(y)][cell(x)] <= open {
                        a
                    } else {
                        0
                    },
                ])
            })
        })
        .collect()
}

/// `records` as a crack shell: every quad on texture layer `layer`, drawn
/// alpha-tested, in `quads_to_records` order (all cutout, by face).
pub fn crack_records(records: &[QuadRecord], layer: u32) -> Vec<QuadRecord> {
    debug_assert!(layer <= LAYER_MAX);
    let mut out: Vec<QuadRecord> = records
        .iter()
        .map(|r| {
            let w0 = (r.0[0] | 1 << CUTOUT_BIT) & !(1 << INTERIOR_BIT);
            let w1 = r.0[1] & !(LAYER_MAX << 12) | layer << 12;
            QuadRecord([w0, w1])
        })
        .collect();
    out.sort_by_key(|r| record_face(*r));
    out
}

/// The scale that keeps every face of a cube tilted by `tilt_rad` outside
/// the untilted cube it covers.
pub fn cover_scale(tilt_rad: f32) -> f32 {
    let t = tilt_rad.abs();
    1.0 / (t.cos() - t.sin()).max(0.5)
}

/// The piece's pose `age` seconds into its `strike`-th wobble: scale and
/// rotation about the block's centre. At rest from [`WOBBLE_SECS`] on.
pub fn wobble(age: f32, strike: u32) -> (f32, Quat) {
    if !(0.0..WOBBLE_SECS).contains(&age) {
        return (PIECE_SCALE, Quat::IDENTITY);
    }
    // Rings down, and ends exactly at rest (no step at `WOBBLE_SECS`).
    let fade = (-age / WOBBLE_DECAY_SECS).exp() * (1.0 - age / WOBBLE_SECS);
    let attack = (age / WOBBLE_ATTACK_SECS).min(1.0);
    let phase = TAU * WOBBLE_HZ * age;
    let pop = WOBBLE_POP * fade * attack * (0.5 + 0.5 * phase.cos());
    let tilt = WOBBLE_TILT_RAD * fade * phase.sin();
    let turn = strike as f32 * STRIKE_AXIS_STEP_RAD;
    let axis = Vec3::new(turn.cos(), 0.0, turn.sin());
    (
        (PIECE_SCALE + pop) * cover_scale(tilt),
        Quat::from_axis_angle(axis, tilt),
    )
}

/// World position of the centre of block `target` of `region`, at its copy
/// in `frame`.
fn block_centre(frame: &crate::wrap_frame::WrapFrame, region: RegionId, target: BlockPos) -> Vec3 {
    let o = frame.offset(region);
    let b = BLOCK_VOXELS as f32;
    Vec3::new(o[0], o[1], o[2]) + (IVec3::from_array(target).as_vec3() + 0.5) * b
}

/// The quads of one block (its chisel included) alone in the air, in the
/// block's own voxel frame (0..`BLOCK_VOXELS` per axis).
fn piece_records(
    block: game::Block,
    chisel: Option<game::ChiselData>,
    layers: &BlockTextureLayers,
) -> Vec<QuadRecord> {
    let model = VoxelModel {
        blocks: [([0i16; 3], (block, chisel))].into_iter().collect(),
    };
    mesh_body(&model, layers)
        .bricks
        .into_iter()
        .find(|(b, _)| *b == [0; 3])
        .map(|(_, r)| r)
        .unwrap_or_default()
}

/// A `ChunkQuads` child of piece `root`, scaled by `scale` about the block's centre.
fn quads_child(root: Entity, records: Arc<Vec<QuadRecord>>, scale: f32) -> impl Bundle {
    let half = BLOCK_VOXELS as f32 / 2.0;
    (
        Transform::from_translation(Vec3::splat(-half * scale)).with_scale(Vec3::splat(scale)),
        ChunkQuads(records),
        Aabb {
            center: Vec3A::splat(half),
            half_extents: Vec3A::splat(half),
        },
        NoAutoAabb,
        ChildOf(root),
    )
}

/// Spawn `piece` at its block, with a crack shell of `stage` if given.
/// Returns its root.
fn spawn_piece(
    commands: &mut Commands,
    frame: &crate::wrap_frame::WrapFrame,
    piece: DamagedBlock,
    shell: Option<u32>,
) -> Entity {
    let records = piece.records.clone();
    let centre = block_centre(frame, piece.region, piece.target);
    let root = commands
        .spawn((
            piece,
            Transform::from_translation(centre).with_scale(Vec3::splat(PIECE_SCALE)),
            Visibility::default(),
            SessionScoped,
        ))
        .id();
    if let Some(layer) = shell {
        commands.spawn((
            CrackShell,
            quads_child(root, Arc::new(crack_records(&records, layer)), CRACK_LIFT),
        ));
    }
    commands.spawn(quads_child(root, records, 1.0));
    root
}

/// A piece at rest: not mined, not struck.
fn resting_piece(
    region: RegionId,
    target: BlockPos,
    block: game::Block,
    records: Arc<Vec<QuadRecord>>,
) -> DamagedBlock {
    DamagedBlock {
        region,
        target,
        block,
        chisel: None,
        records,
        stage: 0,
        mining: false,
        leaving: false,
        next_strike: FIRST_STRIKE_DELAY_SECS,
        wobble_age: WOBBLE_SECS,
        strikes: 0,
        lead: None,
        has_shake: false,
        vanish: None,
    }
}

/// Bring the pieces in line with [`BlockDamage`]: spawn, re-stage, re-mesh,
/// and send off those no longer listed. Pieces of a felled tree
/// (`vanish`) are left alone.
pub fn sync_block_damage(
    mut commands: Commands,
    damage: Res<BlockDamage>,
    layers: Option<Res<BlockTextureLayers>>,
    cracks: Option<Res<CrackLayers>>,
    mut meshes: ResMut<PieceMeshes>,
    mut pieces: Query<(Entity, &mut DamagedBlock, Option<&Children>)>,
    shells: Query<(), With<CrackShell>>,
    frame: Option<Res<crate::wrap_frame::WrapFrame>>,
) {
    if !damage.is_changed() {
        return;
    }
    let frame = frame.as_deref().copied().unwrap_or_default();
    let (Some(layers), Some(cracks)) = (layers, cracks) else {
        return;
    };
    let listed = |region: RegionId, target: BlockPos| {
        damage
            .blocks
            .iter()
            .find(|v| damage.region == Some(region) && v.target == target)
    };
    // Every block that shakes, with its lead (the first that names it); a
    // damaged block is drawn as itself.
    let mut shake: BTreeMap<BlockPos, (BlockPos, game::Block)> = BTreeMap::new();
    for (lead, blocks) in damage
        .shake
        .iter()
        .filter(|(lead, _)| damage.blocks.iter().any(|v| v.target == *lead))
    {
        for (p, b) in blocks {
            shake.entry(*p).or_insert((*lead, *b));
        }
    }
    for v in &damage.blocks {
        shake.remove(&v.target);
    }
    let leads: BTreeSet<BlockPos> = shake.values().map(|(lead, _)| *lead).collect();
    let mut drawn: BTreeSet<BlockPos> = BTreeSet::new();
    for (e, mut piece, children) in &mut pieces {
        let here = damage.region == Some(piece.region);
        if piece.vanish.is_some() {
            if here {
                drawn.insert(piece.target);
            }
            continue;
        }
        if piece.lead.is_some() {
            if listed(piece.region, piece.target).is_some() {
                // Now damaged itself: a piece with cracks takes its place (below).
                commands.entity(e).despawn();
                continue;
            }
            match shake
                .get(&piece.target)
                .filter(|(_, b)| here && *b == piece.block)
            {
                Some((lead, _)) => {
                    drawn.insert(piece.target);
                    piece.lead = Some(*lead);
                    piece.leaving = false;
                }
                None if !piece.leaving => piece.leave(),
                None => {}
            }
            continue;
        }
        let drop_shell = |commands: &mut Commands| {
            for child in children
                .into_iter()
                .flatten()
                .filter(|c| shells.contains(**c))
            {
                commands.entity(*child).despawn();
            }
        };
        let Some(view) = listed(piece.region, piece.target) else {
            if piece.mining && !piece.has_shake {
                // It broke (or its miner left with it): nothing to show.
                commands.entity(e).despawn();
            } else if !piece.leaving {
                // Its damage ran out: whole again, with one last wobble. A
                // tree's lead waits the same way for its felling.
                drop_shell(&mut commands);
                piece.leave();
            }
            continue;
        };
        if view.block != piece.block || view.chisel != piece.chisel {
            // Another block stands there now: a new piece (below).
            commands.entity(e).despawn();
            continue;
        }
        drawn.insert(piece.target);
        let stage = crack_stage(view.progress_per_mille);
        if stage != piece.stage || piece.leaving {
            drop_shell(&mut commands);
            let shell = Arc::new(crack_records(&piece.records, cracks.first + stage));
            commands.spawn((CrackShell, quads_child(e, shell, CRACK_LIFT)));
            piece.stage = stage;
        }
        if view.mining && (!piece.mining || piece.leaving) {
            piece.next_strike = FIRST_STRIKE_DELAY_SECS;
        }
        piece.mining = view.mining;
        piece.leaving = false;
        piece.has_shake = leads.contains(&piece.target);
    }
    let Some(region) = damage.region else { return };
    for view in damage.blocks.iter().filter(|v| !drawn.contains(&v.target)) {
        let records = meshes.of(view.block, view.chisel.as_ref(), &layers);
        if records.is_empty() {
            continue;
        }
        let stage = crack_stage(view.progress_per_mille);
        let piece = DamagedBlock {
            chisel: view.chisel.clone(),
            stage,
            mining: view.mining,
            has_shake: leads.contains(&view.target),
            ..resting_piece(region, view.target, view.block, records)
        };
        spawn_piece(&mut commands, &frame, piece, Some(cracks.first + stage));
    }
    for (target, (lead, block)) in shake.iter().filter(|(target, _)| !drawn.contains(*target)) {
        let records = meshes.of(*block, None, &layers);
        if records.is_empty() {
            continue;
        }
        spawn_piece(
            &mut commands,
            &frame,
            DamagedBlock {
                lead: Some(*lead),
                ..resting_piece(region, *target, *block, records)
            },
            None,
        );
    }
}

/// Seconds between two layers of a felled tree `layers` blocks tall.
pub fn fell_layer_secs(layers: i32) -> f32 {
    FELL_LAYER_SECS.min(FELL_MAX_SECS / layers.max(1) as f32)
}

/// A tree was felled: its blocks are gone from the terrain, so every one is
/// a piece now — the ones already shaking, new ones for the rest — and each
/// waits its layer's turn, top first.
pub fn start_tree_vanish(
    mut commands: Commands,
    mut msgs: MessageReader<TreeFelledMsg>,
    layers: Option<Res<BlockTextureLayers>>,
    mut meshes: ResMut<PieceMeshes>,
    mut recent: Local<VecDeque<(RegionId, BlockPos, u64)>>,
    mut pieces: Query<(Entity, &mut DamagedBlock, Option<&Children>)>,
    shells: Query<(), With<CrackShell>>,
    frame: Option<Res<crate::wrap_frame::WrapFrame>>,
) {
    let Some(layers) = layers else { return };
    let frame = frame.as_deref().copied().unwrap_or_default();
    let mut standing: Option<BTreeMap<(RegionId, BlockPos), Entity>> = None;
    for msg in msgs.read() {
        let blocks = &msg.felled.blocks;
        let Some(&(first, _)) = blocks.first() else {
            continue;
        };
        let tick = msg.felled.tick;
        if recent
            .iter()
            .any(|&(r, b, t)| r == msg.region && b == first && t.abs_diff(tick) < FELL_REPLAY_TICKS)
        {
            continue;
        }
        recent.push_back((msg.region, first, tick));
        if recent.len() > RECENT_FELLED_MAX {
            recent.pop_front();
        }
        let standing = standing.get_or_insert_with(|| {
            pieces
                .iter()
                .filter(|(_, p, _)| p.vanish.is_none())
                .map(|(e, p, _)| ((p.region, p.target), e))
                .collect()
        });
        let top = blocks.iter().map(|(p, _)| p[1]).max().unwrap_or(first[1]);
        let bottom = blocks.iter().map(|(p, _)| p[1]).min().unwrap_or(first[1]);
        let layer_secs = fell_layer_secs(top - bottom + 1);
        for &(target, block) in blocks {
            let vanish = Some(Vanish {
                delay: (top - target[1]) as f32 * layer_secs,
                struck: false,
            });
            if let Some(e) = standing.remove(&(msg.region, target)) {
                if let Ok((_, mut piece, children)) = pieces.get_mut(e) {
                    if piece.block == block {
                        for child in children
                            .into_iter()
                            .flatten()
                            .filter(|c| shells.contains(**c))
                        {
                            commands.entity(*child).despawn();
                        }
                        piece.vanish = vanish;
                        piece.mining = false;
                        piece.leaving = false;
                        continue;
                    }
                }
                commands.entity(e).despawn();
            }
            let records = meshes.of(block, None, &layers);
            if records.is_empty() {
                continue;
            }
            spawn_piece(
                &mut commands,
                &frame,
                DamagedBlock {
                    vanish,
                    ..resting_piece(msg.region, target, block, records)
                },
                None,
            );
        }
    }
}

/// Strike the pieces being mined (and a tree's with its lead, and a felled
/// tree's as their layer's turn comes), pose every piece by its wobble, and
/// despawn those that finished their last one.
pub fn animate_block_damage(
    mut commands: Commands,
    time: Res<Time>,
    mut pieces: Query<(Entity, &mut DamagedBlock, &mut Transform)>,
) {
    let dt = time.delta_secs();
    // The leads struck this frame, and their strike count.
    let mut struck: Vec<(RegionId, BlockPos, u32)> = Vec::new();
    for (_, mut piece, _) in &mut pieces {
        piece.wobble_age += dt;
        if piece.mining && !piece.leaving && piece.lead.is_none() && piece.vanish.is_none() {
            piece.next_strike -= dt;
            if piece.next_strike <= 0.0 {
                piece.next_strike += SWING_SECS;
                piece.wobble_age = 0.0;
                piece.strikes += 1;
                if piece.has_shake {
                    struck.push((piece.region, piece.target, piece.strikes));
                }
            }
        }
    }
    for (e, mut piece, mut transform) in &mut pieces {
        let piece = &mut *piece;
        let mut shrink = 1.0;
        if let Some(vanish) = piece.vanish.as_mut() {
            if !vanish.struck {
                vanish.delay -= dt;
                if vanish.delay <= 0.0 {
                    vanish.struck = true;
                    piece.wobble_age = 0.0;
                    // Its own tilt axis: a layer does not lean as one slab.
                    piece.strikes = piece
                        .strikes
                        .wrapping_add((piece.target[0] * 7 + piece.target[2] * 13) as u32 | 1);
                }
            }
            if vanish.struck {
                if piece.wobble_age >= WOBBLE_SECS {
                    commands.entity(e).despawn();
                    continue;
                }
                shrink = ((WOBBLE_SECS - piece.wobble_age) / FELL_SHRINK_SECS).clamp(0.0, 1.0);
            }
        } else if piece.leaving {
            if piece.wobble_age >= WOBBLE_SECS {
                commands.entity(e).despawn();
                continue;
            }
        } else if let Some(lead) = piece.lead {
            if let Some(&(_, _, strikes)) = struck
                .iter()
                .find(|(r, t, _)| *r == piece.region && *t == lead)
            {
                // A negative age is at rest: the strike arrives later up the tree.
                piece.wobble_age =
                    -SHAKE_RIPPLE_SECS_PER_BLOCK * (piece.target[1] - lead[1]).abs() as f32;
                piece.strikes = strikes;
            }
        }
        let (scale, rotation) = wobble(piece.wobble_age, piece.strikes);
        let scale = scale * shrink;
        if transform.rotation != rotation || transform.scale.x != scale {
            transform.rotation = rotation;
            transform.scale = Vec3::splat(scale);
        }
    }
}
