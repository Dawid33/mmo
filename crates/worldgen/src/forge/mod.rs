//! The world forge: a whole finite world — the Planet, a 2:1 map that wraps
//! east–west with polar seas north and south (default the 16 × 8 km dev
//! size at 32 m cells) — generated OFFLINE by
//! simulating landscape evolution instead of evaluating
//! noise per point. `forge` runs the pipeline and `Forged::artifact` turns it
//! into the stored world file the engine serves (`crate::forged`). The CLI,
//! PNG rendering, seed search and tuning page live in
//! `examples/forge_proto` (`just forge-proto`, `just forge-ui`).
//!
//! Pipeline:
//!   1. world      the continents, cut by seas and held off the polar edges
//!                 by an ocean margin — low initial relief, an UPLIFT map
//!                 with mountain belts along convergent plate boundaries
//!                 (`plates`), interior ranges and volcano cones;
//!   2. geology    rock column in a stratigraphic coordinate -> erodibility of
//!                 whatever rock the surface currently cuts;
//!   3. climate    latitude, temperature and banded-wind precipitation (mm/yr)
//!                 on the current heights -> discharge weights;
//!   4. erosion    drainage routing (a Priority-Flood on the first iteration,
//!                 then the linear-time depression graph of `routing_fast`),
//!                 stream-power incision, depression silting, hillslope
//!                 diffusion, per iteration — coarse-to-fine (`schedule`):
//!                 most iterations on 2x/4x-coarser cells, then a few at
//!                 each finer level after `World::upsample`;
//!   5. post       valley floors planed beside rivers; overdeepened basins
//!                 flooded to their spill level = lakes;
//!   6. hydrology  lakes and rivers as objects + statistics;
//!   7. fields     drainage, volcanism, salinity (0..100 per cell) and the
//!                 volcano sites, whose cones `World::new` already raised;
//!   8. spawn      a flat dry coastal site;
//!   9. regions    (in `Forged::artifact`) biome per cell, named regions by
//!                 flood fill, good / evil per region, savagery.
//! `knobs` holds every tunable.
//!
//! Not bit-reproducible across machines by design (sqrt/ln/sin used freely):
//! the forge's output is a stored artifact, not a function both sides evaluate.

pub mod climate;
pub mod erosion;
pub mod fields;
pub mod geology;
pub mod hydrology;
pub mod knobs;
mod par;
pub mod periodic;
pub mod plates;
pub mod play;
pub mod post;
pub mod presets;
pub mod quality;
pub mod regions;
pub mod render;
pub mod routing_fast;
pub mod spawn;
pub mod world;

pub use knobs::Knobs;
pub use world::{ForgeParams, World};

use geology::Geology;
use hydrology::Analysis;

/// Iterations between refreshes of the climate (its rain follows the relief).
const RAIN_REFRESH_ITERS: usize = 25;

pub struct Forged {
    pub world: World,
    pub initial: World,
    pub geology: Geology,
    /// The final climate (stored in the world file).
    pub climate: climate::Climate,
    /// Discharge weights from the final climate (`climate::discharge_weight`).
    pub rain: Vec<f64>,
    pub analysis: Analysis,
    /// Drainage, volcanism, salinity and the volcano sites (stored).
    pub fields: fields::Fields,
    /// Highest point the simulation built, before the height-band fit (m).
    pub simulated_peak_m: f64,
    /// Factor the fit applied to land heights (1.0 = relief was in band).
    pub height_fit: f64,
}

