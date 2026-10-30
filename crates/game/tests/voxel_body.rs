//! Voxel bodies (`game::voxel_body`): a set of terrain blocks detached into
//! one dynamic rigid body that falls and is despawned once it rests. The
//! detach and every tick of the fall must roll back bit-exact, replay
//! identically, and a body leaving the region must vanish rather than be
//! handed to the neighbour as a (capsule) entity.
mod common;

use common::{floor_chunks, r0};
use crossbeam::channel::Receiver;
use game::rapier3d::math::Vector;
use game::voxel_body::{
    model_mass_properties, SettlePolicy, VoxelModel, LEAF_DENSITY, REST_TICKS, SETTLE_TIMEOUT_TICKS,
    STONE_DENSITY, WOOD_DENSITY,
};
use game::{
    install_block_props, state_hash, Block, BlockId, BlockRegistry, EntityKey, GameDataUpdate,
    GameDataUpdateKind, GameEventKind, Region, Rollback, BLOCK_VOXELS, REGION_BLOCKS, REGION_SIZE,
};

const MANIFEST: &str = include_str!("../../../assets/blocks/blocks.ron");
const STONE: BlockId = BlockId(2);
const B: f32 = BLOCK_VOXELS as f32;

/// The shipped registry with its props installed (leaves non-solid, wood on
/// the half-block collider grid). Process-global and idempotent.
fn registry() -> BlockRegistry {
    let reg = BlockRegistry::from_ron(MANIFEST).unwrap();
    install_block_props(reg.props());
    reg
}

/// A region floored one block deep over block columns `x_blocks` × 0..32,
/// no players, with its render-update receiver (drained).
fn region_over(x_blocks: std::ops::Range<usize>) -> (Region, Receiver<GameDataUpdate>) {
    registry();
    let (send, recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), r0(), None);
    let chunks = floor_chunks(x_blocks.start as f32 * B..x_blocks.end as f32 * B, 0.0..32.0 * B, BLOCK_VOXELS as u32);
    region.with_data(|r| r.seed_terrain_genesis(chunks));
    while recv.try_recv().is_ok() {}
    (region, recv)
}

/// A 1×`h`×1 stone column standing on the floor at block column (x, z).
fn column(x: i32, z: i32, h: i32) -> Vec<[i32; 3]> {
    (1..=h).map(|y| [x, y, z]).collect()
}

/// Places `block` at every position, in a forgotten transaction.
fn build(region: &mut Region, positions: &[[i32; 3]], block: Block) {
    region.with_data(|r| {
        for &p in positions {
            assert!(r.set_block_safe(p, block), "build {p:?}");
        }
    });
}

/// Detaches `positions` in a forgotten transaction.
fn detach(region: &mut Region, positions: &[[i32; 3]], angvel: Vector) -> EntityKey {
    let mut e = None;
    region.with_data(|r| e = r.detach_blocks_safe(positions, SettlePolicy::Despawn, angvel));
    e.expect("something to detach")
}

fn tick(region: &mut Region) {
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
}

/// The block the sim holds at `pos` (air for an empty slot).
fn block_at(r: &Rollback, pos: [i32; 3]) -> Block {
    let (coords, (x, y, z)) = game::edit::locate(pos).unwrap();
    r.chunk_key_at(coords)
        .and_then(|e| r.ecs.chunk.try_get(e).as_ref().map(|c| c.block_at(x, y, z)))
        .unwrap_or(Block::new(BlockId::AIR))
}

fn alive(r: &Rollback, e: EntityKey) -> bool {
    r.ecs.entities.contains_key(e)
}

fn body_pose(r: &Rollback, e: EntityKey) -> game::Pose {
    *r.physics.bodies.get(*r.ecs.rigidbody.get(e)).unwrap().position()
}

