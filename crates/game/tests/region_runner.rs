use crossbeam::channel::unbounded;
use game::{
    Block, BlockId, Chunk, ChunkCoords, Column, ColumnCoords, GameEventKind, Region, RegionCoords,
    RegionInput, RegionOutput, RegionRunner, RegionSeed, SerializedRegion, SubChunk,
};
use game::StateHash;

fn flat_chunks() -> Vec<(ChunkCoords, Chunk)> {
    (0..2)
        .flat_map(|x| (0..2).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(8))))
        .collect()
}

fn flat_columns() -> Vec<(ColumnCoords, Column)> {
    let below = Block::new(BlockId::AIR);
    let above = Block::new(BlockId::AIR);
    (0..2)
        .flat_map(move |x| {
            (0..2).map(move |z| {
                let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(8)));
                (ColumnCoords::new(x, z), Column::new(0, below, above, vec![sub]))
            })
        })
        .collect()
}

fn crc(region: &Region) -> u32 {
    let mut h = crc32fast::Hasher::new();
    region.data().data.state_hash(&mut h);
    h.finalize()
}

fn runner(id: RegionCoords) -> (RegionRunner, crossbeam::channel::Receiver<(RegionCoords, RegionOutput)>) {
    let (out_send, out_recv) = unbounded();
    let region = Region::from_chunks(id, flat_chunks());
    (RegionRunner::new(id, region, out_send), out_recv)
}

#[test]
fn tick_emits_event_processed() {
    let id = RegionCoords::new(0, 0);
    let (mut r, out) = runner(id);
    r.tick();
    let (rc, output) = out.try_recv().expect("tick output");
    assert_eq!(rc, id);
    let RegionOutput::EventProcessed(ev) = output else { panic!("expected EventProcessed") };
    assert_eq!(ev.kind, GameEventKind::Tick);
    assert_eq!(ev.region_id, id);
}

#[test]
fn sync_clock_every_ten_ticks() {
    let (mut r, out) = runner(RegionCoords::new(0, 0));
    for _ in 0..10 {
        r.tick();
    }
    let clocks = out
        .try_iter()
        .filter(|(_, o)| matches!(o, RegionOutput::SyncClock { .. }))
        .count();
    assert_eq!(clocks, 1, "exactly one SyncClock in the first 10 ticks");
}

#[test]
fn create_client_event_is_processed_and_snapshot_includes_player() {
    let id = RegionCoords::new(0, 0);
    let (mut r, out) = runner(id);
    assert!(r.handle_input(RegionInput::Event(GameEventKind::CreateClient(7))));
    assert!(r.handle_input(RegionInput::RequestSnapshot(7)));
    let outputs: Vec<_> = out.try_iter().map(|(_, o)| o).collect();
    assert!(matches!(&outputs[0], RegionOutput::EventProcessed(ev) if ev.kind == GameEventKind::CreateClient(7)));
    let RegionOutput::Snapshot(client, rollback) = &outputs[1] else { panic!("expected Snapshot") };
    assert_eq!(*client, 7);
    assert!(rollback.player_entites.contains_key(&7), "FIFO: snapshot after CreateClient includes the player");
}

#[test]
fn shutdown_stops_and_park_restore_is_hash_exact() {
    let id = RegionCoords::new(1, 0);
    let (mut r, out) = runner(id);
    // Mutate state so the roundtrip is non-trivial.
    r.handle_input(RegionInput::Event(GameEventKind::CreateClient(3)));
    for _ in 0..5 {
        r.tick();
    }
    let before = {
        // Snapshot for hashing: same clone path the wire uses.
        r.handle_input(RegionInput::RequestSnapshot(0));
        let RegionOutput::Snapshot(_, rb) = out.try_iter().map(|(_, o)| o).last().unwrap() else {
            panic!()
        };
        let mut h = crc32fast::Hasher::new();
        rb.data.state_hash(&mut h);
        h.finalize()
    };

    assert!(!r.handle_input(RegionInput::Shutdown), "Shutdown must stop the runner");
    let RegionOutput::Stopped(serialized, _far) = out.try_iter().map(|(_, o)| o).last().unwrap()
    else {
        panic!("expected Stopped");
    };

    // Restore: the exact cycle-in path the manager uses.
    let restored = RegionSeed::Parked(serialized, flat_columns()).into_region(id);
    assert_eq!(before, crc(&restored), "hash(before park) == hash(after restore), bit-exact");
}

