//! Every tunable of the forge in ONE runtime table, so the CLI (`--set
//! key=value`) and the tuning page (`--serve`, serve.rs) can change them
//! without a rebuild. A knob is always an f64; counts are rounded where used.
//! The defaults here ARE the forge's tuning — when a value found in the page
//! should stick, change its default below.

macro_rules! knobs {
    ($( $group:literal { $( $field:ident : $default:expr, [$min:expr, $max:expr], $label:literal, $help:literal; )* } )*) => {
        #[derive(Clone, Debug)]
        pub struct Knobs { $( $( pub $field: f64, )* )* }

        impl Default for Knobs {
            fn default() -> Self { Knobs { $( $( $field: $default, )* )* } }
        }

        pub struct Spec {
            pub key: &'static str,
            pub group: &'static str,
            pub label: &'static str,
            pub help: &'static str,
            pub default: f64,
            pub min: f64,
            pub max: f64,
        }

        pub const SPECS: &[Spec] = &[ $( $( Spec {
            key: stringify!($field), group: $group, label: $label, help: $help,
            default: $default, min: $min, max: $max,
        }, )* )* ];

        impl Knobs {
            /// Set a knob by key; false if there is no such knob.
            pub fn set(&mut self, key: &str, value: f64) -> bool {
                match key { $( $( stringify!($field) => self.$field = value, )* )* _ => return false }
                true
            }

            pub fn get(&self, key: &str) -> Option<f64> {
                match key { $( $( stringify!($field) => Some(self.$field), )* )* _ => None }
            }
        }
    };
}

