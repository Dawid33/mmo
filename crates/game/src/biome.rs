//! Dwarf Fortress–style biomes: a pure function of a forged world's per-cell
//! fields (height, temperature, precipitation, drainage, salinity, lake), and
//! the region types DF groups them into. Pure data + functions, no sim state:
//! the forge classifies every cell once and stores the result in the world
//! file (`worldgen::forged`), and the client UI reads the same names.
//!
//! The table follows DF (docs/superpowers/specs/2026-09-20-df-worldgen-research/
//! 01-pipeline-and-fields.md §3): elevation gates (water, mountain), the
//! frozen override (tundra / glacier), then rainfall × drainage on DF's 0–100
//! scales, then temperature and salinity variants. Our fields are absolute
//! (°C, mm/yr, metres), so the rain index is `precip_mm / RAIN_INDEX_MM`
//! clamped to 0–100, and the temperature cuts are in °C.

use serde::{Deserialize, Serialize};

/// Land at or above this height (m above sea level) is mountain, whatever
/// its climate.
pub const MOUNTAIN_MIN_M: f32 = 1200.0;
/// Precipitation per point of DF's 0–100 rain index: `r = precip_mm / 16`,
/// so desert is under 160 mm/yr and broadleaf forest from 1,200 mm/yr.
pub const RAIN_INDEX_MM: f32 = 16.0;
/// Below this mean annual temperature (°C) land is tundra or glacier and
/// the sea is frozen.
pub const FROZEN_C: f32 = -5.0;
/// Below this (°C) frozen land is glacier whatever its drainage.
pub const GLACIER_C: f32 = -12.0;
/// Conifer forest under this (°C) is taiga.
pub const TAIGA_MAX_C: f32 = 3.0;
/// From this (°C) up, forests and saltwater swamps take their tropical form.
pub const TROPICAL_MIN_C: f32 = 20.0;

/// Rain-index cuts of DF's table (columns): under `RAIN_DESERT` the dry
/// biomes, then grassland, savanna, shrubland / marsh, conifer / swamp and
/// from `RAIN_BROADLEAF` broadleaf / swamp.
pub const RAIN_DESERT: f32 = 10.0;
pub const RAIN_SAVANNA: f32 = 20.0;
pub const RAIN_SHRUBLAND: f32 = 33.0;
pub const RAIN_CONIFER: f32 = 66.0;
pub const RAIN_BROADLEAF: f32 = 75.0;
/// Drainage under this is waterlogged: desert sand, marsh and swamp rows.
pub const DRAINAGE_WET: u8 = 33;
/// Dry ground drained at or above this is badlands instead of rocky wasteland.
pub const DRAINAGE_BADLANDS: u8 = 66;
/// A grassland-family biome drained at or above this is hills (region type).
pub const HILLS_MIN_DRAINAGE: u8 = 50;
/// Frozen land (under `FROZEN_C`) drained at or above this is glacier.
pub const GLACIER_MIN_DRAINAGE: u8 = 75;
/// A tropical swamp is mangrove at this salinity and up …
pub const MANGROVE_MIN_SALINITY: u8 = 66;
/// … and drainage at most this.
pub const MANGROVE_MAX_DRAINAGE: u8 = 9;

/// One cell's biome. Stored as its `u8` discriminant in the world file:
/// append only, never renumber.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Biome {
    Ocean,
    FrozenOcean,
    Lake,
    Glacier,
    Tundra,
    Taiga,
    ConiferForest,
    BroadleafForest,
    TropicalConiferForest,
    TropicalBroadleafForest,
    Grassland,
    Savanna,
    Shrubland,
    SandDesert,
    RockyWasteland,
    Badlands,
    Marsh,
    Swamp,
    MangroveSwamp,
    Mountain,
}

impl Biome {
    /// Every biome, in discriminant order (`ALL[b as usize] == b`).
    pub const ALL: [Biome; 20] = [
        Biome::Ocean,
        Biome::FrozenOcean,
        Biome::Lake,
        Biome::Glacier,
        Biome::Tundra,
        Biome::Taiga,
        Biome::ConiferForest,
        Biome::BroadleafForest,
        Biome::TropicalConiferForest,
        Biome::TropicalBroadleafForest,
        Biome::Grassland,
        Biome::Savanna,
        Biome::Shrubland,
        Biome::SandDesert,
        Biome::RockyWasteland,
        Biome::Badlands,
        Biome::Marsh,
        Biome::Swamp,
        Biome::MangroveSwamp,
        Biome::Mountain,
    ];

