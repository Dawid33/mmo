//! Cross-target determinism: the same physics scene must end in the same
//! state on native and on wasm32 (node, via wasm-bindgen-test-runner) —
//! client and server run the sim on different targets. The scene is small and
//! uses no worldgen, so the wasm build stays light: a kinematic player capsule
//! walking and jumping over 8×8 chunks of hand-built `Voxels` floor (block
//! scale, plus one row of half blocks for chisel scale), one slime, and a
//! kinematic compound of two rotated cuboids whose mass properties go through
//! the eigen solve (std `acos`/`cos` in glamx).
//!
//! A fourth scene bootstraps a small synthetic world history
//! (`game::history`) and pins crc32 of its encoded history file.
//! A fifth pins the explicit mass properties of a compound collider spec
//! (`game::mass_props`: our own Jacobi frame, no glamx eigen solve).
//! A sixth pins the dog's creature body (`game::creature_shapes`: the
//! embedded manifest's decimals parsed by serde_json) and its mass properties.
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
/// Then for the `Ecs` `animator` / `bones` component slots (sim-owned
/// animation, `game::anim`), and again for players being rigged dwarves
/// (an animator + one collider per bone; the scene's player walks). Then
/// for cell mode's removal (`GameData` lost `cells` / `cell_edits`; the
/// 16-block values only — the 2-block ones still need a re-record). Then
/// for the `Ecs` `voxel_body` component slot (`game::voxel_body`; layout
/// only — no body in this scene; 16-block values only).
/// Then for `Client.mode` (`game::adventure`; the scene's player is a
/// client; 16-block values only). Then for the `Ecs` `walker` component
/// slot (`game::adventure`; layout only — nobody walks; 16-block values only). Then for
/// `GameData.item_piles` / `pile_expiry` (`game::item_piles`; layout only —
/// no pile in these scenes; 16-block values only). Then for `Client.mining` /
/// `break_cooldown_until` (`game::mining`; layout only — nobody mines; 16-block
/// values only). Then for `GameData.containers` / `Client.open_container`
/// (`game::containers`) and the starting kit's 2 chests now that the
/// manifest has a `chest` (16-block values only). Then for `GameData.region` /
/// `next_entity_serial` / `id_index` and the `Ecs` `id` slot (`EntityId`;
/// layout only — 16-block values only). Then for the entity halo: `ghosts`
/// became `halo_entities` (entries keep kind + collider, no TTL tick) and
/// `GameData.halo_sent` was added (layout only — no mirror in these scenes;
/// 16-block values only). Then for item piles keyed by `WorldBlockPos` and
/// the new `GameData.halo_piles` / `pile_takes` (`game::item_piles`, Task
/// 6.3; layout only — no pile in these scenes; 16-block values only). Then
/// for chests keyed by `WorldBlockPos`, the new `GameData.halo_containers`
/// and `Client.open_container` as a `WorldBlockPos` (`game::containers`, Task
/// 6.4; layout only — no chest in these scenes; 16-block values only).
/// Then for the slime spawner dropping its edge margin (Task 6.6: the
/// region's tick-100 spawn attempt now picks a column off this floor, so the
/// second slime the scene used to get no longer spawns; 16-block values
/// only). Then for `Motion.immersion` / `Motion.blocked_horizontally` (water
/// plan Task 3; layout only — with both left out of the hash the state hash
/// is the previous 0x4C3E_CFAB; 16-block values only). Then for
/// `ColumnGround.water_top` / `water_body` and `GameData.water_bodies`
/// (water plan Task 5, recipe water; serialized bytes + hashed layout only —
/// with the three left out of the hash every state hash here is the previous
/// one; 16-block values only, the voxel body scene's too).
///
/// Every value is pinned per sim-chunk size (`game::CHUNK_BLOCKS`, a build
/// dial): the same physical scene splits into different chunk entities,
/// bodies and colliders at 2 and 16 blocks, so its bytes and hashes differ.
/// Other sizes are not pinned and skip the comparison.
const WASM_SCENE_DIGEST: u32 = pinned(0xFA8E_102F, 0x25C3_6020);
/// `game::state_hash` of the same state, on every target.
const SCENE_STATE_HASH: u32 = pinned(0x5233_40BE, 0xC939_1267);
const SCENE_TICKS: usize = 120;
/// crc32 of [`underground_scene`]'s region bytes, on every target.
const UNDERGROUND_DIGEST: u32 = pinned(0x5448_586C, 0x8439_7A8D);
/// `game::state_hash` of the same state, on every target.
const UNDERGROUND_STATE_HASH: u32 = pinned(0x4080_B196, 0x4472_2ECB);

