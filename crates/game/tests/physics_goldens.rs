//! Behaviour goldens for movement. A physics engine change can't keep hashes
//! bit-exact, but it must keep behaviour: each scenario drives a body through
//! the real region tick for [`SCENARIO_TICKS`] ticks and compares its
//! per-tick `(x, y, z, grounded)` against
//! `tests/fixtures/physics_goldens/<scenario>.csv`, within [`POS_TOL`] per
//! axis and with [`GROUNDED_SLACK_TICKS`] of slack on `grounded` around a
//! recorded transition.
//!
//! Recorded 2026-11-10 (stud world plan P10b) on stud ground with the stud
//! body (`BodyScale::STUD`), on the rapier 0.36 / parry 0.31 forks: every
//! scenario stood on voxel terrain before (those recordings dated from the
//! nalgebra-era rapier 0.31 / parry 0.25 forks and described the 1.8 m voxel
//! body), so all eight fixtures were rebuilt and re-recorded — three under a
//! new name for what the stud scene is (`walk_brick_step` was
//! `walk_scale_seam`, `jump_up_one_tile` was `step_up_one_block`,
//! `terrace_uphill_1_plate` was `terrace_uphill_1_voxel`).
//!
//! Scenarios use only game-level inputs (key presses through the region tick,
//! `set_body_pose_safe`, `spawn_slime_safe`), so the same source runs on a
//! new engine; `common` holds every read of engine types.
//!
//! Re-record — only on the engine the goldens are meant to describe, and in a
//! commit that says why:
//!
//! ```sh
//! PHYSICS_GOLDENS_RECORD=1 just test-one game "--test it physics_goldens::"
//! ```
use crate::common;

use std::fmt::Write as _;

use common::{
    approach_seam, body, entity_body, key, r0, seam_row, seam_x, spawn_slime, stud_flat, stud_ground, teleport, ticks,
    world_of, Body, FLOOR_PLATES,
};
use game::movement::{BodyScale, CHARACTER_SKIN, UNITS_PER_METER};
use game::{GameEventKind, Key, World, STUD_CHUNK_STUDS, TILE_PLATES, TILE_STUDS};

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

