//! Cross-target determinism: the same physics scene must end in the same
//! state on native and on wasm32 (node, via wasm-bindgen-test-runner) —
//! client and server run the sim on different targets. The scene is small and
//! uses no worldgen, so the wasm build stays light: a kinematic player capsule
//! walking and jumping over 8×8 chunks of hand-built `Voxels` floor (block
//! scale, plus one row of half blocks for chisel scale), one slime, and a
//! kinematic compound of two rotated cuboids whose mass properties go through
//! the eigen solve (std `acos`/`cos` in glamx).
//!
//! Every constant holds on every target: [`WASM_SCENE_DIGEST`] is crc32 of
//! the region's canonical bincode bytes (`SerializedRegion`, the parking and
//! wire format: bincode writes `usize` as u64), and `game::state_hash` walks
//! `StateHash`, which also hashes `usize` as u64.
//!
//! Run both sides with `just determinism-wasm`. The constants change only in
//! a commit whose message says why; print them with
//!
//! ```sh
//! WASM_SCENE_HASH_PRINT=1 just test-one game "--test wasm_determinism -- --nocapture"
//! ```
mod common;

use common::{body, fixed_body_mass_count, key, r0, spawn_slime, ticks, world_of};
use game::rapier3d::math::Vector;
use glamx::Quat;
use game::rapier3d::prelude::{ColliderBuilder, RigidBodyBuilder, RigidBodyHandle};
use game::{
    state_hash, Block, BlockId, Chunk, ChunkCoords, Column, ColumnCoords, ColumnGround,
    Key, Pose, Region, SubChunk,
};
use slotmapd::Key as _;

/// crc32 of the scene region's bincode bytes after [`SCENE_TICKS`] ticks,
/// on every target. Re-recorded for the rapier 0.36 / parry 0.31 switch,
/// then for `dt` = `TICK_DT` and the kinematic `can_sleep(false)` fix,
/// then for the new `GameData.ground` field (serialized bytes + state hash),
/// then for the `designations` / `constructed` order fields, then for the
/// `ChunkBlocks` uniform/dense grid, `u16` `BlockIndex` and boxed
/// `ColumnGround` footprints (serialization + state hash; geometry unchanged).
///
/// Every value is pinned per sim-chunk size (`game::CHUNK_BLOCKS`, a build
/// dial): the same physical scene splits into different chunk entities,
/// bodies and colliders at 2 and 16 blocks, so its bytes and hashes differ.
/// Other sizes are not pinned and skip the comparison.
const WASM_SCENE_DIGEST: u32 = pinned(0x9ADD_3052, 0xB5B3_D519);
/// `game::state_hash` of the same state, on every target.
const SCENE_STATE_HASH: u32 = pinned(0x1D38_127B, 0xADE4_9129);
const SCENE_TICKS: usize = 120;
/// crc32 of [`underground_scene`]'s region bytes, on every target.
const UNDERGROUND_DIGEST: u32 = pinned(0xD4C1_DE27, 0x24CC_65C7);
/// `game::state_hash` of the same state, on every target.
const UNDERGROUND_STATE_HASH: u32 = pinned(0xBA1F_2E0E, 0x8A5A_428D);

/// The value pinned for this build's chunk size: `at2` at 2-block chunks,
/// `at16` at 16 (0 elsewhere; see [`pinned_size`]).
const fn pinned(at2: u32, at16: u32) -> u32 {
    match game::CHUNK_BLOCKS {
        2 => at2,
        16 => at16,
        _ => 0,
    }
}

/// Whether this chunk size has pinned values; prints a skip line if not.
fn pinned_size() -> bool {
    let pinned = matches!(game::CHUNK_BLOCKS, 2 | 16);
    if !pinned {
        log(&format!("wasm_determinism: nothing pinned at {}-block chunks, comparison skipped", game::CHUNK_BLOCKS));
    }
    pinned
}

