//! A jump on flat terrain through the real character controller must follow
//! the ballistic arc `movement::plan` computes: launch on the tick Space is
//! read, rise for JUMP_SPEED/GRAVITY seconds, and land with the body back on
//! its skin. Collision must not shorten, flatten, or delay it.
use std::collections::BTreeMap;

use game::movement::{FALL_GRAVITY, GRAVITY, JUMP_SPEED, TICK_DT, UNITS_PER_METER};
use game::{Chunk, ChunkCoords, GameEventKind, InputEvent, Key, Region, RegionCoords, World};

fn r0() -> RegionCoords {
    RegionCoords::new(0, 0)
}

fn world() -> World {
    let mut chunks = Vec::new();
    for x in 3..=4 {
        for z in 3..=4 {
            chunks.push((ChunkCoords::new(x, 0, z), Chunk::flat_floor(16)));
        }
    }
    let mut world = World::new();
    world.load(&r0(), Region::from_chunks(r0(), chunks));
    world.handle_region_event(GameEventKind::CreateClient(0), r0()).unwrap();
    world.forget_last_event(&r0());
    key(&mut world, Key::KeyE, true);
    key(&mut world, Key::KeyE, false);
    ticks(&mut world, 12);
    assert!(body(&world).1, "player must settle on the floor");
    world
}

fn key(world: &mut World, key: Key, pressed: bool) {
    world
        .handle_region_event(GameEventKind::PlayerInput(0, InputEvent::Key { key, pressed }), r0())
        .unwrap();
    world.forget_last_event(&r0());
}

fn ticks(world: &mut World, n: usize) {
    let mut results = BTreeMap::new();
    for _ in 0..n {
        world.progress_world_one_tick(&mut results);
    }
}

/// (height, grounded, vel_y)
fn body(world: &World) -> (f32, bool, f32) {
    let data = world.data(&r0());
    let e = *data.player_entites.get(&0).unwrap();
    let t = *data.physics.bodies.get(*data.ecs.rigidbody.get(e)).unwrap().translation();
    let m = data.ecs.motion.try_get(e).unwrap_or_default();
    (t.y.0, m.grounded, m.vel_y.0)
}

#[test]
fn jump_follows_the_ballistic_arc() {
    let mut world = world();
    let floor = body(&world).0;
    key(&mut world, Key::Space, true);
    let mut arc = Vec::new();
    for _ in 0..40 {
        ticks(&mut world, 1);
        let (y, grounded, vy) = body(&world);
        arc.push((y - floor, grounded, vy));
        if grounded && arc.len() > 2 {
            break;
        }
    }
    key(&mut world, Key::Space, false);
    for (i, (h, g, vy)) in arc.iter().enumerate() {
        eprintln!("tick {i:2}: h = {:6.2} u ({:.3} m) grounded={g} vel_y={vy:.1}", h, h / UNITS_PER_METER);
    }
    // Ideal arc: rise v/g up to v²/2g, fall back under FALL_GRAVITY.
    let peak_m = JUMP_SPEED * JUMP_SPEED / (2.0 * GRAVITY);
    let airtime_s = JUMP_SPEED / GRAVITY + (2.0 * peak_m / FALL_GRAVITY).sqrt();
    let airtime_ticks = (airtime_s / TICK_DT).round() as usize;
    let peak = arc.iter().map(|a| a.0).fold(0.0, f32::max) / UNITS_PER_METER;
    eprintln!("airborne {} ticks (ideal {airtime_ticks}), peak {peak:.3} m (ideal {peak_m:.3})", arc.len());
    assert!(arc[0].0 > 0.0, "the body leaves the ground on the tick the jump is read");
    assert!((peak - peak_m).abs() < 0.05, "peak {peak} m vs ideal {peak_m} m");
    assert!(
        arc.len().abs_diff(airtime_ticks) <= 1,
        "airborne {} ticks vs ideal {airtime_ticks}",
        arc.len()
    );
}
