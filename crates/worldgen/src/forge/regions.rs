//! Named regions of a forged world, DF style: every cell's biome
//! (`game::classify`) maps to a region type (`game::region_type`), and a
//! region is a 4-connected group of cells of one type, with an id, a size and
//! a good / neutral / evil alignment. Savagery is a separate per-cell fractal.
//! None of it has a consumer yet: it is stored for creatures and
//! civilisations (spec §5, docs/superpowers/specs/2026-09-20-df-worldgen-research/
//! 01-pipeline-and-fields.md §3.4 and step 15).
//!
//! **No confetti** (spec §10, DF's `SUBREGION_MAX`): a region smaller than
//! `MIN_REGION_AREA_M2` merges into its largest neighbouring region (its
//! cells take that region's type; the biome raster keeps their own biome).
//! Merges never cross a frozen / thawed line (DF splits tundra and glacier
//! from everything else) nor a water / land line (DF's lakes never merge into
//! land). Region ids are `u16`: if a world still holds more than
//! `MAX_REGIONS`, the merge size doubles (and may cross those lines) until it
//! fits.

use game::{classify, region_type, Biome, RegionType};
use serde::{Deserialize, Serialize};

use crate::forge::par;
use crate::forge::world::hash01;
use crate::forged::WorldArtifact;
use crate::forge::periodic::PeriodicX;
use crate::noise::OctaveNoise;

/// Edge of one DF world tile (m): 16 × 16 embarks of 48 m. DF Medium
/// (129 tiles) is ~100 km, our default world. DF's region rules count in
/// these tiles; we convert with the forge cell size.
pub const DF_TILE_M: f64 = 768.0;
/// Regions under this area merge into a neighbour: one DF world tile
/// (0.59 km², a 768 m square) — DF never names anything smaller; a patch
/// under it is local variation inside its neighbour. Measured on the 100 km /
/// 64 m default continent (seed default): 15,000 raw components → 282
/// regions at one tile (583 at a quarter tile, 133 at four tiles, which
/// also folds all but one mountain range into the lowlands). DF Medium (our
/// size) has a few hundred; DF rejects worlds over 2,750 (`SUBREGION_MAX`).
pub const MIN_REGION_AREA_M2: f64 = DF_TILE_M * DF_TILE_M;
/// Region ids are `u16`.
pub const MAX_REGIONS: usize = u16::MAX as usize + 1;
/// DF's region size buckets for good / evil, in DF tiles: small up to this …
pub const SMALL_REGION_MAX_TILES: f64 = 24.0;
/// … large from this, medium between.
pub const LARGE_REGION_MIN_TILES: f64 = 100.0;
/// A bucket whose goal no region fits under still gets its smallest region
/// (DF's rule), unless that region is over this many times the goal: on a
/// small world one region can hold most of the land, and forcing it would
/// turn most of the world good or evil.
pub const FORCED_REGION_MAX_GOALS: f64 = 3.0;
/// Wavelength of savagery's first octave (m).
pub const SAVAGERY_WAVELENGTH_M: f64 = 12_000.0;
/// Savagery octaves (each half the wavelength and weight of the last).
pub const SAVAGERY_OCTAVES: usize = 4;
/// Savagery = 50 + 50 * gain * noise, clamped: at this gain the calm,
/// neutral and savage thirds each cover roughly a third of the world.
pub const SAVAGERY_GAIN: f64 = 2.6;

/// One region: `id` is its index in the world file's `regions` and its value
/// in the `region` raster.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegionRecord {
    pub id: u16,
    pub kind: RegionType,
    pub cells: u32,
    /// -1 evil, 0 neutral, 1 good.
    pub alignment: i8,
}

/// Merge classes: a merge stays inside one (unless forced by the `u16` cap).
fn merge_class(kind: RegionType) -> u8 {
    if kind.is_water() {
        0
    } else if kind.is_frozen() {
        1
    } else {
        2
    }
}

/// Smallest region size in cells at `cell_m` (regions with fewer merge).
pub fn min_region_cells(cell_m: f64) -> u32 {
    (MIN_REGION_AREA_M2 / (cell_m * cell_m)).ceil().max(1.0) as u32
}

