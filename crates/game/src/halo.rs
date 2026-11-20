//! The read-only terrain halo: each region holds a mirror of its neighbours'
//! terrain chunks within [`HALO_BLOCKS`] of their shared edges (spec Part 3,
//! P5). Halo chunks are fixed colliders in the region's physics, so a body
//! walking across a seam has a floor before its ownership flips, and the
//! eye ray and tile queries see past the edge.
//!
//! The halo is ordinary rollback state (`GameData::halo`), changed only by
//! [`GameEventKind::HaloUpdate`](crate::GameEventKind::HaloUpdate) events:
//! - a region's tick (and the slot's events after it) that changed a chunk
//!   within the band sends [`HaloUpdate`]s to the neighbours that mirror it,
//!   stamped like departures (`RegionOutput::HaloDeltas`), so
//!   they land in slot T+1 and the client replays them in that slot;
//! - a starting region is seeded before its first tick: from each
//!   neighbour's running state (`RegionInput::HaloRequest`), its parked /
//!   cached terrain, or worldgen ([`HaloUpdate::replace`] seeds).
//!
//! Read-only: edits stay with the owner (a raycast may hit a halo block; an
//! edit of it is an intent to the owner, `intents.rs`). Below a neighbour's
//! materialised band the halo reads the neighbour's implied ground from its
//! ground recipes (`GameData::halo_ground`, carried by seeds), as
//! `implied_block` does in the region itself — solid until dug, never air.
//!
//! Implied ground at a seam: a break that opens air next to the neighbour's
//! implied ground cannot materialise it (it is not this region's). The
//! neighbour does, when the halo delta of the new air reaches it
//! ([`Rollback::materialize_behind_halo_safe`]): its own chunk facing the air
//! across the seam becomes real, and its next halo delta brings that chunk
//! (and its colliders) back here. Until then the halo reads it as the
//! recipe's rock.
//!
//! The ENTITY halo rides the same stamped channel: each tick the owner sends
//! every owned entity within [`HALO_UNITS`] of an edge to that neighbour
//! ([`crate::HaloEntityDelta`], `RegionOutput::HaloEntities`), which upserts
//! a kinematic mirror (`GameData::halo_entities`). The owner remembers what it
//! mirrored where (`GameData::halo_sent`) and ends a mirror explicitly with
//! [`crate::HaloEntityGone`] — the entity left the band, despawned, was
//! parked, crossed into another region, or the owner stopped. A crossing into
//! the receiver itself sends none: the arrival upgrades the mirror in place.
//! A seed ([`HaloUpdate::replace`]) also drops every entity mirror of its
//! source, so a (re)started region never keeps a stale one; the source's next
//! tick re-sends the entities still in the band.
//!
//! The PILE halo rides the terrain halo's updates too
//! ([`HaloUpdate::piles`]): each own pile within [`HALO_BLOCKS`] of an edge
//! (`item_piles::pile_halo_targets`) is mirrored, read-only, into the
//! neighbours' `GameData::halo_piles` — written piles as deltas, all of them
//! on a seed (which drops the source's other mirrors). Pickup reach and a
//! deposit's merge search read them; takes and deposits are intents. The
//! CHEST halo ([`HaloUpdate::containers`], `containers.rs`) rides with it:
//! same band, same dirty cells.
//!
//! A halo chunk is a whole neighbour chunk: with `CHUNK_BLOCKS` >= 8 the
//! band is the one chunk row along each edge, plus the corner chunks.
//!
//! **Across a wrapped seam** (`MapExtent::wrap_x`) every key of the halo maps
//! (`halo`, `halo_ground`, `halo_piles`, `halo_containers`) and every world
//! position a region stores or sends is CANONICAL ([`crate::GameData::world_block`],
//! [`crate::GameData::world_chunk`]); every conversion into the region's own
//! frame takes the image nearest it ([`crate::GameData::local_block`],
//! [`crate::GameData::local_chunk`]: `world_map.rs`'s one rule). With no map,
//! or one that does not wrap, both are the plain arithmetic.

use rapier3d::math::{Pose, Vector};
use rapier3d::prelude::{ColliderBuilder, RigidBodyBuilder, RigidBodyHandle};

use crate::{
    Block, BlockId, BlockIndex, Chunk, ChunkCoords, GameData, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind,
    MapExtent, RegionCoords, Rollback, WorldBlockPos, WorldChunkPos, CHUNK_BLOCKS, CHUNK_UNITS, REGION_CHUNKS,
};

/// Width of the halo past each edge, in blocks: at least every interaction
/// reach that may look past an edge (the asserts below), plus a sprint step
/// and a body's half-extent (spec P5's width rule).
pub const HALO_BLOCKS: i32 = 8;

