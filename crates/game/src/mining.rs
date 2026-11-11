//! Survival mining, placing and throwing (plan 2026-11-05, Decisions 7, 8
//! and 11): what an Adventure player's mouse buttons and Q do. Mining and placing
//! work on BRICKS: the eye ray is a stud ray over own ∪ stud halo
//! ([`Rollback::eye_stud_hit`]) and its target is the brick it meets
//! ([`stud_target_box`]: the brick's own cell box; a terrain filler slab counts as
//! a lattice of [`MINE_BRICK_SIZE`] bricks, a chest as its whole tile). Throwing
//! still lands in TILES (`tiles.rs`), as piles do.
//!
//! - **Mining** is sim state on `Client` ([`Mining`]): while the left button
//!   is held and the eye ray hits the same brick, progress builds; a new
//!   target restarts it, a release (or leaving first person, or opening the
//!   inventory) cancels it. Progress is never accumulated: it is
//!   `tick - started_tick`, all integer ticks. The brick breaks on the
//!   [`Rollback::stud_mine_ticks`]-th tick held (the start tick counts:
//!   [`stud_break_ticks`] of its material over its cells, twice that for tree
//!   wood), and no mining progresses for [`BREAK_COOLDOWN_TICKS`] ticks after.
//!
//!   The brick may be the neighbour's, seen through the halo: the break is
//!   always an [`crate::Intent::BreakStud`] (`survival`) to the brick's owner (a
//!   `Target::Stud`, the box in world stud cells), here or across a seam alike,
//!   which applies it at the opening of the next tick — so it is sent on the tick
//!   before the last one held. The owner's refusals decide; the break carves
//!   exactly the box and drops [`stud_drops_rolled`] into the owner's piles (tree
//!   wood fells the tree, a chest spills).
//! - **Placing** (right press) puts ONE brick ([`PLACE_BRICK_SIZE`], of the
//!   selected hotbar block's stud material) on the stud grid against the hit face
//!   ([`brick_placement`]) and takes one from the slot at once; the place itself
//!   is an [`crate::Intent::PlaceBrick`] to the chunk's owner (next tick; the
//!   chest block is still a tile's [`crate::Intent::PlaceBlock`]), and a
//!   place the owner refuses gives the item back
//!   ([`Rollback::refund_refused_places_safe`]). A non-block item or an
//!   empty slot places nothing.
//! - **Throwing** (Q one, Ctrl+Q the whole stack) drops the selected item as
//!   a pile [`THROW_DISTANCE_BLOCKS`] ahead along the look, in the tile the
//!   stud ray stops before (halo bricks included), settled onto the ground
//!   at once — or, landing in a neighbour's cell, deposited there by an
//!   intent to its owner (`item_piles.rs`).
//!
//! Fortress clients keep `edit.rs`'s creative edit (also intents).

use crate::edit::{BlockPos, BREAK_BUTTON};
use crate::intents::{Effects as _, RegionEffects};
use crate::{
    drop_of, BlockId, Client, ClientId, EntityId, Intent, IntentMsg, IntentReply, ItemId, ItemStack, PlayMode,
    Rollback, Target, WorldBlockPos, BLOCK_UNITS, MAX_STACK, THROWN_PICKUP_DELAY_TICKS,
};

/// Ticks per whole unit of hardness with the bare hand (Minecraft's
/// correct-tool hand rule: `ceil(hardness * 30)` ticks at 20 ticks/s).
pub const HAND_TICKS_PER_HARDNESS: u32 = 30;
/// A tile of tree wood takes this many times as long (its break fells the tree).
pub const FELL_TICKS_FACTOR: u32 = 2;
/// Ticks after a break in which no mining progresses.
pub const BREAK_COOLDOWN_TICKS: u64 = 6;
/// How far ahead (blocks, along the look) a thrown item lands.
pub const THROW_DISTANCE_BLOCKS: f32 = 2.0;

