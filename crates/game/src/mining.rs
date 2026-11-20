//! Survival mining, placing and throwing (plan 2026-11-05, Decisions 7, 8
//! and 11): what an Adventure player's mouse buttons and Q do.
//!
//! - **Mining** is sim state on `Client` ([`Mining`]): while the left button
//!   is held and the eye ray hits the same block, progress builds. A new
//!   target, a release (or leaving first person, or opening the inventory)
//!   stops it, and the block stays damaged ([`Damaged`], `Client::damaged`)
//!   for [`DAMAGE_LINGER_TICKS`]: mined again within that time it goes on
//!   from where it stopped, otherwise the damage is gone (the block is whole
//!   again). At most [`DAMAGED_MAX`] blocks are remembered per player. While
//!   it runs, progress is never accumulated: it is `tick - started_tick`,
//!   all integer ticks (a resumed mining starts that many ticks "ago"). The
//!   block breaks on the [`break_ticks`]-th tick held (the start tick
//!   counts), drops its item as a pile in its own cell, and the next block
//!   starts on the tick the break lands ([`BREAK_COOLDOWN_TICKS`] is 0: no
//!   pause between blocks).
//!
//!   The block may be the neighbour's, seen through the halo: the break is
//!   always an [`crate::Intent::BreakBlock`] (`survival`) to the block's
//!   owner, here or across a seam alike, which applies it at the opening of
//!   the next tick — so it is sent on the tick before the last one held and
//!   lands on the `break_ticks`-th (a one-tick block breaks one tick late).
//!   The owner's refusals decide, and the drop lands in the owner's piles.
//! - **Placing** (right press) puts the selected hotbar block against the
//!   hit face and takes one from the slot at once; the block itself is an
//!   [`crate::Intent::PlaceBlock`] to the cell's owner (next tick), and a
//!   place the owner refuses gives the item back
//!   ([`Rollback::refund_refused_places_safe`]). A non-block item or an
//!   empty slot places nothing.
//! - **Throwing** (Q one, Ctrl+Q the whole stack) drops the selected item as
//!   a pile [`THROW_DISTANCE_BLOCKS`] ahead along the look, clamped by the
//!   first block in the way (halo blocks included), settled onto the ground
//!   at once — or, landing in a neighbour's cell, deposited there by an
//!   intent to its owner (`item_piles.rs`).
//!
//! Overview clients keep `edit.rs`'s creative edit (also intents).

use crate::edit::{BREAK_BUTTON, BlockPos};
use crate::intents::{Effects as _, RegionEffects};
use crate::{
    BLOCK_VOXELS, Block, BlockId, BlockProps, Client, ClientId, EntityId, Intent, IntentMsg,
    IntentReply, ItemId, ItemStack, MAX_STACK, PICKUP_DELAY_TICKS, PlayMode, Rollback,
    THROWN_PICKUP_DELAY_TICKS, Target, WorldBlockPos, block_props, drop_of,
};

/// Ticks per whole unit of hardness with the bare hand (Minecraft's
/// correct-tool hand rule: `ceil(hardness * 30)` ticks at 20 ticks/s).
pub const HAND_TICKS_PER_HARDNESS: u32 = 30;
/// A trunk block (`BlockProps::is_trunk`) takes this many times as long.
pub const FELL_TICKS_FACTOR: u32 = 2;
/// Ticks without mining progress after a break lands. 0: holding the button
/// goes straight on to the next block, on the tick the break lands.
pub const BREAK_COOLDOWN_TICKS: u64 = 0;
/// How long a block a player stopped mining keeps its damage (3 s): mined
/// again within it, the progress goes on; after it the block is whole again.
pub const DAMAGE_LINGER_TICKS: u64 = 3000 / crate::TICK_RATE;
/// Damaged blocks remembered per player; one more forgets the oldest.
pub const DAMAGED_MAX: usize = 4;
/// How far ahead (blocks, along the look) a thrown item lands.
pub const THROW_DISTANCE_BLOCKS: f32 = 2.0;

/// The block a client is mining.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    state_hash::StateHash,
)]
pub struct Mining {
    pub target: BlockPos,
    /// The first tick of progress (it counts as one).
    pub started_tick: u64,
    /// Ticks held until the block breaks ([`break_ticks`]).
    pub needed_ticks: u32,
}

