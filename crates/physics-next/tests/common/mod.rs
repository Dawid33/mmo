//! A player walking through the fork the way the game drives one: the game's
//! own movement code (`game::movement::{plan, sweep_character, settle}`, its
//! constants, `game::collider_cells` for terrain) against a bare physics
//! world, so the fork is pinned by exactly what the game runs. The ports of
//! `crates/game/tests/{walking_smoothness,sunk_body}.rs` drive it.
#![allow(dead_code)]

use game::movement::{self, Motion, MoveKeys, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS};
use game::{BLOCK_VOXELS, CHUNK_BLOCKS, Chunk};
use physics_next::PhysicsWorld;
use rapier3d::prelude::*;
use std::ops::Range;

/// A chunk's edge in world units.
pub const CHUNK: f32 = (CHUNK_BLOCKS * BLOCK_VOXELS) as f32;
/// The game's spawn column (`PLAYER_SPAWN_X`/`_Z`). At 2-block chunks it is
/// the corner where chunk columns 3 and 4 meet rows 3 and 4; at larger chunks
/// it lies inside a chunk, so fixtures state their terrain in world units
/// around it (`chunks_over`, `spawn_x_band`, `spawn_z_walk`), never in chunk
/// indices.
pub const SPAWN: [f32; 2] = [game::PLAYER_SPAWN_X, game::PLAYER_SPAWN_Z];

/// Chunk index along one horizontal axis of the world-unit coordinate `u`.
pub fn chunk_index_of(u: f32) -> i32 {
    (u / CHUNK).floor() as i32
}

/// The chunk holding the world-unit position `units`.
pub fn chunk_of(units: Vector) -> game::ChunkCoords {
    game::chunk_coords_from_translation(game::rapier3d::math::Vector::new(units.x, units.y, units.z))
}

/// Chunk `(x, z)` pairs overlapping the half-open world-unit rectangle
/// (negative edges clamp to 0).
pub fn chunks_over(x_units: Range<f32>, z_units: Range<f32>) -> Vec<(i32, i32)> {
    let span = |r: Range<f32>| chunk_index_of(r.start.max(0.0))..(r.end / CHUNK).ceil() as i32;
    let zs = span(z_units);
    span(x_units).flat_map(|x| zs.clone().map(move |z| (x, z))).collect()
}

/// Every chunk overlapping the unit rectangle, each a `Chunk::flat_floor(depth)`.
pub fn floor_chunks(x_units: Range<f32>, z_units: Range<f32>, depth: u32) -> Vec<((i32, i32, i32), Chunk)> {
    chunks_over(x_units, z_units).into_iter().map(|(x, z)| ((x, 0, z), Chunk::flat_floor(depth))).collect()
}

/// The first chunk row past the spawn's (row 5, z = 160, at 2-block chunks).
pub fn seam_row() -> i32 {
    chunk_index_of(SPAWN[1]) + 1
}
/// World z of [`seam_row`]'s border.
pub fn seam_z() -> f32 {
    seam_row() as f32 * CHUNK
}
/// One chunk either side of the spawn column in x.
pub fn spawn_x_band() -> Range<f32> {
    SPAWN[0] - CHUNK..SPAWN[0] + CHUNK
}
/// From one chunk behind the spawn to `ahead` units past it, padded by two
/// chunks: the z extent of a walk of `ahead` units.
pub fn spawn_z_walk(ahead: f32) -> Range<f32> {
    let end = (chunk_index_of(SPAWN[1]) + 2 + (ahead / CHUNK) as i32) as f32 * CHUNK;
    SPAWN[1] - CHUNK..end
}

