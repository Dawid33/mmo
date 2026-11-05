//! Region hand-over: what a region must spawn when it starts (catch-up), what becomes of
//! its people when it parks or one of them crosses a seam, and the [`Directive`]s that
//! carry history's decisions to running regions.
//!
//! A person is *embodied* while `location` is `InRegion(rc)` with `rc` running or
//! stopping; `home_location` then keeps their abstract place (`AtSite` / `Journey` /
//! `Wandering`) — see [`Person::abstract_location`]. A journey with `held_in` set is
//! walked by bodies in that region and queues no abstract leg.

use std::collections::BTreeSet;

use super::calendar::CalendarTick;
use super::records::*;
use super::rng::{SALT_EDGE, SALT_SPAWN};
use super::rules::{spawn_spread_blocks, EDGE_SPREAD_PER_MILLE, HELD_TIMEOUT_LEGS, STOPPING_DEFER_DAYS};
use super::History;
use crate::{GameEventKind, Goal, PersonSpawn, RegionCoords, SpawnAt, REGION_BLOCKS};

/// An event history wants applied in a running region (spawn, despawn, walk out).
#[derive(Clone, Debug, PartialEq)]
pub struct Directive {
    pub region: RegionCoords,
    pub event: GameEventKind,
}

/// The region's centre column, as a walk goal.
fn centre_goal(region: RegionCoords) -> Goal {
    let c = (REGION_BLOCKS / 2) as i32;
    Goal { region, column: [c, c], y: None }
}

/// The edge of `rc` a party coming from `prev` walks in over: `prev` itself when it is
/// an orthogonal neighbour (or `rc` — no edge), else the orthogonal neighbour between
/// the two, the one across the x side.
fn entry_side(prev: RegionCoords, rc: RegionCoords) -> RegionCoords {
    if prev.x != rc.x && prev.z != rc.z {
        RegionCoords::new(prev.x, rc.z)
    } else {
        prev
    }
}

impl History {
    /// Drain the directives decided since the last call, in decision order.
    pub fn take_directives(&mut self) -> Vec<Directive> {
        std::mem::take(&mut self.directives)
    }

    /// Whether `rc` holds bodies: running, or stopping and not yet parked.
    pub(crate) fn region_live(&self, rc: RegionCoords) -> bool {
        self.running.contains(&rc) || self.stopping.contains(&rc)
    }

    /// The live region `person` is embodied in.
    pub(crate) fn embodied_in(&self, person: PersonId) -> Option<RegionCoords> {
        match self.people.get(&person)?.location {
            Location::InRegion(rc) if self.region_live(rc) => Some(rc),
            _ => None,
        }
    }

    /// Put `person` at abstract place `loc`: kept as `home_location` while they are
    /// embodied (the body stays), else their `location`.
    pub(crate) fn set_abstract_location(&mut self, person: PersonId, loc: Location) {
        let embodied = self.embodied_in(person).is_some();
        if let Some(p) = self.people.get_mut(&person) {
            if embodied {
                p.home_location = loc;
            } else {
                p.location = loc;
                p.home_location = Location::Unplaced;
            }
        }
    }

    fn emit(&mut self, region: RegionCoords, event: GameEventKind) {
        self.directives.push(Directive { region, event });
    }

    pub(crate) fn emit_despawn(&mut self, region: RegionCoords, person: PersonId) {
        self.emit(region, GameEventKind::DespawnPerson(person));
    }

    pub(crate) fn emit_spawn_near(&mut self, region: RegionCoords, person: PersonId, near: PersonId) {
        let spawn = PersonSpawn { person, at: SpawnAt::NearPerson(near), walk_to: None };
        self.emit(region, GameEventKind::SpawnPerson(spawn));
    }

    fn embody(&mut self, person: PersonId, rc: RegionCoords, home: Location) {
        self.positions.remove(&person);
        if let Some(p) = self.people.get_mut(&person) {
            p.location = Location::InRegion(rc);
            p.home_location = home;
        }
    }

