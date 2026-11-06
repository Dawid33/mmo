//! Dropped items as cell-keyed piles (plan 2026-11-05, Decisions 1–4;
//! seams: plan 2026-11-06 Task 6.3).
//!
//! A dropped item is never an entity and never a rapier body: it is a stack
//! in the [`ItemPile`] of the block cell it rests in, kept in
//! `GameData::item_piles` (a `BTreeMap<WorldBlockPos, ItemPile>`) by the
//! region that OWNS the cell. A resting pile costs nothing per tick, and the
//! number of drawable things is bounded by occupied cells.
//!
//! - **Settling** ([`Rollback::settle_cell`]) is an integer column walk in
//!   world cells: up out of solid blocks, then down while the cell below is
//!   not solid. "Solid" is the pathfinder's floor rule over the block the sim
//!   sees — its own (implied ground included) or the halo's: not air, not
//!   water, and solid by the manifest (leaves are not). A walk stops at the
//!   edge of what the region sees (own ∪ halo). No floats enter pile state.
//! - **Merge on deposit** ([`Rollback::deposit_items_safe`]): same-item
//!   stacks in piles within [`PILE_MERGE_RADIUS_BLOCKS`] are topped up first
//!   (nearest by Chebyshev distance, ties by cell key), then new stacks go
//!   into the rest cell's pile, then into the nearest cell of the radius that
//!   rests where it is and has a free stack slot; anything left is destroyed
//!   (and counted). A pile holds at most [`PILE_MAX_STACKS`] stacks. The
//!   search reads the neighbours' piles and blocks through the halo; what it
//!   puts into a NEIGHBOUR's cell is an [`Intent::DepositItems`] to that
//!   cell's owner (`Target::Pile`), which puts it into that cell first, then
//!   into its own cells around it ([`Rollback::apply_pile_intent_safe`]).
//!   What does not fit there comes back (`Refused(.., PileFull)` with the
//!   rest): to the sending body's inventory (else its feet), or — for a
//!   region-sourced deposit ([`crate::EntityId::of_region`]: a settling
//!   voxel body, a chest's spill) — put down in the sender's own cell nearest
//!   the target ([`Rollback::region_reply_safe`]).
//! - **Support** changes (`edit.rs` hooks every block write): a pile whose
//!   support is broken falls and re-merges where it lands; a block placed
//!   into a pile's cell pushes it up ([`Rollback::resettle_pile_safe`]).
//!   Support is the block below, in the same column: always the owner's.
//! - **Lifetime**: [`PILE_DESPAWN_TICKS`], indexed by
//!   `GameData::pile_expiry` so only due piles are touched; a merge keeps the
//!   later expiry and the later pickup tick; at [`PILES_PER_REGION_MAX`] the
//!   earliest-expiring pile is evicted.
//! - **Pickup** ([`Rollback::tick_pickups_safe`]): every Adventure body takes
//!   what fits from the piles in its box grown by [`PICKUP_REACH_H_BLOCKS`] /
//!   [`PICKUP_REACH_V_BLOCKS`], clients in id order, cells in key order. A
//!   pile in a neighbour's cell (its halo mirror) is asked for with an
//!   [`Intent::TakeFromPile`]: the inventory changes only when the owner
//!   grants it ([`Rollback::settle_pile_replies_safe`]), so a race for the
//!   last item is won by one taker and the other is refused. No new take of
//!   that cell is sent for [`PILE_TAKE_RETRY_TICKS`] (the mirror lags).
//! - **Halo**: piles within `HALO_BLOCKS` of an edge are mirrored into the
//!   neighbours' `GameData::halo_piles`, read-only, on the terrain halo's
//!   stamped channel (`HaloUpdate::piles`; seeds carry them all).
//!
//! Every write goes through one funnel that emits `SetItemPiles` with the
//! new views (Do) and registers the prior views for undo, like
//! `orders.rs`'s designations; the halo mirrors through their own
//! (`SetHaloPiles`). Render cells are in the emitting region's block frame.

use std::collections::BTreeMap;

use crate::edit::BlockPos;
use crate::intents::{Effects as _, RegionEffects};
use crate::movement::{PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS};
use crate::{
    block_props, BlockId, ClientId, EntityId, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, Intent,
    IntentMsg, IntentRefusal, IntentReply, ItemId, ItemStack, RegionCoords, Rollback, Target, WorldBlockPos,
    BLOCK_VOXELS, HALO_BLOCKS, MAX_STACK, REGION_BLOCKS,
};

