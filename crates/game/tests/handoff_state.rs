use game::Client;
use game::Rollback;
use game::StateHash;

fn crc(rb: &Rollback) -> u32 {
    let mut h = crc32fast::Hasher::new();
    rb.data.state_hash(&mut h);
    h.finalize()
}

/// Regression: region crossing froze input in the running client (sim thread panicked on hash bar, "input stopped while the window kept drawing"), fix 00eb8fe1 — symptom described from play
#[test]
fn remove_then_create_in_one_transaction_holds_the_hash_bar() {
    // Region-crossing repro: a single reconcile-replayed Tick can BOTH remove
    // an entity (a halo mirror's end / extraction) AND create one (a mirror /
    // apply_arrival). The slotmap reuses the just-freed slot index for the new
    // entity, so the secondary-map (kind/camera/...) slot at that index gets
    // reallocated mid-transaction. Rolling the whole transaction back must
    // still restore hash(before), bit-exact.
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.create_player_safe(1);
    rb.forget(); // bake: player 1 occupies some entity/component slot index
    let before = crc(&rb);
    let key1 = *rb.data.player_entites.get(&1).unwrap();

    rb.new_transaction();
    rb.remove_entity_safe(key1, Some(1)); // frees the slot index
    rb.create_player_safe(2); // reuses the freed index → clobbers the secondary-map slot
    rb.rollback();

    assert_eq!(
        before,
        crc(&rb),
        "remove+create (slot reuse) in one transaction must undo bit-exact"
    );
}

/// Donor rollback: create a player in a source region to get a real
/// (region, key) identity to transfer.
fn donor() -> (Rollback, game::EntityKey) {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.create_player_safe(9);
    rb.forget();
    let key = *rb.data.player_entites.get(&9).unwrap();
    (rb, key)
}

/// Regression: view snapped to default facing on first mouse move after a region flip, seen in play (fix e44bd480: "the first mouse move after a region flip snapped the view")
#[test]
fn arrival_preserves_camera_look_direction() {
    // Camera yaw/pitch are the authoritative look scalars (the body
    // quaternion is derived from them on every look change). They must
    // survive a region crossing, or the first mouse move after the flip
    // snaps the camera back to the default facing.
    let (mut donor_rb, src_key) = donor();

    donor_rb.new_transaction();
    donor_rb.data.clients.insert(9, Client::default());
    let mut cam = donor_rb.data.ecs.camera.try_get(src_key).clone().unwrap();
    cam.yaw = 1.25;
    cam.pitch = -0.5;
    donor_rb.data.ecs.camera.set_safe(src_key, Some(cam));
    donor_rb.forget();

    donor_rb.new_transaction();
    let bundle = donor_rb.extract_entity_safe(src_key);

    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.apply_arrival(bundle);
    let e = *rb.data.player_entites.get(&9).unwrap();
    let cam = rb.data.ecs.camera.try_get(e).clone().unwrap();
    assert_eq!(cam.yaw, 1.25, "yaw must survive the region crossing");
    assert_eq!(cam.pitch, -0.5, "pitch must survive the region crossing");
}

// ---- generated bundle: exhaustive classification (Task 2.2) ----

// ---- dynamic bodies: EntityKind::Body (Task 7.2) ----