    /// The step acting on `person` must wait: they are in a stopping region.
    pub(crate) fn in_stopping_region(&self, person: PersonId) -> bool {
        self.embodied_in(person).is_some_and(|rc| self.stopping.contains(&rc))
    }

    /// `now` plus the deferral for a step that hit a stopping region.
    pub(crate) fn deferred(&self) -> CalendarTick {
        self.now.plus_days(STOPPING_DEFER_DAYS)
    }

    /// The landing column of `rc` (else its centre), moved a hashed few blocks per person —
    /// within [`spawn_spread_blocks`] of `crowd`, the number spawned there together.
    fn landing_column(&self, rc: RegionCoords, person: PersonId, crowd: usize) -> [u32; 2] {
        if let Some(&(r, [x, _, z])) = self.positions.get(&person) {
            if r == rc {
                let n = REGION_BLOCKS as i32 - 1;
                let b = crate::BLOCK_VOXELS as f32;
                return [((x / b).floor() as i32).clamp(0, n) as u32, ((z / b).floor() as i32).clamp(0, n) as u32];
            }
        }
        let c = REGION_BLOCKS as u32 / 2;
        let [x, z] = self.grid().landing(rc).unwrap_or([c, c]);
        let spread = spawn_spread_blocks(u32::try_from(crowd).unwrap_or(u32::MAX));
        let span = u64::from(2 * spread + 1);
        let r = self.roll(self.now, u64::from(person.0), SALT_SPAWN);
        let off = |v: u32, d: u64| {
            (i64::from(v) + d as i64 - i64::from(spread)).clamp(0, REGION_BLOCKS as i64 - 1) as u32
        };
        [off(x, r % span), off(z, (r / span) % span)]
    }

    /// Duration in ticks of the leg `j` is on (the walk out of `route[leg]`).
    fn leg_duration(&self, j: JourneyId) -> u64 {
        let Some(jr) = self.journeys.get(&j) else { return 0 };
        let i = jr.leg as usize;
        match jr.arrivals.get(i) {
            Some(&end) => end - if i == 0 { 0 } else { jr.arrivals[i - 1] },
            None => jr.leg_ticks,
        }
    }

    /// Hold `j` in `rc`: its region's bodies walk it, no abstract leg is queued.
    fn hold(&mut self, j: JourneyId, rc: RegionCoords) {
        if let Some(jr) = self.journeys.get_mut(&j) {
            jr.held_in = Some(rc);
            self.held_since.insert(j, self.now);
        }
        self.cancel_legs(j);
    }

    /// Release every journey held in a running region that has no living traveller
    /// embodied there any more, or has been held for [`HELD_TIMEOUT_LEGS`] of its current
    /// leg: its bodies there are despawned and it goes on abstractly from that region, with
    /// half the leg left (as after a park). Journeys held in a stopping region wait for its
    /// park. [`History::advance`] calls it.
    pub(crate) fn release_stuck_journeys(&mut self) {
        self.held_since.retain(|j, _| self.journeys.get(j).is_some_and(|jr| jr.held_in.is_some()));
        let now = self.now;
        let stuck: Vec<(JourneyId, RegionCoords, Vec<PersonId>)> = self
            .held_since
            .iter()
            .filter_map(|(&j, &since)| {
                let jr = &self.journeys[&j];
                let rc = jr.held_in?;
                if !self.running.contains(&rc) {
                    return None;
                }
                let bodies: Vec<PersonId> = jr
                    .people
                    .iter()
                    .copied()
                    .filter(|p| {
                        self.people.get(p).is_some_and(|q| {
                            q.alive() && q.location == Location::InRegion(rc) && q.home_location == Location::Journey(j)
                        })
                    })
                    .collect();
                let timed_out = now.0 >= since.0.saturating_add(HELD_TIMEOUT_LEGS * self.leg_duration(j));
                (bodies.is_empty() || timed_out).then_some((j, rc, bodies))
            })
            .collect();
        for (j, rc, bodies) in stuck {
            if !bodies.is_empty() {
                ::log::warn!("history: journey {j:?} held in {rc:?} for too long; its bodies there go on abstractly");
            }
            for p in bodies {
                self.emit_despawn(rc, p);
                let q = self.people.get_mut(&p).unwrap();
                q.location = Location::Journey(j);
                q.home_location = Location::Unplaced;
            }
            self.journeys.get_mut(&j).unwrap().held_in = None;
            self.held_since.remove(&j);
            let at = CalendarTick(now.0 + self.leg_duration(j) / 2);
            self.schedule_next_leg_from(j, at);
        }
    }

