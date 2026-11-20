//! `forge_proto --planet [--no-retry] [--seeds N] [--out <stem>] [--frames n] [--key <name>=<value>]`:
//! the plate simulation and derived planet (`worldgen::forge::planet`) on its
//! own, on the planet grid — the `planet.` lines per seed, the summary, and
//! contact sheets (`<stem>-planet-{plates,crust-age,orogeny-kind,orogeny-age,
//! strength,elevation,land,belts,uplift-now}.png`), the seeds side by side in
//! rows of 4, each 512 × 256. `--frames n` writes `n` evenly spaced `plates`
//! frames of the FIRST seed, `<stem>-planet-t<My>.png`.
//! Plan: docs/superpowers/plans/2026-11-20-plates-and-aged-relief-2.md (Tasks 1–4).

use worldgen::forge::ForgeParams;
use worldgen::forge::belts::{AgeClass, NO_BELT, land_components_over};
use worldgen::forge::planet::{
    CrustKind, OrogenyKind, PLANET_CELLS, PLANET_NX, PLANET_NZ, Planet, PlanetKey,
    build_with_retries, planet_lat_deg, plate_seed,
};
use worldgen::forge::plate_sim::{
    BELT_MIN_REL, NO_PLATE, PLATE_DT_MY, POLAR_FADE_DEG, PlateState, simulate,
};
use worldgen::forge::render::{Rgba, encode_png};

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
    let (name, value) = flag
        .split_once('=')
        .ok_or_else(|| format!("--key {flag}: expected <name>=<value>"))?;
    let value: f64 = value
        .parse()
        .map_err(|_| format!("--key {flag}: {value} is not a number"))?;
    let names: Vec<&str> = KEY_RANGES.iter().map(|r| r.0).collect();
    let &(_, lo, hi) = KEY_RANGES
        .iter()
        .find(|r| r.0 == name)
        .ok_or_else(|| format!("--key: no key {name} (one of {})", names.join(", ")))?;
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
    let mut finals: Vec<Planet> = Vec::new();
    for k in 0..seeds {
        let p = ForgeParams {
            seed: base.seed.wrapping_add(k as u64),
            quiet: true,
            ..base.forge.clone()
        };
        let mut key = PlanetKey::of(&p);
        for (name, value) in &base.planet_keys {
            set_key(&mut key, name, *value);
        }
        let planet = if base.no_retry {
            Planet::build(&key, 0)
        } else {
            build_with_retries(&key)
        };
        let rows = planet_lines(&planet);
        println!("seed: {:#018X}", p.seed);
        for (name, value) in &rows {
            println!("{name}: {value}");
        }
        if k == 0 && base.frames > 0 {
            let sim_key = PlanetKey {
                seed: plate_seed(key.seed, planet.attempt),
                ..key
            };
            let steps = (sim_key.plate_duration_my / PLATE_DT_MY).round() as u32;
            let every = (steps / base.frames as u32).max(1);
            let mut frame_states: Vec<PlateState> = Vec::new();
            let _ = simulate(&sim_key, every, &mut |s| frame_states.push(s.clone()));
            for s in &frame_states {
                write_png(
                    &sheet(std::slice::from_ref(s), plate_state_colour),
                    &format!("{stem}-planet-t{:.0}.png", s.time_my()),
                );
            }
        }
        println!();
        all.push(rows);
        finals.push(planet);
    }
    if base.out_given {
        write_png(
            &sheet(&finals, plate_colour),
            &format!("{stem}-planet-plates.png"),
        );
        write_png(
            &sheet(&finals, age_colour),
            &format!("{stem}-planet-crust-age.png"),
        );
        write_png(
            &sheet(&finals, kind_colour),
            &format!("{stem}-planet-orogeny-kind.png"),
        );
        write_png(
            &sheet(&finals, orogeny_age_colour),
            &format!("{stem}-planet-orogeny-age.png"),
        );
        write_png(
            &sheet(&finals, strength_colour),
            &format!("{stem}-planet-strength.png"),
        );
        write_png(
            &sheet(&finals, elevation_colour),
            &format!("{stem}-planet-elevation.png"),
        );
        write_png(
            &sheet(&finals, land_colour),
            &format!("{stem}-planet-land.png"),
        );
        write_png(
            &sheet(&finals, belts_colour),
            &format!("{stem}-planet-belts.png"),
        );
        write_png(
            &sheet(&finals, uplift_now_colour),
            &format!("{stem}-planet-uplift-now.png"),
        );
    }
    crate::report::print_summary(&all, seeds);
}

