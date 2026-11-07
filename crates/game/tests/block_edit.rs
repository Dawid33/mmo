//! Left click breaks the block under the crosshair, right click places one
//! against the face it hit. Edits run inside the tick from the player's eye
//! and look, so they must change terrain, collision and state hash exactly
//! as the tick's other mutations do: bit-exact under rollback and identical
//! on replay.
use game::edit::PLACE_BLOCK;
use game::{
    BlockId, Chunk, ChunkCoords, GameEventKind, InputEvent, Pose, Key, MouseButton, Region,
    RegionCoords, BLOCK_VOXELS, CHUNK_UNITS,
};

/// Chunk column holding the player spawn (128, 128): chunk 4 at 2-block
/// chunks, chunk 0 at 16.
const SPAWN_CHUNK: usize = 128 / CHUNK_UNITS;

fn r0() -> RegionCoords {
    RegionCoords::new(0, 0)
}

/// The chunk columns around the spawn chunk (3..=5 at 2-block chunks, 0..=1
/// at 16) of `floor`, with player 0 spawned and settled at (128, 128),
/// controls on. Setup events are forgotten, so only what a
/// test does afterwards can be rolled back.
fn region(floor: impl Fn() -> Chunk) -> Region {
    crate::common::install_real_block_props();
    let mut chunks = Vec::new();
    let around = SPAWN_CHUNK.saturating_sub(1)..=SPAWN_CHUNK + 1;
    for x in around.clone() {
        for z in around.clone() {
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
    let before: std::collections::HashMap<_, _> = before.iter().map(|b| ((b.0, b.1), b.2)).collect();
    after
        .iter()
        .filter(|a| match before.get(&(a.0, a.1)) {
            Some(&b) => b != a.2,
            None => a.2 != BlockId::AIR,
        })
        .cloned()
        .collect()
}

/// World block of a `changed` entry.
fn world_of(((cx, cy, cz), i, _): ((usize, i32, usize), usize, BlockId)) -> [i32; 3] {
    world_block(ChunkCoords::new(cx, cy, cz), BlockIndex::from_linear(i).xyz())
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
    // World block (8, 1, 8): chunk (4, 0, 4) block index 2 at 2-block chunks.
    assert_eq!((world_of(diff[0]), diff[0].2), ([8, 1, 8], BlockId::AIR), "{diff:?}");

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
fn slope_breaks_like_a_half_block() {
    // Block (8, 1, 8) is a straight slope (low side +x) instead of a whole block.
    const CB: usize = game::CHUNK_BLOCKS;
    let slope = || {
        let mut c = Chunk::flat_floor(32);
        let dirt = BlockId(1);
        c.set_block(8 % CB, 1 % CB, 8 % CB, game::Block::new(dirt));
        c.chisel.insert(
            game::BlockIndex::from_xyz(8 % CB, 1 % CB, 8 % CB),
            game::slope_chisel(dirt, game::SlopeShape::Straight { low: game::Side::PosX }),
        );
        c
    };
    let mut region = region(slope);
    teleport(&mut region, 136.0, 136.0);
    let before = blocks(&region);
    run(&mut region, &click(DOWN, MouseButton::Left));
    let diff = changed(&before, &blocks(&region));
    assert_eq!(diff.len(), 1, "exactly one block must change: {diff:?}");
    assert_eq!((world_of(diff[0]), diff[0].2), ([8, 1, 8], BlockId::AIR), "{diff:?}");
    let (coords, bi) = (diff[0].0, game::BlockIndex::from_linear(diff[0].1));
    let snap = region.terrain_snapshot();
    let chunk = &snap.chunks.iter().find(|(c, _)| (c.x, c.y, c.z) == coords).unwrap().1;
    assert!(!chunk.chisel.contains_key(&bi), "the broken slope leaves no chisel behind");
}

#[test]
fn right_click_places_a_block_that_stops_the_player() {
    let mut region = region(|| Chunk::flat_floor(32));
    let before = blocks(&region);
    // 45 degrees down at yaw 0 (facing -z) hits the floor about 1.6 m ahead.
    run(&mut region, &click(DOWN_45, MouseButton::Right));
    let diff = changed(&before, &blocks(&region));
    assert_eq!(diff.len(), 1, "exactly one block must appear: {diff:?}");
    // On top of the two-block floor (world block y 2: at 2-block chunks the
    // chunk slot above it, which had no chunk until now), ahead of the
    // player (-z) at block z 6.
    let [_, y, z] = world_of(diff[0]);
    assert_eq!((y, diff[0].2), (2, PLACE_BLOCK), "placed on the floor: {diff:?}");
    assert_eq!(z, 6, "placed ahead of the player (-z): {diff:?}");

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

/// The edited chunk of the `terrain` tests: the middle of its 3×3.
const TARGET: ChunkCoords = ChunkCoords { x: 1, y: 0, z: 1 };

/// A rollback-able sim over chunk columns 0..=2 x 0..=2 of `floor`, with its
/// render-update receiver. No player, no ground recipe.
fn terrain(floor: impl Fn(ChunkCoords) -> Chunk) -> (Rollback, crossbeam::channel::Receiver<GameDataUpdate>) {
    crate::common::install_real_block_props();
    let (send, recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    let mut chunks = Vec::new();
    for x in 0..=2 {
        for z in 0..=2 {
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

/// Slow at 16-block chunks: 4096 edits, each undo entry verified by a full
/// state hash on rollback (the edits alone take ~0.1 s).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn dig_chunk_to_air_and_undo() {
    game::set_hash_mode(game::HashMode::Full);
    let target = TARGET;
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
    let target = TARGET;
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

/// `StateHash` of each enabled collider shape on chunk `e`'s body, in order,
/// with its pose relative to the body.
fn collider_shapes(r: &Rollback, e: game::EntityKey) -> Vec<(u32, Option<Pose>)> {
    use game::StateHash;
    let body = r.physics.bodies.get(*r.ecs.rigidbody.get(e)).unwrap();
    body.colliders()
        .iter()
        .map(|&ch| r.physics.colliders.get(ch).unwrap())
        .filter(|c| c.is_enabled())
        .map(|c| {
            let mut h = game::StateHasher::new();
            c.shared_shape().state_hash(&mut h);
            (h.finalize(), c.position_wrt_parent().copied())
        })
        .collect()
}

/// A block edit patches the chunk's `Voxels` shapes in place (or falls back
/// to the rebuild); either way the colliders must be the ones `create_mesh`
/// builds for the same chunk, and the whole sequence rolls back bit-exact.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn incremental_collider_matches_rebuild() {
    game::set_hash_mode(game::HashMode::Full);
    let target = TARGET;
    for (seed, start) in [(11u128, seeded_chunk(5)), (12, Chunk::flat_floor(16)), (13, Chunk::flat_floor(32))] {
        let (mut r, _recv) = terrain(|c| if c == target { start.clone() } else { Chunk::flat_floor(16) });
        let before = game::state_hash(&r);
        let e = r.chunk_key_at(target).unwrap();
        let mut rng = oorandom::Rand64::new(seed);
        r.new_transaction();
        let mut edits = 0;
        for _ in 0..50 {
            let i = rng.rand_range(0..game::CHUNK_BLOCK_COUNT as u64) as usize;
            let local = BlockIndex::from_linear(i).xyz();
            let block = if rng.rand_range(0..2) == 0 { air() } else { Block::new(BlockId(rng.rand_range(1..3) as _)) };
            if !r.set_block_safe(world_block(target, local), block) {
                continue;
            }
            edits += 1;
            let mut fresh = Rollback::new(None);
            fresh.new_transaction();
            let fe = fresh.create_mesh(target, r.ecs.chunk.get(e).clone());
            assert_eq!(
                collider_shapes(&r, e),
                collider_shapes(&fresh, fe),
                "seed {seed}, edit {edits} at {local:?} -> {block:?}"
            );
        }
        assert!(edits > 10, "seed {seed}: only {edits} edits changed anything");
        r.rollback();
        assert_eq!(before, game::state_hash(&r), "seed {seed}: the edits roll back bit-exact");
    }
}

/// Pins that a block edit takes the incremental path: in a partial chunk
/// built by `create_mesh`, breaking one block keeps the same collider handles
/// (none added, none disabled), changes the block shape in place, and never
/// calls the full rebuild (`Rollback::collider_rebuilds`, a thread-local
/// probe that is not state).
#[test]
fn partial_chunk_edit_patches_colliders_without_rebuild() {
    use game::StateHash;
    let target = TARGET;
    let (mut r, _recv) = terrain(|c| if c == target { seeded_chunk(5) } else { Chunk::flat_floor(16) });
    let e = r.chunk_key_at(target).unwrap();
    let body = r.physics.bodies.get(*r.ecs.rigidbody.get(e)).unwrap();
    let handles: Vec<_> = body.colliders().to_vec();
    let state = |r: &Rollback| -> Vec<(bool, u32)> {
        let body = r.physics.bodies.get(*r.ecs.rigidbody.get(e)).unwrap();
        body.colliders()
            .iter()
            .map(|&ch| {
                let c = r.physics.colliders.get(ch).unwrap();
                let mut h = game::StateHasher::new();
                c.shared_shape().state_hash(&mut h);
                (c.is_enabled(), h.finalize())
            })
            .collect()
    };
    let before = state(&r);
    assert!(before.len() >= 2 && before.iter().all(|(on, _)| *on), "a multi-grid partial chunk");
    // A plain (unchiselled) solid block: the block grid keeps other cells.
    let chunk = r.ecs.chunk.get(e);
    let local = (0..game::CHUNK_BLOCK_COUNT)
        .map(|i| BlockIndex::from_linear(i))
        .find(|&i| chunk.block_at(i.xyz().0, i.xyz().1, i.xyz().2).id != BlockId::AIR && !chunk.chisel.contains_key(&i))
        .expect("a plain solid block")
        .xyz();
    let rebuilds = Rollback::collider_rebuilds();
    r.new_transaction();
    assert!(r.set_block_safe(world_block(target, local), air()));
    assert_eq!(Rollback::collider_rebuilds(), rebuilds, "the edit patched in place");
    let body = r.physics.bodies.get(*r.ecs.rigidbody.get(e)).unwrap();
    assert_eq!(body.colliders(), &handles[..], "no collider added");
    let after = state(&r);
    assert!(after.iter().all(|(on, _)| *on), "none disabled");
    assert_ne!(before, after, "the shape changed");
}

// ---- Across a seam (Task 6.2): edits are intents to the block's owner ----

use crate::common::rig::{Rig, CLIENT};
use crate::common::seam_translate::{offset, spawn_tile, uniform_floor, Axis, Placement};
use game::{ColumnCoords, Column, ColumnGround, InputEvent as Input, ItemId, ItemStack, Order, PlayMode, WorldBlockPos, SPAWN_REGION};

const STONE: BlockId = BlockId(2);
/// The grounded floor's bedrock (any id distinct from the strata).
const BEDROCK: BlockId = BlockId(6);
/// Surface block y of the grounded floor: its band is one chunk (blocks 0
/// and 1 stone); everything below block 0 is implied ground.
const SURFACE: i16 = 1;

/// Every region: one stone band chunk per column over plain implied ground.
fn grounded_floor(_rc: RegionCoords) -> Vec<(ColumnCoords, Column)> {
    let mut out = Vec::new();
    for x in 0..game::REGION_CHUNKS {
        for z in 0..game::REGION_CHUNKS {
            let band = game::SubChunk::Dense(Box::new(Chunk::flat_floor_with(32, STONE)));
            let ground =
                ColumnGround::plain(vec![SURFACE; game::COLUMN_FOOTPRINT].into_boxed_slice(), 0, STONE, BEDROCK);
            out.push((ColumnCoords::new(x, z), Column::new(0, Block::new(BlockId(1)), air(), vec![band]).with_ground(ground)));
        }
    }
    out
}

/// The block at world `p` as region `rc` sees it (own or halo).
fn seen_from(rig: &mut Rig, rc: RegionCoords, p: WorldBlockPos) -> Option<BlockId> {
    let mut out = None;
    rig.manager.spawner_mut().with_region(rc, |r| out = r.data().halo_block(p).map(|b| b.id));
    out
}

/// The outermost block ring is diggable below the surface: the owner digs
/// its own block; the neighbour sees the cavity in its halo and materialises
/// its own implied ground behind it (the new air never borders ground that
/// has no chunk), and the halo reads the neighbour's unmaterialised ground
/// as its rock.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn dig_edge_ring_below_surface() {
    let mut rig = Rig::with_floor(None, grounded_floor);
    rig.join_spawn();
    let west = RegionCoords::new(SPAWN_REGION.x - 1, SPAWN_REGION.z);
    let z = 10;
    let y = i32::from(SURFACE) - 3;
    let dug = WorldBlockPos::from_local(SPAWN_REGION, game::BlockPos::new(0, y, z));
    let behind = WorldBlockPos::new(dug.x - 1, y, dug.z);
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        r.with_data(|d| assert!(d.set_block_safe([0, y, z], air()), "the edge ring digs below the surface"));
    });
    for _ in 0..4 {
        rig.round();
    }
    assert_eq!(seen_from(&mut rig, SPAWN_REGION, dug), Some(BlockId::AIR));
    assert_eq!(seen_from(&mut rig, west, dug), Some(BlockId::AIR), "the neighbour's halo shows the cavity");
    assert_eq!(seen_from(&mut rig, west, behind), Some(STONE), "the neighbour's ground next to it is solid");
    assert_eq!(seen_from(&mut rig, SPAWN_REGION, behind), Some(STONE), "and reads solid through the halo");
    // The owner materialised it: the cavity is bordered by a real chunk.
    let cb = CHUNK_BLOCKS as i32;
    let (_, local) = behind.to_local();
    let chunk = ChunkCoords::new((local.x / cb) as usize, y.div_euclid(cb), (local.z / cb) as usize);
    let mut materialised = false;
    rig.manager.spawner_mut().with_region(west, |r| materialised = r.data().chunk_key_at(chunk).is_some());
    assert!(materialised, "the neighbour materialised {chunk:?} behind the cavity");
    // Implied ground the neighbour has not materialised reads as its rock
    // through the halo, not as air.
    let deep = WorldBlockPos::new(dug.x - 1, y - 20, dug.z);
    assert_eq!(seen_from(&mut rig, SPAWN_REGION, deep), Some(STONE), "unmaterialised halo ground is solid");
}

/// The owner (`p`'s region) block id at `p`, and the player's slot 0.
fn place_state(rig: &mut Rig, p: WorldBlockPos) -> (BlockId, Option<ItemStack>) {
    let (rc, local) = p.to_local();
    let mut id = None;
    rig.manager.spawner_mut().with_region(rc, |r| {
        id = Some(r.data().block_at([local.x, local.y, local.z]).map_or(BlockId::AIR, |b| b.id));
    });
    let home = rig.owners()[0];
    let mut slot = None;
    rig.manager.spawner_mut().with_region(home, |r| slot = r.data().clients[&CLIENT].inventory.slots[0]);
    (id.expect("the owner runs"), slot)
}

/// A survival right click on a block across the seam places into the
/// neighbour: the owner has the block at T+1 and the placer's slot lost one
/// item at the click. A place the owner refuses gives the item back.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn place_block_across_seam() {
    let mut rig = Rig::with_floor(None, uniform_floor);
    rig.join_spawn();
    let origin = Placement::Seam(Axis::X).origin();
    rig.order(Order::SetMode { mode: PlayMode::Adventure, at: Some(spawn_tile(offset(origin, 2, 0, 0))) });
    rig.input(Input::FpsCam(true));
    for _ in 0..6 {
        rig.round();
    }
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        r.with_data(|d| {
            let c = d.clients.get_mut(&CLIENT).expect("the client");
            c.inventory.slots[0] = Some(ItemStack { item: ItemId::block(STONE), count: 3 });
            c.selected = 0;
        })
    });
    let floor = offset(origin, -1, -1, 0);
    rig.aim_at([floor.x as f64 + 0.5, floor.y as f64 + 1.0, floor.z as f64 + 0.5]);
    assert_eq!(rig.eye_hit(), Some((floor, [0, 1, 0])), "the crosshair is on the floor across the seam");
    let at = offset(floor, 0, 1, 0);
    let stone = |n| Some(ItemStack { item: ItemId::block(STONE), count: n });
    rig.input(Input::MouseButton { button: MouseButton::Right, pressed: true });
    rig.round();
    rig.input(Input::MouseButton { button: MouseButton::Right, pressed: false });
    assert_eq!(place_state(&mut rig, at), (BlockId::AIR, stone(2)), "tick T sent it and took the item");
    rig.round();
    assert_eq!(place_state(&mut rig, at), (STONE, stone(2)), "the owner placed it at T+1");
    for _ in 0..3 {
        rig.round();
    }
    assert_eq!(place_state(&mut rig, at), (STONE, stone(2)), "granted: nothing given back");

    // Refused: the owner's cell fills between the send and T+1.
    rig.aim_at([at.x as f64 + 0.5, at.y as f64 + 1.0, at.z as f64 + 0.5]);
    let above = offset(at, 0, 1, 0);
    rig.input(Input::MouseButton { button: MouseButton::Right, pressed: true });
    rig.round();
    rig.input(Input::MouseButton { button: MouseButton::Right, pressed: false });
    assert_eq!(place_state(&mut rig, above), (BlockId::AIR, stone(1)));
    let (rc, local) = above.to_local();
    rig.manager.spawner_mut().with_region(rc, |r| {
        r.with_data(|d| assert!(d.set_block_safe([local.x, local.y, local.z], Block::new(BlockId(1)))));
    });
    for _ in 0..3 {
        rig.round();
    }
    assert_eq!(place_state(&mut rig, above), (BlockId(1), stone(2)), "refused at the owner: the item came back");
}

/// Chunk-local block rows `0..POND_ROWS` (z) of [`pond_floor`] hold water;
/// the player stands dry past them, at the spawn (block z 8), and looks
/// toward −z over the pond.
const POND_ROWS: usize = 7;

/// The floor chunk with a pond: stone to block y 2, water in the two blocks
/// above over rows `0..POND_ROWS`. The player must stand dry: two blocks of
/// water are deep enough to swim in (`Immersion::Waist`), and a swimmer is
/// never grounded, which the fixture's settle check needs.
fn pond_floor() -> Chunk {
    let mut chunk = Chunk::flat_floor(2 * BLOCK_VOXELS as u32);
    for by in 2..4 {
        for bx in 0..CHUNK_BLOCKS {
            for bz in 0..POND_ROWS {
                chunk.set_block(bx, by, bz, game::Block::new(BlockId(5)));
            }
        }
    }
    chunk
}

#[test]
fn edit_ray_passes_through_water() {
    // Placing is a ray hit on the floor block's top face: the water above it
    // does not stop the ray, and the placed block replaces the water cell.
    let mut region = region(pond_floor);
    let before = blocks(&region);
    run(&mut region, &click(DOWN_45, MouseButton::Right));
    let diff = changed(&before, &blocks(&region));
    assert_eq!(diff.len(), 1, "exactly one block must change: {diff:?}");
    let [_, y, z] = world_of(diff[0]);
    assert_eq!((y, diff[0].2), (2, PLACE_BLOCK), "the block replaces the water cell above the floor: {diff:?}");
    assert_eq!(z, 6, "{diff:?}");
}