/// The brick a client is mining. `target` is the min cell of its box in
/// region-local stud cells (`[stud x, plate y, stud z]`; it may lie past the edge,
/// in the stud halo) — the box itself is read back from the terrain
/// ([`Rollback::stud_target_at`]).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, state_hash::StateHash,
)]
pub struct Mining {
    pub target: BlockPos,
    /// The first tick of progress (it counts as one).
    pub started_tick: u64,
    /// Ticks held until the tile breaks ([`Rollback::stud_mine_ticks`]).
    pub needed_ticks: u32,
}

/// What the HUD shows of the local client's mining (client-side projection).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MiningView {
    /// The mined box's min, region-local stud cells.
    pub target: BlockPos,
    /// The box's size in stud cells (`[studs, plates, studs]`).
    pub size: [u8; 3],
    /// 0..=1000.
    pub progress_per_mille: u32,
}

/// Stud cells in one tile (`TILE_STUDS`² x `TILE_PLATES`).
pub const STUD_TILE_CELLS: u32 = (crate::TILE_STUDS * crate::TILE_STUDS * crate::TILE_PLATES) as u32;
/// Hand ticks per work unit on stud terrain, as a fraction: a material's
/// `StudMaterialDef::hardness` is work units per stud cell, and a whole tile of stone
/// (8 x 24 = 192 units) takes the 45 ticks a stone block does; dirt (2 x 24 = 48) 12
/// against a dirt block's 15.
pub const STUD_HAND_TICKS_NUM: u32 = 15;
pub const STUD_HAND_TICKS_DEN: u32 = 64;

/// Ticks of holding the button to dig `cells` stud cells of `material` by hand:
/// `ceil(hardness * cells * STUD_HAND_TICKS_NUM / STUD_HAND_TICKS_DEN)`, at least 1.
/// `None` for an unknown material or one that cannot be dug (bedrock).
pub fn stud_break_ticks(material: u16, cells: u32) -> Option<u32> {
    let m = crate::stud_material(material).filter(|m| m.hardness != u16::MAX)?;
    let work = u64::from(m.hardness) * u64::from(cells) * u64::from(STUD_HAND_TICKS_NUM);
    Some(work.div_ceil(u64::from(STUD_HAND_TICKS_DEN)).clamp(1, u64::from(u32::MAX)) as u32)
}

/// What digging stud cells yields, until items are ported to stud materials: per
/// material of `removed` (a carve's cell counts), what the block of
/// the same name drops ([`drop_of`]), one per whole tile's worth of cells
/// ([`STUD_TILE_CELLS`], rounded to the nearest, half up). A material with no such
/// block (a plastic) or whose block drops nothing yields nothing. Pure rounding: a
/// felled trunk's and a settled model's yield; a carve rolls its remainder instead
/// ([`stud_drops_rolled`]).
pub fn stud_drops(removed: &[(u16, u32)]) -> Vec<ItemStack> {
    removed
        .iter()
        .filter_map(|&(material, cells)| {
            let count = (2 * u64::from(cells) + u64::from(STUD_TILE_CELLS)) / (2 * u64::from(STUD_TILE_CELLS));
            let stack = drop_of(crate::block_of_stud_material(material)?)?;
            (count > 0).then(|| ItemStack { count: (u64::from(stack.count) * count).min(u64::from(u32::MAX)) as u32, ..stack })
        })
        .collect()
}

/// Salt of the drop remainder roll ([`stud_drops_rolled`]).
pub const DROP_ROLL_SALT: u64 = 0xD209_B21C;

/// What a batch of carves yields, without rounding small bricks away: the cells
/// are counted per dropped ITEM (grass and dirt both drop dirt) across the whole
/// batch, an item is paid out each time a tile's worth ([`STUD_TILE_CELLS`]) is
/// reached ([`Self::add`], at the carve that reached it), and what is left at the
/// end yields one more with probability `rest / STUD_TILE_CELLS`, by a stateless
/// hash roll ([`Self::finish`]). So a built tile's two 2 x 2 bricks deconstructed
/// together give exactly its one item back, and a single mined 2 x 2 brick (12
/// cells) drops its item every other time: the expected yield of any carve is
/// exactly `cells / STUD_TILE_CELLS`.
///
/// A roll for the remainder, not a remainder carried in the state: the carve is
/// applied by the cells' OWNER, which may not hold the miner (a brick across a
/// seam), so a per-player remainder would have to travel with every break and
/// every crossing — the roll needs no state, nothing to undo and nothing to save.
#[derive(Debug, Default)]
pub struct DropTally {
    /// Per item: what one tile's worth drops, and the cells not yet paid out.
    rest: std::collections::BTreeMap<ItemId, (u32, u64)>,
}

