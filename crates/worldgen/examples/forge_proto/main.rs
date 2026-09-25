//! World forge prototype (F1): a whole finite world — one "super region",
//! 16 km, ocean all round — generated OFFLINE by simulating landscape
//! evolution instead of evaluating noise per point. Touches no engine code and
//! no world output: it writes PNGs, a river/lake object file and statistics,
//! to find out what the real forge should be.
//! Background: docs/superpowers/specs/2026-09-20-df-worldgen-research/00-synthesis.md
//! (addendum) and docs/superpowers/specs/2026-09-20-world-forge-f0/findings.md.
//!
//! Run: just forge-proto                          (16 m cells, ~30 s)
//!      just forge-proto --cell 32                (quick look, ~5 s)
//!      just forge-proto --search 64              (rank 64 seeds at 32 m)
//!      just forge-proto --no-geology --no-climate   (the F0 behaviour)
//!      just forge-ui                             (tuning page: every knob as a
//!                                                 slider, live map preview)
//!
//! Pipeline:
//!   1. world.rs    noise island inside a guaranteed ocean margin, low initial
//!                  relief, an UPLIFT map with mountain belts inland;
//!   2. geology.rs  rock column in a stratigraphic coordinate (tilt + folds,
//!                  domed by the uplift) -> erodibility of whatever rock the
//!                  surface currently cuts: geology BEFORE erosion;
//!   3. climate.rs  orographic rain on the current heights -> discharge weights;
//!   4. erosion.rs  per iteration: Priority-Flood routing, discharge, implicit
//!                  stream-power incision, depression silting, hillslope diffusion;
//!   5. post.rs     valley floors planed beside rivers; overdeepened basins
//!                  flooded to their true spill level = lakes;
//!   6. hydrology.rs  lakes and rivers as objects (Strahler order, width,
//!                  monotone water surface, smoothed polylines) + statistics;
//!   7. search.rs   generate-and-test over seeds.
//!   knobs.rs holds every tunable (CLI `--set key=value`); serve.rs + ui.html
//!   are the local tuning page.
//!
//! Not bit-reproducible across machines by design (sqrt/ln/sin used freely):
//! the forge's output is a stored artifact, not a function both sides evaluate.

#[allow(dead_code)]
#[path = "../../src/noise.rs"]
mod noise;

mod climate;
mod erosion;
mod geology;
mod hydrology;
mod knobs;
mod post;
mod render;
mod search;
mod serve;
mod spawn;
mod world;

use geology::Geology;
use hydrology::Analysis;
use world::{Params, World};

/// Iterations between refreshes of the rain field (it follows the relief).
const RAIN_REFRESH_ITERS: usize = 25;

pub struct Forged {
    pub world: World,
    pub initial: World,
    pub geology: Geology,
    pub rain: Vec<f64>,
    pub analysis: Analysis,
    /// Highest point the simulation built, before the height-band fit (m).
    pub simulated_peak_m: f64,
    /// Factor the fit applied to land heights (1.0 = relief was in band).
    pub height_fit: f64,
}

impl Forged {
    /// The world file the engine serves (`VOXEL_WORLD_FILE`, worldgen::forged).
    pub fn artifact(&self, p: &Params) -> worldgen::forged::WorldArtifact {
        use worldgen::forged::{Lake, RiverPoint, RiverReach, WorldArtifact, ARTIFACT_VERSION};
        let (w, a) = (&self.world, &self.analysis);
        // Lake water surface per cell, from the lake bodies' own levels.
        let level = erosion::route(w, 0.0).filled;
        let lake_level = (0..w.n * w.n)
            .map(|i| {
                let lake = !w.ocean[i] && a.lake_depth[i] >= hydrology::LAKE_MIN_DEPTH_M;
                if lake { level[i] as f32 } else { f32::NAN }
            })
            .collect();
        let spawn = spawn::pick(w, a, p.seed);
        WorldArtifact {
            version: ARTIFACT_VERSION,
            seed: p.seed,
            size_m: p.size_m,
            cell_m: w.dx,
            n: w.n as u32,
            height: w.h.iter().map(|&h| h as f32).collect(),
            lake_level,
            uplifted: w.uplifted.iter().map(|&u| u as f32).collect(),
            bedding: self.geology.bedding.iter().map(|&b| b as f32).collect(),
            column: self.geology.column().iter().map(|&(top, rock)| (top as f32, rock)).collect(),
            rock_names: geology::ROCKS.iter().map(|r| r.name.to_string()).collect(),
            rivers: a
                .rivers
                .iter()
                .map(|r| RiverReach {
                    order: r.order,
                    discharge_m2: r.discharge_m2,
                    width_m: r.width_m as f32,
                    points: r
                        .points
                        .iter()
                        .map(|q| RiverPoint {
                            x: q.x as f32,
                            z: q.z as f32,
                            bed: q.bed as f32,
                            water: q.water as f32,
                            in_lake: q.in_lake > 0.5,
                        })
                        .collect(),
                })
                .collect(),
            lakes: a
                .lakes
                .iter()
                .map(|l| Lake {
                    level: l.level as f32,
                    area_m2: l.area_m2 as f32,
                    max_depth: l.max_depth as f32,
                    outlet: [l.outlet.0 as f32, l.outlet.1 as f32],
                })
                .collect(),
            spawn: [spawn.0, spawn.1],
        }
    }
}

