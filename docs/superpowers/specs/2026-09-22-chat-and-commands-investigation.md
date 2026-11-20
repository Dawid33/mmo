# In-game chat + slash commands (Minecraft-like): investigation

Status: investigation, 2026-09-22; superseded for the client-local MVP by the editor/chat
implementation of 2026-11-20. A session-local Slint chat log, parser and typing gate now exist,
including local editor commands. Ordinary chat still has no transport or journal. The networked
manager-thread design below remains the direction for shared chat, not current behavior.

## TL;DR

- **Nothing exists today.** No chat, command, console, username or auth code
  anywhere. `ClientId` is a per-connection `AtomicUsize` counter
  (`server/src/lib.rs:207`), and TLS runs `with_no_client_auth`.
- **Chat must not be a sim event.** It is not deterministic state, it must
  never be rolled back or re-simulated, and it must not grow a region's event
  log. It belongs on the **manager thread**, as a new `ClientPacket` /
  `ServerPacket` pair handled next to `RequestPlayerRegion`
  (`world_manager.rs:412-420`). The `None` recipient already broadcasts to
  everyone (`server/src/lib.rs:168-203`).
- **Commands are simulation events (decided 2026-09-22).** Running a
  command can change the world, so it must sit in the same rollback timeline
  as the inputs around it. `GameEventKind::Command(CommandSource, Command)`,
  where `Command` is a **typed enum**, not a string:
  - The client predicts it and sends it as `GameEventAfterTick`, like
    `PlayerInput` / `UiAction`.
  - The manager routes it to the player's home region.
  - The region executes it deterministically inside `handle_event`, with
    undo-logged mutations. It is rolled back and
    re-simulated like any other event.
  - Feedback text is **sim state**, emitted through the `#[emit]` machinery
    in both apply and undo directions, so a re-simulation never prints a
    line twice.
  - Only social verbs (`/msg`, `/me`, `/say`) stay on the chat path, since
    they cannot change the world.
- **Parsing uses `clap` (derive), at the edge, not in the sim** (§2
  "Parser choice"). The typing client and the server console parse the line
  into the `Command` enum. Parse errors stay local to the typist and are
  never sent. The sim only ever sees already-typed commands.
- **The client UI is plain `bevy_ui` + `KeyboardInput`.** There is no egui
  and no text-input widget today.
  - T opens chat, `/` opens it with the slash typed in.
  - While the box is open, the input gates apply: movement forwarding stops,
    held keys are released, F3/F5/E are ignored, and the cursor is released.
- **Identity (decided 2026-09-22):** the server assigns placeholder names
  (`Player<ClientId>`) until Postgres accounts exist, so no name handshake is
  needed. Chat is **global** and **every player has admin** for now (§4, §8).
- Four existing hazards bite chat and commands directly and should be fixed first or
  alongside:
  - client→server packet reordering
  - any client can stop the server with `Quit`
  - unbounded `read_to_end` + `unwrap` on every client stream
  - a client can send events as another client (the manager trusts the id
    inside the event)

## 1. What Minecraft does, and what to take

