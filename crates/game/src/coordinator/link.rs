//! The host's end of its link to the world coordinator, and the inline link that runs a
//! [`Coordinator`] in the host's own thread (single-player on wasm, tests).

use crate::HostId;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use super::{CallStamp, CoordMsg, Coordinator, HostMsg, Input, OutboxMsg, ReplicaId, StoreDecision, Target};

/// The host's end of the link to the world coordinator: messages out, messages in, in
/// order (FIFO each way while the link is up).
pub trait WorldLink {
    fn send(&mut self, msg: HostMsg);
    fn recv(&mut self) -> Option<CoordMsg>;
    fn is_up(&self) -> bool;
    /// The link's connection generation: it changes on every (re)connect. A host that
    /// sees it change while [`WorldLink::is_up`] stayed true (the link dropped and came
    /// back between two looks) re-attaches as after a down link. A link that never
    /// reconnects (inline, in-process) keeps the default 0.
    fn generation(&self) -> u64 {
        0
    }
    /// Drive a coordinator that runs in the caller's thread ([`InlineWorldLink`]); the
    /// host calls it before draining [`WorldLink::recv`]. A no-op for a real link.
    fn pump(&mut self, _now_ms: u64) {}
    /// The inline link behind a boxed link, for tests and tools that reach the
    /// coordinator core (`InlineWorldLink::coordinator_mut`). `None` for any other link.
    fn as_inline_mut(&mut self) -> Option<&mut InlineWorldLink> {
        None
    }
    /// [`WorldLink::as_inline_mut`], read-only.
    fn as_inline(&self) -> Option<&InlineWorldLink> {
        None
    }
}

/// splitmix64 of `seed` and `i`: the stateless roll behind the inline link's delays.
fn mix(seed: u64, i: u64) -> u64 {
    let mut z = seed ^ i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A row of the rig's stand-in for the `coordinator` table: the version a compare-and-set
/// writes against, and the two parts of the projection.
#[doc(hidden)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RigRow {
    pub version: u64,
    pub core: Vec<u8>,
    pub history: Vec<u8>,
}

/// A write at a version that is no longer the row's ([`RigStore::write`]).
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RigConflict;

/// A message a transition left in the store's outbox, for the host `host` at its `boot`.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct RigOutboxRow {
    pub seq: u64,
    pub version: u64,
    pub host: HostId,
    pub boot: u64,
    pub msg: CoordMsg,
}

/// One transition to write ([`RigStore::write`]), as `world_store`'s `TransitionWrite`.
#[doc(hidden)]
pub struct RigWrite {
    /// The row version it was read at.
    pub version: u64,
    pub core: Vec<u8>,
    /// `None` keeps the row's.
    pub history: Option<Vec<u8>>,
    pub decisions: Vec<StoreDecision>,
    pub replica: ReplicaId,
    /// Messages for hosts linked to another replica.
    pub outbox: Vec<OutboxMsg>,
    /// The writer's own hosts, with their links' boots: their rows are taken.
    pub take_for: Vec<(HostId, u64)>,
    /// The hosts whose `Attach` it applied: their rows are discarded first.
    pub discard_for: Vec<HostId>,
}

impl RigWrite {
    /// A transition with no outbox message, take or discard.
    pub fn of(version: u64, core: Vec<u8>, history: Option<Vec<u8>>, decisions: Vec<StoreDecision>, replica: ReplicaId) -> RigWrite {
        RigWrite { version, core, history, decisions, replica, outbox: Vec::new(), take_for: Vec::new(), discard_for: Vec::new() }
    }
}

/// The seeded rig's store: one versioned row written by compare-and-set, and an outbox.
#[doc(hidden)]
#[derive(Default)]
pub struct RigStore {
    row: Option<RigRow>,
    outbox: Vec<RigOutboxRow>,
    seq: u64,
    /// Writes that committed.
    writes: u64,
    /// The store decisions committed with their transitions (Elastic 2b Task 3), stamped with
    /// the call they are, in commit order, not executed yet: the test's simulated world store
    /// (`MultiRig`) executes them — as part of the commit, before anything else touches it — and
    /// answers them, or drops the answers (a replica killed after the commit).
    committed: Vec<(CallStamp, StoreDecision)>,
}

impl RigStore {
    /// Write `w` as `world_store`'s `write_transition` does, in one step: the projection over the
    /// row as read at `w.version` (0 = no row yet) — `Err(RigConflict)` when that is not the row's,
    /// nothing written, no decision kept, no outbox row inserted or taken; then the decisions kept
    /// (stamped with the version it commits as), the outbox messages inserted at that version, the
    /// rows of `discard_for` deleted unsent, the rows of `take_for` taken (in `seq` order; a row of
    /// another boot than its host's link deleted unsent). The new version and the rows taken.
    pub fn write(&mut self, w: RigWrite) -> Result<(u64, Vec<RigOutboxRow>), RigConflict> {
        let current = self.row.as_ref().map_or(0, |r| r.version);
        if w.version != current {
            return Err(RigConflict);
        }
        let version = current + 1;
        let history = w.history.unwrap_or_else(|| self.row.take().map(|r| r.history).unwrap_or_default());
        self.row = Some(RigRow { version, core: w.core, history });
        self.writes += 1;
        let stamp = CallStamp { replica: w.replica, version };
        self.committed.extend(w.decisions.into_iter().map(|d| (stamp, d)));
        for m in w.outbox {
            self.push_outbox(version, m.host, m.boot, m.msg);
        }
        self.outbox.retain(|r| !w.discard_for.contains(&r.host));
        Ok((version, self.take_outbox(&w.take_for, version)))
    }