#[test]
fn detach_clears_terrain_and_spawns_body() {
    let (mut region, recv) = region_over(0..32);
    let col = column(16, 16, 6);
    build(&mut region, &col, Block::new(STONE));
    while recv.try_recv().is_ok() {}
    let e = detach(&mut region, &col, Vector::ZERO);
    let r = region.data();
    for &p in &col {
        assert_eq!(block_at(r, p).id, BlockId::AIR, "{p:?} cleared");
    }
    assert_ne!(block_at(r, [16, 0, 16]).id, BlockId::AIR, "the floor under it stays");
    let body = r.physics.bodies.get(*r.ecs.rigidbody.get(e)).unwrap();
    assert!(body.is_dynamic(), "a dynamic body");
    assert_eq!(r.ecs.kind.try_get(e), &None, "no EntityKind: never transferred");
    let vb = r.ecs.voxel_body.get(e);
    assert_eq!(vb.model.blocks.len(), 6);
    assert_eq!(vb.origin, [16, 1, 16]);
    // Body at the centre of mass: the middle of the column.
    let t = body.translation();
    assert_eq!([t.x, t.y, t.z], [16.5 * B, 4.0 * B, 16.5 * B]);
    assert!((model_mass_properties(&vb.model).mass() - 6.0 * STONE_DENSITY).abs() < 0.01);
    let added = recv.try_iter().find_map(|u| match u.update_kind {
        GameDataUpdateKind::AddVoxelBody(k, pose, model) => Some((k, pose, model)),
        _ => None,
    });
    let (k, pose, model) = added.expect("AddVoxelBody emitted");
    assert_eq!((k, pose, *model), (e, *body.position(), vb.model.clone()));
    // Rapier folds the explicit mass in on the first step.
    tick(&mut region);
    let r = region.data();
    let mass = r.physics.bodies.get(*r.ecs.rigidbody.get(e)).unwrap().mass();
    assert!((mass - 6.0 * STONE_DENSITY).abs() < 0.01, "mass {mass}");
}

#[test]
fn detach_of_air_only_changes_nothing() {
    let (mut region, _recv) = region_over(0..32);
    let before = state_hash(region.data());
    let mut out = Some(EntityKey::default());
    region.with_data(|r| out = r.detach_blocks_safe(&column(16, 16, 3), SettlePolicy::Despawn, Vector::ZERO));
    assert_eq!(out, None);
    assert_eq!(before, state_hash(region.data()));
}

/// Ticks until `e` is gone (at most `limit`), tracking its lowest COM y.
/// Returns (ticks taken, lowest y, last still_ticks seen, last x).
fn run_until_gone(region: &mut Region, e: EntityKey, limit: u64, mut each: impl FnMut(&mut Region)) -> Option<(u64, f32, u16, f32)> {
    let mut min_y = f32::INFINITY;
    let mut still = 0;
    let mut last_x = 0.0;
    for t in 0..limit {
        tick(region);
        each(region);
        let r = region.data();
        if !alive(r, e) {
            assert!(r.ecs.rigidbody.try_get(e).is_none() && r.ecs.voxel_body.try_get(e).is_none());
            return Some((t + 1, min_y, still, last_x));
        }
        let p = body_pose(r, e);
        min_y = min_y.min(p.translation.y);
        last_x = p.translation.x;
        still = r.ecs.voxel_body.get(e).still_ticks;
    }
    None
}

#[test]
fn column_topples_and_despawns() {
    let (mut region, _recv) = region_over(0..32);
    let col = column(16, 16, 6);
    build(&mut region, &col, Block::new(STONE));
    // Toppling about the middle of its base (a spin about its centre of
    // mass alone grinds its foot into the floor and stops).
    let pivot = Vector::new(16.5 * B, B, 16.5 * B);
    let mut e = None;
    region.with_data(|r| e = r.detach_blocks_pivoting_safe(&col, SettlePolicy::Despawn, Vector::new(0.0, 0.0, 1.5), Some(pivot)));
    let e = e.expect("a body");
    let v = region.data().physics.bodies.get(*region.data().ecs.rigidbody.get(e)).unwrap().linvel();
    // The rigid motion about the pivot: the centre of mass (3 blocks up)
    // moves at ω × r = 1.5 × 3 blocks, toward −x.
    assert!((v.x + 1.5 * 3.0 * B).abs() < 1e-3 && v.y.abs() < 1e-3 && v.z.abs() < 1e-3, "{v:?}");
    let start_y = body_pose(region.data(), e).translation.y;
    let (ticks, min_y, _, _) =
        run_until_gone(&mut region, e, SETTLE_TIMEOUT_TICKS + 5, |_| {}).expect("the column despawns");
    assert!(ticks < SETTLE_TIMEOUT_TICKS, "it came to rest ({ticks} ticks), not timed out");
    // It falls, then rests REST_TICKS (at the world's gravity a 6-block
    // column is down in a few ticks).
    assert!(ticks > u64::from(REST_TICKS) + 2, "a fall takes time: {ticks} ticks");
    assert!(start_y - min_y > 2.0 * B, "it fell: {start_y} -> {min_y}");
    assert!(region.data().ecs.voxel_body.iter().all(|(_, v)| v.is_none()));
}

