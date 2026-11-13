use game::{
    GameEvent, GameEventKind, Pose, Region, RegionCoords, Rollback, FLIP_HYSTERESIS,
    REGION_SIZE,
};
use game::StateHash;

fn crc(region: &Region) -> u32 {
    let mut h = crc32fast::Hasher::new();
    region.data().data.state_hash(&mut h);
    h.finalize()
}

fn pose(x: f32, z: f32) -> Pose {
    Pose::from_translation(game::rapier3d::math::Vector::new(x, 26.0, z))
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
    assert_eq!(bundle.id.birth_region, id);
    assert!(bundle.client.is_some(), "player carries its client input state");
    assert!(bundle.carried.camera.is_some());
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
    let (departures, halo) = region.take_transfers();
    assert!(departures.is_empty());
    assert_eq!(halo.len(), 1);
    let (msg, target) = &halo[0];
    assert_eq!(*target, RegionCoords::new(1, 0));
    let game::HaloEntityMsg::Update(data) = msg else { panic!("an update, got {msg:?}") };
    assert_eq!(data.source_region, id);
    assert!(region.data().player_entites.contains_key(&7), "still owned here");
}

#[test]
fn corner_mirrors_into_three_neighbours() {
    let mut region = region_with_player_at(RegionCoords::new(0, 0), 10.0, 10.0);
    let (_, halo) = region.take_transfers();
    let targets: Vec<RegionCoords> = halo.iter().map(|(_, t)| *t).collect();
    assert_eq!(targets.len(), 3);
    for t in [RegionCoords::new(-1, 0), RegionCoords::new(0, -1), RegionCoords::new(-1, -1)] {
        assert!(targets.contains(&t));
    }
}

#[test]
fn hysteresis_stops_boundary_thrash() {
    // Just past the line but inside the band: still owned, only mirrored.
    let mut region = region_with_player_at(RegionCoords::new(0, 0), REGION_SIZE + 1.0, 128.0);
    let (departures, halo) = region.take_transfers();
    assert!(departures.is_empty(), "inside the hysteresis band: no flip");
    assert!(!halo.is_empty());
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
    assert_eq!(t1, t2, "departure/halo buffers agree across runs");
}

#[test]
fn kindless_terrain_in_margin_does_not_mirror() {
    // Chunk entities get a fixed rigidbody but their `kind` component stays
    // `None` (create_entity_safe never sets it) — the kind-None guard in
    // scan_boundaries is what excludes them. A chunk's own min-corner can
    // never land strictly inside the band (it's a multiple of `CHUNK_UNITS`,
    // and the band boundary in halo_offsets is `> REGION_SIZE -
    // HALO_UNITS`, a multiple of it too), so spawning the chunk at its
    // natural position wouldn't exercise the guard at all — a chunk sitting
    // ON the boundary never mirrors regardless of `kind`. To actually put a
    // kindless bodied entity strictly inside the margin, teleport the
    // chunk's fixed body there directly via the same undo-safe primitive the
    // other tests use for player teleports.
    let id = RegionCoords::new(0, 0);
    let last = game::REGION_CHUNKS - 1;
    let mut region = Region::from_chunks(
        id,
        vec![(game::ChunkCoords::new(last, 0, last), game::Chunk::flat_floor(8))],
    );
    let chunk_key = region
        .data()
        .ecs
        .chunk
        .iter()
        .find_map(|(k, c)| c.is_some().then_some(k))
        .expect("terrain chunk registered as an entity with a chunk component");
    // Strictly inside the +x margin (half of it in), but short of the
    // departure line (REGION_SIZE + FLIP_HYSTERESIS), so only the halo path
    // is in play.
    let x = REGION_SIZE - game::halo::HALO_UNITS / 2.0;
    region.with_data(|d| d.set_body_pose_safe(chunk_key, pose(x, 128.0)));
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
    let (departures, halo) = region.take_transfers();
    assert!(departures.is_empty(), "terrain never departs (fixed bodies, and short of the flip line anyway)");
    assert!(
        halo.is_empty(),
        "kindless terrain never mirrors — this fails if the `kind`-None guard in scan_boundaries is removed"
    );
}

