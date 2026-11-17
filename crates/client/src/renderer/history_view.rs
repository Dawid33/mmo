//! World history on the main thread. The server sends the sites, the calendar,
//! notices and person info as `ClientUpdateEvent`s;
//! the bridge forwards each as a [`HistoryMsg`] and [`apply_history_msgs`] keeps
//! [`HistoryView`], which the world map / embark / inspector UI reads.

use std::collections::{BTreeMap, VecDeque};

use bevy::prelude::*;

use game::history::{CalendarTick, CivId, PersonId, SiteId};
use game::RegionCoords;
use game::history::{
    AdventureRefusal, LegendLine, LegendsFilter, LegendsPage, LegendsQuery, LEGENDS_PAGE_DEFAULT,
};
use game::{CivMarker, PersonInfo, SiteMarker};

/// How long a notice stays on screen.
pub const NOTICE_SHOW_SECS: f32 = 6.0;
/// A cached person is asked for again once it is this old.
pub const PERSON_INFO_REFRESH_SECS: f32 = 5.0;
/// Notices kept (older ones are dropped when more arrive).
pub const MAX_NOTICES: usize = 16;

#[derive(Resource, Default)]
pub struct HistoryView {
    pub sites: Vec<SiteMarker>,
    /// The server's `Sites` arrived (they may be empty: a world without history).
    pub sites_received: bool,
    pub calendar: Option<CalendarTick>,
    /// (text, `Time::elapsed_secs` when received), oldest first.
    pub notices: VecDeque<(String, f32)>,
    /// Person info and when it was received (secs).
    pub people: BTreeMap<PersonId, (PersonInfo, f32)>,
    /// The player's fortress (the server's `Fortress` packet; deleted with the
    /// embark types).
    pub fortress: Option<SiteId>,
    /// The region the fortress stands in, from the server's `Fortress` packet
    /// : the centre of the simulated
    /// fortress neighbourhood (`world_scale::FORTRESS_NEAR_RADIUS`).
    pub fortress_region: Option<RegionCoords>,
    /// Every civ's name (server `Civs`, on connect).
    pub civs: Vec<CivMarker>,
    /// The embark screen's tab.
    pub tab: EmbarkTab,
    /// The Adventure tab's choice.
    pub adventure: AdventureSelection,
    /// The last answer to a start-adventure request.
    pub adventure_result: Option<Result<SiteId, AdventureRefusal>>,
    /// A `StartAdventure` is out and unanswered.
    pub adventure_pending: bool,
    /// The last adventure refusal and `Time::elapsed_secs` when it arrived.
    pub adventure_refusal: Option<(AdventureRefusal, f32)>,
    /// The site the player is adventuring from.
    pub adventuring: Option<SiteId>,
    /// The Legends tab's paged log.
    pub legends: LegendsState,
}

/// Which tab of the embark screen is open.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbarkTab {
    #[default]
    Adventure,
    Legends,
}

/// The Adventure tab's choice: a civ, then one of its populated sites.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct AdventureSelection {
    pub civ: Option<CivId>,
    pub site: Option<SiteId>,
}

/// A site an adventure may start from.
pub(crate) fn populated(s: &SiteMarker) -> bool {
    !s.abandoned && s.population > 0
}

impl AdventureSelection {
    /// Civs with at least one populated site, in id order.
    pub fn civs(sites: &[SiteMarker], civs: &[CivMarker]) -> Vec<CivMarker> {
        let mut out: Vec<CivMarker> =
            civs.iter().filter(|c| sites.iter().any(|s| s.civ == c.civ && populated(s))).cloned().collect();
        out.sort_by_key(|c| c.civ);
        out
    }

    /// The picked civ's populated sites.
    pub fn sites(&self, sites: &[SiteMarker]) -> Vec<SiteMarker> {
        let Some(civ) = self.civ else { return Vec::new() };
        sites.iter().filter(|s| s.civ == civ && populated(s)).cloned().collect()
    }

    pub fn pick_civ(&mut self, civ: CivId) {
        if self.civ != Some(civ) {
            self.civ = Some(civ);
            self.site = None;
        }
    }

    pub fn pick_site(&mut self, site: SiteId) {
        self.site = Some(site);
    }