knobs! {
    "Land" {
        continent_count: 3.0, [2.0, 4.0], "Continents", "How many landmasses the world is split into, spread round the whole width.";
        continent_sea_width: 0.20, [0.02, 0.3], "Sea between continents", "Half-width of the seas between neighbouring continents, as a share of the half-width.";
        continent_coast: 0.45, [0.15, 0.9], "Polar coast distance", "How far in from the north and south edges the coast sits, as a share of the half-height (each edge varies a little by seed).";
        coast_noise_lo: 0.6, [0.0, 1.6], "Coast wander (8 km)", "Strength of the 8 km noise that pushes the coast in and out: bays, peninsulas, the overall outline.";
        coast_noise_hi: 0.18, [0.0, 0.7], "Coast detail (1 km)", "Strength of the 1 km noise on the coastline: coves and headlands.";
        ocean_margin_start: 0.84, [0.55, 0.95], "Ocean margin", "Land is forced under the sea beyond this share of the half-height toward the north and south edges: the polar seas.";
    }
    "Plates" {
        plate_count: 7.0, [3.0, 24.0], "Plates", "How many plates the world is cut into. Mountain belts rise where two plates converge, so fewer plates means longer ranges.";
        plate_belt_width: 0.15, [0.03, 0.3], "Belt half-width", "Half-width of a plate-boundary mountain belt, as a share of the world's size.";
        belt_uplift: 1.4, [0.3, 2.5], "Belt uplift", "Rock uplift on a fully convergent belt, as a multiple of the uplift rate. Linear belts drain out sideways in a few km, so they need more than broad massifs for the same peaks.";
        belt_length_km: 8.0, [2.0, 60.0], "Interior range length (km)", "Length of each interior range (capped at 60% of the world's size).";
        interior_ranges: 2.0, [0.0, 8.0], "Interior ranges", "Ranges away from any plate boundary (old belts inside a plate), at about a third of a boundary belt's uplift.";
        volcano_count_max: 6.0, [0.0, 24.0], "Volcanoes", "At most this many volcanoes, each a cone of uplift where volcanism is at its peak (hot spots on the flanks of convergent belts), at least 8 km apart.";
        volcanism_strength: 1.0, [0.0, 2.0], "Volcanism", "Scales the volcanism field (continental-scale noise plus hot spots along convergent belts). 0 = none and no volcanoes; higher = more ground at the peak.";
    }
    "Uplift" {
        uplift: 5.0e-3, [5.0e-5, 6.0e-3], "Uplift rate (m/yr)", "Peak rock uplift in the mountain belts. Relief scales with uplift / erodibility: the default builds 2-2.5 km ranges.";
        lowland_uplift_share: 0.04, [0.0, 0.6], "Lowland uplift share", "Uplift outside the belts as a share of the peak rate: rolling lowlands vs flat ones.";
    }
    "Erosion" {
        erodibility: 2.5e-5, [4.0e-6, 1.2e-4], "Erodibility K", "Stream-power erodibility of a rock of hardness 1. Higher = lower, more dissected land.";
        diffusivity: 0.05, [0.0, 0.06], "Hillslope diffusion (m²/yr)", "Rounds ridges and valley walls. 0 = knife edges.";
        hillslope_area_m2: 6_000.0, [500.0, 40_000.0], "Channel head area (m²)", "Drainage area below which a cell is hillslope, not channel: spacing of the finest valleys.";
        depression_fill: 0.15, [0.0, 1.0], "Depression silting", "Share of a depression's depth that sediment fills per iteration.";
        iters: 200.0, [50.0, 1000.0], "Iterations", "Time steps of the coarsest level. At the default rates relief is ~80% of steady state by 200; the belts exhume ~1.7 km of rock per 100, and past ~250 the valleys cut below the geology column's 5 km.";
        dt_years: 2_500.0, [500.0, 10_000.0], "Years per iteration", "The incision step is implicit, so this is limited by routing refresh, not stability.";
    }
    "Schedule" {
        coarse_levels: 2.0, [0.0, 3.0], "Coarse levels", "Erosion runs first on cells this many times 2x coarser, then refines level by level down to the final cell size: most of the work at a quarter (or less) of the cells.";
        refine_iters: 80.0, [10.0, 300.0], "Refine iterations", "Iterations at the final cell size after the coarse levels (the coarsest runs Iterations, any level between half this): lets valleys re-cut at the new resolution.";
    }
    "Geology" {
        geology_strength: 1.0, [0.0, 2.0], "Rock contrast", "0 = uniform rock; 1 = hardnesses as listed; 2 = doubled contrast (more scarps and cliffs).";
        regional_dip: 0.04, [0.0, 0.15], "Regional dip", "Tilt of the strata (rise/run): how many formations outcrop across the world.";
        fold_amplitude_m: 45.0, [0.0, 160.0], "Fold amplitude (m)", "Height of the 2 km folds in the strata.";
        formation_min_m: 18.0, [4.0, 80.0], "Thinnest formation (m)", "Sedimentary formations are between this and the thickest.";
        formation_max_m: 64.0, [10.0, 200.0], "Thickest formation (m)", "Sedimentary formations are between the thinnest and this.";
        basement_top_s: -1_500.0, [-4_500.0, -80.0], "Basement depth (m)", "Stratigraphic level of the crystalline basement: shallower = more granite in the range cores.";
        hard_sandstone: 1.0, [0.2, 3.0], "Sandstone erodibility", "Erodibility multiplier; below 1 resists (steeper, higher), above 1 weathers away.";
        hard_shale: 1.7, [0.2, 3.0], "Shale erodibility", "Erodibility multiplier; shale is the soft one that floors valleys.";
        hard_limestone: 0.75, [0.2, 3.0], "Limestone erodibility", "Erodibility multiplier.";
        hard_conglomerate: 0.65, [0.2, 3.0], "Conglomerate erodibility", "Erodibility multiplier.";
        hard_basalt: 0.5, [0.2, 3.0], "Basalt erodibility", "Erodibility multiplier of the thin sills.";
        hard_granite: 0.6, [0.2, 3.0], "Granite erodibility", "Erodibility multiplier of the basement.";
    }
    "Climate" {
        equator_temp_c: 27.0, [10.0, 40.0], "Equator temperature (C)", "Mean sea-level temperature at the equator.";
        pole_temp_c: -25.0, [-50.0, 5.0], "Pole temperature (C)", "Mean sea-level temperature at a pole; between, temperature follows the cosine of latitude.";
        lapse_c_per_km: 6.5, [3.0, 10.0], "Lapse rate (C/km)", "Cooling per kilometre of height above the sea.";
        meander_amplitude_deg: 5.0, [0.0, 15.0], "Band meander (deg)", "Amplitude of the slow north-south wander of the climate bands (a jet-stream / planetary-wave warp of latitude): temperature, the latitude rain table and the wind bands follow it, so band edges and isotherms wander instead of running ruler-straight. 0 = straight bands.";
        meander_wavelength_deg: 72.0, [9.0, 180.0], "Meander wavelength (deg)", "Wavelength of the main meander wave along the map, in degrees of latitude, so every world size sees the same waves (two shorter waves ride on it). Capped at 90 deg (half the pole-to-pole span), so the planet still sees whole waves.";
        trade_wind_edge_deg: 30.0, [5.0, 45.0], "Trade winds to (deg)", "Below this latitude the wind blows from the east (trade winds); above it the westerlies. Within 5 deg either side (BAND_BLEND_DEG) the two directions blend, so no line marks the edge.";
        polar_edge_deg: 60.0, [45.0, 85.0], "Polar easterlies from (deg)", "Above this latitude the wind blows from the east again (polar easterlies). Within 5 deg either side (BAND_BLEND_DEG) the westerlies and easterlies blend.";
        climate_strength: 1.0, [0.0, 1.0], "Rain shadow strength", "0 = precipitation follows latitude only; 1 = full orographic rain and rain shadows along the prevailing wind.";
        land_depletion_deg: 16.0, [3.0, 100.0], "Inland relaxation (deg)", "Distance over flat land, in degrees of latitude, in which the air's moisture relaxes toward the continental equilibrium (rain-out balanced by evaporation from the land): shorter = the interior reaches its equilibrium sooner. In degrees, so every world size is the same planet.";
        orographic_scale_m: 650.0, [100.0, 3_000.0], "Orographic scale (m)", "Rise that rains out about two thirds of the air's moisture: lower = sharper wet/dry contrast.";
        sea_recharge_deg: 9.0, [1.0, 36.0], "Sea recharge (deg)", "Distance over open sea, in degrees of latitude, in which the air regains its moisture.";
        rain_min: 0.4, [0.05, 1.0], "Driest rain", "Lower bound of precipitation relative to its latitude's base (the latitude table).";
        rain_max: 3.0, [1.0, 6.0], "Wettest rain", "Upper bound of precipitation relative to its latitude's base (the latitude table).";
    }
    "Shaping" {
        max_peak_m: 2_800.0, [80.0, 3_500.0], "Tallest peak allowed (m)", "Land is scaled down if the simulated relief overshoots this (forged terrain is generated up to block 4096; the map ramps end at 3,500 m).";
        floodplain_m_per_sqrt_km2: 28.0, [0.0, 90.0], "Valley floor width", "Half-width of the planed valley floor per sqrt(km²) of discharge.";
        floodplain_max_m: 180.0, [0.0, 500.0], "Widest valley floor (m)", "Cap on the valley-floor half-width.";
        lake_basins: 9.0, [0.0, 40.0], "Lake basins", "How many overdeepened basins are excavated on valley floors.";
        lake_spacing_m: 1_500.0, [300.0, 5_000.0], "Lake spacing (m)", "Lake sites keep at least this far apart.";
        lake_radius_max_m: 420.0, [150.0, 1_200.0], "Largest lake radius (m)", "Basin radii run from 140 m to this.";
        lake_depth_max_m: 14.0, [5.0, 40.0], "Deepest lake basin (m)", "Basin depths run from 5 m to this.";
    }
    "Regions" {
        good_share: 0.1, [0.0, 0.5], "Good share", "Share of the land (by area) whose regions are good, assigned whole region by whole region per size class (DF's rule).";
        evil_share: 0.1, [0.0, 0.5], "Evil share", "Share of the land (by area) whose regions are evil, assigned like good.";
    }
    "Rivers" {
        river_min_discharge_m2: 400_000.0, [50_000.0, 4_000_000.0], "River threshold (m²)", "A cell is a river once this much rain-weighted land drains through it.";
        river_width_m: 5.5, [1.0, 20.0], "River width factor", "Channel width in meters per sqrt(km²) of discharge.";
    }
}

