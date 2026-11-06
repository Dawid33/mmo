use game::{Pose, Rollback};
use game::{
    Client, ColliderSpec, EntityBundle, EntityKind, GhostData, RegionCoords, GHOST_TTL_TICKS,
};
use game::parry::math::Vector;
use game::GameDataUpdateKind;
use game::StateHash;

fn crc(rb: &Rollback) -> u32 {
    let mut h = crc32fast::Hasher::new();
    rb.data.state_hash(&mut h);
    h.finalize()
}

fn pose(x: f32, y: f32, z: f32) -> Pose {
    Pose::from_translation(game::rapier3d::math::Vector::new(x, y, z))
}

#[test]
fn remove_entity_safe_holds_the_hash_bar() {
    // Dummy render sender: the generated rollback() replays tier-2 undo
    // closures with `client.as_ref().unwrap()`, so a None client panics on
    // rollback. Every sibling rollback test (kinematic/random_ops/
    // rollback_restore/log_model) wires a throwaway channel the same way.
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.create_player_safe(7);
    rb.forget(); // bake the create in

    let before = crc(&rb);
    let key = *rb.data.player_entites.get(&7).unwrap();
    rb.new_transaction();
    rb.remove_entity_safe(key, Some(7));
    assert!(!rb.data.ecs.entities.contains_key(key), "entity gone");
    let after_remove = crc(&rb);
    assert_ne!(before, after_remove, "removal must change state");
    rb.rollback();
    assert_eq!(before, crc(&rb), "hash(before) == hash(after undo), bit-exact");
}

#[test]
fn remove_then_create_in_one_transaction_holds_the_hash_bar() {
    // Region-crossing repro: a single reconcile-replayed Tick can BOTH remove
    // an entity (expire_ghosts / extraction) AND create one (apply_ghost /
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

#[test]
fn set_body_pose_safe_moves_and_undoes_exactly() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.create_player_safe(3);
    rb.forget();

    let key = *rb.data.player_entites.get(&3).unwrap();
    let before = crc(&rb);
    rb.new_transaction();
    rb.set_body_pose_safe(key, pose(250.0, 26.0, 10.0));
    let handle = rb.data.ecs.rigidbody.try_get(key).unwrap();
    let t = rb.data.physics.bodies.get(handle).unwrap().translation();
    assert_eq!(t.x, 250.0);
    rb.rollback();
    assert_eq!(before, crc(&rb));
}

fn eid(src: RegionCoords, key: game::EntityKey) -> game::EntityId {
    use slotmapd::Key;
    game::EntityId { birth_region: src, serial: key.data().as_ffi() }
}

fn bundle(src: RegionCoords, key: game::EntityKey, client: game::ClientId, x: f32) -> EntityBundle {
    EntityBundle {
        id: eid(src, key),
        kind: EntityKind::Player,
        body: game::BodyState::kinematic(pose(x, 26.0, 128.0)),
        collider: ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        carried: game::Carried { camera: Some((0.0, 0.0)), motion: Some(game::Motion::default()), ..Default::default() },
        client: Some((client, Client::default())),
        pending_inputs: Vec::new(),
    }
}