    /// Embody site `site`'s people if its region is running (not stopping): every living
    /// member at the site with no body yet is spawned near the landing, as at catch-up. No
    /// change otherwise. [`History::found_site`] calls it; an embark into a running region
    /// (a played site) goes through it.
    pub fn embody_site(&mut self, site: SiteId) {
        let Some(s) = self.sites.get(&site) else { return };
        let rc = s.region;
        if !self.running.contains(&rc) {
            return;
        }
        let here: Vec<PersonId> = s
            .members
            .iter()
            .copied()
            .filter(|p| self.people.get(p).is_some_and(|q| q.alive() && q.location == Location::AtSite(site)))
            .collect();
        let crowd = here.len();
        for p in here {
            let at = SpawnAt::Column(self.landing_column(rc, p, crowd));
            self.emit(rc, GameEventKind::SpawnPerson(PersonSpawn { person: p, at, walk_to: None }));
            self.embody(p, rc, Location::AtSite(site));
        }
    }

    /// Hold `j` in `rc` and walk its travellers embodied there out toward the next route
    /// region. Nothing happens with nowhere left to walk or nobody there to walk it.
    pub(crate) fn hold_and_walk_out(&mut self, j: JourneyId, rc: RegionCoords) {
        let Some(jr) = self.journeys.get(&j) else { return };
        let Some(&next) = jr.route.get(jr.leg as usize + 1) else { return };
        let walkers: Vec<PersonId> = jr
            .people
            .iter()
            .copied()
            .filter(|p| self.people.get(p).is_some_and(|q| q.alive() && q.location == Location::InRegion(rc)))
            .collect();
        if walkers.is_empty() {
            return;
        }
        self.hold(j, rc);
        for p in walkers {
            self.emit(rc, GameEventKind::WalkOut(p, next));
        }
    }

    /// Journey `j` is at `route[idx]`, a running region: its abstract travellers walk in
    /// over the edge and on to the next region (or, at the destination, to its landing,
    /// where they arrive at once), and the journey is held there. False, with no change,
    /// when it has no abstract traveller to walk in (it goes on abstractly).
    pub(crate) fn enter_running(&mut self, j: JourneyId, idx: usize) -> bool {
        let Some(jr) = self.journeys.get(&j) else { return false };
        let travellers: Vec<PersonId> = jr
            .people
            .iter()
            .copied()
            .filter(|p| self.people.get(p).is_some_and(|q| q.alive() && q.location == Location::Journey(j)))
            .collect();
        if travellers.is_empty() {
            return false;
        }
        let rc = jr.route[idx];
        let last = jr.route.len() - 1;
        let from = if idx == 0 { rc } else { entry_side(jr.route[idx - 1], rc) };
        let goal = if idx >= last {
            let [x, z] = self.grid().landing(rc).unwrap_or([REGION_BLOCKS as u32 / 2; 2]);
            Goal { region: rc, column: [x as i32, z as i32], y: None }
        } else {
            centre_goal(jr.route[idx + 1])
        };
        self.journeys.get_mut(&j).unwrap().leg = idx as u32;
        let spread = u64::from(EDGE_SPREAD_PER_MILLE);
        let base = spread + self.roll(self.now, u64::from(j.0), SALT_EDGE) % (1001 - 2 * spread);
        for p in travellers {
            let along = base + self.roll(self.now, u64::from(p.0), SALT_EDGE) % (2 * spread + 1) - spread;
            let at = SpawnAt::Edge { from, along_per_mille: along as u32 };
            self.emit(rc, GameEventKind::SpawnPerson(PersonSpawn { person: p, at, walk_to: Some(goal) }));
            self.embody(p, rc, Location::Journey(j));
        }
        if idx >= last {
            self.arrive(j);
            // An embark party that found the place taken goes on from here.
            self.hold_and_walk_out(j, rc);
        } else {
            self.hold(j, rc);
        }
        true
    }

