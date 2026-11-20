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
use crate::common;

use std::fmt::Write as _;

use common::{
    Body, approach_seam, body, entity_body, floor_chunks, key, r0, seam_row, seam_x, spawn_slime,
    teleport, ticks, world_of,
};
use game::movement::{
    CHARACTER_SKIN, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS, UNITS_PER_METER,
};
use game::{BLOCK_VOXELS, Block, BlockId, CHUNK_BLOCKS, Chunk, GameEventKind, Key, Region, World};

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

/// +x at walk speed on a block-scale floor, from 2 units before the chunk
/// border ahead of the spawn (x = 160 at 2-block chunks), across a dozen
/// more. Floor: x 96..608, z 96..160 units.
fn walk_flat_across_chunk_seam() -> Vec<Row> {
    let mut world = world_of(96.0..608.0, 96.0..160.0, |_, _| Chunk::flat_floor(16));
    let y = body(&world).y;
    teleport(&mut world, seam_x() - 2.0, y, 136.0);
    key(&mut world, Key::KeyD, true);
    run_player(world, |_, _| {})
}

/// +z from a block-scale floor (top 16) onto half blocks (one-cuboid
/// chisels, top 24) at the chunk border ahead of the spawn (z = 160 at 2-block
/// chunks): a scale seam and a half-block autostep. Floor: x 96..160,
/// z 96..576 units.
fn walk_scale_seam() -> Vec<Row> {
    let mut world = world_of(96.0..160.0, 96.0..576.0, |_, z| {
        Chunk::flat_floor(if z >= seam_row() { 24 } else { 16 })
    });
    // Start 32 units short of the border (the spawn itself at 2-block chunks).
    approach_seam(&mut world, 32.0);
    run_player(world, |w, t| {
        if t == 0 {
            key(w, Key::KeyS, true);
        }
    })
}

/// +z into a one-block (16-unit) ledge at the chunk border ahead of the spawn
/// (z = 160 at 2-block chunks), jumping just before it: a block is taller
/// than the step height, so it takes a jump, as in Minecraft. Floor: x 96..160,
/// z 96..576 units.
fn step_up_one_block() -> Vec<Row> {
    let mut world = world_of(96.0..160.0, 96.0..576.0, |_, z| {
        Chunk::flat_floor(if z >= seam_row() { 32 } else { 16 })
    });
    // Start 32 units short of the border (the spawn itself at 2-block chunks).
    approach_seam(&mut world, 32.0);
    run_player(world, |w, t| {
        if t == 0 {
            key(w, Key::KeyS, true);
        }
        tap(w, Key::Space, 4, t);
    })
}

/// +z up one-voxel terraces, a step up every two blocks (one 2-block chunk
/// row at the original chunk size): the gentle generated slope on which
/// walking uphill dips in speed at each terrace.
fn terrace_uphill_1_voxel() -> Vec<Row> {
    let world = world_of(96.0..160.0, 96.0..576.0, |_, z| {
        // Each block row z copies its row of a flat floor 12 + z/2 voxels deep.
        let mut chunk = Chunk::default();
        for bz in 0..CHUNK_BLOCKS {
            let row = Chunk::flat_floor(12 + ((z * CHUNK_BLOCKS + bz) / 2) as u32);
            for by in 0..CHUNK_BLOCKS {
                for bx in 0..CHUNK_BLOCKS {
                    chunk.set_block(bx, by, bz, row.block_at(bx, by, bz));
                    let i = game::BlockIndex::from_xyz(bx, by, bz);
                    if let Some(c) = row.chisel.get(&i) {
                        chunk.chisel.insert(i, c.clone());
                    }
                }
            }
        }
        chunk
    });
    run_player(world, |w, t| {
        if t == 0 {
            key(w, Key::KeyS, true);
        }
    })
}

/// Space tapped once, standing on a flat floor: one jump, then standing.
fn jump_in_place() -> Vec<Row> {
    let world = world_of(96.0..160.0, 96.0..160.0, |_, _| Chunk::flat_floor(16));
    run_player(world, |w, t| tap(w, Key::Space, 0, t))
}

