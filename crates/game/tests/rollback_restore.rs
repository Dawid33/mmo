use std::hash::Hash;

use game::Rollback;

fn state_hash(r: &Rollback) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    r.hash(&mut hasher);
    hasher.finalize()
}

#[test]
fn rollback_of_create_player_restores_state() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    let before = state_hash(&r);

    r.new_transaction();
    r.create_player_safe(0);
    assert_ne!(before, state_hash(&r), "create_player_safe changed nothing");

    r.rollback();
    assert_eq!(before, state_hash(&r), "rollback did not restore state");
}

#[test]
fn rollback_of_create_mesh_restores_state() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    let before = state_hash(&r);

    r.new_transaction();
    r.create_mesh(game::ChunkCoords::new(0, 0, 0));
    assert_ne!(before, state_hash(&r), "create_mesh changed nothing");

    r.rollback();
    assert_eq!(before, state_hash(&r), "rollback did not restore state");
}
