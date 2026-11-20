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

use crate::{
    EntityId, GameEventKind, Intent, IntentMsg, IntentRefusal, IntentReply, RegionCoords, Tick,
};

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
    let GameEventKind::Intent(IntentMsg::Reply(sent)) = kind else {
        return None;
    };
    let (intent, refusal) = match sent.reply {
        IntentReply::Granted(i) => (i, None),
        IntentReply::Refused(i, r) => (i, Some(r)),
    };
    Some(ReplyTie {
        intent,
        refusal,
        hops: sent.hops,
        hint: sent.hint,
    })
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

    fn add(
        &mut self,
        kind: GameEventKind,
        stamp: Tick,
        class: SlotClass,
        key: Key,
        tie: Option<ReplyTie>,
    ) {
        self.add_entry(kind, stamp, class, key, tie, 0);
    }

    fn add_entry(
        &mut self,
        kind: GameEventKind,
        stamp: Tick,
        class: SlotClass,
        key: Key,
        tie: Option<ReplyTie>,
        follower: u8,
    ) {
        let arrival = self.next_arrival;
        self.next_arrival += 1;
        self.entries.push_back(Entry {
            stamp,
            class,
            key,
            tie,
            follower,
            arrival,
            kind,
        });
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
    pub fn holds_entry(
        &self,
        pred: impl Fn(&GameEventKind) -> bool,
    ) -> Option<(Tick, &GameEventKind)> {
        self.entries
            .iter()
            .find(|e| pred(&e.kind))
            .map(|e| (e.stamp, &e.kind))
    }
}