    /// The outbox rows of `for_hosts` (each with its link's boot) at or below row version `upto`
    /// (the taker's cache: final review S-I2), taken: in `seq` order; a row of another boot
    /// deleted unsent.
    pub fn take_outbox(&mut self, for_hosts: &[(HostId, u64)], upto: u64) -> Vec<RigOutboxRow> {
        let (mine, rest): (Vec<RigOutboxRow>, Vec<RigOutboxRow>) = std::mem::take(&mut self.outbox)
            .into_iter()
            .partition(|r| r.version <= upto && for_hosts.iter().any(|(h, _)| *h == r.host));
        self.outbox = rest;
        mine.into_iter().filter(|r| for_hosts.iter().any(|(h, b)| *h == r.host && *b == r.boot)).collect()
    }

    /// The store decisions committed since the last call, with their stamps, in order.
    pub fn take_committed(&mut self) -> Vec<(CallStamp, StoreDecision)> {
        std::mem::take(&mut self.committed)
    }

    pub fn row(&self) -> Option<&RigRow> {
        self.row.as_ref()
    }

    /// Writes committed so far.
    pub fn writes(&self) -> u64 {
        self.writes
    }

    /// Leave a message in the outbox, numbered in order; its `seq`.
    pub fn push_outbox(&mut self, version: u64, host: HostId, boot: u64, msg: CoordMsg) -> u64 {
        self.seq += 1;
        self.outbox.push(RigOutboxRow { seq: self.seq, version, host, boot, msg });
        self.seq
    }

    pub fn outbox(&self) -> &[RigOutboxRow] {
        &self.outbox
    }
}

/// One coordinator process of the rig: its core (which holds the row version it last read or
/// wrote), the hosts linked to it with the boots their `Attach`es named, and the outbox rows it
/// took that its cache is not new enough to relay yet.
#[doc(hidden)]
pub struct RigReplica {
    pub core: Coordinator,
    pub hosts: BTreeSet<HostId>,
    pub boots: BTreeMap<HostId, u64>,
    pub held: VecDeque<RigOutboxRow>,
    /// Its process died ([`RigCluster::kill_replica`]): it handles, writes and sends nothing more.
    pub dead: bool,
}

impl RigReplica {
    fn of(core: Coordinator) -> RigReplica {
        RigReplica { core, hosts: BTreeSet::new(), boots: BTreeMap::new(), held: VecDeque::new(), dead: false }
    }
}

/// What the rig's replicas did, in order ([`RigCluster::start_recording`]): what a reference
/// core fed every replica's committed inputs in commit order replays.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub enum RigEvent {
    /// Replica 0's world tick was set (its process configuration).
    Tick(crate::Tick),
    /// A transition of `replica` committed as `version`: the inputs it holds, in order, the store
    /// decisions it issued, and the hosts linked to `replica` (its `Every`s reach them).
    Commit { replica: ReplicaId, version: u64, inputs: Vec<(u64, Input)>, decisions: Vec<StoreDecision>, linked: BTreeSet<HostId> },
    /// `replica` released what it decided with nothing durable pending, at `version`: the inputs
    /// that left its log.
    Release { replica: ReplicaId, version: u64, inputs: Vec<(u64, Input)>, linked: BTreeSet<HostId> },
}

/// Most passes of one replica's route ([`RigCluster::route`]): writes, refreshes, relays.
const ROUTE_PASSES: usize = 8;

/// The replicas several hosts' inline links share ([`InlineWorldLink::shared`]), the store
/// behind them, and each host's messages not read by its link yet (with the row version that
/// released each). Derefs to the ticker's coordinator ([`RigCluster::ticker_index`]).
///
/// With a store ([`RigCluster::with_store`], the seeded rig) every pump drives the transitions of
/// the replica its link reaches ([`Coordinator::take_transition`]): each is written to the row at
/// the version that replica last read, as a store that never fails, before its messages are
/// routed. A write at a stale version (another writer moved the row) rebases the replica onto the
/// row ([`Coordinator::rebase`]) and writes again, as the runtime does. Several replicas (Elastic
/// 2b Task 5, [`RigCluster::add_replica`]): a replica's messages reach only the hosts linked to it,
/// a message for a host attached to another replica goes through the store's outbox and is relayed
/// by the replica linking that host — taken in its writes, and between them (as on a notice) — once
/// its cache is at least as new as the row's version; a cache found stale (a version check, a cut
/// it cannot know) is refreshed from the row. Timed work (the upkeep, the drives of the test
/// rig) runs on the replica whose core says it is the ticker ([`Coordinator::is_ticker`], Elastic
/// 2b Task 6). Without a store the core is unstored ([`Coordinator::commit_unstored`]).
#[doc(hidden)]
pub struct RigCluster {
    replicas: Vec<RigReplica>,
    store: Option<RigStore>,
    boxes: BTreeMap<HostId, VecDeque<(u64, CoordMsg)>>,
    /// [`Self::arm_foreign_version`]: before the next write of a replica that may rebase, a
    /// foreign writer moves the row.
    foreign_armed: bool,
    /// Rebases done after a conflict, so far.
    rebases: u64,
    /// Refreshes done (a cache found stale between its writes), so far.
    refreshes: u64,
    /// [`Self::start_recording`]: the events so far.
    events: Option<Vec<RigEvent>>,
    /// While recording: what each host's mailbox got, with the version that released it.
    delivered: BTreeMap<HostId, Vec<(u64, CoordMsg)>>,
    /// [`Self::hold_writes`]: replicas whose writes (and so everything they would release) are
    /// held up, as a write waiting on the row lock or a stalled store.
    held_writes: BTreeSet<usize>,
}

