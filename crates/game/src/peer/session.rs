//! One host's session with one peer host over a [`PeerLink`]: reliable messages numbered,
//! acknowledged and resent across reconnects; volatile ones sent only while reachable.

use std::collections::VecDeque;

use super::{PeerHello, PeerLink, PeerMsg, Reliable, Volatile};
use crate::CutId;
use crate::HostId;

/// What the session hands its owner from a [`PeerSession::pump`].
#[derive(Debug, Clone, PartialEq)]
pub enum PeerEvent {
    /// The peer's valid hello of a new connection (once per connection).
    Up(PeerHello),
    /// A connection that was [`PeerEvent::Up`] went away, or the connection broke: a gap
    /// in the peer's numbering, or the peer acknowledged (by `Ack` or by its hello's
    /// [`PeerHello::received_through`]) more than was ever sent to it. Once per connection.
    Down,
    /// The next reliable message, exactly once, in the peer's order.
    Reliable(Reliable),
    Volatile(Volatile),
    /// The peer runs a newer world generation than this host (once per connection):
    /// nothing is exchanged with it; this host is stale.
    HigherGeneration(u64),
    /// The peer said hello as a boot this session refuses ([`PeerSession::refuse_boot`]: a
    /// process declared dead, R4): nothing is processed until the link's next connection
    /// (the owner closes this one).
    Refused(u64),
}

/// Where the current connection stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Conn {
    /// No connection (the link is down, or not seen up yet).
    None,
    /// Our hello sent; the peer's not arrived yet.
    AwaitHello,
    /// Both hellos exchanged, generations equal.
    Up,
    /// The peer runs a higher generation (reported) or a lower one (ignored): nothing
    /// in, nothing out until its next hello.
    OtherGeneration,
    /// A gap in the peer's numbering: nothing is processed until the link's next
    /// connection.
    Broken,
}

/// One host's session with one peer host.
///
/// **Reliable** ([`PeerSession::queue`]): numbered per directed link from 1, kept until
/// the peer acknowledges them (cumulatively, `Ack { through }`). The outgoing numbering
/// belongs to one boot pair (this boot → one peer boot); the incoming count belongs to
/// one peer boot. On the peer's hello of a connection:
/// - the incoming count restarts at 0 when the hello's boot is not the one it counted;
/// - the outgoing side RESUMES — keeps its numbers, forgets what
///   [`PeerHello::received_through`] covers, resends the rest — only when the numbering
///   is addressed to the hello's boot AND the hello counted against this boot
///   ([`PeerHello::received_from_boot`]); in every other case (a new peer boot, or a
///   peer that never counted this boot's numbers) it RESTARTS: everything still owed is
///   renumbered from 1 and sent again. A resumed count above anything sent breaks the
///   connection. **Volatile** ([`PeerSession::send_volatile`]): sent only while
/// [`PeerSession::is_reachable`], lost with the connection, never replayed. Within one
/// [`PeerSession::pump`] reliable messages go out before volatile ones; there is no order
/// between the two classes. **Frozen** ([`PeerSession::freeze`], a world cut counted across a
/// down link) nothing moves either way and both counts stand still until thawed.
pub struct PeerSession {
    me: PeerHello,
    peer: HostId,
    /// Queued or sent and not acknowledged, with their numbers, in order.
    out: VecDeque<(u64, Reliable)>,
    /// The number the next queued message gets.
    next_seq: u64,
    /// Every entry up to this number went out on the current connection.
    sent_through: u64,
    /// The highest number ever put on the wire in the current outgoing numbering.
    max_sent: u64,
    /// The peer boot the outgoing numbering is addressed to (`None` = no peer hello yet).
    send_boot: Option<u64>,
    /// The last reliable number received from the peer boot `recv_boot` (0 = none).
    received_through: u64,
    /// The peer boot `received_through` counts (`None` = no peer hello yet).
    recv_boot: Option<u64>,
    /// The link's connection last seen up (`None` = the link was down at the last look).
    connection: Option<u64>,
    state: Conn,
    /// Volatile messages sent while reachable, out on the next pump.
    volatile_out: Vec<Volatile>,
    held: bool,
    /// [`PeerSession::freeze`]: nothing read, nothing sent, no new connection taken up.
    frozen: bool,
    /// [`PeerSession::hold_on_new_boot`]: a hello of another peer boot holds the session
    /// before anything goes out to it.
    hold_new_boot: bool,
    /// [`PeerSession::reset_connection`]: the connection seen last is gone, whatever the
    /// link's connection value says.
    reset: bool,
    /// Peer boots whose hello is refused ([`PeerSession::refuse_boot`]).
    refused: std::collections::BTreeSet<u64>,
    /// Refused boots already logged (one warning per boot).
    refused_logged: std::collections::BTreeSet<u64>,
}

