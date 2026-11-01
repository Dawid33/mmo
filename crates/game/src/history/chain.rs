//! Event handlers: deaths, and the monthly marriages and births of a site.

use super::calendar::*;
use super::log::LogKind;
use super::records::*;
use super::rng::*;
use super::rules::*;
use super::grid::{route, ROUTE_MAX_NODES};
use super::History;

/// Why a region cannot take a new site, or (the player's embark and return) why the
/// request is refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum EmbarkRefusal {
    OffMap,
    NotHabitable,
    Occupied,
    TooClose { site: SiteId },
    /// The world has no history.
    NoHistory,
    /// The player already plays a fortress: retire it first.
    AlreadyHasFortress,
    /// The party is empty or bigger than `EMBARK_PLAYER_PARTY_MAX`.
    PartySize,
    /// Dead, unknown, of another civ, travelling, embodied in a running region, or
    /// listed twice.
    UnavailablePerson(PersonId),
    /// Return: not a living player-founded site.
    NotPlayerFounded,
    /// Return: another connected player plays it.
    InUse,
}

fn chebyshev(a: crate::RegionCoords, b: crate::RegionCoords) -> u32 {
    (i64::from(a.x) - i64::from(b.x)).unsigned_abs().max((i64::from(a.z) - i64::from(b.z)).unsigned_abs()) as u32
}

impl History {
    /// Old age comes for `id`.
    pub(crate) fn on_death(&mut self, id: PersonId) {
        if self.people.get(&id).is_some_and(|p| p.alive()) {
            if self.in_stopping_region(id) {
                let at = self.deferred();
                self.schedule(at, super::Step::Death(id));
                return;
            }
            if let Some(rc) = self.embodied_in(id) {
                self.emit_despawn(rc, id);
            }
        }
        self.record_death(id, DeathCause::OldAge);
    }

    /// Kill `person` now: widow the spouse, drop them from their site, log it, and
    /// abandon the site if nobody is left. A person already dead is left alone.
    pub fn record_death(&mut self, person: PersonId, cause: DeathCause) {
        let now = self.now;
        let Some(p) = self.people.get_mut(&person) else { return };
        if !p.alive() {
            return;
        }
        p.died = Some((now, cause));
        let spouse = p.spouse;
        let on_journey = match p.abstract_location() {
            Location::Journey(j) => Some(j),
            _ => None,
        };
        // A traveller is not a member of any site; the site has nothing to abandon.
        let site = if on_journey.is_some() { None } else { p.site };
        if let Some(sp) = spouse.and_then(|s| self.people.get_mut(&s)) {
            if sp.spouse == Some(person) {
                sp.spouse = None;
            }
        }
        self.log_event(LogKind::Died { person, cause });
        if let Some(j) = on_journey {
            self.remove_traveller(j, person);
        }
        if let Some(sid) = site {
            let empty = match self.sites.get_mut(&sid) {
                Some(s) => {
                    s.members.remove(&person);
                    s.abandoned.is_none() && s.members.is_empty()
                }
                None => false,
            };
            if empty {
                self.abandon_site(sid);
            }
        }
    }

    pub(crate) fn abandon_site(&mut self, sid: SiteId) {
        let now = self.now;
        if let Some(s) = self.sites.get_mut(&sid) {
            if s.abandoned.is_none() {
                s.abandoned = Some(now);
                self.log_event(LogKind::SiteAbandoned { site: sid });
            }
        }
    }

    pub(crate) fn on_site_month(&mut self, sid: SiteId) {
        match self.sites.get(&sid) {
            Some(s) if s.abandoned.is_none() => {}
            _ => return,
        }
        // Its people are in a region being shut down: try again once it has parked.
        if self.sites[&sid].members.iter().any(|p| self.in_stopping_region(*p)) {
            let at = self.deferred();
            self.schedule(at, super::Step::SiteMonth(sid));
            return;
        }
        self.marriages(sid);
        self.births(sid);
        self.consider_leaving(sid);
        if self.sites.get(&sid).is_some_and(|s| s.abandoned.is_none()) {
            self.schedule(self.now.plus_days(DAYS_PER_MONTH), super::Step::SiteMonth(sid));
        }
    }

