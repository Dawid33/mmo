//! Intents (spec §3.2 P6, plan Decision 1): every write to something the
//! writer does not own — a block, a pile, a chest, another entity — is an
//! [`Intent`] sent through [`Effects`] to a [`Target`], never a direct write.
//!
//! **When.** An intent sent while a region's tick counter reads T (during
//! tick T, or an event of the slot before it) is applied by the target's
//! owner at the opening of tick T+1 — its `due` tick — in the same region
//! exactly as across a seam: a local one waits in the owner's queue
//! ([`crate::GameData`] `intents`), a remote one leaves as
//! [`crate::RegionOutput::Intents`], is routed to the owner as a
//! [`crate::RegionInput::Transfer`] stamped `due` (released in slot `due`,
//! before tick `due`), and joins the same queue. Self-writes stay immediate.
//! A lagging source's intent reaches an owner already past `due`: it applies
//! at the owner's next tick (spec P4).
//!
//! **Order.** The queue is keyed `(due, source EntityId, seq)`: due intents
//! apply in that order, so a conflict (two breaks of one block) goes to the
//! lower source id, and the loser gets a refusal. `seq` counts a source's
//! intents due at one tick, in the order the owner received them.
//!
//! **Replies.** Applying an intent produces an [`IntentReply`] for its
//! source (`Intent::by`), sent like an intent (due one tick later) to the
//! source's region — the manager's location directory finds it; a client,
//! which has none, uses the `hint` (the region the intent was sent from). At
//! the opening of its due tick a reply lands in the source entity's inbox
//! ([`Rollback::intent_inbox`]), which holds that tick's replies only.
//!
//! **Replies follow their entity.** An entity that departs with replies
//! queued for it takes them along: they leave with the departure, addressed
//! to its target and due unchanged, so they are delivered at the same tick
//! as without the seam. A reply that finds its entity gone at delivery is
//! sent on (`hops` + 1) for the manager to find it by its directory; past
//! [`MAX_INTENT_REPLY_HOPS`] the manager gives up, logged and counted.
//!
//! **Region-sourced intents.** A write no entity sends (a settling voxel
//! body's drops) is sent `by` [`EntityId::of_region`] of its region. Its
//! reply goes back to that region (the `hint`), which handles it itself
//! (`Rollback::region_reply_safe`): a refused deposit is put down there.
//!
//! **Never lost.** An intent whose owner is not running — or is stopping —
//! is answered `Refused(.., OwnerNotRunning)` by the manager (an intent never
//! wakes a region). A region that parks refuses the intents still queued in
//! it the same way. Queued replies are region state, so they park with it; a
//! reply for a parked or stopping region waits in the manager's mailbox for
//! its next run (and is saved with it). Intents and replies in flight
//! between regions are in a save cut exactly once: queued in the target's
//! snapshot, or as an in-flight row when neither the sender's nor the
//! target's snapshot holds them (`save_cut.rs`).
use crate::{ClientId, EntityId, RegionCoords, Rollback, Tick, WorldBlockPos};

/// How many times a reply is sent on after finding its entity gone before
/// the manager gives it up (logged at warn and counted).
pub const MAX_INTENT_REPLY_HOPS: u8 = 4;

/// What an intent is addressed to: an entity by its id, or a world position
/// (the region holding it owns it).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize, state_hash::StateHash,
)]
pub enum Target {
    Entity(EntityId),
    Block(WorldBlockPos),
    Pile(WorldBlockPos),
    Container(WorldBlockPos),
    /// Stud terrain of the region holding this world chunk (`y` = stud layer):
    /// `Intent::CarveStud` / `Intent::PlaceStud`.
    Stud(crate::WorldChunkPos),
}

/// Required by the rollback map; never a real target.
impl Default for Target {
    fn default() -> Self {
        Target::Block(WorldBlockPos::default())
    }
}

impl Target {
    /// The region owning a position target; `None` for an entity (found by
    /// its id).
    pub fn region(&self) -> Option<RegionCoords> {
        match self {
            Target::Entity(_) => None,
            Target::Block(p) | Target::Pile(p) | Target::Container(p) => Some(p.to_local().0),
            Target::Stud(c) => Some(c.to_local().0),
        }
    }
}

