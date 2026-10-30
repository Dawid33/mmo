//! Sim-owned animation (`game::anim`): a player is a rigged dwarf with a clip
//! the sim picks from how it moves, whose bones carry colliders that other
//! bodies' sweeps hit, and whose animated ticks undo bit-exact.
mod common;

use common::floor_chunks;
use game::anim::{self, Animator, RigId, REST_CLIP};
use game::rapier3d::math::Vector;
use game::rapier3d::prelude::{ColliderHandle, QueryFilter, Ray};
use game::{state_hash, EntityKey, GameEventKind, InputEvent, Key, Region, RegionCoords, Rollback};

const CLIENT: game::ClientId = 7;

/// One region with a flat floor around the spawn and one player on it, with
/// a render channel (rollback needs one).
fn region_with_player() -> (Region, EntityKey, crossbeam::channel::Receiver<game::GameDataUpdate>) {
    let (send, recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), RegionCoords::new(0, 0), None);
    let (x, z) = region.data().spawn_xz();
    let edge = common::CHUNK_EDGE;
    region.with_data(|r| r.seed_terrain_genesis(floor_chunks(x - edge..x + edge, z - edge..z + edge, 16)));
    region.handle_event(GameEventKind::CreateClient(CLIENT)).unwrap();
    // FPS cam on, so held keys move the body.
    region
        .handle_event(GameEventKind::PlayerInput(CLIENT, InputEvent::FpsCam(true)))
        .unwrap();
    let e = *region.data().player_entites.get(&CLIENT).unwrap();
    // Settle onto the floor.
    for _ in 0..20 {
        tick(&mut region);
    }
    (region, e, recv)
}

fn tick(region: &mut Region) {
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
}

fn animator(region: &Region, e: EntityKey) -> Animator {
    region.data().ecs.animator.try_get(e).expect("players are rigged")
}

fn bone_colliders(region: &Region, e: EntityKey) -> Vec<ColliderHandle> {
    region.data().ecs.bones.try_get(e).clone().expect("players have bone colliders").colliders
}

/// Each bone collider's pose relative to the body.
fn bone_poses(region: &Region, e: EntityKey) -> Vec<game::Pose> {
    let data = region.data();
    bone_colliders(region, e)
        .iter()
        .map(|h| *data.physics.colliders.get(*h).unwrap().position_wrt_parent().unwrap())
        .collect()
}

#[test]
fn dwarf_rig_has_gait_clips_and_hitboxes() {
    let rig = anim::rig(RigId::Dwarf);
    for (name, clip) in [("idle", rig.idle), ("walk", rig.walk), ("run", rig.run)] {
        assert_ne!(clip, REST_CLIP, "the dwarf has a {name} clip");
    }
    assert!(rig.hitboxes.len() >= 6, "head, torso, arms and legs collide: {:?}", rig.hitboxes);
    for h in &rig.hitboxes {
        assert!(h.half_extents.iter().all(|x| *x > 0.0), "{h:?}");
    }
}

#[test]
fn sampling_is_pure_and_walk_moves_bones() {
    let rig = anim::rig(RigId::Dwarf);
    let mut a = Animator::idle(RigId::Dwarf);
    a.play(rig.walk, 1.0);
    a.advance(1.0); // fade done, 1 s into the walk
    assert_eq!(anim::sample(&a, 0.0), anim::sample(&a, 0.0), "same state, same pose");
    let now = anim::model_space(rig, &anim::sample(&a, 0.0));
    let later = anim::model_space(rig, &anim::sample(&a, 0.2));
    assert_ne!(now, later, "a walk moves bones over 0.2 s");
}

