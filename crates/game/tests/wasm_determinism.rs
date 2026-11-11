//! Cross-target determinism: the same physics scene must end in the same
//! state on native and on wasm32 (node, via wasm-bindgen-test-runner) —
//! client and server run the sim on different targets. The scene is small and
//! uses no worldgen, so the wasm build stays light: a kinematic player capsule
//! walking and jumping over flat stud ground derived from recipes (with one
//! strip a brick course higher: an autostep up and down), one slime, and a
//! kinematic compound of two rotated cuboids whose mass properties go through
//! the eigen solve (std `acos`/`cos` in glamx). A second scene digs a shaft
//! through implied stud ground (strata, bedrock); a third detaches bricks
//! into a falling stud body.
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

use common::{body, fixed_body_mass_count, key, r0, spawn_slime, ticks, world_of, FLOOR_PLATES};
use game::rapier3d::math::Vector;
use glamx::Quat;
use game::rapier3d::prelude::{ColliderBuilder, RigidBodyBuilder, RigidBodyHandle};
use game::{state_hash, ColumnCoords, Key, Pose, Region, StudColumnGround, StudRegionSeed, STUD_CHUNK_STUDS};
use slotmapd::Key as _;

// Every region scene below was rebuilt on stud ground and re-recorded on
// 2026-11-10 (stud world plan P10b: the game's tests off voxel terrain): the
// scenes are new (stud recipes, bricks and the 2.2 m stud body instead of
// voxel chunks and the 1.8 m body), so the old values (scene 0x60B6_386A /
// 0xFC53_45B5, underground 0x6E9C_D4DF / 0x4F26_0F66, voxel body
// 0x52FC_9318 / 0xFD58_5519 at 16-block chunks) describe nothing any more.
// `physics_goldens` was rebuilt on stud ground in the same change.
// Re-recorded 2026-11-11 (P10b, the voxel code deleted from `game`): the
// hashed / serialized layout of `GameData` / `Ecs` lost its voxel fields
// (`chunk`, `voxel_body`, `chunk_index`, `ground`, `water_bodies`, `halo`,
// `halo_ground`) — a layout-only move: `physics_goldens` did not move.
// Before: scene 0x3F2D_FC5B / 0x8704_493E, underground 0x1CAA_BA0A /
// 0x3C72_DB95, stud body 0x6502_0AE7 / 0xC78F_A508. They move again
// whenever that layout does.
// Re-recorded 2026-11-11 (re-brick on exposure, `game::stud_rebrick`): the
// UNDERGROUND pair only — its scene digs, and the ground a dig exposes is
// now real bricks (other bricks, stud caps in the colliders). Before:
// 0x3A4E_50AC / 0x2241_D662. The other scenes dig nothing and did not move.
// Re-recorded 2026-11-11 (natural cliffs as pristine bricks): the recipe
// (`StudColumnGround`, hashed and serialized in `GameData.stud_ground`) gained
// its `apron` field — a layout move for all three scene pairs (these scenes'
// hand-made recipes have a closed apron, so no brick changed; `physics_goldens`
// did not move). Before: scene 0x5BCB_AA41 / 0xE5F7_54F5, underground
// 0xF908_B67A / 0x802D_8C79, stud body 0x9F93_B77D / 0xD498_D06F.
/// crc32 of the scene region's bincode bytes after [`SCENE_TICKS`] ticks,
/// on every target.
const WASM_SCENE_DIGEST: u32 = 0x55DE_C516;
/// `game::state_hash` of the same state, on every target.
const SCENE_STATE_HASH: u32 = 0xEA4C_C8F9;
const SCENE_TICKS: usize = 120;
/// crc32 of [`underground_scene`]'s region bytes, on every target.
const UNDERGROUND_DIGEST: u32 = 0xBFCD_2C74;
/// `game::state_hash` of the same state, on every target.
const UNDERGROUND_STATE_HASH: u32 = 0x360F_E7CE;

/// crc32 of [`stud_body_scene`]'s region bytes, on every target.
const STUD_BODY_DIGEST: u32 = 0x2E60_B044;
/// `game::state_hash` of the same state, on every target.
const STUD_BODY_STATE_HASH: u32 = 0xF132_A0D3;
/// Ticks of [`stud_body_scene`] after the detach: it has landed and tumbles.
const STUD_BODY_TICKS: usize = 30;

/// crc32 of `history::persist::encode` of [`history_scene`]'s bootstrapped
/// history, on every target. Depends on the region size (journey leg times
/// come from `REGION_BLOCKS`), so it is pinned at the test default of 64
/// blocks only, and on `TICK_RATE`.
const HISTORY_DIGEST: u32 = 0x4869_3884;

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

