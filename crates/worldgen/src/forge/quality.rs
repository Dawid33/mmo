//! Quality gate for forge speed-ups: a handful of summary numbers from the
//! hydrology analysis (the spec's acceptance stats: concavity theta, Hack
//! exponent, hypsometry, river and lake counts, peak heights) and a tolerance
//! check against a recorded baseline. A change that alters forge output on
//! purpose must stay within `QualityTolerance::DEFAULT` of the baseline.

use game::{Biome, RegionType};

use super::world::smoothstep;
use super::Forged;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quality {
    pub land_share: f64,
    pub height_p50_m: f64,
    pub height_p90_m: f64,
    pub height_p99_m: f64,
    pub max_peak_m: f64,
    pub concavity_theta: f64,
    pub hack_exponent: f64,
    pub river_mouths: f64,
    pub river_km: f64,
    pub max_strahler: f64,
    pub lakes: f64,
}

/// Field names, in declaration order (the baseline file's keys).
const FIELDS: [&str; 11] = [
    "land_share",
    "height_p50_m",
    "height_p90_m",
    "height_p99_m",
    "max_peak_m",
    "concavity_theta",
    "hack_exponent",
    "river_mouths",
    "river_km",
    "max_strahler",
    "lakes",
];

#[derive(Clone, Copy, Debug)]
pub struct QualityTolerance {
    /// Relative, on land share, heights and river length.
    pub relative_heights_and_km: f64,
    /// Absolute, on theta and the Hack exponent.
    pub absolute_exponents: f64,
    /// Relative, on river mouth and lake counts.
    pub relative_counts: f64,
    /// Absolute, on the highest Strahler order.
    pub absolute_strahler: f64,
}

impl QualityTolerance {
    pub const DEFAULT: QualityTolerance = QualityTolerance {
        relative_heights_and_km: 0.10,
        absolute_exponents: 0.08,
        relative_counts: 0.25,
        absolute_strahler: 1.0,
    };
}

pub fn quality(f: &Forged) -> Quality {
    let s = &f.analysis.stats;
    Quality {
        land_share: s.land_share,
        height_p50_m: s.heights[1],
        height_p90_m: s.heights[2],
        height_p99_m: s.heights[3],
        max_peak_m: s.heights[4],
        concavity_theta: s.theta,
        hack_exponent: s.hack,
        river_mouths: s.mouths as f64,
        river_km: s.channel_km,
        max_strahler: s.max_order as f64,
        lakes: s.lake_count as f64,
    }
}

impl Quality {
    fn values(&self) -> [f64; 11] {
        [
            self.land_share,
            self.height_p50_m,
            self.height_p90_m,
            self.height_p99_m,
            self.max_peak_m,
            self.concavity_theta,
            self.hack_exponent,
            self.river_mouths,
            self.river_km,
            self.max_strahler,
            self.lakes,
        ]
    }

    /// The baseline file's text (RON-style struct, one field per line).
    pub fn to_text(&self) -> String {
        let mut s = String::from("(\n");
        for (k, v) in FIELDS.iter().zip(self.values()) {
            s.push_str(&format!("    {k}: {v:?},\n"));
        }
        s.push_str(")\n");
        s
    }

    pub fn from_text(text: &str) -> Result<Quality, String> {
        let mut v = [f64::NAN; 11];
        for line in text.lines() {
            let line = line.trim().trim_end_matches(',');
            let Some((k, val)) = line.split_once(':') else { continue };
            let Some(i) = FIELDS.iter().position(|f| *f == k.trim()) else {
                return Err(format!("unknown field {k:?}"));
            };
            v[i] = val.trim().parse().map_err(|e| format!("{k}: {e}"))?;
        }
        if let Some(i) = v.iter().position(|x| x.is_nan()) {
            // NaN is also how a missing fit would print; both are unusable.
            return Err(format!("field {} missing or NaN", FIELDS[i]));
        }
        Ok(Quality {
            land_share: v[0],
            height_p50_m: v[1],
            height_p90_m: v[2],
            height_p99_m: v[3],
            max_peak_m: v[4],
            concavity_theta: v[5],
            hack_exponent: v[6],
            river_mouths: v[7],
            river_km: v[8],
            max_strahler: v[9],
            lakes: v[10],
        })
    }
}

