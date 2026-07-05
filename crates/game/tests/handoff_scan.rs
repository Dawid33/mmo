use game::{
    GameEvent, GameEventKind, IsometryReal, Region, RegionCoords, Rollback, FLIP_HYSTERESIS,
    REGION_SIZE,
};
use game::na::{Quaternion, Translation3, Unit};
use game::parry::math::Real;
use std::hash::Hash;

fn crc(region: &Region) -> u32 {
    let mut h = crc32fast::Hasher::new();
    region.data().data.hash(&mut h);
    h.finalize()
}

fn pose(x: f32, z: f32) -> IsometryReal {
    IsometryReal::from_parts(
        Translation3::new(Real::from(x), Real::from(26.0), Real::from(z)),
        Unit::<Quaternion<Real>>::identity(),
    )
}

/// A region with one player, teleported to (x, z), ticked once.
fn region_with_player_at(id: RegionCoords, x: f32, z: f32) -> Region {
    let mut region = Region::from_chunks(id, Vec::new());
    region.handle_event(GameEventKind::CreateClient(7)).unwrap();
    region.forget_last_event();
    let key = *region.data().player_entites.get(&7).unwrap();
    // Test-only teleport through the public undo-safe primitive, wrapped in
    // its own forgotten transaction so the tick under test stays clean.
    region.with_data(|d| d.set_body_pose_safe(key, pose(x, z)));
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
    region
}

#[test]
fn tick_extracts_a_leaver_into_a_departure() {
    let id = RegionCoords::new(0, 0);
    let mut region = region_with_player_at(id, REGION_SIZE + FLIP_HYSTERESIS + 1.0, 128.0);
    let (departures, _ghosts) = region.take_transfers();
    assert_eq!(departures.len(), 1);
    let (bundle, target) = &departures[0];
    assert_eq!(*target, RegionCoords::new(1, 0));
    assert_eq!(bundle.source_region, id);
    assert!(bundle.client.is_some(), "player carries its client input state");
    assert!(bundle.has_camera);
    assert!(
        !region.data().player_entites.contains_key(&7),
        "extracted player is gone from the source"
    );
    assert!(!region.data().clients.contains_key(&7));
}

#[test]
fn tick_mirrors_margin_entities_without_extracting() {
    let id = RegionCoords::new(0, 0);
    let mut region = region_with_player_at(id, REGION_SIZE - 10.0, 128.0);
    let (departures, ghosts) = region.take_transfers();
    assert!(departures.is_empty());
    assert_eq!(ghosts.len(), 1);
    let (data, target) = &ghosts[0];
    assert_eq!(*target, RegionCoords::new(1, 0));
    assert_eq!(data.source_region, id);
    assert!(region.data().player_entites.contains_key(&7), "still owned here");
}

#[test]
fn corner_mirrors_into_three_neighbours() {
    let mut region = region_with_player_at(RegionCoords::new(0, 0), 10.0, 10.0);
    let (_, ghosts) = region.take_transfers();
    let targets: Vec<RegionCoords> = ghosts.iter().map(|(_, t)| *t).collect();
    assert_eq!(targets.len(), 3);
    for t in [RegionCoords::new(-1, 0), RegionCoords::new(0, -1), RegionCoords::new(-1, -1)] {
        assert!(targets.contains(&t));
    }
}

#[test]
fn hysteresis_stops_boundary_thrash() {
    // Just past the line but inside the band: still owned, only mirrored.
    let mut region = region_with_player_at(RegionCoords::new(0, 0), REGION_SIZE + 1.0, 128.0);
    let (departures, ghosts) = region.take_transfers();
    assert!(departures.is_empty(), "inside the hysteresis band: no flip");
    assert!(!ghosts.is_empty());
}

#[test]
fn extracting_tick_holds_the_hash_bar() {
    // rollback_last_event replays tier-2 undo closures through
    // client.as_ref().unwrap(); a None render sender panics. Region::from_chunks
    // wires None (server regions forget, never roll back), so build the region
    // with a live throwaway sender instead — mirrors the sibling rollback tests
    // (handoff_state/random_ops/...). _recv stays bound so the channel is live.
    let id = RegionCoords::new(0, 0);
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(None), Some(send), id, None);
    region.handle_event(GameEventKind::CreateClient(7)).unwrap();
    region.forget_last_event();
    let key = *region.data().player_entites.get(&7).unwrap();
    region.with_data(|d| d.set_body_pose_safe(key, pose(REGION_SIZE + 5.0, 128.0)));
    let before = crc(&region);
    // The extracting tick, NOT forgotten: roll it back.
    region.handle_event(GameEventKind::Tick).unwrap();
    assert!(!region.data().player_entites.contains_key(&7));
    region.rollback_last_event();
    assert_eq!(before, crc(&region), "hash(before) == crc(after undo) across extraction");
}

#[test]
fn identical_streams_produce_identical_scans_and_hashes() {
    // The property client prediction depends on (spec Testing #3): two
    // regions fed the same events agree bit-exactly on state AND transfers.
    let run = || {
        let id = RegionCoords::new(0, 0);
        let mut region = region_with_player_at(id, REGION_SIZE - 10.0, 10.0);
        let transfers = region.take_transfers();
        (crc(&region), format!("{:?}", transfers))
    };
    let (h1, t1) = run();
    let (h2, t2) = run();
    assert_eq!(h1, h2, "state hashes agree across runs");
    assert_eq!(t1, t2, "departure/ghost buffers agree across runs");
}

