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
//! **Never lost.** An intent whose owner is not running — or is stopping —
//! is answered `Refused(.., OwnerNotRunning)` by the manager (an intent never
//! wakes a region). A region that parks refuses the intents still queued in
//! it the same way. Queued replies are region state, so they park with it; a
//! reply for a parked or stopping region waits in the manager's mailbox for
//! its next run (and is saved with it). Intents and replies in flight
//! between regions are saved exactly once per autosave round (the manager's
//! `in_flight_intents`, injected into the saved state that lacks them).
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
            Intent::BreakBlock { by, .. } | Intent::PlaceBlock { by, .. } => *by,
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

    /// Put intents / replies that were in flight to (or owed by) this region
    /// into its queues, UNLOGGED: for a saved clone only (the manager's
    /// autosave, `WorldManager::in_flight_intents`), never a live region.
    /// Restored, they apply / deliver at its first tick — or are sent on, if
    /// they belong elsewhere.
    pub fn insert_saved_intents(&mut self, msgs: &[IntentMsg]) {
        for msg in msgs {
            match *msg {
                IntentMsg::Intent(sent) => {
                    let (due, by) = (sent.due, sent.intent.by());
                    let last = self.data.intents.range((due, by, 0)..=(due, by, u32::MAX)).next_back();
                    let seq = last.map_or(0, |(k, _)| k.2 + 1);
                    self.data.intents.insert_raw((due, by, seq), sent);
                }
                IntentMsg::Reply(sent) => {
                    let (due, to) = (sent.due, sent.reply.to());
                    let last = self.data.intent_replies.range((due, to, 0)..=(due, to, u32::MAX)).next_back();
                    let seq = last.map_or(0, |(k, _)| k.2 + 1);
                    self.data.intent_replies.insert_raw((due, to, seq), sent);
                }
            }
        }
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

    /// A reply from here: queued if its entity is here, else out.
    fn send_reply_safe(&mut self, msg: SentReply, out: &mut Vec<IntentMsg>) {
        if self.data.id_index.contains_key(&msg.reply.to()) {
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
        let Target::Block(p) = target else { return refused(IntentRefusal::BadTarget) };
        let (rc, local) = p.to_local();
        if rc != *self.data.region {
            log::warn!("[intents] {intent:?} for {p:?} reached {:?}, not its owner", *self.data.region);
            return refused(IntentRefusal::BadTarget);
        }
        let pos = [local.x, local.y, local.z];
        let Some((coords, l)) = crate::edit::locate(pos) else { return refused(IntentRefusal::NothingThere) };
        let current = self.current_block(coords, l);
        let ground = self.data.ground.get(&crate::ColumnCoords::new(coords.x, coords.z));
        if current.is_some_and(|b| crate::edit::is_bedrock_at(ground, b)) {
            return refused(IntentRefusal::Bedrock);
        }
        let done = match intent {
            Intent::BreakBlock { survival: false, .. } => self.set_block_in(pos, crate::Block::new(crate::BlockId::AIR)),
            Intent::BreakBlock { survival: true, .. } => {
                let tick = *self.data.tick;
                match self.mineable(pos) {
                    Some(block) if block.tree_id() != 0 => self.player_chop_safe(pos, tick),
                    Some(block) => self.mine_block_safe(pos, block, tick),
                    None => false,
                }
            }
            Intent::PlaceBlock { block, .. } => {
                if self.block_solid(pos) || self.block_overlaps_body(pos) {
                    return refused(IntentRefusal::Occupied);
                }
                self.set_block_in(pos, crate::Block::new(block))
            }
        };
        if done {
            IntentReply::Granted(intent)
        } else {
            refused(IntentRefusal::NothingThere)
        }
    }

    /// `Order::DevBreakBlock`: `client`'s body sends `BreakBlock` at `p`.
    pub(crate) fn dev_break_block_safe(&mut self, client: ClientId, p: WorldBlockPos, out: &mut Vec<IntentMsg>) {
        let Some(by) = self.data.player_entites.get(&client).and_then(|&k| self.entity_id(k)) else { return };
        RegionEffects { data: self, out }.send(Target::Block(p), Intent::BreakBlock { by, survival: false });
    }
}
