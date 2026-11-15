//! The authenticated QUIC service link between server-side processes (coordinator <-> host,
//! login -> host, host <-> host).
//!
//! A [`LinkListener`] accepts, a [`LinkDialer`] dials and reconnects forever. Trust is a pinned
//! self-signed certificate (the dialer holds its SHA-256) plus a shared service token in the
//! first frame. Frames are one bidirectional stream of `u32` big-endian length +
//! `game::encode_packet`. The public surface is crossbeam channels; the tokio runtime stays
//! inside this crate (a network edge).
//!
//! The link is not a queue: messages sent while a dialer is down are dropped, and `Down` tells
//! the owner, whose outbox is what retransmits.
//!
//! Like the game server's QUIC edge, rustls is built with the **ring** provider named
//! explicitly (a second provider in the tree makes `builder()` panic); keep `aws-lc-rs` out.

mod cert;
mod frame;
mod target;

use std::collections::BTreeMap;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam::channel::{Receiver, Sender};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use quinn::rustls::crypto::{CryptoProvider, ring};
use quinn::rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use quinn::rustls::{self, DigitallySignedStruct, SignatureScheme};
use quinn::{Connection, Endpoint, RecvStream, SendStream, TransportConfig};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

pub use cert::{SERVICE_TOKEN_ENV, SERVICE_TOKEN_FILE_ENV, cert_sha256_hex, pin_beside, read_cert_hash, service_token_from_env};
pub use frame::{FrameError, LINK_MAX_FRAME_BYTES};
pub use target::LinkTarget;

/// Wire version of the hello; a mismatch is refused. 2: a host's hello carries its claim epoch.
/// 3: hosts link to each other (R3: `game::PeerMsg` frames between hosts).
/// 4: the coordinator's multi-host messages (`Attach.generation`, `ClaimIs`, `PlayerHandedTo`,
/// hops, `LeaveRequest`), and the login relay between hosts (spawn orders, kicks, session ends;
/// `SpawnAck` names the placing host's addresses).
/// 5: R4 — a host's hello carries its registration number (`PeerRole::Host::reg`, not a claim
/// epoch); peers tell each other their claims (`Reliable::Owns`, `Volatile::ClaimWanted`).
/// 6: R4's fix wave — `HostMsg::Attach` carries the host's crash policy (`policy`): a
/// coordinator of several hosts refuses one that is not its own.
pub const LINK_PROTOCOL_VERSION: u32 = 6;
/// First reconnect delay; doubles per failed attempt up to [`LINK_RETRY_MAX_MS`].
pub const LINK_RETRY_MIN_MS: u64 = 250;
pub const LINK_RETRY_MAX_MS: u64 = 5000;
/// While a link stays down, the dialer logs its failed attempts at most this often (the first
/// failure of a down period is logged at once).
pub const LINK_DIAL_LOG_EVERY_SECS: u64 = 60;
/// Longest a peer may take to finish the handshake and hello.
const LINK_HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// A hello is a few dozen bytes; anything bigger is refused before it is allocated.
const LINK_HELLO_MAX_BYTES: usize = 4096;
const LINK_KEEP_ALIVE: Duration = Duration::from_secs(5);
const LINK_IDLE_TIMEOUT: Duration = Duration::from_secs(15);
const LINK_ALPN: &[u8] = b"mmo-svc-link/1";
/// Most connections a listener carries through the QUIC handshake and hello at once; more
/// are refused until one finishes (an unauthenticated peer must not make it hold unbounded
/// handshakes).
const LINK_MAX_PENDING_HANDSHAKES: usize = 16;
/// Per-stream and per-connection flow-control windows: what a peer can make the other end
/// buffer before it reads (the link's one stream is read as it arrives; a frame larger than
/// the window still passes, paced by flow control).
const LINK_STREAM_WINDOW: u32 = 1024 * 1024;
const LINK_CONNECTION_WINDOW: u32 = 2 * 1024 * 1024;
/// The certificate's name; the pin, not the name, is what is verified.
const LINK_SERVER_NAME: &str = "localhost";
/// QUIC application close codes.
const CLOSE_REFUSED: u32 = 1;
const CLOSE_NORMAL: u32 = 0;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerRole {
    /// A region host: its ordinal, its process start (`boot`), and its registration number
    /// `reg` (its `hosts` row's, one more than any before it): a boot that registered later
    /// has the higher one, so a listener keeping one peer per ordinal can tell a stale
    /// process (about to be fenced) from its replacement — whatever it claims or loses.
    Host { ordinal: u32, boot: u64, reg: u64 },
    Login,
    Coordinator,
}