impl DropTally {
    /// Counts one carve's `removed` cells; returns the items now due.
    pub fn add(&mut self, removed: &[(u16, u32)]) -> Vec<ItemStack> {
        let per = u64::from(STUD_TILE_CELLS);
        let mut touched = std::collections::BTreeSet::new();
        for &(material, cells) in removed {
            let Some(stack) = crate::block_of_stud_material(material).and_then(drop_of) else { continue };
            self.rest.entry(stack.item).or_insert((stack.count, 0)).1 += u64::from(cells);
            touched.insert(stack.item);
        }
        let mut out = Vec::new();
        for item in touched {
            let Some((each, cells)) = self.rest.get_mut(&item) else { continue };
            let count = *cells / per;
            *cells %= per;
            if count > 0 {
                out.push(ItemStack { item, count: (u64::from(*each) * count).min(u64::from(u32::MAX)) as u32 });
            }
        }
        out
    }

    /// The rolled remainders: per item with cells left, one more or nothing, by
    /// `roll(tick, subject, ..)` (`subject`: the last carve's position,
    /// [`drop_subject`]). Leaves the tally empty.
    pub fn finish(&mut self, tick: u64, subject: u64) -> Vec<ItemStack> {
        let per = u64::from(STUD_TILE_CELLS);
        let rest = std::mem::take(&mut self.rest);
        rest.into_iter()
            .enumerate()
            .filter(|(i, (_, (_, cells)))| {
                *cells > 0 && crate::slime::roll(tick, subject ^ ((*i as u64 + 1) << 56), DROP_ROLL_SALT) % per < *cells
            })
            .map(|(_, (item, (each, _)))| ItemStack { item, count: each })
            .collect()
    }
}

/// What ONE carve yields ([`DropTally`]: whole tiles' worth, the rest rolled).
pub fn stud_drops_rolled(removed: &[(u16, u32)], tick: u64, subject: u64) -> Vec<ItemStack> {
    let mut tally = DropTally::default();
    let mut out = tally.add(removed);
    for extra in tally.finish(tick, subject) {
        match out.iter_mut().find(|s| s.item == extra.item) {
            Some(s) => s.count = s.count.saturating_add(extra.count),
            None => out.push(extra),
        }
    }
    out
}

/// The roll subject of a carve whose min is stud cell `lo`: its plate height only.
/// Not its x/z — with the world tick that keeps the roll the same wherever the
/// scene sits relative to region seams (regions tile the plane; plates are not
/// offset), which the seam tests translate.
pub fn drop_subject(lo: [i64; 3]) -> u64 {
    lo[1] as u64
}

/// The brick a terrain filler counts as made of, and the part a hand-placed block
/// becomes: a 2 x 2 brick (`[studs, plates, studs]`).
pub const MINE_BRICK_SIZE: [u8; 3] = [2, crate::PLATES_PER_BRICK, 2];
/// The brick a hotbar block places.
pub const PLACE_BRICK_SIZE: [u8; 3] = MINE_BRICK_SIZE;

