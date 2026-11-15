# Playing a Town Across Its Lands Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A town's holder — and nobody else — can designate in every region of the town's lands; residents live anywhere in the lands; the fortress tools, overlays and the simulated window cover the lands instead of one home region.

**Architecture:** Each region carries one small piece of rollback state, its **land hold** (open, closed, or held by a client), set by the manager from the coordinator's site markers; orders and creative edits check it in the sim, so server and predicting client agree. Land orders are stamped with their target region and routed there by the manager. The history treats every land region as "at the site". The client keys its fortress tools by region over the subscribed lands.

**Tech Stack:** Rust; `game` (sim, `#[rollback]` state, `WorldManager`), Bevy client (`crates/client/src/renderer/commander/`), WGSL (`assets/shaders/voxel_pull.wgsl`).

**Spec:** `docs/superpowers/specs/2026-11-14-town-ranks-multi-region-fortress-design.md` — order-of-work pieces 2, 6, 7 and the camera window of 8. **Depends on** `docs/superpowers/plans/2026-11-14-town-ranks-lands-territory.md` being complete (it provides `Site.lands`, `History.dwelling`, `SiteMarker.lands`, `WorldManager::lands_of`, the `VOXEL_DEV_TOWN_RANK` dial and the test helpers in `tests/common/towns.rs`).

**Not in this plan:** ordering land that sits on another host. Until its own plan (after the multi-host go/no-go), such a region's hold is `Closed`.

## Global Constraints

- Work on `develop` in this checkout. **No git worktrees.**
- `game` stays engine-agnostic. Ordered state is `BTreeMap` / `BTreeSet`. Tunables are named consts. Never a region edge as a literal — derive from `REGION_BLOCKS`.
- Rollback bar: `hash(before) == hash(after undo)`, bit-exact. New region state goes through the `#[rollback]` undo wrappers; no hand-written `Undo` closure unless it is a true inverse.
- A region never reads another region's state: the manager tells a region its hold by a message.
- The history file shape does not change in this plan (`dwelling` already exists).
- After Task 1: `just determinism-wasm`. After Tasks 2 and 5: `just test-threads`. `just smoke` after every task.
- Tests: `just test-one game "--test it <module>::"`. Client tests are a slow opt-in — **ask Dawid before running them.** Tag tests slower than 100 ms `#[ignore = "slow: >100ms — run via just test-full"]`.
- Shaders are compiled in: after Task 8 run `just pull-smoke` on a real adapter and look at the terrain with `just run`.
- Any new per-world client resource is registered for session reset (`add_session_reset` / `SessionLocal` / `SessionScoped`).
- Commit after each task: `feat(game|client): <sentence>` + the session's `Co-Authored-By` line.

## Review Focus

1. **An order arrives for land that was just released** (the order and the release cross): the region is already `Closed`, the order is refused, and the predicting client refuses it the same way. → Task 1 test.
2. **The holder disconnects, retires or returns**: the lands close when they leave and open to them again when they are back; a second player can never designate there in between. → Task 2 tests.
3. **A land region parks and starts again**: its hold is in place before the first order reaches it. → Task 2 test.
4. **A resident is standing on land when the land is released**: they are not lost; when that region parks they walk home to the seat. → Task 3 test.
5. **A drag box crosses the edge of the lands**: only the parts on the town's land are ordered; nothing lands in the neighbour's region or in wilderness. → Task 6 test.

---

### Task 1: The land hold — region state that orders and edits obey