/// Serialises the tests that switch the process-global hash mode and roll
/// back: an entry recorded in one mode and verified in another would fail.
static MODE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Holds [`MODE_LOCK`] in `mode`; back to the default Full on drop.
struct Mode(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
impl Mode {
    fn set(mode: game::HashMode) -> Self {
        let g = MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        game::set_hash_mode(mode);
        Mode(g)
    }
}
impl Drop for Mode {
    fn drop(&mut self) {
        game::set_hash_mode(game::HashMode::Full);
    }
}

#[test]
fn rollback_of_detach_is_bit_exact() {
    let _mode = Mode::set(game::HashMode::Full);
    registry();
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    r.seed_terrain_genesis(floor_chunks(0.0..32.0 * B, 0.0..32.0 * B, BLOCK_VOXELS as u32));
    let col = column(16, 16, 6);
    r.new_transaction();
    for &p in &col {
        assert!(r.set_block_safe(p, Block::new(STONE)));
    }
    r.forget();
    let before = state_hash(&r);
    r.new_transaction();
    let e = r.detach_blocks_safe(&col, SettlePolicy::Despawn, Vector::new(0.0, 0.0, 1.0));
    assert!(e.is_some());
    assert_ne!(before, state_hash(&r));
    r.rollback();
    assert_eq!(before, state_hash(&r), "detach rolls back bit-exact");
    // Delta (per-entry footprint digests) must verify and invert too.
    game::set_hash_mode(game::HashMode::Delta);
    r.new_transaction();
    assert!(r.detach_blocks_safe(&col, SettlePolicy::Despawn, Vector::new(0.0, 0.0, 1.0)).is_some());
    r.rollback();
    assert_eq!(before, state_hash(&r), "detach rolls back bit-exact under Delta");
}

#[test]
fn rollback_mid_fall_is_bit_exact() {
    let _mode = Mode::set(game::HashMode::Full);
    let (mut region, _recv) = region_over(0..32);
    let col = column(16, 16, 6);
    build(&mut region, &col, Block::new(STONE));
    let e = detach(&mut region, &col, Vector::new(0.0, 0.0, 1.5));
    for _ in 0..2 {
        tick(&mut region);
    }
    let before = state_hash(region.data());
    // Short of the column's despawn (about 14 ticks at the world's gravity).
    const N: usize = 6;
    for _ in 0..N {
        region.handle_event(GameEventKind::Tick).unwrap();
    }
    let after = state_hash(region.data());
    assert_ne!(before, after, "the body moved");
    for _ in 0..N {
        region.rollback_last_event();
    }
    assert_eq!(before, state_hash(region.data()), "the fall rolls back bit-exact");
    for _ in 0..N {
        region.handle_event(GameEventKind::Tick).unwrap();
    }
    assert_eq!(after, state_hash(region.data()), "replaying the fall reproduces it");
    assert!(alive(region.data(), e), "still falling");
    // And through the despawn: roll back the tick that removed the body.
    loop {
        let pre = state_hash(region.data());
        region.handle_event(GameEventKind::Tick).unwrap();
        if !alive(region.data(), e) {
            region.rollback_last_event();
            assert_eq!(pre, state_hash(region.data()), "the despawn rolls back bit-exact");
            assert!(alive(region.data(), e));
            break;
        }
        region.forget_last_event();
    }
}

#[test]
fn same_inputs_same_end_pose() {
    let run = || {
        let (mut region, _recv) = region_over(0..32);
        let col = column(16, 16, 6);
        build(&mut region, &col, Block::new(STONE));
        let e = detach(&mut region, &col, Vector::new(0.3, 0.0, 1.5));
        let mut poses = Vec::new();
        run_until_gone(&mut region, e, SETTLE_TIMEOUT_TICKS + 5, |reg| {
            let r = reg.data();
            if alive(r, e) {
                let p = body_pose(r, e);
                let (t, q) = (p.translation, p.rotation);
                poses.push([t.x, t.y, t.z, q.x, q.y, q.z, q.w].map(f32::to_bits));
            }
        })
        .expect("despawns");
        (poses, state_hash(region.data()))
    };
    let (a, ha) = run();
    let (b, hb) = run();
    assert!(a.len() > 10);
    assert_eq!(a, b, "identical pose at every tick");
    assert_eq!(ha, hb);
}

#[test]
fn body_leaving_region_is_despawned_not_transferred() {
    let edge = REGION_BLOCKS as i32 - 1;
    let (mut region, _recv) = region_over(REGION_BLOCKS - 32..REGION_BLOCKS);
    let col = column(edge, 16, 6);
    build(&mut region, &col, Block::new(STONE));
    // Tips toward +x, over the region edge.
    let e = detach(&mut region, &col, Vector::new(0.0, 0.0, -2.0));
    let (ticks, _, still, last_x) = run_until_gone(&mut region, e, SETTLE_TIMEOUT_TICKS + 5, |reg| {
        let (departures, ghosts) = reg.take_transfers();
        assert!(departures.is_empty() && ghosts.is_empty(), "a voxel body never crosses the seam");
    })
    .expect("despawned");
    assert!(ticks < SETTLE_TIMEOUT_TICKS && still < REST_TICKS, "left the region, not rested/timed out");
    assert!(last_x > REGION_SIZE - B, "it was leaving over +x: last x {last_x}");
}

#[test]
fn mass_properties_are_diagonal_and_positive() {
    registry();
    // An L: three stone blocks along x, one up at the far end.
    let mut model = VoxelModel::default();
    for o in [[0, 0, 0], [1, 0, 0], [2, 0, 0], [2, 1, 0]] {
        model.blocks.insert(o, (Block::new(STONE), None));
    }
    let mp = model_mass_properties(&model);
    assert!((mp.mass() - 4.0 * STONE_DENSITY).abs() < 0.01, "mass {}", mp.mass());
    // COM: x = (0.5 + 1.5 + 2.5 + 2.5) / 4 blocks, y = (0.5*3 + 1.5) / 4.
    let c = mp.local_com;
    assert_eq!([c.x, c.y, c.z], [1.75 * B, 0.75 * B, 0.5 * B]);
    assert_eq!(mp.principal_inertia_local_frame, game::math::Quat::IDENTITY, "no eigen solve: model axes");
    let inv = mp.inv_principal_inertia;
    assert!(inv.x > 0.0 && inv.y > 0.0 && inv.z > 0.0, "{inv:?}");
    // A single block: the box formula m (a² + a²) / 12 per axis.
    let mut one = VoxelModel::default();
    one.blocks.insert([0, 0, 0], (Block::new(STONE), None));
    let i = 1.0 / model_mass_properties(&one).inv_principal_inertia.x;
    let want = STONE_DENSITY * 2.0 * B * B / 12.0;
    assert!((i - want).abs() <= want * 1e-6, "{i} vs {want}");
}

#[test]
fn falling_leaves_collide_but_add_no_collider_mass() {
    let reg = registry();
    let log = Block::new(reg.id_of("oak_log").unwrap());
    let leaves = Block::new(reg.id_of("oak_leaves").unwrap());
    let (mut region, _recv) = region_over(0..32);
    build(&mut region, &[[16, 1, 16]], log);
    build(&mut region, &[[16, 2, 16], [17, 2, 16]], leaves);
    let e = detach(&mut region, &[[16, 1, 16], [16, 2, 16], [17, 2, 16]], Vector::ZERO);
    tick(&mut region);
    let r = region.data();
    let body = r.physics.bodies.get(*r.ecs.rigidbody.get(e)).unwrap();
    assert!((body.mass() - (WOOD_DENSITY + 2.0 * LEAF_DENSITY)).abs() < 1e-3, "mass {}", body.mass());
    assert_eq!(body.colliders().len(), 1, "one compound");
    let collider = r.physics.colliders.get(body.colliders()[0]).unwrap();
    assert_eq!(collider.mass(), 0.0, "colliders carry no mass");
    let aabb = collider.compute_aabb();
    // The log and both leaf blocks (whole blocks): two wide, two tall.
    let size = aabb.maxs - aabb.mins;
    assert!((size.x - 2.0 * B).abs() < 1e-3 && (size.y - 2.0 * B).abs() < 1e-3 && (size.z - B).abs() < 1e-3, "{aabb:?}");
}

/// A thin branch: a 3×3-voxel rod along x through the block's middle. Every
/// half-grid octant holds at most 8 × 2 × 2 = 32 < `HALF_CELL_FILL_MIN`
/// voxels, so on the half grid it collides nowhere.
fn thin_branch(id: BlockId) -> game::ChiselData {
    game::ChiselData::cuboid(id, [0, 6, 6], [BLOCK_VOXELS, 9, 9])
}

/// Floor chunks over block columns 0..32 × 0..32 (one block deep) with
/// `extra` blocks (and their chisel) written in before genesis.
fn floor_with(extra: &[([i32; 3], Block, Option<game::ChiselData>)]) -> Vec<(game::ChunkCoords, game::Chunk)> {
    let mut chunks = floor_chunks(0.0..32.0 * B, 0.0..32.0 * B, BLOCK_VOXELS as u32);
    for (pos, block, chisel) in extra {
        let (coords, (x, y, z)) = game::edit::locate(*pos).unwrap();
        let (_, chunk) = chunks.iter_mut().find(|(c, _)| *c == coords).expect("inside the floored area");
        chunk.set_block(x, y, z, *block);
        if let Some(c) = chisel {
            chunk.chisel.insert(game::BlockIndex::from_xyz(x, y, z), c.clone());
        }
    }
    chunks
}

/// The branch block at [16, 1, 16] (lowest voxel 6 voxels above the floor).
fn branch_setup() -> (BlockRegistry, [i32; 3], Vec<(game::ChunkCoords, game::Chunk)>) {
    let reg = registry();
    let branch = reg.id_of("pine_branch").unwrap();
    let pos = [16, 1, 16];
    let chunks = floor_with(&[(pos, Block::new(branch), Some(thin_branch(branch)))]);
    (reg, pos, chunks)
}

#[test]
fn thin_chiseled_branch_collides_and_rests_on_the_floor() {
    let (reg, pos, chunks) = branch_setup();
    let branch = reg.id_of("pine_branch").unwrap();
    let cells = game::block_collider_cells(Block::new(branch), Some(&thin_branch(branch)));
    assert!(cells.blocks.is_empty() && cells.halves.is_empty() && cells.voxels.is_empty(), "no half-grid cell");
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), r0(), None);
    region.with_data(|r| r.seed_terrain_genesis(chunks));
    let e = detach(&mut region, &[pos], Vector::ZERO);
    assert_eq!(block_at(region.data(), pos).id, BlockId::AIR);
    let floor_y = B; // the floor is one block deep
    let lowest = |r: &Rollback| {
        let body = r.physics.bodies.get(*r.ecs.rigidbody.get(e)).unwrap();
        assert_eq!(body.colliders().len(), 1, "the thin branch has a collider");
        r.physics.colliders.get(body.colliders()[0]).unwrap().compute_aabb().mins.y
    };
    assert!((lowest(region.data()) - (floor_y + 6.0)).abs() < 1e-3, "its box is the rod's bounds");
    let mut rest_low = f32::INFINITY;
    let (ticks, _, _, _) = run_until_gone(&mut region, e, SETTLE_TIMEOUT_TICKS + 5, |reg| {
        if alive(reg.data(), e) {
            rest_low = lowest(reg.data());
        }
    })
    .expect("the branch despawns");
    assert!(ticks < SETTLE_TIMEOUT_TICKS, "it came to rest ({ticks} ticks), not timed out");
    assert!(rest_low >= floor_y - 1.0, "at rest on the floor: lowest point {rest_low}, floor {floor_y}");
    assert!(rest_low < floor_y + 1.0, "it fell onto the floor: lowest point {rest_low}");
}