/// The box of chunk cells (`lo .. hi`, `hi` exclusive) a hit on `cell` of `brick`
/// targets — what the crosshair outlines and mining breaks:
/// - a catalog part: its whole cell box (a slope's bounding box);
/// - a chest's brick (`containers::STUD_CHEST_TAG`): the chest's whole tile;
/// - a filler (terrain slabs can span a chunk): the [`MINE_BRICK_SIZE`] brick of
///   the lattice anchored at the filler's min holding `cell`, clipped to the filler.
pub fn stud_target_box(brick: &crate::StudBrick, cell: [u8; 3]) -> ([u8; 3], [u8; 3]) {
    let size = brick.size();
    let b_hi = [0, 1, 2].map(|a| brick.min[a] + size[a]);
    if brick.data == crate::containers::STUD_CHEST_TAG && brick.material == crate::containers::STUD_CHEST_MATERIAL {
        let t = [crate::TILE_STUDS as u8, crate::TILE_PLATES as u8, crate::TILE_STUDS as u8];
        let lo = [0, 1, 2].map(|a| cell[a] / t[a] * t[a]);
        return (lo, [0, 1, 2].map(|a| lo[a] + t[a]));
    }
    match brick.shape {
        crate::StudShape::Part { .. } => (brick.min, b_hi),
        crate::StudShape::Filler { .. } => {
            let lo = [0, 1, 2].map(|a| {
                let rel = cell[a].clamp(brick.min[a], b_hi[a] - 1) - brick.min[a];
                brick.min[a] + rel / MINE_BRICK_SIZE[a] * MINE_BRICK_SIZE[a]
            });
            (lo, [0, 1, 2].map(|a| (lo[a] + MINE_BRICK_SIZE[a]).min(b_hi[a])))
        }
    }
}

/// The brick a place against the face of `hit` puts down, of `material`: a
/// [`PLACE_BRICK_SIZE`] box part flush against the face on the face's axis and on
/// the [`PLACE_BRICK_SIZE`] lattice of the chunk on the other two (so placed bricks
/// line up into courses). Where that would straddle the chunk's edge on the face's
/// axis the brick is as thin as fits. Returns the chunk (`[x, layer, z]` in the
/// ray's frame; it may be a neighbour's) and the brick. `None` from inside a brick
/// (no face), outside the legal layers, or when the catalog has no such part.
pub fn brick_placement(hit: &crate::StudRayHit, material: u16) -> Option<([i64; 3], crate::StudBrick)> {
    if hit.normal == [0; 3] {
        return None;
    }
    let dims = [crate::STUD_CHUNK_STUDS as i64, crate::STUD_CHUNK_PLATES as i64, crate::STUD_CHUNK_STUDS as i64];
    // The cell just outside the face, in cells of the ray's frame.
    let g = [0, 1, 2].map(|a| hit.chunk[a] * dims[a] + i64::from(hit.cell[a]) + i64::from(hit.normal[a]));
    let chunk = [0, 1, 2].map(|a| g[a].div_euclid(dims[a]));
    if !i32::try_from(chunk[1]).is_ok_and(crate::stud::stud_layer_ok) {
        return None;
    }
    let (mut min, mut size) = ([0u8; 3], PLACE_BRICK_SIZE);
    for a in 0..3 {
        let c = g[a].rem_euclid(dims[a]);
        let s = i64::from(PLACE_BRICK_SIZE[a]);
        let lo = match hit.normal[a] {
            0 => c / s * s,
            n if n > 0 => {
                size[a] = s.min(dims[a] - c) as u8;
                c
            }
            _ => {
                size[a] = s.min(c + 1) as u8;
                c + 1 - i64::from(size[a])
            }
        };
        min[a] = lo as u8;
    }
    // A height the catalog has no box of (2 plates): a single plate against the face.
    let (part, rot) = crate::stud_carve::box_part(size).or_else(|| {
        if hit.normal[1] < 0 {
            min[1] += size[1] - 1;
        }
        size[1] = 1;
        crate::stud_carve::box_part(size)
    })?;
    Some((chunk, crate::StudBrick::part_at(min, part, rot, material)))
}

/// `client`'s mining progress at `now` (the region's current tick, i.e. after
/// the ticks so far), or `None` while it mines nothing. `size` is the mined box's
/// ([`Rollback::mining_view`] reads it from the terrain).
pub fn mining_view(client: &Client, now: u64, size: [u8; 3]) -> Option<MiningView> {
    let m = client.mining?;
    let held = now.saturating_sub(m.started_tick);
    let progress = (held * 1000 / u64::from(m.needed_ticks.max(1))).min(1000) as u32;
    Some(MiningView { target: m.target, size, progress_per_mille: progress })
}

