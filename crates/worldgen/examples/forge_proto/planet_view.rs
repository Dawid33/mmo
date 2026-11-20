//! `forge_proto --planet [--seeds N] [--out <stem>] [--frames n] [--key <name>=<value>]`:
//! the plate simulation (`worldgen::forge::plate_sim`) on its own, on the
//! planet grid — the `planet.` lines per seed, the summary, and contact
//! sheets: `<stem>-planet-plates.png` (one colour per plate id, continental
//! cells brighter) and `<stem>-planet-crust-age.png` (ocean floor on a ramp
//! over 0 … 500 My, continental grey), the seeds side by side in rows of 4,
//! each 512 × 256. `--frames n` writes `n` evenly spaced `plates` frames of
//! the FIRST seed, `<stem>-planet-t<My>.png`.
//! Plan: docs/superpowers/plans/2026-11-20-plates-and-aged-relief-2.md (Task 1).

use std::time::Instant;

use worldgen::forge::planet::{CrustKind, PlanetKey, PLANET_CELLS, PLANET_NX, PLANET_NZ};
use worldgen::forge::plate_sim::{simulate, PlateState, NO_PLATE, PLATE_DT_MY};
use worldgen::forge::render::{encode_png, Rgba};
use worldgen::forge::ForgeParams;

use crate::Params;

/// The `--key` names and their ranges (the vocabulary's "Knobs after this
/// phase"); `--key` goes with them in Task 7.
const KEY_RANGES: [(&str, f64, f64); 5] = [
    ("land_fraction", 0.15, 0.5),
    ("plate_duration_my", 250.0, 700.0),
    ("rift_rate", 0.25, 4.0),
    ("microcontinents", 0.0, 10.0),
    ("belt_decay_my", 40.0, 400.0),
];

/// Contact sheet columns.
const SHEET_COLUMNS: usize = 4;
/// The crust-age ramp's oldest age, My.
const AGE_RAMP_MY: f64 = 500.0;

/// `--key <name>=<value>`: one `PlanetKey` field by name, refused outside its range.
pub fn parse_key(flag: &str) -> Result<(String, f64), String> {
    let (name, value) = flag.split_once('=').ok_or_else(|| format!("--key {flag}: expected <name>=<value>"))?;
    let value: f64 = value.parse().map_err(|_| format!("--key {flag}: {value} is not a number"))?;
    let names: Vec<&str> = KEY_RANGES.iter().map(|r| r.0).collect();
    let &(_, lo, hi) =
        KEY_RANGES.iter().find(|r| r.0 == name).ok_or_else(|| format!("--key: no key {name} (one of {})", names.join(", ")))?;
    if !(lo..=hi).contains(&value) {
        return Err(format!("--key {name}={value}: outside [{lo}, {hi}]"));
    }
    Ok((name.to_string(), value))
}

fn set_key(key: &mut PlanetKey, name: &str, value: f64) {
    match name {
        "land_fraction" => key.land_fraction = value,
        "plate_duration_my" => key.plate_duration_my = value,
        "rift_rate" => key.rift_rate = value,
        "microcontinents" => key.microcontinents = value,
        "belt_decay_my" => key.belt_decay_my = value,
        other => unreachable!("parse_key let {other} through"),
    }
}

/// The `--planet` run: each seed in turn (`seed`, `seed + 1`, …), then the summary.
pub fn run(base: &Params) {
    let seeds = base.seeds.max(1);
    let stem = base.out.to_string_lossy().to_string();
    let mut all: Vec<Vec<(String, String)>> = Vec::new();
    let mut finals: Vec<PlateState> = Vec::new();
    for k in 0..seeds {
        let p = ForgeParams { seed: base.seed.wrapping_add(k as u64), quiet: true, ..base.forge.clone() };
        let mut key = PlanetKey::of(&p);
        for (name, value) in &base.planet_keys {
            set_key(&mut key, name, *value);
        }
        let steps = (key.plate_duration_my / PLATE_DT_MY).round() as u32;
        let frames = if k == 0 { base.frames } else { 0 };
        let every = if frames > 0 { (steps / frames as u32).max(1) } else { 0 };
        let mut frame_states: Vec<PlateState> = Vec::new();
        let start = PlateState::initial(&key);
        let started = Instant::now();
        let state = simulate(&key, every, &mut |s| frame_states.push(s.clone()));
        let stage_ms = started.elapsed().as_secs_f64() * 1000.0;
        let rows = lines(&start, &state, stage_ms);
        println!("seed: {:#018X}", p.seed);
        for (key, value) in &rows {
            println!("{key}: {value}");
        }
        for s in &frame_states {
            write_png(&sheet(std::slice::from_ref(s), plate_colour), &format!("{stem}-planet-t{:.0}.png", s.time_my()));
        }
        println!();
        all.push(rows);
        finals.push(state);
    }
    if base.out_given {
        write_png(&sheet(&finals, plate_colour), &format!("{stem}-planet-plates.png"));
        write_png(&sheet(&finals, age_colour), &format!("{stem}-planet-crust-age.png"));
    }
    crate::report::print_summary(&all, seeds);
}

