//! Far terrain of stud worlds: one coarse height field per [`LodKey`].
//!
//! The far field is a [`StudLodTile`] per key: the ground
//! recipe point-sampled on the key's **cell-corner lattice** — `side + 1` samples per
//! edge, `side = StudLodTile::side_for(key)` cells of `key.factor` sim units each. A sample
//! is the recipe of the stud column whose min corner is the lattice point: its top
//! (**plates**, the recipe's frame), the material showing there, and the water standing
//! over it. Tiles of one factor share their border samples, and every coarser lattice
//! is a subset of the finer ones, so far terrain meets near terrain and its neighbours
//! exactly at the lattice points.
//!
//! It is a function of the key alone (and the installed forged world): edits and built
//! structures are not in it. Pure data code — no Bevy, no I/O.

use serde::{Deserialize, Serialize};

use crate::lod::{cells_per_region, LodKey, LOD_FACTORS};
use crate::stud::STUD_UNITS;

/// The finest stud LOD cell: one stud column (a factor is a cell edge in sim units).
pub const STUD_LOD_MIN_FACTOR: u32 = STUD_UNITS as u32;
/// Most cells per tile edge the server builds: a finer key is refused (a tile is one
/// mesh on the client, and `side + 1` squared samples on the wire).
pub const STUD_LOD_MAX_SIDE: u16 = 256;
/// How much coarser a stud ring's cells are than the base ring table's
/// (`lod::LOD_RING_TABLE`, cut to a pixel budget per cell) at the same distance
/// ([`stud_ring_table`]). That budget was sized for 8-byte quad records in one GPU
/// arena; a stud tile is an ordinary vertex mesh, ~16x the bytes per cell, so it
/// takes 4x the cell edge (1/16 of the cells).
pub const STUD_LOD_COARSEN: u32 = 4;
/// `water` of a sample with no water standing over its ground.
pub const STUD_LOD_NO_WATER: i16 = i16::MIN;

/// One lattice point of a [`StudLodTile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StudLodSample {
    /// One past the top solid plate (the recipe's `surface`), saturated to `i16`.
    pub top: i16,
    /// The stud material of the top plate.
    pub material: u16,
    /// One past the top water plate where water stands over the ground
    /// (`water > top`), else [`STUD_LOD_NO_WATER`].
    pub water: i16,
}

impl StudLodSample {
    /// A recipe column as a sample: `surface` and `water_top` in plates.
    pub fn of_column(surface: i32, material: u16, water_top: Option<i32>) -> Self {
        let clamp = |p: i32| p.clamp(i32::from(i16::MIN) + 1, i32::from(i16::MAX)) as i16;
        let top = clamp(surface);
        let water = water_top.filter(|&w| w > surface).map_or(STUD_LOD_NO_WATER, clamp);
        Self { top, material, water }
    }

    pub fn has_water(&self) -> bool {
        self.water != STUD_LOD_NO_WATER
    }
}

/// A key's far terrain on the wire: `(side + 1)²` samples, row major (x fastest,
/// then z), each stream packed on its own.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StudLodTile {
    /// Cells per tile edge.
    pub side: u16,
    /// Tops as deltas from the sample before (the first is from 0): one byte per delta
    /// in `-127..=127`, else [`HEIGHT_ESCAPE`] and the delta as two little-endian bytes.
    pub heights: Vec<u8>,
    /// `(run, material)` over the samples.
    pub materials: Vec<(u16, u16)>,
    /// `(run, water)` over the samples.
    pub water: Vec<(u16, i16)>,
}

/// Marks a two-byte height delta in [`StudLodTile::heights`].
pub const HEIGHT_ESCAPE: u8 = 0x80;

fn push_run<T: Copy + PartialEq>(runs: &mut Vec<(u16, T)>, v: T) {
    match runs.last_mut() {
        Some((n, last)) if *last == v && *n < u16::MAX => *n += 1,
        _ => runs.push((1, v)),
    }
}

