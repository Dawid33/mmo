use game::{IsometryReal, Rollback};
use game::na::{Quaternion, Translation3, Unit};
use game::parry::math::Real;
use game::{
    Client, ColliderSpec, EntityBundle, EntityKind, GhostData, RegionCoords, GHOST_TTL_TICKS,
};
use game::parry::math::Vector;
use game::GameDataUpdateKind;
use std::hash::Hash;

fn crc(rb: &Rollback) -> u32 {
    let mut h = crc32fast::Hasher::new();
    rb.data.hash(&mut h);
    h.finalize()
}

fn pose(x: f32, y: f32, z: f32) -> IsometryReal {
    IsometryReal::from_parts(
        Translation3::new(Real::from(x), Real::from(y), Real::from(z)),
        Unit::<Quaternion<Real>>::identity(),
    )
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
    assert_eq!(t.x, Real::from(250.0));
    rb.rollback();
    assert_eq!(before, crc(&rb));
}

fn bundle(src: RegionCoords, key: game::EntityKey, client: game::ClientId, x: f32) -> EntityBundle {
    EntityBundle {
        kind: EntityKind::Player,
        isometry: pose(x, 26.0, 128.0),
        linvel: Vector::zeros(),
        collider: ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.4 },
        has_camera: true,
        client: Some((client, Client::default())),
        source_region: src,
        source_key: key,
    }
}

fn ghost(src: RegionCoords, key: game::EntityKey, x: f32) -> GhostData {
    GhostData {
        source_region: src,
        source_key: key,
        kind: EntityKind::Player,
        isometry: pose(x, 26.0, 128.0),
        linvel: Vector::zeros(),
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
    assert_eq!(t.x, Real::from(2.0), "arrives at the (rebased) bundle pose");
    rb.rollback();
    assert_eq!(before, crc(&rb));
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
    let entry = rb.data.ghosts.get(&(src, src_key)).unwrap().clone();

    // Refresh keeps the same entity and holds the bar.
    let before = crc(&rb);
    rb.new_transaction();
    rb.apply_ghost(ghost(src, src_key, 252.0));
    assert_eq!(
        rb.data.ghosts.get(&(src, src_key)).unwrap().entity,
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
    assert!(rb.data.ghosts.get(&(src, src_key)).is_none(), "stale ghost reaped");
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
    let ghost_entity = rb.data.ghosts.get(&(src, src_key)).unwrap().entity;
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
    assert!(rb.data.ghosts.get(&(src, src_key)).is_none(), "ghost record dropped");
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
    let e = rb.data.ghosts.get(&(src, src_key)).unwrap().entity;
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
    assert_eq!(t.x, Real::from(240.0));
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
    let e = rb.data.ghosts.get(&(src, src_key)).unwrap().entity;
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
    let e = rb.data.ghosts.get(&(src, src_key)).unwrap().entity;
    let ghost_handle = rb.data.ecs.rigidbody.try_get(e).unwrap();

    rb.new_transaction();
    rb.apply_arrival(bundle(src, src_key, 9, 245.0));
    rb.forget();
    let owned_handle = rb.data.ecs.rigidbody.try_get(e).unwrap();
    assert_eq!(ghost_handle, owned_handle, "upgrade corrects pose, keeps the body");
    let t = rb.data.physics.bodies.get(owned_handle).unwrap().translation();
    assert_eq!(t.x, Real::from(245.0));
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
    let ghost_e = region.data().ghosts.get(&(RegionCoords::new(0, -1), src_key)).unwrap().entity;
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
        z.0 > 110.0,
        "ghost collider must block the walk: z={} (unblocked would be 104)",
        z.0
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
    assert_eq!(t.x, Real::from(5.0), "replay corrected the pose");
    rb.rollback();
    assert_eq!(before, crc(&rb), "idempotent replay path holds the hash bar");
}
