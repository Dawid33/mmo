# Embark Removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the player embark / return / retire flow, the per-site fortress claim and the separate disembodied
fortress mode. A player plays one character: a player with no claim picks a populated site on the Adventure tab and
starts there. The commander camera stays, as `PlayMode::Overview` behind a debug toggle.

**Architecture:**
- The order is chosen so every commit builds and passes its tests:
  1. the mode rename and default flip;
  2. the client stops sending embark requests;
  3. every test that used embark to prove another guarantee is re-expressed on `StartAdventure`, while embark still
     exists, so the guarantee is never uncovered;
  4. the embark code is deleted end to end;
  5. the persisted formats are bumped;
  6. renames and docs.
- `Claim` keeps its enum shape with the single arm `Adventure(SiteId)`: holdings will add an arm. The coordinator's
  per-site `claims` map goes.

**Tech Stack:** Rust (game / server / world-store / client crates), Slint, bincode, just.

**Spec:**
- `docs/superpowers/plans/2026-11-17-embark-removal-handoff.md`
- `docs/game-design.md` §5, §6, §10
- Dawid's answers, 2026-11-17 (below)

## Decisions (Dawid, 2026-11-17)

1. **Entry point:** the Adventure tab is the start screen for a player with no claim (pick a populated site, then
   `StartAdventure`). `just run` = today's `--adventure` path (the first populated site), with no embark first.
   `run-two` = both clients adventure at the same site. A world with no populated site keeps the Play button
   (`PlayRegion`) as the way in.
2. **PlayMode:**
   - Keep two variants.
   - Rename `Fortress` → `Overview`, keeping the variant ORDER (`Overview` = 0, `Adventure` = 1), so the bincode
     bytes of an existing value do not change.
   - The default becomes `Adventure`.
   - `Overview` is reached by a debug toggle: F9 and a Debug-tab button, both writing the existing, unused
     `ToggleModeRequest`.
   - `end_adventure` no longer sets the mode.
3. **Claims, option A:**
   - delete `Claim::Fortress`, the coordinator's `claims` map, `set_online`, the host's `fortress_of` and
     `Parking.fortress`;
   - holdings are designed fresh later;
   - no data migration: worlds are re-forged.

## Where this plan departs from the handoff (verified 2026-11-17, `e6f1bfd3`)

- **`end_adventure` does not reuse `retire`:** it duplicates the body. Only the doc reference goes.
- **`dev_structures` needs no new anchor:** `announces_region` skips the spawn region in a forged world, so "home" is
  the first other region announced — now the adventure site's region. Only docs and comments change
  (`server/src/dev_structures.rs:17-59`, `server/src/lib.rs:1058-1061`, `server/tests/dev_structures.rs:31-32`).
- **`fortress_region` on the client is always `None` in adventure play:** `send_fortress` sends `None` once an
  adventure starts. So the fortress neighbourhood ring (`instance.rs` `desired_window`'s `fortress` argument,
  `commander/bounds.rs:63`, `FORTRESS_NEAR_RADIUS`, `VOXEL_FORTRESS_RADIUS`) goes, with nothing replacing it.
- **More formats change than the handoff lists:**
  - `COORDINATOR_STATE_VERSION` 3 → 4 (`game/src/coordinator/state.rs:31`);
  - `HISTORY_FILE_VERSION` and `OLDEST_LOADABLE_VERSION` 4 → 5 (`game/src/history/persist.rs:12-13`);
  - `FORMAT_PLAYER_RECORD` 1 → 2 (`world-store/src/codec.rs:14`);
  - `LINK_PROTOCOL_VERSION` 7 → 8 (`svc-link/src/lib.rs:54`): `WorldRequest` / `WorldReply` variants shift on the
    host↔coordinator link.
- **`too_steep_to_embark` goes:**
  - it is player-only: `game/src/world_map.rs:8-16`, `SocietyGrid.too_steep` (`history/grid.rs:25,91,142-145`), and
    the stats line in `worldgen/examples/world_map.rs:297-310`;
  - so do `player_embark_allowed` and `player_embark_allowed_in` (`history/chain.rs:280-292, 459-473`);
  - `embark_allowed_in` (NPC rule, client map shading) stays.
- **The autopilot flies**, and `PlayMode::Adventure` forbids flight (`game/src/camera.rs:303`). With the new default,
  `VOXEL_AUTOPILOT` runs switch to `Overview` first, or `fly-probe`, `shuttle-probe` and `go-nogo-r3` would walk.

## Global Constraints

- Work on `develop`, no worktrees. **Commit only your own paths:** `git diff --cached --name-only` before each
  commit, then commit with a pathspec. Other sessions stage in the shared index.
- No data migration:
  - a format bump refuses old rows and blobs;
  - no decode fallbacks;
  - login accounts are untouched (`PlayerRecord` is a world-store blob).
- Keep the classification macros complete:
  - `projection!` (`coordinator/mod.rs:240`);
  - `host_fields!` (`world_manager/succession.rs:316`);
  - `carry_components` / `Client::arrive`.
- NPC embarks stay:
  - `Purpose::Embark`, `send_embark_party`, `arrive_embark`, the handover, `found_site`;
  - `EmbarkRefusal::{OffMap, NotHabitable, Occupied, TooClose}`.
- `CivMarker` stays (Adventure and Legends tabs). The fortress HUD (`fortress-panel.slint`, `fortress_panel.rs`)
  stays.