/// Most stacks one pile (one cell) holds.
pub const PILE_MAX_STACKS: usize = 4;
/// How far (blocks, Chebyshev, on all three axes) a deposit looks for piles
/// to merge into or cells to spill into.
pub const PILE_MERGE_RADIUS_BLOCKS: i32 = 1;
/// Furthest a settling pile falls (blocks); it stops there, mid-air, rather
/// than walk an unbounded void.
pub const MAX_PILE_FALL_BLOCKS: i32 = 64;
/// Furthest a settling pile rises out of solid blocks (blocks) — deeper
/// than bedrock lies under any surface, so a deposit into rock surfaces.
pub const MAX_PILE_RISE_BLOCKS: i32 = 256;
/// Ticks a pile lives after its latest deposit (Minecraft's 6000).
pub const PILE_DESPAWN_TICKS: u64 = 6000;
/// Piles one region holds; past it the earliest-expiring is evicted.
pub const PILES_PER_REGION_MAX: usize = 4096;
/// Ticks before a dropped pile can be picked up.
pub const PICKUP_DELAY_TICKS: u64 = 10;
/// Ticks before a thrown pile can be picked up (the thrower walks on).
pub const THROWN_PICKUP_DELAY_TICKS: u64 = 40;
/// Cells a body's box is grown by horizontally to pick piles up.
pub const PICKUP_REACH_H_BLOCKS: i32 = 1;
/// Cells a body's box is grown by vertically to pick piles up.
pub const PICKUP_REACH_V_BLOCKS: i32 = 1;
/// Ticks after a take of a neighbour's pile before the same body sends
/// another take of that cell (the halo mirror shows the pile until the
/// owner's answer comes back through it).
pub const PILE_TAKE_RETRY_TICKS: u64 = 4;

/// The items resting in one cell.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, state_hash::StateHash)]
pub struct ItemPile {
    /// At most [`PILE_MAX_STACKS`], each `1..=MAX_STACK`.
    pub stacks: Vec<ItemStack>,
    /// The tick this pile despawns at.
    pub expire_tick: u64,
    /// The first tick it can be picked up.
    pub pickup_after_tick: u64,
}

impl ItemPile {
    pub fn view(&self) -> PileView {
        PileView { stacks: self.stacks.clone() }
    }

    /// Total items in the pile.
    pub fn count(&self) -> u32 {
        self.stacks.iter().map(|s| s.count).sum()
    }
}

