//! Dropped items as cell-keyed piles (plan 2026-11-05, Decisions 1–4).
//!
//! A dropped item is never an entity and never a rapier body: it is a stack
//! in the [`ItemPile`] of the block cell it rests in, kept in
//! `GameData::item_piles` (a `BTreeMap<BlockPos, ItemPile>`, region-local
//! block coordinates). A resting pile costs nothing per tick, and the number
//! of drawable things is bounded by occupied cells.
//!
//! - **Settling** ([`Rollback::settle_cell`]) is an integer column walk:
//!   x / z clamped into the region, up out of solid blocks, then down while
//!   the cell below is not solid. "Solid" is the pathfinder's floor rule over
//!   the block the sim sees (implied ground included): not air, not water,
//!   and solid by the manifest (leaves are not). No floats enter pile state.
//! - **Merge on deposit** ([`Rollback::deposit_items_safe`]): same-item
//!   stacks in piles within [`PILE_MERGE_RADIUS_BLOCKS`] are topped up first
//!   (nearest by Chebyshev distance, ties by cell key), then new stacks go
//!   into the rest cell's pile, then into the nearest cell of the radius that
//!   rests where it is and has a free stack slot; anything left is destroyed
//!   (and counted). A pile holds at most [`PILE_MAX_STACKS`] stacks.
//! - **Support** changes (`edit.rs` hooks every block write): a pile whose
//!   support is broken falls and re-merges where it lands; a block placed
//!   into a pile's cell pushes it up ([`Rollback::resettle_pile_safe`]).
//! - **Lifetime**: [`PILE_DESPAWN_TICKS`], indexed by
//!   `GameData::pile_expiry` so only due piles are touched; a merge keeps the
//!   later expiry and the later pickup tick; at [`PILES_PER_REGION_MAX`] the
//!   earliest-expiring pile is evicted.
//! - **Pickup** ([`Rollback::tick_pickups_safe`]): every Adventure body takes
//!   what fits from the piles in its box grown by [`PICKUP_REACH_H_BLOCKS`] /
//!   [`PICKUP_REACH_V_BLOCKS`], clients in id order, cells in key order.
//!
//! Every write goes through one funnel that emits `SetItemPiles` with the
//! new views (Do) and registers the prior views for undo, like
//! `orders.rs`'s designations.

use std::collections::BTreeMap;

use crate::edit::BlockPos;
use crate::edit::locate;
use crate::movement::{PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS};
use crate::{
    block_props, BlockId, ClientId, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, ItemStack, Rollback,
    BLOCK_VOXELS, MAX_STACK, REGION_BLOCKS,
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
}

/// Items the pile caps have destroyed on this thread so far (see
/// `ITEMS_DESTROYED`); read before and after a unit of work for its share.
pub fn items_destroyed_on_thread() -> u64 {
    ITEMS_DESTROYED.with(|c| c.get())
}

/// The one place pile items are destroyed from: a stack no pile in reach
/// could take (deposit or re-settle overflow) or a pile evicted by
/// [`PILES_PER_REGION_MAX`]. Logs and counts it; returns `n`.
fn record_destroyed(n: u32, at: BlockPos, why: &str) -> u32 {
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
fn cells_near(c: BlockPos) -> Vec<BlockPos> {
    let r = PILE_MERGE_RADIUS_BLOCKS;
    let mut out = Vec::with_capacity(((2 * r + 1) * (2 * r + 1) * (2 * r + 1)) as usize);
    for dx in -r..=r {
        for dy in -r..=r {
            for dz in -r..=r {
                out.push([c[0] + dx, c[1] + dy, c[2] + dz]);
            }
        }
    }
    let cheb = |p: &BlockPos| (p[0] - c[0]).abs().max((p[1] - c[1]).abs()).max((p[2] - c[2]).abs());
    out.sort_by_key(|p| (cheb(p), *p));
    out
}

/// Pile edits planned against the live map before one batched write: the
/// cells touched so far with their new value, reading through to `base`.
struct PileOverlay<'a> {
    base: &'a BTreeMap<BlockPos, ItemPile>,
    touched: BTreeMap<BlockPos, Option<ItemPile>>,
}

impl<'a> PileOverlay<'a> {
    fn new(base: &'a BTreeMap<BlockPos, ItemPile>) -> Self {
        Self { base, touched: BTreeMap::new() }
    }

    fn get(&self, p: BlockPos) -> Option<&ItemPile> {
        match self.touched.get(&p) {
            Some(v) => v.as_ref(),
            None => self.base.get(&p),
        }
    }

    fn slot(&mut self, p: BlockPos) -> &mut Option<ItemPile> {
        let base = self.base;
        self.touched.entry(p).or_insert_with(|| base.get(&p).cloned())
    }

    /// Piles there would be after the edits.
    fn len(&self) -> usize {
        let mut n = self.base.len();
        for (p, v) in &self.touched {
            match (self.base.contains_key(p), v.is_some()) {
                (false, true) => n += 1,
                (true, false) => n -= 1,
                _ => {}
            }
        }
        n
    }

