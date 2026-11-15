# Town Ranks, Civilisation Territory and Town Lands Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every town has a rank from its resident count, every civilisation owns one connected territory with wilderness around it, a town owns a set of regions that grows and shrinks with its rank, and each civilisation's largest town is its capital — all in the world history, shown on the world map and the Governance tab.

**Architecture:** Everything lives in the one global `History` on the coordinator (`crates/game/src/history/`), in new focused modules (`rank.rs`, `territory.rs`, `lands.rs`, `leaders.rs`). Rules are plain-data functions so the client checks tiles with the same code. The history file format is bumped once (3 → 4) for all new fields. The coordinator gains two requests (claim / release land) and broadcasts markers and territory; the client draws them.

**Tech Stack:** Rust (`game` crate, engine-agnostic: no Bevy), bincode + lz4 history files, Slint UI, Bevy client.

**Spec:** `docs/superpowers/specs/2026-11-14-town-ranks-multi-region-fortress-design.md` — read it first. This plan covers its order-of-work pieces 1, 3, 4, 5. The sim and client side (pieces 2, 6, 7, 8) is `docs/superpowers/plans/2026-11-14-town-lands-play.md`, which builds on this one.

## Global Constraints

- Work on `develop` in this checkout. **No git worktrees.**
- `game`, `server`, `worldgen` stay engine-agnostic: no Bevy, no windowing library.
- Ordered sim/history state is `BTreeMap` / `BTreeSet`, never `HashMap`. Randomness only through the history's stateless rolls (`History::roll` / `chance`). No inherent `f32` transcendentals.
- Every tunable is a named `pub const` in `crates/game/src/history/rules.rs`. Never write a region edge as a literal.
- The history file version is bumped **once**, in Task 4 (3 → 4). No later task changes the shape of `History`, `Site`, `Civ`, `Person`, `Journey` or `Location`. Version 2 and 3 files must still load.
- Wire types only derive serde; framing stays `game::encode_packet` / `decode_packet`.
- Tests: `just test-one game "--test it <module>::"` for the touched module. Never run a whole suite as a routine check; after Task 9 run the `game` non-client suite once (say so first). Tag a test slower than 100 ms `#[ignore = "slow: >100ms — run via just test-full"]`. `just smoke` is the default sanity check.
- Client tests (`just test-one client "--lib <filter>"`) are a slow opt-in: **ask Dawid before running them.**
- Any new or changed run dial updates the `justfile` and `CLAUDE.md` in the same change.
- New integration test files are modules of the one `it` binary: add `mod <name>;` to `crates/game/tests/it.rs`.
- Commit after each task, message style `feat(game): <what, as a sentence>`, ending with the `Co-Authored-By` line the session gives.

## Review Focus

1. **A world saved before this change is loaded** (version 2 or 3 history file): it must load, every site must have `lands == [region]`, a rank, and territory around it. → Task 4 tests.
2. **The same `ClaimLand` arrives twice** (a resend after a reconnect): one region claimed, one log entry, the same answer. → Task 9 test.
3. **A town on the map edge or the coast**: its reach is clipped, a claim off the map is refused, nothing panics on negative or out-of-range coordinates. → Task 3 and Task 7 tests.
4. **Two towns of different civilisations growing toward each other**: neither claims into the other's territory, the gap holds, the later claim is refused. → Task 7 test.
5. **A town shrinks while nobody holds it, or its last resident dies**: land is still released after the grace, Ruins keeps only the seat, a returning player finds the smaller town. → Task 7 tests.

---

### Task 1: `TownRank` and its table

**Files:**
- Create: `crates/game/src/history/rank.rs`
- Modify: `crates/game/src/history/mod.rs` (add `pub mod rank;` and `pub use rank::TownRank;` beside the existing `pub use`s)
- Create: `crates/game/tests/town_rank.rs`; Modify: `crates/game/tests/it.rs` (`mod town_rank;`)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Serialize, Deserialize)]
  pub enum TownRank { #[default] Ruins, Homestead, Hamlet, Village, Settlement, Haven, Town, Enclave,
      Stronghold, Barony, County, Fiefdom, Fortress, Citadel, City, Metropolis, Duchy, Principality,
      Crownlands, Kingsland }
  impl TownRank {
      pub const ALL: [TownRank; 20];
      pub fn of(residents: u32) -> TownRank;       // the highest rank whose residents_min <= residents
      pub fn name(self) -> &'static str;           // "Hamlet"
      pub fn title(self) -> &'static str;          // "Elder"
      pub fn residents_min(self) -> u32;
      pub fn lands_allowed(self) -> u32;
      pub fn next(self) -> Option<TownRank>;
  }
  ```

The table, one `const TABLE: [(TownRank, &str, &str, u32, u32); 20]` in `rank.rs` (rank, name, title, residents ≥, lands):

| Rank | Title | Residents ≥ | Lands | Rank | Title | Residents ≥ | Lands |
|---|---|---|---|---|---|---|---|
| Ruins | Keeper | 0 | 1 | County | Count | 170 | 8 |
| Homestead | Settler | 1 | 1 | Fiefdom | Lord | 220 | 9 |
| Hamlet | Elder | 10 | 1 | Fortress | Marshal | 280 | 10 |
| Village | Headman | 20 | 2 | Citadel | Protector | 350 | 12 |
| Settlement | Warden | 30 | 2 | City | Governor | 430 | 14 |
| Haven | Captain | 45 | 3 | Metropolis | Chancellor | 520 | 16 |
| Town | Mayor | 60 | 4 | Duchy | Duke | 620 | 18 |
| Enclave | Burgomaster | 80 | 5 | Principality | Prince | 720 | 21 |
| Stronghold | Castellan | 100 | 6 | Crownlands | Archon | 840 | 25 |
| Barony | Baron | 130 | 7 | Kingsland | King | 1000 | 25 |

- [ ] **Step 1: Write the failing tests** in `tests/town_rank.rs`

```rust
use game::history::TownRank;

#[test]
fn rank_of_residents() {
    for (n, r) in [(0, TownRank::Ruins), (1, TownRank::Homestead), (9, TownRank::Homestead),
        (10, TownRank::Hamlet), (20, TownRank::Village), (999, TownRank::Crownlands),
        (1000, TownRank::Kingsland), (u32::MAX, TownRank::Kingsland)] {
        assert_eq!(TownRank::of(n), r, "{n} residents");
    }
}

#[test]
fn names_and_titles() {
    assert_eq!((TownRank::Settlement.name(), TownRank::Settlement.title()), ("Settlement", "Warden"));
    assert_eq!((TownRank::Kingsland.name(), TownRank::Kingsland.title()), ("Kingsland", "King"));
    assert_eq!(TownRank::Ruins.title(), "Keeper");
}

#[test]
fn table_is_a_ladder_each_rank_reachable_on_the_land_before_it() {
    for w in TownRank::ALL.windows(2) {
        assert!(w[0].residents_min() < w[1].residents_min());
        assert!(w[0].lands_allowed() <= w[1].lands_allowed());
        assert!(w[1].residents_min() <= 40 * w[0].lands_allowed(), "{:?}", w[1]);
    }
    assert_eq!(TownRank::Kingsland.lands_allowed(), 25);
    assert_eq!(TownRank::Kingsland.next(), None);
    assert_eq!(TownRank::Hamlet.next(), Some(TownRank::Village));
}
```

- [ ] **Step 2: Run to see it fail** — `just test-one game "--test it town_rank::"` → fails to compile (`TownRank` not found).
- [ ] **Step 3: Implement `rank.rs`** from the table; `ALL` in table order.
- [ ] **Step 4: Run** — same command → 3 passed.
- [ ] **Step 5: Commit** — `feat(game): a town's rank is a function of its residents — the twenty ranks and their titles`

---

### Task 2: Region capacity comes down to about 40

