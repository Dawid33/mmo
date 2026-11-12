//! One host's end of a link to a peer host, and the in-process link for tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::PeerMsg;

/// One host's end of the link to a peer.
///
/// **Contract** — every implementation must keep it; [`super::PeerSession`]'s
/// exactly-once delivery depends on it:
/// - Within one connection, messages arrive in the order sent (FIFO each way), without
///   loss except at the connection's end; what is in transit when a connection ends is lost.
/// - [`PeerLink::connection`] changes on every (re)connect, and after it changes no
///   message of an older connection is ever returned by [`PeerLink::recv`]. A link that
///   cannot guarantee this by construction must tag its frames with their connection and
///   filter out stale ones.
pub trait PeerLink {
    fn send(&mut self, msg: PeerMsg);
    fn recv(&mut self) -> Option<PeerMsg>;
    fn is_up(&self) -> bool;
    /// The link's connection count: it changes on every (re)connect, like
    /// `WorldLink::generation`. A session that sees it change while [`PeerLink::is_up`]
    /// stayed true treats it as a dropped and new connection. Required: see the contract.
    fn connection(&self) -> u64;
    /// Drive an in-process link ([`InlinePeerLink`]); the session calls it before draining
    /// [`PeerLink::recv`]. A no-op for a real link.
    fn pump(&mut self, _now_ms: u64) {}
}

/// splitmix64 of `seed` and `i`: the stateless roll behind the inline link's delays.
fn mix(seed: u64, i: u64) -> u64 {
    let mut z = seed ^ i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The state both ends of an [`InlinePeerLink`] share. Index `e` is end `e`'s: `q[e]`
/// holds the messages TO end `e`, with the receiver's pump step from which each may be
/// read.
struct Shared {
    up: bool,
    connection: u64,
    q: [VecDeque<(u64, PeerMsg)>; 2],
    /// Pumps of each end.
    step: [u64; 2],
    /// Messages sent to each end so far (the roll's index).
    sent: [u64; 2],
}

/// One end of an in-process peer link ([`InlinePeerLink::pair`]). Each
/// [`PeerLink::pump`] advances this end's step; a message becomes readable by the other
/// end 0..=`max_delay_steps` of ITS pumps after it was sent — chosen by a stateless roll
/// of (seed, direction, index) — but never before an earlier message of the same
/// direction (order kept).
pub struct InlinePeerLink {
    shared: Arc<Mutex<Shared>>,
    /// This end's index (0 or 1).
    end: usize,
    seed: u64,
    max_delay_steps: u32,
}

impl InlinePeerLink {
    /// The two ends of one link, up, on connection 1.
    pub fn pair(seed: u64, max_delay_steps: u32) -> (InlinePeerLink, InlinePeerLink) {
        let shared = Arc::new(Mutex::new(Shared {
            up: true,
            connection: 1,
            q: [VecDeque::new(), VecDeque::new()],
            step: [0; 2],
            sent: [0; 2],
        }));
        let end = |end| InlinePeerLink { shared: shared.clone(), end, seed, max_delay_steps };
        (end(0), end(1))
    }

    fn shared(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Test hook: take the whole link down (`false`) for both ends — everything in
    /// transit either way is lost — or bring it back (`true`) as a NEW connection
    /// ([`PeerLink::connection`] counts up on both ends). Setting the state it already
    /// has does nothing.
    pub fn set_up(&mut self, up: bool) {
        let mut s = self.shared();
        if s.up == up {
            return;
        }
        s.up = up;
        if up {
            s.connection += 1;
        } else {
            s.q[0].clear();
            s.q[1].clear();
        }
    }

    /// Messages to this end not read yet (delayed or not drained).
    pub fn in_transit(&self) -> usize {
        self.shared().q[self.end].len()
    }

    /// Test support: the messages to this end not read yet, in order.
    pub fn transit(&self) -> Vec<PeerMsg> {
        self.shared().q[self.end].iter().map(|(_, m)| m.clone()).collect()
    }
}

impl PeerLink for InlinePeerLink {
    fn send(&mut self, msg: PeerMsg) {
        let to = 1 - self.end;
        let mut s = self.shared();
        if !s.up {
            log::debug!("inline peer link down: {msg:?} lost");
            return;
        }
        let lag = mix(self.seed ^ to as u64, s.sent[to]) % (u64::from(self.max_delay_steps) + 1);
        s.sent[to] += 1;
        // Never before the message ahead of it: the order is kept.
        let due = s.q[to].back().map_or(0, |(due, _)| *due).max(s.step[to] + lag);
        s.q[to].push_back((due, msg));
    }

    fn recv(&mut self) -> Option<PeerMsg> {
        let me = self.end;
        let mut s = self.shared();
        if !s.up {
            return None;
        }
        if s.q[me].front().is_some_and(|(due, _)| *due <= s.step[me]) {
            s.q[me].pop_front().map(|(_, m)| m)
        } else {
            None
        }
    }

    fn is_up(&self) -> bool {
        self.shared().up
    }

    fn connection(&self) -> u64 {
        self.shared().connection
    }

    fn pump(&mut self, _now_ms: u64) {
        let me = self.end;
        self.shared().step[me] += 1;
    }
}
