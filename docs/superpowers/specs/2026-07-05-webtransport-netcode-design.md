# WebTransport Netcode — Design

**Date:** 2026-07-05
**Status:** Approved (brainstormed with user; scope/protocol/server-shape decisions below are theirs)

## Goal

The browser (wasm) client connects to the real game server and participates in the same
rollback-reconciled world as native clients. Today the wasm build is offline-only: it embeds
`LocalServer` (crates/client/src/local_server.rs) because browsers cannot open raw QUIC/UDP
sockets. WebTransport is the browser-native answer: QUIC + HTTP/3 + Extended CONNECT, exposing
uni/bidirectional streams and datagrams to the page.

## Decisions (locked with user)

1. **Scope: localhost dev only.** Browser and server on one machine. Self-signed rcgen cert
   accepted via `serverCertificateHashes`; no CA, no domain, addresses stay hardcoded
   `127.0.0.1`.
2. **Protocol: streams-only parity.** One uni-stream per bincode packet in each direction,
   byte-identical to the native quinn wire format. Datagrams are a later optimization.
3. **Server shape: `wtransport` on a second port.** Raw quinn stays on `127.0.0.1:6466`
   untouched; a dedicated `wtransport` endpoint listens on `127.0.0.1:6467`. Rejected
   alternatives: shared-port ALPN demux via `web-transport-quinn` (younger crate, ingress
   complexity), WebSocket bridge (TCP head-of-line blocking undermines rollback latency).

## Architecture

```
native client ── raw QUIC (quinn) ──► :6466 ─┐
                                             ├─► ServerEvent channel ─► dumb-router loop ─► World
browser wasm ── WebTransport ────────► :6467 ─┘                    ◄─ (Option<ClientId>, ServerPacket) broadcast
```

Both transports deserialize the same `ClientPacket`s into the same `ServerEvent` channel and
are fanned out to from the same broadcast channel. The sim and router logic never learn which
transport a client uses. Client IDs are allocated from the same counter across both transports
so `find_player`/reconnect semantics hold.

## Components

### Server: `crates/server/src/webtransport.rs` (new)

- Startup: generate an rcgen **ECDSA P-256** cert valid **≤ 14 days** (both properties are
  hard requirements of `serverCertificateHashes`), start a `wtransport::Endpoint` on
  `127.0.0.1:6467` inside the existing tokio runtime.
- Accept loop: on session accept, allocate a ClientId (shared counter with the quinn ingress),
  emit `ServerEvent::ClientConnected`, spawn:
  - a read task: `accept_uni` → read-to-end → `bincode::deserialize::<ClientPacket>` →
    `ServerEvent::ClientPacket`,
  - a write task: drain that client's outgoing packets → `open_uni` → write bincode →
    finish. Mirrors the quinn connection handler.
- Broadcast fan-out: the existing `(Option<ClientId>, ServerPacket)` channel gains
  WebTransport sessions as additional sinks alongside quinn connections (per-client routing
  unchanged: `Some(id)` targets one client, `None` broadcasts).

### Cert-hash delivery: `assets/webtransport-cert-hash.json` (generated, gitignored)

At startup the server writes `{ "sha256_hex": "<hex of SHA-256 over the DER cert>", "port": 6467 }`
into `assets/`. The client decodes the hex into the `BufferSource` that `WebTransportHash`
expects. wasm-server-runner already serves `assets/` over HTTP from the repo root, so the
page can fetch it before opening the transport. This is the dev-mode substitute for a CA.

### Client: `crates/client/src/netcode_web.rs` (new, wasm-only)

The third implementation of the transport seam (after `netcode.rs` and `local_server.rs`):

- Fetch `assets/webtransport-cert-hash.json`; construct
  `new WebTransport("https://127.0.0.1:<port>/", { serverCertificateHashes: [...] })` via
  `web-sys` (features: `WebTransport`, `WebTransportHash`, streams).
- Read loop (`wasm_bindgen_futures::spawn_local`): `incomingUnidirectionalStreams` → read
  each to end → `bincode::deserialize::<ServerPacket>` → inject RTT into `SyncClock` packets
  (from `getStats().smoothedRtt` when the binding exposes it, else `Duration::ZERO`) → send
  into the same crossbeam channel `LocalServer` fills today.
- Write loop: drain `GameInstanceManager::client_packet_recv()` → `createUnidirectionalStream`
  → write bincode → close. Because the browser is single-threaded, draining is polled from a
  `spawn_local` task (or the frame driver) rather than a blocking `recv()`.

### Mode select: `crates/client/src/sim_driver.rs` (modified)

- Page URL query param `?server` (presence alone; host and port come from the cert-hash
  json, host fixed to `127.0.0.1` per scope) → online mode: `SimDriver` holds
  the WebTransport channels and does NOT construct `LocalServer` (no local ticking; the real
  server is the authority; `drive_sim` keeps client-side tick generation + `pump`).
- No query param → offline mode, exactly today's behavior.
- `GameInstanceManager`, `pump()`, `handle_server`, and the render bridge are untouched.

## Error handling

- Server: a browser client vanishing (tab closed) surfaces as session close in the read task —
  clean up the sink entry, log, keep serving (same policy as quinn disconnects today).
- Client: connection failure or mid-session close turns the loading overlay path into a
  console error + on-screen "connection lost" text (dev-grade; no auto-reconnect in scope).
- Malformed packets: same policy as native (`warn!` and skip, netcode.rs:62).

## Testing

1. **Native integration test** (no browser): `wtransport` has a client half; a tokio test
   connects to the server's WebTransport port, performs RequestPlayerRegion →
   RequestRegionConnection, asserts a Region snapshot arrives and an echoed GameEvent
   round-trips. Runs in CI on native.
2. **Headless browser check**: server + wasm-server-runner up, headless Chromium loads
   `?server=1`, console-assert the region handshake and no panics (same harness used to verify
   the offline wasm build).
3. **Manual acceptance**: native client + browser tab in the same world — each sees the
   other's player entity move (the point of the feature).

## Out of scope

Datagrams, LAN/internet deployment (bind config, real certs), Safari-fallback (WebSocket),
auto-reconnect, native client changes beyond the shared broadcast fan-out refactor.

## Risks / notes

- `wtransport` pins its own quinn-stack version; it lives only in `crates/server`, so a
  version skew with the client's `quinn 0.11` is acceptable (two QUIC dependency trees in one
  binary is the accepted cost of the second-port decision).
- `web-sys` WebTransport bindings are behind unstable-apis cfg in some versions — the plan
  must verify the required features compile on the pinned toolchain before committing to the
  binding surface (fallback: `js_sys::Reflect`-based glue for the few calls needed).
- The rollback `ready` gate (`!ready && is_caught_up`, main.rs) now runs with real nonzero
  RTT in the browser; the known sticky-input suspect documented in the wasm plan applies to
  browser play the same way it does natively.