**Files:**
- Modify: `crates/game/src/history/rules.rs:33-38`
- Modify: whichever tests in `crates/game/tests/history.rs` pin capacity numbers

**Interfaces:**
- Produces: `CAPACITY_BASE = 24`, `CAPACITY_PER_RIVER = 4`, `CAPACITY_LAKE = 8`, `CAPACITY_MAX = 40`, `CAPACITY_RELIEF_PENALTY_PER_100M = 6`, `CIV_POPULATION_CAP = 4000`. `BootstrapParams::default().founders_per_civ` goes 30 → 16 (a first site must start under its capacity).

- [ ] **Step 1: Change the constants** and the bootstrap default.
- [ ] **Step 2: Run the history module** — `just test-one game "--test it history::"`. Tests that assert the old numbers fail.
- [ ] **Step 3: Update pinned expectations** to the new constants (compute from the formula in `grid.rs:71-87`; do not loosen an assertion that is not about capacity). If `fast_forward_spreads_civs` (`tests/history.rs`, 3–8 living sites per civ) fails, run `just history` and put the printed per-civ site and population counts in the commit message; adjust only that test's bounds to what the run shows, and only if every civ is alive at year 100.
- [ ] **Step 4: Run** — `just test-one game "--test it history::"` → all pass.
- [ ] **Step 5: Commit** — `feat(game): a region holds about forty people — capacity retuned for the 1000-resident Kingsland`

---

### Task 3: `Territory` and a town's reach (pure)

**Files:**
- Create: `crates/game/src/history/territory.rs`
- Modify: `crates/game/src/history/mod.rs` (`pub mod territory; pub use territory::Territory;`), `crates/game/src/history/rules.rs` (`pub const TOWN_REACH_REGIONS: u32 = 6;` — it must stay above `EMBARK_MIN_DISTANCE_REGIONS`, or no town could ever be founded inside a border; add `const _: () = assert!(TOWN_REACH_REGIONS > EMBARK_MIN_DISTANCE_REGIONS);`), `crates/game/src/history/chain.rs` (make the existing `chebyshev` `pub(crate)`; delete the duplicate in `bootstrap.rs:46-48` and use the one)
- Create: `crates/game/tests/territory.rs`; Create: `crates/game/tests/common/towns.rs` (shared helpers); Modify: `tests/it.rs`, `tests/common/mod.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
  pub struct Territory { cells: BTreeMap<RegionCoords, CivId> }
  impl Territory {
      pub fn from_cells(cells: impl IntoIterator<Item = (RegionCoords, CivId)>) -> Territory;
      pub fn cells(&self) -> impl Iterator<Item = (RegionCoords, CivId)> + '_;
      pub fn len(&self) -> usize;
      pub fn civ_at(&self, rc: RegionCoords) -> Option<CivId>;
      /// Regions within TOWN_REACH_REGIONS (Chebyshev) of `seat`, on the grid, wilderness or
      /// `civ`'s, joined to `seat` by shared edges through such regions. Contains `seat` unless
      /// `seat` is off the grid or another civ's (then empty).
      pub fn reach(&self, grid: &SocietyGrid, civ: CivId, seat: RegionCoords) -> BTreeSet<RegionCoords>;
      /// Whether any of `cells` is `civ`'s or shares an edge with a region of `civ`'s.
      pub fn touches(&self, civ: CivId, cells: &BTreeSet<RegionCoords>) -> bool;
      /// Gives `civ` every wilderness region of `cells`; returns how many were added.
      pub fn grant(&mut self, civ: CivId, cells: &BTreeSet<RegionCoords>) -> usize;
      pub fn has_any(&self, civ: CivId) -> bool;
      /// Whether `civ`'s regions form one edge-connected area (true for none).
      pub fn is_connected(&self, civ: CivId) -> bool;
  }
  ```
- Produces (test helpers, `tests/common/towns.rs`): `flat_map(width: u32, height: u32) -> WorldMap` (all `Biome::Grassland`, `rivers: 4`, `relief_m: 0.0`, `landing: Some([8, 8])`, so every region has capacity `CAPACITY_MAX`; modelled on `test_map` in `tests/history.rs:26`); `history_on(map: &WorldMap) -> History` (`History::new(7, CalendarTick::from_days(0), SocietyGrid::from_world_map(map))`); `rc(x, z) -> RegionCoords`; `town(h: &mut History, civ: CivId, seat: RegionCoords, adults: u32) -> SiteId` (`add_founder` alternating sex, born 30 years before `h.now`, then `found_site(civ, seat, &founders, None, true)` — player-founded, so the AI never claims land for it); `ai_town(..)` (the same with `player_founded: false`); `set_rank(h, site, rank)` (writes `Site.rank` directly, for tests that need an allowance without crowding the town); `add_residents(h, site, n)` and `remove_residents(h, site, n)` (insert/remove directly in `h.people` and `h.sites[..].members`; new people `Person::newborn(.., Some(site), Location::AtSite(site))` born 30 years ago).

- [ ] **Step 1: Write the failing tests** in `tests/territory.rs`

```rust
use crate::common::towns::*;
use game::history::*;
use std::collections::BTreeSet;

fn grid20() -> SocietyGrid { SocietyGrid::from_world_map(&flat_map(20, 20)) }

#[test]
fn reach_is_the_square_around_the_seat() {
    let t = Territory::default();
    assert_eq!(t.reach(&grid20(), CivId(0), rc(10, 10)).len(), 169);          // 13 x 13
    assert!(t.reach(&grid20(), CivId(0), rc(10, 10)).contains(&rc(16, 4)));
    assert!(!t.reach(&grid20(), CivId(0), rc(10, 10)).contains(&rc(17, 10)));
}

#[test]
fn reach_is_clipped_at_the_map_edge() {
    let t = Territory::default();
    assert_eq!(t.reach(&grid20(), CivId(0), rc(0, 0)).len(), 49);             // x, z in 0..=6
    assert!(t.reach(&grid20(), CivId(0), rc(-5, 3)).is_empty());
}

#[test]
fn reach_stops_at_foreign_land_and_what_it_cuts_off() {
    // Civ 1 owns the column x = 12 across the square: everything east of it is cut off from the seat.
    let wall = (4..=16).map(|z| (rc(12, z), CivId(1)));
    let t = Territory::from_cells(wall);
    let reach = t.reach(&grid20(), CivId(0), rc(10, 10));
    assert_eq!(reach.len(), 104);                                              // x 4..=11, z 4..=16
    assert!(reach.iter().all(|r| r.x <= 11));
    assert!(t.reach(&grid20(), CivId(0), rc(12, 10)).is_empty());
}

#[test]
fn grant_takes_wilderness_only_and_touches_sees_edges() {
    let mut t = Territory::from_cells([(rc(5, 5), CivId(1))]);
    let cells: BTreeSet<_> = [rc(5, 5), rc(6, 5)].into();
    assert_eq!(t.grant(CivId(0), &cells), 1);
    assert_eq!(t.civ_at(rc(5, 5)), Some(CivId(1)));
    assert_eq!(t.civ_at(rc(6, 5)), Some(CivId(0)));
    assert!(t.touches(CivId(0), &[rc(7, 5)].into()));      // shares an edge with (6,5)
    assert!(!t.touches(CivId(0), &[rc(7, 6)].into()));     // corner only
    assert!(t.is_connected(CivId(0)));
    t.grant(CivId(0), &[rc(9, 9)].into());
    assert!(!t.is_connected(CivId(0)));
}
```

- [ ] **Step 2: Run to see it fail** — `just test-one game "--test it territory::"` → does not compile.
- [ ] **Step 3: Implement** `territory.rs`. `reach` is a breadth-first flood from `seat` over the four edge-neighbours, admitting a region when `grid.contains(rc)`, `chebyshev(seat, rc) <= TOWN_REACH_REGIONS` and `civ_at(rc)` is `None` or `Some(civ)`.
- [ ] **Step 4: Run** — 4 passed.
- [ ] **Step 5: Commit** — `feat(game): territory per region and a town's reach, as plain data`

