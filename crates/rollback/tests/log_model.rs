//! Invariants of the transaction log that must hold before and after the
//! log-model refactor. Uses only public API that exists in both worlds.
use std::hash::Hash;

use rollback::{ChunkCoords, Rollback};

fn state_hash(r: &Rollback) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    r.hash(&mut hasher);
    hasher.finalize()
}

fn new_rollback() -> (
    Rollback,
    crossbeam::channel::Receiver<rollback::GameDataUpdate>,
) {
    let (send, recv) = crossbeam::channel::unbounded();
    (Rollback::new(Some(send)), recv)
}

#[test]
fn multi_transaction_rollback_restores_each_boundary() {
    let (mut r, _recv) = new_rollback();
    let h0 = state_hash(&r);

    r.new_transaction();
    r.create_mesh(ChunkCoords::new(0, 0, 0));
    let h1 = state_hash(&r);

    r.new_transaction();
    r.create_player_safe(7);
    assert_ne!(h1, state_hash(&r));

    r.rollback();
    assert_eq!(h1, state_hash(&r), "rollback of tx2 must land on tx1 boundary");
    r.rollback();
    assert_eq!(h0, state_hash(&r), "rollback of tx1 must land on initial state");
}

#[test]
fn forget_drops_oldest_transaction_and_keeps_state() {
    let (mut r, _recv) = new_rollback();

    r.new_transaction();
    r.create_mesh(ChunkCoords::new(0, 0, 0));
    r.new_transaction();
    r.create_player_safe(7);
    let h2 = state_hash(&r);

    r.forget(); // drop tx1's undo info; state untouched
    assert_eq!(h2, state_hash(&r));
    assert_eq!(r.oldest(), 1);

    // tx2 must still be rollback-able after forgetting tx1.
    r.rollback();
    // No hash target for the tx1 boundary anymore, but rollback must not
    // panic and must decrement current.
    assert_eq!(r.current(), 1);
}

#[test]
fn lifo_order_is_preserved_across_fields() {
    // player_entites and tick are different fields; undos must pop LIFO
    // across fields within a transaction.
    let (mut r, _recv) = new_rollback();
    let h0 = state_hash(&r);

    r.new_transaction();
    r.tick.update(|t| *t += 1);
    r.player_entites.insert(3, rollback::EntityKey::default());
    r.tick.update(|t| *t += 10);

    r.rollback();
    assert_eq!(h0, state_hash(&r));
}

#[test]
fn undo_scope_snapshot_registration_rolls_back() {
    use std::ops::DerefMut;
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    let key = r.ecs.create_entity_safe();
    let h1 = state_hash(&r);

    r.new_transaction();
    let old: rollback::Ecs = (*r.ecs).clone();
    let mut scope = r.ecs.undo_scope();
    scope.raw_fields().camera.insert(key, Some(Default::default()));
    scope.register(move |ecs, _| *ecs = old);
    r.rollback();
    assert_eq!(h1, state_hash(&r));
}

#[test]
#[should_panic(expected = "UndoScope mutated without register")]
#[cfg(debug_assertions)]
fn undo_scope_drop_without_register_panics() {
    use std::ops::DerefMut;
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    let key = r.ecs.create_entity_safe();
    let mut scope = r.ecs.undo_scope();
    scope.raw_fields().camera.insert(key, Some(Default::default()));
    drop(scope);
}

#[test]
fn create_entity_rolls_back_without_snapshot_and_reuses_key() {
    let (mut r, _recv) = new_rollback();
    let h0 = state_hash(&r);

    r.new_transaction();
    let k1 = r.ecs.create_entity_safe();
    r.rollback();
    assert_eq!(h0, state_hash(&r), "entity creation must fully revert");

    // Determinism: after rollback the next insert must allocate the SAME key.
    r.new_transaction();
    let k2 = r.ecs.create_entity_safe();
    assert_eq!(k1, k2, "key allocation must be deterministic across rollback");
}

#[test]
fn entity_creation_auto_emits_in_both_directions() {
    let (mut r, recv) = new_rollback();
    r.new_transaction();
    let key = r.ecs.create_entity_safe();

    let applied: Vec<_> = recv.try_iter().collect();
    assert!(
        applied.iter().any(|u| matches!(u.update_kind, rollback::GameDataUpdateKind::CreateEntity(k) if k == key)),
        "apply must emit CreateEntity, got {applied:?}"
    );

    r.rollback();
    let undone: Vec<_> = recv.try_iter().collect();
    assert!(
        undone.iter().any(|u| matches!(u.update_kind, rollback::GameDataUpdateKind::RemoveEntity(k) if k == key)),
        "undo must emit RemoveEntity, got {undone:?}"
    );
}

#[test]
fn player_creation_emits_camera_pair() {
    let (mut r, recv) = new_rollback();
    r.new_transaction();
    r.create_player_safe(0);

    let applied: Vec<_> = recv.try_iter().collect();
    assert!(
        applied.iter().any(|u| matches!(
            u.update_kind,
            rollback::GameDataUpdateKind::AddCameraComponent(..)
        )),
        "apply must emit AddCameraComponent, got {applied:?}"
    );

    r.rollback();
    let undone: Vec<_> = recv.try_iter().collect();
    assert!(
        undone.iter().any(|u| matches!(
            u.update_kind,
            rollback::GameDataUpdateKind::RemoveCameraComponent(_)
        )),
        "undo must emit RemoveCameraComponent, got {undone:?}"
    );
}

#[test]
fn undomap_ops_roll_back_exactly() {
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    r.player_entites.insert(1, rollback::EntityKey::default());
    let h1 = state_hash(&r);

    r.new_transaction();
    r.player_entites.insert(1, rollback::EntityKey::default()); // overwrite
    r.player_entites.insert(2, rollback::EntityKey::default()); // fresh insert
    r.player_entites.remove(&1); // remove
    r.rollback();
    assert_eq!(h1, state_hash(&r), "insert-overwrite/insert/remove must all revert");
    assert!(r.player_entites.get(&1).is_some());
    assert!(r.player_entites.get(&2).is_none());
}

#[test]
fn undocell_update_is_rolled_back() {
    let (mut r, _recv) = new_rollback();
    let h0 = state_hash(&r);
    r.new_transaction();
    r.tick.update(|t| *t += 1);
    r.next_game_event_id.update(|n| *n += 5);
    assert_eq!(*r.tick, 1);
    r.rollback();
    assert_eq!(*r.tick, 0);
    assert_eq!(h0, state_hash(&r));
}

#[test]
fn undocell_set_is_rolled_back() {
    let (mut r, _recv) = new_rollback();
    let h0 = state_hash(&r);
    r.new_transaction();
    r.tick.set(42);
    assert_eq!(*r.tick, 42);
    r.rollback();
    assert_eq!(h0, state_hash(&r));
}
