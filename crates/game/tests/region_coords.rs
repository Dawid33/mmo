use game::{RegionCoords, REGION_SIZE};

#[test]
fn world_offset_is_exact_multiples_of_region_size() {
    // A power of two (1024 by default), so every offset is exact in f32.
    assert_eq!(REGION_SIZE, (game::REGION_BLOCKS * game::BLOCK_VOXELS) as f32);
    assert!(game::REGION_BLOCKS_CHOICES.contains(&game::REGION_BLOCKS));
    assert!((REGION_SIZE as u32).is_power_of_two());
    assert_eq!(RegionCoords::new(0, 0).world_offset(), [0.0, 0.0, 0.0]);
    assert_eq!(
        RegionCoords::new(1, -2).world_offset(),
        [REGION_SIZE, 0.0, -2.0 * REGION_SIZE]
    );
}

#[test]
fn from_world_floor_divides_including_negatives() {
    assert_eq!(RegionCoords::from_world(0.0, 0.0), RegionCoords::new(0, 0));
    assert_eq!(
        RegionCoords::from_world(REGION_SIZE - 0.1, REGION_SIZE - 0.1),
        RegionCoords::new(0, 0)
    );
    assert_eq!(
        RegionCoords::from_world(REGION_SIZE, 0.0),
        RegionCoords::new(1, 0)
    );
    // Negative side must floor, not truncate toward zero.
    assert_eq!(
        RegionCoords::from_world(-0.1, -REGION_SIZE - 0.1),
        RegionCoords::new(-1, -2)
    );
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
    let mut world = game::World::new();
    let ev = game::GameEvent::new(game::GameEventKind::Tick, 0, RegionCoords::new(99, 99));
    // Must not panic, must not error: unsubscribe races make this steady-state noise.
    assert!(world.reconcile_event(ev).is_ok());
}
