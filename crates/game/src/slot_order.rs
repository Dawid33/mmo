//! One canonical order for the messages a region's neighbours send it within a
//! tick's slot (design: `.superpowers/sdd/2026-11-13-seam-slot-order/design.md`).
//!
//! Runner state, not simulation state: never hashed, saved or rolled back. A
//! region's runner buffers every region-to-region message here and, right
//! before each `Tick`, applies the ones whose stamp has come, sorted by
//! `(stamp, class, key, arrival)` — so thread timing never decides the order.
//! A halo seed (`HaloUpdate { replace: true }`) has its own class, after
//! `HaloSourceGone` ([`SlotClass::HaloSeed`]; amended in execution).

use std::collections::VecDeque;

use crate::{EntityId, GameEventKind, Intent, IntentMsg, IntentRefusal, IntentReply, RegionCoords, Tick};

/// Where a message sorts within its stamp. Within a class, by the message's own
/// key, then arrival (one FIFO per source, so equal keys keep their source's order):
/// - `Arrival`: the entity's id;
/// - `HaloEntityGone` / `HaloEntity`: the source region, then the entity's id;
/// - `HaloUpdate` (a delta), `HaloSourceGone`, `HaloSeed`: the source region;
/// - `Intent`: intents before replies; an intent by its sending entity (`by`), a
///   reply by the entity it is for, then the region that answered it (the owner
///   of its intent's position), then — replies only — its content (`reply`,
///   `hops`, `hint`; amended in execution: replies to `BreakBlock` /
///   `PlaceBlock` carry no position, and a reply relayed through the entity's old
///   region can tie with a direct one from the same owner). So messages of two
///   sources never tie; equal keys are identical replies or one sender's intents,
///   which keep arrival order (a sender's emission order carries meaning: break
///   then place);
/// - `Other`: arrival only.
///
/// A follower of a buffered arrival ([`SlotInbox::push_after_arrival`]: a
/// client's input or server event for the arriving body) takes its arrival's
/// stamp, class and key and sorts directly after it, followers in arrival
/// order — before the slot's other messages (amended in execution: one
/// position on both sides, the one a crossing body's carried inputs have).
/// A follower of a held creation (itself `Other`) is pushed `Other` behind it
/// ([`SlotInbox::push_behind`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SlotClass {
    Arrival = 0,
    HaloEntityGone = 1,
    HaloEntity = 2,
    /// A `HaloUpdate` delta (`replace: false`).
    HaloUpdate = 3,
    HaloSourceGone = 4,
    /// A `HaloUpdate` seed (`replace: true`), after the stamp's
    /// `HaloSourceGone` (amended in execution): at one stamp a lost
    /// incarnation's last deltas precede the end of its mirrors, and a
    /// respawned incarnation's seed (answered before its first tick, stamped
    /// with its start tick, as the manager's `HaloSourceGone`) follows it.
    /// The stamp's entity updates still sort before a seed, which drops the
    /// source's entity mirrors (its next tick re-sends them). Accepted cost:
    /// an old incarnation's seed answered in the very slot it died also lands
    /// after its Gone (its piles and chests mirrored, nobody answering them,
    /// until the respawn's seed replaces them).
    HaloSeed = 5,
    Intent = 6,
    /// Anything else, and anything queued behind a held creation
    /// ([`SlotInbox::push_behind`]); followers of an arrival sort with the
    /// arrival ([`SlotInbox::push_after_arrival`]).
    Other = 7,
}

/// The class of a message kind.
///
/// No `_` arm, here or in [`key_of`]: a new `GameEventKind` must be classified
/// (a new region-to-region message needs its own class and key, or two
/// sources' copies of it would sort by arrival).
pub fn slot_class(kind: &GameEventKind) -> SlotClass {
    match kind {
        GameEventKind::EntityArrived(_) => SlotClass::Arrival,
        GameEventKind::HaloEntityGone(_) => SlotClass::HaloEntityGone,
        GameEventKind::HaloEntity(_) => SlotClass::HaloEntity,
        GameEventKind::HaloUpdate(u) if u.replace => SlotClass::HaloSeed,
        GameEventKind::HaloUpdate(_) => SlotClass::HaloUpdate,
        GameEventKind::HaloSourceGone(_) => SlotClass::HaloSourceGone,
        GameEventKind::Intent(_) => SlotClass::Intent,
        // Not region-to-region messages (manager directives, client events).
        GameEventKind::Tick
        | GameEventKind::PlayerInput(..)
        | GameEventKind::CreateClient(_)
        | GameEventKind::Quit
        | GameEventKind::UiAction(..)
        | GameEventKind::Order(..)
        | GameEventKind::ClientLeft(_)
        | GameEventKind::CreateClientAt(..)
        | GameEventKind::SpawnPerson(_)
        | GameEventKind::DespawnPerson(_)
        | GameEventKind::WalkOut(..)
        | GameEventKind::ParkPlayer(_)
        | GameEventKind::RestorePlayer(..)
        | GameEventKind::PlaceBody(..) => SlotClass::Other,
    }
}

