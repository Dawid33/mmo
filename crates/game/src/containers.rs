//! Block inventories (plan 2026-11-05, Decision 10): chests. Across region
//! seams: plan 2026-11-06 Task 6.4.
//!
//! Like Minecraft's block entities and Luanti's node inventories, a chest's
//! contents are not in the voxel array: they live in
//! `GameData::containers`, a `BTreeMap<WorldBlockPos, Container>` keyed by
//! the block's WORLD cell and held by the region that owns the cell.
//!
//! - Every block write runs [`Rollback::containers_after_write_safe`]
//!   (`edit.rs` hooks both the single write and the batched fill): a chest
//!   written where there is no container creates an empty one; a container
//!   whose block is no longer a chest spills ([`Rollback::spill_container_safe`])
//!   into the owner's piles. A break across a seam is an intent to the
//!   owner, so the spill always happens there, once.
//! - An Adventure player's right click on a chest (its own region's, or a
//!   neighbour's seen through the halo) within `edit::REACH` opens it
//!   ([`Rollback::open_container_safe`]: `Client::open_container` plus the
//!   inventory screen) instead of placing a block.
//! - It closes ([`Rollback::close_container_safe`]) when the inventory is
//!   closed, when the player's feet tile is farther than
//!   [`CONTAINER_REACH_BLOCKS`] from the chest in WORLD blocks, or when the
//!   viewer no longer sees the chest — own or mirrored
//!   ([`Rollback::close_far_containers_safe`], every tick). A spill closes it
//!   at once for the owner's viewers; a neighbour's viewers close when the
//!   halo delta that removes the mirror reaches them (or the mirror's source
//!   goes away). The open chest travels with its viewer across a seam
//!   (`Client::arrive` carries it), so it stays open while in reach.
//! - **Clicks on a chest are intents**, the same whether the chest is the
//!   viewer's region's or a neighbour's ([`Rollback::container_click_safe`]):
//!   a stack leaving the chest is an [`Intent::TakeFromContainer`] — the
//!   viewer's inventory changes only when the owner grants it
//!   ([`Rollback::settle_container_replies_safe`]); a stack going in is an
//!   [`Intent::PutInContainer`] — the viewer gives it up at once, and what
//!   the owner cannot fit comes back (`Refused(.., ContainerFull)` carrying
//!   the rest). Both apply at the owner at the opening of the next tick, in
//!   `(source EntityId, seq)` order, so two takers of one slot in one tick:
//!   the lower id gets it, the other is refused. What the click reads (the
//!   slot it takes from) is the viewer's view: its own region's chest, or
//!   the halo mirror.
//! - **Halo**: chests within `HALO_BLOCKS` of an edge are mirrored into the
//!   neighbours' `GameData::halo_containers`, read-only, on the pile mirror's
//!   channel (`HaloUpdate::containers`; the pile band and its dirty cells —
//!   `item_piles::pile_halo_targets`). The client's chest screen reads the
//!   open chest through [`container_view`], own or mirrored.
//!
//! There is no render update to emit: chests are blocks, and the screen
//! reads the predicted `GameData`.

use crate::edit::BlockPos;
use crate::intents::{Effects as _, RegionEffects};
use crate::{
    block_props, BlockId, ClickButton, ClientId, GameData, Intent, IntentMsg, IntentRefusal, IntentReply, ItemStack,
    Rollback, SlotRef, Target, WorldBlockPos, MAX_STACK, PICKUP_DELAY_TICKS,
};

/// Slots of a chest (Minecraft's single chest).
pub const CHEST_SLOTS: usize = 27;
/// How far (blocks, Euclidean, from the player's feet tile to the chest
/// cell, in world blocks) a player may be with a chest open; farther closes
/// it.
pub const CONTAINER_REACH_BLOCKS: i32 = 6;
/// Chests in a new player's starting kit (slot 0): there is no crafting.
pub const STARTING_CHESTS: u32 = 2;
/// The block a chest is, by manifest name.
pub const CHEST_BLOCK_NAME: &str = "chest";

/// One block's inventory.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, state_hash::StateHash)]
pub struct Container {
    pub slots: Vec<Option<ItemStack>>,
}

impl Default for Container {
    fn default() -> Self {
        Self::chest()
    }
}

impl Container {
    /// An empty chest.
    pub fn chest() -> Self {
        Self { slots: vec![None; CHEST_SLOTS] }
    }
}

