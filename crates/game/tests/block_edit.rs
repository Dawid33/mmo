//! Left click breaks the block under the crosshair, right click places one
//! against the face it hit. Edits run inside the tick from the player's eye
//! and look, so they must change terrain, collision and state hash exactly
//! as the tick's other mutations do: bit-exact under rollback and identical
//! on replay.
use game::edit::PLACE_BLOCK;
use game::{
    BlockId, Chunk, ChunkCoords, GameEventKind, InputEvent, Pose, Key, MouseButton, Region,
    RegionCoords, BLOCK_VOXELS,
};

fn r0() -> RegionCoords {
    RegionCoords::new(0, 0)
}

/// Chunk columns 3..=5 x 3..=5 of `floor`, with player 0 spawned and settled
/// at (128, 128), controls on. Setup events are forgotten, so only what a
/// test does afterwards can be rolled back.
fn region(floor: impl Fn() -> Chunk) -> Region {
    let mut chunks = Vec::new();
    for x in 3..=5 {
        for z in 3..=5 {
            chunks.push((ChunkCoords::new(x, 0, z), floor()));
        }
    }
    // A render-update channel, as client regions (the ones that roll back)
    // have: undo entries hand their compensating events to it.
    let (send, recv) = crossbeam::channel::unbounded();
    std::mem::forget(recv);
    let mut region = Region::new(game::Rollback::new(Some(send.clone())), Some(send), r0(), None);
    region.with_data(|r| r.seed_terrain_genesis(chunks));
    forget(&mut region, GameEventKind::CreateClient(0));
    forget(&mut region, input(InputEvent::Key { key: Key::KeyE, pressed: true }));
    forget(&mut region, input(InputEvent::Key { key: Key::KeyE, pressed: false }));
    for _ in 0..12 {
        forget(&mut region, GameEventKind::Tick);
    }
    assert!(player(&region).3, "player must settle on the floor");
    region
}

fn input(ev: InputEvent) -> GameEventKind {
    GameEventKind::PlayerInput(0, ev)
}

fn forget(region: &mut Region, ev: GameEventKind) {
    region.handle_event(ev).unwrap();
    region.forget_last_event();
}

/// Player body (x, y, z, grounded).
fn player(region: &Region) -> (f32, f32, f32, bool) {
    let data = region.data();
    let e = *data.player_entites.get(&0).unwrap();
    let t = data.physics.bodies.get(*data.ecs.rigidbody.get(e)).unwrap().translation();
    let grounded = data.ecs.motion.try_get(e).unwrap_or_default().grounded;
    (t.x, t.y, t.z, grounded)
}

/// Moves the player to (x, current y, z), in a forgotten transaction.
fn teleport(region: &mut Region, x: f32, z: f32) {
    let (_, y, _, _) = player(region);
    region.with_data(|r| {
        let e = *r.data.player_entites.get(&0).unwrap();
        r.set_body_pose_safe(e, Pose::translation(x, y, z));
    });
    for _ in 0..3 {
        forget(region, GameEventKind::Tick);
    }
}

/// Events for: pitch the look by `dy` mouse pixels (positive looks down),
/// click `button`, one tick, release.
fn click(dy: f32, button: MouseButton) -> Vec<GameEventKind> {
    vec![
        input(InputEvent::MouseMotion { dx: 0.0, dy }),
        input(InputEvent::MouseButton { button, pressed: true }),
        GameEventKind::Tick,
        input(InputEvent::MouseButton { button, pressed: false }),
        GameEventKind::Tick,
    ]
}

/// Straight down (pitch clamps at the pole).
const DOWN: f32 = 1000.0;
/// About 45 degrees down: 302 px * 0.0026 rad/px at the default sensitivity.
const DOWN_45: f32 = 302.0;

fn run(region: &mut Region, events: &[GameEventKind]) {
    for ev in events {
        region.handle_event(ev.clone()).unwrap();
    }
}

/// Every (chunk, block index, block id) of the region's terrain, for diffs.
fn blocks(region: &Region) -> Vec<((usize, i32, usize), usize, BlockId)> {
    let mut out = Vec::new();
    for (c, chunk) in region.terrain_snapshot().chunks {
        for (i, b) in chunk.blocks.iter() {
            out.push(((c.x, c.y, c.z), i.linear(), b.id));
        }
    }
    out
}