**Files:**
- Create: `crates/game/src/land_hold.rs` (`LandHold`); Modify: `crates/game/src/lib.rs` (`mod land_hold; pub use land_hold::*;`)
- Modify: `crates/game/src/state.rs` (declare `land_hold` beside `designations` at l.307 with the single-value undo wrapper the neighbouring fields use; accessor beside `designations()` at l.464), `crates/game/src/protocol.rs` (`GameEventKind::SetLandHold(LandHold)`), `crates/game/src/region.rs` (apply it in `handle_event`; add it to the two exhaustive matches at l.161 and l.257), `crates/game/src/orders.rs` (`apply_order_safe`, `Order::is_land_order`), `crates/game/src/edit.rs:681` (`player_edit_safe`), `crates/game/src/portable.rs` (classify: not portable), `crates/game/src/world_manager/mod.rs` (the server-authored list at l.1957-1971: a client may not inject it)
- Test: `crates/game/tests/orders.rs`, `crates/game/tests/block_edit.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, StateHash)]
  pub enum LandHold {
      /// No history decides who builds here (a world without one, tests): anyone may order.
      #[default] Open,
      /// Nobody may order here: wilderness, another town's land, a town nobody holds.
      Closed,
      /// This client's town owns the region.
      Held(ClientId),
  }
  impl LandHold { pub fn allows(self, client: ClientId) -> bool; }   // Open: true; Held(c): c == client; Closed: false

  impl Rollback { pub fn set_land_hold_safe(&mut self, hold: LandHold); pub fn land_hold(&self) -> LandHold; }
  impl Order { pub fn is_land_order(&self) -> bool; }                // Designate | Erase | CancelOp
  ```
- Rules: `apply_order_safe` returns `false`, changing nothing, for `Designate`, `Erase`, `CancelOp` and `DevCompleteAll` when `!self.land_hold().allows(client)`. `player_edit_safe` returns `false` for a client whose `mode` is `PlayMode::Fortress` unless the target block is in this region and `land_hold().allows(client)`; Adventure mode is unchanged. A region restored from a parked blob starts `Open` and is told its hold by the manager (Task 2).

- [ ] **Step 1: Write the failing tests** (in `tests/orders.rs`, beside its existing designate tests and with its setup helpers)

```rust
#[test]
fn a_closed_region_refuses_orders_and_a_held_one_takes_its_holders() {
    // rb: a Rollback with client A, as the neighbouring designate tests build it.
    let order = /* the Order::Designate those tests use, op.client == A */;
    rb.set_land_hold_safe(LandHold::Closed);
    assert!(!rb.apply_order_safe(A, order.clone()));
    assert!(rb.data.designations().is_empty());
    rb.set_land_hold_safe(LandHold::Held(B));
    assert!(!rb.apply_order_safe(A, order.clone()));
    rb.set_land_hold_safe(LandHold::Held(A));
    assert!(rb.apply_order_safe(A, order.clone()));
    assert!(!rb.data.designations().is_empty());
    // Review focus 1: the land is released, then a late order arrives.
    rb.set_land_hold_safe(LandHold::Closed);
    assert!(!rb.apply_order_safe(A, Order::Erase { area: /* the same area */, only: None }));
    assert!(!rb.data.designations().is_empty(), "a refused erase removes nothing");
}

#[test]
fn setting_the_hold_undoes_bit_exactly() {
    // hash before; set_land_hold_safe(Held(A)); undo as the file's other undo tests do; hash equal.
}

#[test]
fn an_open_region_behaves_as_before() {
    // No set_land_hold_safe at all: the existing designate succeeds (LandHold::Open is the default).
}
```

and in `tests/block_edit.rs`: `a_fortress_mode_edit_needs_the_hold` — with `Closed`, `player_edit_safe(client, EditAction::Break, &mut out)` is `false` and `out` is empty; with `Held(client)` it is `true`; in `PlayMode::Adventure` with `Closed` it is `true`.

- [ ] **Step 2: Run to see them fail** — `just test-one game "--test it orders::"`, `just test-one game "--test it block_edit::"`.
- [ ] **Step 3: Implement** the type, the state field, the event, the two checks.
- [ ] **Step 4: Run** — both modules pass, with every pre-existing test in them untouched. `just smoke`. `just determinism-wasm`: the region state hash now covers the hold, so pinned hashes (`wasm_determinism`, `scene_hash_golden`, `physics_goldens` if it hashes region state) may move — re-record them on purpose by each file's own procedure, at chunk size 16 only, and list in the commit message which were re-recorded.
- [ ] **Step 5: Commit** — `feat(game): a region knows who may give orders in it — the land hold`