/// Regions of an `nx` × `nz` world from its biome (`Biome` as `u8`) and
/// drainage rasters: per-cell region ids and the regions, ids dense from 0
/// in raster order of each region's first cell. `wrap_x`: columns 0 and
/// `nx - 1` are neighbours (a world that wraps east-west), so a region may
/// run across that seam.
pub fn flood(biome: &[u8], drainage: &[u8], nx: usize, nz: usize, cell_m: f64, wrap_x: bool) -> (Vec<u16>, Vec<RegionRecord>) {
    flood_with(biome, drainage, (nx, nz), wrap_x, min_region_cells(cell_m), MAX_REGIONS)
}

/// `flood` with the merge size (cells) and the region cap as arguments.
pub fn flood_with(
    biome: &[u8],
    drainage: &[u8],
    (nx, nz): (usize, usize),
    wrap_x: bool,
    min_cells: u32,
    max_regions: usize,
) -> (Vec<u16>, Vec<RegionRecord>) {
    assert_eq!(biome.len(), nx * nz);
    assert_eq!(drainage.len(), nx * nz);
    assert!(max_regions >= 1 && max_regions <= MAX_REGIONS);
    let kind: Vec<RegionType> = par::map_range(nx * nz, |i| {
        region_type(Biome::from_u8(biome[i]).unwrap_or(Biome::Ocean), drainage[i])
    });
    let mut f = Flood::components(&kind, nx, nz, wrap_x);
    let mut count = f.merge(nx, nz, wrap_x, min_cells, false);
    let mut threshold = min_cells.max(1);
    while count > max_regions {
        count = f.merge(nx, nz, wrap_x, threshold, true);
        if count <= max_regions {
            break;
        }
        threshold = threshold.saturating_mul(2);
        count = f.merge(nx, nz, wrap_x, threshold, false);
    }
    f.finish()
}

/// Union-find over the 4-connected components of one region type.
struct Flood {
    /// Component per cell.
    label: Vec<u32>,
    parent: Vec<u32>,
    size: Vec<u32>,
    kind: Vec<RegionType>,
}

impl Flood {
    fn components(kind: &[RegionType], nx: usize, nz: usize, wrap_x: bool) -> Flood {
        let mut label = vec![u32::MAX; nx * nz];
        let (mut size, mut kinds) = (Vec::new(), Vec::new());
        let mut stack = Vec::new();
        for start in 0..nx * nz {
            if label[start] != u32::MAX {
                continue;
            }
            let id = size.len() as u32;
            let k = kind[start];
            let mut cells = 0u32;
            label[start] = id;
            stack.push(start);
            while let Some(i) = stack.pop() {
                cells += 1;
                let (x, z) = (i % nx, i / nx);
                let mut visit = |j: usize| {
                    if label[j] == u32::MAX && kind[j] == k {
                        label[j] = id;
                        stack.push(j);
                    }
                };
                // x wraps round on a wrapping world.
                if x > 0 {
                    visit(i - 1);
                } else if wrap_x {
                    visit(i + nx - 1);
                }
                if x + 1 < nx {
                    visit(i + 1);
                } else if wrap_x {
                    visit(i + 1 - nx);
                }
                if z > 0 {
                    visit(i - nx);
                }
                if z + 1 < nz {
                    visit(i + nx);
                }
            }
            size.push(cells);
            kinds.push(k);
        }
        let parent = (0..size.len() as u32).collect();
        Flood { label, parent, size, kind: kinds }
    }

    fn find(&mut self, mut a: u32) -> u32 {
        while self.parent[a as usize] != a {
            let up = self.parent[self.parent[a as usize] as usize];
            self.parent[a as usize] = up;
            a = up;
        }
        a
    }

    fn roots(&mut self) -> usize {
        (0..self.parent.len() as u32).filter(|&a| self.parent[a as usize] == a).count()
    }