/// A block a client mined a part of and left (`Client::damaged`).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    state_hash::StateHash,
)]
pub struct Damaged {
    pub target: BlockPos,
    /// Ticks of progress it was left with (> 0).
    pub progress_ticks: u32,
    /// The [`break_ticks`] of the block it was mined as: a block that now
    /// needs another count is another block, and starts from nothing.
    pub needed_ticks: u32,
    /// The first tick it was not mined; gone [`DAMAGE_LINGER_TICKS`] later.
    pub left_tick: u64,
}

/// A damaged block for the renderer (client-side projection): the cracks
/// follow `progress_per_mille`, and the block is drawn from `block` /
/// `chisel` as its own piece so it can wobble.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BlockDamageView {
    /// Region-local; may lie in the halo (a neighbour's block).
    pub target: BlockPos,
    /// 0..=1000.
    pub progress_per_mille: u32,
    /// Being mined right now (struck), not just remembered.
    pub mining: bool,
    pub block: Block,
    pub chisel: Option<crate::ChiselData>,
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
    if props.is_trunk(block.id) {
        hand * FELL_TICKS_FACTOR
    } else {
        hand
    }
}

/// `client`'s mining progress at `now` (the region's current tick, i.e. after
/// the ticks so far), or `None` while it mines nothing.
pub fn mining_view(client: &Client, now: u64) -> Option<MiningView> {
    let m = client.mining?;
    let held = now.saturating_sub(m.started_tick);
    let progress = (held * 1000 / u64::from(m.needed_ticks.max(1))).min(1000) as u32;
    Some(MiningView {
        target: m.target,
        progress_per_mille: progress,
    })
}

impl Rollback {
    /// Whether an Adventure player may mine the block at `pos` (this
    /// region's): what a break may clear (`clearable_block`: not air, not
    /// bedrock), and not water. Returns the block. The owner's check.
    pub(crate) fn mineable(&self, pos: BlockPos) -> Option<Block> {
        let (block, _) = self.clearable_block(pos)?;
        (!crate::block_props().is_liquid(block.id)).then_some(block)
    }

    /// [`Self::mineable`] as this region sees `pos`, which may lie in the
    /// halo (a neighbour's block: not air, not its bedrock, not water). The
    /// sender's check; the owner checks again when the break reaches it.
    fn mineable_seen(&self, pos: BlockPos) -> Option<Block> {
        if crate::edit::locate(pos).is_some() {
            return self.mineable(pos);
        }
        let block = self.halo_mineable(pos)?;
        (!crate::block_props().is_liquid(block.id)).then_some(block)
    }

    /// The block at `pos` and its chisel as this region sees it (its own, or
    /// a neighbour's in the halo), if it can be mined.
    fn mineable_state_seen(&self, pos: BlockPos) -> Option<(Block, Option<crate::ChiselData>)> {
        let block = self.mineable_seen(pos)?;
        let chisel = match crate::edit::locate(pos) {
            Some(_) => self.clearable_block(pos)?.1,
            None => self.halo_chisel(pos).cloned(),
        };
        Some((block, chisel))
    }

    /// Every block a client of this region is mining or left damaged, for
    /// the renderer, at `now` (the region's current tick). A block two
    /// players damaged is listed once, with the larger progress.
    pub fn block_damage_views(&self, now: u64) -> Vec<BlockDamageView> {
        let mut out: std::collections::BTreeMap<BlockPos, (u32, bool)> =
            std::collections::BTreeMap::new();
        let mut add = |target: BlockPos, progress: u32, mining: bool| {
            let e = out.entry(target).or_insert((0, false));
            *e = (e.0.max(progress), e.1 || mining);
        };
        for c in self.data.clients.values() {
            if let Some(v) = mining_view(c, now) {
                add(v.target, v.progress_per_mille, true);
            }
            for d in &c.damaged {
                add(
                    d.target,
                    (d.progress_ticks * 1000 / d.needed_ticks.max(1)).min(1000),
                    false,
                );
            }
        }
        out.into_iter()
            .filter_map(|(target, (progress_per_mille, mining))| {
                let (block, chisel) = self.mineable_state_seen(target)?;
                Some(BlockDamageView {
                    target,
                    progress_per_mille,
                    mining,
                    block,
                    chisel,
                })
            })
            .collect()
    }