/// What the client's chest screen shows: the open container's cell and
/// slots (client-side projection, never hashed or sent).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContainerView {
    pub pos: WorldBlockPos,
    pub slots: Vec<Option<ItemStack>>,
}

/// The chest block's id (`None` while the manifest has no chest).
pub fn chest_block() -> Option<BlockId> {
    block_props().id_named(CHEST_BLOCK_NAME)
}

/// The container `client` has open, as its screen shows it: its region's
/// own chest, or a neighbour's halo mirror.
pub fn container_view(data: &GameData, client: ClientId) -> Option<ContainerView> {
    let pos = data.clients.get(&client)?.open_container?;
    let c = data.container_seen(pos)?;
    Some(ContainerView { pos, slots: c.slots.clone() })
}

impl GameData {
    /// The chest in world cell `p` as this region sees it: its own, or the
    /// halo's read-only mirror of a neighbour's; `None` elsewhere.
    pub fn container_seen(&self, p: WorldBlockPos) -> Option<&Container> {
        if p.to_local().0 == *self.region { self.containers.get(&p) } else { self.halo_containers.get(&p) }
    }
}

/// What one chest click sends: `(take, put)` — a take first (a swap takes
/// the slot's stack, then puts the cursor's in).
type ClickIntents = (Option<(u8, ItemStack, bool)>, Option<(Option<u8>, ItemStack)>);

/// The chest half of a Minecraft click on chest slot `i` holding `t`, with
/// `cursor` held: what is taken (`(slot, stack, to_cursor)`) and put
/// (`(slot, stack)`); `cursor` keeps what the put leaves on it.
fn chest_slot_click(cursor: &mut Option<ItemStack>, i: u8, t: Option<ItemStack>, button: ClickButton) -> ClickIntents {
    let swap = |cursor: &mut Option<ItemStack>, c: ItemStack, t: ItemStack| {
        *cursor = None;
        (Some((i, t, true)), Some((Some(i), c)))
    };
    match (button, cursor.take(), t) {
        (_, None, None) => (None, None),
        (ClickButton::Left, None, Some(t)) => (Some((i, t, true)), None),
        (ClickButton::Right, None, Some(t)) => {
            (Some((i, ItemStack { count: t.count.div_ceil(2), ..t }, true)), None)
        }
        (ClickButton::Left, Some(c), None) => (None, Some((Some(i), c))),
        (ClickButton::Right, Some(mut c), None) => {
            c.count -= 1;
            *cursor = (c.count > 0).then_some(c);
            (None, Some((Some(i), ItemStack { count: 1, ..c })))
        }
        (button, Some(mut c), Some(t)) if c.item == t.item => {
            let room = MAX_STACK.saturating_sub(t.count);
            let n = match button {
                ClickButton::Left => room.min(c.count),
                ClickButton::Right => room.min(1),
            };
            c.count -= n;
            *cursor = Some(c).filter(|c| c.count > 0);
            (None, (n > 0).then_some((Some(i), ItemStack { count: n, ..c })))
        }
        (_, Some(c), Some(t)) => swap(cursor, c, t),
    }
}

/// Puts `stack` into chest slots: into slot `i` only (empty, or the same
/// item up to `MAX_STACK`), or with `None` anywhere as a shift-click does
/// (topping up, then empty slots). Returns the count that did not fit.
fn put_into(slots: &mut [Option<ItemStack>], slot: Option<u8>, stack: ItemStack) -> u32 {
    let Some(i) = slot else {
        let mut inv = crate::Inventory { slots: slots.to_vec() };
        let left = inv.add(stack);
        slots.clone_from_slice(&inv.slots);
        return left;
    };
    let Some(s) = slots.get_mut(i as usize) else { return stack.count };
    match s {
        None => {
            let n = stack.count.min(MAX_STACK);
            *s = Some(ItemStack { count: n, ..stack });
            stack.count - n
        }
        Some(t) if t.item == stack.item => {
            let n = MAX_STACK.saturating_sub(t.count).min(stack.count);
            t.count += n;
            stack.count - n
        }
        Some(_) => stack.count,
    }
}

impl Rollback {
    /// Whether local `pos` holds a chest block.
    fn is_chest_at(&self, pos: BlockPos) -> bool {
        chest_block().is_some_and(|chest| self.block_at(pos).is_some_and(|b| b.id == chest))
    }