type Key = [i64; 6];

fn region_key(r: RegionCoords) -> [i64; 2] {
    [r.x as i64, r.z as i64]
}

/// Order-preserving `u64` -> `i64`.
fn serial_key(serial: u64) -> i64 {
    (serial ^ (1u64 << 63)) as i64
}

fn id_key(id: EntityId) -> [i64; 3] {
    let [x, z] = region_key(id.birth_region);
    [x, z, serial_key(id.serial)]
}

fn key_of(kind: &GameEventKind) -> Key {
    match kind {
        GameEventKind::EntityArrived(b) => {
            let [a, b, c] = id_key(b.id);
            [a, b, c, 0, 0, 0]
        }
        GameEventKind::HaloEntityGone(g) => {
            let [fx, fz] = region_key(g.from);
            let [a, b, c] = id_key(g.id);
            [fx, fz, a, b, c, 0]
        }
        GameEventKind::HaloEntity(d) => {
            let [sx, sz] = region_key(d.source_region);
            let [a, b, c] = id_key(d.id);
            [sx, sz, a, b, c, 0]
        }
        GameEventKind::HaloUpdate(u) => {
            let [fx, fz] = region_key(u.from);
            [fx, fz, 0, 0, 0, 0]
        }
        GameEventKind::HaloSourceGone(r) => {
            let [x, z] = region_key(*r);
            [x, z, 0, 0, 0, 0]
        }
        GameEventKind::Intent(IntentMsg::Intent(sent)) => {
            let [a, b, c] = id_key(sent.intent.by());
            [INTENT_KEY, a, b, c, 0, 0]
        }
        GameEventKind::Intent(IntentMsg::Reply(sent)) => {
            let [a, b, c] = id_key(sent.reply.to());
            let [ox, oz] = reply_owner(&sent.reply).map_or([i64::MIN; 2], region_key);
            [REPLY_KEY, a, b, c, ox, oz]
        }
        // Class `Other` ([`slot_class`]): arrival order only.
        GameEventKind::Tick
        | GameEventKind::PlayerInput(..)
        | GameEventKind::CreateClient(_)
        | GameEventKind::Quit
        | GameEventKind::UiAction(..)
        | GameEventKind::Order(..)
        | GameEventKind::ClientLeft(_)
        | GameEventKind::CreateClientAt(..)
        | GameEventKind::SpawnPerson(_)
        | GameEventKind::DespawnPerson(_)
        | GameEventKind::WalkOut(..)
        | GameEventKind::ParkPlayer(_)
        | GameEventKind::RestorePlayer(..)
        | GameEventKind::PlaceBody(..) => [0; 6],
    }
}

/// Class `Intent`'s first key: intents before replies.
const INTENT_KEY: i64 = 0;
const REPLY_KEY: i64 = 1;

/// The region that answered `reply`: the owner of the position its intent named
/// (`Target::region` of the intent it answers) — `None` for `BreakBlock` /
/// `PlaceBlock`, whose position travels only in the `SentIntent`.
fn reply_owner(reply: &IntentReply) -> Option<RegionCoords> {
    let (IntentReply::Granted(intent) | IntentReply::Refused(intent, _)) = reply;
    match *intent {
        Intent::BreakBlock { .. } | Intent::PlaceBlock { .. } => None,
        Intent::DepositItems { cell, .. }
        | Intent::TakeFromPile { cell, .. }
        | Intent::PutInContainer { cell, .. }
        | Intent::TakeFromContainer { cell, .. } => Some(cell.to_local().0),
    }
}

/// A reply's content, compared after the key (derived `Ord` on plain integer
/// fields: the same on every platform). `refusal` is `None` for a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ReplyTie {
    intent: Intent,
    refusal: Option<IntentRefusal>,
    hops: u8,
    hint: RegionCoords,
}