/// Sprinting forward (−z) over a one-block trench (z 560..576, one block
/// deep) with a jump just before its edge, then on until W is let go at tick
/// [`SPRINT_RELEASE_TICK`] (held to the end, a faster sprint ran off the floor
/// and out of the region).
fn sprint_jump_over_gap() -> Vec<Row> {
    // The trench is block row 35 along z (units 560..576), spanning the floor's
    // block columns 6..10 (units 96..160).
    let trench_z_block = 560 / BLOCK_VOXELS;
    let floor = |x: usize, z: usize| {
        let mut chunk = Chunk::flat_floor(2 * BLOCK_VOXELS as u32);
        for bx in 0..CHUNK_BLOCKS {
            for bz in 0..CHUNK_BLOCKS {
                let block_x = x * CHUNK_BLOCKS + bx;
                if z * CHUNK_BLOCKS + bz == trench_z_block && (6..10).contains(&block_x) {
                    chunk.set_block(bx, 1, bz, Block::new(BlockId::AIR));
                }
            }
        }
        chunk
    };
    let mut world = world_of(96.0..160.0, 64.0..640.0, floor);
    teleport(&mut world, 144.0, rest_y(2.0 * BLOCK_VOXELS as f32), 620.0);
    ticks(&mut world, 2);
    key(&mut world, Key::ControlLeft, true);
    key(&mut world, Key::KeyW, true);
    run_player(world, |w, t| {
        tap(w, Key::Space, 8, t);
        if t == SPRINT_RELEASE_TICK {
            key(w, Key::KeyW, false);
        }
    })
}

/// When `sprint_jump_over_gap` lets go of W: past the trench, short of the
/// floor's end (z 64).
const SPRINT_RELEASE_TICK: usize = 70;

/// Starts 0.3 units sunk into a flat floor — into the solid, past the skin
/// gap it normally rests on — stands, walks +z, stands.
fn sunk_start_depenetrates() -> Vec<Row> {
    let mut world = world_of(96.0..160.0, 96.0..224.0, |_, _| Chunk::flat_floor(16));
    teleport(
        &mut world,
        136.0,
        rest_y(16.0) - CHARACTER_SKIN - 0.3,
        136.0,
    );
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
    let mut region = Region::from_chunks(r0(), floor_chunks(64.0..288.0, 64.0..288.0, 16));
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
            let f: Vec<f32> = line
                .split(',')
                .skip(1)
                .map(|v| v.parse().unwrap())
                .collect();
            [f[0], f[1], f[2], f[3]]
        })
        .collect()
}

/// The sim-chunk size the chunk-size-dependent fixtures were recorded at.
const RECORDED_CHUNK_BLOCKS: usize = 16;

/// Scenarios whose recording depends on the chunk size, so they are compared
/// only at [`RECORDED_CHUNK_BLOCKS`]: the seam walks start relative to the
/// first chunk border past the spawn (x/z 160 at 2-block chunks, 256 at 16 —
/// the same trajectory translated), and the slime's hop rolls are seeded by
/// its entity key, which follows the number of chunk entities created first.
const CHUNK_SIZE_DEPENDENT: &[&str] = &[
    "walk_flat_across_chunk_seam",
    "walk_scale_seam",
    "step_up_one_block",
    "slime_hop_sequence",
];

/// Records the fixture in record mode; otherwise compares against it.
fn check(name: &str) {
    if CHUNK_BLOCKS != RECORDED_CHUNK_BLOCKS && CHUNK_SIZE_DEPENDENT.contains(&name) {
        eprintln!(
            "{name}: recorded at {RECORDED_CHUNK_BLOCKS}-block chunks, skipped at {CHUNK_BLOCKS}"
        );
        return;
    }
    let actual = run_scenario(name);
    assert_eq!(actual.len(), SCENARIO_TICKS);
    let path = fixture(name);
    if std::env::var_os("PHYSICS_GOLDENS_RECORD").is_some_and(|v| v == "1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, to_csv(&actual)).unwrap();
        eprintln!("recorded {}", path.display());
        return;
    }
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} — record it with PHYSICS_GOLDENS_RECORD=1",
            path.display()
        )
    });
    let golden = from_csv(&text);
    assert_eq!(
        golden.len(),
        SCENARIO_TICKS,
        "{name}: fixture has {} rows",
        golden.len()
    );
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

/// Gate: pins physics behaviour; re-record only on purpose
#[test]
fn walk_flat_across_chunk_seam_golden() {
    check("walk_flat_across_chunk_seam");
}

/// Gate: pins physics behaviour; re-record only on purpose
#[test]
fn walk_scale_seam_golden() {
    check("walk_scale_seam");
}

/// Gate: pins physics behaviour; re-record only on purpose
#[test]
fn step_up_one_block_golden() {
    check("step_up_one_block");
}

/// Gate: pins physics behaviour; re-record only on purpose
#[test]
fn terrace_uphill_1_voxel_golden() {
    check("terrace_uphill_1_voxel");
}

/// Gate: pins physics behaviour; re-record only on purpose
#[test]
fn jump_in_place_golden() {
    check("jump_in_place");
}

/// Gate: pins physics behaviour; re-record only on purpose
#[test]
fn sprint_jump_over_gap_golden() {
    check("sprint_jump_over_gap");
}

/// Gate: pins physics behaviour; re-record only on purpose
#[test]
fn sunk_start_depenetrates_golden() {
    check("sunk_start_depenetrates");
}

/// Gate: pins physics behaviour; re-record only on purpose
#[test]
fn slime_hop_sequence_golden() {
    check("slime_hop_sequence");
}