/// The first frame a dialer sends. Its `Debug` never shows the token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkHello {
    pub token: String,
    pub role: PeerRole,
    pub region_blocks: u32,
    pub chunk_blocks: u32,
    pub protocol: u32,
}

impl std::fmt::Debug for LinkHello {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkHello")
            .field("token", &"<redacted>")
            .field("role", &self.role)
            .field("region_blocks", &self.region_blocks)
            .field("chunk_blocks", &self.chunk_blocks)
            .field("protocol", &self.protocol)
            .finish()
    }
}

impl LinkHello {
    /// A hello for this build's world dials and the current protocol version.
    pub fn new(token: String, role: PeerRole) -> Self {
        Self {
            token,
            role,
            region_blocks: game::REGION_BLOCKS as u32,
            chunk_blocks: game::CHUNK_BLOCKS as u32,
            protocol: LINK_PROTOCOL_VERSION,
        }
    }
}

/// A listener-side connection id, unique per accepted connection (a reconnect is a new peer).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerId(pub u64);

#[derive(Debug)]
pub enum LinkEvent<In> {
    /// The link is established. On the listener this is the peer's hello; on the dialer it is
    /// the hello it sent, once the listener accepted it.
    Up(LinkHello),
    Msg(In),
    /// The link is gone (or an attempt failed); a dialer retries on its own.
    Down(String),
}

// --- listener ---------------------------------------------------------------------------

/// Sends to the listener's connected peers. Cheap to clone.
pub struct PeerSenders<Out> {
    peers: Arc<Mutex<BTreeMap<PeerId, UnboundedSender<Out>>>>,
}

impl<Out> Clone for PeerSenders<Out> {
    fn clone(&self) -> Self {
        Self { peers: self.peers.clone() }
    }
}

impl<Out> PeerSenders<Out> {
    /// Queue `msg` for `peer`; false if that peer is not connected.
    pub fn send(&self, peer: PeerId, msg: Out) -> bool {
        let peers = self.peers.lock().unwrap();
        peers.get(&peer).is_some_and(|tx| tx.send(msg).is_ok())
    }

    pub fn broadcast(&self, msg: Out)
    where
        Out: Clone,
    {
        for tx in self.peers.lock().unwrap().values() {
            let _ = tx.send(msg.clone());
        }
    }

    pub fn peers(&self) -> Vec<PeerId> {
        self.peers.lock().unwrap().keys().copied().collect()
    }

    /// Close `peer`'s connection: nothing more is sent to it, and its link ends (its
    /// `Down` follows; messages it sent before may still arrive ahead of that). False if it
    /// was not connected.
    pub fn close(&self, peer: PeerId) -> bool {
        // Dropping its queue's sender ends the link's writer, and so the link.
        self.peers.lock().unwrap().remove(&peer).is_some()
    }
}

/// Owns the listening endpoint and its runtime; dropping it stops the listener and frees the port.
/// Drop it from synchronous code only: it drops a multi-thread tokio runtime.
pub struct LinkListener {
    runtime: Option<tokio::runtime::Runtime>,
    endpoint: Endpoint,
    addr: SocketAddr,
    cert_sha256: String,
}

impl Drop for LinkListener {
    fn drop(&mut self) {
        self.endpoint.close(CLOSE_NORMAL.into(), b"listener stopped");
        if let Some(rt) = self.runtime.take() {
            drop(rt);
        }
    }
}

