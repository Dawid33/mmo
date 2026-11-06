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