| Minecraft (Java, 1.13+) | Take? | Notes |
|---|---|---|
| Separate packets: chat message vs chat **command** (1.19+ sends commands without the `/`) | **Yes** | Keeps "is this a command" out of string sniffing on the server, and lets commands skip the chat rate limit or use a different one. |
| **Brigadier**: a command tree of `literal` / `argument<T>` nodes, each with `requires(source -> bool)` and `executes`. Typed argument parsers (int ranges, block pos with `~` / `^` relative coords, entity selectors, resource ids). | **Typed result, not the tree** | Use `clap` derive into a typed `Command` enum (§2 "Parser choice"). A hand-rolled Brigadier tree remains the upgrade path if completion needs exact per-argument offsets. |
| Server sends the whole command tree (`Commands` packet) on join and after a permission change; the client completes and validates locally | **Yes** | We have the stronger version: client and server share the `game` crate. Sending the tree still matters for permission filtering and mod-registered commands. |
| `TabComplete` request/response with a transaction id, for suggestions only the server knows (online player names, entity ids) | **Yes, v2** | Player names can also be pushed as a small roster, which covers most cases. |
| Permission levels 0–4 (op levels); console = 4; `ops.json` | **Yes, simplified** | `enum PermissionLevel { Player, Moderator, Admin, Console }`, with the admin list read from a server config file. |
| Target selectors `@p @a @s @e[type=…,distance=..5]` | **Later** | Fits the entity-generic direction (players are entities like NPCs). v1: `@s` and player names only. |
| Rich text components (JSON: colour, bold, click-to-run, hover) | **Minimal** | `Vec<Span { text, style }>` with a small palette. Click events can wait. |
| Chat HUD: last ~10 lines fade after 10 s, 100-line scrollback, ↑/↓ sent-history, 256-char limit | **Yes** | Straight UX copy. |
| System vs player vs whisper message kinds; `/msg` `/me` `/say` | **Yes** | `ChatKind` enum. |
| Server console runs the same commands with source = console | **Yes** | stdin reader thread → `ServerEvent::Console(line)`. |
| Spam kick (chat counter decays; kick over threshold) | **Yes** | Token bucket per session on the manager. |
| 1.19 **signed chat** + chat reporting to Mojang | **No** | Needs account keys we don't have. It was controversial because it moved moderation off the server operator. Server-side logging covers the moderation need. |

## 2. Where it lives in this codebase

### Why chat (but not commands) stays out of `GameEventKind`

A region event is logged, replayed on rollback, hashed and echoed to every
subscriber of that region. Chat has none of those properties:

- It spans regions (a player in region A talks to a player in B).
- Its order relative to ticks is irrelevant.
- Re-running it on rollback would duplicate messages.

It also must not affect `hash(state)`. So it bypasses `RegionInput` entirely.

### Wire additions (`game/src/protocol.rs`)

```rust
// client → server
ClientPacket::Chat { seq: u32, text: String }        // chat + social verbs /msg /me /say
// world commands are NOT a packet: GameEventKind::Command rides GameEventAfterTick (§2)
ClientPacket::Suggest { tx: u32, line: String, cursor: u16 }  // v2

// server → client
ServerPacket::Chat(ChatLine)
ServerPacket::CommandTree(CommandTreeWire)            // on join + on permission change
ServerPacket::Roster(Vec<(ClientId, String)>)         // names for completion + display
ServerPacket::Suggestions { tx: u32, range: (u16, u16), items: Vec<String> }  // v2

struct ChatLine { kind: ChatKind, from: Option<ClientId>, body: Vec<Span> }
enum ChatKind { Player, System, Whisper, Emote, CommandFeedback, Error }
```

**`seq` is needed**, because native QUIC can reorder client→server packets:

- The server spawns one task per incoming uni stream
  (`server/src/lib.rs:416-428`), so two quick messages can swap.
- The WebTransport path reads its streams in a loop, and the client read
  paths are serial, so neither reorders.
- The manager keeps `next_seq` per session and holds out-of-order lines
  briefly, or simply sorts within a short window. The cleaner fix, a
  per-connection ordered reader for non-sim packets, is described in §6.

Everything goes through `encode_packet` / `decode_packet` as the conventions
require. Chat lines are far below the 4 KiB compression threshold.

### Server (manager thread)

Add a `ChatHub`, owned by the `WorldManager` or next to it, keeping it
Bevy-free:

- `names: BTreeMap<ClientId, String>`
- `perms: BTreeMap<ClientId, PermissionLevel>`
- `rate: BTreeMap<ClientId, TokenBucket>`
- the `CommandRegistry`

`handle_server_event` then does three things:

- **`Chat`:**
  - Validate: at most 256 chars, strip control chars, not empty.
  - Rate-limit.
  - Log to the server log with name and id (the moderation trail).
  - Broadcast `(None, ServerPacket::Chat(..))`.