    /// Drop a picked site that is no longer populated.
    pub fn retain_offered(&mut self, sites: &[SiteMarker]) {
        if let Some(site) = self.site {
            if !sites.iter().any(|s| s.site == site && populated(s)) {
                self.site = None;
            }
        }
    }
}

/// The Legends tab's paged log. Pure, so it is unit-tested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegendsState {
    pub filter: LegendsFilter,
    pub lines: Vec<LegendLine>,
    /// Cursor for the next (older) page; `None` = no older match.
    pub next: Option<u32>,
    pub total: u32,
    /// The query sent and not yet answered.
    pub pending: Option<LegendsQuery>,
}

impl Default for LegendsState {
    fn default() -> Self {
        Self { filter: LegendsFilter::All, lines: Vec::new(), next: None, total: 0, pending: None }
    }
}

impl LegendsState {
    /// Start over with `filter`: clears the lines and returns the query to send.
    pub fn query_first(&mut self, filter: LegendsFilter) -> LegendsQuery {
        self.filter = filter.clone();
        self.lines.clear();
        self.next = None;
        self.total = 0;
        let q = LegendsQuery { filter, before: None, limit: LEGENDS_PAGE_DEFAULT };
        self.pending = Some(q.clone());
        q
    }

    /// The query for the next older page; `None` while one is pending or none remains.
    pub fn query_next(&mut self) -> Option<LegendsQuery> {
        if self.pending.is_some() {
            return None;
        }
        let before = self.next?;
        let q = LegendsQuery { filter: self.filter.clone(), before: Some(before), limit: LEGENDS_PAGE_DEFAULT };
        self.pending = Some(q.clone());
        Some(q)
    }

    /// Append `page` if it answers the pending query; a stale page is ignored.
    pub fn accept(&mut self, page: LegendsPage) -> bool {
        if self.pending.as_ref() != Some(&page.query) {
            return false;
        }
        self.pending = None;
        self.lines.extend(page.lines);
        self.next = page.next;
        self.total = page.total;
        true
    }
}

impl HistoryView {
    /// The adventure refusal still worth showing at `now`.
    pub fn visible_adventure_refusal(&self, now: f32) -> Option<AdventureRefusal> {
        self.adventure_refusal
            .filter(|(_, at)| now - *at < super::world_map::REFUSAL_SHOW_SECS)
            .map(|(why, _)| why)
    }

    /// Name of `civ`, from the server's civ list.
    pub fn civ_name(&self, civ: CivId) -> String {
        self.civs.iter().find(|c| c.civ == civ).map_or_else(|| format!("civ {}", civ.0), |c| c.name.clone())
    }
}

/// The inspector's lines for one selected person: `info` once it arrived,
/// otherwise a placeholder from the id. Empty parts are left out.
pub fn person_lines(person: PersonId, info: Option<&PersonInfo>) -> Vec<String> {
    let Some(info) = info else { return vec![format!("Dwarf {}", person.0)] };
    let mut lines = vec![info.name.clone()];
    lines.push(if info.alive { format!("age {}", info.age_years) } else { format!("age {} (deceased)", info.age_years) });
    if let Some(site) = &info.site {
        lines.push(site.clone());
    }
    if let Some(spouse) = &info.spouse {
        lines.push(format!("married to {spouse}"));
    }
    if !info.parents.is_empty() {
        lines.push(format!("parents: {}", info.parents.join(", ")));
    }
    if !info.children.is_empty() {
        lines.push(format!("children: {}", info.children.join(", ")));
    }
    lines
}

/// Bridge -> [`HistoryView`].
#[derive(Message, Debug, Clone)]
pub enum HistoryMsg {
    Sites(Vec<SiteMarker>),
    Calendar(CalendarTick),
    Notice(String),
    PersonInfo(PersonInfo),
    Fortress(Option<game::HeldFortress>),
    Civs(Vec<CivMarker>),
    Legends(LegendsPage),
    AdventureResult(Result<SiteId, AdventureRefusal>),
}

/// The bridge's writer for [`HistoryMsg`] (a param struct: `drain_client_updates`
/// is at the 16-parameter system limit).
#[derive(bevy::ecs::system::SystemParam)]
pub struct HistoryOut<'w> {
    msgs: Option<ResMut<'w, Messages<HistoryMsg>>>,
    /// Rides along for the same reason: the world map's writer.
    pub world_map: Option<ResMut<'w, Messages<super::world_map::WorldMapMsg>>>,
}