    /// The other blocks a chop at `target` brings down with it
    /// (`trees::fell`: the cut and the falling part, less `target`), as this
    /// region sees them, sorted — the renderer shakes them with the block
    /// being chopped. Empty when `target` is not tree wood. A whole-tree
    /// flood: callers cache it per target (the client's region runner).
    pub fn shake_blocks(&self, target: BlockPos) -> Vec<(BlockPos, Block)> {
        if self.is_tree_root(target) {
            return Vec::new();
        }
        let Some(felling) = crate::trees::fell(self, target) else {
            return Vec::new();
        };
        let mut all: Vec<BlockPos> = felling
            .cut
            .into_iter()
            .chain(felling.falling.into_iter().flatten())
            .collect();
        all.sort_unstable();
        all.dedup();
        all.into_iter()
            .filter(|p| *p != target)
            .filter_map(|p| self.tile_block(p).map(|b| (p, b)))
            .collect()
    }

    /// The id of `client`'s body, if it has one here.
    pub(crate) fn client_body_id(&self, client: ClientId) -> Option<EntityId> {
        self.data
            .player_entites
            .get(&client)
            .and_then(|&k| self.entity_id(k))
    }

    /// Writes `client`'s mining state if it differs (one logged delta).
    fn set_mining_safe(
        &mut self,
        client: ClientId,
        mining: Option<Mining>,
        cooldown_until: Option<u64>,
        damaged: Vec<Damaged>,
    ) {
        let Some(c) = self.data.clients.get(&client) else {
            return;
        };
        let until = cooldown_until.unwrap_or(c.break_cooldown_until);
        if c.mining == mining && c.break_cooldown_until == until && c.damaged == damaged {
            return;
        }
        if let Some(c) = self.data.clients.get_mut(&client) {
            c.mining = mining;
            c.break_cooldown_until = until;
            c.damaged = damaged;
        }
    }

    /// One tick of `client`'s survival mining (Decision 7), at region tick
    /// `tick`. Only an Adventure client in first person with no inventory
    /// open and the left button held mines; anything else cancels. The
    /// break goes to the block's owner as an intent (into `out` when that is
    /// a neighbour), sent on the tick before the last one held.
    pub(crate) fn tick_mining_safe(
        &mut self,
        client: ClientId,
        tick: u64,
        out: &mut Vec<IntentMsg>,
    ) {
        let Some(c) = self.data.clients.get(&client) else {
            return;
        };
        let active = c.mode == PlayMode::Adventure
            && !c.editor
            && c.fps_cam_mode
            && !c.inventory_open
            && c.input.mouse_held(&BREAK_BUTTON)
            && tick >= c.break_cooldown_until;
        let current = c.mining;
        let mut damaged = c.damaged.clone();
        let target = if active {
            self.eye_hit(client)
                .and_then(|hit| self.mineable_seen(hit.block).map(|b| (hit.block, b)))
        } else {
            None
        };
        // A mining whose break was already sent (the previous tick) is over:
        // the block is gone and the ray hits the next one, or the owner
        // refused the break and the same block starts again from nothing.
        let sent =
            |m: &Mining| (tick + 1).saturating_sub(m.started_tick) >= u64::from(m.needed_ticks);
        let going_on = current.filter(|m| !sent(m) && target.is_some_and(|(t, _)| t == m.target));
        // A mining that stops short of its break leaves the block damaged.
        if let Some(m) = current.filter(|m| going_on.is_none() && !sent(m)) {
            let progress_ticks = tick.saturating_sub(m.started_tick) as u32;
            if progress_ticks > 0 {
                damaged.retain(|d| d.target != m.target);
                if damaged.len() >= DAMAGED_MAX {
                    damaged.remove(0);
                }
                damaged.push(Damaged {
                    target: m.target,
                    progress_ticks,
                    needed_ticks: m.needed_ticks,
                    left_tick: tick,
                });
            }
        }
        // Damage left too long is gone, and so is that of a block that is.
        damaged.retain(|d| {
            tick.saturating_sub(d.left_tick) < DAMAGE_LINGER_TICKS
                && self.mineable_seen(d.target).is_some()
        });
        let Some((target, block)) = target else {
            self.set_mining_safe(client, None, None, damaged);
            return;
        };
        let mining = going_on.unwrap_or_else(|| {
            let needed_ticks = break_ticks(block_props(), block);
            // Back at a block left damaged: on from where it stopped.
            let left = damaged
                .iter()
                .position(|d| d.target == target)
                .map(|i| damaged.remove(i));
            let done = left
                .filter(|d| d.needed_ticks == needed_ticks)
                .map_or(0, |d| u64::from(d.progress_ticks));
            Mining {
                target,
                started_tick: tick.saturating_sub(done),
                needed_ticks,
            }
        });
        // Held through this tick and the next one counts the needed ticks:
        // the break, applied at the next tick's opening, is due now.
        if (tick + 2).saturating_sub(mining.started_tick) < u64::from(mining.needed_ticks) {
            self.set_mining_safe(client, Some(mining), None, damaged);
            return;
        }
        let Some(by) = self.client_body_id(client) else {
            return;
        };
        let p = WorldBlockPos::from_local(
            *self.data.region,
            crate::BlockPos::new(target[0], target[1], target[2]),
        );
        RegionEffects { data: self, out }
            .send(Target::Block(p), Intent::BreakBlock { by, survival: true });
        // The progress stays until the break lands (the next tick); the
        // cooldown counts from that tick.
        self.set_mining_safe(
            client,
            Some(mining),
            Some(tick + 1 + BREAK_COOLDOWN_TICKS),
            damaged,
        );
    }

