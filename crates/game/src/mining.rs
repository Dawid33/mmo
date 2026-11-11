//! Survival mining, placing and throwing (plan 2026-11-05, Decisions 7, 8
//! and 11): what an Adventure player's mouse buttons and Q do. All three work
//! on TILES (1 m x 1.2 m x 1 m, `tiles.rs`), the unit digging and building have;
//! the eye ray is a stud ray over own ∪ stud halo ([`Rollback::eye_stud_hit`]).
//!
//! - **Mining** is sim state on `Client` ([`Mining`]): while the left button
//!   is held and the eye ray hits the same tile, progress builds; a new
//!   target restarts it, a release (or leaving first person, or opening the
//!   inventory) cancels it. Progress is never accumulated: it is
//!   `tick - started_tick`, all integer ticks. The tile breaks on the
//!   [`Rollback::stud_mine_ticks`]-th tick held (the start tick counts;
//!   [`Rollback::stud_tile_break_ticks`], twice that for a tile of tree wood),
//!   and no mining progresses for [`BREAK_COOLDOWN_TICKS`] ticks after.
//!
//!   The tile may be the neighbour's, seen through the halo: the break is
//!   always an [`crate::Intent::BreakBlock`] (`survival`) to the tile's
//!   owner (a `Target::Block` whose cell is a world tile, as piles and chests
//!   are addressed), here or across a seam alike, which applies it at the
//!   opening of the next tick (`Rollback::apply_stud_tile_intent_safe`) — so it
//!   is sent on the tick before the last one held. The owner's refusals decide;
//!   the break carves the tile and drops [`stud_drops`] into the owner's piles
//!   (a tree tile fells the tree).
//! - **Placing** (right press) fills the tile against the hit face with the
//!   selected hotbar block's stud material (the chest block: a chest) and takes
//!   one from the slot at once; the fill itself is an
//!   [`crate::Intent::PlaceBlock`] to the tile's owner (next tick), and a
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

/// The block a client is mining.
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
    pub target: BlockPos,
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
/// block (a plastic) or whose block drops nothing yields nothing.
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

/// `client`'s mining progress at `now` (the region's current tick, i.e. after
/// the ticks so far), or `None` while it mines nothing.
pub fn mining_view(client: &Client, now: u64) -> Option<MiningView> {
    let m = client.mining?;
    let held = now.saturating_sub(m.started_tick);
    let progress = (held * 1000 / u64::from(m.needed_ticks.max(1))).min(1000) as u32;
    Some(MiningView { target: m.target, progress_per_mille: progress })
}

/// What a stud ray met, in tiles ([`Rollback::stud_ray_tiles`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StudEyeHit {
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
    /// ∪ stud halo (`raycast_studs_seen`), as tiles.
    pub fn stud_ray_tiles(&self, origin: rapier3d::math::Vector, dir: rapier3d::math::Vector, reach: f32) -> Option<StudEyeHit> {
        let hit = self.raycast_studs_seen(origin, dir, reach)?;
        let (cs, cp) = (crate::STUD_CHUNK_STUDS as i64, crate::STUD_CHUNK_PLATES as i64);
        let cell = [
            hit.chunk[0] * cs + i64::from(hit.cell[0]),
            hit.chunk[1] * cp + i64::from(hit.cell[1]),
            hit.chunk[2] * cs + i64::from(hit.cell[2]),
        ];
        let (ts, tp) = (crate::TILE_STUDS as i64, crate::TILE_PLATES as i64);
        let tile_of = |c: [i64; 3]| [c[0].div_euclid(ts) as i32, c[1].div_euclid(tp) as i32, c[2].div_euclid(ts) as i32];
        let out = [0, 1, 2].map(|a| cell[a] + i64::from(hit.normal[a]));
        Some(StudEyeHit { tile: tile_of(cell), outside: tile_of(out), inside: hit.normal == [0; 3] })
    }

    /// The stud tile under `client`'s crosshair within [`crate::edit::REACH`] metres.
    pub fn eye_stud_hit(&self, client: ClientId) -> Option<StudEyeHit> {
        let (eye, dir) = self.eye_ray(client)?;
        self.stud_ray_tiles(eye, dir, crate::edit::REACH * crate::movement::UNITS_PER_METER)
    }

    /// Ticks to mine stud tile `p` by hand as this region sees it (own ∪ stud halo):
    /// [`Self::stud_tile_break_ticks`], times [`FELL_TICKS_FACTOR`] for a tile of
    /// tree wood (its break fells the tree). `None`: nothing to mine (open, bedrock).
    pub fn stud_mine_ticks(&self, p: BlockPos) -> Option<u32> {
        let ticks = self.stud_tile_break_ticks(p)?;
        Some(if self.stud_tree_at(p).is_some() { ticks.saturating_mul(FELL_TICKS_FACTOR) } else { ticks })
    }

    /// Ticks to dig stud tile `p` by hand ([`stud_break_ticks`] of its dominant
    /// material over the cells under its columns' tops); `None` for an open tile or
    /// bedrock.
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
        // The tile under the crosshair and the ticks it takes (`stud_mine_ticks`).
        let target = if !active {
            None
        } else {
            self.eye_stud_hit(client).and_then(|hit| self.stud_mine_ticks(hit.tile).map(|t| (hit.tile, t)))
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
        let p = WorldBlockPos::from_local(*self.data.region, crate::BlockPos::new(target[0], target[1], target[2]));
        RegionEffects { data: self, out }.send(Target::Block(p), Intent::BreakBlock { by, survival: true });
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
        let Some(target) = against else { return false };
        if self.stud_tile(target).fill() == crate::TileFill::Solid || self.stud_tile_overlaps_body(target) {
            return false;
        }
        let Some(by) = self.client_body_id(client) else { return false };
        if let Some(c) = self.data.clients.get_mut(&client) {
            let _ = c.inventory.take_selected(selected, 1);
        }
        let p = WorldBlockPos::from_local(*self.data.region, crate::BlockPos::new(target[0], target[1], target[2]));
        RegionEffects { data: self, out }.send(Target::Block(p), Intent::PlaceBlock { block: id, by, survival: true });
        true
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
                IntentReply::Refused(Intent::PlaceBlock { block, survival: true, .. }, why) => {
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
