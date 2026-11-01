//! Quality gate for forge speed-ups: a handful of summary numbers from the
//! hydrology analysis (the spec's acceptance stats: concavity theta, Hack
//! exponent, hypsometry, river and lake counts, peak heights) and a tolerance
//! check against a recorded baseline. A change that alters forge output on
//! purpose must stay within `QualityTolerance::DEFAULT` of the baseline.

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
}