/// Forge one world: initial condition -> evolution -> shaping -> analysis.
pub fn forge(p: &Params) -> Forged {
    let mut w = World::new(p);
    let initial = World { h: w.h.clone(), ocean: w.ocean.clone(), uplift: Vec::new(), uplifted: Vec::new(), ..w };
    let geo = Geology::new(p, &w);
    let started = std::time::Instant::now();
    let mut scratch = Vec::new();
    let mut rain = climate::rainfall(&w, p);
    let (k, iters) = (&p.knobs, p.knobs.iterations());
    for iter in 1..=iters {
        if iter % RAIN_REFRESH_ITERS == 0 {
            rain = climate::rainfall(&w, p);
        }
        let drowned = erosion::step(&mut w, p, &geo, &rain, &mut scratch);
        if !p.quiet && (iter % 100 == 0 || iter == iters) {
            let max = w.h.iter().copied().fold(f64::MIN, f64::max);
            println!(
                "  iter {iter:4}: max height {max:6.1} m, {drowned} cells under depressions, {:.1?} elapsed",
                started.elapsed()
            );
        }
    }
    let simulated_peak_m = w.h.iter().copied().fold(f64::MIN, f64::max);
    let height_fit = post::fit_height_band(&mut w, k);
    if !p.quiet && height_fit < 1.0 {
        println!("relief overshot the height band: land heights scaled by {height_fit:.3}");
    }
    rain = climate::rainfall(&w, p);
    post::carve_floodplains(&mut w, &rain, k);
    let basins = post::carve_lake_basins(&mut w, &rain, p.seed, k);
    if !p.quiet {
        println!("planed valley floors; excavated {basins} lake basins");
    }
    let analysis = hydrology::analyze(&w, &geo, &rain, k);
    Forged { world: w, initial, geology: geo, rain, analysis, simulated_peak_m, height_fit }
}

fn main() {
    let p = Params::parse();
    if let Some(dir) = p.out.parent() {
        std::fs::create_dir_all(dir).expect("create output directory");
    }
    if let Some(port) = p.serve {
        serve::run(&p, port);
        return;
    }
    if p.search > 0 {
        search::run(&p);
        return;
    }
    let n = (p.size_m / p.cell_m).round() as usize;
    println!(
        "forge: seed {:#018X}, {} m world, {} m cells ({n}x{n}), {} iterations x {} yr, rock contrast {}, rain shadow {}",
        p.seed, p.size_m, p.cell_m, p.knobs.iterations(), p.knobs.dt_years,
        p.knobs.geology_strength, p.knobs.climate_strength,
    );
    let changed: Vec<String> = knobs::SPECS
        .iter()
        .filter_map(|s| {
            let v = p.knobs.get(s.key).unwrap();
            (v != s.default).then(|| format!("{}={v}", s.key))
        })
        .collect();
    if !changed.is_empty() {
        println!("knobs changed from their defaults: {}", changed.join(" "));
    }
    let forged = forge(&p);
    let (w, a) = (&forged.world, &forged.analysis);
    hydrology::print_report("forged world", w.dx, &a.stats);
    println!("world score {:.3}:", search::score(&a.stats, w.n * w.n));
    for (name, value) in search::criteria(&a.stats, w.n * w.n) {
        println!("  {name:<13} {value:.2}");
    }

    let stem = p.out.to_string_lossy().to_string();
    render::relief(&forged.initial, None, &format!("{stem}-initial.png"), false);
    render::relief(w, None, &format!("{stem}-relief.png"), false);
    render::relief(w, Some(a), &format!("{stem}-rivers.png"), false);
    render::geology(w, a, &format!("{stem}-geology.png"));
    render::rainfall(w, a, &forged.rain, &format!("{stem}-rain.png"));
    render::heightmap(w, &format!("{stem}-height.png"));
    let objects = format!("{stem}-water.json");
    std::fs::write(&objects, hydrology::objects_json(w, a)).expect("write water objects");
    println!("wrote {objects} ({} lakes, {} river reaches)", a.lakes.len(), a.rivers.len());

    let artifact = forged.artifact(&p);
    let world_file = format!("{stem}.world");
    artifact.save(std::path::Path::new(&world_file)).expect("write world file");
    let bytes = std::fs::metadata(&world_file).map(|m| m.len()).unwrap_or(0);
    println!(
        "wrote {world_file} ({:.1} MiB), spawn at ({:.0}, {:.0}) m — serve it with {}={world_file}",
        bytes as f64 / (1024.0 * 1024.0),
        artifact.spawn[0],
        artifact.spawn[1],
        worldgen::forged::WORLD_FILE_ENV
    );
}