- **Keep the justfile in sync** with every recipe change, in the same commit.
- Test commands: `just test-one <crate> "--test <target> -- <filter>"` (two or more filters must come after `--`).
  - Server targets `roles` and `coordinator_rt` need `--features test-hooks`.
  - Never run whole suites as a routine check.
  - Client tests (`just test-one client …`) only with Dawid's OK: ask before the client test runs in Tasks 2, 3 and 6.
  - `just test-full` only if he asks.
- Never format from the repo root (`cargo fmt --all`): format touched files only.

## Review Focus

1. **A reconnecting player who was adventuring** is restored into the adventure and lands in play. They are not
   shown the start screen again, and a second `StartAdventure` is not refused `AlreadyPlaying`. Covered by Task 2
   (debug-launch "restored" wait) and Task 4 (`adventure_kept_across_reconnect`, kept).
2. **A world with no history, or no populated site,** still has a way in: the Play button and the debug launch's
   play-here fallback. Covered by the Task 2 test `first_populated_or_play_here` and Task 3's `show_play` rule.
3. **Autopilot probes still fly** after the default flip. Covered by the Task 1 step that sends `Overview` when
   `VOXEL_AUTOPILOT` is set; `just fly-probe` is checked by hand in Task 8.
4. **An old store is refused, not misread:**
   - an old coordinator row → `OldVersion`;
   - an old history file → `found: 4`;
   - an old player record → its format byte;
   - an old host on the link → a version refusal.

   Covered by Task 6's tests (`a_v4_file_is_refused`, `state_round_trips` format byte) and the codec golden.
5. **End adventure, then start another** returns the player to spawn in a body, back to the start screen, with the
   claim released (no `AlreadyPlaying` on the next start). Covered by Task 4's rewrite of
   `end_adventure_returns_to_spawn_in_fortress_mode` → `end_adventure_returns_to_spawn_and_frees_the_claim`.

---

### Task 1: `PlayMode::Fortress` → `Overview`, the body by default, a debug toggle

**Files:**
- Modify:
  - `crates/game/src/adventure.rs:1-23,90-130`
  - `crates/game/src/ui.rs:101`
  - `crates/game/src/world_manager/world_link.rs:1017-1035`
  - `crates/game/src/world_manager/peers.rs:2024`
  - every other `PlayMode::Fortress` in `crates/*/src` and `crates/*/tests`
  - `crates/client/src/renderer/commander/adventure.rs:37-94`
  - `crates/client/src/renderer/dev_ui.rs:82-133`
  - the Debug tab `.slint` beside it
  - `crates/client/src/renderer/autopilot.rs`
- Test:
  - `crates/game/tests/adventure.rs`, `inventory.rs`, `item_piles.rs`, `chop.rs`, `intents.rs`,
    `handoff_schedule.rs`, `world_manager.rs`
  - `crates/server/tests/observer.rs`, `threaded_world.rs`

**Interfaces:**
- Produces:
  - `pub enum PlayMode { Overview, #[default] Adventure }`, with `Overview` declared first;
  - `ToggleModeRequest` written by F9 and `DevAction::ToggleOverview`.

- [ ] **Step 1: Rename the variant everywhere and flip the default.** Keep the declaration order. In the tests,
  rename the test fns that say "fortress mode":
  - `new_client_is_fortress` → `new_client_is_in_a_body` (asserts `PlayMode::Adventure`);
  - `fortress_mode_ends_the_walk` → `overview_ends_the_walk`;
  - `move_to_refused_in_fortress_mode` → `move_to_refused_in_overview`;
  - `fortress_body_still_flies` → `overview_body_still_flies`;
  - `fortress_mode_leaves_survival_state` → `overview_leaves_survival_state`;
  - `closing_in_fortress_returns_the_cursor` → `closing_in_overview_returns_the_cursor`;
  - `fortress_clients_never_pick_up` → `overview_clients_never_pick_up`;
  - `fortress_chop_still_yields_nothing` → `overview_chop_still_yields_nothing`.

  `set_mode_emits_ui_update_once` now asserts that setting `Adventure` (the default) is not news.
- [ ] **Step 2: Fix tests whose setup relied on the old default.** A test whose setup relied on a new client being in
  `Fortress` (flight, no pickup, no walk) sets `client.mode = PlayMode::Overview` in its setup, instead of having
  its assertion changed. Find them by running each touched test file.
- [ ] **Step 3: `end_adventure` (`world_link.rs:1017`) drops its `SetMode` order.** Its doc says the player
  returns to spawn in their body. Rename `history_play.rs` `end_adventure_returns_to_spawn_in_fortress_mode` →
  `end_adventure_returns_to_spawn_in_a_body`, asserting `PlayMode::Adventure` at spawn.
- [ ] **Step 4: Client toggle.**
  - `toggled` / `toggle_order` work on `Overview` ↔ `Adventure`.
  - F9 (free) and a Debug-tab button `DevAction::ToggleOverview` each write one `ToggleModeRequest`.
  - Fix the `ToggleModeRequest` doc ("nothing writes it") to name them.
- [ ] **Step 5: Autopilot.** When `VOXEL_AUTOPILOT` is set, the autopilot writes one `ToggleModeRequest` once the
  body's `ClientPlayMode` is `Adventure` and the world is in play, before it holds any key. The autopilot module doc
  says why.