---

### Task 2: The manager sets each region's hold from the towns' lands

**Files:**
- Create: `crates/game/src/world_manager/land_holds.rs`; Modify: `crates/game/src/world_manager/mod.rs` (`mod land_holds;`, field `land_holds_sent: BTreeMap<RegionCoords, LandHold>`), `crates/game/src/world_manager/world_link.rs` (call sites)
- Test: `crates/game/tests/history_play.rs`

**Interfaces:**
- Consumes: `WorldManager::lands_of(site)`, `fortress_of`, `LandHold`, `GameEventKind::SetLandHold`.
- Produces:
  ```rust
  impl WorldManager {
      /// What `rc`'s hold should be: Open with no history; Held(c) when a connected client c plays
      /// the fortress whose lands contain rc; else Closed.
      pub(super) fn land_hold_of(&self, rc: RegionCoords) -> LandHold;
      /// Send `SetLandHold` to every running region whose hold differs from the last one sent.
      pub(super) fn refresh_land_holds(&mut self);
  }
  ```
- Call `refresh_land_holds` wherever its inputs change: in `world_region_started` (send to `rc` unconditionally — before anything else reaches the region — and record it), after `note_sites` (lands changed), and wherever `fortress_of` is written (`world_reply` embark/return, `retire`, the claim reconciliation at `world_link.rs:813-824`, hand-off arrivals in `peers.rs:478,582`, the disconnect path at `mod.rs:3390`). Drop a region from `land_holds_sent` when it parks or is lost.

- [ ] **Step 1: Write the failing tests** (rig and helpers of `tests/history_play.rs`; read designations the way `tests/orders.rs` reads them from a region)

```rust
#[test]
fn only_the_holder_designates_in_a_fortress() {
    // embark_world(); client A embarks at region F.
    // A sends an Order::Designate in F            -> F holds designations.
    // Client B connects, PlayRegion(F), sends one -> F's designations are unchanged.
    // A retires; A (now at the spawn region) cannot designate there: a history world's
    //   wilderness is Closed.
    // A returns to the fortress and designates    -> accepted again.
}

#[test]
fn a_restarted_fortress_region_has_its_hold_before_the_first_order() {
    // A embarks at F, retires (F parks after the grace), returns (F starts again),
    // and its very first packet after PlayerRegion is a Designate: accepted.
}

#[test]
fn a_world_without_history_stays_open() {
    // The rig built with no history: a client designates in its home region as before.
}
```

- [ ] **Step 2: Run to see them fail** — `just test-one game "--test it history_play::"`.
- [ ] **Step 3: Implement** `land_holds.rs` and the call sites.
- [ ] **Step 4: Run** — `history_play::` passes; then `just test-threads` (the manager and a seam-crossing message changed) and `just smoke`.
- [ ] **Step 5: Commit** — `feat(game): the manager closes every region but a town's lands to everyone but its holder`

---

### Task 3: Residents live anywhere in the lands

**Files:**
- Modify: `crates/game/src/history/handover.rs` (`embody_site` l.212, `embody_abstract_residents` l.318, `settle` l.638, `walk_home` l.658), `crates/game/src/history/play.rs` (`embark_source` l.16, `notice_regions` l.155), `crates/game/src/history/chain.rs` (`record_death`: drop the dead from `dwelling`)
- Create: `crates/game/tests/town_residents.rs`; Modify: `tests/it.rs`