#[test]
fn leaves_only_detach_falls_and_rests() {
    let reg = registry();
    let leaves = Block::new(reg.id_of("oak_leaves").unwrap());
    let (mut region, _recv) = region_over(0..32);
    let set = [[5, 3, 5], [5, 4, 5], [6, 4, 5]];
    build(&mut region, &set, leaves);
    let e = detach(&mut region, &set, Vector::ZERO);
    let r = region.data();
    for p in set {
        assert_eq!(block_at(r, p).id, BlockId::AIR, "{p:?} cleared");
    }
    let start_y = body_pose(r, e).translation.y;
    let (ticks, min_y, _, _) =
        run_until_gone(&mut region, e, SETTLE_TIMEOUT_TICKS + 5, |_| {}).expect("the leaves despawn");
    assert!(ticks < SETTLE_TIMEOUT_TICKS, "they came to rest ({ticks} ticks), not timed out");
    // Lowest leaf block from y 3 down onto the floor top (y 1): two blocks.
    assert!((start_y - min_y - 2.0 * B).abs() < 2.0, "fell two blocks onto the floor: {start_y} -> {min_y}");
}

#[test]
fn rollback_of_thin_branch_and_leaves_only_detach_is_bit_exact() {
    let _mode = Mode::set(game::HashMode::Full);
    let (reg, pos, chunks) = branch_setup();
    let leaves = Block::new(reg.id_of("oak_leaves").unwrap());
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    r.seed_terrain_genesis(chunks);
    let leaf_set = [[5, 1, 5], [5, 2, 5]];
    r.new_transaction();
    for &p in &leaf_set {
        assert!(r.set_block_safe(p, leaves));
    }
    r.forget();
    let before = state_hash(&r);
    for mode in [game::HashMode::Full, game::HashMode::Delta] {
        game::set_hash_mode(mode);
        r.new_transaction();
        assert!(r.detach_blocks_safe(&[pos], SettlePolicy::Despawn, Vector::ZERO).is_some());
        assert_ne!(before, state_hash(&r));
        r.rollback();
        assert_eq!(before, state_hash(&r), "thin branch detach rolls back bit-exact ({mode:?})");
        r.new_transaction();
        assert!(r.detach_blocks_safe(&leaf_set, SettlePolicy::Despawn, Vector::ZERO).is_some());
        assert_ne!(before, state_hash(&r), "the leaves were detached");
        r.rollback();
        assert_eq!(before, state_hash(&r), "leaves-only detach rolls back bit-exact ({mode:?})");
    }
}

