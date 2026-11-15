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
/// in raster order of each region's first cell.
pub fn flood(biome: &[u8], drainage: &[u8], nx: usize, nz: usize, cell_m: f64) -> (Vec<u16>, Vec<RegionRecord>) {
    flood_with(biome, drainage, (nx, nz), min_region_cells(cell_m), MAX_REGIONS)
}

/// `flood` with the merge size (cells) and the region cap as arguments.
pub fn flood_with(
    biome: &[u8],
    drainage: &[u8],
    (nx, nz): (usize, usize),
    min_cells: u32,
    max_regions: usize,
) -> (Vec<u16>, Vec<RegionRecord>) {
    assert_eq!(biome.len(), nx * nz);
    assert_eq!(drainage.len(), nx * nz);
    assert!(max_regions >= 1 && max_regions <= MAX_REGIONS);
    let kind: Vec<RegionType> = par::map_range(nx * nz, |i| {
        region_type(Biome::from_u8(biome[i]).unwrap_or(Biome::Ocean), drainage[i])
    });
    let mut f = Flood::components(&kind, nx, nz);
    let mut count = f.merge(nx, nz, min_cells, false);
    let mut threshold = min_cells.max(1);
    while count > max_regions {
        count = f.merge(nx, nz, threshold, true);
        if count <= max_regions {
            break;
        }
        threshold = threshold.saturating_mul(2);
        count = f.merge(nx, nz, threshold, false);
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
    fn components(kind: &[RegionType], nx: usize, nz: usize) -> Flood {
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
                if x > 0 {
                    visit(i - 1);
                }
                if x + 1 < nx {
                    visit(i + 1);
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
    fn merge(&mut self, nx: usize, nz: usize, min_cells: u32, cross_class: bool) -> usize {
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
/// fractal noise, first octave `SAVAGERY_WAVELENGTH_M`.
pub fn savagery(seed: u64, nx: usize, nz: usize, cell_m: f64) -> Vec<u8> {
    let noise = OctaveNoise::new(seed ^ 0x5A7A_6E27, -13, vec![1.0; SAVAGERY_OCTAVES]);
    // Octave -13 is 8,192 m; stretch the coordinates onto the wavelength.
    let scale = 8_192.0 / SAVAGERY_WAVELENGTH_M;
    par::map_range(nx * nz, |i| {
        let (x, z) = (((i % nx) as f64 + 0.5) * cell_m * scale, ((i / nx) as f64 + 0.5) * cell_m * scale);
        (50.0 + 50.0 * SAVAGERY_GAIN * noise.sample2(x, z)).round().clamp(0.0, 100.0) as u8
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
    let (region, mut regions) = flood(&biome, &a.drainage, nx, nz, a.cell_m);
    assign_alignment(&mut regions, a.seed, good_share, evil_share, a.cell_m);
    a.savagery = savagery(a.seed, nx, nz, a.cell_m);
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

#[cfg(test)]
mod tests {
    use super::*;

    const FOREST_DRAINAGE: u8 = 50;

    fn raster(rows: &[&str]) -> (Vec<u8>, usize) {
        let n = rows.len();
        let mut out = Vec::with_capacity(n * n);
        for row in rows {
            assert_eq!(row.len(), n);
            for ch in row.chars() {
                out.push(match ch {
                    'T' => Biome::Taiga,
                    'J' => Biome::TropicalBroadleafForest,
                    'B' => Biome::BroadleafForest,
                    'U' => Biome::Tundra,
                    'G' => Biome::Glacier,
                    'g' => Biome::Grassland,
                    '~' => Biome::Ocean,
                    'L' => Biome::Lake,
                    _ => panic!("{ch}"),
                } as u8);
            }
        }
        (out, n)
    }

    /// Kind of the region holding cell (x, z).
    fn kind_at(region: &[u16], regions: &[RegionRecord], n: usize, x: usize, z: usize) -> RegionType {
        regions[region[z * n + x] as usize].kind
    }

    #[test]
    fn flood_splits_frozen_but_joins_forests() {
        let (b, n) = raster(&["TTJJ", "TTJB", "UUGG", "UUGG"]);
        let (region, regions) = flood_with(&b, &vec![FOREST_DRAINAGE; n * n], (n, n), 1, MAX_REGIONS);
        // Taiga, jungle and broadleaf: one forest; tundra and glacier apart.
        assert_eq!(regions.len(), 3, "{regions:?}");
        assert_eq!(region[0], region[3]);
        assert_eq!(region[0], region[7]);
        assert_ne!(region[8], region[10]);
        assert_eq!(kind_at(&region, &regions, n, 0, 0), RegionType::Forest);
        assert_eq!(kind_at(&region, &regions, n, 0, 3), RegionType::Tundra);
        assert_eq!(kind_at(&region, &regions, n, 3, 3), RegionType::Glacier);
        for r in &regions {
            assert_eq!(r.cells as usize, region.iter().filter(|&&id| id == r.id).count());
        }
        assert_eq!(regions.iter().map(|r| r.id).collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    #[test]
    fn small_regions_merge_into_their_largest_neighbour() {
        // A 1-cell grassland in a forest, a 2-cell grassland between the
        // forest and the sea, a 1-cell tundra (frozen: stays) and a 1-cell
        // lake beside it (water: stays).
        let (b, n) = raster(&[
            "BBBBBB", //
            "BgBBBB", //
            "BBBLUB", //
            "BBBBBB", //
            "ggBBBB", //
            "~~~~~~",
        ]);
        let d = vec![FOREST_DRAINAGE; n * n];
        let (unmerged, _) = flood_with(&b, &d, (n, n), 1, MAX_REGIONS);
        let (region, regions) = flood_with(&b, &d, (n, n), 3, MAX_REGIONS);
        assert!(regions.len() < unmerged.iter().max().map_or(0, |&m| m as usize + 1));
        assert_eq!(kind_at(&region, &regions, n, 1, 1), RegionType::Forest, "lone grass cell joins the forest");
        assert_eq!(region[n + 1], region[0]);
        assert_eq!(kind_at(&region, &regions, n, 0, 4), RegionType::Forest, "the larger neighbour, land with land");
        assert_eq!(kind_at(&region, &regions, n, 4, 2), RegionType::Tundra, "frozen never joins thawed");
        assert_eq!(kind_at(&region, &regions, n, 3, 2), RegionType::Lake, "water never joins land");
        assert_eq!(kind_at(&region, &regions, n, 0, 5), RegionType::Ocean);
        assert_eq!(regions.len(), 4, "{regions:?}");
        assert_eq!(regions.iter().map(|r| r.cells as usize).sum::<usize>(), n * n);
    }

    #[test]
    fn region_ids_fit_u16() {
        // 300 x 300 checkerboard of land and sea: 90,000 one-cell regions,
        // none of which may merge within its class, and none small at 4 km
        // cells. The cap forces cross-class merges (in a field of nothing but
        // small regions they may collapse very far: that is the fallback's
        // price, and no forged world comes near it).
        let n = 300;
        let b: Vec<u8> = (0..n * n)
            .map(|i| if (i % n + i / n) % 2 == 0 { Biome::Grassland } else { Biome::Ocean } as u8)
            .collect();
        let (region, regions) = flood(&b, &vec![20; n * n], n, n, 4_000.0);
        assert!(regions.len() <= MAX_REGIONS, "{}", regions.len());
        assert_eq!(regions.iter().map(|r| r.cells as usize).sum::<usize>(), n * n);
        assert!(region.iter().all(|&id| (id as usize) < regions.len()));
        // A smaller cap is honoured the same way.
        let (_, capped) = flood_with(&b, &vec![20; n * n], (n, n), 1, 1_000);
        assert!(capped.len() <= 1_000, "{}", capped.len());
    }

    #[test]
    fn min_region_cells_is_area_based() {
        assert_eq!(min_region_cells(64.0), (MIN_REGION_AREA_M2 / 4096.0).ceil() as u32);
        assert_eq!(min_region_cells(16.0), 16 * min_region_cells(64.0));
        assert_eq!(min_region_cells(64.0), 144);
        assert_eq!(min_region_cells(100_000.0), 1);
    }

    #[test]
    fn alignment_hits_target_shares() {
        let cell_m = 64.0;
        // 2,000 land regions of 1 to 20,000 cells (up to ~140 DF tiles, so
        // every size bucket), plus the ocean.
        let mut regions: Vec<RegionRecord> = (0..2_000u32)
            .map(|i| {
                let cells = 1 + (hash01(9, 1, i as u64).powi(3) * 20_000.0) as u32;
                RegionRecord { id: i as u16, kind: RegionType::Forest, cells, alignment: 0 }
            })
            .collect();
        regions.push(RegionRecord { id: 2_000, kind: RegionType::Ocean, cells: 5_000_000, alignment: 0 });
        assign_alignment(&mut regions, 0xBEEF, 0.1, 0.1, cell_m);
        let land: f64 = regions.iter().filter(|r| r.kind != RegionType::Ocean).map(|r| r.cells as f64).sum();
        let share = |a: i8| regions.iter().filter(|r| r.alignment == a).map(|r| r.cells as f64).sum::<f64>() / land;
        let (good, evil) = (share(1), share(-1));
        assert!((good - 0.1).abs() <= 0.005, "good {good:.4}");
        assert!((evil - 0.1).abs() <= 0.005, "evil {evil:.4}");
        assert_eq!(regions.last().unwrap().alignment, 0, "the sea stays neutral");
        // Every bucket with regions has both.
        for bucket in 0..3 {
            let of = |a: i8| regions.iter().any(|r| r.kind != RegionType::Ocean && size_bucket(r.cells, cell_m) == bucket && r.alignment == a);
            if regions.iter().any(|r| r.kind != RegionType::Ocean && size_bucket(r.cells, cell_m) == bucket) {
                assert!(of(1) && of(-1), "bucket {bucket}");
            }
        }
        // Seeded: the same seed, the same answer; another seed, another.
        let mut again = regions.clone();
        assign_alignment(&mut again, 0xBEEF, 0.1, 0.1, cell_m);
        assert_eq!(again, regions);
        assign_alignment(&mut again, 0xF00D, 0.1, 0.1, cell_m);
        assert_ne!(again, regions);
    }

    /// One region holding most of the land is not forced good or evil; a
    /// bucket whose goal is under its smallest region still gets one when it
    /// is close.
    #[test]
    fn forced_region_must_be_near_the_goal() {
        let big = |id: u16, cells: u32| RegionRecord { id, kind: RegionType::Forest, cells, alignment: 0 };
        // At 300 m cells both are large (1,374 and 229 DF tiles).
        let mut regions = vec![big(0, 9_000), big(1, 1_500)];
        assign_alignment(&mut regions, 1, 0.1, 0.1, 300.0);
        // Goal 1,050 cells: the 1,500 one is forced (under 3 goals), the
        // 9,000 one never is.
        assert_eq!(regions[0].alignment, 0);
        assert_ne!(regions[1].alignment, 0);
        let mut lone = vec![big(0, 10_000)];
        assign_alignment(&mut lone, 1, 0.1, 0.1, 300.0);
        assert_eq!(lone[0].alignment, 0);
    }

    #[test]
    fn savagery_covers_the_thirds() {
        let (n, cell_m) = (256, 256.0); // 65 km
        let s = savagery(3, n, n, cell_m);
        let third = |lo: u8, hi: u8| s.iter().filter(|&&v| v >= lo && v <= hi).count() as f64 / s.len() as f64;
        let (calm, mid, savage) = (third(0, 32), third(33, 66), third(67, 100));
        eprintln!("savagery thirds: calm {calm:.2} neutral {mid:.2} savage {savage:.2}");
        for share in [calm, mid, savage] {
            assert!((0.2..=0.47).contains(&share), "{calm:.2} {mid:.2} {savage:.2}");
        }
        assert_eq!(savagery(3, n, n, cell_m), s);
        assert_ne!(savagery(4, n, n, cell_m), s);
    }
}