fn expand_runs<T: Copy>(runs: &[(u16, T)], count: usize) -> Option<Vec<T>> {
    if runs.iter().any(|&(n, _)| n == 0) || runs.iter().map(|&(n, _)| usize::from(n)).sum::<usize>() != count {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for &(n, v) in runs {
        out.extend(std::iter::repeat_n(v, usize::from(n)));
    }
    Some(out)
}

impl StudLodTile {
    /// Cells per edge of `key`'s tile: `region_span() × cells_per_region(factor)`.
    pub fn side_for(key: &LodKey) -> u16 {
        let side = key.region_span() as usize * cells_per_region(key.factor);
        u16::try_from(side).expect("a LOD key's cells per edge fit a u16")
    }

    /// Samples of a tile of `side` cells.
    pub fn sample_count(side: u16) -> usize {
        (usize::from(side) + 1) * (usize::from(side) + 1)
    }

    /// Pack `samples` (`sample_count(side)` of them, row major).
    pub fn encode(side: u16, samples: &[StudLodSample]) -> Self {
        debug_assert_eq!(samples.len(), Self::sample_count(side));
        let mut tile = Self { side, heights: Vec::with_capacity(samples.len()), ..Default::default() };
        let mut prev = 0i32;
        for s in samples {
            let delta = i32::from(s.top) - prev;
            prev = i32::from(s.top);
            if (-127..=127).contains(&delta) {
                tile.heights.push(delta as i8 as u8);
            } else {
                // Two bytes hold any difference of two `i16`s modulo 2^16.
                tile.heights.push(HEIGHT_ESCAPE);
                tile.heights.extend_from_slice(&(delta as u16).to_le_bytes());
            }
            push_run(&mut tile.materials, s.material);
            push_run(&mut tile.water, s.water);
        }
        tile
    }

    /// The samples, row major; `None` when any stream does not hold exactly
    /// `sample_count(side)` of them (wire data is untrusted).
    pub fn decode(&self) -> Option<Vec<StudLodSample>> {
        let count = Self::sample_count(self.side);
        // A sample is at least one height byte: bounds the allocation below.
        if self.heights.len() < count || self.heights.len() > 3 * count {
            return None;
        }
        let materials = expand_runs(&self.materials, count)?;
        let water = expand_runs(&self.water, count)?;
        let mut out = Vec::with_capacity(count);
        let mut bytes = self.heights.iter().copied();
        let mut prev = 0i16;
        for i in 0..count {
            let b = bytes.next()?;
            let delta = if b == HEIGHT_ESCAPE {
                u16::from_le_bytes([bytes.next()?, bytes.next()?])
            } else {
                b as i8 as i16 as u16
            };
            prev = prev.wrapping_add(delta as i16);
            out.push(StudLodSample { top: prev, material: materials[i], water: water[i] });
        }
        bytes.next().is_none().then_some(out)
    }

    pub fn has_water(&self) -> bool {
        self.water.iter().any(|&(n, w)| n > 0 && w != STUD_LOD_NO_WATER)
    }
}

/// Whether the server builds stud far terrain for `key`: a streamable factor no finer
/// than a stud column, and a tile of at most [`STUD_LOD_MAX_SIDE`] cells.
pub fn stud_lod_supported(key: &LodKey) -> bool {
    LOD_FACTORS.contains(&key.factor)
        && key.factor >= STUD_LOD_MIN_FACTOR
        && StudLodTile::side_for(key) <= STUD_LOD_MAX_SIDE
}

/// The coarsest streamable factor.
const MAX_FACTOR: u32 = LOD_FACTORS[LOD_FACTORS.len() - 1];

/// The ring table a client subscribes with, from the base one for the same view
/// (`LOD_RING_TABLE`, a `VOXEL_LOD_RINGS` / view-distance table): the same bounds with
/// every factor [`STUD_LOD_COARSEN`] times coarser, raised until its tiles are
/// [`stud_lod_supported`] and capped at the coarsest factor; neighbouring rings that
/// end up equal are one ring. The reach is the base table's.
pub fn stud_ring_table(table: &[(i32, u32)]) -> Vec<(i32, u32)> {
    let mut out: Vec<(i32, u32)> = Vec::new();
    for &(bound, factor) in table {
        let mut f = factor.saturating_mul(STUD_LOD_COARSEN).clamp(STUD_LOD_MIN_FACTOR, MAX_FACTOR);
        while f < MAX_FACTOR && !stud_lod_supported(&LodKey { factor: f, coords: [0, 0] }) {
            f *= 2;
        }
        match out.last_mut() {
            // Factors only grow outward.
            Some((b, last)) if *last >= f => *b = bound,
            _ => out.push((bound, f)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(top: i16, material: u16, water: i16) -> StudLodSample {
        StudLodSample { top, material, water }
    }

    #[test]
    fn a_tile_round_trips_every_height_and_run() {
        let side = 3u16;
        let n = StudLodTile::sample_count(side);
        // Flats, small steps, steps past a byte, the extremes, and the escape value itself.
        let tops = [0i16, 0, 1, -1, 127, 0, -128, 300, -300, i16::MAX, i16::MIN + 1, 5, 5, 5, 133, 5];
        assert_eq!(tops.len(), n);
        let samples: Vec<StudLodSample> = tops
            .iter()
            .enumerate()
            .map(|(i, &t)| sample(t, if i < 9 { 0x8000 } else { 0x8003 }, if i % 5 == 0 { t.saturating_add(4) } else { STUD_LOD_NO_WATER }))
            .collect();
        let tile = StudLodTile::encode(side, &samples);
        assert_eq!(tile.decode().as_deref(), Some(samples.as_slice()));
        assert_eq!(tile.materials, vec![(9, 0x8000), (7, 0x8003)]);
        assert!(tile.has_water());
        // A flat dry tile is a byte per sample and one run each.
        let flat = StudLodTile::encode(side, &vec![sample(0, 1, STUD_LOD_NO_WATER); n]);
        assert_eq!((flat.heights.len(), flat.materials.len(), flat.water.len()), (n, 1, 1));
        assert!(!flat.has_water());
    }

    #[test]
    fn malformed_tiles_do_not_decode() {
        let side = 2u16;
        let n = StudLodTile::sample_count(side);
        let good = StudLodTile::encode(side, &vec![sample(7, 1, STUD_LOD_NO_WATER); n]);
        assert!(good.decode().is_some());
        let mut short = good.clone();
        short.heights.pop();
        assert_eq!(short.decode(), None);
        let mut long = good.clone();
        long.heights.push(0);
        assert_eq!(long.decode(), None);
        let mut cut = good.clone();
        *cut.heights.last_mut().unwrap() = HEIGHT_ESCAPE; // an escape with no bytes after it
        assert_eq!(cut.decode(), None);
        let mut runs = good.clone();
        runs.materials = vec![(n as u16 - 1, 1)];
        assert_eq!(runs.decode(), None);
        let mut empty_run = good.clone();
        empty_run.water.push((0, 3));
        assert_eq!(empty_run.decode(), None);
        let mut side_lie = good.clone();
        side_lie.side = 200;
        assert_eq!(side_lie.decode(), None);
    }

    #[test]
    fn a_column_becomes_a_sample() {
        assert_eq!(StudLodSample::of_column(12, 9, None), sample(12, 9, STUD_LOD_NO_WATER));
        assert_eq!(StudLodSample::of_column(12, 9, Some(20)), sample(12, 9, 20));
        // Water at or under the ground does not stand over it.
        assert_eq!(StudLodSample::of_column(12, 9, Some(12)), sample(12, 9, STUD_LOD_NO_WATER));
        assert_eq!(StudLodSample::of_column(i32::MAX, 9, None).top, i16::MAX);
        assert_eq!(StudLodSample::of_column(i32::MIN, 9, None).top, i16::MIN + 1);
    }

    #[test]
    fn the_stud_ring_table_keeps_the_reach_with_coarser_supported_tiles() {
        let base = crate::lod::LOD_RING_TABLE;
        let stud = stud_ring_table(base);
        assert_eq!(stud.last().map(|r| r.0), base.last().map(|r| r.0), "same reach");
        assert!(stud.windows(2).all(|w| w[0].0 < w[1].0 && w[0].1 < w[1].1), "bounds and factors ascend: {stud:?}");
        for &(_, f) in &stud {
            assert!(stud_lod_supported(&LodKey { factor: f, coords: [0, 0] }), "factor {f}");
        }
        // Every base ring is covered by a ring at least as coarse as the rule asks.
        for &(bound, f) in base {
            let ring = stud.iter().find(|r| bound <= r.0).unwrap();
            assert!(ring.1 >= (f * STUD_LOD_COARSEN).min(MAX_FACTOR));
        }
        assert!(stud_ring_table(&[]).is_empty());
        assert_eq!(stud_ring_table(&[(2, MAX_FACTOR), (5, MAX_FACTOR)]), vec![(5, MAX_FACTOR)]);
    }

    #[test]
    fn unsupported_keys_are_finer_than_a_stud_or_too_wide() {
        assert!(!stud_lod_supported(&LodKey { factor: 2, coords: [0, 0] }));
        assert!(!stud_lod_supported(&LodKey { factor: 3, coords: [0, 0] }), "not a streamable factor");
        assert!(stud_lod_supported(&LodKey { factor: MAX_FACTOR, coords: [3, -4] }));
        let finest = LodKey { factor: STUD_LOD_MIN_FACTOR, coords: [0, 0] };
        assert_eq!(stud_lod_supported(&finest), StudLodTile::side_for(&finest) <= STUD_LOD_MAX_SIDE);
    }
}