/// crc32 of [`voxel_body_scene`]'s region bytes, on every target.
/// Re-recorded for Task 7.5 (voxel bodies cross seams): the body is now an
/// `EntityKind::Body` with an `EntityId` (`Ecs.kind` / `id`, `id_index`,
/// `next_entity_serial`) and `VoxelBody` gained `drops` — state layout only,
/// the same detach and fall (16-block values only). Re-recorded 2026-11-19
/// (16-block values only): parry's voxels-vs-compound contacts now look
/// `prediction` ahead (`contact_manifolds_voxels_composite_shape`, mmo-fork),
/// which changes the body's contacts with the terrain during its fall.
const VOXEL_BODY_DIGEST: u32 = pinned(0x0F43_FFD4, 0x4F4C_35F6);
/// `game::state_hash` of the same state, on every target.
const VOXEL_BODY_STATE_HASH: u32 = pinned(0x8D9E_A328, 0x566F_DF76);
/// Ticks of [`voxel_body_scene`] after the detach: mid-fall (it rests and
/// despawns at ~50).
const VOXEL_BODY_TICKS: usize = 30;

/// crc32 of `history::persist::encode` of [`history_scene`]'s bootstrapped
/// history, on every target. Depends on the region size (journey leg times
/// come from `REGION_BLOCKS`), so it is pinned at the test default of 64
/// blocks only, and on `TICK_RATE`. Re-recorded for history file v5 (`Site` lost its
/// player-founded flag).
const HISTORY_DIGEST: u32 = 0x4827_CEB2;

/// crc32 of the field bits of `game::mass_props::mass_properties` of
/// [`l_shape`] (two cuboids, one turned 30° about Y) at
/// `BODY_DEFAULT_DENSITY`, on every target: the compound's own Jacobi frame
/// (`jacobi_eigen_sym3`) and `Quat::from_mat3` must agree bit for bit.
/// Independent of the chunk and region size.
const EXPLICIT_MASS_DIGEST: u32 = 0x1A48_699B;

/// crc32 of the field bits of the dog's `creature_shape` body parts (pose
/// translations + half extents) and of their `mass_properties` at
/// `CREATURE_DENSITY`, on every target. Changes when the dog's model (or
/// `build.py`'s collider rule) does. Independent of the chunk and region size.
const CREATURE_MASS_DIGEST: u32 = 0x62EC_1C69;

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

