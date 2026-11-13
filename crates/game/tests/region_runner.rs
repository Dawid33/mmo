use crossbeam::channel::unbounded;
use game::{GameEventKind, Region, RegionCoords, RegionInput, RegionOutput, RegionRunner, RegionSeed};
use game::StateHash;

fn crc(region: &Region) -> u32 {
    let mut h = crc32fast::Hasher::new();
    region.data().data.state_hash(&mut h);
    h.finalize()
}

fn runner(id: RegionCoords) -> (RegionRunner, crossbeam::channel::Receiver<(RegionCoords, RegionOutput)>) {
    let (out_send, out_recv) = unbounded();
    let region = crate::common::region_of(id, crate::common::uniform_seed());
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
    let RegionOutput::Stopped(serialized, _far, _, _) = out.try_iter().map(|(_, o)| o).last().unwrap()
    else {
        panic!("expected Stopped");
    };

    // Restore: the exact cycle-in path the manager uses.
    let restored = RegionSeed::Parked(serialized).into_region(id, || panic!("a parked region never generates")).unwrap();
    assert_eq!(before, crc(&restored), "hash(before park) == hash(after restore), bit-exact");
}

#[test]
fn a_stopped_region_hands_over_its_terrain() {
    let id = RegionCoords::new(0, 0);
    let (mut r, out) = runner(id);
    assert!(!r.handle_input(RegionInput::Shutdown));
    let (rc, output) = out.try_iter().last().expect("the Stopped output");
    assert_eq!(rc, id);
    let RegionOutput::Stopped(_, snap, _, _) = output else { panic!("expected Stopped") };
    // The terrain snapshot of a stud region is its edge columns (what a
    // neighbour's stud halo is seeded from).
    assert_eq!(snap.stud.ground.len(), 4 * game::REGION_CHUNKS - 4);
}

/// A parked blob is a region's whole state: one that does not decode fails
/// the load on the inline spawner too (`LoadFailed`; the manager keeps the
/// region stopped and retries later) and the genesis never runs in its place.
#[test]
fn a_corrupt_parked_blob_fails_the_load_and_is_never_regenerated() {
    use game::RegionSpawner;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let id = RegionCoords::new(1, -2);
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let mut spawner = game::InlineSpawner::default().with_stud_genesis(std::sync::Arc::new(move |_rc| {
        counted.fetch_add(1, Ordering::SeqCst);
        crate::common::uniform_seed()
    }));
    let corrupt = game::SerializedRegion(vec![0xFF; 7]);
    assert!(corrupt.to_rollback().is_err());
    assert!(RegionSeed::Parked(game::SerializedRegion(vec![0xFF; 7])).into_region(id, || panic!("genesis ran")).is_err());

    let (out, out_recv) = unbounded();
    let input = spawner.spawn(id, RegionSeed::Parked(corrupt), out.clone());
    let outputs: Vec<_> = out_recv.try_iter().collect();
    assert_eq!(outputs.len(), 1, "only the failure is reported");
    match &outputs[0] {
        (rc, RegionOutput::LoadFailed(why)) => {
            assert_eq!(*rc, id);
            assert!(why.contains("parked state corrupt"), "{why}");
        }
        (_, other) => panic!("a corrupt parked region must fail its load, got {}", other.name()),
    }
    assert!(spawner.running().is_empty(), "the region does not run");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "genesis must not run over a parked region");
    // Its channel stays open until the manager reaps it (not a crashed region).
    assert!(input.send(RegionInput::Shutdown).is_ok());
    spawner.pump();
    spawner.tick_all();
    assert!(out_recv.try_recv().is_err(), "nothing runs there");
    spawner.reap(id);
    assert!(input.send(RegionInput::Shutdown).is_err(), "reaped: the channel is closed");

    // A fresh seed is what the genesis is for.
    let _input = spawner.spawn(id, RegionSeed::Fresh, out);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(spawner.running(), vec![id]);
}

