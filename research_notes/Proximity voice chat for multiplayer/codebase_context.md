# Codebase context for proximity voice chat (repo `/home/dawids/Projects/mmo`, branch `develop`, read 2026-11-14)

Sources are repository files (`path:line`). "Inference" means I concluded it, not read it. Nothing was built or run.
No `.env` / `*.tfvars` content was read.

## 1. Transport: how clients connect; are QUIC datagrams used; what would unreliable audio need

### Takeaway
Everything on the wire today is **one QUIC unidirectional stream per packet** (reliable, unordered across streams), on both native quinn and browser WebTransport; **no code sends or receives a QUIC/WebTransport datagram anywhere**. The pinned libraries (quinn-proto 0.11.13, wtransport 0.7.1) do support datagrams and quinn's default transport config has them enabled, so a datagram path is an addition at four edges (server quinn, server wtransport, native client, browser page), not a library change.

### Cited Findings
- Native client: `quinn` 0.11 (`rustls-ring`, `runtime-tokio`) — `Cargo.toml:184`; lock has quinn 0.11.9 / quinn-proto 0.11.13 — `Cargo.lock:6853-6874`. Browser ingress on the server: `wtransport` 0.7 (`self-signed`, `ring`) — `Cargo.toml:234`, lock 0.7.1 — `Cargo.lock:10705`.
- Ports: QUIC `DEFAULT_SERVER_PORT = 6466`, WebTransport `WEBTRANSPORT_PORT = 6467` (`VOXEL_WT_PORT`), bind address `VOXEL_BIND_ADDR` — `crates/server/src/lib.rs:33-66`.
- Wire format, server WebTransport ingress doc: "Wire format is identical: one uni-stream per bincode packet" — `crates/server/src/webtransport.rs:1-4`. Server send: `conn.open_uni()` → `write_all` → `finish` for quinn and for wtransport — `crates/server/src/lib.rs:226-258` (`ClientSink::send_packet`). Server receive loops: `accept_uni()` then read-to-end with a 64 MiB cap (`MAX_PACKET_BYTES`) — `crates/server/src/webtransport.rs:27-29,238-256`, `crates/server/src/lib.rs:1480-1495`.
- Native client: Hello on `connection.open_uni()` (`netcode.rs:358-360`), receive `accept_uni()` (`:367`), each outgoing packet a new `open_uni()` (`:457-464`) — `crates/client/src/netcode.rs`.
- Browser client: "shuttles one bincode packet per uni-stream in each direction — the same wire format quinn speaks natively" — `crates/client/src/netcode_web.rs:1-7`. The `web-sys` features enabled are WebTransport streams only (`WebTransport`, `WebTransportOptions`, `WebTransportHash`, `WebTransportCloseInfo`, `ReadableStream*`, `WritableStream*`, `WebTransportReceiveStream`, `WebTransportSendStream`); **no `WebTransportDatagramDuplexStream`** — `crates/client/Cargo.toml:108-120`.
- Grep for `datagram` across `crates/` (excluding rapier/parry), `scripts/`, `deploy/`, `website/` finds only a test helper counting UDP packets on a socket (`crates/client/src/netcode.rs:591-650`, `crates/client/src/instance.rs:4150-4183`); no `send_datagram` / `read_datagram` call exists.
- Server QUIC endpoint uses `TransportConfig::default()` unchanged — `crates/server/src/lib.rs:447-448`. In the pinned quinn-proto the default is `datagram_receive_buffer_size: Some(STREAM_RWND)`, `datagram_send_buffer_size: 1024*1024` — `~/.cargo/registry/src/index.crates.io-*/quinn-proto-0.11.13/src/config/transport.rs:384-385` (so datagrams are negotiated by default on the native path).
- wtransport 0.7.1 exposes `Connection::receive_datagram`, `send_datagram`, `max_datagram_size` — `~/.cargo/registry/src/index.crates.io-*/wtransport-0.7.1/src/connection.rs:272,297,350`.
- WebTransport server config sets only keep-alive 3 s and max idle timeout 10 s — `crates/server/src/webtransport.rs:63-76`.
- Packet framing: `game::encode_packet` / `decode_packet` — `crates/game/src/protocol.rs:1266,1287`; one format byte then raw bincode, or (`FRAME_LZ4 = 1`) a LE `u32` length + lz4 block when the bincode is over `PACKET_COMPRESS_THRESHOLD = 4096` bytes — `protocol.rs:1207,1228,1258-1300`.
- Packet enums: `ClientPacket` (`protocol.rs:737-781`) and `ServerPacket` (`protocol.rs:873-930`). No voice/audio/chat variant exists in either.
- All outgoing server traffic funnels through ONE shared writer task draining a crossbeam channel `(Option<ClientId>, ServerPacket)`, encoding and sending per client with a `SEND_DEADLINE` timeout; comments record two past incidents where a stalled client or a blocked `recv()` wedged that writer for every client — `crates/server/src/lib.rs:455-490`.
- The client already coalesces mouse-motion packets to one per 30 ms because "each packet costs its own QUIC uni-stream" — `crates/client/src/renderer/input.rs:50-60`.
- Server-to-server link (`crates/svc-link`): QUIC with a pinned self-signed cert + shared service token; "Frames are one bidirectional stream of `u32` big-endian length + `game::encode_packet`" — `crates/svc-link/src/lib.rs:1-14`. Peer messages are `PeerMsg::{Hello, Reliable{seq,body}, Ack, Volatile}` — `crates/game/src/peer/msg.rs:224-230`; "Volatile" there means "not replayed after a drop", it still rides the same reliable stream (no datagram call in `crates/server/src/peer_link.rs`). Frame cap 64 MiB — `peer/msg.rs:213`.
- Sim tick period is 50 ms (`TICK_RATE: u64 = 50`, a period in ms) — `crates/game/src/lib.rs:147`.
- Native QUIC connect skips server certificate verification (`SkipServerVerification`); TODO says to pin a hash or use a real cert "before any `jwt` host is public" — `TODO.md:153`, `crates/client/src/netcode.rs:508-553`.