/// A write to something the sender does not own. `by` is the sending entity
/// (its reply goes there). Variants are added by the task that migrates a
/// system onto them.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize, state_hash::StateHash,
)]
pub enum Intent {
    /// Break the target block to air through the ordinary break path
    /// (`set_block_in`), with its refusals. `survival`: an Adventure
    /// player's mined block — its drop goes into the owner's piles, and a
    /// tree block fells its tree (`Rollback::player_chop_safe`); otherwise
    /// a plain (creative / dev) break that drops nothing.
    BreakBlock { by: EntityId, survival: bool },
    /// Put `block` (whole) into the target cell, refused when the owner sees
    /// it occupied (a solid block, or a body in the way). `survival`: the
    /// sender took one `block` item for it, which a refusal gives back
    /// (`Rollback::refund_refused_places_safe`).
    PlaceBlock { block: crate::BlockId, by: EntityId, survival: bool },
    /// Put `stack` into the pile of the target cell `cell` (a
    /// `Target::Pile`), as `item_piles.rs`'s deposit does there: that cell
    /// first, then the owner's own cells around it. The new items carry the
    /// sender's `expire` / `pickup` ticks (world ticks). Granted when all fit;
    /// else `Refused(.., PileFull)` carrying the items that did NOT fit,
    /// which the sender gets back (`Rollback::settle_pile_replies_safe`).
    DepositItems { cell: WorldBlockPos, stack: crate::ItemStack, expire: Tick, pickup: Tick, by: EntityId },
    /// Take up to `count` of `item` from the pile of the target cell `cell`.
    /// Granted with the count actually taken (into the sender's inventory
    /// only then); `Refused(.., NothingThere)` when there is none (or not
    /// yet pickable) — the loser of a race for the last item.
    TakeFromPile { cell: WorldBlockPos, item: crate::ItemId, count: u32, by: EntityId },
    /// Put `stack` into the chest of the target cell `cell` (a
    /// `Target::Container`): into `slot` only (empty, or the same item), or
    /// with `None` anywhere it fits (a shift click). The sender gave the
    /// stack up when it sent this; granted when all fit, else
    /// `Refused(.., ContainerFull)` carrying the rest (`NothingThere`, all
    /// of it, with no chest there), which goes back to the sender
    /// (`Rollback::settle_container_replies_safe`).
    PutInContainer { cell: WorldBlockPos, stack: crate::ItemStack, slot: Option<u8>, by: EntityId },
    /// Take up to `count` of `item` from slot `slot` of the chest of the
    /// target cell `cell`. Granted with the count actually taken — into the
    /// sender's cursor (`to_cursor`) or inventory only then;
    /// `Refused(.., NothingThere)` when the slot holds none of it (the loser
    /// of a race for one slot).
    TakeFromContainer {
        cell: WorldBlockPos,
        slot: u8,
        item: crate::ItemId,
        count: u32,
        to_cursor: bool,
        by: EntityId,
    },
    /// Stud world: carve the box of WORLD stud cells `lo .. hi`
    /// (`stud_halo::WorldStudCell`, `hi` exclusive) out of the owner's stud
    /// terrain (`Rollback::carve_stud_safe`): the part of a carve that reached
    /// past its sender's edge (a `Target::Stud` in the owner). Granted when it
    /// removed something, else `Refused(.., NothingThere)`. What it removed is
    /// not handed to anyone yet (there are no stud drops).
    CarveStud { lo: [i64; 3], hi: [i64; 3], by: EntityId },
    /// Stud world: place `brick` into the stud chunk `chunk` (world chunk, `y` =
    /// stud layer) of its owner (`Rollback::place_stud_safe`). Granted, or
    /// `Refused(.., Occupied)` when it is invalid there or overlaps a brick.
    PlaceStud { chunk: crate::WorldChunkPos, brick: crate::StudBrick, by: EntityId },
    /// Stud world: fill the empty cells of the box of WORLD stud cells `lo .. hi` with
    /// bricks of `material` in the owner's stud terrain
    /// (`Rollback::fill_stud_box_safe`): the part of a fill that reached past its
    /// sender's edge (a `Target::Stud` in the owner). Granted when it filled
    /// something, else `Refused(.., Occupied)`.
    FillStud { lo: [i64; 3], hi: [i64; 3], material: u16, by: EntityId },
    /// Stud world: remove the brick covering `cell` of the stud chunk `chunk` (world
    /// chunk, `y` = stud layer) of its owner (`Rollback::remove_stud_safe`): the
    /// neighbour's half of `Order::RemoveStudAt`. Granted, or
    /// `Refused(.., NothingThere)` with no brick there. Appended.
    RemoveStud { chunk: crate::WorldChunkPos, cell: [u8; 3], by: EntityId },
    /// Stud world: break the box of WORLD stud cells `lo .. hi` (`hi` exclusive; one
    /// brick's target box, `mining::stud_target_box`, inside one chunk) out of its
    /// owner's stud terrain — what a player's mining and creative break send
    /// (`mining.rs`, `edit.rs`), a `Target::Stud`. `survival`: what it removed goes
    /// into the owner's piles (`mining::stud_drops_rolled`), and tree wood fells its
    /// tree instead (`fell_stud_safe`); otherwise a plain break that drops nothing.
    /// `Refused(.., Bedrock)` on bedrock, `NothingThere` on air. Appended.
    BreakStud { lo: [i64; 3], hi: [i64; 3], by: EntityId, survival: bool },
    /// Stud world: place `brick` into the stud chunk `chunk` as
    /// [`Intent::PlaceStud`] does, for a survival player who took one `item` for it:
    /// `Refused(.., Occupied)` (an overlap, a body in the way) gives it back
    /// (`Rollback::refund_refused_places_safe`). Appended.
    PlaceBrick { chunk: crate::WorldChunkPos, brick: crate::StudBrick, item: crate::BlockId, by: EntityId },
}

