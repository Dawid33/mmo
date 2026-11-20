use crossbeam::channel::unbounded;
use game::StateHash;
use game::{
    Chunk, ChunkCoords, GameEventKind, Region, RegionCoords, RegionInput, RegionOutput,
    RegionRunner,
};

fn flat_chunks() -> Vec<(ChunkCoords, Chunk)> {
    (0..2)
        .flat_map(|x| (0..2).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(8))))
        .collect()
}

fn crc(region: &Region) -> u32 {
    let mut h = crc32fast::Hasher::new();
    region.data().data.state_hash(&mut h);
    h.finalize()
}

fn runner(
    id: RegionCoords,
) -> (
    RegionRunner,
    crossbeam::channel::Receiver<(RegionCoords, RegionOutput)>,
) {
    let (out_send, out_recv) = unbounded();
    let region = Region::from_chunks(id, flat_chunks());
    (RegionRunner::new(id, region, out_send), out_recv)
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
        game::InputEvent::Key {
            key: game::Key::KeyW,
            pressed: true,
        },
    )
}

fn processed(
    out: &crossbeam::channel::Receiver<(RegionCoords, RegionOutput)>,
) -> Vec<game::GameEvent> {
    out.try_iter()
        .filter_map(|(_, o)| match o {
            RegionOutput::EventProcessed(ev) => Some(ev),
            _ => None,
        })
        .collect()
}

fn runner_with_player() -> (
    RegionRunner,
    crossbeam::channel::Receiver<(RegionCoords, RegionOutput)>,
) {
    let (mut r, out) = runner(RegionCoords::new(0, 0));
    r.handle_input(RegionInput::Event(GameEventKind::CreateClient(7)));
    r.tick();
    let _ = processed(&out);
    (r, out)
}

/// Regression: live rubber-banding: stamped input slotted on arrival, every input mispredicted; measured live with [client-mispredict]/[client-motion] probes (fix 8e90481f; docs/memories/input-slot-rubber-banding.md)
#[test]
fn input_predicted_after_a_future_tick_waits_for_it() {
    let (mut r, out) = runner_with_player();
    let now = r.current_tick();
    r.handle_input(RegionInput::EventAfterTick(key_input(7), now as u64 + 2));
    assert!(
        processed(&out).is_empty(),
        "two ticks early: nothing applied yet"
    );

    r.tick();
    let kinds: Vec<_> = processed(&out).into_iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        vec![GameEventKind::Tick],
        "one tick early: still held"
    );

    r.tick();
    let events = processed(&out);
    let kinds: Vec<_> = events.iter().map(|e| e.kind.clone()).collect();
    assert_eq!(
        kinds,
        vec![GameEventKind::Tick, key_input(7)],
        "applied right after its tick"
    );
    assert_eq!(
        events[1].id,
        events[0].id + 1,
        "and takes the very next id, as the client predicted"
    );
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
    assert_eq!(
        crc(&client),
        crc(server.region_mut()),
        "client and server must agree either way"
    );
    rollbacks
}

/// Regression: live rubber-banding: leading client rolled back on its own stamped input; measured live with [client-mispredict] probe (fix 8e90481f; docs/memories/input-slot-rubber-banding.md)
#[test]
fn leading_client_reconciles_a_stamped_input_without_rollback() {
    assert_eq!(rollbacks_of_a_leading_client(true), 0);
    assert!(
        rollbacks_of_a_leading_client(false) > 0,
        "applied on arrival, the same input lands ticks early and the client rolls back"
    );
}

// --- Neighbour messages: applied at the end of their slot, in canonical order --
//
// A slot's region-to-region messages wait in the runner's `SlotInbox` and are
// applied, sorted (stamp, class, key, arrival), right before the next `Tick`
// — never on receipt, so thread timing cannot decide their order.