/// Blocks whose id differs between two `blocks` listings (new chunks count
/// with their non-air blocks).
fn changed(
    before: &[((usize, i32, usize), usize, BlockId)],
    after: &[((usize, i32, usize), usize, BlockId)],
) -> Vec<((usize, i32, usize), usize, BlockId)> {
    after
        .iter()
        .filter(|a| match before.iter().find(|b| b.0 == a.0 && b.1 == a.1) {
            Some(b) => b.2 != a.2,
            None => a.2 != BlockId::AIR,
        })
        .cloned()
        .collect()
}

#[test]
fn left_click_breaks_the_block_underfoot_and_the_player_falls_in() {
    // Two blocks of floor: every chunk is fully solid, so genesis built it as
    // one cuboid collider that the edit has to turn into voxel shapes.
    let mut region = region(|| Chunk::flat_floor(32));
    // Stand in the middle of block (8, 1, 8), clear of its neighbours.
    teleport(&mut region, 136.0, 136.0);
    let (_, y0, _, _) = player(&region);
    let before = blocks(&region);

    run(&mut region, &click(DOWN, MouseButton::Left));
    let diff = changed(&before, &blocks(&region));
    assert_eq!(diff.len(), 1, "exactly one block must change: {diff:?}");
    // Chunk (4, 0, 4), block (0, 1, 0) = index 2: world block (8, 1, 8).
    assert_eq!(diff[0], ((4, 0, 4), 2, BlockId::AIR));

    for _ in 0..20 {
        region.handle_event(GameEventKind::Tick).unwrap();
    }
    let (_, y1, _, grounded) = player(&region);
    let drop = y0 - y1;
    assert!(
        (drop - BLOCK_VOXELS as f32).abs() < 0.05 && grounded,
        "the player must fall one block into the hole and land: {y0} -> {y1}"
    );
}

#[test]
fn right_click_places_a_block_that_stops_the_player() {
    let mut region = region(|| Chunk::flat_floor(32));
    let before = blocks(&region);
    // 45 degrees down at yaw 0 (facing -z) hits the floor about 1.6 m ahead.
    run(&mut region, &click(DOWN_45, MouseButton::Right));
    let diff = changed(&before, &blocks(&region));
    assert_eq!(diff.len(), 1, "exactly one block must appear: {diff:?}");
    // On top of the two-block floor, in the chunk slot above it, which had
    // no chunk until now.
    let ((_, cy, cz), _, id) = diff[0];
    assert_eq!((cy, id), (1, PLACE_BLOCK), "placed on the floor: {diff:?}");
    assert_eq!(cz, 3, "placed ahead of the player (-z): {diff:?}");

    // Walk forward: the new block is taller than the step height.
    region.handle_event(input(InputEvent::MouseMotion { dx: 0.0, dy: -DOWN_45 })).unwrap();
    region.handle_event(input(InputEvent::Key { key: Key::KeyW, pressed: true })).unwrap();
    for _ in 0..30 {
        region.handle_event(GameEventKind::Tick).unwrap();
    }
    let (_, _, z, _) = player(&region);
    assert!(z > 112.0 && z < 122.0, "the placed block (z 96..112) must stop the walk: z = {z}");
}

#[test]
fn placing_into_the_players_own_body_is_refused() {
    let mut region = region(|| Chunk::flat_floor(16));
    let before = blocks(&region);
    // Straight down: the face above the floor is where the player stands.
    run(&mut region, &click(DOWN, MouseButton::Right));
    assert!(changed(&before, &blocks(&region)).is_empty(), "must not bury the player");
}

#[test]
fn clicking_at_the_sky_changes_nothing() {
    let mut region = region(|| Chunk::flat_floor(16));
    let before = blocks(&region);
    run(&mut region, &click(-DOWN, MouseButton::Left));
    run(&mut region, &click(0.0, MouseButton::Right));
    assert!(changed(&before, &blocks(&region)).is_empty());
}