/// Required by the rollback map; never a real intent.
impl Default for Intent {
    fn default() -> Self {
        Intent::BreakBlock { by: EntityId::default(), survival: false }
    }
}

impl Intent {
    /// The sending entity.
    pub fn by(&self) -> EntityId {
        match self {
            Intent::BreakBlock { by, .. }
            | Intent::PlaceBlock { by, .. }
            | Intent::DepositItems { by, .. }
            | Intent::TakeFromPile { by, .. }
            | Intent::PutInContainer { by, .. }
            | Intent::TakeFromContainer { by, .. }
            | Intent::CarveStud { by, .. }
            | Intent::PlaceStud { by, .. }
            | Intent::FillStud { by, .. }
            | Intent::RemoveStud { by, .. }
            | Intent::BreakStud { by, .. }
            | Intent::PlaceBrick { by, .. } => *by,
        }
    }
}

/// Why an owner (or the manager) refused an intent.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize, state_hash::StateHash,
)]
pub enum IntentRefusal {
    /// The owning region is not running (never woken by an intent).
    OwnerNotRunning,
    /// Nothing to act on: air, an empty slot, or a conflict lost to an
    /// earlier intent.
    NothingThere,
    /// The block is the column's bedrock.
    Bedrock,
    /// A place into a cell that is solid, or that a body overlaps.
    Occupied,
    /// The intent does not apply to that kind of target (or reached a
    /// region that does not own it).
    BadTarget,
    /// A deposit's items did not all fit in the piles around the cell.
    PileFull,
    /// A put's items did not all fit in the chest (or its slot).
    ContainerFull,
}

/// An owner's answer, delivered to `Intent::by` one tick after the intent
/// applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, state_hash::StateHash)]
pub enum IntentReply {
    Granted(Intent),
    Refused(Intent, IntentRefusal),
}

/// Required by the rollback map; never a real reply.
impl Default for IntentReply {
    fn default() -> Self {
        IntentReply::Granted(Intent::default())
    }
}

impl IntentReply {
    /// The items it hands to its entity, and the cell (a pile's or a
    /// chest's) they came from or were meant for: a granted take, a refused
    /// deposit's or put's rest, or a refused survival place's block (whose
    /// cell the reply does not carry: `None`). A reply that carries items is
    /// never given up silently (`WorldManager`).
    pub fn carried_items(&self) -> Option<(Option<WorldBlockPos>, crate::ItemStack)> {
        match *self {
            IntentReply::Granted(
                Intent::TakeFromPile { cell, item, count, .. } | Intent::TakeFromContainer { cell, item, count, .. },
            ) => Some((Some(cell), crate::ItemStack { item, count })),
            IntentReply::Refused(
                Intent::DepositItems { cell, stack, .. } | Intent::PutInContainer { cell, stack, .. },
                _,
            ) => Some((Some(cell), stack)),
            IntentReply::Refused(Intent::PlaceBlock { block, survival: true, .. }, _) => {
                Some((None, crate::ItemStack { item: crate::ItemId::block(block), count: 1 }))
            }
            _ => None,
        }
    }

    /// The entity it is for.
    pub fn to(&self) -> EntityId {
        match self {
            IntentReply::Granted(i) | IntentReply::Refused(i, _) => i.by(),
        }
    }
}

/// Where gameplay sends its intents (spec §3.3).
pub trait Effects {
    fn send(&mut self, target: Target, intent: Intent);
}

/// An intent on its way or queued at its owner: `from` is the region it was
/// sent from (its reply's `hint`), `due` the tick that opens with it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, state_hash::StateHash)]
pub struct SentIntent {
    pub target: Target,
    pub intent: Intent,
    pub from: RegionCoords,
    pub due: Tick,
}

/// A reply on its way or queued for delivery: `hint` is where its entity was
/// last known to be (a client routes there; the server asks its directory
/// first). `hops` counts the deliveries that found the entity gone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, state_hash::StateHash)]
pub struct SentReply {
    pub reply: IntentReply,
    pub hint: RegionCoords,
    pub due: Tick,
    pub hops: u8,
}