impl Forged {
    /// The world file the engine serves (`VOXEL_WORLD_FILE`, worldgen::forged).
    pub fn artifact(&self, p: &ForgeParams) -> crate::forged::WorldArtifact {
        use crate::forged::{
            RiverPoint, RiverReach, WorldArtifact, ARTIFACT_VERSION, GEO_STEP_M, HEIGHT_STEP_M, PRECIP_STEP_MM,
            TEMPERATURE_STEP_C,
        };
        let (w, a) = (&self.world, &self.analysis);
        // Lake water surface per cell, from the lake bodies' own levels.
        let level = erosion::route(w, 0.0).filled;
        let lakes = self.lake_bodies(&level);
        let spawn = spawn::pick(w, a, p.seed);
        let mut artifact = WorldArtifact {
            version: ARTIFACT_VERSION,
            seed: p.seed,
            // The grid's own extent: exactly `nx * cell_m` by `nz * cell_m`.
            width_m: w.nx as f64 * w.dx,
            height_m: w.nz as f64 * w.dx,
            cell_m: w.dx,
            nx: w.nx as u32,
            nz: w.nz as u32,
            height: quantise(&w.h, HEIGHT_STEP_M),
            uplifted: quantise(&w.uplifted, GEO_STEP_M),
            bedding: quantise(&self.geology.bedding, GEO_STEP_M),
            temperature_c: crate::forged::QRaster::fit(&self.climate.temperature_c, TEMPERATURE_STEP_C),
            precip_mm: crate::forged::QRaster::fit(&self.climate.precip_mm, PRECIP_STEP_MM),
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
            lakes,
            spawn: [spawn.0, spawn.1],
            edges: w.edges,
            drainage: self.fields.drainage.clone(),
            volcanism: self.fields.volcanism.clone(),
            salinity: self.fields.salinity.clone(),
            volcanoes: self.fields.volcanoes.clone(),
            biome: Vec::new(),
            region: Vec::new(),
            regions: Vec::new(),
            savagery: Vec::new(),
            knobs: crate::world_store::knobs_map(&p.knobs),
            climate: climate::monthly_placeholder(w, &self.climate, p).quantise(w.wrap_x),
        };
        // Classified from the stored (quantised) fields, so a reader that
        // re-classifies a cell gets the stored biome.
        regions::derive(&mut artifact, p.knobs.good_share, p.knobs.evil_share);
        artifact
    }
}

/// `v` as an `f32` raster quantised over its own range (`QRaster::fit`).
fn quantise(v: &[f64], preferred_step: f32) -> crate::forged::QRaster {
    let v: Vec<f32> = v.iter().map(|&x| x as f32).collect();
    crate::forged::QRaster::fit(&v, preferred_step)
}

impl Forged {
    /// The lake bodies as stored in the world file: 4-connected groups of
    /// lake cells (final flood at least `LAKE_MIN_DEPTH_M` deep, not ocean)
    /// that stand at one water level, each with its cells as row-major runs.
    /// `level` is the final flood surface per cell. Outlets come from the
    /// hydrology lake of the same level nearest the body.
    fn lake_bodies(&self, level: &[f64]) -> Vec<crate::forged::Lake> {
        let (w, a) = (&self.world, &self.analysis);
        let is_lake = |i: usize| !w.ocean[i] && a.lake_depth[i] >= hydrology::LAKE_MIN_DEPTH_M;
        let mut seen = vec![false; w.cells()];
        let mut lakes = Vec::new();
        for start in 0..w.cells() {
            if seen[start] || !is_lake(start) {
                continue;
            }
            let water = level[start] as f32;
            let (mut stack, mut cells) = (vec![start], Vec::new());
            seen[start] = true;
            while let Some(i) = stack.pop() {
                cells.push(i);
                let mut visit = |j: usize| {
                    if !seen[j] && is_lake(j) && level[j] as f32 == water {
                        seen[j] = true;
                        stack.push(j);
                    }
                };
                // West, east, north, south: x wraps round on a wrapping world.
                for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                    if let Some(j) = w.step(i, dx, dz) {
                        visit(j);
                    }
                }
            }
            cells.sort_unstable();
            // On a wrapping world a lake may hold the last cell of one row
            // and the first of the next: a run never continues into the
            // next row, so every run stays within one.
            let row_start = |i: usize| w.wrap_x && i % w.nx == 0;
            let mut runs: Vec<(u32, u32)> = Vec::new();
            for &i in &cells {
                match runs.last_mut() {
                    Some((s, len)) if (*s + *len) as usize == i && !row_start(i) => *len += 1,
                    _ => runs.push((i as u32, 1)),
                }
            }
            let centre = w.center(cells[0]);
            let outlet = a
                .lakes
                .iter()
                .filter(|l| (l.level as f32 - water).abs() < 0.01)
                .map(|l| l.outlet)
                .min_by(|p, q| {
                    let d = |o: &(f64, f64)| (o.0 - centre.0).powi(2) + (o.1 - centre.1).powi(2);
                    d(p).total_cmp(&d(q))
                })
                .unwrap_or(centre);
            lakes.push(crate::forged::Lake {
                level: water,
                area_m2: (cells.len() as f64 * w.dx * w.dx) as f32,
                max_depth: cells.iter().map(|&i| a.lake_depth[i]).fold(0.0, f64::max) as f32,
                outlet: [outlet.0 as f32, outlet.1 as f32],
                cells: runs,
            });
        }
        lakes
    }
}

