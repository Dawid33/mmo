//! Journeys: parties walking a route over the society grid, one region leg at a time.

use super::calendar::{CalendarTick, DAYS_PER_MONTH};
use super::grid::{ROUTE_MAX_NODES, route};
use super::log::LogKind;
use super::records::*;
use super::rules::walk_ticks_per_region;
use super::{History, Step};
use crate::RegionCoords;

impl History {
    /// Send `people` from site `from` towards `to_region`. Only living members of `from` go;
    /// anyone else passed in stays where they are. `None`, with no change, when no
    /// traveller is left or `to_region` cannot be reached.
    pub fn start_journey(
        &mut self,
        people: Vec<PersonId>,
        from: SiteId,
        to_region: RegionCoords,
        purpose: Purpose,
    ) -> Option<JourneyId> {
        let site = self.sites.get(&from)?;
        let origin = site.region;
        let people: Vec<PersonId> = people
            .into_iter()
            .filter(|p| site.members.contains(p) && self.people.get(p).is_some_and(Person::alive))
            .collect();
        self.begin_journey(people, origin, Some(from), to_region, purpose)
    }

    /// Send `people` who are at no site (strays in `origin`) towards `to_region`. Only living
    /// people neither at a site nor on a journey go. `None`, with no change, when nobody is
    /// left or `to_region` cannot be reached. No `Departed` entry is logged (it names a site).
    pub fn start_journey_from_region(
        &mut self,
        people: Vec<PersonId>,
        origin: RegionCoords,
        to_region: RegionCoords,
        purpose: Purpose,
    ) -> Option<JourneyId> {
        let people: Vec<PersonId> = people
            .into_iter()
            .filter(|p| {
                self.people.get(p).is_some_and(|q| {
                    q.alive()
                        && !matches!(
                            q.abstract_location(),
                            Location::AtSite(_) | Location::Journey(_)
                        )
                })
            })
            .collect();
        self.begin_journey(people, origin, None, to_region, purpose)
    }

    /// Whether `to` can be reached from `from` at all: both on the grid's same landmass.
    pub(crate) fn same_landmass(&self, from: RegionCoords, to: RegionCoords) -> bool {
        let g = self.grid();
        g.component(from).is_some() && g.component(from) == g.component(to)
    }

    fn begin_journey(
        &mut self,
        people: Vec<PersonId>,
        origin: RegionCoords,
        from: Option<SiteId>,
        to_region: RegionCoords,
        purpose: Purpose,
    ) -> Option<JourneyId> {
        let mut seen = std::collections::BTreeSet::new();
        let people: Vec<PersonId> = people.into_iter().filter(|p| seen.insert(*p)).collect();
        if people.is_empty() || !self.same_landmass(origin, to_region) {
            return None;
        }
        let path = route(self.grid(), origin, to_region, ROUTE_MAX_NODES)?;
        let (leg_ticks, arrivals) = self.leg_arrivals(&path);
        self.mark_content_dirty();
        let id = JourneyId(self.next_ids[3]);
        self.next_ids[3] += 1;
        let departed = self.now;
        let mut left_site = false;
        for p in &people {
            self.positions.remove(p);
            if let Some(s) = from.and_then(|f| self.sites.get_mut(&f)) {
                left_site |= s.members.remove(p);
            }
            self.set_abstract_location(*p, Location::Journey(id));
        }
        if let Some(from) = from {
            self.log_event(LogKind::Departed {
                people: people.clone(),
                from,
                purpose,
            });
            if left_site && self.sites.get(&from).is_some_and(|s| s.members.is_empty()) {
                self.abandon_site(from);
            }
        }
        let first = arrivals.first().copied().unwrap_or(0);
        self.journeys.insert(
            id,
            Journey {
                id,
                people,
                purpose,
                route: path,
                departed,
                leg_ticks,
                arrivals,
                leg: 0,
                held_in: None,
            },
        );
        self.schedule(
            CalendarTick(departed.0 + first),
            Step::JourneyLeg {
                journey: id,
                leg: 0,
            },
        );
        // Leaving a running region: the bodies walk out, the journey waits for them.
        if self.running.contains(&origin) {
            self.hold_and_walk_out(id, origin);
        }
        Some(id)
    }