impl RigCluster {
    pub fn new(coordinator: Coordinator) -> Arc<Mutex<RigCluster>> {
        Arc::new(Mutex::new(RigCluster {
            replicas: vec![RigReplica::of(coordinator)],
            store: None,
            boxes: BTreeMap::new(),
            foreign_armed: false,
            rebases: 0,
            refreshes: 0,
            events: None,
            delivered: BTreeMap::new(),
            held_writes: BTreeSet::new(),
        }))
    }

    /// Test hook (final review G-I1): replica `r`'s writes are held up (`true`) — it applies what
    /// its links bring and logs it, writes nothing, releases nothing — or go on (`false`; its next
    /// pass writes, and meets whatever other replicas committed meanwhile).
    pub fn hold_writes(&mut self, r: usize, hold: bool) {
        if hold {
            self.held_writes.insert(r);
        } else {
            self.held_writes.remove(&r);
        }
    }

    /// Test hook: right before the next write of a replica, a foreign writer writes the row's
    /// own projection at the next version — the row moves, its content does not — so that write
    /// conflicts and the replica rebases.
    pub fn arm_foreign_version(&mut self) {
        self.foreign_armed = true;
    }

    /// Rebases after a conflict so far.
    pub fn rebases(&self) -> u64 {
        self.rebases
    }

    /// Refreshes of a stale cache (no conflict) so far.
    pub fn refreshes(&self) -> u64 {
        self.refreshes
    }

    /// [`Self::new`] with a store behind the core: every transition is written to its row,
    /// and a message leaves only with (or after) its transition.
    pub fn with_store(coordinator: Coordinator) -> Arc<Mutex<RigCluster>> {
        let core = Self::new(coordinator);
        lock(&core).store = Some(RigStore::default());
        core
    }

    /// Another replica (Elastic 2b Task 5): `core` — loaded from [`Self::stored`] by the caller, its
    /// version set, its own [`Input::ReplicaJoined`] applied — joins the cluster with no host
    /// linked yet ([`InlineWorldLink::shared_at`], [`InlineWorldLink::move_to`]). Its index.
    pub fn add_replica(&mut self, core: Coordinator) -> usize {
        self.replicas.push(RigReplica::of(core));
        self.replicas.len() - 1
    }

    pub fn replicas(&self) -> &[RigReplica] {
        &self.replicas
    }

    /// Replica `r`'s core.
    pub fn replica_core(&self, r: usize) -> &Coordinator {
        &self.replicas[r].core
    }

    /// Replica `r`'s core, to change.
    pub fn replica_core_mut(&mut self, r: usize) -> &mut Coordinator {
        &mut self.replicas[r].core
    }

    pub fn store(&self) -> Option<&RigStore> {
        self.store.as_ref()
    }

    /// The projection as the store holds it: `(core, history)` of the last transition (`None`
    /// without a store, or before the first transition).
    pub fn stored(&self) -> Option<(Vec<u8>, Vec<u8>)> {
        self.store.as_ref()?.row().filter(|r| !r.core.is_empty()).map(|r| (r.core.clone(), r.history.clone()))
    }

    /// The row's version (0: none).
    pub fn row_version(&self) -> u64 {
        self.store.as_ref().and_then(|s| s.row()).map_or(0, |r| r.version)
    }

    /// Transitions written so far (with a store).
    pub fn written(&self) -> u64 {
        self.store.as_ref().map_or(0, RigStore::writes)
    }

    /// The ticker's pending transition written now (and what may leave routed), as a runtime
    /// pass ends: the store decisions it handed out are committed with it
    /// ([`Self::take_committed`]).
    pub fn commit(&mut self) {
        let r = self.ticker_index();
        self.route(r);
    }

    /// Replica `r`'s pass without a host's input: its transitions written, its relays taken.
    pub fn pump_replica(&mut self, r: usize) {
        self.route(r);
    }

    /// The replica that does the timed work (Elastic 2b Task 6): the first live process whose
    /// core says it is the ticker — or, while none does (the ticker died and nobody ended it
    /// yet), the first live process (whose core then hands nothing out).
    pub fn ticker_index(&self) -> usize {
        let alive = || self.replicas.iter().enumerate().filter(|(_, r)| !r.dead);
        alive().find(|(_, r)| r.core.is_ticker()).or_else(|| alive().next()).map_or(0, |(i, _)| i)
    }