/// `Ok` when every metric of `a` is within `tol` of `baseline`; otherwise an
/// error naming every metric that is not.
pub fn within(a: &Quality, baseline: &Quality, tol: &QualityTolerance) -> Result<(), String> {
    enum Kind {
        Rel(f64),
        Abs(f64),
    }
    let rel = Kind::Rel(tol.relative_heights_and_km);
    let cnt = Kind::Rel(tol.relative_counts);
    let exp = Kind::Abs(tol.absolute_exponents);
    let kinds = [
        &rel, &rel, &rel, &rel, &rel, &exp, &exp, &cnt, &rel, &Kind::Abs(tol.absolute_strahler), &cnt,
    ];
    let mut bad = Vec::new();
    for (i, kind) in kinds.iter().enumerate() {
        let (x, b) = (a.values()[i], baseline.values()[i]);
        let ok = match kind {
            Kind::Rel(t) => (x - b).abs() <= t * b.abs(),
            Kind::Abs(t) => (x - b).abs() <= *t,
        };
        // NaN fails every comparison above, so an unfit exponent is reported.
        if !ok {
            bad.push(format!("{} = {x:.4} (baseline {b:.4})", FIELDS[i]));
        }
    }
    if bad.is_empty() {
        Ok(())
    } else {
        Err(format!("forge quality out of tolerance: {}", bad.join("; ")))
    }
}

// ---------------------------------------------------------------------------
// Seed-search score (spec section 10): what a Dwarf-Fortress-style world needs.
// ---------------------------------------------------------------------------

/// Land share window (of all cells) that scores full marks, and the margin
/// over which it falls to zero on each side.
pub const LAND_SHARE_RANGE: (f64, f64) = (0.30, 0.60);
pub const LAND_SHARE_MARGIN: f64 = 0.10;
/// Peaks above the snow line, mountain regions bordering other land, and
/// forest regions a world should have (each scores full at this count).
pub const WANTED_SNOW_PEAKS: u32 = 3;
pub const WANTED_MOUNTAIN_REGIONS: u32 = 3;
pub const WANTED_FOREST_REGIONS: u32 = 3;
/// Strahler order a river must reach (it then runs to the sea or a land edge:
/// every forged lake spills, so each river ends at a base level).
pub const WANTED_RIVER_ORDER: f64 = 4.0;
/// Distinct biomes a world should show.
pub const WANTED_BIOMES: usize = 10;
/// DF's `SUBREGION_MAX`-style cap on the region count (no confetti); the
/// score falls to zero at twice this.
pub const REGION_CAP: u32 = 2750;
/// A peak is the highest land cell within this distance (m) in every direction.
pub const PEAK_SEPARATION_M: f64 = 500.0;
/// Floor of each term in the product, so one weak point lowers a world
/// instead of zeroing it.
const TERM_FLOOR: f64 = 0.15;

/// The biome and region numbers the score needs, from a world file.
#[derive(Clone, Debug, PartialEq)]
pub struct BiomeSummary {
    /// Share of all cells per biome, largest first.
    pub shares: Vec<(Biome, f64)>,
    pub regions_by_type: Vec<(RegionType, u32)>,
    /// Separate summits (highest land cell within `PEAK_SEPARATION_M`) whose
    /// ground is colder than `forged::SNOW_LINE_C`, i.e. above the snow line.
    pub peaks_above_snow: u32,
    /// Mountain regions that touch land of another type (a dwarf start).
    pub mountain_regions_bordering_land: u32,
    /// A pole is on the map, so a glacier is expected.
    pub polar: bool,
}

