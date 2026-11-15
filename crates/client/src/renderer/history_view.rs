//! World history on the main thread. The server sends the sites, the calendar,
//! notices, embark options / results and person info as `ClientUpdateEvent`s;
//! the bridge forwards each as a [`HistoryMsg`] and [`apply_history_msgs`] keeps
//! [`HistoryView`], which the world map / embark / inspector UI reads.

use std::collections::{BTreeMap, VecDeque};

use bevy::prelude::*;
use std::collections::BTreeSet;

use game::history::rules::EMBARK_PLAYER_PARTY_MAX;
use game::history::{player_embark_allowed_in, CalendarTick, CivId, EmbarkRefusal, PersonId, SiteId, SocietyGrid};
use game::RegionCoords;
use game::history::{
    AdventureRefusal, LegendLine, LegendsFilter, LegendsPage, LegendsQuery, LEGENDS_PAGE_DEFAULT,
};
use game::{CivMarker, EmbarkOptions, EmbarkRequest, PersonInfo, SiteMarker};

/// How long a notice stays on screen.
pub const NOTICE_SHOW_SECS: f32 = 6.0;
/// A cached person is asked for again once it is this old.
pub const PERSON_INFO_REFRESH_SECS: f32 = 5.0;
/// Notices kept (older ones are dropped when more arrive).
pub const MAX_NOTICES: usize = 16;

#[derive(Resource, Default)]
pub struct HistoryView {
    pub sites: Vec<SiteMarker>,
    pub calendar: Option<CalendarTick>,
    /// (text, `Time::elapsed_secs` when received), oldest first.
    pub notices: VecDeque<(String, f32)>,
    pub embark_options: Option<EmbarkOptions>,
    pub embark_result: Option<Result<SiteId, EmbarkRefusal>>,
    /// Person info and when it was received (secs).
    pub people: BTreeMap<PersonId, (PersonInfo, f32)>,
    /// Who the embark panel has ticked.
    pub selection: EmbarkSelection,
    /// `RequestEmbarkOptions` was sent for this session's panel.
    pub options_requested: bool,
    /// An `Embark` / `ReturnTo` is out and unanswered.
    pub embark_pending: bool,
    /// The last refusal and `Time::elapsed_secs` when it arrived (shown for
    /// `world_map::REFUSAL_SHOW_SECS`).
    pub embark_refusal: Option<(EmbarkRefusal, f32)>,
    /// The player retired their fortress: the embark panel is back (set by the
    /// Retire flow).
    pub retired: bool,
    /// The player's fortress (set by a successful Embark / Return, cleared by
    /// Retire): the Esc menu offers Retire while it is `Some`.
    pub fortress: Option<SiteId>,
    /// The region the fortress stands in, from the server's `Fortress` packet
    /// (cleared by Retire with `fortress`): the centre of the simulated
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
    Fortress,
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

/// The embark panel's choice: a civ (index into `EmbarkOptions::civs`) and at
/// most `EMBARK_PLAYER_PARTY_MAX` of its people. Pure, so it is unit-tested.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct EmbarkSelection {
    pub civ: Option<usize>,
    pub chosen: BTreeSet<PersonId>,
}

impl EmbarkSelection {
    /// Pick civ `index`; a different civ drops the people chosen so far.
    pub fn pick_civ(&mut self, index: usize) {
        if self.civ != Some(index) {
            self.civ = Some(index);
            self.chosen.clear();
        }
    }

    /// Tick or untick `person`; an eighth tick is refused (`false`).
    pub fn toggle(&mut self, person: PersonId) -> bool {
        if self.chosen.remove(&person) {
            return true;
        }
        if self.chosen.len() >= EMBARK_PLAYER_PARTY_MAX as usize {
            return false;
        }
        self.chosen.insert(person);
        true
    }

    /// Whether a party of this size may be sent.
    pub fn party_ok(&self) -> bool {
        (1..=EMBARK_PLAYER_PARTY_MAX as usize).contains(&self.chosen.len())
    }

    /// "n / 7".
    pub fn counter(&self) -> String {
        format!("{} / {}", self.chosen.len(), EMBARK_PLAYER_PARTY_MAX)
    }