/// The `planet.` lines of one run. `stage_ms` is the wall time of `simulate`
/// (`initial` included; with `--frames`, the frames' copies too).
fn lines(start: &PlateState, end: &PlateState, stage_ms: f64) -> Vec<(String, String)> {
    let continental = |s: &PlateState| s.crust.iter().filter(|&&k| k == CrustKind::Continental as u8).count();
    let (cont0, cont1) = (continental(start), continental(end));
    let mut holds = [false; 256];
    for &p in &end.plate {
        holds[p as usize] = true;
    }
    let plates = (0..NO_PLATE as usize).filter(|&p| holds[p]).count();
    let t_end = end.time_my();
    let ages: Vec<f64> = (0..PLANET_CELLS)
        .filter(|&c| end.crust[c] == CrustKind::Oceanic as u8)
        .map(|c| t_end - f64::from(end.born_my[c]))
        .collect();
    let share = |n: usize| n as f64 / PLANET_CELLS as f64;
    vec![
        ("planet.steps".into(), end.step.to_string()),
        ("planet.plates_final".into(), plates.to_string()),
        ("planet.continental_share_start".into(), format!("{:.4}", share(cont0))),
        ("planet.continental_share_end".into(), format!("{:.4}", share(cont1))),
        ("planet.continental_area_change".into(), format!("{:.4}", cont1 as f64 / cont0.max(1) as f64 - 1.0)),
        ("planet.ocean_age_p99_my".into(), format!("{:.1}", crate::report::p99(ages))),
        (
            "planet.polar_lost_continental_share".into(),
            format!("{:.4}", end.counters.polar_lost_continental as f64 / cont0.max(1) as f64),
        ),
        ("planet.stage_ms".into(), format!("{stage_ms:.0}")),
    ]
}

/// One colour per plate id (hues a golden angle apart), continental cells brighter.
fn plate_colour(s: &PlateState, c: usize) -> [u8; 3] {
    let p = s.plate[c];
    if p == NO_PLATE {
        return [0, 0, 0];
    }
    let hue = (f64::from(p) * 0.618_034).fract();
    let value = if s.crust[c] == CrustKind::Continental as u8 { 1.0 } else { 0.45 };
    hsv(hue, 0.65, value)
}

/// Ocean floor by age on a ramp (young red → yellow → old deep blue over
/// 0 … `AGE_RAMP_MY`), continental crust grey.
fn age_colour(s: &PlateState, c: usize) -> [u8; 3] {
    if s.crust[c] == CrustKind::Continental as u8 {
        return [150, 150, 150];
    }
    let t = ((s.time_my() - f64::from(s.born_my[c])) / AGE_RAMP_MY).clamp(0.0, 1.0);
    const STOPS: [[f64; 3]; 3] = [[230.0, 50.0, 30.0], [240.0, 220.0, 80.0], [20.0, 30.0, 110.0]];
    let (a, b, u) = if t < 0.5 { (STOPS[0], STOPS[1], 2.0 * t) } else { (STOPS[1], STOPS[2], 2.0 * t - 1.0) };
    [0, 1, 2].map(|j| (a[j] + (b[j] - a[j]) * u).round() as u8)
}

fn hsv(h: f64, s: f64, v: f64) -> [u8; 3] {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match i as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [r, g, b].map(|x| (x * 255.0).round() as u8)
}

/// The states side by side, `SHEET_COLUMNS` to a row, one pixel per planet cell.
fn sheet(states: &[PlateState], colour: fn(&PlateState, usize) -> [u8; 3]) -> Rgba {
    let columns = states.len().clamp(1, SHEET_COLUMNS);
    let rows = states.len().div_ceil(SHEET_COLUMNS).max(1);
    let (width, height) = (columns * PLANET_NX, rows * PLANET_NZ);
    let mut pixels = vec![0u8; width * height * 4];
    for (k, s) in states.iter().enumerate() {
        let (x0, z0) = ((k % SHEET_COLUMNS) * PLANET_NX, (k / SHEET_COLUMNS) * PLANET_NZ);
        for c in 0..PLANET_CELLS {
            let (x, z) = (x0 + c % PLANET_NX, z0 + c / PLANET_NX);
            let i = 4 * (z * width + x);
            pixels[i..i + 3].copy_from_slice(&colour(s, c));
            pixels[i + 3] = 255;
        }
    }
    Rgba { width: width as u32, height: height as u32, pixels }
}

fn write_png(img: &Rgba, path: &str) {
    std::fs::write(path, encode_png(img)).expect("write png");
    println!("wrote {path}");
}
