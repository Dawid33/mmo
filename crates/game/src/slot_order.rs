//! One canonical order for the messages a region's neighbours send it within a
//! tick's slot (design: `.superpowers/sdd/2026-11-13-seam-slot-order/design.md`).
//!
//! Runner state, not simulation state: never hashed, saved or rolled back. A
//! region's runner buffers every region-to-region message here and, right
//! before each `Tick`, applies the ones whose stamp has come, sorted by
//! `(stamp, class, key, arrival)` — so thread timing never decides the order.

use std::collections::VecDeque;

use crate::{EntityId, GameEventKind, Intent, IntentMsg, IntentRefusal, IntentReply, RegionCoords, Tick};

/// Where a message sorts within its stamp. Within a class, by the message's own
/// key, then arrival (one FIFO per source, so equal keys keep their source's order):
/// - `Arrival`: the entity's id;
/// - `HaloEntityGone` / `HaloEntity`: the source region, then the entity's id;
/// - `HaloUpdate`, `HaloSourceGone`: the source region;
/// - `Intent`: intents before replies; an intent by its sending entity (`by`), a
///   reply by the entity it is for, then the region that answered it (the owner
///   of its intent's position), then — replies only — its content (`reply`,
///   `hops`, `hint`; amended in execution: replies to `BreakBlock` /
///   `PlaceBlock` carry no position, and a reply relayed through the entity's old
///   region can tie with a direct one from the same owner). So messages of two
///   sources never tie; equal keys are identical replies or one sender's intents,
///   which keep arrival order (a sender's emission order carries meaning: carve
///   then place);
/// - `Other`: arrival only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SlotClass {
    Arrival = 0,
    HaloEntityGone = 1,
    HaloEntity = 2,
    HaloUpdate = 3,
    HaloSourceGone = 4,
    Intent = 5,
    /// Anything else, and anything queued behind another entry.
    Other = 6,
}

/// The class of a message kind.
pub fn slot_class(kind: &GameEventKind) -> SlotClass {
    match kind {
        GameEventKind::EntityArrived(_) => SlotClass::Arrival,
        GameEventKind::HaloEntityGone(_) => SlotClass::HaloEntityGone,
        GameEventKind::HaloEntity(_) => SlotClass::HaloEntity,
        GameEventKind::HaloUpdate(_) => SlotClass::HaloUpdate,
        GameEventKind::HaloSourceGone(_) => SlotClass::HaloSourceGone,
        GameEventKind::Intent(_) => SlotClass::Intent,
        _ => SlotClass::Other,
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
        _ => [0; 6],
    }
}

/// Class 5's first key: intents before replies.
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
        Intent::CarveStud { lo, .. } | Intent::FillStud { lo, .. } | Intent::BreakStud { lo, .. } => {
            Some(crate::stud_chunk_of_cell(lo).to_local().0)
        }
        Intent::PlaceStud { chunk, .. } | Intent::RemoveStud { chunk, .. } | Intent::PlaceBrick { chunk, .. } => {
            Some(chunk.to_local().0)
        }
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
        let arrival = self.next_arrival;
        self.next_arrival += 1;
        self.entries.push_back(Entry { stamp, class, key, tie, arrival, kind });
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
        out.sort_by_key(|e| (e.stamp, e.class, e.key, e.tie, e.arrival));
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
        self.entries.iter().find(|e| pred(&e.kind)).map(|e| e.stamp)
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
            piles: Vec::new(),
            containers: Vec::new(),
            studs: Vec::new(),
            stud_ground: Vec::new(),
        })
    }

    fn intent() -> GameEventKind {
        GameEventKind::Intent(IntentMsg::Intent(SentIntent::default()))
    }

    /// An intent by `by` carving a box in the world stud cells at `x` (any owner).
    fn carve_by(by: EntityId, x: i64) -> GameEventKind {
        GameEventKind::Intent(IntentMsg::Intent(SentIntent {
            intent: Intent::CarveStud { lo: [x, 0, 0], hi: [x + 4, 6, 4], by },
            ..SentIntent::default()
        }))
    }

    /// A reply to `to` for a carve of the world stud cells at `x` (answered by their owner).
    fn carve_reply(to: EntityId, x: i64) -> GameEventKind {
        GameEventKind::Intent(IntentMsg::Reply(SentReply {
            reply: IntentReply::Granted(Intent::CarveStud { lo: [x, 0, 0], hi: [x + 4, 6, 4], by: to }),
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
            halo(src, true),
            GameEventKind::HaloSourceGone(src),
            intent(),
            input(1),
        ];
        let mut inbox = SlotInbox::new();
        for e in in_order.iter().rev() {
            inbox.push(e.clone(), 3);
        }
        assert_eq!(inbox.drain_all(), in_order);
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
        let (a, b) = (carve_by(eid(-1, 0, u64::MAX), 0), carve_by(eid(0, -1, u64::MAX), 0));
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

    /// Replies to one entity from two owners (a carve over a corner) sort the same
    /// whatever the arrival order; intents come before replies.
    #[test]
    fn two_owners_replies_sort_the_same_whatever_the_arrival_order() {
        let to = eid(0, 0, 3);
        let n = crate::REGION_STUDS as i64;
        // Cells owned by the region at x = -1 and by the one at x = 0.
        let (west, here) = (carve_reply(to, -4), carve_reply(to, n / 2));
        let sent = carve_by(eid(5, 5, 0), 0);
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
        let emitted = vec![carve_by(by, 40), carve_by(by, 0), carve_by(by, 20)];
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
        let carve = Intent::CarveStud { lo: [0, 0, 0], hi: [4, 6, 4], by: to };
        let reply = |r: IntentReply, hops: u8, hint: RegionCoords| {
            GameEventKind::Intent(IntentMsg::Reply(SentReply { reply: r, hint, due: 5, hops }))
        };
        let pairs = [
            (reply(brk(true), 0, rc(0, 0)), reply(brk(false), 0, rc(0, 0))),
            (reply(brk(true), 0, rc(0, 0)), reply(place, 0, rc(0, 0))),
            // A relayed copy (one hop, another hint) beside a direct reply.
            (reply(brk(true), 0, rc(0, 0)), reply(brk(true), 1, rc(1, 0))),
            (
                reply(IntentReply::Granted(carve), 0, rc(0, 0)),
                reply(IntentReply::Refused(carve, IntentRefusal::NothingThere), 0, rc(0, 0)),
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
        let r = carve_reply(eid(0, 0, 3), 0);
        let mut inbox = SlotInbox::new();
        inbox.push(r.clone(), 2);
        inbox.push(input(1), 2);
        inbox.push(r.clone(), 2);
        assert_eq!(inbox.drain_all(), vec![r.clone(), r, input(1)]);
        assert!(reply_tie(&carve_by(eid(0, 0, 3), 0)).is_none(), "intents carry no content tie-break");
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
