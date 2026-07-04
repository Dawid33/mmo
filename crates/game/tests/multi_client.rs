//! Multi-client reconcile: foreign events are insertions into the local
//! timeline, not mispredictions. See
//! docs/superpowers/specs/2026-07-04-multi-client-players-design.md
use std::collections::BTreeMap;
use std::hash::Hash;

use game::{ChunkCoords, GameEventKind, InputEvent, Key, Region, World};

fn state_hash(r: &game::Rollback) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    r.hash(&mut hasher);
    hasher.finalize()
}

fn r0() -> ChunkCoords {
    ChunkCoords::new(0, 0, 0)
}

/// Server world with `n` players created on connection (the new join flow),
/// plus the broadcast CreateClient events it produced.
fn server_with_players(n: usize) -> (World, Vec<game::GameEvent>) {
    let mut server = World::basic();
    let mut events = Vec::new();
    for client_id in 0..n {
        let ev = server
            .handle_region_event(GameEventKind::CreateClient(client_id), r0())
            .unwrap();
        server.forget_last_event(&r0());
        events.push(ev);
    }
    (server, events)
}

/// Client world joined from a server snapshot, as `handle_server` does.
/// The rollback machinery requires a live `GameDataUpdate` channel on the
/// client (undo closures send render updates through it), so the receiver
/// is returned and must be kept alive by the caller.
fn join_client(
    server: &World,
    client_id: usize,
) -> (World, crossbeam::channel::Receiver<game::GameDataUpdate>) {
    let (send, recv) = crossbeam::channel::unbounded();
    let snapshot = server.get_region_data(&r0());
    let mut world = World::new();
    world.load(&r0(), Region::new(snapshot, Some(send), r0(), Some(client_id)));
    (world, recv)
}

/// Run one lockstep tick: client predicts, server executes, client reconciles.
fn lockstep_tick(server: &mut World, client: &mut World) {
    let mut client_results = BTreeMap::new();
    let mut server_results = BTreeMap::new();
    client.progress_world_one_tick(&mut client_results);
    server.progress_world_one_tick(&mut server_results);
    let ev = server_results.get(&r0()).unwrap().as_ref().unwrap().clone();
    client.reconcile_event(ev).unwrap();
}

#[test]
fn join_snapshot_already_contains_player_and_stale_create_is_dropped() {
    let (mut server, events) = server_with_players(1);
    let (mut client, _recv) = join_client(&server, 0);

    // Snapshot already contains our player.
    assert!(client.data(&r0()).player_entites.contains_key(&0));
    let h = state_hash(client.data(&r0()));

    // The broadcast of our own CreateClient arrives after the snapshot:
    // it must be dropped, not applied a second time.
    client.reconcile_event(events[0].clone()).unwrap();
    assert_eq!(h, state_hash(client.data(&r0())));

    // And it must not linger in the input buffer poisoning later
    // reconciles: the next server tick must confirm the next prediction.
    lockstep_tick(&mut server, &mut client);
    assert_eq!(
        client.regions.get(&r0()).unwrap().pending_event_ids(),
        Vec::<usize>::new()
    );
    assert_eq!(state_hash(client.data(&r0())), state_hash(server.data(&r0())));
}

#[test]
fn origin_client_classifies_event_kinds() {
    let input = InputEvent::Key { key: Key::KeyW, pressed: true };
    assert_eq!(GameEventKind::PlayerInput(7, input).origin_client(), Some(7));
    assert_eq!(GameEventKind::CreateClient(3).origin_client(), Some(3));
    assert_eq!(GameEventKind::Tick.origin_client(), None);
    assert_eq!(GameEventKind::Quit.origin_client(), None);
}