/// A stone column with an arm, detached into a voxel body (`game::voxel_body`)
/// toppling about its base (`detach_blocks_pivoting_safe`: tall enough that
/// its spin is raised to go over, with `sqrt`) onto a block floor: a dynamic
/// body whose explicit mass properties, rotation integration, landing and
/// compound-vs-voxels contacts all land in the state. Returns
/// `(digest, state_hash)`.
fn voxel_body_scene() -> (u32, u32) {
    let chunks = common::floor_chunks(0.0..512.0, 0.0..512.0, game::BLOCK_VOXELS as u32);
    let mut region = Region::from_chunks(r0(), chunks);
    let mut blocks: Vec<[i32; 3]> = (1..=8).map(|y| [16, y, 16]).collect();
    blocks.extend([[17, 8, 16], [18, 8, 16]]);
    let mut body = None;
    region.with_data(|d| {
        for &p in &blocks {
            assert!(d.set_block_safe(p, Block::new(BlockId(2))));
        }
    });
    region.with_data(|d| {
        let b = game::BLOCK_VOXELS as f32;
        let pivot = Vector::new(16.5 * b, b, 16.5 * b);
        body = d.detach_blocks_pivoting_safe(&blocks, game::SettlePolicy::Despawn, Vector::new(0.1, 0.0, -0.6), Some(pivot));
    });
    let body = body.expect("the column detaches");
    let start = *region.data().physics.bodies.get(*region.data().ecs.rigidbody.get(body)).unwrap().position();
    for _ in 0..VOXEL_BODY_TICKS {
        region.handle_event(game::GameEventKind::Tick).unwrap();
        region.forget_last_event();
    }
    let data = region.data();
    let end = *data.physics.bodies.get(*data.ecs.rigidbody.get(body)).unwrap().position();
    assert!(start.translation.y - end.translation.y > 16.0, "the body must fall: {start:?} -> {end:?}");
    let digest = crc32fast::hash(&game::SerializedRegion::from_rollback(data).0);
    let hash = state_hash(data);
    if print_hash() {
        log(&format!("VOXEL_BODY_DIGEST = 0x{digest:08X}, VOXEL_BODY_STATE_HASH = 0x{hash:08X}"));
    }
    (digest, hash)
}

/// A 16×16 synthetic world map: land with a sea border and a sea channel at
/// x = 8 for z < 10, two civs of 20 founders, 20 years of history (births,
/// marriages, deaths, migration, maybe an embark). Returns crc32 of the
/// encoded history file.
fn history_scene() -> u32 {
    use game::history::bootstrap::{bootstrap, BootstrapParams};
    use game::{Biome, RegionCoords, TileSummary, WorldMap};
    let (w, h) = (16u32, 16u32);
    let mut tiles = Vec::new();
    for z in 0..h as i32 {
        for x in 0..w as i32 {
            let sea = x == 0 || z == 0 || x == w as i32 - 1 || z == h as i32 - 1 || (x == 8 && z < 10);
            tiles.push(TileSummary {
                ground_min_m: 0.0,
                ground_mean_m: 10.0 + x as f32,
                ground_max_m: 50.0 + (x * z) as f32,
                relief_m: 50.0 + (x * z) as f32,
                slope: [0.0, 0.0],
                sea_fraction: if sea { 1.0 } else { 0.0 },
                lake_fraction: if (x + z) % 7 == 0 { 0.1 } else { 0.0 },
                lake_level_m: None,
                rivers: ((x + 2 * z) % 3) as u16,
                river_max_discharge_m2: 0.0,
                river_max_width_m: 0.0,
                surface_rock: None,
                landing: Some([256, 256]),
                biome: if sea { Biome::Ocean } else { Biome::Grassland },
                region: 0,
                temperature_c: 15.0,
                precip_mm: 800.0,
                savagery: 0,
                alignment: 0,
                volcano: false,
            });
        }
    }
    let map = WorldMap { origin: RegionCoords::new(0, 0), width: w, height: h, tiles, rock_names: vec![], region_types: vec![], wrap_x: false };
    let params = BootstrapParams { seed: 0x5EED, civs: 2, founders_per_civ: 20, years: 20, civ_min_spacing_regions: 6, ..BootstrapParams::default() };
    let history = bootstrap(&map, &params).expect("the scene bootstraps");
    assert!(history.log().len() > 40, "the scene must have happened: {} events", history.log().len());
    let digest = crc32fast::hash(&game::history::persist::encode(&history));
    if print_hash() {
        log(&format!("HISTORY_DIGEST = 0x{digest:08X} ({} events, {} people)", history.log().len(), history.people().count()));
    }
    digest
}