- [ ] **Step 6: Run.**
  ```
  just test-one game "--test it -- adventure:: inventory:: item_piles:: chop:: intents:: world_manager:: history_play::"
  just test-threads
  just test-one server "--test observer"
  just test-one server "--test threaded_world"
  just determinism-wasm
  just smoke
  ```
  Expected: all pass.
  - If `scene_hash_golden`, `wasm_determinism`, or the `entity_bundle` / `player_record` goldens change, the cause
    must be the default value encoded as 1 instead of 0. Confirm that from the diff, then re-record on purpose with
    `MMO_RECORD_GOLDENS=1` (the player record is re-recorded again in Task 6) and note it in the commit.
- [ ] **Step 7: Commit:** `refactor(game): PlayMode::Fortress becomes Overview, a debug toggle; a new player is in
  a body`.

### Task 2: The client starts adventures only — the debug launch and the recipes

**Files:**
- Modify:
  - `crates/client/src/debug_launch.rs` (whole state machine, tests :878-1078)
  - `crates/client/src/renderer/history_view.rs` (add `sites_received`)
  - `crates/client/src/main.rs:175`, `crates/client/src/lib.rs:29` (comments)
  - `justfile` (`run` :191-209, `town` :211-233, `run-two` :237-281, `run-release` :326-335, `run-online`
    :1172-1214, `go-nogo-r3` doc ~:1466-1490, `genesis_probe` doc :485)
  - `scripts/run.sh:14-19`

**Interfaces:**
- Produces:
  - preset `client debug world [--reset | --port N | --join HOST:PORT] [--site N] [--prefab …] [--town …]` and
    `client debug online`;
  - `DEBUG_WORLD_SLUG = "debug-world"`, `DEBUG_WORLD_NAME = "Debug: world"`;
  - log line `[debug] playing: adventure site=N` (run-two greps it), or `[debug] playing: no populated site` on the
    play-here fallback;
  - `HistoryView.sites_received: bool`, set by `HistoryMsg::Sites`.

- [ ] **Step 1: Rewrite the parse tests first.** Every `"embarked"` becomes `"world"`. Then:
  - `--adventure` is now an error naming `--site`;
  - `--adventure-at N` → `--site N`;
  - `the_debug_name_slugs_to_the_debug_dir` asserts `debug-world`.

  Add `first_populated_or_play_here`, a pure fn test:

  ```rust
  assert_eq!(adventure_start(&[], true), Start::PlayHere);
  assert_eq!(adventure_start(&[populated(3)], true), Start::Adventure(SiteId(3)));
  assert_eq!(adventure_start(&[], false), Start::Wait); // Sites not received yet
  ```

  Run `just test-one client "--lib debug_launch::"` only with Dawid's OK. Otherwise rely on `just smoke` plus a
  launch.
- [ ] **Step 2: Implement.**
  - `DebugScenario::World { reset, port, join, site: Option<SiteId>, prefab, town }`.
  - `Phase::Starting(Sent)` with `Sent::{Nothing, Adventure}`.
  - `adventure_start(sites: &[SiteMarker], received: bool) -> Start`, where `Start::{Wait, PlayHere, Adventure(SiteId)}`
    wraps `choose_adventure_site`.
  - The "restored player" wait reads only `adventure_result` / `adventuring`: a player the server restored into an
    adventure is done, with a WARN if `--site` names another site.
  - Delete:
    - `choose_embark`, `EmbarkMove`, `next_move`;
    - `VOXEL_DEBUG_EMBARK_AT` (`embark_target_override`), `tried`, the retire-first block;
    - every `RequestEmbarkOptions` / `Embark` / `ReturnTo` / `Retire` send.
  - `embark_after_rejoin` → `restart_after_rejoin`.
  - `online` starts an adventure the same way.
  - The USAGE text and module doc say "adventure at the first populated site", never "embark".
- [ ] **Step 3: Recipes.**
  - `run`, `run-release` and `scripts/run.sh` → `client debug world`.
  - `town` → `client debug world --reset --town …`; its doc says "the adventure site's region".
  - `run-two`:
    - client 1 = `client debug world --port "$port"`;
    - wait on `grep -aoE 'playing: adventure site=[0-9]+'`, still the readiness signal;
    - client 2 = `--join "127.0.0.1:$port" --site "$site"`;
    - doc: "two players adventuring at one site".
  - `run-online` doc: "starts an adventure (or resumes the account's)".
  - Every doc line naming `debug-embarked` → `debug-world`. Old `worlds/debug-embarked/` dirs are simply no longer
    used: the recipe doc for `run` says so once.
- [ ] **Step 4: Verify.** Run `just smoke` (passes), then `just run` by hand: it forges `worlds/debug-world/`, logs
  `[debug] playing: adventure site=N` and lands in first person. Then `just run-two`: both clients are in one
  region.
- [ ] **Step 5: Commit** with a pathspec: `feat(client): the debug launch starts an adventure; no embark`.

### Task 3: The client's UI loses the Fortress tab, Retire and Return