impl PeerSession {
    /// The session of host `me` with host `peer`. `me.received_through` and
    /// `me.received_from_boot` are ignored: the session fills them into every hello it sends.
    pub fn new(me: PeerHello, peer: HostId) -> Self {
        PeerSession {
            me,
            peer,
            out: VecDeque::new(),
            next_seq: 1,
            sent_through: 0,
            max_sent: 0,
            send_boot: None,
            received_through: 0,
            recv_boot: None,
            connection: None,
            state: Conn::None,
            volatile_out: Vec::new(),
            held: false,
            frozen: false,
            hold_new_boot: false,
            reset: false,
            refused: std::collections::BTreeSet::new(),
            refused_logged: std::collections::BTreeSet::new(),
        }
    }

    /// From now on a hello of peer boot `boot` is refused ([`PeerEvent::Refused`]): that
    /// process was declared dead (R4, a succession) — a zombie of it must exchange nothing.
    /// A hello of ANOTHER boot of the peer is a new host as ever (fresh numbering both ways).
    /// The current connection, if any, counts as gone ([`Self::reset_connection`]): the next
    /// pump sends a new hello and waits for the peer's.
    pub fn refuse_boot(&mut self, boot: u64) {
        self.refused.insert(boot);
        self.reset_connection();
    }

    /// Everything queued, or sent and not acknowledged, taken out in order: the session
    /// owes it to nobody any more (R4: a survivor re-routes what it owed a dead peer). The
    /// next message queued follows the last number ever put on the wire: a later boot of the
    /// peer renumbers it from 1 at its hello, as for every new peer boot; a boot already
    /// counting this numbering (R4 drain: a leaver's new boot whose hello came while the
    /// session was held — nothing of it was ever sent) sees no gap.
    pub fn take_unreceived(&mut self) -> Vec<Reliable> {
        self.volatile_out.clear();
        let taken = self.out.drain(..).map(|(_, r)| r).collect();
        self.next_seq = self.max_sent + 1;
        taken
    }

    /// The link object was replaced; whatever connection was up is gone. The next
    /// [`PeerSession::pump`] treats the link's connection as a new one — even when its
    /// [`PeerLink::connection`] value equals the old link's — exactly as a connection
    /// lost and made again: [`PeerEvent::Down`] if it was up, then a new hello; what was
    /// sent and not acknowledged stays owed and is resent after the peer's hello.
    pub fn reset_connection(&mut self) {
        self.reset = true;
    }

    /// The peer host's ordinal.
    pub fn peer(&self) -> HostId {
        self.peer
    }

    /// Always accepted; kept until the peer acknowledges it (or
    /// [`PeerSession::drop_through_mark`] forgets it). Sent on the next pump that finds
    /// the peer up and the session not held.
    pub fn queue(&mut self, body: Reliable) {
        self.out.push_back((self.next_seq, body));
        self.next_seq += 1;
    }

    /// `false` = dropped: the peer is not reachable. A message accepted here is still lost
    /// if the connection drops before the next pump sends it.
    pub fn send_volatile(&mut self, v: Volatile) -> bool {
        if !self.is_reachable() {
            return false;
        }
        self.volatile_out.push(v);
        true
    }

    /// The link up (at the last pump), both hellos of this connection exchanged,
    /// generations equal, and not held.
    pub fn is_reachable(&self) -> bool {
        self.state == Conn::Up && !self.held && !self.frozen
    }

