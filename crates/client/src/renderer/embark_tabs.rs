//! The embark screen's Adventure and Legends tabs (`ui/embark.slint`): the
//! pure mapping between [`HistoryView`] and the `GameUi` properties — civ /
//! site lists and their indices, the Legends filter column and header — and
//! [`sync_embark_tabs`], which pushes them only when they changed. The
//! `UiAction`s the tabs send are applied in `slint_overlay::apply_ui_actions`.

use game::history::{CivId, LegendLine, LegendsFilter, SiteId};
use game::{CivMarker, RegionCoords, SiteMarker, WorldMap};
use slint_ui::slint::{Model, ModelRc, SharedString, VecModel};
use slint_ui::{GameUi, LegendsFilterRow};

use super::history_view::{AdventureSelection, EmbarkTab, HistoryView};
use super::slint_overlay::map_tile;

/// The tab of the Slint `embark-tab` index (0 Adventure, 1 Legends);
/// Adventure for an unknown index.
pub fn embark_tab(index: i32) -> EmbarkTab {
    match index {
        1 => EmbarkTab::Legends,
        _ => EmbarkTab::Adventure,
    }
}

/// The Adventure tab's civ at button `index`.
pub fn adventure_civ_at(sites: &[SiteMarker], civs: &[CivMarker], index: i32) -> Option<CivId> {
    let i = usize::try_from(index).ok()?;
    AdventureSelection::civs(sites, civs).get(i).map(|c| c.civ)
}

/// The Adventure tab's site at button `index` (the picked civ's populated sites).
pub fn adventure_site_at(sel: &AdventureSelection, sites: &[SiteMarker], index: i32) -> Option<SiteMarker> {
    let i = usize::try_from(index).ok()?;
    sel.sites(sites).into_iter().nth(i)
}

/// A populated site in `region` (a site marker clicked on the map).
pub fn adventure_site_in(sites: &[SiteMarker], region: RegionCoords) -> Option<&SiteMarker> {
    sites.iter().find(|s| s.region == region && !s.abandoned && s.population > 0)
}

/// "<site> — pop N".
pub fn adventure_site_text(site: &SiteMarker) -> String {
    format!("{} \u{2014} pop {}", site.name, site.population)
}

/// The caption under the site list.
pub fn adventure_region_text(site: Option<&SiteMarker>) -> String {
    match site {
        Some(s) => format!("Start in region ({}, {})", s.region.x, s.region.z),
        None => "Pick a site to start from".to_string(),
    }
}

/// One row of the Legends filter column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegendsFilterEntry {
    pub filter: LegendsFilter,
    pub text: String,
    pub depth: i32,
    /// An abandoned site.
    pub dimmed: bool,
}

/// The name of the "All events" row.
pub const LEGENDS_ALL_TEXT: &str = "All events";

/// "All events", then each civ (id order) followed by its sites (depth 1,
/// abandoned ones dimmed). Sites of a civ not in `civs` are left out.
pub fn legends_filter_entries(civs: &[CivMarker], sites: &[SiteMarker]) -> Vec<LegendsFilterEntry> {
    let mut civs: Vec<&CivMarker> = civs.iter().collect();
    civs.sort_by_key(|c| c.civ);
    let mut out =
        vec![LegendsFilterEntry { filter: LegendsFilter::All, text: LEGENDS_ALL_TEXT.into(), depth: 0, dimmed: false }];
    for civ in civs {
        out.push(LegendsFilterEntry { filter: LegendsFilter::Civ(civ.civ), text: civ.name.clone(), depth: 0, dimmed: false });
        for s in sites.iter().filter(|s| s.civ == civ.civ) {
            out.push(LegendsFilterEntry {
                filter: LegendsFilter::Site(s.site),
                text: s.name.clone(),
                depth: 1,
                dimmed: s.abandoned,
            });
        }
    }
    out
}

/// The row of `filter` in `entries`; -1 when it is not listed (a person filter).
pub fn legends_filter_index(entries: &[LegendsFilterEntry], filter: &LegendsFilter) -> i32 {
    entries.iter().position(|e| &e.filter == filter).map_or(-1, |i| i as i32)
}

/// "<filter name> — N events" (the All row's name for `All`).
pub fn legends_header(entries: &[LegendsFilterEntry], filter: &LegendsFilter, total: u32) -> String {
    let name = match filter {
        LegendsFilter::Person(p) => format!("Person {}", p.0),
        f => entries.iter().find(|e| &e.filter == f).map_or_else(|| "Events".to_string(), |e| e.text.clone()),
    };
    let noun = if total == 1 { "event" } else { "events" };
    format!("{name} \u{2014} {total} {noun}")
}

/// The one site a legends line names, `None` when it names none or several.
pub fn legends_line_site(line: &LegendLine) -> Option<SiteId> {
    match line.sites[..] {
        [site] => Some(site),
        _ => None,
    }
}

/// The tile of `site` on `map`.
pub fn site_tile(map: &WorldMap, sites: &[SiteMarker], site: SiteId) -> Option<(i32, i32)> {
    let s = sites.iter().find(|s| s.site == site)?;
    map_tile(map, s.region)
}