### Inferences
- Carrying unreliable audio on the existing connection would need: (a) server quinn read loop + `ClientSink` gaining a datagram path beside `open_uni`; (b) the same for the wtransport session (`receive_datagram` / `send_datagram`); (c) native client `connection.send_datagram` / `read_datagram` in `netcode.rs`; (d) browser: add the `WebTransportDatagramDuplexStream` web-sys feature and readers/writers in `netcode_web.rs`; (e) a relay path that does NOT go through `WorldManager` / region threads or the shared writer task (audio at 20–60 ms frames per speaker per listener would otherwise add load exactly where the writer-wedge incidents happened).
- A voice frame must fit one datagram (roughly < 1200 bytes); `encode_packet`'s 1-byte header is small enough, but its lz4 branch is irrelevant for Opus-sized payloads. CLAUDE.md's rule "never raw bincode on the wire" (CLAUDE.md, Conventions) means a voice datagram format would have to be a deliberate, documented exception or use `encode_packet`.
- Sending audio over the existing uni-stream-per-packet path would work functionally but is reliable delivery (head-of-line blocking per stream is avoided since each packet is its own stream, yet lost packets are retransmitted late rather than dropped) and costs a stream open per frame.
- Audio is not sim state, so it sits outside the rollback/determinism rules; it must never enter `GameEvent`s (which are hashed and replayed).

### Gaps
- Whether datagrams actually pass end to end through the deployed path (host firewall, Docker UDP port mapping) was not tested; nothing in the repo measures datagram loss or MTU.
- wtransport's default for datagram support on the server builder was not read (only that the API exists); quinn's default was read.
- Whether browsers targeted (the page needs WebTransport already) all support WebTransport datagrams was not researched (web research was out of scope).

## 2. Multi-host (R3): reaching nearby players on other hosts; where positions live; interest management

### Takeaway
A client holds one connection per host and subscribes to whole regions; a region streams its full state to every subscriber, so the only "interest management" is region subscription (the near window + fortress neighbourhood), not distance. Player positions live inside region sims (`Ecs.isometry` + `player_entites`); the manager knows only which region holds each player (`homes`). Two nearby players can be on different hosts, and hosts already have direct peer links and an observer-session mechanism that voice could reuse or that a separate media server would bypass.

### Cited Findings
- Client `ConnectionSet` design: first connection (login) is the HOME; "input, UI requests, LOD and the world clock go to the home"; a region another host owns is announced as `ServerPacket::RegionElsewhere { region, endpoint, ticket }`, the client opens/reuses a connection to that host with the observer ticket; the home follows the body via `ServerPacket::HomeHop` — `crates/client/src/connections.rs:1-15`, `crates/game/src/protocol.rs:919-930`.
- Idle non-home connection closes after 10 s without regions (`CONN_IDLE_CLOSE_MS`), host closes an idle observer after 30 s (`OBSERVER_IDLE_CLOSE_MS`); retry/backoff constants — `connections.rs:21-37`, `crates/game/src/world_manager/mod.rs:56`.
- `Outgoing::{Home, Region(RegionId, _), Conn(u32, _)}` is how the client routes packets per connection — `connections.rs:57-67`.
- Observer credential: `Credential::Observer { client, nonce }` "A session to a host that does not hold the client's body" — `crates/game/src/player.rs:114-121`; minted by the home host and pushed to the other host as `Reliable::ObserverOrder { client, player, nonce, ttl_secs }` over the peer link — `crates/game/src/peer/msg.rs:78-80`; edge keeps at most 4096 orders, 4 per client — `crates/server/src/auth_edge.rs:57-62`.
- `HostEndpoint { ordinal, quic, webtransport, cert_hash }` is what a client is told about another host — `crates/game/src/peer/msg.rs:14-19`.
- Hosts talk directly over peer links (crossings, halo, intents, forwarded input, player hand-offs); `Volatile::Input { client, kind, stamp }` already forwards a client's input to the host that runs the region — `peer/msg.rs:190-208`; CLAUDE.md "Many hosts (R3)".
- Session ids are world-unique and the same on every host: `session_client_id(player, session_epoch)` = `PlayerId` above 24 bits of epoch — `crates/game/src/player.rs:27-33`. `PlayerId(u64)` is "the stable identity of a player" — `player.rs:23-25`.
- Positions: per-region `Ecs` has `isometry: Component<Pose>`, `kind`, `id: EntityId` — `crates/game/src/state.rs:367-390`; the region maps `player_entites: BTreeMap<ClientId, EntityKey>` and `clients: BTreeMap<ClientId, Client>` — `state.rs:269-271`. Poses are region-local ("Sims stay region-local; the world offset exists only at the render boundary" — CLAUDE.md Regions & seams; `RegionCoords::world_offset()` used at `crates/client/src/renderer/commander/mod.rs:241-244`).
- Manager level: `sessions: BTreeMap<ClientId, Session>` ("Connected clients and their subscriptions") and `homes: BTreeMap<ClientId, RegionCoords>` ("which region holds this client's player entity") — `crates/game/src/world_manager/mod.rs:923-926`. Each `RegionLink` has `subscribers: BTreeSet<ClientId>` — `world_manager/mod.rs:675`; region output is fanned out to all subscribers — `world_manager/mod.rs:2033-2068`.
- Subscription is by region: `ClientPacket::RequestRegionConnection(RegionId)` / `ReleaseRegionConnection` — `protocol.rs:751-753`. Client near window: `DEFAULT_NEAR_RADIUS` = 0 regions at region size ≥ 256 blocks (else 2), max 3; plus the fortress neighbourhood `FORTRESS_NEAR_RADIUS = 1` (fortress region + 8 neighbours) "in BOTH play modes" — `crates/client/src/world_scale.rs:18-29`. Region edge is a build-time constant, 64/128/256/512 blocks of 1 m; play recipes default to 256 — `protocol.rs:48,62`, CLAUDE.md "Region size".
- Halo: each region mirrors neighbours' terrain within `HALO_BLOCKS = 8` blocks of the shared edge, plus halo entities (`HaloEntityDelta`) — `crates/game/src/halo.rs:1-30,67`, `protocol.rs:622-649`. That is the only cross-region "nearby" data a region sim has.
- The client's predicted regions contain every entity of each subscribed region, including other players' bodies, with poses (rendered by `renderer/avatar.rs` via `SimKind`/`SimTarget`) — `crates/client/src/renderer/avatar.rs:1-40`; the local player is `LocalPlayer(Option<ClientId>)` — `crates/client/src/renderer/mod.rs:94`.
- No player display names travel in the game protocol: grep for `PlayerName|player_name|display_name` in `protocol.rs`, `state.rs`, `renderer/hud.rs`, `renderer/bridge.rs` is empty; no nameplate code exists (grep `nameplate|NameTag` empty). `PlayerName` exists only as a `local` credential — `player.rs:74-78`.
- Multi-host is "BUILT 2026-11-13, go/no-go PENDING … nothing has been run live or measured" — `TODO.md:157`. Ownership is by super-region (16 regions, `VOXEL_SUPER_REGION_REGIONS`) — CLAUDE.md "Owner epochs".

