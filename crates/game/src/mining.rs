//! Survival mining, placing and throwing (plan 2026-11-05, Decisions 7, 8
//! and 11): what an Adventure player's mouse buttons and Q do.
//!
//! - **Mining** is sim state on `Client` ([`Mining`]): while the left button
//!   is held and the eye ray hits the same block, progress builds; a new
//!   target restarts it, a release (or leaving first person, or opening the
//!   inventory) cancels it. Progress is never accumulated: it is
//!   `tick - started_tick`, all integer ticks. The block breaks on the
//!   [`break_ticks`]-th tick held (the start tick counts), drops its item as
//!   a pile in its own cell, and no mining progresses for
//!   [`BREAK_COOLDOWN_TICKS`] ticks after.
//! - **Placing** (right press) puts the selected hotbar block against the
//!   hit face and takes one from the slot; a non-block item or an empty slot
//!   places nothing.
//! - **Throwing** (Q one, Ctrl+Q the whole stack) drops the selected item as
//!   a pile [`THROW_DISTANCE_BLOCKS`] ahead along the look, clamped by the
//!   first block in the way, settled onto the ground at once.
//!
//! Fortress clients keep `edit.rs`'s instant creative edit.

use crate::edit::{BlockPos, BREAK_BUTTON};
use crate::{
    block_props, drop_of, Block, BlockId, BlockProps, Client, ClientId, PlayMode, Rollback, BLOCK_VOXELS,
    MAX_STACK, PICKUP_DELAY_TICKS, THROWN_PICKUP_DELAY_TICKS,
};

/// Ticks per whole unit of hardness with the bare hand (Minecraft's
/// correct-tool hand rule: `ceil(hardness * 30)` ticks at 20 ticks/s).
pub const HAND_TICKS_PER_HARDNESS: u32 = 30;
/// A trunk block (`BlockProps::is_trunk`) takes this many times as long.
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
    /// Ticks held until the block breaks ([`break_ticks`]).
    pub needed_ticks: u32,
}

/// What the HUD shows of the local client's mining (client-side projection).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MiningView {
    pub target: BlockPos,
    /// 0..=1000.
    pub progress_per_mille: u32,
}

/// Ticks of holding the button to break `block` by hand: `ceil(hardness_centi
/// * HAND_TICKS_PER_HARDNESS / 100)` (at least 1), times
/// [`FELL_TICKS_FACTOR`] for trunk wood.
pub fn break_ticks(props: &BlockProps, block: Block) -> u32 {
    let hardness = u32::from(props.hardness_centi(block.id));
    let hand = (hardness * HAND_TICKS_PER_HARDNESS).div_ceil(100).max(1);
    if props.is_trunk(block.id) { hand * FELL_TICKS_FACTOR } else { hand }
}

/// `client`'s mining progress at `now` (the region's current tick, i.e. after
/// the ticks so far), or `None` while it mines nothing.
pub fn mining_view(client: &Client, now: u64) -> Option<MiningView> {
    let m = client.mining?;
    let held = now.saturating_sub(m.started_tick);
    let progress = (held * 1000 / u64::from(m.needed_ticks.max(1))).min(1000) as u32;
    Some(MiningView { target: m.target, progress_per_mille: progress })
}