    /// Whether `client`'s feet tile is within [`CONTAINER_REACH_BLOCKS`] of
    /// world cell `p` (no body: no). Measured in world blocks, so a chest
    /// across a seam is as near as one at home.
    fn in_container_reach(&self, client: ClientId, p: WorldBlockPos) -> bool {
        let Some(&e) = self.data.player_entites.get(&client) else { return false };
        let Some(feet) = self.body_feet(e) else { return false };
        let t = crate::feet_tile(feet);
        let t = WorldBlockPos::from_local(*self.data.region, crate::BlockPos::new(t[0], t[1], t[2]));
        let d2 = (t.x - p.x).pow(2) + i64::from(t.y - p.y).pow(2) + (t.z - p.z).pow(2);
        d2 <= i64::from(CONTAINER_REACH_BLOCKS).pow(2)
    }

    /// Opens the chest at world cell `p` (seen: own or mirrored) for
    /// `client`, and its inventory screen with it. Refused (`false`) with no
    /// chest seen there, or out of reach.
    pub fn open_container_safe(&mut self, client: ClientId, p: WorldBlockPos) -> bool {
        if self.data.container_seen(p).is_none() || !self.in_container_reach(client, p) {
            return false;
        }
        let Some(c) = self.data.clients.get(&client) else { return false };
        if c.open_container == Some(p) && c.inventory_open {
            return true;
        }
        if let Some(c) = self.data.clients.get_mut(&client) {
            c.open_container = Some(p);
            c.inventory_open = true;
        }
        true
    }

    /// Closes `client`'s open container and its inventory screen, as closing
    /// the inventory does: the cursor stack goes back to the inventory and
    /// what does not fit is dropped at the player's feet. Nothing open:
    /// nothing changes.
    pub fn close_container_safe(&mut self, client: ClientId) {
        if self.data.clients.get(&client).is_none_or(|c| c.open_container.is_none()) {
            return;
        }
        let rest = self.data.clients.get_mut(&client).and_then(|c| {
            c.open_container = None;
            if !c.inventory_open {
                return None;
            }
            crate::region::toggle_inventory(c)
        });
        if let Some(rest) = rest {
            self.drop_at_feet_safe(client, rest);
        }
    }

    /// Closes every open container whose player walked out of reach or
    /// that this region no longer sees (gone, or its mirror dropped).
    /// Clients in id order.
    pub fn close_far_containers_safe(&mut self) {
        let open: Vec<(ClientId, WorldBlockPos)> =
            self.data.clients.iter().filter_map(|(id, c)| Some((*id, c.open_container?))).collect();
        for (client, p) in open {
            if self.data.container_seen(p).is_none() || !self.in_container_reach(client, p) {
                self.close_container_safe(client);
            }
        }
    }

    /// One inventory click by `client` while its chest at `open` is open:
    /// a click on one of its own slots is applied at once; one that moves a
    /// stack out of or into the chest sends a take / put intent to the
    /// chest's owner (into `out` when that is a neighbour). The put's stack
    /// leaves the cursor (or the shift-clicked slot) now; the take's arrives
    /// when granted.
    pub(crate) fn container_click_safe(
        &mut self,
        client: ClientId,
        slot: SlotRef,
        button: ClickButton,
        shift: bool,
        out: &mut Vec<IntentMsg>,
    ) {
        let Some(c) = self.data.clients.get(&client) else { return };
        let Some(cell) = c.open_container else { return };
        let Some(view) = self.data.container_seen(cell).map(|k| k.slots.clone()) else { return };
        let Some(by) = self.client_body_id(client) else { return };
        let (take, put) = match slot {
            SlotRef::Player(i) if shift => {
                let Some(c) = self.data.clients.get_mut(&client) else { return };
                let Some(stack) = c.inventory.slots.get_mut(i as usize).and_then(Option::take) else { return };
                (None, Some((None, stack)))
            }
            SlotRef::Player(_) => {
                if let Some(c) = self.data.clients.get_mut(&client) {
                    let crate::Client { cursor, inventory, .. } = c;
                    crate::click(cursor, inventory, None, slot, button, false);
                }
                return;
            }
            SlotRef::Container(i) => {
                let Some(&t) = view.get(i as usize) else { return };
                if shift {
                    // What the inventory has room for (the rest stays).
                    let take = t.and_then(|t| {
                        let mut room = c.inventory.clone();
                        let n = t.count - room.add(t);
                        (n > 0).then_some((i, ItemStack { count: n, ..t }, false))
                    });
                    (take, None)
                } else {
                    let Some(c) = self.data.clients.get_mut(&client) else { return };
                    chest_slot_click(&mut c.cursor, i, t, button)
                }
            }
        };
        let mut fx = RegionEffects { data: self, out };
        if let Some((slot, stack, to_cursor)) = take {
            let intent =
                Intent::TakeFromContainer { cell, slot, item: stack.item, count: stack.count, to_cursor, by };
            fx.send(Target::Container(cell), intent);
        }
        if let Some((slot, stack)) = put {
            fx.send(Target::Container(cell), Intent::PutInContainer { cell, stack, slot, by });
        }
    }