#[test]
fn mirror_past_boundary_is_not_scanned() {
    // A halo mirror sitting past a boundary must never depart or re-mirror
    // (`mirror_keys` in scan_boundaries; the extraction half is pinned by
    // `mirror_past_boundary_is_not_extracted` below).
    let id = RegionCoords::new(0, 0);
    let src = RegionCoords::new(1, 0);
    let mut region = Region::from_chunks(id, Vec::new());
    region
        .handle_event(GameEventKind::HaloEntity(game::HaloEntityDelta {
            id: game::EntityId::default(),
            source_region: src,
            kind: game::EntityKind::Player,
            isometry: pose(300.0, 128.0), // absurdly out of bounds
            linvel: game::parry::math::Vector::ZERO,
            collider: game::ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        }))
        .unwrap();
    region.forget_last_event();
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
    let (departures, halo) = region.take_transfers();
    assert!(departures.is_empty(), "mirrors are never extracted");
    assert!(halo.is_empty(), "mirrors are never re-mirrored");
}

#[test]
fn mirror_past_boundary_is_not_extracted() {
    // Halo mirrors have real rigidbodies, so the missing-rigidbody guard in
    // scan_boundaries does not exclude them — the `mirror_keys` set is the
    // only thing keeping a hosted mirror from being scanned as a
    // leaver. Park a bodied ghost STRICTLY PAST the +x boundary (region-local
    // x > REGION_SIZE + FLIP_HYSTERESIS = 258) and confirm the tick does not
    // extract it into a departure.
    //
    // BITE-PROOF: deleting/bypassing the `mirror_keys` filter in
    // scan_boundaries makes this fail — the bodied ghost past the line would
    // then be extracted into departures.
    let id = RegionCoords::new(0, 0);
    let src = RegionCoords::new(1, 0);
    let ghost_id = game::EntityId { birth_region: src, serial: 0 };
    let mut region = Region::from_chunks(id, Vec::new());
    region
        .handle_event(GameEventKind::HaloEntity(game::HaloEntityDelta {
            id: ghost_id,
            source_region: src,
            kind: game::EntityKind::Player,
            isometry: pose(300.0, 128.0), // strictly past the 258 flip line
            linvel: game::parry::math::Vector::ZERO,
            collider: game::ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        }))
        .unwrap();
    region.forget_last_event();
    // Sanity: stage-2 ghost really has a body (else the guard under test is
    // moot and the bite-proof would be vacuous).
    let ghost_e = region.data().halo_entities.get(&ghost_id).unwrap().entity;
    assert!(
        region.data().ecs.rigidbody.try_get(ghost_e).is_some(),
        "the mirror must carry a body for mirror_keys to be load-bearing"
    );
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
    let (departures, _ghosts) = region.take_transfers();
    // The region hosts nothing but this ghost, so any departure is the ghost
    // wrongly extracted — mirror_keys must keep the buffer empty.
    assert!(
        departures.is_empty(),
        "mirror_keys must exclude a hosted mirror from extraction, even past a boundary: {:?}",
        departures.iter().map(|(_, t)| *t).collect::<Vec<_>>()
    );
}

#[test]
fn reconcile_replaces_a_diverged_predicted_arrival_without_sticking() {
    // Client region B predicted an arrival with pose X; the authoritative
    // arrival has pose Y (server extracted on a different tick). Reconcile
    // must (a) end with the authoritative pose and (b) leave no unconfirmed
    // prediction stuck in the event log.
    let src = RegionCoords::new(0, 0);
    let id = RegionCoords::new(1, 0);
    let mk = |x: f32| game::EntityBundle {
        id: game::EntityId { birth_region: src, serial: 0 },
        kind: game::EntityKind::Player,
        body: game::BodyState::kinematic(pose(x, 128.0)),
        collider: game::ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        carried: game::Carried { camera: Some((0.0, 0.0)), motion: None, ..Default::default() },
        client: Some((9, game::Client::default())),
        pending_inputs: Vec::new(),
    };

    // Client-side region (local_client_id = Some → predictions reconcile).
    // reconcile() rolls back on divergence, which replays tier-2 undo closures
    // through the rollback log's `client` sender (`client.as_ref().unwrap()`);
    // a None sender panics. f32 client regions always have a live sender
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
    assert_eq!(t.x, 4.0, "authoritative pose won");
}