impl HistoryOut<'_> {
    pub fn write(&mut self, msg: HistoryMsg) {
        if let Some(m) = self.msgs.as_mut() {
            m.write(msg);
        }
    }
}

/// Drop notices shown for `NOTICE_SHOW_SECS` or longer; whether any was dropped.
fn prune_notices(notices: &mut VecDeque<(String, f32)>, now: f32) -> bool {
    let before = notices.len();
    while notices.front().is_some_and(|(_, at)| now - at >= NOTICE_SHOW_SECS) {
        notices.pop_front();
    }
    notices.len() != before
}

/// Whether a cached person received at `received` should be asked for again at `now`.
pub fn person_info_refresh_due(received: Option<f32>, now: f32) -> bool {
    received.is_none_or(|at| now - at >= PERSON_INFO_REFRESH_SECS)
}

impl HistoryView {
    /// The oldest notice younger than `NOTICE_SHOW_SECS` (a pure read; `apply_history_msgs` prunes).
    pub fn current_notice(&self, now: f32) -> Option<&str> {
        self.notices.iter().find(|(_, at)| now - at < NOTICE_SHOW_SECS).map(|(t, _)| t.as_str())
    }

    /// Whether `person` should be requested from the server now.
    pub fn person_refresh_due(&self, person: PersonId, now: f32) -> bool {
        person_info_refresh_due(self.people.get(&person).map(|(_, at)| *at), now)
    }
}