/// [`HALO_BLOCKS`] in world units: an owned entity this close to an edge is
/// mirrored into that neighbour's entity halo (`crate::halo_offsets`).
pub const HALO_UNITS: f32 = (HALO_BLOCKS * crate::BLOCK_VOXELS as i32) as f32;

/// How far (units) a halo mirror may be DRIVEN to its next pose in one
/// step; a jump past it TELEPORTS the mirror instead (`apply_halo_entity_safe`).
/// A driven mirror moves with the velocity that covers the jump in one tick,
/// and hands that velocity to any dynamic body it touches — right for motion,
/// a fling for a jump (an in-band teleport, or a mirror resumed after missed
/// ticks under jitter, whose updates land several ticks at once). The fastest
/// legit per-tick motion of a moving entity is a fly-sprint,
/// `FLY_SPRINT_SPEED` × `TICK_DT` = 1.08 blocks; ×1.5 margin = 1.62 blocks
/// (25.9 units), so every walk, sprint and flight step is driven. A fall
/// faster than ~32 m/s (≈0.7 s of free fall; terminal is 78.4) is teleported:
/// it only loses its push, the behaviour before mirrors were driven.
pub const MIRROR_SNAP_DISTANCE_UNITS: f32 = 1.5
    * crate::movement::FLY_SPRINT_SPEED
    * crate::movement::UNITS_PER_METER
    * crate::movement::TICK_DT;

/// The halo width in whole chunks: chunk rows mirrored along each edge.
pub const HALO_CHUNKS: i32 = (HALO_BLOCKS + CHUNK_BLOCKS as i32 - 1) / CHUNK_BLOCKS as i32;

/// `user_data` of a halo chunk's body: no entity owns it. Halo bodies are
/// fixed, so the physics controller's active-body walk (the one reader of
/// body `user_data`) never meets one.
pub const HALO_BODY_USER_DATA: u128 = u128::MAX;

/// Whole blocks covering a reach of `r` blocks.
const fn ceil_blocks(r: f32) -> i32 {
    let whole = r as i32;
    if (whole as f32) < r { whole + 1 } else { whole }
}

// Every interaction reach that can look past an edge fits inside the halo.
// A new reach constant must be added here.
const _: () = assert!(ceil_blocks(crate::edit::REACH) <= HALO_BLOCKS, "edit::REACH exceeds the halo");
const _: () = assert!(crate::CONTAINER_REACH_BLOCKS <= HALO_BLOCKS, "CONTAINER_REACH_BLOCKS exceeds the halo");
const _: () = assert!(crate::PICKUP_REACH_H_BLOCKS <= HALO_BLOCKS, "PICKUP_REACH_H_BLOCKS exceeds the halo");
const _: () =
    assert!(ceil_blocks(crate::THROW_DISTANCE_BLOCKS) <= HALO_BLOCKS, "THROW_DISTANCE_BLOCKS exceeds the halo");

/// One halo chunk changed: its new content, `None` = gone (all air).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HaloDelta {
    pub chunk: WorldChunkPos,
    pub data: Option<Chunk>,
}

/// One region's halo chunks for one neighbour, as a
/// [`crate::GameEventKind::HaloUpdate`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HaloUpdate {
    /// The region the chunks belong to.
    pub from: RegionCoords,
    /// A seed: `deltas` are ALL of `from`'s chunks in the receiver's band;
    /// any other halo chunk of `from` held there is dropped, and so is every
    /// entity mirror of `from` (its next tick re-sends those in the band).
    pub replace: bool,
    pub deltas: Vec<HaloDelta>,
    /// `from`'s ground recipes for the band's chunk columns, by world chunk
    /// column (`y` 0) — on seeds; empty on deltas (ground never changes).
    pub ground: Vec<(WorldChunkPos, crate::ColumnGround)>,
    /// `from`'s piles in the receiver's pile band, by world cell: the new
    /// pile, `None` = gone. On a seed, all of them (the source's other
    /// mirrors are dropped); on a delta, the cells written.
    pub piles: Vec<(crate::WorldBlockPos, Option<crate::ItemPile>)>,
    /// `from`'s chests in the receiver's pile band, by world cell, as
    /// `piles` (`containers.rs`). A delta carries every written band cell in
    /// both lists (a cell's current pile and chest, either may be `None`).
    pub containers: Vec<(crate::WorldBlockPos, Option<crate::Container>)>,
}