    /// The owner applies a chest intent at its own cell `cell`
    /// (`intents.rs`). A take takes what the slot holds of the item, up to
    /// the count asked (refused `NothingThere` when none); a put puts what
    /// fits and refuses the rest back (`ContainerFull`, carrying it), or all
    /// of it when there is no chest (`NothingThere`).
    pub(crate) fn apply_container_intent_safe(&mut self, cell: WorldBlockPos, intent: Intent) -> IntentReply {
        let refused = |why| IntentReply::Refused(intent, why);
        let Some(mut chest) = self.data.containers.get(&cell).cloned() else {
            return refused(IntentRefusal::NothingThere);
        };
        match intent {
            Intent::TakeFromContainer { slot, item, count, .. } => {
                let Some(Some(s)) = chest.slots.get_mut(slot as usize) else {
                    return refused(IntentRefusal::NothingThere);
                };
                if s.item != item {
                    return refused(IntentRefusal::NothingThere);
                }
                let n = s.count.min(count);
                s.count -= n;
                if s.count == 0 {
                    chest.slots[slot as usize] = None;
                }
                if n == 0 {
                    return refused(IntentRefusal::NothingThere);
                }
                self.write_container_safe(cell, Some(chest));
                IntentReply::Granted(match intent {
                    Intent::TakeFromContainer { cell, slot, item, to_cursor, by, .. } => {
                        Intent::TakeFromContainer { cell, slot, item, count: n, to_cursor, by }
                    }
                    other => other,
                })
            }
            Intent::PutInContainer { cell, stack, slot, by } => {
                let left = put_into(&mut chest.slots, slot, stack);
                if left < stack.count {
                    self.write_container_safe(cell, Some(chest));
                }
                if left == 0 {
                    IntentReply::Granted(intent)
                } else {
                    let rest = Intent::PutInContainer { cell, stack: ItemStack { count: left, ..stack }, slot, by };
                    IntentReply::Refused(rest, IntentRefusal::ContainerFull)
                }
            }
            other => IntentReply::Refused(other, IntentRefusal::BadTarget),
        }
    }

    /// `client`'s body's chest replies this tick: a granted take's items go
    /// onto the cursor (`to_cursor`, while the screen is open and the cursor
    /// is empty or holds the same item) or into the inventory; a refused
    /// put's rest goes back the same way. What does not fit goes down at
    /// the feet.
    pub(crate) fn settle_container_replies_safe(&mut self, client: ClientId) {
        let Some(by) = self.client_body_id(client) else { return };
        let mut back = Vec::new();
        for reply in self.intent_inbox(by) {
            match reply {
                IntentReply::Granted(Intent::TakeFromContainer { item, count, to_cursor, .. }) => {
                    back.push((ItemStack { item, count }, to_cursor));
                }
                IntentReply::Refused(Intent::PutInContainer { stack, slot, .. }, why) => {
                    log::debug!("[containers] put of {stack:?} refused ({why:?}): back to {by:?}");
                    back.push((stack, slot.is_some()));
                }
                _ => {}
            }
        }
        for (mut stack, to_cursor) in back {
            if let Some(c) = self.data.clients.get_mut(&client) {
                if to_cursor && c.inventory_open {
                    match &mut c.cursor {
                        None => {
                            c.cursor = Some(stack);
                            stack.count = 0;
                        }
                        Some(held) if held.item == stack.item => {
                            let n = MAX_STACK.saturating_sub(held.count).min(stack.count);
                            held.count += n;
                            stack.count -= n;
                        }
                        Some(_) => {}
                    }
                }
                if stack.count > 0 {
                    stack.count = c.inventory.add(stack);
                }
            }
            if stack.count > 0 {
                self.drop_at_feet_safe(client, stack);
            }
        }
    }