/// Height of a player's body origin resting on studded ground whose surface
/// plate is `plates`: the stud caps, the capsule, the controller's skin.
fn rest_y(plates: i32) -> f32 {
    let s = BodyScale::STUD;
    common::floor_y(plates) + game::stud_cap_units() + s.capsule_half_height() + s.capsule_radius() + CHARACTER_SKIN
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

/// +x at walk speed on flat stud ground, from 2 units before the chunk
/// border ahead of the spawn (x = 256), across one more. Ground: x 96..608,
/// z 96..160 units.
fn walk_flat_across_chunk_seam() -> Vec<Row> {
    let mut world = world_of(96.0..608.0, 96.0..160.0, |_, _| stud_flat(FLOOR_PLATES));
    let y = body(&world).y;
    teleport(&mut world, seam_x() - 2.0, y, 136.0);
    key(&mut world, Key::KeyD, true);
    run_player(world, |_, _| {})
}

/// +z from flat ground onto ground one brick course (3 plates, 0.6 m) higher
/// at the chunk border ahead of the spawn (z = 256): the stud body's
/// autostep at its tallest. Ground: x 96..160, z 96..576 units.
fn walk_brick_step() -> Vec<Row> {
    let step = game::STUD_STEP_PLATES;
    let mut world =
        world_of(96.0..160.0, 96.0..576.0, |_, z| stud_flat(if z >= seam_row() { FLOOR_PLATES + step } else { FLOOR_PLATES }));
    // Start 32 units short of the border.
    approach_seam(&mut world, 32.0);
    run_player(world, |w, t| {
        if t == 0 {
            key(w, Key::KeyS, true);
        }
    })
}

/// +z into a one-tile (1.2 m) ledge at the chunk border ahead of the spawn
/// (z = 256), jumping just before it: a tile is taller than the step height,
/// so it takes a jump. Ground: x 96..160, z 96..576 units.
fn jump_up_one_tile() -> Vec<Row> {
    let tile = TILE_PLATES as i32;
    let mut world =
        world_of(96.0..160.0, 96.0..576.0, |_, z| stud_flat(if z >= seam_row() { FLOOR_PLATES + tile } else { FLOOR_PLATES }));
    // Start 32 units short of the border.
    approach_seam(&mut world, 32.0);
    run_player(world, |w, t| {
        if t == 0 {
            key(w, Key::KeyS, true);
        }
        tap(w, Key::Space, 4, t);
    })
}

/// +z up one-plate terraces, a plate (0.2 m) up every two tiles: the gentle
/// generated slope on which walking uphill dips in speed at each terrace.
fn terrace_uphill_1_plate() -> Vec<Row> {
    let world = world_of(96.0..160.0, 96.0..576.0, |_, cz| {
        // Stud row `sz` of chunk row `cz`: a plate higher every 2 tiles.
        stud_ground(|_, sz| FLOOR_PLATES + ((cz * STUD_CHUNK_STUDS + sz) / (2 * TILE_STUDS)) as i32)
    });
    run_player(world, |w, t| {
        if t == 0 {
            key(w, Key::KeyS, true);
        }
    })
}

/// Space tapped once, standing on flat ground: one jump, then standing.
fn jump_in_place() -> Vec<Row> {
    let world = world_of(96.0..160.0, 96.0..160.0, |_, _| stud_flat(FLOOR_PLATES));
    run_player(world, |w, t| tap(w, Key::Space, 0, t))
}

/// Sprinting forward (−z) over a one-tile trench (z 560..576 units, one tile
/// deep) with a jump just before its edge, then on to the end.
fn sprint_jump_over_gap() -> Vec<Row> {
    // The trench is tile row 35 along z (stud rows 70..72), spanning the
    // ground's tile columns 6..10 (stud columns 12..20).
    let ground = |cx: usize, cz: usize| {
        stud_ground(|sx, sz| {
            let (x, z) = (cx * STUD_CHUNK_STUDS + sx, cz * STUD_CHUNK_STUDS + sz);
            let trench = (70..72).contains(&z) && (12..20).contains(&x);
            if trench { FLOOR_PLATES - TILE_PLATES as i32 } else { FLOOR_PLATES }
        })
    };
    let mut world = world_of(96.0..160.0, 64.0..640.0, ground);
    teleport(&mut world, 144.0, rest_y(FLOOR_PLATES), 620.0);
    ticks(&mut world, 2);
    key(&mut world, Key::ControlLeft, true);
    key(&mut world, Key::KeyW, true);
    run_player(world, |w, t| tap(w, Key::Space, 8, t))
}

/// Starts 0.3 units sunk into flat ground — into the stud caps, past the skin
/// gap it normally rests on — stands, walks +z, stands.
fn sunk_start_depenetrates() -> Vec<Row> {
    let mut world = world_of(96.0..160.0, 96.0..224.0, |_, _| stud_flat(FLOOR_PLATES));
    teleport(&mut world, 136.0, rest_y(FLOOR_PLATES) - CHARACTER_SKIN - 0.3, 136.0);
    run_player(world, |w, t| {
        if t == 20 {
            key(w, Key::KeyS, true);
        } else if t == 40 {
            key(w, Key::KeyS, false);
        }
    })
}

/// One size-1 slime (spawn tick 0, so a fixed roll sequence) dropped from
/// 1.5 m onto flat ground with no player: it lands, sits and hops.
fn slime_hop_sequence() -> Vec<Row> {
    let mut region = common::floor_region(r0(), 64.0..288.0, 64.0..288.0, FLOOR_PLATES);
    let drop = common::floor_y(FLOOR_PLATES) + 1.5 * UNITS_PER_METER;
    let slime = spawn_slime(&mut region, [176.0, drop, 176.0], 1, 0.0, 0);
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
        "walk_brick_step" => walk_brick_step(),
        "jump_up_one_tile" => jump_up_one_tile(),
        "terrace_uphill_1_plate" => terrace_uphill_1_plate(),
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
fn walk_brick_step_golden() {
    check("walk_brick_step");
}

#[test]
fn jump_up_one_tile_golden() {
    check("jump_up_one_tile");
}

#[test]
fn terrace_uphill_1_plate_golden() {
    check("terrace_uphill_1_plate");
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