/// What travels between regions ([`crate::GameEventKind::Intent`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IntentMsg {
    Intent(SentIntent),
    Reply(SentReply),
}

impl IntentMsg {
    /// The tick that opens with it: its transfer stamp.
    pub fn due(&self) -> Tick {
        match self {
            IntentMsg::Intent(s) => s.due,
            IntentMsg::Reply(s) => s.due,
        }
    }

    /// The region it goes to without a location directory (a client's
    /// relay): a position target's owner, a reply's hint; `None` for an
    /// intent to an entity, and for a reply that already found its entity
    /// gone (only the server's directory knows where it went).
    pub fn region_hint(&self) -> Option<RegionCoords> {
        match self {
            IntentMsg::Intent(s) => s.target.region(),
            IntentMsg::Reply(s) => (s.hops == 0).then_some(s.hint),
        }
    }
}

/// A region's [`Effects`]: local intents into its queue, the rest into `out`
/// (the region's outbox, taken after each tick).
pub(crate) struct RegionEffects<'a> {
    pub data: &'a mut Rollback,
    pub out: &'a mut Vec<IntentMsg>,
}

impl Effects for RegionEffects<'_> {
    fn send(&mut self, target: Target, intent: Intent) {
        self.data.send_intent_safe(target, intent, self.out);
    }
}

impl Rollback {
    /// The replies delivered to `id` at this tick's opening, in order.
    pub fn intent_inbox(&self, id: EntityId) -> Vec<IntentReply> {
        self.data.intent_inbox.range((id, 0)..=(id, u32::MAX)).map(|(_, r)| *r).collect()
    }

    /// Intents queued here, in application order (tests, debugging).
    pub fn queued_intents(&self) -> Vec<SentIntent> {
        self.data.intents.values().copied().collect()
    }

    /// Replies queued here for delivery, in order (tests, debugging).
    pub fn queued_replies(&self) -> Vec<SentReply> {
        self.data.intent_replies.values().copied().collect()
    }

    /// Send `intent` at `target`, due at the next tick: queued here when this
    /// region owns the target, else into `out`.
    pub(crate) fn send_intent_safe(&mut self, target: Target, intent: Intent, out: &mut Vec<IntentMsg>) {
        let from = *self.data.region;
        let sent = SentIntent { target, intent, from, due: *self.data.tick + 1 };
        let here = match target {
            Target::Entity(id) => self.data.id_index.contains_key(&id),
            _ => target.region() == Some(from),
        };
        if here {
            self.queue_intent_safe(sent);
        } else {
            out.push(IntentMsg::Intent(sent));
        }
    }

    fn queue_intent_safe(&mut self, sent: SentIntent) {
        let (due, by) = (sent.due, sent.intent.by());
        let seq = self.data.intents.range((due, by, 0)..=(due, by, u32::MAX)).next_back().map_or(0, |(k, _)| k.2 + 1);
        self.data.intents.insert((due, by, seq), sent);
    }

    fn queue_reply_safe(&mut self, sent: SentReply) {
        let (due, to) = (sent.due, sent.reply.to());
        let last = self.data.intent_replies.range((due, to, 0)..=(due, to, u32::MAX)).next_back();
        let seq = last.map_or(0, |(k, _)| k.2 + 1);
        self.data.intent_replies.insert((due, to, seq), sent);
    }

    /// A [`crate::GameEventKind::Intent`] reached this region: into its
    /// queue (applied / delivered at the opening of its due tick, or the
    /// next tick when that has passed).
    pub(crate) fn receive_intent_safe(&mut self, msg: IntentMsg) {
        match msg {
            IntentMsg::Intent(sent) => self.queue_intent_safe(sent),
            IntentMsg::Reply(sent) => self.queue_reply_safe(sent),
        }
    }