/// The L-shape of `tests/mass_props.rs`, its rotation given by explicit
/// components (no `sin`/`cos`) so the input is the same bits everywhere.
fn l_shape() -> game::ColliderSpec {
    use game::ColliderSpec;
    let rot = Quat::from_xyzw(0.0, 0.258_819_04, 0.0, 0.965_925_8).normalize();
    ColliderSpec::Compound(vec![
        (Pose::from_translation(Vector::new(0.0, 8.0, 0.0)), ColliderSpec::Cuboid { half_extents: Vector::new(8.0, 8.0, 8.0) }),
        (Pose::from_parts(Vector::new(24.0, 4.0, 2.0), rot), ColliderSpec::Cuboid { half_extents: Vector::new(16.0, 4.0, 6.0) }),
    ])
}

/// crc32 of every field's bits of the L-shape's explicit mass properties.
fn explicit_mass_scene() -> u32 {
    let mp = game::mass_props::mass_properties(&l_shape(), game::mass_props::BODY_DEFAULT_DENSITY);
    let (c, i, q) = (mp.local_com, mp.inv_principal_inertia, mp.principal_inertia_local_frame);
    let mut bytes = Vec::new();
    for v in [c.x, c.y, c.z, mp.inv_mass, i.x, i.y, i.z, q.x, q.y, q.z, q.w] {
        bytes.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    let digest = crc32fast::hash(&bytes);
    if print_hash() {
        log(&format!("EXPLICIT_MASS_DIGEST = 0x{digest:08X} ({mp:?})"));
    }
    digest
}

/// crc32 of the dog's body parts and its explicit mass properties.
fn creature_mass_scene() -> u32 {
    use game::creature_shapes::{creature_shape, CREATURE_DENSITY};
    let body = &creature_shape("dog").expect("the dog has a model").body;
    let game::ColliderSpec::Compound(parts) = body else { panic!("the body is a compound") };
    let mut bits = Vec::new();
    for (pose, part) in parts {
        let game::ColliderSpec::Cuboid { half_extents: h } = part else { panic!("body parts are cuboids") };
        let t = pose.translation;
        bits.extend([t.x, t.y, t.z, h.x, h.y, h.z]);
    }
    let mp = game::mass_props::mass_properties(body, CREATURE_DENSITY);
    let (c, i, q) = (mp.local_com, mp.inv_principal_inertia, mp.principal_inertia_local_frame);
    bits.extend([c.x, c.y, c.z, mp.inv_mass, i.x, i.y, i.z, q.x, q.y, q.z, q.w]);
    let bytes: Vec<u8> = bits.iter().flat_map(|v| v.to_bits().to_le_bytes()).collect();
    let digest = crc32fast::hash(&bytes);
    if print_hash() {
        log(&format!("CREATURE_MASS_DIGEST = 0x{digest:08X} ({} parts, {mp:?})", parts.len()));
    }
    digest
}

/// Whether the region size has a pinned history digest; prints a skip line if not.
fn history_pinned() -> bool {
    let pinned = game::REGION_BLOCKS == 64;
    if !pinned {
        log(&format!("wasm_determinism: no history digest at {}-block regions, comparison skipped", game::REGION_BLOCKS));
    }
    pinned
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

/// Gate: native and wasm32 runs hash identically (`just determinism-wasm`)
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

/// Gate: native and wasm32 runs hash identically (`just determinism-wasm`)
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

/// Gate: native and wasm32 runs hash identically (`just determinism-wasm`)
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

/// Gate: native and wasm32 runs hash identically (`just determinism-wasm`)
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

/// Gate: native and wasm32 runs hash identically (`just determinism-wasm`)
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn voxel_body_hash_native() {
    let (digest, hash) = voxel_body_scene();
    if !pinned_size() {
        return;
    }
    assert_eq!(digest, VOXEL_BODY_DIGEST, "native digest 0x{digest:08X} != golden 0x{VOXEL_BODY_DIGEST:08X}");
    assert_eq!(hash, VOXEL_BODY_STATE_HASH, "native state_hash 0x{hash:08X} != golden 0x{VOXEL_BODY_STATE_HASH:08X}");
}

/// Gate: native and wasm32 runs hash identically (`just determinism-wasm`)
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test::wasm_bindgen_test]
fn voxel_body_hash_wasm() {
    let (digest, hash) = voxel_body_scene();
    if !pinned_size() {
        return;
    }
    assert_eq!(digest, VOXEL_BODY_DIGEST, "wasm32 digest 0x{digest:08X} != golden 0x{VOXEL_BODY_DIGEST:08X}");
    assert_eq!(hash, VOXEL_BODY_STATE_HASH, "wasm32 state_hash 0x{hash:08X} != golden 0x{VOXEL_BODY_STATE_HASH:08X}");
}

/// Gate: native and wasm32 runs hash identically (`just determinism-wasm`)
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn history_hash_native() {
    let digest = history_scene();
    if !history_pinned() {
        return;
    }
    assert_eq!(digest, HISTORY_DIGEST, "native history digest 0x{digest:08X} != golden 0x{HISTORY_DIGEST:08X}");
}

/// Gate: native and wasm32 runs hash identically (`just determinism-wasm`)
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test::wasm_bindgen_test]
fn history_hash_wasm() {
    let digest = history_scene();
    if !history_pinned() {
        return;
    }
    assert_eq!(digest, HISTORY_DIGEST, "wasm32 history digest 0x{digest:08X} != golden 0x{HISTORY_DIGEST:08X}");
}