fn ghost(src: RegionCoords, key: game::EntityKey, x: f32) -> GhostData {
    GhostData {
        id: eid(src, key),
        source_region: src,
        kind: EntityKind::Player,
        isometry: pose(x, 26.0, 128.0),
        linvel: Vector::ZERO,
        collider: ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
    }
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

#[test]
fn arrival_creates_player_with_client_state_and_holds_hash_bar() {
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    let before = crc(&rb);
    rb.new_transaction();
    rb.apply_arrival(bundle(src, src_key, 9, 2.0));
    assert!(rb.data.player_entites.contains_key(&9));
    assert!(rb.data.clients.contains_key(&9), "input state travels with the player");
    let e = *rb.data.player_entites.get(&9).unwrap();
    let handle = rb.data.ecs.rigidbody.try_get(e).unwrap();
    let t = rb.data.physics.bodies.get(handle).unwrap().translation();
    assert_eq!(t.x, 2.0, "arrives at the (rebased) bundle pose");
    rb.rollback();
    assert_eq!(before, crc(&rb));
}

#[test]
fn arrival_preserves_movement_state() {
    // A player flying (or falling) across a seam must keep flying (or keep
    // its fall speed) in the receiving region.
    let (mut donor_rb, src_key) = donor();
    let motion = game::Motion {
        vel_y: -12.5,
        grounded: false,
        flying: true,
        last_jump_tap: Some(41),
    };

    donor_rb.new_transaction();
    donor_rb.data.clients.insert(9, Client::default());
    donor_rb.data.ecs.motion.set_safe(src_key, Some(motion));
    donor_rb.forget();

    donor_rb.new_transaction();
    let bundle = donor_rb.extract_entity_safe(src_key);
    assert_eq!(bundle.carried.motion, Some(motion));

    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.apply_arrival(bundle);
    let e = *rb.data.player_entites.get(&9).unwrap();
    assert_eq!(*rb.data.ecs.motion.try_get(e), Some(motion));
}

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

#[test]
fn ghost_upsert_refresh_and_ttl_expiry_hold_hash_bar() {
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let (send, recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));

    // Create.
    rb.new_transaction();
    rb.apply_ghost(ghost(src, src_key, 250.0));
    rb.forget();
    assert_eq!(rb.data.ghosts.len(), 1);
    let entry = rb.data.ghosts.get(&eid(src, src_key)).unwrap().clone();

    // Refresh keeps the same entity and holds the bar.
    let before = crc(&rb);
    rb.new_transaction();
    rb.apply_ghost(ghost(src, src_key, 252.0));
    assert_eq!(
        rb.data.ghosts.get(&eid(src, src_key)).unwrap().entity,
        entry.entity,
        "refresh must not respawn the ghost"
    );
    rb.rollback();
    assert_eq!(before, crc(&rb));

    // Expiry: age the region past the TTL, then tick the reaper.
    rb.new_transaction();
    for _ in 0..(GHOST_TTL_TICKS + 1) {
        rb.data.tick.update(|t| *t += 1);
    }
    rb.forget();
    // Drain events accumulated from the create/refresh above (including the
    // creation-time SetGhostSource(Some) Do-emit) so the post-rollback drain
    // below only reflects expire_ghosts' own emits, not stale lookalikes.
    let _: Vec<_> = recv.try_iter().collect();
    let before = crc(&rb);
    rb.new_transaction();
    rb.expire_ghosts();
    assert!(rb.data.ghosts.get(&eid(src, src_key)).is_none(), "stale ghost reaped");
    assert!(!rb.data.ecs.entities.contains_key(entry.entity));
    rb.rollback();
    assert_eq!(before, crc(&rb));
    let events: Vec<_> = recv.try_iter().collect();
    assert!(
        events.iter().any(|u| matches!(
            u.update_kind,
            GameDataUpdateKind::SetGhostSource(e, Some(rc)) if e == entry.entity && rc == src
        )),
        "expire_ghosts rollback must restore the ghost mark via compensating emit_on_undo"
    );
}