// --- Inputs applied at the tick the client predicted them after -------------
//
// The client predicts "my input lands after my tick k". Applied on arrival
// instead, it lands wherever the server's clock happens to be: on the other
// side of a tick boundary the client rolls back, and a key edge a tick early
// or late is a whole tick of movement gained or lost — the rubber band.

fn key_input(client: game::ClientId) -> GameEventKind {
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
    let mut runner = game::RegionRunner::new(rc, crate::common::empty_region(rc), out);
    runner.handle_input(game::RegionInput::Event(game::GameEventKind::CreateClient(0)));
    for _ in 0..50 {
        runner.tick();
    }
    assert!(
        runner.region_mut().pending_event_ids().is_empty(),
        "server regions must not retain applied events"
    );
}

// --- Neighbour messages: applied at the end of their slot, in canonical order --
//
// A slot's region-to-region messages wait in the runner's `SlotInbox` and are
// applied, sorted (stamp, class, key, arrival), right before the next `Tick`
// — never on receipt, so thread timing cannot decide their order.

fn pose(x: f32, y: f32, z: f32) -> game::Pose {
    game::Pose::from_translation(game::rapier3d::math::Vector::new(x, y, z))
}

fn eid(x: i32, z: i32, serial: u64) -> game::EntityId {
    game::EntityId { birth_region: RegionCoords::new(x, z), serial }
}

/// A mirror of a player standing in `source`, by its seam (target-local pose).
fn ghost(source: RegionCoords, x: f32) -> GameEventKind {
    GameEventKind::HaloEntity(game::HaloEntityDelta {
        id: eid(source.x, source.z, 1),
        source_region: source,
        kind: game::EntityKind::Player,
        isometry: pose(x, crate::common::above_floor_y(), 128.0),
        linvel: game::rapier3d::math::Vector::ZERO,
        collider: game::ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
    })
}

/// Client `client`'s body crossing in (serial `serial`, born in (-1, 0)).
fn arriving_body(client: game::ClientId, serial: u64) -> GameEventKind {
    GameEventKind::EntityArrived(game::EntityBundle {
        id: eid(-1, 0, serial),
        kind: game::EntityKind::Player,
        body: game::BodyState::kinematic(pose(2.0 + serial as f32, crate::common::above_floor_y(), 128.0)),
        collider: game::ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        carried: game::Carried { camera: Some((0.0, 0.0)), motion: Some(game::Motion::default()), ..Default::default() },
        client: Some((client, game::Client::default())),
        pending_inputs: Vec::new(),
    })
}

fn is_arrival_of(kind: &GameEventKind, client: game::ClientId) -> bool {
    matches!(kind, GameEventKind::EntityArrived(b) if b.client.as_ref().map(|(c, _)| *c) == Some(client))
}

/// A runner past its first tick, outputs drained.
fn ticked_runner() -> (RegionRunner, crossbeam::channel::Receiver<(RegionCoords, RegionOutput)>) {
    let (mut r, out) = runner(RegionCoords::new(0, 0));
    r.tick();
    let _ = processed(&out);
    (r, out)
}

#[test]
fn transfers_of_a_slot_apply_before_the_next_tick_in_canonical_order() {
    let (mut r, out) = ticked_runner();
    let t = r.current_tick();
    let (a, b) = (RegionCoords::new(-1, 0), RegionCoords::new(1, 0));
    let (from_a, from_b) = (ghost(a, 1.0), ghost(b, game::REGION_SIZE - 1.0));
    // On time (stamp = the open slot), B's first.
    r.handle_input(RegionInput::Transfer(from_b.clone(), t));
    r.handle_input(RegionInput::Transfer(from_a.clone(), t));
    assert!(processed(&out).is_empty(), "nothing applied on receipt");
    r.tick();
    let events = processed(&out);
    let kinds: Vec<_> = events.iter().map(|e| e.kind.clone()).collect();
    assert_eq!(kinds, vec![from_a, from_b, GameEventKind::Tick], "sorted by source, then the Tick");
    assert_eq!(events[1].id, events[0].id + 1);
    assert_eq!(events[2].id, events[1].id + 1);
}

