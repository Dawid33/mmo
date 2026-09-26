//! Cross-target determinism: the same physics scene must end in the same
//! state on native and on wasm32 (node, via wasm-bindgen-test-runner) —
//! client and server run the sim on different targets. The scene is small and
//! uses no worldgen, so the wasm build stays light: a kinematic player capsule
//! walking and jumping over 8×8 chunks of hand-built `Voxels` floor (block
//! scale, plus one row of half blocks for chisel scale), and one slime.
//!
//! The cross-target digest is [`WASM_SCENE_DIGEST`], crc32 of the region's
//! canonical bincode bytes (`SerializedRegion`, the parking and wire format:
//! bincode writes `usize` as u64 everywhere). `game::state_hash` can't be
//! compared across targets: std `Hash` writes `usize` at the target's pointer
//! width — every length prefix, and `[usize]` slices as raw memory — so it
//! is pinned native-only, as [`NATIVE_STATE_HASH`].
//!
//! Run both sides with `just determinism-wasm`. The constants change only in
//! a commit whose message says why; print them with
//!
//! ```sh
//! WASM_SCENE_HASH_PRINT=1 just test-one game "--test wasm_determinism -- --nocapture"
//! ```
mod common;

use common::{body, fixed_body_mass_count, key, r0, spawn_slime, ticks, world_of};
use game::{state_hash, Chunk, Key};

/// crc32 of the scene region's bincode bytes after [`SCENE_TICKS`] ticks,
/// on every target.
const WASM_SCENE_DIGEST: u32 = 0x105E_C680;
/// `game::state_hash` of the same state on 64-bit native.
const NATIVE_STATE_HASH: u32 = 0xA93B_9D8C;
const SCENE_TICKS: usize = 120;

/// Runs the scene; returns `(portable digest, state_hash)`.
fn scene() -> (u32, u32) {
    // Chunks 2..=9 each way: voxels 64..320 around the spawn at (128, 128);
    // row 6 is half blocks (top 24) over the block-scale floor (top 16).
    let mut world = world_of(2..=9, 2..=9, |_, z| Chunk::flat_floor(if z == 6 { 24 } else { 16 }));
    spawn_slime(world.regions.get_mut(&r0()).unwrap(), [176.0, 40.0, 112.0], 1, 0.0, 0);
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
    assert!(fixed >= 64 && massive == 0, "{massive} of {fixed} fixed terrain bodies computed mass properties");
    let digest = crc32fast::hash(&game::SerializedRegion::from_rollback(data).0);
    let hash = state_hash(data);
    if print_hash() {
        log(&format!("WASM_SCENE_DIGEST = 0x{digest:08X}, state_hash = 0x{hash:08X}"));
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
    assert_eq!(digest, WASM_SCENE_DIGEST, "native scene digest 0x{digest:08X} != golden 0x{WASM_SCENE_DIGEST:08X}");
    assert_eq!(hash, NATIVE_STATE_HASH, "native state_hash 0x{hash:08X} != golden 0x{NATIVE_STATE_HASH:08X}");
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test::wasm_bindgen_test]
fn scene_hash_wasm() {
    let (digest, _) = scene();
    assert_eq!(digest, WASM_SCENE_DIGEST, "wasm32 scene digest 0x{digest:08X} != golden 0x{WASM_SCENE_DIGEST:08X}");
}