    /// The opening of a tick: last tick's inbox empties, due replies are
    /// delivered, then due intents apply in `(due, source, seq)` order. Their
    /// replies go out due one tick later (into `out` unless the source is
    /// here).
    pub(crate) fn tick_intents_safe(&mut self, out: &mut Vec<IntentMsg>) {
        let now = *self.data.tick;
        let stale: Vec<_> = self.data.intent_inbox.keys().copied().collect();
        for k in stale {
            self.data.intent_inbox.remove(&k);
        }
        let due: Vec<_> = self.data.intent_replies.keys().copied().take_while(|k| k.0 <= now).collect();
        for k in due {
            let Some(sent) = self.data.intent_replies.remove(&k) else { continue };
            let to = sent.reply.to();
            if to == EntityId::of_region(*self.data.region) {
                self.region_reply_safe(sent.reply);
                continue;
            }
            if !self.data.id_index.contains_key(&to) {
                // Gone (parked, despawned, or arrived elsewhere): sent on for
                // the manager's directory to find.
                log::debug!("[intents] reply for {to:?} sent on from {:?}: it is not here", *self.data.region);
                out.push(IntentMsg::Reply(SentReply { due: now + 1, hops: sent.hops.saturating_add(1), ..sent }));
                continue;
            }
            let seq = self.data.intent_inbox.range((to, 0)..=(to, u32::MAX)).next_back().map_or(0, |(k, _)| k.1 + 1);
            self.data.intent_inbox.insert((to, seq), sent.reply);
        }
        let due: Vec<_> = self.data.intents.keys().copied().take_while(|k| k.0 <= now).collect();
        for k in due {
            let Some(sent) = self.data.intents.remove(&k) else { continue };
            // One that belongs elsewhere (restored from a save that owed it)
            // goes on to its owner.
            if sent.target.region().is_some_and(|rc| rc != *self.data.region) {
                out.push(IntentMsg::Intent(sent));
                continue;
            }
            let reply = self.apply_intent_safe(sent.target, sent.intent);
            self.send_reply_safe(SentReply { reply, hint: sent.from, due: now + 1, hops: 0 }, out);
        }
    }

    /// A reply from here: queued if its entity (or this region, for a
    /// region-sourced intent) is here, else out.
    fn send_reply_safe(&mut self, msg: SentReply, out: &mut Vec<IntentMsg>) {
        let to = msg.reply.to();
        if self.data.id_index.contains_key(&to) || to == EntityId::of_region(*self.data.region) {
            self.queue_reply_safe(msg);
        } else {
            out.push(IntentMsg::Reply(msg));
        }
    }

    /// `id` departs to `target` this tick: the replies queued here for it go
    /// with it, addressed there, due unchanged.
    pub(crate) fn forward_replies_safe(&mut self, id: EntityId, target: RegionCoords, out: &mut Vec<IntentMsg>) {
        let keys: Vec<_> = self.data.intent_replies.keys().copied().filter(|k| k.1 == id).collect();
        for k in keys {
            if let Some(sent) = self.data.intent_replies.remove(&k) {
                out.push(IntentMsg::Reply(SentReply { hint: target, ..sent }));
            }
        }
    }

    /// The region parks: every intent still queued here is refused
    /// `OwnerNotRunning`, so a parked region holds no unanswered intent.
    pub(crate) fn refuse_queued_intents_safe(&mut self, out: &mut Vec<IntentMsg>) {
        let now = *self.data.tick;
        let keys: Vec<_> = self.data.intents.keys().copied().collect();
        for k in keys {
            let Some(sent) = self.data.intents.remove(&k) else { continue };
            let reply = IntentReply::Refused(sent.intent, IntentRefusal::OwnerNotRunning);
            self.send_reply_safe(SentReply { reply, hint: sent.from, due: now + 1, hops: 0 }, out);
        }
    }

    /// Apply one intent as the target's owner. The owner's own refusals
    /// decide; implied ground it opens is materialised by `set_block_in`
    /// (its own), and the neighbour's behind a seam by that neighbour when
    /// the halo delta reaches it (`halo.rs`).
    fn apply_intent_safe(&mut self, target: Target, intent: Intent) -> IntentReply {
        let refused = |why| IntentReply::Refused(intent, why);
        let p = match (target, intent) {
            (Target::Pile(p), Intent::DepositItems { .. } | Intent::TakeFromPile { .. }) => {
                if p.to_local().0 != *self.data.region {
                    log::warn!("[intents] {intent:?} for {p:?} reached {:?}, not its owner", *self.data.region);
                    return refused(IntentRefusal::BadTarget);
                }
                return self.apply_pile_intent_safe(p, intent);
            }
            (Target::Container(p), Intent::PutInContainer { .. } | Intent::TakeFromContainer { .. }) => {
                if p.to_local().0 != *self.data.region {
                    log::warn!("[intents] {intent:?} for {p:?} reached {:?}, not its owner", *self.data.region);
                    return refused(IntentRefusal::BadTarget);
                }
                return self.apply_container_intent_safe(p, intent);
            }
            (
                Target::Stud(c),
                Intent::CarveStud { .. }
                | Intent::PlaceStud { .. }
                | Intent::FillStud { .. }
                | Intent::RemoveStud { .. }
                | Intent::BreakStud { .. }
                | Intent::PlaceBrick { .. },
            ) => {
                if c.to_local().0 != *self.data.region {
                    log::warn!("[intents] {intent:?} for {c:?} reached {:?}, not its owner", *self.data.region);
                    return refused(IntentRefusal::BadTarget);
                }
                return self.apply_stud_intent_safe(intent);
            }
            (Target::Block(p), Intent::BreakBlock { .. } | Intent::PlaceBlock { .. }) => p,
            _ => return refused(IntentRefusal::BadTarget),
        };
        let (rc, local) = p.to_local();
        if rc != *self.data.region {
            log::warn!("[intents] {intent:?} for {p:?} reached {:?}, not its owner", *self.data.region);
            return refused(IntentRefusal::BadTarget);
        }
        let pos = [local.x, local.y, local.z];
        // The cell is a TILE (`tiles.rs`).
        self.apply_stud_tile_intent_safe(pos, intent)
    }