    /// An Adventure player's break of tree block `p` reached its owner
    /// (Decision 9). Under [`crate::TreeFelling::Instant`] the whole felled
    /// part vanishes at once (`fell_instant_safe`) and its items drop where
    /// it stood: per column, summed per item, at the column's lowest cleared
    /// cell, from where the pile settles to the ground. Under `Physics` the
    /// tree is felled as a chop order fells it (`fell_safe`), the cut blocks
    /// drop their items at the stump right away, and the falling part turns
    /// into piles where it comes to rest (`SettlePolicy::DropItems`). A
    /// block `fell` refuses (leaves, or a tree over `MAX_TREE_BLOCKS`) is
    /// broken alone, like any block. Returns whether anything was cleared.
    pub fn player_chop_safe(&mut self, p: BlockPos, tick: u64) -> bool {
        if self.is_tree_root(p) {
            return self
                .mineable(p)
                .is_some_and(|block| self.mine_block_safe(p, block, tick));
        }
        let felled = match crate::TREE_FELLING {
            crate::TreeFelling::Instant => self.fell_instant_safe(p).map(|gone| {
                let mut columns: std::collections::BTreeMap<[i32; 2], (BlockPos, Vec<Block>)> =
                    std::collections::BTreeMap::new();
                for (pos, block) in gone {
                    let column = columns.entry([pos[0], pos[2]]).or_insert((pos, Vec::new()));
                    if pos[1] < column.0[1] {
                        column.0 = pos;
                    }
                    column.1.push(block);
                }
                for (_, (at, blocks)) in columns {
                    self.drop_blocks_at_safe(at, blocks, tick);
                }
            }),
            crate::TreeFelling::Physics => {
                self.fell_safe(p, crate::SettlePolicy::DropItems)
                    .map(|(cut, _body)| {
                        // At the stump: the cut's lowest cell, summed per item.
                        if let Some(&(at, _)) = cut.iter().min_by_key(|(pos, _)| (pos[1], *pos)) {
                            self.drop_blocks_at_safe(at, cut.iter().map(|(_, b)| *b), tick);
                        }
                    })
            }
        };
        if felled.is_none() {
            return match self.mineable(p) {
                Some(block) => self.mine_block_safe(p, block, tick),
                None => false,
            };
        }
        true
    }

