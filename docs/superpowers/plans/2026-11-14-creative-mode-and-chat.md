# Creative Mode and Chat Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A player can type `/creative` or `/normal` into an in-game chat box, in Fortress or
Adventure, and in creative flies in first person, breaks instantly and places blocks picked from a
tabbed creative inventory.

**Architecture:** Creative is one per-client sim flag (`Client.creative`) beside `PlayMode`, switched by
a new `Order` and read by the edit, flight and inventory paths; it is predicted, replayed, carried
across seams and saved like the rest of `Client`. Chat is client-only: a session log, a hand-written
command parser, and a runner that turns each command into the sim event a button would send. One
typing gate empties `ButtonInput<KeyCode>` while a Slint text field has the keyboard.

**Tech Stack:** Rust (`crates/game`, `crates/client` on Bevy 0.19.1), Slint (`crates/slint-ui`), RON
(`assets/blocks/blocks.ron`), the `just` recipes.

**Spec:** `docs/superpowers/specs/2026-11-14-creative-mode-and-chat-design.md` — read it first.

**Status:** the spec is **not yet approved**; its calls marked **(call)** and its three open questions
are the user's to confirm before Task 1 starts.

**Progress (2026-11-14):** built out of order, to stay out of the R4 work that was editing and testing
`crates/game`, `crates/server` and `crates/world-store` in this checkout.

- **Done, uncommitted, type-checked only** (`cargo check -p client --lib --bins --tests`; no test run,
  not seen running): Task 5 (typing gate), Task 6 and Task 7 (chat log, parser, box) and the part of
  Task 8 that needs no sim change (`/help`, `/adventure`, `/fortress`, `/where`, `/clear`, `/complete`,
  `/dropbody`, `/toggle`). Only `crates/client` and `crates/slint-ui` were touched.
- **Differences from the tasks below:** T opens the chat only in first person (in the commander T is
  the Chop tool, `designate.rs:67`); Enter and `/` open it everywhere. `/adventure` and `/fortress`
  press the Adventure tab's buttons (`slint_overlay::queue_ui_action`) rather than sending a request of
  their own. The typing gate also keeps a key that was down while typing up until it is released (key
  repeat would otherwise press Esc or Enter in the game after the box closes). The headless Slint tests
  of Task 7 are not written.
- **Waiting for R4 to land:** Tasks 1–4 (the sim: a new `Client` field moves the state hash, the
  goldens and the saved player / region formats, and travels through the hand-offs R4 is changing),
  then `/creative`, `/normal`, `/gamemode`, `/give` in Task 8, and Tasks 9–11.

## Global Constraints

- Work on `develop` in this checkout. **No git worktrees.** The tree holds the user's uncommitted work:
  `git add` only the files a task names, never `-A`.
- `crates/game` stays free of Bevy and windowing. Ordered sim state in `BTreeMap`; hashing through
  `StateHash`; no `f32` transcendentals outside `libm`.
- New `Order`, `UiAction` and `UiUpdate` variants are **appended** (bincode indices).
- Rollback bar: `hash(before) == hash(after undo)`, bit-exact, for every new sim mutation.
- Never run a whole game/server/client suite as a routine check. Sim tests:
  `just test-one game "--test it <file>::"`. Client tests (`just test-one client "--lib <filter>"`) are
  a slow opt-in: **ask the user once**, before Task 5, for the filters `text_entry::`, `chat::`,
  `first_person::`, `creative_palette::`.
- New sim tests slower than 100 ms carry `#[ignore = "slow: >100ms — run via just test-full"]`.
- A test whose outcome reads block props calls `common::install_real_block_props()` first.
- UI is Slint only. Never bind `visible` / `opacity` on a component's root. New per-world client state
  is registered with `add_session_reset` or held in `SessionLocal`.
- `CursorPolicy` (`crates/client/src/renderer/cursor.rs`) is the only cursor-grab writer.
- Exact values from the spec: chat line limit **256** characters; log **100** lines; **8** recent lines
  shown **10 s** each; sent history **50** lines; `/give` count **1..=64**, default **64**; palette grid
  **9** columns.
- End commit messages with `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>`.

## Review Focus

1. **A key held when the chat opens** (W down, then T): the body stops, and does not walk again when
   the chat closes with W still down until W is pressed anew. Pinned in Task 5.
2. **A palette action from a client that is not creative, or with a bad item or count** (item id
   60000, count 0, count 1 000 000): nothing changes. Pinned in Task 3.
3. **Creative switched off in mid-air, or with a stack on the cursor**: the body falls, the stack goes
   back into the inventory (or drops at the feet). Pinned in Task 1.
4. **A creative player crossing a seam or parked and restored**: still creative on the other side.
   Pinned in Task 1.
5. **The key that opens the chat**: the `t` does not land in the field; Esc closes the chat and does not
   open the menu. Pinned in Task 7.

## File Structure

| File | Responsibility |
|---|---|
| `crates/game/src/creative.rs` (new) | `SetCreative`, creative place, palette actions, `item_exists`, `item_named` |
| `crates/game/src/state.rs`, `migrate.rs`, `player.rs`, `orders.rs`, `ui.rs`, `region.rs`, `camera.rs`, `item_piles.rs` | the flag and the gates that read it |
| `crates/game/src/registry.rs`, `assets/blocks/blocks.ron` | `CreativeTab` on each block |
| `crates/game/tests/creative.rs` (new, listed in `tests/it.rs`) | sim tests |
| `crates/client/src/renderer/text_entry.rs` (new) | the typing gate |
| `crates/client/src/renderer/chat/{mod,command,run,ui}.rs` (new) | log, parser, runner, Slint glue |
| `crates/client/src/renderer/commander/creative.rs` (new) | `ClientCreative`, the `SetCreative` order from the camera |
| `crates/client/src/renderer/creative_palette.rs` (new) | tab lists, search filter, palette clicks |
| `crates/slint-ui/ui/chat.slint` (new), `hud.slint`, `game.slint`, `inventory.slint` | chat box, recent lines, palette |