    /// Test hook (Elastic 2b Task 6): replica `r`'s process dies — it handles, writes and sends
    /// nothing more; what it held (its outgoing queue, the rows it took) and what its links
    /// carried is lost. The hosts linked to it stay linked to nothing until moved
    /// ([`InlineWorldLink::move_to`]); the others end it ([`Input::ReplicaLapsed`]) at their
    /// own pace.
    pub fn kill_replica(&mut self, r: usize) {
        let replica = &mut self.replicas[r];
        replica.dead = true;
        replica.held.clear();
        for h in std::mem::take(&mut replica.hosts) {
            if let Some(b) = self.boxes.get_mut(&h) {
                b.clear();
            }
        }
        self.replicas[r].boots.clear();
    }

    /// The store's answer `a` to a decision replica `replica` issued (Elastic 2b Task 3), applied
    /// on that replica as an input at `now_ms` — none if its process died (the answer dies with
    /// it: its successor looks the mark up). Whether a live replica took it.
    pub fn answer(&mut self, replica: ReplicaId, a: super::DecisionAnswer, now_ms: u64) -> bool {
        let Some(r) = self.replicas.iter_mut().find(|r| !r.dead && r.core.replica() == replica) else { return false };
        r.core.apply(Input::Decision(a), now_ms);
        true
    }

    /// The store decisions committed with transitions since the last call (Elastic 2b Task 3):
    /// for the test's simulated world store to execute and answer.
    pub fn take_committed(&mut self) -> Vec<(CallStamp, StoreDecision)> {
        self.store.as_mut().map(RigStore::take_committed).unwrap_or_default()
    }

    /// A restart of replica 0's process: the core is replaced by `coordinator` (loaded
    /// from [`Self::stored`]), which has read the row as it stands, and whatever the old
    /// process had not handed to a link yet — its outgoing queue, the hosts' mailboxes, the rows
    /// it held — is lost with it.
    pub fn restart(&mut self, mut coordinator: Coordinator) {
        coordinator.set_version(self.row_version());
        self.replicas[0].core = coordinator;
        self.replicas[0].held.clear();
        for b in self.boxes.values_mut() {
            b.clear();
        }
    }

    /// Every live replica's world tick (each process's configuration), recorded for a reference
    /// core.
    pub fn set_world_tick(&mut self, tick: crate::Tick) {
        if let Some(events) = self.events.as_mut() {
            events.push(RigEvent::Tick(tick));
        }
        for r in self.replicas.iter_mut().filter(|r| !r.dead) {
            r.core.set_world_tick(tick);
        }
    }

    /// From now on every replica's commits and releases (with the inputs they hold) and replica
    /// 0's world tick are recorded ([`Self::take_events`]), and what each host's mailbox gets
    /// ([`Self::delivered`]).
    pub fn start_recording(&mut self) {
        for r in &mut self.replicas {
            r.core.record_inputs();
            r.core.take_recorded();
        }
        self.events = Some(Vec::new());
        self.delivered.clear();
    }