    /// What this session has received from the peer: the last reliable number of the peer
    /// boot it counts, and that boot (`None`: no hello of the peer yet). Frozen
    /// ([`Self::freeze`]), it cannot grow: a world cut's count (`world_cut.rs`).
    pub fn received_through(&self) -> (u64, Option<u64>) {
        (self.received_through, self.recv_boot)
    }

    /// The boot of this host's process, as its hello names it (the boot a peer's count of
    /// this session's numbering is counted against).
    pub fn own_boot(&self) -> u64 {
        self.me.boot
    }

    /// What is still owed past the peer's count `count` of this numbering, in order: queued,
    /// or sent and not acknowledged, numbered above `count`. With a frozen peer (its count
    /// cannot grow) none of it reaches the peer before this session is thawed.
    pub fn owed_above(&self, count: u64) -> impl Iterator<Item = &Reliable> {
        self.out
            .iter()
            .filter(move |(seq, _)| *seq > count)
            .map(|(_, r)| r)
    }

    /// Freeze the session for a world cut counted across a down link (`world_cut.rs`, count
    /// mode): from the next pump until [`Self::thaw`] nothing is read from the link (what
    /// arrives waits there, in order), nothing is sent (reliable messages keep queuing,
    /// volatile ones are dropped) and no new connection is taken up (a connection lost meanwhile
    /// still reports `Down`). The incoming count ([`Self::received_through`]) and the outgoing
    /// numbering cannot change while frozen. Independent of [`Self::hold`]: thawing never
    /// releases a hold, and releasing a hold never thaws.
    pub fn freeze(&mut self) {
        self.frozen = true;
        self.volatile_out.clear();
    }

    /// Undo [`Self::freeze`]: the next pump reads what waited on the link (or takes up the
    /// link's new connection with a hello) and sends as usual.
    pub fn thaw(&mut self) {
        self.frozen = false;
    }

    /// Whether the session is frozen ([`Self::freeze`]).
    pub fn is_frozen(&self) -> bool {
        self.frozen
    }

    /// The current connection broke (a gap in the peer's numbering, an impossible ack):
    /// nothing is processed until the link's next connection.
    pub fn is_broken(&self) -> bool {
        self.state == Conn::Broken
    }

    /// Every reliable message queued, or sent and not acknowledged, in order — held or not.
    pub fn unreceived(&self) -> impl Iterator<Item = &Reliable> {
        self.out.iter().map(|(_, r)| r)
    }

    /// The part of [`PeerSession::unreceived`] never put on the wire in the current
    /// numbering (the rest was sent and is not acknowledged yet: the peer may hold it).
    pub fn unsent(&self) -> impl Iterator<Item = &Reliable> {
        let sent = self.max_sent;
        self.out
            .iter()
            .filter(move |(seq, _)| *seq > sent)
            .map(|(_, r)| r)
    }

    /// Stop sending reliable messages (they keep queuing) and volatile ones (dropped): a
    /// leaving peer. Acknowledgements still go out.
    pub fn hold(&mut self) {
        self.held = true;
        self.volatile_out.clear();
    }

    /// While set, a hello of a peer boot other than the one this session's numbering is
    /// addressed to holds the session ([`PeerSession::hold`]) before anything is sent to it:
    /// the peer is leaving in a world cut not decided yet (`world_cut.rs`).
    pub fn hold_on_new_boot(&mut self, on: bool) {
        self.hold_new_boot = on;
    }

    /// Renumber what is queued from 1 if nothing of the current numbering was ever sent
    /// (a held session whose entries before a cut's mark were just forgotten,
    /// [`PeerSession::drop_through_mark`], after a new peer boot's hello: it expects 1).
    pub fn renumber_unsent(&mut self) {
        if self.max_sent != 0 {
            return;
        }
        for (i, (seq, _)) in self.out.iter_mut().enumerate() {
            *seq = i as u64 + 1;
        }
        self.next_seq = self.out.len() as u64 + 1;
        self.sent_through = 0;
    }

    /// Undo [`PeerSession::hold`]: the queue goes out, in order, on the next pump.
    pub fn release(&mut self) {
        self.held = false;
    }