/// A halo entity update from region (1,0), at local `x`.
fn mirror(x: f32) -> GameEventKind {
    GameEventKind::HaloEntity(game::HaloEntityDelta {
        id: game::EntityId { birth_region: RegionCoords::new(1, 0), serial: 0 },
        source_region: RegionCoords::new(1, 0),
        kind: game::EntityKind::Player,
        isometry: pose(x, 128.0),
        linvel: game::parry::math::Vector::ZERO,
        collider: game::ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
    })
}

/// The server's region running `kinds` (its event stream) and a client
/// region that predicted `predicted`, both from the same empty start; the
/// stream is reconciled into the client one event at a time.
fn reconcile_stream(kinds: &[GameEventKind], predicted: &[GameEventKind]) -> (Region, Region) {
    let id = RegionCoords::new(0, 0);
    let mut server = Region::new(Rollback::new(None), None, id, None);
    let stream: Vec<GameEvent> = kinds.iter().map(|k| server.handle_event(k.clone()).unwrap()).collect();
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut client = Region::new(Rollback::new(None), Some(send), id, Some(9));
    for k in predicted {
        client.handle_event(k.clone()).unwrap();
    }
    for event in stream {
        client.reconcile(event).unwrap();
    }
    (server, client)
}

#[test]
fn reconcile_keeps_a_later_slots_predicted_mirror() {
    // The server mirrored in slots 1 and 2; the client (its source a tick
    // behind) only in slot 2. The server's slot-1 mirror is an insertion: it
    // must not consume the client's slot-2 one (they share an EntityId).
    let (server, client) = reconcile_stream(
        &[GameEventKind::Tick, mirror(250.0), GameEventKind::Tick, mirror(251.0)],
        &[GameEventKind::Tick, GameEventKind::Tick, mirror(251.0)],
    );
    assert!(client.pending_event_ids().is_empty(), "{:?}", client.pending_event_ids());
    assert_eq!(crc(&client), crc(&server));
}

#[test]
fn reconcile_drops_a_mirror_the_servers_slot_did_not_have() {
    // The client mirrored in slot 1, the server did not: its Tick closing
    // slot 1 drops the prediction rather than carrying it into slot 2.
    let (server, client) = reconcile_stream(
        &[GameEventKind::Tick, GameEventKind::Tick],
        &[GameEventKind::Tick, mirror(250.0), GameEventKind::Tick],
    );
    assert!(client.pending_event_ids().is_empty(), "{:?}", client.pending_event_ids());
    assert_eq!(crc(&client), crc(&server));
}

#[test]
fn reconcile_drops_a_local_input_where_the_body_is_not() {
    // The client predicted an input in a region its body is not in (here:
    // never was — as when the server's copy of the body left a tick earlier
    // than predicted). The server forwards such an input to the body, so it
    // never shows in this region's stream: the replay drops it.
    let input = GameEventKind::PlayerInput(9, game::InputEvent::Key { key: game::Key::KeyW, pressed: true });
    let (server, client) = reconcile_stream(
        &[GameEventKind::Tick, GameEventKind::Tick],
        &[GameEventKind::Tick, input, GameEventKind::Tick],
    );
    assert!(client.pending_event_ids().is_empty(), "{:?}", client.pending_event_ids());
    assert_eq!(crc(&client), crc(&server));
}

#[test]
fn reconcile_drops_local_inputs_stamped_before_the_authoritative_arrival() {
    // The client predicted its arrival here in slot 0 and an input after it
    // (stamped 0); the server's body crossed a slot later (arrival in slot
    // 1), so the server applied that input in the source region, and the
    // arrival bundle carries its effect. Integrating the arrival drops the
    // prediction: the body IS here on replay, so only the stamp tells.
    let src = RegionCoords::new(0, 0);
    let mut client_state = game::Client::default();
    let input = game::InputEvent::Key { key: game::Key::KeyW, pressed: true };
    let bundle = |client: game::Client| game::EntityBundle {
        id: game::EntityId { birth_region: src, serial: 0 },
        kind: game::EntityKind::Player,
        body: game::BodyState::kinematic(pose(2.0, 128.0)),
        collider: game::ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        carried: game::Carried { camera: Some((0.0, 0.0)), motion: None, ..Default::default() },
        client: Some((9, client)),
        pending_inputs: Vec::new(),
    };
    let predicted = bundle(game::Client::default());
    let _ = client_state.input.update(input.clone());
    let authoritative = bundle(client_state);
    let (server, client) = reconcile_stream(
        &[GameEventKind::Tick, GameEventKind::EntityArrived(authoritative), GameEventKind::Tick],
        &[
            GameEventKind::EntityArrived(predicted),
            GameEventKind::PlayerInput(9, input),
            GameEventKind::Tick,
            GameEventKind::Tick,
        ],
    );
    assert!(client.pending_event_ids().is_empty(), "{:?}", client.pending_event_ids());
    assert_eq!(crc(&client), crc(&server));
}