**Files:**
- Modify:
  - `crates/client/src/renderer/slint_overlay.rs` (UiActions :157-166, callbacks :801-809, handlers :1330-1404,
    `apply_embark_result` :1618-1658, map sync :2018-2118, `can_retire` :2137-2140, governance :2191, tests
    :2441-2496)
  - `renderer/history_view.rs` (`EmbarkSelection`, `embark_rows`, `embark_tile_check`, `embark_refusal_text`,
    `visible_embark_refusal`, `returnable()`, `civ_name` → `history.civs`, fields `embark_options`,
    `embark_result`, `selection`, `options_requested`, `embark_pending`, `embark_refusal`, `retired`; tests
    :585-640)
  - `renderer/embark_tabs.rs:15-22` (tab index map)
  - `renderer/fortress_panel.rs:19-26`
  - `crates/client/tests/session_reset.rs:235-243,302-311`
  - `crates/slint-ui/ui/embark.slint` (`EmbarkRow`, `FortressTab` :38-104, the panel's fortress props :278-288,
    tabs :335-390)
  - `ui/game.slint` (:4, :13, :188-198, :263-266, :491-492, :531, :540-550)
  - `ui/menu.slint:9-15,44-47`
  - `ui/index.slint:6`
  - `crates/slint-ui/tests/embark.rs`

**Interfaces:**
- Consumes: Task 2's `HistoryView.sites_received`.
- Produces:
  - tabs `Adventure` = 0, `Legends` = 1 (`EmbarkTab { Adventure, Legends }`, default `Adventure`);
  - `fortress_panel::governance_lines(site: Option<&SiteMarker>)` shows the adventure site's residents, or
    "No settlement".

- [ ] **Step 1: Slint tests first** (`crates/slint-ui/tests/embark.rs`):
  - `tabs_switch_bodies` asserts the panel opens on Adventure (0) and Legends is 1;
  - the adventure and legends tests use the new indices;
  - `menu_shows_end_adventure` asserts no "Retire fortress" button exists.

  Run `just test-one slint-ui "--test it embark::"`. Expected: FAIL.
- [ ] **Step 2: Slint.**
  - Delete `FortressTab`, `EmbarkRow` and the panel's fortress props.
  - Re-index the tabs to `["Adventure", "Legends"]`.
  - In `game.slint`, delete the fortress-tab properties, `retire`, `can-retire` and the menu wiring; `embark-width`
    tests `embark-tab == 1` for Legends.
  - In `menu.slint`, delete the Retire button, `retire` and `can-retire`.

  Run the Step 1 command. Expected: PASS. Then run `just slint-wasm`.
- [ ] **Step 3: Client.**
  - Delete the embark UiActions, callbacks and handlers, `apply_embark_result` and its registration (:552), and the
    `HistoryView` items listed above.
  - The map sync:
    - the panel shows when not in play;
    - `show_play` = `sites_received && no populated site`, which replaces the `embark_options` / `NoHistory` test;
    - the status line shows only the adventure refusal.
  - `EndAdventure`'s handler doc: "back to the start screen".
  - `governance_lines` reads `history.adventuring`'s marker.
  - `bridge.rs:904-908`: `ClientUpdateEvent::{EmbarkOptions, EmbarkResult}` are dropped there with an empty arm
    (Task 6 deletes the variants), and `HistoryMsg::{EmbarkOptions, EmbarkResult}` go with their fields.
    `HistoryMsg::Fortress`, `HistoryView.fortress` and `fortress_region` stay until Task 6.
  - `session_reset.rs` asserts only the fields that still exist.
- [ ] **Step 4: Verify.** Run `just smoke` (passes). Ask Dawid, then run `just test-one client "--lib slint_overlay::
  history_view:: embark_tabs:: fortress_panel::"` and `just test-one client "--test session_reset"`. Then
  `just play` by hand:
  - menu → forge → map;
  - the Adventure tab starts an adventure;
  - End adventure returns to the map;
  - no Fortress tab, no Retire.
- [ ] **Step 5: Commit:** `feat(client,slint-ui): the start screen is the Adventure tab; Fortress tab, Retire and
  Return go`.

### Task 4: Re-express the game-crate guarantee tests on `StartAdventure`

Embark still exists in this task. Each rewritten test keeps its name's guarantee and must pass before Task 5
deletes anything.
- An embark becomes `WorldRequest::StartAdventure(populated_site)` (helper `start_request(site)`, plus
  `populated_site(&History) -> SiteId`, the first site with a living member).
- The "B's return is refused `InUse`" probe becomes an assertion on the coordinator's `online_on` / `claim_of`.
- A move to a foreign host (retire to a foreign spawn) becomes `end_adventure` to a foreign spawn.

**Files:**
- Modify: `crates/game/tests/coordinator.rs`, `coordinator_requests.rs`, `peer_schedule.rs`, `history_play.rs`,
  `history.rs`, `world_manager.rs`, `common/multi_rig.rs`.

**Interfaces:**
- Produces, in `crates/game/tests/common/`:
  - `fn populated_site(h: &History) -> SiteId`;
  - `fn start_request(site: SiteId) -> WorldRequest`;
  - the helpers `a_adventuring_on_host_a` (was `a_embarked_on_host_a`), `adventure_foreign` / `end_adventure_foreign`
    (were `embark_foreign` / `retire_foreign`).