---

### Task 1: The creative flag in the sim

**Files:**
- Create: `crates/game/src/creative.rs`, `crates/game/tests/creative.rs`
- Modify: `crates/game/src/state.rs` (`Client`, ~line 150), `crates/game/src/migrate.rs:38-70`,
  `crates/game/src/player.rs` (`parked_client`), `crates/game/src/orders.rs` (`Order`, the dispatch at
  ~line 408), `crates/game/src/ui.rs` (`UiUpdate`), `crates/game/src/lib.rs`, `crates/game/tests/it.rs`
- Modify (goldens and formats, as `git show d36a0e7f` did for `open_container`):
  `crates/game/tests/wasm_determinism.rs`, `crates/worldgen/tests/scene_hash_golden.rs`,
  `crates/server/src/player_store.rs`, `crates/server/src/region_store.rs`

**Interfaces:**
- Produces:
  - `pub creative: bool` on `game::Client`
  - `impl Client { pub fn hands(&self) -> bool }` — `mode == PlayMode::Adventure || creative`
  - `Order::SetCreative { on: bool, at: Option<TilePos> }`
  - `impl Rollback { pub fn set_creative_safe(&mut self, client: ClientId, on: bool, at: Option<TilePos>) -> bool }`
  - `UiUpdate::Creative(bool)`

- [ ] **Step 1: Write the failing tests** in `crates/game/tests/creative.rs`, with the rig of
  `tests/adventure.rs` (`region_with_player`, `tick`) and its undo test (`set_mode_undo_bit_exact`,
  line 174) as the pattern. Add `mod creative;` to `tests/it.rs`.

```rust
#[test] fn new_client_is_not_creative()                 // !client.creative, !client.hands()
#[test] fn set_creative_on_then_off()                   // Order::SetCreative{on:true,at:None} -> creative, hands(); off -> neither
#[test] fn set_creative_at_places_the_body_on_the_tile() // on:true, at:Some(t): body_on_tile(nearest standable of t), as set_mode_adventure_places_body_on_tile
#[test] fn set_creative_at_outside_the_region_is_refused() // at x = REGION_BLOCKS: returns false, nothing changed
#[test] fn set_creative_undo_bit_exact()                // hash before == hash after rollback, on and off
#[test] fn set_creative_twice_is_a_noop()               // second on: state hash unchanged, no second UiUpdate
#[test] fn set_creative_emits_ui_update_once()          // UiUpdate::Creative(true) once on the render channel
#[test] fn off_in_mid_air_clears_flight()               // Motion.flying set, SetCreative{on:false}: flying == false, body y falls over 20 ticks
#[test] fn off_with_a_cursor_stack_stows_it()           // Fortress, inventory_open, cursor Some(64 stone): off -> inventory_open false, cursor None, inventory holds 64 stone
#[test] fn creative_travels_and_parks()                 // Client{creative:true,..}.arrive().creative && parked_client(&c).creative
```

- [ ] **Step 2: Run** `just test-one game "--test it creative::"` — expected: does not compile
  (`creative`, `SetCreative` unknown).

- [ ] **Step 3: Implement.**
  - `Client.creative` as the last field. In `Client::arrive` destructure it under the comment
    "Creative is the player's, not the region's: it travels." and keep it. `parked_client` leaves it.
  - `set_creative_safe` in `creative.rs`: unknown client → `false`; `at` goes through the same clamp as
    `set_play_mode_safe` (`adventure.rs:92`, make `clamp_tile` and `place_body_safe` `pub(crate)`);
    same value → `true`, nothing touched; on → set the flag, then `place_body_safe` when `at` is given;
    off → clear the flag, clear `Motion.flying` of the client's body with `ecs.motion.set_safe`, and
    when `mode == Fortress` call `leave_survival` and drop what it returns with `drop_at_feet_safe`.
    Send `UiUpdate::Creative(on)` the way `set_play_mode_safe` sends `UiUpdate::PlayMode`.
  - `Order::SetCreative` appended after `DevDropBody`; dispatch beside `Order::SetMode`.

- [ ] **Step 4: Run** `just test-one game "--test it creative::"` — expected: PASS.

- [ ] **Step 5: Re-record what the new field moves.** Read
  `git show d36a0e7f -- crates/game/tests/wasm_determinism.rs crates/worldgen/tests/scene_hash_golden.rs crates/server/src/player_store.rs crates/server/src/region_store.rs`
  and make the same kind of change in each (pinned hashes at chunk size 16 only; the format constants
  the player and region blobs carry). Run `just determinism-wasm` and
  `just test-one worldgen "--test scene_hash_golden"` — expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/game/src/{creative,state,migrate,player,orders,ui,lib,adventure}.rs crates/game/tests/{creative,it,wasm_determinism}.rs crates/worldgen/tests/scene_hash_golden.rs crates/server/src/{player_store,region_store}.rs