    /// `Intent::BreakBlock` / `Intent::PlaceBlock` at stud TILE `p` of this region, as
    /// its owner — what a player's mining, placing and creative edit send
    /// (`mining.rs`, `edit.rs`), with these refusals:
    /// - a break carves the tile's box (`carve_stud_safe`); `survival` puts what it
    ///   removed down as piles in the tile (`mining::stud_drops`) and, on a tile
    ///   holding tree wood, fells the tree instead (`fell_stud_safe`, its yield at
    ///   the stump, the falling part dropping its items where it rests). An open
    ///   tile is `NothingThere`, a bedrock one `Bedrock`.
    /// - a place fills the tile's empty cells with the block's stud material
    ///   (`stud_material_of_block`; the fortress Build rule), or puts a chest into an
    ///   open tile for the chest block (`place_stud_chest_safe`). A solid tile, a
    ///   body in the tile or a chest that does not fit is `Occupied`.
    fn apply_stud_tile_intent_safe(&mut self, p: crate::TilePos, intent: Intent) -> IntentReply {
        let refused = |why| IntentReply::Refused(intent, why);
        let (Some((lo, hi)), Some(_)) = (crate::stud_tile_box(p), crate::split_stud_tile(p)) else {
            return refused(IntentRefusal::BadTarget);
        };
        let tile = self.stud_tile(p);
        let done = match intent {
            Intent::BreakBlock { survival, .. } => {
                if tile.fill() == crate::TileFill::Open {
                    return refused(IntentRefusal::NothingThere);
                }
                if tile.is_bedrock() {
                    return refused(IntentRefusal::Bedrock);
                }
                if survival && self.stud_tree_at(p).is_some() && self.fell_stud_safe(p, crate::SettlePolicy::DropItems).is_some() {
                    true
                } else if survival {
                    self.carve_stud_boxes_dropping_safe(&[(lo, hi)]).pop().is_some_and(|r| !r.is_empty())
                } else {
                    !self.carve_stud_safe(lo, hi).is_empty()
                }
            }
            Intent::PlaceBlock { block, .. } => {
                if tile.fill() == crate::TileFill::Solid || self.stud_tile_overlaps_body(p) {
                    return refused(IntentRefusal::Occupied);
                }
                let placed = if crate::chest_block() == Some(block) {
                    self.place_stud_chest_safe(p)
                } else {
                    self.fill_stud_box_safe(lo, hi, crate::stud_material_of_block(block)) > 0
                };
                if !placed {
                    return refused(IntentRefusal::Occupied);
                }
                true
            }
            _ => return refused(IntentRefusal::BadTarget),
        };
        if done { IntentReply::Granted(intent) } else { refused(IntentRefusal::NothingThere) }
    }