thread_local! {
    /// Items destroyed by the pile caps on this thread (metrics only: never
    /// state, never hashed, never undone — a client replay counts again).
    /// `Region::handle_event` attributes each event's share to its region.
    static ITEMS_DESTROYED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Pile intents sent from deep inside a write (a block break's pile
    /// re-settling, a voxel body's drops) on this thread since the last
    /// [`take_pile_intents`]: `Region::handle_event` adds them to its
    /// outbox. Not state: never hashed, serialized or undone.
    static PILE_INTENTS: std::cell::RefCell<Vec<IntentMsg>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Cells whose own pile was written on this thread since the last
    /// [`take_touched_piles`]: the region that wrote them sends the halo
    /// deltas. Not state.
    static TOUCHED_PILES: std::cell::RefCell<Vec<WorldBlockPos>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The pile intents sent on this thread since the previous call.
pub(crate) fn take_pile_intents() -> Vec<IntentMsg> {
    PILE_INTENTS.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// The own pile cells written on this thread since the previous call.
pub(crate) fn take_touched_piles() -> Vec<WorldBlockPos> {
    TOUCHED_PILES.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// Sends `intent` to the pile at `cell` (a neighbour's) from `r`, into the
/// thread's pile outbox.
fn send_pile_intent(r: &mut Rollback, cell: WorldBlockPos, intent: Intent) {
    let mut out = Vec::new();
    RegionEffects { data: r, out: &mut out }.send(Target::Pile(cell), intent);
    PILE_INTENTS.with(|q| q.borrow_mut().extend(out));
}

/// Offset toward the neighbour a local block coordinate lies against:
/// -1 / +1 within `HALO_BLOCKS` of the low / high edge, else 0.
fn pile_edge_side(c: i32) -> i32 {
    let n = REGION_BLOCKS as i32;
    if c < HALO_BLOCKS {
        -1
    } else if c >= n - HALO_BLOCKS {
        1
    } else {
        0
    }
}

/// The neighbour offsets that mirror an own pile at local block `[x, z]`
/// (none in the interior; up to 3 at a corner).
pub fn pile_halo_targets(x: i32, z: i32) -> Vec<(i32, i32)> {
    let (sx, sz) = (pile_edge_side(x), pile_edge_side(z));
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

/// Whether local block `[x, z]` (a region's frame) lies in that region's pile
/// halo: outside it, within `HALO_BLOCKS` of an edge.
pub fn in_pile_halo_band(x: i32, z: i32) -> bool {
    let n = REGION_BLOCKS as i32;
    let inside = |c: i32| (0..n).contains(&c);
    let near = |c: i32| (-HALO_BLOCKS..n + HALO_BLOCKS).contains(&c);
    near(x) && near(z) && !(inside(x) && inside(z))
}

/// World cell `w` in `rc`'s block frame (outside `0..REGION_BLOCKS` when `w`
/// is another region's).
pub fn local_cell(rc: RegionCoords, w: WorldBlockPos) -> BlockPos {
    let n = REGION_BLOCKS as i64;
    [(w.x - rc.x as i64 * n) as i32, w.y, (w.z - rc.z as i64 * n) as i32]
}

/// Local block `p` of `rc` as a world cell.
fn world_cell(rc: RegionCoords, p: BlockPos) -> WorldBlockPos {
    WorldBlockPos::from_local(rc, crate::BlockPos::new(p[0], p[1], p[2]))
}

fn up(p: WorldBlockPos, dy: i32) -> WorldBlockPos {
    WorldBlockPos::new(p.x, p.y + dy, p.z)
}


/// Items the pile caps have destroyed on this thread so far (see
/// `ITEMS_DESTROYED`); read before and after a unit of work for its share.
pub fn items_destroyed_on_thread() -> u64 {
    ITEMS_DESTROYED.with(|c| c.get())
}

/// The one place pile items are destroyed from: a stack no pile in reach
/// could take (deposit or re-settle overflow) or a pile evicted by
/// [`PILES_PER_REGION_MAX`]. Logs and counts it; returns `n`.
fn record_destroyed(n: u32, at: WorldBlockPos, why: &str) -> u32 {
    if n > 0 {
        log::debug!("[item_piles] destroyed {n} items near {at:?}: {why}");
        ITEMS_DESTROYED.with(|c| c.set(c.get() + u64::from(n)));
    }
    n
}

/// One pickup, for the renderer (`GameDataUpdateKind::ItemsPickedUp`): the
/// pile's cell, the body that took from it, what it took, and the tick.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PickupView {
    pub cell: BlockPos,
    pub entity: crate::EntityKey,
    pub stacks: Vec<ItemStack>,
    pub tick: u64,
}

/// Items that moved without physics, for the renderer
/// (`GameDataUpdateKind::ItemsLanded`): from the cell they started in to the
/// cell they settled in (merges may spread them to its neighbours).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PileFlight {
    pub from: BlockPos,
    pub to: BlockPos,
    pub stacks: Vec<ItemStack>,
    pub tick: u64,
}

/// What the renderer needs of a pile.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PileView {
    pub stacks: Vec<ItemStack>,
}

/// The cells within [`PILE_MERGE_RADIUS_BLOCKS`] of `c`, nearest first
/// (Chebyshev distance), ties in key order. `c` itself comes first.
fn cells_near(c: WorldBlockPos) -> Vec<WorldBlockPos> {
    let r = PILE_MERGE_RADIUS_BLOCKS;
    let mut out = Vec::with_capacity(((2 * r + 1) * (2 * r + 1) * (2 * r + 1)) as usize);
    for dx in -r..=r {
        for dy in -r..=r {
            for dz in -r..=r {
                out.push(WorldBlockPos::new(c.x + dx as i64, c.y + dy, c.z + dz as i64));
            }
        }
    }
    let cheb = |p: &WorldBlockPos| (p.x - c.x).abs().max((p.y - c.y).abs() as i64).max((p.z - c.z).abs());
    out.sort_by_key(|p| (cheb(p), *p));
    out
}

/// Pile edits planned against the live maps before one batched write: the
/// cells touched so far with their new value, reading through to the own
/// piles (own cells) or the halo's mirrors (the neighbours'), and what the
/// plan puts into neighbours' cells (sent as intents).
struct PileOverlay<'a> {
    region: RegionCoords,
    own: &'a BTreeMap<WorldBlockPos, ItemPile>,
    halo: &'a BTreeMap<WorldBlockPos, ItemPile>,
    touched: BTreeMap<WorldBlockPos, Option<ItemPile>>,
    /// Items planned into a neighbour's cell, by `(cell, item)`.
    remote: BTreeMap<(WorldBlockPos, ItemId), u32>,
}

impl<'a> PileOverlay<'a> {
    fn new(r: &'a Rollback) -> Self {
        Self {
            region: *r.data.region,
            own: &r.data.item_piles,
            halo: &r.data.halo_piles,
            touched: BTreeMap::new(),
            remote: BTreeMap::new(),
        }
    }

    fn owns(&self, p: WorldBlockPos) -> bool {
        p.to_local().0 == self.region
    }

    fn base(&self, p: WorldBlockPos) -> Option<&'a ItemPile> {
        if self.owns(p) { self.own.get(&p) } else { self.halo.get(&p) }
    }

    fn get(&self, p: WorldBlockPos) -> Option<&ItemPile> {
        match self.touched.get(&p) {
            Some(v) => v.as_ref(),
            None => self.base(p),
        }
    }

    fn slot(&mut self, p: WorldBlockPos) -> &mut Option<ItemPile> {
        let base = self.base(p).cloned();
        self.touched.entry(p).or_insert(base)
    }

    /// `n` of `item` go into `p`: recorded for its owner when it is a
    /// neighbour's.
    fn added(&mut self, p: WorldBlockPos, item: ItemId, n: u32) {
        if !self.owns(p) && n > 0 {
            *self.remote.entry((p, item)).or_default() += n;
        }
    }

    /// Own piles there would be after the edits.
    fn len(&self) -> usize {
        let mut n = self.own.len();
        for (p, v) in self.touched.iter().filter(|(p, _)| self.owns(**p)) {
            match (self.own.contains_key(p), v.is_some()) {
                (false, true) => n += 1,
                (true, false) => n -= 1,
                _ => {}
            }
        }
        n
    }

    /// The own-cell edits that change something, in cell order.
    fn into_changes(self) -> Vec<(WorldBlockPos, Option<ItemPile>)> {
        let (own, region) = (self.own, self.region);
        self.touched
            .into_iter()
            .filter(|(p, v)| p.to_local().0 == region && own.get(p) != v.as_ref())
            .collect()
    }
}

impl Rollback {
    /// Whether a pile can rest on top of world cell `pos`: the pathfinder's
    /// floor rule (not air, not water, solid by the manifest) over the block
    /// this region sees there — its own (implied ground included) or the
    /// halo's. A cell it does not see ([`Self::pile_cell_seen`]): no.
    pub fn pile_support(&self, pos: WorldBlockPos) -> bool {
        self.pile_cell_seen(pos)
            && self.halo_block(pos).is_some_and(|b| {
                b.id != BlockId::AIR && !crate::pathfind::is_water(b.id) && block_props().is_solid(b.id)
            })
    }