fn transport() -> Arc<TransportConfig> {
    let mut t = TransportConfig::default();
    t.keep_alive_interval(Some(LINK_KEEP_ALIVE));
    t.max_idle_timeout(Some(LINK_IDLE_TIMEOUT.try_into().expect("idle timeout fits")));
    // One bidirectional stream (the dialer's) and no unidirectional ones: quinn's defaults
    // (100 + 100 streams, ~1.25 MB each) would let an unauthenticated peer make the other
    // end buffer hundreds of MB during the hello.
    t.max_concurrent_bidi_streams(1u32.into());
    t.max_concurrent_uni_streams(0u32.into());
    t.stream_receive_window(LINK_STREAM_WINDOW.into());
    t.receive_window(LINK_CONNECTION_WINDOW.into());
    Arc::new(t)
}

fn runtime(name: &str) -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread().worker_threads(1).thread_name(name).enable_all().build()
}

/// Why a hello is refused, if it is.
fn check_hello(hello: &LinkHello, token: &str) -> Result<(), String> {
    if !cert::ct_eq(hello.token.as_bytes(), token.as_bytes()) {
        return Err("bad service token".to_string());
    }
    if hello.protocol != LINK_PROTOCOL_VERSION {
        return Err(format!("link protocol {} != {LINK_PROTOCOL_VERSION}", hello.protocol));
    }
    if hello.region_blocks != game::REGION_BLOCKS as u32 || hello.chunk_blocks != game::CHUNK_BLOCKS as u32 {
        return Err(format!(
            "world dials differ: peer region/chunk blocks {}/{}, ours {}/{}",
            hello.region_blocks,
            hello.chunk_blocks,
            game::REGION_BLOCKS,
            game::CHUNK_BLOCKS
        ));
    }
    Ok(())
}

impl LinkListener {
    /// Listen on `addr` (port 0 picks one; see [`local_addr`](Self::local_addr)). The certificate
    /// is generated, and with `cert_hash_file` its SHA-256 hex is written there; the cert and key
    /// persist beside it so a restarted listener keeps the pin its dialers hold.
    pub fn bind<In, Out>(
        addr: SocketAddr,
        token: String,
        cert_hash_file: Option<std::path::PathBuf>,
    ) -> io::Result<(LinkListener, Receiver<(PeerId, LinkEvent<In>)>, PeerSenders<Out>)>
    where
        In: DeserializeOwned + Send + 'static,
        Out: Serialize + Send + 'static,
    {
        if token.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "the service token is empty"));
        }
        let id = cert::load_or_generate(cert_hash_file.as_deref())?;
        let cert_sha256 = cert::cert_sha256_hex(&id.cert_der);
        let mut crypto = rustls::ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(io::Error::other)?
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(id.cert_der)],
                PrivatePkcs8KeyDer::from(id.key_der).into(),
            )
            .map_err(io::Error::other)?;
        crypto.alpn_protocols = vec![LINK_ALPN.to_vec()];
        let mut config = quinn::ServerConfig::with_crypto(Arc::new(
            QuicServerConfig::try_from(crypto).map_err(io::Error::other)?,
        ));
        config.transport_config(transport());

        let runtime = runtime("svc-link-listener")?;
        let endpoint = {
            let _guard = runtime.enter();
            Endpoint::server(config, addr)?
        };
        let addr = endpoint.local_addr()?;
        let (event_tx, event_rx) = crossbeam::channel::unbounded();
        let peers: Arc<Mutex<BTreeMap<PeerId, UnboundedSender<Out>>>> = Arc::default();
        let senders = PeerSenders { peers: peers.clone() };
        let accept_endpoint = endpoint.clone();
        runtime.spawn(async move {
            let next_peer = Arc::new(AtomicU64::new(1));
            let token = Arc::new(token);
            let handshakes = Arc::new(tokio::sync::Semaphore::new(LINK_MAX_PENDING_HANDSHAKES));
            while let Some(incoming) = accept_endpoint.accept().await {
                let Ok(permit) = handshakes.clone().try_acquire_owned() else {
                    log::warn!(
                        "[svc-link] refusing {}: {LINK_MAX_PENDING_HANDSHAKES} handshakes already pending",
                        incoming.remote_address()
                    );
                    incoming.refuse();
                    continue;
                };
                let (event_tx, peers, token) = (event_tx.clone(), peers.clone(), token.clone());
                let peer = PeerId(next_peer.fetch_add(1, Ordering::Relaxed));
                tokio::spawn(async move {
                    let Ok(Ok(connection)) = tokio::time::timeout(LINK_HELLO_TIMEOUT, incoming).await else { return };
                    serve_peer(connection, peer, token, event_tx, peers, permit).await;
                });
            }
        });
        log::info!("[svc-link] listening on {addr} (cert sha256 {cert_sha256})");
        Ok((LinkListener { runtime: Some(runtime), endpoint, addr, cert_sha256 }, event_rx, senders))
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// The pin dialers need.
    pub fn cert_sha256(&self) -> &str {
        &self.cert_sha256
    }
}

