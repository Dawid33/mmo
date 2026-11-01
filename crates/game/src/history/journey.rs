//! Journeys: parties walking a route over the society grid, one region leg at a time.

use super::calendar::{CalendarTick, DAYS_PER_MONTH};
use super::grid::{route, ROUTE_MAX_NODES};
use super::log::LogKind;
use super::records::*;
use super::rules::walk_ticks_per_region;
use super::{History, Step};
use crate::RegionCoords;

impl History {
    /// Send `people` from site `from` towards `to_region`. `None`, with no change,
    /// when no living traveller is given or the grid has no route.
    pub fn start_journey(
        &mut self,
        people: Vec<PersonId>,
        from: SiteId,
        to_region: RegionCoords,
        purpose: Purpose,
    ) -> Option<JourneyId> {
        let from_region = self.sites.get(&from)?.region;
        let people: Vec<PersonId> =
            people.into_iter().filter(|p| self.people.get(p).is_some_and(Person::alive)).collect();
        if people.is_empty() {
            return None;
        }
        let path = route(self.grid(), from_region, to_region, ROUTE_MAX_NODES)?;
        let (leg_ticks, arrivals) = self.leg_arrivals(&path);
        let id = JourneyId(self.next_ids[3]);
        self.next_ids[3] += 1;
        let departed = self.now;
        let mut left_site = false;
        for p in &people {
            if let Some(s) = self.sites.get_mut(&from) {
                left_site |= s.members.remove(p);
            }
            if let Some(pr) = self.people.get_mut(p) {
                pr.location = Location::Journey(id);
            }
        }
        self.log_event(LogKind::Departed { people: people.clone(), from, purpose });
        if left_site && self.sites.get(&from).is_some_and(|s| s.members.is_empty()) {
            self.abandon_site(from);
        }
        let first = arrivals.first().copied().unwrap_or(0);
        self.journeys.insert(
            id,
            Journey { id, people, purpose, route: path, departed, leg_ticks, arrivals, leg: 0 },
        );
        self.schedule(CalendarTick(departed.0 + first), Step::JourneyLeg(id));
        Some(id)
    }

    /// Base leg time and the cumulative arrival ticks along `path`.
    fn leg_arrivals(&self, path: &[RegionCoords]) -> (u64, Vec<u64>) {
        let leg_ticks = walk_ticks_per_region();
        let mut arrivals = Vec::with_capacity(path.len().saturating_sub(1));
        let mut total = 0u64;
        for (prev, next) in path.iter().zip(path.iter().skip(1)) {
            let mut d = leg_ticks * u64::from(self.grid().step_cost(*next)) / 1000;
            if prev.x != next.x && prev.z != next.z {
                d = d * 1414 / 1000;
            }
            total += d.max(1);
            arrivals.push(total);
        }
        (leg_ticks, arrivals)
    }

    /// Send journey `j` on from the region it stands in to `to_region`, optionally with a
    /// new purpose. False, with no change, when there is no route.
    fn reroute(&mut self, j: JourneyId, to_region: RegionCoords, purpose: Option<Purpose>) -> bool {
        let Some(from) = self.journeys.get(&j).and_then(|jr| jr.route.last().copied()) else { return false };
        let Some(path) = route(self.grid(), from, to_region, ROUTE_MAX_NODES) else { return false };
        let (leg_ticks, arrivals) = self.leg_arrivals(&path);
        let first = arrivals.first().copied().unwrap_or(0);
        let now = self.now;
        let jr = self.journeys.get_mut(&j).unwrap();
        if let Some(p) = purpose {
            jr.purpose = p;
        }
        jr.route = path;
        jr.departed = now;
        jr.leg_ticks = leg_ticks;
        jr.arrivals = arrivals;
        jr.leg = 0;
        self.schedule(CalendarTick(now.0 + first), Step::JourneyLeg(j));
        true
    }

    /// The route region the party occupies at `at`.
    pub fn journey_region_at(&self, j: JourneyId, at: CalendarTick) -> Option<RegionCoords> {
        let jr = self.journeys.get(&j)?;
        let elapsed = at.0.saturating_sub(jr.departed.0);
        let legs_done = jr.arrivals.partition_point(|&a| a <= elapsed);
        jr.route.get(legs_done).copied()
    }