    /// Whether this region sees world cell `p`: its own, or a halo column
    /// it was seeded with. A deposit's search uses only cells it sees.
    pub fn pile_cell_seen(&self, p: WorldBlockPos) -> bool {
        p.to_local().0 == *self.data.region || (self.halo_block(p).is_some() && self.halo_column_known(p))
    }

    /// Where a pile put at `from` comes to rest (Decision 2): up out of solid
    /// blocks (at most [`MAX_PILE_RISE_BLOCKS`]), then down while the cell
    /// below is not solid (at most [`MAX_PILE_FALL_BLOCKS`]) and still seen
    /// ([`Self::pile_cell_seen`]). The column never changes, so neither does its owner.
    pub fn settle_cell(&self, from: WorldBlockPos) -> WorldBlockPos {
        let mut p = from;
        let mut rose = 0;
        while rose < MAX_PILE_RISE_BLOCKS && self.pile_support(p) {
            p = up(p, 1);
            rose += 1;
        }
        let mut fell = 0;
        while fell < MAX_PILE_FALL_BLOCKS && self.pile_cell_seen(up(p, -1)) && !self.pile_support(up(p, -1)) {
            p = up(p, -1);
            fell += 1;
        }
        p
    }

    /// The pile at world cell `p` as this region sees it: its own, or the
    /// halo's mirror of a neighbour's.
    pub fn pile_seen(&self, p: WorldBlockPos) -> Option<&ItemPile> {
        if p.to_local().0 == *self.data.region { self.data.item_piles.get(&p) } else { self.data.halo_piles.get(&p) }
    }

    /// Drops `stack` at local cell `at` (Decision 1): settles, merges into
    /// nearby piles, else starts stacks in the rest cell or a free cell
    /// nearby — a neighbour's cell by an intent to its owner. The new items
    /// despawn at `tick + PILE_DESPAWN_TICKS` and can be picked up from
    /// `tick + pickup_delay`. Region-sourced (no body to give a refusal
    /// back to). Returns how many items the caps destroyed here — overflow
    /// plus any evicted pile's items (0 normally); every one is counted
    /// (`record_destroyed`). Emits `SetItemPiles` both ways.
    pub fn deposit_items_safe(&mut self, at: BlockPos, stack: ItemStack, tick: u64, pickup_delay: u64) -> u32 {
        self.deposit_items_from_safe(at, at, stack, tick, pickup_delay)
    }

    /// [`Self::deposit_items_safe`] for items that start at `origin` (a
    /// throw's eye cell): the renderer animates them from there to where they
    /// settle (`ItemsLanded`), the sim just places them.
    pub fn deposit_items_from_safe(
        &mut self,
        origin: BlockPos,
        at: BlockPos,
        stack: ItemStack,
        tick: u64,
        pickup_delay: u64,
    ) -> u32 {
        let by = EntityId::of_region(*self.data.region);
        self.deposit_items_by_safe(by, origin, at, stack, tick, pickup_delay)
    }

    /// [`Self::deposit_items_from_safe`] sent by entity `by`: a part a
    /// neighbour cannot hold comes back to it (`settle_pile_replies_safe`).
    pub fn deposit_items_by_safe(
        &mut self,
        by: EntityId,
        origin: BlockPos,
        at: BlockPos,
        stack: ItemStack,
        tick: u64,
        pickup_delay: u64,
    ) -> u32 {
        let rc = *self.data.region;
        let (origin, at) = (world_cell(rc, origin), world_cell(rc, at));
        let (expire, pickup) = (tick + PILE_DESPAWN_TICKS, tick + pickup_delay);
        let mut ov = PileOverlay::new(self);
        let overflow = plan_deposit(self, &mut ov, at, stack, expire, pickup, true);
        let remote = std::mem::take(&mut ov.remote);
        let (changes, evicted) = self.capped(ov);
        self.write_piles_safe(changes);
        for ((cell, item), count) in remote {
            let stack = ItemStack { item, count };
            send_pile_intent(self, cell, Intent::DepositItems { cell, stack, expire, pickup, by });
        }
        self.send_landing(origin, self.settle_cell(at), vec![stack], tick);
        record_destroyed(overflow, at, "deposit overflow") + record_destroyed(evicted, at, "pile cap eviction")
    }