**Interfaces:**
- Consumes: `Site.lands`, `History.dwelling`, `History::{claim_land, release_land}`, helpers in `tests/common/towns.rs`.
- Produces: `pub fn History::dwelling_of(&self, person: PersonId) -> Option<RegionCoords>` — for a person whose abstract location is `AtSite(s)`: their `dwelling` entry, else the seat; `None` otherwise.
- Rules:
  - `walk_home(person, from, s)`: when `site.lands.contains(&from)` the person is (stays) a member, `AtSite(s)`, and `dwelling[person] = from` (entry removed when `from` is the seat). Any other `from` behaves as today.
  - `embody_abstract_residents(rc)` takes the `AtSite(s)` people whose `dwelling_of` is `rc`. `embody_site(site)` embodies, for each land region that is running, the people dwelling there.
  - `embark_source`: a person is free only when none of the site's land regions is live.
  - `notice_regions`: a site's news goes to its seat when the seat is live, else to its first live land region (one region, so the holder gets each line once).
  - (`release_land` already drops `dwelling` entries of the released region — the territory plan.)

- [ ] **Step 1: Write the failing tests** in `tests/town_residents.rs`

```rust
use crate::common::towns::*;
use game::history::*;

/// A Village with lands [(10,10), (11,10)] and one of its adults.
fn two_land_town() -> (History, SiteId, PersonId) {
    let mut h = history_on(&flat_map(30, 30));
    let civ = h.add_civ(Race::Dwarf);
    let s = town(&mut h, civ, rc(10, 10), 20);
    set_rank(&mut h, s, TownRank::Village);
    h.claim_land(s, rc(11, 10)).unwrap();
    let p = *h.site(s).unwrap().members.iter().next().unwrap();
    (h, s, p)
}
fn spawned(events: &[game::GameEventKind]) -> Vec<PersonId> {
    events.iter().filter_map(|e| match e {
        game::GameEventKind::SpawnPerson(sp) => Some(sp.person), _ => None }).collect()
}

#[test]
fn a_resident_who_walks_onto_town_land_stays_a_resident() {
    let (mut h, s, p) = two_land_town();
    assert_eq!(spawned(&h.region_started(rc(10, 10))).len(), 20);
    assert!(!h.person_crossed(p, rc(11, 10)), "that region is not running: absorbed");
    assert!(h.site(s).unwrap().members.contains(&p));
    assert_eq!(h.person(p).unwrap().location, Location::AtSite(s));
    assert_eq!(h.dwelling_of(p), Some(rc(11, 10)));
    assert_eq!(spawned(&h.region_started(rc(11, 10))), vec![p]);
}

#[test]
fn off_the_lands_they_still_leave_and_walk_home() {
    let (mut h, s, p) = two_land_town();
    h.region_started(rc(10, 10));
    assert!(!h.person_crossed(p, rc(10, 11)));
    assert!(!h.site(s).unwrap().members.contains(&p));
    assert_eq!(h.dwelling_of(p), None);
}

#[test]
fn parking_town_land_keeps_its_people_there() {
    let (mut h, s, p) = two_land_town();
    h.region_started(rc(10, 10));
    h.region_started(rc(11, 10));
    assert!(h.person_crossed(p, rc(11, 10)));
    h.region_parked(rc(11, 10), vec![(p, [1.0, 2.0, 3.0])]);
    assert!(h.site(s).unwrap().members.contains(&p));
    assert_eq!(h.dwelling_of(p), Some(rc(11, 10)));
}

#[test]
fn released_land_sends_its_people_home() {
    let (mut h, s, p) = two_land_town();
    h.region_started(rc(10, 10));
    h.person_crossed(p, rc(11, 10));
    h.release_land(s, rc(11, 10)).unwrap();
    assert_eq!(h.dwelling_of(p), Some(rc(10, 10)));
    assert!(h.site(s).unwrap().members.contains(&p));
}

#[test]
fn a_resident_standing_on_land_that_is_released_walks_home_at_the_park() {
    let (mut h, s, p) = two_land_town();
    h.region_started(rc(10, 10));
    h.region_started(rc(11, 10));
    h.person_crossed(p, rc(11, 10));                       // embodied on the land
    h.release_land(s, rc(11, 10)).unwrap();
    h.region_parked(rc(11, 10), vec![(p, [0.0; 3])]);
    assert!(h.person(p).unwrap().alive());
    assert_ne!(h.person(p).unwrap().location, Location::Unplaced, "never lost");
    // Off the lands now: the old rule — a journey home (or a stray if no route).
    assert!(matches!(h.person(p).unwrap().location, Location::Journey(_) | Location::Wandering(_)));
}
```