/// The `planet.` lines of one finished [`Planet`] (shared by `--planet` and `--report`).
pub fn planet_lines(p: &Planet) -> Vec<(String, String)> {
    let steps = (p.key.plate_duration_my / PLATE_DT_MY).round() as u32;
    let cont0 = p.counters.continental_cells_start as usize;
    let cont1 = p
        .crust
        .iter()
        .filter(|&&k| k == CrustKind::Continental as u8)
        .count();
    let mut holds = [false; 256];
    for &plate_id in &p.plate {
        holds[plate_id as usize] = true;
    }
    let plates = (0..NO_PLATE as usize).filter(|&id| holds[id]).count();
    let ages: Vec<f64> = (0..PLANET_CELLS)
        .filter(|&c| p.crust[c] == CrustKind::Oceanic as u8)
        .map(|c| f64::from(p.crust_age_my[c]))
        .collect();
    let share = |n: usize| n as f64 / PLANET_CELLS as f64;
    let mut rows: Vec<(String, String)> = vec![
        ("planet.steps".into(), steps.to_string()),
        ("planet.plates_final".into(), plates.to_string()),
        (
            "planet.continental_share_start".into(),
            format!("{:.4}", share(cont0)),
        ),
        (
            "planet.continental_share_end".into(),
            format!("{:.4}", share(cont1)),
        ),
        (
            "planet.continental_area_change".into(),
            format!("{:.4}", cont1 as f64 / cont0.max(1) as f64 - 1.0),
        ),
        (
            "planet.ocean_age_p99_my".into(),
            format!("{:.1}", crate::report::p99(ages)),
        ),
        (
            "planet.polar_lost_continental_share".into(),
            format!(
                "{:.4}",
                p.counters.polar_lost_continental as f64 / cont0.max(1) as f64
            ),
        ),
        ("planet.stage_ms".into(), format!("{:.0}", p.stage_ms)),
    ];
    // Task 2: orogeny.
    let active: Vec<usize> = (0..PLANET_CELLS)
        .filter(|&c| p.kind[c] != OrogenyKind::None as u8)
        .collect();
    let strength: Vec<f64> = active.iter().map(|&c| f64::from(p.strength[c])).collect();
    let orogeny_age: Vec<f64> = active
        .iter()
        .map(|&c| f64::from(p.orogeny_age_my[c]))
        .collect();
    rows.push(("planet.sutures".into(), p.counters.sutures.to_string()));
    rows.push((
        "planet.collided_share".into(),
        format!("{:.4}", p.counters.collided as f64 / cont0.max(1) as f64),
    ));
    for (name, kind) in KINDS {
        let n = (0..PLANET_CELLS)
            .filter(|&c| p.crust[c] == CrustKind::Continental as u8 && p.kind[c] == kind as u8)
            .count();
        rows.push((
            format!("planet.kind_share.{name}"),
            format!("{:.4}", n as f64 / cont1.max(1) as f64),
        ));
    }
    rows.push((
        "planet.strength_p50".into(),
        format!("{:.3}", percentile(&strength, 0.5)),
    ));
    rows.push((
        "planet.strength_p90".into(),
        format!("{:.3}", percentile(&strength, 0.9)),
    ));
    for (q, name) in [(0.1, "p10"), (0.5, "p50"), (0.9, "p90")] {
        rows.push((
            format!("planet.orogeny_age_{name}_my"),
            format!("{:.1}", percentile(&orogeny_age, q)),
        ));
    }
    // Task 3: rifts, hotspots and the polar fade.
    let hotspot_recent =
        |c: usize| p.kind[c] == OrogenyKind::Hotspot as u8 && f64::from(p.orogeny_age_my[c]) < 30.0;
    let hotspot_cells = (0..PLANET_CELLS).filter(|&c| hotspot_recent(c)).count();
    let hotspot_island_cells = (0..PLANET_CELLS)
        .filter(|&c| {
            hotspot_recent(c) && p.crust[c] == CrustKind::Oceanic as u8 && p.elevation_m[c] > 0.0
        })
        .count();
    let is_belt_kind = |k: u8| {
        k == OrogenyKind::Arc as u8
            || k == OrogenyKind::Andean as u8
            || k == OrogenyKind::Collision as u8
            || k == OrogenyKind::Plateau as u8
    };
    let (mut belt_total, mut belt_polar) = (0usize, 0usize);
    for c in 0..PLANET_CELLS {
        if is_belt_kind(p.kind[c]) && f64::from(p.strength[c]) >= BELT_MIN_REL {
            belt_total += 1;
            if 90.0 - planet_lat_deg(c / PLANET_NX).abs() < POLAR_FADE_DEG {
                belt_polar += 1;
            }
        }
    }
    rows.push(("planet.rifts".into(), p.counters.rifts.to_string()));
    rows.push(("planet.hotspot_cells".into(), hotspot_cells.to_string()));
    rows.push((
        "planet.hotspot_island_cells".into(),
        hotspot_island_cells.to_string(),
    ));
    rows.push((
        "planet.belt_cells_in_polar_fade_share".into(),
        format!(
            "{:.4}",
            if belt_total == 0 {
                0.0
            } else {
                belt_polar as f64 / belt_total as f64
            }
        ),
    ));
    // Task 4: sea level, continents, belts by age class, score and retries.
    let belts_per_continent = if p.continents.is_empty() {
        "none".to_string()
    } else {
        p.continents
            .iter()
            .map(|c| c.belts.len().to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    let belts_per_continent_by_class = if p.continents.is_empty() {
        "none".to_string()
    } else {
        p.continents
            .iter()
            .map(|c| {
                let (mut y, mut m, mut o) = (0u32, 0u32, 0u32);
                for &b in &c.belts {
                    match p.belts[usize::from(b)].class {
                        AgeClass::Young => y += 1,
                        AgeClass::Middle => m += 1,
                        AgeClass::Old => o += 1,
                    }
                }
                format!("{y}y{m}m{o}o")
            })
            .collect::<Vec<_>>()
            .join(",")
    };
    let belt_area_ratio = if p.belts.is_empty() {
        0.0
    } else {
        f64::from(p.belts[0].cells)
            / f64::from(p.belts.last().expect("non-empty belts").cells.max(1))
    };
    rows.push(("planet.attempt".into(), p.attempt.to_string()));
    rows.push((
        "planet.ok".into(),
        (if p.ok { "yes" } else { "no" }).to_string(),
    ));
    rows.push((
        "planet.land_share".into(),
        format!("{:.4}", p.score.land_share),
    ));
    rows.push(("planet.sea_level_m".into(), format!("{:.0}", p.sea_level_m)));
    rows.push(("planet.continents".into(), p.score.continents.to_string()));
    rows.push((
        "planet.land_components_over_1pct".into(),
        land_components_over(&p.land, 0.01).to_string(),
    ));
    rows.push(("planet.belts.young".into(), p.score.young_belts.to_string()));
    rows.push((
        "planet.belts.middle".into(),
        p.score.middle_belts.to_string(),
    ));
    rows.push(("planet.belts.old".into(), p.score.old_belts.to_string()));
    rows.push(("planet.belts".into(), p.belts.len().to_string()));
    rows.push(("planet.belts_per_continent".into(), belts_per_continent));
    rows.push((
        "planet.belts_per_continent_by_class".into(),
        belts_per_continent_by_class,
    ));
    rows.push((
        "planet.continents_with_2_belts".into(),
        p.continents
            .iter()
            .filter(|c| c.belts.len() >= 2)
            .count()
            .to_string(),
    ));
    rows.push(("planet.attempts_ms".into(), format!("{:.0}", p.attempts_ms)));
    rows.push((
        "planet.belt_area_ratio".into(),
        format!("{belt_area_ratio:.2}"),
    ));
    rows.push((
        "planet.belt_depth_deg".into(),
        format!("{:.2}", p.belt_depth_deg),
    ));
    rows.push(("planet.crc".into(), planet_crc(p)));
    rows
}

/// The six orogeny kinds a cell can carry, with their line names.
const KINDS: [(&str, OrogenyKind); 6] = [
    ("arc", OrogenyKind::Arc),
    ("andean", OrogenyKind::Andean),
    ("collision", OrogenyKind::Collision),
    ("plateau", OrogenyKind::Plateau),
    ("rift_shoulder", OrogenyKind::RiftShoulder),
    ("hotspot", OrogenyKind::Hotspot),
];

/// The `q` quantile (nearest rank, as `report::p99`) of `v`; 0 when empty.
fn percentile(v: &[f64], q: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * q).round() as usize]
}

