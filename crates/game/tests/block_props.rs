//! Block properties: the manifest's tree blocks, and non-solid blocks
//! (leaves) having no colliders, being walked through, yet breakable.
use game::{
    collider_cells, install_block_props, Block, BlockId, BlockRegistry, Chunk, ChunkCoords,
    GameEventKind, InputEvent, Key, MouseButton, Pose, Region, RegionCoords, BLOCK_VOXELS,
    CHUNK_UNITS,
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