- [ ] **Step 2: Run to see them fail** — `just test-one game "--test it town_residents::"`.
- [ ] **Step 3: Implement** the rules above.
- [ ] **Step 4: Run** — 5 passed; `just test-one game "--test it history_handover::"` and `"--test it history_play::"` still pass.
- [ ] **Step 5: Commit** — `feat(game): a town's residents are at home anywhere in its lands`

---

### Task 4: A town's notices reach its holder wherever the camera is

**Files:**
- Modify: `crates/game/src/world_manager/world_link.rs:828-835` (`route_notices`)
- Test: `crates/game/tests/history_play.rs` (beside `notices_only_to_home_region`, l.520)

**Interfaces:**
- Rule: a notice `(rc, line)` goes to each connected client that is homed in `rc` **or** plays a fortress whose lands (`lands_of`) contain `rc` — once per client.

- [ ] **Step 1: Write the failing test** `fortress_notices_follow_the_holder` — A embarks at F, then `PlayRegion`s to another region; a birth or arrival at F's site (provoke it as `notices_only_to_home_region` does) still reaches A as `ServerPacket::Notice`, exactly once; client B, homed elsewhere with no fortress, gets none.
- [ ] **Step 2: Run to see it fail**, implement, run `just test-one game "--test it history_play::"` → passes with `notices_only_to_home_region` unchanged.
- [ ] **Step 3: Commit** — `feat(game): a town's news reaches its holder, not only whoever stands in its seat`

---

### Task 5: The server routes a land order to the region it names

**Files:**
- Modify: `crates/game/src/world_manager/mod.rs:1912-1947` (the `PlayerInput | UiAction | Order` routing arm)
- Test: `crates/game/tests/history_play.rs`

