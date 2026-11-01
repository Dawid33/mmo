//! The world forge: a whole finite world (default 16 km, ocean all round)
//! generated OFFLINE by simulating landscape evolution instead of evaluating
//! noise per point. `forge` runs the pipeline and `Forged::artifact` turns it
//! into the stored world file the engine serves (`crate::forged`). The CLI,
//! PNG rendering, seed search and tuning page live in
//! `examples/forge_proto` (`just forge-proto`, `just forge-ui`).
//!
//! Pipeline:
//!   1. world      noise island inside a guaranteed ocean margin, low initial
//!                 relief, an UPLIFT map with mountain belts inland;
//!   2. geology    rock column in a stratigraphic coordinate -> erodibility of
//!                 whatever rock the surface currently cuts;
//!   3. climate    orographic rain on the current heights -> discharge weights;
//!   4. erosion    drainage routing (a Priority-Flood on the first iteration,
//!                 then the linear-time depression graph of `routing_fast`),
//!                 stream-power incision, depression silting, hillslope
//!                 diffusion, per iteration — coarse-to-fine (`schedule`):
//!                 most iterations on 2x/4x-coarser cells, then a few at
//!                 each finer level after `World::upsample`;
//!   5. post       valley floors planed beside rivers; overdeepened basins
//!                 flooded to their spill level = lakes;
//!   6. hydrology  lakes and rivers as objects + statistics;
//!   7. spawn      a flat dry coastal site.
//! `knobs` holds every tunable.
//!
//! Not bit-reproducible across machines by design (sqrt/ln/sin used freely):
//! the forge's output is a stored artifact, not a function both sides evaluate.

pub mod climate;
pub mod erosion;
pub mod geology;
pub mod hydrology;
pub mod knobs;
mod par;
pub mod post;
pub mod presets;
pub mod quality;
pub mod render;
pub mod routing_fast;
pub mod spawn;
pub mod world;

pub use knobs::Knobs;
pub use world::{ForgeParams, World};

use geology::Geology;
use hydrology::Analysis;

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
    pub fn artifact(&self, p: &ForgeParams) -> crate::forged::WorldArtifact {
        use crate::forged::{Lake, RiverPoint, RiverReach, WorldArtifact, ARTIFACT_VERSION};
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
            rain: self.rain.iter().map(|&r| r as f32).collect(),
            coast_dist_m: crate::habitat::coast_distance_m(&w.h, w.n, w.dx),
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

/// Which phase of the forge is running (for a progress bar).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForgeStage {
    Setup,
    Eroding,
    Shaping,
    Hydrology,
}

