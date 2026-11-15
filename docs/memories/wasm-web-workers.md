---
name: wasm-web-workers
description: "wasm client multithreading via Web Workers (sim worker + compute pool), built 2026-11-02, uncommitted; open issues"
metadata:
  node_type: memory
  type: project
  originSessionId: 10e5dca3-90fe-4cb7-989d-1c0e8ef792a3
  modified: 2026-11-02T18:45:48.554Z
---

BUILT 2026-11-02 (uncommitted, docs in CLAUDE.md "Web Workers" + crates/client/src/web_worker/):
message-passing Web Workers (same wasm module re-instantiated; NOT shared-memory threads —
wgpu drops Send/Sync with +atomics so Bevy can't build). Sim worker runs GameInstanceManager;
WebTransport stays on the page (connect_frames). Compute pool runs typed ComputeJobs
(renderer/compute.rs). `?workers=0` = old main-thread behaviour.

Verified live 2026-11-02 in headless Chrome (debug wasm, SwiftShader, ?world=tiny): join,
arrival map, Return-to-site, fortress terrain meshed by 6 workers. Client unit tests
(`just test-one client "--lib compute::"`) written but NOT run (opt-in).

Open: a worker-hosted transport wedged the server's shared writer once (debug build, under
load) — unexplained, so transport kept on the page. Server writer fix (block_in_place around
crossbeam recv, tokio LIFO-slot freeze → "Network blackhole") applies to native clients too.
Gotcha: serde `#[serde(skip)]` on an enum VARIANT renumbers variants on decode → use skipped fields.

**Why:** user asked "make wasm use web workers for multithreading everywhere appropriate".
**How to apply:** new off-main-thread work = a ComputeJob; don't re-research shared-memory threads.
Related: [[wasm-webgl2-per-chunk-fallback]], [[netcode-dropped-packet-gap]].