impl BiomeSummary {
    fn regions(&self, kind: RegionType) -> u32 {
        self.regions_by_type.iter().filter(|(t, _)| *t == kind).map(|(_, c)| *c).sum()
    }
}

/// Biome and region numbers of `a`; `polar` = the forge had a pole on.
pub fn summarise(a: &crate::forged::WorldArtifact, polar: bool) -> BiomeSummary {
    use super::regions::{biome_shares, region_counts};
    let n = a.n as usize;
    let kind_of = |id: u16| a.regions.get(usize::from(id)).map(|r| r.kind);

    // Mountain regions with a 4-neighbour of a different land region.
    let mut bordering = vec![false; a.regions.len()];
    let touch = |me: u16, other: u16, bordering: &mut Vec<bool>| {
        if me == other {
            return;
        }
        let (Some(k), Some(o)) = (kind_of(me), kind_of(other)) else { return };
        if k == RegionType::Mountains && !o.is_water() && o != RegionType::Mountains {
            bordering[usize::from(me)] = true;
        }
    };
    for z in 0..n {
        for x in 0..n {
            let i = z * n + x;
            if x + 1 < n {
                touch(a.region[i], a.region[i + 1], &mut bordering);
                touch(a.region[i + 1], a.region[i], &mut bordering);
            }
            if z + 1 < n {
                touch(a.region[i], a.region[i + n], &mut bordering);
                touch(a.region[i + n], a.region[i], &mut bordering);
            }
        }
    }

    // Peaks: land cells equal to the maximum of their square window
    // (separable running maxima), colder than the snow line.
    let r = ((PEAK_SEPARATION_M / a.cell_m).round() as usize).max(1);
    let h: Vec<f32> = (0..n * n).map(|i| a.height.get(i)).collect();
    let window_max = |src: &[f32], horizontal: bool| -> Vec<f32> {
        let mut out = vec![f32::MIN; n * n];
        for line in 0..n {
            for k in 0..n {
                let (lo, hi) = (k.saturating_sub(r), (k + r).min(n - 1));
                let at = |j: usize| if horizontal { src[line * n + j] } else { src[j * n + line] };
                let m = (lo..=hi).map(at).fold(f32::MIN, f32::max);
                if horizontal {
                    out[line * n + k] = m;
                } else {
                    out[k * n + line] = m;
                }
            }
        }
        out
    };
    let wmax = window_max(&window_max(&h, true), false);
    let peaks_above_snow = (0..n * n)
        .filter(|&i| h[i] > 0.0 && h[i] >= wmax[i] && a.temperature_c.get(i) < crate::forged::SNOW_LINE_C)
        .count() as u32;

    BiomeSummary {
        shares: biome_shares(&a.biome),
        regions_by_type: region_counts(&a.regions).into_iter().map(|(t, c)| (t, c as u32)).collect(),
        peaks_above_snow,
        mountain_regions_bordering_land: bordering.iter().filter(|&&b| b).count() as u32,
        polar,
    }
}

