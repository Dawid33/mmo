//! `game::peer`: the host↔host session over a seeded, delaying in-process link. Every
//! test runs 32 seeds; a failure names its seed.

use game::{
    CutId, HostEndpoint, InlinePeerLink, PeerEvent, PeerHello, PeerSession, RegionCoords, Reliable, Volatile,
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

fn hello(ordinal: u32, boot: u64, generation: u64) -> PeerHello {
    PeerHello {
        ordinal,
        boot,
        generation,
        endpoint: HostEndpoint { ordinal, quic: None, webtransport: None, cert_hash: None },
        received_through: 0,
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

/// One host's side: its session, its end of the link, and what it has seen.
struct Side {
    s: PeerSession,
    link: InlinePeerLink,
    got: Vec<u64>,
    vol: Vec<Volatile>,
    events: Vec<PeerEvent>,
}

impl Side {
    fn new(me: PeerHello, peer: u32, link: InlinePeerLink) -> Self {
        Side { s: PeerSession::new(me, peer), link, got: Vec::new(), vol: Vec::new(), events: Vec::new() }
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
    (Side::new(hello(0, 100, 1), 1, la), Side::new(hello(1, 200, 1), 0, lb))
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
        let mut b2 = Side::new(hello(1, 201, 1), 0, std::mem::replace(&mut b.link, InlinePeerLink::pair(0, 0).0));
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
        let mut a = Side::new(hello(0, 100, 1), 1, la);
        let mut b = Side::new(hello(1, 200, 2), 0, lb);
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
        let mut b2 = Side::new(hello(1, 201, 1), 0, std::mem::replace(&mut b.link, InlinePeerLink::pair(0, 0).0));
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