    /// The biome stored as `v`, if `v` names one.
    pub fn from_u8(v: u8) -> Option<Biome> {
        Biome::ALL.get(v as usize).copied()
    }

    pub fn name(self) -> &'static str {
        match self {
            Biome::Ocean => "Ocean",
            Biome::FrozenOcean => "Frozen ocean",
            Biome::Lake => "Lake",
            Biome::Glacier => "Glacier",
            Biome::Tundra => "Tundra",
            Biome::Taiga => "Taiga",
            Biome::ConiferForest => "Conifer forest",
            Biome::BroadleafForest => "Broadleaf forest",
            Biome::TropicalConiferForest => "Tropical conifer forest",
            Biome::TropicalBroadleafForest => "Tropical broadleaf forest",
            Biome::Grassland => "Grassland",
            Biome::Savanna => "Savanna",
            Biome::Shrubland => "Shrubland",
            Biome::SandDesert => "Sand desert",
            Biome::RockyWasteland => "Rocky wasteland",
            Biome::Badlands => "Badlands",
            Biome::Marsh => "Marsh",
            Biome::Swamp => "Swamp",
            Biome::MangroveSwamp => "Mangrove swamp",
            Biome::Mountain => "Mountain",
        }
    }
}

/// The fields one cell is classified from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellClimate {
    /// Ground (or sea floor) height, m relative to sea level.
    pub height_m: f32,
    /// Mean annual temperature at the ground, °C.
    pub temperature_c: f32,
    /// Annual precipitation, mm/yr.
    pub precip_mm: f32,
    /// DF drainage 0–100 (low = waterlogged).
    pub drainage: u8,
    /// DF salinity 0–100 (sea and closed lakes 100).
    pub salinity: u8,
    /// The cell holds a lake surface.
    pub is_lake: bool,
}

/// DF's rain index 0–100 of an annual precipitation.
pub fn rain_index(precip_mm: f32) -> f32 {
    (precip_mm / RAIN_INDEX_MM).clamp(0.0, 100.0)
}

/// The biome of one cell: water, mountain, frozen, then DF's rainfall ×
/// drainage table with its temperature and salinity variants.
pub fn classify(c: &CellClimate) -> Biome {
    let t = c.temperature_c;
    if c.is_lake {
        return Biome::Lake;
    }
    if c.height_m < 0.0 {
        return if t < FROZEN_C { Biome::FrozenOcean } else { Biome::Ocean };
    }
    if c.height_m >= MOUNTAIN_MIN_M {
        return Biome::Mountain;
    }
    if t < GLACIER_C || (t < FROZEN_C && c.drainage >= GLACIER_MIN_DRAINAGE) {
        return Biome::Glacier;
    }
    if t < FROZEN_C {
        return Biome::Tundra;
    }
    let r = rain_index(c.precip_mm);
    let wet = c.drainage < DRAINAGE_WET;
    let tropical = t >= TROPICAL_MIN_C;
    let swamp = || {
        if tropical && c.salinity >= MANGROVE_MIN_SALINITY && c.drainage <= MANGROVE_MAX_DRAINAGE {
            Biome::MangroveSwamp
        } else {
            Biome::Swamp
        }
    };
    if r < RAIN_DESERT {
        if wet {
            Biome::SandDesert
        } else if c.drainage < DRAINAGE_BADLANDS {
            Biome::RockyWasteland
        } else {
            Biome::Badlands
        }
    } else if r < RAIN_SAVANNA {
        Biome::Grassland
    } else if r < RAIN_SHRUBLAND {
        Biome::Savanna
    } else if r < RAIN_CONIFER {
        if wet { Biome::Marsh } else { Biome::Shrubland }
    } else if r < RAIN_BROADLEAF {
        if wet {
            swamp()
        } else if t < TAIGA_MAX_C {
            Biome::Taiga
        } else if tropical {
            Biome::TropicalConiferForest
        } else {
            Biome::ConiferForest
        }
    } else if wet {
        swamp()
    } else if tropical {
        Biome::TropicalBroadleafForest
    } else {
        Biome::BroadleafForest
    }
}