#[test]
fn arrival_upgrades_ghost_in_place_keeping_entity_key() {
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let (send, recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.apply_ghost(ghost(src, src_key, 250.0));
    rb.forget();
    let ghost_entity = rb.data.ghosts.get(&eid(src, src_key)).unwrap().entity;
    // Drain the creation-time events (including the creation's own
    // SetGhostSource(Some) Do-emit) so the post-rollback drain below only
    // reflects the upgrade transaction's own emits, not a stale lookalike.
    let _: Vec<_> = recv.try_iter().collect();

    // Do NOT forget: the upgrade (ghost drop + SetGhostSource-clear emit + body
    // inject) must be exactly invertible, including the compensating undo emit
    // that restores the ghost mark.
    let before = crc(&rb);
    rb.new_transaction();
    rb.apply_arrival(bundle(src, src_key, 9, 2.0));
    let owned = *rb.data.player_entites.get(&9).unwrap();
    assert_eq!(owned, ghost_entity, "upgrade-in-place: same EntityKey continues");
    assert!(rb.data.ghosts.get(&eid(src, src_key)).is_none(), "ghost record dropped");
    assert!(rb.data.ecs.rigidbody.try_get(owned).is_some(), "body attached on upgrade");
    rb.rollback();
    assert_eq!(before, crc(&rb), "upgrade path holds the hash bar, bit-exact");
    let events: Vec<_> = recv.try_iter().collect();
    assert!(
        events.iter().any(|u| matches!(
            u.update_kind,
            GameDataUpdateKind::SetGhostSource(e, Some(rc)) if e == ghost_entity && rc == src
        )),
        "upgrade rollback must restore the ghost mark via compensating emit_on_undo"
    );
}

#[test]
fn stage2_ghost_has_a_collidable_body_that_tracks_updates() {
    // Live sender: this test rolls back, and rollback() replays tier-2 undo
    // closures through client.as_ref().unwrap() — a None sender panics.
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.apply_ghost(ghost(src, src_key, 250.0));
    rb.forget();
    let e = rb.data.ghosts.get(&eid(src, src_key)).unwrap().entity;
    let handle = rb.data.ecs.rigidbody.try_get(e).expect("stage 2: ghost has a body");
    assert!(
        rb.data.physics.bodies.get(handle).unwrap().colliders().len() == 1,
        "ghost carries its collider"
    );

    // Refresh moves the body (and holds the hash bar).
    let before = crc(&rb);
    rb.new_transaction();
    rb.apply_ghost(ghost(src, src_key, 240.0));
    let t = rb.data.physics.bodies.get(handle).unwrap().translation();
    assert_eq!(t.x, 240.0);
    rb.rollback();
    assert_eq!(before, crc(&rb));
}

#[test]
fn stage2_ghost_create_with_body_is_exactly_invertible() {
    // Task 9 gap: apply_ghost's CREATE path allocates a kinematic body (via
    // build_kinematic_body_safe -> revert_insert) plus a capsule collider
    // (via attach_capsule_collider_safe -> whole-PhysicsState snapshot). The
    // existing stage2 tests only ever roll back a REFRESH (the CREATE is
    // always `forget()`-ed first) or an arrival-upgrade (which reuses an
    // already-created ghost body). Nothing directly proves the ghost CREATE
    // itself — the body+collider allocation — is exactly invertible. This
    // test does, on a fresh Rollback with no ghost yet.
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));

    let before = crc(&rb);
    rb.new_transaction();
    rb.apply_ghost(ghost(src, src_key, 250.0));
    let e = rb.data.ghosts.get(&eid(src, src_key)).unwrap().entity;
    assert!(
        rb.data.ecs.rigidbody.try_get(e).is_some(),
        "ghost create must allocate a body (proves this exercises the bodied-create path)"
    );
    rb.rollback();
    assert_eq!(before, crc(&rb), "ghost create (body + collider alloc) holds the hash bar, bit-exact");
}

#[test]
fn stage2_upgrade_reuses_the_ghost_body() {
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.apply_ghost(ghost(src, src_key, 250.0));
    rb.forget();
    let e = rb.data.ghosts.get(&eid(src, src_key)).unwrap().entity;
    let ghost_handle = rb.data.ecs.rigidbody.try_get(e).unwrap();

    rb.new_transaction();
    rb.apply_arrival(bundle(src, src_key, 9, 245.0));
    rb.forget();
    let owned_handle = rb.data.ecs.rigidbody.try_get(e).unwrap();
    assert_eq!(ghost_handle, owned_handle, "upgrade corrects pose, keeps the body");
    let t = rb.data.physics.bodies.get(owned_handle).unwrap().translation();
    assert_eq!(t.x, 245.0);
}

#[test]
fn stage2_ghost_collider_blocks_a_walking_player() {
    // A player walking into a ghost must be collision-corrected (the same
    // KinematicCharacterController path terrain uses). Place a ghost dead
    // ahead of the player and drive input ticks forward.
    //
    // Note on tick sequencing (differs from the brief sketch): the KeyE
    // fps-cam toggle is applied AFTER the controllers run within a tick, so
    // the tick that flips fps-cam on produces no movement. To get three FULL
    // walk moves (and a genuinely discriminating test — unblocked z=104 <
    // 110, blocked z stays > 110), the toggle gets its own tick before W is
    // held. With both keys pressed before any tick (as the brief sketched)
    // only two moves land, so an uncollided walk would already stop at 112 —
    // a false green. Proven by the bite-check in the task report.
    use game::{GameEventKind, InputEvent, Key, Region};
    let (_, src_key) = donor();
    let id = RegionCoords::new(0, 0);
    let mut region = Region::from_chunks(id, Vec::new());
    region.handle_event(GameEventKind::CreateClient(1)).unwrap();
    region.forget_last_event();
    let player = *region.data().player_entites.get(&1).unwrap();

    // Ghost dead ahead of the player (player spawns at 128,26,128 facing -z;
    // W walks -z at 8 units/tick). Placed at z=104 so the combined capsule
    // radii (6.4 + 6.4 = 12.8) stop the walker near z=116.8.
    region
        .handle_event(GameEventKind::GhostUpdate(ghost(RegionCoords::new(0, -1), src_key, 0.0)))
        .unwrap();
    region.forget_last_event();
    // Reposition the ghost precisely via its entity.
    let ghost_e = region.data().ghosts.get(&eid(RegionCoords::new(0, -1), src_key)).unwrap().entity;
    region.with_data(|d| d.set_body_pose_safe(ghost_e, pose(128.0, 26.0, 104.0)));

    // fps-cam on: press KeyE, then tick once so the toggle takes effect
    // (this tick moves nothing — fps-cam is still off while controllers run).
    region
        .handle_event(GameEventKind::PlayerInput(1, InputEvent::Key { key: Key::KeyE, pressed: true }))
        .unwrap();
    region.forget_last_event();
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();

    // Now hold W and drive three full walk ticks.
    region
        .handle_event(GameEventKind::PlayerInput(1, InputEvent::Key { key: Key::KeyW, pressed: true }))
        .unwrap();
    region.forget_last_event();
    for _ in 0..3 {
        region.handle_event(GameEventKind::Tick).unwrap();
        region.forget_last_event();
    }
    let handle = region.data().ecs.rigidbody.try_get(player).unwrap();
    let z = region.data().physics.bodies.get(handle).unwrap().translation().z;
    // Unblocked: 128 - 3*8 = 104. The ghost capsule (radius 6.4) at z=104
    // must stop the player short of ~117.
    assert!(
        z > 110.0,
        "ghost collider must block the walk: z={} (unblocked would be 104)",
        z
    );
}