#[test]
fn corrupt_parked_blob_falls_back_to_generation() {
    let id = RegionCoords::new(2, 2);
    let garbage = SerializedRegion(vec![0xde, 0xad, 0xbe, 0xef]);
    let region = RegionSeed::Parked(garbage, flat_columns()).into_region(id);
    assert_eq!(region.data().ecs.entities.len(), 4, "fell back to the 2x2 fallback chunks");
}

#[test]
fn request_terrain_snapshot_replies_with_terrain() {
    let id = RegionCoords::new(0, 0);
    let (mut r, out) = runner(id);
    r.handle_input(RegionInput::RequestTerrain(7));
    let (rc, output) = out.try_recv().expect("far snapshot output");
    assert_eq!(rc, id);
    let RegionOutput::Terrain(client, snap) = output else {
        panic!("expected TerrainSnapshot")
    };
    assert_eq!(client, 7);
    assert_eq!(snap.chunks.len(), 4);
}

#[test]
fn stable_terrain_never_pushes_far_change() {
    let (mut r, out) = runner(RegionCoords::new(0, 0));
    for _ in 0..(game::TERRAIN_HASH_INTERVAL_TICKS * 2 + 1) {
        r.tick();
    }
    let pushes = out
        .try_iter()
        .filter(|(_, o)| matches!(o, RegionOutput::TerrainChanged(_)))
        .count();
    assert_eq!(pushes, 0, "unchanged terrain must not push a far refresh");
}

#[test]
fn terrain_edit_pushes_one_throttled_far_change() {
    use game::{Chunk, ChunkCoords};
    let (mut r, out) = runner(RegionCoords::new(0, 0));
    // Mutate terrain: add a new chunk (a flat floor) at y=1 via the test hook.
    r.region_mut().with_data(|d| {
        d.create_mesh(ChunkCoords::new(0, 1, 0), Chunk::flat_floor(8));
    });
    for _ in 0..(game::TERRAIN_HASH_INTERVAL_TICKS + 1) {
        r.tick();
    }
    let pushes = out
        .try_iter()
        .filter(|(_, o)| matches!(o, RegionOutput::TerrainChanged(_)))
        .count();
    assert_eq!(pushes, 1, "one change → exactly one throttled push");
}

// --- Inputs applied at the tick the client predicted them after -------------
//
// The client predicts "my input lands after my tick k". Applied on arrival
// instead, it lands wherever the server's clock happens to be: on the other
// side of a tick boundary the client rolls back, and a key edge a tick early
// or late is a whole tick of movement gained or lost — the rubber band.

fn key_input(client: usize) -> GameEventKind {
    GameEventKind::PlayerInput(
        client,
        game::InputEvent::Key { key: game::Key::KeyW, pressed: true },
    )
}

fn processed(out: &crossbeam::channel::Receiver<(RegionCoords, RegionOutput)>) -> Vec<game::GameEvent> {
    out.try_iter()
        .filter_map(|(_, o)| match o {
            RegionOutput::EventProcessed(ev) => Some(ev),
            _ => None,
        })
        .collect()
}

fn runner_with_player() -> (RegionRunner, crossbeam::channel::Receiver<(RegionCoords, RegionOutput)>) {
    let (mut r, out) = runner(RegionCoords::new(0, 0));
    r.handle_input(RegionInput::Event(GameEventKind::CreateClient(7)));
    r.tick();
    let _ = processed(&out);
    (r, out)
}

#[test]
fn input_predicted_after_a_future_tick_waits_for_it() {
    let (mut r, out) = runner_with_player();
    let now = r.current_tick();
    r.handle_input(RegionInput::EventAfterTick(key_input(7), now as u64 + 2));
    assert!(processed(&out).is_empty(), "two ticks early: nothing applied yet");

    r.tick();
    let kinds: Vec<_> = processed(&out).into_iter().map(|e| e.kind).collect();
    assert_eq!(kinds, vec![GameEventKind::Tick], "one tick early: still held");

    r.tick();
    let events = processed(&out);
    let kinds: Vec<_> = events.iter().map(|e| e.kind.clone()).collect();
    assert_eq!(kinds, vec![GameEventKind::Tick, key_input(7)], "applied right after its tick");
    assert_eq!(events[1].id, events[0].id + 1, "and takes the very next id, as the client predicted");
}