/// DF's region types: what a contiguous group of cells is named as.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RegionType {
    Ocean,
    Lake,
    Glacier,
    Tundra,
    Mountains,
    Forest,
    Wetland,
    Grassland,
    Hills,
    Desert,
}

impl RegionType {
    pub const ALL: [RegionType; 10] = [
        RegionType::Ocean,
        RegionType::Lake,
        RegionType::Glacier,
        RegionType::Tundra,
        RegionType::Mountains,
        RegionType::Forest,
        RegionType::Wetland,
        RegionType::Grassland,
        RegionType::Hills,
        RegionType::Desert,
    ];

    pub fn name(self) -> &'static str {
        match self {
            RegionType::Ocean => "Ocean",
            RegionType::Lake => "Lake",
            RegionType::Glacier => "Glacier",
            RegionType::Tundra => "Tundra",
            RegionType::Mountains => "Mountains",
            RegionType::Forest => "Forest",
            RegionType::Wetland => "Wetland",
            RegionType::Grassland => "Grassland",
            RegionType::Hills => "Hills",
            RegionType::Desert => "Desert",
        }
    }

    /// Tundra and glacier: DF never joins a frozen region with a thawed one.
    pub fn is_frozen(self) -> bool {
        matches!(self, RegionType::Glacier | RegionType::Tundra)
    }

    /// Sea and lakes.
    pub fn is_water(self) -> bool {
        matches!(self, RegionType::Ocean | RegionType::Lake)
    }
}