    /// The edits that change something, in cell order.
    fn into_changes(self) -> Vec<(BlockPos, Option<ItemPile>)> {
        let base = self.base;
        self.touched.into_iter().filter(|(p, v)| base.get(p) != v.as_ref()).collect()
    }
}

impl Rollback {
    /// Whether a pile can rest on top of `pos`: the pathfinder's floor rule
    /// (not air, not water, solid by the manifest) over the block the sim
    /// sees there, implied ground included. Outside the region: no.
    pub fn pile_support(&self, pos: BlockPos) -> bool {
        let Some((coords, local)) = locate(pos) else { return false };
        self.current_block(coords, local)
            .is_some_and(|b| b.id != BlockId::AIR && !crate::pathfind::is_water(b.id) && block_props().is_solid(b.id))
    }

    /// Where a pile put at `from` comes to rest (Decision 2): x / z clamped
    /// to the region's own blocks, then up out of solid blocks (at most
    /// [`MAX_PILE_RISE_BLOCKS`]), then down while the cell below is not
    /// solid (at most [`MAX_PILE_FALL_BLOCKS`]).
    pub fn settle_cell(&self, from: BlockPos) -> BlockPos {
        let n = REGION_BLOCKS as i32;
        let mut p = [from[0].clamp(0, n - 1), from[1], from[2].clamp(0, n - 1)];
        let mut rose = 0;
        while rose < MAX_PILE_RISE_BLOCKS && self.pile_support(p) {
            p[1] += 1;
            rose += 1;
        }
        let mut fell = 0;
        while fell < MAX_PILE_FALL_BLOCKS && !self.pile_support([p[0], p[1] - 1, p[2]]) {
            p[1] -= 1;
            fell += 1;
        }
        p
    }