#[test]
fn foreign_create_client_inserts_into_predicted_timeline() {
    let (mut server, _) = server_with_players(1);
    let (mut client_a, _recv) = join_client(&server, 0);

    // A predicts a tick (id = N) before hearing that B joined.
    let mut client_results = BTreeMap::new();
    client_a.progress_world_one_tick(&mut client_results);
    let predicted_ids = client_a.regions.get(&r0()).unwrap().pending_event_ids();
    assert_eq!(predicted_ids.len(), 1);
    let n = predicted_ids[0];

    // Server creates B's player at id N, then ticks at id N+1.
    let ev_create_b = server
        .handle_region_event(GameEventKind::CreateClient(1), r0())
        .unwrap();
    server.forget_last_event(&r0());
    assert_eq!(ev_create_b.id, n);
    let mut server_results = BTreeMap::new();
    server.progress_world_one_tick(&mut server_results);
    let ev_tick = server_results.get(&r0()).unwrap().as_ref().unwrap().clone();

    // A reconciles the foreign CreateClient: it is an insertion — the
    // pending tick must survive, bumped to id N+1.
    client_a.reconcile_event(ev_create_b).unwrap();
    assert_eq!(
        client_a.regions.get(&r0()).unwrap().pending_event_ids(),
        vec![n + 1]
    );
    assert!(client_a.data(&r0()).player_entites.contains_key(&1));

    // The server tick then confirms the bumped prediction exactly.
    client_a.reconcile_event(ev_tick).unwrap();
    assert_eq!(
        client_a.regions.get(&r0()).unwrap().pending_event_ids(),
        Vec::<usize>::new()
    );
    assert_eq!(state_hash(client_a.data(&r0())), state_hash(server.data(&r0())));
}

#[test]
fn foreign_player_input_converges_and_undo_stays_bit_exact() {
    let (mut server, mut events) = server_with_players(2);
    let (mut client_a, _recv) = join_client(&server, 0);
    // A joined after both players existed: both CreateClient broadcasts are
    // stale for A and must be dropped.
    for ev in events.drain(..) {
        client_a.reconcile_event(ev).unwrap();
    }
    assert_eq!(state_hash(client_a.data(&r0())), state_hash(server.data(&r0())));

    // A predicts two ticks ahead.
    let mut client_results = BTreeMap::new();
    client_a.progress_world_one_tick(&mut client_results);
    client_a.progress_world_one_tick(&mut client_results);

    // Server interleaves B's input into the same id range.
    let input = InputEvent::Key { key: Key::KeyW, pressed: true };
    let ev_b_input = server
        .handle_region_event(GameEventKind::PlayerInput(1, input), r0())
        .unwrap();
    server.forget_last_event(&r0());
    let mut server_results = BTreeMap::new();
    server.progress_world_one_tick(&mut server_results);
    let ev_t1 = server_results.get(&r0()).unwrap().as_ref().unwrap().clone();
    server.progress_world_one_tick(&mut server_results);
    let ev_t2 = server_results.get(&r0()).unwrap().as_ref().unwrap().clone();

    // Foreign input inserted mid-log (exercises rollback + re-apply of the
    // whole pending log; the rollback machinery enforces bit-exact hashes).
    client_a.reconcile_event(ev_b_input).unwrap();
    // Both pending ticks survived, ids bumped by one.
    assert_eq!(
        client_a.regions.get(&r0()).unwrap().pending_event_ids().len(),
        2
    );
    client_a.reconcile_event(ev_t1).unwrap();
    client_a.reconcile_event(ev_t2).unwrap();
    assert_eq!(
        client_a.regions.get(&r0()).unwrap().pending_event_ids(),
        Vec::<usize>::new()
    );
    assert_eq!(state_hash(client_a.data(&r0())), state_hash(server.data(&r0())));
}

#[test]
fn create_player_sets_entity_kind() {
    let (server, _) = server_with_players(1);
    let data = server.data(&r0());
    let e = *data.player_entites.get(&0).unwrap();
    assert_eq!(*data.ecs.kind.try_get(e), Some(game::EntityKind::Player));
}
