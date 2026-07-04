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
fn join_client(server: &World, client_id: usize) -> World {
    let snapshot = server.get_region_data(&r0());
    let mut world = World::new();
    world.load(&r0(), Region::new(snapshot, None, r0(), Some(client_id)));
    world
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
    let mut client = join_client(&server, 0);

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
