//! Deterministic, threadless world history: people, sites, civs and journeys on one calendar.

mod chain;
mod handover;
mod play;
pub mod bootstrap;
pub mod stats;
mod journey;
pub mod calendar;
pub mod grid;
pub mod log;
pub mod persist;
pub mod records;
pub mod rng;
pub mod rules;

pub use calendar::*;
pub use chain::EmbarkRefusal;
pub use handover::Directive;
pub use grid::*;
pub use records::*;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use log::{LogEntry, LogKind};

/// A queued event of the history.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub(crate) enum Step {
    SiteMonth(SiteId),
    Death(PersonId),
    /// The end of leg `leg` (the walk out of `route[leg]`) of `journey`; stale, and
    /// ignored, once the journey is on another leg.
    JourneyLeg { journey: JourneyId, leg: u32 },
}

/// The world's history: people, sites, civs and journeys advanced by a calendar-ordered queue.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct History {
    pub seed: u64,
    pub now: CalendarTick,
    pub people: BTreeMap<PersonId, Person>,
    pub sites: BTreeMap<SiteId, Site>,
    pub civs: BTreeMap<CivId, Civ>,
    pub journeys: BTreeMap<JourneyId, Journey>,
    pub(crate) queue: BTreeMap<(CalendarTick, u64), Step>,
    pub(crate) next_seq: u64,
    /// Next person, site, civ, journey id.
    pub(crate) next_ids: [u32; 4],
    pub log: Vec<LogEntry>,
    #[serde(skip)]
    grid: Option<SocietyGrid>,
    /// Regions running now (the manager's view). Not saved: a load starts with none, and
    /// [`History::after_load`] hands every body and held journey back to the abstract history.
    #[serde(skip)]
    running: BTreeSet<crate::RegionCoords>,
    /// Regions shutting down, not yet parked: steps acting on their people wait.
    #[serde(skip)]
    stopping: BTreeSet<crate::RegionCoords>,
    /// Decided but not yet taken by [`History::take_directives`].
    #[serde(skip)]
    directives: Vec<Directive>,
    /// Everyone who may be a stray citizen (`Wandering` with a site): a superset, checked
    /// on use, filled where hand-over makes strays and rebuilt by [`History::after_load`].
    #[serde(skip)]
    stray_candidates: BTreeSet<PersonId>,
    /// When each held journey was last held (`Journey::held_in` set), for
    /// [`rules::HELD_TIMEOUT_LEGS`]. Not saved: a load releases every hold.
    #[serde(skip)]
    held_since: BTreeMap<JourneyId, CalendarTick>,
    /// Log lines about sites in live regions, for the clients playing there; drained by
    /// [`History::take_notices`]. Nothing is kept for regions not running (nobody plays
    /// them), so an unwatched history never builds these up.
    #[serde(skip)]
    notices: Vec<(crate::RegionCoords, String)>,
}

impl History {
    pub fn new(seed: u64, start: CalendarTick, grid: SocietyGrid) -> Self {
        History {
            seed,
            now: start,
            people: BTreeMap::new(),
            sites: BTreeMap::new(),
            civs: BTreeMap::new(),
            journeys: BTreeMap::new(),
            queue: BTreeMap::new(),
            next_seq: 0,
            next_ids: [0; 4],
            log: Vec::new(),
            grid: Some(grid),
            running: BTreeSet::new(),
            stopping: BTreeSet::new(),
            directives: Vec::new(),
            stray_candidates: BTreeSet::new(),
            held_since: BTreeMap::new(),
            notices: Vec::new(),
        }
    }

    /// Re-attach the society grid after loading (it is not saved).
    pub fn set_grid(&mut self, grid: SocietyGrid) {
        self.grid = Some(grid);
    }

    pub fn grid(&self) -> &SocietyGrid {
        self.grid.as_ref().expect("History::set_grid was not called after load")
    }

    pub fn now(&self) -> CalendarTick {
        self.now
    }

    /// The history's random number for (`at`, `subject`, `salt`): [`rng::roll`] with
    /// `seed` folded in. Every rule draws through this (or [`History::chance`]).
    pub fn roll(&self, at: CalendarTick, subject: u64, salt: u64) -> u64 {
        rng::seeded_roll(self.seed, at, subject, salt)
    }

    /// True with probability `per_mille / 1000`, drawn from [`History::roll`].
    pub fn chance(&self, at: CalendarTick, subject: u64, salt: u64, per_mille: u32) -> bool {
        self.roll(at, subject, salt) % 1000 < u64::from(per_mille)
    }