/// The `HaloEntityGone`s in a tick's halo messages, by target.
fn gones(halo: &[(game::HaloEntityMsg, RegionCoords)]) -> Vec<RegionCoords> {
    halo.iter().filter(|(m, _)| matches!(m, game::HaloEntityMsg::Gone(_))).map(|(_, t)| *t).collect()
}

/// An entity that leaves a neighbour's band ends its mirror there
/// explicitly, once; the tick that does it (and the owner's record of what
/// it mirrored) rolls back bit-exact.
#[test]
fn leaving_the_band_sends_halo_entity_gone() {
    let id = RegionCoords::new(0, 0);
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(None), Some(send), id, None);
    region.handle_event(GameEventKind::CreateClient(7)).unwrap();
    region.forget_last_event();
    let key = *region.data().player_entites.get(&7).unwrap();
    region.with_data(|d| d.set_body_pose_safe(key, pose(REGION_SIZE - 10.0, REGION_SIZE / 2.0)));
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
    let (_, halo) = region.take_transfers();
    assert_eq!(halo.len(), 1, "mirrored east: {halo:?}");
    assert!(gones(&halo).is_empty());
    // Back to the middle: one explicit end, to east.
    region.with_data(|d| d.set_body_pose_safe(key, pose(REGION_SIZE / 2.0, REGION_SIZE / 2.0)));
    let before = crc(&region);
    region.handle_event(GameEventKind::Tick).unwrap();
    let (_, halo) = region.take_transfers();
    assert_eq!(gones(&halo), vec![RegionCoords::new(1, 0)]);
    let entity = region.data().entity_id(key).unwrap();
    assert!(
        matches!(&halo[0].0, game::HaloEntityMsg::Gone(g) if g.id == entity && g.from == id),
        "{halo:?}"
    );
    region.rollback_last_event();
    assert_eq!(before, crc(&region), "the ending tick undoes bit-exact");
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
    region.take_transfers();
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
    let (_, halo) = region.take_transfers();
    assert!(halo.is_empty(), "ended once: {halo:?}");
}

/// A body crossing from a corner ends its mirrors everywhere but in the
/// region it crosses into (the arrival upgrades that one in place).
#[test]
fn crossing_ends_mirrors_except_in_its_target() {
    let id = RegionCoords::new(0, 0);
    let mut region = region_with_player_at(id, 10.0, 10.0);
    assert_eq!(region.take_transfers().1.len(), 3, "a corner mirrors into three neighbours");
    let key = *region.data().player_entites.get(&7).unwrap();
    region.with_data(|d| d.set_body_pose_safe(key, pose(-FLIP_HYSTERESIS - 1.0, REGION_SIZE / 2.0)));
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
    let (departures, halo) = region.take_transfers();
    assert_eq!(departures.len(), 1);
    assert_eq!(departures[0].1, RegionCoords::new(-1, 0));
    let mut ended = gones(&halo);
    ended.sort();
    assert_eq!(ended, vec![RegionCoords::new(-1, -1), RegionCoords::new(0, -1)], "no end in the crossing target");
    assert_eq!(halo.len(), 2, "nothing else: {halo:?}");
}

/// An entity removed between scans (a parked player) ends its mirror at the
/// next scan.
#[test]
fn removed_entity_ends_its_mirror() {
    let id = RegionCoords::new(0, 0);
    let mut region = region_with_player_at(id, REGION_SIZE - 10.0, REGION_SIZE / 2.0);
    assert_eq!(region.take_transfers().1.len(), 1);
    region.handle_event(GameEventKind::ParkPlayer(7)).unwrap();
    region.forget_last_event();
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
    let (_, halo) = region.take_transfers();
    assert_eq!(gones(&halo), vec![RegionCoords::new(1, 0)], "{halo:?}");
}