/// Select tile (`col`, `row`) on the map and bring it into view.
pub fn select_map_tile(ui: &GameUi, (col, row): (i32, i32)) {
    if ui.get_selected_col() != col || ui.get_selected_row() != row {
        ui.set_selected_col(col);
        ui.set_selected_row(row);
    }
    ui.invoke_show_map_tile(col, row);
}

/// What the two tabs show, as last pushed.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TabsView {
    adventure_civs: Vec<String>,
    adventure_civ_picked: i32,
    adventure_sites: Vec<String>,
    adventure_site_picked: i32,
    adventure_region: String,
    adventure_enabled: bool,
    legends_filters: Vec<LegendsFilterEntry>,
    legends_filter_picked: i32,
    legends_header: String,
    legends_lines: Vec<String>,
    legends_more: bool,
}

/// The tabs' view of `h`.
pub fn tabs_view(h: &HistoryView) -> TabsView {
    let civs = AdventureSelection::civs(&h.sites, &h.civs);
    let sites = h.adventure.sites(&h.sites);
    let picked_site = h.adventure.site.and_then(|id| sites.iter().position(|s| s.site == id));
    let legends_filters = legends_filter_entries(&h.civs, &h.sites);
    TabsView {
        adventure_civ_picked: h
            .adventure
            .civ
            .and_then(|id| civs.iter().position(|c| c.civ == id))
            .map_or(-1, |i| i as i32),
        adventure_civs: civs.iter().map(|c| c.name.clone()).collect(),
        adventure_site_picked: picked_site.map_or(-1, |i| i as i32),
        adventure_region: adventure_region_text(picked_site.map(|i| &sites[i])),
        adventure_enabled: picked_site.is_some() && !h.adventure_pending,
        adventure_sites: sites.iter().map(adventure_site_text).collect(),
        legends_filter_picked: legends_filter_index(&legends_filters, &h.legends.filter),
        legends_header: legends_header(&legends_filters, &h.legends.filter, h.legends.total),
        legends_filters,
        legends_lines: h.legends.lines.iter().map(|l| l.text.clone()).collect(),
        legends_more: h.legends.next.is_some(),
    }
}

fn strings(v: &[String]) -> ModelRc<SharedString> {
    let v: Vec<SharedString> = v.iter().map(|s| s.as_str().into()).collect();
    ModelRc::new(VecModel::from(v))
}