/// What a stud ray met ([`Rollback::stud_ray_tiles`]): the brick, and the tiles
/// around the hit (what throwing and chests still address).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StudEyeHit {
    /// The raw hit (chunk in this region's frame; it may be a halo chunk).
    pub ray: crate::StudRayHit,
    /// The target box ([`stud_target_box`] of the brick hit) in region-local stud
    /// cells, `hi` exclusive: what mining breaks.
    pub lo: [i32; 3],
    pub hi: [i32; 3],
    /// The tile holding the stud cell the ray hit (region-local; it may lie past
    /// the edge, in the stud halo).
    pub tile: BlockPos,
    /// The tile holding the cell just outside the hit face: where a place goes. It
    /// is `tile` itself when the face lies inside the tile (a low floor: the place
    /// fills the rest of it).
    pub outside: BlockPos,
    /// The ray began inside a brick (no face was crossed): nothing can be placed.
    pub inside: bool,
}

/// The tile holding region-local point `p` (sim units) — plain floors, with none of
/// [`crate::feet_tile`]'s slack for a standing body.
fn tile_of_point(p: rapier3d::math::Vector) -> BlockPos {
    let (w, h) = (crate::tile_width_units(), crate::tile_height_units());
    [libm::floorf(p.x / w) as i32, libm::floorf(p.y / h) as i32, libm::floorf(p.z / w) as i32]
}

impl Rollback {
    /// The first stud cell along a ray (region-local units) within `reach` units, own
    /// ∪ stud halo (`raycast_studs_seen`), as its brick's target box and tiles.
    pub fn stud_ray_tiles(&self, origin: rapier3d::math::Vector, dir: rapier3d::math::Vector, reach: f32) -> Option<StudEyeHit> {
        let hit = self.raycast_studs_seen(origin, dir, reach)?;
        let (cs, cp) = (crate::STUD_CHUNK_STUDS as i64, crate::STUD_CHUNK_PLATES as i64);
        let base = [hit.chunk[0] * cs, hit.chunk[1] * cp, hit.chunk[2] * cs];
        let cell = [0, 1, 2].map(|a| base[a] + i64::from(hit.cell[a]));
        let (ts, tp) = (crate::TILE_STUDS as i64, crate::TILE_PLATES as i64);
        let tile_of = |c: [i64; 3]| [c[0].div_euclid(ts) as i32, c[1].div_euclid(tp) as i32, c[2].div_euclid(ts) as i32];
        let out = [0, 1, 2].map(|a| cell[a] + i64::from(hit.normal[a]));
        let brick = self.stud_chunk_seen(hit.chunk)?.brick_at(hit.cell)?;
        let (lo, hi) = stud_target_box(brick, hit.cell);
        let local = |c: [u8; 3]| [0, 1, 2].map(|a| (base[a] + i64::from(c[a])) as i32);
        Some(StudEyeHit {
            ray: hit,
            lo: local(lo),
            hi: local(hi),
            tile: tile_of(cell),
            outside: tile_of(out),
            inside: hit.normal == [0; 3],
        })
    }

    /// The stud brick under `client`'s crosshair within [`crate::edit::REACH`] metres.
    pub fn eye_stud_hit(&self, client: ClientId) -> Option<StudEyeHit> {
        let (eye, dir) = self.eye_ray(client)?;
        self.stud_ray_tiles(eye, dir, crate::edit::REACH * crate::movement::UNITS_PER_METER)
    }