pub fn apply_history_msgs(mut msgs: MessageReader<HistoryMsg>, mut view: ResMut<HistoryView>, time: Res<Time>) {
    let now = time.elapsed_secs();
    // Only an actual expiry marks the resource changed.
    prune_notices(&mut view.bypass_change_detection().notices, now).then(|| view.set_changed());
    for msg in msgs.read() {
        match msg {
            HistoryMsg::Sites(v) => {
                view.adventure.retain_offered(v);
                view.sites = v.clone();
                view.sites_received = true;
            }
            HistoryMsg::Fortress(f) => {
                view.fortress = f.map(|f| f.site);
                view.fortress_region = f.map(|f| f.region);
            }
            HistoryMsg::Calendar(c) => view.calendar = Some(*c),
            HistoryMsg::Notice(n) => {
                view.notices.push_back((n.clone(), now));
                while view.notices.len() > MAX_NOTICES {
                    view.notices.pop_front();
                }
            }
            HistoryMsg::Civs(c) => view.civs = c.clone(),
            HistoryMsg::Legends(p) => {
                view.legends.accept(p.clone());
            }
            HistoryMsg::AdventureResult(r) => view.adventure_result = Some(*r),
            HistoryMsg::PersonInfo(p) => {
                view.people.insert(p.person, (p.clone(), now));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(id: u32, civ: u32, population: u32, abandoned: bool) -> SiteMarker {
        SiteMarker {
            site: SiteId(id),
            civ: CivId(civ),
            region: RegionCoords::new(0, 0),
            name: format!("s{id}"),
            population,
            abandoned,
            returnable: false,
        }
    }

    fn civ(id: u32) -> CivMarker {
        CivMarker { civ: CivId(id), name: format!("c{id}") }
    }

    fn line(index: u32) -> LegendLine {
        LegendLine { index, text: String::new(), sites: vec![], people: vec![] }
    }

    #[test]
    fn adventure_lists_only_populated_sites() {
        let sites = [site(1, 1, 5, false), site(2, 1, 0, false), site(3, 1, 4, true), site(4, 2, 0, false)];
        let civs = [civ(2), civ(1)];
        let offered = AdventureSelection::civs(&sites, &civs);
        assert_eq!(offered, vec![civ(1)]);
        let mut sel = AdventureSelection::default();
        assert!(sel.sites(&sites).is_empty());
        sel.pick_civ(CivId(1));
        let got: Vec<_> = sel.sites(&sites).iter().map(|s| s.site).collect();
        assert_eq!(got, vec![SiteId(1)]);
    }

    #[test]
    fn adventure_pick_civ_clears_site() {
        let mut sel = AdventureSelection::default();
        sel.pick_civ(CivId(1));
        sel.pick_site(SiteId(1));
        sel.pick_civ(CivId(1));
        assert_eq!(sel.site, Some(SiteId(1)));
        sel.pick_civ(CivId(2));
        assert_eq!(sel.site, None);
    }

    #[test]
    fn adventure_retain_drops_depopulated_site() {
        let mut sel = AdventureSelection::default();
        sel.pick_civ(CivId(1));
        sel.pick_site(SiteId(1));
        sel.retain_offered(&[site(1, 1, 3, false)]);
        assert_eq!(sel.site, Some(SiteId(1)));
        sel.retain_offered(&[site(1, 1, 0, false)]);
        assert_eq!(sel.site, None);
    }

    #[test]
    fn legends_paging_appends_and_stops() {
        let mut st = LegendsState::default();
        let q1 = st.query_first(LegendsFilter::All);
        assert_eq!((q1.before, q1.limit), (None, LEGENDS_PAGE_DEFAULT));
        assert!(st.query_next().is_none(), "pending");
        assert!(st.accept(LegendsPage { query: q1, lines: vec![line(9), line(8)], next: Some(8), total: 3 }));
        let q2 = st.query_next().expect("older page");
        assert_eq!(q2.before, Some(8));
        assert!(st.accept(LegendsPage { query: q2, lines: vec![line(7)], next: None, total: 3 }));
        assert_eq!(st.lines.len(), 3);
        assert!(st.query_next().is_none());
    }

    #[test]
    fn legends_stale_page_dropped() {
        let mut st = LegendsState::default();
        let a = st.query_first(LegendsFilter::All);
        st.query_first(LegendsFilter::Civ(CivId(1)));
        assert!(!st.accept(LegendsPage { query: a, lines: vec![line(1)], next: None, total: 1 }));
        assert!(st.lines.is_empty());
    }

    #[test]
    fn notices_expire_in_order() {
        let mut v = HistoryView::default();
        v.notices.push_back(("a".into(), 0.0));
        v.notices.push_back(("b".into(), 4.0));
        assert_eq!(v.current_notice(1.0), Some("a"));
        assert_eq!(v.current_notice(NOTICE_SHOW_SECS - 0.1), Some("a"));
        assert_eq!(v.current_notice(NOTICE_SHOW_SECS), Some("b"));
        assert_eq!(v.notices.len(), 2, "reading never prunes");
        assert_eq!(v.current_notice(4.0 + NOTICE_SHOW_SECS), None);
        assert!(!prune_notices(&mut VecDeque::new(), 0.0));
        assert!(prune_notices(&mut v.notices, NOTICE_SHOW_SECS));
        assert_eq!(v.notices.len(), 1);
        assert!(prune_notices(&mut v.notices, 4.0 + NOTICE_SHOW_SECS));
        assert!(v.notices.is_empty());
    }

    #[test]
    fn person_lines_full_family() {
        let info = PersonInfo {
            person: PersonId(4),
            name: "Urist".into(),
            age_years: 31,
            alive: true,
            site: Some("Boatmurdered".into()),
            spouse: Some("Lorbam".into()),
            parents: vec!["Aban".into(), "Besmar".into()],
            children: vec!["Cog".into()],
        };
        assert_eq!(
            person_lines(PersonId(4), Some(&info)),
            ["Urist", "age 31", "Boatmurdered", "married to Lorbam", "parents: Aban, Besmar", "children: Cog"]
        );
        let lone = PersonInfo { site: None, spouse: None, parents: vec![], children: vec![], alive: false, ..info };
        assert_eq!(person_lines(PersonId(4), Some(&lone)), ["Urist", "age 31 (deceased)"]);
    }

    #[test]
    fn person_lines_pending() {
        assert_eq!(person_lines(PersonId(9), None), ["Dwarf 9"]);
    }

    #[test]
    fn person_info_cache_refresh_due() {
        assert!(person_info_refresh_due(None, 10.0));
        assert!(!person_info_refresh_due(Some(10.0), 10.0 + PERSON_INFO_REFRESH_SECS - 0.1));
        assert!(person_info_refresh_due(Some(10.0), 10.0 + PERSON_INFO_REFRESH_SECS));
    }
}
