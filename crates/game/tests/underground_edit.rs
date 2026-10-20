//! Digging into implied ground: rock below a region's materialised surface
//! band exists only as a per-column recipe until a dig exposes it. Breaking a
//! block materialises its own chunk and every implied face-neighbour chunk in
//! the same undoable edit, so reachable air is always bordered by real chunks;
//! bedrock cannot be broken.
use game::{
    state_hash, Block, BlockId, CellChanges, CellCoords, CellPayload, Chunk, ChunkCoords, Column,
    ColumnCoords, ColumnGround, DormantCell, GameEventKind, InputEvent, Key, MouseButton, Pose,
    Region, RegionCoords, Rollback, CHUNK_BLOCKS, REGION_CHUNKS, UNDERGROUND_DEPTH,
};

const STONE: BlockId = BlockId(2);
const BEDROCK: BlockId = BlockId(3);
/// Surface block Y of every test column: the band is one fully solid chunk
/// at sub-Y 0 (blocks 0 and 1); everything below is implied.
const SURFACE: i16 = 1;

fn r0() -> RegionCoords {
    RegionCoords::new(0, 0)
}

fn air() -> Block {
    Block::new(BlockId::AIR)
}

/// One band chunk (sub-Y 0, fully stone) over plain implied ground.
fn column() -> Column {
    let band = game::SubChunk::Dense(Box::new(Chunk::flat_floor_with(32, STONE)));
    // `below` must differ from the band, or normalising collapses it away.
    Column::new(0, Block::new(BlockId(1)), air(), vec![band])
        .with_ground(ColumnGround::plain([SURFACE; 4], 0, STONE, BEDROCK))
}

/// A region over chunk columns `xs` x `zs`, with a render-update channel (as
/// client regions, the ones that roll back, have).
fn region(xs: std::ops::RangeInclusive<usize>, zs: std::ops::RangeInclusive<usize>) -> Region {
    let mut columns = Vec::new();
    for x in xs {
        for z in zs.clone() {
            columns.push((ColumnCoords::new(x, z), column()));
        }
    }
    let built = Region::from_columns(r0(), columns);
    let (send, recv) = crossbeam::channel::unbounded();
    std::mem::forget(recv);
    Region::new(built.data().clone(), Some(send), r0(), None)
}

/// The chunk holding block `pos` and its block index, if inside the region.
fn locate(pos: [i32; 3]) -> Option<(ChunkCoords, (usize, usize, usize))> {
    let cb = CHUNK_BLOCKS as i32;
    let (cx, cz) = (pos[0].div_euclid(cb), pos[2].div_euclid(cb));
    if cx < 0 || cz < 0 || cx >= REGION_CHUNKS as i32 || cz >= REGION_CHUNKS as i32 {
        return None;
    }
    let local = (
        pos[0].rem_euclid(cb) as usize,
        pos[1].rem_euclid(cb) as usize,
        pos[2].rem_euclid(cb) as usize,
    );
    Some((ChunkCoords::new(cx as usize, pos[1].div_euclid(cb), cz as usize), local))
}

fn dig(region: &mut Region, pos: [i32; 3]) -> bool {
    let mut changed = false;
    region.with_data(|r| changed = r.set_block_safe(pos, air()));
    changed
}

fn has_chunk(r: &Rollback, coords: ChunkCoords) -> bool {
    r.chunk_key_at(coords).is_some()
}

const FACES: [[i32; 3]; 6] = [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]];

/// Every air block of every materialised chunk whose face neighbour lies in a
/// chunk that is missing and implied.
fn exposed_implied(r: &Rollback) -> Vec<([i32; 3], ChunkCoords)> {
    let cb = CHUNK_BLOCKS as i32;
    let mut out = Vec::new();
    for (c, chunk) in r.terrain_snapshot().chunks {
        for bx in 0..CHUNK_BLOCKS {
            for by in 0..CHUNK_BLOCKS {
                for bz in 0..CHUNK_BLOCKS {
                    let bi = game::BlockIndex::from_xyz(bx, by, bz);
                    if chunk.block_at(bx, by, bz).id != BlockId::AIR || chunk.chisel.contains_key(&bi) {
                        continue;
                    }
                    let pos = [c.x as i32 * cb + bx as i32, c.y * cb + by as i32, c.z as i32 * cb + bz as i32];
                    for d in FACES {
                        let n = [pos[0] + d[0], pos[1] + d[1], pos[2] + d[2]];
                        let Some((nc, local)) = locate(n) else { continue };
                        if r.implied_block(nc, local).is_some() {
                            out.push((pos, nc));
                        }
                    }
                }
            }
        }
    }
    out
}

