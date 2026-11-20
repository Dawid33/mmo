# Creative mode and chat: design

Status: written 2026-11-14 and **implemented as editor mode on 2026-11-20**, with the permission
decision refreshed below. The original **(call)** notes are historical. The plan
(`docs/superpowers/plans/2026-11-14-creative-mode-and-chat.md`) argues from this file.

## As built

- The public name is **editor mode**; the sim interfaces are `Client.editor`,
  `Order::SetEditor`, `UiUpdate::Editor` and `UiAction::Editor*`.
- Editor permission is not a client-controlled flag. The coordinator reads the world's
  `editor_grants` table for the authenticated account at login and sends one authoritative
  `editor_allowed` bit in the spawn order. Revocation therefore applies at the next join.
- A process-owned local world has no coordinator/account grant boundary: the inline
  `LocalServer` and trusted `AuthMode::Local` player admission set `editor_allowed=true`.
  Observer redirects and JWT/login admission remain grant-controlled; local mode does not widen
  them.
- `/creative`, `/normal`, `/gamemode creative|normal` and `/give <item> [count]` are parsed locally.
  They send the existing authoritative `Order` / `UiAction`; crafted events are still rejected by
  the sim. Ordinary chat text remains only in the local `ChatLog`.
- The palette metadata is `CreativeTab` in `blocks.ron`. Air and liquids are absent; extra items
  are in the client-side Items tab. Protocol 15 carries the editor-bearing packet shapes.

## What was asked

- **Creative mode**, switchable against normal play, inside both Fortress and Adventure. It works like
  Minecraft's: a first-person camera and a creative inventory to pick blocks and items from. The
  inventory is tabbed.
- **Chat**, in the client only for now. A line that is a command makes the game do things. Any other
  line is ordinary chat and lives in the client's memory only. Zulip integration comes later and is not
  part of this work.

## What exists today

- `game::PlayMode { Fortress, Adventure }` is per-client rollback state (`Client.mode`). Every client
  has a body in both modes; in Fortress it stands inert while the commander camera flies free.
- Half of creative mode is already in the sim, left from the old first-person client: a Fortress client
  with `fps_cam_mode` breaks and places instantly (`edit.rs` `player_edit_safe`, always placing
  `PLACE_BLOCK` = dirt) and may fly (`camera.rs:303`, double-tap Space, still collides). Adventure
  clients instead mine with break times and place from the hotbar (`mining.rs`).
- The first-person view (`commander/first_person.rs`, F5) exists only in Adventure. The inventory
  screen, the hotbar strip and item icons exist (`inventory.slint`, `hud.slint`, `item_icons.rs`).
- Blocks: 168 in `assets/blocks/blocks.ron`, with no category. One non-block item, `stick`.
- No chat, no command parser, no text field in the in-game UI, and no "the UI has the keyboard" gate:
  keys reach the sim and the hotkeys even while a Slint overlay is open.
- No permissions of any kind. `Order::DevCompleteAll` and the other dev orders are open to any client.
- `docs/superpowers/specs/2026-09-22-chat-and-commands-investigation.md` investigated server-side chat
  and commands as sim events parsed with `clap`. Nothing of it was built. This design does the
  client-only step the user asked for and keeps that document as the later, networked direction.

## Goals

1. `/creative` and `/normal` in the chat switch the local player, in either play mode, and the switch
   survives a seam crossing, a host hand-off and a save.
2. In creative the player is in the body's eyes, flies, breaks instantly, and places the selected
   hotbar block without using it up.
3. E opens the inventory with a tabbed palette of every placeable block and every item above it.
4. T opens a chat box. Lines are kept for the session; commands run; while typing, no key reaches the
   game.

## Non-goals

- Sending chat to anyone, names, a server console, Zulip.
- Permissions. **(call)** Anyone may switch to creative, as anyone may already send the dev orders; the
  2026-09-22 decision "everyone has admin for now" stands. A public server needs a gate before this
  ships there; see Open questions.
- Noclip, `/tp`, time of day, `/fill`, `/setblock`: none has sim support today.
- Pick-block (middle click), a held break that repeats, tab completion.

## Anti-goals