    /// The request for `region`, `None` without a (valid) civ or a legal party.
    pub fn request(&self, options: &EmbarkOptions, region: RegionCoords) -> Option<EmbarkRequest> {
        let civ = options.civs.get(self.civ?)?;
        self.party_ok().then(|| EmbarkRequest { civ: civ.civ, people: self.chosen.iter().copied().collect(), region })
    }

    /// Forget choices the options no longer offer (new options arrived).
    pub fn retain_offered(&mut self, options: &EmbarkOptions) {
        let Some(civ) = self.civ.and_then(|i| options.civs.get(i)) else {
            *self = Self::default();
            return;
        };
        let offered: BTreeSet<PersonId> = civ.sites.iter().flat_map(|s| s.people.iter().map(|p| p.person)).collect();
        self.chosen.retain(|p| offered.contains(p));
    }
}

/// One line of the panel's people list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbarkRowView {
    pub site: i32,
    /// -1 for a site header.
    pub person: i32,
    pub text: String,
    pub checked: bool,
}

/// The picked civ's sites as headers, each followed by its people.
pub fn embark_rows(options: &EmbarkOptions, sel: &EmbarkSelection) -> Vec<EmbarkRowView> {
    let Some(civ) = sel.civ.and_then(|i| options.civs.get(i)) else { return Vec::new() };
    let mut rows = Vec::new();
    for (si, site) in civ.sites.iter().enumerate() {
        rows.push(EmbarkRowView { site: si as i32, person: -1, text: site.name.clone(), checked: false });
        for (pi, p) in site.people.iter().enumerate() {
            let sex = match p.sex {
                game::history::Sex::Female => "F",
                game::history::Sex::Male => "M",
            };
            let mut text = format!("{}, {}, {sex}", p.name, p.age_years);
            if let Some(spouse) = p.spouse {
                let name = civ
                    .sites
                    .iter()
                    .flat_map(|s| s.people.iter())
                    .find(|q| q.person == spouse)
                    .map_or("someone", |q| q.name.as_str());
                text.push_str(&format!(" (married to {name})"));
            }
            rows.push(EmbarkRowView {
                site: si as i32,
                person: pi as i32,
                text,
                checked: sel.chosen.contains(&p.person),
            });
        }
    }
    rows
}

/// The tile check the server applies to the player's embark, run on the client's markers.
pub fn embark_tile_check(grid: &SocietyGrid, sites: &[SiteMarker], region: RegionCoords) -> Result<(), EmbarkRefusal> {
    player_embark_allowed_in(grid, sites.iter().map(|s| (s.site, s.region, s.abandoned)), region)
}

/// What the player reads for a refusal.
pub fn embark_refusal_text(why: EmbarkRefusal) -> &'static str {
    match why {
        EmbarkRefusal::OffMap => "Not on the map",
        EmbarkRefusal::NotHabitable => "Nobody can live there",
        EmbarkRefusal::Occupied => "A site is already there",
        EmbarkRefusal::TooClose { .. } => "Too close to another site",
        EmbarkRefusal::NoHistory => "This world has no history",
        EmbarkRefusal::AlreadyHasFortress => "Retire your fortress first",
        EmbarkRefusal::PartySize => "Choose 1 to 7 people",
        EmbarkRefusal::UnavailablePerson(_) => "Someone chosen is no longer available",
        EmbarkRefusal::NotPlayerFounded => "Not a fortress you founded",
        EmbarkRefusal::InUse => "Another player is in that fortress",
        EmbarkRefusal::TooSteep => "Too steep: the side of a mountain",
        EmbarkRefusal::RegionUnavailable => "That region could not be loaded; try again shortly",
        EmbarkRefusal::WorldUnavailable => "The world is not answering; try again shortly",
    }
}

impl HistoryView {
    /// The embark refusal still worth showing at `now`.
    pub fn visible_embark_refusal(&self, now: f32) -> Option<EmbarkRefusal> {
        self.embark_refusal
            .filter(|(_, at)| now - *at < super::world_map::REFUSAL_SHOW_SECS)
            .map(|(why, _)| why)
    }

