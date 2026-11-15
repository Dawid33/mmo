---
name: chat-commands-investigation
description: Minecraft-like chat + slash commands investigated 2026-09-22; design in docs/superpowers/specs/2026-09-22-chat-and-commands-investigation.md; nothing built
metadata:
  type: project
---

INVESTIGATED 2026-09-22, nothing built. Spec: docs/superpowers/specs/2026-09-22-chat-and-commands-investigation.md.
Key calls: chat is manager-level ClientPacket/ServerPacket (never a GameEventKind / never rolled back); commands = in-house Brigadier-shaped tree in `game` (data-only, sent to client filtered by permission); USER DECISION: commands are SIM EVENTS (`GameEventKind::Command(source, line)`): client-predicted via GameEventAfterTick like UiAction, routed by homes, executed deterministically in Region::handle_event with undo-logged mutators, rolled back/re-simulated; feedback is sim state emitted insert/retract via #[emit] so re-sim never duplicates; relative args resolved at exec time; permission lives in sim state. Only chat + social verbs (/msg /me /say) stay off-sim.
PARSER (2026-09-22, user asked for clap/lightweight dep): clap derive at the EDGE (client chat box + server console) → typed `Command` enum is the sim event payload (no string parsing in sim; parse errors local only; sim validates fields). clap 4.5.53 already in Cargo.lock via criterion dev-dep; verified no HashMap, no_binary_name/try_get_matches_from/allow_negative_numbers. Completion via Command introspection (clap_complete dynamic is unstable). Fallbacks: argh if wasm size hurts; hand-rolled Brigadier tree if per-arg completion needed. Open action: measure wasm size delta.
USER DECISIONS 2026-09-22: chat GLOBAL for now; names = server-assigned placeholders `Player<ClientId>` until Postgres accounts (no Hello/name handshake); EVERYONE has admin for now (keep permission checks in the tree so tightening is one line).
Hazards found: client `Quit` stops the server (world_manager.rs:429); unbounded read_to_end+unwrap per stream (server/src/lib.rs:417); native QUIC client→server packets can reorder (task per stream); manager routes events by the id INSIDE the event not the connection id (world_manager.rs:435) → spoofing.

**Why:** avoid re-investigating. **How to apply:** start from the spec's build order (§7); see [[player-height-and-world-editing]] for the BlockEdit path /setblock depends on.
