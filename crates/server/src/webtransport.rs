//! WebTransport ingress: browsers can't speak raw QUIC, so they connect here
//! (HTTP/3 Extended CONNECT on `VOXEL_BIND_ADDR`:6467, 127.0.0.1 by default) and
//! are routed into the same ServerEvent channel and ClientSink map as quinn
//! clients. Wire format is identical: one uni-stream per bincode packet.
//!
//! Two certificate sources ([`CertSource`]): in dev a self-signed cert the page
//! pins by hash (`assets/webtransport-cert-hash.json`); when deployed
//! (`just deploy`) a real certificate for the page's hostname — the Let's
//! Encrypt one the reverse proxy obtains — read from `VOXEL_WT_CERT` /
//! `VOXEL_WT_KEY` and reloaded whenever the files change (renewals).
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crossbeam::channel::Sender;
use dashmap::DashMap;
use game::{ClientId, ClientPacket, ServerEvent};
use log::{info, warn};
use wtransport::{Endpoint, Identity, ServerConfig};

use crate::player_store::{HELLO_MAX_BYTES, HELLO_TIMEOUT};
use crate::ClientSink;

/// Max size of a single packet stream; region snapshots are the largest
/// payloads and stay well under this.
const MAX_PACKET_BYTES: usize = 64 * 1024 * 1024;

/// PEM certificate chain for the ingress (set together with [`WT_KEY_ENV`]).
pub const WT_CERT_ENV: &str = "VOXEL_WT_CERT";
/// PEM private key for the ingress (set together with [`WT_CERT_ENV`]).
pub const WT_KEY_ENV: &str = "VOXEL_WT_KEY";

/// How often the certificate files are checked for a renewal, and how often
/// the ingress retries while they don't exist yet (the proxy obtains the
/// certificate only once DNS points at the box).
const CERT_POLL: Duration = Duration::from_secs(60);

/// Where the ingress's certificate comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertSource {
    /// Minted at start, pinned by the page through the hash file (dev).
    SelfSigned,
    /// A real certificate on disk, reloaded when its files change.
    Files { cert: PathBuf, key: PathBuf },
}

/// The certificate source from the values of [`WT_CERT_ENV`] / [`WT_KEY_ENV`]:
/// both or neither (empty counts as unset).
pub fn cert_source_from(cert: Option<&str>, key: Option<&str>) -> Result<CertSource, String> {
    let cert = cert.map(str::trim).filter(|v| !v.is_empty());
    let key = key.map(str::trim).filter(|v| !v.is_empty());
    match (cert, key) {
        (None, None) => Ok(CertSource::SelfSigned),
        (Some(cert), Some(key)) => Ok(CertSource::Files { cert: cert.into(), key: key.into() }),
        _ => Err(format!("set both {WT_CERT_ENV} and {WT_KEY_ENV}, or neither")),
    }
}

fn server_config(bind: SocketAddr, identity: Identity) -> ServerConfig {
    ServerConfig::builder()
        .with_bind_address(bind)
        .with_identity(identity)
        // A browser that vanishes without a close frame (tab killed, laptop
        // lid, frozen headless run) must be DETECTED, or its sink wedges the
        // shared writer task once the dead peer's stream credit runs out —
        // which stalls outgoing traffic for every client.
        .keep_alive_interval(Some(Duration::from_secs(3)))
        .max_idle_timeout(Some(Duration::from_secs(10)))
        .expect("static idle timeout is in range")
        .build()
}

/// The pair of modification times that identifies one version of the files.
fn cert_version(cert: &Path, key: &Path) -> Option<(SystemTime, SystemTime)> {
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    Some((modified(cert)?, modified(key)?))
}

/// Loads the files, waiting (and logging) until they exist and parse.
async fn load_identity_waiting(cert: &Path, key: &Path) -> (Identity, Option<(SystemTime, SystemTime)>) {
    loop {
        let version = cert_version(cert, key);
        match Identity::load_pemfiles(cert, key).await {
            Ok(identity) => return (identity, version),
            Err(e) => warn!(
                "webtransport certificate {} / {} not loadable yet ({e}); retrying in {}s",
                cert.display(),
                key.display(),
                CERT_POLL.as_secs()
            ),
        }
        tokio::time::sleep(CERT_POLL).await;
    }
}

/// Reloads the endpoint's certificate whenever the files change. A change is
/// applied once the files have stayed the same for a whole poll, so a renewal
/// caught between writing the chain and the key is not loaded half-done.
async fn watch_cert_files(
    endpoint: Arc<Endpoint<wtransport::endpoint::endpoint_side::Server>>,
    bind: SocketAddr,
    cert: PathBuf,
    key: PathBuf,
    mut loaded: Option<(SystemTime, SystemTime)>,
) {
    let mut seen = loaded;
    loop {
        tokio::time::sleep(CERT_POLL).await;
        let now = cert_version(&cert, &key);
        let settled = now.is_some() && now == seen && now != loaded;
        seen = now;
        if !settled {
            continue;
        }
        match Identity::load_pemfiles(&cert, &key).await {
            Ok(identity) => match endpoint.reload_config(server_config(bind, identity), false) {
                Ok(()) => {
                    info!("webtransport certificate reloaded from {}", cert.display());
                    loaded = now;
                }
                Err(e) => warn!("webtransport certificate reload failed: {e:?}"),
            },
            Err(e) => warn!("webtransport certificate changed but does not load: {e}"),
        }
    }
}

