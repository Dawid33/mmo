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
    r.player_entites.undo(|d, _| {
        d.remove(&3);
    });
    r.player_entites.insert(3, rollback::EntityKey::default());
    r.tick.update(|t| *t += 10);

    r.rollback();
    assert_eq!(h0, state_hash(&r));
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