/// Runs the scene; returns `(portable digest, state_hash)`.
fn scene() -> (u32, u32) {
    // Units 64..320 each way around the spawn at (128, 128); the 32-unit
    // strip z 192..224 (block rows 12 and 13 — one chunk row at 2-block
    // chunks) is half blocks (top 24) over the block-scale floor (top 16).
    let mut world = world_of(64.0..320.0, 64.0..320.0, |_, z| {
        let half = Chunk::flat_floor(24);
        let mut chunk = Chunk::flat_floor(16);
        for bz in 0..game::CHUNK_BLOCKS {
            if !(12..14).contains(&(z * game::CHUNK_BLOCKS + bz)) {
                continue;
            }
            for by in 0..game::CHUNK_BLOCKS {
                for bx in 0..game::CHUNK_BLOCKS {
                    let i = game::BlockIndex::from_xyz(bx, by, bz);
                    chunk.set_block(bx, by, bz, half.block_at(bx, by, bz));
                    match half.chisel.get(&i) {
                        Some(c) => chunk.chisel.insert(i, c.clone()),
                        None => chunk.chisel.remove(&i),
                    };
                }
            }
        }
        chunk
    });
    spawn_slime(world.regions.get_mut(&r0()).unwrap(), [176.0, 40.0, 112.0], 1, 0.0, 0);
    let compound = add_compound_body(&mut world);
    // S (+z) over the half-block row and back off it, then D (+x), with jumps.
    let script: [(usize, Key, bool); 8] = [
        (0, Key::KeyS, true),
        (10, Key::Space, true),
        (11, Key::Space, false),
        (40, Key::KeyS, false),
        (40, Key::KeyD, true),
        (60, Key::Space, true),
        (61, Key::Space, false),
        (80, Key::KeyD, false),
    ];
    let start = body(&world);
    for t in 0..SCENE_TICKS {
        for &(_, k, pressed) in script.iter().filter(|s| s.0 == t) {
            key(&mut world, k, pressed);
        }
        ticks(&mut world, 1);
    }
    let end = body(&world);
    // 40 ticks each way at walk speed is ~138 units: on the floor, standing.
    assert!(end.z - start.z > 100.0 && end.x - start.x > 100.0, "the script must walk: {start:?} -> {end:?}");
    assert!(end.grounded && end.x < 320.0 && end.z < 320.0, "the walk must end on the floor: {end:?}");
    let data = world.data(&r0());
    // The hazard this check exists for, asserted directly: were terrain mass
    // properties computed, the digest would only differ if std acos/cos
    // happened to round differently on this floor.
    let (fixed, massive) = fixed_body_mass_count(data);
    let chunks = common::chunks_over(64.0..320.0, 64.0..320.0).len();
    assert!(fixed >= chunks && massive == 0, "{massive} of {fixed} fixed terrain bodies computed mass properties");
    // Upgrade plan Task 5.4 Step 5b: a moving body's mass properties do go
    // through `symmetric_eigen` (glamx's std `acos`/`cos`); the compound's
    // principal frame is off-axis, so the digest carries its result.
    let mprops = data.physics.bodies.get(compound).unwrap().mass_properties().local_mprops;
    assert!(
        mprops.principal_inertia_local_frame.angle_between(Quat::IDENTITY) > 0.1,
        "the compound's inertia must be off-axis: {mprops:?}"
    );
    let digest = crc32fast::hash(&game::SerializedRegion::from_rollback(data).0);
    let hash = state_hash(data);
    if print_hash() {
        log(&format!("WASM_SCENE_DIGEST = 0x{digest:08X}, state_hash = 0x{hash:08X}"));
    }
    (digest, hash)
}

/// A kinematic body carrying two offset, rotated cuboids. One collider alone
/// never reaches the eigen solve (adding to zero mass properties returns the
/// other side as is); the sum of two does. The rotations are built from
/// explicit components (only `sqrt`), so the inputs are the same bits on
/// every target.
fn add_compound_body(world: &mut game::World) -> RigidBodyHandle {
    let mut handle = None;
    world.regions.get_mut(&r0()).unwrap().with_data(|d| {
        let e = d.data.ecs.create_entity_safe();
        let body = RigidBodyBuilder::kinematic_position_based()
            .translation(Vector::new(240.0, 40.0, 240.0))
            .can_sleep(false)
            .user_data(e.data().as_ffi() as u128)
            .build();
        let physics = &mut *d.data.physics;
        let bodies = physics.bodies.change();
        let h = bodies.insert(body);
        let colliders = physics.colliders.change();
        for (offset, rotation, half) in [
            (Vector::new(2.0, 1.0, -1.5), Quat::from_xyzw(0.1, 0.3, 0.2, 0.9), Vector::new(3.0, 1.0, 2.0)),
            (Vector::new(-1.0, -0.5, 2.5), Quat::from_xyzw(-0.4, 0.1, 0.25, 0.8), Vector::new(1.0, 2.5, 0.5)),
        ] {
            let collider = ColliderBuilder::cuboid(half.x, half.y, half.z)
                .position(Pose::from_parts(offset, rotation.normalize()))
                .user_data(e.data().as_ffi() as u128)
                .build();
            colliders.insert_with_parent(collider, h, bodies);
        }
        d.data.ecs.rigidbody.set_safe(e, Some(h));
        handle = Some(h);
    });
    ticks(world, 1);
    handle.unwrap()
}

