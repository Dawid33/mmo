//! World history on the main thread. The server sends the sites, the calendar,
//! notices, embark options / results and person info as `ClientUpdateEvent`s;
//! the bridge forwards each as a [`HistoryMsg`] and [`apply_history_msgs`] keeps
//! [`HistoryView`], which the world map / embark / inspector UI reads.

use std::collections::{BTreeMap, VecDeque};

use bevy::prelude::*;
use std::collections::BTreeSet;

use game::history::rules::EMBARK_PLAYER_PARTY_MAX;
use game::history::{embark_allowed_in, CalendarTick, CivId, EmbarkRefusal, PersonId, SiteId, SocietyGrid};
use game::RegionCoords;
use game::{EmbarkOptions, EmbarkRequest, PersonInfo, SiteMarker};

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

/// The tile check the server applies, run on the client's markers.
pub fn embark_tile_check(grid: &SocietyGrid, sites: &[SiteMarker], region: RegionCoords) -> Result<(), EmbarkRefusal> {
    embark_allowed_in(grid, sites.iter().map(|s| (s.site, s.region, s.abandoned)), region)
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
    }
}

impl HistoryView {
    /// The embark refusal still worth showing at `now`.
    pub fn visible_embark_refusal(&self, now: f32) -> Option<EmbarkRefusal> {
        self.embark_refusal
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

/// Bridge -> [`HistoryView`].
#[derive(Message, Debug, Clone)]
pub enum HistoryMsg {
    Sites(Vec<SiteMarker>),
    Calendar(CalendarTick),
    Notice(String),
    EmbarkOptions(EmbarkOptions),
    EmbarkResult(Result<SiteId, EmbarkRefusal>),
    PersonInfo(PersonInfo),
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

/// Drop notices shown for `NOTICE_SHOW_SECS` or longer; the oldest left, if any.
fn live_notice(notices: &mut VecDeque<(String, f32)>, now: f32) -> Option<&str> {
    while notices.front().is_some_and(|(_, at)| now - at >= NOTICE_SHOW_SECS) {
        notices.pop_front();
    }
    notices.front().map(|(t, _)| t.as_str())
}

/// Whether a cached person received at `received` should be asked for again at `now`.
pub fn person_info_refresh_due(received: Option<f32>, now: f32) -> bool {
    received.is_none_or(|at| now - at >= PERSON_INFO_REFRESH_SECS)
}

impl HistoryView {
    /// The oldest notice younger than `NOTICE_SHOW_SECS`; expired ones are dropped.
    pub fn current_notice(&mut self, now: f32) -> Option<&str> {
        live_notice(&mut self.notices, now)
    }

    /// Whether `person` should be requested from the server now.
    pub fn person_refresh_due(&self, person: PersonId, now: f32) -> bool {
        person_info_refresh_due(self.people.get(&person).map(|(_, at)| *at), now)
    }
}

pub fn apply_history_msgs(mut msgs: MessageReader<HistoryMsg>, mut view: ResMut<HistoryView>, time: Res<Time>) {
    let now = time.elapsed_secs();
    for msg in msgs.read() {
        match msg {
            HistoryMsg::Sites(v) => view.sites = v.clone(),
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
            HistoryMsg::PersonInfo(p) => {
                view.people.insert(p.person, (p.clone(), now));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_expire_in_order() {
        let mut v = HistoryView::default();
        v.notices.push_back(("a".into(), 0.0));
        v.notices.push_back(("b".into(), 4.0));
        assert_eq!(v.current_notice(1.0), Some("a"));
        assert_eq!(v.current_notice(NOTICE_SHOW_SECS - 0.1), Some("a"));
        assert_eq!(v.current_notice(NOTICE_SHOW_SECS), Some("b"));
        assert_eq!(v.notices.len(), 1);
        assert_eq!(v.current_notice(4.0 + NOTICE_SHOW_SECS), None);
        assert!(v.notices.is_empty());
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