    /// Merges every region under `min_cells` into its largest neighbour
    /// (same merge class unless `cross_class`), pass after pass until none
    /// can merge. Single-threaded and ordered by id: the result depends only
    /// on the rasters. Returns the region count.
    fn merge(&mut self, nx: usize, nz: usize, wrap_x: bool, min_cells: u32, cross_class: bool) -> usize {
        loop {
            // Root of every cell, then the (small root, neighbour root) pairs.
            let roots: Vec<u32> = (0..nx * nz).map(|i| self.find(self.label[i])).collect();
            let small = |f: &Flood, r: u32| f.size[r as usize] < min_cells;
            let mut pairs: Vec<(u32, u32)> = Vec::new();
            for i in 0..nx * nz {
                let a = roots[i];
                let (x, z) = (i % nx, i / nx);
                let mut edge = |b: u32| {
                    if a != b {
                        if small(self, a) {
                            pairs.push((a, b));
                        }
                        if small(self, b) {
                            pairs.push((b, a));
                        }
                    }
                };
                if x + 1 < nx {
                    edge(roots[i + 1]);
                } else if wrap_x {
                    edge(roots[i + 1 - nx]);
                }
                if z + 1 < nz {
                    edge(roots[i + nx]);
                }
            }
            pairs.sort_unstable();
            pairs.dedup();
            // Every small region picks, by the sizes at the start of the
            // pass, its largest eligible neighbour, and merges only if that
            // neighbour ranks above it (size, then lower id): links point
            // strictly up the ranking, so no cycles, and a pass cannot
            // snowball one growing region through a field of small ones.
            let rank = |f: &Flood, r: u32| (f.size[r as usize], std::cmp::Reverse(r));
            let mut links: Vec<(u32, u32)> = Vec::new();
            let mut k = 0;
            while k < pairs.len() {
                let s = pairs[k].0;
                let class = merge_class(self.kind[s as usize]);
                let mut best: Option<u32> = None;
                while k < pairs.len() && pairs[k].0 == s {
                    let b = pairs[k].1;
                    k += 1;
                    if !cross_class && merge_class(self.kind[b as usize]) != class {
                        continue;
                    }
                    if best.is_none_or(|c| rank(self, b) > rank(self, c)) {
                        best = Some(b);
                    }
                }
                if let Some(t) = best.filter(|&t| rank(self, t) > rank(self, s)) {
                    links.push((s, t));
                }
            }
            let merged = links.len();
            for (s, t) in links {
                let t = self.find(t);
                debug_assert_ne!(t, s, "links point up the ranking");
                self.parent[s as usize] = t;
                self.size[t as usize] += self.size[s as usize];
            }
            if merged == 0 {
                return self.roots();
            }
        }
    }

    /// Dense `u16` ids in raster order of each region's first cell.
    fn finish(mut self) -> (Vec<u16>, Vec<RegionRecord>) {
        let mut id_of = vec![u32::MAX; self.parent.len()];
        let mut records: Vec<RegionRecord> = Vec::new();
        let mut out = vec![0u16; self.label.len()];
        for i in 0..self.label.len() {
            let r = self.find(self.label[i]) as usize;
            if id_of[r] == u32::MAX {
                id_of[r] = records.len() as u32;
                let id = u16::try_from(records.len()).expect("merge kept the region count within u16");
                records.push(RegionRecord { id, kind: self.kind[r], cells: self.size[r], alignment: 0 });
            }
            out[i] = id_of[r] as u16;
        }
        (out, records)
    }
}

/// DF size bucket of a region (0 small, 1 medium, 2 large) at `cell_m`.
fn size_bucket(cells: u32, cell_m: f64) -> usize {
    let tiles = cells as f64 * cell_m * cell_m / (DF_TILE_M * DF_TILE_M);
    if tiles <= SMALL_REGION_MAX_TILES {
        0
    } else if tiles < LARGE_REGION_MIN_TILES {
        1
    } else {
        2
    }
}

/// Good and evil by DF's rule: per size bucket (small / medium / large, in
/// DF tiles at `cell_m`), whole regions in a seeded order turn good until
/// `good_share` of the bucket's cells is reached, then others evil until
/// `evil_share`. A region that would overshoot its goal is skipped; a bucket
/// with a non-zero goal gets at least its smallest region (DF: "any non-zero
/// goal forces at least one whole region of that size class") unless that
/// region is over `FORCED_REGION_MAX_GOALS` goals. The sea stays
/// neutral and out of the shares — it is one region the size of the ocean.
pub fn assign_alignment(regions: &mut [RegionRecord], seed: u64, good_share: f64, evil_share: f64, cell_m: f64) {
    for r in regions.iter_mut() {
        r.alignment = 0;
    }
    for bucket in 0..3 {
        let mut members: Vec<usize> = (0..regions.len())
            .filter(|&i| regions[i].kind != RegionType::Ocean && size_bucket(regions[i].cells, cell_m) == bucket)
            .collect();
        if members.is_empty() {
            continue;
        }
        let total: f64 = members.iter().map(|&i| regions[i].cells as f64).sum();
        members.sort_by(|&a, &b| {
            hash01(seed, 0xA11C + bucket as u64, a as u64).total_cmp(&hash01(seed, 0xA11C + bucket as u64, b as u64)).then(a.cmp(&b))
        });
        for (alignment, share) in [(1i8, good_share), (-1i8, evil_share)] {
            let goal = share.max(0.0) * total;
            if goal <= 0.0 {
                continue;
            }
            let mut got = 0.0;
            for &i in &members {
                let cells = regions[i].cells as f64;
                if regions[i].alignment == 0 && got + cells <= goal {
                    regions[i].alignment = alignment;
                    got += cells;
                }
            }
            if got == 0.0 {
                let forced = members.iter().filter(|&&i| regions[i].alignment == 0).min_by_key(|&&i| (regions[i].cells, i));
                if let Some(&i) = forced.filter(|&&i| regions[i].cells as f64 <= FORCED_REGION_MAX_GOALS * goal) {
                    regions[i].alignment = alignment;
                }
            }
        }
    }
}