    /// Drops `stack` at `at` (Decision 1): settles, merges into nearby piles,
    /// else starts stacks in the rest cell or a free cell nearby. The new
    /// items despawn at `tick + PILE_DESPAWN_TICKS` and can be picked up from
    /// `tick + pickup_delay`. Returns how many items the caps destroyed —
    /// overflow plus any evicted pile's items (0 normally); every one is
    /// counted (`record_destroyed`). Emits `SetItemPiles` both ways.
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
        let mut ov = PileOverlay::new(&self.data.item_piles);
        let overflow = plan_deposit(self, &mut ov, at, stack, tick + PILE_DESPAWN_TICKS, tick + pickup_delay);
        let (changes, evicted) = self.capped(ov);
        self.write_piles_safe(changes);
        self.send_landing(origin, self.settle_cell(at), vec![stack], tick);
        record_destroyed(overflow, at, "deposit overflow") + record_destroyed(evicted, at, "pile cap eviction")
    }

    /// Tells the renderer items went from `from` to `to` (nothing if they
    /// did not move). Render-only.
    fn send_landing(&self, from: BlockPos, to: BlockPos, stacks: Vec<ItemStack>, tick: u64) {
        if from != to && !stacks.is_empty() {
            let flight = PileFlight { from, to, stacks, tick };
            self.data.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::ItemsLanded(vec![flight])));
        }
    }

    /// Takes the pile at `cell` away and deposits its stacks again from
    /// `cell`, keeping its expiry and pickup tick: it falls if its support
    /// went, or rises out of a block placed into its cell. `tick` is the
    /// current tick (it stamps the render-only `ItemsLanded`; the pile keeps
    /// its own times).
    /// Returns the items destroyed (counted, as for a deposit).
    pub fn resettle_pile_safe(&mut self, cell: BlockPos, tick: u64) -> u32 {
        let mut ov = PileOverlay::new(&self.data.item_piles);
        let Some(pile) = ov.slot(cell).take() else { return 0 };
        let stacks = pile.stacks.clone();
        let mut overflow = 0;
        for s in pile.stacks {
            overflow += plan_deposit(self, &mut ov, cell, s, pile.expire_tick, pile.pickup_after_tick);
        }
        let (changes, evicted) = self.capped(ov);
        self.write_piles_safe(changes);
        self.send_landing(cell, self.settle_cell(cell), stacks, tick);
        record_destroyed(overflow, cell, "re-settle overflow") + record_destroyed(evicted, cell, "pile cap eviction")
    }

    /// After block `pos` was written: a pile pushed up out of it, or the
    /// pile it held up falls. Called by every terrain write in `edit.rs`.
    /// Returns the items destroyed (already counted).
    pub(crate) fn piles_after_write_safe(&mut self, pos: BlockPos) -> u32 {
        if self.data.item_piles.is_empty() {
            return 0;
        }
        let tick = *self.data.tick;
        let cell = if self.pile_support(pos) { pos } else { [pos[0], pos[1] + 1, pos[2]] };
        if self.data.item_piles.contains_key(&cell) {
            self.resettle_pile_safe(cell, tick)
        } else {
            0
        }
    }

    /// Every Adventure client's body picks up the piles in its reach
    /// (Decision 4) whose pickup tick has come: clients in id order, cells in
    /// key order, each pile's stacks in order, as far as the inventory has
    /// room; the rest stays in the pile.
    pub fn tick_pickups_safe(&mut self, tick: u64) {
        if self.data.item_piles.is_empty() {
            return;
        }
        let bv = BLOCK_VOXELS as f32;
        let (r, half) = (PLAYER_CAPSULE_RADIUS, PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS);
        let (h, v) = (PICKUP_REACH_H_BLOCKS, PICKUP_REACH_V_BLOCKS);
        let mut takers: Vec<(ClientId, crate::EntityKey, BlockPos, BlockPos)> = Vec::new();
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
            takers.push((*id, e, lo, hi));
        }
        if takers.is_empty() {
            return;
        }
        let mut ov = PileOverlay::new(&self.data.item_piles);
        let mut gains = Vec::new();
        let mut picks = Vec::new();
        for (id, entity, lo, hi) in takers {
            let mut inv = self.data.clients[&id].inventory.clone();
            let mut took = false;
            for x in lo[0]..=hi[0] {
                for y in lo[1]..=hi[1] {
                    for z in lo[2]..=hi[2] {
                        let c = [x, y, z];
                        let Some(pile) = ov.get(c) else { continue };
                        if pile.pickup_after_tick > tick {
                            continue;
                        }
                        let mut left = Vec::with_capacity(pile.stacks.len());
                        let mut taken = Vec::new();
                        let mut moved = false;
                        for &s in &pile.stacks {
                            let rest = inv.add(s);
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
                        picks.push(PickupView { cell: c, entity, stacks: taken, tick });
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
        if !picks.is_empty() {
            self.data.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::ItemsPickedUp(picks)));
        }
    }

    /// Removes every pile whose expiry tick is `tick` or earlier.
    pub fn expire_piles_safe(&mut self, tick: u64) {
        let due: Vec<(BlockPos, Option<ItemPile>)> = self
            .data
            .pile_expiry
            .keys()
            .take_while(|(t, _)| *t <= tick)
            .map(|&(_, p)| (p, None))
            .collect();
        self.write_piles_safe(due);
    }

    /// `ov`'s changes, plus the evictions that keep the region at
    /// [`PILES_PER_REGION_MAX`]: the earliest-expiring piles this edit did
    /// not touch — and the evicted piles' item count.
    fn capped(&self, mut ov: PileOverlay<'_>) -> (Vec<(BlockPos, Option<ItemPile>)>, u32) {
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

    /// The one write funnel: sets or clears each cell's pile and its expiry
    /// index entry, and tells the renderer — the new views now, the prior
    /// ones on undo.
    pub(crate) fn write_piles_safe(&mut self, changes: Vec<(BlockPos, Option<ItemPile>)>) {
        if changes.is_empty() {
            return;
        }
        let prior = changes.iter().map(|(p, _)| (*p, self.data.item_piles.get(p).map(ItemPile::view))).collect();
        // LIFO: registered first, so the prior views are re-sent after the
        // maps are restored.
        self.data.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetItemPiles(prior),
        ));
        let mut views = Vec::with_capacity(changes.len());
        for (p, new) in changes {
            if let Some(old) = self.data.item_piles.remove(&p) {
                self.data.pile_expiry.remove(&(old.expire_tick, p));
            }
            views.push((p, new.as_ref().map(ItemPile::view)));
            if let Some(new) = new {
                self.data.pile_expiry.insert((new.expire_tick, p), ());
                self.data.item_piles.insert(p, new);
            }
        }
        self.data.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetItemPiles(views)));
    }
}

/// Plans one deposit into `ov` (see [`Rollback::deposit_items_safe`]); the
/// stacks it adds carry `expire` / `pickup`, and a pile they join keeps the
/// later of each. Returns the count left over (destroyed).
fn plan_deposit(
    r: &Rollback,
    ov: &mut PileOverlay<'_>,
    at: BlockPos,
    mut stack: ItemStack,
    expire: u64,
    pickup: u64,
) -> u32 {
    if stack.count == 0 {
        return 0;
    }
    let rest = r.settle_cell(at);
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
        let room = ov.get(c).is_some_and(|p| p.stacks.iter().any(|s| s.item == stack.item && s.count < MAX_STACK));
        if !room {
            continue;
        }
        let pile = ov.slot(c).as_mut().expect("checked: a pile");
        for s in pile.stacks.iter_mut().filter(|s| s.item == stack.item) {
            let n = (MAX_STACK - s.count).min(stack.count);
            s.count += n;
            stack.count -= n;
        }
        join(pile);
    }
    // New stacks: the rest cell first (`near[0]`), then the nearest cell that
    // rests where it is and has a free stack slot.
    for &c in &near {
        if stack.count == 0 {
            break;
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
        while stack.count > 0 && pile.stacks.len() < PILE_MAX_STACKS {
            let n = stack.count.min(MAX_STACK);
            pile.stacks.push(ItemStack { item: stack.item, count: n });
            stack.count -= n;
        }
    }
    stack.count
}