/// Gate: native and wasm32 runs hash identically (`just determinism-wasm`)
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn explicit_mass_compound_is_cross_target_native() {
    let digest = explicit_mass_scene();
    assert_eq!(digest, EXPLICIT_MASS_DIGEST, "native explicit mass digest 0x{digest:08X} != golden 0x{EXPLICIT_MASS_DIGEST:08X}");
}

/// Gate: native and wasm32 runs hash identically (`just determinism-wasm`)
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test::wasm_bindgen_test]
fn explicit_mass_compound_is_cross_target_wasm() {
    let digest = explicit_mass_scene();
    assert_eq!(digest, EXPLICIT_MASS_DIGEST, "wasm32 explicit mass digest 0x{digest:08X} != golden 0x{EXPLICIT_MASS_DIGEST:08X}");
}

/// Gate: native and wasm32 runs hash identically (`just determinism-wasm`)
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn creature_mass_is_cross_target_native() {
    let digest = creature_mass_scene();
    assert_eq!(digest, CREATURE_MASS_DIGEST, "native creature mass digest 0x{digest:08X} != golden 0x{CREATURE_MASS_DIGEST:08X}");
}

/// Gate: native and wasm32 runs hash identically (`just determinism-wasm`)
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test::wasm_bindgen_test]
fn creature_mass_is_cross_target_wasm() {
    let digest = creature_mass_scene();
    assert_eq!(digest, CREATURE_MASS_DIGEST, "wasm32 creature mass digest 0x{digest:08X} != golden 0x{CREATURE_MASS_DIGEST:08X}");
}

/// Gate: native and wasm32 runs hash identically (`just determinism-wasm`)
/// A `ClientId` is 64 bits on every target (wasm32's `usize` is 32).
#[test]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
fn client_id_above_u32_round_trips() {
    let id: game::ClientId = 1 << 40;
    let packet = game::ServerPacket::PlayerRegion(None, id);
    let back: game::ServerPacket = game::decode_packet(&game::encode_packet(&packet).unwrap()).unwrap();
    assert!(matches!(back, game::ServerPacket::PlayerRegion(None, got) if got == id), "{back:?}");
}