    /// A crowded site sends a household to a civ site with room, or an embark party to found a new one.
    pub(crate) fn consider_leaving(&mut self, sid: SiteId) {
        let (civ, region, crowd) = {
            let s = &self.sites[&sid];
            (s.civ, s.region, crowding_per_mille(s.members.len() as u32, s.capacity))
        };
        if crowd < CROWDED_PER_MILLE {
            return;
        }
        // The index roll below uses the plain site id, so the chance is drawn on another subject.
        let key = u64::from(sid.0) | (1 << 32);
        let now = self.now;
        match self.least_crowded_reachable(civ, region, Some(sid), Some(CROWDED_PER_MILLE)) {
            Some(to) => {
                if self.chance(now, key, SALT_MIGRATE, MIGRATE_PER_MILLE_MONTHLY) {
                    self.send_household(sid, to);
                }
                if self.chance(now, key, SALT_EMBARK, EMBARK_BASE_PER_MILLE_MONTHLY) {
                    self.send_embark_party(sid);
                }
            }
            None => {
                if self.chance(now, key, SALT_EMBARK, EMBARK_PER_MILLE_MONTHLY) {
                    self.send_embark_party(sid);
                }
            }
        }
    }

    /// The least crowded living site of `civ` (lowest id on ties) with a route from `from`,
    /// skipping `exclude` and, when given, any site crowded to `max_crowding` or more.
    pub(crate) fn least_crowded_reachable(
        &self,
        civ: CivId,
        from: crate::RegionCoords,
        exclude: Option<SiteId>,
        max_crowding: Option<u32>,
    ) -> Option<SiteId> {
        let mut cands: Vec<(u32, SiteId)> = self
            .sites
            .values()
            .filter(|s| s.civ == civ && s.abandoned.is_none() && Some(s.id) != exclude)
            .map(|s| (crowding_per_mille(s.members.len() as u32, s.capacity), s.id))
            .filter(|(c, _)| max_crowding.is_none_or(|m| *c < m))
            .collect();
        cands.sort();
        cands
            .into_iter()
            .filter(|(_, id)| self.same_landmass(from, self.sites[id].region))
            .find(|(_, id)| route(self.grid(), from, self.sites[id].region, ROUTE_MAX_NODES).is_some())
            .map(|(_, id)| id)
    }

    fn living_adults(&self, sid: SiteId) -> Vec<PersonId> {
        self.sites[&sid]
            .members
            .iter()
            .copied()
            .filter(|p| self.people.get(p).is_some_and(|q| q.alive() && q.age_years(self.now) >= ADULT_AGE_YEARS))
            .collect()
    }

    fn send_household(&mut self, sid: SiteId, to: SiteId) {
        let adults = self.living_adults(sid);
        if adults.is_empty() {
            return;
        }
        let start = (self.roll(self.now, u64::from(sid.0), SALT_MIGRATE) % adults.len() as u64) as usize;
        let household = household_of(self, adults[start]);
        let to_region = self.sites[&to].region;
        self.start_journey(household, sid, to_region, Purpose::Migrate { to });
    }

    fn send_embark_party(&mut self, sid: SiteId) {
        let Some(s) = self.sites.get(&sid).filter(|s| s.abandoned.is_none()) else { return };
        let (civ, region) = (s.civ, s.region);
        let adults = self.living_adults(sid);
        if adults.is_empty() {
            return;
        }
        let start = (self.roll(self.now, u64::from(sid.0), SALT_EMBARK) % adults.len() as u64) as usize;
        let order = std::iter::once(adults[start]).chain(adults.iter().copied().filter(|a| *a != adults[start]));
        let mut party: Vec<PersonId> = Vec::new();
        let (mut women, mut men) = (false, false);
        for a in order {
            if party.contains(&a) {
                continue;
            }
            let household: Vec<PersonId> = household_of(self, a).into_iter().filter(|p| !party.contains(p)).collect();
            if party.len() + household.len() > EMBARK_PARTY_MAX as usize {
                continue;
            }
            for p in &household {
                let q = &self.people[p];
                if q.age_years(self.now) >= ADULT_AGE_YEARS {
                    match q.sex {
                        Sex::Female => women = true,
                        Sex::Male => men = true,
                    }
                }
            }
            party.extend(household);
            if party.len() >= EMBARK_PARTY_MIN as usize && women && men {
                break;
            }
        }
        if party.len() < EMBARK_PARTY_MIN as usize || !(women && men) {
            return;
        }
        if let Some(target) = self.embark_target(civ, region) {
            self.start_journey(party, sid, target, Purpose::Embark { civ, from: sid });
        }
    }

