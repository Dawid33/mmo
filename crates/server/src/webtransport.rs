//! WebTransport ingress: browsers can't speak raw QUIC, so they connect here
//! (HTTP/3 Extended CONNECT on `VOXEL_BIND_ADDR`:6467 — `VOXEL_WT_PORT` — 127.0.0.1 by default) and
//! are routed into the same ServerEvent channel and ClientSink map as quinn
//! clients. Wire format is identical: one uni-stream per bincode packet.
//!
//! It serves the host's one client-facing certificate ([`ClientCert`], shared with the QUIC
//! listener): in dev a self-signed one the page pins by hash
//! (`assets/webtransport-cert-hash.json`, written by host 0 only — [`cert_hash_file_names`]);
//! when deployed a real certificate for the page's hostname read from files and reloaded
//! when they change (renewals; `client_cert`).
use std::net::SocketAddr;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use crossbeam::channel::Sender;
use dashmap::DashMap;
use game::{ClientId, ClientPacket, ServerEvent};
use log::{debug, info, warn};
use quinn::rustls;
use wtransport::{Endpoint, ServerConfig};

use crate::client_cert::ClientCert;
use crate::player_store::{HELLO_MAX_BYTES, HELLO_TIMEOUT};
use crate::ClientSink;

/// Max size of a single packet stream; region snapshots are the largest
/// payloads and stay well under this.
const MAX_PACKET_BYTES: usize = 64 * 1024 * 1024;

/// The ingress's configuration: `cert`'s current pair on every handshake (a renewal is
/// served without touching the endpoint), TLS 1.3 and the WebTransport ALPN only — what
/// `wtransport::Identity`'s default TLS configuration sets, with ring named.
fn server_config(bind: SocketAddr, cert: &ClientCert) -> ServerConfig {
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("ring supports TLS 1.3")
        .with_no_client_auth()
        .with_cert_resolver(cert.rustls_resolver());
    tls.alpn_protocols = vec![wtransport::tls::WEBTRANSPORT_ALPN.to_vec()];
    ServerConfig::builder()
        .with_bind_address(bind)
        .with_custom_tls(tls)
        // A browser that vanishes without a close frame (tab killed, laptop
        // lid, frozen headless run) must be DETECTED, or its sink wedges the
        // shared writer task once the dead peer's stream credit runs out —
        // which stalls outgoing traffic for every client.
        .keep_alive_interval(Some(Duration::from_secs(3)))
        .max_idle_timeout(Some(Duration::from_secs(10)))
        .expect("static idle timeout is in range")
        .build()
}

/// Serves WebTransport on `bind` with `cert` as host `ordinal`: a self-signed `cert`'s hash
/// goes to the dev hash files ([`cert_hash_file_names`]) the page pins it by.
#[allow(clippy::too_many_arguments)]
pub async fn serve(
    bind: SocketAddr,
    cert: ClientCert,
    ordinal: game::HostId,
    send: Sender<ServerEvent>,
    sinks: Arc<DashMap<ClientId, ClientSink>>,
    next_client_id: Arc<AtomicU64>,
    players: crate::player_store::SharedPlayerStore,
    auth: Arc<crate::auth_edge::Authenticator>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(hex) = cert.sha256_hex() {
        // Dev nicety, not load-bearing: never fail the ingress over it.
        match write_cert_hash_file(&hex, bind.port(), ordinal) {
            Ok(true) => {}
            // A deployed binary (a container image) has no source tree; its clients pin the
            // hash each `HostEndpoint` carries.
            Ok(false) => debug!("no assets/ directory of the build's source tree: the dev cert-hash files are not written"),
            Err(e) => warn!("could not write cert-hash file (browser clients need it): {e:?}"),
        }
    }
    let endpoint = Arc::new(Endpoint::server(server_config(bind, &cert))?);
    accept_loop(endpoint, bind, send, sinks, next_client_id, players, auth).await
}