### Inferences
- With a region edge of 256 m and typical voice range of tens of metres, speakers in earshot are in the listener's own region or one of the 8 neighbours. A client in a world at `near_radius` 0 subscribes only to its own region (plus the fortress 3×3), so it may NOT have a predicted copy of a neighbour region's players; only the 8-block halo band's entities are mirrored. Client-side position data for attenuation is therefore complete only within subscribed regions.
- The server side has no single place that knows "who is within N metres of whom": distance needs poses that live in region threads (possibly on different hosts). Options visible in the code: (a) region threads emit a low-rate "voice neighbours" output per player (non-sim side channel), (b) the client decides audibility/attenuation itself from the poses it predicts and the relay only filters by region adjacency (`homes` + neighbour set, which the manager already knows), (c) an external SFU with rooms keyed by region/super-region and client-side spatialisation.
- Cross-host reachability for a self-built relay: either voice frames ride each client's connection to the host holding the speaker's region (listeners already hold observer connections to hosts whose regions they subscribe to), or hosts forward frames over peer links (stream-based today, 64 MiB frame cap, `Volatile` class fits "never replayed"). A separate media server avoids both because it is host-independent; then identity mapping uses the world-unique `ClientId`/`PlayerId`.
- Body hand-off between hosts (`HomeHop`, 5 s `HANDOFF_SESSION_GRACE_MS`) would move a speaker's uplink mid-sentence in a host-relayed design; a media-server design is unaffected.
- To label speakers, a name source has to be added (login `account_name`, or the `local` `PlayerName`), since none reaches other clients now.

### Gaps
- `Session` struct fields (whether it stores `PlayerId`, home flag, etc.) were not read in full.
- Whether halo entity mirrors include other players' bodies in practice was not verified beyond the type existing.
- No measurement exists of how many players share a region; R3 itself is unaccepted.

## 3. Auth: login service, tokens, claims, what a voice service could verify or a host could mint

### Takeaway
The login service signs short-lived (300 s) EdDSA/Ed25519 "join" JWTs with audience `mmo-host` and publishes its public keys at `/.well-known/jwks.json`; Keycloak (RS256) is the identity provider in front of it. There is no generic access token for other services: a voice service would either verify a new JWT kind minted by the login service (same keys, different `aud`), or a game host would mint a media-server room token after it has authenticated the session (hosts hold no signing key today, only verify).

### Cited Findings
- `crates/auth`: "Join JWTs: EdDSA (Ed25519) signing on the login service, verification on the game hosts"; keys are PKCS#8 PEM files in a directory, newest signs, all published as JWKS; `kid` = first 16 hex chars of SHA-256 of the public key; leeway 30 s — `crates/auth/src/lib.rs:1-28,99-160`. JWK is `kty OKP / crv Ed25519 / alg EdDSA` — `auth/src/lib.rs:163-185`. `Signer::sign` takes only `&JoinClaims` — `auth/src/lib.rs:148-153`.
- `JoinClaims { sub (account id), world (registry id), player_token (hex, "not a secret"), session_epoch, aud = "mmo-host", iat, exp }` — `crates/login-api/src/lib.rs:25-42`; `JOIN_JWT_TTL_SECS = 300`, `HOST_AUDIENCE = "mmo-host"`, `REFRESH_TTL_DAYS = 30` — `login-api/src/lib.rs:8-13`.
- Login HTTP routes: `/v1/login/oidc`, `/v1/config`, `/v1/refresh`, `/v1/logout`, `/v1/worlds`, `/v1/join`, `/.well-known/jwks.json`, `/healthz`, `/metrics` — `crates/login/src/api.rs:513-524`, paths at `login-api/src/lib.rs:17-23`. `/v1/join` rotates the refresh token and returns a `JoinTicket` (JWT + host QUIC address + WebTransport URL + optional dev cert hash) — `login-api/src/lib.rs:44-56,96-106`, `login/src/api.rs:374-460`.
- Keycloak: the login service verifies Keycloak's access token "RS256 against the provider's published keys (JWKS), then issuer, audience, expiry and a verified email" — `crates/login/src/oidc.rs:1-4,186-225`; discovery from `{issuer}/.well-known/openid-configuration` — `oidc.rs:167-177`. `LoginResponse` carries the account's display name from the identity provider — `login-api/src/lib.rs:59-66`.
- Keycloak realm clients (OpenTofu): `zulip` (CONFIDENTIAL), `game` (PUBLIC, with an audience mapper adding the login-service audience), `site` (PUBLIC) — `deploy/modules/keycloak-realm/clients.tf:2-80`. CLAUDE.md names `VOXEL_OIDC_AUDIENCE` = `mmo-login`, `VOXEL_OIDC_CLIENT_ID` = `game`.
- Host side: `VOXEL_AUTH=local|jwt`; `jwt` checks signature, expiry, audience, world, then `session_epoch` against a pending spawn order pushed by the login service; "The JWT is checked only at connect"; JWKS fetched from `VOXEL_LOGIN_URL` + `PATH_JWKS`, refetch at most every 30 s — `crates/server/src/auth_edge.rs:1-45`. `local` mode: "anyone may claim any name, so a public server should run `jwt`" — `auth_edge.rs:6-9`.
- Login → host control channel exists: `LoginToHost::{SpawnPlayer, Kick}` / `HostToLogin::{SpawnAck, SessionEnded, KickAck}` over `svc-link` with the shared `VOXEL_SERVICE_TOKEN[_FILE]` — `login-api/src/lib.rs:152-185`, `crates/svc-link/src/lib.rs:1-14`, `crates/login/src/host_link.rs`. Kicks are durable until acked.
- Client holds: refresh token + account email + display name in `LoginSession` — `crates/client/src/login/flow.rs:351-361`; the join ticket in `PendingJoin`, presented as `Credential::Jwt` — `crates/client/src/session.rs:120-130`.
- Signing key in the orion deployment is an OpenTofu `tls_private_key.signing` stored as a k8s secret (`0001.pem`), with a comment "Never replace tls_private_key.signing: that invalidates the join tickets" — `deploy/modules/login/main.tf:39,70-80`.
- The only rustls crypto provider allowed is ring; "keep `aws-lc-rs` out of the tree" — CLAUDE.md Server/store; restated at `crates/server/src/lib.rs:434-437` and `svc-link/src/lib.rs:13-14`.
- TODO: "rate limiting on the login endpoints is deferred" — `TODO.md:147`. The current public web deploy is a `local`-auth server (anyone may claim any name) — `TODO.md:150`.

