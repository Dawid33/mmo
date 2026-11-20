//! Final hydrology: lakes and rivers as OBJECTS (what the engine will need to
//! carve channels and place water), plus the statistics used to judge a world.

use std::fmt::Write as _;

use crate::forge::erosion::{FLOOD_EPSILON, Routing, discharge, route};
use crate::forge::geology::{Geology, ROCKS};
use crate::forge::knobs::Knobs;
use crate::forge::world::World;

// `Knobs::river_min_discharge_m2`: a cell is a river once this much
// rain-weighted land drains through it. `Knobs::river_width_m`: channel width
// per sqrt(km^2) of discharge — hydraulic geometry, w ~ sqrt(Q).
/// A cell is lake when the final flood stands this deep over it (m).
pub const LAKE_MIN_DEPTH_M: f64 = 0.3;
const LAKE_MIN_CELLS: usize = 4;
const RIVER_MIN_WIDTH_M: f64 = 1.5;
/// Chaikin corner-cutting passes that turn the D8 staircase into a curve.
const RIVER_SMOOTHING_PASSES: usize = 2;
/// Landmasses smaller than this are islets, not islands (km^2).
const MIN_ISLAND_KM2: f64 = 0.25;
/// "Buildable" land for the world score: gentle, low, dry.
const BUILDABLE_MAX_SLOPE: f64 = 0.08;
const BUILDABLE_HEIGHT_M: (f64, f64) = (1.5, 80.0);

pub struct Lake {
    /// Water surface (m above sea): the basin's true spill level.
    pub level: f64,
    pub area_m2: f64,
    pub max_depth: f64,
    /// Where the water leaves (world meters).
    pub outlet: (f64, f64),
}

#[derive(Clone, Copy)]
pub struct RiverPoint {
    pub x: f64,
    pub z: f64,
    pub bed: f64,
    /// Water surface; never rises downstream (it is the flooded routing surface).
    pub water: f64,
    /// 1.0 inside a lake, 0.0 outside (fractional after smoothing).
    pub in_lake: f64,
}

/// One reach between two network nodes (source, confluence, mouth).
pub struct River {
    /// Strahler order: 1 = headwater, +1 where two equals meet.
    pub order: u32,
    pub discharge_m2: f64,
    pub width_m: f64,
    pub points: Vec<RiverPoint>,
}

#[derive(Default, Clone)]
pub struct Stats {
    pub land_share: f64,
    pub land_km2: f64,
    /// p10, p50, p90, p99, max of land height above sea (m).
    pub heights: [f64; 5],
    /// p50, p90, p99, max of downstream slope.
    pub slopes: [f64; 4],
    pub lake_count: usize,
    pub largest_lake_km2: f64,
    pub depression_km3: f64,
    pub mouths: usize,
    pub channel_km: f64,
    pub longest_river_km: f64,
    pub biggest_basin_km2: f64,
    pub max_order: u32,
    pub theta: f64,
    pub hack: f64,
    pub buildable_km2: f64,
    /// Share of the land surface per rock in `ROCKS`.
    pub rock_shares: [f64; 6],
    pub margin_drowned: usize,
    /// River threshold used (for the report line).
    pub river_min_discharge_m2: f64,
    /// Separate landmasses of at least MIN_ISLAND_KM2, and the largest (km^2).
    pub islands: usize,
    pub largest_island_km2: f64,
}