    /// A stud intent as the owner (the caller checked the target is here).
    fn apply_stud_intent_safe(&mut self, intent: Intent) -> IntentReply {
        let refused = |why| IntentReply::Refused(intent, why);
        let region = *self.data.region;
        let cell = |c: [i64; 3]| -> Option<[i32; 3]> {
            Some([i32::try_from(c[0]).ok()?, i32::try_from(c[1]).ok()?, i32::try_from(c[2]).ok()?])
        };
        match intent {
            Intent::FillStud { lo, hi, material, .. } => {
                let (lo, hi) = (crate::local_stud_cell(region, lo), crate::local_stud_cell(region, hi));
                let (Some(lo), Some(hi)) = (cell(lo), cell(hi)) else { return refused(IntentRefusal::BadTarget) };
                if self.fill_stud_box_safe(lo, hi, material) > 0 {
                    IntentReply::Granted(intent)
                } else {
                    refused(IntentRefusal::Occupied)
                }
            }
            Intent::CarveStud { lo, hi, .. } => {
                let (lo, hi) = (crate::local_stud_cell(region, lo), crate::local_stud_cell(region, hi));
                let (Some(lo), Some(hi)) = (cell(lo), cell(hi)) else { return refused(IntentRefusal::BadTarget) };
                // What the carve removes goes into this region's piles (the owner's,
                // as a mined block's does) — never back across the seam.
                let removed = self.carve_stud_boxes_dropping_safe(&[(lo, hi)]).pop().unwrap_or_default();
                if removed.is_empty() { refused(IntentRefusal::NothingThere) } else { IntentReply::Granted(intent) }
            }
            Intent::PlaceStud { chunk, brick, .. } => {
                let c = chunk.to_local().1;
                if self.place_stud_safe(crate::StudChunkCoords { x: c.x, y: c.y, z: c.z }, brick) {
                    IntentReply::Granted(intent)
                } else {
                    refused(IntentRefusal::Occupied)
                }
            }
            Intent::RemoveStud { chunk, cell, .. } => {
                let c = chunk.to_local().1;
                if self.remove_stud_safe(crate::StudChunkCoords { x: c.x, y: c.y, z: c.z }, cell) {
                    IntentReply::Granted(intent)
                } else {
                    refused(IntentRefusal::NothingThere)
                }
            }
            Intent::BreakStud { lo, hi, survival, .. } => {
                let (lo, hi) = (crate::local_stud_cell(region, lo), crate::local_stud_cell(region, hi));
                let (Some(lo), Some(hi)) = (cell(lo), cell(hi)) else { return refused(IntentRefusal::BadTarget) };
                let Some(at) = crate::StudBox::of_cells(lo, hi) else { return refused(IntentRefusal::BadTarget) };
                // What is in the box: tree wood, bedrock, anything diggable.
                let (mut wood, mut bedrock, mut diggable) = (None, false, false);
                for b in self.stud_bricks_meeting(at) {
                    if b.tree != 0 {
                        wood.get_or_insert(b.at);
                    }
                    if b.bedrock { bedrock = true } else { diggable = true }
                }
                if !diggable {
                    return refused(if bedrock { IntentRefusal::Bedrock } else { IntentRefusal::NothingThere });
                }
                let fell = |b: crate::StudBox| b.tiles().min;
                let done = if survival && wood.is_some_and(|b| self.fell_stud_safe(fell(b), crate::SettlePolicy::DropItems).is_some()) {
                    true
                } else if survival {
                    self.carve_stud_boxes_dropping_safe(&[(lo, hi)]).pop().is_some_and(|r| !r.is_empty())
                } else {
                    !self.carve_stud_safe(lo, hi).is_empty()
                };
                if done { IntentReply::Granted(intent) } else { refused(IntentRefusal::NothingThere) }
            }
            Intent::PlaceBrick { chunk, brick, .. } => {
                let c = chunk.to_local().1;
                let c = crate::StudChunkCoords { x: c.x, y: c.y, z: c.z };
                if self.stud_brick_overlaps_body([c.x as i32, c.y, c.z as i32], &brick) {
                    return refused(IntentRefusal::Occupied);
                }
                // Implied ground there becomes its bricks first, so the brick joins them.
                if self.stud_key_at(c).is_none() {
                    let pristine = self.data.stud_ground.get(&crate::ColumnCoords::new(c.x, c.z)).and_then(|g| g.pristine_chunk(c.y));
                    if let Some(chunk) = pristine {
                        if chunk.clone().place(brick).is_err() {
                            return refused(IntentRefusal::Occupied);
                        }
                        self.create_stud_chunk_safe(c, chunk);
                    }
                }
                if self.place_stud_safe(c, brick) { IntentReply::Granted(intent) } else { refused(IntentRefusal::Occupied) }
            }
            _ => refused(IntentRefusal::BadTarget),
        }
    }

    /// Who a client's stud edit is sent `by`: its body here, else this region (a
    /// commander has no body; the reply then comes back to the region, which drops it).
    pub(crate) fn stud_edit_source(&self, client: ClientId) -> EntityId {
        let body = self.data.player_entites.get(&client).and_then(|&k| self.entity_id(k));
        body.unwrap_or_else(|| EntityId::of_region(*self.data.region))
    }

    /// `Order::CarveStud`: this region's part of the box is carved now
    /// (`carve_stud_safe`); the part past each edge goes to the neighbour owning it
    /// as an `Intent::CarveStud`, applied there at the next tick — its halo delta
    /// brings the hole back here. An empty or oversized box does nothing anywhere.
    pub(crate) fn carve_stud_order_safe(&mut self, client: ClientId, lo: [i32; 3], hi: [i32; 3], out: &mut Vec<IntentMsg>) {
        self.stud_box_order_safe(client, lo, hi, None, out);
    }