#[test]
fn player_is_rigged_with_one_collider_per_bone() {
    let (region, e, _recv) = region_with_player();
    let data = region.data();
    let body = data.physics.bodies.get(*data.ecs.rigidbody.get(e)).unwrap();
    let rig = anim::rig(RigId::Dwarf);
    assert_eq!(body.colliders().len(), 1 + rig.hitboxes.len(), "capsule + one per hitbox");
    assert_eq!(bone_colliders(&region, e).len(), rig.hitboxes.len());
    // The capsule stays first: movement sweeps the body's first collider.
    let first = data.physics.colliders.get(body.colliders()[0]).unwrap();
    assert!(first.shape().as_capsule().is_some());
    assert_eq!(animator(&region, e).current.clip, rig.idle, "standing still idles");
}

#[test]
fn walking_plays_a_gait_and_bone_colliders_follow_it() {
    let (mut region, e, _recv) = region_with_player();
    let rig = anim::rig(RigId::Dwarf);
    region
        .handle_event(GameEventKind::PlayerInput(CLIENT, InputEvent::Key { key: Key::KeyW, pressed: true }))
        .unwrap();
    for _ in 0..10 {
        tick(&mut region);
    }
    let a = animator(&region, e);
    assert!(a.current.clip == rig.walk || a.current.clip == rig.run, "moving plays a gait: {a:?}");
    assert!(a.current.speed > 1.0, "player walk speed outpaces the dwarf's clip: {a:?}");
    let before = bone_poses(&region, e);
    tick(&mut region);
    let after = bone_poses(&region, e);
    let moved = before.iter().zip(&after).filter(|(b, a)| b != a).count();
    assert!(moved >= 2, "limbs swing between ticks ({moved} bone colliders moved)");
}

/// Another body's ray (what a character sweep or an edit ray sees) hits the
/// dwarf's arm, which sticks out past the movement capsule.
#[test]
fn bone_colliders_block_other_queries_beyond_the_capsule() {
    let (region, e, _recv) = region_with_player();
    let data = region.data();
    let body = data.physics.bodies.get(*data.ecs.rigidbody.get(e)).unwrap();
    let t = body.translation();
    let bones = bone_colliders(&region, e);
    let queries = data.physics.broad_phase.as_query_pipeline(
        data.physics.narrow_phase.query_dispatcher(),
        &data.physics.bodies,
        &data.physics.colliders,
        QueryFilter::default(),
    );
    // Sweep a horizontal ray across the body at every height from feet to
    // head: some height must hit a bone collider before the capsule.
    let radius = game::movement::PLAYER_CAPSULE_RADIUS;
    let bottom = t.y - game::movement::PLAYER_CAPSULE_HALF_HEIGHT - radius;
    let mut hit_bone_outside_capsule = false;
    for step in 0..28 {
        let origin = Vector::new(t.x - 64.0, bottom + step as f32 + 0.5, t.z);
        let ray = Ray::new(origin, Vector::new(1.0, 0.0, 0.0));
        if let Some((handle, toi)) = queries.cast_ray(&ray, 128.0, true) {
            let x = origin.x + toi;
            if bones.contains(&handle) && x < t.x - radius {
                hit_bone_outside_capsule = true;
            }
        }
    }
    assert!(hit_bone_outside_capsule, "some bone (an arm) blocks a ray before the capsule would");
}

#[test]
fn animated_ticks_undo_bit_exact_and_replay() {
    let (mut region, _e, _recv) = region_with_player();
    region
        .handle_event(GameEventKind::PlayerInput(CLIENT, InputEvent::Key { key: Key::KeyW, pressed: true }))
        .unwrap();
    region.forget_last_event();
    for n in 0..30 {
        let before = state_hash(region.data());
        region.handle_event(GameEventKind::Tick).unwrap();
        let after = state_hash(region.data());
        region.rollback_last_event();
        assert_eq!(before, state_hash(region.data()), "animated tick {n} must undo bit-exact");
        region.handle_event(GameEventKind::Tick).unwrap();
        assert_eq!(after, state_hash(region.data()), "animated tick {n} must replay identically");
        region.forget_last_event();
    }
}
