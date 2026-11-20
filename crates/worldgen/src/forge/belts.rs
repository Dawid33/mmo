//! Continents, mountain belts by age class, and the plate-stage world score
//! (Phase 2, Task 4).
//! Plan: docs/superpowers/plans/2026-11-20-plates-and-aged-relief-2.md.

use crate::forge::planet::{
    planet_step, OrogenyKind, Planet, OLD_MIN_MY, PLANET_CELLS, YOUNG_MAX_MY,
};
use crate::forge::plate_sim::{self, root_keep};
pub use crate::forge::plate_sim::BELT_MIN_REL;

/// A land component holds at least this share of planet cells to count as a continent.
pub const CONTINENT_MIN_SHARE: f64 = 0.03;
/// A belt holds at least this many cells (about 5 square degrees).
pub const BELT_MIN_CELLS: usize = 10;
/// Sentinel for a cell belonging to no continent.
pub const NO_LABEL: u8 = 255;
/// Sentinel for a cell belonging to no belt.
pub const NO_BELT: u16 = u16::MAX;

/// Age class of a mountain belt on the planet grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgeClass {
    Young,
    Middle,
    Old,
}

/// One continent (4-connected land component holding at least `CONTINENT_MIN_SHARE`).
#[derive(Clone, Debug, PartialEq)]
pub struct Continent {
    pub cells: u32,
    pub belts: Vec<u16>,
}

/// One mountain belt (8-connected belt cells of one age class).
#[derive(Clone, Debug, PartialEq)]
pub struct Belt {
    pub class: AgeClass,
    pub kind: u8,
    pub cells: u32,
    pub continent: u8,
    pub median_age_my: f32,
    pub depth_deg: f32,
}

/// Plate-stage summary used to score an attempt (Ruling 9).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanetScore {
    pub continents: u32,
    pub continents_without_belt: u32,
    pub young_belts: u32,
    pub middle_belts: u32,
    pub old_belts: u32,
    pub land_share: f64,
}

impl PlanetScore {
    /// Whether this planet passes all three plate-stage criteria.
    pub fn ok(&self) -> bool {
        self.continents >= 2 && self.continents_without_belt == 0 && self.young_belts >= 1 && self.old_belts >= 1
    }

    /// Number of criteria met (`0..=3`), used to pick the best failing attempt.
    pub fn met(&self) -> u32 {
        (self.continents >= 2) as u32
            + (self.continents > 0 && self.continents_without_belt == 0) as u32
            + (self.young_belts >= 1 && self.old_belts >= 1) as u32
    }
}

fn is_orogenic_kind(kind: u8) -> bool {
    kind == OrogenyKind::Arc as u8
        || kind == OrogenyKind::Andean as u8
        || kind == OrogenyKind::Collision as u8
        || kind == OrogenyKind::Plateau as u8
}

fn age_class(age_my: f32) -> AgeClass {
    let age = f64::from(age_my);
    if age < YOUNG_MAX_MY {
        AgeClass::Young
    } else if age < OLD_MIN_MY {
        AgeClass::Middle
    } else {
        AgeClass::Old
    }
}

fn flood_components<const EIGHT: bool, F>(include: F) -> Vec<Vec<usize>>
where
    F: Fn(usize, usize) -> bool,
{
    const D4: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
    const D8: [(i32, i32); 8] = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)];

    let mut seen = vec![false; PLANET_CELLS];
    let mut comps = Vec::new();
    for start in 0..PLANET_CELLS {
        if seen[start] || !include(start, start) {
            continue;
        }
        seen[start] = true;
        let mut comp = vec![start];
        let mut head = 0;
        while head < comp.len() {
            let c = comp[head];
            head += 1;
            let nbrs: &[(i32, i32)] = if EIGHT { &D8 } else { &D4 };
            for &(dx, dz) in nbrs {
                let Some(n) = planet_step(c, dx, dz) else { continue };
                if !seen[n] && include(start, n) {
                    seen[n] = true;
                    comp.push(n);
                }
            }
        }
        comps.push(comp);
    }
    comps
}

fn find_land_components(land: &[bool], min_share: f64) -> Vec<Vec<usize>> {
    assert_eq!(land.len(), PLANET_CELLS);
    let min_cells = min_share * PLANET_CELLS as f64;
    let mut comps = flood_components::<false, _>(|_, c| land[c]);
    comps.retain(|c| c.len() as f64 >= min_cells);
    comps
}

/// Count 4-connected components of `land` (x wraps) holding at least `min_share` of the grid.
pub fn land_components_over(land: &[bool], min_share: f64) -> u32 {
    find_land_components(land, min_share).len() as u32
}

