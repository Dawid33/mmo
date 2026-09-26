//! Behaviour goldens for movement, recorded on the nalgebra-era physics
//! (rapier 0.31 / parry 0.25 forks) before the rapier 0.36 switch. A physics
//! engine change can't keep hashes bit-exact, but it must keep behaviour:
//! each scenario drives a body through the real region tick for
//! [`SCENARIO_TICKS`] ticks and compares its per-tick `(x, y, z, grounded)`
//! against `tests/fixtures/physics_goldens/<scenario>.csv`, within
//! [`POS_TOL`] per axis and with [`GROUNDED_SLACK_TICKS`] of slack on
//! `grounded` around a recorded transition.
//!
//! Scenarios use only game-level inputs (key presses through the region tick,
//! `set_body_pose_safe`, `spawn_slime_safe`), so the same source runs on the
//! new engine; `common` holds every read of engine types.
//!
//! Re-record — only on the engine the goldens are meant to describe, and in a
//! commit that says why:
//!
//! ```sh
//! PHYSICS_GOLDENS_RECORD=1 just test-one game "--test physics_goldens"
//! ```
mod common;

use std::fmt::Write as _;

use common::{body, entity_body, key, r0, spawn_slime, teleport, ticks, world_of, Body};
use game::movement::{CHARACTER_SKIN, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS, UNITS_PER_METER};
use game::{Block, BlockId, Chunk, GameEventKind, Key, Region, World, BLOCK_VOXELS};

/// Ticks recorded per scenario.
const SCENARIO_TICKS: usize = 120;
/// Per-axis position tolerance per tick: 0.05 units, about 3 mm at 16 units/m
/// (the upgrade plan's value; no allowance for drift or a shifted landing).
const POS_TOL: f32 = 0.05 * UNITS_PER_METER / 16.0;
/// `grounded` may disagree with the golden within this many ticks of a
/// recorded grounded/airborne transition.
const GROUNDED_SLACK_TICKS: usize = 2;

type Row = [f32; 4];

fn row(b: Body) -> Row {
    [b.x, b.y, b.z, b.grounded as u8 as f32]
}

/// Height of a player's body origin resting on a floor whose top is `floor`.
fn rest_y(floor: f32) -> f32 {
    floor + PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS + CHARACTER_SKIN
}

/// Runs the player for [`SCENARIO_TICKS`] ticks, calling `input(world, tick)`
/// before each one, and records the body after each.
fn run_player(mut world: World, input: impl Fn(&mut World, usize)) -> Vec<Row> {
    (0..SCENARIO_TICKS)
        .map(|t| {
            input(&mut world, t);
            ticks(&mut world, 1);
            row(body(&world))
        })
        .collect()
}

/// Presses `k` just before tick `at` and releases it just before `at + 1`.
fn tap(world: &mut World, k: Key, at: usize, tick: usize) {
    if tick == at {
        key(world, k, true);
    } else if tick == at + 1 {
        key(world, k, false);
    }
}

/// +x at walk speed on a block-scale floor, from 2 units before the x = 160
/// chunk boundary, across a dozen more.
fn walk_flat_across_chunk_seam() -> Vec<Row> {
    let mut world = world_of(3..=18, 3..=4, |_, _| Chunk::flat_floor(16));
    let y = body(&world).y;
    teleport(&mut world, 158.0, y, 136.0);
    key(&mut world, Key::KeyD, true);
    run_player(world, |_, _| {})
}

/// +z from a block-scale floor (top 16) onto half blocks (one-cuboid
/// chisels, top 24) at z = 160: a scale seam and a half-block autostep.
fn walk_scale_seam() -> Vec<Row> {
    let world = world_of(3..=4, 3..=17, |_, z| Chunk::flat_floor(if z >= 5 { 24 } else { 16 }));
    run_player(world, |w, t| {
        if t == 0 {
            key(w, Key::KeyS, true);
        }
    })
}

/// +z into a one-block (16-unit) ledge at z = 160, jumping just before it:
/// a block is taller than the step height, so it takes a jump, as in
/// Minecraft.
fn step_up_one_block() -> Vec<Row> {
    let world = world_of(3..=4, 3..=17, |_, z| Chunk::flat_floor(if z >= 5 { 32 } else { 16 }));
    run_player(world, |w, t| {
        if t == 0 {
            key(w, Key::KeyS, true);
        }
        tap(w, Key::Space, 4, t);
    })
}

/// +z up one-voxel terraces, a step up per chunk: the gentle generated slope
/// on which walking uphill dips in speed at each terrace.
fn terrace_uphill_1_voxel() -> Vec<Row> {
    let world = world_of(3..=4, 3..=17, |_, z| Chunk::flat_floor(12 + z as u32));
    run_player(world, |w, t| {
        if t == 0 {
            key(w, Key::KeyS, true);
        }
    })
}

/// Space tapped once, standing on a flat floor: one jump, then standing.
fn jump_in_place() -> Vec<Row> {
    let world = world_of(3..=4, 3..=4, |_, _| Chunk::flat_floor(16));
    run_player(world, |w, t| tap(w, Key::Space, 0, t))
}

/// Sprinting forward (−z) over a one-block trench (z 560..576, one block
/// deep) with a jump just before its edge, then on to the end.
fn sprint_jump_over_gap() -> Vec<Row> {
    let trench_row = 17;
    let floor = |_: usize, z: usize| {
        let mut chunk = Chunk::flat_floor(2 * BLOCK_VOXELS as u32);
        if z == trench_row {
            for bx in 0..2 {
                chunk.set_block(bx, 1, 1, Block::new(BlockId::AIR));
            }
        }
        chunk
    };
    let mut world = world_of(3..=4, 2..=19, floor);
    teleport(&mut world, 144.0, rest_y(2.0 * BLOCK_VOXELS as f32), 620.0);
    ticks(&mut world, 2);
    key(&mut world, Key::ControlLeft, true);
    key(&mut world, Key::KeyW, true);
    run_player(world, |w, t| tap(w, Key::Space, 8, t))
}

