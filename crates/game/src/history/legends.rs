//! Paged, filtered read of the event log for the Legends screen.

use serde::{Deserialize, Serialize};

use super::records::*;
use super::History;

pub const LEGENDS_PAGE_MAX: u16 = 200;
pub const LEGENDS_PAGE_DEFAULT: u16 = 100;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegendsFilter {
    All,
    Civ(CivId),
    Site(SiteId),
    Person(PersonId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegendsQuery {
    pub filter: LegendsFilter,
    /// Only entries strictly older than this log index (`None` = from the newest).
    pub before: Option<u32>,
    pub limit: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegendLine {
    /// Position in `History::log`.
    pub index: u32,
    pub text: String,
    pub sites: Vec<SiteId>,
    pub people: Vec<PersonId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegendsPage {
    pub query: LegendsQuery,
    pub lines: Vec<LegendLine>,
    /// Cursor for the next (older) page; `None` when no older match remains.
    pub next: Option<u32>,
    /// All entries matching the filter, in the whole log.
    pub total: u32,
}

impl History {
    fn legends_matches(&self, filter: &LegendsFilter, people: &[PersonId], sites: &[SiteId]) -> bool {
        let citizen_site = |p: &PersonId| self.person(*p).and_then(|p| p.site);
        match filter {
            LegendsFilter::All => true,
            LegendsFilter::Person(p) => people.contains(p),
            LegendsFilter::Site(s) => sites.contains(s) || people.iter().any(|p| citizen_site(p) == Some(*s)),
            LegendsFilter::Civ(c) => {
                let of_civ = |s: &SiteId| self.site(*s).is_some_and(|s| s.civ == *c);
                sites.iter().any(of_civ) || people.iter().filter_map(citizen_site).any(|s| of_civ(&s))
            }
        }
    }

    /// One page of the log, newest first, filtered; see [`LegendsPage`].
    pub fn legends(&self, q: &LegendsQuery) -> LegendsPage {
        let limit = usize::from(q.limit.clamp(1, LEGENDS_PAGE_MAX));
        let end = q.before.map_or(self.log.len(), |b| (b as usize).min(self.log.len()));
        let mut lines: Vec<LegendLine> = Vec::new();
        let mut total = 0u32;
        let mut older = false;
        for index in (0..self.log.len()).rev() {
            let entry = &self.log[index];
            let (people, sites) = entry.subjects();
            if !self.legends_matches(&q.filter, &people, &sites) {
                continue;
            }
            total += 1;
            if index >= end {
                continue;
            }
            if lines.len() < limit {
                lines.push(LegendLine { index: index as u32, text: entry.describe(self), sites, people });
            } else {
                older = true;
            }
        }
        let next = if older { lines.last().map(|l| l.index) } else { None };
        LegendsPage { query: q.clone(), lines, next, total }
    }
}