- [ ] **Step 1: `coordinator.rs`.** Rewrite these on `start_request`:
  - memo / replay: `request_replay_answers_from_memo`, `replayed_embark_under_delay_founds_once` →
    `replayed_start_under_delay_grants_once`, `memo_keeps_unsettled_answers_past_the_old_cap`,
    `new_boot_attach_drops_the_old_boots_memo`, `settled_watermark_prunes_exactly_below`;
  - settle and hop: `settle_answers_without_executing`, `a_request_from_an_older_placement_is_refused_not_granted`,
    `a_request_at_the_current_hop_is_granted`, `a_succeeded_boots_messages_are_dropped`;
  - transition gating: `nothing_leaves_while_a_write_is_in_flight`, `no_output_is_released_without_its_transition`,
    `no_second_transition_while_one_is_written`, `a_failed_write_keeps_its_messages_and_writes_again`,
    `release_claim_forces_a_save`;
  - projection: `state_round_trips`, `a_core_reloaded_after_every_input_is_identical` (`a_world_life` starts an
    adventure);
  - presence and hand-offs, `InUse` → `online_on`: `player_offline_from_the_old_host_after_online_on_the_new_is_ignored`
    and the rest listed in the handoff survey;
  - claim reconciliation: `host_report_reconciles_claims`, `claim_is_answers_a_differing_report`,
    `claim_is_queued_for_a_host_is_dropped_at_its_attach`, `two_holders_do_not_ping_pong_for_ever`;
  - directives: `notice_and_directive_go_to_the_regions_host`. A founding produced the notice; use a bootstrap NPC
    event, or delete the test if none is reachable and say so in the commit.

  Delete `embark_validates_and_claims_once` and `return_in_use_only_while_online` in Task 5, not here.
- [ ] **Step 2: `coordinator_requests.rs`.** Same mapping:
  - `expired_embark_is_settled_never_executed` → `expired_start_is_settled_never_executed`;
  - `expired_sent_embark_gives_back_its_claim` → `expired_sent_start_gives_back_its_claim`;
  - `embark_reply_after_disconnect_releases_claim` → `start_reply_after_disconnect_releases_claim`;
  - `host_sends_settled_as_requests_complete` uses start, end, start;
  - also `sent_request_replayed_after_reconnect_answers_once`, `requests_queue_while_link_down_and_replay_once`,
    `reconnect_between_looks_reattaches`, `request_times_out_with_world_unavailable`,
    `host_stamps_requests_with_the_players_hop`, `coordinator_claim_older_than_the_host_never_locks_out`,
    `refused_grant_after_a_claim_is_leaves_no_claim_on_the_host`,
    `ignored_stale_claim_is_triggers_a_re_report_and_converges`.
- [ ] **Step 3: `peer_schedule.rs`.**
  - The move trigger becomes start / end adventure for: `cut_mid_move_holds_the_player_once`,
    `player_with_a_pending_world_request_is_clamped_back_until_it_is_answered`,
    `move_to_unreachable_host_is_refused_before_parking`, `observer_disconnect_parks_nothing`,
    `move_into_an_unavailable_region_parks_and_tells_the_sender`, the four `move_queued_link_down` tests, and
    `cut_with_a_foreign_record_commits` (via `end_adventure_foreign`).
  - `claim_travels_with_the_player_across_hosts`, `claim_cannot_be_taken_while_its_holder_crosses_hosts`,
    `handoff_parked_on_arrival_releases_the_online_state`: an `Adventure` claim travels; the rival probe becomes
    `online_on`.
  - `old_claim_is_after_a_round_trip_is_not_applied` uses `Claim::Adventure`.
- [ ] **Step 4: `history_play.rs`, `history.rs`, `world_manager.rs`.**
  - `notices_only_to_home_region`: two adventurers at two sites.
  - `calendar_and_sites_broadcast_daily`: no embark; drop the `returnable` assertion.
  - `embark_allowed_in_matches_history_with_an_abandoned_site`: abandon the site through the history's NPC path; if
    none is reachable, delete it, since `embark_allowed_in_matches_history` keeps the rule parity.
  - `end_adventure_returns_to_spawn_in_a_body` → `end_adventure_returns_to_spawn_and_frees_the_claim`. It also starts
    a second adventure and expects `AdventureStarted`.
  - `history.rs` `scripted_input`: drop the embark step and the "must embark" assertion.
  - `world_manager.rs` `claim_is_sets_the_host_claim`: an `Adventure` claim restored, dropped, or granted while
    unsettled; refusal `AlreadyPlaying`.
- [ ] **Step 5: Run.**
  ```
  just test-one game "--test it -- coordinator:: coordinator_requests:: history_play:: history:: world_manager::"
  just test-threads
  ```
  Expected: PASS. Then grep: `rg -n 'Embark\(|ReturnTo|Retire\b|embark_request' crates/game/tests`. Only tests that
  Task 5 deletes may still match.
- [ ] **Step 6: Commit:** `test(game): request, settle, hand-off and claim guarantees proven on StartAdventure`.

### Task 5: Re-express the server's guarantee tests on `StartAdventure`

**Files:**
- Modify: `crates/server/tests/roles.rs`, `coordinator_rt.rs`.

**Interfaces:**
- Produces, in `server/tests/` helpers: `start_request`, `start_request_on`, `raw_start` (were `embark_request`,
  `embark_request_on`, `raw_embark`).