/// The DF-style world score: the product of `TERM_FLOOR + (1 - TERM_FLOOR) *
/// term` over the terms (each 0..1), and the terms by name.
pub fn world_score(q: &Quality, biomes: &BiomeSummary) -> (f64, Vec<(&'static str, f64)>) {
    let count = |have: u32, want: u32| (f64::from(have) / f64::from(want)).min(1.0);
    let present = biomes.shares.iter().filter(|(_, s)| *s > 0.0).count();
    let regions: u32 = biomes.regions_by_type.iter().map(|(_, c)| *c).sum();
    let glacier = biomes.shares.iter().any(|(b, s)| *b == Biome::Glacier && *s > 0.0);
    let terms = vec![
        (
            "land share",
            smoothstep(LAND_SHARE_RANGE.0 - LAND_SHARE_MARGIN, LAND_SHARE_RANGE.0, q.land_share)
                * (1.0 - smoothstep(LAND_SHARE_RANGE.1, LAND_SHARE_RANGE.1 + LAND_SHARE_MARGIN, q.land_share)),
        ),
        ("snow peaks", count(biomes.peaks_above_snow, WANTED_SNOW_PEAKS)),
        ("mountain starts", count(biomes.mountain_regions_bordering_land, WANTED_MOUNTAIN_REGIONS)),
        ("forests", count(biomes.regions(RegionType::Forest), WANTED_FOREST_REGIONS)),
        ("big river", (q.max_strahler / WANTED_RIVER_ORDER).min(1.0)),
        ("biome variety", (present as f64 / WANTED_BIOMES as f64).min(1.0)),
        ("glacier", if !biomes.polar || glacier { 1.0 } else { 0.0 }),
        ("region cap", 1.0 - smoothstep(f64::from(REGION_CAP), 2.0 * f64::from(REGION_CAP), f64::from(regions))),
    ];
    let score = terms.iter().map(|(_, v)| TERM_FLOOR + (1.0 - TERM_FLOOR) * v).product();
    (score, terms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Quality {
        Quality::from_text(include_str!("../../tests/fixtures/forge_quality_16k_32m.ron")).unwrap()
    }

    #[test]
    fn text_round_trips_and_identical_passes() {
        let b = base();
        assert_eq!(Quality::from_text(&b.to_text()).unwrap(), b);
        assert!(within(&b, &b, &QualityTolerance::DEFAULT).is_ok());
    }

    #[test]
    fn every_out_of_tolerance_metric_is_named() {
        let b = base();
        let bad = Quality { max_peak_m: b.max_peak_m * 1.5, hack_exponent: b.hack_exponent + 0.2, lakes: 0.0, ..b };
        let e = within(&bad, &b, &QualityTolerance::DEFAULT).unwrap_err();
        assert!(e.contains("max_peak_m") && e.contains("hack_exponent") && e.contains("lakes"), "{e}");
        assert!(!e.contains("river_km"), "{e}");
    }

    fn summary(biomes: usize) -> BiomeSummary {
        BiomeSummary {
            shares: Biome::ALL.iter().take(biomes).map(|&b| (b, 1.0 / biomes as f64)).collect(),
            regions_by_type: vec![(RegionType::Forest, 5), (RegionType::Mountains, 4)],
            peaks_above_snow: 5,
            mountain_regions_bordering_land: 4,
            polar: false,
        }
    }

    #[test]
    fn score_rewards_biome_variety() {
        let q = Quality { land_share: 0.45, max_strahler: 5.0, ..base() };
        let (few, few_terms) = world_score(&q, &summary(4));
        let (many, many_terms) = world_score(&q, &summary(12));
        let variety = |t: &[(&str, f64)]| t.iter().find(|(n, _)| *n == "biome variety").unwrap().1;
        assert!(many > few, "{many} vs {few}");
        assert_eq!(variety(&many_terms), 1.0);
        assert!(variety(&few_terms) < 1.0);
    }

    #[test]
    fn glacier_only_matters_with_a_pole_and_regions_are_capped() {
        let q = Quality { land_share: 0.45, max_strahler: 5.0, ..base() };
        let term = |s: &BiomeSummary, name: &str| world_score(&q, s).1.into_iter().find(|t| t.0 == name).unwrap().1;
        let mut s = summary(12);
        assert_eq!(term(&s, "glacier"), 1.0);
        s.polar = true;
        s.shares.retain(|(b, _)| *b != Biome::Glacier);
        assert_eq!(term(&s, "glacier"), 0.0);
        s.shares.push((Biome::Glacier, 0.01));
        assert_eq!(term(&s, "glacier"), 1.0);
        s.regions_by_type = vec![(RegionType::Forest, REGION_CAP)];
        assert_eq!(term(&s, "region cap"), 1.0);
        s.regions_by_type = vec![(RegionType::Forest, 2 * REGION_CAP)];
        assert_eq!(term(&s, "region cap"), 0.0);
    }
}
