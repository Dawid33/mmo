# WebTransport Netcode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The browser (wasm) client connects to the real game server over WebTransport and reconciles in the same world as native clients.

**Architecture:** Per the approved spec (`docs/superpowers/specs/2026-07-05-webtransport-netcode-design.md`): raw quinn stays on `127.0.0.1:6466`; a `wtransport` endpoint joins on `127.0.0.1:6467`; both feed the same `ServerEvent` channel and share one outgoing-sink map, so the router never learns the transport. The wasm client gains `netcode_web.rs` — a `web-sys` WebTransport implementation of the same channel seam `LocalServer` fills — selected at page load by a `?server` query param. Wire format is byte-identical: one uni-stream per bincode packet, each direction.

**Tech Stack:** `wtransport 0.7` (server), `web-sys 0.3.103` WebTransport bindings behind `--cfg web_sys_unstable_apis` + `wasm-bindgen-futures` (client), rcgen-style self-signed ECDSA cert accepted via `serverCertificateHashes`.

## Global Constraints

- `game` crate untouched; it stays Bevy-free, windowing-free, and network-free.
- `server` stays Bevy-free and windowing-free. `wtransport` is allowed in `server` only.
- Vendored forks untouched; Bevy stays `0.18`; quinn stays `0.11.6` for the native path.
- Native behavior unchanged: native client ↔ quinn ↔ `:6466` works exactly as today; offline wasm (`no ?server`) works exactly as today.
- Ports: quinn `127.0.0.1:6466`, WebTransport `127.0.0.1:6467` (hardcoded, localhost-dev scope).
- Wire format: bincode `ClientPacket`/`ServerPacket`, one uni-stream per packet, read-to-end framing. No protocol changes.
- Public seam signatures that must not change: `GameInstanceManager::{new, pump, start, send_tick, tick_rate_ms, client_packet_recv}`, `ServerEvent`, `ClientUpdateEvent`.
- Native suites keep passing: `cargo test -p game`, `cargo test -p client`, plus new `cargo test -p server`.
- Commit after every task with a conventional-commit message.

## Non-Goals (from the spec)

