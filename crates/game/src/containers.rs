//! Block inventories (plan 2026-11-05, Decision 10): chests.
//!
//! Like Minecraft's block entities and Luanti's node inventories, a chest's
//! contents are not in the voxel array: they live in
//! `GameData::containers`, a `BTreeMap<BlockPos, Container>` keyed by the
//! block's region-local cell, beside the blocks.
//!
//! - Every block write runs [`Rollback::containers_after_write_safe`]
//!   (`edit.rs` hooks both the single write and the batched fill): a chest
//!   written where there is no container creates an empty one; a container
//!   whose block is no longer a chest spills ([`Rollback::spill_container_safe`]).
//! - An Adventure player's right click on a chest opens it
//!   ([`Rollback::open_container_safe`]: `Client::open_container` plus the
//!   inventory screen) instead of placing a block. Inventory clicks then
//!   reach the container's slots (`SlotRef::Container`).
//! - It closes ([`Rollback::close_container_safe`]) when the inventory is
//!   closed, the player is farther than [`CONTAINER_REACH_BLOCKS`]
//!   ([`Rollback::close_far_containers_safe`], every tick), or the chest
//!   goes: a spill closes it for every client that had it open, so two
//!   clients breaking-and-viewing one chest see one spill.
//!
//! The state is shared: every client with the chest open sees the same
//! slots. The client reads it from its predicted `GameData` as a
//! [`ContainerView`] (`container_view`); there is no render update to emit.

use crate::edit::BlockPos;
use crate::{block_props, BlockId, ClientId, GameData, ItemStack, Rollback, PICKUP_DELAY_TICKS};

/// Slots of a chest (Minecraft's single chest).
pub const CHEST_SLOTS: usize = 27;
/// How far (blocks, Euclidean, from the player's feet tile to the chest
/// cell) a player may be with a chest open; farther closes it.
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
    pub pos: BlockPos,
    pub slots: Vec<Option<ItemStack>>,
}

/// The chest block's id (`None` while the manifest has no chest).
pub fn chest_block() -> Option<BlockId> {
    block_props().id_named(CHEST_BLOCK_NAME)
}

/// The container `client` has open, as its screen shows it.
pub fn container_view(data: &GameData, client: ClientId) -> Option<ContainerView> {
    let pos = data.clients.get(&client)?.open_container?;
    let c = data.containers().get(&pos)?;
    Some(ContainerView { pos, slots: c.slots.clone() })
}

impl Rollback {
    /// Whether `pos` holds a chest block.
    fn is_chest_at(&self, pos: BlockPos) -> bool {
        chest_block().is_some_and(|chest| self.block_at(pos).is_some_and(|b| b.id == chest))
    }

    /// Whether `client`'s feet tile is within [`CONTAINER_REACH_BLOCKS`] of
    /// `pos` (no body: no).
    fn in_container_reach(&self, client: ClientId, pos: BlockPos) -> bool {
        let Some(&e) = self.data.player_entites.get(&client) else { return false };
        let Some(feet) = self.body_feet(e) else { return false };
        let t = crate::feet_tile(feet);
        let d2: i64 = (0..3).map(|a| i64::from(t[a] - pos[a]).pow(2)).sum();
        d2 <= i64::from(CONTAINER_REACH_BLOCKS).pow(2)
    }

    /// Opens the container at `pos` for `client` (and its inventory screen
    /// with it). Refused (`false`) with no container there, or out of reach.
    pub fn open_container_safe(&mut self, client: ClientId, pos: BlockPos) -> bool {
        if !self.data.containers.contains_key(&pos) || !self.in_container_reach(client, pos) {
            return false;
        }
        let Some(c) = self.data.clients.get(&client) else { return false };
        if c.open_container == Some(pos) && c.inventory_open {
            return true;
        }
        if let Some(c) = self.data.clients.get_mut(&client) {
            c.open_container = Some(pos);
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
    /// whose container is gone. Clients in id order.
    pub fn close_far_containers_safe(&mut self) {
        let open: Vec<(ClientId, BlockPos)> =
            self.data.clients.iter().filter_map(|(id, c)| Some((*id, c.open_container?))).collect();
        for (client, pos) in open {
            if !self.data.containers.contains_key(&pos) || !self.in_container_reach(client, pos) {
                self.close_container_safe(client);
            }
        }
    }

    /// Removes the container at `pos`, closes it for every client that has
    /// it open, and drops its stacks as piles at its cell (merging, as any
    /// deposit), pickable after [`PICKUP_DELAY_TICKS`] from `tick`. Returns
    /// the items the pile caps destroyed (counted where it happens).
    pub fn spill_container_safe(&mut self, pos: BlockPos, tick: u64) -> u32 {
        let Some(container) = self.data.containers.remove(&pos) else { return 0 };
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
        let mut destroyed = 0;
        for stack in container.slots.into_iter().flatten() {
            destroyed += self.deposit_items_safe(pos, stack, tick, PICKUP_DELAY_TICKS);
        }
        destroyed
    }

    /// After block `pos` was written: a new chest gets an empty container,
    /// a container whose chest went spills. Called by every terrain write in
    /// `edit.rs`. Returns the items destroyed (already counted).
    pub(crate) fn containers_after_write_safe(&mut self, pos: BlockPos) -> u32 {
        let has = self.data.containers.contains_key(&pos);
        let chest = self.is_chest_at(pos);
        if chest && !has {
            self.data.containers.insert(pos, Container::chest());
        } else if !chest && has {
            let tick = *self.data.tick as u64;
            return self.spill_container_safe(pos, tick);
        }
        0
    }
}
