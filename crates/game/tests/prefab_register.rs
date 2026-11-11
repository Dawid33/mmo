//! Per-player prefab registration: the rollback bar, the caps, hostile input, travel.

use crate::common::prefabs::{arch, build_file, build_of_mut, empty_build, part, resolved, rich};
use crate::common::{r0, region_of, uniform_seed};
use game::{
    BuildFile, GameEventKind, PrefabCommand, PrefabKey, Region, RegisteredPrefab, ResolvedPrefab,
    MAX_REGISTERED_ELEMENTS, MAX_REGISTERED_PREFABS, PREFAB_MAX_PARTS,
};

fn region() -> Region {
    let mut r = region_of(r0(), uniform_seed());
    r.handle_event(GameEventKind::CreateClient(0)).unwrap();
    r.forget_last_event();
    r
}

fn hash(r: &Region) -> u32 {
    game::state_hash(r.data())
}

/// The hash of the clients table alone. Every handled event advances the region's event
/// counter, so a refused command still moves [`hash`]; this is what it must not move.
fn clients_hash(r: &Region) -> u32 {
    use state_hash::StateHash;
    let mut h = crc32fast::Hasher::new();
    r.data().clients.state_hash(&mut h);
    h.finalize()
}

fn prefab(r: &mut Region, cmd: PrefabCommand) {
    r.handle_event(GameEventKind::Prefab(0, cmd)).unwrap();
}

fn register(r: &mut Region, p: &ResolvedPrefab) {
    prefab(r, PrefabCommand::Register(Box::new(p.clone())));
}

/// `n` distinct small prefabs.
fn distinct(n: usize) -> Vec<ResolvedPrefab> {
    (0..n as i32).map(|i| resolved(&build_file("p", BuildFile { parts: vec![part("3001", [i, 0, 0], 0)], ..empty_build() }))).collect()
}

/// A prefab of `n` 1 x 1 bricks, distinct per `tag`.
fn big(n: usize, tag: i32) -> ResolvedPrefab {
    let mut parts = vec![part("3005", [0, 0, 0], 0); n];
    parts[0].min[0] = tag;
    resolved(&build_file("big", BuildFile { parts, ..empty_build() }))
}

fn registered(r: &Region) -> Vec<PrefabKey> {
    r.data().clients[&0].prefabs.keys().copied().collect()
}

#[test]
fn register_then_undo_restores_the_hash() {
    let mut r = region();
    let before = hash(&r);
    let p = resolved(&arch());
    register(&mut r, &p);
    let got = &r.data().clients[&0].prefabs[&p.key()];
    assert_eq!((got.key, &*got.def), (p.key(), &p));
    assert_ne!(before, hash(&r));
    r.rollback_last_event();
    assert_eq!(before, hash(&r));
    assert!(registered(&r).is_empty());
}

#[test]
fn deregister_then_undo_restores_the_hash() {
    let mut r = region();
    let p = resolved(&arch());
    register(&mut r, &p);
    r.forget_last_event();
    let before = hash(&r);
    prefab(&mut r, PrefabCommand::Deregister(p.key()));
    assert!(registered(&r).is_empty());
    assert_ne!(before, hash(&r));
    r.rollback_last_event();
    assert_eq!(before, hash(&r));
    assert_eq!(registered(&r), vec![p.key()]);
    // Deregistering a key that is not there changes nothing.
    let clients = clients_hash(&r);
    prefab(&mut r, PrefabCommand::Deregister(PrefabKey(p.key().0 ^ 1)));
    assert_eq!(clients, clients_hash(&r));
}

#[test]
fn registering_twice_changes_nothing() {
    let mut r = region();
    let p = resolved(&rich());
    register(&mut r, &p);
    let once = clients_hash(&r);
    register(&mut r, &p);
    assert_eq!(once, clients_hash(&r));
    assert_eq!(registered(&r).len(), 1);
}

#[test]
fn ninth_prefab_is_refused() {
    let mut r = region();
    let all = distinct(MAX_REGISTERED_PREFABS + 1);
    for p in &all[..MAX_REGISTERED_PREFABS] {
        register(&mut r, p);
    }
    assert_eq!(registered(&r).len(), 8);
    let h8 = clients_hash(&r);
    register(&mut r, &all[MAX_REGISTERED_PREFABS]);
    assert_eq!(registered(&r).len(), 8);
    assert_eq!(h8, clients_hash(&r));
    // Room again after a deregister.
    prefab(&mut r, PrefabCommand::Deregister(all[0].key()));
    register(&mut r, &all[MAX_REGISTERED_PREFABS]);
    assert!(registered(&r).contains(&all[MAX_REGISTERED_PREFABS].key()));
}

