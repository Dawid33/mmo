use game::StateHash;

use game::Rollback;

fn state_hash(r: &Rollback) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    r.state_hash(&mut hasher);
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
fn rollback_after_slot_reuse_restores_camera_hash() {
    // Regression: an entity created via create_entity_safe carries a None
    // camera (and kind) slot. remove_entity_safe must remove those slots
    // unconditionally — symmetric with create — or the entities slotmap frees
    // the index while the None slot dangles. A later create reusing that index
    // clobbers the dangling slot, and the insert's remove-undo is no longer a
    // true inverse: hash(before) != hash(after undo). Reproduces the observed
    // "self.ecs.camera.hash_data() ... .list.slots.N is missing" rollback panic.
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    let before = state_hash(&r);

    // tx1: create a mesh entity — camera/kind slots exist but hold None.
    r.new_transaction();
    let a = r.create_mesh(game::ChunkCoords::new(0, 0, 0), game::Chunk::flat_floor(8));

    // tx2: remove it, freeing its entities slot index for reuse.
    r.new_transaction();
    r.remove_entity_safe(a, None);

    // tx3: create another mesh — reuses the just-freed slot index.
    r.new_transaction();
    let _b = r.create_mesh(game::ChunkCoords::new(0, 0, 0), game::Chunk::flat_floor(8));

    // Roll the whole batch back. Per-transaction hash self-verification panics
    // inside rollback if any undo isn't a bit-exact inverse.
    r.rollback();
    r.rollback();
    r.rollback();

    assert_eq!(
        before,
        state_hash(&r),
        "slot reuse across remove broke rollback"
    );
}

#[test]
fn sim_survives_dropped_render_receiver() {
    // Shutdown-ordering race (observed as a crash when crossing regions
    // rapidly): when the client window closes, the render thread drops its
    // per-region Receiver<GameDataUpdate>, but the game thread keeps
    // reconciling. Every bridge send in the rollback machinery — the Do-path
    // `send`, the slotmap emits, and `emit_on_undo` during rollback — must
    // tolerate a disconnected receiver instead of unwrapping into a panic
    // that kills the game thread (and cascades into the netcode writer).
    let (send, recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    let before = state_hash(&r);

    // Window closed: the render bridge receiver is gone from here on.
    drop(recv);

    // Do-path sends fire here; the rollback fires the emit_on_undo sends.
    r.new_transaction();
    r.create_player_safe(0);
    r.rollback();

    assert_eq!(
        before,
        state_hash(&r),
        "rollback did not restore state after receiver drop"
    );
}

#[test]
fn rollback_of_create_mesh_restores_state() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    let before = state_hash(&r);

    r.new_transaction();
    r.create_mesh(game::ChunkCoords::new(0, 0, 0), game::Chunk::flat_floor(8));
    assert_ne!(before, state_hash(&r), "create_mesh changed nothing");

    r.rollback();
    assert_eq!(before, state_hash(&r), "rollback did not restore state");
}

#[test]
fn rollback_of_mixed_scale_create_mesh_restores_state() {
    // flat_floor(24) yields both whole solid blocks (block-scale collider)
    // and chiseled blocks (voxel-scale collider): two attaches, two
    // PhysicsState snapshots, both must unwind bit-exactly.
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    let before = state_hash(&r);

    r.new_transaction();
    r.create_mesh(game::ChunkCoords::new(0, 0, 0), game::Chunk::flat_floor(24));
    assert_ne!(before, state_hash(&r), "create_mesh changed nothing");

    r.rollback();
    assert_eq!(before, state_hash(&r), "rollback did not restore state");
}
