//! The host's end of its link to the world coordinator, and the inline link that runs a
//! [`Coordinator`] in the host's own thread (single-player on wasm, tests).

use crate::HostId;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use super::{CallStamp, CoordMsg, Coordinator, HostMsg, Input, ReplicaId, StoreDecision, Target};

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
    /// Write the projection over the row as read at `version` (0 = no row yet), with the store
    /// decisions of its transition (written by `replica`): the new version, or
    /// `Err(RigConflict)` when `version` is not the row's — nothing written, no decision kept.
    /// `history` `None` keeps the row's.
    pub fn write(
        &mut self,
        version: u64,
        core: Vec<u8>,
        history: Option<Vec<u8>>,
        decisions: Vec<StoreDecision>,
        replica: ReplicaId,
    ) -> Result<u64, RigConflict> {
        let current = self.row.as_ref().map_or(0, |r| r.version);
        if version != current {
            return Err(RigConflict);
        }
        let history = history.unwrap_or_else(|| self.row.take().map(|r| r.history).unwrap_or_default());
        self.row = Some(RigRow { version: current + 1, core, history });
        self.writes += 1;
        let stamp = CallStamp { replica, version: current + 1 };
        self.committed.extend(decisions.into_iter().map(|d| (stamp, d)));
        Ok(current + 1)
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

/// One coordinator process of the rig: its core, the row version it last read or wrote, and
/// the hosts linked to it.
#[doc(hidden)]
pub struct RigReplica {
    pub core: Coordinator,
    pub version: u64,
    pub hosts: BTreeSet<HostId>,
}

/// The replicas several hosts' inline links share ([`InlineWorldLink::shared`]), the store
/// behind them, and each host's messages not read by its link yet. Derefs to replica 0's
/// coordinator.
///
/// With a store ([`RigCluster::with_store`], the seeded rig) every pump drives a replica's
/// transitions ([`Coordinator::take_transition`]): each is written to the row at the version
/// the replica last read, as a store that never fails, before its messages are routed. A write
/// at a stale version (another writer moved the row) rebases the replica onto the row
/// ([`Coordinator::rebase`]) and writes again, as the runtime does.
/// Without one the core is unstored ([`Coordinator::commit_unstored`]).
#[doc(hidden)]
pub struct RigCluster {
    replicas: Vec<RigReplica>,
    store: Option<RigStore>,
    boxes: BTreeMap<HostId, VecDeque<CoordMsg>>,
    /// [`Self::arm_foreign_version`]: before the next write of a replica that may rebase, a
    /// foreign writer moves the row.
    foreign_armed: bool,
    /// Rebases done after a conflict, so far.
    rebases: u64,
}

impl RigCluster {
    pub fn new(coordinator: Coordinator) -> Arc<Mutex<RigCluster>> {
        let replica = RigReplica { core: coordinator, version: 0, hosts: BTreeSet::new() };
        Arc::new(Mutex::new(RigCluster {
            replicas: vec![replica],
            store: None,
            boxes: BTreeMap::new(),
            foreign_armed: false,
            rebases: 0,
        }))
    }

    /// Test hook: right before the next write of a replica holding nothing changed outside its
    /// log ([`Coordinator::unlogged_pending`]: one the runtime would rebase rather than stop), a
    /// foreign writer writes the row's own projection at the next version — the row moves, its
    /// content does not — so that write conflicts and the replica rebases.
    pub fn arm_foreign_version(&mut self) {
        self.foreign_armed = true;
    }

    /// Rebases after a conflict so far.
    pub fn rebases(&self) -> u64 {
        self.rebases
    }

    /// [`Self::new`] with a store behind the core: every transition is written to its row,
    /// and a message leaves only with (or after) its transition.
    pub fn with_store(coordinator: Coordinator) -> Arc<Mutex<RigCluster>> {
        let core = Self::new(coordinator);
        lock(&core).store = Some(RigStore::default());
        core
    }

    pub fn replicas(&self) -> &[RigReplica] {
        &self.replicas
    }

    pub fn store(&self) -> Option<&RigStore> {
        self.store.as_ref()
    }

    /// The projection as the store holds it: `(core, history)` of the last transition (`None`
    /// without a store, or before the first transition).
    pub fn stored(&self) -> Option<(Vec<u8>, Vec<u8>)> {
        self.store.as_ref()?.row().filter(|r| !r.core.is_empty()).map(|r| (r.core.clone(), r.history.clone()))
    }

    /// Transitions written so far (with a store).
    pub fn written(&self) -> u64 {
        self.store.as_ref().map_or(0, RigStore::writes)
    }

    /// Replica 0's pending transition written now (and what may leave routed), as a runtime
    /// pass ends: the store decisions it handed out are committed with it
    /// ([`Self::take_committed`]).
    pub fn commit(&mut self) {
        self.route(0);
    }

    /// The store decisions committed with transitions since the last call (Elastic 2b Task 3):
    /// for the test's simulated world store to execute and answer.
    pub fn take_committed(&mut self) -> Vec<(CallStamp, StoreDecision)> {
        self.store.as_mut().map(RigStore::take_committed).unwrap_or_default()
    }

    /// A restart of replica 0's process: the core is replaced by `coordinator` (loaded
    /// from [`Self::stored`]), which has read the row as it stands, and whatever the old
    /// process had not handed to a link yet — its outgoing queue, the hosts' mailboxes — is
    /// lost with it.
    pub fn restart(&mut self, mut coordinator: Coordinator) {
        let version = self.store.as_ref().and_then(|s| s.row()).map_or(0, |r| r.version);
        coordinator.set_version(version);
        self.replicas[0].core = coordinator;
        self.replicas[0].version = version;
        for b in self.boxes.values_mut() {
            b.clear();
        }
    }

    /// What may leave replica `r` goes to the mailboxes of the hosts it is for (a message
    /// for a host with no link is lost, as on a real link).
    fn route(&mut self, r: usize) {
        let out = match self.store.as_mut() {
            Some(store) => {
                let replica = &mut self.replicas[r];
                let mut out = Vec::new();
                let mut next = replica.core.take_transition();
                while let Some(t) = next.take() {
                    if self.foreign_armed && !replica.core.unlogged_pending() {
                        if let Some(row) = store.row().cloned() {
                            self.foreign_armed = false;
                            store
                                .write(row.version, row.core, None, Vec::new(), ReplicaId(u64::MAX))
                                .expect("the foreign writer writes at the row's version");
                        }
                    }
                    // A store that never fails, written at the version this replica read.
                    let me = replica.core.replica();
                    match store.write(replica.version, t.core, t.history, t.decisions, me) {
                        Ok(version) => {
                            replica.version = version;
                            replica.core.transition_written(version);
                            out = t.out;
                        }
                        Err(RigConflict) => {
                            // The row moved: rebased onto it, the log replayed, written again.
                            assert!(
                                !replica.core.unlogged_pending(),
                                "a conflict with changes outside the log: the runtime would stop here"
                            );
                            let row = store.row().cloned().expect("a conflict is with a row");
                            replica.core.rebase(&row.core, Some(&row.history), row.version).expect("the row loads");
                            replica.version = row.version;
                            self.rebases += 1;
                            next = replica.core.take_transition();
                        }
                    }
                }
                out.extend(replica.core.take_out());
                out
            }
            None => {
                let core = &mut self.replicas[r].core;
                core.commit_unstored();
                core.take_out()
            }
        };
        for (to, msg) in out {
            match to {
                Target::Host(h) => match self.boxes.get_mut(&h) {
                    Some(b) => b.push_back(msg),
                    None => log::debug!("shared inline coordinator: no link for host {h}; {msg:?} lost"),
                },
                Target::Every => {
                    for b in self.boxes.values_mut() {
                        b.push_back(msg.clone());
                    }
                }
            }
        }
    }
}

impl RigCluster {
    /// An order of the login service, as the coordinator runtime handles one
    /// ([`Coordinator::login_order`]): into the gate host's mailbox (read at its link's next
    /// pumps, after what is already there). The gate host, or `None` (dropped: no gate host).
    pub fn login_order(&mut self, order: crate::LoginOrder) -> Option<HostId> {
        self.route(0);
        let (gate, msg) = self.replicas[0].core.login_order(order)?;
        match self.boxes.get_mut(&gate) {
            Some(b) => b.push_back(msg),
            None => log::debug!("shared inline coordinator: no link for gate host {gate}; a login order lost"),
        }
        Some(gate)
    }
}

impl std::ops::Deref for RigCluster {
    type Target = Coordinator;
    fn deref(&self) -> &Coordinator {
        &self.replicas[0].core
    }
}

impl std::ops::DerefMut for RigCluster {
    fn deref_mut(&mut self) -> &mut Coordinator {
        &mut self.replicas[0].core
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
/// of several hosts' links to one core (the multi-host test rig): it hands the core its
/// host's messages as that host's — each 0..=`max_steps` of its own pumps late under a
/// delay, so different hosts' messages interleave differently per seed — and reads only
/// the outputs for its host (and those for every host).
pub struct InlineWorldLink {
    core: Core,
    /// The host the core is told the messages are from.
    host: HostId,
    /// The replica a shared link reaches.
    replica: usize,
    /// Messages for the core, with the pump step from which they may be handled.
    in_q: VecDeque<(u64, HostMsg)>,
    /// Outputs with the pump step from which they may be read.
    out_q: VecDeque<(u64, CoordMsg)>,
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
        if self.out_q.front().is_some_and(|(due, _)| *due <= self.step) {
            self.out_q.pop_front().map(|(_, m)| m)
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
        let outs: Vec<CoordMsg> = match &mut self.core {
            Core::Owned(coord) => {
                while let Some((_, msg)) = self.in_q.pop_front() {
                    if let HostMsg::Attach { host, .. } = &msg {
                        self.host = *host;
                    }
                    coord.apply(Input::Host { host: self.host, msg }, now_ms);
                }
                coord.apply(Input::Maintain, now_ms);
                coord.commit_unstored();
                coord.take_out().into_iter().map(|(_, m)| m).collect()
            }
            Core::Shared(core) => {
                let mut cluster = lock(core);
                let r = self.replica;
                while self.in_q.front().is_some_and(|(due, _)| *due <= step) {
                    let (_, msg) = self.in_q.pop_front().expect("checked");
                    cluster.replicas[r].core.apply(Input::Host { host: self.host, msg }, now_ms);
                }
                cluster.replicas[r].core.apply(Input::Maintain, now_ms);
                cluster.route(r);
                cluster.boxes.get_mut(&self.host).map(|b| b.drain(..).collect()).unwrap_or_default()
            }
        };
        // Never before the output ahead of it: the order is kept.
        let mut floor = self.out_q.back().map_or(self.step, |(due, _)| (*due).max(self.step));
        for msg in outs {
            let lag = self.delay.map_or(0, |(seed, max)| mix(seed, self.released) % (u64::from(max) + 1));
            self.released += 1;
            floor = floor.max(self.step + lag);
            self.out_q.push_back((floor, msg));
        }
    }

    fn as_inline_mut(&mut self) -> Option<&mut InlineWorldLink> {
        Some(self)
    }

    fn as_inline(&self) -> Option<&InlineWorldLink> {
        Some(self)
    }
}