    /// Tells the renderer items went from `from` to `to` (nothing if they
    /// did not move), in this region's frame. Render-only.
    fn send_landing(&self, from: WorldBlockPos, to: WorldBlockPos, stacks: Vec<ItemStack>, tick: u64) {
        if from != to && !stacks.is_empty() {
            let rc = *self.data.region;
            let flight = PileFlight { from: local_cell(rc, from), to: local_cell(rc, to), stacks, tick };
            self.data.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::ItemsLanded(vec![flight])));
        }
    }

    /// Takes the pile at local cell `cell` away and deposits its stacks
    /// again from `cell`, keeping its expiry and pickup tick: it falls if its
    /// support went, or rises out of a block placed into its cell. `tick` is
    /// the current tick (it stamps the render-only `ItemsLanded`; the pile
    /// keeps its own times). What merges into a neighbour's pile goes as a
    /// region-sourced intent.
    /// Returns the items destroyed (counted, as for a deposit).
    pub fn resettle_pile_safe(&mut self, cell: BlockPos, tick: u64) -> u32 {
        let cell = world_cell(*self.data.region, cell);
        let by = EntityId::of_region(*self.data.region);
        let mut ov = PileOverlay::new(self);
        let Some(pile) = ov.slot(cell).take() else { return 0 };
        let stacks = pile.stacks.clone();
        let mut overflow = 0;
        for s in pile.stacks {
            overflow += plan_deposit(self, &mut ov, cell, s, pile.expire_tick, pile.pickup_after_tick, true);
        }
        let remote = std::mem::take(&mut ov.remote);
        let (changes, evicted) = self.capped(ov);
        self.write_piles_safe(changes);
        for ((at, item), count) in remote {
            let stack = ItemStack { item, count };
            let (expire, pickup) = (pile.expire_tick, pile.pickup_after_tick);
            send_pile_intent(self, at, Intent::DepositItems { cell: at, stack, expire, pickup, by });
        }
        self.send_landing(cell, self.settle_cell(cell), stacks, tick);
        record_destroyed(overflow, cell, "re-settle overflow") + record_destroyed(evicted, cell, "pile cap eviction")
    }

    /// After block `pos` (local) was written: a pile pushed up out of it, or
    /// the pile it held up falls. Called by every terrain write in `edit.rs`.
    /// Returns the items destroyed (already counted).
    pub(crate) fn piles_after_write_safe(&mut self, pos: BlockPos) -> u32 {
        if self.data.item_piles.is_empty() {
            return 0;
        }
        let tick = *self.data.tick;
        let w = world_cell(*self.data.region, pos);
        let cell = if self.pile_support(w) { pos } else { [pos[0], pos[1] + 1, pos[2]] };
        if self.data.item_piles.contains_key(&world_cell(*self.data.region, cell)) {
            self.resettle_pile_safe(cell, tick)
        } else {
            0
        }
    }

    /// Every Adventure client's body picks up the piles in its reach
    /// (Decision 4) whose pickup tick has come: clients in id order, cells in
    /// key order, each pile's stacks in order, as far as the inventory has
    /// room; the rest stays in the pile. A neighbour's pile (the halo's
    /// mirror) is asked for with one `TakeFromPile` per item, sent to its
    /// owner; the inventory changes when the grant comes back.
    pub fn tick_pickups_safe(&mut self, tick: u64) {
        let expired: Vec<_> = self.data.pile_takes.iter().filter(|(_, t)| **t <= tick).map(|(k, _)| *k).collect();
        for k in expired {
            self.data.pile_takes.remove(&k);
        }
        if self.data.item_piles.is_empty() && self.data.halo_piles.is_empty() {
            return;
        }
        let rc = *self.data.region;
        let bv = BLOCK_VOXELS as f32;
        let (r, half) = (PLAYER_CAPSULE_RADIUS, PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS);
        let (h, v) = (PICKUP_REACH_H_BLOCKS, PICKUP_REACH_V_BLOCKS);
        let mut takers: Vec<(ClientId, crate::EntityKey, Option<EntityId>, BlockPos, BlockPos)> = Vec::new();
        for (id, c) in self.data.clients.iter() {
            if c.mode != crate::PlayMode::Adventure {
                continue;
            }
            let Some(&e) = self.data.player_entites.get(id) else { continue };
            let Some(handle) = *self.data.ecs.rigidbody.try_get(e) else { continue };
            let Some(body) = self.data.physics.bodies.get(handle) else { continue };
            let t = body.translation();
            let cell = |x: f32| (x / bv).floor() as i32;
            let lo = [cell(t.x - r) - h, cell(t.y - half) - v, cell(t.z - r) - h];
            let hi = [cell(t.x + r) + h, cell(t.y + half) + v, cell(t.z + r) + h];
            takers.push((*id, e, self.entity_id(e), lo, hi));
        }
        if takers.is_empty() {
            return;
        }
        let mut ov = PileOverlay::new(self);
        let mut gains = Vec::new();
        let mut picks = Vec::new();
        // (taker, cell, item, count): asks of neighbours' piles.
        let mut asks: Vec<(EntityId, WorldBlockPos, ItemId, u32)> = Vec::new();
        for (id, entity, eid, lo, hi) in takers {
            let mut inv = self.data.clients[&id].inventory.clone();
            // `inv` plus what is asked of neighbours: the room left.
            let mut room = inv.clone();
            let mut took = false;
            for x in lo[0]..=hi[0] {
                for y in lo[1]..=hi[1] {
                    for z in lo[2]..=hi[2] {
                        let c = world_cell(rc, [x, y, z]);
                        let Some(pile) = ov.get(c) else { continue };
                        if pile.pickup_after_tick > tick {
                            continue;
                        }
                        if !ov.owns(c) {
                            let Some(eid) = eid else { continue };
                            if self.data.pile_takes.contains_key(&(eid, c)) {
                                continue;
                            }
                            let mut want: BTreeMap<ItemId, u32> = BTreeMap::new();
                            for &s in &pile.stacks {
                                let n = s.count - room.add(s);
                                if n > 0 {
                                    *want.entry(s.item).or_default() += n;
                                }
                            }
                            asks.extend(want.into_iter().map(|(item, n)| (eid, c, item, n)));
                            continue;
                        }
                        let mut left = Vec::with_capacity(pile.stacks.len());
                        let mut taken = Vec::new();
                        let mut moved = false;
                        for &s in &pile.stacks {
                            let rest = inv.add(s);
                            room.add(ItemStack { count: s.count - rest, ..s });
                            moved |= rest < s.count;
                            if rest < s.count {
                                taken.push(ItemStack { item: s.item, count: s.count - rest });
                            }
                            if rest > 0 {
                                left.push(ItemStack { item: s.item, count: rest });
                            }
                        }
                        if !moved {
                            continue;
                        }
                        took = true;
                        picks.push(PickupView { cell: local_cell(rc, c), entity, stacks: taken, tick });
                        let slot = ov.slot(c);
                        if left.is_empty() {
                            *slot = None;
                        } else if let Some(p) = slot.as_mut() {
                            p.stacks = left;
                        }
                    }
                }
            }
            if took {
                gains.push((id, inv));
            }
        }
        let changes = ov.into_changes();
        for (id, inv) in gains {
            if let Some(c) = self.data.clients.get_mut(&id) {
                c.inventory = inv;
            }
        }
        self.write_piles_safe(changes);
        for (by, cell, item, count) in asks {
            self.data.pile_takes.insert((by, cell), tick + PILE_TAKE_RETRY_TICKS);
            send_pile_intent(self, cell, Intent::TakeFromPile { cell, item, count, by });
        }
        if !picks.is_empty() {
            self.data.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::ItemsPickedUp(picks)));
        }
    }

    /// The owner applies a pile intent at its own cell `cell`
    /// (`intents.rs`). A deposit goes into `cell` first (when a pile rests
    /// there), then into the owner's own cells around it; what does not fit
    /// is refused back to the sender (`PileFull`, carrying the rest). A take
    /// takes what the pile has of the item, up to the count asked.
    pub(crate) fn apply_pile_intent_safe(&mut self, cell: WorldBlockPos, intent: Intent) -> IntentReply {
        let now = *self.data.tick;
        match intent {
            Intent::DepositItems { stack, expire, pickup, .. } => {
                let mut ov = PileOverlay::new(self);
                let mut left = stack;
                if self.settle_cell(cell) == cell {
                    left.count = plan_into_cell(&mut ov, cell, stack, expire, pickup);
                }
                left.count = plan_deposit(self, &mut ov, cell, left, expire, pickup, false);
                debug_assert!(ov.remote.is_empty(), "an owner's deposit stays in its own cells");
                let (changes, evicted) = self.capped(ov);
                self.write_piles_safe(changes);
                record_destroyed(evicted, cell, "pile cap eviction");
                if left.count == 0 {
                    IntentReply::Granted(intent)
                } else {
                    let rest = match intent {
                        Intent::DepositItems { cell, expire, pickup, by, .. } => {
                            Intent::DepositItems { cell, stack: left, expire, pickup, by }
                        }
                        other => other,
                    };
                    IntentReply::Refused(rest, IntentRefusal::PileFull)
                }
            }
            Intent::TakeFromPile { cell: c, item, count, by } => {
                let refused = IntentReply::Refused(intent, IntentRefusal::NothingThere);
                let Some(pile) = self.data.item_piles.get(&cell).filter(|p| p.pickup_after_tick <= now) else {
                    return refused;
                };
                let mut pile = pile.clone();
                let mut want = count;
                for s in pile.stacks.iter_mut().filter(|s| s.item == item) {
                    let n = s.count.min(want);
                    s.count -= n;
                    want -= n;
                }
                let taken = count - want;
                if taken == 0 {
                    return refused;
                }
                pile.stacks.retain(|s| s.count > 0);
                let new = (!pile.stacks.is_empty()).then_some(pile);
                self.write_piles_safe(vec![(cell, new)]);
                IntentReply::Granted(Intent::TakeFromPile { cell: c, item, count: taken, by })
            }
            other => IntentReply::Refused(other, IntentRefusal::BadTarget),
        }
    }

    /// `client`'s body's pile replies this tick: a granted take puts its
    /// items into the inventory (what does not fit goes down at the feet),
    /// and a deposit a neighbour refused gives its rest back the same way.
    pub(crate) fn settle_pile_replies_safe(&mut self, client: ClientId) {
        let Some(&key) = self.data.player_entites.get(&client) else { return };
        let Some(by) = self.entity_id(key) else { return };
        let rc = *self.data.region;
        let tick = *self.data.tick;
        let mut back = Vec::new();
        let mut picks = Vec::new();
        for reply in self.intent_inbox(by) {
            match reply {
                IntentReply::Granted(Intent::TakeFromPile { cell, item, count, .. }) => {
                    let stack = ItemStack { item, count };
                    picks.push(PickupView { cell: local_cell(rc, cell), entity: key, stacks: vec![stack], tick });
                    back.push(stack);
                }
                IntentReply::Refused(Intent::DepositItems { stack, .. }, why) => {
                    log::debug!("[item_piles] deposit of {stack:?} refused ({why:?}): back to {by:?}");
                    back.push(stack);
                }
                _ => {}
            }
        }
        for stack in back {
            let left = self.data.clients.get_mut(&client).map_or(stack.count, |c| c.inventory.add(stack));
            if left > 0 {
                self.drop_at_feet_safe(client, ItemStack { count: left, ..stack });
            }
        }
        if !picks.is_empty() {
            self.data.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::ItemsPickedUp(picks)));
        }
    }

    /// A reply to this region itself (a region-sourced intent): a deposit
    /// refused (the neighbour's piles full, or it not running) is put down
    /// in this region's own cell nearest its target — own cells only, so it
    /// never goes back out; what still does not fit is destroyed (counted).
    pub(crate) fn region_reply_safe(&mut self, reply: IntentReply) {
        let IntentReply::Refused(Intent::DepositItems { cell, stack, expire, pickup, .. }, why) = reply else { return };
        let n = REGION_BLOCKS as i32;
        let local = local_cell(*self.data.region, cell);
        let at = world_cell(*self.data.region, [local[0].clamp(0, n - 1), local[1], local[2].clamp(0, n - 1)]);
        log::debug!("[item_piles] region deposit of {stack:?} at {cell:?} refused ({why:?}): put down at {at:?}");
        let mut ov = PileOverlay::new(self);
        let overflow = plan_deposit(self, &mut ov, at, stack, expire, pickup, false);
        let (changes, evicted) = self.capped(ov);
        self.write_piles_safe(changes);
        record_destroyed(overflow, at, "refused deposit overflow");
        record_destroyed(evicted, at, "pile cap eviction");
    }

    /// Removes every pile whose expiry tick is `tick` or earlier.
    pub fn expire_piles_safe(&mut self, tick: u64) {
        let due: Vec<(WorldBlockPos, Option<ItemPile>)> = self
            .data
            .pile_expiry
            .keys()
            .take_while(|(t, _)| *t <= tick)
            .map(|&(_, p)| (p, None))
            .collect();
        self.write_piles_safe(due);
    }

    /// `ov`'s own-cell changes, plus the evictions that keep the region at
    /// [`PILES_PER_REGION_MAX`]: the earliest-expiring piles this edit did
    /// not touch — and the evicted piles' item count.
    fn capped(&self, mut ov: PileOverlay<'_>) -> (Vec<(WorldBlockPos, Option<ItemPile>)>, u32) {
        let mut evicted = 0;
        let mut over = ov.len().saturating_sub(PILES_PER_REGION_MAX);
        if over > 0 {
            let mut evict = Vec::new();
            for &(_, p) in self.data.pile_expiry.keys() {
                if over == 0 {
                    break;
                }
                if !ov.touched.contains_key(&p) {
                    evict.push(p);
                    over -= 1;
                }
            }
            for p in evict {
                evicted += ov.slot(p).take().map_or(0, |pile| pile.count());
            }
        }
        (ov.into_changes(), evicted)
    }

    /// The one write funnel of own piles: sets or clears each cell's pile and
    /// its expiry index entry, and tells the renderer — the new views now,
    /// the prior ones on undo — and the halo, which mirrors the band's.
    pub(crate) fn write_piles_safe(&mut self, changes: Vec<(WorldBlockPos, Option<ItemPile>)>) {
        if changes.is_empty() {
            return;
        }
        let rc = *self.data.region;
        let prior = changes
            .iter()
            .map(|(p, _)| (local_cell(rc, *p), self.data.item_piles.get(p).map(ItemPile::view)))
            .collect();
        // LIFO: registered first, so the prior views are re-sent after the
        // maps are restored.
        self.data.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetItemPiles(prior),
        ));
        let mut views = Vec::with_capacity(changes.len());
        for (p, new) in changes {
            debug_assert_eq!(p.to_local().0, rc, "a region writes only its own piles");
            TOUCHED_PILES.with(|t| t.borrow_mut().push(p));
            if let Some(old) = self.data.item_piles.remove(&p) {
                self.data.pile_expiry.remove(&(old.expire_tick, p));
            }
            views.push((local_cell(rc, p), new.as_ref().map(ItemPile::view)));
            if let Some(new) = new {
                self.data.pile_expiry.insert((new.expire_tick, p), ());
                self.data.item_piles.insert(p, new);
            }
        }
        self.data.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetItemPiles(views)));
    }

    /// The halo mirrors' write funnel (`halo.rs`): sets or clears each
    /// neighbour cell's mirror and tells the renderer both ways.
    pub(crate) fn write_halo_piles_safe(&mut self, changes: Vec<(WorldBlockPos, Option<ItemPile>)>) {
        let changes: Vec<_> =
            changes.into_iter().filter(|(p, new)| self.data.halo_piles.get(p) != new.as_ref()).collect();
        if changes.is_empty() {
            return;
        }
        let rc = *self.data.region;
        let prior = changes
            .iter()
            .map(|(p, _)| (local_cell(rc, *p), self.data.halo_piles.get(p).map(ItemPile::view)))
            .collect();
        self.data.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetHaloPiles(prior),
        ));
        let mut views = Vec::with_capacity(changes.len());
        for (p, new) in changes {
            views.push((local_cell(rc, p), new.as_ref().map(ItemPile::view)));
            match new {
                Some(new) => {
                    self.data.halo_piles.insert(p, new);
                }
                None => {
                    self.data.halo_piles.remove(&p);
                }
            }
        }
        self.data.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetHaloPiles(views)));
    }

    /// Drops every halo pile mirror of region `from` (its source is gone).
    pub(crate) fn drop_halo_piles_of_safe(&mut self, from: RegionCoords) {
        let gone: Vec<_> =
            self.data.halo_piles.keys().filter(|p| p.to_local().0 == from).map(|p| (*p, None)).collect();
        self.write_halo_piles_safe(gone);
    }

    /// This region's piles that neighbour `to` mirrors (`HaloUpdate::piles`
    /// of a seed), in cell order.
    pub(crate) fn pile_seed_for(&self, to: RegionCoords) -> Vec<(WorldBlockPos, Option<ItemPile>)> {
        let rc = *self.data.region;
        let offset = (to.x - rc.x, to.z - rc.z);
        self.data
            .item_piles
            .iter()
            .filter(|(p, _)| {
                let l = local_cell(rc, **p);
                pile_halo_targets(l[0], l[2]).contains(&offset)
            })
            .map(|(p, pile)| (*p, Some(pile.clone())))
            .collect()
    }
}