- **Social verbs on the chat path** (`/msg`, `/me`, `/say`): parse the verb,
  deliver as a `Whisper` / `Emote` / `System` line.
- **`GameEventKind::Command`:** not handled here. It is routed by `homes`
  exactly like `PlayerInput` / `UiAction` (`world_manager.rs:430`); see below.
- **Console (stdin) commands:** the manager wraps the line as
  `GameEventKind::Command` with a console source and injects it with
  `RegionInput::Event` into the target region. That is the target player's
  home, or the spawn region when no player is named. Like `CreateClient`,
  it is server-originated and never predicted.
- **`ClientConnected` / `ClientDisconnected`:** assign the placeholder name,
  push the `Roster`, and broadcast "X joined the game" / "X left the game".

`LocalServer` (offline wasm) runs the same `WorldManager`, so chat and
commands work there with no extra code, and the single player is Console
level.

### Commands run in the simulation

```rust
GameEventKind::Command(CommandSource, Command)
enum CommandSource { Player(ClientId), Console }   // Console never accepted from a packet
enum Command { Tp { x: Coord, y: Coord, z: Coord }, SetBlock { .. }, Fill { .. }, Give { .. }, Fly { on: Option<bool> }, .. }
enum Coord { Abs(f32), Rel(f32) }                   // `~` / `~1.5`, resolved at execution time
```

**Why this shape works here.** `UiAction` is the precedent in every
respect:
- It is client-predicted.
- The manager routes it by `homes` (`world_manager.rs:430`).
- It is applied in `Region::handle_event` (`region.rs:555`) with a mutation
  proven to invert bit-exact (`region.rs:689`).
- Its visible output is derived from state (`diff_ui`), not fired as
  one-shot events.

A command is the same thing with a bigger vocabulary.

**Flow:**
1. Client, on Enter with a `/` draft: parse locally with clap into
   `Command`. A parse error is shown in the chat pane and nothing is sent.
   If it parses, push `GameEventKind::Command(Player(me), cmd)` into the
   sim channel. It goes
   out as `ClientPacket::GameEventAfterTick` and is applied to the
   predicted region immediately.
2. Server: the manager routes it to `homes[cid]` as
   `RegionInput::EventAfterTick`. The region runner holds it to the stamped
   tick (`region_runner.rs:172-178`) and applies it once. Server regions
   never roll back (`region_runner.rs:206`).
3. Region, inside `handle_event`:
   - **validate** the typed fields (bounds, volume caps, known ids).
     A malicious client can send any enum value, so the sim never trusts
     the parser ran.
   - **check** permission against sim state
   - **execute** through the same undo-logged `*_safe` mutators the rest of
     the sim uses
   - **append feedback** to sim state

   A validation or permission failure is a no-op plus an `Error` feedback
   line.
   It is still a logged event, so client and server agree on it.
4. The server echoes the event as usual. A client whose prediction differs
   (another player's edit landed first, or a command it didn't predict)
   rolls back and re-simulates as with any other event.

**Determinism requirements (the new constraints):**
- Parsing happens *outside* the sim, so the parser carries no determinism
  burden. The event carries typed values, so the server's execution cannot
  diverge from the client's through parsing. (Belt and braces: clap's
  parser uses no `HashMap`/`HashSet`, checked in `clap_builder` 4.5.53, and
  Rust's `str::parse::<f32>` is correctly rounded on native and wasm32.)
- Relative arguments (`~ ~1 ~`, `@s`, "the block I'm looking at") are
  resolved **at execution time against sim state**, not by the client when
  typing. That keeps them correct under rollback: a re-sim after a
  correction teleports relative to the corrected position.
- Permission level lives in sim state (a field on `Client`, set at
  `CreateClient`), so client prediction and server agree on who may run
  what. Everyone gets `Admin` for now (§8). Changing it later is itself a
  server-originated event.
- The executor must be a true inverse under undo. Use the existing
  `*_safe` mutators and `#[undo]` fields. Nothing may go around the log.
  Test each command with the bit-exact pattern in `region.rs:671-690`.