#[test]
fn input_for_a_buffered_arrival_follows_it() {
    for stamped in [false, true] {
        let (mut r, out) = ticked_runner();
        let t = r.current_tick();
        r.handle_input(RegionInput::Transfer(arriving_body(7, 1), t));
        let input = key_input(7);
        r.handle_input(if stamped {
            RegionInput::EventAfterTick(input.clone(), t as u64)
        } else {
            RegionInput::Event(input.clone())
        });
        assert!(processed(&out).is_empty(), "stamped {stamped}: the input waits behind the body");
        r.tick();
        let kinds: Vec<_> = processed(&out).into_iter().map(|e| e.kind).collect();
        assert_eq!(kinds.len(), 3, "stamped {stamped}: {kinds:?}");
        assert!(is_arrival_of(&kinds[0], 7), "stamped {stamped}: the body first: {kinds:?}");
        assert_eq!(kinds[1..], [input, GameEventKind::Tick], "stamped {stamped}");
    }
}

#[test]
fn late_transfer_lands_at_the_end_of_the_open_slot() {
    let (mut r, out) = ticked_runner();
    r.tick();
    let _ = processed(&out);
    let t = r.current_tick();
    let (a, b) = (RegionCoords::new(-1, 0), RegionCoords::new(1, 0));
    let (on_time, late) = (ghost(a, 1.0), ghost(b, game::REGION_SIZE - 1.0));
    r.handle_input(RegionInput::Transfer(on_time.clone(), t));
    r.handle_input(RegionInput::Transfer(late.clone(), t - 1));
    assert!(processed(&out).is_empty(), "a late message is not applied on receipt either");
    r.tick();
    let kinds: Vec<_> = processed(&out).into_iter().map(|e| e.kind).collect();
    assert_eq!(kinds, vec![late, on_time, GameEventKind::Tick], "once, by its stamp first, before the Tick");
}

#[test]
fn shutdown_applies_everything_buffered() {
    let (mut r, out) = ticked_runner();
    let t = r.current_tick();
    r.handle_input(RegionInput::Transfer(arriving_body(7, 1), t));
    r.handle_input(RegionInput::Transfer(arriving_body(8, 2), t + 2));
    assert!(processed(&out).is_empty(), "both buffered");
    assert!(!r.handle_input(RegionInput::Shutdown));
    let outputs: Vec<_> = out.try_iter().map(|(_, o)| o).collect();
    let arrived: Vec<usize> = outputs
        .iter()
        .enumerate()
        .filter(|(_, o)| matches!(o, RegionOutput::EventProcessed(e) if matches!(e.kind, GameEventKind::EntityArrived(_))))
        .map(|(i, _)| i)
        .collect();
    let stopped = outputs.iter().position(|o| matches!(o, RegionOutput::Stopped(..))).expect("Stopped");
    assert_eq!(arrived.len(), 2, "both arrivals applied");
    assert!(arrived.iter().all(|&i| i < stopped), "before Stopped");
    let RegionOutput::Stopped(serialized, ..) = &outputs[stopped] else { unreachable!() };
    let parked = serialized.to_rollback().expect("decodes");
    for id in [eid(-1, 0, 1), eid(-1, 0, 2)] {
        assert!(parked.id_index.contains_key(&id), "the parked region holds {id:?}");
    }
}

/// Every output's short name, `EventProcessed` ones as `EventProcessed(<kind>)` — in order.
fn output_names(out: &crossbeam::channel::Receiver<(RegionCoords, RegionOutput)>) -> Vec<String> {
    out.try_iter()
        .map(|(_, o)| match &o {
            RegionOutput::EventProcessed(e) | RegionOutput::EventProcessedHashed(e, ..) => {
                let kind = format!("{:?}", e.kind);
                format!("EventProcessed({})", kind.split(['(', ' ']).next().unwrap_or_default())
            }
            RegionOutput::PlayerParked(c, record, body) => {
                format!("PlayerParked({c}, {}, {})", record.is_some(), body.is_some())
            }
            other => other.name().to_string(),
        })
        .collect()
}

