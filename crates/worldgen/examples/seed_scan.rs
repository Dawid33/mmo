//! Scan candidate world seeds for dry land at the spawn column (world block
//! (8,8) = center of region (0,0)). Used to pick WORLD_SEED; keep around for
//! the next time the terrain shaping changes.
//! Run: cargo run -p worldgen --example seed_scan --release

use worldgen::pipeline::{TerrainPipeline, SEA_LEVEL};

fn surface_y(p: &TerrainPipeline, wx: f64, wz: f64) -> i32 {
    let prof = p.column(wx, wz);
    let mut by = 320;
    while by >= -64 {
        if prof.density_at(by as f64 + 0.5) > 0.0 {
            return by;
        }
        by -= 1;
    }
    -65
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
        if min_h >= sea + 2 && max_h - min_h <= 12 {
            println!("  candidate {seed:#018X}: spawn y={spawn}, ±48m min={min_h} max={max_h}");
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