    pub(crate) fn schedule(&mut self, at: CalendarTick, step: Step) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.queue.insert((at, seq), step);
    }

    pub(crate) fn log_event(&mut self, kind: LogKind) {
        let regions = self.notice_regions(&kind);
        let entry = LogEntry { at: self.now, kind };
        if !regions.is_empty() {
            let line = entry.describe(self);
            self.notices.extend(regions.into_iter().map(|r| (r, line.clone())));
        }
        self.log.push(entry);
    }

    pub fn add_civ(&mut self, race: Race) -> CivId {
        let id = CivId(self.next_ids[2]);
        self.next_ids[2] += 1;
        self.civs.insert(id, Civ { id, race, name: civ_name(id), sites: Vec::new() });
        id
    }

    /// A person with no parents, at no site yet. Dies of old age at the rolled age
    /// (on the first `advance` if that date has already passed).
    pub fn add_founder(&mut self, sex: Sex, born: CalendarTick) -> PersonId {
        let id = PersonId(self.next_ids[0]);
        self.next_ids[0] += 1;
        self.people.insert(id, Person::newborn(id, sex, born, None, None, None, Location::Unplaced));
        let at = born.plus_days(rules::death_age_days(self.seed, id, born)).max(self.now);
        self.schedule(at, Step::Death(id));
        id
    }

    pub fn found_site(
        &mut self,
        civ: CivId,
        region: crate::RegionCoords,
        founders: &[PersonId],
        from: Option<SiteId>,
        player_founded: bool,
    ) -> SiteId {
        for f in founders {
            debug_assert!(
                self.people
                    .get(f)
                    .is_none_or(|p| matches!(p.abstract_location(), Location::Unplaced | Location::Journey(_))),
                "founder {f:?} is already placed: {:?}",
                self.people.get(f).map(|p| p.abstract_location())
            );
        }
        let id = SiteId(self.next_ids[1]);
        self.next_ids[1] += 1;
        let capacity = self.grid().capacity(region);
        let members = founders.iter().copied().collect();
        self.sites.insert(
            id,
            Site {
                id,
                civ,
                name: site_name(id),
                region,
                founded: self.now,
                founders: founders.to_vec(),
                from,
                abandoned: None,
                capacity,
                members,
                player_founded,
            },
        );
        if let Some(c) = self.civs.get_mut(&civ) {
            c.sites.push(id);
        }
        for f in founders {
            if let Some(p) = self.people.get_mut(f) {
                p.site = Some(id);
            }
            self.set_abstract_location(*f, Location::AtSite(id));
        }
        self.log_event(LogKind::SiteFounded { site: id, civ, founders: founders.to_vec(), from });
        self.schedule(self.now.plus_days(DAYS_PER_MONTH), Step::SiteMonth(id));
        // Founded into a running region (an embark into a played region): bodies at once.
        self.embody_site(id);
        id
    }

    /// Run every queued step due by `to`, in (time, insertion) order.
    pub fn advance(&mut self, to: CalendarTick) {
        while let Some((&(at, seq), _)) = self.queue.first_key_value() {
            if at > to {
                break;
            }
            let step = self.queue.remove(&(at, seq)).unwrap();
            self.now = self.now.max(at);
            match step {
                Step::SiteMonth(s) => self.on_site_month(s),
                Step::Death(p) => self.on_death(p),
                Step::JourneyLeg { journey, leg } => self.on_journey_leg(journey, leg),
            }
        }
        self.now = self.now.max(to);
        self.release_stuck_journeys();
    }

    pub fn person(&self, id: PersonId) -> Option<&Person> {
        self.people.get(&id)
    }
    pub fn site(&self, id: SiteId) -> Option<&Site> {
        self.sites.get(&id)
    }
    pub fn civ(&self, id: CivId) -> Option<&Civ> {
        self.civs.get(&id)
    }
    pub fn journey(&self, id: JourneyId) -> Option<&Journey> {
        self.journeys.get(&id)
    }
    pub fn people(&self) -> impl Iterator<Item = &Person> {
        self.people.values()
    }
    pub fn sites(&self) -> impl Iterator<Item = &Site> {
        self.sites.values()
    }
    pub fn civs(&self) -> impl Iterator<Item = &Civ> {
        self.civs.values()
    }
    pub fn log(&self) -> &[LogEntry] {
        &self.log
    }

    /// Queued `SiteMonth` steps of `site`.
    pub fn pending_site_months(&self, site: SiteId) -> usize {
        self.queue.values().filter(|s| matches!(s, Step::SiteMonth(x) if *x == site)).count()
    }

    /// Queued `Death` steps of people who are already dead.
    pub fn pending_deaths_of_dead(&self) -> usize {
        self.queue
            .values()
            .filter(|s| matches!(s, Step::Death(p) if self.people.get(p).is_some_and(|q| !q.alive())))
            .count()
    }

    /// Living members of `site`, by id.
    pub fn living_members(&self, site: SiteId) -> Vec<PersonId> {
        self.sites
            .get(&site)
            .map(|s| s.members.iter().copied().filter(|p| self.people.get(p).is_some_and(Person::alive)).collect())
            .unwrap_or_default()
    }
}
