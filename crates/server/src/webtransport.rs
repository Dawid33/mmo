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
                if let Err(e) = tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut buf).await {
                    warn!("webtransport read failed: {e:?}");
                    continue;
                }
                if buf.len() > MAX_PACKET_BYTES {
                    warn!("oversized packet from client {id}, dropping");
                    continue;
                }
                match bincode::deserialize::<ClientPacket>(&buf) {
                    Ok(packet) => send.send(ServerEvent::ClientPacket(packet, id)).unwrap(),
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
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/webtransport-cert-hash.json"
    );
    std::fs::write(path, json)?;
    Ok(())
}