async fn accept_loop(
    endpoint: Arc<Endpoint<wtransport::endpoint::endpoint_side::Server>>,
    bind: SocketAddr,
    send: Sender<ServerEvent>,
    sinks: Arc<DashMap<ClientId, ClientSink>>,
    next_client_id: Arc<AtomicU64>,
    players: crate::player_store::SharedPlayerStore,
    auth: Arc<crate::auth_edge::Authenticator>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    info!("WebTransport endpoint listening on {bind}");

    loop {
        let incoming = endpoint.accept().await;
        let send = send.clone();
        let sinks = sinks.clone();
        let players = players.clone();
        let auth = auth.clone();
        let next_client_id = next_client_id.clone();
        tokio::spawn(async move {
            let session = match incoming.await {
                Ok(req) => match req.accept().await {
                    Ok(conn) => conn,
                    Err(e) => return warn!("webtransport accept failed: {e:?}"),
                },
                Err(e) => return warn!("webtransport session failed: {e:?}"),
            };
            // The handshake comes first: no session (and no sink) exists until
            // the connection's Hello has resolved to a player.
            let hello =
                match web_hello(&session, &players, &auth).await {
                    Ok(resolved) => resolved,
                    Err(why) => return refuse(&session, &why).await,
                };
            let (player, ticket) = (hello.player, hello.ticket.clone());
            // The id exists only now that the session is authenticated; the sink is
            // claimed in the same step, so a replayed ticket never replaces a live one.
            let id = hello.client_id(&next_client_id);
            // Announced before reading any other packets, mirroring the quinn path (an
            // observer's newer connection replaces its older one).
            let mine = ClientSink::web(session.clone());
            if let Err(why) = crate::seat_session(&sinks, &send, id, &hello, &auth, mine.clone()) {
                return refuse(&session, &why).await;
            }
            info!("webtransport client {id} connected (player {})", player.0);
            game::perf_event!("client_connected", "client={id} player={} transport=webtransport", player.0);

            loop {
                let stream = match session.accept_uni().await {
                    Ok(s) => s,
                    Err(e) => {
                        info!("webtransport client {id} disconnected: {e:?}");
                        break;
                    }
                };
                let mut buf = Vec::new();
                // Cap the read up front: read_to_end would otherwise buffer an
                // arbitrarily large stream before the size check below fires.
                // take() truncates at cap+1, so the post-check still rejects.
                let mut limited =
                    tokio::io::AsyncReadExt::take(stream, (MAX_PACKET_BYTES as u64) + 1);
                if let Err(e) = tokio::io::AsyncReadExt::read_to_end(&mut limited, &mut buf).await {
                    warn!("webtransport read failed: {e:?}");
                    continue;
                }
                if buf.len() > MAX_PACKET_BYTES {
                    warn!("oversized packet from client {id}, dropping");
                    continue;
                }
                match game::decode_packet::<ClientPacket>(&buf) {
                    Ok(packet) => {
                        crate::player_store::forward_packet(&sinks, &send, id, |live| live.same_connection(&mine), packet);
                    }
                    Err(e) => warn!("failed deserializing packet {e:?}"),
                }
            }
            crate::player_store::release_sink(&sinks, &send, id, |live| live.same_connection(&mine));
            if let Some(ticket) = &ticket {
                auth.session_closed(ticket);
            }
        });
    }
}

/// How long a refused session waits for its `Refused` packet to go out
/// before it is closed.
const REFUSE_FLUSH: Duration = Duration::from_secs(1);

/// Read a session's first packet (within [`HELLO_TIMEOUT`]) and resolve it as
/// the handshake.
async fn web_hello(
    session: &wtransport::Connection,
    players: &crate::player_store::SharedPlayerStore,
    auth: &crate::auth_edge::Authenticator,
) -> Result<crate::player_store::HelloSession, String> {
    let first = tokio::time::timeout(HELLO_TIMEOUT, async {
        let stream = session.accept_uni().await.map_err(|e| format!("no first packet: {e}"))?;
        let mut buf = Vec::new();
        let mut limited = tokio::io::AsyncReadExt::take(stream, (HELLO_MAX_BYTES as u64) + 1);
        tokio::io::AsyncReadExt::read_to_end(&mut limited, &mut buf)
            .await
            .map_err(|e| format!("first packet unreadable: {e}"))?;
        if buf.len() > HELLO_MAX_BYTES {
            return Err(format!("first packet over {HELLO_MAX_BYTES} bytes"));
        }
        game::decode_packet::<ClientPacket>(&buf).map_err(|e| format!("first packet does not decode: {e}"))
    })
    .await
    .map_err(|_| format!("no Hello within {HELLO_TIMEOUT:?}"))??;
    crate::player_store::resolve_session(players, auth, first).await
}

