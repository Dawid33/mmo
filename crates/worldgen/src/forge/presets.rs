//! Named starting points for the map generator: a preset is a list of knob
//! overrides on top of `Knobs::default()`. The UI shows [`CUSTOM`] when the
//! knobs match none of them.

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
        name: "Default",
        description: "Normal settings: a continent with sea on some edges, ranges along plate boundaries, rivers to the sea or off the land edges.",
        overrides: &[],
    },
    Preset {
        name: "One island",
        description: "A single island in open sea, ranges inland, rivers to the sea.",
        overrides: &[("layout", 0.0)],
    },
    Preset {
        name: "Archipelago",
        description: "Several islands scattered across the sea, each with its own short rivers.",
        overrides: &[("layout", 1.0)],
    },
    Preset {
        name: "Island arc",
        description: "A chain of islands strung along a curve, the largest in the middle.",
        overrides: &[("layout", 2.0)],
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_override_names_a_knob_in_range() {
        for p in PRESETS {
            for &(key, value) in p.overrides {
                assert!(Knobs::default().get(key).is_some(), "{}: unknown knob {key}", p.name);
                let spec = SPECS.iter().find(|s| s.key == key).unwrap();
                assert!(
                    (spec.min..=spec.max).contains(&value),
                    "{}: {key}={value} outside {}..={}",
                    p.name, spec.min, spec.max
                );
            }
        }
    }

    #[test]
    fn default_preset_has_no_overrides() {
        assert_eq!(PRESETS[0].name, "Default");
        assert!(PRESETS[0].overrides.is_empty());
        let names: Vec<_> = PRESETS.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            ["Default", "One island", "Archipelago", "Island arc", "Highlands", "Lowlands", "Dry", "Wet"]
        );
    }

    #[test]
    fn apply_then_matching_round_trips() {
        for p in PRESETS {
            assert_eq!(matching(&apply(p)).map(|m| m.name), Some(p.name));
        }
    }

    #[test]
    fn edited_knobs_match_no_preset() {
        let mut k = apply(&PRESETS[0]);
        let u = k.get("uplift").unwrap();
        assert!(k.set("uplift", u * 1.1));
        assert!(matching(&k).is_none());
    }
}