    /// The adventure refusal still worth showing at `now`.
    pub fn visible_adventure_refusal(&self, now: f32) -> Option<AdventureRefusal> {
        self.adventure_refusal
            .filter(|(_, at)| now - *at < super::world_map::REFUSAL_SHOW_SECS)
            .map(|(why, _)| why)
    }

    /// Returnable sites, in the order the panel lists them.
    pub fn returnable(&self) -> Vec<&SiteMarker> {
        self.sites.iter().filter(|s| s.returnable).collect()
    }

    /// Name of `civ`, from the embark options when they list it.
    pub fn civ_name(&self, civ: CivId) -> String {
        self.embark_options
            .as_ref()
            .and_then(|o| o.civs.iter().find(|c| c.civ == civ))
            .map_or_else(|| format!("civ {}", civ.0), |c| c.name.clone())
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
    EmbarkOptions(EmbarkOptions),
    EmbarkResult(Result<SiteId, EmbarkRefusal>),
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
            HistoryMsg::EmbarkOptions(o) => {
                view.selection.retain_offered(o);
                view.embark_options = Some(o.clone());
            }
            HistoryMsg::EmbarkResult(r) => view.embark_result = Some(*r),
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

    use game::{CivOption, PersonOption, SiteOption};

    fn options(people: u32) -> EmbarkOptions {
        EmbarkOptions {
            civs: vec![CivOption {
                civ: CivId(0),
                name: "Dwarves".into(),
                sites: vec![SiteOption {
                    site: SiteId(0),
                    name: "Home".into(),
                    region: RegionCoords::new(1, 1),
                    people: (0..people)
                        .map(|i| PersonOption {
                            person: PersonId(i),
                            name: format!("P{i}"),
                            age_years: 20 + i,
                            sex: game::history::Sex::Male,
                            spouse: (i == 1).then_some(PersonId(0)),
                        })
                        .collect(),
                }],
            }],
        }
    }

    #[test]
    fn embark_selection_caps_at_seven() {
        let mut sel = EmbarkSelection::default();
        sel.pick_civ(0);
        assert!(!sel.party_ok(), "an empty party is not sendable");
        for i in 0..EMBARK_PLAYER_PARTY_MAX {
            assert!(sel.toggle(PersonId(i)));
        }
        assert_eq!(sel.counter(), "7 / 7");
        assert!(sel.party_ok());
        assert!(!sel.toggle(PersonId(7)), "the eighth is refused");
        assert_eq!(sel.chosen.len(), 7);
        assert!(sel.toggle(PersonId(3)), "unticking works at the cap");
        assert!(sel.toggle(PersonId(7)));
        sel.pick_civ(0);
        assert_eq!(sel.chosen.len(), 7, "re-picking the same civ keeps the party");
        sel.pick_civ(1);
        assert!(sel.chosen.is_empty(), "another civ starts over");
    }

    #[test]
    fn embark_rows_list_sites_and_spouses() {
        let o = options(3);
        let mut sel = EmbarkSelection::default();
        assert!(embark_rows(&o, &sel).is_empty());
        sel.pick_civ(0);
        sel.toggle(PersonId(1));
        let rows = embark_rows(&o, &sel);
        assert_eq!(rows.len(), 4);
        assert_eq!((rows[0].person, rows[0].text.as_str()), (-1, "Home"));
        assert!(rows[2].checked && !rows[1].checked);
        assert!(rows[2].text.contains("married to P0"));
        let req = sel.request(&o, RegionCoords::new(4, 4)).expect("one person");
        assert_eq!((req.civ, req.people), (CivId(0), vec![PersonId(1)]));
    }

    #[test]
    fn stale_choices_drop_when_options_change() {
        let mut sel = EmbarkSelection::default();
        sel.pick_civ(0);
        sel.toggle(PersonId(0));
        sel.toggle(PersonId(2));
        sel.retain_offered(&options(2));
        assert_eq!(sel.chosen.iter().copied().collect::<Vec<_>>(), vec![PersonId(0)]);
    }
}