/// A 2x2-column region of implied ground (three strata over bedrock) dug
/// down a shaft that wanders across all four columns: each dig materialises chunks from the recipe, the last
/// lands on bedrock and is refused. Returns `(digest, state_hash)`.
fn underground_scene() -> (u32, u32) {
    let stone = BlockId(2);
    let bedrock = BlockId(3);
    let column = || {
        // `below` differs from the band, or normalising trims the band chunk.
        let band = SubChunk::Dense(Box::new(Chunk::flat_floor_with(32, stone)));
        let mut ground = ColumnGround::plain(vec![1; game::COLUMN_FOOTPRINT].into_boxed_slice(), 0, stone, bedrock);
        ground.geo = (0..game::COLUMN_FOOTPRINT).map(|i| (i % 3) as i16).collect();
        ground.strata = vec![(0, stone), (-20, BlockId(4)), (-40, BlockId(5))];
        Column::new(0, Block::new(BlockId(1)), Block::new(BlockId::AIR), vec![band]).with_ground(ground)
    };
    let mut columns = Vec::new();
    for x in 0..2 {
        for z in 0..2 {
            columns.push((ColumnCoords::new(x, z), column()));
        }
    }
    let mut region = Region::from_columns(r0(), columns);
    // Surface block Y is 1, so bedrock starts at 1 - UNDERGROUND_DEPTH = -59.
    let ys = [1, -3, -8, -14, -20, -27, -33, -40, -47, -54, -58, -59];
    let cb = game::CHUNK_BLOCKS as i32;
    // Materialised chunks of the 2x2 columns over every chunk Y the shaft reaches.
    let count = |region: &Region| {
        let mut n = 0;
        for x in 0..2 {
            for z in 0..2 {
                for cy in (-60i32).div_euclid(cb)..=1i32.div_euclid(cb) {
                    n += region.data().chunk_key_at(ChunkCoords::new(x, cy, z)).is_some() as usize;
                }
            }
        }
        n
    };
    let before = count(&region);
    for (i, y) in ys.into_iter().enumerate() {
        // The shaft wanders across the four columns around their shared
        // corner at block (cb, cb).
        let pos = [cb - 1 + (i as i32 % 2), y, cb - 1 + (i as i32 % 3) / 2];
        let mut changed = false;
        region.with_data(|d| changed = d.set_block_safe(pos, Block::new(BlockId::AIR)));
        assert_eq!(changed, i != ys.len() - 1, "dig {i} at y={y}: only the bedrock dig is refused");
    }
    let after = count(&region);
    if print_hash() {
        log(&format!("underground chunks {before} -> {after}"));
    }
    assert!(after > before + 10, "digging must materialise implied ground: {before} -> {after} chunks");
    let data = region.data();
    let digest = crc32fast::hash(&game::SerializedRegion::from_rollback(data).0);
    let hash = state_hash(data);
    if print_hash() {
        log(&format!("UNDERGROUND_DIGEST = 0x{digest:08X}, UNDERGROUND_STATE_HASH = 0x{hash:08X}"));
    }
    (digest, hash)
}

/// Native prints on request; wasm32 always prints (node has no env to ask).
#[cfg(not(target_arch = "wasm32"))]
fn print_hash() -> bool {
    std::env::var_os("WASM_SCENE_HASH_PRINT").is_some()
}

#[cfg(target_arch = "wasm32")]
fn print_hash() -> bool {
    true
}

#[cfg(not(target_arch = "wasm32"))]
fn log(line: &str) {
    println!("{line}");
}

/// `println!` goes nowhere on wasm32-unknown-unknown; the runner shows the
/// console.
#[cfg(target_arch = "wasm32")]
fn log(line: &str) {
    wasm_bindgen_test::console_log!("{line}");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn scene_hash_native() {
    let (digest, hash) = scene();
    if !pinned_size() {
        return;
    }
    assert_eq!(digest, WASM_SCENE_DIGEST, "native scene digest 0x{digest:08X} != golden 0x{WASM_SCENE_DIGEST:08X}");
    assert_eq!(hash, SCENE_STATE_HASH, "native state_hash 0x{hash:08X} != golden 0x{SCENE_STATE_HASH:08X}");
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test::wasm_bindgen_test]
fn scene_hash_wasm() {
    let (digest, hash) = scene();
    if !pinned_size() {
        return;
    }
    assert_eq!(digest, WASM_SCENE_DIGEST, "wasm32 scene digest 0x{digest:08X} != golden 0x{WASM_SCENE_DIGEST:08X}");
    assert_eq!(hash, SCENE_STATE_HASH, "wasm32 state_hash 0x{hash:08X} != golden 0x{SCENE_STATE_HASH:08X}");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn underground_hash_native() {
    let (digest, hash) = underground_scene();
    if !pinned_size() {
        return;
    }
    assert_eq!(digest, UNDERGROUND_DIGEST, "native digest 0x{digest:08X} != golden 0x{UNDERGROUND_DIGEST:08X}");
    assert_eq!(hash, UNDERGROUND_STATE_HASH, "native state_hash 0x{hash:08X} != golden 0x{UNDERGROUND_STATE_HASH:08X}");
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test::wasm_bindgen_test]
fn underground_hash_wasm() {
    let (digest, hash) = underground_scene();
    if !pinned_size() {
        return;
    }
    assert_eq!(digest, UNDERGROUND_DIGEST, "wasm32 digest 0x{digest:08X} != golden 0x{UNDERGROUND_DIGEST:08X}");
    assert_eq!(hash, UNDERGROUND_STATE_HASH, "wasm32 state_hash 0x{hash:08X} != golden 0x{UNDERGROUND_STATE_HASH:08X}");
}
