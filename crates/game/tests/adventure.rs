//! Per-client play mode (`game::adventure`): Fortress by default, switched by
//! `Order::SetMode`, which puts the adventurer's body on a tile; the switch
//! undoes bit-exact and reaches the UI once.
mod common;

use common::floor_chunks;
use game::{
    play_mode_update, state_hash, EntityKey, GameEventKind, Order, PlayMode, Region, RegionCoords, Rollback,
    TilePos, UiUpdate, BLOCK_VOXELS,
};

const CLIENT: game::ClientId = 7;
/// The tile the adventurer is put on: the air above the one-block floor.
const TILE: TilePos = [10, 1, 12];

/// One region with a flat one-block floor around the spawn and one player
/// settled on it, with a render channel (rollback needs one).
fn region_with_player() -> (Region, EntityKey) {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), RegionCoords::new(0, 0), None);
    let (x, z) = region.data().spawn_xz();
    let edge = common::CHUNK_EDGE;
    region.with_data(|r| r.seed_terrain_genesis(floor_chunks(x - edge..x + edge, z - edge..z + edge, 16)));
    region.handle_event(GameEventKind::CreateClient(CLIENT)).unwrap();
    let e = *region.data().player_entites.get(&CLIENT).unwrap();
    for _ in 0..20 {
        tick(&mut region);
    }
    (region, e)
}

fn tick(region: &mut Region) {
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
}

fn mode(region: &Region) -> PlayMode {
    region.data().clients.get(&CLIENT).unwrap().mode
}

fn enter_adventure() -> Order {
    Order::SetMode { mode: PlayMode::Adventure, at: Some(TILE) }
}

#[test]
fn new_client_is_fortress() {
    let (region, _) = region_with_player();
    assert_eq!(mode(&region), PlayMode::Fortress);
}

#[test]
fn set_mode_adventure_places_body_on_tile() {
    let (mut region, e) = region_with_player();
    region.handle_event(GameEventKind::Order(CLIENT, enter_adventure())).unwrap();
    region.forget_last_event();
    tick(&mut region);
    let data = region.data();
    assert_eq!(mode(&region), PlayMode::Adventure);
    let handle = data.ecs.rigidbody.try_get(e).unwrap();
    let t = data.physics.bodies.get(handle).unwrap().translation();
    let bv = BLOCK_VOXELS as f32;
    let (cx, cz) = ((TILE[0] as f32 + 0.5) * bv, (TILE[2] as f32 + 0.5) * bv);
    assert!((t.x - cx).abs() <= 0.5 * bv && (t.z - cz).abs() <= 0.5 * bv, "body at {t:?}, tile centre ({cx}, {cz})");
    assert!(data.ecs.motion.try_get(e).unwrap_or_default().grounded, "body at {t:?} must be grounded");
}

#[test]
fn set_mode_undo_bit_exact() {
    let (mut region, _) = region_with_player();
    let before = state_hash(region.data());
    region.handle_event(GameEventKind::Order(CLIENT, enter_adventure())).unwrap();
    assert_eq!(mode(&region), PlayMode::Adventure);
    region.rollback_last_event();
    assert_eq!(before, state_hash(region.data()), "SetMode must roll back bit-exact");
    assert_eq!(mode(&region), PlayMode::Fortress);
}

#[test]
fn set_mode_adventure_twice_is_a_noop() {
    let (mut region, _) = region_with_player();
    region.handle_event(GameEventKind::Order(CLIENT, enter_adventure())).unwrap();
    region.forget_last_event();
    let before = state_hash(region.data());
    let mut accepted = false;
    region.with_data(|r| accepted = r.set_play_mode_safe(CLIENT, PlayMode::Adventure, Some([20, 1, 20])));
    assert!(accepted);
    assert_eq!(before, state_hash(region.data()));
}

#[test]
fn set_mode_emits_ui_update_once() {
    let (mut region, _) = region_with_player();
    let mut prev = PlayMode::default();
    assert_eq!(play_mode_update(&mut prev, region.data(), CLIENT), None, "Fortress is not news");
    region.handle_event(GameEventKind::Order(CLIENT, enter_adventure())).unwrap();
    region.forget_last_event();
    assert_eq!(
        play_mode_update(&mut prev, region.data(), CLIENT),
        Some(UiUpdate::PlayMode(PlayMode::Adventure))
    );
    tick(&mut region);
    assert_eq!(play_mode_update(&mut prev, region.data(), CLIENT), None, "sent only once");
    region
        .handle_event(GameEventKind::Order(CLIENT, Order::SetMode { mode: PlayMode::Fortress, at: None }))
        .unwrap();
    assert_eq!(
        play_mode_update(&mut prev, region.data(), CLIENT),
        Some(UiUpdate::PlayMode(PlayMode::Fortress))
    );
}