#[test]
fn clear_blocks_batches_per_chunk() {
    let (mut region, recv) = region_over(0..32);
    let cb = game::CHUNK_BLOCKS as i32;
    // Straddles the chunk border at x = cb.
    let blocks = [[cb - 1, 1, 16], [cb, 1, 16], [cb + 1, 1, 16]];
    build(&mut region, &blocks, Block::new(STONE));
    while recv.try_recv().is_ok() {}
    let rebuilds = Rollback::collider_rebuilds();
    let mut cleared = 0;
    // Air and a repeat are skipped.
    region.with_data(|r| cleared = r.clear_blocks_safe(&[blocks[0], blocks[1], blocks[2], blocks[1], [cb, 5, 16]]));
    assert_eq!(cleared, 3);
    let chunks: std::collections::BTreeSet<_> =
        blocks.iter().map(|&p| game::edit::locate(p).unwrap().0).map(|c| (c.x, c.y, c.z)).collect();
    let sets: Vec<usize> = recv
        .try_iter()
        .filter_map(|u| match u.update_kind {
            GameDataUpdateKind::SetBlocks(_, d) => Some(d.len()),
            _ => None,
        })
        .collect();
    assert_eq!(sets.len(), chunks.len(), "one SetBlocks per chunk: {sets:?}");
    assert_eq!(sets.iter().sum::<usize>(), 3);
    assert_eq!(Rollback::collider_rebuilds(), rebuilds, "colliders patched incrementally");
    for p in blocks {
        assert_eq!(block_at(region.data(), p).id, BlockId::AIR);
    }
}