### Inferences
- A LiveKit-style room token is an HS256 JWT signed with the media server's API secret; nothing in the repo holds such a secret. The natural minting point is either (a) the login service at `/v1/join` time or a new `/v1/voice` route (it already knows account, world, display name, and holds secrets in k8s), or (b) the game host after `Hello` succeeds (it knows the live `ClientId`, region and host, and could re-mint on region change), delivered as a new `ServerPacket`. Option (b) needs the secret on every host and works in `local` auth too; option (a) keeps secrets off hosts but the login service does not know positions.
- A self-built voice service could verify the existing JWKS (Ed25519), but the join JWT itself is unsuitable as a voice credential: audience is `mmo-host`, it lives 300 s, and it is consumed against a spawn order. A new claims type with its own `aud` would be needed (`Signer::sign` is typed to `JoinClaims`).
- If voice rides the existing game connections, no new auth is needed at all: the session is already authenticated (including observer sessions via ticket).
- Kick/revoke: an external voice session would need to be ended on `LoginToHost::Kick` too (short token TTL or an explicit remove-participant call).
- A media-server SDK dependency must not pull `aws-lc-rs` into the workspace (feature unification is on).

### Gaps
- Login config (`crates/login/src/config.rs`) and the world registry schema (`store.rs`) were not read in detail; whether per-world metadata could carry a "voice endpoint" field is unverified.
- No moderation, mute, block or report data model exists anywhere (grep for chat/voice shows only the client-local chat).

## 4. Client audio today

### Takeaway
The client has **no audio at all**: no audio crate in the lock file, Bevy is built with `default-features = false` and without `bevy_audio`, there is no microphone capture, no listener, no sound asset pipeline. Voice chat would introduce the project's first audio stack (capture, codec, jitter buffer, mixing/spatialisation, output) on both native and wasm.

### Cited Findings
- `Cargo.lock` has none of: `cpal`, `rodio`, `kira`, `opus`/`audiopus`, `bevy_audio`, `bevy_kira_audio`, `webrtc`, `str0m`, `livekit*`, `oddio`, `symphonia`, `alsa`, `coreaudio-rs`, `oboe`, `lewton`, `hound`, `fundsp` (grep of `^name = "…"` returned only bevy, jsonwebtoken, quinn, quinn-proto, ring, rustls, web-sys, wtransport).
- Workspace Bevy: `version = "0.19.1", default-features = false`, features `std, bevy_winit, bevy_window, bevy_render, bevy_core_pipeline, bevy_anti_alias, bevy_post_process, bevy_pbr, bevy_gizmos, bevy_gizmos_render, bevy_asset, bevy_log, bevy_state, multi_threaded, tonemapping_luts, ktx2, smaa_luts, zstd_rust, …` — no `bevy_audio`, no `vorbis` — `Cargo.toml:118-140`. wasm adds only `webgl2` — `crates/client/Cargo.toml:103`.
- Grep for `AudioPlugin|AudioSource|SpatialListener|AudioListener|getUserMedia|microphone` in client/game/server/slint-ui/login/website: no hits. The only audio-related text is the stock Bevy/wasm-server-runner page snippet that proxies `AudioContext` to resume contexts on first user interaction — `crates/client/index.html:315-365`.
- `docs/claude-md-full-reference-2026-11-08.md`, `TODO.md`, `docs/game-design.md`: no match for `audio|sound|music`.
- web-sys features list contains no `MediaDevices`, `MediaStream`, `AudioContext`, `AudioWorklet` — `crates/client/Cargo.toml:108-120`.
- Compile-time/size sensitivity is a stated project concern (feature unification, client `-Clto=off`, hand-written chat parser chosen over `clap` partly for "its size in the wasm build") — CLAUDE.md "Compile time"; `docs/superpowers/specs/2026-11-14-creative-mode-and-chat-design.md` (Commands section).

