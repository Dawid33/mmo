use game::{
    departure_offset, ghost_offsets, rebase_isometry, IsometryReal, RegionCoords,
    FLIP_HYSTERESIS, GHOST_MARGIN, REGION_SIZE,
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
    assert_eq!(departure_offset(-3.0, 259.0), Some((-1, 1)));
}

#[test]
fn ghost_offsets_cover_edges_and_corners() {
    assert!(ghost_offsets(128.0, 128.0).is_empty());
    assert_eq!(ghost_offsets(GHOST_MARGIN - 1.0, 128.0), vec![(-1, 0)]);
    assert_eq!(ghost_offsets(REGION_SIZE - GHOST_MARGIN + 1.0, 128.0), vec![(1, 0)]);
    assert_eq!(ghost_offsets(128.0, 10.0), vec![(0, -1)]);
    // Corner mirrors into 3 neighbours.
    let corner = ghost_offsets(10.0, 10.0);
    assert_eq!(corner.len(), 3);
    for o in [(-1, 0), (0, -1), (-1, -1)] {
        assert!(corner.contains(&o));
    }
}

#[test]
fn rebase_is_exact_for_boundary_walk() {
    // Walking off A(0,0) at x=258 lands at x=2 in B(1,0), bit-exact.
    use game::na::{Quaternion, Translation3, Unit};
    use game::parry::math::Real;
    let iso = IsometryReal::from_parts(
        Translation3::new(Real::from(258.0), Real::from(26.0), Real::from(100.0)),
        Unit::<Quaternion<Real>>::identity(),
    );
    let out = rebase_isometry(&iso, RegionCoords::new(0, 0), RegionCoords::new(1, 0));
    assert_eq!(out.translation.x, Real::from(2.0));
    assert_eq!(out.translation.z, Real::from(100.0));
    // Round-trip is identity.
    let back = rebase_isometry(&out, RegionCoords::new(1, 0), RegionCoords::new(0, 0));
    assert_eq!(back, iso);
}

#[test]
fn matches_prediction_is_identity_based_for_transfers() {
    use game::{Client, ColliderSpec, EntityBundle, EntityKind, GameEventKind, GhostData};
    use game::parry::math::Vector;
    let mk = |x: f32| EntityBundle {
        kind: EntityKind::Player,
        isometry: IsometryReal::translation(x.into(), 0.0.into(), 0.0.into()),
        linvel: Vector::zeros(),
        collider: ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        has_camera: true,
        client: Some((7, Client::default())),
        source_region: RegionCoords::new(0, 0),
        source_key: Default::default(),
    };
    // Same identity, different pose (predicted vs authoritative tick): matches.
    let a = GameEventKind::EntityArrived(mk(2.0));
    let b = GameEventKind::EntityArrived(mk(3.5));
    assert!(a.matches_prediction(&b));
    assert_ne!(a, b, "full equality still detects divergence");
    // Different identity: no match.
    let mut other = mk(2.0);
    other.source_region = RegionCoords::new(5, 5);
    assert!(!a.matches_prediction(&GameEventKind::EntityArrived(other)));
    // Non-transfer kinds: exact equality.
    assert!(GameEventKind::Tick.matches_prediction(&GameEventKind::Tick));
    let g = |x: f32| GameEventKind::GhostUpdate(GhostData {
        source_region: RegionCoords::new(0, 0),
        source_key: Default::default(),
        kind: EntityKind::Player,
        isometry: IsometryReal::translation(x.into(), 0.0.into(), 0.0.into()),
        linvel: Vector::zeros(),
        collider: ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
    });
    assert!(g(1.0).matches_prediction(&g(9.0)));
}