/// Knobs that are a choice, not a quantity: the page shows a list, and the
/// value is the index. None since the Planet became the only world.
pub const CHOICES: &[(&str, &[&str])] = &[];

impl Knobs {
    /// One `--set <knob>=<value>` flag of the forge tools (`worldctl forge`,
    /// `forge_proto`): the knob takes the value only if it is a number inside
    /// the knob's [`Spec`] range (NaN and infinities are not). A world's
    /// stored knobs are held to that range at load
    /// (`forged::knobs_from_map`), so a value outside it would forge one world
    /// and run another.
    pub fn set_flag(&mut self, arg: &str) -> Result<(), String> {
        let (key, number) = arg.split_once('=').ok_or("--set <knob>=<value>")?;
        let spec = SPECS
            .iter()
            .find(|s| s.key == key)
            .ok_or_else(|| format!("--set: no knob named {key} (see forge/knobs.rs)"))?;
        let value: f64 = number
            .parse()
            .map_err(|_| format!("--set {key}: not a number"))?;
        if !(spec.min..=spec.max).contains(&value) {
            return Err(format!(
                "--set {key}={number}: outside the knob's range; give {} to {}",
                spec.min, spec.max
            ));
        }
        self.set(key, value);
        Ok(())
    }

    pub fn plate_total(&self) -> usize {
        self.plate_count.round().clamp(2.0, 64.0) as usize
    }

    pub fn iterations(&self) -> usize {
        self.iters.round().max(1.0) as usize
    }

    /// Number of 2x-coarser erosion levels before the final cell size.
    pub fn coarse_level_count(&self) -> u32 {
        self.coarse_levels.round().clamp(0.0, 3.0) as u32
    }

    /// Iterations at each level finer than the coarsest.
    pub fn refine_iterations(&self) -> usize {
        self.refine_iters.round().max(1.0) as usize
    }

    /// Erodibility multiplier of rock `id` (index into geology::ROCKS), with
    /// the contrast knob applied around 1.
    pub fn rock_erodibility(&self, id: u8) -> f64 {
        let base = [
            self.hard_sandstone,
            self.hard_shale,
            self.hard_limestone,
            self.hard_conglomerate,
            self.hard_basalt,
            self.hard_granite,
        ][id as usize];
        (1.0 + self.geology_strength * (base - 1.0)).max(0.05)
    }
}