/// Whether this sim-chunk size has pinned values (16 blocks: the only size
/// stud chunks exist at); prints a skip line if not.
fn pinned_size() -> bool {
    let pinned = game::CHUNK_BLOCKS == 16;
    if !pinned {
        log(&format!("wasm_determinism: nothing pinned at {}-block chunks, comparison skipped", game::CHUNK_BLOCKS));
    }
    pinned
}

/// Runs the scene; returns `(portable digest, state_hash)`.
fn scene() -> (u32, u32) {
    // Units 64..320 each way around the spawn at (128, 128); the 32-unit
    // strip z 192..224 (stud rows 24..28: tile rows 12 and 13) lies one brick
    // course (`STUD_STEP_PLATES`) over the flat ground.
    let mut world = world_of(64.0..320.0, 64.0..320.0, |_, cz| {
        common::stud_ground(|_, sz| {
            let strip = (24..28).contains(&(cz * STUD_CHUNK_STUDS + sz));
            FLOOR_PLATES + if strip { game::STUD_STEP_PLATES } else { 0 }
        })
    });
    let drop = common::floor_y(FLOOR_PLATES) + 24.0;
    spawn_slime(world.regions.get_mut(&r0()).unwrap(), [176.0, drop, 112.0], 1, 0.0, 0);
    let compound = add_compound_body(&mut world);
    // S (+z) over the raised strip and back off it, then D (+x), with jumps.
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
    let y = common::floor_y(FLOOR_PLATES) + 24.0;
    world.regions.get_mut(&r0()).unwrap().with_data(|d| {
        let e = d.data.ecs.create_entity_safe();
        let body = RigidBodyBuilder::kinematic_position_based()
            .translation(Vector::new(240.0, y, 240.0))
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

/// Plates below this are bedrock in [`underground_scene`].
const UNDERGROUND_BEDROCK: i32 = -300;

/// A 2x2-column stud region of implied ground (three tilted strata over
/// bedrock) dug down a shaft that wanders across all four columns: each dig
/// materialises chunks from the recipe, the last lies in bedrock and is
/// refused. Returns `(digest, state_hash)`.
fn underground_scene() -> (u32, u32) {
    let n = STUD_CHUNK_STUDS;
    let column = || {
        // Stone over sand over dirt, the formations' tops stepping with the
        // stud column (`geo`): no layer of the shaft is one material.
        StudColumnGround::new(
            vec![FLOOR_PLATES; n * n].into(),
            vec![game::STUD_NO_WATER; n * n].into(),
            vec![0; n * n].into(),
            vec![common::SOIL],
            (0..n * n).map(|i| (i % 3) as i32).collect(),
            vec![(i32::MAX, game::MAT_STONE), (-100, game::MAT_SAND), (-200, game::MAT_DIRT)],
            UNDERGROUND_BEDROCK,
        )
        .expect("a recipe of known materials")
    };
    let ground = (0..2).flat_map(|x| (0..2).map(move |z| (ColumnCoords::new(x, z), column()))).collect();
    let mut region = common::region_of(r0(), StudRegionSeed { ground, features: Vec::new(), ..Default::default() });
    // The top plate of each dig; the last box lies wholly in bedrock.
    let ys = [30, 6, -24, -60, -96, -132, -168, -204, -240, -276, -294, UNDERGROUND_BEDROCK];
    let cw = n as i32;
    let count = |region: &Region| region.data().stud_index.len();
    let before = count(&region);
    for (i, y) in ys.into_iter().enumerate() {
        // The shaft wanders across the four columns around their shared
        // corner at stud (cw, cw): a tile-sized box, a tile deep.
        let (x, z) = (cw - 2 + 2 * (i as i32 % 2), cw - 2 + 2 * ((i as i32 % 3) / 2));
        let mut changed = false;
        region.with_data(|d| changed = !d.carve_stud_safe([x, y - 6, z], [x + 2, y, z + 2]).is_empty());
        assert_eq!(changed, i != ys.len() - 1, "dig {i} at plate {y}: only the bedrock dig is refused");
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

/// A column of eight 2 x 4 bricks with an arm at its top, built 12 plates over
/// flat stud ground and detached into a stud body (`game::stud_body`) spinning
/// about its base: a dynamic body whose explicit mass properties, rotation
/// integration, landing and compound-vs-terrain contacts all land in the
/// state. Returns `(digest, state_hash)`.
fn stud_body_scene() -> (u32, u32) {
    use game::stud_catalog::STUD_PARTS;
    let mut region = common::floor_region(r0(), 0.0..512.0, 0.0..512.0, FLOOR_PLATES);
    let part = STUD_PARTS.iter().position(|p| p.number == "3001").expect("the 2 x 4 brick") as u16;
    let at = common::sc(1, 0, 1);
    let base = FLOOR_PLATES as u8 + 12;
    let mut cells: Vec<[u8; 3]> = (0..8).map(|k| [8, base + 3 * k, 8]).collect();
    cells.push([12, base + 21, 8]);
    let mut body = None;
    region.with_data(|d| {
        for &min in &cells {
            assert!(d.place_stud_safe(at, game::StudBrick::part_at(min, part, 0, 4)), "brick at {min:?}");
        }
    });
    region.with_data(|d| {
        let picks = std::collections::BTreeMap::from([(at, cells.iter().copied().collect())]);
        let u = game::STUD_UNITS as f32;
        let pivot = Vector::new((n_studs(1) + 9.0) * u, common::floor_y(base as i32), (n_studs(1) + 9.0) * u);
        body = d.detach_studs_safe(&picks, game::SettlePolicy::Despawn, Vector::new(0.3, 0.0, -1.5), Some(pivot));
    });
    let body = body.expect("the column detaches");
    let start = *region.data().physics.bodies.get(*region.data().ecs.rigidbody.get(body)).unwrap().position();
    for _ in 0..STUD_BODY_TICKS {
        region.handle_event(game::GameEventKind::Tick).unwrap();
        region.forget_last_event();
    }
    let data = region.data();
    let end = *data.physics.bodies.get(*data.ecs.rigidbody.get(body)).unwrap().position();
    assert!(start.translation.y - end.translation.y > 16.0, "the body must fall: {start:?} -> {end:?}");
    assert!(end.rotation.angle_between(start.rotation) > 0.2, "and turn: {start:?} -> {end:?}");
    let digest = crc32fast::hash(&game::SerializedRegion::from_rollback(data).0);
    let hash = state_hash(data);
    if print_hash() {
        log(&format!("STUD_BODY_DIGEST = 0x{digest:08X}, STUD_BODY_STATE_HASH = 0x{hash:08X}"));
    }
    (digest, hash)
}

/// The first stud of chunk column `c` along an axis, as `f32`.
fn n_studs(c: usize) -> f32 {
    (c * STUD_CHUNK_STUDS) as f32
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
    let map = WorldMap { origin: RegionCoords::new(0, 0), width: w, height: h, tiles, rock_names: vec![], region_types: vec![] };
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

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn stud_body_hash_native() {
    let (digest, hash) = stud_body_scene();
    if !pinned_size() {
        return;
    }
    assert_eq!(digest, STUD_BODY_DIGEST, "native digest 0x{digest:08X} != golden 0x{STUD_BODY_DIGEST:08X}");
    assert_eq!(hash, STUD_BODY_STATE_HASH, "native state_hash 0x{hash:08X} != golden 0x{STUD_BODY_STATE_HASH:08X}");
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test::wasm_bindgen_test]
fn stud_body_hash_wasm() {
    let (digest, hash) = stud_body_scene();
    if !pinned_size() {
        return;
    }
    assert_eq!(digest, STUD_BODY_DIGEST, "wasm32 digest 0x{digest:08X} != golden 0x{STUD_BODY_DIGEST:08X}");
    assert_eq!(hash, STUD_BODY_STATE_HASH, "wasm32 state_hash 0x{hash:08X} != golden 0x{STUD_BODY_STATE_HASH:08X}");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn history_hash_native() {
    let digest = history_scene();
    if !history_pinned() {
        return;
    }
    assert_eq!(digest, HISTORY_DIGEST, "native history digest 0x{digest:08X} != golden 0x{HISTORY_DIGEST:08X}");
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test::wasm_bindgen_test]
fn history_hash_wasm() {
    let digest = history_scene();
    if !history_pinned() {
        return;
    }
    assert_eq!(digest, HISTORY_DIGEST, "wasm32 history digest 0x{digest:08X} != golden 0x{HISTORY_DIGEST:08X}");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn explicit_mass_compound_is_cross_target_native() {
    let digest = explicit_mass_scene();
    assert_eq!(digest, EXPLICIT_MASS_DIGEST, "native explicit mass digest 0x{digest:08X} != golden 0x{EXPLICIT_MASS_DIGEST:08X}");
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test::wasm_bindgen_test]
fn explicit_mass_compound_is_cross_target_wasm() {
    let digest = explicit_mass_scene();
    assert_eq!(digest, EXPLICIT_MASS_DIGEST, "wasm32 explicit mass digest 0x{digest:08X} != golden 0x{EXPLICIT_MASS_DIGEST:08X}");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn creature_mass_is_cross_target_native() {
    let digest = creature_mass_scene();
    assert_eq!(digest, CREATURE_MASS_DIGEST, "native creature mass digest 0x{digest:08X} != golden 0x{CREATURE_MASS_DIGEST:08X}");
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test::wasm_bindgen_test]
fn creature_mass_is_cross_target_wasm() {
    let digest = creature_mass_scene();
    assert_eq!(digest, CREATURE_MASS_DIGEST, "wasm32 creature mass digest 0x{digest:08X} != golden 0x{CREATURE_MASS_DIGEST:08X}");
}