---

### Task 4: History file version 4 — the new fields, and old worlds upgraded

**Files:**
- Modify: `crates/game/src/history/records.rs` (`Site`, `Civ`), `crates/game/src/history/mod.rs` (`History`, `HistoryV2`, new `HistoryV3`, `found_site`, `add_civ`), `crates/game/src/history/log.rs` (`LogKind` + `describe`), `crates/game/src/history/persist.rs`, `crates/game/src/history/play.rs` (`notice_regions` arms), `crates/game/src/history/stats.rs` and `legends.rs` (exhaustive matches on `LogKind`)
- Test: `crates/game/tests/territory.rs`

**Interfaces:**
- Consumes: `TownRank` (Task 1), `Territory` (Task 3).
- Produces — new fields, in this order at the END of each struct:
  - `Site`: `lands: Vec<RegionCoords>` (claim order; `lands[0] == region`, the seat), `rank: TownRank`, `ruler: Option<PersonId>`, `overextended_since: Option<CalendarTick>`
  - `Civ`: `capital: Option<SiteId>`
  - `History` (after `positions`, before the `#[serde(skip)]` fields): `pub territory: Territory`, `pub(crate) dwelling: BTreeMap<PersonId, RegionCoords>` (which land region an at-site person lives in; absent = the seat; filled by the play plan, empty here)
  - `LogKind` (appended, so old logs decode): `RankChanged { site: SiteId, from: TownRank, to: TownRank }`, `LandClaimed { site: SiteId, region: RegionCoords }`, `LandReleased { site: SiteId, region: RegionCoords }`, `RulerChanged { site: SiteId, from: Option<PersonId>, to: Option<PersonId> }`, `CapitalMoved { civ: CivId, from: Option<SiteId>, to: SiteId }`
  - `History::grant_reach(&mut self, site: SiteId)` (`pub(crate)`): `territory.grant(civ, &territory.reach(grid, civ, seat))`
  - `persist::HISTORY_FILE_VERSION = 4`; `#[doc(hidden)] pub fn encode_v3(h: &History) -> Vec<u8>`