impl ForgeStage {
    pub fn label(self) -> &'static str {
        match self {
            ForgeStage::Setup => "Preparing",
            ForgeStage::Eroding => "Eroding",
            ForgeStage::Shaping => "Shaping valleys",
            ForgeStage::Hydrology => "Rivers and lakes",
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
/// the island and the ocean margin no longer resolve (`World::new` needs an
/// ocean ring at the edge).
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
        if ((p.size_m / cell).round() as usize) < MIN_LEVEL_CELLS {
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

/// `forge` with a progress callback, called at each stage boundary and after
/// every erosion iteration (numbered across all levels of the schedule).
/// Returning `Break` abandons the forge (`None`).
pub fn forge_with(
    p: &ForgeParams,
    on_progress: &mut dyn FnMut(ForgeProgress) -> std::ops::ControlFlow<()>,
) -> Option<Forged> {
    let levels = schedule(p);
    let iters: usize = levels.iter().map(|&(_, n)| n).sum();
    let mut report = |stage, iter| on_progress(ForgeProgress { stage, iter, iters }).is_break();
    if report(ForgeStage::Setup, 0) {
        return None;
    }
    // The final grid's initial condition is also the "before erosion" world.
    let fine = World::new(p);
    let initial = World { h: fine.h.clone(), ocean: fine.ocean.clone(), uplift: Vec::new(), uplifted: Vec::new(), ..fine };
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
        let mut rain = climate::rainfall(&lw, &lp);
        if !p.quiet && levels.len() > 1 {
            let (number, n) = (level + 1, lw.n);
            println!("  level {number}: {cell_m} m cells ({n}x{n}), {level_iters} iterations");
        }
        for iter in 1..=level_iters {
            if iter % RAIN_REFRESH_ITERS == 0 {
                rain = climate::rainfall(&lw, &lp);
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
    let rain = climate::rainfall(&w, p);
    post::carve_floodplains(&mut w, &rain, k);
    let basins = post::carve_lake_basins(&mut w, &rain, p.seed, k);
    if !p.quiet {
        println!("planed valley floors; excavated {basins} lake basins");
    }
    if report(ForgeStage::Hydrology, 0) {
        return None;
    }
    let analysis = hydrology::analyze(&w, &geo, &rain, k);
    Some(Forged { world: w, initial, geology: geo, rain, analysis, simulated_peak_m, height_fit })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::ControlFlow;

    fn tiny() -> ForgeParams {
        let mut p = ForgeParams { size_m: 4_096.0, cell_m: 128.0, quiet: true, ..Default::default() };
        p.knobs.iters = 20.0;
        p
    }

    #[test]
    fn forge_with_reports_every_erosion_iteration() {
        let mut seen = Vec::new();
        forge_with(&tiny(), &mut |pr| {
            seen.push(pr);
            ControlFlow::Continue(())
        })
        .unwrap();
        let eroding: Vec<usize> = seen.iter().filter(|e| e.stage == ForgeStage::Eroding).map(|e| e.iter).collect();
        assert_eq!(eroding, (1..=20).collect::<Vec<_>>());
        assert!(seen.iter().all(|e| e.iters == 20));
        assert_eq!(seen.last().unwrap().stage, ForgeStage::Hydrology);
    }

    #[test]
    fn forge_with_cancels_between_iterations() {
        let mut seen = Vec::new();
        let r = forge_with(&tiny(), &mut |pr| {
            seen.push(pr);
            if pr.stage == ForgeStage::Eroding && pr.iter == 3 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        });
        assert!(r.is_none());
        let last = seen.last().unwrap();
        assert_eq!((last.stage, last.iter), (ForgeStage::Eroding, 3));
    }

    #[test]
    fn schedule_levels_add_up() {
        // 4 km / 32 m: 128 cells across; the coarse levels run at 128 m
        // (32 cells) then 64 m (64 cells), none under MIN_LEVEL_CELLS.
        let mut p = ForgeParams { size_m: 4_096.0, cell_m: 32.0, quiet: true, ..Default::default() };
        p.knobs.coarse_levels = 2.0;
        p.knobs.iters = 20.0;
        p.knobs.refine_iters = 6.0;
        // Coarsest: iters; middle: INTERMEDIATE_REFINE_SHARE of refine; final: refine.
        assert_eq!(schedule(&p), vec![(128.0, 20), (64.0, 3), (32.0, 6)]);
        let mut eroding = Vec::new();
        let forged = forge_with(&p, &mut |pr| {
            assert_eq!(pr.iters, 29);
            if pr.stage == ForgeStage::Eroding {
                eroding.push(pr.iter);
            }
            ControlFlow::Continue(())
        })
        .unwrap();
        assert_eq!(eroding, (1..=29).collect::<Vec<_>>());
        assert_eq!(forged.world.n, (p.size_m / p.cell_m) as usize);
        assert_eq!(forged.world.dx, p.cell_m);
    }

    #[test]
    fn schedule_stops_above_min_level_cells() {
        // 4 km / 128 m = 32 cells: no coarser level fits.
        let mut p = ForgeParams { size_m: 4_096.0, cell_m: 128.0, ..Default::default() };
        p.knobs.coarse_levels = 3.0;
        assert_eq!(schedule(&p), vec![(128.0, p.knobs.iterations())]);
        p.knobs.coarse_levels = 0.0;
        p.cell_m = 16.0;
        assert_eq!(schedule(&p), vec![(16.0, p.knobs.iterations())]);
    }

    #[test]
    fn cancel_in_a_refine_level() {
        let mut p = ForgeParams { size_m: 4_096.0, cell_m: 32.0, quiet: true, ..Default::default() };
        (p.knobs.coarse_levels, p.knobs.iters, p.knobs.refine_iters) = (2.0, 4.0, 4.0);
        let mut last = None;
        let r = forge_with(&p, &mut |pr| {
            last = Some(pr);
            if pr.stage == ForgeStage::Eroding && pr.iter == 7 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        });
        assert!(r.is_none());
        assert_eq!(last.map(|l| (l.iter, l.iters)), Some((7, 10)));
    }

    #[test]
    fn forge_matches_forge_with() {
        let p = tiny();
        let a = forge(&p);
        let b = forge_with(&p, &mut |_| ControlFlow::Continue(())).unwrap();
        assert_eq!(a.world.h, b.world.h);
    }
}