**Feedback as state, not messages.** A re-simulation re-runs the handler, so
one-shot "send text" output would duplicate or fork.
- Add a bounded per-client feedback log to `GameData`, e.g.
  `#[undo(map)] #[emit(insert = CommandFeedback, remove = RetractFeedback)]`
  keyed by `(ClientId, event id)`.
- The macro emits `CommandFeedback` on apply and `RetractFeedback` on undo.
  The client's chat pane inserts and retracts lines by key.
- An identical re-sim produces retract then re-insert within one frame,
  which is invisible. A changed outcome replaces the line.
- Prune the oldest entries inside the handler (also undo-logged) to keep
  the map bounded.
- Other clients simulating the same region also produce these entries. The
  pane shows only the local client's keys, plus lines explicitly marked
  broadcast.

**Command by command:**
- **`/tp` within a region:** `set_body_pose_safe` (`state.rs:907`) in the
  handler. Predicted, so it feels instant. Don't reuse
  `LocalServer::teleport_local_player`: it is a test hook in a forgotten
  transaction.
- **`/tp` across regions:** the handler moves the player's body and queues a
  transfer so that `take_transfers()` reports a departure after the tick
  (`region_runner.rs:238`). The manager then does what it already does for a
  walking crossing: `EntityArrived` to the destination, `homes` update, and
  `PlayerRegion` to the client. Only the server's departure is
  authoritative. The client's predicted side follows the existing crossing
  path (`region.rs:227`). Cover it in the seeded `handoff_schedule` layer
  (`just test-threads`). Build this after same-region `/tp`.
- **`/setblock`, `/fill`:** call `set_block_safe` (`game/src/edit.rs:197`,
  landed with block editing) directly. No separate
  `BlockEdit` round-trip is needed, since the command already is the sim
  event.
  - Cap `/fill` volume: every block is an undo-log entry and part of every
    re-sim of that tick.
  - v1 limits it to the issuing region. Multi-region fills need
    per-region events injected by the manager.
- **`/give`, `/fly`:** inventory / `Motion` mutations, both already sim
  state.
- **`/time`, `/weather`:** no world clock exists. When one does, it is
  deterministic region state and these become ordinary sim commands.
- **`/spawn`:** the cross-region `/tp` to `SPAWN_REGION`
  (`world_manager.rs:18`).
- **`/help`, `/list`:** not sim commands.
  - `/help` is clap's generated help, rendered locally.
  - `/list` is resolved client-side from the `Roster`.
- **`/where`:** reads the player's own sim position, so it is a sim command
  whose feedback line comes from state.

**Guards:**
- Reject `CommandSource::Console` in client packets, the same way
  client-sent `EntityArrived` / `GhostUpdate` are dropped
  (`world_manager.rs:463-468`).
- Fix hazard 4 (§6) first. Otherwise, with everyone admin, any client can
  run commands as someone else.
- Add `Command` to `origin_client()` (it returns the player id) and to
  `matches_prediction()`. Add the client `instance.rs::handle_game_event`
  arm, mirroring `UiAction`'s caught-up gate (`instance.rs:484-491`).

### Parser choice: clap derive, typed enum as the sim payload

**Options weighed (2026-09-22).** An in-game command parser has to do five
things:
- parse a line the game hands it, not the process's own arguments
- return an error instead of exiting the process
- accept Minecraft coordinates, including negative numbers and `~`
- complete a half-typed line while the player types
- build commands at runtime for mods