fn reply_tie(kind: &GameEventKind) -> Option<ReplyTie> {
    let GameEventKind::Intent(IntentMsg::Reply(sent)) = kind else { return None };
    let (intent, refusal) = match sent.reply {
        IntentReply::Granted(i) => (i, None),
        IntentReply::Refused(i, r) => (i, Some(r)),
    };
    Some(ReplyTie { intent, refusal, hops: sent.hops, hint: sent.hint })
}

struct Entry {
    stamp: Tick,
    class: SlotClass,
    key: Key,
    /// Replies only (`None` for every other entry, and for `push_behind`).
    tie: Option<ReplyTie>,
    /// 1 for a follower of an arrival (`push_after_arrival`): after the
    /// arrival, which shares its stamp, class and key; 0 otherwise.
    follower: u8,
    arrival: u64,
    kind: GameEventKind,
}

/// A region's buffered neighbour messages, drained in canonical order.
#[derive(Default)]
pub struct SlotInbox {
    entries: VecDeque<Entry>,
    next_arrival: u64,
}

impl SlotInbox {
    pub fn new() -> Self {
        Self::default()
    }

    fn add(&mut self, kind: GameEventKind, stamp: Tick, class: SlotClass, key: Key, tie: Option<ReplyTie>) {
        self.add_entry(kind, stamp, class, key, tie, 0);
    }

    fn add_entry(&mut self, kind: GameEventKind, stamp: Tick, class: SlotClass, key: Key, tie: Option<ReplyTie>, follower: u8) {
        let arrival = self.next_arrival;
        self.next_arrival += 1;
        self.entries.push_back(Entry { stamp, class, key, tie, follower, arrival, kind });
    }

    /// Buffer a message; its class and key come from the kind.
    pub fn push(&mut self, kind: GameEventKind, stamp: Tick) {
        let (class, key, tie) = (slot_class(&kind), key_of(&kind), reply_tie(&kind));
        self.add(kind, stamp, class, key, tie);
    }

    /// Buffer an entry that must follow the others of its stamp (class `Other`,
    /// arrival order among its peers), whatever its kind.
    pub fn push_behind(&mut self, kind: GameEventKind, stamp: Tick) {
        self.add(kind, stamp, SlotClass::Other, [0; 6], None);
    }

    /// Buffer an entry that must follow the arrival of `entity` stamped
    /// `stamp`: directly after that arrival, before the slot's other
    /// messages; followers of one arrival keep their arrival order.
    pub fn push_after_arrival(&mut self, kind: GameEventKind, stamp: Tick, entity: EntityId) {
        let [a, b, c] = id_key(entity);
        self.add_entry(kind, stamp, SlotClass::Arrival, [a, b, c, 0, 0, 0], None, 1);
    }

    fn drain_where(&mut self, take: impl Fn(Tick) -> bool) -> Vec<GameEventKind> {
        let mut out = Vec::new();
        let mut keep = VecDeque::with_capacity(self.entries.len());
        for e in self.entries.drain(..) {
            if take(e.stamp) {
                out.push(e);
            } else {
                keep.push_back(e);
            }
        }
        self.entries = keep;
        // `arrival` is unique, so the order is total (and a stable sort is moot).
        out.sort_by_key(|e| (e.stamp, e.class, e.key, e.tie, e.follower, e.arrival));
        out.into_iter().map(|e| e.kind).collect()
    }

    /// Every entry with `stamp <= now`, canonical order; the rest stay.
    pub fn drain_due(&mut self, now: Tick) -> Vec<GameEventKind> {
        self.drain_where(|s| s <= now)
    }

    /// Every entry with `stamp < tick`, canonical order; the rest stay.
    pub fn drain_before(&mut self, tick: Tick) -> Vec<GameEventKind> {
        self.drain_where(|s| s < tick)
    }

    /// Everything, canonical order.
    pub fn drain_all(&mut self) -> Vec<GameEventKind> {
        self.drain_where(|_| true)
    }