pub struct Analysis {
    pub routing: Routing,
    pub q: Vec<f64>,
    pub lake_depth: Vec<f64>,
    pub surface_rock: Vec<u8>,
    pub lakes: Vec<Lake>,
    pub rivers: Vec<River>,
    pub stats: Stats,
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

/// Least-squares slope of y on x.
fn regress(points: &[(f64, f64)]) -> f64 {
    let n = points.len() as f64;
    let (sx, sy) = points
        .iter()
        .fold((0.0, 0.0), |acc, p| (acc.0 + p.0, acc.1 + p.1));
    let (mx, my) = (sx / n, sy / n);
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for &(x, y) in points {
        sxy += (x - mx) * (y - my);
        sxx += (x - mx) * (x - mx);
    }
    sxy / sxx
}

fn find_lakes(
    w: &World,
    routing: &Routing,
    q: &[f64],
    lake_depth: &[f64],
    level: &[f64],
) -> Vec<Lake> {
    let is_lake = |i: usize| !w.ocean[i] && lake_depth[i] >= LAKE_MIN_DEPTH_M;
    let mut body = vec![u32::MAX; w.cells()];
    let mut lakes = Vec::new();
    for start in 0..w.cells() {
        if body[start] != u32::MAX || !is_lake(start) {
            continue;
        }
        let id = lakes.len() as u32;
        let (mut stack, mut cells) = (vec![start], Vec::new());
        body[start] = id;
        while let Some(i) = stack.pop() {
            cells.push(i);
            let mut visit = |j: usize| {
                if body[j] == u32::MAX && is_lake(j) {
                    body[j] = id;
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
        if cells.len() < LAKE_MIN_CELLS {
            for &i in &cells {
                body[i] = u32::MAX - 1; // too small: not a lake, but visited
            }
            continue;
        }
        // Outlet: the lake cell draining out of the body that carries the most water.
        let outlet = cells
            .iter()
            .copied()
            .filter(|&i| body[routing.receiver[i] as usize] != id)
            .max_by(|&a, &b| q[a].total_cmp(&q[b]))
            .unwrap_or(cells[0]);
        lakes.push(Lake {
            level: cells.iter().map(|&i| level[i]).fold(f64::MIN, f64::max),
            area_m2: cells.len() as f64 * w.dx * w.dx,
            max_depth: cells.iter().map(|&i| lake_depth[i]).fold(0.0, f64::max),
            outlet: w.center(outlet),
        });
    }
    lakes
}

/// Separate landmasses (4-connected) of at least MIN_ISLAND_KM2: (count, largest km^2).
fn find_islands(w: &World) -> (usize, f64) {
    let mut seen = w.ocean.clone();
    let (mut count, mut largest) = (0usize, 0usize);
    let min_cells = (MIN_ISLAND_KM2 * 1.0e6 / (w.dx * w.dx)).ceil() as usize;
    for start in 0..w.cells() {
        if seen[start] {
            continue;
        }
        let (mut stack, mut size) = (vec![start], 0usize);
        seen[start] = true;
        while let Some(i) = stack.pop() {
            size += 1;
            let mut visit = |j: usize| {
                if !seen[j] {
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
        if size >= min_cells {
            count += 1;
            largest = largest.max(size);
        }
    }
    (count, largest as f64 * w.dx * w.dx / 1.0e6)
}

fn chaikin(points: &[RiverPoint]) -> Vec<RiverPoint> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mix = |a: &RiverPoint, b: &RiverPoint, t: f64| RiverPoint {
        x: a.x + (b.x - a.x) * t,
        z: a.z + (b.z - a.z) * t,
        bed: a.bed + (b.bed - a.bed) * t,
        water: a.water + (b.water - a.water) * t,
        in_lake: a.in_lake + (b.in_lake - a.in_lake) * t,
    };
    let mut out = vec![points[0]];
    for pair in points.windows(2) {
        out.push(mix(&pair[0], &pair[1], 0.25));
        out.push(mix(&pair[0], &pair[1], 0.75));
    }
    out.push(*points.last().unwrap());
    out
}

/// An x difference `dx` (m) on a world `width` m round, taken the shorter
/// way: folded into (-width / 2, width / 2].
pub fn seam_dx(dx: f64, width: f64) -> f64 {
    let d = dx.rem_euclid(width);
    if d > 0.5 * width { d - width } else { d }
}

/// Reaches between network nodes, with Strahler order. Returns (rivers, max order).
fn find_rivers(
    w: &World,
    routing: &Routing,
    q: &[f64],
    lake_depth: &[f64],
    k: &Knobs,
) -> (Vec<River>, u32) {
    let cells = w.cells();
    // Outlets (the sea) end a river: they have no receiver.
    let channel = |i: usize| !w.outlet[i] && q[i] >= k.river_min_discharge_m2;
    let mut donors = vec![0u8; cells];
    let (mut order, mut max_in, mut max_count) =
        (vec![0u32; cells], vec![0u32; cells], vec![0u8; cells]);
    // Upstream-first: every donor of a cell is handled before the cell itself.
    for &i in routing.order.iter().rev() {
        let i = i as usize;
        if !channel(i) {
            continue;
        }
        order[i] = match max_count[i] {
            0 => 1,
            1 => max_in[i],
            _ => max_in[i] + 1,
        };
        let rc = routing.receiver[i] as usize;
        if channel(rc) {
            donors[rc] += 1;
            if order[i] > max_in[rc] {
                max_in[rc] = order[i];
                max_count[rc] = 1;
            } else if order[i] == max_in[rc] {
                max_count[rc] += 1;
            }
        }
    }

    let point = |i: usize| {
        let (x, z) = w.center(i);
        let lake = lake_depth[i] >= LAKE_MIN_DEPTH_M;
        RiverPoint {
            x,
            z,
            bed: w.h[i],
            water: routing.filled[i],
            in_lake: lake as u8 as f64,
        }
    };
    let mut rivers = Vec::new();
    let mut max_order = 0;
    for &start in &routing.order {
        let start = start as usize;
        if !channel(start) || donors[start] == 1 {
            continue; // not a node: mid-reach
        }
        let mut points = vec![point(start)];
        let mut last = start;
        let mut cur = routing.receiver[start] as usize;
        while channel(cur) && donors[cur] == 1 {
            points.push(point(cur));
            last = cur;
            cur = routing.receiver[cur] as usize;
        }
        // Close the reach on its downstream node: the confluence, or the sea
        // (every outlet on the Planet).
        if channel(cur) {
            points.push(point(cur));
        } else if w.ocean[cur] {
            let (x, z) = w.center(cur);
            points.push(RiverPoint {
                x,
                z,
                bed: 0.0,
                water: 0.0,
                in_lake: 0.0,
            });
        } else if w.outlet[cur] {
            let (x, z) = w.center(cur);
            points.push(RiverPoint {
                x,
                z,
                bed: w.h[cur],
                water: w.h[cur],
                in_lake: 0.0,
            });
        }
        // On a wrapping world a reach may step across the seam (x = width
        // beside x = 0): smooth it unwrapped, then fold every point back, so
        // stored points stay in [0, width) and a seam-crossing pair is read
        // the shorter way round (`seam_dx`).
        let width = w.nx as f64 * w.dx;
        if w.wrap_x {
            for n in 1..points.len() {
                points[n].x = points[n - 1].x + seam_dx(points[n].x - points[n - 1].x, width);
            }
        }
        for _ in 0..RIVER_SMOOTHING_PASSES {
            points = chaikin(&points);
        }
        if w.wrap_x {
            for p in &mut points {
                p.x = p.x.rem_euclid(width);
            }
        }
        max_order = max_order.max(order[start]);
        rivers.push(River {
            order: order[start],
            discharge_m2: q[last],
            width_m: (k.river_width_m * (q[last] / 1.0e6).sqrt()).max(RIVER_MIN_WIDTH_M),
            points,
        });
    }
    (rivers, max_order)
}

pub fn analyze(w: &World, geo: &Geology, rain: &[f64], k: &Knobs) -> Analysis {
    let routing = route(w, FLOOD_EPSILON);
    let q = discharge(w, &routing, rain);
    // Lakes: a flood WITHOUT epsilon fills exactly the closed depressions.
    let level = route(w, 0.0).filled;
    let lake_depth: Vec<f64> = (0..w.cells()).map(|i| level[i] - w.h[i]).collect();
    let land: Vec<usize> = routing.order.iter().map(|&i| i as usize).collect();
    let surface_rock: Vec<u8> = (0..w.cells())
        .map(|i| {
            if w.ocean[i] {
                0
            } else {
                geo.surface_rock(w, i)
            }
        })
        .collect();

    let slope_of = |i: usize| {
        let rc = routing.receiver[i] as usize;
        (w.h[i] - w.h[rc].max(0.0)).max(0.0) / routing.receiver_dist[i]
    };
    let mut heights: Vec<f64> = land.iter().map(|&i| w.h[i]).collect();
    heights.sort_by(f64::total_cmp);
    let mut slopes: Vec<f64> = land.iter().map(|&i| slope_of(i)).collect();
    slopes.sort_by(f64::total_cmp);

    let lakes = find_lakes(w, &routing, &q, &lake_depth, &level);
    let (rivers, max_order) = find_rivers(w, &routing, &q, &lake_depth, k);

    // Flow-path length to the divide, for Hack's law and the longest river.
    let mut length = vec![0.0f64; w.cells()];
    for &i in routing.order.iter().rev() {
        let (i, rc) = (i as usize, routing.receiver[i as usize] as usize);
        length[rc] = length[rc].max(length[i] + routing.receiver_dist[i]);
    }
    let (mut mouths, mut longest, mut biggest) = (0, 0.0f64, 0.0f64);
    for &i in &land {
        if w.outlet[routing.receiver[i] as usize] && q[i] >= k.river_min_discharge_m2 {
            mouths += 1;
            longest = longest.max(length[i]);
            biggest = biggest.max(q[i]);
        }
    }
    let width = w.nx as f64 * w.dx;
    let along_x = |a: f64, b: f64| {
        if w.wrap_x {
            seam_dx(b - a, width)
        } else {
            b - a
        }
    };
    let channel_m: f64 = rivers
        .iter()
        .map(|r| {
            r.points
                .windows(2)
                .map(|p| (along_x(p[0].x, p[1].x).powi(2) + (p[1].z - p[0].z).powi(2)).sqrt())
                .sum::<f64>()
        })
        .sum();

    // Flint's law S ~ Q^-theta (theta = m/n = 0.5 at steady state in uniform
    // rock) and Hack's law L ~ A^h, over the fluvial range.
    let fluvial: Vec<usize> = land
        .iter()
        .copied()
        .filter(|&i| q[i] >= 1.0e5 && q[i] <= 3.0e7 && lake_depth[i] < LAKE_MIN_DEPTH_M)
        .collect();
    let slope_area: Vec<(f64, f64)> = fluvial
        .iter()
        .filter_map(|&i| {
            let s = slope_of(i);
            (s > 1.0e-6).then(|| (q[i].ln(), s.ln()))
        })
        .collect();
    let hack: Vec<(f64, f64)> = fluvial
        .iter()
        .filter(|&&i| length[i] > 0.0)
        .map(|&i| (q[i].ln(), length[i].ln()))
        .collect();
    let fits = slope_area.len() > 100 && hack.len() > 100;

    let cell_km2 = w.dx * w.dx / 1.0e6;
    let buildable = land
        .iter()
        .filter(|&&i| {
            slope_of(i) < BUILDABLE_MAX_SLOPE
                && (BUILDABLE_HEIGHT_M.0..=BUILDABLE_HEIGHT_M.1).contains(&w.h[i])
                && lake_depth[i] < LAKE_MIN_DEPTH_M
        })
        .count();
    let mut rock_shares = [0.0; 6];
    for &i in &land {
        rock_shares[surface_rock[i] as usize] += 1.0 / land.len() as f64;
    }

    let (islands, largest_island_km2) = find_islands(w);
    let stats = Stats {
        islands,
        largest_island_km2,
        land_share: land.len() as f64 / w.cells() as f64,
        land_km2: land.len() as f64 * cell_km2,
        heights: [0.1, 0.5, 0.9, 0.99, 1.0].map(|p| percentile(&heights, p)),
        slopes: [0.5, 0.9, 0.99, 1.0].map(|p| percentile(&slopes, p)),
        lake_count: lakes.len(),
        largest_lake_km2: lakes.iter().map(|l| l.area_m2).fold(0.0, f64::max) / 1.0e6,
        depression_km3: land
            .iter()
            .map(|&i| lake_depth[i].max(0.0) * w.dx * w.dx)
            .sum::<f64>()
            / 1.0e9,
        mouths,
        channel_km: channel_m / 1000.0,
        longest_river_km: longest / 1000.0,
        biggest_basin_km2: biggest / 1.0e6,
        max_order,
        theta: if fits {
            -regress(&slope_area)
        } else {
            f64::NAN
        },
        hack: if fits { regress(&hack) } else { f64::NAN },
        buildable_km2: buildable as f64 * cell_km2,
        rock_shares,
        margin_drowned: w.margin_drowned,
        river_min_discharge_m2: k.river_min_discharge_m2,
    };
    Analysis {
        routing,
        q,
        lake_depth,
        surface_rock,
        lakes,
        rivers,
        stats,
    }
}

pub fn print_report(label: &str, dx: f64, s: &Stats) {
    println!("--- {label} ---");
    println!(
        "land {:.1}% of the world ({:.1} km^2) in {} island{} (largest {:.1} km^2)",
        100.0 * s.land_share,
        s.land_km2,
        s.islands,
        if s.islands == 1 { "" } else { "s" },
        s.largest_island_km2
    );
    println!(
        "land height above sea (m): p10 {:.0} p50 {:.0} p90 {:.0} p99 {:.0} max {:.0}",
        s.heights[0], s.heights[1], s.heights[2], s.heights[3], s.heights[4]
    );
    println!(
        "downstream slope (rise/run at {dx:.0} m): p50 {:.3} p90 {:.3} p99 {:.3} max {:.3}",
        s.slopes[0], s.slopes[1], s.slopes[2], s.slopes[3]
    );
    println!(
        "lakes: {} (largest {:.3} km^2), {:.4} km^3 of standing water; buildable land {:.1} km^2",
        s.lake_count, s.largest_lake_km2, s.depression_km3, s.buildable_km2
    );
    println!(
        "rivers (>= {:.1} km^2 of rain drained): {} mouths, {:.1} km of channel, longest {:.1} km, \
         biggest basin {:.1} km^2, Strahler order up to {}",
        s.river_min_discharge_m2 / 1.0e6,
        s.mouths,
        s.channel_km,
        s.longest_river_km,
        s.biggest_basin_km2,
        s.max_order
    );
    println!(
        "slope-discharge concavity theta = {:.2} (uniform-rock steady state 0.50); Hack exponent = {:.2} \
         (nature ~0.5-0.6)",
        s.theta, s.hack
    );
    let rocks: Vec<String> = ROCKS
        .iter()
        .zip(s.rock_shares)
        .filter(|(_, share)| *share > 0.0005)
        .map(|(rock, share)| format!("{} {:.0}%", rock.name, 100.0 * share))
        .collect();
    println!("surface rock: {}", rocks.join(", "));
}

/// The river/lake objects as JSON — a first sketch of that part of the world
/// artifact. Hand-written so the prototype needs no extra dependency.
pub fn objects_json(w: &World, a: &Analysis) -> String {
    let mut s = String::new();
    let _ = write!(
        s,
        "{{\n  \"cell_m\": {}, \"width_m\": {}, \"height_m\": {},\n  \"lakes\": [",
        w.dx,
        w.dx * w.nx as f64,
        w.dx * w.nz as f64
    );
    for (k, l) in a.lakes.iter().enumerate() {
        let _ = write!(
            s,
            "{}\n    {{\"level\": {:.2}, \"area_m2\": {:.0}, \"max_depth\": {:.2}, \"outlet\": [{:.1}, {:.1}]}}",
            if k == 0 { "" } else { "," },
            l.level,
            l.area_m2,
            l.max_depth,
            l.outlet.0,
            l.outlet.1
        );
    }
    s.push_str("\n  ],\n  \"rivers\": [");
    for (k, r) in a.rivers.iter().enumerate() {
        let _ = write!(
            s,
            "{}\n    {{\"order\": {}, \"discharge_m2\": {:.0}, \"width_m\": {:.1}, \"points\": [",
            if k == 0 { "" } else { "," },
            r.order,
            r.discharge_m2,
            r.width_m
        );
        for (j, p) in r.points.iter().enumerate() {
            let _ = write!(
                s,
                "{}[{:.1}, {:.1}, {:.2}, {:.2}]",
                if j == 0 { "" } else { ", " },
                p.x,
                p.z,
                p.bed,
                p.water
            );
        }
        s.push_str("]}");
    }
    s.push_str("\n  ]\n}\n");
    s
}
