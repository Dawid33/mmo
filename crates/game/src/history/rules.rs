//! Named tunables and integer rule math for world history.

use super::History;
use super::calendar::{CalendarTick, DAYS_PER_YEAR};
use super::records::PersonId;
use super::rng::{SALT_DEATH_AGE, seeded_roll};

/// The year the bootstrapped history starts at.
pub const HISTORY_START_YEAR: u64 = 250;

pub const ADULT_AGE_YEARS: u32 = 12;
pub const MARRY_PER_MILLE_MONTHLY: u32 = 40;
pub const FERTILE_MAX_AGE_YEARS: u32 = 80;
pub const BIRTH_PER_MILLE_MONTHLY: u32 = 25;
pub const BIRTH_MIN_GAP_DAYS: u64 = DAYS_PER_YEAR;
pub const OLD_AGE_MIN_YEARS: u32 = 80;
pub const OLD_AGE_MAX_YEARS: u32 = 160;
/// members/capacity (per mille) at which a site is crowded.
pub const CROWDED_PER_MILLE: u32 = 900;
/// Per crowded site per month.
pub const MIGRATE_PER_MILLE_MONTHLY: u32 = 150;
/// Crowded, no civ site with room.
pub const EMBARK_PER_MILLE_MONTHLY: u32 = 60;
/// Crowded but a civ site has room.
pub const EMBARK_BASE_PER_MILLE_MONTHLY: u32 = 2;
pub const EMBARK_PARTY_MIN: u32 = 7;
pub const EMBARK_PARTY_MAX: u32 = 20;
/// Chebyshev distance between sites, for embarks.
pub const EMBARK_MIN_DISTANCE_REGIONS: u32 = 4;
pub const EMBARK_MAX_SEARCH_REGIONS: u32 = 16;
pub const CIV_POPULATION_CAP: u32 = 2000;
pub const CAPACITY_BASE: u32 = 40;
pub const CAPACITY_PER_RIVER: u32 = 10;
pub const CAPACITY_LAKE: u32 = 15;
pub const CAPACITY_MAX: u32 = 200;
pub const CAPACITY_RELIEF_PENALTY_PER_100M: u32 = 10;
pub const MIN_DRY_FRACTION_PER_MILLE: u32 = 300;

/// Capacity factor of a tile's dominant biome, per mille: nothing lives on
/// water or ice, deserts and other harsh ground hold half.
pub fn biome_capacity_per_mille(biome: crate::Biome) -> u32 {
    use crate::Biome::*;
    match biome {
        Ocean | FrozenOcean | Lake | Glacier => 0,
        Tundra | SandDesert | RockyWasteland | Badlands => 500,
        _ => 1000,
    }
}

/// Blocks either way around the landing column that caught-up site people spread over.
pub const SPAWN_SPREAD_BLOCKS: u32 = 4;
/// Per mille either way around a party's point on the edge that its members spread over.
pub const EDGE_SPREAD_PER_MILLE: u32 = 20;
/// Days a step that acts on people in a stopping region waits before it is tried again.
pub const STOPPING_DEFER_DAYS: u64 = 1;
/// A journey held in a running region for this many of its current leg's durations is
/// released: its bodies there are despawned and it goes on abstractly (a walk out that
/// found no path, or a walker that gave up, must not freeze it in a region that never parks).
pub const HELD_TIMEOUT_LEGS: u64 = 3;

/// Blocks either way around the landing column that `members` caught-up people spread
/// over: [`SPAWN_SPREAD_BLOCKS`], or `ceil(sqrt(members)) * 2` for a bigger crowd.
pub fn spawn_spread_blocks(members: u32) -> u32 {
    let r = members.isqrt();
    let ceil = if r * r < members { r + 1 } else { r };
    SPAWN_SPREAD_BLOCKS.max(ceil * 2)
}

/// Age at death by old age, in days: uniform in `[OLD_AGE_MIN_YEARS, OLD_AGE_MAX_YEARS]` years,
/// drawn from the history's `seed`.
pub fn death_age_days(seed: u64, person: PersonId, born: CalendarTick) -> u64 {
    let min = u64::from(OLD_AGE_MIN_YEARS) * DAYS_PER_YEAR;
    let span = u64::from(OLD_AGE_MAX_YEARS - OLD_AGE_MIN_YEARS) * DAYS_PER_YEAR + 1;
    min + seeded_roll(seed, born, u64::from(person.0), SALT_DEATH_AGE) % span
}

/// members/capacity per mille (capacity 0 reads as 1000 or more).
pub fn crowding_per_mille(members: u32, capacity: u32) -> u32 {
    if capacity == 0 {
        return 1000;
    }
    (u64::from(members) * 1000 / u64::from(capacity)).min(u64::from(u32::MAX)) as u32
}

/// Monthly birth chance per fertile couple, falling linearly to 0 at capacity.
pub fn birth_per_mille(members: u32, capacity: u32) -> u32 {
    let crowd = crowding_per_mille(members, capacity);
    BIRTH_PER_MILLE_MONTHLY * 1000u32.saturating_sub(crowd) / 1000
}

/// Abstract travel time across one region in calendar ticks, derived from the
/// physical walk speed so abstract and played travel agree:
/// `REGION_BLOCKS` m / `WALK_SPEED` m/s seconds, times 1000 / `TICK_RATE` ms
/// per tick (integer, speed taken in mm/s).
pub fn walk_ticks_per_region() -> u64 {
    let mm_per_s = (crate::movement::WALK_SPEED * 1000.0).round() as u64;
    let region_mm = crate::REGION_BLOCKS as u64 * 1000;
    region_mm * 1000 / (mm_per_s * crate::TICK_RATE)
}

/// `person`, their living spouse and their living children under `ADULT_AGE_YEARS`, all
/// present at the person's site (a traveller has no household), in id order.
pub fn household_of(h: &History, person: PersonId) -> Vec<PersonId> {
    let Some(p) = h.people.get(&person).filter(|p| p.alive()) else {
        return Vec::new();
    };
    let Some(site) = p.site.and_then(|s| h.sites.get(&s)) else {
        return Vec::new();
    };
    if !site.members.contains(&person) {
        return Vec::new();
    }
    let present =
        |id: &PersonId| site.members.contains(id) && h.people.get(id).is_some_and(|q| q.alive());
    let mut out = std::collections::BTreeSet::new();
    out.insert(person);
    let mut parents = vec![person];
    if let Some(sp) = p.spouse.filter(|s| present(s)) {
        out.insert(sp);
        parents.push(sp);
    }
    for parent in parents {
        for c in &h.people[&parent].children {
            if present(c) && h.people[c].age_years(h.now) < ADULT_AGE_YEARS {
                out.insert(*c);
            }
        }
    }
    out.into_iter().collect()
}