    /// Forget every entry numbered up to `count` — the peer's count of this numbering in a
    /// world cut counted across a down link (`world_cut.rs`): it received them. Later
    /// entries stay owed. As [`Self::drop_through_mark`], only for a HELD session whose peer
    /// comes back as a new boot.
    pub fn drop_through(&mut self, count: u64) {
        debug_assert!(self.held, "drop_through on a session that is not held");
        while self.out.front().is_some_and(|(seq, _)| *seq <= count) {
            self.out.pop_front();
        }
    }

    /// Forget this side's `CutMark { cut }` and every entry before it (those belong to the
    /// cut); later entries stay owed. Without such a mark in the buffer, nothing changes.
    ///
    /// Only for a HELD session whose peer will come back with a new boot (a leaving
    /// host): the new boot's hello renumbers what is left from 1. With an unchanged peer
    /// boot it would leave a hole in the numbering (the peer sees a gap).
    pub fn drop_through_mark(&mut self, cut: CutId) {
        debug_assert!(self.held, "drop_through_mark on a session that is not held");
        let Some(at) = self
            .out
            .iter()
            .position(|(_, r)| matches!(r, Reliable::CutMark { cut: c } if *c == cut))
        else {
            return;
        };
        self.out.drain(..=at);
    }

    /// Drive the link, read everything it has, answer and send: the events in order.
    pub fn pump(&mut self, link: &mut dyn PeerLink, now_ms: u64) -> Vec<PeerEvent> {
        let mut events = Vec::new();
        link.pump(now_ms);
        let up = link.is_up();
        let connection = link.connection();
        // The connection we knew went away (down, or replaced between two looks, or its
        // link object replaced).
        if self.connection.is_some() && (!up || self.connection != Some(connection) || self.reset) {
            if self.state == Conn::Up {
                events.push(PeerEvent::Down);
            }
            self.lose_connection();
        }
        self.reset = false;
        if !up || self.frozen {
            // Frozen: what arrives waits on the link, and a new connection is taken up only
            // once thawed (its hello would move the counts).
            return events;
        }
        if self.connection.is_none() {
            self.connection = Some(connection);
            self.state = Conn::AwaitHello;
            link.send(PeerMsg::Hello(PeerHello {
                received_through: self.received_through,
                received_from_boot: self.recv_boot,
                ..self.me.clone()
            }));
        }
        let mut ack_due = false;
        while let Some(msg) = link.recv() {
            match (self.state, msg) {
                (Conn::Broken, _) => {}
                (Conn::AwaitHello | Conn::OtherGeneration, PeerMsg::Hello(h)) => {
                    self.hello(h, &mut events)
                }
                (Conn::Up, PeerMsg::Reliable { seq, body }) => {
                    ack_due = true;
                    if seq <= self.received_through {
                        continue;
                    }
                    if seq != self.received_through + 1 {
                        log::warn!(
                            "peer {}: reliable {seq} after {}: a gap, the connection is broken",
                            self.peer,
                            self.received_through
                        );
                        self.break_connection(&mut events);
                        ack_due = false;
                        continue;
                    }
                    self.received_through = seq;
                    events.push(PeerEvent::Reliable(body));
                }
                (Conn::Up, PeerMsg::Ack { through }) => {
                    if through > self.max_sent {
                        log::warn!(
                            "peer {}: ack of {through}, but only {} was sent: the connection is broken",
                            self.peer,
                            self.max_sent
                        );
                        self.break_connection(&mut events);
                        ack_due = false;
                        continue;
                    }
                    while self.out.front().is_some_and(|(seq, _)| *seq <= through) {
                        self.out.pop_front();
                    }
                }
                (Conn::Up, PeerMsg::Volatile(v)) => events.push(PeerEvent::Volatile(v)),
                (state, msg) => log::debug!(
                    "peer {}: {:?} dropped while {state:?}",
                    self.peer,
                    msg.redacted()
                ),
            }
        }
        if self.state != Conn::Up {
            return events;
        }
        if ack_due {
            link.send(PeerMsg::Ack {
                through: self.received_through,
            });
        }
        if !self.held {
            let from = self.sent_through;
            for (seq, body) in self.out.iter().filter(|(seq, _)| *seq > from) {
                link.send(PeerMsg::Reliable {
                    seq: *seq,
                    body: body.clone(),
                });
                self.sent_through = *seq;
                self.max_sent = self.max_sent.max(*seq);
            }
            for v in self.volatile_out.drain(..) {
                link.send(PeerMsg::Volatile(v));
            }
        }
        events
    }