    /// Region `rc` starts: the events that put history's people into it now — site people
    /// and strays at its landing, parties walking through it at the edge they came in by.
    pub fn region_started(&mut self, rc: RegionCoords) -> Vec<GameEventKind> {
        self.stopping.remove(&rc);
        self.running.insert(rc);
        let mark = self.directives.len();
        let residents: Vec<(PersonId, Location)> = self
            .people
            .values()
            .filter(|p| p.alive())
            .filter(|p| match p.location {
                Location::AtSite(s) => self.sites.get(&s).is_some_and(|s| s.region == rc),
                Location::Wandering(r) => r == rc,
                _ => false,
            })
            .map(|p| (p.id, p.location))
            .collect();
        let crowd = residents.len();
        for (p, home) in residents {
            let at = SpawnAt::Column(self.landing_column(rc, p, crowd));
            self.emit(rc, GameEventKind::SpawnPerson(PersonSpawn { person: p, at, walk_to: None }));
            self.embody(p, rc, home);
        }
        let passing: Vec<(JourneyId, usize)> = self
            .journeys
            .values()
            .filter(|jr| jr.held_in.is_none() && self.journey_region_at(jr.id, self.now) == Some(rc))
            .filter_map(|jr| jr.route.iter().position(|r| *r == rc).map(|i| (jr.id, i)))
            .collect();
        for (j, idx) in passing {
            self.enter_running(j, idx);
        }
        let mut events = Vec::new();
        let mut i = mark;
        while i < self.directives.len() {
            if self.directives[i].region == rc {
                events.push(self.directives.remove(i).event);
            } else {
                i += 1;
            }
        }
        events
    }

    /// Region `rc` is shutting down: steps acting on its people wait until it has parked.
    pub fn region_stopping(&mut self, rc: RegionCoords) {
        self.running.remove(&rc);
        self.stopping.insert(rc);
    }

    /// Region `rc` parked with `people`'s bodies in it: each goes back to the abstract
    /// history, and every journey held in `rc` resumes with half its current leg left.
    /// Anyone else still in `rc` is restored too, with a warning — a body went missing.
    pub fn region_parked(&mut self, rc: RegionCoords, people: Vec<(PersonId, [f32; 3])>) {
        self.running.remove(&rc);
        self.stopping.remove(&rc);
        self.note_positions(people.clone());
        for (p, _pose) in people {
            if self.people.get(&p).is_some_and(|q| q.location == Location::InRegion(rc)) {
                self.restore(p, rc);
            } else {
                ::log::warn!("history: parked region {rc:?} returned {p:?}, who is not in it");
            }
        }
        self.release(rc, true);
    }

    /// Record where embodied people stand now (region-local translations, from an
    /// autosave or a park): a later embodiment in the same region spawns them there.
    /// Changes nothing else — nobody is despawned or restored.
    pub fn note_positions(&mut self, people: Vec<(PersonId, [f32; 3])>) {
        for (p, pos) in people {
            if let Some(Location::InRegion(rc)) = self.people.get(&p).map(|q| q.location) {
                self.positions.insert(p, (rc, pos));
            }
        }
    }