/// Savagery 0..100 per cell of an `nx` × `nz` grid of `cell_m` cells:
/// fractal noise, first octave `SAVAGERY_WAVELENGTH_M`; periodic in x with
/// `wrap` (a planet).
pub fn savagery(seed: u64, nx: usize, nz: usize, cell_m: f64, wrap: Option<PeriodicX>) -> Vec<u8> {
    let noise = OctaveNoise::new(seed ^ 0x5A7A_6E27, -13, vec![1.0; SAVAGERY_OCTAVES]);
    // Octave -13 is 8,192 m; stretch the coordinates onto the wavelength.
    let scale = 8_192.0 / SAVAGERY_WAVELENGTH_M;
    let wrap = wrap.map(|w| w.for_noise(SAVAGERY_WAVELENGTH_M));
    par::map_range(nx * nz, |i| {
        let value = match &wrap {
            Some(wrap) => {
                let (mx, mz) = (((i % nx) as f64 + 0.5) * cell_m, ((i / nx) as f64 + 0.5) * cell_m);
                wrap.sample(mx, mz, |x, z| noise.sample2(x * scale, z * scale))
            }
            None => {
                let (x, z) = (((i % nx) as f64 + 0.5) * cell_m * scale, ((i / nx) as f64 + 0.5) * cell_m * scale);
                noise.sample2(x, z)
            }
        };
        (50.0 + 50.0 * SAVAGERY_GAIN * value).round().clamp(0.0, 100.0) as u8
    })
}

/// Fills `a`'s biome, region, regions and savagery from its own stored
/// fields, so the biome raster is exactly `classify` of what a reader sees.
pub fn derive(a: &mut WorldArtifact, good_share: f64, evil_share: f64) {
    let (nx, nz) = (a.nx as usize, a.nz as usize);
    let mut lake = vec![false; nx * nz];
    for l in &a.lakes {
        for &(start, len) in &l.cells {
            for c in &mut lake[start as usize..(start + len) as usize] {
                *c = true;
            }
        }
    }
    let biome = par::map_range(nx * nz, |i| classify(&a.cell_climate_at(i, lake[i])) as u8);
    let wrap_x = a.edges[1] == crate::forge::world::EdgeKind::Wrap;
    let (region, mut regions) = flood(&biome, &a.drainage, nx, nz, a.cell_m, wrap_x);
    assign_alignment(&mut regions, a.seed, good_share, evil_share, a.cell_m);
    let wrap = PeriodicX::for_edges(&a.edges, nx as f64 * a.cell_m);
    a.savagery = savagery(a.seed, nx, nz, a.cell_m, wrap);
    a.biome = biome;
    a.region = region;
    a.regions = regions;
}

/// Share of cells per biome (land and water), largest first.
pub fn biome_shares(biome: &[u8]) -> Vec<(Biome, f64)> {
    let mut count = [0usize; Biome::ALL.len()];
    for &b in biome {
        count[(b as usize).min(Biome::ALL.len() - 1)] += 1;
    }
    let total = biome.len().max(1) as f64;
    let mut out: Vec<(Biome, f64)> =
        Biome::ALL.iter().filter(|b| count[**b as usize] > 0).map(|&b| (b, count[b as usize] as f64 / total)).collect();
    out.sort_by(|p, q| q.1.total_cmp(&p.1));
    out
}

/// Region count per type, in `RegionType::ALL` order (types with none left out).
pub fn region_counts(regions: &[RegionRecord]) -> Vec<(RegionType, usize)> {
    RegionType::ALL
        .iter()
        .map(|&t| (t, regions.iter().filter(|r| r.kind == t).count()))
        .filter(|&(_, c)| c > 0)
        .collect()
}

