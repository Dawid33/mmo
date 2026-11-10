//! Same-engine regression golden: a canonical scene — the generated stud
//! region (0, 0) of the procedural world (ground recipes and trees), one
//! player, one slime, 60 ticks of scripted input — must end in exactly
//! [`SCENE_HASH`] (`game::state_hash`, whose Full-mode crc32 is the default).
//! Any change to worldgen, the sim or the physics engine that moves a single
//! bit of state shows up here; behaviour across an engine switch is judged by
//! `crates/game/tests/physics_goldens.rs` instead.
//!
//! The constant changes only in a commit whose message says why. Regenerate:
//!
//! ```sh
//! SCENE_HASH_PRINT=1 just test-one worldgen "--test it -- --include-ignored --nocapture scene_hash_golden"
//! ```
//!
//! It lives in `worldgen` because `game` can't depend on it (worldgen
//! depends on game). It holds for the default build dials only: another
//! region size, chunk size, spawn island size or a forged world file is a
//! different world, and the test says so and skips.
// The game crate's movement rig: every read of physics-engine types.
#[path = "../../game/tests/common/mod.rs"]
mod common;

use common::{entity_body, region_of, spawn_slime};
use game::{state_hash, GameEventKind, InputEvent, Key, Region, RegionCoords};

/// `state_hash` after the scene's last tick.
///
/// Recorded 2026-11-10 (stud world P10b): the scene was rebuilt on stud ground
/// (`generate_stud_region` -> `Region::seed_stud_world`) when worldgen's voxel
/// generator was deleted, so this value has no predecessor to compare with —
/// the voxel scene's history (last value 0xD0F2_E7D3) is in git up to
/// `6251fa09`. `physics_goldens` judges trajectories.
const SCENE_HASH: u32 = match game::CHUNK_BLOCKS {
    16 => 0x6D9E_67C7,
    _ => 0,
};
/// Ticks of scripted input.
const SCENE_TICKS: usize = 60;

fn default_dials() -> bool {
    game::CHUNK_BLOCKS == 16
        && game::REGION_BLOCKS == 64
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
    // The real block props before any state: the starting kit (2 chests)
    // reads them, and other tests in this binary install them too — the
    // scene must not depend on whether one ran first.
    common::install_real_block_props();
    if !default_dials() {
        eprintln!("scene_hash_golden: non-default world dials, skipped");
        return;
    }
    let rc = RegionCoords::new(0, 0);
    let mut region = region_of(rc, worldgen::stud_gen::generate_stud_region(rc, worldgen::WORLD_SEED));
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
    let surface = region.data().stud_surface_y(x, z).expect("ground next to the spawn");
    spawn_slime(&mut region, [x, surface + 24.0, z], 1, 0.0, 0);
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