#[test]
fn breaking_the_band_floor_materialises_the_chunk_below() {
    let mut region = region(0..=1, 0..=1);
    let below = ChunkCoords::new(0, -1, 0);
    assert!(!has_chunk(region.data(), below));
    assert!(dig(&mut region, [0, 1, 0]), "the band top breaks");
    assert!(!has_chunk(region.data(), below), "a block still covers the chunk below");
    assert!(dig(&mut region, [0, 0, 0]), "the band floor breaks");
    let r = region.data();
    let e = r.chunk_key_at(below).expect("the chunk below is materialised");
    let chunk = r.ecs.chunk.try_get(e).clone().unwrap();
    for (bx, bz) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
        assert_eq!(chunk.block_at(bx, 1, bz).id, STONE);
    }
    assert!(r.implied_block(below, (0, 1, 0)).is_none(), "materialised, so no longer implied");
    // The neighbour exists as a real, solid chunk: a ray finds it.
    let hit = r
        .raycast_block(
            game::rapier3d::math::Vector::new(8.0, 8.0, 8.0),
            game::rapier3d::math::Vector::new(0.0, -1.0, 0.0),
            3.0,
        )
        .expect("the ray stops on the materialised stone");
    assert_eq!(hit.block, [0, -1, 0]);
}

#[test]
fn implied_ground_is_solid_to_a_ray() {
    let region = region(0..=1, 0..=1);
    // Straight down from inside the band top: block 0 is band, below it implied.
    let r = region.data();
    assert!(!has_chunk(r, ChunkCoords::new(0, -1, 0)));
    let hit = r.raycast_block(
        game::rapier3d::math::Vector::new(8.0, -8.0, 8.0),
        game::rapier3d::math::Vector::new(0.0, -1.0, 0.0),
        3.0,
    );
    assert_eq!(hit.map(|h| h.block), Some([0, -1, 0]), "implied stone stops the ray");
}

#[test]
fn exposure_invariant_after_random_digs() {
    // Shaft area: blocks 0..4 x 0..4 (chunk columns 0 and 1); column 2 is an
    // untouched band ring around it.
    let mut region = region(0..=2, 0..=2);
    let mut depth = [[SURFACE as i32; 4]; 4];
    let mut seed: u64 = 0x5EED_D16;
    let mut refused = 0;
    for i in 0..200 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let (x, z) = (((seed >> 33) % 4) as usize, ((seed >> 41) % 4) as usize);
        let pos = [x as i32, depth[x][z], z as i32];
        if dig(&mut region, pos) {
            depth[x][z] -= 1;
        } else {
            refused += 1;
        }
        let exposed = exposed_implied(region.data());
        assert!(exposed.is_empty(), "dig {i} at {pos:?} left air next to implied chunks: {exposed:?}");
    }
    assert_eq!(refused, 0, "nothing reached bedrock in 200 digs");
}

#[test]
fn bedrock_cannot_be_broken() {
    let mut region = region(0..=1, 0..=1);
    let floor = SURFACE as i32 - UNDERGROUND_DEPTH; // first bedrock block
    for y in (floor + 1..=SURFACE as i32).rev() {
        assert!(dig(&mut region, [0, y, 0]), "stone at y={y} breaks");
    }
    assert!(exposed_implied(region.data()).is_empty());
    let bedrock = [0, floor, 0];
    let (c, local) = locate(bedrock).unwrap();
    let e = region.data().chunk_key_at(c).expect("the bedrock chunk is materialised");
    assert_eq!(region.data().ecs.chunk.try_get(e).as_ref().unwrap().block_at(local.0, local.1, local.2).id, BEDROCK);
    let before = state_hash(region.data());
    assert!(!dig(&mut region, bedrock), "bedrock is unbreakable");
    assert_eq!(before, state_hash(region.data()));
    // Implied bedrock (no chunk yet) is refused too, without materialising.
    let deep = [3, floor - 4, 3];
    let (dc, _) = locate(deep).unwrap();
    assert!(!dig(&mut region, deep));
    assert!(!has_chunk(region.data(), dc));
    assert_eq!(before, state_hash(region.data()));
}

#[test]
fn undo_of_a_materialising_dig_restores_the_hash() {
    let mut region = region(0..=1, 0..=1);
    // Block (0, -2, 0) is the bottom of implied chunk sub-Y -1: the dig
    // materialises that chunk and the one below it (sub-Y -2).
    let (own, below) = (ChunkCoords::new(0, -1, 0), ChunkCoords::new(0, -2, 0));
    region.with_data(|r| {
        let before = state_hash(r);
        r.new_transaction();
        assert!(r.set_block_safe([0, -2, 0], air()));
        assert!(has_chunk(r, own) && has_chunk(r, below), "both chunks materialise");
        // The default hash mode verifies every undo entry as it is rolled back.
        r.rollback();
        assert_eq!(before, state_hash(r), "the dig rolls back bit-exact");
        assert!(!has_chunk(r, own) && !has_chunk(r, below), "the chunk entities are gone");
        assert!(r.implied_block(own, (0, 0, 0)).is_some(), "the ground is implied again");
    });
}