async fn refuse(connection: &Connection, why: &str) {
    log::warn!("[svc-link] refusing {}: {why}", connection.remote_address());
    connection.close(CLOSE_REFUSED.into(), why.as_bytes());
}

async fn serve_peer<In, Out>(
    connection: Connection,
    peer: PeerId,
    token: Arc<String>,
    events: Sender<(PeerId, LinkEvent<In>)>,
    peers: Arc<Mutex<BTreeMap<PeerId, UnboundedSender<Out>>>>,
    handshake_permit: tokio::sync::OwnedSemaphorePermit,
) where
    In: DeserializeOwned + Send + 'static,
    Out: Serialize + Send + 'static,
{
    let handshake = tokio::time::timeout(LINK_HELLO_TIMEOUT, async {
        let (send, mut recv) = connection.accept_bi().await.map_err(|e| format!("no hello stream: {e}"))?;
        let hello: LinkHello =
            frame::read_frame(&mut recv, LINK_HELLO_MAX_BYTES, true).await.map_err(|e| format!("hello: {e}"))?;
        Ok::<_, String>((send, recv, hello))
    })
    .await;
    let (mut send, recv, hello) = match handshake {
        Ok(Ok(parts)) => parts,
        Ok(Err(why)) => return refuse(&connection, &why).await,
        Err(_) => return refuse(&connection, "hello timed out").await,
    };
    if let Err(why) = check_hello(&hello, &token) {
        return refuse(&connection, &why).await;
    }
    // Authenticated: no longer a pending handshake.
    drop(handshake_permit);
    if frame::write_frame(&mut send, &true).await.is_err() {
        return;
    }
    let (out_tx, out_rx) = unbounded_channel::<Out>();
    peers.lock().unwrap().insert(peer, out_tx);
    log::info!("[svc-link] peer {} up: {:?}", peer.0, hello.role);
    if events.send((peer, LinkEvent::Up(hello))).is_err() {
        peers.lock().unwrap().remove(&peer);
        return;
    }
    let deliver_events = events.clone();
    let reason = pump(connection.clone(), send, recv, out_rx, move |msg: In| {
        deliver_events.send((peer, LinkEvent::Msg(msg))).is_ok()
    })
    .await;
    peers.lock().unwrap().remove(&peer);
    log::info!("[svc-link] peer {} down: {reason}", peer.0);
    let _ = events.send((peer, LinkEvent::Down(reason)));
    connection.close(CLOSE_NORMAL.into(), b"link ended");
}