/// The region type a cell of biome `b` belongs to. Grassland, savanna and
/// shrubland are hills where drainage is `HILLS_MIN_DRAINAGE` and up; a
/// forest spans taiga to tropical forest.
pub fn region_type(b: Biome, drainage: u8) -> RegionType {
    match b {
        Biome::Ocean | Biome::FrozenOcean => RegionType::Ocean,
        Biome::Lake => RegionType::Lake,
        Biome::Glacier => RegionType::Glacier,
        Biome::Tundra => RegionType::Tundra,
        Biome::Mountain => RegionType::Mountains,
        Biome::Taiga
        | Biome::ConiferForest
        | Biome::BroadleafForest
        | Biome::TropicalConiferForest
        | Biome::TropicalBroadleafForest => RegionType::Forest,
        Biome::Marsh | Biome::Swamp | Biome::MangroveSwamp => RegionType::Wetland,
        Biome::Grassland | Biome::Savanna | Biome::Shrubland => {
            if drainage >= HILLS_MIN_DRAINAGE {
                RegionType::Hills
            } else {
                RegionType::Grassland
            }
        }
        Biome::SandDesert | Biome::RockyWasteland | Biome::Badlands => RegionType::Desert,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Temperate, well-drained, wet lowland: broadleaf forest.
    const BASE: CellClimate =
        CellClimate { height_m: 200.0, temperature_c: 10.0, precip_mm: 1500.0, drainage: 50, salinity: 0, is_lake: false };

    #[test]
    fn classify_table() {
        let rows: &[(CellClimate, Biome)] = &[
            (CellClimate { height_m: -50.0, ..BASE }, Biome::Ocean),
            (CellClimate { height_m: -50.0, temperature_c: -8.0, ..BASE }, Biome::FrozenOcean),
            (CellClimate { is_lake: true, ..BASE }, Biome::Lake),
            (CellClimate { is_lake: true, height_m: -3.0, temperature_c: -20.0, ..BASE }, Biome::Lake),
            (CellClimate { temperature_c: -8.0, drainage: 80, ..BASE }, Biome::Glacier),
            (CellClimate { temperature_c: -15.0, drainage: 20, ..BASE }, Biome::Glacier),
            (CellClimate { temperature_c: -8.0, drainage: 50, ..BASE }, Biome::Tundra),
            (CellClimate { precip_mm: 1100.0, temperature_c: 0.0, ..BASE }, Biome::Taiga),
            (CellClimate { precip_mm: 1100.0, ..BASE }, Biome::ConiferForest),
            (BASE, Biome::BroadleafForest),
            (CellClimate { precip_mm: 1100.0, temperature_c: 24.0, ..BASE }, Biome::TropicalConiferForest),
            (CellClimate { temperature_c: 24.0, ..BASE }, Biome::TropicalBroadleafForest),
            (CellClimate { precip_mm: 200.0, ..BASE }, Biome::Grassland),
            (CellClimate { precip_mm: 400.0, ..BASE }, Biome::Savanna),
            (CellClimate { precip_mm: 800.0, ..BASE }, Biome::Shrubland),
            (CellClimate { precip_mm: 100.0, drainage: 20, ..BASE }, Biome::SandDesert),
            (CellClimate { precip_mm: 100.0, drainage: 50, ..BASE }, Biome::RockyWasteland),
            (CellClimate { precip_mm: 100.0, drainage: 80, ..BASE }, Biome::Badlands),
            (CellClimate { precip_mm: 800.0, drainage: 20, ..BASE }, Biome::Marsh),
            (CellClimate { drainage: 20, ..BASE }, Biome::Swamp),
            (CellClimate { precip_mm: 1100.0, drainage: 20, temperature_c: 24.0, ..BASE }, Biome::Swamp),
            (CellClimate { drainage: 5, salinity: 80, temperature_c: 24.0, ..BASE }, Biome::MangroveSwamp),
            // Mangrove needs all three: salt, tropical, waterlogged.
            (CellClimate { drainage: 5, salinity: 50, temperature_c: 24.0, ..BASE }, Biome::Swamp),
            (CellClimate { drainage: 20, salinity: 80, temperature_c: 24.0, ..BASE }, Biome::Swamp),
            (CellClimate { drainage: 5, salinity: 80, ..BASE }, Biome::Swamp),
            (CellClimate { height_m: 1500.0, ..BASE }, Biome::Mountain),
            (CellClimate { height_m: 1500.0, temperature_c: -20.0, ..BASE }, Biome::Mountain),
        ];
        for (c, want) in rows {
            assert_eq!(classify(c), *want, "{c:?}");
        }
        for b in Biome::ALL {
            assert!(rows.iter().any(|(_, want)| *want == b), "no row for {b:?}");
        }
    }

    /// The rain-index cuts sit where the brief puts them in mm/yr.
    #[test]
    fn rain_cuts_in_millimetres() {
        let at = |precip_mm: f32| classify(&CellClimate { precip_mm, ..BASE });
        assert_eq!(at(159.0), Biome::RockyWasteland);
        assert_eq!(at(160.0), Biome::Grassland);
        assert_eq!(at(1199.0), Biome::ConiferForest);
        assert_eq!(at(1200.0), Biome::BroadleafForest);
        assert_eq!(at(10_000.0), Biome::BroadleafForest);
        assert_eq!(rain_index(-5.0), 0.0);
    }

    #[test]
    fn region_type_hills_needs_high_drainage() {
        for b in [Biome::Grassland, Biome::Savanna, Biome::Shrubland] {
            assert_eq!(region_type(b, 49), RegionType::Grassland, "{b:?}");
            assert_eq!(region_type(b, 50), RegionType::Hills, "{b:?}");
        }
        // Drainage only splits the grassland family.
        for b in [Biome::BroadleafForest, Biome::RockyWasteland, Biome::Marsh, Biome::Tundra] {
            assert_eq!(region_type(b, 10), region_type(b, 90), "{b:?}");
        }
        assert_eq!(region_type(Biome::Taiga, 50), RegionType::Forest);
        assert_eq!(region_type(Biome::TropicalBroadleafForest, 50), RegionType::Forest);
        assert_eq!(region_type(Biome::MangroveSwamp, 0), RegionType::Wetland);
        assert_eq!(region_type(Biome::FrozenOcean, 0), RegionType::Ocean);
    }

    #[test]
    fn biome_bytes_round_trip() {
        for (i, b) in Biome::ALL.iter().enumerate() {
            assert_eq!(*b as usize, i);
            assert_eq!(Biome::from_u8(*b as u8), Some(*b));
        }
        assert_eq!(Biome::from_u8(Biome::ALL.len() as u8), None);
        for (i, t) in RegionType::ALL.iter().enumerate() {
            assert_eq!(*t as usize, i);
        }
    }
}
