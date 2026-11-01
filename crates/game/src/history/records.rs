//! Ids and records of the world history.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::calendar::CalendarTick;
use crate::StateHash;

macro_rules! id_type {
    ($($(#[$m:meta])* $name:ident),*) => {$(
        $(#[$m])*
        #[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize, StateHash)]
        pub struct $name(pub u32);
    )*};
}
id_type!(PersonId, SiteId, CivId, JourneyId);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Race {
    Dwarf,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Sex {
    Female,
    Male,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum DeathCause {
    OldAge,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Location {
    /// Created but not yet at a site or on a journey: founders between `add_founder` and `found_site`.
    Unplaced,
    AtSite(SiteId),
    Journey(JourneyId),
    /// Embodied in a running (or stopping) region: a body there stands for them, and
    /// [`Person::home_location`] keeps where they are in the abstract history.
    InRegion(crate::RegionCoords),
    /// An embark party that found nowhere to settle: it stays in `region` and takes no further part.
    Wandering(crate::RegionCoords),
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Person {
    pub id: PersonId,
    pub race: Race,
    pub sex: Sex,
    pub name: String,
    pub born: CalendarTick,
    pub died: Option<(CalendarTick, DeathCause)>,
    pub mother: Option<PersonId>,
    pub father: Option<PersonId>,
    pub spouse: Option<PersonId>,
    pub children: Vec<PersonId>,
    /// Citizenship.
    pub site: Option<SiteId>,
    pub location: Location,
    /// While `location` is `InRegion`: what they are in the abstract history (`AtSite`,
    /// `Journey`, `Wandering`), restored when the region parks or they leave it.
    /// `Unplaced` otherwise.
    pub home_location: Location,
    pub last_birth: Option<CalendarTick>,
}

impl Person {
    pub fn newborn(
        id: PersonId,
        sex: Sex,
        born: CalendarTick,
        mother: Option<PersonId>,
        father: Option<PersonId>,
        site: Option<SiteId>,
        location: Location,
    ) -> Person {
        Person {
            id,
            race: Race::Dwarf,
            sex,
            name: person_name(id),
            born,
            died: None,
            mother,
            father,
            spouse: None,
            children: Vec::new(),
            site,
            location,
            home_location: Location::Unplaced,
            last_birth: None,
        }
    }

    pub fn alive(&self) -> bool {
        self.died.is_none()
    }

    /// Where they are in the abstract history: `home_location` while embodied, else `location`.
    pub fn abstract_location(&self) -> Location {
        match self.location {
            Location::InRegion(_) => self.home_location,
            l => l,
        }
    }

    pub fn age_years(&self, now: CalendarTick) -> u32 {
        now.years_since(self.born)
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Site {
    pub id: SiteId,
    pub civ: CivId,
    pub name: String,
    pub region: crate::RegionCoords,
    pub founded: CalendarTick,
    pub founders: Vec<PersonId>,
    pub from: Option<SiteId>,
    pub abandoned: Option<CalendarTick>,
    pub capacity: u32,
    pub members: BTreeSet<PersonId>,
    pub player_founded: bool,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Civ {
    pub id: CivId,
    pub race: Race,
    pub name: String,
    pub sites: Vec<SiteId>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Purpose {
    Migrate { to: SiteId },
    Embark { civ: CivId, from: SiteId },
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Journey {
    pub id: JourneyId,
    pub people: Vec<PersonId>,
    pub purpose: Purpose,
    pub route: Vec<crate::RegionCoords>,
    pub departed: CalendarTick,
    /// Base ticks to cross one region at cost 1000 (`walk_ticks_per_region`).
    pub leg_ticks: u64,
    /// `arrivals[i]`: ticks after `departed` at which the party reaches `route[i + 1]`.
    pub arrivals: Vec<u64>,
    /// Index into `route` of the current region.
    pub leg: u32,
    /// The running region the party is being played in: no abstract leg is queued while
    /// it is set; the region's people walk it, and hand it back on a crossing or a park.
    pub held_in: Option<crate::RegionCoords>,
}

pub fn person_name(id: PersonId) -> String {
    format!("Dwarf {}", id.0)
}

pub fn site_name(id: SiteId) -> String {
    format!("Site {}", id.0)
}

pub fn civ_name(id: CivId) -> String {
    format!("Civ {}", id.0)
}
