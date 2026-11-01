//! Event handlers: deaths, and the monthly marriages and births of a site.

use super::calendar::*;
use super::log::LogKind;
use super::records::*;
use super::rng::*;
use super::rules::*;
use super::History;

impl History {
    /// Old age comes for `id`.
    pub(crate) fn on_death(&mut self, id: PersonId) {
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
        let site = p.site;
        if let Some(sp) = spouse.and_then(|s| self.people.get_mut(&s)) {
            if sp.spouse == Some(person) {
                sp.spouse = None;
            }
        }
        self.log_event(LogKind::Died { person, cause });
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
        self.marriages(sid);
        self.births(sid);
        self.consider_leaving(sid);
        if self.sites.get(&sid).is_some_and(|s| s.abandoned.is_none()) {
            self.schedule(self.now.plus_days(DAYS_PER_MONTH), super::Step::SiteMonth(sid));
        }
    }

    /// Migration and embarking (filled in by the journey tasks).
    pub(crate) fn consider_leaving(&mut self, _site: SiteId) {}

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
            if !chance_per_mille(self.now, u64::from(id.0), SALT_MARRY, MARRY_PER_MILLE_MONTHLY) {
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

    /// Living people of every site of `civ`.
    fn civ_population(&self, civ: CivId) -> u32 {
        self.sites.values().filter(|s| s.civ == civ).map(|s| s.members.len() as u32).sum()
    }

    fn births(&mut self, sid: SiteId) {
        let now = self.now;
        let (civ, capacity) = {
            let s = &self.sites[&sid];
            (s.civ, s.capacity)
        };
        let mothers: Vec<PersonId> = self.sites[&sid].members.iter().copied().collect();
        let gap = BIRTH_MIN_GAP_DAYS * TICKS_PER_DAY;
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
            if self.civ_population(civ) >= CIV_POPULATION_CAP {
                break;
            }
            let members = self.sites[&sid].members.len() as u32;
            if !chance_per_mille(now, u64::from(m.0), SALT_BIRTH, birth_per_mille(members, capacity)) {
                continue;
            }
            let sex = if roll(now, u64::from(m.0), SALT_SEX) & 1 == 0 { Sex::Female } else { Sex::Male };
            let id = PersonId(self.next_ids[0]);
            self.next_ids[0] += 1;
            self.people.insert(id, Person::newborn(id, sex, now, Some(m), Some(f), Some(sid), Location::AtSite(sid)));
            self.sites.get_mut(&sid).unwrap().members.insert(id);
            for parent in [m, f] {
                self.people.get_mut(&parent).unwrap().children.push(id);
            }
            self.people.get_mut(&m).unwrap().last_birth = Some(now);
            self.log_event(LogKind::Born { person: id, mother: Some(m), father: Some(f), site: Some(sid) });
            self.schedule(now.plus_days(death_age_days(id, now)), super::Step::Death(id));
        }
    }
}