    /// Base leg time and the cumulative arrival ticks along `path`.
    fn leg_arrivals(&self, path: &[RegionCoords]) -> (u64, Vec<u64>) {
        let leg_ticks = walk_ticks_per_region();
        let mut arrivals = Vec::with_capacity(path.len().saturating_sub(1));
        let mut total = 0u64;
        for (prev, next) in path.iter().zip(path.iter().skip(1)) {
            let mut d = leg_ticks * u64::from(self.grid().step_cost(*next)) / 1000;
            let (dx, dz) = self.grid().delta(*prev, *next);
            if dx != 0 && dz != 0 {
                d = d * 1414 / 1000;
            }
            total += d.max(1);
            arrivals.push(total);
        }
        (leg_ticks, arrivals)
    }

    /// Send journey `j` on from the region it stands in to `to_region`, optionally with a
    /// new purpose. False, with no change, when there is no route. Any queued leg of the
    /// journey is cancelled.
    pub(crate) fn reroute(
        &mut self,
        j: JourneyId,
        to_region: RegionCoords,
        purpose: Option<Purpose>,
    ) -> bool {
        let Some(from) = self
            .journeys
            .get(&j)
            .and_then(|jr| jr.route.get(jr.leg as usize).copied())
        else {
            return false;
        };
        self.reroute_from(j, from, to_region, purpose)
    }

    /// [`History::reroute`] from `from` rather than the journey's current region (a party
    /// that walked off its route).
    pub(crate) fn reroute_from(
        &mut self,
        j: JourneyId,
        from: RegionCoords,
        to_region: RegionCoords,
        purpose: Option<Purpose>,
    ) -> bool {
        if !self.journeys.contains_key(&j) || !self.same_landmass(from, to_region) {
            return false;
        }
        let Some(path) = route(self.grid(), from, to_region, ROUTE_MAX_NODES) else {
            return false;
        };
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
        self.cancel_legs(j);
        self.schedule(
            CalendarTick(now.0 + first),
            Step::JourneyLeg { journey: j, leg: 0 },
        );
        true
    }

    /// Drop every queued leg step of `j`.
    pub(crate) fn cancel_legs(&mut self, j: JourneyId) {
        self.mark_content_dirty();
        self.queue
            .retain(|_, s| !matches!(s, Step::JourneyLeg { journey, .. } if *journey == j));
    }

    /// Re-time journey `j` so the leg it is on now ends at `at` (not before now), with every
    /// later leg taking its own duration as before, and queue that leg's end (cancelling any
    /// queued leg of `j`). For a party whose walk was paused or played out in a region.
    pub(crate) fn schedule_next_leg_from(&mut self, j: JourneyId, at: CalendarTick) {
        let now = self.now;
        let at = at.max(now);
        let Some(jr) = self.journeys.get_mut(&j) else {
            return;
        };
        let leg = jr.leg;
        let l = leg as usize;
        if l < jr.arrivals.len() {
            // The leg started (relatively) at `arrivals[l - 1]`; put that start at now.
            let started = if l == 0 { 0 } else { jr.arrivals[l - 1] };
            let departed = now.0.saturating_sub(started);
            let shift_to = at.0 - departed;
            let old_end = jr.arrivals[l];
            for a in &mut jr.arrivals[l..] {
                *a = *a - old_end + shift_to;
            }
            jr.departed = CalendarTick(departed);
        }
        self.cancel_legs(j);
        self.schedule(at, Step::JourneyLeg { journey: j, leg });
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
        let Some(jr) = self.journeys.get(&j) else {
            return 0;
        };
        let elapsed = at.0.saturating_sub(jr.departed.0);
        let i = jr.arrivals.partition_point(|&a| a <= elapsed);
        let Some(&end) = jr.arrivals.get(i) else {
            return 0;
        };
        let start = if i == 0 { 0 } else { jr.arrivals[i - 1] };
        ((elapsed - start) * 1000 / (end - start).max(1)) as u32
    }

