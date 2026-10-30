//! Digging into implied ground: rock below a region's materialised surface
//! band exists only as a per-column recipe until a dig exposes it. Breaking a
//! block materialises its own chunk and every implied face-neighbour chunk in
//! the same undoable edit, so reachable air is always bordered by real chunks;
//! bedrock cannot be broken.
use game::{
    state_hash, Block, BlockId, Chunk, ChunkCoords, Column, ColumnCoords, ColumnGround,
    GameEventKind, InputEvent, Key, MouseButton, Pose,
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
        .with_ground(ColumnGround::plain(vec![SURFACE; game::COLUMN_FOOTPRINT].into_boxed_slice(), 0, STONE, BEDROCK))
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

/// [`region`] over the chunk columns holding the block ranges `xs` x `zs`
/// (half-open, in blocks), so fixtures state their extent in blocks.
fn region_blocks(xs: std::ops::Range<i32>, zs: std::ops::Range<i32>) -> Region {
    let cb = CHUNK_BLOCKS as i32;
    let cols = |r: std::ops::Range<i32>| (r.start / cb) as usize..=((r.end - 1) / cb) as usize;
    region(cols(xs), cols(zs))
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
    let mut region = region_blocks(2..6, 2..6);
    // The chunk under the one holding block (2, 0, 2): the band floor sits on
    // its bottom face.
    let below = locate([2, -1, 2]).unwrap().0;
    assert!(!has_chunk(region.data(), below));
    assert!(dig(&mut region, [2, 1, 2]), "the band top breaks");
    assert!(!has_chunk(region.data(), below), "a block still covers the chunk below");
    assert!(dig(&mut region, [2, 0, 2]), "the band floor breaks");
    let r = region.data();
    let e = r.chunk_key_at(below).expect("the chunk below is materialised");
    let chunk = r.ecs.chunk.try_get(e).clone().unwrap();
    let top = CHUNK_BLOCKS - 1;
    for bx in 0..CHUNK_BLOCKS {
        for bz in 0..CHUNK_BLOCKS {
            assert_eq!(chunk.block_at(bx, top, bz).id, STONE);
        }
    }
    assert!(r.implied_block(below, (0, top, 0)).is_none(), "materialised, so no longer implied");
    // The neighbour exists as a real, solid chunk: a ray finds it.
    let hit = r
        .raycast_block(
            game::rapier3d::math::Vector::new(40.0, 8.0, 40.0),
            game::rapier3d::math::Vector::new(0.0, -1.0, 0.0),
            3.0,
        )
        .expect("the ray stops on the materialised stone");
    assert_eq!(hit.block, [2, -1, 2]);
}

#[test]
fn implied_ground_is_solid_to_a_ray() {
    let region = region_blocks(0..4, 0..4);
    // Straight down from inside the band top: block 0 is band, below it implied.
    let r = region.data();
    assert!(!has_chunk(r, locate([0, -1, 0]).unwrap().0));
    let hit = r.raycast_block(
        game::rapier3d::math::Vector::new(8.0, -8.0, 8.0),
        game::rapier3d::math::Vector::new(0.0, -1.0, 0.0),
        3.0,
    );
    assert_eq!(hit.map(|h| h.block), Some([0, -1, 0]), "implied stone stops the ray");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn exposure_invariant_after_random_digs() {
    // Shaft area: a 4x4 block square straddling the chunk faces at block
    // 2 * CHUNK_BLOCKS (blocks 2..6 at 2-block chunks, in chunk columns 1 and
    // 2); columns 0 and 3 are an untouched band ring around it (column 0 holds
    // the region's edge ring, which can't be dug below the surface).
    let at = 2 * CHUNK_BLOCKS as i32 - 2;
    let mut region = region_blocks(0..4 * CHUNK_BLOCKS as i32, 0..4 * CHUNK_BLOCKS as i32);
    let mut depth = [[SURFACE as i32; 4]; 4];
    let mut seed: u64 = 0x5EED_D16;
    let mut refused = 0;
    for i in 0..200 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let (x, z) = (((seed >> 33) % 4) as usize, ((seed >> 41) % 4) as usize);
        let pos = [x as i32 + at, depth[x][z], z as i32 + at];
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
    let mut region = region_blocks(2..6, 2..6);
    let floor = SURFACE as i32 - UNDERGROUND_DEPTH; // first bedrock block
    for y in (floor + 1..=SURFACE as i32).rev() {
        assert!(dig(&mut region, [2, y, 2]), "stone at y={y} breaks");
    }
    assert!(exposed_implied(region.data()).is_empty());
    let bedrock = [2, floor, 2];
    let (c, local) = locate(bedrock).unwrap();
    let e = region.data().chunk_key_at(c).expect("the bedrock chunk is materialised");
    assert_eq!(region.data().ecs.chunk.try_get(e).as_ref().unwrap().block_at(local.0, local.1, local.2).id, BEDROCK);
    let before = state_hash(region.data());
    assert!(!dig(&mut region, bedrock), "bedrock is unbreakable");
    assert_eq!(before, state_hash(region.data()));
    // Implied bedrock (no chunk yet) is refused too, without materialising:
    // two chunks below the bedrock block's own (y -63 at 2-block chunks).
    let cb = CHUNK_BLOCKS as i32;
    let deep = [3, (floor.div_euclid(cb) - 1) * cb - 1, 3];
    let (dc, _) = locate(deep).unwrap();
    assert!(!dig(&mut region, deep));
    assert!(!has_chunk(region.data(), dc));
    assert_eq!(before, state_hash(region.data()));
}

#[test]
fn placing_into_implied_ground_keeps_the_siblings() {
    let cb = CHUNK_BLOCKS as i32;
    let mut region = region_blocks(0..2 * cb, 0..2 * cb);
    // The implied chunk below chunk (0, 0, 0); the block placed is its bottom
    // corner block (0, -CHUNK_BLOCKS, 0).
    let c = ChunkCoords::new(0, -1, 0);
    let expected: Vec<_> = (0..CHUNK_BLOCKS.pow(3))
        .map(|i| (i % CHUNK_BLOCKS, (i / CHUNK_BLOCKS) % CHUNK_BLOCKS, i / (CHUNK_BLOCKS * CHUNK_BLOCKS)))
        .map(|l| (l, region.data().implied_block(c, l).unwrap()))
        .collect();
    let dirt = Block::new(BlockId(1));
    let mut placed = false;
    region.with_data(|r| placed = r.set_block_safe([0, -cb, 0], dirt));
    assert!(placed);
    let r = region.data();
    let e = r.chunk_key_at(c).expect("the chunk materialised");
    let chunk = r.ecs.chunk.try_get(e).clone().unwrap();
    for ((bx, by, bz), want) in expected {
        let want = if (bx, by, bz) == (0, 0, 0) { dirt } else { want };
        assert_eq!(chunk.block_at(bx, by, bz), want, "block {:?}", (bx, by, bz));
    }
    assert!(exposed_implied(r).is_empty());
    // Writing an implied slot's own block changes nothing and reports so.
    let before = state_hash(region.data());
    let mut same = true;
    region.with_data(|r| same = r.set_block_safe([cb + 1, -cb, cb + 1], Block::new(STONE)));
    assert!(!same);
    assert!(!has_chunk(region.data(), ChunkCoords::new(1, -1, 1)));
    assert_eq!(before, state_hash(region.data()));
}

#[test]
fn bedrock_cannot_be_replaced() {
    let mut region = region_blocks(2..6, 2..6);
    let floor = SURFACE as i32 - UNDERGROUND_DEPTH;
    let before = state_hash(region.data());
    for pos in [[2, floor, 2], [3, floor - 3, 3]] {
        let mut changed = true;
        region.with_data(|r| changed = r.set_block_safe(pos, Block::new(STONE)));
        assert!(!changed, "implied bedrock at {pos:?} takes no write");
    }
    assert_eq!(before, state_hash(region.data()));
    for y in (floor + 1..=SURFACE as i32).rev() {
        assert!(dig(&mut region, [2, y, 2]));
    }
    let before = state_hash(region.data());
    let mut changed = true;
    region.with_data(|r| changed = r.set_block_safe([2, floor, 2], Block::new(STONE)));
    assert!(!changed, "materialised bedrock takes no write");
    assert_eq!(before, state_hash(region.data()));
    assert!(!dig(&mut region, [2, floor, 2]));
}

#[test]
fn undo_of_a_materialising_dig_restores_the_hash() {
    let mut region = region_blocks(2..6, 2..6);
    // Block (2, -CHUNK_BLOCKS, 2) is the bottom of implied chunk sub-Y -1: the
    // dig materialises that chunk and the one below it (sub-Y -2).
    let pos = [2, -(CHUNK_BLOCKS as i32), 2];
    let own = locate(pos).unwrap().0;
    let below = ChunkCoords::new(own.x, own.y - 1, own.z);
    region.with_data(|r| {
        let before = state_hash(r);
        r.new_transaction();
        assert!(r.set_block_safe(pos, air()));
        assert!(has_chunk(r, own) && has_chunk(r, below), "both chunks materialise");
        // The default hash mode verifies every undo entry as it is rolled back.
        r.rollback();
        assert_eq!(before, state_hash(r), "the dig rolls back bit-exact");
        assert!(!has_chunk(r, own) && !has_chunk(r, below), "the chunk entities are gone");
        assert!(r.implied_block(own, (0, 0, 0)).is_some(), "the ground is implied again");
    });
}

/// Known limit: the outermost block ring can't be dug below the surface (its
/// outward face would open into the next region's unmaterialised ground).
/// The surface block itself and blocks one column in stay diggable.
#[test]
fn digging_at_the_region_edge_does_not_reach_outside() {
    let last = REGION_CHUNKS - 1;
    let mut high = region(last..=last, last..=last);
    let edge = (REGION_CHUNKS * CHUNK_BLOCKS) as i32 - 1;
    assert!(dig(&mut high, [edge, 1, edge]), "the edge surface block breaks");
    let before = state_hash(high.data());
    assert!(!dig(&mut high, [edge, 0, edge]), "below the surface on the edge ring is refused");
    assert!(!dig(&mut high, [edge - 1, 0, edge]), "edge ring on one axis is enough");
    assert_eq!(before, state_hash(high.data()), "a refused dig changes nothing");
    assert!(!has_chunk(high.data(), ChunkCoords::new(last, -1, last)));
    // One column in: diggable, and materialises the ground below.
    assert!(dig(&mut high, [edge - 1, 1, edge - 1]));
    assert!(dig(&mut high, [edge - 1, 0, edge - 1]));
    let chunks = high.data().terrain_snapshot().chunks;
    assert!(chunks.iter().any(|(c, _)| *c == ChunkCoords::new(last, -1, last)));
    assert!(chunks.iter().all(|(c, _)| c.x < REGION_CHUNKS && c.z < REGION_CHUNKS));
    assert!(exposed_implied(high.data()).is_empty());
    // The low edge too: a region whose ground starts at column 0.
    let mut low = region(0..=0, 0..=0);
    assert!(dig(&mut low, [0, 1, 0]));
    assert!(!dig(&mut low, [0, 0, 0]), "the low edge ring is refused below the surface");
    assert!(!dig(&mut low, [1, 0, 0]), "z = 0 is still the edge ring");
    assert!(low.data().terrain_snapshot().chunks.iter().all(|(c, _)| c.x == 0 && c.z == 0));
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
    // The chunk columns around the (128, 128) spawn: blocks 6..12.
    let mut region = region_blocks(6..12, 6..12);
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
    assert!(has_chunk(region.data(), locate([8, -1, 8]).unwrap().0), "the chunk below materialised");
    let (y, grounded) = player(&region);
    assert!(grounded, "the player stands on the materialised stone");
    assert!(y > 0.0 && y < 25.0, "standing on block -1 (top at y=0), got {y}");
    assert!(exposed_implied(region.data()).is_empty());
}
