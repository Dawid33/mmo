//! The history log: one entry per event, each describable as an English line.

use serde::{Deserialize, Serialize};

use super::calendar::CalendarTick;
use super::records::*;
use super::History;

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub at: CalendarTick,
    pub kind: LogKind,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum LogKind {
    Born { person: PersonId, mother: Option<PersonId>, father: Option<PersonId>, site: Option<SiteId> },
    Died { person: PersonId, cause: DeathCause },
    Married { a: PersonId, b: PersonId },
    Departed { people: Vec<PersonId>, from: SiteId, purpose: Purpose },
    /// Migrants reaching an existing site.
    Arrived { people: Vec<PersonId>, site: SiteId },
    SiteFounded { site: SiteId, civ: CivId, founders: Vec<PersonId>, from: Option<SiteId> },
    SiteAbandoned { site: SiteId },
}

fn pname(h: &History, id: PersonId) -> String {
    h.person(id).map(|p| p.name.clone()).unwrap_or_else(|| person_name(id))
}

fn sname(h: &History, id: SiteId) -> String {
    h.site(id).map(|s| s.name.clone()).unwrap_or_else(|| site_name(id))
}

fn names(h: &History, ids: &[PersonId]) -> String {
    match ids {
        [] => "nobody".to_string(),
        [one] => pname(h, *one),
        _ => format!("{} people", ids.len()),
    }
}

impl LogEntry {
    /// One English line, e.g. `12 Slate 40: Dwarf 7 was born at Site 2 to Dwarf 3 and Dwarf 4`.
    pub fn describe(&self, h: &History) -> String {
        let what = match &self.kind {
            LogKind::Born { person, mother, father, site } => {
                let mut s = format!("{} was born", pname(h, *person));
                if let Some(site) = site {
                    s += &format!(" at {}", sname(h, *site));
                }
                match (mother, father) {
                    (Some(m), Some(f)) => s += &format!(" to {} and {}", pname(h, *m), pname(h, *f)),
                    (Some(p), None) | (None, Some(p)) => s += &format!(" to {}", pname(h, *p)),
                    (None, None) => {}
                }
                s
            }
            LogKind::Died { person, cause } => {
                let how = match cause {
                    DeathCause::OldAge => "of old age",
                };
                format!("{} died {how}", pname(h, *person))
            }
            LogKind::Married { a, b } => format!("{} married {}", pname(h, *a), pname(h, *b)),
            LogKind::Departed { people, from, purpose } => {
                let why = match purpose {
                    Purpose::Migrate { to } => format!("to migrate to {}", sname(h, *to)),
                    Purpose::Embark { .. } => "to found a new site".to_string(),
                };
                format!("{} left {} {why}", names(h, people), sname(h, *from))
            }
            LogKind::Arrived { people, site } => format!("{} arrived at {}", names(h, people), sname(h, *site)),
            LogKind::SiteFounded { site, civ, founders, from } => {
                let mut s = format!("{} was founded for {}", sname(h, *site), h.civ(*civ).map(|c| c.name.clone()).unwrap_or_else(|| civ_name(*civ)));
                if let Some(f) = from {
                    s += &format!(" from {}", sname(h, *f));
                }
                s += &format!(" by {}", names(h, founders));
                s
            }
            LogKind::SiteAbandoned { site } => format!("{} was abandoned", sname(h, *site)),
        };
        format!("{}: {what}", self.at.date())
    }
}

impl LogEntry {
    /// The people and sites this entry names (for filtering; no duplicates guaranteed).
    pub fn subjects(&self) -> (Vec<PersonId>, Vec<SiteId>) {
        match &self.kind {
            LogKind::Born { person, mother, father, site } => {
                let mut people = vec![*person];
                people.extend(mother);
                people.extend(father);
                (people, site.iter().copied().collect())
            }
            LogKind::Died { person, .. } => (vec![*person], Vec::new()),
            LogKind::Married { a, b } => (vec![*a, *b], Vec::new()),
            LogKind::Departed { people, from, purpose } => {
                let mut sites = vec![*from];
                if let Purpose::Migrate { to } = purpose {
                    sites.push(*to);
                }
                (people.clone(), sites)
            }
            LogKind::Arrived { people, site } => (people.clone(), vec![*site]),
            LogKind::SiteFounded { site, founders, from, .. } => {
                let mut sites = vec![*site];
                sites.extend(from);
                (founders.clone(), sites)
            }
            LogKind::SiteAbandoned { site } => (Vec::new(), vec![*site]),
        }
    }
}
