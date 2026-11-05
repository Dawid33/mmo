//! The player's side of the history: embark options, the player's embark, which sites
//! may be returned to, site markers, notices and person info.

use std::collections::{BTreeMap, BTreeSet};

use super::chain::EmbarkRefusal;
use super::log::{LogEntry, LogKind};
use super::records::*;
use super::rules::EMBARK_PLAYER_PARTY_MAX;
use super::History;
use crate::protocol::{CivMarker, CivOption, EmbarkOptions, EmbarkRequest, PersonInfo, PersonOption, SiteMarker, SiteOption};
use crate::RegionCoords;

impl History {
    /// The site `person` could embark from: alive, at a living site of theirs in the
    /// abstract history (not travelling, not embodied), whose region is not live.
    fn embark_source(&self, person: PersonId) -> Option<SiteId> {
        let p = self.people.get(&person).filter(|p| p.alive())?;
        let Location::AtSite(sid) = p.location else { return None };
        let s = self.sites.get(&sid)?;
        (s.abandoned.is_none() && s.members.contains(&person) && !self.region_live(s.region)).then_some(sid)
    }

    /// Who may embark now: per civ, per site, its people free to leave (living, at a
    /// living site whose region is not running). Sites and civs with nobody are left out.
    pub fn embark_options(&self) -> EmbarkOptions {
        let mut civs = Vec::new();
        for civ in self.civs.values() {
            let mut sites = Vec::new();
            for sid in &civ.sites {
                let Some(s) = self.sites.get(sid) else { continue };
                let people: Vec<PersonOption> = s
                    .members
                    .iter()
                    .filter(|p| self.embark_source(**p) == Some(*sid))
                    .map(|p| {
                        let q = &self.people[p];
                        PersonOption {
                            person: q.id,
                            name: q.name.clone(),
                            age_years: q.age_years(self.now),
                            sex: q.sex,
                            spouse: q.spouse,
                        }
                    })
                    .collect();
                if !people.is_empty() {
                    sites.push(SiteOption { site: s.id, name: s.name.clone(), region: s.region, people });
                }
            }
            if !sites.is_empty() {
                civs.push(CivOption { civ: civ.id, name: civ.name.clone(), sites });
            }
        }
        EmbarkOptions { civs }
    }

    /// The player's embark (spec §7b): the people leave their sites and found a
    /// player-founded site in `req.region` at once — no journey is walked. All or
    /// nothing: a refusal changes no state.
    pub fn player_embark(&mut self, req: &EmbarkRequest) -> Result<SiteId, EmbarkRefusal> {
        self.player_embark_allowed(req.region)?;
        if req.people.is_empty() || req.people.len() > EMBARK_PLAYER_PARTY_MAX as usize {
            return Err(EmbarkRefusal::PartySize);
        }
        let mut seen = BTreeSet::new();
        let mut sources: BTreeMap<SiteId, Vec<PersonId>> = BTreeMap::new();
        for &p in &req.people {
            let from = self
                .embark_source(p)
                .filter(|s| self.sites[s].civ == req.civ && seen.insert(p))
                .ok_or(EmbarkRefusal::UnavailablePerson(p))?;
            sources.entry(from).or_default().push(p);
        }
        // Ruling P3-B: out of their old sites first (found_site wants them unplaced).
        for (&from, people) in &sources {
            for p in people {
                self.sites.get_mut(&from).unwrap().members.remove(p);
                self.set_abstract_location(*p, Location::Unplaced);
            }
            self.log_event(LogKind::Departed {
                people: people.clone(),
                from,
                purpose: Purpose::Embark { civ: req.civ, from },
            });
            if self.sites[&from].members.is_empty() {
                self.abandon_site(from);
            }
        }
        let from = sources.keys().next().copied();
        let site = self.found_site(req.civ, req.region, &req.people, from, true);
        // The fortress is not running yet, so the founding made no notice: its player,
        // about to play it, gets one all the same.
        if !self.region_live(req.region) {
            let kind = LogKind::SiteFounded { site, civ: req.civ, founders: req.people.clone(), from };
            let line = LogEntry { at: self.now, kind }.describe(self);
            self.notices.push((req.region, line));
        }
        Ok(site)
    }

    /// Whether a player may return to `site`: a living site a player founded. (Which
    /// player is the manager's business: there are no accounts, so it lets anyone return
    /// to a player-founded site no other connected player is playing.)
    pub fn returnable(&self, site: SiteId) -> bool {
        self.sites.get(&site).is_some_and(|s| s.player_founded && s.abandoned.is_none())
    }

    /// Every site, by id, as the world map shows it.
    pub fn site_markers(&self) -> Vec<SiteMarker> {
        self.sites
            .values()
            .map(|s| SiteMarker {
                site: s.id,
                civ: s.civ,
                region: s.region,
                name: s.name.clone(),
                population: self.living_members(s.id).len() as u32,
                abandoned: s.abandoned.is_some(),
                returnable: self.returnable(s.id),
            })
            .collect()
    }

    /// Every civilisation's name, by id.
    pub fn civ_markers(&self) -> Vec<CivMarker> {
        self.civs.values().map(|c| CivMarker { civ: c.id, name: c.name.clone() }).collect()
    }

    /// `person` for the inspector; `None` for an unknown id.
    pub fn person_info(&self, person: PersonId) -> Option<PersonInfo> {
        let p = self.people.get(&person)?;
        let name = |id: PersonId| self.people.get(&id).map_or_else(|| person_name(id), |q| q.name.clone());
        let at = p.died.map_or(self.now, |(t, _)| t);
        Some(PersonInfo {
            person,
            name: p.name.clone(),
            age_years: p.age_years(at),
            alive: p.alive(),
            site: p.site.and_then(|s| self.sites.get(&s)).map(|s| s.name.clone()),
            spouse: p.spouse.map(name),
            parents: p.mother.into_iter().chain(p.father).map(name).collect(),
            children: p.children.iter().copied().map(name).collect(),
        })
    }

    /// Drain the notices made since the last call: `(region, line)` per log entry about a
    /// site in a live region — born, died or married at it, departed from it, arrived at,
    /// founded or abandoned there.
    pub fn take_notices(&mut self) -> Vec<(RegionCoords, String)> {
        std::mem::take(&mut self.notices)
    }

    /// The live regions an entry about to be logged is news for.
    pub(crate) fn notice_regions(&self, kind: &LogKind) -> Vec<RegionCoords> {
        let site_of = |p: &PersonId| match self.people.get(p).map(Person::abstract_location) {
            Some(Location::AtSite(s)) => Some(s),
            _ => None,
        };
        let site = match kind {
            LogKind::Born { site, .. } => *site,
            LogKind::Died { person, .. } => site_of(person),
            LogKind::Married { a, .. } => site_of(a),
            LogKind::Departed { from, .. } => Some(*from),
            LogKind::Arrived { site, .. } => Some(*site),
            LogKind::SiteFounded { site, .. } => Some(*site),
            LogKind::SiteAbandoned { site } => Some(*site),
        };
        site.and_then(|s| self.sites.get(&s))
            .map(|s| s.region)
            .filter(|r| self.region_live(*r))
            .into_iter()
            .collect()
    }
}