/// Push the Adventure and Legends tabs' properties that differ from `shown`
/// (`None` = nothing pushed yet). Call when the history changed or nothing
/// was pushed. Appended legends lines are added to the existing model, so
/// the list keeps its scroll position.
pub fn sync_embark_tabs(ui: &GameUi, h: &HistoryView, shown: &mut Option<TabsView>) {
    let new = tabs_view(h);
    let first = shown.is_none();
    let old = shown.take().unwrap_or_default();
    if first || old.adventure_civs != new.adventure_civs {
        ui.set_adventure_civs(strings(&new.adventure_civs));
    }
    if first || old.adventure_civ_picked != new.adventure_civ_picked {
        ui.set_adventure_civ_picked(new.adventure_civ_picked);
    }
    if first || old.adventure_sites != new.adventure_sites {
        ui.set_adventure_sites(strings(&new.adventure_sites));
    }
    if first || old.adventure_site_picked != new.adventure_site_picked {
        ui.set_adventure_site_picked(new.adventure_site_picked);
    }
    if first || old.adventure_region != new.adventure_region {
        ui.set_adventure_region(new.adventure_region.as_str().into());
    }
    if first || old.adventure_enabled != new.adventure_enabled {
        ui.set_adventure_enabled(new.adventure_enabled);
    }
    if first || old.legends_filters != new.legends_filters {
        let rows: Vec<LegendsFilterRow> = new
            .legends_filters
            .iter()
            .map(|e| LegendsFilterRow { text: e.text.as_str().into(), depth: e.depth, dimmed: e.dimmed })
            .collect();
        ui.set_legends_filters(ModelRc::new(VecModel::from(rows)));
    }
    if first || old.legends_filter_picked != new.legends_filter_picked {
        ui.set_legends_filter_picked(new.legends_filter_picked);
    }
    if first || old.legends_header != new.legends_header {
        ui.set_legends_header(new.legends_header.as_str().into());
    }
    if first || old.legends_lines != new.legends_lines {
        let model = ui.get_legends_lines();
        let appended = !old.legends_lines.is_empty() && new.legends_lines.starts_with(&old.legends_lines);
        let extended = appended
            && model.row_count() == old.legends_lines.len()
            && model.as_any().downcast_ref::<VecModel<SharedString>>().is_some_and(|vm| {
                vm.extend(new.legends_lines[old.legends_lines.len()..].iter().map(|s| SharedString::from(s.as_str())));
                true
            });
        if !extended {
            ui.set_legends_lines(strings(&new.legends_lines));
        }
    }
    if first || old.legends_more != new.legends_more {
        ui.set_legends_more(new.legends_more);
    }
    *shown = Some(new);
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::history::PersonId;

    fn site(id: u32, civ: u32, population: u32, abandoned: bool) -> SiteMarker {
        SiteMarker {
            site: SiteId(id),
            civ: CivId(civ),
            region: RegionCoords::new(id as i32, 0),
            name: format!("s{id}"),
            population,
            abandoned,
        }
    }

    fn civ(id: u32) -> CivMarker {
        CivMarker { civ: CivId(id), name: format!("c{id}") }
    }

    #[test]
    fn tab_index_maps_to_tab() {
        assert_eq!(embark_tab(0), EmbarkTab::Adventure);
        assert_eq!(embark_tab(1), EmbarkTab::Legends);
        assert_eq!(embark_tab(7), EmbarkTab::Adventure);
    }

    #[test]
    fn adventure_indices_follow_the_offered_lists() {
        let sites = [site(1, 2, 3, false), site(2, 1, 4, false), site(3, 1, 0, false), site(4, 1, 5, false)];
        let civs = [civ(2), civ(1), civ(3)];
        assert_eq!(adventure_civ_at(&sites, &civs, 0), Some(CivId(1)));
        assert_eq!(adventure_civ_at(&sites, &civs, 1), Some(CivId(2)));
        assert_eq!(adventure_civ_at(&sites, &civs, 2), None, "civ 3 has no populated site");
        assert_eq!(adventure_civ_at(&sites, &civs, -1), None);
        let mut sel = AdventureSelection::default();
        sel.pick_civ(CivId(1));
        let picked: Vec<_> = (0..3).map(|i| adventure_site_at(&sel, &sites, i).map(|s| s.site)).collect();
        assert_eq!(picked, [Some(SiteId(2)), Some(SiteId(4)), None]);
        assert_eq!(adventure_site_in(&sites, RegionCoords::new(3, 0)), None, "unpopulated");
        assert_eq!(adventure_site_in(&sites, RegionCoords::new(4, 0)).map(|s| s.site), Some(SiteId(4)));
    }

    #[test]
    fn adventure_texts() {
        let s = site(1, 1, 12, false);
        assert_eq!(adventure_site_text(&s), "s1 \u{2014} pop 12");
        assert_eq!(adventure_region_text(Some(&s)), "Start in region (1, 0)");
        assert_eq!(adventure_region_text(None), "Pick a site to start from");
    }

    #[test]
    fn legends_filters_list_civs_then_their_sites() {
        let sites = [site(1, 2, 3, false), site(2, 1, 0, true), site(3, 9, 1, false)];
        let entries = legends_filter_entries(&[civ(2), civ(1)], &sites);
        let rows: Vec<_> = entries.iter().map(|e| (e.text.as_str(), e.depth, e.dimmed)).collect();
        assert_eq!(
            rows,
            [("All events", 0, false), ("c1", 0, false), ("s2", 1, true), ("c2", 0, false), ("s1", 1, false)]
        );
        assert_eq!(legends_filter_index(&entries, &LegendsFilter::All), 0);
        assert_eq!(legends_filter_index(&entries, &LegendsFilter::Site(SiteId(1))), 4);
        assert_eq!(legends_filter_index(&entries, &LegendsFilter::Person(PersonId(1))), -1);
    }

    #[test]
    fn legends_header_names_the_filter() {
        let entries = legends_filter_entries(&[civ(1)], &[site(5, 1, 2, false)]);
        assert_eq!(legends_header(&entries, &LegendsFilter::All, 12), "All events \u{2014} 12 events");
        assert_eq!(legends_header(&entries, &LegendsFilter::Site(SiteId(5)), 1), "s5 \u{2014} 1 event");
        assert_eq!(legends_header(&entries, &LegendsFilter::Person(PersonId(3)), 0), "Person 3 \u{2014} 0 events");
    }

    #[test]
    fn legends_line_needs_exactly_one_site() {
        let line = |sites: Vec<SiteId>| LegendLine { index: 0, text: String::new(), sites, people: vec![] };
        assert_eq!(legends_line_site(&line(vec![])), None);
        assert_eq!(legends_line_site(&line(vec![SiteId(4)])), Some(SiteId(4)));
        assert_eq!(legends_line_site(&line(vec![SiteId(4), SiteId(5)])), None);
    }

    #[test]
    fn tabs_view_follows_the_history() {
        let mut h = HistoryView { sites: vec![site(1, 1, 3, false)], civs: vec![civ(1)], ..Default::default() };
        let v = tabs_view(&h);
        assert_eq!((v.adventure_civ_picked, v.adventure_site_picked, v.adventure_enabled), (-1, -1, false));
        h.adventure.pick_civ(CivId(1));
        h.adventure.pick_site(SiteId(1));
        let v = tabs_view(&h);
        assert_eq!((v.adventure_civ_picked, v.adventure_site_picked, v.adventure_enabled), (0, 0, true));
        h.adventure_pending = true;
        assert!(!tabs_view(&h).adventure_enabled, "one request at a time");
        assert_eq!(tabs_view(&h).legends_filter_picked, 0);
    }
}