    /// Whether a new site may be founded in `region`; plan 3's player embark shares the rule.
    pub fn embark_allowed(&self, region: crate::RegionCoords) -> Result<(), EmbarkRefusal> {
        let grid = self.grid();
        if !grid.contains(region) {
            return Err(EmbarkRefusal::OffMap);
        }
        if grid.capacity(region) == 0 {
            return Err(EmbarkRefusal::NotHabitable);
        }
        if self.sites.values().any(|s| s.region == region) {
            return Err(EmbarkRefusal::Occupied);
        }
        if let Some(s) = self
            .sites
            .values()
            .find(|s| s.abandoned.is_none() && chebyshev(s.region, region) < EMBARK_MIN_DISTANCE_REGIONS)
        {
            return Err(EmbarkRefusal::TooClose { site: s.id });
        }
        Ok(())
    }

    /// The best region to found a site in from `from_region`: allowed, reachable, scored
    /// `capacity * 1000 / (distance + 1)`, ties by `(z, x)`.
    pub fn embark_target(&self, _civ: CivId, from_region: crate::RegionCoords) -> Option<crate::RegionCoords> {
        let r = EMBARK_MAX_SEARCH_REGIONS as i32;
        let mut cands: Vec<(std::cmp::Reverse<u64>, i32, i32)> = Vec::new();
        for z in from_region.z - r..=from_region.z + r {
            for x in from_region.x - r..=from_region.x + r {
                let rc = crate::RegionCoords::new(x, z);
                if self.embark_allowed(rc).is_ok() && self.same_landmass(from_region, rc) {
                    let score = u64::from(self.grid().capacity(rc)) * 1000 / (u64::from(chebyshev(from_region, rc)) + 1);
                    cands.push((std::cmp::Reverse(score), z, x));
                }
            }
        }
        cands.sort();
        cands
            .into_iter()
            .map(|(_, z, x)| crate::RegionCoords::new(x, z))
            .find(|rc| route(self.grid(), from_region, *rc, ROUTE_MAX_NODES).is_some())
    }

    fn related(&self, a: PersonId, b: PersonId) -> bool {
        let (Some(pa), Some(pb)) = (self.people.get(&a), self.people.get(&b)) else { return false };
        pa.mother == Some(b)
            || pa.father == Some(b)
            || pb.mother == Some(a)
            || pb.father == Some(a)
            || (pa.mother.is_some() && pa.mother == pb.mother)
            || (pa.father.is_some() && pa.father == pb.father)
    }

    fn eligible(&self, id: PersonId) -> bool {
        self.people
            .get(&id)
            .is_some_and(|p| p.alive() && p.spouse.is_none() && p.age_years(self.now) >= ADULT_AGE_YEARS)
    }

    fn marriages(&mut self, sid: SiteId) {
        let members: Vec<PersonId> = self.sites[&sid].members.iter().copied().collect();
        for &id in &members {
            if !self.eligible(id) {
                continue;
            }
            if !self.chance(self.now, u64::from(id.0), SALT_MARRY, MARRY_PER_MILLE_MONTHLY) {
                continue;
            }
            let sex = self.people[&id].sex;
            let partner = members
                .iter()
                .copied()
                .find(|&o| o != id && self.people[&o].sex != sex && self.eligible(o) && !self.related(id, o));
            if let Some(o) = partner {
                self.people.get_mut(&id).unwrap().spouse = Some(o);
                self.people.get_mut(&o).unwrap().spouse = Some(id);
                self.log_event(LogKind::Married { a: id, b: o });
            }
        }
    }