- [ ] **Step 1: `roles.rs`.**
  - `embark_during_downtime_completes_once` → `start_during_downtime_completes_once`.
  - `embark_answered_then_crash_is_not_refounded` → `start_answered_then_crash_is_not_granted_twice`.
  - Moved onto `raw_start`: `no_output_leaves_before_its_state_is_written` (asserts `WorldReply::AdventureStarted`),
    `a_restart_resumes_exactly_where_the_last_transition_left`,
    `a_coordinator_killed_after_the_commit_before_the_sends_loses_nothing`, `host_survives_coordinator_absence_at_start`,
    `request_timeout_answers_world_unavailable` (no late grant).
  - `coordinator_crash_mid_play_conserves_people`: start an adventure at a populated site, which keeps its region
    running; `settled` checks that region's population.
  - `two_hosts_attach_and_both_get_their_people`: the populated site's region.
  - `split_world_plays_like_all_in_one`'s `play_script` = connect, start, legends, end, legends.
  - Delete the `player_founded` helpers `player_sites` / `stored_player_sites` and `couple_embark_request`.
- [ ] **Step 2: `coordinator_rt.rs`.** Every `attach_and_embark` becomes `attach_and_start`.
  - `state_survives_restart` restores a saved `Claim::Adventure`.
  - `coordinator_panic_skips_the_exit_cut` asserts `record.adventure.is_none()`.
- [ ] **Step 3: Run** `just test-r2`, then `just test-one server "--features test-hooks --test coordinator_rt"`.
  Expected: PASS.
- [ ] **Step 4: Commit:** `test(server): coordinator crash, downtime and memo guarantees proven on StartAdventure`.

### Task 6: Delete embark, return, retire and the fortress claim; bump the formats

**Files:**
- Modify (game):
  - `protocol.rs:765-910`
  - `lib.rs:296-309`
  - `coordinator/{msg.rs, mod.rs, state.rs}`
  - `world_manager/{mod.rs, world_link.rs, peers.rs, succession.rs:337}`
  - `peer/msg.rs:48` (doc)
  - `history/{play.rs, rules.rs:28-29, chain.rs, records.rs:126, mod.rs:194-243, persist.rs:12-13, grid.rs,
    bootstrap.rs:128, journey.rs:288,379}`
  - `player.rs:146,188`
  - `world_map.rs:8-16`
- Modify (elsewhere):
  - `world-store/src/codec.rs:14` and its tests
  - `svc-link/src/lib.rs:54`
  - `worldgen/examples/world_map.rs:297-310`
  - client: `instance.rs`, `bridge.rs:881-908`, `connections.rs:529,632`, `commander/bounds.rs:63`,
    `world_scale.rs`, `web_worker/sim.rs:63-83`, `history_view.rs` (`fortress`, `fortress_region`,
    `HistoryMsg::Fortress`), `fortress_panel.rs` tests
  - `justfile:26`
- Test:
  - every `fortress: None` / `Some` in `game/tests` and `server/tests` (the survey list: `player.rs:29`,
    `peer_journal.rs:48`, `lod_streaming.rs:298`, `world_manager.rs` ×6, `multi_rig.rs:1844`, `peer_schedule.rs`,
    `threaded_world.rs:702`, `observer.rs:331`, `player_store.rs:30`, `takeover.rs` ×2, `store_world.rs:930`,
    `hosts.rs` ×2, `hosts_rig.rs:388`, `world-store/tests/handle.rs:238`, `codec.rs:198-232`)
  - `observer.rs:636-637,755-780`, `webtransport_handshake.rs:131-134`
  - the Task 4 leftovers, and `history.rs:1036`

**Interfaces:**
- Produces:
  - `enum Claim { Adventure(SiteId) }` with `site()`;
  - `WorldRequest { StartAdventure, Legends, PersonInfo }`;
  - `WorldReply { AdventureStarted, AdventureRefused, Legends, PersonInfo }`;
  - `WorldManager::player_for(id) -> Option<(PlayerId, Option<SiteId>)>`;
  - `sites_key: Vec<(SiteId, bool, u32)>`;
  - `EmbarkRefusal { OffMap, NotHabitable, Occupied, TooClose { site } }`;
  - `COORDINATOR_STATE_VERSION = 4`, `HISTORY_FILE_VERSION = 5`, `OLDEST_LOADABLE_VERSION = 5`,
    `FORMAT_PLAYER_RECORD = 2`, `LINK_PROTOCOL_VERSION = 8`.

- [ ] **Step 1: Format tests first.**
  - `history.rs` `a_v3_file_is_refused` → `a_v4_file_is_refused` (writes version 4 and expects `found: 4`).
  - `coordinator.rs` `state_round_trips` asserts format byte 4, and that a byte-3 core decodes to `OldVersion`.
  - `world-store/tests/codec.rs` `player()` drops `fortress`.

  Run them: expected FAIL (compile).