#[test]
fn clear_blocks_refuses_bedrock_and_materialises_ground() {
    use game::{Column, ColumnCoords, ColumnGround, SubChunk};
    registry();
    let (stone, bedrock) = (BlockId(2), BlockId(6));
    let column = || {
        let band = SubChunk::Dense(Box::new(game::Chunk::flat_floor_with(32, stone)));
        let ground = ColumnGround::plain(vec![1; game::COLUMN_FOOTPRINT].into_boxed_slice(), 0, stone, bedrock);
        Column::new(0, Block::new(BlockId(1)), Block::new(BlockId::AIR), vec![band]).with_ground(ground)
    };
    let mut region = Region::from_columns(r0(), vec![(ColumnCoords::new(0, 0), column())]);
    let cb = game::CHUNK_BLOCKS as i32;
    let before = state_hash(region.data());
    let mut cleared = usize::MAX;
    // Surface block y is 1, so bedrock starts at 1 - UNDERGROUND_DEPTH.
    let floor = 1 - game::UNDERGROUND_DEPTH;
    region.with_data(|r| cleared = r.clear_blocks_safe(&[[cb / 2, floor, cb / 2]]));
    assert_eq!(cleared, 0, "bedrock is refused");
    assert_eq!(before, state_hash(region.data()), "a refusal changes nothing");
    let deep = [cb / 2, -3, cb / 2];
    let (coords, _) = game::edit::locate(deep).unwrap();
    assert!(region.data().chunk_key_at(coords).is_none(), "implied before the dig");
    region.with_data(|r| cleared = r.clear_blocks_safe(&[deep]));
    assert_eq!(cleared, 1);
    assert!(region.data().chunk_key_at(coords).is_some(), "the implied chunk became real");
    assert_eq!(block_at(region.data(), deep).id, BlockId::AIR);
}