    /// Breaks one mined block and drops its item as a pile in its cell.
    /// Returns whether the block was cleared.
    pub(crate) fn mine_block_safe(&mut self, pos: BlockPos, block: Block, tick: u64) -> bool {
        if !self.set_block_in(pos, Block::new(BlockId::AIR)) {
            return false;
        }
        if let Some(stack) = drop_of(block) {
            // A loss to the pile caps is counted where it happens.
            let _destroyed = self.deposit_items_safe(pos, stack, tick, PICKUP_DELAY_TICKS);
        }
        true
    }

    /// `client`'s right click in survival (Decision 8): takes one of the
    /// selected hotbar block and sends it to the cell against the hit face
    /// (an intent to that cell's owner, into `out` when that is a
    /// neighbour). Returns whether a place was sent. A non-block item, an
    /// empty slot, a hit from inside a block, a cell seen occupied or a body
    /// in the way place nothing.
    pub(crate) fn survival_place_safe(
        &mut self,
        client: ClientId,
        out: &mut Vec<IntentMsg>,
    ) -> bool {
        self.place_selected_safe(client, true, out)
    }

    /// The selected hotbar block placement shared by survival and editor
    /// clients. Chests open before a placement. Editor placement does not
    /// consume the selected stack.
    pub(crate) fn place_selected_safe(
        &mut self,
        client: ClientId,
        consume: bool,
        out: &mut Vec<IntentMsg>,
    ) -> bool {
        let Some(hit) = self.eye_hit(client) else {
            return false;
        };
        // A chest under the crosshair — here, or a neighbour's seen through
        // the halo — opens instead (Decision 10).
        let [x, y, z] = hit.block;
        let cell = WorldBlockPos::from_local(*self.data.region, crate::BlockPos::new(x, y, z));
        if self.data.container_seen(cell).is_some() {
            self.open_container_safe(client, cell);
            return false;
        }
        let Some(c) = self.data.clients.get(&client) else {
            return false;
        };
        let selected = c.selected;
        let Some(stack) = c.inventory.slots.get(selected as usize).copied().flatten() else {
            return false;
        };
        if selected as usize >= crate::HOTBAR_SLOTS {
            return false;
        }
        let Some(id) = stack
            .item
            .as_block()
            .filter(|id| *id != BlockId::AIR && (consume || crate::item_exists(stack.item)))
        else {
            return false;
        };
        if hit.normal == [0; 3] {
            return false;
        }
        let target = [
            hit.block[0] + hit.normal[0],
            hit.block[1] + hit.normal[1],
            hit.block[2] + hit.normal[2],
        ];
        if self.block_solid(target) || self.block_overlaps_body(target) {
            return false;
        }
        let Some(by) = self.client_body_id(client) else {
            return false;
        };
        if consume {
            if let Some(c) = self.data.clients.get_mut(&client) {
                let _ = c.inventory.take_selected(selected, 1);
            }
        }
        let p = WorldBlockPos::from_local(
            *self.data.region,
            crate::BlockPos::new(target[0], target[1], target[2]),
        );
        RegionEffects { data: self, out }.send(
            Target::Block(p),
            Intent::PlaceBlock {
                block: id,
                by,
                survival: consume,
            },
        );
        true
    }

    /// The survival places refused by their owner, in `client`'s inbox this
    /// tick, give their item back: into the inventory, else (full) as a
    /// pile at the feet.
    pub(crate) fn refund_refused_places_safe(&mut self, client: ClientId) {
        let Some(by) = self.client_body_id(client) else {
            return;
        };
        let refunds: Vec<BlockId> = self
            .intent_inbox(by)
            .into_iter()
            .filter_map(|r| match r {
                IntentReply::Refused(
                    Intent::PlaceBlock {
                        block,
                        survival: true,
                        ..
                    },
                    why,
                ) => {
                    log::debug!("[mining] place of {block:?} refused ({why:?}): refunded");
                    Some(block)
                }
                _ => None,
            })
            .collect();
        for block in refunds {
            let stack = ItemStack {
                item: ItemId::block(block),
                count: 1,
            };
            let left = self
                .data
                .clients
                .get_mut(&client)
                .map_or(1, |c| c.inventory.add(stack));
            if left > 0 {
                self.drop_at_feet_safe(
                    client,
                    ItemStack {
                        count: left,
                        ..stack
                    },
                );
            }
        }
    }

