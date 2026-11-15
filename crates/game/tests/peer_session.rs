//! `game::peer`: the host↔host session over a seeded, delaying in-process link. Every
//! test runs 32 seeds; a failure names its seed.

use crate::common::h;
use std::collections::VecDeque;

use game::{
    CutId, HostEndpoint, InlinePeerLink, PeerEvent, PeerHello, PeerLink, PeerMsg, PeerSession, RegionCoords, Reliable,
    Volatile,
};

const SEEDS: u64 = 32;
/// Most pumps a test waits for something to settle.
const MAX_ROUNDS: usize = 4000;

/// splitmix64 of `seed` and `i`: the tests' seeded choices.
fn roll(seed: u64, i: u64) -> u64 {
    let mut z = seed ^ i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn hello(ordinal: game::HostId, boot: u64, generation: u64) -> PeerHello {
    PeerHello {
        id: ordinal,
        boot,
        generation,
        endpoint: HostEndpoint { id: ordinal, quic: None, webtransport: None, cert_hash: None },
        received_through: 0,
        received_from_boot: None,
    }
}

/// A numbered payload: the cheapest real reliable message that carries a number.
fn payload(n: u64) -> Reliable {
    Reliable::Kick { player_token: format!("p{n}"), session_epoch: n, reason: String::new() }
}

fn number(r: &Reliable) -> u64 {
    match r {
        Reliable::Kick { session_epoch, .. } => *session_epoch,
        Reliable::CutMark { cut } => 1_000_000 + cut.0,
        other => panic!("unexpected reliable {other:?}"),
    }
}

/// A wire tap around one end of an [`InlinePeerLink`]: records what the session sends and
/// reads (with the connection it was read on), and can inject, re-deliver or drop frames.
struct Tap {
    inner: InlinePeerLink,
    /// Frames the session sent through this end.
    sent: Vec<PeerMsg>,
    /// Frames the session read through this end, with the connection they arrived on.
    read: Vec<(u64, PeerMsg)>,
    /// Returned by `recv` before anything from the link.
    inject: VecDeque<PeerMsg>,
    /// After reading the reliable frame with this seq, return it once more (next pump).
    dup_seq: Option<u64>,
    /// Swallow the first reliable frame with this seq.
    drop_seq: Option<u64>,
    /// Frames to inject from the next pump on.
    next_pump: Vec<PeerMsg>,
}

impl Tap {
    fn new(inner: InlinePeerLink) -> Self {
        Tap {
            inner,
            sent: Vec::new(),
            read: Vec::new(),
            inject: VecDeque::new(),
            dup_seq: None,
            drop_seq: None,
            next_pump: Vec::new(),
        }
    }

    fn set_up(&mut self, up: bool) {
        self.inner.set_up(up);
    }

    /// Reliable seqs read on connection `c`.
    fn seqs_on(&self, c: u64) -> Vec<u64> {
        self.read
            .iter()
            .filter_map(|(conn, m)| match m {
                PeerMsg::Reliable { seq, .. } if *conn == c => Some(*seq),
                _ => None,
            })
            .collect()
    }

    fn reliable_sent(&self) -> usize {
        self.sent.iter().filter(|m| matches!(m, PeerMsg::Reliable { .. })).count()
    }
}

impl PeerLink for Tap {
    fn send(&mut self, msg: PeerMsg) {
        self.sent.push(msg.clone());
        self.inner.send(msg);
    }

    fn recv(&mut self) -> Option<PeerMsg> {
        let msg = match self.inject.pop_front() {
            Some(m) => m,
            None => loop {
                let m = self.inner.recv()?;
                if let PeerMsg::Reliable { seq, .. } = &m {
                    if self.drop_seq == Some(*seq) {
                        self.drop_seq = None;
                        continue;
                    }
                    if self.dup_seq == Some(*seq) {
                        self.dup_seq = None;
                        self.next_pump.push(m.clone());
                    }
                }
                break m;
            },
        };
        self.read.push((self.inner.connection(), msg.clone()));
        Some(msg)
    }

    fn is_up(&self) -> bool {
        self.inner.is_up()
    }

    fn connection(&self) -> u64 {
        self.inner.connection()
    }

    fn pump(&mut self, now_ms: u64) {
        self.inject.extend(self.next_pump.drain(..));
        self.inner.pump(now_ms);
    }
}

/// One host's side: its session, its end of the link, and what it has seen.
struct Side {
    s: PeerSession,
    link: Tap,
    got: Vec<u64>,
    vol: Vec<Volatile>,
    events: Vec<PeerEvent>,
}

impl Side {
    fn new(me: PeerHello, peer: game::HostId, link: InlinePeerLink) -> Self {
        Side { s: PeerSession::new(me, peer), link: Tap::new(link), got: Vec::new(), vol: Vec::new(), events: Vec::new() }
    }

    fn pump(&mut self, now: u64) {
        for e in self.s.pump(&mut self.link, now) {
            match &e {
                PeerEvent::Reliable(r) => self.got.push(number(r)),
                PeerEvent::Volatile(v) => self.vol.push(v.clone()),
                _ => {}
            }
            self.events.push(e);
        }
    }

    fn unreceived(&self) -> Vec<u64> {
        self.s.unreceived().map(number).collect()
    }

    fn count(&self, f: impl Fn(&PeerEvent) -> bool) -> usize {
        self.events.iter().filter(|e| f(e)).count()
    }
}

fn pair(seed: u64, max_delay: u32) -> (Side, Side) {
    let (la, lb) = InlinePeerLink::pair(seed, max_delay);
    (Side::new(hello(h(0), 100, 1), h(1), la), Side::new(hello(h(1), 200, 1), h(0), lb))
}

/// Pumps both sides (in a seeded order) until `done` or `MAX_ROUNDS`.
fn run(seed: u64, a: &mut Side, b: &mut Side, now: &mut u64, done: impl Fn(&Side, &Side) -> bool) {
    for _ in 0..MAX_ROUNDS {
        if done(a, b) {
            return;
        }
        *now += 1;
        if roll(seed ^ 0xABCD, *now) % 2 == 0 {
            a.pump(*now);
            b.pump(*now);
        } else {
            b.pump(*now);
            a.pump(*now);
        }
    }
    panic!("seed {seed}: did not settle in {MAX_ROUNDS} rounds");
}

fn rounds(a: &mut Side, b: &mut Side, now: &mut u64, n: usize) {
    for _ in 0..n {
        *now += 1;
        a.pump(*now);
        b.pump(*now);
    }
}

fn rc(x: i32) -> RegionCoords {
    RegionCoords::new(x, 0)
}

#[test]
fn reliable_exactly_once_in_order_across_flaps() {
    const N: u64 = 200;
    for seed in 0..SEEDS {
        let (mut a, mut b) = pair(seed, 3);
        let mut now = 0;
        // Five seeded rounds (of the first ~300) at which the link drops, each for a
        // seeded number of rounds.
        let mut flaps: Vec<u64> = (0..5).map(|i| 1 + roll(seed, i) % 300).collect();
        flaps.sort();
        let (mut queued_a, mut queued_b) = (0, 0);
        let mut down_until = None;
        for round in 1..=(MAX_ROUNDS as u64) {
            if queued_a == N && queued_b == N && a.got.len() == N as usize && b.got.len() == N as usize {
                break;
            }
            // Each side queues 0..=2 payloads per round.
            for _ in 0..roll(seed ^ 1, round) % 3 {
                if queued_a < N {
                    queued_a += 1;
                    a.s.queue(payload(queued_a));
                }
            }
            for _ in 0..roll(seed ^ 2, round) % 3 {
                if queued_b < N {
                    queued_b += 1;
                    b.s.queue(payload(queued_b));
                }
            }
            if down_until.is_some_and(|r| r == round) {
                (if round % 2 == 0 { &mut a } else { &mut b }).link.set_up(true);
                down_until = None;
            }
            if down_until.is_none() && flaps.first() == Some(&round) {
                flaps.remove(0);
                (if round % 2 == 0 { &mut b } else { &mut a }).link.set_up(false);
                down_until = Some(round + 1 + roll(seed ^ 3, round) % 8);
            }
            now += 1;
            a.pump(now);
            b.pump(now);
        }
        let want: Vec<u64> = (1..=N).collect();
        assert_eq!(b.got, want, "seed {seed}: b received a's payloads exactly once, in order");
        assert_eq!(a.got, want, "seed {seed}: a received b's payloads exactly once, in order");
        assert!(a.count(|e| matches!(e, PeerEvent::Down)) >= 1, "seed {seed}: the link flapped");
        run(seed, &mut a, &mut b, &mut now, |a, b| a.s.unreceived().count() == 0 && b.s.unreceived().count() == 0);
    }
}

#[test]
fn volatile_is_dropped_while_down_and_not_replayed() {
    for seed in 0..SEEDS {
        let (mut a, mut b) = pair(seed, 3);
        let mut now = 0;
        assert!(!a.s.send_volatile(Volatile::RegionUp(rc(0))), "seed {seed}: not reachable before any hello");
        run(seed, &mut a, &mut b, &mut now, |a, b| a.s.is_reachable() && b.s.is_reachable());
        assert!(a.s.send_volatile(Volatile::RegionUp(rc(1))), "seed {seed}");
        run(seed, &mut a, &mut b, &mut now, |_, b| !b.vol.is_empty());
        // Sent while reachable, then the link drops before (or while) it travels.
        assert!(a.s.send_volatile(Volatile::RegionUp(rc(2))), "seed {seed}");
        if roll(seed, 0) % 2 == 0 {
            now += 1;
            a.pump(now);
        }
        a.link.set_up(false);
        rounds(&mut a, &mut b, &mut now, 3);
        assert!(!a.s.is_reachable() && !b.s.is_reachable(), "seed {seed}: down");
        assert!(!a.s.send_volatile(Volatile::RegionUp(rc(3))), "seed {seed}: dropped while down");
        assert!(!b.s.send_volatile(Volatile::RegionDown(rc(3))), "seed {seed}: dropped while down");
        b.link.set_up(true);
        run(seed, &mut a, &mut b, &mut now, |a, b| a.s.is_reachable() && b.s.is_reachable());
        rounds(&mut a, &mut b, &mut now, 10);
        assert!(a.s.send_volatile(Volatile::RegionUp(rc(4))), "seed {seed}");
        run(seed, &mut a, &mut b, &mut now, |_, b| b.vol.len() >= 2);
        rounds(&mut a, &mut b, &mut now, 10);
        assert_eq!(
            b.vol,
            vec![Volatile::RegionUp(rc(1)), Volatile::RegionUp(rc(4))],
            "seed {seed}: what was sent across the drop is gone, never replayed"
        );
        assert!(a.vol.is_empty(), "seed {seed}");
        assert_eq!(a.count(|e| matches!(e, PeerEvent::Up(_))), 2, "seed {seed}: one Up per connection");
        assert_eq!(a.count(|e| matches!(e, PeerEvent::Down)), 1, "seed {seed}: one Down per lost connection");
    }
}

#[test]
fn new_peer_boot_restarts_numbering_and_requeues() {
    for seed in 0..SEEDS {
        let (mut a, mut b) = pair(seed, 3);
        let mut now = 0;
        for n in 1..=50 {
            a.s.queue(payload(n));
        }
        for n in 1..=20 {
            b.s.queue(payload(n));
        }
        // A seeded part gets through, then b's process dies with the link.
        rounds(&mut a, &mut b, &mut now, (roll(seed, 0) % 40) as usize);
        a.link.set_up(false);
        let owed = a.unreceived();
        let b_old_got = b.got.clone();
        // The restarted b: a new boot, nothing remembered.
        let mut b2 = Side::new(hello(h(1), 201, 1), h(0), std::mem::replace(&mut b.link, Tap::new(InlinePeerLink::pair(0, 0).0)).inner);
        for n in 101..=105 {
            b2.s.queue(payload(n));
        }
        b2.link.set_up(true);
        run(seed, &mut a, &mut b2, &mut now, |a, b2| {
            a.s.unreceived().count() == 0 && b2.s.unreceived().count() == 0 && a.got.iter().any(|n| *n == 105)
        });
        rounds(&mut a, &mut b2, &mut now, 10);
        assert_eq!(b2.got, owed, "seed {seed}: the new boot gets everything unacknowledged, in order");
        assert_eq!(b2.count(|e| matches!(e, PeerEvent::Down)), 0, "seed {seed}: numbering restarted at 1 (no gap)");
        let mut all = b_old_got.clone();
        all.extend(&owed);
        all.sort();
        all.dedup();
        assert_eq!(all, (1..=50).collect::<Vec<_>>(), "seed {seed}: nothing lost between the two boots");
        // a forgot its count of b's old boot: the new boot's 1..=5 are delivered, not dropped as duplicates.
        let a_tail: Vec<u64> = a.got.iter().copied().filter(|n| *n > 100).collect();
        assert_eq!(a_tail, (101..=105).collect::<Vec<_>>(), "seed {seed}: a delivers the new boot's messages");
        let ups: Vec<u64> = a.events.iter().filter_map(|e| if let PeerEvent::Up(h) = e { Some(h.boot) } else { None }).collect();
        assert_eq!(ups.last(), Some(&201), "seed {seed}: a saw the new boot come up");
    }
}

#[test]
fn higher_generation_is_reported_and_nothing_delivered() {
    for seed in 0..SEEDS {
        let (la, lb) = InlinePeerLink::pair(seed, 3);
        let mut a = Side::new(hello(h(0), 100, 1), h(1), la);
        let mut b = Side::new(hello(h(1), 200, 2), h(0), lb);
        let mut now = 0;
        for n in 1..=10 {
            a.s.queue(payload(n));
            b.s.queue(payload(n));
        }
        rounds(&mut a, &mut b, &mut now, 40);
        assert!(!a.s.send_volatile(Volatile::RegionUp(rc(1))), "seed {seed}");
        assert!(!b.s.send_volatile(Volatile::RegionUp(rc(1))), "seed {seed}");
        rounds(&mut a, &mut b, &mut now, 40);
        assert_eq!(
            a.events.iter().filter(|e| matches!(e, PeerEvent::HigherGeneration(2))).count(),
            1,
            "seed {seed}: reported once"
        );
        assert_eq!(a.events.len(), 1, "seed {seed}: nothing but the report: {:?}", a.events);
        assert!(b.events.is_empty(), "seed {seed}: a lower generation is ignored: {:?}", b.events);
        assert!(!a.s.is_reachable() && !b.s.is_reachable(), "seed {seed}");
        assert_eq!(a.unreceived(), (1..=10).collect::<Vec<_>>(), "seed {seed}: nothing acknowledged");
        assert_eq!(b.unreceived(), (1..=10).collect::<Vec<_>>(), "seed {seed}: nothing acknowledged");
    }
}

#[test]
fn held_session_queues_and_release_sends_in_order() {
    for seed in 0..SEEDS {
        let (mut a, mut b) = pair(seed, 3);
        let mut now = 0;
        a.s.hold();
        for n in 1..=10 {
            a.s.queue(payload(n));
        }
        run(seed, &mut a, &mut b, &mut now, |_, b| b.s.is_reachable());
        rounds(&mut a, &mut b, &mut now, 30);
        assert!(b.got.is_empty(), "seed {seed}: held: nothing sent");
        assert!(!a.s.is_reachable(), "seed {seed}: held is not reachable");
        assert!(!a.s.send_volatile(Volatile::RegionUp(rc(1))), "seed {seed}: held drops volatile");
        a.s.release();
        run(seed, &mut a, &mut b, &mut now, |a, _| a.s.unreceived().count() == 0);
        assert_eq!(b.got, (1..=10).collect::<Vec<_>>(), "seed {seed}");
        // Held mid-stream.
        a.s.hold();
        for n in 11..=20 {
            a.s.queue(payload(n));
        }
        rounds(&mut a, &mut b, &mut now, 30);
        assert_eq!(b.got.len(), 10, "seed {seed}: held again: nothing new sent");
        assert_eq!(a.unreceived(), (11..=20).collect::<Vec<_>>(), "seed {seed}");
        a.s.release();
        run(seed, &mut a, &mut b, &mut now, |a, _| a.s.unreceived().count() == 0);
        assert_eq!(b.got, (1..=20).collect::<Vec<_>>(), "seed {seed}: released in order");
        assert!(b.vol.is_empty(), "seed {seed}");
    }
}

#[test]
fn drop_through_mark_forgets_exactly_the_prefix() {
    for seed in 0..SEEDS {
        let (mut a, mut b) = pair(seed, 3);
        let mut now = 0;
        // A seeded part of 1..=10 gets through and is acknowledged, then a holds (a
        // leaving peer), queues its mark and more.
        for n in 1..=10 {
            a.s.queue(payload(n));
        }
        rounds(&mut a, &mut b, &mut now, (roll(seed, 0) % 30) as usize);
        a.s.hold();
        let before = a.unreceived();
        let k = roll(seed, 1) % 5;
        for n in 11..11 + k {
            a.s.queue(payload(n));
        }
        a.s.queue(Reliable::CutMark { cut: CutId(7) });
        for n in 50..=55 {
            a.s.queue(payload(n));
        }
        let mut all = before.clone();
        all.extend(11..11 + k);
        all.push(1_000_007);
        all.extend(50..=55);
        assert_eq!(a.unreceived(), all, "seed {seed}");
        a.s.drop_through_mark(CutId(99));
        assert_eq!(a.unreceived(), all, "seed {seed}: no such mark: nothing forgotten");
        a.s.drop_through_mark(CutId(7));
        assert_eq!(a.unreceived(), (50..=55).collect::<Vec<_>>(), "seed {seed}: exactly the prefix forgotten");
        // The peer comes back as a new boot: it gets exactly what is still owed.
        a.link.set_up(false);
        let mut b2 = Side::new(hello(h(1), 201, 1), h(0), std::mem::replace(&mut b.link, Tap::new(InlinePeerLink::pair(0, 0).0)).inner);
        b2.link.set_up(true);
        run(seed, &mut a, &mut b2, &mut now, |_, b2| b2.s.is_reachable());
        a.s.release();
        run(seed, &mut a, &mut b2, &mut now, |a, _| a.s.unreceived().count() == 0);
        assert_eq!(b2.got, (50..=55).collect::<Vec<_>>(), "seed {seed}");
    }
}

#[test]
fn unreceived_lists_queued_and_unacked() {
    for seed in 0..SEEDS {
        let (mut a, mut b) = pair(seed, 4);
        let mut now = 0;
        for n in 1..=30 {
            a.s.queue(payload(n));
        }
        assert_eq!(a.unreceived(), (1..=30).collect::<Vec<_>>(), "seed {seed}: queued, nothing sent");
        for round in 0..MAX_ROUNDS as u64 {
            if round == 5 + roll(seed, 0) % 20 {
                a.link.set_up(false);
            }
            if round == 30 + roll(seed, 1) % 20 {
                a.link.set_up(true);
            }
            now += 1;
            a.pump(now);
            b.pump(now);
            let u = a.unreceived();
            let first = u.first().copied().unwrap_or(31);
            assert_eq!(u, (first..=30).collect::<Vec<_>>(), "seed {seed} round {round}: a suffix, in order");
            assert!(
                first as usize <= b.got.len() + 1,
                "seed {seed} round {round}: never forgets what b has not received ({first} vs {})",
                b.got.len()
            );
            if u.is_empty() && round > 60 {
                break;
            }
        }
        assert!(a.unreceived().is_empty(), "seed {seed}: all acknowledged in the end");
        assert_eq!(b.got, (1..=30).collect::<Vec<_>>(), "seed {seed}");
    }
}

/// A connected pair where b has sent a 1..=15 (all delivered and acknowledged) and a has
/// sent b 1..=20 (a seeded part through); then b's process dies with the link.
fn crash_b(seed: u64, now: &mut u64) -> (Side, Side, Vec<u64>) {
    let (mut a, mut b) = pair(seed, 3);
    for n in 1..=15 {
        b.s.queue(payload(n));
    }
    run(seed, &mut a, &mut b, now, |a, b| a.got.len() == 15 && b.s.unreceived().count() == 0);
    for n in 1..=20 {
        a.s.queue(payload(n));
    }
    rounds(&mut a, &mut b, now, (roll(seed, 7) % 12) as usize);
    a.link.set_up(false);
    let link = std::mem::replace(&mut b.link, Tap::new(InlinePeerLink::pair(0, 0).0)).inner;
    let owed = a.unreceived();
    (a, Side::new(hello(h(1), 201, 1), h(0), link), owed)
}

#[test]
fn new_boot_hello_seen_by_one_side_then_link_drops() {
    for seed in 0..SEEDS {
        for restarted_reads_first in [true, false] {
            let mut now = 0;
            let (mut a, mut b2, owed) = crash_b(seed, &mut now);
            for n in 101..=105 {
                b2.s.queue(payload(n));
            }
            b2.link.set_up(true);
            // Only one side reads the other's hello of this connection; then the link drops.
            let (first, second) = if restarted_reads_first { (&mut a, &mut b2) } else { (&mut b2, &mut a) };
            now += 1;
            first.pump(now);
            for _ in 0..MAX_ROUNDS {
                if second.s.is_reachable() {
                    break;
                }
                now += 1;
                second.pump(now);
            }
            assert!(second.s.is_reachable(), "seed {seed} {restarted_reads_first}: one side up");
            assert!(!first.s.is_reachable(), "seed {seed} {restarted_reads_first}: the other not yet");
            a.link.set_up(false);
            rounds(&mut a, &mut b2, &mut now, 2);
            b2.link.set_up(true);
            run(seed, &mut a, &mut b2, &mut now, |a, b2| {
                a.s.unreceived().count() == 0 && b2.s.unreceived().count() == 0
            });
            rounds(&mut a, &mut b2, &mut now, 10);
            let mut want_a: Vec<u64> = (1..=15).collect();
            want_a.extend(101..=105);
            assert_eq!(a.got, want_a, "seed {seed} {restarted_reads_first}: a gets the new boot's messages once, in order");
            assert_eq!(b2.got, owed, "seed {seed} {restarted_reads_first}: the new boot gets what a owed, once, in order");
        }
    }
}

fn reliable_frame(seq: u64, n: u64) -> PeerMsg {
    PeerMsg::Reliable { seq, body: payload(n) }
}

#[test]
fn duplicate_frame_is_delivered_once_and_still_acked() {
    const N: u64 = 20;
    for seed in 0..SEEDS {
        let (mut a, mut b) = pair(seed, 3);
        let mut now = 0;
        let k = 1 + roll(seed, 0) % N;
        b.link.dup_seq = Some(k);
        for n in 1..=N {
            a.s.queue(payload(n));
        }
        run(seed, &mut a, &mut b, &mut now, |a, b| a.s.unreceived().count() == 0 && b.got.len() == N as usize);
        rounds(&mut a, &mut b, &mut now, 10);
        let c = b.link.connection();
        assert_eq!(b.link.seqs_on(c).iter().filter(|s| **s == k).count(), 2, "seed {seed}: {k} re-delivered");
        assert_eq!(b.got, (1..=N).collect::<Vec<_>>(), "seed {seed}: delivered once");
        // A duplicate alone in a pump: dropped, and still acknowledged.
        let before = b.link.sent.len();
        b.link.inject.push_back(reliable_frame(N, N));
        now += 1;
        b.pump(now);
        assert_eq!(b.got.len(), N as usize, "seed {seed}: the duplicate is not delivered");
        assert!(
            b.link.sent[before..].contains(&PeerMsg::Ack { through: N }),
            "seed {seed}: a duplicate is still acknowledged: {:?}",
            &b.link.sent[before..]
        );
    }
}

#[test]
fn reconnect_resends_from_received_through_plus_one() {
    const N: u64 = 100;
    for seed in 0..SEEDS {
        let (mut a, mut b) = pair(seed, 3);
        let mut now = 0;
        let mut queued = 0;
        // (connection, what b had received when it came up)
        let mut resumes = Vec::new();
        let flaps: Vec<u64> = (0..4).map(|i| 10 + i * 25 + roll(seed, i) % 15).collect();
        for round in 0..MAX_ROUNDS as u64 {
            if queued == N && a.s.unreceived().count() == 0 {
                break;
            }
            if queued < N {
                queued += 1;
                a.s.queue(payload(queued));
            }
            if flaps.contains(&round) {
                a.link.set_up(false);
            }
            if flaps.contains(&round.wrapping_sub(3)) {
                b.link.set_up(true);
                resumes.push((b.link.connection(), b.got.len() as u64));
            }
            now += 1;
            a.pump(now);
            b.pump(now);
        }
        assert_eq!(b.got, (1..=N).collect::<Vec<_>>(), "seed {seed}");
        assert_eq!(resumes.len(), 4, "seed {seed}");
        for (c, rt) in resumes {
            let seqs = b.link.seqs_on(c);
            assert!(
                seqs.iter().all(|s| *s > rt),
                "seed {seed}: connection {c} resent at or below received_through {rt}: {seqs:?}"
            );
            let mut sorted = seqs.clone();
            sorted.dedup();
            assert_eq!(sorted, seqs, "seed {seed}: connection {c}: each seq on the wire once, in order");
        }
    }
}

#[test]
fn gap_breaks_the_connection_until_the_next() {
    const N: u64 = 20;
    for seed in 0..SEEDS {
        let (mut a, mut b) = pair(seed, 3);
        let mut now = 0;
        // Not the last: a lost tail has no later frame to show the gap.
        let k = 2 + roll(seed, 0) % (N - 2);
        b.link.drop_seq = Some(k);
        for n in 1..=N {
            a.s.queue(payload(n));
        }
        run(seed, &mut a, &mut b, &mut now, |_, b| b.count(|e| matches!(e, PeerEvent::Down)) == 1);
        rounds(&mut a, &mut b, &mut now, 20);
        assert_eq!(b.got, (1..k).collect::<Vec<_>>(), "seed {seed}: nothing after the gap at {k} on that connection");
        assert!(!b.s.is_reachable(), "seed {seed}: broken");
        assert_eq!(b.count(|e| matches!(e, PeerEvent::Down)), 1, "seed {seed}: one Down");
        a.link.set_up(false);
        rounds(&mut a, &mut b, &mut now, 2);
        a.link.set_up(true);
        run(seed, &mut a, &mut b, &mut now, |a, _| a.s.unreceived().count() == 0);
        assert_eq!(b.got, (1..=N).collect::<Vec<_>>(), "seed {seed}: exactly once after the next connection");
    }
}

#[test]
fn nothing_is_processed_before_the_peers_hello() {
    for seed in 0..SEEDS {
        let (mut a, mut b) = pair(seed, 3);
        let mut now = 0;
        for n in 1..=3 {
            a.s.queue(payload(n));
        }
        let stray = |b: &mut Side| {
            b.link.inject.push_back(reliable_frame(1, 999));
            b.link.inject.push_back(PeerMsg::Volatile(Volatile::RegionUp(rc(9))));
        };
        stray(&mut b);
        now += 1;
        b.pump(now);
        run(seed, &mut a, &mut b, &mut now, |a, _| a.s.unreceived().count() == 0);
        assert_eq!(b.got, vec![1, 2, 3], "seed {seed}: nothing before the hello is delivered");
        // And on a later connection.
        a.link.set_up(false);
        rounds(&mut a, &mut b, &mut now, 2);
        for n in 4..=5 {
            a.s.queue(payload(n));
        }
        a.link.set_up(true);
        stray(&mut b);
        now += 1;
        b.pump(now);
        run(seed, &mut a, &mut b, &mut now, |a, _| a.s.unreceived().count() == 0);
        assert_eq!(b.got, vec![1, 2, 3, 4, 5], "seed {seed}: nothing before the hello is delivered");
        assert!(b.vol.is_empty(), "seed {seed}: {:?}", b.vol);
    }
}

#[test]
fn held_session_sends_nothing_across_flaps_then_all_in_order() {
    const N: u64 = 15;
    for seed in 0..SEEDS {
        let (mut a, mut b) = pair(seed, 3);
        let mut now = 0;
        a.s.hold();
        for n in 1..=N {
            a.s.queue(payload(n));
        }
        for flap in 0..3 {
            run(seed, &mut a, &mut b, &mut now, |_, b| b.s.is_reachable());
            rounds(&mut a, &mut b, &mut now, (roll(seed, flap) % 10) as usize);
            (if flap % 2 == 0 { &mut a } else { &mut b }).link.set_up(false);
            rounds(&mut a, &mut b, &mut now, 2);
            a.link.set_up(true);
        }
        run(seed, &mut a, &mut b, &mut now, |_, b| b.s.is_reachable());
        rounds(&mut a, &mut b, &mut now, 10);
        assert_eq!(a.link.reliable_sent(), 0, "seed {seed}: nothing reliable on the wire while held");
        assert!(b.got.is_empty(), "seed {seed}");
        a.s.release();
        run(seed, &mut a, &mut b, &mut now, |a, _| a.s.unreceived().count() == 0);
        assert_eq!(b.got, (1..=N).collect::<Vec<_>>(), "seed {seed}: in order after release");
        assert_eq!(a.link.reliable_sent(), N as usize, "seed {seed}: each sent once");
    }
}

#[test]
fn ack_above_anything_sent_breaks_the_connection() {
    for seed in 0..SEEDS {
        let (mut a, mut b) = pair(seed, 3);
        let mut now = 0;
        for n in 1..=5 {
            a.s.queue(payload(n));
        }
        run(seed, &mut a, &mut b, &mut now, |a, _| a.s.unreceived().count() == 0);
        a.s.hold();
        for n in 6..=8 {
            a.s.queue(payload(n));
        }
        rounds(&mut a, &mut b, &mut now, 5);
        // 6 was never sent (held): an ack of it is a lie.
        a.link.inject.push_back(PeerMsg::Ack { through: 6 });
        now += 1;
        a.pump(now);
        assert_eq!(a.count(|e| matches!(e, PeerEvent::Down)), 1, "seed {seed}: the ack broke the connection");
        assert_eq!(a.unreceived(), vec![6, 7, 8], "seed {seed}: nothing forgotten");
        rounds(&mut a, &mut b, &mut now, 5);
        assert_eq!(a.unreceived(), vec![6, 7, 8], "seed {seed}");
        // A hello that claims more than was sent, on a new connection.
        a.link.set_up(false);
        rounds(&mut a, &mut b, &mut now, 2);
        a.link.set_up(true);
        a.link.inject.push_back(PeerMsg::Hello(PeerHello {
            received_through: 1000,
            received_from_boot: Some(100),
            ..hello(h(1), 200, 1)
        }));
        now += 1;
        a.pump(now);
        assert_eq!(a.count(|e| matches!(e, PeerEvent::Down)), 2, "seed {seed}: the hello broke the connection");
        assert_eq!(a.count(|e| matches!(e, PeerEvent::Up(_))), 1, "seed {seed}: not up on it");
        assert_eq!(a.unreceived(), vec![6, 7, 8], "seed {seed}: nothing forgotten");
        // A clean connection, released: everything arrives once.
        a.link.set_up(false);
        rounds(&mut a, &mut b, &mut now, 2);
        a.link.set_up(true);
        a.s.release();
        run(seed, &mut a, &mut b, &mut now, |a, _| a.s.unreceived().count() == 0);
        assert_eq!(b.got, (1..=8).collect::<Vec<_>>(), "seed {seed}");
    }
}

#[test]
fn lower_generation_peer_returns_at_equal_generation() {
    for seed in 0..SEEDS {
        let (la, lb) = InlinePeerLink::pair(seed, 3);
        let mut a = Side::new(hello(h(0), 100, 1), h(1), la);
        let mut b = Side::new(hello(h(1), 200, 0), h(0), lb);
        let mut now = 0;
        for n in 1..=3 {
            a.s.queue(payload(n));
        }
        rounds(&mut a, &mut b, &mut now, 30);
        assert!(a.events.is_empty(), "seed {seed}: the lower generation is ignored: {:?}", a.events);
        assert_eq!(b.events, vec![PeerEvent::HigherGeneration(1)], "seed {seed}");
        a.link.set_up(false);
        let link = std::mem::replace(&mut b.link, Tap::new(InlinePeerLink::pair(0, 0).0)).inner;
        let mut b2 = Side::new(hello(h(1), 201, 1), h(0), link);
        b2.s.queue(payload(101));
        b2.link.set_up(true);
        run(seed, &mut a, &mut b2, &mut now, |a, b2| {
            a.s.unreceived().count() == 0 && b2.s.unreceived().count() == 0
        });
        assert!(a.s.is_reachable(), "seed {seed}: reachable at an equal generation");
        assert_eq!(b2.got, vec![1, 2, 3], "seed {seed}");
        assert_eq!(a.got, vec![101], "seed {seed}");
    }
}

/// The link object is replaced by a fresh one whose `connection()` equals the old one's
/// (both on their first connection): without being told, a session would take it for
/// the same connection. `reset_connection` makes each side drop it (`Down`), send a new
/// hello and resume — everything owed both ways arrives exactly once, in order.
#[test]
fn reset_connection_forces_a_new_hello() {
    const N: u64 = 8;
    for seed in 0..SEEDS {
        let (mut a, mut b) = pair(seed, 3);
        let mut now = 0;
        for n in 1..=N {
            a.s.queue(payload(n));
            b.s.queue(payload(n));
        }
        // Part of it on its way: some sent and unacknowledged, some received.
        for _ in 0..3 {
            now += 1;
            a.pump(now);
            b.pump(now);
        }
        let (la, lb) = InlinePeerLink::pair(seed ^ 0x5EED, 3);
        assert_eq!(la.connection(), a.link.inner.connection(), "seed {seed}: the same connection value");
        a.link = Tap::new(la);
        b.link = Tap::new(lb);
        a.s.reset_connection();
        b.s.reset_connection();
        let (ea, eb) = (a.events.len(), b.events.len());
        let ups = |s: &Side, from: usize| s.events[from..].iter().filter(|e| matches!(e, PeerEvent::Up(_))).count();
        run(seed, &mut a, &mut b, &mut now, |a, b| {
            a.s.unreceived().count() == 0 && b.s.unreceived().count() == 0 && a.got.len() as u64 == N && b.got.len() as u64 == N
        });
        let all: Vec<u64> = (1..=N).collect();
        assert_eq!(a.got, all, "seed {seed}: a got everything once, in order");
        assert_eq!(b.got, all, "seed {seed}: b got everything once, in order");
        assert_eq!((ups(&a, ea), ups(&b, eb)), (1, 1), "seed {seed}: one hello each way on the new link");
        assert!(a.s.is_reachable() && b.s.is_reachable(), "seed {seed}");
    }
}

/// One end of an [`InlinePeerLink`] that counts [`PeerLink::close`] calls.
struct Closing {
    inner: std::sync::Arc<std::sync::Mutex<InlinePeerLink>>,
    closes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl PeerLink for Closing {
    fn send(&mut self, msg: PeerMsg) {
        self.inner.lock().unwrap().send(msg)
    }
    fn recv(&mut self) -> Option<PeerMsg> {
        self.inner.lock().unwrap().recv()
    }
    fn is_up(&self) -> bool {
        self.inner.lock().unwrap().is_up()
    }
    fn connection(&self) -> u64 {
        self.inner.lock().unwrap().connection()
    }
    fn pump(&mut self, now_ms: u64) {
        self.inner.lock().unwrap().pump(now_ms)
    }
    fn close(&mut self) {
        self.closes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The manager ends a connection its session declared broken (a gap: the session waits
/// for the link's next connection) by closing the link — once — and never closes one
/// that merely dropped or was replaced.
#[test]
fn manager_closes_a_link_whose_session_broke() {
    use std::sync::atomic::Ordering;
    let mut rig = crate::common::rig::Rig::with_ownership(
        game::InlineSpawner::default(),
        crate::common::seam_translate::uniform_floor,
        Box::new(game::Static { id: h(0), hosts: 2 }),
        1,
    );
    let (mine, mut theirs) = InlinePeerLink::pair(7, 0);
    let mine = std::sync::Arc::new(std::sync::Mutex::new(mine));
    let closes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    rig.manager.set_peer(
        h(1),
        PeerSession::new(hello(h(0), 100, 1), h(1)),
        Box::new(Closing { inner: mine.clone(), closes: closes.clone() }),
    );
    let mut now = 0;
    let mut pump = |rig: &mut crate::common::rig::Rig, theirs: &mut InlinePeerLink| {
        now += 1;
        rig.manager.pump_peers(now);
        theirs.pump(now);
        std::iter::from_fn(|| theirs.recv()).collect::<Vec<_>>()
    };
    // Up: our hello out, theirs in.
    let got = pump(&mut rig, &mut theirs);
    assert!(matches!(got.as_slice(), [PeerMsg::Hello(_)]), "{got:?}");
    theirs.send(PeerMsg::Hello(hello(h(1), 200, 1)));
    pump(&mut rig, &mut theirs);
    assert_eq!(closes.load(Ordering::SeqCst), 0, "an up session closes nothing");

    // A plain drop and reconnect (between two pumps: a replaced connection): no close.
    mine.lock().unwrap().set_up(false);
    mine.lock().unwrap().set_up(true);
    let got = pump(&mut rig, &mut theirs);
    assert!(matches!(got.as_slice(), [PeerMsg::Hello(_)]), "a new hello: {got:?}");
    theirs.send(PeerMsg::Hello(hello(h(1), 200, 1)));
    pump(&mut rig, &mut theirs);
    mine.lock().unwrap().set_up(false);
    pump(&mut rig, &mut theirs);
    mine.lock().unwrap().set_up(true);
    pump(&mut rig, &mut theirs);
    theirs.send(PeerMsg::Hello(hello(h(1), 200, 1)));
    pump(&mut rig, &mut theirs);
    assert_eq!(closes.load(Ordering::SeqCst), 0, "a dropped or replaced connection is not closed");

    // A gap in their numbering: broken, the link is closed once.
    theirs.send(reliable_frame(5, 5));
    pump(&mut rig, &mut theirs);
    assert_eq!(closes.load(Ordering::SeqCst), 1, "the broken connection is closed");
    theirs.send(reliable_frame(6, 6));
    pump(&mut rig, &mut theirs);
    pump(&mut rig, &mut theirs);
    assert_eq!(closes.load(Ordering::SeqCst), 1, "once");
}

/// Final review m1: what a log prints of a dropped message never shows a player token
/// (a spawn order's, a kick's, an in-cut player move's login).
#[test]
fn redacted_messages_hide_player_tokens() {
    let order = Reliable::SpawnOrder {
        origin: h(0),
        hops: 0,
        player_token: "tok-secret-1".into(),
        session_epoch: 1,
        player: game::PlayerId(3),
        record_region: None,
    };
    let kick = Reliable::InCut {
        cut: CutId(1),
        body: Box::new(Reliable::Kick { player_token: "tok-secret-2".into(), session_epoch: 1, reason: "bye".into() }),
    };
    for body in [order, kick] {
        let shown = format!("{:?}", PeerMsg::Reliable { seq: 1, body: body.clone() }.redacted());
        assert!(!shown.contains("tok-secret") && shown.contains("<secret>"), "{shown}");
        assert!(format!("{body:?}").contains("tok-secret"), "the message itself is unchanged");
    }
}

/// R4 Task 12 (a world cut counted across a down link): a's 1..=30 to b, a seeded part
/// through, b's acknowledgements of a seeded part of that lost; then the link drops and
/// both sessions freeze. b's count of a's numbering is exactly what b received (counted
/// against a's boot) and a's `owed_above` that count is exactly what b never got — the two
/// partition the stream whatever was acknowledged. The link comes back while frozen: nothing
/// is read or sent (b's count stays, a's owed stays). Thawed, b gets the rest once, in order.
#[test]
fn owed_above_and_received_through() {
    for seed in 0..SEEDS {
        let (mut a, mut b) = pair(seed, 3);
        let mut now = 0;
        run(seed, &mut a, &mut b, &mut now, |a, b| a.s.is_reachable() && b.s.is_reachable());
        assert_eq!(b.s.received_through(), (0, Some(100)), "seed {seed}: nothing yet, counted against a's boot");
        for n in 1..=30 {
            a.s.queue(payload(n));
        }
        let through = roll(seed, 0) % 25;
        for _ in 0..through {
            now += 1;
            a.pump(now);
            if roll(seed, now) % 3 != 0 {
                b.pump(now);
            } else {
                // b reads, its acknowledgement lost: what it got stays owed on a.
                PeerLink::pump(&mut b.link, now);
                for e in b.s.pump(&mut NoAcks(&mut b.link), now) {
                    if let PeerEvent::Reliable(r) = &e {
                        b.got.push(number(r));
                    }
                }
            }
        }
        a.link.set_up(false);
        a.pump(now + 1);
        b.pump(now + 1);
        now += 1;
        a.s.freeze();
        b.s.freeze();
        let (count, boot) = b.s.received_through();
        assert_eq!(boot, Some(100), "seed {seed}: counted against a's boot");
        assert_eq!(b.got, (1..=count).collect::<Vec<_>>(), "seed {seed}: the count is what b received");
        let owed: Vec<u64> = a.s.owed_above(count).map(number).collect();
        assert_eq!(owed, (count + 1..=30).collect::<Vec<_>>(), "seed {seed}: owed above the count is what b lacks");
        // Back while frozen: nothing read, nothing sent, nothing reachable.
        a.link.set_up(true);
        let sent_before = a.link.reliable_sent();
        rounds(&mut a, &mut b, &mut now, 40);
        assert_eq!(b.s.received_through(), (count, Some(100)), "seed {seed}: frozen, the count stays");
        assert_eq!(b.got.len() as u64, count, "seed {seed}: nothing above the count arrived");
        assert_eq!(a.link.reliable_sent(), sent_before, "seed {seed}: frozen, nothing sent");
        assert!(!a.s.is_reachable() && !b.s.is_reachable(), "seed {seed}: frozen is not reachable");
        assert_eq!(a.s.owed_above(count).map(number).collect::<Vec<_>>(), owed, "seed {seed}");
        // A hold is not a freeze: releasing it thaws nothing.
        a.s.release();
        rounds(&mut a, &mut b, &mut now, 10);
        assert_eq!(b.got.len() as u64, count, "seed {seed}: a release does not thaw");
        a.s.thaw();
        b.s.thaw();
        run(seed, &mut a, &mut b, &mut now, |a, _| a.s.unreceived().count() == 0);
        assert_eq!(b.got, (1..=30).collect::<Vec<_>>(), "seed {seed}: the rest once, in order");
    }
}

/// A link end that loses every acknowledgement the session sends through it.
struct NoAcks<'a>(&'a mut Tap);

impl PeerLink for NoAcks<'_> {
    fn send(&mut self, msg: PeerMsg) {
        if !matches!(msg, PeerMsg::Ack { .. }) {
            self.0.send(msg);
        }
    }
    fn recv(&mut self) -> Option<PeerMsg> {
        self.0.recv()
    }
    fn is_up(&self) -> bool {
        self.0.is_up()
    }
    fn connection(&self) -> u64 {
        self.0.connection()
    }
}