    /// The events recorded so far ([`Self::start_recording`]), taken.
    pub fn take_events(&mut self) -> Vec<RigEvent> {
        self.events.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// What each host's mailbox got while recording, with the version that released each.
    pub fn delivered(&self) -> &BTreeMap<HostId, Vec<(u64, CoordMsg)>> {
        &self.delivered
    }

    /// What was released for each host and has not reached it yet: the store's outbox rows and
    /// the rows a replica took and holds.
    pub fn undelivered(&self) -> BTreeMap<HostId, Vec<CoordMsg>> {
        let mut out: BTreeMap<HostId, Vec<CoordMsg>> = BTreeMap::new();
        let rows = self.store.iter().flat_map(|s| s.outbox().iter()).chain(self.replicas.iter().flat_map(|r| r.held.iter()));
        for row in rows {
            out.entry(row.host).or_default().push(row.msg.clone());
        }
        out
    }

    /// Host `host`'s link moves to replica `to` (Elastic 2b Task 5): its link to the replica it
    /// reached goes down there ([`Input::LinkDown`] at `now_ms`, written with that replica's next
    /// transition), what that link carried and had not been read is lost, and the host is linked
    /// to `to` (its next `Attach` goes there). The replica it left.
    pub fn move_host(&mut self, host: HostId, to: usize, now_ms: u64) -> usize {
        let from = self.replicas.iter().position(|r| r.hosts.contains(&host)).expect("the host is linked");
        self.replicas[from].hosts.remove(&host);
        self.replicas[from].boots.remove(&host);
        self.replicas[to].hosts.insert(host);
        if let Some(b) = self.boxes.get_mut(&host) {
            b.clear();
        }
        self.replicas[from].core.apply(Input::LinkDown { host }, now_ms);
        self.route(from);
        from
    }

    /// Host `host`, linked to a replica whose process died ([`Self::kill_replica`]) — or to none —
    /// is linked to replica `to` (its next `Attach` goes there).
    pub fn relink_host(&mut self, host: HostId, to: usize) {
        for r in &mut self.replicas {
            r.hosts.remove(&host);
            r.boots.remove(&host);
        }
        if let Some(b) = self.boxes.get_mut(&host) {
            b.clear();
        }
        self.replicas[to].hosts.insert(host);
    }

    /// The hosts replica `r` takes outbox rows for: those its core has attached to it and that
    /// are linked to it, at the boots their links' `Attach`es named.
    fn take_for(&self, r: usize) -> Vec<(HostId, u64)> {
        let replica = &self.replicas[r];
        replica
            .core
            .hosts_attached_to(replica.core.replica())
            .into_iter()
            .filter(|h| replica.hosts.contains(h))
            .filter_map(|h| replica.boots.get(&h).map(|b| (h, *b)))
            .collect()
    }

    /// Replica `r` refreshed from the row if the row is newer and its core may be rebased (as
    /// the runtime does on a notice or a stale finding); whether it did.
    fn refresh(&mut self, r: usize) -> bool {
        let Some(row) = self.store.as_ref().and_then(|s| s.row()).cloned() else { return false };
        let core = &mut self.replicas[r].core;
        if row.version <= core.version() {
            return false;
        }
        let moved = core.refresh(&row.core, Some(&row.history), row.version).expect("the row loads");
        if moved {
            self.refreshes += 1;
        }
        moved
    }

    /// Replica `r`'s held rows its cache is new enough for, in order.
    fn release_held(&mut self, r: usize, out: &mut Vec<(u64, Target, CoordMsg)>) {
        let replica = &mut self.replicas[r];
        while replica.held.front().is_some_and(|row| row.version <= replica.core.version()) {
            let row = replica.held.pop_front().expect("checked");
            out.push((row.version, Target::Host(row.host), row.msg));
        }
    }

    /// What may leave replica `r` goes to the mailboxes of the hosts linked to it (a message
    /// for a host not linked to it is lost, as on a real link), each tagged with the row version
    /// that released it.
    fn route(&mut self, r: usize) {
        if self.replicas[r].dead || self.held_writes.contains(&r) {
            return;
        }
        let mut out: Vec<(u64, Target, CoordMsg)> = Vec::new();
        if self.store.is_none() {
            let core = &mut self.replicas[r].core;
            core.commit_unstored();
            out.extend(core.take_out().into_iter().map(|(to, m)| (0, to, m)));
        } else {
            for _ in 0..ROUTE_PASSES {
                self.write_all(r, &mut out);
                // A cut this cache cannot know (Elastic 2b Task 5): refreshed and replayed, or —
                // the row no newer — looked up.
                if self.replicas[r].core.take_refresh_wanted() {
                    if self.refresh(r) {
                        continue;
                    }
                    self.replicas[r].core.refresh_not_newer();
                }
                // The version check of what leaves without a write (several replicas).
                let row_version = self.row_version();
                if let Some(v) = self.replicas[r].core.version_check_wanted() {
                    if !self.replicas[r].core.version_checked(row_version) {
                        debug_assert!(v != row_version);
                        if self.refresh(r) {
                            continue;
                        }
                        break;
                    }
                }
                // A notice: the row moved past this cache (another replica wrote) — refreshed (its
                // log replayed onto the row) before anything more is released.
                if self.refresh(r) {
                    continue;
                }
                // The rows other replicas left for this replica's hosts (as on a notice).
                let take_for = self.take_for(r);
                let upto = self.replicas[r].core.version();
                let rows = self.store.as_mut().expect("a store").take_outbox(&take_for, upto);
                self.replicas[r].held.extend(rows);
                if self.replicas[r].held.iter().any(|row| row.version > self.replicas[r].core.version()) && self.refresh(r) {
                    continue;
                }
                self.release_held(r, &mut out);
                let core = &mut self.replicas[r].core;
                let free = core.take_out();
                let (replica, version) = (core.replica(), core.version());
                let inputs = core.take_recorded();
                if let Some(events) = self.events.as_mut() {
                    if !inputs.is_empty() || !free.is_empty() {
                        events.push(RigEvent::Release { replica, version, inputs, linked: self.replicas[r].hosts.clone() });
                    }
                }
                out.extend(free.into_iter().map(|(to, m)| (version, to, m)));
                break;
            }
        }
        for (version, to, msg) in out {
            self.deliver(r, version, to, msg);
        }
    }

    /// Replica `r`'s pending transitions written (a conflict rebases it and writes again); what
    /// each commit releases into `out`: the rows it held and took, then its own messages.
    fn write_all(&mut self, r: usize, out: &mut Vec<(u64, Target, CoordMsg)>) {
        let mut next = self.replicas[r].core.take_transition();
        while let Some(t) = next.take() {
            let take_for = self.take_for(r);
            let store = self.store.as_mut().expect("a store");
            let core = &mut self.replicas[r].core;
            if self.foreign_armed {
                if let Some(row) = store.row().cloned() {
                    self.foreign_armed = false;
                    let foreign = RigWrite::of(row.version, row.core, None, Vec::new(), ReplicaId(u64::MAX));
                    store.write(foreign).expect("the foreign writer writes at the row's version");
                }
            }
            let decisions = t.decisions.clone();
            // A store that never fails, written at the version this replica read.
            let w = RigWrite {
                version: core.version(),
                core: t.core,
                history: t.history,
                decisions: t.decisions,
                replica: core.replica(),
                outbox: t.outbox,
                take_for,
                discard_for: t.attached_now,
            };
            match store.write(w) {
                Ok((version, taken)) => {
                    core.transition_written(version);
                    let inputs = core.take_recorded();
                    let replica = core.replica();
                    if let Some(events) = self.events.as_mut() {
                        events.push(RigEvent::Commit { replica, version, inputs, decisions, linked: self.replicas[r].hosts.clone() });
                    }
                    self.replicas[r].held.extend(taken);
                    self.release_held(r, out);
                    out.extend(t.out.into_iter().map(|(to, m)| (version, to, m)));
                    next = self.replicas[r].core.take_transition();
                }
                Err(RigConflict) => {
                    // The row moved: rebased onto it, the log replayed, written again.
                    let row = store.row().cloned().expect("a conflict is with a row");
                    core.rebase(&row.core, Some(&row.history), row.version).expect("the row loads");
                    self.rebases += 1;
                    next = core.take_transition();
                }
            }
        }
    }

    /// `msg`, released at `version` by replica `r`, into the mailboxes it reaches: a host's if it
    /// is linked to `r`, every host linked to `r` for [`Target::Every`].
    fn deliver(&mut self, r: usize, version: u64, to: Target, msg: CoordMsg) {
        let recording = self.events.is_some();
        let hosts: Vec<HostId> = match to {
            Target::Host(h) if self.replicas[r].hosts.contains(&h) => vec![h],
            Target::Host(h) => {
                log::debug!("shared inline coordinator: host {h} is not linked to replica {r}; {msg:?} lost");
                Vec::new()
            }
            Target::Every => self.replicas[r].hosts.iter().copied().collect(),
        };
        for h in hosts {
            match self.boxes.get_mut(&h) {
                Some(b) => {
                    b.push_back((version, msg.clone()));
                    if recording {
                        self.delivered.entry(h).or_default().push((version, msg.clone()));
                    }
                }
                None => log::debug!("shared inline coordinator: no link for host {h}; {msg:?} lost"),
            }
        }
    }
}

impl RigCluster {
    /// An order of the login service, as the coordinator runtime handles one
    /// ([`Coordinator::login_order`]): into the gate host's mailbox (read at its link's next
    /// pumps, after what is already there). The gate host, or `None` (dropped: no gate host).
    pub fn login_order(&mut self, order: crate::LoginOrder) -> Option<HostId> {
        let r = self.ticker_index();
        self.route(r);
        let (gate, msg) = self.replicas[r].core.login_order(order)?;
        let version = self.replicas[r].core.version();
        match self.boxes.get_mut(&gate) {
            Some(b) => b.push_back((version, msg)),
            None => log::debug!("shared inline coordinator: no link for gate host {gate}; a login order lost"),
        }
        Some(gate)
    }
}

impl std::ops::Deref for RigCluster {
    type Target = Coordinator;
    fn deref(&self) -> &Coordinator {
        &self.replicas[self.ticker_index()].core
    }
}

impl std::ops::DerefMut for RigCluster {
    fn deref_mut(&mut self) -> &mut Coordinator {
        let r = self.ticker_index();
        &mut self.replicas[r].core
    }
}

/// The coordinator behind an inline link: its own, or the replicas several hosts' links
/// share ([`InlineWorldLink::shared`]).
enum Core {
    Owned(Coordinator),
    Shared(Arc<Mutex<RigCluster>>),
}

/// Salt of the roll behind a shared link's input delay (the output delay rolls unsalted).
const INPUT_DELAY_SALT: u64 = 0x5EED_1A7E;

/// A [`Coordinator`] driven in the host's thread. [`WorldLink::pump`] hands it the host's
/// messages, runs its `maintain`, commits its state at once (no store: nothing to be
/// durable to, see [`Coordinator::commit_unstored`]) and releases its outputs. With no
/// delay every output is readable right after the pump that made it — today's
/// synchronous ordering; with a delay ([`InlineWorldLink::with_delay`],
/// `InlineSchedule::coordinator_delay`) each one becomes readable 0..=`max_steps` pumps
/// later, chosen by a stateless roll of (seed, its index), order kept.
///
/// A link of its own ([`InlineWorldLink::new`]) serves one host: it reads every output,
/// whatever host it names, and hands the core the host's messages as those of the host
/// its `Attach` names (0 before one). A shared link ([`InlineWorldLink::shared`]) is one
/// of several hosts' links to one cluster (the multi-host test rig): it hands the replica it
/// reaches its host's messages as that host's — each 0..=`max_steps` of its own pumps late
/// under a delay, so different hosts' messages interleave differently per seed — runs the
/// upkeep only on replica 0 (the rig's ticker), and reads only the outputs for its host (and
/// those for every host).
pub struct InlineWorldLink {
    core: Core,
    /// The host the core is told the messages are from.
    host: HostId,
    /// The replica a shared link reaches.
    replica: usize,
    /// Messages for the core, with the pump step from which they may be handled.
    in_q: VecDeque<(u64, HostMsg)>,
    /// Outputs with the pump step from which they may be read, and the row version that
    /// released each (0: no store).
    out_q: VecDeque<(u64, u64, CoordMsg)>,
    /// `(seed, max_steps)`.
    delay: Option<(u64, u32)>,
    /// Pumps so far.
    step: u64,
    /// Outputs released so far (the roll's index).
    released: u64,
    /// Messages sent so far (a shared link's input roll index).
    sent: u64,
    /// Test hook ([`InlineWorldLink::set_up`]): `false` simulates a link that dropped.
    up: bool,
    /// The row version that released each message read so far, in reading order.
    read_versions: Vec<u64>,
}

impl InlineWorldLink {
    pub fn new(coord: Coordinator) -> Self {
        Self::over(Core::Owned(coord), HostId::SOLO)
    }