/// A falling body's leaves collide (standing leaves do not): a log with two
/// leaf blocks hanging under it, detached above the floor, comes to rest on
/// its LEAVES — their bottom on the floor, not the log's.
#[test]
fn falling_leaves_collide_and_rest_on_the_floor() {
    let reg = registry();
    let log = Block::new(reg.id_of("oak_log").unwrap());
    let leaves = Block::new(reg.id_of("oak_leaves").unwrap());
    let (mut region, _recv) = region_over(0..32);
    build(&mut region, &[[16, 5, 16]], log);
    build(&mut region, &[[16, 3, 16], [16, 4, 16]], leaves);
    let e = detach(&mut region, &[[16, 3, 16], [16, 4, 16], [16, 5, 16]], Vector::ZERO);
    let model = region.data().ecs.voxel_body.get(e).model.clone();
    let com = model_mass_properties(&model).local_com;
    let floor_y = B;
    let mut leaf_bottom = f32::INFINITY;
    let (ticks, _, _, _) = run_until_gone(&mut region, e, SETTLE_TIMEOUT_TICKS + 5, |reg| {
        let r = reg.data();
        if alive(r, e) {
            let pose = body_pose(r, e);
            // The leaf block at model offset [0, 0, 0]: its four bottom corners.
            leaf_bottom = [[0.0, 0.0], [B, 0.0], [0.0, B], [B, B]]
                .into_iter()
                .map(|[x, z]| (pose.translation + pose.rotation * (Vector::new(x, 0.0, z) - com)).y)
                .fold(f32::INFINITY, f32::min);
        }
    })
    .expect("the body despawns");
    assert!(ticks < SETTLE_TIMEOUT_TICKS, "it came to rest ({ticks} ticks), not timed out");
    assert!(leaf_bottom >= floor_y - 1.0, "the leaves rest on the floor: leaf bottom {leaf_bottom}, floor {floor_y}");
    assert!(leaf_bottom < floor_y + 1.0, "it fell onto the floor: leaf bottom {leaf_bottom}");
}