/// Puts what fits of `stack` into the pile of `c` itself (a deposit
/// intent's cell, at its owner): same-item stacks topped up, then new
/// stacks while the pile has a free slot. Returns the count left over.
fn plan_into_cell(ov: &mut PileOverlay<'_>, c: WorldBlockPos, mut stack: ItemStack, expire: u64, pickup: u64) -> u32 {
    let had = ov.get(c).cloned();
    let pile = ov.slot(c).get_or_insert_with(|| ItemPile { stacks: Vec::new(), expire_tick: expire, pickup_after_tick: pickup });
    let before = stack.count;
    for s in pile.stacks.iter_mut().filter(|s| s.item == stack.item) {
        let n = (MAX_STACK - s.count).min(stack.count);
        s.count += n;
        stack.count -= n;
    }
    while stack.count > 0 && pile.stacks.len() < PILE_MAX_STACKS {
        let n = stack.count.min(MAX_STACK);
        pile.stacks.push(ItemStack { item: stack.item, count: n });
        stack.count -= n;
    }
    if stack.count == before {
        // Nothing fitted: the cell keeps what it had.
        *ov.slot(c) = had;
    } else {
        pile.expire_tick = pile.expire_tick.max(expire);
        pile.pickup_after_tick = pile.pickup_after_tick.max(pickup);
    }
    stack.count
}