    /// Host `host`'s link to `core`, which other hosts' links share: each host reads only
    /// what the core sends it (or every host). Make every host's link before any of them
    /// is pumped: a host whose link does not exist yet misses what is sent to every host.
    pub fn shared(core: Arc<Mutex<RigCluster>>, host: HostId) -> Self {
        Self::shared_at(core, 0, host)
    }

    /// [`Self::shared`], to replica `replica` of the cluster.
    pub fn shared_at(core: Arc<Mutex<RigCluster>>, replica: usize, host: HostId) -> Self {
        {
            let mut c = lock(&core);
            let replicas = c.replicas.len();
            c.boxes.entry(host).or_default();
            c.replicas
                .get_mut(replica)
                .unwrap_or_else(|| panic!("replica {replica} of a cluster of {replicas}: no such replica"))
                .hosts
                .insert(host);
        }
        let mut link = Self::over(Core::Shared(core), host);
        link.replica = replica;
        link
    }

    fn over(core: Core, host: HostId) -> Self {
        InlineWorldLink {
            core,
            host,
            replica: 0,
            in_q: VecDeque::new(),
            out_q: VecDeque::new(),
            delay: None,
            step: 0,
            released: 0,
            sent: 0,
            up: true,
            read_versions: Vec::new(),
        }
    }