    /// Throws one of `client`'s selected hotbar item (the whole stack with
    /// `whole_stack`) [`THROW_DISTANCE_BLOCKS`] ahead along its look, short of
    /// the first block in the way, settled as a pile that can be picked up
    /// after [`THROWN_PICKUP_DELAY_TICKS`]. Returns whether anything was
    /// thrown.
    pub fn throw_selected_safe(&mut self, client: ClientId, whole_stack: bool, tick: u64) -> bool {
        let Some((eye, landing)) = self.throw_landing(client) else {
            return false;
        };
        let Some(c) = self.data.clients.get(&client) else {
            return false;
        };
        let selected = c.selected;
        if selected as usize >= crate::HOTBAR_SLOTS
            || c.inventory
                .slots
                .get(selected as usize)
                .is_none_or(|s| s.is_none())
        {
            return false;
        }
        let n = if whole_stack { MAX_STACK } else { 1 };
        let Some(stack) = self
            .data
            .clients
            .get_mut(&client)
            .and_then(|c| c.inventory.take_selected(selected, n))
        else {
            return false;
        };
        self.throw_stack_safe(client, eye, landing, stack, tick);
        true
    }

    /// Throws the complete authoritative cursor stack from an open inventory.
    /// The endpoint is resolved before taking the stack, so invalid throws
    /// retain it.
    pub fn throw_cursor_safe(&mut self, client: ClientId, tick: u64) -> bool {
        if !self
            .data
            .clients
            .get(&client)
            .is_some_and(|c| c.inventory_open && c.cursor.as_ref().is_some_and(|s| s.count > 0))
        {
            return false;
        }
        let Some((eye, landing)) = self.throw_landing(client) else {
            return false;
        };
        let Some(stack) = self
            .data
            .clients
            .get_mut(&client)
            .and_then(|c| c.cursor.take())
            .filter(|s| s.count > 0)
        else {
            return false;
        };
        self.throw_stack_safe(client, eye, landing, stack, tick);
        true
    }

    /// Resolves an authoritative eye ray and safe landing without mutating any
    /// item state.
    fn throw_landing(&self, client: ClientId) -> Option<(rapier3d::math::Vector, [i32; 3])> {
        let (eye, dir) = self.eye_ray(client)?;
        let bv = BLOCK_VOXELS as f32;
        let cell_of = |p: rapier3d::math::Vector| {
            [
                (p.x / bv).floor() as i32,
                (p.y / bv).floor() as i32,
                (p.z / bv).floor() as i32,
            ]
        };
        let landing = match self.raycast_block_in(eye, dir, THROW_DISTANCE_BLOCKS) {
            // Inside a block already: from the eye's own cell.
            Some(hit) if hit.normal == [0; 3] => cell_of(eye),
            Some(hit) => [
                hit.block[0] + hit.normal[0],
                hit.block[1] + hit.normal[1],
                hit.block[2] + hit.normal[2],
            ],
            None => cell_of(eye + dir * (THROW_DISTANCE_BLOCKS * bv)),
        };
        Some((eye, landing))
    }

    fn throw_stack_safe(
        &mut self,
        client: ClientId,
        eye: rapier3d::math::Vector,
        landing: BlockPos,
        stack: ItemStack,
        tick: u64,
    ) {
        let bv = BLOCK_VOXELS as f32;
        let origin = [
            (eye.x / bv).floor() as i32,
            (eye.y / bv).floor() as i32,
            (eye.z / bv).floor() as i32,
        ];
        // A loss to the pile caps is counted where it happens.
        // From the eye: the renderer arcs it to where it lands (`ItemsLanded`).
        // A landing across a seam is a deposit intent to its owner; what its
        // piles cannot hold comes back to the thrower.
        let by = self
            .client_body_id(client)
            .unwrap_or(crate::EntityId::of_region(*self.data.region));
        let _destroyed =
            self.deposit_items_by_safe(by, origin, landing, stack, tick, THROWN_PICKUP_DELAY_TICKS);
    }
}