Datagrams; LAN/internet deployment; Safari/WebSocket fallback; auto-reconnect; `getStats()` RTT injection (browser `SyncClock` RTT is `Duration::ZERO` this iteration — note the client-side `ready` math consequence is the same as LocalServer's, which works).

---

### Task 1: Server outgoing-sink abstraction + shared ClientId counter

Pure refactor of `crates/server/src/main.rs`: the writer task currently drains `server_recv` into a `DashMap<ClientId, quinn::Connection>` (`crates/server/src/main.rs:75-104`) and the ClientId counter is a local `next_id` in `listen()` (`:107`). Task 2 needs (a) a sink type that can also hold a WebTransport connection and (b) a counter both accept loops share. No behavior change.

**Files:**
- Modify: `crates/server/src/main.rs`

**Interfaces:**
- Consumes: existing `WorldIngress::listen`, `handle_connection`.
- Produces (Task 2 relies on these exact shapes):
  - `enum ClientSink { Quinn(quinn::Connection) }` — Task 2 adds a `Web(...)` variant; every construction/match site written in this task must compile unchanged when that variant appears (no `if let` that silently drops unknown variants on the send path — use exhaustive `match`).
  - `impl ClientSink { async fn send_packet(&self, packet: Vec<u8>) }` — opens a uni stream, writes, finishes. Logs-and-drops on error (a vanished client must not kill the writer task; today's `unwrap`s die with the spawned task, killing ALL outgoing traffic — this refactor deliberately downgrades them to `warn!`, the sole intended behavior change, matching the "drop the client" doc comment on `listen`).
  - `sinks: Arc<DashMap<ClientId, ClientSink>>` replaces `connections`.
  - `next_client_id: Arc<AtomicUsize>` replaces the local counter (`use std::sync::atomic::AtomicUsize`).

- [ ] **Step 1: Refactor the writer task**

In `listen()`, replace the `connections` map and writer task with:

```rust
let sinks: Arc<DashMap<ClientId, ClientSink>> = Arc::new(DashMap::new());
let writer_sinks = sinks.clone();
tokio::spawn(async move {
    while let Ok((target, event)) = server_recv.recv() {
        let packet = bincode::serialize(&event).unwrap();
        match target {
            // Directed packet: if the client is gone, drop it.
            Some(id) => {
                let sink = writer_sinks.get(&id).map(|e| e.value().clone());
                if let Some(sink) = sink {
                    sink.send_packet(packet.clone()).await;
                }
            }
            None => {
                for entry in writer_sinks.iter() {
                    entry.value().send_packet(packet.clone()).await;
                }
            }
        }
    }
});
```

And add above `WorldIngress`:

```rust
/// One connected client's outgoing half, transport-agnostic. The router
/// writes bincode ServerPackets; each variant frames them as one
/// uni-stream per packet.
#[derive(Clone)]
pub enum ClientSink {
    Quinn(quinn::Connection),
}

impl ClientSink {
    /// Send one packet; a failed/vanished client is logged and skipped so a
    /// single dead connection can't kill the shared writer task.
    pub async fn send_packet(&self, packet: Vec<u8>) {
        match self {
            ClientSink::Quinn(conn) => {
                let mut stream = match conn.open_uni().await {
                    Ok(s) => s,
                    Err(e) => {
                        log::warn!("dropping packet to gone client: {e:?}");
                        return;
                    }
                };
                if let Err(e) = stream.write_all(&packet).await {
                    log::warn!("write to client failed: {e:?}");
                    return;
                }
                let _ = stream.finish();
                tokio::spawn(async move {
                    let _ = stream.stopped().await;
                });
            }
        }
    }
}
```

- [ ] **Step 2: Share the ClientId counter**

Replace `let mut next_id = 0;` with `let next_client_id = Arc::new(AtomicUsize::new(0));` and in the accept loop `let id = next_client_id.fetch_add(1, Ordering::SeqCst);`. Insert `ClientSink::Quinn(connection.clone())` into `sinks` where `connections.insert` was; `conns.remove(&id)` becomes `sinks.remove(&id)` in the disconnect cleanup.

Note: `ClientId = usize` (`crates/game/src/state.rs:24`), hence `AtomicUsize`.

- [ ] **Step 3: Verify**

Run: `cargo check -p server && cargo build -p server`
Expected: PASS, no new warnings.

Manual smoke (the server crate has no test suite until Task 3): run `cargo run --bin server` and `cargo run --bin client` (native), confirm connect + movement works, Ctrl-C both.

- [ ] **Step 4: Commit**

```bash
git add crates/server/src/main.rs
git commit -m "refactor(server): transport-agnostic ClientSink + shared client id counter"
```

---

### Task 2: WebTransport endpoint on the server

**Files:**
- Create: `crates/server/src/webtransport.rs`
- Modify: `crates/server/src/main.rs` (module decl, spawn the endpoint, `ClientSink::Web` variant)
- Modify: `Cargo.toml` (workspace) + `crates/server/Cargo.toml` (add `wtransport`)
- Modify: `.gitignore` (the generated cert-hash file)

**Interfaces:**
- Consumes: `ClientSink`, `sinks`, `next_client_id`, `Sender<ServerEvent>` from Task 1.
- Produces:
  - `webtransport::serve(bind: SocketAddr, send: Sender<ServerEvent>, sinks: Arc<DashMap<ClientId, ClientSink>>, next_client_id: Arc<AtomicUsize>) -> anyhow-free Result<(), Box<dyn std::error::Error + Send + Sync>>` — runs forever inside the existing tokio runtime.
  - `ClientSink::Web(wtransport::Connection)` variant with the same one-uni-stream-per-packet framing.
  - Generated file `assets/webtransport-cert-hash.json`: `{"sha256_hex": "...", "port": 6467}`.

- [ ] **Step 1: Add the dependency**

Workspace `Cargo.toml` `[workspace.dependencies]`:
```toml
wtransport = { version = "0.7", default-features = false, features = ["self-signed", "ring"] }
```
`crates/server/Cargo.toml` `[dependencies]`: `wtransport = { workspace = true }`.

(Feature names drift between wtransport minor versions — if `cargo check` rejects a feature, consult `cargo info wtransport` / its docs.rs for the 0.7 set; the requirements are: server endpoint, self-signed identity generation, ring crypto. Default features are acceptable as a fallback.)

- [ ] **Step 2: Write `crates/server/src/webtransport.rs`**

```rust
//! WebTransport ingress: browsers can't speak raw QUIC, so they connect here
//! (HTTP/3 Extended CONNECT on 127.0.0.1:6467) and are routed into the same
//! ServerEvent channel and ClientSink map as quinn clients. Wire format is
//! identical: one uni-stream per bincode packet.
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crossbeam::channel::Sender;
use dashmap::DashMap;
use game::{ClientId, ClientPacket};
use log::{info, warn};
use wtransport::{Endpoint, Identity, ServerConfig};

use crate::{ClientSink, ServerEvent};

/// Max size of a single packet stream; region snapshots are the largest
/// payloads and stay well under this.
const MAX_PACKET_BYTES: usize = 64 * 1024 * 1024;

pub async fn serve(
    bind: SocketAddr,
    send: Sender<ServerEvent>,
    sinks: Arc<DashMap<ClientId, ClientSink>>,
    next_client_id: Arc<AtomicUsize>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // serverCertificateHashes requires an ECDSA cert valid <= 14 days;
    // wtransport's self-signed identity satisfies both.
    let identity = Identity::self_signed(["localhost", "127.0.0.1"])?;
    // Dev nicety, not load-bearing: never fail the ingress over it.
    if let Err(e) = write_cert_hash_file(&identity, bind.port()) {
        warn!("could not write cert-hash file (browser clients need it): {e:?}");
    }

    let config = ServerConfig::builder()
        .with_bind_address(bind)
        .with_identity(identity)
        .build();
    let endpoint = Endpoint::server(config)?;
    info!("WebTransport endpoint listening on {bind}");

    loop {
        let incoming = endpoint.accept().await;
        let send = send.clone();
        let sinks = sinks.clone();
        let id = next_client_id.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            let session = match incoming.await {
                Ok(req) => match req.accept().await {
                    Ok(conn) => conn,
                    Err(e) => return warn!("webtransport accept failed: {e:?}"),
                },
                Err(e) => return warn!("webtransport session failed: {e:?}"),
            };
            info!("webtransport client {id} connected");
            sinks.insert(id, ClientSink::Web(session.clone()));
            // Announce before reading any packets, mirroring the quinn path:
            // guarantees the player exists before its first request is served.
            send.send(ServerEvent::ClientConnected(id)).unwrap();

            loop {
                let mut stream = match session.accept_uni().await {
                    Ok(s) => s,
                    Err(e) => {
                        info!("webtransport client {id} disconnected: {e:?}");
                        break;
                    }
                };
                let mut buf = Vec::new();
                if let Err(e) =
                    tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut buf).await
                {
                    warn!("webtransport read failed: {e:?}");
                    continue;
                }
                if buf.len() > MAX_PACKET_BYTES {
                    warn!("oversized packet from client {id}, dropping");
                    continue;
                }
                match bincode::deserialize::<ClientPacket>(&buf) {
                    Ok(packet) => {
                        send.send(ServerEvent::ClientPacket(packet, id)).unwrap()
                    }
                    Err(e) => warn!("failed deserializing packet {e:?}"),
                }
            }
            sinks.remove(&id);
        });
    }
}

/// Dev-mode CA substitute: the page fetches this (wasm-server-runner serves
/// assets/ over HTTP) and passes the hash to serverCertificateHashes.
fn write_cert_hash_file(
    identity: &Identity,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let hash = identity
        .certificate_chain()
        .as_slice()
        .first()
        .expect("self-signed identity has one cert")
        .hash();
    let hex = hash
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let json = format!(r#"{{"sha256_hex":"{hex}","port":{port}}}"#);
    // Resolve the repo-root assets dir from the crate location, not the CWD:
    // the binary is run from the repo root but tests run from crates/server.
    // (env! bakes a build-machine path; acceptable for this localhost-dev file.)
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/webtransport-cert-hash.json");
    std::fs::write(path, json)?;
    Ok(())
}
```

API-drift notes for the implementer (shape is fixed, names may need adjusting to wtransport 0.7 docs): `Identity::self_signed`, `certificate_chain()`, per-cert `hash()` (wtransport ships SHA-256 cert digests for exactly this browser flow), `Endpoint::server`, `endpoint.accept().await` → incoming session → `.await` → session request → `.accept().await` → `Connection`; `Connection::accept_uni`/`open_uni`; `RecvStream` implements tokio `AsyncRead`. If `hash()` is absent in 0.7, add `sha2 = "0.10"` to the server and hash the DER bytes directly.

- [ ] **Step 3: Wire into `main.rs`**

Add `mod webtransport;`, add the variant:

```rust
    Web(wtransport::Connection),
```

with its `send_packet` arm (same framing, same log-and-skip error policy):

```rust
            ClientSink::Web(session) => {
                let mut stream = match session.open_uni().await {
                    Ok(opening) => match opening.await {
                        Ok(s) => s,
                        Err(e) => return warn!("webtransport open failed: {e:?}"),
                    },
                    Err(e) => return warn!("dropping packet to gone client: {e:?}"),
                };
                if let Err(e) =
                    tokio::io::AsyncWriteExt::write_all(&mut stream, &packet).await
                {
                    return warn!("webtransport write failed: {e:?}");
                }
                let _ = stream.finish().await;
            }
```

In `listen()`, after the quinn `Endpoint::server(...)` is up, spawn the second ingress on the same runtime:

```rust
        let wt_send = send.clone();
        let wt_sinks = sinks.clone();
        let wt_next = next_client_id.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::webtransport::serve(
                "127.0.0.1:6467".parse().unwrap(),
                wt_send,
                wt_sinks,
                wt_next,
            )
            .await
            {
                log::error!("webtransport ingress died: {e:?}");
            }
        });
```

- [ ] **Step 4: Gitignore the generated file**

Append to `.gitignore`:
```
assets/webtransport-cert-hash.json
```

- [ ] **Step 5: Verify**

Run: `cargo build -p server`
Expected: PASS. Run `cargo run --bin server` briefly: log shows both endpoints; `assets/webtransport-cert-hash.json` appears and contains a 64-char hex hash and port 6467. Native client still connects (`cargo run --bin client`).

- [ ] **Step 6: Commit**

```bash
git add crates/server crates/server/Cargo.toml Cargo.toml Cargo.lock .gitignore
git commit -m "feat(server): WebTransport ingress on :6467 sharing the router with quinn"
```

---

### Task 3: Native integration test (wtransport client half)

CI-runnable proof that a WebTransport client gets the full handshake from the real server loop — no browser involved. This also retro-covers Task 1's refactor.

**Files:**
- Create: `crates/server/tests/webtransport_handshake.rs`
- Modify: `crates/server/src/main.rs` + `crates/server/Cargo.toml` (expose the pieces to tests: add `src/lib.rs` OR make the test spawn the real binary — see Step 1 decision)

**Interfaces:**
- Consumes: `webtransport::serve`, `ClientSink`, `ServerEvent`, the router loop.
- Produces: `cargo test -p server` suite.

- [ ] **Step 1: Make the server testable**

The server is a single `main.rs` binary. Split minimally: move everything except `fn main()` into a new `crates/server/src/lib.rs` (module `pub mod ingress;` is NOT needed — just `pub` the existing items in a lib target), leaving `main.rs` as:

```rust
use server::run;

fn main() {
    run();
}
```

with `pub fn run()` in `lib.rs` holding today's `main` body. Add to `crates/server/Cargo.toml`:

```toml
[lib]
name = "server"
path = "src/lib.rs"

[[bin]]
name = "server"
path = "src/main.rs"
```

Keep every item's code byte-identical — this is a file move plus `pub` on `WorldIngress`, `ServerEvent`, `ClientSink`, `webtransport`, and `run`.

- [ ] **Step 2: Write the failing test**

`crates/server/tests/webtransport_handshake.rs`:

```rust
//! End-to-end: a wtransport client (the same library the browser-facing
//! endpoint uses) performs the full join handshake against the real router
//! loop, proving Region snapshots and GameEvent echoes flow over WebTransport.
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use game::{ChunkCoords, ClientPacket, ServerPacket};
use server::{webtransport, ClientSink, ServerEvent};

async fn recv_packet(conn: &wtransport::Connection) -> ServerPacket {
    let mut stream = conn.accept_uni().await.expect("server closed stream");
    let mut buf = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut buf)
        .await
        .expect("read failed");
    bincode::deserialize(&buf).expect("bad packet")
}

async fn send_packet(conn: &wtransport::Connection, packet: &ClientPacket) {
    let mut stream = conn.open_uni().await.unwrap().await.unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut stream, &bincode::serialize(packet).unwrap())
        .await
        .unwrap();
    stream.finish().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn webtransport_client_full_handshake() {
    let (event_send, event_recv) = crossbeam::channel::unbounded();
    let (server_send, server_recv) = crossbeam::channel::unbounded();
    let sinks: Arc<DashMap<usize, ClientSink>> = Arc::new(DashMap::new());
    let next_id = Arc::new(AtomicUsize::new(0));

    // Writer task (Task 1's) — drains router output into sinks.
    let writer_sinks = sinks.clone();
    tokio::spawn(async move {
        while let Ok((target, event)) = server_recv.recv() {
            let packet = bincode::serialize(&event).unwrap();
            match target {
                Some(id) => {
                    let sink = writer_sinks.get(&id).map(|e| e.value().clone());
                    if let Some(sink) = sink {
                        sink.send_packet(packet.clone()).await;
                    }
                }
                None => {
                    for entry in writer_sinks.iter() {
                        entry.value().send_packet(packet.clone()).await;
                    }
                }
            }
        }
    });

    // WebTransport ingress on an ephemeral-ish test port.
    let bind = "127.0.0.1:16467".parse().unwrap();
    let ingress_sinks = sinks.clone();
    tokio::spawn(async move {
        webtransport::serve(bind, event_send, ingress_sinks, next_id)
            .await
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(300)).await; // endpoint up + hash file written

    // Router loop on a plain thread, same shape as server::run's tail.
    std::thread::spawn(move || {
        let mut world = game::World::basic();
        let mut results_buffer = std::collections::BTreeMap::new();
        while let Ok(event) = event_recv.recv() {
            match event {
                ServerEvent::ClientPacket(packet, client_id) => match packet {
                    ClientPacket::RequestPlayerRegion => {
                        let id = world.find_player(&client_id);
                        server_send
                            .send((Some(client_id), ServerPacket::PlayerRegion(id, client_id)))
                            .unwrap();
                    }
                    ClientPacket::RequestRegionConnection(id) => {
                        server_send
                            .send((Some(client_id), world.build_region_server_packet(&id)))
                            .unwrap();
                    }
                    ClientPacket::GameEvent(ev) => {
                        let out = world.handle_region_event(ev.kind, ev.region_id).unwrap();
                        world.forget_last_event(&ev.region_id);
                        server_send.send((None, ServerPacket::GameEvent(out))).unwrap();
                    }
                },
                ServerEvent::ClientConnected(client_id) => {
                    if world.find_player(&client_id).is_none() {
                        let region = ChunkCoords::new(0, 0, 0);
                        let ev = world
                            .handle_region_event(
                                game::GameEventKind::CreateClient(client_id),
                                region,
                            )
                            .unwrap();
                        world.forget_last_event(&region);
                        server_send.send((None, ServerPacket::GameEvent(ev))).unwrap();
                    }
                }
                ServerEvent::ServerTickTimer => {}
            }
        }
    });

    // Client side: read the hash file the ingress just wrote, connect.
    let hash_json = std::fs::read_to_string("assets/webtransport-cert-hash.json")
        .or_else(|_| std::fs::read_to_string("../../assets/webtransport-cert-hash.json"))
        .expect("cert hash file written by serve()");
    assert!(hash_json.contains("sha256_hex"));

    let config = wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_no_cert_validation() // test-only; the browser path uses the hash
        .build();
    let conn = wtransport::Endpoint::client(config)
        .unwrap()
        .connect("https://127.0.0.1:16467")
        .await
        .expect("connect failed");

    // CreateClient broadcast arrives first (ClientConnected fired on accept).
    let first = recv_packet(&conn).await;
    assert!(matches!(first, ServerPacket::GameEvent(_)), "got {first:?}");

    send_packet(&conn, &ClientPacket::RequestPlayerRegion).await;
    let player_region = recv_packet(&conn).await;
    let region_id = match player_region {
        ServerPacket::PlayerRegion(id, client_id) => {
            assert_eq!(client_id, 0);
            id.unwrap_or(ChunkCoords::new(0, 0, 0))
        }
        p => panic!("expected PlayerRegion, got {p:?}"),
    };

    send_packet(&conn, &ClientPacket::RequestRegionConnection(region_id)).await;
    let snapshot = recv_packet(&conn).await;
    assert!(matches!(snapshot, ServerPacket::Region(..)), "got {snapshot:?}");
}
```

(If `with_no_cert_validation` requires a wtransport feature (`dangerous-configuration`-style), enable it under `[dev-dependencies]` only. Add `wtransport`, `tokio` with `macros`+`rt-multi-thread`+`time`, `crossbeam`, `dashmap`, `bincode`, `game` to `[dev-dependencies]` of `crates/server` as needed — `tokio::test` and `tokio::time` are test-only requirements.)

- [ ] **Step 3: Run test to verify it fails, then passes**

Before Step 1's lib split lands: FAIL (unresolved `server::` imports). After implementation:
Run: `cargo test -p server webtransport_client_full_handshake`
Expected: PASS.

Run: `cargo test -p server && cargo test -p game && cargo test -p client && cargo check -p client`
Expected: all PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/server
git commit -m "test(server): wtransport-client integration test for the full join handshake"
```

---

### Task 4: wasm client transport (`netcode_web.rs`) + mode select

**Files:**
- Create: `crates/client/src/netcode_web.rs` (wasm-only)
- Modify: `crates/client/src/sim_driver.rs` (online/offline mode)
- Modify: `crates/client/src/main.rs` (module decl)
- Modify: `crates/client/Cargo.toml` (wasm deps), `.cargo/config.toml` (unstable-apis cfg)

**Interfaces:**
- Consumes: `GameInstanceManager::{client_packet_recv, start}`, the `Sender<ServerPacket>` side that `LocalServer` uses today.
- Produces: `netcode_web::connect(server_send: Sender<ServerPacket>, client_recv: Receiver<ClientPacket>)` — fire-and-forget: spawns `spawn_local` read/write loops; `sim_driver::SimMode { Offline(LocalServer, f64), Online }` driving `drive_sim`'s branch.

- [ ] **Step 1: Build config for the unstable bindings**

`.cargo/config.toml` (merge into the existing `[target.wasm32-unknown-unknown]` table):

```toml
[target.wasm32-unknown-unknown]
runner = "wasm-server-runner"
rustflags = ["--cfg", "web_sys_unstable_apis"]
```

`crates/client/Cargo.toml`, wasm target table — add:

```toml
wasm-bindgen = "0.2"
wasm-bindgen-futures = "0.4"
js-sys = "0.3"
web-sys = { version = "0.3", features = [
  "Window", "Location", "Response", "Document",
  "WebTransport", "WebTransportOptions", "WebTransportHash",
  "WebTransportCloseInfo", "ReadableStream", "ReadableStreamDefaultReader",
  "WritableStream", "WritableStreamDefaultWriter",
  "WebTransportReceiveStream", "WebTransportSendStream",
] }
```

(web-sys is already in the lockfile at 0.3.103 via Bevy; these lines only add features. If a feature name is rejected, `ls ~/.cargo/registry/src/*/web-sys-0.3.103/src/features/ | grep -i <name>` finds the real one.)

- [ ] **Step 2: Write `crates/client/src/netcode_web.rs`**

```rust
//! WebTransport netcode for the browser: the third implementation of the
//! transport seam (after netcode.rs/quinn and local_server.rs). Fetches the
//! dev cert hash the server publishes, opens a WebTransport session, and
//! shuttles one bincode packet per uni-stream in each direction — the same
//! wire format quinn speaks natively.
use crossbeam::channel::{Receiver, Sender};
use game::{ClientPacket, ServerPacket};
use js_sys::{Array, Object, Reflect, Uint8Array};
use log::{error, info, warn};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    ReadableStreamDefaultReader, WebTransport, WebTransportHash, WebTransportOptions,
};

/// Kick off the connection; returns immediately. All I/O runs on the browser
/// event loop via spawn_local, feeding the same channels LocalServer feeds
/// in offline mode.
pub fn connect(server_send: Sender<ServerPacket>, client_recv: Receiver<ClientPacket>) {
    spawn_local(async move {
        if let Err(e) = run(server_send, client_recv).await {
            error!("webtransport connection failed: {e:?}");
            // Spec: dev-grade on-screen signal, not just the console.
            if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                doc.set_title("CONNECTION LOST — Labour of Love");
            }
        }
    });
}

async fn run(
    server_send: Sender<ServerPacket>,
    client_recv: Receiver<ClientPacket>,
) -> Result<(), JsValue> {
    let window = web_sys::window().expect("no window");

    // 1. Fetch the cert hash + port the server wrote into assets/.
    let resp: web_sys::Response =
        JsFuture::from(window.fetch_with_str("assets/webtransport-cert-hash.json"))
            .await?
            .dyn_into()?;
    let text = JsFuture::from(resp.text()?).await?;
    let text = text.as_string().expect("hash file not text");
    let (hash_hex, port) = parse_hash_json(&text).expect("malformed cert hash file");
    let hash_bytes: Vec<u8> = (0..hash_hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hash_hex[i..i + 2], 16).unwrap())
        .collect();

    // 2. Open the transport, trusting exactly that certificate.
    let hash = WebTransportHash::new();
    hash.set_algorithm("sha-256");
    hash.set_value(&Uint8Array::from(hash_bytes.as_slice()));
    let options = WebTransportOptions::new();
    options.set_server_certificate_hashes(&Array::of1(&hash));
    let transport = WebTransport::new_with_options(
        &format!("https://127.0.0.1:{port}/"),
        &options,
    )?;
    JsFuture::from(transport.ready()).await?;
    info!("[client] webtransport connected");

    // 3. Read loop: each incoming uni-stream is one ServerPacket.
    let incoming: ReadableStreamDefaultReader =
        transport.incoming_unidirectional_streams().get_reader().dyn_into()?;
    let read_send = server_send.clone();
    spawn_local(async move {
        loop {
            let next = match JsFuture::from(incoming.read()).await {
                Ok(v) => v,
                Err(e) => return warn!("incoming streams closed: {e:?}"),
            };
            if Reflect::get(&next, &"done".into())
                .map(|d| d.is_truthy())
                .unwrap_or(true)
            {
                return info!("server closed the connection");
            }
            let stream: web_sys::ReadableStream =
                match Reflect::get(&next, &"value".into()).and_then(|v| v.dyn_into()) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("bad incoming stream: {e:?}");
                        continue;
                    }
                };
            let send = read_send.clone();
            spawn_local(async move {
                match read_stream_to_end(stream).await {
                    Ok(bytes) => match bincode::deserialize::<ServerPacket>(&bytes) {
                        Ok(packet) => {
                            let _ = send.send(packet);
                        }
                        Err(e) => warn!("failed deserializing packet {e:?}"),
                    },
                    Err(e) => warn!("stream read failed: {e:?}"),
                }
            });
        }
    });

    // 4. Write loop: drain outgoing ClientPackets. The channel is filled by
    //    the sim on the same (only) thread, so poll it with a task that
    //    yields to the event loop between drains.
    spawn_local(async move {
        loop {
            while let Ok(packet) = client_recv.try_recv() {
                let payload = bincode::serialize(&packet).unwrap();
                let stream_fut = transport.create_unidirectional_stream();
                match JsFuture::from(stream_fut).await {
                    Ok(stream) => {
                        let stream: web_sys::WritableStream = match stream.dyn_into() {
                            Ok(s) => s,
                            Err(e) => {
                                warn!("bad outgoing stream: {e:?}");
                                continue;
                            }
                        };
                        let writer = stream.get_writer().expect("stream locked");
                        let chunk = Uint8Array::from(payload.as_slice());
                        if let Err(e) = JsFuture::from(writer.write_with_chunk(&chunk)).await {
                            warn!("write failed: {e:?}");
                        }
                        let _ = JsFuture::from(writer.close()).await;
                    }
                    Err(e) => warn!("open uni failed: {e:?}"),
                }
            }
            // Yield ~one frame so the sim can enqueue more packets.
            sleep_ms(16).await;
        }
    });

    Ok(())
}

async fn read_stream_to_end(stream: web_sys::ReadableStream) -> Result<Vec<u8>, JsValue> {
    let reader: ReadableStreamDefaultReader = stream.get_reader().dyn_into()?;
    let mut bytes = Vec::new();
    loop {
        let next = JsFuture::from(reader.read()).await?;
        if Reflect::get(&next, &"done".into())?.is_truthy() {
            return Ok(bytes);
        }
        let chunk: Uint8Array = Reflect::get(&next, &"value".into())?.dyn_into()?;
        bytes.extend(chunk.to_vec());
    }
}

fn parse_hash_json(text: &str) -> Option<(String, u16)> {
    // {"sha256_hex":"…","port":6467} — hand-rolled to avoid a serde_json
    // dependency in the client for one file.
    let hex = text.split("\"sha256_hex\":\"").nth(1)?.split('"').next()?;
    let port = text
        .split("\"port\":")
        .nth(1)?
        .trim_end_matches(['}', ' ', '\n'])
        .parse()
        .ok()?;
    Some((hex.to_string(), port))
}

async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        web_sys::window()
            .unwrap()
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms)
            .unwrap();
    });
    let _ = JsFuture::from(promise).await;
}
```

API-drift notes: web-sys 0.3.103 setters are the `set_*` form shown; older docs show builder-style `algorithm(...)`. `Object` import may be unused — drop it if so. If `WebTransport::ready()` returns `Promise` vs `js_sys::Promise` mismatches, `JsFuture::from` accepts both paths once cast. The unused `Object` import and exact `dyn_into` chains are compiler-guided fixes; the module structure and channel contract are not negotiable.

- [ ] **Step 3: Mode select in `sim_driver.rs`**

Replace `SimDriver`'s `local_server` field with a mode enum:

```rust
enum SimMode {
    /// Embedded single-player authority (today's behavior).
    Offline {
        local_server: LocalServer,
        server_tick_ms: f64,
    },
    /// Real server over WebTransport; the authority ticks remotely.
    Online,
}
```

`start_wasm_sim` picks by URL (`?server` present → online):

```rust
    let online = web_sys::window()
        .and_then(|w| w.location().search().ok())
        .map(|s| s.contains("server"))
        .unwrap_or(false);

    let mode = if online {
        crate::netcode_web::connect(server_send, manager.client_packet_recv());
        SimMode::Online
    } else {
        let local_server = LocalServer::new(manager.client_packet_recv(), server_send)
            .expect("failed to build offline world");
        SimMode::Offline { local_server, server_tick_ms: 0.0 }
    };
```

`drive_sim`: the server-tick accumulator + `local_server.tick()/pump()` calls move inside the `SimMode::Offline` arm; client tick generation and `manager.pump(server_recv)` stay unconditional. Client tick pacing, `SyncClock` handling, and `pump` semantics are untouched.

Note: `web_sys::window()` needs the `Window`/`Location` features from Step 1. `main.rs` gains `#[cfg(target_arch = "wasm32")] mod netcode_web;` next to `mod sim_driver;`.

- [ ] **Step 4: Verify both targets**

Run: `cargo check -p client --target wasm32-unknown-unknown && cargo test -p client && cargo check -p client && cargo build -p client --target wasm32-unknown-unknown`
Expected: all PASS, no new warnings on either target. (The offline-mode client tests exercise `LocalServer` exactly as before.)

- [ ] **Step 5: Commit**

```bash
git add crates/client .cargo/config.toml
git commit -m "feat(client): WebTransport netcode for wasm, ?server mode select"
```

---

**Amendment (2026-07-05, found by Task 5's E2E, fix approved by user as Task 6):** the browser
connects over WebTransport but the first `Region` snapshot fails to deserialize on wasm32:
vendored rapier's `RigidBodyIds::{active_island_id, active_set_id}`
(`crates/rapier/src/dynamics/rigid_body_components.rs:1033-1043`) use `usize::MAX` as a
sentinel; bincode encodes a 64-bit server's sentinel as `u64::MAX`, which cannot fit a 32-bit
`usize`. Resolution: a serde adapter on exactly those fields mapping the sentinel cross-width
(`usize::MAX` ⇄ `u64::MAX`, other values must fit or error). On 64-bit builds the emitted
bytes are provably unchanged (bincode already encodes `usize` as `u64`), so native wire format
and CRC state hashes are untouched; 32-bit builds now emit/accept the same bytes as 64-bit,
restoring cross-architecture parity — which is the vendored forks' entire reason to exist.
Whack-a-mole bound: if the E2E trips further sentinel fields, fix at most 2 more the same way,
then stop and report.

### Task 5: End-to-end verification + docs

**Files:**
- Modify: `CLAUDE.md` (browser-multiplayer dev loop)
- No other planned source changes; fixups only where the browser disagrees.

- [ ] **Step 1: Full-stack headless check**

From the repo/worktree root:
```bash
cargo run --bin server &            # quinn :6466 + webtransport :6467, writes assets/webtransport-cert-hash.json
WASM_SERVER_RUNNER_CUSTOM_INDEX_HTML=crates/client/index.html \
  cargo run -p client --target wasm32-unknown-unknown &   # serves the page + assets
curl -s http://127.0.0.1:1334/assets/webtransport-cert-hash.json   # expect the JSON
chromium --headless=new --disable-gpu --enable-unsafe-swiftshader --no-sandbox \
  --enable-logging=stderr --v=0 --virtual-time-budget=30000 \
  "http://127.0.0.1:1334/?server" 2> console.log
grep -c "webtransport connected" console.log    # expect 1
grep -c "Region recieved and loaded" console.log # expect 1
grep -ciE "panicked|RuntimeError" console.log    # expect 0
```
Server log should show `webtransport client 0 connected`. Kill both background processes after.

Known contingency: headless Chromium requires certificate hashes to be ECDSA + ≤14 days — if `ready()` rejects with a cert error, dump `assets/webtransport-cert-hash.json` and the server's identity parameters before touching code; the most likely cause is hash/DER mismatch (hash the wrong cert in a chain) or an RSA identity.

- [ ] **Step 2: Native regression**

`cargo run --bin server` + native `cargo run --bin client`: connect, move. Offline wasm regression: load the page WITHOUT `?server`, confirm the old offline flow (console: region loaded, no connection attempt).

- [ ] **Step 3: Manual acceptance (human)**

Server up; native client connected; browser tab at `http://127.0.0.1:1334/?server`. Each client sees the other's avatar move. This is the feature's definition of done and needs human eyes.

- [ ] **Step 4: Document**

CLAUDE.md, extend the "WASM build" section:

```markdown
Browser multiplayer (WebTransport): start the native server (`cargo run --bin
server` — it also opens a WebTransport ingress on 127.0.0.1:6467 and writes
`assets/webtransport-cert-hash.json` for the page), then open the wasm client
with `?server` appended: `http://127.0.0.1:1334/?server`. Without the query
param the wasm build stays offline single-player. See
`docs/superpowers/specs/2026-07-05-webtransport-netcode-design.md`.
```

- [ ] **Step 5: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: browser multiplayer dev loop (WebTransport)"
```