    /// Each output readable 0..=`max_steps` pumps after the one that made it (and, on a
    /// shared link, each message handled 0..=`max_steps` pumps after it was sent).
    pub fn with_delay(mut self, seed: u64, max_steps: u32) -> Self {
        self.delay = (max_steps > 0).then_some((seed, max_steps));
        self
    }

    /// The delay `schedule` asks for (`InlineSchedule::coordinator_delay`), if any.
    pub fn with_schedule(self, schedule: &crate::InlineSchedule) -> Self {
        match schedule.coordinator_link_delay() {
            Some((seed, max)) => self.with_delay(seed, max),
            None => self,
        }
    }

    /// The link's own coordinator. Panics for a shared link: lock [`Self::shared_core`].
    pub fn coordinator(&self) -> &Coordinator {
        match &self.core {
            Core::Owned(c) => c,
            Core::Shared(_) => panic!("a shared inline link: lock its shared_core()"),
        }
    }

    /// [`Self::coordinator`], to change. Panics for a shared link.
    pub fn coordinator_mut(&mut self) -> &mut Coordinator {
        match &mut self.core {
            Core::Owned(c) => c,
            Core::Shared(_) => panic!("a shared inline link: lock its shared_core()"),
        }
    }

    /// The core a shared link shares; `None` for a link of its own.
    pub fn shared_core(&self) -> Option<Arc<Mutex<RigCluster>>> {
        match &self.core {
            Core::Owned(_) => None,
            Core::Shared(c) => Some(c.clone()),
        }
    }

    /// The replica a shared link reaches.
    pub fn replica_index(&self) -> usize {
        self.replica
    }

    /// Test hook (Elastic 2b Task 5): this shared link moves to replica `to` — its old link goes
    /// down there at `now_ms` ([`RigCluster::move_host`]), and whatever was on it, either way, is
    /// lost. The link is down until [`Self::set_up`]; the host then attaches to `to`.
    pub fn move_to(&mut self, to: usize, now_ms: u64) {
        let Core::Shared(core) = &self.core else { panic!("only a shared link moves between replicas") };
        lock(core).move_host(self.host, to, now_ms);
        self.replica = to;
        self.in_q.clear();
        self.out_q.clear();
        self.up = false;
    }

    /// Test hook (Elastic 2b Task 6): this shared link, whose replica's process died
    /// ([`RigCluster::kill_replica`]), dials replica `to` instead — what was on it is lost, and it
    /// is down until [`Self::set_up`]; the host then attaches to `to`.
    pub fn relink_to(&mut self, to: usize) {
        let Core::Shared(core) = &self.core else { panic!("only a shared link moves between replicas") };
        lock(core).relink_host(self.host, to);
        self.replica = to;
        self.in_q.clear();
        self.out_q.clear();
        self.up = false;
    }