### Inferences
- Native capture/playback would need `cpal` (ALSA dev headers on Linux builds, CoreAudio/WASAPI elsewhere) and an Opus encoder; a C `libopus` binding affects the wasm build (would need a pure-Rust or browser-native codec path there: WebCodecs `AudioEncoder`, or WebRTC's built-in Opus if a WebRTC/SFU route is taken).
- Because there is no game audio, there is no existing mixer or listener to attach spatial voice to; the "ear" transform must be read from the render camera/body directly (section 6).
- The page's existing `AudioContext` resume hook means a Web Audio context created by wasm code will be resumed on the first user gesture without extra work.
- Enabling `bevy_audio` just for voice is not required; a self-contained voice module (own cpal stream) would avoid changing the pinned Bevy feature set.

### Gaps
- Target platforms for the native client beyond Linux (the dev box) are not stated in the files read.
- Whether the build environment/CI has ALSA headers is unknown.

## 5. wasm constraints: page vs worker, implications for mic and playback

### Takeaway
In the browser, Bevy rendering and all WebTransport sessions run on the page (main thread); the sim runs in a separate-instance Web Worker and compute jobs in a worker pool, all talking by bincode bytes in transferred ArrayBuffers; there is no SharedArrayBuffer and no cross-origin isolation. Mic capture (`getUserMedia`) and Web Audio playback must be driven from the page, which is also where the network sockets already are, so voice frames need not cross the worker boundary.

### Cited Findings
- "Shared-memory wasm threads (`+atomics`, `-Zbuild-std`, COOP/COEP) are not an option for this client: wgpu drops `Send`/`Sync` … So the workers here are separate wasm instances that talk by message passing"; roles `sim` and `pool`; messages are bincode bytes in a transferred `ArrayBuffer`; every worker instantiates the same compiled module (tens of MB each); `?workers=0` turns it off — `crates/client/src/web_worker/mod.rs:1-45`.
- "The WebTransport connections stay on the page … and move raw frames": each server frame is posted to the sim worker tagged with its connection key; a worker-hosted transport was tried 2026-11-02 and abandoned (server writer wedged, unexplained) — `crates/client/src/web_worker/sim.rs:15-24`, `crates/client/src/netcode_web.rs:9-16`.
- Page → worker framing: tag byte `TAG_TO_SIM = 0` / `TAG_SERVER_FRAME = 1` + LE `u32` connection key + frame — `crates/client/src/web_frame.rs:8-28`.
- One WebTransport session per server host, each pinned by `HostEndpoint.cert_hash` (`serverCertificateHashes`; self-signed ECDSA ≤ 14 days in dev) or a real certificate when deployed — `netcode_web.rs:18-24,420-484`, `crates/server/src/webtransport.rs:6-11,135-145`.
- The sim worker ticks on its own timer, not on page frames — `web_worker/sim.rs:26-28`.
- No COOP/COEP/Permissions-Policy headers are configured: grep for `Cross-Origin|Permissions-Policy|microphone` in `crates/client/index.html`, `deploy/web`, `deploy/modules/site`, `scripts/*.sh`, `website` is empty.
- The deployed page is served over HTTPS at `https://orion.factoriosurvivalworld.com/` via an external Caddy proxy (`deploy/web/compose.yml:1-13`); dev pages are localhost (`just web`, `justfile:829-835`).
- wasm build is `wasm32-unknown-unknown` + wasm-bindgen `--target web` + `wasm-opt -O3` from a pinned binaryen — `scripts/web_release.sh:24-53`. Browser renderer is WebGL2 (`crates/client/Cargo.toml:103`).
- Browser client frame codec is tested on wasm32 by `just client-wasm` (`justfile:1638-1644`; CLAUDE.md Testing).

### Inferences
- `getUserMedia` needs a secure context (HTTPS or localhost) and a user gesture/permission prompt: both deployment and dev origins qualify. It is unavailable in workers, so capture lives on the page next to Bevy's main loop.
- The page's main thread is shared with Bevy rendering (single-threaded task pools on wasm), so encode/decode/mix should run off it: an `AudioWorklet` (its own thread, no SAB needed with `postMessage`/MessagePort) and/or browser-native codecs (WebCodecs or WebRTC) rather than wasm Opus in the render loop. No SharedArrayBuffer ring buffer is possible without COOP/COEP, which the project has ruled out for other reasons.
- Since the sockets are on the page, a WebTransport-datagram voice path would stay entirely on the page (capture → encode → datagram; datagram → decode → worklet) and never touch the sim worker; only positions for spatialisation come from the app (which already receives region updates from the worker).
- If a WebRTC SFU is used instead, the browser gets echo cancellation/noise suppression/jitter buffering for free from the browser stack via JS interop, while the native client would need a Rust WebRTC stack — an asymmetric implementation.
- With one WebTransport session per host, a browser voice path over game connections must pick the right session per speaker/listener (same routing problem as native, section 2).

### Gaps
- Echo cancellation / noise suppression for the NATIVE client is unaddressed by anything in the repo.
- No evidence of which browsers are supported/tested beyond a Firefox profiling recipe (`just firefox-profile`, CLAUDE.md) and Chrome mentions in comments.

## 6. Game modes, "ear" and "mouth", existing chat, multiplayer intent

### Takeaway
Every client has a body in both modes; in Fortress it "stands inert while the commander camera flies free", in Adventure the player walks that body. So the mouth is unambiguous in Adventure (the body) and a design decision in Fortress (inert body vs camera focus point vs fortress site). A text chat exists since 2026-11-14 but is client-local only (lines are sent to nobody); the design docs say almost nothing about social features.

### Cited Findings
- `PlayMode { Fortress (default), Adventure }` "How a client plays: the commander camera or one walking body" — `crates/game/src/adventure.rs:17-23`; `Client.mode` is per-client rollback state — `crates/game/src/state.rs:150-155`.
- "Every client has a body in both modes; in Fortress it stands inert while the commander camera flies free." — `docs/superpowers/specs/2026-11-14-creative-mode-and-chat-design.md` ("What exists today").
- Commander camera state: `CommanderCamera { focus: Vec2 (ground point, world x/z in sim units), ground_y, distance, yaw, pitch, … }`, client-side Bevy resource reset per session — `crates/client/src/renderer/commander/mod.rs:157-172,486`. First person: `FirstPerson { on }`, toggled with F5 — `crates/client/src/renderer/commander/first_person.rs:38-50`. Top-down key V — `commander/top_down.rs:33`.
- The commander camera is client-only: the server knows the body's pose, not where the commander is looking (the focus is sent only as an argument of specific orders, e.g. `SetMode`/`SetCreative { at }` — chat design spec, Creative mode → Sim).
- The fortress neighbourhood (fortress region ± 1) stays subscribed in both modes; "in Adventure the fortress keeps living while the adventurer walks away" — `crates/client/src/world_scale.rs:23-29`.
- Chat: `crates/client/src/renderer/chat/{mod,command,run,ui}.rs`; "In-game chat, client-side only for now: what the player types is kept in memory for the session (`ChatLog`) and sent to nobody … Ordinary lines enter the log through `ChatLog::say` alone: that is where a transport (Zulip, later) takes them." — `chat/mod.rs:1-9,69-73`. Slint side `crates/slint-ui/ui/chat.slint`, wired in `game.slint:380-390`, `hud.slint:49-65`.
- Chat spec non-goals: "Sending chat to anyone, names, a server console, Zulip"; "Permissions … 'everyone has admin for now'"; status "not yet approved" (but code exists) — `docs/superpowers/specs/2026-11-14-creative-mode-and-chat-design.md`. An older investigation of server-side chat as sim events exists: `docs/superpowers/specs/2026-09-22-chat-and-commands-investigation.md` (not read).
- A Zulip server is deployed for the community at https://chat.factoriosurvivalworld.com with Keycloak OIDC login; the website links "Join the chat" — CLAUDE.md Deployment; `website/site/index.html:35`; `deploy/modules/keycloak-realm/clients.tf:2-11`.
- Game design: the only multiplayer statement is a table row — Fortress gives "player-built places to visit and quests raised by their troubles", Adventure gives "player adventurers who come and solve those troubles" — `docs/game-design.md:93`; "Player visits, ratings and similar for fortresses — out of scope for a long time" — `docs/game-design.md:137`. No mention of voice, chat, parties or friends in `docs/game-design.md` or `TODO.md` (grep).
- Co-op exists as a dev recipe: `run-two` = "two clients, one fortress" (CLAUDE.md Building and running); "Human look still owed: … `just run-two`" — `TODO.md:194`.
- A second login of the same account/name takes over the first session — `TODO.md:149-150`.

### Inferences
- Adventure mode maps cleanly to proximity voice: mouth = body pose, ear = body (first person) or camera.
- Fortress mode options, each with a cost: (a) mouth/ear = the inert body — players would hear nothing unless bodies happen to stand together, and the commander can be far from the body; (b) mouth/ear = commander camera focus — requires the client to report the focus to the server (new, non-sim data) or purely client-side attenuation with region-level relay; (c) non-proximity "fortress channel" for co-owners of one fortress (`run-two` case) plus proximity for visitors. This is an open product decision; nothing in the docs settles it.
- A top-down camera high above the ground makes "listener at the camera position" attenuate everything; listener at the focus ground point (x, z, `ground_y`) with camera yaw for panning is the workable reading of `CommanderCamera`.
- The chat's `TextEntry` typing gate (keys do not reach the game while a text field has the keyboard) must also govern a push-to-talk key.

### Gaps
- Whether two fortress players of one fortress share anything like a "team" identity server-side (`HeldFortress`, `fortress: Option<SiteId>` in `PlayerRecord`, `player.rs:145-146`) was not traced; it would be the key for a non-proximity fortress channel.
- No decision exists on voice in fortress mode, moderation, or whether voice is wanted at all — the docs are silent.

## 7. Single-player vs multiplayer detection

### Takeaway
A menu world spawns a local child server (`LocalHost` resource, `VOXEL_SINGLE_PLAYER=1`, `local` auth); an online world is entered through the login flow (`PendingJoin` with a JWT). The client can tell them apart by how the session started, but there is no explicit "is multiplayer" flag, and dev-server joins (`VOXEL_SKIP_MENU=1`, browser "Join server") are multiplayer with `local` credentials.

### Cited Findings
- Native single-player: `LocalHost::start*` launches the server binary with `VOXEL_SINGLE_PLAYER=1` on a free port; it is a Bevy resource present for the session — `crates/client/src/local_host.rs:44-46,63,126-177`; started from `PendingWorld` in `session.rs:636-667`; removed at session end — `session.rs:603`.
- Server: `single_player = env VOXEL_SINGLE_PLAYER == "1"`; such a server exits when its last client leaves — `crates/server/src/lib.rs:20-24,696`. "a client `Quit` is dropped (only single-player honours it)" — `TODO.md:177`.
- Session entry kinds on the client: `PendingWorld { entry_dir, terrain, name }` (local world), a pending browser "Join server" address, and `PendingJoin { ticket: JoinTicket, addr }` → `Credential::Jwt` — `crates/client/src/session.rs:100-130`. `AppState { MainMenu, MapGenerator, LoadWorld, Starting, InWorld, Login, Worlds }` — `session.rs:41-53`.
- `LoginSession` resource exists only for Play online — `crates/client/src/login/flow.rs:351-361`.
- Dev paths: `VOXEL_SKIP_MENU=1` connects to the dev server on 127.0.0.1:6466 (`just client`, `justfile:182-188`); `run-two` runs two clients against one server; the browser build has no local server at all (always joins a server) — `crates/client/src/netcode_web.rs:1-7`, `local_host` is `cfg(not(wasm32))` at `crates/client/src/frontend/mod.rs:341`.
- Nothing in `ServerPacket` tells the client the server's auth mode, single-player flag or player count — `crates/game/src/protocol.rs:873-930`.

### Inferences
- Simplest client rule: voice is off when a `LocalHost` resource exists (native menu world). Stricter rule: voice only when the session came from `PendingJoin` (authenticated accounts), which also excludes anonymous `local`-auth public servers where any name can be claimed.
- A more robust design is server-driven: the server (or login service) announces voice availability/credentials (e.g. a new `ServerPacket` or a field in `JoinTicket`); a single-player server simply never sends it. This also covers `run-two` dev testing by opt-in env.
- Any voice state (capture stream, connections, mute list) is per-world and must be registered with the session lifecycle (`add_session_reset` / `SessionScoped`), per CLAUDE.md "Session lifecycle", or it leaks into the next world.

### Gaps
- The exact resource holding a browser "Join server" pending address was not read (only its doc comment at `session.rs:108-110`).

## 8. Deployment: what exists, how services are exposed, where a media server would fit

### Takeaway
On the orion box, k3s (with its bundled Traefik, cert-manager, Route53 DNS) hosts only HTTP services — login, Keycloak, site, Zulip, PicoShare, Garage, monitoring — each as an OpenTofu root + module with an Ingress. **The game server is not in k8s**: the public build runs as a Docker Compose container on the same box with UDP 6466/6467 published directly and its TLS certificate borrowed from an external Caddy proxy. No UDP service is exposed through k3s today, so a media server (UDP-heavy) has no existing pattern to copy inside the cluster; the nearest precedents are the compose file's host-published UDP ports and the unused Agones "Dynamic UDP host ports" fleet module.

### Cited Findings
- Orion roots present: `bootstrap, garage, garage-buckets, keycloak, keycloak-realm, login, monitoring, picoshare, platform, registry, site, zulip` — `deploy/envs/orion/`. There is no `world` root under orion; `deploy/envs/local/world/main.tf` is the only user of `modules/world`, with `game_image` null "until the server has a container" — `deploy/envs/local/world/main.tf:1`.
- Orion `platform` root = cert-manager + Traefik settings + node sysctls; "CNPG etc. later" — `deploy/envs/orion/platform/main.tf`. Traefik is the one k3s ships, tuned by a `HelmChartConfig` in `kube-system` (static args: entrypoint read timeouts) — `deploy/modules/traefik-config/main.tf`, `…/templates/helmchartconfig.yaml`.
- Service pattern (login as example): namespace, secrets (random service token, signing key), `kubernetes_deployment_v1`, PDB, `kubernetes_service_v1` (port 80 → named port `http`), `kubernetes_ingress_v1` with `cert-manager.io/cluster-issuer` annotation + `ingress_class_name` + TLS secret, `aws_route53_record` A record, and a tiny local Helm chart for the `ServiceMonitor` — `deploy/modules/login/main.tf:49-348`. Same Ingress pattern in `site`, `picoshare`, `keycloak`, `garage`, `zulip`, `monitoring` (grep `ingress_class`).
- Login image: built from `deploy/images/login/Dockerfile`, pushed to ECR `mmo/login:latest`, pull secret refreshed by a CronJob every 6 h; release via `just orion-login-push` + `kubectl set image` — CLAUDE.md Deployment; `deploy/modules/login/ecr.tf`, `ecr-refresh.sh`.
- State/backend: S3 remote state (`deploy/envs/orion/backend.s3.tfbackend`), `just orion-plan|apply <root>` "always plan first" — CLAUDE.md.
- Grep for `UDP|hostPort|hostNetwork|NodePort|LoadBalancer` in `deploy/**/*.tf|yaml|tftpl|yml`: only `deploy/web/compose.yml` (UDP 6466/6467), `deploy/modules/world/main.tf` (Agones `portPolicy = "Dynamic"` UDP ports), and a comment in `deploy/modules/platform/main.tf:63` that LoadBalancer Services "never get an address" there (Agones allocator set to ClusterIP). `deploy/modules/platform/variables.tf:19`: the Agones host port range "Must be open (UDP) on the node's firewall".
- Public game deploy: `deploy/web/compose.yml` — project `mmo` in `/srv/mmo`; `server` = release binary in `debian:trixie-slim`, `ports: 6466/udp, 6467/udp`, `VOXEL_BIND_ADDR=0.0.0.0`, `VOXEL_WORLD_FILE=/srv/mmo/worlds/default.world`, `VOXEL_WT_CERT/KEY` pointing at the Let's Encrypt cert that the survival-world Caddy proxy obtains for `orion.factoriosurvivalworld.com`; `mmo-web` = Caddy file server for the web bundle. "The proxy, its Caddyfile, the host firewall (UDP 6466/6467) and the DNS record live in `~/Projects/mod/mods/survival-world`" (another repo) — `deploy/web/compose.yml:1-13,24-46`.
- Deploy mechanics: `just deploy` = `web-release` + release server build + `scripts/deploy_push.sh` to `root@5.9.7.138`; `just tunnel` forwards "the deploy box's k3s API" from the same host — `justfile:874-900` (so compose game server and k3s share one machine).
- That public server runs `local` auth with a file world (no `VOXEL_AUTH`, no Postgres in the compose env) — `deploy/web/compose.yml:36-43`; "a public `local` server (the current web deploy) lets anyone claim any name" — `TODO.md:150`. Its world must be re-forged before the next deploy (stud world refused) — `TODO.md:193`.
- Dev stack: `deploy/dev/compose.yml` (+ `keycloak/`), started by `just dev-login|dev-r2|dev-r3` (Postgres with `max_prepared_transactions`, Keycloak `kc-up`) — CLAUDE.md; `deploy/dev/README.md` (not read in full).
- Monitoring stack on orion: kube-prometheus-stack, Thanos, Loki, Tempo, Pyroscope, Alloy; ServiceMonitors per service — `deploy/modules/monitoring/`, `deploy/modules/login/charts/login-monitor/`.
- Object storage on the box: Garage, "one copy, no off-box backup" — CLAUDE.md Deployment.
- Domains in the repo are all `*.factoriosurvivalworld.com` (auth, chat, grafana, login, orion, s3, share, www) — grep across deploy/justfile/website/crates. The user's memory index says "all hosts live under errantry.renownandruin.com" and the player-facing name is Errantry; the repo does not reflect that domain yet — conflict noted, memory file body not read.
- No media/TURN/SFU component is present: grep for `livekit|coturn|mumble|janus|mediasoup` in non-vendored deploy files and `docs/superpowers` matches only unrelated substrings.

### Inferences
- A media server (e.g. an SFU) needs: one HTTPS/WSS signalling endpoint (fits the existing Ingress + cert-manager + Route53 pattern exactly) and a UDP media port or port range reachable from the internet (does NOT fit Traefik-as-configured; would need `hostNetwork`/`hostPort` on the single node, or a Traefik UDP entrypoint, plus a host firewall change that is managed in the other repo, `~/Projects/mod/mods/survival-world`). A single-node box makes `hostNetwork` workable; TURN/TCP fallback adds more ports.
- A new root would follow `deploy/envs/orion/<name>/{backend,main,providers,versions}.tf` + `deploy/modules/<name>/` and be added to the CLAUDE.md root order and justfile (`just orion-plan|apply <name>`); API keys as `random_password` + `kubernetes_secret_v1` like the login service token, never in tfvars committed.
- If voice instead rides the game's own QUIC/WebTransport connections, there is nothing new to deploy: UDP 6466/6467 are already open, and each extra R3 host already needs its own advertised ports (`VOXEL_ADVERTISE_QUIC/_WT`).
- The real multi-host, `jwt`-auth game server deployment does not exist yet (no orion `world` root, no server image); a voice design should not assume how game hosts will be exposed in k8s. The Agones fleet module shows the intended direction (per-pod dynamic UDP host ports).
- Single box, one copy: voice adds CPU/bandwidth on the same machine as the game server, Postgres, Keycloak and monitoring; no capacity figures exist in the repo.

### Gaps
- Host firewall rules, the Caddy proxy config and DNS for `orion.factoriosurvivalworld.com` are outside this repo; not inspected.
- Box capacity (CPU, bandwidth, public IPv4/IPv6) is not documented in files read.
- Whether k3s's Traefik has any UDP entrypoint or whether klipper ServiceLB is enabled was not determinable from the repo (the platform module comment about LoadBalancer addresses is for the local k0s setup).
- `.tfvars`/`.env` deliberately not read, so configured hostnames/values beyond the committed defaults are unknown.

## 9. UI and input: where push-to-talk, mute list, speaking indicators would go

### Takeaway
UI is Slint only (generated from `crates/slint-ui/ui/*.slint`); the HUD and chat box give direct precedents for a speaking indicator and a voice panel. Keys are hard-coded constants (no rebinding/settings screen exists), sim keys go through `map_key` in `renderer/input.rs`, and client-only keys are read by their own Bevy systems behind the `TextEntry` typing gate and `CursorPolicy`.

### Cited Findings
- Slint sources: `chat.slint, controls.slint, debug-tab.slint, embark.slint, fortress-panel.slint, frontend.slint, game.slint, hud.slint, index.slint, inventory.slint, loading.slint, login.slint, main-menu.slint, menu.slint, …` — `crates/slint-ui/ui/`. Rules: fortress theme only; never bind `visible`/`opacity`/shadows on a component root; every root calls `GameTheme.apply()`; after changes to Slint itself run `just slint-wasm` — CLAUDE.md "Slint UI".
- HUD: `hud.slint` is the pass-through layer (chat's recent lines are drawn there "so they never take a click") — `crates/slint-ui/ui/hud.slint:49-65`; client side `crates/client/src/renderer/hud.rs`. In-game interactive UI root: `game.slint` (chat box properties/callbacks at `:380-390`); bridged in `crates/client/src/renderer/slint_overlay.rs` (`connect_game_ui`, chat hookup at `:849`, action queue at `:338`).
- Esc menu: `menu.slint` with entries World Map / Retire fortress / End adventure / Main menu / Close — `crates/slint-ui/ui/menu.slint:5-61`. No settings/options screen and no key-binding code exist (grep `keybind|rebind|struct Settings` finds nothing relevant).
- Sim keys: `map_key` forwards only W A S D E Space LCtrl LShift Esc 1-9 Q to the sim as `game::Key` — `crates/client/src/renderer/input.rs:13-37`. Client-local keys are consts read elsewhere: M map (`slint_overlay.rs:70`), V top-down (`commander/top_down.rs:33`), G dig / B build / T chop tools (`commander/designate.rs:64-67`), F5 first person, F3 dev UI, Enter and `/` open chat (`chat/ui.rs:1,47`). Note the chat spec said "T opens a chat box" but T is the Chop tool and `chat/ui.rs` uses Enter / `/`.
- Typing gate: `TextEntry { on }` empties `ButtonInput<KeyCode>` before game systems while a Slint text field has the keyboard — `crates/client/src/renderer/text_entry.rs`, chat spec "Typing gate". `CursorPolicy` is "the only cursor-grab writer" — `crates/client/src/renderer/cursor.rs:1-8`, CLAUDE.md Client.
- Chat commands are a hand-written parser (`chat/command.rs`, help lines such as `/clear`) executed in `chat/run.rs` — a place for `/mute <name>`-style commands.
- `dev_ui` (F3 Debug tab) is dev tooling only, never player-facing — CLAUDE.md Client.
- Session rule: new per-world resources/Locals/entities must be registered with `add_session_reset` / `SessionLocal` / `SessionScoped` — CLAUDE.md "Session lifecycle"; example at `commander/mod.rs:486-492`.
- Other players are drawn by `attach_avatars` (`renderer/avatar.rs:36-40`); there are no nameplates or over-head UI today.

### Inferences
- Push-to-talk: a new const key read by a client-only Bevy system (not `map_key`, since it must not become a sim `InputEvent`), gated by `TextEntry` and overlay state like the other hotkeys. Free letters after the existing bindings include e.g. `KeyC`, `KeyF`, `KeyX`, `KeyZ`, `CapsLock`, `Backquote` (V, T, G, B, M, E, Q are taken); in the browser, some keys/combos are reserved by the browser.
- Speaking indicator / mic state: `hud.slint` (non-interactive) fed from `renderer/hud.rs`. Mute list / device and volume settings: a new panel in `game.slint` or an entry in `menu.slint` (there is no settings screen to extend, so this would be the first one). Over-head speaking markers would need new world-space UI near `avatar.rs`.
- A mute list keyed by something stable needs `PlayerId`/account identity and names delivered to clients (not available today, section 2); persistence across sessions has no client settings store to reuse (only `player_name` file / localStorage for the name: `crates/client/src/player_name.rs`, `Cargo.toml:118-119` comment).
- Browser mic permission prompts and the "user gesture" requirement interact with the UI: the first push-to-talk press or a settings toggle is the natural gesture.

### Gaps
- `hud.rs`, `slint_overlay.rs` and `text_entry.rs` were sampled, not read in full; exact overlay-blocking predicates for hotkeys were not traced.
- Whether Slint's software/GPU composite path (`slint_composite.rs`) has cost implications for a frequently-updating speaking indicator was not examined.
