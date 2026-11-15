---
name: testing-one-client
description: "When live-testing the game, launch the server plus ONE client by default, not two"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 53828178-a245-4802-99ff-8647b2a56282
---

Dawid (2026-07-04): start one client, not two, when testing.

**Why:** every live check was spawning two windows he then had to close by
hand; one client covers most verification (join, rendering, movement,
collision), and closing windows also trips the known shutdown-panic noise.

**How to apply:** default live runs to server + 1 client (launch with
`WGPU_ADAPTER_NAME=6700`, see [[egpu-adapter-selection]]). Only launch a
second client when the thing under test is inherently multi-client
(reconcile of foreign events, avatar visibility, identity separation) — and
say why when doing so.