#[test]
fn input_for_the_current_or_a_past_tick_applies_on_arrival() {
    let (mut r, out) = runner_with_player();
    let now = r.current_tick() as u64;
    r.handle_input(RegionInput::EventAfterTick(key_input(7), now));
    r.handle_input(RegionInput::EventAfterTick(key_input(7), now.saturating_sub(1)));
    assert_eq!(processed(&out).len(), 2, "a client level with or behind the server is not delayed");
}

#[test]
fn held_inputs_keep_their_arrival_order() {
    let (mut r, out) = runner_with_player();
    let now = r.current_tick() as u64;
    let look = |dx| GameEventKind::PlayerInput(7, game::InputEvent::MouseMotion { dx, dy: 0.0 });
    r.handle_input(RegionInput::EventAfterTick(look(1.0), now + 1));
    r.handle_input(RegionInput::EventAfterTick(look(2.0), now + 1));
    r.tick();
    let kinds: Vec<_> = processed(&out).into_iter().map(|e| e.kind).collect();
    assert_eq!(kinds, vec![GameEventKind::Tick, look(1.0), look(2.0)]);
}

#[test]
fn input_stamped_too_far_ahead_is_not_parked() {
    let (mut r, out) = runner_with_player();
    let far = r.current_tick() as u64 + game::MAX_INPUT_LEAD_TICKS + 1;
    r.handle_input(RegionInput::EventAfterTick(key_input(7), far));
    assert_eq!(processed(&out).len(), 1, "a runaway stamp must not hold input hostage");
}

#[test]
fn shutdown_applies_what_is_still_held() {
    let (mut r, out) = runner_with_player();
    let now = r.current_tick() as u64;
    r.handle_input(RegionInput::EventAfterTick(key_input(7), now + 5));
    assert!(!r.handle_input(RegionInput::Shutdown));
    assert_eq!(processed(&out).len(), 1, "parking a region must not lose input");
}

/// A client two ticks ahead of the server predicts an input, and the server
/// receives it at once. Returns the rollbacks the client needed once the
/// server's stream caught up.
fn rollbacks_of_a_leading_client(stamped: bool) -> u32 {
    let id = RegionCoords::new(0, 0);
    let (mut server, out) = runner_with_player();
    server.handle_input(RegionInput::RequestSnapshot(7));
    let snapshot = out
        .try_iter()
        .find_map(|(_, o)| match o {
            RegionOutput::Snapshot(_, rollback) => Some(rollback),
            _ => None,
        })
        .expect("snapshot");
    let (send, _updates) = unbounded();
    let mut client = Region::new(snapshot, Some(send), id, Some(7));

    client.handle_event(GameEventKind::Tick).unwrap();
    client.handle_event(GameEventKind::Tick).unwrap();
    let after_tick = client.current_tick() as u64;
    client.handle_event(key_input(7)).unwrap();
    client.handle_event(GameEventKind::Tick).unwrap();

    server.handle_input(if stamped {
        RegionInput::EventAfterTick(key_input(7), after_tick)
    } else {
        RegionInput::Event(key_input(7))
    });
    for _ in 0..3 {
        server.tick();
    }
    let rollbacks = processed(&out)
        .into_iter()
        .map(|ev| client.reconcile(ev).unwrap().mispredictions)
        .sum();
    assert_eq!(crc(&client), crc(server.region_mut()), "client and server must agree either way");
    rollbacks
}

#[test]
fn leading_client_reconciles_a_stamped_input_without_rollback() {
    assert_eq!(rollbacks_of_a_leading_client(true), 0);
    assert!(
        rollbacks_of_a_leading_client(false) > 0,
        "applied on arrival, the same input lands ticks early and the client rolls back"
    );
}

#[test]
fn a_server_runner_keeps_no_event_history() {
    let (out, _rx) = crossbeam::channel::unbounded();
    let rc = game::RegionCoords::new(0, 0);
    let mut runner = game::RegionRunner::new(rc, game::Region::from_chunks(rc, Vec::new()), out);
    runner.handle_input(game::RegionInput::Event(game::GameEventKind::CreateClient(0)));
    for _ in 0..50 {
        runner.tick();
    }
    assert!(
        runner.region_mut().pending_event_ids().is_empty(),
        "server regions must not retain applied events"
    );
}