**Interfaces:**
- Consumes: `Order::is_land_order`, `land_hold_of`, `GameEvent.region_id`.
- Rule: for `GameEventKind::Order(cid, order)` with `order.is_land_order()`, let `rc` be the coordinates of `event.region_id`. When `self.land_hold_of(rc) == LandHold::Held(cid)` and `rc` is running on this host, send the input to `rc`. In every other case the event takes today's path (the client's home). The region applies an order from a client that has no body in it: `apply_order_safe` needs only the hold.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_land_order_goes_to_the_land_it_names() {
    // Coordinator with set_dev_min_rank(Some(TownRank::Village)). A embarks at F, a month passes,
    // A claims E (east of F) and subscribes to it (RequestRegionConnection), so E runs.
    // A's home is still F.
    // GameEvent { kind: Order(A, Designate{..}), region_id: E } -> E holds the designation, F none.
    // The same order stamped with a region W that is not A's land   -> W holds none; it went to
    //   F (A's home) exactly as before this task.
    // Client B stamps E                                             -> E unchanged.
}
```

Write it out in full with the rig's helpers.

- [ ] **Step 2: Run to see it fail**, implement, run `just test-one game "--test it history_play::"`, then `just test-threads` and `just smoke`.
- [ ] **Step 3: Commit** — `feat(game): land orders are routed to the region they are stamped for`

---

### Task 6: The client designates on any of its lands

**Files:**
- Modify: `crates/game/src/orders.rs` (new pure `split_world_box`), `crates/game/tests/orders.rs`
- Modify: `crates/client/src/instance.rs:1251-1277` (order send path), `crates/client/src/renderer/commander/designate.rs` (l.415-417 `clamp_to_region`, l.493, l.502-507 `cursor_ray`, l.604-616 commit), `crates/client/src/renderer/history_view.rs` (`fortress_lands()`)

**Interfaces:**
- Produces:
  ```rust
  // game/src/orders.rs
  /// The parts of the world-block box `a..=b` (corners in any order), one per region it overlaps,
  /// each as that region's local TileBox. Ordered by (region.z, region.x).
  pub fn split_world_box(a: WorldBlockPos, b: WorldBlockPos) -> Vec<(RegionCoords, TileBox)>;

  // client HistoryView
  pub fn fortress_lands(&self) -> Option<&[RegionCoords]>;      // the held fortress's SiteMarker.lands

  // client Instance
  /// One of this client's land orders for `region`: predicted there when that region is
  /// predicted here, else sent unpredicted; either way the event is stamped with `region`.
  pub fn send_land_order(&mut self, region: RegionCoords, order: game::Order);
  ```
- Rules in `designate.rs`: the hover tile is found in the region under the cursor (`RegionId::from_world`, as `selection.rs:201` does) and exists only when that region is one of `fortress_lands()`; a drag is kept in world blocks, split with `split_world_box`, the parts outside the lands are dropped, and each remaining part is sent with `send_land_order` as an `Order::Designate` / `Erase` carrying the **same `OpId`**. `Order::CancelOp(op)` is sent to every land region. All other orders (`SetMode`, `MoveTo`, the dev orders) keep today's home path.

- [ ] **Step 1: Write the failing test** (`tests/orders.rs`)

```rust
#[test]
fn a_world_box_splits_at_region_edges() {
    let r = game::REGION_BLOCKS as i32;
    let at = |x, y, z| game::WorldBlockPos::from_local(RegionCoords::new(0, 0).into(), game::BlockPos::new(x, y, z));
    let parts = split_world_box(at(r - 2, 5, 3), at(r + 1, 5, 4));
    assert_eq!(parts, vec![
        (RegionCoords::new(0, 0), TileBox { min: [r - 2, 5, 3], max: [r - 1, 5, 4] }),
        (RegionCoords::new(1, 0), TileBox { min: [0, 5, 3], max: [1, 5, 4] }),
    ]);
    assert_eq!(split_world_box(at(3, 0, 3), at(1, 2, 1)).len(), 1, "corners in any order, one region");
    assert_eq!(split_world_box(at(-1, 0, -1), at(0, 0, 0)).len(), 4, "negative coordinates split too");
}
```

(Build `at` with whatever constructor `WorldBlockPos` really offers — `from_local` is the one `edit.rs:705` uses.)

- [ ] **Step 2: Run to see it fail** — `just test-one game "--test it orders::a_world_box_splits"`; implement `split_world_box`; passes.
- [ ] **Step 3: Client.** Add `fortress_lands`, `send_land_order` (the `Predict` / `send_unpredicted` pair of l.1251-1277, with the target region in place of `home`), and change `designate.rs` as ruled above. Add a unit test in `designate.rs` for the pure filter `parts_on_lands(parts, lands) -> Vec<(RegionCoords, TileBox)>`: of the two parts of the Step 1 box with lands `[(0,0)]`, only the first survives (review focus 5).
- [ ] **Step 4: Verify** — `just smoke`; ask Dawid before `just test-one client "--lib parts_on_lands"`. Then by hand: `just town_rank=Village run`, claim the eastern region on the world map, drag a dig designation across the seam: it appears on both sides, and stops at the edge of the lands.
- [ ] **Step 5: Commit** — `feat(client): designations reach every region of the town's lands and stop at their edge`

---

### Task 7: The fortress tools cover the lands

Read these files in full before changing them; each is bound to the home region today.

**Files:**
- Modify: `crates/client/src/renderer/commander/tiles.rs` (`TileMirror`, l.1-14, 98-121, 144-147), `crates/client/src/renderer/commander/level.rs` (l.155-161, 277-279), `crates/client/src/renderer/commander/visible.rs` (l.28-39), `crates/client/src/renderer/commander/fortress_view.rs` (l.15-18, 384-398), `crates/client/src/renderer/commander/cut_entities.rs` (l.6, 47-73), `crates/client/src/renderer/atmosphere.rs` or wherever the underwater fog reads the mirror (`TODO.md:137`)