| Option | Fit | Notes |
|---|---|---|
| **clap 4 (derive)** | **Chosen** | Handles the first three (verified in the 4.5.53 source already in the registry cache): `no_binary_name` + `try_get_matches_from`, errors as values, `allow_negative_numbers`. The builder API builds `Command`s at runtime for mods. Completion is not built in: `clap_complete::engine::complete` is feature `unstable-dynamic`, `OsString`/shell-shaped. But walking the `Command` tree (`get_subcommands`, `get_arguments`, `get_possible_values`) is stable and enough for names, subcommands and enum values. Already in `Cargo.lock` (4.5.53, only as criterion's dev-dependency), so no new version. |
| argh | Lighter alternative | Built for small binaries (Fuchsia). Derive + `from_args(&[..], &[..])`, runtime `DynamicSubCommand`. No in-process completion. Help is terse. Negative-number positionals unverified: check before choosing it. Pick it if the wasm download size of clap turns out to matter. |
| bpaf | Viable | No required deps, derive or combinators, and `run_inner` on a provided `Args`. In-process completion exists but speaks a shell protocol. The API is the hardest of the three to read. |
| pico-args / lexopt | No | Token lexers. There is no subcommand tree or help, so we would hand-write the structure anyway. |
| Hand-rolled Brigadier tree | Previous plan | Best UX: per-argument completion and red-underline at an exact offset. ~400–600 lines to own. Keep it as the upgrade path if clap's completion feels too coarse. The typed `Command` enum doesn't change, only the parser in front of it. |

**Shape:**
- `game/src/command.rs`: `#[derive(clap::Parser, Serialize, Deserialize)]`
  on `Command` (subcommand per verb) + `CommandSource` + `Coord`
  (a `FromStr` for `~`, `~n` and `n`).
- One `parse_command(line) -> Result<Command, String>`:
  - `shlex::split` for quoting (already locked, 1.3.0), or
    `split_whitespace`
  - then `Command::try_parse_from`, with the command configured with
    `no_binary_name` and `allow_negative_numbers`
- `game` takes `clap = { default-features = false, features = ["std", "derive", "help", "usage", "error-context", "suggestions"] }`.
  - Drop `color`, since errors render in the chat pane, not a terminal.
  - Never enable `wrap_help`, since terminal width has no meaning in-game.
  - `game` stays Bevy-free and compiles on wasm32.
- Error text: clap renders "unexpected argument / did you mean / usage".
  Show `err.render().to_string()` in the typist's pane. It is never sim
  state.
- Completion (step 5): walk `Command::command()` for the current token:
  subcommand names, `--flags`, `PossibleValue`s, and player names from the
  client's `Roster`. Suggestions at an exact argument offset are the one
  thing given up versus a hand-rolled tree.
- `/help` is clap's generated help, rendered locally. It is not a sim
  command.
- `CommandTree` over the wire: not needed while all commands are compiled
  into both builds. Revisit when mods register commands (clap builder
  specs sent by the server).
- Mods later: `Command::Mod { id, args: Vec<String> }` via clap's
  `external_subcommand`. The in-sim wasm mod parses its own typed args
  deterministically.
- Action item before merging: build the wasm client with and without clap
  and record the `.wasm` size difference. If it matters, switch to argh:
  only the derive attributes change.
- Tests: `just test-one game "--lib command::"`. Cover each verb's parse,
  negative and relative coords, error rendering, and a bit-exact undo test
  per executor.

## 3. Client

There is nothing to reuse for text entry. Bevy UI + text + `default_font` are
enabled, and `renderer/hud.rs` has the F3 overlay pattern to copy.

- **`ChatState` resource:** `open: bool`, `draft: String`, `cursor`,
  `sent_history` (↑/↓), `lines: VecDeque<(ChatLine, received_at)>` capped at
  100, and a completion popup.
- **Open and close:**
  - `T` opens empty; `/` opens pre-filled with `/`.
  - `Enter` sends a `/` draft as a sim `GameEventKind::Command` (predicted),
    except social verbs `/msg /me /say`, which go out as `Chat`. Any other
    draft goes out as `Chat`. Empty closes.
  - `Esc` closes without sending.
  - Read `KeyboardInput` messages and use `logical_key` / `text` for
    characters, so keyboard layouts work, not `KeyCode`. Handle Backspace,
    ←/→, Home/End, Ctrl+V (clipboard via `arboard` native / web API; v2).
  - The key event that opens chat must not also insert its `t`. Consume it
    that frame.
- **Input gating (the subtle part):**
  - `renderer/input.rs::forward_input` (`:103-150`) turns `ButtonInput`
    edges into sim `PlayerInput`.
  - On open, send **release events for every currently-held mapped key**.
    Otherwise the sim keeps walking, since the server holds inputs by tick
    (8e90481).
  - Then gate `forward_input` with `run_if(chat_closed)`.
  - Gate `hud::toggle_debug`, `grid_debug` (F5) and the E toggle the same
    way.
- **Cursor:** two writers exist already:
  - sim-driven `fps_cam_mode` in `bridge.rs:676-702`, written idempotently
    to avoid the X11 re-grab loop
  - `ui_bridge` `SetCursorGrab` for the inventory

  Don't add a third fighter. Make chat an input to the one place that decides
  the grab: "grabbed = fps_cam && !inventory_open && !chat_open". Keep it
  idempotent.
- **Rendering:**
  - A bottom-left `Node` column of `Text` spans, fed by a new
    `ClientUpdateEvent::Chat(ChatLine)`.
  - Lines fade after 10 s unless chat is open. Open shows scrollback plus the
    draft line.
  - Completion hints come from the local `CommandTreeWire` + `Roster`.
    Invalid input is red from the parse error's `at` offset.
- **Plumbing:**
  - Outbound, the Bevy side currently only has `GameEvents(Sender<GameEventKind>)`.
    Add a separate `ChatOut(Sender<ChatOutbound>)` channel so chat never
    enters the sim channel.
  - The game thread forwards it as `ClientPacket::Chat`. Commands don't use
    this channel: they are `GameEventKind::Command` on the existing
    `GameEvents` sim channel.
  - Command feedback arrives as the `CommandFeedback` / `RetractFeedback`
    render updates (§2), not over the chat channel.
  - `instance.rs::handle_server` (`:926`) maps `ServerPacket::Chat` to
    `ClientUpdateEvent::Chat`.
- **wasm:** the same `ClientPacket` / `ServerPacket` enums and
  `GameInstanceManager` make it work unchanged. Browser caveats:
  - `Esc` also releases pointer lock.
  - `/` may trigger Firefox quick-find unless the canvas handler calls
    `preventDefault`. Check this in the browser.
- **IME:** out of scope for v1. Bevy exposes `Ime` events if it's needed
  later.

## 4. Identity (decided 2026-09-22)

- **Names are server-assigned placeholders:** `Player<ClientId>`, set on
  `ClientConnected`. No name handshake and no client-side name input until
  Postgres accounts exist (the multi-instance work). A reconnect gets a new
  `ClientId` and therefore a new name, which is acceptable for now.
- **Everyone is admin:** every `CommandSource::Player` gets `Admin` level;
  Console (stdin) and `LocalServer` are `Console`. Keep the
  `PermissionLevel` checks in the tree anyway, so tightening it later is a
  one-line change in `ChatHub` (where the level is granted), not a rewrite.
- **Deferred to the accounts work:** chosen names, auth, stable player ids,
  and a `Hello { protocol }` version check. The version check is still
  worth adding on its own, since the region-size build dial has no mismatch
  guard.

## 5. Scale and the product direction

- **Visibility:** global chat is fine for one server. Minecraft is global
  per server. Later options: proximity "say" (routed by `homes` / region
  neighbourhood, which the manager already knows) plus explicit
  channels/whispers.
- **Multi-instance:** the k8s/Postgres spec already names NATS for global
  chat/presence fan-out. Keep broadcasting behind one `ChatHub::deliver`
  method so the transport can change without touching callers.
- **Cozy / non-violent direction:** the generated-regions research
  (`04-cozy-nonviolent-multiplayer.md`) discusses gated or reduced free
  chat. The system above supports that as policy, since `ChatHub` decides
  who may send and see what. Emotes/`/me` and a quick-phrase wheel can sit
  on the same `ChatLine` path.
- **Moderation v1:**
  - server-log every line
  - `/mute <player> [secs]` and `/kick`
  - the rate limit
  - no filtering beyond control-char stripping

## 6. Existing hazards found on the way (fix before or with chat)

1. **Any client can stop the server:** a client-sent `GameEvent(Quit)` makes
   `handle_server_event` return `false` (`world_manager.rs:429`). With a
   command console, stopping belongs to `/stop` at Console level. Drop
   client `Quit`, or map it to a disconnect.
2. **Unbounded read + panic per stream:**
   `stream.read_to_end(usize::MAX).await.unwrap()`
   (`server/src/lib.rs:417`).
   - A client can send an arbitrarily large stream.
   - A reset stream panics the task.
   - Cap client→server reads (e.g. 64 KiB, since clients send no large
     packets) and log errors instead of unwrapping.
3. **Client→server reordering** on native QUIC (per-stream spawned tasks,
   `server/src/lib.rs:416`).
   - Inputs survive it because they are tick-stamped.
   - Chat, commands and ordering-sensitive requests (Request/Release
     subscription pairs!) do not.
   - Better fix than per-packet `seq`: decode streams in accept order.
     Accept, then push the `read_to_end` future into a
     `FuturesOrdered`, which keeps the reads concurrent and emits in order.
     The WebTransport path already reads in order.

4. **Client id spoofing:** the manager routes `GameEvent` /
   `GameEventAfterTick` by the client id *inside* the event
   (`kind.origin_client()`, `world_manager.rs:435`), not by the connection
   id `id` the packet arrived on. Any client can already send
   `PlayerInput` / `UiAction` as another player. With sim commands and
   everyone admin, it could run commands as them too. Fix: drop the event
   when `origin_client() != Some(id)`.

## 7. Build order

1. Hazard fixes 1–2 (small, independent).
2. Placeholder names (`Player<id>`) + roster + join/leave system lines.
3. Chat wire + `ChatHub` (validate, rate-limit, log, broadcast) + client HUD:
   T//, draft, send, fade, input gating incl. key release, cursor rule.
   Verify live with server + 1 client, then a second client for the
   broadcast.
4. Hazard 4 (client id spoofing).
   `Command` enum + clap-derive parser in `game` with pure unit tests
   (negative and relative coords, error text).
   `GameEventKind::Command` end to end: manager routing, `handle_event`,
   and the feedback log with `#[emit]` insert/retract.
   First sim commands `/where /fly`, each with a bit-exact undo test;
   `/help` (clap help) and `/list` (roster) client-side.
   Social verbs `/msg /me /say` on the chat path. Console stdin source.
   (`CommandTree` wire deferred: clap-derived `Command` compiled into both
   builds.)
5. Client completion from clap `Command` introspection + `Roster`.
6. Same-region `/tp`, `/give`. Then `/setblock` / `/fill` on the existing
   `set_block_safe` (`game/src/edit.rs:197`). Then cross-region `/tp` + `/spawn` with
   `test-threads` coverage. Check a mispredicted command (two players
   editing the same block in one tick) rolls back cleanly in a client
   convergence harness.
7. v2: server suggestions (`Suggest`/`Suggestions`), `@a/@p/@e` selectors,
   clickable spans, proximity chat, mod-registered commands.

## 8. Decisions (2026-09-22)

- **Chat is global** for now. Proximity chat stays a later option behind
  `ChatHub::deliver`.
- **Names are placeholders** (`Player<ClientId>`) until Postgres accounts.
- **Everyone has admin** for now. Permission checks stay in the tree.
- **Commands are part of the simulation**: predicted, logged, rolled back
  and re-simulated like inputs, because executing them can change the
  world. Chat stays outside the sim.
- **Parser: `clap` derive at the edge** (client chat box + server console)
  into a typed `Command` enum carried by the sim event. argh is the fallback
  if clap's wasm size cost is too high. A hand-rolled tree is the fallback
  if completion UX needs exact per-argument offsets.
