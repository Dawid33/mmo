//! Scan candidate world seeds for a good spawn (world block (8,8) = center of
//! region (0,0)): dry and flat underfoot, with a mountain and the sea both
//! inside the LOD horizon so the first view shows what the world is. Used to
//! pick WORLD_SEED; keep around for the next time the terrain shaping changes.
//! Check the winner with `just world-map --span 4096`.
//! Run: cargo run -p worldgen --example seed_scan --release

use worldgen::pipeline::{TerrainPipeline, SEA_LEVEL};

/// Scenery search window around spawn: ±(VIEW_STEPS * VIEW_STEP_M) meters.
const VIEW_STEPS: i32 = 15;
const VIEW_STEP_M: f64 = 100.0;
/// A peak this far above the sea must stand inside that window.
const MIN_PEAK_ABOVE_SEA: i32 = 110;

/// The height band scanned for the surface (blocks), shared with the other
/// height tools (examples/world_map.rs, forge_proto/render.rs).
const HEIGHT_MIN_Y: i32 = -1_000;
const HEIGHT_MAX_Y: i32 = 3_600;

/// Topmost solid block of the column (`HEIGHT_MIN_Y - 1` if none). Density
/// falls monotonically with y, so an integer bisection finds the same block a
/// top-down scan would, in ~13 probes instead of thousands.
fn surface_y(p: &TerrainPipeline, wx: f64, wz: f64) -> i32 {
    let prof = p.column(wx, wz);
    let solid = |by: i32| prof.density_at(by as f64 + 0.5) > 0.0;
    if !solid(HEIGHT_MIN_Y) {
        return HEIGHT_MIN_Y - 1;
    }
    if solid(HEIGHT_MAX_Y) {
        return HEIGHT_MAX_Y;
    }
    // Invariant: solid(lo), !solid(hi).
    let (mut lo, mut hi) = (HEIGHT_MIN_Y, HEIGHT_MAX_Y);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if solid(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

fn main() {
    let sea = SEA_LEVEL as i32;
    let p = TerrainPipeline::new(worldgen::WORLD_SEED);
    println!(
        "current WORLD_SEED {:#018X}: spawn surface y = {} (sea = {})",
        worldgen::WORLD_SEED,
        surface_y(&p, 8.5, 8.5),
        sea
    );

    let mut found = 0;
    for s in 0u64..100_000 {
        let seed = 0x00D5_C0DE_0000 + s;
        let p = TerrainPipeline::new(seed);
        // Spawn column comfortably dry, but not on a mountain top.
        let spawn = surface_y(&p, 8.5, 8.5);
        if spawn < sea + 4 || spawn > sea + 40 {
            continue;
        }
        // Whole ±48 m neighborhood dry too (no swimming to shore), and
        // flat-ish (height spread cap) so we don't spawn against a cliff.
        let mut min_h = i32::MAX;
        let mut max_h = i32::MIN;
        for gx in -6..=6 {
            for gz in -6..=6 {
                let h = surface_y(&p, 8.5 + gx as f64 * 8.0, 8.5 + gz as f64 * 8.0);
                min_h = min_h.min(h);
                max_h = max_h.max(h);
            }
        }
        if min_h < sea + 2 || max_h - min_h > 12 {
            continue;
        }
        // Scenery inside the ~2 km LOD horizon: a real mountain and open sea.
        let (mut peak, mut lowest) = (i32::MIN, i32::MAX);
        for gx in -VIEW_STEPS..=VIEW_STEPS {
            for gz in -VIEW_STEPS..=VIEW_STEPS {
                let h = surface_y(&p, 8.5 + gx as f64 * VIEW_STEP_M, 8.5 + gz as f64 * VIEW_STEP_M);
                peak = peak.max(h);
                lowest = lowest.min(h);
            }
        }
        if peak >= sea + MIN_PEAK_ABOVE_SEA && lowest < sea - 4 {
            println!(
                "  candidate {seed:#018X}: spawn y={spawn}, ±48m min={min_h} max={max_h}, \
                 ±{}m peak={peak} lowest={lowest}",
                VIEW_STEPS as f64 * VIEW_STEP_M
            );
            found += 1;
            if found >= 10 {
                break;
            }
        }
    }
    if found == 0 {
        println!("no candidates — widen the scan or loosen the criteria");
    }
}
