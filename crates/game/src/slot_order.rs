//! One canonical order for the messages a region's neighbours send it within a
//! tick's slot (design: `.superpowers/sdd/2026-11-13-seam-slot-order/design.md`).
//!
//! Runner state, not simulation state: never hashed, saved or rolled back. A
//! region's runner buffers every region-to-region message here and, right
//! before each `Tick`, applies the ones whose stamp has come, sorted by
//! `(stamp, class, key, arrival)` — so thread timing never decides the order.

use std::collections::VecDeque;

use crate::{EntityId, GameEventKind, RegionCoords, Tick};

/// Where a message sorts within its stamp.
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

type Key = [i64; 5];

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
            [a, b, c, 0, 0]
        }
        GameEventKind::HaloEntityGone(g) => {
            let [fx, fz] = region_key(g.from);
            let [a, b, c] = id_key(g.id);
            [fx, fz, a, b, c]
        }
        GameEventKind::HaloEntity(d) => {
            let [sx, sz] = region_key(d.source_region);
            let [a, b, c] = id_key(d.id);
            [sx, sz, a, b, c]
        }
        GameEventKind::HaloUpdate(u) => {
            let [fx, fz] = region_key(u.from);
            [fx, fz, 0, 0, 0]
        }
        GameEventKind::HaloSourceGone(r) => {
            let [x, z] = region_key(*r);
            [x, z, 0, 0, 0]
        }
        _ => [0; 5],
    }
}

struct Entry {
    stamp: Tick,
    class: SlotClass,
    key: Key,
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

    fn add(&mut self, kind: GameEventKind, stamp: Tick, class: SlotClass, key: Key) {
        let arrival = self.next_arrival;
        self.next_arrival += 1;
        self.entries.push_back(Entry { stamp, class, key, arrival, kind });
    }

    /// Buffer a message; its class and key come from the kind.
    pub fn push(&mut self, kind: GameEventKind, stamp: Tick) {
        let (class, key) = (slot_class(&kind), key_of(&kind));
        self.add(kind, stamp, class, key);
    }

    /// Buffer an entry that must follow the others of its stamp (class `Other`,
    /// arrival order among its peers), whatever its kind.
    pub fn push_behind(&mut self, kind: GameEventKind, stamp: Tick) {
        self.add(kind, stamp, SlotClass::Other, [0; 5]);
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
        out.sort_by_key(|e| (e.stamp, e.class, e.key, e.arrival));
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
        HaloUpdate, InputEvent, IntentMsg, Pose, SentIntent,
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

    #[test]
    fn holds_finds_a_buffered_arrival() {
        let mut inbox = SlotInbox::new();
        assert_eq!(inbox.holds(|k| matches!(k, GameEventKind::EntityArrived(_))), None);
        inbox.push(intent(), 2);
        inbox.push(arrival(eid(0, 0, 1)), 6);
        assert_eq!(inbox.holds(|k| matches!(k, GameEventKind::EntityArrived(_))), Some(6));
    }
}
