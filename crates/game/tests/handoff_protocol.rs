use game::{
    departure_offset, halo_offsets, rebase_isometry, Pose, RegionCoords,
    FLIP_HYSTERESIS, REGION_SIZE,
};

#[test]
fn departure_needs_hysteresis() {
    // Inside, on the line, and within the hysteresis band: no flip.
    assert_eq!(departure_offset(128.0, 128.0), None);
    assert_eq!(departure_offset(-0.0, 10.0), None);
    assert_eq!(departure_offset(-FLIP_HYSTERESIS, 10.0), None);
    assert_eq!(departure_offset(REGION_SIZE + FLIP_HYSTERESIS, 10.0), None);
    // Past the band: flip toward the right neighbour.
    assert_eq!(departure_offset(-FLIP_HYSTERESIS - 0.1, 10.0), Some((-1, 0)));
    assert_eq!(departure_offset(REGION_SIZE + FLIP_HYSTERESIS + 0.1, 10.0), Some((1, 0)));
    assert_eq!(departure_offset(10.0, -3.0), Some((0, -1)));
    // Corner: diagonal neighbour.
    assert_eq!(
        departure_offset(-3.0, REGION_SIZE + FLIP_HYSTERESIS + 1.0),
        Some((-1, 1))
    );
}

#[test]
fn halo_offsets_cover_edges_and_corners() {
    use game::halo::HALO_UNITS;
    let mid = REGION_SIZE / 2.0;
    assert!(halo_offsets(mid, mid).is_empty());
    assert!(halo_offsets(HALO_UNITS, mid).is_empty(), "the band ends HALO_BLOCKS in");
    assert_eq!(halo_offsets(HALO_UNITS - 1.0, mid), vec![(-1, 0)]);
    assert_eq!(halo_offsets(REGION_SIZE - HALO_UNITS + 1.0, mid), vec![(1, 0)]);
    assert_eq!(halo_offsets(mid, 10.0), vec![(0, -1)]);
    // Corner mirrors into 3 neighbours.
    let corner = halo_offsets(10.0, 10.0);
    assert_eq!(corner.len(), 3);
    for o in [(-1, 0), (0, -1), (-1, -1)] {
        assert!(corner.contains(&o));
    }
}

#[test]
fn rebase_is_exact_for_boundary_walk() {
    // Walking off A(0,0) at x=REGION_SIZE+2 lands at x=2 in B(1,0), bit-exact.
    let iso = Pose::from_translation(game::rapier3d::math::Vector::new(REGION_SIZE + 2.0, 26.0, 100.0));
    let out = rebase_isometry(&iso, RegionCoords::new(0, 0), RegionCoords::new(1, 0));
    assert_eq!(out.translation.x, 2.0);
    assert_eq!(out.translation.z, 100.0);
    // Round-trip is identity.
    let back = rebase_isometry(&out, RegionCoords::new(1, 0), RegionCoords::new(0, 0));
    assert_eq!(back, iso);
}

#[test]
fn matches_prediction_is_identity_based_for_transfers() {
    use game::{Client, ColliderSpec, EntityBundle, EntityKind, GameEventKind, HaloEntityDelta, HaloEntityGone};
    use game::parry::math::Vector;
    let mk = |x: f32| EntityBundle {
        id: game::EntityId { birth_region: RegionCoords::new(0, 0), serial: 0 },
        kind: EntityKind::Player,
        body: game::BodyState::kinematic(Pose::translation(x, 0.0, 0.0)),
        collider: ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        carried: game::Carried { camera: Some((0.0, 0.0)), motion: None, ..Default::default() },
        client: Some((7, Client::default())),
        pending_inputs: Vec::new(),
    };
    // Same identity, different pose (predicted vs authoritative tick): matches.
    let a = GameEventKind::EntityArrived(mk(2.0));
    let b = GameEventKind::EntityArrived(mk(3.5));
    assert!(a.matches_prediction(&b));
    assert_ne!(a, b, "full equality still detects divergence");
    // Different identity: no match.
    let mut other = mk(2.0);
    other.id.birth_region = RegionCoords::new(5, 5);
    assert!(!a.matches_prediction(&GameEventKind::EntityArrived(other)));
    // Non-transfer kinds: exact equality.
    assert!(GameEventKind::Tick.matches_prediction(&GameEventKind::Tick));
    let g = |x: f32| GameEventKind::HaloEntity(HaloEntityDelta {
        id: game::EntityId { birth_region: RegionCoords::new(0, 0), serial: 0 },
        source_region: RegionCoords::new(0, 0),
        kind: EntityKind::Player,
        isometry: Pose::translation(x, 0.0, 0.0),
        linvel: Vector::ZERO,
        collider: ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
    });
    assert!(g(1.0).matches_prediction(&g(9.0)));
    // An end matches only the same end (id and sender), never an update.
    let id = game::EntityId { birth_region: RegionCoords::new(0, 0), serial: 0 };
    let gone = |from| GameEventKind::HaloEntityGone(HaloEntityGone { id, from });
    assert!(gone(RegionCoords::new(0, 0)).matches_prediction(&gone(RegionCoords::new(0, 0))));
    assert!(!gone(RegionCoords::new(0, 0)).matches_prediction(&gone(RegionCoords::new(1, 0))));
    assert!(!gone(RegionCoords::new(0, 0)).matches_prediction(&g(1.0)));
}