#[test]
fn element_budget_is_enforced() {
    let mut r = region();
    assert_eq!(2 * PREFAB_MAX_PARTS, MAX_REGISTERED_ELEMENTS);
    register(&mut r, &big(PREFAB_MAX_PARTS, 0));
    register(&mut r, &big(PREFAB_MAX_PARTS, 1));
    assert_eq!(registered(&r).len(), 2);
    let full = clients_hash(&r);
    register(&mut r, &distinct(1)[0]);
    assert_eq!(registered(&r).len(), 2);
    assert_eq!(full, clients_hash(&r));
}

#[test]
fn hand_built_invalid_prefab_is_refused() {
    let good = resolved(&arch());
    let part_index_out_of_range = {
        let mut p = good.clone();
        build_of_mut(&mut p).parts[0].part = u16::MAX;
        p
    };
    let data_nonzero = {
        let mut p = good.clone();
        build_of_mut(&mut p).parts[0].data = 3;
        p
    };
    let parts_5000 = {
        let mut p = good.clone();
        let one = build_of_mut(&mut p).parts[0];
        build_of_mut(&mut p).parts = vec![one; 5000];
        p
    };
    let cell_at_x_48 = {
        let mut p = good.clone();
        build_of_mut(&mut p).parts[0].min = [48, 0, 0];
        p
    };
    let bad_rotation = {
        let mut p = good.clone();
        build_of_mut(&mut p).parts[0].rot = 9;
        p
    };
    let mut r = region();
    for bad in [part_index_out_of_range, data_nonzero, parts_5000, cell_at_x_48, bad_rotation] {
        let h = clients_hash(&r);
        register(&mut r, &bad);
        assert_eq!(h, clients_hash(&r));
        assert!(registered(&r).is_empty());
    }
}

#[test]
fn an_unknown_client_registers_nothing() {
    let mut r = region();
    let h = clients_hash(&r);
    r.handle_event(GameEventKind::Prefab(7, PrefabCommand::Register(Box::new(resolved(&arch()))))).unwrap();
    assert_eq!(h, clients_hash(&r));
    // And a refused command undoes to the same state as any other event.
    let before = hash(&r);
    r.handle_event(GameEventKind::Prefab(7, PrefabCommand::Deregister(PrefabKey(1)))).unwrap();
    r.rollback_last_event();
    assert_eq!(before, hash(&r));
}

#[test]
fn prefabs_travel_on_arrive() {
    let mut r = region();
    register(&mut r, &resolved(&arch()));
    let c = r.data().clients[&0].clone().arrive();
    assert_eq!(c.prefabs.len(), 1);
}

#[test]
fn a_parked_record_keeps_no_registrations() {
    // Registrations last for the session: the client registers again after connecting.
    let mut r = region();
    register(&mut r, &resolved(&arch()));
    assert!(game::parked_client(&r.data().clients[&0]).prefabs.is_empty());
}

#[test]
fn registered_prefabs_survive_a_snapshot_round_trip() {
    let mut r = region();
    let p = resolved(&rich());
    register(&mut r, &p);
    let client = r.data().clients[&0].clone();
    let bytes = game::encode_packet(&client).unwrap();
    let back: game::Client = game::decode_packet(&bytes).unwrap();
    assert_eq!(back, client);
    assert_eq!(*back.prefabs[&p.key()].def, p);
}

#[test]
fn a_stored_key_is_never_trusted() {
    // The wire form is the definition alone: the key is always recomputed from it.
    let p = resolved(&arch());
    let lying = RegisteredPrefab { key: PrefabKey(1), def: std::sync::Arc::new(p.clone()) };
    let back: RegisteredPrefab = game::decode_packet(&game::encode_packet(&lying).unwrap()).unwrap();
    assert_eq!(back.key, p.key());
}

#[test]
fn the_command_travels_as_a_client_event() {
    let e = GameEventKind::Prefab(3, PrefabCommand::Deregister(PrefabKey(9)));
    assert_eq!(e.input_client(), Some(3));
    assert_eq!(e.origin_client(), Some(3));
    let back: GameEventKind = game::decode_packet(&game::encode_packet(&e).unwrap()).unwrap();
    assert_eq!(back, e);
}