#[test]
fn replayed_arrival_is_a_pose_correction_not_a_duplicate() {
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.apply_arrival(bundle(src, src_key, 9, 2.0));
    rb.forget();
    let count = rb.data.ecs.entities.len();

    // Do NOT forget: the idempotent replay (pose correction, no duplicate)
    // must be exactly invertible.
    let before = crc(&rb);
    rb.new_transaction();
    rb.apply_arrival(bundle(src, src_key, 9, 5.0));
    assert_eq!(rb.data.ecs.entities.len(), count, "no duplicate entity");
    let e = *rb.data.player_entites.get(&9).unwrap();
    let handle = rb.data.ecs.rigidbody.try_get(e).unwrap();
    let t = rb.data.physics.bodies.get(handle).unwrap().translation();
    assert_eq!(t.x, 5.0, "replay corrected the pose");
    rb.rollback();
    assert_eq!(before, crc(&rb), "idempotent replay path holds the hash bar");
}

#[test]
fn entity_id_survives_crossing() {
    let (mut donor_rb, key) = donor();
    donor_rb.new_transaction();
    donor_rb.data.clients.insert(9, Client::default());
    donor_rb.forget();
    let id = donor_rb.entity_id(key).expect("a player has an id");
    assert_eq!(donor_rb.entity_by_id(id), Some(key));

    donor_rb.new_transaction();
    let bundle = donor_rb.extract_entity_safe(key);
    assert_eq!(bundle.id, id);
    assert_eq!(donor_rb.entity_by_id(id), None, "gone from the source");

    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    let e = rb.apply_arrival(bundle).expect("arrives");
    assert_eq!(rb.entity_id(e), Some(id), "keeps its id");
    assert_eq!(rb.entity_by_id(id), Some(e));
}

#[test]
fn predicted_and_authoritative_ids_agree() {
    let run = || {
        let (send, _recv) = crossbeam::channel::unbounded();
        let mut rb = Rollback::new(Some(send));
        rb.new_transaction();
        let slime = rb.spawn_slime_safe(
            game::rapier3d::math::Vector::new(100.0, 40.0, 100.0),
            2,
            0.0,
            0,
        );
        rb.create_player_safe(3);
        rb.forget();
        let player = *rb.data.player_entites.get(&3).unwrap();
        (rb.entity_id(slime).unwrap(), rb.entity_id(player).unwrap())
    };
    let (a, b) = (run(), run());
    assert_eq!(a, b, "same events, same ids");
    assert_ne!(a.0, a.1, "distinct entities, distinct ids");
}

#[test]
fn undo_spawn_frees_serial() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    let before = crc(&rb);
    rb.new_transaction();
    rb.spawn_slime_safe(game::rapier3d::math::Vector::new(100.0, 40.0, 100.0), 2, 0.0, 0);
    assert_eq!(*rb.data.next_entity_serial, 1);
    rb.rollback();
    assert_eq!(*rb.data.next_entity_serial, 0, "the serial is freed");
    assert_eq!(before, crc(&rb));
}

