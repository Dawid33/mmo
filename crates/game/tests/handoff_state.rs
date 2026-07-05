use game::{IsometryReal, Rollback};
use game::na::{Quaternion, Translation3, Unit};
use game::parry::math::Real;
use game::{
    Client, ColliderSpec, EntityBundle, EntityKind, GhostData, RegionCoords, GHOST_TTL_TICKS,
};
use game::parry::math::Vector;
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
    let (send, _recv) = crossbeam::channel::unbounded();
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
    let before = crc(&rb);
    rb.new_transaction();
    rb.expire_ghosts();
    assert!(rb.data.ghosts.get(&(src, src_key)).is_none(), "stale ghost reaped");
    assert!(!rb.data.ecs.entities.contains_key(entry.entity));
    rb.rollback();
    assert_eq!(before, crc(&rb));
}

#[test]
fn arrival_upgrades_ghost_in_place_keeping_entity_key() {
    let (_, src_key) = donor();
    let src = RegionCoords::new(0, 0);
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    rb.new_transaction();
    rb.apply_ghost(ghost(src, src_key, 250.0));
    rb.forget();
    let ghost_entity = rb.data.ghosts.get(&(src, src_key)).unwrap().entity;

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