/// A voxel body falls at the world's gravity (`movement::GRAVITY`, what
/// players fall with), not a real-world 9.81 m/s².
#[test]
fn bodies_fall_at_world_gravity() {
    let (mut region, _recv) = region_over(0..32);
    let col = [[16, 12, 16]];
    build(&mut region, &col, Block::new(STONE));
    let e = detach(&mut region, &col, Vector::ZERO);
    let vy = |r: &Rollback| r.physics.bodies.get(*r.ecs.rigidbody.get(e)).unwrap().linvel().y;
    tick(&mut region);
    let v1 = vy(region.data());
    tick(&mut region);
    let dv = v1 - vy(region.data());
    let want = game::movement::GRAVITY * game::movement::UNITS_PER_METER * game::movement::TICK_DT;
    assert!((dv - want).abs() < 1e-2, "falls {dv} units/s faster per tick, want {want}");
}

/// The tick a toppling body lands (its damping raised, `VoxelBody::landed`
/// set) rolls back bit-exact.
#[test]
fn rollback_of_landing_is_bit_exact() {
    let _mode = Mode::set(game::HashMode::Full);
    let (mut region, _recv) = region_over(0..32);
    let col = column(16, 16, 6);
    build(&mut region, &col, Block::new(STONE));
    let pivot = Vector::new(16.5 * B, B, 16.5 * B);
    let mut e = None;
    region.with_data(|r| e = r.detach_blocks_pivoting_safe(&col, SettlePolicy::Despawn, Vector::new(0.0, 0.0, 1.5), Some(pivot)));
    let e = e.expect("a body");
    for _ in 0..SETTLE_TIMEOUT_TICKS {
        let pre = state_hash(region.data());
        region.handle_event(GameEventKind::Tick).unwrap();
        let r = region.data();
        assert!(alive(r, e), "despawned before landing");
        if r.ecs.voxel_body.get(e).landed {
            let damping = r.physics.bodies.get(*r.ecs.rigidbody.get(e)).unwrap().angular_damping();
            assert_eq!(damping, game::voxel_body::VOXEL_BODY_LANDED_ANGULAR_DAMPING);
            region.rollback_last_event();
            assert_eq!(pre, state_hash(region.data()), "the landing rolls back bit-exact");
            assert!(!region.data().ecs.voxel_body.get(e).landed);
            return;
        }
        region.forget_last_event();
    }
    panic!("never landed");
}

/// A column toppling about its base onto a one-block floor lies down on
/// it: it never stops mid-topple, and never ends up under the floor. When
/// the landing damping fired on "tilted and no longer falling" (true
/// mid-topple, while the foot grips the floor), rapier's own CCD (on for
/// every fast dynamic body) froze the heavily damped, still-turning body at
/// ~34° of tilt while its velocity grew to -200 units/s, then it dropped
/// through the floor upside down; landing now needs an impact.
#[test]
fn toppling_column_lies_down_on_the_floor() {
    let (mut region, _recv) = region_over(0..32);
    let col = column(16, 16, 6);
    build(&mut region, &col, Block::new(STONE));
    let pivot = Vector::new(16.5 * B, B, 16.5 * B);
    let mut e = None;
    region.with_data(|r| e = r.detach_blocks_pivoting_safe(&col, SettlePolicy::Despawn, Vector::new(0.0, 0.0, 1.5), Some(pivot)));
    let e = e.expect("a body");
    let floor_y = B;
    let mut last = None;
    for _ in 0..SETTLE_TIMEOUT_TICKS {
        tick(&mut region);
        let r = region.data();
        if !alive(r, e) {
            break;
        }
        let body = r.physics.bodies.get(*r.ecs.rigidbody.get(e)).unwrap();
        let low = r.physics.colliders.get(body.colliders()[0]).unwrap().compute_aabb().mins.y;
        assert!(low >= floor_y - 2.0, "under the floor: lowest point {low}, speed {:?}", body.linvel());
        last = Some(((body.rotation() * Vector::Y).y, low));
    }
    let (up, low) = last.expect("it fell for a while");
    assert!(up.abs() < 0.2, "lying down: up axis {up}");
    assert!(low < floor_y + 1.0, "on the floor: lowest point {low}");
}