/// Which phase of the forge is running (for a progress bar).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForgeStage {
    Setup,
    Eroding,
    Shaping,
    Hydrology,
    /// Building and writing the world file (`play::forge_to_file`; never
    /// reported by `forge_with`).
    Saving,
}

impl ForgeStage {
    pub fn label(self) -> &'static str {
        match self {
            ForgeStage::Setup => "Preparing",
            ForgeStage::Eroding => "Eroding",
            ForgeStage::Shaping => "Shaping valleys",
            ForgeStage::Hydrology => "Rivers and lakes",
            ForgeStage::Saving => "Saving the world",
        }
    }
}

/// One progress report: `iter` of `iters` erosion iterations are done
/// (0 outside `Eroding`, except `iters` which is always the total).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForgeProgress {
    pub stage: ForgeStage,
    pub iter: usize,
    pub iters: usize,
}

/// Forge one world: initial condition -> evolution -> shaping -> analysis.
pub fn forge(p: &ForgeParams) -> Forged {
    forge_with(p, &mut |_| std::ops::ControlFlow::Continue(())).expect("callback never breaks")
}

/// Coarse erosion levels stop before a grid this many cells across: coarser,
/// the land and the ocean margin no longer resolve (`World::new` needs sea
/// along the polar edges).
pub const MIN_LEVEL_CELLS: usize = 32;

/// Levels between the coarsest and the final one run this share of
/// `refine_iters`. Measured on the 16 km / 32 m quality gate: with every finer
/// level at the full `refine_iters`, concavity theta sat on the gate's edge
/// (0.72-0.74 vs a 0.733 limit, pass/fail flipping with single iterations);
/// a short middle level and a longer final one passed for every
/// `refine_iters` from 70 to 88 (theta 0.71-0.73).
pub const INTERMEDIATE_REFINE_SHARE: f64 = 0.5;

/// The erosion schedule: cell size and iterations per level, coarsest first,
/// ending at `p.cell_m`. The coarsest level runs `iters`, the final one
/// `refine_iters`, any between `INTERMEDIATE_REFINE_SHARE` of that; up to
/// `coarse_levels` 2x-coarser levels, fewer when one would drop under
/// `MIN_LEVEL_CELLS` cells across.
pub fn schedule(p: &ForgeParams) -> Vec<(f64, usize)> {
    let k = &p.knobs;
    let mut levels = vec![p.cell_m];
    for _ in 0..k.coarse_level_count() {
        let cell = levels[0] * 2.0;
        let (nx, nz) = ((p.width_m / cell).round() as usize, (p.height_m / cell).round() as usize);
        if nx.min(nz) < MIN_LEVEL_CELLS {
            break;
        }
        levels.insert(0, cell);
    }
    let n_levels = levels.len();
    levels
        .iter()
        .enumerate()
        .map(|(l, &cell)| {
            let iters = if l == 0 {
                k.iterations()
            } else if l + 1 < n_levels {
                ((k.refine_iterations() as f64 * INTERMEDIATE_REFINE_SHARE).round() as usize).max(1)
            } else {
                k.refine_iterations()
            };
            (cell, iters)
        })
        .collect()
}

/// Whether `p` may be forged as far as its east–west wrap goes: every world
/// wraps, so it is at least `play::MIN_WRAP_WIDTH_M` wide, a whole number of
/// its coarsest erosion cells (`upsample_onto` samples x round the circle: a
/// width between two coarse cells would put the mismatch on the seam) and of
/// `play::WRAP_WIDTH_STEP_M` (its regions tile the circle).
pub fn check_wrap(p: &ForgeParams) -> Result<(), String> {
    if let Some(why) = play::too_narrow_to_wrap(p.width_m) {
        return Err(why);
    }
    let whole = |step: f64| (p.width_m / step).fract() == 0.0;
    let coarsest_m = schedule(p)[0].0;
    if !whole(coarsest_m) {
        return Err(format!(
            "a world that wraps east–west must be a whole number of its coarsest erosion cells wide \
             ({:.0} m is not a multiple of {coarsest_m:.0} m)",
            p.width_m
        ));
    }
    if !whole(play::WRAP_WIDTH_STEP_M) {
        return Err(format!(
            "a world that wraps east–west must be a whole number of {:.0} m regions wide ({:.0} m is not)",
            play::WRAP_WIDTH_STEP_M,
            p.width_m
        ));
    }
    Ok(())
}