/// crc32 of `elevation_m`, `kind`, and `strength` (in little-endian byte order).
fn planet_crc(p: &Planet) -> String {
    let mut h = crc32fast::Hasher::new();
    for v in &p.elevation_m {
        h.update(&v.to_le_bytes());
    }
    h.update(&p.kind);
    for v in &p.strength {
        h.update(&v.to_le_bytes());
    }
    format!("{:08x}", h.finalize())
}

fn plate_rgb(plate: u8, crust: u8) -> [u8; 3] {
    if plate == NO_PLATE {
        return [0, 0, 0];
    }
    let hue = (f64::from(plate) * 0.618_034).fract();
    let value = if crust == CrustKind::Continental as u8 {
        1.0
    } else {
        0.45
    };
    hsv(hue, 0.65, value)
}

/// Plate colour of a snapshot frame (`--frames`).
fn plate_state_colour(s: &PlateState, c: usize) -> [u8; 3] {
    plate_rgb(s.plate[c], s.crust[c])
}

/// One colour per plate id (hues a golden angle apart), continental cells brighter.
fn plate_colour(p: &Planet, c: usize) -> [u8; 3] {
    plate_rgb(p.plate[c], p.crust[c])
}

/// Ocean floor by age on a ramp (young red → yellow → old deep blue over
/// 0 … `AGE_RAMP_MY`), continental crust grey.
fn age_colour(p: &Planet, c: usize) -> [u8; 3] {
    if p.crust[c] == CrustKind::Continental as u8 {
        return [150, 150, 150];
    }
    age_ramp(f64::from(p.crust_age_my[c]))
}