    /// After a load: nothing runs yet, so everyone embodied goes back to the abstract
    /// history and every held journey resumes, as if their regions had parked.
    /// [`super::persist::decode`] calls it.
    pub fn after_load(&mut self) {
        self.running.clear();
        self.stopping.clear();
        self.held_since.clear();
        self.stray_candidates = self
            .people
            .values()
            .filter(|p| p.site.is_some() && matches!(p.abstract_location(), Location::Wandering(_)))
            .map(|p| p.id)
            .collect();
        let mut regions: BTreeSet<RegionCoords> = self
            .people
            .values()
            .filter_map(|p| match p.location {
                Location::InRegion(rc) => Some(rc),
                _ => None,
            })
            .collect();
        regions.extend(self.journeys.values().filter_map(|jr| jr.held_in));
        for rc in regions {
            self.release(rc, false);
        }
    }

    /// Restore everyone still in `rc` (warning about the living when `warn`) and resume
    /// every journey held there.
    fn release(&mut self, rc: RegionCoords, warn: bool) {
        let left: Vec<PersonId> =
            self.people.values().filter(|q| q.location == Location::InRegion(rc)).map(|q| q.id).collect();
        for p in left {
            if warn && self.people[&p].alive() {
                ::log::warn!("history: {p:?} was in region {rc:?} but not parked with it; restoring");
            }
            self.restore(p, rc);
        }
        let held: Vec<JourneyId> =
            self.journeys.values().filter(|jr| jr.held_in == Some(rc)).map(|jr| jr.id).collect();
        for j in held {
            self.journeys.get_mut(&j).unwrap().held_in = None;
            let at = CalendarTick(self.now.0 + self.leg_duration(j) / 2);
            self.schedule_next_leg_from(j, at);
        }
    }

    /// Return `person`, embodied in `rc` (no longer live), to the abstract history. Their
    /// journey, if any, resumes in [`History::release`].
    fn restore(&mut self, person: PersonId, rc: RegionCoords) {
        let Some(p) = self.people.get_mut(&person) else { return };
        let home = std::mem::replace(&mut p.home_location, Location::Unplaced);
        if !p.alive() {
            p.location = home;
            return;
        }
        match home {
            Location::Journey(j) if self.journeys.contains_key(&j) => p.location = Location::Journey(j),
            _ => self.settle(person, rc, home),
        }
    }

    /// `person`, out of any body and any journey, in `region`: back to their site (walking
    /// if it is elsewhere) when they have one, else a stray in `region`. The one rule for a
    /// park and a seam crossing.
    fn settle(&mut self, person: PersonId, region: RegionCoords, home: Location) {
        let site = match home {
            Location::AtSite(s) => Some(s),
            _ => self.people.get(&person).and_then(|p| p.site),
        };
        match site {
            Some(s) => self.walk_home(person, region, s),
            None => {
                if let Some(p) = self.people.get_mut(&person) {
                    p.location = Location::Wandering(region);
                    p.home_location = Location::Unplaced;
                }
            }
        }
    }

    /// `person`, abstract and in `from`, goes back to site `s`: at once when `from` is its
    /// region, else on a `Migrate` journey (no longer a member meanwhile), or as a stray
    /// in `from` with no route.
    fn walk_home(&mut self, person: PersonId, from: RegionCoords, s: SiteId) {
        let Some(site) = self.sites.get_mut(&s) else {
            self.people.get_mut(&person).unwrap().location = Location::Wandering(from);
            return;
        };
        let home = site.region;
        if home == from {
            site.members.insert(person);
            self.people.get_mut(&person).unwrap().location = Location::AtSite(s);
            return;
        }
        let left = site.members.remove(&person);
        self.people.get_mut(&person).unwrap().location = Location::Wandering(from);
        self.stray_candidates.insert(person);
        // No route: they stay a stray in `from`, still a citizen of `s`. A site whose
        // last citizen is walking home to it is not abandoned (the walk's end, by death,
        // abandons it — `remove_traveller`).
        let walking = self.start_journey_from_region(vec![person], from, home, Purpose::Migrate { to: s }).is_some();
        if left && !walking && self.sites.get(&s).is_some_and(|site| site.members.is_empty()) {
            self.abandon_site(s);
        }
    }