/// `forge` with a progress callback, called at each stage boundary and after
/// every erosion iteration (numbered across all levels of the schedule).
/// Returning `Break` abandons the forge (`None`).
///
/// # Panics
/// On a world [`check_wrap`] or [`climate::check_climate_grid`] refuses
/// (every caller catches the panic and shows its message).
pub fn forge_with(
    p: &ForgeParams,
    on_progress: &mut dyn FnMut(ForgeProgress) -> std::ops::ControlFlow<()>,
) -> Option<Forged> {
    if let Err(why) = check_wrap(p) {
        panic!("{why}");
    }
    if let Err(why) = climate::check_climate_grid(p) {
        panic!("{why}");
    }
    let levels = schedule(p);
    let iters: usize = levels.iter().map(|&(_, n)| n).sum();
    let mut report = |stage, iter| on_progress(ForgeProgress { stage, iter, iters }).is_break();
    if report(ForgeStage::Setup, 0) {
        return None;
    }
    // The final grid's initial condition is also the "before erosion" world.
    let fine = World::new(p);
    let initial = World {
        h: fine.h.clone(),
        ocean: fine.ocean.clone(),
        outlet: fine.outlet.clone(),
        uplift: Vec::new(),
        uplifted: Vec::new(),
        ..fine
    };
    let mut fine = Some(fine);
    let started = std::time::Instant::now();
    let mut w: Option<World> = None;
    let mut geo: Option<Geology> = None;
    let mut done = 0;
    for (level, &(cell_m, level_iters)) in levels.iter().enumerate() {
        let last = level + 1 == levels.len();
        let lp = ForgeParams { cell_m, ..p.clone() };
        let base = if last { fine.take().expect("final grid built once") } else { World::new(&lp) };
        let mut lw = match w.take() {
            Some(coarse) => coarse.upsample_onto(base),
            None => base,
        };
        // Per level: geology and rain on this grid, and a fresh scratch so the
        // first step floods the upsampled surface with the full Priority-Flood.
        let level_geo = Geology::new(&lp, &lw);
        let mut scratch = erosion::StepScratch::default();
        let mut rain = climate::discharge_weight(&climate::compute(&lw, &lp), &lw);
        if !p.quiet && levels.len() > 1 {
            let (number, nx, nz) = (level + 1, lw.nx, lw.nz);
            println!("  level {number}: {cell_m} m cells ({nx}x{nz}), {level_iters} iterations");
        }
        for iter in 1..=level_iters {
            if iter % RAIN_REFRESH_ITERS == 0 {
                // Lakes from the last step's routing: no extra flood.
                let lakes = match scratch.filled() {
                    Some(filled) => climate::lakes_from_filled(&lw, filled),
                    None => vec![false; lw.cells()],
                };
                rain = climate::discharge_weight(&climate::compute_with_lakes(&lw, &lp, &lakes), &lw);
            }
            let drowned = erosion::step(&mut lw, &lp, &level_geo, &rain, &mut scratch);
            done += 1;
            if !p.quiet && (done % 100 == 0 || done == iters) {
                let max = lw.h.iter().copied().fold(f64::MIN, f64::max);
                println!(
                    "  iter {done:4}: max height {max:6.1} m, {drowned} cells under depressions, {:.1?} elapsed",
                    started.elapsed()
                );
            }
            if report(ForgeStage::Eroding, done) {
                return None;
            }
        }
        w = Some(lw);
        geo = Some(level_geo);
    }
    let (mut w, geo) = (w.expect("the schedule has a final level"), geo.expect("final geology"));
    let k = &p.knobs;
    if report(ForgeStage::Shaping, 0) {
        return None;
    }
    let simulated_peak_m = w.h.iter().copied().fold(f64::MIN, f64::max);
    let height_fit = post::fit_height_band(&mut w, k);
    if !p.quiet && height_fit < 1.0 {
        println!("relief overshot the height band: land heights scaled by {height_fit:.3}");
    }
    let rain = climate::discharge_weight(&climate::compute(&w, p), &w);
    post::carve_floodplains(&mut w, &rain, k);
    let basins = post::carve_lake_basins(&mut w, &rain, p.seed, k);
    if !p.quiet {
        println!("planed valley floors; excavated {basins} lake basins");
    }
    if report(ForgeStage::Hydrology, 0) {
        return None;
    }
    // The stored climate sees the final heights and lakes.
    let climate = climate::compute(&w, p);
    let rain = climate::discharge_weight(&climate, &w);
    let analysis = hydrology::analyze(&w, &geo, &rain, k);
    let fields = fields::compute(p, &w, &analysis, &geo);
    Some(Forged { world: w, initial, geology: geo, climate, rain, analysis, fields, simulated_peak_m, height_fit })
}