    /// Number of queued `JourneyLeg` steps (live or stale).
    pub fn pending_journey_legs(&self) -> usize {
        self.queue
            .values()
            .filter(|s| matches!(s, Step::JourneyLeg { .. }))
            .count()
    }

    pub(crate) fn on_journey_leg(&mut self, j: JourneyId, step_leg: u32) {
        let Some(jr) = self.journeys.get(&j) else {
            return;
        };
        // A held journey is walked by bodies, not by steps.
        if jr.leg != step_leg || jr.held_in.is_some() {
            return;
        }
        let idx = (jr.leg as usize + 1).min(jr.route.len() - 1);
        let entering = jr.route[idx];
        if self.stopping.contains(&entering) {
            let at = self.deferred();
            self.schedule_next_leg_from(j, at);
            return;
        }
        if self.running.contains(&entering) && self.enter_running(j, idx) {
            return;
        }
        let jr = self.journeys.get_mut(&j).unwrap();
        jr.leg += 1;
        let (leg, last) = (jr.leg as usize, jr.route.len() - 1);
        let next = jr
            .arrivals
            .get(leg)
            .map(|a| CalendarTick(jr.departed.0 + a));
        if leg >= last {
            self.arrive(j);
        } else if let Some(at) = next {
            self.schedule(
                at,
                Step::JourneyLeg {
                    journey: j,
                    leg: leg as u32,
                },
            );
        }
    }

    pub(crate) fn arrive(&mut self, j: JourneyId) {
        let Some(jr) = self.journeys.get(&j) else {
            return;
        };
        match jr.purpose {
            Purpose::Migrate { to } => {
                let jr = self.journeys.remove(&j).unwrap();
                let people: Vec<PersonId> = jr
                    .people
                    .into_iter()
                    .filter(|p| self.people.get(p).is_some_and(Person::alive))
                    .collect();
                if !people.is_empty() {
                    self.revive_site(to);
                }
                for p in &people {
                    if let Some(pr) = self.people.get_mut(p) {
                        pr.site = Some(to);
                    }
                    self.set_abstract_location(*p, Location::AtSite(to));
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
        let Some(s) = self.sites.get_mut(&sid) else {
            return;
        };
        if s.abandoned.take().is_some() && self.pending_site_months(sid) == 0 {
            self.schedule(self.now.plus_days(DAYS_PER_MONTH), Step::SiteMonth(sid));
        }
    }

    /// An embark party reached its target: found the site, or look for somewhere else.
    pub(crate) fn arrive_embark(&mut self, j: JourneyId) {
        let Some(jr) = self.journeys.get(&j) else {
            return;
        };
        let Purpose::Embark { civ, from } = jr.purpose else {
            return;
        };
        let region = *jr.route.last().expect("a route has a region");
        let party: Vec<PersonId> = jr
            .people
            .iter()
            .copied()
            .filter(|p| self.people.get(p).is_some_and(Person::alive))
            .collect();
        if party.is_empty() {
            self.journeys.remove(&j);
            return;
        }
        if self.embark_allowed(region).is_ok() {
            self.journeys.remove(&j);
            self.found_site(civ, region, &party, Some(from));
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
            }
            self.set_abstract_location(p, Location::Wandering(region));
        }
    }

    /// A traveller died: drop them from the journey, and the journey if it is empty. An
    /// emptied migration to a site nobody lives at any more (its last citizens were walking
    /// home to it) abandons that site.
    pub(crate) fn remove_traveller(&mut self, j: JourneyId, person: PersonId) {
        let Some(jr) = self.journeys.get_mut(&j) else {
            return;
        };
        jr.people.retain(|p| *p != person);
        if !jr.people.is_empty() {
            return;
        }
        let purpose = jr.purpose;
        self.journeys.remove(&j);
        if let Purpose::Migrate { to } = purpose {
            if self
                .sites
                .get(&to)
                .is_some_and(|s| s.abandoned.is_none() && s.members.is_empty())
            {
                self.abandon_site(to);
            }
        }
    }
}
