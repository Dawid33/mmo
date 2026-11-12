//! One host's session with one peer host over a [`PeerLink`]: reliable messages numbered,
//! acknowledged and resent across reconnects; volatile ones sent only while reachable.

use std::collections::VecDeque;

use super::{PeerHello, PeerLink, PeerMsg, Reliable, Volatile};
use crate::CutId;

/// What the session hands its owner from a [`PeerSession::pump`].
#[derive(Debug, Clone, PartialEq)]
pub enum PeerEvent {
    /// The peer's valid hello of a new connection (once per connection).
    Up(PeerHello),
    /// A connection that was [`PeerEvent::Up`] went away (or broke: a gap in the
    /// numbering). Once per connection.
    Down,
    /// The next reliable message, exactly once, in the peer's order.
    Reliable(Reliable),
    Volatile(Volatile),
    /// The peer runs a newer world generation than this host (once per connection):
    /// nothing is exchanged with it; this host is stale.
    HigherGeneration(u64),
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
/// the peer acknowledges them (cumulatively, `Ack { through }`), resent after a
/// reconnect from the peer's [`PeerHello::received_through`] + 1 with their original
/// numbers. A peer hello with a new boot voids the numbering both ways: everything still
/// unacknowledged is renumbered from 1 and sent again, and the count received from the
/// peer restarts at 0. **Volatile** ([`PeerSession::send_volatile`]): sent only while
/// [`PeerSession::is_reachable`], lost with the connection, never replayed. Within one
/// [`PeerSession::pump`] reliable messages go out before volatile ones; there is no order
/// between the two classes.
pub struct PeerSession {
    me: PeerHello,
    peer: u32,
    /// Queued or sent and not acknowledged, with their numbers, in order.
    out: VecDeque<(u64, Reliable)>,
    /// The number the next queued message gets.
    next_seq: u64,
    /// Every entry up to this number went out on the current connection.
    sent_through: u64,
    /// The last reliable number received from the peer in this boot pair (0 = none).
    received_through: u64,
    /// The peer boot of the current numbering (`None` = no peer hello yet).
    peer_boot: Option<u64>,
    /// The link's connection last seen up (`None` = the link was down at the last look).
    connection: Option<u64>,
    state: Conn,
    /// Volatile messages sent while reachable, out on the next pump.
    volatile_out: Vec<Volatile>,
    held: bool,
}

impl PeerSession {
    /// The session of host `me` with host `peer`. `me.received_through` is ignored: the
    /// session fills it into every hello it sends.
    pub fn new(me: PeerHello, peer: u32) -> Self {
        PeerSession {
            me,
            peer,
            out: VecDeque::new(),
            next_seq: 1,
            sent_through: 0,
            received_through: 0,
            peer_boot: None,
            connection: None,
            state: Conn::None,
            volatile_out: Vec::new(),
            held: false,
        }
    }

    /// The peer host's ordinal.
    pub fn peer(&self) -> u32 {
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
        self.state == Conn::Up && !self.held
    }

    /// Every reliable message queued, or sent and not acknowledged, in order — held or not.
    pub fn unreceived(&self) -> impl Iterator<Item = &Reliable> {
        self.out.iter().map(|(_, r)| r)
    }

    /// Stop sending reliable messages (they keep queuing) and volatile ones (dropped): a
    /// leaving peer. Acknowledgements still go out.
    pub fn hold(&mut self) {
        self.held = true;
        self.volatile_out.clear();
    }

    /// Undo [`PeerSession::hold`]: the queue goes out, in order, on the next pump.
    pub fn release(&mut self) {
        self.held = false;
    }

    /// Forget this side's `CutMark { cut }` and every entry before it (those belong to the
    /// cut); later entries stay owed. Without such a mark in the buffer, nothing changes.
    pub fn drop_through_mark(&mut self, cut: CutId) {
        let Some(at) = self.out.iter().position(|(_, r)| matches!(r, Reliable::CutMark { cut: c } if *c == cut)) else {
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
        // The connection we knew went away (down, or replaced between two looks).
        if self.connection.is_some() && (!up || self.connection != Some(connection)) {
            if self.state == Conn::Up {
                events.push(PeerEvent::Down);
            }
            self.lose_connection();
        }
        if !up {
            return events;
        }
        if self.connection.is_none() {
            self.connection = Some(connection);
            self.state = Conn::AwaitHello;
            link.send(PeerMsg::Hello(PeerHello { received_through: self.received_through, ..self.me.clone() }));
        }
        let mut ack_due = false;
        while let Some(msg) = link.recv() {
            match (self.state, msg) {
                (Conn::Broken, _) => {}
                (Conn::AwaitHello | Conn::OtherGeneration, PeerMsg::Hello(h)) => self.hello(h, &mut events),
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
                        events.push(PeerEvent::Down);
                        self.state = Conn::Broken;
                        self.volatile_out.clear();
                        ack_due = false;
                        continue;
                    }
                    self.received_through = seq;
                    events.push(PeerEvent::Reliable(body));
                }
                (Conn::Up, PeerMsg::Ack { through }) => {
                    while self.out.front().is_some_and(|(seq, _)| *seq <= through) {
                        self.out.pop_front();
                    }
                }
                (Conn::Up, PeerMsg::Volatile(v)) => events.push(PeerEvent::Volatile(v)),
                (state, msg) => log::debug!("peer {}: {msg:?} dropped while {state:?}", self.peer),
            }
        }
        if self.state != Conn::Up {
            return events;
        }
        if ack_due {
            link.send(PeerMsg::Ack { through: self.received_through });
        }
        if !self.held {
            let from = self.sent_through;
            for (seq, body) in self.out.iter().filter(|(seq, _)| *seq > from) {
                link.send(PeerMsg::Reliable { seq: *seq, body: body.clone() });
                self.sent_through = *seq;
            }
            for v in self.volatile_out.drain(..) {
                link.send(PeerMsg::Volatile(v));
            }
        }
        events
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
        if h.ordinal != self.peer {
            log::warn!("peer {}: hello from host {} ignored", self.peer, h.ordinal);
            return;
        }
        if h.generation != self.me.generation {
            if h.generation > self.me.generation {
                log::warn!("peer {}: generation {} is newer than ours ({})", self.peer, h.generation, self.me.generation);
                events.push(PeerEvent::HigherGeneration(h.generation));
            } else {
                log::info!("peer {}: generation {} is older than ours, ignored", self.peer, h.generation);
            }
            self.state = Conn::OtherGeneration;
            return;
        }
        if self.peer_boot == Some(h.boot) {
            // Same boot pair: resume after what the peer has.
            while self.out.front().is_some_and(|(seq, _)| *seq <= h.received_through) {
                self.out.pop_front();
            }
            self.sent_through = h.received_through;
        } else {
            // A new peer boot: the old numbering is void both ways.
            if self.peer_boot.is_some() {
                log::info!("peer {}: new boot {}, renumbering {} unacknowledged", self.peer, h.boot, self.out.len());
            }
            self.peer_boot = Some(h.boot);
            for (i, (seq, _)) in self.out.iter_mut().enumerate() {
                *seq = i as u64 + 1;
            }
            self.next_seq = self.out.len() as u64 + 1;
            self.sent_through = 0;
            self.received_through = 0;
        }
        self.state = Conn::Up;
        events.push(PeerEvent::Up(h));
    }
}