- [ ] **Step 2: Delete, game side.**
  - Protocol:
    - `ClientPacket::{RequestEmbarkOptions, Embark, Retire, ReturnTo}`;
    - `ServerPacket::{EmbarkOptions, EmbarkResult, Fortress}`;
    - `EmbarkRequest`, `HeldFortress`, `EmbarkOptions`, `CivOption`, `SiteOption`, `PersonOption`;
    - `SiteMarker.returnable`;
    - `ClientUpdateEvent::{EmbarkOptions, EmbarkResult, Fortress}`.
  - Coordinator:
    - `claims`, `set_online`, `embark`, `return_to`, `Claim::Fortress` (from `projection!`, `new_for` and `load`);
    - the `Embark` / `ReturnTo` settle arm;
    - `sites_key`'s 4th field.
  - WorldManager:
    - `fortress_of` (and `host_fields!`), `Parking.fortress`;
    - `send_fortress`, `embark_result`, `embark`, `retire`, `return_to`, the embark `perf_event!`;
    - the fortress arms of `give_back`, `claim_is`, `send_claims`, `restore_player`, `client_disconnected`,
      `client_id_live`, `packet_name`, `defers_until_restored` and the hand-off code in `peers.rs`
      (`claim_of` / `split_claim` / `handoff_record` carry only the adventure).
  - History:
    - `play.rs` `embark_source`, `embark_options`, `player_embark`, `returnable`;
    - `EMBARK_PLAYER_PARTY_MAX`, `Site.player_founded` (and `found_site`'s parameter);
    - `player_embark_allowed(_in)`, `too_steep_to_embark`, `SocietyGrid.too_steep`;
    - the player-only `EmbarkRefusal` variants.
  - Docs:
    - `AdventureRefusal::AlreadyPlaying`'s text loses "retire";
    - the module and fn docs listed in the survey (coordinator `mod.rs:2-3`, `world_link.rs:2-11`, `peers.rs:26-27`,
      `world_manager/mod.rs:1012-1017`).
  - Bump the five constants.
- [ ] **Step 3: Delete, client side.** Remove:
  - `ClientRequest::{RequestEmbarkOptions, Embark, Retire, ReturnTo}` and their dispatch;
  - the three incoming packet arms and `fortress_region`;
  - `desired_window`'s `fortress` / `fortress_radius` parameters, `set_fortress_radius`, `FORTRESS_NEAR_RADIUS`,
    `VOXEL_FORTRESS_RADIUS` (and `world_scale` tests, `web_worker/sim.rs`, the justfile doc line);
  - `desired_window_keeps_fortress_neighbourhood`;
  - `bounds.rs`'s fortress widening;
  - `HistoryView.fortress` / `fortress_region`.

  `connections.rs`' tests route `ClientPacket::PlayRegion` instead.
- [ ] **Step 4: Delete the tests of embark itself.**
  - `history_play.rs`: `embark_*`, `second_embark_too_close`, `one_fortress_per_player`, `retire_*`, `return_*`,
    `fortress_packet_*`, `fortress_claim_survives_restart`, `fortress_and_adventure_exclusive`, and the helpers
    `embark`, `return_to`, `embark_world`, `fortress_in`. Tests that used `embark_world` only for its world take a
    bootstrap world.
  - `coordinator.rs`: `embark_validates_and_claims_once`, `return_in_use_only_while_online`.
  - `coordinator_requests.rs`: `embark_round_trip_moves_player_once`, `return_in_use_across_reconnect`.
  - `peer_schedule.rs`: `embark_into_a_foreign_host_*`, `retire_to_a_foreign_spawn_region`.
  - `observer.rs`: drop the embark packets; the expected packet sequence loses `"Fortress"`.
- [ ] **Step 5: Goldens.**
  - Re-record `world-store/tests/fixtures/player_record.bin` (`MMO_RECORD_GOLDENS=1 just test-one world-store
    "--test it codec::"`).
  - Re-record `wasm_determinism.rs` `HISTORY_DIGEST`: it changes because `Site.player_founded` went. Run
    `just determinism-wasm`, check that only `HISTORY_DIGEST` moved, and update it.
- [ ] **Step 6: Run.**
  ```
  just test-one game "--test it"
  just test-one world-store "--test it"
  just test-server
  just test-r2
  just test-r3
  just test-r4
  just test-threads
  just determinism-wasm
  just smoke
  ```
  Each touched crate's whole non-client suite, once. Expected: all PASS. Then
  `rg -n 'EmbarkRequest|HeldFortress|EmbarkOptions|Claim::Fortress|fortress_of|ReturnTo|player_founded|returnable' crates`
  must return nothing.
- [ ] **Step 7: Commit:** `feat!: embark, return, retire and the fortress claim are gone; coordinator core v4,
  history v5, player record v2, link protocol 8`.

### Task 7: Renames and docs

**Files:**
- Rename:
  - `client/src/session.rs` `Embarked` → `InPlay` and every reader and writer from the survey (`slint_overlay.rs`,
    `frontend/{loading,mod}.rs`, `bridge.rs:1431`, `commander/mod.rs:624`, `debug_launch.rs`, tests incl.
    `skip_menu_starts_embarked_in_world` → `skip_menu_starts_in_play`);
  - `PlayAction::EmbarkHere` → `PlayHere`;
  - `renderer/embark_tabs.rs` → `arrival_tabs.rs` (`EmbarkTab` → `ArrivalTab`, `sync_embark_tabs` →
    `sync_arrival_tabs`);
  - `ui/embark.slint` → `ui/arrival.slint` (`EmbarkPanel` → `ArrivalPanel`, `EmbarkLayout` → `ArrivalLayout`,
    `embark-*` properties → `arrival-*`);
  - `slint-ui/tests/embark.rs` → `arrival.rs` (and `tests/it.rs`);
  - chat `/fortress` → `/leave` (`chat/command.rs`, `chat/run.rs`; check that `/leave` is unused first).
- Modify:
  - comments naming embark: `world-map.slint:71-84`, `loading.slint:1`, `meshing/coarse.rs:6`,
    `autopilot.rs:442`, `instance.rs:1877`, `fortress_panel.rs` tests, the dev_structures docs;
  - `CLAUDE.md` (lines 29, 35-37, 51-52, 264; the link-protocol number 7 → 8 in "Link rules");
  - `TODO.md:1-5,39,148,154,159,164`;
  - `docs/game-design.md` §10 (mark embark removal done, linking this plan).

- [ ] **Step 1: Renames.** Run `just smoke`, `just test-one slint-ui "--test it arrival::"` and `just slint-wasm`.
  Expected: PASS.
- [ ] **Step 2: Docs.** Then `rg -in 'embark' crates/client crates/slint-ui justfile scripts CLAUDE.md`: what remains
  names only NPC embarks or the shading rule `embark_allowed_in`.
- [ ] **Step 3: Commit:** `refactor(client,slint-ui): the start screen's names; docs after the embark removal`.

### Task 8: Hand checks and the closing review

- [ ] **Step 1: Hand checks.**
  - `just run --reset` → first person at a populated site.
  - F9 → Overview: commander camera, designations and fortress HUD reachable. F9 again → in a body.
  - End adventure → start screen → start again.
  - `just run-two` → two players in one region.
  - `just fly-probe` flies (Overview).
  - `just play` → menu → forge → Adventure tab start.
  - With Dawid's go-ahead, `just go-nogo-r3` (its first valid run is a hand check in TODO.md).
- [ ] **Step 2: Update the plan.** Add an "As built" section to this plan listing what departed from it, then commit
  the plan.

## As built (2026-11-17, `e6f1bfd3..1e3094bd`)

Executed subagent-driven: each task had an implementer and a task review, then one whole-branch review and one fix
wave. Every task review came back clean, some after fix rounds.

| Task | Commits |
|---|---|
| 1 | `e66d3995`, `6f3665ec` (player-record golden re-recorded: only the mode discriminant moved), `f2d8b53e` |
| 2 | `75203352` |
| 3 | `1e554e90` |
| 4 | `d6fbd356`, `92c433a5`, `2805586a`, `17fb580b` |
| 5 | `07a38925` |
| 6 | `b3f8d6f6`, `b9aaf938` |
| 7 | `2cc5a773` |
| Final fix wave | `64839627`, `1e3094bd` |

`f2d8b53e` fixed a session-reset UI sync that `rejoin_starts_clean` exposed.

Departures from the plan:
- **Task 1:**
  - Many more test rigs relied on the old Fortress default than listed. They now start in Overview (`Rig::overview()`).
  - The Slint game UI defaults follow the new mode (`adventure: true`, `tools-available: false`).
  - The HUD minimap shows only once a home region is joined.
- **Task 4:**
  - Sub-cases that only existed because a fortress belonged to one player were dropped (listed in the task report;
    the review confirmed each against `player_online` / `adopt_claim`).
  - `old_claim_is_after_a_round_trip_is_not_applied` stayed on embark until Task 6.
- **Task 5:**
  - `roles.rs` `never_twice` now allows a just-died person's body at every moment. A despawn directive always trails
    the death; `settled` still requires the body gone.
  - `crash_mid_play` plays the smallest populated NPC site with a couple. It takes about 60 s per seed.
- **Task 6:**
  - `old_claim_is_after_a_round_trip_is_not_applied` was reworked onto `Claim::Adventure` with a scripted coordinator,
    with a positive control. `RETIRED_SITE` became `RESTORED_SITE`.
  - `HISTORY_DIGEST` was re-recorded (0x4827_CEB2) and the `packet_codec` tags were re-pinned.
  - `b3f8d6f6` swept the owner's uncommitted justfile hunks into the commit. `b9aaf938` took them back out; the net
    change is one doc line.
- **Task 7:**
  - `Embarked` → `InPlay`, `embark.slint` → `arrival.slint` (`ArrivalPanel`), `embark_tabs` → `arrival_tabs`,
    `/fortress` → `/leave`.
  - The dead `hover-blocked` map overlay was deleted.
  - Three doc edits sit only in the owner's uncommitted text and were left uncommitted: CLAUDE.md line 29, TODO.md
    lines 1-5, `docs/game-design.md` §10.
- **Final-review fixes:**
  - An adventure started in the player's own region lands the body with a server-only `GameEventKind::PlaceBody`.
    The default flip had made `SetMode{Adventure, at}` a no-op; client events cannot carry `PlaceBody`.
  - An End adventure the host cannot carry out re-sends `AdventureResult(Ok(site))`, so the client re-enters play.
  - The autopilot re-asks for Overview every 2 s.
  - The Play-button rule is a pure, tested function.

Open, for Dawid:
- **Task 8 hand checks** — none run yet:
  - `just run --reset`;
  - F9 → Overview and back;
  - end the adventure, then start again;
  - `just run-two`;
  - `just fly-probe`;
  - `just play`;
  - `go-nogo-r3`.
- **F9 / Overview is ungated.** Any client can reach flight and creative edits through it, as Fortress allowed
  before. Gate it (a dev feature, or the server refusing `SetMode{Overview}` for non-admins) before an online release.
- **Deferred test-strength follow-ups** (ledgered):
  - `roles.rs`: births re-applied under a new id, a late grant given back, a short-lived body for the dead;
  - `crash_mid_play`'s timing margin;
  - pinning `PlaceBody` in `client_cannot_spoof_server_events`;
  - three `lod::tests` that fail only in a whole-lib run (predates this work).