impl HaloUpdate {
    /// Same source and the same chunks and pile cells (reconcile's identity
    /// match; the contents may differ).
    pub fn same_chunks(&self, other: &Self) -> bool {
        self.from == other.from
            && self.replace == other.replace
            && self.deltas.len() == other.deltas.len()
            && self.deltas.iter().zip(&other.deltas).all(|(a, b)| a.chunk == b.chunk)
            && self.piles.len() == other.piles.len()
            && self.piles.iter().zip(&other.piles).all(|(a, b)| a.0 == b.0)
            && self.containers.len() == other.containers.len()
            && self.containers.iter().zip(&other.containers).all(|(a, b)| a.0 == b.0)
    }
}

/// A mirrored chunk and the fixed body carrying its colliders.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, state_hash::StateHash)]
pub struct HaloChunk {
    /// `None`: the chunk went away; its body stays, colliders disabled.
    pub chunk: Option<Chunk>,
    pub body: RigidBodyHandle,
}

/// Offset toward the neighbour a local chunk column index lies against:
/// -1 / +1 within `HALO_CHUNKS` of the low / high edge, else 0.
fn edge_side(c: i32) -> i32 {
    let n = REGION_CHUNKS as i32;
    if c < HALO_CHUNKS {
        -1
    } else if c >= n - HALO_CHUNKS {
        1
    } else {
        0
    }
}

/// The neighbour offsets that mirror local chunk column `[cx, cz]` of a
/// region (none for an interior column; up to 3 at a corner).
pub fn halo_targets(cx: i32, cz: i32) -> Vec<(i32, i32)> {
    let (sx, sz) = (edge_side(cx), edge_side(cz));
    let mut out = Vec::with_capacity(3);
    for dx in [0, sx] {
        for dz in [0, sz] {
            if (dx, dz) != (0, 0) && !out.contains(&(dx, dz)) {
                out.push((dx, dz));
            }
        }
    }
    out
}

/// Whether local chunk column `[cx, cz]` (in a region's chunk frame) lies in
/// that region's halo: outside it, within `HALO_CHUNKS` of an edge.
pub fn in_halo_band(cx: i32, cz: i32) -> bool {
    let n = REGION_CHUNKS as i32;
    let inside = |c: i32| (0..n).contains(&c);
    let near = |c: i32| (-HALO_CHUNKS..n + HALO_CHUNKS).contains(&c);
    near(cx) && near(cz) && !(inside(cx) && inside(cz))
}

/// The neighbour offset from `from` to `to`, when they are neighbours (the
/// short way across a wrapped seam, `crate::region_delta`).
fn neighbour_offset(map: Option<MapExtent>, from: RegionCoords, to: RegionCoords) -> Option<(i32, i32)> {
    let (dx, dz) = crate::region_delta(map, from, to);
    ((dx, dz) != (0, 0) && dx.abs() <= 1 && dz.abs() <= 1).then_some((dx, dz))
}

/// `from`'s chunks that `to` mirrors, out of `chunks` (`from`'s own frame)
/// — a seed ([`HaloUpdate::replace`]). `None` when the two are not
/// neighbours. Used for a running region's reply and for the manager's
/// seeding from cached terrain or worldgen.
pub fn halo_seed_from_chunks<'a>(
    map: Option<MapExtent>,
    from: RegionCoords,
    to: RegionCoords,
    chunks: impl IntoIterator<Item = (ChunkCoords, &'a Chunk)>,
) -> Option<HaloUpdate> {
    halo_seed(map, from, to, chunks, std::iter::empty())
}

/// [`halo_seed_from_chunks`] from a terrain snapshot, its ground recipes
/// included (what the receiver's halo reads below the band).
pub fn halo_seed_from_snapshot(
    map: Option<MapExtent>,
    from: RegionCoords,
    to: RegionCoords,
    snap: &crate::TerrainSnapshot,
) -> Option<HaloUpdate> {
    halo_seed(
        map,
        from,
        to,
        snap.chunks.iter().map(|(c, k)| (*c, k)),
        snap.ground.iter().map(|(c, g)| (*c, g)),
    )
}

