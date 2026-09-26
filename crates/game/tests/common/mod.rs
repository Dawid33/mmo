//! Movement rig shared by the integration tests that drive a player through
//! the real region tick: a hand-built floor, one settled player, key presses
//! and per-tick body reads. Every read of the physics engine's types lives
//! here, so an engine switch migrates one file.
#![allow(dead_code)]

use std::collections::BTreeMap;

use game::{
    Chunk, ChunkCoords, EntityKey, GameEventKind, InputEvent, IsometryReal, Key, Region,
    RegionCoords, Rollback, World,
};

pub fn r0() -> RegionCoords {
    RegionCoords::new(0, 0)
}

/// A world of chunks (y 0) with one player whose controls are on, settled on
/// the ground at the spawn column (128, 128) — the corner where columns 3 and
/// 4 meet rows 3 and 4.
pub fn world_of(
    columns: std::ops::RangeInclusive<usize>,
    rows: std::ops::RangeInclusive<usize>,
    chunk: impl Fn(usize, usize) -> Chunk,
) -> World {
    let mut chunks = Vec::new();
    for x in columns {
        for z in rows.clone() {
            chunks.push((ChunkCoords::new(x, 0, z), chunk(x, z)));
        }
    }
    let mut world = World::new();
    world.load(&r0(), Region::from_chunks(r0(), chunks));
    world.handle_region_event(GameEventKind::CreateClient(0), r0()).unwrap();
    world.forget_last_event(&r0());
    key(&mut world, Key::KeyE, true);
    key(&mut world, Key::KeyE, false);
    // The spawn clearance is a few ticks of fall.
    ticks(&mut world, 12);
    assert!(body(&world).grounded, "player must settle on the floor");
    world
}

/// One-voxel terraces, a step down per chunk along +z: the gentle slopes of
/// generated terrain.
pub fn terraces(rows: std::ops::RangeInclusive<usize>) -> World {
    world_of(3..=4, rows, |_, z| Chunk::flat_floor(28 - z as u32))
}

pub fn key(world: &mut World, key: Key, pressed: bool) {
    world
        .handle_region_event(GameEventKind::PlayerInput(0, InputEvent::Key { key, pressed }), r0())
        .unwrap();
    world.forget_last_event(&r0());
}

pub fn ticks(world: &mut World, n: usize) {
    let mut results = BTreeMap::new();
    for _ in 0..n {
        world.progress_world_one_tick(&mut results);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Body {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub grounded: bool,
    pub vel_y: f32,
}

/// Position and motion of any entity with a body.
pub fn entity_body(data: &Rollback, e: EntityKey) -> Body {
    let t = *data.physics.bodies.get(*data.ecs.rigidbody.get(e)).unwrap().translation();
    let m = data.ecs.motion.try_get(e).unwrap_or_default();
    Body { x: t.x.0, y: t.y.0, z: t.z.0, grounded: m.grounded, vel_y: m.vel_y.0 }
}

/// The player's body.
pub fn body(world: &World) -> Body {
    let data = world.data(&r0());
    entity_body(data, *data.player_entites.get(&0).unwrap())
}

/// Puts the player's body at `(x, y, z)`, undo-safely.
pub fn teleport(world: &mut World, x: f32, y: f32, z: f32) {
    world.regions.get_mut(&r0()).unwrap().with_data(|d| {
        let e = *d.data.player_entites.get(&0).unwrap();
        d.set_body_pose_safe(e, IsometryReal::translation(x.into(), y.into(), z.into()));
    });
}

/// Spawns a slime of `size` with its body centre at `pos`, undo-safely.
/// `tick` seeds its roll sequence.
pub fn spawn_slime(region: &mut Region, pos: [f32; 3], size: u8, yaw: f32, tick: u64) -> EntityKey {
    let mut slime = None;
    region.with_data(|d| {
        let pos = game::parry::math::Vector::new(pos[0].into(), pos[1].into(), pos[2].into());
        slime = Some(d.spawn_slime_safe(pos, size, yaw.into(), tick));
    });
    slime.unwrap()
}

/// `(fixed bodies, fixed bodies with mass)`. Terrain bodies are fixed and
/// must skip collider mass properties: for `Voxels` those go through
/// `SymmetricEigen`'s std `acos`/`cos`, which is not bit-identical across
/// targets.
pub fn fixed_body_mass_count(data: &Rollback) -> (usize, usize) {
    let fixed: Vec<_> = data.physics.bodies.iter().filter(|(_, b)| b.is_fixed()).collect();
    let massive = fixed.iter().filter(|(_, b)| b.mass().0 != 0.0).count();
    (fixed.len(), massive)
}