#[test]
fn digging_at_the_region_edge_does_not_reach_outside() {
    let last = REGION_CHUNKS - 1;
    let mut edge_region = region(last..=last, last..=last);
    let edge = (REGION_CHUNKS * CHUNK_BLOCKS) as i32 - 1;
    assert!(dig(&mut edge_region, [edge, 1, edge]));
    assert!(dig(&mut edge_region, [edge, 0, edge]));
    let chunks = edge_region.data().terrain_snapshot().chunks;
    assert!(chunks.iter().any(|(c, _)| *c == ChunkCoords::new(last, -1, last)));
    assert!(chunks.iter().all(|(c, _)| c.x < REGION_CHUNKS && c.z < REGION_CHUNKS));
    // The low edge too: a region whose ground starts at column 0.
    let mut low = region(0..=0, 0..=0);
    assert!(dig(&mut low, [0, 1, 0]) && dig(&mut low, [0, 0, 0]));
    assert!(low.data().terrain_snapshot().chunks.iter().all(|(c, _)| c.x == 0 && c.z == 0));
}

#[test]
fn cell_mode_digs_keep_band_semantics() {
    const CC: usize = 8;
    let mut region = Region::new_cells(r0(), CC, None, None);
    let cell = CellCoords::new(0, 0);
    let content = DormantCell {
        chunks: vec![(ChunkCoords::new(0, 0, 0), Chunk::flat_floor_with(32, STONE))],
        entities: vec![],
    };
    region
        .handle_event(GameEventKind::CellChanges(CellChanges {
            deactivate: vec![],
            activate: vec![CellPayload { cell, version: 1, materialized_hash: 0, content }],
        }))
        .unwrap();
    region.forget_last_event();
    let ground = ColumnGround::plain([SURFACE; 4], 0, STONE, BEDROCK);
    region.with_data(|r| r.seed_ground_genesis(vec![(ColumnCoords::new(0, 0), ground)]));
    assert!(dig(&mut region, [0, 1, 0]));
    assert!(dig(&mut region, [0, 0, 0]));
    assert!(!has_chunk(region.data(), ChunkCoords::new(0, -1, 0)), "cell mode materialises nothing");
}

// --- one raycast case through the player's click, as tests/block_edit.rs ---

fn input(ev: InputEvent) -> GameEventKind {
    GameEventKind::PlayerInput(0, ev)
}

fn forget(region: &mut Region, ev: GameEventKind) {
    region.handle_event(ev).unwrap();
    region.forget_last_event();
}

/// Player body (y, grounded).
fn player(region: &Region) -> (f32, bool) {
    let data = region.data();
    let e = *data.player_entites.get(&0).unwrap();
    let t = data.physics.bodies.get(*data.ecs.rigidbody.get(e)).unwrap().translation();
    (t.y, data.ecs.motion.try_get(e).unwrap_or_default().grounded)
}

fn click_down() -> Vec<GameEventKind> {
    let button = MouseButton::Left;
    vec![
        input(InputEvent::MouseMotion { dx: 0.0, dy: 1000.0 }),
        input(InputEvent::MouseButton { button, pressed: true }),
        GameEventKind::Tick,
        input(InputEvent::MouseButton { button, pressed: false }),
        GameEventKind::Tick,
    ]
}

#[test]
fn a_player_digging_down_lands_on_materialised_ground() {
    // Chunk columns 3..=5 around the (128, 128) spawn.
    let mut region = region(3..=5, 3..=5);
    forget(&mut region, GameEventKind::CreateClient(0));
    forget(&mut region, input(InputEvent::Key { key: Key::KeyE, pressed: true }));
    forget(&mut region, input(InputEvent::Key { key: Key::KeyE, pressed: false }));
    for _ in 0..12 {
        forget(&mut region, GameEventKind::Tick);
    }
    assert!(player(&region).1, "player must settle on the band");
    // Middle of block (8, _, 8), clear of its neighbours.
    let y = player(&region).0;
    region.with_data(|r| {
        let e = *r.data.player_entites.get(&0).unwrap();
        r.set_body_pose_safe(e, Pose::translation(136.0, y, 136.0));
    });
    for _ in 0..3 {
        forget(&mut region, GameEventKind::Tick);
    }
    // Break the band top, fall in, break the band floor, fall onto implied
    // ground that the dig materialised.
    for _ in 0..2 {
        for ev in click_down() {
            region.handle_event(ev).unwrap();
        }
        for _ in 0..20 {
            region.handle_event(GameEventKind::Tick).unwrap();
        }
    }
    assert!(has_chunk(region.data(), ChunkCoords::new(4, -1, 4)), "the chunk below materialised");
    let (y, grounded) = player(&region);
    assert!(grounded, "the player stands on the materialised stone");
    assert!(y > 0.0 && y < 25.0, "standing on block -1 (top at y=0), got {y}");
    assert!(exposed_implied(region.data()).is_empty());
}