    /// Per mille through the leg being walked at `at` (0 once arrived).
    pub fn journey_progress_in_leg(&self, j: JourneyId, at: CalendarTick) -> u32 {
        let Some(jr) = self.journeys.get(&j) else { return 0 };
        let elapsed = at.0.saturating_sub(jr.departed.0);
        let i = jr.arrivals.partition_point(|&a| a <= elapsed);
        let Some(&end) = jr.arrivals.get(i) else { return 0 };
        let start = if i == 0 { 0 } else { jr.arrivals[i - 1] };
        ((elapsed - start) * 1000 / (end - start).max(1)) as u32
    }

    /// Number of queued `JourneyLeg` steps (live or stale).
    pub fn pending_journey_legs(&self) -> usize {
        self.queue.values().filter(|s| matches!(s, Step::JourneyLeg(_))).count()
    }

    pub(crate) fn on_journey_leg(&mut self, j: JourneyId) {
        let Some(jr) = self.journeys.get_mut(&j) else { return };
        jr.leg += 1;
        let (leg, last) = (jr.leg as usize, jr.route.len() - 1);
        let region = jr.route[leg.min(last)];
        let next = jr.arrivals.get(leg).map(|a| CalendarTick(jr.departed.0 + a));
        if leg >= last {
            self.arrive(j);
        } else {
            self.entered_region(j, region);
            if let Some(at) = next {
                self.schedule(at, Step::JourneyLeg(j));
            }
        }
    }

    /// Plan 2 hook: the party entered `region` on its way.
    fn entered_region(&mut self, _j: JourneyId, _region: RegionCoords) {}

    fn arrive(&mut self, j: JourneyId) {
        let Some(jr) = self.journeys.get(&j) else { return };
        match jr.purpose {
            Purpose::Migrate { to } => {
                let jr = self.journeys.remove(&j).unwrap();
                let people: Vec<PersonId> =
                    jr.people.into_iter().filter(|p| self.people.get(p).is_some_and(Person::alive)).collect();
                if !people.is_empty() {
                    self.revive_site(to);
                }
                for p in &people {
                    if let Some(pr) = self.people.get_mut(p) {
                        pr.site = Some(to);
                        pr.location = Location::AtSite(to);
                    }
                    if let Some(s) = self.sites.get_mut(&to) {
                        s.members.insert(*p);
                    }
                }
                if !people.is_empty() {
                    self.log_event(LogKind::Arrived { people, site: to });
                }
            }
            Purpose::Embark { .. } => self.arrive_embark(j),
        }
    }

    /// Migrants reached an abandoned site: it lives again, one month on.
    fn revive_site(&mut self, sid: SiteId) {
        let Some(s) = self.sites.get_mut(&sid) else { return };
        if s.abandoned.take().is_some() && self.pending_site_months(sid) == 0 {
            self.schedule(self.now.plus_days(DAYS_PER_MONTH), Step::SiteMonth(sid));
        }
    }

    /// An embark party reached its target: found the site, or look for somewhere else.
    pub(crate) fn arrive_embark(&mut self, j: JourneyId) {
        let Some(jr) = self.journeys.get(&j) else { return };
        let Purpose::Embark { civ, from } = jr.purpose else { return };
        let region = *jr.route.last().expect("a route has a region");
        let party: Vec<PersonId> =
            jr.people.iter().copied().filter(|p| self.people.get(p).is_some_and(Person::alive)).collect();
        if party.is_empty() {
            self.journeys.remove(&j);
            return;
        }
        if self.embark_allowed(region).is_ok() {
            self.journeys.remove(&j);
            self.found_site(civ, region, &party, Some(from), false);
            return;
        }
        if let Some(target) = self.embark_target(civ, region) {
            if self.reroute(j, target, None) {
                return;
            }
        }
        if let Some(site) = self.least_crowded_reachable(civ, region, None, None) {
            let to_region = self.sites[&site].region;
            if self.reroute(j, to_region, Some(Purpose::Migrate { to: site })) {
                return;
            }
        }
        self.journeys.remove(&j);
        for p in party {
            if let Some(pr) = self.people.get_mut(&p) {
                pr.site = None;
                pr.location = Location::Wandering(region);
            }
        }
    }

    /// A traveller died: drop them from the journey, and the journey if it is empty.
    pub(crate) fn remove_traveller(&mut self, j: JourneyId, person: PersonId) {
        if let Some(jr) = self.journeys.get_mut(&j) {
            jr.people.retain(|p| *p != person);
            if jr.people.is_empty() {
                self.journeys.remove(&j);
            }
        }
    }
}
