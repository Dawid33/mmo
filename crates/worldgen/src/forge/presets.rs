//! Named starting points for the map generator: a preset is a list of knob
//! overrides on top of `Knobs::default()`. The UI shows [`CUSTOM`] when the
//! knobs match none of them. The first is the defaults (Continent);
//! Highlands / Lowlands / Dry / Wet apply on top of it.

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
        name: "Continent",
        description: "One continent with sea on some edges and land running off the others, a pole to the north, ranges along plate boundaries.",
        overrides: &[],
    },
    Preset {
        name: "Continents",
        description: "Several landmasses with seas between them, ranges where their plates meet.",
        overrides: &[("layout", 4.0)],
    },
    Preset {
        name: "Inland",
        description: "Land on every edge around an inland sea; drier, with closed basins whose lakes turn salty.",
        overrides: &[("layout", 5.0), ("rain_max", 2.0), ("land_depletion_m", 5_000.0), ("lake_basins", 14.0)],
    },
    Preset {
        name: "Temperate band",
        description: "No pole: the whole map lies in the mid-latitude westerlies, mostly forest and grassland.",
        overrides: &[("pole", 0.0), ("latitude_center_deg", 48.0)],
    },
    Preset {
        name: "Frozen north",
        description: "The far north from the pole down to 60 degrees: tundra, taiga and glaciers.",
        overrides: &[("latitude_span_deg", 30.0), ("pole_temp_c", -30.0)],
    },
    Preset {
        name: "Tropical",
        description: "No pole, near the equator: jungle, savanna and wetlands.",
        overrides: &[("pole", 0.0), ("latitude_center_deg", 16.0)],
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
        name: "Island",
        description: "A single island in open sea at one latitude, ranges inland, rivers to the sea (the pre-continent look).",
        overrides: &[
            ("layout", 0.0),
            ("pole", 0.0),
            ("latitude_center_deg", 45.0),
            ("sea_edges", 4.0),
            ("max_peak_m", 236.0),
            ("meander_amplitude_deg", 0.0),
        ],
    },
    Preset {
        name: "Archipelago",
        description: "Several islands scattered across the sea, each with its own short rivers.",
        overrides: &[
            ("layout", 1.0),
            ("pole", 0.0),
            ("latitude_center_deg", 45.0),
            ("sea_edges", 4.0),
            ("max_peak_m", 236.0),
            ("meander_amplitude_deg", 0.0),
        ],
    },
    Preset {
        name: "Island arc",
        description: "A chain of islands strung along a curve, the largest in the middle.",
        overrides: &[
            ("layout", 2.0),
            ("pole", 0.0),
            ("latitude_center_deg", 45.0),
            ("sea_edges", 4.0),
            ("max_peak_m", 236.0),
            ("meander_amplitude_deg", 0.0),
        ],
    },
    Preset {
        name: "Highlands",
        description: "Fast uplift and hard rock raise broad, steep ranges cut by deep valleys.",
        overrides: &[
            ("uplift", 6.0e-3),
            ("erodibility", 1.6e-5),
            ("belt_width", 0.40),
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