/// Run both directions of an established link until either ends; returns why.
async fn pump<In, Out>(
    connection: Connection,
    mut send: SendStream,
    mut recv: RecvStream,
    mut out_rx: UnboundedReceiver<Out>,
    deliver: impl Fn(In) -> bool + Send + 'static,
) -> String
where
    In: DeserializeOwned + Send + 'static,
    Out: Serialize + Send + 'static,
{
    // Two tasks, not one `select!`: a read cancelled mid-frame would desync the stream.
    let mut reader = tokio::spawn(async move {
        loop {
            match frame::read_frame::<In>(&mut recv, LINK_MAX_FRAME_BYTES, false).await {
                Ok(msg) => {
                    if !deliver(msg) {
                        return "owner dropped the receiver".to_string();
                    }
                }
                Err(e) => return e.to_string(),
            }
        }
    });
    let mut writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let packet = frame::encode(&msg);
            drop(msg);
            let sent = match packet {
                Ok(packet) => frame::write_packet(&mut send, &packet).await,
                Err(e) => Err(e),
            };
            match sent {
                Ok(()) => {}
                // Too big to send: drop that message, keep the link.
                Err(FrameError::Unsendable(why)) => log::error!("[svc-link] dropping an unsendable message: {why}"),
                Err(e) => return e.to_string(),
            }
        }
        "owner dropped the sender".to_string()
    });
    let reason = tokio::select! {
        r = &mut reader => r.unwrap_or_else(|e| format!("reader failed: {e}")),
        w = &mut writer => w.unwrap_or_else(|e| format!("writer failed: {e}")),
        e = connection.closed() => e.to_string(),
    };
    reader.abort();
    writer.abort();
    reason
}

// --- dialer -----------------------------------------------------------------------------

/// Accepts exactly the listener whose certificate hashes to the pin.
#[derive(Debug)]
struct PinnedCert {
    pin: String,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if cert::ct_eq(cert::cert_sha256_hex(end_entity).as_bytes(), self.pin.as_bytes()) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("service link certificate does not match the pinned sha256".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn client_config(pin: &str) -> io::Result<quinn::ClientConfig> {
    let provider = Arc::new(ring::default_provider());
    let mut crypto = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedCert { pin: pin.to_string(), provider }))
        .with_no_client_auth();
    crypto.alpn_protocols = vec![LINK_ALPN.to_vec()];
    let mut config =
        quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(crypto).map_err(io::Error::other)?));
    config.transport_config(transport());
    Ok(config)
}

pub struct LinkDialer;

impl LinkDialer {
    /// Dial `target` (an address or a `host:port` name, resolved again at every attempt), pinning the listener's certificate to `cert_sha256`, and keep the link up:
    /// reconnect forever with backoff. `Down` is sent when a link drops or the first attempt of a
    /// down period fails; messages sent while down are dropped. The link ends when the returned
    /// sender is dropped, or when the receiver is dropped and the next event send fails (an idle
    /// link notices only then). The dialer threads are detached; shutdown takes up to one backoff
    /// plus one attempt.
    ///
    /// Delivery is at-least-once around reconnects, never lossy after `Up`: a message sent just
    /// before the owner observes `Up` may also be delivered, so owners must be idempotent.
    pub fn connect<In, Out>(
        target: impl Into<LinkTarget>,
        cert_sha256: String,
        hello: LinkHello,
    ) -> (Sender<Out>, Receiver<LinkEvent<In>>)
    where
        In: DeserializeOwned + Send + 'static,
        Out: Serialize + Send + 'static,
    {
        let target: LinkTarget = target.into();
        let (out_tx, out_rx) = crossbeam::channel::unbounded::<Out>();
        let (event_tx, event_rx) = crossbeam::channel::unbounded::<LinkEvent<In>>();
        // The live connection's queue; None while down, so a send then is dropped.
        let slot: Arc<Mutex<Option<UnboundedSender<Out>>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (slot, stop) = (slot.clone(), stop.clone());
            std::thread::Builder::new()
                .name("svc-link-dialer-out".into())
                .spawn(move || {
                    for msg in out_rx {
                        if let Some(tx) = slot.lock().unwrap().as_ref() {
                            let _ = tx.send(msg);
                        }
                    }
                    stop.store(true, Ordering::SeqCst);
                    slot.lock().unwrap().take(); // ends the live writer, and so the live link
                })
                .expect("spawn the dialer's outbound thread");
        }
        std::thread::Builder::new()
            .name("svc-link-dialer".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("dialer runtime");
                rt.block_on(dial_loop(target, cert_sha256, hello, slot, stop, event_tx));
            })
            .expect("spawn the dialer thread");
        (out_tx, event_rx)
    }
}

