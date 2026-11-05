# Adventure Survival: First Person, Hotbar, Inventory, Items, Chests — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Work on `develop` in the main checkout — **no worktrees** (CLAUDE.md).

**Goal:** Adventure mode plays like Minecraft survival in first person — hold-to-mine with break times, no flight, a 9-slot hotbar inside a 36-slot inventory, dropped items that lie on the ground and bunch together, chests, and items yielded by digging and by chopping trees as a player.

**Architecture:** Everything gameplay-relevant is rollback sim state in `crates/game` (predicted by the client, authoritative on the server, exactly undoable). **Dropped items are not entities**: they are *piles* in a per-region `BTreeMap<BlockPos, ItemPile>` keyed by the block cell they rest in, with no rapier body, settled by an integer column walk at deposit time; "bunching" is merge-on-deposit into a pile within one block, so the number of drawable things is bounded by occupied cells, and a resting pile costs nothing per tick. Chests are a second per-region map, `BTreeMap<BlockPos, Container>` (Minecraft "block entity" / Luanti node-inventory pattern). The client draws piles as instanced little cubes and the inventory as a Slint screen.

**Tech Stack:** Rust, the `#[rollback]` undo layer (`#[undo(map)]` `BTreeMap`s, `GameDataUpdate` emits), rapier only for the existing felled-tree voxel body, Bevy (pile rendering), Slint (`crates/slint-ui`, hotbar + inventory screens).

**Spec:** this document's *Research & Design* section (no separate spec; research done 2026-11-05 — sources below).

---

## Research & Design (the decisions this plan argues from)

### What exists today (verified 2026-11-05)

- `state.rs:115-162`: `ItemKind {Empty, Dirt, Stone}`, `ItemStack {kind, count: u32}`, `Inventory {slots: Vec<Option<ItemStack>>}` (`hotbar()` 9 empty, `seeded()` Dirt×42 + Stone×7) and `Client {input, fps_cam_mode, mode, inventory, inventory_open}` — the inventory already rides `EntityBundle` across seams. No selected slot.
- `ui.rs`: `IconId::of(ItemKind)`, `UiAction {MoveItem{from,to}, ToggleInventory}` (plain swap, handled `region.rs:715-728`), `HotbarView`/`build_hotbar`, `UiUpdate::Hotbar` — the client ignores it (`slint_overlay.rs:552`). The old HUD hotbar strip lives in git: `git show 087692a -- crates/slint-ui/ui/hud.slint`.
- `edit.rs`: breaking is **instant** on click (`region.rs:603-625` → `player_edit_safe`, `edit.rs:610`), place always puts `PLACE_BLOCK = BlockId(1)` (dirt); no hardness anywhere; nothing touches the inventory.
- `movement.rs:144-153`: double-tap Space toggles flight for any keyboard-driven body — no creative/survival distinction.
- Sim `E` (`region.rs:631-645`) toggles `fps_cam_mode`; the client filters E out in first person (`input.rs:137-145`).
- Trees: `trees::fell` + `chop_safe` (`orders.rs:459`) → `detach_blocks_pivoting_safe(.., SettlePolicy::Despawn, ..)`; `SettlePolicy` has one variant, documented "felled trees: no items" (`voxel_body.rs:139`).
- No chest/item-drop/block-entity types. `Block.data` (24 bits) is taken by tree id + limb. Region saves are lz4 bincode of the whole `Rollback` (`region_store.rs`, `REGION_FILE_VERSION = 1`), so new `GameData` fields persist automatically.
- `TICK_RATE = 50` ms → 20 Hz, the same as Minecraft, so Minecraft tick constants carry over 1:1.
- Entities: one rapier body each; `remove_entity_safe` (`state.rs:1094`) snapshots the whole physics state — fine for rare despawns, ruinous for item churn. There is no spatial entity index.

### What other games do (research, 2026-11-05)