// ---- generated bundle: exhaustive classification (Task 2.2) ----

/// A donor player carrying every transferable component, plus a client with
/// region-local state to be reset.
fn loaded_donor() -> (Rollback, game::EntityKey, Client) {
    let (mut rb, key) = donor();
    let client = Client {
        fps_cam_mode: true,
        selected: 3,
        inventory_open: true,
        mining: Some(game::Mining {
            target: [1, 2, 3],
            started_tick: 40,
            needed_ticks: 9,
        }),
        break_cooldown_until: 77,
        open_container: Some([4, 5, 6]),
        ..Client::default()
    };
    rb.new_transaction();
    rb.data.clients.insert(9, client.clone());
    rb.data.ecs.motion.set_safe(
        key,
        Some(game::Motion { vel_y: -3.5, grounded: false, flying: true, last_jump_tap: Some(12) }),
    );
    rb.data.ecs.slime.set_safe(
        key,
        Some(game::slime::Slime { yaw: 0.5, target_yaw: 1.5, next_turn_tick: 90, next_hop_tick: 95, hopping: true }),
    );
    rb.data.ecs.walker.set_safe(
        key,
        Some(game::Walker { goal: Default::default(), next: 2, stalled: 4, ..Default::default() }),
    );
    rb.data.ecs.wander.set_safe(key, Some(game::people::Wander { next_tick: 123, home: [7, 8] }));
    let mut cam = rb.data.ecs.camera.try_get(key).clone().unwrap();
    cam.yaw = 0.75;
    cam.pitch = -0.25;
    rb.data.ecs.camera.set_safe(key, Some(cam));
    rb.forget();
    (rb, key, client)
}

#[test]
fn bundle_round_trip_is_state_exact() {
    let (mut donor_rb, key, client) = loaded_donor();
    donor_rb.new_transaction();
    // An animator needs a rig (and its bone colliders): attach the dwarf's.
    let animator = game::anim::Animator::idle(game::anim::RigId::default());
    donor_rb.attach_rig_safe(key, animator, 2.0);
    donor_rb.forget();

    donor_rb.new_transaction();
    let bundle = donor_rb.extract_entity_safe(key);
    let want = bundle.carried.clone();
    assert!(want.motion.is_some() && want.slime.is_some() && want.walker.is_some());
    assert!(want.animator.is_some() && want.wander.is_some() && want.camera.is_some());
    assert_eq!(bundle.client.as_ref().map(|(_, c)| c), Some(&client), "extraction copies the client as is");

    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    let e = rb.apply_arrival(bundle.clone()).expect("arrives");
    let handle = rb.data.ecs.rigidbody.try_get(e).unwrap();
    assert_eq!(game::BodyState::of(rb.data.physics.bodies.get(handle).unwrap()), bundle.body);
    assert_eq!(rb.data.clients[&9], client.arrive());
    assert_eq!(rb.entity_id(e), Some(bundle.id));
    // Every carried component is bit-equal: extracting again yields it.
    let again = rb.extract_entity_safe(e);
    assert_eq!(again.carried, want, "every carried component is bit-equal");
}

#[test]
fn replayed_arrival_overwrites_whole_bundle() {
    let (mut donor_rb, key, _) = loaded_donor();
    donor_rb.new_transaction();
    let bundle = donor_rb.extract_entity_safe(key);

    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    let e = rb.apply_arrival(bundle.clone()).expect("arrives");
    rb.data.ecs.motion.set_safe(e, Some(game::Motion { vel_y: 99.0, ..Default::default() }));
    rb.data.ecs.wander.set_safe(e, None);
    rb.data.clients.get_mut(&9).unwrap().selected = 8;
    assert!(rb.apply_arrival(bundle.clone()).is_none(), "a replay creates nothing");
    assert_eq!(*rb.data.ecs.motion.try_get(e), bundle.carried.motion);
    assert_eq!(*rb.data.ecs.wander.try_get(e), bundle.carried.wander);
    assert_eq!(rb.data.clients[&9].selected, 3, "the client is the bundle's too");
}

#[test]
fn arrival_closes_open_chest_view() {
    let (mut donor_rb, key, _) = loaded_donor();
    donor_rb.new_transaction();
    let bundle = donor_rb.extract_entity_safe(key);
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.apply_arrival(bundle);
    let c = &rb.data.clients[&9];
    assert_eq!(c.open_container, None);
    assert!(!c.inventory_open);
    assert_eq!((c.mining, c.break_cooldown_until), (None, 0));
}