    /// The brick covering region-local stud cell `cell` as this region sees it (own ∪
    /// stud halo, materialised chunks only): its target box ([`stud_target_box`],
    /// region-local cells) and the brick. `None` for air or implied ground.
    pub fn stud_target_at(&self, cell: [i32; 3]) -> Option<([i32; 3], [i32; 3], crate::StudBrick)> {
        let dims = [crate::STUD_CHUNK_STUDS as i32, crate::STUD_CHUNK_PLATES as i32, crate::STUD_CHUNK_STUDS as i32];
        let chunk = [0, 1, 2].map(|a| cell[a].div_euclid(dims[a]));
        let local = [0, 1, 2].map(|a| cell[a].rem_euclid(dims[a]) as u8);
        let brick = *self.stud_chunk_seen(chunk.map(i64::from))?.brick_at(local)?;
        let (lo, hi) = stud_target_box(&brick, local);
        let at = |c: [u8; 3]| [0, 1, 2].map(|a| chunk[a] * dims[a] + i32::from(c[a]));
        Some((at(lo), at(hi), brick))
    }

    /// Ticks to mine the brick whose target box starts at region-local stud cell `lo`
    /// by hand, as this region sees it (own ∪ stud halo): [`stud_break_ticks`] of its
    /// material over the box's cells, times [`FELL_TICKS_FACTOR`] for tree wood (its
    /// break fells the tree). `None`: nothing to mine there (air, bedrock).
    pub fn stud_mine_ticks(&self, lo: [i32; 3]) -> Option<u32> {
        let (lo, hi, brick) = self.stud_target_at(lo)?;
        let cells = (0..3).map(|a| (hi[a] - lo[a]) as u32).product();
        let ticks = stud_break_ticks(brick.material, cells)?;
        let tree = brick.data != 0 && brick.material == crate::MAT_WOOD;
        Some(if tree { ticks.saturating_mul(FELL_TICKS_FACTOR) } else { ticks })
    }

    /// `client`'s mining as the HUD shows it ([`mining_view`], with the mined box's
    /// size read from the terrain; a tile's worth where the brick is already gone).
    pub fn mining_view(&self, client: ClientId, now: u64) -> Option<MiningView> {
        let c = self.data.clients.get(&client)?;
        let m = c.mining?;
        let size = self
            .stud_target_at(m.target)
            .map_or(MINE_BRICK_SIZE, |(lo, hi, _)| [0, 1, 2].map(|a| (hi[a] - lo[a]) as u8));
        mining_view(c, now, size)
    }

    /// Ticks to dig stud tile `p` by hand ([`stud_break_ticks`] of its dominant
    /// material over the cells under its columns' tops); `None` for an open tile or
    /// bedrock. What a fortress dig job of a whole tile costs.
    pub fn stud_tile_break_ticks(&self, p: crate::TilePos) -> Option<u32> {
        let tile = self.stud_tile(p);
        stud_break_ticks(tile.material()?, tile.cells_under_tops())
    }

    /// The id of `client`'s body, if it has one here.
    pub(crate) fn client_body_id(&self, client: ClientId) -> Option<EntityId> {
        self.data.player_entites.get(&client).and_then(|&k| self.entity_id(k))
    }

    /// Writes `client`'s mining state if it differs (one logged delta).
    fn set_mining_safe(&mut self, client: ClientId, mining: Option<Mining>, cooldown_until: Option<u64>) {
        let Some(c) = self.data.clients.get(&client) else { return };
        let until = cooldown_until.unwrap_or(c.break_cooldown_until);
        if c.mining == mining && c.break_cooldown_until == until {
            return;
        }
        if let Some(c) = self.data.clients.get_mut(&client) {
            c.mining = mining;
            c.break_cooldown_until = until;
        }
    }

