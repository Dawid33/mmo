use game::{RegionCoords, REGION_SIZE};

#[test]
fn world_offset_is_exact_multiples_of_region_size() {
    assert_eq!(REGION_SIZE, 256.0);
    assert_eq!(RegionCoords::new(0, 0).world_offset(), [0.0, 0.0, 0.0]);
    assert_eq!(RegionCoords::new(1, -2).world_offset(), [256.0, 0.0, -512.0]);
}

#[test]
fn from_world_floor_divides_including_negatives() {
    assert_eq!(RegionCoords::from_world(0.0, 0.0), RegionCoords::new(0, 0));
    assert_eq!(RegionCoords::from_world(255.9, 255.9), RegionCoords::new(0, 0));
    assert_eq!(RegionCoords::from_world(256.0, 0.0), RegionCoords::new(1, 0));
    // Negative side must floor, not truncate toward zero.
    assert_eq!(RegionCoords::from_world(-0.1, -256.1), RegionCoords::new(-1, -2));
}

#[test]
fn window_3x3_is_the_nine_neighbours() {
    let w = RegionCoords::new(2, -1).window_3x3();
    assert_eq!(w.len(), 9);
    for dx in -1..=1 {
        for dz in -1..=1 {
            assert!(w.contains(&RegionCoords::new(2 + dx, -1 + dz)));
        }
    }
}

#[test]
fn reconcile_event_tolerates_unknown_region() {
    let mut world = game::World::basic();
    let ev = game::GameEvent::new(game::GameEventKind::Tick, 0, RegionCoords::new(99, 99));
    // Must not panic, must not error: unsubscribe races make this steady-state noise.
    assert!(world.reconcile_event(ev).is_ok());
}