    /// Living citizens of `civ`: everyone alive whose `Person.site` is one of its sites
    /// (as `stats().per_civ` counts them). Counted as the sites' members (all alive,
    /// embodied or not — Ruling P2-C), plus living travellers on journeys (held or not),
    /// plus strays (`Wandering`, still citizens) — the latter found through
    /// `stray_candidates`, so no rule walks every person.
    pub fn civ_population(&self, civ: CivId) -> u32 {
        let away = |p: &Person| {
            p.alive()
                && p.site.and_then(|s| self.sites.get(&s)).is_some_and(|s| s.civ == civ && !s.members.contains(&p.id))
        };
        let members: u32 = self.sites.values().filter(|s| s.civ == civ).map(|s| s.members.len() as u32).sum();
        let travellers = self
            .journeys
            .values()
            .flat_map(|j| j.people.iter().map(move |p| (j.id, p)))
            .filter_map(|(j, p)| self.people.get(p).map(|q| (j, q)))
            .filter(|(j, q)| q.abstract_location() == Location::Journey(*j) && away(q))
            .count() as u32;
        let strays = self
            .stray_candidates
            .iter()
            .filter_map(|p| self.people.get(p))
            .filter(|q| matches!(q.abstract_location(), Location::Wandering(_)) && away(q))
            .count() as u32;
        members + travellers + strays
    }

    fn births(&mut self, sid: SiteId) {
        let now = self.now;
        let (civ, capacity) = {
            let s = &self.sites[&sid];
            (s.civ, s.capacity)
        };
        let mothers: Vec<PersonId> = self.sites[&sid].members.iter().copied().collect();
        let gap = BIRTH_MIN_GAP_DAYS * TICKS_PER_DAY;
        // Counted once, on the first fertile couple, then kept up by this month's births.
        let mut population: Option<u32> = None;
        for m in mothers {
            let p = &self.people[&m];
            let Some(f) = p.spouse else { continue };
            if p.sex != Sex::Female
                || !p.alive()
                || p.age_years(now) > FERTILE_MAX_AGE_YEARS
                || p.last_birth.is_some_and(|lb| now.0.saturating_sub(lb.0) < gap)
                || !self.sites[&sid].members.contains(&f)
            {
                continue;
            }
            let pop = *population.get_or_insert_with(|| self.civ_population(civ));
            if pop >= CIV_POPULATION_CAP {
                break;
            }
            let members = self.sites[&sid].members.len() as u32;
            if !self.chance(now, u64::from(m.0), SALT_BIRTH, birth_per_mille(members, capacity)) {
                continue;
            }
            let sex = if self.roll(now, u64::from(m.0), SALT_SEX) & 1 == 0 { Sex::Female } else { Sex::Male };
            let id = PersonId(self.next_ids[0]);
            self.next_ids[0] += 1;
            self.people.insert(id, Person::newborn(id, sex, now, Some(m), Some(f), Some(sid), Location::AtSite(sid)));
            // Born to an embodied mother: the child gets a body beside her.
            if let Some(rc) = self.embodied_in(m) {
                let p = self.people.get_mut(&id).unwrap();
                p.location = Location::InRegion(rc);
                p.home_location = Location::AtSite(sid);
                self.emit_spawn_near(rc, id, m);
            }
            self.sites.get_mut(&sid).unwrap().members.insert(id);
            population = Some(pop + 1);
            for parent in [m, f] {
                self.people.get_mut(&parent).unwrap().children.push(id);
            }
            self.people.get_mut(&m).unwrap().last_birth = Some(now);
            self.log_event(LogKind::Born { person: id, mother: Some(m), father: Some(f), site: Some(sid) });
            self.schedule(now.plus_days(death_age_days(self.seed, id, now)), super::Step::Death(id));
        }
    }
}