git commit -m "feat(game): a per-client creative flag, switched by Order::SetCreative"
```

### Task 2: Creative hands — instant edits from the hotbar, flight, the inventory keys

**Files:**
- Modify: `crates/game/src/creative.rs`, `crates/game/src/region.rs:803-900` and `:931`,
  `crates/game/src/camera.rs:266,303`, `crates/game/src/mining.rs:134`,
  `crates/game/src/item_piles.rs:519`
- Test: `crates/game/tests/creative.rs`

**Interfaces:**
- Consumes: `Client.creative`, `Client::hands()`, `Order::SetCreative` (Task 1).
- Produces: `impl Rollback { pub(crate) fn creative_place_safe(&mut self, client: ClientId, out: &mut Vec<IntentMsg>) -> bool }`

- [ ] **Step 1: Write the failing tests**, with the terrain rig of `tests/block_edit.rs` (`region`,
  `input`, `forget`, `player`; send `InputEvent::FpsCam(true)` instead of its KeyE toggle) and creative
  switched on by `Order::SetCreative`. Each runs once in Fortress and once in Adventure
  (`Order::SetMode`) through a shared helper `fn creative_region(mode: PlayMode) -> Region`.

```rust
#[test] fn left_click_breaks_at_once_and_drops_nothing() // one tick after the press + intent tick: block is AIR, no item pile
#[test] fn right_click_places_the_selected_block_and_keeps_the_stack() // hotbar 0 = 5 stone: placed cell is stone, stack still 5
#[test] fn right_click_with_an_empty_slot_places_nothing()
#[test] fn right_click_with_a_stick_places_nothing()
#[test] fn right_click_on_a_chest_opens_it()            // client.open_container == Some(cell), nothing placed
#[test] fn no_edit_while_the_inventory_is_open()
#[test] fn no_mining_timer_in_creative_adventure()      // client.mining stays None while left is held
#[test] fn double_tap_space_flies_in_adventure_creative() // Motion.flying after two presses within the existing window
#[test] fn e_opens_the_inventory_in_fortress_creative() // inventory_open, and fps_cam_mode unchanged
#[test] fn digits_and_wheel_select_in_fortress_creative() // Key::Digit3 -> selected == 2; wheel y=-1 -> 3
#[test] fn creative_edit_undo_bit_exact()               // place then rollback: hash equal
#[test] fn fortress_fps_without_creative_still_places_dirt() // the legacy path: PLACE_BLOCK
```

- [ ] **Step 2: Run** `just test-one game "--test it creative::"` — expected: the new tests FAIL.

- [ ] **Step 3: Implement.**
  - `creative_place_safe`: `survival_place_safe` (`mining.rs:207`) without `take_selected` and with
    `survival: false`; share the body through one private function taking `consume: bool` rather than
    copying it.
  - `region.rs` edit dispatch: three disjoint sets per tick, in this order —
    creative (`c.creative && c.fps_cam_mode && !c.inventory_open`): break → `player_edit_safe(Break)`,
    place (pressed, or held with `pillar_target`) → `creative_place_safe`;
    legacy (`!c.creative && c.mode != Adventure && …`): unchanged;
    survivors (`c.mode == Adventure && !c.creative`): unchanged, except that the throw (`Q`) runs for
    `c.hands()`.
  - `region.rs:877`: the E / digits branch reads `client.hands()`; the fps-cam toggle on E stays
    `!hands`. `region.rs:931` (wheel) and `item_piles.rs:519` (pickup) read `hands()`.
  - `mining.rs:134`: `active` also needs `!c.creative`.
  - `camera.rs:303`: `allow_flight: client.creative || client.mode != PlayMode::Adventure`.
    `camera.rs:266` (the walker steering an inactive Adventure body) is left as it is.

- [ ] **Step 4: Run** `just test-one game "--test it creative::"`, then the files whose paths were
  touched: `just test-one game "--test it block_edit::"`, `… mining::"`, `… inventory::"`,
  `… adventure::"`, `… chests::"` — expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/{creative,region,camera,mining,item_piles}.rs crates/game/tests/creative.rs
git commit -m "feat(game): creative hands — instant edits from the hotbar, flight, inventory keys in both modes"
```

### Task 3: Palette actions

**Files:**
- Modify: `crates/game/src/creative.rs`, `crates/game/src/ui.rs` (`UiAction`),
  `crates/game/src/region.rs` (`GameEventKind::UiAction`, ~line 1011), `crates/game/src/items.rs`
- Test: `crates/game/tests/creative.rs`

**Interfaces:**
- Produces:
  - `UiAction::CreativeTake { item: ItemId, count: u32 }`, `UiAction::CreativeGive { item: ItemId, count: u32 }`, `UiAction::CreativeTrash` (appended)
  - `pub fn item_exists(item: ItemId) -> bool` — a registry block that is not air and not `BlockKind::Liquid`, or an `EXTRA_ITEMS` id
  - `pub fn item_named(name: &str) -> Option<ItemId>` — a block's manifest name or an extra item's, only when `item_exists`

- [ ] **Step 1: Write the failing tests.**

```rust
#[test] fn take_puts_a_stack_on_the_cursor()            // creative, inventory open: cursor == Some(64 stone)
#[test] fn take_replaces_what_the_cursor_held()
#[test] fn take_needs_the_inventory_open()
#[test] fn give_adds_to_the_inventory_and_drops_the_rest() // 35 slots full: 64 land in the last, a second give drops a pile at the feet
#[test] fn trash_empties_the_cursor()
#[test] fn palette_actions_are_refused_outside_creative() // each of the three: state hash unchanged
#[test] fn bad_items_and_counts_are_refused()           // ItemId(60000), air, water, count 0, count 1_000_000: hash unchanged
#[test] fn palette_actions_undo_bit_exact()
#[test] fn item_named_finds_blocks_and_extras()         // "stone" -> block stone, "stick" -> STICK, "air" / "water" / "nope" -> None
```

- [ ] **Step 2: Run** `just test-one game "--test it creative::"` — expected: FAIL (variants unknown).

