//! Block properties: the manifest's tree blocks, and non-solid blocks
//! (leaves) having no colliders, being walked through, yet breakable.
use game::{
    collider_cells, install_block_props, Block, BlockId, BlockIndex, BlockRegistry, ChiselData, Chunk,
    ChunkCoords, ColliderScale, GameEventKind, InputEvent, Key, MouseButton, Pose, Region, RegionCoords,
    Rollback, BLOCK_VOXELS, CHUNK_BLOCKS, CHUNK_UNITS, HALF_BLOCK_VOXELS, HALF_CELL_FILL_MIN,
};

const MANIFEST: &str = include_str!("../../../assets/blocks/blocks.ron");

/// The shipped registry, with its props installed (idempotent: every test
/// in this binary shares the process-global).
fn registry() -> BlockRegistry {
    let reg = BlockRegistry::from_ron(MANIFEST).unwrap();
    install_block_props(reg.props());
    reg
}

#[test]
fn manifest_tree_blocks_parse() {
    let reg = registry();
    let names = [
        "oak_log", "oak_branch", "oak_leaves", "pine_log", "pine_branch", "pine_leaves",
        "birch_log", "birch_branch", "birch_leaves",
    ];
    for (i, n) in names.iter().enumerate() {
        assert_eq!(reg.id_of(n), Some(BlockId(13 + i as u16)), "{n}");
    }
    let props = reg.props();
    let id = |n: &str| reg.id_of(n).unwrap();
    for sp in ["oak", "pine", "birch"] {
        assert!(!props.is_solid(id(&format!("{sp}_leaves"))));
        assert!(props.is_cutout(id(&format!("{sp}_leaves"))));
        for part in ["log", "branch"] {
            assert!(props.is_solid(id(&format!("{sp}_{part}"))));
            assert!(!props.is_cutout(id(&format!("{sp}_{part}"))));
        }
    }
    // Trunks: every `_log`, and the one-block cactus; never branches or leaves.
    for sp in ["oak", "pine", "birch", "spruce", "acacia", "palm", "jungle"] {
        assert!(props.is_trunk(id(&format!("{sp}_log"))), "{sp}");
        assert!(!props.is_trunk(id(&format!("{sp}_branch"))) && !props.is_trunk(id(&format!("{sp}_leaves"))), "{sp}");
    }
    assert!(props.is_trunk(id("cactus")) && props.is_solid(id("cactus")));
    assert!(!props.is_trunk(id("stone")));
    assert!(props.is_solid(id("stone")) && !props.is_cutout(id("stone")));
    assert!(props.is_solid(BlockId(9999)) && !props.is_cutout(BlockId(9999)), "unknown = solid, opaque");
    assert_eq!(props, BlockRegistry::from_ron(MANIFEST).unwrap().props(), "equal manifests, equal props");
}

#[test]
fn leaf_blocks_get_no_collider_cells() {
    let reg = registry();
    let leaves = reg.id_of("oak_leaves").unwrap();
    let log = reg.id_of("oak_log").unwrap();
    // Uniform leaf chunk and a chunk with scattered leaves: no cells.
    assert!(collider_cells(&Chunk::uniform(Block::new(leaves))).is_empty());
    let mut chunk = Chunk::default();
    chunk.set_block(1, 1, 1, Block::new(leaves));
    chunk.set_block(2, 1, 1, Block::new(leaves));
    assert!(collider_cells(&chunk).is_empty());
    // With a log: only the log's cell.
    chunk.set_block(3, 1, 1, Block::new(log));
    let cells = collider_cells(&chunk);
    assert_eq!(cells.blocks.len(), 1);
    assert!(cells.halves.is_empty() && cells.voxels.is_empty());
    assert_eq!(cells.blocks[0].to_array(), [3, 1, 1]);
}

