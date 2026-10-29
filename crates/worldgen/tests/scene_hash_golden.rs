//! Same-engine regression golden: a canonical scene — the generated region
//! (0, 0), one player, one slime, 60 ticks of scripted input — must end in
//! exactly [`SCENE_HASH`] (`game::state_hash`, whose Full-mode crc32 is the
//! default). Any change to worldgen, the sim or the physics engine that moves
//! a single bit of state shows up here; behaviour across an engine switch is
//! judged by `crates/game/tests/physics_goldens.rs` instead.
//!
//! The constant changes only in a commit whose message says why. Regenerate:
//!
//! ```sh
//! SCENE_HASH_PRINT=1 just test-one worldgen "--test scene_hash_golden -- --include-ignored --nocapture"
//! ```
//!
//! It lives in `worldgen` because `game` can't depend on it (worldgen
//! depends on game). It holds for the default build dials only (both pinned
//! chunk sizes, 2 and 16): another region size, cell mode, spawn island size or a forged world file is a
//! different world, and the test says so and skips.
// The game crate's movement rig: every read of physics-engine types.
#[path = "../../game/tests/common/mod.rs"]
mod common;

use common::{entity_body, spawn_slime};
use game::{state_hash, GameEventKind, InputEvent, Key, Region, RegionCoords};

/// `state_hash` after the scene's last tick. Re-recorded for the rapier 0.36
/// / parry 0.31 switch (with the continuous-surfaces tie-break, `dt` =
/// `TICK_DT` and the kinematic `can_sleep(false)` fix), then for the lazy
/// underground: columns carry `ColumnGround` recipes, keep their stone band
/// bottoms, fill the band's bottom sub-chunk, and reach down cliff faces;
/// then (already stale at 0xB42B_F7B3 after an earlier worldgen change) for
/// the `ChunkBlocks` uniform/dense grid, `u16` `BlockIndex` and boxed
/// `ColumnGround` footprints — hashed layout only, geometry unchanged.
/// Pinned per sim-chunk size (`game::CHUNK_BLOCKS`): the same world splits
/// into other chunk entities, bodies and colliders at 2 and 16 blocks.
const SCENE_HASH: u32 = match game::CHUNK_BLOCKS {
    2 => 0xDB24_FC10,
    16 => 0x8A57_7DEA,
    _ => 0,
};
/// Ticks of scripted input.
const SCENE_TICKS: usize = 60;

fn mats() -> worldgen::material::Materials {
    worldgen::material::Materials {
        stone: game::BlockId(2),
        dirt: game::BlockId(1),
        grass: game::BlockId(3),
        sand: game::BlockId(4),
        water: game::BlockId(5),
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
    }
}

fn default_dials() -> bool {
    matches!(game::CHUNK_BLOCKS, 2 | 16)
        && game::REGION_BLOCKS == 64
        && game::CELL_CHUNKS == 0
        && option_env!("VOXEL_ISLAND_CELLS").is_none_or(|v| v.is_empty() || v == "0" || v == "16")
        && std::env::var_os(worldgen::forged::WORLD_FILE_ENV).is_none_or(|v| v.is_empty())
}

/// Events of the scene: controls on, settle, then walk W, D, S with a jump,
/// one key at a time — [`SCENE_TICKS`] ticks in all.
fn script() -> Vec<GameEventKind> {
    let key = |key, pressed| GameEventKind::PlayerInput(0, InputEvent::Key { key, pressed });
    let ticks = |n| std::iter::repeat_with(|| GameEventKind::Tick).take(n);
    let mut events = vec![key(Key::KeyE, true), key(Key::KeyE, false)];
    events.extend(ticks(15));
    for dir in [Key::KeyW, Key::KeyD, Key::KeyS] {
        events.push(key(dir, true));
        events.extend(ticks(5));
        events.push(key(Key::Space, true));
        events.extend(ticks(1));
        events.push(key(Key::Space, false));
        events.extend(ticks(9));
        events.push(key(dir, false));
    }
    events
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn canonical_scene_state_hash() {
    if !default_dials() {
        eprintln!("scene_hash_golden: non-default world dials, skipped");
        return;
    }
    let rc = RegionCoords::new(0, 0);
    let mut region = Region::from_columns(rc, worldgen::generate_region(rc, worldgen::WORLD_SEED, mats()));
    let apply = |region: &mut Region, ev| {
        region.handle_event(ev).unwrap();
        region.forget_last_event();
    };
    apply(&mut region, GameEventKind::CreateClient(0));
    let player = |region: &Region| {
        let b = entity_body(region.data(), *region.data().player_entites.get(&0).unwrap());
        [b.x, b.z]
    };
    let spawn = player(&region);
    // One slime, a few blocks off the player's spawn, dropped onto the surface.
    let (x, z) = (spawn[0] + 48.0, spawn[1] + 48.0);
    let surface = region.data().surface_voxel_y(x as i32, z as i32).expect("land next to the spawn");
    spawn_slime(&mut region, [x, surface as f32 + 24.0, z], 1, 0.0, 0);
    let start = player(&region);
    let events = script();
    assert_eq!(events.iter().filter(|e| matches!(e, GameEventKind::Tick)).count(), SCENE_TICKS);
    for ev in events {
        apply(&mut region, ev);
    }
    let end = player(&region);
    let walked = ((end[0] - start[0]).powi(2) + (end[1] - start[1]).powi(2)).sqrt();
    assert!(walked > 16.0, "the scripted walk must move the player: {start:?} -> {end:?}");
    let hash = state_hash(region.data());
    if std::env::var_os("SCENE_HASH_PRINT").is_some() {
        eprintln!("SCENE_HASH = 0x{hash:08X}");
    }
    assert_eq!(hash, SCENE_HASH, "canonical scene hash 0x{hash:08X} != golden 0x{SCENE_HASH:08X}");
}