- **Minecraft** item entities ([wiki: Item (entity)](https://minecraft.wiki/w/Item_(entity))): full entities with gravity/drag; merge when within 0.5 × 0.25 × 0.5 blocks of another of the same item (not exceeding max stack), checked every 40 ticks, or every 2 ticks while moving across blocks; despawn after 6000 ticks; pickup delay 10 ticks (40 when thrown by a player); pickup box = player box grown 1 block horizontally, 0.5 vertically; drawn as 1–5 models by count, bob + spin client-side. Item lag comes from every item doing physics + a neighbour query per tick and being network-tracked; Spigot raises `merge-radius.item` to 2.5 ([SPIGOT-5779](https://hub.spigotmc.org/jira/browse/SPIGOT-5779)); Paper has an option to stop merging through walls.
- **Factorio**: item-on-ground is one entity per *single item* with no physics — 87k of them on a pad cost ~100 ms/tick ([forum 134798](https://forums.factorio.com/134798)). One entity per item does not scale, even without physics.
- **Terraria**: soft cap of 400 dropped items; the oldest despawn first ([wiki](https://terraria.wiki.gg/wiki/Items)).
- **Dwarf Fortress**: items are objects on a tile with no physics; a tile holds any number of them ([stockpile design](https://dwarffortresswiki.org/index.php/Stockpile_design)).
- **Valheim / Palworld**: persistent physics drops degrade servers; mods that merge on spawn report big gains from fewer physics bodies.
- **Block inventories**: Minecraft *block entities* are a per-chunk list keyed by x/y/z, separate from the voxel array ([Block entity](https://minecraft.wiki/w/Block_entity)); Vintage Story uses `BlockEntity`; Luanti uses node metadata with named inventory lists ([api](https://api.luanti.org/metadata/)). A broken chest spills its contents (Luanti mods sometimes refuse the dig instead).
- **Break time** ([Breaking](https://minecraft.wiki/w/Breaking)): damage per tick = speed / hardness / 30 with the correct tool (/100 without); ticks = ceil(1 / damage); a 6-tick delay before the next block. Hands are the correct tool for dirt and logs, so dirt (0.5) takes 15 ticks and a log (2.0) takes 60 ticks (3 s).
- **Felling**: vanilla Minecraft breaks one log at a time. Tree-capitator mods and Vintage Story's axe fell the whole tree and yield about one log per log block; Vintage Story felling takes longer for hardwoods and gives few sticks or seeds ([VS Axe](https://wiki.vintagestory.at/index.php/Axe/en)).

### Decisions

1. **Dropped items are cell-keyed piles, never entities, never rapier bodies.** `GameData.item_piles: #[undo(map)] BTreeMap<BlockPos, ItemPile>`. This takes the DF and research recommendation and goes further than Minecraft: its per-tick merge query is replaced by **merge on deposit**. A deposit first looks for a pile with room for that item within `PILE_MERGE_RADIUS_BLOCKS` (1: dx, dz ∈ −1..=1, dy ∈ −1..=1), visited in a fixed order (nearest first, ties broken by key), and only then creates a pile at its own rest cell. A tree's 200 logs therefore become ≈ 4 piles of 64, not 200 objects.
2. **Settling is an integer column walk**, done at deposit and whenever a pile's support changes: from the requested cell, move up out of solid blocks and then down while the cell below is not solid (`MAX_PILE_FALL_BLOCKS` 64; implied ground counts as solid). There is no ballistic sim phase, so floats never enter the pile's state. Thrown items resolve their landing cell at once, and the client may animate the arc (follow-up). When a block is removed, the pile directly above re-settles and merges where it lands; when a block is placed into a pile's cell, the pile is pushed up.
3. **Lifetime** follows Minecraft: `PILE_DESPAWN_TICKS = 6000`; a merge keeps the later expiry; despawn uses an index `pile_expiry: BTreeMap<(u64, BlockPos), ()>`, so only expiring piles are touched; `PILES_PER_REGION_MAX = 4096` evicts the earliest-expiring pile first (Terraria's rule). `PICKUP_DELAY_TICKS = 10` and `THROWN_PICKUP_DELAY_TICKS = 40`; a merge keeps the larger `pickup_after_tick`.
4. **Pickup**: each tick, for each Adventure client body, check the cells of its box grown by `PICKUP_REACH_H_BLOCKS` (1) horizontally and `PICKUP_REACH_V_BLOCKS` (1; the research gives 0.5, rounded up to whole cells) vertically, in key order. Each pile moves into the inventory as far as there is room (`Inventory::add`); the remainder stays.
5. **Items** become `ItemId(u16)`, replacing `ItemKind`: ids below `ITEM_BLOCK_LIMIT` (1024) are the block with that `BlockId` (block items); ids from 1024 up are non-block items from one const table in `items.rs` (`stick` first). `MAX_STACK = 64`.
6. **Block data** gains two optional fields in `blocks.ron` / `BlockDef`: `hardness: <centi>` (u16, default `DEFAULT_HARDNESS_CENTI` 100) and `drop: Same | Nothing | Item("<name>")` (default `Same`). Initial values: dirt 50, grass 60 → `Item("dirt")`, sand 50, stone/sandstone/shale/limestone/conglomerate 150, basalt/granite 200, `*_log` 200, `*_branch` 100 → `Item("stick")`, `*_leaves` 20 → `Nothing`, glacier ice 50 → `Nothing`, bedrock and water stay unbreakable (existing rules).
7. **Mining** is sim state on `Client`: `mining: Option<Mining {target: BlockPos, started_tick: u64, needed_ticks: u32}>`. While left is held and the eye ray hits the same block, progress builds; a new target restarts it; release cancels. `needed_ticks = ceil(hardness_centi * HAND_TICKS_PER_HARDNESS / 100)` with `HAND_TICKS_PER_HARDNESS = 30` (Minecraft's correct-tool hand). A trunk cut (`BlockProps::is_trunk`) multiplies that by `FELL_TICKS_FACTOR = 2`. After a break, `BREAK_COOLDOWN_TICKS = 6`. **Tools and crafting are out of scope**: everything mines with the hand rule, and every block drops.
8. **Survival = Adventure mode.** A client whose `mode == Adventure` cannot fly (`MoveParams::allow_flight`), mines with break times, places only the selected hotbar block (consuming it), and has E toggle the inventory instead of `fps_cam_mode`. Fortress keeps today's instant creative edit. Entering Adventure switches the client to first person automatically; F5 still returns to the overhead view.
9. **Chopping as a player**: when an Adventure player finishes mining any tree block (`Block::tree_id() != 0`), it runs the existing `fell` path (the same one `chop_safe` uses). The cut blocks yield at their own cells right away. The falling part becomes a voxel body with the new `SettlePolicy::DropItems`: when it settles (rest, timeout, or leaving the region), each of its model blocks is turned into its drop and deposited at that block's final world cell, clamped into the region. Merging bunches them up. Yield is decided at settle, not at the cut, so the logs lie where the tree landed. This is deterministic because the voxel body is rollback physics.
10. **Chests**: a `chest` block (appended id), solid. `GameData.containers: #[undo(map)] BTreeMap<BlockPos, Container>`, with `CHEST_SLOTS = 27`. Placing a chest creates its container; right-clicking a chest opens it (`Client.open_container`) instead of placing; it closes when the player is farther than `CONTAINER_REACH_BLOCKS` (6) or the block stops being a chest. Breaking a chest deposits its contents and the chest item into piles at its cell (spill, merging). There is no crafting, so each new client starts with `STARTING_CHESTS = 2` chests.
11. **Inventory interaction is Minecraft's cursor model**, as pure functions: `Client.cursor: Option<ItemStack>`; `UiAction::Click {slot: SlotRef, button: ClickButton, shift: bool}`, where `SlotRef = Player(u8) | Container(u8)`. Left click picks up, places, merges or swaps; right click takes half or places one; shift moves the stack to the other area (hotbar ↔ main, or container ↔ player). Closing the inventory with an item on the cursor returns it to the inventory, or drops it as a pile at the player's feet. `Q` throws one of the selected item (Ctrl+Q the whole stack) `THROW_DISTANCE_BLOCKS` (2) ahead along the look direction, raycast-clamped, then settled.
12. **Client view**: pile changes are emitted as `GameDataUpdateKind::SetItemPiles(Vec<(BlockPos, Option<PileView>)>)`, in both the Do and Undo directions, exactly like `SetDesignations` (`orders.rs:483-516`). The client draws each pile as `min(4, ceil(count / 16))` small cubes (`ITEM_CUBE_SIZE_M` 0.25) per stack, bobbing and spinning client-side only, in the pile's region-local cell shifted to world space at the render boundary. Containers and the inventory are read from the predicted `GameData` through `HomeView` (the client has it).

13. **Players have stable identities** (user decision, 2026-11-05: "players need stable identities which will in future be served by an external database"). Today a `ClientId` is a per-process connection counter (`server/src/lib.rs:312`, restarting at 0). Everything about a player hangs off it, so a reconnect gets a fresh body and the old one is orphaned (`client_disconnected`, `world_manager.rs:1168`, leaves the body in its home region forever). A restart deletes every player (`restore_saved`, `region_runner.rs:115`). The fix:
    - **Tokens.** `ClientId` stays the *session* (transport, packets, sim keys). A new `PlayerId(u64)` is the *identity*, resolved from a client-held secret `PlayerToken` (128 random bits, hex). The native client keeps the token in `$XDG_DATA_HOME/mmo/player_token` (fallback `~/.local/share/mmo/player_token`; `VOXEL_PLAYER_TOKEN_FILE` overrides, so `just run-two`'s second client gets its own). The browser keeps it in `localStorage["mmo.player_token"]`. In-process harnesses make one per client.
    - **Handshake.** The first packet of every connection is `ClientPacket::Hello { token }`. The netcode edge resolves it *before* announcing the session, through a server-side `PlayerStore` trait. That is the async edge where a future external database (Postgres, as in `docs/superpowers/specs/2026-09-20-multi-instance-…`) slots in without touching the `WorldManager`, which stays I/O-free. The file implementation now: `<world>.players/` (forged worlds), with an in-memory store for procedural worlds and tests.
    - **Records.** A `PlayerRecord` holds: the `Client` (input state reset), the body pose, the home region, and the fortress / adventure claims.
    - **Disconnect and reconnect.** A disconnect *parks* the player: the body is extracted (no orphan) and the record goes to the store. A reconnect with the same token restores the body where it stood, with its inventory, mode and claims.
    - **Autosave.** It also extracts the records of online players from each region's `Saved` state, so a crash loses at most the autosave interval. `restore_saved` keeps stripping session bodies, which is now safe because their records live in the store.
    - **No authentication yet**: the token is a bearer secret. That's fine for single-player and the dev deploy; the external account service replaces the resolve step later.
14. **People's positions are saved with the autosave**: each `Saved` state's `people_here()` updates the history, as `PeopleParked` does at park time. A restart then respawns people where the save had them, not where they were last parked.

### Known limits accepted (follow-ups, not in this plan)

Tools, crafting, health, hunger and fall damage; authentication for player tokens (comes with the external database); a people body's in-flight sim state beyond position (a walk in progress, animation) on park/restart; piles crossing region seams (deposits clamp into the region, like `TIP_EDGE_MARGIN_BLOCKS`); fortress dig orders yielding items (one call to `deposit_items_safe` in `clear_tiles_safe` later); a thrown-item arc animation; a block-crack overlay (this plan shows progress on the crosshair).

## Global Constraints

- `game` and `server` stay Bevy- and Slint-free.
- All new sim state lives in `#[undo(map)]` `BTreeMap`s or on `Client`. Never use `HashMap`, never use f32 in pile or container state, never use wall time; randomness only through `slime::roll`. `StateHash` is derived on every new state type.
- Every mutator is a `*_safe` method that undoes exactly: `hash(before) == hash(after undo)`. Pile and container edits emit their render updates in both directions.
- Tunables are named `pub const`s in the module that owns them (the names in Decisions above, with these values).
- Adding `GameData` fields changes the serialized state: re-record `scene_hash_golden` and the `wasm_determinism` goldens **on purpose** in the task that adds the field (precedent 087692a), and bump `REGION_FILE_VERSION` to 2 in `region_store.rs` (old region files are then set aside as `.bak` and regenerated; say so in the commit).
- `blocks.ron` ids are append-only.
- Verification is `just test-one <crate> "<target> <filter>"` and `just smoke`. Never run whole suites. Tag any test slower than 100 ms `#[ignore = "slow: >100ms — run via just test-full"]`. New game test files are modules of `crates/game/tests/it.rs`.
- Commit after each task on `develop`, ending the message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. The tree carries the user's uncommitted work: stage only the files you touched.

## Review Focus

1. **Rollback of a pickup or merge**: a client mispredicts a pickup (the server gave the pile to another player), and after reconcile both the pile and the inventory are exactly the server's. Test: Task 7 `pickup_undo_restores_pile_and_inventory` plus a two-client race in Task 7.
2. **Pile on a block that gets broken or built over**: a pile never floats or hides inside a solid block. Tests in Task 7: `pile_falls_when_support_broken`, `pile_pushed_up_by_placed_block`.
3. **Deposits at the region edge or into bedrock or water**: they land on standable ground inside the region and are never lost silently. Test in Task 7: `deposit_outside_region_clamps_inside`, plus a water-column case (piles rest on the floor of water; water is not solid).
4. **A full inventory walking over piles**: nothing is duplicated or lost; the remainder stays in the pile. Test in Task 7: `pickup_full_inventory_keeps_remainder`.
5. **Breaking an open chest while another client has it open**: the contents spill exactly once and the open view closes. Test in Task 10: `break_open_chest_spills_once_and_closes`.
6. **The same token connects twice at once** (two windows): the second session is refused with a clear reason and never gets a duplicate body. Test in Task 2: `second_session_same_player_refused`.
7. **A player whose home region was parked, or whose saved position is now inside a block someone built**, reconnects: they land on the nearest standable cell, never inside terrain. Test in Task 2: `restore_into_built_over_cell_lands_standable`.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/game/src/items.rs` (new) | `ItemId`, extra-item table, `MAX_STACK`, `drop_of(Block) -> Option<ItemStack>`, block↔item mapping |
| `crates/game/src/inventory.rs` (new) | `Inventory` (moved from `state.rs`), `add`, `take_selected`, cursor `click`, `SlotRef`, `ClickButton` |
| `crates/game/src/item_piles.rs` (new) | `ItemPile`, `PileView`, settle walk, `deposit_items_safe`, `take_pile_safe`, pickup, expiry, support hooks |
| `crates/game/src/mining.rs` (new) | `Mining`, break-tick formula, `tick_mining_safe`, survival place/throw |
| `crates/game/src/containers.rs` (new) | `Container`, chest open/close/spill |
| `crates/game/src/{state.rs,region.rs,edit.rs,movement.rs,camera.rs,voxel_body.rs,orders.rs,ui.rs,input.rs,registry.rs,block_props.rs}` | wiring |
| `assets/blocks/blocks.ron`, `tools/textures/gen.py`, `assets/blocks/chest*.png`, `assets/items/stick.png` | data + art |
| `crates/client/src/renderer/item_piles.rs` (new) | pile rendering |
| `crates/client/src/renderer/item_icons.rs` (new) | `ItemId` → `slint::Image` from embedded textures |
| `crates/slint-ui/ui/hud.slint`, `crates/slint-ui/ui/inventory.slint` (new), `slint_overlay.rs`, `commander/adventure.rs`, `renderer/input.rs` | HUD hotbar, inventory/chest screen, input wiring |

---

### Task 1: Player identity — token, handshake, `PlayerStore`

**Files:**
- Create: `crates/game/src/player.rs` (`PlayerId`, `PlayerToken`, `PlayerRecord`), `crates/server/src/player_store.rs` (`PlayerStore` trait, `FilePlayerStore`, `MemoryPlayerStore`), `crates/client/src/player_token.rs` (load-or-create; native file, wasm `localStorage`)
- Modify: `game/src/protocol.rs` (`ClientPacket::Hello { token: PlayerToken }`; `ServerPacket::Refused(String)` if absent), `server/src/lib.rs` + `server/src/webtransport.rs` (read the first frame; anything other than `Hello` closes the connection; resolve through the store; only then `ServerEvent::ClientConnected { client, player: PlayerId, record: Option<PlayerRecord> }`), `world_manager.rs` (`player_of: BTreeMap<ClientId, PlayerId>`), client `netcode.rs` / `netcode_web.rs` / `local_server.rs` (send `Hello` first), `justfile` `run-two` (client 2 gets `VOXEL_PLAYER_TOKEN_FILE=target/run-two/player2_token`)
- Test: `crates/server/tests/player_store.rs`, `crates/game/tests/player.rs`, `just test-one client "--lib player_token::"`

**Interfaces — Produces:**
- `pub struct PlayerId(pub u64)` (`Copy, Ord, StateHash, serde`); `pub struct PlayerToken(pub String)` with `PlayerToken::generate(rng_bytes: [u8; 16]) -> Self` (32 hex chars) and `is_valid(&self) -> bool` (32 lowercase hex). `Debug` prints only the first 6 chars.
- `pub struct PlayerRecord { pub client: Client, pub pose: Pose, pub region: RegionCoords, pub fortress: Option<SiteId>, pub adventure: Option<SiteId> }` (serde + `StateHash`)
- `pub trait PlayerStore: Send { fn resolve(&mut self, token: &PlayerToken) -> io::Result<(PlayerId, Option<PlayerRecord>)>; fn store(&mut self, records: Vec<(PlayerId, PlayerRecord)>) -> io::Result<()>; }`. `resolve` creates a new id for an unknown token. File layout: `<world>.players/index.ron` (token → id, atomic write) plus `p.<id>.player` (lz4 bincode with a version + crc header, written as temp + rename like `region_store.rs`).
- `pub fn load_or_create_token() -> PlayerToken` (client)

- [ ] **Step 1: Write failing tests**:
  - `player_store::resolve_same_token_same_id_across_reopen` (FilePlayerStore in a temp dir: resolve, drop, reopen → same id)
  - `player_store::store_then_resolve_returns_record`
  - `player_store::corrupt_record_is_set_aside` (renamed `.bak`, resolve → `None` record, logged)
  - `player::token_validates_and_debug_redacts`
  - `player_token::creates_once_then_reuses` (native, temp path via the env override)
  - A server ingress test `first_packet_not_hello_closes` (extend `crates/server/tests/` next to `webtransport_handshake.rs`)
- [ ] **Step 2:** `just test-one server "--test player_store"` (give it its own `[[test]]` if it touches the filesystem globally; otherwise use the server's `it` binary) → FAIL.
- [ ] **Step 3: Implement.** The server's main loop holds the store: `FilePlayerStore` beside a forged world, `MemoryPlayerStore` otherwise. Resolution runs on the tokio edge through an `Arc<Mutex<dyn PlayerStore>>` inside `spawn_blocking` (a DB impl will be async there).
- [ ] **Step 4:** the tests → PASS; `just smoke` green; `just test-one server "--test webtransport_handshake"` → PASS.
- [ ] **Step 5: Commit** `feat: stable player identities (token handshake, PlayerStore)`

### Task 2: Park and restore players across disconnects and restarts

**Files:**
- Modify: `world_manager.rs` (`client_disconnected` sends `GameEventKind::ParkPlayer(client)` to the home region and drops `homes` / claims after recording them; on `ClientConnected` with a record: home = record.region, then `GameEventKind::RestorePlayer(client, Box<PlayerRecord>)` instead of `CreateClientAt`, and `fortress_of` / `adventurer_of` restored from it; a second session for a `PlayerId` that is already online is refused with `ServerPacket::Refused("already playing")`; `take_player_saves() -> Vec<(PlayerId, PlayerRecord)>`), `region.rs` (`ParkPlayer` extracts the body like `ClientLeft` and outputs `RegionOutput::PlayerParked(ClientId, PlayerRecord)` (the manager adds the claims); `RestorePlayer` inserts the `Client` and creates the body at `record.pose`, lifted to the nearest standable cell if the cell is solid (`pathfind::nearest_standable`)), `region_runner.rs` (`fn players_in(&Rollback) -> Vec<(ClientId, PlayerRecord-without-claims)>` used on `Saved`), `server/src/lib.rs` main loop (forward `take_player_saves()` + autosave-extracted records to the store on the saver thread; the orderly exit parks every player before the final save), client (a restored player whose record has a fortress or adventure skips the arrival map: it already gets `PlayerRegion` + `Fortress(Some)` / `AdventureResult(Ok)` on connect — check `session.rs`'s embark gate and `debug_launch.rs`'s auto-embark handle that and don't embark again)
- Test: `crates/game/tests/world_manager.rs` (extend), `crates/server/tests/threaded_world.rs` (one slow real-thread case)

- [ ] **Step 1: Write failing tests** (`InlineSpawner` manager):
  - `disconnect_parks_player_no_orphan_body` (after the disconnect the home region has no player entity; `take_player_saves` holds the record with its inventory)
  - `reconnect_same_token_restores_body_and_inventory` (move the player and give it a distinct inventory first; after reconnect: same pose ±0, same inventory, same mode)
  - `second_session_same_player_refused`
  - `restore_into_built_over_cell_lands_standable`
  - `autosave_extracts_online_player_records`
  - `restart_restores_player` (save the manager's region states + player store, build a fresh manager over them, reconnect → restored)
  - `fortress_claim_survives_restart`
- [ ] **Step 2:** `just test-one game "--test it world_manager::"` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4:** the tests → PASS; `just test-threads` → PASS; `just smoke` green.
- [ ] **Step 5: Commit** `feat: players (body, inventory, claims) persist across disconnects and restarts`

### Task 3: People's positions saved with the autosave

**Files:** `crates/server/src/lib.rs` (on `RegionOutput::Saved`, before forwarding: `history.note_positions(rollback.people_here())`), `crates/game/src/history/play.rs` (`History::note_positions(&mut self, Vec<(PersonId, [f32; 3])>)` = the position half of the `PeopleParked` handling, without despawn); test in `crates/game/tests/history_play.rs`: `autosave_positions_are_respawn_positions`.

- [ ] **Step 1:** Write the failing test (`note_positions`, then a region restart via `restore_saved` + `catch_up` → the person spawns at the noted position).
- [ ] **Step 2:** `just test-one game "--test it history_play::"` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** → PASS.
- [ ] **Step 5: Commit** `feat(server): autosave records people's positions in the history`

### Task 4: Item ids and block drop/hardness data

**Files:**
- Create: `crates/game/src/items.rs`; Test: `crates/game/tests/items.rs` (+ `mod items;` in `tests/it.rs`)
- Modify: `crates/game/src/registry.rs` (`BlockDef` gains `hardness: Option<u16>`, `drop: Option<DropDef>`), `block_props.rs` (`BlockProps` gains `hardness_centi: u16`, `drop: Drop`), `assets/blocks/blocks.ron` (values from Decision 6), `state.rs` (delete `ItemKind`; `ItemStack {item: ItemId, count: u32}`), `ui.rs` (`IconId::of(ItemId) -> IconId(item.0 as u32)`), every `ItemKind` user (`grep -rn ItemKind crates`)

**Interfaces — Produces:**
- `pub struct ItemId(pub u16)` (`Copy, Ord, Hash, StateHash, serde`); `ItemId::block(BlockId) -> ItemId`; `ItemId::as_block(self) -> Option<BlockId>` (`None` for id ≥ `ITEM_BLOCK_LIMIT`); `pub const ITEM_BLOCK_LIMIT: u16 = 1024; pub const STICK: ItemId = ItemId(1024); pub const MAX_STACK: u32 = 64;`
- `pub fn item_name(ItemId) -> &'static str` (block name from the registry, else the extra table)
- `pub enum Drop { Same, Nothing, Item(ItemId) }`; `pub fn drop_of(block: Block) -> Option<ItemStack>` (count 1, using `block_props()`; `Item("stick")` resolves through the extra table at registry load; an unknown name fails the registry load)

- [ ] **Step 1: Write failing tests** `items::drops_follow_manifest`: with the manifest installed, `drop_of(dirt) == Some(dirt×1)`, `drop_of(grass) == Some(dirt×1)`, `drop_of(oak_leaves) == None`, `drop_of(oak_branch) == Some(STICK×1)`, `drop_of(oak_log with tree data) == Some(oak_log×1)` (tree data bits do not leak into the item). `items::hardness_defaults`: stone 150, dirt 50, an entry with no `hardness` is 100. `items::unknown_drop_name_fails_load`: `BlockRegistry::from_ron` on a manifest with `drop: Item("nope")` is `Err`.
- [ ] **Step 2:** `just test-one game "--test it items::"` → FAIL (does not compile).
- [ ] **Step 3: Implement** the types and fields above; migrate `ItemKind` users; `Inventory::seeded()` becomes dirt×42 + stone×7 as `ItemId`s (Task 5 replaces it).
- [ ] **Step 4:** `just test-one game "--test it items::"` and `just test-one game "--test it ui_roundtrip::"` → PASS; `just smoke` green.
- [ ] **Step 5: Commit** `feat(game): item ids, block hardness and drops`

### Task 5: Inventory model and cursor clicks

**Files:**
- Create: `crates/game/src/inventory.rs` (move `Inventory` here, re-exported at the crate root); Test: `crates/game/tests/inventory.rs`
- Modify: `state.rs` `Client` (+ `selected: u8`, `cursor: Option<ItemStack>`), `ui.rs` (`UiAction::MoveItem` → `UiAction::Click {slot: SlotRef, button: ClickButton, shift: bool}`; keep `ToggleInventory`), `region.rs:715-728` (route `Click` through `inventory::click`), `build_hotbar` (slots 0..9 + `selected`)

**Interfaces — Produces:**
- `pub const INVENTORY_SLOTS: usize = 36; pub const HOTBAR_SLOTS: usize = 9;` with hotbar = slots `0..9`
- `Inventory::new() -> Self` (36 empty slots); `Inventory::starting_kit() -> Self` (`STARTING_CHESTS` chests in slot 0; it compiles before Task 10 adds the chest block by resolving the id by name, and is empty when the name is absent)
- `Inventory::add(&mut self, stack: ItemStack) -> u32` (returns the remainder; tops up existing stacks first in hotbar-then-main order, then the first empty slot)
- `Inventory::take_selected(&mut self, selected: u8, n: u32) -> Option<ItemStack>`
- `pub enum SlotRef { Player(u8), Container(u8) }`, `pub enum ClickButton { Left, Right }`
- `pub fn click(cursor: &mut Option<ItemStack>, player: &mut Inventory, container: Option<&mut Vec<Option<ItemStack>>>, slot: SlotRef, button: ClickButton, shift: bool)` — the Minecraft rules from Decision 11; an out-of-range slot or a `Container` slot with no container is a no-op
- `HotbarView` gains `selected: u8`

- [ ] **Step 1: Write failing tests**, one per rule: `add_tops_up_then_fills` (hotbar dirt 60 + add 10 → 64, 6 in the next empty slot, remainder 0); `add_full_returns_remainder`; `left_click_pick_place_swap_merge`; `right_click_halves_and_places_one` (a stack of 7 → cursor 4, slot 3); `shift_moves_hotbar_to_main_and_container_to_player`; `click_out_of_range_is_noop`; `ui_click_event_undoes` (apply `GameEventKind::UiAction(c, Click{..})` on a region, undo, `state_hash` equal).
- [ ] **Step 2:** `just test-one game "--test it inventory::"` → FAIL.
- [ ] **Step 3: Implement.** CreateClient / CreateClientAt seed `Inventory::starting_kit()`. Update `ui_roundtrip` to the new `UiAction`.
- [ ] **Step 4:** `just test-one game "--test it inventory::"`, `"--test it ui_roundtrip::"` → PASS.
- [ ] **Step 5: Commit** `feat(game): 36-slot inventory with Minecraft cursor clicks`

### Task 6: Survival controls in Adventure

**Files:**
- Modify: `crates/game/src/input.rs` (`Key` + `Digit1..=Digit9`, `KeyQ`; `InputState::mouse_held(&MouseButton) -> bool`), `movement.rs` (`MoveParams.allow_flight: bool`; `plan` ignores the double tap and clears `flying` when false), `camera.rs:273-282` (pass `allow_flight = client.mode != PlayMode::Adventure`), `region.rs:631-645` (in Adventure, E toggles `inventory_open`, and closing returns the cursor stack through `Inventory::add`, dropping any remainder via Task 7 — until Task 7 lands, the remainder stays on the cursor; digits set `selected`; `MouseWheel` steps `selected` mod 9 while `fps_cam_mode && !inventory_open`), client `renderer/input.rs` (`map_key` digits + Q; in first person forward E when `ClientPlayMode` is Adventure), client `commander/adventure.rs` `on_mode_change` (entering Adventure turns first person on through the same path F5 uses, `first_person.rs`)
- Test: `crates/game/tests/adventure.rs` (extend), `crates/game/tests/movement` coverage via `jump_arc.rs` if the plan signature changes

**Interfaces — Produces:** `MoveParams { allow_flight: bool, .. }` (Fortress callers pass `true`); `Client.selected` is written only here and in Task 5.

- [ ] **Step 1: Write failing tests**: `adventure::no_flight_in_adventure` (an Adventure body with `fps_cam_mode`, double-tap Space within `DOUBLE_TAP_WINDOW_MS` → `motion.flying == false`, and the body comes back down); `adventure::fortress_body_still_flies`; `adventure::e_toggles_inventory_in_adventure` (`inventory_open` flips, `fps_cam_mode` unchanged); `adventure::digits_and_wheel_select_slot` (Digit3 → `selected == 2`; wheel −1 from 0 → 8).
- [ ] **Step 2:** `just test-one game "--test it -- --include-ignored adventure::"` → the new tests FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4:** the same command → PASS; `just test-one game "--test it jump_arc::"` → PASS; `just smoke` green (it type-checks the client).
- [ ] **Step 5: Commit** `feat: survival controls in adventure (no flight, E inventory, hotbar keys, first person on entry)`

### Task 7: Item piles — deposit, merge, settle, pickup, expiry

**Files:**
- Create: `crates/game/src/item_piles.rs`; Test: `crates/game/tests/item_piles.rs`
- Modify: `state.rs` (`GameData` + `#[undo(map)] item_piles: BTreeMap<BlockPos, ItemPile>`, `#[undo(map)] pile_expiry: BTreeMap<(u64, BlockPos), ()>`; `GameDataUpdateKind::SetItemPiles(Vec<(BlockPos, Option<PileView>)>)`), `edit.rs` `set_block_in` (after a successful write: if the block became non-solid, re-settle the pile at `pos + y1`; if it became solid and a pile sits at `pos`, re-settle it from `pos + y1`), `region.rs` tick (after the edits: `tick_pickups_safe(tick)`, then `expire_piles_safe(tick)`), `region.rs` Task 6's inventory close (drop the remainder), `server/src/region_store.rs` (`REGION_FILE_VERSION = 2`), the hash goldens (re-record)

**Interfaces — Produces:**
- `pub struct ItemPile { pub stacks: Vec<ItemStack>, pub expire_tick: u64, pub pickup_after_tick: u64 }` (≤ `PILE_MAX_STACKS = 4` stacks, each ≤ `MAX_STACK`); `pub struct PileView { pub stacks: Vec<ItemStack> }`
- `Rollback::settle_cell(&self, from: BlockPos) -> BlockPos` — Decision 2's walk, x/z first clamped to `0..REGION_BLOCKS` (the region's own blocks)
- `Rollback::deposit_items_safe(&mut self, at: BlockPos, stack: ItemStack, tick: u64, pickup_delay: u64) -> u32` — returns the count destroyed by caps (0 normally). Order: settle `at`; top up same-item stacks in piles within `PILE_MERGE_RADIUS_BLOCKS` of the rest cell (order: Chebyshev distance, then `BlockPos` key); then a new stack in the rest-cell pile; if that pile is full, the nearest free cell in the radius that settles to itself; else the remainder is destroyed. At `PILES_PER_REGION_MAX`, evict the first `pile_expiry` entry. Emits `SetItemPiles` (Do: new views; `emit_on_undo`: prior views).
- `Rollback::resettle_pile_safe(&mut self, cell: BlockPos, tick: u64)` (removes the pile and re-deposits its stacks)
- `Rollback::tick_pickups_safe(&mut self, tick: u64)` (Adventure clients only; Decision 4) and `Rollback::expire_piles_safe(&mut self, tick: u64)`

- [ ] **Step 1: Write failing tests** (region from `chop.rs`'s `region()` pattern: floor at y 0, so the rest cell is y 1):
  - `deposit_settles_on_floor` (deposit at y 10 → pile at y 1)
  - `deposits_bunch` (40 dirt dropped one at a time at 9 cells of a 3×3 → exactly 1 pile holding 40)
  - `merge_respects_max_stack` (100 logs at one cell → one pile with stacks [64, 36])
  - `different_items_share_pile_until_full` (5 kinds → a second pile appears in the radius)
  - `pile_falls_when_support_broken`, `pile_pushed_up_by_placed_block`, `deposit_outside_region_clamps_inside`
  - `deposit_in_water_rests_on_floor`
  - `pickup_after_delay_only` (picked up at tick ≥ `pickup_after_tick`, not before)
  - `pickup_full_inventory_keeps_remainder`
  - `pickup_undo_restores_pile_and_inventory` (`state_hash` before == after undo)
  - `two_clients_one_pile_goes_to_lower_client_id_first` (deterministic order)
  - `expiry_removes_pile_at_tick` (deposit at t, gone at t + `PILE_DESPAWN_TICKS`)
  - `cap_evicts_earliest`
  - `deposit_emits_updates_both_ways`
- [ ] **Step 2:** `just test-one game "--test it item_piles::"` → FAIL.
- [ ] **Step 3: Implement.** Re-record the goldens: `just test-one worldgen "--test it scene_hash_golden::"` and `just determinism-wasm`, updating the pinned values with a note on why.
- [ ] **Step 4:** `just test-one game "--test it item_piles::"`, `"--test it block_edit::"`, `"--test it underground_edit::"` → PASS; `just determinism-wasm` → PASS; `just smoke` green.
- [ ] **Step 5: Commit** `feat(game): cell-keyed item piles that merge on deposit (no entities)`

### Task 8: Mining with break times, block drops, survival place and throw

**Files:**
- Create: `crates/game/src/mining.rs`; Test: `crates/game/tests/mining.rs`
- Modify: `state.rs` `Client` (+ `mining: Option<Mining>`, `break_cooldown_until: u64`), `region.rs:603-625` (Adventure clients go through `tick_mining_safe`; Fortress keeps `player_edit_safe`), `edit.rs` (split the eye-ray lookup out of `player_edit_safe` as `Rollback::eye_hit(client) -> Option<BlockHit>`, used by both)

**Interfaces:**
- Consumes: `drop_of`, `deposit_items_safe`, `Inventory::take_selected`, `Client.selected`, `InputState::mouse_held`
- Produces:
  - `pub struct Mining { pub target: BlockPos, pub started_tick: u64, pub needed_ticks: u32 }`
  - `pub fn break_ticks(props: &BlockProps) -> u32` (Decision 7 formula)
  - `Rollback::tick_mining_safe(&mut self, client: ClientId, tick: u64)`: on completion, an ordinary block → `clearable_block` + `set_block_in(AIR)`, then `deposit_items_safe(block cell, drop, tick, PICKUP_DELAY_TICKS)`; a tree block calls Task 9's `player_chop_safe`
  - `Rollback::survival_place_safe(client) -> bool`: the right click opens a chest if one is hit (Task 10 fills this in), else places `as_block()` of the selected item and takes 1
  - `Rollback::throw_selected_safe(client, whole_stack: bool, tick) -> bool` (Q / Ctrl+Q)
  - `MiningView { target, progress_per_mille }` in `HomeView` for the HUD

- [ ] **Step 1: Write failing tests**: `dirt_breaks_after_15_ticks` (left held while looking at dirt: still solid at tick 14, air at 15, one dirt pile below the hole); `release_cancels_progress`; `retarget_restarts`; `cooldown_blocks_next_break_for_6_ticks`; `leaves_drop_nothing`; `grass_drops_dirt`; `place_consumes_selected_and_refuses_non_block` (stick selected → no place); `place_with_empty_slot_does_nothing`; `throw_lands_two_blocks_ahead_on_ground` (pickup delay 40); `mining_undo_roundtrip` (state hash).
- [ ] **Step 2:** `just test-one game "--test it mining::"` → FAIL.
- [ ] **Step 3: Implement.** Wire Q / Ctrl+Q in the tick.
- [ ] **Step 4:** `just test-one game "--test it mining::"` and `"--test it block_edit::"` → PASS (Fortress instant edit unchanged).
- [ ] **Step 5: Commit** `feat(game): survival mining with break times, drops, place from hotbar, throw`

### Task 9: Chopping trees as a player yields logs where the tree lands

**Files:**
- Modify: `voxel_body.rs` (`SettlePolicy::DropItems`; in `tick_voxel_bodies_safe`, before `remove_entity_safe`, for `DropItems` iterate `model.blocks` in key order, map each model block through the body's final isometry to a region block cell (the block centre transformed, floored), `drop_of` it, and `deposit_items_safe(cell, stack, tick, PICKUP_DELAY_TICKS)`), `orders.rs` (extract `chop_safe`'s body into `Rollback::fell_safe(p, policy) -> Option<(Vec<BlockPos> /*cut*/, Option<EntityKey>)>`; the fortress order keeps `Despawn`), `mining.rs` (`player_chop_safe(client, p, tick)` = `fell_safe(p, DropItems)` + deposit the cut blocks' drops at their cells)
- Test: `crates/game/tests/chop.rs` (extend), `crates/worldgen/tests/tree_chop.rs` (extend, slow)

**Interfaces — Produces:** `SettlePolicy::DropItems`; `Rollback::fell_safe`; `Rollback::player_chop_safe(&mut self, client: ClientId, p: BlockPos, tick: u64)`.

- [ ] **Step 1: Write failing tests** in `chop.rs`, using its hand-built tree (8 trunk logs, branch, leaves):
  - `player_trunk_cut_yields_every_log` (an Adventure player mines the trunk at y 1 for `break_ticks × FELL_TICKS_FACTOR` ticks; run to settle (≤ `SETTLE_TIMEOUT_TICKS`); the sum of oak_log over all piles == 8; sticks == branch blocks; no leaves item; pile count ≤ 3)
  - `logs_lie_where_tree_landed` (every pile's x/z is within the tipped tree's reach from the stump, not stacked on the stump)
  - `fortress_chop_still_yields_nothing` (orders path unchanged)
  - `player_chop_undo_roundtrip` (state hash restored after undoing the whole sequence)
  - On generated trees (worldgen, `#[ignore]` slow): `tree_chop::player_fell_yields_one_log_per_wood_block` for each species with logs.
- [ ] **Step 2:** `just test-one game "--test it chop::"` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4:** `just test-one game "--test it chop::"`, `just test-one game "--test voxel_body"`, `just test-one worldgen "--test it -- --include-ignored tree_chop::"` → PASS; `just determinism-wasm` → PASS.
- [ ] **Step 5: Commit** `feat(game): players fell trees in adventure; logs drop where the tree lands`

### Task 10: Chests

**Files:**
- Create: `crates/game/src/containers.rs`; Test: `crates/game/tests/chests.rs`
- Modify: `assets/blocks/blocks.ron` (append `chest`, `Faces` with `chest_top.png` / `chest_side.png`, hardness 250), `tools/textures/gen.py` (the two chest PNGs, recoloured from the oak textures) + run `just block-textures`, client `EMBEDDED_BLOCK_TEXTURES` (+ the two PNGs), `state.rs` (`GameData` + `#[undo(map)] containers: BTreeMap<BlockPos, Container>`; `Client.open_container: Option<BlockPos>`), `edit.rs` `set_block_in` (a chest written → create an empty container; a chest replaced → spill), `mining.rs` `survival_place_safe` (open a hit chest), `inventory::click` callers (pass the open container), `region.rs` tick (`close_far_containers_safe`)

**Interfaces — Produces:**
- `pub struct Container { pub slots: Vec<Option<ItemStack>> }` (`CHEST_SLOTS = 27`)
- `pub const CONTAINER_REACH_BLOCKS: i32 = 6; pub const STARTING_CHESTS: u32 = 2;`
- `Rollback::open_container_safe(client, pos) -> bool`, `close_container_safe(client)`, `spill_container_safe(pos, tick)`
- `ContainerView { pos, slots }` in `HomeView` while open

- [ ] **Step 1: Write failing tests**: `placing_chest_creates_container`; `right_click_chest_opens_not_places`; `shift_click_moves_into_chest`; `walking_away_closes`; `break_chest_spills_contents_and_chest` (27 slots of mixed items → piles at and around the cell holding every item plus 1 chest; the container is gone); `break_open_chest_spills_once_and_closes` (two clients have it open: one breaks it, the spill happens once, both `open_container == None`); `chest_survives_region_park_restore` (`SerializedRegion` round trip keeps the contents); `chest_undo_roundtrip`.
- [ ] **Step 2:** `just test-one game "--test it chests::"` → FAIL.
- [ ] **Step 3: Implement.** Starting kit now resolves to 2 chests.
- [ ] **Step 4:** `just test-one game "--test it chests::"`, `"--test it inventory::"` → PASS; `just smoke` green.
- [ ] **Step 5: Commit** `feat(game): chests (block inventories) that spill when broken`

### Task 11: Client — draw item piles

**Files:**
- Create: `crates/client/src/renderer/item_piles.rs` (`ItemPilesPlugin`, registered in `renderer/mod.rs`; entities tagged `SessionScoped`)
- Modify: `renderer/bridge.rs` (route `SetItemPiles` per region into an `ItemPileEvents` message; `RemoveRegion` despawns that region's piles)

**Interfaces:**
- Consumes: `GameDataUpdateKind::SetItemPiles`, `PileView`, the region's world offset (as the bridge places chunks)
- Produces: one root entity per `(RegionId, BlockPos)` with child cubes: one shared `Cuboid` mesh of `ITEM_CUBE_SIZE_M` (0.25), one `StandardMaterial` per `ItemId` (the block's side texture, nearest-sampled; for non-block items, `assets/items/<name>.png` on a flat quad), `cubes_for(count) = min(4, ceil(count/16))` per stack, spread on a fixed little offset table; client-only bob (`ITEM_BOB_M` 0.05, period π s) and spin (1 rad/s); hidden by the fortress cut like terrain above the level.

- [ ] **Step 1: Write a failing headless test** `item_piles::spawns_and_despawns` (`MinimalPlugins` + `AssetPlugin`, as the `model::` tests do): send `SetItemPiles([(cell, Some(view of 40 dirt))])` → 1 root with 3 cube children at the cell's world position; send `None` → the root is gone. Also `item_piles::cubes_for_counts` (1→1, 16→1, 17→2, 64→4).
- [ ] **Step 2:** `just test-one client "--lib item_piles::"` → FAIL. (A client test binary build is slow; this is the one opt-in for this task.)
- [ ] **Step 3: Implement.**
- [ ] **Step 4:** the same → PASS; `just smoke` green.
- [ ] **Step 5: Commit** `feat(client): render item piles as instanced little cubes`

### Task 12: Client — hotbar HUD, inventory/chest screen, mining progress

**Files:**
- Create: `crates/slint-ui/ui/inventory.slint` (`InventoryScreen`: a 9×3 main grid + a 9×1 hotbar row + an optional 9×3 container grid above, a cursor stack following the pointer, using the `fortress`-theme widgets' slot style), `crates/client/src/renderer/item_icons.rs` (`ItemId` → `slint::Image` decoded once from the embedded block/item PNGs; works on wasm the same way `wasm_images` does)
- Modify: `crates/slint-ui/ui/hud.slint` (restore the hotbar strip from `git show 087692a -- crates/slint-ui/ui/hud.slint`, + a selected highlight + a crosshair progress ring `mining-progress: float`; shown only in Adventure first person), `ui/game.slint` / `index.slint` (mount `InventoryScreen`), `slint_overlay.rs` (consume `UiUpdate::Hotbar`; a `Screens.inventory` modal while `HomeView` says `inventory_open`; slot presses → `UiAction::Click` with Shift from the keyboard state; Esc closes → `UiAction::ToggleInventory`), `renderer/input.rs:130` (also drop clicks and motion to the sim while `CursorPolicy::ui_modal` — the gap the investigation found)
- Test: `crates/slint-ui/tests/inventory.rs` (+ module in `tests/it.rs`)

**Interfaces — Consumes:** `HotbarView {slots, selected}`, `MiningView`, `ContainerView`, the inventory and cursor from `HomeView` (add `inventory: Inventory`, `cursor: Option<ItemStack>`, `inventory_open: bool` to `HomeView` in `regions/runner.rs` if absent).

- [ ] **Step 1: Write failing slint-ui tests** (`#[ignore]` slow, headless, as `fortress_panel::` does): `inventory::slot_press_reports_slot_ref` (pressing main slot (2,1) calls back with `Player(9 + 1*9 + 2)`; a container slot with `Container(n)`); `inventory::container_grid_hidden_without_container`; `inventory::hud_hotbar_highlights_selected`.
- [ ] **Step 2:** `just test-one slint-ui "--test it -- --include-ignored inventory::"` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4:** the same → PASS; `just smoke` green; `just slint-wasm` → PASS (the embedded icon path).
- [ ] **Step 5: Commit** `feat(ui): hotbar, inventory and chest screens, mining progress`

### Task 13: Live check and docs

**Files:** `CLAUDE.md` (the Adventure-mode paragraph: survival rules, piles, chests, the tunables' homes and the test commands), `TODO.md` (follow-ups from "Known limits"), `justfile` (no new recipe expected)

- [ ] **Step 1:** `just smoke`, `just determinism-wasm`, `just test-threads` (inventory rides `Client` across seams) → all PASS.
- [ ] **Step 2: Live run** (one client, Intel adapter per memory): `just adapter=Intel run` → embark → Esc/embark Adventure tab → Start adventure. Check: first person on entry; double-tap Space does not fly; dirt breaks in ~0.75 s, a log in 3 s; dirt drops bunch into one pile; E opens the inventory, clicks move stacks; place 2 chests, store items, break one → it spills; chop a trunk → the tree falls → logs lie where it landed in ≤ a few piles → walk over them to pick up → quit the client and relaunch `just adapter=Intel run` → you're back where you stood, with the same inventory; quit both and relaunch → the same. Report what was seen, with anything off.
- [ ] **Step 3:** Update the docs, then **commit** `docs: adventure survival items`.