    /// The one write funnel of own chests: sets (`Some`) or removes the
    /// container at world cell `p` — this region's — and marks the cell for
    /// the neighbours' halo mirrors.
    pub fn write_container_safe(&mut self, p: WorldBlockPos, c: Option<crate::Container>) {
        debug_assert_eq!(p.to_local().0, *self.data.region, "a region writes only its own chests");
        if self.data.containers.get(&p) == c.as_ref() {
            return;
        }
        crate::item_piles::touch_cell(p);
        match c {
            Some(c) => {
                self.data.containers.insert(p, c);
            }
            None => {
                self.data.containers.remove(&p);
            }
        }
    }

    /// The halo chest mirrors' write funnel (`halo.rs`): sets or clears each
    /// neighbour cell's mirror.
    pub(crate) fn write_halo_containers_safe(&mut self, changes: Vec<(WorldBlockPos, Option<Container>)>) {
        for (p, new) in changes {
            if self.data.halo_containers.get(&p) == new.as_ref() {
                continue;
            }
            match new {
                Some(new) => {
                    self.data.halo_containers.insert(p, new);
                }
                None => {
                    self.data.halo_containers.remove(&p);
                }
            }
        }
    }

    /// Drops every halo chest mirror of region `from` (its source is gone).
    pub(crate) fn drop_halo_containers_of_safe(&mut self, from: crate::RegionCoords) {
        let gone: Vec<_> =
            self.data.halo_containers.keys().filter(|p| p.to_local().0 == from).map(|p| (*p, None)).collect();
        self.write_halo_containers_safe(gone);
    }

    /// This region's chests that neighbour `to` mirrors (`HaloUpdate::
    /// containers` of a seed), in cell order.
    pub(crate) fn container_seed_for(&self, to: crate::RegionCoords) -> Vec<(WorldBlockPos, Option<Container>)> {
        let rc = *self.data.region;
        let offset = (to.x - rc.x, to.z - rc.z);
        self.data
            .containers
            .iter()
            .filter(|(p, _)| {
                let l = crate::item_piles::local_cell(rc, **p);
                crate::item_piles::pile_halo_targets(l[0], l[2]).contains(&offset)
            })
            .map(|(p, c)| (*p, Some(c.clone())))
            .collect()
    }

    /// Removes the container at world cell `pos` (this region's), closes it
    /// for every client here that has it open (a neighbour's viewers close
    /// when the halo delta reaches them), and drops its stacks as piles at
    /// its cell (merging, as any deposit), pickable after
    /// [`PICKUP_DELAY_TICKS`] from `tick`. Returns the items the pile caps
    /// destroyed (counted where it happens).
    pub fn spill_container_safe(&mut self, pos: WorldBlockPos, tick: u64) -> u32 {
        let Some(container) = self.data.containers.get(&pos).cloned() else { return 0 };
        self.write_container_safe(pos, None);
        let viewers: Vec<ClientId> = self
            .data
            .clients
            .iter()
            .filter(|(_, c)| c.open_container == Some(pos))
            .map(|(id, _)| *id)
            .collect();
        for client in viewers {
            self.close_container_safe(client);
        }
        let local = crate::item_piles::local_cell(*self.data.region, pos);
        let mut destroyed = 0;
        for stack in container.slots.into_iter().flatten() {
            destroyed += self.deposit_items_safe(local, stack, tick, PICKUP_DELAY_TICKS);
        }
        destroyed
    }

    /// After local block `pos` was written: a new chest gets an empty
    /// container, a container whose chest went spills. Called by every
    /// terrain write in `edit.rs`. Returns the items destroyed (already
    /// counted).
    pub(crate) fn containers_after_write_safe(&mut self, pos: BlockPos) -> u32 {
        let w = WorldBlockPos::from_local(*self.data.region, crate::BlockPos::new(pos[0], pos[1], pos[2]));
        let has = self.data.containers.contains_key(&w);
        let chest = self.is_chest_at(pos);
        if chest && !has {
            self.write_container_safe(w, Some(Container::chest()));
        } else if !chest && has {
            let tick = *self.data.tick;
            return self.spill_container_safe(w, tick);
        }
        0
    }
}
