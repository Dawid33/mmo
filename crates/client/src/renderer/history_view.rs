//! World history on the main thread. The server sends the sites, the calendar,
//! notices, embark options / results and person info as `ClientUpdateEvent`s;
//! the bridge forwards each as a [`HistoryMsg`] and [`apply_history_msgs`] keeps
//! [`HistoryView`], which the world map / embark / inspector UI reads.

use std::collections::{BTreeMap, VecDeque};

use bevy::prelude::*;
use game::history::{CalendarTick, EmbarkRefusal, PersonId, SiteId};
use game::{EmbarkOptions, PersonInfo, SiteMarker};

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
            HistoryMsg::EmbarkOptions(o) => view.embark_options = Some(o.clone()),
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
}