/// Break underfoot, place ahead, then a click at nothing; roll it all back
/// and replay it.
fn assert_edits_roll_back(floor: fn() -> Chunk) {
    let mut region = region(floor);
    let start = game::state_hash(region.data());
    let terrain = blocks(&region);
    let mut events = click(DOWN, MouseButton::Left);
    // From straight down (-90 degrees) back up to about -45.
    events.extend(click(-DOWN_45, MouseButton::Right));
    events.extend(click(-DOWN, MouseButton::Left));
    for _ in 0..5 {
        events.push(GameEventKind::Tick);
    }
    run(&mut region, &events);
    let end = game::state_hash(region.data());
    let diff = changed(&terrain, &blocks(&region));
    assert_eq!(diff.len(), 2, "one break and one place: {diff:?}");
    // The default hash mode verifies every undo entry as it is rolled back.
    for _ in 0..events.len() {
        region.rollback_last_event();
    }
    assert_eq!(start, game::state_hash(region.data()), "edits must roll back bit-exact");
    run(&mut region, &events);
    assert_eq!(end, game::state_hash(region.data()), "replaying the edits must reproduce them");
}

#[test]
fn edits_roll_back_bit_exact_and_replay_identically() {
    // One-block floor: partial chunks with block-scale voxel colliders; the
    // placed block lands in an existing chunk.
    assert_edits_roll_back(|| Chunk::flat_floor(16));
}

#[test]
fn edits_on_solid_chunks_roll_back_bit_exact() {
    // Two-block floor: genesis cuboid colliders turned into voxel shapes, and
    // the placed block creates a new chunk above.
    assert_edits_roll_back(|| Chunk::flat_floor(32));
}

#[test]
fn clicks_do_nothing_while_the_inventory_is_open() {
    let mut region = region(|| Chunk::flat_floor(16));
    let before = blocks(&region);
    region.handle_event(GameEventKind::UiAction(0, game::UiAction::ToggleInventory)).unwrap();
    run(&mut region, &click(DOWN, MouseButton::Left));
    assert!(changed(&before, &blocks(&region)).is_empty(), "inventory clicks are UI, not edits");
}

// ---- Block-delta edits (SetBlocks) and per-block undo ----

use game::{Block, BlockDelta, BlockIndex, ChiselData, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, Rollback, CHUNK_BLOCKS};

fn air() -> Block {
    Block::new(BlockId::AIR)
}

/// World block position of local block `(bx, by, bz)` in chunk `c`.
fn world_block(c: ChunkCoords, (bx, by, bz): (usize, usize, usize)) -> [i32; 3] {
    let cb = CHUNK_BLOCKS as i32;
    [c.x as i32 * cb + bx as i32, c.y * cb + by as i32, c.z as i32 * cb + bz as i32]
}