/// A self-signed identity ([`CertSource::SelfSigned`]) minted before the ingress starts:
/// its hash is part of this host's endpoint (`ServerPacket::Host`, its `hosts` row) — a
/// browser pins it to open a connection here.
pub struct SelfSignedCert {
    identity: Identity,
    sha256_hex: String,
}

impl SelfSignedCert {
    /// serverCertificateHashes requires an ECDSA cert valid <= 14 days; wtransport's
    /// self-signed identity satisfies both.
    pub fn mint() -> Result<SelfSignedCert, Box<dyn std::error::Error + Send + Sync>> {
        let identity = Identity::self_signed(["localhost", "127.0.0.1"])?;
        let sha256_hex = cert_sha256_hex(&identity);
        Ok(SelfSignedCert { identity, sha256_hex })
    }

    /// The certificate's SHA-256, lowercase hex (what the page passes to
    /// serverCertificateHashes).
    pub fn sha256_hex(&self) -> &str {
        &self.sha256_hex
    }
}

pub async fn serve(
    bind: SocketAddr,
    certs: CertSource,
    send: Sender<ServerEvent>,
    sinks: Arc<DashMap<ClientId, ClientSink>>,
    next_client_id: Arc<AtomicU64>,
    players: crate::player_store::SharedPlayerStore,
    auth: Arc<crate::auth_edge::Authenticator>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let endpoint = match &certs {
        CertSource::SelfSigned => return serve_minted(bind, SelfSignedCert::mint()?, send, sinks, next_client_id, players, auth).await,
        CertSource::Files { cert, key } => {
            let (identity, version) = load_identity_waiting(cert, key).await;
            info!("webtransport certificate loaded from {}", cert.display());
            let endpoint = Arc::new(Endpoint::server(server_config(bind, identity))?);
            tokio::spawn(watch_cert_files(endpoint.clone(), bind, cert.clone(), key.clone(), version));
            endpoint
        }
    };
    accept_loop(endpoint, bind, send, sinks, next_client_id, players, auth).await
}

/// [`serve`] with the self-signed identity `cert` minted already (its hash known to this
/// host's endpoint).
pub async fn serve_minted(
    bind: SocketAddr,
    cert: SelfSignedCert,
    send: Sender<ServerEvent>,
    sinks: Arc<DashMap<ClientId, ClientSink>>,
    next_client_id: Arc<AtomicU64>,
    players: crate::player_store::SharedPlayerStore,
    auth: Arc<crate::auth_edge::Authenticator>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Dev nicety, not load-bearing: never fail the ingress over it.
    if let Err(e) = write_cert_hash_file(cert.sha256_hex(), bind.port()) {
        warn!("could not write cert-hash file (browser clients need it): {e:?}");
    }
    let endpoint = Arc::new(Endpoint::server(server_config(bind, cert.identity))?);
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

/// Dev-mode CA substitute: the page fetches this (wasm-server-runner serves
/// assets/ over HTTP) and passes the hash to serverCertificateHashes.
fn write_cert_hash_file(hex: &str, port: u16) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let json = format!(r#"{{"sha256_hex":"{hex}","port":{port}}}"#);
    // Resolve the repo-root assets dir from the crate location, not the CWD:
    // the binary is run from the repo root but tests run from crates/server.
    // (env! bakes a build-machine path; acceptable for this localhost-dev file.)
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/webtransport-cert-hash.json"
    );
    std::fs::write(path, json)?;
    Ok(())
}

/// The SHA-256 of `identity`'s (one) certificate, lowercase hex.
fn cert_sha256_hex(identity: &Identity) -> String {
    let hash = identity.certificate_chain().as_slice().first().expect("self-signed identity has one cert").hash();
    hash.as_ref().iter().map(|b| format!("{b:02x}")).collect::<String>()
}

#[cfg(test)]
mod cert_tests {
    use super::*;

    #[test]
    fn cert_source_is_both_files_or_self_signed() {
        assert_eq!(cert_source_from(None, None), Ok(CertSource::SelfSigned));
        assert_eq!(cert_source_from(Some(""), Some(" ")), Ok(CertSource::SelfSigned));
        assert_eq!(
            cert_source_from(Some("/c.crt"), Some("/c.key")),
            Ok(CertSource::Files { cert: "/c.crt".into(), key: "/c.key".into() })
        );
        assert!(cert_source_from(Some("/c.crt"), None).is_err());
        assert!(cert_source_from(None, Some("/c.key")).is_err());
    }
}