/// Starts 0.3 units sunk into a flat floor — into the solid, past the skin
/// gap it normally rests on — stands, walks +z, stands.
fn sunk_start_depenetrates() -> Vec<Row> {
    let mut world = world_of(3..=4, 3..=6, |_, _| Chunk::flat_floor(16));
    teleport(&mut world, 136.0, rest_y(16.0) - CHARACTER_SKIN - 0.3, 136.0);
    run_player(world, |w, t| {
        if t == 20 {
            key(w, Key::KeyS, true);
        } else if t == 40 {
            key(w, Key::KeyS, false);
        }
    })
}

/// One size-1 slime (spawn tick 0, so a fixed roll sequence) dropped on a
/// flat floor with no player: it lands, sits and hops.
fn slime_hop_sequence() -> Vec<Row> {
    let mut chunks = Vec::new();
    for x in 2..=8 {
        for z in 2..=8 {
            chunks.push((game::ChunkCoords::new(x, 0, z), Chunk::flat_floor(16)));
        }
    }
    let mut region = Region::from_chunks(r0(), chunks);
    let slime = spawn_slime(&mut region, [176.0, 40.0, 176.0], 1, 0.0, 0);
    (0..SCENARIO_TICKS)
        .map(|_| {
            region.handle_event(GameEventKind::Tick).unwrap();
            region.forget_last_event();
            row(entity_body(region.data(), slime))
        })
        .collect()
}

/// Per tick `(x, y, z, grounded as f32)` of the scenario's tracked body.
fn run_scenario(name: &str) -> Vec<Row> {
    match name {
        "walk_flat_across_chunk_seam" => walk_flat_across_chunk_seam(),
        "walk_scale_seam" => walk_scale_seam(),
        "step_up_one_block" => step_up_one_block(),
        "terrace_uphill_1_voxel" => terrace_uphill_1_voxel(),
        "jump_in_place" => jump_in_place(),
        "sprint_jump_over_gap" => sprint_jump_over_gap(),
        "sunk_start_depenetrates" => sunk_start_depenetrates(),
        "slime_hop_sequence" => slime_hop_sequence(),
        _ => panic!("unknown scenario {name}"),
    }
}

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/physics_goldens")
        .join(format!("{name}.csv"))
}

/// `tick,x,y,z,grounded`, one row per tick. Floats print shortest-exact, so
/// a fixture reads back bit-identical to what was recorded.
fn to_csv(rows: &[Row]) -> String {
    let mut s = String::from("tick,x,y,z,grounded\n");
    for (t, r) in rows.iter().enumerate() {
        writeln!(s, "{t},{},{},{},{}", r[0], r[1], r[2], r[3] as u8).unwrap();
    }
    s
}

fn from_csv(text: &str) -> Vec<Row> {
    text.lines()
        .skip(1)
        .map(|line| {
            let f: Vec<f32> = line.split(',').skip(1).map(|v| v.parse().unwrap()).collect();
            [f[0], f[1], f[2], f[3]]
        })
        .collect()
}

/// Records the fixture in record mode; otherwise compares against it.
fn check(name: &str) {
    let actual = run_scenario(name);
    assert_eq!(actual.len(), SCENARIO_TICKS);
    let path = fixture(name);
    if std::env::var_os("PHYSICS_GOLDENS_RECORD").is_some_and(|v| v == "1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, to_csv(&actual)).unwrap();
        eprintln!("recorded {}", path.display());
        return;
    }
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} — record it with PHYSICS_GOLDENS_RECORD=1", path.display()));
    let golden = from_csv(&text);
    assert_eq!(golden.len(), SCENARIO_TICKS, "{name}: fixture has {} rows", golden.len());
    let transition_near = |t: usize| {
        let lo = t.saturating_sub(GROUNDED_SLACK_TICKS).max(1);
        let hi = (t + GROUNDED_SLACK_TICKS).min(SCENARIO_TICKS - 1);
        (lo..=hi).any(|i| golden[i][3] != golden[i - 1][3])
    };
    for (t, (g, a)) in golden.iter().zip(&actual).enumerate() {
        let pos_ok = (0..3).all(|i| (g[i] - a[i]).abs() <= POS_TOL);
        let grounded_ok = g[3] == a[3] || transition_near(t);
        assert!(
            pos_ok && grounded_ok,
            "{name}: first divergence at tick {t} (POS_TOL {POS_TOL}, grounded slack {GROUNDED_SLACK_TICKS})\n  golden {g:?}\n  actual {a:?}",
        );
    }
}

#[test]
fn walk_flat_across_chunk_seam_golden() {
    check("walk_flat_across_chunk_seam");
}

#[test]
fn walk_scale_seam_golden() {
    check("walk_scale_seam");
}

#[test]
fn step_up_one_block_golden() {
    check("step_up_one_block");
}

#[test]
fn terrace_uphill_1_voxel_golden() {
    check("terrace_uphill_1_voxel");
}

#[test]
fn jump_in_place_golden() {
    check("jump_in_place");
}

#[test]
fn sprint_jump_over_gap_golden() {
    check("sprint_jump_over_gap");
}

#[test]
fn sunk_start_depenetrates_golden() {
    check("sunk_start_depenetrates");
}

#[test]
fn slime_hop_sequence_golden() {
    check("slime_hop_sequence");
}