    /// A broken connection: `Down`, and nothing is processed until the link's next one.
    fn break_connection(&mut self, events: &mut Vec<PeerEvent>) {
        events.push(PeerEvent::Down);
        self.state = Conn::Broken;
        self.volatile_out.clear();
    }

    /// The connection is gone: everything per connection resets; what was sent and not
    /// acknowledged stays owed, the volatile outbox is lost.
    fn lose_connection(&mut self) {
        self.connection = None;
        self.state = Conn::None;
        self.sent_through = 0;
        self.volatile_out.clear();
    }

    /// The peer's hello of this connection.
    fn hello(&mut self, h: PeerHello, events: &mut Vec<PeerEvent>) {
        if h.id != self.peer {
            log::warn!("peer {}: hello from host {} ignored", self.peer, h.id);
            return;
        }
        if self.refused.contains(&h.boot) {
            // Once per boot: a zombie redials every few seconds.
            if self.refused_logged.insert(h.boot) {
                log::warn!(
                    "peer {}: hello of boot {}, declared dead: refused (and every later one)",
                    self.peer,
                    h.boot
                );
            } else {
                log::debug!(
                    "peer {}: hello of boot {}, declared dead: refused",
                    self.peer,
                    h.boot
                );
            }
            self.state = Conn::Broken;
            self.volatile_out.clear();
            events.push(PeerEvent::Refused(h.boot));
            return;
        }
        if h.generation != self.me.generation {
            if h.generation > self.me.generation {
                log::warn!(
                    "peer {}: generation {} is newer than ours ({})",
                    self.peer,
                    h.generation,
                    self.me.generation
                );
                events.push(PeerEvent::HigherGeneration(h.generation));
            } else {
                log::info!(
                    "peer {}: generation {} is older than ours, ignored",
                    self.peer,
                    h.generation
                );
            }
            self.state = Conn::OtherGeneration;
            return;
        }
        let resume = self.send_boot == Some(h.boot) && h.received_from_boot == Some(self.me.boot);
        if resume && h.received_through > self.max_sent {
            log::warn!(
                "peer {}: hello says {} received, but only {} was sent: the connection is broken",
                self.peer,
                h.received_through,
                self.max_sent
            );
            self.break_connection(events);
            return;
        }
        // Incoming: a count of another peer boot is void.
        if self.recv_boot != Some(h.boot) {
            if self.recv_boot.is_some() {
                log::info!(
                    "peer {}: new boot {}, its numbering starts again",
                    self.peer,
                    h.boot
                );
            }
            self.recv_boot = Some(h.boot);
            self.received_through = 0;
        }
        if resume {
            // Same boot pair, counted against us: resume after what the peer has.
            while self
                .out
                .front()
                .is_some_and(|(seq, _)| *seq <= h.received_through)
            {
                self.out.pop_front();
            }
            self.sent_through = h.received_through;
        } else {
            // The peer has none of our numbers (a new peer boot, or a peer that never
            // counted this boot): restart the numbering of everything still owed.
            log::debug!(
                "peer {}: renumbering {} unacknowledged from 1",
                self.peer,
                self.out.len()
            );
            if self.hold_new_boot && self.send_boot.is_some_and(|b| b != h.boot) {
                // A leaving peer's next boot (R4 drain): nothing of the leaving boot's time
                // goes to it before the leave is decided here.
                self.held = true;
            }
            self.send_boot = Some(h.boot);
            for (i, (seq, _)) in self.out.iter_mut().enumerate() {
                *seq = i as u64 + 1;
            }
            self.next_seq = self.out.len() as u64 + 1;
            self.sent_through = 0;
            self.max_sent = 0;
        }
        self.state = Conn::Up;
        events.push(PeerEvent::Up(h));
    }
}