/// A seed of `from`'s `chunks` and column `ground` recipes (its own frame)
/// that `to` mirrors, keyed by canonical world chunk (`map`).
pub fn halo_seed<'a>(
    map: Option<MapExtent>,
    from: RegionCoords,
    to: RegionCoords,
    chunks: impl IntoIterator<Item = (ChunkCoords, &'a Chunk)>,
    ground: impl IntoIterator<Item = (crate::ColumnCoords, &'a crate::ColumnGround)>,
) -> Option<HaloUpdate> {
    let offset = neighbour_offset(map, from, to)?;
    let mut deltas: Vec<HaloDelta> = chunks
        .into_iter()
        .filter(|(c, _)| halo_targets(c.x as i32, c.z as i32).contains(&offset))
        .map(|(c, chunk)| HaloDelta {
            chunk: crate::world_chunk(map, from, [c.x as i32, c.y, c.z as i32]),
            data: Some(chunk.clone()),
        })
        .collect();
    deltas.sort_by_key(|d| d.chunk);
    let mut ground: Vec<(WorldChunkPos, crate::ColumnGround)> = ground
        .into_iter()
        .filter(|(c, _)| halo_targets(c.x as i32, c.z as i32).contains(&offset))
        // Bodies are per region: a halo recipe carries none, whichever
        // source (a running region's labelled ground, an unlabelled
        // snapshot) seeded it.
        .map(|(c, g)| (crate::world_chunk(map, from, [c.x as i32, 0, c.z as i32]), g.without_bodies()))
        .collect();
    ground.sort_by_key(|(k, _)| *k);
    Some(HaloUpdate { from, replace: true, deltas, ground, piles: Vec::new(), containers: Vec::new() })
}

/// The chunk columns `xs × zs` of `from` (its own frame) that neighbour `to`
/// mirrors: the band along their shared edge, or the corner block for a
/// diagonal neighbour. Empty ranges when they are not neighbours.
pub fn facing_columns(
    map: Option<MapExtent>,
    from: RegionCoords,
    to: RegionCoords,
) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
    let n = REGION_CHUNKS;
    let h = (HALO_CHUNKS as usize).min(n);
    let span = |d: i32| match d {
        -1 => 0..h,
        1 => n - h..n,
        _ => 0..n,
    };
    match neighbour_offset(map, from, to) {
        Some((dx, dz)) => (span(dx), span(dz)),
        None => (0..0, 0..0),
    }
}

/// [`facing_columns`] cut into square windows at most `HALO_CHUNKS` on a
/// side, in a fixed order: worldgen's patch is square on the longer side, so
/// a whole edge strip in one window would cost a whole region's patch.
pub fn facing_windows(
    map: Option<MapExtent>,
    from: RegionCoords,
    to: RegionCoords,
) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let (xs, zs) = facing_columns(map, from, to);
    let h = (HALO_CHUNKS as usize).max(1);
    let cut = |r: std::ops::Range<usize>| -> Vec<std::ops::Range<usize>> {
        r.clone().step_by(h).map(|s| s..(s + h).min(r.end)).collect()
    };
    let zcuts = cut(zs);
    cut(xs).into_iter().flat_map(|x| zcuts.iter().map(move |z| (x.clone(), z.clone()))).collect()
}

/// The 8 neighbours of `rc`, in a fixed order.
pub fn neighbours(rc: RegionCoords) -> impl Iterator<Item = RegionCoords> {
    [(-1, -1), (-1, 0), (-1, 1), (0, -1), (0, 1), (1, -1), (1, 0), (1, 1)]
        .into_iter()
        .map(move |(dx, dz)| RegionCoords::new(rc.x + dx, rc.z + dz))
}