/// A rollback-able sim over chunk columns 3..=5 x 3..=5 of `floor`, with its
/// render-update receiver. No player, no ground recipe.
fn terrain(floor: impl Fn(ChunkCoords) -> Chunk) -> (Rollback, crossbeam::channel::Receiver<GameDataUpdate>) {
    let (send, recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    let mut chunks = Vec::new();
    for x in 3..=5 {
        for z in 3..=5 {
            let c = ChunkCoords::new(x, 0, z);
            chunks.push((c, floor(c)));
        }
    }
    r.seed_terrain_genesis(chunks);
    while recv.try_recv().is_ok() {}
    (r, recv)
}

/// Block-, chisel- and air-mixed chunk content from `seed`.
fn seeded_chunk(seed: u128) -> Chunk {
    let mut rng = oorandom::Rand64::new(seed);
    let mut chunk = Chunk::uniform(Block::new(BlockId(2)));
    for i in 0..game::CHUNK_BLOCK_COUNT {
        let (bx, by, bz) = BlockIndex::from_linear(i).xyz();
        let id = BlockId(rng.rand_range(0..4) as _);
        chunk.set_block(bx, by, bz, Block::new(id));
        if id != BlockId::AIR && rng.rand_range(0..3) == 0 {
            chunk.chisel.insert(BlockIndex::from_linear(i), ChiselData::cuboid(id, [0, 0, 0], [16, 8, 16]));
        }
    }
    chunk
}

#[test]
fn dig_chunk_to_air_and_undo() {
    game::set_hash_mode(game::HashMode::Full);
    let target = ChunkCoords::new(4, 0, 4);
    let (mut r, _recv) = terrain(|c| if c == target { seeded_chunk(7) } else { Chunk::flat_floor(16) });
    let before = game::state_hash(&r);
    let e = r.chunk_key_at(target).unwrap();
    assert!(r.ecs.chunk.get(e).blocks.uniform().is_none(), "the seeded chunk is dense");
    r.new_transaction();
    for i in 0..game::CHUNK_BLOCK_COUNT {
        let local = BlockIndex::from_linear(i).xyz();
        let chunk = r.ecs.chunk.get(e);
        if chunk.block_at(local.0, local.1, local.2).id != BlockId::AIR
            || chunk.chisel.contains_key(&BlockIndex::from_linear(i))
        {
            assert!(r.set_block_safe(world_block(target, local), air()));
        }
    }
    let chunk = r.ecs.chunk.get(e);
    assert_eq!(chunk.blocks, game::ChunkBlocks::Uniform(air()), "dug to canonical uniform air");
    assert!(chunk.chisel.is_empty());
    r.rollback();
    assert_eq!(before, game::state_hash(&r), "the dig rolls back bit-exact");
}

#[test]
fn edit_emits_set_blocks() {
    let (mut r, recv) = terrain(|_| Chunk::flat_floor(16));
    let target = ChunkCoords::new(4, 0, 4);
    let e = r.chunk_key_at(target).unwrap();
    let index = BlockIndex::from_xyz(0, 0, 0);
    let old = r.ecs.chunk.get(e).block_at(0, 0, 0);
    assert_ne!(old.id, BlockId::AIR);
    r.new_transaction();
    assert!(r.set_block_safe(world_block(target, (0, 0, 0)), air()));
    let done: Vec<GameDataUpdate> = recv.try_iter().collect();
    assert_eq!(done.len(), 1, "one render update per edit: {done:?}");
    match &done[0] {
        GameDataUpdate {
            do_kind: GameDataTransactionKind::Do,
            update_kind: GameDataUpdateKind::SetBlocks(k, deltas),
        } => {
            assert_eq!(*k, e);
            assert_eq!(deltas, &vec![BlockDelta { index, block: air(), chisel: None }]);
        }
        other => panic!("expected Do SetBlocks, got {other:?}"),
    }
    r.rollback();
    let undone: Vec<GameDataUpdate> = recv.try_iter().collect();
    assert_eq!(undone.len(), 1, "one render update per undone edit: {undone:?}");
    match &undone[0] {
        GameDataUpdate {
            do_kind: GameDataTransactionKind::Undo,
            update_kind: GameDataUpdateKind::SetBlocks(k, deltas),
        } => {
            assert_eq!(*k, e);
            assert_eq!(deltas, &vec![BlockDelta { index, block: old, chisel: None }]);
        }
        other => panic!("expected Undo SetBlocks, got {other:?}"),
    }
}

/// One band chunk (sub-Y 0, fully stone) over plain implied ground.
fn ground_column() -> game::Column {
    const STONE: BlockId = BlockId(2);
    const BEDROCK: BlockId = BlockId(3);
    let band = game::SubChunk::Dense(Box::new(Chunk::flat_floor_with(32, STONE)));
    game::Column::new(0, Block::new(BlockId(1)), air(), vec![band]).with_ground(game::ColumnGround::plain(
        vec![1; game::COLUMN_FOOTPRINT].into_boxed_slice(),
        0,
        STONE,
        BEDROCK,
    ))
}

#[test]
fn break_on_chunk_seam_undoes_exactly() {
    game::set_hash_mode(game::HashMode::Full);
    let mut columns = Vec::new();
    for x in 2..=3 {
        for z in 2..=3 {
            columns.push((game::ColumnCoords::new(x, z), ground_column()));
        }
    }
    let built = Region::from_columns(r0(), columns);
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(built.data().clone(), Some(send), r0(), None);
    // The top block of implied chunk (2, -1, 2) on its +x face: the +x
    // neighbour (3, -1, 2) is implied ground too and materialises with it.
    let own = ChunkCoords::new(2, -1, 2);
    let neighbour = ChunkCoords::new(3, -1, 2);
    let pos = world_block(own, (CHUNK_BLOCKS - 1, CHUNK_BLOCKS - 1, 0));
    region.with_data(|r| {
        assert!(r.chunk_key_at(own).is_none() && r.chunk_key_at(neighbour).is_none());
        let before = game::state_hash(r);
        r.new_transaction();
        assert!(r.set_block_safe(pos, air()));
        assert!(r.chunk_key_at(own).is_some(), "its own chunk materialises");
        assert!(r.chunk_key_at(neighbour).is_some(), "the seam neighbour materialises");
        r.rollback();
        assert_eq!(before, game::state_hash(r), "the seam break rolls back bit-exact");
        assert!(r.chunk_key_at(own).is_none() && r.chunk_key_at(neighbour).is_none());
    });
}
