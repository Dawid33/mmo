//! The character controller on REAL generated terrain — half-block steps,
//! whole-block ledges and slab/block seams, which the flat-floor tests in
//! `game` never see: a
//! spawned player must land, walk and jump across it without tunneling, and
//! every one of those ticks must roll back bit-exact.
use std::hash::Hash;

use game::{GameEventKind, InputEvent, Key, Region, RegionCoords};

fn mats() -> worldgen::material::Materials {
    worldgen::material::Materials {
        stone: game::BlockId(2),
        dirt: game::BlockId(1),
        grass: game::BlockId(3),
        sand: game::BlockId(4),
        water: game::BlockId(5),
    }
}

/// Capsule half-extent (half-height 4.4 + radius 6.4): body origin → feet.
const FEET: f32 = 10.8;

fn crc(region: &Region) -> u32 {
    let mut h = crc32fast::Hasher::new();
    region.data().hash(&mut h);
    h.finalize()
}

fn pose(region: &Region) -> ([f32; 3], game::Motion) {
    let data = region.data();
    let e = *data.player_entites.get(&0).unwrap();
    let handle = *data.ecs.rigidbody.get(e);
    let t = data.physics.bodies.get(handle).unwrap().translation();
    ([t.x.0, t.y.0, t.z.0], data.ecs.motion.try_get(e).unwrap())
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn player_lands_walks_and_jumps_on_generated_terrain() {
    let rc = RegionCoords::new(0, 0);
    let columns = worldgen::generate_region(rc, worldgen::WORLD_SEED, mats());
    let mut server = Region::from_columns(rc, columns);
    server.handle_event(GameEventKind::CreateClient(0)).unwrap();
    // A client-style region (live render sender) so events can roll back.
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(server.data().clone(), Some(send), rc, Some(0));
    drop(server);

    let key = |key, pressed| GameEventKind::PlayerInput(0, InputEvent::Key { key, pressed });
    let mut events = vec![key(Key::KeyE, true), GameEventKind::Tick, key(Key::KeyE, false)];
    events.extend(std::iter::repeat_with(|| GameEventKind::Tick).take(20));
    let landed_at = events.len();
    // Walk each way in turn, holding Space so ledges taller than a step are
    // hopped (Minecraft auto-jump-by-holding).
    events.push(key(Key::Space, true));
    for dir in [Key::KeyW, Key::KeyD, Key::KeyS, Key::KeyA] {
        events.push(key(dir, true));
        events.extend(std::iter::repeat_with(|| GameEventKind::Tick).take(25));
        events.push(key(dir, false));
    }

    let mut hashes = vec![crc(&region)];
    let (mut travelled, mut grounded_ticks) = (0.0f32, 0);
    let mut last = pose(&region).0;
    for (i, ev) in events.into_iter().enumerate() {
        region.handle_event(ev).unwrap();
        hashes.push(crc(&region));

        let (p, motion) = pose(&region);
        if i + 1 == landed_at {
            assert!(motion.grounded, "player must land on the terrain: {p:?} {motion:?}");
        }
        // Never inside the ground: the feet stay above the column they are over.
        if let Some(surface) = region.data().surface_voxel_y(p[0].floor() as i32, p[2].floor() as i32) {
            let top = (surface + 1) as f32;
            assert!(p[1] - FEET > top - 0.5, "event {i}: feet at {} under surface {top}", p[1] - FEET);
        }
        travelled += ((p[0] - last[0]).powi(2) + (p[2] - last[2]).powi(2)).sqrt();
        grounded_ticks += motion.grounded as usize;
        last = p;
    }
    assert!(travelled > 100.0, "player barely moved across the terrain: {travelled} units");
    assert!(grounded_ticks > 20, "player was almost never on the ground: {grounded_ticks} ticks");

    for expected in hashes.iter().rev().skip(1) {
        region.rollback_last_event();
        assert_eq!(*expected, crc(&region), "rollback hash mismatch");
    }
}