- A second way to change the world that is not a tick-stamped sim event. Every command that changes
  sim state sends an event the client predicts and the server replays, like a click.
- A key typed into the chat that also walks the body, opens the menu or toggles a debug view.

## Creative mode

### Sim (`crates/game`)

- `Client.creative: bool`, rollback state, hashed. It travels with the player (`Client::arrive` keeps
  it) and is saved in the `PlayerRecord` (`parked_client` keeps it).
- `Order::SetCreative { on: bool, at: Option<TilePos> }`, appended to `Order`. `at` is where to put the
  body when turning creative **on**; the client sends it only in Fortress (the tile under the commander
  camera's focus, as `SetMode` does for Adventure) so the eyes open where the player was looking. In
  Adventure the body stays where it stands.
- Turning it off clears flight (`Motion.flying`), closes the inventory as `leave_survival` does when
  the play mode is Fortress, and leaves the body where it is.
- **(call)** One inventory for both: creative neither consumes nor needs items, and what was taken in
  creative is still there in normal play, as in Minecraft. Kept simple on purpose; with no permissions
  there is nothing to protect yet.
- "Has hands" becomes `Client::hands()`: `mode == Adventure || creative`. The E key (inventory), the
  digit keys and the wheel (hotbar) and item pickup follow it instead of `mode == Adventure`.
- Edits. A creative client with the body driven and no inventory open:
  - left press: instant break, no drop (`Intent::BreakBlock { survival: false }`), as today's Fortress
    path;
  - right press: a chest under the crosshair opens; otherwise the selected hotbar slot's block is
    placed against the hit face, not consumed (`Intent::PlaceBlock { survival: false }`). An empty slot
    or a non-block item places nothing.
  - It never takes the survival path (no mining timer, no throw with Q: **(call)** Q still throws the
    selected stack, since throwing is how the inventory is cleared by hand).
- The legacy path (Fortress, not creative, `fps_cam_mode`, places dirt) stays as it is: the headless
  harnesses, the autopilot and `block_edit.rs` use it.
- Flight: `allow_flight` becomes `creative || mode != Adventure`.
- Palette actions, appended to `UiAction`, both refused unless `creative`, the item exists and
  `1 <= count <= MAX_STACK`:
  - `CreativeTake { item, count }`: the cursor stack becomes that stack (what it held is destroyed).
    Needs the inventory open.
  - `CreativeGive { item, count }`: `inventory.add`; what does not fit is dropped.
  - `CreativeTrash`: the cursor stack is destroyed. Needs the inventory open.
- An item exists when it is a block of the registry that is not air and not a liquid, or an entry of
  `EXTRA_ITEMS`. `game::item_named(&str) -> Option<ItemId>` is the lookup `/give` uses.
- `UiUpdate::Creative(bool)` tells the client, sent only on change, like `UiUpdate::PlayMode`.

### Tabs

**(call)** The tab is data on the block: `BlockDef.tab: Option<CreativeTab>` in `blocks.ron`, with
`CreativeTab { Natural, Wood, Building, Colour, Functional }`. Air and liquids have none and are not in
the palette. A test fails for any other block without one, so a new block is classified where it is
declared. The palette's tabs are: All, Natural, Wood, Building, Colour, Functional, Items (the
`EXTRA_ITEMS`). A search field above the grid filters the open tab by name.

### Client

- `ClientCreative(bool)` mirrors `UiUpdate::Creative`.
- Creative forces first person: `first_person::wanted` is true whenever creative is on, and F5 does
  nothing then. Leaving creative in Fortress returns to the commander camera over the body (the code
  that leaves first person today); in Adventure the view stays in the eyes until F5.
- The pointer is taken at once on entering creative (no "click to take it", since the player just asked
  for it in the chat).
- The hotbar strip and the crosshair show in creative as they do in Adventure first person.
- The inventory screen grows a palette above the player's grid when creative is on and no chest is
  open: the `Tabs` widget (`controls.slint`), a search field, a scrolling 9-column grid of `SlotCell`s.
  Left click on a palette cell with an empty cursor takes a full stack, right click takes one, shift
  click gives a stack to the inventory; any click with a held stack trashes it.

## Chat

### Behaviour