/// Young red → yellow → old deep blue over 0 … `AGE_RAMP_MY`.
fn age_ramp(age_my: f64) -> [u8; 3] {
    let t = (age_my / AGE_RAMP_MY).clamp(0.0, 1.0);
    const STOPS: [[f64; 3]; 3] = [
        [230.0, 50.0, 30.0],
        [240.0, 220.0, 80.0],
        [20.0, 30.0, 110.0],
    ];
    let (a, b, u) = if t < 0.5 {
        (STOPS[0], STOPS[1], 2.0 * t)
    } else {
        (STOPS[1], STOPS[2], 2.0 * t - 1.0)
    };
    [0, 1, 2].map(|j| (a[j] + (b[j] - a[j]) * u).round() as u8)
}

/// The ground of the orogeny layers: continental grey, oceanic dark.
fn crust_ground(p: &Planet, c: usize) -> [u8; 3] {
    if p.crust[c] == CrustKind::Continental as u8 {
        [120, 120, 120]
    } else {
        [25, 30, 45]
    }
}

/// A colour per orogeny kind (Arc cyan, Andean orange, Collision red,
/// Plateau magenta, RiftShoulder green, Hotspot yellow) on `crust_ground`.
fn kind_colour(p: &Planet, c: usize) -> [u8; 3] {
    const COLOURS: [[u8; 3]; 7] = [
        [0, 0, 0],
        [60, 200, 230],
        [245, 150, 30],
        [220, 40, 40],
        [200, 60, 220],
        [80, 200, 80],
        [250, 230, 60],
    ];
    match p.kind[c] {
        0 => crust_ground(p, c),
        k => COLOURS[k as usize],
    }
}

/// The age ramp of the last orogeny where a kind is set, else `crust_ground`.
fn orogeny_age_colour(p: &Planet, c: usize) -> [u8; 3] {
    if p.kind[c] == OrogenyKind::None as u8 {
        return crust_ground(p, c);
    }
    age_ramp(f64::from(p.orogeny_age_my[c]))
}

fn lerp_stops(stops: &[(f64, [f64; 3])], x: f64) -> [u8; 3] {
    if x <= stops[0].0 {
        return stops[0].1.map(|v| v.round() as u8);
    }
    for w in stops.windows(2) {
        if x <= w[1].0 {
            let u = (x - w[0].0) / (w[1].0 - w[0].0);
            return [0, 1, 2].map(|j| (w[0].1[j] + (w[1].1[j] - w[0].1[j]) * u).round() as u8);
        }
    }
    stops
        .last()
        .expect("non-empty stops")
        .1
        .map(|v| v.round() as u8)
}