    /// One tick of `client`'s survival mining (Decision 7), at region tick
    /// `tick`. Only an Adventure client in first person with no inventory
    /// open and the left button held mines; anything else cancels. The
    /// break goes to the block's owner as an intent (into `out` when that is
    /// a neighbour), sent on the tick before the last one held.
    pub(crate) fn tick_mining_safe(&mut self, client: ClientId, tick: u64, out: &mut Vec<IntentMsg>) {
        let Some(c) = self.data.clients.get(&client) else { return };
        let active = c.mode == PlayMode::Adventure
            && c.fps_cam_mode
            && !c.inventory_open
            && c.input.mouse_held(&BREAK_BUTTON)
            && tick >= c.break_cooldown_until;
        let current = c.mining;
        // The brick under the crosshair and the ticks it takes (`stud_mine_ticks`).
        let target = if !active {
            None
        } else {
            self.eye_stud_hit(client).and_then(|hit| self.stud_mine_ticks(hit.lo).map(|t| (hit.lo, t)))
        };
        let Some((target, needed_ticks)) = target else {
            self.set_mining_safe(client, None, None);
            return;
        };
        let mining = match current {
            Some(m) if m.target == target => m,
            _ => Mining { target, started_tick: tick, needed_ticks },
        };
        // Held through this tick and the next one counts the needed ticks:
        // the break, applied at the next tick's opening, is due now.
        if (tick + 2).saturating_sub(mining.started_tick) < u64::from(mining.needed_ticks) {
            self.set_mining_safe(client, Some(mining), None);
            return;
        }
        let Some(by) = self.client_body_id(client) else { return };
        let Some((lo, hi, _)) = self.stud_target_at(target) else { return };
        let region = *self.data.region;
        let (lo, hi) = (crate::world_stud_cell(region, lo.map(i64::from)), crate::world_stud_cell(region, hi.map(i64::from)));
        RegionEffects { data: self, out }
            .send(Target::Stud(crate::stud_chunk_of_cell(lo)), Intent::BreakStud { lo, hi, by, survival: true });
        // The progress stays until the break lands; the cooldown counts from
        // the tick it lands at.
        self.set_mining_safe(client, Some(mining), Some(tick + 2 + BREAK_COOLDOWN_TICKS));
    }

    /// `client`'s right click in survival (Decision 8): takes one of the
    /// selected hotbar block and sends it to the cell against the hit face
    /// (an intent to that cell's owner, into `out` when that is a
    /// neighbour). Returns whether a place was sent. A non-block item, an
    /// empty slot, a hit from inside a block, a cell seen occupied or a body
    /// in the way place nothing.
    pub(crate) fn survival_place_safe(&mut self, client: ClientId, out: &mut Vec<IntentMsg>) -> bool {
        // The tile hit and the tile against the hit face (`None` from inside one).
        let Some(hit) = self.eye_stud_hit(client) else { return false };
        let (hit_cell, against) = (hit.tile, (!hit.inside).then_some(hit.outside));
        // A chest under the crosshair — here, or a neighbour's seen through
        // the halo — opens instead (Decision 10).
        let [x, y, z] = hit_cell;
        let cell = WorldBlockPos::from_local(*self.data.region, crate::BlockPos::new(x, y, z));
        if self.data.container_seen(cell).is_some() {
            self.open_container_safe(client, cell);
            return false;
        }
        let Some(c) = self.data.clients.get(&client) else { return false };
        let selected = c.selected;
        let Some(stack) = c.inventory.slots.get(selected as usize).copied().flatten() else { return false };
        if selected as usize >= crate::HOTBAR_SLOTS {
            return false;
        }
        let Some(id) = stack.item.as_block().filter(|id| *id != BlockId::AIR) else { return false };
        let Some(by) = self.client_body_id(client) else { return false };
        // The chest block is a chest in the tile against the face, as before.
        if crate::chest_block() == Some(id) {
            let Some(target) = against else { return false };
            if self.stud_tile(target).fill() == crate::TileFill::Solid || self.stud_tile_overlaps_body(target) {
                return false;
            }
            if let Some(c) = self.data.clients.get_mut(&client) {
                let _ = c.inventory.take_selected(selected, 1);
            }
            let p = WorldBlockPos::from_local(*self.data.region, crate::BlockPos::new(target[0], target[1], target[2]));
            RegionEffects { data: self, out }.send(Target::Block(p), Intent::PlaceBlock { block: id, by, survival: true });
            return true;
        }
        // Anything else: one brick against the face.
        let Some((chunk, brick)) = self.seen_brick_placement(&hit, crate::stud_material_of_block(id)) else { return false };
        if let Some(c) = self.data.clients.get_mut(&client) {
            let _ = c.inventory.take_selected(selected, 1);
        }
        RegionEffects { data: self, out }.send(Target::Stud(chunk), Intent::PlaceBrick { chunk, brick, item: id, by });
        true
    }