- `describe` lines (exact copy): `"{site} grew into a {rank}"` when `to > from` (use "an" before Enclave), `"{site} declined to a {rank}"` when `to < from` and `to != Ruins`, `"{site} fell to Ruins"`; `"{site} claimed the land at ({x}, {z})"`; `"{site} gave up the land at ({x}, {z})"`; `"{person} became {title} of {site}"` (title = the site's current rank title; `to: None` → `"{site} has no ruler"`); `"{site} became the capital of {civ}"`.

- [ ] **Step 1: Write the failing tests** (append to `tests/territory.rs`)

```rust
#[test]
fn founding_a_site_sets_lands_rank_and_grants_its_reach() {
    let mut h = history_on(&flat_map(20, 20));
    let civ = h.add_civ(Race::Dwarf);
    let s = town(&mut h, civ, rc(10, 10), 7);
    let site = h.site(s).unwrap();
    assert_eq!(site.lands, vec![rc(10, 10)]);
    assert_eq!(site.rank, TownRank::Homestead);
    assert_eq!(h.territory.len(), 169);
    assert_eq!(h.territory.civ_at(rc(16, 16)), Some(civ));
    assert_eq!(h.territory.civ_at(rc(17, 10)), None);
}

#[test]
fn a_version_3_file_loads_with_lands_rank_and_territory() {
    let map = flat_map(20, 20);
    let mut h = history_on(&map);
    let civ = h.add_civ(Race::Dwarf);
    let s = town(&mut h, civ, rc(10, 10), 12);
    let loaded = persist::decode(&persist::encode_v3(&h), SocietyGrid::from_world_map(&map)).unwrap();
    let site = loaded.site(s).unwrap();
    assert_eq!(site.lands, vec![rc(10, 10)]);
    assert_eq!(site.rank, TownRank::Hamlet);
    assert_eq!(loaded.territory, h.territory);
    assert_eq!(loaded.civs[&civ].capital, None);
}

#[test]
fn a_version_4_file_round_trips() {
    let map = flat_map(20, 20);
    let mut h = history_on(&map);
    let civ = h.add_civ(Race::Dwarf);
    town(&mut h, civ, rc(10, 10), 7);
    let loaded = persist::decode(&persist::encode(&h), SocietyGrid::from_world_map(&map)).unwrap();
    assert_eq!(loaded.sites, h.sites);
    assert_eq!(loaded.territory, h.territory);
}
```

The existing `history_v2_file_loads_with_empty_positions` (`tests/history_play.rs:943`) must keep passing unchanged.

- [ ] **Step 2: Run to see it fail** — `just test-one game "--test it territory::"`.
- [ ] **Step 3: Add the fields and the log kinds.** `found_site` sets `lands: vec![region]`, `rank: TownRank::of(founders.len() as u32)`, `ruler: None`, `overextended_since: None`, and calls `grant_reach(id)` right after inserting the site. `add_civ` sets `capital: None`. `notice_regions`: the four site kinds map to their site; `CapitalMoved` maps to `to`.
- [ ] **Step 4: Persistence.** Keep bincode shapes of the old versions as private structs: `SiteV3` and `CivV3` (the structs as they are before this task), `HistoryV3` (today's serialized `History` fields with `sites: BTreeMap<SiteId, SiteV3>`, `civs: BTreeMap<CivId, CivV3>`), and change `HistoryV2`'s `sites` / `civs` to the V3 shapes. Both upgrade to `History` with `lands = vec![region]`, `rank = Ruins`, `capital = None`, empty `territory` and `dwelling`. In `persist::decode`, `found == 2` → `HistoryV2`, `found == 3` → `HistoryV3`, else `History`; after `set_grid`, when `found < 4` call a new `History::upgrade_to_v4(&mut self)`: for every site in `(founded, id)` order `grant_reach`, and set `rank = TownRank::of(living_members(id).len())`. `encode_v3` mirrors `encode_v2`.
- [ ] **Step 5: Check the coordinator row.** `CoordinatorState.history` (`crates/game/src/coordinator/state.rs:24-32`) must be `persist::encode` output decoded through `persist::decode`; if it is plain bincode of `History` instead, route it through `persist::encode` / `decode` and bump the state ladder at `state.rs:10-14` by one, with the old arm decoding the old bytes. State which it was in the commit message.
- [ ] **Step 6: Run** — `just test-one game "--test it territory::"`, `just test-one game "--test it history_play::history_v2_file_loads_with_empty_positions"`, `just test-one game "--test it history::"` → all pass. `just smoke`.
- [ ] **Step 7: Commit** — `feat(game): history file version 4 — lands, rank, ruler, capital and territory; older worlds are upgraded on load`

---

### Task 5: Where a town may be founded — borders and frontier

**Files:**
- Modify: `crates/game/src/history/chain.rs` (`EmbarkRefusal`, replace `embark_allowed_in` / `player_embark_allowed_in`, `embark_allowed`, `player_embark_allowed`, `embark_target`), `crates/game/src/history/journey.rs:284,289` (`arrive_embark`), `crates/game/src/history/play.rs:62` (`player_embark`), `crates/game/src/history/mod.rs` (re-exports)
- Modify (keep the build green): `crates/client/src/renderer/history_view.rs:283-285` (`embark_tile_check`), `crates/client/src/renderer/slint_overlay.rs` (refusal text match)
- Test: `crates/game/tests/territory.rs`; update `crates/game/tests/history_play.rs` (`embark_allowed_in_matches_history*`, `second_embark_too_close`, any other caller)

**Interfaces:**
- Consumes: `Territory::{reach, touches, civ_at, has_any}`.
- Produces:
  ```rust
  // EmbarkRefusal gains:  ForeignLand { civ: CivId },  TooFarFromBorders,
  #[derive(Clone, PartialEq, Eq, Debug)]
  pub struct TownView { pub site: SiteId, pub civ: CivId, pub lands: Vec<RegionCoords>, pub abandoned: bool, pub rank: TownRank }
  pub fn founding_allowed_in(grid: &SocietyGrid, towns: &[TownView], territory: &Territory,
      civ: CivId, seat: RegionCoords) -> Result<(), EmbarkRefusal>;
  pub fn player_founding_allowed_in(/* same */) -> Result<(), EmbarkRefusal>;   // + TooSteep
  impl History {
      pub fn town_views(&self) -> Vec<TownView>;
      pub fn embark_allowed(&self, civ: CivId, region: RegionCoords) -> Result<(), EmbarkRefusal>;
      pub fn player_embark_allowed(&self, civ: CivId, region: RegionCoords) -> Result<(), EmbarkRefusal>;
      pub fn embark_target(&self, civ: CivId, from_region: RegionCoords) -> Option<RegionCoords>;  // `civ` now used
  }
  ```
  `embark_allowed_in` and `player_embark_allowed_in` are removed.
- Check order in `founding_allowed_in`: `OffMap`; `NotHabitable` (capacity 0); `Occupied` (the seat is in any town's `lands`, abandoned or not); `TooClose { site }` (a living town has a land at Chebyshev distance `< EMBARK_MIN_DISTANCE_REGIONS`); `ForeignLand { civ: owner }` (`civ_at(seat)` is another civ); `TooFarFromBorders` (`civ_at(seat)` is `None`, `territory.has_any(civ)`, and `!territory.touches(civ, &territory.reach(grid, civ, seat))`). A civ with no territory yet may found anywhere.
- Client, this task only: `embark_tile_check` takes the chosen `CivId`, builds `TownView`s from the site markers (`lands: vec![m.region]`, `rank: TownRank::Ruins`) and passes `&Territory::default()`; Task 11 gives it the real data. Refusal lines (exact copy): `ForeignLand` → `"That is {civ name}'s land — choose {civ name} to settle there"`, `TooFarFromBorders` → `"Too far from {chosen civ name}'s borders"`.

- [ ] **Step 1: Write the failing tests** (append to `tests/territory.rs`)

```rust
fn two_civs() -> (History, CivId, CivId) {
    let mut h = history_on(&flat_map(40, 20));
    let (a, b) = (h.add_civ(Race::Dwarf), h.add_civ(Race::Dwarf));
    town(&mut h, a, rc(5, 10), 7);     // territory x 0..=11, z 4..=16
    town(&mut h, b, rc(30, 10), 7);    // territory x 24..=36, z 4..=16
    (h, a, b)
}

#[test]
fn founding_follows_the_borders() {
    let (h, a, b) = two_civs();
    assert_eq!(h.embark_allowed(a, rc(9, 10)), Ok(()));                   // inside own borders, 4 from the town
    assert_eq!(h.embark_allowed(a, rc(15, 10)), Ok(()));                  // wilderness; its reach (x 9..=21) overlaps a's land
    assert_eq!(h.embark_allowed(a, rc(19, 10)), Err(EmbarkRefusal::TooFarFromBorders));   // reach starts at x = 13
    assert_eq!(h.embark_allowed(a, rc(26, 10)), Err(EmbarkRefusal::ForeignLand { civ: b }));
    assert_eq!(h.embark_allowed(b, rc(26, 10)), Ok(()));                  // inside b's borders: b's town
    assert_eq!(h.embark_allowed(a, rc(5, 10)), Err(EmbarkRefusal::Occupied));
    assert!(matches!(h.embark_allowed(a, rc(7, 10)), Err(EmbarkRefusal::TooClose { .. })));
}

#[test]
fn ai_parties_settle_on_their_own_frontier() {
    let (h, a, _) = two_civs();
    let target = h.embark_target(a, rc(5, 10)).unwrap();
    assert!(h.embark_allowed(a, target).is_ok());
    assert!(target.x < 24, "{target:?} is in the other civilisation's land");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn every_civ_is_one_piece_after_a_bootstrap() {
    // Same map and params as `fast_forward_spreads_civs` in tests/history.rs.
    let h = /* bootstrap as that test does */;
    for civ in h.civs.keys() {
        assert!(h.territory.is_connected(*civ), "{civ:?} is in pieces");
    }
}
```

- [ ] **Step 2: Run to see it fail** — `just test-one game "--test it territory::"`.
- [ ] **Step 3: Implement** the rule and thread `civ` through every caller (`embark_target` filters candidates with `self.embark_allowed(civ, rc)`; `arrive_embark` passes its `civ`; `player_embark` passes `req.civ`). Update the client call site and the two refusal lines.
- [ ] **Step 4: Update the existing embark tests** in `tests/history_play.rs` to the new signatures; `embark_allowed_in_matches_history*` become `founding_allowed_in` against `h.town_views()` and `&h.territory`.
- [ ] **Step 5: Run** — `just test-one game "--test it territory::"`, `just test-one game "--test it history_play::"`, `just test-one game "--test it history::"`, the ignored one once with `-- --ignored every_civ_is_one_piece`, then `just smoke`.
- [ ] **Step 6: Commit** — `feat(game): a town is founded inside its civilisation's borders or on its frontier — wilderness beyond is closed`

---

### Task 6: Rank is reviewed each month

**Files:**
- Modify: `crates/game/src/history/chain.rs` (`on_site_month`, `abandon_site`), `crates/game/src/history/rank.rs` (`impl History`)
- Test: `crates/game/tests/town_rank.rs`

**Interfaces:**
- Produces: `pub(crate) fn History::review_rank(&mut self, sid: SiteId)` — `new = TownRank::of(living_members(sid).len())`; when it differs from `site.rank`, store it and `log_event(LogKind::RankChanged { site, from, to })`. Called first thing in `on_site_month`, and from `abandon_site` after `abandoned` is set.

- [ ] **Step 1: Write the failing tests**

```rust
use crate::common::towns::*;
use game::history::*;

fn rank_changes(h: &History) -> Vec<(TownRank, TownRank)> {
    h.log.iter().filter_map(|e| match e.kind {
        LogKind::RankChanged { from, to, .. } => Some((from, to)),
        _ => None,
    }).collect()
}

#[test]
fn rank_follows_residents_at_the_month() {
    let mut h = history_on(&flat_map(20, 20));
    let civ = h.add_civ(Race::Dwarf);
    let s = town(&mut h, civ, rc(10, 10), 7);
    add_residents(&mut h, s, 13);
    assert_eq!(h.site(s).unwrap().rank, TownRank::Homestead, "not before the month turns");
    h.advance(h.now.plus_days(DAYS_PER_MONTH));
    assert_eq!(h.site(s).unwrap().rank, TownRank::Village);
    assert_eq!(rank_changes(&h), vec![(TownRank::Homestead, TownRank::Village)]);
    remove_residents(&mut h, s, 15);
    h.advance(h.now.plus_days(DAYS_PER_MONTH));
    assert_eq!(h.site(s).unwrap().rank, TownRank::Homestead);
    assert_eq!(h.log.last().unwrap().describe(&h), format!("{}: Site {} declined to a Homestead", h.now.date(), s.0));
}
```

(Adjust the expected line's date prefix to what `LogEntry::describe` really prints — see the existing describe tests in `tests/history.rs`.)

- [ ] **Step 2: Run to see it fail** — `just test-one game "--test it town_rank::"`.
- [ ] **Step 3: Implement `review_rank`** and its two call sites.
- [ ] **Step 4: Run** — passes; `just test-one game "--test it history::"` still passes.
- [ ] **Step 5: Commit** — `feat(game): a town's rank is reviewed each month and its changes are history`

---

### Task 7: Town lands — claim, release, shrink, AI claims

**Files:**
- Create: `crates/game/src/history/lands.rs`
- Modify: `crates/game/src/history/mod.rs` (`pub mod lands; pub use lands::{LandRefusal, claim_allowed_in};`), `crates/game/src/history/rules.rs`, `crates/game/src/history/chain.rs` (`on_site_month`, `abandon_site`, `consider_leaving`)
- Create: `crates/game/tests/town_lands.rs`; Modify: `tests/it.rs`

**Interfaces:**
- Consumes: `TownView`, `Territory`, `TownRank::lands_allowed`, `chebyshev`.
- Produces:
  ```rust
  // rules.rs
  pub const TOWN_GAP_REGIONS: u32 = 2;                         // regions that must lie between two towns' lands
  pub const LAND_RELEASE_GRACE_DAYS: u64 = 3 * DAYS_PER_MONTH; // a season

  #[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
  pub enum LandRefusal { NoFortress, UnknownSite, NoAllowance, OutOfReach, Taken, NotAdjacent,
      TooCloseToTown { site: SiteId }, NotYours, Seat, WouldSplit }

  /// The claim rule over plain data (the client marks tiles with it).
  pub fn claim_allowed_in(grid: &SocietyGrid, towns: &[TownView], territory: &Territory,
      site: SiteId, rc: RegionCoords) -> Result<(), LandRefusal>;
  /// Whether `lands` minus `rc` is still one edge-connected set.
  pub fn lands_connected_without(lands: &[RegionCoords], rc: RegionCoords) -> bool;

  impl History {
      pub fn claim_land(&mut self, site: SiteId, rc: RegionCoords) -> Result<(), LandRefusal>;
      pub fn release_land(&mut self, site: SiteId, rc: RegionCoords) -> Result<(), LandRefusal>;
      pub(crate) fn review_lands(&mut self, sid: SiteId);
      pub(crate) fn claim_best_land(&mut self, sid: SiteId) -> bool;
  }
  ```
- `claim_allowed_in` check order: the town exists and is not abandoned (`UnknownSite`); `lands.len() < rank.lands_allowed()` (`NoAllowance`); `grid.contains(rc)`, `chebyshev(seat, rc) <= TOWN_REACH_REGIONS` and `territory.civ_at(rc) == Some(civ)` (`OutOfReach`); `rc` in no town's lands (`Taken`); `rc` shares an edge with one of the town's lands (`NotAdjacent`); no other living town has a land at Chebyshev distance `<= TOWN_GAP_REGIONS` from `rc` (`TooCloseToTown { site }`).
- `claim_land`: the rule, then push `rc` onto `lands`, `capacity = sum of grid.capacity over lands`, log `LandClaimed`.
- `release_land`: `rc` not in lands → `NotYours`; `rc == lands[0]` → `Seat`; `!lands_connected_without` → `WouldSplit`; else remove it, recompute capacity, drop every `dwelling` entry equal to `rc` whose person is a member of this site, clear `overextended_since` when the town now fits, log `LandReleased`.
- `review_lands` (called in `on_site_month` right after `review_rank`): town fits → `overextended_since = None`. Otherwise: `None` → set to `now`; set and `now` is at least `LAND_RELEASE_GRACE_DAYS` later → release the last entry of `lands` for which `release_land` succeeds (one per month).
- `abandon_site`: after `review_rank`, release every land but the seat (newest first; no grace).
- `claim_best_land`: among the regions `claim_allowed_in` accepts within the reach square, the highest `grid.capacity`, ties by `(z, x)`; claim it. Called in `consider_leaving` once the site is crowded, for sites with `!player_founded`: `if self.claim_best_land(sid) { return; }` before any household or party is sent.

- [ ] **Step 1: Write the failing tests** in `tests/town_lands.rs`

```rust
use crate::common::towns::*;
use game::history::*;
use game::history::rules::*;

fn village() -> (History, CivId, SiteId) {
    let mut h = history_on(&flat_map(30, 30));
    let civ = h.add_civ(Race::Dwarf);
    let s = town(&mut h, civ, rc(10, 10), 7);
    add_residents(&mut h, s, 13);                       // 20: a Village, 2 lands
    h.advance(h.now.plus_days(DAYS_PER_MONTH));
    (h, civ, s)
}
fn month(h: &mut History) { h.advance(h.now.plus_days(DAYS_PER_MONTH)); }

#[test]
fn a_village_claims_one_neighbour_and_its_capacity_grows() {
    let (mut h, _, s) = village();
    let one = h.site(s).unwrap().capacity;
    assert_eq!(h.claim_land(s, rc(11, 10)), Ok(()));
    assert_eq!(h.site(s).unwrap().lands, vec![rc(10, 10), rc(11, 10)]);
    assert_eq!(h.site(s).unwrap().capacity, 2 * one);
    assert_eq!(h.claim_land(s, rc(12, 10)), Err(LandRefusal::NoAllowance));
    assert!(matches!(h.log.last().unwrap().kind, LogKind::LandClaimed { .. }));
}

#[test]
fn claims_must_touch_the_town_stay_in_reach_and_on_the_map() {
    let (mut h, _, s) = village();
    assert_eq!(h.claim_land(s, rc(12, 10)), Err(LandRefusal::NotAdjacent));
    assert_eq!(h.claim_land(s, rc(11, 11)), Err(LandRefusal::NotAdjacent));   // corner only
    assert_eq!(h.claim_land(s, rc(17, 10)), Err(LandRefusal::OutOfReach));     // 7 from the seat
    assert_eq!(h.claim_land(s, rc(-1, 10)), Err(LandRefusal::OutOfReach));
    assert_eq!(h.claim_land(s, rc(10, 10)), Err(LandRefusal::Taken));
    assert_eq!(h.claim_land(SiteId(99), rc(11, 10)), Err(LandRefusal::UnknownSite));
}

#[test]
fn a_homestead_has_no_allowance() {
    let mut h = history_on(&flat_map(30, 30));
    let civ = h.add_civ(Race::Dwarf);
    let s = town(&mut h, civ, rc(10, 10), 7);
    assert_eq!(h.claim_land(s, rc(11, 10)), Err(LandRefusal::NoAllowance));
}

#[test]
fn towns_keep_a_gap() {
    let (mut h, civ, s) = village();
    let near = town(&mut h, civ, rc(14, 10), 7);        // same civ, founded inside the borders, seats 4 apart
    // (11,10) is 3 from (14,10): allowed. A Haven's third land at (12,10) would be 2 away: refused.
    assert_eq!(h.claim_land(s, rc(11, 10)), Ok(()));
    set_rank(&mut h, s, TownRank::Haven);               // 3 lands
    assert_eq!(h.claim_land(s, rc(12, 10)), Err(LandRefusal::TooCloseToTown { site: near }));
    assert_eq!(h.claim_land(s, rc(10, 11)), Ok(()));
}

#[test]
fn foreign_territory_is_out_of_reach() {
    let mut h = history_on(&flat_map(30, 30));
    let (a, b) = (h.add_civ(Race::Dwarf), h.add_civ(Race::Dwarf));
    h.territory.grant(b, &[rc(11, 10)].into());         // b holds the region east of a's future seat
    let s = town(&mut h, a, rc(10, 10), 7);
    add_residents(&mut h, s, 13);
    month(&mut h);
    assert_eq!(h.claim_land(s, rc(11, 10)), Err(LandRefusal::OutOfReach));
}

#[test]
fn a_shrunken_town_loses_its_newest_land_after_a_season() {
    let (mut h, _, s) = village();
    h.claim_land(s, rc(11, 10)).unwrap();
    remove_residents(&mut h, s, 13);                    // 7: a Homestead again, nobody holding it
    month(&mut h);
    assert!(h.site(s).unwrap().overextended_since.is_some());
    assert_eq!(h.site(s).unwrap().lands.len(), 2, "grace");
    month(&mut h); month(&mut h);
    assert_eq!(h.site(s).unwrap().lands.len(), 2, "still inside the season");
    month(&mut h);
    assert_eq!(h.site(s).unwrap().lands, vec![rc(10, 10)]);
    assert_eq!(h.site(s).unwrap().overextended_since, None);
    assert!(h.log.iter().any(|e| matches!(e.kind, LogKind::LandReleased { region, .. } if region == rc(11, 10))));
}

#[test]
fn growing_back_inside_the_grace_keeps_the_land() {
    let (mut h, _, s) = village();
    h.claim_land(s, rc(11, 10)).unwrap();
    remove_residents(&mut h, s, 13);
    month(&mut h);
    add_residents(&mut h, s, 13);
    for _ in 0..5 { month(&mut h); }
    assert_eq!(h.site(s).unwrap().lands.len(), 2);
    assert_eq!(h.site(s).unwrap().overextended_since, None);
}

#[test]
fn ruins_keep_only_the_seat() {
    let (mut h, _, s) = village();
    h.claim_land(s, rc(11, 10)).unwrap();
    remove_residents(&mut h, s, 20);
    h.abandon_site_for_test(s);                         // #[doc(hidden)] pub wrapper of abandon_site
    assert_eq!(h.site(s).unwrap().lands, vec![rc(10, 10)]);
    assert_eq!(h.site(s).unwrap().rank, TownRank::Ruins);
}

#[test]
fn the_holder_may_give_land_up_but_not_the_seat_nor_split_the_town() {
    let (mut h, _, s) = village();
    set_rank(&mut h, s, TownRank::Haven);               // 3 lands
    h.claim_land(s, rc(11, 10)).unwrap();
    h.claim_land(s, rc(12, 10)).unwrap();
    assert_eq!(h.release_land(s, rc(10, 10)), Err(LandRefusal::Seat));
    assert_eq!(h.release_land(s, rc(11, 10)), Err(LandRefusal::WouldSplit));
    assert_eq!(h.release_land(s, rc(20, 20)), Err(LandRefusal::NotYours));
    assert_eq!(h.release_land(s, rc(12, 10)), Ok(()));
}

#[test]
fn a_crowded_ai_town_claims_land_before_it_sends_anyone_away() {
    let mut h = history_on(&flat_map(30, 30));
    let civ = h.add_civ(Race::Dwarf);
    let s = ai_town(&mut h, civ, rc(10, 10), 7);
    add_residents(&mut h, s, 33);                       // 40 on a capacity of 40: crowded, and a Settlement (2 lands)
    month(&mut h);
    assert_eq!(h.site(s).unwrap().lands.len(), 2);
    assert!(!h.log.iter().any(|e| matches!(e.kind, LogKind::Departed { .. })));
}
```

- [ ] **Step 2: Run to see it fail** — `just test-one game "--test it town_lands::"`.
- [ ] **Step 3: Implement `lands.rs`**, the constants, and the three call sites in `chain.rs`. Add `#[doc(hidden)] pub fn abandon_site_for_test`.
- [ ] **Step 4: Run** — `just test-one game "--test it town_lands::"` → 10 passed; `just test-one game "--test it history::"` and `territory::` still pass (re-check `every_civ_is_one_piece` with `-- --ignored`: AI towns now claim land).
- [ ] **Step 5: Commit** — `feat(game): a town claims neighbouring regions up to its rank's allowance and gives them up when it shrinks`

---

### Task 8: Rulers and the capital

**Files:**
- Create: `crates/game/src/history/leaders.rs`
- Modify: `crates/game/src/history/mod.rs` (`mod leaders;`, call in `found_site`), `crates/game/src/history/chain.rs` (`on_site_month`, `abandon_site`), `crates/game/src/history/rules.rs`
- Create: `crates/game/tests/capital.rs`; Modify: `tests/it.rs`

**Interfaces:**
- Produces:
  ```rust
  pub const CAPITAL_MARGIN_PER_MILLE: u32 = 100;   // rules.rs
  impl History {
      pub(crate) fn review_ruler(&mut self, sid: SiteId);
      pub(crate) fn review_capital(&mut self, civ: CivId);
      /// The civ's capital and that town's ruler.
      pub fn leader_of(&self, civ: CivId) -> Option<(SiteId, Option<PersonId>)>;
  }
  ```
- `review_ruler`: the ruler stands while alive and a member. Otherwise the successor is the eldest living member aged `>= ADULT_AGE_YEARS` among the old ruler's `children`, else the eldest such member of the site (ties: lower id), else `None`. A change logs `RulerChanged`. Called at the end of `found_site`, in `on_site_month` (after `review_lands`), and in `abandon_site` (ruler → `None`).
- `review_capital`: candidates are the civ's sites with `abandoned.is_none()`, sized by `living_members`. No candidates → leave `capital` as it is. The capital stands unless it is not a candidate or a candidate has `residents * 1000 > capital_residents * (1000 + CAPITAL_MARGIN_PER_MILLE)`; the new capital is the largest candidate (ties: earlier `founded`, then lower id). A change logs `CapitalMoved`. Called at the end of `found_site`, in `on_site_month` (last), and in `abandon_site`.

- [ ] **Step 1: Write the failing tests** in `tests/capital.rs`

```rust
use crate::common::towns::*;
use game::history::*;

fn month(h: &mut History) { h.advance(h.now.plus_days(DAYS_PER_MONTH)); }
fn capital_moves(h: &History) -> usize {
    h.log.iter().filter(|e| matches!(e.kind, LogKind::CapitalMoved { .. })).count()
}

#[test]
fn the_first_town_is_the_capital_and_has_a_ruler() {
    let mut h = history_on(&flat_map(30, 30));
    let civ = h.add_civ(Race::Dwarf);
    let s = town(&mut h, civ, rc(10, 10), 10);
    let ruler = h.site(s).unwrap().ruler.expect("an adult founder rules");
    assert!(h.site(s).unwrap().members.contains(&ruler));
    assert_eq!(h.leader_of(civ), Some((s, Some(ruler))));
    assert_eq!(capital_moves(&h), 1);
}

#[test]
fn the_capital_moves_only_past_the_margin() {
    let mut h = history_on(&flat_map(30, 30));
    let civ = h.add_civ(Race::Dwarf);
    let a = town(&mut h, civ, rc(10, 10), 10);
    let b = town(&mut h, civ, rc(14, 10), 10);
    add_residents(&mut h, b, 1);                 // 11 vs 10: 11000 is not more than 11000
    month(&mut h);
    assert_eq!(h.civs[&civ].capital, Some(a));
    add_residents(&mut h, b, 1);                 // 12 vs 10
    month(&mut h);
    assert_eq!(h.civs[&civ].capital, Some(b));
    assert_eq!(capital_moves(&h), 2);
}

#[test]
fn a_ruined_capital_hands_over_and_a_dead_ruler_is_succeeded() {
    let mut h = history_on(&flat_map(30, 30));
    let civ = h.add_civ(Race::Dwarf);
    let a = town(&mut h, civ, rc(10, 10), 10);
    let b = town(&mut h, civ, rc(14, 10), 8);
    let old = h.site(b).unwrap().ruler.unwrap();
    h.people.get_mut(&old).unwrap().died = Some((h.now, DeathCause::OldAge));
    month(&mut h);
    let new = h.site(b).unwrap().ruler.unwrap();
    assert_ne!(new, old);
    assert!(h.people[&new].alive());
    remove_residents(&mut h, a, 10);
    h.abandon_site_for_test(a);
    assert_eq!(h.civs[&civ].capital, Some(b));
    assert_eq!(h.site(a).unwrap().ruler, None);
}
```

- [ ] **Step 2: Run to see it fail** — `just test-one game "--test it capital::"`.
- [ ] **Step 3: Implement `leaders.rs`** and the call sites.
- [ ] **Step 4: Run** — 3 passed; `history::`, `territory::`, `town_lands::` still pass.
- [ ] **Step 5: Commit** — `feat(game): every town has a ruler and each civilisation's largest town is its capital`

---

### Task 9: Protocol, coordinator and host — markers, territory, claim and release

**Files:**
- Modify: `crates/game/src/protocol.rs` (`SiteMarker`, `CivMarker`, `ClientPacket`, `ServerPacket`), `crates/game/src/history/play.rs` (`site_markers`, `civ_markers`, new `territory_cells`), `crates/game/src/coordinator/msg.rs` (`WorldRequest`, `WorldReply`, `CoordMsg`), `crates/game/src/coordinator/mod.rs` (`sites_key`, `broadcast_daily`, `push_view`, request dispatch, new `claim_land` / `release_land`), `crates/game/src/world_manager/mod.rs` (packet dispatch near l.1874, new field `site_lands`), `crates/game/src/world_manager/world_link.rs` (l.598-630 marker intake, `world_reply`, connect-time sends), `justfile`, `CLAUDE.md` ("Build/run dials")
- Test: `crates/game/tests/history_play.rs` (the `World` rig), `crates/game/tests/coordinator_requests.rs`

**Interfaces:**
- Consumes: `History::{claim_land, release_land, leader_of}`, `LandRefusal`, `TownRank`, `Territory::cells`.
- Produces:
  - `SiteMarker` gains `rank: TownRank`, `lands: Vec<RegionCoords>`, `ruler: Option<String>` (the ruler's name). `CivMarker` gains `capital: Option<SiteId>`.
  - `ClientPacket::ClaimLand(RegionCoords)`, `ClientPacket::ReleaseLand(RegionCoords)`.
  - `ServerPacket::LandResult(Result<RegionCoords, LandRefusal>)`, `ServerPacket::Territory(Vec<(RegionCoords, CivId)>)` (the whole territory, replace on receipt: on connect next to `Sites`, and when it grows).
  - `WorldRequest::ClaimLand(RegionCoords)`, `WorldRequest::ReleaseLand(RegionCoords)` — both in `changes_state()`. `WorldReply::LandChanged { site: SiteId, region: RegionCoords }`, `WorldReply::LandRefused(LandRefusal)` — neither in `grants_claim()`.
  - `CoordMsg::View` gains `territory: Vec<(RegionCoords, CivId)>`; new `CoordMsg::Territory(Vec<(RegionCoords, CivId)>)` and `CoordMsg::Civs(Vec<CivMarker>)`.
  - `History::territory_cells(&self) -> Vec<(RegionCoords, CivId)>`.
  - Host: `site_lands: BTreeMap<SiteId, BTreeSet<RegionCoords>>` on `WorldManager`, filled by one helper `note_sites(&mut self, sites: &[SiteMarker])` that replaces the three `site_regions.extend(...)` lines; `pub(super) fn lands_of(&self, site: SiteId) -> Option<&BTreeSet<RegionCoords>>` (the play plan consumes it).
  - Dev dial: env `VOXEL_DEV_TOWN_RANK=<rank name>` read by the coordinator at start into `History::dev_min_rank: Option<TownRank>` (`#[serde(skip)]`, set by `pub fn set_dev_min_rank`); `review_rank` uses `max(of(residents), dev_min_rank)`. Justfile variable `town_rank := ""` passed as that env by `run`, `run-two`, `server`. One line under CLAUDE.md "Build/run dials".
- Coordinator behaviour:
  - `claim_land(player, rc)`: `player_claims[player]` must be `Claim::Fortress(site)`, else `LandRefused(NoFortress)`; then `history.claim_land(site, rc)`. `release_land` likewise. On success push `CoordMsg::Sites` to `Target::Every` at once and store the new `sites_key`.
  - `sites_key` tuple gains `rank` and `lands.len()`.
  - `broadcast_daily` also pushes `CoordMsg::Territory` when `history.territory.len()` changed, and `CoordMsg::Civs` when `civ_markers()` changed (keep the last of each beside `sites_key`).
- Host behaviour: `ClaimLand` / `ReleaseLand` from a client with no entry in `fortress_of`, or with no history, answer `LandResult(Err(NoFortress))` at once; otherwise `world_request`. `LandChanged` → `LandResult(Ok(region))`; `LandRefused(e)` → `LandResult(Err(e))`. `CoordMsg::Territory` / `Civs` are forwarded to every session and kept for connect-time sends, as `Sites` is.

- [ ] **Step 1: Write the failing tests** (in `tests/history_play.rs`, using its rig and helpers `embark_world`, `sites_in`)

```rust
#[test]
fn connect_sends_territory_and_markers_carry_rank_and_lands() {
    let (mut w, site, civ, _, free) = embark_world();
    // … connect a client as the neighbouring tests do, collect its packets …
    let territory = packets.iter().find_map(|p| match p {
        ServerPacket::Territory(t) => Some(t.clone()), _ => None }).expect("a Territory packet");
    let home = sites_in(&packets).unwrap().into_iter().find(|m| m.site == site).unwrap();
    assert!(territory.contains(&(home.region, civ)));
    assert_eq!(home.lands, vec![home.region]);
    assert_eq!(home.rank, TownRank::of(home.population));
    assert!(home.ruler.is_some());
}

#[test]
fn claiming_land_needs_a_fortress_and_an_allowance() {
    // A client with no fortress:
    //   ClientPacket::ClaimLand(r)  ->  ServerPacket::LandResult(Err(LandRefusal::NoFortress))
    // After an embark with 7 people (a Homestead), claiming the region east of the fortress:
    //   ->  ServerPacket::LandResult(Err(LandRefusal::NoAllowance))
}

#[test]
fn a_dev_rank_lets_a_small_fortress_claim_and_the_markers_follow() {
    // Coordinator built with set_dev_min_rank(Some(TownRank::Village)); embark; advance one month.
    // ClaimLand(east) -> LandResult(Ok(east)); the next Sites packet has lands == [seat, east].
    // ReleaseLand(east) -> LandResult(Ok(east)); lands == [seat].
}
```

and in `tests/coordinator_requests.rs`, beside the existing memo tests:

```rust
#[test]
fn a_repeated_claim_request_is_answered_from_memory() {
    // Same RequestId sent twice with WorldRequest::ClaimLand(east):
    // both replies are WorldReply::LandChanged { .. } and the history log holds exactly one LandClaimed.
}
```

Write the three sketched tests out in full in the style of their neighbours (`fortress_packet_follows_embark_retire_and_reconnect`, the memo test in `coordinator_requests.rs`); the assertions above are the required ones.

- [ ] **Step 2: Run to see them fail** — `just test-one game "--test it history_play::"`, `just test-one game "--test it coordinator_requests::"`.
- [ ] **Step 3: Implement** protocol, markers, coordinator, host, the dev dial and its justfile / CLAUDE.md lines. Fix every construction site of `SiteMarker` / `CivMarker` / `CoordMsg::View` the compiler reports (the client builds none; server tests may).
- [ ] **Step 4: Run** — the two modules pass; `just smoke`. Then, saying so first, the whole non-client suites once: `just test-one game "--test it"` and `just test-server` (the split-role tests carry `CoordMsg` over the real link).
- [ ] **Step 5: Commit** — `feat(game): the coordinator answers land claims and broadcasts ranks, lands, capitals and territory`

---

### Task 10: The Governance tab shows the rank and the title

**Files:**
- Modify: `crates/client/src/renderer/fortress_panel.rs:17-29` (`governance_lines`), its caller in `crates/client/src/renderer/slint_overlay.rs:2170-2176`, the status line for `ServerPacket::LandResult`
- Test: the `#[cfg(test)]` module of `fortress_panel.rs`

**Interfaces:**
- Produces: `pub fn governance_lines(calendar: Option<CalendarTick>, fortress: Option<&SiteMarker>, capital_of: Option<&str>) -> Vec<String>` — `capital_of` is the civilisation's name when this fortress is its capital. Lines, exact copy:
  1. `"Date {date}"` (when known)
  2. `"{Rank} of {site name}"`
  3. `"You are its {Title}"`, or `"You are its {Title}, leader of {civ}"` when `capital_of` is set
  4. `"Residents {n} — {next rank} at {min}"`, or `"Residents {n}"` at Kingsland
  5. `"Lands {held} / {allowed}"`
  With no fortress: `"No fortress"` (as today).
- `LandRefusal` lines for the status line (exact copy): `NoFortress` "You hold no fortress", `NoAllowance` "Your town's rank allows no more land", `OutOfReach` "Too far from your seat", `Taken` "That land is taken", `NotAdjacent` "Land must border your town", `TooCloseToTown` "Too close to {site name}", `NotYours` "That is not your land", `Seat` "A town cannot give up its seat", `WouldSplit` "That would cut your town in two", `UnknownSite` "No such town".

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn governance_shows_rank_title_and_lands() {
    let m = SiteMarker { site: SiteId(5), civ: CivId(1), region: RegionCoords::new(3, 4), name: "Site 5".into(),
        population: 12, abandoned: false, returnable: true, rank: TownRank::Hamlet,
        lands: vec![RegionCoords::new(3, 4)], ruler: Some("Dwarf 9".into()) };
    assert_eq!(governance_lines(None, Some(&m), None),
        ["Hamlet of Site 5", "You are its Elder", "Residents 12 — Village at 20", "Lands 1 / 1"]);
    assert_eq!(governance_lines(None, Some(&m), Some("Civ 1"))[1], "You are its Elder, leader of Civ 1");
    assert_eq!(governance_lines(None, None, None), ["No fortress"]);
}
```

- [ ] **Step 2: Implement**, pass `capital_of` from the civ markers at the call site, show `LandResult` errors and `"Claimed ({x}, {z})"` / `"Gave up ({x}, {z})"` in the `game-status` line.
- [ ] **Step 3: Verify** — `just smoke`. Ask Dawid before running `just test-one client "--lib fortress_panel"`; run it on a yes.
- [ ] **Step 4: Commit** — `feat(client): the Governance tab names the town's rank and the player's title`

---

### Task 11: The world map shows territory and lands, and claims land

**Files:**
- Modify: `crates/client/src/renderer/history_view.rs` (`HistoryView`: `territory: Territory`, intake of `ServerPacket::Territory`, `embark_tile_check`, new `land_tile_check`), `crates/client/src/renderer/slint_overlay.rs` (near `site_marks` l.1073-1107 and the embark check l.2052-2112), `crates/slint-ui/ui/world-map.slint`
- Test: `#[cfg(test)]` modules of `history_view.rs` and `slint_overlay.rs`

**Interfaces:**
- Consumes: `founding_allowed_in`, `claim_allowed_in`, `TownView`, `Territory`, `SiteMarker.{lands, rank}`, `CivMarker.capital`, `site_color(civ)`.
- Produces:
  - `HistoryView::town_views(&self) -> Vec<TownView>` (from the markers, real `lands` and `rank`); `embark_tile_check(civ, tile)` now passes it and `&self.territory`.
  - `pub fn land_tile_check(&self, tile: RegionCoords) -> Option<LandAction>` with `pub enum LandAction { Claim, Release, Refused(LandRefusal) }` — `None` when no fortress is held or the tile neither is one of its lands nor shares an edge with one; `Release` for an own land that is not the seat (`Refused(Seat)` for the seat); else `Claim` or `Refused(e)` from `claim_allowed_in`.
  - `pub fn political_overlay_rgba(origin: RegionCoords, width: u32, height: u32, territory: &Territory, sites: &[SiteMarker]) -> Vec<u8>` — one RGBA pixel per map tile, row-major from `origin`: wilderness `[0, 0, 0, 0]`; territory the civ's `site_color` with alpha 70; a town's land the same colour with alpha 150; a seat alpha 220.
  - `world-map.slint`: `in property <image> political-overlay` drawn over the map image at the same rect with `image-rendering: pixelated`; `in-out property <bool> show-political: true` with a toggle beside the layer buttons; side-panel `in property <string> land-action-text` (`""` hides the button — bind the button's own `visible`, never a component root's), `in property <string> land-reason`, `callback land-action()`. The capital's `SiteMark` gets `capital: bool`, drawn as a ring around the square.
- Side-panel copy: `Claim` → button "Claim for {site name}"; `Release` → button "Give up this land"; `Refused(e)` → the Task 10 line as `land-reason`.

- [ ] **Step 1: Write the failing tests**

```rust
// slint_overlay.rs
#[test]
fn political_overlay_marks_territory_lands_and_wilderness() {
    let t = Territory::from_cells([(RegionCoords::new(1, 0), CivId(0)), (RegionCoords::new(2, 0), CivId(0))]);
    let town = /* SiteMarker: civ 0, region (2,0), lands [(2,0)] */;
    let px = political_overlay_rgba(RegionCoords::new(0, 0), 3, 1, &t, &[town]);
    assert_eq!(px.len(), 12);
    assert_eq!(px[3], 0);        // (0,0) wilderness
    assert_eq!(px[7], 70);       // (1,0) territory
    assert_eq!(px[11], 220);     // (2,0) the seat
    assert_eq!(&px[4..7], &px[8..11], "one colour per civ");
}

// history_view.rs
#[test]
fn land_tile_check_offers_claim_release_or_a_reason() {
    // A held Village at (10,10) with lands [(10,10)] inside its own 13x13 territory:
    //   (11,10) -> Some(LandAction::Claim)
    //   (10,10) -> Some(LandAction::Refused(LandRefusal::Seat))
    //   (13,10) -> None                      (not next to the town)
    // With lands [(10,10),(11,10)]:
    //   (11,10) -> Some(LandAction::Release)
    //   (12,10) -> Some(LandAction::Refused(LandRefusal::NoAllowance))
    // With no fortress held: every tile -> None
}
```

Write the second test out in full with a `HistoryView` built the way the existing `history_view.rs` tests build one.

- [ ] **Step 2: Implement** the view state, the two pure functions, the Slint properties and the wiring: the overlay image is rebuilt when `Territory` or `Sites` arrive; `land-action()` sends `ClientPacket::ClaimLand` / `ReleaseLand` for the selected tile to the home connection.
- [ ] **Step 3: Verify** — `just smoke`. Ask Dawid before running `just test-one client "--lib political_overlay"` and `"--lib land_tile_check"`. Then look at it: `just town_rank=Village run`, open the world map — the home civilisation's territory is tinted around the fortress, wilderness is plain, the tile east of the fortress offers "Claim for …", and claiming it darkens the tile and changes the Governance line to "Lands 2 / 2".
- [ ] **Step 4: Commit** — `feat(client): the world map shows civilisation territory and town lands, and claims land`

---

### Task 12: Docs follow the code

**Files:**
- Modify: `docs/game-design.md` (§7.1: the fixed name "the Warden of <site>" is replaced by the rank title; add ranks, lands, territory and capital to the decided sections with a pointer to the spec), `TODO.md` (direction notes: ranks/lands/territory landed on the history side; the play plan is next), `docs/superpowers/specs/2026-11-01-world-history-events-design.md` (l.206 and l.458: "one site per region" → "one seat per region; a site's lands are in the 2026-11-14 town-ranks spec"), `CLAUDE.md` (Architecture rules, Server / store: one bullet — "Ranks, lands, territory and capitals live in the coordinator's `History` (`history/{rank,territory,lands,leaders}.rs`); the rules are plain-data functions the client shares; territory is only ever added")

- [ ] **Step 1: Make the four edits.**
- [ ] **Step 2: Commit** — `docs: town ranks, lands and civilisation territory are part of the design`
