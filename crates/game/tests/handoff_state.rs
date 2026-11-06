use game::{Pose, Rollback};
use game::{
    Client, ColliderSpec, EntityBundle, EntityKind, HaloEntityDelta, HaloEntityGone, RegionCoords,
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

fn ghost(src: RegionCoords, key: game::EntityKey, x: f32) -> HaloEntityDelta {
    HaloEntityDelta {
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
fn halo_entity_upsert_refresh_and_gone_hold_hash_bar() {
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let (send, recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));

    // Create.
    rb.new_transaction();
    rb.apply_halo_entity_safe(ghost(src, src_key, 250.0));
    rb.forget();
    assert_eq!(rb.data.halo_entities.len(), 1);
    let entry = rb.data.halo_entities.get(&eid(src, src_key)).unwrap().clone();

    // Refresh keeps the same entity and holds the bar.
    let before = crc(&rb);
    rb.new_transaction();
    rb.apply_halo_entity_safe(ghost(src, src_key, 252.0));
    assert_eq!(
        rb.data.halo_entities.get(&eid(src, src_key)).unwrap().entity,
        entry.entity,
        "refresh must not respawn the ghost"
    );
    rb.rollback();
    assert_eq!(before, crc(&rb));

    // Drain events accumulated from the create/refresh above (including the
    // creation-time SetHaloSource(Some) Do-emit) so the post-rollback drain
    // below only reflects the end's own emits, not stale lookalikes.
    let _: Vec<_> = recv.try_iter().collect();
    // An end from another owner leaves the mirror alone.
    let before = crc(&rb);
    rb.new_transaction();
    rb.apply_halo_entity_gone_safe(HaloEntityGone { id: eid(src, src_key), from: RegionCoords::new(7, 7) });
    assert!(rb.data.halo_entities.get(&eid(src, src_key)).is_some(), "not that owner's mirror");
    rb.forget();
    assert_eq!(before, crc(&rb));
    // The owner's end removes it.
    rb.new_transaction();
    rb.apply_halo_entity_gone_safe(HaloEntityGone { id: eid(src, src_key), from: src });
    assert!(rb.data.halo_entities.get(&eid(src, src_key)).is_none(), "mirror ended");
    assert!(!rb.data.ecs.entities.contains_key(entry.entity));
    rb.rollback();
    assert_eq!(before, crc(&rb));
    let events: Vec<_> = recv.try_iter().collect();
    assert!(
        events.iter().any(|u| matches!(
            u.update_kind,
            GameDataUpdateKind::SetHaloSource(e, Some((rc, _))) if e == entry.entity && rc == src
        )),
        "the end's rollback must restore the mirror mark via compensating emit_on_undo"
    );
}

#[test]
fn arrival_upgrades_ghost_in_place_keeping_entity_key() {
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let (send, recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.apply_halo_entity_safe(ghost(src, src_key, 250.0));
    rb.forget();
    let ghost_entity = rb.data.halo_entities.get(&eid(src, src_key)).unwrap().entity;
    // Drain the creation-time events (including the creation's own
    // SetHaloSource(Some) Do-emit) so the post-rollback drain below only
    // reflects the upgrade transaction's own emits, not a stale lookalike.
    let _: Vec<_> = recv.try_iter().collect();

    // Do NOT forget: the upgrade (mirror drop + SetHaloSource-clear emit + body
    // inject) must be exactly invertible, including the compensating undo emit
    // that restores the ghost mark.
    let before = crc(&rb);
    rb.new_transaction();
    rb.apply_arrival(bundle(src, src_key, 9, 2.0));
    let owned = *rb.data.player_entites.get(&9).unwrap();
    assert_eq!(owned, ghost_entity, "upgrade-in-place: same EntityKey continues");
    assert!(rb.data.halo_entities.get(&eid(src, src_key)).is_none(), "ghost record dropped");
    assert!(rb.data.ecs.rigidbody.try_get(owned).is_some(), "body attached on upgrade");
    rb.rollback();
    assert_eq!(before, crc(&rb), "upgrade path holds the hash bar, bit-exact");
    let events: Vec<_> = recv.try_iter().collect();
    assert!(
        events.iter().any(|u| matches!(
            u.update_kind,
            GameDataUpdateKind::SetHaloSource(e, Some((rc, _))) if e == ghost_entity && rc == src
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
    rb.apply_halo_entity_safe(ghost(src, src_key, 250.0));
    rb.forget();
    let e = rb.data.halo_entities.get(&eid(src, src_key)).unwrap().entity;
    let handle = rb.data.ecs.rigidbody.try_get(e).expect("stage 2: ghost has a body");
    assert!(
        rb.data.physics.bodies.get(handle).unwrap().colliders().len() == 1,
        "ghost carries its collider"
    );

    // Refresh drives the kinematic body toward the new pose — reached at
    // the next step (Task 7.2: driven, not teleported) — and holds the hash
    // bar.
    let before = crc(&rb);
    rb.new_transaction();
    rb.apply_halo_entity_safe(ghost(src, src_key, 240.0));
    let t = rb.data.physics.bodies.get(handle).unwrap().next_position().translation;
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
    rb.apply_halo_entity_safe(ghost(src, src_key, 250.0));
    let e = rb.data.halo_entities.get(&eid(src, src_key)).unwrap().entity;
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
    rb.apply_halo_entity_safe(ghost(src, src_key, 250.0));
    rb.forget();
    let e = rb.data.halo_entities.get(&eid(src, src_key)).unwrap().entity;
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
        .handle_event(GameEventKind::HaloEntity(ghost(RegionCoords::new(0, -1), src_key, 0.0)))
        .unwrap();
    region.forget_last_event();
    // Reposition the ghost precisely via its entity.
    let ghost_e = region.data().halo_entities.get(&eid(RegionCoords::new(0, -1), src_key)).unwrap().entity;
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

/// An entity born here (so never in `arrivals`) whose arrival comes in
/// again — a region restored from a file that still holds it, then sent the
/// in-flight copy — is corrected, not duplicated.
#[test]
fn arrival_of_an_entity_born_here_is_not_a_duplicate() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    let key = rb.spawn_slime_safe(Vector::new(100.0, 200.0, 100.0), 2, 0.0, 0);
    rb.forget();
    let id = rb.entity_id(key).expect("a slime has an EntityId");
    let mut other = rb.clone();
    other.new_transaction();
    let mut bundle = other.extract_entity_safe(key);
    other.forget();
    bundle.body.pose.translation.x = 150.0;
    let count = rb.data.ecs.entities.len();

    let before = crc(&rb);
    rb.new_transaction();
    rb.apply_arrival(bundle);
    assert_eq!(rb.data.ecs.entities.len(), count, "no duplicate entity");
    assert_eq!(rb.entity_by_id(id), Some(key), "the entity born here is the one corrected");
    let handle = rb.data.ecs.rigidbody.try_get(key).unwrap();
    assert_eq!(rb.data.physics.bodies.get(handle).unwrap().translation().x, 150.0, "pose corrected");
    rb.rollback();
    assert_eq!(before, crc(&rb), "the correction holds the hash bar");
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
        open_container: Some(game::WorldBlockPos::new(4, 5, 6)),
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

/// Task 6.4: the open chest is a world cell, so it travels — the new
/// region closes it only when out of reach or unseen. Mining and the break
/// cooldown are the old region's and reset.
#[test]
fn arrival_keeps_open_chest_view() {
    let (mut donor_rb, key, _) = loaded_donor();
    donor_rb.new_transaction();
    let bundle = donor_rb.extract_entity_safe(key);
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.apply_arrival(bundle);
    let c = &rb.data.clients[&9];
    assert_eq!(c.open_container, Some(game::WorldBlockPos::new(4, 5, 6)));
    assert!(c.inventory_open);
    assert_eq!((c.mining, c.break_cooldown_until), (None, 0));
}

/// A person's arrival into a region where that person already has a body
/// is ignored — and takes the arriving entity's mirror with it, so no
/// mirror outlives the crossing that skipped the upgrade.
#[test]
fn ignored_duplicate_person_arrival_drops_its_mirror() {
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    let person = game::history::PersonId(7);
    rb.new_transaction();
    rb.spawn_person_safe(person, (128.0, 128.0)).expect("embodied here");
    let mut mirror = ghost(src, src_key, 250.0);
    mirror.kind = EntityKind::Person { person };
    rb.apply_halo_entity_safe(mirror);
    rb.forget();
    assert!(rb.halo_entity(eid(src, src_key)).is_some());
    let mut arrival = bundle(src, src_key, 9, 2.0);
    arrival.kind = EntityKind::Person { person };
    arrival.client = None;
    let before = crc(&rb);
    rb.new_transaction();
    assert_eq!(rb.apply_arrival(arrival), None, "the duplicate is ignored");
    assert!(rb.halo_entity(eid(src, src_key)).is_none(), "its mirror went with it");
    rb.rollback();
    assert_eq!(before, crc(&rb), "and undoes bit-exact");
}

// ---- dynamic bodies: EntityKind::Body (Task 7.2) ----

use crate::body::{cube, floored_region, live_body, spawn, tick};
use game::rapier3d::dynamics::{RigidBody, RigidBodyActivation};
use game::{BodyKind, BodyState, EntityId, GameEventKind, Region};

/// The neighbour the test bodies come from.
fn body_src() -> RegionCoords {
    RegionCoords::new(0, -1)
}

fn body_id(serial: u64) -> EntityId {
    EntityId { birth_region: body_src(), serial }
}

fn vbits(v: Vector) -> [u32; 3] {
    [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]
}

fn mbits(mp: &game::parry::mass_properties::MassProperties) -> [u32; 11] {
    let (c, i, q) = (mp.local_com, mp.inv_principal_inertia, mp.principal_inertia_local_frame);
    [
        c.x.to_bits(),
        c.y.to_bits(),
        c.z.to_bits(),
        mp.inv_mass.to_bits(),
        i.x.to_bits(),
        i.y.to_bits(),
        i.z.to_bits(),
        q.x.to_bits(),
        q.y.to_bits(),
        q.z.to_bits(),
        q.w.to_bits(),
    ]
}

/// A tumbling, floating (no gravity force) 1 m cube at region-local x.
fn body_bundle(serial: u64, x: f32) -> EntityBundle {
    let mass = game::mass_props::mass_properties(&cube(), game::mass_props::BODY_DEFAULT_DENSITY);
    EntityBundle {
        id: body_id(serial),
        kind: EntityKind::Body,
        body: BodyState {
            kind: BodyKind::Dynamic,
            linvel: Vector::new(3.25, -1.5, 0.75),
            angvel: Vector::new(0.5, -0.25, 1.0),
            activation: RigidBodyActivation::active(),
            angular_damping: 0.0,
            mass: Some(mass),
            ..BodyState::kinematic(pose(x, 300.0, 128.0))
        },
        collider: cube(),
        carried: game::Carried::default(),
        client: None,
        pending_inputs: Vec::new(),
    }
}

/// The source's halo mirror of [`body_bundle`]'s body.
fn body_ghost(serial: u64, x: f32) -> HaloEntityDelta {
    HaloEntityDelta {
        id: body_id(serial),
        source_region: body_src(),
        kind: EntityKind::Body,
        isometry: pose(x, 300.0, 128.0),
        linvel: Vector::ZERO,
        collider: cube(),
    }
}

fn bare_region() -> Region {
    crate::common::install_real_block_props();
    let (send, _recv) = crossbeam::channel::unbounded();
    Region::new(Rollback::new(Some(send.clone())), Some(send), RegionCoords::new(0, 0), None)
}

fn arrive(region: &mut Region, bundle: EntityBundle) -> Option<game::EntityKey> {
    let mut e = None;
    region.with_data(|r| e = r.apply_arrival(bundle));
    e
}

fn owned(region: &Region, id: EntityId) -> game::EntityKey {
    *region.data().id_index.get(&id).expect("owned here")
}

/// The arrived body carries the bundle's velocities and mass exactly.
fn assert_carries(body: &RigidBody, bundle: &EntityBundle) {
    assert!(body.is_dynamic(), "a dynamic body");
    assert_eq!(vbits(body.linvel()), vbits(bundle.body.linvel), "linvel");
    assert_eq!(vbits(body.angvel()), vbits(bundle.body.angvel), "angvel");
    let carried = BodyState::of(body).mass.expect("explicit mass");
    assert_eq!(mbits(&carried), mbits(bundle.body.mass.as_ref().unwrap()), "explicit mass");
}

/// After one step: the total mass is exactly the carried mass.
fn assert_stepped_mass(body: &RigidBody, bundle: &EntityBundle) {
    assert_eq!(mbits(&body.mass_properties().local_mprops), mbits(bundle.body.mass.as_ref().unwrap()), "local_mprops");
}

/// The same body after the same step, wherever it came from.
fn assert_same_motion(a: &RigidBody, b: &RigidBody) {
    assert_eq!(vbits(a.translation()), vbits(b.translation()), "translation");
    let (qa, qb) = (a.rotation(), b.rotation());
    assert_eq!([qa.x, qa.y, qa.z, qa.w].map(f32::to_bits), [qb.x, qb.y, qb.z, qb.w].map(f32::to_bits), "rotation");
    assert_eq!(vbits(a.linvel()), vbits(b.linvel()), "linvel");
    assert_eq!(vbits(a.angvel()), vbits(b.angvel()), "angvel");
}

#[test]
fn dynamic_arrival_without_ghost() {
    let bundle = body_bundle(1, 2.0);
    let mut region = bare_region();
    let e = arrive(&mut region, bundle.clone()).expect("a fresh arrival");
    assert_eq!(*region.data().ecs.kind.try_get(e), Some(EntityKind::Body));
    assert_carries(live_body(&region, e), &bundle);
    tick(&mut region);
    assert_stepped_mass(live_body(&region, e), &bundle);
}

#[test]
fn dynamic_arrival_upgrades_ghost_to_dynamic() {
    let bundle = body_bundle(1, 2.0);
    let mut region = bare_region();
    region.handle_event(GameEventKind::HaloEntity(body_ghost(1, 2.0))).unwrap();
    region.forget_last_event();
    let mirror = region.data().halo_entities.get(&body_id(1)).unwrap().entity;
    assert!(live_body(&region, mirror).is_kinematic(), "the mirror is kinematic");

    let e = arrive(&mut region, bundle.clone()).expect("an upgrade");
    assert_eq!(e, mirror, "the same EntityKey continues");
    assert!(region.data().halo_entities.get(&body_id(1)).is_none(), "the mirror record is gone");
    assert_carries(live_body(&region, e), &bundle);
    assert_eq!(live_body(&region, e).colliders().len(), 1, "one collider, no leftover of the mirror's");
    tick(&mut region);
    assert_stepped_mass(live_body(&region, e), &bundle);

    // Bit-equal to the same body arriving with no mirror.
    let mut fresh = bare_region();
    let f = arrive(&mut fresh, bundle.clone()).unwrap();
    tick(&mut fresh);
    assert_same_motion(live_body(&region, e), live_body(&fresh, f));
}

#[test]
fn undo_dynamic_upgrade_restores_ghost() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.apply_halo_entity_safe(body_ghost(1, 2.0));
    rb.forget();
    let mirror = rb.data.halo_entities.get(&body_id(1)).unwrap().clone();
    let before = crc(&rb);

    rb.new_transaction();
    let e = rb.apply_arrival(body_bundle(1, 2.0)).unwrap();
    assert_eq!(e, mirror.entity);
    rb.rollback();
    assert_eq!(before, crc(&rb), "the upgrade rolls back bit-exact");
    assert_eq!(rb.data.halo_entities.get(&body_id(1)), Some(&mirror), "the mirror record is back");
    let h = rb.data.ecs.rigidbody.try_get(mirror.entity).expect("its body is back");
    assert!(rb.data.physics.bodies.get(h).unwrap().is_kinematic(), "kinematic again");
}

/// Task 7.1 review: a replayed dynamic arrival (the same bundle applied
/// twice) steps exactly like a single one.
#[test]
fn replayed_dynamic_arrival_is_bit_exact() {
    let bundle = body_bundle(1, 2.0);
    let mut once = bare_region();
    let a = arrive(&mut once, bundle.clone()).unwrap();
    let mut twice = bare_region();
    let b = arrive(&mut twice, bundle.clone()).unwrap();
    assert_eq!(arrive(&mut twice, bundle.clone()), None, "a replay creates nothing");
    assert_carries(live_body(&twice, b), &bundle);
    tick(&mut once);
    tick(&mut twice);
    assert_stepped_mass(live_body(&twice, b), &bundle);
    assert_same_motion(live_body(&once, a), live_body(&twice, b));
    assert_eq!(game::state_hash(once.data()), game::state_hash(twice.data()), "the whole state agrees");
}

#[test]
fn sleeping_body_arrives_asleep() {
    let mut a = floored_region();
    let rest = game::BLOCK_VOXELS as f32 * 1.5;
    let e = spawn(&mut a, pose(512.0, rest, 512.0), cube(), Vector::ZERO, Vector::ZERO);
    for _ in 0..game::body::BODY_SETTLE_TICKS {
        tick(&mut a);
        if live_body(&a, e).is_sleeping() {
            break;
        }
    }
    assert!(live_body(&a, e).is_sleeping(), "settled asleep in the source");
    let mut bundle = None;
    a.with_data(|r| bundle = Some(r.extract_entity_safe(e)));
    let bundle = bundle.unwrap();
    assert!(bundle.body.activation.sleeping);

    let mut b = floored_region();
    let f = arrive(&mut b, bundle.clone()).unwrap();
    assert!(live_body(&b, f).is_sleeping(), "arrives asleep");
    assert_eq!(*live_body(&b, f).activation(), bundle.body.activation, "activation carried");
    // Rapier wakes a sleeping body when a contact of it STARTS, and every
    // contact of a body just inserted here starts on its first step: it
    // wakes once, finds itself at rest, and goes back to sleep where it lay.
    let at = live_body(&b, f).translation();
    for _ in 0..game::body::BODY_SETTLE_TICKS {
        tick(&mut b);
        if live_body(&b, f).is_sleeping() {
            break;
        }
    }
    assert!(live_body(&b, f).is_sleeping(), "asleep again");
    let moved = (live_body(&b, f).translation() - at).length();
    assert!(moved < 0.01 * game::BLOCK_VOXELS as f32, "where it lay: moved {moved}");
}

/// A halo mirror moved toward a resting body pushes it (one way: the
/// mirror is kinematic, and nothing about it goes back to its owner).
#[test]
fn ghost_pushes_dynamic_body_one_way() {
    let mut region = floored_region();
    let b = game::BLOCK_VOXELS as f32;
    let e = spawn(&mut region, pose(512.0, b * 1.5, 512.0), cube(), Vector::ZERO, Vector::ZERO);
    for _ in 0..10 {
        tick(&mut region);
    }
    let start = live_body(&region, e).translation();
    // A slime-sized capsule mirror standing on the floor, 3 blocks to -x,
    // walking +x at 2 units/tick: it reaches the cube in ~20 ticks.
    let capsule = ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 };
    let y = b + 8.0 + 6.4;
    let mut x = 512.0 - 3.0 * b;
    let mirror = |x: f32| HaloEntityDelta {
        id: body_id(5),
        source_region: body_src(),
        kind: EntityKind::Slime { size: 2 },
        isometry: pose(x, y, 512.0),
        linvel: Vector::ZERO,
        collider: capsule.clone(),
    };
    for _ in 0..40 {
        region.handle_event(GameEventKind::HaloEntity(mirror(x))).unwrap();
        region.forget_last_event();
        tick(&mut region);
        let (departures, halo) = region.take_transfers();
        assert!(departures.is_empty() && halo.is_empty(), "nothing goes back to the owner");
        x += 2.0;
    }
    let end = live_body(&region, e).translation();
    assert!(end.x - start.x > 1.0, "pushed along +x: {start:?} -> {end:?}");
    // Driven (not teleported), the mirror has a velocity, and the contact
    // hands it on: the cube is carried at the mirror's speed (a teleported
    // mirror only shoves it out of overlap, at ~2 units/s).
    let speed = 2.0 / game::movement::TICK_DT;
    let vx = live_body(&region, e).linvel().x;
    assert!(vx > 0.8 * speed, "carried at the mirror's {speed} units/s: {vx}");
    assert!(region.data().id_index.get(&body_id(5)).is_none(), "the mirror stays a mirror");
}

/// A mirror whose pose JUMPS (past `MIRROR_SNAP_DISTANCE_UNITS`: an in-band
/// teleport, or updates of several missed ticks landing at once) is
/// teleported, not driven: a mirror pushing a body that then jumps through it
/// to 10 blocks past leaves it be. Driven across the jump in one step, it
/// would hit the body at the jump's speed (~200 m/s) and fling it.
#[test]
fn ghost_jump_snaps_without_flinging() {
    use game::halo::MIRROR_SNAP_DISTANCE_UNITS;
    let mut region = floored_region();
    let b = game::BLOCK_VOXELS as f32;
    let e = spawn(&mut region, pose(512.0, b * 1.5, 512.0), cube(), Vector::ZERO, Vector::ZERO);
    for _ in 0..10 {
        tick(&mut region);
    }
    let capsule = ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 };
    let y = b + 8.0 + 6.4;
    let mirror = |x: f32| HaloEntityDelta {
        id: body_id(5),
        source_region: body_src(),
        kind: EntityKind::Slime { size: 2 },
        isometry: pose(x, y, 512.0),
        linvel: Vector::ZERO,
        collider: capsule.clone(),
    };
    let mut step = |region: &mut Region, x: f32| {
        region.handle_event(GameEventKind::HaloEntity(mirror(x))).unwrap();
        region.forget_last_event();
        tick(region);
    };
    // Walk up to the cube and push it (as `ghost_pushes_dynamic_body_one_way`).
    let mut x = 512.0 - 3.0 * b;
    for _ in 0..30 {
        step(&mut region, x);
        x += 2.0;
    }
    let before = live_body(&region, e).translation();
    assert!(before.x > 512.0 + 1.0, "the walk pushed the cube: {before:?}");
    // One update puts the mirror 10 blocks past the cube's far side.
    let past = before.x + 10.0 * b;
    assert!(past - x > MIRROR_SNAP_DISTANCE_UNITS, "the test's jump is past the snap distance");
    step(&mut region, past);
    for _ in 0..5 {
        tick(&mut region);
    }
    let body = live_body(&region, e);
    let moved = body.translation() - before;
    // Teleported, the cube keeps only the walk's push (2 units/tick), which
    // friction ends within a few ticks.
    assert!(moved.length() < b, "not flung by the mirror's jump: moved {moved:?}, linvel {:?}", body.linvel());
}


