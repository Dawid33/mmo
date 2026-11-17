//! What the top-left fortress panel (`crates/slint-ui/ui/fortress-panel.slint`)
//! shows, as pure functions of client state: the Governance lines, one
//! Designations line per order, and People row names. `slint_overlay` pushes
//! them into the UI when their inputs change. Plain lists for now — the
//! People and Designations tabs are expected to be redone (spec
//! `docs/superpowers/specs/2026-11-02-fortress-hud-panels-design.md`).

use std::collections::BTreeMap;

use game::history::{CalendarTick, PersonId};
use game::{Designation, DesignationKind, OpId, PersonInfo, SiteMarker, TilePos};

/// At most this many `RequestPersonInfo`s per second for People rows with
/// no name yet (only while the People tab is open).
pub const PERSON_NAME_REQUESTS_PER_SEC: u32 = 4;

/// The Governance tab: the date when the calendar is known, then the
/// residents of the site the player adventures from, or "No settlement".
pub fn governance_lines(calendar: Option<CalendarTick>, site: Option<&SiteMarker>) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(calendar) = calendar {
        lines.push(format!("Date {}", calendar.date()));
    }
    match site {
        Some(site) => lines.push(format!("Residents {}", site.population)),
        None => lines.push("No settlement".into()),
    }
    lines
}

/// The Designations tab: one line per order (`OpId` order), counting the
/// tiles it still holds — "Dig · priority 4 · 3 tiles".
pub fn designation_lines(designations: &BTreeMap<TilePos, Designation>) -> Vec<String> {
    let mut orders: BTreeMap<OpId, (DesignationKind, u8, usize)> = BTreeMap::new();
    for d in designations.values() {
        orders.entry(d.op).or_insert((d.kind, d.priority, 0)).2 += 1;
    }
    orders
        .into_values()
        .map(|(kind, priority, tiles)| {
            let unit = if tiles == 1 { "tile" } else { "tiles" };
            format!("{} · priority {priority} · {tiles} {unit}", kind_name(kind))
        })
        .collect()
}

fn kind_name(kind: DesignationKind) -> &'static str {
    match kind {
        DesignationKind::Dig => "Dig",
        DesignationKind::Deconstruct => "Deconstruct",
        DesignationKind::Build(_) => "Build",
        DesignationKind::Chop => "Chop",
    }
}

/// A People row: the person's name once their info has arrived.
pub fn person_name(person: PersonId, info: Option<&PersonInfo>) -> String {
    info.map_or_else(|| format!("Person #{}", person.0), |i| i.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(kind: DesignationKind, seq: u32, priority: u8) -> Designation {
        Designation { kind, op: OpId { client: 0, seq }, priority }
    }

    #[test]
    fn designation_lines_group_by_order() {
        let mut map = BTreeMap::new();
        for x in 0..3 {
            map.insert([x, 0, 0], d(DesignationKind::Dig, 1, 4));
        }
        map.insert([9, 0, 0], d(DesignationKind::Chop, 2, 2));
        assert_eq!(designation_lines(&map), ["Dig · priority 4 · 3 tiles", "Chop · priority 2 · 1 tile"]);
        assert!(designation_lines(&BTreeMap::new()).is_empty());
    }

    #[test]
    fn governance_without_site() {
        assert_eq!(governance_lines(None, None), ["No settlement"]);
        let lines = governance_lines(Some(CalendarTick(0)), None);
        assert_eq!(lines, [format!("Date {}", CalendarTick(0).date()), "No settlement".to_string()]);
    }

    #[test]
    fn governance_with_site() {
        let site = SiteMarker {
            site: game::history::SiteId(1),
            civ: game::history::CivId(0),
            region: game::RegionCoords::new(0, 0),
            name: "Ustuth".into(),
            population: 7,
            abandoned: false,
        };
        assert_eq!(governance_lines(None, Some(&site)), ["Residents 7"]);
    }

    #[test]
    fn person_name_falls_back() {
        assert_eq!(person_name(PersonId(12), None), "Person #12");
        let info = PersonInfo {
            person: PersonId(12),
            name: "Urist".into(),
            age_years: 40,
            alive: true,
            site: None,
            spouse: None,
            parents: Vec::new(),
            children: Vec::new(),
        };
        assert_eq!(person_name(PersonId(12), Some(&info)), "Urist");
    }
}
