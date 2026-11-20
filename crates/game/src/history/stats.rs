//! A one-glance summary of a history, like the forge's report.

use std::collections::BTreeMap;

use super::log::LogKind;
use super::records::*;
use super::{CalendarTick, History};

#[derive(Clone, Debug)]
pub struct HistoryStats {
    pub now: CalendarTick,
    pub civs: u32,
    pub civs_alive: u32,
    pub sites: u32,
    pub sites_alive: u32,
    pub alive: u32,
    pub dead: u32,
    /// Per civ: living citizens, living sites.
    pub per_civ: Vec<(CivId, u32, u32)>,
    pub events: BTreeMap<&'static str, u32>,
}

impl History {
    pub fn stats(&self) -> HistoryStats {
        let mut per: BTreeMap<CivId, (u32, u32)> = self.civs.keys().map(|&c| (c, (0, 0))).collect();
        let mut alive = 0;
        let mut dead = 0;
        for p in self.people.values() {
            if !p.alive() {
                dead += 1;
                continue;
            }
            alive += 1;
            if let Some(s) = p.site.and_then(|s| self.sites.get(&s)) {
                if let Some(e) = per.get_mut(&s.civ) {
                    e.0 += 1;
                }
            }
        }
        let mut sites_alive = 0;
        for s in self.sites.values().filter(|s| s.abandoned.is_none()) {
            sites_alive += 1;
            if let Some(e) = per.get_mut(&s.civ) {
                e.1 += 1;
            }
        }
        let mut events: BTreeMap<&'static str, u32> = BTreeMap::new();
        for e in &self.log {
            let name = match e.kind {
                LogKind::Born { .. } => "born",
                LogKind::Died { .. } => "died",
                LogKind::Married { .. } => "married",
                LogKind::Departed { .. } => "departed",
                LogKind::Arrived { .. } => "arrived",
                LogKind::SiteFounded { .. } => "site founded",
                LogKind::SiteAbandoned { .. } => "site abandoned",
            };
            *events.entry(name).or_default() += 1;
        }
        let per_civ: Vec<(CivId, u32, u32)> =
            per.into_iter().map(|(c, (a, s))| (c, a, s)).collect();
        HistoryStats {
            now: self.now,
            civs: self.civs.len() as u32,
            civs_alive: per_civ.iter().filter(|(_, a, _)| *a > 0).count() as u32,
            sites: self.sites.len() as u32,
            sites_alive,
            alive,
            dead,
            per_civ,
            events,
        }
    }
}

impl std::fmt::Display for HistoryStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "history at {}", self.now.date())?;
        writeln!(f, "  civs:  {} ({} alive)", self.civs, self.civs_alive)?;
        writeln!(f, "  sites: {} ({} alive)", self.sites, self.sites_alive)?;
        writeln!(f, "  people: {} alive, {} dead", self.alive, self.dead)?;
        for (civ, alive, sites) in &self.per_civ {
            writeln!(f, "  civ {}: {alive} alive, {sites} sites", civ.0)?;
        }
        for (name, n) in &self.events {
            writeln!(f, "  {name}: {n}")?;
        }
        Ok(())
    }
}