**Interfaces:**
- Consumes: `HistoryView::fortress_lands`, the instance's set of predicted regions.
- Produces — decisions:
  - **The tool regions** are `tool_regions(home, lands, predicted) -> BTreeSet<RegionCoords>`: the home region plus every land region currently predicted (pure, in `tiles.rs`, unit-tested: home always present; an unpredicted land absent; a predicted non-land region absent).
  - `TileMirror` holds `BTreeMap<RegionCoords, RegionTiles>` (today's per-region contents become `RegionTiles`) for the tool regions; `in_home` becomes `in_tool_region(world_pos) -> Option<RegionCoords>`; lookups take a region.
  - **One working level for the town**: the level stays a single value (regions share the vertical axis); the cut and the designation overlay are applied to every tool region's rect, one overlay texture per region (`REGION_BLOCKS` square, as today's single one).
  - Entities are hidden by the cut in every tool region; underwater fog uses whichever tool region the camera is over.
  - The mirror's per-region entries are dropped when a region leaves the tool regions and on session reset.

- [ ] **Step 1: Write the failing unit test** for `tool_regions` in `tiles.rs`.
- [ ] **Step 2: Implement** file by file in the order listed; `just smoke` after each file.
- [ ] **Step 3: Verify** — ask Dawid before `just test-one client "--lib tool_regions"`. By hand with `just town_rank=Village run` and the eastern region claimed: lower the working level — the cut shows in both regions at the same height; a designation's overlay shows on both sides of the seam; units on the eastern land are hidden above the cut; the unclaimed northern neighbour is drawn uncut.
- [ ] **Step 4: Commit** — `feat(client): the working level, cut and overlays cover every region of the town`

---

### Task 8: The outline of the lands, the minimap and the People tab

**Files:**
- Modify: `crates/client/src/renderer/boundaries.rs` (l.41-88), `assets/shaders/voxel_pull.wgsl` (`TerrainBoundaries`), `crates/client/src/renderer/commander/minimap.rs`, `crates/client/src/renderer/slint_overlay.rs:2188-2195` (People rows), `crates/game/src/history/rank.rs` (`pub const MAX_TOWN_LANDS: usize = 25;`), `crates/game/tests/town_rank.rs`

**Interfaces:**
- Produces:
  - `MAX_TOWN_LANDS`, with a test `assert_eq!(MAX_TOWN_LANDS as u32, TownRank::Kingsland.lands_allowed())`.
  - `TerrainBoundaries` uniform: `lands: array<vec4<f32>, 25>` (world-space `min_x, min_z, max_x, max_z`), `land_count: u32` — the WGSL array length written as the literal 25 with a comment naming `MAX_TOWN_LANDS`, and a Rust `const _: () = assert!(MAX_TOWN_LANDS == 25);` beside the uniform struct.
  - `build_uniform(lands: &[Vec4]) -> TerrainBoundariesUniform` (pure; with no fortress held it is given the home region's rect alone, today's picture).
  - Shader rule: a fragment is on the line when it is within the line width outside some land rect and inside no land rect — shared edges between two lands draw nothing.
  - Minimap: shows the tool region (Task 7) the camera focus is over, else the home region.
  - People tab: every `EntityKind::Person` in the tool regions.

- [ ] **Step 1: Write the failing tests** — the `MAX_TOWN_LANDS` test (game); a `boundaries.rs` unit test: `build_uniform` of two rects has `land_count == 2` and the rects in order; of 30 rects, `land_count == 25`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verify** — `just test-one game "--test it town_rank::"`, `just smoke`, ask before the client test, then `just pull-smoke` on a real adapter and `just town_rank=Village run`: F8 shows one outline around both regions with no line on the seam between them.
- [ ] **Step 4: Commit** — `feat(client): the boundary line outlines the whole town; the minimap and People tab follow its lands`