fn argmax_count(counts: &[u32]) -> usize {
    let mut best_idx = 0;
    let mut best_count = 0;
    for (idx, &count) in counts.iter().enumerate() {
        if count > best_count {
            best_count = count;
            best_idx = idx;
        }
    }
    best_idx
}

fn build_belt(planet: &Planet, class: AgeClass, cells: &[usize], depth: &[f64]) -> Belt {
    let mut kind_counts = [0u32; 7];
    let mut cont_counts = [0u32; 256];
    let mut ages = Vec::with_capacity(cells.len());
    let mut max_depth = 0.0_f64;

    for &c in cells {
        let k = usize::from(planet.kind[c]);
        if k < kind_counts.len() {
            kind_counts[k] += 1;
        }
        cont_counts[usize::from(planet.continent[c])] += 1;
        ages.push(planet.orogeny_age_my[c]);
        max_depth = max_depth.max(depth[c]);
    }
    ages.sort_by(f32::total_cmp);
    let mid = ages.len() / 2;
    let median_age_my = if ages.len().is_multiple_of(2) { 0.5 * (ages[mid - 1] + ages[mid]) } else { ages[mid] };

    Belt {
        class,
        kind: argmax_count(&kind_counts) as u8,
        cells: cells.len() as u32,
        continent: argmax_count(&cont_counts) as u8,
        median_age_my,
        depth_deg: max_depth as f32,
    }
}

/// Label continents and belts on `planet`, compute `belt_depth_deg`, `score` and `ok`.
pub fn label(planet: &mut Planet) {
    let mut cont_comps = find_land_components(&planet.land, CONTINENT_MIN_SHARE);
    cont_comps.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a[0].cmp(&b[0])));
    planet.continent.fill(NO_LABEL);
    planet.continents = Vec::with_capacity(cont_comps.len());
    for (idx, comp) in cont_comps.iter().enumerate() {
        assert!(idx < usize::from(NO_LABEL), "more continents than label slots");
        for &c in comp {
            planet.continent[c] = idx as u8;
        }
        planet.continents.push(Continent { cells: comp.len() as u32, belts: Vec::new() });
    }

    let cell_class: Vec<Option<AgeClass>> = (0..PLANET_CELLS)
        .map(|c| {
            let active = planet.land[c]
                && is_orogenic_kind(planet.kind[c])
                && f64::from(planet.strength[c]) * root_keep(f64::from(planet.orogeny_age_my[c])) >= BELT_MIN_REL;
            active.then(|| age_class(planet.orogeny_age_my[c]))
        })
        .collect();

    let mut belt_comps = flood_components::<true, _>(|start, c| cell_class[c].is_some() && cell_class[c] == cell_class[start]);
    belt_comps.retain(|comp| comp.len() >= BELT_MIN_CELLS);
    belt_comps.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a[0].cmp(&b[0])));

    planet.belt.fill(NO_BELT);
    for (idx, comp) in belt_comps.iter().enumerate() {
        assert!(idx < usize::from(NO_BELT), "more belts than label slots");
        for &c in comp {
            planet.belt[c] = idx as u16;
        }
    }

    let depth = plate_sim::chamfer_deg(|c| planet.belt[c] == NO_BELT);
    planet.belts = Vec::with_capacity(belt_comps.len());
    let (mut young_belts, mut middle_belts, mut old_belts) = (0u32, 0u32, 0u32);
    for (idx, comp) in belt_comps.iter().enumerate() {
        let class = cell_class[comp[0]].expect("belt cell must have class");
        match class {
            AgeClass::Young => young_belts += 1,
            AgeClass::Middle => middle_belts += 1,
            AgeClass::Old => old_belts += 1,
        }
        let belt = build_belt(planet, class, comp, &depth);
        if belt.continent != NO_LABEL {
            planet.continents[usize::from(belt.continent)].belts.push(idx as u16);
        }
        planet.belts.push(belt);
    }

    let young_mid = planet
        .belts
        .iter()
        .filter(|b| matches!(b.class, AgeClass::Young | AgeClass::Middle))
        .map(|b| f64::from(b.depth_deg))
        .reduce(f64::max);
    let any_belt = planet.belts.iter().map(|b| f64::from(b.depth_deg)).reduce(f64::max);
    planet.belt_depth_deg = young_mid.or(any_belt).unwrap_or(4.0);

    let land_cells = planet.land.iter().filter(|&&b| b).count();
    let continents_without_belt = planet.continents.iter().filter(|c| c.belts.is_empty()).count() as u32;
    planet.score = PlanetScore {
        continents: planet.continents.len() as u32,
        continents_without_belt,
        young_belts,
        middle_belts,
        old_belts,
        land_share: land_cells as f64 / PLANET_CELLS as f64,
    };
    planet.ok = planet.score.ok();
}