thread_local! {
    /// Terrain chunk entities written on this thread since the last
    /// [`take_touched_chunks`]: the region that wrote them sends the halo
    /// deltas. Not state: never hashed, serialized or undone.
    static TOUCHED: std::cell::RefCell<Vec<crate::EntityKey>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Depenetration lifts on this thread (see [`depenetration_lifts`]).
    static LIFTS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Notes that terrain chunk entity `e` was written (its blocks, or created).
pub(crate) fn touch_chunk(e: crate::EntityKey) {
    TOUCHED.with(|t| t.borrow_mut().push(e));
}

/// The chunk entities written on this thread since the previous call.
pub(crate) fn take_touched_chunks() -> Vec<crate::EntityKey> {
    TOUCHED.with(|t| std::mem::take(&mut *t.borrow_mut()))
}

/// Notes one non-zero `movement::depenetration_lift`.
pub(crate) fn note_lift() {
    LIFTS.with(|c| c.set(c.get() + 1));
}

/// How many bodies `movement::depenetration_lift` lifted on this thread: a
/// test probe that a seam crossing no longer sinks into the next region.
#[doc(hidden)]
pub fn depenetration_lifts() -> u64 {
    LIFTS.with(|c| c.get())
}

/// The seam frame of a region's state (module docs): canonical world
/// positions out, nearest-image local positions in.
impl GameData {
    /// The world map's extent (`None`: unbounded), as the region's runner set it.
    pub fn map(&self) -> Option<MapExtent> {
        *self.map_extent
    }

    /// The canonical world block of local block `local` (it may lie past the
    /// region's edges).
    pub fn world_block(&self, local: [i32; 3]) -> WorldBlockPos {
        crate::world_block(self.map(), *self.region, local)
    }

    /// World block `p` (canonical, or unwrapped in this region's frame) in
    /// this region's block frame: the nearest image.
    pub fn local_block(&self, p: WorldBlockPos) -> [i32; 3] {
        crate::local_block(self.map(), *self.region, p)
    }

    /// `p` canonical on the world map.
    pub fn canonical_block(&self, p: WorldBlockPos) -> WorldBlockPos {
        crate::canonical_block(self.map(), p)
    }

    /// The canonical world chunk of local chunk `local`.
    pub fn world_chunk(&self, local: [i32; 3]) -> WorldChunkPos {
        crate::world_chunk(self.map(), *self.region, local)
    }

    /// World chunk `c` in this region's chunk frame: the nearest image.
    pub fn local_chunk(&self, c: WorldChunkPos) -> [i32; 3] {
        crate::local_chunk(self.map(), *self.region, c)
    }
}

impl Rollback {
    /// The block at world block `p` as this region sees it: its own (implied
    /// ground included), or its halo's (air where the neighbour has no
    /// chunk). `None` outside own ∪ halo.
    pub fn halo_block(&self, p: WorldBlockPos) -> Option<Block> {
        let local = self.data.local_block(p);
        if let Some((coords, l)) = crate::edit::locate(local) {
            return Some(self.current_block(coords, l).unwrap_or(Block::new(BlockId::AIR)));
        }
        self.halo_cell(local).map(|(b, _)| b)
    }

    /// The halo block at region-local block `pos` (outside the region) and
    /// whether it is chiseled; `None` outside the halo band.
    fn halo_cell(&self, pos: [i32; 3]) -> Option<(Block, bool)> {
        let cb = CHUNK_BLOCKS as i32;
        let chunk = [pos[0].div_euclid(cb), pos[1].div_euclid(cb), pos[2].div_euclid(cb)];
        if !in_halo_band(chunk[0], chunk[2]) {
            return None;
        }
        let key = self.data.world_chunk(chunk);
        let l = [pos[0].rem_euclid(cb) as usize, pos[1].rem_euclid(cb) as usize, pos[2].rem_euclid(cb) as usize];
        let Some(c) = self.data.halo.get(&key).and_then(|h| h.chunk.as_ref()) else {
            // No chunk: the neighbour's implied ground below its band, else air.
            let implied = self.halo_ground_of(key).and_then(|g| {
                crate::ground::implied_block(Some(g), ChunkCoords::new(0, key.y, 0), (l[0], l[1], l[2]))
            });
            return Some((implied.unwrap_or(Block::new(BlockId::AIR)), false));
        };
        Some((c.block_at(l[0], l[1], l[2]), c.chisel.contains_key(&BlockIndex::from_xyz(l[0], l[1], l[2]))))
    }

    /// Whether the halo knows world block `p`'s column: a neighbour's
    /// column it holds a ground recipe or a chunk of (seeded). An unseeded
    /// band column reads as air, but nothing there is known.
    pub(crate) fn halo_column_known(&self, p: WorldBlockPos) -> bool {
        let p = self.data.canonical_block(p);
        let cb = CHUNK_BLOCKS as i64;
        let (cx, cz) = (p.x.div_euclid(cb), p.z.div_euclid(cb));
        if self.data.halo_ground.contains_key(&WorldChunkPos::new(cx, 0, cz)) {
            return true;
        }
        self.data
            .halo
            .range(WorldChunkPos::new(cx, i32::MIN, i64::MIN)..=WorldChunkPos::new(cx, i32::MAX, i64::MAX))
            .any(|(k, h)| k.z == cz && h.chunk.is_some())
    }

    /// The neighbour's ground recipe for halo chunk `key`'s column.
    fn halo_ground_of(&self, key: WorldChunkPos) -> Option<&crate::ColumnGround> {
        self.data.halo_ground.get(&WorldChunkPos::new(key.x, 0, key.z))
    }

    /// The halo block at region-local `pos` (outside the region) if a break
    /// of it may clear something: not air, not the neighbour's bedrock. The
    /// sender's view; the owner decides (`intents.rs`).
    pub(crate) fn halo_mineable(&self, pos: [i32; 3]) -> Option<Block> {
        let (block, chiseled) = self.halo_cell(pos)?;
        if block.id == BlockId::AIR && !chiseled {
            return None;
        }
        let cb = CHUNK_BLOCKS as i32;
        let key = self.data.world_chunk([pos[0].div_euclid(cb), 0, pos[2].div_euclid(cb)]);
        let bedrock = self.halo_ground_of(key).is_some_and(|g| block.id == g.bedrock);
        (!bedrock).then_some(block)
    }

    /// After halo chunks `keys` changed: every chunk of this region that
    /// faces one across a seam, is still implied ground, and borders air in
    /// it is materialised (undo-safely) — the new air of a neighbour's dig
    /// never borders ground that has no chunk, mesh or collider. Its halo
    /// delta then brings the chunk to the neighbour. Corner chunks share no
    /// face with this region and are skipped.
    pub(crate) fn materialize_behind_halo_safe(&mut self, keys: &[WorldChunkPos]) {
        let n = REGION_CHUNKS as i32;
        let cb = CHUNK_BLOCKS as i32;
        let inside = |c: i32| (0..n).contains(&c);
        for &key in keys {
            let [lx, ly, lz] = self.data.local_chunk(key);
            // The own chunk across the shared face, and the halo chunk's
            // face plane (axis, block index) touching it.
            let (own, axis, plane) = match (lx, lz) {
                (-1, z) if inside(z) => ([0, ly, z], 0, cb - 1),
                (x, z) if x == n && inside(z) => ([n - 1, ly, z], 0, 0),
                (x, -1) if inside(x) => ([x, ly, 0], 2, cb - 1),
                (x, z) if z == n && inside(x) => ([x, ly, n - 1], 2, 0),
                _ => continue,
            };
            let coords = ChunkCoords::new(own[0] as usize, own[1], own[2] as usize);
            if self.implied_block(coords, (0, 0, 0)).is_none() || self.chunk_key_at(coords).is_some() {
                continue;
            }
            let base = [lx * cb, ly * cb, lz * cb];
            let opens = (0..cb).any(|a| {
                (0..cb).any(|y| {
                    let mut p = [base[0], base[1] + y, base[2]];
                    p[axis] += plane;
                    p[2 - axis] += a;
                    self.halo_cell(p).is_some_and(|(b, chiseled)| b.id == BlockId::AIR || chiseled)
                })
            });
            if opens {
                self.materialize_ground_safe(coords);
            }
        }
    }

    /// The block at region-local tile `p` as this region sees it: its own
    /// (implied ground included), else the halo's; `None` for an empty own
    /// slot or outside own ∪ halo.
    pub(crate) fn tile_block(&self, p: [i32; 3]) -> Option<Block> {
        match crate::edit::locate(p) {
            Some((coords, local)) => self.current_block(coords, local),
            None => self.halo_cell(p).map(|(b, _)| b),
        }
    }

    /// The chisel of the halo block at region-local `pos` (outside the
    /// region), if any.
    pub(crate) fn halo_chisel(&self, pos: [i32; 3]) -> Option<&crate::ChiselData> {
        let cb = CHUNK_BLOCKS as i32;
        let chunk = [pos[0].div_euclid(cb), pos[1].div_euclid(cb), pos[2].div_euclid(cb)];
        if !in_halo_band(chunk[0], chunk[2]) {
            return None;
        }
        let c = self.halo_chunk_at(chunk)?.chunk.as_ref()?;
        let l = [pos[0].rem_euclid(cb) as usize, pos[1].rem_euclid(cb) as usize, pos[2].rem_euclid(cb) as usize];
        c.chisel.get(&BlockIndex::from_xyz(l[0], l[1], l[2]))
    }

    /// Whether the halo block at region-local `pos` is anything but plain air
    /// (the edit ray's solidity, past the region's edge).
    pub(crate) fn halo_cell_solid(&self, pos: [i32; 3]) -> bool {
        self.halo_cell(pos).is_some_and(|(b, chiseled)| crate::block_props().blocks_ray(b.id) || chiseled)
    }

    /// The halo chunk at local chunk `[cx, cy, cz]` (outside the region).
    pub fn halo_chunk_at(&self, local: [i32; 3]) -> Option<&HaloChunk> {
        self.data.halo.get(&self.data.world_chunk(local))
    }

    /// This region's chunks (and piles) that neighbour `to` mirrors, as a
    /// seed.
    pub fn halo_seed_for(&self, to: RegionCoords) -> Option<HaloUpdate> {
        let from = *self.data.region;
        let chunks: Vec<(ChunkCoords, &Chunk)> = self
            .data
            .chunk_index
            .iter()
            .filter_map(|(&k, &e)| Some((crate::chunk_coords_of_index_key(k), self.data.ecs.chunk.try_get(e).as_ref()?)))
            .collect();
        let mut seed = halo_seed(self.data.map(), from, to, chunks, self.data.ground.iter().map(|(c, g)| (*c, g)))?;
        seed.piles = self.pile_seed_for(to);
        seed.containers = self.container_seed_for(to);
        Some(seed)
    }

    /// The halo updates for chunk entities (`keys`) and own pile cells
    /// (`piles`) written since the last call (any order, repeats allowed):
    /// one update per neighbour that mirrors any of them, with each chunk's
    /// and pile's current content.
    pub(crate) fn halo_updates_for(
        &self,
        keys: &std::collections::BTreeSet<crate::ChunkIndexKey>,
        piles: &std::collections::BTreeSet<crate::WorldBlockPos>,
    ) -> Vec<(HaloUpdate, RegionCoords)> {
        type Pending = (
            Vec<HaloDelta>,
            Vec<(crate::WorldBlockPos, Option<crate::ItemPile>)>,
            Vec<(crate::WorldBlockPos, Option<crate::Container>)>,
        );
        let from = *self.data.region;
        let mut by_target: std::collections::BTreeMap<(i32, i32), Pending> = Default::default();
        for &k in keys {
            let c = crate::chunk_coords_of_index_key(k);
            let targets = halo_targets(c.x as i32, c.z as i32);
            if targets.is_empty() {
                continue;
            }
            let data = self.chunk_key_at(c).and_then(|e| self.data.ecs.chunk.try_get(e).clone());
            let chunk = self.data.world_chunk([c.x as i32, c.y, c.z as i32]);
            for t in targets {
                by_target.entry(t).or_default().0.push(HaloDelta { chunk, data: data.clone() });
            }
        }
        for &p in piles {
            let l = self.data.local_block(p);
            let pile = self.data.item_piles.get(&p).cloned();
            let chest = self.data.containers().get(&p).cloned();
            for t in crate::item_piles::pile_halo_targets(l[0], l[2]) {
                let e = by_target.entry(t).or_default();
                e.1.push((p, pile.clone()));
                e.2.push((p, chest.clone()));
            }
        }
        // Each target canonical (across a wrapped seam, the map's other
        // end); one off the map gets nothing.
        let map = self.data.map();
        by_target
            .into_iter()
            .filter_map(|((dx, dz), (mut deltas, piles, containers))| {
                let to = crate::region_neighbour(map, from, dx, dz)?;
                deltas.sort_by_key(|d| d.chunk);
                let update = HaloUpdate { from, replace: false, deltas, ground: Vec::new(), piles, containers };
                Some((update, to))
            })
            .collect()
    }

    /// Applies a [`HaloUpdate`], undo-safely: its ground recipes; a seed
    /// first drops the source's halo chunks it does not list; then each
    /// delta; then this region materialises its implied ground facing new
    /// air in them ([`Self::materialize_behind_halo_safe`]).
    pub(crate) fn apply_halo_update_safe(&mut self, u: &HaloUpdate) {
        let mut touched = Vec::new();
        for (k, g) in &u.ground {
            // Bodies are per region: the halo's copy carries none (see `halo_seed`).
            let g = &g.without_bodies();
            let local = self.data.local_chunk(*k);
            if k.to_local().0 != u.from || k.y != 0 || !in_halo_band(local[0], local[2]) {
                log::warn!("halo update from {:?}: ground for {k:?} outside its band; ignored", u.from);
                continue;
            }
            if self.data.halo_ground.get(k) != Some(g) {
                if self.data.has_render_sink() {
                    let prior = self.data.halo_ground.get(k).cloned();
                    self.data.emit_on_undo(GameDataUpdate::new(
                        GameDataTransactionKind::Undo,
                        GameDataUpdateKind::SetHaloGround(vec![(*k, prior)]),
                    ));
                    let now = GameDataUpdateKind::SetHaloGround(vec![(*k, Some(g.clone()))]);
                    self.data.send(GameDataUpdate::new(GameDataTransactionKind::Do, now));
                }
                self.data.halo_ground.insert(*k, g.clone());
            }
        }
        if u.replace {
            let listed: std::collections::BTreeSet<WorldChunkPos> = u.deltas.iter().map(|d| d.chunk).collect();
            let stale: Vec<WorldChunkPos> = self
                .data
                .halo
                .iter()
                .filter(|(k, h)| h.chunk.is_some() && k.to_local().0 == u.from && !listed.contains(k))
                .map(|(k, _)| *k)
                .collect();
            for chunk in stale {
                self.apply_halo_delta_safe(&HaloDelta { chunk, data: None });
                touched.push(chunk);
            }
            self.drop_halo_source_safe(u.from);
            let listed: std::collections::BTreeSet<crate::WorldBlockPos> = u.piles.iter().map(|(p, _)| *p).collect();
            let stale: Vec<_> = self
                .data
                .halo_piles
                .keys()
                .filter(|p| p.to_local().0 == u.from && !listed.contains(p))
                .map(|p| (*p, None))
                .collect();
            self.write_halo_piles_safe(stale);
            let listed: std::collections::BTreeSet<crate::WorldBlockPos> =
                u.containers.iter().map(|(p, _)| *p).collect();
            let stale: Vec<_> = self
                .data
                .halo_containers()
                .keys()
                .filter(|p| p.to_local().0 == u.from && !listed.contains(p))
                .map(|p| (*p, None))
                .collect();
            self.write_halo_containers_safe(stale);
        }
        let piles: Vec<_> = u
            .piles
            .iter()
            .filter(|(p, _)| {
                let l = self.data.local_block(*p);
                let ok = p.to_local().0 == u.from && crate::item_piles::in_pile_halo_band(l[0], l[2]);
                if !ok {
                    log::warn!("halo update from {:?}: pile {p:?} outside its band; ignored", u.from);
                }
                ok
            })
            .cloned()
            .collect();
        self.write_halo_piles_safe(piles);
        let containers: Vec<_> = u
            .containers
            .iter()
            .filter(|(p, _)| {
                let l = self.data.local_block(*p);
                let ok = p.to_local().0 == u.from && crate::item_piles::in_pile_halo_band(l[0], l[2]);
                if !ok {
                    log::warn!("halo update from {:?}: chest {p:?} outside its band; ignored", u.from);
                }
                ok
            })
            .cloned()
            .collect();
        self.write_halo_containers_safe(containers);
        for d in &u.deltas {
            if d.chunk.to_local().0 != u.from {
                log::warn!("halo update from {:?} carries a chunk of another region: {:?}", u.from, d.chunk);
                continue;
            }
            self.apply_halo_delta_safe(d);
            touched.push(d.chunk);
        }
        self.materialize_behind_halo_safe(&touched);
    }

    /// One halo chunk takes its new content, undo-safely: the stored chunk
    /// and the fixed colliders on its body (built the first time, patched in
    /// place after — see `rebuild_body_colliders_safe`). A chunk outside this
    /// region's band, or unchanged, is ignored.
    pub(crate) fn apply_halo_delta_safe(&mut self, d: &HaloDelta) {
        let local = self.data.local_chunk(d.chunk);
        if !in_halo_band(local[0], local[2]) {
            log::debug!("halo delta {:?} outside the band of {:?}; ignored", d.chunk, *self.data.region);
            return;
        }
        let cells = match &d.data {
            Some(chunk) => crate::collider_cells(chunk),
            None => crate::ColliderCells { blocks: Vec::new(), halves: Vec::new(), voxels: Vec::new() },
        };
        let unchanged = self.data.halo.get(&d.chunk).is_some_and(|h| h.chunk == d.data)
            || (d.data.is_none() && !self.data.halo.contains_key(&d.chunk));
        if !unchanged && self.data.has_render_sink() {
            // The client's tile mirror plans walks over the halo too.
            let prior = self.data.halo.get(&d.chunk).and_then(|h| h.chunk.clone());
            self.data.emit_on_undo(GameDataUpdate::new(
                GameDataTransactionKind::Undo,
                GameDataUpdateKind::SetHaloTerrain(vec![(d.chunk, prior)]),
            ));
            let now = GameDataUpdateKind::SetHaloTerrain(vec![(d.chunk, d.data.clone())]);
            self.data.send(GameDataUpdate::new(GameDataTransactionKind::Do, now));
        }
        match self.data.halo.get(&d.chunk).map(|h| (h.chunk == d.data, h.body)) {
            Some((true, _)) => {}
            Some((false, body)) => {
                self.rebuild_body_colliders_safe(body, HALO_BODY_USER_DATA, &cells);
                self.data.halo.insert(d.chunk, HaloChunk { chunk: d.data.clone(), body });
            }
            None => {
                if d.data.is_none() {
                    return;
                }
                let u = CHUNK_UNITS as f32;
                let pose = Pose::from_translation(Vector::new(local[0] as f32 * u, local[1] as f32 * u, local[2] as f32 * u));
                let body = self.insert_body_safe(RigidBodyBuilder::fixed().pose(pose).user_data(HALO_BODY_USER_DATA).build());
                for (size, keys) in cells.grids() {
                    let collider =
                        ColliderBuilder::voxels(Vector::splat(size), keys).user_data(HALO_BODY_USER_DATA).build();
                    self.attach_collider_safe(body, collider);
                }
                self.data.halo.insert(d.chunk, HaloChunk { chunk: d.data.clone(), body });
            }
        }
    }
}