---

### Task 9: The simulated window follows the camera over the lands

**Files:**
- Modify: `crates/client/src/instance.rs:795-820` (`desired_window` and its caller), `crates/client/src/renderer/commander/bounds.rs:27-42` (`adventure_window`)
- Test: the `#[cfg(test)]` module of `instance.rs`

**Interfaces:**
- Produces:
  ```rust
  /// The fortress part of the window: the seat, plus the camera's square of `radius` clipped to
  /// the lands and the ring of regions one step around them (corners included).
  fn fortress_window(camera: RegionCoords, lands: &[RegionCoords], radius: i32) -> BTreeSet<RegionCoords>;

  fn desired_window(center: RegionCoords, home: Option<RegionCoords>, radius: i32,
      camera: RegionCoords, lands: Option<&[RegionCoords]>, fortress_radius: i32) -> BTreeSet<RegionCoords>;
  ```
  `lands[0]` is the seat. `camera` is the commander camera's focus region in fortress mode and `center` in Adventure. `adventure_window` widens to `fortress_window`'s bounding rect while the body is inside it.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_one_region_fortress_keeps_todays_window() {
    let seat = RegionCoords::new(10, 10);
    assert_eq!(fortress_window(seat, &[seat], 1), seat.window_radius(1).into_iter().collect());
}

#[test]
fn the_window_follows_the_camera_and_stops_one_ring_outside_the_lands() {
    let lands: Vec<_> = (10..=14).map(|x| RegionCoords::new(x, 10)).collect();   // a row of five
    let w = fortress_window(RegionCoords::new(14, 10), &lands, 1);
    assert!(w.contains(&RegionCoords::new(10, 10)), "the seat always runs");
    assert!(w.contains(&RegionCoords::new(15, 11)), "the ring around the land under the camera");
    assert!(!w.contains(&RegionCoords::new(12, 10)), "land out of view parks");
    assert_eq!(w.len(), 10);                                                     // 3x3 at x 13..=15, plus the seat
    let far = fortress_window(RegionCoords::new(40, 40), &lands, 1);
    assert_eq!(far, [RegionCoords::new(10, 10)].into());
    assert_eq!(fortress_window(RegionCoords::new(14, 10), &lands, 0).len(), 2);  // small worlds: camera region + seat
}
```

- [ ] **Step 2: Implement**, pass the camera focus region and `fortress_lands()` at the caller.
- [ ] **Step 3: Verify** — `just smoke`; ask before `just test-one client "--lib fortress_window"`. By hand: with two lands, pan east — the far side's neighbours load; the server log (`worlds/debug-embarked/server.log`) shows regions two steps behind the camera parking after the 5 s grace, and their residents reappear where they stood when you pan back.
- [ ] **Step 4: Commit** — `feat(client): the simulated neighbourhood follows the camera across the town's lands`

---

### Task 10: Docs follow the code

**Files:**
- Modify: `CLAUDE.md` — Architecture rules: under "Regions & seams" add "A region's **land hold** (`game::LandHold`, set only by the manager's `SetLandHold`) decides who may order there; a town's lands are the regions its holder is `Held` in. Land orders are stamped with their target region and routed there; everything else follows the body's home."; under "Client" replace the home-region wording for the fortress tools with "the tool regions (home ∪ predicted lands, `commander/tiles.rs`)"; the `VOXEL_FORTRESS_RADIUS` dial line: "the camera's radius inside the lands".
- Modify: `TODO.md` — remove l.130 ("Designations are still clipped to the home region…") and l.137 (underwater fog only inside the home region); add "Orders for land on another host (needs its own plan after the multi-host go/no-go)".
- Modify: `docs/game-design.md` §7.2.6 — "its region" → "its lands in view".

- [ ] **Step 1: Make the edits.** **Step 2: Commit** — `docs: a town is played across its lands`
