//! Building a world's history from nothing: place civs, seed founders, run the years.

use std::cmp::Reverse;

use super::calendar::*;
use super::records::*;
use super::rng::{roll, SALT_PLACE, SALT_SEX};
use super::rules::HISTORY_START_YEAR;
use super::{History, SocietyGrid};

#[derive(Clone, Debug)]
pub struct BootstrapParams {
    pub seed: u64,
    pub civs: u32,
    pub founders_per_civ: u32,
    pub years: u32,
    pub civ_min_spacing_regions: u32,
    pub max_attempts: u32,
}

impl Default for BootstrapParams {
    fn default() -> Self {
        BootstrapParams { seed: 0, civs: 4, founders_per_civ: 30, years: 100, civ_min_spacing_regions: 10, max_attempts: 8 }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BootstrapError {
    /// No region of the map can hold a civ.
    NoHabitableLand,
    /// Every attempt ended with no civ alive.
    AllCivsDied { attempts: u32 },
}

impl std::fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BootstrapError::NoHabitableLand => write!(f, "the map has no habitable land"),
            BootstrapError::AllCivsDied { attempts } => write!(f, "every civ died out in all {attempts} attempts"),
        }
    }
}

impl std::error::Error for BootstrapError {}

fn chebyshev(a: crate::RegionCoords, b: crate::RegionCoords) -> u32 {
    (i64::from(a.x) - i64::from(b.x)).unsigned_abs().max((i64::from(a.z) - i64::from(b.z)).unsigned_abs()) as u32
}

/// Civ home regions: highest capacity first (ties by hashed order), at least `spacing` apart.
fn place_civs(grid: &SocietyGrid, p: &BootstrapParams, attempt: u32) -> Vec<crate::RegionCoords> {
    let at = CalendarTick(u64::from(attempt));
    let mut cands: Vec<(Reverse<u32>, u64, crate::RegionCoords)> = grid
        .regions()
        .filter(|&rc| grid.capacity(rc) > 0)
        .map(|rc| {
            let subject = (u64::from(rc.z as u32) << 32) | u64::from(rc.x as u32);
            (Reverse(grid.capacity(rc)), roll(at, subject, SALT_PLACE), rc)
        })
        .collect();
    cands.sort_by_key(|&(c, r, _)| (c, r));
    let mut placed: Vec<crate::RegionCoords> = Vec::new();
    for (_, _, rc) in cands {
        if placed.len() as u32 >= p.civs {
            break;
        }
        if placed.iter().all(|&o| chebyshev(o, rc) >= p.civ_min_spacing_regions) {
            placed.push(rc);
        }
    }
    placed
}

/// Founders of one civ: couples aged 15-60 and children 0-11 (about a third), linked as families.
fn seed_civ(h: &mut History, civ: CivId, region: crate::RegionCoords, n: u32, start: CalendarTick) {
    let day_ticks = TICKS_PER_DAY;
    let key = |i: u32| (u64::from(civ.0) << 32) | u64::from(i);
    let born_at = |i: u32, min_years: u64, span_years: u64| {
        let age_days = min_years * DAYS_PER_YEAR + roll(start, key(i), SALT_PLACE ^ 0x100) % (span_years * DAYS_PER_YEAR);
        CalendarTick(start.0.saturating_sub(age_days * day_ticks))
    };
    let couples = (n * 2 / 3 / 2).max(u32::from(n >= 2));
    let mut all = Vec::new();
    let mut pairs = Vec::new();
    let mut idx = 0u32;
    for _ in 0..couples {
        let f = h.add_founder(Sex::Female, born_at(idx, 15, 45));
        let m = h.add_founder(Sex::Male, born_at(idx + 1, 15, 45));
        idx += 2;
        h.people.get_mut(&f).unwrap().spouse = Some(m);
        h.people.get_mut(&m).unwrap().spouse = Some(f);
        pairs.push((f, m));
        all.extend([f, m]);
    }
    let kids = n.saturating_sub(couples * 2);
    for k in 0..kids {
        let sex = if roll(start, key(idx), SALT_SEX) & 1 == 0 { Sex::Female } else { Sex::Male };
        let c = h.add_founder(sex, born_at(idx, 0, 12));
        idx += 1;
        if !pairs.is_empty() {
            let (f, m) = pairs[k as usize % pairs.len()];
            let p = h.people.get_mut(&c).unwrap();
            p.mother = Some(f);
            p.father = Some(m);
            h.people.get_mut(&f).unwrap().children.push(c);
            h.people.get_mut(&m).unwrap().children.push(c);
        }
        all.push(c);
    }
    h.found_site(civ, region, &all, None, false);
}

/// Seed civs on `map`, then run `p.years` of history. Play starts at the result's `now()`.
pub fn bootstrap(map: &crate::WorldMap, p: &BootstrapParams) -> Result<History, BootstrapError> {
    let start = CalendarTick::from_days(HISTORY_START_YEAR * DAYS_PER_YEAR);
    for a in 0..p.max_attempts {
        let sub_seed = p.seed ^ roll(CalendarTick(u64::from(a)), u64::from(a), SALT_PLACE);
        let mut h = History::new(sub_seed, start, SocietyGrid::from_world_map(map));
        let homes = place_civs(h.grid(), p, a);
        if homes.is_empty() {
            return Err(BootstrapError::NoHabitableLand);
        }
        for region in homes {
            let civ = h.add_civ(Race::Dwarf);
            seed_civ(&mut h, civ, region, p.founders_per_civ, start);
        }
        h.advance(start.plus_days(u64::from(p.years) * DAYS_PER_YEAR));
        if h.stats().civs_alive >= 1 {
            return Ok(h);
        }
    }
    Err(BootstrapError::AllCivsDied { attempts: p.max_attempts })
}
