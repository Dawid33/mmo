//! Orographic rainfall on the CURRENT heightfield: moist air crosses the world
//! on the prevailing wind, rains out as it is forced up slopes, and arrives
//! dry on the lee side. The rain field weights discharge, so wet flanks grow
//! bigger rivers and erode faster — Dwarf Fortress computes rain shadows AFTER
//! its rivers, which is why a DF desert river is as wide as a jungle one.

use crate::world::{Params, World};

// Sea recharge, land rain-out distance, orographic scale and the rain bounds
// are knobs (knobs.rs, "Climate").
/// Heights are blurred over this radius (cells) before taking windward slopes,
/// so rain follows ranges, not individual gullies.
const TERRAIN_BLUR_CELLS: usize = 6;
/// Cross-wind blur of the rain field (cells): air mixes sideways.
const RAIN_BLUR_CELLS: usize = 5;

fn box_blur_rows(src: &[f64], n: usize, radius: usize, along_x: bool) -> Vec<f64> {
    let mut out = vec![0.0; n * n];
    for a in 0..n {
        let at = |b: usize| if along_x { src[a * n + b] } else { src[b * n + a] };
        let mut sum: f64 = (0..=radius.min(n - 1)).map(at).sum();
        let mut count = radius.min(n - 1) + 1;
        for b in 0..n {
            let v = sum / count as f64;
            if along_x { out[a * n + b] = v } else { out[b * n + a] = v }
            if b + radius + 1 < n {
                sum += at(b + radius + 1);
                count += 1;
            }
            if b >= radius {
                sum -= at(b - radius);
                count -= 1;
            }
        }
    }
    out
}

/// Rain per cell relative to the land mean (1.0 = average). Wind blows from
/// the west (+x); uniform 1.0 when the climate is switched off.
pub fn rainfall(w: &World, p: &Params) -> Vec<f64> {
    let n = w.n;
    let k = &p.knobs;
    if k.climate_strength <= 0.0 {
        return vec![1.0; n * n];
    }
    let ground: Vec<f64> = w.h.iter().map(|h| h.max(0.0)).collect();
    let ground = box_blur_rows(&ground, n, TERRAIN_BLUR_CELLS, true);
    let ground = box_blur_rows(&ground, n, TERRAIN_BLUR_CELLS, false);

    let mut rain = vec![0.0; n * n];
    for z in 0..n {
        let mut moisture: f64 = 1.0;
        for x in 1..n {
            let i = z * n + x;
            if w.ocean[i] {
                moisture += (1.0 - moisture) * (w.dx / k.sea_recharge_m).min(1.0);
                continue;
            }
            let rise = (ground[i] - ground[i - 1]).max(0.0);
            let fallout = moisture * (w.dx / k.land_depletion_m + rise / k.orographic_scale_m).min(1.0);
            moisture -= fallout;
            rain[i] = fallout / w.dx;
        }
    }
    let rain = box_blur_rows(&rain, n, RAIN_BLUR_CELLS, false);
    let mut rain = box_blur_rows(&rain, n, 2, true);

    let (mut sum, mut land) = (0.0, 0usize);
    for i in 0..n * n {
        if !w.ocean[i] {
            sum += rain[i];
            land += 1;
        }
    }
    let mean = (sum / land.max(1) as f64).max(f64::MIN_POSITIVE);
    for (i, r) in rain.iter_mut().enumerate() {
        *r = if w.ocean[i] {
            1.0
        } else {
            let relative = (*r / mean).clamp(k.rain_min, k.rain_max.max(k.rain_min));
            1.0 + k.climate_strength.min(1.0) * (relative - 1.0)
        };
    }
    rain
}
