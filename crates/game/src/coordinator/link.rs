//! The host's end of its link to the world coordinator, and the inline link that runs a
//! [`Coordinator`] in the host's own thread (single-player on wasm, tests).

use crate::HostId;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use super::{CoordMsg, Coordinator, HostMsg, Target};

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

/// One [`Coordinator`] several hosts' inline links share ([`InlineWorldLink::shared`]),
/// with each host's messages not read by its link yet. Derefs to the coordinator.
pub struct SharedCore {
    coordinator: Coordinator,
    boxes: BTreeMap<HostId, VecDeque<CoordMsg>>,
}

impl SharedCore {
    pub fn new(coordinator: Coordinator) -> Arc<Mutex<SharedCore>> {
        Arc::new(Mutex::new(SharedCore { coordinator, boxes: BTreeMap::new() }))
    }

    /// What may leave goes to the mailboxes of the hosts it is for (a message for a host
    /// with no link is lost, as on a real link).
    fn route(&mut self) {
        for (to, msg) in self.coordinator.take_out() {
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

impl std::ops::Deref for SharedCore {
    type Target = Coordinator;
    fn deref(&self) -> &Coordinator {
        &self.coordinator
    }
}

impl std::ops::DerefMut for SharedCore {
    fn deref_mut(&mut self) -> &mut Coordinator {
        &mut self.coordinator
    }
}

/// The coordinator behind an inline link: its own, or one core several hosts' links share
/// ([`InlineWorldLink::shared`]).
enum Core {
    Owned(Coordinator),
    Shared(Arc<Mutex<SharedCore>>),
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
    pub fn shared(core: Arc<Mutex<SharedCore>>, host: HostId) -> Self {
        lock(&core).boxes.entry(host).or_default();
        Self::over(Core::Shared(core), host)
    }

    fn over(core: Core, host: HostId) -> Self {
        InlineWorldLink {
            core,
            host,
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
    pub fn shared_core(&self) -> Option<Arc<Mutex<SharedCore>>> {
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
}

fn lock(core: &Mutex<SharedCore>) -> std::sync::MutexGuard<'_, SharedCore> {
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
                    coord.handle(self.host, msg, now_ms);
                }
                coord.maintain(now_ms);
                coord.commit_unstored();
                coord.take_out().into_iter().map(|(_, m)| m).collect()
            }
            Core::Shared(core) => {
                let mut coord = lock(core);
                while self.in_q.front().is_some_and(|(due, _)| *due <= step) {
                    let (_, msg) = self.in_q.pop_front().expect("checked");
                    coord.handle(self.host, msg, now_ms);
                }
                coord.maintain(now_ms);
                coord.commit_unstored();
                coord.route();
                coord.boxes.get_mut(&self.host).map(|b| b.drain(..).collect()).unwrap_or_default()
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