/// A park the manager sends as a plain `Event` (a move right after a crossing) waits
/// behind its client's buffered arrival: the body is in the region when it parks.
#[test]
fn park_for_a_buffered_arrival_follows_it() {
    let (mut r, out) = ticked_runner();
    let t = r.current_tick();
    r.handle_input(RegionInput::Transfer(arriving_body(7, 1), t));
    r.handle_input(RegionInput::Event(GameEventKind::ParkPlayer(7)));
    assert!(out.try_recv().is_err(), "nothing output before the slot closes");
    r.tick();
    let names = output_names(&out);
    let at = |n: &str| names.iter().position(|x| x == n).unwrap_or_else(|| panic!("no {n} in {names:?}"));
    let (arrived, parked, record, tick) = (
        at("EventProcessed(EntityArrived)"),
        at("EventProcessed(ParkPlayer)"),
        at("PlayerParked(7, true, true)"),
        at("EventProcessed(Tick)"),
    );
    assert!(arrived < parked && parked < record && record < tick, "{names:?}");
}

/// A `ClientLeft` likewise waits behind its client's buffered arrival.
#[test]
fn client_left_for_a_buffered_arrival_follows_it() {
    let (mut r, out) = ticked_runner();
    let t = r.current_tick();
    r.handle_input(RegionInput::Transfer(arriving_body(7, 1), t));
    r.handle_input(RegionInput::Event(GameEventKind::ClientLeft(7)));
    assert!(out.try_recv().is_err(), "nothing output before the slot closes");
    r.tick();
    let kinds: Vec<_> = processed(&out).into_iter().map(|e| e.kind).collect();
    assert_eq!(kinds.len(), 3, "{kinds:?}");
    assert!(is_arrival_of(&kinds[0], 7), "the body first: {kinds:?}");
    assert_eq!(kinds[1..], [GameEventKind::ClientLeft(7), GameEventKind::Tick]);
    assert!(!r.region_mut().data().clients.contains_key(&7), "the client left after its body arrived");
}

/// The inputs an arriving body carries (`pending_inputs`) are routed right after its
/// `EntityArrived`, ahead of an input queued behind the arrival (that one is pushed
/// behind the arrival entry, and the carried ones are routed while it is applied);
/// a carried input stamped for a later slot waits for it.
#[test]
fn carried_inputs_follow_the_arrival_before_queued_ones() {
    let (mut r, out) = ticked_runner();
    let t = r.current_tick();
    let look = |dx| GameEventKind::PlayerInput(7, game::InputEvent::MouseMotion { dx, dy: 0.0 });
    let GameEventKind::EntityArrived(mut bundle) = arriving_body(7, 1) else { unreachable!() };
    bundle.pending_inputs = vec![(look(1.0), t as u64), (look(3.0), t as u64 + 1)];
    r.handle_input(RegionInput::Transfer(GameEventKind::EntityArrived(bundle), t));
    r.handle_input(RegionInput::Event(look(2.0)));
    r.tick();
    let kinds: Vec<_> = processed(&out).into_iter().map(|e| e.kind).collect();
    assert_eq!(kinds.len(), 5, "{kinds:?}");
    assert!(is_arrival_of(&kinds[0], 7), "{kinds:?}");
    assert_eq!(kinds[1..], [look(1.0), look(2.0), GameEventKind::Tick, look(3.0)]);
}

/// A late input for a body whose arrival is buffered lands directly after that
/// arrival — where a crossing body's carried inputs land — before the slot's
/// other messages (here a halo message of another source).
#[test]
fn late_input_for_a_buffered_arrival_lands_right_after_it() {
    let (mut r, out) = ticked_runner();
    let t = r.current_tick();
    let other = ghost(RegionCoords::new(1, 0), game::REGION_SIZE - 1.0);
    r.handle_input(RegionInput::Transfer(arriving_body(7, 1), t));
    r.handle_input(RegionInput::Transfer(other.clone(), t));
    let input = key_input(7);
    r.handle_input(RegionInput::Event(input.clone()));
    r.tick();
    let kinds: Vec<_> = processed(&out).into_iter().map(|e| e.kind).collect();
    assert_eq!(kinds.len(), 4, "{kinds:?}");
    assert!(is_arrival_of(&kinds[0], 7), "{kinds:?}");
    assert_eq!(kinds[1..], [input, other, GameEventKind::Tick]);
}