    /// [`brick_placement`] against `hit` as this region sees it: `None` when the
    /// brick would overlap a brick it sees (own ∪ halo) or a body. The owner checks
    /// again.
    pub(crate) fn seen_brick_placement(&self, hit: &StudEyeHit, material: u16) -> Option<(crate::WorldChunkPos, crate::StudBrick)> {
        let (chunk, brick) = brick_placement(&hit.ray, material)?;
        if self.stud_chunk_seen(chunk).is_some_and(|c| c.clone().place(brick).is_err()) {
            return None;
        }
        let local = [chunk[0] as i32, chunk[1] as i32, chunk[2] as i32];
        if self.stud_brick_overlaps_body(local, &brick) {
            return None;
        }
        Some((crate::WorldChunkPos::from_local(*self.data.region, local), brick))
    }

    /// The survival places refused by their owner, in `client`'s inbox this
    /// tick, give their item back: into the inventory, else (full) as a
    /// pile at the feet.
    pub(crate) fn refund_refused_places_safe(&mut self, client: ClientId) {
        let Some(by) = self.client_body_id(client) else { return };
        let refunds: Vec<BlockId> = self
            .intent_inbox(by)
            .into_iter()
            .filter_map(|r| match r {
                IntentReply::Refused(Intent::PlaceBlock { block, survival: true, .. }, why)
                | IntentReply::Refused(Intent::PlaceBrick { item: block, .. }, why) => {
                    log::debug!("[mining] place of {block:?} refused ({why:?}): refunded");
                    Some(block)
                }
                _ => None,
            })
            .collect();
        for block in refunds {
            let stack = ItemStack { item: ItemId::block(block), count: 1 };
            let left = self.data.clients.get_mut(&client).map_or(1, |c| c.inventory.add(stack));
            if left > 0 {
                self.drop_at_feet_safe(client, ItemStack { count: left, ..stack });
            }
        }
    }

    /// Throws one of `client`'s selected hotbar item (the whole stack with
    /// `whole_stack`) [`THROW_DISTANCE_BLOCKS`] ahead along its look, short of
    /// the first block in the way, settled as a pile that can be picked up
    /// after [`THROWN_PICKUP_DELAY_TICKS`]. Returns whether anything was
    /// thrown.
    pub fn throw_selected_safe(&mut self, client: ClientId, whole_stack: bool, tick: u64) -> bool {
        let Some((eye, dir)) = self.eye_ray(client) else { return false };
        let Some(c) = self.data.clients.get(&client) else { return false };
        let selected = c.selected;
        if selected as usize >= crate::HOTBAR_SLOTS || c.inventory.slots.get(selected as usize).is_none_or(|s| s.is_none()) {
            return false;
        }
        let bv = BLOCK_UNITS as f32;
        // The tile the stud ray stops before (the distance is in metres).
        let landing = match self.stud_ray_tiles(eye, dir, THROW_DISTANCE_BLOCKS * bv) {
            Some(hit) if hit.inside => tile_of_point(eye),
            Some(hit) => hit.outside,
            None => tile_of_point(eye + dir * (THROW_DISTANCE_BLOCKS * bv)),
        };
        let n = if whole_stack { MAX_STACK } else { 1 };
        let Some(stack) = self.data.clients.get_mut(&client).and_then(|c| c.inventory.take_selected(selected, n)) else {
            return false;
        };
        // A loss to the pile caps is counted where it happens.
        // From the eye: the renderer arcs it to where it lands (`ItemsLanded`).
        // A landing across a seam is a deposit intent to its owner; what its
        // piles cannot hold comes back to the thrower.
        let by = self.client_body_id(client).unwrap_or(crate::EntityId::of_region(*self.data.region));
        let _destroyed =
            self.deposit_items_by_safe(by, tile_of_point(eye), landing, stack, tick, THROWN_PICKUP_DELAY_TICKS);
        true
    }
}