impl Rollback {
    /// Whether an Adventure player may mine the block at `pos`: what a break
    /// may clear (`clearable_block`: not air, not bedrock, not the locked
    /// edge ring), and not water. Returns the block.
    fn mineable(&self, pos: BlockPos) -> Option<Block> {
        let (block, _) = self.clearable_block(pos)?;
        (!crate::pathfind::is_water(block.id)).then_some(block)
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
    /// open and the left button held mines; anything else cancels.
    pub fn tick_mining_safe(&mut self, client: ClientId, tick: u64) {
        let Some(c) = self.data.clients.get(&client) else { return };
        let active = c.mode == PlayMode::Adventure
            && c.fps_cam_mode
            && !c.inventory_open
            && c.input.mouse_held(&BREAK_BUTTON)
            && tick >= c.break_cooldown_until;
        let current = c.mining;
        let target = if active {
            self.eye_hit(client).and_then(|hit| self.mineable(hit.block).map(|b| (hit.block, b)))
        } else {
            None
        };
        let Some((target, block)) = target else {
            self.set_mining_safe(client, None, None);
            return;
        };
        let mining = match current {
            Some(m) if m.target == target => m,
            _ => Mining { target, started_tick: tick, needed_ticks: break_ticks(block_props(), block) },
        };
        if (tick + 1).saturating_sub(mining.started_tick) < u64::from(mining.needed_ticks) {
            self.set_mining_safe(client, Some(mining), None);
            return;
        }
        let broke = match block.tree_id() {
            0 => self.mine_block_safe(target, block, tick),
            // Task 9 hook: a tree block will run `player_chop_safe` (the
            // felling path) here. Until then it is mined like any block.
            _tree => self.mine_block_safe(target, block, tick),
        };
        let cooldown = broke.then_some(tick + 1 + BREAK_COOLDOWN_TICKS);
        self.set_mining_safe(client, None, cooldown);
    }

    /// Breaks one mined block and drops its item as a pile in its cell.
    /// Returns whether the block was cleared.
    fn mine_block_safe(&mut self, pos: BlockPos, block: Block, tick: u64) -> bool {
        if !self.set_block_in(pos, Block::new(BlockId::AIR)) {
            return false;
        }
        if let Some(stack) = drop_of(block) {
            // A loss to the pile caps is counted where it happens.
            let _destroyed = self.deposit_items_safe(pos, stack, tick, PICKUP_DELAY_TICKS);
        }
        true
    }

    /// `client`'s right click in survival (Decision 8): places the selected
    /// hotbar block against the hit face and takes one from the slot.
    /// Returns whether a block was placed. A non-block item, an empty slot,
    /// a hit from inside a block, an occupied cell or a body in the way
    /// place nothing.
    pub fn survival_place_safe(&mut self, client: ClientId) -> bool {
        let Some(hit) = self.eye_hit(client) else { return false };
        // Task 10 hook: a chest under the crosshair opens here instead.
        let Some(c) = self.data.clients.get(&client) else { return false };
        let selected = c.selected;
        let Some(stack) = c.inventory.slots.get(selected as usize).copied().flatten() else { return false };
        if selected as usize >= crate::HOTBAR_SLOTS {
            return false;
        }
        let Some(id) = stack.item.as_block().filter(|id| *id != BlockId::AIR) else { return false };
        if hit.normal == [0; 3] {
            return false;
        }
        let target = [hit.block[0] + hit.normal[0], hit.block[1] + hit.normal[1], hit.block[2] + hit.normal[2]];
        if self.block_solid(target) || self.block_overlaps_body(target) {
            return false;
        }
        if !self.set_block_in(target, Block::new(id)) {
            return false;
        }
        if let Some(c) = self.data.clients.get_mut(&client) {
            let _ = c.inventory.take_selected(selected, 1);
        }
        true
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
        let bv = BLOCK_VOXELS as f32;
        let cell_of = |p: rapier3d::math::Vector| [(p.x / bv).floor() as i32, (p.y / bv).floor() as i32, (p.z / bv).floor() as i32];
        let landing = match self.raycast_block_in(eye, dir, THROW_DISTANCE_BLOCKS) {
            // Inside a block already: from the eye's own cell.
            Some(hit) if hit.normal == [0; 3] => cell_of(eye),
            Some(hit) => [hit.block[0] + hit.normal[0], hit.block[1] + hit.normal[1], hit.block[2] + hit.normal[2]],
            None => cell_of(eye + dir * (THROW_DISTANCE_BLOCKS * bv)),
        };
        let n = if whole_stack { MAX_STACK } else { 1 };
        let Some(stack) = self.data.clients.get_mut(&client).and_then(|c| c.inventory.take_selected(selected, n)) else {
            return false;
        };
        // A loss to the pile caps is counted where it happens.
        let _destroyed = self.deposit_items_safe(landing, stack, tick, THROWN_PICKUP_DELAY_TICKS);
        true
    }
}
