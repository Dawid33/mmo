//! Named starting points for the map generator: a preset is a list of knob
//! overrides on top of `Knobs::default()`. The UI shows [`CUSTOM`] when the
//! knobs match none of them. The first is the defaults (Earth-like); the
//! others apply on top of it.

use super::knobs::{Knobs, SPECS};

pub struct Preset {
    pub name: &'static str,
    pub description: &'static str,
    pub overrides: &'static [(&'static str, f64)],
}

/// Shown instead of a preset name when the knobs match no preset.
pub const CUSTOM: &str = "Custom";

pub const PRESETS: &[Preset] = &[
    Preset {
        name: "Earth-like",
        description: "A whole planet: continents round a world that wraps east to west, polar seas and ice at both poles, ranges along plate boundaries.",
        overrides: &[],
    },
    Preset {
        name: "Great ranges",
        description: "Taller, broader mountain belts of harder rock.",
        overrides: &[
            ("uplift", 6.0e-3),
            ("belt_uplift", 2.0),
            ("plate_belt_width", 0.2),
            ("max_peak_m", 3_400.0),
            ("hard_granite", 0.4),
            ("geology_strength", 1.3),
        ],
    },
    Preset {
        name: "Highlands",
        description: "Fast uplift and hard rock raise broad, steep ranges cut by deep valleys.",
        overrides: &[
            ("uplift", 6.0e-3),
            ("erodibility", 1.6e-5),
            ("plate_belt_width", 0.22),
            ("lowland_uplift_share", 0.10),
        ],
    },
    Preset {
        name: "Lowlands",
        description: "Slow uplift and soft rock leave rolling plains with wide floodplains and many lakes.",
        overrides: &[
            ("uplift", 2.0e-3),
            ("erodibility", 4.0e-5),
            ("lowland_uplift_share", 0.02),
            ("lake_basins", 24.0),
            ("floodplain_m_per_sqrt_km2", 45.0),
        ],
    },
    Preset {
        name: "Dry",
        description: "Rain runs out quickly inland, so rivers are few and the interior is parched.",
        overrides: &[
            ("rain_max", 1.8),
            ("rain_min", 0.15),
            ("land_depletion_m", 4_000.0),
            ("lake_basins", 3.0),
            ("river_min_discharge_m2", 1_200_000.0),
        ],
    },
    Preset {
        name: "Wet",
        description: "Rain reaches far inland, feeding many rivers, wide channels and lakes.",
        overrides: &[
            ("rain_max", 4.5),
            ("rain_min", 0.7),
            ("land_depletion_m", 30_000.0),
            ("lake_basins", 18.0),
            ("river_min_discharge_m2", 200_000.0),
        ],
    },
];

/// The default knobs with the preset's overrides applied.
pub fn apply(preset: &Preset) -> Knobs {
    let mut knobs = Knobs::default();
    for &(key, value) in preset.overrides {
        let known = knobs.set(key, value);
        debug_assert!(known, "preset {} names unknown knob {key}", preset.name);
    }
    knobs
}

/// The first preset whose knobs equal `knobs` exactly, if any.
pub fn matching(knobs: &Knobs) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| {
        let candidate = apply(p);
        SPECS.iter().all(|s| candidate.get(s.key) == knobs.get(s.key))
    })
}

