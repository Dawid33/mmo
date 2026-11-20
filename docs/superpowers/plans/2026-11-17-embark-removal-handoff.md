# Handoff — removing embark / return

Date: 2026-11-17. For the context that **plans and then carries out** the removal of the embark / return flow.
Decided by Dawid (`docs/game-design.md` §10, revision 2026-11-16): embark and return go, and so do the separate
disembodied fortress mode and the `embarked` debug presets. A player now plays one first-person character, and
land will later be claimed as a **holding** (a region), not embarked on. Removals go before new features that
would otherwise be built around them. The quest integration reworks the same coordinator code, so this goes
first.

**First deliverable:** a plan in `docs/superpowers/plans/` (superpowers:writing-plans), agreed with Dawid,
before anything is deleted. Then execute it.

## Decided (Dawid)

- **Goes:**
  - embark and return, and everything only they use: choosing a site and party, the embark screen, the
    coordinator's embark/return requests, the `embarked` debug presets and their recipes;
  - fortress mode as a separate disembodied mode, and the "spirit" framing.
- **Stays:** the **commander camera**, which becomes the mayor's optional top-down view (game-design §5).
- **People join a settlement over time or are born there**; nobody arrives by embark.
- **No data migration** (Dawid's standing rule): worlds and world databases are wiped and re-forged. Format
  bumps that refuse old rows are fine, and decode fallbacks are not needed. Ask before assuming the same for
  login accounts. `PlayerRecord` is a world-store blob, not a login row.

## Where embark lives (surveyed 2026-11-17; verify, line numbers drift)

Rough size: `game` 26 files (~840 matches), `client` 16 (~640), `server` 7 (~180), `slint-ui` 10 (~120). None
in `world-store` SQL, `login` or `worldgen` code.

- **Protocol** (`game/src/protocol.rs`):
  - `ClientPacket::{RequestEmbarkOptions, Embark, Retire, ReturnTo}`;
  - `ServerPacket::{EmbarkOptions, EmbarkResult, Fortress}`;
  - `EmbarkRequest`, `HeldFortress`, `EmbarkOptions` and its options types, `SiteMarker.returnable`;
  - `ClientUpdateEvent` variants (`game/src/lib.rs:296-309`).

  `CivMarker` is shared with the Adventure and Legends tabs, so keep it.
- **Coordinator** (`game/src/coordinator/{mod.rs,msg.rs}`):
  - `WorldRequest::{EmbarkOptions, Embark, ReturnTo}`;
  - `WorldReply::{EmbarkOptions, Embarked, EmbarkRefused, Returned}`;
  - `Claim::Fortress`;
  - the durable `claims` map (fortress-only);
  - `fn embark` / `fn return_to`.

  `player_claims` and `memo` are **in the `projection!`** (Elastic 2a). Changing them changes the coordinator
  row format: bump it, and an old row is refused (re-forge).
- **History** (`game/src/history/`):
  - **Player-only (goes):** `play.rs` (`embark_options`, `player_embark`, `returnable`), `rules.rs`
    `EMBARK_PLAYER_PARTY_MAX`, `Site.player_founded`, and the player-only variants of `chain.rs`
    `EmbarkRefusal`.
  - **NPC-side (stays):** NPC embarks — `Purpose::Embark`, `send_embark_party`, `arrive_embark`, the
    handover, `found_site`, and `EmbarkRefusal`'s shared site-legality variants (`OffMap`, `NotHabitable`,
    `Occupied`, `TooClose`).
  - Split `EmbarkRefusal` into the shared site rule and nothing else.
- **WorldManager / server:**
  - `world_manager/world_link.rs`: `send_fortress`, `embark`, `retire`, `return_to`, reply handling, claim
    restore and reconciliation. `end_adventure` reuses the retire path.
  - `world_manager/mod.rs` (`fortress_of`, packet routing), `peers.rs` (`claim_of` / `split_claim`,
    `fortress_of` in hand-offs and parking), `succession.rs` (destructures `fortress_of`; `host_fields!`
    must stay complete).
  - **`server/src/dev_structures.rs` + `server/src/lib.rs`:** the `home=` / `town=` dev sources announce the
    embark region. They need a new anchor (the spawn region, or an explicit region), not just deletion.
- **Store:** no SQL. `PlayerRecord.fortress` (`game/src/player.rs:146`) sits in the `FORMAT_PLAYER_RECORD`
  blob (`world-store/src/rows.rs`), so bump the format; `world-store/tests/codec.rs` sets it.