/// Where a dial attempt goes, asked again before every attempt (a seam for the tests).
trait Resolve {
    async fn resolve(&self) -> io::Result<SocketAddr>;
}

impl Resolve for LinkTarget {
    async fn resolve(&self) -> io::Result<SocketAddr> {
        LinkTarget::resolve(self).await
    }
}

async fn attempt(
    endpoint: &Endpoint,
    addr: SocketAddr,
    config: &quinn::ClientConfig,
    hello: &LinkHello,
) -> Result<(Connection, SendStream, RecvStream), String> {
    let connecting = endpoint.connect_with(config.clone(), addr, LINK_SERVER_NAME).map_err(|e| e.to_string())?;
    let connection = tokio::time::timeout(LINK_HELLO_TIMEOUT, connecting)
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|e| format!("connect: {e}"))?;
    let handshake = tokio::time::timeout(LINK_HELLO_TIMEOUT, async {
        let (mut send, mut recv) = connection.open_bi().await.map_err(|e| e.to_string())?;
        frame::write_frame(&mut send, hello).await.map_err(|e| e.to_string())?;
        let ok: bool = frame::read_frame(&mut recv, LINK_HELLO_MAX_BYTES, true).await.map_err(|e| e.to_string())?;
        if ok { Ok((send, recv)) } else { Err("hello not accepted".to_string()) }
    })
    .await;
    match handshake {
        Ok(Ok((send, recv))) => Ok((connection, send, recv)),
        failure => {
            let why = match failure {
                Ok(Err(e)) => e,
                _ => "hello timed out".to_string(),
            };
            // A refusal arrives as the peer's close reason; prefer it over the stream error.
            let why = match tokio::time::timeout(Duration::from_millis(500), connection.closed()).await {
                Ok(closed) => closed.to_string(),
                Err(_) => why,
            };
            Err(why)
        }
    }
}