    /// The manager moved `person`'s body over a seam into `to`. True when `to` is running
    /// and the manager must deliver the body there; false when the history absorbed them
    /// (their journey goes on abstractly, re-routed if `to` is off its route; anyone else
    /// is settled as by a park: a citizen heads home, a stray wanders in `to`). Only a
    /// crossing out of the region a journey is held in (or of an unheld journey) moves the
    /// journey; it never steps back along its route.
    pub fn person_crossed(&mut self, person: PersonId, to: RegionCoords) -> bool {
        let Some(p) = self.people.get_mut(&person).filter(|p| p.alive()) else { return false };
        let home = p.abstract_location();
        let from = match p.location {
            Location::InRegion(r) => Some(r),
            _ => None,
        };
        if self.running.contains(&to) {
            p.location = Location::InRegion(to);
            p.home_location = home;
            if let Location::Journey(j) = home {
                self.journey_delivered(j, person, from, to);
            }
            return true;
        }
        p.location = home;
        p.home_location = Location::Unplaced;
        match home {
            Location::Journey(j) if self.journeys.contains_key(&j) => self.journey_absorbed(j, person, from, to),
            _ => self.settle(person, to, home),
        }
        false
    }

    /// A traveller of `j` walked from `from` into running region `to`.
    fn journey_delivered(&mut self, j: JourneyId, person: PersonId, from: Option<RegionCoords>, to: RegionCoords) {
        let Some(jr) = self.journeys.get_mut(&j) else { return };
        let held = jr.held_in;
        if held == Some(to) {
            // The party is walking here already: join it.
            if let Some(&next) = jr.route.get(jr.leg as usize + 1) {
                self.emit(to, GameEventKind::WalkOut(person, next));
            }
            return;
        }
        if held.is_some() && held != from {
            // Held where other bodies walk it: this one idles here until the region parks.
            return;
        }
        if let Some(i) = jr.route.iter().position(|r| *r == to) {
            jr.leg = jr.leg.max(i as u32);
            if i + 1 >= jr.route.len() {
                self.arrive(j);
                self.hold_and_walk_out(j, to);
            } else {
                let next = jr.route[i + 1];
                self.hold(j, to);
                self.emit(to, GameEventKind::WalkOut(person, next));
            }
            return;
        }
        let dest = *jr.route.last().expect("a route has a region");
        if self.reroute_from(j, to, dest, None) {
            let next = self.journeys[&j].route[1];
            self.hold(j, to);
            self.emit(to, GameEventKind::WalkOut(person, next));
        } else {
            // No way on from here: they leave the party (settled when `to` parks).
            self.remove_traveller(j, person);
            self.people.get_mut(&person).unwrap().home_location = Location::Wandering(to);
            self.stray_candidates.insert(person);
        }
    }

    /// A traveller of `j` walked from `from` into `to`, which is not running.
    fn journey_absorbed(&mut self, j: JourneyId, person: PersonId, from: Option<RegionCoords>, to: RegionCoords) {
        let jr = self.journeys.get_mut(&j).unwrap();
        let held = jr.held_in;
        if held.is_some() && held != from {
            // Held where other bodies walk it: rejoin, and go on when that region parks.
            return;
        }
        jr.held_in = None;
        let unheld = held.is_some();
        if let Some(i) = jr.route.iter().position(|r| *r == to) {
            let moved = i as u32 > jr.leg;
            if moved {
                jr.leg = i as u32;
            }
            if moved || unheld {
                if jr.leg as usize + 1 >= jr.route.len() {
                    self.arrive(j);
                } else {
                    let at = CalendarTick(self.now.0 + self.leg_duration(j));
                    self.schedule_next_leg_from(j, at);
                }
            }
            return;
        }
        let dest = *jr.route.last().expect("a route has a region");
        if !self.reroute_from(j, to, dest, None) {
            // No way on from here: they leave the party; the rest is unchanged.
            if let Some(jr) = self.journeys.get_mut(&j) {
                jr.held_in = held;
            }
            self.remove_traveller(j, person);
            self.settle(person, to, Location::Wandering(to));
        }
    }
}