/// Tell the client why (best effort), then close the session.
async fn refuse(session: &wtransport::Connection, why: &str) {
    warn!("refusing a webtransport session from {}: {why}", session.remote_address());
    if let Ok(packet) = game::encode_packet(&game::ServerPacket::Refused(why.to_string())) {
        let _ = tokio::time::timeout(REFUSE_FLUSH, async {
            let mut stream = session.open_uni().await.ok()?.await.ok()?;
            tokio::io::AsyncWriteExt::write_all(&mut stream, &packet).await.ok()?;
            stream.finish().await.ok()
        })
        .await;
    }
    session.close(wtransport::VarInt::from_u32(1), b"refused");
}

/// The dev hash files (under `assets/`) host `ordinal` writes. The page reads
/// `webtransport-cert-hash.json` to pin the server a typed local address names (its first
/// connection; the others are pinned by each host's `HostEndpoint::cert_hash`): only host 0
/// writes it — the one host of a single-host run — so several dev hosts (`just dev-r3`) no
/// longer overwrite each other's. Every host also writes its own
/// `webtransport-cert-hash-<ordinal>.json`.
pub fn cert_hash_file_names(ordinal: game::HostId) -> Vec<String> {
    let own = format!("webtransport-cert-hash-{}.json", ordinal.0);
    match ordinal.0 {
        0 => vec!["webtransport-cert-hash.json".to_string(), own],
        _ => vec![own],
    }
}

/// Dev-mode CA substitute: the page fetches this (wasm-server-runner serves
/// assets/ over HTTP) and passes the hash to serverCertificateHashes. `Ok(false)`: no
/// source tree here, nothing written (see [`write_cert_hash_files_into`]).
fn write_cert_hash_file(hex: &str, port: u16, ordinal: game::HostId) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    // Resolve the repo-root assets dir from the crate location, not the CWD:
    // the binary is run from the repo root but tests run from crates/server.
    // (env! bakes a build-machine path; acceptable for this localhost-dev file.)
    let assets = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets"));
    write_cert_hash_files_into(assets, hex, port, ordinal)
}

/// Writes host `ordinal`'s dev hash files into `assets`, if that directory exists: a
/// binary running away from the source tree it was built in (a container image) has none,
/// and writes nothing (`Ok(false)`) instead of failing on every start.
fn write_cert_hash_files_into(
    assets: &std::path::Path,
    hex: &str,
    port: u16,
    ordinal: game::HostId,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    if !assets.is_dir() {
        return Ok(false);
    }
    let json = format!(r#"{{"sha256_hex":"{hex}","port":{port}}}"#);
    for name in cert_hash_file_names(ordinal) {
        std::fs::write(assets.join(name), &json)?;
    }
    Ok(true)
}

#[cfg(test)]
mod cert_tests {
    use super::*;

    /// Several dev hosts each mint a certificate: only host 0 writes the file the page
    /// pins a typed local address by, so the last host to start no longer wins it.
    #[test]
    fn only_host_zero_writes_the_page_s_hash_file() {
        assert_eq!(cert_hash_file_names(game::HostId(0)), ["webtransport-cert-hash.json", "webtransport-cert-hash-0.json"]);
        assert_eq!(cert_hash_file_names(game::HostId(1)), ["webtransport-cert-hash-1.json"]);
        assert_eq!(cert_hash_file_names(game::HostId(7)), ["webtransport-cert-hash-7.json"]);
    }

    /// A deployed binary has no `assets/` of its build's source tree: it writes nothing and
    /// that is not an error (the k3d twin logged a NotFound WARN on every host start).
    #[test]
    fn no_assets_directory_writes_nothing() {
        let dir = std::env::temp_dir().join(format!("cert-hash-missing-{}", std::process::id()));
        assert!(!dir.exists());
        assert!(!write_cert_hash_files_into(&dir, "ab", 6467, game::HostId(0)).expect("a missing directory is not an error"));
        assert!(!dir.exists(), "nothing created");
    }

    #[test]
    fn an_assets_directory_gets_the_hash_files() {
        let dir = std::env::temp_dir().join(format!("cert-hash-present-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(write_cert_hash_files_into(&dir, "ab", 6467, game::HostId(0)).unwrap());
        for name in cert_hash_file_names(game::HostId(0)) {
            assert_eq!(std::fs::read_to_string(dir.join(name)).unwrap(), r#"{"sha256_hex":"ab","port":6467}"#);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
