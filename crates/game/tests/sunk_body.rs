//! A body that starts a tick sunk into terrain is put back on its surface.
//! The character controller cannot slide or step out of an overlap by
//! itself; it happens at region seams, where the region a body leaves has
//! no floor past its edge and the body's rounded bottom dips over that edge
//! on the crossing tick (about 3 units over a corner).
use game::movement::{CHARACTER_SKIN, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS};
use game::{Chunk, ChunkCoords, GameEventKind, InputEvent, Pose, Key, Region, RegionCoords};

fn ev(r: &mut Region, e: GameEventKind) {
    r.handle_event(e).unwrap();
    r.forget_last_event();
}

fn body(r: &Region) -> (f32, f32, f32) {
    let d = r.data();
    let e = *d.player_entites.get(&0).unwrap();
    let t = d.physics.bodies.get(*d.ecs.rigidbody.get(e)).unwrap().translation();
    (t.x, t.y, t.z)
}

/// Floor top at y = 8 over x, z in 96..192; the player placed at (x, y, z)
/// walking W+A (towards -x, -z) for four ticks. Returns its trail.
fn walk_from(x: f32, y: f32, z: f32) -> Vec<(f32, f32, f32)> {
    // The floor is block columns 6..12 each way (chunks 3..=5 at 2-block
    // chunks), its edges and corner where they are at any chunk size.
    let (cb, blocks) = (game::CHUNK_BLOCKS, 6..12usize);
    let floor = Chunk::flat_floor(8);
    let mut chunks = Vec::new();
    for cx in blocks.start / cb..=(blocks.end - 1) / cb {
        for cz in blocks.start / cb..=(blocks.end - 1) / cb {
            let mut chunk = Chunk::default();
            for bx in (0..cb).filter(|b| blocks.contains(&(cx * cb + b))) {
                for bz in (0..cb).filter(|b| blocks.contains(&(cz * cb + b))) {
                    let i = game::BlockIndex::from_xyz(bx, 0, bz);
                    chunk.set_block(bx, 0, bz, floor.block_at(bx, 0, bz));
                    if let Some(c) = floor.chisel.get(&i) {
                        chunk.chisel.insert(i, c.clone());
                    }
                }
            }
            chunks.push((ChunkCoords::new(cx, 0, cz), chunk));
        }
    }
    let mut r = Region::from_chunks(RegionCoords::new(0, 0), chunks);
    ev(&mut r, GameEventKind::CreateClient(0));
    for pressed in [true, false] {
        ev(&mut r, GameEventKind::PlayerInput(0, InputEvent::Key { key: Key::KeyE, pressed }));
    }
    for _ in 0..12 {
        ev(&mut r, GameEventKind::Tick);
    }
    r.with_data(|d| {
        let e = *d.data.player_entites.get(&0).unwrap();
        d.set_body_pose_safe(e, Pose::translation(x, y, z));
    });
    for key in [Key::KeyW, Key::KeyA] {
        ev(&mut r, GameEventKind::PlayerInput(0, InputEvent::Key { key, pressed: true }));
    }
    (0..4)
        .map(|_| {
            ev(&mut r, GameEventKind::Tick);
            body(&r)
        })
        .collect()
}

#[test]
fn a_body_sunk_into_the_floor_is_put_back_on_it_and_walks_on() {
    let rest = 8.0 + PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS + CHARACTER_SKIN;
    // Mid-floor and 4 units inside a corner; resting, over an edge, over a corner.
    for (x, z) in [(150.0, 150.0), (188.0, 188.0)] {
        for sink in [0.0, 0.86, 3.03] {
            let trail = walk_from(x, rest - sink, z);
            let at = format!("start ({x}, {}, {z}): {trail:?}", rest - sink);
            let (lx, ly, lz) = *trail.last().unwrap();
            assert!((ly - rest).abs() < 0.01, "must stand on the floor: {at}");
            assert!(lx < x - 5.0 && lz < z - 5.0, "must walk on: {at}");
        }
    }
}
