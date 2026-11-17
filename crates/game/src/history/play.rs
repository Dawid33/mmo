//! The player's side of the history: site markers, notices and person info.

use super::log::LogKind;
use super::records::*;
use super::History;
use crate::protocol::{CivMarker, PersonInfo, SiteMarker};
use crate::RegionCoords;

impl History {
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
        if !self.notices.is_empty() {
            self.mark_content_dirty();
        }
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
