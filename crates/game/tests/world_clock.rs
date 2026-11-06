use game::WorldClock;

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