- **Client:**
  - `debug_launch.rs`: the `embarked` preset, auto-embark systems, `VOXEL_DEBUG_EMBARK_AT`, `online`
    embarks, and `--adventure` retires first.
  - `session.rs`: `Embarked(bool)` also covers `PlayRegion`, so **rename, don't delete**.
  - `instance.rs`: the requests, and the fortress region feeding the neighbourhood centre.
  - `renderer/history_view.rs`: embark selection and rows; `EmbarkTab::Fortress` goes, Adventure and Legends
    stay.
  - `renderer/embark_tabs.rs`: mostly Adventure and Legends, so keep or rename.
  - `renderer/slint_overlay.rs`: about 200 lines of embark panel, Return and Retire wiring.
  - `bridge.rs`, `connections.rs`.
- **slint-ui:**
  - `ui/embark.slint`: `FortressTab` goes; `AdventureTab`, `LegendsTab` and the panel shell stay (rename).
  - `ui/game.slint`: the `embark-*` and `retire` properties.
  - `tests/embark.rs`, registered in `tests/it.rs`.
  - **Not embark:** `fortress-panel.slint` / `fortress_panel.rs` (the Governance / Designations / People
    HUD) stay as mayor tools.
- **Recipes and docs:**
  - `just run`, `town`, `run-two` (greps `embarked: fortress site=N`), `run-release`, `run-online` (via
    `client debug online`), `go-nogo-r3`, `play` (menu → map → embark), and `scripts/run.sh`.
  - **Keep the justfile in sync** (CLAUDE.md); update CLAUDE.md's Building / Debug launches lines and
    `TODO.md`.

## Decisions the plan must take (ask Dawid where marked)

1. **What `just run` starts in.** It must still go straight into a world. The obvious replacement is a
   first-person character at a spawn — the adventure path without embarking first. `run-two` becomes two
   players in one region. **Ask.**
2. **`PlayMode`** is `{ Fortress (default), Adventure }`, and `EndAdventure` sets `Fortress`. Without a
   separate fortress mode the default becomes "in a body". Either collapse `PlayMode` or keep a mayor-view
   flag for the commander camera. **Ask.** The commander camera, designations, orders and the fortress HUD
   must stay reachable (a debug toggle until holdings exist).
3. **Claims:**
   - **Option A — delete:** remove `Claim::Fortress` and `claims` now, and design holdings fresh. A holding
     is per region; a fortress claim was per site.
   - **Option B — repurpose:** keep the claim machinery for holdings.

   Recommendation: A. **Ask.**
4. **Tests that use embark to prove other guarantees — rewrite, don't delete.** They use embark as the
   example of a state-changing request:
   - the request memo;
   - `Settle` / crash / replay;
   - downtime;
   - hand-off claim semantics.

   Where they live:
   - `server/tests/roles.rs`: `embark_during_downtime_completes_once`, `embark_answered_then_crash_is_not_refounded`,
     `raw_embark`;
   - `game/tests/coordinator.rs`, `coordinator_requests.rs`, `peer_schedule.rs`.

   Re-express them on `StartAdventure` / `EndAdventure` (or another durable request) so the guarantees stay
   covered.
5. **Goldens:** `game/tests/wasm_determinism.rs` `history_scene()` pins a crc32 of the encoded history.
   Dropping `Site.player_founded` or serialized refusal variants changes it, so re-record on purpose.
   `scene_hash_golden` has no embark ties.

## Rules that bind

- Plan first; **commit only your own paths** — other sessions stage in the shared index, so use
  `git diff --cached --name-only` and a pathspec.
- **Work on `develop`, no worktrees.**
- **Check for concurrent work in `game/src/coordinator/`** before starting:
  - Elastic 2a just reshaped the coordinator (`projection!`, write-before-send);
  - Elastic 2 handoffs are open (`docs/superpowers/plans/2026-11-16-elastic-2*`).
- **Classification macros must stay complete:** `projection!`, `host_fields!`, `carry_components` /
  `Client::arrive`. Removing a field means removing it from its classification.
- **Tests** (CLAUDE.md "Testing"):
  - `just test-one` for touched tests;
  - each touched crate's whole non-client suite once (a wide change);
  - `just test-r2` / `test-r3` / `test-r4` for the coordinator, roles and hand-offs;
  - `just test-threads` (WorldManager, peers);
  - `just determinism-wasm`;
  - `just smoke`;
  - slint-ui tests;
  - client tests only with Dawid's OK;
  - `just test-full` only if he asks.
- **Reading:** `docs/game-design.md` §5, §6, §10; CLAUDE.md "Role split", "The coordinator's state is in
  the store", "Many hosts"; `docs/superpowers/plans/2026-11-16-elastic-2a-coordinator-state-in-the-store.md`
  ("As built").
