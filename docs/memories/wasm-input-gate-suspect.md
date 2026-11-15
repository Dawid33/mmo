---
name: wasm-input-gate-suspect
description: "RESOLVED 2026-07-05 (commit 190fdc0) — the `!ready` input gate that dropped key events (sticky WASD, dead browser input) was removed; input now gates only on is_caught_up"
metadata: 
  node_type: memory
  type: project
  originSessionId: 247ddb70-03e4-4c24-b129-4980ae9a5f24
---

RESOLVED: the pre-existing `!ready && is_caught_up` PlayerInput drop-gate in client `main.rs` was confirmed live in the browser (261 inputs dropped : 1 sent — `ready` flaps false whenever a SyncClock sample catches the client one tick behind at rtt≈0, silencing input for 500ms per flap) and fixed in commit `190fdc0`: input gates only on `is_caught_up` (join/replay window); `ready` only tunes the adaptive tick rate. Regression test: `player_input_flows_while_not_ready_once_caught_up` in client main.rs.

**How to apply:** if sticky/dead input ever reappears, do NOT re-suspect this gate first — check the newer layers instead: browser pointer-lock needs user activation for cursor grab, and a wedged server writer task freezes SyncClock delivery entirely (see [[wasm32-usize-sentinel-hazard]] session; server fix `6831e1a` added keep-alive/idle-timeout + bounded sends).