- T or Enter opens the box empty; `/` opens it with the slash typed. Esc closes it and keeps nothing.
  Enter submits and closes. Up / Down walk the lines sent this session. A line is at most 256
  characters.
- A submitted line starting with `/` is parsed and run; anything else is added to the log as the
  player's own line. An empty line only closes the box.
- The log holds the last 100 lines of the session and is dropped with the session. While the box is
  closed the newest lines (at most 8) show for 10 s each, bottom left, drawn in the pass-through HUD so
  they never take a click. While it is open the scrollback and the input field show.
- The box opens from the commander camera and from first person, never while a menu, the map, the
  inventory or the loading screen is up.

### Typing gate

One resource, `TextEntry { on: bool }`, says a Slint text field has the keyboard (the chat box, the
palette's search field). While it is on:

- `ButtonInput<KeyCode>` is emptied before any game system reads it, each frame, at one point
  (`PreUpdate`, after Bevy's input systems). The frame it turns on, every held key is released first, so
  the sim gets its key-ups and the body stops.
- `CursorPolicy.typing` frees the pointer and makes `blocks_game()` true.
- Slint still gets every key: it reads `KeyboardInput` messages, not `ButtonInput`. The chat's own
  Esc / Up / Down are read from those messages too.

### Commands

Parsed in the client by a small hand-written parser into a typed `Command` (no `clap`: a handful of
verbs, and its size in the wasm build). Running one either sends the same sim event a button would, or
changes client state. Feedback is a local line, printed once, when the command is sent.

| Command | Does |
|---|---|
| `/help` | lists the commands |
| `/gamemode creative\|normal`, `/creative`, `/normal` | `Order::SetCreative` |
| `/give <item> [count]` | `UiAction::CreativeGive`; count 1..=64, default 64; refused locally outside creative |
| `/adventure`, `/fortress` | what the Adventure tab's start and End-adventure buttons send |
| `/where` | prints the home region and the body's block position |
| `/clear` | empties the log |
| `/complete`, `/dropbody`, `/toggle <name>` | the Debug tab's actions (`dev_ui` builds only) |

An unknown verb or a bad argument prints one error line naming it and `/help`; nothing is sent.

### Later: Zulip

Ordinary lines go through one function (`ChatLog::say`). That is the place a transport is put when chat
leaves the client; nothing else is prepared for it now.

## Approaches considered

1. **Creative as a third `PlayMode`.** Rejected: the user wants it inside both Fortress and Adventure,
   and `PlayMode` is tied to sites, claims and the manager's `fortress_of` / `adventurer_of`.
2. **Creative as a client-only switch over the existing Fortress first-person path.** Cheapest, but the
   sim decides who edits instantly and who flies, the server replays it, and it could not exist in
   Adventure. Rejected.
3. **A per-client sim flag beside `PlayMode`** (chosen). Small, predicted and replayed like every other
   input, and it travels and saves with the `Client` that already carries the inventory.

For commands: sim-side typed commands with rollback-exact feedback (the 2026-09-22 investigation)
against client-side parsing onto existing events (chosen, as asked: "for now it is client side"). The
commands that change the world still go through sim events, so moving the parser to the server later
changes where a line is read, not what it does.

## Constraints

- `game` stays free of Bevy. New `Order` / `UiAction` / `UiUpdate` variants are appended (bincode
  indices). A new `Client` field is classified in `Client::arrive` and `parked_client`, changes the
  state hash and the saved formats: follow commit `d36a0e7f` (the last field added) for the goldens and
  the format constants.
- Slint only; no root-level `visible` / `opacity`; new per-world client state is registered with
  `add_session_reset` or `SessionLocal`.
- `CursorPolicy` stays the only cursor writer.
- F5 is also the chunk-grid toggle (`grid_debug.rs:23`), an existing conflict this work leaves alone.

## Open questions for the user

1. **Who may use creative on a shared server?** Today: anyone. A server rule would have to be sim
   state so prediction agrees with the server.
2. **One inventory or two?** Chosen: one. Minecraft does the same; a separate survival inventory is
   more state to carry and save.
3. **Tabs as block data** (chosen) or a name-pattern table in the client (no manifest edit, but new
   blocks land in a catch-all tab unnoticed)?