    pub fn latest_stamp(&self) -> Option<Tick> {
        self.entries.iter().map(|e| e.stamp).max()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The stamp of the first buffered entry (in arrival order) matching `pred`.
    pub fn holds(&self, pred: impl Fn(&GameEventKind) -> bool) -> Option<Tick> {
        self.holds_entry(pred).map(|(stamp, _)| stamp)
    }

    /// The stamp and kind of the first buffered entry (in arrival order)
    /// matching `pred`: what a follower needs to address it.
    pub fn holds_entry(&self, pred: impl Fn(&GameEventKind) -> bool) -> Option<(Tick, &GameEventKind)> {
        self.entries.iter().find(|e| pred(&e.kind)).map(|e| (e.stamp, &e.kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BodyState, Carried, Client, ColliderSpec, EntityBundle, EntityKind, HaloEntityDelta, HaloEntityGone,
        HaloUpdate, InputEvent, IntentMsg, Pose, SentIntent, SentReply,
    };
    use rapier3d::math::Vector;

    fn rc(x: i32, z: i32) -> RegionCoords {
        RegionCoords::new(x, z)
    }

    fn eid(x: i32, z: i32, serial: u64) -> EntityId {
        EntityId { birth_region: rc(x, z), serial }
    }

    fn arrival(id: EntityId) -> GameEventKind {
        GameEventKind::EntityArrived(EntityBundle {
            id,
            kind: EntityKind::Player,
            body: BodyState::kinematic(Pose::translation(0.0, 0.0, 0.0)),
            collider: ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
            carried: Carried::default(),
            client: Some((7, Client::default())),
            pending_inputs: Vec::new(),
        })
    }

    fn gone(from: RegionCoords, id: EntityId) -> GameEventKind {
        GameEventKind::HaloEntityGone(HaloEntityGone { id, from })
    }

    fn update(src: RegionCoords, id: EntityId, x: f32) -> GameEventKind {
        GameEventKind::HaloEntity(HaloEntityDelta {
            id,
            source_region: src,
            kind: EntityKind::Player,
            isometry: Pose::translation(x, 0.0, 0.0),
            linvel: Vector::ZERO,
            collider: ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        })
    }

    fn halo(from: RegionCoords, replace: bool) -> GameEventKind {
        GameEventKind::HaloUpdate(HaloUpdate {
            from,
            replace,
            deltas: Vec::new(),
            ground: Vec::new(),
            piles: Vec::new(),
            containers: Vec::new(),
        })
    }

    fn intent() -> GameEventKind {
        GameEventKind::Intent(IntentMsg::Intent(SentIntent::default()))
    }

    /// A take from the pile at world block `x` (any owner) by `by`.
    fn take(x: i64, by: EntityId) -> Intent {
        Intent::TakeFromPile { cell: crate::WorldBlockPos::new(x, 0, 0), item: crate::ItemId(1), count: 1, by }
    }

    /// An intent by `by` taking from the pile at world block `x` (any owner).
    fn take_by(by: EntityId, x: i64) -> GameEventKind {
        GameEventKind::Intent(IntentMsg::Intent(SentIntent { intent: take(x, by), ..SentIntent::default() }))
    }

    /// A reply to `to` for a take from the pile at world block `x` (answered by its owner).
    fn take_reply(to: EntityId, x: i64) -> GameEventKind {
        GameEventKind::Intent(IntentMsg::Reply(SentReply {
            reply: IntentReply::Granted(take(x, to)),
            ..SentReply::default()
        }))
    }

    /// A granted `BreakBlock` reply to `to` (no position: owner `None`).
    fn break_reply(to: EntityId) -> GameEventKind {
        GameEventKind::Intent(IntentMsg::Reply(SentReply {
            reply: IntentReply::Granted(Intent::BreakBlock { by: to, survival: true }),
            ..SentReply::default()
        }))
    }

    fn input(client: u64) -> GameEventKind {
        GameEventKind::PlayerInput(client, InputEvent::Focused(true))
    }

    #[test]
    fn two_sources_sort_the_same_whatever_the_arrival_order() {
        let id = eid(0, 0, 1);
        let a = update(rc(-1, 0), id, 1.0);
        let b = update(rc(0, -1), id, 2.0);
        let mut one = SlotInbox::new();
        one.push(a.clone(), 5);
        one.push(b.clone(), 5);
        let mut two = SlotInbox::new();
        two.push(b, 5);
        two.push(a, 5);
        assert_eq!(one.drain_due(5), two.drain_due(5));
    }

    #[test]
    fn one_source_keeps_its_emission_order() {
        let src = rc(1, 0);
        let emitted = vec![
            arrival(eid(1, 0, 9)),
            gone(src, eid(1, 0, 8)),
            update(src, eid(1, 0, 2), 1.0),
            update(src, eid(1, 0, 3), 2.0),
            halo(src, false),
        ];
        let mut inbox = SlotInbox::new();
        for e in &emitted {
            inbox.push(e.clone(), 4);
        }
        assert_eq!(inbox.drain_due(4), emitted);
    }

    #[test]
    fn classes_order_arrival_gone_update_delta_intent_other() {
        let src = rc(0, 1);
        let in_order = vec![
            arrival(eid(0, 0, 1)),
            gone(src, eid(0, 0, 1)),
            update(src, eid(0, 0, 1), 0.0),
            halo(src, false),
            GameEventKind::HaloSourceGone(src),
            halo(src, true),
            intent(),
            input(1),
        ];
        let mut inbox = SlotInbox::new();
        for e in in_order.iter().rev() {
            inbox.push(e.clone(), 3);
        }
        assert_eq!(inbox.drain_all(), in_order);
    }

    /// One source's delta, the end of its mirrors, its seed and an entity
    /// update at one stamp sort entity update, delta, Gone, seed whatever the
    /// push order: a lost incarnation's deltas before its Gone, a respawned
    /// one's seed after it.
    #[test]
    fn a_seed_sorts_after_its_sources_gone() {
        let src = rc(0, -1);
        let want = vec![
            update(src, eid(0, -1, 4), 1.0),
            halo(src, false),
            GameEventKind::HaloSourceGone(src),
            halo(src, true),
        ];
        let orders: [[usize; 4]; 4] = [[0, 1, 2, 3], [3, 2, 1, 0], [2, 3, 0, 1], [1, 3, 2, 0]];
        for order in orders {
            let mut inbox = SlotInbox::new();
            for i in order {
                inbox.push(want[i].clone(), 6);
            }
            assert_eq!(inbox.drain_due(6), want, "push order {order:?}");
        }
    }

    #[test]
    fn late_entries_sort_by_stamp_first() {
        let late = arrival(eid(0, 0, 1));
        let early = intent();
        let mut inbox = SlotInbox::new();
        inbox.push(late.clone(), 5);
        inbox.push(early.clone(), 4);
        assert_eq!(inbox.drain_due(5), vec![early, late]);
    }

    #[test]
    fn push_behind_follows_everything_of_its_stamp() {
        let (i1, i2) = (input(1), input(2));
        let arr = arrival(eid(0, 0, 1));
        let mut inbox = SlotInbox::new();
        inbox.push_behind(i1.clone(), 7);
        inbox.push_behind(i2.clone(), 7);
        inbox.push(arr.clone(), 7);
        assert_eq!(inbox.drain_due(7), vec![arr, i1, i2]);
    }

    #[test]
    fn drain_due_leaves_later_stamps() {
        let mut inbox = SlotInbox::new();
        inbox.push(intent(), 3);
        inbox.push(intent(), 4);
        assert_eq!(inbox.drain_due(3).len(), 1);
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox.latest_stamp(), Some(4));
        assert!(!inbox.is_empty());
    }

    #[test]
    fn drain_before_takes_only_earlier_stamps() {
        let mut inbox = SlotInbox::new();
        inbox.push(intent(), 3);
        inbox.push(intent(), 4);
        assert_eq!(inbox.drain_before(4).len(), 1);
        assert_eq!(inbox.latest_stamp(), Some(4));
        assert_eq!(inbox.drain_before(4).len(), 0);
    }

    #[test]
    fn arrival_keys_sort_numerically_at_the_extremes() {
        let big = i64::MAX as u64;
        let in_order = vec![
            eid(i32::MIN, 0, u64::MAX),
            eid(-1, 5, 0),
            eid(-1, 5, big),
            eid(-1, 5, big + 1),
            eid(-1, 5, u64::MAX),
            eid(0, -3, 0),
            eid(0, -3, u64::MAX),
            eid(0, 0, big),
            eid(0, 0, big + 1),
        ];
        // A fixed shuffle of the order above.
        let mut inbox = SlotInbox::new();
        for i in [5, 2, 8, 0, 7, 3, 1, 6, 4] {
            inbox.push(arrival(in_order[i]), 2);
        }
        let drained: Vec<EntityId> = inbox
            .drain_all()
            .into_iter()
            .map(|k| match k {
                GameEventKind::EntityArrived(b) => b.id,
                other => panic!("not an arrival: {other:?}"),
            })
            .collect();
        assert_eq!(drained, in_order);
    }

    #[test]
    fn entries_left_by_drain_due_keep_their_arrival_order() {
        let (a, b) = (input(1), input(2));
        let mut inbox = SlotInbox::new();
        inbox.push_behind(a.clone(), 9);
        inbox.push(intent(), 3);
        inbox.push_behind(b.clone(), 9);
        assert_eq!(inbox.drain_due(3), vec![intent()]);
        inbox.push(intent(), 4);
        assert_eq!(inbox.drain_due(4).len(), 1);
        assert_eq!(inbox.drain_due(9), vec![a, b]);
    }

    /// Two sources' intents (other senders) sort the same whatever the arrival order.
    #[test]
    fn two_sources_intents_sort_the_same_whatever_the_arrival_order() {
        let (a, b) = (take_by(eid(-1, 0, u64::MAX), 0), take_by(eid(0, -1, u64::MAX), 0));
        let mut one = SlotInbox::new();
        one.push(a.clone(), 5);
        one.push(b.clone(), 5);
        let mut two = SlotInbox::new();
        two.push(b, 5);
        two.push(a.clone(), 5);
        let out = one.drain_due(5);
        assert_eq!(out, two.drain_due(5));
        assert_eq!(out[0], a, "by the sender's id");
    }

    /// Replies to one entity from two owners (piles either side of a seam) sort the same
    /// whatever the arrival order; intents come before replies.
    #[test]
    fn two_owners_replies_sort_the_same_whatever_the_arrival_order() {
        let to = eid(0, 0, 3);
        let n = crate::REGION_BLOCKS as i64;
        // Cells owned by the region at x = -1 and by the one at x = 0.
        let (west, here) = (take_reply(to, -4), take_reply(to, n / 2));
        let sent = take_by(eid(5, 5, 0), 0);
        let mut one = SlotInbox::new();
        one.push(here.clone(), 5);
        one.push(west.clone(), 5);
        one.push(sent.clone(), 5);
        let mut two = SlotInbox::new();
        two.push(west.clone(), 5);
        two.push(sent.clone(), 5);
        two.push(here.clone(), 5);
        let out = one.drain_due(5);
        assert_eq!(out, two.drain_due(5));
        assert_eq!(out, vec![sent, west, here], "intents first, then replies by the answering owner");
    }

    /// One source's intents by one sender keep their emission order (equal keys).
    #[test]
    fn one_senders_intents_keep_their_order() {
        let by = eid(1, 1, 7);
        let emitted = vec![take_by(by, 40), take_by(by, 0), take_by(by, 20)];
        let mut inbox = SlotInbox::new();
        for e in &emitted {
            inbox.push(e.clone(), 2);
        }
        assert_eq!(inbox.drain_all(), emitted);
    }

    /// Two different replies to one entity from one owner (no position to tell
    /// them apart: `BreakBlock`; or a relayed copy beside a direct one) sort the
    /// same whatever the arrival order.
    #[test]
    fn one_owners_replies_tie_break_on_their_content() {
        let to = eid(0, 0, 3);
        let brk = |survival: bool| IntentReply::Granted(Intent::BreakBlock { by: to, survival });
        let place = IntentReply::Refused(
            Intent::PlaceBlock { block: crate::BlockId::default(), by: to, survival: true },
            IntentRefusal::Occupied,
        );
        let take = take(0, to);
        let reply = |r: IntentReply, hops: u8, hint: RegionCoords| {
            GameEventKind::Intent(IntentMsg::Reply(SentReply { reply: r, hint, due: 5, hops }))
        };
        let pairs = [
            (reply(brk(true), 0, rc(0, 0)), reply(brk(false), 0, rc(0, 0))),
            (reply(brk(true), 0, rc(0, 0)), reply(place, 0, rc(0, 0))),
            // A relayed copy (one hop, another hint) beside a direct reply.
            (reply(brk(true), 0, rc(0, 0)), reply(brk(true), 1, rc(1, 0))),
            (
                reply(IntentReply::Granted(take), 0, rc(0, 0)),
                reply(IntentReply::Refused(take, IntentRefusal::NothingThere), 0, rc(0, 0)),
            ),
        ];
        for (a, b) in pairs {
            let mut one = SlotInbox::new();
            one.push(a.clone(), 5);
            one.push(b.clone(), 5);
            let mut two = SlotInbox::new();
            two.push(b, 5);
            two.push(a, 5);
            assert_eq!(one.drain_due(5), two.drain_due(5));
        }
    }

    /// Two identical replies keep their arrival order (indistinguishable anyway);
    /// a reply's tie-break never reorders one sender's intents.
    #[test]
    fn identical_replies_keep_arrival_order() {
        let r = take_reply(eid(0, 0, 3), 0);
        let mut inbox = SlotInbox::new();
        inbox.push(r.clone(), 2);
        inbox.push(input(1), 2);
        inbox.push(r.clone(), 2);
        assert_eq!(inbox.drain_all(), vec![r.clone(), r, input(1)]);
        assert!(reply_tie(&take_by(eid(0, 0, 3), 0)).is_none(), "intents carry no content tie-break");
    }

    /// A follower of an arrival sorts directly after it, before the slot's
    /// other messages, whatever the push order; two followers keep their
    /// arrival order.
    #[test]
    fn a_follower_of_an_arrival_sorts_directly_after_it() {
        let (e, smaller) = (eid(0, 0, 9), eid(-1, 0, 1));
        let (f1, f2) = (input(1), input(2));
        let other = update(rc(0, -1), eid(0, -1, 4), 1.0);
        let want = vec![arrival(smaller), arrival(e), f1.clone(), f2.clone(), other.clone()];
        let orders: [[usize; 5]; 4] = [[0, 1, 2, 3, 4], [4, 3, 2, 1, 0], [2, 4, 0, 3, 1], [3, 0, 4, 2, 1]];
        for order in orders {
            let mut inbox = SlotInbox::new();
            let mut pushed_f1 = false;
            for i in order {
                match i {
                    0 => inbox.push(arrival(e), 5),
                    1 => inbox.push(arrival(smaller), 5),
                    // f1 is always pushed before f2: they keep that order.
                    2 | 3 if !pushed_f1 => {
                        inbox.push_after_arrival(f1.clone(), 5, e);
                        pushed_f1 = true;
                    }
                    2 | 3 => inbox.push_after_arrival(f2.clone(), 5, e),
                    _ => inbox.push(other.clone(), 5),
                }
            }
            assert_eq!(inbox.drain_due(5), want, "push order {order:?}");
        }
    }

    /// A follower of a held creation is pushed behind it (class `Other`), as before.
    #[test]
    fn a_follower_of_a_creation_stays_behind_it() {
        let create = GameEventKind::CreateClient(7);
        let mut inbox = SlotInbox::new();
        inbox.push(create.clone(), 4);
        inbox.push_behind(input(7), 4);
        inbox.push(arrival(eid(0, 0, 1)), 4);
        assert_eq!(inbox.drain_due(4), vec![arrival(eid(0, 0, 1)), create, input(7)]);
    }

    /// Two `BreakBlock` replies of one slot for different senders (no position,
    /// so no owner key) come out in sender order whatever order they went in.
    #[test]
    fn a_break_block_reply_sorts_by_sender() {
        let (first, second) = (eid(-1, 0, 7), eid(0, 0, 2));
        let want = vec![break_reply(first), break_reply(second)];
        for order in [[0, 1], [1, 0]] {
            let mut inbox = SlotInbox::new();
            for i in order {
                inbox.push(want[i].clone(), 5);
            }
            assert_eq!(inbox.drain_due(5), want, "push order {order:?}");
        }
    }

    /// A block reply and an arrival of one slot: the arrival first
    /// (`SlotClass::Arrival` before `SlotClass::Intent`), whatever the push order.
    #[test]
    fn a_block_reply_and_an_arrival_of_one_slot_keep_class_order() {
        let body = eid(0, 0, 4);
        let want = vec![arrival(body), break_reply(body)];
        for order in [[0, 1], [1, 0]] {
            let mut inbox = SlotInbox::new();
            for i in order {
                inbox.push(want[i].clone(), 3);
            }
            assert_eq!(inbox.drain_due(3), want, "push order {order:?}");
        }
    }

    #[test]
    fn holds_finds_a_buffered_arrival() {
        let mut inbox = SlotInbox::new();
        assert_eq!(inbox.holds(|k| matches!(k, GameEventKind::EntityArrived(_))), None);
        inbox.push(intent(), 2);
        inbox.push(arrival(eid(0, 0, 1)), 6);
        assert_eq!(inbox.holds(|k| matches!(k, GameEventKind::EntityArrived(_))), Some(6));
    }
}