    /// `Order::FillStud`: as [`Self::carve_stud_order_safe`], filling the box's empty
    /// cells with `material` (`fill_stud_box_safe` here, `Intent::FillStud` to each
    /// neighbour the box reaches into). An unknown material does nothing anywhere.
    pub(crate) fn fill_stud_order_safe(
        &mut self,
        client: ClientId,
        lo: [i32; 3],
        hi: [i32; 3],
        material: u16,
        out: &mut Vec<IntentMsg>,
    ) {
        if crate::stud_material(material).is_some() {
            self.stud_box_order_safe(client, lo, hi, Some(material), out);
        }
    }

    /// A box edit of stud cells ordered here: carved (`fill` `None`) or filled with a
    /// material, this region's part at once and each neighbour's part as an intent.
    fn stud_box_order_safe(
        &mut self,
        client: ClientId,
        lo: [i32; 3],
        hi: [i32; 3],
        fill: Option<u16>,
        out: &mut Vec<IntentMsg>,
    ) {
        let (lo, hi) = (lo.map(i64::from), hi.map(i64::from));
        if (0..3).any(|i| hi[i] <= lo[i] || hi[i] - lo[i] > crate::stud_ops::STUD_CARVE_MAX[i] as i64) {
            return;
        }
        let n = crate::REGION_STUDS;
        let region = *self.data.region;
        let by = self.stud_edit_source(client);
        match fill {
            None => {
                self.carve_stud_boxes_dropping_safe(&[(lo.map(|v| v as i32), hi.map(|v| v as i32))]);
            }
            Some(material) => {
                self.fill_stud_box_safe(lo.map(|v| v as i32), hi.map(|v| v as i32), material);
            }
        }
        for dx in -1..=1i64 {
            for dz in -1..=1i64 {
                if (dx, dz) == (0, 0) {
                    continue;
                }
                // The box clipped to that neighbour's cells, in this region's frame.
                let l = [lo[0].max(dx * n), lo[1], lo[2].max(dz * n)];
                let h = [hi[0].min(dx * n + n), hi[1], hi[2].min(dz * n + n)];
                if l[0] >= h[0] || l[2] >= h[2] {
                    continue;
                }
                let (wl, wh) = (crate::world_stud_cell(region, l), crate::world_stud_cell(region, h));
                let target = Target::Stud(crate::stud_chunk_of_cell(wl));
                let intent = match fill {
                    None => Intent::CarveStud { lo: wl, hi: wh, by },
                    Some(material) => Intent::FillStud { lo: wl, hi: wh, material, by },
                };
                RegionEffects { data: self, out }.send(target, intent);
            }
        }
    }

    /// `Order::PlaceStudAt`: placed now when this region owns the chunk, else sent
    /// to its owner as an `Intent::PlaceStud` (applied there at the next tick).
    pub(crate) fn place_stud_order_safe(
        &mut self,
        client: ClientId,
        chunk: crate::WorldChunkPos,
        brick: crate::StudBrick,
        out: &mut Vec<IntentMsg>,
    ) {
        let (rc, c) = chunk.to_local();
        if rc == *self.data.region {
            self.place_stud_safe(crate::StudChunkCoords { x: c.x, y: c.y, z: c.z }, brick);
            return;
        }
        if !matches!(brick.shape, crate::StudShape::Part { .. }) || !crate::stud::stud_layer_ok(chunk.y) {
            return;
        }
        let by = self.stud_edit_source(client);
        RegionEffects { data: self, out }.send(Target::Stud(chunk), Intent::PlaceStud { chunk, brick, by });
    }

    /// `Order::RemoveStudAt`: removed now when this region owns the chunk, else sent
    /// to its owner as an `Intent::RemoveStud` (applied there at the next tick; its
    /// halo delta brings the gap back here).
    pub(crate) fn remove_stud_order_safe(
        &mut self,
        client: ClientId,
        chunk: crate::WorldChunkPos,
        cell: [u8; 3],
        out: &mut Vec<IntentMsg>,
    ) {
        let (rc, c) = chunk.to_local();
        if rc == *self.data.region {
            self.remove_stud_safe(crate::StudChunkCoords { x: c.x, y: c.y, z: c.z }, cell);
            return;
        }
        if !crate::stud::stud_layer_ok(chunk.y) {
            return;
        }
        let by = self.stud_edit_source(client);
        RegionEffects { data: self, out }.send(Target::Stud(chunk), Intent::RemoveStud { chunk, cell, by });
    }

    /// `Order::DevBreakBlock`: `client`'s body sends `BreakBlock` at `p`.
    pub(crate) fn dev_break_block_safe(&mut self, client: ClientId, p: WorldBlockPos, out: &mut Vec<IntentMsg>) {
        let Some(by) = self.data.player_entites.get(&client).and_then(|&k| self.entity_id(k)) else { return };
        RegionEffects { data: self, out }.send(Target::Block(p), Intent::BreakBlock { by, survival: false });
    }
}