    /// The row version that released each message this link read so far, in reading order (a
    /// host reads in commit order: never decreasing).
    pub fn read_versions(&self) -> &[u64] {
        &self.read_versions
    }

    /// Messages sent and not handed to the coordinator yet (a shared link's delay).
    pub fn pending(&self) -> impl Iterator<Item = &HostMsg> {
        self.in_q.iter().map(|(_, m)| m)
    }

    /// Outputs made but not read yet (delayed or not drained).
    pub fn in_transit(&self) -> usize {
        self.out_q.len()
    }

    /// Test hook: take the link down (`false`) or bring it back (`true`). While down,
    /// [`WorldLink::is_up`] is false, whatever the host sends is lost (as on a dropped
    /// connection) and nothing is readable; the coordinator keeps running (its calendar
    /// moves on each pump), and what it sent meanwhile — or had on the link before —
    /// is read once the link is up again: stale messages arriving after a reconnect.
    pub fn set_up(&mut self, up: bool) {
        self.up = up;
    }

    /// Test hook: lose every output not read yet, as a dropped connection loses what was
    /// on it (the coordinator still made them: a request it answered is answered again
    /// from its memo when the host replays it).
    pub fn lose_in_transit(&mut self) {
        self.out_q.clear();
    }

    /// Test hook: lose every message sent and not handed to the coordinator yet (a shared
    /// link's delay), as a coordinator process that died loses what it had not read.
    pub fn lose_unhandled(&mut self) {
        self.in_q.clear();
    }
}

fn lock(core: &Mutex<RigCluster>) -> std::sync::MutexGuard<'_, RigCluster> {
    core.lock().unwrap_or_else(|e| e.into_inner())
}

impl WorldLink for InlineWorldLink {
    fn send(&mut self, msg: HostMsg) {
        if self.up {
            // A shared link's message waits 0..=max pumps (order kept); otherwise none.
            let lag = match (&self.core, self.delay) {
                (Core::Shared(_), Some((seed, max))) => mix(seed ^ INPUT_DELAY_SALT, self.sent) % (u64::from(max) + 1),
                _ => 0,
            };
            self.sent += 1;
            let floor = self.in_q.back().map_or(0, |(due, _)| *due);
            self.in_q.push_back((floor.max(self.step + 1 + lag), msg));
        } else {
            log::debug!("inline world link down: {msg:?} lost");
        }
    }

    fn recv(&mut self) -> Option<CoordMsg> {
        if !self.up {
            return None;
        }
        if self.out_q.front().is_some_and(|(due, _, _)| *due <= self.step) {
            self.out_q.pop_front().map(|(_, version, m)| {
                self.read_versions.push(version);
                m
            })
        } else {
            None
        }
    }

    fn is_up(&self) -> bool {
        self.up
    }

    fn pump(&mut self, now_ms: u64) {
        self.step += 1;
        let step = self.step;
        let outs: Vec<(u64, CoordMsg)> = match &mut self.core {
            Core::Owned(coord) => {
                while let Some((_, msg)) = self.in_q.pop_front() {
                    if let HostMsg::Attach { host, .. } = &msg {
                        self.host = *host;
                    }
                    coord.apply(Input::Host { host: self.host, msg }, now_ms);
                }
                coord.apply(Input::Maintain, now_ms);
                coord.commit_unstored();
                coord.take_out().into_iter().map(|(_, m)| (0, m)).collect()
            }
            Core::Shared(core) => {
                let mut cluster = lock(core);
                let r = self.replica;
                if cluster.replicas[r].dead {
                    // Its process died: what this link sends is lost until it is moved.
                    self.in_q.clear();
                    return;
                }
                while self.in_q.front().is_some_and(|(due, _)| *due <= step) {
                    let (_, msg) = self.in_q.pop_front().expect("checked");
                    if let HostMsg::Attach { boot, .. } = &msg {
                        // The boot this link's host attached as: what its outbox rows are taken for.
                        cluster.replicas[r].boots.insert(self.host, *boot);
                    }
                    cluster.replicas[r].core.apply(Input::Host { host: self.host, msg }, now_ms);
                }
                // Timed work runs on the ticker only (Elastic 2b decision 2, Task 6): the core
                // says whether it is the one.
                if cluster.replicas[r].core.is_ticker() {
                    cluster.replicas[r].core.apply(Input::Maintain, now_ms);
                }
                cluster.route(r);
                cluster.boxes.get_mut(&self.host).map(|b| b.drain(..).collect()).unwrap_or_default()
            }
        };
        // Never before the output ahead of it: the order is kept.
        let mut floor = self.out_q.back().map_or(self.step, |(due, _, _)| (*due).max(self.step));
        for (version, msg) in outs {
            let lag = self.delay.map_or(0, |(seed, max)| mix(seed, self.released) % (u64::from(max) + 1));
            self.released += 1;
            floor = floor.max(self.step + lag);
            self.out_q.push_back((floor, version, msg));
        }
    }

    fn as_inline_mut(&mut self) -> Option<&mut InlineWorldLink> {
        Some(self)
    }

    fn as_inline(&self) -> Option<&InlineWorldLink> {
        Some(self)
    }
}