/// Adds `chunk`'s terrain at chunk coordinates `(x, y, z)`, the way
/// `seed_terrain_genesis` does: one fixed body at the chunk origin; a full
/// chunk is one cuboid, anything else a `Voxels` per `collider_cells` grid.
pub fn add_chunk(w: &mut PhysicsWorld, (x, y, z): (i32, i32, i32), chunk: &Chunk) {
    let origin = Vector::new(x as f32, y as f32, z as f32) * CHUNK;
    let body = w.bodies.insert(RigidBodyBuilder::fixed().translation(origin));
    let cells = game::collider_cells(&chunk.blocks, &chunk.chisel);
    if cells.is_full_cube() {
        let half = 0.5 * CHUNK;
        let cuboid = ColliderBuilder::cuboid(half, half, half).translation(Vector::splat(half));
        w.colliders.insert_with_parent(cuboid, body, &mut w.bodies);
        return;
    }
    for (size, keys) in cells.grids() {
        let keys: Vec<IVector> = keys.iter().map(|p| IVector::new(p.x, p.y, p.z)).collect();
        w.colliders.insert_with_parent(ColliderBuilder::voxels(Vector::splat(size), &keys), body, &mut w.bodies);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Body {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub grounded: bool,
}

/// One player on hand-built terrain, stepped like a region tick.
pub struct Walker {
    pub w: PhysicsWorld,
    pipeline: PhysicsPipeline,
    pub body: RigidBodyHandle,
    pub motion: Motion,
    pub keys: MoveKeys,
    tick: u64,
}

impl Walker {
    /// `chunks` at their chunk coordinates, and a player dropped at the spawn
    /// column from above them, settled on the ground.
    pub fn new(chunks: impl IntoIterator<Item = ((i32, i32, i32), Chunk)>) -> Self {
        let mut w = PhysicsWorld {
            // Exactly the game's parameters (D6's length unit included), so the
            // rig steps as a region does.
            params: game::PhysicsParams::default().0,
            ..Default::default()
        };
        let mut top = 0;
        for (coords, chunk) in chunks {
            top = top.max(coords.1 + 1);
            add_chunk(&mut w, coords, &chunk);
        }
        let spawn = Vector::new(SPAWN[0], top as f32 * CHUNK + PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS, SPAWN[1]);
        let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(spawn));
        let capsule = ColliderBuilder::capsule_y(PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS);
        w.colliders.insert_with_parent(capsule, body, &mut w.bodies);
        let mut pipeline = PhysicsPipeline::new();
        w.step(&mut pipeline);
        let mut walker = Walker { w, pipeline, body, motion: Motion::default(), keys: MoveKeys::default(), tick: 0 };
        // The spawn clearance is a few ticks of fall.
        walker.ticks(20);
        assert!(walker.body().grounded, "player must settle on the floor: {:?}", walker.body());
        walker
    }

    /// The chunks at y 0 overlapping the world-unit rectangle `x_units` x
    /// `z_units`, one each (`chunk(x, z)` gets its chunk coordinates).
    pub fn on(x_units: Range<f32>, z_units: Range<f32>, chunk: impl Fn(i32, i32) -> Chunk) -> Self {
        let chunks: Vec<_> =
            chunks_over(x_units, z_units).into_iter().map(|(x, z)| ((x, 0, z), chunk(x, z))).collect();
        Self::new(chunks)
    }

    /// Moves the settled player along +z to stand `gap` units short of the
    /// chunk seam at [`seam_z`]; where the spawn is already within `gap` of it
    /// (2-block chunks: 32 away) the player stays put.
    pub fn approach_seam(&mut self, gap: f32) {
        let z = seam_z() - gap;
        let b = self.body();
        if z > b.z {
            self.teleport(Vector::new(b.x, b.y, z));
            self.ticks(2);
        }
    }

    /// One region tick of the player: plan, sweep, settle, step.
    pub fn tick(&mut self) {
        let desired = movement::plan(&mut self.motion, &self.keys, 0.0, self.tick);
        self.keys.jump_pressed = false;
        let w = &self.w;
        let (corrected, grounded) = movement::sweep_character(
            &w.broad_phase,
            &w.narrow_phase,
            &w.bodies,
            &w.colliders,
            self.body,
            &self.motion,
            desired,
        );
        movement::settle(&mut self.motion, &desired, &corrected, grounded);
        if corrected != Vector::ZERO {
            let t = self.w.bodies[self.body].translation();
            self.w.bodies[self.body].set_next_kinematic_translation(t + corrected);
        }
        self.w.step(&mut self.pipeline);
        self.tick += 1;
    }

    pub fn ticks(&mut self, n: usize) {
        for _ in 0..n {
            self.tick();
        }
    }

    pub fn body(&self) -> Body {
        let t = self.w.bodies[self.body].translation();
        Body { x: t.x, y: t.y, z: t.z, grounded: self.motion.grounded }
    }

    /// Puts the body at `pos`.
    pub fn teleport(&mut self, pos: Vector) {
        let body = &mut self.w.bodies[self.body];
        body.set_translation(pos, true);
        body.set_next_kinematic_translation(pos);
        self.w.step(&mut self.pipeline);
    }
}