/// With carried inputs and a late one: the arrival, the carried ones (older),
/// the late one, then the slot's other messages.
#[test]
fn carried_then_late_inputs_land_right_after_the_arrival() {
    let (mut r, out) = ticked_runner();
    let t = r.current_tick();
    let look = |dx| GameEventKind::PlayerInput(7, game::InputEvent::MouseMotion { dx, dy: 0.0 });
    let other = ghost(RegionCoords::new(1, 0), game::REGION_SIZE - 1.0);
    let GameEventKind::EntityArrived(mut bundle) = arriving_body(7, 1) else { unreachable!() };
    bundle.pending_inputs = vec![(look(1.0), t as u64)];
    r.handle_input(RegionInput::Transfer(other.clone(), t));
    r.handle_input(RegionInput::Transfer(GameEventKind::EntityArrived(bundle), t));
    r.handle_input(RegionInput::Event(look(2.0)));
    r.tick();
    let kinds: Vec<_> = processed(&out).into_iter().map(|e| e.kind).collect();
    assert_eq!(kinds.len(), 5, "{kinds:?}");
    assert!(is_arrival_of(&kinds[0], 7), "{kinds:?}");
    assert_eq!(kinds[1..], [look(1.0), look(2.0), other, GameEventKind::Tick]);
}

/// `Shutdown` with a save cut pending at a tick the region will not reach: what is
/// buffered for the slots before the cut tick is applied before the `CutSnapshot`,
/// the rest after it, before `Stopped` — nothing lost.
#[test]
fn shutdown_with_a_pending_cut_splits_the_inbox_at_the_cut_tick() {
    let (mut r, out) = ticked_runner();
    let t = r.current_tick();
    r.handle_input(RegionInput::SaveAt(game::CutId(1), t + 2));
    r.handle_input(RegionInput::Transfer(arriving_body(7, 1), t + 1));
    r.handle_input(RegionInput::Transfer(arriving_body(8, 2), t + 2));
    assert!(!r.handle_input(RegionInput::Shutdown));
    let outputs: Vec<_> = out.try_iter().map(|(_, o)| o).collect();
    let arrival_of = |client| {
        outputs
            .iter()
            .position(|o| matches!(o, RegionOutput::EventProcessed(e) if is_arrival_of(&e.kind, client)))
            .unwrap_or_else(|| panic!("client {client}'s body never arrived"))
    };
    let cut = outputs.iter().position(|o| matches!(o, RegionOutput::CutSnapshot(..))).expect("CutSnapshot");
    let stopped = outputs.iter().position(|o| matches!(o, RegionOutput::Stopped(..))).expect("Stopped");
    assert!(arrival_of(7) < cut, "stamped before the cut tick: in the snapshot");
    assert!(cut < arrival_of(8) && arrival_of(8) < stopped, "stamped at the cut tick: after it, before the park");
}

/// A `ListPeople` while a transfer is buffered is answered once the slot closes: after
/// the drain, before the `Tick`.
#[test]
fn list_people_waits_for_the_buffered_transfers() {
    let (mut r, out) = ticked_runner();
    let t = r.current_tick();
    r.handle_input(RegionInput::Transfer(ghost(RegionCoords::new(-1, 0), 1.0), t));
    r.handle_input(RegionInput::ListPeople(5));
    assert!(out.try_recv().is_err(), "not answered while a transfer is buffered");
    r.tick();
    let names = output_names(&out);
    let at = |n: &str| names.iter().position(|x| x == n).unwrap_or_else(|| panic!("no {n} in {names:?}"));
    assert!(at("EventProcessed(HaloEntity)") < at("PeopleListed"), "{names:?}");
    assert!(at("PeopleListed") < at("EventProcessed(Tick)"), "{names:?}");
}