- [ ] **Step 3: Implement** `impl Rollback { pub(crate) fn creative_action_safe(&mut self, client: ClientId, action: UiAction) }`
  in `creative.rs` and route the three variants to it from `region.rs`. The check order is: client
  exists, `creative`, `item_exists`, `1 <= count <= MAX_STACK`, then (Take, Trash) `inventory_open`.

- [ ] **Step 4: Run** `just test-one game "--test it creative::"` — expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/{creative,ui,region,items}.rs crates/game/tests/creative.rs
git commit -m "feat(game): creative palette actions — take, give, trash"
```

### Task 4: A creative tab on every block

**Files:**
- Modify: `crates/game/src/registry.rs` (`BlockDef`, ~line 32), `assets/blocks/blocks.ron`,
  `crates/game/src/lib.rs` (re-export, if the glob does not cover it)
- Test: `crates/game/src/registry.rs` (`#[cfg(test)]`, beside the registry's own tests)

**Interfaces:**
- Produces:
  - `pub enum CreativeTab { Natural, Wood, Building, Colour, Functional }` (`Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize`)
  - `pub tab: Option<CreativeTab>` on `BlockDef`, `#[serde(default)]`

- [ ] **Step 1: Write the failing test.**

```rust
#[test]
fn every_placeable_block_has_a_creative_tab() {
    let reg = BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
    let missing: Vec<&str> = reg.defs().filter(|d| d.id != 0 && d.kind != BlockKind::Liquid && d.tab.is_none()).map(|d| d.name.as_str()).collect();
    assert!(missing.is_empty(), "blocks without a creative tab: {missing:?}");
    assert!(reg.defs().filter(|d| d.id == 0 || d.kind == BlockKind::Liquid).all(|d| d.tab.is_none()), "air and liquids are not in the palette");
}
```