#[test]
fn kindless_terrain_in_margin_does_not_mirror() {
    // Chunk entities get a fixed rigidbody but their `kind` component stays
    // `None` (create_entity_safe never sets it) — the kind-None guard in
    // scan_boundaries is what excludes them. A chunk's own min-corner can
    // never land strictly inside the margin (it's a multiple of 32, and the
    // margin boundary in ghost_offsets is `> REGION_SIZE - GHOST_MARGIN` =
    // `> 224`, itself a multiple of 32), so spawning the chunk at its
    // natural position wouldn't exercise the guard at all — a chunk sitting
    // ON the boundary never mirrors regardless of `kind`. To actually put a
    // kindless bodied entity strictly inside the margin, teleport the
    // chunk's fixed body there directly via the same undo-safe primitive the
    // other tests use for player teleports.
    let id = RegionCoords::new(0, 0);
    let mut region = Region::from_chunks(
        id,
        vec![(game::ChunkCoords::new(7, 0, 7), game::Chunk::flat_floor(8))],
    );
    let chunk_key = region
        .data()
        .ecs
        .chunk
        .iter()
        .find_map(|(k, c)| c.is_some().then_some(k))
        .expect("terrain chunk registered as an entity with a chunk component");
    // 224 < 250 < 256: strictly inside the +x margin, but not past the
    // departure line (258), so only the ghost path is in play.
    region.with_data(|d| d.set_body_pose_safe(chunk_key, pose(250.0, 128.0)));
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
    let (departures, ghosts) = region.take_transfers();
    assert!(departures.is_empty(), "terrain never departs (fixed bodies, and 250 < the 258 flip line anyway)");
    assert!(
        ghosts.is_empty(),
        "kindless terrain never mirrors — this fails if the `kind`-None guard in scan_boundaries is removed"
    );
}

#[test]
fn bodyless_ghost_is_not_scanned() {
    // A ghost sitting past a boundary must never depart or re-mirror. Today
    // this holds because stage-1 ghosts (apply_ghost) never get a rigidbody,
    // so scan_boundaries' `let Some(handle) = ... else { continue }` skips
    // them before the `ghost_keys` set is even consulted.
    //
    // NOTE: this test does NOT exercise `ghost_keys` in scan_boundaries —
    // deleting that set entirely still leaves this test green, because the
    // missing-rigidbody guard already excludes bodyless ghosts. `ghost_keys`
    // only becomes load-bearing (and gets its own bite-test) once Task 9
    // gives hosted ghosts rigidbodies of their own.
    let id = RegionCoords::new(0, 0);
    let src = RegionCoords::new(1, 0);
    let mut region = Region::from_chunks(id, Vec::new());
    region
        .handle_event(GameEventKind::GhostUpdate(game::GhostData {
            source_region: src,
            source_key: Default::default(),
            kind: game::EntityKind::Player,
            isometry: pose(300.0, 128.0), // absurdly out of bounds
            linvel: game::parry::math::Vector::zeros(),
            collider: game::ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        }))
        .unwrap();
    region.forget_last_event();
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
    let (departures, ghosts) = region.take_transfers();
    assert!(departures.is_empty(), "bodyless ghosts are never extracted");
    assert!(ghosts.is_empty(), "bodyless ghosts are never re-mirrored");
}

#[test]
fn reconcile_replaces_a_diverged_predicted_arrival_without_sticking() {
    // Client region B predicted an arrival with pose X; the authoritative
    // arrival has pose Y (server extracted on a different tick). Reconcile
    // must (a) end with the authoritative pose and (b) leave no unconfirmed
    // prediction stuck in the event log.
    let (_, src_key) = {
        let mut rb = Rollback::new(None);
        rb.new_transaction();
        rb.create_player_safe(9);
        rb.forget();
        let key = *rb.data.player_entites.get(&9).unwrap();
        (rb, key)
    };
    let src = RegionCoords::new(0, 0);
    let id = RegionCoords::new(1, 0);
    let mk = |x: f32| game::EntityBundle {
        kind: game::EntityKind::Player,
        isometry: pose(x, 128.0),
        linvel: game::parry::math::Vector::zeros(),
        collider: game::ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        has_camera: true,
        client: Some((9, game::Client::default())),
        source_region: src,
        source_key: src_key,
    };

    // Client-side region (local_client_id = Some → predictions reconcile).
    // reconcile() rolls back on divergence, which replays tier-2 undo closures
    // through the rollback log's `client` sender (`client.as_ref().unwrap()`);
    // a None sender panics. Real client regions always have a live sender
    // (built via new_region with Some(send)), so give this one a throwaway
    // sender too — _recv stays bound to keep the channel connected.
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(None), Some(send), id, Some(9));
    // Predict the arrival.
    region.handle_event(GameEventKind::EntityArrived(mk(2.0))).unwrap();
    // Authoritative copy arrives at the same event id with a different pose.
    region
        .reconcile(GameEvent::new(GameEventKind::EntityArrived(mk(4.0)), 0, id))
        .unwrap();

    assert!(
        region.pending_event_ids().is_empty(),
        "identity-matched prediction must be consumed, not stuck: {:?}",
        region.pending_event_ids()
    );
    let e = *region.data().player_entites.get(&9).unwrap();
    let handle = region.data().ecs.rigidbody.try_get(e).unwrap();
    let t = region.data().physics.bodies.get(handle).unwrap().translation();
    assert_eq!(t.x, Real::from(4.0), "authoritative pose won");
}