async fn dial_loop<In, Out>(
    target: impl Resolve + std::fmt::Display,
    pin: String,
    hello: LinkHello,
    slot: Arc<Mutex<Option<UnboundedSender<Out>>>>,
    stop: Arc<AtomicBool>,
    events: Sender<LinkEvent<In>>,
) where
    In: DeserializeOwned + Send + 'static,
    Out: Serialize + Send + 'static,
{
    // Bound once, at the first address that resolves: IPv4 wildcard unless that one is IPv6.
    let mut dialer: Option<(Endpoint, quinn::ClientConfig)> = None;
    let mut backoff = LINK_RETRY_MIN_MS;
    let mut announced_down = false;
    // Failed attempts since the last dial-failure line, and when that line went out.
    let mut unlogged_failures: u64 = 0;
    let mut last_dial_log: Option<std::time::Instant> = None;
    while !stop.load(Ordering::SeqCst) {
        let tried = async {
            let addr = target.resolve().await.map_err(|e| format!("resolve {target}: {e}"))?;
            if dialer.is_none() {
                let bind: SocketAddr = if addr.is_ipv4() { ([0, 0, 0, 0], 0).into() } else { ([0u16; 8], 0).into() };
                let parts = Endpoint::client(bind).and_then(|e| Ok((e, client_config(&pin)?)));
                dialer = Some(parts.map_err(|e| format!("dialer setup failed: {e}"))?);
            }
            let (endpoint, config) = dialer.as_ref().expect("bound above");
            attempt(endpoint, addr, config, &hello).await.map(|up| (addr, up))
        }
        .await;
        match tried {
            Ok((addr, (connection, send, recv))) => {
                backoff = LINK_RETRY_MIN_MS;
                unlogged_failures = 0;
                last_dial_log = None;
                let (tx, rx) = unbounded_channel::<Out>();
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                log::info!("[svc-link] up to {target} ({addr})");
                // Hold the slot lock across `Up`: the out thread blocks on it until the slot is
                // open, so anything the owner sends after seeing `Up` lands on this link.
                let mut open = slot.lock().unwrap();
                if events.send(LinkEvent::Up(hello.clone())).is_err() {
                    break;
                }
                *open = Some(tx);
                drop(open);
                if stop.load(Ordering::SeqCst) {
                    slot.lock().unwrap().take();
                    break;
                }
                let deliver_events = events.clone();
                let reason =
                    pump(connection.clone(), send, recv, rx, move |m: In| deliver_events.send(LinkEvent::Msg(m)).is_ok())
                        .await;
                slot.lock().unwrap().take();
                connection.close(CLOSE_NORMAL.into(), b"link ended");
                log::warn!("[svc-link] link to {target} ({addr}) down: {reason}");
                if events.send(LinkEvent::Down(reason)).is_err() {
                    break;
                }
                announced_down = true;
            }
            Err(why) => {
                unlogged_failures += 1;
                if last_dial_log.is_none_or(|t| t.elapsed() >= Duration::from_secs(LINK_DIAL_LOG_EVERY_SECS)) {
                    if last_dial_log.is_none() {
                        log::warn!("[svc-link] dial {target} failed: {why}");
                    } else {
                        log::warn!("[svc-link] dial {target} failed: {why} ({unlogged_failures} failed attempts since the last line)");
                    }
                    last_dial_log = Some(std::time::Instant::now());
                    unlogged_failures = 0;
                }
                if !announced_down {
                    if events.send(LinkEvent::Down(why)).is_err() {
                        break;
                    }
                    announced_down = true;
                }
            }
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(backoff)).await;
        backoff = (backoff * 2).min(LINK_RETRY_MAX_MS);
    }
    if let Some((endpoint, _)) = dialer {
        endpoint.close(CLOSE_NORMAL.into(), b"dialer stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Fails its first lookup, then names the live listener; counts its calls.
    struct FlakyResolver {
        live: SocketAddr,
        calls: Arc<AtomicUsize>,
    }

    impl Resolve for FlakyResolver {
        async fn resolve(&self) -> io::Result<SocketAddr> {
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => Err(io::Error::new(io::ErrorKind::NotFound, "no such host yet")),
                _ => Ok(self.live),
            }
        }
    }

    impl std::fmt::Display for FlakyResolver {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("flaky:6480")
        }
    }

    #[test]
    fn dial_resolves_again_after_a_failed_attempt() {
        let token = "t".to_string();
        let (listener, _from_peers, _senders) =
            LinkListener::bind::<u64, u64>(([127, 0, 0, 1], 0).into(), token.clone(), None).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = FlakyResolver { live: listener.local_addr(), calls: calls.clone() };
        let hello = LinkHello::new(token, PeerRole::Coordinator);
        let (slot, stop) = (Arc::<Mutex<Option<UnboundedSender<u64>>>>::default(), Arc::new(AtomicBool::new(false)));
        let (event_tx, events) = crossbeam::channel::unbounded::<LinkEvent<u64>>();
        let (pin, stop2) = (listener.cert_sha256().to_string(), stop.clone());
        let dialer = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(dial_loop(resolver, pin, hello, slot, stop2, event_tx));
        });
        let mut up = false;
        while let Ok(event) = events.recv_timeout(Duration::from_secs(10)) {
            if matches!(event, LinkEvent::Up(_)) {
                up = true;
                break;
            }
        }
        stop.store(true, Ordering::SeqCst);
        assert!(up, "the link came up on the second resolve");
        assert!(calls.load(Ordering::SeqCst) >= 2, "the resolver was asked again");
        // Left detached: a live link ends only when its connection does (idle timeout).
        drop(dialer);
    }
}