(Use the registry's existing iterator over its definitions; add `defs()` only if there is none.)

- [ ] **Step 2: Run** `just test-one game "--lib registry::"` — expected: FAIL, listing 166 names.

- [ ] **Step 3: Add the field and write `tab:` on each block in `blocks.ron`**, by these rules:
  - `Natural`: soils, sands, stones, ores, snow, ice, mud, peat, gravel, bedrock — what worldgen lays.
  - `Wood`: logs, branches, leaves, planks and anything else made of a tree.
  - `Colour`: every block that comes in the 16 dyes (concrete, terracotta, wool, stained glass).
  - `Functional`: blocks with behaviour (`chest`).
  - `Building`: every other made block (bricks, cut and polished stone, plain glass, metal blocks).

- [ ] **Step 4: Run** `just test-one game "--lib registry::"` — expected: PASS. Then `just smoke`
  (the client parses the manifest too) — expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/registry.rs crates/game/src/lib.rs assets/blocks/blocks.ron
git commit -m "feat(game): a creative tab on every placeable block"
```

### Task 5: The typing gate

**Files:**
- Create: `crates/client/src/renderer/text_entry.rs`
- Modify: `crates/client/src/renderer/cursor.rs` (`CursorPolicy`, `grabbed`, `blocks_game`,
  `reset_session_grab`), `crates/client/src/renderer/mod.rs` (plugin registration)

**Interfaces:**
- Produces:
  - `#[derive(Resource, Default)] pub struct TextEntry { pub on: bool }` (registered with `add_session_reset`)
  - `pub fn mute_keys_while_typing(entry: Res<TextEntry>, mut was_on: SessionLocal<bool>, mut keys: ResMut<ButtonInput<KeyCode>>, mut policy: ResMut<CursorPolicy>)` — in `PreUpdate`, `.after(bevy::input::InputSystems)`
  - `pub typing: bool` on `CursorPolicy`; `grabbed()` needs `!typing`, `blocks_game()` is true with it

- [ ] **Step 1: Write the failing tests** in `text_entry.rs` (`#[cfg(test)]`, a bare `App` with
  `ButtonInput<KeyCode>`, `TextEntry`, `CursorPolicy` and the system; drive the keys by hand).

```rust
#[test] fn off_leaves_the_keys_alone()                  // W pressed stays pressed and just_pressed
#[test] fn turning_on_releases_what_was_held_once()     // W held, on: that frame !pressed(W) && just_released(W); next frame neither
#[test] fn keys_pressed_while_on_never_show()           // on, press T: !pressed(T), !just_pressed(T)
#[test] fn a_key_held_through_stays_up_after_off()      // W held, on, off, W never re-pressed: !pressed(W) (Review Focus 1)
#[test] fn typing_frees_the_pointer_and_blocks_the_game() // policy.typing == on; first_person && typing: !grabbed(), blocks_game()
```

- [ ] **Step 2: Ask the user** for the go-ahead to run the client filters this plan names (Global
  Constraints), then run `just test-one client "--lib text_entry::"` — expected: FAIL (module missing).

- [ ] **Step 3: Implement.** The frame `on` rises: `keys.release_all()` and leave `just_released` for
  the frame's readers. Every later frame while on: `keys.release_all()` then `keys.clear()`. Write
  `policy.typing` only when it differs. `reset_session_grab` drops `typing` with the rest.

- [ ] **Step 4: Run** `just test-one client "--lib text_entry::"` and
  `just test-one client "--lib cursor::"` — expected: PASS. `just smoke` — expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/{text_entry,cursor,mod}.rs
git commit -m "feat(client): a typing gate — no key reaches the game while a text field has the keyboard"
```

### Task 6: The chat log and the command parser

**Files:**
- Create: `crates/client/src/renderer/chat/mod.rs`, `crates/client/src/renderer/chat/command.rs`
- Modify: `crates/client/src/renderer/mod.rs`

**Interfaces:**
- Produces (`chat/mod.rs`):
  - `pub const CHAT_LINE_MAX: usize = 256; pub const CHAT_LOG_LINES: usize = 100; pub const CHAT_SENT_HISTORY: usize = 50; pub const CHAT_RECENT_LINES: usize = 8; pub const CHAT_RECENT_SECS: f32 = 10.0;`
  - `pub enum ChatKind { Said, Info, Error }`
  - `pub struct ChatLine { pub kind: ChatKind, pub text: String, pub at: f64 }` (`at` = `Time::elapsed_secs_f64` when added)
  - `#[derive(Resource, Default)] pub struct ChatLog { pub lines: VecDeque<ChatLine>, pub sent: VecDeque<String>, pub open: bool, pub revision: u64 }` (`add_session_reset`)
  - `impl ChatLog { pub fn say(&mut self, text: &str, now: f64); pub fn info(&mut self, text: impl Into<String>, now: f64); pub fn error(&mut self, text: impl Into<String>, now: f64); pub fn remember_sent(&mut self, line: &str); pub fn clear(&mut self); pub fn recent(&self, now: f64) -> impl Iterator<Item = &ChatLine> }` — `say` is the one door ordinary chat goes through (the later Zulip seam); every mutation bumps `revision`
- Produces (`chat/command.rs`):

```rust
pub enum Command {
    Help,
    Creative(bool),                         // /creative, /normal, /gamemode creative|normal
    Give { item: game::ItemId, count: u32 },
    Adventure,
    Fortress,
    Where,
    Clear,
    #[cfg(feature = "dev_ui")] Complete,
    #[cfg(feature = "dev_ui")] DropBody,
    #[cfg(feature = "dev_ui")] Toggle(crate::renderer::dev_ui::DevToggle),
}
pub enum Submitted { Empty, Say(String), Run(Command), Bad(String) }
/// `item` resolves a name to an item (`game::item_named` in the app, a table in tests).
pub fn submit(line: &str, item: impl Fn(&str) -> Option<game::ItemId>) -> Submitted
pub const HELP: &[&str]   // one line per command, in the spec table's order
```

- [ ] **Step 1: Write the failing tests** (`#[cfg(test)]` in each file).

```rust
// command.rs
#[test] fn blank_is_empty()                             // "", "   "
#[test] fn plain_text_is_said_trimmed()                 // " hello there " -> Say("hello there")
#[test] fn a_long_line_is_cut_at_256_chars()            // 300 × 'é' -> Say of 256 chars (chars, not bytes)
#[test] fn gamemode_forms()                             // "/creative", "/gamemode creative", "/GAMEMODE Creative" -> Creative(true); "/normal", "/gamemode normal" -> Creative(false)
#[test] fn gamemode_needs_a_known_mode()                // "/gamemode", "/gamemode god" -> Bad naming the argument and "/help"
#[test] fn give_defaults_to_a_stack()                   // "/give stone" -> Give{stone, 64}; "/give stone 5" -> 5
#[test] fn give_refuses_bad_items_and_counts()          // "/give", "/give nope", "/give stone 0", "/give stone 65", "/give stone x" -> Bad
#[test] fn extra_arguments_are_refused()                // "/creative now" -> Bad
#[test] fn unknown_verb()                               // "/fly" -> Bad("Unknown command: /fly. Try /help")
#[test] fn a_lone_slash_is_unknown()
// mod.rs
#[test] fn the_log_keeps_the_last_100_lines()
#[test] fn sent_history_keeps_50_and_skips_a_repeat_of_the_last()
#[test] fn recent_is_the_newest_8_younger_than_10_s()
#[test] fn clear_empties_lines_but_keeps_sent_history()
```

- [ ] **Step 2: Run** `just test-one client "--lib chat::"` — expected: FAIL (module missing).

- [ ] **Step 3: Implement** both files. The parser is `split_whitespace` on the text after `/`, the
  verb and the mode names compared in ASCII lower case, item names as typed.

- [ ] **Step 4: Run** `just test-one client "--lib chat::"` — expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/chat/{mod,command}.rs crates/client/src/renderer/mod.rs
git commit -m "feat(client): the chat log and a command parser"
```

### Task 7: The chat box

**Files:**
- Create: `crates/slint-ui/ui/chat.slint`, `crates/client/src/renderer/chat/ui.rs`
- Modify: `crates/slint-ui/ui/game.slint`, `crates/slint-ui/ui/hud.slint`,
  `crates/client/src/renderer/chat/mod.rs` (plugin), `crates/client/src/renderer/slint_overlay.rs`
  (system ordering only)

**Interfaces:**
- Consumes: `ChatLog`, `Submitted`, `submit` (Task 6); `TextEntry` (Task 5).
- Produces:
  - Slint `export struct ChatRow { text: string, kind: int }` (0 said, 1 info, 2 error) and
    `export component ChatBox` in `chat.slint`: `in property <[ChatRow]> lines`, `in-out property <string> draft`,
    `callback accepted(string)`, `public function take-focus()`. The field is a raw `TextInput` in a
    styled rectangle, as `login.slint:88` does (`TextField` forwards no focus).
  - `GameUi`: `in property <bool> chat-open`, `in property <[ChatRow]> chat-lines`,
    `in-out property <string> chat-draft`, `callback chat-accepted(string)`,
    `public function chat-take-focus()`. The box is drawn `if root.chat-open`, bottom left, 40 % of the
    window wide, above the minimap.
  - `HudUi`: `in property <[ChatRow]> chat-recent`, drawn bottom left above the minimap, each row on a
    dark translucent strip.
  - `#[derive(Message)] pub struct ChatSubmitted(pub Submitted)` — written once per accepted line.
  - `pub fn open_wanted(open: bool, blocked: bool, t: bool, enter: bool, slash: bool) -> Option<&'static str>` —
    the draft to open with (`""` or `"/"`), `None` when already open, blocked, or no key.

- [ ] **Step 1: Write the failing tests** in `chat/ui.rs`.

```rust
#[test] fn t_and_enter_open_empty_slash_opens_with_a_slash()
#[test] fn nothing_opens_while_the_game_is_blocked_or_the_chat_is_open()
#[test] fn a_repeated_key_press_opens_nothing()         // pure fn opening_key(code, state, repeat) -> Option<..>: Enter with repeat == true -> None
#[test] fn history_walks_up_and_back_down_to_the_draft() // pure fn history_step(sent, index: Option<usize>, up: bool) -> Option<usize>
```

and one headless Slint test beside the existing inventory-screen test (find it with
`grep -rn inventory_grid_left crates/`), dispatching key events to a `GameUi`:

```rust
#[test] fn the_chat_field_takes_typed_text_and_reports_enter() // chat-open, chat_take_focus(), keys "h","i",Return: chat-accepted("hi") once
#[test] fn the_opening_key_is_not_typed()               // KeyPressed "t" dispatched BEFORE chat-open is set: draft stays ""  (Review Focus 5)
```

- [ ] **Step 2: Run** `just test-one client "--lib chat::"` — expected: the new tests FAIL.

- [ ] **Step 3: Implement.**
  - `chat.slint`, the `GameUi` and `HudUi` additions.
  - Systems in `chat/ui.rs`, all `Update`, all **after** `slint_input` (so the opening key press has
    already been dispatched to Slint before the field has focus) and `open_chat` **before** `menu_keys`:
    - `open_chat`, first of the three: reads `MessageReader<KeyboardInput>` for a `Pressed` event with
      `repeat == false` on `KeyT`, `Enter` or `Slash` (not `ButtonInput`: a held Enter's key repeat
      would press it again and reopen the chat it just submitted), with
      `blocked = policy.blocks_game() || inventory open`; on open sets `ChatLog.open`, `TextEntry.on`,
      the draft, and calls `chat_take_focus()`. It runs before `chat_accept`, so the Enter that submits
      is seen while the chat is still open and opens nothing.
    - `chat_keys`, while open: reads `MessageReader<KeyboardInput>` (not `ButtonInput`, which the gate
      has emptied) for Escape (close, drop the draft), ArrowUp / ArrowDown (`history_step`).
    - `chat_accept`: drains the `chat-accepted` queue (an `Rc<RefCell<Vec<String>>>` non-send resource,
      as `InventoryClicks` is), runs `submit(line, game::item_named)`, writes `ChatSubmitted`, calls
      `remember_sent` for anything not `Empty`, adds a `Say` line with `ChatLog::say`, closes.
    - `sync_chat`: on a changed `revision` or `open` sets `chat-lines` (all lines) and, each frame it
      differs, `chat-recent` (`ChatLog::recent`; empty while open).
  - Closing sets `TextEntry.on = false`.

- [ ] **Step 4: Run** `just test-one client "--lib chat::"`, `just smoke`, `just slint-wasm` —
  expected: PASS.

- [ ] **Step 5: Look at it.** `just run`, press T, type `hello`, Enter: the line shows bottom left and
  goes after 10 s. Hold W, press T: the camera stops; Esc closes the chat and the menu does not open.
  Say what was seen.

- [ ] **Step 6: Commit**

```bash
git add crates/slint-ui/ui/{chat,game,hud}.slint crates/client/src/renderer/chat/{mod,ui}.rs crates/client/src/renderer/slint_overlay.rs
git commit -m "feat(client): the chat box — T opens, lines kept for the session"
```

### Task 8: Running commands

**Files:**
- Create: `crates/client/src/renderer/chat/run.rs`, `crates/client/src/renderer/commander/creative.rs`
- Modify: `crates/client/src/renderer/chat/mod.rs`, `crates/client/src/renderer/commander/mod.rs`,
  `crates/client/src/renderer/slint_overlay.rs` (expose what the Adventure buttons at `:1391` and
  `:1444` send as two functions)

**Interfaces:**
- Consumes: `ChatSubmitted`, `Command`, `HELP` (Tasks 6, 7); `Order::SetCreative`,
  `UiAction::CreativeGive`, `UiUpdate::Creative` (Tasks 1, 3); `dev_ui::apply_dev_action`.
- Produces (`commander/creative.rs`):
  - `#[derive(Resource, Default, Clone, Copy, PartialEq, Eq)] pub struct ClientCreative(pub bool)` —
    set from `UiUpdate::Creative` where `ClientPlayMode` is set from `UiUpdate::PlayMode`;
    `add_session_reset`
  - `pub fn creative_order(on: bool, mode: PlayMode, cam: &CommanderCamera, home: Option<RegionId>) -> game::Order` —
    `at` is `Some(focus tile clamped into the home region)` only for `on && mode == Fortress`, built as
    `adventure::toggle_order` builds its `at`
- Produces (`chat/run.rs`):
  - `pub enum Effect { Sim(game::GameEventKind), Request(crate::ClientRequest), Info(String), Error(String), ClearLog, #[cfg(feature = "dev_ui")] Dev(DevAction) }`
  - `pub struct RunContext { pub client: game::ClientId, pub mode: PlayMode, pub creative: bool, pub creative_order: game::Order /* for the opposite state */, pub home: Option<RegionId>, pub body_block: Option<[i64; 3]> }`
  - `pub fn effects(cmd: Command, ctx: &RunContext) -> Vec<Effect>` — pure
  - `pub fn run_commands(...)` — the system: builds the context, applies the effects

- [ ] **Step 1: Write the failing tests** in `chat/run.rs` and `commander/creative.rs`.

```rust
// run.rs
#[test] fn creative_sends_the_order_and_says_so()       // Creative(true), not creative: [Sim(Order(7, SetCreative{on:true,..})), Info("Creative mode")]
#[test] fn creative_when_already_creative_only_says_so() // [Info("Already in creative mode")], nothing sent
#[test] fn normal_sends_off()                           // [Sim(SetCreative{on:false, at:None}), Info("Normal mode")]
#[test] fn give_outside_creative_is_refused_locally()   // [Error("/give needs creative mode")]
#[test] fn give_in_creative_sends_creative_give()       // [Sim(UiAction(7, CreativeGive{stone,5})), Info("Gave 5 stone")]
#[test] fn help_prints_every_help_line()
#[test] fn where_prints_region_and_block()              // Info("Region (0, 0), block 12 64 -3"); Info("No body here") without one
#[test] fn clear_clears()
#[test] fn adventure_and_fortress_send_what_the_buttons_send()
// creative.rs
#[test] fn fortress_entry_carries_the_focus_tile()      // on, Fortress, focus in region: at == Some(focus_tile)
#[test] fn a_focus_outside_home_is_clamped_into_it()
#[test] fn adventure_entry_and_every_exit_carry_no_tile()
```

- [ ] **Step 2: Run** `just test-one client "--lib chat::"` — expected: the new tests FAIL.

- [ ] **Step 3: Implement.** `run_commands` reads `ChatSubmitted`: `Bad(text)` → `ChatLog::error`;
  `Run(cmd)` → `effects`, then `Sim` → `GameEvents.0.send`, `Request` → the channel the Adventure
  buttons use, `Info` / `Error` → the log, `ClearLog` → `ChatLog::clear`, `Dev` → `DevActions` queue.
  With no local player: one error line, "Not in a world".

- [ ] **Step 4: Run** `just test-one client "--lib chat::"`, `just smoke` — expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/chat/{mod,run}.rs crates/client/src/renderer/commander/{mod,creative}.rs crates/client/src/renderer/slint_overlay.rs
git commit -m "feat(client): chat commands — gamemode, give, adventure, fortress, where, clear, dev actions"
```

### Task 9: Creative in the client's view

**Files:**
- Modify: `crates/client/src/renderer/commander/first_person.rs` (`wanted`, `toggle_first_person`),
  `crates/client/src/renderer/input.rs:167`, `crates/client/src/renderer/slint_overlay.rs:634` (the
  hotbar condition) and the Esc / E handling around `:1177-1200`,
  `crates/client/src/renderer/commander/designate.rs:448`, `commander/move_order.rs:264`

**Interfaces:**
- Consumes: `ClientCreative` (Task 8).
- Produces: `pub fn wanted(on: bool, toggled: bool, adventure: bool, creative: bool) -> bool`

- [ ] **Step 1: Change the tests** in `first_person.rs`: `first_person_only_in_adventure` gains the
  fourth argument (`false`, same assertions), and add:

```rust
#[test]
fn creative_is_always_first_person() {
    assert!(wanted(false, false, false, true), "creative enters by itself, in Fortress too");
    assert!(wanted(true, true, true, true), "the key does not leave it");
    assert!(!wanted(true, false, false, false), "leaving creative in Fortress leaves the eyes");
    assert!(wanted(true, false, true, false), "leaving creative in Adventure keeps them");
}
```

- [ ] **Step 2: Run** `just test-one client "--lib first_person::"` — expected: FAIL (arity).

- [ ] **Step 3: Implement.**
  - `wanted`: `creative || (adventure && (on != toggled))`.
  - `toggle_first_person`: an entry caused by creative takes the pointer at once
    (`policy.released = false`); the rest of the entry / exit code is unchanged.
  - `input.rs:167`: E reaches the sim when `adventure || creative`.
  - `slint_overlay.rs:634`: the hotbar shows when `(adventure || creative) && first person`.
  - Designation tools and move orders stay off while creative (`designate.rs:448`,
    `move_order.rs:264` treat creative as they treat first person; check each already stands aside
    through `first_person::inactive` before adding a condition).

- [ ] **Step 4: Run** `just test-one client "--lib first_person::"`, `just smoke` — expected: PASS.

- [ ] **Step 5: Look at it**, in both modes. `just run`: `/creative` → the view drops into the eyes at
  the camera's focus, the pointer is held, the hotbar shows, left click breaks at once, double-tap
  Space flies; `/normal` → back over the body with the commander camera. `client debug embarked
  --adventure` (see `just --list` for the recipe): `/creative` → flight and instant break; `/normal` in
  the air → the body falls and mining takes time again. Say what was seen.

- [ ] **Step 6: Commit**

```bash
git add crates/client/src/renderer/commander/{first_person,designate,move_order}.rs crates/client/src/renderer/{input,slint_overlay}.rs
git commit -m "feat(client): creative is first person, in Fortress and Adventure"
```

### Task 10: The tabbed creative inventory

**Files:**
- Create: `crates/client/src/renderer/creative_palette.rs`
- Modify: `crates/slint-ui/ui/inventory.slint`, `crates/slint-ui/ui/game.slint`,
  `crates/client/src/renderer/slint_overlay.rs` (`sync_inventory_screen` at `:739`, the callback wiring
  at `:833`), `crates/client/src/renderer/mod.rs`

**Interfaces:**
- Consumes: `CreativeTab`, `BlockDef.tab` (Task 4); `UiAction::CreativeTake / CreativeGive / CreativeTrash`
  (Task 3); `ClientCreative` (Task 8); `TextEntry` (Task 5); `item_icons::icon`.
- Produces (`creative_palette.rs`):
  - `pub const PALETTE_TABS: [&str; 7] = ["All", "Natural", "Wood", "Building", "Colour", "Functional", "Items"];`
  - `pub fn tab_items(registry: &game::BlockRegistry, tab: usize) -> Vec<game::ItemId>` — id order; tab 0 = blocks then extras
  - `pub fn filtered(items: &[game::ItemId], search: &str, name: impl Fn(game::ItemId) -> String) -> Vec<game::ItemId>` — case-insensitive substring, `_` and space equal
  - `pub fn palette_click(item: game::ItemId, right: bool, shift: bool, cursor_held: bool) -> game::UiAction`
- Produces (Slint): `InventoryScreen` gains `in property <bool> creative`, `in property <[string]> palette-tabs`,
  `in-out property <int> palette-tab`, `in property <[ItemSlot]> palette-slots`,
  `in-out property <string> palette-search`, `out property <bool> palette-search-focused`,
  `callback palette-tab-changed(int)`, `callback palette-pressed(int /*index*/, bool /*right*/)`,
  `callback palette-search-edited(string)`; the same seven on `GameUi` prefixed `inventory-`.

- [ ] **Step 1: Write the failing tests** in `creative_palette.rs`.

```rust
#[test] fn every_placeable_block_is_in_exactly_one_named_tab_and_in_all() // real manifest: tabs 1..=5 partition the blocks of tab 0
#[test] fn air_and_water_are_in_no_tab()
#[test] fn the_items_tab_is_the_extra_items()           // [STICK]
#[test] fn search_matches_names_loosely()               // "oak pl" finds oak_planks; "OAK" finds every oak block; "" keeps all
#[test] fn clicks_map_to_actions()                      // empty cursor: left -> CreativeTake{64}, right -> CreativeTake{1}, shift -> CreativeGive{64}; held cursor: any -> CreativeTrash
```

- [ ] **Step 2: Run** `just test-one client "--lib creative_palette::"` — expected: FAIL.

- [ ] **Step 3: Implement.**
  - `creative_palette.rs`, with `#[derive(Resource, Default)] struct PaletteState { tab: usize, search: String, shown: Vec<ItemId> }`
    (`add_session_reset`).
  - `inventory.slint`: when `creative` and there are no container slots, a palette block above the
    main grid — the `Tabs` widget (`controls.slint:241`) over a `Flickable` holding the 9-column grid
    of `SlotCell`s (5 rows visible), a raw `TextInput` search field on the tab row after `tabs-end`.
    The frame's title is "Creative". `chest-height` and the `*-top` out properties account for the
    palette's height so the existing headless slot-position test still holds for a non-creative screen.
  - `sync_inventory_screen`: sets `creative` from `ClientCreative`; rebuilds `palette-slots` (icons,
    `filled: true`, empty count) only when the tab, the search or the creative flag changed.
  - Palette presses → `palette_click` → `GameEventKind::UiAction`, Shift read as `inventory_clicks`
    (`:704`) reads it.
  - `TextEntry.on` follows `palette-search-focused` while the inventory is open (and the chat is
    closed); closing the inventory clears the search and the flag. Esc with the search focused closes
    the inventory through the existing `CloseInventory` path: read it from `KeyboardInput` there, as
    `chat_keys` does.

- [ ] **Step 4: Run** `just test-one client "--lib creative_palette::"`, the existing inventory-screen
  headless test, `just smoke`, `just slint-wasm` — expected: PASS.

- [ ] **Step 5: Look at it.** `just run`, `/creative`, E: seven tabs; pick stone from Natural onto the
  hotbar, close, place it — the stack does not shrink. Type `wool` in the search: only wools. Type `e`
  in the search: the inventory stays open. In normal mode E in Adventure shows no palette. Say what was
  seen.

- [ ] **Step 6: Commit**

```bash
git add crates/client/src/renderer/{creative_palette,slint_overlay,mod}.rs crates/slint-ui/ui/{inventory,game}.slint
git commit -m "feat(client): a tabbed creative inventory with search"
```

### Task 11: Wide checks and the written record

**Files:**
- Modify: `CLAUDE.md` (Client section: one bullet for the typing gate and the chat, one clause for
  creative), `TODO.md`, `docs/superpowers/specs/2026-09-22-chat-and-commands-investigation.md` (a
  status line under the title: "2026-11-14: a client-only chat and commands were built first —
  `specs/2026-11-14-creative-mode-and-chat-design.md`; this document is the networked direction."),
  `docs/game-design.md` (creative mode under what is decided)

- [ ] **Step 1: Run the wide checks**, saying so first: `just determinism-wasm`; `just test-threads`
  (a `Client` field crosses seams); the `game` crate's whole non-client suite once (`just test-one game ""`);
  `just test-one server "--test it"` for the player and region store formats; `just clippy`;
  `just smoke`. Expected: all PASS.
- [ ] **Step 2: Two clients.** `just run-two`: one goes `/creative` and places blocks; the other sees
  them appear and stays in step for five minutes (no desync line in either log, the state-hash interval
  is left as it is). Say what was seen.
- [ ] **Step 3: Write the four doc changes.** `CLAUDE.md` had uncommitted edits of the user's when this
  plan was written: if it still has, stage only this task's hunks (`git add -p` is interactive and not
  available here — write the hunk with `git apply --cached`), or leave `CLAUDE.md` unstaged and say so.
- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md TODO.md docs/game-design.md docs/superpowers/specs/2026-09-22-chat-and-commands-investigation.md
git commit -m "docs: creative mode and the client-side chat"
```
