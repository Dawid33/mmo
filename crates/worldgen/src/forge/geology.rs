//! Geology BEFORE erosion: a layered rock column indexed by a stratigraphic
//! coordinate, so rock hardness shapes the landscape (Dwarf Fortress assigns
//! geology after erosion; Tarn Adams: "should really be earlier") and the same
//! model can later tell the engine which rock is at any (x, y, z).
//!
//!   s = (y - uplifted(x, z)) - bedding(x, z)
//!
//! `y - uplifted` undoes the rock uplift applied during the simulation: strata
//! were laid flat-ish (tilt + gentle folds = `bedding`) and then pushed up with
//! the mountains, so the belts dome them and the range cores expose the oldest,
//! deepest rock — for free, no "alpha" fudge needed. Erosion cuts the surface
//! through the column; hard bands stand proud, soft ones floor the valleys.

use crate::noise::OctaveNoise;
use crate::forge::world::{hash01, ForgeParams, World};

pub struct Rock {
    pub name: &'static str,
    pub rgb: [u8; 3],
}

/// Rock ids index this table; erodibilities are knobs (`Knobs::rock_erodibility`).
pub const ROCKS: [Rock; 6] = [
    Rock { name: "sandstone", rgb: [214, 180, 120] },
    Rock { name: "shale", rgb: [110, 116, 128] },
    Rock { name: "limestone", rgb: [226, 222, 200] },
    Rock { name: "conglomerate", rgb: [160, 110, 84] },
    Rock { name: "basalt", rgb: [58, 60, 66] },
    Rock { name: "granite", rgb: [222, 150, 160] },
];
const SANDSTONE: u8 = 0;
const SHALE: u8 = 1;
const LIMESTONE: u8 = 2;
const CONGLOMERATE: u8 = 3;
const BASALT: u8 = 4;
const GRANITE: u8 = 5;

/// The column covers this stratigraphic range (m). Surface rock starts near
/// s = 0 +- bedding relief and ends as deep as the total uplift (~800 m).
const COLUMN_TOP_S: f64 = 700.0;
const COLUMN_BOTTOM_S: f64 = -1_800.0;
/// Chance that a formation boundary carries a thin basalt sill, and its thickness.
const SILL_CHANCE: f64 = 0.10;
const SILL_THICKNESS_M: (f64, f64) = (6.0, 12.0);

/// The fold noise's first octave (2^11 = 2 km). Dip, fold amplitude,
/// formation thickness and basement depth are knobs: a dip of 0.04 over a
/// 16 km world walks the outcrop through ~640 m of column — broad provinces.
const FOLD_FIRST_OCTAVE: i32 = -11;

pub struct Geology {
    /// Bedding surface height per cell (m): tilt + folds.
    pub bedding: Vec<f64>,
    /// Formations from the top down: (top s, rock id). Bottom is COLUMN_BOTTOM_S.
    column: Vec<(f64, u8)>,
    /// Erodibility multiplier per rock id, contrast knob applied.
    erodibility: [f64; 6],
}

impl Geology {
    pub fn new(p: &ForgeParams, w: &World) -> Geology {
        let folds = OctaveNoise::new(p.seed ^ 0x6E01, FOLD_FIRST_OCTAVE, vec![1.0, 0.5]);
        // Dip direction from the seed.
        let angle = hash01(p.seed, 0x6E0, 1) * std::f64::consts::TAU;
        let k = &p.knobs;
        let (dip_x, dip_z) = (k.regional_dip * angle.cos(), k.regional_dip * angle.sin());
        let basement_top = k.basement_top_s;
        let (thin, thick) = (k.formation_min_m, k.formation_max_m.max(k.formation_min_m));
        let half = p.size_m * 0.5;
        let bedding = (0..w.n * w.n)
            .map(|i| {
                let (x, z) = w.center(i);
                dip_x * (x - half) + dip_z * (z - half) + k.fold_amplitude_m * 2.0 * folds.sample2(x, z)
            })
            .collect();

        let mut column = Vec::new();
        let (mut s, mut n, mut previous) = (COLUMN_TOP_S, 0u64, u8::MAX);
        while s > basement_top {
            // Weighted pick, never the same rock twice in a row.
            let rock = loop {
                let r = hash01(p.seed, 0x6E1, n);
                n += 1;
                let rock = match r {
                    r if r < 0.34 => SANDSTONE,
                    r if r < 0.64 => SHALE,
                    r if r < 0.86 => LIMESTONE,
                    _ => CONGLOMERATE,
                };
                if rock != previous {
                    break rock;
                }
            };
            previous = rock;
            column.push((s, rock));
            let t = hash01(p.seed, 0x6E2, n);
            s -= thin + t * (thick - thin);
            if hash01(p.seed, 0x6E3, n) < SILL_CHANCE && s > basement_top {
                column.push((s, BASALT));
                let t = hash01(p.seed, 0x6E4, n);
                s -= SILL_THICKNESS_M.0 + t * (SILL_THICKNESS_M.1 - SILL_THICKNESS_M.0);
                previous = BASALT;
            }
        }
        column.push((basement_top.min(s), GRANITE));
        let erodibility = [0u8, 1, 2, 3, 4, 5].map(|id| k.rock_erodibility(id));
        Geology { bedding, column, erodibility }
    }

    /// Formations from the top down: (top s, rock id).
    pub fn column(&self) -> &[(f64, u8)] {
        &self.column
    }

    /// Rock at stratigraphic coordinate `s`.
    pub fn rock_at_s(&self, s: f64) -> u8 {
        debug_assert!(s > COLUMN_BOTTOM_S, "column too shallow for s = {s}");
        // First formation whose top is at or above s, scanning down: the column
        // is sorted by descending top, so partition on "top > s".
        let idx = self.column.partition_point(|&(top, _)| top > s);
        self.column[idx.saturating_sub(1).min(self.column.len() - 1)].1
    }

    /// Rock exposed at the surface of cell `i`.
    pub fn surface_rock(&self, w: &World, i: usize) -> u8 {
        self.rock_at_s(w.h[i] - w.uplifted[i] - self.bedding[i])
    }

    /// Erodibility multiplier of the rock at the surface of cell `i`.
    pub fn erodibility(&self, w: &World, i: usize) -> f64 {
        self.erodibility[self.surface_rock(w, i) as usize]
    }
}
