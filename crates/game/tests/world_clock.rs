use game::{ClockAnchor, WorldClock, TICK_RATE};

#[test]
fn tick_at_counts_tick_rate_periods() {
    let c = WorldClock::starting_at(1000, 40);
    assert_eq!(c.tick_at(1000), 40);
    assert_eq!(c.tick_at(1049), 40);
    assert_eq!(c.tick_at(1050), 41);
    assert_eq!(c.ms_of(41), 1050);
    // Before the anchor it saturates.
    assert_eq!(c.tick_at(10), 40);
    assert_eq!(c.ms_of(3), 1000);
}

#[test]
fn ticks_due_is_a_bounded_burst_that_never_skips() {
    use game::{ticks_due, MAX_CATCH_UP_BURST as B};
    assert_eq!(ticks_due(10, 10), 0, "caught up");
    assert_eq!(ticks_due(11, 10), 0, "ahead never ticks");
    assert_eq!(ticks_due(10, 12), 2);
    assert_eq!(ticks_due(10, 10 + u64::from(B)), B);
    // Far behind: bursts of B, one tick number at a time, until caught up.
    let (mut cur, target) = (0u64, 23u64);
    let mut bursts = Vec::new();
    while cur < target {
        let n = ticks_due(cur, target);
        assert!(n >= 1 && n <= B);
        cur += u64::from(n);
        bursts.push(n);
    }
    assert_eq!(bursts, [B, B, B, B, 3]);
    assert_eq!(cur, target, "no tick number skipped or overshot");
}

#[test]
fn anchored_clocks_agree() {
    let anchor = ClockAnchor { unix_ms: 1_700_000_000_000, tick: 9_000 };
    let first_start = anchor.unix_ms + 5_000_017;
    let second_start = first_start + 1_234;
    let (a, a_phase) = WorldClock::from_anchor(anchor, first_start);
    let (b, b_phase) = WorldClock::from_anchor(anchor, second_start);
    assert!(a_phase < TICK_RATE && b_phase < TICK_RATE);
    for n in 0..100u64 {
        let wall = second_start + n * 37 + n % 7;
        let ta = a.tick_at(wall - first_start + a_phase);
        let tb = b.tick_at(wall - second_start + b_phase);
        assert_eq!(ta, tb, "wall {wall}");
        assert_eq!(ta, anchor.tick_at_unix(wall));
    }
}

#[test]
fn anchor_before_now_never_goes_back() {
    let anchor = ClockAnchor { unix_ms: 10_000, tick: 77 };
    assert_eq!(anchor.tick_at_unix(0), 77);
    assert_eq!(anchor.tick_at_unix(10_000 + TICK_RATE - 1), 77);
    assert_eq!(anchor.tick_at_unix(10_000 + TICK_RATE), 78);
    let (clock, phase) = WorldClock::from_anchor(anchor, 5_000);
    assert_eq!((clock.tick_at(0), phase), (77, 0));
    let (clock, phase) = WorldClock::from_anchor(anchor, 10_000 + 3 * TICK_RATE + 11);
    assert_eq!((clock.tick_at(0), phase), (80, 11));
}