fn heat_ramp(t: f64) -> [u8; 3] {
    const STOPS: [(f64, [f64; 3]); 4] = [
        (0.0, [0.0, 0.0, 0.0]),
        (1.0 / 3.0, [200.0, 30.0, 20.0]),
        (2.0 / 3.0, [250.0, 210.0, 40.0]),
        (1.0, [255.0, 255.0, 255.0]),
    ];
    lerp_stops(&STOPS, t.clamp(0.0, 1.0))
}

/// `strength` from black (0) through red and yellow to white (1); a cell
/// with no orogeny shows `crust_ground`.
fn strength_colour(p: &Planet, c: usize) -> [u8; 3] {
    if p.kind[c] == OrogenyKind::None as u8 {
        return crust_ground(p, c);
    }
    heat_ramp(f64::from(p.strength[c]))
}

/// Hypsometric elevation relative to the solved sea level: blue ramp below 0,
/// green → tan → brown → white above 0.
fn elevation_colour(p: &Planet, c: usize) -> [u8; 3] {
    let e = f64::from(p.elevation_m[c]);
    if e < 0.0 {
        const SEA: [(f64, [f64; 3]); 3] = [
            (-6000.0, [10.0, 25.0, 70.0]),
            (-2000.0, [25.0, 70.0, 145.0]),
            (0.0, [95.0, 180.0, 225.0]),
        ];
        lerp_stops(&SEA, e)
    } else {
        const LAND: [(f64, [f64; 3]); 5] = [
            (0.0, [60.0, 140.0, 65.0]),
            (500.0, [120.0, 165.0, 75.0]),
            (1500.0, [190.0, 165.0, 105.0]),
            (3500.0, [140.0, 95.0, 65.0]),
            (5500.0, [245.0, 245.0, 250.0]),
        ];
        lerp_stops(&LAND, e)
    }
}

/// Smoothed coast mask: land tan vs sea deep blue.
fn land_colour(p: &Planet, c: usize) -> [u8; 3] {
    if p.land[c] {
        [190, 185, 165]
    } else {
        [25, 40, 75]
    }
}

/// Mountain belts by age class (young red, middle orange, old blue) over land grey / sea dark.
fn belts_colour(p: &Planet, c: usize) -> [u8; 3] {
    let b = p.belt[c];
    if b != NO_BELT {
        return match p.belts[usize::from(b)].class {
            AgeClass::Young => [225, 45, 40],
            AgeClass::Middle => [245, 155, 35],
            AgeClass::Old => [55, 125, 225],
        };
    }
    if p.land[c] {
        [145, 145, 145]
    } else {
        [25, 30, 45]
    }
}

/// Blurred active uplift (`uplift_now`) on the heat ramp over subtle land/sea ground.
fn uplift_now_colour(p: &Planet, c: usize) -> [u8; 3] {
    let u = f64::from(p.uplift_now[c]).clamp(0.0, 1.0);
    if u <= 0.005 {
        return if p.land[c] {
            [55, 55, 55]
        } else {
            [20, 25, 38]
        };
    }
    heat_ramp(u)
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

/// The items side by side, `SHEET_COLUMNS` to a row, one pixel per planet cell.
fn sheet<T>(items: &[T], colour: fn(&T, usize) -> [u8; 3]) -> Rgba {
    let columns = items.len().clamp(1, SHEET_COLUMNS);
    let rows = items.len().div_ceil(SHEET_COLUMNS).max(1);
    let (width, height) = (columns * PLANET_NX, rows * PLANET_NZ);
    let mut pixels = vec![0u8; width * height * 4];
    for (k, item) in items.iter().enumerate() {
        let (x0, z0) = (
            (k % SHEET_COLUMNS) * PLANET_NX,
            (k / SHEET_COLUMNS) * PLANET_NZ,
        );
        for c in 0..PLANET_CELLS {
            let (x, z) = (x0 + c % PLANET_NX, z0 + c / PLANET_NX);
            let i = 4 * (z * width + x);
            pixels[i..i + 3].copy_from_slice(&colour(item, c));
            pixels[i + 3] = 255;
        }
    }
    Rgba {
        width: width as u32,
        height: height as u32,
        pixels,
    }
}

fn write_png(img: &Rgba, path: &str) {
    std::fs::write(path, encode_png(img)).expect("write png");
    println!("wrote {path}");
}