/// A chisel of `id` whose octant (0, 0, 0) holds `HALF_CELL_FILL_MIN - 1`
/// solid voxels, octant (1, 0, 0) exactly `HALF_CELL_FILL_MIN`, octant
/// (0, 1, 0) all 512, the rest none.
fn threshold_chisel(id: BlockId) -> ChiselData {
    let h = HALF_BLOCK_VOXELS;
    ChiselData::from_fn(vec![id], |x, y, z| {
        // Rank of the voxel inside its octant, filled x, then z, then y.
        let (o, rank) = ((x / h, y / h, z / h), (x % h + h * (z % h + h * (y % h))) as u32);
        let fill = match o {
            (0, 0, 0) => HALF_CELL_FILL_MIN - 1,
            (1, 0, 0) => HALF_CELL_FILL_MIN,
            (0, 1, 0) => (h * h * h) as u32,
            _ => 0,
        };
        (rank < fill).then_some(0)
    })
}

#[test]
fn chiseled_wood_collides_on_the_half_grid() {
    let reg = registry();
    let props = reg.props();
    for sp in ["oak", "pine", "birch"] {
        for part in ["log", "branch"] {
            assert_eq!(props.collider_scale(reg.id_of(&format!("{sp}_{part}")).unwrap()), ColliderScale::Half);
        }
        assert_eq!(props.collider_scale(reg.id_of(&format!("{sp}_leaves")).unwrap()), ColliderScale::Voxel);
    }
    assert_eq!(props.collider_scale(reg.id_of("stone").unwrap()), ColliderScale::Voxel);

    let log = reg.id_of("oak_log").unwrap();
    let mut chunk = Chunk::default();
    chunk.set_block(3, 1, 2, Block::new(log));
    chunk.chisel.insert(BlockIndex::from_xyz(3, 1, 2), threshold_chisel(log));
    let cells = collider_cells(&chunk);
    let halves: Vec<_> = cells.halves.iter().map(|v| v.to_array()).collect();
    // 127 voxels: open; 128 and 512: one half cell each; no voxel cells.
    assert_eq!(halves, vec![[7, 2, 4], [6, 3, 4]]);
    assert!(cells.blocks.is_empty() && cells.voxels.is_empty());

    // Unchanged for other chisel: the same shape in stone keeps its voxels.
    let stone = reg.id_of("stone").unwrap();
    let mut chunk = Chunk::default();
    chunk.set_block(3, 1, 2, Block::new(stone));
    chunk.chisel.insert(BlockIndex::from_xyz(3, 1, 2), threshold_chisel(stone));
    let cells = collider_cells(&chunk);
    assert_eq!(cells.halves.iter().map(|v| v.to_array()).collect::<Vec<_>>(), vec![[6, 3, 4]]);
    assert_eq!(cells.voxels.len(), 2 * HALF_CELL_FILL_MIN as usize - 1);
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

/// Block edits next to and of chiseled wood patch the colliders to exactly
/// what genesis (`create_mesh`) builds for the same chunk.
#[test]
fn incremental_edits_of_chiseled_wood_match_genesis() {
    let reg = registry();
    let (log, stone) = (reg.id_of("pine_log").unwrap(), reg.id_of("stone").unwrap());
    let target = ChunkCoords::new(1, 0, 1);
    let mut start = Chunk::flat_floor(BLOCK_VOXELS as u32);
    for (x, z) in [(4, 4), (5, 4), (4, 5), (9, 9)] {
        start.set_block(x, 1, z, Block::new(log));
        start.chisel.insert(BlockIndex::from_xyz(x, 1, z), threshold_chisel(log));
    }
    let mut r = Rollback::new(None);
    let mut chunks = Vec::new();
    for x in 0..=2 {
        for z in 0..=2 {
            let c = ChunkCoords::new(x, 0, z);
            chunks.push((c, if c == target { start.clone() } else { Chunk::flat_floor(BLOCK_VOXELS as u32) }));
        }
    }
    r.seed_terrain_genesis(chunks);
    let e = r.chunk_key_at(target).unwrap();
    let genesis = |r: &Rollback| {
        let mut fresh = Rollback::new(None);
        fresh.new_transaction();
        let fe = fresh.create_mesh(target, r.ecs.chunk.get(e).clone());
        collider_shapes(&fresh, fe)
    };
    assert_eq!(collider_shapes(&r, e), genesis(&r), "seeded");
    let cb = CHUNK_BLOCKS as i32;
    let world = |x: i32, y: i32, z: i32| [cb + x, y, cb + z];
    r.new_transaction();
    for (p, block) in [
        (world(6, 1, 4), Block::new(stone)), // beside the wood
        (world(5, 1, 4), Block::new(BlockId::AIR)), // breaks a chiseled log
        (world(4, 2, 4), Block::new(log)), // a whole log on top
        (world(4, 1, 4), Block::new(BlockId::AIR)),
    ] {
        assert!(r.set_block_safe(p, block), "{p:?}");
        assert_eq!(collider_shapes(&r, e), genesis(&r), "after {p:?} -> {block:?}");
    }
    assert!(collider_cells(r.ecs.chunk.get(e)).voxels.is_empty(), "wood left no voxel cells");
}

fn ev(r: &mut Region, e: GameEventKind) {
    r.handle_event(e).unwrap();
    r.forget_last_event();
}

/// A region with a one-block floor under the spawn and player 0 settled on
/// it, at the chunk column holding (128, 128) units.
fn floor_region() -> Region {
    let spawn = 128 / CHUNK_UNITS;
    let mut chunks = Vec::new();
    for x in spawn.saturating_sub(1)..=spawn + 1 {
        for z in spawn.saturating_sub(1)..=spawn + 1 {
            chunks.push((ChunkCoords::new(x, 0, z), Chunk::flat_floor(BLOCK_VOXELS as u32)));
        }
    }
    let mut r = Region::new(game::Rollback::new(None), None, RegionCoords::new(0, 0), None);
    r.with_data(|d| d.seed_terrain_genesis(chunks));
    ev(&mut r, GameEventKind::CreateClient(0));
    // A new client is in its body, where E opens the inventory: start from the overview.
    ev(&mut r, GameEventKind::Order(0, game::Order::SetMode { mode: game::PlayMode::Overview, at: None }));
    for pressed in [true, false] {
        ev(&mut r, GameEventKind::PlayerInput(0, InputEvent::Key { key: Key::KeyE, pressed }));
    }
    for _ in 0..12 {
        ev(&mut r, GameEventKind::Tick);
    }
    r
}

fn pos(r: &Region) -> (f32, f32, f32) {
    let d = r.data();
    let e = *d.player_entites.get(&0).unwrap();
    let t = d.physics.bodies.get(*d.ecs.rigidbody.get(e)).unwrap().translation();
    (t.x, t.y, t.z)
}

/// Places a 3x3x3 cube of `id` blocks, block coords x 8..11, y 1..4 (on the
/// floor), z 4..7, i.e. straight ahead (-z) of a player at block (9, 1, 8).
fn put_cube(r: &mut Region, id: BlockId, z0: i32) {
    for x in 8..11 {
        for y in 1..4 {
            for z in z0..z0 + 3 {
                r.with_data(|d| {
                    assert!(d.set_block_safe([x, y, z], Block::new(id)));
                });
            }
        }
    }
}

/// Walk forward (-z) until `ticks` pass or the body is `target` units past
/// where it started; returns the distance covered. (Ticks are long: walking
/// on far would leave the region and remove the player.)
fn walk_forward(r: &mut Region, ticks: usize, target: f32) -> f32 {
    let z0 = pos(r).2;
    ev(r, GameEventKind::PlayerInput(0, InputEvent::Key { key: Key::KeyW, pressed: true }));
    for _ in 0..ticks {
        ev(r, GameEventKind::Tick);
        if z0 - pos(r).2 >= target {
            break;
        }
    }
    z0 - pos(r).2
}

fn start(r: &mut Region) {
    r.with_data(|d| {
        let e = *d.data.player_entites.get(&0).unwrap();
        // block (9, 1, 9) centre, standing on the floor top (y = 1 block)
        let u = BLOCK_VOXELS as f32;
        d.set_body_pose_safe(e, Pose::translation(9.5 * u, 1.0 * u + 20.0, 9.5 * u));
    });
    for _ in 0..6 {
        ev(r, GameEventKind::Tick);
    }
}

#[test]
fn body_passes_through_leaves() {
    let reg = registry();
    let leaves = reg.id_of("oak_leaves").unwrap();
    let mut r = floor_region();
    put_cube(&mut r, leaves, 5);
    start(&mut r);
    // Cube spans z blocks 5..8, player at z 9.5: 4.5 blocks to the far side.
    let far = 5.0 * BLOCK_VOXELS as f32;
    let walked = walk_forward(&mut r, 60, far);
    assert!(walked >= far, "walked through the leaves: {walked}");
}

#[test]
fn a_log_cube_blocks_the_body() {
    // Control: the same cube in logs stops the player.
    let reg = registry();
    let log = reg.id_of("oak_log").unwrap();
    let mut r = floor_region();
    put_cube(&mut r, log, 5);
    start(&mut r);
    let walked = walk_forward(&mut r, 20, 5.0 * BLOCK_VOXELS as f32);
    assert!(walked < 2.0 * BLOCK_VOXELS as f32, "stopped by the logs: {walked}");
}

#[test]
fn edit_ray_hits_and_breaks_a_leaf() {
    let reg = registry();
    let leaves = reg.id_of("oak_leaves").unwrap();
    let mut r = floor_region();
    // One leaf block directly in front of the player's eye, level with it.
    start(&mut r);
    let u = BLOCK_VOXELS as f32;
    let (x, y, z) = pos(&r);
    let (bx, bz) = ((x / u).floor() as i32, (z / u).floor() as i32 - 2);
    // A column two blocks ahead, floor to head height: the ray from the eye
    // hits one of them whatever the exact eye height.
    let column: Vec<[i32; 3]> = (1..4).map(|by| [bx, by, bz]).collect();
    for t in &column {
        r.with_data(|d| assert!(d.set_block_safe(*t, Block::new(leaves))));
    }
    let _ = y;
    let leaf_count = |r: &Region| {
        r.terrain_snapshot()
            .chunks
            .iter()
            .map(|(_, c)| c.blocks.iter().filter(|(_, b)| b.id == leaves).count())
            .sum::<usize>()
    };
    assert_eq!(leaf_count(&r), 3);
    for e in [
        GameEventKind::PlayerInput(0, InputEvent::MouseButton { button: MouseButton::Left, pressed: true }),
        GameEventKind::Tick,
        GameEventKind::PlayerInput(0, InputEvent::MouseButton { button: MouseButton::Left, pressed: false }),
        GameEventKind::Tick,
    ] {
        r.handle_event(e).unwrap();
    }
    assert_eq!(leaf_count(&r), 2, "the ray hit exactly one leaf and broke it");
}

/// Biome ground, ice and the new tree species (ids 22+): every name resolves
/// and every texture file the manifest names exists on disk.
#[test]
fn manifest_has_biome_blocks() {
    let reg = registry();
    let mut names: Vec<String> = [
        "snow", "ice", "glacier_ice", "mud", "peat", "gravel", "red_sand", "dry_grass", "podzol",
        "frozen_dirt", "salt", "cactus",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for sp in ["spruce", "acacia", "palm", "jungle"] {
        for part in ["log", "branch", "leaves"] {
            names.push(format!("{sp}_{part}"));
        }
    }
    let props = reg.props();
    for n in &names {
        let id = reg.id_of(n).unwrap_or_else(|| panic!("{n} missing"));
        assert_ne!(id, BlockId(0), "{n} is air");
        if n.ends_with("_leaves") {
            assert!(!props.is_solid(id) && props.is_cutout(id), "{n}");
        } else {
            assert!(props.is_solid(id), "{n}");
        }
    }
    // Every texture file named in the manifest exists.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/blocks");
    let mut files = 0;
    for tok in MANIFEST.split('"').skip(1).step_by(2) {
        if tok.ends_with(".png") {
            assert!(dir.join(tok).is_file(), "texture {tok} missing");
            files += 1;
        }
    }
    assert!(files > 30);
}

#[test]
fn water_is_liquid_and_not_solid() {
    let reg = registry();
    let props = game::block_props();
    let water = reg.id_of("water").unwrap();
    assert_eq!(water, BlockId(5));
    assert!(props.is_liquid(water));
    assert!(!props.is_solid(water), "a liquid is always non-solid");
    assert!(!props.blocks_ray(water));
    assert!(!props.blocks_ray(BlockId::AIR));
    let ice = reg.id_of("ice").unwrap();
    assert!(props.blocks_ray(ice) && props.is_solid(ice) && !props.is_liquid(ice));
    assert!(props.blocks_ray(reg.id_of("stone").unwrap()));
}