/// Plans one deposit into `ov` (see [`Rollback::deposit_items_safe`]); the
/// stacks it adds carry `expire` / `pickup`, and a pile they join keeps the
/// later of each. With `remote`, a neighbour's cell seen in the halo is a
/// candidate too (its share is in `ov.remote`, for an intent); a rest cell
/// past the halo takes the whole stack that way. Without, only own cells
/// are. Returns the count left over (destroyed, or refused).
fn plan_deposit(
    r: &Rollback,
    ov: &mut PileOverlay<'_>,
    at: WorldBlockPos,
    mut stack: ItemStack,
    expire: u64,
    pickup: u64,
    remote: bool,
) -> u32 {
    if stack.count == 0 {
        return 0;
    }
    let rest = r.settle_cell(at);
    let usable = |ov: &PileOverlay<'_>, c: WorldBlockPos| ov.owns(c) || (remote && r.pile_cell_seen(c));
    if !usable(ov, rest) {
        if remote && !ov.owns(rest) {
            ov.added(rest, stack.item, stack.count);
            return 0;
        }
        return stack.count;
    }
    let near = cells_near(rest);
    let join = |p: &mut ItemPile| {
        p.expire_tick = p.expire_tick.max(expire);
        p.pickup_after_tick = p.pickup_after_tick.max(pickup);
    };
    // Top up same-item stacks nearby.
    for &c in &near {
        if stack.count == 0 {
            break;
        }
        if !usable(ov, c) {
            continue;
        }
        let room = ov.get(c).is_some_and(|p| p.stacks.iter().any(|s| s.item == stack.item && s.count < MAX_STACK));
        if !room {
            continue;
        }
        let pile = ov.slot(c).as_mut().expect("checked: a pile");
        let mut added = 0;
        for s in pile.stacks.iter_mut().filter(|s| s.item == stack.item) {
            let n = (MAX_STACK - s.count).min(stack.count);
            s.count += n;
            stack.count -= n;
            added += n;
        }
        join(pile);
        ov.added(c, stack.item, added);
    }
    // New stacks: the rest cell first (`near[0]`), then the nearest cell that
    // rests where it is and has a free stack slot.
    for &c in &near {
        if stack.count == 0 {
            break;
        }
        if !usable(ov, c) {
            continue;
        }
        if ov.get(c).is_some_and(|p| p.stacks.len() >= PILE_MAX_STACKS) {
            continue;
        }
        if c != rest && r.settle_cell(c) != c {
            continue;
        }
        let pile = ov.slot(c).get_or_insert_with(|| ItemPile {
            stacks: Vec::new(),
            expire_tick: expire,
            pickup_after_tick: pickup,
        });
        join(pile);
        let mut added = 0;
        while stack.count > 0 && pile.stacks.len() < PILE_MAX_STACKS {
            let n = stack.count.min(MAX_STACK);
            pile.stacks.push(ItemStack { item: stack.item, count: n });
            stack.count -= n;
            added += n;
        }
        ov.added(c, stack.item, added);
    }
    stack.count
}
